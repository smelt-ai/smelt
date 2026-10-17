use crate::{
    AutomationRecord, AutomationRunRecord, AutomationSnapshot, AutomationStateRecord,
    HistoryTitleRecord, HistoryTitleSnapshot, LaunchEntryRecord, LaunchSnapshot, StoreError,
    WorkspaceAcpRecord, WorkspaceLayoutNode, WorkspaceSessionRecord, WorkspaceSnapshot,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params, params_from_iter};
use serde_json::{Map, Number, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

const LAUNCH_SCOPE: &str = "launch";
const WORKSPACE_SCOPE: &str = "workspace";
const DOCUMENT_SCOPE_PREFIX: &str = "document:";
const ACP_SCOPE_PREFIX: &str = "acp:";

#[derive(Clone)]
struct KvNode {
    value_type: String,
    value: Option<String>,
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

fn acp_scope(session_id: &str) -> String {
    format!("{ACP_SCOPE_PREFIX}{session_id}")
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

/// 行已经存在时，恢复身份以库里的值为准。窗口快照可以在第一次插入时填上，
/// 之后不能改写或清空；换 id、放弃旧 id 只走守护进程的显式写入。
fn preserved_history_session_ids(
    transaction: &Transaction<'_>,
) -> Result<HashMap<String, Option<String>>, StoreError> {
    let mut statement =
        transaction.prepare("SELECT session_id, history_session_id FROM acp_detail")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    let mut preserved = HashMap::new();
    for row in rows {
        let (session_id, history_session_id) = row?;
        preserved.insert(session_id, history_session_id);
    }
    Ok(preserved)
}

fn normalize_history_id(id: Option<&str>) -> Option<String> {
    id.map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn history_session_id_for_rewrite(
    session_id: &str,
    preserved: &HashMap<String, Option<String>>,
) -> Option<String> {
    // 没有行，或列还是空，第一次落库也不采用窗口载荷。
    // 这一列只由守护进程 remember / clear 来写。
    preserved
        .get(session_id)
        .and_then(|stored| normalize_history_id(stored.as_deref()))
}

pub(crate) fn write_workspace_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &WorkspaceSnapshot,
) -> Result<(), StoreError> {
    let preserved_history = preserved_history_session_ids(transaction)?;
    delete_workspace(transaction)?;
    delete_document_scope(transaction, "workspace.json")?;

    let mut project_roots = Vec::new();
    let mut seen_projects = HashSet::new();
    for root in &snapshot.projects {
        let root = root.trim();
        if root.is_empty() || !seen_projects.insert(root.to_string()) {
            continue;
        }
        let position = project_roots.len() as i64;
        transaction.execute(
            "INSERT INTO project(root, position) VALUES (?1, ?2)",
            params![root, position],
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
            &preserved_history,
        )?;
    }
    Ok(())
}

pub(crate) fn workspace_from_document(value: &Value) -> Result<WorkspaceSnapshot, StoreError> {
    let mut projects = Vec::new();
    let mut seen = HashSet::new();
    for root in string_list(value.get("projects")) {
        if !root.trim().is_empty() && seen.insert(root.clone()) {
            projects.push(root);
        }
    }
    Ok(WorkspaceSnapshot {
        projects,
        sessions: value
            .get("sessions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
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
        // 分叉来源不再属于对话。写入时清空旧列，避免下次读回。
        fork_session_id: None,
        fork_title: None,
        fork_agent: None,
        fork_profile_label: None,
        fork_from_history: false,
        pending_prompt: as_str(value.get("pending_prompt")).map(str::to_string),
        pending_delivery_id: as_str(value.get("pending_delivery_id")).map(str::to_string),
        agent_definition_id: as_str(value.get("agent_definition_id")).map(str::to_string),
        automation_id: as_str(value.get("automation_id")).map(str::to_string),
        config_values_json: optional_json_bytes(value.get("config_values")),
        conversation_binding_json: optional_json_bytes(value.get("conversation_binding")),
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
    preserved_history: &HashMap<String, Option<String>>,
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
        let history_key = acp.sid.as_deref().unwrap_or(session_id.as_str());
        let history_session_id = history_session_id_for_rewrite(history_key, preserved_history);
        write_acp_record(transaction, &session_id, acp, history_session_id.as_deref())?;
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
    history_session_id: Option<&str>,
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
            history_session_id,
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

pub(crate) fn remember_history_session_id(
    connection: &Connection,
    session_id: &str,
    history_id: &str,
) -> Result<bool, StoreError> {
    let Some(session_id) = normalize_history_id(Some(session_id)) else {
        return Ok(false);
    };
    let Some(history_id) = normalize_history_id(Some(history_id)) else {
        return Ok(false);
    };
    let updated = connection.execute(
        "UPDATE acp_detail SET history_session_id = ?1 WHERE session_id = ?2",
        params![history_id, session_id],
    )?;
    Ok(updated > 0)
}

pub(crate) fn clear_history_session_id(
    connection: &Connection,
    session_id: &str,
) -> Result<bool, StoreError> {
    let Some(session_id) = normalize_history_id(Some(session_id)) else {
        return Ok(false);
    };
    let updated = connection.execute(
        "UPDATE acp_detail SET history_session_id = NULL WHERE session_id = ?1",
        [session_id],
    )?;
    Ok(updated > 0)
}

pub(crate) fn history_session_id(
    connection: &Connection,
    session_id: &str,
) -> Result<Option<String>, StoreError> {
    Ok(match stored_history_identity(connection, session_id)? {
        crate::StoredHistoryIdentity::Bound(id) => Some(id),
        crate::StoredHistoryIdentity::NoRow | crate::StoredHistoryIdentity::Unbound => None,
    })
}

pub(crate) fn stored_history_identity(
    connection: &Connection,
    session_id: &str,
) -> Result<crate::StoredHistoryIdentity, StoreError> {
    let Some(session_id) = normalize_history_id(Some(session_id)) else {
        return Ok(crate::StoredHistoryIdentity::NoRow);
    };
    let stored = connection
        .query_row(
            "SELECT history_session_id FROM acp_detail WHERE session_id = ?1",
            [session_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    Ok(match stored {
        None => crate::StoredHistoryIdentity::NoRow,
        Some(id) => match normalize_history_id(id.as_deref()) {
            Some(id) => crate::StoredHistoryIdentity::Bound(id),
            None => crate::StoredHistoryIdentity::Unbound,
        },
    })
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

pub(crate) fn read_workspace_snapshot(
    connection: &Connection,
) -> Result<Option<WorkspaceSnapshot>, StoreError> {
    let has_projects: bool =
        connection.query_row("SELECT EXISTS(SELECT 1 FROM project)", [], |row| row.get(0))?;
    let has_groups: bool =
        connection.query_row("SELECT EXISTS(SELECT 1 FROM session_group)", [], |row| {
            row.get(0)
        })?;
    if !has_projects && !has_groups {
        return Ok(None);
    }

    let mut project_statement = connection.prepare("SELECT root FROM project ORDER BY position")?;
    let projects = project_statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;

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
    let groups = group_rows.collect::<Result<Vec<_>, _>>()?;
    let cache = load_workspace_read_cache(connection)?;
    let mut sessions = Vec::new();
    for (group_id, active_session_id, custom_title, last_updated_at) in groups {
        sessions.push(session_from_value(&read_session_group(
            &cache,
            &group_id,
            active_session_id.as_deref(),
            custom_title,
            last_updated_at,
        )?)?);
    }
    Ok(Some(WorkspaceSnapshot { projects, sessions }))
}

pub(crate) fn json_from_workspace_snapshot(
    snapshot: &WorkspaceSnapshot,
) -> Result<Value, StoreError> {
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
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "projects": snapshot.projects,
        "sessions": sessions,
    }))
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
    for (key, raw) in [
        ("config_values", acp.config_values_json.as_deref()),
        (
            "conversation_binding",
            acp.conversation_binding_json.as_deref(),
        ),
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

#[derive(Clone)]
struct LayoutNodeRow {
    id: String,
    group_id: String,
    parent_id: Option<String>,
    position: i64,
    kind: String,
    axis: Option<String>,
    session_id: Option<String>,
    size_px: Option<f64>,
}

struct SessionSqlRow {
    kind: String,
    cwd: Option<String>,
    custom_title: Option<String>,
    launch_label: Option<String>,
    launch_command: Option<String>,
}

struct WorkspaceReadCache {
    layout_children: HashMap<(String, String), Vec<LayoutNodeRow>>,
    layout_by_id: HashMap<String, LayoutNodeRow>,
    sessions: HashMap<String, SessionSqlRow>,
    acp_session_by_group: HashMap<String, String>,
    acp_rows: HashMap<String, AcpRow>,
    kv: HashMap<String, BTreeMap<String, KvNode>>,
}

fn load_workspace_read_cache(connection: &Connection) -> Result<WorkspaceReadCache, StoreError> {
    let mut layout_children: HashMap<(String, String), Vec<LayoutNodeRow>> = HashMap::new();
    let mut layout_by_id = HashMap::new();
    let mut layout_statement = connection.prepare(
        "SELECT id, group_id, parent_id, position, kind, axis, session_id, size_px
         FROM layout_node",
    )?;
    let layout_rows = layout_statement.query_map([], |row| {
        Ok(LayoutNodeRow {
            id: row.get(0)?,
            group_id: row.get(1)?,
            parent_id: row.get(2)?,
            position: row.get(3)?,
            kind: row.get(4)?,
            axis: row.get(5)?,
            session_id: row.get(6)?,
            size_px: row.get(7)?,
        })
    })?;
    for row in layout_rows {
        let node = row?;
        layout_by_id.insert(node.id.clone(), node.clone());
        let parent_key = node.parent_id.clone().unwrap_or_default();
        layout_children
            .entry((node.group_id.clone(), parent_key))
            .or_default()
            .push(node);
    }
    for children in layout_children.values_mut() {
        children.sort_by_key(|node| node.position);
    }

    let mut sessions = HashMap::new();
    let mut acp_session_by_group = HashMap::new();
    let mut session_statement = connection.prepare(
        "SELECT id, group_id, kind, cwd, custom_title, launch_label, launch_command
         FROM session ORDER BY rowid",
    )?;
    let session_rows = session_statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            SessionSqlRow {
                kind: row.get(2)?,
                cwd: row.get(3)?,
                custom_title: row.get(4)?,
                launch_label: row.get(5)?,
                launch_command: row.get(6)?,
            },
        ))
    })?;
    for row in session_rows {
        let (id, group_id, session) = row?;
        if session.kind == "acp" {
            acp_session_by_group
                .entry(group_id)
                .or_insert_with(|| id.clone());
        }
        sessions.insert(id, session);
    }

    let mut acp_rows = HashMap::new();
    let mut acp_statement = connection.prepare(
        "SELECT session_id, agent, profile_id, history_session_id, launch_command,
                refresh_launch_from_settings, pending_prompt, pending_delivery_id,
                agent_definition_id, automation_id
         FROM acp_detail",
    )?;
    let acp_detail_rows = acp_statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            AcpRow {
                agent: row.get(1)?,
                profile_id: row.get(2)?,
                history_session_id: row.get(3)?,
                launch_command: row.get(4)?,
                refresh_launch_from_settings: row.get::<_, i64>(5)? != 0,
                pending_prompt: row.get(6)?,
                pending_delivery_id: row.get(7)?,
                agent_definition_id: row.get(8)?,
                automation_id: row.get(9)?,
            },
        ))
    })?;
    for row in acp_detail_rows {
        let (session_id, detail) = row?;
        acp_rows.insert(session_id, detail);
    }

    let mut kv: HashMap<String, BTreeMap<String, KvNode>> = HashMap::new();
    let mut kv_statement = connection.prepare(
        "SELECT scope, key, value_type,
                CASE WHEN value_type = 'blob' THEN NULL ELSE CAST(value AS TEXT) END
         FROM kv
         WHERE scope LIKE 'acp:%'
         ORDER BY scope, key",
    )?;
    let kv_rows = kv_statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            KvNode {
                value_type: row.get(2)?,
                value: row.get(3)?,
            },
        ))
    })?;
    for row in kv_rows {
        let (scope, key, node) = row?;
        kv.entry(scope).or_default().insert(key, node);
    }

    Ok(WorkspaceReadCache {
        layout_children,
        layout_by_id,
        sessions,
        acp_session_by_group,
        acp_rows,
        kv,
    })
}

