//! 动作：某一刻小人摆成什么样（[`Look`]），按时间现算。
//!
//! 干活和喊你按拍子走：每拍按（种子, 第几拍）随机挑一个动作，动作从站姿出发、回到站姿，拍与拍
//! 之间接得上。眨眼也是每拍随机的。欢呼、打招呼是演一遍就停的，停了回站姿。站着不动：站姿按
//! 种子略有不同（胳膊、道具的角度、往哪看、表情），每次停下来换一个。
//!
//! 角度都是画面里的，正上方 0°、顺时针。胳膊 180° 是垂直往下，右胳膊小于 180° 往外、左胳膊
//! 大于 180° 往外。道具 0° 是朝上。

use std::f32::consts::{PI, TAU};
use std::ops::Range;

use super::sprites::{self, ARM_LEN};
use super::{toward, turned};

/// 每个角色都可能用到的零件（脸、身前的物件、特效），预先解。
pub(super) const SHARED: [Range<usize>; 2] = [
    sprites::EYE_OPEN..sprites::HAT_CODER,
    sprites::LAPTOP..sprites::SHADOW,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Coder,
    Fixer,
    Detective,
    Writer,
    Chemist,
    Gardener,
    Conductor,
    Sweeper,
}

/// 手里的道具：零件组的第一张，朝上时尖端（冒火星、洒水的地方）相对握点在哪。
#[derive(Clone, Copy)]
pub(super) struct Prop {
    pub(super) first: usize,
    tip: (f32, f32),
}

impl Role {
    /// 和渲染脚本里的职业顺序一致。
    pub(super) const ALL: [Self; 8] = [
        Self::Coder,
        Self::Fixer,
        Self::Detective,
        Self::Writer,
        Self::Chemist,
        Self::Gardener,
        Self::Conductor,
        Self::Sweeper,
    ];

    pub(super) fn hat(self) -> usize {
        sprites::HAT_CODER + self as usize
    }

    pub(super) fn prop(self) -> Option<Prop> {
        let (first, tip) = match self {
            Self::Coder => return None,
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
        }
    }

