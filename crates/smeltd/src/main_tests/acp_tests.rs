use super::*;
use smelt_core::acp_chat::{AcpEntry, ToolCallStatus, ToolKind};
use smelt_core::acp_session::AcpSessionState;
use smelt_core::daemon_state::DaemonPhase;
use smelt_event_protocol::{PluginId, SubscriptionId, Topic};
use std::collections::BTreeSet;

fn make_acp_session(id: &str, reduced: AcpSessionState) -> Arc<AcpSession> {
    Arc::new(make_acp_session_value(id, reduced))
}

/// 没发首帧的连接：对方手里什么都没有，之后的增量帧会把快照头和条目整个带过去。
fn nothing_sent() -> smelt_core::acp_wire::FrameMirror {
    full_frame(&serde_json::json!({ "snapshot": { "entries": [] } })).1
}

/// 像客户端那样读一帧：增量帧经这条连接的接收方镜像还原成完整的快照帧，条目是从
/// 第一个变化的那条到末尾；整段帧顺手重建镜像。同一条连接上的帧要共用一份镜像。
fn decode_frame(mirror: &mut smelt_core::acp_wire::FrameMirror, line: &str) -> serde_json::Value {
    let frame: serde_json::Value = serde_json::from_str(line).unwrap();
    match frame.get("delta") {
        Some(delta) => mirror
            .apply(&serde_json::from_value(delta.clone()).unwrap())
            .unwrap(),
        None => {
            mirror.full(&frame);
            frame
        }
    }
}

fn test_acp_attachment(sess: &Arc<AcpSession>, stream: UnixStream, id: &str) -> AcpSnapshotLink {
    let tool_debug_generation_sent = sess.reduced.lock().unwrap().tool_debug_generation;
    let encode_sess = Arc::downgrade(sess);
    let detach_sess = Arc::downgrade(sess);
    spawn_acp_snapshot_link(
        stream,
        Vec::new(),
        nothing_sent(),
        tool_debug_generation_sent,
        id,
        "acp-test",
        Arc::new(
            move |frame_kind,
                  from,
                  should_persist,
                  include_envelope,
                  include_runtime_debug,
                  tool_debug_generation_sent,
                  runtime_debug| {
                let sess = encode_sess.upgrade()?;
                Some(encode_acp_snapshot(
                    &sess,
                    frame_kind,
                    from,
                    should_persist,
                    include_envelope,
                    include_runtime_debug,
                    tool_debug_generation_sent,
                    SnapshotProjection::FULL,
                    runtime_debug,
                ))
            },
        ),
        Arc::new(move |fd| {
            let Some(sess) = detach_sess.upgrade() else {
                return;
            };
            detach_failed_snapshot_link(&sess, fd);
        }),
    )
    .unwrap()
}

#[test]
fn hot_attach_never_relaunches_or_replays_agent_history() {
    let mut idle = AcpSessionState::default();
    idle.phase = DaemonPhase::Idle;
    assert!(!acp_open_needs_relaunch(false, true, true, None, &idle));
    assert!(!acp_open_needs_relaunch(false, true, false, None, &idle));
    assert!(acp_open_needs_relaunch(true, false, true, None, &idle));
    assert!(acp_open_needs_relaunch(false, false, true, None, &idle));

    let mut starting = AcpSessionState::default();
    starting.phase = DaemonPhase::Connecting;
    starting.history_session_id = Some("history".into());
    assert!(!acp_open_needs_relaunch(
        false,
        true,
        true,
        Some("history"),
        &starting,
    ));
    assert!(acp_open_needs_relaunch(
        false,
        true,
        true,
        Some("other-history"),
        &starting,
    ));
    assert!(!acp_open_needs_relaunch(
        false,
        true,
        true,
        Some("history"),
        &idle,
    ));
}

#[test]
fn changed_acp_runtime_spec_relaunches_live_session() {
    let current = smelt_core::agent_kind::ConversationLaunchSpec::from_command("copilot --acp");
    let requested =
        smelt_core::agent_kind::ConversationLaunchSpec::from_command("copilot --acp --allow-all");
    let empty = BTreeMap::new();

    assert!(!acp_runtime_needs_relaunch(
        true,
        Some(&current),
        &empty,
        &current,
        &empty,
    ));
    assert!(acp_runtime_needs_relaunch(
        true,
        Some(&current),
        &empty,
        &requested,
        &empty,
    ));

    let mut requested_env = BTreeMap::new();
    requested_env.insert("PLUGIN_TOKEN".to_string(), "new-token".to_string());
    assert!(acp_runtime_needs_relaunch(
        true,
        Some(&current),
        &empty,
        &current,
        &requested_env,
    ));
    assert!(!acp_runtime_needs_relaunch(
        false,
        Some(&current),
        &empty,
        &requested,
        &requested_env,
    ));
    // 没有记录过的 launch 无法比较规格，不能因此杀掉仍活着的进程。
    assert!(!acp_runtime_needs_relaunch(
        true,
        None,
        &empty,
        &requested,
        &requested_env,
    ));
}

#[test]
fn runtime_spec_fingerprint_survives_handoff_without_serializing_secrets() {
    let launch = smelt_core::agent_kind::ConversationLaunchSpec::from_command("copilot --acp");
    let mut original_env = BTreeMap::new();
    original_env.insert("PLUGIN_TOKEN".to_string(), "original-token".to_string());

    let before_handoff = acp_runtime_spec_fingerprint(&launch, &original_env);
    assert_eq!(
        before_handoff,
        acp_runtime_spec_fingerprint(&launch, &original_env),
        "相同 runtime 规格在 daemon exec 前后必须稳定，不能误重启仍存活的宿主"
    );

    let mut changed_env = original_env;
    changed_env.insert("PLUGIN_TOKEN".to_string(), "rotated-token".to_string());
    assert_ne!(
        before_handoff,
        acp_runtime_spec_fingerprint(&launch, &changed_env),
        "凭据真的变化时仍必须触发 runtime 重启"
    );
}

#[test]
fn blank_restore_policy_survives_replay_reset_and_success_becomes_strict() {
    let mut state = AcpRestoreState::Fresh;
    assert_eq!(
        state.begin(Some("history"), false),
        Some(AcpRestorePolicy::FreshOnMissing)
    );
    // HistoryReplayStarted 可能已经清空或随后重建了本地 entries，重试仍沿用
    // 首次恢复时的判定，不能因为当前 entries 恰好为空而重新推断。
    assert_eq!(
        state.begin(Some("history"), true),
        Some(AcpRestorePolicy::FreshOnMissing)
    );
    state.mark_restored(Some("history"));
    assert_eq!(
        state.begin(Some("history"), false),
        Some(AcpRestorePolicy::Strict)
    );

    assert_eq!(
        state.begin(Some("another-history"), false),
        Some(AcpRestorePolicy::FreshOnMissing)
    );
}

#[test]
fn restore_with_local_entries_is_strict_from_the_first_attempt() {
    let mut state = AcpRestoreState::Fresh;
    assert_eq!(
        state.begin(Some("history"), true),
        Some(AcpRestorePolicy::Strict)
    );
}

#[test]
fn reopening_live_session_neither_relaunches_nor_trims_history() {
    let mut reduced = AcpSessionState::default();
    for index in 0..5 {
        reduced
            .entries_mut()
            .push(AcpEntry::User(format!("message-{index}")));
    }
    reduced.phase = DaemonPhase::Idle;

    let acp_sessions = new_test_acp_sessions();
    let (slot, _) =
        acp_sessions.reserve_with("acp-live", || make_acp_session_value("acp-live", reduced));
    let (cmd_tx, _cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });
    *slot.value.conversation_binding.lock().unwrap() =
        Some(smelt_core::conversation::ConversationBinding::Automation {
            run_id: "run-current".into(),
        });

    let (server, client) = UnixStream::pair().unwrap();
    let reader = BufReader::new(server.try_clone().unwrap());
    let sessions_for_open = Arc::clone(&acp_sessions);
    let worker = thread::spawn(move || {
        handle_acp_open(
            server,
            reader,
            &serde_json::json!({
                "id": "acp-live",
                "launch": {"command": "/must/not/be/launched"},
                "tail_limit": 2,
                "conversation_binding": {"type": "direct"}
            }),
            new_sessions(),
            sessions_for_open,
            new_event_hub(),
        );
    });

    let mut reader = BufReader::new(client.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    // tail_limit 只是旧客户端还在发的字段：窗口按页另取历史，首帧不再裁。
    assert_eq!(response["snapshot"]["entries_offset"], 0);
    assert_eq!(response["snapshot"]["entries_total"], 5);
    assert_eq!(response["snapshot"]["entries"].as_array().unwrap().len(), 5);
    assert_eq!(
        response["snapshot"]["conversation_state"]["binding"]["run_id"],
        "run-current"
    );
    assert!(slot.value.handle.lock().unwrap().is_some());
    assert!(matches!(
        &*slot.value.conversation_binding.lock().unwrap(),
        Some(smelt_core::conversation::ConversationBinding::Automation { run_id })
            if run_id == "run-current"
    ));

    drop(reader);
    drop(client);
    worker.join().unwrap();
}

#[test]
fn reopening_live_session_adopts_client_binding_when_unknown() {
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Idle;

    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-unknown", || {
        make_acp_session_value("acp-unknown", reduced)
    });
    let (cmd_tx, _cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });
    *slot.value.conversation_binding.lock().unwrap() = None;

    let (server, client) = UnixStream::pair().unwrap();
    let reader = BufReader::new(server.try_clone().unwrap());
    let sessions_for_open = Arc::clone(&acp_sessions);
    let worker = thread::spawn(move || {
        handle_acp_open(
            server,
            reader,
            &serde_json::json!({
                "id": "acp-unknown",
                "launch": {"command": "/must/not-be-launched"},
                "conversation_binding": {
                    "type": "automation",
                    "run_id": "run-restored"
                }
            }),
            new_sessions(),
            sessions_for_open,
            new_event_hub(),
        );
    });

    let mut reader = BufReader::new(client.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        response["snapshot"]["conversation_state"]["binding"]["run_id"],
        "run-restored"
    );
    assert!(matches!(
        &*slot.value.conversation_binding.lock().unwrap(),
        Some(smelt_core::conversation::ConversationBinding::Automation { run_id })
            if run_id == "run-restored"
    ));

    drop(reader);
    drop(client);
    worker.join().unwrap();
}

#[test]
fn unknown_conversation_binding_is_not_mirrored_as_direct() {
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Idle;

    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-no-binding", || {
        make_acp_session_value("acp-no-binding", reduced)
    });
    let (cmd_tx, _cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });
    *slot.value.conversation_binding.lock().unwrap() = None;

    let (server, client) = UnixStream::pair().unwrap();
    let reader = BufReader::new(server.try_clone().unwrap());
    let sessions_for_open = Arc::clone(&acp_sessions);
    let worker = thread::spawn(move || {
        handle_acp_open(
            server,
            reader,
            &serde_json::json!({
                "id": "acp-no-binding",
                "launch": {"command": "/must/not-be-launched"}
            }),
            new_sessions(),
            sessions_for_open,
            new_event_hub(),
        );
    });

    let mut reader = BufReader::new(client.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert!(
        response["snapshot"]["conversation_state"]["binding"].is_null(),
        "未知 binding 不能被快照发明成 Direct：{}",
        response["snapshot"]["conversation_state"]
    );
    assert!(slot.value.conversation_binding.lock().unwrap().is_none());

    drop(reader);
    drop(client);
    worker.join().unwrap();
}

#[test]
fn one_shot_action_keeps_existing_control_client_attached() {
    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-action", || {
        make_acp_session_value("acp-action", AcpSessionState::default())
    });
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });

    let (control_server, _control_client) = UnixStream::pair().unwrap();
    let control_fd = control_server.as_raw_fd();
    slot.value.out.lock().unwrap().client = Some(
        acp_snapshot_link_for_slot(
            &slot,
            control_server,
            Vec::new(),
            nothing_sent(),
            0,
            "acp-action",
            "acp-test",
            SnapshotProjection::FULL,
        )
        .unwrap(),
    );

    let (action_server, action_client) = UnixStream::pair().unwrap();
    handle_acp_action(
        action_server,
        &serde_json::json!({
            "id": "acp-action",
            "action": {"Prompt": {"text": "from mobile", "images": []}},
        }),
        &acp_sessions,
        &new_event_hub(),
    );

    let mut response = String::new();
    BufReader::new(action_client)
        .read_line(&mut response)
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&response).unwrap()["ok"],
        true
    );
    assert_eq!(
        slot.value
            .out
            .lock()
            .unwrap()
            .client
            .as_ref()
            .map(|client| client.fd),
        Some(control_fd),
        "one-shot action must not replace the PC control client"
    );
    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Prompt { text, images } => {
            assert_eq!(text, "from mobile");
            assert!(images.is_empty());
        }
        _ => panic!("expected prompt command"),
    }
    assert!(matches!(
        slot.value.reduced.lock().unwrap().entries().last(),
        Some(AcpEntry::User(text)) if text == "from mobile"
    ));
}

#[test]
fn submit_input_applies_pending_model_before_the_prompt() {
    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-submit-model", || {
        make_acp_session_value("acp-submit-model", AcpSessionState::default())
    });
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });

    let (server, client) = UnixStream::pair().unwrap();
    handle_acp_submit_input(
        server,
        &serde_json::json!({
            "id": "acp-submit-model",
            "input": {"text": "用新模型回答", "images": []},
            "config_values": [["model", "sonnet"]],
        }),
        &acp_sessions,
        &new_event_hub(),
    );
    let mut response = String::new();
    BufReader::new(client).read_line(&mut response).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["ok"], true, "{response}");

    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::SetConfigOption { config_id, value } => {
            assert_eq!(config_id, "model");
            assert_eq!(
                value,
                smelt_core::acp_conn::ConfigValue::Select("sonnet".into())
            );
        }
        _ => panic!("模型切换必须排在 prompt 前面"),
    }
    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } => {
            assert_eq!(text, "用新模型回答");
        }
        _ => panic!("配置之后才该发 prompt"),
    }
    assert!(cmd_rx.try_recv().is_err());
}

#[test]
fn switching_model_does_not_stop_the_current_turn() {
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Thinking;
    reduced.turn_started_at_ms = Some(1);
    let (slot, _) = acp_sessions.reserve_with("acp-model-keep-running", || {
        make_acp_session_value("acp-model-keep-running", reduced)
    });
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(1)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: true,
        supports_compaction: false,
        supports_native_queue: true,
        supports_rewind: false,
    });
    slot.value.prompt_in_flight.store(true, Ordering::SeqCst);

    let (server, client) = UnixStream::pair().unwrap();
    handle_acp_submit_input(
        server,
        &serde_json::json!({
            "id": "acp-model-keep-running",
            "input": {"text": "你是什么模型", "images": []},
            "config_values": [["model", "github-copilot/kimi-k3"]],
        }),
        &acp_sessions,
        &new_event_hub(),
    );
    let mut response = String::new();
    BufReader::new(client).read_line(&mut response).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["ok"], true, "{response}");

    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::SetConfigOption { config_id, value } => {
            assert_eq!(config_id, "model");
            assert_eq!(
                value,
                smelt_core::acp_conn::ConfigValue::Select("github-copilot/kimi-k3".into())
            );
        }
        smelt_core::acp_conn::ConversationCommand::Cancel => {
            panic!("切模型不该停掉当前回合，停掉用 Stop / ⌥↩")
        }
        _ => panic!("应先写下新模型"),
    }
    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Steer { text, .. } => {
            assert_eq!(text, "你是什么模型");
        }
        _ => panic!("回车仍应插进当前回合"),
    }
    assert!(cmd_rx.try_recv().is_err());
    assert!(slot.value.pending_prompts.lock().unwrap().is_empty());
}

#[test]
fn automation_run_rejects_direct_prompt_actions_but_keeps_approval_actions() {
    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-automation-action", || {
        make_acp_session_value("acp-automation-action", AcpSessionState::default())
    });
    *slot.value.conversation_binding.lock().unwrap() =
        Some(smelt_core::conversation::ConversationBinding::Automation {
            run_id: "run-1".into(),
        });
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });

    let (server, client) = UnixStream::pair().unwrap();
    handle_acp_action(
        server,
        &serde_json::json!({
            "id": "acp-automation-action",
            "action": {"Prompt": {"text": "second prompt", "images": []}},
        }),
        &acp_sessions,
        &new_event_hub(),
    );
    let mut response = String::new();
    BufReader::new(client).read_line(&mut response).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"], "automation Run input is daemon-owned");
    assert!(cmd_rx.try_recv().is_err());

    let (server, client) = UnixStream::pair().unwrap();
    handle_acp_action(
        server,
        &serde_json::json!({
            "id": "acp-automation-action",
            "action": {"SetConfigOption": {
                "config_id": "mode",
                "value_id": "bypassPermissions"
            }},
        }),
        &acp_sessions,
        &new_event_hub(),
    );
    let mut response = String::new();
    BufReader::new(client).read_line(&mut response).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"], "automation Run action is daemon-owned");
    assert!(cmd_rx.try_recv().is_err());

    let selected = Arc::new(Mutex::new(None));
    let selected_for_responder = Arc::clone(&selected);
    slot.value
        .reduced
        .lock()
        .unwrap()
        .permissions
        .push(smelt_core::acp_session::LivePermission {
            question: "允许执行？".into(),
            tool_call_id: "tool-1".into(),
            options: vec![smelt_core::acp_session::PermissionOptionView {
                option_id: "allow".into(),
                name: "允许".into(),
                kind: smelt_core::acp_session::PermissionOptionKindView::AllowOnce,
            }],
            details: smelt_core::acp_session::ApprovalDetailsView::Generic,
            responder: Some(smelt_core::acp_conn::PermissionResponder::external(
                move |option| *selected_for_responder.lock().unwrap() = Some(option),
            )),
            raw_request_line: None,
        });
    let (server, client) = UnixStream::pair().unwrap();
    handle_acp_action(
        server,
        &serde_json::json!({
            "id": "acp-automation-action",
            "action": {"PermissionSelect": {
                "tool_call_id": "tool-1",
                "option_id": "allow"
            }},
        }),
        &acp_sessions,
        &new_event_hub(),
    );
    let mut response = String::new();
    BufReader::new(client).read_line(&mut response).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&response).unwrap()["ok"],
        true
    );
    assert_eq!(selected.lock().unwrap().as_deref(), Some("allow"));
}

