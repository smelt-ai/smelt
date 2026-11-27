//! `subagent.transcript`：读一个 Pi 子智能体的子会话，给侧边视图看它的过程。
//!
//! 子会话是子进程自己写的 Pi 会话文件（`~/.smelt/subagents/<id>.jsonl`），不起 Pi 进程，
//! 按恢复会话时同一套规则读成对话条目。只认宿主分的 id 格式，拿它拼路径不会越出目录。
//!
//! 子智能体每多一条消息，侧边视图就带上次的 `cursor` 再取一次，只拿变了的条目：文件只往后
//! 追加，把「读到 `cursor` 为止」和现在各归约一遍、逐条比，第一条不一样的就是起点。已有条目
//! 怎么变（工具结果回来、上次读时写到一半的那一轮被当成中断）都不用另记规则。

use smelt_core::acp_chat::AcpEntry;
use smelt_core::control_api::{SubagentTranscriptParams, SubagentTranscriptResult};
use smelt_core::pi_rpc::transcript_entries_from_session_file;
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode, ControlReason};

const PAGE_DEFAULT: u32 = 200;
const PAGE_MAX: u32 = 1000;

pub(crate) fn subagent_transcript(
    params: SubagentTranscriptParams,
) -> Result<SubagentTranscriptResult, ControlError> {
    let id = params.subagent_id.trim();
    if !smelt_core::subagent_sessions::is_subagent_id(id) {
        return Err(ControlError::because(
            ControlErrorCode::InvalidParams,
            ControlReason::NotSubagentId { id: id.to_string() },
        ));
    }
    let file = smelt_core::subagent_sessions::session_file(id).ok_or_else(|| {
        ControlError::because(
            ControlErrorCode::OperationFailed,
            ControlReason::NoSmeltHome,
        )
    })?;
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ControlError::because(
                ControlErrorCode::NotFound,
                ControlReason::SubagentTranscriptGone,
            ));
        }
        Err(error) => {
            return Err(ControlError::because(
                ControlErrorCode::OperationFailed,
                ControlReason::SubagentTranscriptUnreadable {
                    error: error.to_string(),
                },
            ));
        }
    };
    Ok(transcript_page(&text, &params))
}

fn transcript_page(text: &str, params: &SubagentTranscriptParams) -> SubagentTranscriptResult {
    let entries = transcript_entries_from_session_file(text);
    let changed_from = params
        .since
        .map(|since| changed_since(text, since, &entries));
    SubagentTranscriptResult {
        cursor: text.len() as u64,
        ..page(entries, params.before, params.limit, changed_from)
    }
}

/// 文件读到 `since` 字节时的条目和现在的 `entries` 从第一条哪里不一样。`since` 不在文件里
/// （文件被换成更短的了）就从头算。
fn changed_since(text: &str, since: u64, entries: &[AcpEntry]) -> usize {
    let Some(earlier) = usize::try_from(since)
        .ok()
        .and_then(|since| text.get(..since))
    else {
        return 0;
    };
    if earlier.len() == text.len() {
        return entries.len();
    }
    let earlier = transcript_entries_from_session_file(earlier);
    earlier
        .iter()
        .zip(entries)
        .position(|(earlier, now)| earlier != now)
        .unwrap_or(earlier.len().min(entries.len()))
}