fn cached_json(
    cache: &WorkspaceReadCache,
    scope: &str,
    root: &str,
) -> Result<Option<Value>, StoreError> {
    let Some(nodes) = cache.kv.get(scope) else {
        return Ok(None);
    };
    decode_json_tree(nodes, root)
}

pub(crate) fn read_session_agent_definition_ids(
    connection: &Connection,
) -> Result<Option<BTreeMap<String, String>>, StoreError> {
    let has_groups: bool =
        connection.query_row("SELECT EXISTS(SELECT 1 FROM session_group)", [], |row| {
            row.get(0)
        })?;
    if !has_groups {
        return Ok(None);
    }
    let mut statement = connection.prepare(
        "SELECT session_id, agent_definition_id FROM acp_detail
         WHERE agent_definition_id IS NOT NULL AND agent_definition_id != ''",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut map = BTreeMap::new();
    for row in rows {
        let (sid, id) = row?;
        let sid = sid.trim();
        let id = id.trim();
        if sid.is_empty() || id.is_empty() {
            continue;
        }
        map.insert(sid.to_string(), id.to_string());
    }
    Ok(Some(map))
}

fn read_session_group(
    cache: &WorkspaceReadCache,
    group_id: &str,
    active_session_id: Option<&str>,
    custom_title: Option<String>,
    last_updated_at: i64,
) -> Result<Value, StoreError> {
    let root_id = cache
        .layout_children
        .get(&(group_id.to_string(), String::new()))
        .and_then(|children| children.first())
        .map(|node| node.id.clone())
        .ok_or_else(|| StoreError::from(rusqlite::Error::QueryReturnedNoRows))?;
    let mut leaf_ids = Vec::new();
    let layout = read_layout_node(cache, group_id, &root_id, &mut leaf_ids)?;
    let active = active_session_id
        .and_then(|active| leaf_ids.iter().position(|session_id| session_id == active))
        .unwrap_or(0);
    let acp = cache
        .acp_session_by_group
        .get(group_id)
        .map(|session_id| read_acp_detail(cache, session_id))
        .transpose()?
        .unwrap_or(Value::Null);
    Ok(json!({
        "layout": layout,
        "active": active,
        "last_updated_at": last_updated_at.max(0),
        "custom_title": custom_title,
        "acp": acp,
    }))
}

fn read_layout_node(
    cache: &WorkspaceReadCache,
    group_id: &str,
    node_id: &str,
    leaf_ids: &mut Vec<String>,
) -> Result<Value, StoreError> {
    let node = cache
        .layout_by_id
        .get(node_id)
        .filter(|node| node.group_id == group_id)
        .ok_or_else(|| StoreError::from(rusqlite::Error::QueryReturnedNoRows))?;
    let kind = node.kind.clone();
    let axis = node.axis.clone();
    let session_id = node.session_id.clone();
    if kind == "leaf" {
        let session_id = session_id.ok_or_else(|| StoreError::LayoutLeafMissingSession {
            node_id: node_id.to_string(),
        })?;
        let session = cache
            .sessions
            .get(&session_id)
            .ok_or_else(|| StoreError::from(rusqlite::Error::QueryReturnedNoRows))?;
        leaf_ids.push(session_id.clone());
        return Ok(json!({
            "Leaf": {
                "cwd": session.cwd.clone(),
                "id": if session.kind == "terminal" { Some(session_id) } else { None },
                "custom_title": session.custom_title.clone(),
                "launch_label": session.launch_label.clone(),
                "launch_cmd": session.launch_command.clone(),
            }
        }));
    }

    let empty = Vec::new();
    let children_with_sizes = cache
        .layout_children
        .get(&(group_id.to_string(), node_id.to_string()))
        .unwrap_or(&empty);
    let all_sized = !children_with_sizes.is_empty()
        && children_with_sizes
            .iter()
            .all(|child| child.size_px.is_some());
    let mut children = Vec::new();
    let mut sizes = Vec::new();
    for child in children_with_sizes {
        children.push(read_layout_node(cache, group_id, &child.id, leaf_ids)?);
        if all_sized {
            sizes.push(json!(child.size_px.unwrap_or_default()));
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

#[derive(Clone)]
struct AcpRow {
    agent: String,
    profile_id: Option<String>,
    history_session_id: Option<String>,
    launch_command: String,
    refresh_launch_from_settings: bool,
    pending_prompt: Option<String>,
    pending_delivery_id: Option<String>,
    agent_definition_id: Option<String>,
    automation_id: Option<String>,
}

fn read_acp_detail(cache: &WorkspaceReadCache, session_id: &str) -> Result<Value, StoreError> {
    let row = cache
        .acp_rows
        .get(session_id)
        .ok_or_else(|| StoreError::from(rusqlite::Error::QueryReturnedNoRows))?
        .clone();
    let cwd = cache
        .sessions
        .get(session_id)
        .and_then(|session| session.cwd.clone());
    let scope = acp_scope(session_id);
    let env = cached_json(cache, &scope, "env")?.unwrap_or_else(|| json!({}));
    let config_values = cached_json(cache, &scope, "config_values")?.unwrap_or_else(|| json!([]));
    let conversation_binding = cached_json(cache, &scope, "conversation_binding")?
        .unwrap_or_else(|| json!({"type": "direct"}));
    let pending_agent_preset =
        cached_json(cache, &scope, "pending_agent_preset")?.unwrap_or(Value::Null);
    let session_title = cached_json(cache, &scope, "session_title")?.unwrap_or(Value::Null);
    Ok(json!({
        "cwd": cwd,
        "launch": {"command": row.launch_command, "env": env},
        "profile_id": row.profile_id,
        "agent": row.agent,
        "history_session_id": row.history_session_id,
        "sid": session_id,
        "refresh_launch_from_settings": row.refresh_launch_from_settings,
        "conversation_binding": conversation_binding,
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
