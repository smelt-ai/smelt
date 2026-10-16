//! 智能体记忆的元数据：使用统计和来源。
//!
//! 记忆正文是 Markdown 文件，不进库；这里只存结构化状态。`scope` 为 `agent:<智能体 id>`
//! 或 `user`（跨智能体的全局层），`file_name` 是记忆目录下的文件名。

use crate::{MemoryMetaSnapshot, MemoryTrust, MemoryUsageRecord, StoreError};
use rusqlite::{Connection, Transaction, params};

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
) -> Result<bool, StoreError> {
    let changed = transaction.execute(
        "UPDATE agent_memory_provenance SET trust = 'trusted', reason = '用户采纳'
         WHERE scope = ?1 AND file_name = ?2 AND trust = 'pending'",
        params![scope, file_name],
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
    Ok(MemoryMetaSnapshot { usage, pending })
}
