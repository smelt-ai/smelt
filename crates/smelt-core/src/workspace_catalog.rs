//! 运行时的工作区目录：持久化、版本，和改它的两条路。
//!
//! 单条改动（`workspace.conversation.*`、`workspace.project.*`）是目录的正路：每条命令
//! 在运行时里原子执行，按 id 幂等，不带基准版本，多个客户端同时改不会互相覆盖。改完
//! 返回整份目录，由守护经 `workspace.changed` 推给所有客户端。
//!
//! `workspace.load` / `workspace.save` 是整份读写，桌面换到单条改动之后删掉。整份保存
//! 必须基于当前版本，否则拒绝；唯一的例外是当前版本就是同一个写入方提交的（回执丢了，
//! 它还停在旧版本），规则见 `Store::commit_workspace_snapshot`。单条改动会清掉写入方，
//! 之后基于旧版本的整份保存一律被拒。

use std::sync::Mutex;

use smelt_model::agent_kind::ConversationLaunchSpec;
use smelt_model::conversation::ConversationBinding;
use smelt_model::workspace_directory::{
    WorkspaceAcp, WorkspaceConversation, WorkspaceDirectory, WorkspaceTerminal,
};
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

/// 单条改动（`workspace.conversation.*`、`workspace.project.*`）失败的原因。
#[derive(Debug, PartialEq, Eq)]
pub enum DirectoryError {
    /// 要改的对话已经不在（别的客户端关掉了）。
    NotFound(String),
    /// 参数不合法：缺 id、空对话、会话 id 撞了别的对话。
    Invalid(String),
    Store(String),
}

impl std::fmt::Display for DirectoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(message) | Self::Invalid(message) | Self::Store(message) => {
                f.write_str(message)
            }
        }
    }
}

impl From<smelt_store::StoreError> for DirectoryError {
    fn from(error: smelt_store::StoreError) -> Self {
        use smelt_store::StoreError;
        match error {
            StoreError::ConversationNotFound { .. } => Self::NotFound(error.to_string()),
            StoreError::ConversationMissingId { .. }
            | StoreError::EmptyConversation { .. }
            | StoreError::AcpConversationMissingSid { .. }
            | StoreError::DuplicateSessionId { .. }
            | StoreError::EmptyProjectRoot => Self::Invalid(error.to_string()),
            error => Self::Store(error.to_string()),
        }
    }
}

/// 一次单条改动的结果：改完的整份目录（`changed` 时由调用方推送），和关掉的那段对话。
pub struct DirectoryChange {
    pub directory: WorkspaceDirectory,
    pub changed: bool,
    pub removed: Option<WorkspaceConversation>,
}

pub fn directory_on(store: &smelt_store::Store) -> Result<WorkspaceDirectory, DirectoryError> {
    let _guard = CATALOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (revision, snapshot) = store.workspace_directory()?;
    Ok(directory_from_snapshot(revision, snapshot))
}

pub fn create_conversation_on(
    store: &smelt_store::Store,
    conversation: &WorkspaceConversation,
) -> Result<DirectoryChange, DirectoryError> {
    change_on(store, |store| {
        let commit = store.create_workspace_conversation(&conversation_record(conversation))?;
        Ok((commit.changed, None))
    })
}

pub fn update_conversation_on(
    store: &smelt_store::Store,
    conversation: &WorkspaceConversation,
) -> Result<DirectoryChange, DirectoryError> {
    change_on(store, |store| {
        let commit = store.update_workspace_conversation(&conversation_record(conversation))?;
        Ok((commit.changed, None))
    })
}

pub fn delete_conversation_on(
    store: &smelt_store::Store,
    id: &str,
) -> Result<DirectoryChange, DirectoryError> {
    change_on(store, |store| {
        let (commit, removed) = store.delete_workspace_conversation(id)?;
        Ok((commit.changed, removed.map(conversation_from_record)))
    })
}

