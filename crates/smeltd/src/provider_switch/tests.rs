use super::*;
use smelt_core::daemon_state::DaemonPhase;
use smelt_core::session_control::AcpAgentOption;
use smelt_runtime_client::protocol::history::HistoryTurn;
use std::io::{BufRead as _, Write as _};

const FAKE: &str = "SMELT_TEST_SWITCH_PROVIDER";

// 和 acp_conn 的假 agent 一样，在 libtest 输出任何内容前接管子进程 stdio。
#[ctor::ctor(unsafe)]
fn fake_provider() {
    let Ok(kind) = std::env::var(FAKE) else {
        return;
    };
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(request) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let (Some(id), Some(method)) = (request.get("id"), request["method"].as_str()) else {
            continue;
        };
        let native = format!("native-{kind}");
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(std::env::var("SMELT_TEST_SWITCH_LOG").unwrap())
            .unwrap();
        writeln!(
            log,
            "{}",
            serde_json::json!({"kind": kind, "method": method, "params": request["params"]})
        )
        .unwrap();
        if kind == "fail" {
            writeln!(out, "{}", serde_json::json!({"jsonrpc":"2.0", "id": id, "error":{"code":-32603,"message":"fake startup failed"}})).unwrap();
            out.flush().unwrap();
            break;
        }
        let mut update = |kind: &str, text: &str| {
            writeln!(out, "{}", serde_json::json!({"jsonrpc":"2.0", "method":"session/update",
                "params":{"sessionId":native,"update":{"sessionUpdate":kind,"content":{"type":"text","text":text}}}})).unwrap();
            out.flush().unwrap();
        };
        let result = match method {
            "initialize" => {
                serde_json::json!({"protocolVersion":1,"agentCapabilities":{"loadSession":true}})
            }
            "session/new" => serde_json::json!({"sessionId":native}),
            "session/load" => {
                assert_eq!(
                    request["params"]["sessionId"], native,
                    "原生 ID 必须属于目标 provider"
                );
                update("user_message_chunk", &format!("{kind} user"));
                update("agent_message_chunk", &format!("{kind} answer"));
                serde_json::json!({})
            }
            "session/prompt" => {
                update("agent_message_chunk", &format!("{kind} answer"));
                serde_json::json!({"stopReason":"end_turn"})
            }
            _ => serde_json::json!({}),
        };
        writeln!(
            out,
            "{}",
            serde_json::json!({"jsonrpc":"2.0","id":id,"result":result})
        )
        .unwrap();
        out.flush().unwrap();
    }
    std::process::exit(0);
}

fn option(kind: &str, id: &str, log: &std::path::Path) -> AcpAgentOption {
    AcpAgentOption {
        id: id.into(),
        kind: id.into(),
        label: id.into(),
        profile: false,
        agent_definition_id: None,
        history_dir: None,
        launch: ConversationLaunchSpec {
            command: shell_words::quote(&std::env::current_exe().unwrap().to_string_lossy())
                .into_owned(),
            env: std::collections::BTreeMap::from([
                (FAKE.into(), kind.into()),
                (
                    "SMELT_TEST_SWITCH_LOG".into(),
                    log.to_string_lossy().into_owned(),
                ),
            ]),
        },
    }
}

