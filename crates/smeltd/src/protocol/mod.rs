//! Unix socket 协议分发：`handle_conn` 以及 open/watch/subscribe。
//!
//! 从 `main.rs` 整块搬出；ACP 相关 op 转给 `acp_host`。
//! 身份校验、终端流、远程/iroh、订阅与插件 UI 分文件，这里只做首帧分流。

use super::*;
use smelt_core::daemon_protocol::{DaemonOperation, DaemonRequest};

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

/// smeltd 连接分发所需的全部服务状态。`handle_conn` 参数太多，收成一组；
/// 函数开头解构回局部变量，分发体本身不用改。
pub(crate) struct ServerContext {
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
    pub(crate) remote_state: RemoteState,
    pub(crate) iroh_state: IrohState,
    pub(crate) iroh_connections: IrohConnections,
    pub(crate) event_hub: EventHubHandle,
}

pub(crate) fn handle_conn(conn: UnixStream, ctx: ServerContext) {
    let ServerContext {
        sessions,
        acp_sessions,
        remote_sessions,
        workspace_menu,
        automations,
        exe_mtime,
        daemon_fingerprint,
        listen_fd,
        remote_state,
        iroh_state,
        iroh_connections,
        event_hub,
    } = ctx;
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
    match operation {
        DaemonOperation::Control => control::handle_control(
            conn,
            v,
            &sessions,
            &acp_sessions,
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
        DaemonOperation::EventSubscribe => handle_event_subscribe_op(conn, &v, &event_hub),
        DaemonOperation::PluginContributions => {
            let identity = authenticate_desktop_connection(&v, &conn);
            handle_plugin_contributions(conn, identity);
        }
        DaemonOperation::PluginStatuses => {
            let identity = authenticate_desktop_connection(&v, &conn);
            handle_plugin_statuses(conn, identity);
        }
        DaemonOperation::PluginInvoke => {
            let identity = authenticate_desktop_connection(&v, &conn);
            handle_plugin_invoke(conn, &v, identity);
        }
        DaemonOperation::PluginSetEnabled => {
            let identity = authenticate_desktop_connection(&v, &conn);
            handle_plugin_set_enabled(conn, &v, identity);
        }
        DaemonOperation::PluginReload => {
            let identity = authenticate_desktop_connection(&v, &conn);
            handle_plugin_reload(conn, identity);
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
        DaemonOperation::AcpWatch => handle_acp_watch(conn, reader, &v, acp_sessions),
        DaemonOperation::AcpSnapshot => handle_acp_snapshot(conn, &v, &acp_sessions),
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
        DaemonOperation::Kill => handle_kill(conn, &v, &sessions, &remote_sessions, &event_hub),
        DaemonOperation::Upgrade => {
            handle_upgrade(conn, &v, &sessions, &acp_sessions, &event_hub, listen_fd)
        }
        DaemonOperation::Version => {
            handle_version(conn, &sessions, exe_mtime, daemon_fingerprint.as_deref())
        }
        DaemonOperation::Shutdown => handle_shutdown(conn, &event_hub),
        DaemonOperation::RemoteStart => handle_remote_start(conn, &v, &remote_state),
        DaemonOperation::RemoteStop => handle_remote_stop(conn, &remote_state),
        DaemonOperation::RemoteSetWrite => handle_remote_set_write(conn, &v, &remote_state),
        DaemonOperation::RemoteRotateToken => {
            handle_remote_rotate_token(conn, &remote_state, &iroh_state)
        }
        DaemonOperation::RemoteStatus => handle_remote_status(conn, &remote_state),
        DaemonOperation::IrohStart => {
            handle_iroh_start(conn, &v, &iroh_state, &remote_state, &iroh_connections)
        }
        DaemonOperation::IrohStop => handle_iroh_stop(conn, &iroh_state, &remote_state),
        DaemonOperation::IrohStatus => handle_iroh_status(conn, &iroh_state, &remote_state),
        DaemonOperation::IrohConnections => handle_iroh_connections(conn, &iroh_state),
        DaemonOperation::AgentEvent => handle_agent_event_op(conn, &v, &sessions, &event_hub),
        DaemonOperation::Action => handle_action(conn, &v, &sessions),
        DaemonOperation::Input => handle_raw_input(conn, &v, &sessions),
        DaemonOperation::Resize => handle_resize(conn, &v, &sessions),
    }
}

#[cfg(test)]
mod tests;
