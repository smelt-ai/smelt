//! 已配对手机的推送登记：一台设备至多一行，外键挂在 `paired_device` 上，撤销设备时跟着删。

use crate::{PushRegistration, StoreError};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

pub(crate) fn list(connection: &Connection) -> Result<Vec<PushRegistration>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT device_endpoint_id, handle, relay_url, notify_approval, notify_input,
                notify_failure, updated_at_ms
         FROM push_registration ORDER BY device_endpoint_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(PushRegistration {
            device_endpoint_id: row.get(0)?,
            handle: row.get(1)?,
            relay_url: row.get(2)?,
            notify_approval: row.get(3)?,
            notify_input: row.get(4)?,
            notify_failure: row.get(5)?,
            updated_at_ms: row.get(6)?,
        })
    })?;
    rows.collect::<Result<_, _>>().map_err(StoreError::from)
}

/// 设备不在白名单里（登记和撤销赛跑）时不写，返回 false，不靠外键报错。
pub(crate) fn put(
    transaction: &Transaction<'_>,
    registration: &PushRegistration,
) -> Result<bool, StoreError> {
    let paired = transaction
        .query_row(
            "SELECT 1 FROM paired_device WHERE endpoint_id = ?1",
            params![registration.device_endpoint_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !paired {
        return Ok(false);
    }
    transaction.execute(
        "INSERT INTO push_registration (device_endpoint_id, handle, relay_url, notify_approval,
                                        notify_input, notify_failure, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(device_endpoint_id) DO UPDATE SET
           handle = ?2, relay_url = ?3, notify_approval = ?4, notify_input = ?5,
           notify_failure = ?6, updated_at_ms = ?7",
        params![
            registration.device_endpoint_id,
            registration.handle,
            registration.relay_url,
            registration.notify_approval,
            registration.notify_input,
            registration.notify_failure,
            registration.updated_at_ms,
        ],
    )?;
    Ok(true)
}

pub(crate) fn delete(transaction: &Transaction<'_>, endpoint_id: &str) -> Result<bool, StoreError> {
    let changed = transaction.execute(
        "DELETE FROM push_registration WHERE device_endpoint_id = ?1",
        params![endpoint_id],
    )?;
    Ok(changed > 0)
}
