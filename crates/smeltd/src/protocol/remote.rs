//! 对话改名 op（命令行、Pi 工具的 `smelt agent conversation-rename`）。

use super::super::*;

pub(super) fn handle_conversation_rename<C: ClientSocket>(
    conn: C,
    v: &serde_json::Value,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    event_hub: &EventHubHandle,
) {
    let id = v["id"].as_str().unwrap_or_default();
    let title = v["title"].as_str().unwrap_or_default().trim();
    let result = (|| -> Result<smelt_core::session_control::AgentConversationSummary, String> {
        if title.is_empty() {
            return Err("对话标题不能为空".to_string());
        }
        let store = crate::workspace_directory::default_store().map_err(|error| error.message)?;
        let directory = smelt_core::workspace_catalog::directory_on(&store)
            .map_err(|error| error.to_string())?;
        let acp = directory
            .conversations
            .into_iter()
            .find_map(|conversation| conversation.acp.filter(|acp| acp.sid == id))
            .ok_or_else(|| format!("对话不存在：{id}"))?;
        crate::workspace_directory::rename_acp_conversation(
            &store,
            id,
            Some(title),
            acp_sessions,
            event_hub,
        )
        .map_err(|error| error.message)?;
        crate::terminal_names::refresh_all(sessions, event_hub);
        Ok(smelt_core::session_control::AgentConversationSummary {
            session_id: acp.sid,
            title: title.to_string(),
            agent_definition_id: acp.agent_definition_id,
            cwd: acp.cwd.unwrap_or_default(),
        })
    })();
    let mut c = conn;
    let response = match result {
        Ok(conversation) => serde_json::json!({ "ok": true, "conversation": conversation }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = smelt_runtime_client::protocol::write_json_line(&mut c, &response);
}
