//! Unix socket 协议分发：`handle_conn` 以及 open/watch/subscribe。
//!
//! 从 `main.rs` 整块搬出；ACP 相关 op 转给 `acp_host`。
//! 身份校验、终端流、远程/iroh、订阅分文件，这里只做首帧分流。

use super::*;
use crate::devices::Principal;
use smelt_runtime_client::protocol::control::ControlMethod;
use smelt_runtime_client::protocol::daemon::{DaemonOperation, DaemonRequest};

mod auth;
mod lifecycle;
mod remote;
mod subscribe;
mod terminal;

use lifecycle::*;
use remote::*;

pub(crate) use auth::*;
pub(crate) use subscribe::*;
pub(crate) use terminal::*;

/// 进程级的服务状态。unix socket 和 iroh 两种传输的每条连接都从它取一份
/// [`ServerContext`]，交给同一个 [`handle_conn`]。
#[derive(Clone)]
pub(crate) struct ServerShared {
    pub(crate) sessions: Sessions,
    pub(crate) acp_sessions: AcpSessions,
    pub(crate) remote_sessions: RemoteSessions,
    pub(crate) workspace_menu: WorkspaceMenuStore,
    pub(crate) automations: AutomationStore,
    pub(crate) exe_mtime: u64,
    /// 启动时钉死的进程指纹（version 回给 GUI 判 outdated 用）。
    /// 内容哈希，不随 StageDiskOnly 变化；None=启动时文件已消失的孤儿进程。
    pub(crate) daemon_fingerprint: Option<String>,
    pub(crate) listen_fd: RawFd,
    pub(crate) event_hub: EventHubHandle,
}

impl ServerShared {
    pub(crate) fn context(&self, principal: Principal) -> ServerContext {
        ServerContext {
            shared: self.clone(),
            principal,
        }
    }
}

/// accept 循环启动时登记；iroh 流从这里取服务状态。
pub(crate) static SERVER: std::sync::OnceLock<ServerShared> = std::sync::OnceLock::new();

/// 一条连接的分发上下文：服务状态加上对端是谁。
pub(crate) struct ServerContext {
    pub(crate) shared: ServerShared,
    pub(crate) principal: Principal,
}

/// 未配对的远程设备只能提交配对码（设计文档 D3），连 `system.describe` 都不行。
fn unpaired_may_call(operation: DaemonOperation, payload: &serde_json::Value) -> bool {
    operation == DaemonOperation::Control
        && payload.get("method").and_then(serde_json::Value::as_str)
            == Some(ControlMethod::DevicePair.as_str())
}

