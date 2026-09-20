use super::apply::recovered_elicitation;
use super::*;
use crate::acp_chat::{ToolCallStatus, ToolKind, ToolOutputPart};
use crate::acp_conn::{ConversationEvent, ElicitFieldKind, ReadyKind};
use agent_client_protocol::schema::v1::{
    StopReason, ToolCall, ToolCallStatus as AcpToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields, ToolKind as AcpToolKind,
};

fn fresh_state() -> AcpSessionState {
    AcpSessionState::default()
}

#[test]
fn protocol_title_overrides_prompt_fallback_and_clear_restores_it() {
    let mut state = fresh_state();
    note_prompt_sent(&mut state, "帮我排查登录接口为什么超时".into(), Vec::new());
    assert_eq!(
        state.to_snapshot(false).session_title.as_deref(),
        Some("帮我排查登录接口为什么超时")
    );

    apply_event(
        &mut state,
        ConversationEvent::SessionTitle(Some("定位登录接口超时".into())),
    );
    assert_eq!(
        state.to_snapshot(false).session_title.as_deref(),
        Some("定位登录接口超时"),
        "Agent 通过通用 ACP 上报的标题应优先于本地首条消息兜底"
    );
    apply_event(
        &mut state,
        ConversationEvent::UserChunk("帮我排查登录接口为什么超时".into()),
    );
    assert_eq!(
        state.entries.len(),
        1,
        "标题更新不能关闭 prompt 回声窗口，否则用户消息会重复"
    );

    apply_event(&mut state, ConversationEvent::SessionTitle(None));
    assert_eq!(
        state.to_snapshot(false).session_title.as_deref(),
        Some("帮我排查登录接口为什么超时"),
        "Agent 清空协议标题后仍应有稳定的本地兜底"
    );
}

#[test]
fn agent_chunk_appends_and_merges_consecutive_same_kind() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "hi".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::AgentChunk {
            thought: false,
            text: "he".into(),
            parent_id: None,
        },
    );
    apply_event(
        &mut s,
        ConversationEvent::AgentChunk {
            thought: false,
            text: "llo".into(),
            parent_id: None,
        },
    );
    assert_eq!(s.entries.len(), 2);
    assert!(
        matches!(&s.entries[1], AcpEntry::Assistant { text, thought: false } if text == "hello")
    );
    assert!(matches!(s.phase, DaemonPhase::Thinking));
}

#[test]
fn tool_started_with_the_same_id_updates_the_existing_card() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "run it".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "call-1".into(),
            title: "bash".into(),
            kind: ToolKind::Execute,
        },
    );
    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "call-1".into(),
            title: "git status".into(),
            kind: ToolKind::Execute,
        },
    );

    assert_eq!(s.entries.len(), 2);
    assert!(matches!(
        &s.entries[1],
        AcpEntry::ToolCall {
            id,
            title,
            kind: ToolKind::Execute,
            status: ToolCallStatus::InProgress,
            ..
        } if id == "call-1" && title == "git status"
    ));
}

#[test]
fn usage_keeps_tokens_when_only_cost_arrives() {
    let mut s = fresh_state();
    apply_event(
        &mut s,
        ConversationEvent::Usage {
            used: 60_000,
            size: 200_000,
            cached_read: Some(10_000),
            cost: None,
            breakdown: None,
        },
    );
    apply_event(
        &mut s,
        ConversationEvent::Usage {
            used: 0,
            size: 200_000,
            cached_read: Some(40_000),
            cost: Some(0.45),
            breakdown: None,
        },
    );
    assert_eq!(s.usage, Some((60_000, 200_000)));
    assert_eq!(s.usage_cached_read, Some(40_000));
    assert_eq!(s.usage_cost, Some(0.45));
}

#[test]
fn usage_breakdown_is_kept_when_totals_arrive_separately() {
    let mut s = fresh_state();
    let breakdown = crate::acp_conn::ContextUsageBreakdown {
        system_prompt: 2_000,
        tools_definition: 5_000,
        rules: 1_000,
        skills: 1_500,
        mcp_dynamic: 0,
        subagent: 0,
        summarized: 0,
        conversation: 1_500,
    };
    apply_event(
        &mut s,
        ConversationEvent::Usage {
            used: 0,
            size: 0,
            cached_read: None,
            cost: None,
            breakdown: Some(breakdown.clone()),
        },
    );
    apply_event(
        &mut s,
        ConversationEvent::Usage {
            used: 12_000,
            size: 1_000_000,
            cached_read: None,
            cost: None,
            breakdown: None,
        },
    );
    assert_eq!(s.usage, Some((12_000, 1_000_000)));
    assert_eq!(s.usage_breakdown, Some(breakdown));
    let aligned = s.usage_breakdown.unwrap().aligned_to_used(12_000);
    assert_eq!(aligned.occupied(), 12_000);
}

#[test]
fn user_echo_suppressed_once_after_prompt_sent() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "hi".into(), Vec::new());
    assert!(s.awaiting_user_echo);
    // 回声窗口内收到 UserChunk：吞掉，不重复追加。
    apply_event(&mut s, ConversationEvent::UserChunk("hi".into()));
    assert_eq!(s.entries.len(), 1);
    // 任何非 UserChunk/Status/AvailableCommands/Usage 事件都清掉等回声窗口。
    apply_event(
        &mut s,
        ConversationEvent::AgentChunk {
            thought: false,
            text: "ok".into(),
            parent_id: None,
        },
    );
    assert!(!s.awaiting_user_echo);
    // 窗口关闭后再来的 UserChunk 是重放历史，正常追加。
    apply_event(&mut s, ConversationEvent::UserChunk("old question".into()));
    assert_eq!(s.entries.len(), 3);
    assert!(matches!(&s.entries[2], AcpEntry::User(t) if t == "old question"));
}

#[test]
fn replayed_user_image_is_kept_with_its_text() {
    let mut s = fresh_state();
    apply_event(&mut s, ConversationEvent::UserChunk("看这里".into()));
    apply_event(
        &mut s,
        ConversationEvent::UserImage(crate::acp_chat::AcpImage {
            mime: "image/png".into(),
            data_b64: "QUJD".into(),
        }),
    );

    assert!(matches!(
        &s.entries[..],
        [AcpEntry::UserWithImages { text, images }]
            if text == "看这里" && images.len() == 1
    ));
}

#[test]
fn ready_does_not_erase_replay_started_before_buffered_updates() {
    let mut s = fresh_state();
    s.entries.push(AcpEntry::User("old".into()));
    s.history_session_id = Some("canonical-history".into());
    apply_event(&mut s, ConversationEvent::HistoryReplayStarted);
    assert!(s.entries.is_empty());
    apply_event(
        &mut s,
        ConversationEvent::Ready {
            session_id: agent_client_protocol::schema::v1::SessionId::new("runtime-session"),
            kind: ReadyKind::ResumedWithReplay,
            supports_image: true,
        },
    );
    apply_event(
        &mut s,
        ConversationEvent::UserChunk("replayed question".into()),
    );
    assert!(matches!(
        &s.entries[..],
        [AcpEntry::User(text)] if text == "replayed question"
    ));
    assert_eq!(s.acp_session_id.as_deref(), Some("runtime-session"));
    assert_eq!(s.history_session_id.as_deref(), Some("canonical-history"));
}

#[test]
fn replayed_agent_updates_do_not_leave_session_running() {
    let mut s = fresh_state();
    apply_event(&mut s, ConversationEvent::HistoryReplayStarted);
    apply_event(
        &mut s,
        ConversationEvent::Ready {
            session_id: agent_client_protocol::schema::v1::SessionId::new("sid-1"),
            kind: ReadyKind::ResumedWithReplay,
            supports_image: true,
        },
    );
    apply_event(
        &mut s,
        ConversationEvent::AgentChunk {
            text: "old answer".into(),
            thought: false,
            parent_id: None,
        },
    );

    assert!(matches!(s.phase, DaemonPhase::Idle));

    note_prompt_sent(&mut s, "continue".into(), Vec::new());
    assert!(matches!(s.phase, DaemonPhase::Thinking));
    assert!(!s.replaying_history);
}

#[test]
fn load_replay_rebuilds_legacy_projection_without_duplicates() {
    let mut s = fresh_state();
    s.entries.push(AcpEntry::User("old question".into()));
    s.entries.push(AcpEntry::Assistant {
        text: "old answer".into(),
        thought: false,
    });

    apply_event(&mut s, ConversationEvent::HistoryReplayStarted);
    apply_event(&mut s, ConversationEvent::UserChunk("old question".into()));
    apply_event(
        &mut s,
        ConversationEvent::AgentChunk {
            text: "old answer".into(),
            thought: false,
            parent_id: None,
        },
    );
    apply_event(
        &mut s,
        ConversationEvent::Ready {
            session_id: agent_client_protocol::schema::v1::SessionId::new("sid-1"),
            kind: ReadyKind::ResumedWithReplay,
            supports_image: true,
        },
    );

    assert_eq!(s.entries.len(), 2);
    assert!(matches!(&s.entries[0], AcpEntry::User(text) if text == "old question"));
    assert!(matches!(
        &s.entries[1],
        AcpEntry::Assistant { text, thought: false } if text == "old answer"
    ));
}

