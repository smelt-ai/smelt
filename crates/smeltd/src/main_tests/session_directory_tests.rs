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
        custom_title: None,
        cli_exited: false,
        agent_token: "should-not-persist".to_string(),
        title: Some("崩溃前的会话".to_string()),
        title_source: Default::default(),
        phase: Phase::AwaitingApproval,
        phase_since: 100,
        turn_started_at: 0,
        pending_question: Some("要不要继续".to_string()),
        pending_request: None,
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
        program_status: false,
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
            ServerShared {
                sessions: new_sessions(),
                acp_sessions: new_test_acp_sessions(),
                workspace_directory: new_test_workspace_directory(),
                automations: new_test_automation_store(),
                exe_mtime: 0,
                daemon_fingerprint: None,
                event_hub: new_event_hub(),
            }
            .context(Principal::Local),
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
        let snapshot = collect_subscription_snapshot(
            &sessions,
            &acp_sessions,
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

        let (server, mut client) = UnixStream::pair().unwrap();
        writeln!(
            client,
            "{}",
            serde_json::json!({ "op": "kill", "id": "ghost-1" })
        )
        .unwrap();
        handle_conn(
            server,
            ServerShared {
                sessions: Arc::clone(&sessions),
                acp_sessions: Arc::clone(&acp_sessions),
                workspace_directory: new_test_workspace_directory(),
                automations: new_test_automation_store(),
                exe_mtime: 0,
                daemon_fingerprint: None,
                event_hub: Arc::clone(&subscribers),
            }
            .context(Principal::Local),
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

        let (server, client) = UnixStream::pair().unwrap();
        let reader = BufReader::new(server.try_clone().unwrap());
        let sessions_b = Arc::clone(&sessions);
        let subscribers_b = Arc::clone(&subscribers);
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
                Some(smelt_core::conversation::ConversationBinding::Direct),
                None,
            )
        });
        {
            let mut state = slot.value.state.lock();
            state.phase = Phase::Thinking;
            state.title = Some("已恢复运行".to_string());
        }

        let snapshot = collect_subscription_snapshot(
            &sessions,
            &acp_sessions,
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
        let (server, client) = UnixStream::pair().unwrap();
        handle_acp_kill(
            server,
            &serde_json::json!({"id": "acp-1"}),
            &acp_sessions,
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

/// 守护重启后桌面用原 id 重新 open，启动续接命令；之后 CLI 一直没发 hook
/// （续接后 CLI 不一定再报 SessionStarted）。新 PTY 的初始状态不能把目录里的
/// provider / 对话 id 覆盖成空，否则再停一次机，这个终端就不再被保留、无法续接。
#[cfg(unix)]
#[test]
fn reopened_terminal_without_hooks_keeps_resume_identity_across_another_restart() {
    with_directory_env(|| {
        use std::os::unix::fs::PermissionsExt;

        let _env = super::SESSION_METADATA_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // 用户给这段对话起过名字：续接后的终端要顺着它。
        let metadata_dir =
            std::path::PathBuf::from(std::env::var_os("SMELT_GHOST_SESSIONS_DIR").unwrap())
                .join("metadata");
        std::fs::create_dir_all(&metadata_dir).unwrap();
        unsafe { std::env::set_var("SMELT_SESSION_METADATA_DIR", &metadata_dir) };
        smelt_core::session_metadata::set_custom_title(
            smelt_core::agent_kind::TerminalAgentKind::Antigravity.into(),
            None,
            "conv-1",
            Some("夜里那次排查"),
        )
        .unwrap();

        // 用阻塞读取的假 CLI 避免依赖本机安装和真实 agent；停机时由测试回收。
        let bin = std::path::PathBuf::from(std::env::var_os("SMELT_GHOST_SESSIONS_DIR").unwrap());
        let cli = bin.join("agy");
        std::fs::write(&cli, "#!/bin/sh\nread value\n").unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        let launch = format!(
            "env PATH={} agy",
            shell_words::quote(&bin.to_string_lossy())
        );
        let mut previous = recovered_state("resume-term");
        previous.launch = Some(launch.clone());
        previous.provider = Some("antigravity".to_string());
        previous.pending_question = None;
        previous.active_blocker = None;
        let directory = SessionDirectory::new();
        directory.upsert(&previous);
        directory.keep_resumable_terminals(&HashSet::from(["resume-term".to_string()]));

        // 第一次重启：条目以 runtime=false 恢复，可以续接。
        set_session_directory_for_test(SessionDirectory::restore_from_disk());
        let expected_run = format!("{launch} --conversation conv-1");
        assert_eq!(
            with_session_directory(|d| d.resume_launch("resume-term"))
                .flatten()
                .map(|resume| resume.run),
            Some(expected_run.clone())
        );

        // 桌面用原 id 重新 open：新建 PTY，走续接命令。
        let sessions = new_sessions();
        let acp_sessions = new_test_acp_sessions();
        let event_hub = new_event_hub();
        let (_, session) = open_terminal_runtime(
            TerminalOpenRequest {
                id: "resume-term".to_string(),
                cols: 80,
                rows: 24,
                cwd: None,
                launch: None,
                create_if_missing: true,
            },
            &sessions,
            &acp_sessions,
            &event_hub,
            |session, reattached| {
                assert!(!reattached, "目录条目没有运行时，应新建 PTY");
                Arc::clone(session)
            },
        )
        .unwrap_or_else(|_| panic!("重新打开留下的终端失败"));
        assert_eq!(
            session.state.lock().unwrap().launch.as_deref(),
            Some(launch.as_str()),
            "会话状态记原启动命令，不叠 --resume"
        );
        // 身份是带进来的，之后 hook 报同一段对话不算换对话、不会再查名字，开的时候就要有。
        assert_eq!(
            session.state.lock().unwrap().custom_title.as_deref(),
            Some("夜里那次排查")
        );

        // 不收到任何 hook，直接停机保存（与停机流程一样，只保留还开着的终端）。
        let open_terminals: HashSet<String> =
            sessions.snapshot().into_iter().map(|(id, _)| id).collect();
        assert!(open_terminals.contains("resume-term"));
        with_session_directory(|d| d.keep_resumable_terminals(&open_terminals));
        let _ = session.child.terminate_and_wait(TERMINAL_CHILD_REAP_TIMEOUT);

        // 第二次重启：续接身份和续接命令都还在。
        let next = SessionDirectory::restore_from_disk();
        let entry = next
            .snapshot()
            .into_iter()
            .find(|entry| entry.id == "resume-term")
            .expect("续接后没收到 hook 的终端，再停一次机不能丢");
        assert_eq!(entry.provider.as_deref(), Some("antigravity"));
        assert_eq!(entry.conversation_id.as_deref(), Some("conv-1"));
        assert_eq!(entry.launch.as_deref(), Some(launch.as_str()));
        assert!(!entry.cli_exited);
        assert_eq!(
            next.resume_launch("resume-term").map(|resume| resume.run),
            Some(expected_run)
        );
        unsafe { std::env::remove_var("SMELT_SESSION_METADATA_DIR") };
    });
}
