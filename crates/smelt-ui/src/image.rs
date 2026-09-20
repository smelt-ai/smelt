//! 图片显示统一抽象：解码 + 降采样 + 内容寻址缓存 + contain 绘制。
//!
//! 背景：ACP 图片显示反复出 bug——大图撑破屏幕、长截图只露顶部一截、超高图撞
//! sprite atlas 上限（16384px）画不出来。先后四次修复（「盒子 min_w/min_h 归
//! 零」→「预览改 absolute+inset」→「viewport_size 算显式尺寸」→「canvas 手动
//! contain + 降采样」）都在调用点各打各的补丁，第五次是迟早的事。这里把「图片
//! → 受控尺寸的 RenderImage → contain 绘制」收敛成唯一路径，调用点只描述「显
//! 示哪张图、盒子多大」：
//!
//! - 解码/降采样在后台线程执行，结果按内容 hash 缓存（同一张图多处显示只解一次）
//! - 大图降到 `MAX_PREVIEW_DIM` 以内，避免撞 sprite atlas 上限
//! - 绘制用 canvas 手动 contain，不依赖 taffy 的 aspect_ratio 推导（那是历次
//!   「撑破屏幕 / 只露顶部」的根因）
//!
//! 消息流内嵌缩略图、点击大图预览、头像显示都应走这里，不要各自写 `img()` +
//! 尺寸补丁。`Svg` 不走本路径：gpui 的 `img()` 对 `.svg` 走全彩栅格化管线，本
//! 模块的降采样用 image crate 不解码 SVG，调用点对 SVG 保留原 `img()` 渲染即可。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use gpui::*;
use sha2::{Digest, Sha256};

/// 降采样后的最大边长。gpui sprite atlas 上限 16384px，这里留足余量；
/// 长截图几万像素高时仍能完整画出来。
pub const MAX_PREVIEW_DIM: u32 = 2048;

/// 缓存条数上限：图片消息流可能很长，内容寻址缓存要防内存无限增长。
/// 超出后按 FIFO 淘汰最旧条目。
const CACHE_CAPACITY: usize = 64;

/// 正在后台解码的图片内容 hash：同一张图并发显示时只解一次。
static IN_FLIGHT: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();

struct ImageCacheInner {
    entries: HashMap<u64, Arc<RenderImage>>,
    order: VecDeque<u64>,
}

fn image_cache() -> &'static Mutex<ImageCacheInner> {
    static CACHE: OnceLock<Mutex<ImageCacheInner>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(ImageCacheInner {
            entries: HashMap::new(),
            order: VecDeque::new(),
        })
    })
}

fn content_hash(bytes: &[u8]) -> u64 {
    let digest = Sha256::digest(bytes);
    u64::from_be_bytes(digest[..8].try_into().expect("sha256 摘要至少 8 字节"))
}

/// gpui::ImageFormat → image crate 的 ImageFormat。SVG 走 gpui 的 svg_renderer，
/// image crate 不解码，返回 None。
fn to_image_format(f: gpui::ImageFormat) -> Option<image::ImageFormat> {
    Some(match f {
        gpui::ImageFormat::Png => image::ImageFormat::Png,
        gpui::ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        gpui::ImageFormat::Webp => image::ImageFormat::WebP,
        gpui::ImageFormat::Gif => image::ImageFormat::Gif,
        gpui::ImageFormat::Bmp => image::ImageFormat::Bmp,
        gpui::ImageFormat::Tiff => image::ImageFormat::Tiff,
        gpui::ImageFormat::Ico => image::ImageFormat::Ico,
        gpui::ImageFormat::Pnm => image::ImageFormat::Pnm,
        gpui::ImageFormat::Svg => return None,
    })
}

/// 同步命中：图片内容 hash 已在缓存时直接返回降采样结果。解码未完成 / 失败
/// （格式不支持、数据损坏）返回 None，调用点留空、显示占位或回退 `img()`。
pub fn cached(image: &gpui::Image) -> Option<Arc<RenderImage>> {
    let hash = content_hash(&image.bytes);
    image_cache().lock().unwrap().entries.get(&hash).cloned()
}

