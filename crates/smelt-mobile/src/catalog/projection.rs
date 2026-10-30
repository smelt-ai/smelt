//! 会话目录的纯投影：订阅到的运行时状态（会话状态、工作区目录、远程目录、自动化、
//! 智能体归属）在手机本地拼成会话列表，并维护 attention。不做 IO，单测直接喂事件。
//!
//! 列表每次现拼，不另存：名册来自运行时的工作区目录（项目和对话），加上自动化运行的
//! 会话（它们不进工作区目录）；运行态来自会话状态。会话归哪个项目、项目叫什么，
//! 和桌面侧栏用同一份算法（`smelt_model::workspace_directory`）现算。

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
use smelt_model::runtime_path::PathStyle;
use smelt_model::session_control::AcpAgentOption;
use smelt_model::workspace_directory::{
    WorkspaceConversation, WorkspaceDirectory, project_labels, project_root_of,
};

use crate::api::catalog as view;

/// 一个运行时的目录状态。换运行时（generation 变）就整个丢掉重建。
#[derive(Default)]
pub(crate) struct CatalogState {
    /// 收到过这一代的完整快照。之前列表不算实时数据。
    pub(crate) primed: bool,
    /// 运行时的活动记录。会话名册在工作区目录里，这里只有动过的会话。
    sessions: HashMap<String, DaemonSessionState>,
    /// 手机本地的提醒与已读。
    pub(crate) attention: AttentionStore,
    /// 运行时的工作区目录：开着的项目和对话。
    workspace: Option<WorkspaceDirectory>,
    /// 运行时那台机器的系统（`system.describe` 的 `os`），路径按它切。连上之前不知道，按 POSIX。
    os: Option<String>,
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
                workspace,
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
                self.workspace = workspace;
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
            DaemonStateEvent::Workspace(directory) => {
                self.workspace = Some(directory);
                applied.sessions_changed = true;
                applied.refresh_bindings = true;
            }
            DaemonStateEvent::Automations(automations) => {
                self.automations = Some(automations);
                applied.sessions_changed = true;
            }
            // 目录订阅不订这些 topic（见 `SubscriptionShape::CATALOG`）。
            DaemonStateEvent::ConversationWakeRequested { .. }
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

    /// 连上运行时之后按它的系统切路径。
    pub(crate) fn set_os(&mut self, os: Option<&str>) {
        self.os = os.map(str::to_string);
    }

    /// 会话没了：运行时报的，或刚在手机上删掉的。列表的名册来自工作区目录，等运行时
    /// 推来新目录再变，这里不改它。
    pub(crate) fn remove_session(&mut self, id: &str) {
        self.sessions.remove(id);
        self.attention.remove_session(id);
    }

    /// 现拼一份列表：工作区目录 + 自动化运行的会话。
    fn listing(&self) -> Listing {
        let empty_directory = WorkspaceDirectory::default();
        let directory = self.workspace.as_ref().unwrap_or(&empty_directory);
        let mut listing = Listing::new(PathStyle::of_os(self.os.as_deref()), &directory.projects);
        for conversation in &directory.conversations {
            listing.push_conversation(conversation);
        }
        if let Some(automations) = self.automations.as_ref() {
            listing.push_automation_runs(automations, &self.sessions);
        }
        listing
    }

    /// 能在里面新建会话的项目：工作区目录里开着的。只靠会话 cwd 撑起来的分组不算，
    /// 运行时新建会话只认开着的项目。
    pub(crate) fn projects(&self) -> Vec<view::WorkspaceProject> {
        let opened = self
            .workspace
            .as_ref()
            .map(|directory| directory.projects.len())
            .unwrap_or_default();
        self.listing()
            .projects
            .into_iter()
            .take(opened)
            .map(|project| view::WorkspaceProject {
                root: project.root,
                title: project.title,
                order: project.order,
            })
            .collect()
    }

