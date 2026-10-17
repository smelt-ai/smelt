//! 运行时持有的工作区目录：项目、会话组、分屏布局、会话和 ACP 会话细节。

use crate::{
    StoreError, StoredHistoryIdentity, WorkspaceAcpRecord, WorkspaceLayoutNode,
    WorkspaceSessionRecord, WorkspaceSnapshot,
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

/// 整份替换工作区目录。会话、布局和 ACP 细节都挂在会话组上，删组即级联删光。
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
    let anchor = saved
        .acp
        .as_ref()
        .and_then(|acp| acp.sid.as_deref().or(acp.history_session_id.as_deref()))
        .or_else(|| first_leaf_id(&saved.layout))
        .unwrap_or("session");
    let group_id = unique_id(
        format!("group:{position}:{anchor}"),
        used_group_ids,
        format!("group:{position}"),
    );
    let cwd = saved
        .acp
        .as_ref()
        .and_then(|acp| acp.cwd.as_deref())
        .or_else(|| first_leaf_cwd(&saved.layout));
    let project_root = cwd.and_then(|cwd| matching_project_root(cwd, project_roots));

    transaction.execute(
        "INSERT INTO session_group(
               id, project_root, position, active_session_id, custom_title, last_updated_at
             ) VALUES (?1, ?2, ?3, NULL, ?4, ?5)",
        params![
            group_id,
            project_root,
            position as i64,
            saved.custom_title.as_deref(),
            saved.last_updated_at,
        ],
    )?;

    let active_session_id = if let Some(acp) = &saved.acp {
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
                   id, group_id, kind, cwd, custom_title, launch_label, launch_command
                 ) VALUES (?1, ?2, 'acp', ?3, NULL, NULL, NULL)",
            params![session_id, group_id, acp.cwd.as_deref()],
        )?;
        transaction.execute(
            "INSERT INTO layout_node(
                   id, group_id, parent_id, position, kind, axis, session_id, size_px
                 ) VALUES (?1, ?2, NULL, 0, 'leaf', NULL, ?3, NULL)",
            params![format!("{group_id}:node:0"), group_id, session_id],
        )?;
        let history_key = acp.sid.as_deref().unwrap_or(session_id.as_str());
        let history_session_id = history_session_id_for_rewrite(history_key, preserved_history);
        write_acp_record(transaction, &session_id, acp, history_session_id.as_deref())?;
        session_id
    } else {
        let mut node_counter = 0usize;
        let mut leaf_ids = Vec::new();
        {
            let mut writer = LayoutWriter {
                transaction,
                group_id: &group_id,
                node_counter: &mut node_counter,
                used_session_ids,
                leaf_ids: &mut leaf_ids,
            };
            writer.write(None, 0, None, &saved.layout)?;
        }
        leaf_ids
            .get(saved.active)
            .or_else(|| leaf_ids.first())
            .cloned()
            .ok_or_else(|| StoreError::SessionGroupMissingLeaf {
                group_id: group_id.clone(),
            })?
    };

    transaction.execute(
        "UPDATE session_group SET active_session_id = ?1 WHERE id = ?2",
        params![active_session_id, group_id],
    )?;
    Ok(())
}

fn first_leaf_id(layout: &WorkspaceLayoutNode) -> Option<&str> {
    match layout {
        WorkspaceLayoutNode::Leaf { id, .. } => id.as_deref(),
        WorkspaceLayoutNode::Split { children, .. } => children.iter().find_map(first_leaf_id),
    }
}

fn first_leaf_cwd(layout: &WorkspaceLayoutNode) -> Option<&str> {
    match layout {
        WorkspaceLayoutNode::Leaf { cwd, .. } => cwd.as_deref(),
        WorkspaceLayoutNode::Split { children, .. } => children.iter().find_map(first_leaf_cwd),
    }
}

struct LayoutWriter<'a, 'conn> {
    transaction: &'a Transaction<'conn>,
    group_id: &'a str,
    node_counter: &'a mut usize,
    used_session_ids: &'a mut HashSet<String>,
    leaf_ids: &'a mut Vec<String>,
}

