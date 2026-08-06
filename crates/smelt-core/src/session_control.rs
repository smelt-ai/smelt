//! UI-independent ACP session catalog and lifecycle requests.
//!
//! Desktop and mobile render different controls, but agent/profile resolution, transcript
//! discovery and smeltd lifecycle operations belong here. Mobile clients only send stable ids;
//! launch commands and local paths never need to be assembled on the phone.

use crate::agent_kind::{AcpAgentKind, AcpLaunchSpec, AcpProfile};
use crate::workspace_menu::{WorkspaceMenuProject, WorkspaceMenuSnapshot};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

static REMOTE_STORE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpAgentOption {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub profile: bool,
    #[serde(skip_serializing)]
    pub launch: AcpLaunchSpec,
    #[serde(skip_serializing)]
    pub history_dir: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySessionSummary {
    #[serde(skip)]
    pub path: PathBuf,
    pub resume_id: String,
    pub title: String,
    pub started_at: Option<DateTime<Utc>>,
    pub last_active_at: Option<DateTime<Utc>>,
    pub message_count: usize,
    #[serde(skip)]
    pub total_tokens: u64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RemoteAcpSession {
    pub id: String,
    pub cwd: String,
    #[serde(default)]
    pub title: String,
    pub agent_option_id: String,
    pub agent: String,
    pub launch: AcpLaunchSpec,
    #[serde(default)]
    pub resume_id: Option<String>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct RemoteAcpSessions {
    #[serde(default)]
    sessions: Vec<RemoteAcpSession>,
}

/// 手机端远程新建的终端会话（不依赖 PC GUI 写 workspace.json 才能显示/管理）。
/// 跟 `RemoteAcpSession` 是同一套思路，见文件顶部注释。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RemoteTerminalSession {
    pub id: String,
    pub cwd: String,
    #[serde(default)]
    pub title: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct RemoteTerminalSessions {
    #[serde(default)]
    sessions: Vec<RemoteTerminalSession>,
}

fn smelt_path(name: &str) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".smelt")
        .join(name)
}

fn agent_ui_value() -> Value {
    let path = smelt_path("agent_ui.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null)
}

fn configured_command(value: &Value, kind: AcpAgentKind) -> String {
    let key = match kind {
        AcpAgentKind::Claude => "acp_cmd",
        AcpAgentKind::Copilot => "acp_copilot_cmd",
        AcpAgentKind::Codex => "acp_codex_cmd",
        AcpAgentKind::Grok => "acp_grok_cmd",
    };
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|command| !command.trim().is_empty())
        .map(String::from)
        .unwrap_or_else(|| kind.default_cmd())
}

pub fn agent_options() -> Vec<AcpAgentOption> {
    let value = agent_ui_value();
    let mut options = AcpAgentKind::ALL
        .into_iter()
        .map(|kind| AcpAgentOption {
            id: kind.id().to_string(),
            kind: kind.id().to_string(),
            label: kind.label().to_string(),
            profile: false,
            launch: AcpLaunchSpec::from_command(configured_command(&value, kind)),
            history_dir: None,
        })
        .collect::<Vec<_>>();
    let profiles = value
        .get("profiles")
        .cloned()
        .and_then(|profiles| serde_json::from_value::<Vec<AcpProfile>>(profiles).ok())
        .unwrap_or_default();
    for profile in profiles {
        let kind = profile.kind();
        let mut launch = profile.launch_spec();
        launch.command = configured_command(&value, kind);
        options.push(AcpAgentOption {
            id: format!("profile:{}", profile.id),
            kind: kind.id().to_string(),
            label: profile.label,
            profile: true,
            launch,
            history_dir: Some(crate::workspace_override::expand_tilde(&profile.workspace_dir)),
        });
    }
    options
}

pub fn find_agent_option(id: &str) -> Option<AcpAgentOption> {
    agent_options().into_iter().find(|option| option.id == id)
}

pub fn workspace_projects(menu: &WorkspaceMenuSnapshot) -> Vec<WorkspaceMenuProject> {
    let mut projects = menu.projects.clone();
    projects.sort_by_key(|project| project.order);
    projects
}

fn remote_sessions_path() -> PathBuf {
    smelt_path("remote_acp_sessions.json")
}

pub fn load_remote_sessions() -> Vec<RemoteAcpSession> {
    let _guard = REMOTE_STORE_LOCK.lock().unwrap();
    load_remote_sessions_unlocked()
}

fn load_remote_sessions_unlocked() -> Vec<RemoteAcpSession> {
    crate::json_store::load_json::<RemoteAcpSessions>(Some(remote_sessions_path())).sessions
}

fn save_remote_sessions(sessions: Vec<RemoteAcpSession>) {
    crate::json_store::save_json_private(
        Some(remote_sessions_path()),
        &RemoteAcpSessions { sessions },
    );
}

pub fn remember_remote_session(session: RemoteAcpSession) {
    let _guard = REMOTE_STORE_LOCK.lock().unwrap();
    let mut sessions = load_remote_sessions_unlocked();
    sessions.retain(|existing| existing.id != session.id);
    sessions.push(session);
    save_remote_sessions(sessions);
}

pub fn forget_remote_session(id: &str) {
    let _guard = REMOTE_STORE_LOCK.lock().unwrap();
    let mut sessions = load_remote_sessions_unlocked();
    sessions.retain(|session| session.id != id);
    save_remote_sessions(sessions);
}

fn remote_terminal_sessions_path() -> PathBuf {
    smelt_path("remote_terminal_sessions.json")
}

pub fn load_remote_terminal_sessions() -> Vec<RemoteTerminalSession> {
    let _guard = REMOTE_STORE_LOCK.lock().unwrap();
    load_remote_terminal_sessions_unlocked()
}

fn load_remote_terminal_sessions_unlocked() -> Vec<RemoteTerminalSession> {
    crate::json_store::load_json::<RemoteTerminalSessions>(Some(remote_terminal_sessions_path()))
        .sessions
}

fn save_remote_terminal_sessions(sessions: Vec<RemoteTerminalSession>) {
    crate::json_store::save_json_private(
        Some(remote_terminal_sessions_path()),
        &RemoteTerminalSessions { sessions },
    );
}

pub fn remember_remote_terminal_session(session: RemoteTerminalSession) {
    let _guard = REMOTE_STORE_LOCK.lock().unwrap();
    let mut sessions = load_remote_terminal_sessions_unlocked();
    sessions.retain(|existing| existing.id != session.id);
    sessions.push(session);
    save_remote_terminal_sessions(sessions);
}

pub fn forget_remote_terminal_session(id: &str) {
    let _guard = REMOTE_STORE_LOCK.lock().unwrap();
    let mut sessions = load_remote_terminal_sessions_unlocked();
    sessions.retain(|session| session.id != id);
    save_remote_terminal_sessions(sessions);
}

/// smeltd 当前活着的会话 id（终端 + ACP，靠前缀区分）。任务对账用：判断绑定会话
/// 是否还活着，不活的任务标失败，避免「会话没了但任务永远卡 Running」。
pub fn list_sessions() -> Result<Vec<String>, String> {
    let response = daemon_request(json!({ "op": "list" }))?;
    Ok(response
        .get("sessions")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default())
}

fn daemon_request(request: Value) -> Result<Value, String> {
    let mut stream = UnixStream::connect(crate::daemon_state::smeltd_sock_path())
        .map_err(|error| format!("smeltd unavailable: {error}"))?;
    writeln!(stream, "{request}").map_err(|error| error.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    if line.trim().is_empty() {
        return Err("smeltd returned no response".into());
    }
    let response: Value = serde_json::from_str(line.trim()).map_err(|error| error.to_string())?;
    if response.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("session operation failed")
            .to_string());
    }
    Ok(response)
}

