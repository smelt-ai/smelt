//! 对端节点：和本节点双向配对过的其它运行时（见 `docs/cross-machine.md`）。

use crate::{PeerNodeRecord, StoreError};
use rusqlite::{Connection, OptionalExtension as _, Row, Transaction, params};

const COLUMNS: &str = "runtime_id, endpoint_id, name, relay_url, os, agent_access, added_at_ms";

fn record(row: &Row<'_>) -> rusqlite::Result<PeerNodeRecord> {
    Ok(PeerNodeRecord {
        runtime_id: row.get(0)?,
        endpoint_id: row.get(1)?,
        name: row.get(2)?,
        relay_url: row.get(3)?,
        os: row.get(4)?,
        agent_access: row.get(5)?,
        added_at_ms: row.get(6)?,
    })
}

pub(crate) fn list(connection: &Connection) -> Result<Vec<PeerNodeRecord>, StoreError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {COLUMNS} FROM peer_node ORDER BY added_at_ms, runtime_id"
    ))?;
    let rows = statement.query_map([], record)?;
    rows.collect::<Result<_, _>>().map_err(StoreError::from)
}

/// `key` 只能是 `runtime_id` 或 `endpoint_id`。
pub(crate) fn get(
    connection: &Connection,
    key: &'static str,
    value: &str,
) -> Result<Option<PeerNodeRecord>, StoreError> {
    debug_assert!(matches!(key, "runtime_id" | "endpoint_id"));
    connection
        .query_row(
            &format!("SELECT {COLUMNS} FROM peer_node WHERE {key} = ?1"),
            params![value],
            record,
        )
        .optional()
        .map_err(StoreError::from)
}

pub(crate) fn put(transaction: &Transaction<'_>, peer: &PeerNodeRecord) -> Result<(), StoreError> {
    transaction.execute(
        "DELETE FROM peer_node WHERE runtime_id = ?1 OR endpoint_id = ?2",
        params![peer.runtime_id, peer.endpoint_id],
    )?;
    transaction.execute(
        &format!("INSERT INTO peer_node ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"),
        params![
            peer.runtime_id,
            peer.endpoint_id,
            peer.name,
            peer.relay_url,
            peer.os,
            peer.agent_access,
            peer.added_at_ms,
        ],
    )?;
    Ok(())
}

pub(crate) fn delete(transaction: &Transaction<'_>, runtime_id: &str) -> Result<bool, StoreError> {
    let changed = transaction.execute(
        "DELETE FROM peer_node WHERE runtime_id = ?1",
        params![runtime_id],
    )?;
    Ok(changed > 0)
}