#[test]
fn ready_resumed_keep_history_preserves_local_entries() {
    let mut s = fresh_state();
    s.entries.push(AcpEntry::User("old".into()));
    apply_event(
        &mut s,
        ConversationEvent::Ready {
            session_id: agent_client_protocol::schema::v1::SessionId::new("sid-1"),
            kind: ReadyKind::ResumedKeepHistory,
            supports_image: true,
        },
    );
    assert_eq!(s.entries.len(), 1);
}

#[test]
fn ready_fresh_replaces_id_and_preserves_legacy_history_with_divider() {
    let mut s = fresh_state();
    s.entries.push(AcpEntry::User("old".into()));
    s.acp_session_id = Some("old-sid".into());
    apply_event(&mut s, ConversationEvent::Status("正在恢复".into()));
    assert_eq!(s.acp_session_id.as_deref(), Some("old-sid"));

    apply_event(
        &mut s,
        ConversationEvent::Ready {
            session_id: agent_client_protocol::schema::v1::SessionId::new("new-sid"),
            kind: ReadyKind::Fresh,
            supports_image: true,
        },
    );
    assert_eq!(s.acp_session_id.as_deref(), Some("new-sid"));
    assert_eq!(s.history_session_id.as_deref(), Some("new-sid"));
    assert!(s.status_line.is_none());
    assert_eq!(s.entries.len(), 2);
    assert!(matches!(&s.entries[0], AcpEntry::User(text) if text == "old"));
    assert!(matches!(s.entries[1], AcpEntry::Divider(_)));
}

#[test]
fn ready_fresh_blank_session_does_not_create_history_id() {
    let mut s = fresh_state();

    apply_event(
        &mut s,
        ConversationEvent::Ready {
            session_id: agent_client_protocol::schema::v1::SessionId::new("new-sid"),
            kind: ReadyKind::Fresh,
            supports_image: true,
        },
    );

    assert_eq!(s.acp_session_id.as_deref(), Some("new-sid"));
    assert!(s.history_session_id.is_none());
}

#[test]
fn first_prompt_after_ready_makes_blank_session_resumable() {
    let mut s = fresh_state();
    apply_event(
        &mut s,
        ConversationEvent::Ready {
            session_id: agent_client_protocol::schema::v1::SessionId::new("new-sid"),
            kind: ReadyKind::Fresh,
            supports_image: true,
        },
    );

    note_prompt_sent(&mut s, "hello".into(), Vec::new());

    assert_eq!(s.history_session_id.as_deref(), Some("new-sid"));
}

#[test]
fn ready_keeps_a_pre_ready_prompt_running_without_a_divider() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "你好".into(), Vec::new());
    let started_at = s.turn_started_at_ms;

    apply_event(
        &mut s,
        ConversationEvent::Ready {
            session_id: agent_client_protocol::schema::v1::SessionId::new("new-sid"),
            kind: ReadyKind::Fresh,
            supports_image: true,
        },
    );

    assert!(matches!(s.phase, DaemonPhase::Thinking));
    assert_eq!(s.turn_started_at_ms, started_at);
    assert!(s.awaiting_user_echo);
    assert!(matches!(&s.entries[..], [AcpEntry::User(text)] if text == "你好"));

    // session/new 完成后才出现的用户回声也必须吞掉，不能把首条消息重复一遍。
    apply_event(&mut s, ConversationEvent::UserChunk("你好".into()));
    assert!(matches!(&s.entries[..], [AcpEntry::User(text)] if text == "你好"));
}

#[test]
fn ready_fresh_does_not_replace_preseeded_history_id() {
    let mut s = fresh_state();
    s.history_session_id = Some("canonical-history".into());

    apply_event(
        &mut s,
        ConversationEvent::Ready {
            session_id: agent_client_protocol::schema::v1::SessionId::new("new-runtime"),
            kind: ReadyKind::Fresh,
            supports_image: true,
        },
    );

    assert_eq!(s.acp_session_id.as_deref(), Some("new-runtime"));
    assert_eq!(s.history_session_id.as_deref(), Some("canonical-history"));
}

#[test]
fn acp_subagent_tool_shapes_are_normalized_to_collaboration() {
    let codex_meta = serde_json::json!({
        "codex": {
            "subagent": {
                "threadId": "child-1",
                "path": "/root/explorer"
            }
        }
    })
    .as_object()
    .expect("meta fixture should be an object")
    .clone();
    let calls = [
        ToolCall::new("subagent-1", "Start subagent explorer")
            .kind(AcpToolKind::Other)
            .meta(codex_meta.clone()),
        ToolCall::new("subagent-2", "Task")
            .kind(AcpToolKind::Other)
            .raw_input(serde_json::json!({
                "subagent_type": "Explore",
                "description": "Inspect the ACP pipeline",
                "prompt": "Find where updates are dropped"
            })),
    ];

    for call in calls {
        let mut state = fresh_state();
        apply_event(&mut state, ConversationEvent::ToolCall(call));
        assert!(matches!(
            state.entries.as_slice(),
            [AcpEntry::ToolCall {
                kind: ToolKind::Collaborate,
                ..
            }]
        ));
    }

    let mut described = fresh_state();
    apply_event(
        &mut described,
        ConversationEvent::ToolCall(
            ToolCall::new("subagent-2", "Task")
                .kind(AcpToolKind::Other)
                .raw_input(serde_json::json!({
                    "subagent_type": "Explore",
                    "description": "Inspect the ACP pipeline",
                    "prompt": "Find where updates are dropped"
                })),
        ),
    );
    match described.entries.as_slice() {
        [
            AcpEntry::ToolCall {
                title,
                kind: ToolKind::Collaborate,
                ..
            },
        ] => assert_eq!(title, "Inspect the ACP pipeline"),
        other => panic!("expected described subagent title, got {other:?}"),
    }

    let mut ordinary = fresh_state();
    apply_event(
        &mut ordinary,
        ConversationEvent::ToolCall(
            ToolCall::new("ordinary", "Query agent record")
                .kind(AcpToolKind::Other)
                .raw_input(serde_json::json!({"agent_id": "record-1"})),
        ),
    );
    assert!(matches!(
        ordinary.entries.as_slice(),
        [AcpEntry::ToolCall {
            kind: ToolKind::Other,
            ..
        }]
    ));

    let mut updated = fresh_state();
    apply_event(
        &mut updated,
        ConversationEvent::ToolCall(
            ToolCall::new("subagent-update", "background task").kind(AcpToolKind::Other),
        ),
    );
    apply_event(
        &mut updated,
        ConversationEvent::ToolCallUpdate(
            ToolCallUpdate::new(
                "subagent-update",
                ToolCallUpdateFields::new().status(AcpToolCallStatus::Completed),
            )
            .meta(codex_meta),
        ),
    );
    assert!(matches!(
        updated.entries.as_slice(),
        [AcpEntry::ToolCall {
            kind: ToolKind::Collaborate,
            status: ToolCallStatus::Completed,
            ..
        }]
    ));
}

#[test]
fn nested_subagent_updates_attach_to_parent_tool() {
    let mut parent_meta = serde_json::Map::new();
    let mut claude = serde_json::Map::new();
    claude.insert("subagent".into(), serde_json::Value::Bool(true));
    parent_meta.insert("claudeCode".into(), serde_json::Value::Object(claude));

    let mut child_meta = serde_json::Map::new();
    let mut child_claude = serde_json::Map::new();
    child_claude.insert(
        "parentToolUseId".into(),
        serde_json::Value::String("agent-1".into()),
    );
    child_meta.insert("claudeCode".into(), serde_json::Value::Object(child_claude));

    let mut s = fresh_state();
    note_prompt_sent(&mut s, "explore".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::ToolCall(
            ToolCall::new("agent-1", "Task")
                .kind(AcpToolKind::Other)
                .meta(parent_meta),
        ),
    );
    apply_event(
        &mut s,
        ConversationEvent::AgentChunk {
            thought: true,
            text: "先看目录".into(),
            parent_id: Some("agent-1".into()),
        },
    );
    apply_event(
        &mut s,
        ConversationEvent::ToolCall(
            ToolCall::new("read-1", "Read README")
                .kind(AcpToolKind::Read)
                .meta(child_meta),
        ),
    );
    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::EndTurn));

    assert_eq!(s.entries.len(), 2, "子代理事件不应顶到主对话");
    let AcpEntry::ToolCall {
        id, kind, children, ..
    } = &s.entries[1]
    else {
        panic!("parent should be a tool call");
    };
    assert_eq!(id, "agent-1");
    assert_eq!(*kind, ToolKind::Collaborate);
    assert!(matches!(
        children.as_slice(),
        [
            AcpEntry::Assistant {
                thought: true,
                ..
            },
            AcpEntry::ToolCall { id, .. }
        ] if id == "read-1"
    ));
    assert!(crate::acp_chat::has_unfinished_tool_call(&s.entries));
    assert_eq!(finalize_dangling_tool_calls(&mut s), None);
    assert!(crate::acp_chat::has_unfinished_tool_call(&s.entries));
}