#[test]
fn automation_run_accepts_only_its_daemon_delivery_id() {
    let session = make_acp_session("acp-automation-delivery", AcpSessionState::default());
    *session.conversation_binding.lock().unwrap() =
        Some(smelt_core::conversation::ConversationBinding::Automation {
            run_id: "run-owned".into(),
        });
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *session.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });

    let rejected = apply_acp_user_action(
        &session,
        smelt_core::acp_session::AcpUserAction::Prompt {
            text: "foreign delivery".into(),
            images: Vec::new(),
            delivery_id: Some("peer-delivery".into()),
        },
        &new_event_hub(),
    );
    assert_eq!(
        rejected.unwrap_err(),
        "automation Run input is daemon-owned"
    );
    assert!(cmd_rx.try_recv().is_err());

    apply_acp_user_action(
        &session,
        smelt_core::acp_session::AcpUserAction::Prompt {
            text: "owned delivery".into(),
            images: Vec::new(),
            delivery_id: Some("run-owned".into()),
        },
        &new_event_hub(),
    )
    .unwrap();
    assert!(matches!(
        cmd_rx.try_recv().unwrap(),
        smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } if text == "owned delivery"
    ));
}

#[test]
fn automation_run_keeps_elicitation_actions() {
    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-automation-elicit", || {
        make_acp_session_value("acp-automation-elicit", AcpSessionState::default())
    });
    *slot.value.conversation_binding.lock().unwrap() =
        Some(smelt_core::conversation::ConversationBinding::Automation {
            run_id: "run-2".into(),
        });
    slot.value.reduced.lock().unwrap().elicitation =
        Some(smelt_core::acp_session::LiveElicitation {
            message: "补充信息".into(),
            raw_fields: Vec::new(),
            chosen: BTreeMap::new(),
            text_values: BTreeMap::new(),
            responder: None,
            recovered_tool_call_id: None,
            raw_request_line: None,
        });

    let (server, client) = UnixStream::pair().unwrap();
    handle_acp_action(
        server,
        &serde_json::json!({
            "id": "acp-automation-elicit",
            "action": "ElicitationDismiss",
        }),
        &acp_sessions,
        &new_event_hub(),
    );
    let mut response = String::new();
    BufReader::new(client).read_line(&mut response).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&response).unwrap()["ok"],
        true
    );
    assert!(slot.value.reduced.lock().unwrap().elicitation.is_none());
}

#[test]
fn automation_run_rejects_freeform_conversation_input() {
    let session = make_acp_session("acp-automation", AcpSessionState::default());
    *session.conversation_binding.lock().unwrap() =
        Some(smelt_core::conversation::ConversationBinding::Automation {
            run_id: "run-1".into(),
        });

    let result = dispatch_conversation_input(
        &session,
        smelt_core::conversation::ConversationInput::new("second prompt".into(), Vec::new()),
        &new_event_hub(),
    );

    assert_eq!(
        result.unwrap_err().message,
        "automation Run input is daemon-owned"
    );
    assert!(session.reduced.lock().unwrap().entries().is_empty());
}

#[test]
fn conversation_submit_response_preserves_rejected_and_unknown_errors() {
    let rejected = conversation_submit_response(Err(
        smelt_core::conversation::ConversationSubmitError::rejected("not accepted"),
    ));
    assert_eq!(rejected["error"]["kind"], "rejected");

    let unknown = conversation_submit_response(Err(
        smelt_core::conversation::ConversationSubmitError::unknown("connection closed"),
    ));
    assert_eq!(unknown["error"]["kind"], "unknown");
}

#[test]
fn direct_input_drops_images_only_after_the_direct_route_is_selected() {
    let mut reduced = AcpSessionState::default();
    reduced.supports_image = false;
    let session = make_acp_session("acp-direct-images", reduced);
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *session.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });

    dispatch_conversation_input(
        &session,
        smelt_core::conversation::ConversationInput {
            submission_id: "submission-3".into(),
            text: "看图".into(),
            images: vec![smelt_core::acp_chat::AcpImage::new("image/png", "aW1hZ2U=")],
        },
        &new_event_hub(),
    )
    .unwrap();

    assert!(matches!(
        cmd_rx.try_recv().unwrap(),
        smelt_core::acp_conn::ConversationCommand::Prompt { text, images }
            if text.contains("当前智能体不支持图片输入") && images.is_empty()
    ));
}

#[test]
fn direct_agent_preset_remains_part_of_the_first_acp_prompt() {
    let session = make_acp_session("acp-direct-preset", AcpSessionState::default());
    *session.pending_agent_preset.lock().unwrap() = Some("你是量化研究员".into());
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *session.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });

    dispatch_conversation_input(
        &session,
        smelt_core::conversation::ConversationInput::new("分析组合风险".into(), Vec::new()),
        &new_event_hub(),
    )
    .unwrap();

    assert!(matches!(
        cmd_rx.try_recv().unwrap(),
        smelt_core::acp_conn::ConversationCommand::Prompt { text, .. }
            if text == "你是量化研究员\n\n分析组合风险"
    ));
    assert!(session.pending_agent_preset.lock().unwrap().is_none());
}

#[test]
fn submitted_direct_input_delivers_to_acp() {
    let session = make_acp_session("acp-direct-route", AcpSessionState::default());
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *session.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });

    dispatch_conversation_input(
        &session,
        smelt_core::conversation::ConversationInput {
            submission_id: "submission-6".into(),
            text: "local".to_string(),
            images: Vec::new(),
        },
        &new_event_hub(),
    )
    .unwrap();

    assert!(matches!(
        cmd_rx.try_recv().unwrap(),
        smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } if text == "local"
    ));
}

#[test]
fn resubmitted_input_with_the_same_submission_id_is_delivered_once() {
    let session = make_acp_session("acp-direct-resubmit", AcpSessionState::default());
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *session.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });
    let input = |id: &str, text: &str| smelt_core::conversation::ConversationInput {
        submission_id: id.into(),
        text: text.into(),
        images: Vec::new(),
    };
    let hub = new_event_hub();

    // 客户端没等到回包就重发同一条：第二次直接回成功，不再提交，也不进排队。
    dispatch_conversation_input(&session, input("phone-1", "第一条"), &hub).unwrap();
    dispatch_conversation_input(&session, input("phone-1", "第一条"), &hub).unwrap();

    let prompts: Vec<String> = std::iter::from_fn(|| cmd_rx.try_recv().ok())
        .filter_map(|command| match command {
            smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(prompts, ["第一条"]);
    assert!(
        session.pending_prompts.lock().unwrap().is_empty(),
        "重发不该变成排队的第二条"
    );
}

#[test]
fn recent_submissions_only_recognize_ids_actually_seen() {
    let mut recent = crate::acp_host::RecentSubmissions::default();
    recent.remember("a".into());
    assert!(recent.contains("a"));
    assert!(!recent.contains("b"), "不同的输入照常提交");
    recent.remember(String::new());
    assert!(!recent.contains(""), "没带 id 的输入不参与去重");
    for i in 0..64 {
        recent.remember(format!("n{i}"));
    }
    assert!(!recent.contains("a"), "只记最近一段");
    assert!(recent.contains("n63"));
}

#[test]
fn prompt_submitted_while_turn_active_is_deferred_until_turn_end() {
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Thinking;
    reduced.turn_started_at_ms = Some(1);
    let (slot, _) = acp_sessions.reserve_with("acp-queued-prompt", || {
        make_acp_session_value("acp-queued-prompt", reduced)
    });
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });
    slot.value.prompt_in_flight.store(true, Ordering::SeqCst);
    let subscribers = new_event_hub();

    apply_acp_user_action(
        &slot.value,
        smelt_core::acp_session::AcpUserAction::Prompt {
            text: "after current turn".to_string(),
            images: Vec::new(),
            delivery_id: None,
        },
        &subscribers,
    )
    .unwrap();
    assert!(cmd_rx.try_recv().is_err());
    assert_eq!(slot.value.pending_prompts.lock().unwrap().len(), 1);

    let _turn_completion = slot.value.turn_completion.lock().unwrap();
    slot.value.prompt_in_flight.store(false, Ordering::SeqCst);
    flush_pending_acp_prompt_locked(&slot.value, &subscribers);
    assert!(slot.value.pending_prompts.lock().unwrap().is_empty());
    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Prompt { text, images } => {
            assert_eq!(text, "after current turn");
            assert!(images.is_empty());
        }
        _ => panic!("expected deferred prompt command"),
    }
    assert!(matches!(
        slot.value.reduced.lock().unwrap().entries().last(),
        Some(AcpEntry::User(text)) if text == "after current turn"
    ));
}

/// 消息流里正文为 `text` 的用户气泡有几个。乐观回显最容易错的地方就是重发
/// 时写出第二条，所以这里数数量而不是只看最后一条。
fn user_bubbles(session: &AcpSession, text: &str) -> usize {
    session
        .reduced
        .lock()
        .unwrap()
        .entries()
        .iter()
        .filter(|entry| match entry {
            AcpEntry::User(entry_text) => entry_text == text,
            AcpEntry::UserWithImages {
                text: entry_text, ..
            } => entry_text == text,
            _ => false,
        })
        .count()
}

fn attach_prompt_handle(
    session: &AcpSession,
) -> smol::channel::Receiver<smelt_core::acp_conn::ConversationCommand> {
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *session.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: true,
        supports_compaction: false,
        supports_native_queue: true,
        supports_rewind: false,
    });
    cmd_rx
}

#[test]
fn background_notice_while_idle_is_a_status_line() {
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Idle;
    let (slot, _) = acp_sessions.reserve_with("acp-bg-idle", || {
        make_acp_session_value("acp-bg-idle", reduced)
    });
    let cmd_rx = attach_prompt_handle(&slot.value);
    let _turn_completion = slot.value.turn_completion.lock().unwrap();

    smelt_core::acp_session::apply_event(
        &mut slot.value.reduced.lock().unwrap(),
        smelt_core::acp_conn::ConversationEvent::BackgroundNotice(task_note()),
    );

    assert!(cmd_rx.try_recv().is_err(), "结束说明不能变成一条 prompt");
    assert!(slot.value.pending_prompts.lock().unwrap().is_empty());
    assert!(!slot.value.prompt_in_flight.load(Ordering::SeqCst));
    assert!(slot.value.unaccepted_prompt.lock().unwrap().is_none());
    let state = slot.value.reduced.lock().unwrap();
    assert!(state.turn_started_at_ms.is_none());
    assert!(matches!(state.phase, DaemonPhase::Idle));
    assert!(matches!(
        state.entries().last(),
        Some(AcpEntry::TaskNote(note)) if note.id == "bg-1" && note.exit_code == Some(1)
    ));
}

#[test]
fn background_notice_during_a_turn_does_not_queue_a_prompt() {
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Thinking;
    reduced.turn_started_at_ms = Some(1);
    let (slot, _) = acp_sessions.reserve_with("acp-bg-busy", || {
        make_acp_session_value("acp-bg-busy", reduced)
    });
    let cmd_rx = attach_prompt_handle(&slot.value);
    slot.value.prompt_in_flight.store(true, Ordering::SeqCst);
    let _turn_completion = slot.value.turn_completion.lock().unwrap();

    smelt_core::acp_session::apply_event(
        &mut slot.value.reduced.lock().unwrap(),
        smelt_core::acp_conn::ConversationEvent::BackgroundNotice(task_note()),
    );

    assert!(cmd_rx.try_recv().is_err());
    assert!(slot.value.pending_prompts.lock().unwrap().is_empty());
    assert!(slot.value.prompt_in_flight.load(Ordering::SeqCst));
    let state = slot.value.reduced.lock().unwrap();
    assert_eq!(state.turn_started_at_ms, Some(1));
    assert!(matches!(state.phase, DaemonPhase::Thinking));
    assert!(matches!(
        state.entries().last(),
        Some(AcpEntry::TaskNote(note)) if note.exit_code == Some(1)
    ));
}

fn task_note() -> smelt_core::acp_chat::TaskNote {
    smelt_core::acp_chat::TaskNote {
        kind: smelt_core::acp_chat::TaskNoteKind::BackgroundTask,
        id: "bg-1".into(),
        title: "编译".into(),
        status: "failed".into(),
        exit_code: Some(1),
        output_tail: "error: boom".into(),
        runs: Vec::new(),
    }
}

#[test]
fn pi_cancel_does_not_admit_the_next_prompt_until_the_provider_is_idle() {
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Idle;
    let (slot, _) = acp_sessions.reserve_with("acp-pi-cancel-gate", || {
        let session = make_acp_session_value("acp-pi-cancel-gate", reduced);
        *session.launch_spec.lock().unwrap() = Some(pi_launch());
        session
    });
    let cmd_rx = attach_prompt_handle(&slot.value);
    slot.value.prompt_in_flight.store(true, Ordering::SeqCst);
    slot.value
        .pending_prompts
        .lock()
        .unwrap()
        .push_back(QueuedAcpPrompt {
            id: 1,
            text: "下一句".to_string(),
            images: Vec::new(),
            delivery_id: None,
            origin: AcpPromptOrigin::User,
            echoed: false,
        });
    let subscribers = new_event_hub();

    settle_acp_turn_locked(&slot.value, true, false, false, &subscribers);
    assert!(cmd_rx.try_recv().is_err());
    assert_eq!(slot.value.pending_prompts.lock().unwrap().len(), 1);
    assert!(slot.value.prompt_in_flight.load(Ordering::SeqCst));

    settle_acp_turn_locked(&slot.value, false, false, true, &subscribers);
    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } => {
            assert_eq!(text, "下一句");
        }
        _ => panic!("provider idle must release exactly one prompt"),
    }
    assert!(cmd_rx.try_recv().is_err());
    assert!(slot.value.prompt_in_flight.load(Ordering::SeqCst));
    let state = slot.value.reduced.lock().unwrap();
    // 排队中的消息不在消息流里（GUI 显示在队列区），真正派发时才落气泡。
    assert!(matches!(
        state.entries().last(),
        Some(AcpEntry::User(text)) if text == "下一句"
    ));
    assert!(state.turn_started_at_ms.is_some());
    drop(state);
    let unaccepted = slot.value.unaccepted_prompt.lock().unwrap();
    assert_eq!(
        unaccepted.as_ref().map(|prompt| prompt.text.as_str()),
        Some("下一句")
    );
}

#[test]
fn turn_based_agent_queue_is_in_the_snapshot_and_user_prompts_can_be_withdrawn() {
    use smelt_core::acp_session::AcpUserAction;
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Idle;
    let (slot, _) = acp_sessions.reserve_with("acp-withdraw-queued", || {
        make_acp_session_value("acp-withdraw-queued", reduced)
    });
    let cmd_rx = attach_prompt_handle(&slot.value);
    // Claude / Codex 这类通用 ACP agent 不能中途插话，回合在跑时只能排在 daemon 闸门后面。
    slot.value
        .handle
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .supports_mid_turn_input = false;
    let subscribers = new_event_hub();
    let prompt = |text: &str, images, delivery_id: Option<&str>| AcpUserAction::Prompt {
        text: text.to_string(),
        images,
        delivery_id: delivery_id.map(str::to_string),
    };
    let image = smelt_core::acp_chat::AcpImage::new("image/png", "aW1n");

    apply_acp_user_action(
        &slot.value,
        prompt("第一句", Vec::new(), None),
        &subscribers,
    )
    .unwrap();
    assert!(matches!(
        cmd_rx.try_recv().unwrap(),
        smelt_core::acp_conn::ConversationCommand::Prompt { .. }
    ));
    apply_acp_user_action(
        &slot.value,
        prompt("第二句", vec![image.clone()], None),
        &subscribers,
    )
    .unwrap();
    apply_acp_user_action(
        &slot.value,
        prompt("任务投递", Vec::new(), Some("run-1")),
        &subscribers,
    )
    .unwrap();
    assert!(cmd_rx.try_recv().is_err(), "回合在跑，不能并发派发");

    // 排队的消息在快照里：客户端看得见，不会以为发丢了。
    let queued = slot
        .value
        .reduced
        .lock()
        .unwrap()
        .to_snapshot(false)
        .pending_prompts;
    assert_eq!(
        queued
            .iter()
            .map(|item| (item.text.as_str(), item.image_count, item.withdrawable))
            .collect::<Vec<_>>(),
        vec![("第二句", 1, true), ("任务投递", 0, false)]
    );
    assert_ne!(queued[0].id, queued[1].id);

    // 外部投递归守护进程，输入框撤不走。
    assert!(
        apply_acp_user_action(
            &slot.value,
            AcpUserAction::WithdrawPendingPrompt { id: queued[1].id },
            &subscribers,
        )
        .is_err()
    );
    apply_acp_user_action(
        &slot.value,
        AcpUserAction::WithdrawPendingPrompt { id: queued[0].id },
        &subscribers,
    )
    .unwrap();
    {
        let state = slot.value.reduced.lock().unwrap();
        assert_eq!(
            state
                .pending_prompts
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>(),
            vec!["任务投递"]
        );
        // 正文和附图回到输入框，消息流里没有这条。
        assert_eq!(state.composer_restore_texts, vec!["第二句".to_string()]);
        assert_eq!(state.composer_restore_images, vec![image]);
        assert!(!state.entries().iter().any(|entry| matches!(
            entry,
            AcpEntry::User(text) | AcpEntry::UserWithImages { text, .. } if text == "第二句"
        )));
    }
    // 撤过的不能再撤第二次，也不会在回合结束后发出去。
    assert!(
        apply_acp_user_action(
            &slot.value,
            AcpUserAction::WithdrawPendingPrompt { id: queued[0].id },
            &subscribers,
        )
        .is_err()
    );
    {
        let mut state = slot.value.reduced.lock().unwrap();
        state.phase = DaemonPhase::Idle;
        state.turn_started_at_ms = None;
    }
    settle_acp_turn_locked(&slot.value, true, false, false, &subscribers);
    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } => {
            assert_eq!(text, "任务投递");
        }
        _ => panic!("回合结束只放出队首一条"),
    }
    assert!(cmd_rx.try_recv().is_err());
    assert!(
        slot.value
            .reduced
            .lock()
            .unwrap()
            .pending_prompts
            .is_empty()
    );
}