pub fn create_acp_session(session: &RemoteAcpSession) -> Result<(), String> {
    daemon_request(serde_json::json!({
        "op": "acp_create",
        "id": session.id,
        "cwd": session.cwd,
        "launch": session.launch,
        "agent": session.agent,
        "resume_id": session.resume_id,
    }))?;
    Ok(())
}

pub fn delete_acp_session(id: &str) -> Result<(), String> {
    daemon_request(serde_json::json!({"op": "acp_kill", "id": id}))?;
    forget_remote_session(id);
    Ok(())
}

/// 新建一个终端 PTY 会话。走 smeltd 的 `open` op：会话在守护里落地（进程已
/// spawn、已存进 sessions map）发生在它回第一行 JSON 之前，所以这条请求/响应
/// 一来一回就够——不需要像交互 attach 那样占住连接进流模式，回完这行 socket
/// 直接丢掉，PTY 照样常驻（同一套"GUI 退出会话不死"的保证）。
pub fn create_terminal_session(id: &str, cwd: &str) -> Result<(), String> {
    daemon_request(serde_json::json!({
        "op": "open",
        "id": id,
        "cwd": cwd,
        "cols": 100,
        "rows": 32,
    }))?;
    Ok(())
}

pub fn delete_terminal_session(id: &str) -> Result<(), String> {
    daemon_request(serde_json::json!({"op": "kill", "id": id}))?;
    forget_remote_terminal_session(id);
    Ok(())
}

