//! 远程操作网关的核心逻辑（路由 + handler），供两个地方使用：
//! - `crates/smeltd/src/bin/gateway.rs`：独立进程，命令行启动，自己管一个 `--bind`/`--port`
//! - `crates/smeltd/src/main.rs`：内嵌进守护，靠 `remote_start`/`remote_stop` op 按需开关
//!
//! 两边共用同一份 handler，避免同一套鉴权/转义/协议逻辑复制两次（CLAUDE.md 明令
//! 别复制）。这个模块本身**不碰 smeltd 主协议**：所有跟 smeltd 的交互都是走
//! `sock_path()` 连它自己的 unix socket，用既有的 `list`/`watch` op——不管是从独立
//! 进程调用还是从 smeltd 内部的这个模块调用，走的都是同一条路径，行为完全一致。
//!
//! 只服务移动 App：对外能力是 `/acp/*` 与 `/terminal/*`。浏览器面板（remote-web SPA、
//! 内嵌 HTML 终端、`/s/{id}` 那套流式终端接口）连同 Cloudflare/WebRTC 一起下线了，
//! 所以这里没有任何 HTML 模板与静态资源托管。
//!
//! 见 docs/remote-ops-roadmap.md（Phase 1/2）、docs/collaboration.md（安全底线）。

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::agent_status::AgentStatus;
use crate::attention::{AttentionItem, AttentionStore, apply_daemon_transition};
use crate::daemon_state::{DaemonPhase, DaemonSessionState};
use crate::workspace_menu::{
    WORKSPACE_MENU_VERSION, WorkspaceMenuProject, WorkspaceMenuSession, WorkspaceMenuSessionKind,
    WorkspaceMenuSnapshot,
};

/// 找到（或按 cwd 现造一个）`menu.projects` 里的项目，返回其下标。给"手机远程
/// 建的会话没有对应 PC 项目"兜底——终端和 ACP 会话共用同一份兜底逻辑。
fn ensure_menu_project(menu: &mut WorkspaceMenuSnapshot, cwd: &str) -> usize {
    menu.projects
        .iter()
        .position(|project| project.root == cwd)
        .unwrap_or_else(|| {
            let order = menu.projects.len();
            menu.projects.push(WorkspaceMenuProject {
                root: cwd.to_string(),
                title: path_title(cwd),
                order: order.min(u32::MAX as usize) as u32,
            });
            order
        })
}

fn mobile_workspace_menu() -> WorkspaceMenuSnapshot {
    let mut menu = load_workspace_menu();
    let remote_sessions = crate::session_control::load_remote_sessions();
    for remote in remote_sessions {
        let project_order = ensure_menu_project(&mut menu, &remote.cwd);
        if menu.sessions.iter().any(|session| session.id == remote.id) {
            continue;
        }
        let project = &menu.projects[project_order];
        menu.sessions.push(WorkspaceMenuSession {
            id: remote.id,
            kind: WorkspaceMenuSessionKind::Acp,
            title: if remote.title.trim().is_empty() {
                format!("{} conversation", remote.agent)
            } else {
                remote.title
            },
            custom_title: false,
            cwd: Some(remote.cwd),
            project_root: Some(project.root.clone()),
            project_title: Some(project.title.clone()),
            project_order: project.order,
            session_order: menu.sessions.len().min(u32::MAX as usize) as u32,
            leaf_order: 0,
            agent: Some(remote.agent),
        });
    }
    let remote_terminal_sessions = crate::session_control::load_remote_terminal_sessions();
    for remote in remote_terminal_sessions {
        let project_order = ensure_menu_project(&mut menu, &remote.cwd);
        if menu.sessions.iter().any(|session| session.id == remote.id) {
            continue;
        }
        let project = &menu.projects[project_order];
        menu.sessions.push(WorkspaceMenuSession {
            id: remote.id,
            kind: WorkspaceMenuSessionKind::Terminal,
            title: if remote.title.trim().is_empty() {
                "Terminal".to_string()
            } else {
                remote.title
            },
            custom_title: false,
            cwd: Some(remote.cwd),
            project_root: Some(project.root.clone()),
            project_title: Some(project.title.clone()),
            project_order: project.order,
            session_order: menu.sessions.len().min(u32::MAX as usize) as u32,
            leaf_order: 0,
            agent: None,
        });
    }
    menu
}

pub fn sock_path() -> std::path::PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| "/tmp".into())
        .join(".smelt");
    dir.join("smeltd.sock")
}

#[derive(Clone)]
struct AppState {
    token: Arc<String>,
    /// 这个 token 是否有写权限（approve/deny/reply）。链接分享出去那一刻就是
    /// 授权动作，这里不再加一层"每次点击都要主人当面确认"——见
    /// smeltd.rs「远程操控」一节的授权模型说明。开没开由生成链接时的 GUI 开关
    /// 决定，`build_router` 只是如实转达。
    write_enabled: bool,
    mobile_lifecycle: Arc<MobileLifecycleHub>,
}

struct MobileLifecycleHub {
    state: Mutex<MobileLifecycleState>,
    updates: tokio::sync::broadcast::Sender<MobileLifecycleEvent>,
}

#[derive(Default)]
struct MobileLifecycleState {
    sessions: std::collections::HashMap<String, DaemonSessionState>,
    attention: AttentionStore,
}

#[derive(Clone)]
enum MobileLifecycleEvent {
    SessionsChanged,
    Attention(AttentionItem),
    AttentionResolved(String),
}

#[derive(Deserialize)]
struct AuthQuery {
    token: String,
}

/// 组好整个网关的路由，鉴权用这一个 token（见 collaboration.md：一个网关/token 管
/// 这台机器上的全部活会话，泄漏一条链接的代价是明确的，不是没想到的疏漏）。
///
/// 手机 App 是唯一消费方，它经 iroh 隧道连到这里。ACP 控制面与终端数据面分开，
/// 避免高吞吐 PTY 字节阻塞会话状态与审批消息。
pub fn build_router(token: String, write_enabled: bool) -> Router {
    let mobile_lifecycle = MobileLifecycleHub::start();
    let state = AppState {
        token: Arc::new(token),
        write_enabled,
        mobile_lifecycle,
    };
    Router::new()
        .route("/acp/sessions", get(acp_sessions_handler))
        .route("/acp/ws", get(acp_ws_handler))
        .route("/terminal/{id}/ws", get(terminal_ws_handler))
        .with_state(state)
}

fn workspace_json_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| "/tmp".into())
        .join(".smelt")
        .join("workspace.json")
}

fn load_workspace_menu() -> WorkspaceMenuSnapshot {
    let Ok(raw) = std::fs::read_to_string(workspace_json_path()) else {
        return WorkspaceMenuSnapshot::default();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return WorkspaceMenuSnapshot::default();
    };
    workspace_menu_from_value(&value)
}

fn workspace_menu_from_value(value: &serde_json::Value) -> WorkspaceMenuSnapshot {
    if let Some(menu) = value.get("menu")
        && let Ok(snapshot) = serde_json::from_value::<WorkspaceMenuSnapshot>(menu.clone())
        && snapshot.version > 0
    {
        let mut snapshot = snapshot;
        if snapshot.version < WORKSPACE_MENU_VERSION {
            add_legacy_terminal_leaves(value, &mut snapshot);
        }
        return snapshot;
    }
    let mut snapshot = legacy_workspace_menu_from_value(value);
    add_legacy_terminal_leaves(value, &mut snapshot);
    snapshot
}

fn add_legacy_terminal_leaves(value: &serde_json::Value, menu: &mut WorkspaceMenuSnapshot) {
    let Some(saved_sessions) = value
        .get("sessions")
        .and_then(|sessions| sessions.as_array())
    else {
        return;
    };
    for (session_order, saved) in saved_sessions.iter().enumerate() {
        if saved.get("acp").is_some_and(|acp| !acp.is_null()) {
            continue;
        }
        let mut leaves = Vec::new();
        collect_saved_terminal_leaves(&saved["layout"], &mut leaves);
        if leaves.is_empty() {
            continue;
        }
        let base = leaves.iter().find_map(|leaf| {
            let id = leaf.get("id")?.as_str()?;
            menu.session(id).cloned()
        });
        for (leaf_order, leaf) in leaves.into_iter().enumerate() {
            let Some(id) = leaf.get("id").and_then(|id| id.as_str()) else {
                continue;
            };
            if let Some(existing) = menu.sessions.iter_mut().find(|session| session.id == id) {
                existing.leaf_order = leaf_order.min(u32::MAX as usize) as u32;
                continue;
            }
            let cwd = leaf
                .get("cwd")
                .and_then(|cwd| cwd.as_str())
                .map(String::from);
            let project_root = base
                .as_ref()
                .and_then(|session| session.project_root.clone())
                .or_else(|| {
                    cwd.as_deref().and_then(|cwd| {
                        let roots = menu
                            .projects
                            .iter()
                            .map(|project| project.root.clone())
                            .collect::<Vec<_>>();
                        mobile_project_root(&roots, cwd)
                    })
                });
            let project = project_root
                .as_deref()
                .and_then(|root| menu.projects.iter().find(|project| project.root == root));
            let custom_title = leaf
                .get("custom_title")
                .and_then(|title| title.as_str())
                .map(str::trim)
                .filter(|title| !title.is_empty());
            let title = custom_title
                .map(String::from)
                .or_else(|| {
                    leaf.get("launch_label")
                        .and_then(|title| title.as_str())
                        .map(str::trim)
                        .filter(|title| !title.is_empty())
                        .map(String::from)
                })
                .or_else(|| cwd.as_deref().map(path_title))
                .unwrap_or_else(|| id.to_string());
            menu.sessions.push(WorkspaceMenuSession {
                id: id.to_string(),
                kind: WorkspaceMenuSessionKind::Terminal,
                title,
                custom_title: custom_title.is_some(),
                cwd,
                project_root: project_root.clone(),
                project_title: base
                    .as_ref()
                    .and_then(|session| session.project_title.clone())
                    .or_else(|| project.map(|project| project.title.clone())),
                project_order: base
                    .as_ref()
                    .map(|session| session.project_order)
                    .or_else(|| project.map(|project| project.order))
                    .unwrap_or(u32::MAX),
                session_order: base
                    .as_ref()
                    .map(|session| session.session_order)
                    .unwrap_or_else(|| session_order.min(u32::MAX as usize) as u32),
                leaf_order: leaf_order.min(u32::MAX as usize) as u32,
                agent: None,
            });
        }
    }
}