#[test]
fn pi_prompt_is_requeued_when_pi_refuses_it_and_is_not_a_failed_turn() {
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Idle;
    let (slot, _) = acp_sessions.reserve_with("acp-pi-refuse", || {
        let session = make_acp_session_value("acp-pi-refuse", reduced);
        *session.launch_spec.lock().unwrap() = Some(pi_launch());
        session
    });
    let cmd_rx = attach_prompt_handle(&slot.value);
    let subscribers = new_event_hub();
    let _turn_completion = slot.value.turn_completion.lock().unwrap();

    send_acp_prompt_reserved(
        &slot.value,
        "再问一句".into(),
        Vec::new(),
        None,
        AcpPromptOrigin::User,
        false,
        &subscribers,
    )
    .unwrap();
    assert!(matches!(
        cmd_rx.try_recv(),
        Ok(smelt_core::acp_conn::ConversationCommand::Prompt { .. })
    ));
    assert_eq!(user_bubbles(&slot.value, "再问一句"), 1);
    assert!(slot.value.unaccepted_prompt.lock().unwrap().is_some());

    requeue_unaccepted_prompt(&slot.value);
    smelt_core::acp_session::note_provider_still_running(&mut slot.value.reduced.lock().unwrap());
    assert_eq!(slot.value.pending_prompts.lock().unwrap().len(), 1);
    assert!(slot.value.prompt_in_flight.load(Ordering::SeqCst));
    assert!(slot.value.unaccepted_prompt.lock().unwrap().is_none());
    {
        let state = slot.value.reduced.lock().unwrap();
        assert!(matches!(state.phase, DaemonPhase::Thinking));
        assert_eq!(
            state.status_line.as_deref(),
            Some("已排队，这一轮结束后再发")
        );
    }
    // 「Pi 还在跑」不是用户没说过：气泡留在原地，不撤也不重写。
    assert_eq!(user_bubbles(&slot.value, "再问一句"), 1);
    settle_acp_turn_locked(&slot.value, false, false, false, &subscribers);
    assert!(cmd_rx.try_recv().is_err());

    smelt_core::acp_session::apply_event(
        &mut slot.value.reduced.lock().unwrap(),
        smelt_core::acp_conn::ConversationEvent::TurnEnded(
            agent_client_protocol::schema::v1::StopReason::EndTurn,
        ),
    );
    settle_acp_turn_locked(&slot.value, true, false, true, &subscribers);
    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } => {
            assert_eq!(text, "再问一句");
        }
        _ => panic!("idle provider must send the refused prompt once"),
    }
    // 重发的是同一条，消息流里永远只能有一个气泡。
    assert_eq!(user_bubbles(&slot.value, "再问一句"), 1);
    assert!(
        !slot.value.reduced.lock().unwrap().entries().iter().any(
            |entry| matches!(entry, AcpEntry::Assistant { text, .. } if text.contains("回合失败"))
        ),
        "拒绝不能写成回合失败"
    );
}

#[test]
fn pi_refusal_and_idle_in_the_same_batch_sends_once_without_a_spinner() {
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Idle;
    let (slot, _) = acp_sessions.reserve_with("acp-pi-refuse-idle", || {
        let session = make_acp_session_value("acp-pi-refuse-idle", reduced);
        *session.launch_spec.lock().unwrap() = Some(pi_launch());
        session
    });
    let cmd_rx = attach_prompt_handle(&slot.value);
    *slot.value.unaccepted_prompt.lock().unwrap() = Some(QueuedAcpPrompt {
        id: 1,
        text: "再问一句".to_string(),
        images: Vec::new(),
        delivery_id: None,
        origin: AcpPromptOrigin::User,
        echoed: false,
    });
    slot.value.prompt_in_flight.store(true, Ordering::SeqCst);
    let subscribers = new_event_hub();

    requeue_unaccepted_prompt(&slot.value);
    settle_acp_turn_locked(&slot.value, false, false, true, &subscribers);
    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } => {
            assert_eq!(text, "再问一句");
        }
        _ => panic!("same-batch idle must flush the refused prompt"),
    }
    assert!(matches!(
        slot.value.reduced.lock().unwrap().phase,
        DaemonPhase::Idle
    ));
    settle_acp_turn_locked(&slot.value, false, false, true, &subscribers);
    assert!(
        cmd_rx.try_recv().is_err(),
        "一条还没被收下的 prompt 不能被下一次空闲信号再发一遍"
    );
}

#[test]
fn pi_prompt_is_echoed_without_waiting_for_pi_to_accept() {
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Idle;
    let (slot, _) = acp_sessions.reserve_with("acp-pi-accept", || {
        let session = make_acp_session_value("acp-pi-accept", reduced);
        *session.launch_spec.lock().unwrap() = Some(pi_launch());
        session
    });
    let cmd_rx = attach_prompt_handle(&slot.value);
    let subscribers = new_event_hub();

    send_acp_prompt_reserved(
        &slot.value,
        "卡住了吗".into(),
        Vec::new(),
        None,
        AcpPromptOrigin::User,
        false,
        &subscribers,
    )
    .unwrap();
    assert!(matches!(
        cmd_rx.try_recv(),
        Ok(smelt_core::acp_conn::ConversationCommand::Prompt { .. })
    ));
    {
        // Pi 的 admission 可能要数秒（扩展在 before_agent_start 里做慢活）。
        // 用户确实提交过这条，气泡不能等到那时候才出现。
        let state = slot.value.reduced.lock().unwrap();
        assert!(matches!(
            state.entries().last(),
            Some(AcpEntry::User(text)) if text == "卡住了吗"
        ));
        assert!(matches!(state.phase, DaemonPhase::Thinking));
        assert!(state.turn_started_at_ms.is_some());
    }
    assert!(slot.value.unaccepted_prompt.lock().unwrap().is_some());

    slot.value.reduced.lock().unwrap().status_line = Some("已排队，这一轮结束后再发".to_string());
    assert!(accept_unaccepted_prompt(&slot.value));
    let state = slot.value.reduced.lock().unwrap();
    // 收下只是确认：不能再写第二个气泡。
    assert_eq!(
        state
            .entries()
            .iter()
            .filter(|entry| matches!(entry, AcpEntry::User(text) if text == "卡住了吗"))
            .count(),
        1
    );
    assert!(matches!(state.phase, DaemonPhase::Thinking));
    assert!(state.status_line.is_none());
    assert!(state.turn_started_at_ms.is_some());
}

#[test]
fn real_prompt_failure_returns_the_text_instead_of_retrying() {
    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-pi-fail", || {
        let session = make_acp_session_value("acp-pi-fail", AcpSessionState::default());
        *session.launch_spec.lock().unwrap() = Some(pi_launch());
        session
    });
    *slot.value.unaccepted_prompt.lock().unwrap() = Some(QueuedAcpPrompt {
        id: 1,
        text: "再试一次".to_string(),
        images: Vec::new(),
        delivery_id: None,
        origin: AcpPromptOrigin::User,
        echoed: true,
    });
    smelt_core::acp_session::note_prompt_sent(
        &mut slot.value.reduced.lock().unwrap(),
        "再试一次".to_string(),
        Vec::new(),
    );

    restore_unaccepted_to_composer(&slot.value);
    assert!(slot.value.pending_prompts.lock().unwrap().is_empty());
    assert!(slot.value.unaccepted_prompt.lock().unwrap().is_none());
    assert_eq!(
        slot.value.reduced.lock().unwrap().composer_restore_texts,
        ["再试一次".to_string()]
    );
    // 正文回了输入框，消息流里就不能再留着它。
    assert_eq!(user_bubbles(&slot.value, "再试一次"), 0);
    assert!(
        slot.value
            .reduced
            .lock()
            .unwrap()
            .turn_started_at_ms
            .is_none()
    );
}

#[test]
fn cancel_before_accept_restores_the_images_with_the_text() {
    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-pi-cancel-images", || {
        make_acp_session_value("acp-pi-cancel-images", AcpSessionState::default())
    });
    let image = smelt_core::acp_chat::AcpImage::new("image/png", "aGk=");
    *slot.value.unaccepted_prompt.lock().unwrap() = Some(QueuedAcpPrompt {
        id: 1,
        text: "看这张".to_string(),
        images: vec![image.clone()],
        delivery_id: None,
        origin: AcpPromptOrigin::User,
        echoed: true,
    });
    smelt_core::acp_session::note_prompt_sent(
        &mut slot.value.reduced.lock().unwrap(),
        "看这张".to_string(),
        vec![image.clone()],
    );
    park_unaccepted_after_cancel(&slot.value);
    {
        let state = slot.value.reduced.lock().unwrap();
        assert_eq!(state.composer_restore_texts, ["看这张".to_string()]);
        assert_eq!(state.composer_restore_images, vec![image.clone()]);
    }
    assert_eq!(user_bubbles(&slot.value, "看这张"), 0);

    *slot.value.unaccepted_prompt.lock().unwrap() = Some(QueuedAcpPrompt {
        id: 1,
        text: "   ".to_string(),
        images: vec![image.clone()],
        delivery_id: None,
        origin: AcpPromptOrigin::User,
        echoed: false,
    });
    park_unaccepted_after_cancel(&slot.value);
    let state = slot.value.reduced.lock().unwrap();
    assert!(state.composer_restore_texts.is_empty());
    assert_eq!(state.composer_restore_images, vec![image]);
    assert!(state.composer_restore_revision >= 2);
}

#[test]
fn real_prompt_failure_restores_the_images_with_the_text() {
    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-pi-fail-images", || {
        let session = make_acp_session_value("acp-pi-fail-images", AcpSessionState::default());
        *session.launch_spec.lock().unwrap() = Some(pi_launch());
        session
    });
    let image = smelt_core::acp_chat::AcpImage::new("image/png", "aGk=");
    *slot.value.unaccepted_prompt.lock().unwrap() = Some(QueuedAcpPrompt {
        id: 1,
        text: "再试一次".to_string(),
        images: vec![image.clone()],
        delivery_id: None,
        origin: AcpPromptOrigin::User,
        echoed: true,
    });
    smelt_core::acp_session::note_prompt_sent(
        &mut slot.value.reduced.lock().unwrap(),
        "再试一次".to_string(),
        vec![image.clone()],
    );
    restore_unaccepted_to_composer(&slot.value);
    assert_eq!(user_bubbles(&slot.value, "再试一次"), 0);
    let state = slot.value.reduced.lock().unwrap();
    assert_eq!(state.composer_restore_texts, ["再试一次".to_string()]);
    assert_eq!(state.composer_restore_images, vec![image]);
    assert!(slot.value.pending_prompts.lock().unwrap().is_empty());
}

#[test]
fn cancel_before_accept_restores_user_text() {
    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-pi-cancel-unaccepted", || {
        make_acp_session_value("acp-pi-cancel-unaccepted", AcpSessionState::default())
    });
    *slot.value.unaccepted_prompt.lock().unwrap() = Some(QueuedAcpPrompt {
        id: 1,
        text: "用户刚发的".to_string(),
        images: Vec::new(),
        delivery_id: None,
        origin: AcpPromptOrigin::User,
        echoed: true,
    });
    smelt_core::acp_session::note_prompt_sent(
        &mut slot.value.reduced.lock().unwrap(),
        "用户刚发的".to_string(),
        Vec::new(),
    );
    park_unaccepted_after_cancel(&slot.value);
    assert_eq!(
        slot.value.reduced.lock().unwrap().composer_restore_texts,
        ["用户刚发的".to_string()]
    );
    assert_eq!(user_bubbles(&slot.value, "用户刚发的"), 0);
    assert!(slot.value.pending_prompts.lock().unwrap().is_empty());
}

#[test]
fn pi_queue_hold_clears_when_the_provider_becomes_idle() {
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Idle;
    let (slot, _) = acp_sessions.reserve_with("acp-pi-hold", || {
        let session = make_acp_session_value("acp-pi-hold", reduced);
        *session.launch_spec.lock().unwrap() = Some(pi_launch());
        session
    });
    let cmd_rx = attach_prompt_handle(&slot.value);
    *slot.value.unaccepted_prompt.lock().unwrap() = Some(QueuedAcpPrompt {
        id: 1,
        text: "再问一句".to_string(),
        images: Vec::new(),
        delivery_id: None,
        origin: AcpPromptOrigin::User,
        echoed: false,
    });
    let subscribers = new_event_hub();

    requeue_unaccepted_prompt(&slot.value);
    remember_provider_hold(&slot.value);
    assert!(matches!(
        slot.value.reduced.lock().unwrap().phase,
        DaemonPhase::Thinking
    ));
    settle_acp_turn_locked(&slot.value, false, false, false, &subscribers);
    assert!(cmd_rx.try_recv().is_err());

    release_provider_hold(&slot.value);
    settle_acp_turn_locked(&slot.value, false, false, true, &subscribers);
    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } => {
            assert_eq!(text, "再问一句");
        }
        _ => panic!("占位转圈收回后，空闲信号要放出这条消息"),
    }
    assert!(matches!(
        slot.value.reduced.lock().unwrap().phase,
        DaemonPhase::Idle
    ));
    assert!(slot.value.reduced.lock().unwrap().status_line.is_none());
}

#[test]
fn prompt_submitted_while_turn_active_is_steered_when_driver_supports_it() {
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Thinking;
    reduced.turn_started_at_ms = Some(1);
    let (slot, _) = acp_sessions.reserve_with("acp-steered-prompt", || {
        make_acp_session_value("acp-steered-prompt", reduced)
    });
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(1)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: true,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });
    slot.value.prompt_in_flight.store(true, Ordering::SeqCst);
    let subscribers = new_event_hub();

    apply_acp_user_action(
        &slot.value,
        smelt_core::acp_session::AcpUserAction::Prompt {
            text: "换个方向".to_string(),
            images: Vec::new(),
            delivery_id: None,
        },
        &subscribers,
    )
    .unwrap();

    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Steer { text, images } => {
            assert_eq!(text, "换个方向");
            assert!(images.is_empty());
        }
        _ => panic!("expected steer command"),
    }
    // 插入不占闸门、不进 daemon 回合后队列，也不能把正在跑的那一轮改成新回合。
    // 此时只进入 steering 队列；Pi 真正吃掉之前不得写进会话。
    assert!(slot.value.pending_prompts.lock().unwrap().is_empty());
    assert!(slot.value.prompt_in_flight.load(Ordering::SeqCst));
    let state = slot.value.reduced.lock().unwrap();
    assert_eq!(state.turn_started_at_ms, Some(1));
    assert!(matches!(state.phase, DaemonPhase::Thinking));
    assert_eq!(state.queued_steering, ["换个方向"]);
    assert!(
        !state
            .entries()
            .iter()
            .any(|entry| matches!(entry, AcpEntry::User(text) if text == "换个方向")),
        "待插入不得进会话"
    );
}

#[test]
fn prompt_after_cancel_waits_for_provider_idle_instead_of_reentering_the_queue() {
    // 立即发送会先 cancel。Pi 立刻把界面收成 Idle，但闸门要等 provider 空闲才放开。
    // 这条替换 prompt 若再 steer，会回到「当前回合」；再点一次就会连发两遍。
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Idle;
    reduced.turn_outcome = Some(smelt_core::acp_session::AcpTurnOutcome::Cancelled);
    reduced.queued_steering.push("但是我想字数一致".into());
    let (slot, _) = acp_sessions.reserve_with("acp-immediate-after-cancel", || {
        make_acp_session_value("acp-immediate-after-cancel", reduced)
    });
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(1)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: true,
        supports_compaction: false,
        supports_native_queue: true,
        supports_rewind: false,
    });
    slot.value.prompt_in_flight.store(true, Ordering::SeqCst);
    let subscribers = new_event_hub();

    apply_acp_user_action(
        &slot.value,
        smelt_core::acp_session::AcpUserAction::Prompt {
            text: "但是我想字数一致".to_string(),
            images: Vec::new(),
            delivery_id: None,
        },
        &subscribers,
    )
    .unwrap();

    assert!(
        cmd_rx.try_recv().is_err(),
        "取消后的替换 prompt 不能再 steer 回当前回合"
    );
    assert_eq!(
        slot.value.pending_prompts.lock().unwrap().len(),
        1,
        "要停在 daemon 队列，等 provider 空闲再发一次"
    );
    assert_eq!(
        slot.value.reduced.lock().unwrap().queued_steering,
        ["但是我想字数一致".to_string()]
    );

    let _turn_completion = slot.value.turn_completion.lock().unwrap();
    settle_acp_turn_locked(&slot.value, false, false, true, &subscribers);
    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } => {
            assert_eq!(text, "但是我想字数一致");
        }
        _ => panic!("expected a single prompt"),
    }
    assert!(cmd_rx.try_recv().is_err(), "只能发一次");
    assert!(slot.value.pending_prompts.lock().unwrap().is_empty());
}

#[test]
fn deliver_queued_now_sends_the_claimed_item_once_after_the_provider_is_idle() {
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Thinking;
    reduced.turn_started_at_ms = Some(1);
    reduced.supports_native_queue = true;
    reduced.queued_steering = vec!["但是我想字数一致".into(), "别的".into()];
    reduced.queued_steering_images = vec![
        vec![smelt_core::acp_chat::AcpImage::new("image/png", "aGk=")],
        Vec::new(),
    ];
    let (slot, _) = acp_sessions.reserve_with("acp-deliver-now", || {
        let session = make_acp_session_value("acp-deliver-now", reduced);
        *session.launch_spec.lock().unwrap() = Some(pi_launch());
        session
    });
    let cmd_rx = attach_prompt_handle(&slot.value);
    slot.value.prompt_in_flight.store(true, Ordering::SeqCst);
    let subscribers = new_event_hub();
    let deliver = |text: &str| {
        apply_acp_user_action(
            &slot.value,
            smelt_core::acp_session::AcpUserAction::DeliverQueuedNow {
                text: text.to_string(),
            },
            &subscribers,
        )
    };

    deliver("但是我想字数一致").unwrap();
    assert!(
        matches!(
            cmd_rx.try_recv(),
            Ok(smelt_core::acp_conn::ConversationCommand::Cancel)
        ),
        "运行中的立即发送只停当前回答，不能把这条再 steer 回去"
    );
    assert!(cmd_rx.try_recv().is_err());
    deliver("但是我想字数一致").unwrap();
    assert!(cmd_rx.try_recv().is_err(), "再点一次不能再取消或再发");
    assert!(deliver("别的").is_err(), "另一条要等这一条落地");

    let _turn = slot.value.turn_completion.lock().unwrap();
    try_flush_immediate_delivery(&slot.value, &subscribers);
    assert!(cmd_rx.try_recv().is_err(), "还原没到不能发");

    let mut texts = vec!["但是我想字数一致".to_string(), "别的".to_string()];
    let mut images = vec![
        vec![smelt_core::acp_chat::AcpImage::new("image/png", "aGk=")],
        Vec::new(),
    ];
    note_immediate_queue_restore(&slot.value, &mut texts, &mut images);
    assert_eq!(texts, vec!["别的".to_string()]);
    assert_eq!(images, vec![Vec::new()], "认领那条的图不能再还回输入框");
    smelt_core::acp_session::apply_event(
        &mut slot.value.reduced.lock().unwrap(),
        smelt_core::acp_conn::ConversationEvent::ComposerRestore {
            revision: 1,
            texts,
            images,
        },
    );
    slot.value.reduced.lock().unwrap().phase = DaemonPhase::Idle;
    try_flush_immediate_delivery(&slot.value, &subscribers);
    assert!(
        cmd_rx.try_recv().is_err(),
        "provider 还没空闲，闸门不能放行"
    );

    settle_acp_turn_locked(&slot.value, false, false, true, &subscribers);
    match cmd_rx.try_recv() {
        Ok(smelt_core::acp_conn::ConversationCommand::Prompt { text, images }) => {
            assert_eq!(text, "但是我想字数一致");
            assert_eq!(images.len(), 1);
        }
        Ok(_) => panic!("expected one prompt, got another command"),
        Err(_) => panic!("expected one prompt, channel was empty"),
    }
    assert!(cmd_rx.try_recv().is_err(), "只能发一次");
    assert!(slot.value.pending_prompts.lock().unwrap().is_empty());
    assert_eq!(
        slot.value.reduced.lock().unwrap().composer_restore_texts,
        vec!["别的".to_string()]
    );
}

