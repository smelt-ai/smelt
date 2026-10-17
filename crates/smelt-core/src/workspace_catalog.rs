//! `workspace.load` / `workspace.save` 背后的运行时实现：工作区目录的持久化和版本。
//!
//! 版本号单独一行，每次保存加一。保存必须基于当前版本，否则拒绝：两个客户端
//! 同时开着时，后到的那份不能悄悄盖掉先到的。

use std::sync::Mutex;

use smelt_runtime_client::protocol::workspace::WorkspaceLoadResult;

/// 读版本、比对、写快照、写版本必须连成一气。运行时里改工作区快照的其它入口
/// （比如持久化对话标题）也拿这把锁。
pub(crate) static CATALOG_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, PartialEq, Eq)]
pub enum CatalogError {
    /// 客户端基于的版本已经过时。
    Conflict {
        current: u64,
    },
    /// 文档不符合存储格式。
    Invalid(String),
    Store(String),
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { .. } => f.write_str("工作区已在别处被修改"),
            Self::Invalid(message) => write!(f, "工作区文档无效：{message}"),
            Self::Store(message) => f.write_str(message),
        }
    }
}

fn revision_on(store: &smelt_store::Store) -> Result<u64, CatalogError> {
    store
        .workspace_catalog_revision()
        .map_err(|e| CatalogError::Store(e.to_string()))
}

pub fn load_on(store: &smelt_store::Store) -> Result<WorkspaceLoadResult, CatalogError> {
    let _guard = CATALOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let revision = revision_on(store)?;
    let snapshot = store
        .get_workspace_snapshot()
        .map_err(|e| CatalogError::Store(e.to_string()))?;
    let document = match snapshot {
        Some(snapshot) => {
            let raw = smelt_store::Store::workspace_snapshot_to_json(&snapshot)
                .map_err(|e| CatalogError::Store(e.to_string()))?;
            Some(serde_json::from_slice(&raw).map_err(|e| CatalogError::Store(e.to_string()))?)
        }
        None => None,
    };
    Ok(WorkspaceLoadResult { revision, document })
}

pub fn save_on(
    store: &smelt_store::Store,
    base_revision: u64,
    document: &serde_json::Value,
) -> Result<u64, CatalogError> {
    let _guard = CATALOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let current = revision_on(store)?;
    if current != base_revision {
        return Err(CatalogError::Conflict { current });
    }
    let raw = serde_json::to_vec(document).map_err(|e| CatalogError::Invalid(e.to_string()))?;
    let snapshot = smelt_store::Store::workspace_snapshot_from_json(&raw)
        .map_err(|e| CatalogError::Invalid(e.to_string()))?;
    store
        .put_workspace_snapshot(&snapshot)
        .map_err(|e| CatalogError::Store(e.to_string()))?;
    let next = current + 1;
    store
        .put_workspace_catalog_revision(next)
        .map_err(|e| CatalogError::Store(e.to_string()))?;
    Ok(next)
}

pub fn load() -> Result<WorkspaceLoadResult, CatalogError> {
    load_on(&crate::sqlite_state::default_sqlite_store().map_err(CatalogError::Store)?)
}

pub fn save(base_revision: u64, document: &serde_json::Value) -> Result<u64, CatalogError> {
    save_on(
        &crate::sqlite_state::default_sqlite_store().map_err(CatalogError::Store)?,
        base_revision,
        document,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (smelt_store::Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "smelt-workspace-catalog-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store =
            smelt_store::Store::open_or_create(dir.join(smelt_store::DATABASE_FILE_NAME)).unwrap();
        (store, dir)
    }

    fn document(project: &str) -> serde_json::Value {
        serde_json::json!({ "projects": [project], "sessions": [] })
    }

    #[test]
    fn fresh_runtime_has_no_document_at_revision_zero() {
        let (store, dir) = temp_store();
        let loaded = load_on(&store).unwrap();
        assert_eq!(loaded.revision, 0);
        assert_eq!(loaded.document, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn saves_advance_the_revision_and_round_trip_projects() {
        let (store, dir) = temp_store();
        let first = save_on(&store, 0, &document("/repo/a")).unwrap();
        let second = save_on(&store, first, &document("/repo/b")).unwrap();
        assert_eq!((first, second), (1, 2));

        let loaded = load_on(&store).unwrap();
        assert_eq!(loaded.revision, 2);
        assert_eq!(
            loaded.document.unwrap()["projects"],
            serde_json::json!(["/repo/b"])
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 另一个客户端已经保存过：基于旧版本的提交必须被拒，库里保持别人的那份。
    #[test]
    fn saving_against_a_stale_revision_is_a_conflict() {
        let (store, dir) = temp_store();
        save_on(&store, 0, &document("/repo/theirs")).unwrap();

        assert_eq!(
            save_on(&store, 0, &document("/repo/mine")),
            Err(CatalogError::Conflict { current: 1 })
        );
        assert_eq!(
            load_on(&store).unwrap().document.unwrap()["projects"],
            serde_json::json!(["/repo/theirs"])
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
