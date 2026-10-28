//! 会话目录的纯投影：订阅到的运行时状态（会话状态、远程目录、桌面菜单、自动化、
//! 智能体归属）在手机本地拼成会话列表，并维护 attention。不做 IO，单测直接喂事件。
//!
//! 搬自已删除的手机网关（`MobileLifecycleState`、`mobile_workspace_menu`、
//! `mobile_summary`、`automation_catalog`），行为不变，只是视图模型换成 FRB 类型。

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use smelt_model::agent_definition::AgentDefinition;
use smelt_model::agent_kind::{ConversationAgentKind, TerminalAgentKind};
use smelt_model::agent_status::AgentStatus;
use smelt_model::attention::{
    self, AttentionItem as ModelAttention, AttentionKind as ModelAttentionKind, AttentionStore,
};
use smelt_model::automation::{
    self as model_automation, Automation, AutomationAction, AutomationFile, AutomationRun,
};
use smelt_model::daemon_state::{DaemonPhase, DaemonSessionState, DaemonStateEvent};
use smelt_model::new_session::{NewSessionAction, NewSessionSections, NewSessionTarget};
use smelt_model::session_control::{AcpAgentOption, RemoteSessionKind, RemoteSessionSnapshot};
use smelt_model::workspace_menu::{
    WorkspaceMenuProject, WorkspaceMenuSession, WorkspaceMenuSessionKind, WorkspaceMenuSnapshot,
};

use crate::api::catalog as view;

/// 一个运行时的目录状态。换运行时（generation 变）就整个丢掉重建。
#[derive(Default)]
pub(crate) struct CatalogState {
    /// 收到过这一代的完整快照。之前列表不算实时数据。
    pub(crate) primed: bool,
    /// 运行时的活动记录。会话名册在菜单里，这里只有动过的会话。
    sessions: HashMap<String, DaemonSessionState>,
    /// 手机本地的提醒与已读。
    pub(crate) attention: AttentionStore,
    /// 快照里为空表示运行时没有这份投影（例如没有桌面发布菜单的 Linux 运行时），按空处理。
    remote_sessions: Option<RemoteSessionSnapshot>,
    workspace_menu: Option<WorkspaceMenuSnapshot>,
    pub(crate) automations: Option<AutomationFile>,
    /// 会话 → 智能体定义 id（`session.agent_bindings`）。
    agent_bindings: BTreeMap<String, String>,
}

/// 应用一条事件的结果。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Applied {
    pub(crate) sessions_changed: bool,
    /// 要推给界面的新提醒（已按投递去重）。
    pub(crate) attention: Vec<ModelAttention>,
    /// 要处理的事已经过去的会话。
    pub(crate) resolved: Vec<String>,
    /// 会话集合可能变了，智能体归属要重取。
    pub(crate) refresh_bindings: bool,
}