pub fn open_project_on(
    store: &smelt_store::Store,
    root: &str,
) -> Result<DirectoryChange, DirectoryError> {
    change_on(store, |store| {
        Ok((store.open_workspace_project(root)?.changed, None))
    })
}

pub fn close_project_on(
    store: &smelt_store::Store,
    root: &str,
) -> Result<DirectoryChange, DirectoryError> {
    change_on(store, |store| {
        Ok((store.close_workspace_project(root)?.changed, None))
    })
}

/// 改动和随后读回整份目录在同一把锁里：推出去的那份正是这次改完的样子，不会夹进
/// 别的改动又带着旧版本号。
fn change_on(
    store: &smelt_store::Store,
    change: impl FnOnce(
        &smelt_store::Store,
    ) -> Result<(bool, Option<WorkspaceConversation>), smelt_store::StoreError>,
) -> Result<DirectoryChange, DirectoryError> {
    let _guard = CATALOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (changed, removed) = change(store)?;
    let (revision, snapshot) = store.workspace_directory()?;
    Ok(DirectoryChange {
        directory: directory_from_snapshot(revision, snapshot),
        changed,
        removed,
    })
}

pub fn directory() -> Result<WorkspaceDirectory, DirectoryError> {
    directory_on(&default_store()?)
}

fn default_store() -> Result<smelt_store::Store, DirectoryError> {
    crate::sqlite_state::default_sqlite_store().map_err(DirectoryError::Store)
}

fn directory_from_snapshot(
    revision: u64,
    snapshot: smelt_store::WorkspaceSnapshot,
) -> WorkspaceDirectory {
    WorkspaceDirectory {
        revision,
        projects: snapshot.projects,
        conversations: snapshot
            .sessions
            .into_iter()
            .map(conversation_from_record)
            .collect(),
    }
}

fn conversation_record(conversation: &WorkspaceConversation) -> smelt_store::WorkspaceSessionRecord {
    smelt_store::WorkspaceSessionRecord {
        id: conversation.id.clone(),
        custom_title: conversation.custom_title.clone(),
        last_updated_at: conversation.last_updated_at,
        terminals: conversation
            .terminals
            .iter()
            .map(|terminal| smelt_store::WorkspaceTerminalRecord {
                id: terminal.id.clone(),
                cwd: terminal.cwd.clone(),
                custom_title: terminal.custom_title.clone(),
                launch_label: terminal.launch_label.clone(),
                launch_cmd: terminal.launch_cmd.clone(),
            })
            .collect(),
        acp: conversation.acp.as_ref().map(acp_record),
    }
}

fn acp_record(acp: &WorkspaceAcp) -> smelt_store::WorkspaceAcpRecord {
    smelt_store::WorkspaceAcpRecord {
        cwd: acp.cwd.clone(),
        sid: Some(acp.sid.clone()),
        agent: acp.agent.clone(),
        profile_id: acp.profile_id.clone(),
        history_session_id: acp.history_session_id.clone(),
        launch_command: acp.launch.command.clone(),
        launch_env: acp
            .launch
            .env
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
        refresh_launch_from_settings: acp.refresh_launch_from_settings,
        pending_prompt: acp.pending_prompt.clone(),
        pending_delivery_id: acp.pending_delivery_id.clone(),
        agent_definition_id: acp.agent_definition_id.clone(),
        automation_id: acp.automation_id.clone(),
        config_values: acp.config_values.clone(),
        binding_automation_run_id: match &acp.conversation_binding {
            ConversationBinding::Automation { run_id } => Some(run_id.clone()),
            ConversationBinding::Direct => None,
        },
        pending_agent_preset: acp.pending_agent_preset.clone(),
        session_title: acp.session_title.clone(),
    }
}

