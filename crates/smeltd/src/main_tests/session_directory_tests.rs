use super::*;
use session_directory::SessionDirectory;

fn with_directory_env<T>(f: impl FnOnce() -> T) -> T {
    let _guard = session_directory::DIRECTORY_TEST_LOCK.lock().unwrap();
    let dir = std::env::temp_dir().join(format!("smelt-session-dir-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    unsafe { std::env::set_var("SMELT_GHOST_SESSIONS_DIR", &dir) };
    let result = f();
    clear_session_directory_for_test();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn recovered_directory() -> SessionDirectory {
    let directory = SessionDirectory::new();
    directory.upsert(&recovered_state("ghost-1"));
    directory.persist_now();
    SessionDirectory::restore_from_disk()
}

fn recovered_state(id: &str) -> SessionState {
    SessionState {
        id: id.to_string(),
        instance: 3,
        cwd: Some("/work/project".to_string()),
        launch: Some("claude".to_string()),
        provider: Some("claude".to_string()),
        conversation_id: Some("conv-1".to_string()),
        agent_mcp: true,
        agent_token: "should-not-persist".to_string(),
        title: Some("崩溃前的会话".to_string()),
        prompt_title: None,
        phase: Phase::AwaitingApproval,
        phase_since: 100,
        pending_question: Some("要不要继续".to_string()),
        tokens_used: Some(42),
        branch: Some("feat/ghost".to_string()),
        dirty_files: vec!["a.rs".to_string()],
        revision: 9,
        updated_at: 200,
        structured_events: true,
        turn_events: true,
        agent_event_version: Some(1),
        active_blocker: Some(AgentBlocker {
            tool_use_id: Some("t1".to_string()),
            agent_id: None,
            tool_name: None,
        }),
        runtime: true,
    }
}

#[test]
fn crash_recovery_restores_disconnected_sessions() {
    with_directory_env(|| {
        let directory = SessionDirectory::new();
        directory.upsert(&recovered_state("s1"));
        directory.persist_now();
        assert!(session_directory::session_directory_persisted());

        let recovered = SessionDirectory::restore_from_disk();
        let snap = recovered.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].id, "s1");
        assert!(!snap[0].runtime, "恢复的会话必须标记为无运行时");
        assert_eq!(snap[0].phase, Phase::AwaitingApproval);
        assert_eq!(snap[0].title.as_deref(), Some("崩溃前的会话"));
        assert_eq!(snap[0].cwd.as_deref(), Some("/work/project"));
        assert_eq!(snap[0].launch.as_deref(), Some("claude"));
        assert_eq!(snap[0].provider.as_deref(), Some("claude"));
        assert_eq!(snap[0].tokens_used, Some(42));
        assert_eq!(snap[0].branch.as_deref(), Some("feat/ghost"));
        assert_eq!(snap[0].dirty_files, vec!["a.rs".to_string()]);
        assert_eq!(snap[0].pending_question, None);
        assert_eq!(snap[0].agent_token, "");
        assert!(snap[0].active_blocker.is_none());
    });
}

#[test]
fn list_includes_recovered_sessions_as_disconnected() {
    with_directory_env(|| {
        set_session_directory_for_test(recovered_directory());

        let (server, mut client) = UnixStream::pair().unwrap();
        writeln!(client, "{}", serde_json::json!({ "op": "list" })).unwrap();
        handle_conn(
            server,
            ServerContext {
                sessions: new_sessions(),
                acp_sessions: new_test_acp_sessions(),
                remote_sessions: new_test_remote_sessions(),
                workspace_menu: new_test_workspace_menu(),
                automations: new_test_automation_store(),
                exe_mtime: 0,
                daemon_fingerprint: None,
                listen_fd: -1,
                remote_state: new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string())),
                iroh_state: Arc::new(Mutex::new(None)),
                iroh_connections: new_iroh_connections(),
                event_hub: new_event_hub(),
            },
        );

        let mut resp = String::new();
        BufReader::new(client).read_line(&mut resp).unwrap();
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let ids = v["sessions"].as_array().unwrap();
        assert_eq!(ids.len(), 1, "崩溃恢复的会话应出现在 list 输出");
        assert_eq!(ids[0], "ghost-1");
        let state = &v["states"][0];
        assert_eq!(state["runtime"], false);
        assert_eq!(state["phase"], "awaiting_approval");
        assert_eq!(state["connected"], false);
        assert_eq!(state["title"], "崩溃前的会话");
    });
}

#[test]
fn subscribe_snapshot_includes_recovered_sessions() {
    with_directory_env(|| {
        set_session_directory_for_test(recovered_directory());

        let sessions = new_sessions();
        let acp_sessions = new_test_acp_sessions();
        let remote_sessions = new_test_remote_sessions();
        let snapshot = collect_subscription_snapshot(
            &sessions,
            &acp_sessions,
            &remote_sessions,
            &new_test_workspace_menu(),
            AutomationFile::default(),
        );
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].id, "ghost-1");
        assert!(!snapshot.sessions[0].runtime);
        assert_eq!(snapshot.sessions[0].phase, Phase::AwaitingApproval);
    });
}

