use crate::{
    AppearanceSnapshot, AutomationRecord, AutomationRunRecord, AutomationSnapshot,
    AutomationStateRecord, HistoryTitleRecord, HistoryTitleSnapshot, LaunchEntryRecord,
    LaunchSnapshot, StoreError, WorkspaceAcpRecord, WorkspaceLayoutNode, WorkspaceProjectRecord,
    WorkspaceSessionRecord, WorkspaceSnapshot,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params, params_from_iter};
use serde_json::{Map, Number, Value, json};
use std::collections::{BTreeMap, HashSet};

const APPEARANCE_SCOPE: &str = "appearance";
const LAUNCH_SCOPE: &str = "launch";
const WORKSPACE_SCOPE: &str = "workspace";
const DOCUMENT_SCOPE_PREFIX: &str = "document:";
const GROUP_SCOPE_PREFIX: &str = "session_group:";
const ACP_SCOPE_PREFIX: &str = "acp:";

#[derive(Clone)]
struct KvNode {
    value_type: String,
    value: Option<String>,
}

pub fn document_exists(connection: &Connection, key: &str) -> Result<bool, StoreError> {
    Ok(read_document(connection, key)?.is_some())
}

pub fn read_document(connection: &Connection, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
    match key {
        "launch.json" => read_launch(connection),
        "session_metadata.json" => read_history_titles(connection),
        "workspace.json" => read_workspace(connection),
        "appearance.json" => read_appearance(connection),
        "automations.json" => read_automations(connection),
        "agent_ui.json" => match crate::catalog::read_agent_ui_snapshot(connection)? {
            Some(snapshot) => to_vec(&crate::catalog::json_from_agent_ui(&snapshot)?),
            None => read_generic_document(connection, key),
        },
        "remote_acp_sessions.json" => {
            match crate::catalog::read_remote_session_snapshot(connection)? {
                Some(snapshot) => to_vec(&json!({ "sessions": remote_acp_json(&snapshot) })),
                None => read_generic_document(connection, key),
            }
        }
        "remote_terminal_sessions.json" => {
            match crate::catalog::read_remote_session_snapshot(connection)? {
                Some(snapshot) => to_vec(&json!({ "sessions": remote_terminal_json(&snapshot) })),
                None => read_generic_document(connection, key),
            }
        }
        "sessions.json" => match crate::catalog::read_published_session_snapshot(connection)? {
            Some(snapshot) => to_vec(&published_json(&snapshot)),
            None => read_generic_document(connection, key),
        },
        key if crate::config::CONFIG_DOCUMENTS.contains(&key) => {
            crate::config::read_config_document(connection, key)
        }
        _ => read_generic_document(connection, key),
    }
}

pub fn write_document(
    transaction: &Transaction<'_>,
    key: &str,
    value: &[u8],
) -> Result<(), StoreError> {
    match key {
        "launch.json"
        | "session_metadata.json"
        | "workspace.json"
        | "automations.json"
        | "agent_ui.json"
        | "remote_acp_sessions.json"
        | "remote_terminal_sessions.json"
        | "sessions.json" => {
            let parsed: Value = serde_json::from_slice(value)?;
            match key {
                "launch.json" => write_launch(transaction, &parsed),
                "session_metadata.json" => write_history_titles(transaction, &parsed),
                "automations.json" => write_automations(transaction, &parsed),
                "agent_ui.json" => crate::catalog::write_agent_ui_snapshot(
                    transaction,
                    &crate::catalog::agent_ui_from_document(&parsed)?,
                ),
                "remote_acp_sessions.json" => write_remote_acp_document(transaction, &parsed),
                "remote_terminal_sessions.json" => {
                    write_remote_terminal_document(transaction, &parsed)
                }
                "sessions.json" => crate::catalog::write_published_session_snapshot(
                    transaction,
                    &crate::catalog::published_from_document(&parsed)?,
                ),
                _ => write_workspace(transaction, &parsed),
            }
        }
        "appearance.json" => match serde_json::from_slice::<Value>(value) {
            Ok(Value::Object(object)) => write_appearance(transaction, &object),
            _ => write_generic_document(transaction, key, value),
        },
        key if crate::config::CONFIG_DOCUMENTS.contains(&key) => {
            match serde_json::from_slice::<Value>(value) {
                Ok(parsed) => {
                    crate::config::write_config_document(transaction, key, &parsed)?;
                    Ok(())
                }
                Err(_) => write_generic_document(transaction, key, value),
            }
        }
        _ => write_generic_document(transaction, key, value),
    }
}

pub fn delete_document(transaction: &Transaction<'_>, key: &str) -> Result<bool, StoreError> {
    let existed = read_document(transaction, key)?.is_some();
    match key {
        "launch.json" => {
            transaction.execute("DELETE FROM launch_entry", [])?;
            delete_scope(transaction, LAUNCH_SCOPE)?;
        }
        "session_metadata.json" => {
            transaction.execute("DELETE FROM history_title", [])?;
        }
        "workspace.json" => delete_workspace(transaction)?,
        "automations.json" => delete_automations(transaction)?,
        "agent_ui.json" => crate::catalog::write_agent_ui_snapshot(
            transaction,
            &crate::AgentUiSnapshot {
                agent_hooks_enabled: true,
                cross_agent_enabled: true,
                notify_approval: true,
                notify_input: true,
                notify_success: true,
                notify_failure: true,
                notify_terminal_bell: true,
                commands: Vec::new(),
                env: Vec::new(),
                config_memory: Vec::new(),
                agents: Vec::new(),
                profiles: Vec::new(),
            },
        )?,
        "remote_acp_sessions.json" | "remote_terminal_sessions.json" => {
            let mut snapshot = crate::catalog::read_remote_session_snapshot(transaction)?
                .unwrap_or(crate::RemoteSessionCatalogSnapshot {
                    acp: Vec::new(),
                    terminal: Vec::new(),
                });
            if key == "remote_acp_sessions.json" {
                snapshot.acp.clear();
            } else {
                snapshot.terminal.clear();
            }
            crate::catalog::write_remote_session_snapshot(transaction, &snapshot)?;
        }
        "sessions.json" => crate::catalog::write_published_session_snapshot(
            transaction,
            &crate::PublishedSessionSnapshot {
                version: 1,
                sessions: Vec::new(),
            },
        )?,
        "appearance.json" => {
            delete_scope(transaction, APPEARANCE_SCOPE)?;
            delete_scope(transaction, &document_scope(key))?;
        }
        key if crate::config::CONFIG_DOCUMENTS.contains(&key) => {
            crate::config::delete_config_document(transaction, key)?;
        }
        _ => delete_scope(transaction, &document_scope(key))?,
    }
    Ok(existed)
}

pub fn list_document_keys(connection: &Connection) -> Result<Vec<String>, StoreError> {
    let mut keys = Vec::new();
    for key in [
        "agent_ui.json",
        "appearance.json",
        "automations.json",
        "collab.json",
        "dsh-auto-models.json",
        "launch.json",
        "pi-auto-models.json",
        "quota-cache.json",
        "remote_acp_sessions.json",
        "remote_terminal_sessions.json",
        "session_metadata.json",
        "sessions.json",
        "terminal-theme.json",
        "update-settings.json",
        "update-state.json",
        "worktree-inherit.json",
        "workspace.json",
        "workspace_menu.json",
    ] {
        if document_exists(connection, key)? {
            keys.push(key.to_string());
        }
    }
    let mut statement = connection
        .prepare("SELECT DISTINCT scope FROM kv WHERE scope LIKE 'document:%' ORDER BY scope")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        let scope = row?;
        let Some(key) = scope.strip_prefix(DOCUMENT_SCOPE_PREFIX) else {
            continue;
        };
        if !keys.iter().any(|existing| existing == key) {
            keys.push(key.to_string());
        }
    }
    keys.sort();
    Ok(keys)
}

fn document_scope(key: &str) -> String {
    format!("{DOCUMENT_SCOPE_PREFIX}{key}")
}

pub(crate) fn delete_document_scope(
    transaction: &Transaction<'_>,
    key: &str,
) -> Result<(), StoreError> {
    delete_scope(transaction, &document_scope(key))
}

pub(crate) fn read_generic_document_bytes(
    connection: &Connection,
    key: &str,
) -> Result<Option<Vec<u8>>, StoreError> {
    read_generic_document(connection, key)
}

fn write_remote_acp_document(
    transaction: &Transaction<'_>,
    value: &Value,
) -> Result<(), StoreError> {
    let mut snapshot = crate::catalog::read_remote_session_snapshot(transaction)?.unwrap_or(
        crate::RemoteSessionCatalogSnapshot {
            acp: Vec::new(),
            terminal: Vec::new(),
        },
    );
    snapshot.acp = crate::catalog::remote_acp_from_document(value)?;
    crate::catalog::write_remote_session_snapshot(transaction, &snapshot)
}

fn write_remote_terminal_document(
    transaction: &Transaction<'_>,
    value: &Value,
) -> Result<(), StoreError> {
    let mut snapshot = crate::catalog::read_remote_session_snapshot(transaction)?.unwrap_or(
        crate::RemoteSessionCatalogSnapshot {
            acp: Vec::new(),
            terminal: Vec::new(),
        },
    );
    snapshot.terminal = crate::catalog::remote_terminal_from_document(value)?;
    crate::catalog::write_remote_session_snapshot(transaction, &snapshot)
}

fn remote_acp_json(snapshot: &crate::RemoteSessionCatalogSnapshot) -> Vec<Value> {
    snapshot
        .acp
        .iter()
        .map(|session| {
            json!({
                "id": session.id,
                "cwd": session.cwd,
                "title": session.title,
                "agent_option_id": session.agent_option_id,
                "agent": session.agent,
                "launch": {
                    "command": session.launch_command,
                    "env": serde_json::from_slice::<Value>(&session.launch_env_json)
                        .unwrap_or_else(|_| json!({})),
                },
                "resume_id": session.resume_id,
                "created_at": session.created_at,
                "lifecycle": session.lifecycle,
                "hidden": session.hidden,
            })
        })
        .collect()
}