fn collect_saved_terminal_leaves<'a>(
    pane: &'a serde_json::Value,
    leaves: &mut Vec<&'a serde_json::Value>,
) {
    if let Some(leaf) = pane.get("Leaf") {
        leaves.push(leaf);
        return;
    }
    if let Some(children) = pane
        .get("Split")
        .and_then(|split| split.get("children"))
        .and_then(|children| children.as_array())
    {
        for child in children {
            collect_saved_terminal_leaves(child, leaves);
        }
    }
}

/// 兼容尚未写入共享 menu 快照的旧 workspace.json。这里只负责一次性读旧结构；
/// 新版 PC 下一次 save_state 后，移动端就完全消费共享快照。
fn legacy_workspace_menu_from_value(value: &serde_json::Value) -> WorkspaceMenuSnapshot {
    let project_roots: Vec<String> = value["projects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|project| project.as_str().map(String::from))
        .collect();
    let projects = project_roots
        .iter()
        .enumerate()
        .map(|(order, root)| WorkspaceMenuProject {
            root: root.clone(),
            title: path_title(root),
            order: order.min(u32::MAX as usize) as u32,
        })
        .collect::<Vec<_>>();
    let mut sessions = Vec::new();
    for (session_order, session) in value["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let Some(acp) = session.get("acp").filter(|acp| !acp.is_null()) else {
            continue;
        };
        let Some(id) = acp.get("sid").and_then(|sid| sid.as_str()) else {
            continue;
        };
        let cwd = acp
            .get("cwd")
            .and_then(|cwd| cwd.as_str())
            .map(String::from);
        let project_root = cwd
            .as_deref()
            .and_then(|cwd| mobile_project_root(&project_roots, cwd));
        let project_order = project_root
            .as_deref()
            .and_then(|root| {
                project_roots
                    .iter()
                    .position(|candidate| candidate.trim_end_matches('/') == root)
            })
            .unwrap_or_else(|| project_roots.len().saturating_add(session_order));
        let project_title = project_root.as_deref().map(path_title);
        let title = session
            .get("custom_title")
            .and_then(|title| title.as_str())
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(String::from)
            .or_else(|| cwd.as_deref().map(path_title))
            .unwrap_or_else(|| id.to_string());
        let custom_title = session
            .get("custom_title")
            .and_then(|title| title.as_str())
            .is_some_and(|title| !title.trim().is_empty());
        sessions.push(WorkspaceMenuSession {
            id: id.to_string(),
            kind: WorkspaceMenuSessionKind::Acp,
            title,
            custom_title,
            cwd,
            project_root,
            project_title,
            project_order: project_order.min(u32::MAX as usize) as u32,
            session_order: session_order.min(u32::MAX as usize) as u32,
            leaf_order: 0,
            agent: acp
                .get("agent")
                .and_then(|agent| agent.as_str())
                .map(String::from),
        });
    }
    WorkspaceMenuSnapshot::current(projects, sessions)
}

fn path_title(path: &str) -> String {
    std::path::Path::new(path.trim_end_matches('/'))
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn mobile_project_root(projects: &[String], cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    let cwd = cwd.trim_end_matches('/');
    projects
        .iter()
        .map(|project| project.trim_end_matches('/'))
        .filter(|root| !root.is_empty() && (cwd == *root || cwd.starts_with(&format!("{root}/"))))
        .max_by_key(|root| root.len())
        .map(String::from)
}

impl MobileLifecycleHub {
    fn start() -> Arc<Self> {
        let (updates, _) = tokio::sync::broadcast::channel(128);
        let hub = Arc::new(Self {
            state: Mutex::new(MobileLifecycleState::default()),
            updates,
        });
        let weak = Arc::downgrade(&hub);
        std::thread::spawn(move || mobile_lifecycle_subscription(weak));
        hub
    }

    fn apply_snapshot(&self, sessions: Vec<DaemonSessionState>) {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap();
        let mut resolved = Vec::new();
        let ids: std::collections::HashSet<_> =
            sessions.iter().map(|session| session.id.clone()).collect();
        for session in &sessions {
            let had_unresolved_action = state.attention.has_unresolved_action(&session.id);
            let previous = state.sessions.get(&session.id).map(|old| old.phase);
            apply_daemon_transition(&mut state.attention, previous, session, now);
            if had_unresolved_action && !state.attention.has_unresolved_action(&session.id) {
                resolved.push(session.id.clone());
            }
        }
        let stale: Vec<_> = state
            .sessions
            .keys()
            .filter(|id| !ids.contains(*id))
            .cloned()
            .collect();
        for id in stale {
            if state.attention.has_unresolved_action(&id) {
                resolved.push(id.clone());
            }
            state.attention.remove_session(&id);
        }
        state.sessions = sessions
            .into_iter()
            .map(|session| (session.id.clone(), session))
            .collect();
        drop(state);
        let _ = self.updates.send(MobileLifecycleEvent::SessionsChanged);
        for session_id in resolved {
            let _ = self
                .updates
                .send(MobileLifecycleEvent::AttentionResolved(session_id));
        }
    }

    fn apply_update(&self, session: DaemonSessionState) {
        let mut state = self.state.lock().unwrap();
        let had_unresolved_action = state.attention.has_unresolved_action(&session.id);
        let previous = state.sessions.get(&session.id).map(|old| old.phase);
        let attention =
            apply_daemon_transition(&mut state.attention, previous, &session, Instant::now());
        let resolved = had_unresolved_action && !state.attention.has_unresolved_action(&session.id);
        let session_id = session.id.clone();
        state.sessions.insert(session.id.clone(), session);
        drop(state);
        let _ = self.updates.send(MobileLifecycleEvent::SessionsChanged);
        if let Some(item) = attention {
            let _ = self.updates.send(MobileLifecycleEvent::Attention(item));
        }
        if resolved {
            let _ = self
                .updates
                .send(MobileLifecycleEvent::AttentionResolved(session_id));
        }
    }

    fn mark_read(&self, session_id: &str) -> bool {
        let changed = self
            .state
            .lock()
            .unwrap()
            .attention
            .mark_read(session_id)
            .is_some();
        if changed {
            let _ = self.updates.send(MobileLifecycleEvent::SessionsChanged);
        }
        changed
    }

    fn refresh_from_daemon(&self) {
        if let Some(sessions) = load_daemon_session_snapshot() {
            self.apply_snapshot(sessions);
        }
    }

    fn summaries(&self) -> Vec<MobileSessionSummary> {
        let menu = mobile_workspace_menu();
        self.summaries_with_menu(&menu)
    }

    fn remove_session(&self, id: &str) {
        let mut state = self.state.lock().unwrap();
        state.sessions.remove(id);
        state.attention.remove_session(id);
        drop(state);
        let _ = self.updates.send(MobileLifecycleEvent::SessionsChanged);
    }

    fn summaries_with_menu(&self, menu: &WorkspaceMenuSnapshot) -> Vec<MobileSessionSummary> {
        let state = self.state.lock().unwrap();
        let mut summaries: Vec<_> = state
            .sessions
            .values()
            .filter_map(|session| mobile_summary_from_daemon(session, &menu, &state.attention))
            .collect();
        summaries.sort_by(|a, b| {
            a.project_order
                .cmp(&b.project_order)
                .then(a.session_order.cmp(&b.session_order))
                .then(a.leaf_order.cmp(&b.leaf_order))
                .then(a.title.cmp(&b.title))
                .then(a.id.cmp(&b.id))
        });
        summaries
    }
}

fn daemon_session_snapshot_from_response(
    response: &serde_json::Value,
) -> Option<Vec<DaemonSessionState>> {
    serde_json::from_value(response.get("states")?.clone()).ok()
}

fn load_daemon_session_snapshot() -> Option<Vec<DaemonSessionState>> {
    let mut stream = UnixStream::connect(sock_path()).ok()?;
    let timeout = Some(Duration::from_secs(2));
    stream.set_read_timeout(timeout).ok()?;
    stream.set_write_timeout(timeout).ok()?;
    writeln!(stream, "{}", serde_json::json!({ "op": "list" })).ok()?;

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    let response = serde_json::from_str(&line).ok()?;
    daemon_session_snapshot_from_response(&response)
}

async fn refreshed_mobile_summaries(hub: Arc<MobileLifecycleHub>) -> Vec<MobileSessionSummary> {
    let fallback = Arc::clone(&hub);
    tokio::task::spawn_blocking(move || {
        hub.refresh_from_daemon();
        hub.summaries()
    })
    .await
    .unwrap_or_else(|_| fallback.summaries())
}

fn mobile_lifecycle_subscription(hub: Weak<MobileLifecycleHub>) {
    while hub.strong_count() > 0 {
        if let Ok(mut stream) = UnixStream::connect(sock_path()) {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            if writeln!(stream, "{}", serde_json::json!({ "op": "subscribe" })).is_ok() {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                loop {
                    if hub.strong_count() == 0 {
                        return;
                    }
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(_) => {
                            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                                continue;
                            };
                            let Some(hub) = hub.upgrade() else {
                                return;
                            };
                            if let Some(sessions) = value.get("sessions") {
                                if let Ok(sessions) = serde_json::from_value(sessions.clone()) {
                                    hub.apply_snapshot(sessions);
                                }
                            } else if let Some(session) = value.get("session") {
                                if let Ok(session) = serde_json::from_value(session.clone()) {
                                    hub.apply_update(session);
                                }
                            }
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(_) => break,
                    }
                }
            }
        }
        if hub.strong_count() == 0 {
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// 移动端会话摘要。会话类型只认 PC 写入的共享菜单快照，不从命令或 id 猜测。
#[derive(serde::Serialize)]
struct MobileSessionSummary {
    id: String,
    kind: WorkspaceMenuSessionKind,
    title: String,
    phase: String,
    status: String,
    agent: String,
    cwd: Option<String>,
    project_root: Option<String>,
    project_title: Option<String>,
    project_order: u32,
    session_order: u32,
    leaf_order: u32,
    updated_at: i64,
    detail: Option<String>,
    unread: bool,
    attention: Option<AttentionItem>,
}

fn daemon_phase_name(phase: DaemonPhase) -> &'static str {
    match phase {
        DaemonPhase::Thinking => "thinking",
        DaemonPhase::ExecutingTool => "executing_tool",
        DaemonPhase::AwaitingApproval => "awaiting_approval",
        DaemonPhase::WaitingForUser => "waiting_for_user",
        DaemonPhase::Succeeded => "succeeded",
        DaemonPhase::Failed => "failed",
        DaemonPhase::Idle => "idle",
        DaemonPhase::Dead => "dead",
    }
}

fn mobile_status_name(phase: DaemonPhase, unread: bool) -> &'static str {
    match AgentStatus::from_daemon_phase(phase) {
        Some(AgentStatus::WaitingApproval) => "waiting_approval",
        Some(AgentStatus::NeedsAttention) => "needs_attention",
        Some(AgentStatus::Running) => "running",
        Some(AgentStatus::Done) if unread => "done",
        Some(AgentStatus::Done) | Some(AgentStatus::Idle) | None => "idle",
    }
}

fn agent_from_launch(launch: &str) -> &'static str {
    if launch.contains("claude") {
        "claude"
    } else if launch.contains("copilot") {
        "copilot"
    } else if launch.contains("codex") {
        "codex"
    } else if launch.contains("grok") {
        "grok"
    } else {
        "other"
    }
}