#[test]
fn tool_children_replace_the_nested_transcript() {
    let mut s = fresh_state();
    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "sa-1".into(),
            title: "scout".into(),
            kind: ToolKind::Collaborate,
        },
    );
    apply_event(
        &mut s,
        ConversationEvent::ToolChildren {
            id: "sa-1".into(),
            children: vec![AcpEntry::Assistant {
                text: "looking".into(),
                thought: true,
            }],
        },
    );
    let AcpEntry::ToolCall { children, .. } = &s.entries[0] else {
        panic!("parent should be a tool call");
    };
    assert!(matches!(
        children.as_slice(),
        [AcpEntry::Assistant {
            thought: true,
            text
        }] if text == "looking"
    ));
}

#[test]
fn dangling_tool_calls_are_finalized_after_turn_ended() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "do it".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "shell".into(),
            title: "Reading shell output".into(),
            kind: ToolKind::Execute,
        },
    );
    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::EndTurn));
    assert!(crate::acp_chat::has_unfinished_tool_call(&s.entries));

    assert_eq!(finalize_dangling_tool_calls(&mut s), Some(1));
    assert!(!crate::acp_chat::has_unfinished_tool_call(&s.entries));
    assert!(s.completed_unread);
    // 已经收尾过就没有可改的了，重复调用不再产生广播。
    assert_eq!(finalize_dangling_tool_calls(&mut s), None);
}

#[test]
fn running_turn_keeps_its_unfinished_tool_calls() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "do it".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "shell".into(),
            title: "Reading shell output".into(),
            kind: ToolKind::Execute,
        },
    );

    assert_eq!(finalize_dangling_tool_calls(&mut s), None);
    assert!(crate::acp_chat::has_unfinished_tool_call(&s.entries));
}

#[test]
fn pending_permission_defers_dangling_tool_finalization() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "do it".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "shell".into(),
            title: "Reading shell output".into(),
            kind: ToolKind::Execute,
        },
    );
    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::EndTurn));
    s.permissions.push(LivePermission {
        tool_call_id: "shell".into(),
        question: "允许执行？".into(),
        options: Vec::new(),
        details: ApprovalDetailsView::default(),
        responder: None,
        raw_request_line: None,
    });

    assert_eq!(finalize_dangling_tool_calls(&mut s), None);
}

#[test]
fn turn_ended_clears_pending_cards_and_marks_unread() {
    let mut s = fresh_state();
    s.phase = DaemonPhase::Thinking;
    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::EndTurn));
    assert!(matches!(s.phase, DaemonPhase::Idle));
    assert!(s.completed_unread);
    assert_eq!(s.turn_outcome, Some(AcpTurnOutcome::Succeeded));
}

#[test]
fn delivery_identity_follows_turn_end_and_handoff_snapshot() {
    let mut state = fresh_state();
    state.accepted_delivery_ids.insert("run-1".into());
    note_prompt_sent_with_delivery(&mut state, "do it".into(), Vec::new(), Some("run-1".into()));
    assert_eq!(state.active_delivery_id.as_deref(), Some("run-1"));
    assert!(state.completed_delivery_id.is_none());

    apply_event(
        &mut state,
        ConversationEvent::TurnEnded(StopReason::EndTurn),
    );
    assert!(state.active_delivery_id.is_none());
    assert_eq!(state.completed_delivery_id.as_deref(), Some("run-1"));

    let snapshot: ConversationSnapshot =
        serde_json::from_str(&serde_json::to_string(&state.to_snapshot(true)).unwrap()).unwrap();
    let restored = AcpSessionState::from_snapshot(snapshot);
    assert!(restored.accepted_delivery_ids.contains("run-1"));
    assert!(restored.active_delivery_id.is_none());
    assert_eq!(restored.completed_delivery_id.as_deref(), Some("run-1"));
}

#[test]
fn snapshot_wire_keeps_legacy_phase_names() {
    let mut starting = AcpSessionState::default();
    assert_eq!(starting.phase, DaemonPhase::Connecting);
    let value = serde_json::to_value(starting.to_snapshot(true)).unwrap();
    assert_eq!(value["phase"], "Starting");

    force_end(
        &mut starting,
        AcpEndKind::TransportDisconnected,
        "连接意外中断",
    );
    let ended = serde_json::to_value(starting.to_snapshot(true)).unwrap();
    assert_eq!(ended["phase"]["Ended"], "连接意外中断");

    let parsed: ConversationSnapshot = serde_json::from_value(serde_json::json!({
        "entries": [],
        "phase": {"Ended": "超时"},
        "pending_elicitation": null,
        "supports_image": true,
        "available_commands": [],
        "config_options": [],
        "completed_unread": false,
        "should_persist": true,
    }))
    .unwrap();
    assert_eq!(parsed.phase, DaemonPhase::Dead);
    assert_eq!(parsed.end_reason, "超时");

    let choice: ConversationSnapshot = serde_json::from_value(serde_json::json!({
        "entries": [],
        "phase": "AwaitingChoice",
        "pending_elicitation": null,
        "supports_image": true,
        "available_commands": [],
        "config_options": [],
        "completed_unread": false,
        "should_persist": true,
    }))
    .unwrap();
    assert_eq!(choice.phase, DaemonPhase::WaitingForUser);
}

#[test]
fn cancelled_turn_finishes_its_unfinished_tools() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "ask another agent".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "ask-agent".into(),
            title: "smelt-ask-agent".into(),
            kind: ToolKind::Wait,
        },
    );

    let outcome = apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::Cancelled));

    assert_eq!(outcome.entries_offset, Some(1));
    assert!(matches!(s.phase, DaemonPhase::Idle));
    assert_eq!(s.turn_outcome, Some(AcpTurnOutcome::Cancelled));
    assert!(matches!(
        &s.entries[1],
        AcpEntry::ToolCall {
            status: ToolCallStatus::Failed,
            output,
            ..
        } if matches!(output.last(), Some(ToolOutputPart::Text(text)) if text == "已因用户停止而取消")
    ));
}

#[test]
fn cancelled_turn_does_not_reopen_for_a_late_tool_notification() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "stop it".into(), Vec::new());
    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::Cancelled));

    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "late-tool".into(),
            title: "smelt-ask-agent".into(),
            kind: ToolKind::Wait,
        },
    );

    assert!(matches!(s.phase, DaemonPhase::Idle));
    assert!(matches!(
        &s.entries[1],
        AcpEntry::ToolCall {
            status: ToolCallStatus::Failed,
            ..
        }
    ));
}

#[test]
fn next_turn_can_start_a_new_tool_after_the_previous_turn_was_cancelled() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "stop it".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "old-tool".into(),
            title: "smelt-ask-agent".into(),
            kind: ToolKind::Wait,
        },
    );
    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::Cancelled));

    note_prompt_sent(&mut s, "continue".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "new-tool".into(),
            title: "read_file".into(),
            kind: ToolKind::Read,
        },
    );

    assert!(matches!(
        s.entries.last(),
        Some(AcpEntry::ToolCall {
            id,
            status: ToolCallStatus::InProgress,
            ..
        }) if id == "new-tool"
    ));
    assert_eq!(s.turn_seq, 2);
    assert_eq!(s.cancelled_turn_seq, Some(1));
}

#[test]
fn turn_seq_increments_on_each_prompt() {
    let mut s = fresh_state();
    assert_eq!(s.turn_seq, 0);
    note_prompt_sent(&mut s, "one".into(), Vec::new());
    assert_eq!(s.turn_seq, 1);
    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::EndTurn));
    note_prompt_sent(&mut s, "two".into(), Vec::new());
    assert_eq!(s.turn_seq, 2);
}

#[test]
fn finish_turn_records_elapsed_against_the_prompt_user_entry() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "hello".into(), Vec::new());
    assert_eq!(s.turn_timings.len(), 1);
    assert_eq!(s.turn_timings[0].user_index, 0);
    assert!(s.turn_timings[0].ended_at_ms.is_none());

    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::EndTurn));

    assert!(s.turn_timings[0].ended_at_ms.is_some());
    assert!(s.turn_timings[0].completed_elapsed_ms().is_some());
    assert!(s.turn_started_at_ms.is_none());
}

