//! 历史会话的元数据：用户改过的标题，以及「这条历史会话由哪个智能体创建」。

use crate::{HistoryTitleRecord, HistoryTitleSnapshot, StoreError};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::BTreeMap;

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

pub(crate) fn read_history_agent_binding(
    connection: &Connection,
    agent: &str,
    profile_id: &str,
    resume_id: &str,
) -> Result<Option<String>, StoreError> {
    connection
        .query_row(
            "SELECT agent_definition_id FROM history_agent_binding
             WHERE agent = ?1 AND profile_id = ?2 AND resume_id = ?3",
            params![agent, profile_id, resume_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::from)
}

pub(crate) fn read_history_agent_bindings(
    connection: &Connection,
    agent: &str,
    profile_id: &str,
) -> Result<BTreeMap<String, String>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT resume_id, agent_definition_id FROM history_agent_binding
         WHERE agent = ?1 AND profile_id = ?2",
    )?;
    let rows = statement.query_map(params![agent, profile_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(StoreError::from)
}

pub(crate) fn write_history_agent_binding(
    transaction: &Transaction<'_>,
    agent: &str,
    profile_id: &str,
    resume_id: &str,
    agent_definition_id: &str,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO history_agent_binding(agent, profile_id, resume_id, agent_definition_id)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(agent, profile_id, resume_id) DO UPDATE SET
           agent_definition_id = excluded.agent_definition_id",
        params![agent, profile_id, resume_id, agent_definition_id],
    )?;
    Ok(())
}
