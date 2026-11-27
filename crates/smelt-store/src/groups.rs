//! 智能体群组持久化（`agent_group` / `agent_group_member` / `agent_group_message` /
//! `agent_group_message_mention`，见 `docs/agent-groups.md`）。

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension as _, Row, Transaction, params, params_from_iter};

use crate::{
    AgentGroupListStats, AgentGroupMemberRecord, AgentGroupMentionRecord, AgentGroupMessageQuery,
    AgentGroupMessageRecord, AgentGroupPendingMentionRecord, AgentGroupRecord,
    NewAgentGroupMention, NewAgentGroupMessage, StoreError,
};

const GROUP_COLUMNS: &str = "id, title, description, project_root, workset_id, mode, \
    coordinator_member_id, allow_worker_cross_talk, max_chain_hops, max_chain_depth, \
    max_pair_ping_pong, auto_bridge_mentions, paused, next_seq, archived_at_ms, \
    created_at_ms, updated_at_ms";

const MEMBER_COLUMNS: &str = "id, group_id, alias, display_name, ownership, session_id, \
    role_kind, role_title, role_prompt, tool_restriction_profile, briefing_sent, \
    last_delivered_seq, last_read_seq, muted, removed_at_ms, created_at_ms";

const MESSAGE_COLUMNS: &str = "id, group_id, seq, sender_type, sender_member_id, kind, content, \
    reply_to_seq, thread_root_seq, task_status, chain_id, chain_depth, auto_bridged, pinned, \
    idempotency_key, created_at_ms";

const MENTION_COLUMNS: &str = "message_id, target_member_id, mention_kind, delivery_status, \
    delivery_id, status_reason, updated_at_ms";

fn group_from_row(row: &Row<'_>) -> rusqlite::Result<AgentGroupRecord> {
    Ok(AgentGroupRecord {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        project_root: row.get(3)?,
        workset_id: row.get(4)?,
        mode: row.get(5)?,
        coordinator_member_id: row.get(6)?,
        allow_worker_cross_talk: row.get::<_, i64>(7)? != 0,
        max_chain_hops: row.get::<_, i64>(8)?.max(0) as u32,
        max_chain_depth: row.get::<_, i64>(9)?.max(0) as u32,
        max_pair_ping_pong: row.get::<_, i64>(10)?.max(0) as u32,
        auto_bridge_mentions: row.get::<_, i64>(11)? != 0,
        paused: row.get::<_, i64>(12)? != 0,
        next_seq: row.get(13)?,
        archived_at_ms: row.get(14)?,
        created_at_ms: row.get(15)?,
        updated_at_ms: row.get(16)?,
    })
}

fn member_from_row(row: &Row<'_>) -> rusqlite::Result<AgentGroupMemberRecord> {
    Ok(AgentGroupMemberRecord {
        id: row.get(0)?,
        group_id: row.get(1)?,
        alias: row.get(2)?,
        display_name: row.get(3)?,
        ownership: row.get(4)?,
        session_id: row.get(5)?,
        role_kind: row.get(6)?,
        role_title: row.get(7)?,
        role_prompt: row.get(8)?,
        tool_restriction_profile: row.get(9)?,
        briefing_sent: row.get::<_, i64>(10)? != 0,
        last_delivered_seq: row.get(11)?,
        last_read_seq: row.get(12)?,
        muted: row.get::<_, i64>(13)? != 0,
        removed_at_ms: row.get(14)?,
        created_at_ms: row.get(15)?,
    })
}

fn message_from_row(row: &Row<'_>) -> rusqlite::Result<AgentGroupMessageRecord> {
    Ok(AgentGroupMessageRecord {
        id: row.get(0)?,
        group_id: row.get(1)?,
        seq: row.get(2)?,
        sender_type: row.get(3)?,
        sender_member_id: row.get(4)?,
        kind: row.get(5)?,
        content: row.get(6)?,
        reply_to_seq: row.get(7)?,
        thread_root_seq: row.get(8)?,
        task_status: row.get(9)?,
        chain_id: row.get(10)?,
        chain_depth: row.get::<_, i64>(11)?.max(0) as u32,
        auto_bridged: row.get::<_, i64>(12)? != 0,
        pinned: row.get::<_, i64>(13)? != 0,
        idempotency_key: row.get(14)?,
        created_at_ms: row.get(15)?,
    })
}

