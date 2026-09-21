//! 工作区快照：SQLite 类型化快照的读写、单消费者写盘队列。

use std::path::PathBuf;

use gpui::{App, Context, EntityId};

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
    pub(crate) menu: smelt_core::workspace_menu::WorkspaceMenuSnapshot,
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
    /// 当前打开的插件工作台；None 表示舞台仍显示当前会话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) active_workspace_surface: Option<String>,
    /// 用户给工作台改过的名字。
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub(crate) workspace_surface_titles: std::collections::HashMap<String, String>,
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

pub(crate) fn workspace_database_path() -> Option<PathBuf> {
    smelt_paths::smelt_home().map(|h| h.join(smelt_store::DATABASE_FILE_NAME))
}

/// 工作区存档的读取结果。读失败绝不能当成「没有会话」，否则 GUI 会按新 App
/// 启动，首帧 `save_state` 再把空快照写进已经存在的库。
pub(crate) enum WorkspaceLoad {
    Loaded(WsState),
    Missing,
    Failed(String),
}

fn workspace_has_recoverable_state(state: &WsState) -> bool {
    !state.sessions.is_empty()
        || !state.legacy_agent_runs.is_empty()
        || state.layout.is_some()
        || !state.tabs.is_empty()
}

/// 这份待写快照里有没有值得落盘的东西。
///
/// 比 `workspace_has_recoverable_state` 多看一眼 projects：侧栏项目可以一个会话都没有，
/// 而 `delete_workspace` 恰恰会连 project 表一起清空。
fn snapshot_worth_persisting(json: &str) -> bool {
    let Ok(state) = serde_json::from_str::<WsState>(json) else {
        // 解析不了就别在这儿拦，让正式写入路径给出真正的错误。
        return true;
    };
    !state.projects.is_empty() || workspace_has_recoverable_state(&state)
}

fn load_ws_state_from_store(store: &smelt_store::Store) -> WorkspaceLoad {
    match store.get_workspace_snapshot() {
        Ok(Some(snapshot)) => {
            let raw = match smelt_store::Store::workspace_snapshot_to_json(&snapshot) {
                Ok(raw) => raw,
                Err(error) => {
                    return WorkspaceLoad::Failed(format!("workspace 快照编码失败: {error}"));
                }
            };
            match serde_json::from_slice(&raw) {
                Ok(state) => WorkspaceLoad::Loaded(state),
                Err(error) => WorkspaceLoad::Failed(format!("workspace 快照解析失败: {error}")),
            }
        }
        Ok(None) => WorkspaceLoad::Missing,
        Err(error) => WorkspaceLoad::Failed(format!("读取 workspace 快照失败: {error}")),
    }
}

pub(crate) fn load_ws_state() -> WorkspaceLoad {
    match workspace_database_path() {
        Some(path) => load_ws_state_from_database(&path),
        None => WorkspaceLoad::Missing,
    }
}

pub(crate) fn load_ws_state_from_database(database: &std::path::Path) -> WorkspaceLoad {
    // 只读。Store::open 不会建库；orphan sidecar 当成失败而不是「没有存档」，
    // 否则 persist 会在残留 WAL 旁边再造一个空主库。
    match smelt_store::database_presence(database) {
        smelt_store::DatabasePresence::Absent => WorkspaceLoad::Missing,
        smelt_store::DatabasePresence::OrphanSidecars => WorkspaceLoad::Failed(format!(
            "主库缺失但留下 WAL/SHM sidecar（{}），拒绝当作空存档",
            database.display()
        )),
        smelt_store::DatabasePresence::Ready => match smelt_store::Store::open(database) {
            Ok(store) => load_ws_state_from_store(&store),
            Err(error) => WorkspaceLoad::Failed(error.to_string()),
        },
    }
}

