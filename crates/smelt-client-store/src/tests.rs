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

// 固定旧版 workspace_ui、conversation_route、key_binding 的形状，不能把当前 schema 的改动
// 带进旧版本迁移夹具。
fn create_v7_schema(connection: &Connection) {
    connection.execute_batch(schema::CREATE_SCHEMA).unwrap();
    restore_v11_notify_prefs(connection);
    connection
        .execute_batch(
            "ALTER TABLE key_binding RENAME COLUMN command TO action_id;
             DROP TABLE workspace_layout_state;
             ALTER TABLE appearance DROP COLUMN layout_id;
             DROP TABLE conversation_route;
             CREATE TABLE conversation_route (
               runtime_id TEXT NOT NULL,
               conversation_id TEXT NOT NULL,
               active_terminal_id TEXT,
               dock_layout TEXT CHECK (dock_layout IS NULL OR json_valid(dock_layout)),
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
               PRIMARY KEY (runtime_id, conversation_id)
             ) WITHOUT ROWID;
             CREATE TABLE conversation_route_path (
               runtime_id TEXT NOT NULL,
               conversation_id TEXT NOT NULL,
               kind TEXT NOT NULL
                 CHECK (kind IN ('expanded', 'pinned_root', 'collapsed_root', 'git_tree_collapsed')),
               path TEXT NOT NULL,
               position INTEGER NOT NULL,
               PRIMARY KEY (runtime_id, conversation_id, kind, path)
             ) WITHOUT ROWID;
             DROP TABLE workspace_ui;
             CREATE TABLE workspace_ui (
               id INTEGER PRIMARY KEY CHECK (id = 1),
               active_session INTEGER NOT NULL CHECK (active_session >= 0),
               active_session_id TEXT,
               route TEXT NOT NULL,
               selected_agent_id TEXT,
               sidebar_w REAL,
               sidebar_open INTEGER CHECK (sidebar_open IN (0, 1)),
               sidebar_grouping TEXT NOT NULL,
               sidebar_hide_empty_projects INTEGER NOT NULL
                 CHECK (sidebar_hide_empty_projects IN (0, 1))
             );",
        )
        .unwrap();
}

/// v11 及以前客户端库里还有通知偏好表。
/// v12 及更早没有固定工作树的表。
fn restore_v12_without_pinned_worksets(connection: &Connection) {
    connection
        .execute_batch("DROP TABLE pinned_workset;")
        .unwrap();
}

fn restore_v11_notify_prefs(connection: &Connection) {
    restore_v12_without_pinned_worksets(connection);
    connection
        .execute_batch(
            "CREATE TABLE notify_prefs (
               id INTEGER PRIMARY KEY CHECK (id = 1),
               approval INTEGER NOT NULL CHECK (approval IN (0, 1)),
               input INTEGER NOT NULL CHECK (input IN (0, 1)),
               success INTEGER NOT NULL CHECK (success IN (0, 1)),
               failure INTEGER NOT NULL CHECK (failure IN (0, 1)),
               terminal_bell INTEGER NOT NULL CHECK (terminal_bell IN (0, 1))
             );",
        )
        .unwrap();
}

fn has_table(connection: &Connection, name: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = ?1)",
            [name],
            |row| row.get(0),
        )
        .unwrap()
}

