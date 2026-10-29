//! v17 → v18：把旧 `kv` 里运行时自己的数据搬进按字段建的表，然后删掉 `kv`。
//!
//! 只服务这一次迁移。`kv` 有两种存法：二进制 blob（一个 key 一个值），以及把一份 JSON
//! 拆成节点的「作用域对象」（`$` 是根，子节点的 key 是以 `/` 连接的路径，`~0`/`~1` 转义）。
//! 这里把需要的作用域还原出来，按新表的字段写回；客户端的界面状态、已经没人读的作用域
//! 不搬，随 `kv` 一起删掉。单项解析不了就跳过这一项，不让整次升级失败。

use crate::{
    MemorySettingsRecord, QuotaCacheQuota, QuotaCacheSnapshot, QuotaCacheStatusRecord,
    QuotaCacheWindowRecord, RemoteConfigSnapshot, StoreError, TerminalThemeSnapshot,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Map, Number, Value};
use std::collections::BTreeMap;

/// 历史会话绑定 key 里 agent / profile / resume_id 之间的分隔符（旧 `history_agent` 作用域）。
const HISTORY_AGENT_KEY_SEP: char = '\u{1f}';

/// ACP 会话原来散在 `acp:<session_id>` 作用域里的字段。
#[derive(Default)]
struct AcpExtras {
    launch_env: Vec<(String, String)>,
    config_values: Vec<(String, String)>,
    binding_automation_run_id: Option<String>,
    pending_agent_preset: Option<String>,
    session_title: Option<String>,
}

/// 从 v17 库里读出来、要写进新表的全部数据。
#[derive(Default)]
pub(crate) struct V17RuntimeState {
    runtime_id: Option<String>,
    workspace_catalog_revision: Option<u64>,
    history_agent_bindings: Vec<(String, String, String, String)>,
    default_conversation_agent: Option<String>,
    new_session_pins: Vec<String>,
    memory_settings: Option<MemorySettingsRecord>,
    remote_config: Option<RemoteConfigSnapshot>,
    terminal_theme: Option<TerminalThemeSnapshot>,
    pi_auto_models: Vec<String>,
    quota_cache: Option<QuotaCacheSnapshot>,
    launch_version: Option<u32>,
    agent_host_pref: Option<(bool, bool)>,
    acp: Vec<(String, AcpExtras)>,
}