/// 后台解码 + 入缓存。已缓存或已在途时立即返回；否则在后台 executor 解码，
/// 完成后返回。调用点模式（解码完成需要重绘）：
///
/// ```ignore
/// if smelt_ui::image::cached(&image).is_none() {
///     let image = image.clone();
///     let this = cx.entity();
///     cx.spawn(async move |this, cx| {
///         smelt_ui::image::fetch_async(image, cx.background_executor()).await;
///         this.update(cx, |_, cx| cx.notify());
///     })
///     .detach();
/// }
/// ```
///
/// 内容寻址缓存让调用点无需自管竞态：渲染时查 `cached()` 拿「当前这张图」的
/// 结果，迟到的旧图解码不会覆盖新图的显示。
pub async fn fetch_async(image: Arc<gpui::Image>, executor: &BackgroundExecutor) {
    if cached(&image).is_some() {
        return;
    }
    let hash = content_hash(&image.bytes);
    {
        let mut in_flight = IN_FLIGHT.get_or_init(Default::default).lock().unwrap();
        if !in_flight.insert(hash) {
            return; // 已在途，等它的结果进缓存
        }
    }
    let image_clone = image.clone();
    let render = executor
        .spawn(async move { decode_preview(&image_clone) })
        .await;
    IN_FLIGHT
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .remove(&hash);
    if let Some(render) = render {
        put(hash, render);
    }
}

fn put(hash: u64, render: Arc<RenderImage>) {
    let mut cache = image_cache().lock().unwrap();
    if cache.entries.contains_key(&hash) {
        return;
    }
    cache.entries.insert(hash, render);
    cache.order.push_back(hash);
    while cache.order.len() > CACHE_CAPACITY {
        if let Some(oldest) = cache.order.pop_front() {
            cache.entries.remove(&oldest);
        }
    }
}

