use super::*;

#[test]
fn history_rename_writes_metadata_overlay() {
    let dir = std::env::temp_dir().join(format!(
        "smelt-history-rename-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::create_dir_all(&dir);
    unsafe { std::env::set_var("SMELT_SESSION_METADATA_DIR", &dir) };
    let (server, mut client) = UnixStream::pair().unwrap();
    writeln!(
        client,
        "{}",
        serde_json::json!({
            "op": "history_rename",
            "agent": "claude",
            "resume_id": "hist-1",
            "title": "夜里那次排查",
        })
    )
    .unwrap();
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
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&resp).unwrap()["ok"],
        true
    );
    assert_eq!(
        smelt_core::session_metadata::custom_title(
            smelt_core::agent_kind::ConversationAgentKind::Claude.into(),
            None,
            "hist-1",
        )
        .as_deref(),
        Some("夜里那次排查")
    );
    unsafe { std::env::remove_var("SMELT_SESSION_METADATA_DIR") };
    let _ = std::fs::remove_dir_all(&dir);
}
