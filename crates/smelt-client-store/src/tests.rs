use super::*;

fn temp_path() -> PathBuf {
    std::env::temp_dir()
        .join(format!("smelt-client-store-{}", uuid::Uuid::new_v4()))
        .join(DATABASE_FILE_NAME)
}

fn temp_store() -> (ClientStore, PathBuf) {
    let path = temp_path();
    let store = ClientStore::open(&path).unwrap();
    (store, path)
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

fn appearance() -> AppearanceRecord {
    AppearanceRecord {
        bg_color: 0x1e1e1e,
        bg_image: Some("/Users/me/bg.png".to_string()),
        bg_image_opacity: 0.25,
        opacity: 0.95,
        blur: true,
        glass_style: "regular".to_string(),
        theme_mode: "dark".to_string(),
        theme_name: "Default Dark".to_string(),
        ui_font_px: 16,
        ui_font_family: String::new(),
        font_px: 13,
        font_family: "JetBrains Mono".to_string(),
    }
}

fn workspace_ui() -> WorkspaceUiRecord {
    WorkspaceUiRecord {
        active_session: 2,
        active_session_id: Some("s-1".to_string()),
        route: "session".to_string(),
        selected_agent_id: None,
        sidebar_w: Some(320.0),
        sidebar_open: Some(true),
        collapsed_projects: vec![ProjectKey::local("/repo/b"), ProjectKey::local("/repo/a")],
        collapsed_agents: vec!["writer".to_string()],
        sidebar_grouping: "project".to_string(),
        pinned_projects: vec![ProjectKey::local("/repo/z"), ProjectKey::local("/repo/a")],
        sidebar_hide_empty_projects: true,
    }
}

#[test]
fn records_round_trip_and_start_empty() {
    let (store, path) = temp_store();
    assert_eq!(store.appearance().unwrap(), None);
    assert_eq!(store.workspace_ui().unwrap(), None);
    assert_eq!(store.update_state().unwrap(), UpdateStateRecord::default());
    assert!(!store.runtime_migrated().unwrap());

    store.put_appearance(&appearance()).unwrap();
    assert_eq!(store.appearance().unwrap(), Some(appearance()));

    let settings = UpdateSettingsRecord {
        channel: "dev".to_string(),
        auto_install: false,
    };
    store.put_update_settings(&settings).unwrap();
    assert_eq!(store.update_settings().unwrap(), Some(settings));

    let prefs = NotifyPrefsRecord {
        approval: true,
        input: false,
        success: true,
        failure: true,
        terminal_bell: false,
    };
    store.put_notify_prefs(&prefs).unwrap();
    assert_eq!(store.notify_prefs().unwrap(), Some(prefs));

    store.put_workspace_ui(&workspace_ui()).unwrap();
    assert_eq!(store.workspace_ui().unwrap(), Some(workspace_ui()));
    let shrunk = WorkspaceUiRecord {
        collapsed_projects: Vec::new(),
        pinned_projects: vec![ProjectKey::local("/repo/a")],
        ..workspace_ui()
    };
    store.put_workspace_ui(&shrunk).unwrap();
    assert_eq!(store.workspace_ui().unwrap(), Some(shrunk));

    // 两个运行时上可以有同一个路径，各算各的。
    let remote_a = ProjectKey {
        runtime_id: Some("rt-linux".to_string()),
        root: "/repo/a".to_string(),
    };
    let both = WorkspaceUiRecord {
        pinned_projects: vec![remote_a.clone(), ProjectKey::local("/repo/a")],
        collapsed_projects: vec![remote_a],
        ..workspace_ui()
    };
    store.put_workspace_ui(&both).unwrap();
    assert_eq!(store.workspace_ui().unwrap(), Some(both));

    store.mark_runtime_migrated().unwrap();
    store.mark_runtime_migrated().unwrap();
    assert!(store.runtime_migrated().unwrap());
    cleanup(&path);
}

fn route(runtime_id: Option<&str>, conversation_id: &str) -> ConversationRouteRecord {
    ConversationRouteRecord {
        runtime_id: runtime_id.map(str::to_string),
        conversation_id: conversation_id.to_string(),
        active_terminal_id: Some("term-2".to_string()),
        dock_layout: Some(r#"{"v":1,"center":{"terminal":"term-2"}}"#.to_string()),
        stage_cover: Some("inspector".to_string()),
        tool_panel_tab: "git".to_string(),
        tool_panel_open: true,
        tool_panel_w: 420.0,
        expanded: vec!["/repo/src".to_string(), "/repo/docs".to_string()],
        file_tree_selected: Some("/repo/src/main.rs".to_string()),
        open_file_path: None,
        file_tree_w: 240.0,
        file_tree_open: false,
        pinned_roots: vec!["/repo".to_string()],
        collapsed_roots: Vec::new(),
        git_tab: "log".to_string(),
        diff_split: true,
        git_tree_collapsed: vec!["src".to_string()],
        diff_scope: "unstaged".to_string(),
    }
}

/// 对话的界面状态整份换：关掉的对话不留；两个运行时上同一个对话 id 各算各的。
#[test]
fn conversation_routes_are_replaced_as_a_whole() {
    let (store, path) = temp_store();
    assert!(store.conversation_routes().unwrap().is_empty());

    let routes = vec![
        route(None, "conv-1"),
        route(Some("rt-linux"), "conv-1"),
        ConversationRouteRecord {
            active_terminal_id: None,
            dock_layout: None,
            stage_cover: None,
            expanded: Vec::new(),
            ..route(None, "conv-2")
        },
    ];
    store.put_conversation_routes(&routes).unwrap();
    let key = |record: &ConversationRouteRecord| {
        (record.runtime_id.clone(), record.conversation_id.clone())
    };
    let mut read = store.conversation_routes().unwrap();
    read.sort_by_key(key);
    let mut expected = routes.clone();
    expected.sort_by_key(key);
    assert_eq!(read, expected);

    store.put_conversation_routes(&routes[1..2]).unwrap();
    assert_eq!(store.conversation_routes().unwrap(), routes[1..2].to_vec());
    cleanup(&path);
}

/// 分屏布局只当 JSON 存：不是合法 JSON 的写不进去，整份写入跟着回滚。
#[test]
fn a_dock_layout_must_be_json() {
    let (store, path) = temp_store();
    store
        .put_conversation_routes(&[route(None, "conv-1")])
        .unwrap();
    let broken = ConversationRouteRecord {
        dock_layout: Some("{not json".to_string()),
        ..route(None, "conv-2")
    };
    assert!(store.put_conversation_routes(&[broken]).is_err());
    assert_eq!(
        store.conversation_routes().unwrap(),
        vec![route(None, "conv-1")]
    );
    cleanup(&path);
}

/// v2 的会话路由按活动终端的 id 存，对不上对话的稳定 id，升到 v3 时丢掉；别的界面状态留着。
#[test]
fn v2_session_routes_are_dropped() {
    let path = temp_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let v2 = Connection::open(&path).unwrap();
    v2.execute_batch(schema::CREATE_SCHEMA).unwrap();
    v2.execute_batch(
        "DROP TABLE conversation_route;
         DROP TABLE conversation_route_path;
         CREATE TABLE session_route (
           runtime_id TEXT NOT NULL,
           session_id TEXT NOT NULL,
           stage_cover TEXT,
           tool_panel_tab TEXT NOT NULL,
           tool_panel_open INTEGER NOT NULL CHECK (tool_panel_open IN (0, 1)),
           tool_panel_w REAL NOT NULL,
           file_tree_selected TEXT,
           open_file_path TEXT,
           file_tree_w REAL NOT NULL,
           file_tree_open INTEGER NOT NULL CHECK (file_tree_open IN (0, 1)),
           git_tab TEXT NOT NULL,
           diff_split INTEGER NOT NULL CHECK (diff_split IN (0, 1)),
           diff_scope TEXT NOT NULL,
           PRIMARY KEY (runtime_id, session_id)
         ) WITHOUT ROWID;
         CREATE TABLE session_route_path (
           runtime_id TEXT NOT NULL,
           session_id TEXT NOT NULL,
           kind TEXT NOT NULL,
           path TEXT NOT NULL,
           position INTEGER NOT NULL,
           PRIMARY KEY (runtime_id, session_id, kind, path)
         ) WITHOUT ROWID;
         INSERT INTO session_route VALUES
           ('', 'term-1', NULL, 'git', 1, 420.0, NULL, NULL, 240.0, 1, 'changes', 0, 'all');
         INSERT INTO session_route_path VALUES ('', 'term-1', 'expanded', '/repo/src', 0);
         INSERT INTO workspace_ui VALUES (1, 2, 's-1', 'session', NULL, 320.0, 1, 'project', 1);
         PRAGMA user_version = 2;",
    )
    .unwrap();
    drop(v2);

    let store = ClientStore::open(&path).unwrap();
    assert!(store.conversation_routes().unwrap().is_empty());
    assert_eq!(store.workspace_ui().unwrap(), Some(workspace_ui()));
    store
        .put_conversation_routes(&[route(None, "conv-1")])
        .unwrap();
    assert_eq!(store.conversation_routes().unwrap().len(), 1);
    drop(store);
    let version: u32 = Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, schema::SCHEMA_VERSION);

    // 迁移前的库原样留了一份，覆盖回去就回到 v2。
    let backup = Connection::open(path.with_file_name(format!("{DATABASE_FILE_NAME}.bak-v2")))
        .expect("迁移前必须留下备份");
    let backup_version: u32 = backup
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(backup_version, 2);
    let old_routes: u32 = backup
        .query_row("SELECT COUNT(*) FROM session_route", [], |row| row.get(0))
        .unwrap();
    assert_eq!(old_routes, 1);
    cleanup(&path);
}

#[test]
fn a_new_or_current_store_leaves_no_backup() {
    let (store, path) = temp_store();
    drop(store);
    ClientStore::open(&path).unwrap();
    let backups: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().contains(".bak-v"))
        .collect();
    assert!(backups.is_empty());
    cleanup(&path);
}