    /// 会话列表：项目按目录里打开的先后（没开成项目的分组排在后面），组里按新建的先后，
    /// 同一段对话的几个终端按分屏顺序。
    pub(crate) fn summaries(&self) -> Vec<view::SessionSummary> {
        let listing = self.listing();
        let sources = self
            .automations
            .as_ref()
            .map(automation_sources)
            .unwrap_or_default();
        let mut rows: Vec<(&Row, view::SessionSummary)> = listing
            .rows
            .iter()
            .map(|row| {
                (
                    row,
                    mobile_summary(
                        row,
                        listing.project(row),
                        self.sessions.get(&row.id),
                        &self.attention,
                        sources.get(&row.id).cloned(),
                        self.agent_bindings.get(&row.id).cloned(),
                    ),
                )
            })
            .collect();
        rows.sort_by(|(a_row, a), (b_row, b)| {
            a.project_order
                .cmp(&b.project_order)
                .then(a_row.order.cmp(&b_row.order))
                .then(a_row.leaf.cmp(&b_row.leaf))
                .then(a.title.cmp(&b.title))
                .then(a.id.cmp(&b.id))
        });
        rows.into_iter().map(|(_, summary)| summary).collect()
    }
}

/// 列表里的一个项目分组。
struct ListedProject {
    root: String,
    title: String,
    order: u32,
}

/// 列表里的一行：一段 ACP 对话，或一个终端。
struct Row {
    id: String,
    kind: view::SessionKind,
    /// 用户起的名字。
    custom_title: Option<String>,
    /// 没有用户名字、运行时也还没报标题时显示的。
    fallback_title: String,
    cwd: Option<String>,
    /// 所在项目分组在 [`Listing::projects`] 里的下标；没有分组（智能体对话、自动化运行）
    /// 是 `None`。
    project: Option<usize>,
    agent: Option<String>,
    /// 在名册里的先后，和同一段对话里第几个终端。
    order: u32,
    leaf: u32,
}

/// 现拼的列表。项目分组先是目录里开着的项目，之后是会话 cwd 撑起来的分组。
struct Listing {
    style: PathStyle,
    opened: Vec<String>,
    projects: Vec<ListedProject>,
    rows: Vec<Row>,
}

impl Listing {
    fn new(style: PathStyle, opened: &[String]) -> Self {
        let mut listing = Self {
            style,
            opened: opened.to_vec(),
            projects: Vec::new(),
            rows: Vec::new(),
        };
        for root in opened {
            listing.group_for_root(root);
        }
        listing
    }

    fn project(&self, row: &Row) -> Option<&ListedProject> {
        row.project.map(|index| &self.projects[index])
    }

    fn group_for_root(&mut self, root: &str) -> usize {
        let root = root.trim_end_matches('/');
        if let Some(index) = self.projects.iter().position(|project| project.root == root) {
            return index;
        }
        self.projects.push(ListedProject {
            root: root.to_string(),
            title: String::new(),
            order: u32::try_from(self.projects.len()).unwrap_or(u32::MAX),
        });
        let roots: Vec<&str> = self.projects.iter().map(|p| p.root.as_str()).collect();
        let labels = project_labels(self.style, &roots);
        for (project, label) in self.projects.iter_mut().zip(labels) {
            project.title = label;
        }
        self.projects.len() - 1
    }

    /// 会话按 cwd 归到开着的项目；不在任何项目里的按自己的 cwd 自成一组，和桌面侧栏一样。
    /// 智能体对话不在用户项目里时不分组（手机把它们放进「智能体对话」）。
    fn group_for_cwd(&mut self, cwd: Option<&str>, agent_conversation: bool) -> Option<usize> {
        let cwd = cwd.map(str::trim).filter(|cwd| !cwd.is_empty())?;
        match project_root_of(self.style, &self.opened, cwd) {
            Some(root) => {
                let root = root.to_string();
                Some(self.group_for_root(&root))
            }
            None if agent_conversation => None,
            None => Some(self.group_for_root(cwd)),
        }
    }

    fn next_order(&self) -> u32 {
        u32::try_from(self.rows.len()).unwrap_or(u32::MAX)
    }