/// 在改结构之前读出旧数据。
pub(crate) fn read_v17_state(connection: &Connection) -> Result<V17RuntimeState, StoreError> {
    let mut state = V17RuntimeState {
        runtime_id: blob_text(connection, "runtime", "runtime_id")?
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty()),
        workspace_catalog_revision: blob_text(connection, "workspace_catalog", "revision")?
            .and_then(|raw| raw.trim().parse().ok()),
        default_conversation_agent: blob_json(
            connection,
            "preferences",
            "default_conversation_agent",
        )?
        .and_then(|value| as_string(value.get("agent_id")))
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty()),
        new_session_pins: blob_json(connection, "preferences", "new_session_pins")?
            .map(|value| string_list(value.get("pinned")))
            .unwrap_or_default(),
        ..V17RuntimeState::default()
    };

    for (key, raw) in blobs(connection, "history_agent")? {
        let mut parts = key.split(HISTORY_AGENT_KEY_SEP);
        let (Some(agent), Some(profile), Some(resume), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Ok(id) = String::from_utf8(raw) else {
            continue;
        };
        let id = id.trim();
        if agent.is_empty() || resume.is_empty() || id.is_empty() {
            continue;
        }
        state.history_agent_bindings.push((
            agent.to_string(),
            profile.to_string(),
            resume.to_string(),
            id.to_string(),
        ));
    }

    let memory = blobs(connection, "memory")?;
    if !memory.is_empty() {
        let field = |key: &str| {
            memory
                .iter()
                .find(|(stored, _)| stored == key)
                .map(|(_, raw)| String::from_utf8_lossy(raw).trim().to_string())
                .unwrap_or_default()
        };
        state.memory_settings = Some(MemorySettingsRecord {
            embedding_base_url: field("embedding_base_url"),
            embedding_model: field("embedding_model"),
            embedding_api_key: field("embedding_api_key"),
            rerank_model_provider: field("rerank_model_provider"),
            rerank_model_id: field("rerank_model_id"),
        });
    }

    state.remote_config = scoped_object(connection, "collab")?.map(|object| RemoteConfigSnapshot {
        enabled: bool_field(&object, "enabled", false),
        iroh_relay: as_string(object.get("iroh_relay")).unwrap_or_default(),
    });
    state.terminal_theme = scoped_object(connection, "terminal_theme")?.map(|object| {
        let color = |snake: &str, camel: &str| {
            u32_field(object.get(snake).or_else(|| object.get(camel))).unwrap_or(0)
        };
        TerminalThemeSnapshot {
            version: u32_field(object.get("version")).unwrap_or(1),
            dark: bool_field(&object, "dark", true),
            background: color("background", "background"),
            foreground: color("foreground", "foreground"),
            cursor: color("cursor", "cursor"),
            selection: color("selection", "selection"),
            palette: object
                .get("palette")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|value| u32_field(Some(value)))
                .collect(),
            search_hit: color("search_hit", "searchHit"),
            search_hit_current: color("search_hit_current", "searchHitCurrent"),
        }
    });
    if let Some(object) = scoped_object(connection, "pi_auto_models")? {
        state.pi_auto_models = string_list(object.get("providers"));
    }
    state.quota_cache =
        scoped_object(connection, "quota_cache")?.map(|object| quota_cache(&object));
    state.launch_version = read_scope_nodes(connection, "launch")
        .ok()
        .and_then(|nodes| decode_tree(&nodes, "version").ok().flatten())
        .and_then(|value| u32_field(Some(&value)));

    let prefs: Vec<(String, String)> = connection
        .prepare("SELECT key, value FROM agent_ui_pref")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    if !prefs.is_empty() {
        let flag = |key: &str| {
            prefs
                .iter()
                .find(|(stored, _)| stored == key)
                .is_none_or(|(_, value)| value == "1" || value == "true")
        };
        state.agent_host_pref = Some((flag("agent_hooks_enabled"), flag("cross_agent_enabled")));
    }

    let acp_scopes: Vec<String> = connection
        .prepare("SELECT DISTINCT scope FROM kv WHERE scope LIKE 'acp:%' ORDER BY scope")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    for scope in acp_scopes {
        let Some(session_id) = scope.strip_prefix("acp:") else {
            continue;
        };
        let Ok(nodes) = read_scope_nodes(connection, &scope) else {
            continue;
        };
        let field = |root: &str| decode_tree(&nodes, root).ok().flatten();
        let mut extras = AcpExtras::default();
        if let Some(Value::Object(env)) = field("env") {
            extras.launch_env = env
                .into_iter()
                .filter_map(|(name, value)| Some((name, value.as_str()?.to_string())))
                .collect();
            extras.launch_env.sort();
        }
        if let Some(Value::Array(values)) = field("config_values") {
            extras.config_values = values
                .iter()
                .filter_map(|pair| {
                    Some((
                        pair.get(0)?.as_str()?.to_string(),
                        pair.get(1)?.as_str()?.to_string(),
                    ))
                })
                .collect();
        }
        if let Some(binding) = field("conversation_binding")
            && binding.get("type").and_then(Value::as_str) == Some("automation")
        {
            extras.binding_automation_run_id = as_string(binding.get("run_id"));
        }
        extras.pending_agent_preset =
            field("pending_agent_preset").and_then(|v| as_string(Some(&v)));
        extras.session_title = field("session_title").and_then(|v| as_string(Some(&v)));
        state.acp.push((session_id.to_string(), extras));
    }
    Ok(state)
}

