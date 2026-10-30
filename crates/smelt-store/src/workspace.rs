//! 运行时持有的工作区目录：项目、对话（会话组）、对话里的终端和 ACP 会话细节。终端怎么
//! 分屏归客户端，不在这里（设计文档 D1）。

use crate::{
    StoreError, StoredHistoryIdentity, WorkspaceAcpRecord, WorkspaceSessionRecord,
    WorkspaceSnapshot, WorkspaceTerminalRecord,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::{BTreeMap, HashMap, HashSet};

/// 行已经存在时，恢复身份以库里的值为准。客户端新建对话时可以填上，之后它的修改
/// 不能改写或清空；换 id、放弃旧 id 只走守护进程的显式写入。
fn preserved_history_session_ids(
    transaction: &Transaction<'_>,
) -> Result<HashMap<String, Option<String>>, StoreError> {
    let mut statement =
        transaction.prepare("SELECT session_id, history_session_id FROM acp_detail")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    let mut preserved = HashMap::new();
    for row in rows {
        let (session_id, history_session_id) = row?;
        preserved.insert(session_id, history_session_id);
    }
    Ok(preserved)
}

fn normalize_history_id(id: Option<&str>) -> Option<String> {
    id.map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn history_session_id_for_rewrite(
    session_id: &str,
    client: Option<&str>,
    preserved: &HashMap<String, Option<String>>,
) -> Option<String> {
    match preserved.get(session_id) {
        // 行已经在：确认过的 id 或守护进程放弃后的空，客户端都不能改。
        Some(stored) => normalize_history_id(stored.as_deref()),
        // 第一次落库：和守护进程「没有行就用请求带的 id」是同一条规则。历史页「继续」
        // 新开的槽位常常先由客户端新建进目录、后才 acp_open，这里写空就会被读成「已放弃」，
        // 续接退化成一段空白新会话。
        None => normalize_history_id(client),
    }
}

/// 插入一段对话的会话组和它的会话行。会话 id 由调用方定好：ACP 会话一个、每个终端一个。
fn insert_conversation(
    transaction: &Transaction<'_>,
    saved: &WorkspaceSessionRecord,
    position: i64,
    acp_id: Option<&str>,
    terminal_ids: &[String],
    preserved_history: &HashMap<String, Option<String>>,
) -> Result<(), StoreError> {
    let group_id = saved.id.trim();
    transaction.execute(
        "INSERT INTO session_group(id, position, custom_title, last_updated_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            group_id,
            position,
            saved.custom_title.as_deref(),
            saved.last_updated_at,
        ],
    )?;

    // 对话里的会话按位置排：ACP 会话在前，终端按客户端给的顺序跟在后面。
    let mut session_position = 0i64;
    if let (Some(acp), Some(session_id)) = (&saved.acp, acp_id) {
        transaction.execute(
            "INSERT INTO session(
                   id, group_id, kind, cwd, custom_title, launch_label, launch_command, position
                 ) VALUES (?1, ?2, 'acp', ?3, NULL, NULL, NULL, ?4)",
            params![session_id, group_id, acp.cwd.as_deref(), session_position],
        )?;
        let history_key = acp.sid.as_deref().unwrap_or(session_id);
        let history_session_id = history_session_id_for_rewrite(
            history_key,
            acp.history_session_id.as_deref(),
            preserved_history,
        );
        write_acp_record(transaction, session_id, acp, history_session_id.as_deref())?;
        session_position += 1;
    }
    for (terminal, session_id) in saved.terminals.iter().zip(terminal_ids) {
        transaction.execute(
            "INSERT INTO session(
                   id, group_id, kind, cwd, custom_title, launch_label, launch_command, position
                 ) VALUES (?1, ?2, 'terminal', ?3, ?4, ?5, ?6, ?7)",
            params![
                session_id,
                group_id,
                terminal.cwd.as_deref(),
                terminal.custom_title.as_deref(),
                terminal.launch_label.as_deref(),
                terminal.launch_cmd.as_deref(),
                session_position,
            ],
        )?;
        session_position += 1;
    }
    Ok(())
}

