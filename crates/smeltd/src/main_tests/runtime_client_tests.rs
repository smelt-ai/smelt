//! 协议一致性：用客户端真正用的 `smelt_runtime_client` 打真正的分发器 `handle_conn`。
//!
//! 连接器每次给出一对 socket，另一端在新线程里交给 `handle_conn`，和运行时以后接
//! iroh 流的方式相同。`event_subscribe` 要校验对端进程身份，进程内 socketpair 过不了，
//! 由 `protocol/tests.rs` 用假身份提供者覆盖。

use super::*;
use smelt_core::control_api::SessionList;
use smelt_runtime_client::protocol::control::{
    CONTROL_API_VERSION, ControlClientError, ControlErrorCode, EmptyParams,
    SUPPORTED_CONTROL_METHODS,
};
use smelt_runtime_client::protocol::daemon::DaemonOperation;
use smelt_runtime_client::{DEFAULT_REQUEST_TIMEOUT, RequestError, RuntimeClient};

const TEST_EXE_MTIME: u64 = 4242;

fn in_process_runtime() -> RuntimeClient {
    let sessions = new_sessions();
    let acp_sessions = new_test_acp_sessions();
    let remote_sessions = new_test_remote_sessions();
    let workspace_menu = new_test_workspace_menu();
    let automations = new_test_automation_store();
    let remote_state = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
    let iroh_state = Arc::new(Mutex::new(None));
    let iroh_connections = new_iroh_connections();
    let event_hub = new_event_hub();
    RuntimeClient::new(move || {
        let (client, server) = UnixStream::pair()?;
        let context = ServerContext {
            sessions: Arc::clone(&sessions),
            acp_sessions: Arc::clone(&acp_sessions),
            remote_sessions: Arc::clone(&remote_sessions),
            workspace_menu: Arc::clone(&workspace_menu),
            automations: Arc::clone(&automations),
            exe_mtime: TEST_EXE_MTIME,
            daemon_fingerprint: None,
            listen_fd: -1,
            remote_state: Arc::clone(&remote_state),
            iroh_state: Arc::clone(&iroh_state),
            iroh_connections: Arc::clone(&iroh_connections),
            event_hub: Arc::clone(&event_hub),
        };
        thread::spawn(move || handle_conn(server, context));
        Ok(client)
    })
}

#[test]
fn describe_reports_protocol_version_methods_and_platform() {
    let result = in_process_runtime().describe().unwrap();

    assert_eq!(result.control.current_version, CONTROL_API_VERSION);
    assert_eq!(
        result.control.methods,
        SUPPORTED_CONTROL_METHODS
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    );
    assert_eq!(result.daemon.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(result.daemon.exe_mtime, TEST_EXE_MTIME);
    assert_eq!(result.daemon.os.as_deref(), Some(std::env::consts::OS));
    assert_eq!(result.daemon.arch.as_deref(), Some(std::env::consts::ARCH));
    // 测试进程不开 SQLite 路由，运行时身份读不到；持久化由 runtime_identity 的单测覆盖。
    assert_eq!(result.daemon.runtime_id, None);
}

#[test]
fn typed_control_call_round_trips_through_the_dispatcher() {
    let result = in_process_runtime()
        .call::<SessionList>(&EmptyParams::default(), DEFAULT_REQUEST_TIMEOUT)
        .unwrap();

    assert!(result.sessions.is_empty());
}

#[test]
fn control_version_outside_the_supported_range_is_a_structured_error() {
    let error = in_process_runtime()
        .call_with_version::<SessionList>(
            CONTROL_API_VERSION + 1,
            &EmptyParams::default(),
            DEFAULT_REQUEST_TIMEOUT,
        )
        .unwrap_err();

    let ControlClientError::Protocol(error) = error else {
        panic!("expected a protocol error, got {error}");
    };
    assert_eq!(error.code, ControlErrorCode::UnsupportedVersion);
}

#[test]
fn first_frame_request_returns_the_handler_response() {
    let response = in_process_runtime()
        .request(
            &serde_json::json!({ "op": DaemonOperation::Version }),
            DEFAULT_REQUEST_TIMEOUT,
        )
        .unwrap();

    assert_eq!(response["exe_mtime"], TEST_EXE_MTIME);
    assert_eq!(response["pid"], std::process::id());
}

/// 运行时不认识的 op 直接断开、不回包。客户端据此判断「老运行时不支持」，
/// 所以这个行为本身是契约。
#[test]
fn unknown_operation_closes_without_a_response() {
    let error = in_process_runtime()
        .request(
            &serde_json::json!({ "op": "not_a_real_operation" }),
            DEFAULT_REQUEST_TIMEOUT,
        )
        .unwrap_err();

    assert!(matches!(error, RequestError::Closed), "{error}");
}

fn git_repo_with_change(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "smeltd-git-{tag}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(root.join("f.txt"), "before\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "init"]);
    std::fs::write(root.join("f.txt"), "after\n").unwrap();
    root
}

