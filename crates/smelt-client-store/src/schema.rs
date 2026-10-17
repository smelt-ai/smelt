//! 客户端库的表结构和版本迁移。
//!
//! 改表结构时：改 [`CREATE_SCHEMA`]，`SCHEMA_VERSION` 加一，在 [`migrate`] 里补上从上一版
//! 过来的那一步。认不出的版本拒绝打开，不猜。

use rusqlite::{Connection, Transaction};

pub(crate) const SCHEMA_VERSION: u32 = 2;

pub(crate) const CREATE_SCHEMA: &str = "
CREATE TABLE appearance (
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
  font_family TEXT NOT NULL
);
CREATE TABLE update_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  channel TEXT NOT NULL,
  auto_install INTEGER NOT NULL CHECK (auto_install IN (0, 1))
);
CREATE TABLE notify_prefs (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  approval INTEGER NOT NULL CHECK (approval IN (0, 1)),
  input INTEGER NOT NULL CHECK (input IN (0, 1)),
  success INTEGER NOT NULL CHECK (success IN (0, 1)),
  failure INTEGER NOT NULL CHECK (failure IN (0, 1)),
  terminal_bell INTEGER NOT NULL CHECK (terminal_bell IN (0, 1))
);
CREATE TABLE workspace_ui (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  active_session INTEGER NOT NULL CHECK (active_session >= 0),
  active_session_id TEXT,
  route TEXT NOT NULL,
  selected_agent_id TEXT,
  sidebar_w REAL,
  sidebar_open INTEGER CHECK (sidebar_open IN (0, 1)),
  sidebar_grouping TEXT NOT NULL,
  sidebar_hide_empty_projects INTEGER NOT NULL CHECK (sidebar_hide_empty_projects IN (0, 1))
);
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
CREATE TABLE update_state (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  current_url TEXT,
  cleanup_app BLOB
);
CREATE TABLE staged_update (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  update_id TEXT NOT NULL,
  version TEXT NOT NULL,
  url TEXT NOT NULL,
  staged_app BLOB NOT NULL,
  fingerprint TEXT NOT NULL,
  phase TEXT NOT NULL,
  swap_app BLOB,
  installer_failed INTEGER NOT NULL CHECK (installer_failed IN (0, 1))
);
CREATE TABLE installer_attempt (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  attempt_id TEXT NOT NULL,
  target_app BLOB NOT NULL,
  relaunch INTEGER NOT NULL CHECK (relaunch IN (0, 1)),
  helper_dir BLOB NOT NULL
);
CREATE TABLE runtime_migration (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  migrated_at_ms INTEGER NOT NULL
);
CREATE TABLE runtime_connection (
  runtime_id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  endpoint_id TEXT NOT NULL,
  relay_url TEXT,
  position INTEGER NOT NULL,
  added_at_ms INTEGER NOT NULL
) WITHOUT ROWID;
";

/// v1 → v2：折叠、置顶的项目加上所在运行时。v1 时只有本机运行时（空串）。
const PROJECTS_BY_RUNTIME: &str = "
CREATE TABLE collapsed_project_v2 (
  runtime_id TEXT NOT NULL,
  root TEXT NOT NULL,
  position INTEGER NOT NULL,
  PRIMARY KEY (runtime_id, root)
) WITHOUT ROWID;
INSERT INTO collapsed_project_v2 SELECT '', root, position FROM collapsed_project;
DROP TABLE collapsed_project;
ALTER TABLE collapsed_project_v2 RENAME TO collapsed_project;
CREATE TABLE pinned_project_v2 (
  runtime_id TEXT NOT NULL,
  root TEXT NOT NULL,
  position INTEGER NOT NULL,
  PRIMARY KEY (runtime_id, root)
) WITHOUT ROWID;
INSERT INTO pinned_project_v2 SELECT '', root, position FROM pinned_project;
DROP TABLE pinned_project;
ALTER TABLE pinned_project_v2 RENAME TO pinned_project;
";

/// 打开时把库带到当前版本。
pub(crate) fn migrate(connection: &mut Connection) -> Result<(), String> {
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let version: u32 = transaction
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| error.to_string())?;
    match version {
        SCHEMA_VERSION => return Ok(()),
        0 => {
            transaction
                .execute_batch(CREATE_SCHEMA)
                .map_err(|error| error.to_string())?;
            if has_table(&transaction, "client_pref")? {
                legacy::import(&transaction)?;
                transaction
                    .execute_batch("DROP TABLE client_pref;")
                    .map_err(|error| error.to_string())?;
            }
        }
        1 => transaction
            .execute_batch(PROJECTS_BY_RUNTIME)
            .map_err(|error| error.to_string())?,
        newer => return Err(format!("客户端库版本 {newer} 比这个程序新，不认识")),
    }
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())
}