#[test]
fn unknown_tool_permission_after_next_turn_is_ignored() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "stop it".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "old-tool".into(),
            title: "write".into(),
            kind: ToolKind::Edit,
        },
    );
    note_cancel_requested(&mut s);
    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::Cancelled));
    note_prompt_sent(&mut s, "continue".into(), Vec::new());

    apply_event(&mut s, late_permission_event("ghost-tool"));

    assert!(matches!(s.phase, DaemonPhase::Thinking));
    assert!(s.permissions.is_empty());
}

#[test]
fn cancelled_turn_finishes_leftover_tools_from_earlier_turns() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "first".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "old-tool".into(),
            title: "stale".into(),
            kind: ToolKind::Wait,
        },
    );
    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::EndTurn));
    assert!(crate::acp_chat::has_unfinished_tool_call(&s.entries));

    note_prompt_sent(&mut s, "second".into(), Vec::new());
    note_cancel_requested(&mut s);
    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::Cancelled));

    assert!(!crate::acp_chat::has_unfinished_tool_call(&s.entries));
    assert!(matches!(
        &s.entries[1],
        AcpEntry::ToolCall {
            id,
            status: ToolCallStatus::Failed,
            ..
        } if id == "old-tool"
    ));
}

#[test]
fn next_prompt_finalizes_leftover_tools_from_the_previous_turn() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "first".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "stale".into(),
            title: "read".into(),
            kind: ToolKind::Read,
        },
    );
    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::EndTurn));
    assert!(crate::acp_chat::has_unfinished_tool_call(&s.entries));

    note_prompt_sent(&mut s, "second".into(), Vec::new());

    assert!(matches!(
        &s.entries[1],
        AcpEntry::ToolCall {
            id,
            status: ToolCallStatus::Failed,
            ..
        } if id == "stale"
    ));
    assert!(matches!(s.entries.last(), Some(AcpEntry::User(text)) if text == "second"));
}

#[test]
fn requested_cancel_finishes_tools_even_when_adapter_reports_end_turn() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "stop it".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "ask-agent".into(),
            title: "smelt-ask-agent".into(),
            kind: ToolKind::Wait,
        },
    );
    note_cancel_requested(&mut s);

    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::EndTurn));

    assert_eq!(s.turn_outcome, Some(AcpTurnOutcome::Cancelled));

    assert!(matches!(
        &s.entries[1],
        AcpEntry::ToolCall {
            status: ToolCallStatus::Failed,
            ..
        }
    ));
}

fn late_permission_event(tool_call_id: &str) -> ConversationEvent {
    ConversationEvent::Permission {
        question: "允许执行？".into(),
        tool_call_id: agent_client_protocol::schema::v1::ToolCallId::new(tool_call_id),
        pub_options: vec![PermissionOptionView {
            option_id: "allow".into(),
            name: "Allow".into(),
            kind: PermissionOptionKindView::AllowOnce,
        }],
        responder: PermissionResponder::external(|_| {}),
        details: ApprovalDetailsView::Generic,
        raw_request_line: None,
    }
}

#[test]
fn cancel_request_dismisses_pending_approval_cards() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "do it".into(), Vec::new());
    apply_event(&mut s, late_permission_event("tool-1"));
    assert!(matches!(s.phase, DaemonPhase::AwaitingApproval));
    assert_eq!(s.permissions.len(), 1);

    note_cancel_requested(&mut s);

    assert!(s.permissions.is_empty());
    assert!(s.elicitation.is_none());
    assert!(matches!(s.phase, DaemonPhase::Thinking));
}

#[test]
fn late_permission_after_cancel_does_not_reopen_approval() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "do it".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "tool-1".into(),
            title: "write".into(),
            kind: ToolKind::Edit,
        },
    );
    note_cancel_requested(&mut s);
    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::Cancelled));

    apply_event(&mut s, late_permission_event("tool-1"));

    assert!(matches!(s.phase, DaemonPhase::Idle));
    assert!(s.permissions.is_empty());
    assert_eq!(s.turn_outcome, Some(AcpTurnOutcome::Cancelled));
}

#[test]
fn late_permission_for_cancelled_tool_does_not_attach_to_next_turn() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "stop it".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "tool-1".into(),
            title: "write".into(),
            kind: ToolKind::Edit,
        },
    );
    note_cancel_requested(&mut s);
    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::Cancelled));

    note_prompt_sent(&mut s, "continue".into(), Vec::new());
    apply_event(&mut s, late_permission_event("tool-1"));

    assert!(matches!(s.phase, DaemonPhase::Thinking));
    assert!(s.permissions.is_empty());
}

#[test]
fn late_agent_chunk_after_cancel_is_dropped_before_next_prompt() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "stop it".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::AgentChunk {
            thought: false,
            text: "先说一半".into(),
            parent_id: None,
        },
    );
    note_cancel_requested(&mut s);
    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::Cancelled));

    apply_event(
        &mut s,
        ConversationEvent::AgentChunk {
            thought: false,
            text: "取消后的迟到正文".into(),
            parent_id: None,
        },
    );

    assert!(matches!(s.phase, DaemonPhase::Idle));
    assert!(matches!(
        &s.entries[1],
        AcpEntry::Assistant { text, thought: false } if text == "先说一半"
    ));
}

#[test]
fn stop_reasons_preserve_success_cancel_and_failure_semantics() {
    for (reason, expected) in [
        (StopReason::EndTurn, AcpTurnOutcome::Succeeded),
        (StopReason::Cancelled, AcpTurnOutcome::Cancelled),
        (StopReason::MaxTokens, AcpTurnOutcome::MaxTokens),
        (StopReason::MaxTurnRequests, AcpTurnOutcome::MaxTurnRequests),
        (StopReason::Refusal, AcpTurnOutcome::Refused),
    ] {
        let mut state = fresh_state();
        note_prompt_sent(&mut state, "run".into(), Vec::new());
        apply_event(&mut state, ConversationEvent::TurnEnded(reason));
        assert_eq!(state.turn_outcome, Some(expected));
        assert!(state.completed_unread);
    }
}

#[test]
fn late_task_complete_after_turn_ended_does_not_reopen_turn() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "do it".into(), Vec::new());
    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::EndTurn));
    assert!(matches!(s.phase, DaemonPhase::Idle));

    apply_event(
        &mut s,
        ConversationEvent::ToolCall(
            agent_client_protocol::schema::v1::ToolCall::new("done", "task_complete")
                .kind(agent_client_protocol::schema::v1::ToolKind::Other)
                .status(agent_client_protocol::schema::v1::ToolCallStatus::Completed),
        ),
    );

    assert!(matches!(s.phase, DaemonPhase::Idle));
    assert!(s.completed_unread);
}

#[test]
fn ordinary_tool_during_active_turn_keeps_turn_running() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "read it".into(), Vec::new());
    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "read".into(),
            title: "read_file".into(),
            kind: ToolKind::Read,
        },
    );

    apply_event(
        &mut s,
        ConversationEvent::ToolFinished {
            id: "read".into(),
            status: ToolCallStatus::Completed,
            output: Vec::new(),
        },
    );

    assert!(matches!(s.phase, DaemonPhase::Thinking));
    assert!(!s.completed_unread);
}

#[test]
fn snapshot_restore_repairs_running_without_an_active_turn() {
    let mut s = fresh_state();
    s.phase = DaemonPhase::Thinking;
    s.turn_started_at_ms = None;
    s.completed_unread = true;

    let restored = AcpSessionState::from_snapshot(s.to_snapshot(false));

    assert!(matches!(restored.phase, DaemonPhase::Idle));
    assert!(restored.completed_unread);
}

#[test]
fn selecting_one_permission_keeps_other_requests_visible() {
    let mut s = fresh_state();
    s.permissions = vec![
        LivePermission {
            question: "first".into(),
            tool_call_id: "tool-1".into(),
            options: vec![PermissionOptionView {
                option_id: "allow-first".into(),
                name: "Allow".into(),
                kind: PermissionOptionKindView::AllowOnce,
            }],
            details: ApprovalDetailsView::Generic,
            responder: None,
            raw_request_line: None,
        },
        LivePermission {
            question: "second".into(),
            tool_call_id: "tool-2".into(),
            options: vec![PermissionOptionView {
                option_id: "allow-second".into(),
                name: "Allow".into(),
                kind: PermissionOptionKindView::AllowOnce,
            }],
            details: ApprovalDetailsView::Generic,
            responder: None,
            raw_request_line: None,
        },
    ];
    s.phase = DaemonPhase::AwaitingApproval;

    select_permission(&mut s, "tool-1", "allow-first");

    assert_eq!(s.permissions.len(), 1);
    assert_eq!(s.permissions[0].tool_call_id, "tool-2");
    assert!(matches!(s.phase, DaemonPhase::AwaitingApproval));
}

