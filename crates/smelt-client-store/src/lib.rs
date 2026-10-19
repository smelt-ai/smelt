//! 桌面客户端自己的库：`~/.smelt/client.sqlite3`。
//!
//! 放的是每台设备可以不同、不该交给运行时的东西（设计文档 D1）：外观、更新设置、
//! 通知开关、侧栏等界面状态、远程运行时的连接。运行时的数据在运行时自己的库里，GUI
//! 只经协议读写。每样东西一张表、一个字段一列，表结构见 `schema.rs`。

use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use rusqlite::{Connection, OptionalExtension, params};

mod schema;

pub const DATABASE_FILE_NAME: &str = "client.sqlite3";

#[derive(Debug)]
pub struct ClientStoreError(String);

impl std::fmt::Display for ClientStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ClientStoreError {}

impl From<rusqlite::Error> for ClientStoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<ClientStoreError> for String {
    fn from(error: ClientStoreError) -> Self {
        error.0
    }
}

/// 外观。枚举类字段存的是它们的 serde 名（如 `dark`、`regular`），由界面转换。
#[derive(Clone, Debug, PartialEq)]
pub struct AppearanceRecord {
    pub bg_color: u32,
    pub bg_image: Option<String>,
    pub bg_image_opacity: f32,
    pub opacity: f32,
    pub blur: bool,
    pub glass_style: String,
    pub theme_mode: String,
    pub theme_name: String,
    pub ui_font_px: u32,
    pub ui_font_family: String,
    pub font_px: u32,
    pub font_family: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateSettingsRecord {
    pub channel: String,
    pub auto_install: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotifyPrefsRecord {
    pub approval: bool,
    pub input: bool,
    pub success: bool,
    pub failure: bool,
    pub terminal_bell: bool,
}

/// 侧栏里的一个项目：哪个运行时上的哪个根目录。路径只在一个运行时里唯一。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProjectKey {
    /// `None` 是本机运行时。
    pub runtime_id: Option<String>,
    pub root: String,
}

impl ProjectKey {
    pub fn local(root: impl Into<String>) -> Self {
        Self {
            runtime_id: None,
            root: root.into(),
        }
    }
}

/// 一段对话在这台设备上的界面状态：终端怎么分屏、当前看哪个，舞台覆盖页、工具面板、
/// 文件树、Git 页的开合与选中。枚举类字段存 serde 名，由界面转换。路径列表按原顺序存取。
#[derive(Clone, Debug, PartialEq)]
pub struct ConversationRouteRecord {
    /// `None` 是本机运行时。
    pub runtime_id: Option<String>,
    /// 对话的稳定 id，和运行时工作区目录里的一致。
    pub conversation_id: String,
    /// 当前看的终端（守护里的会话 id）。
    pub active_terminal_id: Option<String>,
    /// 终端的分屏布局。格式归界面定义并带版本号，这里只当一段 JSON 存取。
    pub dock_layout: Option<String>,
    pub stage_cover: Option<String>,
    pub tool_panel_tab: String,
    pub tool_panel_open: bool,
    pub tool_panel_w: f32,
    pub expanded: Vec<String>,
    pub file_tree_selected: Option<String>,
    pub open_file_path: Option<String>,
    pub file_tree_w: f32,
    pub file_tree_open: bool,
    pub pinned_roots: Vec<String>,
    pub collapsed_roots: Vec<String>,
    pub git_tab: String,
    pub diff_split: bool,
    pub git_tree_collapsed: Vec<String>,
    pub diff_scope: String,
}

/// 侧栏、路由这些界面状态。列表按原顺序存取。
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceUiRecord {
    pub active_session: usize,
    /// 当前看的那段对话的稳定 id。
    pub active_session_id: Option<String>,
    pub route: String,
    pub selected_agent_id: Option<String>,
    pub sidebar_w: Option<f32>,
    pub sidebar_open: Option<bool>,
    pub collapsed_projects: Vec<ProjectKey>,
    pub collapsed_agents: Vec<String>,
    pub sidebar_grouping: String,
    pub pinned_projects: Vec<ProjectKey>,
    pub sidebar_hide_empty_projects: bool,
}

/// 在线更新的进度，强杀之后下次启动接着装。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateStateRecord {
    pub current_url: Option<String>,
    pub cleanup_app: Option<PathBuf>,
    pub staged: Option<StagedUpdateRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedUpdateRecord {
    pub update_id: String,
    pub version: String,
    pub url: String,
    pub staged_app: PathBuf,
    pub fingerprint: String,
    pub phase: String,
    pub swap_app: Option<PathBuf>,
    pub installer_attempt: Option<InstallerAttemptRecord>,
    pub installer_failed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallerAttemptRecord {
    pub attempt_id: String,
    pub target_app: PathBuf,
    pub relaunch: bool,
    pub helper_dir: PathBuf,
}

/// 一个远程运行时怎么连（设计文档 D6）。本机运行时总在，不存这里。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeConnection {
    /// 运行时第一次启动时生成、永久不变的身份（`system.describe` 里的 `runtime_id`）。
    pub runtime_id: String,
    /// 侧栏和设置里显示的名字。
    pub name: String,
    /// 运行时的 iroh endpoint id。
    pub endpoint_id: String,
    /// 运行时用了自建 relay 时的地址；`None` 走公共 relay 和地址发现。
    pub relay_url: Option<String>,
    pub added_at_ms: i64,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn path_bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

fn path_from(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

fn sql(error: rusqlite::Error) -> String {
    error.to_string()
}

pub(crate) fn write_appearance(
    connection: &Connection,
    record: &AppearanceRecord,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT OR REPLACE INTO appearance (id, bg_color, bg_image, bg_image_opacity, opacity,
               blur, glass_style, theme_mode, theme_name, ui_font_px, ui_font_family, font_px,
               font_family)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                record.bg_color,
                record.bg_image,
                record.bg_image_opacity,
                record.opacity,
                record.blur,
                record.glass_style,
                record.theme_mode,
                record.theme_name,
                record.ui_font_px,
                record.ui_font_family,
                record.font_px,
                record.font_family
            ],
        )
        .map(drop)
        .map_err(sql)
}

pub(crate) fn write_update_settings(
    connection: &Connection,
    record: &UpdateSettingsRecord,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT OR REPLACE INTO update_settings (id, channel, auto_install) VALUES (1, ?1, ?2)",
            params![record.channel, record.auto_install],
        )
        .map(drop)
        .map_err(sql)
}

