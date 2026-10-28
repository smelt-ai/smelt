use super::*;
use smelt_core::workspace_directory::{
    WorkspaceConversation, WorkspaceDirectory, WorkspaceTerminal,
};
use smelt_event_protocol::{CORE_SNAPSHOT_WORKSPACE, CoreProjectionEvent, SnapshotKind};
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode};

/// 守护测试不开默认主库，目录命令跑在临时库上。
struct TempStore {
    dir: std::path::PathBuf,
    store: smelt_store::Store,
}

impl TempStore {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "smelt-workspace-directory-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store =
            smelt_store::Store::open_or_create(dir.join(smelt_store::DATABASE_FILE_NAME)).unwrap();
        Self { dir, store }
    }
}

impl Drop for TempStore {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 订阅方首帧拿到的那份目录。
fn published_directory(event_hub: &EventHubHandle) -> Option<WorkspaceDirectory> {
    let snapshot = event_hub
        .runtime()
        .snapshot(&SnapshotKind(CORE_SNAPSHOT_WORKSPACE.to_string()))
        .unwrap();
    let projection: CoreProjectionEvent = serde_json::from_value(snapshot.payload).unwrap();
    (!projection.data.is_null()).then(|| serde_json::from_value(projection.data).unwrap())
}

fn terminal_conversation() -> WorkspaceConversation {
    WorkspaceConversation {
        id: format!("conv-{}", uuid::Uuid::new_v4()),
        last_updated_at: 1,
        terminals: vec![WorkspaceTerminal {
            id: Some(format!("term-{}", uuid::Uuid::new_v4())),
            cwd: Some("/repo".to_string()),
            ..WorkspaceTerminal::default()
        }],
        ..WorkspaceConversation::default()
    }
}

fn delete(
    store: &smelt_store::Store,
    id: &str,
    event_hub: &EventHubHandle,
) -> Result<smelt_core::workspace_directory::WorkspaceRevision, ControlError> {
    workspace_directory::delete_conversation(
        store,
        id,
        &new_sessions(),
        &new_acp_sessions(),
        &new_test_remote_sessions(),
        &new_test_automation_store(),
        event_hub,
    )
}

/// 一台客户端新建、关掉对话，订阅方拿到的目录跟着变；关掉之后晚到的修改不能把它写回来。
#[test]
fn directory_commands_publish_the_changed_directory() {
    let temp = TempStore::new();
    let event_hub = new_event_hub();
    let conversation = terminal_conversation();

    let created =
        workspace_directory::create_conversation(&temp.store, &conversation, &event_hub).unwrap();
    let published = published_directory(&event_hub).expect("新建之后推过目录");
    assert_eq!(published.revision, created.revision);
    assert_eq!(published.conversations, [conversation.clone()]);

    let deleted = delete(&temp.store, &conversation.id, &event_hub).unwrap();
    let published = published_directory(&event_hub).expect("关掉之后推过目录");
    assert_eq!(published.revision, deleted.revision);
    assert!(published.revision > created.revision);
    assert!(published.conversations.is_empty());

    let late =
        workspace_directory::update_conversation(&temp.store, &conversation, &event_hub)
            .unwrap_err();
    assert_eq!(late.code, ControlErrorCode::NotFound);
}

/// 什么都没改的命令（重发的新建、关一段已经不在的对话）不推送。
#[test]
fn a_command_that_changes_nothing_does_not_publish() {
    let temp = TempStore::new();
    let conversation = terminal_conversation();
    workspace_directory::create_conversation(&temp.store, &conversation, &new_event_hub())
        .unwrap();

    let event_hub = new_event_hub();
    workspace_directory::create_conversation(&temp.store, &conversation, &event_hub).unwrap();
    delete(&temp.store, "conv-missing", &event_hub).unwrap();
    assert_eq!(published_directory(&event_hub), None);
}
