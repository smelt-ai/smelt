//! 智能体配置、远程会话、守护会话目录的关系表读写。

use crate::{
    AgentAcpConfigMemoryRecord, AgentAcpEnvRecord, AgentConversationCommandRecord,
    AgentDefinitionRecord, AgentProfileRecord, AgentUiSnapshot, PublishedSessionRecord,
    PublishedSessionSnapshot, RemoteAcpSessionRecord, RemoteSessionCatalogSnapshot,
    RemoteTerminalSessionRecord, StoreError,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const AGENT_UI_DOCUMENT: &str = "agent_ui.json";
const REMOTE_ACP_DOCUMENT: &str = "remote_acp_sessions.json";
const REMOTE_TERMINAL_DOCUMENT: &str = "remote_terminal_sessions.json";
const PUBLISHED_SESSION_DOCUMENT: &str = "sessions.json";

const AGENT_UI_RESERVED: &[&str] = &[
    "agent_hooks_enabled",
    "cross_agent_enabled",
    "notify_approval",
    "notify_awaiting",
    "notify_input",
    "notify_success",
    "notify_failure",
    "notify_terminal_bell",
    "acp_env",
    "acp_config_memory",
    "profiles",
    "agents",
    "agent_presets",
];

pub(crate) fn read_agent_ui_snapshot(
    connection: &Connection,
) -> Result<Option<AgentUiSnapshot>, StoreError> {
    if !agent_ui_exists(connection)? {
        return Ok(None);
    }
    Ok(Some(AgentUiSnapshot {
        agent_hooks_enabled: pref_bool(connection, "agent_hooks_enabled")?.unwrap_or(true),
        cross_agent_enabled: pref_bool(connection, "cross_agent_enabled")?.unwrap_or(true),
        notify_approval: pref_bool(connection, "notify_approval")?.unwrap_or(true),
        notify_input: pref_bool(connection, "notify_input")?.unwrap_or(true),
        notify_success: pref_bool(connection, "notify_success")?.unwrap_or(true),
        notify_failure: pref_bool(connection, "notify_failure")?.unwrap_or(true),
        notify_terminal_bell: pref_bool(connection, "notify_terminal_bell")?.unwrap_or(true),
        commands: query_commands(connection)?,
        env: query_env(connection)?,
        config_memory: query_config_memory(connection)?,
        agents: query_agents(connection)?,
        profiles: query_profiles(connection)?,
    }))
}

pub(crate) fn write_agent_ui_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &AgentUiSnapshot,
) -> Result<(), StoreError> {
    write_agent_ui_prefs(transaction, snapshot)?;
    replace_agent_definitions(transaction, &snapshot.agents)?;
    Ok(())
}