pub(crate) fn write_notify_prefs(
    connection: &Connection,
    record: &NotifyPrefsRecord,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT OR REPLACE INTO notify_prefs (id, approval, input, success, failure, terminal_bell)
             VALUES (1, ?1, ?2, ?3, ?4, ?5)",
            params![
                record.approval,
                record.input,
                record.success,
                record.failure,
                record.terminal_bell
            ],
        )
        .map(drop)
        .map_err(sql)
}

fn write_list(
    connection: &Connection,
    table: &str,
    column: &str,
    items: &[String],
) -> Result<(), String> {
    connection
        .execute(&format!("DELETE FROM {table}"), [])
        .map_err(sql)?;
    let mut statement = connection
        .prepare(&format!(
            "INSERT OR IGNORE INTO {table} ({column}, position) VALUES (?1, ?2)"
        ))
        .map_err(sql)?;
    for (position, item) in items.iter().enumerate() {
        statement
            .execute(params![item, position as i64])
            .map_err(sql)?;
    }
    Ok(())
}

fn read_list(connection: &Connection, table: &str, column: &str) -> Result<Vec<String>, String> {
    let mut statement = connection
        .prepare(&format!("SELECT {column} FROM {table} ORDER BY position"))
        .map_err(sql)?;
    let rows = statement.query_map([], |row| row.get(0)).map_err(sql)?;
    rows.collect::<Result<_, _>>().map_err(sql)
}

/// 项目表里本机运行时的 `runtime_id` 存空串：它要进主键，不能是 NULL。
const LOCAL_RUNTIME_ID: &str = "";

fn write_project_list(
    connection: &Connection,
    table: &str,
    items: &[ProjectKey],
) -> Result<(), String> {
    connection
        .execute(&format!("DELETE FROM {table}"), [])
        .map_err(sql)?;
    let mut statement = connection
        .prepare(&format!(
            "INSERT OR IGNORE INTO {table} (runtime_id, root, position) VALUES (?1, ?2, ?3)"
        ))
        .map_err(sql)?;
    for (position, item) in items.iter().enumerate() {
        statement
            .execute(params![
                item.runtime_id.as_deref().unwrap_or(LOCAL_RUNTIME_ID),
                item.root,
                position as i64
            ])
            .map_err(sql)?;
    }
    Ok(())
}