#[test]
fn deliver_queued_now_does_not_resend_when_the_provider_already_took_the_item() {
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Thinking;
    reduced.supports_native_queue = true;
    reduced.queued_steering = vec!["但是我想字数一致".into()];
    let (slot, _) = acp_sessions.reserve_with("acp-deliver-consumed", || {
        let session = make_acp_session_value("acp-deliver-consumed", reduced);
        *session.launch_spec.lock().unwrap() = Some(pi_launch());
        session
    });
    let cmd_rx = attach_prompt_handle(&slot.value);
    slot.value.prompt_in_flight.store(true, Ordering::SeqCst);
    let subscribers = new_event_hub();
    apply_acp_user_action(
        &slot.value,
        smelt_core::acp_session::AcpUserAction::DeliverQueuedNow {
            text: "但是我想字数一致".into(),
        },
        &subscribers,
    )
    .unwrap();
    assert!(matches!(
        cmd_rx.try_recv(),
        Ok(smelt_core::acp_conn::ConversationCommand::Cancel)
    ));

    let mut texts = vec!["别的".to_string()];
    let mut images = vec![Vec::new()];
    note_immediate_queue_restore(&slot.value, &mut texts, &mut images);
    assert_eq!(texts, vec!["别的".to_string()]);
    slot.value.reduced.lock().unwrap().phase = DaemonPhase::Idle;
    let _turn = slot.value.turn_completion.lock().unwrap();
    settle_acp_turn_locked(&slot.value, false, false, true, &subscribers);
    assert!(
        cmd_rx.try_recv().is_err(),
        "provider 已经吃掉这一条时不能再发一次"
    );
}

#[test]
fn deliver_queued_now_while_idle_waits_for_the_queue_clear_before_sending() {
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Idle;
    reduced.supports_native_queue = true;
    reduced.queued_follow_up = vec!["总结".into()];
    let (slot, _) = acp_sessions.reserve_with("acp-deliver-idle", || {
        make_acp_session_value("acp-deliver-idle", reduced)
    });
    let cmd_rx = attach_prompt_handle(&slot.value);
    let subscribers = new_event_hub();
    apply_acp_user_action(
        &slot.value,
        smelt_core::acp_session::AcpUserAction::DeliverQueuedNow {
            text: "总结".into(),
        },
        &subscribers,
    )
    .unwrap();
    assert!(
        matches!(
            cmd_rx.try_recv(),
            Ok(smelt_core::acp_conn::ConversationCommand::ClearQueue)
        ),
        "空闲时只清队列，不取消一个已经结束的回合"
    );

    try_flush_immediate_delivery(&slot.value, &subscribers);
    assert!(cmd_rx.try_recv().is_err(), "清队列的回执没到不能发");
    assert!(!slot.value.prompt_in_flight.load(Ordering::SeqCst));

    let mut texts = vec!["总结".to_string()];
    let mut images = vec![Vec::new()];
    note_immediate_queue_restore(&slot.value, &mut texts, &mut images);
    assert!(texts.is_empty());
    assert!(images.is_empty());
    try_flush_immediate_delivery(&slot.value, &subscribers);
    assert!(matches!(
        cmd_rx.try_recv(),
        Ok(smelt_core::acp_conn::ConversationCommand::Prompt { text, .. }) if text == "总结"
    ));
    assert!(cmd_rx.try_recv().is_err(), "只能发一次");
}

fn pi_launch() -> smelt_core::agent_kind::ConversationLaunchSpec {
    smelt_core::agent_kind::ConversationLaunchSpec::from_command("smelt-pi-agent")
}

#[test]
fn pi_session_commands_are_not_steered_during_a_turn() {
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Thinking;
    reduced.turn_started_at_ms = Some(1);
    let (slot, _) = acp_sessions.reserve_with("acp-pi-slash", || {
        let session = make_acp_session_value("acp-pi-slash", reduced);
        *session.launch_spec.lock().unwrap() = Some(pi_launch());
        session
    });
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(1)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: true,
        supports_compaction: true,
        supports_native_queue: true,
        supports_rewind: false,
    });
    slot.value.prompt_in_flight.store(true, Ordering::SeqCst);
    let subscribers = new_event_hub();

    apply_acp_user_action(
        &slot.value,
        smelt_core::acp_session::AcpUserAction::Prompt {
            text: "/reload".to_string(),
            images: Vec::new(),
            delivery_id: None,
        },
        &subscribers,
    )
    .unwrap();
    apply_acp_user_action(
        &slot.value,
        smelt_core::acp_session::AcpUserAction::FollowUp {
            text: "/handoff 关注测试".to_string(),
            images: Vec::new(),
            delivery_id: None,
        },
        &subscribers,
    )
    .unwrap();

    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } => {
            assert_eq!(text, "/reload");
        }
        _ => panic!("reload must stay a prompt so the driver can reject it"),
    }
    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Compact {
            custom_instructions,
        } => {
            assert_eq!(custom_instructions.as_deref(), Some("关注测试"));
        }
        _ => panic!("handoff must be native compact"),
    }
    assert!(slot.value.prompt_in_flight.load(Ordering::SeqCst));
    assert_eq!(
        slot.value
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .in_flight_rpc
            .load(Ordering::SeqCst),
        3
    );
    let state = slot.value.reduced.lock().unwrap();
    assert_eq!(state.turn_started_at_ms, Some(1));
    assert!(state.queued_steering.is_empty());
    assert!(state.entries().is_empty());
}

#[test]
fn non_pi_slash_text_is_still_steered() {
    let acp_sessions = new_test_acp_sessions();
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Thinking;
    reduced.turn_started_at_ms = Some(1);
    let (slot, _) = acp_sessions.reserve_with("acp-claude-slash", || {
        let session = make_acp_session_value("acp-claude-slash", reduced);
        *session.launch_spec.lock().unwrap() =
            Some(smelt_core::agent_kind::ConversationLaunchSpec::from_command("claude --acp"));
        session
    });
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(1)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: true,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });
    slot.value.prompt_in_flight.store(true, Ordering::SeqCst);

    apply_acp_user_action(
        &slot.value,
        smelt_core::acp_session::AcpUserAction::Prompt {
            text: "/reload".to_string(),
            images: Vec::new(),
            delivery_id: None,
        },
        &new_event_hub(),
    )
    .unwrap();

    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Steer { text, .. } => {
            assert_eq!(text, "/reload");
        }
        _ => panic!("non-pi slash text is ordinary input"),
    }
}

#[test]
fn watchdog_and_late_tool_update_serialize_prompt_dispatch() {
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Idle;
    reduced.completed_unread = true;
    reduced.entries_mut().push(AcpEntry::ToolCall {
        id: "shell".into(),
        title: "Reading shell output".into(),
        kind: ToolKind::Execute,
        status: ToolCallStatus::InProgress,
        output: Vec::new(),
        children: Vec::new(),
        subagents: Vec::new(),
    });
    let session = make_acp_session("acp-dangling-tool", reduced);
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *session.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });
    session.prompt_in_flight.store(true, Ordering::SeqCst);
    session
        .pending_prompts
        .lock()
        .unwrap()
        .push_back(QueuedAcpPrompt {
            id: 1,
            text: "prompt B".to_string(),
            images: Vec::new(),
            delivery_id: None,
            origin: AcpPromptOrigin::User,
            echoed: false,
        });
    let subscribers = new_event_hub();
    let (late_started_tx, late_started_rx) = std::sync::mpsc::channel();
    let (late_done_tx, late_done_rx) = std::sync::mpsc::channel();
    let (prompt_started_tx, prompt_started_rx) = std::sync::mpsc::channel();
    let (prompt_done_tx, prompt_done_rx) = std::sync::mpsc::channel();

    // event drain 会先应用迟到更新、再进入回合收尾。让它在 watchdog 的
    // 收尾锁外等待，模拟 15 秒边界刚好收到 ToolFinished 的竞态。
    let turn_completion = session.turn_completion.lock().unwrap();
    {
        let mut reduced = session.reduced.lock().unwrap();
        assert!(smelt_core::acp_session::finalize_dangling_tool_calls(&mut reduced).is_some());
    }
    let late_session = Arc::clone(&session);
    let late_subscribers = Arc::clone(&subscribers);
    let late_update = thread::spawn(move || {
        {
            let mut reduced = late_session.reduced.lock().unwrap();
            smelt_core::acp_session::apply_event(
                &mut reduced,
                smelt_core::acp_conn::ConversationEvent::ToolFinished {
                    id: "shell".into(),
                    status: ToolCallStatus::Completed,
                    output: Vec::new(),
                },
            );
        }
        late_started_tx.send(()).unwrap();
        let _turn_completion = late_session.turn_completion.lock().unwrap();
        settle_acp_turn_locked(&late_session, false, false, false, &late_subscribers);
        late_done_tx.send(()).unwrap();
    });
    let prompt_session = Arc::clone(&session);
    let prompt_subscribers = Arc::clone(&subscribers);
    let concurrent_prompt = thread::spawn(move || {
        prompt_started_tx.send(()).unwrap();
        prompt_done_tx
            .send(apply_acp_user_action(
                &prompt_session,
                smelt_core::acp_session::AcpUserAction::Prompt {
                    text: "prompt C".into(),
                    images: Vec::new(),
                    delivery_id: None,
                },
                &prompt_subscribers,
            ))
            .unwrap();
    });
    late_started_rx.recv().unwrap();
    prompt_started_rx.recv().unwrap();
    assert!(
        late_done_rx.try_recv().is_err(),
        "迟到更新必须等 watchdog 完整派发下一回合后再重算状态"
    );
    assert!(
        prompt_done_rx.try_recv().is_err(),
        "新 prompt 也必须等 watchdog 完整派发下一回合后才能取得 gate"
    );

    settle_acp_turn_locked(&session, false, false, false, &subscribers);
    drop(turn_completion);
    late_done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let _ = prompt_done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    late_update.join().unwrap();
    concurrent_prompt.join().unwrap();

    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } => {
            assert_eq!(text, "prompt B");
        }
        _ => panic!("expected the first queued prompt"),
    }
    assert!(
        cmd_rx.try_recv().is_err(),
        "迟到工具终态不能把 prompt C 与 B 并发下发"
    );
    assert_eq!(session.pending_prompts.lock().unwrap().len(), 1);
    assert!(session.prompt_in_flight.load(Ordering::SeqCst));
}

#[test]
fn cancelled_turn_releases_prompt_gate_without_a_timer() {
    let mut reduced = AcpSessionState::default();
    smelt_core::acp_session::note_prompt_sent(&mut reduced, "stop it".into(), Vec::new());
    smelt_core::acp_session::note_cancel_requested(&mut reduced);
    smelt_core::acp_session::apply_event(
        &mut reduced,
        smelt_core::acp_conn::ConversationEvent::TurnEnded(
            agent_client_protocol::schema::v1::StopReason::Cancelled,
        ),
    );
    let session = make_acp_session("acp-cancel-release", reduced);
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *session.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });
    session.prompt_in_flight.store(true, Ordering::SeqCst);
    session
        .pending_prompts
        .lock()
        .unwrap()
        .push_back(QueuedAcpPrompt {
            id: 1,
            text: "send now".to_string(),
            images: Vec::new(),
            delivery_id: None,
            origin: AcpPromptOrigin::User,
            echoed: false,
        });
    let subscribers = new_event_hub();

    settle_acp_turn_locked(&session, true, false, false, &subscribers);
    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } => {
            assert_eq!(text, "send now");
        }
        _ => panic!("expected queued prompt as soon as the cancelled turn is Idle"),
    }
    assert!(session.pending_prompts.lock().unwrap().is_empty());
}

#[test]
fn one_shot_action_rejects_invalid_prompt_shape() {
    let acp_sessions = new_test_acp_sessions();
    acp_sessions.reserve_with("acp-action", || {
        make_acp_session_value("acp-action", AcpSessionState::default())
    });
    let (server, client) = UnixStream::pair().unwrap();

    handle_acp_action(
        server,
        &serde_json::json!({
            "id": "acp-action",
            "action": {"Prompt": {"content": "wrong field", "images": []}},
        }),
        &acp_sessions,
        &new_event_hub(),
    );

    let mut response = String::new();
    BufReader::new(client).read_line(&mut response).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["ok"], false);
    let error = response["error"].as_str().unwrap();
    // 负载形状不对是真的坏消息，不能甲锅给版本差异——`Prompt` 本版是认识的。
    assert!(error.starts_with("invalid ACP action"), "实际 {error}");
    assert!(
        error.contains("Prompt"),
        "错误要指出是哪个动作，实际 {error}"
    );
    assert!(
        !error.contains("older than the client"),
        "字段写错不是版本落后，实际 {error}"
    );
}

/// 新客户端配旧守护进程：不认识的动作只让这一次失败，并且要能看出是版本差异。
#[test]
fn one_shot_action_from_a_newer_client_reports_a_version_gap() {
    let acp_sessions = new_test_acp_sessions();
    acp_sessions.reserve_with("acp-action", || {
        make_acp_session_value("acp-action", AcpSessionState::default())
    });
    let (server, client) = UnixStream::pair().unwrap();

    handle_acp_action(
        server,
        &serde_json::json!({
            "id": "acp-action",
            "action": "SomeFutureAction",
        }),
        &acp_sessions,
        &new_event_hub(),
    );

    let mut response = String::new();
    BufReader::new(client).read_line(&mut response).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["ok"], false);
    let error = response["error"].as_str().unwrap();
    assert!(error.contains("SomeFutureAction"), "实际 {error}");
    assert!(error.contains("older than the client"), "实际 {error}");
}

#[test]
fn skills_on_a_session_that_is_not_running_are_reported_as_unsupported() {
    let acp_sessions = new_test_acp_sessions();
    let (server, client) = UnixStream::pair().unwrap();

    handle_acp_skills(
        server,
        &serde_json::json!({"id": "acp-nope"}),
        &acp_sessions,
    );

    let mut response = String::new();
    BufReader::new(client).read_line(&mut response).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    // 不是错误：没在跑的会话就没有「加载了什么」可言，手机据此收起入口。
    assert_eq!(response["ok"], true);
    assert_eq!(response["supported"], false);
    assert_eq!(response["skills"].as_array().unwrap().len(), 0);
}

#[test]
fn skills_are_only_a_pi_concept() {
    let acp_sessions = new_test_acp_sessions();
    acp_sessions.reserve_with("acp-claude", || {
        let session = make_acp_session_value("acp-claude", AcpSessionState::default());
        *session.launch_spec.lock().unwrap() =
            Some(smelt_core::agent_kind::ConversationLaunchSpec::from_command("claude --acp"));
        session
    });
    let (server, client) = UnixStream::pair().unwrap();

    handle_acp_skills(
        server,
        &serde_json::json!({"id": "acp-claude"}),
        &acp_sessions,
    );

    let mut response = String::new();
    BufReader::new(client).read_line(&mut response).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["agent"], "claude");
    assert_eq!(response["supported"], false);
}

#[test]
fn pi_skills_come_from_the_launch_spec_plugin_args() {
    let temp = std::env::temp_dir().join(format!("smelt-daemon-skills-{}", uuid::Uuid::new_v4()));
    let skill_dir = temp.join("deploy");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: deploy\ndescription: 把构建推上去\n---\n",
    )
    .unwrap();

    let acp_sessions = new_test_acp_sessions();
    acp_sessions.reserve_with("acp-pi", || {
        let session = make_acp_session_value("acp-pi", AcpSessionState::default());
        *session.launch_spec.lock().unwrap() = Some(
            smelt_core::agent_kind::ConversationLaunchSpec::from_command("pi acp").with_env(
                smelt_core::agent_kind::SMELT_AGENT_PLUGIN_ARGS_ENV,
                serde_json::json!(["--no-skills", "--skill", skill_dir.to_str().unwrap()])
                    .to_string(),
            ),
        );
        session
    });
    let (server, client) = UnixStream::pair().unwrap();

    handle_acp_skills(server, &serde_json::json!({"id": "acp-pi"}), &acp_sessions);

    let mut response = String::new();
    BufReader::new(client).read_line(&mut response).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["supported"], true);
    let skills = response["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0]["name"], "deploy");
    assert_eq!(skills[0]["description"], "把构建推上去");

    std::fs::remove_dir_all(&temp).ok();
}

fn read_watch_unavailable(client: UnixStream) -> String {
    let mut line = String::new();
    BufReader::new(client).read_line(&mut line).unwrap();
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    v["unavailable"]["reason"].as_str().unwrap().to_string()
}

fn desktop_menu_with_acp(id: &str) -> WorkspaceMenuStore {
    let menu = new_test_workspace_menu();
    *menu.lock().unwrap() = WorkspaceMenuSnapshot::current(
        vec![],
        vec![smelt_core::workspace_menu::WorkspaceMenuSession {
            id: id.into(),
            kind: smelt_core::workspace_menu::WorkspaceMenuSessionKind::Acp,
            title: "agent chat".into(),
            custom_title: false,
            cwd: None,
            project_root: None,
            project_title: None,
            project_order: 0,
            session_order: 0,
            leaf_order: 0,
            agent: Some("pi".into()),
        }],
    );
    menu
}

fn subscribe_conversation_wake(subscribers: &EventHubHandle) -> smelt_event_bus::Subscription {
    subscribers
        .subscribe(
            PluginId::new("test.desktop").unwrap(),
            SubscriptionId::new("wake").unwrap(),
            BTreeSet::from([Topic::new(
                smelt_event_protocol::CORE_TOPIC_CONVERSATION_WAKE_REQUESTED,
            )
            .unwrap()]),
            &BTreeSet::from([smelt_event_protocol::Capability::new(
                smelt_event_protocol::CORE_CAPABILITY_CONVERSATION_WAKE,
            )
            .unwrap()]),
        )
        .unwrap()
}

#[test]
fn watch_on_unknown_session_reports_unavailable_instead_of_silently_closing() {
    let acp_sessions = new_test_acp_sessions();
    let (server, client) = UnixStream::pair().unwrap();
    let reader = BufReader::new(server.try_clone().unwrap());
    handle_acp_watch(
        server,
        reader,
        &serde_json::json!({"id": "acp-nope"}),
        acp_sessions,
        &new_test_workspace_menu(),
        &new_event_hub(),
    );
    // 静默 EOF 会让手机一直转圈：旁观方必须拿到一句明确的「没有」。
    assert!(!read_watch_unavailable(client).is_empty());
}

#[test]
fn watch_on_dormant_desktop_conversation_without_desktop_fails_fast() {
    let acp_sessions = new_test_acp_sessions();
    let (server, client) = UnixStream::pair().unwrap();
    let reader = BufReader::new(server.try_clone().unwrap());
    let started = Instant::now();
    handle_acp_watch(
        server,
        reader,
        &serde_json::json!({"id": "acp-dormant"}),
        acp_sessions,
        &desktop_menu_with_acp("acp-dormant"),
        &new_event_hub(),
    );
    assert!(read_watch_unavailable(client).contains("not open"));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "没有桌面端能兑现唤醒时不能空等超时"
    );
}