fn write_acp_record(
    transaction: &Transaction<'_>,
    session_id: &str,
    acp: &WorkspaceAcpRecord,
    history_session_id: Option<&str>,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO acp_detail(
               session_id, agent, profile_id, history_session_id, launch_command,
               refresh_launch_from_settings, pending_prompt, pending_delivery_id,
               agent_definition_id, automation_id, binding_automation_run_id,
               pending_agent_preset, session_title
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            session_id,
            acp.agent,
            acp.profile_id.as_deref(),
            history_session_id,
            acp.launch_command,
            acp.refresh_launch_from_settings,
            acp.pending_prompt.as_deref(),
            acp.pending_delivery_id.as_deref(),
            acp.agent_definition_id.as_deref(),
            acp.automation_id.as_deref(),
            acp.binding_automation_run_id.as_deref(),
            acp.pending_agent_preset.as_deref(),
            acp.session_title.as_deref(),
        ],
    )?;
    for (name, value) in &acp.launch_env {
        transaction.execute(
            "INSERT OR REPLACE INTO acp_launch_env(session_id, name, value) VALUES (?1, ?2, ?3)",
            params![session_id, name, value],
        )?;
    }
    for (position, (config_id, value_id)) in acp.config_values.iter().enumerate() {
        transaction.execute(
            "INSERT INTO acp_config_value(session_id, position, config_id, value_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_id, position as i64, config_id, value_id],
        )?;
    }
    Ok(())
}

/// 新建一段对话，排在最后。同 id 的对话已经在就什么都不做、返回 `false`：客户端的回执
/// 丢了会拿同一个 id 重发。名字不随新建写进来，只经改名命令写（见 [`rename_conversation`]）。
pub(crate) fn create_conversation(
    transaction: &Transaction<'_>,
    saved: &WorkspaceSessionRecord,
) -> Result<bool, StoreError> {
    let group_id = validated_conversation_id(saved)?;
    let saved = &with_names(saved, &StoredNames::default());
    if conversation_position(transaction, group_id)?.is_some() {
        return Ok(false);
    }
    let (acp_id, terminal_ids) = conversation_session_ids(transaction, saved)?;
    let position: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM session_group",
        [],
        |row| row.get(0),
    )?;
    insert_conversation(
        transaction,
        saved,
        position,
        acp_id.as_deref(),
        &terminal_ids,
        &preserved_history_session_ids(transaction)?,
    )?;
    Ok(true)
}

/// 整段替换一段已有的对话，位置不变。对话已经不在（别的客户端关掉了）回
/// [`StoreError::ConversationNotFound`]，不把它写回来。写完和原来一样（恢复身份以库里
/// 的为准，客户端带来的旧值不算改动）返回 `false`。名字和 agent 报的标题以库里的为准：
/// 名字只经改名命令写，agent 标题只由守护写，客户端带来的值不算数。
pub(crate) fn update_conversation(
    transaction: &Transaction<'_>,
    saved: &WorkspaceSessionRecord,
) -> Result<bool, StoreError> {
    let group_id = validated_conversation_id(saved)?;
    let position = conversation_position(transaction, group_id)?.ok_or_else(|| {
        StoreError::ConversationNotFound {
            id: group_id.to_string(),
        }
    })?;
    let (acp_id, terminal_ids) = conversation_session_ids(transaction, saved)?;
    let before = read_conversation(transaction, group_id)?;
    // 恢复身份、名字、agent 标题都以库里的为准，要在删行之前读出来。
    let preserved_history = preserved_history_session_ids(transaction)?;
    let saved = &with_names(saved, &stored_names(transaction, group_id)?);
    transaction.execute("DELETE FROM session_group WHERE id = ?1", [group_id])?;
    insert_conversation(
        transaction,
        saved,
        position,
        acp_id.as_deref(),
        &terminal_ids,
        &preserved_history,
    )?;
    Ok(read_conversation(transaction, group_id)? != before)
}

fn read_conversation(
    connection: &Connection,
    id: &str,
) -> Result<Option<WorkspaceSessionRecord>, StoreError> {
    Ok(read_workspace_snapshot(connection)?
        .and_then(|snapshot| snapshot.sessions.into_iter().find(|session| session.id == id)))
}

