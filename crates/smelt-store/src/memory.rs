//! 智能体记忆的元数据：使用统计和来源。
//!
//! 记忆正文是 Markdown 文件，不进库；这里只存结构化状态。`scope` 为 `agent:<智能体 id>`
//! 或 `user`（跨智能体的全局层），`file_name` 是记忆目录下的文件名。
//!
//! 信任闸门：只有 `trust = 'trusted'` 的来源记录才允许注入；没有记录或 `pending` 都不注入。
//! 升级迁移用哨兵行标记「存量已回填为 trusted」，见 [`LEGACY_TRUST_BACKFILL_SCOPE`]。

use crate::{
    MemoryMetaSnapshot, MemoryTrust, MemoryTrustedRecord, MemoryUsageRecord, StoreError,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

/// 升级迁移哨兵：这一行存在表示「无来源记录的存量文件」已经按 trusted 回填过，
/// 之后新出现的无记录文件一律当待确认。
pub const LEGACY_TRUST_BACKFILL_SCOPE: &str = "system:trust-gate";
pub const LEGACY_TRUST_BACKFILL_FILE: &str = "legacy-backfill-v1";

pub(crate) fn record_read(
    transaction: &Transaction<'_>,
    scope: &str,
    file_name: &str,
    now_ms: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO agent_memory_usage (scope, file_name, read_count, last_read_at_ms)
         VALUES (?1, ?2, 1, ?3)
         ON CONFLICT(scope, file_name)
         DO UPDATE SET read_count = read_count + 1, last_read_at_ms = ?3",
        params![scope, file_name, now_ms],
    )?;
    Ok(())
}

pub(crate) fn record_write(
    transaction: &Transaction<'_>,
    scope: &str,
    file_name: &str,
    trust: MemoryTrust,
    reason: &str,
    session_id: &str,
    now_ms: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO agent_memory_provenance (scope, file_name, trust, reason, session_id, written_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(scope, file_name)
         DO UPDATE SET trust = ?3, reason = ?4, session_id = ?5, written_at_ms = ?6",
        params![scope, file_name, trust.as_str(), reason, session_id, now_ms],
    )?;
    Ok(())
}

pub(crate) fn approve(
    transaction: &Transaction<'_>,
    scope: &str,
    file_name: &str,
    now_ms: i64,
) -> Result<bool, StoreError> {
    let changed = transaction.execute(
        "UPDATE agent_memory_provenance
         SET trust = 'trusted', reason = '用户采纳', written_at_ms = ?3
         WHERE scope = ?1 AND file_name = ?2 AND trust = 'pending'",
        params![scope, file_name, now_ms],
    )?;
    Ok(changed > 0)
}

pub(crate) fn forget(
    transaction: &Transaction<'_>,
    scope: &str,
    file_name: &str,
) -> Result<(), StoreError> {
    for table in ["agent_memory_usage", "agent_memory_provenance"] {
        transaction.execute(
            &format!("DELETE FROM {table} WHERE scope = ?1 AND file_name = ?2"),
            params![scope, file_name],
        )?;
    }
    Ok(())
}

pub(crate) fn forget_scope(transaction: &Transaction<'_>, scope: &str) -> Result<(), StoreError> {
    for table in ["agent_memory_usage", "agent_memory_provenance"] {
        transaction.execute(
            &format!("DELETE FROM {table} WHERE scope = ?1"),
            params![scope],
        )?;
    }
    Ok(())
}

pub(crate) fn legacy_trust_backfill_done(connection: &Connection) -> Result<bool, StoreError> {
    let found = connection
        .query_row(
            "SELECT 1 FROM agent_memory_provenance
             WHERE scope = ?1 AND file_name = ?2",
            params![LEGACY_TRUST_BACKFILL_SCOPE, LEGACY_TRUST_BACKFILL_FILE],
            |_| Ok(()),
        )
        .optional()?;
    Ok(found.is_some())
}

/// 标记存量回填已完成。幂等。
pub(crate) fn mark_legacy_trust_backfill_done(
    transaction: &Transaction<'_>,
    now_ms: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO agent_memory_provenance (scope, file_name, trust, reason, session_id, written_at_ms)
         VALUES (?1, ?2, 'trusted', '升级迁移哨兵', '', ?3)
         ON CONFLICT(scope, file_name) DO NOTHING",
        params![LEGACY_TRUST_BACKFILL_SCOPE, LEGACY_TRUST_BACKFILL_FILE, now_ms],
    )?;
    Ok(())
}

pub(crate) fn read_meta(
    connection: &Connection,
    scope: &str,
) -> Result<MemoryMetaSnapshot, StoreError> {
    let mut usage_statement = connection.prepare(
        "SELECT file_name, read_count, last_read_at_ms FROM agent_memory_usage
         WHERE scope = ?1 ORDER BY file_name",
    )?;
    let usage = usage_statement
        .query_map(params![scope], |row| {
            Ok(MemoryUsageRecord {
                file_name: row.get(0)?,
                read_count: row.get::<_, i64>(1)?.max(0) as u64,
                last_read_at_ms: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut pending_statement = connection.prepare(
        "SELECT file_name, reason FROM agent_memory_provenance
         WHERE scope = ?1 AND trust = 'pending' ORDER BY file_name",
    )?;
    let pending = pending_statement
        .query_map(params![scope], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut trusted_statement = connection.prepare(
        "SELECT file_name, written_at_ms FROM agent_memory_provenance
         WHERE scope = ?1 AND trust = 'trusted' ORDER BY file_name",
    )?;
    let trusted = trusted_statement
        .query_map(params![scope], |row| {
            Ok(MemoryTrustedRecord {
                file_name: row.get(0)?,
                written_at_ms: row.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MemoryMetaSnapshot {
        usage,
        pending,
        trusted,
    })
}
