//! 什么时候演什么：每段会话的小人在演哪一出（[`Act`]）、从什么时候起、用哪个种子。
//!
//! 只在侧栏重算、选中变了时（事件里）改：开始跑就干活，要你就喊，跑完欢呼一下，被选中时
//! 闲着就打个招呼。第一次看到的会话（打开 app、远端连上）只按状态摆，不起反应。演一遍就停的
//! 那几出不用定时器收尾：render 按时间取样，过了时长就画站姿，也不再要下一帧。

use std::cell::Cell;
use std::collections::HashMap;
use std::time::Instant;

use gpui::AnyElement;
use smelt_plugin_api::model::agent_status::AgentStatus;

use super::moves::{self, Look, Role};
use super::{Buddy, figure, fnv};
use crate::model::ConversationKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    /// 站着，不动。
    Rest,
    /// 干活，一直演到状态变。
    Work,
    /// 喊你，一直演到状态变。
    Call,
    /// 跑完欢呼一下，演完站着。
    Cheer,
    /// 被选中打个招呼，演完站着。
    Greet,
}

impl Act {
    fn of(status: AgentStatus) -> Self {
        match status {
            AgentStatus::Idle => Self::Rest,
            AgentStatus::Running => Self::Work,
            AgentStatus::NeedsYou => Self::Call,
        }
    }

    /// 状态从 `from` 变成 `to` 时演哪一出。
    fn on_change(from: AgentStatus, to: AgentStatus) -> Self {
        match (from, to) {
            (_, AgentStatus::NeedsYou) => Self::Call,
            (_, AgentStatus::Running) => Self::Work,
            (AgentStatus::Running, AgentStatus::Idle) => Self::Cheer,
            (_, AgentStatus::Idle) => Self::Rest,
        }
    }