fn mobile_summary_from_daemon(
    session: &DaemonSessionState,
    menu: &WorkspaceMenuSnapshot,
    attention_store: &AttentionStore,
) -> Option<MobileSessionSummary> {
    // 会话类型来自 PC 的共享菜单快照，不再根据 id 前缀或启动命令猜测。
    let menu_session = menu.session(&session.id)?;
    let attention = attention_store.unread(&session.id).cloned();
    let unread = attention.is_some();
    let agent = menu_session
        .agent
        .clone()
        .or_else(|| {
            session
                .launch
                .as_deref()
                .map(|launch| agent_from_launch(launch).to_string())
        })
        .unwrap_or_else(|| "other".to_string());
    let title = if menu_session.kind == WorkspaceMenuSessionKind::Acp
        && !menu_session.custom_title
        && agent == "codex"
    {
        session
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| menu_session.title.clone())
    } else {
        menu_session.title.clone()
    };
    Some(MobileSessionSummary {
        id: session.id.clone(),
        kind: menu_session.kind,
        title,
        phase: daemon_phase_name(session.phase).to_string(),
        status: mobile_status_name(session.phase, unread).to_string(),
        agent,
        cwd: menu_session.cwd.clone().or_else(|| session.cwd.clone()),
        project_root: menu_session.project_root.clone(),
        project_title: menu_session.project_title.clone(),
        project_order: menu_session.project_order,
        session_order: menu_session.session_order,
        leaf_order: menu_session.leaf_order,
        updated_at: session.updated_at.min(i64::MAX as u64) as i64,
        detail: session.detail_line(),
        unread,
        attention,
    })
}

/// GET /acp/sessions - 列出所有 ACP 会话
async fn acp_sessions_handler(
    Query(q): Query<AuthQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if q.token != *state.token {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "invalid token"})),
        )
            .into_response();
    }

    let sessions = refreshed_mobile_summaries(Arc::clone(&state.mobile_lifecycle)).await;

    Json(serde_json::json!({
        "sessions": sessions
    }))
    .into_response()
}

const TERMINAL_ATTACH_TIMEOUT: Duration = Duration::from_secs(15);
const TERMINAL_DAEMON_TIMEOUT: Duration = Duration::from_secs(5);
const TERMINAL_WATCH_POLL: Duration = Duration::from_millis(100);
const MAX_TERMINAL_INPUT_BYTES: usize = 64 * 1024;

/// How often the terminal socket probes an otherwise silent viewer. A dead peer
/// is noticed after at most twice this, which is soon enough that the desktop
/// is not stuck at the phone's grid for long, while still being far cheaper
/// than the traffic a visible terminal generates anyway.
const TERMINAL_WS_PING_INTERVAL: Duration = Duration::from_secs(15);

/// 终端 socket 的探活判定。抽出来是因为这一小段状态机就是这个补丁的全部风险
/// 所在——判早了会把静看的用户踢下线，判晚了几何租约就一直不还——而端到端测
/// 一个 WebSocket 超时得起真守护进程加真客户端，最后测到的还是 tokio 的计时器。
#[derive(Debug, Default)]
struct TerminalLiveness {
    awaiting_pong: bool,
}

impl TerminalLiveness {
    /// 收到任何一帧都算活着，不限于 pong。
    fn observed_frame(&mut self) {
        self.awaiting_pong = false;
    }

    /// 返回 true 表示该断开：上一轮探测发出去之后一帧都没回来。
    fn should_disconnect_on_tick(&mut self) -> bool {
        if self.awaiting_pong {
            return true;
        }
        self.awaiting_pong = true;
        false
    }
}
const MAX_TERMINAL_REPLAY_BYTES: usize = 16 * 1024 * 1024;

#[derive(serde::Deserialize)]
#[serde(tag = "method")]
enum TerminalWsRequest {
    #[serde(rename = "attach")]
    Attach { params: TerminalGeometryParams },
    #[serde(rename = "input")]
    Input { params: TerminalInputParams },
    #[serde(rename = "resize")]
    Resize { params: TerminalGeometryParams },
    #[serde(rename = "ping")]
    Ping { params: PingParams },
}

#[derive(Clone, Copy, serde::Deserialize)]
struct TerminalGeometryParams {
    cols: u16,
    rows: u16,
    #[serde(default, rename = "cellWidth")]
    cell_width: u16,
    #[serde(default, rename = "cellHeight")]
    cell_height: u16,
}

impl TerminalGeometryParams {
    fn normalized(self) -> Result<Self, &'static str> {
        if self.cols == 0 || self.rows == 0 {
            return Err("cols/rows must be greater than zero");
        }
        Ok(Self {
            cols: self.cols.min(300),
            rows: self.rows.min(200),
            cell_width: self.cell_width.min(256),
            cell_height: self.cell_height.min(256),
        })
    }
}

#[derive(serde::Deserialize)]
struct TerminalInputParams {
    data: String,
}

enum TerminalFrame {
    Header {
        cols: u16,
        rows: u16,
        replay_len: usize,
    },
    Bytes(Vec<u8>),
    Closed,
    Error(String),
}

enum TerminalWatchCommand {
    Resize(TerminalGeometryParams),
}

async fn terminal_ws_handler(
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
    Query(q): Query<AuthQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if q.token != *state.token {
        return (StatusCode::FORBIDDEN, "token incorrect").into_response();
    }
    let exposed = mobile_workspace_menu()
        .session(&id)
        .is_some_and(|session| session.kind == WorkspaceMenuSessionKind::Terminal);
    if !exposed {
        return (StatusCode::NOT_FOUND, "terminal session not found").into_response();
    }
    ws.on_upgrade(move |socket| terminal_ws_pump(socket, state, id))
        .into_response()
}