#[test]
fn kill_recovered_session_removes_identity() {
    with_directory_env(|| {
        set_session_directory_for_test(recovered_directory());

        let sessions = new_sessions();
        let acp_sessions = new_test_acp_sessions();
        let subscribers = new_event_hub();
        let remote_sessions = new_test_remote_sessions();

        let (server, mut client) = UnixStream::pair().unwrap();
        writeln!(
            client,
            "{}",
            serde_json::json!({ "op": "kill", "id": "ghost-1" })
        )
        .unwrap();
        handle_conn(
            server,
            ServerContext {
                sessions: Arc::clone(&sessions),
                acp_sessions: Arc::clone(&acp_sessions),
                remote_sessions: Arc::clone(&remote_sessions),
                workspace_menu: new_test_workspace_menu(),
                automations: new_test_automation_store(),
                exe_mtime: 0,
                daemon_fingerprint: None,
                listen_fd: -1,
                remote_state: new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string())),
                iroh_state: Arc::new(Mutex::new(None)),
                iroh_connections: new_iroh_connections(),
                event_hub: Arc::clone(&subscribers),
            },
        );

        let mut resp = String::new();
        BufReader::new(client).read_line(&mut resp).unwrap();
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["ok"], true);

        assert!(!with_session_directory(|d| d.contains("ghost-1")).unwrap_or(false));
        let reloaded = SessionDirectory::restore_from_disk();
        assert!(reloaded.snapshot().is_empty());
        let snapshot = collect_subscription_snapshot(
            &sessions,
            &acp_sessions,
            &remote_sessions,
            &new_test_workspace_menu(),
            AutomationFile::default(),
        );
        assert!(snapshot.sessions.is_empty());
    });
}

#[test]
fn failed_open_keeps_recovered_identity() {
    with_directory_env(|| {
        set_session_directory_for_test(recovered_directory());

        let sessions = new_sessions();
        let subscribers = new_event_hub();
        let remote_sessions = new_test_remote_sessions();

        let (server, client) = UnixStream::pair().unwrap();
        let reader = BufReader::new(server.try_clone().unwrap());
        let sessions_b = Arc::clone(&sessions);
        let subscribers_b = Arc::clone(&subscribers);
        let remote_b = Arc::clone(&remote_sessions);
        let h = thread::spawn(move || {
            handle_open(
                server,
                reader,
                &serde_json::json!({
                    "id": "ghost-1",
                    "cols": 80,
                    "rows": 24,
                    "cwd": "/nonexistent-dir-xyz",
                    "create_if_missing": true,
                }),
                sessions_b,
                new_test_acp_sessions(),
                subscribers_b,
                remote_b,
            );
        });

        drop(client);
        let _ = h.join();

        assert!(
            with_session_directory(|d| d.contains("ghost-1")).unwrap_or(false),
            "open 失败不得删掉崩溃恢复的身份"
        );
    });
}

#[test]
fn live_acp_wins_over_recovered_identity_in_snapshot() {
    with_directory_env(|| {
        let directory = SessionDirectory::new();
        directory.upsert(&recovered_state("acp-1"));
        directory.persist_now();
        set_session_directory_for_test(SessionDirectory::restore_from_disk());

        let sessions = new_sessions();
        let acp_sessions = new_test_acp_sessions();
        let (slot, _) = acp_sessions.reserve_with("acp-1", || {
            make_acp_session(
                "acp-1",
                None,
                false,
                Some(smelt_core::conversation::ConversationBinding::Direct),
                None,
                None,
            )
        });
        {
            let mut state = slot.value.state.lock().unwrap();
            state.phase = Phase::Thinking;
            state.title = Some("已恢复运行".to_string());
        }

        let snapshot = collect_subscription_snapshot(
            &sessions,
            &acp_sessions,
            &new_test_remote_sessions(),
            &new_test_workspace_menu(),
            AutomationFile::default(),
        );
        let same_id = snapshot
            .sessions
            .iter()
            .filter(|state| state.id == "acp-1")
            .collect::<Vec<_>>();
        assert_eq!(same_id.len(), 1, "同 id 不得同时出现活会话和断连条目");
        assert!(same_id[0].runtime);
        assert_eq!(same_id[0].phase, Phase::Thinking);
    });
}

#[test]
fn acp_kill_removes_disconnected_identity() {
    with_directory_env(|| {
        let directory = SessionDirectory::new();
        directory.upsert(&recovered_state("acp-1"));
        directory.persist_now();
        set_session_directory_for_test(SessionDirectory::restore_from_disk());

        let acp_sessions = new_test_acp_sessions();
        let subscribers = new_event_hub();
        let remote_sessions = new_test_remote_sessions();
        let (server, client) = UnixStream::pair().unwrap();
        handle_acp_kill(
            server,
            &serde_json::json!({"id": "acp-1"}),
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

        assert!(!with_session_directory(|d| d.contains("acp-1")).unwrap_or(false));
        assert!(SessionDirectory::restore_from_disk().snapshot().is_empty());
        let snapshot = collect_subscription_snapshot(
            &new_sessions(),
            &acp_sessions,
            &remote_sessions,
            &new_test_workspace_menu(),
            AutomationFile::default(),
        );
        assert!(snapshot.sessions.is_empty());
    });
}

#[test]
fn rejected_bus_updates_cannot_resurrect_directory_entries() {
    with_directory_env(|| {
        set_session_directory_for_test(SessionDirectory::new());
        let subscribers = new_event_hub();
        let current = SessionState {
            id: "race-1".to_string(),
            instance: 9,
            revision: 2,
            title: Some("new state".to_string()),
            ..Default::default()
        };
        let mut stale = current.clone();
        stale.revision = 1;
        stale.title = Some("old state".to_string());

        broadcast_state(&subscribers, &current);
        broadcast_state(&subscribers, &stale);
        let entry = with_session_directory(|directory| directory.snapshot())
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(entry.revision, 2);
        assert_eq!(entry.title.as_deref(), Some("new state"));

        forget_session(&subscribers, "race-1", 9);
        broadcast_state(&subscribers, &stale);
        assert!(
            !with_session_directory(|directory| directory.contains("race-1")).unwrap(),
            "被退休 runtime 的迟到回调不能把目录条目写回来"
        );
        assert!(SessionDirectory::restore_from_disk().snapshot().is_empty());
    });
}