fn has_table(connection: &Connection, name: &str) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [name],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())
}

/// 第一版客户端库是一张「作用域 + 键 → JSON 值」的偏好表。打开时一次性搬进正式的表，
/// 然后删掉它。字段缺省按当时的出厂值补；某一项解析不了就丢掉这一项，用出厂值重来。
mod legacy {
    use serde::Deserialize;

    use super::Transaction;
    use crate::{
        AppearanceRecord, InstallerAttemptRecord, NotifyPrefsRecord, StagedUpdateRecord,
        UpdateSettingsRecord, UpdateStateRecord, WorkspaceUiRecord,
    };

    #[derive(Deserialize)]
    struct Appearance {
        bg_color: u32,
        bg_image: Option<String>,
        #[serde(default = "default_bg_image_opacity")]
        bg_image_opacity: f32,
        opacity: f32,
        blur: bool,
        #[serde(default = "default_glass_style")]
        glass_style: String,
        #[serde(default = "default_theme_mode")]
        theme_mode: String,
        #[serde(default = "default_theme_name")]
        theme_name: String,
        #[serde(default = "default_ui_font_px")]
        ui_font_px: u32,
        #[serde(default)]
        ui_font_family: String,
        #[serde(default = "default_font_px")]
        font_px: u32,
        #[serde(default)]
        font_family: String,
    }

    fn default_bg_image_opacity() -> f32 {
        0.25
    }
    fn default_glass_style() -> String {
        "regular".to_string()
    }
    fn default_theme_mode() -> String {
        "dark".to_string()
    }
    fn default_theme_name() -> String {
        "Default Dark".to_string()
    }
    fn default_ui_font_px() -> u32 {
        16
    }
    fn default_font_px() -> u32 {
        13
    }
    fn default_true() -> bool {
        true
    }

    #[derive(Deserialize)]
    struct UpdateSettings {
        #[serde(default = "default_channel")]
        channel: String,
        #[serde(default = "default_true")]
        auto_install: bool,
    }

    fn default_channel() -> String {
        "prod".to_string()
    }

    #[derive(Deserialize)]
    struct NotifyPrefs {
        approval: bool,
        input: bool,
        success: bool,
        failure: bool,
        terminal_bell: bool,
    }

    #[derive(Deserialize)]
    struct WorkspaceUi {
        #[serde(default)]
        active_session: usize,
        #[serde(default)]
        active_session_id: Option<String>,
        #[serde(default = "default_route")]
        route: String,
        #[serde(default)]
        selected_agent_id: Option<String>,
        #[serde(default)]
        sidebar_w: Option<f32>,
        #[serde(default)]
        sidebar_open: Option<bool>,
        #[serde(default)]
        collapsed_projects: Vec<String>,
        #[serde(default)]
        collapsed_agents: Vec<String>,
        #[serde(default = "default_grouping")]
        sidebar_grouping: String,
        #[serde(default)]
        pinned_projects: Vec<String>,
        #[serde(default)]
        sidebar_hide_empty_projects: bool,
    }

    fn default_route() -> String {
        "session".to_string()
    }
    fn default_grouping() -> String {
        "project".to_string()
    }

    #[derive(Deserialize)]
    struct UpdateState {
        #[serde(default)]
        current_url: Option<String>,
        #[serde(default)]
        staged: Option<StagedUpdate>,
        #[serde(default)]
        cleanup_app: Option<std::path::PathBuf>,
    }

    #[derive(Deserialize)]
    struct StagedUpdate {
        #[serde(default)]
        id: String,
        version: String,
        url: String,
        staged_app: std::path::PathBuf,
        #[serde(default)]
        fingerprint: String,
        #[serde(default = "default_phase")]
        phase: String,
        #[serde(default)]
        swap_app: Option<std::path::PathBuf>,
        #[serde(default)]
        installer_attempt: Option<InstallerAttempt>,
        #[serde(default)]
        installer_failed: bool,
    }

    fn default_phase() -> String {
        "ready".to_string()
    }