#[test]
fn permission_selection_needs_the_matching_tool_call() {
    let mut s = fresh_state();
    for tool_call_id in ["tool-1", "tool-2"] {
        s.permissions.push(LivePermission {
            question: tool_call_id.into(),
            tool_call_id: tool_call_id.into(),
            options: vec![PermissionOptionView {
                option_id: "allow".into(),
                name: "Allow".into(),
                kind: PermissionOptionKindView::AllowOnce,
            }],
            details: ApprovalDetailsView::Generic,
            responder: None,
            raw_request_line: None,
        });
    }

    select_permission(&mut s, "tool-2", "allow");

    assert_eq!(s.permissions.len(), 1);
    assert_eq!(s.permissions[0].tool_call_id, "tool-1");
}

#[test]
fn streaming_updates_keep_awaiting_approval_while_a_card_is_pending() {
    // 回归：agent 在发出审批请求后继续推流（并行工具、思考片段、plan 刷新
    // 都会），相位一旦掉回 Running，smeltd 的四色相位就变成
    // Thinking/ExecutingTool，移动端会把这条会话的行动项判成已解决，
    // 提醒闪一下就没了——但请求其实还挂着。
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "hi".into(), Vec::new());
    s.permissions.push(LivePermission {
        question: "允许写文件？".into(),
        tool_call_id: "tool-1".into(),
        options: vec![PermissionOptionView {
            option_id: "allow".into(),
            name: "Allow".into(),
            kind: PermissionOptionKindView::AllowOnce,
        }],
        details: ApprovalDetailsView::Generic,
        responder: None,
        raw_request_line: None,
    });
    s.phase = DaemonPhase::AwaitingApproval;

    apply_event(
        &mut s,
        ConversationEvent::AgentChunk {
            thought: false,
            text: "还在想".into(),
            parent_id: None,
        },
    );
    assert!(matches!(s.phase, DaemonPhase::AwaitingApproval));

    apply_event(
        &mut s,
        ConversationEvent::ToolStarted {
            id: "tool-2".into(),
            title: "并行工具".into(),
            kind: crate::acp_chat::ToolKind::Other,
        },
    );
    assert!(matches!(s.phase, DaemonPhase::AwaitingApproval));

    select_permission(&mut s, "tool-1", "allow");
    assert!(matches!(s.phase, DaemonPhase::Thinking));
}

/// 回合失败 != 会话结束。曾经 `session/prompt` 的错误响应会一路拖垮连接，
/// 「没配 API key」这种一句话能改好的问题直接表现为会话猝死、输入框消失。
#[test]
fn turn_failed_keeps_session_alive_and_shows_reason() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "你好".into(), Vec::new());
    assert!(matches!(s.phase, DaemonPhase::Thinking));

    apply_event(
        &mut s,
        ConversationEvent::TurnFailed("llm-deepseek: no API key".into()),
    );

    assert!(
        matches!(s.phase, DaemonPhase::Idle),
        "回合失败后会话必须还能继续用，不能进 Ended：{:?}",
        s.phase
    );
    assert_eq!(s.turn_outcome, Some(AcpTurnOutcome::Failed));
    let last = s.entries.last().expect("失败原因必须留在消息流里");
    match last {
        AcpEntry::Assistant { text, thought } => {
            assert!(!thought, "失败原因不能藏进思考块（默认折叠就等于没显示）");
            assert!(
                text.contains("no API key"),
                "错误原文必须原样带给用户：{text}"
            );
        }
        other => panic!("失败原因没落进消息流：{other:?}"),
    }
    // 失败要能作为提醒冒出来，不能静默。
    assert!(s.turn_outcome.unwrap().failure_message().is_some());
}

/// 失败的回合不能把「等回合结束」的闸门永久卡住——下一条 prompt 要能发。
#[test]
fn turn_failed_allows_the_next_prompt() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "第一次".into(), Vec::new());
    apply_event(&mut s, ConversationEvent::TurnFailed("boom".into()));

    note_prompt_sent(&mut s, "重试".into(), Vec::new());
    assert!(matches!(s.phase, DaemonPhase::Thinking));
    apply_event(
        &mut s,
        ConversationEvent::AgentChunk {
            thought: false,
            text: "好的".into(),
            parent_id: None,
        },
    );
    apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::EndTurn));
    assert_eq!(s.turn_outcome, Some(AcpTurnOutcome::Succeeded));
}

/// 用户已经按了取消，之后才收到 agent 的错误响应：那是取消的副作用，
/// 不该报成一次失败。
#[test]
fn turn_failed_after_cancel_counts_as_cancelled() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "你好".into(), Vec::new());
    s.cancel_requested = true;

    apply_event(&mut s, ConversationEvent::TurnFailed("aborted".into()));

    assert_eq!(s.turn_outcome, Some(AcpTurnOutcome::Cancelled));
    assert!(matches!(s.phase, DaemonPhase::Idle));
}

#[test]
fn fatal_ends_session_and_keeps_reason() {
    let mut s = fresh_state();
    s.acp_session_id = Some("old-sid".into());
    s.entries.push(AcpEntry::User("old".into()));
    let message = "恢复失败，可重试：temporary failure";

    apply_event(&mut s, ConversationEvent::Fatal(message.into()));

    assert_eq!(s.acp_session_id.as_deref(), Some("old-sid"));
    assert_eq!(s.entries.len(), 1);
    assert!(matches!(&s.entries[0], AcpEntry::User(text) if text == "old"));
    assert_eq!(s.phase, DaemonPhase::Dead);
    assert_eq!(s.end_reason, message);
    assert_eq!(s.end_kind, AcpEndKind::ProviderFailed);
}

#[test]
fn force_end_sets_kind_and_clears_pending_cards() {
    let mut s = fresh_state();
    note_prompt_sent(&mut s, "hi".into(), Vec::new());
    s.permissions.push(LivePermission {
        tool_call_id: "tool".into(),
        question: "允许？".into(),
        options: Vec::new(),
        details: ApprovalDetailsView::Generic,
        responder: None,
        raw_request_line: None,
    });
    s.elicitation = Some(LiveElicitation {
        message: "选一个".into(),
        raw_fields: Vec::new(),
        chosen: BTreeMap::new(),
        text_values: BTreeMap::new(),
        responder: None,
        recovered_tool_call_id: None,
        raw_request_line: None,
    });

    force_end(&mut s, AcpEndKind::SessionOwnershipConflict, "会话已被占用");

    assert_eq!(s.phase, DaemonPhase::Dead);
    assert_eq!(s.end_reason, "会话已被占用");
    assert_eq!(s.end_kind, AcpEndKind::SessionOwnershipConflict);
    assert!(s.permissions.is_empty());
    assert!(s.elicitation.is_none());
    assert!(matches!(&s.entries[0], AcpEntry::User(text) if text == "hi"));
}

#[test]
fn force_end_keeps_each_end_kind() {
    for kind in [
        AcpEndKind::Unknown,
        AcpEndKind::TransportDisconnected,
        AcpEndKind::ProviderFailed,
        AcpEndKind::RestoreFailed,
        AcpEndKind::SessionOwnershipConflict,
        AcpEndKind::SessionTerminated,
    ] {
        let mut s = fresh_state();
        force_end(&mut s, kind, "x");
        assert_eq!(s.end_kind, kind, "{kind:?}");
        assert_eq!(s.phase, DaemonPhase::Dead);
        assert_eq!(s.end_reason, "x");
    }
}

/// 删除会话必须是不可重连的终态：客户端一旦把它当成“传输抖动”，就会用
/// 同一个 sid 重新 acp_open，把刚被删掉的会话复活。
#[test]
fn terminated_sessions_are_never_reconnected() {
    assert!(!AcpEndKind::SessionTerminated.is_transient());
    assert!(AcpEndKind::SessionTerminated.is_terminated());
    assert!(!AcpEndKind::TransportDisconnected.is_terminated());
}

#[test]
fn history_missing_marks_restore_failure_without_erasing_identity() {
    let mut s = fresh_state();
    s.history_session_id = Some("missing-history".into());

    apply_event(
        &mut s,
        ConversationEvent::RestoreFailed(
            crate::acp_conn::ConversationRestoreFailure::HistoryMissing,
        ),
    );

    assert_eq!(s.history_session_id.as_deref(), Some("missing-history"));
    assert_eq!(s.phase, DaemonPhase::Dead);
    assert_eq!(s.end_reason, "旧会话记录不存在，无法恢复");
}

#[test]
fn should_persist_excludes_streaming_and_ephemeral_events() {
    let mut s = fresh_state();
    let o = apply_event(
        &mut s,
        ConversationEvent::AgentChunk {
            thought: false,
            text: "x".into(),
            parent_id: None,
        },
    );
    assert!(!o.should_persist);
    let o = apply_event(&mut s, ConversationEvent::TurnEnded(StopReason::EndTurn));
    assert!(o.should_persist);
}

