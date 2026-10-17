//! 工作区持久化：运行时那份工作区目录（项目、会话组）经 `workspace.*` 读写；客户端
//! 自己的界面状态（侧栏、当前页、Dock 布局）单独存在客户端。写入走单消费者队列。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use gpui::{App, Context, EntityId, Window};
use smelt_runtime_client::protocol::control::EmptyParams;
use smelt_runtime_client::protocol::control::{ControlClientError, ControlErrorCode};
use smelt_runtime_client::protocol::workspace::{
    WorkspaceLoadCall, WorkspaceLoadResult, WorkspaceSaveCall, WorkspaceSaveParams,
};

use crate::{
    AcpSaved, LegacyAgentRunState, MIN_TOOL_PANEL_WIDTH, Pane, PaneState, SessionKind,
    SessionRouteArchive, SessionState, SidebarGrouping, Workspace, WorkspaceRoute, tool_panel,
    unix_now_secs,
};

/// 工作台的持久化状态：主区分屏布局树 + 活动叶子 + 侧栏宽度。
/// 落在 `~/.smelt/smelt.sqlite3` 的工作区类型化快照里，启动时据此重建分屏。
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub(crate) struct WsState {
    /// 所有会话（每个 = 一棵分屏树 + 会话内活动叶子遍历序）。
    #[serde(default)]
    pub(crate) sessions: Vec<SessionState>,
    /// 已打开的项目根目录（有序）。独立于会话存在，见 Workspace::projects。
    /// 旧存档没有这个字段 → 启动时从各会话 cwd 反推一份（见 Workspace::new）。
    #[serde(default)]
    pub(crate) projects: Vec<String>,
    /// PC 侧栏的跨端纯数据投影。移动端直接消费并过滤 ACP，不再重新推导菜单。
    #[serde(default)]
    pub(crate) menu: smelt_model::workspace_menu::WorkspaceMenuSnapshot,
    /// 当前活动会话的存档下标。只作旧档回退；认人用 `active_session_id`。
    #[serde(default)]
    pub(crate) active_session: usize,
    /// 当前活动会话的稳定 id（ACP `sid` 或终端叶子 smeltd id）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) active_session_id: Option<String>,
    /// 当前一级路由；旧存档默认回到 session。
    #[serde(default)]
    pub(crate) route: WorkspaceRoute,
    /// 智能体工作台最后选中的定义；运行进程不写进普通会话存档。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selected_agent_id: Option<String>,
    /// 0.8.0 开发版单例 AgentRuntime 的兼容入口。加载时迁移进普通对话，之后
    /// 不再写回；智能体定义自身仍不属于 Session。
    #[serde(default, rename = "agent_runs", skip_serializing)]
    pub(crate) legacy_agent_runs: Vec<LegacyAgentRunState>,
    #[serde(default, rename = "agent_runtime_visible", skip_serializing)]
    pub(crate) _legacy_agent_runtime_visible: bool,
    /// 会话侧栏拖出的宽度（px）；None = 用默认值。
    #[serde(default)]
    pub(crate) sidebar_w: Option<f32>,
    /// 会话侧栏上次是否展开；None（旧存档）= 默认展开。
    #[serde(default)]
    pub(crate) sidebar_open: Option<bool>,
    /// 历史兼容字段：只读一次，用于把旧会话迁移为会话级 Tool Panel 状态。
    #[serde(
        default,
        skip_serializing,
        alias = "inspector_w",
        alias = "tool_panel_w"
    )]
    pub(crate) legacy_tool_panel_w: Option<f32>,
    #[serde(
        default,
        skip_serializing,
        alias = "inspector_open",
        alias = "tool_panel_open"
    )]
    pub(crate) legacy_tool_panel_open: Option<bool>,
    #[serde(default, skip_serializing, alias = "file_tree_w")]
    pub(crate) legacy_file_tree_w: Option<f32>,
    #[serde(default, skip_serializing, alias = "pinned_file_tree_roots")]
    pub(crate) legacy_pinned_file_tree_roots: Vec<String>,
    #[serde(default, skip_serializing, alias = "collapsed_file_tree_roots")]
    pub(crate) legacy_collapsed_file_tree_roots: Vec<String>,
    /// 会话侧栏里被折叠起来的项目根。
    #[serde(default)]
    pub(crate) collapsed_projects: Vec<String>,
    /// 智能体侧栏里被折叠起来的智能体定义 id。
    #[serde(default)]
    pub(crate) collapsed_agents: Vec<String>,
    /// 会话侧栏的分组方式；旧存档默认按项目。
    #[serde(default)]
    pub(crate) sidebar_grouping: SidebarGrouping,
    /// 侧栏里被固定的项目根。固定的空项目在「隐藏无会话」时仍显示。
    #[serde(default)]
    pub(crate) pinned_projects: Vec<String>,
    /// 侧栏是否隐藏无会话项目；旧存档默认显示全部。
    #[serde(default)]
    pub(crate) sidebar_hide_empty_projects: bool,
    // --- 以下为旧存档兼容字段（读到就迁移，不再写出）---
    /// 旧格式：单棵分屏树。
    #[serde(default)]
    pub(crate) layout: Option<PaneState>,
    /// 更旧格式：终端 cwd 列表（每个迁移成一个独立会话）。
    #[serde(default)]
    pub(crate) tabs: Vec<Option<String>>,
    /// 旧格式的活动索引。
    #[serde(default)]
    pub(crate) active: usize,
}

