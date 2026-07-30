//! 远程操作网关的核心逻辑（路由 + handler），供两个地方使用：
//! - `crates/smeltd/src/bin/gateway.rs`：独立进程，命令行启动，自己管一个 `--bind`/`--port`
//! - `crates/smeltd/src/main.rs`：内嵌进守护，靠 `remote_start`/`remote_stop` op 按需开关
//!
//! 两边共用同一份 handler，避免同一套鉴权/转义/协议逻辑复制两次（CLAUDE.md 明令
//! 别复制）。这个模块本身**不碰 smeltd 主协议**：所有跟 smeltd 的交互都是走
//! `sock_path()` 连它自己的 unix socket，用既有的 `list`/`watch` op——不管是从独立
//! 进程调用还是从 smeltd 内部的这个模块调用，走的都是同一条路径，行为完全一致。
//!
//! 只服务移动 App：唯一的对外能力是 `/acp/*`。浏览器面板（remote-web SPA、
//! 内嵌 HTML 终端、`/s/{id}` 那套流式终端接口）连同 Cloudflare/WebRTC 一起下线了，
//! 所以这里没有任何 HTML 模板与静态资源托管。
//!
//! 见 docs/remote-ops-roadmap.md（Phase 1/2）、docs/collaboration.md（安全底线）。

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::agent_status::AgentStatus;
use crate::attention::{AttentionItem, AttentionStore, apply_daemon_transition};
use crate::daemon_state::{DaemonPhase, DaemonSessionState};

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
/// 只有 ACP 两条路由：手机 App 是唯一消费方，它经 iroh 隧道连到这里。
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
        .with_state(state)
}

fn workspace_json_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| "/tmp".into())
        .join(".smelt")
        .join("workspace.json")
}

fn load_gui_acp_titles() -> std::collections::HashMap<String, String> {
    let Ok(raw) = std::fs::read_to_string(workspace_json_path()) else {
        return std::collections::HashMap::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return std::collections::HashMap::new();
    };
    value["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|session| {
            let id = session["acp"]["sid"].as_str()?;
            let title = session["custom_title"].as_str()?.trim();
            (!title.is_empty()).then(|| (id.to_string(), title.to_string()))
        })
        .collect()
}

#[derive(Default)]
struct MobileWorkspaceOrder {
    projects: Vec<String>,
    sessions: std::collections::HashMap<String, (usize, usize)>,
}

fn mobile_workspace_order_from_value(value: &serde_json::Value) -> MobileWorkspaceOrder {
    let projects = value["projects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|project| project.as_str().map(String::from))
        .collect();
    let mut sessions = std::collections::HashMap::new();
    for (session_order, session) in value["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if let Some(sid) = session["acp"]["sid"].as_str() {
            sessions.insert(sid.to_string(), (session_order, 0));
        }
        let Some(layout) = session.get("layout") else {
            continue;
        };
        let mut leaf_order = 0;
        fn collect_ordered_leaf_ids(
            pane: &serde_json::Value,
            session_order: usize,
            leaf_order: &mut usize,
            out: &mut std::collections::HashMap<String, (usize, usize)>,
        ) {
            if let Some(leaf) = pane.get("Leaf") {
                if let Some(id) = leaf["id"].as_str() {
                    out.insert(id.to_string(), (session_order, *leaf_order));
                }
                *leaf_order += 1;
            } else if let Some(children) = pane
                .get("Split")
                .and_then(|split| split.get("children"))
                .and_then(|children| children.as_array())
            {
                for child in children {
                    collect_ordered_leaf_ids(child, session_order, leaf_order, out);
                }
            }
        }
        collect_ordered_leaf_ids(layout, session_order, &mut leaf_order, &mut sessions);
    }
    MobileWorkspaceOrder { projects, sessions }
}