fn read_project_list(connection: &Connection, table: &str) -> Result<Vec<ProjectKey>, String> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT runtime_id, root FROM {table} ORDER BY position"
        ))
        .map_err(sql)?;
    let rows = statement
        .query_map([], |row| {
            let runtime_id: String = row.get(0)?;
            Ok(ProjectKey {
                runtime_id: (runtime_id != LOCAL_RUNTIME_ID).then_some(runtime_id),
                root: row.get(1)?,
            })
        })
        .map_err(sql)?;
    rows.collect::<Result<_, _>>().map_err(sql)
}

/// 对话界面状态里的几类路径列表，在 `conversation_route_path.kind` 里的名字。
const ROUTE_PATH_KINDS: [&str; 4] = [
    "expanded",
    "pinned_root",
    "collapsed_root",
    "git_tree_collapsed",
];

fn route_paths<'a>(record: &'a ConversationRouteRecord, kind: &str) -> &'a [String] {
    match kind {
        "expanded" => &record.expanded,
        "pinned_root" => &record.pinned_roots,
        "collapsed_root" => &record.collapsed_roots,
        _ => &record.git_tree_collapsed,
    }
}

fn route_paths_mut<'a>(record: &'a mut ConversationRouteRecord, kind: &str) -> &'a mut Vec<String> {
    match kind {
        "expanded" => &mut record.expanded,
        "pinned_root" => &mut record.pinned_roots,
        "collapsed_root" => &mut record.collapsed_roots,
        _ => &mut record.git_tree_collapsed,
    }
}

fn write_conversation_routes(
    connection: &Connection,
    records: &[ConversationRouteRecord],
) -> Result<(), String> {
    connection
        .execute_batch("DELETE FROM conversation_route_path; DELETE FROM conversation_route;")
        .map_err(sql)?;
    let mut route = connection
        .prepare(
            "INSERT OR REPLACE INTO conversation_route (runtime_id, conversation_id,
               active_terminal_id, dock_layout, stage_cover, tool_panel_tab, tool_panel_open,
               tool_panel_w, file_tree_selected, open_file_path, file_tree_w, file_tree_open,
               git_tab, diff_split, diff_scope)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        )
        .map_err(sql)?;
    let mut path = connection
        .prepare(
            "INSERT OR IGNORE INTO conversation_route_path
               (runtime_id, conversation_id, kind, path, position)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(sql)?;
    for record in records {
        let runtime_id = record.runtime_id.as_deref().unwrap_or(LOCAL_RUNTIME_ID);
        route
            .execute(params![
                runtime_id,
                record.conversation_id,
                record.active_terminal_id,
                record.dock_layout,
                record.stage_cover,
                record.tool_panel_tab,
                record.tool_panel_open,
                record.tool_panel_w,
                record.file_tree_selected,
                record.open_file_path,
                record.file_tree_w,
                record.file_tree_open,
                record.git_tab,
                record.diff_split,
                record.diff_scope
            ])
            .map_err(sql)?;
        for kind in ROUTE_PATH_KINDS {
            for (position, item) in route_paths(record, kind).iter().enumerate() {
                path.execute(params![
                    runtime_id,
                    record.conversation_id,
                    kind,
                    item,
                    position as i64
                ])
                .map_err(sql)?;
            }
        }
    }
    Ok(())
}

