//! 零件怎么画：解包里的 WebP，按要的颜色乘、按要的角度转，结果留着复用；每帧再在 [`Canvas`]
//! 上叠成整只小人，一张图交给 gpui。
//!
//! 不把零件一张张交给 gpui 叠：gpui 画图时把每张图的四条边各自对齐到屏幕像素，身子起伏一两个
//! 像素时，眼睛、胳膊、身子各自取整的方向不一样，零件之间来回差一个像素，看着一直在抖。在这里
//! 按亚像素插值叠好，整张图的位置不动，动作就是平滑的。
//!
//! gpui 转不了彩色图、也不给图乘色，所以都在这里算好：会转的零件每 45° 渲一张（`sprites::KEYS`），
//! 要别的角度时取最近的那张再在画面里转剩下的（最多 20°）。镜头是正交的、零件是绕朝着镜头
//! 的轴转的，转出来的轮廓和 Blender 渲的一样，只有高光跟着偏了这一点。身子、胳膊和戴的东西的
//! 主料渲成中性灰、按线性存，这里在线性空间里逐通道乘系数（`sprites::BODY_TINTS`、`WEAR_TINTS`），
//! 再用 Khronos PBR Neutral 色调映射压回去（[`tone`]）：浅色乘完会超过白，硬截断会把受光的
//! 半边截成一片平色，压一下高光还是往白里渐变。别的零件 Blender 渲时已经按同一套映射出图。
//!
//! 零件算好的留到退出：要画的组合有上限（每组 72 个角度 × 几种颜色），全算出来也就几 MB。按进程
//! 共用缓存，后台线程预解码上色，渲染线程直接取来叠。

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use gpui::RenderImage;
use image::{Frame, Rgba, RgbaImage};

use super::sprites::{self, CANVAS, KEYS, SPRITES};

/// 会转的零件画到多细：每格几度。
pub(super) const TURN: f32 = 5.;
/// 转一圈几格。
pub(super) const TURNS: usize = 72;
/// 两个关键朝向之间几格。
const PER_KEY: usize = TURNS / KEYS;
const _: () = assert!(TURNS * TURN as usize == 360 && TURNS.is_multiple_of(KEYS));

/// 成图每 pt 几像素。
pub(super) const PX: f32 = 2.;

/// 成图在画布（[`CANVAS`]）四周各多留几 pt：举起来的道具、头顶的特效、车边的速度线会伸出画布。
pub(super) const MARGIN: f32 = 12.;
/// 成图边长（像素）。
const SIDE: usize = ((CANVAS + 2. * MARGIN) * PX) as usize;

/// 乘什么色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Tint {
    /// 原色。
    None,
    /// 第几种身子色。
    Body(usize),
    /// 戴的东西的第几种颜色。
    Wear(usize),
}

impl Tint {
    fn factors(self) -> Option<[f32; 3]> {
        match self {
            Self::None => None,
            Self::Body(color) => Some(sprites::BODY_TINTS[color]),
            Self::Wear(color) => Some(sprites::WEAR_TINTS[color]),
        }
    }
}

/// 要画的一张零件：包里第几张，在它上面再转几格（每格 [`TURN`]°，顺时针为正），乘什么色。画
/// 出来的大小、锚点和包里那张一样。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Paint {
    pub(super) sprite: usize,
    spin: i8,
    tint: Tint,
}

impl Paint {
    pub(super) fn plain(sprite: usize) -> Self {
        Self::tinted(sprite, Tint::None)
    }

    pub(super) fn tinted(sprite: usize, tint: Tint) -> Self {
        Self {
            sprite,
            spin: 0,
            tint,
        }
    }

    /// 会转的组（第一张是 `first`）转到第 `turn` 格：取最近的关键朝向，再转差的那几格。
    pub(super) fn turned(first: usize, turn: usize, tint: Tint) -> Self {
        let nearest = (turn % TURNS + PER_KEY / 2) / PER_KEY;
        Self {
            sprite: first + nearest % KEYS,
            spin: (turn % TURNS) as i8 - (nearest * PER_KEY) as i8,
            tint,
        }
    }

