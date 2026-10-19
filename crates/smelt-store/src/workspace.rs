//! 运行时持有的工作区目录：项目、对话（会话组）、对话里的终端和 ACP 会话细节。终端怎么
//! 分屏归客户端，不在这里（设计文档 D1）。

use crate::{
    StoreError, StoredHistoryIdentity, WorkspaceAcpRecord, WorkspaceSessionRecord,
    WorkspaceSnapshot, WorkspaceTerminalRecord,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::{BTreeMap, HashMap, HashSet};

/// 行已经存在时，恢复身份以库里的值为准。窗口快照可以在第一次插入时填上，
/// 之后不能改写或清空；换 id、放弃旧 id 只走守护进程的显式写入。
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
    preserved: &HashMap<String, Option<String>>,
) -> Option<String> {
    // 没有行，或列还是空，第一次落库也不采用窗口载荷。
    // 这一列只由守护进程 remember / clear 来写。
    preserved
        .get(session_id)
        .and_then(|stored| normalize_history_id(stored.as_deref()))
}

/// 整份替换工作区目录。会话和 ACP 细节都挂在会话组上，删组即级联删光。
pub(crate) fn write_workspace_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &WorkspaceSnapshot,
) -> Result<(), StoreError> {
    let preserved_history = preserved_history_session_ids(transaction)?;
    transaction.execute("DELETE FROM session_group", [])?;
    transaction.execute("DELETE FROM project", [])?;

    let mut project_roots = Vec::new();
    let mut seen_projects = HashSet::new();
    for root in &snapshot.projects {
        let root = root.trim();
        if root.is_empty() || !seen_projects.insert(root.to_string()) {
            continue;
        }
        let position = project_roots.len() as i64;
        transaction.execute(
            "INSERT INTO project(root, position) VALUES (?1, ?2)",
            params![root, position],
        )?;
        project_roots.push(root.to_string());
    }

    let mut used_group_ids = HashSet::new();
    let mut used_session_ids = HashSet::new();
    for (position, session) in snapshot.sessions.iter().enumerate() {
        write_session_record(
            transaction,
            session,
            position,
            &project_roots,
            &mut used_group_ids,
            &mut used_session_ids,
            &preserved_history,
        )?;
    }
    Ok(())
}

fn write_session_record(
    transaction: &Transaction<'_>,
    saved: &WorkspaceSessionRecord,
    position: usize,
    project_roots: &[String],
    used_group_ids: &mut HashSet<String>,
    used_session_ids: &mut HashSet<String>,
    preserved_history: &HashMap<String, Option<String>>,
) -> Result<(), StoreError> {
    // 对话 id 由客户端定、之后不再变，客户端按它存这段对话的界面状态，所以这里不能改写。
    let group_id = saved.id.trim();
    if group_id.is_empty() {
        return Err(StoreError::ConversationMissingId { position });
    }
    if !used_group_ids.insert(group_id.to_string()) {
        return Err(StoreError::DuplicateConversationId {
            id: group_id.to_string(),
        });
    }
    if saved.acp.is_none() && saved.terminals.is_empty() {
        return Err(StoreError::EmptyConversation {
            id: group_id.to_string(),
        });
    }
    let cwd = saved
        .acp
        .as_ref()
        .and_then(|acp| acp.cwd.as_deref())
        .or_else(|| {
            saved
                .terminals
                .iter()
                .find_map(|terminal| terminal.cwd.as_deref())
        });
    let project_root = cwd.and_then(|cwd| matching_project_root(cwd, project_roots));

    transaction.execute(
        "INSERT INTO session_group(id, project_root, position, custom_title, last_updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            group_id,
            project_root,
            position as i64,
            saved.custom_title.as_deref(),
            saved.last_updated_at,
        ],
    )?;

    // 对话里的会话按位置排：ACP 会话在前，终端按客户端给的顺序跟在后面。
    let mut session_position = 0i64;
    if let Some(acp) = &saved.acp {
        let candidate = acp
            .sid
            .as_deref()
            .or(acp.history_session_id.as_deref())
            .unwrap_or("acp");
        let session_id = unique_id(
            candidate.to_string(),
            used_session_ids,
            format!("{group_id}:acp"),
        );
        transaction.execute(
            "INSERT INTO session(
                   id, group_id, kind, cwd, custom_title, launch_label, launch_command, position
                 ) VALUES (?1, ?2, 'acp', ?3, NULL, NULL, NULL, ?4)",
            params![session_id, group_id, acp.cwd.as_deref(), session_position],
        )?;
        let history_key = acp.sid.as_deref().unwrap_or(session_id.as_str());
        let history_session_id = history_session_id_for_rewrite(history_key, preserved_history);
        write_acp_record(transaction, &session_id, acp, history_session_id.as_deref())?;
        session_position += 1;
    }
    for (index, terminal) in saved.terminals.iter().enumerate() {
        let session_id = unique_id(
            terminal.id.clone().unwrap_or_default(),
            used_session_ids,
            format!("{group_id}:terminal:{index}"),
        );
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

pub(crate) fn remember_history_session_id(
    connection: &Connection,
    session_id: &str,
    history_id: &str,
) -> Result<bool, StoreError> {
    let Some(session_id) = normalize_history_id(Some(session_id)) else {
        return Ok(false);
    };
    let Some(history_id) = normalize_history_id(Some(history_id)) else {
        return Ok(false);
    };
    let updated = connection.execute(
        "UPDATE acp_detail SET history_session_id = ?1 WHERE session_id = ?2",
        params![history_id, session_id],
    )?;
    Ok(updated > 0)
}

pub(crate) fn clear_history_session_id(
    connection: &Connection,
    session_id: &str,
) -> Result<bool, StoreError> {
    let Some(session_id) = normalize_history_id(Some(session_id)) else {
        return Ok(false);
    };
    let updated = connection.execute(
        "UPDATE acp_detail SET history_session_id = NULL WHERE session_id = ?1",
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
        .prepare("SELECT id, custom_title, last_updated_at FROM session_group ORDER BY position")?
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

fn matching_project_root<'a>(cwd: &str, roots: &'a [String]) -> Option<&'a str> {
    roots
        .iter()
        .filter(|root| path_contains(root, cwd))
        .max_by_key(|root| root.len())
        .map(String::as_str)
}

fn path_contains(root: &str, path: &str) -> bool {
    let root = root.trim_end_matches(['/', '\\']);
    let path = path.trim_end_matches(['/', '\\']);
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/') || rest.starts_with('\\'))
}

fn unique_id(candidate: String, used: &mut HashSet<String>, fallback: String) -> String {
    let base = if candidate.trim().is_empty() {
        fallback
    } else {
        candidate
    };
    if used.insert(base.clone()) {
        return base;
    }
    for suffix in 2usize.. {
        let value = format!("{base}#{suffix}");
        if used.insert(value.clone()) {
            return value;
        }
    }
    unreachable!()
}
