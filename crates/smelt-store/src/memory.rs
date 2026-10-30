//! 智能体记忆的元数据：使用统计和来源。
//!
//! 记忆正文是 Markdown 文件，不进库；这里只存结构化状态。`scope` 为 `agent:<智能体 id>`
//! 或 `user`（跨智能体的全局层），`file_name` 是记忆目录下的文件名。
//!
//! 信任闸门：只有 `trust = 'trusted'` 的来源记录才允许注入；没有记录或 `pending` 都不注入。
//! 升级迁移用哨兵行标记「存量已回填为 trusted」，见 [`LEGACY_TRUST_BACKFILL_SCOPE`]。

use crate::{
    MemoryMetaSnapshot, MemoryProvenanceRecord, MemoryTrust, MemoryTrustedRecord,
    MemoryUsageRecord, StoreError,
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
    // 大小写不敏感的盘上 Style.md 与 style.md 是同一个文件，主键却区分大小写。
    // 写入规范名之前先清掉其它大小写变体，避免留下一条 mtime 对不上的旧行。
    delete_case_variants(transaction, scope, file_name)?;
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
    delete_case_variants(transaction, scope, file_name)?;
    // 无来源行、以及外部改过但 trust 仍是 trusted 的行，都要能被采纳。
    // 不能再要求 WHERE trust = 'pending'，否则这两类点击「采纳」匹配 0 行。
    let changed = transaction.execute(
        "INSERT INTO agent_memory_provenance (scope, file_name, trust, reason, session_id, written_at_ms)
         VALUES (?1, ?2, 'trusted', '用户采纳', '', ?3)
         ON CONFLICT(scope, file_name) DO UPDATE SET
             trust = 'trusted',
             reason = '用户采纳',
             written_at_ms = excluded.written_at_ms",
        params![scope, file_name, now_ms],
    )?;
    Ok(changed > 0)
}

fn delete_case_variants(
    transaction: &Transaction<'_>,
    scope: &str,
    file_name: &str,
) -> Result<(), StoreError> {
    for table in ["agent_memory_usage", "agent_memory_provenance"] {
        transaction.execute(
            &format!(
                "DELETE FROM {table}
                 WHERE scope = ?1 AND lower(file_name) = lower(?2) AND file_name != ?2"
            ),
            params![scope, file_name],
        )?;
    }
    Ok(())
}

pub(crate) fn get_provenance(
    connection: &Connection,
    scope: &str,
    file_name: &str,
) -> Result<Option<MemoryProvenanceRecord>, StoreError> {
    let found = connection
        .query_row(
            "SELECT trust, reason, session_id, written_at_ms
             FROM agent_memory_provenance
             WHERE scope = ?1 AND lower(file_name) = lower(?2)
             ORDER BY file_name
             LIMIT 1",
            params![scope, file_name],
            |row| {
                let trust = match row.get::<_, String>(0)?.as_str() {
                    "trusted" => MemoryTrust::Trusted,
                    _ => MemoryTrust::Pending,
                };
                Ok(MemoryProvenanceRecord {
                    trust,
                    reason: row.get(1)?,
                    session_id: row.get(2)?,
                    written_at_ms: row.get(3)?,
                })
            },
        )
        .optional()?;
    Ok(found)
}

/// 回填记录和哨兵放进同一个事务：中途失败不会先写上哨兵，把还没回填的目录永久漏掉。
pub(crate) fn commit_legacy_backfill(
    transaction: &Transaction<'_>,
    files: &[(String, String, i64)],
    reason: &str,
    session_id: &str,
    now_ms: i64,
) -> Result<(), StoreError> {
    for (scope, file_name, written_at_ms) in files {
        record_write(
            transaction,
            scope,
            file_name,
            MemoryTrust::Trusted,
            reason,
            session_id,
            *written_at_ms,
        )?;
    }
    mark_legacy_trust_backfill_done(transaction, now_ms)?;
    Ok(())
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
        "SELECT file_name, written_at_ms, session_id FROM agent_memory_provenance
         WHERE scope = ?1 AND trust = 'trusted' ORDER BY file_name",
    )?;
    let trusted = trusted_statement
        .query_map(params![scope], |row| {
            Ok(MemoryTrustedRecord {
                file_name: row.get(0)?,
                written_at_ms: row.get(1)?,
                session_id: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MemoryMetaSnapshot {
        usage,
        pending,
        trusted,
    })
}