async fn terminal_ws_pump(socket: WebSocket, state: AppState, id: String) {
    use futures::{SinkExt, StreamExt};

    let (mut ws_tx, mut ws_rx) = socket.split();
    let connected = serde_json::json!({
        "type": "terminalConnected",
        "sessionId": id,
        "writeEnabled": state.write_enabled,
    });
    if ws_tx
        .send(Message::Text(connected.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    let attach = tokio::time::timeout(TERMINAL_ATTACH_TIMEOUT, ws_rx.next()).await;
    let geometry = match attach {
        Ok(Some(Ok(Message::Text(text)))) => {
            match serde_json::from_str::<TerminalWsRequest>(&text) {
                Ok(TerminalWsRequest::Attach { params }) => params.normalized(),
                _ => Err("first terminal request must be attach"),
            }
        }
        Ok(Some(Ok(_))) => Err("first terminal request must be text"),
        Ok(Some(Err(_))) | Ok(None) => return,
        Err(_) => Err("terminal attach timed out"),
    };
    let geometry = match geometry {
        Ok(geometry) => geometry,
        Err(error) => {
            let _ = send_terminal_fatal_error(&mut ws_tx, error).await;
            return;
        }
    };

    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<TerminalFrame>(64);
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let (watch_command_tx, watch_command_rx) = std::sync::mpsc::channel();
    let (watch_ready_tx, watch_ready_rx) = tokio::sync::oneshot::channel();
    let watch_id = id.clone();
    let watch_task = tokio::task::spawn_blocking(move || {
        terminal_watch_and_forward(
            &watch_id,
            geometry,
            frame_tx,
            watch_command_rx,
            stop_rx,
            watch_ready_tx,
        )
    });

    // Geometry ownership and the first resize are part of the daemon watch
    // handshake. The daemon resizes its persistent grid before producing the
    // snapshot and keeps the same connection as the ownership lease.
    match tokio::time::timeout(TERMINAL_DAEMON_TIMEOUT, watch_ready_rx).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(error))) => {
            let _ = send_terminal_fatal_error(&mut ws_tx, &error).await;
            let _ = stop_tx.send(true);
            let _ = tokio::time::timeout(Duration::from_secs(1), watch_task).await;
            return;
        }
        Ok(Err(_)) => {
            let _ =
                send_terminal_fatal_error(&mut ws_tx, "terminal watch ended during attach").await;
            let _ = stop_tx.send(true);
            let _ = tokio::time::timeout(Duration::from_secs(1), watch_task).await;
            return;
        }
        Err(_) => {
            let _ = send_terminal_fatal_error(&mut ws_tx, "terminal watch attach timed out").await;
            let _ = stop_tx.send(true);
            let _ = tokio::time::timeout(Duration::from_secs(1), watch_task).await;
            return;
        }
    }

    // Server-driven liveness. Nothing on this socket is periodic — a viewer
    // that is merely watching sends no frames at all — so a dead phone (network
    // drop, wifi to cellular, app killed) would otherwise leave `ws_rx.next()`
    // pending forever. That matters because this connection holds the session's
    // remote geometry lease: until it closes, the desktop is pinned to the
    // phone's grid and refuses every resize. Probing with protocol-level pings
    // costs the client nothing, since WebSocket implementations answer them
    // automatically, and it means an idle-but-alive viewer is never dropped.
    let mut liveness = tokio::time::interval(TERMINAL_WS_PING_INTERVAL);
    liveness.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    liveness.tick().await; // the first tick resolves immediately
    let mut peer = TerminalLiveness::default();

    loop {
        tokio::select! {
            _ = liveness.tick() => {
                // Nothing came back from the previous probe: the peer is gone in
                // a way TCP has not reported. Drop it so the lease is freed.
                if peer.should_disconnect_on_tick() {
                    break;
                }
                if ws_tx.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            incoming = ws_rx.next() => {
                let Some(Ok(message)) = incoming else { break };
                peer.observed_frame();
                match message {
                    Message::Text(text) => {
                        let request = serde_json::from_str::<TerminalWsRequest>(&text);
                        match request {
                            Ok(TerminalWsRequest::Input { params }) => {
                                if !state.write_enabled {
                                    let _ = send_terminal_error(&mut ws_tx, "write not enabled").await;
                                    continue;
                                }
                                if params.data.is_empty() {
                                    let _ = send_terminal_error(&mut ws_tx, "terminal input must not be empty").await;
                                    continue;
                                }
                                if params.data.len() > MAX_TERMINAL_INPUT_BYTES {
                                    let _ = send_terminal_error(&mut ws_tx, "terminal input is too large").await;
                                    continue;
                                }
                                let input_id = id.clone();
                                match tokio::task::spawn_blocking(move || send_terminal_input(&input_id, &params.data)).await {
                                    Ok(Ok(())) => {}
                                    Ok(Err(error)) => {
                                        let _ = send_terminal_error(&mut ws_tx, &error).await;
                                    }
                                    Err(error) => {
                                        let _ = send_terminal_error(&mut ws_tx, &format!("failed to write terminal input: {error}")).await;
                                    }
                                }
                            }
                            Ok(TerminalWsRequest::Resize { params }) => {
                                let geometry = match params.normalized() {
                                    Ok(geometry) => geometry,
                                    Err(error) => {
                                        let _ = send_terminal_error(&mut ws_tx, error).await;
                                        continue;
                                    }
                                };
                                match watch_command_tx.send(TerminalWatchCommand::Resize(geometry)) {
                                    Ok(()) => {
                                        let response = serde_json::json!({
                                            "type": "terminalResized",
                                            "sessionId": id,
                                            "cols": geometry.cols,
                                            "rows": geometry.rows,
                                        });
                                        if ws_tx.send(Message::Text(response.to_string().into())).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(_) => {
                                        let _ = send_terminal_error(&mut ws_tx, "terminal geometry lease ended").await;
                                    }
                                }
                            }
                            Ok(TerminalWsRequest::Ping { params }) => {
                                let response = serde_json::json!({
                                    "type": "pong",
                                    "sentAtMs": params.sent_at_ms,
                                });
                                if ws_tx.send(Message::Text(response.to_string().into())).await.is_err() {
                                    break;
                                }
                            }
                            Ok(TerminalWsRequest::Attach { .. }) => {
                                let _ = send_terminal_error(&mut ws_tx, "terminal is already attached").await;
                            }
                            Err(_) => {
                                let _ = send_terminal_error(&mut ws_tx, "invalid terminal request").await;
                            }
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            frame = frame_rx.recv() => {
                let Some(frame) = frame else { break };
                let message = match frame {
                    TerminalFrame::Header { cols, rows, replay_len } => {
                        // 配色跟着每条连接下发：一部手机可以连多台设备，各台的
                        // 主题（深浅色 / 用户自选底色）不一样，客户端不能写死。
                        // 也必须跟 PC 对 OSC 11 的应答同源，否则 TUI 按查到的底色
                        // 挑灰度，手机上就是对比度不对。
                        // PC 中途改主题要等这条连接重连才生效——重连是常态（切前后台、
                        // 换网），不值得为此再加一条推送通道。
                        let ready = serde_json::json!({
                            "type": "terminalReady",
                            "sessionId": id,
                            "cols": cols,
                            "rows": rows,
                            "replayBytes": replay_len,
                            "writeEnabled": state.write_enabled,
                            "theme": crate::terminal_theme::load().to_wire(),
                        });
                        Message::Text(ready.to_string().into())
                    }
                    TerminalFrame::Bytes(bytes) => Message::Binary(bytes.into()),
                    TerminalFrame::Error(error) => {
                        let _ = send_terminal_fatal_error(&mut ws_tx, &error).await;
                        break;
                    }
                    TerminalFrame::Closed => {
                        let closed = serde_json::json!({
                            "type": "terminalClosed",
                            "sessionId": id,
                        });
                        let _ = ws_tx.send(Message::Text(closed.to_string().into())).await;
                        break;
                    }
                };
                if ws_tx.send(message).await.is_err() {
                    break;
                }
            }
        }
    }

    let _ = stop_tx.send(true);
    drop(frame_rx);
    let _ = tokio::time::timeout(Duration::from_secs(1), watch_task).await;
}

async fn send_terminal_error<S>(sink: &mut S, error: &str) -> Result<(), S::Error>
where
    S: futures::Sink<Message> + Unpin,
{
    use futures::SinkExt;
    let response = serde_json::json!({"type": "terminalError", "error": error});
    sink.send(Message::Text(response.to_string().into())).await
}

async fn send_terminal_fatal_error<S>(sink: &mut S, error: &str) -> Result<(), S::Error>
where
    S: futures::Sink<Message> + Unpin,
{
    use futures::SinkExt;
    let response = serde_json::json!({
        "type": "terminalError",
        "error": error,
        "fatal": true,
    });
    sink.send(Message::Text(response.to_string().into())).await
}

fn send_terminal_input(id: &str, data: &str) -> Result<(), String> {
    send_terminal_daemon_command(serde_json::json!({
        "op": "input",
        "id": id,
        "data": data,
    }))
}

fn send_terminal_daemon_command(request: serde_json::Value) -> Result<(), String> {
    let mut stream =
        UnixStream::connect(sock_path()).map_err(|error| format!("connect failed: {error}"))?;
    stream
        .set_read_timeout(Some(TERMINAL_DAEMON_TIMEOUT))
        .map_err(|error| format!("set timeout failed: {error}"))?;
    writeln!(stream, "{request}").map_err(|error| format!("write failed: {error}"))?;
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|error| format!("read failed: {error}"))?;
    let response: serde_json::Value = serde_json::from_str(&response)
        .map_err(|error| format!("invalid daemon response: {error}"))?;
    if response["ok"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err(response["err"]
            .as_str()
            .unwrap_or("terminal command failed")
            .to_string())
    }
}

fn terminal_watch_and_forward(
    id: &str,
    geometry: TerminalGeometryParams,
    tx: tokio::sync::mpsc::Sender<TerminalFrame>,
    commands: std::sync::mpsc::Receiver<TerminalWatchCommand>,
    stop: tokio::sync::watch::Receiver<bool>,
    ready: tokio::sync::oneshot::Sender<Result<(), String>>,
) {
    let mut ready = Some(ready);
    let result = terminal_watch_loop(id, geometry, &tx, &commands, &stop, &mut ready);
    match result {
        Ok(()) => {
            let _ = tx.blocking_send(TerminalFrame::Closed);
        }
        Err(error) if !*stop.borrow() => {
            if let Some(ready) = ready.take() {
                let _ = ready.send(Err(error.clone()));
            }
            let _ = tx.blocking_send(TerminalFrame::Error(error));
        }
        Err(_) => {}
    }
}

fn terminal_watch_loop(
    id: &str,
    geometry: TerminalGeometryParams,
    tx: &tokio::sync::mpsc::Sender<TerminalFrame>,
    commands: &std::sync::mpsc::Receiver<TerminalWatchCommand>,
    stop: &tokio::sync::watch::Receiver<bool>,
    ready: &mut Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
) -> Result<(), String> {
    let conn =
        UnixStream::connect(sock_path()).map_err(|error| format!("connect failed: {error}"))?;
    conn.set_read_timeout(Some(TERMINAL_DAEMON_TIMEOUT))
        .map_err(|error| format!("set timeout failed: {error}"))?;
    let mut writer = conn
        .try_clone()
        .map_err(|error| format!("clone failed: {error}"))?;
    writeln!(
        writer,
        "{}",
        serde_json::json!({
            "op": "watch",
            "id": id,
            "controls_geometry": true,
            "cols": geometry.cols,
            "rows": geometry.rows,
            "cell_w": geometry.cell_width,
            "cell_h": geometry.cell_height,
        })
    )
    .map_err(|error| format!("watch failed: {error}"))?;

    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| format!("watch header failed: {error}"))?;
    if line.is_empty() {
        return Err("terminal session not found".to_string());
    }
    let header: serde_json::Value =
        serde_json::from_str(&line).map_err(|error| format!("invalid watch header: {error}"))?;
    let cols = header["cols"].as_u64().unwrap_or(80).min(300) as u16;
    let rows = header["rows"].as_u64().unwrap_or(24).min(200) as u16;
    let replay_len = header["replay_len"].as_u64().unwrap_or(0) as usize;
    if replay_len > MAX_TERMINAL_REPLAY_BYTES {
        return Err("terminal snapshot is too large".to_string());
    }
    tx.blocking_send(TerminalFrame::Header {
        cols,
        rows,
        replay_len,
    })
    .map_err(|_| "terminal client disconnected".to_string())?;

    if replay_len > 0 {
        let mut snapshot = vec![0; replay_len];
        reader
            .read_exact(&mut snapshot)
            .map_err(|error| format!("terminal snapshot failed: {error}"))?;
        tx.blocking_send(TerminalFrame::Bytes(snapshot))
            .map_err(|_| "terminal client disconnected".to_string())?;
    }

    if let Some(ready) = ready.take() {
        let _ = ready.send(Ok(()));
    }

    reader
        .get_ref()
        .set_read_timeout(Some(TERMINAL_WATCH_POLL))
        .map_err(|error| format!("set watch timeout failed: {error}"))?;
    let mut buffer = [0_u8; 8192];
    loop {
        if *stop.borrow() {
            return Err("terminal watch stopped".to_string());
        }
        while let Ok(command) = commands.try_recv() {
            match command {
                TerminalWatchCommand::Resize(geometry) => {
                    write_terminal_resize_frame(&mut writer, geometry)?;
                }
            }
        }
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => tx
                .blocking_send(TerminalFrame::Bytes(buffer[..read].to_vec()))
                .map_err(|_| "terminal client disconnected".to_string())?,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(format!("terminal watch failed: {error}")),
        }
    }
}

fn write_terminal_resize_frame(
    writer: &mut UnixStream,
    geometry: TerminalGeometryParams,
) -> Result<(), String> {
    let mut payload = [0u8; 16];
    payload[0..4].copy_from_slice(&(u32::from(geometry.cols)).to_be_bytes());
    payload[4..8].copy_from_slice(&(u32::from(geometry.rows)).to_be_bytes());
    payload[8..12].copy_from_slice(&(u32::from(geometry.cell_width)).to_be_bytes());
    payload[12..16].copy_from_slice(&(u32::from(geometry.cell_height)).to_be_bytes());
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(1);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    writer
        .write_all(&frame)
        .map_err(|error| format!("terminal resize failed: {error}"))
}

/// WebSocket 消息类型（移动端 → 服务端）
#[derive(serde::Deserialize)]
#[serde(tag = "method")]
enum AcpWsRequest {
    #[serde(rename = "ping")]
    Ping { params: PingParams },
    #[serde(rename = "subscribe")]
    Subscribe { params: SubscribeParams },
    #[serde(rename = "loadHistory")]
    LoadHistory { params: LoadHistoryParams },
    #[serde(rename = "unsubscribe")]
    Unsubscribe,
    #[serde(rename = "sendMessage")]
    SendMessage { params: SendMessageParams },
    #[serde(rename = "cancelTurn")]
    CancelTurn { params: SessionActionParams },
    #[serde(rename = "setConfigOption")]
    SetConfigOption { params: ConfigOptionParams },
    #[serde(rename = "respondApproval")]
    RespondApproval { params: ApprovalParams },
    #[serde(rename = "chooseElicitation")]
    ChooseElicitation { params: ElicitationChoiceParams },
    #[serde(rename = "updateElicitationText")]
    UpdateElicitationText { params: ElicitationTextParams },
    #[serde(rename = "submitElicitation")]
    SubmitElicitation { params: SessionActionParams },
    #[serde(rename = "dismissElicitation")]
    DismissElicitation { params: SessionActionParams },
    #[serde(rename = "listSessions")]
    ListSessions,
    #[serde(rename = "listWorkspace")]
    ListWorkspace,
    #[serde(rename = "listSessionHistory")]
    ListSessionHistory { params: SessionHistoryParams },
    #[serde(rename = "createSession")]
    CreateSession { params: CreateSessionParams },
    #[serde(rename = "deleteSession")]
    DeleteSession { params: SessionActionParams },
    #[serde(rename = "markRead")]
    MarkRead {
        #[allow(dead_code)]
        params: MarkReadParams,
    },
}

#[derive(serde::Deserialize)]
struct SessionHistoryParams {
    #[serde(rename = "projectRoot")]
    project_root: String,
    #[serde(rename = "agentOptionId")]
    agent_option_id: String,
}

#[derive(serde::Deserialize)]
struct CreateSessionParams {
    #[serde(rename = "projectRoot")]
    project_root: String,
    #[serde(default, rename = "agentOptionId")]
    agent_option_id: Option<String>,
    #[serde(default, rename = "resumeId")]
    resume_id: Option<String>,
    /// `"acp"`（默认，向后兼容旧客户端）或 `"terminal"`。
    #[serde(default, rename = "kind")]
    kind: Option<String>,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

fn create_mobile_session(params: CreateSessionParams) -> Result<String, String> {
    match params.kind.as_deref() {
        Some("terminal") => create_mobile_terminal_session(params.project_root),
        _ => create_mobile_acp_session(params),
    }
}

fn create_mobile_terminal_session(project_root: String) -> Result<String, String> {
    let menu = mobile_workspace_menu();
    if !menu
        .projects
        .iter()
        .any(|project| project.root == project_root)
    {
        return Err("project is not in the Smelt workspace".to_string());
    }
    let id = format!("term-{}", uuid::Uuid::new_v4());
    crate::session_control::create_terminal_session(&id, &project_root)?;
    crate::session_control::remember_remote_terminal_session(
        crate::session_control::RemoteTerminalSession {
            id: id.clone(),
            cwd: project_root,
            title: String::new(),
            created_at: now_unix(),
        },
    );
    Ok(id)
}

fn create_mobile_acp_session(params: CreateSessionParams) -> Result<String, String> {
    let menu = mobile_workspace_menu();
    let project = menu
        .projects
        .iter()
        .find(|project| project.root == params.project_root)
        .ok_or_else(|| "project is not in the Smelt workspace".to_string())?;
    let agent_option_id = params
        .agent_option_id
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| "missing agentOptionId".to_string())?;
    let option = crate::session_control::find_agent_option(&agent_option_id)
        .ok_or_else(|| "unknown ACP agent or profile".to_string())?;
    let resume_id = params.resume_id.filter(|id| !id.trim().is_empty());
    let title = resume_id
        .as_deref()
        .and_then(|resume_id| {
            crate::session_control::list_history(&option, &project.root)
                .into_iter()
                .find(|session| session.resume_id == resume_id)
                .map(|session| session.title)
        })
        .unwrap_or_else(|| format!("{} conversation", option.label));
    let now = now_unix();
    let session = crate::session_control::RemoteAcpSession {
        id: format!("acp-{}", uuid::Uuid::new_v4()),
        cwd: project.root.clone(),
        title,
        agent_option_id: option.id,
        agent: option.kind,
        launch: option.launch,
        resume_id,
        created_at: now,
    };
    crate::session_control::remember_remote_session(session.clone());
    if let Err(error) = crate::session_control::create_acp_session(&session) {
        crate::session_control::forget_remote_session(&session.id);
        return Err(error);
    }
    Ok(session.id)
}

#[derive(serde::Deserialize)]
struct PingParams {
    #[serde(rename = "sentAtMs")]
    sent_at_ms: i64,
}

#[derive(serde::Deserialize)]
struct SubscribeParams {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(default, rename = "historySessionId")]
    history_session_id: Option<String>,
    #[serde(default, rename = "knownEntries")]
    known_entries: Option<usize>,
    #[serde(default, rename = "snapshotRevision")]
    snapshot_revision: Option<u64>,
    #[serde(default = "default_mobile_tail_limit", rename = "tailLimit")]
    tail_limit: usize,
}

fn default_mobile_tail_limit() -> usize {
    100
}

#[derive(serde::Deserialize)]
struct LoadHistoryParams {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "beforeOffset")]
    before_offset: usize,
    #[serde(default = "default_mobile_tail_limit")]
    limit: usize,
}

