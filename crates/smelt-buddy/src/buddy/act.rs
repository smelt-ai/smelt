//! 一出戏：小人在演哪一出（[`Act`]）、从什么时候起、用哪个种子，此刻摆成什么样。
//!
//! 什么时候换哪一出不归这里，由用组件的一方按自己的事件定（会话状态变了、被选中了）。演一遍
//! 就停的那几出不用定时器收尾：按时间取样，过了时长就画站姿，也不再要下一帧。

use std::time::Instant;

use super::moves::{self, Look};
use super::{Buddy, fnv};

/// 演哪一出。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Act {
    /// 站着，不动。
    Rest,
    /// 干活，一直演到换下一出。
    Work,
    /// 喊你，一直演到换下一出。
    Call,
    /// 欢呼一下，演完站着。
    Cheer,
    /// 打个招呼，演完站着。
    Greet,
}

impl Act {
    pub const ALL: [Self; 5] = [Self::Rest, Self::Work, Self::Call, Self::Cheer, Self::Greet];

    /// 给脚本用的名字。
    pub fn name(self) -> &'static str {
        match self {
            Self::Rest => "rest",
            Self::Work => "work",
            Self::Call => "call",
            Self::Cheer => "cheer",
            Self::Greet => "greet",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|act| act.name() == name)
    }

    /// 演一遍就停的那几出演多久（秒）；一直演的是 `None`。
    pub fn length(self) -> Option<f32> {
        match self {
            Self::Cheer => Some(moves::CHEER),
            Self::Greet => Some(moves::GREET),
            Self::Rest | Self::Work | Self::Call => None,
        }
    }
}

/// 正在演的一出。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Playing {
    act: Act,
    since: Instant,
    /// 这一出的随机种子，每换一出换一个：站姿、每拍挑的动作都按它。
    seed: u64,
}

impl Playing {
    /// 种子是 `figure` 的小人从 `since` 起演 `act`；`salt` 每换一出换一个（开演的时刻）。
    pub(crate) fn new(figure: &str, act: Act, since: Instant, salt: u64) -> Self {
        Self {
            act,
            since,
            seed: fnv(figure) ^ salt.wrapping_mul(0x9e37_79b9_7f4a_7c15),
        }
    }

    /// `buddy` 此刻的样子，和还要不要下一帧。`moving` 为假时给定格。
    pub(super) fn look(&self, buddy: Buddy, now: Instant, moving: bool) -> (Look, bool) {
        let t = now.saturating_duration_since(self.since).as_secs_f32();
        let seed = self.seed;
        let (role, temper) = (buddy.role, buddy.temper);
        match self.act {
            Act::Work if moving => (moves::work(role, temper, seed, t), true),
            Act::Work => (moves::still_work(role, seed), false),
            Act::Call if moving => (moves::call(role, temper, seed, t), true),
            Act::Call => (moves::still_call(role, seed), false),
            Act::Cheer if moving && t < moves::CHEER => (moves::cheer(role, temper, seed, t), true),
            Act::Greet if moving && t < moves::GREET => (moves::greet(role, seed, t), true),
            Act::Rest | Act::Cheer | Act::Greet => (Look::rest(role, seed), false),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn names_round_trip() {
        for act in Act::ALL {
            assert_eq!(Act::from_name(act.name()), Some(act));
        }
        assert_eq!(Act::from_name("dance"), None);
    }

    #[test]
    fn one_shots_settle_without_frames() {
        let now = Instant::now();
        let buddy = Buddy::of("a");
        let playing = Playing::new("a", Act::Cheer, now, 1);
        assert!(playing.look(buddy, now, true).1);
        let (look, animating) = playing.look(buddy, now + Duration::from_secs(5), true);
        assert!(!animating);
        assert_eq!(look, Look::rest(buddy.role, playing.seed));
    }

    #[test]
    fn nothing_moves_when_motion_is_off() {
        let now = Instant::now();
        let buddy = Buddy::of("a");
        let working = Playing::new("a", Act::Work, now, 1);
        assert!(!working.look(buddy, now, false).1);
        assert!(working.look(buddy, now, true).1);
        assert!(
            !Playing::new("a", Act::Rest, now, 1)
                .look(buddy, now, true)
                .1
        );
    }

    #[test]
    fn each_new_act_rolls_a_new_seed() {
        let now = Instant::now();
        let first = Playing::new("a", Act::Rest, now, 1);
        let second = Playing::new("a", Act::Rest, now, 2);
        assert_ne!(first.seed, second.seed);
    }
}
