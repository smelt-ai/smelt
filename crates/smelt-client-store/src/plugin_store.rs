//! 插件自己的库：每个插件一个 SQLite 文件，`~/.smelt/plugin-data/<来源>.sqlite3`。
//!
//! 宿主不认识插件存什么，只给一个连接：表由插件自己建、自己迁移（[`PluginStore::migrate`]），
//! 和客户端库分开，插件建错表、写坏数据都波及不到宿主和别的插件。删掉一个插件，它的
//! 文件留着，再装回来接着用。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use rusqlite::{Connection, Transaction};

use crate::{ClientStoreError, render_guard};

/// 插件库放在 smelt 目录下的哪个子目录。
pub const PLUGIN_DATA_DIR: &str = "plugin-data";

pub struct PluginStore {
    path: PathBuf,
    connection: Mutex<Connection>,
}

impl PluginStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, ClientStoreError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| ClientStoreError(format!("创建 {}：{error}", parent.display())))?;
        }
        let connection = Connection::open(&path)
            .map_err(|error| ClientStoreError(format!("{}：{error}", path.display())))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self {
            path,
            connection: Mutex::new(connection),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 用连接做一件事（读，或单条写）。不要在 render 里调。
    #[track_caller]
    pub fn read<T>(
        &self,
        run: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, ClientStoreError> {
        render_guard::check_store_access();
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        run(&connection).map_err(ClientStoreError::from)
    }

    /// 在一个事务里写：`run` 返回错误就整个回滚。不要在 render 里调。
    #[track_caller]
    pub fn write<T>(
        &self,
        run: impl FnOnce(&Transaction) -> rusqlite::Result<T>,
    ) -> Result<T, ClientStoreError> {
        render_guard::check_store_access();
        let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection.transaction()?;
        let value = run(&transaction)?;
        transaction.commit()?;
        Ok(value)
    }

    /// 把表结构迁到最新。`steps[i]` 把版本 `i` 迁到 `i + 1`（版本 0 是空库），版本记在
    /// `PRAGMA user_version`。每一步和它的版本号在同一个事务里，失败停在上一个版本。
    /// 库比 `steps` 新（装回了旧版插件）时报错、不动它：旧代码读不懂新表。
    pub fn migrate(&self, steps: &[&str]) -> Result<(), ClientStoreError> {
        render_guard::check_store_access();
        let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let current: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let current = usize::try_from(current).map_err(|_| {
            ClientStoreError(format!("{}：版本号 {current} 不对", self.path.display()))
        })?;
        if current > steps.len() {
            return Err(ClientStoreError(format!(
                "{} 是版本 {current}，比这个插件新（它只认到 {}）",
                self.path.display(),
                steps.len()
            )));
        }
        for (version, step) in steps.iter().enumerate().skip(current) {
            let transaction = connection.transaction()?;
            transaction.execute_batch(step).map_err(|error| {
                ClientStoreError(format!(
                    "{}：迁到版本 {} 失败：{error}",
                    self.path.display(),
                    version + 1
                ))
            })?;
            transaction.pragma_update(None, "user_version", version as i64 + 1)?;
            transaction.commit()?;
        }
        Ok(())
    }
}

/// 来源 `owner`（命令、布局用的同一个来源 id）的库，第一次用时打开，之后同一份。
/// 来源 id 只能是字母、数字和 `.`、`_`、`-`，不能以 `.` 开头：它直接当文件名。
pub fn plugin_store(owner: &str) -> Result<Arc<PluginStore>, ClientStoreError> {
    static STORES: OnceLock<Mutex<HashMap<String, Arc<PluginStore>>>> = OnceLock::new();
    validate_owner(owner)?;
    let mut stores = STORES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(store) = stores.get(owner) {
        return Ok(store.clone());
    }
    let home = smelt_paths::smelt_home().ok_or_else(|| ClientStoreError("找不到用户目录".into()))?;
    let store = Arc::new(PluginStore::open(plugin_store_path(&home, owner))?);
    stores.insert(owner.to_string(), store.clone());
    Ok(store)
}

fn plugin_store_path(home: &Path, owner: &str) -> PathBuf {
    home.join(PLUGIN_DATA_DIR).join(format!("{owner}.sqlite3"))
}

fn validate_owner(owner: &str) -> Result<(), ClientStoreError> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    if owner.is_empty() || owner.starts_with('.') || !owner.chars().all(allowed) {
        return Err(ClientStoreError(format!(
            "插件来源「{owner}」不能当库文件名：只能用字母、数字和 . _ -，不能以 . 开头"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (PluginStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("smelt-plugin-store-{}", uuid::Uuid::new_v4()));
        let store = PluginStore::open(plugin_store_path(&dir, "smelt.files")).unwrap();
        (store, dir)
    }

    const STEPS: [&str; 2] = [
        "CREATE TABLE item (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
        "ALTER TABLE item ADD COLUMN done INTEGER NOT NULL DEFAULT 0;",
    ];

    #[test]
    fn migrate_runs_each_step_once_and_records_the_version() {
        let (store, dir) = temp_store();
        store.migrate(&STEPS[..1]).unwrap();
        store
            .write(|tx| tx.execute("INSERT INTO item (name) VALUES ('a')", []))
            .unwrap();
        store.migrate(&STEPS).unwrap();
        // 再跑一次什么都不做：第一步要是重跑，CREATE TABLE 会报表已存在。
        store.migrate(&STEPS).unwrap();
        let (name, done, version): (String, i64, i64) = store
            .read(|db| {
                let version = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
                db.query_row("SELECT name, done FROM item", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, version))
                })
            })
            .unwrap();
        assert_eq!((name.as_str(), done, version), ("a", 0, 2));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn migrate_refuses_a_store_newer_than_the_plugin() {
        let (store, dir) = temp_store();
        store.migrate(&STEPS).unwrap();
        let error = store.migrate(&STEPS[..1]).unwrap_err().to_string();
        assert!(error.contains("比这个插件新"), "{error}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_step_rolls_back_and_keeps_the_previous_version() {
        let (store, dir) = temp_store();
        let broken = [STEPS[0], "CREATE TABLE ok (x); NOT SQL;"];
        assert!(store.migrate(&broken).is_err());
        let (version, ok_table): (i64, i64) = store
            .read(|db| {
                let version = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
                let ok = db.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = 'ok'",
                    [],
                    |row| row.get(0),
                )?;
                Ok((version, ok))
            })
            .unwrap();
        assert_eq!((version, ok_table), (1, 0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn write_rolls_back_when_the_closure_fails() {
        let (store, dir) = temp_store();
        store.migrate(&STEPS).unwrap();
        let result = store.write(|tx| {
            tx.execute("INSERT INTO item (name) VALUES ('a')", [])?;
            tx.execute("INSERT INTO missing VALUES (1)", [])
        });
        assert!(result.is_err());
        let count: i64 = store
            .read(|db| db.query_row("SELECT count(*) FROM item", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(count, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn owner_must_be_a_plain_file_name() {
        for good in ["smelt.files", "my-plugin_2"] {
            assert!(validate_owner(good).is_ok(), "{good}");
        }
        for bad in ["", ".hidden", "../x", "a/b", "a b", "名字"] {
            assert!(validate_owner(bad).is_err(), "{bad}");
        }
    }
}
