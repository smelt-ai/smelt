//! 工作区持久化。运行时那份工作区目录（项目、对话）归运行时：这边收 `workspace.changed`
//! 的推送，改动发成单条命令。项目列表直接取推送里的那份；对话要和本地活着的会话对账
//! （账本见 [`crate::workspace_directory`]）。客户端自己的界面状态（侧栏开合和顺序、
//! 当前页、各对话的分屏和路由）存在客户端库。写入走单消费者队列。

use std::collections::HashMap;
use std::collections::HashSet;

use gpui::{App, Context, Window};
use smelt_client_store::{ConversationRouteRecord, ProjectKey, SidebarOrderRecord};
use smelt_model::workspace_directory::{
    WorkspaceAcp, WorkspaceConversation, WorkspaceConversationIdParams, WorkspaceDirectory,
    WorkspaceProjectParams, WorkspaceTerminal,
};
use smelt_runtime_client::protocol::control::{ControlClientError, ControlErrorCode};

use crate::runtimes::Runtime;
use crate::workspace_directory::{DirectoryCommands, DirectoryOutcome, order_by_saved};
use crate::{
    AcpSaved, PaneState, RuntimeKey, SessionKind, SessionRouteArchive, SessionState,
    SidebarGrouping, Workspace, WorkspaceRoute,
};

/// 工作区快照要记住用户还能再打开的会话。
///
/// 远程投影的终端 PTY 随 daemon 进程死去，不能当开着的标签存。ACP 对话有
/// history / resume：smeltd 冷启动会按「没有 live runtime」清空远程目录，
/// 若 GUI 也不落盘，全局对话（智能体 space）就会从侧栏消失，只剩项目里那些
/// 本来就写在工作区里的会话。
pub(crate) fn persist_in_workspace_snapshot(is_acp: bool, remote_owned: bool) -> bool {
    is_acp || !remote_owned
}

/// 这段会话在不在运行时的工作区目录里。
pub(crate) fn in_workspace_directory(session: &crate::Session) -> bool {
    persist_in_workspace_snapshot(
        matches!(session.kind, SessionKind::Conversation(_)),
        session.remote_owned,
    )
}

/// 远程目录缺席时要不要拆掉 GUI 上的 ACP 投影。
///
/// 误投影的后台 session 仍然拆掉。普通对话不拆：daemon 冷启动会清空没有
/// live runtime 的远程目录，那不是用户删了它。真删除走 `SessionTerminated`。
pub(crate) fn unproject_acp_when_remote_catalog_drops(
    remote_owned: bool,
    auto_projected_background: bool,
) -> bool {
    remote_owned && auto_projected_background
}

/// 还没落地的恢复会话占着哪些 smeltd 会话 id。
///
/// 启动时远程目录先到、存档恢复后到（恢复要等 managed daemon 就绪）。只看已经建
/// 好的 `sessions` 会把“正在恢复的那一个”当成缺失，把同一场对话再投影一份；两个
/// 视图随后互抢同一个 ACP 会话的 client，表现就是侧栏里两条一模一样的会话、切过去
/// 反复断开重连。
pub(crate) fn pending_restore_session_ids(
    pending: &[(usize, SessionState)],
) -> std::collections::HashSet<String> {
    pending
        .iter()
        .flat_map(|(_, state)| {
            state
                .acp
                .as_ref()
                .and_then(|acp| acp.sid.clone())
                .into_iter()
                .chain(crate::pane_state_leaf_ids(&state.layout))
        })
        .filter(|id| !id.is_empty())
        .collect()
}

pub(crate) fn merge_restore_pending<T: Clone>(mut sessions: Vec<T>, pending: &[(usize, T)]) -> Vec<T> {
    let mut pending = pending.to_vec();
    pending.sort_by_key(|(index, _)| *index);
    for (index, session) in pending {
        sessions.insert(index.min(sessions.len()), session);
    }
    sessions
}

pub(crate) fn persisted_active_position(
    active_session: usize,
    pending: &[(usize, SessionState)],
    sessions_restored: bool,
) -> usize {
    if !sessions_restored {
        return active_session;
    }
    let mut position = active_session;
    let mut pending_indices = pending.iter().map(|(index, _)| *index).collect::<Vec<_>>();
    pending_indices.sort_unstable();
    for index in pending_indices {
        if index <= position {
            position += 1;
        }
    }
    position
}

/// 运行时目录里的一段对话 → 待恢复的会话。终端先按默认摆法，客户端存的分屏随后填回。
pub(crate) fn session_state_from_conversation(conversation: WorkspaceConversation) -> SessionState {
    let acp = conversation.acp.map(acp_saved_from_directory);
    let layout = match &acp {
        // ACP 对话的 layout 只是占位叶子。
        Some(acp) => PaneState::Leaf {
            cwd: acp.cwd.clone(),
            id: None,
            custom_title: None,
            launch_label: None,
            launch_cmd: None,
        },
        None => crate::conversation_layout::default_layout(
            conversation
                .terminals
                .into_iter()
                .map(|terminal| PaneState::Leaf {
                    cwd: terminal.cwd,
                    id: terminal.id,
                    custom_title: terminal.custom_title,
                    launch_label: terminal.launch_label,
                    launch_cmd: terminal.launch_cmd,
                })
                .collect(),
        ),
    };
    SessionState {
        id: conversation.id,
        layout,
        active: 0,
        last_updated_at: conversation.last_updated_at.max(0) as u64,
        custom_title: conversation.custom_title,
        acp,
        route: None,
    }
}

fn acp_saved_from_directory(acp: WorkspaceAcp) -> AcpSaved {
    AcpSaved {
        cwd: acp.cwd,
        launch: acp.launch,
        profile_id: acp.profile_id,
        agent: Some(acp.agent).filter(|agent| !agent.is_empty()),
        agent_definition_id: acp.agent_definition_id,
        history_session_id: acp
            .history_session_id
            .map(agent_client_protocol::schema::v1::SessionId::new),
        sid: Some(acp.sid).filter(|sid| !sid.is_empty()),
        refresh_launch_from_settings: acp.refresh_launch_from_settings,
        conversation_binding: acp.conversation_binding,
        config_values: acp.config_values,
        pending_prompt: acp.pending_prompt,
        pending_delivery_id: acp.pending_delivery_id,
        pending_agent_preset: acp.pending_agent_preset,
        automation_id: acp.automation_id,
        session_title: acp.session_title,
    }
}