impl CatalogState {
    pub(crate) fn apply(&mut self, event: DaemonStateEvent, now: Instant) -> Applied {
        let mut applied = Applied::default();
        match event {
            DaemonStateEvent::Snapshot {
                sessions,
                remote_sessions,
                workspace_menu,
                // 手机还在读菜单，之后改读工作区目录；`CATALOG` 也没订它，这里总是 None。
                workspace: _,
                automations,
            } => {
                // 这一代的第一份快照是基线：冷启动前就在审批 / 失败的会话不算新提醒，
                // 只静默记成已读的待处理事项（列表上仍靠 status 显示「要你」）。之后的
                // 快照（订阅断线重连、lag 重收）按边沿处理，期间发生的变化照常提醒。
                let baseline = !self.primed;
                let ids: HashSet<&str> = sessions.iter().map(|state| state.id.as_str()).collect();
                for state in &sessions {
                    let had_action = self.attention.has_unresolved_action(&state.id);
                    if baseline {
                        attention::apply_daemon_baseline(&mut self.attention, state);
                    } else {
                        attention::apply_daemon_transition(
                            &mut self.attention,
                            self.sessions.get(&state.id),
                            state,
                            now,
                        );
                    }
                    if had_action && !self.attention.has_unresolved_action(&state.id) {
                        applied.resolved.push(state.id.clone());
                    }
                }
                let stale: Vec<String> = self
                    .sessions
                    .keys()
                    .filter(|id| !ids.contains(id.as_str()))
                    .cloned()
                    .collect();
                for id in stale {
                    if self.attention.has_unresolved_action(&id) {
                        applied.resolved.push(id.clone());
                    }
                    self.attention.remove_session(&id);
                }
                self.sessions = sessions
                    .into_iter()
                    .map(|state| (state.id.clone(), state))
                    .collect();
                self.remote_sessions = remote_sessions;
                self.workspace_menu = workspace_menu;
                self.automations = automations;
                self.primed = true;
                applied.sessions_changed = true;
                applied.refresh_bindings = true;
            }
            DaemonStateEvent::Update(state) => {
                let had_action = self.attention.has_unresolved_action(&state.id);
                attention::apply_daemon_transition(
                    &mut self.attention,
                    self.sessions.get(&state.id),
                    &state,
                    now,
                );
                if had_action && !self.attention.has_unresolved_action(&state.id) {
                    applied.resolved.push(state.id.clone());
                }
                self.sessions.insert(state.id.clone(), state);
                applied.sessions_changed = true;
            }
            DaemonStateEvent::Removed { id } => {
                if self.attention.has_unresolved_action(&id) {
                    applied.resolved.push(id.clone());
                }
                self.remove_session(&id);
                applied.sessions_changed = true;
            }
            DaemonStateEvent::RemoteSessions(remote_sessions) => {
                self.remote_sessions = Some(remote_sessions);
                applied.sessions_changed = true;
                applied.refresh_bindings = true;
            }
            DaemonStateEvent::WorkspaceMenu(menu) => {
                self.workspace_menu = Some(menu);
                applied.sessions_changed = true;
                applied.refresh_bindings = true;
            }
            DaemonStateEvent::Automations(automations) => {
                self.automations = Some(automations);
                applied.sessions_changed = true;
            }
            // 目录订阅不订这些 topic（见 `SubscriptionShape::CATALOG`）。工作区目录之后
            // 手机改读它时再接。
            DaemonStateEvent::Workspace(_)
            | DaemonStateEvent::ConversationWakeRequested { .. }
            | DaemonStateEvent::ProjectChanged { .. }
            | DaemonStateEvent::AgentMemoryChanged { .. }
            | DaemonStateEvent::TerminalNotification { .. }
            | DaemonStateEvent::TerminalProgress { .. }
            | DaemonStateEvent::AppearanceChangeRequested { .. }
            | DaemonStateEvent::Disconnected => {}
        }
        // 投递队列自带 60 秒去重；不取走会一直涨。
        applied.attention = self.attention.drain_deliveries();
        applied
    }

    /// 只影响本机的已读。返回是否真有变化。
    pub(crate) fn mark_read(&mut self, session_id: &str) -> bool {
        self.attention.mark_read(session_id).is_some()
    }

    /// 内容没变返回 false。
    pub(crate) fn set_agent_bindings(&mut self, bindings: BTreeMap<String, String>) -> bool {
        if self.agent_bindings == bindings {
            return false;
        }
        self.agent_bindings = bindings;
        true
    }

    /// 会话没了：运行时报的，或刚在手机上删掉的。名册是菜单，所以菜单和远程目录里
    /// 也要划掉，否则要等下一份菜单快照才会从列表上消失。
    pub(crate) fn remove_session(&mut self, id: &str) {
        self.sessions.remove(id);
        self.attention.remove_session(id);
        if let Some(menu) = self.workspace_menu.as_mut() {
            menu.sessions.retain(|session| session.id != id);
        }
        if let Some(remote) = self.remote_sessions.as_mut() {
            remote.sessions.retain(|session| session.id != id);
        }
    }

