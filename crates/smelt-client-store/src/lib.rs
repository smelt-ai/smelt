//! 桌面客户端自己的库：`~/.smelt/client.sqlite3`。
//!
//! 放的是每台设备可以不同、不该交给运行时的东西（设计文档 D1）：外观、更新设置、
//! 快捷键、工作台布局和页面路由、远程运行时的连接。插件的界面状态存在插件自己的库里
//! （[`plugin_store`]）。运行时的数据在运行时自己的库里，GUI
//! 只经协议读写。每样东西一张表、一个字段一列，表结构见 `schema.rs`。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use rusqlite::{Connection, OptionalExtension, params};

mod plugin_store;
mod schema;

pub use plugin_store::{PLUGIN_DATA_DIR, PluginStore, plugin_store};
pub use rusqlite;

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

/// 外观。枚举类字段存的是它们的 serde 名（如 `dark`），由界面转换。
#[derive(Clone, Debug, PartialEq)]
pub struct AppearanceRecord {
    pub bg_image: Option<String>,
    pub bg_image_opacity: f32,
    pub theme_mode: String,
    pub theme_name: String,
    pub ui_font_px: u32,
    pub ui_font_family: String,
    /// 等宽字体（代码、终端）。空是出厂默认。
    pub font_family: String,
    /// 选的是哪份布局。`None` 是没选过，用第一个登记的布局。
    pub layout_id: Option<String>,
    /// 窗口底下垫的系统材质的名字。空的、不认识的由界面回到这个平台的默认。
    pub material: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateSettingsRecord {
    pub channel: String,
    pub auto_install: bool,
}

/// 用户改过的一条快捷键。`command` 是命令 id（`<来源>/<名字>`）；`keystroke` 是 GPUI 的
/// 按键写法（`cmd-k`），`None` 是用户把这条清掉了、不绑任何键。没有记录的命令用出厂键位。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyBindingRecord {
    pub command: String,
    pub keystroke: Option<String>,
}

/// 宿主自己的界面状态：页面路由。会话列表、分屏这些插件的状态存在插件自己的库里。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceUiRecord {
    /// 首页出口里放着的视图 id；空是默认内容。
    pub outlet: String,
}

/// 一份布局被用户改过之后的样子。`state` 是 Dock 的 `DockAreaState` 原样序列化，格式归
/// 界面。布局文件后来改了也照用，用户要新版就扔掉存档（[`ClientStore::delete_workspace_layout_state`]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceLayoutStateRecord {
    pub layout_id: String,
    pub state: String,
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
    /// 运行时的 `SMELT_HOME`，连上时从 `system.describe` 记下。客户端拿它认出那台机器上
    /// 哪些会话是智能体对话。还没连上过（或运行时太旧不报）是 `None`。
    pub smelt_home: Option<String>,
    /// 运行时所在系统（`std::env::consts::OS`，如 `linux`、`windows`），连上时从
    /// `system.describe` 记下。客户端按它切那台机器上的路径。还没连上过是 `None`。
    pub os: Option<String>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// 更新器状态里的路径。更新器只在 macOS 写这些列（设计文档 D4）。
///
/// Unix 上这就是 `OsStr` 的原始字节，和以前的 `OsStrExt::as_bytes` 相同，已经落库的
/// 非 UTF-8 路径还能读回来。别的平台用同一套 `as_encoded_bytes`（WTF-8），不再引用
/// Unix 的 `OsStrExt`。
fn path_bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_encoded_bytes()
}

fn path_from(bytes: Vec<u8>) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        return PathBuf::from(std::ffi::OsString::from_vec(bytes));
    }
    #[cfg(not(unix))]
    {
        // 更新器不在这些平台写列。迁移进来的 JSON 路径是 UTF-8；坏数据用替换符，
        // 不把不可信字节交给 `from_encoded_bytes_unchecked`。
        PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
    }
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
            "INSERT OR REPLACE INTO appearance (id, bg_image, bg_image_opacity, material,
               theme_mode, theme_name, ui_font_px, ui_font_family, font_family, layout_id)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.bg_image,
                record.bg_image_opacity,
                record.material,
                record.theme_mode,
                record.theme_name,
                record.ui_font_px,
                record.ui_font_family,
                record.font_family,
                record.layout_id
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