struct Fixture {
    temp: tempfile::TempDir,
    store: Store,
    sessions: AcpSessions,
    events: EventHubHandle,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::create(temp.path().join("test.sqlite3")).unwrap();
        let a = option("A", "codex", &temp.path().join("wire.log"));
        store
            .create_workspace_conversation(&smelt_store::WorkspaceSessionRecord {
                id: "g".into(),
                custom_title: None,
                last_updated_at: 0,
                archived_at: None,
                workset_id: None,
                closed_workset_root: None,
                terminals: vec![],
                acp: Some(WorkspaceAcpRecord {
                    sid: Some("acp-switch-test".into()),
                    cwd: Some(temp.path().to_string_lossy().into_owned()),
                    agent: "codex".into(),
                    history_session_id: Some("native-A".into()),
                    launch_command: a.launch.command,
                    launch_env: a.launch.env.into_iter().collect(),
                    ..Default::default()
                }),
            })
            .unwrap();
        let sessions = crate::acp_host::new_test_acp_sessions();
        let (slot, _) = sessions.reserve_with("acp-switch-test", || {
            crate::acp_host::make_acp_session(
                "acp-switch-test",
                Some(temp.path().to_string_lossy().into_owned()),
                None,
                None,
            )
        });
        {
            let mut state = slot.value.reduced.lock();
            state.phase = DaemonPhase::Idle;
            state.history_session_id = Some("native-A".into());
            state.acp_session_id = Some("native-A".into());
            state.entries_mut().extend([
                AcpEntry::User("A user".into()),
                AcpEntry::Assistant {
                    text: "A answer".into(),
                    thought: false,
                },
            ]);
        }
        Self {
            temp,
            store,
            sessions,
            events: crate::new_event_hub(),
        }
    }

    fn switch(&self, expected: &str, target: &str, fake: &str) -> Result<WorkspaceAcp, String> {
        switch_with(
            ProviderSwitchParams {
                session_id: "acp-switch-test".into(),
                expected_provider: expected.into(),
                agent_option_id: target.into(),
            },
            &self.store,
            option(fake, target, &self.temp.path().join("wire.log")),
            &self.sessions,
            &self.events,
            turns,
        )
    }

    fn prompt(&self, text: &str) {
        let slot = self.sessions.get("acp-switch-test").unwrap();
        crate::acp_host::apply_acp_user_action(
            &slot.value,
            AcpUserAction::Prompt {
                text: text.into(),
                images: vec![],
                delivery_id: None,
            },
            &self.events,
        )
        .unwrap();
        let (state, timeout) = slot
            .value
            .reduced
            .wait_timeout_while(Duration::from_secs(5), |s| {
                s.phase != DaemonPhase::Idle || s.turn_outcome.is_none()
            });
        assert!(!timeout);
        drop(state);
        // 等这批事件的交接回执写完，不能只看相位先变成 Idle。
        drop(slot.value.turn_completion.lock());
    }

    fn wire(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.temp.path().join("wire.log"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = crate::acp_host::kill_acp_session("acp-switch-test", &self.sessions, &self.events);
    }
}

fn turns(_: &WorkspaceAcp, id: &str) -> Result<Vec<HistoryTurn>, String> {
    let kind = id.strip_prefix("native-").ok_or("wrong native id")?;
    Ok(vec![
        HistoryTurn {
            is_user: true,
            text: format!("{kind} user"),
            ..Default::default()
        },
        HistoryTurn {
            text: format!("{kind} answer"),
            ..Default::default()
        },
    ])
}