    /// 桌面菜单 + 可见的远程会话 + 自动化运行的会话。
    fn projected_menu(&self) -> WorkspaceMenuSnapshot {
        let empty_remote = RemoteSessionSnapshot::default();
        let remote = self.remote_sessions.as_ref().unwrap_or(&empty_remote);
        let mut menu = match self.workspace_menu.as_ref() {
            Some(menu) => mobile_workspace_menu(menu, remote),
            None => mobile_workspace_menu(&WorkspaceMenuSnapshot::default(), remote),
        };
        if let Some(automations) = self.automations.as_ref() {
            append_automation_run_sessions(&mut menu, remote, automations);
        }
        menu
    }

    pub(crate) fn projects(&self) -> Vec<view::WorkspaceProject> {
        smelt_model::session_control::workspace_projects(&self.projected_menu())
            .into_iter()
            .map(|project| view::WorkspaceProject {
                root: project.root,
                title: project.title,
                order: project.order,
            })
            .collect()
    }

    /// 会话列表，按菜单顺序排好。遍历菜单而不是运行时的会话表：名册在菜单，运行时
    /// 只有活动记录。
    pub(crate) fn summaries(&self) -> Vec<view::SessionSummary> {
        let menu = self.projected_menu();
        let sources = self
            .automations
            .as_ref()
            .map(automation_sources)
            .unwrap_or_default();
        let mut rows: Vec<(&WorkspaceMenuSession, view::SessionSummary)> = menu
            .sessions
            .iter()
            .map(|session| {
                (
                    session,
                    mobile_summary(
                        session,
                        self.sessions.get(&session.id),
                        &self.attention,
                        sources.get(&session.id).cloned(),
                        self.agent_bindings.get(&session.id).cloned(),
                    ),
                )
            })
            .collect();
        rows.sort_by(|(a_menu, a), (b_menu, b)| {
            a_menu
                .project_order
                .cmp(&b_menu.project_order)
                .then(a_menu.session_order.cmp(&b_menu.session_order))
                .then(a_menu.leaf_order.cmp(&b_menu.leaf_order))
                .then(a.title.cmp(&b.title))
                .then(a.id.cmp(&b.id))
        });
        rows.into_iter().map(|(_, summary)| summary).collect()
    }
}

fn path_title(path: &str) -> String {
    std::path::Path::new(path.trim_end_matches('/'))
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// 找到（或按 cwd 现造一个）菜单里的项目，返回下标。手机远程建的会话可能没有对应的
/// 桌面项目。
fn ensure_menu_project(menu: &mut WorkspaceMenuSnapshot, cwd: &str) -> usize {
    menu.projects
        .iter()
        .position(|project| project.root == cwd)
        .unwrap_or_else(|| {
            let order = menu.projects.len();
            menu.projects.push(WorkspaceMenuProject {
                root: cwd.to_string(),
                title: path_title(cwd),
                order: u32::try_from(order).unwrap_or(u32::MAX),
            });
            order
        })
}

/// 把运行时的远程目录投影到桌面发布的菜单上。
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
        let project_index = ensure_menu_project(&mut menu, &remote.cwd);
        if menu.sessions.iter().any(|session| session.id == remote.id) {
            continue;
        }
        let project = &menu.projects[project_index];
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
            session_order: u32::try_from(menu.sessions.len()).unwrap_or(u32::MAX),
            leaf_order: 0,
            agent,
        });
    }
    menu
}

/// 自动化运行的对话在运行时目录里是 hidden（桌面侧栏不该被后台运行刷屏），但手机是
/// 指挥台：运行停下来等审批时只有手机在场。所以按运行档案把它们补进来，不带项目归属
/// （运行的工作区是运行时按自动化分配的目录，不是用户项目）。
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
            session_order: u32::try_from(menu.sessions.len()).unwrap_or(u32::MAX),
            leaf_order: 0,
            agent: remote
                .agent
                .clone()
                .or_else(|| run.context.engine_kind_id.clone()),
        });
    }
}

fn automation_source(run: &AutomationRun) -> view::AutomationSource {
    view::AutomationSource {
        automation_name: run.context.automation_name.clone(),
        run_status: run_status(run.status),
        run_source: run_source(run.source),
        prompt: run
            .context
            .prompt
            .as_ref()
            .map(|prompt| prompt.trim().to_string())
            .filter(|prompt| !prompt.is_empty()),
        started_at: run.started_at.or(Some(run.created_at)),
    }
}