pub(crate) fn handle_conn(conn: UnixStream, ctx: ServerContext) {
    let ServerContext { shared, principal } = ctx;
    let ServerShared {
        sessions,
        acp_sessions,
        remote_sessions,
        workspace_menu,
        automations,
        exe_mtime,
        daemon_fingerprint,
        listen_fd,
        event_hub,
    } = shared;
    // 头一行 JSON。之后的帧字节可能已被 BufReader 预读，故帧循环必须复用同一个 reader。
    let Ok(rc) = conn.try_clone() else { return };
    let mut reader = BufReader::new(rc);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let Ok(request) = serde_json::from_str::<DaemonRequest>(&line) else {
        return;
    };
    let operation = request.operation;
    let v = serde_json::Value::Object(request.payload);
    if matches!(principal, Principal::Unpaired(_)) && !unpaired_may_call(operation, &v) {
        reject_unpaired(conn, operation, &v);
        return;
    }
    match operation {
        DaemonOperation::Control => control::handle_control(
            conn,
            v,
            &principal,
            &sessions,
            &acp_sessions,
            &remote_sessions,
            &workspace_menu,
            &automations,
            &event_hub,
            exe_mtime,
        ),
        DaemonOperation::Open => handle_open(
            conn,
            reader,
            &v,
            sessions,
            Arc::clone(&acp_sessions),
            Arc::clone(&event_hub),
            Arc::clone(&remote_sessions),
        ),
        DaemonOperation::Watch => handle_watch(conn, reader, &v, sessions),
        DaemonOperation::EventSubscribe => {
            handle_event_subscribe_op(conn, &v, &principal, &event_hub)
        }
        DaemonOperation::AcpOpen => {
            handle_acp_open(conn, reader, &v, sessions, acp_sessions, event_hub)
        }
        DaemonOperation::AcpCreate => handle_acp_create(
            conn,
            &v,
            &sessions,
            &acp_sessions,
            &event_hub,
            &remote_sessions,
        ),
        DaemonOperation::AcpWatch => {
            handle_acp_watch(conn, reader, &v, acp_sessions, &workspace_menu, &event_hub)
        }
        DaemonOperation::AcpSnapshot => handle_acp_snapshot(conn, &v, &acp_sessions, &event_hub),
        DaemonOperation::AcpSkills => handle_acp_skills(conn, &v, &acp_sessions),
        DaemonOperation::AcpSubmitInput => {
            handle_acp_submit_input(conn, &v, &acp_sessions, &event_hub)
        }
        DaemonOperation::AcpAction => handle_acp_action(conn, &v, &acp_sessions, &event_hub),
        DaemonOperation::AcpKill => {
            handle_acp_kill(conn, &v, &acp_sessions, &remote_sessions, &event_hub)
        }
        DaemonOperation::AcpRestart => handle_acp_restart(conn, &v, &acp_sessions, &event_hub),
        DaemonOperation::List => handle_list(conn, &sessions, &acp_sessions),
        DaemonOperation::RemoteSessions => handle_remote_sessions_op(conn, &remote_sessions),
        DaemonOperation::HistoryRename => {
            handle_history_rename(conn, &v, &remote_sessions, &event_hub)
        }
        DaemonOperation::WorkspaceMenu => {
            handle_workspace_menu(conn, &v, &workspace_menu, &event_hub)
        }
        DaemonOperation::AutomationsSnapshot => {
            handle_automations_snapshot(conn, &automations, &event_hub)
        }
        DaemonOperation::AutomationCommand => {
            handle_automation_command(conn, &v, &automations, &event_hub)
        }
        DaemonOperation::EventPublish => handle_event_publish(conn, &v, &automations, &event_hub),
        DaemonOperation::RemoteRenameResume => {
            handle_remote_rename_resume(conn, &v, &remote_sessions, &event_hub)
        }
        DaemonOperation::ConversationRename => {
            handle_conversation_rename(conn, &v, &acp_sessions, &remote_sessions, &event_hub)
        }
        DaemonOperation::Kill => handle_kill(conn, &v, &sessions, &remote_sessions, &event_hub),
        DaemonOperation::Upgrade => {
            handle_upgrade(conn, &v, &sessions, &acp_sessions, &event_hub, listen_fd)
        }
        DaemonOperation::Version => {
            handle_version(conn, &sessions, exe_mtime, daemon_fingerprint.as_deref())
        }
        DaemonOperation::Shutdown => handle_shutdown(conn, &event_hub),
        DaemonOperation::AgentEvent => handle_agent_event_op(conn, &v, &sessions, &event_hub),
        DaemonOperation::Action => handle_action(conn, &v, &sessions),
        DaemonOperation::Input => handle_raw_input(conn, &v, &sessions),
        DaemonOperation::Resize => handle_resize(conn, &v, &sessions),
        DaemonOperation::DesktopAttach => {
            crate::desktop::handle_desktop_attach(conn, reader, &crate::desktop::vnc_endpoint())
        }
        DaemonOperation::RuntimeUpgrade => {
            crate::runtime_upgrade::handle_runtime_upgrade(conn, &v)
        }
    }
}

/// 控制请求回一个带 request_id 的 PermissionDenied，客户端能看懂；其余 op 直接断开。
fn reject_unpaired(mut conn: UnixStream, operation: DaemonOperation, payload: &serde_json::Value) {
    if operation != DaemonOperation::Control {
        return;
    }
    let request_id = payload
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let version = payload
        .get("control_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|version| u16::try_from(version).ok())
        .unwrap_or(smelt_runtime_client::protocol::control::CONTROL_API_VERSION);
    let response =
        smelt_runtime_client::protocol::control::ControlResponse::<()>::error_with_version(
            version,
            request_id,
            smelt_runtime_client::protocol::control::ControlError::new(
                smelt_runtime_client::protocol::control::ControlErrorCode::PermissionDenied,
                "这台设备还没有配对",
            ),
        );
    let _ = smelt_runtime_client::protocol::write_json_line(&mut conn, &response);
}

#[cfg(test)]
mod tests;