#[test]
fn update_state_keeps_the_staged_job_and_its_attempt() {
    let (store, path) = temp_store();
    let state = UpdateStateRecord {
        current_url: Some("https://example.test/a.zip".to_string()),
        cleanup_app: Some(PathBuf::from("/Applications/.Smelt.old.app")),
        staged: Some(StagedUpdateRecord {
            update_id: "job".to_string(),
            version: "0.9.1".to_string(),
            url: "https://example.test/b.zip".to_string(),
            staged_app: PathBuf::from("/cache/Smelt-0.9.1.app"),
            fingerprint: "abc".to_string(),
            phase: "applying".to_string(),
            swap_app: Some(PathBuf::from("/Applications/.Smelt.swap.app")),
            installer_attempt: Some(InstallerAttemptRecord {
                attempt_id: "attempt".to_string(),
                target_app: PathBuf::from("/Applications/Smelt.app"),
                relaunch: true,
                helper_dir: PathBuf::from("/cache/helper"),
            }),
            installer_failed: false,
        }),
    };
    store.put_update_state(&state).unwrap();
    assert_eq!(store.update_state().unwrap(), state);

    let finished = UpdateStateRecord {
        staged: None,
        ..state
    };
    store.put_update_state(&finished).unwrap();
    assert_eq!(store.update_state().unwrap(), finished);
    cleanup(&path);
}

