//! 客户端库的表结构和版本迁移。
//!
//! 改表结构时：改 [`CREATE_SCHEMA`]，`SCHEMA_VERSION` 加一，在 [`migrate`] 里补上从上一版
//! 过来的那一步。认不出的版本拒绝打开，不猜。
//!
//! 版本号在真实数据上跑过就不再挪作他用：没提交的构建迁过 `~/.smelt` 后又推翻了设计，要么
//! 往前再写一步迁移，要么拿迁移前的 `.bak-v<N>` 把数据还原，不要让同一个版本号对应两种表结构。

use std::path::Path;

use rusqlite::{Connection, Transaction};

pub(crate) const SCHEMA_VERSION: u32 = 15;

pub(crate) const CREATE_SCHEMA: &str = "
CREATE TABLE appearance (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  bg_image TEXT,
  bg_image_opacity REAL NOT NULL,
  bg_opacity REAL NOT NULL CHECK (bg_opacity BETWEEN 0 AND 1),
  material TEXT NOT NULL,
  theme_mode TEXT NOT NULL,
  theme_name TEXT NOT NULL,
  ui_font_px INTEGER NOT NULL,
  ui_font_family TEXT NOT NULL,
  font_family TEXT NOT NULL,
  layout_id TEXT
);
CREATE TABLE update_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  channel TEXT NOT NULL,
  auto_install INTEGER NOT NULL CHECK (auto_install IN (0, 1))
);
CREATE TABLE workspace_ui (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  outlet TEXT NOT NULL
);
CREATE TABLE workspace_layout_state (
  layout_id TEXT PRIMARY KEY,
  state TEXT NOT NULL CHECK (json_valid(state))
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
  os TEXT
) WITHOUT ROWID;
CREATE TABLE key_binding (
  command TEXT PRIMARY KEY,
  keystroke TEXT
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

/// v6 → v7：用户改过的快捷键。只存改过的那几条，没有行就是出厂键位。
const MIGRATE_V6_TO_V7: &str = "
CREATE TABLE key_binding (
  action_id TEXT PRIMARY KEY,
  keystroke TEXT
) WITHOUT ROWID;
";

/// v8：归档分段默认折叠。
const MIGRATE_V7_TO_V8: &str = "
ALTER TABLE workspace_ui ADD COLUMN archive_expanded INTEGER NOT NULL DEFAULT 0
  CHECK (archive_expanded IN (0, 1));
";

/// v9：整个工作台换成一块 Dock。三栏的宽度、开合和每段对话的工具面板状态都没了；外观里
/// 记选的是哪份布局（空 = 没选过），每份布局被用户拖成的样子按布局 id 各存一份。
const MIGRATE_V8_TO_V9: &str = "
ALTER TABLE workspace_ui DROP COLUMN sidebar_w;
ALTER TABLE workspace_ui DROP COLUMN sidebar_open;
ALTER TABLE appearance ADD COLUMN layout_id TEXT;
ALTER TABLE conversation_route DROP COLUMN stage_cover;
ALTER TABLE conversation_route DROP COLUMN tool_panel_tab;
ALTER TABLE conversation_route DROP COLUMN tool_panel_open;
ALTER TABLE conversation_route DROP COLUMN tool_panel_w;
CREATE TABLE workspace_layout_state (
  layout_id TEXT PRIMARY KEY,
  state TEXT NOT NULL CHECK (json_valid(state))
) WITHOUT ROWID;
";

/// v10：快捷键绑的是插件可登记的命令（`<来源>/<名字>`），不再是写死的动作名。旧的
/// 14 个动作名换成对应的命令 id；别的行是早已下线的动作，留着也用不上，删掉。
const MIGRATE_V9_TO_V10: &str = "
ALTER TABLE key_binding RENAME COLUMN action_id TO command;
DELETE FROM key_binding WHERE command NOT IN (
  'open_settings', 'command_palette', 'new_conversation', 'focus_sidebar', 'prev_session',
  'next_session', 'prev_pane', 'next_pane', 'split_beside', 'split_below', 'close_pane',
  'save_file', 'prev_hunk', 'next_hunk'
);
UPDATE key_binding SET command = CASE command
  WHEN 'open_settings' THEN 'smelt.app/settings'
  WHEN 'command_palette' THEN 'smelt.palette/toggle'
  WHEN 'new_conversation' THEN 'smelt.sessions/new-conversation'
  WHEN 'focus_sidebar' THEN 'smelt.sessions/focus-list'
  WHEN 'prev_session' THEN 'smelt.sessions/prev'
  WHEN 'next_session' THEN 'smelt.sessions/next'
  WHEN 'prev_pane' THEN 'smelt.stage/prev-pane'
  WHEN 'next_pane' THEN 'smelt.stage/next-pane'
  WHEN 'split_beside' THEN 'smelt.stage/split-right'
  WHEN 'split_below' THEN 'smelt.stage/split-down'
  WHEN 'close_pane' THEN 'smelt.stage/close-pane'
  WHEN 'save_file' THEN 'smelt.files/save'
  WHEN 'prev_hunk' THEN 'smelt.git/prev-hunk'
  WHEN 'next_hunk' THEN 'smelt.git/next-hunk'
END;
";

/// v11：文件树和变更页拆成了插件，各自的界面状态存在插件自己的库里。对话界面状态只剩
/// 终端的分屏和当前终端；文件树、Git 页的列和路径表删掉，旧的选中、展开不搬。
const MIGRATE_V10_TO_V11: &str = "
ALTER TABLE conversation_route DROP COLUMN file_tree_selected;
ALTER TABLE conversation_route DROP COLUMN open_file_path;
ALTER TABLE conversation_route DROP COLUMN file_tree_w;
ALTER TABLE conversation_route DROP COLUMN file_tree_open;
ALTER TABLE conversation_route DROP COLUMN git_tab;
ALTER TABLE conversation_route DROP COLUMN diff_split;
ALTER TABLE conversation_route DROP COLUMN diff_scope;
DROP TABLE conversation_route_path;
";

/// 通知偏好归通知插件（存在它自己的库里），客户端库不再留一份。旧值不搬：宿主不认识插件
/// 的表，插件的开关默认都开。
const MIGRATE_V11_TO_V12: &str = "
DROP TABLE notify_prefs;
";

/// v13：侧栏里固定的工作树，按运行时分开记。
const MIGRATE_V12_TO_V13: &str = "
CREATE TABLE pinned_workset (
  runtime_id TEXT NOT NULL,
  workset_id TEXT NOT NULL,
  PRIMARY KEY (runtime_id, workset_id)
) WITHOUT ROWID;
";

/// v14：智能体页拆成了插件，选中哪个智能体由插件自己记，客户端库不留。「新对话」命令跟着
/// 归了智能体插件，改过的快捷键搬到新 id 上。
const MIGRATE_V13_TO_V14: &str = "
ALTER TABLE workspace_ui DROP COLUMN selected_agent_id;
UPDATE key_binding SET command = 'smelt.agents/new-conversation'
  WHERE command = 'smelt.sessions/new-conversation';
";

/// 迁移进来的库没有底色不透明度，给出厂值（界面的 `DEFAULT_BG_OPACITY`）。
/// [`MIGRATE_V14_TO_V15`] 里的 `DEFAULT 0.8` 是同一个数。
const MIGRATED_BG_OPACITY: f32 = 0.8;

/// v15：会话列表、分屏拆成了会话插件，置顶、排序、折叠、选中和分屏布局都存在插件自己的
/// 库里；终端字号归了终端插件。客户端库里这些表和列删掉，旧数据不搬，插件从默认开始。
/// `workspace_ui` 只剩宿主自己的页面路由，列名跟宿主的说法改成 `outlet`。
///
/// 外观里早已没人读的 `bg_color`、`opacity`、`blur` 删掉；`glass_style` 存的其实是材质，
/// 改名 `material`。新加 `bg_opacity`：毛玻璃时主题底色铺在材质上的不透明度；旧的
/// `opacity` 是整扇窗口的不透明度，意思不一样，不搬。远程机器的 `smelt_home` 删掉：智能体
/// space 在哪改由运行时随智能体配置推来，客户端不再记。
///
/// 分屏命令跟着从 `smelt.stage` 搬到会话插件，名字不变，改过的键搬到新 id 上；新 id 已经
/// 有记录的（用户在新版里改过）以新的为准。别的 `smelt.stage/*` 和按侧栏顺序跳的
/// `smelt.sessions/activate-<N>` 已经没有对应命令，删掉。
const MIGRATE_V14_TO_V15: &str = "
DROP TABLE pinned_project;
DROP TABLE pinned_workset;
DROP TABLE collapsed_project;
DROP TABLE collapsed_agent;
DROP TABLE conversation_route;
DROP TABLE sidebar_project_order;
DROP TABLE sidebar_conversation_order;
ALTER TABLE workspace_ui DROP COLUMN active_session;
ALTER TABLE workspace_ui DROP COLUMN active_session_id;
ALTER TABLE workspace_ui DROP COLUMN sidebar_grouping;
ALTER TABLE workspace_ui DROP COLUMN sidebar_hide_empty_projects;
ALTER TABLE workspace_ui DROP COLUMN archive_expanded;
ALTER TABLE workspace_ui RENAME COLUMN route TO outlet;
ALTER TABLE appearance DROP COLUMN font_px;
ALTER TABLE appearance DROP COLUMN bg_color;
ALTER TABLE appearance DROP COLUMN opacity;
ALTER TABLE appearance DROP COLUMN blur;
ALTER TABLE appearance RENAME COLUMN glass_style TO material;
ALTER TABLE appearance ADD COLUMN bg_opacity REAL NOT NULL DEFAULT 0.8
  CHECK (bg_opacity BETWEEN 0 AND 1);
ALTER TABLE runtime_connection DROP COLUMN smelt_home;
UPDATE OR IGNORE key_binding SET command = CASE command
  WHEN 'smelt.stage/prev-pane' THEN 'smelt.sessions/prev-pane'
  WHEN 'smelt.stage/next-pane' THEN 'smelt.sessions/next-pane'
  WHEN 'smelt.stage/split-right' THEN 'smelt.sessions/split-right'
  WHEN 'smelt.stage/split-down' THEN 'smelt.sessions/split-down'
  WHEN 'smelt.stage/close-pane' THEN 'smelt.sessions/close-pane'
END
  WHERE command IN ('smelt.stage/prev-pane', 'smelt.stage/next-pane', 'smelt.stage/split-right',
    'smelt.stage/split-down', 'smelt.stage/close-pane');
DELETE FROM key_binding
  WHERE command GLOB 'smelt.stage/*' OR command GLOB 'smelt.sessions/activate-[1-9]';
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
                "{MIGRATE_V1_TO_V2}\n{MIGRATE_V2_TO_V3}\n{MIGRATE_V3_TO_V4}\n{MIGRATE_V4_TO_V5}\n{MIGRATE_V5_TO_V6}\n{MIGRATE_V6_TO_V7}"
            ))
            .map_err(|error| error.to_string())?,
        2 => transaction
            .execute_batch(&format!(
                "{MIGRATE_V2_TO_V3}\n{MIGRATE_V3_TO_V4}\n{MIGRATE_V4_TO_V5}\n{MIGRATE_V5_TO_V6}\n{MIGRATE_V6_TO_V7}"
            ))
            .map_err(|error| error.to_string())?,
        3 => transaction
            .execute_batch(&format!(
                "{MIGRATE_V3_TO_V4}\n{MIGRATE_V4_TO_V5}\n{MIGRATE_V5_TO_V6}\n{MIGRATE_V6_TO_V7}"
            ))
            .map_err(|error| error.to_string())?,
        4 => transaction
            .execute_batch(&format!(
                "{MIGRATE_V4_TO_V5}\n{MIGRATE_V5_TO_V6}\n{MIGRATE_V6_TO_V7}"
            ))
            .map_err(|error| error.to_string())?,
        5 => transaction
            .execute_batch(&format!("{MIGRATE_V5_TO_V6}\n{MIGRATE_V6_TO_V7}"))
            .map_err(|error| error.to_string())?,
        6 => transaction
            .execute_batch(MIGRATE_V6_TO_V7)
            .map_err(|error| error.to_string())?,
        7..=14 => {}
        newer => return Err(format!("client database version {newer} is newer than this app and isn't recognized")),
    }
    if (1..=7).contains(&version) {
        transaction
            .execute_batch(MIGRATE_V7_TO_V8)
            .map_err(|error| error.to_string())?;
    }
    if (1..=8).contains(&version) {
        transaction
            .execute_batch(MIGRATE_V8_TO_V9)
            .map_err(|error| error.to_string())?;
    }
    if (1..=9).contains(&version) {
        transaction
            .execute_batch(MIGRATE_V9_TO_V10)
            .map_err(|error| error.to_string())?;
    }
    if (1..=10).contains(&version) {
        transaction
            .execute_batch(MIGRATE_V10_TO_V11)
            .map_err(|error| error.to_string())?;
    }
    if (1..=11).contains(&version) {
        transaction
            .execute_batch(MIGRATE_V11_TO_V12)
            .map_err(|error| error.to_string())?;
    }
    if (1..=12).contains(&version) {
        transaction
            .execute_batch(MIGRATE_V12_TO_V13)
            .map_err(|error| error.to_string())?;
    }
    if (1..=13).contains(&version) {
        transaction
            .execute_batch(MIGRATE_V13_TO_V14)
            .map_err(|error| error.to_string())?;
    }
    if (1..=14).contains(&version) {
        transaction
            .execute_batch(MIGRATE_V14_TO_V15)
            .map_err(|error| error.to_string())?;
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
        Err(error) => {
            return Err(format!(
                "couldn't delete the old backup {}: {error}",
                backup.display()
            ));
        }
    }
    connection
        .execute("VACUUM INTO ?1", [backup.to_string_lossy()])
        .map_err(|error| {
            format!(
                "couldn't back up to {} before migrating: {error}",
                backup.display()
            )
        })?;
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
/// 然后删掉它。字段缺省按当时的出厂值补；某一项解析不了就丢掉这一项，用出厂值重来。通知
/// 偏好、终端字号和侧栏状态不搬：它们归了各自的插件。
mod legacy {
    use serde::Deserialize;