impl LayoutWriter<'_, '_> {
    fn write(
        &mut self,
        parent_id: Option<&str>,
        position: usize,
        size_px: Option<f64>,
        layout: &WorkspaceLayoutNode,
    ) -> Result<(), StoreError> {
        let node_id = format!("{}:node:{}", self.group_id, *self.node_counter);
        *self.node_counter += 1;
        match layout {
            WorkspaceLayoutNode::Split {
                vertical,
                children,
                sizes,
            } => {
                let axis = if *vertical { "vertical" } else { "horizontal" };
                self.transaction.execute(
                    "INSERT INTO layout_node(
                           id, group_id, parent_id, position, kind, axis, session_id, size_px
                         ) VALUES (?1, ?2, ?3, ?4, 'split', ?5, NULL, ?6)",
                    params![
                        node_id,
                        self.group_id,
                        parent_id,
                        position as i64,
                        axis,
                        size_px
                    ],
                )?;
                for (child_position, child) in children.iter().enumerate() {
                    let child_size = sizes.get(child_position).copied();
                    self.write(Some(&node_id), child_position, child_size, child)?;
                }
                Ok(())
            }
            WorkspaceLayoutNode::Leaf {
                cwd,
                id,
                custom_title,
                launch_label,
                launch_cmd,
            } => {
                let candidate = id.as_deref().unwrap_or("terminal");
                let session_id = unique_id(
                    candidate.to_string(),
                    self.used_session_ids,
                    format!("{}:terminal:{}", self.group_id, self.leaf_ids.len()),
                );
                self.transaction.execute(
                    "INSERT INTO session(
                           id, group_id, kind, cwd, custom_title, launch_label, launch_command
                         ) VALUES (?1, ?2, 'terminal', ?3, ?4, ?5, ?6)",
                    params![
                        session_id,
                        self.group_id,
                        cwd.as_deref(),
                        custom_title.as_deref(),
                        launch_label.as_deref(),
                        launch_cmd.as_deref(),
                    ],
                )?;
                self.transaction.execute(
                    "INSERT INTO layout_node(
                           id, group_id, parent_id, position, kind, axis, session_id, size_px
                         ) VALUES (?1, ?2, ?3, ?4, 'leaf', NULL, ?5, ?6)",
                    params![
                        node_id,
                        self.group_id,
                        parent_id,
                        position as i64,
                        session_id,
                        size_px
                    ],
                )?;
                self.leaf_ids.push(session_id);
                Ok(())
            }
        }
    }
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
        .prepare(
            "SELECT id, active_session_id, custom_title, last_updated_at
             FROM session_group ORDER BY position",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut cache = WorkspaceReadCache::load(connection)?;
    let mut sessions = Vec::new();
    for (group_id, active_session_id, custom_title, last_updated_at) in groups {
        let root_id = cache
            .layout_children
            .get(&(group_id.clone(), String::new()))
            .and_then(|children| children.first())
            .map(|node| node.id.clone())
            .ok_or_else(|| StoreError::from(rusqlite::Error::QueryReturnedNoRows))?;
        let mut leaf_ids = Vec::new();
        let layout = cache.layout(&group_id, &root_id, &mut leaf_ids)?;
        let active = active_session_id
            .as_deref()
            .and_then(|active| leaf_ids.iter().position(|session_id| session_id == active))
            .unwrap_or(0);
        let acp = match cache.acp_session_by_group.get(&group_id).cloned() {
            Some(session_id) => Some(cache.acp(&session_id)?),
            None => None,
        };
        sessions.push(WorkspaceSessionRecord {
            custom_title,
            last_updated_at: last_updated_at.max(0),
            active,
            layout,
            acp,
        });
    }
    Ok(Some(WorkspaceSnapshot { projects, sessions }))
}

#[derive(Clone)]
struct LayoutNodeRow {
    id: String,
    group_id: String,
    kind: String,
    axis: Option<String>,
    session_id: Option<String>,
    size_px: Option<f64>,
}

struct SessionRow {
    kind: String,
    cwd: Option<String>,
    custom_title: Option<String>,
    launch_label: Option<String>,
    launch_command: Option<String>,
}

/// 一次把工作区相关的表都读进内存，再拼成快照，免得每个节点一次查询。
struct WorkspaceReadCache {
    layout_children: HashMap<(String, String), Vec<LayoutNodeRow>>,
    layout_by_id: HashMap<String, LayoutNodeRow>,
    sessions: HashMap<String, SessionRow>,
    acp_session_by_group: HashMap<String, String>,
    acp_rows: HashMap<String, WorkspaceAcpRecord>,
    launch_env: HashMap<String, Vec<(String, String)>>,
    config_values: HashMap<String, Vec<(String, String)>>,
}