/// 交给运行时的 ACP 对话。没有守护会话 id 的不交：运行时按它认这段对话。
fn acp_directory(acp: AcpSaved) -> Option<WorkspaceAcp> {
    Some(WorkspaceAcp {
        sid: acp.sid.filter(|sid| !sid.is_empty())?,
        cwd: acp.cwd,
        agent: acp.agent.unwrap_or_default(),
        profile_id: acp.profile_id,
        history_session_id: acp.history_session_id.map(|id| id.to_string()),
        launch: acp.launch,
        refresh_launch_from_settings: acp.refresh_launch_from_settings,
        conversation_binding: acp.conversation_binding,
        config_values: acp.config_values,
        pending_prompt: acp.pending_prompt,
        pending_delivery_id: acp.pending_delivery_id,
        pending_agent_preset: acp.pending_agent_preset,
        session_title: acp.session_title,
        agent_definition_id: acp.agent_definition_id,
        automation_id: acp.automation_id,
    })
}

fn directory_terminal(leaf: PaneState) -> Option<WorkspaceTerminal> {
    match leaf {
        PaneState::Leaf {
            cwd,
            id,
            custom_title,
            launch_label,
            launch_cmd,
        } => Some(WorkspaceTerminal {
            id,
            cwd,
            custom_title,
            launch_label,
            launch_cmd,
        }),
        _ => None,
    }
}

/// 客户端自己的界面状态。每台设备可以不同（设计文档 D1），不交给运行时。
#[derive(Clone, Debug, Default)]
pub(crate) struct WorkspaceUiState {
    pub(crate) active_session: usize,
    pub(crate) active_session_id: Option<String>,
    pub(crate) route: WorkspaceRoute,
    pub(crate) selected_agent_id: Option<String>,
    pub(crate) sidebar_w: Option<f32>,
    pub(crate) sidebar_open: Option<bool>,
    /// 各运行时上折叠、置顶的项目。
    pub(crate) collapsed_projects: Vec<ProjectKey>,
    pub(crate) collapsed_agents: Vec<String>,
    pub(crate) sidebar_grouping: SidebarGrouping,
    pub(crate) pinned_projects: Vec<ProjectKey>,
    pub(crate) sidebar_hide_empty_projects: bool,
}

/// 项目在客户端库里的身份。
pub(crate) fn project_key(runtime: &RuntimeKey, root: &str) -> ProjectKey {
    ProjectKey {
        runtime_id: match runtime {
            RuntimeKey::Local => None,
            RuntimeKey::Remote(id) => Some(id.clone()),
        },
        root: root.to_string(),
    }
}

/// 某个运行时上的那些项目根。
pub(crate) fn project_roots_of(keys: &[ProjectKey], runtime: &RuntimeKey) -> Vec<String> {
    keys.iter()
        .filter(|key| project_key(runtime, &key.root) == **key)
        .map(|key| key.root.clone())
        .collect()
}
/// 一段对话在客户端库里的身份：哪个运行时上的哪段对话（对话 id）。
pub(crate) type ConversationKey = (RuntimeKey, String);

/// 客户端存的一段对话的界面状态（设计文档 D1）：终端怎么分屏、当前看哪个，右侧 route。
#[derive(Clone, Default)]
pub(crate) struct SavedConversationUi {
    pub(crate) route: SessionRouteArchive,
    /// 分屏布局，格式见 [`crate::conversation_layout`]。ACP 对话没有。
    pub(crate) dock_layout: Option<String>,
    pub(crate) active_terminal_id: Option<String>,
}

impl SavedConversationUi {
    /// 一段还没恢复上来、或恢复失败的会话现在的样子。
    fn of_state(state: &SessionState) -> Self {
        let is_terminal = state.acp.is_none();
        Self {
            route: state.route.clone().unwrap_or_default(),
            dock_layout: is_terminal
                .then(|| crate::conversation_layout::encode(&state.layout))
                .flatten(),
            active_terminal_id: is_terminal
                .then(|| crate::conversation_layout::terminal_id_at(&state.layout, state.active))
                .flatten(),
        }
    }

    /// 把它填回刚从运行时读回来的会话：终端按存下的布局摆，以运行时的终端为准。
    fn apply_to(self, state: &mut SessionState) {
        state.route = Some(self.route);
        if state.acp.is_none() {
            let terminals = crate::conversation_layout::terminals_of(&state.layout);
            state.layout =
                crate::conversation_layout::restore(self.dock_layout.as_deref(), terminals);
            state.active = crate::conversation_layout::terminal_position(
                &state.layout,
                self.active_terminal_id.as_deref(),
            );
        }
    }
}

fn sorted_paths(paths: &std::collections::HashSet<String>) -> Vec<String> {
    let mut paths: Vec<String> = paths.iter().cloned().collect();
    paths.sort();
    paths
}

fn conversation_record(
    key: &ConversationKey,
    saved: &SavedConversationUi,
) -> ConversationRouteRecord {
    use crate::settings::enum_name;
    let route = &saved.route;
    ConversationRouteRecord {
        runtime_id: project_key(&key.0, "").runtime_id,
        conversation_id: key.1.clone(),
        active_terminal_id: saved.active_terminal_id.clone(),
        dock_layout: saved.dock_layout.clone(),
        stage_cover: route.stage_cover.as_ref().map(enum_name),
        tool_panel_tab: enum_name(&route.tool_panel_tab),
        tool_panel_open: route.tool_panel_open,
        tool_panel_w: route.tool_panel_w,
        expanded: sorted_paths(&route.expanded),
        file_tree_selected: route.file_tree_selected.clone(),
        open_file_path: route.open_file_path.clone(),
        file_tree_w: route.file_tree_w,
        file_tree_open: route.file_tree_open,
        pinned_roots: route.pinned_roots.clone(),
        collapsed_roots: sorted_paths(&route.collapsed_roots),
        git_tab: enum_name(&route.git_tab),
        diff_split: route.diff_split,
        git_tree_collapsed: sorted_paths(&route.git_tree_collapsed),
        diff_scope: enum_name(&route.diff_scope),
    }
}

