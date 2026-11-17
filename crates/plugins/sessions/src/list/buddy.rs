//! 会话行首的小人头像。角色是职业 × 颜色（8 × 7 = 56 个），每个会话按种子（智能体名或会话 id）
//! 落到一个角色上。
//!
//! 不放整段动画：`scripts/buddies_render.py` 用 Blender 渲零件（身子、会转的胳膊和道具、眼睛、
//! 嘴、帽子、特效），`scripts/buddies_pack.py` 打包成 [`sprites`]，这里每帧按姿势（[`Look`]）
//! 把零件叠起来画。动作是现算的（[`moves`]）：干活时每拍随机挑一个这一行的动作，什么时候眨眼
//! 也是随机的，看久了不重样。什么时候演什么（干活、喊你、干完欢呼、被选中打招呼）由
//! [`Stage`] 按状态和选中的变化定。
//!
//! gpui 转不了彩色图，所以会转的零件每 10° 一张，画的时候取最近的那张。零件是编进来的
//! WebP，按需解：算侧栏（数据变化时）时 [`Buddy::prepare`] 把这个角色要的解好，render 只取。

mod moves;
mod sprites;
mod stage;

use std::sync::{Arc, OnceLock};

use gpui::{
    AnyElement, IntoElement as _, ObjectFit, ParentElement as _, RenderImage, Styled as _,
    StyledImage as _, div, img, px,
};
use image::Frame;

pub(super) use stage::{Stage, StageFrame};

use sprites::{ANGLES, COLORS, SPRITES};

/// 包里一张零件：在 [`sprites::BLOB`] 里的位置，画出来多大（pt），锚点离左上角多远（pt）。
pub(super) struct Sprite {
    start: usize,
    len: usize,
    size: (f32, f32),
    anchor: (f32, f32),
}

/// 一个会话的小人是哪个角色：`职业 × 颜色数 + 颜色`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Buddy(usize);

/// 职业的个数，和 [`moves::Role`] 一一对应。
const ROLES: usize = 8;

impl Buddy {
    /// `seed` 落到哪个角色。用 FNV-1a 而不是 std 的哈希：std 的不保证跨版本稳定，升级后人人
    /// 换脸。
    pub(super) fn of(seed: &str) -> Self {
        Self((fnv(seed) % (ROLES * COLORS) as u64) as usize)
    }

    fn role(self) -> moves::Role {
        moves::Role::ALL[self.0 / COLORS]
    }

    fn color(self) -> usize {
        self.0 % COLORS
    }

    /// 把这个角色要的零件解好。在数据变化时调，不在 render 里。
    pub(super) fn prepare(self) {
        for index in self.sprites() {
            texture(index);
        }
    }

    /// 这个角色可能用到的所有零件。
    fn sprites(self) -> impl Iterator<Item = usize> {
        let color = self.color();
        let role = self.role();
        let prop = role.prop().map(|prop| prop.first);
        [sprites::BODY[color], role.hat(), sprites::SHADOW]
            .into_iter()
            .chain(sprites::ARM[color]..sprites::ARM[color] + ANGLES)
            .chain(prop.into_iter().flat_map(|first| first..first + ANGLES))
            .chain(moves::SHARED.iter().flat_map(|range| range.clone()))
    }
}

fn fnv(seed: &str) -> u64 {
    seed.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// 解过的零件，留到退出（全解开也就两三 MB）。
static IMAGES: [OnceLock<Arc<RenderImage>>; SPRITES.len()] =
    [const { OnceLock::new() }; SPRITES.len()];

/// 第 `index` 张零件。没 [`Buddy::prepare`] 过的这里现解。
fn texture(index: usize) -> Arc<RenderImage> {
    IMAGES[index].get_or_init(|| decode(index)).clone()
}

/// 解成 gpui 要的 BGRA。零件是编进来的，解不开是打包出了错，直接 panic（测试会先拦下）。
fn decode(index: usize) -> Arc<RenderImage> {
    let sprite = &SPRITES[index];
    let bytes = &sprites::BLOB[sprite.start..sprite.start + sprite.len];
    let mut pixels = image::load_from_memory_with_format(bytes, image::ImageFormat::WebP)
        .expect("小人零件不是合法的 WebP")
        .into_rgba8();
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![Frame::new(pixels)]))
}

/// 会转的零件里最接近 `degrees` 的那张的序号，和它实际的角度。
fn turned(degrees: f32) -> (usize, f32) {
    let step = 360. / ANGLES as f32;
    let index = (degrees / step).round().rem_euclid(ANGLES as f32) as usize % ANGLES;
    (index, index as f32 * step)
}

/// 画面里朝 `degrees`（正上方起顺时针）的单位向量（y 朝下）。
fn toward(degrees: f32) -> (f32, f32) {
    let radians = degrees.to_radians();
    (radians.sin(), -radians.cos())
}