pub fn list_history(option: &AcpAgentOption, cwd: &str) -> Vec<HistorySessionSummary> {
    let kind = AcpAgentKind::from_id(&option.kind).unwrap_or(AcpAgentKind::Claude);
    list_history_for(kind, cwd, option.history_dir.as_deref())
}

pub fn list_history_for(
    kind: AcpAgentKind,
    cwd: &str,
    override_dir: Option<&str>,
) -> Vec<HistorySessionSummary> {
    let mut sessions = match kind {
        AcpAgentKind::Claude => list_claude_history(cwd, override_dir),
        AcpAgentKind::Codex => list_codex_history(cwd, override_dir),
        AcpAgentKind::Grok => list_grok_history(cwd, override_dir),
        AcpAgentKind::Copilot => list_copilot_history(cwd, override_dir),
    };
    sessions.sort_by(|a, b| b.last_active_at.cmp(&a.last_active_at));
    sessions
}

fn parse_time(value: Option<&Value>) -> Option<DateTime<Utc>> {
    value
        .and_then(Value::as_str)
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(|time| time.with_timezone(&Utc))
}

fn truncate(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= 80 {
        return text.to_string();
    }
    format!("{}…", text.chars().take(80).collect::<String>())
}

fn read_paths(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default()
}

fn claude_user_text(content: &Value) -> Option<String> {
    let mut text = if let Some(text) = content.as_str() {
        text.to_string()
    } else {
        content
            .as_array()?
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let synthetic = [
        ("<command-name>", "</command-name>"),
        ("<command-message>", "</command-message>"),
        ("<command-args>", "</command-args>"),
        ("<local-command-stdout>", "</local-command-stdout>"),
        ("<local-command-stderr>", "</local-command-stderr>"),
    ];
    for (open, close) in synthetic {
        while let Some(start) = text.find(open) {
            let Some(relative_end) = text[start + open.len()..].find(close) else { break };
            let end = start + open.len() + relative_end + close.len();
            text.replace_range(start..end, "");
        }
    }
    (!text.trim().is_empty()).then_some(text)
}

fn list_claude_history(cwd: &str, override_dir: Option<&str>) -> Vec<HistorySessionSummary> {
    let dir = crate::claude_paths::projects_root(override_dir)
        .join(crate::claude_paths::project_dir(cwd));
    read_paths(&dir)
        .into_iter()
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .filter_map(|path| {
            let body = std::fs::read_to_string(&path).ok()?;
            let mut title = None;
            let mut started_at = None;
            let mut last_active_at = None;
            let mut message_count = 0;
            let mut total_tokens = 0;
            let mut seen = HashSet::new();
            for row in body.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()) {
                if row.get("isMeta").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                let Some(kind) = row.get("type").and_then(Value::as_str) else { continue };
                if kind != "user" && kind != "assistant" {
                    continue;
                }
                if let Some(time) = parse_time(row.get("timestamp")) {
                    started_at = Some(started_at.map_or(time, |old: DateTime<Utc>| old.min(time)));
                    last_active_at = Some(last_active_at.map_or(time, |old: DateTime<Utc>| old.max(time)));
                }
                if kind == "user" {
                    if let Some(text) = row.get("message").and_then(|m| m.get("content")).and_then(claude_user_text) {
                        message_count += 1;
                        if title.is_none() { title = Some(truncate(&text)); }
                    }
                } else {
                    let duplicate = row.get("uuid").and_then(Value::as_str).is_some_and(|id| !seen.insert(id.to_string()));
                    let has_text = row.get("message").and_then(|m| m.get("content")).and_then(Value::as_array)
                        .is_some_and(|blocks| blocks.iter().any(|block| block.get("type").and_then(Value::as_str) == Some("text")));
                    if has_text && !duplicate { message_count += 1; }
                    if !duplicate {
                        if let Some(usage) = row.get("message").and_then(|message| message.get("usage")) {
                            total_tokens += ["input_tokens", "output_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"]
                                .into_iter()
                                .map(|key| usage.get(key).and_then(Value::as_u64).unwrap_or(0))
                                .sum::<u64>();
                        }
                    }
                }
            }
            Some(HistorySessionSummary {
                path: path.clone(),
                resume_id: path.file_stem()?.to_str()?.to_string(),
                title: title?,
                started_at,
                last_active_at,
                message_count,
                total_tokens,
            })
        })
        .collect()
}