#[derive(serde::Deserialize)]
struct SendMessageParams {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(default, rename = "requestId")]
    request_id: Option<String>,
    content: String,
    #[serde(default)]
    images: Vec<crate::acp_chat::AcpImage>,
}

#[derive(serde::Deserialize)]
struct ConfigOptionParams {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "configId")]
    config_id: String,
    #[serde(rename = "valueId")]
    value_id: String,
}

#[derive(serde::Deserialize)]
struct ApprovalParams {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "toolCallId")]
    tool_call_id: String,
    #[serde(rename = "optionKey")]
    option_key: String,
    #[serde(rename = "customText")]
    custom_text: Option<String>,
}

#[derive(serde::Deserialize)]
struct ElicitationChoiceParams {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "fieldIndex")]
    field_index: usize,
    #[serde(rename = "optionIndex")]
    option_index: usize,
}

#[derive(serde::Deserialize)]
struct ElicitationTextParams {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "fieldIndex")]
    field_index: usize,
    value: String,
}

#[derive(serde::Deserialize)]
struct SessionActionParams {
    #[serde(rename = "sessionId")]
    session_id: String,
}

#[derive(serde::Deserialize)]
struct MarkReadParams {
    #[allow(dead_code)]
    #[serde(rename = "sessionId")]
    session_id: String,
}

/// GET /acp/ws - ACP WebSocket 连接（移动端用）
async fn acp_ws_handler(
    ws: WebSocketUpgrade,
    Query(q): Query<AuthQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if q.token != *state.token {
        return (StatusCode::FORBIDDEN, "token 不对").into_response();
    }
    ws.on_upgrade(move |socket| acp_ws_pump(socket, state))
        .into_response()
}

