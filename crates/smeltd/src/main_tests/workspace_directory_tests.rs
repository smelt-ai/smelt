use super::*;
use smelt_core::workspace_catalog;
use smelt_core::workspace_directory::{
    WorkspaceConversation, WorkspaceDirectory, WorkspaceTerminal,
};
use smelt_core::workspace_directory::{
    WorkspaceProjectRepositoryAction as RepositoryAction, WorkspaceProjectRepositoryActionParams,
};
use smelt_event_protocol::{CORE_SNAPSHOT_WORKSPACE, CoreProjectionEvent, SnapshotKind};
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode};

fn init_repository(path: &std::path::Path) {
    std::fs::create_dir_all(path).unwrap();
    assert!(
        std::process::Command::new("git")
            .arg("init")
            .arg(path)
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn repository_discovery_respects_a_removed_root_repository() {
    let temp = TempStore::new();
    let project = temp.dir.join("project");
    init_repository(&project);
    let root = project.to_str().unwrap();
    let event_hub = new_event_hub();
    workspace_directory::open_project(&temp.store, root, &event_hub).unwrap();
    let directory = workspace_catalog::directory_on(&temp.store).unwrap();
    assert_eq!(directory.repositories.len(), 1);
    let name = directory.repositories[0].name.clone();
    crate::workset_control::repository_action(
        &temp.store,
        WorkspaceProjectRepositoryActionParams {
            project_root: root.into(),
            action: RepositoryAction::Remove { name },
        },
        &event_hub,
    )
    .unwrap();
    let directory = crate::workset_control::repository_action(
        &temp.store,
        WorkspaceProjectRepositoryActionParams {
            project_root: root.into(),
            action: RepositoryAction::Discover,
        },
        &event_hub,
    )
    .unwrap();
    assert!(directory.repositories.is_empty());
    workspace_directory::close_project(&temp.store, root, &event_hub).unwrap();
    workspace_directory::open_project(&temp.store, root, &event_hub).unwrap();
    assert!(
        workspace_catalog::directory_on(&temp.store)
            .unwrap()
            .repositories
            .is_empty()
    );
}

#[test]
#[cfg(unix)]
fn referenced_repository_cannot_be_removed_and_owned_link_is_preserved() {
    let temp = TempStore::new();
    let project = temp.dir.join("project");
    let source = temp.dir.join("source");
    std::fs::create_dir_all(&project).unwrap();
    init_repository(&source);
    let project_root = project.to_str().unwrap().to_string();
    let event_hub = new_event_hub();
    let directory = crate::workset_control::repository_action(
        &temp.store,
        WorkspaceProjectRepositoryActionParams {
            project_root: project_root.clone(),
            action: RepositoryAction::Bind {
                source: source.to_str().unwrap().into(),
                name: "api".into(),
            },
        },
        &event_hub,
    )
    .unwrap();
    let repository = &directory.repositories[0];
    assert!(repository.managed_link.is_some());
    temp.store
        .create_workspace_workset(&smelt_store::WorkspaceWorksetRecord {
            id: "workset-1".into(),
            project_root: project_root.clone(),
            task: "issue-1".into(),
            branch: "feat/issue-1".into(),
            position: 0,
            repositories: vec![smelt_store::WorkspaceWorksetRepositoryRecord {
                name: "api".into(),
                root: repository.root.clone(),
                worktree: project
                    .join("worktrees/issue-1/api")
                    .to_str()
                    .unwrap()
                    .into(),
                base_ref: "HEAD".into(),
                branch_created: true,
                position: 0,
            }],
        })
        .unwrap();
    let error = crate::workset_control::repository_action(
        &temp.store,
        WorkspaceProjectRepositoryActionParams {
            project_root,
            action: RepositoryAction::Remove { name: "api".into() },
        },
        &event_hub,
    )
    .unwrap_err();
    assert!(error.message.contains("issue-1"));
    assert!(project.join("repos/api").exists());
    assert!(source.join(".git").exists());
}

#[cfg(unix)]
#[test]
fn discovering_repository_links_does_not_claim_or_delete_user_links() {
    let temp = TempStore::new();
    let project = temp.dir.join("project");
    let source = temp.dir.join("source");
    init_repository(&source);
    std::fs::create_dir_all(project.join("repos")).unwrap();
    let link = project.join("repos/api");
    std::os::unix::fs::symlink(&source, &link).unwrap();
    let project_root = project.to_str().unwrap().to_string();
    let event_hub = new_event_hub();
    let directory = crate::workset_control::repository_action(
        &temp.store,
        WorkspaceProjectRepositoryActionParams {
            project_root: project_root.clone(),
            action: RepositoryAction::Discover,
        },
        &event_hub,
    )
    .unwrap();
    assert_eq!(directory.repositories.len(), 1);
    assert_eq!(directory.repositories[0].managed_link, None);
    let error = crate::workset_control::repository_action(
        &temp.store,
        WorkspaceProjectRepositoryActionParams {
            project_root: project_root.clone(),
            action: RepositoryAction::Bind {
                source: source.to_str().unwrap().into(),
                name: "duplicate".into(),
            },
        },
        &event_hub,
    )
    .unwrap_err();
    assert!(error.message.contains("重复"));
    let directory = crate::workset_control::repository_action(
        &temp.store,
        WorkspaceProjectRepositoryActionParams {
            project_root,
            action: RepositoryAction::Remove { name: "api".into() },
        },
        &event_hub,
    )
    .unwrap();
    assert!(directory.repositories.is_empty());
    assert!(
        std::fs::symlink_metadata(link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(source.join(".git").exists());
}

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

/// 新建时的活动时间由运行时打上，不用客户端给的：期望值换成目录里那条的时间。
fn stamped(
    conversation: &WorkspaceConversation,
    published: &WorkspaceDirectory,
) -> WorkspaceConversation {
    WorkspaceConversation {
        last_updated_at: published.conversations[0].last_updated_at,
        ..conversation.clone()
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

#[test]
fn archiving_preserves_the_record_and_restoring_publishes_it_again() {
    let temp = TempStore::new();
    let event_hub = new_event_hub();
    let conversation = terminal_conversation();
    workspace_directory::create_conversation(&temp.store, &conversation, &event_hub).unwrap();
    for archived in [true, false] {
        workspace_directory::archive_conversation(
            &temp.store,
            &conversation.id,
            archived,
            &new_sessions(),
            &new_acp_sessions(),
            &new_test_automation_store(),
            &event_hub,
        )
        .unwrap();
        let directory = published_directory(&event_hub).unwrap();
        assert_eq!(directory.conversations.len(), 1);
        let stored = &directory.conversations[0];
        assert_eq!(stored.archived_at.is_some(), archived);
        assert_eq!(stored.terminals, conversation.terminals);
    }
}

#[test]
fn finishing_workset_preserves_archive_and_prevents_unsafe_restore() {
    let temp = TempStore::new();
    let event_hub = new_event_hub();
    let project_root = temp.dir.join("project");
    let task_root = project_root.join("worktrees").join("task");
    let repository_root = temp.dir.join("repository");
    init_repository(&repository_root);
    temp.store
        .create_workspace_workset(&smelt_store::WorkspaceWorksetRecord {
            id: "workset-1".into(),
            project_root: project_root.to_string_lossy().into_owned(),
            task: "task".into(),
            branch: "feat/task".into(),
            position: 0,
            repositories: vec![smelt_store::WorkspaceWorksetRepositoryRecord {
                name: "repo".into(),
                root: repository_root.to_string_lossy().into_owned(),
                worktree: task_root.join("repo").to_string_lossy().into_owned(),
                base_ref: "HEAD".into(),
                branch_created: false,
                position: 0,
            }],
        })
        .unwrap();
    let mut conversation = terminal_conversation();
    conversation.workset_id = Some("workset-1".into());
    conversation.terminals[0].cwd = Some(task_root.to_string_lossy().into_owned());
    workspace_directory::create_conversation(&temp.store, &conversation, &event_hub).unwrap();
    let active_error = crate::workset_control::finish(
        &temp.store,
        smelt_core::workspace_directory::WorkspaceWorksetFinishParams {
            workset_id: "workset-1".into(),
            ..Default::default()
        },
        &event_hub,
    )
    .unwrap_err();
    assert!(active_error.message.contains("未归档会话"));
    temp.store
        .set_workspace_conversation_archived(&conversation.id, Some(1))
        .unwrap();

    let missing_root = workspace_directory::archive_conversation(
        &temp.store,
        &conversation.id,
        false,
        &new_sessions(),
        &new_acp_sessions(),
        &new_test_automation_store(),
        &event_hub,
    )
    .unwrap_err();
    assert!(missing_root.message.contains("工作树"));
    crate::workset_control::finish(
        &temp.store,
        smelt_core::workspace_directory::WorkspaceWorksetFinishParams {
            workset_id: "workset-1".into(),
            ..Default::default()
        },
        &event_hub,
    )
    .unwrap();
    let directory = workspace_catalog::directory_on(&temp.store).unwrap();
    assert!(directory.worksets.is_empty());
    assert_eq!(directory.conversations.len(), 1);
    assert!(directory.conversations[0].archived_at.is_some());
    assert_eq!(directory.conversations[0].workset_id, None);
    assert_eq!(
        directory.conversations[0].closed_workset_root.as_deref(),
        task_root.to_str()
    );
    assert_eq!(directory.conversations[0].terminals, conversation.terminals);

    let error = workspace_directory::archive_conversation(
        &temp.store,
        &conversation.id,
        false,
        &new_sessions(),
        &new_acp_sessions(),
        &new_test_automation_store(),
        &event_hub,
    )
    .unwrap_err();
    assert!(error.message.contains("工作树"));
    assert!(
        workspace_catalog::directory_on(&temp.store)
            .unwrap()
            .conversations[0]
            .archived_at
            .is_some()
    );
    let mut stale = conversation.clone();
    stale.terminals[0].cwd = Some(
        temp.dir
            .join("wrong-directory")
            .to_string_lossy()
            .into_owned(),
    );
    assert!(
        workspace_directory::update_conversation(&temp.store, &stale, &new_sessions(), &event_hub)
            .is_err()
    );
    assert_eq!(
        workspace_catalog::directory_on(&temp.store)
            .unwrap()
            .conversations[0]
            .terminals,
        conversation.terminals
    );
}

#[test]
fn restoring_closed_workset_archive_rebinds_to_selected_workset() {
    let temp = TempStore::new();
    let event_hub = new_event_hub();
    let project = temp.dir.join("project");
    let repository = temp.dir.join("repository");
    init_repository(&repository);
    let old_root = project.join("worktrees").join("old");
    let target_root = project.join("worktrees").join("target");
    std::fs::create_dir_all(&target_root).unwrap();
    let record = |id: &str, task: &str| smelt_store::WorkspaceWorksetRecord {
        id: id.into(),
        project_root: project.to_string_lossy().into_owned(),
        task: task.into(),
        branch: format!("feat/{task}"),
        position: if task == "old" { 0 } else { 1 },
        repositories: vec![smelt_store::WorkspaceWorksetRepositoryRecord {
            name: "repo".into(),
            root: repository.to_string_lossy().into_owned(),
            worktree: project
                .join("worktrees")
                .join(task)
                .join("repo")
                .to_string_lossy()
                .into_owned(),
            base_ref: "HEAD".into(),
            branch_created: false,
            position: 0,
        }],
    };
    temp.store
        .create_workspace_workset(&record("old", "old"))
        .unwrap();
    temp.store
        .create_workspace_workset(&record("target", "target"))
        .unwrap();
    let mut conversation = terminal_conversation();
    conversation.workset_id = Some("old".into());
    conversation.terminals[0].cwd = Some(old_root.to_string_lossy().into_owned());
    workspace_directory::create_conversation(&temp.store, &conversation, &event_hub).unwrap();
    let acp = WorkspaceConversation {
        id: "acp-archive".into(),
        workset_id: Some("old".into()),
        acp: Some(smelt_core::workspace_directory::WorkspaceAcp {
            sid: "acp-session".into(),
            agent: "pi".into(),
            cwd: conversation.terminals[0].cwd.clone(),
            history_session_id: Some("provider-history".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    workspace_directory::create_conversation(&temp.store, &acp, &event_hub).unwrap();
    temp.store
        .set_workspace_conversation_archived(&conversation.id, Some(1))
        .unwrap();
    temp.store
        .set_workspace_conversation_archived(&acp.id, Some(1))
        .unwrap();
    crate::workset_control::finish(
        &temp.store,
        smelt_core::workspace_directory::WorkspaceWorksetFinishParams {
            workset_id: "old".into(),
            ..Default::default()
        },
        &event_hub,
    )
    .unwrap();

    let error = workspace_directory::restore_conversation_into_workset(
        &temp.store,
        &conversation.id,
        "target",
        &event_hub,
    )
    .unwrap_err();
    assert!(error.message.contains("repo"));
    let still_archived = workspace_catalog::directory_on(&temp.store).unwrap();
    assert!(still_archived.conversations[0].archived_at.is_some());
    std::fs::create_dir_all(target_root.join("repo")).unwrap();

    workspace_directory::restore_conversation_into_workset(
        &temp.store,
        &conversation.id,
        "target",
        &event_hub,
    )
    .unwrap();
    workspace_directory::restore_conversation_into_workset(
        &temp.store,
        &acp.id,
        "target",
        &event_hub,
    )
    .unwrap();
    let directory = workspace_catalog::directory_on(&temp.store).unwrap();
    let restored = &directory.conversations[0];
    assert_eq!(restored.workset_id.as_deref(), Some("target"));
    assert_eq!(restored.closed_workset_root.as_deref(), old_root.to_str());
    assert_eq!(restored.archived_at, None);
    assert_eq!(restored.terminals[0].cwd.as_deref(), target_root.to_str());
    let restored_acp = directory.conversations[1].acp.as_ref().unwrap();
    assert_eq!(restored_acp.cwd.as_deref(), target_root.to_str());
    assert_eq!(
        restored_acp.history_session_id.as_deref(),
        Some("provider-history")
    );
    assert_eq!(restored_acp.sid, "acp-session");
    let stale_update = workspace_directory::update_conversation(
        &temp.store,
        &conversation,
        &new_sessions(),
        &event_hub,
    );
    assert!(
        stale_update.is_err(),
        "旧客户端不能把恢复后的会话改回失效目录"
    );
    let current = workspace_catalog::directory_on(&temp.store).unwrap();
    assert_eq!(
        current.conversations[0].terminals[0].cwd.as_deref(),
        target_root.to_str()
    );
    temp.store
        .set_workspace_conversation_archived(&conversation.id, Some(2))
        .unwrap();
    workspace_directory::archive_conversation(
        &temp.store,
        &conversation.id,
        false,
        &new_sessions(),
        &new_acp_sessions(),
        &new_test_automation_store(),
        &event_hub,
    )
    .unwrap();
    let current = workspace_catalog::directory_on(&temp.store).unwrap();
    assert_eq!(
        current.conversations[0].workset_id.as_deref(),
        Some("target")
    );
}

#[test]
fn automation_owned_conversations_cannot_be_archived() {
    let temp = TempStore::new();
    let event_hub = new_event_hub();
    let conversation = WorkspaceConversation {
        id: "automation".into(),
        acp: Some(smelt_core::workspace_directory::WorkspaceAcp {
            sid: "automation-session".into(),
            automation_id: Some("a1".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    workspace_directory::create_conversation(&temp.store, &conversation, &event_hub).unwrap();
    let error = workspace_directory::archive_conversation(
        &temp.store,
        &conversation.id,
        true,
        &new_sessions(),
        &new_acp_sessions(),
        &new_test_automation_store(),
        &event_hub,
    )
    .unwrap_err();
    assert_eq!(error.code, ControlErrorCode::FailedPrecondition);
    assert!(
        published_directory(&event_hub).unwrap().conversations[0]
            .archived_at
            .is_none()
    );
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
    assert_eq!(
        published.conversations,
        [stamped(&conversation, &published)]
    );

    let deleted = delete(&temp.store, &conversation.id, &event_hub).unwrap();
    let published = published_directory(&event_hub).expect("关掉之后推过目录");
    assert_eq!(published.revision, deleted.revision);
    assert!(published.revision > created.revision);
    assert!(published.conversations.is_empty());

    let late = workspace_directory::update_conversation(
        &temp.store,
        &conversation,
        &new_sessions(),
        &event_hub,
    )
    .unwrap_err();
    assert_eq!(late.code, ControlErrorCode::NotFound);
}

/// 什么都没改的命令（重发的新建、关一段已经不在的对话）不推送。
#[test]
fn a_command_that_changes_nothing_does_not_publish() {
    let temp = TempStore::new();
    let conversation = terminal_conversation();
    workspace_directory::create_conversation(&temp.store, &conversation, &new_event_hub()).unwrap();

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
    assert!(
        published.conversations.is_empty(),
        "最后一格、ACP 会话都删整段对话"
    );

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

/// 守护起来时库读不出来（例：开发构建留下认不出形状的库），订阅首帧没有目录。库修好以后
/// 没有改动可等，新订阅进来要现读一次：重启桌面就能拿回对话，不用重启守护。
#[test]
fn a_subscriber_gets_the_directory_the_daemon_could_not_read_at_startup() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let temp = TempStore::new();
    let conversation = terminal_conversation();
    smelt_core::workspace_catalog::create_conversation_on(&temp.store, &conversation).unwrap();
    let readable = Arc::new(AtomicBool::new(false));
    let reader = {
        let readable = Arc::clone(&readable);
        let store = temp.store.clone();
        WorkspaceDirectoryReader::from_fn(move || {
            if !readable.load(Ordering::Acquire) {
                return Err("数据库 schema 33 形状无法识别".to_string());
            }
            smelt_core::workspace_catalog::directory_on(&store).map_err(|error| error.to_string())
        })
    };
    let event_hub = crate::event_hub::DaemonEventHub::new(reader).unwrap();
    assert_eq!(
        published_directory(&event_hub),
        None,
        "库读不出来时首帧不带目录"
    );

    readable.store(true, Ordering::Release);
    let published = published_directory(&event_hub).expect("库好了，新订阅拿到目录");
    assert_eq!(
        published.conversations,
        [stamped(&conversation, &published)]
    );
}

fn run_git(root: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 项目根本身是一个有远端的仓库（建工作树要从远端拿基线）。
fn project_with_remote(temp: &TempStore) -> std::path::PathBuf {
    let project = temp.dir.join("project");
    let remote = temp.dir.join("remote.git");
    std::fs::create_dir_all(&project).unwrap();
    run_git(&project, &["init", "--initial-branch=main"]);
    run_git(&project, &["config", "user.name", "Test"]);
    run_git(&project, &["config", "user.email", "test@example.invalid"]);
    run_git(&project, &["commit", "--allow-empty", "-m", "seed"]);
    run_git(&temp.dir, &["init", "--bare", remote.to_str().unwrap()]);
    run_git(
        &project,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    run_git(&project, &["push", "-u", "origin", "main"]);
    run_git(&project, &["remote", "set-head", "origin", "main"]);
    project
}

/// 建工作树要 fetch 远端，期间不能占着目录锁：会话的目录写入照常进行。远端的
/// upload-pack 换成一个脚本，开始时通知测试、等测试放行再继续，模拟一次很慢的 fetch。
#[test]
#[cfg(unix)]
fn creating_a_workset_does_not_block_directory_writes_while_fetching() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempStore::new();
    let event_hub = new_event_hub();
    let project = project_with_remote(&temp);

    let started = temp.dir.join("fetch-started");
    let release = temp.dir.join("fetch-release");
    for fifo in [&started, &release] {
        assert!(
            std::process::Command::new("mkfifo")
                .arg(fifo)
                .status()
                .unwrap()
                .success()
        );
    }
    let hook = temp.dir.join("slow-upload-pack");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\necho started > '{}'\ncat '{}' > /dev/null\nexec git upload-pack \"$@\"\n",
            started.display(),
            release.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    run_git(
        &project,
        &["config", "remote.origin.uploadpack", hook.to_str().unwrap()],
    );

    let root = project.to_str().unwrap().to_string();
    workspace_directory::open_project(&temp.store, &root, &event_hub).unwrap();
    let repository = workspace_catalog::directory_on(&temp.store)
        .unwrap()
        .repositories[0]
        .name
        .clone();

    std::thread::scope(|scope| {
        let creating = scope.spawn(|| {
            crate::workset_control::create(
                &temp.store,
                smelt_core::workspace_directory::WorkspaceWorksetCreateParams {
                    project_root: root.clone(),
                    task: "slow".into(),
                    branch_prefix: "feat/".into(),
                    branch: None,
                    repository_names: vec![repository],
                },
                &event_hub,
            )
        });
        // fetch 开始了：upload-pack 脚本卡在等放行上。
        std::fs::read(&started).unwrap();

        let (sent, written) = std::sync::mpsc::channel();
        let (store, hub) = (&temp.store, &event_hub);
        scope.spawn(move || {
            let conversation = terminal_conversation();
            let _ = sent.send(workspace_directory::create_conversation(
                store,
                &conversation,
                hub,
            ));
        });
        let written = written.recv_timeout(std::time::Duration::from_secs(10));

        std::fs::write(&release, b"go").unwrap();
        creating.join().unwrap().unwrap();
        assert!(
            matches!(written, Ok(Ok(_))),
            "fetch 期间目录写入被挡住了：{written:?}"
        );
    });

    let directory = workspace_catalog::directory_on(&temp.store).unwrap();
    assert_eq!(directory.worksets.len(), 1);
    assert_eq!(directory.conversations.len(), 1);
}

/// 打开项目、按项目根这个仓库建一个工作树，返回回执和任务目录。
fn create_workset_in(
    temp: &TempStore,
    project: &std::path::Path,
    event_hub: &EventHubHandle,
) -> (
    smelt_core::workspace_directory::WorkspaceWorksetCreateResult,
    std::path::PathBuf,
) {
    let root = project.to_str().unwrap().to_string();
    workspace_directory::open_project(&temp.store, &root, event_hub).unwrap();
    let repository = workspace_catalog::directory_on(&temp.store)
        .unwrap()
        .repositories[0]
        .name
        .clone();
    let worktree = project.join("worktrees/setup").join(&repository);
    let created = crate::workset_control::create(
        &temp.store,
        smelt_core::workspace_directory::WorkspaceWorksetCreateParams {
            project_root: root,
            task: "setup".into(),
            branch_prefix: "feat/".into(),
            branch: None,
            repository_names: vec![repository],
        },
        event_hub,
    )
    .unwrap();
    (created, worktree)
}

#[test]
#[cfg(unix)]
fn creating_a_workset_runs_the_setup_script_from_the_source_repository() {
    let temp = TempStore::new();
    let event_hub = new_event_hub();
    let project = project_with_remote(&temp);
    // 脚本和 .env 都没提交，只在源仓库的工作区里。
    std::fs::write(project.join(".env"), "TOKEN=1\n").unwrap();
    std::fs::create_dir_all(project.join(".smelt")).unwrap();
    std::fs::write(
        project.join(".smelt/worktree-setup"),
        "cp \"$SMELT_SOURCE_ROOT/.env\" .\nprintf %s \"$SMELT_TASK_ROOT\" > task-root\n",
    )
    .unwrap();

    let (created, worktree) = create_workset_in(&temp, &project, &event_hub);

    assert_eq!(created.setup.len(), 1);
    assert!(created.setup[0].success, "{:?}", created.setup);
    assert_eq!(
        std::fs::read_to_string(worktree.join(".env")).unwrap(),
        "TOKEN=1\n"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("task-root")).unwrap(),
        std::fs::canonicalize(&project)
            .unwrap()
            .join("worktrees/setup")
            .to_str()
            .unwrap()
    );
}

#[test]
#[cfg(unix)]
fn a_failing_setup_script_keeps_the_workset_and_reports_its_output() {
    let temp = TempStore::new();
    let event_hub = new_event_hub();
    let project = project_with_remote(&temp);
    std::fs::create_dir_all(project.join(".smelt")).unwrap();
    std::fs::write(
        project.join(".smelt/worktree-setup"),
        "echo installing\necho 'npm ERR! offline' >&2\nexit 3\n",
    )
    .unwrap();

    let (created, worktree) = create_workset_in(&temp, &project, &event_hub);

    assert_eq!(created.setup.len(), 1);
    let setup = &created.setup[0];
    assert!(!setup.success);
    for expected in ["installing", "npm ERR! offline", "退出码 3"] {
        assert!(setup.output.contains(expected), "{}", setup.output);
    }
    assert!(worktree.exists());
    assert_eq!(
        workspace_catalog::directory_on(&temp.store)
            .unwrap()
            .worksets
            .len(),
        1
    );
}

#[test]
fn a_workset_without_a_setup_script_reports_nothing() {
    let temp = TempStore::new();
    let event_hub = new_event_hub();
    let project = project_with_remote(&temp);

    let (created, worktree) = create_workset_in(&temp, &project, &event_hub);

    assert!(created.setup.is_empty());
    assert!(worktree.exists());
}
