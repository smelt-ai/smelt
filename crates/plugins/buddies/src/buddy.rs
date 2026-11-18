//! 小人：一个小人由几个槽位拼成，职业（拿什么、怎么干活）、身子色、头顶戴的、脸上戴的、脖子上
//! 戴的（后两样可以没有），戴的东西还各有颜色（不和身子撞色），还有性格（动起来什么样）。全按
//! 种子（智能体 id 或会话 id）落，不让人挑：同一个种子永远是同一个小人。
//!
//! 不放整段动画：`scripts/buddies_render.py` 用 Blender 渲零件（身子、会转的胳膊和道具、眼睛、
//! 嘴、戴的、特效，赛车手的车；身子、脸和戴的每个朝向一张，能转身），`scripts/buddies_pack.py` 打包成 [`sprites`]，这里每帧按姿势
//! （[`Look`]）把零件叠起来画。动作是现算的（[`moves`]）：干活时每拍随机挑一个这一行的动作，
//! 什么时候眨眼也是随机的，看久了不重样。什么时候演什么（干活、喊你、干完欢呼、被选中打招呼）
//! 由 [`Stage`] 按状态和露着的会话的变化定。
//!
//! 零件怎么上色、转角度、叠成一帧见 [`paint`]。数据变化时 [`Buddy::prepare`] 把这个小人要的
//! 零件画好；每帧只叠，样子没变的帧不重叠（[`Portrait`]）。

mod moves;
mod paint;
mod sprites;
mod stage;

use std::sync::Arc;

use gpui::{
    AnyElement, App, IntoElement as _, ObjectFit, RenderImage, Styled as _, StyledImage as _,
    Window, img, px,
};

pub(crate) use stage::Stage;

use moves::{Dice, Look, Role, Temper};
use paint::{Canvas, MARGIN, Paint, TURN, TURNS, Tint, centroid};

/// 包里一张零件：在 [`sprites::BLOB`] 里的位置，锚点离左上角多远（pt）。多大看解出来的图。
pub(super) struct Sprite {
    start: usize,
    len: usize,
    anchor: (f32, f32),
}

/// 一件戴的东西：可换色的主料（中性灰，乘色）和固定色的点缀各是哪张，至少有一张。两张的
/// 大小、锚点一样，叠着画。`eyes` 是占不占眼睛（护目镜、眼镜），占眼睛的一个小人只戴一件。
pub(super) struct Wearable {
    base: Option<usize>,
    trim: Option<usize>,
    eyes: bool,
}

/// 一个小人。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Buddy {
    role: Role,
    /// 第几种身子色（[`sprites::BODY_TINTS`]）。
    body: usize,
    head: Worn,
    face: Option<Worn>,
    neck: Option<Worn>,
    temper: Temper,
}

/// 戴着的一件：对应表里第几件，主料第几种颜色（[`sprites::WEAR_TINTS`]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Worn {
    item: usize,
    color: usize,
}

/// 脸上、脖子上各有多少小人戴：戴满了显得挤。
const WEAR_ODDS: f32 = 0.35;

impl Worn {
    /// 表里 `items` 件挑一件，颜色从配得上身子色 `body` 的里挑。
    fn pick(dice: &mut Dice, items: usize, body: usize) -> Self {
        let item = dice.below(items);
        let allowed = sprites::WEAR_FOR_BODY[body];
        let nth = dice.below(allowed.count_ones() as usize);
        let color = (0..sprites::WEAR_TINTS.len())
            .filter(|color| (allowed >> color) & 1 == 1)
            .nth(nth)
            .expect("每种身子色都配得上几种戴的颜色");
        Self { item, color }
    }

    fn maybe(dice: &mut Dice, items: usize, body: usize) -> Option<Self> {
        let worn = Self::pick(dice, items, body);
        (dice.unit() < WEAR_ODDS).then_some(worn)
    }
}

