//! 远程会话目录与对话改名 op。

use super::super::*;
use std::os::unix::net::UnixStream;

pub(super) fn handle_remote_sessions_op(conn: UnixStream, remote_sessions: &RemoteSessions) {
    let mut c = conn;
    let response = match remote_session_snapshot(remote_sessions) {
        Ok(snapshot) => serde_json::json!({ "ok": true, "remote_sessions": snapshot }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = smelt_runtime_client::protocol::write_json_line(&mut c, &response);
}

pub(super) fn handle_conversation_rename(
    conn: UnixStream,
    v: &serde_json::Value,
    acp_sessions: &AcpSessions,
    remote_sessions: &RemoteSessions,
    event_hub: &EventHubHandle,
) {
    let id = v["id"].as_str().unwrap_or_default();
    let title = v["title"].as_str().unwrap_or_default();
    let result = (|| -> Result<smelt_core::session_control::AgentConversationSummary, String> {
        let (renamed, snapshot) = mutate_remote_catalog(remote_sessions, |catalog| {
            catalog.rename_acp_by_id(id, title)
        })?;
        broadcast_remote_sessions(event_hub, &snapshot);
        if let Some(resume_id) = renamed.resume_id.as_deref().filter(|id| !id.is_empty()) {
            if let Some(kind) = smelt_core::agent_kind::HistorySourceKind::from_id(&renamed.agent) {
                let profile_id = renamed.agent_option_id.strip_prefix("profile:");
                smelt_core::session_metadata::set_custom_title(
                    kind,
                    profile_id,
                    resume_id,
                    Some(renamed.title.as_str()),
                )?;
            }
        }
        let _ = smelt_core::session_control::persist_workspace_conversation_title(
            &renamed.id,
            &renamed.title,
        );
        acp_host::try_rename_running_acp_session(acp_sessions, &renamed.id, &renamed.title);
        let agent_definition_id = renamed.agent_definition_id().map(str::to_string);
        Ok(smelt_core::session_control::AgentConversationSummary {
            session_id: renamed.id,
            title: renamed.title,
            agent_definition_id,
            cwd: renamed.cwd,
        })
    })();
    let mut c = conn;
    let response = match result {
        Ok(conversation) => serde_json::json!({ "ok": true, "conversation": conversation }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = smelt_runtime_client::protocol::write_json_line(&mut c, &response);
}

pub(super) fn handle_remote_rename_resume(
    conn: UnixStream,
    v: &serde_json::Value,
    remote_sessions: &RemoteSessions,
    event_hub: &EventHubHandle,
) {
    let agent_option_id = v["agent_option_id"].as_str().unwrap_or_default();
    let resume_id = v["resume_id"].as_str().unwrap_or_default();
    let title = v["title"].as_str().unwrap_or_default();
    let result = if agent_option_id.is_empty() || resume_id.is_empty() {
        Err("missing agent_option_id or resume_id".to_string())
    } else {
        mutate_remote_catalog(remote_sessions, |catalog| {
            catalog.rename_acp_by_resume_id(agent_option_id, resume_id, title)
        })
        .map(|(changed, snapshot)| {
            if changed {
                broadcast_remote_sessions(event_hub, &snapshot);
            }
        })
    };
    let mut c = conn;
    let response = match result {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = smelt_runtime_client::protocol::write_json_line(&mut c, &response);
}
