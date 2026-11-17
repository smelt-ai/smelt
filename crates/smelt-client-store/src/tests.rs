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

/// v14 及以前客户端库里还有会话列表、分屏的界面状态和终端字号。外观、界面状态表照
/// v14 的列顺序重建，已有的行搬过去（位置插入的夹具靠这个顺序）。
fn restore_v14_session_ui(connection: &Connection) {
    connection
        .execute_batch(
            "CREATE TABLE appearance_v14 (
               id INTEGER PRIMARY KEY CHECK (id = 1),
               bg_color INTEGER NOT NULL,
               bg_image TEXT,
               bg_image_opacity REAL NOT NULL,
               opacity REAL NOT NULL,
               blur INTEGER NOT NULL CHECK (blur IN (0, 1)),
               glass_style TEXT NOT NULL,
               theme_mode TEXT NOT NULL,
               theme_name TEXT NOT NULL,
               ui_font_px INTEGER NOT NULL,
               ui_font_family TEXT NOT NULL,
               font_px INTEGER NOT NULL,
               font_family TEXT NOT NULL,
               layout_id TEXT
             );
             INSERT INTO appearance_v14 SELECT id, 0, bg_image, bg_image_opacity, 1.0, 1,
               material, theme_mode, theme_name, ui_font_px, ui_font_family, 13, font_family,
               layout_id FROM appearance;
             DROP TABLE appearance;
             ALTER TABLE appearance_v14 RENAME TO appearance;
             CREATE TABLE workspace_ui_v14 (
               id INTEGER PRIMARY KEY CHECK (id = 1),
               active_session INTEGER NOT NULL CHECK (active_session >= 0),
               active_session_id TEXT,
               route TEXT NOT NULL,
               sidebar_grouping TEXT NOT NULL,
               sidebar_hide_empty_projects INTEGER NOT NULL
                 CHECK (sidebar_hide_empty_projects IN (0, 1)),
               archive_expanded INTEGER NOT NULL DEFAULT 0 CHECK (archive_expanded IN (0, 1))
             );
             INSERT INTO workspace_ui_v14 (id, active_session, route, sidebar_grouping,
               sidebar_hide_empty_projects)
               SELECT id, 0, outlet, 'project', 0 FROM workspace_ui;
             DROP TABLE workspace_ui;
             ALTER TABLE workspace_ui_v14 RENAME TO workspace_ui;
             CREATE TABLE collapsed_project (
               runtime_id TEXT NOT NULL,
               root TEXT NOT NULL,
               position INTEGER NOT NULL,
               PRIMARY KEY (runtime_id, root)
             ) WITHOUT ROWID;
             CREATE TABLE collapsed_agent (
               agent_id TEXT PRIMARY KEY,
               position INTEGER NOT NULL
             ) WITHOUT ROWID;
             CREATE TABLE pinned_project (
               runtime_id TEXT NOT NULL,
               root TEXT NOT NULL,
               position INTEGER NOT NULL,
               PRIMARY KEY (runtime_id, root)
             ) WITHOUT ROWID;
             CREATE TABLE pinned_workset (
               runtime_id TEXT NOT NULL,
               workset_id TEXT NOT NULL,
               PRIMARY KEY (runtime_id, workset_id)
             ) WITHOUT ROWID;
             CREATE TABLE conversation_route (
               runtime_id TEXT NOT NULL,
               conversation_id TEXT NOT NULL,
               active_terminal_id TEXT,
               dock_layout TEXT CHECK (dock_layout IS NULL OR json_valid(dock_layout)),
               PRIMARY KEY (runtime_id, conversation_id)
             ) WITHOUT ROWID;
             CREATE TABLE sidebar_project_order (
               runtime_id TEXT NOT NULL,
               root TEXT NOT NULL,
               position INTEGER NOT NULL,
               PRIMARY KEY (runtime_id, root)
             ) WITHOUT ROWID;
             CREATE TABLE sidebar_conversation_order (
               runtime_id TEXT NOT NULL,
               conversation_id TEXT NOT NULL,
               position INTEGER NOT NULL,
               PRIMARY KEY (runtime_id, conversation_id)
             ) WITHOUT ROWID;",
        )
        .unwrap();
}

/// v13 的界面状态表还有选中的智能体。
fn restore_v13_selected_agent(connection: &Connection) {
    restore_v14_session_ui(connection);
    connection
        .execute_batch("ALTER TABLE workspace_ui ADD COLUMN selected_agent_id TEXT;")
        .unwrap();
}

/// v12 及更早没有固定工作树的表。
fn restore_v12_without_pinned_worksets(connection: &Connection) {
    restore_v13_selected_agent(connection);
    connection
        .execute_batch("DROP TABLE pinned_workset;")
        .unwrap();
}

/// v11 及以前客户端库里还有通知偏好表。
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

