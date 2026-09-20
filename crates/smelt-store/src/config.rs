//! 配置/缓存的类型化 kv 作用域。不建新表；活路径不经 `document:*.json`。

use crate::{
    AutoModelSnapshot, PublishedWorkspaceMenuSnapshot, QuotaCacheSnapshot, RemoteConfigSnapshot,
    StoreError, TerminalThemeSnapshot, UpdateSettingsSnapshot, UpdateStateSnapshot,
    WorktreeInheritSnapshot,
};
use rusqlite::{Connection, Transaction};
use serde_json::{Map, Value, json};

const COLLAB_SCOPE: &str = "collab";
const COLLAB_DOCUMENT: &str = "collab.json";
const UPDATE_SETTINGS_SCOPE: &str = "update_settings";
const UPDATE_SETTINGS_DOCUMENT: &str = "update-settings.json";
const UPDATE_STATE_SCOPE: &str = "update_state";
const UPDATE_STATE_DOCUMENT: &str = "update-state.json";
const WORKTREE_SCOPE: &str = "worktree_inherit";
const WORKTREE_DOCUMENT: &str = "worktree-inherit.json";
const TERMINAL_THEME_SCOPE: &str = "terminal_theme";
const TERMINAL_THEME_DOCUMENT: &str = "terminal-theme.json";
const QUOTA_CACHE_SCOPE: &str = "quota_cache";
const QUOTA_CACHE_DOCUMENT: &str = "quota-cache.json";
const DSH_AUTO_SCOPE: &str = "dsh_auto_models";
const DSH_AUTO_DOCUMENT: &str = "dsh-auto-models.json";
const PI_AUTO_SCOPE: &str = "pi_auto_models";
const PI_AUTO_DOCUMENT: &str = "pi-auto-models.json";
const WORKSPACE_MENU_SCOPE: &str = "workspace_menu";
const WORKSPACE_MENU_DOCUMENT: &str = "workspace_menu.json";

fn read_object(
    connection: &Connection,
    scope: &str,
    leftover: &str,
) -> Result<Option<Map<String, Value>>, StoreError> {
    if let Some(object) = crate::documents::read_scoped_object(connection, scope)? {
        return Ok(Some(object));
    }
    let Some(raw) = crate::documents::read_generic_document_bytes(connection, leftover)? else {
        return Ok(None);
    };
    let Ok(value) = serde_json::from_slice::<Value>(&raw) else {
        return Ok(None);
    };
    Ok(value.as_object().cloned())
}

fn write_object(
    transaction: &Transaction<'_>,
    scope: &str,
    leftover: &str,
    object: &Map<String, Value>,
) -> Result<(), StoreError> {
    crate::documents::delete_scope(transaction, scope)?;
    crate::documents::delete_document_scope(transaction, leftover)?;
    crate::documents::write_scoped_object(transaction, scope, object)
}

