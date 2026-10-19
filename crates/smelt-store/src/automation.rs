//! 自动化：定义、调度状态、Run 和元数据。触发器、动作等嵌套配置仍以 JSON 字节放在对应列。

use crate::{
    AutomationRecord, AutomationRunRecord, AutomationSnapshot, AutomationStateRecord, StoreError,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params, params_from_iter};

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

pub(crate) fn write_automation_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &AutomationSnapshot,
) -> Result<(), StoreError> {
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