/// 工作区快照要记住用户还能再打开的会话。
///
/// 远程投影的终端 PTY 随 daemon 进程死去，不能当开着的标签存。ACP 对话有
/// history / resume：smeltd 冷启动会按「没有 live runtime」清空远程目录，
/// 若 GUI 也不落盘，全局对话（智能体 space）就会从侧栏消失，只剩项目里那些
/// 本来就写在工作区里的会话。
pub(crate) fn persist_in_workspace_snapshot(is_acp: bool, remote_owned: bool) -> bool {
    is_acp || !remote_owned
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

pub(crate) fn merge_restore_pending(
    mut sessions: Vec<SessionState>,
    pending: &[(usize, SessionState)],
) -> Vec<SessionState> {
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

/// 把渲染用的布局树导出成可序列化镜像（叶子读取各终端当前 cwd）。
fn pane_to_state(pane: &Pane, cx: &App) -> PaneState {
    match pane {
        Pane::Leaf(t) => {
            let t = t.read(cx);
            PaneState::Leaf {
                cwd: t.cwd(),
                id: Some(t.session_id().to_string()),
                custom_title: t.custom_title().map(str::to_string),
                launch_label: t.launch_label().map(str::to_string),
                launch_cmd: t.launch_cmd().map(str::to_string),
            }
        }
        Pane::Split {
            axis,
            state,
            children,
            ..
        } => PaneState::Split {
            axis: (*axis).into(),
            children: children.iter().map(|c| pane_to_state(c, cx)).collect(),
            // 直接读 ResizableState：用户拖出来的当前尺寸就在里面，不用自己跟着同步一份。
            sizes: state
                .read(cx)
                .sizes()
                .iter()
                .map(|p| f32::from(*p))
                .collect(),
        },
    }
}

/// 把存档里的会话列表规范成 `Vec<SessionState>`（兼容旧 layout / tabs 字段）。
pub(crate) fn normalize_saved_sessions(s: &WsState) -> (Vec<SessionState>, usize) {
    let legacy_route = legacy_route_from_workspace(s);
    let (mut sessions, mut active_session) = if !s.sessions.is_empty() {
        let mut sessions = s.sessions.clone();
        for session in &mut sessions {
            // 旧版 route 尚未按会话持久化时，顶层字段只作为迁移来源使用一次。
            // 已有 route 的会话必须完整保留自己的 UI 状态，绝不受别的会话影响。
            if session.route.is_none() {
                session.route = Some(legacy_route.clone());
            }
        }
        (sessions, s.active_session)
    } else if let Some(ps) = &s.layout {
        (
            vec![SessionState {
                layout: ps.clone(),
                active: s.active,
                last_updated_at: unix_now_secs(),
                custom_title: None,
                acp: None,
                route: Some(legacy_route.clone()),
            }],
            0,
        )
    } else {
        (
            s.tabs
                .iter()
                .map(|cwd| SessionState {
                    layout: PaneState::Leaf {
                        cwd: cwd.clone(),
                        id: None,
                        custom_title: None,
                        launch_label: None,
                        launch_cmd: None,
                    },
                    active: 0,
                    last_updated_at: unix_now_secs(),
                    custom_title: None,
                    acp: None,
                    route: Some(legacy_route.clone()),
                })
                .collect(),
            s.active,
        )
    };

    // 开发版曾把每个智能体限制成一条独立运行。把它们追加成普通对话后，既保住
    // smeltd session id 和未消费指令，又解除“一定义一运行”的基数限制。
    let mut known_sids = sessions
        .iter()
        .filter_map(|session| session.acp.as_ref()?.sid.clone())
        .collect::<std::collections::HashSet<_>>();
    for run in &s.legacy_agent_runs {
        if run
            .acp
            .sid
            .as_ref()
            .is_some_and(|sid| !known_sids.insert(sid.clone()))
        {
            continue;
        }
        let mut acp = run.acp.clone();
        acp.agent_definition_id = Some(run.definition_id.clone());
        sessions.push(SessionState {
            layout: PaneState::Leaf {
                cwd: acp.cwd.clone(),
                id: None,
                custom_title: None,
                launch_label: None,
                launch_cmd: None,
            },
            active: 0,
            last_updated_at: unix_now_secs(),
            custom_title: None,
            acp: Some(acp),
            route: Some(legacy_route.clone()),
        });
    }

    (
        dedupe_saved_sessions(sessions, &mut active_session),
        active_session,
    )
}

/// 同一个 smeltd 会话 id 在存档里只能有一条。
///
/// 历史上的启动竞态（远程目录先到、存档恢复后到）会把同一场对话写成两条。它们
/// 会各自去 attach 同一个 ACP 会话、互相顶掉对方的 client，用户看到的就是侧栏里两
/// 条一模一样的会话、切过去反复断开重连。产生竞态的路径已经堵住，这里负责把已经
/// 写脏的存档在读取时收敛回来。
fn dedupe_saved_sessions(
    sessions: Vec<SessionState>,
    active_session: &mut usize,
) -> Vec<SessionState> {
    let saved_active = *active_session;
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut kept: Vec<SessionState> = Vec::with_capacity(sessions.len());
    for (old_ix, session) in sessions.into_iter().enumerate() {
        let sid = session
            .acp
            .as_ref()
            .and_then(|acp| acp.sid.clone())
            .filter(|sid| !sid.is_empty());
        if let Some(sid) = sid {
            if let Some(&kept_ix) = seen.get(&sid) {
                // 被丢掉的那条和保留的那条是同一场会话：当时停在它上面就改停在双胞胎上，
                // 而不是随便滑到邻居。
                if old_ix == saved_active {
                    *active_session = kept_ix;
                } else if old_ix < saved_active {
                    *active_session = active_session.saturating_sub(1);
                }
                continue;
            }
            seen.insert(sid, kept.len());
        }
        kept.push(session);
    }
    kept
}

/// 把旧 workspace.json 的工作区级右侧栏状态投影为单个会话的 route。
/// 新格式从不调用这里：新会话和新存档都直接拥有 `SessionRouteArchive`。
fn legacy_route_from_workspace(state: &WsState) -> SessionRouteArchive {
    let mut route = SessionRouteArchive::default();
    if let Some(open) = state.legacy_tool_panel_open {
        route.tool_panel_open = open;
    }
    if let Some(width) = state.legacy_tool_panel_w {
        route.tool_panel_w = width.max(MIN_TOOL_PANEL_WIDTH);
    }
    if let Some(width) = state.legacy_file_tree_w {
        route.file_tree_w = width.clamp(
            tool_panel::MIN_FILE_TREE_WIDTH,
            tool_panel::MAX_FILE_TREE_WIDTH,
        );
    }
    route.pinned_roots = state.legacy_pinned_file_tree_roots.clone();
    route.collapsed_roots = state
        .legacy_collapsed_file_tree_roots
        .iter()
        .cloned()
        .collect();
    route
}

/// 收集布局树所有叶子终端的 EntityId，顺序 = 深度优先遍历序（= 存档 active 基准）。
fn collect_leaf_ids(pane: &Pane, out: &mut Vec<EntityId>) {
    match pane {
        Pane::Leaf(t) => out.push(t.entity_id()),
        Pane::Split { children, .. } => {
            for c in children {
                collect_leaf_ids(c, out);
            }
        }
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
    pub(crate) collapsed_projects: Vec<String>,
    pub(crate) collapsed_agents: Vec<String>,
    pub(crate) sidebar_grouping: SidebarGrouping,
    pub(crate) pinned_projects: Vec<String>,
    pub(crate) sidebar_hide_empty_projects: bool,
}

impl WorkspaceUiState {
    fn from_state(state: &WsState) -> Self {
        Self {
            active_session: state.active_session,
            active_session_id: state.active_session_id.clone(),
            route: state.route.clone(),
            selected_agent_id: state.selected_agent_id.clone(),
            sidebar_w: state.sidebar_w,
            sidebar_open: state.sidebar_open,
            collapsed_projects: state.collapsed_projects.clone(),
            collapsed_agents: state.collapsed_agents.clone(),
            sidebar_grouping: state.sidebar_grouping,
            pinned_projects: state.pinned_projects.clone(),
            sidebar_hide_empty_projects: state.sidebar_hide_empty_projects,
        }
    }

    /// 只带界面状态的工作区存档：启动时先用它把窗口立起来，项目和会话等运行时
    /// 的目录到了再补。
    pub(crate) fn into_state(self) -> WsState {
        WsState {
            active_session: self.active_session,
            active_session_id: self.active_session_id,
            route: self.route,
            selected_agent_id: self.selected_agent_id,
            sidebar_w: self.sidebar_w,
            sidebar_open: self.sidebar_open,
            collapsed_projects: self.collapsed_projects,
            collapsed_agents: self.collapsed_agents,
            sidebar_grouping: self.sidebar_grouping,
            pinned_projects: self.pinned_projects,
            sidebar_hide_empty_projects: self.sidebar_hide_empty_projects,
            ..Default::default()
        }
    }
}

/// 交给运行时的那部分：去掉客户端界面状态。
fn catalog_document(mut state: WsState) -> WsState {
    let ui_defaults = WorkspaceUiState::default().into_state();
    state.active_session = ui_defaults.active_session;
    state.active_session_id = None;
    state.route = ui_defaults.route;
    state.selected_agent_id = None;
    state.sidebar_w = None;
    state.sidebar_open = None;
    state.collapsed_projects = Vec::new();
    state.collapsed_agents = Vec::new();
    state.sidebar_grouping = ui_defaults.sidebar_grouping;
    state.pinned_projects = Vec::new();
    state.sidebar_hide_empty_projects = false;
    state
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

/// 和运行时那份工作区目录的同步状态。保存在后台线程推进版本，所以用原子量共享。
#[derive(Clone, Default)]
pub(crate) struct WorkspaceCatalogSync {
    /// 上次读到或保存成功后的版本，下一次保存以它为基准。
    pub(crate) revision: Arc<AtomicU64>,
    /// 别的客户端改过目录、这边的保存被拒之后置位：不再提交，免得覆盖对方。
    pub(crate) blocked: Arc<AtomicBool>,
}

/// 一次 workspace 状态提交的快照。主线程构建（纯内存），后台线程提交。
struct SnapshotJob {
    ui: WorkspaceUiState,
    document: serde_json::Value,
    catalog: WorkspaceCatalogSync,
    /// 同步发给 smeltd 的侧栏菜单。网关只消费 daemon 订阅，不读工作区快照。
    menu: smelt_model::workspace_menu::WorkspaceMenuSnapshot,
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
    fn enqueue(&mut self, job: SnapshotJob) -> bool {
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

/// 后台线程提交：界面状态写客户端，目录以上次的版本为基准交给运行时。
///
/// 返回 `Some(提示)` 表示目录被别的客户端改过、这边以后不再提交。
fn persist_ws_snapshot(job: &SnapshotJob) -> Option<String> {
    if let Err(error) = save_workspace_ui(&job.ui) {
        eprintln!("[workspace] 保存界面状态失败：{error}");
    }
    let mut notice = None;
    if !job.catalog.blocked.load(Ordering::Acquire) {
        let params = WorkspaceSaveParams {
            base_revision: job.catalog.revision.load(Ordering::Acquire),
            document: job.document.clone(),
        };
        match crate::runtime_call::call::<WorkspaceSaveCall>(
            params,
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
        ) {
            Ok(saved) => job
                .catalog
                .revision
                .store(saved.revision, Ordering::Release),
            Err(ControlClientError::Protocol(error))
                if error.code == ControlErrorCode::Conflict =>
            {
                job.catalog.blocked.store(true, Ordering::Release);
                notice = Some(
                    "工作区已在另一台设备上被修改。本机之后的改动不再保存，重启 Smelt 会载入最新的工作区。"
                        .to_string(),
                );
            }
            Err(error) => eprintln!(
                "[workspace] 保存工作区失败：{}",
                crate::runtime_call::error_text(error)
            ),
        }
    }
    if let Err(e) =
        smelt_client::session_ops::publish_workspace_menu(&crate::runtimes::local(), &job.menu)
    {
        eprintln!("[workspace] 发布侧栏菜单失败: {e}");
    }
    notice
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
        let Some(job) = self.build_ws_snapshot(cx) else {
            return;
        };
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
                this.enqueue_ws_snapshot(job, epoch, cx);
            });
        });
    }

    /// 窗口起来后向运行时读工作区目录。先把本机守护拉起并对齐版本（可能交接），
    /// 再读；运行时还没就绪就按退避重试。
    pub(crate) fn start_workspace_catalog_load(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .spawn(async {
                    let _ = crate::terminal::ensure_managed_daemon_current();
                    crate::terminal::ensure_daemon_running();
                })
                .await;
            let mut attempt = 0u32;
            loop {
                let loaded = cx
                    .background_executor()
                    .spawn(async {
                        crate::runtime_call::call::<WorkspaceLoadCall>(
                            EmptyParams::default(),
                            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                        )
                    })
                    .await;
                match loaded {
                    Ok(result) => {
                        let _ = this.update_in(cx, |this, window, cx| {
                            this.apply_workspace_catalog(result, window, cx)
                        });
                        return;
                    }
                    Err(error) => {
                        if attempt == 4 {
                            let message = format!(
                                "正在等待运行时的工作区：{}",
                                crate::runtime_call::error_text(error)
                            );
                            let _ =
                                this.update(cx, |this, _| this.report_background_error(message));
                        }
                    }
                }
                cx.background_executor()
                    .timer(smelt_client::daemon_state::daemon_reconnect_backoff(
                        attempt,
                    ))
                    .await;
                attempt = attempt.saturating_add(1);
            }
        })
        .detach();
    }

    /// 目录到了：挂上项目、恢复会话、补做远程目录投影，此后才开始保存。
    fn apply_workspace_catalog(
        &mut self,
        result: WorkspaceLoadResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace_catalog
            .revision
            .store(result.revision, Ordering::Release);
        let parsed = result
            .document
            .map(serde_json::from_value::<WsState>)
            .transpose();
        let mut state = match parsed {
            Ok(state) => state.unwrap_or_default(),
            Err(error) => {
                // 读不懂的文档不能当成空工作区，否则下一次保存就把它冲掉了。
                self.workspace_catalog
                    .blocked
                    .store(true, Ordering::Release);
                self.report_background_error(format!(
                    "工作区目录无法读取，已停止保存以免覆盖原会话：{error}"
                ));
                WsState::default()
            }
        };
        state.active_session = self.startup_active_session;
        state.active_session_id = self.saved_active_session_id.clone();
        let (pending, active) = normalize_saved_sessions(&state);
        let saved_active_session_id = state
            .active_session_id
            .clone()
            .filter(|id| !id.is_empty())
            .or_else(|| {
                pending
                    .get(active)
                    .and_then(crate::session_state_persist_id)
            });
        // 项目列表：新存档直接读；旧存档（没有 projects 字段）从各会话 cwd 反推一份。
        // 目录到之前用户刚加的项目排在后面保留。
        let mut projects: Vec<String> = if state.projects.is_empty() {
            let mut seen: Vec<String> = Vec::new();
            for cwd in pending.iter().filter_map(crate::session_state_cwd) {
                if !cwd.is_empty() && !seen.contains(&cwd) {
                    seen.push(cwd);
                }
            }
            seen
        } else {
            std::mem::take(&mut state.projects)
        };
        for existing in &self.projects {
            if !projects.contains(existing) {
                projects.push(existing.clone());
            }
        }
        self.projects = projects;
        self.saved_active_session_id = saved_active_session_id.clone();
        self.restore_pending = pending.iter().cloned().enumerate().collect();
        self.workspace_catalog_loaded = true;
        if pending.is_empty() {
            self.sessions_restored = true;
            self.check_daemon_outdated(cx);
        } else {
            eprintln!("[workspace] 后台恢复 {} 个会话（不堵 UI）…", pending.len());
            self.schedule_session_restore(pending, active, saved_active_session_id, window, cx);
        }
        self.reconcile_remote_catalog_projection(window, cx);
        self.save_state(cx);
        cx.notify();
    }

    /// 所有 workspace 文档写入（常规保存与退出 flush）都进入同一条单消费者队列。
    /// 取下一项、标记 idle、唤醒退出等待者在同一个 `update` 中完成，避免旧实现里
    /// “看到空队列”与“把 inflight 置 false”之间的新快照永久搁置。
    fn enqueue_ws_snapshot(&mut self, job: SnapshotJob, epoch: u64, cx: &mut Context<Self>) {
        if epoch != self.ws_snapshot_epoch {
            return;
        }
        // 解封只能由「写入真正成功」触发，不能由「内存里有会话」触发：损坏窗口期
        // 新建的会话同样是非空快照，旧逻辑会立刻解除封锁，等库被别的进程修好后
        // 用这份残缺状态覆盖掉完整存档。
        if !self.ws_write_queue.enqueue(job) {
            return;
        }
        cx.spawn(async move |workspace, cx| {
            let executor = cx.background_executor().clone();
            loop {
                let next =
                    workspace.update(cx, |this, _| this.ws_write_queue.take_next_or_finish());
                let Ok((job, waiters)) = next else {
                    break;
                };
                let Some(job) = job else {
                    for waiter in waiters {
                        let _ = waiter.try_send(());
                    }
                    break;
                };
                let notice = executor
                    .spawn(async move { persist_ws_snapshot(&job) })
                    .await;
                if let Some(notice) = notice {
                    let _ = workspace.update(cx, |this, _| this.report_background_error(notice));
                }
            }
        })
        .detach();
    }

    /// 主线程构建待写快照（纯内存，不碰磁盘）。返回 None 表示本次无需写。
    fn build_ws_snapshot(&self, cx: &mut Context<Self>) -> Option<SnapshotJob> {
        // 目录还没从运行时读到：内存里的空列表不代表「用户关光了会话」，不能提交。
        if !self.workspace_catalog_loaded {
            return None;
        }
        let menu = self.workspace_menu_snapshot(cx);
        let mut sessions: Vec<SessionState> = self
            .sessions
            .iter()
            .filter(|session| {
                persist_in_workspace_snapshot(
                    matches!(session.kind, SessionKind::Conversation(_)),
                    session.remote_owned,
                )
            })
            .map(|s| {
                let route = if self.ui_session_id == Some(s.ui_id) {
                    &self.active_session_ui
                } else {
                    &s.ui_state
                };
                let last_updated_at = s.effective_updated_at(cx);
                match &s.kind {
                    SessionKind::Term { layout: l, .. } => {
                        let layout = pane_to_state(l, cx);
                        let mut ids = Vec::new();
                        collect_leaf_ids(l, &mut ids);
                        let active = ids.iter().position(|x| *x == s.anchor_id()).unwrap_or(0);
                        SessionState {
                            layout,
                            active,
                            last_updated_at,
                            custom_title: s.custom_title.clone(),
                            acp: None,
                            route: Some(route.archive()),
                        }
                    }
                    SessionKind::Conversation(view) => {
                        let v = view.read(cx);
                        SessionState {
                            // 占位叶子：旧版 smelt 读到降级开普通终端，不炸档。
                            layout: PaneState::Leaf {
                                cwd: v.cwd(),
                                id: None,
                                custom_title: None,
                                launch_label: None,
                                launch_cmd: None,
                            },
                            active: 0,
                            last_updated_at,
                            custom_title: s.custom_title.clone(),
                            acp: Some(snapshot_acp_view(
                                v,
                                s.agent_definition_id.clone(),
                                s.automation_id.clone(),
                            )),
                            route: Some(route.archive()),
                        }
                    }
                }
            })
            .collect();
        // 启动时恢复失败的会话按原位置写回，下次冷启动重试。
        sessions = merge_restore_pending(sessions, &self.restore_pending);

        let state = WsState {
            sessions,
            projects: self.projects.clone(),
            menu: menu.clone(),
            active_session: persisted_active_position(
                self.active_session,
                &self.restore_pending,
                self.sessions_restored,
            ),
            active_session_id: self
                .sessions
                .get(self.active_session)
                .and_then(|session| crate::live_session_persist_id(session, cx))
                .filter(|id| !id.is_empty())
                .or_else(|| self.saved_active_session_id.clone())
                .filter(|id| !id.is_empty()),
            route: self.nav.persist(),
            selected_agent_id: self.agent_surface.selected_id.clone(),
            sidebar_w: Some(self.sidebar_w),
            sidebar_open: Some(self.sidebar_open),
            collapsed_projects: self.collapsed_projects.iter().cloned().collect(),
            collapsed_agents: self.collapsed_agents.iter().cloned().collect(),
            sidebar_grouping: self.sidebar_grouping,
            pinned_projects: self.pinned_projects.iter().cloned().collect(),
            sidebar_hide_empty_projects: self.sidebar_hide_empty_projects,
            ..Default::default()
        };
        let ui = WorkspaceUiState::from_state(&state);
        let document = serde_json::to_value(catalog_document(state)).ok()?;
        Some(SnapshotJob {
            ui,
            document,
            catalog: self.workspace_catalog.clone(),
            menu,
        })
    }

    /// 退出阶段强制构建一份最新快照，并等待现有写盘循环自然清空。不能把 pending
    /// 快照拿出来另起线程写，否则旧循环可能在它之后完成并覆盖最终状态。
    pub(crate) fn flush_ws_state_on_quit(
        &mut self,
        cx: &mut Context<Self>,
    ) -> smol::channel::Receiver<()> {
        self.ws_snapshot_finalizing = true;
        self.ws_snapshot_epoch = self.ws_snapshot_epoch.wrapping_add(1);
        let epoch = self.ws_snapshot_epoch;
        if let Some(job) = self.build_ws_snapshot(cx) {
            self.enqueue_ws_snapshot(job, epoch, cx);
        }
        self.ws_write_queue.wait_for_drain()
    }
}

