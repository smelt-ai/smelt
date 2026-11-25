//! 对话快照条目窗口的合并：实时尾部、翻出来的更早页、断线重连后的整段帧并进同一个连续窗口。
//!
//! 手机端和桌面对话插件（经 SDK）共用这一份。只看窗口的位置和归属，不碰条目本身：
//! 调用方拿 [`WindowMerge`] 按下标拼自己手里的条目（Rust 用 [`WindowMerge::splice`]）。
//!
//! - 恢复身份（`history_session_id`，没有时 `acp_session_id`）两边都有且不同：换了一段对话，
//!   整个换成新的。
//! - 快照头（相位、审批、模型……）取修订号较新的那份。
//! - 两个窗口相交或相接就按下标拼接；不相接时，新的一份若到末尾就以它为准（实时尾部），
//!   否则丢掉它的条目（孤立的旧页拼上去会留下空洞），只更新快照头。
//! - 拼好后按较新快照头的 `entries_total` 截掉多出来的尾巴（对面删了条目）。窗口起点已经超过
//!   总数（对面回退到窗口之前）时窗口清空、停在末尾，由调用方往前补页。

use serde::{Deserialize, Serialize};

use crate::acp_session::ConversationSnapshot;

/// 跨重启稳定的对话身份：`history_session_id`，没有时 `acp_session_id`。空串当没有。
pub fn conversation_id<'a>(
    history_session_id: Option<&'a str>,
    acp_session_id: Option<&'a str>,
) -> Option<&'a str> {
    history_session_id
        .or(acp_session_id)
        .filter(|id| !id.trim().is_empty())
}

/// 一段条目窗口：全局下标 `[offset, offset + len)`，加上它所在快照的总数、修订号和对话身份。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryWindow {
    pub offset: usize,
    pub len: usize,
    /// 快照的 `entries_total`。老运行时缺这个字段（0）时按窗口末尾算。
    pub total: usize,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub history_session_id: Option<String>,
    #[serde(default)]
    pub acp_session_id: Option<String>,
}

impl EntryWindow {
    pub fn of(snapshot: &ConversationSnapshot) -> Self {
        Self {
            offset: snapshot.entries_offset,
            len: snapshot.entries.len(),
            total: snapshot.entries_total,
            revision: snapshot.snapshot_revision,
            history_session_id: snapshot.history_session_id.clone(),
            acp_session_id: snapshot.acp_session_id.clone(),
        }
    }

    pub fn end(&self) -> usize {
        self.offset + self.len
    }

    pub fn conversation_id(&self) -> Option<&str> {
        conversation_id(
            self.history_session_id.as_deref(),
            self.acp_session_id.as_deref(),
        )
    }

    fn normalized(&self) -> Self {
        Self {
            total: self.total.max(self.end()),
            ..self.clone()
        }
    }
}

/// 合并结果。合并后的条目 = 手里窗口的前 `keep` 条，接上新来的全部（`take_next` 时），
/// 再接手里窗口从下标 `resume` 起的，最后截到 `window.len` 条。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WindowMerge {
    pub window: EntryWindow,
    pub keep: usize,
    pub take_next: bool,
    pub resume: usize,
    /// 快照头取新来的那份；否则沿用手里的。
    pub next_is_newer: bool,
    /// 两边是不同的对话，新来的整个替换了手里的。翻页拿回来的页是这样时，说明拉页期间对话重新
    /// 开始了，这页属于旧对话，调用方应丢掉它而不是用这个结果。
    pub conversation_changed: bool,
    /// 合并后第一条和手里不同的条目在窗口里的下标；条目没变是 `None`。窗口起点挪了从 0 算。
    pub changed_from: Option<usize>,
}

impl WindowMerge {
    pub fn splice<T>(&self, current: Vec<T>, next: Vec<T>) -> Vec<T> {
        let mut merged = Vec::with_capacity(self.window.len);
        let mut current = current.into_iter();
        merged.extend(current.by_ref().take(self.keep));
        if self.take_next {
            merged.extend(next);
        }
        merged.extend(current.skip(self.resume.saturating_sub(self.keep)));
        merged.truncate(self.window.len);
        merged
    }
}

pub fn merge(current: &EntryWindow, next: &EntryWindow) -> WindowMerge {
    let current = current.normalized();
    let next = next.normalized();
    let replace = WindowMerge {
        window: next.clone(),
        keep: 0,
        take_next: true,
        resume: current.len,
        next_is_newer: true,
        conversation_changed: false,
        changed_from: None,
    };
    if let (Some(current_id), Some(next_id)) = (current.conversation_id(), next.conversation_id())
        && current_id != next_id
    {
        let plan = WindowMerge {
            conversation_changed: true,
            ..replace
        };
        return with_changed(&current, &next, plan);
    }
    let (current_start, current_end) = (current.offset, current.end());
    let (next_start, next_end) = (next.offset, next.end());
    let (start, keep, take_next, resume) = if next_end < current_start || next_start > current_end {
        if next_end == next.total {
            return with_changed(&current, &next, replace);
        }
        (current_start, current.len, false, current.len)
    } else if next_start <= current_start {
        let resume = (next_end - current_start).min(current.len);
        (next_start, 0, true, resume)
    } else {
        let keep = (next_start - current_start).min(current.len);
        let resume = (next_end - current_start).min(current.len);
        (current_start, keep, true, resume)
    };
    let next_is_newer = next.revision >= current.revision;
    let newer = if next_is_newer { &next } else { &current };
    let taken = if take_next { next.len } else { 0 };
    let assembled = keep + taken + current.len.saturating_sub(resume);
    let window = EntryWindow {
        offset: start.min(newer.total),
        len: assembled.min(newer.total.saturating_sub(start)),
        ..newer.clone()
    };
    with_changed(
        &current,
        &next,
        WindowMerge {
            window,
            keep,
            take_next,
            resume,
            next_is_newer,
            conversation_changed: false,
            changed_from: None,
        },
    )
}