impl Buddy {
    /// `seed` 落到哪个小人：种子哈希后起一串随机数，按固定顺序一个槽位取一个。哈希用 FNV-1a
    /// 而不是 std 的：std 的不保证跨版本稳定，升级后人人换脸。新加槽位接在最后取，前面的不变。
    pub(crate) fn of(seed: &str) -> Self {
        let mut dice = Dice::new(fnv(seed));
        let role = Role::ALL[dice.below(Role::ALL.len())];
        let body = dice.below(sprites::BODY_TINTS.len());
        let head = Worn::pick(&mut dice, sprites::HEADS.len(), body);
        // 脸上的照样取（后面的槽位取到的数不跟着变），头顶已经占了眼睛就不戴。
        let face = Worn::maybe(&mut dice, sprites::FACES.len(), body)
            .filter(|_| !sprites::HEADS[head.item].eyes);
        let neck = Worn::maybe(&mut dice, sprites::NECKS.len(), body);
        let temper = Temper::roll(&mut dice);
        Self {
            role,
            body,
            head,
            face,
            neck,
            temper,
        }
    }

    fn role(self) -> Role {
        self.role
    }

    fn skin(self) -> Tint {
        Tint::Body(self.body)
    }

    /// 把这个小人要的零件画好（画过的跳过）。在数据变化时调，不在 render 里。
    pub(crate) fn prepare(self) {
        for paint in self.paints() {
            paint.pixels();
        }
    }

    /// 这个小人可能用到的所有零件。
    fn paints(self) -> Vec<Paint> {
        let mut paints = vec![Paint::plain(sprites::SHADOW)];
        for facing in 0..sprites::FACINGS {
            paints.push(Paint::tinted(sprites::BODY + facing, self.skin()));
            paints.push(Paint::plain(sprites::BLUSH + facing));
            paints.extend(self.wearing().map(|worn| worn.at(facing)));
        }
        paints.extend((0..TURNS).map(|turn| Paint::turned(sprites::ARM, turn, self.skin())));
        if let Some(first) = self.role.held() {
            paints.extend((0..TURNS).map(|turn| Paint::turned(first, turn, Tint::None)));
        }
        let plain = self
            .role
            .own_sprites()
            .chain(moves::SHARED.iter().flat_map(|range| range.clone()));
        paints.extend(plain.map(Paint::plain));
        paints
    }

    /// 戴着的要画的几层，从里到外：脸上、脖子上、头顶，每件先主料（乘色）再点缀。都和身子同框，
    /// 跟着身子压扁、转身；一件的主料、点缀按同一张挪（`guide`），转身时不会错开。
    fn wearing(self) -> impl Iterator<Item = Facing> {
        let on = |table: &'static [Wearable], worn: Worn| (&table[worn.item], worn.color);
        [
            self.face.map(|worn| on(&sprites::FACES, worn)),
            self.neck.map(|worn| on(&sprites::NECKS, worn)),
            Some(on(&sprites::HEADS, self.head)),
        ]
        .into_iter()
        .flatten()
        .flat_map(|(wearable, color)| {
            let guide = wearable.base.or(wearable.trim);
            let layer = move |first, tint| Facing {
                first,
                tint,
                at: (0., 0.),
                guide,
            };
            let base = wearable.base.map(|first| layer(first, Tint::Wear(color)));
            let trim = wearable.trim.map(|first| layer(first, Tint::None));
            base.into_iter().chain(trim)
        })
    }

    /// 跟着身子转身的几层，从下往上：身子、腮红、眼睛（跟着 `look` 往哪看挪）、嘴、戴的。
    fn turning(self, look: &Look) -> Vec<Facing> {
        let [left_eye, right_eye] = look.eyes.sprites();
        let mouth = look.mouth.sprite();
        let feature = |first, at| Facing {
            first,
            tint: Tint::None,
            at,
            guide: Some(first),
        };
        let mut layers = vec![
            // 身子差不多是圆的，转过去轮廓不怎么变，不挪。
            Facing {
                first: sprites::BODY,
                tint: self.skin(),
                at: (0., 0.),
                guide: None,
            },
            feature(sprites::BLUSH, (0., 0.)),
            feature(left_eye, look.gaze),
            feature(right_eye, look.gaze),
            feature(mouth, (0., 0.)),
        ];
        layers.extend(self.wearing());
        layers
    }
}