pub(crate) fn write_workspace_ui(
    connection: &Connection,
    record: &WorkspaceUiRecord,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT OR REPLACE INTO workspace_ui (id, outlet) VALUES (1, ?1)",
            params![record.outlet],
        )
        .map(drop)
        .map_err(sql)
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
                    "SELECT bg_image, bg_image_opacity, theme_mode, theme_name, ui_font_px,
                       ui_font_family, font_family, layout_id, material
                     FROM appearance WHERE id = 1",
                    [],
                    |row| {
                        Ok(AppearanceRecord {
                            bg_image: row.get(0)?,
                            bg_image_opacity: row.get(1)?,
                            theme_mode: row.get(2)?,
                            theme_name: row.get(3)?,
                            ui_font_px: row.get(4)?,
                            ui_font_family: row.get(5)?,
                            font_family: row.get(6)?,
                            layout_id: row.get(7)?,
                            material: row.get(8)?,
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
    pub fn workspace_ui(&self) -> Result<Option<WorkspaceUiRecord>, ClientStoreError> {
        self.with(|connection| {
            connection
                .query_row("SELECT outlet FROM workspace_ui WHERE id = 1", [], |row| {
                    Ok(WorkspaceUiRecord {
                        outlet: row.get(0)?,
                    })
                })
                .optional()
                .map_err(sql)
        })
    }

    #[track_caller]
    pub fn put_workspace_ui(&self, record: &WorkspaceUiRecord) -> Result<(), ClientStoreError> {
        self.write(|connection| write_workspace_ui(connection, record))
    }

    /// 一份布局的存档。没改过是 `None`。
    #[track_caller]
    pub fn workspace_layout_state(
        &self,
        layout_id: &str,
    ) -> Result<Option<WorkspaceLayoutStateRecord>, ClientStoreError> {
        self.with(|connection| {
            connection
                .query_row(
                    "SELECT layout_id, state FROM workspace_layout_state
                     WHERE layout_id = ?1",
                    [layout_id],
                    |row| {
                        Ok(WorkspaceLayoutStateRecord {
                            layout_id: row.get(0)?,
                            state: row.get(1)?,
                        })
                    },
                )
                .optional()
                .map_err(sql)
        })
    }

    #[track_caller]
    pub fn put_workspace_layout_state(
        &self,
        record: &WorkspaceLayoutStateRecord,
    ) -> Result<(), ClientStoreError> {
        self.write(|connection| {
            connection
                .execute(
                    "INSERT OR REPLACE INTO workspace_layout_state (layout_id, state)
                     VALUES (?1, ?2)",
                    params![record.layout_id, record.state],
                )
                .map(drop)
                .map_err(sql)
        })
    }

    /// 扔掉一份布局的存档：下次按布局文件重来。
    #[track_caller]
    pub fn delete_workspace_layout_state(&self, layout_id: &str) -> Result<(), ClientStoreError> {
        self.write(|connection| {
            connection
                .execute(
                    "DELETE FROM workspace_layout_state WHERE layout_id = ?1",
                    [layout_id],
                )
                .map(drop)
                .map_err(sql)
        })
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

    /// 用户改过的快捷键，按命令 id 排。
    #[track_caller]
    pub fn key_bindings(&self) -> Result<Vec<KeyBindingRecord>, ClientStoreError> {
        self.with(|connection| {
            let mut statement = connection
                .prepare("SELECT command, keystroke FROM key_binding ORDER BY command")
                .map_err(sql)?;
            let rows = statement
                .query_map([], |row| {
                    Ok(KeyBindingRecord {
                        command: row.get(0)?,
                        keystroke: row.get(1)?,
                    })
                })
                .map_err(sql)?;
            rows.collect::<Result<_, _>>().map_err(sql)
        })
    }

    /// 记下改过的快捷键，同一条命令覆盖旧的。几条一起写：改绑时清掉对方和换上自己要么都成，
    /// 要么都不成。
    #[track_caller]
    pub fn put_key_bindings(&self, records: &[KeyBindingRecord]) -> Result<(), ClientStoreError> {
        self.write(|connection| {
            let mut statement = connection
                .prepare("INSERT OR REPLACE INTO key_binding (command, keystroke) VALUES (?1, ?2)")
                .map_err(sql)?;
            for record in records {
                statement
                    .execute(params![record.command, record.keystroke])
                    .map_err(sql)?;
            }
            Ok(())
        })
    }

    /// 回到出厂键位：删掉这条命令的记录。
    #[track_caller]
    pub fn delete_key_binding(&self, command: &str) -> Result<(), ClientStoreError> {
        self.write(|connection| {
            connection
                .execute(
                    "DELETE FROM key_binding WHERE command = ?1",
                    params![command],
                )
                .map(drop)
                .map_err(sql)
        })
    }

    /// 按添加顺序列出远程运行时。
    #[track_caller]
    pub fn runtime_connections(&self) -> Result<Vec<RuntimeConnection>, ClientStoreError> {
        self.with(|connection| {
            let mut statement = connection
                .prepare(
                    "SELECT runtime_id, name, endpoint_id, relay_url, added_at_ms, smelt_home, os
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
                        smelt_home: row.get(5)?,
                        os: row.get(6)?,
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
                       (runtime_id, name, endpoint_id, relay_url, position, added_at_ms, smelt_home, os)
                     VALUES (?1, ?2, ?3, ?4,
                       (SELECT COALESCE(MAX(position) + 1, 0) FROM runtime_connection), ?5, ?6, ?7)
                     ON CONFLICT (runtime_id) DO UPDATE SET name = excluded.name,
                       endpoint_id = excluded.endpoint_id, relay_url = excluded.relay_url,
                       smelt_home = excluded.smelt_home, os = excluded.os",
                    params![
                        runtime.runtime_id,
                        runtime.name,
                        runtime.endpoint_id,
                        runtime.relay_url,
                        runtime.added_at_ms,
                        runtime.smelt_home,
                        runtime.os
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