#[test]
fn parallel_tool_completion_reports_the_matching_entry_offset() {
    let mut s = fresh_state();
    for id in ["tool-a", "tool-b"] {
        apply_event(
            &mut s,
            ConversationEvent::ToolStarted {
                id: id.into(),
                title: id.into(),
                kind: ToolKind::Execute,
            },
        );
    }

    let outcome = apply_event(
        &mut s,
        ConversationEvent::ToolFinished {
            id: "tool-a".into(),
            status: ToolCallStatus::Completed,
            output: vec![ToolOutputPart::Text("done".into())],
        },
    );

    assert_eq!(outcome.entries_offset, Some(0));
    assert!(matches!(
        &s.entries[0],
        AcpEntry::ToolCall {
            status: ToolCallStatus::Completed,
            ..
        }
    ));
    assert!(matches!(
        &s.entries[1],
        AcpEntry::ToolCall {
            status: ToolCallStatus::InProgress,
            ..
        }
    ));
}

#[test]
fn incremental_snapshot_contains_only_requested_tail() {
    let mut state = fresh_state();
    state.entries = vec![
        AcpEntry::User("one".into()),
        AcpEntry::User("two".into()),
        AcpEntry::User("three".into()),
    ];
    let snapshot = state.to_snapshot_since(false, 2);
    assert_eq!(snapshot.entries_offset, 2);
    assert_eq!(snapshot.entries.len(), 1);
    assert!(matches!(&snapshot.entries[0], AcpEntry::User(text) if text == "three"));
}

#[test]
fn snapshot_replay_flag_is_preserved() {
    let mut state = fresh_state();
    state.replaying_history = true;
    let snapshot = state.to_snapshot(false);
    assert!(snapshot.replaying_history);
}

#[test]
fn legacy_elicitation_field_without_required_stays_required() {
    let legacy: ElicitFieldView = serde_json::from_value(serde_json::json!({
        "key": "scope",
        "title": "Scope",
        "kind": { "Text": { "secret": false } }
    }))
    .expect("legacy elicitation field should deserialize");
    assert!(legacy.required);

    let optional: ElicitFieldView = serde_json::from_value(serde_json::json!({
        "key": "notes",
        "title": "Notes",
        "required": false,
        "kind": { "Text": { "secret": false } }
    }))
    .expect("new optional elicitation field should deserialize");
    assert!(!optional.required);
}

#[test]
fn choose_elicitation_single_select_signals_auto_submit() {
    use agent_client_protocol::schema::v1::ElicitationContentValue as V;
    let mut s = fresh_state();
    s.elicitation = Some(LiveElicitation {
        message: "pick one".into(),
        raw_fields: vec![ElicitField {
            key: "k".into(),
            title: "t".into(),
            required: true,
            allow_custom_input: false,
            kind: ElicitFieldKind::Select(vec![
                crate::acp_conn::ElicitOption {
                    value: V::String("a".into()),
                    label: "A".into(),
                },
                crate::acp_conn::ElicitOption {
                    value: V::String("b".into()),
                    label: "B".into(),
                },
            ]),
        }],
        chosen: Default::default(),
        text_values: Default::default(),
        responder: None,
        recovered_tool_call_id: None,
        raw_request_line: None,
    });
    let auto_submit = choose_elicitation(&mut s, 0, 1);
    assert!(auto_submit);
    assert_eq!(
        s.elicitation.as_ref().unwrap().chosen.get(&0),
        Some(&vec![1])
    );
}

#[test]
fn choose_elicitation_multi_select_toggles() {
    use agent_client_protocol::schema::v1::ElicitationContentValue as V;
    let mut s = fresh_state();
    s.elicitation = Some(LiveElicitation {
        message: "pick many".into(),
        raw_fields: vec![ElicitField {
            key: "k".into(),
            title: "t".into(),
            required: true,
            allow_custom_input: false,
            kind: ElicitFieldKind::MultiSelect(vec![
                crate::acp_conn::ElicitOption {
                    value: V::String("a".into()),
                    label: "A".into(),
                },
                crate::acp_conn::ElicitOption {
                    value: V::String("b".into()),
                    label: "B".into(),
                },
            ]),
        }],
        chosen: Default::default(),
        text_values: Default::default(),
        responder: None,
        recovered_tool_call_id: None,
        raw_request_line: None,
    });
    let auto_submit = choose_elicitation(&mut s, 0, 0);
    assert!(!auto_submit); // multi-select 从不自动提交
    assert_eq!(
        s.elicitation.as_ref().unwrap().chosen.get(&0),
        Some(&vec![0])
    );
    choose_elicitation(&mut s, 0, 0); // 再点一次 = 取消
    assert_eq!(
        s.elicitation.as_ref().unwrap().chosen.get(&0),
        Some(&vec![])
    );
}

/// 题目卡的「自己写答案」：提交时自定义文本优先于已选选项；多选则追加在末尾。
#[test]
fn submit_elicitation_custom_answer_wins_over_chosen_option() {
    use agent_client_protocol::schema::v1::ElicitationContentValue as V;
    use std::sync::mpsc;

    let (select_tx, select_rx) = mpsc::channel();
    let mut s = fresh_state();
    s.elicitation = Some(LiveElicitation {
        message: "两道题".into(),
        raw_fields: vec![
            ElicitField {
                key: "one".into(),
                title: "单选".into(),
                required: true,
                allow_custom_input: true,
                kind: ElicitFieldKind::Select(vec![
                    crate::acp_conn::ElicitOption {
                        value: V::String("a".into()),
                        label: "A".into(),
                    },
                    crate::acp_conn::ElicitOption {
                        value: V::String("b".into()),
                        label: "B".into(),
                    },
                ]),
            },
            ElicitField {
                key: "many".into(),
                title: "多选".into(),
                required: true,
                allow_custom_input: true,
                kind: ElicitFieldKind::MultiSelect(vec![crate::acp_conn::ElicitOption {
                    value: V::String("x".into()),
                    label: "X".into(),
                }]),
            },
        ],
        chosen: std::collections::BTreeMap::from([(0, vec![0]), (1, vec![0])]),
        text_values: std::collections::BTreeMap::from([
            (0, "自己写的答案".into()),
            (1, "补充项".into()),
        ]),
        responder: Some(ElicitationResponder::external(move |content| {
            select_tx.send(content).unwrap();
        })),
        recovered_tool_call_id: None,
        raw_request_line: None,
    });
    submit_elicitation(&mut s);
    let content = select_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap()
        .expect("custom answer should not cancel");
    // 单选：自定义文本覆盖已选的 A。
    assert_eq!(content.get("one"), Some(&V::String("自己写的答案".into())));
    // 多选：选中的 x + 自定义「补充项」。
    assert_eq!(
        content.get("many"),
        Some(&V::StringArray(vec!["x".to_string(), "补充项".to_string()]))
    );
    assert!(s.elicitation.is_none());
}

/// 没开启自定义输入的字段，text_values 写不进去，提交仍走选项。
#[test]
fn set_elicitation_text_rejects_fields_without_custom_input() {
    let mut s = fresh_state();
    s.elicitation = Some(LiveElicitation {
        message: "普通单选".into(),
        raw_fields: vec![ElicitField {
            key: "k".into(),
            title: "t".into(),
            required: true,
            allow_custom_input: false,
            kind: ElicitFieldKind::Select(vec![crate::acp_conn::ElicitOption {
                value: agent_client_protocol::schema::v1::ElicitationContentValue::String(
                    "a".into(),
                ),
                label: "A".into(),
            }]),
        }],
        chosen: Default::default(),
        text_values: Default::default(),
        responder: None,
        recovered_tool_call_id: None,
        raw_request_line: None,
    });
    set_elicitation_text(&mut s, 0, "强塞的答案".into());
    assert!(s.elicitation.as_ref().unwrap().text_values.is_empty());
    // 开启后能写进去。
    s.elicitation.as_mut().unwrap().raw_fields[0].allow_custom_input = true;
    set_elicitation_text(&mut s, 0, "自己写".into());
    assert_eq!(
        s.elicitation.as_ref().unwrap().text_values.get(&0),
        Some(&"自己写".to_string())
    );
}

/// 恢复卡（responder 已丢）的自定义答案也要拼进纯文本回答。
#[test]
fn recovered_elicitation_answer_appends_custom_text() {
    let mut s = fresh_state();
    s.elicitation = Some(LiveElicitation {
        message: "恢复的题".into(),
        raw_fields: vec![ElicitField {
            key: "k".into(),
            title: "t".into(),
            required: true,
            allow_custom_input: true,
            kind: ElicitFieldKind::Select(vec![crate::acp_conn::ElicitOption {
                value: agent_client_protocol::schema::v1::ElicitationContentValue::String(
                    "a".into(),
                ),
                label: "A".into(),
            }]),
        }],
        chosen: std::collections::BTreeMap::from([(0, vec![0])]),
        text_values: std::collections::BTreeMap::from([(0, "外加一句".into())]),
        responder: None,
        recovered_tool_call_id: None,
        raw_request_line: None,
    });
    assert_eq!(
        recovered_elicitation_answer(&s),
        Some("A、外加一句".to_string())
    );
    // 只写自定义答案、不点选项：回答就是那段自定义文本。
    s.elicitation.as_mut().unwrap().chosen.clear();
    assert_eq!(
        recovered_elicitation_answer(&s),
        Some("外加一句".to_string())
    );
    // 什么都没选也没写：不算完成。
    s.elicitation.as_mut().unwrap().text_values.clear();
    assert_eq!(recovered_elicitation_answer(&s), None);
}