fn columns(connection: &Connection, table: &str) -> Vec<String> {
    let mut statement = connection
        .prepare("SELECT name FROM pragma_table_info(?1)")
        .unwrap();
    statement
        .query_map([table], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
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
        bg_image: Some("/Users/me/bg.png".to_string()),
        bg_image_opacity: 0.25,
        theme_mode: "dark".to_string(),
        theme_name: "Default Dark".to_string(),
        ui_font_px: 16,
        ui_font_family: String::new(),
        font_family: "JetBrains Mono".to_string(),
        layout_id: None,
        material: "hud".to_string(),
    }
}

fn workspace_ui() -> WorkspaceUiRecord {
    WorkspaceUiRecord {
        outlet: "session".to_string(),
    }
}

/// v13 → v14：选中的智能体那一列删掉，别的界面状态留着；改过的「新对话」快捷键跟着命令
/// 搬到智能体插件的 id 上。
#[test]
fn v13_drops_the_selected_agent_and_moves_the_new_conversation_key() {
    let (store, path) = temp_store();
    drop(store);
    let v13 = Connection::open(&path).unwrap();
    restore_v13_selected_agent(&v13);
    v13.execute_batch(
        "INSERT INTO workspace_ui
           (id, active_session, active_session_id, route, sidebar_grouping,
            sidebar_hide_empty_projects, archive_expanded, selected_agent_id)
           VALUES (1, 2, 's-1', 'agents', 'project', 1, 0, 'writer');
         INSERT INTO key_binding VALUES ('smelt.sessions/new-conversation', 'cmd-shift-m');
         INSERT INTO key_binding VALUES ('smelt.palette/toggle', 'cmd-j');
         PRAGMA user_version = 13;",
    )
    .unwrap();
    drop(v13);
    let store = ClientStore::open(&path).unwrap();
    assert_eq!(store.workspace_ui().unwrap().unwrap().outlet, "agents");
    assert_eq!(
        store.key_bindings().unwrap(),
        vec![
            KeyBindingRecord {
                command: "smelt.agents/new-conversation".to_string(),
                keystroke: Some("cmd-shift-m".to_string()),
            },
            KeyBindingRecord {
                command: "smelt.palette/toggle".to_string(),
                keystroke: Some("cmd-j".to_string()),
            },
        ]
    );
    drop(store);
    cleanup(&path);
}

/// v14 → v15：会话列表、分屏的界面状态和终端字号归了插件，那些表和列删掉；外观里没人读的
/// 三列删掉、`glass_style` 改名 `material`，路由列改名 `outlet`；值和布局存档留着。分屏的
/// 五条快捷键搬到会话插件的同名命令上（新 id 已有记录的以新的为准），别的 `smelt.stage/*`
/// 和按侧栏顺序跳的键删掉。迁移前留了备份。
#[test]
fn v14_drops_the_session_ui_and_moves_the_pane_keys() {
    let (store, path) = temp_store();
    store.put_appearance(&appearance()).unwrap();
    drop(store);
    let v14 = Connection::open(&path).unwrap();
    restore_v14_session_ui(&v14);
    v14.execute_batch(
        "INSERT INTO workspace_ui VALUES (1, 2, 's-1', 'smelt.archive/main', 'agent', 1, 1);
         UPDATE appearance SET font_px = 17;
         INSERT INTO workspace_layout_state VALUES ('default', '{\"version\":1}');
         INSERT INTO collapsed_project VALUES ('', '/repo/b', 0);
         INSERT INTO collapsed_agent VALUES ('writer', 0);
         INSERT INTO pinned_project VALUES ('', '/repo/z', 0);
         INSERT INTO pinned_workset VALUES ('rt-linux', 'task');
         INSERT INTO conversation_route VALUES ('', 'conv-1', 'term-2', '{\"v\":1}');
         INSERT INTO sidebar_project_order VALUES ('', '/repo/a', 0);
         INSERT INTO sidebar_conversation_order VALUES ('', 'conv-1', 0);
         INSERT INTO key_binding VALUES
           ('smelt.stage/prev-pane', 'cmd-shift-['),
           ('smelt.stage/next-pane', NULL),
           ('smelt.stage/split-right', 'cmd-e'),
           ('smelt.stage/split-down', 'cmd-shift-e'),
           ('smelt.stage/close-pane', 'cmd-shift-w'),
           ('smelt.sessions/close-pane', 'cmd-alt-w'),
           ('smelt.stage/cover', 'cmd-k'),
           ('smelt.sessions/activate-1', 'ctrl-1'),
           ('smelt.sessions/focus-list', 'cmd-0'),
           ('smelt.palette/toggle', 'cmd-j');
         PRAGMA user_version = 14;",
    )
    .unwrap();
    drop(v14);

    let store = ClientStore::open(&path).unwrap();
    assert_eq!(
        store.workspace_ui().unwrap(),
        Some(WorkspaceUiRecord {
            outlet: "smelt.archive/main".to_string(),
        })
    );
    assert_eq!(store.appearance().unwrap(), Some(appearance()));
    assert!(store.workspace_layout_state("default").unwrap().is_some());
    let record = |command: &str, keystroke: Option<&str>| KeyBindingRecord {
        command: command.to_string(),
        keystroke: keystroke.map(str::to_string),
    };
    assert_eq!(
        store.key_bindings().unwrap(),
        vec![
            record("smelt.palette/toggle", Some("cmd-j")),
            record("smelt.sessions/close-pane", Some("cmd-alt-w")),
            record("smelt.sessions/focus-list", Some("cmd-0")),
            record("smelt.sessions/next-pane", None),
            record("smelt.sessions/prev-pane", Some("cmd-shift-[")),
            record("smelt.sessions/split-down", Some("cmd-shift-e")),
            record("smelt.sessions/split-right", Some("cmd-e")),
        ]
    );
    drop(store);

    let connection = Connection::open(&path).unwrap();
    for table in [
        "pinned_project",
        "pinned_workset",
        "collapsed_project",
        "collapsed_agent",
        "conversation_route",
        "sidebar_project_order",
        "sidebar_conversation_order",
    ] {
        assert!(!has_table(&connection, table), "{table}");
    }
    assert_eq!(columns(&connection, "workspace_ui"), ["id", "outlet"]);
    // 没人读的背景色、不透明度、模糊和终端字号删掉，`glass_style` 改名 `material`（值见上面
    // 外观的断言）。
    assert_eq!(
        columns(&connection, "appearance"),
        [
            "id",
            "bg_image",
            "bg_image_opacity",
            "material",
            "theme_mode",
            "theme_name",
            "ui_font_px",
            "ui_font_family",
            "font_family",
            "layout_id",
        ]
    );
    drop(connection);
    assert!(
        path.with_file_name(format!("{DATABASE_FILE_NAME}.bak-v14"))
            .exists(),
        "迁移前必须留下备份"
    );
    cleanup(&path);
}