/// 认不出的枚举名（别的版本写的）用出厂值。
fn conversation_from_record(
    record: ConversationRouteRecord,
) -> (ConversationKey, SavedConversationUi) {
    use crate::settings::enum_from_name;
    let defaults = SessionRouteArchive::default();
    let runtime = record
        .runtime_id
        .map(RuntimeKey::Remote)
        .unwrap_or(RuntimeKey::Local);
    let route = SessionRouteArchive {
        stage_cover: record.stage_cover.as_deref().and_then(enum_from_name),
        tool_panel_tab: enum_from_name(&record.tool_panel_tab).unwrap_or(defaults.tool_panel_tab),
        tool_panel_open: record.tool_panel_open,
        tool_panel_w: record.tool_panel_w,
        expanded: record.expanded.into_iter().collect(),
        file_tree_selected: record.file_tree_selected,
        open_file_path: record.open_file_path,
        file_tree_w: record.file_tree_w,
        file_tree_open: record.file_tree_open,
        pinned_roots: record.pinned_roots,
        collapsed_roots: record.collapsed_roots.into_iter().collect(),
        git_tab: enum_from_name(&record.git_tab).unwrap_or(defaults.git_tab),
        diff_split: record.diff_split,
        git_tree_collapsed: record.git_tree_collapsed.into_iter().collect(),
        diff_scope: enum_from_name(&record.diff_scope).unwrap_or(defaults.diff_scope),
        ..defaults
    };
    (
        (runtime, record.conversation_id),
        SavedConversationUi {
            route,
            dock_layout: record.dock_layout,
            active_terminal_id: record.active_terminal_id,
        },
    )
}

/// 启动时同步读各对话的界面状态（客户端库，本地，很快）。
pub(crate) fn load_conversation_ui() -> HashMap<ConversationKey, SavedConversationUi> {
    match smelt_client_store::client_store().and_then(|store| store.conversation_routes()) {
        Ok(records) => records.into_iter().map(conversation_from_record).collect(),
        Err(error) => {
            eprintln!("[workspace] 读取对话界面状态失败，使用默认值：{error}");
            HashMap::new()
        }
    }
}
/// 启动时同步读界面状态（客户端库，本地，很快）。没有时用默认值。
pub(crate) fn load_workspace_ui() -> WorkspaceUiState {
    let record = match smelt_client_store::client_store().and_then(|store| store.workspace_ui()) {
        Ok(record) => record,
        Err(error) => {
            eprintln!("[workspace] 读取界面状态失败，使用默认值：{error}");
            None
        }
    };
    let Some(record) = record else {
        return WorkspaceUiState::default();
    };
    WorkspaceUiState {
        active_session: record.active_session,
        active_session_id: record.active_session_id,
        route: crate::settings::enum_from_name(&record.route).unwrap_or_default(),
        selected_agent_id: record.selected_agent_id,
        sidebar_w: record.sidebar_w,
        sidebar_open: record.sidebar_open,
        collapsed_projects: record.collapsed_projects,
        collapsed_agents: record.collapsed_agents,
        sidebar_grouping: crate::settings::enum_from_name(&record.sidebar_grouping)
            .unwrap_or_default(),
        pinned_projects: record.pinned_projects,
        sidebar_hide_empty_projects: record.sidebar_hide_empty_projects,
    }
}

fn save_workspace_ui(ui: &WorkspaceUiState) -> Result<(), String> {
    let record = smelt_client_store::WorkspaceUiRecord {
        active_session: ui.active_session,
        active_session_id: ui.active_session_id.clone(),
        route: crate::settings::enum_name(&ui.route),
        selected_agent_id: ui.selected_agent_id.clone(),
        sidebar_w: ui.sidebar_w,
        sidebar_open: ui.sidebar_open,
        collapsed_projects: ui.collapsed_projects.clone(),
        collapsed_agents: ui.collapsed_agents.clone(),
        sidebar_grouping: crate::settings::enum_name(&ui.sidebar_grouping),
        pinned_projects: ui.pinned_projects.clone(),
        sidebar_hide_empty_projects: ui.sidebar_hide_empty_projects,
    };
    smelt_client_store::client_store()
        .and_then(|store| store.put_workspace_ui(&record))
        .map_err(|error| error.to_string())
}

/// 启动时同步读各运行时的侧栏顺序（客户端库，本地，很快）。
pub(crate) fn load_sidebar_orders() -> HashMap<RuntimeKey, SidebarOrderRecord> {
    match smelt_client_store::client_store().and_then(|store| store.sidebar_orders()) {
        Ok(records) => records
            .into_iter()
            .map(|record| {
                let key = record
                    .runtime_id
                    .clone()
                    .map(RuntimeKey::Remote)
                    .unwrap_or(RuntimeKey::Local);
                (key, record)
            })
            .collect(),
        Err(error) => {
            eprintln!("[workspace] 读取侧栏顺序失败，按目录顺序排：{error}");
            HashMap::new()
        }
    }
}


/// 一个运行时这一轮要写的东西。主线程构建（纯内存），后台线程提交。
struct SnapshotJob {
    runtime: Runtime,
    /// 客户端自己的那份（界面状态、各对话路由、侧栏顺序）只有一份，跟本机那份一起写。
    ui: Option<ClientSnapshot>,
    /// 目录还没收到时为 `None`：什么命令都不发。
    commands: Option<DirectoryCommands>,
    /// 用户打开、关闭的项目，按发生的先后。跟在对话命令后面发。
    project_ops: Vec<ProjectOp>,
    /// 同步发给 smeltd 的侧栏菜单。远程客户端只消费 daemon 订阅，不读工作区目录。
    menu: smelt_model::workspace_menu::WorkspaceMenuSnapshot,
}

/// 打开或关闭一个项目。项目列表不在本地另记一份，发出去等推送回来再变。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ProjectOp {
    Open(String),
    Close(String),
}

struct ClientSnapshot {
    ui: WorkspaceUiState,
    routes: Vec<ConversationRouteRecord>,
    orders: Vec<SidebarOrderRecord>,
}

/// workspace 状态的单消费者写入队列。它不直接做 I/O，只负责把所有生产者（普通保存、
/// 退出 flush、重连重发）归到一个线性序列，因此可以独立验证“最新值覆盖 pending、但
/// 已经开始写的任务必须先完成”的语义。
#[derive(Default)]
pub(crate) struct WorkspaceSnapshotWriteQueue {
    pending: Option<SnapshotJob>,
    in_flight: bool,
    flush_waiters: Vec<smol::channel::Sender<()>>,
}

impl WorkspaceSnapshotWriteQueue {
    /// 返回值表示调用方是否需要启动消费者循环。
    fn enqueue(&mut self, mut job: SnapshotJob) -> bool {
        // 快照是整份的，新的盖掉旧的；项目操作是一条条的，被盖掉的那份里的要接在前面。
        if let Some(replaced) = self.pending.take() {
            let mut ops = replaced.project_ops;
            ops.append(&mut job.project_ops);
            job.project_ops = ops;
        }
        self.pending = Some(job);
        if self.in_flight {
            false
        } else {
            self.in_flight = true;
            true
        }
    }