#[test]
fn unfinished_ask_user_question_rebuilds_a_prompt_backed_elicitation() {
    let raw_input = serde_json::json!({
        "questions": [{
            "question": "提醒走哪个渠道？",
            "multiSelect": false,
            "options": [
                {"label": "Gitea Issue", "description": "仓库待办"},
                {"label": "Bark", "description": "手机推送"}
            ]
        }]
    });

    let card = recovered_elicitation("提醒走哪个渠道？", Some(&raw_input), "ask-1")
        .expect("replayed AskUserQuestion should remain actionable");
    assert!(card.responder.is_none());
    assert_eq!(card.recovered_tool_call_id.as_deref(), Some("ask-1"));
    assert_eq!(card.raw_fields.len(), 1);
    let ElicitFieldKind::Select(options) = &card.raw_fields[0].kind else {
        panic!("single-select question should remain a select");
    };
    assert_eq!(options[1].label, "Bark");

    let mut state = fresh_state();
    state.elicitation = Some(card);
    assert!(choose_elicitation(&mut state, 0, 1));
    assert_eq!(
        recovered_elicitation_answer(&state).as_deref(),
        Some("Bark")
    );

    dismiss_elicitation(&mut state);
    assert!(matches!(state.phase, DaemonPhase::Idle));
}

#[test]
fn unfinished_multiselect_ask_user_question_joins_chosen_labels() {
    let raw_input = serde_json::json!({
        "questions": [{
            "question": "提醒走哪个渠道？",
            "multiSelect": true,
            "options": [
                {"label": "Gitea Issue"},
                {"label": "Bark"},
                {"label": "邮件"}
            ]
        }]
    });

    let card = recovered_elicitation("提醒走哪个渠道？", Some(&raw_input), "ask-2")
        .expect("replayed multi-select should remain actionable");
    assert!(matches!(
        card.raw_fields[0].kind,
        ElicitFieldKind::MultiSelect(_)
    ));

    let mut state = fresh_state();
    state.elicitation = Some(card);
    assert!(!choose_elicitation(&mut state, 0, 0));
    assert!(!choose_elicitation(&mut state, 0, 1));
    assert_eq!(
        recovered_elicitation_answer(&state).as_deref(),
        Some("Gitea Issue、Bark")
    );
}

#[test]
fn completed_replayed_question_does_not_rebuild_an_elicitation() {
    let raw_input = serde_json::json!({
        "questions": [{
            "question": "Already answered?",
            "options": [{"label": "Yes"}, {"label": "No"}]
        }]
    });
    let mut state = fresh_state();
    apply_event(&mut state, ConversationEvent::HistoryReplayStarted);

    apply_event(
        &mut state,
        ConversationEvent::ToolCall(
            agent_client_protocol::schema::v1::ToolCall::new("ask-1", "Already answered?")
                .status(agent_client_protocol::schema::v1::ToolCallStatus::Completed)
                .raw_input(raw_input),
        ),
    );

    assert!(state.elicitation.is_none());
    assert!(matches!(state.phase, DaemonPhase::Connecting));
}

#[test]
fn terminal_tool_update_clears_recovered_elicitation() {
    let raw_input = serde_json::json!({
        "questions": [{
            "question": "Already answered?",
            "options": [{"label": "Yes"}, {"label": "No"}]
        }]
    });
    let mut state = fresh_state();
    apply_event(&mut state, ConversationEvent::HistoryReplayStarted);
    apply_event(
        &mut state,
        ConversationEvent::ToolCall(
            agent_client_protocol::schema::v1::ToolCall::new("ask-1", "Already answered?")
                .status(agent_client_protocol::schema::v1::ToolCallStatus::Pending)
                .raw_input(raw_input),
        ),
    );
    assert!(matches!(state.phase, DaemonPhase::WaitingForUser));

    apply_event(
        &mut state,
        ConversationEvent::ToolCallUpdate(agent_client_protocol::schema::v1::ToolCallUpdate::new(
            "ask-1",
            agent_client_protocol::schema::v1::ToolCallUpdateFields::new()
                .status(agent_client_protocol::schema::v1::ToolCallStatus::Completed),
        )),
    );

    assert!(state.elicitation.is_none());
    assert!(matches!(state.phase, DaemonPhase::Idle));
    assert!(matches!(
        state.entries.last(),
        Some(AcpEntry::ToolCall {
            status: ToolCallStatus::Completed,
            ..
        })
    ));
}

#[test]
fn replayed_user_answer_clears_question_without_a_terminal_tool_update() {
    let raw_input = serde_json::json!({
        "questions": [{
            "question": "Fix it now?",
            "options": [{"label": "Fix"}, {"label": "Later"}]
        }]
    });
    let mut state = fresh_state();
    apply_event(&mut state, ConversationEvent::HistoryReplayStarted);
    apply_event(
        &mut state,
        ConversationEvent::ToolCall(
            agent_client_protocol::schema::v1::ToolCall::new("ask-1", "Fix it now?")
                .status(agent_client_protocol::schema::v1::ToolCallStatus::Pending)
                .raw_input(raw_input),
        ),
    );
    assert!(matches!(state.phase, DaemonPhase::WaitingForUser));

    apply_event(&mut state, ConversationEvent::UserChunk("Fix".into()));

    assert!(state.elicitation.is_none());
    assert!(matches!(state.phase, DaemonPhase::Idle));
    assert!(matches!(
        state.entries.last(),
        Some(AcpEntry::User(answer)) if answer == "Fix"
    ));
}

#[test]
fn ready_does_not_overwrite_a_recovered_choice_phase() {
    let raw_input = serde_json::json!({
        "questions": [{
            "question": "Choose",
            "options": [{"label": "A"}]
        }]
    });
    let mut state = fresh_state();
    state.elicitation = recovered_elicitation("Choose", Some(&raw_input), "ask-1");

    apply_event(
        &mut state,
        ConversationEvent::Ready {
            session_id: agent_client_protocol::schema::v1::SessionId::new("session"),
            kind: ReadyKind::ResumedKeepHistory,
            supports_image: true,
        },
    );

    assert!(matches!(state.phase, DaemonPhase::WaitingForUser));
    assert!(state.elicitation.is_some());
}

#[test]
fn should_auto_resume_requires_ended_and_known_session_id() {
    let mut s = fresh_state();
    s.phase = DaemonPhase::Dead;
    s.end_reason = "gone".into();
    assert!(!should_auto_resume(&s)); // 没有旧 session id
    s.history_session_id = Some("sid-1".into());
    assert!(should_auto_resume(&s));
    s.phase = DaemonPhase::Idle;
    assert!(!should_auto_resume(&s)); // 还活着，用不上「自动续接」
}

#[test]
fn compaction_and_native_queue_events_update_the_snapshot() {
    let mut state = fresh_state();
    apply_event(
        &mut state,
        ConversationEvent::SessionControls {
            compaction: true,
            native_queue: true,
            rewind: true,
        },
    );
    apply_event(
        &mut state,
        ConversationEvent::Compaction {
            running: true,
            detail: "正在压缩上下文…".into(),
            used: None,
            size: None,
        },
    );
    assert!(state.supports_compaction);
    assert!(state.supports_native_queue);
    assert!(state.compacting);
    assert_eq!(state.status_line.as_deref(), Some("正在压缩上下文…"));

    apply_event(
        &mut state,
        ConversationEvent::Compaction {
            running: false,
            detail: "已压缩上下文：150000 → 32000 tokens".into(),
            used: Some(32_000),
            size: Some(200_000),
        },
    );
    assert!(!state.compacting);
    assert_eq!(state.usage, Some((32_000, 200_000)));

    apply_event(
        &mut state,
        ConversationEvent::PromptQueue {
            steering: vec!["换方向".into()],
            follow_up: vec!["总结".into()],
        },
    );
    apply_event(
        &mut state,
        ConversationEvent::ComposerRestore {
            revision: 1,
            texts: vec!["换方向".into(), "总结".into()],
        },
    );
    let snap = state.to_snapshot(false);
    assert!(snap.queued_steering.is_empty());
    assert!(snap.queued_follow_up.is_empty());
    assert_eq!(snap.composer_restore_revision, 1);
    assert_eq!(snap.composer_restore_texts, ["换方向", "总结"]);
    assert!(
        !state
            .entries
            .iter()
            .any(|entry| matches!(entry, AcpEntry::User(text) if text == "换方向")),
        "撤回排队不得把中途插入写进会话"
    );
}

