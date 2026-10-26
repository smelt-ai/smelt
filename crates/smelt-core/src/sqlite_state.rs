//! SQLite 主库入口。
//!
//! 桌面/守护进程启动时调用 [`enable_sqlite_state`]，之后各领域通过
//! [`default_sqlite_store`] / [`open_sqlite_store`] 读写 `~/.smelt/smelt.sqlite3`。
//! 各领域都走类型化的表，不经 JSON 文档入口。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static SQLITE_STATE_ENABLED: AtomicBool = AtomicBool::new(false);
static DEFAULT_SQLITE_STORE: OnceLock<(PathBuf, smelt_store::Store)> = OnceLock::new();
/// 为当前桌面进程启用 `~/.smelt` 下的 SQLite 主库路由。
///
/// 真实入口在启动最早阶段显式调用；库和测试进程默认保持普通文件语义，避免仅仅调用
/// 一个业务 helper 就打开开发者家目录里的状态。
pub fn enable_sqlite_state() {
    SQLITE_STATE_ENABLED.store(true, Ordering::Release);
}

/// 当前进程是否已打开 SQLite 主库路由。未启用时智能体/自动化读配置应视为空，
/// 而不是把「测试进程默认不碰家目录」误报成业务错误。
pub fn is_sqlite_state_enabled() -> bool {
    SQLITE_STATE_ENABLED.load(Ordering::Acquire)
}

/// 当前进程的默认 SQLite 库。桌面启动启用 SQLite 路由后才可用。
#[track_caller]
pub fn default_sqlite_store() -> Result<smelt_store::Store, String> {
    let root = default_smelt_root().ok_or_else(|| "SQLite state 未启用".to_string())?;
    open_sqlite_store(&root.join(smelt_store::DATABASE_FILE_NAME))
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

#[track_caller]
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
