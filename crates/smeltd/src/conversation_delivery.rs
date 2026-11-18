//! `conversation.prompt` / `conversation.turn_result`：插件以外部投递的身份驱动一段对话。
//!
//! 一轮是显式的：投递方带着自己的 `delivery_id` 发话，这一轮结束时会话状态的
//! `completed_delivery_id` 变成它，结果（怎么结束的、最后一段助手正文）在推状态之前就记在
//! 会话上（`acp_host::remember_delivery_result`），按 id 来取。对话本身是普通对话，人照样
//! 能看、能插话。

use smelt_core::acp_session::AcpUserAction;
use smelt_core::control_api::{
    ConversationPromptParams, ConversationTurnResultParams, ConversationTurnStatus,
};
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode};

use super::{AcpSessions, EventHubHandle};

fn error(code: ControlErrorCode, message: impl Into<String>) -> ControlError {
    ControlError::new(code, message)
}

fn required<'a>(value: &'a str, name: &str) -> Result<&'a str, ControlError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(error(
            ControlErrorCode::InvalidParams,
            format!("{name} can't be empty"),
        ));
    }
    Ok(value)
}

pub(crate) fn prompt(
    params: ConversationPromptParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(), ControlError> {
    let session_id = required(&params.session_id, "session_id")?;
    let delivery_id = required(&params.delivery_id, "delivery_id")?.to_string();
    if params.text.trim().is_empty() {
        return Err(error(
            ControlErrorCode::InvalidParams,
            "text can't be empty",
        ));
    }
    let slot = acp_sessions.get(session_id).ok_or_else(|| {
        error(
            ControlErrorCode::NotFound,
            format!("The runtime doesn't have this conversation: {session_id}"),
        )
    })?;
    // 限制在会话上、只活在这个进程里。对不上多半是运行时重启后续上的、不带限制的对话。
    if *slot.value.disallowed_tools.lock() != params.disallowed_tools {
        return Err(error(
            ControlErrorCode::FailedPrecondition,
            "This conversation's tool restrictions differ from the request (conversations resumed after a runtime restart have none)",
        ));
    }
    crate::acp_idle::wake_suspended_acp_session(session_id, &slot, acp_sessions, subscribers)
        .map_err(|message| error(ControlErrorCode::OperationFailed, message))?;
    // 刚拉起、还在续上历史的 runtime 先等它就绪：重放会重建对话，排在它前面的这一轮会被冲掉。
    crate::acp_idle::wait_for_acp_ready(&slot.value, crate::acp_idle::ACP_WAKE_READY_TIMEOUT)
        .map_err(|message| error(ControlErrorCode::OperationFailed, message))?;
    let action = AcpUserAction::Prompt {
        text: params.text,
        images: Vec::new(),
        delivery_id: Some(delivery_id),
    };
    acp_sessions
        .with_current(session_id, &slot, |session| {
            crate::acp_host::apply_acp_user_action(session, action, subscribers)
        })
        .ok_or_else(|| {
            error(
                ControlErrorCode::NotFound,
                format!("This conversation was just closed: {session_id}"),
            )
        })?
        .map_err(|message| error(ControlErrorCode::OperationFailed, message))
}

pub(crate) fn turn_result(
    params: ConversationTurnResultParams,
    acp_sessions: &AcpSessions,
) -> Result<ConversationTurnStatus, ControlError> {
    let session_id = required(&params.session_id, "session_id")?;
    let delivery_id = required(&params.delivery_id, "delivery_id")?;
    let Some(slot) = acp_sessions.get(session_id) else {
        return Ok(ConversationTurnStatus::Unknown);
    };
    let sess = &slot.value;
    // 先看收下没有，再看结果：结果在回合结束、推状态之前写，两处之间不会漏掉。
    let accepted = sess
        .reduced
        .lock()
        .accepted_delivery_ids
        .contains(delivery_id);
    if let Some(result) = sess
        .delivery_results
        .lock()
        .iter()
        .find(|result| result.delivery_id == delivery_id)
    {
        return Ok(ConversationTurnStatus::Finished {
            outcome: result.outcome,
            text: result.text.clone(),
        });
    }
    Ok(if accepted {
        ConversationTurnStatus::Pending
    } else {
        ConversationTurnStatus::Unknown
    })
}

#[cfg(test)]
mod tests {
    use smelt_core::acp_chat::AcpEntry;
    use smelt_core::acp_session::AcpTurnOutcome;

    use super::*;
    use crate::{make_acp_session, new_event_hub, new_test_acp_sessions};

    fn turn_result_of(acp_sessions: &AcpSessions, delivery_id: &str) -> ConversationTurnStatus {
        turn_result(
            ConversationTurnResultParams {
                session_id: "acp-driven".into(),
                delivery_id: delivery_id.into(),
            },
            acp_sessions,
        )
        .unwrap()
    }

    /// 收下了是 pending；回合结束记下结果后按 id 取到最后一段正文；之后人接着说话、
    /// 别的投递结束，都不影响按 id 取。
    #[test]
    fn a_delivery_is_pending_until_its_turn_finishes() {
        let acp_sessions = new_test_acp_sessions();
        assert_eq!(
            turn_result_of(&acp_sessions, "d1"),
            ConversationTurnStatus::Unknown
        );
        let (slot, _) = acp_sessions.reserve_with("acp-driven", || {
            make_acp_session("acp-driven", None, None, None)
        });
        let sess = &slot.value;
        assert_eq!(
            turn_result_of(&acp_sessions, "d1"),
            ConversationTurnStatus::Unknown
        );

        sess.reduced
            .lock()
            .accepted_delivery_ids
            .insert("d1".into());
        assert_eq!(
            turn_result_of(&acp_sessions, "d1"),
            ConversationTurnStatus::Pending
        );

        {
            let mut reduced = sess.reduced.lock();
            reduced.completed_delivery_id = Some("d1".into());
            reduced.turn_outcome = Some(AcpTurnOutcome::Succeeded);
            *reduced.entries_mut() = vec![
                AcpEntry::User("review".into()),
                AcpEntry::Assistant {
                    text: "想一想".into(),
                    thought: true,
                },
                AcpEntry::Assistant {
                    text: "{\"verdict\":\"approve\"}".into(),
                    thought: false,
                },
            ];
            crate::acp_host::remember_delivery_result(sess, &reduced);
        }
        let finished = ConversationTurnStatus::Finished {
            outcome: AcpTurnOutcome::Succeeded,
            text: "{\"verdict\":\"approve\"}".into(),
        };
        assert_eq!(turn_result_of(&acp_sessions, "d1"), finished);

        {
            let mut reduced = sess.reduced.lock();
            reduced.completed_delivery_id = None;
            reduced.turn_outcome = Some(AcpTurnOutcome::Succeeded);
            reduced
                .entries_mut()
                .push(AcpEntry::User("人插了一句".into()));
            crate::acp_host::remember_delivery_result(sess, &reduced);
            // 同一个 id 再走一遍状态同步，不会被后来的正文改写。
            reduced.completed_delivery_id = Some("d1".into());
            crate::acp_host::remember_delivery_result(sess, &reduced);
        }
        assert_eq!(turn_result_of(&acp_sessions, "d1"), finished);
        assert_eq!(sess.delivery_results.lock().len(), 1);
    }

    fn prompt_params(disallowed_tools: &[&str]) -> ConversationPromptParams {
        ConversationPromptParams {
            session_id: "acp-driven".into(),
            delivery_id: "d1".into(),
            text: "review PR #240".into(),
            disallowed_tools: disallowed_tools
                .iter()
                .map(|rule| rule.to_string())
                .collect(),
        }
    }

    /// 限制对不上（运行时重启后续上的对话不带限制）就不发，也不去唤醒它。
    #[test]
    fn a_prompt_needs_the_same_tool_restrictions_as_the_runtime() {
        let acp_sessions = new_test_acp_sessions();
        let hub = new_event_hub();
        let code = |params| prompt(params, &acp_sessions, &hub).unwrap_err().code;
        assert_eq!(code(prompt_params(&["Edit"])), ControlErrorCode::NotFound);

        let (slot, _) = acp_sessions.reserve_with("acp-driven", || {
            make_acp_session("acp-driven", None, None, None)
        });
        assert_eq!(
            code(prompt_params(&["Edit"])),
            ControlErrorCode::FailedPrecondition
        );
        *slot.value.disallowed_tools.lock() = vec!["Edit".into(), "Write".into()];
        assert_eq!(
            code(prompt_params(&[])),
            ControlErrorCode::FailedPrecondition
        );
        assert_eq!(
            code(prompt_params(&["Edit"])),
            ControlErrorCode::FailedPrecondition
        );
        assert!(slot.value.reduced.lock().accepted_delivery_ids.is_empty());

        let blank = ConversationPromptParams {
            delivery_id: " ".into(),
            ..prompt_params(&["Edit", "Write"])
        };
        assert_eq!(code(blank), ControlErrorCode::InvalidParams);
    }
}
