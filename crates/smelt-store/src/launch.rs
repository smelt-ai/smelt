//! 项目行「+」菜单的快捷启动项。清单格式版本单独一行，读的时候据此做出厂命令迁移。

use crate::{LaunchEntryRecord, LaunchSnapshot, StoreError};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

pub(crate) fn read_launch_snapshot(
    connection: &Connection,
) -> Result<Option<LaunchSnapshot>, StoreError> {
    let version: Option<u32> = connection
        .query_row(
            "SELECT version FROM launch_settings WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(version) = version else {
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
    Ok(Some(LaunchSnapshot { version, entries }))
}

pub(crate) fn write_launch_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &LaunchSnapshot,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO launch_settings(id, version) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET version = excluded.version",
        [snapshot.version],
    )?;
    transaction.execute("DELETE FROM launch_entry", [])?;
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