fn codex_home(override_dir: Option<&str>) -> PathBuf {
    override_dir
        .map(PathBuf::from)
        .or_else(|| crate::login_env::codex_home().map(PathBuf::from))
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join(".codex"))
}

fn codex_message_text(payload: &Value) -> Option<String> {
    let text = payload.get("content")?.as_array()?.iter()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>().join("\n");
    (!text.trim().is_empty()).then_some(text)
}

fn list_codex_history(cwd: &str, override_dir: Option<&str>) -> Vec<HistorySessionSummary> {
    let root = codex_home(override_dir).join("sessions");
    let mut paths = Vec::new();
    for year in read_paths(&root) { for month in read_paths(&year) { for day in read_paths(&month) { paths.extend(read_paths(&day)); } } }
    paths.into_iter().filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")).filter_map(|path| {
        let body = std::fs::read_to_string(&path).ok()?;
        let mut lines = body.lines();
        let meta: Value = serde_json::from_str(lines.next()?).ok()?;
        if meta.get("type").and_then(Value::as_str) != Some("session_meta") { return None; }
        let payload = meta.get("payload")?;
        if payload.get("cwd").and_then(Value::as_str) != Some(cwd) { return None; }
        let resume_id = payload.get("id").and_then(Value::as_str)?.to_string();
        let started_at = parse_time(meta.get("timestamp"));
        let mut last_active_at = started_at;
        let mut title = None;
        let mut message_count = 0;
        for row in lines.filter_map(|line| serde_json::from_str::<Value>(line).ok()) {
            if let Some(time) = parse_time(row.get("timestamp")) { last_active_at = Some(last_active_at.map_or(time, |old| old.max(time))); }
            if row.get("type").and_then(Value::as_str) != Some("response_item") { continue; }
            let Some(item) = row.get("payload") else { continue };
            if item.get("type").and_then(Value::as_str) != Some("message") { continue; }
            let role = item.get("role").and_then(Value::as_str);
            if role != Some("user") && role != Some("assistant") { continue; }
            let Some(text) = codex_message_text(item) else { continue };
            if role == Some("user") && (text.trim_start().starts_with('<') || text.trim_start().starts_with("# Context from")) { continue; }
            message_count += 1;
            if role == Some("user") && title.is_none() { title = Some(truncate(&text)); }
        }
        Some(HistorySessionSummary { path, resume_id: resume_id.clone(), title: title.unwrap_or(resume_id), started_at, last_active_at, message_count, total_tokens: 0 })
    }).collect()
}