/// 会话 id → 来源。同一个会话真出现多条运行时取最近创建的那条。
fn automation_sources(automations: &AutomationFile) -> HashMap<String, view::AutomationSource> {
    let mut sources: HashMap<String, (i64, view::AutomationSource)> = HashMap::new();
    for run in &automations.runs {
        let Some(session_id) = run.session_id.clone() else {
            continue;
        };
        match sources.entry(session_id) {
            Entry::Occupied(mut slot) => {
                if run.created_at >= slot.get().0 {
                    slot.insert((run.created_at, automation_source(run)));
                }
            }
            Entry::Vacant(slot) => {
                slot.insert((run.created_at, automation_source(run)));
            }
        }
    }
    sources
        .into_iter()
        .map(|(id, (_, source))| (id, source))
        .collect()
}

pub(crate) fn session_phase(state: &DaemonSessionState) -> view::SessionPhase {
    match state.effective_phase() {
        DaemonPhase::Connecting => view::SessionPhase::Connecting,
        DaemonPhase::Thinking => view::SessionPhase::Thinking,
        DaemonPhase::ExecutingTool => view::SessionPhase::ExecutingTool,
        DaemonPhase::AwaitingApproval => view::SessionPhase::AwaitingApproval,
        DaemonPhase::WaitingForUser => view::SessionPhase::WaitingForUser,
        DaemonPhase::Succeeded => view::SessionPhase::Succeeded,
        DaemonPhase::Failed => view::SessionPhase::Failed,
        DaemonPhase::Idle => view::SessionPhase::Idle,
        DaemonPhase::Dead => view::SessionPhase::Dead,
    }
}

pub(crate) fn session_status(state: &DaemonSessionState) -> view::SessionStatus {
    match AgentStatus::from_daemon_state(state) {
        Some(AgentStatus::NeedsYou) => view::SessionStatus::NeedsYou,
        Some(AgentStatus::Running) => view::SessionStatus::Running,
        Some(AgentStatus::Idle) | None => view::SessionStatus::Idle,
    }
}

/// 从启动命令认 agent：先查对话表，再查终端表（Antigravity 等只有终端）。
fn agent_from_launch(launch: &str) -> &'static str {
    ConversationAgentKind::from_command_loose(launch)
        .map(ConversationAgentKind::id)
        .or_else(|| TerminalAgentKind::from_command_prefix(launch).map(TerminalAgentKind::id))
        .unwrap_or("other")
}

pub(crate) fn attention_item(item: &ModelAttention) -> view::AttentionItem {
    view::AttentionItem {
        session_id: item.session_id.clone(),
        title: item.title.clone(),
        message: item.message.clone(),
        kind: match item.kind {
            ModelAttentionKind::Approval => view::AttentionKind::Approval,
            ModelAttentionKind::Input => view::AttentionKind::Input,
            ModelAttentionKind::Success => view::AttentionKind::Success,
            ModelAttentionKind::Failure => view::AttentionKind::Failure,
            ModelAttentionKind::Bell => view::AttentionKind::Bell,
            ModelAttentionKind::Notice => view::AttentionKind::Notice,
        },
    }
}

/// 一条会话在手机上的样子。存在性来自菜单，运行态来自运行时；没有运行态只表示
/// 还没动过（`updated_at` 为 0，阶段和状态都是空闲）。
fn mobile_summary(
    menu_session: &WorkspaceMenuSession,
    daemon: Option<&DaemonSessionState>,
    attention_store: &AttentionStore,
    automation: Option<view::AutomationSource>,
    agent_definition_id: Option<String>,
) -> view::SessionSummary {
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
    let title = smelt_model::session_title::display_title(
        menu_session
            .custom_title
            .then_some(menu_session.title.as_str()),
        daemon.and_then(|state| state.title.as_deref()),
        None,
        Some(menu_session.title.as_str()),
    );
    view::SessionSummary {
        id: menu_session.id.clone(),
        kind: match menu_session.kind {
            WorkspaceMenuSessionKind::Acp => view::SessionKind::Acp,
            WorkspaceMenuSessionKind::Terminal => view::SessionKind::Terminal,
        },
        title,
        phase: daemon
            .map(session_phase)
            .unwrap_or(view::SessionPhase::Idle),
        status: daemon
            .map(session_status)
            .unwrap_or(view::SessionStatus::Idle),
        agent,
        cwd: menu_session
            .cwd
            .clone()
            .or_else(|| daemon.and_then(|state| state.cwd.clone())),
        project_root: menu_session.project_root.clone(),
        project_title: menu_session.project_title.clone(),
        project_order: menu_session.project_order,
        updated_at: daemon
            .map(|state| i64::try_from(state.updated_at).unwrap_or(i64::MAX))
            .unwrap_or(0),
        detail: daemon.and_then(DaemonSessionState::detail_line),
        attention: attention_store.unread(&menu_session.id).map(attention_item),
        automation,
        agent_definition_id,
    }
}

