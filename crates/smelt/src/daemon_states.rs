//! 各运行时上报的会话状态和会话提醒镜像（GPUI 全局单例）。会话状态由唯一的守护订阅循环写，
//! 提醒由 `attention.changed` 话题写（都见 `main`），插件经宿主接口读（见 `plugin_host`），
//! 变了发 `HostEvent::SessionsChanged`。

use std::collections::HashMap;

use gpui::{App, Global};

use smelt_client::daemon_state::{DaemonSessionState, RuntimePresence};
use smelt_client::runtime::RuntimeKey;
use smelt_event_protocol::{AttentionChanged, AttentionItem, AttentionSnapshot, SessionAttention};
use smelt_plugin_api::TopicMessage;

/// 各运行时上报的会话状态镜像（全局单例，跨窗口共享）。按运行时分开：每个运行时一条
/// 常驻 subscribe，各自的快照只替换自己那份，断线也只影响自己那份。会话 key =
/// smeltd session id（每个 pane 一个）/ ACP 会话的 `acp-` 前缀 sid。终端 hook 与 ACP
/// 协议状态都先在运行时归约，再共用同一套状态着色链路。
///
/// 写入只走这里的几个方法：先只读比较，真变了才可写地取全局。GPUI 里可写地取一下就算变了，
/// 观察者（转成插件的 `SessionsChanged`、守护换代等会话停下来）都会被叫醒，读过它的视图
/// 都要重建；运行时重发一份一样的状态不能惊动它们。
#[derive(Default)]
pub struct DaemonStates {
    next_revision: u64,
    runtimes: HashMap<RuntimeKey, RuntimeMirror>,
}

#[derive(Default)]
struct RuntimeMirror {
    sessions: HashMap<String, DaemonSessionState>,
    revision: u64,
    /// 收到过至少一帧快照（空目录也算）。
    primed: bool,
    /// 当前这条 subscribe 连接是否仍活着。守护 exec 后旧连接断开，必须先置
    /// false，否则上一 epoch 的快照会把「正在交接」误判成会话已死。
    subscription_live: bool,
    attention: Attention,
}

/// 一台机器的会话提醒。运行时（smeltd）是唯一权威，这里只照它推来的留一份，不自己判定。
///
/// 订上（含重连）时运行时先发整份快照，之后逐条发变化，每条带递增的 `revision`。快照整份
/// 换掉；比手里旧的变化（快照前排着的）丢掉。断开、积压只是提示，后面一定跟着快照。
#[derive(Default)]
struct Attention {
    /// 收到过快照：这台机器上没在镜像里的会话确实没有提醒。
    primed: bool,
    revision: u64,
    /// 有提醒的会话；两样都没有的不在里面。
    sessions: HashMap<String, SessionAttention>,
}

