//! 各台机器上哪些守护会话有没看过的提醒（跑完了、失败了、等你批准……你还没看）。会话列表
//! 按它把标题提亮、在可视区外时提示。
//!
//! 权威在运行时（smeltd），这里只照它推来的 `attention.changed` 留一份只读缓存，不自己
//! 判定、不标已读（看过由运行时记：在别的设备上看、在终端里打字、在对话里回复、通知插件
//! 替正看着的会话标）。订上（含重连）时先来整份快照，之后逐条变化，每条带递增的
//! `revision`：快照整份换掉，比手里旧的变化（快照前排着的）丢掉。
//!
//! 通知插件也留着一份同样的镜像。两个插件各管各的用途，不互相依赖，所以各留一份。

use std::collections::{HashMap, HashSet};

use serde::de::DeserializeOwned;
use smelt_plugin_api::model::daemon_state::{AttentionChanged, AttentionSnapshot};
use smelt_plugin_api::{RuntimeKey, TopicMessage};

#[derive(Default)]
pub(crate) struct Unread {
    machines: HashMap<RuntimeKey, MachineUnread>,
}

#[derive(Default)]
struct MachineUnread {
    /// 收到过快照：之后比它旧的变化丢掉。
    primed: bool,
    revision: u64,
    /// 有没看过的提醒的守护会话 id。
    sessions: HashSet<String>,
}

impl Unread {
    /// 收下一条推送。有没看过的会话变了是 `true`。
    pub(crate) fn apply(&mut self, machine: &RuntimeKey, message: &TopicMessage) -> bool {
        match message {
            TopicMessage::Snapshot(value) => {
                let Some(snapshot) = readable::<AttentionSnapshot>(machine, value) else {
                    return false;
                };
                let sessions = snapshot
                    .sessions
                    .into_iter()
                    .filter(|session| session.unread.is_some())
                    .map(|session| session.session_id)
                    .collect();
                let entry = self.machines.entry(machine.clone()).or_default();
                let changed = entry.sessions != sessions;
                *entry = MachineUnread {
                    primed: true,
                    revision: snapshot.revision,
                    sessions,
                };
                changed
            }
            TopicMessage::Event(value) => {
                let Some(changed) = readable::<AttentionChanged>(machine, value) else {
                    return false;
                };
                let entry = self.machines.entry(machine.clone()).or_default();
                if entry.primed && changed.revision <= entry.revision {
                    return false;
                }
                entry.revision = changed.revision;
                let session = changed.session;
                if session.unread.is_some() {
                    entry.sessions.insert(session.session_id)
                } else {
                    entry.sessions.remove(&session.session_id)
                }
            }
            // 积压时后面跟着一份快照；这一轮不会再推时留着上次收到的，重新连上时再订。
            TopicMessage::Lagged | TopicMessage::Unavailable(_) => false,
        }
    }

    /// 机器移除了，它的提醒跟着没了。
    pub(crate) fn forget(&mut self, machine: &RuntimeKey) {
        self.machines.remove(machine);
    }

    pub(crate) fn contains(&self, machine: &RuntimeKey, session_id: &str) -> bool {
        self.machines
            .get(machine)
            .is_some_and(|unread| unread.sessions.contains(session_id))
    }
}

/// 读不懂的载荷记一笔丢掉。
fn readable<T: DeserializeOwned>(machine: &RuntimeKey, value: &serde_json::Value) -> Option<T> {
    serde_json::from_value(value.clone())
        .inspect_err(|error| eprintln!("[sessions] {machine:?} 的提醒读不懂：{error}"))
        .ok()
}

#[cfg(test)]
mod tests {
    use serde_json::to_value;
    use smelt_plugin_api::model::daemon_state::{AttentionItem, AttentionKind, SessionAttention};

    use super::*;

    const LOCAL: RuntimeKey = RuntimeKey::Local;

    fn session(id: &str, unread: bool) -> SessionAttention {
        let item = AttentionItem {
            kind: AttentionKind::Success,
            title: "Done".to_string(),
            message: String::new(),
            note: None,
        };
        SessionAttention {
            session_id: id.to_string(),
            action: None,
            unread: unread.then_some(item),
        }
    }

    fn snapshot(revision: u64, sessions: Vec<SessionAttention>) -> TopicMessage {
        TopicMessage::Snapshot(to_value(AttentionSnapshot { revision, sessions }).unwrap())
    }

    fn event(revision: u64, session: SessionAttention) -> TopicMessage {
        TopicMessage::Event(
            to_value(AttentionChanged {
                revision,
                session,
                alert: None,
            })
            .unwrap(),
        )
    }

    #[test]
    fn snapshot_replaces_everything() {
        let mut unread = Unread::default();
        assert!(unread.apply(&LOCAL, &event(1, session("old", true))));
        assert!(unread.apply(&LOCAL, &snapshot(5, vec![session("a", true)])));
        assert!(unread.contains(&LOCAL, "a"));
        assert!(!unread.contains(&LOCAL, "old"), "快照整份换掉");
        assert!(
            !unread.apply(&LOCAL, &snapshot(6, vec![session("a", true)])),
            "内容一样不算变"
        );
    }

    #[test]
    fn events_older_than_the_snapshot_are_dropped() {
        let mut unread = Unread::default();
        unread.apply(&LOCAL, &snapshot(5, vec![]));
        assert!(!unread.apply(&LOCAL, &event(5, session("a", true))));
        assert!(!unread.contains(&LOCAL, "a"));
        assert!(unread.apply(&LOCAL, &event(6, session("a", true))));
        assert!(unread.contains(&LOCAL, "a"));
    }

    /// 看过了（运行时推来不带未读的整份）就不算了；本来就不算的再来一次不算变。
    #[test]
    fn read_or_action_only_clears_unread() {
        let mut unread = Unread::default();
        unread.apply(&LOCAL, &snapshot(1, vec![session("a", true)]));
        assert!(unread.apply(&LOCAL, &event(2, session("a", false))));
        assert!(!unread.contains(&LOCAL, "a"));
        assert!(!unread.apply(&LOCAL, &event(3, session("a", false))));
    }

    #[test]
    fn machines_are_kept_apart_and_forgotten() {
        let remote = RuntimeKey::Remote("m".into());
        let mut unread = Unread::default();
        unread.apply(&LOCAL, &snapshot(1, vec![session("a", true)]));
        unread.apply(&remote, &snapshot(1, vec![session("b", true)]));
        assert!(!unread.contains(&LOCAL, "b"));
        unread.forget(&remote);
        assert!(!unread.contains(&remote, "b"));
        assert!(unread.contains(&LOCAL, "a"));
    }
}
