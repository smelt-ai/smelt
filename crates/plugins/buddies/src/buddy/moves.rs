//! 动作：某一刻小人摆成什么样（[`Look`]），按时间现算。
//!
//! 干活和喊你按拍子走：每拍按（种子, 第几拍）随机挑一个动作（[`choose`]），动作从站姿出发、
//! 回到站姿，拍与拍之间接得上。眨眼也是每拍随机的。欢呼、打招呼是演一遍就停的，停了回站姿。
//! 站着不动：站姿按种子略有不同（胳膊、道具的角度、往哪看、表情），每次停下来换一个。
//!
//! 不同的小人动起来不一样：每个小人有一份固定的性格（[`Temper`]），定节奏快慢、团子有多弹、
//! 多爱东张西望、欢呼时的招牌动作。动作照着手绘动画的几条老规矩现算：起跳前先蹲（预备）、
//! 腾空时拉长、落地压扁再按阻尼弹簧回弹几下（[`jiggle`]）；转身时眼睛先过去、身子跟上。
//!
//! 角度都是画面里的，正上方 0°、顺时针。胳膊 180° 是垂直往下，右胳膊小于 180° 往外、左胳膊
//! 大于 180° 往外。道具 0° 是朝上。转身（[`Look::turn`]）是绕竖轴的，0 是正面，往画面右边
//! 转为正。

use std::f32::consts::{PI, TAU};
use std::ops::Range;

use super::sprites::{self, ARM_LEN};
use super::{foreshortened, rotated, shoulder, toward, turned};

/// 每个小人都可能用到的零件（眼睛、嘴、身前的物件、特效），预先解。
pub(super) const SHARED: [Range<usize>; 2] = [
    sprites::EYE_OPEN_L..sprites::MOUTH_OPEN + sprites::FACINGS,
    sprites::LAPTOP..sprites::KART_FRONT,
];

/// 职业：手里拿什么、身前摆什么、怎么干活。戴什么、什么颜色另按种子落，和职业无关。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Role {
    Coder,
    Fixer,
    Detective,
    Writer,
    Chemist,
    Gardener,
    Conductor,
    Sweeper,
    Racer,
}

/// 手里的道具：零件组的第一张，朝上时尖端（冒火星、洒水的地方）相对握点在哪。
#[derive(Clone, Copy)]
pub(super) struct Prop {
    pub(super) first: usize,
    tip: (f32, f32),
}

impl Role {
    pub(super) const ALL: [Self; 9] = [
        Self::Coder,
        Self::Fixer,
        Self::Detective,
        Self::Writer,
        Self::Chemist,
        Self::Gardener,
        Self::Conductor,
        Self::Sweeper,
        Self::Racer,
    ];

    pub(super) fn prop(self) -> Option<Prop> {
        let (first, tip) = match self {
            Self::Coder | Self::Racer => return None,
            Self::Fixer => (sprites::PROP_WRENCH, sprites::TIP_WRENCH),
            Self::Detective => (sprites::PROP_MAGNIFIER, sprites::TIP_MAGNIFIER),
            Self::Writer => (sprites::PROP_PENCIL, sprites::TIP_PENCIL),
            Self::Chemist => (sprites::PROP_FLASK, sprites::TIP_FLASK),
            Self::Gardener => (sprites::PROP_CAN, sprites::TIP_CAN),
            Self::Conductor => (sprites::PROP_BATON, sprites::TIP_BATON),
            Self::Sweeper => (sprites::PROP_BROOM, sprites::TIP_BROOM),
        };
        Some(Prop { first, tip })
    }

    /// 站着时：左右胳膊、道具的角度。
    fn stance(self) -> ([f32; 2], f32) {
        match self {
            Self::Coder => ([160., 200.], 0.),
            Self::Fixer => ([210., 150.], 20.),
            Self::Detective => ([210., 140.], 30.),
            Self::Writer => ([165., 200.], 285.),
            Self::Chemist => ([210., 150.], 0.),
            Self::Gardener => ([210., 160.], 0.),
            Self::Conductor => ([210., 140.], 30.),
            Self::Sweeper => ([210., 120.], 220.),
            // 两手搭在方向盘两边。
            Self::Racer => ([148., 236.], 0.),
        }
    }

    /// 干活时每拍挑的动作的权重，0 号是本职动作（见 [`work`]）。
    fn moves(self) -> &'static [u32] {
        match self {
            Self::Coder | Self::Writer => &[5, 2, 1],
            Self::Fixer => &[4, 3, 1],
            Self::Detective => &[4, 2],
            Self::Chemist => &[4, 2, 1],
            Self::Gardener => &[4, 2],
            Self::Conductor => &[4, 1, 1],
            Self::Sweeper => &[5, 2],
            Self::Racer => &[5, 3, 2, 2],
        }
    }

    /// 干活时转过去东张西望的权重，加在性格的上面：侦探本来就爱四处看，赛车手坐在车里不转。
    fn glances(self, curiosity: Curiosity) -> u32 {
        match self {
            Self::Racer => 0,
            Self::Detective => curiosity.weight() + 2,
            _ => curiosity.weight(),
        }
    }

    /// 干活时一拍多长（秒），没算性格的快慢。
    fn beat(self) -> f32 {
        match self {
            Self::Coder | Self::Chemist | Self::Sweeper | Self::Racer => 1.4,
            Self::Fixer => 1.5,
            Self::Writer => 1.6,
            Self::Detective | Self::Gardener => 1.8,
            Self::Conductor => 1.2,
        }
    }

    fn front(self) -> Option<Front> {
        match self {
            Self::Coder => Some(Front::Laptop(2)),
            Self::Writer => Some(Front::Notepad(2)),
            _ => None,
        }
    }

    /// 手里会转的零件组（道具，赛车手是方向盘）的第一张。
    pub(super) fn held(self) -> Option<usize> {
        match self {
            Self::Racer => Some(sprites::WHEEL),
            _ => self.prop().map(|prop| prop.first),
        }
    }

    /// 只有这个职业用的、不转的零件（赛车手的车身），别的小人不用解。
    pub(super) fn own_sprites(self) -> Range<usize> {
        match self {
            Self::Racer => sprites::KART_FRONT..sprites::WHEEL,
            _ => 0..0,
        }
    }
}