#[test]
fn watch_on_dormant_desktop_conversation_wakes_desktop_then_attaches() {
    let acp_sessions = new_test_acp_sessions();
    let subscribers = new_event_hub();
    let wake_requests = subscribe_conversation_wake(&subscribers);
    let menu = desktop_menu_with_acp("acp-dormant");

    let (server, client) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let reader = BufReader::new(server.try_clone().unwrap());
    let sessions_for_watch = Arc::clone(&acp_sessions);
    let watch = thread::spawn(move || {
        handle_acp_watch(
            server,
            reader,
            &serde_json::json!({"id": "acp-dormant"}),
            sessions_for_watch,
            &menu,
            &subscribers,
        );
    });

    // 扮演桌面端：收到唤醒请求才把会话拉起来（真实路径是 acp_open 占住 slot）。
    let deadline = Instant::now() + Duration::from_secs(5);
    let request = loop {
        match wake_requests.try_recv() {
            Ok(smelt_event_bus::Delivery::Event(event)) => break event,
            Ok(smelt_event_bus::Delivery::Snapshot(_)) => {}
            Ok(smelt_event_bus::Delivery::Lag { dropped, .. }) => {
                panic!("unexpected lag: {dropped}")
            }
            Err(_) => {
                assert!(Instant::now() < deadline, "watch 没有发出唤醒请求");
                thread::sleep(Duration::from_millis(10));
            }
        }
    };
    assert_eq!(request.envelope.payload["session_id"], "acp-dormant");
    let mut reduced = AcpSessionState::default();
    reduced
        .entries_mut()
        .push(AcpEntry::User("restored".into()));
    acp_sessions.reserve_with("acp-dormant", || {
        make_acp_session_value("acp-dormant", reduced)
    });

    let mut lines = BufReader::new(client.try_clone().unwrap());
    let mut line = String::new();
    lines.read_line(&mut line).unwrap();
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["snapshot"]["entries"].as_array().unwrap().len(), 1);

    drop(lines);
    drop(client);
    watch.join().unwrap();
}

#[test]
fn watch_delivers_initial_snapshot_matching_to_snapshot() {
    let mut reduced = AcpSessionState::default();
    reduced.entries_mut().push(AcpEntry::User("hi".into()));
    reduced.phase = DaemonPhase::Idle;
    let expected = reduced.to_snapshot(false);

    let acp_sessions = new_test_acp_sessions();
    acp_sessions.reserve_with("acp-1", || make_acp_session_value("acp-1", reduced));

    let (server, client) = UnixStream::pair().unwrap();
    let reader = BufReader::new(server.try_clone().unwrap());
    let h = thread::spawn(move || {
        handle_acp_watch(
            server,
            reader,
            &serde_json::json!({"id": "acp-1"}),
            acp_sessions,
            &new_test_workspace_menu(),
            &new_event_hub(),
        );
    });

    let mut br = BufReader::new(client.try_clone().unwrap());
    let mut line = String::new();
    br.read_line(&mut line).unwrap();
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["snapshot"]["entries"].as_array().unwrap().len(), 1);
    assert_eq!(
        serde_json::to_value(&expected).unwrap()["entries"],
        v["snapshot"]["entries"]
    );

    // 全部克隆（`br` 内部那份 + 这个原始 `client`）都要丢，socket 才会真正
    // 关闭产生 EOF——只 drop 一份，另一份还开着，对端读不到 EOF 会一直卡住
    // （这个坑踩过一次，见 watch_tests 里同款收尾写法）。
    drop(br);
    drop(client);
    h.join().unwrap();
}

#[test]
fn watch_registration_is_serialized_with_snapshot_push() {
    let mut reduced = AcpSessionState::default();
    reduced.entries_mut().push(AcpEntry::User("initial".into()));
    reduced.phase = DaemonPhase::Idle;

    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-watch-race", || {
        make_acp_session_value("acp-watch-race", reduced)
    });
    let mut reduced_guard = slot.value.reduced.lock().unwrap();

    let (server, client) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let reader = BufReader::new(server.try_clone().unwrap());
    let sessions_for_watch = Arc::clone(&acp_sessions);
    let watch = thread::spawn(move || {
        handle_acp_watch(
            server,
            reader,
            &serde_json::json!({"id": "acp-watch-race"}),
            sessions_for_watch,
            &new_test_workspace_menu(),
            &new_event_hub(),
        );
    });

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match slot.value.output_gate.try_lock() {
            Err(std::sync::TryLockError::WouldBlock) => break,
            Err(std::sync::TryLockError::Poisoned(error)) => {
                panic!("ACP output gate poisoned: {error}");
            }
            Ok(guard) => {
                drop(guard);
                assert!(
                    Instant::now() < deadline,
                    "watch 在读取初始快照前必须持有 output gate"
                );
                thread::yield_now();
            }
        }
    }

    reduced_guard.entries_mut().push(AcpEntry::Assistant {
        text: "arrived-during-attach".into(),
        thought: false,
    });
    let slot_for_push = Arc::clone(&slot);
    let push = thread::spawn(move || push_acp_snapshot_since(&slot_for_push.value, false, None));
    drop(reduced_guard);

    let mut lines = BufReader::new(client.try_clone().unwrap());
    let mut initial = String::new();
    let mut update = String::new();
    lines.read_line(&mut initial).unwrap();
    lines.read_line(&mut update).unwrap();
    assert!(!initial.is_empty(), "watch 必须先收到初始快照");
    assert!(!update.is_empty(), "attach 期间到达的推送不能永久丢失");

    push.join().unwrap();
    drop(lines);
    drop(client);
    watch.join().unwrap();
}

#[test]
fn bounded_acp_snapshot_reads_only_the_requested_older_page() {
    let mut reduced = AcpSessionState::default();
    for index in 0..5 {
        reduced
            .entries_mut()
            .push(AcpEntry::User(format!("message-{index}")));
    }
    let acp_sessions = new_test_acp_sessions();
    acp_sessions.reserve_with("acp-history", || {
        make_acp_session_value("acp-history", reduced)
    });

    let (server, client) = UnixStream::pair().unwrap();
    handle_acp_snapshot(
        server,
        &serde_json::json!({
            "id": "acp-history",
            "before": 3,
            "limit": 2,
        }),
        &acp_sessions,
        &new_event_hub(),
    );

    let mut line = String::new();
    BufReader::new(client).read_line(&mut line).unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["snapshot"]["entries_offset"], 1);
    assert_eq!(response["snapshot"]["entries_total"], 5);
    assert_eq!(response["snapshot"]["entries"].as_array().unwrap().len(), 2);
}

#[test]
fn missing_acp_snapshot_reports_that_the_session_is_gone() {
    let acp_sessions = new_test_acp_sessions();
    let (server, mut client) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    handle_acp_snapshot(
        server,
        &serde_json::json!({
            "id": "acp-does-not-exist",
            "before": 10,
            "limit": 100,
        }),
        &acp_sessions,
        &new_event_hub(),
    );

    let mut line = String::new();
    BufReader::new(&mut client)
        .read_line(&mut line)
        .expect("会话不存在时必须回一行错误，不能沉默");
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["error"], "ACP 会话不存在");
    assert!(response.get("snapshot").is_none());
}

fn reduced_with_debug_sidecars() -> AcpSessionState {
    let mut reduced = AcpSessionState::default();
    reduced.entries_mut().push(AcpEntry::User("hi".into()));
    reduced.tool_debug.insert(
        "tool-1".into(),
        smelt_core::acp_session::ToolCallDebug::default(),
    );
    reduced.tool_debug_generation = 1;
    reduced.runtime_debug = smelt_core::acp_session::RuntimeDebug {
        version: 3,
        source: "pi_runtime_debug".into(),
        model_calls: vec![smelt_core::acp_session::RuntimeDebugModelCall {
            sequence: 1,
            payload: serde_json::json!({"messages": [{"content": "huge"}]}),
            ..Default::default()
        }],
        ..Default::default()
    };
    reduced
}

#[test]
fn acp_snapshot_without_debug_omits_the_debug_sidecars() {
    let acp_sessions = new_test_acp_sessions();
    acp_sessions.reserve_with("acp-no-debug", || {
        make_acp_session_value("acp-no-debug", reduced_with_debug_sidecars())
    });
    let read = |request: serde_json::Value| {
        let (server, client) = UnixStream::pair().unwrap();
        handle_acp_snapshot(server, &request, &acp_sessions, &new_event_hub());
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        serde_json::from_str::<serde_json::Value>(&line).unwrap()
    };

    let full = read(serde_json::json!({"id": "acp-no-debug"}));
    // 父镜像里的 runtime debug 只是去掉请求体的账本，历史页带上它会把窗口里
    // 完整的调试体盖掉；完整调试体只随宿主推送帧走。工具参数表照旧默认带。
    assert!(full["snapshot"].get("runtime_debug").is_none());
    assert!(full["snapshot"].get("tool_debug").is_some());

    let lean = read(serde_json::json!({"id": "acp-no-debug", "include_debug": false}));
    assert!(lean["snapshot"].get("runtime_debug").is_none());
    assert!(lean["snapshot"].get("tool_debug").is_none());
    assert_eq!(lean["snapshot"]["entries"].as_array().unwrap().len(), 1);
}

#[test]
fn watch_without_debug_never_streams_the_debug_sidecars() {
    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-watch-lean", || {
        make_acp_session_value("acp-watch-lean", reduced_with_debug_sidecars())
    });

    let (server, client) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let reader = BufReader::new(server.try_clone().unwrap());
    let sessions_for_watch = Arc::clone(&acp_sessions);
    let watch = thread::spawn(move || {
        handle_acp_watch(
            server,
            reader,
            &serde_json::json!({"id": "acp-watch-lean", "include_debug": false}),
            sessions_for_watch,
            &new_test_workspace_menu(),
            &new_event_hub(),
        );
    });

    let mut lines = BufReader::new(client.try_clone().unwrap());
    let mut initial = String::new();
    lines.read_line(&mut initial).unwrap();
    let initial: serde_json::Value = serde_json::from_str(&initial).unwrap();
    assert_eq!(initial["snapshot"]["entries"].as_array().unwrap().len(), 1);
    assert!(initial["snapshot"].get("runtime_debug").is_none());
    assert!(initial["snapshot"].get("tool_debug").is_none());

    // 模型调用把 runtime debug 和工具参数都改了：桌面控制连接会收到整表，
    // 不要调试数据的旁观者仍然只能收到正文。
    slot.value.reduced.lock().unwrap().tool_debug_generation = 2;
    push_acp_snapshot_with_runtime_debug_for_test(&slot.value);
    let mut changed = String::new();
    lines.read_line(&mut changed).unwrap();
    let changed: serde_json::Value = serde_json::from_str(&changed).unwrap();
    assert!(changed["snapshot"].get("runtime_debug").is_none());
    assert!(changed["snapshot"].get("tool_debug").is_none());

    drop(lines);
    drop(client);
    watch.join().unwrap();
}

#[test]
fn push_snapshot_reaches_control_client_and_watchers_and_drops_dead_ones() {
    let sess = make_acp_session("acp-2", AcpSessionState::default());

    let (c_server, c_client) = UnixStream::pair().unwrap();
    let (w_server, w_client) = UnixStream::pair().unwrap();
    // 对端 close 的传播是异步的：紧接着 write 可能在关闭生效前成功（socket
    // 缓冲为空时数据直接进缓冲），client 就不会被摘掉。clone 一个探针同步等
    // EOF——对端读端全关后，探针 read 必返回 0，之后 c_server 的 write 才
    // 必然失败。
    let mut c_probe = c_server.try_clone().unwrap();
    {
        let mut out = sess.out.lock().unwrap();
        out.client = Some(test_acp_attachment(&sess, c_server, "acp-2-client"));
        out.watchers
            .push(test_acp_attachment(&sess, w_server, "acp-2-watcher"));
    }
    drop(c_client); // 控制连接对端已经断了：推送应该发现写失败并自己摘掉
    let mut eof = [0u8; 1];
    assert_eq!(
        c_probe.read(&mut eof).unwrap(),
        0,
        "control peer 应已关闭，探针应读到 EOF"
    );

    push_acp_snapshot(&sess, false);

    let deadline = Instant::now() + Duration::from_secs(1);
    while sess.out.lock().unwrap().client.is_some() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
    if sess.out.lock().unwrap().client.is_some() {
        // Unix socket 的 orderly EOF 允许首个反向 write 成功；下一次入队会
        // 观察到写线程已经关闭的 mailbox，并完成摘除。
        push_acp_snapshot(&sess, false);
        let retry_deadline = Instant::now() + Duration::from_secs(1);
        while sess.out.lock().unwrap().client.is_some() && Instant::now() < retry_deadline {
            thread::sleep(Duration::from_millis(1));
        }
    }
    assert!(
        sess.out.lock().unwrap().client.is_none(),
        "写线程发现断线后应摘掉 client"
    );
    assert_eq!(
        sess.out.lock().unwrap().watchers.len(),
        1,
        "还活着的 watcher 不该被牵连摘掉"
    );

    let mut line = String::new();
    BufReader::new(w_client).read_line(&mut line).unwrap();
    let mut mirror = nothing_sent();
    let v = decode_frame(&mut mirror, &line);
    assert!(v.get("snapshot").is_some());
}

#[test]
fn composer_restore_ack_action_clears_text_but_preserves_revision() {
    let mut reduced = AcpSessionState::default();
    reduced.composer_restore_revision = 7;
    reduced.composer_restore_texts = vec!["不要在重启后复活".into()];
    let session = make_acp_session("acp-composer-restore-ack", reduced);
    let subscribers = new_event_hub();

    apply_acp_user_action(
        &session,
        smelt_core::acp_session::AcpUserAction::AcknowledgeComposerRestore { revision: 7 },
        &subscribers,
    )
    .unwrap();
    {
        let reduced = session.reduced.lock().unwrap();
        assert_eq!(reduced.composer_restore_revision, 7);
        assert!(reduced.composer_restore_texts.is_empty());
    }

    session.reduced.lock().unwrap().composer_restore_texts = vec!["更新的一次恢复".into()];
    session.reduced.lock().unwrap().composer_restore_revision = 8;
    apply_acp_user_action(
        &session,
        smelt_core::acp_session::AcpUserAction::AcknowledgeComposerRestore { revision: 7 },
        &subscribers,
    )
    .unwrap();
    let reduced = session.reduced.lock().unwrap();
    assert_eq!(reduced.composer_restore_revision, 8);
    assert_eq!(reduced.composer_restore_texts, ["更新的一次恢复"]);
}

#[test]
fn runtime_debug_sidecar_is_sent_only_on_changed_frames() {
    let mut reduced = AcpSessionState::default();
    reduced.runtime_debug = smelt_core::acp_session::RuntimeDebug {
        version: 3,
        source: "pi_runtime_debug".into(),
        system_prompt: Some("exact prompt".into()),
        tools: Vec::new(),
        model_calls: vec![smelt_core::acp_session::RuntimeDebugModelCall {
            sequence: 1,
            turn: Some(1),
            compaction_sequence: None,
            captured_at_ms: 1_725_000_000_000,
            source: "pi_context_with_system".into(),
            model: smelt_core::acp_session::RuntimeDebugModel {
                provider: Some("openai".into()),
                id: Some("gpt-test".into()),
                ..Default::default()
            },
            request_config: smelt_core::acp_session::RuntimeDebugRequestConfig::default(),
            payload: serde_json::json!({
                "messages": [{"role": "user", "content": "hello"}]
            }),
            response: None,
            response_captured_at_ms: None,
            response_redacted_paths: None,
            redacted_paths: Vec::new(),
            ..smelt_core::acp_session::RuntimeDebugModelCall::default()
        }],
        ..smelt_core::acp_session::RuntimeDebug::default()
    };
    let sess = make_acp_session("acp-runtime-debug-wire", reduced);
    let (server, client) = UnixStream::pair().unwrap();
    sess.out.lock().unwrap().client =
        Some(test_acp_attachment(&sess, server, "acp-runtime-debug-wire"));
    let mut reader = BufReader::new(client);

    push_acp_snapshot_since(&sess, false, None);
    let mut incremental = String::new();
    reader.read_line(&mut incremental).unwrap();
    let mut mirror = nothing_sent();
    let incremental = decode_frame(&mut mirror, &incremental);
    assert!(
        incremental["snapshot"].get("runtime_debug").is_none(),
        "普通流式帧不应重复携带完整 provider payload"
    );

    push_acp_snapshot_with_runtime_debug_for_test(&sess);
    let mut changed = String::new();
    reader.read_line(&mut changed).unwrap();
    let changed = decode_frame(&mut mirror, &changed);
    assert_eq!(
        changed["snapshot"]["runtime_debug"]["modelCalls"][0]["payload"]["messages"][0]["content"],
        "hello"
    );
}

#[test]
fn parallel_tool_completion_replaces_snapshot_from_the_changed_card() {
    let mut reduced = AcpSessionState::default();
    for id in ["tool-a", "tool-b"] {
        reduced.entries_mut().push(AcpEntry::ToolCall {
            id: id.into(),
            title: id.into(),
            kind: ToolKind::Execute,
            status: ToolCallStatus::InProgress,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
        });
    }
    let sess = make_acp_session("acp-parallel", reduced);
    let (server, client) = UnixStream::pair().unwrap();
    sess.out.lock().unwrap().client = Some(test_acp_attachment(&sess, server, "acp-parallel"));

    let outcome = {
        let mut state = sess.reduced.lock().unwrap();
        smelt_core::acp_session::apply_event(
            &mut state,
            smelt_core::acp_conn::ConversationEvent::ToolFinished {
                id: "tool-a".into(),
                status: ToolCallStatus::Completed,
                output: Vec::new(),
            },
        )
    };
    push_acp_snapshot_since(&sess, false, outcome.entries_offset);

    let mut line = String::new();
    BufReader::new(client).read_line(&mut line).unwrap();
    let response = decode_frame(&mut nothing_sent(), &line);
    assert_eq!(response["snapshot"]["entries_offset"], 0);
    assert_eq!(response["snapshot"]["entries"].as_array().unwrap().len(), 2);
    assert_eq!(
        response["snapshot"]["entries"][0]["ToolCall"]["status"],
        "completed"
    );
    assert_eq!(
        response["snapshot"]["entries"][1]["ToolCall"]["status"],
        "in_progress"
    );
}

