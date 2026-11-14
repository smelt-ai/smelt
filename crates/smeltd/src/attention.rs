//! 提醒：哪段会话有事要告诉用户、用户看过没有。
//!
//! 运行时是唯一算提醒的地方，桌面、手机、推送都读这一份（`attention.changed`），看过的状态在
//! 各设备间同步：哪台设备上看过、在终端里打字、在对话里回复，都清掉同一条未读。
//!
//! 每段会话最多两样（[`SessionAttention`]）：
//! - `action`：等批准、等输入、失败。看过也留着，相位往下走了才清。
//! - `unread`：最近一条没看过的提醒，相位提醒、终端通知、响铃、智能体消息都算。
//!
//! 相位提醒只认回合级结构化事件（hook / ACP）给的相位，进入等批准、等输入、完成、失败的那一刻
//! 算一次；同一相位里换了一个审批请求也算。离开这些相位时清掉 `action` 和相位类的未读：新一轮
//! 开始了，上一轮的「完成」已经过时。终端通知和响铃只在没有结构化事件的会话上记（有 hook 的
//! 智能体自己也会响铃、发 OSC，只会重复）。
//!
//! 自动化后台跑的会话不在侧栏里，没处可看：完成、失败归自动化自己的运行结果通知，这里不记；
//! 它们的终端通知只响一下（[`AttentionChanged::alert`]），不留未读。命令行发的消息没有会话，
//! 同样只响。
//!
//! 每次变化带一条 `alert`（该弹系统通知、该推手机的那条），同一段会话同样的提醒 60 秒内只响
//! 一次；相位离开后指纹清掉，下一轮同样的文字照常响。
//!
//! 只在内存里。一段会话第一次出现时只建基线：运行时换代接回来的会话已经在等批准，记成看过的
//! 待办，不重新响。

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use smelt_core::daemon_state::{DaemonPhase, DaemonSessionState};
use smelt_core::session_control::is_background_acp_session_id;
use smelt_event_protocol::{
    AttentionChanged, AttentionItem, AttentionKind, AttentionSnapshot, SessionAttention,
};
use smelt_push::PushKind;

use crate::push::{self, Content};

/// 同一段会话同样的提醒（类型 + 正文）多久内只响一次。
const ALERT_DEDUP: Duration = Duration::from_secs(60);

/// 终端程序发的通知、智能体消息没给标题时用的。
pub(crate) const NOTICE_TITLE: &str = "终端通知";
pub(crate) const BELL_TITLE: &str = "终端响铃";

#[derive(Default)]
pub(crate) struct AttentionLedger {
    revision: u64,
    /// 每段会话上一次看到的状态：比相位边沿、给推送取会话名。
    sessions: HashMap<String, DaemonSessionState>,
    /// 只放有提醒的会话。
    current: BTreeMap<String, SessionAttention>,
    /// 每段会话上一次响的提醒：类型、正文、时刻。
    last_alert: HashMap<String, (AttentionKind, String, Instant)>,
}

/// 一次变化要往外送的：广播给订阅者的、交给推送的。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Outcome {
    pub(crate) changed: Option<AttentionChanged>,
    pub(crate) push: Option<push::Command>,
}

impl AttentionLedger {
    pub(crate) fn snapshot(&self) -> AttentionSnapshot {
        AttentionSnapshot {
            revision: self.revision,
            sessions: self.current.values().cloned().collect(),
        }
    }