/// 性格：同一个小人一直一样，不同的小人同一个动作做出来不一样。按小人的种子落（`Buddy::of`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Temper {
    pub(super) pace: Pace,
    pub(super) spring: Spring,
    pub(super) curiosity: Curiosity,
    pub(super) flourish: Flourish,
}

impl Temper {
    pub(super) fn roll(dice: &mut Dice) -> Self {
        Self {
            pace: [Pace::Calm, Pace::Steady, Pace::Brisk][dice.below(3)],
            spring: [Spring::Firm, Spring::Soft, Spring::Wobbly][dice.below(3)],
            curiosity: [Curiosity::Focused, Curiosity::Curious, Curiosity::Restless][dice.below(3)],
            flourish: [Flourish::Spin, Flourish::Bounce, Flourish::Wiggle][dice.below(3)],
        }
    }
}

/// 节奏：干活的拍子、喘气的快慢。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Pace {
    Calm,
    Steady,
    Brisk,
}

impl Pace {
    /// 拍子乘多少。
    fn stretch(self) -> f32 {
        match self {
            Self::Calm => 1.2,
            Self::Steady => 1.,
            Self::Brisk => 0.85,
        }
    }
}

/// 团子有多弹：落地压多扁、回弹得多快、弹几下才停，干活时起伏多大。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Spring {
    /// 紧实，压一下就停。
    Firm,
    Soft,
    /// 软糯，颤好几下。
    Wobbly,
}

impl Spring {
    /// 回弹的频率（Hz）、阻尼比、落地那一下压多扁。
    fn feel(self) -> (f32, f32, f32) {
        match self {
            Self::Firm => (4.5, 0.5, 0.08),
            Self::Soft => (3.2, 0.3, 0.11),
            Self::Wobbly => (2.6, 0.16, 0.14),
        }
    }

    /// 干活时喘气起伏多大。
    fn breath(self) -> f32 {
        match self {
            Self::Firm => 0.015,
            Self::Soft => 0.025,
            Self::Wobbly => 0.035,
        }
    }
}

/// 多爱东张西望：干活时转过去看看的权重，坐不住的转得更远（侧过身去）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Curiosity {
    /// 埋头干，从不四处看。
    Focused,
    Curious,
    Restless,
}

impl Curiosity {
    fn weight(self) -> u32 {
        match self {
            Self::Focused => 0,
            Self::Curious => 1,
            Self::Restless => 2,
        }
    }

    /// 转过去多少度。
    fn reach(self) -> f32 {
        match self {
            Self::Restless => 90.,
            Self::Focused | Self::Curious => 45.,
        }
    }
}

/// 干完欢呼时的招牌动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Flourish {
    /// 跳起来在空中转一圈。
    Spin,
    /// 连蹦两下，第二下更高。
    Bounce,
    /// 左右扭着转身。
    Wiggle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Eyes {
    Open,
    /// 笑眯成两道弯。
    Happy,
    Shut,
}

impl Eyes {
    /// 左、右眼的零件组（按朝向编号）。
    pub(super) fn sprites(self) -> [usize; 2] {
        match self {
            Self::Open => [sprites::EYE_OPEN_L, sprites::EYE_OPEN_R],
            Self::Happy => [sprites::EYE_HAPPY_L, sprites::EYE_HAPPY_R],
            Self::Shut => [sprites::EYE_SHUT_L, sprites::EYE_SHUT_R],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mouth {
    Smile,
    Flat,
    Open,
}

impl Mouth {
    /// 零件组（按朝向编号）。
    pub(super) fn sprite(self) -> usize {
        match self {
            Self::Smile => sprites::MOUTH_SMILE,
            Self::Flat => sprites::MOUTH_FLAT,
            Self::Open => sprites::MOUTH_OPEN,
        }
    }
}

/// 身前的物件和上面写了几行（0 到 4）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Front {
    /// 码农摆在身前的电脑。
    Laptop(usize),
    /// 写手左手拿的本子。
    Notepad(usize),
}

/// 赛车手坐的卡丁车。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Kart {
    /// 车身左右歪多少度（顺时针为正，右边往下沉）。
    pub(super) lean: f32,
    /// 方向盘打了多少度（顺时针为正，往右打）。
    pub(super) steer: f32,
}

/// 一个特效：零件、锚点放在哪（相对脚底，跟着身子走）、缩放、不透明度。
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Fx {
    pub(super) sprite: usize,
    pub(super) at: (f32, f32),
    pub(super) scale: f32,
    pub(super) alpha: f32,
}

/// 某一刻的样子。位置是 pt，相对脚底，y 朝下。
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Look {
    /// 离地多高。
    pub(super) lift: f32,
    /// 压扁多少：正的变矮变宽，负的拉高变窄。
    pub(super) squash: f32,
    /// 身子左右挪多少。
    pub(super) sway: f32,
    /// 转身转到多少度。
    pub(super) turn: f32,
    /// 左、右胳膊的角度。
    pub(super) arms: [f32; 2],
    /// 右手道具的角度。
    pub(super) prop: f32,
    pub(super) eyes: Eyes,
    /// 眼睛往哪挪（往哪看）。
    pub(super) gaze: (f32, f32),
    pub(super) mouth: Mouth,
    pub(super) front: Option<Front>,
    /// 园丁脚边的苗长到第几截（0 到 4）。
    pub(super) sprout: Option<usize>,
    pub(super) kart: Option<Kart>,
    pub(super) fx: Vec<Fx>,
}

/// 小的确定性随机数（SplitMix64）：同一个种子出同一串，动作才能按时间现算。
pub(super) struct Dice(u64);

impl Dice {
    pub(super) fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// [0, `n`) 里的一个。
    pub(super) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// [0, 1)
    pub(super) fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn range(&mut self, low: f32, high: f32) -> f32 {
        low + (high - low) * self.unit()
    }

    /// 按权重挑第几个。
    fn pick(&mut self, weights: &[u32]) -> usize {
        let total: u32 = weights.iter().sum();
        let mut roll = (self.next() % u64::from(total)) as u32;
        for (index, weight) in weights.iter().enumerate() {
            if roll < *weight {
                return index;
            }
            roll -= weight;
        }
        weights.len() - 1
    }
}

/// 0 → 1 → 0 的一个拱，两头平缓。
fn arch(p: f32) -> f32 {
    (PI * p.clamp(0., 1.)).sin()
}

/// 在一拍里来回 `times` 次。
fn swing(p: f32, times: f32) -> f32 {
    (TAU * times * p).sin()
}

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0., 1.);
    x * x * (3. - 2. * x)
}