    /// 这一步必须在主线程的一次 update 内完成。若返回 None，队列已经确认为 idle，
    /// 退出等待者可被唤醒；新的 enqueue 随后会看到 in_flight=false 并启动新消费者。
    fn take_next_or_finish(&mut self) -> (Option<SnapshotJob>, Vec<smol::channel::Sender<()>>) {
        if let Some(job) = self.pending.take() {
            (Some(job), Vec::new())
        } else {
            self.in_flight = false;
            (None, std::mem::take(&mut self.flush_waiters))
        }
    }

    fn wait_for_drain(&mut self) -> smol::channel::Receiver<()> {
        let (sender, receiver) = smol::channel::bounded(1);
        if self.in_flight || self.pending.is_some() {
            self.flush_waiters.push(sender);
        } else {
            let _ = sender.try_send(());
        }
        receiver
    }
}

/// 后台线程提交：客户端那份写客户端库，目录命令按顺序发给运行时。返回发成功的对话
/// 命令（主线程据此记账，没发成功的下一轮再发），和没做成的项目操作的错误文案。
fn persist_ws_snapshot(job: &SnapshotJob) -> (DirectoryOutcome, Vec<String>) {
    if let Some(client) = &job.ui {
        if let Err(error) = save_workspace_ui(&client.ui) {
            eprintln!("[workspace] 保存界面状态失败：{error}");
        }
        match smelt_client_store::client_store() {
            Ok(store) => {
                if let Err(error) = store.put_conversation_routes(&client.routes) {
                    eprintln!("[workspace] 保存对话界面状态失败：{error}");
                }
                if let Err(error) = store.put_sidebar_orders(&client.orders) {
                    eprintln!("[workspace] 保存侧栏顺序失败：{error}");
                }
            }
            Err(error) => eprintln!("[workspace] 打开客户端库失败：{error}"),
        }
    }
    let outcome = job
        .commands
        .as_ref()
        .map(|commands| send_directory_commands(&job.runtime, commands))
        .unwrap_or_default();
    let project_errors = send_project_ops(&job.runtime, &job.project_ops);
    if let Err(e) =
        smelt_client::session_ops::publish_workspace_menu(job.runtime.client(), &job.menu)
    {
        eprintln!("[workspace] 发布侧栏菜单失败: {e}");
    }
    (outcome, project_errors)
}

/// 项目操作不重发：运行时连不上就告诉用户，项目列表保持运行时那边的样子。
fn send_project_ops(runtime: &Runtime, ops: &[ProjectOp]) -> Vec<String> {
    use smelt_model::control_api::{WorkspaceProjectClose, WorkspaceProjectOpen};
    let client = runtime.client();
    let timeout = crate::runtime_call::RUNTIME_CALL_TIMEOUT;
    ops.iter()
        .filter_map(|op| {
            let (what, result) = match op {
                ProjectOp::Open(root) => (
                    "打开项目",
                    client.call::<WorkspaceProjectOpen>(
                        &WorkspaceProjectParams { root: root.clone() },
                        timeout,
                    ),
                ),
                ProjectOp::Close(root) => (
                    "关闭项目",
                    client.call::<WorkspaceProjectClose>(
                        &WorkspaceProjectParams { root: root.clone() },
                        timeout,
                    ),
                ),
            };
            let root = match op {
                ProjectOp::Open(root) | ProjectOp::Close(root) => root,
            };
            result.err().map(|error| {
                format!(
                    "{what} {root} 失败：{}",
                    crate::runtime_call::error_text(error)
                )
            })
        })
        .collect()
}

/// 按依赖顺序发：先删、再建、再改。一条失败不影响后面的。
fn send_directory_commands(runtime: &Runtime, commands: &DirectoryCommands) -> DirectoryOutcome {
    use smelt_model::control_api::{
        WorkspaceConversationCreate, WorkspaceConversationDelete, WorkspaceConversationUpdate,
    };
    let client = runtime.client();
    let timeout = crate::runtime_call::RUNTIME_CALL_TIMEOUT;
    let failed = |what: &str, error: ControlClientError| {
        eprintln!(
            "[workspace] {what}失败：{}",
            crate::runtime_call::error_text(error)
        );
    };
    let mut outcome = DirectoryOutcome::default();
    for id in &commands.deletes {
        let params = WorkspaceConversationIdParams {
            conversation_id: id.clone(),
        };
        match client.call::<WorkspaceConversationDelete>(&params, timeout) {
            Ok(_) => outcome.deleted.push(id.clone()),
            Err(error) => failed("关闭对话", error),
        }
    }
    for conversation in &commands.creates {
        match client.call::<WorkspaceConversationCreate>(conversation, timeout) {
            Ok(_) => outcome.created.push(conversation.clone()),
            Err(error) => failed("新建对话", error),
        }
    }
    for conversation in &commands.updates {
        match client.call::<WorkspaceConversationUpdate>(conversation, timeout) {
            Ok(_) => outcome.settled.push(conversation.clone()),
            // 别处已经关掉了：推送随后会把它从本地摘掉，不必再发。
            Err(ControlClientError::Protocol(error))
                if error.code == ControlErrorCode::NotFound => {}
            // 参数不对，再发也一样：记成发过，等内容变了再发。
            Err(ControlClientError::Protocol(error))
                if error.code == ControlErrorCode::InvalidParams =>
            {
                eprintln!("[workspace] 运行时拒收对话 {}：{}", conversation.id, error.message);
                outcome.settled.push(conversation.clone());
            }
            Err(error) => failed("更新对话", error),
        }
    }
    outcome
}

fn snapshot_acp_view(
    view: &crate::acp_view::AcpView,
    agent_definition_id: Option<String>,
    automation_id: Option<String>,
) -> AcpSaved {
    AcpSaved {
        cwd: view.cwd(),
        launch: view.launch_spec(),
        profile_id: view.profile_id().map(str::to_string),
        agent: Some(view.agent_kind().id().to_string()),
        agent_definition_id,
        history_session_id: view.history_session_id_for_save(),
        sid: Some(view.session_id().to_string()),
        refresh_launch_from_settings: view.refresh_launch_from_settings(),
        conversation_binding: view.conversation_binding_for_save(),
        config_values: view.config_values_for_save(),
        pending_prompt: view.pending_prompt_for_save(),
        pending_delivery_id: view.pending_delivery_id_for_save(),
        pending_agent_preset: view.pending_agent_preset_for_save(),
        automation_id,
        session_title: view.session_title_for_save(),
    }
}