impl WorkspaceReadCache {
    fn load(connection: &Connection) -> Result<Self, StoreError> {
        let mut layout_children: HashMap<(String, String), Vec<(i64, LayoutNodeRow)>> =
            HashMap::new();
        let mut layout_by_id = HashMap::new();
        let mut statement = connection.prepare(
            "SELECT id, group_id, parent_id, position, kind, axis, session_id, size_px
             FROM layout_node",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                LayoutNodeRow {
                    id: row.get(0)?,
                    group_id: row.get(1)?,
                    kind: row.get(4)?,
                    axis: row.get(5)?,
                    session_id: row.get(6)?,
                    size_px: row.get(7)?,
                },
            ))
        })?;
        for row in rows {
            let (parent_id, position, node) = row?;
            layout_by_id.insert(node.id.clone(), node.clone());
            layout_children
                .entry((node.group_id.clone(), parent_id.unwrap_or_default()))
                .or_default()
                .push((position, node));
        }
        let layout_children = layout_children
            .into_iter()
            .map(|(key, mut children)| {
                children.sort_by_key(|(position, _)| *position);
                (key, children.into_iter().map(|(_, node)| node).collect())
            })
            .collect();

        let mut sessions = HashMap::new();
        let mut acp_session_by_group = HashMap::new();
        let mut statement = connection.prepare(
            "SELECT id, group_id, kind, cwd, custom_title, launch_label, launch_command
             FROM session ORDER BY rowid",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                SessionRow {
                    kind: row.get(2)?,
                    cwd: row.get(3)?,
                    custom_title: row.get(4)?,
                    launch_label: row.get(5)?,
                    launch_command: row.get(6)?,
                },
            ))
        })?;
        for row in rows {
            let (id, group_id, session) = row?;
            if session.kind == "acp" {
                acp_session_by_group
                    .entry(group_id)
                    .or_insert_with(|| id.clone());
            }
            sessions.insert(id, session);
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
            layout_children,
            layout_by_id,
            sessions,
            acp_session_by_group,
            acp_rows,
            launch_env,
            config_values,
        })
    }

    fn layout(
        &self,
        group_id: &str,
        node_id: &str,
        leaf_ids: &mut Vec<String>,
    ) -> Result<WorkspaceLayoutNode, StoreError> {
        let node = self
            .layout_by_id
            .get(node_id)
            .filter(|node| node.group_id == group_id)
            .ok_or_else(|| StoreError::from(rusqlite::Error::QueryReturnedNoRows))?;
        if node.kind == "leaf" {
            let session_id =
                node.session_id
                    .clone()
                    .ok_or_else(|| StoreError::LayoutLeafMissingSession {
                        node_id: node_id.to_string(),
                    })?;
            let session = self
                .sessions
                .get(&session_id)
                .ok_or_else(|| StoreError::from(rusqlite::Error::QueryReturnedNoRows))?;
            leaf_ids.push(session_id.clone());
            return Ok(WorkspaceLayoutNode::Leaf {
                cwd: session.cwd.clone(),
                id: (session.kind == "terminal").then_some(session_id),
                custom_title: session.custom_title.clone(),
                launch_label: session.launch_label.clone(),
                launch_cmd: session.launch_command.clone(),
            });
        }

        let empty = Vec::new();
        let children = self
            .layout_children
            .get(&(group_id.to_string(), node_id.to_string()))
            .unwrap_or(&empty);
        // 尺寸要么全有要么全无：缺一个就按均分，免得错位套到别的子块上。
        let all_sized =
            !children.is_empty() && children.iter().all(|child| child.size_px.is_some());
        let mut layouts = Vec::new();
        let mut sizes = Vec::new();
        for child in children {
            layouts.push(self.layout(group_id, &child.id, leaf_ids)?);
            if all_sized {
                sizes.push(child.size_px.unwrap_or_default());
            }
        }
        Ok(WorkspaceLayoutNode::Split {
            vertical: node.axis.as_deref() == Some("vertical"),
            children: layouts,
            sizes,
        })
    }

    fn acp(&mut self, session_id: &str) -> Result<WorkspaceAcpRecord, StoreError> {
        let mut record = self
            .acp_rows
            .remove(session_id)
            .ok_or_else(|| StoreError::from(rusqlite::Error::QueryReturnedNoRows))?;
        record.cwd = self
            .sessions
            .get(session_id)
            .and_then(|session| session.cwd.clone());
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
