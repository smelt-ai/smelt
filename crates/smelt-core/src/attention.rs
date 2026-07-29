//! Agent 状态变化产生的“需要告知用户的事”。状态描述当前事实，关注事件描述一次性
//! 消息；两者分开后，铃铛、toast、系统通知和角标只需选择投递渠道，不再各自猜 phase。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::daemon_state::{DaemonPhase, DaemonSessionState};

const DELIVERY_DEDUP: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    Approval,
    Input,
    Success,
    Failure,
    Bell,
    Notice,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryChannel {
    Suppress,
    Toast,
    System,
}

pub fn delivery_channel(
    enabled: bool,
    window_active: bool,
    is_current_view: bool,
    requires_action: bool,
) -> DeliveryChannel {
    if !enabled || (is_current_view && !requires_action) {
        DeliveryChannel::Suppress
    } else if window_active {
        DeliveryChannel::Toast
    } else {
        DeliveryChannel::System
    }
}

impl AttentionKind {
    pub fn requires_action(self) -> bool {
        matches!(self, Self::Approval | Self::Input | Self::Failure)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionItem {
    pub session_id: String,
    pub title: String,
    pub message: String,
    pub kind: AttentionKind,
}

#[derive(Default)]
pub struct AttentionStore {
    current: HashMap<String, AttentionRecord>,
    pending_delivery: Vec<AttentionItem>,
    last_delivery: HashMap<String, (AttentionKind, String, Instant)>,
    next_sequence: u64,
}

struct AttentionRecord {
    item: AttentionItem,
    read: bool,
    sequence: u64,
}

impl AttentionStore {
    /// 记录未读并在需要时排入投递队列。同一会话、类型和正文 60 秒内只投递一次，
    /// 但未读始终更新为最新内容。
    pub fn publish(&mut self, item: AttentionItem, now: Instant) -> bool {
        let should_deliver =
            !self
                .last_delivery
                .get(&item.session_id)
                .is_some_and(|(kind, message, at)| {
                    *kind == item.kind
                        && *message == item.message
                        && now.saturating_duration_since(*at) < DELIVERY_DEDUP
                });
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.current.insert(
            item.session_id.clone(),
            AttentionRecord {
                item: item.clone(),
                read: false,
                sequence,
            },
        );
        if should_deliver {
            self.last_delivery.insert(
                item.session_id.clone(),
                (item.kind, item.message.clone(), now),
            );
            self.pending_delivery.push(item);
        }
        should_deliver
    }

    pub fn mark_read(&mut self, session_id: &str) -> Option<AttentionItem> {
        let record = self.current.get_mut(session_id)?;
        record.read = true;
        Some(record.item.clone())
    }

    /// 当前 phase 已离开等待/失败状态，行动项才算真正解决。完成、响铃等非行动项
    /// 在已读后也可用同一路径清理。
    pub fn resolve(&mut self, session_id: &str) -> Option<AttentionItem> {
        self.current.remove(session_id).map(|record| record.item)
    }

    pub fn remove_session(&mut self, session_id: &str) {
        self.current.remove(session_id);
        self.last_delivery.remove(session_id);
        self.pending_delivery
            .retain(|item| item.session_id != session_id);
    }

    pub fn unread(&self, session_id: &str) -> Option<&AttentionItem> {
        self.current
            .get(session_id)
            .filter(|record| !record.read)
            .map(|record| &record.item)
    }

    pub fn unread_items(&self) -> Vec<AttentionItem> {
        let mut records: Vec<_> = self
            .current
            .values()
            .filter(|record| !record.read)
            .collect();
        records.sort_by_key(|record| record.sequence);
        records
            .into_iter()
            .map(|record| record.item.clone())
            .collect()
    }

    pub fn unread_count(&self) -> usize {
        self.current.values().filter(|record| !record.read).count()
    }

    pub fn unresolved_action_count(&self) -> usize {
        self.current
            .values()
            .filter(|record| record.item.kind.requires_action())
            .count()
    }

    pub fn has_unresolved_action(&self, session_id: &str) -> bool {
        self.current
            .get(session_id)
            .is_some_and(|record| record.item.kind.requires_action())
    }

    pub fn drain_deliveries(&mut self) -> Vec<AttentionItem> {
        std::mem::take(&mut self.pending_delivery)
    }
}

/// 仅在进入一个新的可通知 phase 时产生事件。未启用结构化事件的会话继续交给
/// OSC/BEL fallback，避免同一回合从两条信源各投递一次。
pub fn item_for_daemon_transition(
    previous_phase: Option<DaemonPhase>,
    state: &DaemonSessionState,
) -> Option<AttentionItem> {
    if !state.structured_events || previous_phase == Some(state.phase) {
        return None;
    }

    let kind = match state.phase {
        DaemonPhase::AwaitingApproval => AttentionKind::Approval,
        DaemonPhase::WaitingForUser => AttentionKind::Input,
        DaemonPhase::Succeeded => AttentionKind::Success,
        DaemonPhase::Failed => AttentionKind::Failure,
        DaemonPhase::Thinking
        | DaemonPhase::ExecutingTool
        | DaemonPhase::Idle
        | DaemonPhase::Dead => return None,
    };
    let message = state
        .detail_line()
        .or_else(|| state.title.clone())
        .unwrap_or_else(|| format!("会话 {}", &state.id[..8.min(state.id.len())]));

    Some(AttentionItem {
        session_id: state.id.clone(),
        title: state.phase_label().to_string(),
        message,
        kind,
    })
}

/// 将一次 daemon 更新完整应用到 store：可通知 phase 发布事件；结构化会话离开
/// 等待/失败状态时解决旧行动项。调用者只负责保存最新 phase，不再复制生命周期判断。
pub fn apply_daemon_transition(
    store: &mut AttentionStore,
    previous_phase: Option<DaemonPhase>,
    state: &DaemonSessionState,
    now: Instant,
) -> Option<AttentionItem> {
    if let Some(item) = item_for_daemon_transition(previous_phase, state) {
        store.publish(item.clone(), now);
        return Some(item);
    }
    if state.structured_events
        && matches!(
            state.phase,
            DaemonPhase::Thinking
                | DaemonPhase::ExecutingTool
                | DaemonPhase::Idle
                | DaemonPhase::Dead
        )
    {
        store.resolve(&state.id);
    }
    None
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        AttentionItem, AttentionKind, AttentionStore, DeliveryChannel, apply_daemon_transition,
        delivery_channel, item_for_daemon_transition,
    };
    use crate::daemon_state::{DaemonPhase, DaemonSessionState};

    fn state(phase: DaemonPhase) -> DaemonSessionState {
        DaemonSessionState {
            id: "session-12345678".into(),
            phase,
            structured_events: true,
            ..Default::default()
        }
    }

    #[test]
    fn transition_emits_one_typed_item() {
        let mut current = state(DaemonPhase::AwaitingApproval);
        current.pending_question = Some("允许执行？".into());
        let item = item_for_daemon_transition(Some(DaemonPhase::ExecutingTool), &current).unwrap();
        assert_eq!(item.kind, AttentionKind::Approval);
        assert_eq!(item.message, "⚠ 允许执行？");
        assert!(item.kind.requires_action());
        assert!(item_for_daemon_transition(Some(current.phase), &current).is_none());
    }

    #[test]
    fn transition_maps_input_success_and_failure() {
        let mut input = state(DaemonPhase::WaitingForUser);
        input.pending_question = Some("Pick one".into());
        let item = item_for_daemon_transition(Some(DaemonPhase::Thinking), &input).unwrap();
        assert_eq!(item.kind, AttentionKind::Input);
        assert_eq!(item.message, "💬 Pick one");

        let success = state(DaemonPhase::Succeeded);
        let item = item_for_daemon_transition(Some(DaemonPhase::Thinking), &success).unwrap();
        assert_eq!(item.kind, AttentionKind::Success);
        assert_eq!(item.message, "会话 session-");
        assert!(!item.kind.requires_action());

        let mut failure = state(DaemonPhase::Failed);
        failure.pending_question = Some("rate limited".into());
        let item = item_for_daemon_transition(Some(DaemonPhase::Thinking), &failure).unwrap();
        assert_eq!(item.kind, AttentionKind::Failure);
        assert_eq!(item.message, "rate limited");
    }

    #[test]
    fn fallback_sessions_and_non_attention_phases_do_not_emit() {
        let mut current = state(DaemonPhase::Succeeded);
        current.structured_events = false;
        assert!(item_for_daemon_transition(Some(DaemonPhase::Thinking), &current).is_none());

        current.structured_events = true;
        current.phase = DaemonPhase::Thinking;
        assert!(item_for_daemon_transition(Some(DaemonPhase::Idle), &current).is_none());
    }

    fn item(session_id: &str, kind: AttentionKind, message: &str) -> AttentionItem {
        AttentionItem {
            session_id: session_id.into(),
            title: "状态".into(),
            message: message.into(),
            kind,
        }
    }

    #[test]
    fn store_tracks_latest_unread_per_session() {
        let now = Instant::now();
        let mut store = AttentionStore::default();
        store.publish(item("a", AttentionKind::Input, "first"), now);
        store.publish(item("a", AttentionKind::Success, "done"), now);
        store.publish(item("b", AttentionKind::Bell, "bell"), now);

        assert_eq!(store.unread_count(), 2);
        assert_eq!(store.unread("a").unwrap().message, "done");
        assert_eq!(store.unread("a").unwrap().kind, AttentionKind::Success);
        assert_eq!(store.drain_deliveries().len(), 3);
        assert!(store.drain_deliveries().is_empty());
    }

    #[test]
    fn store_deduplicates_delivery_without_losing_unread() {
        let now = Instant::now();
        let mut store = AttentionStore::default();
        assert!(store.publish(item("a", AttentionKind::Input, "pick"), now));
        assert!(!store.publish(
            item("a", AttentionKind::Input, "pick"),
            now + Duration::from_secs(59)
        ));
        assert_eq!(store.unread("a").unwrap().message, "pick");
        assert_eq!(store.drain_deliveries().len(), 1);
        assert!(store.publish(
            item("a", AttentionKind::Input, "pick"),
            now + Duration::from_secs(60)
        ));
        assert_eq!(store.drain_deliveries().len(), 1);
    }

    #[test]
    fn mark_read_and_remove_session_have_distinct_lifecycles() {
        let now = Instant::now();
        let mut store = AttentionStore::default();
        store.publish(item("a", AttentionKind::Success, "done"), now);
        assert!(store.mark_read("a").is_some());
        assert!(store.unread("a").is_none());
        assert_eq!(store.unresolved_action_count(), 0);

        // 已读不重置投递去重，短时间重复事件不会再次打扰。
        assert!(!store.publish(
            item("a", AttentionKind::Success, "done"),
            now + Duration::from_secs(1)
        ));
        store.remove_session("a");
        assert!(store.publish(
            item("a", AttentionKind::Success, "done"),
            now + Duration::from_secs(2)
        ));
    }

    #[test]
    fn reading_action_keeps_it_unresolved_until_phase_moves_on() {
        let now = Instant::now();
        let mut store = AttentionStore::default();
        store.publish(item("a", AttentionKind::Approval, "allow?"), now);
        assert_eq!(store.unresolved_action_count(), 1);
        store.mark_read("a");
        assert!(store.unread("a").is_none());
        assert_eq!(store.unresolved_action_count(), 1);
        store.resolve("a");
        assert_eq!(store.unresolved_action_count(), 0);
    }

    #[test]
    fn unread_items_keep_publish_order_after_replacement() {
        let now = Instant::now();
        let mut store = AttentionStore::default();
        store.publish(item("a", AttentionKind::Notice, "old"), now);
        store.publish(item("b", AttentionKind::Notice, "middle"), now);
        store.publish(item("a", AttentionKind::Success, "latest"), now);
        let messages: Vec<_> = store
            .unread_items()
            .into_iter()
            .map(|item| item.message)
            .collect();
        assert_eq!(messages, ["middle", "latest"]);
    }

    #[test]
    fn delivery_policy_covers_focus_background_and_settings() {
        assert_eq!(
            delivery_channel(true, true, true, false),
            DeliveryChannel::Suppress
        );
        assert_eq!(
            delivery_channel(false, true, false, true),
            DeliveryChannel::Suppress
        );
        assert_eq!(
            delivery_channel(true, true, true, true),
            DeliveryChannel::Toast
        );
        assert_eq!(
            delivery_channel(true, true, false, false),
            DeliveryChannel::Toast
        );
        assert_eq!(
            delivery_channel(true, false, false, false),
            DeliveryChannel::System
        );
    }

    #[test]
    fn daemon_lifecycle_keeps_read_action_until_phase_resolves_it() {
        let now = Instant::now();
        let mut store = AttentionStore::default();
        let waiting = state(DaemonPhase::AwaitingApproval);
        apply_daemon_transition(&mut store, Some(DaemonPhase::ExecutingTool), &waiting, now);
        store.mark_read(&waiting.id);
        assert_eq!(store.unresolved_action_count(), 1);

        let running = state(DaemonPhase::Thinking);
        apply_daemon_transition(
            &mut store,
            Some(DaemonPhase::AwaitingApproval),
            &running,
            now + Duration::from_secs(1),
        );
        assert_eq!(store.unresolved_action_count(), 0);
        assert!(store.unread(&running.id).is_none());
    }
}