#[test]
fn window_link_carries_entries_written_as_the_turn_closes() {
    // 窗口连接只订阅当前一轮。回合失败在同一步里追加报错并收尾，报错已经不在
    // 当前一轮里；这一帧不带它，窗口会以为漏了更新，把整段对话换成空的。
    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-turn-failed", || {
        make_acp_session_value("acp-turn-failed", AcpSessionState::default())
    });
    {
        let mut reduced = slot.value.reduced.lock().unwrap();
        reduced.entries_mut().push(AcpEntry::User("A".into()));
        reduced.phase = DaemonPhase::Thinking;
        reduced.turn_started_at_ms = Some(1);
    }
    let (server, client) = UnixStream::pair().unwrap();
    slot.value.out.lock().unwrap().client = Some(
        acp_snapshot_link_for_slot(
            &slot,
            server,
            Vec::new(),
            nothing_sent(),
            0,
            "acp-turn-failed",
            "acp-client",
            SnapshotProjection {
                history: false,
                debug: true,
                image_refs: false,
            },
        )
        .unwrap(),
    );

    let outcome = smelt_core::acp_session::apply_event(
        &mut slot.value.reduced.lock().unwrap(),
        smelt_core::acp_conn::ConversationEvent::TurnFailed("499 status code (no body)".into()),
    );
    push_acp_snapshot_since(&slot.value, false, outcome.entries_offset);

    let mut line = String::new();
    BufReader::new(client).read_line(&mut line).unwrap();
    let frame = decode_frame(&mut nothing_sent(), &line);
    assert_eq!(frame["live"]["history_len"], 2);
    assert!(frame["live"]["open_turn"].as_array().unwrap().is_empty());
    assert_eq!(frame["snapshot"]["entries_offset"], 1);
    assert_eq!(frame["snapshot"]["entries_total"], 2);
    let entries = frame["snapshot"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "这一帧应只带收尾时写下的那条：{line}");
    assert!(
        entries[0]
            .to_string()
            .contains("回合失败：499 status code (no body)"),
        "收尾时写下的报错必须随这一帧送到窗口：{line}"
    );
}

#[test]
fn hosted_window_link_carries_entries_written_as_the_turn_closes() {
    // 主 daemon 的镜像只留最后一轮。收尾时写下的报错同样要从镜像送到窗口。
    let mut host = AcpSessionState::default();
    host.entries_mut().extend([
        AcpEntry::User("早先".into()),
        AcpEntry::Assistant {
            text: "好".into(),
            thought: false,
        },
        AcpEntry::User("A".into()),
    ]);
    host.phase = DaemonPhase::Thinking;
    host.turn_started_at_ms = Some(1);
    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-hosted-turn-failed", || {
        make_acp_session_value("acp-hosted-turn-failed", AcpSessionState::default())
    });
    slot.value
        .reduced
        .lock()
        .unwrap()
        .merge_hosted_snapshot(host.to_snapshot(false))
        .unwrap();
    let (server, client) = UnixStream::pair().unwrap();
    slot.value.out.lock().unwrap().client = Some(
        acp_snapshot_link_for_slot(
            &slot,
            server,
            Vec::new(),
            nothing_sent(),
            0,
            "acp-hosted-turn-failed",
            "acp-client",
            SnapshotProjection {
                history: false,
                debug: true,
                image_refs: false,
            },
        )
        .unwrap(),
    );

    let outcome = smelt_core::acp_session::apply_event(
        &mut host,
        smelt_core::acp_conn::ConversationEvent::TurnFailed("499 status code (no body)".into()),
    );
    let host_frame = host.to_snapshot_since(false, outcome.entries_offset.unwrap());
    let merged = slot
        .value
        .reduced
        .lock()
        .unwrap()
        .merge_hosted_snapshot(host_frame)
        .unwrap();
    assert_eq!(slot.value.reduced.lock().unwrap().entries_base(), 2);
    push_acp_snapshot_since(&slot.value, false, Some(merged.changed_from));

    let mut line = String::new();
    BufReader::new(client).read_line(&mut line).unwrap();
    let frame = decode_frame(&mut nothing_sent(), &line);
    assert_eq!(frame["snapshot"]["entries_offset"], 3);
    assert_eq!(frame["snapshot"]["entries_total"], 4);
    let entries = frame["snapshot"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "镜像这一帧应只带收尾时写下的那条：{line}");
    assert!(entries[0].to_string().contains("回合失败"), "{line}");
}

#[test]
fn unchanged_tool_debug_is_not_copied_into_later_snapshots() {
    let sess = make_acp_session("acp-tool-debug", AcpSessionState::default());
    let (server, client) = UnixStream::pair().unwrap();
    sess.out.lock().unwrap().client = Some(test_acp_attachment(&sess, server, "acp-tool-debug"));
    let mut reader = BufReader::new(client);

    push_acp_snapshot_since(&sess, false, None);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut mirror = nothing_sent();
    let first = decode_frame(&mut mirror, &line);
    assert!(
        first["snapshot"].get("tool_debug").is_none(),
        "没变过的工具参数表不该出现在增量帧里"
    );

    {
        let mut state = sess.reduced.lock().unwrap();
        smelt_core::acp_session::apply_event(
            &mut state,
            smelt_core::acp_conn::ConversationEvent::ToolDebug {
                id: "tool-1".into(),
                name: Some("bash".into()),
                raw_input: Some(serde_json::json!({"cmd": "ls"})),
            },
        );
    }
    push_acp_snapshot_since(&sess, false, Some(0));
    line.clear();
    reader.read_line(&mut line).unwrap();
    let changed = decode_frame(&mut mirror, &line);
    assert_eq!(
        changed["snapshot"]["tool_debug"]["tool-1"]["raw_input"]["cmd"],
        "ls"
    );

    push_acp_snapshot_since(&sess, false, None);
    line.clear();
    reader.read_line(&mut line).unwrap();
    let later = decode_frame(&mut mirror, &line);
    assert!(
        later["snapshot"].get("tool_debug").is_none(),
        "参数表没再变时，下一帧必须省略整表"
    );
}

#[test]
fn kill_removes_session_and_closes_connections() {
    let acp_sessions = new_test_acp_sessions();
    let remote_sessions = new_test_remote_sessions();
    let subscribers = new_event_hub();
    let (slot, _) = acp_sessions.reserve_with("acp-3", || {
        make_acp_session_value("acp-3", AcpSessionState::default())
    });

    let (c_server, c_client) = UnixStream::pair().unwrap();
    slot.value.out.lock().unwrap().client = Some(
        acp_snapshot_link_for_slot(
            &slot,
            c_server,
            Vec::new(),
            nothing_sent(),
            0,
            "acp-3",
            "acp-test",
            SnapshotProjection::FULL,
        )
        .unwrap(),
    );

    let (server, client) = UnixStream::pair().unwrap();
    handle_acp_kill(
        server,
        &serde_json::json!({"id": "acp-3"}),
        &acp_sessions,
        &remote_sessions,
        &subscribers,
    );

    let mut resp = String::new();
    BufReader::new(client).read_line(&mut resp).unwrap();
    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["ok"], true);
    assert!(acp_sessions.get("acp-3").is_none());
    assert_eq!(
        slot.value.connection_generation.load(Ordering::SeqCst),
        1,
        "kill 持锁后再 retire，一次 generation 推进就足以作废当前 drain"
    );

    // 控制连接要先收到「会话已被终结」的终态快照，再读到 EOF。
    let mut tail = String::new();
    BufReader::new(c_client).read_to_string(&mut tail).unwrap();
    let mut lines = tail.lines();
    let payload = decode_frame(&mut nothing_sent(), lines.next().expect("终态快照"));
    let final_snapshot: smelt_core::acp_session::ConversationSnapshot =
        serde_json::from_value(payload["snapshot"].clone()).unwrap();
    assert_eq!(
        final_snapshot.end_kind,
        smelt_core::acp_session::AcpEndKind::SessionTerminated,
        "删除必须跟传输断开区分开，否则客户端会重连并把会话复活"
    );
    assert_eq!(final_snapshot.phase, DaemonPhase::Dead);
    assert!(lines.next().is_none(), "终态快照之后就该 EOF");
}

/// kill 一个不存在的 id：跟终端 `kill` 一样静默回 ok，不报错。
#[test]
fn kill_unknown_session_is_a_harmless_no_op() {
    let acp_sessions = new_test_acp_sessions();
    let remote_sessions = new_test_remote_sessions();
    let subscribers = new_event_hub();
    let (server, client) = UnixStream::pair().unwrap();
    handle_acp_kill(
        server,
        &serde_json::json!({"id": "acp-ghost"}),
        &acp_sessions,
        &remote_sessions,
        &subscribers,
    );
    let mut resp = String::new();
    BufReader::new(client).read_line(&mut resp).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&resp).unwrap()["ok"],
        true
    );
}

/// 帮手：走一遍 `handle_acp_open` 的完整流程（跟真实客户端一样连接→读首行
/// 快照→断开），cmd 用一个必然不存在的路径——`spawn_acp_client` 保证不阻塞调用方，
/// 子进程起不来只会异步产出 `Fatal` 状态，不影响这里要测的「登记进表」这件事。
fn open_acp_session_once(
    id: &str,
    acp_sessions: &AcpSessions,
    event_hub: &EventHubHandle,
) -> Arc<acp_registry::AcpSlot<AcpSession>> {
    let (server, client) = UnixStream::pair().unwrap();
    let reader = BufReader::new(server.try_clone().unwrap());
    let acp_sessions2 = Arc::clone(acp_sessions);
    let subscribers2 = Arc::clone(event_hub);
    let id_owned = id.to_string();
    let h = thread::spawn(move || {
        handle_acp_open(
            server,
            reader,
            &serde_json::json!({
                "id": id_owned,
                "launch": {"command": "/definitely/not/a/real/binary-xyz"}
            }),
            new_sessions(),
            acp_sessions2,
            subscribers2,
        );
    });
    let mut br = BufReader::new(client.try_clone().unwrap());
    let mut line = String::new();
    br.read_line(&mut line).unwrap(); // 读到首行快照，说明 session open 已经完成登记并下发首帧
    drop(br);
    drop(client); // 两份 clone 都要丢，读循环那头才会真正见到 EOF 退出
    h.join().unwrap();
    acp_sessions
        .get(id)
        .expect("open 后 registry 中应保留该 slot")
}

#[test]
fn concurrent_open_same_id_keeps_one_registry_slot() {
    let acp_sessions: AcpSessions = Arc::new(acp_registry::AcpRegistry::new_same_process(
        Arc::new(RwLock::new(())),
    ));
    let subscribers = new_event_hub();
    let barrier = Arc::new(std::sync::Barrier::new(8));

    let slots = thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..8 {
            let acp_sessions = Arc::clone(&acp_sessions);
            let subscribers = Arc::clone(&subscribers);
            let barrier = Arc::clone(&barrier);
            workers.push(scope.spawn(move || {
                barrier.wait();
                open_acp_session_once("acp-race", &acp_sessions, &subscribers)
            }));
        }
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });

    assert_eq!(acp_sessions.snapshot().len(), 1);
    assert!(
        slots.windows(2).all(|pair| Arc::ptr_eq(&pair[0], &pair[1])),
        "并发 open 必须拿到同一个稳定 slot"
    );
}

#[test]
fn kill_does_not_unpublish_until_lifecycle_is_free() {
    let acp_sessions: AcpSessions = Arc::new(acp_registry::AcpRegistry::new_same_process(
        Arc::new(RwLock::new(())),
    ));
    let (old, _) = acp_sessions.reserve_with("acp-race", || {
        make_acp_session_value("acp-race", AcpSessionState::default())
    });
    let lifecycle = old.lifecycle.lock().unwrap();
    let registry_for_kill = Arc::clone(&acp_sessions);
    let remote_sessions = new_test_remote_sessions();
    let subscribers = new_event_hub();
    let (server, client) = UnixStream::pair().unwrap();
    let killer = thread::spawn(move || {
        handle_acp_kill(
            server,
            &serde_json::json!({"id": "acp-race"}),
            &registry_for_kill,
            &remote_sessions,
            &subscribers,
        );
    });

    thread::sleep(Duration::from_millis(50));
    assert!(
        acp_sessions.get("acp-race").is_some(),
        "kill 必须等 lifecycle；拆卸完成前 sid 仍在表里，不能先 detach 再 sleep 等"
    );

    drop(lifecycle);
    killer.join().unwrap();
    let mut response = String::new();
    BufReader::new(client)
        .read_line(&mut response)
        .expect("kill response");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&response).unwrap()["ok"],
        true
    );
    assert!(acp_sessions.get("acp-race").is_none());
}

#[test]
fn open_after_kill_creates_a_fresh_slot() {
    let acp_sessions = new_test_acp_sessions();
    let subscribers = new_event_hub();
    let remote_sessions = new_test_remote_sessions();
    let (old, _) = acp_sessions.reserve_with("acp-open-kill", || {
        make_acp_session_value("acp-open-kill", AcpSessionState::default())
    });

    let (kill_server, kill_client) = UnixStream::pair().unwrap();
    handle_acp_kill(
        kill_server,
        &serde_json::json!({"id": "acp-open-kill"}),
        &acp_sessions,
        &remote_sessions,
        &subscribers,
    );
    let mut kill_response = String::new();
    BufReader::new(kill_client)
        .read_line(&mut kill_response)
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&kill_response).unwrap()["ok"],
        true
    );
    assert!(acp_sessions.get("acp-open-kill").is_none());

    open_acp_session_once("acp-open-kill", &acp_sessions, &subscribers);
    let current = acp_sessions
        .get("acp-open-kill")
        .expect("kill 之后的 open 应登记新 slot");
    assert!(!Arc::ptr_eq(&current, &old));
}

/// 回归 code review 发现的高严重度 bug：创建新会话却从没插进
/// `acp_sessions` 表，导致 watch/list/kill 都找不到它，`handle_upgrade`
/// 收集 fd 时也会漏掉它——无缝升级直接把这条会话弄丢。
#[test]
fn open_new_session_registers_it_in_acp_sessions_table() {
    let acp_sessions = new_test_acp_sessions();
    let subscribers = new_event_hub();

    open_acp_session_once("acp-new", &acp_sessions, &subscribers);

    assert!(
        acp_sessions.get("acp-new").is_some(),
        "新建会话必须登记进 acp_sessions，不然 watch/list/kill 和无缝升级的 fd 收集都找不到它"
    );
}

/// 同一个 bug 的另一面：表里没有它，`handle_acp_open` 的「已存在就复用」
/// 分支永远命中不了 —— 同一个 id 重开一次就会再走一遍子进程创建，多起
/// 一个 agent 子进程，旧的那个泄漏在后台再也够不着。
#[test]
fn reopening_same_id_reuses_existing_session_instead_of_spawning_a_duplicate() {
    let acp_sessions = new_test_acp_sessions();
    let subscribers = new_event_hub();

    open_acp_session_once("acp-dup", &acp_sessions, &subscribers);
    let first = acp_sessions.get("acp-dup").expect("首次打开该已登记");

    open_acp_session_once("acp-dup", &acp_sessions, &subscribers);
    let second = acp_sessions.get("acp-dup").expect("重开该还在表里");

    assert_eq!(
        acp_sessions.snapshot().len(),
        1,
        "同一个 id 重开不该在表里多出一条"
    );
    assert!(
        Arc::ptr_eq(&first, &second),
        "重开应该复用已登记的会话，不能是 acp_spawn 又建了一个新对象（否则旧 agent 进程/线程直接泄漏）"
    );
}

#[test]
fn open_then_handoff_keeps_one_stable_registry_slot() {
    let acp_sessions = new_test_acp_sessions();
    let subscribers = new_event_hub();
    let (stdin_fd_owner, _stdin_peer) = UnixStream::pair().unwrap();
    let (stdout_fd_owner, _stdout_peer) = UnixStream::pair().unwrap();
    let (cmd_tx, _cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    let (slot, created) = acp_sessions.reserve_with("acp-handoff", || {
        let sess = make_acp_session_value("acp-handoff", AcpSessionState::default());
        *sess.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
            cmd_tx,
            event_rx,
            stdio: Arc::new(Mutex::new(Some(smelt_core::acp_conn::AcpStdio {
                pid: std::process::id() as i32,
                stdin_fd: stdin_fd_owner.as_raw_fd(),
                stdout_fd: stdout_fd_owner.as_raw_fd(),
            }))),
            in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            supports_mid_turn_input: false,
            supports_compaction: false,
            supports_native_queue: false,
            supports_rewind: false,
        });
        *sess.launch_spec.lock().unwrap() = Some(
            smelt_core::agent_kind::ConversationLaunchSpec::from_command(
                "/definitely/not/a/real/binary-xyz",
            ),
        );
        sess
    });
    assert!(created);

    let opened = open_acp_session_once("acp-handoff", &acp_sessions, &subscribers);
    assert!(Arc::ptr_eq(&opened, &slot));

    let (items, fds) = collect_acp_handoff_typed(&acp_sessions);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id(), "acp-handoff");
    // typed 枚举无 cmd 字段，"不发 legacy cmd"由类型系统保证。
    let crate::handoff_v2::manifest::AcpHandoff::Direct { launch, .. } = &items[0] else {
        panic!("应为 direct 形态");
    };
    assert_eq!(launch.command, "/definitely/not/a/real/binary-xyz");
    assert_eq!(
        fds,
        vec![
            (
                crate::handoff_v2::manifest::FdRole::AcpStdin {
                    session_id: "acp-handoff".to_string()
                },
                stdin_fd_owner.as_raw_fd(),
            ),
            (
                crate::handoff_v2::manifest::FdRole::AcpStdout {
                    session_id: "acp-handoff".to_string()
                },
                stdout_fd_owner.as_raw_fd(),
            ),
        ]
    );
    assert_eq!(acp_sessions.snapshot().len(), 1);
    assert!(Arc::ptr_eq(
        &acp_sessions.get("acp-handoff").unwrap(),
        &slot
    ));
}

