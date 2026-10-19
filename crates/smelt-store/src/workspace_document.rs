//! `workspace.load` / `workspace.save` 线上的工作区文档与类型化快照互转。
//!
//! 文档只是传输格式，落库一律走 [`crate::workspace`] 的关系表。只认当前客户端写出的
//! 格式；界面状态（侧栏、当前页、会话 route 等）归客户端，读到也不收。

use crate::{
    StoreError, WorkspaceAcpRecord, WorkspaceSessionRecord, WorkspaceSnapshot,
    WorkspaceTerminalRecord,
};
use serde_json::{Map, Value, json};
use std::collections::HashSet;

pub(crate) fn workspace_from_document(value: &Value) -> Result<WorkspaceSnapshot, StoreError> {
    let mut projects = Vec::new();
    let mut seen = HashSet::new();
    for root in string_list(value.get("projects")) {
        if !root.trim().is_empty() && seen.insert(root.clone()) {
            projects.push(root);
        }
    }
    let mut sessions = Vec::new();
    let mut seen_ids = HashSet::new();
    for (position, session) in value
        .get("sessions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let session = session_from_value(session);
        if session.id.trim().is_empty() {
            return Err(StoreError::ConversationMissingId { position });
        }
        if !seen_ids.insert(session.id.clone()) {
            return Err(StoreError::DuplicateConversationId { id: session.id });
        }
        sessions.push(session);
    }
    Ok(WorkspaceSnapshot { projects, sessions })
}

pub(crate) fn json_from_workspace_snapshot(snapshot: &WorkspaceSnapshot) -> Value {
    let sessions = snapshot
        .sessions
        .iter()
        .map(|session| {
            json!({
                "id": session.id,
                "custom_title": session.custom_title,
                "last_updated_at": session.last_updated_at,
                "terminals": session.terminals.iter().map(terminal_to_value).collect::<Vec<_>>(),
                "acp": session.acp.as_ref().map(acp_to_value),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "projects": snapshot.projects,
        "sessions": sessions,
    })
}

fn session_from_value(value: &Value) -> WorkspaceSessionRecord {
    WorkspaceSessionRecord {
        id: as_str(value.get("id")).unwrap_or_default().to_string(),
        custom_title: as_str(value.get("custom_title")).map(str::to_string),
        last_updated_at: value
            .get("last_updated_at")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        terminals: value
            .get("terminals")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(terminal_from_value)
            .collect(),
        acp: value
            .get("acp")
            .and_then(Value::as_object)
            .map(acp_from_object),
    }
}

fn terminal_from_value(value: &Value) -> WorkspaceTerminalRecord {
    let text = |key: &str| as_str(value.get(key)).map(str::to_string);
    WorkspaceTerminalRecord {
        id: text("id"),
        cwd: text("cwd"),
        custom_title: text("custom_title"),
        launch_label: text("launch_label"),
        launch_cmd: text("launch_cmd"),
    }
}

fn terminal_to_value(terminal: &WorkspaceTerminalRecord) -> Value {
    json!({
        "id": terminal.id,
        "cwd": terminal.cwd,
        "custom_title": terminal.custom_title,
        "launch_label": terminal.launch_label,
        "launch_cmd": terminal.launch_cmd,
    })
}

fn acp_from_object(acp: &Map<String, Value>) -> WorkspaceAcpRecord {
    let text = |key: &str| as_str(acp.get(key)).map(str::to_string);
    let launch = acp.get("launch");
    let mut launch_env = launch
        .and_then(|launch| launch.get("env"))
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_string())))
        .collect::<Vec<_>>();
    launch_env.sort();
    let config_values = acp
        .get("config_values")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|pair| {
            Some((
                pair.get(0)?.as_str()?.to_string(),
                pair.get(1)?.as_str()?.to_string(),
            ))
        })
        .collect();
    let binding = acp.get("conversation_binding");
    let binding_automation_run_id = (as_str(binding.and_then(|b| b.get("type")))
        == Some("automation"))
    .then(|| as_str(binding.and_then(|b| b.get("run_id"))).map(str::to_string))
    .flatten();
    WorkspaceAcpRecord {
        cwd: text("cwd"),
        sid: text("sid"),
        agent: text("agent").unwrap_or_else(|| "claude".to_string()),
        profile_id: text("profile_id"),
        history_session_id: text("history_session_id"),
        launch_command: as_str(launch.and_then(|launch| launch.get("command")))
            .unwrap_or_default()
            .to_string(),
        launch_env,
        refresh_launch_from_settings: acp
            .get("refresh_launch_from_settings")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        pending_prompt: text("pending_prompt"),
        pending_delivery_id: text("pending_delivery_id"),
        agent_definition_id: text("agent_definition_id"),
        automation_id: text("automation_id"),
        config_values,
        binding_automation_run_id,
        pending_agent_preset: text("pending_agent_preset"),
        session_title: text("session_title"),
    }
}

fn acp_to_value(acp: &WorkspaceAcpRecord) -> Value {
    let env = acp
        .launch_env
        .iter()
        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
        .collect::<Map<_, _>>();
    let conversation_binding = match &acp.binding_automation_run_id {
        Some(run_id) => json!({"type": "automation", "run_id": run_id}),
        None => json!({"type": "direct"}),
    };
    json!({
        "cwd": acp.cwd,
        "sid": acp.sid,
        "agent": acp.agent,
        "profile_id": acp.profile_id,
        "history_session_id": acp.history_session_id,
        "launch": {"command": acp.launch_command, "env": env},
        "refresh_launch_from_settings": acp.refresh_launch_from_settings,
        "conversation_binding": conversation_binding,
        "config_values": acp
            .config_values
            .iter()
            .map(|(config_id, value_id)| json!([config_id, value_id]))
            .collect::<Vec<_>>(),
        "pending_prompt": acp.pending_prompt,
        "pending_delivery_id": acp.pending_delivery_id,
        "pending_agent_preset": acp.pending_agent_preset,
        "session_title": acp.session_title,
        "agent_definition_id": acp.agent_definition_id,
        "automation_id": acp.automation_id,
    })
}

fn as_str(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str)
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}