/// 跟着身子转身的一层：哪组零件（每个朝向一张，第一张是 `first`）、乘什么色、另外挪多少（pt）。
/// 转身夹在两个朝向之间时，按 `guide` 那组零件在两个朝向里的重心差挪过去（[`Facing::draw`]）；
/// 没有的不挪。
#[derive(Clone, Copy)]
struct Facing {
    first: usize,
    tint: Tint,
    at: (f32, f32),
    guide: Option<usize>,
}

impl Facing {
    /// 第 `facing` 个朝向的那张。
    fn at(self, facing: usize) -> Paint {
        Paint::tinted(self.first + facing, self.tint)
    }

    /// 画第 `facing` 个朝向的那张，往第 `other` 个朝向里它的位置挪 `toward`（0 到 1）。转身时
    /// 两个朝向各画一张、互相往对方挪，再按转到哪混起来（[`Canvas::blend`]）：五官两张落在
    /// 同一处，只是渐变，不会一只眼睛看着有两个影子。
    fn draw(
        self,
        canvas: &mut Canvas,
        facing: usize,
        other: usize,
        toward: f32,
        on_body: impl Fn((f32, f32)) -> (f32, f32),
        squash: (f32, f32),
    ) {
        let shift = self
            .guide
            .filter(|_| toward > 0.)
            .and_then(|first| Some((centroid(first + facing)?, centroid(first + other)?)))
            .map_or((0., 0.), |(from, to)| {
                ((to.0 - from.0) * toward, (to.1 - from.1) * toward)
            });
        let at = (self.at.0 + shift.0, self.at.1 + shift.1);
        canvas.layer(self.at(facing), on_body(at), squash, 1.);
    }
}