    /// 会话状态变了（运行时刚把它广播出去）。
    pub(crate) fn observe(&mut self, state: DaemonSessionState, now: Instant) -> Outcome {
        let id = state.id.clone();
        let before = self.attention_of(&id);
        let mut after = before.clone();
        let mut alert = None;
        let previous = self.sessions.get(&id).cloned();
        if !state.has_runtime() {
            // 运行时没了的会话（崩溃留下的、被归档的）没法再处理。
            after = empty(&id);
            self.last_alert.remove(&id);
        } else if let Some(previous) = previous {
            match phase_edge(&previous, &state) {
                Some(Edge::Enter(item)) => {
                    after.action = item.kind.requires_action().then(|| item.clone());
                    after.unread = Some(item.clone());
                    alert = self.alert(&id, item, now);
                }
                Some(Edge::Leave) => {
                    after.action = None;
                    if after
                        .unread
                        .as_ref()
                        .is_some_and(|item| is_phase_kind(item.kind))
                    {
                        after.unread = None;
                    }
                    self.last_alert.remove(&id);
                }
                None => {}
            }
        } else if let Some(item) = phase_item(&state).filter(|item| item.kind.requires_action()) {
            after.action = Some(item);
        }
        let push = match &alert {
            Some(item) => push_kind(item.kind).map(|kind| push::Command::Notify {
                session_id: id.clone(),
                kind,
                content: Content::of(&state),
            }),
            None if before.action.is_some() && after.action.is_none() => {
                Some(push::Command::Cancel {
                    session_id: id.clone(),
                })
            }
            None => None,
        };
        self.sessions.insert(id, state);
        Outcome {
            changed: self.commit(before, after, alert),
            push,
        }
    }

    /// 会话删掉了。
    pub(crate) fn remove(&mut self, session_id: &str) -> Outcome {
        self.sessions.remove(session_id);
        self.last_alert.remove(session_id);
        let before = self.attention_of(session_id);
        Outcome {
            changed: self.commit(before, empty(session_id), None),
            push: Some(push::Command::Cancel {
                session_id: session_id.to_string(),
            }),
        }
    }

    /// 终端程序的通知、响铃、智能体发来的消息。`session_id` 为空是命令行发的。
    pub(crate) fn notice(
        &mut self,
        session_id: &str,
        kind: AttentionKind,
        title: &str,
        message: &str,
        now: Instant,
    ) -> Outcome {
        let item = AttentionItem {
            kind,
            title: title.to_string(),
            message: message.to_string(),
        };
        let before = self.attention_of(session_id);
        let mut after = before.clone();
        // 没处可看的只响不记：记了也没人能把它标成看过。
        if self.sessions.contains_key(session_id) && !is_background_acp_session_id(session_id) {
            after.unread = Some(item.clone());
        }
        let alert = self.alert(session_id, item, now);
        Outcome {
            changed: self.commit(before, after, alert),
            push: None,
        }
    }

    /// 用户看过这段会话了：哪台设备上打开着它、在终端里打字、在对话里回复。
    pub(crate) fn mark_read(&mut self, session_id: &str) -> Outcome {
        let before = self.attention_of(session_id);
        let mut after = before.clone();
        after.unread = None;
        Outcome {
            changed: self.commit(before, after, None),
            push: None,
        }
    }

    fn attention_of(&self, session_id: &str) -> SessionAttention {
        self.current
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| empty(session_id))
    }

    /// 这条提醒该不该响：同一段会话同样的提醒 60 秒内只响一次。
    fn alert(
        &mut self,
        session_id: &str,
        item: AttentionItem,
        now: Instant,
    ) -> Option<AttentionItem> {
        let repeated = self
            .last_alert
            .get(session_id)
            .is_some_and(|(kind, message, at)| {
                *kind == item.kind
                    && *message == item.message
                    && now.saturating_duration_since(*at) < ALERT_DEDUP
            });
        if repeated {
            return None;
        }
        self.last_alert.insert(
            session_id.to_string(),
            (item.kind, item.message.clone(), now),
        );
        Some(item)
    }

    /// 记下新状态。没变也没响就什么都不发。
    fn commit(
        &mut self,
        before: SessionAttention,
        after: SessionAttention,
        alert: Option<AttentionItem>,
    ) -> Option<AttentionChanged> {
        if before == after && alert.is_none() {
            return None;
        }
        if after.is_empty() {
            self.current.remove(&after.session_id);
        } else {
            self.current.insert(after.session_id.clone(), after.clone());
        }
        self.revision = self.revision.saturating_add(1);
        Some(AttentionChanged {
            revision: self.revision,
            session: after,
            alert,
        })
    }
}