/// 把当前结构的对话界面状态退回 v10 的形状：文件树、Git 页的列还在，路径单独一张表。
fn restore_v10_conversation_route(connection: &Connection) {
    restore_v11_notify_prefs(connection);
    connection
        .execute_batch(
            "DROP TABLE conversation_route;
             CREATE TABLE conversation_route (
               runtime_id TEXT NOT NULL,
               conversation_id TEXT NOT NULL,
               active_terminal_id TEXT,
               dock_layout TEXT CHECK (dock_layout IS NULL OR json_valid(dock_layout)),
               file_tree_selected TEXT,
               open_file_path TEXT,
               file_tree_w REAL NOT NULL,
               file_tree_open INTEGER NOT NULL CHECK (file_tree_open IN (0, 1)),
               git_tab TEXT NOT NULL,
               diff_split INTEGER NOT NULL CHECK (diff_split IN (0, 1)),
               diff_scope TEXT NOT NULL,
               PRIMARY KEY (runtime_id, conversation_id)
             ) WITHOUT ROWID;
             CREATE TABLE conversation_route_path (
               runtime_id TEXT NOT NULL,
               conversation_id TEXT NOT NULL,
               kind TEXT NOT NULL
                 CHECK (kind IN ('expanded', 'pinned_root', 'collapsed_root', 'git_tree_collapsed')),
               path TEXT NOT NULL,
               position INTEGER NOT NULL,
               PRIMARY KEY (runtime_id, conversation_id, kind, path)
             ) WITHOUT ROWID;",
        )
        .unwrap();
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
        layout_id: None,
    }
}

fn workspace_ui() -> WorkspaceUiRecord {
    WorkspaceUiRecord {
        active_session: 2,
        active_session_id: Some("s-1".to_string()),
        route: "session".to_string(),
        selected_agent_id: None,
        collapsed_projects: vec![ProjectKey::local("/repo/b"), ProjectKey::local("/repo/a")],
        collapsed_agents: vec!["writer".to_string()],
        sidebar_grouping: "project".to_string(),
        pinned_projects: vec![ProjectKey::local("/repo/z"), ProjectKey::local("/repo/a")],
        sidebar_hide_empty_projects: true,
        archive_expanded: false,
    }
}

#[test]
fn archive_expansion_round_trips() {
    let (store, path) = temp_store();
    let mut record = workspace_ui();
    record.archive_expanded = true;
    store.put_workspace_ui(&record).unwrap();
    drop(store);
    let store = ClientStore::open(&path).unwrap();
    assert!(store.workspace_ui().unwrap().unwrap().archive_expanded);
    drop(store);
    cleanup(&path);
}

#[test]
fn worktree_pins_are_persistent_and_isolated_by_runtime() {
    let (store, path) = temp_store();
    store.set_workset_pinned(None, "shared-id", true).unwrap();
    store
        .set_workset_pinned(Some("remote"), "shared-id", true)
        .unwrap();
    store.set_workset_pinned(None, "shared-id", false).unwrap();
    drop(store);

    let store = ClientStore::open(&path).unwrap();
    assert!(store.pinned_worksets(None).unwrap().is_empty());
    assert_eq!(
        store.pinned_worksets(Some("remote")).unwrap(),
        vec!["shared-id"]
    );
    drop(store);
    cleanup(&path);
}

#[test]
fn v8_migrates_worktree_pins_without_losing_workspace_state() {
    let path = temp_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let v8 = Connection::open(&path).unwrap();
    create_v7_schema(&v8);
    v8.execute_batch(
        "ALTER TABLE workspace_ui ADD COLUMN archive_expanded INTEGER NOT NULL DEFAULT 0;
         INSERT INTO workspace_ui VALUES
           (1, 2, 's-1', 'session', NULL, 320.0, 1, 'project', 1, 1);
         PRAGMA user_version = 8;",
    )
    .unwrap();
    drop(v8);
    let store = ClientStore::open(&path).unwrap();
    assert!(store.workspace_ui().unwrap().unwrap().archive_expanded);
    assert!(store.pinned_worksets(None).unwrap().is_empty());
    store.set_workset_pinned(None, "task", true).unwrap();
    drop(store);
    let store = ClientStore::open(&path).unwrap();
    assert_eq!(store.pinned_worksets(None).unwrap(), vec!["task"]);
    drop(store);
    cleanup(&path);
}