impl DaemonStates {
    fn mirror<'a>(runtime: &RuntimeKey, cx: &'a App) -> Option<&'a RuntimeMirror> {
        cx.try_global::<Self>()?.runtimes.get(runtime)
    }

    fn mutate_mirror(runtime: &RuntimeKey, cx: &mut App, update: impl FnOnce(&mut RuntimeMirror)) {
        let states = cx.default_global::<Self>();
        states.next_revision = states.next_revision.wrapping_add(1).max(1);
        let rev = states.next_revision;
        let mirror = states.runtimes.entry(runtime.clone()).or_default();
        update(mirror);
        mirror.revision = rev;
    }

    /// 该运行时最近一次状态镜像变化后的单调修订号（未变动时保持不变）。
    pub fn revision(runtime: &RuntimeKey, cx: &App) -> u64 {
        Self::mirror(runtime, cx).map_or(0, |mirror| mirror.revision)
    }

    /// 一个运行时的完整快照：换掉它那份会话状态（提醒走自己的快照）。
    pub fn replace_all(runtime: &RuntimeKey, list: Vec<DaemonSessionState>, cx: &mut App) {
        let sessions: HashMap<String, DaemonSessionState> = list
            .into_iter()
            .map(|state| (state.id.clone(), state))
            .collect();
        if Self::mirror(runtime, cx).is_some_and(|current| {
            current.primed && current.subscription_live && current.sessions == sessions
        }) {
            return;
        }
        Self::mutate_mirror(runtime, cx, |mirror| {
            mirror.sessions = sessions;
            mirror.primed = true;
            mirror.subscription_live = true;
        });
    }

    pub fn remove(runtime: &RuntimeKey, id: &str, cx: &mut App) {
        if Self::mirror(runtime, cx).is_some_and(|mirror| mirror.sessions.contains_key(id)) {
            Self::mutate_mirror(runtime, cx, |mirror| {
                mirror.sessions.remove(id);
            });
        }
    }

    pub fn upsert(runtime: &RuntimeKey, state: DaemonSessionState, cx: &mut App) {
        let current = Self::mirror(runtime, cx).and_then(|mirror| mirror.sessions.get(&state.id));
        if current == Some(&state) {
            return;
        }
        Self::mutate_mirror(runtime, cx, |mirror| {
            mirror.sessions.insert(state.id.clone(), state);
        });
    }

    /// 运行时被移除：它那份镜像整个丢掉。
    pub fn forget_runtime(runtime: &RuntimeKey, cx: &mut App) {
        if Self::mirror(runtime, cx).is_some() {
            let states = cx.default_global::<Self>();
            states.next_revision = states.next_revision.wrapping_add(1).max(1);
            states.runtimes.remove(runtime);
        }
    }

    /// 这个运行时的 subscribe 断开：它那份镜像不再代表活着的 daemon epoch。
    pub fn mark_subscription_disconnected(runtime: &RuntimeKey, cx: &mut App) {
        if Self::mirror(runtime, cx).is_some_and(|mirror| mirror.subscription_live) {
            Self::mutate_mirror(runtime, cx, |mirror| {
                mirror.subscription_live = false;
            });
        }
    }

    pub fn snapshot(runtime: &RuntimeKey, cx: &App) -> Vec<DaemonSessionState> {
        Self::mirror(runtime, cx)
            .map(|mirror| mirror.sessions.values().cloned().collect())
            .unwrap_or_default()
    }

    /// `None`：这个运行时的订阅未就绪，不能判死。其余见 [`RuntimePresence`]。
    pub fn runtime_presence(
        runtime: &RuntimeKey,
        session_id: &str,
        cx: &App,
    ) -> Option<RuntimePresence> {
        let mirror = Self::mirror(runtime, cx);
        DaemonSessionState::runtime_presence_from_mirror(
            mirror.is_some_and(|m| m.subscription_live),
            mirror.is_some_and(|m| m.primed),
            mirror.and_then(|m| m.sessions.get(session_id)),
        )
    }
}

impl DaemonStates {
    /// 收下那台机器推来的一条 `attention.changed`。有一段会话的提醒刚来了该响一下的那条时
    /// 返回它（和这段会话此刻的提醒），镜像已经更新。旧的、认不出的、断开积压的提示什么都
    /// 不动。
    ///
    /// 收下的每一条都可写地取全局：提醒推送只在真有变化或该响时才来，不会频繁惊动观察者。
    pub fn apply_attention(
        runtime: &RuntimeKey,
        message: &TopicMessage,
        cx: &mut App,
    ) -> Option<(SessionAttention, AttentionItem)> {
        match message {
            TopicMessage::Snapshot(value) => {
                let snapshot: AttentionSnapshot = readable(runtime, value)?;
                Self::mutate_mirror(runtime, cx, |mirror| {
                    mirror.attention = Attention {
                        primed: true,
                        revision: snapshot.revision,
                        sessions: snapshot
                            .sessions
                            .into_iter()
                            .filter(|session| !session.is_empty())
                            .map(|session| (session.session_id.clone(), session))
                            .collect(),
                    };
                });
                None
            }
            TopicMessage::Event(value) => {
                let changed: AttentionChanged = readable(runtime, value)?;
                let stale = Self::mirror(runtime, cx).is_some_and(|mirror| {
                    mirror.attention.primed && changed.revision <= mirror.attention.revision
                });
                if stale {
                    return None;
                }
                let session = changed.session;
                Self::mutate_mirror(runtime, cx, |mirror| {
                    let attention = &mut mirror.attention;
                    attention.revision = changed.revision;
                    if session.is_empty() {
                        attention.sessions.remove(&session.session_id);
                    } else {
                        attention
                            .sessions
                            .insert(session.session_id.clone(), session.clone());
                    }
                });
                changed.alert.map(|alert| (session, alert))
            }
            // 积压时后面跟着一份快照；这一轮不会再推时留着上次收到的，重新连上时再订。
            TopicMessage::Lagged | TopicMessage::Unavailable(_) => None,
        }
    }

    /// 那台机器上有提醒的会话。还没收到快照是 `None`。
    pub fn attention(runtime: &RuntimeKey, cx: &App) -> Option<Vec<SessionAttention>> {
        let attention = &Self::mirror(runtime, cx)?.attention;
        attention
            .primed
            .then(|| attention.sessions.values().cloned().collect())
    }
}

/// 读不懂的载荷记一笔丢掉。
fn readable<T: serde::de::DeserializeOwned>(
    runtime: &RuntimeKey,
    value: &serde_json::Value,
) -> Option<T> {
    T::deserialize(value)
        .inspect_err(|error| eprintln!("[attention] {runtime:?} 的提醒读不懂：{error}"))
        .ok()
}

