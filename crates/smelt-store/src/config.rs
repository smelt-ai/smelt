//! 运行时的设置与缓存：每项一张单行表（`id = 1`），列表另放子表，按 `position` 保序。

use crate::{
    MemorySettingsRecord, PublishedWorkspaceMenuSnapshot, QuotaCacheQuota,
    QuotaCacheSnapshot, QuotaCacheStatusRecord, QuotaCacheWindowRecord, RemoteConfigSnapshot,
    StoreError, TerminalThemeSnapshot, WorkspaceMenuProjectRecord,
    WorkspaceMenuSessionRecord,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

pub(crate) fn read_runtime_id(connection: &Connection) -> Result<Option<String>, StoreError> {
    connection
        .query_row(
            "SELECT runtime_id FROM runtime_identity WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::from)
}

pub(crate) fn write_runtime_id(
    transaction: &Transaction<'_>,
    runtime_id: &str,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO runtime_identity(id, runtime_id) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET runtime_id = excluded.runtime_id",
        [runtime_id],
    )?;
    Ok(())
}

pub(crate) fn read_workspace_catalog_revision(connection: &Connection) -> Result<u64, StoreError> {
    let revision: Option<i64> = connection
        .query_row(
            "SELECT revision FROM workspace_catalog WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(revision.unwrap_or(0).max(0) as u64)
}

pub(crate) fn write_workspace_catalog_revision(
    transaction: &Transaction<'_>,
    revision: u64,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO workspace_catalog(id, revision) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET revision = excluded.revision",
        [i64::try_from(revision).unwrap_or(i64::MAX)],
    )?;
    Ok(())
}

/// 目录有一条改动提交后推进版本，返回新版本。
pub(crate) fn bump_workspace_catalog_revision(
    transaction: &Transaction<'_>,
) -> Result<u64, StoreError> {
    let revision = read_workspace_catalog_revision(transaction)? + 1;
    write_workspace_catalog_revision(transaction, revision)?;
    Ok(revision)
}

pub(crate) fn read_default_conversation_agent(
    connection: &Connection,
) -> Result<Option<String>, StoreError> {
    connection
        .query_row(
            "SELECT agent_definition_id FROM default_conversation_agent WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::from)
}

pub(crate) fn write_default_conversation_agent(
    transaction: &Transaction<'_>,
    agent_definition_id: Option<&str>,
) -> Result<(), StoreError> {
    match agent_definition_id {
        Some(id) => transaction.execute(
            "INSERT INTO default_conversation_agent(id, agent_definition_id) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET agent_definition_id = excluded.agent_definition_id",
            [id],
        )?,
        None => transaction.execute("DELETE FROM default_conversation_agent", [])?,
    };
    Ok(())
}

pub(crate) fn read_new_session_pins(connection: &Connection) -> Result<Vec<String>, StoreError> {
    let mut statement =
        connection.prepare("SELECT action_key FROM new_session_pin ORDER BY position")?;
    let rows = statement.query_map([], |row| row.get(0))?;
    rows.collect::<Result<Vec<String>, _>>()
        .map_err(StoreError::from)
}

pub(crate) fn write_new_session_pins(
    transaction: &Transaction<'_>,
    pins: &[String],
) -> Result<(), StoreError> {
    transaction.execute("DELETE FROM new_session_pin", [])?;
    let mut position = 0i64;
    for pin in pins {
        // 同一个动作只记一次：重复的 key 撞 UNIQUE 就跳过，位置按第一次出现算。
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO new_session_pin(position, action_key) VALUES (?1, ?2)",
            params![position, pin],
        )?;
        position += inserted as i64;
    }
    Ok(())
}

pub(crate) fn read_memory_settings(
    connection: &Connection,
) -> Result<MemorySettingsRecord, StoreError> {
    let record = connection
        .query_row(
            "SELECT embedding_base_url, embedding_model, embedding_api_key,
                    rerank_model_provider, rerank_model_id
             FROM memory_settings WHERE id = 1",
            [],
            |row| {
                Ok(MemorySettingsRecord {
                    embedding_base_url: row.get(0)?,
                    embedding_model: row.get(1)?,
                    embedding_api_key: row.get(2)?,
                    rerank_model_provider: row.get(3)?,
                    rerank_model_id: row.get(4)?,
                })
            },
        )
        .optional()?;
    Ok(record.unwrap_or_default())
}

pub(crate) fn write_memory_settings(
    transaction: &Transaction<'_>,
    record: &MemorySettingsRecord,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO memory_settings(
           id, embedding_base_url, embedding_model, embedding_api_key,
           rerank_model_provider, rerank_model_id
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET
           embedding_base_url = excluded.embedding_base_url,
           embedding_model = excluded.embedding_model,
           embedding_api_key = excluded.embedding_api_key,
           rerank_model_provider = excluded.rerank_model_provider,
           rerank_model_id = excluded.rerank_model_id",
        params![
            record.embedding_base_url,
            record.embedding_model,
            record.embedding_api_key,
            record.rerank_model_provider,
            record.rerank_model_id,
        ],
    )?;
    Ok(())
}

pub(crate) fn read_remote_config_snapshot(
    connection: &Connection,
) -> Result<Option<RemoteConfigSnapshot>, StoreError> {
    connection
        .query_row(
            "SELECT enabled, iroh_relay FROM remote_config WHERE id = 1",
            [],
            |row| {
                Ok(RemoteConfigSnapshot {
                    enabled: row.get(0)?,
                    iroh_relay: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::from)
}

pub(crate) fn write_remote_config_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &RemoteConfigSnapshot,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO remote_config(id, enabled, iroh_relay) VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET
           enabled = excluded.enabled,
           iroh_relay = excluded.iroh_relay",
        params![snapshot.enabled, snapshot.iroh_relay],
    )?;
    Ok(())
}

pub(crate) fn read_terminal_theme_snapshot(
    connection: &Connection,
) -> Result<Option<TerminalThemeSnapshot>, StoreError> {
    let theme = connection
        .query_row(
            "SELECT version, dark, background, foreground, cursor, selection,
                    search_hit, search_hit_current
             FROM terminal_theme WHERE id = 1",
            [],
            |row| {
                Ok(TerminalThemeSnapshot {
                    version: row.get(0)?,
                    dark: row.get(1)?,
                    background: row.get(2)?,
                    foreground: row.get(3)?,
                    cursor: row.get(4)?,
                    selection: row.get(5)?,
                    palette: Vec::new(),
                    search_hit: row.get(6)?,
                    search_hit_current: row.get(7)?,
                })
            },
        )
        .optional()?;
    let Some(mut theme) = theme else {
        return Ok(None);
    };
    let mut statement =
        connection.prepare("SELECT color FROM terminal_theme_palette ORDER BY position")?;
    theme.palette = statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<u32>, _>>()?;
    Ok(Some(theme))
}

pub(crate) fn write_terminal_theme_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &TerminalThemeSnapshot,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO terminal_theme(
           id, version, dark, background, foreground, cursor, selection,
           search_hit, search_hit_current
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
           version = excluded.version,
           dark = excluded.dark,
           background = excluded.background,
           foreground = excluded.foreground,
           cursor = excluded.cursor,
           selection = excluded.selection,
           search_hit = excluded.search_hit,
           search_hit_current = excluded.search_hit_current",
        params![
            snapshot.version,
            snapshot.dark,
            snapshot.background,
            snapshot.foreground,
            snapshot.cursor,
            snapshot.selection,
            snapshot.search_hit,
            snapshot.search_hit_current,
        ],
    )?;
    transaction.execute("DELETE FROM terminal_theme_palette", [])?;
    for (position, color) in snapshot.palette.iter().enumerate() {
        transaction.execute(
            "INSERT INTO terminal_theme_palette(position, color) VALUES (?1, ?2)",
            params![position as i64, color],
        )?;
    }
    Ok(())
}

pub(crate) fn read_auto_model_providers(
    connection: &Connection,
) -> Result<Vec<String>, StoreError> {
    let mut statement =
        connection.prepare("SELECT provider FROM auto_model_provider ORDER BY provider")?;
    let rows = statement.query_map([], |row| row.get(0))?;
    rows.collect::<Result<Vec<String>, _>>()
        .map_err(StoreError::from)
}

pub(crate) fn write_auto_model_providers(
    transaction: &Transaction<'_>,
    providers: &[String],
) -> Result<(), StoreError> {
    transaction.execute("DELETE FROM auto_model_provider", [])?;
    for provider in providers {
        transaction.execute(
            "INSERT OR IGNORE INTO auto_model_provider(provider) VALUES (?1)",
            [provider],
        )?;
    }
    Ok(())
}

pub(crate) fn read_quota_cache_snapshot(
    connection: &Connection,
) -> Result<Option<QuotaCacheSnapshot>, StoreError> {
    let head: Option<(i64, u32)> = connection
        .query_row(
            "SELECT saved_at_ms, schema_version FROM quota_cache WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((saved_at_ms, schema_version)) = head else {
        return Ok(None);
    };
    let provider_ids = connection
        .prepare("SELECT provider_id FROM quota_cache_provider ORDER BY provider_id")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    let mut window_statement = connection.prepare(
        "SELECT label, used_percent, resets_at_ms, reset_after_seconds
         FROM quota_cache_window WHERE provider_id = ?1 ORDER BY position",
    )?;
    let mut status_statement = connection.prepare(
        "SELECT provider_id, kind, checked_at_ms, error,
                copilot_unit, copilot_remaining, copilot_limit, copilot_used
         FROM quota_cache_status ORDER BY provider_id",
    )?;
    let rows = status_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<f64>>(5)?,
                row.get::<_, Option<f64>>(6)?,
                row.get::<_, Option<f64>>(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut statuses = Vec::new();
    for (provider_id, kind, checked_at_ms, error, unit, remaining, limit, used) in rows {
        let quota = if kind == "copilot" {
            QuotaCacheQuota::Copilot {
                unit: unit.unwrap_or_default(),
                remaining: remaining.unwrap_or_default(),
                limit,
                used,
            }
        } else {
            QuotaCacheQuota::Windows(
                window_statement
                    .query_map([&provider_id], |row| {
                        Ok(QuotaCacheWindowRecord {
                            label: row.get(0)?,
                            used_percent: row.get(1)?,
                            resets_at_ms: row.get(2)?,
                            reset_after_seconds: row.get(3)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?,
            )
        };
        statuses.push(QuotaCacheStatusRecord {
            provider_id,
            checked_at_ms,
            error,
            quota,
        });
    }
    Ok(Some(QuotaCacheSnapshot {
        saved_at_ms,
        schema_version,
        provider_ids,
        statuses,
    }))
}

pub(crate) fn write_quota_cache_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &QuotaCacheSnapshot,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO quota_cache(id, saved_at_ms, schema_version) VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET
           saved_at_ms = excluded.saved_at_ms,
           schema_version = excluded.schema_version",
        params![snapshot.saved_at_ms, snapshot.schema_version],
    )?;
    transaction.execute("DELETE FROM quota_cache_provider", [])?;
    for provider_id in &snapshot.provider_ids {
        transaction.execute(
            "INSERT OR IGNORE INTO quota_cache_provider(provider_id) VALUES (?1)",
            [provider_id],
        )?;
    }
    // 窗口随状态行级联删除。
    transaction.execute("DELETE FROM quota_cache_status", [])?;
    for status in &snapshot.statuses {
        match &status.quota {
            QuotaCacheQuota::Windows(windows) => {
                transaction.execute(
                    "INSERT INTO quota_cache_status(provider_id, kind, checked_at_ms, error)
                     VALUES (?1, 'windows', ?2, ?3)",
                    params![status.provider_id, status.checked_at_ms, status.error],
                )?;
                for (position, window) in windows.iter().enumerate() {
                    transaction.execute(
                        "INSERT INTO quota_cache_window(
                           provider_id, position, label, used_percent, resets_at_ms,
                           reset_after_seconds
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            status.provider_id,
                            position as i64,
                            window.label,
                            window.used_percent,
                            window.resets_at_ms,
                            window.reset_after_seconds,
                        ],
                    )?;
                }
            }
            QuotaCacheQuota::Copilot {
                unit,
                remaining,
                limit,
                used,
            } => {
                transaction.execute(
                    "INSERT INTO quota_cache_status(
                       provider_id, kind, checked_at_ms, error,
                       copilot_unit, copilot_remaining, copilot_limit, copilot_used
                     ) VALUES (?1, 'copilot', ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        status.provider_id,
                        status.checked_at_ms,
                        status.error,
                        unit,
                        remaining,
                        limit,
                        used,
                    ],
                )?;
            }
        }
    }
    Ok(())
}

pub(crate) fn read_workspace_menu_snapshot(
    connection: &Connection,
) -> Result<Option<PublishedWorkspaceMenuSnapshot>, StoreError> {
    let head: Option<(u32, i64, String, i64)> = connection
        .query_row(
            "SELECT version, revision, source_id, source_revision FROM workspace_menu WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((version, revision, source_id, source_revision)) = head else {
        return Ok(None);
    };
    let projects = connection
        .prepare("SELECT root, title, display_order FROM workspace_menu_project ORDER BY position")?
        .query_map([], |row| {
            Ok(WorkspaceMenuProjectRecord {
                root: row.get(0)?,
                title: row.get(1)?,
                order: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let sessions = connection
        .prepare(
            "SELECT id, kind, title, custom_title, cwd, project_root, project_title,
                    project_order, session_order, leaf_order, agent
             FROM workspace_menu_session ORDER BY position",
        )?
        .query_map([], |row| {
            Ok(WorkspaceMenuSessionRecord {
                id: row.get(0)?,
                kind: row.get(1)?,
                title: row.get(2)?,
                custom_title: row.get(3)?,
                cwd: row.get(4)?,
                project_root: row.get(5)?,
                project_title: row.get(6)?,
                project_order: row.get(7)?,
                session_order: row.get(8)?,
                leaf_order: row.get(9)?,
                agent: row.get(10)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(PublishedWorkspaceMenuSnapshot {
        version,
        revision: revision.max(0) as u64,
        source_id,
        source_revision: source_revision.max(0) as u64,
        projects,
        sessions,
    }))
}

pub(crate) fn write_workspace_menu_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &PublishedWorkspaceMenuSnapshot,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO workspace_menu(id, version, revision, source_id, source_revision)
         VALUES (1, ?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET
           version = excluded.version,
           revision = excluded.revision,
           source_id = excluded.source_id,
           source_revision = excluded.source_revision",
        params![
            snapshot.version,
            i64::try_from(snapshot.revision).unwrap_or(i64::MAX),
            snapshot.source_id,
            i64::try_from(snapshot.source_revision).unwrap_or(i64::MAX),
        ],
    )?;
    transaction.execute("DELETE FROM workspace_menu_project", [])?;
    for (position, project) in snapshot.projects.iter().enumerate() {
        transaction.execute(
            "INSERT INTO workspace_menu_project(position, root, title, display_order)
             VALUES (?1, ?2, ?3, ?4)",
            params![position as i64, project.root, project.title, project.order],
        )?;
    }
    transaction.execute("DELETE FROM workspace_menu_session", [])?;
    for (position, session) in snapshot.sessions.iter().enumerate() {
        transaction.execute(
            "INSERT INTO workspace_menu_session(
               position, id, kind, title, custom_title, cwd, project_root, project_title,
               project_order, session_order, leaf_order, agent
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                position as i64,
                session.id,
                session.kind,
                session.title,
                session.custom_title,
                session.cwd,
                session.project_root,
                session.project_title,
                session.project_order,
                session.session_order,
                session.leaf_order,
                session.agent,
            ],
        )?;
    }
    Ok(())
}
