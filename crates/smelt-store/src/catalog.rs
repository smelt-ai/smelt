//! 智能体配置、远程会话、守护会话目录的关系表读写。

use crate::{
    AgentAcpConfigMemoryRecord, AgentAcpEnvRecord, AgentConversationCommandRecord,
    AgentDefinitionRecord, AgentProfileRecord, AgentUiSnapshot, PublishedSessionRecord,
    PublishedSessionSnapshot, RemoteAcpSessionRecord, RemoteSessionCatalogSnapshot,
    RemoteTerminalSessionRecord, StoreError,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

pub(crate) fn read_agent_ui_snapshot(
    connection: &Connection,
) -> Result<Option<AgentUiSnapshot>, StoreError> {
    let hooks_enabled: Option<bool> = connection
        .query_row(
            "SELECT hooks_enabled FROM agent_host_pref WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if hooks_enabled.is_none() && !agent_definitions_exist(connection)? {
        return Ok(None);
    }
    Ok(Some(AgentUiSnapshot {
        agent_hooks_enabled: hooks_enabled.unwrap_or(true),
        commands: query_commands(connection)?,
        env: query_env(connection)?,
        config_memory: query_config_memory(connection)?,
        agents: query_agents(connection)?,
        profiles: query_profiles(connection)?,
    }))
}

pub(crate) fn write_agent_ui_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &AgentUiSnapshot,
) -> Result<(), StoreError> {
    write_agent_ui_prefs(transaction, snapshot)?;
    replace_agent_definitions(transaction, &snapshot.agents)?;
    Ok(())
}

