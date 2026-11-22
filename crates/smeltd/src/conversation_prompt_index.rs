//! `conversation.prompt_index`：对话里的每一问，给客户端的提问导航用。
//!
//! 客户端只加载最近的几页，导航却要整段对话的每一问。这里按
//! [`smelt_core::acp_chat::prompt_index`] 在会话自己的条目上算好一行摘要发过去，不拷整页条目。
//! 挂起的会话内存里只剩当前回合，前面的先唤醒再读，和 `acp_snapshot` 往上翻页同一条路。

use smelt_core::control_api::{ConversationPromptIndexParams, ConversationPromptIndexResult};
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode, ControlReason};

use super::{AcpSessions, EventHubHandle};

pub(crate) fn prompt_index(
    params: ConversationPromptIndexParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<ConversationPromptIndexResult, ControlError> {
    let ConversationPromptIndexParams { session_id, before } = params;
    let before = before
        .and_then(|before| usize::try_from(before).ok())
        .unwrap_or(usize::MAX);
    let gone =
        || ControlError::because(ControlErrorCode::NotFound, ControlReason::ConversationGone);
    let slot = acp_sessions.get(&session_id).ok_or_else(gone)?;
    if slot.value.provider_timeline.lock().is_some() {
        let snapshot = crate::acp_host::read_history_page(
            &session_id,
            acp_sessions,
            subscribers,
            before,
            usize::MAX,
            false,
        )
        .map_err(|error| match error {
            crate::acp_host::HistoryReadError::SessionMissing => gone(),
            crate::acp_host::HistoryReadError::Failed(error) => {
                ControlError::new(ControlErrorCode::OperationFailed, error)
            }
        })?;
        return Ok(ConversationPromptIndexResult {
            prompts: smelt_core::acp_chat::prompt_index(&snapshot.entries, snapshot.entries_offset),
        });
    }
    let needs_wake = acp_sessions
        .with_current(&session_id, &slot, |session| {
            let reduced = session.reduced.lock();
            reduced.entries_base() > 0 && before > 0 && session.idle.is_suspended()
        })
        .ok_or_else(gone)?;
    if needs_wake {
        crate::acp_idle::wake_suspended_acp_session(&session_id, &slot, acp_sessions, subscribers)
            .map_err(|error| ControlError::new(ControlErrorCode::OperationFailed, error))?;
    }
    acp_sessions
        .with_current(&session_id, &slot, |session| {
            let reduced = session.reduced.lock();
            let base = reduced.entries_base();
            let entries = reduced.entries();
            let end = before.min(base + entries.len()).saturating_sub(base);
            ConversationPromptIndexResult {
                prompts: smelt_core::acp_chat::prompt_index(&entries[..end], base),
            }
        })
        .ok_or_else(gone)
}