fn fnv(seed: &str) -> u64 {
    seed.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// 会转的零件转到 `degrees`，最接近的是第几格（每格 [`TURN`]°），和它实际的角度。
fn turned(degrees: f32) -> (usize, f32) {
    let index = (degrees / TURN).round().rem_euclid(TURNS as f32) as usize % TURNS;
    (index, index as f32 * TURN)
}

/// 转身转到 `degrees` 夹在哪两个朝向之间（每 360 / [`sprites::FACINGS`] 度一张）：前一个、
/// 后一个，往后一个走了多少（0 到 1）。
fn between(degrees: f32) -> (usize, usize, f32) {
    const FACINGS: usize = sprites::FACINGS;
    let step = degrees.rem_euclid(360.) / 360. * FACINGS as f32;
    let index = step.floor() as usize % FACINGS;
    (index, (index + 1) % FACINGS, step.fract())
}

/// 转身转到 `turn` 时左（`side` 0）、右（1）肩在哪（相对脚底），转没转到身子后面。夹在两个
/// 朝向之间时按转到哪插。
fn shoulder(turn: f32, side: usize) -> ((f32, f32), bool) {
    // 离镜头比身子中轴远这么多（pt）才算在后面：正面时肩在身子两边，随视角差一点点不算。
    const BEHIND: f32 = 2.;
    let (from, to, toward) = between(turn);
    let (a, b) = (sprites::SHOULDERS[from][side], sprites::SHOULDERS[to][side]);
    let mix = |a: f32, b: f32| a + (b - a) * toward;
    ((mix(a.0, b.0), mix(a.1, b.1)), mix(a.2, b.2) > BEHIND)
}

/// 胳膊在身子上朝 `angle`（正面看的画面角度），身子转到 `turn` 时在画面里朝哪：伸向两边的
/// 那一份随转身按余弦缩短，上下的不变，侧过身时胳膊看着垂在身侧，转到背面左右反过来，中间
/// 是连着的。
fn foreshortened(angle: f32, turn: f32) -> f32 {
    let squeeze = turn.to_radians().cos();
    let (sin, cos) = angle.to_radians().sin_cos();
    (sin * squeeze).atan2(cos).to_degrees()
}

/// 车身歪的角度里最接近 `degrees` 的那张的序号（在车头、车尾各自组里），和它实际的角度。
/// 车每 10° 一张，左右对称。
fn leaned(degrees: f32) -> (usize, f32) {
    const STEPS: usize = sprites::KART_BACK - sprites::KART_FRONT;
    let most = (STEPS / 2) as f32;
    let step = (degrees / 10.).round().clamp(-most, most);
    ((step + most) as usize, step * 10.)
}

/// `point` 在画面里绕原点顺时针转 `degrees`（y 朝下）。
fn rotated((x, y): (f32, f32), degrees: f32) -> (f32, f32) {
    let (sin, cos) = degrees.to_radians().sin_cos();
    (x * cos - y * sin, x * sin + y * cos)
}

/// 画面里朝 `degrees`（正上方起顺时针）的单位向量（y 朝下）。
fn toward(degrees: f32) -> (f32, f32) {
    let radians = degrees.to_radians();
    (radians.sin(), -radians.cos())
}

/// 画布边长（pt）。
pub(crate) const CANVAS: f32 = sprites::CANVAS;

/// 一个小人头像在画的那一帧。样子没变就接着用；变了另叠一张，旧的从 gpui 的贴图里删掉，不然
/// 动起来每帧一张，贴图越攒越多。
#[derive(Default)]
pub(crate) struct Portrait {
    shown: Option<(Buddy, Look, Arc<RenderImage>)>,
}

impl Portrait {
    /// 画 `buddy` 摆着 `look` 的样子：盖住 [`CANVAS`] 见方的画布，四周伸出去 [`MARGIN`]（举起来
    /// 的道具、头顶的特效），外面不裁。
    fn show(&mut self, buddy: Buddy, look: Look, window: &mut Window) -> AnyElement {
        let image = match &self.shown {
            Some((shown, seen, image)) if *shown == buddy && *seen == look => image.clone(),
            _ => {
                let image = figure(buddy, &look);
                if let Some((_, _, stale)) = self.shown.replace((buddy, look, image.clone())) {
                    window.drop_image(stale).ok();
                }
                image
            }
        };
        img(image)
            .object_fit(ObjectFit::Fill)
            .absolute()
            .left(px(-MARGIN))
            .top(px(-MARGIN))
            .size(px(CANVAS + 2. * MARGIN))
            .into_any_element()
    }

    /// 头像关掉时调：画着的那张从各个窗口的贴图里删掉。
    pub(crate) fn release(&mut self, cx: &mut App) {
        if let Some((_, _, image)) = self.shown.take() {
            cx.drop_image(image, None);
        }
    }
}

/// 叠一帧：`look` 里的零件按锚点叠在 [`CANVAS`] 见方的画布上，先叠的在下面。转身时身子、
/// 脸、戴的取夹着的两个朝向混起来（[`Facing::draw`]）；转到身子后面的胳膊垫在身子底下。
fn figure(buddy: Buddy, look: &Look) -> Arc<RenderImage> {
    const SAME: (f32, f32) = (1., 1.);
    let mut canvas = Canvas::new();
    let (fx, fy) = sprites::FEET;
    let squash = (1. + look.squash, 1. - look.squash);
    // 身上的点（相对脚底）落到画布上：随身子压扁、离地、左右晃。
    let on_body =
        |(x, y): (f32, f32)| (fx + look.sway + x * squash.0, fy - look.lift + y * squash.1);
    let on_ground = |(x, y): (f32, f32)| (fx + x, fy + y);

    let shadow = 1. - (look.lift / 16.).min(0.5);
    // 车比人宽，影子拉宽些。
    let wide = if look.kart.is_some() { 1.5 } else { 1. };
    canvas.layer(
        Paint::plain(sprites::SHADOW),
        on_ground((0., 0.)),
        (shadow * wide, shadow),
        shadow,
    );
    if let Some(stage) = look.sprout {
        let sprout = Paint::plain(sprites::SPROUT + stage);
        canvas.layer(sprout, on_ground(sprites::SPROUT_AT), SAME, 1.);
    }
    // 车尾垫在身子后面，车头和方向盘挡在身子前面、手后面。车停在地上，不跟着身子晃。
    let kart = look.kart.map(|kart| (kart, leaned(kart.lean)));
    if let Some((_, (index, _))) = kart {
        let back = Paint::plain(sprites::KART_BACK + index);
        canvas.layer(back, on_ground((0., 0.)), SAME, 1.);
    }
    let arms = [0, 1].map(|side| {
        let (at, behind) = shoulder(look.turn, side);
        let angle = foreshortened(look.arms[side], look.turn);
        (
            Arm {
                side,
                shoulder: on_body(at),
                angle,
            },
            behind,
        )
    });
    for (arm, _) in arms.iter().filter(|(_, behind)| *behind) {
        arm.draw(&mut canvas, buddy, look);
    }
    // 腮红单独渲，叠在乘过色的身子上，不跟着身子色变。眼睛、嘴、戴的都和身子同框。差一点就
    // 到某个朝向时只画那一个，省得每帧多叠两张。
    const NEAR: f32 = 0.02;
    let layers = buddy.turning(look);
    let (from, to, toward) = between(look.turn);
    let draw = |canvas: &mut Canvas, facing, other, toward| {
        for layer in &layers {
            layer.draw(canvas, facing, other, toward, on_body, squash);
        }
    };
    if toward < NEAR {
        draw(&mut canvas, from, to, 0.);
    } else if toward > 1. - NEAR {
        draw(&mut canvas, to, from, 0.);
    } else {
        let (mut before, mut after) = (Canvas::new(), Canvas::new());
        draw(&mut before, from, to, toward);
        draw(&mut after, to, from, 1. - toward);
        canvas.blend(&before, &after, toward);
    }
    if let Some(moves::Front::Laptop(lines)) = look.front {
        let laptop = Paint::plain(sprites::LAPTOP + lines);
        canvas.layer(laptop, on_body(sprites::LAPTOP_AT), SAME, 1.);
    }

    if let Some((kart, (index, angle))) = kart {
        let front = Paint::plain(sprites::KART_FRONT + index);
        canvas.layer(front, on_ground((0., 0.)), SAME, 1.);
        let (wheel, _) = turned(kart.steer);
        canvas.layer(
            Paint::turned(sprites::WHEEL, wheel, Tint::None),
            on_ground(rotated(sprites::WHEEL_AT, angle)),
            SAME,
            1.,
        );
    }

    for (arm, _) in arms.iter().filter(|(_, behind)| !*behind) {
        arm.draw(&mut canvas, buddy, look);
    }

    for effect in &look.fx {
        let scale = (effect.scale, effect.scale);
        let paint = Paint::plain(effect.sprite);
        canvas.layer(paint, on_body(effect.at), scale, effect.alpha);
    }
    canvas.finish()
}

/// 一条胳膊：左（`side` 0）还是右（1），肩在画布上哪，在画面里朝哪。
struct Arm {
    side: usize,
    shoulder: (f32, f32),
    angle: f32,
}

impl Arm {
    /// 画胳膊和手里拿的：左手的本子，右手的道具。
    fn draw(&self, canvas: &mut Canvas, buddy: Buddy, look: &Look) {
        let (turn, angle) = turned(self.angle);
        let arm = Paint::turned(sprites::ARM, turn, buddy.skin());
        canvas.layer(arm, self.shoulder, (1., 1.), 1.);
        let hand = reach(self.shoulder, angle);
        match self.side {
            0 => {
                if let Some(moves::Front::Notepad(lines)) = look.front {
                    let at = (
                        hand.0 + sprites::NOTEPAD_AT.0,
                        hand.1 + sprites::NOTEPAD_AT.1,
                    );
                    canvas.layer(Paint::plain(sprites::NOTEPAD + lines), at, (1., 1.), 1.);
                }
            }
            _ => {
                if let Some(prop) = buddy.role().prop() {
                    let (index, _) = turned(look.prop);
                    let held = Paint::turned(prop.first, index, Tint::None);
                    canvas.layer(held, hand, (1., 1.), 1.);
                }
            }
        }
    }
}

/// 从肩（画布上）朝 `angle` 伸一条胳膊，手在哪。
fn reach(shoulder: (f32, f32), angle: f32) -> (f32, f32) {
    let (dx, dy) = toward(angle);
    (
        shoulder.0 + dx * sprites::ARM_LEN,
        shoulder.1 + dy * sprites::ARM_LEN,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use moves::{Curiosity, Flourish, Pace, Spring};
    use sprites::SPRITES;

    /// 第 `sprite` 张解出来多大（pt）。
    fn size(sprite: usize) -> (f32, f32) {
        let (width, height) = Paint::plain(sprite).pixels().dimensions();
        (width as f32 / paint::PX, height as f32 / paint::PX)
    }

    #[test]
    fn a_seed_always_picks_the_same_buddy() {
        // 钉住具体值：换了哈希、槽位顺序或某个槽位的选项数，已有会话的小人会全部换人。
        assert_eq!(
            Buddy::of("session-1"),
            Buddy {
                role: Role::Detective,
                body: 6,
                head: Worn { item: 1, color: 0 },
                face: Some(Worn { item: 0, color: 0 }),
                neck: Some(Worn { item: 0, color: 1 }),
                temper: Temper {
                    pace: Pace::Brisk,
                    spring: Spring::Wobbly,
                    curiosity: Curiosity::Restless,
                    flourish: Flourish::Bounce,
                },
            }
        );
        assert_eq!(
            Buddy::of("写作助手"),
            Buddy {
                role: Role::Fixer,
                body: 9,
                head: Worn { item: 2, color: 2 },
                face: Some(Worn { item: 0, color: 6 }),
                neck: Some(Worn { item: 0, color: 1 }),
                temper: Temper {
                    pace: Pace::Calm,
                    spring: Spring::Soft,
                    curiosity: Curiosity::Focused,
                    flourish: Flourish::Spin,
                },
            }
        );
    }

    #[test]
    fn seeds_spread_over_buddies() {
        let buddies: Vec<_> = (0..400)
            .map(|n| Buddy::of(&format!("session-{n}")))
            .collect();
        let distinct: std::collections::HashSet<_> = buddies.iter().collect();
        assert!(distinct.len() > 390, "只有 {} 个不同的小人", distinct.len());
        let roles: std::collections::HashSet<_> = buddies.iter().map(|b| b.role).collect();
        assert_eq!(roles.len(), Role::ALL.len());
        // 性格四样各三种，一共 81 种，400 个里差不多都该出现过。
        let tempers: std::collections::HashSet<_> = buddies.iter().map(|b| b.temper).collect();
        assert!(tempers.len() > 70, "只有 {} 种性格", tempers.len());
        let wearing = |slot: fn(&Buddy) -> bool| buddies.iter().filter(|&b| slot(b)).count();
        for count in [wearing(|b| b.face.is_some()), wearing(|b| b.neck.is_some())] {
            assert!((90..190).contains(&count), "{count} 个戴着");
        }
    }

    #[test]
    fn wear_never_matches_the_body() {
        for body in 0..sprites::BODY_TINTS.len() {
            let allowed = sprites::WEAR_FOR_BODY[body];
            assert!(allowed.count_ones() >= 4, "身子色 {body} 能配的颜色太少");
            assert_eq!(allowed >> sprites::WEAR_TINTS.len(), 0);
        }
        for n in 0..400 {
            let buddy = Buddy::of(&format!("session-{n}"));
            for worn in [Some(buddy.head), buddy.face, buddy.neck]
                .into_iter()
                .flatten()
            {
                assert_eq!((sprites::WEAR_FOR_BODY[buddy.body] >> worn.color) & 1, 1);
            }
        }
    }

    #[test]
    fn eyewear_is_worn_once() {
        let eyes =
            |table: &[Wearable], worn: Option<Worn>| worn.is_some_and(|w| table[w.item].eyes);
        let mut goggles = 0;
        for n in 0..2000 {
            let buddy = Buddy::of(&format!("session-{n}"));
            let head = eyes(&sprites::HEADS, Some(buddy.head));
            goggles += usize::from(head);
            assert!(
                !(head && eyes(&sprites::FACES, buddy.face)),
                "第 {n} 个戴了两副"
            );
        }
        assert!(goggles > 0);
    }

    #[test]
    fn wearables_stack_in_one_frame() {
        for table in [
            &sprites::HEADS[..],
            &sprites::FACES[..],
            &sprites::NECKS[..],
        ] {
            for wearable in table {
                let parts: Vec<_> = wearable.base.iter().chain(&wearable.trim).collect();
                assert!(!parts.is_empty());
                for part in &parts[1..] {
                    let (a, b) = (*parts[0], **part);
                    assert_eq!((size(a), SPRITES[a].anchor), (size(b), SPRITES[b].anchor));
                }
            }
        }
    }

    #[test]
    fn turned_picks_the_nearest_step() {
        assert_eq!(turned(0.), (0, 0.));
        assert_eq!(turned(14.), (3, 15.));
        assert_eq!(turned(358.), (0, 0.));
        assert_eq!(turned(-20.), (68, 340.));
        assert_eq!(turned(725.), (1, 5.));
    }

    #[test]
    fn between_finds_the_two_facings_around_a_turn() {
        const STEP: f32 = 360. / sprites::FACINGS as f32;
        const LAST: usize = sprites::FACINGS - 1;
        assert_eq!(between(0.), (0, 1, 0.));
        assert_eq!(between(STEP * 1.25), (1, 2, 0.25));
        assert_eq!(between(-STEP / 2.), (LAST, 0, 0.5));
        assert_eq!(between(360.), (0, 1, 0.));
        assert_eq!(between(-1.).0, LAST);
    }

    #[test]
    fn arms_foreshorten_with_the_turn() {
        let close = |a: f32, b: f32| (a - b).rem_euclid(360.).min((b - a).rem_euclid(360.)) < 0.5;
        // 正面不变。
        for angle in [0., 90., 150., 210., 330.] {
            assert!(close(foreshortened(angle, 0.), angle), "{angle}");
        }
        // 背面左右反过来，上下不变。
        assert!(close(foreshortened(150., 180.), 210.));
        assert!(close(foreshortened(0., 180.), 0.));
        // 侧过身：伸向外边的胳膊收向垂直，往哪边侧都一样；正侧面垂直。
        for turn in [70., 290., -70.] {
            let side = foreshortened(120., turn);
            assert!(side > 140. && side < 180., "{turn}: {side}");
        }
        assert!(close(foreshortened(120., 90.), 180.));
        // 转身是连着的：每转 1° 胳膊在画面里转得不多。
        for turn in 0..360 {
            let (a, b) = (
                foreshortened(120., turn as f32),
                foreshortened(120., turn as f32 + 1.),
            );
            let step = (a - b).rem_euclid(360.).min((b - a).rem_euclid(360.));
            assert!(step < 3., "{turn}: {a} → {b}");
        }
    }

    #[test]
    fn shoulders_go_behind_when_turned_away() {
        // 正面两条胳膊都在身子前面；转到右边（右肩往后转）右胳膊在后面、左胳膊在前面。
        assert!(!shoulder(0., 0).1 && !shoulder(0., 1).1);
        assert!(!shoulder(90., 0).1 && shoulder(90., 1).1);
        assert!(shoulder(-90., 0).1 && !shoulder(-90., 1).1);
    }

    #[test]
    fn every_facing_has_its_parts() {
        // 跟着身子转的零件组每组都有 FACINGS 张（框各按那个朝向的身子，大小不一样）。
        let groups = [
            sprites::BODY,
            sprites::BLUSH,
            sprites::EYE_OPEN_L,
            sprites::EYE_OPEN_R,
            sprites::MOUTH_SMILE,
            sprites::MOUTH_OPEN,
        ];
        for first in groups {
            for facing in 0..sprites::FACINGS {
                let (width, height) = size(first + facing);
                let anchor = SPRITES[first + facing].anchor;
                // 都和身子同框：锚点是脚底，落在框里。
                assert!(anchor.0 > 0. && anchor.0 < width);
                assert!(anchor.1 > 0. && anchor.1 <= height);
            }
        }
        assert_eq!(sprites::BLUSH - sprites::BODY, sprites::FACINGS);
        assert_eq!(sprites::MOUTH_FLAT - sprites::MOUTH_SMILE, sprites::FACINGS);
    }

    #[test]
    fn leaned_stays_within_the_rendered_kart() {
        assert_eq!(leaned(0.), (3, 0.));
        assert_eq!(leaned(-14.), (2, -10.));
        assert_eq!(leaned(26.), (6, 30.));
        assert_eq!(leaned(-90.), (0, -30.));
    }
}