    /// 画好的零件（预乘透明度），没画过的这里现画；先无锁查缓存，算的过程不占写锁。
    pub(super) fn pixels(self) -> Arc<RgbaImage> {
        static PAINTED: OnceLock<RwLock<HashMap<Paint, Arc<RgbaImage>>>> = OnceLock::new();
        let painted = PAINTED.get_or_init(Default::default);
        if let Some(hit) = painted.read().unwrap_or_else(|e| e.into_inner()).get(&self) {
            return Arc::clone(hit);
        }
        let drawn = self.draw();
        let mut guard = painted.write().unwrap_or_else(|e| e.into_inner());
        Arc::clone(guard.entry(self).or_insert(drawn))
    }

    fn draw(self) -> Arc<RgbaImage> {
        let mut pixels = decode(self.sprite);
        if let Some(factors) = self.tint.factors() {
            tint(&mut pixels, factors);
        }
        premultiply(&mut pixels);
        if self.spin != 0 {
            let (x, y) = SPRITES[self.sprite].anchor;
            pixels = spin(&pixels, (x * PX, y * PX), f32::from(self.spin) * TURN);
        }
        Arc::new(pixels)
    }
}

/// 一帧：零件按锚点一张张叠上去（后叠的盖住先叠的），叠完成一张图。位置用 pt，相对画布
/// （[`CANVAS`] 见方）左上角，伸出画布 [`MARGIN`] 以内的照画。
pub(super) struct Canvas {
    /// 预乘透明度，0 到 255。
    pixels: Vec<[f32; 4]>,
}

impl Canvas {
    pub(super) fn new() -> Self {
        Self {
            pixels: vec![[0.; 4]; SIDE * SIDE],
        }
    }

    /// 叠一张零件：锚点放到 `at`，按 `scale` 缩放，再乘透明度 `alpha`。
    pub(super) fn layer(&mut self, paint: Paint, at: (f32, f32), scale: (f32, f32), alpha: f32) {
        self.layer_tilted(paint, at, scale, 0., alpha);
    }

    /// 和 [`Canvas::layer`] 一样，再绕锚点在画面里顺时针转 `degrees`（先缩放再转）。歪头、身子
    /// 左右倾时用：叠的时候反着转回零件里双线性取，不用先另转一张。
    pub(super) fn layer_tilted(
        &mut self,
        paint: Paint,
        at: (f32, f32),
        scale: (f32, f32),
        degrees: f32,
        alpha: f32,
    ) {
        let alpha = alpha.clamp(0., 1.);
        if alpha == 0. || scale.0 <= 0. || scale.1 <= 0. {
            return;
        }
        if degrees.abs() > 0.01 {
            self.rotated(paint, at, scale, degrees, alpha);
            return;
        }
        let source = paint.pixels();
        let anchor = SPRITES[paint.sprite].anchor;
        // 零件左上角、每个源像素在成图上占几个像素。
        let (left, top) = (
            (at.0 - anchor.0 * scale.0 + MARGIN) * PX,
            (at.1 - anchor.1 * scale.1 + MARGIN) * PX,
        );
        let (width, height) = source.dimensions();
        let rows = covered(top, top + height as f32 * scale.1);
        let columns = covered(left, left + width as f32 * scale.0);
        for y in rows {
            let v = (y as f32 + 0.5 - top) / scale.1;
            for x in columns.clone() {
                let u = (x as f32 + 0.5 - left) / scale.0;
                let color = sample(&source, u, v);
                if color[3] == 0. {
                    continue;
                }
                let pixel = &mut self.pixels[y * SIDE + x];
                let keep = 1. - color[3] * alpha / 255.;
                for (out, value) in pixel.iter_mut().zip(color) {
                    *out = value * alpha + *out * keep;
                }
            }
        }
    }