/// 0 起、很快到 1、停住、最后很快回 0：给要接上站姿的动作套在外面。
fn ramp(p: f32) -> f32 {
    smooth(within(p, 0., 0.15)) * smooth(within(1. - p, 0., 0.15))
}

fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

fn add(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    (a.0 + b.0, a.1 + b.1)
}

/// 一段 0 到 1 里的 [`from`, `to`) 那一截走到哪了，没到是 0、过了是 1。
fn within(p: f32, from: f32, to: f32) -> f32 {
    ((p - from) / (to - from)).clamp(0., 1.)
}

/// 第 `beat` 拍的随机数种子。
fn salt(seed: u64, beat: u64) -> u64 {
    seed ^ beat.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// 第 `beat` 拍做第几个动作：按权重挑，0 号是本职动作，别的是偶尔来一下的。偶尔的不连着两拍
/// 出现：上一拍掷到的不是 0 号，这一拍就做本职，看着是「干活、来一下、接着干」，不会连着
/// 花样。上一拍掷到什么按（种子, 拍）现算，不用记。
fn choose(seed: u64, beat: u64, weights: &[u32]) -> usize {
    let roll = |beat| Dice::new(salt(seed, beat) ^ 0xc405e).pick(weights);
    match roll(beat) {
        0 => 0,
        _ if beat.checked_sub(1).is_some_and(|before| roll(before) != 0) => 0,
        pick => pick,
    }
}

/// 落地后回弹：欠阻尼弹簧的解 `give·e^(−ζωt)·cos(ω_d·t)`。`since` 是落地后多少秒，返回
/// 压扁多少，按性格的 [`Spring`] 弹。
fn jiggle(spring: Spring, since: f32) -> f32 {
    let (hz, damping, give) = spring.feel();
    let omega = TAU * hz;
    let damped = omega * (1. - damping * damping).sqrt();
    give * (-damping * omega * since).exp() * (damped * since).cos()
}

/// 头顶上方（相对脚底）。
const OVERHEAD: (f32, f32) = sprites::OVERHEAD;

impl Look {
    /// 站姿。`seed` 定胳膊、道具略偏多少，往哪看，什么表情。
    pub(super) fn rest(role: Role, seed: u64) -> Self {
        let mut dice = Dice::new(seed);
        let ([left, right], prop) = role.stance();
        let mood = dice.unit();
        // 赛车手的手搭在方向盘上，不能随便放。
        let loose = if role == Role::Racer { 3. } else { 10. };
        Self {
            lift: 0.,
            squash: 0.,
            sway: 0.,
            turn: 0.,
            arms: [
                left + dice.range(-loose, loose),
                right + dice.range(-loose, loose),
            ],
            prop: prop + dice.range(-10., 10.),
            eyes: if mood < 0.15 { Eyes::Happy } else { Eyes::Open },
            gaze: (dice.range(-0.7, 0.7), dice.range(-0.4, 0.3)),
            mouth: if mood > 0.8 {
                Mouth::Flat
            } else {
                Mouth::Smile
            },
            front: role.front(),
            sprout: None,
            kart: (role == Role::Racer).then_some(Kart {
                lean: 0.,
                steer: 0.,
            }),
            fx: Vec::new(),
        }
    }

    /// 右手在哪（相对脚底，没算离地和晃）。
    fn right_hand(&self) -> (f32, f32) {
        let (at, _) = shoulder(self.turn, 1);
        hand(at, foreshortened(self.arms[1], self.turn))
    }

    /// 道具尖端在哪。
    fn tip(&self, prop: Prop) -> (f32, f32) {
        let (_, angle) = turned(self.prop);
        add(self.right_hand(), rotated(prop.tip, angle))
    }

    fn fx(&mut self, sprite: usize, at: (f32, f32), scale: f32, alpha: f32) {
        self.fx.push(Fx {
            sprite,
            at,
            scale,
            alpha,
        });
    }

    /// 从 `at` 冒出来、往 `drift` 飘、渐渐淡掉的一串，`count` 个错开。`phase` 走一整圈每个
    /// 冒一遍，传拍子里的进度（乘整数），拍与拍之间接得上；`fall` 是往下坠的劲。整串再按
    /// 动作的进度 `p` 在开头结尾淡进淡出，换动作时不会一下没了。
    #[allow(clippy::too_many_arguments)]
    fn stream(
        &mut self,
        sprite: usize,
        at: (f32, f32),
        drift: (f32, f32),
        fall: f32,
        count: usize,
        phase: f32,
        p: f32,
        salt: u64,
    ) {
        for k in 0..count {
            let age = (phase + k as f32 / count as f32).fract();
            // 每颗自己掷，不和别的随机数共用一串：调用次数随帧变时也不跳。
            let jitter = Dice::new(salt ^ k as u64).range(-1.5, 1.5);
            let x = at.0 + (drift.0 + jitter) * age;
            let y = at.1 + drift.1 * age + fall * age * age;
            let alpha = (1. - age) * within(age, 0., 0.15) * ramp(p);
            if alpha > 0.01 {
                self.fx(sprite, (x, y), 0.7 + 0.3 * (1. - age), alpha);
            }
        }
    }
}

fn hand(shoulder: (f32, f32), angle: f32) -> (f32, f32) {
    let (_, angle) = turned(angle);
    let (dx, dy) = toward(angle);
    (shoulder.0 + dx * ARM_LEN, shoulder.1 + dy * ARM_LEN)
}

/// 干活一拍多长（秒）。
fn beat_len(role: Role, temper: Temper) -> f32 {
    role.beat() * temper.pace.stretch()
}

/// 干活。`t` 是从开始干起的秒数。每拍挑一个动作（[`choose`]）：这一行的几个动作，或者转过去
/// 东张西望一下（多常见看性格和职业）。
pub(super) fn work(role: Role, temper: Temper, seed: u64, t: f32) -> Look {
    let mut look = Look::rest(role, seed);
    look.mouth = Mouth::Smile;
    look.eyes = Eyes::Open;
    let beat_len = beat_len(role, temper);
    let beat = (t / beat_len) as u64;
    let p = (t / beat_len).fract();
    let salt = salt(seed, beat);
    // 干活时一直有点起伏，像在喘气；多大、多快看性格。
    look.squash = temper.spring.breath() * swing(t / (0.9 * temper.pace.stretch()), 1.);
    progress(role, &mut look, t);
    let own = role.moves();
    let mut weights = own.to_vec();
    weights.push(role.glances(temper.curiosity));
    match choose(seed, beat, &weights) {
        glance if glance == own.len() => {
            let side = if Dice::new(salt).unit() < 0.5 {
                -1.
            } else {
                1.
            };
            look_around(&mut look, p, side, temper.curiosity.reach());
        }
        choice => perform(role, temper, &mut look, choice, t, p, salt),
    }
    blink(&mut look, p, beat_len, salt);
    look
}

/// 干出来的进度，按干了多久走，不按拍子：屏幕上的行、本子上的字、脚边的苗。
fn progress(role: Role, look: &mut Look, t: f32) {
    match role {
        Role::Coder => look.front = Some(Front::Laptop(((t * 2.5) as usize % 7).min(4))),
        Role::Writer => look.front = Some(Front::Notepad(((t * 1.5) as usize % 7).min(4))),
        // 长满了就开花。
        Role::Gardener => look.sprout = Some(((t / 4.) as usize).min(4)),
        _ => {}
    }
}

/// 这一行的第 `choice` 个动作（[`Role::moves`]），`p` 是这一拍走到哪，`salt` 是这一拍的种子。
fn perform(role: Role, temper: Temper, look: &mut Look, choice: usize, t: f32, p: f32, salt: u64) {
    let ([left, right], prop) = role.stance();
    // 动作开头、结尾的一小段淡进淡出，接上站姿。
    let ease = ramp(p);
    match role {
        Role::Coder => {
            match choice {
                0 => {
                    // 打字：两只手交替点。
                    look.arms = [left - 10. * swing(p, 7.), right + 10. * swing(p + 0.07, 7.)];
                    look.gaze = (0., 0.6);
                }
                1 => {
                    // 想：手抬到下巴，眼睛往上瞟，头顶一个个冒点。
                    let a = arch(p);
                    look.arms[1] = lerp(right, 320., a);
                    look.gaze = (0.6 * a, -0.9 * a);
                    look.mouth = Mouth::Flat;
                    thinking_dots(look, p);
                }
                _ => {
                    // 有主意了：头顶亮个灯泡，手指着它。
                    let a = arch(p);
                    look.arms[1] = lerp(right, 20., a);
                    bulb(look, p);
                }
            }
        }
        Role::Fixer => match choice {
            0 => {
                // 拧：扳手来回转，转到头蹦火星。
                look.prop = prop + 45. * swing(p, 2.);
                look.arms[1] = right + 12. * swing(p, 2.);
                look.gaze = (0.7, 0.);
                look.mouth = Mouth::Flat;
                let tip = look.tip(Role::Fixer.prop().unwrap());
                look.stream(sprites::FX_SPARK, tip, (3., -4.), 10., 3, p, p, salt);
            }
            1 => {
                // 敲：一下一下往下砸，砸到底蹦火星。
                let hit = (3. * PI * p).sin().abs();
                look.arms[1] = right - 40. * hit;
                look.prop = prop - 20. * hit;
                look.gaze = (0.6, 0.3);
                if hit < 0.35 {
                    let tip = look.tip(Role::Fixer.prop().unwrap());
                    look.stream(sprites::FX_SPARK, tip, (2., -5.), 12., 2, p * 3., p, salt);
                }
            }
            _ => {
                // 举起来端详，闪一下。
                let a = arch(p);
                look.arms[1] = lerp(right, 70., a);
                look.prop = lerp(prop, -10., a);
                look.gaze = (0.8 * a, -0.6 * a);
                if a > 0.8 {
                    let tip = look.tip(Role::Fixer.prop().unwrap());
                    look.fx(sprites::FX_STAR, tip, 0.5 + 0.5 * within(a, 0.8, 1.), 1.);
                }
            }
        },
        Role::Detective => match choice {
            0 => {
                // 扫一遍：放大镜左右移，眼睛跟着。
                let s = swing(p, 1.);
                look.arms[1] = right + 30. * s;
                look.prop = prop + 20. * s;
                look.gaze = (0.9 * s, 0.3);
            }
            _ => {
                // 凑到眼前看。
                let a = smooth(arch(p) * 1.4);
                look.arms[1] = lerp(right, 40., a);
                look.prop = lerp(prop, -30., a);
                look.mouth = if a > 0.7 { Mouth::Open } else { Mouth::Flat };
            }
        },
        Role::Writer => {
            match choice {
                0 => {
                    // 写：笔尖抖着走。
                    look.prop = prop + 8. * swing(p, 6.);
                    look.arms[1] = right + 4. * swing(p, 3.);
                    look.gaze = (-0.7, 0.4);
                }
                1 => {
                    // 笔抵着下巴想。
                    let a = arch(p);
                    look.arms[1] = lerp(right, 320., a);
                    look.prop = lerp(prop, 340., a);
                    look.gaze = (0.6 * a, -0.8 * a);
                    look.mouth = Mouth::Flat;
                    thinking_dots(look, p);
                }
                _ => {
                    // 写得满意，蹦一下。
                    hop(look, p, beat_len(role, temper), 3., temper.spring);
                    look.eyes = Eyes::Happy;
                    look.mouth = Mouth::Open;
                }
            }
        }
        Role::Chemist => match choice {
            0 => {
                // 摇瓶子，冒泡。
                let s = swing(p, 5.);
                look.arms[1] = right + 14. * s;
                look.prop = 15. * s;
                look.gaze = (0.7, -0.2);
                let tip = look.tip(Role::Chemist.prop().unwrap());
                look.stream(sprites::FX_BUBBLE, tip, (1., -9.), 0., 3, p, p, salt);
            }
            1 => {
                // 举起来对着光看。
                let a = arch(p);
                look.arms[1] = lerp(right, 30., a);
                look.prop = lerp(prop, -10., a);
                look.gaze = (0.8 * a, -a);
                look.mouth = if a > 0.6 { Mouth::Open } else { Mouth::Smile };
                if a > 0.85 {
                    let tip = look.tip(Role::Chemist.prop().unwrap());
                    look.fx(sprites::FX_STAR, add(tip, (2., -2.)), 0.6, 1.);
                }
            }
            _ => {
                // 噗地冒一股烟，眯眼。
                let tip = look.tip(Role::Chemist.prop().unwrap());
                let puff = within(p, 0.1, 0.9);
                look.fx(
                    sprites::FX_PUFF,
                    add(tip, (0., -4. * puff)),
                    0.6 + 0.8 * puff,
                    1. - puff,
                );
                look.eyes = if puff > 0. && puff < 0.8 {
                    Eyes::Shut
                } else {
                    Eyes::Open
                };
                look.mouth = Mouth::Flat;
            }
        },
        Role::Gardener => {
            match choice {
                0 => {
                    // 浇水：壶嘴往下倾，水滴落下去。
                    let a = smooth(arch(p) * 1.3);
                    look.prop = prop + 45. * a;
                    look.arms[1] = lerp(right, 135., a);
                    look.gaze = (1., 0.6);
                    if a > 0.6 {
                        let tip = look.tip(Role::Gardener.prop().unwrap());
                        look.stream(sprites::FX_DROP, tip, (0.5, 2.), 6., 3, p * 2., p, salt);
                    }
                }
                _ => {
                    // 看着苗乐，冒个爱心。
                    look.gaze = (1., 0.6);
                    look.eyes = Eyes::Happy;
                    let rise = within(p, 0.25, 0.95);
                    if rise > 0. {
                        let at = add(sprites::SPROUT_AT, (0., -6. - 6. * rise));
                        look.fx(
                            sprites::FX_HEART,
                            at,
                            0.6 + 0.4 * rise.min(0.3) / 0.3,
                            1. - rise * rise,
                        );
                    }
                }
            }
        }
        Role::Conductor => match choice {
            0 => {
                // 打拍子：指挥棒画弧，音符飘出来。
                look.arms[1] = lerp(right, 70. + 40. * swing(p, 2.), ease);
                look.prop = lerp(prop, 20. + 25. * swing(p + 0.1, 2.), ease);
                look.arms[0] = left + 10. * swing(p, 2.);
                look.sway = 0.6 * swing(p, 1.);
                let tip = look.tip(Role::Conductor.prop().unwrap());
                look.stream(sprites::FX_NOTE, tip, (4., -8.), 0., 2, p, p, salt);
            }
            1 => {
                // 渐强：两手举高，踮一下。
                let a = arch(p);
                look.arms = [lerp(left, 340., a), lerp(right, 20., a)];
                look.lift = 3. * a;
                look.eyes = Eyes::Happy;
                look.mouth = Mouth::Open;
                look.stream(
                    sprites::FX_NOTE,
                    add(OVERHEAD, (0., 2.)),
                    (6., -6.),
                    0.,
                    3,
                    p,
                    p,
                    salt,
                );
            }
            _ => {
                // 收：手压下去，闭眼。
                let a = arch(p);
                look.arms[1] = lerp(right, 110., a);
                look.prop = lerp(prop, 90., a);
                look.eyes = if a > 0.4 { Eyes::Happy } else { Eyes::Open };
                look.mouth = Mouth::Flat;
            }
        },
        Role::Sweeper => match choice {
            0 => {
                // 扫：扫帚来回，身子跟着晃，扬灰。
                let s = swing(p, 2.);
                look.prop = prop + 12. * s;
                look.arms[1] = right + 10. * s;
                look.sway = 0.8 * s;
                look.gaze = (-0.6, 0.6);
                let tip = look.tip(Role::Sweeper.prop().unwrap());
                look.stream(sprites::FX_PUFF, tip, (-3., -3.), 0., 2, p, p, salt);
            }
            _ => {
                // 拄着扫帚歇口气。
                let a = arch(p);
                look.prop = lerp(prop, 195., a);
                look.arms[1] = lerp(right, 150., a);
                look.sway = -0.5 * a;
                look.eyes = if a > 0.3 { Eyes::Happy } else { Eyes::Open };
            }
        },
        Role::Racer => drive(look, choice, t, p, salt),
    }
}

/// 开卡丁车：每拍过一个弯、一段 S 弯、甩一个漂移，或者在直道上伏低冲。两边的速度线一直在
/// 往后飘，冲刺时更浓。
fn drive(look: &mut Look, choice: usize, t: f32, p: f32, salt: u64) {
    // 发动机一直在抖。
    look.lift = 0.3 * (t * 40.).sin().abs();
    let side = if Dice::new(salt).unit() < 0.5 {
        -1.
    } else {
        1.
    };
    // 往右拐多少（-1 到 1）。车身往弯外歪，人往弯里靠、看着弯里，手跟着方向盘转。
    let mut turn = 0.;
    let mut tilt = 15.;
    let mut steer = 70.;
    let mut rush = 0.;
    match choice {
        0 => {
            // 过一个弯。
            turn = side * arch(p);
        }
        1 => {
            // S 弯：先拐一边再拐另一边。
            turn = side * swing(p, 1.);
        }
        2 => {
            // 漂移：车甩得更狠，方向盘反打，弯外那只后轮冒烟。
            turn = side * smooth(arch(p) * 1.4);
            tilt = 25.;
            steer = -35.;
            look.mouth = Mouth::Open;
            let tyre = if side > 0. {
                sprites::TYRE_L
            } else {
                sprites::TYRE_R
            };
            look.stream(
                sprites::FX_PUFF,
                tyre,
                (-side * 5., -3.),
                0.,
                3,
                p * 2.,
                p,
                salt,
            );
        }
        _ => {
            // 直道：伏低身子往前冲。
            rush = ramp(p);
            look.squash = 0.07 * rush;
            look.mouth = if rush > 0.5 {
                Mouth::Flat
            } else {
                Mouth::Smile
            };
        }
    }
    let wheel = steer * turn;
    look.kart = Some(Kart {
        lean: -tilt * turn,
        steer: wheel,
    });
    look.sway = 1.8 * turn;
    // 冲刺时不拐弯（turn 是 0），盯着前面的路；拐弯时看着弯里。
    look.gaze = (0.9 * turn, 0.4 * rush);
    // 方向盘顺时针转时左手往上、右手往下：两条胳膊都往小角度转。
    look.arms[0] -= 0.13 * wheel;
    look.arms[1] -= 0.13 * wheel;
    speed_lines(look, t, turn, rush);
}

/// 车两边往后飘的速度线：按时间走，不按拍子，换动作时不断。拐弯时往弯外偏，`rush` 越大越浓。
fn speed_lines(look: &mut Look, t: f32, turn: f32, rush: f32) {
    const PER_SIDE: usize = 3;
    for side in [-1., 1.] {
        for k in 0..PER_SIDE {
            let age = (t * 1.6 + k as f32 / PER_SIDE as f32 + 0.17 * side).fract();
            let x = side * (15. + 3. * age) - turn * 4. * age;
            let y = -2. - 14. * age;
            let alpha = arch(age) * (0.5 + 0.5 * rush);
            look.fx(sprites::FX_SPEED, (x, y), 0.8 + 0.4 * (1. - age), alpha);
        }
    }
}

/// 想事情：头顶右边一个个冒点，最后一个亮。
fn thinking_dots(look: &mut Look, p: f32) {
    for k in 0..3 {
        let shown = within(p, 0.15 + 0.15 * k as f32, 0.25 + 0.15 * k as f32);
        if shown > 0. && p < 0.9 {
            let sprite = if k == 2 {
                sprites::FX_DOT_LIT
            } else {
                sprites::FX_DOT
            };
            let at = add(OVERHEAD, (4. + 2.6 * k as f32, 1.5 - 1.2 * k as f32));
            look.fx(sprite, at, 0.6 + 0.4 * shown, shown);
        }
    }
}

/// 头顶亮个灯泡：弹出来，再点亮。
fn bulb(look: &mut Look, p: f32) {
    let pop = within(p, 0.15, 0.35);
    if pop > 0. && p < 0.92 {
        let lit = p > 0.45;
        let sprite = if lit {
            sprites::FX_BULB_LIT
        } else {
            sprites::FX_BULB
        };
        let scale = 0.5 + 0.6 * pop - 0.1 * within(pop, 0.7, 1.);
        look.fx(sprite, add(OVERHEAD, (0., -1.)), scale, 1.);
        if lit {
            look.eyes = Eyes::Happy;
            look.mouth = Mouth::Open;
        }
    }
}

/// 蹦一下：先蹲（预备），蹦起来时拉长，落地压扁再回弹（[`jiggle`]），到 `p` = 1 正好停稳。
/// `len` 是 `p` 走完 0 到 1 要几秒，`height` 是最高离地多少。
fn hop(look: &mut Look, p: f32, len: f32, height: f32, spring: Spring) {
    const TAKE_OFF: f32 = 0.15;
    const LAND: f32 = 0.6;
    if p < TAKE_OFF {
        look.squash = 0.12 * arch(p / TAKE_OFF);
    } else if p < LAND {
        let air = within(p, TAKE_OFF, LAND);
        look.lift = height * arch(air);
        look.squash = -0.06 * arch(air);
    } else {
        let settle = 1. - smooth(within(p, 0.85, 1.));
        look.squash = jiggle(spring, (p - LAND) * len) * settle;
    }
}

/// 蹦起来那一段（0 到 1），给空中的动作（转圈）用。
fn airborne(p: f32) -> f32 {
    within(p, 0.15, 0.6)
}

/// 转过去看看：往一边转、停一下、转到另一边、回正。眼睛比身子先过去一点。
fn look_around(look: &mut Look, p: f32, side: f32, reach: f32) {
    let facing = |p: f32| {
        if p < 0.12 {
            0.
        } else if p < 0.45 {
            side
        } else if p < 0.55 {
            0.
        } else if p < 0.88 {
            -side
        } else {
            0.
        }
    };
    look.turn = reach * facing(p);
    look.gaze = (0.9 * facing(p + 0.06), 0.);
    look.mouth = Mouth::Flat;
}

/// 每拍可能眨一下眼，眨在哪随机。笑眯着不眨。
fn blink(look: &mut Look, p: f32, beat_len: f32, salt: u64) {
    const SHUT: f32 = 0.12;
    let mut dice = Dice::new(salt ^ 0xb11c);
    if look.eyes != Eyes::Open || dice.unit() > 0.55 {
        return;
    }
    let at = dice.range(0.1, 0.9);
    if ((p - at) * beat_len).abs() < SHUT / 2. {
        look.eyes = Eyes::Shut;
    }
}

/// 要你：蹦着挥手，旁边一个感叹号。
pub(super) fn call(role: Role, temper: Temper, seed: u64, t: f32) -> Look {
    const BEAT: f32 = 1.;
    let mut look = Look::rest(role, seed);
    look.eyes = Eyes::Open;
    look.mouth = Mouth::Open;
    let beat = (t / BEAT) as u64;
    let p = (t / BEAT).fract();
    let salt = salt(seed, beat);
    let mut dice = Dice::new(salt);
    look.arms[0] = 330. + 20. * swing(p, 2.);
    let bang = add(OVERHEAD, (9., 4.));
    match dice.pick(&[3, 2]) {
        0 => hop(&mut look, p, BEAT, 5., temper.spring),
        _ => look.sway = 1.2 * swing(p, 4.) * arch(p),
    }
    look.fx(
        sprites::FX_BANG,
        bang,
        1. + 0.15 * arch((p * 2.).fract()),
        1.,
    );
    blink(&mut look, p, BEAT, salt);
    look
}

/// 干完了：两手举高、撒彩纸，再来一下性格里的招牌动作（[`Flourish`]）。`t` 超过 [`CHEER`]
/// 就是站姿。
pub(super) const CHEER: f32 = 1.6;

pub(super) fn cheer(role: Role, temper: Temper, seed: u64, t: f32) -> Look {
    let mut look = Look::rest(role, seed);
    let p = t / CHEER;
    // 从站姿的胳膊（带随机偏移）起落，结束时正好回到站姿，不跳一下。
    let [left, right] = look.arms;
    let up = arch(within(p, 0.05, 0.95));
    let up = smooth(up * 1.6);
    look.arms = [lerp(left, 330., up), lerp(right, 30., up)];
    flourish(&mut look, role, temper, p);
    look.eyes = Eyes::Happy;
    look.mouth = if p < 0.85 { Mouth::Open } else { Mouth::Smile };
    look.front = role.front().map(|front| match front {
        Front::Laptop(_) => Front::Laptop(4),
        Front::Notepad(_) => Front::Notepad(4),
    });
    let mut dice = Dice::new(seed ^ 0xc4ee);
    for k in 0..8 {
        // 先把这一片的随机数都掷完再看要不要画，掷的次数不随时间变。
        let start = dice.range(0.05, 0.35);
        let x = dice.range(-15., 15.);
        let drift = dice.range(-3., 3.);
        let age = within(p, start, start + 0.6);
        if age <= 0. || age >= 1. {
            continue;
        }
        let y = -30. + 30. * age;
        let wobble = 1.5 * swing(age, 2.);
        let sprite = sprites::CONFETTI + k % 4;
        look.fx(
            sprite,
            (x + drift * age + wobble, y),
            0.9,
            1. - within(age, 0.7, 1.),
        );
    }
    for k in 0..2 {
        let pop = within(p, 0.2 + 0.2 * k as f32, 0.6 + 0.2 * k as f32);
        if pop > 0. && pop < 1. {
            let at = add(
                OVERHEAD,
                (if k == 0 { -9. } else { 9. }, 3. - 3. * k as f32),
            );
            look.fx(sprites::FX_STAR, at, 0.4 + 0.6 * arch(pop), arch(pop));
        }
    }
    look
}

/// 欢呼的招牌动作，`p` 是欢呼走到哪。赛车手坐在车里，只会蹦。
fn flourish(look: &mut Look, role: Role, temper: Temper, p: f32) {
    let spring = temper.spring;
    let flourish = match role {
        Role::Racer => Flourish::Bounce,
        _ => temper.flourish,
    };
    match flourish {
        Flourish::Spin => {
            let q = within(p, 0., 0.75);
            hop(look, q, 0.75 * CHEER, 7., spring);
            look.turn = (360. * smooth(airborne(q))).rem_euclid(360.);
        }
        Flourish::Bounce => {
            if p < 0.35 {
                hop(look, within(p, 0., 0.35), 0.35 * CHEER, 3., spring);
            } else {
                hop(look, within(p, 0.35, 0.8), 0.45 * CHEER, 6., spring);
            }
        }
        Flourish::Wiggle => {
            hop(look, within(p, 0., 0.5), 0.5 * CHEER, 3., spring);
            // 蹦完扭三下：左、右、左，每下侧过去一格，身子跟着往那边挪。
            let w = within(p, 0.45, 0.9);
            if w > 0. && w < 1. {
                let side = swing(w, 1.5).signum();
                look.turn = -45. * side;
                look.sway = -1.2 * swing(w, 1.5);
            }
        }
    }
}

/// 被选中：挥挥手。`t` 超过 [`GREET`] 就是站姿。
pub(super) const GREET: f32 = 1.1;

pub(super) fn greet(role: Role, seed: u64, t: f32) -> Look {
    let mut look = Look::rest(role, seed);
    let p = t / GREET;
    let [left, _] = look.arms;
    let up = smooth(arch(p) * 1.8);
    look.arms[0] = lerp(left, 330. + 18. * swing(p, 2.5), up);
    look.lift = 1.5 * arch(within(p, 0., 0.4));
    look.eyes = if p > 0.2 && p < 0.8 {
        Eyes::Happy
    } else {
        Eyes::Open
    };
    look.mouth = if up > 0.5 { Mouth::Open } else { Mouth::Smile };
    look
}

/// 不能动（看不见、减弱动态效果）时各状态的定格：干活的摆着干活的架势，喊你的举着手、
/// 旁边有感叹号。
pub(super) fn still_work(role: Role, seed: u64) -> Look {
    let mut look = Look::rest(role, seed);
    look.front = role.front().map(|front| match front {
        Front::Laptop(_) => Front::Laptop(4),
        Front::Notepad(_) => Front::Notepad(3),
    });
    if role == Role::Gardener {
        look.sprout = Some(2);
    }
    look.eyes = Eyes::Open;
    look
}

pub(super) fn still_call(role: Role, seed: u64) -> Look {
    let mut look = Look::rest(role, seed);
    look.arms[0] = 330.;
    look.eyes = Eyes::Open;
    look.mouth = Mouth::Open;
    look.fx(sprites::FX_BANG, add(OVERHEAD, (9., 4.)), 1., 1.);
    look
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEEDS: [u64; 4] = [0, 1, 0xdead_beef, u64::MAX];

    /// 每样性格的每一种都至少出现在一份里。
    const TEMPERS: [Temper; 3] = [
        Temper {
            pace: Pace::Calm,
            spring: Spring::Firm,
            curiosity: Curiosity::Focused,
            flourish: Flourish::Spin,
        },
        Temper {
            pace: Pace::Steady,
            spring: Spring::Soft,
            curiosity: Curiosity::Curious,
            flourish: Flourish::Bounce,
        },
        Temper {
            pace: Pace::Brisk,
            spring: Spring::Wobbly,
            curiosity: Curiosity::Restless,
            flourish: Flourish::Wiggle,
        },
    ];

    /// 动作只用打包里有的零件、数值都是有限的。
    fn sane(look: &Look) {
        for value in [
            look.lift,
            look.squash,
            look.sway,
            look.turn,
            look.arms[0],
            look.arms[1],
            look.prop,
            look.gaze.0,
            look.gaze.1,
        ] {
            assert!(value.is_finite());
        }
        assert!(look.squash.abs() < 0.3);
        for fx in &look.fx {
            assert!(fx.sprite < sprites::SPRITES.len());
            assert!(fx.alpha.is_finite() && fx.scale > 0.);
        }
        if let Some(Front::Laptop(lines) | Front::Notepad(lines)) = look.front {
            assert!(lines <= 4);
        }
        assert!(look.sprout.is_none_or(|stage| stage <= 4));
        if let Some(kart) = look.kart {
            assert!(kart.lean.is_finite() && kart.lean.abs() <= 30.);
            assert!(kart.steer.is_finite());
        }
    }

    #[test]
    fn every_move_stays_in_range() {
        for role in Role::ALL {
            for temper in TEMPERS {
                for seed in SEEDS {
                    for step in 0..2000 {
                        let t = step as f32 * 0.037;
                        sane(&work(role, temper, seed, t));
                        sane(&call(role, temper, seed, t));
                        sane(&cheer(role, temper, seed, t.min(CHEER)));
                        sane(&greet(role, seed, t.min(GREET)));
                    }
                }
            }
            for seed in SEEDS {
                sane(&still_work(role, seed));
                sane(&still_call(role, seed));
            }
        }
    }

    #[test]
    fn beats_start_and_end_near_the_stance() {
        // 拍与拍接得上：每拍开头、结尾的胳膊离站姿不远（站姿本身按种子偏 ±10°）。
        for role in Role::ALL {
            let ([left, right], _) = role.stance();
            for (seed, temper) in SEEDS.into_iter().zip(TEMPERS.into_iter().cycle()) {
                for beat in 0..40 {
                    for edge in [0.001, 0.999] {
                        let t = (beat as f32 + edge) * beat_len(role, temper);
                        let look = work(role, temper, seed, t);
                        assert!(look.turn.abs() < 1., "{role:?} 第 {beat} 拍没转回正面");
                        assert!(
                            (look.arms[0] - left).abs() <= 20.,
                            "{role:?} 第 {beat} 拍左手"
                        );
                        assert!(
                            (look.arms[1] - right).abs() <= 20.,
                            "{role:?} 第 {beat} 拍右手"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn work_varies_from_beat_to_beat() {
        // 同一个种子，前 20 拍里至少挑到两种动作（比较拍中间的样子）。
        for role in Role::ALL {
            let temper = TEMPERS[1];
            let looks: Vec<Look> = (0..20)
                .map(|beat| {
                    work(
                        role,
                        temper,
                        7,
                        (beat as f32 + 0.5) * beat_len(role, temper),
                    )
                })
                .collect();
            assert!(
                looks
                    .iter()
                    .any(|look| look.arms != looks[0].arms || look.fx.len() != looks[0].fx.len()),
                "{role:?} 每拍都一样"
            );
        }
    }

    #[test]
    fn one_shots_end_at_the_stance() {
        for role in Role::ALL {
            let rest = Look::rest(role, 3);
            for temper in TEMPERS {
                let cheer = cheer(role, temper, 3, CHEER);
                assert!(cheer.fx.is_empty());
                assert!((cheer.arms[0] - rest.arms[0]).abs() < 1.);
                assert!(cheer.lift.abs() < 0.01);
                assert!(cheer.squash.abs() < 0.01, "{temper:?} 欢呼完还在颤");
                assert!(
                    cheer.turn.rem_euclid(360.) < 1.,
                    "{temper:?} 欢呼完没转回正面"
                );
            }
            let greet = greet(role, 3, GREET);
            assert!((greet.arms[0] - rest.arms[0]).abs() < 1.);
        }
    }

    #[test]
    fn flourishes_differ() {
        // 同一个职业、同一个种子，招牌动作不同，欢呼到一半的样子就不同。
        let mid = |flourish| {
            let temper = Temper {
                flourish,
                ..TEMPERS[1]
            };
            let samples: Vec<(f32, f32, f32)> = (1..64)
                .map(|k| cheer(Role::Fixer, temper, 3, CHEER * k as f32 / 64.))
                .map(|look| (look.lift, look.turn, look.sway))
                .collect();
            samples
        };
        let looks = [
            mid(Flourish::Spin),
            mid(Flourish::Bounce),
            mid(Flourish::Wiggle),
        ];
        assert_ne!(looks[0], looks[1]);
        assert_ne!(looks[1], looks[2]);
        assert_ne!(looks[0], looks[2]);
        // 转圈的转过背面，扭的只侧过去。
        assert!(
            looks[0]
                .iter()
                .any(|(_, turn, _)| (turn - 180.).abs() < 30.)
        );
        assert!(looks[2].iter().all(|(_, turn, _)| turn.abs() <= 45.));
    }

    #[test]
    fn landing_jiggles_by_temper_and_settles() {
        // 刚落地压扁，软糯的比紧实的压得扁、过一会儿还在颤；最后都停稳。
        assert!(jiggle(Spring::Wobbly, 0.) > jiggle(Spring::Firm, 0.));
        let wobble = |spring| {
            (30..60)
                .map(|k| jiggle(spring, k as f32 / 100.).abs())
                .fold(0., f32::max)
        };
        assert!(wobble(Spring::Wobbly) > 3. * wobble(Spring::Firm));
        for spring in [Spring::Firm, Spring::Soft, Spring::Wobbly] {
            let mut look = Look::rest(Role::Fixer, 1);
            hop(&mut look, 1., 1., 5., spring);
            assert!(look.squash.abs() < 1e-3 && look.lift.abs() < 1e-3);
        }
    }

    #[test]
    fn flourish_moves_never_come_twice_in_a_row() {
        for seed in SEEDS {
            let picks: Vec<usize> = (0..400)
                .map(|beat| choose(seed, beat, &[3, 2, 2]))
                .collect();
            assert!(
                picks.windows(2).all(|pair| pair[0] == 0 || pair[1] == 0),
                "{picks:?}"
            );
            // 偶尔的还是常有：不会被压得几乎不出现。
            let special = picks.iter().filter(|&&pick| pick != 0).count();
            assert!(special > 60, "{special}");
        }
    }

    #[test]
    fn tempers_change_the_work() {
        // 节奏快的拍子短；坐不住的会转过去看看，埋头的从不转。
        let brisk = beat_len(Role::Fixer, TEMPERS[2]);
        let calm = beat_len(Role::Fixer, TEMPERS[0]);
        assert!(brisk < calm);
        let turns = |temper: Temper| {
            (0..400)
                .map(|beat| {
                    work(
                        Role::Fixer,
                        temper,
                        9,
                        (beat as f32 + 0.3) * beat_len(Role::Fixer, temper),
                    )
                })
                .filter(|look| look.turn != 0.)
                .count()
        };
        assert_eq!(turns(TEMPERS[0]), 0);
        assert!(turns(TEMPERS[2]) > 12);
    }

    #[test]
    fn dice_picks_by_weight() {
        let mut dice = Dice::new(1);
        let mut counts = [0; 3];
        for _ in 0..3000 {
            counts[dice.pick(&[5, 2, 1])] += 1;
        }
        assert!(
            counts[0] > counts[1] && counts[1] > counts[2] && counts[2] > 0,
            "{counts:?}"
        );
    }
}
