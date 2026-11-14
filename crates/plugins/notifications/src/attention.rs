//! 各台机器的会话提醒镜像：运行时（smeltd）是提醒的唯一权威，这里只照它推来的
//! `attention.changed` 留一份，不自己判定。
//!
//! 订上（含重连）时运行时先发整份快照，之后逐条发变化，每条带递增的 `revision`。快照整份
//! 换掉；比手里旧的变化（快照前排着的）丢掉。断开、积压只是提示，后面一定跟着快照。

use std::collections::HashMap;

use api::model::daemon_state::{
    AttentionChanged, AttentionItem, AttentionSnapshot, SessionAttention,
};
use api::{RuntimeKey, TopicMessage};
use smelt_plugin_api as api;

#[derive(Default)]
pub(crate) struct Mirror {
    machines: HashMap<RuntimeKey, MachineAttention>,
}

#[derive(Default)]
struct MachineAttention {
    /// 收到过快照：这台机器上没在镜像里的会话确实没有提醒。
    primed: bool,
    revision: u64,
    sessions: HashMap<String, SessionAttention>,
}

/// 一条推送带来的变化。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Applied {
    /// 什么都没变：旧的、认不出的，或只是断开提示。
    Nothing,
    /// 整份换了。
    Replaced,
    /// 一段会话变了；`alert` 是刚来、该响一下的那条。
    Changed {
        session: SessionAttention,
        alert: Option<AttentionItem>,
    },
}

impl Mirror {
    pub(crate) fn apply(&mut self, machine: &RuntimeKey, message: &TopicMessage) -> Applied {
        match message {
            TopicMessage::Snapshot(value) => {
                let Some(snapshot) = readable::<AttentionSnapshot>(
                    machine,
                    api::serde_json::from_value(value.clone()),
                ) else {
                    return Applied::Nothing;
                };
                self.machines.insert(
                    machine.clone(),
                    MachineAttention {
                        primed: true,
                        revision: snapshot.revision,
                        sessions: snapshot
                            .sessions
                            .into_iter()
                            .filter(|session| !session.is_empty())
                            .map(|session| (session.session_id.clone(), session))
                            .collect(),
                    },
                );
                Applied::Replaced
            }
            TopicMessage::Event(value) => {
                let Some(changed) = readable::<AttentionChanged>(
                    machine,
                    api::serde_json::from_value(value.clone()),
                ) else {
                    return Applied::Nothing;
                };
                let entry = self.machines.entry(machine.clone()).or_default();
                if entry.primed && changed.revision <= entry.revision {
                    return Applied::Nothing;
                }
                entry.revision = changed.revision;
                let session = changed.session;
                if session.is_empty() {
                    entry.sessions.remove(&session.session_id);
                } else {
                    entry
                        .sessions
                        .insert(session.session_id.clone(), session.clone());
                }
                Applied::Changed {
                    session,
                    alert: changed.alert,
                }
            }
            TopicMessage::Lagged => Applied::Nothing,
        }
    }

    /// 只留 `keep` 认的机器（移除了的机器提醒跟着没了）。去掉了哪台就是 `true`。
    pub(crate) fn retain_machines(&mut self, keep: impl Fn(&RuntimeKey) -> bool) -> bool {
        let before = self.machines.len();
        self.machines.retain(|machine, _| keep(machine));
        self.machines.len() != before
    }

    /// 这台机器的提醒确定了：收到过快照。没确定前不能拿「镜像里没有」当「看过了」。
    pub(crate) fn primed(&self, machine: &RuntimeKey) -> bool {
        self.machines
            .get(machine)
            .is_some_and(|attention| attention.primed)
    }

    pub(crate) fn has_unread(&self, machine: &RuntimeKey, session_id: &str) -> bool {
        self.machines
            .get(machine)
            .and_then(|attention| attention.sessions.get(session_id))
            .is_some_and(|session| session.unread.is_some())
    }

    /// 要你看的会话，所有机器的：会话 id 到还等不等你处理。
    pub(crate) fn by_session(&self) -> HashMap<String, bool> {
        self.machines
            .values()
            .flat_map(|attention| attention.sessions.values())
            .map(|session| (session.session_id.clone(), session.action.is_some()))
            .collect()
    }
}

/// 读不懂的载荷记一笔丢掉。插件不直接依赖 serde，所以按类型各写一个。
fn readable<T>(machine: &RuntimeKey, decoded: api::serde_json::Result<T>) -> Option<T> {
    decoded
        .inspect_err(|error| eprintln!("[notifications] {machine:?} 的提醒读不懂：{error}"))
        .ok()
}