    /// 干活时一拍多长（秒）。
    fn beat(self) -> f32 {
        match self {
            Self::Coder | Self::Chemist | Self::Sweeper => 1.4,
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Eyes {
    Open,
    /// 笑眯成两道弯。
    Happy,
    Shut,
}

impl Eyes {
    pub(super) fn sprite(self) -> usize {
        match self {
            Self::Open => sprites::EYE_OPEN,
            Self::Happy => sprites::EYE_HAPPY,
            Self::Shut => sprites::EYE_SHUT,
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
    pub(super) fx: Vec<Fx>,
}

/// 小的确定性随机数（SplitMix64）：同一个种子出同一串，动作才能按时间现算。
struct Dice(u64);

impl Dice {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// [0, 1)
    fn unit(&mut self) -> f32 {
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

/// 头顶上方（相对脚底）。
const OVERHEAD: (f32, f32) = (0., -25.);

impl Look {
    /// 站姿。`seed` 定胳膊、道具略偏多少，往哪看，什么表情。
    pub(super) fn rest(role: Role, seed: u64) -> Self {
        let mut dice = Dice::new(seed);
        let ([left, right], prop) = role.stance();
        let mood = dice.unit();
        Self {
            lift: 0.,
            squash: 0.,
            sway: 0.,
            arms: [left + dice.range(-10., 10.), right + dice.range(-10., 10.)],
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
            fx: Vec::new(),
        }
    }

    /// 右手在哪（相对脚底，没算离地和晃）。
    fn right_hand(&self) -> (f32, f32) {
        hand(sprites::SHOULDER_R, self.arms[1])
    }

    /// 道具尖端在哪。
    fn tip(&self, prop: Prop) -> (f32, f32) {
        let (_, angle) = turned(self.prop);
        let (x, y) = prop.tip;
        let (sin, cos) = angle.to_radians().sin_cos();
        // 画面里顺时针转（y 朝下）。
        add(self.right_hand(), (x * cos - y * sin, x * sin + y * cos))
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

/// 干活。`t` 是从开始干起的秒数。
pub(super) fn work(role: Role, seed: u64, t: f32) -> Look {
    let mut look = Look::rest(role, seed);
    look.mouth = Mouth::Smile;
    look.eyes = Eyes::Open;
    let beat_len = role.beat();
    let beat = (t / beat_len) as u64;
    let p = (t / beat_len).fract();
    let salt = seed ^ beat.wrapping_mul(0x2545_f491_4f6c_dd1d);
    let mut dice = Dice::new(salt);
    // 干活时一直有点起伏，像在喘气、打拍子。
    look.squash = 0.025 * swing(t / 0.9, 1.);
    let ([left, right], prop) = role.stance();
    // 动作开头、结尾的一小段淡进淡出，接上站姿。
    let ease = ramp(p);
    match role {
        Role::Coder => {
            let lines = ((t * 2.5) as usize % 7).min(4);
            look.front = Some(Front::Laptop(lines));
            match dice.pick(&[5, 2, 1]) {
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
                    thinking_dots(&mut look, p);
                }
                _ => {
                    // 有主意了：头顶亮个灯泡，手指着它。
                    let a = arch(p);
                    look.arms[1] = lerp(right, 20., a);
                    bulb(&mut look, p);
                }
            }
        }
        Role::Fixer => match dice.pick(&[4, 3, 1]) {
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
        Role::Detective => match dice.pick(&[4, 2, 2]) {
            0 => {
                // 扫一遍：放大镜左右移，眼睛跟着。
                let s = swing(p, 1.);
                look.arms[1] = right + 30. * s;
                look.prop = prop + 20. * s;
                look.gaze = (0.9 * s, 0.3);
            }
            1 => {
                // 凑到眼前看。
                let a = smooth(arch(p) * 1.4);
                look.arms[1] = lerp(right, 40., a);
                look.prop = lerp(prop, -30., a);
                look.mouth = if a > 0.7 { Mouth::Open } else { Mouth::Flat };
            }
            _ => {
                // 左看看、右看看。
                let side = if p < 0.4 {
                    -1.
                } else if p < 0.8 {
                    1.
                } else {
                    0.
                };
                look.gaze = (1.1 * side, 0.);
                look.sway = 0.5 * side;
                look.mouth = Mouth::Flat;
            }
        },
        Role::Writer => {
            let lines = ((t * 1.5) as usize % 7).min(4);
            look.front = Some(Front::Notepad(lines));
            match dice.pick(&[5, 2, 1]) {
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
                    thinking_dots(&mut look, p);
                }
                _ => {
                    // 写得满意，蹦一下。
                    hop(&mut look, p, 3.);
                    look.eyes = Eyes::Happy;
                    look.mouth = Mouth::Open;
                }
            }
        }
        Role::Chemist => match dice.pick(&[4, 2, 1]) {
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
                look.gaze = (0.8 * a, -1. * a);
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
            // 脚边的苗跟着干活的时长长，长满了就开花。
            look.sprout = Some(((t / 4.) as usize).min(4));
            match dice.pick(&[4, 2]) {
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
        Role::Conductor => match dice.pick(&[4, 1, 1]) {
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
        Role::Sweeper => match dice.pick(&[5, 2]) {
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
    }
    blink(&mut look, p, beat_len, salt);
    look
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

/// 蹲一下、蹦起来、落地再蹲一下。`height` 是最高离地多少。
fn hop(look: &mut Look, p: f32, height: f32) {
    if p < 0.15 {
        look.squash = 0.12 * arch(p / 0.15);
    } else if p < 0.65 {
        let air = (p - 0.15) / 0.5;
        look.lift = height * arch(air);
        look.squash = -0.06 * arch(air);
    } else if p < 0.8 {
        look.squash = 0.08 * arch((p - 0.65) / 0.15);
    }
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
pub(super) fn call(role: Role, seed: u64, t: f32) -> Look {
    const BEAT: f32 = 1.;
    let mut look = Look::rest(role, seed);
    look.eyes = Eyes::Open;
    look.mouth = Mouth::Open;
    let beat = (t / BEAT) as u64;
    let p = (t / BEAT).fract();
    let salt = seed ^ beat.wrapping_mul(0x2545_f491_4f6c_dd1d);
    let mut dice = Dice::new(salt);
    look.arms[0] = 330. + 20. * swing(p, 2.);
    let bang = add(OVERHEAD, (9., 4.));
    match dice.pick(&[3, 2]) {
        0 => hop(&mut look, p, 5.),
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

/// 干完了：蹦起来、两手举高、撒彩纸。`t` 超过 [`CHEER`] 就是站姿。
pub(super) const CHEER: f32 = 1.6;

pub(super) fn cheer(role: Role, seed: u64, t: f32) -> Look {
    let mut look = Look::rest(role, seed);
    let p = t / CHEER;
    let ([left, right], _) = role.stance();
    let up = arch(within(p, 0.05, 0.95));
    let up = smooth(up * 1.6);
    look.arms = [lerp(left, 330., up), lerp(right, 30., up)];
    hop(&mut look, within(p, 0., 0.7), 5.);
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

/// 被选中：挥挥手。`t` 超过 [`GREET`] 就是站姿。
pub(super) const GREET: f32 = 1.1;

pub(super) fn greet(role: Role, seed: u64, t: f32) -> Look {
    let mut look = Look::rest(role, seed);
    let p = t / GREET;
    let ([left, _], _) = role.stance();
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

    /// 动作只用打包里有的零件、数值都是有限的。
    fn sane(look: &Look) {
        for value in [
            look.lift,
            look.squash,
            look.sway,
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
    }

    #[test]
    fn every_move_stays_in_range() {
        for role in Role::ALL {
            for seed in SEEDS {
                for step in 0..2000 {
                    let t = step as f32 * 0.037;
                    sane(&work(role, seed, t));
                    sane(&call(role, seed, t));
                    sane(&cheer(role, seed, t.min(CHEER)));
                    sane(&greet(role, seed, t.min(GREET)));
                }
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
            for seed in SEEDS {
                for beat in 0..40 {
                    for edge in [0.001, 0.999] {
                        let t = (beat as f32 + edge) * role.beat();
                        let look = work(role, seed, t);
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
            let looks: Vec<Look> = (0..20)
                .map(|beat| work(role, 7, (beat as f32 + 0.5) * role.beat()))
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
            let cheer = cheer(role, 3, CHEER);
            assert!(cheer.fx.is_empty());
            assert!((cheer.arms[0] - rest.arms[0]).abs() < 1.);
            assert!(cheer.lift.abs() < 0.01);
            let greet = greet(role, 3, GREET);
            assert!((greet.arms[0] - rest.arms[0]).abs() < 1.);
        }
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
