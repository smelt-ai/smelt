use super::*;
use smelt_core::acp_chat::{AcpEntry, ToolCallStatus, ToolKind};
use smelt_core::acp_session::AcpSessionState;
use smelt_core::daemon_state::DaemonPhase;

fn make_acp_session(id: &str, reduced: AcpSessionState) -> Arc<AcpSession> {
    Arc::new(make_acp_session_value(id, reduced))
}

fn test_acp_attachment(stream: UnixStream, id: &str) -> OutputAttachment {
    OutputAttachment::new(stream, Vec::new(), id, "acp-test").unwrap()
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
fn reopening_live_session_returns_requested_tail_without_relaunch() {
    let mut reduced = AcpSessionState::default();
    for index in 0..5 {
        reduced
            .entries
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
        Some(smelt_core::conversation::ConversationBinding::Plugin {
            plugin_id: smelt_plugin_api::PluginId::new("com.example.current").unwrap(),
            route: smelt_plugin_api::PluginInputRouteBinding {
                contribution_id: smelt_plugin_api::ContributionId::new("current-input").unwrap(),
                context: serde_json::json!({"thread_id": "current-thread"}),
            },
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
    assert_eq!(response["snapshot"]["entries_offset"], 3);
    assert_eq!(response["snapshot"]["entries_total"], 5);
    assert_eq!(response["snapshot"]["entries"].as_array().unwrap().len(), 2);
    assert_eq!(
        response["snapshot"]["conversation_state"]["binding"]["plugin_id"],
        "com.example.current"
    );
    assert!(slot.value.handle.lock().unwrap().is_some());
    assert!(matches!(
        &*slot.value.conversation_binding.lock().unwrap(),
        Some(smelt_core::conversation::ConversationBinding::Plugin { plugin_id, route })
            if plugin_id.as_str() == "com.example.current"
                && route.context["thread_id"] == "current-thread"
    ));

    drop(reader);
    drop(client);
    worker.join().unwrap();
}

#[test]
fn reopening_live_session_adopts_client_plugin_binding_when_unknown() {
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
                    "type": "plugin",
                    "plugin_id": "com.example.restored",
                    "route": {
                        "contribution_id": "issue-comment-input",
                        "context": {"issue_id": "issue-1"}
                    }
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
        response["snapshot"]["conversation_state"]["binding"]["plugin_id"],
        "com.example.restored"
    );
    assert!(matches!(
        &*slot.value.conversation_binding.lock().unwrap(),
        Some(smelt_core::conversation::ConversationBinding::Plugin { plugin_id, route })
            if plugin_id.as_str() == "com.example.restored"
                && route.context["issue_id"] == "issue-1"
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
    slot.value.out.lock().unwrap().client = Some(test_acp_attachment(control_server, "acp-action"));

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
        slot.value.reduced.lock().unwrap().entries.last(),
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
fn submitted_plugin_input_never_falls_back_to_local_acp() {
    let session = make_acp_session("acp-plugin-route", AcpSessionState::default());
    *session.conversation_binding.lock().unwrap() =
        Some(smelt_core::conversation::ConversationBinding::Plugin {
            plugin_id: smelt_plugin_api::PluginId::new("com.example.chat").unwrap(),
            route: smelt_plugin_api::PluginInputRouteBinding {
                contribution_id: smelt_plugin_api::ContributionId::new("conversation-input")
                    .unwrap(),
                context: serde_json::json!({"thread_id": "thread-1"}),
            },
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

    let result = dispatch_conversation_input_with(
        &session,
        smelt_core::conversation::ConversationInput {
            submission_id: "submission-1".into(),
            text: "remote only".to_string(),
            images: Vec::new(),
        },
        &new_event_hub(),
        |_plugin_id, _route, _input, _agent_preset, _agent_session| {
            Err(smelt_core::conversation::ConversationSubmitError::rejected(
                "remote rejected",
            ))
        },
    );

    assert_eq!(result.unwrap_err().message, "remote rejected");
    assert!(cmd_rx.try_recv().is_err(), "失败时不能偷偷投递给本地 ACP");
    assert!(session.reduced.lock().unwrap().entries.is_empty());
}

#[test]
fn automation_run_rejects_freeform_conversation_input() {
    let session = make_acp_session("acp-automation", AcpSessionState::default());
    *session.conversation_binding.lock().unwrap() =
        Some(smelt_core::conversation::ConversationBinding::Automation {
            run_id: "run-1".into(),
        });

    let result = dispatch_conversation_input_with(
        &session,
        smelt_core::conversation::ConversationInput::new("second prompt".into(), Vec::new()),
        &new_event_hub(),
        |_plugin_id, _route, _input, _agent_preset, _agent_session| {
            panic!("automation input must not invoke a plugin")
        },
    );

    assert_eq!(
        result.unwrap_err().message,
        "automation Run input is daemon-owned"
    );
    assert!(session.reduced.lock().unwrap().entries.is_empty());
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
fn plugin_input_keeps_images_even_when_the_local_agent_does_not_support_them() {
    let mut reduced = AcpSessionState::default();
    reduced.supports_image = false;
    let session = make_acp_session("acp-plugin-images", reduced);
    *session.conversation_binding.lock().unwrap() =
        Some(smelt_core::conversation::ConversationBinding::Plugin {
            plugin_id: smelt_plugin_api::PluginId::new("com.example.chat").unwrap(),
            route: smelt_plugin_api::PluginInputRouteBinding {
                contribution_id: smelt_plugin_api::ContributionId::new("conversation-input")
                    .unwrap(),
                context: serde_json::json!({"thread_id": "thread-1"}),
            },
        });
    let image = smelt_core::acp_chat::AcpImage {
        mime: "image/png".into(),
        data_b64: "aW1hZ2U=".into(),
    };

    let route = dispatch_conversation_input_with(
        &session,
        smelt_core::conversation::ConversationInput {
            submission_id: "submission-2".into(),
            text: "看图".into(),
            images: vec![image.clone()],
        },
        &new_event_hub(),
        |_plugin_id, _route, input, _agent_preset, _agent_session| {
            assert_eq!(input.images, vec![image]);
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(
        route,
        smelt_core::conversation::ConversationInputRoute::Plugin
    );
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

    dispatch_conversation_input_with(
        &session,
        smelt_core::conversation::ConversationInput {
            submission_id: "submission-3".into(),
            text: "看图".into(),
            images: vec![smelt_core::acp_chat::AcpImage {
                mime: "image/png".into(),
                data_b64: "aW1hZ2U=".into(),
            }],
        },
        &new_event_hub(),
        |_plugin_id, _route, _input, _agent_preset, _agent_session| {
            panic!("direct input must not invoke a plugin")
        },
    )
    .unwrap();

    assert!(matches!(
        cmd_rx.try_recv().unwrap(),
        smelt_core::acp_conn::ConversationCommand::Prompt { text, images }
            if text.contains("当前智能体不支持图片输入") && images.is_empty()
    ));
}

#[test]
fn agent_preset_is_consumed_only_after_the_first_input_is_routed() {
    let session = make_acp_session("acp-agent-preset", AcpSessionState::default());
    *session.conversation_binding.lock().unwrap() =
        Some(smelt_core::conversation::ConversationBinding::Plugin {
            plugin_id: smelt_plugin_api::PluginId::new("com.example.chat").unwrap(),
            route: smelt_plugin_api::PluginInputRouteBinding {
                contribution_id: smelt_plugin_api::ContributionId::new("conversation-input")
                    .unwrap(),
                context: serde_json::json!({"thread_id": "thread-1"}),
            },
        });
    *session.pending_agent_preset.lock().unwrap() = Some("你是严谨的代码审查者".into());

    let rejected = dispatch_conversation_input_with(
        &session,
        smelt_core::conversation::ConversationInput {
            submission_id: "submission-4".into(),
            text: "检查这次改动".into(),
            images: Vec::new(),
        },
        &new_event_hub(),
        |_plugin_id, _route, input, agent_preset, _agent_session| {
            assert_eq!(input.text, "检查这次改动");
            assert_eq!(agent_preset, Some("你是严谨的代码审查者"));
            Err(smelt_core::conversation::ConversationSubmitError::rejected(
                "remote rejected",
            ))
        },
    );
    assert_eq!(rejected.unwrap_err().message, "remote rejected");
    assert_eq!(
        session.pending_agent_preset.lock().unwrap().as_deref(),
        Some("你是严谨的代码审查者"),
        "路由失败不能消费预设"
    );

    let routed = dispatch_conversation_input_with(
        &session,
        smelt_core::conversation::ConversationInput {
            submission_id: "submission-5".into(),
            text: "检查这次改动".into(),
            images: Vec::new(),
        },
        &new_event_hub(),
        |_plugin_id, _route, input, agent_preset, _agent_session| {
            assert_eq!(input.text, "检查这次改动");
            assert_eq!(agent_preset, Some("你是严谨的代码审查者"));
            Ok(())
        },
    );
    assert_eq!(
        routed.unwrap(),
        smelt_core::conversation::ConversationInputRoute::Plugin
    );
    assert!(session.pending_agent_preset.lock().unwrap().is_none());
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

    dispatch_conversation_input_with(
        &session,
        smelt_core::conversation::ConversationInput::new("分析组合风险".into(), Vec::new()),
        &new_event_hub(),
        |_plugin_id, _route, _input, _agent_preset, _agent_session| {
            panic!("direct input must not invoke a plugin")
        },
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
fn submitted_direct_input_delivers_to_acp_without_invoking_a_plugin() {
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

    let route = dispatch_conversation_input_with(
        &session,
        smelt_core::conversation::ConversationInput {
            submission_id: "submission-6".into(),
            text: "local".to_string(),
            images: Vec::new(),
        },
        &new_event_hub(),
        |_plugin_id, _route, _input, _agent_preset, _agent_session| {
            panic!("direct input must not invoke a plugin")
        },
    )
    .unwrap();

    assert_eq!(
        route,
        smelt_core::conversation::ConversationInputRoute::Direct
    );
    assert!(matches!(
        cmd_rx.try_recv().unwrap(),
        smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } if text == "local"
    ));
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
        slot.value.reduced.lock().unwrap().entries.last(),
        Some(AcpEntry::User(text)) if text == "after current turn"
    ));
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
            .entries
            .iter()
            .any(|entry| matches!(entry, AcpEntry::User(text) if text == "换个方向")),
        "待插入不得进会话"
    );
}

#[test]
fn watchdog_and_late_tool_update_serialize_prompt_dispatch() {
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Idle;
    reduced.completed_unread = true;
    reduced.entries.push(AcpEntry::ToolCall {
        id: "shell".into(),
        title: "Reading shell output".into(),
        kind: ToolKind::Execute,
        status: ToolCallStatus::InProgress,
        output: Vec::new(),
        children: Vec::new(),
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
            text: "prompt B".to_string(),
            images: Vec::new(),
            delivery_id: None,
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
        settle_acp_turn_locked(&late_session, false, false, &late_subscribers);
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

    settle_acp_turn_locked(&session, false, false, &subscribers);
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
            text: "send now".to_string(),
            images: Vec::new(),
            delivery_id: None,
        });
    let subscribers = new_event_hub();

    settle_acp_turn_locked(&session, true, false, &subscribers);
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
    assert_eq!(response["error"], "invalid ACP action");
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

#[test]
fn watch_on_unknown_session_just_disconnects() {
    let acp_sessions = new_test_acp_sessions();
    let (server, client) = UnixStream::pair().unwrap();
    let reader = BufReader::new(server.try_clone().unwrap());
    handle_acp_watch(
        server,
        reader,
        &serde_json::json!({"id": "acp-nope"}),
        acp_sessions,
    );
    // 没有会话可接：函数直接 return，客户端读到 EOF（不是某行 JSON）。
    let mut buf = Vec::new();
    BufReader::new(client).read_to_end(&mut buf).unwrap();
    assert!(buf.is_empty());
}

#[test]
fn watch_delivers_initial_snapshot_matching_to_snapshot() {
    let mut reduced = AcpSessionState::default();
    reduced.entries.push(AcpEntry::User("hi".into()));
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
    reduced.entries.push(AcpEntry::User("initial".into()));
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

    reduced_guard.entries.push(AcpEntry::Assistant {
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
            .entries
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
    );

    let mut line = String::new();
    BufReader::new(client).read_line(&mut line).unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["snapshot"]["entries_offset"], 1);
    assert_eq!(response["snapshot"]["entries_total"], 5);
    assert_eq!(response["snapshot"]["entries"].as_array().unwrap().len(), 2);
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
        out.client = Some(test_acp_attachment(c_server, "acp-2-client"));
        out.watchers
            .push(test_acp_attachment(w_server, "acp-2-watcher"));
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
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert!(v.get("snapshot").is_some());
}

#[test]
fn runtime_debug_sidecar_is_sent_only_on_changed_frames() {
    let mut reduced = AcpSessionState::default();
    reduced.runtime_debug = smelt_core::acp_session::RuntimeDebug {
        version: 2,
        source: "pi_runtime_debug".into(),
        system_prompt: Some("exact prompt".into()),
        tools: Vec::new(),
        model_call: Some(smelt_core::acp_session::RuntimeDebugModelCall {
            sequence: 1,
            source: "pi_before_provider_request".into(),
            model: smelt_core::acp_session::RuntimeDebugModel {
                provider: Some("openai".into()),
                id: Some("gpt-test".into()),
                ..Default::default()
            },
            payload: serde_json::json!({
                "messages": [{"role": "user", "content": "hello"}]
            }),
            redacted_paths: Vec::new(),
        }),
    };
    let sess = make_acp_session("acp-runtime-debug-wire", reduced);
    let (server, client) = UnixStream::pair().unwrap();
    sess.out.lock().unwrap().client = Some(test_acp_attachment(server, "acp-runtime-debug-wire"));
    let mut reader = BufReader::new(client);

    push_acp_snapshot_since(&sess, false, None);
    let mut incremental = String::new();
    reader.read_line(&mut incremental).unwrap();
    let incremental: serde_json::Value = serde_json::from_str(&incremental).unwrap();
    assert!(
        incremental["snapshot"].get("runtime_debug").is_none(),
        "普通流式帧不应重复携带完整 provider payload"
    );

    push_acp_snapshot_with_runtime_debug_for_test(&sess);
    let mut changed = String::new();
    reader.read_line(&mut changed).unwrap();
    let changed: serde_json::Value = serde_json::from_str(&changed).unwrap();
    assert_eq!(
        changed["snapshot"]["runtime_debug"]["modelCall"]["payload"]["messages"][0]["content"],
        "hello"
    );
}

#[test]
fn parallel_tool_completion_replaces_snapshot_from_the_changed_card() {
    let mut reduced = AcpSessionState::default();
    for id in ["tool-a", "tool-b"] {
        reduced.entries.push(AcpEntry::ToolCall {
            id: id.into(),
            title: id.into(),
            kind: ToolKind::Execute,
            status: ToolCallStatus::InProgress,
            output: Vec::new(),
            children: Vec::new(),
        });
    }
    let sess = make_acp_session("acp-parallel", reduced);
    let (server, client) = UnixStream::pair().unwrap();
    sess.out.lock().unwrap().client = Some(test_acp_attachment(server, "acp-parallel"));

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
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
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
fn kill_removes_session_and_closes_connections() {
    let acp_sessions = new_test_acp_sessions();
    let remote_sessions = new_test_remote_sessions();
    let subscribers = new_event_hub();
    let (slot, _) = acp_sessions.reserve_with("acp-3", || {
        make_acp_session_value("acp-3", AcpSessionState::default())
    });

    let (c_server, c_client) = UnixStream::pair().unwrap();
    slot.value.out.lock().unwrap().client = Some(test_acp_attachment(c_server, "acp-3"));

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
    let payload: serde_json::Value = serde_json::from_str(lines.next().expect("终态快照")).unwrap();
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
        .entries
        .push(AcpEntry::ToolCall {
            id: "active-tool".into(),
            title: "running".into(),
            kind: ToolKind::Execute,
            status: ToolCallStatus::InProgress,
            output: Vec::new(),
            children: Vec::new(),
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
        &new_event_hub(),
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
    running_with_tool.entries.push(AcpEntry::ToolCall {
        id: "t1".into(),
        title: "Read".into(),
        kind: ToolKind::Read,
        status: ToolCallStatus::InProgress,
        output: Vec::new(),
        children: Vec::new(),
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
    idle_with_tool.entries.push(AcpEntry::ToolCall {
        id: "late-tool".into(),
        title: "navigate".into(),
        kind: ToolKind::Fetch,
        status: ToolCallStatus::Pending,
        output: Vec::new(),
        children: Vec::new(),
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
fn daemon_phase_projects_acp_outcomes_and_failure_reason() {
    use smelt_core::acp_session::AcpTurnOutcome;

    let mut completed = AcpSessionState::default();
    completed.phase = DaemonPhase::Idle;
    completed.completed_unread = true;
    assert_eq!(compute_acp_daemon_phase(&completed), Phase::Succeeded);

    completed.entries.push(AcpEntry::ToolCall {
        id: "late-tool".into(),
        title: "navigate".into(),
        kind: ToolKind::Fetch,
        status: ToolCallStatus::InProgress,
        output: Vec::new(),
        children: Vec::new(),
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
fn acp_open_request_keeps_agent_session_identity_separate_from_execution_provider() {
    let req = parse_acp_open_request(&serde_json::json!({
        "id": "acp-agent-session",
        "launch": {"command": "codex"},
        "agent": "codex",
        "agent_session": {
            "agent": {
                "plugin_id": "com.example.quant",
                "contribution_id": "quant-agent"
            },
            "controller": {
                "plugin_id": "com.example.quant",
                "contribution_id": "quant-session"
            },
            "instance": {
                "plugin_id": "com.example.quant",
                "resource_type": "strategy-run",
                "resource_id": "run-1"
            }
        }
    }))
    .unwrap();

    assert_eq!(req.launch.command, "codex");
    let binding = req.agent_session.unwrap();
    assert_eq!(binding.agent.contribution_id.as_str(), "quant-agent");
    assert_eq!(binding.controller.contribution_id.as_str(), "quant-session");
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
        "conversation_binding": {"type": "plugin"}
    }));
    assert!(malformed_binding.is_none());

    let malformed_agent = parse_acp_open_request(&serde_json::json!({
        "id": "acp-malformed-agent",
        "launch": {"command": "codex"},
        "agent_session": {
            "agent": {"plugin_id": "com.example.quant"}
        }
    }));
    assert!(malformed_agent.is_none());
}

#[test]
fn agent_session_without_plugin_binding_never_falls_through_to_local_acp() {
    let session = make_acp_session("acp-agent-without-route", AcpSessionState::default());
    *session.agent_session.lock().unwrap() = Some(
        serde_json::from_value(serde_json::json!({
            "agent": {
                "plugin_id": "com.example.quant",
                "contribution_id": "quant-agent"
            },
            "controller": {
                "plugin_id": "com.example.quant",
                "contribution_id": "quant-session"
            },
            "instance": {
                "plugin_id": "com.example.quant",
                "resource_type": "strategy-run",
                "resource_id": "run-1"
            }
        }))
        .unwrap(),
    );
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

    let result = dispatch_conversation_input_with(
        &session,
        smelt_core::conversation::ConversationInput::new("do not send locally".into(), Vec::new()),
        &new_event_hub(),
        |_plugin_id, _route, _input, _agent_preset, _agent_session| {
            panic!("missing binding must not invent a plugin route")
        },
    );

    assert!(result.is_err());
    assert!(cmd_rx.try_recv().is_err());
}

#[test]
fn restored_agent_session_must_match_the_declared_contribution_graph() {
    use smelt_plugin_api::{
        Contribution, ContributionId, InvocationOperation, PluginContributionSet, PluginId,
    };

    let plugin_id = PluginId::new("com.example.quant").unwrap();
    let input_id = ContributionId::new("quant-input").unwrap();
    let controller_id = ContributionId::new("quant-session").unwrap();
    let agent_id = ContributionId::new("quant-agent").unwrap();
    let set = PluginContributionSet {
        plugin_id: plugin_id.clone(),
        name: "Quant".into(),
        version: "1.0.0".into(),
        contributions: vec![
            Contribution::InputRoute {
                id: input_id.clone(),
                operation: InvocationOperation::new("submit").unwrap(),
            },
            Contribution::SessionController {
                id: controller_id.clone(),
                input_route: input_id.clone(),
            },
            Contribution::Agent {
                id: agent_id,
                name: "Quant".into(),
                icon: None,
                controller: controller_id,
            },
        ],
    };
    let binding = smelt_core::conversation::ConversationBinding::Plugin {
        plugin_id,
        route: smelt_plugin_api::PluginInputRouteBinding {
            contribution_id: input_id,
            context: serde_json::json!({"strategy_id": "strategy-1"}),
        },
    };
    let mut agent_session: smelt_plugin_api::AgentSessionBinding =
        serde_json::from_value(serde_json::json!({
            "agent": {
                "plugin_id": "com.example.quant",
                "contribution_id": "quant-agent"
            },
            "controller": {
                "plugin_id": "com.example.quant",
                "contribution_id": "quant-session"
            },
            "instance": {
                "plugin_id": "com.example.quant",
                "resource_type": "strategy",
                "resource_id": "strategy-1"
            }
        }))
        .unwrap();

    assert!(
        validate_conversation_state(
            Some(&binding),
            Some(&agent_session),
            std::slice::from_ref(&set)
        )
        .is_ok()
    );
    agent_session.controller.contribution_id = ContributionId::new("other-session").unwrap();
    assert!(validate_conversation_state(Some(&binding), Some(&agent_session), &[set]).is_err());
}

#[test]
fn direct_conversation_must_not_touch_plugin_runtime() {
    use smelt_core::conversation::ConversationBinding;

    assert!(!conversation_needs_plugin_contributions(None, None));
    assert!(!conversation_needs_plugin_contributions(
        Some(&ConversationBinding::Direct),
        None
    ));
    assert!(!conversation_needs_plugin_contributions(
        Some(&ConversationBinding::Automation {
            run_id: "run-1".into()
        }),
        None
    ));
    assert!(validate_conversation_state(Some(&ConversationBinding::Direct), None, &[]).is_ok());
}

#[test]
fn plugin_conversation_requires_contribution_identity() {
    use smelt_core::conversation::ConversationBinding;
    use smelt_plugin_api::{ContributionId, PluginId};

    let binding = ConversationBinding::Plugin {
        plugin_id: PluginId::new("com.example.quant").unwrap(),
        route: smelt_plugin_api::PluginInputRouteBinding {
            contribution_id: ContributionId::new("quant-input").unwrap(),
            context: serde_json::json!({}),
        },
    };
    assert!(conversation_needs_plugin_contributions(
        Some(&binding),
        None
    ));
    assert!(conversation_needs_plugin_contributions(
        Some(&ConversationBinding::Direct),
        Some(
            &serde_json::from_value(serde_json::json!({
                "agent": {
                    "plugin_id": "com.example.quant",
                    "contribution_id": "quant-agent"
                },
                "controller": {
                    "plugin_id": "com.example.quant",
                    "contribution_id": "quant-session"
                },
                "instance": {
                    "plugin_id": "com.example.quant",
                    "resource_type": "strategy",
                    "resource_id": "strategy-1"
                }
            }))
            .unwrap()
        )
    ));
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
        select_resume_id(Some(" ".into()), Some("known-history".into())).as_deref(),
        Some("known-history")
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
        .entries
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
fn requested_history_id_wins_when_dead_session_is_relaunched() {
    assert_eq!(
        select_resume_id(
            Some("saved-history".to_string()),
            Some("daemon-runtime".to_string())
        ),
        Some("saved-history".to_string())
    );
    assert_eq!(
        select_resume_id(None, Some("known-history".to_string())),
        Some("known-history".to_string())
    );
}

/// 直连会话回退动作的完整门控矩阵：能力位、相位、目标类型都要在 daemon
/// 侧验证，GUI 传来的下标只当参考，不当事实源。
#[test]
fn rewind_action_gates_on_capability_phase_and_target() {
    let mut reduced = AcpSessionState::default();
    reduced.phase = DaemonPhase::Idle;
    reduced.entries.push(AcpEntry::User("first".into()));
    reduced.entries.push(AcpEntry::Assistant {
        text: "answer".into(),
        thought: false,
    });
    reduced.entries.push(AcpEntry::User("same".into()));
    reduced.entries.push(AcpEntry::User("same".into()));
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

    // 5) 合法目标：第 2 条 "same"（下标 3），occurrence 必须算出 1。
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