fn remote_terminal_json(snapshot: &crate::RemoteSessionCatalogSnapshot) -> Vec<Value> {
    snapshot
        .terminal
        .iter()
        .map(|session| {
            json!({
                "id": session.id,
                "cwd": session.cwd,
                "title": session.title,
                "created_at": session.created_at,
                "lifecycle": session.lifecycle,
            })
        })
        .collect()
}

fn published_json(snapshot: &crate::PublishedSessionSnapshot) -> Value {
    json!({
        "version": snapshot.version,
        "sessions": snapshot.sessions.iter().map(|session| json!({
            "id": session.id,
            "cwd": session.cwd,
            "launch": session.launch,
            "provider": session.provider,
            "conversation_id": session.conversation_id,
            "title": session.title,
            "phase": session.phase,
            "phase_since": session.phase_since,
            "updated_at": session.updated_at,
            "structured_events": session.structured_events,
            "turn_events": session.turn_events,
            "agent_event_version": session.agent_event_version,
            "tokens_used": session.tokens_used,
            "branch": session.branch,
            "dirty_files": serde_json::from_slice::<Value>(&session.dirty_files_json)
                .unwrap_or_else(|_| json!([])),
        })).collect::<Vec<_>>(),
    })
}

fn group_scope(group_id: &str) -> String {
    format!("{GROUP_SCOPE_PREFIX}{group_id}")
}

fn acp_scope(session_id: &str) -> String {
    format!("{ACP_SCOPE_PREFIX}{session_id}")
}

fn scope_exists(connection: &Connection, scope: &str) -> Result<bool, StoreError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM kv WHERE scope = ?1)",
            [scope],
            |row| row.get(0),
        )
        .map_err(StoreError::from)
}

pub(crate) fn delete_scope(transaction: &Transaction<'_>, scope: &str) -> Result<(), StoreError> {
    transaction.execute("DELETE FROM kv WHERE scope = ?1", [scope])?;
    Ok(())
}

fn upsert_kv_text(
    transaction: &Transaction<'_>,
    scope: &str,
    key: &str,
    value: Option<&str>,
    value_type: &str,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(scope, key) DO UPDATE SET
               value = excluded.value,
               value_type = excluded.value_type,
               updated_at_ms = excluded.updated_at_ms",
        params![scope, key, value, value_type, now_ms()],
    )?;
    Ok(())
}

fn upsert_kv_blob(
    transaction: &Transaction<'_>,
    scope: &str,
    key: &str,
    value: &[u8],
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
             VALUES (?1, ?2, ?3, 'blob', ?4)
             ON CONFLICT(scope, key) DO UPDATE SET
               value = excluded.value,
               value_type = 'blob',
               updated_at_ms = excluded.updated_at_ms",
        params![scope, key, value, now_ms()],
    )?;
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn escape_segment(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn unescape_segment(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

fn child_path(parent: &str, segment: &str) -> String {
    format!("{parent}/{}", escape_segment(segment))
}

fn write_json_tree(
    transaction: &Transaction<'_>,
    scope: &str,
    path: &str,
    value: &Value,
) -> Result<(), StoreError> {
    match value {
        Value::Null => upsert_kv_text(transaction, scope, path, None, "null"),
        Value::Bool(value) => upsert_kv_text(
            transaction,
            scope,
            path,
            Some(if *value { "1" } else { "0" }),
            "boolean",
        ),
        Value::Number(value) => upsert_kv_text(
            transaction,
            scope,
            path,
            Some(&value.to_string()),
            if value.is_i64() || value.is_u64() {
                "integer"
            } else {
                "real"
            },
        ),
        Value::String(value) => upsert_kv_text(transaction, scope, path, Some(value), "text"),
        Value::Array(values) => {
            upsert_kv_text(transaction, scope, path, Some(""), "array")?;
            for (index, value) in values.iter().enumerate() {
                write_json_tree(
                    transaction,
                    scope,
                    &child_path(path, &index.to_string()),
                    value,
                )?;
            }
            Ok(())
        }
        Value::Object(values) => {
            upsert_kv_text(transaction, scope, path, Some(""), "object")?;
            for (key, value) in values {
                write_json_tree(transaction, scope, &child_path(path, key), value)?;
            }
            Ok(())
        }
    }
}

fn read_scope_nodes(
    connection: &Connection,
    scope: &str,
) -> Result<BTreeMap<String, KvNode>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT key, value_type,
                    CASE WHEN value_type = 'blob' THEN NULL ELSE CAST(value AS TEXT) END
             FROM kv WHERE scope = ?1 ORDER BY key",
    )?;
    let rows = statement.query_map([scope], |row| {
        Ok((
            row.get::<_, String>(0)?,
            KvNode {
                value_type: row.get(1)?,
                value: row.get(2)?,
            },
        ))
    })?;
    rows.collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(StoreError::from)
}

fn read_json_tree(
    connection: &Connection,
    scope: &str,
    root: &str,
) -> Result<Option<Value>, StoreError> {
    let nodes = read_scope_nodes(connection, scope)?;
    decode_json_tree(&nodes, root)
}

pub(crate) fn write_scoped_object(
    transaction: &Transaction<'_>,
    scope: &str,
    object: &Map<String, Value>,
) -> Result<(), StoreError> {
    upsert_kv_text(transaction, scope, "$", Some(""), "object")?;
    for (key, value) in object {
        write_json_tree(transaction, scope, &escape_segment(key), value)?;
    }
    Ok(())
}

pub(crate) fn read_scoped_object(
    connection: &Connection,
    scope: &str,
) -> Result<Option<Map<String, Value>>, StoreError> {
    let nodes = read_scope_nodes(connection, scope)?;
    let Some(root) = nodes.get("$") else {
        return Ok(None);
    };
    if root.value_type != "object" {
        return Err(StoreError::KvRootNotObject {
            scope: scope.to_string(),
        });
    }
    let mut object = Map::new();
    for key in nodes
        .keys()
        .filter(|key| key.as_str() != "$" && !key.contains('/'))
    {
        if let Some(value) = decode_json_tree(&nodes, key)? {
            object.insert(unescape_segment(key), value);
        }
    }
    Ok(Some(object))
}

fn decode_json_tree(
    nodes: &BTreeMap<String, KvNode>,
    path: &str,
) -> Result<Option<Value>, StoreError> {
    let Some(node) = nodes.get(path) else {
        return Ok(None);
    };
    let value = match node.value_type.as_str() {
        "null" => Value::Null,
        "boolean" => Value::Bool(node.value.as_deref() == Some("1")),
        "integer" | "real" => {
            let raw = node.value.as_deref().unwrap_or_default();
            Value::Number(
                raw.parse::<Number>()
                    .map_err(|error| StoreError::InvalidKvNumber {
                        path: path.to_string(),
                        source: error,
                    })?,
            )
        }
        "text" => Value::String(node.value.clone().unwrap_or_default()),
        "object" => {
            let mut object = Map::new();
            for segment in direct_child_segments(nodes, path) {
                let child = child_path(path, &unescape_segment(&segment));
                if let Some(value) = decode_json_tree(nodes, &child)? {
                    object.insert(unescape_segment(&segment), value);
                }
            }
            Value::Object(object)
        }
        "array" => {
            let mut indexed = direct_child_segments(nodes, path)
                .into_iter()
                .filter_map(|segment| segment.parse::<usize>().ok().map(|index| (index, segment)))
                .collect::<Vec<_>>();
            indexed.sort_by_key(|(index, _)| *index);
            let mut values = Vec::new();
            for (index, segment) in indexed {
                while values.len() < index {
                    values.push(Value::Null);
                }
                let child = child_path(path, &segment);
                values.push(decode_json_tree(nodes, &child)?.unwrap_or(Value::Null));
            }
            Value::Array(values)
        }
        "blob" => {
            return Err(StoreError::KvBlobNotStructured {
                path: scope_label(path).to_string(),
            });
        }
        other => {
            return Err(StoreError::UnknownKvValueType {
                value_type: other.to_string(),
            });
        }
    };
    Ok(Some(value))
}

fn scope_label(path: &str) -> &str {
    path
}

fn direct_child_segments(nodes: &BTreeMap<String, KvNode>, path: &str) -> Vec<String> {
    let prefix = format!("{path}/");
    let mut segments = nodes
        .keys()
        .filter_map(|key| {
            let rest = key.strip_prefix(&prefix)?;
            (!rest.is_empty() && !rest.contains('/')).then(|| rest.to_string())
        })
        .collect::<Vec<_>>();
    segments.sort();
    segments.dedup();
    segments
}