fn mention_from_row(row: &Row<'_>) -> rusqlite::Result<AgentGroupMentionRecord> {
    Ok(AgentGroupMentionRecord {
        message_id: row.get(0)?,
        target_member_id: row.get(1)?,
        mention_kind: row.get(2)?,
        delivery_status: row.get(3)?,
        delivery_id: row.get(4)?,
        status_reason: row.get(5)?,
        updated_at_ms: row.get(6)?,
    })
}

fn pending_mention_from_row(row: &Row<'_>) -> rusqlite::Result<AgentGroupPendingMentionRecord> {
    Ok(AgentGroupPendingMentionRecord {
        group_id: row.get(0)?,
        seq: row.get(1)?,
        mention: AgentGroupMentionRecord {
            message_id: row.get(2)?,
            target_member_id: row.get(3)?,
            mention_kind: row.get(4)?,
            delivery_status: row.get(5)?,
            delivery_id: row.get(6)?,
            status_reason: row.get(7)?,
            updated_at_ms: row.get(8)?,
        },
    })
}

pub(crate) fn list_groups(
    connection: &Connection,
    include_archived: bool,
) -> Result<Vec<AgentGroupRecord>, StoreError> {
    let sql = if include_archived {
        format!("SELECT {GROUP_COLUMNS} FROM agent_group ORDER BY updated_at_ms DESC, id ASC")
    } else {
        format!(
            "SELECT {GROUP_COLUMNS} FROM agent_group \
             WHERE archived_at_ms IS NULL \
             ORDER BY updated_at_ms DESC, id ASC"
        )
    };
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], group_from_row)?;
    rows.collect::<Result<_, _>>().map_err(StoreError::from)
}

pub(crate) fn list_group_stats(
    connection: &Connection,
    group_ids: &[String],
) -> Result<HashMap<String, AgentGroupListStats>, StoreError> {
    let mut result: HashMap<String, AgentGroupListStats> = group_ids
        .iter()
        .map(|id| (id.clone(), AgentGroupListStats::default()))
        .collect();
    if group_ids.is_empty() {
        return Ok(result);
    }

    let placeholders = placeholders_for(group_ids.len());

    {
        let sql = format!(
            "SELECT group_id, COUNT(*), \
                    SUM(CASE WHEN removed_at_ms IS NULL THEN 1 ELSE 0 END) \
             FROM agent_group_member \
             WHERE group_id IN ({placeholders}) \
             GROUP BY group_id"
        );
        let mut stmt = connection.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(group_ids.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.max(0) as usize,
                row.get::<_, Option<i64>>(2)?.unwrap_or(0).max(0) as usize,
            ))
        })?;
        for row in rows {
            let (gid, total, active) = row?;
            if let Some(stats) = result.get_mut(&gid) {
                stats.member_count = total;
                stats.active_member_count = active;
            }
        }
    }

    {
        let sql = format!(
            "SELECT m.group_id, COUNT(*) \
             FROM agent_group_message_mention mn \
             JOIN agent_group_message m ON m.id = mn.message_id \
             WHERE m.group_id IN ({placeholders}) AND mn.delivery_status = 'pending_approval' \
             GROUP BY m.group_id"
        );
        let mut stmt = connection.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(group_ids.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.max(0) as usize,
            ))
        })?;
        for row in rows {
            let (gid, pending) = row?;
            if let Some(stats) = result.get_mut(&gid) {
                stats.pending_approval_count = pending;
            }
        }
    }

    {
        let sql = format!(
            "SELECT group_id, COUNT(*) \
             FROM agent_group_message \
             WHERE group_id IN ({placeholders}) \
               AND kind = 'task' \
               AND task_status IN ('open', 'in_progress', 'blocked') \
             GROUP BY group_id"
        );
        let mut stmt = connection.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(group_ids.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.max(0) as usize,
            ))
        })?;
        for row in rows {
            let (gid, open) = row?;
            if let Some(stats) = result.get_mut(&gid) {
                stats.open_task_count = open;
            }
        }
    }

    {
        let sql = format!(
            "SELECT m.group_id, m.seq, m.content, m.created_at_ms \
             FROM agent_group_message m \
             JOIN ( \
               SELECT group_id, MAX(seq) AS max_seq \
               FROM agent_group_message \
               WHERE group_id IN ({placeholders}) \
               GROUP BY group_id \
             ) latest ON latest.group_id = m.group_id AND latest.max_seq = m.seq"
        );
        let mut stmt = connection.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(group_ids.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (gid, seq, content, created_at_ms) = row?;
            if let Some(stats) = result.get_mut(&gid) {
                stats.last_seq = seq;
                stats.last_message_preview = Some(preview_snippet(&content, 160));
                stats.last_message_at_ms = Some(created_at_ms);
            }
        }
    }

    Ok(result)
}