#[test]
fn v12_keeps_the_layout_state() {
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
    assert!(store.workspace_layout_state("default").unwrap().is_some());
    drop(store);
    cleanup(&path);
}

#[test]
fn v7_keeps_the_route_and_leaves_a_backup() {
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
    assert_eq!(store.workspace_ui().unwrap(), Some(workspace_ui()));
    drop(store);

    let store = ClientStore::open(&path).unwrap();
    assert_eq!(store.workspace_ui().unwrap(), Some(workspace_ui()));
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

/// v8 的三栏宽度、开合和每段对话的工具面板状态升到 v9 时丢掉，路由和外观留着；外观里
/// 还没选过布局，布局存档表是空的。
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
    assert_eq!(store.workspace_ui().unwrap(), Some(workspace_ui()));
    // 旧库这一列是从来没人读过的 `regular`，原样读出来，界面认不出会回到默认材质。
    assert_eq!(
        store.appearance().unwrap(),
        Some(AppearanceRecord {
            material: "regular".to_string(),
            ..appearance()
        })
    );
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
    let moved = WorkspaceUiRecord {
        outlet: "smelt.archive/main".to_string(),
    };
    store.put_workspace_ui(&moved).unwrap();
    assert_eq!(store.workspace_ui().unwrap(), Some(moved));

    store.mark_runtime_migrated().unwrap();
    store.mark_runtime_migrated().unwrap();
    assert!(store.runtime_migrated().unwrap());
    cleanup(&path);
}

/// v2 的会话路由按活动终端的 id 存，对不上对话的稳定 id，升到 v3 时丢掉；对话界面状态到
/// v15 整张归了会话插件。路由留着，迁移前留了备份。
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
    assert_eq!(store.workspace_ui().unwrap(), Some(workspace_ui()));
    drop(store);
    let connection = Connection::open(&path).unwrap();
    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, schema::SCHEMA_VERSION);
    for table in ["session_route", "session_route_path", "conversation_route"] {
        assert!(!has_table(&connection, table), "{table}");
    }
    drop(connection);

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
    // 旧偏好没有材质，留空由界面回到默认；终端字号归了终端插件，不搬。
    assert_eq!(
        store.appearance().unwrap(),
        Some(AppearanceRecord {
            material: String::new(),
            ..appearance()
        })
    );
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

/// v1 一路迁到当前版本：中间各步都在 v1 的表上跑得通，路由留着。
#[test]
fn v1_migrates_through_to_the_current_version() {
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

/// v5 升上来：远程运行时和路由原样留下，迁移前留了备份。
#[test]
fn v5_keeps_runtime_connections() {
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
    assert_eq!(store.workspace_ui().unwrap(), Some(workspace_ui()));
    assert_eq!(
        store.runtime_connections().unwrap()[0].os.as_deref(),
        Some("windows")
    );
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
/// （`NULL`）照样留着；分屏命令到 v15 跟着搬进会话插件。迁移前留了备份。
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
            record("smelt.sessions/split-right", Some("cmd-e")),
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

/// v10 的对话界面状态带着文件树、Git 页的状态：路径表在 v11 删掉，整张对话界面状态在 v15
/// 归了会话插件；迁移前留了备份。
#[test]
fn v10_conversation_ui_tables_are_gone() {
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

    drop(ClientStore::open(&path).unwrap());
    let connection = Connection::open(&path).unwrap();
    assert!(!has_table(&connection, "conversation_route_path"));
    assert!(!has_table(&connection, "conversation_route"));
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
