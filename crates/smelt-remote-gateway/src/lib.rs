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
//! 这个 crate 只依赖 `smelt-core` 的无 UI 数据和操作边界，不把 WebSocket
//! 依赖带进 `smelt-core`。见 docs/remote-ops-roadmap.md（Phase 1/2）、
//! docs/collaboration.md（安全底线）。

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use smelt_core::agent_definition::AgentDefinition;
use smelt_core::agent_status::AgentStatus;
use smelt_core::attention::{AttentionItem, AttentionStore, apply_daemon_transition};
#[cfg(test)]
use smelt_core::automation::{
    Automation, AutomationRunContext, AutomationState, AutomationTrigger, WebhookIngress,
};
use smelt_core::automation::{
    AutomationAction, AutomationCommand, AutomationFile, AutomationRun, AutomationRunSource,
    AutomationRunStatus, AutomationSchedule,
};
use smelt_core::daemon_protocol::DaemonOperation;
use smelt_core::daemon_state::{
    DaemonPhase, DaemonSessionState, DaemonStateEvent, DaemonStateEventClient,
};
use smelt_core::session_control::{RemoteSessionKind, RemoteSessionSnapshot};
#[cfg(test)]
use smelt_core::session_control::{RemoteSessionLifecycle, RemoteSessionRecord};
use smelt_core::workspace_menu::{
    WorkspaceMenuProject, WorkspaceMenuSession, WorkspaceMenuSessionKind, WorkspaceMenuSnapshot,
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

/// 将 daemon 已订阅到的远程目录投影到桌面发布的菜单上。两边都来自 subscribe，
/// 网关不读 workspace.json。
fn mobile_workspace_menu(
    desktop_menu: &WorkspaceMenuSnapshot,
    remote_sessions: &RemoteSessionSnapshot,
) -> WorkspaceMenuSnapshot {
    let mut menu = desktop_menu.clone();
    for remote in remote_sessions
        .sessions
        .iter()
        .filter(|remote| remote.is_visible())
    {
        let project_order = ensure_menu_project(&mut menu, &remote.cwd);
        if menu.sessions.iter().any(|session| session.id == remote.id) {
            continue;
        }
        let project = &menu.projects[project_order];
        let (kind, title, agent) = match remote.kind {
            RemoteSessionKind::Acp => {
                let agent = remote.agent.clone().unwrap_or_else(|| "Agent".to_string());
                let title = if remote.title.trim().is_empty() {
                    format!("{agent} conversation")
                } else {
                    remote.title.clone()
                };
                (WorkspaceMenuSessionKind::Acp, title, Some(agent))
            }
            RemoteSessionKind::Terminal => {
                let title = if remote.title.trim().is_empty() {
                    "Terminal".to_string()
                } else {
                    remote.title.clone()
                };
                (WorkspaceMenuSessionKind::Terminal, title, None)
            }
        };
        menu.sessions.push(WorkspaceMenuSession {
            id: remote.id.clone(),
            kind,
            title,
            custom_title: false,
            cwd: Some(remote.cwd.clone()),
            project_root: Some(project.root.clone()),
            project_title: Some(project.title.clone()),
            project_order: project.order,
            session_order: menu.sessions.len().min(u32::MAX as usize) as u32,
            leaf_order: 0,
            agent,
        });
    }
    menu
}

/// 自动化 Run 的 ACP 会话在 daemon 目录里是 `hidden`：桌面侧栏不该被后台 Run 刷屏，
/// 那批会话挂在自动化页的运行历史下面。但手机是指挥台——Run 停下来等审批的那一刻
/// 正是手机唯一不可替代的场景。所以这里按运行档案单独把它们补进移动端菜单，而不是
/// 去掉 daemon 的 `hidden`（那会连桌面侧栏一起改）。
///
/// 补进来的行不带项目归属：Run 的工作区是 daemon 按自动化 id 分配的目录，不是用户
/// 项目，落进项目树只会造出一个假分组。
fn append_automation_run_sessions(
    menu: &mut WorkspaceMenuSnapshot,
    remote_sessions: &RemoteSessionSnapshot,
    automations: &AutomationFile,
) {
    for run in &automations.runs {
        let Some(session_id) = run.session_id.as_deref() else {
            continue;
        };
        if menu.sessions.iter().any(|session| session.id == session_id) {
            continue;
        }
        let Some(remote) = remote_sessions
            .sessions
            .iter()
            .find(|remote| remote.id == session_id)
        else {
            continue;
        };
        if !remote.is_present() || remote.kind != RemoteSessionKind::Acp {
            continue;
        }
        let title = if remote.title.trim().is_empty() {
            run.context.automation_name.clone()
        } else {
            remote.title.clone()
        };
        menu.sessions.push(WorkspaceMenuSession {
            id: session_id.to_string(),
            kind: WorkspaceMenuSessionKind::Acp,
            title,
            custom_title: false,
            cwd: Some(remote.cwd.clone()),
            project_root: None,
            project_title: None,
            project_order: u32::MAX,
            session_order: menu.sessions.len().min(u32::MAX as usize) as u32,
            leaf_order: 0,
            agent: remote
                .agent
                .clone()
                .or_else(|| run.context.engine_kind_id.clone()),
        });
    }
}

/// 会话行的「这不是我开的」来源标注。指挥台靠它把一条卡片解释成
/// 「每日晨报 · 定时触发 · 它想执行 X」，用户不进会话就能判断该不该放行。
#[derive(Clone, serde::Serialize)]
struct MobileAutomationSource {
    automation_id: String,
    automation_name: String,
    run_id: String,
    run_status: AutomationRunStatus,
    run_source: AutomationRunSource,
    /// 这次 Run 固化的输入，不是自动化当前定义值——定义改过之后回看旧 Run，
    /// 显示当前值会直接误导排查。
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<i64>,
}

impl MobileAutomationSource {
    fn from_run(run: &AutomationRun) -> Self {
        Self {
            automation_id: run.automation_id.clone(),
            automation_name: run.context.automation_name.clone(),
            run_id: run.id.clone(),
            run_status: run.status,
            run_source: run.source,
            prompt: run
                .context
                .prompt
                .as_ref()
                .map(|prompt| prompt.trim().to_string())
                .filter(|prompt| !prompt.is_empty()),
            started_at: run.started_at.or(Some(run.created_at)),
        }
    }
}

/// 会话 id → 来源。同一个 session id 只由一条 Run 绑定，但真出现重复时取最近创建的
/// 那条，避免历史档案里的旧记录盖住当前正在跑的。
fn automation_sources(automations: &AutomationFile) -> HashMap<String, MobileAutomationSource> {
    let mut sources: HashMap<String, (i64, MobileAutomationSource)> = HashMap::new();
    for run in &automations.runs {
        let Some(session_id) = run.session_id.clone() else {
            continue;
        };
        let created_at = run.created_at;
        match sources.entry(session_id) {
            Entry::Occupied(mut slot) => {
                if created_at >= slot.get().0 {
                    slot.insert((created_at, MobileAutomationSource::from_run(run)));
                }
            }
            Entry::Vacant(slot) => {
                slot.insert((created_at, MobileAutomationSource::from_run(run)));
            }
        }
    }
    sources
        .into_iter()
        .map(|(id, (_, source))| (id, source))
        .collect()
}

/// 自动化目录的一行。**脱敏**：webhook 的 endpoint 和 secret 不出现在这里。
///
/// 手机上这一屏只回答三件事——它叫什么、什么时候由谁做、上次结果如何。触发器
/// 一律降解成机器码（`schedule` / `webhook` / `event`），文案在移动端落地。
#[derive(Clone, serde::Serialize)]
struct MobileAutomationSummary {
    id: String,
    name: String,
    enabled: bool,
    /// 原样透传给移动端拼「工作日 09:00」。
    ///
    /// 三种时机可以混排在同一条自动化上，所以这里不投一个单选的 kind：那会让
    /// 一条「既定时又能被 webhook 打」的自动化在手机上只剩一半，而用户上手机
    /// 恰恰是来核对「它到底什么时候跑」的。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    schedules: Vec<AutomationSchedule>,
    /// topic 是用户自己起的名字，不是凭据。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    event_topics: Vec<String>,
    /// 有没有外部触发入口。endpoint / secret 是凭据，绝不下发。
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    webhook: bool,
    /// `agent` / `shell`。
    action_kind: &'static str,
    /// action 是 agent 时的智能体展示名；定义被删掉就退回 None，不编一个名字。
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_run_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_run: Option<MobileAutomationRunSummary>,
}

/// 目录行上的「上次结果」。带 `session_id` 是为了让用户从目录直接跳进那次执行现场。
#[derive(Clone, serde::Serialize)]
struct MobileAutomationRunSummary {
    run_id: String,
    status: AutomationRunStatus,
    source: AutomationRunSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    finished_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl MobileAutomationRunSummary {
    fn from_run(run: &AutomationRun) -> Self {
        Self {
            run_id: run.id.clone(),
            status: run.status,
            source: run.source,
            started_at: run.started_at.or(Some(run.created_at)),
            finished_at: run.finished_at,
            session_id: run.session_id.clone(),
            error: run.error.clone(),
        }
    }
}

/// 智能体定义的只读投影。
///
/// 给的是「它为什么这么干」所需的全部输入：工作方式全文、插件、绑定的上下文。
/// 手机不开放编辑（见 `docs/mobile-agents-ux.md` §0），所以这里没有任何写回路径。
#[derive(Clone, serde::Serialize)]
struct MobileAgentDefinition {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    description: String,
    /// 执行引擎的机器码（`pi` 等），标签在移动端落地。
    agent_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    prompt: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    plugins: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    context_folders: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    context_links: Vec<String>,
}

impl MobileAgentDefinition {
    fn from_definition(definition: &AgentDefinition) -> Self {
        Self {
            id: definition.id.clone(),
            name: definition.name.clone(),
            description: definition.description.clone(),
            agent_id: definition.engine_kind_id.clone(),
            prompt: definition.prompt.clone(),
            plugins: definition.plugins.clone(),
            context_folders: definition.context_folders.clone(),
            context_links: definition.context_links.clone(),
        }
    }
}

/// 目录投影：自动化 + 智能体定义。
///
/// 自动化的顺序原样保留存档顺序（桌面列表就是这个顺序），不按状态重排——用户在
/// 两个端上找同一条自动化时，位置得对得上。
fn automation_catalog(
    automations: &AutomationFile,
    definitions: &[AgentDefinition],
) -> Vec<MobileAutomationSummary> {
    automations
        .automations
        .iter()
        .map(|automation| {
            let trigger = &automation.trigger;
            // endpoint / secret 是凭据，绝不下发；手机上只需要知道「有外部触发」。
            let event_topics = trigger
                .events
                .iter()
                .map(|event| event.topic.clone())
                .collect();
            let (action_kind, agent_name) = match &automation.action {
                AutomationAction::Agent {
                    agent_definition_id,
                    ..
                } => (
                    "agent",
                    definitions
                        .iter()
                        .find(|definition| &definition.id == agent_definition_id)
                        .map(|definition| definition.name.clone()),
                ),
                AutomationAction::Shell { .. } => ("shell", None),
            };
            let state = automations
                .states
                .iter()
                .find(|state| state.automation_id == automation.id);
            // 优先按存档里登记的 last_run_id 取；没登记就退回这条自动化最近创建的
            // 那次运行，否则刚触发还没落状态的 Run 会在目录里凭空消失。
            let last_run = state
                .and_then(|state| state.last_run_id.as_ref())
                .and_then(|run_id| automations.runs.iter().find(|run| &run.id == run_id))
                .or_else(|| {
                    automations
                        .runs
                        .iter()
                        .filter(|run| run.automation_id == automation.id)
                        .max_by_key(|run| run.created_at)
                })
                .map(MobileAutomationRunSummary::from_run);
            MobileAutomationSummary {
                id: automation.id.clone(),
                name: automation.name.clone(),
                enabled: automation.enabled,
                schedules: trigger.schedules.clone(),
                event_topics,
                webhook: !trigger.webhooks.is_empty(),
                action_kind,
                agent_name,
                next_run_at: state.and_then(|state| state.next_run_at),
                last_run,
            }
        })
        .collect()
}
/// 目录响应。智能体定义读的是本地存档（SQLite），所以放进 `spawn_blocking`。
async fn automation_catalog_response(automations: &AutomationFile) -> serde_json::Value {
    let definitions =
        tokio::task::spawn_blocking(smelt_core::agent_definition_store::load_agent_definitions)
            .await
            .unwrap_or_default();
    serde_json::json!({
        "type": "automations",
        "automations": automation_catalog(automations, &definitions),
        "agents": definitions
            .iter()
            .map(MobileAgentDefinition::from_definition)
            .collect::<Vec<_>>(),
    })
}

/// 写命令统一走 daemon，再把 daemon 回吐的最新存档整份重投影回去。
///
/// 不做本地乐观更新：daemon 才是自动化的所有者，它可能拒绝（比如 Run 重叠）或
/// 把命令解释成别的结果。回一份权威快照，手机就不会出现「开关拨过去了、实际没生效」。
async fn submit_automation_catalog_command(command: AutomationCommand) -> serde_json::Value {
    let submitted = tokio::task::spawn_blocking(move || {
        smelt_core::session_control::submit_automation_command(&command)
    })
    .await;
    match submitted {
        Ok(Ok((_, automations))) => automation_catalog_response(&automations).await,
        Ok(Err(error)) => serde_json::json!({"type": "error", "error": error}),
        Err(error) => serde_json::json!({
            "type": "error",
            "error": format!("failed to submit automation command: {error}"),
        }),
    }
}

pub fn sock_path() -> std::path::PathBuf {
    let dir = smelt_paths::smelt_home().unwrap_or_else(|| "/tmp/.smelt".into());
    dir.join("smeltd.sock")
}

#[derive(Clone)]
struct AppState {
    token: Arc<String>,
    /// 这个 token 是否有写权限（approve/deny/reply）。链接分享出去那一刻就是
    /// 授权动作，这里不再加一层"每次点击都要主人当面确认"——见
    /// smeltd.rs「远程操控」一节的授权模型说明。开没开由生成链接时的 GUI 开关
    /// 决定，`build_router` 只是如实转达。
    write_enabled: Arc<AtomicBool>,
    mobile_lifecycle: Arc<MobileLifecycleHub>,
}

fn write_enabled(state: &AppState) -> bool {
    state.write_enabled.load(Ordering::Acquire)
}

struct MobileLifecycleHub {
    state: Mutex<MobileLifecycleState>,
    updates: tokio::sync::broadcast::Sender<MobileLifecycleEvent>,
}

#[derive(Default)]
struct MobileLifecycleState {
    sessions: std::collections::HashMap<String, DaemonSessionState>,
    attention: AttentionStore,
    /// `None` is not an empty catalog: it means this gateway has not yet received a usable
    /// daemon event subscription snapshot (or the connected daemon does not provide this field).
    remote_sessions: Option<RemoteSessionSnapshot>,
    /// `None` 同样不是空菜单：表示还没收到桌面经 daemon 发布的侧栏快照。
    workspace_menu: Option<WorkspaceMenuSnapshot>,
    /// daemon 拥有的自动化投影。`None` 表示尚未收到（或所连 daemon 不发这个字段），
    /// 与「没有任何自动化」不是一回事。
    automations: Option<AutomationFile>,
    /// 会话 → 智能体定义 id。桌面工作区存档才知道这个绑定，所以由事件循环在
    /// 菜单变动时异步刷进来，`summaries()` 只查这份内存表——那是每条会话事件都
    /// 跑一遍的热路径，不能在里面读存档。
    agent_definition_ids: std::collections::BTreeMap<String, String>,
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
    build_router_with_write_state(token, Arc::new(AtomicBool::new(write_enabled)))
}

/// 使用调用方持有的共享写权限状态组装网关。内嵌网关在设置页切换权限时
/// 不需要断开现有 WebSocket，旧连接也会读取同一份最新状态。
pub fn build_router_with_write_state(token: String, write_enabled: Arc<AtomicBool>) -> Router {
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

fn path_title(path: &str) -> String {
    std::path::Path::new(path.trim_end_matches('/'))
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_string()
}

#[cfg(test)]
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
        tokio::spawn(mobile_lifecycle_subscription(weak));
        hub
    }

    fn apply_snapshot(
        &self,
        sessions: Vec<DaemonSessionState>,
        remote_sessions: Option<RemoteSessionSnapshot>,
        workspace_menu: Option<WorkspaceMenuSnapshot>,
        automations: Option<AutomationFile>,
    ) {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap();
        let mut resolved = Vec::new();
        let ids: std::collections::HashSet<_> =
            sessions.iter().map(|session| session.id.clone()).collect();
        for session in &sessions {
            let had_unresolved_action = state.attention.has_unresolved_action(&session.id);
            let previous = state.sessions.get(&session.id).cloned();
            apply_daemon_transition(&mut state.attention, previous.as_ref(), session, now);
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
        state.remote_sessions = remote_sessions;
        state.workspace_menu = workspace_menu;
        state.automations = automations;
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
        let previous = state.sessions.get(&session.id).cloned();
        let attention = apply_daemon_transition(
            &mut state.attention,
            previous.as_ref(),
            &session,
            Instant::now(),
        );
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

    fn apply_remote_sessions(&self, remote_sessions: RemoteSessionSnapshot) {
        self.state.lock().unwrap().remote_sessions = Some(remote_sessions);
        let _ = self.updates.send(MobileLifecycleEvent::SessionsChanged);
    }

    fn apply_workspace_menu(&self, workspace_menu: WorkspaceMenuSnapshot) {
        self.state.lock().unwrap().workspace_menu = Some(workspace_menu);
        let _ = self.updates.send(MobileLifecycleEvent::SessionsChanged);
    }

    fn apply_automations(&self, automations: AutomationFile) {
        self.state.lock().unwrap().automations = Some(automations);
        let _ = self.updates.send(MobileLifecycleEvent::SessionsChanged);
    }

    /// 会话归属表换新。内容没变就不广播，避免每次菜单刷新都惊动所有订阅者。
    fn apply_agent_definition_ids(
        &self,
        agent_definition_ids: std::collections::BTreeMap<String, String>,
    ) {
        let mut state = self.state.lock().unwrap();
        if state.agent_definition_ids == agent_definition_ids {
            return;
        }
        state.agent_definition_ids = agent_definition_ids;
        drop(state);
        let _ = self.updates.send(MobileLifecycleEvent::SessionsChanged);
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

    fn workspace_projection(
        &self,
    ) -> Result<(WorkspaceMenuSnapshot, RemoteSessionSnapshot), String> {
        let state = self.state.lock().unwrap();
        let desktop_menu = state.workspace_menu.clone().ok_or_else(|| {
            "workspace menu unavailable; waiting for daemon subscription".to_string()
        })?;
        let remote_sessions = state.remote_sessions.clone().ok_or_else(|| {
            "remote session catalog unavailable; waiting for daemon subscription".to_string()
        })?;
        let automations = state.automations.clone();
        drop(state);
        let mut menu = mobile_workspace_menu(&desktop_menu, &remote_sessions);
        if let Some(automations) = automations.as_ref() {
            append_automation_run_sessions(&mut menu, &remote_sessions, automations);
        }
        Ok((menu, remote_sessions))
    }

    fn workspace_menu(&self) -> Result<WorkspaceMenuSnapshot, String> {
        self.workspace_projection().map(|(menu, _)| menu)
    }

    fn summaries(&self) -> Result<Vec<MobileSessionSummary>, String> {
        let menu = self.workspace_menu()?;
        Ok(self.summaries_with_menu(&menu))
    }

    /// 这条会话是不是某次自动化 Run 的执行现场。
    fn automation_run_id(&self, session_id: &str) -> Option<String> {
        let state = self.state.lock().unwrap();
        let automations = state.automations.as_ref()?;
        automation_sources(automations)
            .remove(session_id)
            .map(|source| source.run_id)
    }

    /// 自动化目录快照。还没收到 daemon 的自动化事件时返回 `None`，让调用方
    /// 明确回一句「还在同步」，而不是把空目录冒充成「你一条自动化都没有」。
    fn automations_snapshot(&self) -> Option<AutomationFile> {
        self.state.lock().unwrap().automations.clone()
    }

    fn delete_kinds(
        &self,
        id: &str,
    ) -> Result<(Option<RemoteSessionKind>, Option<WorkspaceMenuSessionKind>), String> {
        let (menu, remote_sessions) = self.workspace_projection()?;
        let catalog_kind = remote_sessions
            .sessions
            .iter()
            .find(|session| session.id == id)
            .map(|session| session.kind);
        let menu_kind = menu.session(id).map(|session| session.kind);
        Ok((catalog_kind, menu_kind))
    }

    /// 这条会话没了：daemon 报的，或者用户刚在手机上删掉的。
    ///
    /// 名册是菜单，所以**必须连菜单缓存一起划掉**——只删运行态的话，被删的会话会
    /// 一直挂在列表上，直到 daemon 推来下一份菜单快照。远端会话目录同样要划：它
    /// 会在投影时把自己那条补回菜单里。
    fn remove_session(&self, id: &str) {
        let mut state = self.state.lock().unwrap();
        state.sessions.remove(id);
        state.attention.remove_session(id);
        if let Some(menu) = state.workspace_menu.as_mut() {
            menu.sessions.retain(|session| session.id != id);
        }
        if let Some(remote) = state.remote_sessions.as_mut() {
            remote.sessions.retain(|session| session.id != id);
        }
        drop(state);
        let _ = self.updates.send(MobileLifecycleEvent::SessionsChanged);
    }

    fn summaries_with_menu(&self, menu: &WorkspaceMenuSnapshot) -> Vec<MobileSessionSummary> {
        let state = self.state.lock().unwrap();
        let sources = state
            .automations
            .as_ref()
            .map(automation_sources)
            .unwrap_or_default();
        // 遍历菜单而不是 daemon 会话表：名册在菜单，daemon 只有活动记录。
        let mut summaries: Vec<_> = menu
            .sessions
            .iter()
            .map(|menu_session| {
                mobile_summary(
                    menu_session,
                    state.sessions.get(&menu_session.id),
                    &state.attention,
                    sources.get(&menu_session.id).cloned(),
                    state.agent_definition_ids.get(&menu_session.id).cloned(),
                )
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

async fn refreshed_mobile_summaries(
    hub: Arc<MobileLifecycleHub>,
) -> Result<Vec<MobileSessionSummary>, String> {
    hub.summaries()
}

async fn mobile_lifecycle_subscription(hub: Weak<MobileLifecycleHub>) {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    let mut reconnect_attempt = 0u32;
    while hub.strong_count() > 0 {
        let connected_at = Instant::now();
        if let Ok(Ok(stream)) = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::UnixStream::connect(sock_path()),
        )
        .await
        {
            let (reader, mut writer) = stream.into_split();
            let mut client = DaemonStateEventClient::remote_gateway();
            let Ok(request) = client.request_line() else {
                return;
            };
            if tokio::time::timeout(Duration::from_secs(2), writer.write_all(request.as_bytes()))
                .await
                .is_ok_and(|result| result.is_ok())
            {
                let mut reader = tokio::io::BufReader::new(reader);
                let mut line = String::new();
                loop {
                    if hub.strong_count() == 0 {
                        return;
                    }
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let Some(hub) = hub.upgrade() else {
                                return;
                            };
                            match client.decode_line(&line) {
                                Ok(Some(event)) => {
                                    if apply_mobile_lifecycle_event(&hub, event) {
                                        refresh_agent_definition_ids(&hub).await;
                                    }
                                }
                                Ok(None) => {}
                                Err(_) => break,
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        }
        if hub.strong_count() == 0 {
            return;
        }
        if connected_at.elapsed() >= Duration::from_secs(5) {
            reconnect_attempt = 0;
        }
        let delay = smelt_core::daemon_state::daemon_reconnect_backoff(reconnect_attempt);
        reconnect_attempt = reconnect_attempt.saturating_add(1);
        tokio::time::sleep(delay).await;
    }
}

/// 应用一条 daemon 事件；返回 true 表示会话集合可能变了，归属表要跟着重读。
///
/// 桌面每次开关会话都会重发菜单，而工作区存档正是同一批动作写下的，所以这就是
/// 「新开的智能体对话该有归属了」最早的可靠信号。读存档的 IO 留给调用方放到
/// blocking 线程上，这里只做纯内存更新。
fn apply_mobile_lifecycle_event(hub: &MobileLifecycleHub, event: DaemonStateEvent) -> bool {
    match event {
        DaemonStateEvent::Snapshot {
            sessions,
            remote_sessions,
            workspace_menu,
            automations,
            ..
        } => {
            hub.apply_snapshot(sessions, remote_sessions, workspace_menu, automations);
            true
        }
        DaemonStateEvent::Update(session) => {
            hub.apply_update(session);
            false
        }
        DaemonStateEvent::Removed { id } => {
            hub.remove_session(&id);
            false
        }
        DaemonStateEvent::RemoteSessions(remote_sessions) => {
            hub.apply_remote_sessions(remote_sessions);
            true
        }
        DaemonStateEvent::WorkspaceMenu(workspace_menu) => {
            hub.apply_workspace_menu(workspace_menu);
            true
        }
        DaemonStateEvent::Automations(automations) => {
            hub.apply_automations(automations);
            false
        }
        DaemonStateEvent::Disconnected => false,
    }
}

/// 重读会话归属表。存档在 SQLite 上，所以整段放进 `spawn_blocking`。
async fn refresh_agent_definition_ids(hub: &MobileLifecycleHub) {
    if let Ok(ids) = tokio::task::spawn_blocking(
        smelt_core::agent_definition_store::load_session_agent_definition_ids,
    )
    .await
    {
        hub.apply_agent_definition_ids(ids);
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
    /// 只有自动化 Run 的会话才有：谁在什么时候把它起起来的。
    #[serde(skip_serializing_if = "Option::is_none")]
    automation: Option<MobileAutomationSource>,
    /// 这是一条智能体对话，值是那个智能体定义的 id。
    ///
    /// 归属来自桌面工作区存档里的绑定，**不是**从 cwd 反推的：智能体对话的工作
    /// 目录不固定——可以是智能体自己的 space，也可以是任意一个普通项目仓库，
    /// 反推只认得出前一半，开在项目里的对话会整批失去归属。
    ///
    /// 只给 id 不给名字，是因为 `summaries()` 是热路径不能读存档。名字由移动端
    /// 拿 `listAutomations` 的定义表配；配不上就退回一个通用标签，不编名字。
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_definition_id: Option<String>,
}

fn daemon_phase_name(state: &DaemonSessionState) -> &'static str {
    match state.effective_phase() {
        DaemonPhase::Connecting => "connecting",
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

fn mobile_status_name(state: &DaemonSessionState, _unread: bool) -> &'static str {
    match AgentStatus::from_daemon_state(state) {
        Some(AgentStatus::NeedsYou) => "needs_you",
        Some(AgentStatus::Running) => "running",
        Some(AgentStatus::Idle) | None => "idle",
    }
}

/// mobile 网关展示名：跟 main.rs 的 `acp_agent_from_cmd`（旧存档反推）共用
/// `ConversationAgentKind::from_command_loose` 这份「命令里出现哪家关键字就算哪家」判断，
/// 只是认不出时的兜底值不同（这里是通用的 "other"，那边默认 Claude）。
///
/// ACP 表查不到再查终端表：两张表不是同一个集合，Antigravity 和 Crush 只有 TUI。
/// 这个字段在移动端是纯展示的（chip 文字、图标、读屏标签），认出来只会更准——
/// 掉到 "other" 的后果是用户在手机上看不出这条会话是谁在跑。
fn agent_from_launch(launch: &str) -> &'static str {
    smelt_core::agent_kind::ConversationAgentKind::from_command_loose(launch)
        .map(smelt_core::agent_kind::ConversationAgentKind::id)
        .or_else(|| {
            smelt_core::agent_kind::TerminalAgentKind::from_command_prefix(launch)
                .map(smelt_core::agent_kind::TerminalAgentKind::id)
        })
        .unwrap_or("other")
}

/// 一条会话在移动端长什么样。
///
/// **存在性来自菜单快照，运行态才来自 daemon 订阅。** 这两件事原来是一件：投影
/// 遍历 daemon 会话表，菜单只用来查类型，于是「PC 上开着但还没跑过任何东西」的
/// 会话在手机上根本不存在——用户在桌面侧栏看得见它，掏出手机却找不到，直到它第
/// 一次上报事件才凭空出现。daemon 表是**活动记录**，不是会话名册；名册是菜单。
///
/// 所以 `daemon` 是 `Option`：没有运行态不代表没有这条会话，只代表它还没动过。
fn mobile_summary(
    menu_session: &WorkspaceMenuSession,
    daemon: Option<&DaemonSessionState>,
    attention_store: &AttentionStore,
    automation: Option<MobileAutomationSource>,
    agent_definition_id: Option<String>,
) -> MobileSessionSummary {
    let attention = attention_store.unread(&menu_session.id).cloned();
    let unread = attention.is_some();
    let agent = menu_session
        .agent
        .clone()
        .or_else(|| daemon.and_then(|state| state.provider.clone()))
        .or_else(|| {
            daemon
                .and_then(|state| state.launch.as_deref())
                .map(|launch| agent_from_launch(launch).to_string())
        })
        .unwrap_or_else(|| "other".to_string());
    let title = smelt_core::session_title::display_title(
        menu_session
            .custom_title
            .then_some(menu_session.title.as_str()),
        daemon.and_then(|state| state.title.as_deref()),
        None,
        Some(menu_session.title.as_str()),
    );
    MobileSessionSummary {
        id: menu_session.id.clone(),
        kind: menu_session.kind,
        title,
        // 没有运行态就是「还没动过」，不是某种失败态。`updated_at` 保持 0：那是
        // 「没有活动时间」的实话，移动端的分诊屏正是靠它把这些会话排除在外的。
        phase: daemon.map(daemon_phase_name).unwrap_or("idle").to_string(),
        status: daemon
            .map(|state| mobile_status_name(state, unread))
            .unwrap_or("idle")
            .to_string(),
        agent,
        cwd: menu_session
            .cwd
            .clone()
            .or_else(|| daemon.and_then(|state| state.cwd.clone())),
        project_root: menu_session.project_root.clone(),
        project_title: menu_session.project_title.clone(),
        project_order: menu_session.project_order,
        session_order: menu_session.session_order,
        leaf_order: menu_session.leaf_order,
        updated_at: daemon
            .map(|state| state.updated_at.min(i64::MAX as u64) as i64)
            .unwrap_or(0),
        detail: daemon.and_then(|state| state.detail_line()),
        unread,
        attention,
        automation,
        agent_definition_id,
    }
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

    match refreshed_mobile_summaries(Arc::clone(&state.mobile_lifecycle)).await {
        Ok(sessions) => Json(serde_json::json!({ "sessions": sessions })).into_response(),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": error})),
        )
            .into_response(),
    }
}

const TERMINAL_ATTACH_TIMEOUT: Duration = Duration::from_secs(15);
const TERMINAL_DAEMON_TIMEOUT: Duration = Duration::from_secs(5);
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
/// 客户端能要到的 scrollback 行数上限；daemon 侧还有一道同样的闸。
const MAX_TERMINAL_SCROLLBACK_LINES: u32 = 10_000;

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
    /// 这次 attach 要多少行 scrollback。只在 attach 上有意义，resize 忽略。
    /// 不传（老客户端）= 由 daemon 给全量，行为不变。
    #[serde(default, rename = "maxScrollbackLines")]
    max_scrollback_lines: Option<u32>,
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
            max_scrollback_lines: self
                .max_scrollback_lines
                .map(|lines| lines.clamp(1, MAX_TERMINAL_SCROLLBACK_LINES)),
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
        history_lines: usize,
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
    let exposed = match state.mobile_lifecycle.workspace_menu() {
        Ok(menu) => menu
            .session(&id)
            .is_some_and(|session| session.kind == WorkspaceMenuSessionKind::Terminal),
        Err(error) => return (StatusCode::SERVICE_UNAVAILABLE, error).into_response(),
    };
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
        "writeEnabled": write_enabled(&state),
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
    let (watch_command_tx, watch_command_rx) = tokio::sync::mpsc::channel(16);
    let (watch_ready_tx, watch_ready_rx) = tokio::sync::oneshot::channel();
    let watch_id = id.clone();
    let watch_task = tokio::spawn(terminal_watch_and_forward(
        watch_id,
        geometry,
        frame_tx,
        watch_command_rx,
        stop_rx,
        watch_ready_tx,
    ));

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
                                if !write_enabled(&state) {
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
                                match send_terminal_input(&id, &params.data).await {
                                    Ok(()) => {}
                                    Err(error) => {
                                        let _ = send_terminal_error(&mut ws_tx, &error).await;
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
                                match watch_command_tx.send(TerminalWatchCommand::Resize(geometry)).await {
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
                    TerminalFrame::Header { cols, rows, replay_len, history_lines } => {
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
                            // 手机据此判断还能不能往上拉更老的内容。
                            "historyLines": history_lines,
                            "scrollbackLines": geometry.max_scrollback_lines,
                            "writeEnabled": write_enabled(&state),
                            "theme": smelt_core::terminal_theme::load().to_wire(),
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

async fn send_terminal_input(id: &str, data: &str) -> Result<(), String> {
    send_terminal_daemon_command(serde_json::json!({
        "op": DaemonOperation::Input,
        "id": id,
        "data": data,
    }))
    .await
}

async fn send_terminal_daemon_command(request: serde_json::Value) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    let stream = tokio::net::UnixStream::connect(sock_path())
        .await
        .map_err(|error| format!("connect failed: {error}"))?;
    let (reader, mut writer) = stream.into_split();
    let req_bytes = format!("{request}\n");
    tokio::time::timeout(
        TERMINAL_DAEMON_TIMEOUT,
        writer.write_all(req_bytes.as_bytes()),
    )
    .await
    .map_err(|_| "write timeout".to_string())?
    .map_err(|error| format!("write failed: {error}"))?;

    let mut reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    tokio::time::timeout(TERMINAL_DAEMON_TIMEOUT, reader.read_line(&mut response))
        .await
        .map_err(|_| "read timeout".to_string())?
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

async fn terminal_watch_and_forward(
    id: String,
    geometry: TerminalGeometryParams,
    tx: tokio::sync::mpsc::Sender<TerminalFrame>,
    mut commands: tokio::sync::mpsc::Receiver<TerminalWatchCommand>,
    stop: tokio::sync::watch::Receiver<bool>,
    ready: tokio::sync::oneshot::Sender<Result<(), String>>,
) {
    let mut ready = Some(ready);
    let result = terminal_watch_loop(&id, geometry, &tx, &mut commands, &stop, &mut ready).await;
    match result {
        Ok(()) => {
            let _ = tx.send(TerminalFrame::Closed).await;
        }
        Err(error) if !*stop.borrow() => {
            if let Some(ready) = ready.take() {
                let _ = ready.send(Err(error.clone()));
            }
            let _ = tx.send(TerminalFrame::Error(error)).await;
        }
        Err(_) => {}
    }
}

async fn terminal_watch_loop(
    id: &str,
    geometry: TerminalGeometryParams,
    tx: &tokio::sync::mpsc::Sender<TerminalFrame>,
    commands: &mut tokio::sync::mpsc::Receiver<TerminalWatchCommand>,
    stop: &tokio::sync::watch::Receiver<bool>,
    ready: &mut Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};

    let stream = tokio::net::UnixStream::connect(sock_path())
        .await
        .map_err(|error| format!("connect failed: {error}"))?;
    let (reader, mut writer) = stream.into_split();

    let watch_req = serde_json::json!({
        "op": DaemonOperation::Watch,
        "id": id,
        "controls_geometry": true,
        "cols": geometry.cols,
        "rows": geometry.rows,
        "cell_w": geometry.cell_width,
        "cell_h": geometry.cell_height,
        "max_scrollback_lines": geometry.max_scrollback_lines,
    });
    let req_line = format!("{watch_req}\n");
    tokio::time::timeout(
        TERMINAL_DAEMON_TIMEOUT,
        writer.write_all(req_line.as_bytes()),
    )
    .await
    .map_err(|_| "write watch timeout".to_string())?
    .map_err(|error| format!("watch failed: {error}"))?;

    let mut reader = tokio::io::BufReader::new(reader);
    let mut line = String::new();
    tokio::time::timeout(TERMINAL_DAEMON_TIMEOUT, reader.read_line(&mut line))
        .await
        .map_err(|_| "watch header timeout".to_string())?
        .map_err(|error| format!("watch header failed: {error}"))?;
    if line.is_empty() {
        return Err("terminal session not found".to_string());
    }
    let header: serde_json::Value =
        serde_json::from_str(&line).map_err(|error| format!("invalid watch header: {error}"))?;
    let cols = header["cols"].as_u64().unwrap_or(80).min(300) as u16;
    let rows = header["rows"].as_u64().unwrap_or(24).min(200) as u16;
    let replay_len = header["replay_len"].as_u64().unwrap_or(0) as usize;
    let history_lines = header["history_lines"].as_u64().unwrap_or(0) as usize;
    if replay_len > MAX_TERMINAL_REPLAY_BYTES {
        return Err("terminal snapshot is too large".to_string());
    }
    tx.send(TerminalFrame::Header {
        cols,
        rows,
        replay_len,
        history_lines,
    })
    .await
    .map_err(|_| "terminal client disconnected".to_string())?;

    if replay_len > 0 {
        let mut snapshot = vec![0; replay_len];
        tokio::time::timeout(TERMINAL_DAEMON_TIMEOUT, reader.read_exact(&mut snapshot))
            .await
            .map_err(|_| "read snapshot timeout".to_string())?
            .map_err(|error| format!("terminal snapshot failed: {error}"))?;
        tx.send(TerminalFrame::Bytes(snapshot))
            .await
            .map_err(|_| "terminal client disconnected".to_string())?;
    }

    if let Some(ready) = ready.take() {
        let _ = ready.send(Ok(()));
    }

    let mut buffer = [0_u8; 8192];
    let mut stop_rx = stop.clone();

    loop {
        if *stop.borrow() {
            return Err("terminal watch stopped".to_string());
        }
        tokio::select! {
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    return Err("terminal watch stopped".to_string());
                }
            }
            cmd = commands.recv() => {
                let Some(command) = cmd else {
                    return Err("command channel closed".to_string());
                };
                match command {
                    TerminalWatchCommand::Resize(geometry) => {
                        write_terminal_resize_frame(&mut writer, geometry).await?;
                    }
                }
            }
            res = reader.read(&mut buffer) => {
                match res {
                    Ok(0) => return Ok(()),
                    Ok(read) => {
                        tx.send(TerminalFrame::Bytes(buffer[..read].to_vec()))
                            .await
                            .map_err(|_| "terminal client disconnected".to_string())?;
                    }
                    Err(error) => return Err(format!("terminal watch failed: {error}")),
                }
            }
        }
    }
}

async fn write_terminal_resize_frame<W: tokio::io::AsyncWriteExt + Unpin>(
    writer: &mut W,
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
        .await
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
    /// 输入栏的「技能」入口：按需问一次，不跟着快照推。
    #[serde(rename = "listSessionSkills")]
    ListSessionSkills { params: SessionActionParams },
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
    #[serde(rename = "renameSessionHistory")]
    RenameSessionHistory { params: RenameSessionHistoryParams },
    #[serde(rename = "createSession")]
    CreateSession { params: CreateSessionParams },
    #[serde(rename = "deleteSession")]
    DeleteSession { params: SessionActionParams },
    #[serde(rename = "markRead")]
    MarkRead { params: MarkReadParams },
    #[serde(rename = "listAutomations")]
    ListAutomations,
    #[serde(rename = "setAutomationEnabled")]
    SetAutomationEnabled { params: AutomationEnabledParams },
    #[serde(rename = "runAutomationOnce")]
    RunAutomationOnce { params: AutomationActionParams },
}

/// 目录里的启停开关。手机上这是唯一的自动化写操作之一，故意做成幂等的
/// 设值（而不是 toggle）：列表状态可能已经被桌面改过，toggle 会翻反。
#[derive(serde::Deserialize)]
struct AutomationEnabledParams {
    #[serde(rename = "automationId")]
    automation_id: String,
    enabled: bool,
}

#[derive(serde::Deserialize)]
struct AutomationActionParams {
    #[serde(rename = "automationId")]
    automation_id: String,
}

#[derive(serde::Deserialize)]
struct SessionHistoryParams {
    #[serde(rename = "projectRoot")]
    project_root: String,
    #[serde(rename = "agentOptionId")]
    agent_option_id: String,
}

/// 历史会话重命名：`title` 缺省 / 空串 = 恢复 agent 原始标题。`projectRoot` 不参与
/// 存储身份（自定义名称按 agent + profile + resumeId 存），只用于把结果回传给发起
/// 的那一页，好让它对上自己正在展示的列表。
#[derive(serde::Deserialize)]
struct RenameSessionHistoryParams {
    #[serde(default, rename = "projectRoot")]
    project_root: String,
    #[serde(rename = "agentOptionId")]
    agent_option_id: String,
    #[serde(rename = "resumeId")]
    resume_id: String,
    #[serde(default)]
    title: Option<String>,
}

#[derive(serde::Deserialize)]
struct CreateSessionParams {
    /// 智能体对话可以不带项目：那时会话落在智能体自己的 space。
    #[serde(default, rename = "projectRoot")]
    project_root: String,
    #[serde(default, rename = "agentOptionId")]
    agent_option_id: Option<String>,
    /// 新建选择器里那一行的稳定 key（见 `smelt_core::new_session`）。给了它就由
    /// 网关自己解析该起对话还是起哪种终端，`kind` / `agentOptionId` 不用再猜。
    #[serde(default, rename = "launchKey")]
    launch_key: Option<String>,
    #[serde(default, rename = "resumeId")]
    resume_id: Option<String>,
    /// `"acp"`（默认，向后兼容旧客户端）或 `"terminal"`。
    #[serde(default, rename = "kind")]
    kind: Option<String>,
    /// 客户端为一次创建意图生成并在重试时复用的 UUID。它直接进入 daemon session id，
    /// 因而重试会走同一条 reserve/reattach 路径，不会创建第二个 runtime。
    #[serde(default, rename = "requestId")]
    request_id: Option<String>,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

/// 历史会话改名：交给 smeltd 写 overlay 并同步远程目录，两端读同一份落盘。
/// 返回 `(展示标题, 自定义标题)`——恢复默认时展示标题得回到 agent 原始标题，那个值
/// 只有重新扫一遍 transcript 才知道，不能让手机自己猜。
fn rename_mobile_history_session(
    params: RenameSessionHistoryParams,
) -> Result<(String, Option<String>), String> {
    let option = smelt_core::session_control::find_agent_option(&params.agent_option_id)
        .ok_or_else(|| "unknown ACP agent or profile".to_string())?;
    let kind = smelt_core::agent_kind::ConversationAgentKind::from_id(&option.kind)
        .ok_or_else(|| "unknown ACP agent".to_string())?;
    let resume_id = params.resume_id.trim();
    if resume_id.is_empty() {
        return Err("missing resumeId".to_string());
    }
    let custom_title = params
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty());
    smelt_core::session_control::rename_history_title(
        kind.into(),
        option.profile_id(),
        resume_id,
        custom_title,
        Some(params.project_root.as_str()),
    )?;

    let display_title = smelt_core::session_control::list_history(&option, &params.project_root)
        .into_iter()
        .find(|session| session.resume_id == resume_id)
        .map(|session| session.display_title().to_string())
        .or_else(|| custom_title.map(str::to_string))
        .unwrap_or_default();
    Ok((display_title, custom_title.map(str::to_string)))
}

fn mobile_session_id(prefix: &str, request_id: Option<&str>) -> Result<String, String> {
    let Some(request_id) = request_id.map(str::trim).filter(|id| !id.is_empty()) else {
        return Ok(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    };
    let request_id =
        uuid::Uuid::parse_str(request_id).map_err(|_| "requestId must be a UUID".to_string())?;
    Ok(format!("{prefix}-mobile-{}", request_id.simple()))
}

fn create_mobile_session(
    mut params: CreateSessionParams,
    menu: WorkspaceMenuSnapshot,
    remote_sessions: RemoteSessionSnapshot,
) -> Result<String, String> {
    let launch_key = params
        .launch_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string);
    if let Some(key) = launch_key {
        use smelt_core::new_session::NewSessionTarget;
        let action = resolve_launch_action(&key).ok_or_else(|| format!("unknown launch {key}"))?;
        return match action.target {
            NewSessionTarget::Conversation {
                agent_option_id, ..
            } => {
                params.agent_option_id = Some(agent_option_id);
                create_mobile_acp_session(params, &menu, &remote_sessions)
            }
            NewSessionTarget::Terminal { command, .. } => {
                let id = mobile_session_id("term", params.request_id.as_deref())?;
                create_mobile_terminal_session(
                    &menu,
                    &remote_sessions,
                    params.project_root,
                    id,
                    Some(command.as_str()),
                )
            }
            NewSessionTarget::BlankTerminal => {
                let id = mobile_session_id("term", params.request_id.as_deref())?;
                create_mobile_terminal_session(
                    &menu,
                    &remote_sessions,
                    params.project_root,
                    id,
                    None,
                )
            }
        };
    }
    if params.kind.as_deref() == Some("terminal") {
        let id = mobile_session_id("term", params.request_id.as_deref())?;
        return create_mobile_terminal_session(
            &menu,
            &remote_sessions,
            params.project_root,
            id,
            None,
        );
    }
    create_mobile_acp_session(params, &menu, &remote_sessions)
}

/// `launch`：新建 PTY 时先跑的命令（空白终端为 None）。它不是会话状态，只是创建
/// 参数——重试会带着同一个 key 再来一次，已经建好的会话 reattach 时守护会忽略它。
fn create_mobile_terminal_session(
    menu: &WorkspaceMenuSnapshot,
    remote_sessions: &RemoteSessionSnapshot,
    project_root: String,
    id: String,
    launch: Option<&str>,
) -> Result<String, String> {
    if !menu
        .projects
        .iter()
        .any(|project| project.root == project_root)
    {
        return Err("project is not in the Smelt workspace".to_string());
    }
    if let Some(existing) = remote_sessions
        .sessions
        .iter()
        .find(|session| session.id == id)
    {
        let existing = existing
            .as_terminal()
            .ok_or_else(|| "requestId is already bound to a different session kind".to_string())?;
        if existing.cwd != project_root {
            return Err("requestId is already bound to a different project".to_string());
        }
        smelt_core::session_control::create_remote_terminal_session(&existing, launch)?;
        return Ok(id);
    }
    let session = smelt_core::session_control::RemoteTerminalSession {
        id: id.clone(),
        cwd: project_root,
        title: String::new(),
        created_at: now_unix(),
        lifecycle: Default::default(),
    };
    smelt_core::session_control::create_remote_terminal_session(&session, launch)?;
    Ok(id)
}

/// 手机新建对话的全部选项：产品智能体在前，裸引擎与 profile 在后——与桌面新建
/// 菜单同序，别让同一份清单在两端读出不同的推荐顺序。
fn mobile_agent_options() -> Vec<smelt_core::session_control::AcpAgentOption> {
    smelt_core::new_session::stored_conversation_options()
}

/// CLI 探测要逐个跑 `--version`，不能每次 `listWorkspace` 都来一遍；同一台机器上
/// 装没装 CLI 不会在几分钟内反复变化，缓存过期后再探一次即可。
const RUNTIME_DIAGNOSTICS_TTL: Duration = Duration::from_secs(120);

fn cached_runtime_diagnostics() -> smelt_core::acp_conn::AcpRuntimeDiagnostics {
    static CACHE: std::sync::OnceLock<
        Mutex<Option<(Instant, smelt_core::acp_conn::AcpRuntimeDiagnostics)>>,
    > = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock()
        && let Some((checked_at, diagnostics)) = guard.as_ref()
        && checked_at.elapsed() < RUNTIME_DIAGNOSTICS_TTL
    {
        return diagnostics.clone();
    }
    let diagnostics = smelt_core::acp_conn::inspect_acp_runtime();
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((Instant::now(), diagnostics.clone()));
    }
    diagnostics
}

/// 手机新建会话的启动动作目录。分组、顺序、Pin 都来自桌面那份共享实现，手机只管画。
fn mobile_launch_actions() -> Vec<serde_json::Value> {
    let diagnostics = cached_runtime_diagnostics();
    launch_actions_json(smelt_core::new_session::stored_new_session_sections(Some(
        &diagnostics,
    )))
}

/// 把三个分组拍平成手机要的 JSON：每行在动作本身的字段上再补一个 `section`。
fn launch_actions_json(
    sections: smelt_core::new_session::NewSessionSections,
) -> Vec<serde_json::Value> {
    let [common, terminal, conversation] = sections.into_array();
    [
        ("common", common),
        ("terminal", terminal),
        ("conversation", conversation),
    ]
    .into_iter()
    .flat_map(|(section, actions)| {
        actions.into_iter().filter_map(move |action| {
            let mut value = serde_json::to_value(&action).ok()?;
            value
                .as_object_mut()?
                .insert("section".into(), section.into());
            Some(value)
        })
    })
    .collect()
}

/// 手机只回传 key，命令永远在这台机器上解析——别让配对设备决定跑什么进程。
/// 探测状态不参与解析：目录里还在的启动项就能起，免得探测缓存过期让刚点下的
/// 那一行变成“未知启动项”。
fn resolve_launch_action(key: &str) -> Option<smelt_core::new_session::NewSessionAction> {
    let actions = smelt_core::new_session::new_session_actions(
        &smelt_core::new_session::stored_conversation_options(),
        &smelt_core::new_session::stored_launch_entries(),
        None,
    );
    smelt_core::new_session::find_action(&actions, key).cloned()
}

/// 新对话落在哪个目录。项目对话必须是工作区里已有的项目，智能体对话则走智能体
/// 自己的 space——手机上从「智能体」页开对话时根本没有项目可选，这时不给它一个
/// 托管目录就只能逼用户先挑一个无关仓库。
fn resolve_acp_session_cwd(
    project_root: &str,
    option: &smelt_core::session_control::AcpAgentOption,
    menu: &WorkspaceMenuSnapshot,
) -> Result<String, String> {
    let project_root = project_root.trim();
    if project_root.is_empty() {
        let definition_id = option
            .agent_definition_id()
            .ok_or_else(|| "missing projectRoot".to_string())?;
        return smelt_core::agent_definition_store::ensure_agent_space(definition_id)
            .map(|dir| dir.to_string_lossy().into_owned())
            .ok_or_else(|| "could not create the agent workspace".to_string());
    }
    menu.projects
        .iter()
        .find(|project| project.root == project_root)
        .map(|project| project.root.clone())
        .ok_or_else(|| "project is not in the Smelt workspace".to_string())
}

fn create_mobile_acp_session(
    params: CreateSessionParams,
    menu: &WorkspaceMenuSnapshot,
    remote_sessions: &RemoteSessionSnapshot,
) -> Result<String, String> {
    let CreateSessionParams {
        project_root,
        agent_option_id,
        resume_id,
        request_id,
        ..
    } = params;
    let agent_option_id = agent_option_id
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| "missing agentOptionId".to_string())?;
    let option = smelt_core::session_control::find_agent_option(&agent_option_id)
        .ok_or_else(|| "unknown ACP agent or profile".to_string())?;
    let cwd = resolve_acp_session_cwd(&project_root, &option, menu)?;
    let resume_id = resume_id.filter(|id| !id.trim().is_empty());
    let id = mobile_session_id("acp", request_id.as_deref())?;
    let title = resume_id
        .as_deref()
        .and_then(|resume_id| {
            smelt_core::session_control::list_history(&option, &cwd)
                .into_iter()
                .find(|session| session.resume_id == resume_id)
                .map(|session| session.display_title().to_string())
        })
        .unwrap_or_else(|| format!("{} conversation", option.label));
    let now = now_unix();
    if let Some(existing) = remote_sessions
        .sessions
        .iter()
        .find(|session| session.id == id)
    {
        let existing = existing
            .as_acp()
            .ok_or_else(|| "requestId is already bound to a different session kind".to_string())?;
        if existing.cwd != cwd
            || existing.agent_option_id != option.id
            || existing.resume_id != resume_id
        {
            return Err("requestId is already bound to different session parameters".to_string());
        }
        smelt_core::session_control::create_remote_acp_session(&existing)?;
        return Ok(id);
    }
    let session = smelt_core::session_control::RemoteAcpSession {
        id,
        cwd,
        title,
        agent_option_id: option.id,
        agent: option.kind,
        launch: option.launch,
        resume_id,
        created_at: now,
        lifecycle: Default::default(),
        hidden: false,
    };
    smelt_core::session_control::create_remote_acp_session(&session)?;
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
    images: Vec<smelt_core::acp_chat::AcpImage>,
}

#[derive(serde::Deserialize)]
struct ConfigOptionParams {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "configId")]
    config_id: String,
    #[serde(rename = "valueId")]
    value_id: String,
    #[serde(default)]
    boolean: Option<bool>,
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
    let mut lifecycle_rx = state.mobile_lifecycle.updates.subscribe();

    // 发送欢迎消息
    let welcome = serde_json::json!({
        "type": "connected",
        "writeEnabled": write_enabled(&state),
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
                        let resp = match refreshed_mobile_summaries(Arc::clone(
                            &state.mobile_lifecycle,
                        )).await {
                            Ok(sessions) => serde_json::json!({
                                "type": "sessions",
                                "sessions": sessions,
                            }),
                            Err(error) => serde_json::json!({"type": "error", "error": error}),
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::ListWorkspace => {
                        // 目录要读 SQLite、还可能重新探测 CLI，别占住这条连接的 executor。
                        let catalog = tokio::task::spawn_blocking(|| {
                            (mobile_agent_options(), mobile_launch_actions())
                        })
                        .await;
                        let resp = match (state.mobile_lifecycle.workspace_menu(), catalog) {
                            (Ok(menu), Ok((agents, launch_actions))) => serde_json::json!({
                                "type": "workspace",
                                "projects": smelt_core::session_control::workspace_projects(&menu),
                                "agents": agents,
                                "launchActions": launch_actions,
                            }),
                            (Err(error), _) => serde_json::json!({"type": "error", "error": error}),
                            (_, Err(error)) => serde_json::json!({
                                "type": "error",
                                "error": format!("failed to build the launch catalog: {error}"),
                            }),
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::ListAutomations => {
                        let resp = match state.mobile_lifecycle.automations_snapshot() {
                            Some(automations) => automation_catalog_response(&automations).await,
                            None => serde_json::json!({
                                "type": "error",
                                "error": "automation catalog unavailable; waiting for daemon subscription",
                            }),
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::SetAutomationEnabled { params } => {
                        let resp = if write_enabled(&state) {
                            submit_automation_catalog_command(
                                AutomationCommand::SetEnabled {
                                    automation_id: params.automation_id,
                                    enabled: params.enabled,
                                },
                            )
                            .await
                        } else {
                            serde_json::json!({"type": "error", "error": "write not enabled"})
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::RunAutomationOnce { params } => {
                        let resp = if write_enabled(&state) {
                            submit_automation_catalog_command(
                                AutomationCommand::RunOnce {
                                    automation_id: params.automation_id,
                                },
                            )
                            .await
                        } else {
                            serde_json::json!({"type": "error", "error": "write not enabled"})
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::ListSessionHistory { params } => {
                        let menu = match state.mobile_lifecycle.workspace_menu() {
                            Ok(menu) => menu,
                            Err(error) => {
                                let resp = serde_json::json!({"type": "error", "error": error});
                                let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                                continue;
                            }
                        };
                        let response = tokio::task::spawn_blocking(move || {
                            if !menu.projects.iter().any(|project| project.root == params.project_root) {
                                return Err("project is not in the Smelt workspace".to_string());
                            }
                            let option = smelt_core::session_control::find_agent_option(&params.agent_option_id)
                                .ok_or_else(|| "unknown ACP agent or profile".to_string())?;
                            Ok((params.project_root.clone(), params.agent_option_id.clone(), smelt_core::session_control::list_history(&option, &params.project_root)))
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
                    AcpWsRequest::RenameSessionHistory { params } => {
                        if !write_enabled(&state) {
                            let resp = serde_json::json!({"type": "error", "error": "write not enabled"});
                            let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                            continue;
                        }
                        let project_root = params.project_root.clone();
                        let agent_option_id = params.agent_option_id.clone();
                        let resume_id = params.resume_id.clone();
                        let response = tokio::task::spawn_blocking(move || rename_mobile_history_session(params)).await;
                        let resp = match response {
                            Ok(Ok((title, custom_title))) => serde_json::json!({
                                "type": "sessionHistoryRenamed",
                                "projectRoot": project_root,
                                "agentOptionId": agent_option_id,
                                "resumeId": resume_id,
                                "title": title,
                                "customTitle": custom_title,
                            }),
                            Ok(Err(error)) => serde_json::json!({"type": "error", "error": error}),
                            Err(error) => serde_json::json!({"type": "error", "error": format!("failed to rename session: {error}")}),
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::CreateSession { params } => {
                        if !write_enabled(&state) {
                            let resp = serde_json::json!({"type": "error", "error": "write not enabled"});
                            let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                            continue;
                        }
                        let (menu, remote_sessions) = match state.mobile_lifecycle.workspace_projection() {
                            Ok(projection) => projection,
                            Err(error) => {
                                let resp = serde_json::json!({"type": "error", "error": error});
                                let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                                continue;
                            }
                        };
                        let response = tokio::task::spawn_blocking(move || {
                            create_mobile_session(params, menu, remote_sessions)
                        })
                        .await;
                        let resp = match response {
                            Ok(Ok(id)) => serde_json::json!({"type": "sessionCreated", "sessionId": id}),
                            Ok(Err(error)) => serde_json::json!({"type": "error", "error": error}),
                            Err(error) => serde_json::json!({"type": "error", "error": format!("failed to create session: {error}")}),
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::DeleteSession { params } => {
                        if !write_enabled(&state) {
                            let resp = serde_json::json!({"type": "error", "error": "write not enabled"});
                            let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                            continue;
                        }
                        let id = params.session_id;
                        // Run 的生命周期由 daemon 拥有（claim / 投递 / 对账 / 回收）。
                        // 手机删掉执行现场等于从旁边把 daemon 的账本抽走，所以这里直接拒绝，
                        // 而不是让它删一半再由 daemon 发现会话没了。
                        if state.mobile_lifecycle.automation_run_id(&id).is_some() {
                            let resp = serde_json::json!({
                                "type": "error",
                                "error": "automation runs are managed by the desktop daemon",
                            });
                            let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                            continue;
                        }
                        let delete_id = id.clone();
                        let kinds = match state.mobile_lifecycle.delete_kinds(&id) {
                            Ok(kinds) => kinds,
                            Err(error) => {
                                let resp = serde_json::json!({"type": "error", "error": error});
                                let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                                continue;
                            }
                        };
                        let response = tokio::task::spawn_blocking(move || {
                            let kind = smelt_core::session_control::resolve_session_delete_kind(
                                kinds.0,
                                kinds.1,
                            )?;
                            smelt_core::session_control::delete_session(&delete_id, kind)
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

                        let handle = tokio::spawn(acp_watch_loop(
                            session_id,
                            history_session_id,
                            known_entries,
                            snapshot_revision,
                            tail_limit,
                            tx,
                            stop_rx,
                        ));

                        current_subscription =
                            Some((params.session_id.clone(), stop_tx, handle));

                        let resp = serde_json::json!({
                            "type": "subscribed",
                            "sessionId": params.session_id,
                        });
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::ListSessionSkills { params } => {
                        let session_id = params.session_id.clone();
                        let resp = match read_session_skills(&session_id).await {
                            Ok(value) => serde_json::json!({
                                "type": "sessionSkills",
                                "sessionId": session_id,
                                "agent": value.get("agent").cloned().unwrap_or(serde_json::Value::Null),
                                "supported": value.get("supported").and_then(serde_json::Value::as_bool).unwrap_or(false),
                                "skills": value.get("skills").cloned().unwrap_or_else(|| serde_json::json!([])),
                            }),
                            Err(error) => serde_json::json!({"type": "error", "error": error}),
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::LoadHistory { params } => {
                        let result = read_acp_history(
                            &params.session_id,
                            params.before_offset,
                            params.limit.clamp(1, 500),
                        ).await;
                        let response = match result {
                            Ok(line) => line,
                            Err(error) => serde_json::json!({"type": "error", "error": error}).to_string(),
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
                        if !write_enabled(&state) {
                            let err = mobile_send_message_error(
                                &smelt_core::conversation::ConversationSubmitError::rejected(
                                    "write not enabled",
                                ),
                                request_id.as_deref(),
                            );
                            let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(err.to_string().into())).await;
                            continue;
                        }

                        let session_id = params.session_id.clone();
                        let content = params.content.clone();
                        let images = params.images.clone();
                        let result = send_acp_message(&session_id, &content, images).await;

                        let resp = match result {
                            Ok(()) => serde_json::json!({
                                "type": "messageSent",
                                "ok": true,
                                "requestId": request_id,
                            }),
                            Err(error) => mobile_send_message_error(&error, request_id.as_deref()),
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::CancelTurn { params } => {
                        let resp = dispatch_mobile_action(
                            write_enabled(&state),
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
                                "boolean": params.boolean,
                            }
                        });
                        let resp = dispatch_mobile_action(
                            write_enabled(&state),
                            params.session_id,
                            action,
                            "configuration update",
                        ).await;
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::RespondApproval { params } => {
                        if !write_enabled(&state) {
                            let err = serde_json::json!({"type": "error", "error": "write not enabled"});
                            let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(err.to_string().into())).await;
                            continue;
                        }

                        let session_id = params.session_id.clone();
                        let option_key = params.option_key.clone();
                        let custom_text = params.custom_text.clone();
                        let result = respond_acp_approval(
                            &session_id,
                            &params.tool_call_id,
                            &option_key,
                            custom_text.as_deref(),
                        ).await;

                        let resp = match result {
                            Ok(()) => serde_json::json!({"type": "approvalResponded", "ok": true}),
                            Err(error) => serde_json::json!({"type": "error", "error": error}),
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
                            write_enabled(&state),
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
                            write_enabled(&state),
                            params.session_id,
                            action,
                            "elicitation text",
                        ).await;
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::SubmitElicitation { params } => {
                        let resp = dispatch_mobile_action(
                            write_enabled(&state),
                            params.session_id,
                            serde_json::json!("ElicitationSubmit"),
                            "elicitation submission",
                        ).await;
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::DismissElicitation { params } => {
                        let resp = dispatch_mobile_action(
                            write_enabled(&state),
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
                        let resp = match state.mobile_lifecycle.summaries() {
                            Ok(sessions) => serde_json::json!({
                                "type": "sessions",
                                "sessions": sessions,
                            }),
                            Err(error) => serde_json::json!({"type": "error", "error": error}),
                        };
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
                        let resp = match state.mobile_lifecycle.summaries() {
                            Ok(sessions) => serde_json::json!({
                                "type": "sessions",
                                "sessions": sessions,
                            }),
                            Err(error) => serde_json::json!({"type": "error", "error": error}),
                        };
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
    match send_acp_action(&session_id, action).await {
        Ok(()) => serde_json::json!({"type": "actionCompleted", "ok": true}),
        Err(error) => serde_json::json!({
            "type": "error",
            "error": format!("failed to dispatch {description}: {error}"),
        }),
    }
}

/// 后台任务：监听 smeltd 的 acp_watch 推送
async fn acp_watch_loop(
    session_id: String,
    history_session_id: Option<String>,
    known_entries: Option<usize>,
    snapshot_revision: Option<u64>,
    tail_limit: usize,
    tx: tokio::sync::mpsc::Sender<String>,
    stop: tokio::sync::watch::Receiver<bool>,
) {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    let Ok(stream) = tokio::net::UnixStream::connect(sock_path()).await else {
        return;
    };
    let (reader, mut writer) = stream.into_split();

    // 发送 acp_watch 请求
    let req = serde_json::json!({
        "op": DaemonOperation::AcpWatch,
        "id": session_id,
        "history_session_id": history_session_id,
        "known_entries": known_entries,
        "snapshot_revision": snapshot_revision,
        "tail_limit": tail_limit,
    });
    let req_line = format!("{req}\n");
    if writer.write_all(req_line.as_bytes()).await.is_err() {
        return;
    }

    let mut reader = tokio::io::BufReader::new(reader);
    let mut line = String::new();
    let mut stop_rx = stop.clone();

    loop {
        if *stop.borrow() {
            break;
        }

        line.clear();
        tokio::select! {
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    break;
                }
            }
            res = reader.read_line(&mut line) => {
                match res {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let trimmed = line.trim();
                        if !trimmed.is_empty()
                            && tx.send(tag_snapshot_line(trimmed, &session_id)).await.is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

/// 问一次 daemon：这场对话现在加载了哪些技能。技能要读本机文件系统，只有 daemon
/// 手上有这个会话完整的 launch spec（含 `--skill` 参数）和 cwd，网关不自己猜。
async fn read_session_skills(session_id: &str) -> Result<serde_json::Value, String> {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    let stream = tokio::net::UnixStream::connect(sock_path())
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    let (reader, mut writer) = stream.into_split();
    let request = serde_json::json!({
        "op": DaemonOperation::AcpSkills,
        "id": session_id,
    });
    let req_line = format!("{request}\n");
    tokio::time::timeout(
        Duration::from_secs(5),
        writer.write_all(req_line.as_bytes()),
    )
    .await
    .map_err(|_| "write timeout".to_string())?
    .map_err(|e| format!("write failed: {e}"))?;

    let mut reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut response))
        .await
        .map_err(|_| "read timeout".to_string())?
        .map_err(|e| format!("read failed: {e}"))?;
    let value: serde_json::Value = serde_json::from_str(response.trim())
        .map_err(|e| format!("invalid skills response: {e}"))?;
    if value["ok"].as_bool() == Some(true) {
        Ok(value)
    } else {
        Err(value["error"]
            .as_str()
            .unwrap_or("failed to read session skills")
            .to_string())
    }
}

async fn read_acp_history(
    session_id: &str,
    before_offset: usize,
    limit: usize,
) -> Result<String, String> {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    let stream = tokio::net::UnixStream::connect(sock_path())
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    let (reader, mut writer) = stream.into_split();
    let request = serde_json::json!({
        "op": DaemonOperation::AcpSnapshot,
        "id": session_id,
        "before": before_offset,
        "limit": limit,
    });
    let req_line = format!("{request}\n");
    tokio::time::timeout(
        Duration::from_secs(5),
        writer.write_all(req_line.as_bytes()),
    )
    .await
    .map_err(|_| "write timeout".to_string())?
    .map_err(|e| format!("write failed: {e}"))?;

    let mut reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut response))
        .await
        .map_err(|_| "read timeout".to_string())?
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

async fn send_acp_action(session_id: &str, action: serde_json::Value) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    let stream = tokio::net::UnixStream::connect(sock_path())
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    let (reader, mut writer) = stream.into_split();

    let req = serde_json::json!({
        "op": DaemonOperation::AcpAction,
        "id": session_id,
        "action": action,
    });
    let req_line = format!("{req}\n");
    tokio::time::timeout(
        Duration::from_secs(5),
        writer.write_all(req_line.as_bytes()),
    )
    .await
    .map_err(|_| "write timeout".to_string())?
    .map_err(|e| format!("write failed: {e}"))?;

    let mut reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut response))
        .await
        .map_err(|_| "read timeout".to_string())?
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

fn mobile_send_message_error(
    error: &smelt_core::conversation::ConversationSubmitError,
    request_id: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "type": "error",
        "error": error.message,
        "errorKind": error.kind,
        "requestId": request_id,
    })
}

/// 发送 ACP 消息，不占用或替换 PC GUI 的 control client。
async fn send_acp_message(
    session_id: &str,
    content: &str,
    images: Vec<smelt_core::acp_chat::AcpImage>,
) -> Result<(), smelt_core::conversation::ConversationSubmitError> {
    use smelt_core::conversation::ConversationSubmitError;
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    let stream = tokio::net::UnixStream::connect(sock_path())
        .await
        .map_err(|error| ConversationSubmitError::unknown(format!("connect failed: {error}")))?;
    let (reader, mut writer) = stream.into_split();
    let request = serde_json::json!({
        "op": DaemonOperation::AcpSubmitInput,
        "id": session_id,
        "input": smelt_core::conversation::ConversationInput::new(content.to_string(), images),
    });
    let request_line = format!("{request}\n");
    tokio::time::timeout(
        Duration::from_secs(35),
        writer.write_all(request_line.as_bytes()),
    )
    .await
    .map_err(|_| ConversationSubmitError::unknown("write timeout"))?
    .map_err(|error| ConversationSubmitError::unknown(format!("write failed: {error}")))?;
    let mut reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(35), reader.read_line(&mut response))
        .await
        .map_err(|_| ConversationSubmitError::unknown("read timeout"))?
        .map_err(|error| ConversationSubmitError::unknown(format!("read failed: {error}")))?;
    let response: serde_json::Value = serde_json::from_str(&response)
        .map_err(|error| ConversationSubmitError::unknown(format!("invalid response: {error}")))?;
    smelt_core::session_control::parse_conversation_submit_response(response).map(|_| ())
}

/// 响应 ACP 审批请求
async fn respond_acp_approval(
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
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手机只认 `section` + `target` 两个字段来分组和分派。这两个名字一旦漂了，
    /// 选择器会静默变成空列表——所以把下发的 JSON 形状钉死在测试里。
    #[test]
    fn launch_actions_carry_their_section_and_target() {
        use smelt_core::new_session::{
            NewSessionPreferences, build_sections, new_session_actions, stored_launch_entries,
        };

        let actions = new_session_actions(&[], &stored_launch_entries(), None);
        let preferences = NewSessionPreferences {
            pinned: vec![smelt_core::new_session::BLANK_TERMINAL_KEY.to_string()],
        };
        let rows = launch_actions_json(build_sections(&actions, &preferences));

        let blank = rows
            .iter()
            .find(|row| row["key"] == smelt_core::new_session::BLANK_TERMINAL_KEY)
            .expect("空白终端永远在目录里");
        assert_eq!(blank["section"], "common", "Pin 过的动作要落在常用组");
        assert_eq!(blank["target"], "blankTerminal");
        assert_eq!(blank["kind"], "terminal");
        assert_eq!(blank["pinned"], true);

        for row in &rows {
            let section = row["section"].as_str().expect("每行都要带分组");
            assert!(
                matches!(section, "common" | "terminal" | "conversation"),
                "未知分组 {section}"
            );
            assert!(row["target"].is_string(), "每行都要带 target");
            assert!(row["key"].is_string() && row["label"].is_string());
        }

        assert!(
            rows.iter().filter(|row| row["section"] == "common").count() == 1,
            "没 Pin 的动作不该跑进常用组"
        );
    }

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
            max_scrollback_lines: None,
        };
        assert!(zero.normalized().is_err());
    }

    /// scrollback 预算是消费端声明的：手机说要多少就带多少，超了封顶，不传就是
    /// 老客户端——那时必须仍然是「daemon 给全量」，不能悄悄替它砍掉历史。
    #[test]
    fn attach_carries_the_consumer_scrollback_budget() {
        let parse = |params: serde_json::Value| -> TerminalGeometryParams {
            match serde_json::from_value::<TerminalWsRequest>(
                serde_json::json!({"method": "attach", "params": params}),
            )
            .unwrap()
            {
                TerminalWsRequest::Attach { params } => params.normalized().unwrap(),
                _ => panic!("expected terminal attach"),
            }
        };

        let budgeted = parse(serde_json::json!({
            "cols": 80, "rows": 24, "maxScrollbackLines": 600
        }));
        assert_eq!(budgeted.max_scrollback_lines, Some(600));

        let greedy = parse(serde_json::json!({
            "cols": 80, "rows": 24, "maxScrollbackLines": 10_000_000_u32
        }));
        assert_eq!(
            greedy.max_scrollback_lines,
            Some(MAX_TERMINAL_SCROLLBACK_LINES)
        );

        let legacy = parse(serde_json::json!({"cols": 80, "rows": 24}));
        assert_eq!(legacy.max_scrollback_lines, None);
    }

    #[tokio::test]
    async fn terminal_resize_uses_the_geometry_lease_frame() {
        use tokio::io::AsyncReadExt as _;

        let (mut writer, mut reader) = tokio::net::UnixStream::pair().unwrap();
        let geometry = TerminalGeometryParams {
            cols: 49,
            rows: 47,
            cell_width: 8,
            cell_height: 15,
            max_scrollback_lines: None,
        };
        write_terminal_resize_frame(&mut writer, geometry)
            .await
            .unwrap();

        let mut frame = [0u8; 21];
        reader.read_exact(&mut frame).await.unwrap();
        assert_eq!(frame[0], 1);
        assert_eq!(u32::from_be_bytes(frame[1..5].try_into().unwrap()), 16);
        let values: Vec<u32> = frame[5..]
            .chunks_exact(4)
            .map(|bytes| u32::from_be_bytes(bytes.try_into().unwrap()))
            .collect();
        assert_eq!(values, vec![49, 47, 8, 15]);
    }

    #[test]
    fn send_message_error_preserves_rejected_and_unknown_kinds() {
        let rejected = mobile_send_message_error(
            &smelt_core::session_control::parse_conversation_submit_response(serde_json::json!({
                "ok": false,
                "error": {"kind": "rejected", "message": "not accepted"}
            }))
            .unwrap_err(),
            Some("request-1"),
        );
        assert_eq!(rejected["type"], "error");
        assert_eq!(rejected["error"], "not accepted");
        assert_eq!(rejected["errorKind"], "rejected");
        assert_eq!(rejected["requestId"], "request-1");

        let unknown = mobile_send_message_error(
            &smelt_core::session_control::parse_conversation_submit_response(serde_json::json!({
                "ok": false,
                "error": {"kind": "unknown", "message": "connection closed"}
            }))
            .unwrap_err(),
            None,
        );
        assert_eq!(unknown["error"], "connection closed");
        assert_eq!(unknown["errorKind"], "unknown");

        let legacy = mobile_send_message_error(
            &smelt_core::session_control::parse_conversation_submit_response(serde_json::json!({
                "ok": false,
                "error": "legacy string"
            }))
            .unwrap_err(),
            None,
        );
        assert_eq!(legacy["error"], "legacy string");
        assert_eq!(
            legacy["errorKind"], "unknown",
            "旧 daemon 的字符串错误必须保守成 Unknown，不能报成未发送"
        );
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
                assert_eq!(params.boolean, None);
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

        let skills: AcpWsRequest = serde_json::from_value(serde_json::json!({
            "method": "listSessionSkills",
            "params": {"sessionId": "session-1"}
        }))
        .unwrap();
        match skills {
            AcpWsRequest::ListSessionSkills { params } => {
                assert_eq!(params.session_id, "session-1");
            }
            _ => panic!("expected listSessionSkills"),
        }

        let rename: AcpWsRequest = serde_json::from_value(serde_json::json!({
            "method": "renameSessionHistory",
            "params": {
                "projectRoot": "/repo/smelt",
                "agentOptionId": "profile:quant",
                "resumeId": "history-1",
                "title": "夜里那次排查"
            }
        }))
        .unwrap();
        match rename {
            AcpWsRequest::RenameSessionHistory { params } => {
                assert_eq!(params.resume_id, "history-1");
                assert_eq!(params.title.as_deref(), Some("夜里那次排查"));
            }
            _ => panic!("expected renameSessionHistory"),
        }

        // 省掉 title = 恢复默认名称，不能被当成缺字段解析失败。
        let reset: AcpWsRequest = serde_json::from_value(serde_json::json!({
            "method": "renameSessionHistory",
            "params": {
                "projectRoot": "/repo/smelt",
                "agentOptionId": "codex",
                "resumeId": "history-1"
            }
        }))
        .unwrap();
        match reset {
            AcpWsRequest::RenameSessionHistory { params } => assert_eq!(params.title, None),
            _ => panic!("expected renameSessionHistory"),
        }

        let create: AcpWsRequest = serde_json::from_value(serde_json::json!({
            "method": "createSession",
            "params": {
                "projectRoot": "/repo/smelt",
                "agentOptionId": "codex",
                "resumeId": "history-1",
                "requestId": "550e8400-e29b-41d4-a716-446655440000"
            }
        }))
        .unwrap();
        match create {
            AcpWsRequest::CreateSession { params } => {
                assert_eq!(params.agent_option_id.as_deref(), Some("codex"));
                assert_eq!(params.resume_id.as_deref(), Some("history-1"));
                assert_eq!(params.kind, None);
                assert_eq!(
                    params.request_id.as_deref(),
                    Some("550e8400-e29b-41d4-a716-446655440000")
                );
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

    #[test]
    fn create_request_id_maps_retries_to_the_same_kind_scoped_session_id() {
        let request_id = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(
            mobile_session_id("acp", Some(request_id)).unwrap(),
            mobile_session_id("acp", Some(request_id)).unwrap()
        );
        assert_ne!(
            mobile_session_id("acp", Some(request_id)).unwrap(),
            mobile_session_id("term", Some(request_id)).unwrap()
        );
        assert!(mobile_session_id("acp", Some("not-a-uuid")).is_err());
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
            turn_events: true,
            ..Default::default()
        }
    }

    fn agent_option_for_test(id: &str) -> smelt_core::session_control::AcpAgentOption {
        smelt_core::session_control::AcpAgentOption {
            id: id.to_string(),
            kind: smelt_core::agent_kind::ConversationAgentKind::Pi
                .id()
                .to_string(),
            label: "写作助手".to_string(),
            profile: false,
            agent_definition_id: smelt_core::session_control::agent_definition_id_from_option_id(
                id,
            )
            .map(str::to_string),
            launch: smelt_core::agent_kind::ConversationLaunchSpec::from_command("pi".to_string()),
            history_dir: None,
        }
    }

    #[test]
    fn a_project_conversation_still_has_to_name_a_project_in_the_workspace() {
        let menu = mobile_menu_for_test();
        let option = agent_option_for_test("pi");

        assert_eq!(
            resolve_acp_session_cwd("/tmp/mobile-project", &option, &menu),
            Ok("/tmp/mobile-project".to_string())
        );
        assert!(resolve_acp_session_cwd("/tmp/elsewhere", &option, &menu).is_err());
        // 裸引擎没有自己的 space，缺项目就只能报错，不能默默挑一个目录。
        assert!(resolve_acp_session_cwd("", &option, &menu).is_err());
        assert!(resolve_acp_session_cwd("   ", &option, &menu).is_err());
    }

    #[test]
    fn an_agent_conversation_without_a_project_lands_in_the_agent_space() {
        let menu = mobile_menu_for_test();
        let option = agent_option_for_test("agent:writer");

        let cwd = resolve_acp_session_cwd("", &option, &menu).expect("agent space");
        assert!(std::path::Path::new(&cwd).ends_with("writer"));
        assert!(smelt_core::agent_definition_store::is_agent_space(
            std::path::Path::new(&cwd)
        ));

        // 手机上从项目里开的智能体对话照旧跟着项目走。
        assert_eq!(
            resolve_acp_session_cwd("/tmp/mobile-project", &option, &menu),
            Ok("/tmp/mobile-project".to_string())
        );
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

        let summary = mobile_summary(
            menu.session(&session.id).unwrap(),
            Some(&session),
            &attention,
            None,
            None,
        );
        assert_eq!(summary.kind, WorkspaceMenuSessionKind::Terminal);
        assert_eq!(summary.title, "修复移动端项目列表");
        assert_eq!(summary.leaf_order, 1);
    }

    /// 名册是菜单，不是 daemon 的活动记录。
    ///
    /// 一条刚在 PC 上开起来、还没跑过任何东西的会话，daemon 里没有它的运行态。
    /// 投影要是遍历 daemon 会话表，这条会话在手机上就彻底不存在——桌面侧栏看得
    /// 见、手机上找不到，直到它第一次上报事件才凭空出现。
    #[test]
    fn a_session_that_has_never_run_is_still_on_the_roster() {
        let hub = mobile_lifecycle_hub_for_test();
        let mut menu = mobile_menu_for_test();
        menu.sessions.push(WorkspaceMenuSession {
            id: "terminal-never-ran".into(),
            kind: WorkspaceMenuSessionKind::Terminal,
            title: "copilot --allow-all".into(),
            custom_title: false,
            cwd: Some("/tmp/mobile-project".into()),
            project_root: Some("/tmp/mobile-project".into()),
            project_title: Some("mobile-project".into()),
            project_order: 0,
            session_order: 3,
            leaf_order: 0,
            agent: Some("copilot".into()),
        });
        // daemon 只知道另一条会话：新开的那条还没上报过任何事件。
        hub.apply_snapshot(
            vec![mobile_daemon_state(DaemonPhase::Thinking)],
            Some(RemoteSessionSnapshot::default()),
            Some(menu.clone()),
            None,
        );

        let summaries = hub.summaries().unwrap();
        let ids: Vec<_> = summaries.iter().map(|s| s.id.as_str()).collect();
        assert!(
            ids.contains(&"terminal-never-ran"),
            "菜单里的会话不该因为没有运行态而消失: {ids:?}"
        );

        let never_ran = summaries
            .iter()
            .find(|s| s.id == "terminal-never-ran")
            .unwrap();
        assert_eq!(never_ran.title, "copilot --allow-all");
        assert_eq!(never_ran.agent, "copilot");
        assert_eq!(never_ran.kind, WorkspaceMenuSessionKind::Terminal);
        // 「还没动过」不是失败态，也不该被编造成某个时间点：移动端的分诊屏正是
        // 靠 `updated_at == 0` 把这些会话排除在外的。
        assert_eq!(never_ran.phase, "idle");
        assert_eq!(never_ran.status, "idle");
        assert_eq!(never_ran.updated_at, 0);
        assert!(never_ran.detail.is_none());

        // 有运行态的那条照常带上它。
        let running = summaries
            .iter()
            .find(|s| s.id == "acp-session-mobile")
            .unwrap();
        assert_eq!(running.phase, "thinking");
        assert_eq!(running.updated_at, 42);
    }

    /// 删掉一条会话，它就得立刻从名册上消失。
    ///
    /// 名册改成以菜单为准之后，只清运行态是不够的：菜单缓存和远端会话目录里各有
    /// 一份，任何一份没划掉，被删的会话都会继续挂在列表上，直到 daemon 推来下一
    /// 份快照——用户看到的是「删了但没删掉」。
    #[test]
    fn a_deleted_session_leaves_the_roster_immediately() {
        let hub = mobile_lifecycle_hub_for_test();
        let mut menu = mobile_menu_for_test();
        menu.sessions.push(WorkspaceMenuSession {
            id: "remote-term".into(),
            kind: WorkspaceMenuSessionKind::Terminal,
            title: "Mobile terminal".into(),
            custom_title: false,
            cwd: Some("/tmp/mobile-project".into()),
            project_root: Some("/tmp/mobile-project".into()),
            project_title: Some("mobile-project".into()),
            project_order: 0,
            session_order: 3,
            leaf_order: 0,
            agent: None,
        });
        hub.apply_snapshot(
            vec![mobile_daemon_state(DaemonPhase::Thinking)],
            Some(RemoteSessionSnapshot {
                revision: 1,
                sessions: vec![RemoteSessionRecord {
                    kind: RemoteSessionKind::Terminal,
                    id: "remote-term".into(),
                    cwd: "/tmp/mobile-project".into(),
                    title: "Mobile terminal".into(),
                    agent_option_id: None,
                    agent: None,
                    launch: None,
                    resume_id: None,
                    created_at: 1,
                    lifecycle: RemoteSessionLifecycle::Active,
                    hidden: false,
                }],
            }),
            Some(menu),
            None,
        );
        assert!(
            hub.summaries()
                .unwrap()
                .iter()
                .any(|s| s.id == "remote-term")
        );

        hub.remove_session("remote-term");

        let ids: Vec<_> = hub.summaries().unwrap().into_iter().map(|s| s.id).collect();
        assert!(
            !ids.contains(&"remote-term".to_string()),
            "删掉的会话还挂在名册上: {ids:?}"
        );
    }

    /// 只有 TUI 的 CLI（Antigravity / Crush）不在 ACP 表里。掉到 "other" 的后果
    /// 是用户在手机上看不出这条会话是谁在跑，也拿不到那家的图标。
    #[test]
    fn agent_from_launch_falls_back_to_the_terminal_only_clis() {
        assert_eq!(agent_from_launch("claude --acp"), "claude");
        // Antigravity 的 CLI 叫 `agy`，命令里根本没有 "antigravity" 这个词——
        // 只靠 ACP 表的关键字匹配永远认不出它。
        assert_eq!(
            agent_from_launch("agy --dangerously-skip-permissions"),
            "antigravity"
        );
        assert_eq!(agent_from_launch("crush"), "crush");
        assert_eq!(agent_from_launch("zsh -l"), "other");
    }

    #[test]
    fn mobile_summary_uses_hook_provider_for_an_agent_started_in_a_bare_shell() {
        let mut session = mobile_daemon_state(DaemonPhase::Thinking);
        session.id = "manual-opencode".into();
        session.launch = None;
        session.provider = Some("opencode".into());
        let menu = WorkspaceMenuSnapshot::current(
            vec![],
            vec![WorkspaceMenuSession {
                id: session.id.clone(),
                kind: WorkspaceMenuSessionKind::Terminal,
                title: "OpenCode task".into(),
                custom_title: false,
                cwd: session.cwd.clone(),
                project_root: None,
                project_title: None,
                project_order: 0,
                session_order: 0,
                leaf_order: 0,
                agent: None,
            }],
        );

        let summary = mobile_summary(
            menu.session(&session.id).unwrap(),
            Some(&session),
            &AttentionStore::default(),
            None,
            None,
        );
        assert_eq!(summary.agent, "opencode");
    }

    #[test]
    fn mobile_summary_uses_copilot_daemon_title_without_spinner() {
        let mut session = mobile_daemon_state(DaemonPhase::Thinking);
        session.id = "terminal-copilot".into();
        session.title = Some(
            "Implement Nio Feature Request - Verifying frontend-only gates - GitHub Copilot".into(),
        );
        let menu = WorkspaceMenuSnapshot::current(
            vec![],
            vec![WorkspaceMenuSession {
                id: session.id.clone(),
                kind: WorkspaceMenuSessionKind::Terminal,
                title: "GitHub Copilot".into(),
                custom_title: false,
                cwd: session.cwd.clone(),
                project_root: None,
                project_title: None,
                project_order: 0,
                session_order: 0,
                leaf_order: 0,
                agent: Some("copilot".into()),
            }],
        );
        let summary = mobile_summary(
            menu.session(&session.id).unwrap(),
            Some(&session),
            &AttentionStore::default(),
            None,
            None,
        );
        assert_eq!(
            summary.title,
            "Implement Nio Feature Request - Verifying frontend-only gates - GitHub Copilot"
        );
    }

    #[test]
    fn mobile_summary_hides_an_unproven_activity_phase() {
        let mut session = mobile_daemon_state(DaemonPhase::Thinking);
        session.turn_events = false;
        session.title = Some("Thinking - TSLA 趋势且入场与 put 判断 - grok".into());
        session.pending_question = Some("run_terminal_command".into());
        let menu = mobile_menu_for_test();

        let summary = mobile_summary(
            menu.session(&session.id).unwrap(),
            Some(&session),
            &AttentionStore::default(),
            None,
            None,
        );
        assert_eq!(summary.phase, "idle");
        assert_eq!(summary.status, "idle");
        assert_eq!(summary.detail, None);
        assert_eq!(
            summary.title,
            "Thinking - TSLA 趋势且入场与 put 判断 - grok"
        );
    }

    #[test]
    fn mobile_summary_preserves_pc_manual_title() {
        let mut menu = mobile_menu_for_test();
        menu.sessions[0].title = "用户重命名".into();
        menu.sessions[0].custom_title = true;
        let session = mobile_daemon_state(DaemonPhase::Idle);

        let summary = mobile_summary(
            menu.session(&session.id).unwrap(),
            Some(&session),
            &AttentionStore::default(),
            None,
            None,
        );
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
    fn gateway_lifecycle_uses_formal_first_party_subscription_and_shared_events() {
        let first = DaemonStateEventClient::remote_gateway();
        let second = DaemonStateEventClient::remote_gateway();
        let request: serde_json::Value =
            serde_json::from_str(first.request_line().unwrap().trim()).unwrap();
        assert_eq!(request["op"], "event_subscribe");
        assert_eq!(request["auth"]["type"], "first_party");
        assert_eq!(request["auth"]["kind"], "remote_gateway");
        let topics = request["request"]["subscription"]["topics"]
            .as_array()
            .unwrap();
        assert_eq!(topics.len(), 5);
        assert!(!topics.iter().any(|topic| topic == "tasks.changed"));
        assert!(topics.iter().any(|topic| topic == "automations.changed"));
        assert_ne!(
            first.handshake().request.subscription.id,
            second.handshake().request.subscription.id
        );

        let hub = mobile_lifecycle_hub_for_test();
        apply_mobile_lifecycle_event(
            &hub,
            DaemonStateEvent::Snapshot {
                sessions: vec![mobile_daemon_state(DaemonPhase::Thinking)],
                remote_sessions: Some(RemoteSessionSnapshot::default()),
                workspace_menu: Some(mobile_menu_for_test()),
                automations: None,
            },
        );
        assert_eq!(hub.state.lock().unwrap().sessions.len(), 1);
        apply_mobile_lifecycle_event(
            &hub,
            DaemonStateEvent::Update(mobile_daemon_state(DaemonPhase::Succeeded)),
        );
        assert_eq!(
            hub.state
                .lock()
                .unwrap()
                .sessions
                .get("acp-session-mobile")
                .unwrap()
                .phase,
            DaemonPhase::Succeeded
        );
        apply_mobile_lifecycle_event(
            &hub,
            DaemonStateEvent::Removed {
                id: "acp-session-mobile".to_string(),
            },
        );
        assert!(hub.state.lock().unwrap().sessions.is_empty());
    }

    #[test]
    fn mobile_workspace_menu_uses_subscribed_desktop_snapshot() {
        let desktop = WorkspaceMenuSnapshot::current(
            vec![WorkspaceMenuProject {
                root: "/repo/two".into(),
                title: "repo · two".into(),
                order: 0,
            }],
            vec![WorkspaceMenuSession {
                id: "stable-acp-id".into(),
                kind: WorkspaceMenuSessionKind::Acp,
                title: "PC display title".into(),
                custom_title: true,
                cwd: Some("/repo/two".into()),
                project_root: Some("/repo/two".into()),
                project_title: Some("repo · two".into()),
                project_order: 0,
                session_order: 3,
                leaf_order: 0,
                agent: Some("codex".into()),
            }],
        );
        let menu = mobile_workspace_menu(&desktop, &RemoteSessionSnapshot::default());
        let session = menu.acp_session("stable-acp-id").unwrap();
        assert_eq!(session.title, "PC display title");
        assert_eq!(session.project_title.as_deref(), Some("repo · two"));
        assert_eq!(session.session_order, 3);
        assert_eq!(session.leaf_order, 0);
    }

    fn automation_run_for_test(status: AutomationRunStatus) -> AutomationRun {
        AutomationRun {
            id: "run-1".into(),
            automation_id: "automation-1".into(),
            context: AutomationRunContext {
                automation_name: "每日晨报".into(),
                action: AutomationAction::agent("agent-1", Some("拉取昨日 PR 列表".into())),
                cwd: "/home/me/.smelt/workspaces/automations/automation-1".into(),
                trigger_payload: None,
                prompt: Some("拉取昨日 PR 列表".into()),
                agent_definition_name: Some("晨报助手".into()),
                engine_kind_id: Some("pi".into()),
                agent_instructions: None,
            },
            source: AutomationRunSource::Scheduled,
            scheduled_for: None,
            status,
            created_at: 100,
            started_at: Some(120),
            delivery_attempt_at: None,
            delivery_attempts: 0,
            finished_at: None,
            session_id: Some("acp-automation-1".into()),
            provider_session_id: None,
            output: None,
            error: None,
            runtime_released_at: None,
        }
    }

    fn automation_file_for_test(status: AutomationRunStatus) -> AutomationFile {
        AutomationFile {
            schema_version: 1,
            store_id: "store".into(),
            revision: 1,
            timezone_fingerprint: String::new(),
            automations: Vec::new(),
            states: Vec::new(),
            runs: vec![automation_run_for_test(status)],
            store_error: None,
            webhook_base_url: None,
        }
    }

    fn agent_definitions_for_test() -> Vec<AgentDefinition> {
        vec![AgentDefinition {
            id: "agent-1".into(),
            name: "晨报助手".into(),
            description: String::new(),
            engine_kind_id: "pi".into(),
            prompt: "每天汇总昨日进展".into(),
            plugins: vec!["skill:research".into()],
            context_folders: vec!["/home/me/notes".into()],
            context_links: Vec::new(),
            ..Default::default()
        }]
    }

    fn scheduled_automation_for_test() -> Automation {
        Automation {
            id: "automation-1".into(),
            name: "每日晨报".into(),
            enabled: true,
            workspace_dir: Some("/home/me/.smelt/workspaces/automations/automation-1".into()),
            trigger: AutomationTrigger {
                schedules: vec![AutomationSchedule::Daily { hour: 9, minute: 0 }],
                ..Default::default()
            },
            action: AutomationAction::agent("agent-1", Some("拉取昨日 PR 列表".into())),
            sinks: Vec::new(),
        }
    }

    #[test]
    fn automation_catalog_pairs_each_automation_with_its_agent_and_last_run() {
        let mut file = automation_file_for_test(AutomationRunStatus::AwaitingApproval);
        file.automations = vec![scheduled_automation_for_test()];
        file.states = vec![AutomationState {
            automation_id: "automation-1".into(),
            next_run_at: Some(9_000),
            last_run_id: Some("run-1".into()),
        }];

        let catalog = automation_catalog(&file, &agent_definitions_for_test());

        assert_eq!(catalog.len(), 1);
        let entry = &catalog[0];
        assert_eq!(entry.name, "每日晨报");
        assert_eq!(entry.schedules.len(), 1);
        assert!(!entry.webhook);
        assert_eq!(entry.action_kind, "agent");
        assert_eq!(entry.agent_name.as_deref(), Some("晨报助手"));
        assert_eq!(entry.next_run_at, Some(9_000));
        let last_run = entry.last_run.as_ref().expect("last run projected");
        assert_eq!(last_run.status, AutomationRunStatus::AwaitingApproval);
        assert_eq!(last_run.session_id.as_deref(), Some("acp-automation-1"));
    }

    #[test]
    fn automation_catalog_falls_back_to_the_newest_run_when_state_lags() {
        let mut file = automation_file_for_test(AutomationRunStatus::Running);
        file.automations = vec![scheduled_automation_for_test()];
        // 刚触发、状态还没登记 last_run_id：目录里也必须看得见这次运行。
        file.states = vec![AutomationState {
            automation_id: "automation-1".into(),
            next_run_at: None,
            last_run_id: None,
        }];

        let catalog = automation_catalog(&file, &agent_definitions_for_test());

        let last_run = catalog[0].last_run.as_ref().expect("newest run projected");
        assert_eq!(last_run.run_id, "run-1");
    }

    #[test]
    fn automation_catalog_never_leaks_webhook_credentials() {
        let mut automation = scheduled_automation_for_test();
        automation.trigger = AutomationTrigger {
            webhooks: vec![WebhookIngress {
                endpoint: "https://hooks.example.com/very-secret-path".into(),
                secret: Some("s3cr3t-token".into()),
            }],
            ..Default::default()
        };
        let mut file = automation_file_for_test(AutomationRunStatus::Completed);
        file.automations = vec![automation];

        let catalog = automation_catalog(&file, &agent_definitions_for_test());
        let encoded = serde_json::to_string(&catalog).unwrap();

        assert!(catalog[0].webhook);
        assert!(catalog[0].schedules.is_empty());
        assert!(!encoded.contains("hooks.example.com"));
        assert!(!encoded.contains("s3cr3t-token"));
    }

    #[test]
    fn automation_catalog_keeps_the_row_when_its_agent_definition_is_gone() {
        let mut file = automation_file_for_test(AutomationRunStatus::Failed);
        file.automations = vec![scheduled_automation_for_test()];

        let catalog = automation_catalog(&file, &[]);

        // 定义被删掉是排查线索本身，不能让这条自动化从手机上消失。
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].agent_name, None);
    }

    #[test]
    fn automation_requests_parse_from_the_mobile_wire_format() {
        let toggle: AcpWsRequest = serde_json::from_value(serde_json::json!({
            "method": "setAutomationEnabled",
            "params": {"automationId": "automation-1", "enabled": false},
        }))
        .unwrap();
        assert!(matches!(
            toggle,
            AcpWsRequest::SetAutomationEnabled { params } if !params.enabled
        ));
        let run: AcpWsRequest = serde_json::from_value(serde_json::json!({
            "method": "runAutomationOnce",
            "params": {"automationId": "automation-1"},
        }))
        .unwrap();
        assert!(matches!(run, AcpWsRequest::RunAutomationOnce { .. }));
        let list: AcpWsRequest =
            serde_json::from_value(serde_json::json!({"method": "listAutomations"})).unwrap();
        assert!(matches!(list, AcpWsRequest::ListAutomations));
    }

    /// 归属必须一路投到手机：cwd 是任意项目目录时也要认得出主人，否则指挥台
    /// 里一堆智能体对话看起来跟普通会话一样，分不出是谁在跑。
    #[test]
    fn agent_conversations_carry_their_definition_id_regardless_of_working_directory() {
        let hub = mobile_lifecycle_hub_for_test();
        let mut menu = mobile_menu_for_test();
        // 开在一个普通仓库里的智能体对话——cwd 反推做不到的那一半。
        menu.sessions[0].cwd = Some("/Users/me/Desktop/novel".into());
        hub.apply_workspace_menu(menu);
        hub.apply_remote_sessions(RemoteSessionSnapshot::default());
        hub.apply_update(mobile_daemon_state(DaemonPhase::Idle));

        // 存档还没读进来时不硬猜归属。
        let before = hub.summaries().unwrap();
        assert_eq!(before[0].agent_definition_id, None);

        hub.apply_agent_definition_ids(std::collections::BTreeMap::from([(
            "acp-session-mobile".to_string(),
            "writer".to_string(),
        )]));

        let after = hub.summaries().unwrap();
        assert_eq!(after[0].agent_definition_id.as_deref(), Some("writer"));
        // 只投 id 不投名字：名字在移动端拿定义表配，热路径不读存档。
        let json = serde_json::to_value(&after[0]).unwrap();
        assert_eq!(json["agent_definition_id"], "writer");
    }

    /// 菜单变动才是「新开的对话该有归属了」的信号；纯相位更新不该逼着网关
    /// 每秒重读一次存档。
    #[test]
    fn only_menu_shaped_events_ask_for_a_binding_refresh() {
        let hub = mobile_lifecycle_hub_for_test();

        assert!(apply_mobile_lifecycle_event(
            &hub,
            DaemonStateEvent::WorkspaceMenu(mobile_menu_for_test()),
        ));
        assert!(!apply_mobile_lifecycle_event(
            &hub,
            DaemonStateEvent::Update(mobile_daemon_state(DaemonPhase::Thinking)),
        ));
    }

    fn hidden_automation_remote_sessions() -> RemoteSessionSnapshot {
        RemoteSessionSnapshot {
            revision: 1,
            sessions: vec![RemoteSessionRecord {
                kind: RemoteSessionKind::Acp,
                id: "acp-automation-1".into(),
                cwd: "/home/me/.smelt/workspaces/automations/automation-1".into(),
                title: "每日晨报 · 晨报助手".into(),
                agent_option_id: Some("pi".into()),
                agent: Some("pi".into()),
                launch: None,
                resume_id: None,
                created_at: 100,
                lifecycle: RemoteSessionLifecycle::Active,
                hidden: true,
            }],
        }
    }

    /// 后台 Run 的会话在 daemon 目录里是 hidden，桌面侧栏看不到它；手机指挥台必须
    /// 看得到，否则「自动化半夜停下来等审批」这件事在手机上根本不存在。
    #[test]
    fn hidden_automation_run_sessions_reach_the_mobile_console_without_a_project() {
        let mut menu = WorkspaceMenuSnapshot::current(vec![], vec![]);
        let remotes = hidden_automation_remote_sessions();
        assert!(
            mobile_workspace_menu(&menu, &remotes)
                .session("acp-automation-1")
                .is_none(),
            "hidden 会话不能走可见远程目录那条路进来"
        );

        append_automation_run_sessions(
            &mut menu,
            &remotes,
            &automation_file_for_test(AutomationRunStatus::AwaitingApproval),
        );

        let session = menu.session("acp-automation-1").expect("run session");
        assert_eq!(session.kind, WorkspaceMenuSessionKind::Acp);
        assert_eq!(session.title, "每日晨报 · 晨报助手");
        // Run 的工作区是 daemon 分配的目录，不是用户项目：落进项目树会造出假分组。
        assert_eq!(session.project_root, None);
        assert_eq!(session.project_order, u32::MAX);
    }

    #[test]
    fn automation_summary_carries_the_frozen_run_input_and_trigger() {
        let hub = mobile_lifecycle_hub_for_test();
        let mut run_session = mobile_daemon_state(DaemonPhase::AwaitingApproval);
        run_session.id = "acp-automation-1".into();
        run_session.cwd = Some("/home/me/.smelt/workspaces/automations/automation-1".into());
        apply_mobile_lifecycle_event(
            &hub,
            DaemonStateEvent::Snapshot {
                sessions: vec![run_session],
                remote_sessions: Some(hidden_automation_remote_sessions()),
                workspace_menu: Some(WorkspaceMenuSnapshot::current(vec![], vec![])),
                automations: Some(automation_file_for_test(
                    AutomationRunStatus::AwaitingApproval,
                )),
            },
        );

        let summaries = hub.summaries().expect("summaries");
        let summary = summaries
            .iter()
            .find(|summary| summary.id == "acp-automation-1")
            .expect("automation run summary");
        let source = summary.automation.as_ref().expect("automation source");
        assert_eq!(source.automation_name, "每日晨报");
        assert_eq!(source.run_id, "run-1");
        assert_eq!(source.run_source, AutomationRunSource::Scheduled);
        assert_eq!(source.run_status, AutomationRunStatus::AwaitingApproval);
        assert_eq!(source.prompt.as_deref(), Some("拉取昨日 PR 列表"));
        assert_eq!(
            hub.automation_run_id("acp-automation-1").as_deref(),
            Some("run-1")
        );
    }

    /// Run 的生命周期归 daemon：手机不能把执行现场删掉。
    #[test]
    fn mobile_cannot_delete_an_automation_run_session() {
        let hub = mobile_lifecycle_hub_for_test();
        hub.apply_automations(automation_file_for_test(AutomationRunStatus::Running));
        assert!(hub.automation_run_id("acp-automation-1").is_some());
        assert!(hub.automation_run_id("acp-session-mobile").is_none());
    }

    #[test]
    fn mobile_workspace_menu_projects_unseen_remotes_onto_desktop_menu() {
        let desktop = WorkspaceMenuSnapshot::current(
            vec![WorkspaceMenuProject {
                root: "/repo".into(),
                title: "repo".into(),
                order: 0,
            }],
            vec![],
        );
        let remotes = RemoteSessionSnapshot {
            revision: 1,
            sessions: vec![RemoteSessionRecord {
                kind: RemoteSessionKind::Terminal,
                id: "remote-term".into(),
                cwd: "/repo".into(),
                title: "Mobile terminal".into(),
                agent_option_id: None,
                agent: None,
                launch: None,
                resume_id: None,
                created_at: 1,
                lifecycle: RemoteSessionLifecycle::Active,
                hidden: false,
            }],
        };
        let menu = mobile_workspace_menu(&desktop, &remotes);
        let session = menu.session("remote-term").unwrap();
        assert_eq!(session.kind, WorkspaceMenuSessionKind::Terminal);
        assert_eq!(session.title, "Mobile terminal");
        assert_eq!(session.project_root.as_deref(), Some("/repo"));
    }

    #[test]
    fn mobile_lifecycle_keeps_all_split_terminal_leaves_in_order() {
        let hub = mobile_lifecycle_hub_for_test();
        let mut left = mobile_daemon_state(DaemonPhase::Idle);
        left.id = "terminal-left".into();
        let mut right = mobile_daemon_state(DaemonPhase::Idle);
        right.id = "terminal-right".into();
        hub.apply_snapshot(
            vec![right, left],
            Some(RemoteSessionSnapshot::default()),
            None,
            None,
        );

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
    fn mobile_lifecycle_requires_subscribe_snapshots() {
        let hub = mobile_lifecycle_hub_for_test();
        hub.apply_snapshot(
            vec![mobile_daemon_state(DaemonPhase::Idle)],
            None,
            None,
            None,
        );
        assert!(hub.workspace_menu().is_err());

        hub.apply_remote_sessions(RemoteSessionSnapshot::default());
        assert!(hub.workspace_menu().is_err());

        hub.apply_workspace_menu(WorkspaceMenuSnapshot::default());
        assert!(hub.workspace_menu().is_ok());
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
        hub.apply_snapshot(
            vec![mobile_daemon_state(DaemonPhase::Thinking)],
            Some(RemoteSessionSnapshot::default()),
            None,
            None,
        );
        hub.apply_update(mobile_daemon_state(DaemonPhase::Succeeded));

        let before = hub
            .summaries_with_menu(&mobile_menu_for_test())
            .pop()
            .unwrap();
        assert_eq!(before.phase, "succeeded");
        assert_eq!(before.status, "idle");
        assert!(before.unread);
        assert_eq!(
            before.attention.unwrap().kind,
            smelt_core::attention::AttentionKind::Success
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
        hub.apply_snapshot(
            vec![mobile_daemon_state(DaemonPhase::Thinking)],
            Some(RemoteSessionSnapshot::default()),
            None,
            None,
        );
        hub.apply_update(mobile_daemon_state(DaemonPhase::AwaitingApproval));

        assert!(hub.mark_read("acp-session-mobile"));
        let summary = hub
            .summaries_with_menu(&mobile_menu_for_test())
            .pop()
            .unwrap();
        assert_eq!(summary.status, "needs_you");
        assert!(!summary.unread);
    }

    #[test]
    fn mobile_lifecycle_broadcasts_when_an_action_is_resolved_elsewhere() {
        let hub = mobile_lifecycle_hub_for_test();
        let mut updates = hub.updates.subscribe();
        hub.apply_snapshot(
            vec![mobile_daemon_state(DaemonPhase::Thinking)],
            Some(RemoteSessionSnapshot::default()),
            None,
            None,
        );
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