pub(crate) fn get_group(
    connection: &Connection,
    id: &str,
) -> Result<Option<AgentGroupRecord>, StoreError> {
    connection
        .query_row(
            &format!("SELECT {GROUP_COLUMNS} FROM agent_group WHERE id = ?1"),
            params![id],
            group_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

pub(crate) fn insert_group(
    transaction: &Transaction<'_>,
    group: &AgentGroupRecord,
) -> Result<(), StoreError> {
    transaction.execute(
        &format!(
            "INSERT INTO agent_group ({GROUP_COLUMNS}) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)"
        ),
        params![
            group.id,
            group.title,
            group.description,
            group.project_root,
            group.workset_id,
            group.mode,
            group.coordinator_member_id,
            i64::from(group.allow_worker_cross_talk),
            i64::from(group.max_chain_hops),
            i64::from(group.max_chain_depth),
            i64::from(group.max_pair_ping_pong),
            i64::from(group.auto_bridge_mentions),
            i64::from(group.paused),
            group.next_seq,
            group.archived_at_ms,
            group.created_at_ms,
            group.updated_at_ms,
        ],
    )?;
    Ok(())
}

pub(crate) fn update_group(
    transaction: &Transaction<'_>,
    group: &AgentGroupRecord,
) -> Result<(), StoreError> {
    let changed = transaction.execute(
        "UPDATE agent_group SET \
           title = ?2, \
           description = ?3, \
           project_root = ?4, \
           workset_id = ?5, \
           mode = ?6, \
           coordinator_member_id = ?7, \
           allow_worker_cross_talk = ?8, \
           max_chain_hops = ?9, \
           max_chain_depth = ?10, \
           max_pair_ping_pong = ?11, \
           auto_bridge_mentions = ?12, \
           paused = ?13, \
           archived_at_ms = ?14, \
           updated_at_ms = ?15 \
         WHERE id = ?1",
        params![
            group.id,
            group.title,
            group.description,
            group.project_root,
            group.workset_id,
            group.mode,
            group.coordinator_member_id,
            i64::from(group.allow_worker_cross_talk),
            i64::from(group.max_chain_hops),
            i64::from(group.max_chain_depth),
            i64::from(group.max_pair_ping_pong),
            i64::from(group.auto_bridge_mentions),
            i64::from(group.paused),
            group.archived_at_ms,
            group.updated_at_ms,
        ],
    )?;
    if changed == 0 {
        return Err(StoreError::AgentGroupNotFound {
            id: group.id.clone(),
        });
    }
    Ok(())
}

pub(crate) fn delete_group(transaction: &Transaction<'_>, id: &str) -> Result<bool, StoreError> {
    let changed = transaction.execute("DELETE FROM agent_group WHERE id = ?1", params![id])?;
    Ok(changed > 0)
}

pub(crate) fn list_members(
    connection: &Connection,
    group_id: &str,
    include_removed: bool,
) -> Result<Vec<AgentGroupMemberRecord>, StoreError> {
    let sql = if include_removed {
        format!(
            "SELECT {MEMBER_COLUMNS} FROM agent_group_member \
             WHERE group_id = ?1 \
             ORDER BY created_at_ms ASC, id ASC"
        )
    } else {
        format!(
            "SELECT {MEMBER_COLUMNS} FROM agent_group_member \
             WHERE group_id = ?1 AND removed_at_ms IS NULL \
             ORDER BY created_at_ms ASC, id ASC"
        )
    };
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params![group_id], member_from_row)?;
    rows.collect::<Result<_, _>>().map_err(StoreError::from)
}

pub(crate) fn get_member(
    connection: &Connection,
    member_id: &str,
) -> Result<Option<AgentGroupMemberRecord>, StoreError> {
    connection
        .query_row(
            &format!("SELECT {MEMBER_COLUMNS} FROM agent_group_member WHERE id = ?1"),
            params![member_id],
            member_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

pub(crate) fn insert_member(
    transaction: &Transaction<'_>,
    member: &AgentGroupMemberRecord,
    group_updated_at_ms: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        &format!(
            "INSERT INTO agent_group_member ({MEMBER_COLUMNS}) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)"
        ),
        params![
            member.id,
            member.group_id,
            member.alias,
            member.display_name,
            member.ownership,
            member.session_id,
            member.role_kind,
            member.role_title,
            member.role_prompt,
            member.tool_restriction_profile,
            i64::from(member.briefing_sent),
            member.last_delivered_seq,
            member.last_read_seq,
            i64::from(member.muted),
            member.removed_at_ms,
            member.created_at_ms,
        ],
    )?;
    transaction.execute(
        "UPDATE agent_group SET updated_at_ms = ?2 WHERE id = ?1",
        params![member.group_id, group_updated_at_ms],
    )?;
    Ok(())
}

pub(crate) fn update_member(
    transaction: &Transaction<'_>,
    member: &AgentGroupMemberRecord,
    group_updated_at_ms: i64,
) -> Result<(), StoreError> {
    let changed = transaction.execute(
        "UPDATE agent_group_member SET \
           alias = ?2, \
           display_name = ?3, \
           role_kind = ?4, \
           role_title = ?5, \
           role_prompt = ?6, \
           tool_restriction_profile = ?7, \
           briefing_sent = ?8, \
           last_delivered_seq = ?9, \
           last_read_seq = ?10, \
           muted = ?11, \
           removed_at_ms = ?12 \
         WHERE id = ?1",
        params![
            member.id,
            member.alias,
            member.display_name,
            member.role_kind,
            member.role_title,
            member.role_prompt,
            member.tool_restriction_profile,
            i64::from(member.briefing_sent),
            member.last_delivered_seq,
            member.last_read_seq,
            i64::from(member.muted),
            member.removed_at_ms,
        ],
    )?;
    if changed == 0 {
        return Err(StoreError::AgentGroupMemberNotFound {
            id: member.id.clone(),
        });
    }
    transaction.execute(
        "UPDATE agent_group SET updated_at_ms = ?2 WHERE id = ?1",
        params![member.group_id, group_updated_at_ms],
    )?;
    Ok(())
}

pub(crate) fn mark_member_removed(
    transaction: &Transaction<'_>,
    group_id: &str,
    member_id: &str,
    removed_at_ms: i64,
) -> Result<bool, StoreError> {
    let changed = transaction.execute(
        "UPDATE agent_group_member SET removed_at_ms = ?3 \
         WHERE id = ?1 AND group_id = ?2 AND removed_at_ms IS NULL",
        params![member_id, group_id, removed_at_ms],
    )?;
    if changed > 0 {
        transaction.execute(
            "UPDATE agent_group \
             SET coordinator_member_id = CASE \
                   WHEN coordinator_member_id = ?2 THEN NULL \
                   ELSE coordinator_member_id \
                 END, \
                 updated_at_ms = ?3 \
             WHERE id = ?1",
            params![group_id, member_id, removed_at_ms],
        )?;
    }
    Ok(changed > 0)
}

pub(crate) fn find_active_members_by_session(
    connection: &Connection,
    session_id: &str,
) -> Result<Vec<AgentGroupMemberRecord>, StoreError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {MEMBER_COLUMNS} FROM agent_group_member \
         WHERE session_id = ?1 AND removed_at_ms IS NULL \
         ORDER BY created_at_ms ASC, id ASC"
    ))?;
    let rows = statement.query_map(params![session_id], member_from_row)?;
    rows.collect::<Result<_, _>>().map_err(StoreError::from)
}

