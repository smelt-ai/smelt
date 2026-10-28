//! 运行时的工作区目录：持久化、版本，和改它的单条改动。
//!
//! 目录只经单条改动写：客户端发来的 `workspace.conversation.*`、`workspace.project.*`，
//! 和运行时自己的改动（对话改名、守护确认或放弃恢复身份）。每条在存储层的一个事务里
//! 原子执行，按 id 幂等，不带基准版本，多个客户端同时改不会互相覆盖；真改了才推进版本。
//! 改完返回整份目录，由守护经 `workspace.changed` 推给所有客户端。

use std::sync::Mutex;

use smelt_model::agent_kind::ConversationLaunchSpec;
use smelt_model::conversation::ConversationBinding;
use smelt_model::workspace_directory::{
    WorkspaceAcp, WorkspaceConversation, WorkspaceDirectory, WorkspaceTerminal,
};

/// 改动和随后读回整份目录要连在一起，见 [`change_on`]。
static CATALOG_LOCK: Mutex<()> = Mutex::new(());

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
            StoreError::ConversationMissingId
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

/// 把以 `sid` 为 ACP 会话的那段对话改名：对话标题和 ACP 会话标题一起改。GUI 没开时，
/// 下次冷启动靠这份标题，而不是远程目录里的种子标题。没有这段对话、或标题本来就是它，
/// 不改动。
pub fn rename_acp_conversation_on(
    store: &smelt_store::Store,
    sid: &str,
    title: &str,
) -> Result<DirectoryChange, DirectoryError> {
    change_on(store, |store| {
        Ok((store.rename_workspace_acp_conversation(sid, title)?.changed, None))
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

    /// 守护改名同时改对话标题和 ACP 会话标题；同名再改一次不推进版本。
    #[test]
    fn renaming_an_acp_conversation_changes_both_titles_once() {
        let (store, dir) = temp_store();
        create_conversation_on(&store, &acp_conversation()).unwrap();

        let renamed = rename_acp_conversation_on(&store, "acp-1", "新标题").unwrap();
        assert!(renamed.changed);
        assert_eq!(renamed.directory.revision, 2);
        let conversation = &renamed.directory.conversations[0];
        assert_eq!(conversation.custom_title.as_deref(), Some("新标题"));
        assert_eq!(
            conversation.acp.as_ref().unwrap().session_title.as_deref(),
            Some("新标题")
        );

        let again = rename_acp_conversation_on(&store, "acp-1", "新标题").unwrap();
        assert!(!again.changed);
        assert_eq!(again.directory.revision, 2);
        let missing = rename_acp_conversation_on(&store, "acp-missing", "x").unwrap();
        assert!(!missing.changed);
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
}