fn grok_home(override_dir: Option<&str>) -> PathBuf {
    override_dir.map(PathBuf::from)
        .or_else(|| crate::login_env::grok_home().map(PathBuf::from))
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join(".grok"))
}

fn list_grok_history(cwd: &str, override_dir: Option<&str>) -> Vec<HistorySessionSummary> {
    let mut sessions = Vec::new();
    for project in read_paths(&grok_home(override_dir).join("sessions")) {
        for dir in read_paths(&project) {
            let summary: Value = match std::fs::read_to_string(dir.join("summary.json")).ok().and_then(|raw| serde_json::from_str(&raw).ok()) { Some(value) => value, None => continue };
            if summary.get("info").and_then(|info| info.get("cwd")).and_then(Value::as_str) != Some(cwd) { continue; }
            let Some(resume_id) = dir.file_name().and_then(|name| name.to_str()).map(String::from) else { continue };
            let title = summary.get("session_summary").and_then(Value::as_str).filter(|text| !text.trim().is_empty()).map(truncate).unwrap_or_else(|| resume_id.clone());
            sessions.push(HistorySessionSummary { path: dir, resume_id, title, started_at: parse_time(summary.get("created_at")), last_active_at: parse_time(summary.get("updated_at")), message_count: summary.get("num_chat_messages").and_then(Value::as_u64).unwrap_or(0) as usize, total_tokens: 0 });
        }
    }
    sessions
}

fn copilot_home(override_dir: Option<&str>) -> PathBuf {
    override_dir.map(PathBuf::from)
        .or_else(|| crate::login_env::copilot_home().map(PathBuf::from))
        .or_else(|| crate::login_env::xdg_config_home().map(|dir| PathBuf::from(dir).join("copilot")))
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join(".copilot"))
}

fn flat_yaml(text: &str) -> BTreeMap<String, String> {
    text.lines().filter_map(|line| line.split_once(": ")).map(|(key, value)| (key.trim().to_string(), value.trim().to_string())).collect()
}

fn list_copilot_history(cwd: &str, override_dir: Option<&str>) -> Vec<HistorySessionSummary> {
    read_paths(&copilot_home(override_dir).join("session-state")).into_iter().filter_map(|dir| {
        let fields = flat_yaml(&std::fs::read_to_string(dir.join("workspace.yaml")).ok()?);
        if fields.get("cwd").map(String::as_str) != Some(cwd) { return None; }
        let resume_id = dir.file_name()?.to_str()?.to_string();
        let title = fields.get("summary").or_else(|| fields.get("name")).filter(|text| !text.trim().is_empty()).map(|text| truncate(text)).unwrap_or_else(|| resume_id.clone());
        let message_count = std::fs::read_to_string(dir.join("events.jsonl")).ok().map(|body| body.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()).filter(|row| matches!(row.get("type").and_then(Value::as_str), Some("user.message" | "assistant.message"))).count()).unwrap_or(0);
        Some(HistorySessionSummary { path: dir, resume_id, title, started_at: fields.get("created_at").and_then(|text| DateTime::parse_from_rfc3339(text).ok()).map(|time| time.with_timezone(&Utc)), last_active_at: fields.get("updated_at").and_then(|text| DateTime::parse_from_rfc3339(text).ok()).map(|time| time.with_timezone(&Utc)), message_count, total_tokens: 0 })
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_agent_options_use_shared_agent_definitions() {
        let options = agent_options();
        for kind in AcpAgentKind::ALL {
            let option = options.iter().find(|option| option.id == kind.id()).unwrap();
            assert_eq!(option.kind, kind.id());
            assert!(!option.launch.command.trim().is_empty());
        }
    }

    #[test]
    fn truncates_history_titles_without_breaking_unicode() {
        let input = "你".repeat(81);
        assert_eq!(truncate(&input).chars().count(), 81);
        assert!(truncate(&input).ends_with('…'));
    }
}
