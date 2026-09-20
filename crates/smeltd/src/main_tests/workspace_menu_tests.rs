use super::*;
use smelt_event_bus::Delivery;
use smelt_plugin_api::{
    CORE_TOPIC_WORKSPACE_MENU_CHANGED, DeliveryClass, PluginId, SubscriptionId, Topic,
};
use std::collections::BTreeSet;

static WORKSPACE_MENU_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn workspace_menu_op_persists() {
    let _guard = WORKSPACE_MENU_TEST_LOCK.lock().unwrap();
    let dir = std::env::temp_dir().join(format!(
        "smelt-workspace-menu-op-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::create_dir_all(&dir);
    unsafe { std::env::set_var("SMELT_WORKSPACE_MENU_DIR", &dir) };

    let store = new_test_workspace_menu();
    let subscribers = new_event_hub();
    let (server, mut client) = UnixStream::pair().unwrap();
    writeln!(
        client,
        "{}",
        serde_json::json!({
            "op": "workspace_menu",
            "menu": {
                "version": 2,
                "projects": [{"root": "/repo", "title": "repo", "order": 0}],
                "sessions": []
            }
        })
    )
    .unwrap();
    handle_conn(
        server,
        ServerContext {
            sessions: new_sessions(),
            acp_sessions: new_test_acp_sessions(),
            remote_sessions: new_test_remote_sessions(),
            workspace_menu: Arc::clone(&store),
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
    assert_eq!(v["revision"], 1);
    assert_eq!(store.lock().unwrap().projects[0].root, "/repo");
    assert_eq!(
        smelt_core::workspace_menu::load_published_workspace_menu().revision,
        1
    );

    unsafe { std::env::remove_var("SMELT_WORKSPACE_MENU_DIR") };
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stale_menu_from_same_desktop_source_is_ignored() {
    let _guard = WORKSPACE_MENU_TEST_LOCK.lock().unwrap();
    let dir = std::env::temp_dir().join(format!(
        "smelt-workspace-menu-order-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    unsafe { std::env::set_var("SMELT_WORKSPACE_MENU_DIR", &dir) };

    let store = new_test_workspace_menu();
    let subscribers = new_event_hub();
    let events = subscribers
        .subscribe(
            PluginId::new("test.workspace-menu").unwrap(),
            SubscriptionId::new("menu").unwrap(),
            BTreeSet::from([Topic::new(CORE_TOPIC_WORKSPACE_MENU_CHANGED).unwrap()]),
            DeliveryClass::Ephemeral,
            &core_read_capabilities(),
        )
        .unwrap();
    let newer = WorkspaceMenuSnapshot::current(
        vec![smelt_core::workspace_menu::WorkspaceMenuProject {
            root: "/new".into(),
            title: "new".into(),
            order: 0,
        }],
        vec![],
    )
    .with_source("desktop-a", 2);
    let accepted = publish_workspace_menu_snapshot(&store, &subscribers, newer).unwrap();
    assert_eq!(accepted.revision, 1);
    loop {
        match events.recv().unwrap() {
            Delivery::Event(_) => break,
            Delivery::Snapshot(_) => {}
            Delivery::Lag { dropped, .. } => panic!("unexpected lag: {dropped}"),
        }
    }

    let stale = WorkspaceMenuSnapshot::current(
        vec![smelt_core::workspace_menu::WorkspaceMenuProject {
            root: "/old".into(),
            title: "old".into(),
            order: 0,
        }],
        vec![],
    )
    .with_source("desktop-a", 1);
    let reply = publish_workspace_menu_snapshot(&store, &subscribers, stale).unwrap();

    assert_eq!(reply, accepted, "迟到请求应收到当前已提交快照");
    assert_eq!(store.lock().unwrap().projects[0].root, "/new");
    assert!(matches!(
        events.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));

    unsafe { std::env::remove_var("SMELT_WORKSPACE_MENU_DIR") };
    let _ = std::fs::remove_dir_all(&dir);
}
