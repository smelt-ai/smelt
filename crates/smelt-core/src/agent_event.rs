//! CLI agent hooks 归一化后的稳定事件协议。
//!
//! provider 适配器只负责把各家的 hook 名称/字段翻译成这里的语义事件；smeltd
//! 再据此归约会话状态。协议带版本号，避免 helper 与守护升级不同步时静默误判。

pub use smelt_model::agent_event::*;

#[cfg(test)]
mod turn_event_tests {
    use super::{AGENT_EVENT_VERSION, AgentEvent, AgentEventKind, Occupancy};

    #[test]
    fn occupancy_is_the_kind_translation() {
        use Occupancy::*;
        let check = |kind: AgentEventKind, expected: Occupancy| {
            assert_eq!(kind.occupancy(), expected, "{kind:?}");
            assert_eq!(kind.is_turn_event(), expected.records_turn(), "{kind:?}");
        };
        check(AgentEventKind::SessionStarted, Metadata);
        check(AgentEventKind::SessionTitleChanged, Metadata);
        check(AgentEventKind::PromptSubmitted, OpenTurn);
        check(
            AgentEventKind::ToolStarted,
            ResumeWork {
                follow_matched_wait: true,
            },
        );
        check(
            AgentEventKind::SubagentStarted,
            ResumeWork {
                follow_matched_wait: false,
            },
        );
        check(
            AgentEventKind::ToolFinished,
            Progress {
                follow_matched_wait: true,
            },
        );
        check(
            AgentEventKind::ToolFailed,
            Progress {
                follow_matched_wait: true,
            },
        );
        check(
            AgentEventKind::SubagentStopped,
            Progress {
                follow_matched_wait: false,
            },
        );
        check(AgentEventKind::ApprovalRequested, Wait { approval: true });
        check(AgentEventKind::InputRequested, Wait { approval: false });
        check(AgentEventKind::TurnSucceeded, Finish { failed: false });
        check(AgentEventKind::TurnFailed, Finish { failed: true });
        check(AgentEventKind::SessionEnded, EndProcess);
    }

    #[test]
    fn v1_payload_without_conversation_title_remains_compatible() {
        let event: AgentEvent =
            serde_json::from_str(r#"{"version":1,"provider":"codex","kind":"prompt_submitted"}"#)
                .unwrap();
        assert_eq!(event.conversation_title, None);
    }

    #[test]
    fn only_the_new_title_event_requires_v2_on_the_wire() {
        assert_eq!(
            AgentEvent::new("codex", AgentEventKind::PromptSubmitted).version,
            1
        );
        assert_eq!(
            AgentEvent::new("opencode", AgentEventKind::SessionTitleChanged).version,
            AGENT_EVENT_VERSION
        );
    }
}