/// 写智能体宿主偏好、启动命令和 profile，不动 `agent_definition`。
///
/// 改偏好时不能整表重写定义：否则并发的单行 create 会被内存里的旧清单盖掉。
pub(crate) fn write_agent_ui_prefs(
    transaction: &Transaction<'_>,
    snapshot: &AgentUiSnapshot,
) -> Result<(), StoreError> {
    transaction.execute("DELETE FROM agent_acp_command", [])?;
    transaction.execute("DELETE FROM agent_acp_env", [])?;
    transaction.execute("DELETE FROM agent_acp_config_memory", [])?;
    transaction.execute("DELETE FROM agent_profile", [])?;
    transaction.execute(
        "INSERT INTO agent_host_pref(id, hooks_enabled) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET hooks_enabled = excluded.hooks_enabled",
        params![snapshot.agent_hooks_enabled],
    )?;
    for command in &snapshot.commands {
        transaction.execute(
            "INSERT INTO agent_acp_command(command_key, command) VALUES (?1, ?2)",
            params![command.command_key, command.command],
        )?;
    }
    for env in &snapshot.env {
        transaction.execute(
            "INSERT INTO agent_acp_env(engine_kind_id, name, value) VALUES (?1, ?2, ?3)",
            params![env.engine_kind_id, env.name, env.value],
        )?;
    }
    for (position, memory) in snapshot.config_memory.iter().enumerate() {
        transaction.execute(
            "INSERT INTO agent_acp_config_memory(engine_kind_id, config_id, value_id, position)
                 VALUES (?1, ?2, ?3, ?4)",
            params![
                memory.engine_kind_id,
                memory.config_id,
                memory.value_id,
                position as i64
            ],
        )?;
    }
    for (position, profile) in snapshot.profiles.iter().enumerate() {
        transaction.execute(
            "INSERT INTO agent_profile(id, kind_id, label, workspace_dir, position)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                profile.id,
                profile.kind_id,
                profile.label,
                profile.workspace_dir,
                position as i64,
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn replace_agent_definitions(
    transaction: &Transaction<'_>,
    agents: &[AgentDefinitionRecord],
) -> Result<(), StoreError> {
    transaction.execute("DELETE FROM agent_definition", [])?;
    for (position, agent) in agents.iter().enumerate() {
        insert_agent_definition_at(transaction, agent, position as i64)?;
    }
    Ok(())
}

fn insert_agent_definition_at(
    transaction: &Transaction<'_>,
    agent: &AgentDefinitionRecord,
    position: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO agent_definition(
               id, name, description, engine_kind_id, prompt, plugins_json,
               mcp_servers_json, context_folders_json, context_links_json,
               model_provider, model_id, memory_enabled, position
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            agent.id,
            agent.name,
            agent.description,
            agent.engine_kind_id,
            agent.prompt,
            &agent.plugins_json,
            &agent.mcp_servers_json,
            &agent.context_folders_json,
            &agent.context_links_json,
            agent.model_provider,
            agent.model_id,
            agent.memory_enabled,
            position,
        ],
    )?;
    Ok(())
}

pub(crate) fn insert_agent_definition(
    transaction: &Transaction<'_>,
    agent: &AgentDefinitionRecord,
) -> Result<(), StoreError> {
    let next_position: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM agent_definition",
        [],
        |row| row.get(0),
    )?;
    insert_agent_definition_at(transaction, agent, next_position)
}

pub(crate) fn update_agent_definition(
    transaction: &Transaction<'_>,
    agent: &AgentDefinitionRecord,
) -> Result<bool, StoreError> {
    let changed = transaction.execute(
        "UPDATE agent_definition
             SET name = ?1,
                 description = ?2,
                 engine_kind_id = ?3,
                 prompt = ?4,
                 plugins_json = ?5,
                 mcp_servers_json = ?6,
                 context_folders_json = ?7,
                 context_links_json = ?8,
                 model_provider = ?9,
                 model_id = ?10,
                 memory_enabled = ?11
           WHERE id = ?12",
        params![
            agent.name,
            agent.description,
            agent.engine_kind_id,
            agent.prompt,
            &agent.plugins_json,
            &agent.mcp_servers_json,
            &agent.context_folders_json,
            &agent.context_links_json,
            agent.model_provider,
            agent.model_id,
            agent.memory_enabled,
            agent.id,
        ],
    )?;
    Ok(changed > 0)
}

pub(crate) fn delete_agent_definition(
    transaction: &Transaction<'_>,
    id: &str,
) -> Result<bool, StoreError> {
    let changed = transaction.execute("DELETE FROM agent_definition WHERE id = ?1", [id])?;
    Ok(changed > 0)
}

pub(crate) fn read_remote_session_snapshot(
    connection: &Connection,
) -> Result<Option<RemoteSessionCatalogSnapshot>, StoreError> {
    let acp = query_remote_acp(connection)?;
    let terminal = query_remote_terminal(connection)?;
    if acp.is_empty() && terminal.is_empty() {
        return Ok(None);
    }
    Ok(Some(RemoteSessionCatalogSnapshot { acp, terminal }))
}

pub(crate) fn write_remote_session_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &RemoteSessionCatalogSnapshot,
) -> Result<(), StoreError> {
    transaction.execute("DELETE FROM remote_acp_session", [])?;
    transaction.execute("DELETE FROM remote_terminal_session", [])?;
    for session in &snapshot.acp {
        transaction.execute(
            "INSERT INTO remote_acp_session(
                   id, cwd, title, agent_option_id, agent, launch_command, launch_env_json,
                   resume_id, created_at, lifecycle, hidden
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                session.id,
                session.cwd,
                session.title,
                session.agent_option_id,
                session.agent,
                session.launch_command,
                &session.launch_env_json,
                session.resume_id.as_deref(),
                session.created_at,
                session.lifecycle,
                i64::from(session.hidden),
            ],
        )?;
    }
    for session in &snapshot.terminal {
        transaction.execute(
            "INSERT INTO remote_terminal_session(id, cwd, title, created_at, lifecycle)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session.id,
                session.cwd,
                session.title,
                session.created_at,
                session.lifecycle,
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn read_published_session_snapshot(
    connection: &Connection,
) -> Result<Option<PublishedSessionSnapshot>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, cwd, launch, provider, conversation_id, title, phase, phase_since,
                    updated_at, structured_events, turn_events, agent_event_version, tokens_used,
                    branch, dirty_files_json
             FROM published_session ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(PublishedSessionRecord {
            id: row.get(0)?,
            cwd: row.get(1)?,
            launch: row.get(2)?,
            provider: row.get(3)?,
            conversation_id: row.get(4)?,
            title: row.get(5)?,
            phase: row.get(6)?,
            phase_since: row.get(7)?,
            updated_at: row.get(8)?,
            structured_events: row.get::<_, i64>(9)? != 0,
            turn_events: row.get::<_, i64>(10)? != 0,
            agent_event_version: row.get(11)?,
            tokens_used: row.get(12)?,
            branch: row.get(13)?,
            dirty_files_json: row.get(14)?,
        })
    })?;
    let sessions = rows.collect::<Result<Vec<_>, _>>()?;
    if sessions.is_empty() {
        return Ok(None);
    }
    Ok(Some(PublishedSessionSnapshot {
        version: 1,
        sessions,
    }))
}

pub(crate) fn write_published_session_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &PublishedSessionSnapshot,
) -> Result<(), StoreError> {
    transaction.execute("DELETE FROM published_session", [])?;
    for session in &snapshot.sessions {
        transaction.execute(
            "INSERT INTO published_session(
                   id, cwd, launch, provider, conversation_id, title, phase, phase_since,
                   updated_at, structured_events, turn_events, agent_event_version, tokens_used,
                   branch, dirty_files_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                session.id,
                session.cwd.as_deref(),
                session.launch.as_deref(),
                session.provider.as_deref(),
                session.conversation_id.as_deref(),
                session.title.as_deref(),
                session.phase,
                session.phase_since,
                session.updated_at,
                i64::from(session.structured_events),
                i64::from(session.turn_events),
                session.agent_event_version,
                session.tokens_used,
                session.branch.as_deref(),
                &session.dirty_files_json,
            ],
        )?;
    }
    Ok(())
}