#[test]
fn handoff_snapshot_keeps_the_open_turn_and_omits_debug_sidecars() {
    let acp_sessions = new_test_acp_sessions();
    let (stdin_fd_owner, _stdin_peer) = UnixStream::pair().unwrap();
    let (stdout_fd_owner, _stdout_peer) = UnixStream::pair().unwrap();
    let (cmd_tx, _cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    let mut reduced = AcpSessionState::default();
    // 已提交的正文留在 agent 自己的会话文件里，交接只带还没写完的这一轮。
    reduced
        .entries_mut()
        .push(AcpEntry::User("以前的话".into()));
    reduced
        .entries_mut()
        .push(AcpEntry::User("继续这条对话".into()));
    reduced.phase = smelt_core::daemon_state::DaemonPhase::Thinking;
    reduced.turn_started_at_ms = Some(1);
    reduced.tool_debug.insert(
        "call-1".into(),
        smelt_core::acp_session::ToolCallDebug {
            name: Some("read".into()),
            raw_input: Some(serde_json::json!({"path": "/tmp/secret"})),
        },
    );
    reduced.runtime_debug = smelt_core::acp_session::RuntimeDebug {
        version: 3,
        source: "pi_runtime_debug".into(),
        system_prompt: Some("完整系统提示".into()),
        model_calls: vec![smelt_core::acp_session::RuntimeDebugModelCall {
            sequence: 1,
            source: "pi_context_with_system".into(),
            payload: serde_json::json!({"messages": [{"role": "user", "content": "hello"}]}),
            ..smelt_core::acp_session::RuntimeDebugModelCall::default()
        }],
        ..smelt_core::acp_session::RuntimeDebug::default()
    };
    acp_sessions.reserve_with("acp-debug-handoff", || {
        let sess = make_acp_session_value("acp-debug-handoff", reduced);
        *sess.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
            cmd_tx,
            event_rx,
            stdio: Arc::new(Mutex::new(Some(smelt_core::acp_conn::AcpStdio {
                pid: std::process::id() as i32,
                stdin_fd: stdin_fd_owner.as_raw_fd(),
                stdout_fd: stdout_fd_owner.as_raw_fd(),
            }))),
            in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            supports_mid_turn_input: false,
            supports_compaction: false,
            supports_native_queue: false,
            supports_rewind: false,
        });
        sess
    });

    let (items, _) = collect_acp_handoff_typed(&acp_sessions);
    let crate::handoff_v2::manifest::AcpHandoff::Direct { snapshot, .. } = &items[0] else {
        panic!("应为 direct 形态");
    };
    assert!(
        matches!(&snapshot.entries[..], [AcpEntry::User(text)] if text == "继续这条对话"),
        "交接必须带走还没写完的这一轮，且不带已提交的正文"
    );
    assert!(snapshot.history_omitted);
    assert!(snapshot.runtime_debug.is_none());
    assert!(snapshot.tool_debug.is_none());
    let wire = serde_json::to_value(snapshot).unwrap();
    assert!(wire.get("runtime_debug").is_none());
    assert!(wire.get("tool_debug").is_none());

    let live = acp_sessions.get("acp-debug-handoff").unwrap();
    let live = live.value.reduced.lock().unwrap();
    assert_eq!(
        live.runtime_debug.system_prompt.as_deref(),
        Some("完整系统提示")
    );
    assert!(live.tool_debug.contains_key("call-1"));
}

#[test]
fn upgrade_barrier_requires_quiescent_phase_and_no_outstanding_rpc() {
    let acp_sessions = new_test_acp_sessions();

    let mut idle = AcpSessionState::default();
    idle.phase = DaemonPhase::Idle;
    let (idle_slot, _) =
        acp_sessions.reserve_with("acp-idle", || make_acp_session_value("acp-idle", idle));
    let (cmd_tx, _cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *idle_slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });
    assert!(acp_upgrade_blockers(&acp_sessions).is_empty());

    idle_slot.value.reduced.lock().unwrap().phase = DaemonPhase::Connecting;
    assert!(
        acp_upgrade_blockers(&acp_sessions).is_empty(),
        "握手中的会话不是活跃回合，不能把升级永久拦住"
    );

    {
        let mut reduced = idle_slot.value.reduced.lock().unwrap();
        reduced.phase = DaemonPhase::Thinking;
        reduced.turn_started_at_ms = Some(1);
    }
    assert_eq!(acp_upgrade_blockers(&acp_sessions), vec!["acp-idle"]);

    // 乱序/旧 handoff 可能留下 Running，但回合开始时间已经清空。这个相位
    // 不是活跃回合，升级不应被它永久拦住。
    idle_slot.value.reduced.lock().unwrap().turn_started_at_ms = None;
    assert!(acp_upgrade_blockers(&acp_sessions).is_empty());

    // 真正仍有未收尾工具时，即使相位字段不完整，也必须继续拦截。
    idle_slot
        .value
        .reduced
        .lock()
        .unwrap()
        .entries_mut()
        .push(AcpEntry::ToolCall {
            id: "active-tool".into(),
            title: "running".into(),
            kind: ToolKind::Execute,
            status: ToolCallStatus::InProgress,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
        });
    assert_eq!(acp_upgrade_blockers(&acp_sessions), vec!["acp-idle"]);

    // TurnEnded 已经落地但 adapter 没补工具终态时，交接恢复会统一收尾，
    // 不应让更新无限等待看门狗。
    {
        let mut reduced = idle_slot.value.reduced.lock().unwrap();
        reduced.phase = DaemonPhase::Idle;
        reduced.completed_unread = true;
        reduced.turn_started_at_ms = None;
    }
    assert!(acp_upgrade_blockers(&acp_sessions).is_empty());
    idle_slot.value.reduced.lock().unwrap().completed_unread = false;
    assert!(acp_upgrade_blockers(&acp_sessions).is_empty());

    idle_slot
        .value
        .prompt_in_flight
        .store(true, Ordering::SeqCst);
    assert_eq!(acp_upgrade_blockers(&acp_sessions), vec!["acp-idle"]);
    idle_slot
        .value
        .prompt_in_flight
        .store(false, Ordering::SeqCst);

    // 有 outstanding RPC 仍然是硬门槛。
    idle_slot.value.reduced.lock().unwrap().completed_unread = false;

    idle_slot.value.reduced.lock().unwrap().phase = DaemonPhase::Idle;
    idle_slot
        .value
        .handle
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .in_flight_rpc
        .store(1, Ordering::SeqCst);
    assert_eq!(acp_upgrade_blockers(&acp_sessions), vec!["acp-idle"]);
}

#[test]
fn hosted_runtime_blocks_upgrade_until_the_active_turn_is_idle() {
    let acp_sessions = new_test_acp_sessions();
    let mut running = AcpSessionState::default();
    running.phase = DaemonPhase::AwaitingApproval;
    running.turn_started_at_ms = Some(1);
    let (slot, _) = acp_sessions.reserve_with("acp-hosted", || {
        make_acp_session_value("acp-hosted", running)
    });
    slot.value.prompt_in_flight.store(true, Ordering::SeqCst);
    let (hosted, peer) = acp_runtime_host::HostedConversationHandle::test_stub();
    *slot.value.hosted_handle.lock().unwrap() = Some(hosted);

    assert_eq!(
        acp_upgrade_blockers(&acp_sessions),
        vec!["acp-hosted"],
        "session host 能恢复回合不等于应主动打断回合；升级必须等到 idle"
    );

    {
        let mut reduced = slot.value.reduced.lock().unwrap();
        reduced.phase = DaemonPhase::Idle;
        reduced.turn_started_at_ms = None;
    }
    slot.value.prompt_in_flight.store(false, Ordering::SeqCst);
    assert!(acp_upgrade_blockers(&acp_sessions).is_empty());
    drop(peer);
}

/// unreaped 重试只 prove、不重杀：活着的陌生组 → false + pid 原样留下，
/// 调用方不得 spawn。旧语义 ECHILD 直接 true（杀都不杀就放行 spawn），
/// 收养路径下等于没证明；更不能重杀——该号可能已被复用。
#[test]
fn unreaped_retry_proves_without_killing_strangers() {
    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-unreaped", || {
        make_acp_session_value("acp-unreaped", AcpSessionState::default())
    });
    // 自己的进程组当“活陌生组”：非亲生、活着、杀不得。
    let foreign_live = unsafe { libc::getpgrp() };
    *slot.value.unreaped_pid.lock().unwrap() = Some(foreign_live);
    assert!(
        !retire_acp_runtime(&slot.value),
        "活陌生组必须判失败、保住 unreaped"
    );
    assert_eq!(*slot.value.unreaped_pid.lock().unwrap(), Some(foreign_live));
    assert_eq!(unsafe { libc::kill(std::process::id() as i32, 0) }, 0);
}

#[test]
fn upgrade_returns_busy_before_touching_handoff_for_active_acp_turn() {
    let sessions = new_sessions();
    let acp_sessions = new_test_acp_sessions();
    let mut running = AcpSessionState::default();
    running.phase = DaemonPhase::Thinking;
    running.turn_started_at_ms = Some(1);
    acp_sessions.reserve_with("acp-running", || {
        make_acp_session_value("acp-running", running)
    });

    let (server, client) = UnixStream::pair().unwrap();
    handle_upgrade(
        server,
        &serde_json::json!({"op": "upgrade"}),
        &sessions,
        &acp_sessions,
        -1,
    );

    let mut line = String::new();
    BufReader::new(client).read_line(&mut line).unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["busy"], true);
    assert_eq!(response["sessions"], serde_json::json!(["acp-running"]));
}

#[test]
fn daemon_phase_distinguishes_executing_tool_from_thinking() {
    let mut running_with_tool = AcpSessionState::default();
    running_with_tool.phase = DaemonPhase::Thinking;
    running_with_tool.entries_mut().push(AcpEntry::ToolCall {
        id: "t1".into(),
        title: "Read".into(),
        kind: ToolKind::Read,
        status: ToolCallStatus::InProgress,
        output: Vec::new(),
        children: Vec::new(),
        subagents: Vec::new(),
    });
    assert_eq!(
        compute_acp_daemon_phase(&running_with_tool),
        Phase::ExecutingTool
    );

    let mut running_no_tool = AcpSessionState::default();
    running_no_tool.phase = DaemonPhase::Thinking;
    assert_eq!(compute_acp_daemon_phase(&running_no_tool), Phase::Thinking);

    let mut idle_with_tool = AcpSessionState::default();
    idle_with_tool.phase = DaemonPhase::Idle;
    idle_with_tool.entries_mut().push(AcpEntry::ToolCall {
        id: "late-tool".into(),
        title: "navigate".into(),
        kind: ToolKind::Fetch,
        status: ToolCallStatus::Pending,
        output: Vec::new(),
        children: Vec::new(),
        subagents: Vec::new(),
    });
    assert_eq!(
        compute_acp_daemon_phase(&idle_with_tool),
        Phase::Idle,
        "回合已结束后，未收尾工具不能把守护相位重新打开成执行工具"
    );

    let connecting = AcpSessionState::default();
    assert_eq!(connecting.phase, DaemonPhase::Connecting);
    assert_eq!(compute_acp_daemon_phase(&connecting), Phase::Connecting);
}

#[test]
fn daemon_phase_keeps_working_while_background_work_outlives_the_turn() {
    use smelt_core::acp_session::{AcpTurnOutcome, BackgroundTaskStatus, BackgroundTaskView};

    let mut state = AcpSessionState::default();
    state.phase = DaemonPhase::Idle;
    state.turn_outcome = Some(AcpTurnOutcome::Succeeded);
    state.background_tasks = vec![BackgroundTaskView {
        id: "bg-1".into(),
        title: "测试".into(),
        command: "npm test".into(),
        status: BackgroundTaskStatus::Running,
        exit_code: None,
        output: String::new(),
        started_at_ms: 10,
        finished_at_ms: None,
    }];
    assert_eq!(
        compute_acp_daemon_phase(&state),
        Phase::ExecutingTool,
        "回合说完了但后台还在跑，对外不能是空闲或完成"
    );

    state.background_tasks[0].status = BackgroundTaskStatus::Stopped;
    state.completed_unread = true;
    assert_eq!(compute_acp_daemon_phase(&state), Phase::Succeeded);
}

#[test]
fn daemon_phase_projects_acp_outcomes_and_failure_reason() {
    use smelt_core::acp_session::AcpTurnOutcome;

    let mut completed = AcpSessionState::default();
    completed.phase = DaemonPhase::Idle;
    completed.completed_unread = true;
    assert_eq!(
        compute_acp_daemon_phase(&completed),
        Phase::Idle,
        "没有回合结果不是成功"
    );
    completed.turn_outcome = Some(AcpTurnOutcome::Succeeded);

    completed.entries_mut().push(AcpEntry::ToolCall {
        id: "late-tool".into(),
        title: "navigate".into(),
        kind: ToolKind::Fetch,
        status: ToolCallStatus::InProgress,
        output: Vec::new(),
        children: Vec::new(),
        subagents: Vec::new(),
    });
    assert_eq!(
        compute_acp_daemon_phase(&completed),
        Phase::Succeeded,
        "完成结果必须优先于迟到的工具明细"
    );

    completed.turn_outcome = Some(AcpTurnOutcome::Cancelled);
    assert_eq!(compute_acp_daemon_phase(&completed), Phase::Idle);
    completed.turn_outcome = Some(AcpTurnOutcome::MaxTokens);
    assert_eq!(compute_acp_daemon_phase(&completed), Phase::Failed);
    assert_eq!(
        acp_pending_question(&completed).as_deref(),
        Some("已达到本轮最大令牌数")
    );

    let mut ended = AcpSessionState::default();
    ended.phase = DaemonPhase::Dead;
    ended.end_reason = "连接意外中断".into();
    ended
        .permissions
        .push(smelt_core::acp_session::LivePermission {
            question: "旧审批问题".into(),
            tool_call_id: "tool".into(),
            options: Vec::new(),
            details: smelt_core::acp_session::ApprovalDetailsView::Generic,
            responder: None,
            raw_request_line: None,
        });
    assert_eq!(compute_acp_daemon_phase(&ended), Phase::Failed);
    assert_eq!(
        acp_pending_question(&ended).as_deref(),
        Some("连接意外中断")
    );
}

#[test]
fn pending_question_prefers_permission_over_elicitation() {
    use smelt_core::acp_session::LivePermission;

    let mut s = AcpSessionState::default();
    assert_eq!(acp_pending_question(&s), None);

    s.permissions.push(LivePermission {
        question: "要不要覆盖这个文件？".into(),
        tool_call_id: "t1".into(),
        options: Vec::new(),
        details: smelt_core::acp_session::ApprovalDetailsView::Generic,
        responder: None,
        raw_request_line: None,
    });
    assert_eq!(
        acp_pending_question(&s).as_deref(),
        Some("要不要覆盖这个文件？")
    );
}

#[test]
fn acp_open_request_prefers_structured_launch() {
    let req = parse_acp_open_request(&serde_json::json!({
        "id": "acp-1",
        "cwd": "/repo",
        "launch": {
            "command": "claude --print",
            "env": {
                "CLAUDE_CONFIG_DIR": "~/Claude Workspaces/quant"
            }
        },
        "agent": "claude",
        "ephemeral_env": {
            "PLUGIN_TOKEN": "transient"
        },
        "resume_id": "resume-1"
    }))
    .unwrap();

    assert_eq!(req.id, "acp-1");
    assert_eq!(req.launch.command, "claude --print");
    assert_eq!(
        req.launch.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
        Some("~/Claude Workspaces/quant")
    );
    assert_eq!(
        req.ephemeral_env.get("PLUGIN_TOKEN").map(String::as_str),
        Some("transient")
    );
    assert_eq!(req.resume_id.as_deref(), Some("resume-1"));
}

#[test]
fn acp_open_request_rejects_legacy_cmd() {
    assert!(
        parse_acp_open_request(&serde_json::json!({
            "id": "acp-legacy",
            "cmd": "claude --dangerously-skip-permissions",
            "agent": "claude"
        }))
        .is_none()
    );
}

#[test]
fn acp_open_invalid_request_still_returns_ended_snapshot() {
    let (server, client) = UnixStream::pair().unwrap();
    let reader = BufReader::new(server.try_clone().unwrap());
    let acp_sessions: AcpSessions = Arc::new(acp_registry::AcpRegistry::new_same_process(
        Arc::new(RwLock::new(())),
    ));
    let subscribers = new_event_hub();
    let worker = thread::spawn(move || {
        handle_acp_open(
            server,
            reader,
            &serde_json::json!({ "op": "acp_open" }),
            new_sessions(),
            acp_sessions,
            subscribers,
        );
    });
    let mut line = String::new();
    BufReader::new(client).read_line(&mut line).unwrap();
    worker.join().unwrap();
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    let snapshot: smelt_core::acp_session::ConversationSnapshot =
        serde_json::from_value(value["snapshot"].clone()).unwrap();
    assert_eq!(snapshot.phase, DaemonPhase::Dead);
    assert_eq!(
        snapshot.end_kind,
        smelt_core::acp_session::AcpEndKind::ProviderFailed
    );
    assert!(!snapshot.end_reason.is_empty());
}

#[test]
fn acp_open_rejects_malformed_conversation_state_instead_of_erasing_it() {
    let malformed_binding = parse_acp_open_request(&serde_json::json!({
        "id": "acp-malformed-binding",
        "launch": {"command": "codex"},
        "conversation_binding": {"type": "automation"}
    }));
    assert!(malformed_binding.is_none());
}

#[test]
fn daemon_relaunch_upgrades_released_adapter_defaults_only() {
    let mut old = smelt_core::agent_kind::ConversationLaunchSpec::from_command(
        "bunx --bun @agentclientprotocol/codex-acp@1.1.7",
    );
    assert!(upgrade_released_adapter_launch(&mut old));
    assert_eq!(
        old.command,
        "bunx --bun @agentclientprotocol/codex-acp@1.12.0"
    );

    let custom = "NO_BROWSER=1 bunx --bun @agentclientprotocol/codex-acp@1.1.7";
    let mut customized = smelt_core::agent_kind::ConversationLaunchSpec::from_command(custom);
    assert!(!upgrade_released_adapter_launch(&mut customized));
    assert_eq!(customized.command, custom);
}

#[test]
fn blank_resume_id_is_treated_as_a_new_session() {
    let req = parse_acp_open_request(&serde_json::json!({
        "id": "acp-blank-resume",
        "launch": {"command": "codex"},
        "resume_id": "   "
    }))
    .unwrap();

    assert!(req.resume_id.is_none());
    assert_eq!(
        select_resume_id(
            Some(" ".into()),
            smelt_store::StoredHistoryIdentity::NoRow,
            None
        ),
        None
    );
}

#[test]
fn blank_acp_session_does_not_reuse_runtime_id_as_history() {
    let mut reduced = smelt_core::acp_session::AcpSessionState::default();
    reduced.acp_session_id = Some("runtime-only".into());

    assert_eq!(known_acp_resume_id(&reduced), None);
}

#[test]
fn acp_session_with_entries_can_fallback_to_runtime_history_id() {
    let mut reduced = smelt_core::acp_session::AcpSessionState::default();
    reduced.acp_session_id = Some("runtime-history".into());
    reduced
        .entries_mut()
        .push(smelt_core::acp_chat::AcpEntry::User("hello".into()));

    assert_eq!(
        known_acp_resume_id(&reduced).as_deref(),
        Some("runtime-history")
    );
}

#[test]
fn live_acp_ownership_includes_canonical_and_runtime_aliases() {
    let mut reduced = smelt_core::acp_session::AcpSessionState::default();
    reduced.history_session_id = Some("history-1".into());
    reduced.acp_session_id = Some("runtime-1".into());
    assert_eq!(
        live_acp_owner_ids(&reduced),
        vec!["history-1".to_string(), "runtime-1".to_string()]
    );

    reduced.acp_session_id = Some("history-1".into());
    assert_eq!(live_acp_owner_ids(&reduced), vec!["history-1".to_string()]);
}

