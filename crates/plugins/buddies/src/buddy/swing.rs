//! 头顶挂着的部分（天线、毛线帽的球、芽）跟着身子慢半拍地晃：身子一动，它先被甩在后面，再
//! 弹回来晃几下停住（跟随动作）。
//!
//! 挂着的部分当成一个拴在弹簧上的小球：静止时球在挂点外「挂着的部分的重心」那里（跟着身子
//! 压扁、离地、晃、歪），动起来球按阻尼弹簧追这个位置；画的时候挂着的部分绕挂点转到球那边。
//! 只看角度，不管拉长。追的快慢按挂着的部分有多长定，长的晃得慢，像钟摆。
//!
//! 动作本身是按时间现算的，晃不是：球在哪要接着上一帧算，状态放在头像（[`super::Portrait`]）
//! 上，每帧走一步。两帧隔太久（看不见、切走过）就不接着算，直接回到静止。

use std::f32::consts::TAU;
use std::time::Instant;

use super::moves::Look;
use super::{Buddy, OnBody};

/// 每小步最多多长（秒）：30 帧一秒的两帧之间分几小步走，弹簧走得稳。
const STEP: f32 = 1. / 120.;
/// 两帧隔得比这久（秒）就不接着算。
const GAP: f32 = 0.25;
/// 阻尼比：小的弹得多。挂着的东西轻，多弹几下。
const DAMPING: f32 = 0.18;
/// 最多偏多少度：再大看着像要掉下来。
const MOST: f32 = 30.;
/// 偏得比这小（度）、球也几乎不动了，就算停住。
const SETTLED: f32 = 0.2;

/// 挂着的部分晃到哪（[`Portrait`](super::Portrait) 上的状态）。
#[derive(Default)]
pub(super) struct Swing {
    state: Option<State>,
}

#[derive(Clone, Copy)]
struct State {
    at: Instant,
    buddy: Buddy,
    /// 静止时球该在哪（画布上，pt）。
    rest: (f32, f32),
    /// 球在哪、多快（pt、pt/秒）。
    ball: (f32, f32),
    velocity: (f32, f32),
}

impl Swing {
    /// `buddy` 在 `now` 摆着 `look`：走到这一刻，返回挂着的部分绕挂点偏了多少度（画面里顺时针
    /// 为正），和还在不在晃。没戴挂着东西的不晃。
    pub(super) fn step(&mut self, buddy: Buddy, look: &Look, now: Instant) -> (f32, bool) {
        let Some(dangle) = buddy.dangle() else {
            self.state = None;
            return (0., false);
        };
        let (pivot, reach) = dangle.at(look.turn);
        let body = OnBody::new(look);
        let hinge = body.point(pivot);
        let rest = body.point((pivot.0 + reach.0, pivot.1 + reach.1));
        let still = State {
            at: now,
            buddy,
            rest,
            ball: rest,
            velocity: (0., 0.),
        };
        let mut state = match self.state {
            Some(state) if state.buddy == buddy && now >= state.at => state,
            _ => still,
        };
        let dt = now.saturating_duration_since(state.at).as_secs_f32();
        if dt > GAP {
            state = still;
        } else if dt > 0. {
            let omega = TAU * frequency(reach.0.hypot(reach.1));
            follow(&mut state, rest, dt, omega);
            state.at = now;
        }
        let angle = bearing(sub(state.ball, hinge)) - bearing(sub(rest, hinge));
        let angle = (angle + 180.).rem_euclid(360.) - 180.;
        let speed = state.velocity.0.hypot(state.velocity.1);
        let swinging = angle.abs() > SETTLED || speed > 1.;
        if !swinging {
            state.ball = rest;
            state.velocity = (0., 0.);
        }
        self.state = Some(state);
        if swinging {
            (MOST * (angle / MOST).tanh(), true)
        } else {
            (0., false)
        }
    }
}

/// 挂着的部分重心离挂点 `length`（pt）时晃多快（每秒几下）：像钟摆，长的慢。
fn frequency(length: f32) -> f32 {
    (3.2 * (2.5 / length.max(0.5)).sqrt()).clamp(1.5, 5.)
}