    /// 转着叠：成图上每个像素反着转、反着缩放回零件里取。
    fn rotated(
        &mut self,
        paint: Paint,
        at: (f32, f32),
        scale: (f32, f32),
        degrees: f32,
        alpha: f32,
    ) {
        let source = paint.pixels();
        let (ax, ay) = SPRITES[paint.sprite].anchor;
        let (ax, ay) = (ax * PX, ay * PX);
        let (cx, cy) = ((at.0 + MARGIN) * PX, (at.1 + MARGIN) * PX);
        let (sin, cos) = degrees.to_radians().sin_cos();
        let (width, height) = source.dimensions();
        // 零件四个角转过去落在哪，取外接框。
        let corners = [
            (0., 0.),
            (width as f32, 0.),
            (0., height as f32),
            (width as f32, height as f32),
        ]
        .map(|(x, y)| {
            let (dx, dy) = ((x - ax) * scale.0, (y - ay) * scale.1);
            (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos)
        });
        let fold = |pick: fn(&(f32, f32)) -> f32| {
            let values = corners.iter().map(pick);
            let low = values.clone().fold(f32::INFINITY, f32::min);
            (low, values.fold(f32::NEG_INFINITY, f32::max))
        };
        let (left, right) = fold(|corner| corner.0);
        let (top, bottom) = fold(|corner| corner.1);
        for y in covered(top, bottom) {
            for x in covered(left, right) {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let u = (dx * cos + dy * sin) / scale.0 + ax;
                let v = (-dx * sin + dy * cos) / scale.1 + ay;
                let color = sample(&source, u, v);
                if color[3] == 0. {
                    continue;
                }
                let pixel = &mut self.pixels[y * SIDE + x];
                let keep = 1. - color[3] * alpha / 255.;
                for (out, value) in pixel.iter_mut().zip(color) {
                    *out = value * alpha + *out * keep;
                }
            }
        }
    }

    /// 把另外叠好的两张按 `mix` 混成一张（0 全是 `a`、1 全是 `b`），整张叠上来。转身夹在两个
    /// 朝向之间时用：两张各自不透明的地方混出来还是不透明；各画半透明叠上来，中间会透底。
    pub(super) fn blend(&mut self, a: &Canvas, b: &Canvas, mix: f32) {
        for ((out, a), b) in self.pixels.iter_mut().zip(&a.pixels).zip(&b.pixels) {
            let color: [f32; 4] =
                std::array::from_fn(|channel| a[channel] + (b[channel] - a[channel]) * mix);
            let keep = 1. - color[3] / 255.;
            for (out, value) in out.iter_mut().zip(color) {
                *out = value + *out * keep;
            }
        }
    }

    /// 叠好的整张，gpui 按 BGRA、不预乘的取。
    pub(super) fn finish(self) -> Arc<RenderImage> {
        let pixels = RgbaImage::from_fn(SIDE as u32, SIDE as u32, |x, y| {
            let [r, g, b, a] = self.pixels[y as usize * SIDE + x as usize];
            if a < 0.5 {
                return Rgba([0; 4]);
            }
            let color = |value: f32| (value * 255. / a).round().min(255.) as u8;
            Rgba([color(b), color(g), color(r), a.round() as u8])
        });
        Arc::new(RenderImage::new(vec![Frame::new(pixels)]))
    }
}

/// 第 `sprite` 张零件画出来的东西的重心（按不透明度加权）离锚点多远（pt），全透明的没有。
/// 转身夹在两个朝向之间时，五官、戴的按两个朝向里重心的差挪过去。算过的留着。
pub(super) fn centroid(sprite: usize) -> Option<(f32, f32)> {
    static CENTROIDS: OnceLock<RwLock<HashMap<usize, Option<(f32, f32)>>>> = OnceLock::new();
    let known = CENTROIDS.get_or_init(Default::default);
    if let Some(hit) = known
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&sprite)
        .copied()
    {
        return hit;
    }
    let pixels = Paint::plain(sprite).pixels();
    let (mut weight, mut x, mut y) = (0., 0., 0.);
    for (px, py, pixel) in pixels.enumerate_pixels() {
        let alpha = f32::from(pixel.0[3]);
        weight += alpha;
        x += alpha * (px as f32 + 0.5);
        y += alpha * (py as f32 + 0.5);
    }
    let (ax, ay) = SPRITES[sprite].anchor;
    let computed = (weight > 0.).then(|| (x / weight / PX - ax, y / weight / PX - ay));
    let mut guard = known.write().unwrap_or_else(|e| e.into_inner());
    *guard.entry(sprite).or_insert(computed)
}

/// 从 `from` 到 `to`（像素，可以是小数）沾到的成图像素，出了成图的不算。
fn covered(from: f32, to: f32) -> std::ops::Range<usize> {
    let bound = |value: f32| value.clamp(0., SIDE as f32) as usize;
    bound(from.floor())..bound(to.ceil())
}

/// 解成 RGBA。零件是编进来的，解不开是打包出了错，直接 panic（测试会先拦下）。
fn decode(index: usize) -> RgbaImage {
    let sprite = &SPRITES[index];
    let bytes = &sprites::BLOB[sprite.start..sprite.start + sprite.len];
    image::load_from_memory_with_format(bytes, image::ImageFormat::WebP)
        .expect("小人零件不是合法的 WebP")
        .into_rgba8()
}