    #[derive(Deserialize)]
    struct InstallerAttempt {
        id: String,
        target_app: std::path::PathBuf,
        relaunch: bool,
        helper_dir: std::path::PathBuf,
    }

    fn value<T: serde::de::DeserializeOwned>(
        transaction: &Transaction<'_>,
        scope: &str,
        key: &str,
    ) -> Result<Option<T>, String> {
        let raw: Option<String> = rusqlite::OptionalExtension::optional(transaction.query_row(
            "SELECT value FROM client_pref WHERE scope = ?1 AND key = ?2",
            [scope, key],
            |row| row.get(0),
        ))
        .map_err(|error| error.to_string())?;
        Ok(raw.and_then(|raw| match serde_json::from_str(&raw) {
            Ok(value) => Some(value),
            Err(error) => {
                eprintln!("[client-store] 旧的 {scope}/{key} 解析不了，丢掉用出厂值：{error}");
                None
            }
        }))
    }

    pub(super) fn import(transaction: &Transaction<'_>) -> Result<(), String> {
        if let Some(old) = value::<Appearance>(transaction, "appearance", "current")? {
            crate::write_appearance(
                transaction,
                &AppearanceRecord {
                    bg_color: old.bg_color,
                    bg_image: old.bg_image,
                    bg_image_opacity: old.bg_image_opacity,
                    opacity: old.opacity,
                    blur: old.blur,
                    glass_style: old.glass_style,
                    theme_mode: old.theme_mode,
                    theme_name: old.theme_name,
                    ui_font_px: old.ui_font_px,
                    ui_font_family: old.ui_font_family,
                    font_px: old.font_px,
                    font_family: old.font_family,
                },
            )?;
        }
        if let Some(old) = value::<UpdateSettings>(transaction, "update", "settings")? {
            crate::write_update_settings(
                transaction,
                &UpdateSettingsRecord {
                    channel: old.channel,
                    auto_install: old.auto_install,
                },
            )?;
        }
        if let Some(old) = value::<NotifyPrefs>(transaction, "notify", "prefs")? {
            crate::write_notify_prefs(
                transaction,
                &NotifyPrefsRecord {
                    approval: old.approval,
                    input: old.input,
                    success: old.success,
                    failure: old.failure,
                    terminal_bell: old.terminal_bell,
                },
            )?;
        }
        if let Some(old) = value::<WorkspaceUi>(transaction, "workspace", "ui")? {
            crate::write_workspace_ui(
                transaction,
                &WorkspaceUiRecord {
                    active_session: old.active_session,
                    active_session_id: old.active_session_id,
                    route: old.route,
                    selected_agent_id: old.selected_agent_id,
                    sidebar_w: old.sidebar_w,
                    sidebar_open: old.sidebar_open,
                    collapsed_projects: old
                        .collapsed_projects
                        .into_iter()
                        .map(crate::ProjectKey::local)
                        .collect(),
                    collapsed_agents: old.collapsed_agents,
                    sidebar_grouping: old.sidebar_grouping,
                    pinned_projects: old
                        .pinned_projects
                        .into_iter()
                        .map(crate::ProjectKey::local)
                        .collect(),
                    sidebar_hide_empty_projects: old.sidebar_hide_empty_projects,
                },
            )?;
        }
        if value::<bool>(transaction, "meta", "runtime_migrated")? == Some(true) {
            crate::write_runtime_migrated(transaction)?;
        }
        if let Some(old) = value::<UpdateState>(transaction, "update", "state")? {
            crate::write_update_state(
                transaction,
                &UpdateStateRecord {
                    current_url: old.current_url,
                    cleanup_app: old.cleanup_app,
                    staged: old.staged.map(|staged| StagedUpdateRecord {
                        update_id: staged.id,
                        version: staged.version,
                        url: staged.url,
                        staged_app: staged.staged_app,
                        fingerprint: staged.fingerprint,
                        phase: staged.phase,
                        swap_app: staged.swap_app,
                        installer_attempt: staged.installer_attempt.map(|attempt| {
                            InstallerAttemptRecord {
                                attempt_id: attempt.id,
                                target_app: attempt.target_app,
                                relaunch: attempt.relaunch,
                                helper_dir: attempt.helper_dir,
                            }
                        }),
                        installer_failed: staged.installer_failed,
                    }),
                },
            )?;
        }
        Ok(())
    }
}
