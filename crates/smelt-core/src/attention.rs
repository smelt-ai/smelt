//! Agent 状态变化产生的“需要告知用户的事”。状态描述当前事实，关注事件描述一次性
//! 消息；两者分开后，铃铛、系统通知和角标只需选择投递渠道，不再各自猜 phase。

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
    System,
}

pub fn delivery_channel(
    enabled: bool,
    app_active: bool,
    notification_window_active: bool,
    is_current_view: bool,
) -> DeliveryChannel {
    if !enabled || (app_active && notification_window_active && is_current_view) {
        DeliveryChannel::Suppress
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
        self.publish_from(item, now)
    }

    /// 记录终端产生的信息性 OSC/BEL 通知。它不包含 agent phase 或完成推断。
    pub fn publish_terminal_notification(&mut self, item: AttentionItem, now: Instant) -> bool {
        self.publish_from(item, now)
    }

    fn publish_from(&mut self, item: AttentionItem, now: Instant) -> bool {
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

    /// 建立冷启动时已经存在的行动项事实。它会参与角标，但视为用户在上次运行中
    /// 已经见过，因此既不记为未读，也不进入本次启动的系统投递队列。
    fn seed_read_action(&mut self, item: AttentionItem) {
        debug_assert!(item.kind.requires_action());
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.current.insert(
            item.session_id.clone(),
            AttentionRecord {
                item,
                read: true,
                sequence,
            },
        );
    }

    pub fn mark_read(&mut self, session_id: &str) -> Option<AttentionItem> {
        let record = self.current.get_mut(session_id)?;
        if record.read {
            return None;
        }
        record.read = true;
        Some(record.item.clone())
    }

    /// 当前 phase 已离开等待/失败/完成状态，旧关注周期才算真正结束。这里同时
    /// 清掉投递指纹，让同一会话下一轮即使在 60 秒内产生相同文案也能正常提醒；
    /// 单纯 `mark_read` 不会重置指纹，仍可压住同一状态的重复广播。
    pub fn resolve(&mut self, session_id: &str) -> Option<AttentionItem> {
        let item = self.current.remove(session_id).map(|record| record.item);
        self.last_delivery.remove(session_id);
        item
    }

    pub fn remove_session(&mut self, session_id: &str) -> bool {
        let removed_current = self.current.remove(session_id).is_some();
        let removed_fingerprint = self.last_delivery.remove(session_id).is_some();
        let pending_before = self.pending_delivery.len();
        self.pending_delivery
            .retain(|item| item.session_id != session_id);
        removed_current || removed_fingerprint || self.pending_delivery.len() != pending_before
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

    /// Dock / 菜单栏数字：未读事件和仍未解决的行动项取并集，每个会话最多计 1。
    /// 因而普通完成在看过后消失，审批/输入/失败即使看过也会保留到 daemon 确认继续。
    pub fn badge_count(&self) -> usize {
        self.current
            .values()
            .filter(|record| !record.read || record.item.kind.requires_action())
            .count()
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

    pub fn has_pending_deliveries(&self) -> bool {
        !self.pending_delivery.is_empty()
    }

    pub fn drain_deliveries(&mut self) -> Vec<AttentionItem> {
        std::mem::take(&mut self.pending_delivery)
    }
}

/// 仅在进入一个新的、由回合级结构化事件证明的可通知 phase 时产生事件。
pub fn item_for_daemon_transition(
    previous: Option<&DaemonSessionState>,
    state: &DaemonSessionState,
) -> Option<AttentionItem> {
    if !state.has_runtime() || !state.phase_is_authoritative() {
        return None;
    }
    if !state.structured_events
        || previous.is_some_and(|previous| {
            previous.phase == state.phase
                && previous.structured_events
                && previous.phase_is_authoritative()
        })
    {
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

/// 用首次 daemon 快照建立 attention 基线。冷启动前就已经完成或失败的回合不能被
/// 当成刚发生的边沿重新投递；其中审批、等待输入和失败仍是当前未解决事实，所以静默
/// 保留为已读行动项，继续显示角标直到 daemon 状态离开该 phase。
pub fn apply_daemon_baseline(
    store: &mut AttentionStore,
    state: &DaemonSessionState,
) -> Option<AttentionItem> {
    let item = item_for_daemon_transition(None, state)?;
    if !item.kind.requires_action() {
        return None;
    }
    store.seed_read_action(item.clone());
    Some(item)
}

/// 将一次 daemon 更新完整应用到 store：可通知 phase 发布事件；结构化会话离开
/// 等待/失败状态时解决旧行动项。调用者只负责保存最新 phase，不再复制生命周期判断。
pub fn apply_daemon_transition(
    store: &mut AttentionStore,
    previous: Option<&DaemonSessionState>,
    state: &DaemonSessionState,
    now: Instant,
) -> Option<AttentionItem> {
    if !state.has_runtime() {
        store.resolve(&state.id);
        return None;
    }
    if !state.phase_is_authoritative() {
        return None;
    }
    if let Some(item) = item_for_daemon_transition(previous, state) {
        store.publish(item.clone(), now);
        return Some(item);
    }
    if state.structured_events
        && state.phase_is_authoritative()
        && matches!(
            state.phase,
            DaemonPhase::Thinking
                | DaemonPhase::ExecutingTool
                | DaemonPhase::Connecting
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
        AttentionItem, AttentionKind, AttentionStore, DeliveryChannel, apply_daemon_baseline,
        apply_daemon_transition, delivery_channel, item_for_daemon_transition,
    };
    use crate::daemon_state::{DaemonPhase, DaemonSessionState};

    fn state(phase: DaemonPhase) -> DaemonSessionState {
        DaemonSessionState {
            id: "session-12345678".into(),
            phase,
            structured_events: true,
            turn_events: true,
            ..Default::default()
        }
    }

    #[test]
    fn transition_emits_one_typed_item() {
        let mut current = state(DaemonPhase::AwaitingApproval);
        current.pending_question = Some("允许执行？".into());
        let previous = state(DaemonPhase::ExecutingTool);
        let item = item_for_daemon_transition(Some(&previous), &current).unwrap();
        assert_eq!(item.kind, AttentionKind::Approval);
        assert_eq!(item.message, "⚠ 允许执行？");
        assert!(item.kind.requires_action());
        assert!(item_for_daemon_transition(Some(&current), &current).is_none());
    }

    #[test]
    fn ghost_runtime_does_not_emit_stale_approval() {
        let mut ghost = state(DaemonPhase::AwaitingApproval);
        ghost.runtime = false;
        ghost.pending_question = Some("允许执行？".into());
        assert!(item_for_daemon_transition(None, &ghost).is_none());
        let mut store = AttentionStore::default();
        assert!(apply_daemon_transition(&mut store, None, &ghost, Instant::now()).is_none());
    }

    #[test]
    fn transition_maps_input_success_and_failure() {
        let mut input = state(DaemonPhase::WaitingForUser);
        input.pending_question = Some("Pick one".into());
        let thinking = state(DaemonPhase::Thinking);
        let item = item_for_daemon_transition(Some(&thinking), &input).unwrap();
        assert_eq!(item.kind, AttentionKind::Input);
        assert_eq!(item.message, "💬 Pick one");

        let success = state(DaemonPhase::Succeeded);
        let item = item_for_daemon_transition(Some(&thinking), &success).unwrap();
        assert_eq!(item.kind, AttentionKind::Success);
        assert_eq!(item.message, "会话 session-");
        assert!(!item.kind.requires_action());

        let mut failure = state(DaemonPhase::Failed);
        failure.pending_question = Some("rate limited".into());
        let item = item_for_daemon_transition(Some(&thinking), &failure).unwrap();
        assert_eq!(item.kind, AttentionKind::Failure);
        assert_eq!(item.message, "rate limited");
    }

    #[test]
    fn initial_snapshot_seeds_only_action_facts_without_delivery() {
        for phase in [
            DaemonPhase::AwaitingApproval,
            DaemonPhase::WaitingForUser,
            DaemonPhase::Failed,
        ] {
            let current = state(phase);
            let mut store = AttentionStore::default();

            let item = apply_daemon_baseline(&mut store, &current)
                .expect("冷启动时仍应保留需要用户处理的状态事实");

            assert!(item.kind.requires_action());
            assert!(store.unread(&current.id).is_none());
            assert!(store.has_unresolved_action(&current.id));
            assert_eq!(store.badge_count(), 1);
            assert!(!store.has_pending_deliveries());
        }

        let success = state(DaemonPhase::Succeeded);
        let mut store = AttentionStore::default();
        assert!(apply_daemon_baseline(&mut store, &success).is_none());
        assert_eq!(store.badge_count(), 0);
        assert!(!store.has_pending_deliveries());
    }

    #[test]
    fn untrusted_sessions_and_non_attention_phases_do_not_emit() {
        let mut current = state(DaemonPhase::Succeeded);
        current.structured_events = false;
        current.turn_events = false;
        let thinking = state(DaemonPhase::Thinking);
        assert!(item_for_daemon_transition(Some(&thinking), &current).is_none());

        current.structured_events = true;
        current.phase = DaemonPhase::Thinking;
        let idle = state(DaemonPhase::Idle);
        assert!(item_for_daemon_transition(Some(&idle), &current).is_none());
    }

    #[test]
    fn unproven_phase_cannot_create_or_resolve_attention() {
        let now = Instant::now();
        let mut previous = state(DaemonPhase::Thinking);
        previous.turn_events = false;
        let mut current = state(DaemonPhase::Succeeded);
        current.turn_events = false;
        assert!(item_for_daemon_transition(Some(&previous), &current).is_none());

        let mut store = AttentionStore::default();
        store.publish(
            item(&current.id, AttentionKind::Approval, "允许执行？"),
            now,
        );
        assert!(apply_daemon_transition(&mut store, Some(&previous), &current, now).is_none());
        assert!(store.has_unresolved_action(&current.id));
    }

    #[test]
    fn authoritative_success_after_an_untrusted_phase_emits_success() {
        let mut untrusted_success = state(DaemonPhase::Succeeded);
        untrusted_success.structured_events = false;
        untrusted_success.turn_events = false;
        let structured_success = state(DaemonPhase::Succeeded);
        let mut store = AttentionStore::default();

        let item = apply_daemon_transition(
            &mut store,
            Some(&untrusted_success),
            &structured_success,
            Instant::now(),
        )
        .expect("验证过的回合完成应产生提醒");

        assert_eq!(item.kind, AttentionKind::Success);
        assert_eq!(item.session_id, structured_success.id);
        assert_eq!(
            store.unread(&structured_success.id).unwrap().kind,
            AttentionKind::Success
        );
        assert_eq!(store.drain_deliveries(), vec![item]);
    }

    #[test]
    fn informational_terminal_notification_does_not_mask_structured_completion() {
        let now = Instant::now();
        let structured_success = state(DaemonPhase::Succeeded);
        let mut store = AttentionStore::default();
        store.publish_terminal_notification(
            item(
                &structured_success.id,
                AttentionKind::Notice,
                "OSC notification",
            ),
            now,
        );
        store.drain_deliveries();
        let thinking = state(DaemonPhase::Thinking);
        let completion = apply_daemon_transition(
            &mut store,
            Some(&thinking),
            &structured_success,
            now + Duration::from_millis(1),
        )
        .unwrap();
        assert_eq!(completion.kind, AttentionKind::Success);
        assert_eq!(store.drain_deliveries(), vec![completion]);
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
        assert_eq!(store.badge_count(), 1);
        assert!(store.mark_read("a").is_some());
        assert!(store.mark_read("a").is_none());
        assert!(store.unread("a").is_none());
        assert_eq!(store.unresolved_action_count(), 0);
        assert_eq!(store.badge_count(), 0);

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
        assert_eq!(store.badge_count(), 1);
        store.mark_read("a");
        assert!(store.unread("a").is_none());
        assert_eq!(store.unresolved_action_count(), 1);
        assert_eq!(store.badge_count(), 1);
        store.resolve("a");
        assert_eq!(store.unresolved_action_count(), 0);
        assert_eq!(store.badge_count(), 0);
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
            delivery_channel(true, true, true, true),
            DeliveryChannel::Suppress
        );
        assert_eq!(
            delivery_channel(false, true, true, false),
            DeliveryChannel::Suppress
        );
        assert_eq!(
            delivery_channel(true, true, true, false),
            DeliveryChannel::System
        );
        assert_eq!(
            delivery_channel(true, false, true, true),
            DeliveryChannel::System
        );
        assert_eq!(
            delivery_channel(true, true, false, true),
            DeliveryChannel::System
        );
        assert_eq!(
            delivery_channel(true, true, false, false),
            DeliveryChannel::System
        );
    }

    #[test]
    fn daemon_lifecycle_keeps_read_action_until_phase_resolves_it() {
        let now = Instant::now();
        let mut store = AttentionStore::default();
        let waiting = state(DaemonPhase::AwaitingApproval);
        let executing = state(DaemonPhase::ExecutingTool);
        apply_daemon_transition(&mut store, Some(&executing), &waiting, now);
        store.mark_read(&waiting.id);
        assert_eq!(store.unresolved_action_count(), 1);

        let running = state(DaemonPhase::Thinking);
        apply_daemon_transition(
            &mut store,
            Some(&waiting),
            &running,
            now + Duration::from_secs(1),
        );
        assert_eq!(store.unresolved_action_count(), 0);
        assert!(store.unread(&running.id).is_none());
    }

    #[test]
    fn a_new_daemon_cycle_rearms_identical_delivery_immediately() {
        let now = Instant::now();
        let mut store = AttentionStore::default();
        let thinking = state(DaemonPhase::Thinking);
        let success = state(DaemonPhase::Succeeded);

        apply_daemon_transition(&mut store, Some(&thinking), &success, now);
        assert_eq!(store.drain_deliveries().len(), 1);

        apply_daemon_transition(
            &mut store,
            Some(&success),
            &thinking,
            now + Duration::from_secs(1),
        );
        apply_daemon_transition(
            &mut store,
            Some(&thinking),
            &success,
            now + Duration::from_secs(2),
        );
        assert_eq!(store.drain_deliveries().len(), 1);
    }
}