/// 一次 workspace 状态提交的快照。主线程构建（纯内存），后台线程提交。
struct SnapshotJob {
    database: PathBuf,
    json: String,
    /// 内存会话为空且恢复流程未跑完 → 落盘前需读盘检查，磁盘有数据则放弃写，
    /// 避免启动恢复失败时用空列表抹掉旧存档。
    guard_empty: bool,
    /// 本进程启动时磁盘上有存档但读不出来（库损坏/schema 不认识）。此时内存状态
    /// 只是「损坏窗口期内攒出来的残缺副本」，绝不能代表全量。落盘前必须确认磁盘
    /// 仍然读不出来，否则说明别的进程已经把库修好，写下去就是抹掉完整数据。
    load_failed: bool,
    /// 同步发给 smeltd 的侧栏菜单。网关只消费 daemon 订阅，不读工作区快照。
    menu: smelt_core::workspace_menu::WorkspaceMenuSnapshot,
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

/// 后台线程提交快照：先做「空保护」和「损坏窗口期保护」检查，再写 SQLite。
/// 失败只告警，不打扰用户。
///
/// 返回 true 表示磁盘状态已由本次写入接管（调用方可解除损坏窗口期封锁）。
fn persist_ws_snapshot(job: &SnapshotJob) -> bool {
    match load_ws_state_from_database(&job.database) {
        WorkspaceLoad::Failed(error) => {
            eprintln!("[workspace] 存档仍无法读取，跳过提交以免覆盖: {error}");
            return false;
        }
        WorkspaceLoad::Loaded(existing) => {
            let had = workspace_has_recoverable_state(&existing);
            if job.guard_empty && had {
                eprintln!("[workspace] 内存会话为空但持久化存档有数据，跳过提交以免抹掉 workspace");
                return false;
            }
            if job.load_failed && had {
                eprintln!(
                    "[workspace] 启动时存档读取失败，但磁盘现在已能读出 {} 个会话和 {} 个智能体运行；\
                     跳过提交以免覆盖，请重启 Smelt 以加载完整工作区",
                    existing.sessions.len(),
                    existing.legacy_agent_runs.len()
                );
                return false;
            }
        }
        WorkspaceLoad::Missing => {
            // 主文件和 sidecar 都不在：全新安装，或库被整份移走。
            // 空快照没有写入收益，推迟到用户真建了东西再落盘（那时 open_or_create
            // 才建库）。orphan sidecar 走 Failed，不会进这里。
            if !snapshot_worth_persisting(&job.json) {
                eprintln!("[workspace] 存档文件不存在且待写快照为空，跳过提交以免把空状态固化");
                return false;
            }
        }
    }
    let persist = (|| {
        let store = smelt_core::sqlite_state::open_sqlite_store(&job.database)?;
        let snapshot = smelt_store::Store::workspace_snapshot_from_json(job.json.as_bytes())?;
        store.put_workspace_snapshot(&snapshot)?;
        Ok::<(), String>(())
    })();
    if let Err(e) = persist {
        eprintln!("[workspace] 提交快照失败: {e}");
        return false;
    }
    if let Err(e) = smelt_core::session_control::publish_workspace_menu(&job.menu) {
        eprintln!("[workspace] 发布侧栏菜单失败: {e}");
    }
    true
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
        fork_origin: view.fork_origin(),
        conversation_binding: view.conversation_binding_for_save(),
        agent_session: view.agent_session_for_save(),
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
        let Some(database) = workspace_database_path() else {
            return;
        };
        let Some(job) = self.build_ws_snapshot(database, cx) else {
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
                let committed = executor
                    .spawn(async move { persist_ws_snapshot(&job) })
                    .await;
                if committed {
                    // 磁盘状态已由本进程接管，与内存一致，后续按常规路径写入。
                    let _ =
                        workspace.update(cx, |this, _| this.workspace_state_load_failed = false);
                }
            }
        })
        .detach();
    }

    /// 主线程构建待写快照（纯内存，不碰磁盘）。返回 None 表示本次无需写。
    fn build_ws_snapshot(&self, database: PathBuf, cx: &mut Context<Self>) -> Option<SnapshotJob> {
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

        // 文档存在但解析失败时，当前内存没有任何可证明的新状态。不要把空快照
        // 写回去；用户新建会话后 sessions 非空，才会用有效快照替换损坏数据。
        if self.workspace_state_load_failed && sessions.is_empty() {
            return None;
        }

        // 抹盘安全阀的决策输入：内存里一个会话都没有、也没有待恢复条目。
        // 历史上「守护未就绪 → 恢复全失败 → save_state 抹盘」会把用户所有侧栏
        // 会话永久清掉，所以这种「还没恢复上来的空」必须读盘确认磁盘真没数据才
        // 允许写空（否则重启又全恢复回来——「用户自己把会话全关了」是合法状态，
        // sessions_restored 跑完后的空允许写）。实际读盘判断在后台 persist 里做。
        let guard_empty = sessions.is_empty() && !self.sessions_restored;

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
            route: {
                let (route, _) = self.nav.persist();
                route
            },
            selected_agent_id: self.agent_surface.selected_id.clone(),
            active_workspace_surface: {
                let (_, surface) = self.nav.persist();
                surface.filter(|key| crate::plugin_ui::workspace_surface_by_key(key).is_some())
            },
            workspace_surface_titles: self.workspace_surface_titles.clone(),
            sidebar_w: Some(self.sidebar_w),
            sidebar_open: Some(self.sidebar_open),
            collapsed_projects: self.collapsed_projects.iter().cloned().collect(),
            collapsed_agents: self.collapsed_agents.iter().cloned().collect(),
            sidebar_grouping: self.sidebar_grouping,
            pinned_projects: self.pinned_projects.iter().cloned().collect(),
            sidebar_hide_empty_projects: self.sidebar_hide_empty_projects,
            ..Default::default()
        };
        let json = serde_json::to_string_pretty(&state).ok()?;
        Some(SnapshotJob {
            database,
            json,
            guard_empty,
            load_failed: self.workspace_state_load_failed,
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
        if let Some(database) = workspace_database_path()
            && let Some(job) = self.build_ws_snapshot(database, cx)
        {
            self.enqueue_ws_snapshot(job, epoch, cx);
        }
        self.ws_write_queue.wait_for_drain()
    }
}

