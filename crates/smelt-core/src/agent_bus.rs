use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const MAX_AGENT_MESSAGE_BYTES: usize = 64 * 1024;

/// 跨 Agent 通讯的用户偏好和 Agent UI 配置共用同一个文档。守护进程不能依赖
/// GPUI 的 `AgentHostState`，因此只读取这一项；缺失或损坏时保持开启，兼容功能
/// 上线前写入的旧配置。
pub fn cross_agent_enabled() -> bool {
    let Ok(store) = crate::sqlite_state::default_sqlite_store() else {
        return true;
    };
    match store.get_agent_ui_snapshot() {
        Ok(Some(snapshot)) => snapshot.cross_agent_enabled,
        Ok(None) | Err(_) => true,
    }
}

#[cfg(test)]
fn cross_agent_enabled_from_config(config: &serde_json::Value) -> bool {
    config
        .get("cross_agent_enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
}

pub fn mcp_executable_path() -> PathBuf {
    std::env::var_os("SMELT_AGENT_MCP_BIN")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .map(|path| path.with_file_name("smelt-agent-mcp"))
        })
        .unwrap_or_else(|| PathBuf::from("smelt-agent-mcp"))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentBackend {
    Acp,
    Terminal,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentEndpoint {
    pub session_id: String,
    pub backend: AgentBackend,
    pub provider: Option<String>,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub phase: String,
    pub available: bool,
}

pub fn validate_message(message: &str) -> Result<(), &'static str> {
    if message.trim().is_empty() {
        return Err("message must not be empty");
    }
    if message.len() > MAX_AGENT_MESSAGE_BYTES {
        return Err("message is too large");
    }
    Ok(())
}

pub fn render_delivery(source_session_id: &str, message_id: &str, message: &str) -> String {
    format!(
        "[Smelt peer message]\n\
         source_session: {source_session_id}\n\
         message_id: {message_id}\n\
         --- peer message ---\n\
         {message}\n\
         --- end peer message ---\n\
         If a response is needed, call the Smelt MCP tool `smelt_send_message` with `target` set to `{source_session_id}`. Do not answer only in chat because the peer cannot see chat-only text. Do not send acknowledgements for informational messages or replies that already answer your earlier request."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_message_bounds() {
        assert!(validate_message("hello").is_ok());
        assert!(validate_message("  ").is_err());
        assert!(validate_message(&"x".repeat(MAX_AGENT_MESSAGE_BYTES + 1)).is_err());
    }

    #[test]
    fn delivery_uses_the_same_tool_for_a_peer_reply() {
        let rendered = render_delivery("claude-1", "message-1", "review");
        assert!(rendered.contains("smelt_send_message"));
        assert!(rendered.contains("message-1"));
        assert!(rendered.contains("claude-1"));
        assert!(rendered.contains("review"));
    }

    #[test]
    fn future_agent_backends_do_not_break_endpoint_decoding() {
        let endpoint: AgentEndpoint = serde_json::from_value(serde_json::json!({
            "session_id": "future-1",
            "backend": "future_backend",
            "provider": null,
            "title": null,
            "cwd": null,
            "phase": "idle",
            "available": true
        }))
        .expect("an older client should retain endpoints from a newer backend");

        assert_eq!(endpoint.backend, AgentBackend::Unknown);
    }

    #[test]
    fn cross_agent_setting_defaults_to_enabled() {
        assert!(cross_agent_enabled_from_config(&serde_json::json!({})));
        assert!(cross_agent_enabled_from_config(&serde_json::json!({
            "cross_agent_enabled": true
        })));
        assert!(!cross_agent_enabled_from_config(&serde_json::json!({
            "cross_agent_enabled": false
        })));
        assert!(cross_agent_enabled_from_config(&serde_json::json!({
            "cross_agent_enabled": "invalid"
        })));
    }
}
