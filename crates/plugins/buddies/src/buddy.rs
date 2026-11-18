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
//! 零件画好；每帧只叠，样子没变的帧不重叠（[`Portrait`]）。头顶挂着的部分（天线、毛线帽的球、
//! 芽）跟着身子慢半拍地晃，要接着上一帧算，状态放在 [`Portrait`] 里（[`swing`]）。

mod moves;
mod paint;
mod sprites;
mod stage;
mod swing;

use std::sync::Arc;
use std::time::Instant;

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

/// 一件戴的东西：可换色的主料（中性灰，乘色）和固定色的点缀各是哪张。两张的大小、锚点一样，
/// 叠着画。`eyes` 是占不占眼睛（护目镜、眼镜），占眼睛的一个小人只戴一件。头顶会晃的部分另
/// 记在 `dangle`；整件都晃的（天线、芽）主料、点缀都没有。
pub(super) struct Wearable {
    base: Option<usize>,
    trim: Option<usize>,
    eyes: bool,
    dangle: Option<Dangle>,
}

/// 一件戴的东西上挂着、会晃的部分：主料、点缀各是哪组（和 [`Wearable`] 的一样，每个朝向一张、
/// 和身子同框），每个朝向挂点在哪（相对脚底，pt）。画的时候绕挂点转（[`swing`]）。
pub(super) struct Dangle {
    base: Option<usize>,
    trim: Option<usize>,
    pivots: [(f32, f32); sprites::FACINGS],
}

impl Dangle {
    /// 身子转到 `turn` 时挂点在哪，和静止时挂着的部分的重心离挂点多远（都相对脚底，pt）。
    fn at(&self, turn: f32) -> ((f32, f32), (f32, f32)) {
        let (from, to, toward) = between(turn);
        let mix =
            |a: (f32, f32), b: (f32, f32)| (a.0 + (b.0 - a.0) * toward, a.1 + (b.1 - a.1) * toward);
        // 两个朝向各算一份再插，转身时不跳。
        let reach = |facing: usize| {
            let pivot = self.pivots[facing];
            self.base
                .or(self.trim)
                .and_then(|first| centroid(first + facing))
                .map_or((0., -1.), |center| (center.0 - pivot.0, center.1 - pivot.1))
        };
        (
            mix(self.pivots[from], self.pivots[to]),
            mix(reach(from), reach(to)),
        )
    }
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
            paints.extend(self.wearing(0.).map(|worn| worn.at(facing)));
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

    /// 头顶戴的那件上挂着、会晃的部分。
    fn dangle(self) -> Option<&'static Dangle> {
        sprites::HEADS[self.head.item].dangle.as_ref()
    }

    /// 戴着的要画的几层，从里到外：脸上、脖子上、头顶，每件先主料（乘色）再点缀，最后是头顶
    /// 挂着的部分（绕挂点偏 `swing` 度）。都和身子同框，跟着身子压扁、转身；一件的主料、点缀按
    /// 同一张挪（`guide`），转身时不会错开。抬头低头时脸上的跟着五官挪，头顶的在球顶上、挪得
    /// 少，脖子上的在脸下面、反着挪一点。
    fn wearing(self, swing: f32) -> impl Iterator<Item = Facing> {
        let on = |table: &'static [Wearable], worn: Worn, nod| (&table[worn.item], worn.color, nod);
        [
            self.face.map(|worn| on(&sprites::FACES, worn, 1.)),
            self.neck.map(|worn| on(&sprites::NECKS, worn, -0.25)),
            Some(on(&sprites::HEADS, self.head, 0.35)),
        ]
        .into_iter()
        .flatten()
        .flat_map(move |(wearable, color, nod)| {
            let guide = wearable.base.or(wearable.trim);
            let layer = move |first, tint| Facing {
                first,
                tint,
                at: (0., 0.),
                guide,
                nod,
                hinge: None,
            };
            let base = wearable.base.map(|first| layer(first, Tint::Wear(color)));
            let trim = wearable.trim.map(|first| layer(first, Tint::None));
            let hung = wearable
                .dangle
                .as_ref()
                .into_iter()
                .flat_map(move |dangle| {
                    let guide = dangle.base.or(dangle.trim);
                    let hinge = Some(Hinge {
                        pivots: &dangle.pivots,
                        angle: swing,
                    });
                    let layer = move |first, tint| Facing {
                        guide,
                        hinge,
                        ..layer(first, tint)
                    };
                    let base = dangle.base.map(|first| layer(first, Tint::Wear(color)));
                    base.into_iter()
                        .chain(dangle.trim.map(|first| layer(first, Tint::None)))
                });
            base.into_iter().chain(trim).chain(hung)
        })
    }

    /// 跟着身子转身的几层，从下往上：身子、腮红、眼睛（跟着 `look` 往哪看挪，眨一只眼的那只
    /// 闭上）、嘴、戴的（头顶挂着的部分偏 `swing` 度）。
    fn turning(self, look: &Look, swing: f32) -> Vec<Facing> {
        let mut eyes = look.eyes.sprites();
        if let Some(side) = look.wink {
            eyes[side] = moves::Eyes::Shut.sprites()[side];
        }
        let [left_eye, right_eye] = eyes;
        let mouth = look.mouth.sprite();
        let feature = |first, at| Facing {
            first,
            tint: Tint::None,
            at,
            guide: Some(first),
            nod: 1.,
            hinge: None,
        };
        let mut layers = vec![
            // 身子差不多是圆的，转过去、抬头低头轮廓都不怎么变，不挪。
            Facing {
                first: sprites::BODY,
                tint: self.skin(),
                at: (0., 0.),
                guide: None,
                nod: 0.,
                hinge: None,
            },
            Facing {
                nod: 0.9,
                ..feature(sprites::BLUSH, (0., 0.))
            },
            feature(left_eye, look.gaze),
            feature(right_eye, look.gaze),
            feature(mouth, (0., 0.)),
        ];
        layers.extend(self.wearing(swing));
        layers
    }
}