/// 画一帧：`look` 里的零件按锚点叠在 [`sprites::CANVAS`] 见方的画布上。零件可以伸出画布
/// （举起来的道具、头顶的特效），外面不裁。
fn figure(buddy: Buddy, look: &moves::Look) -> AnyElement {
    let mut layers = Vec::with_capacity(12);
    let (fx, fy) = sprites::FEET;
    let (sx, sy) = (1. + look.squash, 1. - look.squash);
    // 身上的点（相对脚底）落到画布上：随身子压扁、离地、左右晃。
    let on_body = |(x, y): (f32, f32)| (fx + look.sway + x * sx, fy - look.lift + y * sy);
    let on_ground = |(x, y): (f32, f32)| (fx + x, fy + y);

    let shadow = 1. - (look.lift / 16.).min(0.5);
    layers.push(layer(
        sprites::SHADOW,
        on_ground((0., 0.)),
        (shadow, shadow),
        shadow,
    ));
    if let Some(stage) = look.sprout {
        layers.push(layer(
            sprites::SPROUT + stage,
            on_ground(sprites::SPROUT_AT),
            (1., 1.),
            1.,
        ));
    }
    layers.push(layer(
        sprites::BODY[buddy.color()],
        on_body((0., 0.)),
        (sx, sy),
        1.,
    ));
    let (lx, ly) = look.gaze;
    for eye in [sprites::EYE_L, sprites::EYE_R] {
        let at = on_body((eye.0 + lx, eye.1 + ly));
        layers.push(layer(look.eyes.sprite(), at, (1., 1.), 1.));
    }
    layers.push(layer(
        look.mouth.sprite(),
        on_body(sprites::MOUTH),
        (1., 1.),
        1.,
    ));
    layers.push(layer(
        buddy.role().hat(),
        on_body(sprites::HAT),
        (1., 1.),
        1.,
    ));
    if let Some(moves::Front::Laptop(lines)) = look.front {
        layers.push(layer(
            sprites::LAPTOP + lines,
            on_body(sprites::LAPTOP_AT),
            (1., 1.),
            1.,
        ));
    }

    let arm = sprites::ARM[buddy.color()];
    let [left, right] = look.arms;
    let (left_arm, left_angle) = turned(left);
    let shoulder = on_body(sprites::SHOULDER_L);
    layers.push(layer(arm + left_arm, shoulder, (1., 1.), 1.));
    if let Some(moves::Front::Notepad(lines)) = look.front {
        let hand = reach(shoulder, left_angle);
        let at = (
            hand.0 + sprites::NOTEPAD_AT.0,
            hand.1 + sprites::NOTEPAD_AT.1,
        );
        layers.push(layer(sprites::NOTEPAD + lines, at, (1., 1.), 1.));
    }
    let (right_arm, right_angle) = turned(right);
    let shoulder = on_body(sprites::SHOULDER_R);
    layers.push(layer(arm + right_arm, shoulder, (1., 1.), 1.));
    if let Some(prop) = buddy.role().prop() {
        let (index, _) = turned(look.prop);
        layers.push(layer(
            prop.first + index,
            reach(shoulder, right_angle),
            (1., 1.),
            1.,
        ));
    }

    for effect in &look.fx {
        let at = on_body(effect.at);
        let scale = (effect.scale, effect.scale);
        layers.push(layer(effect.sprite, at, scale, effect.alpha));
    }

    div()
        .absolute()
        .top_0()
        .left_0()
        .size(px(sprites::CANVAS))
        .children(layers)
        .into_any_element()
}

/// 从肩（画布上）朝 `angle` 伸一条胳膊，手在哪。
fn reach(shoulder: (f32, f32), angle: f32) -> (f32, f32) {
    let (dx, dy) = toward(angle);
    (
        shoulder.0 + dx * sprites::ARM_LEN,
        shoulder.1 + dy * sprites::ARM_LEN,
    )
}

/// 一张零件：锚点放到 `at`（画布上，pt），按 `scale` 缩放。
fn layer(index: usize, at: (f32, f32), scale: (f32, f32), alpha: f32) -> AnyElement {
    let sprite = &SPRITES[index];
    img(texture(index))
        .object_fit(ObjectFit::Fill)
        .absolute()
        .left(px(at.0 - sprite.anchor.0 * scale.0))
        .top(px(at.1 - sprite.anchor.1 * scale.1))
        .w(px(sprite.size.0 * scale.0))
        .h(px(sprite.size.1 * scale.1))
        .opacity(alpha.clamp(0., 1.))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sprite_decodes_at_its_size() {
        for index in 0..SPRITES.len() {
            let size = texture(index).size(0);
            let sprite = &SPRITES[index];
            assert_eq!(
                (size.width.0 as f32, size.height.0 as f32),
                (sprite.size.0 * 2., sprite.size.1 * 2.),
                "零件 {index} 的大小和索引对不上"
            );
        }
    }

    #[test]
    fn roles_match_the_rendered_hats() {
        assert_eq!(moves::Role::ALL.len(), ROLES);
        assert_eq!(sprites::HAT_SWEEPER - sprites::HAT_CODER + 1, ROLES);
    }

    #[test]
    fn a_seed_always_picks_the_same_character() {
        // 钉住具体值：换了哈希或角色数，已有会话的小人会全部换人。
        assert_eq!(ROLES * COLORS, 56);
        assert_eq!(Buddy::of("session-1"), Buddy(5));
        assert_eq!(Buddy::of("写作助手"), Buddy(6));
    }

    #[test]
    fn seeds_spread_over_characters() {
        let spread: std::collections::HashSet<_> = (0..400)
            .map(|n| Buddy::of(&format!("session-{n}")).0)
            .collect();
        assert!(spread.len() > 40, "只落到 {} 个角色上", spread.len());
    }

    #[test]
    fn turned_picks_the_nearest_rendered_angle() {
        assert_eq!(turned(0.), (0, 0.));
        assert_eq!(turned(14.), (1, 10.));
        assert_eq!(turned(356.), (0, 0.));
        assert_eq!(turned(-20.), (34, 340.));
        assert_eq!(turned(725.), (1, 10.));
    }
}