#[cfg(test)]
mod workspace_load_failed_guard_tests {
    use super::{
        PathBuf, SnapshotJob, WorkspaceLoad, load_ws_state_from_database, persist_ws_snapshot,
    };

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smelt-ws-guard-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn database(dir: &std::path::Path) -> PathBuf {
        dir.join(smelt_store::DATABASE_FILE_NAME)
    }

    fn job(database: PathBuf, json: &str, load_failed: bool) -> SnapshotJob {
        SnapshotJob {
            database,
            json: json.to_string(),
            guard_empty: false,
            load_failed,
            menu: Default::default(),
        }
    }

    fn write_snapshot(database: &std::path::Path, json: &str) {
        let store = smelt_store::Store::open_or_create(database).unwrap();
        let snapshot = smelt_store::Store::workspace_snapshot_from_json(json.as_bytes()).unwrap();
        store.put_workspace_snapshot(&snapshot).unwrap();
    }

    fn existing_disk_state() -> &'static str {
        r#"{"projects":["/repo-a"],"sessions":[
            {"layout":{"Leaf":{"cwd":"/repo-a"}}},
            {"layout":{"Leaf":{"cwd":"/repo-b"}}}
        ]}"#
    }

    #[test]
    fn load_failed_snapshot_refuses_to_overwrite_readable_disk_state() {
        let dir = temp_dir("refuse");
        let database = database(&dir);
        write_snapshot(&database, existing_disk_state());

        let committed = persist_ws_snapshot(&job(
            database.clone(),
            r#"{"sessions":[{"layout":{"Leaf":{"cwd":"/only-new-session"}}}]}"#,
            true,
        ));

        assert!(!committed, "损坏窗口期的快照不该被提交");
        let WorkspaceLoad::Loaded(restored) = load_ws_state_from_database(&database) else {
            panic!("应读出原存档");
        };
        assert_eq!(restored.sessions.len(), 2, "磁盘上的完整状态必须原样保留");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_failed_snapshot_preserves_agent_owned_sessions() {
        let dir = temp_dir("agent-session");
        let database = database(&dir);
        write_snapshot(
            &database,
            r#"{
                "sessions": [{
                    "layout": {"Leaf": {"cwd": "/repo"}},
                    "acp": {
                        "sid": "acp-1",
                        "cwd": "/repo",
                        "agent": "pi",
                        "launch": {"command": "pi"},
                        "agent_definition_id": "quant"
                    }
                }]
            }"#,
        );

        let committed = persist_ws_snapshot(&job(database.clone(), r#"{"sessions":[]}"#, true));

        assert!(!committed, "智能体会话也是不可覆盖的有效工作区状态");
        let WorkspaceLoad::Loaded(restored) = load_ws_state_from_database(&database) else {
            panic!("应读出原存档");
        };
        assert_eq!(
            restored.sessions[0]
                .acp
                .as_ref()
                .and_then(|acp| acp.agent_definition_id.as_deref()),
            Some("quant")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_failed_snapshot_commits_when_disk_has_nothing_to_lose() {
        let dir = temp_dir("commit");
        let database = database(&dir);

        let committed = persist_ws_snapshot(&job(
            database.clone(),
            r#"{"sessions":[{"layout":{"Leaf":{"cwd":"/new"}}}]}"#,
            true,
        ));

        assert!(
            committed,
            "磁盘确实无可救数据时必须放行，否则新状态永远存不下"
        );
        let WorkspaceLoad::Loaded(restored) = load_ws_state_from_database(&database) else {
            panic!("应读出新存档");
        };
        assert_eq!(restored.sessions.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reading_workspace_state_never_creates_the_database() {
        // 只读检查一旦建库，库消失时就会立刻读出「Loaded(空)」，
        // persist 的三道防线全部失效——事故就是这么固化下来的。
        let dir = temp_dir("readonly");
        let database = database(&dir);

        assert!(matches!(
            load_ws_state_from_database(&database),
            WorkspaceLoad::Missing
        ));
        assert!(!database.exists(), "读一次不该把库造出来");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn vanished_database_does_not_get_recreated_from_an_empty_snapshot() {
        // 真实事故：库在运行途中消失，GUI 仍按内存里那份空状态提交，
        // put_workspace_snapshot 的 delete_workspace 把空固化成了新库。
        let dir = temp_dir("vanished");
        let database = database(&dir);

        let committed = persist_ws_snapshot(&job(
            database.clone(),
            r#"{"projects":[],"sessions":[]}"#,
            true,
        ));

        assert!(!committed, "库不在了又没内容可写，不能落盘");
        assert!(
            !database.exists(),
            "空快照连库都不该建出来，否则下次启动会把空状态当成正常存档"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn orphan_sidecars_are_load_failure_and_never_recreated() {
        let dir = temp_dir("orphan-wal");
        let database = database(&dir);
        std::fs::write(
            format!("{}-wal", database.to_string_lossy()),
            b"leftover-wal",
        )
        .unwrap();

        match load_ws_state_from_database(&database) {
            WorkspaceLoad::Failed(error) => {
                assert!(error.contains("WAL/SHM"), "{error}");
            }
            WorkspaceLoad::Loaded(_) => panic!("orphan sidecar 必须是 Failed，实际 Loaded"),
            WorkspaceLoad::Missing => panic!("orphan sidecar 必须是 Failed，实际 Missing"),
        }

        let committed = persist_ws_snapshot(&job(
            database.clone(),
            r#"{"projects":["/repo-a"],"sessions":[]}"#,
            true,
        ));
        assert!(!committed, "有 WAL 残留时即使内存有内容也不得建空主库");
        assert!(!database.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn vanished_database_still_accepts_a_snapshot_that_only_has_projects() {
        // 项目可以一个会话都没有，而 delete_workspace 会连 project 表一起清空，
        // 所以「有没有内容」的判据必须算上 projects。
        let dir = temp_dir("vanished-projects");
        let database = database(&dir);

        let committed = persist_ws_snapshot(&job(
            database.clone(),
            r#"{"projects":["/repo-a"],"sessions":[]}"#,
            false,
        ));

        assert!(committed, "只有项目没有会话也是真实工作区状态，必须存下来");
        let WorkspaceLoad::Loaded(restored) = load_ws_state_from_database(&database) else {
            panic!("应读出新存档");
        };
        assert_eq!(restored.projects, vec!["/repo-a".to_string()]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn healthy_snapshot_still_overwrites_disk_state() {
        let dir = temp_dir("healthy");
        let database = database(&dir);
        write_snapshot(&database, existing_disk_state());

        let committed = persist_ws_snapshot(&job(
            database.clone(),
            r#"{"sessions":[{"layout":{"Leaf":{"cwd":"/repo-a"}}}]}"#,
            false,
        ));

        assert!(committed);
        let WorkspaceLoad::Loaded(restored) = load_ws_state_from_database(&database) else {
            panic!("应读出覆盖后的存档");
        };
        assert_eq!(restored.sessions.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unrecognized_schema_is_load_failure_not_empty_workspace() {
        let dir = temp_dir("schema-refuse");
        let database = database(&dir);
        drop(smelt_store::Store::open_or_create(&database).unwrap());
        {
            let connection = rusqlite::Connection::open(&database).unwrap();
            connection
                .execute(
                    "INSERT INTO project(root, position, collapsed) VALUES ('/repo', 0, 0)",
                    [],
                )
                .unwrap();
            connection.pragma_update(None, "user_version", 99).unwrap();
        }

        match load_ws_state_from_database(&database) {
            WorkspaceLoad::Failed(error) => {
                assert!(
                    error.contains("拒绝打开") || error.contains("schema"),
                    "应报告 schema 拒绝打开，实际: {error}"
                );
            }
            WorkspaceLoad::Loaded(_) => panic!("未来 schema 必须是 Failed，实际 Loaded"),
            WorkspaceLoad::Missing => panic!("未来 schema 必须是 Failed，实际 Missing"),
        }

        let committed = persist_ws_snapshot(&job(
            database.clone(),
            r#"{"sessions":[{"layout":{"Leaf":{"cwd":"/new"}}}]}"#,
            true,
        ));
        assert!(!committed, "库打不开时禁止把内存里的新会话写进去");

        let connection = rusqlite::Connection::open(&database).unwrap();
        let projects: i64 = connection
            .query_row("SELECT COUNT(*) FROM project", [], |row| row.get(0))
            .unwrap();
        assert_eq!(projects, 1, "拒绝打开的库里原有项目必须还在");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod workspace_snapshot_write_queue_tests {
    use super::{PathBuf, SnapshotJob, WorkspaceSnapshotWriteQueue};

    fn job(name: &str) -> SnapshotJob {
        SnapshotJob {
            database: PathBuf::from(format!("/tmp/{name}.sqlite3")),
            json: name.to_string(),
            guard_empty: false,
            load_failed: false,
            menu: Default::default(),
        }
    }

    #[test]
    fn latest_pending_snapshot_wins_and_flush_waits_for_drain() {
        let mut queue = WorkspaceSnapshotWriteQueue::default();
        assert!(queue.enqueue(job("first")));
        let (first, waiters) = queue.take_next_or_finish();
        assert_eq!(first.unwrap().json, "first");
        assert!(waiters.is_empty());

        assert!(!queue.enqueue(job("final")));
        let done = queue.wait_for_drain();
        let (final_job, waiters) = queue.take_next_or_finish();
        assert_eq!(final_job.unwrap().json, "final");
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
        PathBuf, SnapshotJob, WorkspaceLoad, load_ws_state_from_database,
        persist_in_workspace_snapshot, persist_ws_snapshot,
        unproject_acp_when_remote_catalog_drops,
    };

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
        let committed = persist_ws_snapshot(&SnapshotJob {
            database: database.clone(),
            json,
            guard_empty: false,
            load_failed: false,
            menu: Default::default(),
        });
        assert!(committed, "智能体 space 上的对话必须能写入工作区");

        let WorkspaceLoad::Loaded(restored) = load_ws_state_from_database(&database) else {
            panic!("应读出智能体对话");
        };
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
            Some("pi-history-1")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