fn read_generic_document(
    connection: &Connection,
    key: &str,
) -> Result<Option<Vec<u8>>, StoreError> {
    let scope = document_scope(key);
    let root_type = connection
        .query_row(
            "SELECT value_type FROM kv WHERE scope = ?1 AND key = '$'",
            [&scope],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match root_type.as_deref() {
        None => Ok(None),
        Some("blob") => connection
            .query_row(
                "SELECT value FROM kv WHERE scope = ?1 AND key = '$'",
                [&scope],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(StoreError::from),
        Some(_) => {
            let Some(value) = read_json_tree(connection, &scope, "$")? else {
                return Ok(None);
            };
            serde_json::to_vec(&value)
                .map(Some)
                .map_err(StoreError::from)
        }
    }
}

fn write_generic_document(
    transaction: &Transaction<'_>,
    key: &str,
    value: &[u8],
) -> Result<(), StoreError> {
    let scope = document_scope(key);
    delete_scope(transaction, &scope)?;
    match serde_json::from_slice::<Value>(value) {
        Ok(value) => write_json_tree(transaction, &scope, "$", &value),
        Err(_) => upsert_kv_blob(transaction, &scope, "$", value),
    }
}

fn read_appearance(connection: &Connection) -> Result<Option<Vec<u8>>, StoreError> {
    if let Some(object) = read_scoped_object(connection, APPEARANCE_SCOPE)? {
        return serde_json::to_vec(&Value::Object(object))
            .map(Some)
            .map_err(StoreError::from);
    }
    read_generic_document(connection, "appearance.json")
}

pub(crate) fn read_appearance_snapshot(
    connection: &Connection,
) -> Result<Option<AppearanceSnapshot>, StoreError> {
    let Some(object) = read_scoped_object(connection, APPEARANCE_SCOPE)? else {
        return Ok(None);
    };
    Ok(Some(appearance_from_object(&object)))
}

fn write_appearance(
    transaction: &Transaction<'_>,
    object: &Map<String, Value>,
) -> Result<(), StoreError> {
    delete_scope(transaction, APPEARANCE_SCOPE)?;
    delete_scope(transaction, &document_scope("appearance.json"))?;
    write_scoped_object(transaction, APPEARANCE_SCOPE, object)
}

pub(crate) fn write_appearance_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &AppearanceSnapshot,
) -> Result<(), StoreError> {
    delete_scope(transaction, APPEARANCE_SCOPE)?;
    delete_scope(transaction, &document_scope("appearance.json"))?;
    write_scoped_object(transaction, APPEARANCE_SCOPE, &appearance_object(snapshot))
}

fn appearance_from_object(object: &Map<String, Value>) -> AppearanceSnapshot {
    AppearanceSnapshot {
        bg_color: object.get("bg_color").and_then(Value::as_u64).unwrap_or(0) as u32,
        bg_image: object
            .get("bg_image")
            .and_then(Value::as_str)
            .map(str::to_string),
        bg_image_opacity: object
            .get("bg_image_opacity")
            .and_then(Value::as_f64)
            .unwrap_or(0.25) as f32,
        opacity: object.get("opacity").and_then(Value::as_f64).unwrap_or(1.0) as f32,
        blur: object.get("blur").and_then(Value::as_bool).unwrap_or(false),
        glass_style: object
            .get("glass_style")
            .and_then(Value::as_str)
            .unwrap_or("regular")
            .to_string(),
        theme_mode: object
            .get("theme_mode")
            .and_then(Value::as_str)
            .unwrap_or("dark")
            .to_string(),
        ui_font_px: object
            .get("ui_font_px")
            .and_then(Value::as_u64)
            .unwrap_or(16) as u32,
        ui_font_family: object
            .get("ui_font_family")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        font_px: object.get("font_px").and_then(Value::as_u64).unwrap_or(13) as u32,
        font_family: object
            .get("font_family")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    }
}

fn appearance_object(snapshot: &AppearanceSnapshot) -> Map<String, Value> {
    let mut object = Map::new();
    object.insert("bg_color".into(), json!(snapshot.bg_color));
    object.insert("bg_image".into(), json!(snapshot.bg_image));
    object.insert("bg_image_opacity".into(), json!(snapshot.bg_image_opacity));
    object.insert("opacity".into(), json!(snapshot.opacity));
    object.insert("blur".into(), json!(snapshot.blur));
    object.insert("glass_style".into(), json!(snapshot.glass_style));
    object.insert("theme_mode".into(), json!(snapshot.theme_mode));
    object.insert("ui_font_px".into(), json!(snapshot.ui_font_px));
    object.insert("ui_font_family".into(), json!(snapshot.ui_font_family));
    object.insert("font_px".into(), json!(snapshot.font_px));
    object.insert("font_family".into(), json!(snapshot.font_family));
    object
}

fn read_launch(connection: &Connection) -> Result<Option<Vec<u8>>, StoreError> {
    let Some(snapshot) = read_launch_snapshot(connection)? else {
        return Ok(None);
    };
    to_vec(&json!({
        "version": snapshot.version,
        "entries": snapshot.entries.iter().map(|entry| json!({
            "label": entry.label,
            "command": entry.command,
            "provider": entry.provider,
        })).collect::<Vec<_>>(),
    }))
}

pub(crate) fn read_launch_snapshot(
    connection: &Connection,
) -> Result<Option<LaunchSnapshot>, StoreError> {
    let Some(version) = read_json_tree(connection, LAUNCH_SCOPE, "version")? else {
        return Ok(None);
    };
    let mut statement = connection
        .prepare("SELECT label, command, provider FROM launch_entry ORDER BY position")?;
    let rows = statement.query_map([], |row| {
        Ok(LaunchEntryRecord {
            label: row.get(0)?,
            command: row.get(1)?,
            provider: row.get(2)?,
        })
    })?;
    let entries = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(Some(LaunchSnapshot {
        version: version.as_u64().unwrap_or(1) as u32,
        entries,
    }))
}

fn write_launch(transaction: &Transaction<'_>, value: &Value) -> Result<(), StoreError> {
    write_launch_snapshot(transaction, &launch_from_document(value))
}

pub(crate) fn write_launch_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &LaunchSnapshot,
) -> Result<(), StoreError> {
    transaction.execute("DELETE FROM launch_entry", [])?;
    delete_scope(transaction, LAUNCH_SCOPE)?;
    write_json_tree(
        transaction,
        LAUNCH_SCOPE,
        "version",
        &json!(snapshot.version),
    )?;
    for (position, entry) in snapshot.entries.iter().enumerate() {
        transaction.execute(
            "INSERT INTO launch_entry(position, label, command, provider)
                 VALUES (?1, ?2, ?3, ?4)",
            params![
                position as i64,
                entry.label,
                entry.command,
                entry.provider.as_deref(),
            ],
        )?;
    }
    Ok(())
}

fn launch_from_document(value: &Value) -> LaunchSnapshot {
    LaunchSnapshot {
        version: as_i64(value.get("version")).unwrap_or(1) as u32,
        entries: value
            .get("entries")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|entry| LaunchEntryRecord {
                label: as_str(entry.get("label")).unwrap_or_default().to_string(),
                command: as_str(entry.get("command")).unwrap_or_default().to_string(),
                provider: as_str(entry.get("provider")).map(str::to_string),
            })
            .collect(),
    }
}

fn read_history_titles(connection: &Connection) -> Result<Option<Vec<u8>>, StoreError> {
    let Some(snapshot) = read_history_title_snapshot(connection)? else {
        return Ok(None);
    };
    to_vec(&json!({
        "schema_version": snapshot.schema_version,
        "sessions": snapshot.sessions.iter().map(|session| json!({
            "agent": session.agent,
            "profile_id": if session.profile_id.is_empty() {
                Value::Null
            } else {
                Value::String(session.profile_id.clone())
            },
            "resume_id": session.resume_id,
            "custom_title": session.custom_title,
            "updated_at_ms": session.updated_at_ms,
        })).collect::<Vec<_>>(),
    }))
}

pub(crate) fn read_history_title_snapshot(
    connection: &Connection,
) -> Result<Option<HistoryTitleSnapshot>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT agent, profile_id, resume_id, custom_title, updated_at_ms
             FROM history_title ORDER BY agent, profile_id, resume_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(HistoryTitleRecord {
            agent: row.get(0)?,
            profile_id: row.get(1)?,
            resume_id: row.get(2)?,
            custom_title: row.get(3)?,
            updated_at_ms: row.get(4)?,
        })
    })?;
    let sessions = rows.collect::<Result<Vec<_>, _>>()?;
    if sessions.is_empty() {
        return Ok(None);
    }
    Ok(Some(HistoryTitleSnapshot {
        schema_version: 1,
        sessions,
    }))
}

fn write_history_titles(transaction: &Transaction<'_>, value: &Value) -> Result<(), StoreError> {
    write_history_title_snapshot(transaction, &history_from_document(value))
}

pub(crate) fn write_history_title_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &HistoryTitleSnapshot,
) -> Result<(), StoreError> {
    transaction.execute("DELETE FROM history_title", [])?;
    for session in &snapshot.sessions {
        transaction.execute(
            "INSERT INTO history_title(
                   agent, profile_id, resume_id, custom_title, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session.agent,
                session.profile_id,
                session.resume_id,
                session.custom_title,
                session.updated_at_ms,
            ],
        )?;
    }
    Ok(())
}

fn history_from_document(value: &Value) -> HistoryTitleSnapshot {
    HistoryTitleSnapshot {
        schema_version: as_i64(value.get("schema_version")).unwrap_or(1) as u32,
        sessions: value
            .get("sessions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|session| HistoryTitleRecord {
                agent: as_str(session.get("agent")).unwrap_or_default().to_string(),
                profile_id: as_str(session.get("profile_id"))
                    .unwrap_or_default()
                    .to_string(),
                resume_id: as_str(session.get("resume_id"))
                    .unwrap_or_default()
                    .to_string(),
                custom_title: as_str(session.get("custom_title"))
                    .unwrap_or_default()
                    .to_string(),
                updated_at_ms: as_i64(session.get("updated_at_ms")).unwrap_or(0),
            })
            .collect(),
    }
}

const AUTOMATION_DOCUMENT: &str = "automations.json";

fn automation_domain_exists(connection: &Connection) -> Result<bool, StoreError> {
    let has_meta: bool =
        connection.query_row("SELECT EXISTS(SELECT 1 FROM automation_meta)", [], |row| {
            row.get(0)
        })?;
    if has_meta {
        return Ok(true);
    }
    connection
        .query_row("SELECT EXISTS(SELECT 1 FROM automation)", [], |row| {
            row.get(0)
        })
        .map_err(StoreError::from)
}

fn read_automations(connection: &Connection) -> Result<Option<Vec<u8>>, StoreError> {
    match read_automation_snapshot(connection)? {
        Some(snapshot) => to_vec(&json_from_snapshot(&snapshot)?),
        None => read_generic_document(connection, AUTOMATION_DOCUMENT),
    }
}

