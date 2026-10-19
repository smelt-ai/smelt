//! 已配对设备：远程客户端按 iroh endpoint id 进白名单（设计文档 D3）。

use crate::{PairedDevice, StoreError};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

pub(crate) fn list(connection: &Connection) -> Result<Vec<PairedDevice>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT endpoint_id, name, paired_at_ms, last_seen_at_ms FROM paired_device
         ORDER BY paired_at_ms, endpoint_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(PairedDevice {
            endpoint_id: row.get(0)?,
            name: row.get(1)?,
            paired_at_ms: row.get(2)?,
            last_seen_at_ms: row.get(3)?,
        })
    })?;
    rows.collect::<Result<_, _>>().map_err(StoreError::from)
}

pub(crate) fn get(
    connection: &Connection,
    endpoint_id: &str,
) -> Result<Option<PairedDevice>, StoreError> {
    connection
        .query_row(
            "SELECT endpoint_id, name, paired_at_ms, last_seen_at_ms FROM paired_device
             WHERE endpoint_id = ?1",
            params![endpoint_id],
            |row| {
                Ok(PairedDevice {
                    endpoint_id: row.get(0)?,
                    name: row.get(1)?,
                    paired_at_ms: row.get(2)?,
                    last_seen_at_ms: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::from)
}

/// 同一台设备重新配对：换名字和配对时间，最近连接时间清空。
pub(crate) fn put(transaction: &Transaction<'_>, device: &PairedDevice) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO paired_device (endpoint_id, name, paired_at_ms, last_seen_at_ms)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(endpoint_id)
         DO UPDATE SET name = ?2, paired_at_ms = ?3, last_seen_at_ms = ?4",
        params![
            device.endpoint_id,
            device.name,
            device.paired_at_ms,
            device.last_seen_at_ms
        ],
    )?;
    Ok(())
}

pub(crate) fn delete(transaction: &Transaction<'_>, endpoint_id: &str) -> Result<bool, StoreError> {
    let changed = transaction.execute(
        "DELETE FROM paired_device WHERE endpoint_id = ?1",
        params![endpoint_id],
    )?;
    Ok(changed > 0)
}

pub(crate) fn touch(
    transaction: &Transaction<'_>,
    endpoint_id: &str,
    now_ms: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        "UPDATE paired_device SET last_seen_at_ms = ?2 WHERE endpoint_id = ?1",
        params![endpoint_id, now_ms],
    )?;
    Ok(())
}
