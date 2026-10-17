//! 桌面客户端自己的库：`~/.smelt/client.sqlite3`。
//!
//! 放的是每台设备可以不同、不该交给运行时的东西（设计文档 D1）：外观、更新设置、
//! 通知开关、侧栏和布局等界面状态。运行时的数据在运行时自己的库里，GUI 只经协议读写。
//!
//! 偏好条目不多、各自独立，按「作用域 + 键」存一个 JSON 值就够了；需要按字段查询的
//! 客户端数据出现时再建专门的表。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub const DATABASE_FILE_NAME: &str = "client.sqlite3";

/// 客户端库里各条目的位置（作用域, 键）。读写两端都从这里取，别各写一份字符串。
pub mod keys {
    pub const APPEARANCE: (&str, &str) = ("appearance", "current");
    pub const UPDATE_SETTINGS: (&str, &str) = ("update", "settings");
    pub const NOTIFY_PREFS: (&str, &str) = ("notify", "prefs");
    pub const WORKSPACE_UI: (&str, &str) = ("workspace", "ui");
    /// 已经从运行时取回过旧的客户端数据（升级后只做一次）。
    pub const RUNTIME_MIGRATED: (&str, &str) = ("meta", "runtime_migrated");
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS client_pref (
  scope TEXT NOT NULL,
  key TEXT NOT NULL,
  value TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (scope, key)
) WITHOUT ROWID;
";

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
        let connection = Connection::open(&path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self {
            path,
            connection: Mutex::new(connection),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读一个值。没有这个键返回 `None`；值解析不了视为错误，不悄悄当成没有。
    #[track_caller]
    pub fn get<T: DeserializeOwned>(
        &self,
        scope: &str,
        key: &str,
    ) -> Result<Option<T>, ClientStoreError> {
        render_guard::check_store_access();
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let raw: Option<String> = connection
            .query_row(
                "SELECT value FROM client_pref WHERE scope = ?1 AND key = ?2",
                params![scope, key],
                |row| row.get(0),
            )
            .optional()?;
        raw.map(|raw| {
            serde_json::from_str(&raw)
                .map_err(|error| ClientStoreError(format!("{scope}/{key} 解析失败：{error}")))
        })
        .transpose()
    }

    /// 把解析不了的值挪到 `quarantine` 作用域（键带上原位置和时间），返回新键。原位置
    /// 清空，调用方从默认值重新开始。没有这个键返回 `None`。
    pub fn quarantine(&self, scope: &str, key: &str) -> Result<Option<String>, ClientStoreError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default();
        let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let tx = connection.transaction()?;
        let raw: Option<String> = tx
            .query_row(
                "SELECT value FROM client_pref WHERE scope = ?1 AND key = ?2",
                params![scope, key],
                |row| row.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        let moved = format!("{scope}/{key}@{now}");
        tx.execute(
            "INSERT OR REPLACE INTO client_pref (scope, key, value, updated_at_ms)
             VALUES ('quarantine', ?1, ?2, ?3)",
            params![moved, raw, now],
        )?;
        tx.execute(
            "DELETE FROM client_pref WHERE scope = ?1 AND key = ?2",
            params![scope, key],
        )?;
        tx.commit()?;
        Ok(Some(moved))
    }

    #[track_caller]
    pub fn put<T: Serialize>(
        &self,
        scope: &str,
        key: &str,
        value: &T,
    ) -> Result<(), ClientStoreError> {
        render_guard::check_store_access();
        let raw = serde_json::to_string(value)
            .map_err(|error| ClientStoreError(format!("{scope}/{key} 编码失败：{error}")))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default();
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection.execute(
            "INSERT INTO client_pref (scope, key, value, updated_at_ms) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (scope, key) DO UPDATE SET value = excluded.value,
               updated_at_ms = excluded.updated_at_ms",
            params![scope, key, raw, now],
        )?;
        Ok(())
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
mod tests {
    use super::*;

    fn temp_store() -> (ClientStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("smelt-client-store-{}", uuid::Uuid::new_v4()));
        let store = ClientStore::open(dir.join(DATABASE_FILE_NAME)).unwrap();
        (store, dir)
    }

    #[test]
    fn values_round_trip_and_overwrite() {
        let (store, dir) = temp_store();
        assert_eq!(store.get::<u32>("ui", "width").unwrap(), None);
        store.put("ui", "width", &320u32).unwrap();
        store.put("ui", "width", &360u32).unwrap();
        assert_eq!(store.get::<u32>("ui", "width").unwrap(), Some(360));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unreadable_value_is_an_error_not_missing() {
        let (store, dir) = temp_store();
        store.put("ui", "width", &"wide").unwrap();
        assert!(store.get::<u32>("ui", "width").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}

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