#[test]
fn v12_gains_worktree_pins_without_losing_layout() {
    let (store, path) = temp_store();
    drop(store);
    let v12 = Connection::open(&path).unwrap();
    restore_v12_without_pinned_worksets(&v12);
    v12.execute_batch(
        "INSERT INTO workspace_layout_state VALUES ('default', '{\"version\":1}');
         PRAGMA user_version = 12;",
    )
    .unwrap();
    drop(v12);
    let store = ClientStore::open(&path).unwrap();
    assert!(store.pinned_worksets(None).unwrap().is_empty());
    assert!(store.workspace_layout_state("default").unwrap().is_some());
    store.set_workset_pinned(None, "task", true).unwrap();
    assert_eq!(store.pinned_worksets(None).unwrap(), vec!["task"]);
    drop(store);
    cleanup(&path);
}

#[test]
fn v7_gains_a_collapsed_archive_section_and_preserves_workspace_state() {
    let path = temp_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let v7 = Connection::open(&path).unwrap();
    create_v7_schema(&v7);
    v7.execute_batch(
        "INSERT INTO workspace_ui VALUES
           (1, 2, 's-1', 'session', NULL, 320.0, 1, 'project', 1);
         PRAGMA user_version = 7;",
    )
    .unwrap();
    drop(v7);

    let store = ClientStore::open(&path).unwrap();
    let migrated = store.workspace_ui().unwrap().unwrap();
    let expected = WorkspaceUiRecord {
        collapsed_projects: Vec::new(),
        collapsed_agents: Vec::new(),
        pinned_projects: Vec::new(),
        ..workspace_ui()
    };
    assert_eq!(migrated, expected);
    assert!(!migrated.archive_expanded);
    drop(store);

    let store = ClientStore::open(&path).unwrap();
    assert_eq!(store.workspace_ui().unwrap(), Some(expected));
    drop(store);
    let backup = Connection::open(path.with_file_name(format!("{DATABASE_FILE_NAME}.bak-v7")))
        .expect("迁移前必须留下备份");
    let backup_version: u32 = backup
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(backup_version, 7);
    drop(backup);
    cleanup(&path);
}

/// v8 的三栏宽度、开合和每段对话的工具面板状态升到 v9 时丢掉，别的界面状态和外观留着；
/// 外观里还没选过布局，布局存档表是空的。
#[test]
fn v8_drops_the_shell_columns_and_gains_an_empty_layout_state_table() {
    let path = temp_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let v8 = Connection::open(&path).unwrap();
    create_v7_schema(&v8);
    v8.execute_batch(
        "ALTER TABLE workspace_ui ADD COLUMN archive_expanded INTEGER NOT NULL DEFAULT 0
           CHECK (archive_expanded IN (0, 1));
         INSERT INTO workspace_ui VALUES
           (1, 2, 's-1', 'session', NULL, 320.0, 1, 'project', 1, 1);
         INSERT INTO conversation_route VALUES
           ('', 'conv-1', 'term-2', NULL, 'inspector', 'git', 1, 420.0, '/repo/a.rs', NULL,
            240.0, 0, 'log', 1, 'unstaged');
         INSERT INTO appearance VALUES
           (1, 1973790, '/Users/me/bg.png', 0.25, 0.95, 1, 'regular', 'dark', 'Default Dark',
            16, '', 13, 'JetBrains Mono');
         PRAGMA user_version = 8;",
    )
    .unwrap();
    drop(v8);

    let store = ClientStore::open(&path).unwrap();
    assert_eq!(
        store.workspace_ui().unwrap(),
        Some(WorkspaceUiRecord {
            collapsed_projects: Vec::new(),
            collapsed_agents: Vec::new(),
            pinned_projects: Vec::new(),
            archive_expanded: true,
            ..workspace_ui()
        })
    );
    let routes = store.conversation_routes().unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].active_terminal_id.as_deref(), Some("term-2"));
    assert_eq!(store.appearance().unwrap(), Some(appearance()));
    assert_eq!(store.workspace_layout_state("default").unwrap(), None);
    drop(store);
    assert!(
        path.with_file_name(format!("{DATABASE_FILE_NAME}.bak-v8"))
            .exists(),
        "迁移前必须留下备份"
    );
    cleanup(&path);
}