fn conversation_from_record(record: smelt_store::WorkspaceSessionRecord) -> WorkspaceConversation {
    WorkspaceConversation {
        id: record.id,
        custom_title: record.custom_title,
        last_updated_at: record.last_updated_at,
        terminals: record
            .terminals
            .into_iter()
            .map(|terminal| WorkspaceTerminal {
                id: terminal.id,
                cwd: terminal.cwd,
                custom_title: terminal.custom_title,
                launch_label: terminal.launch_label,
                launch_cmd: terminal.launch_cmd,
            })
            .collect(),
        acp: record.acp.map(acp_from_record),
    }
}

fn acp_from_record(acp: smelt_store::WorkspaceAcpRecord) -> WorkspaceAcp {
    WorkspaceAcp {
        cwd: acp.cwd,
        sid: acp.sid.unwrap_or_default(),
        agent: acp.agent,
        profile_id: acp.profile_id,
        history_session_id: acp.history_session_id,
        launch: ConversationLaunchSpec {
            command: acp.launch_command,
            env: acp.launch_env.into_iter().collect(),
        },
        refresh_launch_from_settings: acp.refresh_launch_from_settings,
        conversation_binding: match acp.binding_automation_run_id {
            Some(run_id) => ConversationBinding::Automation { run_id },
            None => ConversationBinding::Direct,
        },
        config_values: acp.config_values,
        pending_prompt: acp.pending_prompt,
        pending_delivery_id: acp.pending_delivery_id,
        pending_agent_preset: acp.pending_agent_preset,
        session_title: acp.session_title,
        agent_definition_id: acp.agent_definition_id,
        automation_id: acp.automation_id,
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

    fn acp_conversation() -> WorkspaceConversation {
        WorkspaceConversation {
            id: "conv-1".to_string(),
            custom_title: Some("标题".to_string()),
            last_updated_at: 7,
            terminals: Vec::new(),
            acp: Some(WorkspaceAcp {
                cwd: Some("/repo".to_string()),
                sid: "acp-1".to_string(),
                agent: "claude".to_string(),
                profile_id: Some("work".to_string()),
                history_session_id: Some("history-1".to_string()),
                launch: ConversationLaunchSpec::from_command("claude").with_env("A", "1"),
                refresh_launch_from_settings: true,
                conversation_binding: ConversationBinding::Automation {
                    run_id: "run-1".to_string(),
                },
                config_values: vec![("model".to_string(), "opus".to_string())],
                pending_prompt: Some("hi".to_string()),
                pending_delivery_id: Some("delivery".to_string()),
                pending_agent_preset: Some("preset".to_string()),
                session_title: Some("会话".to_string()),
                agent_definition_id: Some("agent-1".to_string()),
                automation_id: Some("auto-1".to_string()),
            }),
        }
    }

    /// 线上的对话类型和库里的记录来回转换不丢字段。
    #[test]
    fn a_created_conversation_reads_back_unchanged() {
        let (store, dir) = temp_store();
        let conversation = acp_conversation();
        let change = create_conversation_on(&store, &conversation).unwrap();
        assert!(change.changed);
        assert_eq!(change.directory.revision, 1);
        assert_eq!(change.directory.conversations, [conversation.clone()]);

        let removed = delete_conversation_on(&store, "conv-1").unwrap();
        assert_eq!(removed.removed, Some(conversation));
        assert!(removed.directory.conversations.is_empty());
        assert_eq!(removed.directory.revision, 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn single_change_errors_are_classified() {
        let (store, dir) = temp_store();
        assert!(matches!(
            update_conversation_on(&store, &acp_conversation()),
            Err(DirectoryError::NotFound(_))
        ));
        let mut missing_sid = acp_conversation();
        missing_sid.acp.as_mut().unwrap().sid = String::new();
        assert!(matches!(
            create_conversation_on(&store, &missing_sid),
            Err(DirectoryError::Invalid(_))
        ));
        assert!(matches!(
            open_project_on(&store, ""),
            Err(DirectoryError::Invalid(_))
        ));
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