// ---------------------------------------------------------------------------
// 工作区
// ---------------------------------------------------------------------------

pub(crate) fn agent_option(option: &AcpAgentOption) -> view::AcpAgentOption {
    view::AcpAgentOption {
        id: option.id.clone(),
        kind: option.kind.clone(),
        label: option.label.clone(),
        agent_definition_id: option.agent_definition_id().map(str::to_string),
    }
}

fn launch_action(action: NewSessionAction) -> view::LaunchAction {
    let (target, agent) = match action.target {
        NewSessionTarget::Conversation { agent_kind, .. } => {
            (view::LaunchTarget::Conversation, agent_kind)
        }
        NewSessionTarget::Terminal { provider, .. } => {
            (view::LaunchTarget::Terminal, provider.unwrap_or_default())
        }
        NewSessionTarget::BlankTerminal => (view::LaunchTarget::BlankTerminal, String::new()),
    };
    view::LaunchAction {
        key: action.key,
        label: action.label,
        target,
        agent,
    }
}

pub(crate) fn launch_sections(sections: NewSessionSections) -> view::LaunchSections {
    let [common, terminal, conversation] = sections.into_array();
    let map = |actions: Vec<NewSessionAction>| actions.into_iter().map(launch_action).collect();
    view::LaunchSections {
        common: map(common),
        terminal: map(terminal),
        conversation: map(conversation),
    }
}

// ---------------------------------------------------------------------------
// 自动化
// ---------------------------------------------------------------------------

fn run_status(status: model_automation::AutomationRunStatus) -> view::AutomationRunStatus {
    use model_automation::AutomationRunStatus as S;
    match status {
        S::Starting => view::AutomationRunStatus::Starting,
        S::Queued => view::AutomationRunStatus::Queued,
        S::Dispatching => view::AutomationRunStatus::Dispatching,
        S::Running => view::AutomationRunStatus::Running,
        S::AwaitingApproval => view::AutomationRunStatus::AwaitingApproval,
        S::WaitingForUser => view::AutomationRunStatus::WaitingForUser,
        S::Completed => view::AutomationRunStatus::Completed,
        S::Failed => view::AutomationRunStatus::Failed,
        S::Cancelled => view::AutomationRunStatus::Cancelled,
        S::Skipped => view::AutomationRunStatus::Skipped,
    }
}

fn run_source(source: model_automation::AutomationRunSource) -> view::AutomationRunSource {
    use model_automation::AutomationRunSource as S;
    match source {
        S::Manual => view::AutomationRunSource::Manual,
        S::Scheduled => view::AutomationRunSource::Scheduled,
        S::Webhook => view::AutomationRunSource::Webhook,
        S::Event => view::AutomationRunSource::Event,
    }
}