/// 新表建好之后写回。
pub(crate) fn write_v18_state(
    transaction: &Transaction<'_>,
    state: V17RuntimeState,
) -> Result<(), StoreError> {
    if let Some(id) = &state.runtime_id {
        crate::config::write_runtime_id(transaction, id)?;
    }
    if let Some(revision) = state.workspace_catalog_revision {
        // v18 的目录表还没有 epoch 列（v29 才加），按那时的形状写。
        transaction.execute(
            "INSERT INTO workspace_catalog(id, revision) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET revision = excluded.revision",
            [i64::try_from(revision).unwrap_or(i64::MAX)],
        )?;
    }
    for (agent, profile, resume, id) in &state.history_agent_bindings {
        crate::history::write_history_agent_binding(transaction, agent, profile, resume, id)?;
    }
    if let Some(id) = &state.default_conversation_agent {
        crate::config::write_default_conversation_agent(transaction, Some(id))?;
    }
    crate::config::write_new_session_pins(transaction, &state.new_session_pins)?;
    if let Some(memory) = &state.memory_settings {
        crate::config::write_memory_settings(transaction, memory)?;
    }
    if let Some(remote) = &state.remote_config {
        crate::config::write_remote_config_snapshot(transaction, remote)?;
    }
    if let Some(theme) = &state.terminal_theme {
        crate::config::write_terminal_theme_snapshot(transaction, theme)?;
    }
    // v18 的表还带 `harness` 列（v24 去掉），不能用按当前形状写的 `config` 函数。
    for provider in &state.pi_auto_models {
        transaction.execute(
            "INSERT OR IGNORE INTO auto_model_provider(harness, provider) VALUES ('pi', ?1)",
            [provider],
        )?;
    }
    if let Some(quota) = &state.quota_cache {
        crate::config::write_quota_cache_snapshot(transaction, quota)?;
    }
    if let Some(version) = state.launch_version {
        transaction.execute(
            "INSERT INTO launch_settings(id, version) VALUES (1, ?1)",
            [version],
        )?;
    }
    if let Some((hooks_enabled, cross_agent_enabled)) = state.agent_host_pref {
        transaction.execute(
            "INSERT INTO agent_host_pref(id, hooks_enabled, cross_agent_enabled) VALUES (1, ?1, ?2)",
            params![hooks_enabled, cross_agent_enabled],
        )?;
    }
    for (session_id, extras) in &state.acp {
        // 会话已经不在目录里的残留作用域直接丢掉。
        let updated = transaction.execute(
            "UPDATE acp_detail
             SET binding_automation_run_id = ?1, pending_agent_preset = ?2, session_title = ?3
             WHERE session_id = ?4",
            params![
                extras.binding_automation_run_id,
                extras.pending_agent_preset,
                extras.session_title,
                session_id,
            ],
        )?;
        if updated == 0 {
            continue;
        }
        for (name, value) in &extras.launch_env {
            transaction.execute(
                "INSERT OR REPLACE INTO acp_launch_env(session_id, name, value) VALUES (?1, ?2, ?3)",
                params![session_id, name, value],
            )?;
        }
        for (position, (config_id, value_id)) in extras.config_values.iter().enumerate() {
            transaction.execute(
                "INSERT INTO acp_config_value(session_id, position, config_id, value_id)
                 VALUES (?1, ?2, ?3, ?4)",
                params![session_id, position as i64, config_id, value_id],
            )?;
        }
    }
    Ok(())
}

fn quota_cache(object: &Map<String, Value>) -> QuotaCacheSnapshot {
    let mut statuses = Vec::new();
    for (provider_id, cached) in object
        .get("providers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let (kind, status) = match (cached.get("windows"), cached.get("copilot")) {
            (Some(status), _) => ("windows", status),
            (None, Some(status)) => ("copilot", status),
            (None, None) => continue,
        };
        let Some(quota) = status.get("quota").filter(|quota| quota.is_object()) else {
            continue;
        };
        let quota = if kind == "windows" {
            QuotaCacheQuota::Windows(
                quota
                    .get("windows")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|window| QuotaCacheWindowRecord {
                        label: as_string(window.get("label")).unwrap_or_default(),
                        used_percent: window
                            .get("used_percent")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0),
                        resets_at_ms: window.get("resets_at_ms").and_then(Value::as_i64),
                        reset_after_seconds: window
                            .get("reset_after_seconds")
                            .and_then(Value::as_i64),
                    })
                    .collect(),
            )
        } else {
            let unit = match quota.get("unit").and_then(Value::as_str) {
                Some("AiCredits") => "ai_credits",
                Some("PremiumRequests") => "premium_requests",
                _ => continue,
            };
            QuotaCacheQuota::Copilot {
                unit: unit.to_string(),
                remaining: quota
                    .get("remaining")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
                limit: quota.get("limit").and_then(Value::as_f64),
                used: quota.get("used").and_then(Value::as_f64),
            }
        };
        statuses.push(QuotaCacheStatusRecord {
            provider_id: provider_id.clone(),
            checked_at_ms: status
                .get("checked_at_ms")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            error: as_string(status.get("error")),
            quota,
        });
    }
    QuotaCacheSnapshot {
        saved_at_ms: object
            .get("saved_at_ms")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        schema_version: u32_field(object.get("schema_version")).unwrap_or(0),
        provider_ids: string_list(object.get("provider_ids")),
        statuses,
    }
}