#[test]
fn runtime_connections_keep_order_and_repairing_overwrites_in_place() {
    let (store, path) = temp_store();
    let linux = RuntimeConnection {
        runtime_id: "rt-linux".to_string(),
        name: "linux-box".to_string(),
        endpoint_id: "aa".to_string(),
        relay_url: None,
        added_at_ms: 1,
    };
    let pi = RuntimeConnection {
        runtime_id: "rt-pi".to_string(),
        name: "pi".to_string(),
        endpoint_id: "bb".to_string(),
        relay_url: Some("https://relay.example".to_string()),
        added_at_ms: 2,
    };
    store.put_runtime_connection(&linux).unwrap();
    store.put_runtime_connection(&pi).unwrap();
    store
        .put_runtime_connection(&RuntimeConnection {
            endpoint_id: "cc".to_string(),
            added_at_ms: 3,
            ..linux
        })
        .unwrap();
    let listed = store.runtime_connections().unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|r| r.runtime_id.as_str())
            .collect::<Vec<_>>(),
        ["rt-linux", "rt-pi"]
    );
    assert_eq!(listed[0].endpoint_id, "cc");
    assert_eq!(listed[0].added_at_ms, 1, "重新配对不改添加时间");
    assert!(store.delete_runtime_connection("rt-linux").unwrap());
    assert!(!store.delete_runtime_connection("rt-linux").unwrap());
    assert_eq!(store.runtime_connections().unwrap(), vec![pi]);
    cleanup(&path);
}

