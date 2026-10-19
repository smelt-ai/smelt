//! 跨 agent 消息的端点和消息格式。

use serde::{Deserialize, Serialize};

pub const MAX_AGENT_MESSAGE_BYTES: usize = 64 * 1024;

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
