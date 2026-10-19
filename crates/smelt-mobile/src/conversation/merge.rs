//! 快照窗口的合并：实时尾部、翻出来的更早页、断线重连后的整段帧都并进同一个连续窗口。
//!
//! 规则搬自旧 Dart 的 `AcpSnapshot.merge`：
//! - 恢复身份（`history_session_id`，没有时 `acp_session_id`）两边都有且不同：换了一段
//!   对话，整个换成新的。
//! - 快照头（相位、审批、模型……）取修订号较新的那份。
//! - 两个窗口相交或相接就按下标拼接；不相接时，新的一份若到末尾就以它为准（实时尾部），
//!   否则丢掉它的条目（孤立的旧页拼上去会留下空洞），只更新快照头。
//! - 拼好后按快照头的 `entries_total` 截掉多出来的尾巴（对面删了条目）。
//! - `envelope_omitted` 的帧只带正文和相位：信封字段沿用手里的那份。旧 Dart 只在拼接
//!   路径上沿用，不相接的两条路径会把审批、模型等清空，这里统一沿用。

use smelt_model::acp_session::ConversationSnapshot;

/// 跨重启稳定的对话身份。空串当没有。
pub(crate) fn stable_history_id(snapshot: &ConversationSnapshot) -> Option<&str> {
    snapshot
        .history_session_id
        .as_deref()
        .or(snapshot.acp_session_id.as_deref())
        .filter(|id| !id.trim().is_empty())
}

pub(crate) fn entries_end(snapshot: &ConversationSnapshot) -> usize {
    snapshot.entries_offset + snapshot.entries.len()
}

/// 收到的快照先规整：老运行时缺 `entries_total`（反序列化成 0）时按窗口末尾算，
/// 免得合并时把条目全截掉。
pub(crate) fn normalize(snapshot: &mut ConversationSnapshot) {
    snapshot.entries_total = snapshot.entries_total.max(entries_end(snapshot));
}

pub(crate) fn merge(
    mut current: ConversationSnapshot,
    mut next: ConversationSnapshot,
) -> ConversationSnapshot {
    if let (Some(current_id), Some(next_id)) =
        (stable_history_id(&current), stable_history_id(&next))
        && current_id != next_id
    {
        return next;
    }
    let envelope_omitted = next.envelope_omitted;
    let (current_start, current_end) = (current.entries_offset, entries_end(&current));
    let (next_start, next_end) = (next.entries_offset, entries_end(&next));
    let current_entries = std::mem::take(&mut current.entries);
    let next_entries = std::mem::take(&mut next.entries);

    let (start, entries) = if next_end < current_start || next_start > current_end {
        if next_end == next.entries_total {
            // 不相接但到了末尾：实时尾部为准，丢掉手里的旧窗口。
            next.entries = next_entries;
            if envelope_omitted {
                keep_envelope(&mut next, current);
            }
            return next;
        }
        (current_start, current_entries)
    } else if next_start <= current_start {
        let old_suffix = (next_end - current_start).min(current_entries.len());
        let mut merged = next_entries;
        merged.extend(current_entries.into_iter().skip(old_suffix));
        (next_start, merged)
    } else {
        let prefix = (next_start - current_start).min(current_entries.len());
        let suffix = (next_end - current_start).min(current_entries.len());
        let mut merged = Vec::with_capacity(prefix + next_entries.len());
        let mut old = current_entries.into_iter();
        merged.extend(old.by_ref().take(prefix));
        merged.extend(next_entries);
        merged.extend(old.skip(suffix - prefix));
        (current_start, merged)
    };

    let next_is_newer = next.snapshot_revision >= current.snapshot_revision;
    let (mut merged, other) = if next_is_newer {
        (next, current)
    } else {
        (current, next)
    };
    let allowed = merged.entries_total.saturating_sub(start);
    let mut entries = entries;
    entries.truncate(allowed);
    merged.entries_offset = start;
    merged.entries = entries;
    if envelope_omitted && next_is_newer {
        keep_envelope(&mut merged, other);
    }
    merged.envelope_omitted = false;
    merged
}