fn empty(session_id: &str) -> SessionAttention {
    SessionAttention {
        session_id: session_id.to_string(),
        ..Default::default()
    }
}

/// 由相位产生的提醒：新一轮开始时一起作废。
fn is_phase_kind(kind: AttentionKind) -> bool {
    matches!(
        kind,
        AttentionKind::Approval
            | AttentionKind::Input
            | AttentionKind::Success
            | AttentionKind::Failure
    )
}

enum Edge {
    /// 进入了一个要告诉用户的相位。
    Enter(AttentionItem),
    /// 离开了：开始干活、空闲、结束，或者是不归这里提醒的相位。
    Leave,
}

/// 相位有没有翻篇。只看回合级结构化事件给的相位：标题、OSC 猜的不算，也不清旧提醒。
fn phase_edge(previous: &DaemonSessionState, state: &DaemonSessionState) -> Option<Edge> {
    if !state.structured_events || !state.phase_is_authoritative() {
        return None;
    }
    let same_phase = previous.phase == state.phase
        && previous.structured_events
        && previous.phase_is_authoritative();
    // 对话会话连着两个审批请求时相位不变，换的是请求。
    let new_request = matches!(
        state.phase,
        DaemonPhase::AwaitingApproval | DaemonPhase::WaitingForUser
    ) && state.pending_request.is_some()
        && state.pending_request != previous.pending_request;
    if same_phase && !new_request {
        return None;
    }
    Some(match phase_item(state) {
        Some(item) => Edge::Enter(item),
        None => Edge::Leave,
    })
}

/// 这个相位要告诉用户什么。
fn phase_item(state: &DaemonSessionState) -> Option<AttentionItem> {
    if !state.structured_events || !state.phase_is_authoritative() {
        return None;
    }
    let kind = match state.phase {
        DaemonPhase::AwaitingApproval => AttentionKind::Approval,
        DaemonPhase::WaitingForUser => AttentionKind::Input,
        DaemonPhase::Succeeded => AttentionKind::Success,
        DaemonPhase::Failed => AttentionKind::Failure,
        DaemonPhase::Thinking
        | DaemonPhase::ExecutingTool
        | DaemonPhase::Connecting
        | DaemonPhase::Idle
        | DaemonPhase::Dead => return None,
    };
    // 自动化运行的结果由自动化自己的通知报。
    if matches!(kind, AttentionKind::Success | AttentionKind::Failure)
        && is_background_acp_session_id(&state.id)
    {
        return None;
    }
    let message = state
        .detail_line()
        .or_else(|| state.title.clone())
        .unwrap_or_else(|| format!("会话 {}", &state.id[..8.min(state.id.len())]));
    Some(AttentionItem {
        kind,
        title: state.phase_label().to_string(),
        message,
    })
}