#[test]
fn live_memory_then_column_then_unbound_row_selects_resume_id() {
    use smelt_store::StoredHistoryIdentity::{Bound, NoRow, Unbound};

    assert_eq!(
        select_resume_id(
            Some("live".into()),
            Bound("column".into()),
            Some("gui".into()),
        ),
        Some("live".into()),
        "正在跟 agent 说话的进程记得身份"
    );
    assert_eq!(
        select_resume_id(None, Bound("column".into()), Some("gui-stale".into())),
        Some("column".into()),
        "进程已经忘了时，库里确认过的 id 胜过窗口缓存"
    );
    assert_eq!(
        select_resume_id(None, Unbound, Some("gui-stale".into())),
        None,
        "行在、列是空，窗口缓存不能填回来"
    );
    assert_eq!(
        select_resume_id(None, NoRow, Some("chosen".into())),
        Some("chosen".into()),
        "还没有这一行时，历史页继续带上的 id 可以开会话"
    );
    assert_eq!(select_resume_id(None, NoRow, None), None);
    assert_eq!(
        select_resume_id(
            Some("  ".into()),
            Bound("column".into()),
            Some("gui".into()),
        ),
        Some("column".into())
    );
    assert_eq!(
        resolve_resume_id(
            "acp-fork",
            Some("requested".into()),
            Some("known".into()),
            false
        )
        .as_deref(),
        Some("known"),
        "分叉不读这个槽位的库"
    );
    assert_eq!(
        resolve_resume_id("acp-fork", Some("requested".into()), None, false).as_deref(),
        Some("requested")
    );
}

#[test]
fn durable_history_write_keeps_a_confirmed_id_until_it_is_abandoned() {
    assert_eq!(
        durable_history_write(Some("pi-1".into()), false),
        DurableHistoryWrite::Remember("pi-1".into())
    );
    assert_eq!(
        durable_history_write(Some("pi-next".into()), true),
        DurableHistoryWrite::Remember("pi-next".into()),
        "新确认的身份胜出，同一帧上的放弃标志不能把它清掉"
    );
    assert_eq!(
        durable_history_write(None, false),
        DurableHistoryWrite::Leave
    );
    assert_eq!(
        durable_history_write(Some("  ".into()), false),
        DurableHistoryWrite::Leave,
        "重新开始先拿掉内存身份，这一帧不能把库里的旧 id 清掉"
    );
    assert_eq!(
        durable_history_write(None, true),
        DurableHistoryWrite::Clear
    );
    assert_eq!(
        durable_history_write(Some("  ".into()), true),
        DurableHistoryWrite::Clear
    );
}

/// 直连会话回退动作的完整门控矩阵：能力位、相位、目标类型都要在 daemon
/// 侧验证，GUI 传来的下标只当参考，不当事实源。
#[test]
fn rewind_action_gates_on_capability_phase_and_target() {
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Idle;
    reduced.entries_mut().push(AcpEntry::User("first".into()));
    reduced.entries_mut().push(AcpEntry::Assistant {
        text: "answer".into(),
        thought: false,
    });
    reduced.entries_mut().push(AcpEntry::User("same".into()));
    reduced.entries_mut().push(AcpEntry::User("same".into()));
    let session = make_acp_session("acp-rewind-gates", reduced);
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *session.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });

    // 1) 驱动不支持（ACP / 旧句柄）直接拒绝。
    let rejected = apply_acp_user_action(
        &session,
        smelt_core::acp_session::AcpUserAction::RewindToMessage { entry_index: 0 },
        &new_event_hub(),
    );
    assert_eq!(rejected.unwrap_err(), "agent does not support rewind");
    assert!(cmd_rx.try_recv().is_err());

    // 2) 打开能力位，目标不是用户消息（下标 1 是 assistant）。
    session
        .handle
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .supports_rewind = true;
    let rejected = apply_acp_user_action(
        &session,
        smelt_core::acp_session::AcpUserAction::RewindToMessage { entry_index: 1 },
        &new_event_hub(),
    );
    assert_eq!(rejected.unwrap_err(), "rewind target is not a user message");

    // 3) 下标越界同样按“不是用户消息”拒绝。
    let rejected = apply_acp_user_action(
        &session,
        smelt_core::acp_session::AcpUserAction::RewindToMessage { entry_index: 42 },
        &new_event_hub(),
    );
    assert_eq!(rejected.unwrap_err(), "rewind target is not a user message");

    // 4) 相位不是 Idle：回退会丢弃未决回合，正在思考时必须拒绝。
    session.reduced.lock().unwrap().phase = DaemonPhase::Thinking;
    let rejected = apply_acp_user_action(
        &session,
        smelt_core::acp_session::AcpUserAction::RewindToMessage { entry_index: 0 },
        &new_event_hub(),
    );
    assert_eq!(rejected.unwrap_err(), "rewind requires an idle session");
    session.reduced.lock().unwrap().phase = DaemonPhase::Idle;

    // 5) 历史重放中：草稿还在重建，GUI 的下标是按已发布正文算的，对不上草稿。
    //    相位虽是 Idle，也必须等重放收尾，不能拿草稿去校验或截断。
    smelt_core::acp_session::apply_event(
        &mut session.reduced.lock().unwrap(),
        smelt_core::acp_conn::ConversationEvent::HistoryReplayStarted,
    );
    let rejected = apply_acp_user_action(
        &session,
        smelt_core::acp_session::AcpUserAction::RewindToMessage { entry_index: 3 },
        &new_event_hub(),
    );
    assert_eq!(
        rejected.unwrap_err(),
        "rewind requires history replay to finish"
    );
    assert!(cmd_rx.try_recv().is_err());
    session.reduced.lock().unwrap().abandon_replay_scratch();

    // 6) 编辑重发只允许最后一条真实用户消息；旧消息应走独立的分叉对话功能。
    let rejected = apply_acp_user_action(
        &session,
        smelt_core::acp_session::AcpUserAction::RewindToMessage { entry_index: 0 },
        &new_event_hub(),
    );
    assert_eq!(
        rejected.unwrap_err(),
        "rewind target is not the latest user message"
    );
    assert!(cmd_rx.try_recv().is_err());

    // 7) 合法目标：第 2 条 "same"（下标 3），occurrence 必须算出 1。
    apply_acp_user_action(
        &session,
        smelt_core::acp_session::AcpUserAction::RewindToMessage { entry_index: 3 },
        &new_event_hub(),
    )
    .unwrap();
    match cmd_rx.try_recv().unwrap() {
        smelt_core::acp_conn::ConversationCommand::Rewind {
            text,
            occurrence,
            truncate_from,
        } => {
            assert_eq!(text, "same");
            assert_eq!(occurrence, 1);
            assert_eq!(truncate_from, 3);
        }
        _ => panic!("expected rewind command"),
    }
}

/// automation 绑定的会话禁止回退：Run 的输入序列归 daemon 所有。
#[test]
fn automation_binding_rejects_rewind() {
    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-rewind-automation", || {
        make_acp_session_value("acp-rewind-automation", AcpSessionState::default())
    });
    *slot.value.conversation_binding.lock().unwrap() =
        Some(smelt_core::conversation::ConversationBinding::Automation {
            run_id: "run-owned".into(),
        });
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: true,
    });

    let (server, client) = UnixStream::pair().unwrap();
    handle_acp_action(
        server,
        &serde_json::json!({
            "id": "acp-rewind-automation",
            "action": {"RewindToMessage": {"entry_index": 0}},
        }),
        &acp_sessions,
        &new_event_hub(),
    );
    let mut response = String::new();
    BufReader::new(client).read_line(&mut response).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"], "automation Run action is daemon-owned");
    assert!(cmd_rx.try_recv().is_err());
}

/// 超过 64 KiB 的 base64：带 `image_refs` 的连接上只发引用。
fn big_image(seed: char) -> smelt_core::acp_chat::AcpImage {
    smelt_core::acp_chat::AcpImage::new(
        "image/png",
        seed.to_string()
            .repeat(smelt_core::acp_chat::IMAGE_INLINE_MAX_B64 + 1),
    )
}

fn tool_with_image(id: &str, status: ToolCallStatus, image: char) -> AcpEntry {
    let mut tool = AcpEntry::tool_call(
        id,
        "截图",
        ToolKind::Read,
        status,
        vec![
            smelt_core::acp_chat::ToolOutputPart::Text("Read image file".into()),
            smelt_core::acp_chat::ToolOutputPart::Image(big_image(image)),
        ],
    );
    if let AcpEntry::ToolCall { children, .. } = &mut tool {
        children.push(AcpEntry::UserWithImages {
            text: "子代理的图".into(),
            images: vec![big_image(image)],
        });
    }
    tool
}

/// 这条连接该收到的条目：要图片引用时是替换后的样子。
fn projected_entries(entries: &[AcpEntry], image_refs: bool) -> Vec<serde_json::Value> {
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let entry = if image_refs {
                smelt_core::acp_chat::entry_with_image_refs(entry, index as u64)
            } else {
                std::borrow::Cow::Borrowed(entry)
            };
            serde_json::to_value(&*entry).unwrap()
        })
        .collect()
}

/// 读一帧，按客户端的做法把它合进手里那份条目。返回这一帧的原始字节数、是不是增量帧、
/// 还原出的帧。
fn read_into_view(
    lines: &mut BufReader<UnixStream>,
    mirror: &mut smelt_core::acp_wire::FrameMirror,
    view: &mut Vec<serde_json::Value>,
) -> (usize, bool, serde_json::Value) {
    let mut line = String::new();
    lines.read_line(&mut line).unwrap();
    let is_delta = serde_json::from_str::<serde_json::Value>(&line)
        .unwrap()
        .get("delta")
        .is_some();
    let frame = decode_frame(mirror, &line);
    let offset = frame["snapshot"]["entries_offset"].as_u64().unwrap() as usize;
    view.truncate(offset);
    view.extend(
        frame["snapshot"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .cloned(),
    );
    (line.len(), is_delta, frame)
}

fn watch_frames_follow_the_image_projection(image_refs: bool) {
    let id = if image_refs {
        "acp-watch-image-refs"
    } else {
        "acp-watch-inline-images"
    };
    let mut reduced = AcpSessionState::default();
    reduced.entries_mut().extend([
        AcpEntry::UserWithImages {
            text: "看这张".into(),
            images: vec![
                big_image('A'),
                smelt_core::acp_chat::AcpImage::new("image/png", "iVBORw0KGgo="),
            ],
        },
        tool_with_image("t1", ToolCallStatus::InProgress, 'B'),
    ]);
    reduced.composer_restore_images = vec![big_image('Z')];
    reduced.phase = DaemonPhase::Thinking;
    reduced.turn_started_at_ms = Some(1);
    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with(id, || make_acp_session_value(id, reduced));

    let (server, client) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let reader = BufReader::new(server.try_clone().unwrap());
    let sessions_for_watch = Arc::clone(&acp_sessions);
    let request = serde_json::json!({"id": id, "include_debug": false, "image_refs": image_refs});
    let watch = thread::spawn(move || {
        handle_acp_watch(
            server,
            reader,
            &request,
            sessions_for_watch,
            &new_test_workspace_menu(),
            &new_event_hub(),
        );
    });
    let mut lines = BufReader::new(client.try_clone().unwrap());
    let mut mirror = smelt_core::acp_wire::FrameMirror::default();
    let mut view = Vec::new();
    let current = || slot.value.reduced.lock().unwrap().entries().to_vec();
    let big = smelt_core::acp_chat::IMAGE_INLINE_MAX_B64;

    // 首帧（整段帧）。
    let (bytes, is_delta, frame) = read_into_view(&mut lines, &mut mirror, &mut view);
    assert!(!is_delta);
    assert_eq!(view, projected_entries(&current(), image_refs));
    let restore = frame["snapshot"]["composer_restore_images"]
        .as_array()
        .unwrap();
    if image_refs {
        assert!(bytes < big, "首帧不能带大图：{bytes} 字节");
        assert!(restore.is_empty(), "输入框还原图片不发给要引用的连接");
        let original = &view[1]["ToolCall"]["output"][1]["Image"]["original"];
        assert_eq!(original["entry"], 1);
    } else {
        assert!(bytes > big * 4, "桌面照旧内联：{bytes} 字节");
        assert_eq!(restore.len(), 1);
    }

    // 增量帧：工具收尾（含大图的条目改了），再追加一条带新大图的工具和一段回复。
    {
        let mut reduced = slot.value.reduced.lock().unwrap();
        let entries = reduced.entries_mut();
        entries[1] = tool_with_image("t1", ToolCallStatus::Completed, 'B');
        entries.push(tool_with_image("t2", ToolCallStatus::Completed, 'C'));
        entries.push(AcpEntry::Assistant {
            text: "看完了".into(),
            thought: false,
        });
    }
    push_acp_snapshot_since(&slot.value, false, Some(1));
    let (bytes, is_delta, _) = read_into_view(&mut lines, &mut mirror, &mut view);
    assert!(is_delta);
    assert_eq!(view, projected_entries(&current(), image_refs));
    if image_refs {
        assert!(bytes < big, "增量帧不能带大图：{bytes} 字节");
        assert_eq!(
            view[2]["ToolCall"]["output"][1]["Image"]["original"]["entry"],
            2
        );
    }

    // 回复继续长：增量只带新文字，镜像里替换过的条目原样保留。
    {
        let mut reduced = slot.value.reduced.lock().unwrap();
        reduced.entries_mut()[3] = AcpEntry::Assistant {
            text: "看完了，图里是登录页".into(),
            thought: false,
        };
    }
    push_acp_snapshot_since(&slot.value, false, Some(0));
    let (bytes, is_delta, _) = read_into_view(&mut lines, &mut mirror, &mut view);
    assert!(is_delta);
    assert_eq!(view, projected_entries(&current(), image_refs));
    assert!(bytes < 4096, "只该发新增的文字：{bytes} 字节");

    // Refresh 之后的整段帧和其后的增量帧也按同一投影。
    apply_acp_user_action(
        &slot.value,
        smelt_core::acp_session::AcpUserAction::Refresh,
        &new_event_hub(),
    )
    .unwrap();
    let (bytes, is_delta, _) = read_into_view(&mut lines, &mut mirror, &mut view);
    assert!(!is_delta);
    assert_eq!(view, projected_entries(&current(), image_refs));
    if image_refs {
        assert!(bytes < big, "整段帧不能带大图：{bytes} 字节");
    }
    {
        let mut reduced = slot.value.reduced.lock().unwrap();
        reduced.entries_mut()[2] = tool_with_image("t2", ToolCallStatus::Failed, 'C');
    }
    push_acp_snapshot_since(&slot.value, false, Some(2));
    let (_, is_delta, _) = read_into_view(&mut lines, &mut mirror, &mut view);
    assert!(is_delta);
    assert_eq!(view, projected_entries(&current(), image_refs));

    drop(lines);
    drop(client);
    watch.join().unwrap();
}

#[test]
fn image_refs_watch_sends_references_in_full_and_delta_frames() {
    watch_frames_follow_the_image_projection(true);
}

#[test]
fn watch_without_image_refs_still_inlines_images() {
    watch_frames_follow_the_image_projection(false);
}

#[test]
fn conversation_image_finds_the_original_by_reference() {
    use crate::conversation_image::tests::{dimensions, png_b64};
    use smelt_core::acp_chat::{AcpImage, image_digest};
    use smelt_core::control_api::ConversationImageParams;
    use smelt_runtime_client::protocol::control::ControlErrorCode;

    // 64 KiB 到 1 MiB 之间：原图原样给。
    let medium = png_b64(300, 200, u8::MAX);
    // 子代理里嵌的截图：缩略图按长边等比缩。
    let tall = png_b64(600, 1300, u8::MAX);
    let mut reduced = AcpSessionState::default();
    reduced.entries_mut().extend([
        AcpEntry::User("看图".into()),
        AcpEntry::UserWithImages {
            text: "附图".into(),
            images: vec![AcpImage::new("image/png", medium.clone())],
        },
        {
            let mut tool = AcpEntry::tool_call(
                "t1",
                "子代理",
                ToolKind::Other,
                ToolCallStatus::Completed,
                Vec::new(),
            );
            if let AcpEntry::ToolCall { children, .. } = &mut tool {
                children.push(AcpEntry::tool_call(
                    "t2",
                    "截图",
                    ToolKind::Read,
                    ToolCallStatus::Completed,
                    vec![smelt_core::acp_chat::ToolOutputPart::Image(AcpImage::new(
                        "image/png",
                        tall.clone(),
                    ))],
                ));
            }
            tool
        },
    ]);
    reduced.composer_restore_images = vec![AcpImage::new("image/png", medium.clone())];
    let acp_sessions = new_test_acp_sessions();
    acp_sessions.reserve_with("acp-image", || make_acp_session_value("acp-image", reduced));
    let fetch = |entry: u64, digest: &str, max_edge: Option<u32>| {
        crate::conversation_image::conversation_image(
            ConversationImageParams {
                session_id: "acp-image".into(),
                entry,
                digest: digest.into(),
                max_edge,
            },
            &acp_sessions,
            &new_event_hub(),
        )
    };

    let original = fetch(1, &image_digest(&medium), None).unwrap();
    assert_eq!(original.mime, "image/png");
    assert_eq!(original.data_b64, medium);

    let thumb = fetch(2, &image_digest(&tall), Some(100)).unwrap();
    assert_eq!(thumb.mime, "image/jpeg");
    assert_eq!(dimensions(&thumb.data_b64), (46, 100));
    // 太小的边夹到 32。
    let tiny = fetch(2, &image_digest(&tall), Some(1)).unwrap();
    assert_eq!(dimensions(&tiny.data_b64), (15, 32));

    // 摘要对不上（条目被改写过）、下标不对、超出末尾、会话不存在。
    for (entry, digest) in [
        (2, image_digest("别的图")),
        (1, image_digest(&tall)),
        (3, image_digest(&tall)),
        (u64::MAX, image_digest(&tall)),
    ] {
        let error = fetch(entry, &digest, None).unwrap_err();
        assert_eq!(error.code, ControlErrorCode::NotFound, "entry={entry}");
    }
    let error = crate::conversation_image::conversation_image(
        ConversationImageParams {
            session_id: "acp-nope".into(),
            entry: 1,
            digest: image_digest(&medium),
            max_edge: None,
        },
        &acp_sessions,
        &new_event_hub(),
    )
    .unwrap_err();
    assert_eq!(error.code, ControlErrorCode::NotFound);

    // 一次性翻页同样只给引用，引用能直接拿来取图。
    let (server, client) = UnixStream::pair().unwrap();
    handle_acp_snapshot(
        server,
        &serde_json::json!({"id": "acp-image", "image_refs": true}),
        &acp_sessions,
        &new_event_hub(),
    );
    let mut line = String::new();
    BufReader::new(client).read_line(&mut line).unwrap();
    let page: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert!(
        page["snapshot"]["composer_restore_images"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let nested = &page["snapshot"]["entries"][2]["ToolCall"]["children"][0]["ToolCall"]["output"]
        [0]["Image"];
    assert_eq!(nested["data_b64"], "");
    let reference = &nested["original"];
    assert_eq!(reference["entry"], 2);
    let again = fetch(
        reference["entry"].as_u64().unwrap(),
        reference["digest"].as_str().unwrap(),
        Some(100),
    )
    .unwrap();
    assert_eq!(again, thumb);
}