pub(crate) fn read_automation_snapshot(
    connection: &Connection,
) -> Result<Option<AutomationSnapshot>, StoreError> {
    if !automation_domain_exists(connection)? {
        return Ok(None);
    }
    let mut automations = Vec::new();
    {
        let mut statement = connection.prepare(
            "SELECT id, name, enabled, workspace_dir, trigger_json, action_json, sinks_json
                 FROM automation ORDER BY position",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(AutomationRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                enabled: row.get::<_, i64>(2)? != 0,
                workspace_dir: row.get(3)?,
                trigger_json: row.get(4)?,
                action_json: row.get(5)?,
                sinks_json: row.get(6)?,
            })
        })?;
        for row in rows {
            automations.push(row?);
        }
    }
    let mut states = Vec::new();
    {
        let mut statement = connection.prepare(
            "SELECT automation_id, next_run_at, last_run_id
                 FROM automation_state ORDER BY automation_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(AutomationStateRecord {
                automation_id: row.get(0)?,
                next_run_at: row.get(1)?,
                last_run_id: row.get(2)?,
            })
        })?;
        for row in rows {
            states.push(row?);
        }
    }
    let mut runs = Vec::new();
    {
        let mut statement = connection.prepare(
            "SELECT id, automation_id, source, status, created_at, scheduled_for, started_at,
                        delivery_attempt_at, delivery_attempts, finished_at, session_id,
                        provider_session_id, output, error, runtime_released_at, context_json
                 FROM automation_run ORDER BY created_at, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(AutomationRunRecord {
                id: row.get(0)?,
                automation_id: row.get(1)?,
                source: row.get(2)?,
                status: row.get(3)?,
                created_at: row.get(4)?,
                scheduled_for: row.get(5)?,
                started_at: row.get(6)?,
                delivery_attempt_at: row.get(7)?,
                delivery_attempts: row.get(8)?,
                finished_at: row.get(9)?,
                session_id: row.get(10)?,
                provider_session_id: row.get(11)?,
                output: row.get(12)?,
                error: row.get(13)?,
                runtime_released_at: row.get(14)?,
                context_json: row.get(15)?,
            })
        })?;
        for row in rows {
            runs.push(row?);
        }
    }
    Ok(Some(AutomationSnapshot {
        schema_version: meta_u32(connection, "schema_version")?.unwrap_or(1),
        store_id: meta_text(connection, "store_id")?.unwrap_or_default(),
        revision: meta_u64(connection, "revision")?.unwrap_or(0),
        timezone_fingerprint: meta_text(connection, "timezone_fingerprint")?.unwrap_or_default(),
        automations,
        states,
        runs,
    }))
}

fn write_automations(transaction: &Transaction<'_>, value: &Value) -> Result<(), StoreError> {
    write_automation_snapshot(transaction, &snapshot_from_document(value)?)
}

pub(crate) fn write_automation_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &AutomationSnapshot,
) -> Result<(), StoreError> {
    delete_scope(transaction, &document_scope(AUTOMATION_DOCUMENT))?;
    let keep_automation_ids = snapshot
        .automations
        .iter()
        .map(|automation| automation.id.clone())
        .collect::<Vec<_>>();
    delete_missing_ids(transaction, "automation", "id", &keep_automation_ids)?;
    for (position, automation) in snapshot.automations.iter().enumerate() {
        transaction.execute(
            "INSERT INTO automation(
                   id, name, enabled, workspace_dir, position, trigger_json, action_json, sinks_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(id) DO UPDATE SET
                   name = excluded.name,
                   enabled = excluded.enabled,
                   workspace_dir = excluded.workspace_dir,
                   position = excluded.position,
                   trigger_json = excluded.trigger_json,
                   action_json = excluded.action_json,
                   sinks_json = excluded.sinks_json",
            params![
                automation.id,
                automation.name,
                if automation.enabled { 1 } else { 0 },
                automation.workspace_dir.as_deref(),
                position as i64,
                &automation.trigger_json,
                &automation.action_json,
                &automation.sinks_json,
            ],
        )?;
    }

    let keep_run_ids = snapshot
        .runs
        .iter()
        .map(|run| run.id.clone())
        .collect::<Vec<_>>();
    delete_missing_ids(transaction, "automation_run", "id", &keep_run_ids)?;
    for run in &snapshot.runs {
        transaction.execute(
            "INSERT INTO automation_run(
                   id, automation_id, source, status, created_at, scheduled_for, started_at,
                   delivery_attempt_at, delivery_attempts, finished_at, session_id,
                   provider_session_id, output, error, runtime_released_at, context_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                 ON CONFLICT(id) DO UPDATE SET
                   automation_id = excluded.automation_id,
                   source = excluded.source,
                   status = excluded.status,
                   created_at = excluded.created_at,
                   scheduled_for = excluded.scheduled_for,
                   started_at = excluded.started_at,
                   delivery_attempt_at = excluded.delivery_attempt_at,
                   delivery_attempts = excluded.delivery_attempts,
                   finished_at = excluded.finished_at,
                   session_id = excluded.session_id,
                   provider_session_id = excluded.provider_session_id,
                   output = excluded.output,
                   error = excluded.error,
                   runtime_released_at = excluded.runtime_released_at,
                   context_json = excluded.context_json",
            params![
                run.id,
                run.automation_id,
                run.source,
                run.status,
                run.created_at,
                run.scheduled_for,
                run.started_at,
                run.delivery_attempt_at,
                run.delivery_attempts,
                run.finished_at,
                run.session_id.as_deref(),
                run.provider_session_id.as_deref(),
                run.output.as_deref(),
                run.error.as_deref(),
                run.runtime_released_at,
                &run.context_json,
            ],
        )?;
    }

    transaction.execute("DELETE FROM automation_state", [])?;
    for state in &snapshot.states {
        transaction.execute(
            "INSERT INTO automation_state(automation_id, next_run_at, last_run_id)
                 VALUES (?1, ?2, ?3)",
            params![
                state.automation_id,
                state.next_run_at,
                state.last_run_id.as_deref(),
            ],
        )?;
    }

    upsert_meta(
        transaction,
        "schema_version",
        &snapshot.schema_version.to_string(),
    )?;
    upsert_meta(transaction, "store_id", &snapshot.store_id)?;
    upsert_meta(transaction, "revision", &snapshot.revision.to_string())?;
    upsert_meta(
        transaction,
        "timezone_fingerprint",
        &snapshot.timezone_fingerprint,
    )?;
    Ok(())
}

fn snapshot_from_document(value: &Value) -> Result<AutomationSnapshot, StoreError> {
    let mut automations = Vec::new();
    if let Some(items) = value.get("automations").and_then(Value::as_array) {
        for automation in items {
            let id = as_str(automation.get("id")).unwrap_or_default();
            if id.is_empty() {
                continue;
            }
            let sinks = automation
                .get("sinks")
                .cloned()
                .unwrap_or_else(|| json!([]));
            automations.push(AutomationRecord {
                id: id.to_string(),
                name: as_str(automation.get("name"))
                    .unwrap_or_default()
                    .to_string(),
                enabled: automation
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                workspace_dir: as_str(automation.get("workspace_dir")).map(str::to_string),
                trigger_json: json_blob(automation.get("trigger"))?,
                action_json: json_blob(automation.get("action"))?,
                sinks_json: json_blob(Some(&sinks))?,
            });
        }
    }
    let mut runs = Vec::new();
    if let Some(items) = value.get("runs").and_then(Value::as_array) {
        for run in items {
            let id = as_str(run.get("id")).unwrap_or_default();
            let automation_id = as_str(run.get("automation_id")).unwrap_or_default();
            if id.is_empty() || automation_id.is_empty() {
                continue;
            }
            runs.push(AutomationRunRecord {
                id: id.to_string(),
                automation_id: automation_id.to_string(),
                source: as_str(run.get("source")).unwrap_or("manual").to_string(),
                status: as_str(run.get("status")).unwrap_or("starting").to_string(),
                created_at: as_i64(run.get("created_at")).unwrap_or(0),
                scheduled_for: as_i64(run.get("scheduled_for")),
                started_at: as_i64(run.get("started_at")),
                delivery_attempt_at: as_i64(run.get("delivery_attempt_at")),
                delivery_attempts: as_i64(run.get("delivery_attempts")).unwrap_or(0),
                finished_at: as_i64(run.get("finished_at")),
                session_id: as_str(run.get("session_id")).map(str::to_string),
                provider_session_id: as_str(run.get("provider_session_id")).map(str::to_string),
                output: as_str(run.get("output")).map(str::to_string),
                error: as_str(run.get("error")).map(str::to_string),
                runtime_released_at: as_i64(run.get("runtime_released_at")),
                context_json: json_blob(run.get("context"))?,
            });
        }
    }
    let mut states = Vec::new();
    if let Some(items) = value.get("states").and_then(Value::as_array) {
        for state in items {
            let automation_id = as_str(state.get("automation_id")).unwrap_or_default();
            if automation_id.is_empty() {
                continue;
            }
            states.push(AutomationStateRecord {
                automation_id: automation_id.to_string(),
                next_run_at: as_i64(state.get("next_run_at")),
                last_run_id: as_str(state.get("last_run_id")).map(str::to_string),
            });
        }
    }
    Ok(AutomationSnapshot {
        schema_version: as_i64(value.get("schema_version")).unwrap_or(1) as u32,
        store_id: as_str(value.get("store_id"))
            .unwrap_or_default()
            .to_string(),
        revision: as_i64(value.get("revision")).unwrap_or(0) as u64,
        timezone_fingerprint: as_str(value.get("timezone_fingerprint"))
            .unwrap_or_default()
            .to_string(),
        automations,
        states,
        runs,
    })
}