fn with_changed(current: &EntryWindow, next: &EntryWindow, mut plan: WindowMerge) -> WindowMerge {
    let len = plan.window.len;
    plan.changed_from = if plan.window.offset != current.offset {
        Some(0)
    } else {
        let rewritten = (plan.take_next && next.len > 0) || plan.resume != plan.keep;
        let same = if rewritten { plan.keep } else { current.len }.min(len);
        (same < len || len != current.len).then_some(same)
    };
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(offset: usize, len: usize, total: usize, revision: u64) -> EntryWindow {
        EntryWindow {
            offset,
            len,
            total,
            revision,
            history_session_id: Some("h".into()),
            acp_session_id: None,
        }
    }

    fn apply(current: &EntryWindow, next: &EntryWindow) -> (WindowMerge, Vec<usize>) {
        let plan = merge(current, next);
        let entries = plan.splice(
            (current.offset..current.end()).collect(),
            (next.offset..next.end()).map(|ix| ix + 1000).collect(),
        );
        assert_eq!(entries.len(), plan.window.len, "条数和窗口对得上");
        (plan, entries)
    }

    #[test]
    fn live_tail_rewrites_last_entry_and_appends() {
        let (plan, entries) = apply(&window(10, 3, 13, 1), &window(12, 2, 14, 2));
        assert_eq!(plan.window, window(10, 4, 14, 2));
        assert_eq!(entries, [10, 11, 1012, 1013]);
        assert_eq!(plan.changed_from, Some(2));
        assert!(plan.next_is_newer);
    }

    #[test]
    fn header_only_frame_keeps_entries() {
        let (plan, entries) = apply(&window(10, 3, 13, 1), &window(13, 0, 13, 2));
        assert_eq!(entries, [10, 11, 12]);
        assert_eq!(plan.changed_from, None);
        assert_eq!(plan.window.revision, 2);
    }

    #[test]
    fn older_page_is_prepended() {
        let (plan, entries) = apply(&window(10, 2, 12, 5), &window(8, 2, 12, 3));
        assert_eq!(entries, [1008, 1009, 10, 11]);
        assert_eq!(plan.window.offset, 8);
        assert_eq!(plan.changed_from, Some(0));
        assert!(!plan.next_is_newer, "旧页不覆盖快照头");
        assert_eq!(plan.window.revision, 5);
    }

    #[test]
    fn older_page_total_does_not_override_newer() {
        let (plan, _) = apply(&window(10, 4, 14, 5), &window(8, 2, 12, 3));
        assert_eq!(plan.window.total, 14);
        assert_eq!(plan.window.len, 6);
    }

    #[test]
    fn middle_page_replaces_only_its_span() {
        let (_, entries) = apply(&window(0, 5, 5, 1), &window(1, 2, 5, 1));
        assert_eq!(entries, [0, 1001, 1002, 3, 4]);
    }

    #[test]
    fn detached_live_tail_replaces_window() {
        let (plan, entries) = apply(&window(0, 3, 3, 1), &window(200, 10, 210, 2));
        assert_eq!(entries, (1200..1210).collect::<Vec<_>>());
        assert_eq!(plan.window.offset, 200);
        assert_eq!(plan.changed_from, Some(0));
    }

    #[test]
    fn detached_old_page_only_updates_header() {
        let (plan, entries) = apply(&window(100, 3, 103, 1), &window(10, 5, 103, 2));
        assert_eq!(entries, [100, 101, 102]);
        assert_eq!(plan.changed_from, None);
        assert!(plan.next_is_newer);
    }

    #[test]
    fn shrunk_total_truncates_tail() {
        let (plan, entries) = apply(&window(2, 3, 5, 1), &window(0, 3, 3, 2));
        assert_eq!(entries, [1000, 1001, 1002]);
        assert_eq!(plan.window, window(0, 3, 3, 2));
    }

    #[test]
    fn rewind_before_window_empties_at_end() {
        let (plan, entries) = apply(&window(5, 1, 6, 1), &window(2, 0, 2, 2));
        assert!(entries.is_empty());
        assert_eq!(plan.window, window(2, 0, 2, 2));
        assert_eq!(plan.changed_from, Some(0));
    }

    #[test]
    fn new_conversation_replaces_window() {
        let current = window(0, 3, 3, 9);
        let next = EntryWindow {
            history_session_id: Some("other".into()),
            ..window(0, 1, 1, 1)
        };
        let (plan, entries) = apply(&current, &next);
        assert_eq!(entries, [1000]);
        assert!(plan.next_is_newer, "换了对话不看修订号");
        assert!(plan.conversation_changed);
        assert_eq!(plan.changed_from, Some(0));
        let (plan, _) = apply(&current, &window(3, 1, 4, 10));
        assert!(!plan.conversation_changed, "同一段对话");
    }

    #[test]
    fn empty_window_takes_first_frame() {
        let (plan, entries) = apply(&EntryWindow::default(), &window(7, 0, 7, 1));
        assert!(entries.is_empty());
        assert_eq!(plan.window, window(7, 0, 7, 1));
        let (plan, entries) = apply(&EntryWindow::default(), &window(0, 2, 2, 1));
        assert_eq!(entries, [1000, 1001]);
        assert_eq!(plan.changed_from, Some(0));
    }

    #[test]
    fn missing_total_falls_back_to_window_end() {
        let (plan, entries) = apply(&window(0, 2, 2, 1), &window(2, 1, 0, 2));
        assert_eq!(entries, [0, 1, 1002]);
        assert_eq!(plan.window.total, 3);
    }
}
