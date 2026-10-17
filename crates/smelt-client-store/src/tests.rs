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