/// 布局存档按布局 id 一份，整份覆盖；不是 JSON 的写不进去；删掉就回到没改过。
#[test]
fn layout_states_are_kept_per_layout() {
    let (store, path) = temp_store();
    let record = |layout_id: &str, state: &str| WorkspaceLayoutStateRecord {
        layout_id: layout_id.to_string(),
        state: state.to_string(),
    };
    store
        .put_workspace_layout_state(&record("default", r#"{"version":1}"#))
        .unwrap();
    store
        .put_workspace_layout_state(&record("stocks", r#"{"version":1,"center":{}}"#))
        .unwrap();
    store
        .put_workspace_layout_state(&record("default", r#"{"version":2}"#))
        .unwrap();
    assert!(
        store
            .put_workspace_layout_state(&record("default", "{not json"))
            .is_err()
    );
    assert_eq!(
        store.workspace_layout_state("default").unwrap(),
        Some(record("default", r#"{"version":2}"#))
    );
    store.delete_workspace_layout_state("default").unwrap();
    assert_eq!(store.workspace_layout_state("default").unwrap(), None);
    assert!(store.workspace_layout_state("stocks").unwrap().is_some());

    // 选的是哪份布局记在外观里。
    let chosen = AppearanceRecord {
        layout_id: Some("stocks".to_string()),
        ..appearance()
    };
    store.put_appearance(&chosen).unwrap();
    assert_eq!(store.appearance().unwrap(), Some(chosen));
    cleanup(&path);
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
    create_v7_schema(&v2);
    v2.execute_batch(
        "DROP TABLE sidebar_project_order;
         DROP TABLE sidebar_conversation_order;
         DROP TABLE key_binding;
         DROP TABLE conversation_route;
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
         -- v2 起折叠、置顶就在独立表里（本机运行时的 runtime_id 是空串）。v3 只换会话路由，这些表原样留下。
         INSERT INTO collapsed_project VALUES ('', '/repo/b', 0), ('', '/repo/a', 1);
         INSERT INTO collapsed_agent VALUES ('writer', 0);
         INSERT INTO pinned_project VALUES ('', '/repo/z', 0), ('', '/repo/a', 1);
         ALTER TABLE runtime_connection DROP COLUMN os;
         ALTER TABLE runtime_connection DROP COLUMN smelt_home;
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
        smelt_home: None,
        os: None,
    };
    let pi = RuntimeConnection {
        runtime_id: "rt-pi".to_string(),
        name: "pi".to_string(),
        endpoint_id: "bb".to_string(),
        relay_url: Some("https://relay.example".to_string()),
        added_at_ms: 2,
        smelt_home: Some("/home/pi/.smelt".to_string()),
        os: Some("linux".to_string()),
    };
    store.put_runtime_connection(&linux).unwrap();
    store.put_runtime_connection(&pi).unwrap();
    store
        .put_runtime_connection(&RuntimeConnection {
            endpoint_id: "cc".to_string(),
            added_at_ms: 3,
            smelt_home: Some("/home/linux/.smelt".to_string()),
            os: Some("linux".to_string()),
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
    assert_eq!(listed[0].smelt_home.as_deref(), Some("/home/linux/.smelt"));
    assert_eq!(listed[0].os.as_deref(), Some("linux"));
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
    assert!(store.runtime_migrated().unwrap());
    drop(store);

    let connection = Connection::open(&path).unwrap();
    assert!(
        !has_table(&connection, "notify_prefs"),
        "通知偏好不搬，归通知插件"
    );
    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, schema::SCHEMA_VERSION);
    assert!(!has_table(&connection, "client_pref"));
    drop(connection);
    cleanup(&path);
}

/// v1 的折叠、置顶不分运行时，升上来都算本机的。
#[test]
fn v1_projects_become_local_projects() {
    let path = temp_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let v1 = Connection::open(&path).unwrap();
    create_v7_schema(&v1);
    v1.execute_batch(
        "DROP TABLE sidebar_project_order;
         DROP TABLE sidebar_conversation_order;
         DROP TABLE key_binding;
         DROP TABLE collapsed_project;
         DROP TABLE pinned_project;
         DROP TABLE conversation_route;
         DROP TABLE conversation_route_path;
         CREATE TABLE collapsed_project (root TEXT PRIMARY KEY, position INTEGER NOT NULL) WITHOUT ROWID;
         CREATE TABLE pinned_project (root TEXT PRIMARY KEY, position INTEGER NOT NULL) WITHOUT ROWID;
         INSERT INTO workspace_ui VALUES (1, 2, 's-1', 'session', NULL, 320.0, 1, 'project', 1);
         INSERT INTO collapsed_project VALUES ('/repo/b', 0), ('/repo/a', 1);
         INSERT INTO collapsed_agent VALUES ('writer', 0);
         INSERT INTO pinned_project VALUES ('/repo/z', 0), ('/repo/a', 1);
         ALTER TABLE runtime_connection DROP COLUMN os;
         ALTER TABLE runtime_connection DROP COLUMN smelt_home;
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

/// v3 的远程运行时还没有 SMELT_HOME：升到 v4 时原样留下，SMELT_HOME 先空着，连上一次再记。
#[test]
fn v3_runtime_connections_gain_an_empty_smelt_home() {
    let path = temp_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let v3 = Connection::open(&path).unwrap();
    create_v7_schema(&v3);
    v3.execute_batch(
        "DROP TABLE sidebar_project_order;
         DROP TABLE sidebar_conversation_order;
         DROP TABLE key_binding;
         ALTER TABLE runtime_connection DROP COLUMN os;
         ALTER TABLE runtime_connection DROP COLUMN smelt_home;
         INSERT INTO runtime_connection VALUES ('rt-linux', 'linux-box', 'aa', NULL, 0, 1);
         PRAGMA user_version = 3;",
    )
    .unwrap();
    drop(v3);

    let store = ClientStore::open(&path).unwrap();
    let listed = store.runtime_connections().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "linux-box");
    assert_eq!(listed[0].smelt_home, None);
    store
        .put_runtime_connection(&RuntimeConnection {
            smelt_home: Some("/home/linux/.smelt".to_string()),
            ..listed[0].clone()
        })
        .unwrap();
    assert_eq!(
        store.runtime_connections().unwrap()[0]
            .smelt_home
            .as_deref(),
        Some("/home/linux/.smelt")
    );
    cleanup(&path);
}

/// v4 的远程运行时还不知道自己在什么系统上：升到 v5 时原样留下，`os` 先空着，连上一次再记。
#[test]
fn v4_runtime_connections_gain_an_empty_os() {
    let path = temp_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let v4 = Connection::open(&path).unwrap();
    create_v7_schema(&v4);
    v4.execute_batch(
        "DROP TABLE sidebar_project_order;
         DROP TABLE sidebar_conversation_order;
         DROP TABLE key_binding;
         ALTER TABLE runtime_connection DROP COLUMN os;
         INSERT INTO runtime_connection
           VALUES ('rt-win', 'DESKTOP-1', 'aa', NULL, 0, 1, 'C:\\Users\\w\\.smelt');
         PRAGMA user_version = 4;",
    )
    .unwrap();
    drop(v4);

    let store = ClientStore::open(&path).unwrap();
    let listed = store.runtime_connections().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].smelt_home.as_deref(),
        Some("C:\\Users\\w\\.smelt")
    );
    assert_eq!(listed[0].os, None);
    store
        .put_runtime_connection(&RuntimeConnection {
            os: Some("windows".to_string()),
            ..listed[0].clone()
        })
        .unwrap();
    assert_eq!(
        store.runtime_connections().unwrap()[0].os.as_deref(),
        Some("windows")
    );
    cleanup(&path);
}

fn sidebar_order(
    runtime_id: Option<&str>,
    projects: &[&str],
    conversations: &[&str],
) -> SidebarOrderRecord {
    SidebarOrderRecord {
        runtime_id: runtime_id.map(str::to_string),
        projects: projects.iter().map(|item| item.to_string()).collect(),
        conversations: conversations.iter().map(|item| item.to_string()).collect(),
    }
}

/// 侧栏顺序按运行时存取：本机（`None`）和远程各记各的，读回来本机在最前。
#[test]
fn sidebar_orders_round_trip() {
    let (store, path) = temp_store();
    assert!(store.sidebar_orders().unwrap().is_empty());

    let remote = sidebar_order(Some("rt-linux"), &["/home/a", "/home/b"], &["conv-9"]);
    let local = sidebar_order(
        None,
        &["/repo/z", "/repo/a"],
        &["conv-2", "conv-1", "conv-3"],
    );
    store
        .put_sidebar_orders(&[remote.clone(), local.clone()])
        .unwrap();
    assert_eq!(store.sidebar_orders().unwrap(), vec![local, remote]);

    // 只有对话、没有项目的运行时也读得回来。
    let conversations_only = sidebar_order(Some("rt-win"), &[], &["conv-5"]);
    store
        .put_sidebar_orders(std::slice::from_ref(&conversations_only))
        .unwrap();
    assert!(
        store
            .sidebar_orders()
            .unwrap()
            .contains(&conversations_only)
    );
    cleanup(&path);
}

/// 只换给出的运行时：别的运行时的顺序原样留着；给出空列表就清掉这个运行时的顺序。
#[test]
fn putting_one_runtime_keeps_the_others() {
    let (store, path) = temp_store();
    let local = sidebar_order(None, &["/repo/a", "/repo/b"], &["conv-1", "conv-2"]);
    let remote = sidebar_order(Some("rt-linux"), &["/home/a"], &["conv-9"]);
    store.put_sidebar_orders(&[local, remote.clone()]).unwrap();

    let reordered = sidebar_order(None, &["/repo/b", "/repo/c"], &["conv-2"]);
    store
        .put_sidebar_orders(std::slice::from_ref(&reordered))
        .unwrap();
    assert_eq!(
        store.sidebar_orders().unwrap(),
        vec![reordered, remote.clone()]
    );

    store
        .put_sidebar_orders(&[sidebar_order(None, &[], &[])])
        .unwrap();
    assert_eq!(store.sidebar_orders().unwrap(), vec![remote.clone()]);

    // 什么都不给就什么都不动。
    store.put_sidebar_orders(&[]).unwrap();
    assert_eq!(store.sidebar_orders().unwrap(), vec![remote]);
    cleanup(&path);
}

/// 同一运行时里重复的条目只留第一次出现的位置。
#[test]
fn duplicate_sidebar_entries_keep_the_first() {
    let (store, path) = temp_store();
    store
        .put_sidebar_orders(&[sidebar_order(
            None,
            &["/repo/a", "/repo/b", "/repo/a"],
            &["conv-2", "conv-1", "conv-2", "conv-1"],
        )])
        .unwrap();
    assert_eq!(
        store.sidebar_orders().unwrap(),
        vec![sidebar_order(
            None,
            &["/repo/a", "/repo/b"],
            &["conv-2", "conv-1"]
        )]
    );
    cleanup(&path);
}

/// v5 还没有侧栏顺序：升到 v6 时建两张空表，别的数据原样留下，迁移前留了备份。
#[test]
fn v5_gains_empty_sidebar_order_tables() {
    let path = temp_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let v5 = Connection::open(&path).unwrap();
    create_v7_schema(&v5);
    v5.execute_batch(
        "DROP TABLE sidebar_project_order;
         DROP TABLE sidebar_conversation_order;
         DROP TABLE key_binding;
         INSERT INTO workspace_ui VALUES (1, 2, 's-1', 'session', NULL, 320.0, 1, 'project', 1);
         INSERT INTO collapsed_project VALUES ('', '/repo/b', 0), ('', '/repo/a', 1);
         INSERT INTO collapsed_agent VALUES ('writer', 0);
         INSERT INTO pinned_project VALUES ('', '/repo/z', 0), ('', '/repo/a', 1);
         INSERT INTO runtime_connection
           VALUES ('rt-win', 'DESKTOP-1', 'aa', NULL, 0, 1, NULL, 'windows');
         PRAGMA user_version = 5;",
    )
    .unwrap();
    drop(v5);

    let store = ClientStore::open(&path).unwrap();
    assert!(store.sidebar_orders().unwrap().is_empty());
    assert_eq!(store.workspace_ui().unwrap(), Some(workspace_ui()));
    assert_eq!(
        store.runtime_connections().unwrap()[0].os.as_deref(),
        Some("windows")
    );
    let order = sidebar_order(Some("rt-win"), &["/repo"], &["conv-1"]);
    store
        .put_sidebar_orders(std::slice::from_ref(&order))
        .unwrap();
    assert_eq!(store.sidebar_orders().unwrap(), vec![order]);
    drop(store);

    let version: u32 = Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, schema::SCHEMA_VERSION);
    assert!(
        path.with_file_name(format!("{DATABASE_FILE_NAME}.bak-v5"))
            .exists(),
        "迁移前必须留下备份"
    );
    cleanup(&path);
}

#[test]
fn key_bindings_override_and_reset_per_command() {
    let (store, path) = temp_store();
    assert!(store.key_bindings().unwrap().is_empty());

    let palette = KeyBindingRecord {
        command: "smelt.palette/toggle".to_string(),
        keystroke: Some("cmd-j".to_string()),
    };
    let sidebar = KeyBindingRecord {
        command: "smelt.sessions/focus-list".to_string(),
        keystroke: None,
    };
    store
        .put_key_bindings(&[palette.clone(), sidebar.clone()])
        .unwrap();
    assert_eq!(
        store.key_bindings().unwrap(),
        vec![palette.clone(), sidebar.clone()]
    );

    let moved = KeyBindingRecord {
        keystroke: Some("cmd-shift-p".to_string()),
        ..palette
    };
    store
        .put_key_bindings(std::slice::from_ref(&moved))
        .unwrap();
    store
        .delete_key_binding("smelt.sessions/focus-list")
        .unwrap();
    assert_eq!(store.key_bindings().unwrap(), vec![moved]);
    cleanup(&path);
}

/// v9 的快捷键按写死的动作名存：升到 v10 时换成命令 id，早已下线的动作名删掉，清掉的键
/// （`NULL`）照样留着；迁移前留了备份。
#[test]
fn v9_key_bindings_move_to_command_ids() {
    let (store, path) = temp_store();
    drop(store);
    // v9 比 v10 只差快捷键表这一列的名字；对话界面状态先退回 v10 的形状。
    let v9 = Connection::open(&path).unwrap();
    restore_v10_conversation_route(&v9);
    v9.execute_batch(
        "ALTER TABLE key_binding RENAME COLUMN command TO action_id;
         INSERT INTO key_binding VALUES
           ('command_palette', 'cmd-j'), ('next_hunk', NULL), ('split_beside', 'cmd-e'),
           ('toggle_sidebar', 'cmd-b');
         PRAGMA user_version = 9;",
    )
    .unwrap();
    drop(v9);

    let store = ClientStore::open(&path).unwrap();
    let record = |command: &str, keystroke: Option<&str>| KeyBindingRecord {
        command: command.to_string(),
        keystroke: keystroke.map(str::to_string),
    };
    assert_eq!(
        store.key_bindings().unwrap(),
        vec![
            record("smelt.git/next-hunk", None),
            record("smelt.palette/toggle", Some("cmd-j")),
            record("smelt.stage/split-right", Some("cmd-e")),
        ]
    );
    drop(store);
    assert!(
        path.with_file_name(format!("{DATABASE_FILE_NAME}.bak-v9"))
            .exists(),
        "迁移前必须留下备份"
    );
    cleanup(&path);
}

/// v10 的对话界面状态带着文件树、Git 页的状态：升到 v11 时这些列和路径表删掉，终端的
/// 分屏和当前终端留着；迁移前留了备份。
#[test]
fn v10_conversation_routes_keep_only_terminal_state() {
    let (store, path) = temp_store();
    drop(store);
    let v10 = Connection::open(&path).unwrap();
    restore_v10_conversation_route(&v10);
    v10.execute_batch(
        "INSERT INTO conversation_route VALUES
           ('', 'conv-1', 'term-2', '{\"v\":1}', '/repo/a.rs', '/repo/a.rs', 240.0, 1,
            'log', 1, 'unstaged');
         INSERT INTO conversation_route_path VALUES ('', 'conv-1', 'expanded', '/repo/src', 0);
         PRAGMA user_version = 10;",
    )
    .unwrap();
    drop(v10);

    let store = ClientStore::open(&path).unwrap();
    assert_eq!(
        store.conversation_routes().unwrap(),
        vec![ConversationRouteRecord {
            runtime_id: None,
            conversation_id: "conv-1".to_string(),
            active_terminal_id: Some("term-2".to_string()),
            dock_layout: Some(r#"{"v":1}"#.to_string()),
        }]
    );
    drop(store);
    let connection = Connection::open(&path).unwrap();
    let path_tables: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'conversation_route_path'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(path_tables, 0);
    drop(connection);
    assert!(
        path.with_file_name(format!("{DATABASE_FILE_NAME}.bak-v10"))
            .exists(),
        "迁移前必须留下备份"
    );
    cleanup(&path);
}

/// v11 的通知偏好归了通知插件：升到 v12 时这张表删掉，别的数据原样留下；迁移前留了备份。
#[test]
fn v11_drops_notify_prefs() {
    let (store, path) = temp_store();
    store.put_appearance(&appearance()).unwrap();
    drop(store);
    let v11 = Connection::open(&path).unwrap();
    restore_v11_notify_prefs(&v11);
    v11.execute_batch(
        "INSERT INTO notify_prefs VALUES (1, 1, 0, 1, 1, 0);
         PRAGMA user_version = 11;",
    )
    .unwrap();
    drop(v11);

    let store = ClientStore::open(&path).unwrap();
    assert_eq!(store.appearance().unwrap(), Some(appearance()));
    drop(store);
    let connection = Connection::open(&path).unwrap();
    assert!(!has_table(&connection, "notify_prefs"));
    drop(connection);
    assert!(
        path.with_file_name(format!("{DATABASE_FILE_NAME}.bak-v11"))
            .exists(),
        "迁移前必须留下备份"
    );
    cleanup(&path);
}

/// v6 还没有快捷键表：升到 v7 时建空表，别的数据原样留下，迁移前留了备份。
#[test]
fn v6_gains_an_empty_key_binding_table() {
    let path = temp_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let v6 = Connection::open(&path).unwrap();
    create_v7_schema(&v6);
    v6.execute_batch(
        "DROP TABLE key_binding;
         INSERT INTO workspace_ui VALUES (1, 2, 's-1', 'session', NULL, 320.0, 1, 'project', 1);
         INSERT INTO collapsed_project VALUES ('', '/repo/b', 0), ('', '/repo/a', 1);
         INSERT INTO collapsed_agent VALUES ('writer', 0);
         INSERT INTO pinned_project VALUES ('', '/repo/z', 0), ('', '/repo/a', 1);
         PRAGMA user_version = 6;",
    )
    .unwrap();
    drop(v6);

    let store = ClientStore::open(&path).unwrap();
    assert!(store.key_bindings().unwrap().is_empty());
    assert_eq!(store.workspace_ui().unwrap(), Some(workspace_ui()));
    drop(store);

    let version: u32 = Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, schema::SCHEMA_VERSION);
    assert!(
        path.with_file_name(format!("{DATABASE_FILE_NAME}.bak-v6"))
            .exists(),
        "迁移前必须留下备份"
    );
    cleanup(&path);
}