/// 乘上透明度：插值、叠图都在预乘下做，半透明的边上才不发黑。
fn premultiply(pixels: &mut RgbaImage) {
    for pixel in pixels.pixels_mut() {
        let alpha = u16::from(pixel.0[3]);
        for channel in &mut pixel.0[..3] {
            *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
}

/// 双线性取 `source`（预乘）里 (`u`, `v`) 处的颜色，坐标按像素、(0, 0) 是左上角那个像素的
/// 左上角。图外当透明。
fn sample(source: &RgbaImage, u: f32, v: f32) -> [f32; 4] {
    let (width, height) = source.dimensions();
    let (u, v) = (u - 0.5, v - 0.5);
    let (x0, y0) = (u.floor(), v.floor());
    let (fx, fy) = (u - x0, v - y0);
    let (x0, y0) = (x0 as i64, y0 as i64);
    let mut sum = [0.; 4];
    for (x, y, weight) in [
        (x0, y0, (1. - fx) * (1. - fy)),
        (x0 + 1, y0, fx * (1. - fy)),
        (x0, y0 + 1, (1. - fx) * fy),
        (x0 + 1, y0 + 1, fx * fy),
    ] {
        let inside = (0..i64::from(width)).contains(&x) && (0..i64::from(height)).contains(&y);
        if weight == 0. || !inside {
            continue;
        }
        let pixel = source.get_pixel(x as u32, y as u32).0;
        for (total, value) in sum.iter_mut().zip(pixel) {
            *total += f32::from(value) * weight;
        }
    }
    sum
}

/// 每个通道在线性空间里乘 `factors`，再做色调映射。透明度不变。
fn tint(pixels: &mut RgbaImage, factors: [f32; 3]) {
    let linear = std::array::from_fn::<f32, 256, _>(|value| linear(value as u8));
    for pixel in pixels.pixels_mut() {
        let lit =
            std::array::from_fn(|channel| linear[usize::from(pixel.0[channel])] * factors[channel]);
        let [r, g, b] = tone(lit).map(encode);
        pixel.0[..3].copy_from_slice(&[r, g, b]);
    }
}

/// Khronos PBR Neutral 色调映射（线性 Rec.709 进出）：亮度在 0.76 以下的颜色几乎原样（只把
/// 黑位挪一点），往上的按亮度平滑压到 1 以内，压得越多越往白里褪，色相不变。和 Blender 的
/// 「Khronos PBR Neutral」一样，公式见 <https://github.com/KhronosGroup/ToneMapping>。
fn tone(color: [f32; 3]) -> [f32; 3] {
    const START: f32 = 0.8 - 0.04;
    const DESATURATION: f32 = 0.15;
    let darkest = color.into_iter().fold(f32::INFINITY, f32::min);
    let offset = if darkest < 0.08 {
        darkest - 6.25 * darkest * darkest
    } else {
        0.04
    };
    let color = color.map(|value| value - offset);
    let peak = color.into_iter().fold(0., f32::max);
    if peak < START {
        return color;
    }
    let d = 1. - START;
    let new_peak = 1. - d * d / (peak + d - START);
    let fade = 1. - 1. / (DESATURATION * (peak - new_peak) + 1.);
    color.map(|value| value * new_peak / peak * (1. - fade) + new_peak * fade)
}

fn linear(value: u8) -> f32 {
    let v = f32::from(value) / 255.;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn encode(v: f32) -> u8 {
    let v = v.clamp(0., 1.);
    let srgb = if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1. / 2.4) - 0.055
    };
    (srgb * 255.).round() as u8
}

/// 绕 `anchor`（像素）在画面里顺时针转 `degrees`，大小不变。进出都是预乘的，双线性取样。
fn spin(source: &RgbaImage, (ax, ay): (f32, f32), degrees: f32) -> RgbaImage {
    let (width, height) = source.dimensions();
    let (sin, cos) = degrees.to_radians().sin_cos();
    RgbaImage::from_fn(width, height, |x, y| {
        // 从成图的像素中心反着转回原图里取：画面里顺时针转，原图上就是逆时针。
        let (dx, dy) = (x as f32 + 0.5 - ax, y as f32 + 0.5 - ay);
        let u = dx * cos + dy * sin + ax;
        let v = -dx * sin + dy * cos + ay;
        Rgba(sample(source, u, v).map(|value| value.round().min(255.) as u8))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sprite_decodes() {
        for index in 0..SPRITES.len() {
            let (width, height) = Paint::plain(index).pixels().dimensions();
            assert!(width > 0 && height > 0, "零件 {index} 解不出来");
        }
    }

    #[test]
    fn cross_thread_cache_reuses_background_painted_sprites() {
        let handle = std::thread::spawn(|| {
            (
                Paint::plain(sprites::BODY).pixels(),
                centroid(sprites::EYE_OPEN_L),
            )
        });
        let (bg_pixels, bg_centroid) = handle.join().unwrap();
        let main_pixels = Paint::plain(sprites::BODY).pixels();
        assert!(Arc::ptr_eq(&bg_pixels, &main_pixels));
        assert_eq!(bg_centroid, centroid(sprites::EYE_OPEN_L));
    }

    #[test]
    fn a_turn_takes_the_nearest_key_and_spins_the_rest() {
        let first = sprites::ARM;
        for turn in 0..TURNS {
            let paint = Paint::turned(first, turn, Tint::None);
            let key = paint.sprite - first;
            assert!(key < KEYS);
            assert!(usize::from(paint.spin.unsigned_abs()) <= PER_KEY / 2);
            let degrees = key as f32 * 360. / KEYS as f32 + f32::from(paint.spin) * TURN;
            assert_eq!(degrees.rem_euclid(360.), turn as f32 * TURN, "第 {turn} 格");
        }
        assert_eq!(
            Paint::turned(first, TURNS, Tint::None),
            Paint::turned(first, 0, Tint::None)
        );
    }

    /// 身子叠在画布左上角（锚点放在它自己的锚点上）时，成图里第 `(x, y)` 个像素。
    fn body_on_canvas(nudge: f32) -> impl Fn(u32, u32) -> [f32; 4] {
        let paint = Paint::plain(sprites::BODY);
        let (ax, ay) = SPRITES[sprites::BODY].anchor;
        let mut canvas = Canvas::new();
        canvas.layer(paint, (ax + nudge, ay), (1., 1.), 1.);
        let origin = (MARGIN * PX) as u32;
        move |x, y| canvas.pixels[(y + origin) as usize * SIDE + (x + origin) as usize]
    }

    #[test]
    fn a_layer_on_whole_pixels_is_copied_as_is() {
        let source = Paint::plain(sprites::BODY).pixels();
        let at = body_on_canvas(0.);
        for (x, y, pixel) in source.enumerate_pixels() {
            assert_eq!(
                at(x, y).map(|value| value.round() as u8),
                pixel.0,
                "({x}, {y})"
            );
        }
    }

    #[test]
    fn a_layer_between_pixels_is_blended_not_snapped() {
        // 往右挪半个像素：每个像素是原图左右两个的平均，不是整个跳过去。
        let source = Paint::plain(sprites::BODY).pixels();
        let at = body_on_canvas(0.5 / PX);
        let (width, height) = source.dimensions();
        for (x, y) in [(width / 2, height / 2), (width / 3, height / 2)] {
            let (left, right) = (source.get_pixel(x - 1, y).0, source.get_pixel(x, y).0);
            let mean = |channel: usize| (f32::from(left[channel]) + f32::from(right[channel])) / 2.;
            let got = at(x, y);
            assert!((0..4).all(|channel| (got[channel] - mean(channel)).abs() < 0.01));
        }
    }

    #[test]
    fn blending_two_opaque_layers_stays_opaque() {
        let body = Paint::plain(sprites::BODY);
        let (mut a, mut b) = (Canvas::new(), Canvas::new());
        a.layer(body, sprites::FEET, (1., 1.), 1.);
        b.layer(body, (sprites::FEET.0 + 0.5, sprites::FEET.1), (1., 1.), 1.);
        let mut mixed = Canvas::new();
        mixed.blend(&a, &b, 0.5);
        // 身子中间（脚底往上 8pt）。
        let (x, y) = (sprites::FEET.0 + MARGIN, sprites::FEET.1 - 8. + MARGIN);
        let middle = mixed.pixels[(y * PX) as usize * SIDE + (x * PX) as usize];
        assert!(middle[3] > 254., "{middle:?}");
    }

    #[test]
    fn a_tilted_layer_turns_about_its_anchor() {
        // 转 90°：锚点不动，原来锚点正上方的东西转到右边。
        let paint = Paint::plain(sprites::BODY);
        let mut straight = Canvas::new();
        straight.layer(paint, sprites::FEET, (1., 1.), 1.);
        let mut tilted = Canvas::new();
        tilted.layer_tilted(paint, sprites::FEET, (1., 1.), 90., 1.);
        let at = |canvas: &Canvas, (x, y): (f32, f32)| {
            let (x, y) = ((x + MARGIN) * PX, (y + MARGIN) * PX);
            canvas.pixels[y as usize * SIDE + x as usize][3]
        };
        let (fx, fy) = sprites::FEET;
        assert!(at(&straight, (fx, fy - 8.)) > 250.);
        assert!(at(&tilted, (fx + 8., fy)) > 250.);
        assert!(at(&tilted, (fx, fy - 20.)) < 5.);
        // 转得很小也走转的路，和不转的差不多。
        let mut nudged = Canvas::new();
        nudged.layer_tilted(paint, sprites::FEET, (1., 1.), 0.5, 1.);
        assert!((at(&nudged, (fx, fy - 8.)) - at(&straight, (fx, fy - 8.))).abs() < 1.);
    }

    #[test]
    fn centroids_sit_where_the_part_is() {
        // 正面的左眼在锚点（脚底）左上方，右眼在右上方；背面看不见眼睛。
        let (lx, ly) = centroid(sprites::EYE_OPEN_L).unwrap();
        let (rx, _) = centroid(sprites::EYE_OPEN_R).unwrap();
        assert!(lx < rx && ly < 0., "{lx} {rx} {ly}");
        assert_eq!(centroid(sprites::EYE_OPEN_L + sprites::FACINGS / 2), None);
    }

    #[test]
    fn spinning_turns_pixels_clockwise_about_the_anchor() {
        let mut source = RgbaImage::new(9, 9);
        // 锚点在正中（4.5, 4.5），右边两格的一个点，顺时针转 90° 到下边两格。
        source.put_pixel(6, 4, Rgba([200, 100, 50, 255]));
        let turned = spin(&source, (4.5, 4.5), 90.);
        assert_eq!(turned.get_pixel(4, 6).0, [200, 100, 50, 255]);
        assert_eq!(turned.get_pixel(6, 4).0[3], 0);
        assert_eq!(turned.dimensions(), (9, 9));
    }

    #[test]
    fn tone_keeps_the_middle_and_rolls_highlights_off() {
        assert_eq!(tone([0.; 3]), [0.; 3]);
        // 中间调只挪黑位。
        let [r, g, b] = tone([0.5, 0.3, 0.2]);
        assert!((r - 0.46).abs() < 1e-6 && (g - 0.26).abs() < 1e-6 && (b - 0.16).abs() < 1e-6);
        // 高光压到 1 以内，越亮越往白里褪，不倒过来。
        let mut last = tone([0.7, 0.35, 0.2]);
        for step in 1..50 {
            let lit = tone([0.7, 0.35, 0.2].map(|value| value * (1. + step as f32 * 0.2)));
            assert!(lit.iter().all(|&value| value < 1.));
            assert!(lit[0] > last[0] && lit[2] > last[2], "第 {step} 步变暗了");
            assert!(
                lit[2] / lit[0] >= last[2] / last[0],
                "第 {step} 步没往白里褪"
            );
            last = lit;
        }
    }

    #[test]
    fn tinting_keeps_highlights_apart() {
        // 一条灰阶乘 4 倍：硬截断时上半段全成 255，压过的还分得出深浅。
        let mut pixels = RgbaImage::from_fn(256, 1, |x, _| Rgba([x as u8, x as u8, x as u8, 255]));
        tint(&mut pixels, [4.; 3]);
        let at = |x: u32| pixels.get_pixel(x, 0).0;
        assert!(at(128)[0] < at(192)[0] && at(192)[0] < at(255)[0] && at(255)[0] < 255);
        assert_eq!(at(200)[3], 255);
        // 灰的还是灰的。
        assert!((0..256).all(|x| {
            let [r, g, b, _] = at(x);
            r == g && g == b
        }));
    }
}