/// 推给手机的四种。终端通知、响铃不推；智能体消息由 `crate::agent_notify` 直接推。
fn push_kind(kind: AttentionKind) -> Option<PushKind> {
    match kind {
        AttentionKind::Approval => Some(PushKind::Approval),
        AttentionKind::Input => Some(PushKind::Input),
        AttentionKind::Failure => Some(PushKind::Failure),
        AttentionKind::Success => Some(PushKind::Success),
        AttentionKind::Bell | AttentionKind::Notice | AttentionKind::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(id: &str, phase: DaemonPhase) -> DaemonSessionState {
        DaemonSessionState {
            id: id.into(),
            phase,
            structured_events: true,
            turn_events: true,
            ..Default::default()
        }
    }

    fn item(kind: AttentionKind, title: &str, message: &str) -> AttentionItem {
        AttentionItem {
            kind,
            title: title.into(),
            message: message.into(),
        }
    }

    fn seen(ledger: &mut AttentionLedger, id: &str, phase: DaemonPhase, now: Instant) {
        ledger.observe(state(id, phase), now);
    }

    fn current(ledger: &AttentionLedger, id: &str) -> SessionAttention {
        ledger.attention_of(id)
    }

    /// 第一次见到的会话只建基线：已经在等批准的记成看过的待办，不响、不推。
    #[test]
    fn first_sighting_is_a_baseline() {
        let mut ledger = AttentionLedger::default();
        let outcome = ledger.observe(state("a", DaemonPhase::AwaitingApproval), Instant::now());
        let changed = outcome.changed.expect("基线里的待办要进快照");
        assert_eq!(changed.alert, None);
        assert_eq!(changed.session.unread, None);
        assert_eq!(
            changed.session.action.map(|item| item.kind),
            Some(AttentionKind::Approval)
        );
        assert_eq!(outcome.push, None);

        let outcome = ledger.observe(state("b", DaemonPhase::Succeeded), Instant::now());
        assert_eq!(outcome, Outcome::default(), "启动前就完成的不算新提醒");
    }

    /// 进入等批准：响、推、记成待办和未读；看过后待办还在；开始干活后都清掉并撤推送。
    #[test]
    fn approval_stays_until_the_phase_moves_on() {
        let mut ledger = AttentionLedger::default();
        let now = Instant::now();
        seen(&mut ledger, "a", DaemonPhase::Thinking, now);

        let mut waiting = state("a", DaemonPhase::AwaitingApproval);
        waiting.pending_question = Some("rm -rf build".into());
        let outcome = ledger.observe(waiting, now);
        let expected = item(AttentionKind::Approval, "等你批准", "⚠ rm -rf build");
        let changed = outcome.changed.unwrap();
        assert_eq!(changed.alert.as_ref(), Some(&expected));
        assert_eq!(changed.session.action.as_ref(), Some(&expected));
        assert_eq!(changed.session.unread.as_ref(), Some(&expected));
        assert!(matches!(
            outcome.push,
            Some(push::Command::Notify {
                kind: PushKind::Approval,
                ..
            })
        ));

        let changed = ledger.mark_read("a").changed.unwrap();
        assert_eq!(changed.session.unread, None);
        assert_eq!(changed.session.action.as_ref(), Some(&expected));
        assert_eq!(ledger.mark_read("a").changed, None, "已经看过就不再发");

        let outcome = ledger.observe(state("a", DaemonPhase::ExecutingTool), now);
        let changed = outcome.changed.clone().unwrap();
        assert!(changed.session.is_empty(), "{changed:?}");
        assert_eq!(
            outcome.push,
            Some(push::Command::Cancel {
                session_id: "a".into()
            })
        );
        assert_eq!(ledger.snapshot().sessions, vec![]);
    }

    /// 完成只是未读：看过就没了。新一轮开始时没看的「完成」也作废。
    #[test]
    fn success_is_unread_until_seen_or_a_new_turn() {
        let mut ledger = AttentionLedger::default();
        let now = Instant::now();
        seen(&mut ledger, "a", DaemonPhase::Thinking, now);
        let outcome = ledger.observe(state("a", DaemonPhase::Succeeded), now);
        let changed = outcome.changed.unwrap();
        assert_eq!(changed.session.action, None);
        assert_eq!(
            changed.session.unread.map(|item| item.kind),
            Some(AttentionKind::Success)
        );
        assert!(matches!(
            outcome.push,
            Some(push::Command::Notify {
                kind: PushKind::Success,
                ..
            })
        ));

        let outcome = ledger.observe(state("a", DaemonPhase::Thinking), now);
        let changed = outcome.changed.clone().unwrap();
        assert!(changed.session.is_empty(), "{changed:?}");
        assert_eq!(outcome.push, None, "没有待办就没有要撤的推送");
    }

    /// 同一相位里换了一个审批请求也算新提醒；同一个请求重复广播不算。
    #[test]
    fn a_new_request_in_the_same_phase_alerts_again() {
        let mut ledger = AttentionLedger::default();
        let now = Instant::now();
        seen(&mut ledger, "a", DaemonPhase::Thinking, now);
        let mut first = state("a", DaemonPhase::AwaitingApproval);
        first.pending_request = Some("req-1".into());
        first.pending_question = Some("ls".into());
        assert!(
            ledger
                .observe(first.clone(), now)
                .changed
                .unwrap()
                .alert
                .is_some()
        );
        assert_eq!(ledger.observe(first, now), Outcome::default());

        let mut second = state("a", DaemonPhase::AwaitingApproval);
        second.pending_request = Some("req-2".into());
        second.pending_question = Some("cat".into());
        let changed = ledger.observe(second, now).changed.unwrap();
        assert_eq!(changed.alert.unwrap().message, "⚠ cat");
    }

    /// 同样的提醒 60 秒内只响一次，但未读照样更新；相位离开后下一轮照常响。
    #[test]
    fn repeated_alerts_are_deduplicated_until_the_phase_moves_on() {
        let mut ledger = AttentionLedger::default();
        let now = Instant::now();
        seen(&mut ledger, "a", DaemonPhase::Idle, now);
        let first = ledger
            .notice("a", AttentionKind::Notice, NOTICE_TITLE, "构建完成", now)
            .changed
            .unwrap();
        assert!(first.alert.is_some());
        ledger.mark_read("a");
        let again = ledger
            .notice("a", AttentionKind::Notice, NOTICE_TITLE, "构建完成", now)
            .changed
            .unwrap();
        assert_eq!(again.alert, None);
        assert!(again.session.unread.is_some(), "没响也要记成未读");
        let later = ledger
            .notice(
                "a",
                AttentionKind::Notice,
                NOTICE_TITLE,
                "构建完成",
                now + ALERT_DEDUP,
            )
            .changed
            .unwrap();
        assert!(later.alert.is_some());

        seen(&mut ledger, "b", DaemonPhase::Thinking, now);
        assert!(
            ledger
                .observe(state("b", DaemonPhase::Succeeded), now)
                .changed
                .unwrap()
                .alert
                .is_some()
        );
        seen(&mut ledger, "b", DaemonPhase::Thinking, now);
        assert!(
            ledger
                .observe(state("b", DaemonPhase::Succeeded), now)
                .changed
                .unwrap()
                .alert
                .is_some(),
            "新一轮同样的完成照常响"
        );
    }

    /// 终端通知不清待办，相位离开时也不跟着作废：它不是这一轮的结果。
    #[test]
    fn notices_are_independent_of_phase_attention() {
        let mut ledger = AttentionLedger::default();
        let now = Instant::now();
        seen(&mut ledger, "a", DaemonPhase::Thinking, now);
        ledger.observe(state("a", DaemonPhase::WaitingForUser), now);
        let changed = ledger
            .notice("a", AttentionKind::Bell, BELL_TITLE, "", now)
            .changed
            .unwrap();
        assert_eq!(
            changed.session.action.map(|item| item.kind),
            Some(AttentionKind::Input),
            "通知不能盖掉还没处理的待办"
        );
        assert_eq!(
            changed.session.unread.map(|item| item.kind),
            Some(AttentionKind::Bell)
        );

        let changed = ledger
            .observe(state("a", DaemonPhase::Thinking), now)
            .changed
            .unwrap();
        assert_eq!(changed.session.action, None);
        assert_eq!(
            changed.session.unread.map(|item| item.kind),
            Some(AttentionKind::Bell)
        );
    }

    /// 命令行发的、自动化会话的、还没见过的会话的通知只响不记。
    #[test]
    fn notices_without_a_visible_session_only_alert() {
        let mut ledger = AttentionLedger::default();
        let now = Instant::now();
        seen(&mut ledger, "acp-automation-1", DaemonPhase::Thinking, now);
        for session in ["", "acp-automation-1", "unknown"] {
            let changed = ledger
                .notice(session, AttentionKind::Notice, "智能体消息", "好了", now)
                .changed
                .unwrap();
            assert!(changed.alert.is_some(), "{session}");
            assert!(changed.session.is_empty(), "{session}");
        }
        assert_eq!(ledger.snapshot().sessions, vec![]);
    }

    /// 自动化会话的完成、失败不记，等批准照常记。
    #[test]
    fn automation_results_are_left_to_the_automation() {
        let mut ledger = AttentionLedger::default();
        let now = Instant::now();
        let id = "acp-automation-1";
        seen(&mut ledger, id, DaemonPhase::Thinking, now);
        assert_eq!(
            ledger.observe(state(id, DaemonPhase::Succeeded), now),
            Outcome::default()
        );
        let outcome = ledger.observe(state(id, DaemonPhase::AwaitingApproval), now);
        assert!(outcome.changed.unwrap().session.action.is_some());
        let outcome = ledger.observe(state(id, DaemonPhase::Failed), now);
        let changed = outcome.changed.clone().unwrap();
        assert!(changed.session.is_empty(), "{changed:?}");
        assert!(matches!(outcome.push, Some(push::Command::Cancel { .. })));
    }

    /// 没有结构化事件的相位（标题、OSC 猜的）不提醒，也不清掉已有的提醒。
    #[test]
    fn guessed_phases_are_ignored() {
        let mut ledger = AttentionLedger::default();
        let now = Instant::now();
        let mut guessed = state("a", DaemonPhase::Idle);
        guessed.turn_events = false;
        seen(&mut ledger, "a", DaemonPhase::Thinking, now);
        ledger.observe(state("a", DaemonPhase::WaitingForUser), now);
        guessed.phase = DaemonPhase::Succeeded;
        assert_eq!(ledger.observe(guessed, now), Outcome::default());
        assert!(current(&ledger, "a").action.is_some());
    }

    /// 运行时没了、会话删了：清掉并撤推送。
    #[test]
    fn lost_runtime_and_removal_clear_everything() {
        let mut ledger = AttentionLedger::default();
        let now = Instant::now();
        seen(&mut ledger, "a", DaemonPhase::Thinking, now);
        ledger.observe(state("a", DaemonPhase::AwaitingApproval), now);
        let mut ghost = state("a", DaemonPhase::AwaitingApproval);
        ghost.runtime = false;
        let outcome = ledger.observe(ghost, now);
        let changed = outcome.changed.clone().unwrap();
        assert!(changed.session.is_empty(), "{changed:?}");
        assert!(matches!(outcome.push, Some(push::Command::Cancel { .. })));

        seen(&mut ledger, "b", DaemonPhase::Thinking, now);
        ledger.observe(state("b", DaemonPhase::Failed), now);
        let outcome = ledger.remove("b");
        let changed = outcome.changed.clone().unwrap();
        assert!(changed.session.is_empty(), "{changed:?}");
        assert_eq!(ledger.remove("b").changed, None);
        assert_eq!(ledger.snapshot().sessions, vec![]);
    }

    /// 快照只放有提醒的会话，版本号随每次变化递增。
    #[test]
    fn snapshot_lists_sessions_with_attention() {
        let mut ledger = AttentionLedger::default();
        let now = Instant::now();
        seen(&mut ledger, "a", DaemonPhase::Thinking, now);
        seen(&mut ledger, "b", DaemonPhase::Thinking, now);
        let first = ledger.observe(state("b", DaemonPhase::Failed), now);
        let second = ledger.observe(state("a", DaemonPhase::Succeeded), now);
        assert!(second.changed.unwrap().revision > first.changed.unwrap().revision);
        let snapshot = ledger.snapshot();
        assert_eq!(snapshot.revision, 2);
        assert_eq!(
            snapshot
                .sessions
                .iter()
                .map(|session| session.session_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
    }
}