/// 球按阻尼弹簧追静止的位置，从上一帧的 `state.rest` 匀速挪到这一帧的 `rest`，走 `dt` 秒。
/// 阻尼按球相对静止位置的速度算：身子匀速挪时球不拖后，只在快慢变的时候甩。
fn follow(state: &mut State, rest: (f32, f32), dt: f32, omega: f32) {
    let steps = (dt / STEP).ceil().max(1.);
    let h = dt / steps;
    let from = state.rest;
    let drift = ((rest.0 - from.0) / dt, (rest.1 - from.1) / dt);
    for k in 1..=steps as usize {
        let mix = k as f32 / steps;
        let target = (
            from.0 + (rest.0 - from.0) * mix,
            from.1 + (rest.1 - from.1) * mix,
        );
        let pull = |ball: f32, velocity: f32, target: f32, drift: f32| {
            omega * omega * (target - ball) - 2. * DAMPING * omega * (velocity - drift)
        };
        let (ball, velocity) = (&mut state.ball, &mut state.velocity);
        velocity.0 += pull(ball.0, velocity.0, target.0, drift.0) * h;
        velocity.1 += pull(ball.1, velocity.1, target.1, drift.1) * h;
        ball.0 += velocity.0 * h;
        ball.1 += velocity.1 * h;
    }
    state.rest = rest;
}

fn sub(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    (a.0 - b.0, a.1 - b.1)
}

/// 画面里的方向（y 朝下）是正上方起顺时针几度。
fn bearing((x, y): (f32, f32)) -> f32 {
    x.atan2(-y).to_degrees()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::buddy::moves::Role;

    /// 戴天线的小人。
    fn antenna() -> Buddy {
        (0..)
            .map(|k| Buddy::of(&format!("antenna-{k}")))
            .find(|buddy| buddy.dangle().is_some())
            .unwrap()
    }

    /// 从 `start` 起每帧（30 帧一秒）按 `look` 摆，`seconds` 秒里每帧偏多少度。
    fn run(buddy: Buddy, seconds: f32, look: impl Fn(f32) -> Look) -> Vec<(f32, bool)> {
        let start = Instant::now();
        let mut swing = Swing::default();
        (0..(seconds * 30.) as u32)
            .map(|frame| {
                let t = frame as f32 / 30.;
                swing.step(buddy, &look(t), start + Duration::from_secs_f32(t))
            })
            .collect()
    }

    #[test]
    fn standing_still_does_not_swing() {
        let buddy = antenna();
        let rest = Look::rest(Role::Coder, 1);
        for (angle, swinging) in run(buddy, 2., |_| rest.clone()) {
            assert_eq!(angle, 0.);
            assert!(!swinging);
        }
    }

    #[test]
    fn a_sudden_lean_lags_then_settles() {
        // 身子猛地往右挪：挂着的被甩在后面（偏向左，逆时针为负），弹回来，一秒多后停住。
        let buddy = antenna();
        let frames = run(buddy, 3., |t| {
            let mut look = Look::rest(Role::Coder, 1);
            look.sway = if t >= 0.5 { 2. } else { 0. };
            look
        });
        let kick = &frames[15..30];
        let least = kick.iter().map(|(angle, _)| *angle).fold(0., f32::min);
        let most = kick.iter().map(|(angle, _)| *angle).fold(0., f32::max);
        assert!(least < -5., "{least}");
        assert!(most > 0.5, "回弹 {most}");
        assert!(frames[..15].iter().all(|(angle, _)| *angle == 0.));
        let (angle, swinging) = *frames.last().unwrap();
        assert_eq!(angle, 0.);
        assert!(!swinging);
        assert!(frames.iter().all(|(angle, _)| angle.abs() <= MOST));
    }

    #[test]
    fn steady_drift_does_not_drag() {
        // 匀速往一边挪（起步那一下之后）不拖着。
        let buddy = antenna();
        let frames = run(buddy, 3., |t| {
            let mut look = Look::rest(Role::Coder, 1);
            look.sway = 0.5 * t;
            look
        });
        assert!(frames[60..].iter().all(|(angle, _)| angle.abs() < 1.));
    }

    #[test]
    fn a_long_gap_starts_over() {
        let buddy = antenna();
        let start = Instant::now();
        let mut swing = Swing::default();
        let rest = Look::rest(Role::Coder, 1);
        swing.step(buddy, &rest, start);
        let mut moved = rest.clone();
        moved.sway = 3.;
        let later = start + Duration::from_secs(5);
        assert_eq!(swing.step(buddy, &moved, later), (0., false));
    }

    #[test]
    fn buddies_without_dangles_never_swing() {
        let plain = (0..)
            .map(|k| Buddy::of(&format!("plain-{k}")))
            .find(|buddy| buddy.dangle().is_none())
            .unwrap();
        let frames = run(plain, 1., |t| {
            let mut look = Look::rest(Role::Coder, 1);
            look.sway = if t >= 0.3 { 2. } else { 0. };
            look
        });
        assert!(frames.iter().all(|frame| *frame == (0., false)));
    }
}