/// ACP WebSocket 主循环
async fn acp_ws_pump(socket: WebSocket, state: AppState) {
    use futures::stream::StreamExt;
    use tokio::sync::mpsc;

    let (mut ws_tx, mut ws_rx) = socket.split();
    let write_enabled = state.write_enabled;
    let mut lifecycle_rx = state.mobile_lifecycle.updates.subscribe();

    // 发送欢迎消息
    let welcome = serde_json::json!({
        "type": "connected",
        "writeEnabled": write_enabled,
    });
    if futures::SinkExt::send(&mut ws_tx, Message::Text(welcome.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // 用于从后台任务接收 smeltd 推送
    let (daemon_tx, mut daemon_rx) = mpsc::channel::<String>(64);

    // 每个 watcher 都有自己的停止信号；watch channel 一旦变成 true 不会自动复位，
    // 不能跨多次 subscribe 复用。
    let mut current_subscription: Option<(
        String,
        tokio::sync::watch::Sender<bool>,
        tokio::task::JoinHandle<()>,
    )> = None;

    loop {
        tokio::select! {
            // 接收来自移动端的消息
            msg = ws_rx.next() => {
                let Some(Ok(msg)) = msg else {
                    break;
                };

                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Close(_) => break,
                    _ => continue,
                };

                let Ok(req): Result<AcpWsRequest, _> = serde_json::from_str(&text) else {
                    let err = serde_json::json!({"type": "error", "error": "invalid request"});
                    let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(err.to_string().into())).await;
                    continue;
                };

                match req {
                    AcpWsRequest::Ping { params } => {
                        let resp = serde_json::json!({
                            "type": "pong",
                            "sentAtMs": params.sent_at_ms,
                        });
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::ListSessions => {
                        let sessions = refreshed_mobile_summaries(Arc::clone(
                            &state.mobile_lifecycle,
                        )).await;
                        let resp = serde_json::json!({
                            "type": "sessions",
                            "sessions": sessions,
                        });
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::ListWorkspace => {
                        let menu = mobile_workspace_menu();
                        let resp = serde_json::json!({
                            "type": "workspace",
                            "projects": crate::session_control::workspace_projects(&menu),
                            "agents": crate::session_control::agent_options(),
                        });
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::ListSessionHistory { params } => {
                        let response = tokio::task::spawn_blocking(move || {
                            let menu = mobile_workspace_menu();
                            if !menu.projects.iter().any(|project| project.root == params.project_root) {
                                return Err("project is not in the Smelt workspace".to_string());
                            }
                            let option = crate::session_control::find_agent_option(&params.agent_option_id)
                                .ok_or_else(|| "unknown ACP agent or profile".to_string())?;
                            Ok((params.project_root.clone(), params.agent_option_id.clone(), crate::session_control::list_history(&option, &params.project_root)))
                        }).await;
                        let resp = match response {
                            Ok(Ok((project_root, agent_option_id, sessions))) => serde_json::json!({
                                "type": "sessionHistory",
                                "projectRoot": project_root,
                                "agentOptionId": agent_option_id,
                                "sessions": sessions,
                            }),
                            Ok(Err(error)) => serde_json::json!({"type": "error", "error": error}),
                            Err(error) => serde_json::json!({"type": "error", "error": format!("failed to scan session history: {error}")}),
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::CreateSession { params } => {
                        if !write_enabled {
                            let resp = serde_json::json!({"type": "error", "error": "write not enabled"});
                            let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                            continue;
                        }
                        let response = tokio::task::spawn_blocking(move || create_mobile_session(params)).await;
                        let resp = match response {
                            Ok(Ok(id)) => serde_json::json!({"type": "sessionCreated", "sessionId": id}),
                            Ok(Err(error)) => serde_json::json!({"type": "error", "error": error}),
                            Err(error) => serde_json::json!({"type": "error", "error": format!("failed to create session: {error}")}),
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::DeleteSession { params } => {
                        if !write_enabled {
                            let resp = serde_json::json!({"type": "error", "error": "write not enabled"});
                            let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                            continue;
                        }
                        let id = params.session_id;
                        // 会话种类以共享菜单快照为准（跟列表展示同一份数据源），
                        // 终端走 PTY kill，ACP 走 acp_kill；查不到时按老行为当 ACP 处理。
                        let is_terminal = mobile_workspace_menu()
                            .session(&id)
                            .is_some_and(|session| session.kind == WorkspaceMenuSessionKind::Terminal);
                        let delete_id = id.clone();
                        let response = tokio::task::spawn_blocking(move || {
                            if is_terminal {
                                crate::session_control::delete_terminal_session(&delete_id)
                            } else {
                                crate::session_control::delete_acp_session(&delete_id)
                            }
                        }).await;
                        let resp = match response {
                            Ok(Ok(())) => {
                                state.mobile_lifecycle.remove_session(&id);
                                serde_json::json!({"type": "sessionDeleted", "sessionId": id})
                            }
                            Ok(Err(error)) => serde_json::json!({"type": "error", "error": error}),
                            Err(error) => serde_json::json!({"type": "error", "error": format!("failed to delete session: {error}")}),
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::Subscribe { params } => {
                        // 停止旧的订阅
                        if let Some((_, stop_tx, handle)) = current_subscription.take() {
                            let _ = stop_tx.send(true);
                            handle.abort();
                        }

                        // 启动新的订阅
                        let session_id = params.session_id.clone();
                        let history_session_id = params.history_session_id.clone();
                        let known_entries = params.known_entries;
                        let snapshot_revision = params.snapshot_revision;
                        let tail_limit = params.tail_limit.clamp(1, 500);
                        let tx = daemon_tx.clone();
                        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

                        let handle = tokio::task::spawn_blocking(move || {
                            acp_watch_loop(
                                &session_id,
                                history_session_id.as_deref(),
                                known_entries,
                                snapshot_revision,
                                tail_limit,
                                tx,
                                stop_rx,
                            );
                        });

                        current_subscription =
                            Some((params.session_id.clone(), stop_tx, handle));

                        let resp = serde_json::json!({
                            "type": "subscribed",
                            "sessionId": params.session_id,
                        });
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::LoadHistory { params } => {
                        let result = tokio::task::spawn_blocking(move || {
                            read_acp_history(
                                &params.session_id,
                                params.before_offset,
                                params.limit.clamp(1, 500),
                            )
                        }).await;
                        let response = match result {
                            Ok(Ok(line)) => line,
                            Ok(Err(error)) => serde_json::json!({"type": "error", "error": error}).to_string(),
                            Err(error) => serde_json::json!({
                                "type": "error",
                                "error": format!("failed to load history: {error}"),
                            }).to_string(),
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(response.into())).await;
                    }
                    AcpWsRequest::Unsubscribe => {
                        if let Some((_, stop_tx, handle)) = current_subscription.take() {
                            let _ = stop_tx.send(true);
                            handle.abort();
                        }
                        let resp = serde_json::json!({"type": "unsubscribed"});
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::SendMessage { params } => {
                        let request_id = params.request_id.clone();
                        if !write_enabled {
                            let err = serde_json::json!({
                                "type": "error",
                                "error": "write not enabled",
                                "requestId": request_id,
                            });
                            let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(err.to_string().into())).await;
                            continue;
                        }

                        let session_id = params.session_id.clone();
                        let content = params.content.clone();
                        let images = params.images.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            send_acp_message(&session_id, &content, images)
                        }).await;

                        let resp = match result {
                            Ok(Ok(())) => serde_json::json!({
                                "type": "messageSent",
                                "ok": true,
                                "requestId": request_id,
                            }),
                            Ok(Err(error)) => serde_json::json!({
                                "type": "error",
                                "error": error,
                                "requestId": request_id,
                            }),
                            Err(error) => serde_json::json!({
                                "type": "error",
                                "error": format!("failed to dispatch message: {error}"),
                                "requestId": request_id,
                            }),
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::CancelTurn { params } => {
                        let resp = dispatch_mobile_action(
                            write_enabled,
                            params.session_id,
                            serde_json::json!("Cancel"),
                            "turn cancellation",
                        ).await;
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::SetConfigOption { params } => {
                        let action = serde_json::json!({
                            "SetConfigOption": {
                                "config_id": params.config_id,
                                "value_id": params.value_id,
                            }
                        });
                        let resp = dispatch_mobile_action(
                            write_enabled,
                            params.session_id,
                            action,
                            "configuration update",
                        ).await;
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::RespondApproval { params } => {
                        if !write_enabled {
                            let err = serde_json::json!({"type": "error", "error": "write not enabled"});
                            let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(err.to_string().into())).await;
                            continue;
                        }

                        let session_id = params.session_id.clone();
                        let option_key = params.option_key.clone();
                        let custom_text = params.custom_text.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            respond_acp_approval(
                                &session_id,
                                &params.tool_call_id,
                                &option_key,
                                custom_text.as_deref(),
                            )
                        }).await;

                        let resp = match result {
                            Ok(Ok(())) => serde_json::json!({"type": "approvalResponded", "ok": true}),
                            Ok(Err(error)) => serde_json::json!({"type": "error", "error": error}),
                            Err(error) => serde_json::json!({
                                "type": "error",
                                "error": format!("failed to dispatch approval: {error}"),
                            }),
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::ChooseElicitation { params } => {
                        let action = serde_json::json!({
                            "ElicitationChoose": {
                                "field_ix": params.field_index,
                                "opt_ix": params.option_index,
                            }
                        });
                        let resp = dispatch_mobile_action(
                            write_enabled,
                            params.session_id,
                            action,
                            "elicitation choice",
                        ).await;
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::UpdateElicitationText { params } => {
                        let action = serde_json::json!({
                            "ElicitationText": {
                                "field_ix": params.field_index,
                                "value": params.value,
                            }
                        });
                        let resp = dispatch_mobile_action(
                            write_enabled,
                            params.session_id,
                            action,
                            "elicitation text",
                        ).await;
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::SubmitElicitation { params } => {
                        let resp = dispatch_mobile_action(
                            write_enabled,
                            params.session_id,
                            serde_json::json!("ElicitationSubmit"),
                            "elicitation submission",
                        ).await;
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::DismissElicitation { params } => {
                        let resp = dispatch_mobile_action(
                            write_enabled,
                            params.session_id,
                            serde_json::json!("ElicitationDismiss"),
                            "elicitation dismissal",
                        ).await;
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::MarkRead { params } => {
                        let changed = state.mobile_lifecycle.mark_read(&params.session_id);
                        let resp = serde_json::json!({
                            "type": "markedRead",
                            "ok": true,
                            "changed": changed,
                            "sessionId": params.session_id,
                        });
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                }
            }
            lifecycle = lifecycle_rx.recv() => {
                match lifecycle {
                    Ok(MobileLifecycleEvent::SessionsChanged) => {
                        let resp = serde_json::json!({
                            "type": "sessions",
                            "sessions": state.mobile_lifecycle.summaries(),
                        });
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    Ok(MobileLifecycleEvent::Attention(item)) => {
                        let resp = serde_json::json!({"type": "attention", "item": item});
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    Ok(MobileLifecycleEvent::AttentionResolved(session_id)) => {
                        let resp = serde_json::json!({
                            "type": "attentionResolved",
                            "sessionId": session_id,
                        });
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let resp = serde_json::json!({
                            "type": "sessions",
                            "sessions": state.mobile_lifecycle.summaries(),
                        });
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            // 接收来自 smeltd 的推送，直接透传原始格式
            Some(line) = daemon_rx.recv() => {
                // 直接转发 smeltd 的原始 JSON（与 PC GUI 一致）
                let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(line.into())).await;
            }
        }
    }

    // 清理
    if let Some((_, stop_tx, handle)) = current_subscription.take() {
        let _ = stop_tx.send(true);
        handle.abort();
    }
}

async fn dispatch_mobile_action(
    write_enabled: bool,
    session_id: String,
    action: serde_json::Value,
    description: &'static str,
) -> serde_json::Value {
    if !write_enabled {
        return serde_json::json!({"type": "error", "error": "write not enabled"});
    }
    match tokio::task::spawn_blocking(move || send_acp_action(&session_id, action)).await {
        Ok(Ok(())) => serde_json::json!({"type": "actionCompleted", "ok": true}),
        Ok(Err(error)) => serde_json::json!({"type": "error", "error": error}),
        Err(error) => serde_json::json!({
            "type": "error",
            "error": format!("failed to dispatch {description}: {error}"),
        }),
    }
}

/// 后台任务：监听 smeltd 的 acp_watch 推送
fn acp_watch_loop(
    session_id: &str,
    history_session_id: Option<&str>,
    known_entries: Option<usize>,
    snapshot_revision: Option<u64>,
    tail_limit: usize,
    tx: tokio::sync::mpsc::Sender<String>,
    stop: tokio::sync::watch::Receiver<bool>,
) {
    let Ok(mut stream) = UnixStream::connect(sock_path()) else {
        return;
    };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(100)));

    // 发送 acp_watch 请求
    let req = serde_json::json!({
        "op": "acp_watch",
        "id": session_id,
        "history_session_id": history_session_id,
        "known_entries": known_entries,
        "snapshot_revision": snapshot_revision,
        "tail_limit": tail_limit,
    });
    if writeln!(stream, "{}", req).is_err() {
        return;
    }

    let mut reader = BufReader::new(stream);
    let mut line = String::new();

    loop {
        // 检查是否需要停止
        if *stop.borrow() {
            break;
        }

        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    // 发送到 WebSocket 任务
                    if tx
                        .blocking_send(tag_snapshot_line(trimmed, session_id))
                        .is_err()
                    {
                        break;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // 超时，继续循环检查 stop
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }
}

fn read_acp_history(
    session_id: &str,
    before_offset: usize,
    limit: usize,
) -> Result<String, String> {
    let mut stream =
        UnixStream::connect(sock_path()).map_err(|e| format!("connect failed: {e}"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|e| format!("set timeout failed: {e}"))?;
    let request = serde_json::json!({
        "op": "acp_snapshot",
        "id": session_id,
        "before": before_offset,
        "limit": limit,
    });
    writeln!(stream, "{request}").map_err(|e| format!("write failed: {e}"))?;
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|e| format!("read failed: {e}"))?;
    if response.trim().is_empty() {
        Err("session not found".to_string())
    } else {
        Ok(tag_snapshot_line(response.trim(), session_id))
    }
}

fn tag_snapshot_line(line: &str, session_id: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(line) else {
        return line.to_string();
    };
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "sessionId".to_string(),
            serde_json::Value::String(session_id.to_string()),
        );
    }
    value.to_string()
}

fn send_acp_action(session_id: &str, action: serde_json::Value) -> Result<(), String> {
    let mut stream =
        UnixStream::connect(sock_path()).map_err(|e| format!("connect failed: {e}"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|e| format!("set timeout failed: {e}"))?;

    let req = serde_json::json!({
        "op": "acp_action",
        "id": session_id,
        "action": action,
    });
    writeln!(stream, "{req}").map_err(|e| format!("write failed: {e}"))?;

    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|e| format!("read failed: {e}"))?;
    let response: serde_json::Value =
        serde_json::from_str(&response).map_err(|e| format!("invalid response: {e}"))?;
    if response["ok"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err(response["error"]
            .as_str()
            .unwrap_or("ACP action failed")
            .to_string())
    }
}

/// 发送 ACP 消息，不占用或替换 PC GUI 的 control client。
fn send_acp_message(
    session_id: &str,
    content: &str,
    images: Vec<crate::acp_chat::AcpImage>,
) -> Result<(), String> {
    send_acp_action(
        session_id,
        serde_json::json!({
            "Prompt": {
                "text": content,
                "images": images,
            }
        }),
    )
}

/// 响应 ACP 审批请求
fn respond_acp_approval(
    session_id: &str,
    tool_call_id: &str,
    option_key: &str,
    _custom_text: Option<&str>,
) -> Result<(), String> {
    send_acp_action(
        session_id,
        serde_json::json!({
            "PermissionSelect": {
                "tool_call_id": tool_call_id,
                "option_id": option_key,
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 静看终端的用户一帧都不发。要是把"没流量"直接当成掉线，正常用户会被踢，
    /// 所以探测必须先发出去、给对面一整个周期回话的机会。
    #[test]
    fn a_silent_but_answering_viewer_is_never_dropped() {
        let mut peer = TerminalLiveness::default();

        for _ in 0..100 {
            assert!(!peer.should_disconnect_on_tick(), "探测周期本身不该断连");
            peer.observed_frame(); // 客户端自动回的 pong
        }
    }

    /// 手机断网时 TCP 可能一直不报错（切蜂窝、被前台杀掉），`ws_rx` 就永远挂着。
    /// 这条连接握着几何租约，不断开的话桌面端会被永久钉在手机的网格上、拒绝一切
    /// resize，只能杀会话或重启守护进程才能恢复。
    #[test]
    fn a_peer_that_stops_answering_is_dropped_on_the_next_tick() {
        let mut peer = TerminalLiveness::default();

        assert!(!peer.should_disconnect_on_tick(), "第一轮只发探测");
        assert!(peer.should_disconnect_on_tick(), "探测无人应答就该断开");
    }

    /// 输入、resize 这些帧同样能证明对面活着——不能因为它没回 pong 就踢掉一个
    /// 正在打字的用户。
    #[test]
    fn any_inbound_frame_counts_as_proof_of_life() {
        let mut peer = TerminalLiveness::default();

        assert!(!peer.should_disconnect_on_tick());
        peer.observed_frame(); // 比如一次按键
        assert!(
            !peer.should_disconnect_on_tick(),
            "收到过帧就该重新给一个周期"
        );
    }

    #[test]
    fn terminal_requests_preserve_control_input_and_bound_geometry() {
        let input: TerminalWsRequest = serde_json::from_value(serde_json::json!({
            "method": "input",
            "params": {"data": "\u{1b}[A\u{3}"}
        }))
        .unwrap();
        match input {
            TerminalWsRequest::Input { params } => {
                assert_eq!(params.data.as_bytes(), b"\x1b[A\x03");
            }
            _ => panic!("expected terminal input"),
        }

        let resize: TerminalWsRequest = serde_json::from_value(serde_json::json!({
            "method": "resize",
            "params": {
                "cols": 900,
                "rows": 400,
                "cellWidth": 512,
                "cellHeight": 1024
            }
        }))
        .unwrap();
        match resize {
            TerminalWsRequest::Resize { params } => {
                let params = params.normalized().unwrap();
                assert_eq!(params.cols, 300);
                assert_eq!(params.rows, 200);
                assert_eq!(params.cell_width, 256);
                assert_eq!(params.cell_height, 256);
            }
            _ => panic!("expected terminal resize"),
        }

        let zero = TerminalGeometryParams {
            cols: 0,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        };
        assert!(zero.normalized().is_err());
    }

    #[test]
    fn terminal_resize_uses_the_geometry_lease_frame() {
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let geometry = TerminalGeometryParams {
            cols: 49,
            rows: 47,
            cell_width: 8,
            cell_height: 15,
        };
        write_terminal_resize_frame(&mut writer, geometry).unwrap();

        let mut frame = [0u8; 21];
        reader.read_exact(&mut frame).unwrap();
        assert_eq!(frame[0], 1);
        assert_eq!(u32::from_be_bytes(frame[1..5].try_into().unwrap()), 16);
        let values: Vec<u32> = frame[5..]
            .chunks_exact(4)
            .map(|bytes| u32::from_be_bytes(bytes.try_into().unwrap()))
            .collect();
        assert_eq!(values, vec![49, 47, 8, 15]);
    }

    #[test]
    fn mobile_requests_preserve_images_and_session_controls() {
        let ping: AcpWsRequest = serde_json::from_value(serde_json::json!({
            "method": "ping",
            "params": {"sentAtMs": 12345}
        }))
        .unwrap();
        match ping {
            AcpWsRequest::Ping { params } => assert_eq!(params.sent_at_ms, 12345),
            _ => panic!("expected ping"),
        }

        let subscribe: AcpWsRequest = serde_json::from_value(serde_json::json!({
            "method": "subscribe",
            "params": {
                "sessionId": "session-1",
                "historySessionId": "history-1",
                "knownEntries": 240,
                "snapshotRevision": 9,
                "tailLimit": 80
            }
        }))
        .unwrap();
        match subscribe {
            AcpWsRequest::Subscribe { params } => {
                assert_eq!(params.history_session_id.as_deref(), Some("history-1"));
                assert_eq!(params.known_entries, Some(240));
                assert_eq!(params.snapshot_revision, Some(9));
                assert_eq!(params.tail_limit, 80);
            }
            _ => panic!("expected subscribe"),
        }

        let request: AcpWsRequest = serde_json::from_value(serde_json::json!({
            "method": "sendMessage",
            "params": {
                "sessionId": "session-1",
                "requestId": "request-1",
                "content": "inspect this",
                "images": [{"mime": "image/png", "data_b64": "aW1hZ2U="}]
            }
        }))
        .unwrap();
        match request {
            AcpWsRequest::SendMessage { params } => {
                assert_eq!(params.session_id, "session-1");
                assert_eq!(params.request_id.as_deref(), Some("request-1"));
                assert_eq!(params.images.len(), 1);
                assert_eq!(params.images[0].mime, "image/png");
            }
            _ => panic!("expected sendMessage"),
        }

        let cancel: AcpWsRequest = serde_json::from_value(serde_json::json!({
            "method": "cancelTurn",
            "params": {"sessionId": "session-1"}
        }))
        .unwrap();
        assert!(matches!(cancel, AcpWsRequest::CancelTurn { .. }));

        let config: AcpWsRequest = serde_json::from_value(serde_json::json!({
            "method": "setConfigOption",
            "params": {
                "sessionId": "session-1",
                "configId": "mode",
                "valueId": "full"
            }
        }))
        .unwrap();
        match config {
            AcpWsRequest::SetConfigOption { params } => {
                assert_eq!(params.config_id, "mode");
                assert_eq!(params.value_id, "full");
            }
            _ => panic!("expected setConfigOption"),
        }

        let history: AcpWsRequest = serde_json::from_value(serde_json::json!({
            "method": "listSessionHistory",
            "params": {
                "projectRoot": "/repo/smelt",
                "agentOptionId": "profile:quant"
            }
        }))
        .unwrap();
        match history {
            AcpWsRequest::ListSessionHistory { params } => {
                assert_eq!(params.project_root, "/repo/smelt");
                assert_eq!(params.agent_option_id, "profile:quant");
            }
            _ => panic!("expected listSessionHistory"),
        }

        let create: AcpWsRequest = serde_json::from_value(serde_json::json!({
            "method": "createSession",
            "params": {
                "projectRoot": "/repo/smelt",
                "agentOptionId": "codex",
                "resumeId": "history-1"
            }
        }))
        .unwrap();
        match create {
            AcpWsRequest::CreateSession { params } => {
                assert_eq!(params.agent_option_id.as_deref(), Some("codex"));
                assert_eq!(params.resume_id.as_deref(), Some("history-1"));
                assert_eq!(params.kind, None);
            }
            _ => panic!("expected createSession"),
        }

        let create_terminal: AcpWsRequest = serde_json::from_value(serde_json::json!({
            "method": "createSession",
            "params": {
                "projectRoot": "/repo/smelt",
                "kind": "terminal"
            }
        }))
        .unwrap();
        match create_terminal {
            AcpWsRequest::CreateSession { params } => {
                assert_eq!(params.project_root, "/repo/smelt");
                assert_eq!(params.agent_option_id, None);
                assert_eq!(params.kind.as_deref(), Some("terminal"));
            }
            _ => panic!("expected createSession"),
        }

        let delete: AcpWsRequest = serde_json::from_value(serde_json::json!({
            "method": "deleteSession",
            "params": {"sessionId": "acp-1"}
        }))
        .unwrap();
        assert!(matches!(delete, AcpWsRequest::DeleteSession { .. }));
    }

    fn mobile_daemon_state(phase: DaemonPhase) -> DaemonSessionState {
        DaemonSessionState {
            id: "acp-session-mobile".into(),
            phase,
            title: Some("修复移动端项目列表".into()),
            launch: Some("codex app-server".into()),
            cwd: Some("/tmp/mobile-project".into()),
            updated_at: 42,
            structured_events: true,
            ..Default::default()
        }
    }

    fn mobile_menu_for_test() -> WorkspaceMenuSnapshot {
        WorkspaceMenuSnapshot::current(
            vec![WorkspaceMenuProject {
                root: "/tmp/mobile-project".into(),
                title: "mobile-project".into(),
                order: 0,
            }],
            vec![WorkspaceMenuSession {
                id: "acp-session-mobile".into(),
                kind: WorkspaceMenuSessionKind::Acp,
                title: "修复移动端项目列表".into(),
                custom_title: false,
                cwd: Some("/tmp/mobile-project".into()),
                project_root: Some("/tmp/mobile-project".into()),
                project_title: Some("mobile-project".into()),
                project_order: 0,
                session_order: 2,
                leaf_order: 0,
                agent: Some("codex".into()),
            }],
        )
    }

    #[test]
    fn mobile_summary_includes_terminal_agent_cli_sessions() {
        let attention = AttentionStore::default();
        let mut session = mobile_daemon_state(DaemonPhase::Thinking);
        session.id = "terminal-codex-cli".into();
        let menu = WorkspaceMenuSnapshot::current(
            vec![],
            vec![WorkspaceMenuSession {
                id: session.id.clone(),
                kind: WorkspaceMenuSessionKind::Terminal,
                title: "Codex CLI".into(),
                custom_title: false,
                cwd: session.cwd.clone(),
                project_root: None,
                project_title: None,
                project_order: 0,
                session_order: 0,
                leaf_order: 1,
                agent: Some("codex".into()),
            }],
        );

        let summary = mobile_summary_from_daemon(&session, &menu, &attention).unwrap();
        assert_eq!(summary.kind, WorkspaceMenuSessionKind::Terminal);
        assert_eq!(summary.title, "Codex CLI");
        assert_eq!(summary.leaf_order, 1);
    }

    #[test]
    fn mobile_summary_preserves_pc_manual_title() {
        let mut menu = mobile_menu_for_test();
        menu.sessions[0].title = "用户重命名".into();
        menu.sessions[0].custom_title = true;
        let session = mobile_daemon_state(DaemonPhase::Idle);

        let summary =
            mobile_summary_from_daemon(&session, &menu, &AttentionStore::default()).unwrap();
        assert_eq!(summary.title, "用户重命名");
    }

    fn mobile_lifecycle_hub_for_test() -> MobileLifecycleHub {
        let (updates, _) = tokio::sync::broadcast::channel(16);
        MobileLifecycleHub {
            state: Mutex::new(MobileLifecycleState::default()),
            updates,
        }
    }

    #[test]
    fn mobile_workspace_menu_uses_pc_snapshot_verbatim() {
        let value = serde_json::json!({
            "projects": ["legacy-is-ignored"],
            "menu": {
                "version": 1,
                "projects": [{"root": "/repo/two", "title": "repo · two", "order": 0}],
                "sessions": [{
                    "id": "stable-acp-id",
                    "kind": "acp",
                    "title": "PC display title",
                    "custom_title": true,
                    "cwd": "/repo/two",
                    "project_root": "/repo/two",
                    "project_title": "repo · two",
                    "project_order": 0,
                    "session_order": 3,
                    "agent": "codex"
                }]
            }
        });

        let menu = workspace_menu_from_value(&value);
        let session = menu.acp_session("stable-acp-id").unwrap();
        assert_eq!(session.title, "PC display title");
        assert_eq!(session.project_title.as_deref(), Some("repo · two"));
        assert_eq!(session.session_order, 3);
        assert_eq!(session.leaf_order, 0);
    }

    #[test]
    fn version_one_workspace_menu_recovers_all_split_terminal_leaves() {
        let value = serde_json::json!({
            "projects": ["/repo"],
            "sessions": [{
                "layout": {
                    "Split": {
                        "axis": "H",
                        "children": [
                            {"Leaf": {"id": "terminal-left", "cwd": "/repo", "custom_title": "Tests"}},
                            {"Leaf": {"id": "terminal-right", "cwd": "/repo"}}
                        ]
                    }
                },
                "active": 1,
                "acp": null
            }],
            "menu": {
                "version": 1,
                "projects": [{"root": "/repo", "title": "repo", "order": 0}],
                "sessions": [{
                    "id": "terminal-right",
                    "kind": "terminal",
                    "title": "repo",
                    "cwd": "/repo",
                    "project_root": "/repo",
                    "project_title": "repo",
                    "project_order": 0,
                    "session_order": 0
                }]
            }
        });

        let menu = workspace_menu_from_value(&value);
        assert_eq!(menu.sessions.len(), 2);
        let left = menu.session("terminal-left").unwrap();
        assert_eq!(left.title, "Tests");
        assert_eq!(left.leaf_order, 0);
        let right = menu.session("terminal-right").unwrap();
        assert_eq!(right.leaf_order, 1);
    }

    #[test]
    fn mobile_lifecycle_keeps_all_split_terminal_leaves_in_order() {
        let hub = mobile_lifecycle_hub_for_test();
        let mut left = mobile_daemon_state(DaemonPhase::Idle);
        left.id = "terminal-left".into();
        let mut right = mobile_daemon_state(DaemonPhase::Idle);
        right.id = "terminal-right".into();
        hub.apply_snapshot(vec![right, left]);

        let menu = WorkspaceMenuSnapshot::current(
            vec![],
            vec![
                WorkspaceMenuSession {
                    id: "terminal-left".into(),
                    kind: WorkspaceMenuSessionKind::Terminal,
                    title: "Left pane".into(),
                    custom_title: false,
                    cwd: Some("/tmp/mobile-project".into()),
                    project_root: Some("/tmp/mobile-project".into()),
                    project_title: Some("mobile-project".into()),
                    project_order: 0,
                    session_order: 4,
                    leaf_order: 0,
                    agent: None,
                },
                WorkspaceMenuSession {
                    id: "terminal-right".into(),
                    kind: WorkspaceMenuSessionKind::Terminal,
                    title: "Right pane".into(),
                    custom_title: false,
                    cwd: Some("/tmp/mobile-project".into()),
                    project_root: Some("/tmp/mobile-project".into()),
                    project_title: Some("mobile-project".into()),
                    project_order: 0,
                    session_order: 4,
                    leaf_order: 1,
                    agent: None,
                },
            ],
        );

        let summaries = hub.summaries_with_menu(&menu);
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].id, "terminal-left");
        assert_eq!(summaries[0].leaf_order, 0);
        assert_eq!(summaries[1].id, "terminal-right");
        assert_eq!(summaries[1].leaf_order, 1);
    }

    #[test]
    fn daemon_list_response_restores_new_sessions_for_mobile_refresh() {
        let response = serde_json::json!({
            "sessions": ["existing", "new-terminal"],
            "states": [
                {
                    "id": "existing",
                    "phase": "idle",
                    "structured_events": false
                },
                {
                    "id": "new-terminal",
                    "cwd": "/repo",
                    "phase": "idle",
                    "structured_events": false
                }
            ]
        });

        let sessions = daemon_session_snapshot_from_response(&response).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[1].id, "new-terminal");
        assert_eq!(sessions[1].cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn mobile_project_root_matches_deepest_pc_project() {
        let projects = vec!["/repo".into(), "/repo/packages/app".into()];
        assert_eq!(
            mobile_project_root(&projects, "/repo/packages/app/src"),
            Some("/repo/packages/app".into())
        );
        assert_eq!(mobile_project_root(&projects, "/repo-other"), None);
    }

    #[test]
    fn mobile_lifecycle_marks_completed_attention_read_without_changing_phase() {
        let hub = mobile_lifecycle_hub_for_test();
        hub.apply_snapshot(vec![mobile_daemon_state(DaemonPhase::Thinking)]);
        hub.apply_update(mobile_daemon_state(DaemonPhase::Succeeded));

        let before = hub
            .summaries_with_menu(&mobile_menu_for_test())
            .pop()
            .unwrap();
        assert_eq!(before.phase, "succeeded");
        assert_eq!(before.status, "done");
        assert!(before.unread);
        assert_eq!(
            before.attention.unwrap().kind,
            crate::attention::AttentionKind::Success
        );

        assert!(hub.mark_read("acp-session-mobile"));
        let after = hub
            .summaries_with_menu(&mobile_menu_for_test())
            .pop()
            .unwrap();
        assert_eq!(after.phase, "succeeded");
        assert_eq!(after.status, "idle");
        assert!(!after.unread);
        assert!(after.attention.is_none());
    }

    #[test]
    fn mobile_lifecycle_keeps_action_status_after_mark_read() {
        let hub = mobile_lifecycle_hub_for_test();
        hub.apply_snapshot(vec![mobile_daemon_state(DaemonPhase::Thinking)]);
        hub.apply_update(mobile_daemon_state(DaemonPhase::AwaitingApproval));

        assert!(hub.mark_read("acp-session-mobile"));
        let summary = hub
            .summaries_with_menu(&mobile_menu_for_test())
            .pop()
            .unwrap();
        assert_eq!(summary.status, "waiting_approval");
        assert!(!summary.unread);
    }

    #[test]
    fn mobile_lifecycle_broadcasts_when_an_action_is_resolved_elsewhere() {
        let hub = mobile_lifecycle_hub_for_test();
        let mut updates = hub.updates.subscribe();
        hub.apply_snapshot(vec![mobile_daemon_state(DaemonPhase::Thinking)]);
        hub.apply_update(mobile_daemon_state(DaemonPhase::AwaitingApproval));
        hub.apply_update(mobile_daemon_state(DaemonPhase::Thinking));

        let mut resolved = Vec::new();
        while let Ok(event) = updates.try_recv() {
            if let MobileLifecycleEvent::AttentionResolved(session_id) = event {
                resolved.push(session_id);
            }
        }
        assert_eq!(resolved, vec!["acp-session-mobile"]);
    }
}
