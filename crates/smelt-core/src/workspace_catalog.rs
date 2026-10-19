//! `workspace.load` / `workspace.save` 背后的运行时实现：工作区目录的持久化和版本。
//!
//! 版本号单独一行，每次保存加一。保存必须基于当前版本，否则拒绝：两个客户端
//! 同时开着时，后到的那份不能悄悄盖掉先到的。唯一的例外是当前版本就是同一个写入方
//! 提交的（回执丢了，它还停在旧版本），规则见 `Store::commit_workspace_snapshot`。

use std::sync::Mutex;

use smelt_runtime_client::protocol::workspace::{WorkspaceLoadResult, WorkspaceSaveParams};

/// 读版本、比对、写快照、写版本在存储层的一个事务里完成。运行时里改工作区快照的
/// 其它入口（比如持久化对话标题）是先读后写，要拿这把锁跟保存互斥。
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
    params: &WorkspaceSaveParams,
) -> Result<u64, CatalogError> {
    let raw =
        serde_json::to_vec(&params.document).map_err(|e| CatalogError::Invalid(e.to_string()))?;
    let snapshot = smelt_store::Store::workspace_snapshot_from_json(&raw)
        .map_err(|e| CatalogError::Invalid(e.to_string()))?;
    if params.writer.trim().is_empty() {
        return Err(CatalogError::Invalid("缺少写入方".to_string()));
    }
    let writer = smelt_store::WorkspaceCatalogWriter {
        id: params.writer.clone(),
        seq: params.writer_seq,
    };
    let _guard = CATALOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    match store
        .commit_workspace_snapshot(params.base_revision, &writer, &snapshot)
        .map_err(|e| CatalogError::Store(e.to_string()))?
    {
        smelt_store::WorkspaceCatalogCommit::Committed { revision } => Ok(revision),
        smelt_store::WorkspaceCatalogCommit::Conflict { current } => {
            Err(CatalogError::Conflict { current })
        }
    }
}

pub fn load() -> Result<WorkspaceLoadResult, CatalogError> {
    load_on(&crate::sqlite_state::default_sqlite_store().map_err(CatalogError::Store)?)
}

pub fn save(params: &WorkspaceSaveParams) -> Result<u64, CatalogError> {
    save_on(
        &crate::sqlite_state::default_sqlite_store().map_err(CatalogError::Store)?,
        params,
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

    fn save_as(
        store: &smelt_store::Store,
        writer: &str,
        writer_seq: u64,
        base_revision: u64,
        project: &str,
    ) -> Result<u64, CatalogError> {
        save_on(
            store,
            &WorkspaceSaveParams {
                base_revision,
                writer: writer.to_string(),
                writer_seq,
                document: document(project),
            },
        )
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
        let first = save_as(&store, "client", 1, 0, "/repo/a").unwrap();
        let second = save_as(&store, "client", 2, first, "/repo/b").unwrap();
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
        save_as(&store, "theirs", 1, 0, "/repo/theirs").unwrap();

        assert_eq!(
            save_as(&store, "mine", 1, 0, "/repo/mine"),
            Err(CatalogError::Conflict { current: 1 })
        );
        assert_eq!(
            load_on(&store).unwrap().document.unwrap()["projects"],
            serde_json::json!(["/repo/theirs"])
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 保存经远程中继超时：运行时其实已经提交，回执没回来，客户端的版本没动。
    /// 本机下一次保存不能被当成别的设备改过而从此停掉。
    #[test]
    fn a_save_whose_reply_was_lost_does_not_block_the_next_one() {
        let (store, dir) = temp_store();
        save_as(&store, "client", 1, 0, "/repo/a").unwrap();

        assert_eq!(save_as(&store, "client", 2, 0, "/repo/b"), Ok(2));
        assert_eq!(
            load_on(&store).unwrap().document.unwrap()["projects"],
            serde_json::json!(["/repo/b"])
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_save_without_a_writer_is_invalid() {
        let (store, dir) = temp_store();
        assert!(matches!(
            save_as(&store, " ", 1, 0, "/repo/a"),
            Err(CatalogError::Invalid(_))
        ));
        assert_eq!(load_on(&store).unwrap().revision, 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