impl Workspace {
    /// 把所有会话（各自分屏树 + 活动叶子遍历序）+ 侧栏宽度 + 文件树列宽写入
    /// SQLite 工作区类型化快照。
    ///
    /// 主线程只做纯内存快照（构建 + 序列化），读盘/写盘一律挪到后台线程——
    /// 历史上 open() 会被系统 EndpointSecurity 钩子（杀毒/EDR/打印管理等）拖住，
    /// 同步写盘会直接冻结 UI（崩溃报告 0805）。后台写盘是串行的：高频调用只
    /// 保留最新快照，旧快照不会后到覆盖新数据。
    pub(crate) fn save_state(&self, cx: &mut Context<Self>) {
        if self.ws_snapshot_finalizing {
            return;
        }
        let jobs = self
            .space_keys()
            .into_iter()
            .filter_map(|key| Some((key.clone(), self.build_ws_snapshot(&key, cx)?)))
            .collect::<Vec<_>>();
        if jobs.is_empty() {
            return;
        }
        let epoch = self.ws_snapshot_epoch;
        // render 期间不能再次 update 当前 Workspace 实体。启动首帧会触发一次
        // save_state，直接 spawn 可能在布局仍借用实体时回到 update_entity，最终
        // 以 panic_in_cleanup/SIGABRT 退出。defer 等当前 effect cycle 完成后再入队。
        let workspace = cx.weak_entity();
        cx.defer(move |cx| {
            let Some(workspace) = workspace.upgrade() else {
                return;
            };
            workspace.update(cx, |this, cx| {
                for (key, job) in jobs {
                    this.enqueue_ws_snapshot(&key, job, epoch, cx);
                }
            });
        });
    }

    /// 收到一个运行时的工作区目录（订阅首帧或之后的推送）。第一份按冷启动恢复；之后的
    /// 和本地对账：别处新开的接上，别处关掉的摘掉（进程由关它的那边收），别处改的名字跟上。
    pub(crate) fn apply_workspace_directory(
        &mut self,
        key: &RuntimeKey,
        directory: WorkspaceDirectory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_local = *key == RuntimeKey::Local;
        let mut local_conversations = self
            .sessions
            .iter()
            .filter(|session| session.runtime(cx).key() == key)
            .map(|session| session.conversation_id.clone())
            .collect::<HashSet<_>>();
        let saved_order = self.saved_sidebar_orders.get(key).cloned().unwrap_or_default();
        let Some(space) = self.space_mut(key) else {
            return;
        };
        local_conversations.extend(space.restore_pending.iter().map(|(_, state)| state.id.clone()));
        // 项目列表就是这份目录里的，只按这台桌面的顺序排：第一份按库里记的，之后按侧栏
        // 现在的（拖过的顺序在这里），新开的排在最后。
        let order = if space.directory.loaded() {
            std::mem::take(&mut space.projects)
        } else {
            saved_order.projects.clone()
        };
        space.projects = order_by_saved(directory.projects, &order, String::as_str);
        let inbound = space
            .directory
            .receive(directory.revision, directory.conversations, &local_conversations);
        let runtime = space.runtime.clone();

        space
            .restore_pending
            .retain(|(_, state)| !inbound.removed.contains(&state.id));

        for id in &inbound.removed {
            if let Some(ix) = self.sessions.iter().position(|session| {
                session.conversation_id == *id && session.runtime(cx).key() == key
            }) {
                self.remove_session(ix, false, cx);
            }
        }
        for (id, title) in inbound.retitled {
            let Some(session) = self.sessions.iter_mut().find(|session| {
                session.conversation_id == id && session.runtime(cx).key() == key
            }) else {
                continue;
            };
            session.custom_title = title.clone();
            if let SessionKind::Conversation(view) = &session.kind {
                let view = view.clone();
                view.update(cx, |view, _| view.rename_session(title));
            }
            self.session_list_revision = self.session_list_revision.wrapping_add(1);
        }

        let mut added = inbound
            .added
            .into_iter()
            .map(session_state_from_conversation)
            .collect::<Vec<_>>();
        if inbound.first {
            added = order_by_saved(added, &saved_order.conversations, |state| state.id.as_str());
        }
        // 对话的界面状态（分屏、当前看哪个、右侧 route）存在客户端：按对话 id 填回来。
        for session in &mut added {
            if let Some(saved) = self
                .saved_conversation_ui
                .remove(&(key.clone(), session.id.clone()))
            {
                saved.apply_to(session);
            }
        }
        if inbound.first {
            self.start_directory_restore(runtime, added, window, cx);
        } else if !added.is_empty() {
            self.restore_incoming_conversations(runtime, added, window, cx);
        }
        if inbound.first && is_local {
            self.reconcile_remote_catalog_projection(window, cx);
        }
        self.save_state(cx);
        cx.notify();
    }

    /// 一个运行时的第一份目录：恢复上次的会话。活动会话只按本机那份认（客户端记的是上次的
    /// 活动会话）。
    fn start_directory_restore(
        &mut self,
        runtime: Runtime,
        pending: Vec<SessionState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = runtime.key().clone();
        let is_local = key == RuntimeKey::Local;
        let (active, saved_active_session_id) = if is_local {
            let saved_id = self.saved_active_session_id.clone().filter(|id| !id.is_empty());
            let active = saved_id
                .as_deref()
                .and_then(|id| pending.iter().position(|state| state.id == id))
                .unwrap_or(self.startup_active_session);
            (active, saved_id)
        } else {
            (0, None)
        };
        let Some(space) = self.space_mut(&key) else {
            return;
        };
        space.restore_pending = pending.iter().cloned().enumerate().collect();
        if pending.is_empty() {
            space.sessions_restored = true;
            if is_local {
                self.check_daemon_outdated(cx);
            }
            return;
        }
        eprintln!(
            "[workspace] 后台恢复 {:?} 上的 {} 个会话（不堵 UI）…",
            key,
            pending.len()
        );
        self.schedule_session_restore(
            runtime,
            pending,
            active,
            saved_active_session_id,
            window,
            cx,
        );
    }

    /// 一个运行时那份的写入（常规保存与退出 flush）都进入它自己的单消费者队列。
    /// 取下一项、标记 idle、唤醒退出等待者在同一个 `update` 中完成，避免旧实现里
    /// “看到空队列”与“把 inflight 置 false”之间的新快照永久搁置。
    fn enqueue_ws_snapshot(
        &mut self,
        key: &RuntimeKey,
        job: SnapshotJob,
        epoch: u64,
        cx: &mut Context<Self>,
    ) {
        if epoch != self.ws_snapshot_epoch {
            return;
        }
        let Some(space) = self.space_mut(key) else {
            return;
        };
        if !space.write_queue.enqueue(job) {
            return;
        }
        let key = key.clone();
        cx.spawn(async move |workspace, cx| {
            let executor = cx.background_executor().clone();
            loop {
                let next = workspace.update(cx, |this, _| {
                    this.space_mut(&key)
                        .map(|space| space.write_queue.take_next_or_finish())
                });
                let Ok(Some((job, waiters))) = next else {
                    break;
                };
                let Some(job) = job else {
                    for waiter in waiters {
                        let _ = waiter.try_send(());
                    }
                    break;
                };
                let (outcome, project_errors) = executor
                    .spawn(async move { persist_ws_snapshot(&job) })
                    .await;
                let _ = workspace.update(cx, |this, _| {
                    if let Some(space) = this.space_mut(&key) {
                        space.directory.record(outcome);
                    }
                    for error in project_errors {
                        eprintln!("[workspace] {error}");
                        this.report_background_error(error);
                    }
                });
            }
        })
        .detach();
    }

