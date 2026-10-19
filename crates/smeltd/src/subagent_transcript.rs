//! `subagent.transcript`：读一个 Pi 子智能体的子会话，给侧边视图看它的过程。
//!
//! 子会话是子进程自己写的 Pi 会话文件（`~/.smelt/subagents/<id>.jsonl`），不起 Pi 进程，
//! 按恢复会话时同一套规则读成对话条目。只认宿主分的 id 格式，拿它拼路径不会越出目录。

use smelt_core::control_api::{SubagentTranscriptParams, SubagentTranscriptResult};
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode};

const PAGE_DEFAULT: u32 = 200;
const PAGE_MAX: u32 = 1000;

pub(crate) fn subagent_transcript(
    params: SubagentTranscriptParams,
) -> Result<SubagentTranscriptResult, ControlError> {
    let id = params.subagent_id.trim();
    if !smelt_core::subagent_sessions::is_subagent_id(id) {
        return Err(ControlError::new(
            ControlErrorCode::InvalidParams,
            format!("不是子智能体 id：{id}"),
        ));
    }
    let file = smelt_core::subagent_sessions::session_file(id).ok_or_else(|| {
        ControlError::new(
            ControlErrorCode::OperationFailed,
            "找不到 Smelt 主目录，读不到子会话",
        )
    })?;
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ControlError::new(
                ControlErrorCode::NotFound,
                "这个子智能体的子会话不在了（还没开始写，或已随对话删除）",
            ));
        }
        Err(error) => {
            return Err(ControlError::new(
                ControlErrorCode::OperationFailed,
                format!("读不了子会话：{error}"),
            ));
        }
    };
    let entries = smelt_core::pi_rpc::transcript_entries_from_session_file(&text);
    Ok(page(entries, params.before, params.limit))
}

fn page(entries: Vec<smelt_core::acp_chat::AcpEntry>, before: Option<u64>, limit: Option<u32>) -> SubagentTranscriptResult {
    let total = entries.len();
    let end = before
        .map(|before| usize::try_from(before).unwrap_or(usize::MAX).min(total))
        .unwrap_or(total);
    let limit = limit.unwrap_or(PAGE_DEFAULT).clamp(1, PAGE_MAX) as usize;
    let start = end.saturating_sub(limit);
    SubagentTranscriptResult {
        entries: entries.into_iter().skip(start).take(end - start).collect(),
        offset: start as u64,
        total: total as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_core::acp_chat::AcpEntry;

    fn entries(count: usize) -> Vec<AcpEntry> {
        (0..count).map(|index| AcpEntry::User(index.to_string())).collect()
    }

    #[test]
    fn the_default_page_is_the_tail() {
        let result = page(entries(250), None, None);
        assert_eq!(result.total, 250);
        assert_eq!(result.offset, 50);
        assert_eq!(result.entries.len(), 200);
        assert_eq!(result.entries[0], AcpEntry::User("50".into()));
    }

    #[test]
    fn an_older_page_ends_before_the_given_entry() {
        let result = page(entries(250), Some(50), Some(20));
        assert_eq!(result.offset, 30);
        assert_eq!(result.entries.len(), 20);
        assert_eq!(result.entries.last(), Some(&AcpEntry::User("49".into())));
        assert!(page(entries(3), Some(0), None).entries.is_empty());
    }

    #[test]
    fn an_id_that_is_not_a_subagent_id_is_rejected() {
        let error = subagent_transcript(SubagentTranscriptParams {
            subagent_id: "../../etc/passwd".into(),
            before: None,
            limit: None,
        })
        .unwrap_err();
        assert_eq!(error.code, ControlErrorCode::InvalidParams);
    }
}