impl Global for DaemonStates {}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use serde_json::to_value;
    use smelt_event_protocol::AttentionKind;

    use super::*;

    fn state(id: &str) -> DaemonSessionState {
        DaemonSessionState {
            id: id.to_string(),
            generation: 1,
            revision: 1,
            ..Default::default()
        }
    }

    fn ids(runtime: &RuntimeKey, cx: &App) -> Vec<String> {
        DaemonStates::snapshot(runtime, cx)
            .into_iter()
            .map(|state| state.id)
            .collect()
    }

    /// 一个运行时的快照、断线只动它自己那份：远程运行时连上时不能把本机会话冲掉，
    /// 远程断线也不能让本机会话被判死。
    #[gpui::test]
    fn each_runtime_keeps_its_own_mirror(cx: &mut TestAppContext) {
        let remote = RuntimeKey::Remote("rt-linux".to_string());
        cx.update(|cx| {
            cx.set_global(DaemonStates::default());
            DaemonStates::replace_all(&RuntimeKey::Local, vec![state("t-local")], cx);
            assert_eq!(
                DaemonStates::runtime_presence(&remote, "t-remote", cx),
                None
            );

            DaemonStates::replace_all(&remote, vec![state("t-remote")], cx);
            assert_eq!(ids(&RuntimeKey::Local, cx), ["t-local"]);
            assert_eq!(ids(&remote, cx), ["t-remote"]);

            DaemonStates::mark_subscription_disconnected(&remote, cx);
            assert_eq!(
                DaemonStates::runtime_presence(&remote, "t-remote", cx),
                None
            );
            assert_ne!(
                DaemonStates::runtime_presence(&RuntimeKey::Local, "t-local", cx),
                None
            );

            DaemonStates::forget_runtime(&remote, cx);
            assert!(DaemonStates::snapshot(&remote, cx).is_empty());
            assert_eq!(DaemonStates::snapshot(&RuntimeKey::Local, cx).len(), 1);
        });
    }

    /// 运行时重发一份一样的状态、快照，或断开已经断开的订阅，都不惊动观察者：它们会把整个
    /// 会话列表重算一遍。真变了才叫醒。
    #[gpui::test]
    fn unchanged_reports_leave_observers_alone(cx: &mut TestAppContext) {
        let signals = std::rc::Rc::new(std::cell::Cell::new(0));
        cx.update(|cx| {
            cx.set_global(DaemonStates::default());
            let signals = signals.clone();
            cx.observe_global::<DaemonStates>(move |_| signals.set(signals.get() + 1))
                .detach();
        });
        let local = RuntimeKey::Local;
        let report = |run: &dyn Fn(&mut App)| cx.update(|cx| run(cx));
        let rev = || cx.read(|cx| DaemonStates::revision(&local, cx));
        report(&|cx| DaemonStates::replace_all(&local, vec![state("a")], cx));
        assert_eq!(signals.get(), 1);
        let r1 = rev();
        assert!(r1 > 0);
        report(&|cx| DaemonStates::replace_all(&local, vec![state("a")], cx));
        report(&|cx| DaemonStates::upsert(&local, state("a"), cx));
        report(&|cx| DaemonStates::remove(&local, "missing", cx));
        report(&|cx| DaemonStates::forget_runtime(&RuntimeKey::Remote("x".into()), cx));
        assert_eq!(signals.get(), 1, "什么都没变");
        assert_eq!(rev(), r1, "未变动时修订号不能涨");

        let mut busy = state("a");
        busy.phase = smelt_client::daemon_state::DaemonPhase::Thinking;
        report(&|cx| DaemonStates::upsert(&local, busy.clone(), cx));
        assert_eq!(signals.get(), 2);
        let r2 = rev();
        assert!(r2 > r1);

        report(&|cx| DaemonStates::mark_subscription_disconnected(&local, cx));
        report(&|cx| DaemonStates::mark_subscription_disconnected(&local, cx));
        assert_eq!(signals.get(), 3, "断开第二次不算变");
        assert!(rev() > r2);
    }

    fn item(kind: AttentionKind) -> AttentionItem {
        AttentionItem {
            kind,
            title: "Waiting for approval".to_string(),
            message: "rm -rf build".to_string(),
            note: None,
        }
    }

    fn unread(id: &str) -> SessionAttention {
        SessionAttention {
            session_id: id.to_string(),
            action: None,
            unread: Some(item(AttentionKind::Success)),
        }
    }

    fn snapshot(revision: u64, sessions: Vec<SessionAttention>) -> TopicMessage {
        TopicMessage::Snapshot(to_value(AttentionSnapshot { revision, sessions }).unwrap())
    }

    fn event(revision: u64, session: SessionAttention, alert: bool) -> TopicMessage {
        TopicMessage::Event(
            to_value(AttentionChanged {
                revision,
                alert: alert.then(|| item(AttentionKind::Success)),
                session,
            })
            .unwrap(),
        )
    }

    fn unread_ids(runtime: &RuntimeKey, cx: &App) -> Option<Vec<String>> {
        let mut ids: Vec<String> = DaemonStates::attention(runtime, cx)?
            .into_iter()
            .filter(|session| session.unread.is_some())
            .map(|session| session.session_id)
            .collect();
        ids.sort();
        Some(ids)
    }

    const LOCAL: RuntimeKey = RuntimeKey::Local;

    /// 没收到快照前说不准（`None`）；快照整份换掉，之前的变化不算。会话状态的快照不碰提醒。
    #[gpui::test]
    fn attention_snapshot_replaces_and_primes_the_machine(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(DaemonStates::default());
            DaemonStates::apply_attention(&LOCAL, &event(1, unread("old"), false), cx);
            assert_eq!(unread_ids(&LOCAL, cx), None, "没收到快照前不算确定");

            DaemonStates::apply_attention(&LOCAL, &snapshot(5, vec![unread("a")]), cx);
            assert_eq!(unread_ids(&LOCAL, cx), Some(vec!["a".to_string()]));

            DaemonStates::replace_all(&LOCAL, vec![state("t")], cx);
            assert_eq!(unread_ids(&LOCAL, cx), Some(vec!["a".to_string()]));
        });
    }

    /// 快照前排着的变化比快照旧，丢掉，也不响；之后的照常记，带着该响的那条。
    #[gpui::test]
    fn attention_events_older_than_the_snapshot_are_dropped(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(DaemonStates::default());
            DaemonStates::apply_attention(&LOCAL, &snapshot(5, vec![]), cx);
            assert_eq!(
                DaemonStates::apply_attention(&LOCAL, &event(5, unread("a"), true), cx),
                None
            );
            assert_eq!(unread_ids(&LOCAL, cx), Some(vec![]));

            let alert = DaemonStates::apply_attention(&LOCAL, &event(6, unread("a"), true), cx);
            assert_eq!(alert, Some((unread("a"), item(AttentionKind::Success))));
            assert_eq!(unread_ids(&LOCAL, cx), Some(vec!["a".to_string()]));
        });
    }

    /// 两样都没有的会话离开镜像；运行时重启后 revision 从头数，重连带来的快照照样整份换上。
    #[gpui::test]
    fn attention_follows_a_restarted_runtime(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(DaemonStates::default());
            DaemonStates::apply_attention(
                &LOCAL,
                &snapshot(40, vec![unread("a"), unread("b")]),
                cx,
            );
            let cleared = SessionAttention {
                session_id: "b".to_string(),
                ..Default::default()
            };
            DaemonStates::apply_attention(&LOCAL, &event(41, cleared, false), cx);
            assert_eq!(unread_ids(&LOCAL, cx), Some(vec!["a".to_string()]));

            DaemonStates::apply_attention(&LOCAL, &TopicMessage::Lagged, cx);
            DaemonStates::apply_attention(&LOCAL, &snapshot(0, vec![]), cx);
            DaemonStates::apply_attention(&LOCAL, &event(1, unread("c"), true), cx);
            assert_eq!(unread_ids(&LOCAL, cx), Some(vec!["c".to_string()]));
        });
    }

    /// 同一个会话 id 在两台机器上各是各的；机器移除了，它的提醒跟着没了。读不懂的什么都不动。
    #[gpui::test]
    fn attention_keeps_machines_apart(cx: &mut TestAppContext) {
        let remote = RuntimeKey::Remote("rt".to_string());
        cx.update(|cx| {
            cx.set_global(DaemonStates::default());
            DaemonStates::apply_attention(&LOCAL, &snapshot(1, vec![unread("a")]), cx);
            DaemonStates::apply_attention(&remote, &snapshot(1, vec![]), cx);
            assert_eq!(unread_ids(&remote, cx), Some(vec![]));

            DaemonStates::forget_runtime(&remote, cx);
            assert_eq!(unread_ids(&remote, cx), None);
            assert_eq!(unread_ids(&LOCAL, cx), Some(vec!["a".to_string()]));

            let junk = TopicMessage::Event(serde_json::json!(42));
            assert_eq!(DaemonStates::apply_attention(&remote, &junk, cx), None);
            assert_eq!(unread_ids(&remote, cx), None);
        });
    }
}