fn read_conversation_routes(
    connection: &Connection,
) -> Result<Vec<ConversationRouteRecord>, String> {
    let mut statement = connection
        .prepare(
            "SELECT runtime_id, conversation_id, active_terminal_id, dock_layout, stage_cover,
               tool_panel_tab, tool_panel_open, tool_panel_w, file_tree_selected, open_file_path,
               file_tree_w, file_tree_open, git_tab, diff_split, diff_scope
             FROM conversation_route ORDER BY runtime_id, conversation_id",
        )
        .map_err(sql)?;
    let mut records = statement
        .query_map([], |row| {
            let runtime_id: String = row.get(0)?;
            Ok(ConversationRouteRecord {
                runtime_id: (runtime_id != LOCAL_RUNTIME_ID).then_some(runtime_id),
                conversation_id: row.get(1)?,
                active_terminal_id: row.get(2)?,
                dock_layout: row.get(3)?,
                stage_cover: row.get(4)?,
                tool_panel_tab: row.get(5)?,
                tool_panel_open: row.get(6)?,
                tool_panel_w: row.get(7)?,
                expanded: Vec::new(),
                file_tree_selected: row.get(8)?,
                open_file_path: row.get(9)?,
                file_tree_w: row.get(10)?,
                file_tree_open: row.get(11)?,
                pinned_roots: Vec::new(),
                collapsed_roots: Vec::new(),
                git_tab: row.get(12)?,
                diff_split: row.get(13)?,
                git_tree_collapsed: Vec::new(),
                diff_scope: row.get(14)?,
            })
        })
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?;
    let mut paths = connection
        .prepare(
            "SELECT runtime_id, conversation_id, kind, path FROM conversation_route_path
             ORDER BY runtime_id, conversation_id, kind, position",
        )
        .map_err(sql)?;
    let rows = paths
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(sql)?;
    for row in rows {
        let (runtime_id, conversation_id, kind, path) = row.map_err(sql)?;
        let runtime_id = (runtime_id != LOCAL_RUNTIME_ID).then_some(runtime_id);
        if let Some(record) = records.iter_mut().find(|record| {
            record.runtime_id == runtime_id && record.conversation_id == conversation_id
        }) {
            route_paths_mut(record, &kind).push(path);
        }
    }
    Ok(records)
}

pub(crate) fn write_workspace_ui(
    connection: &Connection,
    record: &WorkspaceUiRecord,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT OR REPLACE INTO workspace_ui (id, active_session, active_session_id, route,
               selected_agent_id, sidebar_w, sidebar_open, sidebar_grouping,
               sidebar_hide_empty_projects)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                record.active_session as i64,
                record.active_session_id,
                record.route,
                record.selected_agent_id,
                record.sidebar_w,
                record.sidebar_open,
                record.sidebar_grouping,
                record.sidebar_hide_empty_projects
            ],
        )
        .map_err(sql)?;
    write_project_list(connection, "collapsed_project", &record.collapsed_projects)?;
    write_list(
        connection,
        "collapsed_agent",
        "agent_id",
        &record.collapsed_agents,
    )?;
    write_project_list(connection, "pinned_project", &record.pinned_projects)
}

pub(crate) fn write_update_state(
    connection: &Connection,
    record: &UpdateStateRecord,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT OR REPLACE INTO update_state (id, current_url, cleanup_app) VALUES (1, ?1, ?2)",
            params![
                record.current_url,
                record.cleanup_app.as_deref().map(path_bytes)
            ],
        )
        .map_err(sql)?;
    connection
        .execute("DELETE FROM staged_update", [])
        .map_err(sql)?;
    connection
        .execute("DELETE FROM installer_attempt", [])
        .map_err(sql)?;
    let Some(staged) = record.staged.as_ref() else {
        return Ok(());
    };
    connection
        .execute(
            "INSERT INTO staged_update (id, update_id, version, url, staged_app, fingerprint, phase,
               swap_app, installer_failed)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                staged.update_id,
                staged.version,
                staged.url,
                path_bytes(&staged.staged_app),
                staged.fingerprint,
                staged.phase,
                staged.swap_app.as_deref().map(path_bytes),
                staged.installer_failed
            ],
        )
        .map_err(sql)?;
    if let Some(attempt) = staged.installer_attempt.as_ref() {
        connection
            .execute(
                "INSERT INTO installer_attempt (id, attempt_id, target_app, relaunch, helper_dir)
                 VALUES (1, ?1, ?2, ?3, ?4)",
                params![
                    attempt.attempt_id,
                    path_bytes(&attempt.target_app),
                    attempt.relaunch,
                    path_bytes(&attempt.helper_dir)
                ],
            )
            .map_err(sql)?;
    }
    Ok(())
}

pub(crate) fn write_runtime_migrated(connection: &Connection) -> Result<(), String> {
    connection
        .execute(
            "INSERT OR IGNORE INTO runtime_migration (id, migrated_at_ms) VALUES (1, ?1)",
            params![now_ms()],
        )
        .map(drop)
        .map_err(sql)
}

pub struct ClientStore {
    path: PathBuf,
    connection: Mutex<Connection>,
}