pub(crate) fn list_group_owned_session_ids(
    connection: &Connection,
) -> Result<Vec<String>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT DISTINCT session_id FROM agent_group_member \
         WHERE ownership = 'group_owned' AND removed_at_ms IS NULL \
         ORDER BY session_id ASC",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<Result<_, _>>().map_err(StoreError::from)
}

pub(crate) fn append_message_with_mentions(
    transaction: &Transaction<'_>,
    message: &NewAgentGroupMessage,
    mentions: &[NewAgentGroupMention],
) -> Result<(AgentGroupMessageRecord, Vec<AgentGroupMentionRecord>, bool), StoreError> {
    if let Some(key) = message.idempotency_key.as_deref()
        && let Some(existing) = transaction
            .query_row(
                &format!(
                    "SELECT {MESSAGE_COLUMNS} FROM agent_group_message \
                     WHERE group_id = ?1 AND idempotency_key = ?2"
                ),
                params![message.group_id, key],
                message_from_row,
            )
            .optional()?
    {
        let existing_mentions = list_mentions_for_message(transaction, &existing.id)?;
        return Ok((existing, existing_mentions, false));
    }

    let next_seq: Option<i64> = transaction
        .query_row(
            "SELECT next_seq FROM agent_group WHERE id = ?1",
            params![message.group_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(seq) = next_seq else {
        return Err(StoreError::AgentGroupNotFound {
            id: message.group_id.clone(),
        });
    };

    transaction.execute(
        "UPDATE agent_group SET next_seq = ?2, updated_at_ms = ?3 WHERE id = ?1",
        params![message.group_id, seq + 1, message.created_at_ms],
    )?;

    transaction.execute(
        &format!(
            "INSERT INTO agent_group_message ({MESSAGE_COLUMNS}) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)"
        ),
        params![
            message.id,
            message.group_id,
            seq,
            message.sender_type,
            message.sender_member_id,
            message.kind,
            message.content,
            message.reply_to_seq,
            message.thread_root_seq,
            message.task_status,
            message.chain_id,
            i64::from(message.chain_depth),
            i64::from(message.auto_bridged),
            i64::from(message.pinned),
            message.idempotency_key,
            message.created_at_ms,
        ],
    )?;

    let mut mention_records = Vec::with_capacity(mentions.len());
    for mention in mentions {
        transaction.execute(
            &format!(
                "INSERT INTO agent_group_message_mention ({MENTION_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
            ),
            params![
                message.id,
                mention.target_member_id,
                mention.mention_kind,
                mention.delivery_status,
                mention.delivery_id,
                mention.status_reason,
                mention.updated_at_ms,
            ],
        )?;
        mention_records.push(AgentGroupMentionRecord {
            message_id: message.id.clone(),
            target_member_id: mention.target_member_id.clone(),
            mention_kind: mention.mention_kind.clone(),
            delivery_status: mention.delivery_status.clone(),
            delivery_id: mention.delivery_id.clone(),
            status_reason: mention.status_reason.clone(),
            updated_at_ms: mention.updated_at_ms,
        });
    }

    let record = AgentGroupMessageRecord {
        id: message.id.clone(),
        group_id: message.group_id.clone(),
        seq,
        sender_type: message.sender_type.clone(),
        sender_member_id: message.sender_member_id.clone(),
        kind: message.kind.clone(),
        content: message.content.clone(),
        reply_to_seq: message.reply_to_seq,
        thread_root_seq: message.thread_root_seq,
        task_status: message.task_status.clone(),
        chain_id: message.chain_id.clone(),
        chain_depth: message.chain_depth,
        auto_bridged: message.auto_bridged,
        pinned: message.pinned,
        idempotency_key: message.idempotency_key.clone(),
        created_at_ms: message.created_at_ms,
    };

    Ok((record, mention_records, true))
}

pub(crate) fn get_message_by_id(
    connection: &Connection,
    message_id: &str,
) -> Result<Option<AgentGroupMessageRecord>, StoreError> {
    connection
        .query_row(
            &format!("SELECT {MESSAGE_COLUMNS} FROM agent_group_message WHERE id = ?1"),
            params![message_id],
            message_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

pub(crate) fn get_message_by_seq(
    connection: &Connection,
    group_id: &str,
    seq: i64,
) -> Result<Option<AgentGroupMessageRecord>, StoreError> {
    connection
        .query_row(
            &format!(
                "SELECT {MESSAGE_COLUMNS} FROM agent_group_message \
                 WHERE group_id = ?1 AND seq = ?2"
            ),
            params![group_id, seq],
            message_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

pub(crate) fn list_messages(
    connection: &Connection,
    query: &AgentGroupMessageQuery,
) -> Result<Vec<AgentGroupMessageRecord>, StoreError> {
    let limit = query.limit.clamp(1, 500) as i64;
    let mut rows = Vec::new();
    match (query.since_seq, query.before_seq, query.pinned_only) {
        (Some(since), _, false) => {
            let mut stmt = connection.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM agent_group_message \
                 WHERE group_id = ?1 AND seq > ?2 \
                 ORDER BY seq ASC LIMIT ?3"
            ))?;
            for row in stmt.query_map(params![query.group_id, since, limit], message_from_row)? {
                rows.push(row?);
            }
        }
        (Some(since), _, true) => {
            let mut stmt = connection.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM agent_group_message \
                 WHERE group_id = ?1 AND seq > ?2 AND pinned = 1 \
                 ORDER BY seq ASC LIMIT ?3"
            ))?;
            for row in stmt.query_map(params![query.group_id, since, limit], message_from_row)? {
                rows.push(row?);
            }
        }
        (None, Some(before), false) => {
            let mut stmt = connection.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM agent_group_message \
                 WHERE group_id = ?1 AND seq < ?2 \
                 ORDER BY seq DESC LIMIT ?3"
            ))?;
            for row in stmt.query_map(params![query.group_id, before, limit], message_from_row)? {
                rows.push(row?);
            }
            rows.reverse();
        }
        (None, Some(before), true) => {
            let mut stmt = connection.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM agent_group_message \
                 WHERE group_id = ?1 AND seq < ?2 AND pinned = 1 \
                 ORDER BY seq DESC LIMIT ?3"
            ))?;
            for row in stmt.query_map(params![query.group_id, before, limit], message_from_row)? {
                rows.push(row?);
            }
            rows.reverse();
        }
        (None, None, false) => {
            let mut stmt = connection.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM agent_group_message \
                 WHERE group_id = ?1 \
                 ORDER BY seq DESC LIMIT ?2"
            ))?;
            for row in stmt.query_map(params![query.group_id, limit], message_from_row)? {
                rows.push(row?);
            }
            rows.reverse();
        }
        (None, None, true) => {
            let mut stmt = connection.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM agent_group_message \
                 WHERE group_id = ?1 AND pinned = 1 \
                 ORDER BY seq DESC LIMIT ?2"
            ))?;
            for row in stmt.query_map(params![query.group_id, limit], message_from_row)? {
                rows.push(row?);
            }
            rows.reverse();
        }
    }
    Ok(rows)
}