fn json_from_snapshot(snapshot: &AutomationSnapshot) -> Result<Value, StoreError> {
    let automations = snapshot
        .automations
        .iter()
        .map(|automation| {
            Ok(json!({
                "id": automation.id,
                "name": automation.name,
                "enabled": automation.enabled,
                "workspace_dir": automation.workspace_dir,
                "trigger": blob_json(&automation.trigger_json)?,
                "action": blob_json(&automation.action_json)?,
                "sinks": blob_json(&automation.sinks_json)?,
            }))
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let states = snapshot
        .states
        .iter()
        .map(|state| {
            json!({
                "automation_id": state.automation_id,
                "next_run_at": state.next_run_at,
                "last_run_id": state.last_run_id,
            })
        })
        .collect::<Vec<_>>();
    let mut runs = Vec::new();
    for run in &snapshot.runs {
        let mut value = json!({
            "id": run.id,
            "automation_id": run.automation_id,
            "source": run.source,
            "status": run.status,
            "created_at": run.created_at,
            "delivery_attempts": run.delivery_attempts,
            "context": blob_json(&run.context_json)?,
        });
        let object = value.as_object_mut().expect("run object");
        if let Some(scheduled_for) = run.scheduled_for {
            object.insert("scheduled_for".into(), json!(scheduled_for));
        }
        if let Some(started_at) = run.started_at {
            object.insert("started_at".into(), json!(started_at));
        }
        if let Some(delivery_attempt_at) = run.delivery_attempt_at {
            object.insert("delivery_attempt_at".into(), json!(delivery_attempt_at));
        }
        if let Some(finished_at) = run.finished_at {
            object.insert("finished_at".into(), json!(finished_at));
        }
        if let Some(session_id) = &run.session_id {
            object.insert("session_id".into(), json!(session_id));
        }
        if let Some(provider_session_id) = &run.provider_session_id {
            object.insert("provider_session_id".into(), json!(provider_session_id));
        }
        if let Some(output) = &run.output {
            object.insert("output".into(), json!(output));
        }
        if let Some(error) = &run.error {
            object.insert("error".into(), json!(error));
        }
        if let Some(runtime_released_at) = run.runtime_released_at {
            object.insert("runtime_released_at".into(), json!(runtime_released_at));
        }
        runs.push(value);
    }
    Ok(json!({
        "schema_version": snapshot.schema_version,
        "store_id": snapshot.store_id,
        "revision": snapshot.revision,
        "timezone_fingerprint": snapshot.timezone_fingerprint,
        "automations": automations,
        "states": states,
        "runs": runs,
    }))
}

fn delete_automations(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    transaction.execute("DELETE FROM automation", [])?;
    transaction.execute("DELETE FROM automation_meta", [])?;
    delete_scope(transaction, &document_scope(AUTOMATION_DOCUMENT))
}

pub(crate) fn migrate_automations_from_kv_document(
    transaction: &Transaction<'_>,
) -> Result<(), StoreError> {
    if automation_domain_exists(transaction)? {
        return Ok(());
    }
    let Some(raw) = read_generic_document(transaction, AUTOMATION_DOCUMENT)? else {
        return Ok(());
    };
    let value: Value = serde_json::from_slice(&raw)?;
    write_automations(transaction, &value)
}

pub(crate) fn rebuild_automation_run_transcript_fk(
    transaction: &Transaction<'_>,
) -> Result<(), StoreError> {
    let sql: Option<String> = transaction
        .query_row(
            "SELECT sql FROM sqlite_schema
             WHERE type = 'table' AND name = 'automation_run_transcript'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(sql) = sql else {
        return Ok(());
    };
    if sql.to_ascii_lowercase().contains("references") {
        return Ok(());
    }
    transaction
        .execute_batch(
            "CREATE TABLE automation_run_transcript_new (
               run_id TEXT PRIMARY KEY,
               entries_json BLOB NOT NULL,
               updated_at_ms INTEGER NOT NULL
             ) WITHOUT ROWID;
             INSERT INTO automation_run_transcript_new(run_id, entries_json, updated_at_ms)
             SELECT run_id, entries_json, updated_at_ms FROM automation_run_transcript
             WHERE run_id IN (SELECT id FROM automation_run);
             DROP TABLE automation_run_transcript;
             CREATE TABLE automation_run_transcript (
               run_id TEXT PRIMARY KEY REFERENCES automation_run(id) ON DELETE CASCADE,
               entries_json BLOB NOT NULL,
               updated_at_ms INTEGER NOT NULL
             ) WITHOUT ROWID;
             INSERT INTO automation_run_transcript(run_id, entries_json, updated_at_ms)
             SELECT run_id, entries_json, updated_at_ms FROM automation_run_transcript_new;
             DROP TABLE automation_run_transcript_new;",
        )
        .map_err(StoreError::from)
}

fn delete_missing_ids(
    transaction: &Transaction<'_>,
    table: &str,
    column: &str,
    keep: &[String],
) -> Result<(), StoreError> {
    if keep.is_empty() {
        transaction.execute(&format!("DELETE FROM {table}"), [])?;
        return Ok(());
    }
    let placeholders = keep.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!("DELETE FROM {table} WHERE {column} NOT IN ({placeholders})");
    let mut statement = transaction.prepare(&sql)?;
    statement.execute(params_from_iter(keep.iter()))?;
    Ok(())
}

fn upsert_meta(transaction: &Transaction<'_>, key: &str, value: &str) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO automation_meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn meta_text(connection: &Connection, key: &str) -> Result<Option<String>, StoreError> {
    connection
        .query_row(
            "SELECT value FROM automation_meta WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::from)
}

fn meta_u32(connection: &Connection, key: &str) -> Result<Option<u32>, StoreError> {
    Ok(meta_text(connection, key)?.and_then(|value| value.parse().ok()))
}

fn meta_u64(connection: &Connection, key: &str) -> Result<Option<u64>, StoreError> {
    Ok(meta_text(connection, key)?.and_then(|value| value.parse().ok()))
}

fn json_blob(value: Option<&Value>) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(value.unwrap_or(&Value::Null)).map_err(StoreError::from)
}

fn blob_json(raw: &[u8]) -> Result<Value, StoreError> {
    serde_json::from_slice(raw).map_err(StoreError::from)
}

fn delete_workspace(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    let mut scopes = transaction
        .prepare(
            "SELECT DISTINCT scope FROM kv
             WHERE scope = ?1 OR scope LIKE 'session_group:%' OR scope LIKE 'acp:%'",
        )?
        .query_map([WORKSPACE_SCOPE], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    scopes.push(WORKSPACE_SCOPE.to_string());
    for scope in scopes {
        delete_scope(transaction, &scope)?;
    }
    transaction.execute("DELETE FROM session_group", [])?;
    transaction.execute("DELETE FROM project", [])?;
    Ok(())
}

fn write_workspace(transaction: &Transaction<'_>, value: &Value) -> Result<(), StoreError> {
    write_workspace_snapshot(transaction, &workspace_from_document(value)?)
}

pub(crate) fn write_workspace_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &WorkspaceSnapshot,
) -> Result<(), StoreError> {
    delete_workspace(transaction)?;
    delete_document_scope(transaction, "workspace.json")?;
    write_scoped_object(
        transaction,
        WORKSPACE_SCOPE,
        &workspace_meta_object(snapshot),
    )?;

    let mut project_roots = Vec::new();
    let mut seen_projects = HashSet::new();
    for project in &snapshot.projects {
        let root = project.root.trim();
        if root.is_empty() || !seen_projects.insert(root.to_string()) {
            continue;
        }
        let position = project_roots.len() as i64;
        transaction.execute(
            "INSERT INTO project(root, position, collapsed) VALUES (?1, ?2, ?3)",
            params![root, position, i64::from(project.collapsed)],
        )?;
        project_roots.push(root.to_string());
    }

    let mut used_group_ids = HashSet::new();
    let mut used_session_ids = HashSet::new();
    for (position, session) in snapshot.sessions.iter().enumerate() {
        write_session_record(
            transaction,
            session,
            position,
            &project_roots,
            &mut used_group_ids,
            &mut used_session_ids,
        )?;
    }
    Ok(())
}

fn workspace_meta_object(snapshot: &WorkspaceSnapshot) -> Map<String, Value> {
    let mut meta = Map::new();
    meta.insert("active_session".into(), json!(snapshot.active_session));
    if let Some(id) = &snapshot.active_session_id {
        meta.insert("active_session_id".into(), json!(id));
    }
    meta.insert(
        "pinned_projects".into(),
        json!(snapshot.pinned_projects.clone()),
    );
    meta.insert(
        "collapsed_agents".into(),
        json!(snapshot.collapsed_agents.clone()),
    );
    meta.insert(
        "sidebar_hide_empty_projects".into(),
        json!(snapshot.sidebar_hide_empty_projects),
    );
    if let Some(raw) = &snapshot.route_json
        && let Ok(value) = serde_json::from_slice(raw)
    {
        meta.insert("route".into(), value);
    }
    if let Some(id) = &snapshot.selected_agent_id {
        meta.insert("selected_agent_id".into(), json!(id));
    }
    if let Some(surface) = &snapshot.active_workspace_surface {
        meta.insert("active_workspace_surface".into(), json!(surface));
    }
    if let Some(raw) = &snapshot.workspace_surface_titles_json
        && let Ok(value) = serde_json::from_slice(raw)
    {
        meta.insert("workspace_surface_titles".into(), value);
    }
    if let Some(width) = snapshot.sidebar_w {
        meta.insert("sidebar_w".into(), json!(width));
    }
    if let Some(open) = snapshot.sidebar_open {
        meta.insert("sidebar_open".into(), json!(open));
    }
    if let Some(grouping) = &snapshot.sidebar_grouping {
        meta.insert("sidebar_grouping".into(), json!(grouping));
    }
    meta
}

pub(crate) fn workspace_from_document(value: &Value) -> Result<WorkspaceSnapshot, StoreError> {
    let mut meta = value.as_object().cloned().unwrap_or_default();
    let mut sessions = meta
        .remove("sessions")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    let projects = meta
        .remove("projects")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    let collapsed_projects = meta
        .remove("collapsed_projects")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    meta.remove("menu");

    if sessions.is_empty() {
        if let Some(layout) = meta.remove("layout").filter(|value| !value.is_null()) {
            let active = meta
                .remove("active")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            sessions.push(json!({
                "layout": layout,
                "active": active,
                "last_updated_at": 0,
                "custom_title": null,
                "acp": null,
                "route": null,
            }));
            meta.insert("active_session".into(), json!(0));
        } else if let Some(tabs) = meta
            .remove("tabs")
            .and_then(|value| value.as_array().cloned())
        {
            let active = meta
                .remove("active")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            sessions.extend(tabs.into_iter().map(|cwd| {
                json!({
                    "layout": {"Leaf": {
                        "cwd": cwd,
                        "id": null,
                        "custom_title": null,
                        "launch_label": null,
                        "launch_cmd": null
                    }},
                    "active": 0,
                    "last_updated_at": 0,
                    "custom_title": null,
                    "acp": null,
                    "route": null,
                })
            }));
            meta.insert("active_session".into(), json!(active));
        }
    }
    meta.remove("layout");
    meta.remove("tabs");
    meta.remove("active");

    let collapsed: HashSet<String> = collapsed_projects
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let mut project_records = Vec::new();
    let mut seen = HashSet::new();
    for project in projects {
        let Some(root) = project.as_str().filter(|root| !root.trim().is_empty()) else {
            continue;
        };
        if !seen.insert(root.to_string()) {
            continue;
        }
        project_records.push(WorkspaceProjectRecord {
            root: root.to_string(),
            collapsed: collapsed.contains(root),
        });
    }

    Ok(WorkspaceSnapshot {
        active_session: meta
            .get("active_session")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        active_session_id: as_str(meta.get("active_session_id")).map(str::to_string),
        route_json: optional_json_bytes(meta.get("route")),
        selected_agent_id: as_str(meta.get("selected_agent_id")).map(str::to_string),
        active_workspace_surface: as_str(meta.get("active_workspace_surface")).map(str::to_string),
        workspace_surface_titles_json: optional_json_bytes(meta.get("workspace_surface_titles")),
        sidebar_w: meta.get("sidebar_w").and_then(Value::as_f64),
        sidebar_open: meta.get("sidebar_open").and_then(Value::as_bool),
        sidebar_grouping: as_str(meta.get("sidebar_grouping")).map(str::to_string),
        collapsed_agents: string_list(meta.get("collapsed_agents")),
        pinned_projects: string_list(meta.get("pinned_projects")),
        sidebar_hide_empty_projects: meta
            .get("sidebar_hide_empty_projects")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        projects: project_records,
        sessions: sessions
            .iter()
            .map(session_from_value)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn optional_json_bytes(value: Option<&Value>) -> Option<Vec<u8>> {
    let value = value.filter(|value| !value.is_null())?;
    serde_json::to_vec(value).ok()
}

fn session_from_value(value: &Value) -> Result<WorkspaceSessionRecord, StoreError> {
    let layout = value
        .get("layout")
        .cloned()
        .unwrap_or_else(default_leaf_layout);
    Ok(WorkspaceSessionRecord {
        custom_title: as_str(value.get("custom_title")).map(str::to_string),
        last_updated_at: as_i64(value.get("last_updated_at")).unwrap_or(0),
        active: as_usize(value.get("active")).unwrap_or(0),
        layout: layout_from_value(&layout),
        acp: value
            .get("acp")
            .filter(|value| value.is_object())
            .map(acp_from_value),
        route_json: optional_json_bytes(value.get("route")),
    })
}

fn layout_from_value(value: &Value) -> WorkspaceLayoutNode {
    if let Some(split) = value.get("Split") {
        let vertical =
            as_str(split.get("axis")) == Some("V") || as_str(split.get("axis")) == Some("vertical");
        let children = split
            .get("children")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(layout_from_value)
            .collect();
        let sizes = split
            .get("sizes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_f64)
            .collect();
        return WorkspaceLayoutNode::Split {
            vertical,
            children,
            sizes,
        };
    }
    let leaf = value.get("Leaf").and_then(Value::as_object);
    WorkspaceLayoutNode::Leaf {
        cwd: leaf
            .and_then(|leaf| as_str(leaf.get("cwd")))
            .map(str::to_string),
        id: leaf
            .and_then(|leaf| as_str(leaf.get("id")))
            .map(str::to_string),
        custom_title: leaf
            .and_then(|leaf| as_str(leaf.get("custom_title")))
            .map(str::to_string),
        launch_label: leaf
            .and_then(|leaf| as_str(leaf.get("launch_label")))
            .map(str::to_string),
        launch_cmd: leaf
            .and_then(|leaf| as_str(leaf.get("launch_cmd")))
            .map(str::to_string),
    }
}

fn acp_from_value(value: &Value) -> WorkspaceAcpRecord {
    let launch = value.get("launch").and_then(Value::as_object);
    let fork = value.get("fork_origin").and_then(Value::as_object);
    WorkspaceAcpRecord {
        cwd: as_str(value.get("cwd")).map(str::to_string),
        sid: as_str(value.get("sid")).map(str::to_string),
        agent: as_str(value.get("agent")).unwrap_or("claude").to_string(),
        profile_id: as_str(value.get("profile_id")).map(str::to_string),
        history_session_id: as_str(value.get("history_session_id")).map(str::to_string),
        launch_command: launch
            .and_then(|launch| as_str(launch.get("command")))
            .or_else(|| as_str(value.get("cmd")))
            .unwrap_or_default()
            .to_string(),
        launch_env_json: launch.and_then(|launch| optional_json_bytes(launch.get("env"))),
        refresh_launch_from_settings: value
            .get("refresh_launch_from_settings")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        fork_session_id: fork
            .and_then(|fork| as_str(fork.get("session_id")))
            .map(str::to_string),
        fork_title: fork
            .and_then(|fork| as_str(fork.get("title")))
            .map(str::to_string),
        fork_agent: fork
            .and_then(|fork| as_str(fork.get("agent")))
            .map(str::to_string),
        fork_profile_label: fork
            .and_then(|fork| as_str(fork.get("profile_label")))
            .map(str::to_string),
        fork_from_history: fork
            .and_then(|fork| fork.get("from_history"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        pending_prompt: as_str(value.get("pending_prompt")).map(str::to_string),
        pending_delivery_id: as_str(value.get("pending_delivery_id")).map(str::to_string),
        agent_definition_id: as_str(value.get("agent_definition_id")).map(str::to_string),
        automation_id: as_str(value.get("automation_id")).map(str::to_string),
        config_values_json: optional_json_bytes(value.get("config_values")),
        conversation_binding_json: optional_json_bytes(value.get("conversation_binding")),
        agent_session_json: optional_json_bytes(value.get("agent_session")),
        pending_agent_preset_json: optional_json_bytes(value.get("pending_agent_preset")),
        session_title_json: optional_json_bytes(value.get("session_title")),
    }
}

fn write_session_record(
    transaction: &Transaction<'_>,
    saved: &WorkspaceSessionRecord,
    position: usize,
    project_roots: &[String],
    used_group_ids: &mut HashSet<String>,
    used_session_ids: &mut HashSet<String>,
) -> Result<(), StoreError> {
    let anchor = saved
        .acp
        .as_ref()
        .and_then(|acp| acp.sid.as_deref().or(acp.history_session_id.as_deref()))
        .or_else(|| first_typed_leaf_id(&saved.layout))
        .unwrap_or("session");
    let group_id = unique_id(
        format!("group:{position}:{anchor}"),
        used_group_ids,
        format!("group:{position}"),
    );
    let cwd = saved
        .acp
        .as_ref()
        .and_then(|acp| acp.cwd.as_deref())
        .or_else(|| first_typed_leaf_cwd(&saved.layout));
    let project_root = cwd.and_then(|cwd| matching_project_root(cwd, project_roots));

    transaction.execute(
        "INSERT INTO session_group(
               id, project_root, position, active_session_id, custom_title, last_updated_at
             ) VALUES (?1, ?2, ?3, NULL, ?4, ?5)",
        params![
            group_id,
            project_root,
            position as i64,
            saved.custom_title.as_deref(),
            saved.last_updated_at,
        ],
    )?;

    let active_session_id = if let Some(acp) = &saved.acp {
        let candidate = acp
            .sid
            .as_deref()
            .or(acp.history_session_id.as_deref())
            .unwrap_or("acp");
        let session_id = unique_id(
            candidate.to_string(),
            used_session_ids,
            format!("{group_id}:acp"),
        );
        transaction.execute(
            "INSERT INTO session(
                   id, group_id, kind, cwd, custom_title, launch_label, launch_command
                 ) VALUES (?1, ?2, 'acp', ?3, NULL, NULL, NULL)",
            params![session_id, group_id, acp.cwd.as_deref()],
        )?;
        transaction.execute(
            "INSERT INTO layout_node(
                   id, group_id, parent_id, position, kind, axis, session_id, size_px
                 ) VALUES (?1, ?2, NULL, 0, 'leaf', NULL, ?3, NULL)",
            params![format!("{group_id}:node:0"), group_id, session_id],
        )?;
        write_acp_record(transaction, &session_id, acp)?;
        session_id
    } else {
        let mut node_counter = 0usize;
        let mut leaf_ids = Vec::new();
        {
            let mut writer = LayoutWriter {
                transaction,
                group_id: &group_id,
                node_counter: &mut node_counter,
                used_session_ids,
                leaf_ids: &mut leaf_ids,
            };
            writer.write_typed(None, 0, None, &saved.layout)?;
        }
        leaf_ids
            .get(saved.active)
            .or_else(|| leaf_ids.first())
            .cloned()
            .ok_or_else(|| StoreError::SessionGroupMissingLeaf {
                group_id: group_id.clone(),
            })?
    };

    transaction.execute(
        "UPDATE session_group SET active_session_id = ?1 WHERE id = ?2",
        params![active_session_id, group_id],
    )?;

    if let Some(route) = saved
        .route_json
        .as_deref()
        .and_then(|raw| serde_json::from_slice::<Value>(raw).ok())
        .filter(|value| !value.is_null())
    {
        write_json_tree(transaction, &group_scope(&group_id), "route", &route)?;
    }
    Ok(())
}

fn first_typed_leaf_id(layout: &WorkspaceLayoutNode) -> Option<&str> {
    match layout {
        WorkspaceLayoutNode::Leaf { id, .. } => id.as_deref(),
        WorkspaceLayoutNode::Split { children, .. } => {
            children.iter().find_map(first_typed_leaf_id)
        }
    }
}

fn first_typed_leaf_cwd(layout: &WorkspaceLayoutNode) -> Option<&str> {
    match layout {
        WorkspaceLayoutNode::Leaf { cwd, .. } => cwd.as_deref(),
        WorkspaceLayoutNode::Split { children, .. } => {
            children.iter().find_map(first_typed_leaf_cwd)
        }
    }
}

struct LayoutWriter<'a, 'conn> {
    transaction: &'a Transaction<'conn>,
    group_id: &'a str,
    node_counter: &'a mut usize,
    used_session_ids: &'a mut HashSet<String>,
    leaf_ids: &'a mut Vec<String>,
}

impl LayoutWriter<'_, '_> {
    fn write_typed(
        &mut self,
        parent_id: Option<&str>,
        position: usize,
        size_px: Option<f64>,
        layout: &WorkspaceLayoutNode,
    ) -> Result<(), StoreError> {
        let node_id = format!("{}:node:{}", self.group_id, *self.node_counter);
        *self.node_counter += 1;
        if let WorkspaceLayoutNode::Split {
            vertical,
            children,
            sizes,
        } = layout
        {
            let axis = if *vertical { "vertical" } else { "horizontal" };
            self.transaction.execute(
                "INSERT INTO layout_node(
                       id, group_id, parent_id, position, kind, axis, session_id, size_px
                     ) VALUES (?1, ?2, ?3, ?4, 'split', ?5, NULL, ?6)",
                params![
                    node_id,
                    self.group_id,
                    parent_id,
                    position as i64,
                    axis,
                    size_px
                ],
            )?;
            for (child_position, child) in children.iter().enumerate() {
                let child_size = sizes.get(child_position).copied();
                self.write_typed(Some(&node_id), child_position, child_size, child)?;
            }
            return Ok(());
        }

        let WorkspaceLayoutNode::Leaf {
            cwd,
            id,
            custom_title,
            launch_label,
            launch_cmd,
        } = layout
        else {
            unreachable!("split 已处理");
        };
        let candidate = id.as_deref().unwrap_or("terminal");
        let session_id = unique_id(
            candidate.to_string(),
            self.used_session_ids,
            format!("{}:terminal:{}", self.group_id, self.leaf_ids.len()),
        );
        self.transaction.execute(
            "INSERT INTO session(
                   id, group_id, kind, cwd, custom_title, launch_label, launch_command
                 ) VALUES (?1, ?2, 'terminal', ?3, ?4, ?5, ?6)",
            params![
                session_id,
                self.group_id,
                cwd.as_deref(),
                custom_title.as_deref(),
                launch_label.as_deref(),
                launch_cmd.as_deref(),
            ],
        )?;
        self.transaction.execute(
            "INSERT INTO layout_node(
                   id, group_id, parent_id, position, kind, axis, session_id, size_px
                 ) VALUES (?1, ?2, ?3, ?4, 'leaf', NULL, ?5, ?6)",
            params![
                node_id,
                self.group_id,
                parent_id,
                position as i64,
                session_id,
                size_px
            ],
        )?;
        self.leaf_ids.push(session_id);
        Ok(())
    }
}

fn write_acp_record(
    transaction: &Transaction<'_>,
    session_id: &str,
    acp: &WorkspaceAcpRecord,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO acp_detail(
               session_id, agent, profile_id, history_session_id, launch_command,
               refresh_launch_from_settings, fork_session_id, fork_title, fork_agent,
               fork_profile_label, fork_from_history, pending_prompt, pending_delivery_id,
               agent_definition_id, automation_id
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15
             )",
        params![
            session_id,
            acp.agent,
            acp.profile_id.as_deref(),
            acp.history_session_id.as_deref(),
            acp.launch_command,
            i64::from(acp.refresh_launch_from_settings),
            acp.fork_session_id.as_deref(),
            acp.fork_title.as_deref(),
            acp.fork_agent.as_deref(),
            acp.fork_profile_label.as_deref(),
            i64::from(acp.fork_from_history),
            acp.pending_prompt.as_deref(),
            acp.pending_delivery_id.as_deref(),
            acp.agent_definition_id.as_deref(),
            acp.automation_id.as_deref(),
        ],
    )?;

    let scope = acp_scope(session_id);
    write_optional_json_tree(transaction, &scope, "env", acp.launch_env_json.as_deref())?;
    write_optional_json_tree(
        transaction,
        &scope,
        "config_values",
        acp.config_values_json.as_deref(),
    )?;
    write_optional_json_tree(
        transaction,
        &scope,
        "conversation_binding",
        acp.conversation_binding_json.as_deref(),
    )?;
    write_optional_json_tree(
        transaction,
        &scope,
        "agent_session",
        acp.agent_session_json.as_deref(),
    )?;
    write_optional_json_tree(
        transaction,
        &scope,
        "session_title",
        acp.session_title_json.as_deref(),
    )?;
    write_optional_json_tree(
        transaction,
        &scope,
        "pending_agent_preset",
        acp.pending_agent_preset_json.as_deref(),
    )?;
    Ok(())
}

fn write_optional_json_tree(
    transaction: &Transaction<'_>,
    scope: &str,
    key: &str,
    raw: Option<&[u8]>,
) -> Result<(), StoreError> {
    let Some(raw) = raw else {
        return Ok(());
    };
    let value: Value = serde_json::from_slice(raw)?;
    write_json_tree(transaction, scope, key, &value)
}

fn read_workspace(connection: &Connection) -> Result<Option<Vec<u8>>, StoreError> {
    let Some(snapshot) = read_workspace_snapshot(connection)? else {
        return Ok(None);
    };
    to_vec(&json_from_workspace_snapshot(&snapshot)?)
}

fn read_workspace_snapshot_from_generic_document(
    connection: &Connection,
) -> Result<Option<WorkspaceSnapshot>, StoreError> {
    let Some(raw) = read_generic_document(connection, "workspace.json")? else {
        return Ok(None);
    };
    let value: Value = serde_json::from_slice(&raw)?;
    workspace_from_document(&value).map(Some)
}

pub(crate) fn read_workspace_snapshot(
    connection: &Connection,
) -> Result<Option<WorkspaceSnapshot>, StoreError> {
    let has_meta = scope_exists(connection, WORKSPACE_SCOPE)?;
    let has_projects: bool =
        connection.query_row("SELECT EXISTS(SELECT 1 FROM project)", [], |row| row.get(0))?;
    let has_groups: bool =
        connection.query_row("SELECT EXISTS(SELECT 1 FROM session_group)", [], |row| {
            row.get(0)
        })?;
    if !has_meta && !has_projects && !has_groups {
        // 关系表尚未落盘时，仍可能只存在旧的 `document:workspace.json` kv 树。
        // appearance 已经有这条回退；workspace 漏了会把「库在、会话在 json 文档里」
        // 读成空工作区，首屏像新装的 App。
        return read_workspace_snapshot_from_generic_document(connection);
    }

    let state = read_scoped_object(connection, WORKSPACE_SCOPE)?.unwrap_or_default();
    let mut project_statement =
        connection.prepare("SELECT root, collapsed FROM project ORDER BY position")?;
    let project_rows = project_statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut projects = Vec::new();
    let mut collapsed_projects = Vec::new();
    for row in project_rows {
        let (root, collapsed) = row?;
        if collapsed != 0 {
            collapsed_projects.push(Value::String(root.clone()));
        }
        projects.push(Value::String(root));
    }

    let mut group_statement = connection.prepare(
        "SELECT id, active_session_id, custom_title, last_updated_at
             FROM session_group ORDER BY position",
    )?;
    let group_rows = group_statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut sessions = Vec::new();
    for row in group_rows {
        let (group_id, active_session_id, custom_title, last_updated_at) = row?;
        sessions.push(session_from_value(&read_session_group(
            connection,
            &group_id,
            active_session_id.as_deref(),
            custom_title,
            last_updated_at,
        )?)?);
    }
    let mut project_records = Vec::new();
    for project in projects {
        let Some(root) = project.as_str() else {
            continue;
        };
        project_records.push(WorkspaceProjectRecord {
            root: root.to_string(),
            collapsed: collapsed_projects
                .iter()
                .any(|value| value.as_str() == Some(root)),
        });
    }
    Ok(Some(WorkspaceSnapshot {
        active_session: state
            .get("active_session")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        active_session_id: as_str(state.get("active_session_id")).map(str::to_string),
        route_json: optional_json_bytes(state.get("route")),
        selected_agent_id: as_str(state.get("selected_agent_id")).map(str::to_string),
        active_workspace_surface: as_str(state.get("active_workspace_surface")).map(str::to_string),
        workspace_surface_titles_json: optional_json_bytes(state.get("workspace_surface_titles")),
        sidebar_w: state.get("sidebar_w").and_then(Value::as_f64),
        sidebar_open: state.get("sidebar_open").and_then(Value::as_bool),
        sidebar_grouping: as_str(state.get("sidebar_grouping")).map(str::to_string),
        collapsed_agents: string_list(state.get("collapsed_agents")),
        pinned_projects: string_list(state.get("pinned_projects")),
        sidebar_hide_empty_projects: state
            .get("sidebar_hide_empty_projects")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        projects: project_records,
        sessions,
    }))
}

pub(crate) fn json_from_workspace_snapshot(
    snapshot: &WorkspaceSnapshot,
) -> Result<Value, StoreError> {
    let mut state = workspace_meta_object(snapshot);
    state.insert(
        "projects".into(),
        json!(
            snapshot
                .projects
                .iter()
                .map(|project| project.root.clone())
                .collect::<Vec<_>>()
        ),
    );
    state.insert(
        "collapsed_projects".into(),
        json!(
            snapshot
                .projects
                .iter()
                .filter(|project| project.collapsed)
                .map(|project| project.root.clone())
                .collect::<Vec<_>>()
        ),
    );
    let sessions = snapshot
        .sessions
        .iter()
        .map(|session| {
            json!({
                "custom_title": session.custom_title,
                "last_updated_at": session.last_updated_at,
                "active": session.active,
                "layout": layout_to_value(&session.layout),
                "acp": session.acp.as_ref().map(acp_to_value),
                "route": session
                    .route_json
                    .as_deref()
                    .and_then(|raw| serde_json::from_slice::<Value>(raw).ok())
                    .unwrap_or(Value::Null),
            })
        })
        .collect::<Vec<_>>();
    state.insert("sessions".into(), Value::Array(sessions));
    Ok(Value::Object(state))
}

fn layout_to_value(layout: &WorkspaceLayoutNode) -> Value {
    match layout {
        WorkspaceLayoutNode::Leaf {
            cwd,
            id,
            custom_title,
            launch_label,
            launch_cmd,
        } => json!({
            "Leaf": {
                "cwd": cwd,
                "id": id,
                "custom_title": custom_title,
                "launch_label": launch_label,
                "launch_cmd": launch_cmd,
            }
        }),
        WorkspaceLayoutNode::Split {
            vertical,
            children,
            sizes,
        } => json!({
            "Split": {
                "axis": if *vertical { "V" } else { "H" },
                "children": children.iter().map(layout_to_value).collect::<Vec<_>>(),
                "sizes": sizes,
            }
        }),
    }
}

fn acp_to_value(acp: &WorkspaceAcpRecord) -> Value {
    let mut value = json!({
        "cwd": acp.cwd,
        "sid": acp.sid,
        "agent": acp.agent,
        "profile_id": acp.profile_id,
        "history_session_id": acp.history_session_id,
        "launch": {
            "command": acp.launch_command,
            "env": acp
                .launch_env_json
                .as_deref()
                .and_then(|raw| serde_json::from_slice::<Value>(raw).ok())
                .unwrap_or_else(|| json!({})),
        },
        "refresh_launch_from_settings": acp.refresh_launch_from_settings,
        "pending_prompt": acp.pending_prompt,
        "pending_delivery_id": acp.pending_delivery_id,
        "agent_definition_id": acp.agent_definition_id,
        "automation_id": acp.automation_id,
    });
    let object = value.as_object_mut().expect("acp object");
    if let (Some(session_id), Some(title)) = (&acp.fork_session_id, &acp.fork_title) {
        object.insert(
            "fork_origin".into(),
            json!({
                "session_id": session_id,
                "title": title,
                "agent": acp.fork_agent,
                "profile_label": acp.fork_profile_label,
                "from_history": acp.fork_from_history,
            }),
        );
    }
    for (key, raw) in [
        ("config_values", acp.config_values_json.as_deref()),
        (
            "conversation_binding",
            acp.conversation_binding_json.as_deref(),
        ),
        ("agent_session", acp.agent_session_json.as_deref()),
        (
            "pending_agent_preset",
            acp.pending_agent_preset_json.as_deref(),
        ),
        ("session_title", acp.session_title_json.as_deref()),
    ] {
        if let Some(raw) = raw
            && let Ok(parsed) = serde_json::from_slice::<Value>(raw)
        {
            object.insert(key.into(), parsed);
        }
    }
    value
}

fn read_session_group(
    connection: &Connection,
    group_id: &str,
    active_session_id: Option<&str>,
    custom_title: Option<String>,
    last_updated_at: i64,
) -> Result<Value, StoreError> {
    let root_id = connection.query_row(
        "SELECT id FROM layout_node
             WHERE group_id = ?1 AND parent_id IS NULL",
        [group_id],
        |row| row.get::<_, String>(0),
    )?;
    let mut leaf_ids = Vec::new();
    let layout = read_layout_node(connection, group_id, &root_id, &mut leaf_ids)?;
    let active = active_session_id
        .and_then(|active| leaf_ids.iter().position(|session_id| session_id == active))
        .unwrap_or(0);
    let acp_session = connection
        .query_row(
            "SELECT id FROM session WHERE group_id = ?1 AND kind = 'acp' LIMIT 1",
            [group_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let acp = acp_session
        .as_deref()
        .map(|session_id| read_acp_detail(connection, session_id))
        .transpose()?
        .unwrap_or(Value::Null);
    let route = read_json_tree(connection, &group_scope(group_id), "route")?.unwrap_or(Value::Null);
    Ok(json!({
        "layout": layout,
        "active": active,
        "last_updated_at": last_updated_at.max(0),
        "custom_title": custom_title,
        "acp": acp,
        "route": route,
    }))
}

fn read_layout_node(
    connection: &Connection,
    group_id: &str,
    node_id: &str,
    leaf_ids: &mut Vec<String>,
) -> Result<Value, StoreError> {
    let (kind, axis, session_id) = connection.query_row(
        "SELECT kind, axis, session_id FROM layout_node WHERE id = ?1 AND group_id = ?2",
        params![node_id, group_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        },
    )?;
    if kind == "leaf" {
        let session_id = session_id.ok_or_else(|| StoreError::LayoutLeafMissingSession {
            node_id: node_id.to_string(),
        })?;
        let (session_kind, cwd, custom_title, launch_label, launch_command) = connection
            .query_row(
                "SELECT kind, cwd, custom_title, launch_label, launch_command
                 FROM session WHERE id = ?1",
                [&session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )?;
        leaf_ids.push(session_id.clone());
        return Ok(json!({
            "Leaf": {
                "cwd": cwd,
                "id": if session_kind == "terminal" { Some(session_id) } else { None },
                "custom_title": custom_title,
                "launch_label": launch_label,
                "launch_cmd": launch_command,
            }
        }));
    }

    let mut statement = connection.prepare(
        "SELECT id, size_px FROM layout_node
             WHERE group_id = ?1 AND parent_id = ?2 ORDER BY position",
    )?;
    let rows = statement.query_map(params![group_id, node_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<f64>>(1)?))
    })?;
    let children_with_sizes = rows.collect::<Result<Vec<_>, _>>()?;
    let all_sized = !children_with_sizes.is_empty()
        && children_with_sizes.iter().all(|(_, size)| size.is_some());
    let mut children = Vec::new();
    let mut sizes = Vec::new();
    for (child_id, size) in children_with_sizes {
        children.push(read_layout_node(connection, group_id, &child_id, leaf_ids)?);
        if all_sized {
            sizes.push(json!(size.unwrap_or_default()));
        }
    }
    Ok(json!({
        "Split": {
            "axis": if axis.as_deref() == Some("vertical") { "V" } else { "H" },
            "children": children,
            "sizes": sizes,
        }
    }))
}

struct AcpRow {
    agent: String,
    profile_id: Option<String>,
    history_session_id: Option<String>,
    launch_command: String,
    refresh_launch_from_settings: bool,
    fork_session_id: Option<String>,
    fork_title: Option<String>,
    fork_agent: Option<String>,
    fork_profile_label: Option<String>,
    fork_from_history: bool,
    pending_prompt: Option<String>,
    pending_delivery_id: Option<String>,
    agent_definition_id: Option<String>,
    automation_id: Option<String>,
}

fn read_acp_detail(connection: &Connection, session_id: &str) -> Result<Value, StoreError> {
    let (cwd, row) = connection.query_row(
        "SELECT s.cwd, d.agent, d.profile_id, d.history_session_id, d.launch_command,
                    d.refresh_launch_from_settings, d.fork_session_id, d.fork_title,
                    d.fork_agent, d.fork_profile_label, d.fork_from_history,
                    d.pending_prompt, d.pending_delivery_id,
                    d.agent_definition_id, d.automation_id
             FROM acp_detail d JOIN session s ON s.id = d.session_id
             WHERE d.session_id = ?1",
        [session_id],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                AcpRow {
                    agent: row.get(1)?,
                    profile_id: row.get(2)?,
                    history_session_id: row.get(3)?,
                    launch_command: row.get(4)?,
                    refresh_launch_from_settings: row.get::<_, i64>(5)? != 0,
                    fork_session_id: row.get(6)?,
                    fork_title: row.get(7)?,
                    fork_agent: row.get(8)?,
                    fork_profile_label: row.get(9)?,
                    fork_from_history: row.get::<_, i64>(10)? != 0,
                    pending_prompt: row.get(11)?,
                    pending_delivery_id: row.get(12)?,
                    agent_definition_id: row.get(13)?,
                    automation_id: row.get(14)?,
                },
            ))
        },
    )?;
    let scope = acp_scope(session_id);
    let env = read_json_tree(connection, &scope, "env")?.unwrap_or_else(|| json!({}));
    let config_values =
        read_json_tree(connection, &scope, "config_values")?.unwrap_or_else(|| json!([]));
    let conversation_binding = read_json_tree(connection, &scope, "conversation_binding")?
        .unwrap_or_else(|| json!({"type": "direct"}));
    let agent_session = read_json_tree(connection, &scope, "agent_session")?.unwrap_or(Value::Null);
    let pending_agent_preset =
        read_json_tree(connection, &scope, "pending_agent_preset")?.unwrap_or(Value::Null);
    let session_title = read_json_tree(connection, &scope, "session_title")?.unwrap_or(Value::Null);
    let fork_origin = match (row.fork_session_id, row.fork_title) {
        (Some(origin_session_id), Some(title)) => json!({
            "session_id": origin_session_id,
            "title": title,
            "agent": row.fork_agent,
            "profile_label": row.fork_profile_label,
            "from_history": row.fork_from_history,
        }),
        _ => Value::Null,
    };
    Ok(json!({
        "cwd": cwd,
        "launch": {"command": row.launch_command, "env": env},
        "profile_id": row.profile_id,
        "agent": row.agent,
        "history_session_id": row.history_session_id,
        "sid": session_id,
        "refresh_launch_from_settings": row.refresh_launch_from_settings,
        "fork_origin": fork_origin,
        "conversation_binding": conversation_binding,
        "agent_session": agent_session,
        "config_values": config_values,
        "pending_prompt": row.pending_prompt,
        "pending_delivery_id": row.pending_delivery_id,
        "pending_agent_preset": pending_agent_preset,
        "session_title": session_title,
        "agent_definition_id": row.agent_definition_id,
        "automation_id": row.automation_id,
    }))
}