/// 解码 + 降采样成 RenderImage（BGRA）供 canvas 直接绘制。
///
/// 顺带把 RGBA 字节换成 gpui 纹理要的 BGRA。失败（格式不支持 / 数据损坏）返回
/// None，调用点留空即可，不阻塞 UI。解码结果同时喂给 `cached()` 的内容寻址
/// 缓存，同一张图多处显示只解一次。
pub fn decode_preview(image: &gpui::Image) -> Option<Arc<RenderImage>> {
    let format = to_image_format(image.format)?;
    let decoded = image::load_from_memory_with_format(&image.bytes, format).ok()?;
    let (w, h) = (decoded.width(), decoded.height());
    let scale = (MAX_PREVIEW_DIM as f32 / w as f32)
        .min(MAX_PREVIEW_DIM as f32 / h as f32)
        .min(1.0);
    let img = if scale < 1.0 {
        decoded.resize(
            ((w as f32) * scale) as u32,
            ((h as f32) * scale) as u32,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        decoded
    };
    let mut rgba = img.into_rgba8();
    // RGBA → BGRA：gpui 纹理格式。
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let frame = image::Frame::new(rgba);
    Some(Arc::new(RenderImage::new(vec![frame])))
}

/// contain 绘制：把 render 完整放入 bounds 并居中，不裁剪、不拉伸。
///
/// `contain_canvas` 内部用它，预览层等需要自定义 hitbox 的 canvas 也直接调它，
/// 保证所有场景的绘制路径完全一致。
pub fn paint_contain(
    bounds: Bounds<Pixels>,
    window: &mut Window,
    render: Option<&Arc<RenderImage>>,
    corner_radius: f32,
) {
    let Some(render) = render else {
        return;
    };
    let img_size = render.size(0);
    let (iw, ih) = (img_size.width.0 as f32, img_size.height.0 as f32);
    if iw <= 0.0 || ih <= 0.0 {
        return;
    }
    // contain：完整放入盒子并居中（可放大也可缩小）。
    let scale = (bounds.size.width / iw).min(bounds.size.height / ih);
    let w = iw * scale;
    let h = ih * scale;
    let origin = point(
        bounds.origin.x + (bounds.size.width - w) / 2.0,
        bounds.origin.y + (bounds.size.height - h) / 2.0,
    );
    let image_bounds = Bounds {
        origin,
        size: size(w, h),
    };
    let _ = window.paint_image(
        bounds,
        image_bounds,
        Corners::all(px(corner_radius)),
        render.clone(),
        0,
        false,
    );
}

/// contain 绘制 canvas：图片完整放入盒子并居中，不裁剪、不拉伸。
///
/// 盒子尺寸由调用点布局决定（`.h()`/`.w()`/`.size_full()` 等），绘制矩形在
/// paint 阶段按实际 bounds 自算，不依赖 taffy 的 aspect_ratio 推导——那正是
/// 历次「长截图高度被撑到几万像素、contain 失效只露顶部」的根因。`render` 为
/// None（解码未完成 / 失败）时画空白，调用点负责在解码完成后 notify 重绘。
pub fn contain_canvas(render: Option<Arc<RenderImage>>, corner_radius: f32) -> Canvas<()> {
    canvas(
        move |_bounds, _window, _cx| {},
        move |bounds, _prepaint, window, _cx| {
            paint_contain(bounds, window, render.as_ref(), corner_radius);
        },
    )
}

/// 固定高度的 contain Canvas。Canvas 没有 `img()` 的固有尺寸，单独设置高度和
/// `max_w` 会在收缩布局中得到零宽；这里根据已解码图片的比例显式给出宽度，同时
/// 保持宽度不超过调用方的上限。
fn contain_width_at_height(
    source_width: f32,
    source_height: f32,
    height: Pixels,
    max_width: Pixels,
) -> Pixels {
    if source_width > 0. && source_height > 0. {
        (height * (source_width / source_height)).min(max_width)
    } else {
        // 正常图片不会走到这里；仍保留非零布局盒，避免异常帧退化成不可见元素。
        max_width
    }
}

pub fn contain_canvas_at_height(
    render: Arc<RenderImage>,
    height: Pixels,
    max_width: Pixels,
    corner_radius: f32,
) -> Canvas<()> {
    let image_size = render.size(0);
    let width = contain_width_at_height(
        image_size.width.0 as f32,
        image_size.height.0 as f32,
        height,
        max_width,
    );
    contain_canvas(Some(render), corner_radius)
        .w(width)
        .h(height)
}

#[cfg(test)]
mod tests {
    // 注意：不能用 `use super::*`——image.rs 顶层的 `use gpui::*` 会把 gpui 的
    // `test` 属性宏带进测试模块，遮蔽标准 `#[test]`（gpui 宏会把函数体当测试
    // 上下文深度解析，rustc 直接 SIGBUS）。见 acp_view.rs 测试模块的同一惯例。
    use super::{MAX_PREVIEW_DIM, contain_width_at_height, content_hash, decode_preview};
    use std::sync::Arc;

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([255u8, 0, 0, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn make_image(bytes: Vec<u8>) -> Arc<gpui::Image> {
        Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes))
    }

    #[test]
    fn thumbnail_width_uses_source_aspect_ratio_and_cap() {
        assert_eq!(
            contain_width_at_height(400., 200., gpui::px(160.), gpui::px(280.)),
            gpui::px(280.)
        );
        assert_eq!(
            contain_width_at_height(200., 400., gpui::px(160.), gpui::px(280.)),
            gpui::px(80.)
        );
        assert_eq!(
            contain_width_at_height(0., 200., gpui::px(160.), gpui::px(280.)),
            gpui::px(280.)
        );
    }

    #[test]
    fn small_image_is_not_resized() {
        let render = decode_preview(&make_image(png_bytes(64, 48))).expect("png 应可解码");
        let size = render.size(0);
        assert_eq!(size.width.0 as u32, 64);
        assert_eq!(size.height.0 as u32, 48);
    }

    #[test]
    fn huge_image_is_downsampled() {
        // 超高长截图：高度被压到上限以内，宽度等比缩小。image crate 的 resize
        // 是分步缩小（每步最多减半，保证缩放质量），最终尺寸不精确等于目标，
        // 断言只验证「不超上限 + 确实缩小」。
        let render = decode_preview(&make_image(png_bytes(64, 6000))).expect("png 应可解码");
        let size = render.size(0);
        assert!((size.height.0 as u32) <= MAX_PREVIEW_DIM);
        assert!((size.height.0 as u32) > 1000); // 从 6000 压下来了
        assert!((size.width.0 as u32) < 64);
    }

    #[test]
    fn invalid_bytes_fail_silently() {
        assert!(decode_preview(&make_image(b"not an image".to_vec())).is_none());
    }

    #[test]
    fn content_hash_is_byte_addressed() {
        assert_eq!(
            content_hash(&png_bytes(8, 8)),
            content_hash(&png_bytes(8, 8))
        );
        assert_ne!(
            content_hash(&png_bytes(8, 8)),
            content_hash(&png_bytes(9, 8))
        );
    }
}