#[cfg(test)]
mod tests {
    use api::model::daemon_state::AttentionKind;
    use api::serde_json::to_value;

    use super::*;

    fn item(kind: AttentionKind) -> AttentionItem {
        AttentionItem {
            kind,
            title: "等待批准".to_string(),
            message: "rm -rf build".to_string(),
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

    const LOCAL: RuntimeKey = RuntimeKey::Local;

    #[test]
    fn snapshot_replaces_and_primes_the_machine() {
        let mut mirror = Mirror::default();
        assert!(!mirror.primed(&LOCAL));
        mirror.apply(&LOCAL, &event(1, unread("old"), false));
        assert!(!mirror.primed(&LOCAL), "没收到快照前不算确定");

        assert_eq!(
            mirror.apply(&LOCAL, &snapshot(5, vec![unread("a")])),
            Applied::Replaced
        );
        assert!(mirror.primed(&LOCAL));
        assert!(mirror.has_unread(&LOCAL, "a"));
        assert!(!mirror.has_unread(&LOCAL, "old"), "快照整份换掉");
    }

    /// 快照前排着的变化比快照旧，丢掉；之后的照常记。
    #[test]
    fn events_older_than_the_snapshot_are_dropped() {
        let mut mirror = Mirror::default();
        mirror.apply(&LOCAL, &snapshot(5, vec![]));
        assert_eq!(
            mirror.apply(&LOCAL, &event(5, unread("a"), true)),
            Applied::Nothing
        );
        assert!(!mirror.has_unread(&LOCAL, "a"));

        let applied = mirror.apply(&LOCAL, &event(6, unread("a"), true));
        assert!(matches!(applied, Applied::Changed { alert: Some(_), .. }));
        assert!(mirror.has_unread(&LOCAL, "a"));
    }

    #[test]
    fn an_empty_session_leaves_the_mirror() {
        let mut mirror = Mirror::default();
        mirror.apply(&LOCAL, &snapshot(1, vec![unread("a")]));
        mirror.apply(
            &LOCAL,
            &event(
                2,
                SessionAttention {
                    session_id: "a".to_string(),
                    ..Default::default()
                },
                false,
            ),
        );
        assert!(mirror.by_session().is_empty());
    }

    /// 运行时重启后 revision 从头数：重连带来的快照照样整份换上，之后的变化照常记。
    #[test]
    fn a_restarted_runtime_starts_over_from_its_snapshot() {
        let mut mirror = Mirror::default();
        mirror.apply(&LOCAL, &snapshot(40, vec![unread("a")]));
        assert_eq!(
            mirror.apply(&LOCAL, &TopicMessage::Lagged),
            Applied::Nothing
        );
        mirror.apply(&LOCAL, &snapshot(0, vec![]));
        assert!(!mirror.has_unread(&LOCAL, "a"));
        mirror.apply(&LOCAL, &event(1, unread("b"), true));
        assert!(mirror.has_unread(&LOCAL, "b"));
    }

    /// 同一个会话 id 在两台机器上各是各的；合起来给菜单栏时按「等不等你处理」标。
    #[test]
    fn machines_are_kept_apart() {
        let remote = RuntimeKey::Remote("rt".to_string());
        let mut mirror = Mirror::default();
        mirror.apply(&LOCAL, &snapshot(1, vec![unread("a")]));
        mirror.apply(
            &remote,
            &snapshot(
                1,
                vec![SessionAttention {
                    session_id: "b".to_string(),
                    action: Some(item(AttentionKind::Approval)),
                    unread: None,
                }],
            ),
        );
        assert!(!mirror.has_unread(&remote, "a"));
        assert_eq!(
            mirror.by_session(),
            HashMap::from([("a".to_string(), false), ("b".to_string(), true)])
        );

        assert!(mirror.retain_machines(|machine| *machine == LOCAL));
        assert!(!mirror.primed(&remote));
        assert!(!mirror.retain_machines(|machine| *machine == LOCAL));
    }

    #[test]
    fn unreadable_payloads_change_nothing() {
        let mut mirror = Mirror::default();
        assert_eq!(
            mirror.apply(&LOCAL, &TopicMessage::Event(api::serde_json::json!(42))),
            Applied::Nothing
        );
        assert!(!mirror.primed(&LOCAL));
    }
}