impl ClientStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, ClientStoreError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| ClientStoreError(format!("创建 {}：{error}", parent.display())))?;
        }
        let mut connection = Connection::open(&path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        schema::migrate(&mut connection, &path)
            .map_err(|error| ClientStoreError(format!("{}：{error}", path.display())))?;
        Ok(Self {
            path,
            connection: Mutex::new(connection),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[track_caller]
    fn with<T>(
        &self,
        run: impl FnOnce(&mut Connection) -> Result<T, String>,
    ) -> Result<T, ClientStoreError> {
        render_guard::check_store_access();
        let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        run(&mut connection).map_err(ClientStoreError)
    }

    /// 在一个事务里写，几张表要么都改要么都不改。
    #[track_caller]
    fn write(
        &self,
        run: impl FnOnce(&Connection) -> Result<(), String>,
    ) -> Result<(), ClientStoreError> {
        self.with(|connection| {
            let transaction = connection.transaction().map_err(sql)?;
            run(&transaction)?;
            transaction.commit().map_err(sql)
        })
    }

    #[track_caller]
    pub fn appearance(&self) -> Result<Option<AppearanceRecord>, ClientStoreError> {
        self.with(|connection| {
            connection
                .query_row(
                    "SELECT bg_color, bg_image, bg_image_opacity, opacity, blur, glass_style,
                       theme_mode, theme_name, ui_font_px, ui_font_family, font_px, font_family
                     FROM appearance WHERE id = 1",
                    [],
                    |row| {
                        Ok(AppearanceRecord {
                            bg_color: row.get(0)?,
                            bg_image: row.get(1)?,
                            bg_image_opacity: row.get(2)?,
                            opacity: row.get(3)?,
                            blur: row.get(4)?,
                            glass_style: row.get(5)?,
                            theme_mode: row.get(6)?,
                            theme_name: row.get(7)?,
                            ui_font_px: row.get(8)?,
                            ui_font_family: row.get(9)?,
                            font_px: row.get(10)?,
                            font_family: row.get(11)?,
                        })
                    },
                )
                .optional()
                .map_err(sql)
        })
    }

    #[track_caller]
    pub fn put_appearance(&self, record: &AppearanceRecord) -> Result<(), ClientStoreError> {
        self.write(|connection| write_appearance(connection, record))
    }

    #[track_caller]
    pub fn update_settings(&self) -> Result<Option<UpdateSettingsRecord>, ClientStoreError> {
        self.with(|connection| {
            connection
                .query_row(
                    "SELECT channel, auto_install FROM update_settings WHERE id = 1",
                    [],
                    |row| {
                        Ok(UpdateSettingsRecord {
                            channel: row.get(0)?,
                            auto_install: row.get(1)?,
                        })
                    },
                )
                .optional()
                .map_err(sql)
        })
    }

    #[track_caller]
    pub fn put_update_settings(
        &self,
        record: &UpdateSettingsRecord,
    ) -> Result<(), ClientStoreError> {
        self.write(|connection| write_update_settings(connection, record))
    }

    #[track_caller]
    pub fn notify_prefs(&self) -> Result<Option<NotifyPrefsRecord>, ClientStoreError> {
        self.with(|connection| {
            connection
                .query_row(
                    "SELECT approval, input, success, failure, terminal_bell
                     FROM notify_prefs WHERE id = 1",
                    [],
                    |row| {
                        Ok(NotifyPrefsRecord {
                            approval: row.get(0)?,
                            input: row.get(1)?,
                            success: row.get(2)?,
                            failure: row.get(3)?,
                            terminal_bell: row.get(4)?,
                        })
                    },
                )
                .optional()
                .map_err(sql)
        })
    }

    #[track_caller]
    pub fn put_notify_prefs(&self, record: &NotifyPrefsRecord) -> Result<(), ClientStoreError> {
        self.write(|connection| write_notify_prefs(connection, record))
    }

    #[track_caller]
    pub fn workspace_ui(&self) -> Result<Option<WorkspaceUiRecord>, ClientStoreError> {
        self.with(|connection| {
            let Some(mut record) = connection
                .query_row(
                    "SELECT active_session, active_session_id, route, selected_agent_id, sidebar_w,
                       sidebar_open, sidebar_grouping, sidebar_hide_empty_projects
                     FROM workspace_ui WHERE id = 1",
                    [],
                    |row| {
                        Ok(WorkspaceUiRecord {
                            active_session: row.get::<_, i64>(0)? as usize,
                            active_session_id: row.get(1)?,
                            route: row.get(2)?,
                            selected_agent_id: row.get(3)?,
                            sidebar_w: row.get(4)?,
                            sidebar_open: row.get(5)?,
                            collapsed_projects: Vec::new(),
                            collapsed_agents: Vec::new(),
                            sidebar_grouping: row.get(6)?,
                            pinned_projects: Vec::new(),
                            sidebar_hide_empty_projects: row.get(7)?,
                        })
                    },
                )
                .optional()
                .map_err(sql)?
            else {
                return Ok(None);
            };
            record.collapsed_projects = read_project_list(connection, "collapsed_project")?;
            record.collapsed_agents = read_list(connection, "collapsed_agent", "agent_id")?;
            record.pinned_projects = read_project_list(connection, "pinned_project")?;
            Ok(Some(record))
        })
    }

    #[track_caller]
    pub fn put_workspace_ui(&self, record: &WorkspaceUiRecord) -> Result<(), ClientStoreError> {
        self.write(|connection| write_workspace_ui(connection, record))
    }

    #[track_caller]
    pub fn conversation_routes(&self) -> Result<Vec<ConversationRouteRecord>, ClientStoreError> {
        self.with(|connection| read_conversation_routes(connection))
    }

    /// 整份换掉：不在里面的对话（关掉了）的界面状态一并删掉。
    pub fn put_conversation_routes(
        &self,
        records: &[ConversationRouteRecord],
    ) -> Result<(), ClientStoreError> {
        self.write(|connection| write_conversation_routes(connection, records))
    }

    #[track_caller]
    pub fn update_state(&self) -> Result<UpdateStateRecord, ClientStoreError> {
        self.with(|connection| {
            let (current_url, cleanup_app) = connection
                .query_row(
                    "SELECT current_url, cleanup_app FROM update_state WHERE id = 1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<Vec<u8>>>(1)?,
                        ))
                    },
                )
                .optional()
                .map_err(sql)?
                .unwrap_or_default();
            let attempt = connection
                .query_row(
                    "SELECT attempt_id, target_app, relaunch, helper_dir
                     FROM installer_attempt WHERE id = 1",
                    [],
                    |row| {
                        Ok(InstallerAttemptRecord {
                            attempt_id: row.get(0)?,
                            target_app: path_from(row.get(1)?),
                            relaunch: row.get(2)?,
                            helper_dir: path_from(row.get(3)?),
                        })
                    },
                )
                .optional()
                .map_err(sql)?;
            let staged = connection
                .query_row(
                    "SELECT update_id, version, url, staged_app, fingerprint, phase, swap_app,
                       installer_failed
                     FROM staged_update WHERE id = 1",
                    [],
                    |row| {
                        Ok(StagedUpdateRecord {
                            update_id: row.get(0)?,
                            version: row.get(1)?,
                            url: row.get(2)?,
                            staged_app: path_from(row.get(3)?),
                            fingerprint: row.get(4)?,
                            phase: row.get(5)?,
                            swap_app: row.get::<_, Option<Vec<u8>>>(6)?.map(path_from),
                            installer_attempt: None,
                            installer_failed: row.get(7)?,
                        })
                    },
                )
                .optional()
                .map_err(sql)?
                .map(|staged| StagedUpdateRecord {
                    installer_attempt: attempt,
                    ..staged
                });
            Ok(UpdateStateRecord {
                current_url,
                cleanup_app: cleanup_app.map(path_from),
                staged,
            })
        })
    }

    #[track_caller]
    pub fn put_update_state(&self, record: &UpdateStateRecord) -> Result<(), ClientStoreError> {
        self.write(|connection| write_update_state(connection, record))
    }

    /// 旧版本留在运行时库里的客户端数据是否已经取回过（只做一次）。
    #[track_caller]
    pub fn runtime_migrated(&self) -> Result<bool, ClientStoreError> {
        self.with(|connection| {
            connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM runtime_migration WHERE id = 1)",
                    [],
                    |row| row.get(0),
                )
                .map_err(sql)
        })
    }

    #[track_caller]
    pub fn mark_runtime_migrated(&self) -> Result<(), ClientStoreError> {
        self.write(write_runtime_migrated)
    }

    /// 按添加顺序列出远程运行时。
    #[track_caller]
    pub fn runtime_connections(&self) -> Result<Vec<RuntimeConnection>, ClientStoreError> {
        self.with(|connection| {
            let mut statement = connection
                .prepare(
                    "SELECT runtime_id, name, endpoint_id, relay_url, added_at_ms
                     FROM runtime_connection ORDER BY position, added_at_ms",
                )
                .map_err(sql)?;
            let rows = statement
                .query_map([], |row| {
                    Ok(RuntimeConnection {
                        runtime_id: row.get(0)?,
                        name: row.get(1)?,
                        endpoint_id: row.get(2)?,
                        relay_url: row.get(3)?,
                        added_at_ms: row.get(4)?,
                    })
                })
                .map_err(sql)?;
            rows.collect::<Result<_, _>>().map_err(sql)
        })
    }

    /// 添加或更新一个远程运行时。同一个运行时重新配对（endpoint 或 relay 变了）就覆盖，
    /// 位置和添加时间不变；新的排在最后。
    #[track_caller]
    pub fn put_runtime_connection(
        &self,
        runtime: &RuntimeConnection,
    ) -> Result<(), ClientStoreError> {
        self.write(|connection| {
            connection
                .execute(
                    "INSERT INTO runtime_connection
                       (runtime_id, name, endpoint_id, relay_url, position, added_at_ms)
                     VALUES (?1, ?2, ?3, ?4,
                       (SELECT COALESCE(MAX(position) + 1, 0) FROM runtime_connection), ?5)
                     ON CONFLICT (runtime_id) DO UPDATE SET name = excluded.name,
                       endpoint_id = excluded.endpoint_id, relay_url = excluded.relay_url",
                    params![
                        runtime.runtime_id,
                        runtime.name,
                        runtime.endpoint_id,
                        runtime.relay_url,
                        runtime.added_at_ms
                    ],
                )
                .map(drop)
                .map_err(sql)
        })
    }

    /// 移除一个远程运行时。返回是否真有它。
    #[track_caller]
    pub fn delete_runtime_connection(&self, runtime_id: &str) -> Result<bool, ClientStoreError> {
        self.with(|connection| {
            connection
                .execute(
                    "DELETE FROM runtime_connection WHERE runtime_id = ?1",
                    params![runtime_id],
                )
                .map(|changed| changed > 0)
                .map_err(sql)
        })
    }
}