    /// 演一遍就停的那几出演多久（秒）；一直演的是 `None`。
    fn length(self) -> Option<f32> {
        match self {
            Self::Cheer => Some(moves::CHEER),
            Self::Greet => Some(moves::GREET),
            Self::Rest | Self::Work | Self::Call => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Playing {
    act: Act,
    since: Instant,
    /// 这一出的随机种子，每换一出换一个：站姿、每拍挑的动作都按它。
    seed: u64,
}

impl Playing {
    /// 闲着：站着，或演一遍的那出已经演完。
    fn idle(&self, now: Instant) -> bool {
        match self.act.length() {
            Some(length) => elapsed(self.since, now) >= length,
            None => self.act == Act::Rest,
        }
    }

    /// 此刻的样子，和还要不要下一帧。`moving` 为假时给定格。
    fn look(&self, role: Role, now: Instant, moving: bool) -> (Look, bool) {
        let t = elapsed(self.since, now);
        let seed = self.seed;
        match self.act {
            Act::Work if moving => (moves::work(role, seed, t), true),
            Act::Work => (moves::still_work(role, seed), false),
            Act::Call if moving => (moves::call(role, seed, t), true),
            Act::Call => (moves::still_call(role, seed), false),
            Act::Cheer if moving && t < moves::CHEER => (moves::cheer(role, seed, t), true),
            Act::Greet if moving && t < moves::GREET => (moves::greet(role, seed, t), true),
            Act::Rest | Act::Cheer | Act::Greet => (Look::rest(role, seed), false),
        }
    }
}

fn elapsed(since: Instant, now: Instant) -> f32 {
    now.saturating_duration_since(since).as_secs_f32()
}

struct Actor {
    status: AgentStatus,
    playing: Playing,
}

/// 列表视图上的小人状态。
pub(in crate::list) struct Stage {
    actors: HashMap<ConversationKey, Actor>,
    /// 上一次看到的选中会话，比出选中变了没有。
    selected: Option<ConversationKey>,
    /// 掷过几次种子，混进下一个种子里。
    rolls: u64,
}

impl Stage {
    /// `rows` 是侧栏里所有的会话和状态。一开始看到的都只按状态摆。
    pub(in crate::list) fn new<'a>(
        rows: impl IntoIterator<Item = (&'a ConversationKey, AgentStatus)>,
        selected: Option<&ConversationKey>,
        now: Instant,
    ) -> Self {
        let start = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos() as u64);
        let mut stage = Self {
            actors: HashMap::new(),
            selected: selected.cloned(),
            rolls: start,
        };
        stage.track(rows, selected, now);
        stage
    }

    /// 侧栏重算过、或选中变了：状态变了的换一出，新来的按状态摆，没了的去掉；刚选中的
    /// 闲着就打个招呼。
    pub(in crate::list) fn track<'a>(
        &mut self,
        rows: impl IntoIterator<Item = (&'a ConversationKey, AgentStatus)>,
        selected: Option<&ConversationKey>,
        now: Instant,
    ) {
        let mut seen = Vec::new();
        for (key, status) in rows {
            seen.push(key.clone());
            let act = match self.actors.get(key) {
                None => Act::of(status),
                Some(actor) if actor.status != status => Act::on_change(actor.status, status),
                Some(_) => continue,
            };
            let playing = self.play(key, act, now);
            self.actors.insert(key.clone(), Actor { status, playing });
        }
        if seen.len() != self.actors.len() {
            let seen: std::collections::HashSet<_> = seen.into_iter().collect();
            self.actors.retain(|key, _| seen.contains(key));
        }
        if self.selected.as_ref() != selected {
            self.selected = selected.cloned();
            if let Some(key) = selected {
                self.greet(key, now);
            }
        }
    }

    /// `key` 闲着就打个招呼（被选中、刚新建出来）。
    pub(in crate::list) fn greet(&mut self, key: &ConversationKey, now: Instant) {
        let idle = self
            .actors
            .get(key)
            .is_some_and(|actor| actor.playing.idle(now));
        if idle {
            let playing = self.play(key, Act::Greet, now);
            if let Some(actor) = self.actors.get_mut(key) {
                actor.playing = playing;
            }
        }
    }

    fn play(&mut self, key: &ConversationKey, act: Act, now: Instant) -> Playing {
        self.rolls = self.rolls.wrapping_add(1);
        Playing {
            act,
            since: now,
            seed: fnv(&key.id) ^ self.rolls.wrapping_mul(0x9e37_79b9_7f4a_7c15),
        }
    }

    /// 这一帧要的东西。`moving` 为假（看不见、减弱动态效果）时都画定格。
    pub(in crate::list) fn frame(&self, now: Instant, moving: bool) -> StageFrame {
        StageFrame {
            playing: self
                .actors
                .iter()
                .map(|(key, actor)| (key.clone(), actor.playing))
                .collect(),
            now,
            moving,
            wants_frame: Cell::new(false),
        }
    }
}

/// 一帧里各行画小人用的取样。
pub(in crate::list) struct StageFrame {
    playing: HashMap<ConversationKey, Playing>,
    now: Instant,
    moving: bool,
    wants_frame: Cell<bool>,
}

impl StageFrame {
    /// 画 `key` 那一行的小人。在动的记下来，画完这一遍由视图要下一帧：只有真画出来的行
    /// 算数，折起来的组里在跑的不让整个列表一直重绘。
    pub(in crate::list) fn draw(
        &self,
        key: &ConversationKey,
        buddy: Buddy,
        status: AgentStatus,
    ) -> AnyElement {
        let role = buddy.role();
        let (look, animating) = match self.playing.get(key) {
            Some(playing) => playing.look(role, self.now, self.moving),
            // 侧栏和小人状态在同一个事件里更新，到不了这里；万一到了按状态画定格。
            None => Playing {
                act: Act::of(status),
                since: self.now,
                seed: fnv(&key.id),
            }
            .look(role, self.now, false),
        };
        if animating {
            self.wants_frame.set(true);
        }
        figure(buddy, &look)
    }

    /// 这一遍画了在动的小人，要下一帧。
    pub(in crate::list) fn wants_frame(&self) -> bool {
        self.wants_frame.get()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use smelt_plugin_api::RuntimeKey;

    use super::*;

    fn key(id: &str) -> ConversationKey {
        ConversationKey {
            machine: RuntimeKey::Local,
            id: id.to_string(),
        }
    }

    fn act(stage: &Stage, id: &str) -> Act {
        stage.actors[&key(id)].playing.act
    }

    #[test]
    fn first_sight_follows_status_without_reacting() {
        let (a, b, c) = (key("a"), key("b"), key("c"));
        let now = Instant::now();
        let stage = Stage::new(
            [
                (&a, AgentStatus::Idle),
                (&b, AgentStatus::Running),
                (&c, AgentStatus::NeedsYou),
            ],
            Some(&a),
            now,
        );
        assert_eq!(act(&stage, "a"), Act::Rest);
        assert_eq!(act(&stage, "b"), Act::Work);
        assert_eq!(act(&stage, "c"), Act::Call);
    }

    #[test]
    fn status_changes_pick_the_act() {
        let a = key("a");
        let now = Instant::now();
        let mut stage = Stage::new([(&a, AgentStatus::Idle)], None, now);
        let mut step = |status, expected| {
            stage.track([(&a, status)], None, now);
            assert_eq!(act(&stage, "a"), expected, "变成 {status:?}");
        };
        step(AgentStatus::Running, Act::Work);
        step(AgentStatus::NeedsYou, Act::Call);
        step(AgentStatus::Running, Act::Work);
        step(AgentStatus::Idle, Act::Cheer);
        step(AgentStatus::NeedsYou, Act::Call);
        step(AgentStatus::Idle, Act::Rest);
    }

    #[test]
    fn same_status_keeps_playing() {
        let a = key("a");
        let now = Instant::now();
        let mut stage = Stage::new([(&a, AgentStatus::Running)], None, now);
        let before = stage.actors[&a].playing.seed;
        stage.track(
            [(&a, AgentStatus::Running)],
            None,
            now + Duration::from_secs(5),
        );
        assert_eq!(stage.actors[&a].playing.seed, before);
    }

    #[test]
    fn selecting_an_idle_session_greets_once() {
        let (a, b) = (key("a"), key("b"));
        let now = Instant::now();
        let rows = [(&a, AgentStatus::Idle), (&b, AgentStatus::Running)];
        let mut stage = Stage::new(rows, None, now);
        stage.track(rows, Some(&a), now);
        assert_eq!(act(&stage, "a"), Act::Greet);
        // 在忙的不打招呼。
        stage.track(rows, Some(&b), now);
        assert_eq!(act(&stage, "b"), Act::Work);
        // 招呼还没打完再选回来，不从头打。
        let since = stage.actors[&a].playing.since;
        stage.track(rows, Some(&a), now + Duration::from_millis(300));
        assert_eq!(stage.actors[&a].playing.since, since);
    }

    #[test]
    fn one_shots_settle_without_frames() {
        let a = key("a");
        let now = Instant::now();
        let mut stage = Stage::new([(&a, AgentStatus::Running)], None, now);
        stage.track([(&a, AgentStatus::Idle)], None, now);
        let role = Role::Coder;
        let playing = stage.actors[&a].playing;
        assert!(playing.look(role, now, true).1);
        let later = now + Duration::from_secs(5);
        let (look, animating) = playing.look(role, later, true);
        assert!(!animating);
        assert_eq!(look, Look::rest(role, playing.seed));
        assert!(playing.idle(later));
    }

    #[test]
    fn nothing_moves_when_motion_is_off() {
        let a = key("a");
        let now = Instant::now();
        let stage = Stage::new([(&a, AgentStatus::Running)], None, now);
        let frame = stage.frame(now, false);
        frame.draw(&a, Buddy(0), AgentStatus::Running);
        assert!(!frame.wants_frame());
        let frame = stage.frame(now, true);
        frame.draw(&a, Buddy(0), AgentStatus::Running);
        assert!(frame.wants_frame());
    }

    #[test]
    fn gone_sessions_are_dropped() {
        let (a, b) = (key("a"), key("b"));
        let now = Instant::now();
        let mut stage = Stage::new(
            [(&a, AgentStatus::Idle), (&b, AgentStatus::Idle)],
            None,
            now,
        );
        stage.track([(&a, AgentStatus::Idle)], None, now);
        assert!(!stage.actors.contains_key(&b));
    }
}