    /// 一段会话在运行时目录里该是的样子。不进目录的（远程投影的终端）返回 `None`。
    fn directory_conversation(
        &self,
        session: &crate::Session,
        cx: &App,
    ) -> Option<WorkspaceConversation> {
        if !in_workspace_directory(session) {
            return None;
        }
        let last_updated_at = session.effective_updated_at(cx).min(i64::MAX as u64) as i64;
        let (terminals, acp) = match &session.kind {
            SessionKind::Term { dock, active } => {
                // 树里一格都没有不会发生：关掉最后一格就关整个会话。真碰上了退回活动终端。
                let layout = dock
                    .pane_state(cx)
                    .unwrap_or_else(|| crate::term_dock::pane_leaf_state(active, cx));
                let terminals = crate::conversation_layout::terminals_of(&layout)
                    .into_iter()
                    .filter_map(directory_terminal)
                    .collect();
                (terminals, None)
            }
            SessionKind::Conversation(view) => {
                let saved = snapshot_acp_view(
                    view.read(cx),
                    session.agent_definition_id.clone(),
                    session.automation_id.clone(),
                );
                (Vec::new(), Some(acp_directory(saved)?))
            }
        };
        Some(WorkspaceConversation {
            id: session.conversation_id.clone(),
            custom_title: session.custom_title.clone(),
            last_updated_at,
            terminals,
            acp,
        })
    }

    /// 在运行时上打开一个项目。本地不先加：运行时改完推回目录，侧栏随推送出现。
    /// 这个运行时的目录还没到（没连上）时发不了，返回 `false`。
    pub(crate) fn request_project_open(
        &mut self,
        key: &RuntimeKey,
        root: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        self.request_project_op(key, ProjectOp::Open(root.to_string()), cx)
    }

    /// 在运行时上关闭一个项目，侧栏随推送摘掉。
    pub(crate) fn request_project_close(
        &mut self,
        key: &RuntimeKey,
        root: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        self.request_project_op(key, ProjectOp::Close(root.to_string()), cx)
    }

    /// 项目操作和这一轮快照一起进这个运行时的写入队列，排在已经入队的改动后面。
    fn request_project_op(&mut self, key: &RuntimeKey, op: ProjectOp, cx: &mut Context<Self>) -> bool {
        let Some(mut job) = self.build_ws_snapshot(key, cx) else {
            return false;
        };
        job.project_ops.push(op);
        let epoch = self.ws_snapshot_epoch;
        let key = key.clone();
        // 和 save_state 一样等当前 effect cycle 结束再入队。
        let workspace = cx.weak_entity();
        cx.defer(move |cx| {
            if let Some(workspace) = workspace.upgrade() {
                workspace.update(cx, |this, cx| this.enqueue_ws_snapshot(&key, job, epoch, cx));
            }
        });
        true
    }

    /// 本地关掉了一段对话：运行时目录里也关掉它（运行时顺带收进程），发成功之前一直留着。
    pub(crate) fn close_directory_conversation(&mut self, key: &RuntimeKey, conversation_id: &str) {
        if let Some(space) = self.space_mut(key) {
            space.directory.close_conversation(conversation_id);
        }
    }

    /// 主线程构建一个运行时那份待写快照（纯内存，不碰磁盘）。返回 None 表示本次无需写。
    fn build_ws_snapshot(&self, key: &RuntimeKey, cx: &mut Context<Self>) -> Option<SnapshotJob> {
        let space = self.space(key)?;
        // 本机目录还没到时连界面状态也不写：活动会话的下标要按恢复上来的会话算。
        if !space.directory.loaded() {
            return None;
        }
        let desired = self
            .sessions
            .iter()
            .filter(|session| session.runtime(cx).key() == key)
            .filter_map(|session| self.directory_conversation(session, cx))
            .collect::<Vec<_>>();
        let commands = space
            .directory
            .commands(&desired)
            .filter(|commands| !commands.is_empty());
        let ui = (*key == RuntimeKey::Local).then(|| ClientSnapshot {
            ui: self.workspace_ui_state(),
            routes: self.conversation_route_records(cx),
            orders: self.sidebar_order_records(cx),
        });
        Some(SnapshotJob {
            runtime: space.runtime.clone(),
            ui,
            commands,
            project_ops: Vec::new(),
            menu: self.workspace_menu_snapshot(key, cx),
        })
    }

    /// 客户端界面状态：折叠、置顶按运行时收齐。
    fn workspace_ui_state(&self) -> WorkspaceUiState {
        let mut ui = WorkspaceUiState {
            active_session: self
                .space(&RuntimeKey::Local)
                .map(|space| {
                    persisted_active_position(
                        self.active_session,
                        &space.restore_pending,
                        space.sessions_restored,
                    )
                })
                .unwrap_or(self.active_session),
            active_session_id: self
                .sessions
                .get(self.active_session)
                .map(|session| session.conversation_id.clone())
                .or_else(|| self.saved_active_session_id.clone())
                .filter(|id| !id.is_empty()),
            route: self.nav.persist(),
            selected_agent_id: self.agent_surface.selected_id.clone(),
            sidebar_w: Some(self.sidebar_w),
            sidebar_open: Some(self.sidebar_open),
            collapsed_projects: Vec::new(),
            collapsed_agents: self.collapsed_agents.iter().cloned().collect(),
            sidebar_grouping: self.sidebar_grouping,
            pinned_projects: Vec::new(),
            sidebar_hide_empty_projects: self.sidebar_hide_empty_projects,
        };
        for space_key in self.space_keys() {
            let Some(space) = self.space(&space_key) else {
                continue;
            };
            ui.collapsed_projects.extend(
                space
                    .collapsed_projects
                    .iter()
                    .map(|root| project_key(&space_key, root)),
            );
            ui.pinned_projects.extend(
                space
                    .pinned_projects
                    .iter()
                    .map(|root| project_key(&space_key, root)),
            );
        }
        ui
    }