/// 写智能体界面偏好、启动命令和 profile，不动 `agent_definition`。
///
/// GUI 改通知开关时不能整表重写定义：否则并发的单行 create 会被内存里的旧清单盖掉。
pub(crate) fn write_agent_ui_prefs(
    transaction: &Transaction<'_>,
    snapshot: &AgentUiSnapshot,
) -> Result<(), StoreError> {
    transaction.execute("DELETE FROM agent_ui_pref", [])?;
    transaction.execute("DELETE FROM agent_acp_command", [])?;
    transaction.execute("DELETE FROM agent_acp_env", [])?;
    transaction.execute("DELETE FROM agent_acp_config_memory", [])?;
    transaction.execute("DELETE FROM agent_profile", [])?;
    crate::documents::delete_document_scope(transaction, AGENT_UI_DOCUMENT)?;

    upsert_pref(
        transaction,
        "agent_hooks_enabled",
        snapshot.agent_hooks_enabled,
    )?;
    upsert_pref(
        transaction,
        "cross_agent_enabled",
        snapshot.cross_agent_enabled,
    )?;
    upsert_pref(transaction, "notify_approval", snapshot.notify_approval)?;
    upsert_pref(transaction, "notify_input", snapshot.notify_input)?;
    upsert_pref(transaction, "notify_success", snapshot.notify_success)?;
    upsert_pref(transaction, "notify_failure", snapshot.notify_failure)?;
    upsert_pref(
        transaction,
        "notify_terminal_bell",
        snapshot.notify_terminal_bell,
    )?;
    for command in &snapshot.commands {
        transaction.execute(
            "INSERT INTO agent_acp_command(command_key, command) VALUES (?1, ?2)",
            params![command.command_key, command.command],
        )?;
    }
    for env in &snapshot.env {
        transaction.execute(
            "INSERT INTO agent_acp_env(engine_kind_id, name, value) VALUES (?1, ?2, ?3)",
            params![env.engine_kind_id, env.name, env.value],
        )?;
    }
    for (position, memory) in snapshot.config_memory.iter().enumerate() {
        transaction.execute(
            "INSERT INTO agent_acp_config_memory(engine_kind_id, config_id, value_id, position)
                 VALUES (?1, ?2, ?3, ?4)",
            params![
                memory.engine_kind_id,
                memory.config_id,
                memory.value_id,
                position as i64
            ],
        )?;
    }
    for (position, profile) in snapshot.profiles.iter().enumerate() {
        transaction.execute(
            "INSERT INTO agent_profile(id, kind_id, label, workspace_dir, position)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                profile.id,
                profile.kind_id,
                profile.label,
                profile.workspace_dir,
                position as i64,
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn replace_agent_definitions(
    transaction: &Transaction<'_>,
    agents: &[AgentDefinitionRecord],
) -> Result<(), StoreError> {
    transaction.execute("DELETE FROM agent_definition", [])?;
    for (position, agent) in agents.iter().enumerate() {
        insert_agent_definition_at(transaction, agent, position as i64)?;
    }
    Ok(())
}

fn insert_agent_definition_at(
    transaction: &Transaction<'_>,
    agent: &AgentDefinitionRecord,
    position: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO agent_definition(
               id, name, description, engine_kind_id, prompt, plugins_json,
               context_folders_json, context_links_json, model_provider, model_id, position
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            agent.id,
            agent.name,
            agent.description,
            agent.engine_kind_id,
            agent.prompt,
            &agent.plugins_json,
            &agent.context_folders_json,
            &agent.context_links_json,
            agent.model_provider,
            agent.model_id,
            position,
        ],
    )?;
    Ok(())
}

pub(crate) fn insert_agent_definition(
    transaction: &Transaction<'_>,
    agent: &AgentDefinitionRecord,
) -> Result<(), StoreError> {
    let next_position: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM agent_definition",
        [],
        |row| row.get(0),
    )?;
    insert_agent_definition_at(transaction, agent, next_position)
}

pub(crate) fn update_agent_definition(
    transaction: &Transaction<'_>,
    agent: &AgentDefinitionRecord,
) -> Result<bool, StoreError> {
    let changed = transaction.execute(
        "UPDATE agent_definition
             SET name = ?1,
                 description = ?2,
                 engine_kind_id = ?3,
                 prompt = ?4,
                 plugins_json = ?5,
                 context_folders_json = ?6,
                 context_links_json = ?7,
                 model_provider = ?8,
                 model_id = ?9
           WHERE id = ?10",
        params![
            agent.name,
            agent.description,
            agent.engine_kind_id,
            agent.prompt,
            &agent.plugins_json,
            &agent.context_folders_json,
            &agent.context_links_json,
            agent.model_provider,
            agent.model_id,
            agent.id,
        ],
    )?;
    Ok(changed > 0)
}

pub(crate) fn delete_agent_definition(
    transaction: &Transaction<'_>,
    id: &str,
) -> Result<bool, StoreError> {
    let changed = transaction.execute("DELETE FROM agent_definition WHERE id = ?1", [id])?;
    Ok(changed > 0)
}

pub(crate) fn agent_ui_from_document(value: &Value) -> Result<AgentUiSnapshot, StoreError> {
    let object = value.as_object();
    let reserved: BTreeSet<&str> = AGENT_UI_RESERVED.iter().copied().collect();
    let mut commands = Vec::new();
    if let Some(object) = object {
        for (key, item) in object {
            if reserved.contains(key.as_str()) {
                continue;
            }
            if let Some(command) = item.as_str() {
                commands.push(AgentConversationCommandRecord {
                    command_key: key.clone(),
                    command: command.to_string(),
                });
            }
        }
    }
    let mut env = Vec::new();
    if let Some(map) = value.get("acp_env").and_then(Value::as_object) {
        for (engine_kind_id, vars) in map {
            if let Some(vars) = vars.as_object() {
                for (name, item) in vars {
                    if let Some(item) = item.as_str() {
                        env.push(AgentAcpEnvRecord {
                            engine_kind_id: engine_kind_id.clone(),
                            name: name.clone(),
                            value: item.to_string(),
                        });
                    }
                }
            }
        }
    }
    let mut config_memory = Vec::new();
    if let Some(map) = value.get("acp_config_memory").and_then(Value::as_object) {
        for (engine_kind_id, pairs) in map {
            if let Some(pairs) = pairs.as_array() {
                for pair in pairs {
                    let config_id = pair
                        .get(0)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let value_id = pair
                        .get(1)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if config_id.is_empty() {
                        continue;
                    }
                    config_memory.push(AgentAcpConfigMemoryRecord {
                        engine_kind_id: engine_kind_id.clone(),
                        config_id,
                        value_id,
                    });
                }
            }
        }
    }
    let agents_value = value
        .get("agents")
        .or_else(|| value.get("agent_presets"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut agents = Vec::new();
    for agent in agents_value {
        let id = agent.get("id").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        agents.push(AgentDefinitionRecord {
            id: id.to_string(),
            name: agent
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            description: agent
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            engine_kind_id: agent
                .get("engine_kind")
                .or_else(|| agent.get("agent_id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            prompt: agent
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            plugins_json: json_bytes(agent.get("plugins"), json!([])),
            context_folders_json: json_bytes(agent.get("context_folders"), json!([])),
            context_links_json: json_bytes(agent.get("context_links"), json!([])),
            model_provider: agent
                .get("model_provider")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            model_id: agent
                .get("model_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        });
    }
    let mut profiles = Vec::new();
    if let Some(items) = value.get("profiles").and_then(Value::as_array) {
        for profile in items {
            let id = profile
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if id.is_empty() {
                continue;
            }
            profiles.push(AgentProfileRecord {
                id: id.to_string(),
                kind_id: profile
                    .get("kind_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                label: profile
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                workspace_dir: profile
                    .get("workspace_dir")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            });
        }
    }
    Ok(AgentUiSnapshot {
        agent_hooks_enabled: value
            .get("agent_hooks_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        cross_agent_enabled: value
            .get("cross_agent_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        notify_approval: value
            .get("notify_approval")
            .or_else(|| value.get("notify_awaiting"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
        notify_input: value
            .get("notify_input")
            .or_else(|| value.get("notify_awaiting"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
        notify_success: value
            .get("notify_success")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        notify_failure: value
            .get("notify_failure")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        notify_terminal_bell: value
            .get("notify_terminal_bell")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        commands,
        env,
        config_memory,
        agents,
        profiles,
    })
}

pub(crate) fn json_from_agent_ui(snapshot: &AgentUiSnapshot) -> Result<Value, StoreError> {
    let mut object = serde_json::Map::new();
    object.insert(
        "agent_hooks_enabled".into(),
        json!(snapshot.agent_hooks_enabled),
    );
    object.insert(
        "cross_agent_enabled".into(),
        json!(snapshot.cross_agent_enabled),
    );
    object.insert("notify_approval".into(), json!(snapshot.notify_approval));
    object.insert("notify_input".into(), json!(snapshot.notify_input));
    object.insert("notify_success".into(), json!(snapshot.notify_success));
    object.insert("notify_failure".into(), json!(snapshot.notify_failure));
    object.insert(
        "notify_terminal_bell".into(),
        json!(snapshot.notify_terminal_bell),
    );
    for command in &snapshot.commands {
        object.insert(command.command_key.clone(), json!(command.command));
    }
    let mut env = serde_json::Map::new();
    for record in &snapshot.env {
        env.entry(record.engine_kind_id.clone())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("env object")
            .insert(record.name.clone(), json!(record.value));
    }
    object.insert("acp_env".into(), Value::Object(env));
    let mut memory = serde_json::Map::new();
    for record in &snapshot.config_memory {
        memory
            .entry(record.engine_kind_id.clone())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .expect("memory array")
            .push(json!([record.config_id, record.value_id]));
    }
    object.insert("acp_config_memory".into(), Value::Object(memory));
    object.insert(
        "agents".into(),
        json!(snapshot
            .agents
            .iter()
            .map(|agent| {
                json!({
                    "id": agent.id,
                    "name": agent.name,
                    "description": agent.description,
                    "engine_kind": agent.engine_kind_id,
                    "prompt": agent.prompt,
                    "plugins": serde_json::from_slice::<Value>(&agent.plugins_json)
                        .unwrap_or_else(|_| json!([])),
                    "context_folders": serde_json::from_slice::<Value>(&agent.context_folders_json)
                        .unwrap_or_else(|_| json!([])),
                    "context_links": serde_json::from_slice::<Value>(&agent.context_links_json)
                        .unwrap_or_else(|_| json!([])),
                })
            })
            .collect::<Vec<_>>()),
    );
    object.insert(
        "profiles".into(),
        json!(
            snapshot
                .profiles
                .iter()
                .map(|profile| json!({
                    "id": profile.id,
                    "kind_id": profile.kind_id,
                    "label": profile.label,
                    "workspace_dir": profile.workspace_dir,
                }))
                .collect::<Vec<_>>()
        ),
    );
    Ok(Value::Object(object))
}

pub(crate) fn read_remote_session_snapshot(
    connection: &Connection,
) -> Result<Option<RemoteSessionCatalogSnapshot>, StoreError> {
    let acp = query_remote_acp(connection)?;
    let terminal = query_remote_terminal(connection)?;
    if acp.is_empty() && terminal.is_empty() {
        return Ok(None);
    }
    Ok(Some(RemoteSessionCatalogSnapshot { acp, terminal }))
}

pub(crate) fn write_remote_session_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &RemoteSessionCatalogSnapshot,
) -> Result<(), StoreError> {
    transaction.execute("DELETE FROM remote_acp_session", [])?;
    transaction.execute("DELETE FROM remote_terminal_session", [])?;
    crate::documents::delete_document_scope(transaction, REMOTE_ACP_DOCUMENT)?;
    crate::documents::delete_document_scope(transaction, REMOTE_TERMINAL_DOCUMENT)?;
    for session in &snapshot.acp {
        transaction.execute(
            "INSERT INTO remote_acp_session(
                   id, cwd, title, agent_option_id, agent, launch_command, launch_env_json,
                   resume_id, created_at, lifecycle, hidden
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                session.id,
                session.cwd,
                session.title,
                session.agent_option_id,
                session.agent,
                session.launch_command,
                &session.launch_env_json,
                session.resume_id.as_deref(),
                session.created_at,
                session.lifecycle,
                i64::from(session.hidden),
            ],
        )?;
    }
    for session in &snapshot.terminal {
        transaction.execute(
            "INSERT INTO remote_terminal_session(id, cwd, title, created_at, lifecycle)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session.id,
                session.cwd,
                session.title,
                session.created_at,
                session.lifecycle,
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn read_published_session_snapshot(
    connection: &Connection,
) -> Result<Option<PublishedSessionSnapshot>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, cwd, launch, provider, conversation_id, title, phase, phase_since,
                    updated_at, structured_events, turn_events, agent_event_version, tokens_used,
                    branch, dirty_files_json
             FROM published_session ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(PublishedSessionRecord {
            id: row.get(0)?,
            cwd: row.get(1)?,
            launch: row.get(2)?,
            provider: row.get(3)?,
            conversation_id: row.get(4)?,
            title: row.get(5)?,
            phase: row.get(6)?,
            phase_since: row.get(7)?,
            updated_at: row.get(8)?,
            structured_events: row.get::<_, i64>(9)? != 0,
            turn_events: row.get::<_, i64>(10)? != 0,
            agent_event_version: row.get(11)?,
            tokens_used: row.get(12)?,
            branch: row.get(13)?,
            dirty_files_json: row.get(14)?,
        })
    })?;
    let sessions = rows.collect::<Result<Vec<_>, _>>()?;
    if sessions.is_empty() {
        return Ok(None);
    }
    Ok(Some(PublishedSessionSnapshot {
        version: 1,
        sessions,
    }))
}

pub(crate) fn write_published_session_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &PublishedSessionSnapshot,
) -> Result<(), StoreError> {
    transaction.execute("DELETE FROM published_session", [])?;
    crate::documents::delete_document_scope(transaction, PUBLISHED_SESSION_DOCUMENT)?;
    for session in &snapshot.sessions {
        transaction.execute(
            "INSERT INTO published_session(
                   id, cwd, launch, provider, conversation_id, title, phase, phase_since,
                   updated_at, structured_events, turn_events, agent_event_version, tokens_used,
                   branch, dirty_files_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                session.id,
                session.cwd.as_deref(),
                session.launch.as_deref(),
                session.provider.as_deref(),
                session.conversation_id.as_deref(),
                session.title.as_deref(),
                session.phase,
                session.phase_since,
                session.updated_at,
                i64::from(session.structured_events),
                i64::from(session.turn_events),
                session.agent_event_version,
                session.tokens_used,
                session.branch.as_deref(),
                &session.dirty_files_json,
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn migrate_catalog_from_kv_documents(
    transaction: &Transaction<'_>,
) -> Result<(), StoreError> {
    if !agent_ui_exists(transaction)?
        && let Some(raw) =
            crate::documents::read_generic_document_bytes(transaction, AGENT_UI_DOCUMENT)?
    {
        let value: Value = serde_json::from_slice(&raw)?;
        write_agent_ui_snapshot(transaction, &agent_ui_from_document(&value)?)?;
    }
    let mut remote =
        read_remote_session_snapshot(transaction)?.unwrap_or(RemoteSessionCatalogSnapshot {
            acp: Vec::new(),
            terminal: Vec::new(),
        });
    let mut remote_changed = false;
    if remote.acp.is_empty()
        && let Some(raw) =
            crate::documents::read_generic_document_bytes(transaction, REMOTE_ACP_DOCUMENT)?
    {
        let value: Value = serde_json::from_slice(&raw)?;
        remote.acp = remote_acp_from_document(&value)?;
        remote_changed = true;
    }
    if remote.terminal.is_empty()
        && let Some(raw) =
            crate::documents::read_generic_document_bytes(transaction, REMOTE_TERMINAL_DOCUMENT)?
    {
        let value: Value = serde_json::from_slice(&raw)?;
        remote.terminal = remote_terminal_from_document(&value)?;
        remote_changed = true;
    }
    if remote_changed {
        write_remote_session_snapshot(transaction, &remote)?;
    }
    if read_published_session_snapshot(transaction)?.is_none()
        && let Some(raw) =
            crate::documents::read_generic_document_bytes(transaction, PUBLISHED_SESSION_DOCUMENT)?
    {
        let value: Value = serde_json::from_slice(&raw)?;
        write_published_session_snapshot(transaction, &published_from_document(&value)?)?;
    }
    Ok(())
}

pub(crate) fn remote_acp_from_document(
    value: &Value,
) -> Result<Vec<RemoteAcpSessionRecord>, StoreError> {
    let mut sessions = Vec::new();
    let items = value
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for session in items {
        let id = session
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let launch = session.get("launch").cloned().unwrap_or(json!({}));
        sessions.push(RemoteAcpSessionRecord {
            id: id.to_string(),
            cwd: session
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            title: session
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            agent_option_id: session
                .get("agent_option_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            agent: session
                .get("agent")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            launch_command: launch
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            launch_env_json: json_bytes(launch.get("env"), json!({})),
            resume_id: session
                .get("resume_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            created_at: session
                .get("created_at")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            lifecycle: session
                .get("lifecycle")
                .and_then(Value::as_str)
                .unwrap_or("active")
                .to_string(),
            hidden: session
                .get("hidden")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    Ok(sessions)
}

pub(crate) fn remote_terminal_from_document(
    value: &Value,
) -> Result<Vec<RemoteTerminalSessionRecord>, StoreError> {
    let mut sessions = Vec::new();
    let items = value
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for session in items {
        let id = session
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        sessions.push(RemoteTerminalSessionRecord {
            id: id.to_string(),
            cwd: session
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            title: session
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            created_at: session
                .get("created_at")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            lifecycle: session
                .get("lifecycle")
                .and_then(Value::as_str)
                .unwrap_or("active")
                .to_string(),
        });
    }
    Ok(sessions)
}

pub(crate) fn published_from_document(
    value: &Value,
) -> Result<PublishedSessionSnapshot, StoreError> {
    let mut sessions = Vec::new();
    let items = value
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for session in items {
        let id = session
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        sessions.push(PublishedSessionRecord {
            id: id.to_string(),
            cwd: session
                .get("cwd")
                .and_then(Value::as_str)
                .map(str::to_string),
            launch: session
                .get("launch")
                .and_then(Value::as_str)
                .map(str::to_string),
            provider: session
                .get("provider")
                .and_then(Value::as_str)
                .map(str::to_string),
            conversation_id: session
                .get("conversation_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            title: session
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string),
            phase: session
                .get("phase")
                .and_then(Value::as_str)
                .unwrap_or("idle")
                .to_string(),
            phase_since: session
                .get("phase_since")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            updated_at: session
                .get("updated_at")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            structured_events: session
                .get("structured_events")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            turn_events: session
                .get("turn_events")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            agent_event_version: session.get("agent_event_version").and_then(Value::as_i64),
            tokens_used: session.get("tokens_used").and_then(Value::as_i64),
            branch: session
                .get("branch")
                .and_then(Value::as_str)
                .map(str::to_string),
            dirty_files_json: json_bytes(session.get("dirty_files"), json!([])),
        });
    }
    Ok(PublishedSessionSnapshot {
        version: value.get("version").and_then(Value::as_u64).unwrap_or(1) as u32,
        sessions,
    })
}

fn agent_ui_exists(connection: &Connection) -> Result<bool, StoreError> {
    let has_pref: bool =
        connection.query_row("SELECT EXISTS(SELECT 1 FROM agent_ui_pref)", [], |row| {
            row.get(0)
        })?;
    if has_pref {
        return Ok(true);
    }
    connection
        .query_row("SELECT EXISTS(SELECT 1 FROM agent_definition)", [], |row| {
            row.get(0)
        })
        .map_err(StoreError::from)
}

fn pref_bool(connection: &Connection, key: &str) -> Result<Option<bool>, StoreError> {
    let value: Option<String> = connection
        .query_row(
            "SELECT value FROM agent_ui_pref WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.map(|value| value == "1" || value == "true"))
}

fn upsert_pref(transaction: &Transaction<'_>, key: &str, value: bool) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO agent_ui_pref(key, value) VALUES (?1, ?2)",
        params![key, if value { "1" } else { "0" }],
    )?;
    Ok(())
}

fn query_commands(
    connection: &Connection,
) -> Result<Vec<AgentConversationCommandRecord>, StoreError> {
    let mut statement = connection
        .prepare("SELECT command_key, command FROM agent_acp_command ORDER BY command_key")?;
    let rows = statement.query_map([], |row| {
        Ok(AgentConversationCommandRecord {
            command_key: row.get(0)?,
            command: row.get(1)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn query_env(connection: &Connection) -> Result<Vec<AgentAcpEnvRecord>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT engine_kind_id, name, value
             FROM agent_acp_env ORDER BY engine_kind_id, name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(AgentAcpEnvRecord {
            engine_kind_id: row.get(0)?,
            name: row.get(1)?,
            value: row.get(2)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn query_config_memory(
    connection: &Connection,
) -> Result<Vec<AgentAcpConfigMemoryRecord>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT engine_kind_id, config_id, value_id FROM agent_acp_config_memory
             ORDER BY engine_kind_id, position",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(AgentAcpConfigMemoryRecord {
            engine_kind_id: row.get(0)?,
            config_id: row.get(1)?,
            value_id: row.get(2)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn query_agents(connection: &Connection) -> Result<Vec<AgentDefinitionRecord>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, name, description, engine_kind_id, prompt, plugins_json,
                    context_folders_json, context_links_json, model_provider, model_id
             FROM agent_definition ORDER BY position",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(AgentDefinitionRecord {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            engine_kind_id: row.get(3)?,
            prompt: row.get(4)?,
            plugins_json: row.get(5)?,
            context_folders_json: row.get(6)?,
            context_links_json: row.get(7)?,
            model_provider: row.get(8)?,
            model_id: row.get(9)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn query_profiles(connection: &Connection) -> Result<Vec<AgentProfileRecord>, StoreError> {
    let mut statement = connection
        .prepare("SELECT id, kind_id, label, workspace_dir FROM agent_profile ORDER BY position")?;
    let rows = statement.query_map([], |row| {
        Ok(AgentProfileRecord {
            id: row.get(0)?,
            kind_id: row.get(1)?,
            label: row.get(2)?,
            workspace_dir: row.get(3)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn query_remote_acp(connection: &Connection) -> Result<Vec<RemoteAcpSessionRecord>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, cwd, title, agent_option_id, agent, launch_command, launch_env_json,
                    resume_id, created_at, lifecycle, hidden
             FROM remote_acp_session ORDER BY created_at, id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(RemoteAcpSessionRecord {
            id: row.get(0)?,
            cwd: row.get(1)?,
            title: row.get(2)?,
            agent_option_id: row.get(3)?,
            agent: row.get(4)?,
            launch_command: row.get(5)?,
            launch_env_json: row.get(6)?,
            resume_id: row.get(7)?,
            created_at: row.get(8)?,
            lifecycle: row.get(9)?,
            hidden: row.get::<_, i64>(10)? != 0,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn query_remote_terminal(
    connection: &Connection,
) -> Result<Vec<RemoteTerminalSessionRecord>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, cwd, title, created_at, lifecycle
             FROM remote_terminal_session ORDER BY created_at, id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(RemoteTerminalSessionRecord {
            id: row.get(0)?,
            cwd: row.get(1)?,
            title: row.get(2)?,
            created_at: row.get(3)?,
            lifecycle: row.get(4)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn json_bytes(value: Option<&Value>, fallback: Value) -> Vec<u8> {
    serde_json::to_vec(value.unwrap_or(&fallback)).unwrap_or_else(|_| b"null".to_vec())
}