    fn push_conversation(&mut self, conversation: &WorkspaceConversation) {
        let order = self.next_order();
        if let Some(acp) = &conversation.acp {
            let agent_conversation =
                acp.agent_definition_id.is_some() || acp.automation_id.is_some();
            let project = self.group_for_cwd(acp.cwd.as_deref(), agent_conversation);
            self.rows.push(Row {
                id: acp.sid.clone(),
                kind: view::SessionKind::Acp,
                custom_title: conversation.custom_title.clone(),
                fallback_title: acp
                    .session_title
                    .clone()
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| format!("{} conversation", acp.agent)),
                cwd: acp.cwd.clone(),
                project,
                agent: Some(acp.agent.clone()),
                order,
                leaf: 0,
            });
            return;
        }
        let single = conversation.terminals.len() == 1;
        for (leaf, terminal) in conversation.terminals.iter().enumerate() {
            // 没起过的终端守护里没有，手机打不开。
            let Some(id) = terminal.id.clone() else {
                continue;
            };
            // 只有一个终端时，对话的名字就是这个终端的名字。
            let custom_title = terminal.custom_title.clone().or_else(|| {
                single
                    .then(|| conversation.custom_title.clone())
                    .flatten()
            });
            let project = self.group_for_cwd(terminal.cwd.as_deref(), false);
            self.rows.push(Row {
                id,
                kind: view::SessionKind::Terminal,
                custom_title,
                fallback_title: terminal
                    .launch_label
                    .clone()
                    .filter(|label| !label.trim().is_empty())
                    .unwrap_or_else(|| "Terminal".to_string()),
                cwd: terminal.cwd.clone(),
                project,
                agent: terminal
                    .launch_cmd
                    .as_deref()
                    .map(|launch| agent_from_launch(launch).to_string()),
                order,
                leaf: u32::try_from(leaf).unwrap_or(u32::MAX),
            });
        }
    }

    fn contains(&self, id: &str) -> bool {
        self.rows.iter().any(|row| row.id == id)
    }

    /// 自动化运行的对话不进工作区目录（桌面侧栏不该被后台运行刷屏），但手机是指挥台：
    /// 运行停下来等审批时只有手机在场。所以按运行档案把守护里还在的那些补进来，不带项目
    /// 归属（运行的工作区是运行时按自动化分配的目录，不是用户项目）。
    fn push_automation_runs(
        &mut self,
        automations: &AutomationFile,
        live: &HashMap<String, DaemonSessionState>,
    ) {
        for run in &automations.runs {
            let Some(session_id) = run.session_id.as_deref() else {
                continue;
            };
            if self.contains(session_id) || !live.contains_key(session_id) {
                continue;
            }
            let order = self.next_order();
            self.rows.push(Row {
                id: session_id.to_string(),
                kind: view::SessionKind::Acp,
                custom_title: None,
                fallback_title: run.context.automation_name.clone(),
                cwd: Some(run.context.cwd.clone()),
                project: None,
                agent: run.context.engine_kind_id.clone(),
                order,
                leaf: 0,
            });
        }

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

/// 一条会话在手机上的样子。存在性来自工作区目录（或远程目录、自动化运行），运行态来自
/// 运行时；没有运行态只表示还没动过（`updated_at` 为 0，阶段和状态都是空闲）。
fn mobile_summary(
    row: &Row,
    project: Option<&ListedProject>,
    daemon: Option<&DaemonSessionState>,
    attention_store: &AttentionStore,
    automation: Option<view::AutomationSource>,
    agent_definition_id: Option<String>,
) -> view::SessionSummary {
    // ACP 对话的 agent 在目录里定死；终端以 hook 认出来的为准，其次运行时报的启动命令，
    // 最后才看目录里记的启动命令（还没动过的终端）。
    let agent = match row.kind {
        view::SessionKind::Acp => row.agent.clone(),
        view::SessionKind::Terminal => None,
    }
    .or_else(|| daemon.and_then(|state| state.provider.clone()))
    .or_else(|| {
        daemon
            .and_then(|state| state.launch.as_deref())
            .map(|launch| agent_from_launch(launch).to_string())
    })
    .or_else(|| row.agent.clone())
    .unwrap_or_else(|| "other".to_string());
    let title = smelt_model::session_title::display_title(
        row.custom_title.as_deref(),
        daemon.and_then(|state| state.title.as_deref()),
        None,
        Some(row.fallback_title.as_str()),
    );
    view::SessionSummary {
        id: row.id.clone(),
        kind: row.kind,
        title,
        phase: daemon
            .map(session_phase)
            .unwrap_or(view::SessionPhase::Idle),
        status: daemon
            .map(session_status)
            .unwrap_or(view::SessionStatus::Idle),
        agent,
        cwd: row
            .cwd
            .clone()
            .or_else(|| daemon.and_then(|state| state.cwd.clone())),
        project_root: project.map(|project| project.root.clone()),
        project_title: project.map(|project| project.title.clone()),
        project_order: project.map_or(u32::MAX, |project| project.order),
        updated_at: daemon
            .map(|state| i64::try_from(state.updated_at).unwrap_or(i64::MAX))
            .unwrap_or(0),
        detail: daemon.and_then(DaemonSessionState::detail_line),
        attention: attention_store.unread(&row.id).map(attention_item),
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