/// 当前进程的客户端库（`$SMELT_HOME/client.sqlite3`），第一次用时打开。
pub fn client_store() -> Result<&'static ClientStore, ClientStoreError> {
    static STORE: OnceLock<Result<ClientStore, String>> = OnceLock::new();
    STORE
        .get_or_init(|| {
            let home = smelt_paths::smelt_home().ok_or_else(|| "找不到用户目录".to_string())?;
            ClientStore::open(home.join(DATABASE_FILE_NAME)).map_err(String::from)
        })
        .as_ref()
        .map_err(|error| ClientStoreError(error.clone()))
}

#[cfg(test)]
mod tests;

/// 界面 render 期间访问客户端库的检查。GPUI 每次重绘都整段重跑 render，在里面读写
/// SQLite 会把每一帧都拖慢；debug 构建下发现就在 stderr 报一次调用栈（AGENTS.md）。
pub mod render_guard {
    use std::cell::Cell;

    thread_local! {
        static DEPTH: Cell<u32> = const { Cell::new(0) };
    }

    /// 当前线程正在执行视图 render。随作用域结束自动退出。
    #[must_use = "scope 一 drop 就退出渲染标记"]
    pub struct RenderScope(());

    impl RenderScope {
        pub fn enter() -> Self {
            DEPTH.with(|depth| depth.set(depth.get() + 1));
            Self(())
        }
    }

    impl Drop for RenderScope {
        fn drop(&mut self) {
            DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }

    pub fn in_render() -> bool {
        DEPTH.with(Cell::get) > 0
    }

    #[track_caller]
    pub(crate) fn check_store_access() {
        if cfg!(debug_assertions) && in_render() {
            report(std::panic::Location::caller());
        }
    }

    fn report(location: &'static std::panic::Location<'static>) {
        use std::collections::HashSet;
        use std::sync::{Mutex, OnceLock};

        static SEEN: OnceLock<Mutex<HashSet<(&'static str, u32)>>> = OnceLock::new();
        let first = SEEN
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert((location.file(), location.line()));
        if first {
            eprintln!(
                "[smelt] render 里访问了客户端库（{location}），每次重绘都会重复读库：\n{}",
                std::backtrace::Backtrace::force_capture()
            );
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn scope_marks_render_until_dropped_and_nests() {
            assert!(!in_render());
            let outer = RenderScope::enter();
            {
                let _inner = RenderScope::enter();
                assert!(in_render());
            }
            assert!(in_render());
            drop(outer);
            assert!(!in_render());
        }
    }
}
