//! SQLite 主库入口与无法解析残留的隔离。
//!
//! 桌面/守护进程启动时调用 [`enable_sqlite_state`]，之后各领域通过
//! [`default_sqlite_store`] / [`open_sqlite_store`] 读写 `~/.smelt/smelt.sqlite3`。
//! 自有状态的活路径走类型化快照，不经 JSON 文档入口。
//! [`quarantine_json`] 只把无法解析的残留 JSON 移出活路径，供排障保留 payload。

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const JSON_NAMESPACE: &str = "json";
const QUARANTINE_NAMESPACE: &str = "quarantine";
static SQLITE_STATE_ENABLED: AtomicBool = AtomicBool::new(false);
static DEFAULT_SQLITE_STORE: OnceLock<(PathBuf, smelt_store::Store)> = OnceLock::new();

struct SqliteDocument {
    database: PathBuf,
    legacy_path: PathBuf,
    key: String,
    source: String,
}

/// 为当前桌面进程启用 `~/.smelt` 下的 SQLite 主库路由。
///
/// 真实入口在启动最早阶段显式调用；库和测试进程默认保持普通文件语义，避免仅仅调用
/// 一个业务 helper 就打开开发者家目录里的状态。
pub fn enable_sqlite_state() {
    SQLITE_STATE_ENABLED.store(true, Ordering::Release);
}

/// 直接读写 SQLite 的作用域 blob，不经过 JSON 文档路由。
///
/// 这组接口给只存在于新版应用里的偏好使用：不会读取同名 `.json` 文件，也不会
/// 写入/删除 legacy tombstone。值仅在 SQLite blob 内用 serde JSON 编码，物理存储始终
/// 是 `~/.smelt/smelt.sqlite3`。
pub fn load_sqlite_kv<T: DeserializeOwned>(
    namespace: &str,
    key: &str,
) -> Result<Option<T>, String> {
    let Some(raw) = default_sqlite_store()?.get_blob(namespace, key)? else {
        return Ok(None);
    };
    serde_json::from_slice(&raw)
        .map(Some)
        .map_err(|error| format!("SQLite KV {namespace}/{key} 解析失败: {error}"))
}

/// 直接写入 SQLite KV；不会生成兼容用的 JSON 文件。
pub fn save_sqlite_kv<T: Serialize>(namespace: &str, key: &str, value: &T) -> Result<(), String> {
    let store = default_sqlite_store()?;
    let raw = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    store.put_blob(namespace, key, &raw).map_err(Into::into)
}

/// 当前进程的默认 SQLite 库。桌面启动启用 SQLite 路由后才可用。
pub fn default_sqlite_store() -> Result<smelt_store::Store, String> {
    let root = default_smelt_root().ok_or_else(|| "SQLite state 未启用".to_string())?;
    open_sqlite_store(&root.join(smelt_store::DATABASE_FILE_NAME))
}

/// 隔离无法解析的 JSON，保留原始 payload 供排障或手工恢复。
///
/// SQLite 文档会原子移入 `quarantine` namespace；普通文件保持同目录 rename，调用方
/// 不需要知道当前文档是否已经完成 legacy 迁移。
pub fn quarantine_json(path: Option<PathBuf>, label: &str) -> Result<Option<String>, String> {
    quarantine_json_with_root(path, label, default_smelt_root().as_deref())
}

/// 非文档路径会回退成直接操作文件，而数据库自身绝不是可以被整体删除或移走的文档：
/// 抹掉它就是抹掉全部用户数据。这里把「回退只处理普通 JSON 文件」这条契约显式化，
/// 让任何把库路径误传进来的调用方立刻拿到错误，而不是静默丢数据。
fn reject_database_path(path: &Path) -> Result<(), String> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    if name == smelt_store::DATABASE_FILE_NAME
        || name.starts_with(&format!("{}-", smelt_store::DATABASE_FILE_NAME))
    {
        return Err(format!(
            "拒绝以普通文件的方式删除或移走数据库 {}",
            path.display()
        ));
    }
    Ok(())
}