#[test]
fn mid_turn_steer_stays_in_queue_until_consumed() {
    let mut state = fresh_state();
    note_prompt_sent(&mut state, "先做这个".into(), Vec::new());
    apply_event(
        &mut state,
        ConversationEvent::AgentChunk {
            thought: false,
            text: "好".into(),
            parent_id: None,
        },
    );

    queue_mid_turn_input(&mut state, "打算".into(), Vec::new());
    assert_eq!(state.queued_steering, ["打算"]);
    assert!(
        !state
            .entries
            .iter()
            .any(|entry| matches!(entry, AcpEntry::User(text) if text == "打算")),
        "刚发出的中途插入只应在队列里"
    );

    apply_event(
        &mut state,
        ConversationEvent::PromptQueue {
            steering: vec!["打算".into()],
            follow_up: Vec::new(),
        },
    );
    assert_eq!(state.queued_steering, ["打算"]);
    assert!(
        !state
            .entries
            .iter()
            .any(|entry| matches!(entry, AcpEntry::User(text) if text == "打算")),
        "Pi 确认入队仍未插入，不得进会话"
    );

    let outcome = apply_event(
        &mut state,
        ConversationEvent::PromptQueue {
            steering: Vec::new(),
            follow_up: Vec::new(),
        },
    );
    assert!(state.queued_steering.is_empty());
    assert!(
        matches!(state.entries.last(), Some(AcpEntry::User(text)) if text == "打算"),
        "Pi 从队列里吃掉之后才应出现会话气泡"
    );
    assert_eq!(outcome.entries_offset, Some(state.entries.len() - 1));
    assert!(outcome.should_persist);
}

#[test]
fn withdrawing_steering_does_not_leave_a_user_bubble() {
    let mut state = fresh_state();
    note_prompt_sent(&mut state, "先做这个".into(), Vec::new());
    queue_mid_turn_input(&mut state, "打算".into(), Vec::new());

    apply_event(
        &mut state,
        ConversationEvent::ComposerRestore {
            revision: 1,
            texts: vec!["打算".into()],
        },
    );

    assert!(state.queued_steering.is_empty());
    assert_eq!(state.composer_restore_texts, ["打算"]);
    assert!(
        !state
            .entries
            .iter()
            .any(|entry| matches!(entry, AcpEntry::User(text) if text == "打算")),
        "撤回必须把字还回输入框，不能在会话里留下气泡"
    );
}

#[test]
fn consuming_the_first_steering_item_keeps_the_rest_queued() {
    let mut state = fresh_state();
    queue_mid_turn_input(&mut state, "先改测试".into(), Vec::new());
    queue_mid_turn_input(&mut state, "再提交".into(), Vec::new());

    apply_event(
        &mut state,
        ConversationEvent::PromptQueue {
            steering: vec!["再提交".into()],
            follow_up: Vec::new(),
        },
    );

    assert_eq!(state.queued_steering, ["再提交"]);
    assert!(matches!(state.entries.last(), Some(AcpEntry::User(text)) if text == "先改测试"));
    assert_eq!(
        state
            .entries
            .iter()
            .filter(|entry| matches!(entry, AcpEntry::User(_)))
            .count(),
        1
    );
}

#[test]
fn restart_clears_old_runtime_id_but_keeps_history_identity() {
    let mut s = fresh_state();
    s.acp_session_id = Some("runtime".into());
    s.history_session_id = Some("history".into());

    reset_for_restart(&mut s);

    assert!(s.acp_session_id.is_none());
    assert_eq!(s.history_session_id.as_deref(), Some("history"));
}

#[test]
fn hosted_snapshot_merge_replaces_incremental_tail_and_runtime_state() {
    let mut state = AcpSessionState {
        entries: vec![
            AcpEntry::User("first".into()),
            AcpEntry::Assistant {
                text: "old tail".into(),
                thought: false,
            },
        ],
        ..Default::default()
    };

    let mut snapshot = state.to_snapshot(false);
    snapshot.entries_offset = 1;
    snapshot.entries_total = 3;
    snapshot.entries = vec![
        AcpEntry::Assistant {
            text: "new tail".into(),
            thought: false,
        },
        AcpEntry::User("next".into()),
    ];
    snapshot.phase = DaemonPhase::Thinking;
    snapshot.turn_started_at_ms = Some(123);

    let changed_from = state
        .merge_hosted_snapshot(snapshot)
        .expect("连续的 host 增量快照应能合并");

    assert_eq!(changed_from, 1);
    assert_eq!(state.entries.len(), 3);
    assert!(matches!(
        &state.entries[1],
        AcpEntry::Assistant { text, thought: false } if text == "new tail"
    ));
    assert_eq!(state.phase, DaemonPhase::Thinking);
    assert_eq!(state.turn_started_at_ms, Some(123));
}

#[test]
fn hosted_snapshot_merge_rejects_a_missing_prefix() {
    let mut state = AcpSessionState::default();
    state.entries.push(AcpEntry::User("only".into()));
    let mut snapshot = state.to_snapshot(false);
    snapshot.entries_offset = 2;
    snapshot.entries_total = 3;
    snapshot.entries = vec![AcpEntry::User("gap".into())];

    assert!(state.merge_hosted_snapshot(snapshot).is_err());
    assert!(matches!(
        state.entries.as_slice(),
        [AcpEntry::User(text)] if text == "only"
    ));
}

#[test]
fn rewind_truncates_projection_and_adopts_the_new_provider_identity() {
    let mut state = fresh_state();
    // 三轮对话，回退到第 2 条用户消息（下标 2）之后全部丢弃。
    state.entries.push(AcpEntry::User("one".into()));
    state.entries.push(AcpEntry::Assistant {
        text: "a1".into(),
        thought: false,
    });
    state.entries.push(AcpEntry::User("two".into()));
    state.entries.push(AcpEntry::Assistant {
        text: "a2".into(),
        thought: false,
    });
    state.entries.push(AcpEntry::User("three".into()));
    state.entries.push(AcpEntry::Assistant {
        text: "a3".into(),
        thought: false,
    });
    state.queued_steering.push("排队中".into());
    state.queued_follow_up.push("后续".into());
    state.turn_timings.push(TurnTiming {
        user_index: 4,
        started_at_ms: 1,
        ended_at_ms: Some(2),
    });
    state.turn_timings.push(TurnTiming {
        user_index: 0,
        started_at_ms: 3,
        ended_at_ms: None,
    });
    state.completed_unread = true;
    state.completed_delivery_id = Some("delivery-9".into());
    state.acp_session_id = Some("old-branch".into());
    state.history_session_id = Some("old-branch".into());

    let outcome = apply_event(&mut state, ConversationEvent::Rewound { truncate_from: 2 });

    assert_eq!(state.entries.len(), 2);
    assert!(matches!(
        &state.entries[1],
        AcpEntry::Assistant { text, .. } if text == "a1"
    ));
    assert!(state.queued_steering.is_empty());
    assert!(state.queued_follow_up.is_empty());
    assert!(state.turn_timings.iter().all(|t| t.user_index < 2));
    assert!(!state.completed_unread);
    assert_eq!(state.completed_delivery_id, None);
    // 截断是“变短”，必须走 offset=0 的全量快照广播。
    assert_eq!(outcome.entries_offset, Some(0));
    assert!(outcome.should_persist);

    // fork 之后 provider 侧换了新 session 文件，旧 id 必须被覆盖，
    // 否则重连会把被丢弃的旧分支恢复出来。
    apply_event(
        &mut state,
        ConversationEvent::ProviderSessionIdChanged(
            agent_client_protocol::schema::v1::SessionId::from("new-branch"),
        ),
    );
    assert_eq!(state.acp_session_id.as_deref(), Some("new-branch"));
    assert_eq!(state.history_session_id.as_deref(), Some("new-branch"));

    // 游标越界（极端时序下投影还没追上）不能把会话清空。
    let outcome = apply_event(&mut state, ConversationEvent::Rewound { truncate_from: 99 });
    assert_eq!(state.entries.len(), 2);
    assert_eq!(outcome.entries_offset, None);
}

#[test]
fn session_controls_rewind_flag_reaches_the_snapshot() {
    let mut state = fresh_state();
    assert!(!state.supports_rewind);
    apply_event(
        &mut state,
        ConversationEvent::SessionControls {
            compaction: false,
            native_queue: false,
            rewind: true,
        },
    );
    assert!(state.supports_rewind);
    let snap = state.to_snapshot(false);
    assert!(snap.supports_rewind);
    // round-trip：老 daemon 的缺省值是 false，新快照往返必须保住 true。
    let wire = crate::acp_session::ConversationSnapshotDe::from(snap);
    assert!(wire.supports_rewind);
}