fn bool_field(object: &Map<String, Value>, key: &str, default: bool) -> bool {
    object.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn string_field(object: &Map<String, Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn u32_field(object: &Map<String, Value>, key: &str, default: u32) -> u32 {
    object
        .get(key)
        .and_then(Value::as_u64)
        .map(|value| value as u32)
        .unwrap_or(default)
}

fn u32_aliased(object: &Map<String, Value>, snake: &str, camel: &str) -> u32 {
    if object.contains_key(snake) {
        u32_field(object, snake, 0)
    } else {
        u32_field(object, camel, 0)
    }
}

fn i64_field(object: &Map<String, Value>, key: &str, default: i64) -> i64 {
    object
        .get(key)
        .and_then(Value::as_i64)
        .or_else(|| {
            object
                .get(key)
                .and_then(Value::as_u64)
                .map(|value| value as i64)
        })
        .unwrap_or(default)
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

fn json_bytes(value: Option<&Value>, fallback: Value) -> Vec<u8> {
    serde_json::to_vec(value.unwrap_or(&fallback)).unwrap_or_else(|_| b"null".to_vec())
}

pub(crate) fn read_remote_config_snapshot(
    connection: &Connection,
) -> Result<Option<RemoteConfigSnapshot>, StoreError> {
    let Some(object) = read_object(connection, COLLAB_SCOPE, COLLAB_DOCUMENT)? else {
        return Ok(None);
    };
    Ok(Some(RemoteConfigSnapshot {
        enabled: bool_field(&object, "enabled", false),
        iroh_relay: string_field(&object, "iroh_relay"),
        write_enabled: bool_field(&object, "write_enabled", false),
    }))
}

pub(crate) fn write_remote_config_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &RemoteConfigSnapshot,
) -> Result<(), StoreError> {
    write_object(
        transaction,
        COLLAB_SCOPE,
        COLLAB_DOCUMENT,
        &json!({
            "enabled": snapshot.enabled,
            "iroh_relay": snapshot.iroh_relay,
            "write_enabled": snapshot.write_enabled,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    )
}

pub(crate) fn json_from_remote_config(snapshot: &RemoteConfigSnapshot) -> Value {
    json!({
        "enabled": snapshot.enabled,
        "iroh_relay": snapshot.iroh_relay,
        "write_enabled": snapshot.write_enabled,
    })
}

pub(crate) fn remote_config_from_document(
    value: &Value,
) -> Result<RemoteConfigSnapshot, StoreError> {
    let object = value.as_object().cloned().unwrap_or_default();
    Ok(RemoteConfigSnapshot {
        enabled: bool_field(&object, "enabled", false),
        iroh_relay: string_field(&object, "iroh_relay"),
        write_enabled: bool_field(&object, "write_enabled", false),
    })
}

pub(crate) fn read_update_settings_snapshot(
    connection: &Connection,
) -> Result<Option<UpdateSettingsSnapshot>, StoreError> {
    let Some(object) = read_object(connection, UPDATE_SETTINGS_SCOPE, UPDATE_SETTINGS_DOCUMENT)?
    else {
        return Ok(None);
    };
    Ok(Some(UpdateSettingsSnapshot {
        channel: string_field(&object, "channel"),
        auto_install: bool_field(&object, "auto_install", true),
    }))
}

pub(crate) fn write_update_settings_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &UpdateSettingsSnapshot,
) -> Result<(), StoreError> {
    write_object(
        transaction,
        UPDATE_SETTINGS_SCOPE,
        UPDATE_SETTINGS_DOCUMENT,
        &json!({
            "channel": snapshot.channel,
            "auto_install": snapshot.auto_install,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    )
}

pub(crate) fn json_from_update_settings(snapshot: &UpdateSettingsSnapshot) -> Value {
    json!({
        "channel": snapshot.channel,
        "auto_install": snapshot.auto_install,
    })
}

pub(crate) fn update_settings_from_document(
    value: &Value,
) -> Result<UpdateSettingsSnapshot, StoreError> {
    let object = value.as_object().cloned().unwrap_or_default();
    Ok(UpdateSettingsSnapshot {
        channel: string_field(&object, "channel"),
        auto_install: bool_field(&object, "auto_install", true),
    })
}

pub(crate) fn read_update_state_snapshot(
    connection: &Connection,
) -> Result<Option<UpdateStateSnapshot>, StoreError> {
    let Some(object) = read_object(connection, UPDATE_STATE_SCOPE, UPDATE_STATE_DOCUMENT)? else {
        return Ok(None);
    };
    Ok(Some(UpdateStateSnapshot {
        current_url: object
            .get("current_url")
            .and_then(Value::as_str)
            .map(str::to_string),
        staged_json: object
            .get("staged")
            .filter(|value| !value.is_null())
            .map(|value| serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec())),
        cleanup_app: object
            .get("cleanup_app")
            .and_then(Value::as_str)
            .map(str::to_string),
    }))
}

pub(crate) fn write_update_state_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &UpdateStateSnapshot,
) -> Result<(), StoreError> {
    let staged = snapshot
        .staged_json
        .as_deref()
        .and_then(|raw| serde_json::from_slice::<Value>(raw).ok())
        .unwrap_or(Value::Null);
    write_object(
        transaction,
        UPDATE_STATE_SCOPE,
        UPDATE_STATE_DOCUMENT,
        &json!({
            "current_url": snapshot.current_url,
            "staged": staged,
            "cleanup_app": snapshot.cleanup_app,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    )
}

pub(crate) fn json_from_update_state(snapshot: &UpdateStateSnapshot) -> Result<Value, StoreError> {
    let staged = match snapshot.staged_json.as_deref() {
        Some(raw) => serde_json::from_slice(raw)?,
        None => Value::Null,
    };
    Ok(json!({
        "current_url": snapshot.current_url,
        "staged": staged,
        "cleanup_app": snapshot.cleanup_app,
    }))
}

pub(crate) fn update_state_from_document(value: &Value) -> Result<UpdateStateSnapshot, StoreError> {
    let object = value.as_object().cloned().unwrap_or_default();
    Ok(UpdateStateSnapshot {
        current_url: object
            .get("current_url")
            .and_then(Value::as_str)
            .map(str::to_string),
        staged_json: object
            .get("staged")
            .filter(|value| !value.is_null())
            .map(|value| serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec())),
        cleanup_app: object
            .get("cleanup_app")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

pub(crate) fn read_worktree_inherit_snapshot(
    connection: &Connection,
) -> Result<Option<WorktreeInheritSnapshot>, StoreError> {
    let Some(object) = read_object(connection, WORKTREE_SCOPE, WORKTREE_DOCUMENT)? else {
        return Ok(None);
    };
    Ok(Some(WorktreeInheritSnapshot {
        enabled: bool_field(&object, "enabled", false),
        skip_patterns: string_list(object.get("skip_patterns")),
    }))
}

pub(crate) fn write_worktree_inherit_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &WorktreeInheritSnapshot,
) -> Result<(), StoreError> {
    write_object(
        transaction,
        WORKTREE_SCOPE,
        WORKTREE_DOCUMENT,
        &json!({
            "enabled": snapshot.enabled,
            "skip_patterns": snapshot.skip_patterns,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    )
}

pub(crate) fn json_from_worktree_inherit(snapshot: &WorktreeInheritSnapshot) -> Value {
    json!({
        "enabled": snapshot.enabled,
        "skip_patterns": snapshot.skip_patterns,
    })
}

pub(crate) fn worktree_inherit_from_document(
    value: &Value,
) -> Result<WorktreeInheritSnapshot, StoreError> {
    let object = value.as_object().cloned().unwrap_or_default();
    Ok(WorktreeInheritSnapshot {
        enabled: bool_field(&object, "enabled", false),
        skip_patterns: string_list(object.get("skip_patterns")),
    })
}

pub(crate) fn read_terminal_theme_snapshot(
    connection: &Connection,
) -> Result<Option<TerminalThemeSnapshot>, StoreError> {
    let Some(object) = read_object(connection, TERMINAL_THEME_SCOPE, TERMINAL_THEME_DOCUMENT)?
    else {
        return Ok(None);
    };
    Ok(Some(terminal_theme_from_object(&object)))
}

pub(crate) fn write_terminal_theme_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &TerminalThemeSnapshot,
) -> Result<(), StoreError> {
    write_object(
        transaction,
        TERMINAL_THEME_SCOPE,
        TERMINAL_THEME_DOCUMENT,
        &terminal_theme_object(snapshot),
    )
}

pub(crate) fn json_from_terminal_theme(snapshot: &TerminalThemeSnapshot) -> Value {
    Value::Object(terminal_theme_object(snapshot))
}

pub(crate) fn terminal_theme_from_document(
    value: &Value,
) -> Result<TerminalThemeSnapshot, StoreError> {
    Ok(terminal_theme_from_object(
        &value.as_object().cloned().unwrap_or_default(),
    ))
}

fn terminal_theme_from_object(object: &Map<String, Value>) -> TerminalThemeSnapshot {
    TerminalThemeSnapshot {
        version: u32_field(object, "version", 1),
        dark: bool_field(object, "dark", true),
        background: u32_field(object, "background", 0),
        foreground: u32_field(object, "foreground", 0),
        cursor: u32_field(object, "cursor", 0),
        selection: u32_field(object, "selection", 0),
        palette: object
            .get("palette")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .map(|value| value as u32)
            .collect(),
        search_hit: u32_aliased(object, "search_hit", "searchHit"),
        search_hit_current: u32_aliased(object, "search_hit_current", "searchHitCurrent"),
    }
}

fn terminal_theme_object(snapshot: &TerminalThemeSnapshot) -> Map<String, Value> {
    json!({
        "version": snapshot.version,
        "dark": snapshot.dark,
        "background": snapshot.background,
        "foreground": snapshot.foreground,
        "cursor": snapshot.cursor,
        "selection": snapshot.selection,
        "palette": snapshot.palette,
        "search_hit": snapshot.search_hit,
        "search_hit_current": snapshot.search_hit_current,
    })
    .as_object()
    .cloned()
    .unwrap_or_default()
}

pub(crate) fn read_quota_cache_snapshot(
    connection: &Connection,
) -> Result<Option<QuotaCacheSnapshot>, StoreError> {
    let Some(object) = read_object(connection, QUOTA_CACHE_SCOPE, QUOTA_CACHE_DOCUMENT)? else {
        return Ok(None);
    };
    Ok(Some(quota_from_object(&object)))
}

pub(crate) fn write_quota_cache_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &QuotaCacheSnapshot,
) -> Result<(), StoreError> {
    write_object(
        transaction,
        QUOTA_CACHE_SCOPE,
        QUOTA_CACHE_DOCUMENT,
        &quota_object(snapshot)?,
    )
}

pub(crate) fn json_from_quota_cache(snapshot: &QuotaCacheSnapshot) -> Result<Value, StoreError> {
    Ok(Value::Object(quota_object(snapshot)?))
}

pub(crate) fn quota_cache_from_document(value: &Value) -> Result<QuotaCacheSnapshot, StoreError> {
    Ok(quota_from_object(
        &value.as_object().cloned().unwrap_or_default(),
    ))
}

fn quota_from_object(object: &Map<String, Value>) -> QuotaCacheSnapshot {
    QuotaCacheSnapshot {
        saved_at_ms: i64_field(object, "saved_at_ms", 0),
        schema_version: u32_field(object, "schema_version", 0),
        provider_ids: string_list(object.get("provider_ids")),
        providers_json: json_bytes(object.get("providers"), json!({})),
    }
}

fn quota_object(snapshot: &QuotaCacheSnapshot) -> Result<Map<String, Value>, StoreError> {
    let providers: Value = serde_json::from_slice(&snapshot.providers_json)?;
    Ok(json!({
        "saved_at_ms": snapshot.saved_at_ms,
        "schema_version": snapshot.schema_version,
        "provider_ids": snapshot.provider_ids,
        "providers": providers,
    })
    .as_object()
    .cloned()
    .unwrap_or_default())
}

pub(crate) fn read_dsh_auto_model_snapshot(
    connection: &Connection,
) -> Result<Option<AutoModelSnapshot>, StoreError> {
    read_auto_model(connection, DSH_AUTO_SCOPE, DSH_AUTO_DOCUMENT)
}

pub(crate) fn write_dsh_auto_model_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &AutoModelSnapshot,
) -> Result<(), StoreError> {
    write_auto_model(transaction, DSH_AUTO_SCOPE, DSH_AUTO_DOCUMENT, snapshot)
}

pub(crate) fn read_pi_auto_model_snapshot(
    connection: &Connection,
) -> Result<Option<AutoModelSnapshot>, StoreError> {
    read_auto_model(connection, PI_AUTO_SCOPE, PI_AUTO_DOCUMENT)
}

pub(crate) fn write_pi_auto_model_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &AutoModelSnapshot,
) -> Result<(), StoreError> {
    write_auto_model(transaction, PI_AUTO_SCOPE, PI_AUTO_DOCUMENT, snapshot)
}

fn read_auto_model(
    connection: &Connection,
    scope: &str,
    leftover: &str,
) -> Result<Option<AutoModelSnapshot>, StoreError> {
    let Some(object) = read_object(connection, scope, leftover)? else {
        return Ok(None);
    };
    Ok(Some(AutoModelSnapshot {
        providers: string_list(object.get("providers")),
    }))
}

fn write_auto_model(
    transaction: &Transaction<'_>,
    scope: &str,
    leftover: &str,
    snapshot: &AutoModelSnapshot,
) -> Result<(), StoreError> {
    write_object(
        transaction,
        scope,
        leftover,
        &json!({ "providers": snapshot.providers })
            .as_object()
            .cloned()
            .unwrap_or_default(),
    )
}

pub(crate) fn json_from_auto_model(snapshot: &AutoModelSnapshot) -> Value {
    json!({ "providers": snapshot.providers })
}

pub(crate) fn auto_model_from_document(value: &Value) -> Result<AutoModelSnapshot, StoreError> {
    Ok(AutoModelSnapshot {
        providers: string_list(value.get("providers")),
    })
}

pub(crate) fn read_workspace_menu_snapshot(
    connection: &Connection,
) -> Result<Option<PublishedWorkspaceMenuSnapshot>, StoreError> {
    let Some(object) = read_object(connection, WORKSPACE_MENU_SCOPE, WORKSPACE_MENU_DOCUMENT)?
    else {
        return Ok(None);
    };
    Ok(Some(PublishedWorkspaceMenuSnapshot {
        version: u32_field(&object, "version", 0),
        revision: object.get("revision").and_then(Value::as_u64).unwrap_or(0),
        source_id: string_field(&object, "source_id"),
        source_revision: object
            .get("source_revision")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        projects_json: json_bytes(object.get("projects"), json!([])),
        sessions_json: json_bytes(object.get("sessions"), json!([])),
    }))
}

pub(crate) fn write_workspace_menu_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &PublishedWorkspaceMenuSnapshot,
) -> Result<(), StoreError> {
    write_object(
        transaction,
        WORKSPACE_MENU_SCOPE,
        WORKSPACE_MENU_DOCUMENT,
        &workspace_menu_object(snapshot)?,
    )
}

pub(crate) fn json_from_workspace_menu(
    snapshot: &PublishedWorkspaceMenuSnapshot,
) -> Result<Value, StoreError> {
    Ok(Value::Object(workspace_menu_object(snapshot)?))
}

pub(crate) fn workspace_menu_from_document(
    value: &Value,
) -> Result<PublishedWorkspaceMenuSnapshot, StoreError> {
    let object = value.as_object().cloned().unwrap_or_default();
    Ok(PublishedWorkspaceMenuSnapshot {
        version: u32_field(&object, "version", 0),
        revision: object.get("revision").and_then(Value::as_u64).unwrap_or(0),
        source_id: string_field(&object, "source_id"),
        source_revision: object
            .get("source_revision")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        projects_json: json_bytes(object.get("projects"), json!([])),
        sessions_json: json_bytes(object.get("sessions"), json!([])),
    })
}

fn workspace_menu_object(
    snapshot: &PublishedWorkspaceMenuSnapshot,
) -> Result<Map<String, Value>, StoreError> {
    let projects: Value = serde_json::from_slice(&snapshot.projects_json)?;
    let sessions: Value = serde_json::from_slice(&snapshot.sessions_json)?;
    Ok(json!({
        "version": snapshot.version,
        "revision": snapshot.revision,
        "source_id": snapshot.source_id,
        "source_revision": snapshot.source_revision,
        "projects": projects,
        "sessions": sessions,
    })
    .as_object()
    .cloned()
    .unwrap_or_default())
}

pub(crate) const CONFIG_DOCUMENTS: &[&str] = &[
    COLLAB_DOCUMENT,
    UPDATE_SETTINGS_DOCUMENT,
    UPDATE_STATE_DOCUMENT,
    WORKTREE_DOCUMENT,
    TERMINAL_THEME_DOCUMENT,
    QUOTA_CACHE_DOCUMENT,
    DSH_AUTO_DOCUMENT,
    PI_AUTO_DOCUMENT,
    WORKSPACE_MENU_DOCUMENT,
];

pub(crate) fn read_config_document(
    connection: &Connection,
    key: &str,
) -> Result<Option<Vec<u8>>, StoreError> {
    let value = match key {
        COLLAB_DOCUMENT => read_remote_config_snapshot(connection)?
            .map(|snapshot| json_from_remote_config(&snapshot)),
        UPDATE_SETTINGS_DOCUMENT => read_update_settings_snapshot(connection)?
            .map(|snapshot| json_from_update_settings(&snapshot)),
        UPDATE_STATE_DOCUMENT => read_update_state_snapshot(connection)?
            .map(|snapshot| json_from_update_state(&snapshot))
            .transpose()?,
        WORKTREE_DOCUMENT => read_worktree_inherit_snapshot(connection)?
            .map(|snapshot| json_from_worktree_inherit(&snapshot)),
        TERMINAL_THEME_DOCUMENT => read_terminal_theme_snapshot(connection)?
            .map(|snapshot| json_from_terminal_theme(&snapshot)),
        QUOTA_CACHE_DOCUMENT => read_quota_cache_snapshot(connection)?
            .map(|snapshot| json_from_quota_cache(&snapshot))
            .transpose()?,
        DSH_AUTO_DOCUMENT => read_dsh_auto_model_snapshot(connection)?
            .map(|snapshot| json_from_auto_model(&snapshot)),
        PI_AUTO_DOCUMENT => {
            read_pi_auto_model_snapshot(connection)?.map(|snapshot| json_from_auto_model(&snapshot))
        }
        WORKSPACE_MENU_DOCUMENT => read_workspace_menu_snapshot(connection)?
            .map(|snapshot| json_from_workspace_menu(&snapshot))
            .transpose()?,
        _ => return Ok(None),
    };
    match value {
        Some(value) => serde_json::to_vec(&value)
            .map(Some)
            .map_err(StoreError::from),
        None => crate::documents::read_generic_document_bytes(connection, key),
    }
}

pub(crate) fn write_config_document(
    transaction: &Transaction<'_>,
    key: &str,
    value: &Value,
) -> Result<bool, StoreError> {
    match key {
        COLLAB_DOCUMENT => {
            write_remote_config_snapshot(transaction, &remote_config_from_document(value)?)?
        }
        UPDATE_SETTINGS_DOCUMENT => {
            write_update_settings_snapshot(transaction, &update_settings_from_document(value)?)?
        }
        UPDATE_STATE_DOCUMENT => {
            write_update_state_snapshot(transaction, &update_state_from_document(value)?)?
        }
        WORKTREE_DOCUMENT => {
            write_worktree_inherit_snapshot(transaction, &worktree_inherit_from_document(value)?)?
        }
        TERMINAL_THEME_DOCUMENT => {
            write_terminal_theme_snapshot(transaction, &terminal_theme_from_document(value)?)?
        }
        QUOTA_CACHE_DOCUMENT => {
            write_quota_cache_snapshot(transaction, &quota_cache_from_document(value)?)?
        }
        DSH_AUTO_DOCUMENT => {
            write_dsh_auto_model_snapshot(transaction, &auto_model_from_document(value)?)?
        }
        PI_AUTO_DOCUMENT => {
            write_pi_auto_model_snapshot(transaction, &auto_model_from_document(value)?)?
        }
        WORKSPACE_MENU_DOCUMENT => {
            write_workspace_menu_snapshot(transaction, &workspace_menu_from_document(value)?)?
        }
        _ => return Ok(false),
    }
    Ok(true)
}

pub(crate) fn delete_config_document(
    transaction: &Transaction<'_>,
    key: &str,
) -> Result<bool, StoreError> {
    let (scope, leftover) = match key {
        COLLAB_DOCUMENT => (COLLAB_SCOPE, COLLAB_DOCUMENT),
        UPDATE_SETTINGS_DOCUMENT => (UPDATE_SETTINGS_SCOPE, UPDATE_SETTINGS_DOCUMENT),
        UPDATE_STATE_DOCUMENT => (UPDATE_STATE_SCOPE, UPDATE_STATE_DOCUMENT),
        WORKTREE_DOCUMENT => (WORKTREE_SCOPE, WORKTREE_DOCUMENT),
        TERMINAL_THEME_DOCUMENT => (TERMINAL_THEME_SCOPE, TERMINAL_THEME_DOCUMENT),
        QUOTA_CACHE_DOCUMENT => (QUOTA_CACHE_SCOPE, QUOTA_CACHE_DOCUMENT),
        DSH_AUTO_DOCUMENT => (DSH_AUTO_SCOPE, DSH_AUTO_DOCUMENT),
        PI_AUTO_DOCUMENT => (PI_AUTO_SCOPE, PI_AUTO_DOCUMENT),
        WORKSPACE_MENU_DOCUMENT => (WORKSPACE_MENU_SCOPE, WORKSPACE_MENU_DOCUMENT),
        _ => return Ok(false),
    };
    crate::documents::delete_scope(transaction, scope)?;
    crate::documents::delete_document_scope(transaction, leftover)?;
    Ok(true)
}