fn default_leaf_layout() -> Value {
    json!({
        "Leaf": {
            "cwd": null,
            "id": null,
            "custom_title": null,
            "launch_label": null,
            "launch_cmd": null
        }
    })
}

fn matching_project_root<'a>(cwd: &str, roots: &'a [String]) -> Option<&'a str> {
    roots
        .iter()
        .filter(|root| path_contains(root, cwd))
        .max_by_key(|root| root.len())
        .map(String::as_str)
}

fn path_contains(root: &str, path: &str) -> bool {
    let root = root.trim_end_matches(['/', '\\']);
    let path = path.trim_end_matches(['/', '\\']);
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/') || rest.starts_with('\\'))
}

fn unique_id(candidate: String, used: &mut HashSet<String>, fallback: String) -> String {
    let base = if candidate.trim().is_empty() {
        fallback
    } else {
        candidate
    };
    if used.insert(base.clone()) {
        return base;
    }
    for suffix in 2usize.. {
        let value = format!("{base}#{suffix}");
        if used.insert(value.clone()) {
            return value;
        }
    }
    unreachable!()
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

fn as_i64(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64).or_else(|| {
        value
            .and_then(Value::as_u64)
            .and_then(|value| i64::try_from(value).ok())
    })
}

fn as_usize(value: Option<&Value>) -> Option<usize> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn to_vec(value: &Value) -> Result<Option<Vec<u8>>, StoreError> {
    serde_json::to_vec(value)
        .map(Some)
        .map_err(StoreError::from)
}
