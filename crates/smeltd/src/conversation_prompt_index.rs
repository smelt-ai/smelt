//! 要整段对话提问的两个读：`conversation.prompt_index`（提问导航）和
//! `conversation.fork_cut`（分叉切点）。
//!
//! 客户端只加载最近的几页，这两件事却要整段对话的每一问。这里在会话自己的条目上算好发过去，
//! 不拷整页条目。挂起的会话内存里只剩当前回合，前面的先唤醒再读，和 `acp_snapshot` 往上翻页
//! 同一条路。

use smelt_core::acp_chat::AcpEntry;
use smelt_core::acp_conn::AcpForkCut;
use smelt_core::control_api::{
    ConversationForkCutParams, ConversationForkCutResult, ConversationPromptIndexParams,
    ConversationPromptIndexResult, ForkSide,
};
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
    with_entries(
        &session_id,
        before > 0,
        acp_sessions,
        subscribers,
        |entries, base| {
            let end = before.min(base + entries.len()).saturating_sub(base);
            ConversationPromptIndexResult {
                prompts: smelt_core::acp_chat::prompt_index(&entries[..end], base),
            }
        },
    )
}

pub(crate) fn fork_cut(
    params: ConversationForkCutParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<ConversationForkCutResult, ControlError> {
    let ConversationForkCutParams {
        session_id,
        entry_index,
        side,
    } = params;
    let entry_index = usize::try_from(entry_index).unwrap_or(usize::MAX);
    let cut = with_entries(
        &session_id,
        true,
        acp_sessions,
        subscribers,
        |entries, base| {
            // 同文本序号要从头数；唤醒后还缺前缀就说不准，不给一个会切错地方的切点。
            if base > 0 {
                return Err(ControlError::new(
                    ControlErrorCode::OperationFailed,
                    "the start of the conversation isn't loaded",
                ));
            }
            Ok(cut_at(entries, entry_index, side))
        },
    )??;
    Ok(ConversationForkCutResult { cut })
}

fn cut_at(entries: &[AcpEntry], entry_index: usize, side: ForkSide) -> Option<AcpForkCut> {
    let (text, occurrence) = match side {
        ForkSide::After => smelt_core::acp_chat::fork_cut_after(entries, entry_index),
        ForkSide::Before => smelt_core::acp_chat::fork_cut_before(entries, entry_index),
    }?;
    Some(AcpForkCut { text, occurrence })
}

/// 在会话的条目上跑 `read`（条目和它前面裁掉的条数）。`needs_prefix` 时挂起裁过前缀的会话
/// 先唤醒，正在唤醒重放的等它放完。
fn with_entries<T>(
    session_id: &str,
    needs_prefix: bool,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
    read: impl FnOnce(&[AcpEntry], usize) -> T,
) -> Result<T, ControlError> {
    let gone =
        || ControlError::because(ControlErrorCode::NotFound, ControlReason::ConversationGone);
    let slot = acp_sessions.get(session_id).ok_or_else(gone)?;
    enum PrefixPrep {
        None,
        Wake,
        WaitReady,
    }
    let prep = acp_sessions
        .with_current(session_id, &slot, |session| {
            let reduced = session.reduced.lock();
            if reduced.entries_base() == 0 || !needs_prefix {
                PrefixPrep::None
            } else if session.idle.is_suspended() {
                PrefixPrep::Wake
            } else if crate::acp_idle::acp_ready_outcome(&reduced).is_none() {
                PrefixPrep::WaitReady
            } else {
                PrefixPrep::None
            }
        })
        .ok_or_else(gone)?;
    match prep {
        PrefixPrep::None => {}
        PrefixPrep::Wake => {
            crate::acp_idle::wake_suspended_acp_session(
                session_id,
                &slot,
                acp_sessions,
                subscribers,
            )
            .map_err(|error| ControlError::new(ControlErrorCode::OperationFailed, error))?;
        }
        PrefixPrep::WaitReady => {
            crate::acp_idle::wait_for_acp_ready(
                &slot.value,
                crate::acp_idle::ACP_WAKE_READY_TIMEOUT,
            )
            .map_err(|error| ControlError::new(ControlErrorCode::OperationFailed, error))?;
        }
    }
    acp_sessions
        .with_current(session_id, &slot, |session| {
            let reduced = session.reduced.lock();
            read(reduced.entries(), reduced.entries_base())
        })
        .ok_or_else(gone)
}
