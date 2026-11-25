//! 插件自己的库：每个插件一个 SQLite 文件，`~/.smelt/plugin-data/<来源>/store.sqlite3`。
//! `plugin-data/<来源>/` 整个归这个插件，库以外的文件也放这里（见 [`plugin_data_dir`]）。
//!
//! 宿主不认识插件存什么，只给一个连接：表由插件自己建、自己迁移（[`PluginStore::migrate`]），
//! 和客户端库分开，插件建错表、写坏数据都波及不到宿主和别的插件。删掉一个插件，它的
//! 文件留着，再装回来接着用。
//!
//! 用户覆写自带插件时，覆写版的数据另放一份（[`override_store`]），不和自带版共用。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use rusqlite::{Connection, OpenFlags, Transaction};

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
            std::fs::create_dir_all(parent).map_err(|error| {
                ClientStoreError(format!("couldn't create {}: {error}", parent.display()))
            })?;
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
            ClientStoreError(format!(
                "{}: invalid version {current}",
                self.path.display()
            ))
        })?;
        if current > steps.len() {
            return Err(ClientStoreError(format!(
                "{} is version {current}, newer than this plugin (it only knows up to {})",
                self.path.display(),
                steps.len()
            )));
        }
        for (version, step) in steps.iter().enumerate().skip(current) {
            let transaction = connection.transaction()?;
            transaction.execute_batch(step).map_err(|error| {
                ClientStoreError(format!(
                    "{}: migrating to version {} failed: {error}",
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

/// 覆写自带插件的用户版，数据放在 `plugin-data/.overrides/<id>/`，和自带版各存各的：
/// 两份代码各自迁移表结构，共用一个库时版本号会互相打架（覆写版多迁一步，回退到自带版
/// 就打不开；升级后自带版新加的那步又被跳过）。点开头的名字不会是插件 id，撞不上。
const OVERRIDES_DIR: &str = ".overrides";

fn open_stores() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Arc<PluginStore>>> {
    static STORES: OnceLock<Mutex<HashMap<PathBuf, Arc<PluginStore>>>> = OnceLock::new();
    STORES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn smelt_home() -> Result<PathBuf, ClientStoreError> {
    smelt_paths::smelt_home()
        .ok_or_else(|| ClientStoreError("couldn't find the home directory".into()))
}

/// 来源 `owner`（命令、布局用的同一个来源 id）的库，第一次用时打开，之后同一份。
/// 来源 id 只能是字母、数字和 `.`、`_`、`-`，不能以 `.` 开头：它直接当目录名。
pub fn plugin_store(owner: &str) -> Result<Arc<PluginStore>, ClientStoreError> {
    validate_owner(owner)?;
    plugin_store_at(&smelt_home()?, owner)
}

fn plugin_store_at(home: &Path, owner: &str) -> Result<Arc<PluginStore>, ClientStoreError> {
    let key = plugin_store_path(home, owner);
    let mut stores = open_stores();
    if let Some(store) = stores.get(&key) {
        return Ok(store.clone());
    }
    let store = Arc::new(PluginStore::open(open_path(home, owner))?);
    stores.insert(key, store.clone());
    Ok(store)
}

/// 覆写自带插件 `id` 的用户版用的库。第一次打开时从自带版的库复制一份（自带版还没有库就
/// 从空库开始），之后两边各改各的：回退或恢复默认时自带版拿到的还是它自己那份。
pub fn override_store(id: &str) -> Result<Arc<PluginStore>, ClientStoreError> {
    validate_owner(id)?;
    override_store_at(&smelt_home()?, id)
}

fn override_store_at(home: &Path, id: &str) -> Result<Arc<PluginStore>, ClientStoreError> {
    let path = override_store_path(home, id);
    if let Some(store) = open_stores().get(&path) {
        return Ok(store.clone());
    }
    if !path.exists() && has_store(home, id) {
        copy_store(&*plugin_store_at(home, id)?, &path)?;
    }
    let mut stores = open_stores();
    // 复制时没拿着锁，别的线程可能已经打开了。
    if let Some(store) = stores.get(&path) {
        return Ok(store.clone());
    }
    let store = Arc::new(PluginStore::open(&path)?);
    stores.insert(path, store.clone());
    Ok(store)
}

/// 来源 `owner` 的数据目录 `~/.smelt/plugin-data/<来源>/`，整个归这个插件。不存在就建。
pub fn plugin_data_dir(owner: &str) -> Result<PathBuf, ClientStoreError> {
    validate_owner(owner)?;
    create_dir(data_dir(&smelt_home()?, owner))
}

/// 覆写自带插件 `id` 的用户版的数据目录 `~/.smelt/plugin-data/.overrides/<id>/`。不存在就建。
pub fn override_data_dir(id: &str) -> Result<PathBuf, ClientStoreError> {
    validate_owner(id)?;
    create_dir(override_dir(&smelt_home()?, id))
}

/// 丢掉覆写版的数据（恢复默认、重新 fork 时）：下次覆写再从自带版复制。开着的连接一并
/// 关掉。没有数据不算错。
pub fn remove_override_data(id: &str) -> Result<(), ClientStoreError> {
    validate_owner(id)?;
    remove_override_data_at(&smelt_home()?, id)
}

fn remove_override_data_at(home: &Path, id: &str) -> Result<(), ClientStoreError> {
    open_stores().remove(&override_store_path(home, id));
    let dir = override_dir(home, id);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ClientStoreError(format!(
            "couldn't remove {}: {error}",
            dir.display()
        ))),
    }
}

fn create_dir(dir: PathBuf) -> Result<PathBuf, ClientStoreError> {
    std::fs::create_dir_all(&dir)
        .map_err(|error| ClientStoreError(format!("couldn't create {}: {error}", dir.display())))?;
    Ok(dir)
}

fn override_dir(home: &Path, id: &str) -> PathBuf {
    home.join(PLUGIN_DATA_DIR).join(OVERRIDES_DIR).join(id)
}

fn override_store_path(home: &Path, id: &str) -> PathBuf {
    override_dir(home, id).join("store.sqlite3")
}

/// 自带版有没有库（新位置或旧位置）。没有就不去打开它：打开会建一个空库。
fn has_store(home: &Path, owner: &str) -> bool {
    plugin_store_path(home, owner).exists()
        || home
            .join(PLUGIN_DATA_DIR)
            .join(format!("{owner}.sqlite3"))
            .exists()
}

/// 用 `VACUUM INTO` 拍一份一致的快照（含 WAL 里还没并回的写入），先写临时文件再改名，
/// 中途失败不会留下半份库。
fn copy_store(source: &PluginStore, path: &Path) -> Result<(), ClientStoreError> {
    let dir = path
        .parent()
        .ok_or_else(|| ClientStoreError(format!("{} has no parent", path.display())))?;
    std::fs::create_dir_all(dir)
        .map_err(|error| ClientStoreError(format!("couldn't create {}: {error}", dir.display())))?;
    let staging = path.with_extension(format!("sqlite3.{}", std::process::id()));
    let _ = std::fs::remove_file(&staging);
    let staging_text = staging
        .to_str()
        .ok_or_else(|| ClientStoreError(format!("{} is not UTF-8", staging.display())))?;
    let copied = source
        .read(|db| db.execute("VACUUM INTO ?1", [staging_text]).map(|_| ()))
        .and_then(|()| {
            std::fs::rename(&staging, path).map_err(|error| {
                ClientStoreError(format!("couldn't move {}: {error}", staging.display()))
            })
        });
    if copied.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    copied
}

fn data_dir(home: &Path, owner: &str) -> PathBuf {
    home.join(PLUGIN_DATA_DIR).join(owner)
}

fn plugin_store_path(home: &Path, owner: &str) -> PathBuf {
    data_dir(home, owner).join("store.sqlite3")
}

/// 打开时用的路径：库从旧位置 `plugin-data/<来源>.sqlite3` 挪进来源自己的目录。挪不成
/// （比如还有别的进程开着旧库）这次就用旧位置、下次再挪，数据始终只在一处。
fn open_path(home: &Path, owner: &str) -> PathBuf {
    let path = plugin_store_path(home, owner);
    let legacy = home.join(PLUGIN_DATA_DIR).join(format!("{owner}.sqlite3"));
    if path.exists() || !legacy.exists() {
        return path;
    }
    match move_legacy(&legacy, &path) {
        Ok(()) => path,
        // 别的进程刚挪好。
        Err(_) if path.exists() => path,
        Err(error) => {
            eprintln!(
                "[plugin-store] 把 {} 挪到 {} 失败，这次先用旧位置：{error}",
                legacy.display(),
                path.display()
            );
            legacy
        }
    }
}

/// 先把 WAL 并回主库、退出 WAL 模式（这一步要独占，别的连接开着就失败），旧库就只剩
/// 一个文件，再挪过去。别的进程可能同时在挪：打开旧库时不新建（它已经被挪走就失败），
/// 挪用硬链接再删旧名（新位置已经有了就失败），都不会拿一份空库盖掉挪好的。中途退出
/// 最多两个名字指着同一个文件，下次直接用新位置。
fn move_legacy(legacy: &Path, path: &Path) -> Result<(), ClientStoreError> {
    let connection = Connection::open_with_flags(legacy, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let mode: String =
        connection.pragma_update_and_check(None, "journal_mode", "DELETE", |row| row.get(0))?;
    drop(connection);
    if !mode.eq_ignore_ascii_case("delete") {
        return Err(ClientStoreError(format!(
            "{} is still in {mode} mode, another process may have it open",
            legacy.display()
        )));
    }
    let failed = |error: std::io::Error| ClientStoreError(error.to_string());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    std::fs::hard_link(legacy, path).map_err(failed)?;
    std::fs::remove_file(legacy).map_err(failed)
}

fn validate_owner(owner: &str) -> Result<(), ClientStoreError> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    if owner.is_empty() || owner.starts_with('.') || !owner.chars().all(allowed) {
        return Err(ClientStoreError(format!(
            "Plugin owner `{owner}` can't be a directory name: use only letters, digits and . _ -, and don't start with ."
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
        assert!(error.contains("newer than this plugin"), "{error}");
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
    fn legacy_store_moves_into_the_plugin_directory() {
        let home =
            std::env::temp_dir().join(format!("smelt-plugin-store-{}", uuid::Uuid::new_v4()));
        let legacy = home.join(PLUGIN_DATA_DIR).join("smelt.files.sqlite3");
        {
            // 旧库留在 WAL 模式、写进去的行还在 -wal 里没并回主库。
            let old = PluginStore::open(&legacy).unwrap();
            old.migrate(&STEPS).unwrap();
            old.write(|tx| tx.execute("INSERT INTO item (name) VALUES ('kept')", []))
                .unwrap();
            let wal = legacy.with_file_name("smelt.files.sqlite3-wal");
            assert!(wal.is_file());
            // 再拿一个连接占着旧库：挪不成，这次照旧用旧位置。
            assert_eq!(open_path(&home, "smelt.files"), legacy);
        }

        let path = open_path(&home, "smelt.files");
        assert_eq!(path, plugin_store_path(&home, "smelt.files"));
        let leftovers: Vec<_> = std::fs::read_dir(home.join(PLUGIN_DATA_DIR))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(leftovers, ["smelt.files"], "旧库和伴生文件都不该留下");

        let store = PluginStore::open(&path).unwrap();
        store.migrate(&STEPS).unwrap();
        let name: String = store
            .read(|db| db.query_row("SELECT name FROM item", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(name, "kept");
        // 挪过之后不再碰：新位置在，就用它。
        assert_eq!(open_path(&home, "smelt.files"), path);
        let _ = std::fs::remove_dir_all(home);
    }

    /// 另一个进程已经挪好、旧名又冒出一份（比如旧版 App 新建了空库）：不拿它盖掉。
    #[test]
    fn moving_never_overwrites_the_new_location() {
        let home =
            std::env::temp_dir().join(format!("smelt-plugin-store-{}", uuid::Uuid::new_v4()));
        let path = plugin_store_path(&home, "smelt.files");
        let legacy = home.join(PLUGIN_DATA_DIR).join("smelt.files.sqlite3");
        let store = PluginStore::open(&path).unwrap();
        store.migrate(&STEPS).unwrap();
        drop(store);
        PluginStore::open(&legacy)
            .unwrap()
            .migrate(&STEPS[..1])
            .unwrap();

        assert!(move_legacy(&legacy, &path).is_err());
        assert_eq!(open_path(&home, "smelt.files"), path);
        let version: i64 = PluginStore::open(&path)
            .unwrap()
            .read(|db| db.query_row("PRAGMA user_version", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(version, 2);
        // 旧名被挪走了：不新建一份空库。
        let gone = home.join(PLUGIN_DATA_DIR).join("smelt.gone.sqlite3");
        assert!(move_legacy(&gone, &plugin_store_path(&home, "smelt.gone")).is_err());
        assert!(!gone.exists());
        let _ = std::fs::remove_dir_all(home);
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

    fn version(store: &PluginStore) -> i64 {
        store
            .read(|db| db.query_row("PRAGMA user_version", [], |row| row.get(0)))
            .unwrap()
    }

    #[test]
    fn an_override_starts_from_a_copy_and_never_touches_the_builtin_store() {
        let home = std::env::temp_dir().join(format!("smelt-override-{}", uuid::Uuid::new_v4()));
        let builtin = plugin_store_at(&home, "smelt.files").unwrap();
        builtin.migrate(&STEPS[..1]).unwrap();
        builtin
            .write(|tx| tx.execute("INSERT INTO item (name) VALUES ('a')", []))
            .unwrap();

        let fork = override_store_at(&home, "smelt.files").unwrap();
        // 覆写版多迁一步、写自己的数据。
        fork.migrate(&STEPS).unwrap();
        fork.write(|tx| tx.execute("INSERT INTO item (name) VALUES ('b')", []))
            .unwrap();
        let count = |store: &PluginStore| -> i64 {
            store
                .read(|db| db.query_row("SELECT count(*) FROM item", [], |row| row.get(0)))
                .unwrap()
        };
        assert_eq!((version(&fork), count(&fork)), (2, 2));
        // 自带版还是它自己那份：版本没被抬高，回退时迁移照常。
        assert_eq!((version(&builtin), count(&builtin)), (1, 1));
        builtin.migrate(&STEPS[..1]).unwrap();

        // 再开一次是同一份，不会重新复制。
        assert!(Arc::ptr_eq(
            &fork,
            &override_store_at(&home, "smelt.files").unwrap()
        ));

        // 丢掉后下次重新从自带版复制。App 里只在覆写版还没装上时丢，没人拿着连接；Windows 上
        // 开着的库文件删不掉，先放掉。
        drop(fork);
        remove_override_data_at(&home, "smelt.files").unwrap();
        let again = override_store_at(&home, "smelt.files").unwrap();
        assert_eq!((version(&again), count(&again)), (1, 1));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn an_override_of_a_plugin_without_a_store_starts_empty() {
        let home = std::env::temp_dir().join(format!("smelt-override-{}", uuid::Uuid::new_v4()));
        let fork = override_store_at(&home, "smelt.fresh").unwrap();
        assert_eq!(version(&fork), 0);
        // 没替自带版建空库。
        assert!(!plugin_store_path(&home, "smelt.fresh").exists());
        let _ = std::fs::remove_dir_all(home);
    }
}