fn load_mobile_workspace_order() -> MobileWorkspaceOrder {
    let Ok(raw) = std::fs::read_to_string(workspace_json_path()) else {
        return MobileWorkspaceOrder::default();
    };
    let Ok(value) = serde_json::from_str(&raw) else {
        return MobileWorkspaceOrder::default();
    };
    mobile_workspace_order_from_value(&value)
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

    fn summaries(&self) -> Vec<AcpSessionSummary> {
        let gui_titles = load_gui_acp_titles();
        let workspace_order = load_mobile_workspace_order();
        let state = self.state.lock().unwrap();
        let mut summaries: Vec<_> = state
            .sessions
            .values()
            .filter_map(|session| {
                acp_summary_from_daemon(session, &gui_titles, &workspace_order, &state.attention)
            })
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

/// ACP 会话摘要（移动端列表用）
#[derive(serde::Serialize)]
struct AcpSessionSummary {
    id: String,
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

fn acp_summary_from_daemon(
    session: &DaemonSessionState,
    gui_titles: &std::collections::HashMap<String, String>,
    workspace_order: &MobileWorkspaceOrder,
    attention_store: &AttentionStore,
) -> Option<AcpSessionSummary> {
    let launch = session.launch.as_deref()?;
    let attention = attention_store.unread(&session.id).cloned();
    let unread = attention.is_some();
    let title = gui_titles
        .get(&session.id)
        .cloned()
        .or_else(|| session.title.clone().filter(|title| !title.is_empty()))
        .or_else(|| {
            session
                .cwd
                .as_ref()
                .and_then(|path| std::path::Path::new(path).file_name())
                .and_then(|name| name.to_str())
                .map(String::from)
        })
        .unwrap_or_else(|| session.id.clone());
    let (session_order, leaf_order) = workspace_order
        .sessions
        .get(&session.id)
        .copied()
        .unwrap_or((usize::MAX, usize::MAX));
    let project_root = session.cwd.as_deref().and_then(|cwd| {
        mobile_project_root(&workspace_order.projects, cwd)
            .or_else(|| Some(cwd.trim_end_matches('/').to_string()))
    });
    let project_order = project_root
        .as_deref()
        .and_then(|root| {
            workspace_order
                .projects
                .iter()
                .position(|project| project.trim_end_matches('/') == root)
        })
        .unwrap_or_else(|| workspace_order.projects.len().saturating_add(session_order));
    let project_title = project_root
        .as_deref()
        .and_then(|root| std::path::Path::new(root).file_name())
        .and_then(|name| name.to_str())
        .map(String::from);
    Some(AcpSessionSummary {
        id: session.id.clone(),
        title,
        phase: daemon_phase_name(session.phase).to_string(),
        status: mobile_status_name(session.phase, unread).to_string(),
        agent: agent_from_launch(launch).to_string(),
        cwd: session.cwd.clone(),
        project_root,
        project_title,
        project_order: project_order.min(u32::MAX as usize) as u32,
        session_order: session_order.min(u32::MAX as usize) as u32,
        leaf_order: leaf_order.min(u32::MAX as usize) as u32,
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

    let sessions = state.mobile_lifecycle.summaries();

    Json(serde_json::json!({
        "sessions": sessions
    }))
    .into_response()
}

/// WebSocket 消息类型（移动端 → 服务端）
#[derive(serde::Deserialize)]
#[serde(tag = "method")]
enum AcpWsRequest {
    #[serde(rename = "subscribe")]
    Subscribe { params: SubscribeParams },
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
    #[serde(rename = "markRead")]
    MarkRead {
        #[allow(dead_code)]
        params: MarkReadParams,
    },
}

#[derive(serde::Deserialize)]
struct SubscribeParams {
    #[serde(rename = "sessionId")]
    session_id: String,
}

#[derive(serde::Deserialize)]
struct SendMessageParams {
    #[serde(rename = "sessionId")]
    session_id: String,
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
                    AcpWsRequest::ListSessions => {
                        let sessions = state.mobile_lifecycle.summaries();
                        let resp = serde_json::json!({
                            "type": "sessions",
                            "sessions": sessions,
                        });
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
                        let tx = daemon_tx.clone();
                        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

                        let handle = tokio::task::spawn_blocking(move || {
                            acp_watch_loop(&session_id, tx, stop_rx);
                        });

                        current_subscription =
                            Some((params.session_id.clone(), stop_tx, handle));

                        let resp = serde_json::json!({
                            "type": "subscribed",
                            "sessionId": params.session_id,
                        });
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
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
                        if !write_enabled {
                            let err = serde_json::json!({"type": "error", "error": "write not enabled"});
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
                            Ok(Ok(())) => serde_json::json!({"type": "messageSent", "ok": true}),
                            Ok(Err(error)) => serde_json::json!({"type": "error", "error": error}),
                            Err(error) => serde_json::json!({
                                "type": "error",
                                "error": format!("failed to dispatch message: {error}"),
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
                    if tx.blocking_send(trimmed.to_string()).is_err() {
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

    #[test]
    fn mobile_requests_preserve_images_and_session_controls() {
        let request: AcpWsRequest = serde_json::from_value(serde_json::json!({
            "method": "sendMessage",
            "params": {
                "sessionId": "session-1",
                "content": "inspect this",
                "images": [{"mime": "image/png", "data_b64": "aW1hZ2U="}]
            }
        }))
        .unwrap();
        match request {
            AcpWsRequest::SendMessage { params } => {
                assert_eq!(params.session_id, "session-1");
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
    }

    fn mobile_daemon_state(phase: DaemonPhase) -> DaemonSessionState {
        DaemonSessionState {
            id: "session-mobile".into(),
            phase,
            title: Some("Mobile agent".into()),
            launch: Some("codex app-server".into()),
            cwd: Some("/tmp/mobile-project".into()),
            updated_at: 42,
            structured_events: true,
            ..Default::default()
        }
    }

    fn mobile_lifecycle_hub_for_test() -> MobileLifecycleHub {
        let (updates, _) = tokio::sync::broadcast::channel(16);
        MobileLifecycleHub {
            state: Mutex::new(MobileLifecycleState::default()),
            updates,
        }
    }

    #[test]
    fn mobile_workspace_order_uses_pc_project_and_session_order() {
        let value = serde_json::json!({
            "projects": ["/repo/two", "/repo/one"],
            "sessions": [
                {
                    "layout": {"Leaf": {"id": "terminal-first"}},
                    "acp": null
                },
                {
                    "layout": {
                        "Split": {
                            "children": [
                                {"Leaf": {"id": "terminal-a"}},
                                {"Leaf": {"id": "terminal-b"}}
                            ]
                        }
                    },
                    "acp": {"sid": "acp-second"}
                }
            ]
        });

        let order = mobile_workspace_order_from_value(&value);
        assert_eq!(order.projects, vec!["/repo/two", "/repo/one"]);
        assert_eq!(order.sessions["terminal-first"], (0, 0));
        assert_eq!(order.sessions["acp-second"], (1, 0));
        assert_eq!(order.sessions["terminal-a"], (1, 0));
        assert_eq!(order.sessions["terminal-b"], (1, 1));
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

        let before = hub.summaries().pop().unwrap();
        assert_eq!(before.phase, "succeeded");
        assert_eq!(before.status, "done");
        assert!(before.unread);
        assert_eq!(
            before.attention.unwrap().kind,
            crate::attention::AttentionKind::Success
        );

        assert!(hub.mark_read("session-mobile"));
        let after = hub.summaries().pop().unwrap();
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

        assert!(hub.mark_read("session-mobile"));
        let summary = hub.summaries().pop().unwrap();
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
        assert_eq!(resolved, vec!["session-mobile"]);
    }
}