    /// 各运行时的侧栏顺序。目录还没到的运行时不给：库里那份原样留着，不被空顺序冲掉。
    fn sidebar_order_records(&self, cx: &App) -> Vec<SidebarOrderRecord> {
        self.space_keys()
            .into_iter()
            .filter_map(|key| {
                let space = self.space(&key)?;
                if !space.directory.loaded() {
                    return None;
                }
                let live = self
                    .sessions
                    .iter()
                    .filter(|session| session.runtime(cx).key() == &key)
                    .map(|session| session.conversation_id.clone())
                    .collect::<Vec<_>>();
                let pending = space
                    .restore_pending
                    .iter()
                    .map(|(index, state)| (*index, state.id.clone()))
                    .collect::<Vec<_>>();
                let conversations = merge_restore_pending(live, &pending);
                Some(SidebarOrderRecord {
                    runtime_id: project_key(&key, "").runtime_id,
                    projects: space.projects.clone(),
                    conversations,
                })
            })
            .collect()
    }


    /// 各对话的界面状态：开着的、还没恢复上来的，以及目录还没读到的运行时上记着的那些
    /// （不能因为它离线就把界面状态冲掉）。
    fn conversation_route_records(&self, cx: &App) -> Vec<ConversationRouteRecord> {
        let mut records: HashMap<ConversationKey, ConversationRouteRecord> = HashMap::new();
        for (key, saved) in &self.saved_conversation_ui {
            records.insert(key.clone(), conversation_record(key, saved));
        }
        for space_key in self.space_keys() {
            let Some(space) = self.space(&space_key) else {
                continue;
            };
            for (_, session) in &space.restore_pending {
                let key = (space_key.clone(), session.id.clone());
                records.insert(
                    key.clone(),
                    conversation_record(&key, &SavedConversationUi::of_state(session)),
                );
            }
        }
        for session in &self.sessions {
            let route = if self.ui_session_id == Some(session.ui_id) {
                self.active_session_ui.archive()
            } else {
                session.ui_state.archive()
            };
            let (dock_layout, active_terminal_id) = match &session.kind {
                SessionKind::Term { dock, active } => (
                    dock.pane_state(cx)
                        .and_then(|layout| crate::conversation_layout::encode(&layout)),
                    Some(active.read(cx).session_id().to_string()).filter(|id| !id.is_empty()),
                ),
                SessionKind::Conversation(_) => (None, None),
            };
            let key = (
                session.runtime(cx).key().clone(),
                session.conversation_id.clone(),
            );
            records.insert(
                key.clone(),
                conversation_record(
                    &key,
                    &SavedConversationUi {
                        route,
                        dock_layout,
                        active_terminal_id,
                    },
                ),
            );
        }
        records.into_values().collect()
    }

    /// 退出阶段强制构建一份最新快照，并等待现有写盘循环自然清空。不能把 pending
    /// 快照拿出来另起线程写，否则旧循环可能在它之后完成并覆盖最终状态。
    ///
    /// 只等本机那份（界面状态也跟它走）：远程运行时可能已经离线，等它就要等到拨号
    /// 超时，退出会卡住。远程的照样入队，赶得上就写进去。
    pub(crate) fn flush_ws_state_on_quit(
        &mut self,
        cx: &mut Context<Self>,
    ) -> smol::channel::Receiver<()> {
        self.ws_snapshot_finalizing = true;
        self.ws_snapshot_epoch = self.ws_snapshot_epoch.wrapping_add(1);
        let epoch = self.ws_snapshot_epoch;
        for key in self.space_keys() {
            if let Some(job) = self.build_ws_snapshot(&key, cx) {
                self.enqueue_ws_snapshot(&key, job, epoch, cx);
            }
        }
        match self.space_mut(&crate::RuntimeKey::Local) {
            Some(space) => space.write_queue.wait_for_drain(),
            None => {
                let (tx, rx) = smol::channel::bounded(1);
                let _ = tx.send_blocking(());
                rx
            }
        }
    }
}

#[cfg(test)]
mod workspace_snapshot_write_queue_tests {
    use super::{ProjectOp, SnapshotJob, WorkspaceSnapshotWriteQueue};

    fn job(name: &str) -> SnapshotJob {
        SnapshotJob {
            runtime: crate::runtimes::Runtime::new(
                crate::runtimes::RuntimeKey::Local,
                smelt_runtime_client::RuntimeClient::new(|| {
                    Err(std::io::Error::other("snapshot test"))
                }),
            ),
            ui: None,
            commands: Some(crate::workspace_directory::DirectoryCommands {
                deletes: vec![name.to_string()],
                ..Default::default()
            }),
            project_ops: Vec::new(),
            menu: Default::default(),
        }
    }

    fn with_ops(mut job: SnapshotJob, ops: &[ProjectOp]) -> SnapshotJob {
        job.project_ops = ops.to_vec();
        job
    }

    /// 还没发的快照被新的盖掉时，里面的项目操作不能跟着丢：拖进来紧接着又保存一次，
    /// 打开项目照样要发出去，而且按先后发。
    #[test]
    fn project_ops_survive_a_replaced_pending_snapshot() {
        let mut queue = WorkspaceSnapshotWriteQueue::default();
        assert!(queue.enqueue(job("running")));
        let _ = queue.take_next_or_finish();

        queue.enqueue(with_ops(job("a"), &[ProjectOp::Close("/p".into())]));
        queue.enqueue(with_ops(job("b"), &[ProjectOp::Open("/p".into())]));
        queue.enqueue(job("c"));
        let (next, _) = queue.take_next_or_finish();
        let next = next.unwrap();
        assert_eq!(
            next.project_ops,
            [ProjectOp::Close("/p".into()), ProjectOp::Open("/p".into())]
        );
        assert_eq!(name(next), "c");
    }

    fn name(job: SnapshotJob) -> String {
        job.commands.unwrap().deletes.remove(0)
    }

    #[test]
    fn latest_pending_snapshot_wins_and_flush_waits_for_drain() {
        let mut queue = WorkspaceSnapshotWriteQueue::default();
        assert!(queue.enqueue(job("first")));
        let (first, waiters) = queue.take_next_or_finish();
        assert_eq!(name(first.unwrap()), "first");
        assert!(waiters.is_empty());

        assert!(!queue.enqueue(job("final")));
        let done = queue.wait_for_drain();
        let (final_job, waiters) = queue.take_next_or_finish();
        assert_eq!(name(final_job.unwrap()), "final");
        assert!(waiters.is_empty());

        let (last, waiters) = queue.take_next_or_finish();
        assert!(last.is_none());
        assert_eq!(waiters.len(), 1);
        for waiter in waiters {
            waiter.try_send(()).unwrap();
        }
        assert!(done.try_recv().is_ok());
    }
}

