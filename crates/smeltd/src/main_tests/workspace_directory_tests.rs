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
    assert_eq!(published.conversations, std::slice::from_ref(&conversation));

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

/// 手机、命令行用 `session.delete` 删掉的会话，目录里也要摘掉：终端只摘这一格，摘到最后一格
/// 删整段对话；ACP 会话删整段对话。不然列表一直留着它，桌面重启还会把它恢复回来。
#[test]
fn a_deleted_session_leaves_the_directory() {
    let temp = TempStore::new();
    let event_hub = new_event_hub();
    let mut split = terminal_conversation();
    split.terminals.push(WorkspaceTerminal {
        id: Some("term-right".to_string()),
        cwd: Some("/repo".to_string()),
        ..WorkspaceTerminal::default()
    });
    let left = split.terminals[0].id.clone().unwrap();
    let acp = WorkspaceConversation {
        id: "conv-acp".to_string(),
        acp: Some(smelt_core::workspace_directory::WorkspaceAcp {
            sid: "acp-1".to_string(),
            agent: "pi".to_string(),
            cwd: Some("/repo".to_string()),
            ..Default::default()
        }),
        ..WorkspaceConversation::default()
    };
    for conversation in [&split, &acp] {
        workspace_directory::create_conversation(&temp.store, conversation, &event_hub).unwrap();
    }

    workspace_directory::forget_session(&temp.store, &left, &event_hub).unwrap();
    let published = published_directory(&event_hub).expect("摘掉之后推过目录");
    let remaining = published
        .conversations
        .iter()
        .find(|conversation| conversation.id == split.id)
        .expect("还有一格，对话留着");
    assert_eq!(
        remaining
            .terminals
            .iter()
            .map(|terminal| terminal.id.as_deref())
            .collect::<Vec<_>>(),
        [Some("term-right")]
    );

    workspace_directory::forget_session(&temp.store, "term-right", &event_hub).unwrap();
    workspace_directory::forget_session(&temp.store, "acp-1", &event_hub).unwrap();
    let published = published_directory(&event_hub).unwrap();
    assert!(published.conversations.is_empty(), "最后一格、ACP 会话都删整段对话");

    // 不在目录里的（远程目录里的会话）不改目录。
    let revision = published.revision;
    workspace_directory::forget_session(&temp.store, "term-remote", &event_hub).unwrap();
    assert_eq!(published_directory(&event_hub).unwrap().revision, revision);
}

/// 运行时自己开的对话（手机、命令行、Pi 派生）起好之后写进目录；同一个会话的重试（同一个
/// `request_id`）不再写第二段，返回已经在的那一段。
#[test]
fn a_daemon_opened_conversation_enters_the_directory_once() {
    let temp = TempStore::new();
    let event_hub = new_event_hub();
    let mut first = terminal_conversation();
    first.terminals[0].id = Some("term-phone".to_string());
    let entered =
        workspace_directory::enter_conversation(&temp.store, first.clone(), &event_hub).unwrap();
    assert_eq!(entered.id, first.id);

    let mut retry = terminal_conversation();
    retry.terminals[0].id = Some("term-phone".to_string());
    let again = workspace_directory::enter_conversation(&temp.store, retry, &event_hub).unwrap();
    assert_eq!(again.id, first.id, "重试拿到的是已经在的那一段");

    let published = published_directory(&event_hub).expect("写进去之后推过目录");
    assert_eq!(published.conversations.len(), 1);
}