#[cfg(test)]
mod workspace_snapshot_write_queue_tests {
    use super::{SnapshotJob, WorkspaceSnapshotWriteQueue};

    fn job(name: &str) -> SnapshotJob {
        SnapshotJob {
            ui: Default::default(),
            document: serde_json::json!(name),
            catalog: Default::default(),
            menu: Default::default(),
        }
    }

    #[test]
    fn latest_pending_snapshot_wins_and_flush_waits_for_drain() {
        let mut queue = WorkspaceSnapshotWriteQueue::default();
        assert!(queue.enqueue(job("first")));
        let (first, waiters) = queue.take_next_or_finish();
        assert_eq!(first.unwrap().document, "first");
        assert!(waiters.is_empty());

        assert!(!queue.enqueue(job("final")));
        let done = queue.wait_for_drain();
        let (final_job, waiters) = queue.take_next_or_finish();
        assert_eq!(final_job.unwrap().document, "final");
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
        WorkspaceUiState, WsState, catalog_document, persist_in_workspace_snapshot,
        unproject_acp_when_remote_catalog_drops,
    };
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smelt-ws-agent-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 复现：手机/远程投影的 ACP 若不当工作区会话落盘，daemon 冷启动清空远程目录后
    /// 全局对话从侧栏消失，项目里的对话却还在。
    #[test]
    fn remote_owned_acp_conversations_are_workspace_durable() {
        assert!(
            persist_in_workspace_snapshot(true, true),
            "ACP 对话必须进工作区快照，即使最初是远程投影"
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

    #[test]
    fn agent_space_conversation_round_trips_through_workspace_snapshot() {
        let dir = temp_dir("space");
        let database = dir.join(smelt_store::DATABASE_FILE_NAME);
        let cwd = "/Users/me/.smelt/agents/space-agent";
        let json = format!(
            r#"{{
                "sessions": [{{
                    "layout": {{"Leaf": {{"cwd": "{cwd}"}}}},
                    "acp": {{
                        "sid": "acp-global-1",
                        "cwd": "{cwd}",
                        "agent": "pi",
                        "launch": {{"command": "smelt-pi-agent"}},
                        "agent_definition_id": "space-agent",
                        "history_session_id": "pi-history-1"
                    }}
                }}]
            }}"#
        );
        // 运行时按存储层的格式校验并落库；这里验证 GUI 的存档格式经过一趟仍完好。
        let store = smelt_store::Store::open_or_create(&database).unwrap();
        let snapshot = smelt_store::Store::workspace_snapshot_from_json(json.as_bytes())
            .expect("智能体 space 上的对话必须能写入工作区");
        store.put_workspace_snapshot(&snapshot).unwrap();
        let raw = smelt_store::Store::workspace_snapshot_to_json(
            &store.get_workspace_snapshot().unwrap().unwrap(),
        )
        .unwrap();
        let restored: WsState = serde_json::from_slice(&raw).expect("应读出智能体对话");
        assert_eq!(restored.sessions.len(), 1);
        let acp = restored.sessions[0].acp.as_ref().expect("ACP 细节");
        assert_eq!(acp.cwd.as_deref(), Some(cwd));
        assert_eq!(acp.agent_definition_id.as_deref(), Some("space-agent"));
        assert_eq!(acp.sid.as_deref(), Some("acp-global-1"));
        assert_eq!(
            acp.history_session_id
                .as_ref()
                .map(|id| id.to_string())
                .as_deref(),
            None,
            "第一次落库后这一列仍是空的，要等守护进程确认"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 侧栏、当前页这些归客户端，不能跟着工作区目录交给运行时；拆开再合回来，
    /// 两边各自的字段都不丢。
    #[test]
    fn client_ui_state_is_split_out_of_the_runtime_document() {
        let state = WsState {
            projects: vec!["/repo".into()],
            sidebar_w: Some(320.0),
            pinned_projects: vec!["/repo".into()],
            active_session_id: Some("acp-1".into()),
            ..Default::default()
        };
        let ui = WorkspaceUiState::from_state(&state);
        let document = catalog_document(state);

        assert_eq!(document.projects, vec!["/repo".to_string()]);
        assert_eq!(document.sidebar_w, None, "界面状态不进运行时的文档");
        assert!(document.pinned_projects.is_empty());
        assert_eq!(document.active_session_id, None);

        let restored = ui.into_state();
        assert_eq!(restored.sidebar_w, Some(320.0));
        assert_eq!(restored.pinned_projects, vec!["/repo".to_string()]);
        assert_eq!(restored.active_session_id.as_deref(), Some("acp-1"));
    }
}