fn quarantine_json_with_root(
    path: Option<PathBuf>,
    label: &str,
    smelt_root: Option<&Path>,
) -> Result<Option<String>, String> {
    let Some(path) = path else { return Ok(None) };
    validate_quarantine_label(label)?;
    if let Some(document) = sqlite_document(&path, smelt_root) {
        let store = open_sqlite_store(&document.database)?;
        let quarantine_key = format!(
            "{}/{}-{}-{}",
            document.key,
            label,
            std::process::id(),
            timestamp_nonce()
        );
        let moved = store.quarantine_with_legacy_tombstone(
            &document.source,
            JSON_NAMESPACE,
            &document.key,
            QUARANTINE_NAMESPACE,
            &quarantine_key,
        )?;
        if moved {
            archive_legacy(&document);
            return Ok(Some(format!(
                "sqlite:{QUARANTINE_NAMESPACE}/{quarantine_key}"
            )));
        }
        let Some(quarantined) = quarantine_file(&document.legacy_path, label)? else {
            return Ok(None);
        };
        if let Err(error) =
            store.mark_legacy_deleted(&document.source, JSON_NAMESPACE, &document.key)
        {
            return match std::fs::rename(&quarantined, &document.legacy_path) {
                Ok(()) => Err(format!(
                    "记录 legacy 隔离墓碑失败，已恢复原文件 {}: {error}",
                    document.legacy_path.display()
                )),
                Err(restore_error) => Err(format!(
                    "记录 legacy 隔离墓碑失败: {error}；恢复原文件也失败: {restore_error}；payload 保留在 {}",
                    quarantined.display()
                )),
            };
        }
        return Ok(Some(quarantined.display().to_string()));
    }
    reject_database_path(&path)?;
    quarantine_file(&path, label).map(|path| path.map(|path| path.display().to_string()))
}

fn default_smelt_root() -> Option<PathBuf> {
    if !SQLITE_STATE_ENABLED.load(Ordering::Acquire) {
        return None;
    }
    smelt_paths::smelt_home()
}

pub fn store_beside_json(path: &Path) -> Result<smelt_store::Store, String> {
    let database = path
        .parent()
        .ok_or_else(|| format!("{} 没有父目录", path.display()))?
        .join(smelt_store::DATABASE_FILE_NAME);
    open_sqlite_store(&database)
}

pub fn open_sqlite_store(database: &Path) -> Result<smelt_store::Store, String> {
    let default_database =
        default_smelt_root().map(|root| root.join(smelt_store::DATABASE_FILE_NAME));
    if default_database.as_deref() != Some(database) {
        return smelt_store::Store::open_or_create(database).map_err(Into::into);
    }
    if let Some((cached_path, store)) = DEFAULT_SQLITE_STORE.get()
        && cached_path == database
    {
        return Ok(store.clone());
    }

    let store = smelt_store::Store::open_or_create(database)?;
    let _ = DEFAULT_SQLITE_STORE.set((database.to_path_buf(), store.clone()));
    Ok(DEFAULT_SQLITE_STORE
        .get()
        .filter(|(cached_path, _)| cached_path == database)
        .map_or(store, |(_, cached)| cached.clone()))
}

fn sqlite_document(path: &Path, smelt_root: Option<&Path>) -> Option<SqliteDocument> {
    #[cfg(any(target_os = "ios", target_os = "android"))]
    {
        let _ = (path, smelt_root);
        return None;
    }

    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        let root = smelt_root?;
        let relative = path.strip_prefix(root).ok()?;
        if relative.as_os_str().is_empty()
            || path.extension().and_then(|extension| extension.to_str()) != Some("json")
        {
            return None;
        }
        let mut parts = Vec::new();
        for component in relative.components() {
            let std::path::Component::Normal(component) = component else {
                return None;
            };
            parts.push(component.to_str()?);
        }
        let key = parts.join("/");
        if key.is_empty() || key.starts_with("migration-backups/") {
            return None;
        }
        Some(SqliteDocument {
            database: root.join(smelt_store::DATABASE_FILE_NAME),
            legacy_path: path.to_path_buf(),
            source: format!("json:{key}"),
            key,
        })
    }
}

fn archive_legacy(document: &SqliteDocument) {
    if let Err(error) = remove_legacy_file(&document.legacy_path) {
        eprintln!(
            "[storage] 删除已导入的 legacy 文件 {} 失败: {error}",
            document.legacy_path.display()
        );
    }
}

fn remove_legacy_file(path: &Path) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(());
    }
    std::fs::remove_file(path).map_err(|error| error.to_string())
}

fn validate_quarantine_label(label: &str) -> Result<(), String> {
    if label.is_empty()
        || !label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("quarantine label 只能包含 ASCII 字母、数字、点、横线和下划线".to_string());
    }
    Ok(())
}

fn quarantine_file(path: &Path, label: &str) -> Result<Option<PathBuf>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!("拒绝隔离非普通文件: {}", path.display()));
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} 没有父目录", path.display()))?;
    let extension = path.extension().and_then(|value| value.to_str());
    let file_name = match extension {
        Some(extension) => format!(
            "{label}-{}-{}.{}",
            std::process::id(),
            timestamp_nonce(),
            extension
        ),
        None => format!("{label}-{}-{}", std::process::id(), timestamp_nonce()),
    };
    let quarantined = parent.join(file_name);
    std::fs::rename(path, &quarantined).map_err(|error| error.to_string())?;
    Ok(Some(quarantined))
}