/// 跟着身子转身的一层：哪组零件（每个朝向一张，第一张是 `first`）、乘什么色、另外挪多少（pt）。
/// 转身夹在两个朝向之间时，按 `guide` 那组零件在两个朝向里的重心差挪过去（[`Facing::draw`]）；
/// 没有的不挪。抬头低头（[`Look::pitch`]）时往上下挪 `nod` 倍的 [`PITCH`]。挂着晃的部分
/// （`hinge`）再绕挂点转。
#[derive(Clone, Copy)]
struct Facing {
    first: usize,
    tint: Tint,
    at: (f32, f32),
    guide: Option<usize>,
    nod: f32,
    hinge: Option<Hinge>,
}

/// 挂着晃的一层绕哪转（每个朝向的挂点，相对脚底，pt）、偏了多少度（画面里顺时针为正）。
#[derive(Clone, Copy)]
struct Hinge {
    pivots: &'static [(f32, f32); sprites::FACINGS],
    angle: f32,
}

/// 抬头（`pitch` 为 1）时五官往上挪多少（pt），低头（-1）往下。团子身子是圆的，抬头低头轮廓
/// 不怎么变，变的是脸在球面上的位置。
const PITCH: f32 = 2.5;

/// 身上的东西怎么落到画布上：随身子压扁、鼓气、离地、左右晃，再绕脚底歪（[`Look::tilt`]）。
struct OnBody {
    feet: (f32, f32),
    scale: (f32, f32),
    tilt: f32,
    pitch: f32,
}

impl OnBody {
    fn new(look: &Look) -> Self {
        let grow = 1. + look.swell;
        Self {
            feet: (sprites::FEET.0 + look.sway, sprites::FEET.1 - look.lift),
            scale: ((1. + look.squash) * grow, (1. - look.squash) * grow),
            tilt: look.tilt,
            pitch: look.pitch,
        }
    }

    /// 身上的点（相对脚底）落到画布上哪。
    fn point(&self, (x, y): (f32, f32)) -> (f32, f32) {
        let (x, y) = rotated((x * self.scale.0, y * self.scale.1), self.tilt);
        (self.feet.0 + x, self.feet.1 + y)
    }

    /// 不跟着歪的（摆在身前的电脑）：只随压扁、离地、晃。
    fn upright(&self, (x, y): (f32, f32)) -> (f32, f32) {
        (
            self.feet.0 + x * self.scale.0,
            self.feet.1 + y * self.scale.1,
        )
    }

    /// 和身子同框的一张（锚点在脚底）叠上去，另外挪 `at`。
    fn layer(&self, canvas: &mut Canvas, paint: Paint, at: (f32, f32)) {
        self.layer_turned(canvas, paint, at, 0.);
    }