/// 第一版的「作用域 + 键 → JSON」偏好表：打开时搬进正式的表，然后删掉。
#[test]
fn legacy_preference_rows_move_into_tables_once() {
    let path = temp_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let legacy = Connection::open(&path).unwrap();
    legacy
        .execute_batch(
            r#"CREATE TABLE client_pref (
                 scope TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
                 updated_at_ms INTEGER NOT NULL, PRIMARY KEY (scope, key)) WITHOUT ROWID;
               INSERT INTO client_pref VALUES ('appearance', 'current',
                 '{"bg_color":1973790,"bg_image":"/Users/me/bg.png","opacity":0.95,"blur":true,
                   "glass_style":"regular","theme_mode":"dark","theme_name":"Default Dark",
                   "ui_font_px":16,"ui_font_family":"","font_px":13,
                   "font_family":"JetBrains Mono"}', 1);
               INSERT INTO client_pref VALUES ('workspace', 'ui',
                 '{"active_session":2,"active_session_id":"s-1","route":"session",
                   "sidebar_w":320.0,"sidebar_open":true,
                   "collapsed_projects":["/repo/b","/repo/a"],"collapsed_agents":["writer"],
                   "sidebar_grouping":"project","pinned_projects":["/repo/z","/repo/a"],
                   "sidebar_hide_empty_projects":true,
                   "shell_dock":{"version":3,"center":{}}}', 1);
               INSERT INTO client_pref VALUES ('notify', 'prefs', 'not json', 1);
               INSERT INTO client_pref VALUES ('meta', 'runtime_migrated', 'true', 1);"#,
        )
        .unwrap();
    drop(legacy);

    let store = ClientStore::open(&path).unwrap();
    assert_eq!(store.appearance().unwrap(), Some(appearance()));
    assert_eq!(store.workspace_ui().unwrap(), Some(workspace_ui()));
    assert_eq!(
        store.notify_prefs().unwrap(),
        None,
        "解析不了的旧值用出厂值重来"
    );
    assert!(store.runtime_migrated().unwrap());
    drop(store);

    let connection = Connection::open(&path).unwrap();
    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, schema::SCHEMA_VERSION);
    let leftover: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'client_pref')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!leftover);
    drop(connection);
    cleanup(&path);
}

/// v1 的折叠、置顶不分运行时，升上来都算本机的。
#[test]
fn v1_projects_become_local_projects() {
    let path = temp_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let v1 = Connection::open(&path).unwrap();
    v1.execute_batch(schema::CREATE_SCHEMA).unwrap();
    v1.execute_batch(
        "DROP TABLE collapsed_project;
         DROP TABLE pinned_project;
         DROP TABLE conversation_route;
         DROP TABLE conversation_route_path;
         CREATE TABLE collapsed_project (root TEXT PRIMARY KEY, position INTEGER NOT NULL) WITHOUT ROWID;
         CREATE TABLE pinned_project (root TEXT PRIMARY KEY, position INTEGER NOT NULL) WITHOUT ROWID;
         INSERT INTO workspace_ui VALUES (1, 2, 's-1', 'session', NULL, 320.0, 1, 'project', 1);
         INSERT INTO collapsed_project VALUES ('/repo/b', 0), ('/repo/a', 1);
         INSERT INTO collapsed_agent VALUES ('writer', 0);
         INSERT INTO pinned_project VALUES ('/repo/z', 0), ('/repo/a', 1);
         PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(v1);

    let store = ClientStore::open(&path).unwrap();
    assert_eq!(store.workspace_ui().unwrap(), Some(workspace_ui()));
    drop(store);
    let version: u32 = Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, schema::SCHEMA_VERSION);
    cleanup(&path);
}

#[test]
fn a_newer_store_is_refused() {
    let path = temp_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "user_version", schema::SCHEMA_VERSION + 1)
        .unwrap();
    drop(connection);
    assert!(ClientStore::open(&path).is_err());
    cleanup(&path);
}
