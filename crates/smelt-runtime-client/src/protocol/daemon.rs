//! smeltd 首帧 JSON 协议的共享信封。
//!
//! 各操作的 payload 仍由对应 handler 拥有；这里只收拢所有客户端都必须共享的
//! `op` 名称，避免 GUI、远程网关和 daemon 各自维护字符串常量。

/// smeltd 支持的操作名。序列化值是稳定的 wire contract。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonOperation {
    Control,
    Open,
    Watch,
    EventSubscribe,
    AcpOpen,
    AcpCreate,
    AcpWatch,
    AcpSnapshot,
    AcpSkills,
    AcpSubmitInput,
    AcpAction,
    AcpKill,
    AcpRestart,
    List,
    RemoteSessions,
    HistoryRename,
    WorkspaceMenu,
    AutomationsSnapshot,
    AutomationCommand,
    EventPublish,
    RemoteRenameResume,
    ConversationRename,
    Kill,
    Upgrade,
    Version,
    Shutdown,
    AgentEvent,
    Action,
    Input,
    Resize,
    /// 远程桌面：转发到运行时本机 VNC 的原始字节流，见 [`super::desktop`]。
    DesktopAttach,
}

/// 首帧只先解析操作名，其余字段原样留给对应 handler。
#[derive(Debug, serde::Deserialize)]
pub struct DaemonRequest {
    #[serde(rename = "op")]
    pub operation: DaemonOperation,
    #[serde(flatten)]
    pub payload: serde_json::Map<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_names_are_a_typed_wire_contract() {
        let operations = [
            (DaemonOperation::Control, "control"),
            (DaemonOperation::Open, "open"),
            (DaemonOperation::Watch, "watch"),
            (DaemonOperation::EventSubscribe, "event_subscribe"),
            (DaemonOperation::AcpOpen, "acp_open"),
            (DaemonOperation::AcpCreate, "acp_create"),
            (DaemonOperation::AcpWatch, "acp_watch"),
            (DaemonOperation::AcpSnapshot, "acp_snapshot"),
            (DaemonOperation::AcpSkills, "acp_skills"),
            (DaemonOperation::AcpSubmitInput, "acp_submit_input"),
            (DaemonOperation::AcpAction, "acp_action"),
            (DaemonOperation::AcpKill, "acp_kill"),
            (DaemonOperation::AcpRestart, "acp_restart"),
            (DaemonOperation::List, "list"),
            (DaemonOperation::RemoteSessions, "remote_sessions"),
            (DaemonOperation::HistoryRename, "history_rename"),
            (DaemonOperation::WorkspaceMenu, "workspace_menu"),
            (DaemonOperation::AutomationsSnapshot, "automations_snapshot"),
            (DaemonOperation::AutomationCommand, "automation_command"),
            (DaemonOperation::EventPublish, "event_publish"),
            (DaemonOperation::RemoteRenameResume, "remote_rename_resume"),
            (DaemonOperation::ConversationRename, "conversation_rename"),
            (DaemonOperation::Kill, "kill"),
            (DaemonOperation::Upgrade, "upgrade"),
            (DaemonOperation::Version, "version"),
            (DaemonOperation::Shutdown, "shutdown"),
            (DaemonOperation::AgentEvent, "agent_event"),
            (DaemonOperation::Action, "action"),
            (DaemonOperation::Input, "input"),
            (DaemonOperation::Resize, "resize"),
            (DaemonOperation::DesktopAttach, "desktop_attach"),
        ];

        for (operation, wire_name) in operations {
            assert_eq!(serde_json::to_value(operation).unwrap(), wire_name);
            let request: DaemonRequest = serde_json::from_value(serde_json::json!({
                "op": wire_name,
                "sentinel": 7,
            }))
            .unwrap_or_else(|error| panic!("{wire_name}: {error}"));
            assert_eq!(request.operation, operation);
            assert_eq!(request.payload["sentinel"], 7, "{wire_name}");
        }
    }

    #[test]
    fn unknown_operation_is_rejected() {
        let result = serde_json::from_value::<DaemonRequest>(serde_json::json!({
            "op": "not_a_real_operation"
        }));
        assert!(result.is_err());
    }
}