    /// 和 [`OnBody::layer`] 一样，再多转 `degrees`（挂着晃的部分）。
    fn layer_turned(&self, canvas: &mut Canvas, paint: Paint, at: (f32, f32), degrees: f32) {
        canvas.layer_tilted(paint, self.point(at), self.scale, self.tilt + degrees, 1.);
    }
}

impl Facing {
    /// 第 `facing` 个朝向的那张。
    fn at(self, facing: usize) -> Paint {
        Paint::tinted(self.first + facing, self.tint)
    }

    /// 画第 `facing` 个朝向的那张，往第 `other` 个朝向里它的位置挪 `toward`（0 到 1）。转身时
    /// 两个朝向各画一张、互相往对方挪，再按转到哪混起来（[`Canvas::blend`]）：五官两张落在
    /// 同一处，只是渐变，不会一只眼睛看着有两个影子。
    fn draw(self, canvas: &mut Canvas, facing: usize, other: usize, toward: f32, body: &OnBody) {
        let shift = self
            .guide
            .filter(|_| toward > 0.)
            .and_then(|first| Some((centroid(first + facing)?, centroid(first + other)?)))
            .map_or((0., 0.), |(from, to)| {
                ((to.0 - from.0) * toward, (to.1 - from.1) * toward)
            });
        let at = (
            self.at.0 + shift.0,
            self.at.1 + shift.1 - body.pitch * self.nod * PITCH,
        );
        match self.hinge {
            // 零件的锚点在脚底：绕挂点转 `angle`，锚点跟着挪到挂点加上转过的「挂点到锚点」。
            Some(hinge) if hinge.angle.abs() > 0.05 => {
                let pivot = hinge.pivots[facing];
                let pivot = (pivot.0 + at.0, pivot.1 + at.1);
                let (x, y) = rotated((at.0 - pivot.0, at.1 - pivot.1), hinge.angle);
                body.layer_turned(
                    canvas,
                    self.at(facing),
                    (pivot.0 + x, pivot.1 + y),
                    hinge.angle,
                );
            }
            _ => body.layer(canvas, self.at(facing), at),
        }
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
/// 动起来每帧一张，贴图越攒越多。头顶挂着的部分晃到哪要接着上一帧算（[`swing::Swing`]）。
#[derive(Default)]
pub(crate) struct Portrait {
    shown: Option<(Buddy, Look, f32, Arc<RenderImage>)>,
    swing: swing::Swing,
}

impl Portrait {
    /// 画 `buddy` 在 `now` 摆着 `look` 的样子：盖住 [`CANVAS`] 见方的画布，四周伸出去
    /// [`MARGIN`]（举起来的道具、头顶的特效），外面不裁。`moving` 为假（定格）时挂着的部分
    /// 不晃。另返回挂着的部分还在不在晃，在晃就还要下一帧。
    fn show(
        &mut self,
        buddy: Buddy,
        look: Look,
        now: Instant,
        moving: bool,
        window: &mut Window,
    ) -> (AnyElement, bool) {
        let (swing, swinging) = if moving {
            self.swing.step(buddy, &look, now)
        } else {
            self.swing = swing::Swing::default();
            (0., false)
        };
        let image = match &self.shown {
            Some((shown, seen, angle, image))
                if *shown == buddy && *seen == look && *angle == swing =>
            {
                image.clone()
            }
            _ => {
                let image = figure(buddy, &look, swing);
                let shown = (buddy, look, swing, image.clone());
                if let Some((_, _, _, stale)) = self.shown.replace(shown) {
                    window.drop_image(stale).ok();
                }
                image
            }
        };
        let image = img(image)
            .object_fit(ObjectFit::Fill)
            .absolute()
            .left(px(-MARGIN))
            .top(px(-MARGIN))
            .size(px(CANVAS + 2. * MARGIN))
            .into_any_element();
        (image, swinging)
    }

    /// 头像关掉时调：画着的那张从各个窗口的贴图里删掉。
    pub(crate) fn release(&mut self, cx: &mut App) {
        if let Some((_, _, _, image)) = self.shown.take() {
            cx.drop_image(image, None);
        }
    }
}

/// 叠一帧：`look` 里的零件按锚点叠在 [`CANVAS`] 见方的画布上，先叠的在下面。转身时身子、
/// 脸、戴的取夹着的两个朝向混起来（[`Facing::draw`]）；转到身子后面的胳膊垫在身子底下。头顶
/// 挂着的部分绕挂点偏 `swing` 度。
fn figure(buddy: Buddy, look: &Look, swing: f32) -> Arc<RenderImage> {
    const SAME: (f32, f32) = (1., 1.);
    let mut canvas = Canvas::new();
    let (fx, fy) = sprites::FEET;
    let body = OnBody::new(look);
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
        let angle = foreshortened(look.arms[side], look.turn) + look.tilt;
        (
            Arm {
                side,
                shoulder: body.point(at),
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
    let layers = buddy.turning(look, swing);
    let (from, to, toward) = between(look.turn);
    let draw = |canvas: &mut Canvas, facing, other, toward| {
        for layer in &layers {
            layer.draw(canvas, facing, other, toward, &body);
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
        canvas.layer(laptop, body.upright(sprites::LAPTOP_AT), SAME, 1.);
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
        canvas.layer(paint, body.point(effect.at), scale, effect.alpha);
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
                    let (index, _) = turned(look.prop + look.tilt);
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
                // 不晃的主料、点缀同框，晃的那两张同框；整件至少有一张。
                let hung = wearable.dangle.as_ref();
                let groups = [
                    (wearable.base, wearable.trim),
                    hung.map_or((None, None), |dangle| (dangle.base, dangle.trim)),
                ];
                let mut any = false;
                for (base, trim) in groups {
                    let parts: Vec<_> = base.into_iter().chain(trim).collect();
                    any |= !parts.is_empty();
                    for part in parts.iter().skip(1) {
                        let (a, b) = (parts[0], *part);
                        assert_eq!((size(a), SPRITES[a].anchor), (size(b), SPRITES[b].anchor));
                    }
                }
                assert!(any);
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
    fn the_body_swells_and_tilts_about_the_feet() {
        let mut look = Look::rest(Role::Fixer, 0);
        look.turn = 0.;
        look.tilt = 90.;
        look.swell = 0.1;
        let body = OnBody::new(&look);
        let (fx, fy) = sprites::FEET;
        // 脚底不动；脚底正上方 10pt 的点鼓大一成、顺时针歪 90° 到右边 11pt。
        let feet = body.point((0., 0.));
        assert!((feet.0 - fx).abs() < 1e-4 && (feet.1 - fy).abs() < 1e-4);
        let (x, y) = body.point((0., -10.));
        assert!(
            (x - (fx + 11.)).abs() < 1e-3 && (y - fy).abs() < 1e-3,
            "{x} {y}"
        );
        // 身前的电脑跟着鼓，不跟着歪。
        let (x, y) = body.upright((0., -10.));
        assert!((x - fx).abs() < 1e-4 && (y - (fy - 11.)).abs() < 1e-3);
    }

    #[test]
    fn looking_up_moves_the_face_not_the_body() {
        let buddy = Buddy::of("pitch");
        let mut look = Look::rest(buddy.role(), 0);
        look.pitch = 1.;
        let layers = buddy.turning(&look, 0.);
        let body = layers
            .iter()
            .find(|layer| layer.first == sprites::BODY)
            .unwrap();
        let eye = layers
            .iter()
            .find(|layer| layer.first == look.eyes.sprites()[0])
            .unwrap();
        assert_eq!(body.nod, 0.);
        assert_eq!(eye.nod, 1.);
    }

    #[test]
    fn a_wink_shuts_one_eye() {
        let buddy = Buddy::of("wink");
        let mut look = Look::rest(buddy.role(), 0);
        look.eyes = moves::Eyes::Open;
        look.wink = Some(1);
        let firsts: Vec<_> = buddy
            .turning(&look, 0.)
            .iter()
            .map(|layer| layer.first)
            .collect();
        assert!(firsts.contains(&sprites::EYE_OPEN_L));
        assert!(firsts.contains(&sprites::EYE_SHUT_R));
        assert!(!firsts.contains(&sprites::EYE_OPEN_R));
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
            sprites::EYE_SLEEPY_L,
            sprites::EYE_WIDE_R,
            sprites::MOUTH_SMILE,
            sprites::MOUTH_OPEN,
            sprites::MOUTH_O,
            sprites::MOUTH_YAWN,
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
