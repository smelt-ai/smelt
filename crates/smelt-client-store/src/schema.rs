//! 客户端库的表结构和版本迁移。
//!
//! 改表结构时：改 [`CREATE_SCHEMA`]，`SCHEMA_VERSION` 加一，在 [`migrate`] 里补上从上一版
//! 过来的那一步。认不出的版本拒绝打开，不猜。
//!
//! 版本号在真实数据上跑过就不再挪作他用：没提交的构建迁过 `~/.smelt` 后又推翻了设计，要么
//! 往前再写一步迁移，要么拿迁移前的 `.bak-v<N>` 把数据还原，不要让同一个版本号对应两种表结构。

use std::path::Path;

use rusqlite::{Connection, Transaction};

pub(crate) const SCHEMA_VERSION: u32 = 6;

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
  kind TEXT NOT NULL CHECK (kind IN ('expanded', 'pinned_root', 'collapsed_root', 'git_tree_collapsed')),
  path TEXT NOT NULL,
  position INTEGER NOT NULL,
  PRIMARY KEY (runtime_id, conversation_id, kind, path)
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
  added_at_ms INTEGER NOT NULL,
  smelt_home TEXT,
  os TEXT
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
) WITHOUT ROWID;
";

/// v1 → v2：折叠、置顶的项目加上所在运行时（v1 时只有本机运行时，空串）；会话的界面
/// 路由从运行时搬回客户端，新建表。
const MIGRATE_V1_TO_V2: &str = "
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
  kind TEXT NOT NULL CHECK (kind IN ('expanded', 'pinned_root', 'collapsed_root', 'git_tree_collapsed')),
  path TEXT NOT NULL,
  position INTEGER NOT NULL,
  PRIMARY KEY (runtime_id, session_id, kind, path)
) WITHOUT ROWID;

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

/// v2 → v3：界面状态改按对话存，键从「活动终端的 id」（切一下终端就变）换成对话的稳定 id，
/// 并加上这段对话的分屏布局（JSON 字段，见 AGENTS.md 的例外）和当前看的终端。旧路由的键
/// 对不上新 id，直接丢掉，各对话回到默认界面状态。
const MIGRATE_V2_TO_V3: &str = "
DROP TABLE session_route_path;
DROP TABLE session_route;
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
  kind TEXT NOT NULL CHECK (kind IN ('expanded', 'pinned_root', 'collapsed_root', 'git_tree_collapsed')),
  path TEXT NOT NULL,
  position INTEGER NOT NULL,
  PRIMARY KEY (runtime_id, conversation_id, kind, path)
) WITHOUT ROWID;
";

/// v3 → v4：记下远程运行时的 `SMELT_HOME`（`system.describe` 报的）。客户端拿它认出那台
/// 机器上哪些会话是智能体对话；记在库里，机器还没连上时也认得出。旧行先空着，连上一次就有。
const MIGRATE_V3_TO_V4: &str = "
ALTER TABLE runtime_connection ADD COLUMN smelt_home TEXT;
";

/// v4 → v5：记下远程运行时所在系统（`system.describe` 报的 `os`）。协议里的路径是那台机器
/// 上的，Windows 运行时报来的 cwd 要按 Windows 的规则切；记在库里，机器离线时侧栏也切得对。
/// 旧行先空着，连上一次就有。
const MIGRATE_V4_TO_V5: &str = "
ALTER TABLE runtime_connection ADD COLUMN os TEXT;
";

/// v5 → v6：侧栏里项目、对话的顺序从运行时搬回客户端，每台设备各记各的。旧顺序在运行时
/// 的库里，不在这个库里，这一步只新建两张空表。
const MIGRATE_V5_TO_V6: &str = "
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
) WITHOUT ROWID;
";

/// 打开时把库带到当前版本。从旧版本迁移前先留一份备份，见 [`backup_before_migration`]。
pub(crate) fn migrate(connection: &mut Connection, path: &Path) -> Result<(), String> {
    let observed: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| error.to_string())?;
    if (1..SCHEMA_VERSION).contains(&observed) {
        backup_before_migration(connection, path, observed)?;
    }
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
            .execute_batch(&format!(
                "{MIGRATE_V1_TO_V2}\n{MIGRATE_V2_TO_V3}\n{MIGRATE_V3_TO_V4}\n{MIGRATE_V4_TO_V5}\n{MIGRATE_V5_TO_V6}"
            ))
            .map_err(|error| error.to_string())?,
        2 => transaction
            .execute_batch(&format!(
                "{MIGRATE_V2_TO_V3}\n{MIGRATE_V3_TO_V4}\n{MIGRATE_V4_TO_V5}\n{MIGRATE_V5_TO_V6}"
            ))
            .map_err(|error| error.to_string())?,
        3 => transaction
            .execute_batch(&format!(
                "{MIGRATE_V3_TO_V4}\n{MIGRATE_V4_TO_V5}\n{MIGRATE_V5_TO_V6}"
            ))
            .map_err(|error| error.to_string())?,
        4 => transaction
            .execute_batch(&format!("{MIGRATE_V4_TO_V5}\n{MIGRATE_V5_TO_V6}"))
            .map_err(|error| error.to_string())?,
        5 => transaction
            .execute_batch(MIGRATE_V5_TO_V6)
            .map_err(|error| error.to_string())?,
        newer => return Err(format!("客户端库版本 {newer} 比这个程序新，不认识")),
    }
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())
}

/// 迁移只往前走。把旧库整份留成 `client.sqlite3.bak-v<旧版本>`：新版本的表结构万一后来
/// 被推翻（开发中途的构建在真实数据上跑过），拿它覆盖回去就回到迁移前。同名的旧备份换成
/// 这一次的。`VACUUM INTO` 不能在事务里跑，所以放在迁移事务之前。
fn backup_before_migration(
    connection: &Connection,
    path: &Path,
    version: u32,
) -> Result<(), String> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".bak-v{version}"));
    let backup = path.with_file_name(name);
    match std::fs::remove_file(&backup) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("删除旧备份 {}：{error}", backup.display())),
    }
    connection
        .execute("VACUUM INTO ?1", [backup.to_string_lossy()])
        .map_err(|error| format!("迁移前备份到 {}：{error}", backup.display()))?;
    Ok(())
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