/// 删掉一段对话，返回删掉前的样子（调用方据此收掉它的终端和 ACP 会话）。已经不在时
/// 返回 `None`。
pub(crate) fn delete_conversation(
    transaction: &Transaction<'_>,
    id: &str,
) -> Result<Option<WorkspaceSessionRecord>, StoreError> {
    let id = id.trim();
    let removed = read_conversation(transaction, id)?;
    if removed.is_some() {
        transaction.execute("DELETE FROM session_group WHERE id = ?1", [id])?;
    }
    Ok(removed)
}

/// 打开一个项目，排在最后。已经开着返回 `false`。
pub(crate) fn open_project(transaction: &Transaction<'_>, root: &str) -> Result<bool, StoreError> {
    let root = root.trim();
    if root.is_empty() {
        return Err(StoreError::EmptyProjectRoot);
    }
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM project WHERE root = ?1)",
        [root],
        |row| row.get(0),
    )?;
    if exists {
        return Ok(false);
    }
    transaction.execute(
        "INSERT INTO project(root, position)
         VALUES (?1, (SELECT COALESCE(MAX(position) + 1, 0) FROM project))",
        [root],
    )?;
    Ok(true)
}

/// 关闭一个项目。项目下的对话还在：对话归哪个项目不存，客户端按 cwd 现算。没开着返回
/// `false`。
pub(crate) fn close_project(transaction: &Transaction<'_>, root: &str) -> Result<bool, StoreError> {
    let removed = transaction.execute("DELETE FROM project WHERE root = ?1", [root.trim()])?;
    Ok(removed > 0)
}

fn validated_conversation_id(saved: &WorkspaceSessionRecord) -> Result<&str, StoreError> {
    let group_id = saved.id.trim();
    if group_id.is_empty() {
        return Err(StoreError::ConversationMissingId);
    }
    if saved.acp.is_none() && saved.terminals.is_empty() {
        return Err(StoreError::EmptyConversation {
            id: group_id.to_string(),
        });
    }
    Ok(group_id)
}

fn conversation_position(
    transaction: &Transaction<'_>,
    group_id: &str,
) -> Result<Option<i64>, StoreError> {
    transaction
        .query_row(
            "SELECT position FROM session_group WHERE id = ?1",
            [group_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::from)
}

/// 会话 id 不能撞了就改名：终端 id 是守护里 PTY 的 id，改了就接不回去。ACP 会话必须带 `sid`；没起过的终端用对话 id 派生一个。撞上别的对话里的
/// 会话、或同一段对话里写了两遍，都拒绝。
fn conversation_session_ids(
    transaction: &Transaction<'_>,
    saved: &WorkspaceSessionRecord,
) -> Result<(Option<String>, Vec<String>), StoreError> {
    let group_id = saved.id.trim();
    let acp_id = match &saved.acp {
        Some(acp) => Some(
            acp.sid
                .as_deref()
                .map(str::trim)
                .filter(|sid| !sid.is_empty())
                .ok_or_else(|| StoreError::AcpConversationMissingSid {
                    id: group_id.to_string(),
                })?
                .to_string(),
        ),
        None => None,
    };
    let terminal_ids = saved
        .terminals
        .iter()
        .enumerate()
        .map(|(index, terminal)| {
            terminal
                .id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("{group_id}:terminal:{index}"))
        })
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    for id in acp_id.iter().chain(&terminal_ids) {
        if !seen.insert(id.as_str()) {
            return Err(StoreError::DuplicateSessionId { id: id.clone() });
        }
        let owner: Option<String> = transaction
            .query_row("SELECT group_id FROM session WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        if owner.is_some_and(|owner| owner != group_id) {
            return Err(StoreError::DuplicateSessionId { id: id.clone() });
        }
    }
    Ok((acp_id, terminal_ids))
}