/// `[start, end)`：`end` 是 `before` 或末尾，往前最多 `limit` 条，不早于 `changed_from`。
fn page(
    entries: Vec<AcpEntry>,
    before: Option<u64>,
    limit: Option<u32>,
    changed_from: Option<usize>,
) -> SubagentTranscriptResult {
    let total = entries.len();
    let end = before
        .map(|before| usize::try_from(before).unwrap_or(usize::MAX).min(total))
        .unwrap_or(total);
    let limit = limit.unwrap_or(PAGE_DEFAULT).clamp(1, PAGE_MAX) as usize;
    let start = end
        .saturating_sub(limit)
        .max(changed_from.unwrap_or(0).min(end));
    SubagentTranscriptResult {
        entries: entries.into_iter().skip(start).take(end - start).collect(),
        offset: start as u64,
        total: total as u64,
        cursor: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_core::acp_chat::ToolCallStatus;

    fn entries(count: usize) -> Vec<AcpEntry> {
        (0..count)
            .map(|index| AcpEntry::User(index.to_string()))
            .collect()
    }

    #[test]
    fn the_default_page_is_the_tail() {
        let result = page(entries(250), None, None, None);
        assert_eq!(result.total, 250);
        assert_eq!(result.offset, 50);
        assert_eq!(result.entries.len(), 200);
        assert_eq!(result.entries[0], AcpEntry::User("50".into()));
    }

    #[test]
    fn an_older_page_ends_before_the_given_entry() {
        let result = page(entries(250), Some(50), Some(20), None);
        assert_eq!(result.offset, 30);
        assert_eq!(result.entries.len(), 20);
        assert_eq!(result.entries.last(), Some(&AcpEntry::User("49".into())));
        assert!(page(entries(3), Some(0), None, None).entries.is_empty());
    }

    /// 一个并行两路读文件的子智能体：第一路结果落盘时读过一次，之后第二路结果和总结落盘。
    fn session_lines() -> Vec<String> {
        [
            serde_json::json!({"type": "session", "version": 3, "id": "sa-aaaaaaaaaaaa", "cwd": "/repo"}),
            serde_json::json!({"type": "message", "id": "u1", "parentId": null,
                "message": {"role": "user", "content": [{"type": "text", "text": "查登录"}]}}),
            serde_json::json!({"type": "message", "id": "a1", "parentId": "u1",
                "message": {"role": "assistant", "content": [
                    {"type": "text", "text": "先看看"},
                    {"type": "toolCall", "id": "t1", "name": "read", "arguments": {"path": "a.rs"}},
                    {"type": "toolCall", "id": "t2", "name": "read", "arguments": {"path": "b.rs"}}
                ]}}),
            serde_json::json!({"type": "message", "id": "r1", "parentId": "a1",
                "message": {"role": "toolResult", "toolCallId": "t1", "toolName": "read",
                    "content": [{"type": "text", "text": "fn a() {}"}]}}),
            serde_json::json!({"type": "message", "id": "r2", "parentId": "r1",
                "message": {"role": "toolResult", "toolCallId": "t2", "toolName": "read",
                    "content": [{"type": "text", "text": "fn b() {}"}]}}),
            serde_json::json!({"type": "message", "id": "a2", "parentId": "r2",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "登录在 a.rs"}]}}),
        ]
        .iter()
        .map(|line| format!("{line}\n"))
        .collect()
    }

    fn params(since: Option<u64>) -> SubagentTranscriptParams {
        SubagentTranscriptParams {
            subagent_id: "sa-aaaaaaaaaaaa".into(),
            since,
            ..Default::default()
        }
    }

    #[test]
    fn since_returns_only_what_changed_after_the_cursor() {
        let lines = session_lines();
        let earlier = transcript_page(&lines[..4].concat(), &params(None));
        let now = lines.concat();

        let changed = transcript_page(&now, &params(Some(earlier.cursor)));

        // 第二路工具上次还没结果（读的时候当成中断），这次补上了：从它起换，前面三条不重发。
        assert_eq!(changed.offset, 3);
        assert_eq!(changed.total, 5);
        assert_eq!(changed.cursor, now.len() as u64);
        assert!(matches!(
            &changed.entries[..],
            [
                AcpEntry::ToolCall { status: ToolCallStatus::Completed, .. },
                AcpEntry::Assistant { text, .. },
            ] if text == "登录在 a.rs"
        ));
        assert_eq!(
            transcript_page(&now, &params(None)).entries[3..],
            changed.entries[..],
            "拼上去和整段重读一样"
        );
    }

    #[test]
    fn since_at_the_end_returns_nothing_and_a_cursor_outside_the_file_starts_over() {
        let now = session_lines().concat();
        let unchanged = transcript_page(&now, &params(Some(now.len() as u64)));
        assert!(unchanged.entries.is_empty());
        assert_eq!(unchanged.offset, 5);

        let replaced = transcript_page(&now, &params(Some(now.len() as u64 + 10)));
        assert_eq!(replaced.offset, 0);
        assert_eq!(replaced.entries.len(), 5);
    }

    #[test]
    fn an_id_that_is_not_a_subagent_id_is_rejected() {
        let error = subagent_transcript(SubagentTranscriptParams {
            subagent_id: "../../etc/passwd".into(),
            ..Default::default()
        })
        .unwrap_err();
        assert_eq!(error.code, ControlErrorCode::InvalidParams);
    }
}