pub(crate) fn list_mentions_for_message(
    connection: &Connection,
    message_id: &str,
) -> Result<Vec<AgentGroupMentionRecord>, StoreError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {MENTION_COLUMNS} FROM agent_group_message_mention \
         WHERE message_id = ?1 \
         ORDER BY target_member_id ASC"
    ))?;
    let rows = statement.query_map(params![message_id], mention_from_row)?;
    rows.collect::<Result<_, _>>().map_err(StoreError::from)
}

pub(crate) fn list_mentions_for_messages(
    connection: &Connection,
    message_ids: &[String],
) -> Result<HashMap<String, Vec<AgentGroupMentionRecord>>, StoreError> {
    let mut out: HashMap<String, Vec<AgentGroupMentionRecord>> = HashMap::new();
    if message_ids.is_empty() {
        return Ok(out);
    }
    // 分批避免超大列表触碰 SQLite 变量上限。
    for chunk in message_ids.chunks(500) {
        let placeholders = placeholders_for(chunk.len());
        let sql = format!(
            "SELECT {MENTION_COLUMNS} FROM agent_group_message_mention \
             WHERE message_id IN ({placeholders}) \
             ORDER BY message_id ASC, target_member_id ASC"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(chunk.iter()), mention_from_row)?;
        for row in rows {
            let mention = row?;
            out.entry(mention.message_id.clone())
                .or_default()
                .push(mention);
        }
    }
    Ok(out)
}