/// `target` 是只带正文的帧：信封字段换回 `previous` 的。
fn keep_envelope(target: &mut ConversationSnapshot, previous: ConversationSnapshot) {
    target.pending_permissions = previous.pending_permissions;
    target.pending_elicitation = previous.pending_elicitation;
    target.status_line = previous.status_line;
    target.available_commands = previous.available_commands;
    target.usage = previous.usage;
    target.usage_cached_read = previous.usage_cached_read;
    target.usage_cost = previous.usage_cost;
    target.usage_breakdown = previous.usage_breakdown;
    target.plan = previous.plan;
    target.background_tasks = previous.background_tasks;
    target.model = previous.model;
    target.config_options = previous.config_options;
    target.conversation_state = previous.conversation_state;
    target.provider_retry = previous.provider_retry;
    target.notice = previous.notice;
    target.pending_prompts = previous.pending_prompts;
    target.queued_steering = previous.queued_steering;
    target.queued_follow_up = previous.queued_follow_up;
    target.turn_started_at_ms = previous.turn_started_at_ms;
    target.turn_timings = previous.turn_timings;
    target.completed_unread = previous.completed_unread;
    target.turn_outcome = previous.turn_outcome;
    target.envelope_omitted = false;
}

/// 存缓存用：只留最后 `limit` 条，`entries_total` 不变（缓存窗口没到开头）。
pub(crate) fn cache_tail(snapshot: &ConversationSnapshot, limit: usize) -> ConversationSnapshot {
    let skipped = snapshot.entries.len().saturating_sub(limit);
    let mut tail = snapshot.clone();
    tail.entries.drain(..skipped);
    tail.entries_offset += skipped;
    tail
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;
    use smelt_model::acp_chat::AcpEntry;

    use super::*;

    pub(crate) fn assistant(text: &str) -> AcpEntry {
        AcpEntry::Assistant {
            text: text.to_string(),
            thought: false,
        }
    }

    pub(crate) fn texts(snapshot: &ConversationSnapshot) -> Vec<String> {
        snapshot
            .entries
            .iter()
            .map(|entry| match entry {
                AcpEntry::Assistant { text, .. } | AcpEntry::User(text) => text.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// 测试用快照：`offset` 起的 `entries`，其余字段取线格式的最小值。
    pub(crate) fn snapshot(
        offset: usize,
        total: usize,
        revision: u64,
        entries: &[&str],
    ) -> ConversationSnapshot {
        let entries: Vec<AcpEntry> = entries.iter().map(|text| assistant(text)).collect();
        serde_json::from_value(snapshot_json(offset, total, revision, &entries)).unwrap()
    }

    pub(crate) fn snapshot_json(
        offset: usize,
        total: usize,
        revision: u64,
        entries: &[AcpEntry],
    ) -> serde_json::Value {
        json!({
            "entries_offset": offset,
            "entries_total": total,
            "snapshot_revision": revision,
            "entries": entries,
            "phase": "idle",
            "end_reason": "",
            "pending_permissions": [],
            "pending_elicitation": null,
            "status_line": null,
            "acp_session_id": null,
            "history_session_id": "h1",
            "supports_image": true,
            "available_commands": [],
            "usage": null,
            "plan": null,
            "model": null,
            "config_options": [],
            "completed_unread": false,
            "should_persist": false,
        })
    }

    fn page(range: std::ops::Range<usize>, total: usize, revision: u64) -> ConversationSnapshot {
        let labels: Vec<String> = range.clone().map(|i| format!("e{i}")).collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        snapshot(range.start, total, revision, &labels)
    }

    fn labels(range: std::ops::Range<usize>) -> Vec<String> {
        range.map(|i| format!("e{i}")).collect()
    }

    #[test]
    fn a_tail_frame_replaces_from_its_offset() {
        let current = page(0..3, 3, 1);
        let mut next = snapshot(2, 4, 2, &["e2'", "e3"]);
        next.status_line = Some("新的头".into());
        let merged = merge(current, next);
        assert_eq!(merged.entries_offset, 0);
        assert_eq!(texts(&merged), ["e0", "e1", "e2'", "e3"]);
        assert_eq!(merged.status_line.as_deref(), Some("新的头"));
    }

    #[test]
    fn an_older_page_is_prepended_and_keeps_the_newer_header() {
        let mut current = page(100..103, 103, 9);
        current.status_line = Some("实时".into());
        let mut older = page(0..100, 103, 5);
        older.status_line = Some("旧页".into());
        let merged = merge(current, older);
        assert_eq!(merged.entries_offset, 0);
        assert_eq!(texts(&merged), labels(0..103));
        assert_eq!(
            merged.status_line.as_deref(),
            Some("实时"),
            "头取修订号新的"
        );
    }

    #[test]
    fn a_disjoint_live_tail_wins() {
        let current = page(0..10, 10, 1);
        let merged = merge(current, page(200..210, 210, 2));
        assert_eq!(merged.entries_offset, 200);
        assert_eq!(texts(&merged), labels(200..210));
    }

    #[test]
    fn an_isolated_older_page_is_ignored_but_its_header_is_taken() {
        let current = page(200..210, 210, 1);
        let mut isolated = page(0..10, 212, 2);
        isolated.status_line = Some("新头".into());
        let merged = merge(current, isolated);
        assert_eq!(merged.entries_offset, 200);
        assert_eq!(texts(&merged), labels(200..210));
        assert_eq!(merged.status_line.as_deref(), Some("新头"));
        assert_eq!(merged.entries_total, 212);
    }

    #[test]
    fn a_shorter_total_trims_removed_entries() {
        let current = page(0..5, 5, 1);
        let merged = merge(current, snapshot(3, 3, 2, &[]));
        assert_eq!(texts(&merged), labels(0..3));
    }

    #[test]
    fn a_new_history_id_resets_the_window() {
        let current = page(0..5, 5, 1);
        let mut next = page(4..6, 6, 0);
        next.history_session_id = Some("h2".into());
        let merged = merge(current, next);
        assert_eq!(merged.entries_offset, 4);
        assert_eq!(texts(&merged), labels(4..6));
        assert_eq!(merged.history_session_id.as_deref(), Some("h2"));
    }

    #[test]
    fn an_envelope_omitted_frame_keeps_the_envelope() {
        let mut current = page(0..2, 2, 1);
        current.status_line = Some("干活中".into());
        current.available_commands = vec![("help".into(), "帮助".into())];
        current.turn_started_at_ms = Some(42);
        let mut next = snapshot(1, 2, 2, &["e1'"]);
        next.envelope_omitted = true;
        let merged = merge(current.clone(), next.clone());
        assert_eq!(texts(&merged), ["e0", "e1'"]);
        assert_eq!(merged.status_line.as_deref(), Some("干活中"));
        assert_eq!(merged.available_commands.len(), 1);
        assert_eq!(merged.turn_started_at_ms, Some(42));
        assert!(!merged.envelope_omitted);

        // 不相接的实时尾部同样沿用信封。
        next.entries_offset = 50;
        next.entries_total = 51;
        let merged = merge(current, next);
        assert_eq!(merged.entries_offset, 50);
        assert_eq!(merged.status_line.as_deref(), Some("干活中"));
    }

    #[test]
    fn cache_tail_keeps_the_last_entries() {
        let full = page(0..150, 150, 1);
        let tail = cache_tail(&full, 100);
        assert_eq!(tail.entries_offset, 50);
        assert_eq!(tail.entries_total, 150);
        assert_eq!(texts(&tail), labels(50..150));
    }

    #[test]
    fn a_missing_total_is_derived_from_the_window() {
        let mut snapshot = page(3..5, 0, 1);
        normalize(&mut snapshot);
        assert_eq!(snapshot.entries_total, 5);
    }
}
