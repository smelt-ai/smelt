//! 快照窗口的合并：拼条目的规则在 `smelt_model::entry_window`（桌面对话插件共用），这里按它的结果
//! 拼条目、挑快照头。
//!
//! `envelope_omitted` 的帧：快照头取它时，信封字段（`ConversationSnapshot::ENVELOPE_KEYS`）沿用手里的那份。

use smelt_model::{
    acp_session::ConversationSnapshot,
    entry_window::{self, EntryWindow},
};

/// 跨重启稳定的对话身份。空串当没有。
pub(crate) fn stable_history_id(snapshot: &ConversationSnapshot) -> Option<&str> {
    entry_window::conversation_id(
        snapshot.history_session_id.as_deref(),
        snapshot.acp_session_id.as_deref(),
    )
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
    let plan = entry_window::merge(&EntryWindow::of(&current), &EntryWindow::of(&next));
    let entries = plan.splice(
        std::mem::take(&mut current.entries),
        std::mem::take(&mut next.entries),
    );
    let envelope_omitted = next.envelope_omitted;
    let (mut merged, other) = if plan.next_is_newer {
        (next, current)
    } else {
        (current, next)
    };
    if envelope_omitted && plan.next_is_newer {
        merged.keep_envelope(other);
    }
    merged.entries_offset = plan.window.offset;
    merged.entries_total = plan.window.total;
    merged.entries = entries;
    merged.envelope_omitted = false;
    merged
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

    /// 省略信封的帧照样带用量、重试、回合结果（每帧都带的字段），要用本帧的；
    /// 标题、还回输入框的原文是信封，要沿用手里的。
    #[test]
    fn an_envelope_omitted_frame_takes_per_frame_fields_and_keeps_title() {
        let mut current = page(0..2, 2, 1);
        current.session_title = Some("标题".into());
        current.composer_restore_revision = 3;
        current.composer_restore_texts = vec!["还回来".into()];
        current.usage = Some((10, 100));
        current.completed_unread = true;
        let mut next = snapshot(1, 2, 2, &["e1'"]);
        next.envelope_omitted = true;
        next.usage = Some((20, 100));
        next.completed_unread = false;
        let merged = merge(current, next);
        assert_eq!(merged.usage, Some((20, 100)));
        assert!(!merged.completed_unread);
        assert_eq!(merged.session_title.as_deref(), Some("标题"));
        assert_eq!(merged.composer_restore_revision, 3);
        assert_eq!(merged.composer_restore_texts, ["还回来"]);
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