pub(crate) fn get_mention(
    connection: &Connection,
    message_id: &str,
    target_member_id: &str,
) -> Result<Option<AgentGroupPendingMentionRecord>, StoreError> {
    connection
        .query_row(
            "SELECT m.group_id, m.seq, mn.message_id, mn.target_member_id, mn.mention_kind, \
                    mn.delivery_status, mn.delivery_id, mn.status_reason, mn.updated_at_ms \
             FROM agent_group_message_mention mn \
             JOIN agent_group_message m ON m.id = mn.message_id \
             WHERE mn.message_id = ?1 AND mn.target_member_id = ?2",
            params![message_id, target_member_id],
            pending_mention_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

pub(crate) fn update_mention_status(
    transaction: &Transaction<'_>,
    message_id: &str,
    target_member_id: &str,
    delivery_status: &str,
    status_reason: Option<&str>,
    updated_at_ms: i64,
) -> Result<bool, StoreError> {
    let changed = transaction.execute(
        "UPDATE agent_group_message_mention \
         SET delivery_status = ?3, status_reason = ?4, updated_at_ms = ?5 \
         WHERE message_id = ?1 AND target_member_id = ?2",
        params![
            message_id,
            target_member_id,
            delivery_status,
            status_reason,
            updated_at_ms,
        ],
    )?;
    Ok(changed > 0)
}

/// 当 `peer_messages` 结算一封群组投递信件时，原子更新投递状态、推进成员投递游标与首条简报标记，
/// 并将该成员在更早序号上的 `queued` 记录合并为 `coalesced`。
pub(crate) fn settle_mention_by_delivery_id(
    transaction: &Transaction<'_>,
    delivery_id: &str,
    delivery_status: &str,
    status_reason: Option<&str>,
    updated_at_ms: i64,
) -> Result<Option<AgentGroupPendingMentionRecord>, StoreError> {
    let Some(mut record) = transaction
        .query_row(
            "SELECT m.group_id, m.seq, mn.message_id, mn.target_member_id, mn.mention_kind, \
                    mn.delivery_status, mn.delivery_id, mn.status_reason, mn.updated_at_ms \
             FROM agent_group_message_mention mn \
             JOIN agent_group_message m ON m.id = mn.message_id \
             WHERE mn.delivery_id = ?1",
            params![delivery_id],
            pending_mention_from_row,
        )
        .optional()?
    else {
        return Ok(None);
    };

    if record.mention.delivery_status == "delivered" || record.mention.delivery_status == "rejected"
    {
        return Ok(None);
    }

    transaction.execute(
        "UPDATE agent_group_message_mention \
         SET delivery_status = ?2, status_reason = ?3, updated_at_ms = ?4 \
         WHERE message_id = ?1 AND target_member_id = ?5",
        params![
            record.mention.message_id,
            delivery_status,
            status_reason,
            updated_at_ms,
            record.mention.target_member_id,
        ],
    )?;

    if delivery_status == "delivered" {
        transaction.execute(
            "UPDATE agent_group_member \
             SET last_delivered_seq = MAX(last_delivered_seq, ?2), briefing_sent = 1 \
             WHERE id = ?1",
            params![record.mention.target_member_id, record.seq],
        )?;
        transaction.execute(
            "UPDATE agent_group_message_mention \
             SET delivery_status = 'coalesced', updated_at_ms = ?4 \
             WHERE target_member_id = ?1 \
               AND delivery_status = 'queued' \
               AND message_id IN ( \
                 SELECT id FROM agent_group_message \
                 WHERE group_id = ?2 AND seq < ?3 \
               )",
            params![
                record.mention.target_member_id,
                record.group_id,
                record.seq,
                updated_at_ms,
            ],
        )?;
    }

    record.mention.delivery_status = delivery_status.to_string();
    record.mention.status_reason = status_reason.map(str::to_string);
    record.mention.updated_at_ms = updated_at_ms;
    Ok(Some(record))
}

pub(crate) fn list_mentions_by_status(
    connection: &Connection,
    group_id: Option<&str>,
    status: &str,
) -> Result<Vec<AgentGroupPendingMentionRecord>, StoreError> {
    let mut rows = Vec::new();
    if let Some(gid) = group_id {
        let mut statement = connection.prepare(
            "SELECT m.group_id, m.seq, mn.message_id, mn.target_member_id, mn.mention_kind, \
                    mn.delivery_status, mn.delivery_id, mn.status_reason, mn.updated_at_ms \
             FROM agent_group_message_mention mn \
             JOIN agent_group_message m ON m.id = mn.message_id \
             WHERE m.group_id = ?1 AND mn.delivery_status = ?2 \
             ORDER BY m.seq ASC, mn.target_member_id ASC",
        )?;
        for row in statement.query_map(params![gid, status], pending_mention_from_row)? {
            rows.push(row?);
        }
    } else {
        let mut statement = connection.prepare(
            "SELECT m.group_id, m.seq, mn.message_id, mn.target_member_id, mn.mention_kind, \
                    mn.delivery_status, mn.delivery_id, mn.status_reason, mn.updated_at_ms \
             FROM agent_group_message_mention mn \
             JOIN agent_group_message m ON m.id = mn.message_id \
             JOIN agent_group g ON g.id = m.group_id \
             WHERE mn.delivery_status = ?1 \
               AND g.archived_at_ms IS NULL \
               AND g.paused = 0 \
             ORDER BY m.created_at_ms ASC, m.seq ASC",
        )?;
        for row in statement.query_map(params![status], pending_mention_from_row)? {
            rows.push(row?);
        }
    }
    Ok(rows)
}

pub(crate) fn set_message_pinned(
    transaction: &Transaction<'_>,
    group_id: &str,
    seq: i64,
    pinned: bool,
    updated_at_ms: i64,
) -> Result<bool, StoreError> {
    let changed = transaction.execute(
        "UPDATE agent_group_message SET pinned = ?3 WHERE group_id = ?1 AND seq = ?2",
        params![group_id, seq, i64::from(pinned)],
    )?;
    if changed > 0 {
        transaction.execute(
            "UPDATE agent_group SET updated_at_ms = ?2 WHERE id = ?1",
            params![group_id, updated_at_ms],
        )?;
    }
    Ok(changed > 0)
}

pub(crate) fn update_task_status(
    transaction: &Transaction<'_>,
    group_id: &str,
    seq: i64,
    task_status: &str,
    updated_at_ms: i64,
) -> Result<bool, StoreError> {
    let changed = transaction.execute(
        "UPDATE agent_group_message SET task_status = ?3 \
         WHERE group_id = ?1 AND seq = ?2 AND kind = 'task'",
        params![group_id, seq, task_status],
    )?;
    if changed > 0 {
        transaction.execute(
            "UPDATE agent_group SET updated_at_ms = ?2 WHERE id = ?1",
            params![group_id, updated_at_ms],
        )?;
    }
    Ok(changed > 0)
}

pub(crate) fn list_open_tasks(
    connection: &Connection,
    group_id: &str,
) -> Result<Vec<AgentGroupMessageRecord>, StoreError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM agent_group_message \
         WHERE group_id = ?1 \
           AND kind = 'task' \
           AND task_status IN ('open', 'in_progress', 'blocked') \
         ORDER BY seq ASC"
    ))?;
    let rows = statement.query_map(params![group_id], message_from_row)?;
    rows.collect::<Result<_, _>>().map_err(StoreError::from)
}

pub(crate) fn advance_member_read_seq(
    transaction: &Transaction<'_>,
    member_id: &str,
    seq: i64,
) -> Result<bool, StoreError> {
    let changed = transaction.execute(
        "UPDATE agent_group_member SET last_read_seq = MAX(last_read_seq, ?2) WHERE id = ?1",
        params![member_id, seq],
    )?;
    Ok(changed > 0)
}

fn placeholders_for(count: usize) -> String {
    (1..=count)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn preview_snippet(content: &str, max_chars: usize) -> String {
    let collapsed = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let snippet: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{snippet}…")
    } else {
        snippet
    }
}