/// 守护确认了一份恢复身份。行还不在（这段对话还没进目录）或 id 为空时返回 `None`，不建行；
/// 行在时返回库里是否真的变了。
pub(crate) fn remember_history_session_id(
    transaction: &Transaction<'_>,
    session_id: &str,
    history_id: &str,
) -> Result<Option<bool>, StoreError> {
    let Some(session_id) = normalize_history_id(Some(session_id)) else {
        return Ok(None);
    };
    let Some(history_id) = normalize_history_id(Some(history_id)) else {
        return Ok(None);
    };
    if matches!(
        stored_history_identity(transaction, &session_id)?,
        StoredHistoryIdentity::NoRow
    ) {
        return Ok(None);
    }
    let updated = transaction.execute(
        "UPDATE acp_detail SET history_session_id = ?1
         WHERE session_id = ?2 AND history_session_id IS NOT ?1",
        params![history_id, session_id],
    )?;
    // 有了身份，名字就属于那段 provider 对话：对话上起过的名字搬进 `history_title`（那边
    // 已经有名字就以那边的为准），对话上的清掉。名字任何时候只在一处。
    let moved = move_conversation_name_to_history(transaction, &session_id)?;
    Ok(Some(updated > 0 || moved))
}

fn move_conversation_name_to_history(
    transaction: &Transaction<'_>,
    session_id: &str,
) -> Result<bool, StoreError> {
    let Some((group_id, title, agent, profile_id, history_id)) = transaction
        .query_row(
            "SELECT g.id, g.custom_title, a.agent, COALESCE(a.profile_id, ''), a.history_session_id
             FROM acp_detail a
             JOIN session s ON s.id = a.session_id
             JOIN session_group g ON g.id = s.group_id
             WHERE a.session_id = ?1 AND g.custom_title IS NOT NULL
               AND a.history_session_id IS NOT NULL",
            [session_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(false);
    };
    transaction.execute(
        "INSERT OR IGNORE INTO history_title(agent, profile_id, resume_id, custom_title, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![agent, profile_id, history_id, title, now_ms()],
    )?;
    transaction.execute(
        "UPDATE session_group SET custom_title = NULL WHERE id = ?1",
        [group_id],
    )?;
    Ok(true)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// 库里一段对话的名字和 agent 标题。客户端改对话时用它们替掉客户端带来的值。
#[derive(Default)]
struct StoredNames {
    conversation: Option<String>,
    terminals: HashMap<String, Option<String>>,
    acp_session_title: Option<String>,
}

fn stored_names(transaction: &Transaction<'_>, group_id: &str) -> Result<StoredNames, StoreError> {
    let conversation = transaction
        .query_row(
            "SELECT custom_title FROM session_group WHERE id = ?1",
            [group_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let mut terminals = HashMap::new();
    {
        let mut statement = transaction.prepare(
            "SELECT id, custom_title FROM session WHERE group_id = ?1 AND kind = 'terminal'",
        )?;
        let rows = statement.query_map([group_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (id, title) = row?;
            terminals.insert(id, title);
        }
    }
    let acp_session_title = transaction
        .query_row(
            "SELECT a.session_title FROM acp_detail a JOIN session s ON s.id = a.session_id
             WHERE s.group_id = ?1",
            [group_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(StoredNames {
        conversation,
        terminals,
        acp_session_title,
    })
}

/// 客户端带来的对话，名字和 agent 标题换成库里的。
fn with_names(saved: &WorkspaceSessionRecord, names: &StoredNames) -> WorkspaceSessionRecord {
    let mut saved = saved.clone();
    saved.custom_title = names.conversation.clone();
    for terminal in &mut saved.terminals {
        terminal.custom_title = terminal
            .id
            .as_ref()
            .and_then(|id| names.terminals.get(id).cloned().flatten());
    }
    if let Some(acp) = &mut saved.acp {
        acp.session_title = names.acp_session_title.clone();
    }
    saved
}

/// 给一段对话改名，`None` 是清掉。认得出 provider 身份的 ACP 对话，名字属于那段 provider
/// 对话（历史页、手机上看到的都是它），写 `history_title`；没有身份的写对话自己的。对话不在
/// 回 [`StoreError::ConversationNotFound`]。
pub(crate) fn rename_conversation(
    transaction: &Transaction<'_>,
    conversation_id: &str,
    title: Option<&str>,
) -> Result<bool, StoreError> {
    let conversation_id = conversation_id.trim();
    if conversation_position(transaction, conversation_id)?.is_none() {
        return Err(StoreError::ConversationNotFound {
            id: conversation_id.to_string(),
        });
    }
    let title = title.map(str::trim).filter(|title| !title.is_empty());
    let identity = transaction
        .query_row(
            "SELECT a.agent, COALESCE(a.profile_id, ''), a.history_session_id
             FROM acp_detail a JOIN session s ON s.id = a.session_id
             WHERE s.group_id = ?1 AND a.history_session_id IS NOT NULL",
            [conversation_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    match identity {
        Some((agent, profile_id, resume_id)) => {
            let history = set_history_title(transaction, &agent, &profile_id, &resume_id, title)?;
            let cleared = transaction.execute(
                "UPDATE session_group SET custom_title = NULL
                 WHERE id = ?1 AND custom_title IS NOT NULL",
                [conversation_id],
            )?;
            Ok(history || cleared > 0)
        }
        None => Ok(transaction.execute(
            "UPDATE session_group SET custom_title = ?1 WHERE id = ?2 AND custom_title IS NOT ?1",
            params![title, conversation_id],
        )? > 0),
    }
}

/// 以 `sid` 为 ACP 会话的那段对话改名（命令行、Pi 工具按会话 id 改）。没有这段对话返回
/// `false`。
pub(crate) fn rename_acp_conversation(
    transaction: &Transaction<'_>,
    sid: &str,
    title: Option<&str>,
) -> Result<bool, StoreError> {
    let Some(group_id) = transaction
        .query_row(
            "SELECT session.group_id FROM acp_detail
             JOIN session ON session.id = acp_detail.session_id
             WHERE acp_detail.session_id = ?1",
            [sid.trim()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(false);
    };
    rename_conversation(transaction, &group_id, title)
}

/// 给一格终端改名（这格还认不出 provider 身份时；认得出的写 `history_title`）。
pub(crate) fn rename_terminal(
    transaction: &Transaction<'_>,
    terminal_id: &str,
    title: Option<&str>,
) -> Result<bool, StoreError> {
    let title = title.map(str::trim).filter(|title| !title.is_empty());
    Ok(transaction.execute(
        "UPDATE session SET custom_title = ?1
         WHERE id = ?2 AND kind = 'terminal' AND custom_title IS NOT ?1",
        params![title, terminal_id.trim()],
    )? > 0)
}

/// agent 报的标题变了（只由守护写）。没有这段 ACP 会话返回 `false`。
pub(crate) fn set_acp_session_title(
    transaction: &Transaction<'_>,
    sid: &str,
    title: Option<&str>,
) -> Result<bool, StoreError> {
    let title = title.map(str::trim).filter(|title| !title.is_empty());
    Ok(transaction.execute(
        "UPDATE acp_detail SET session_title = ?1 WHERE session_id = ?2 AND session_title IS NOT ?1",
        params![title, sid.trim()],
    )? > 0)
}

/// 一段 provider 对话的名字（历史页、侧栏、手机看到的同一份），`None` 是清掉。
pub(crate) fn set_history_title(
    transaction: &Transaction<'_>,
    agent: &str,
    profile_id: &str,
    resume_id: &str,
    title: Option<&str>,
) -> Result<bool, StoreError> {
    let title = title.map(str::trim).filter(|title| !title.is_empty());
    let changed = match title {
        Some(title) => transaction.execute(
            "INSERT INTO history_title(agent, profile_id, resume_id, custom_title, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(agent, profile_id, resume_id) DO UPDATE
               SET custom_title = excluded.custom_title, updated_at_ms = excluded.updated_at_ms
               WHERE history_title.custom_title IS NOT excluded.custom_title",
            params![agent, profile_id, resume_id, title, now_ms()],
        )?,
        None => transaction.execute(
            "DELETE FROM history_title WHERE agent = ?1 AND profile_id = ?2 AND resume_id = ?3",
            params![agent, profile_id, resume_id],
        )?,
    };
    Ok(changed > 0)
}

/// 守护放弃一份恢复身份。返回库里是否真的变了：没有行、或本来就是空，都是 `false`。
pub(crate) fn clear_history_session_id(
    transaction: &Transaction<'_>,
    session_id: &str,
) -> Result<bool, StoreError> {
    let Some(session_id) = normalize_history_id(Some(session_id)) else {
        return Ok(false);
    };
    let updated = transaction.execute(
        "UPDATE acp_detail SET history_session_id = NULL
         WHERE session_id = ?1 AND history_session_id IS NOT NULL",
        [session_id],
    )?;
    Ok(updated > 0)
}

pub(crate) fn history_session_id(
    connection: &Connection,
    session_id: &str,
) -> Result<Option<String>, StoreError> {
    Ok(match stored_history_identity(connection, session_id)? {
        StoredHistoryIdentity::Bound(id) => Some(id),
        StoredHistoryIdentity::NoRow | StoredHistoryIdentity::Unbound => None,
    })
}

pub(crate) fn stored_history_identity(
    connection: &Connection,
    session_id: &str,
) -> Result<StoredHistoryIdentity, StoreError> {
    let Some(session_id) = normalize_history_id(Some(session_id)) else {
        return Ok(StoredHistoryIdentity::NoRow);
    };
    let stored = connection
        .query_row(
            "SELECT history_session_id FROM acp_detail WHERE session_id = ?1",
            [session_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    Ok(match stored {
        None => StoredHistoryIdentity::NoRow,
        Some(id) => match normalize_history_id(id.as_deref()) {
            Some(id) => StoredHistoryIdentity::Bound(id),
            None => StoredHistoryIdentity::Unbound,
        },
    })
}

/// 会话 id → 智能体定义 id。
pub(crate) fn read_session_agent_definition_ids(
    connection: &Connection,
) -> Result<BTreeMap<String, String>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT session_id, agent_definition_id FROM acp_detail
         WHERE agent_definition_id IS NOT NULL AND agent_definition_id != ''",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut map = BTreeMap::new();
    for row in rows {
        let (sid, id) = row?;
        let sid = sid.trim();
        let id = id.trim();
        if sid.is_empty() || id.is_empty() {
            continue;
        }
        map.insert(sid.to_string(), id.to_string());
    }
    Ok(map)
}

pub(crate) fn read_workspace_snapshot(
    connection: &Connection,
) -> Result<Option<WorkspaceSnapshot>, StoreError> {
    let has_projects: bool =
        connection.query_row("SELECT EXISTS(SELECT 1 FROM project)", [], |row| row.get(0))?;
    let has_groups: bool =
        connection.query_row("SELECT EXISTS(SELECT 1 FROM session_group)", [], |row| {
            row.get(0)
        })?;
    if !has_projects && !has_groups {
        return Ok(None);
    }

    let projects = connection
        .prepare("SELECT root FROM project ORDER BY position")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;

    let groups = connection
        // 对话的名字：认得出 provider 身份的 ACP 对话用那段对话的名字（`history_title`），
        // 没有身份的用对话自己的。一个名字只存在其中一处，读的时候合起来。
        .prepare(
            "SELECT g.id, COALESCE(h.custom_title, g.custom_title), g.last_updated_at
             FROM session_group g
             LEFT JOIN session s ON s.group_id = g.id AND s.kind = 'acp'
             LEFT JOIN acp_detail a ON a.session_id = s.id
             LEFT JOIN history_title h
               ON h.agent = a.agent
              AND h.profile_id = COALESCE(a.profile_id, '')
              AND h.resume_id = a.history_session_id
             ORDER BY g.position",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut cache = WorkspaceReadCache::load(connection)?;
    let mut sessions = Vec::new();
    for (group_id, custom_title, last_updated_at) in groups {
        let terminals = cache
            .terminals_by_group
            .remove(&group_id)
            .unwrap_or_default();
        let acp = match cache.acp_session_by_group.get(&group_id).cloned() {
            Some(session_id) => Some(cache.acp(&session_id)?),
            None => None,
        };
        sessions.push(WorkspaceSessionRecord {
            id: group_id,
            custom_title,
            last_updated_at: last_updated_at.max(0),
            terminals,
            acp,
        });
    }
    Ok(Some(WorkspaceSnapshot { projects, sessions }))
}

/// 一次把工作区相关的表都读进内存，再拼成快照，免得每段对话一次查询。
struct WorkspaceReadCache {
    /// 各段对话里的终端，按位置排好。
    terminals_by_group: HashMap<String, Vec<WorkspaceTerminalRecord>>,
    /// ACP 会话的 cwd。
    acp_cwds: HashMap<String, Option<String>>,
    acp_session_by_group: HashMap<String, String>,
    acp_rows: HashMap<String, WorkspaceAcpRecord>,
    launch_env: HashMap<String, Vec<(String, String)>>,
    config_values: HashMap<String, Vec<(String, String)>>,
}

impl WorkspaceReadCache {
    fn load(connection: &Connection) -> Result<Self, StoreError> {
        let mut terminals_by_group: HashMap<String, Vec<WorkspaceTerminalRecord>> = HashMap::new();
        let mut acp_cwds = HashMap::new();
        let mut acp_session_by_group = HashMap::new();
        let mut statement = connection.prepare(
            "SELECT id, group_id, kind, cwd, custom_title, launch_label, launch_command
             FROM session ORDER BY group_id, position",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                WorkspaceTerminalRecord {
                    id: None,
                    cwd: row.get(3)?,
                    custom_title: row.get(4)?,
                    launch_label: row.get(5)?,
                    launch_cmd: row.get(6)?,
                },
            ))
        })?;
        for row in rows {
            let (id, group_id, kind, mut terminal) = row?;
            if kind == "acp" {
                acp_cwds.insert(id.clone(), terminal.cwd);
                acp_session_by_group.entry(group_id).or_insert(id);
            } else {
                terminal.id = Some(id);
                terminals_by_group
                    .entry(group_id)
                    .or_default()
                    .push(terminal);
            }
        }

        let mut acp_rows = HashMap::new();
        let mut statement = connection.prepare(
            "SELECT session_id, agent, profile_id, history_session_id, launch_command,
                    refresh_launch_from_settings, pending_prompt, pending_delivery_id,
                    agent_definition_id, automation_id, binding_automation_run_id,
                    pending_agent_preset, session_title
             FROM acp_detail",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                WorkspaceAcpRecord {
                    cwd: None,
                    sid: None,
                    agent: row.get(1)?,
                    profile_id: row.get(2)?,
                    history_session_id: row.get(3)?,
                    launch_command: row.get(4)?,
                    launch_env: Vec::new(),
                    refresh_launch_from_settings: row.get(5)?,
                    pending_prompt: row.get(6)?,
                    pending_delivery_id: row.get(7)?,
                    agent_definition_id: row.get(8)?,
                    automation_id: row.get(9)?,
                    config_values: Vec::new(),
                    binding_automation_run_id: row.get(10)?,
                    pending_agent_preset: row.get(11)?,
                    session_title: row.get(12)?,
                },
            ))
        })?;
        for row in rows {
            let (session_id, detail) = row?;
            acp_rows.insert(session_id, detail);
        }

        let mut launch_env: HashMap<String, Vec<(String, String)>> = HashMap::new();
        let mut statement = connection.prepare(
            "SELECT session_id, name, value FROM acp_launch_env ORDER BY session_id, name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (session_id, name, value) = row?;
            launch_env
                .entry(session_id)
                .or_default()
                .push((name, value));
        }

        let mut config_values: HashMap<String, Vec<(String, String)>> = HashMap::new();
        let mut statement = connection.prepare(
            "SELECT session_id, config_id, value_id FROM acp_config_value
             ORDER BY session_id, position",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (session_id, config_id, value_id) = row?;
            config_values
                .entry(session_id)
                .or_default()
                .push((config_id, value_id));
        }

        Ok(Self {
            terminals_by_group,
            acp_cwds,
            acp_session_by_group,
            acp_rows,
            launch_env,
            config_values,
        })
    }

    fn acp(&mut self, session_id: &str) -> Result<WorkspaceAcpRecord, StoreError> {
        let mut record = self
            .acp_rows
            .remove(session_id)
            .ok_or_else(|| StoreError::from(rusqlite::Error::QueryReturnedNoRows))?;
        record.cwd = self.acp_cwds.get(session_id).cloned().flatten();
        record.sid = Some(session_id.to_string());
        record.launch_env = self.launch_env.remove(session_id).unwrap_or_default();
        record.config_values = self.config_values.remove(session_id).unwrap_or_default();
        Ok(record)
    }
}