    use super::Transaction;
    use crate::{
        AppearanceRecord, InstallerAttemptRecord, StagedUpdateRecord, UpdateSettingsRecord,
        UpdateStateRecord, WorkspaceUiRecord,
    };

    #[derive(Deserialize)]
    struct Appearance {
        bg_image: Option<String>,
        #[serde(default = "default_bg_image_opacity")]
        bg_image_opacity: f32,
        #[serde(default = "default_theme_mode")]
        theme_mode: String,
        #[serde(default = "default_theme_name")]
        theme_name: String,
        #[serde(default = "default_ui_font_px")]
        ui_font_px: u32,
        #[serde(default)]
        ui_font_family: String,
        #[serde(default)]
        font_family: String,
    }

    fn default_bg_image_opacity() -> f32 {
        0.25
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
    struct WorkspaceUi {
        #[serde(default = "default_route")]
        route: String,
    }

    fn default_route() -> String {
        "session".to_string()
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
                    bg_image: old.bg_image,
                    bg_image_opacity: old.bg_image_opacity,
                    theme_mode: old.theme_mode,
                    theme_name: old.theme_name,
                    ui_font_px: old.ui_font_px,
                    ui_font_family: old.ui_font_family,
                    font_family: old.font_family,
                    layout_id: None,
                    material: String::new(),
                    bg_opacity: super::MIGRATED_BG_OPACITY,
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
        if let Some(old) = value::<WorkspaceUi>(transaction, "workspace", "ui")? {
            crate::write_workspace_ui(transaction, &WorkspaceUiRecord { outlet: old.route })?;
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