#[test]
fn git_status_runs_on_the_runtime() {
    use smelt_runtime_client::protocol::git::{GitRootParams, GitStatusCall};
    let root = git_repo_with_change("status");

    let status = in_process_runtime()
        .call::<GitStatusCall>(
            &GitRootParams {
                root: root.to_string_lossy().into_owned(),
            },
            DEFAULT_REQUEST_TIMEOUT,
        )
        .unwrap();

    assert!(status.ok);
    assert_eq!(status.branch, "main");
    assert_eq!(status.files, vec![(" M".to_string(), "f.txt".to_string())]);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn git_failure_reaches_the_client_as_an_operation_error() {
    use smelt_runtime_client::protocol::git::{GitBranchParams, GitCheckoutCall};
    let root = git_repo_with_change("checkout");

    let error = in_process_runtime()
        .call::<GitCheckoutCall>(
            &GitBranchParams {
                root: root.to_string_lossy().into_owned(),
                branch: "no-such-branch".to_string(),
            },
            DEFAULT_REQUEST_TIMEOUT,
        )
        .unwrap_err();

    let ControlClientError::Protocol(error) = error else {
        panic!("expected a protocol error, got {error}");
    };
    assert_eq!(error.code, ControlErrorCode::OperationFailed);
    assert!(!error.message.is_empty(), "git 的失败原因要带回客户端");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn file_save_with_a_stale_version_is_a_conflict() {
    use smelt_runtime_client::protocol::files::{
        FileContent, FileOpenCall, FilePathParams, FileSaveCall, FileSaveParams,
    };
    let dir = std::env::temp_dir().join(format!("smeltd-file-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("a.txt");
    std::fs::write(&file, "v1").unwrap();
    let path = file.to_string_lossy().into_owned();
    let runtime = in_process_runtime();

    let opened = runtime
        .call::<FileOpenCall>(
            &FilePathParams { path: path.clone() },
            DEFAULT_REQUEST_TIMEOUT,
        )
        .unwrap();
    let FileContent::Text { text, version } = opened.content else {
        panic!("应作为文本打开");
    };
    assert_eq!(text, "v1");

    std::fs::write(&file, "changed elsewhere").unwrap();
    let error = runtime
        .call::<FileSaveCall>(
            &FileSaveParams {
                path,
                text: "mine".to_string(),
                base_version: Some(version),
            },
            DEFAULT_REQUEST_TIMEOUT,
        )
        .unwrap_err();
    let ControlClientError::Protocol(error) = error else {
        panic!("expected a protocol error, got {error}");
    };
    assert_eq!(error.code, ControlErrorCode::Conflict);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "changed elsewhere");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn project_tree_rejects_a_directory_outside_the_project() {
    use smelt_runtime_client::protocol::files::{ProjectDirParams, ProjectTreeCall};
    let error = in_process_runtime()
        .call::<ProjectTreeCall>(
            &ProjectDirParams {
                root: "/no/such/project".to_string(),
                dir: "/etc".to_string(),
            },
            DEFAULT_REQUEST_TIMEOUT,
        )
        .unwrap_err();

    let ControlClientError::Protocol(error) = error else {
        panic!("expected a protocol error, got {error}");
    };
    assert_eq!(error.code, ControlErrorCode::InvalidParams);
}

#[test]
fn history_list_rejects_an_unknown_source_and_lists_an_empty_project() {
    use smelt_runtime_client::protocol::history::{HistoryListCall, HistoryListParams};
    let runtime = in_process_runtime();

    let error = runtime
        .call::<HistoryListCall>(
            &HistoryListParams {
                agent: "no-such-agent".to_string(),
                profile_id: None,
                cwd: "/no/such/project".to_string(),
            },
            DEFAULT_REQUEST_TIMEOUT,
        )
        .unwrap_err();
    let ControlClientError::Protocol(error) = error else {
        panic!("expected a protocol error, got {error}");
    };
    assert_eq!(error.code, ControlErrorCode::InvalidParams);

    let listed = runtime
        .call::<HistoryListCall>(
            &HistoryListParams {
                agent: "antigravity".to_string(),
                profile_id: None,
                cwd: "/no/such/project".to_string(),
            },
            DEFAULT_REQUEST_TIMEOUT,
        )
        .unwrap();
    assert!(listed.sessions.is_empty());
}