#[cfg(test)]
mod agent_conversation_persist_tests {
    use super::{
        RuntimeKey, SavedConversationUi, acp_directory, acp_saved_from_directory,
        conversation_from_record, conversation_record, persist_in_workspace_snapshot, project_key,
        project_roots_of, session_state_from_conversation, unproject_acp_when_remote_catalog_drops,
    };
    use smelt_model::workspace_directory::{WorkspaceAcp, WorkspaceConversation, WorkspaceTerminal};

    /// 复现：手机/远程投影的 ACP 若不当工作区会话落盘，daemon 冷启动清空远程目录后
    /// 全局对话从侧栏消失，项目里的对话却还在。
    #[test]
    fn remote_owned_acp_conversations_are_workspace_durable() {
        assert!(
            persist_in_workspace_snapshot(true, true),
            "ACP 对话必须进工作区目录，即使最初是远程投影"
        );
        assert!(
            persist_in_workspace_snapshot(true, false),
            "本机开的 ACP 对话本来就要落盘"
        );
        assert!(
            !persist_in_workspace_snapshot(false, true),
            "远程终端 PTY 随进程死去，不能当开着的标签存"
        );
        assert!(persist_in_workspace_snapshot(false, false));
    }

    #[test]
    fn daemon_restart_does_not_unproject_an_open_acp_conversation() {
        assert!(
            !unproject_acp_when_remote_catalog_drops(true, false),
            "远程目录被冷启动清空不是用户删除"
        );
        assert!(
            unproject_acp_when_remote_catalog_drops(true, true),
            "误投影的后台 session 仍然要拆掉"
        );
        assert!(!unproject_acp_when_remote_catalog_drops(false, false));
    }

    /// 智能体 space 上的 ACP 对话经运行时目录走一趟，恢复用到的字段都还在。
    #[test]
    fn agent_space_conversation_round_trips_through_the_directory() {
        let cwd = "/Users/me/.smelt/agents/space-agent";
        let acp = WorkspaceAcp {
            cwd: Some(cwd.into()),
            sid: "acp-global-1".into(),
            agent: "pi".into(),
            agent_definition_id: Some("space-agent".into()),
            history_session_id: Some("pi-history-1".into()),
            launch: smelt_model::agent_kind::ConversationLaunchSpec::from_command("smelt-pi-agent"),
            config_values: vec![("model".into(), "opus".into())],
            ..WorkspaceAcp::default()
        };
        let state = session_state_from_conversation(WorkspaceConversation {
            id: "conv-space".into(),
            acp: Some(acp.clone()),
            ..WorkspaceConversation::default()
        });
        assert_eq!(state.id, "conv-space");
        let saved = state.acp.expect("ACP 细节");
        assert_eq!(saved.cwd.as_deref(), Some(cwd));
        assert_eq!(saved.sid.as_deref(), Some("acp-global-1"));
        assert_eq!(
            saved.history_session_id.as_ref().map(|id| id.to_string()).as_deref(),
            Some("pi-history-1")
        );
        assert_eq!(acp_directory(saved), Some(acp.clone()));
        assert_eq!(
            acp_directory(acp_saved_from_directory(WorkspaceAcp {
                sid: String::new(),
                ..acp
            })),
            None,
            "没有守护会话 id 的对话不交给运行时"
        );
    }

    /// 终端对话恢复时按默认摆法铺开目录里的终端。
    #[test]
    fn terminal_conversation_restores_its_terminals() {
        let terminal = |id: &str| WorkspaceTerminal {
            id: Some(id.into()),
            cwd: Some("/srv/app".into()),
            ..WorkspaceTerminal::default()
        };
        let state = session_state_from_conversation(WorkspaceConversation {
            id: "conv-1".into(),
            last_updated_at: 42,
            terminals: vec![terminal("t-1"), terminal("t-2")],
            ..WorkspaceConversation::default()
        });
        assert!(state.acp.is_none());
        assert_eq!(state.last_updated_at, 42);
        assert_eq!(crate::pane_state_leaf_ids(&state.layout), ["t-1", "t-2"]);
    }

    /// 对话的界面状态存进客户端库再读回来一样不少。
    #[test]
    fn conversation_ui_lives_in_the_client_store() {
        let mut route = crate::SessionRouteArchive {
            stage_cover: Some(crate::StageCover::ToolPanel),
            tool_panel_tab: crate::tool_panel::ToolPanelTab::Git,
            tool_panel_open: true,
            tool_panel_w: 420.0,
            file_tree_selected: Some("/srv/app/main.rs".into()),
            file_tree_open: false,
            pinned_roots: vec!["/srv/app".into(), "/srv/lib".into()],
            git_tab: crate::GitTab::Log,
            diff_split: true,
            diff_scope: crate::git_panel::DiffScope::Staged,
            ..Default::default()
        };
        route.expanded.insert("/srv/app/src".into());
        route.git_tree_collapsed.insert("src".into());
        let key = (RuntimeKey::Remote("rt-linux".into()), "conv-1".to_string());
        let saved = SavedConversationUi {
            route: route.clone(),
            dock_layout: Some(r#"{"v":1,"center":{"terminal":"t-1"}}"#.into()),
            active_terminal_id: Some("t-1".into()),
        };

        let (back_key, back) = conversation_from_record(conversation_record(&key, &saved));
        assert_eq!(back_key, key);
        assert_eq!(
            serde_json::to_value(&back.route).unwrap(),
            serde_json::to_value(&route).unwrap()
        );
        assert_eq!(back.dock_layout, saved.dock_layout);
        assert_eq!(back.active_terminal_id.as_deref(), Some("t-1"));
    }

    /// 折叠、置顶的项目按运行时存：同一个路径在两台机器上是两个项目。
    #[test]
    fn project_keys_split_by_runtime() {
        let remote = RuntimeKey::Remote("rt-linux".to_string());
        let keys = vec![
            project_key(&RuntimeKey::Local, "/repo"),
            project_key(&remote, "/repo"),
            project_key(&remote, "/srv/app"),
        ];

        assert_eq!(
            project_roots_of(&keys, &RuntimeKey::Local),
            vec!["/repo".to_string()]
        );
        assert_eq!(
            project_roots_of(&keys, &remote),
            vec!["/repo".to_string(), "/srv/app".to_string()]
        );
        assert!(project_roots_of(&keys, &RuntimeKey::Remote("rt-pi".to_string())).is_empty());
    }
}