#[test]
fn a_b_a_uses_separate_native_ids_and_only_hands_back_missed_context() {
    let f = Fixture::new();
    let b = f.switch("codex", "claude", "B").unwrap();
    assert_eq!(b.sid, "acp-switch-test");
    assert_eq!(b.cwd, Some(f.temp.path().to_string_lossy().into_owned()));
    assert!(
        !f.wire()
            .iter()
            .any(|r| r["kind"] == "B" && r["method"] == "session/load")
    );
    f.prompt("B user");
    let first = f
        .wire()
        .into_iter()
        .find(|r| r["kind"] == "B" && r["method"] == "session/prompt")
        .unwrap();
    assert!(first.to_string().contains("A user"));
    assert!(first.to_string().contains("A answer"));
    assert_eq!(
        f.store.provider_history(&b.sid).unwrap().handoffs[0].status,
        "delivered"
    );
    let a = f.switch("claude", "codex", "A").unwrap();
    assert_eq!(a.history_session_id.as_deref(), Some("native-A"));
    assert!(f.wire().iter().any(|r| r["kind"] == "A"
        && r["method"] == "session/load"
        && r["params"]["sessionId"] == "native-A"));
    let slot = f.sessions.get(&a.sid).unwrap();
    let mut snapshot = slot.value.reduced.lock().to_snapshot(false);
    project(&slot.value, &mut snapshot);
    let users: Vec<_> = snapshot
        .entries
        .iter()
        .filter_map(|e| match e {
            AcpEntry::User(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        users,
        ["A user", "B user"],
        "原生重放不能覆盖或重复混合历史"
    );
    let page = crate::acp_host::read_history_page(&a.sid, &f.sessions, &f.events, 6, 2, false)
        .unwrap_or_else(|_| panic!("读取混合历史页"));
    assert_eq!(page.entries_offset, 4);
    assert_eq!(page.entries, snapshot.entries[4..6]);
    let index = crate::conversation_prompt_index::prompt_index(
        smelt_core::control_api::ConversationPromptIndexParams {
            session_id: a.sid.clone(),
            before: None,
        },
        &f.sessions,
        &f.events,
    )
    .unwrap();
    assert_eq!(
        index.prompts.iter().map(|p| p.entry_ix).collect::<Vec<_>>(),
        [1, 4]
    );
    assert_eq!(watch_offset(&slot.value, 3, Some(7), None), (7, 3));
    assert_eq!(watch_offset(&slot.value, 3, Some(4), None), (4, 2));
    let reopened = Store::open(f.temp.path().join("test.sqlite3")).unwrap();
    let cold = assemble(
        active_record(&reopened, &a.sid).unwrap(),
        reopened.provider_history(&a.sid).unwrap(),
        reopened,
        turns,
    )
    .unwrap();
    assert_eq!(cold.prefix, snapshot.entries);
    assert_eq!(cold.skip_users, 1);
    f.prompt("A followup");
    let last = f
        .wire()
        .into_iter()
        .rfind(|r| r["kind"] == "A" && r["method"] == "session/prompt")
        .unwrap();
    assert!(last.to_string().contains("B answer"));
    assert!(!last.to_string().contains("A user"));
    let mut snapshot = slot.value.reduced.lock().to_snapshot(false);
    project(&slot.value, &mut snapshot);
    let end = snapshot.entries_total;
    let page = crate::acp_host::read_history_page(&a.sid, &f.sessions, &f.events, end, 1, false)
        .unwrap_or_else(|_| panic!("读取切回后的原生历史页"));
    assert_eq!(page.entries_offset, end - 1);
    assert_eq!(page.entries, snapshot.entries[end - 1..end]);
}

#[test]
fn failed_target_start_retains_source_binding_and_history() {
    let f = Fixture::new();
    assert!(
        f.switch("codex", "claude", "fail")
            .unwrap_err()
            .contains("original provider binding was retained")
    );
    let active = active_record(&f.store, "acp-switch-test").unwrap();
    assert_eq!(active.agent, "codex");
    assert_eq!(active.history_session_id.as_deref(), Some("native-A"));
    assert!(
        f.store
            .provider_history(&active.sid)
            .unwrap()
            .segments
            .is_empty()
    );
    let slot = f.sessions.get(&active.sid).unwrap();
    crate::acp_idle::wait_for_acp_ready(&slot.value, Duration::from_secs(5)).unwrap();
}

#[test]
fn busy_stale_and_restricted_sessions_are_rejected_before_starting_target() {
    let f = Fixture::new();
    assert!(
        f.switch("claude", "codex", "A")
            .unwrap_err()
            .contains("active provider changed")
    );
    let slot = f.sessions.get("acp-switch-test").unwrap();
    slot.value.reduced.lock().phase = DaemonPhase::Thinking;
    assert!(
        f.switch("codex", "claude", "B")
            .unwrap_err()
            .contains("finish before switching")
    );
    slot.value.reduced.lock().phase = DaemonPhase::Idle;
    slot.value.disallowed_tools.lock().push("Edit".into());
    assert!(
        f.switch("codex", "claude", "B")
            .unwrap_err()
            .contains("tool-restricted")
    );
    assert!(!f.temp.path().join("wire.log").exists());
}

#[test]
fn missing_native_history_does_not_silently_produce_an_empty_handoff() {
    let f = Fixture::new();
    let result = switch_with(
        ProviderSwitchParams {
            session_id: "acp-switch-test".into(),
            expected_provider: "codex".into(),
            agent_option_id: "claude".into(),
        },
        &f.store,
        option("B", "claude", &f.temp.path().join("wire.log")),
        &f.sessions,
        &f.events,
        |_, _| Err("history missing".into()),
    );
    assert_eq!(result.unwrap_err(), "history missing");
    assert_eq!(
        active_record(&f.store, "acp-switch-test").unwrap().agent,
        "codex"
    );
}

#[test]
fn concurrent_switches_only_start_one_target_and_reject_the_stale_caller() {
    let f = Fixture::new();
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let run = || {
            barrier.wait();
            f.switch("codex", "claude", "B")
        };
        let one = scope.spawn(run);
        let two = scope.spawn(run);
        [one.join().unwrap(), two.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(results.iter().any(|r| {
        r.as_ref()
            .is_err_and(|e| e.contains("active provider changed"))
    }));
    assert_eq!(
        f.wire()
            .iter()
            .filter(|r| r["method"] == "session/new")
            .count(),
        1
    );
}

#[test]
fn stale_clients_cannot_relaunch_the_old_provider_or_replay_the_old_initial_prompt() {
    let f = Fixture::new();
    f.switch("codex", "claude", "B").unwrap();
    let request = crate::acp_host::parse_acp_open_request(&serde_json::json!({
        "id": "acp-switch-test", "launch": {"command": "old-A"}, "provider_key": "codex",
        "resume_id": "native-A", "cwd": "/wrong-workspace",
        "initial_prompt": {"text": "must not run again"},
    }))
    .unwrap();
    let prepared = prepare_open(request.clone(), &f.store).unwrap();
    assert_eq!(prepared.launch.env.get(FAKE).map(String::as_str), Some("B"));
    assert!(prepared.resume_id.is_none());
    assert!(prepared.initial_prompt.is_none());
    assert_eq!(
        prepared.cwd,
        Some(f.temp.path().to_string_lossy().into_owned())
    );
    let mut current = request;
    current.provider_key = Some("claude".into());
    current.launch.command = "explicit-current-provider-restart".into();
    assert_eq!(
        prepare_open(current, &f.store).unwrap().launch.command,
        "explicit-current-provider-restart"
    );
}

#[test]
fn an_unsent_handoff_cannot_advance_coverage_on_a_late_provider_event() {
    let f = Fixture::new();
    f.switch("codex", "claude", "B").unwrap();
    let slot = f.sessions.get("acp-switch-test").unwrap();
    let wrapped = prompt(&slot.value, "next request").unwrap();
    assert!(wrapped.contains("A user"));
    not_sent(&slot.value);
    acknowledge(&slot.value);
    let history = f.store.provider_history("acp-switch-test").unwrap();
    assert_eq!(history.handoffs[0].status, "prepared");
    assert_eq!(
        history
            .threads
            .iter()
            .find(|t| t.provider_key == "claude")
            .unwrap()
            .covered_segment,
        0
    );
}

#[test]
fn switching_preserves_logical_delivery_deduplication_and_clears_the_old_preset() {
    let f = Fixture::new();
    let slot = f.sessions.get("acp-switch-test").unwrap();
    slot.value
        .reduced
        .lock()
        .accepted_delivery_ids
        .insert("already-done".into());
    *slot.value.pending_agent_preset.lock() = Some("old provider preset".into());
    f.switch("codex", "claude", "B").unwrap();
    assert!(slot.value.pending_agent_preset.lock().is_none());
    crate::acp_host::apply_acp_user_action(
        &slot.value,
        AcpUserAction::Prompt {
            text: "must not run again".into(),
            images: vec![],
            delivery_id: Some("already-done".into()),
        },
        &f.events,
    )
    .unwrap();
    assert!(!f.wire().iter().any(|r| r["method"] == "session/prompt"));
}