fn agent_definitions_exist(connection: &Connection) -> Result<bool, StoreError> {
    connection
        .query_row("SELECT EXISTS(SELECT 1 FROM agent_definition)", [], |row| {
            row.get(0)
        })
        .map_err(StoreError::from)
}

fn query_commands(
    connection: &Connection,
) -> Result<Vec<AgentConversationCommandRecord>, StoreError> {
    let mut statement = connection
        .prepare("SELECT command_key, command FROM agent_acp_command ORDER BY command_key")?;
    let rows = statement.query_map([], |row| {
        Ok(AgentConversationCommandRecord {
            command_key: row.get(0)?,
            command: row.get(1)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn query_env(connection: &Connection) -> Result<Vec<AgentAcpEnvRecord>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT engine_kind_id, name, value
             FROM agent_acp_env ORDER BY engine_kind_id, name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(AgentAcpEnvRecord {
            engine_kind_id: row.get(0)?,
            name: row.get(1)?,
            value: row.get(2)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn query_config_memory(
    connection: &Connection,
) -> Result<Vec<AgentAcpConfigMemoryRecord>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT engine_kind_id, config_id, value_id FROM agent_acp_config_memory
             ORDER BY engine_kind_id, position",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(AgentAcpConfigMemoryRecord {
            engine_kind_id: row.get(0)?,
            config_id: row.get(1)?,
            value_id: row.get(2)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

const AGENT_DEFINITION_COLUMNS: &str =
    "id, name, description, engine_kind_id, prompt, plugins_json, mcp_servers_json,
     context_folders_json, context_links_json, model_provider, model_id, memory_enabled";

fn agent_definition_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentDefinitionRecord> {
    Ok(AgentDefinitionRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        engine_kind_id: row.get(3)?,
        prompt: row.get(4)?,
        plugins_json: row.get(5)?,
        mcp_servers_json: row.get(6)?,
        context_folders_json: row.get(7)?,
        context_links_json: row.get(8)?,
        model_provider: row.get(9)?,
        model_id: row.get(10)?,
        memory_enabled: row.get(11)?,
    })
}

fn query_agents(connection: &Connection) -> Result<Vec<AgentDefinitionRecord>, StoreError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {AGENT_DEFINITION_COLUMNS} FROM agent_definition ORDER BY position"
    ))?;
    let rows = statement.query_map([], agent_definition_from_row)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

pub(crate) fn query_agent_definition(
    connection: &Connection,
    id: &str,
) -> Result<Option<AgentDefinitionRecord>, StoreError> {
    connection
        .query_row(
            &format!("SELECT {AGENT_DEFINITION_COLUMNS} FROM agent_definition WHERE id = ?1"),
            [id],
            agent_definition_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

fn query_profiles(connection: &Connection) -> Result<Vec<AgentProfileRecord>, StoreError> {
    let mut statement = connection
        .prepare("SELECT id, kind_id, label, workspace_dir FROM agent_profile ORDER BY position")?;
    let rows = statement.query_map([], |row| {
        Ok(AgentProfileRecord {
            id: row.get(0)?,
            kind_id: row.get(1)?,
            label: row.get(2)?,
            workspace_dir: row.get(3)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn query_remote_acp(connection: &Connection) -> Result<Vec<RemoteAcpSessionRecord>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, cwd, title, agent_option_id, agent, launch_command, launch_env_json,
                    resume_id, created_at, lifecycle, hidden
             FROM remote_acp_session ORDER BY created_at, id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(RemoteAcpSessionRecord {
            id: row.get(0)?,
            cwd: row.get(1)?,
            title: row.get(2)?,
            agent_option_id: row.get(3)?,
            agent: row.get(4)?,
            launch_command: row.get(5)?,
            launch_env_json: row.get(6)?,
            resume_id: row.get(7)?,
            created_at: row.get(8)?,
            lifecycle: row.get(9)?,
            hidden: row.get::<_, i64>(10)? != 0,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn query_remote_terminal(
    connection: &Connection,
) -> Result<Vec<RemoteTerminalSessionRecord>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, cwd, title, created_at, lifecycle
             FROM remote_terminal_session ORDER BY created_at, id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(RemoteTerminalSessionRecord {
            id: row.get(0)?,
            cwd: row.get(1)?,
            title: row.get(2)?,
            created_at: row.get(3)?,
            lifecycle: row.get(4)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}