fn blobs(connection: &Connection, scope: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT key, value FROM kv WHERE scope = ?1 AND value_type = 'blob' ORDER BY key",
    )?;
    let rows = statement.query_map([scope], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn blob_text(
    connection: &Connection,
    scope: &str,
    key: &str,
) -> Result<Option<String>, StoreError> {
    let raw: Option<Vec<u8>> = connection
        .query_row(
            "SELECT value FROM kv WHERE scope = ?1 AND key = ?2 AND value_type = 'blob'",
            params![scope, key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw.and_then(|raw| String::from_utf8(raw).ok()))
}

fn blob_json(connection: &Connection, scope: &str, key: &str) -> Result<Option<Value>, StoreError> {
    Ok(blob_text(connection, scope, key)?.and_then(|raw| serde_json::from_str(&raw).ok()))
}

struct KvNode {
    value_type: String,
    value: Option<String>,
}

fn read_scope_nodes(
    connection: &Connection,
    scope: &str,
) -> Result<BTreeMap<String, KvNode>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT key, value_type,
                CASE WHEN value_type = 'blob' THEN NULL ELSE CAST(value AS TEXT) END
         FROM kv WHERE scope = ?1 ORDER BY key",
    )?;
    let rows = statement.query_map([scope], |row| {
        Ok((
            row.get::<_, String>(0)?,
            KvNode {
                value_type: row.get(1)?,
                value: row.get(2)?,
            },
        ))
    })?;
    rows.collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(StoreError::from)
}

/// 还原一个作用域对象（根节点 `$`）。作用域不在或根不是对象时为 `None`。
fn scoped_object(
    connection: &Connection,
    scope: &str,
) -> Result<Option<Map<String, Value>>, StoreError> {
    let nodes = read_scope_nodes(connection, scope)?;
    if nodes.get("$").map(|node| node.value_type.as_str()) != Some("object") {
        return Ok(None);
    }
    let mut object = Map::new();
    for key in nodes
        .keys()
        .filter(|key| key.as_str() != "$" && !key.contains('/'))
    {
        if let Ok(Some(value)) = decode_tree(&nodes, key) {
            object.insert(unescape_segment(key), value);
        }
    }
    Ok(Some(object))
}

fn decode_tree(nodes: &BTreeMap<String, KvNode>, path: &str) -> Result<Option<Value>, ()> {
    let Some(node) = nodes.get(path) else {
        return Ok(None);
    };
    let value = match node.value_type.as_str() {
        "null" => Value::Null,
        "boolean" => Value::Bool(node.value.as_deref() == Some("1")),
        "integer" | "real" => Value::Number(
            node.value
                .as_deref()
                .unwrap_or_default()
                .parse::<Number>()
                .map_err(|_| ())?,
        ),
        "text" => Value::String(node.value.clone().unwrap_or_default()),
        "object" => {
            let mut object = Map::new();
            for segment in direct_child_segments(nodes, path) {
                let child = format!("{path}/{segment}");
                if let Some(value) = decode_tree(nodes, &child)? {
                    object.insert(unescape_segment(&segment), value);
                }
            }
            Value::Object(object)
        }
        "array" => {
            let mut indexed = direct_child_segments(nodes, path)
                .into_iter()
                .filter_map(|segment| segment.parse::<usize>().ok().map(|index| (index, segment)))
                .collect::<Vec<_>>();
            indexed.sort_by_key(|(index, _)| *index);
            let mut values = Vec::new();
            for (index, segment) in indexed {
                while values.len() < index {
                    values.push(Value::Null);
                }
                values
                    .push(decode_tree(nodes, &format!("{path}/{segment}"))?.unwrap_or(Value::Null));
            }
            Value::Array(values)
        }
        _ => return Err(()),
    };
    Ok(Some(value))
}

fn direct_child_segments(nodes: &BTreeMap<String, KvNode>, path: &str) -> Vec<String> {
    let prefix = format!("{path}/");
    let mut segments = nodes
        .keys()
        .filter_map(|key| {
            let rest = key.strip_prefix(&prefix)?;
            (!rest.is_empty() && !rest.contains('/')).then(|| rest.to_string())
        })
        .collect::<Vec<_>>();
    segments.sort();
    segments.dedup();
    segments
}

fn unescape_segment(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

fn as_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_string)
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn bool_field(object: &Map<String, Value>, key: &str, default: bool) -> bool {
    object.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn u32_field(value: Option<&Value>) -> Option<u32> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}