fn schedule(schedule: &model_automation::AutomationSchedule) -> view::AutomationSchedule {
    use model_automation::AutomationSchedule as S;
    let blank = view::AutomationSchedule {
        kind: view::ScheduleKind::Daily,
        hour: 0,
        minute: 0,
        minutes: 0,
        hours: 0,
        days: 0,
        end_hour: 0,
        end_minute: 0,
    };
    match *schedule {
        S::Daily { hour, minute } => view::AutomationSchedule {
            hour,
            minute,
            ..blank
        },
        S::EveryMinutes { minutes } => view::AutomationSchedule {
            kind: view::ScheduleKind::EveryMinutes,
            minutes,
            ..blank
        },
        S::EveryHours { hours } => view::AutomationSchedule {
            kind: view::ScheduleKind::EveryHours,
            hours,
            ..blank
        },
        S::Weekly { days, hour, minute } => view::AutomationSchedule {
            kind: view::ScheduleKind::Weekly,
            hour,
            minute,
            days,
            ..blank
        },
        S::During {
            minutes,
            days,
            start_hour,
            start_minute,
            end_hour,
            end_minute,
        } => view::AutomationSchedule {
            kind: view::ScheduleKind::During,
            hour: start_hour,
            minute: start_minute,
            minutes,
            days,
            end_hour,
            end_minute,
            ..blank
        },
    }
}

fn run_summary(run: &AutomationRun) -> view::AutomationRunSummary {
    view::AutomationRunSummary {
        status: run_status(run.status),
        source: run_source(run.source),
        started_at: run.started_at.or(Some(run.created_at)),
        finished_at: run.finished_at,
        session_id: run.session_id.clone(),
        error: run.error.clone(),
    }
}

pub(crate) fn agent_definition(definition: &AgentDefinition) -> view::AgentDefinitionSummary {
    view::AgentDefinitionSummary {
        id: definition.id.clone(),
        name: definition.name.clone(),
        description: definition.description.clone(),
        engine_kind_id: definition.engine_kind_id.clone(),
        prompt: definition.prompt.clone(),
        plugins: definition.plugins.clone(),
        context_folders: definition.context_folders.clone(),
        context_links: definition.context_links.clone(),
    }
}

/// 自动化目录。定义来自 `automation.list`（顺序与桌面一致），状态和运行记录来自订阅到的
/// 自动化投影（还没收到时没有「下次 / 上次」）。`fresh_run`：刚由 `automation.run_once`
/// 拿到、投影里可能还没有的那次运行，算作它那条自动化的上次运行。
///
/// webhook 的地址和密钥是凭据，只投成「有外部触发」。
pub(crate) fn automation_catalog(
    automations: &[Automation],
    file: Option<&AutomationFile>,
    definitions: &[AgentDefinition],
    fresh_run: Option<&AutomationRun>,
) -> view::AutomationCatalog {
    let summaries = automations
        .iter()
        .map(|automation| {
            let trigger = &automation.trigger;
            let (action, agent_name) = match &automation.action {
                AutomationAction::Agent {
                    agent_definition_id,
                    ..
                } => (
                    view::AutomationActionKind::Agent,
                    definitions
                        .iter()
                        .find(|definition| &definition.id == agent_definition_id)
                        .map(|definition| definition.name.clone()),
                ),
                AutomationAction::Shell { .. } => (view::AutomationActionKind::Shell, None),
            };
            let state = file.and_then(|file| file.state_for(&automation.id));
            // 先按登记的 last_run_id 取；没登记就退回最近创建的那次，否则刚触发还没落
            // 状态的运行会在目录里凭空消失。
            let last_run = fresh_run
                .filter(|run| run.automation_id == automation.id)
                .or_else(|| {
                    let file = file?;
                    state
                        .and_then(|state| state.last_run_id.as_ref())
                        .and_then(|run_id| file.runs.iter().find(|run| &run.id == run_id))
                        .or_else(|| file.latest_run_for(&automation.id))
                })
                .map(run_summary);
            view::AutomationSummary {
                id: automation.id.clone(),
                name: automation.name.clone(),
                enabled: automation.enabled,
                schedules: trigger.schedules.iter().map(schedule).collect(),
                event_topics: trigger
                    .events
                    .iter()
                    .map(|event| event.topic.clone())
                    .collect(),
                webhook: !trigger.webhooks.is_empty(),
                action,
                agent_name,
                next_run_at: state.and_then(|state| state.next_run_at),
                last_run,
            }
        })
        .collect();
    view::AutomationCatalog {
        automations: summaries,
        agents: definitions.iter().map(agent_definition).collect(),
    }
}