fn timestamp_nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("smelt-json-{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn sqlite_document_rejects_parent_path_components() {
        let root = PathBuf::from("/tmp/smelt-json-root");
        let escaped = root.join("nested/../../../outside.json");
        assert!(sqlite_document(&escaped, Some(&root)).is_none());
    }

    /// 数据库不是 `.json`，走不进文档分支；若回退分支照常删文件，全部用户数据会一次性消失。
    #[test]
    fn quarantining_the_database_path_is_refused_instead_of_wiping_it() {
        let root = temp_root("guard");
        let database = root.join(smelt_store::DATABASE_FILE_NAME);
        std::fs::write(&database, b"not really a database").unwrap();
        let wal = root.join(format!("{}-wal", smelt_store::DATABASE_FILE_NAME));
        std::fs::write(&wal, b"wal").unwrap();

        assert!(quarantine_json_with_root(Some(database.clone()), "x", Some(&root)).is_err());
        assert!(database.exists(), "守卫失效，主库被隔离搬走了");
        assert!(quarantine_json_with_root(Some(wal.clone()), "x", Some(&root)).is_err());
        assert!(wal.exists(), "sidecar 被隔离同样会让库打不开");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sqlite_json_quarantine_preserves_payload_and_writes_tombstone() {
        let root = temp_root("quarantine");
        let path = root.join("update-state.json");
        let store =
            smelt_store::Store::open_or_create(root.join(smelt_store::DATABASE_FILE_NAME)).unwrap();
        store
            .put(JSON_NAMESPACE, "update-state.json", b"{not-json")
            .unwrap();

        let quarantined =
            quarantine_json_with_root(Some(path.clone()), "update-state.corrupt", Some(&root))
                .unwrap()
                .expect("坏 payload 应被隔离");
        assert!(quarantined.starts_with("sqlite:quarantine/"));
        assert!(
            store
                .get(JSON_NAMESPACE, "update-state.json")
                .unwrap()
                .is_none()
        );
        let quarantine_keys = store.keys("quarantine").unwrap();
        assert_eq!(quarantine_keys.len(), 1);
        assert_eq!(
            store
                .get("quarantine", &quarantine_keys[0])
                .unwrap()
                .unwrap(),
            b"{not-json"
        );
        assert_eq!(
            store.legacy_state("json:update-state.json").unwrap(),
            smelt_store::LegacyState::Deleted
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_legacy_quarantine_does_not_tombstone_future_valid_data() {
        use std::os::unix::fs::symlink;

        let root = temp_root("quarantine-failure");
        let target = root.join("corrupt-source.json");
        let path = root.join("update-state.json");
        std::fs::write(&target, "{not-json").unwrap();
        symlink(&target, &path).unwrap();

        assert!(
            quarantine_json_with_root(Some(path.clone()), "update-state.corrupt", Some(&root),)
                .is_err()
        );
        let store =
            smelt_store::Store::open_or_create(root.join(smelt_store::DATABASE_FILE_NAME)).unwrap();
        assert_eq!(
            store.legacy_state("json:update-state.json").unwrap(),
            smelt_store::LegacyState::Pending
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_tombstone_restores_quarantined_legacy_file_for_retry() {
        let root = temp_root("quarantine-tombstone-failure");
        let path = root.join("update-state.json");
        let database = root.join(smelt_store::DATABASE_FILE_NAME);
        drop(smelt_store::Store::open_or_create(&database).unwrap());
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_deleted_legacy
                 BEFORE INSERT ON kv
                 WHEN NEW.scope LIKE 'legacy:%'
                   AND NEW.key = 'status'
                   AND CAST(NEW.value AS TEXT) = 'deleted'
                 BEGIN
                   SELECT RAISE(ABORT, 'forced tombstone failure');
                 END;",
            )
            .unwrap();
        drop(connection);
        std::fs::write(&path, "{not-json").unwrap();

        let error =
            quarantine_json_with_root(Some(path.clone()), "update-state.corrupt", Some(&root))
                .unwrap_err();

        assert!(error.contains("forced tombstone failure"), "{error}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"{not-json",
            "墓碑提交失败后必须恢复原路径，让下次启动可以重试"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_quarantine_preserves_file_then_writes_tombstone() {
        let root = temp_root("legacy-quarantine");
        let path = root.join("update-state.json");
        std::fs::write(&path, "{not-json").unwrap();

        let quarantined =
            quarantine_json_with_root(Some(path.clone()), "update-state.corrupt", Some(&root))
                .unwrap()
                .expect("legacy 坏文件应被隔离");
        let quarantined = PathBuf::from(quarantined);
        assert_eq!(std::fs::read(&quarantined).unwrap(), b"{not-json");
        assert!(!path.exists());
        let store =
            smelt_store::Store::open_or_create(root.join(smelt_store::DATABASE_FILE_NAME)).unwrap();
        assert_eq!(
            store.legacy_state("json:update-state.json").unwrap(),
            smelt_store::LegacyState::Deleted
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
