//! 工作区持久化。运行时那份工作区目录（项目、对话）只有运行时写：这边收
//! `workspace.changed` 的推送，在改动发生的那一刻发单条操作（新建、关掉、换终端格子、
//! 改名、归档、置顶、开关项目），等推送回来再变。推送怎么落到本地会话见
//! [`crate::workspace_directory`]。客户端自己的界面状态（侧栏开合和顺序、当前页、各对话
//! 的分屏和路由）存在客户端库。写入走每个运行时一条的单消费者队列。

use std::collections::HashMap;
use std::collections::HashSet;

use gpui::{App, Context, Window};
use smelt_client_store::{ConversationRouteRecord, ProjectKey, SidebarOrderRecord};
use smelt_model::workspace_directory::{
    WorkspaceAcp, WorkspaceConversation, WorkspaceConversationIdParams, WorkspaceDirectory,
    WorkspaceProjectParams, WorkspaceTerminal,
};
use smelt_runtime_client::protocol::control::{
    ControlClientError, ControlErrorCode, ControlMethod,
};

use crate::runtimes::Runtime;
use crate::workspace_directory::{Receipts, order_by_saved};
use crate::{
    AcpSaved, PaneState, RuntimeKey, SessionKind, SessionState, SidebarGrouping, Workspace,
};

pub(crate) fn merge_restore_pending<T: Clone>(
    mut sessions: Vec<T>,
    pending: &[(usize, T)],
) -> Vec<T> {
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
        workset_id: conversation.workset_id,
        layout,
        active: 0,
        last_updated_at: conversation.last_updated_at.max(0) as u64,
        custom_title: conversation.custom_title,
        acp,
    }
}

fn acp_saved_from_directory(acp: WorkspaceAcp) -> AcpSaved {
    AcpSaved {
        cwd: acp.cwd,
        launch: acp.launch,
        profile_id: acp.profile_id,
        agent: Some(acp.agent).filter(|agent| !agent.is_empty()),
        agent_definition_id: acp.agent_definition_id,
        history_session_id: acp.history_session_id,
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
        history_session_id: acp.history_session_id,
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

/// 终端对话里现在的格子，按树里的顺序。ACP 对话没有。
fn session_terminals(session: &crate::Session, cx: &App) -> Vec<WorkspaceTerminal> {
    let SessionKind::Term { dock, active } = &session.kind else {
        return Vec::new();
    };
    // 树里一格都没有不会发生：关掉最后一格就关整个会话。真碰上了退回活动终端。
    let layout = dock
        .pane_state(cx)
        .unwrap_or_else(|| crate::term_dock::pane_leaf_state(active, cx));
    crate::conversation_layout::terminals_of(&layout)
        .into_iter()
        .filter_map(directory_terminal)
        .collect()
}

/// 客户端自己的界面状态。每台设备可以不同（设计文档 D1），不交给运行时。
#[derive(Clone, Debug, Default)]
pub(crate) struct WorkspaceUiState {
    pub(crate) active_session: usize,
    pub(crate) active_session_id: Option<String>,
    /// 首页出口里放着的视图 id（见 `router`）；放的是默认内容时是 `None`。
    pub(crate) outlet: Option<String>,
    /// 各运行时上折叠、置顶的项目。
    pub(crate) collapsed_projects: Vec<ProjectKey>,
    pub(crate) collapsed_agents: Vec<String>,
    pub(crate) sidebar_grouping: SidebarGrouping,
    pub(crate) pinned_projects: Vec<ProjectKey>,
    pub(crate) sidebar_hide_empty_projects: bool,
    pub(crate) archive_expanded: bool,
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

/// 客户端存的一段对话的界面状态（设计文档 D1）：终端怎么分屏、当前看哪个。
#[derive(Clone, Default)]
pub(crate) struct SavedConversationUi {
    /// 分屏布局，格式见 [`crate::conversation_layout`]。ACP 对话没有。
    pub(crate) dock_layout: Option<String>,
    pub(crate) active_terminal_id: Option<String>,
}

impl SavedConversationUi {
    /// 一段还没恢复上来、或恢复失败的会话现在的样子。
    fn of_state(state: &SessionState) -> Self {
        let is_terminal = state.acp.is_none();
        Self {
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

fn conversation_record(
    key: &ConversationKey,
    saved: &SavedConversationUi,
) -> ConversationRouteRecord {
    ConversationRouteRecord {
        runtime_id: project_key(&key.0, "").runtime_id,
        conversation_id: key.1.clone(),
        active_terminal_id: saved.active_terminal_id.clone(),
        dock_layout: saved.dock_layout.clone(),
    }
}

fn conversation_from_record(
    record: ConversationRouteRecord,
) -> (ConversationKey, SavedConversationUi) {
    let runtime = record
        .runtime_id
        .map(RuntimeKey::Remote)
        .unwrap_or(RuntimeKey::Local);
    (
        (runtime, record.conversation_id),
        SavedConversationUi {
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
        // 视图 id 是 `<来源>/<名字>`；旧版存的舞台路由名（session、conversation）不是。
        outlet: Some(record.route).filter(|view| view.contains('/')),
        collapsed_projects: record.collapsed_projects,
        collapsed_agents: record.collapsed_agents,
        sidebar_grouping: crate::settings::enum_from_name(&record.sidebar_grouping)
            .unwrap_or_default(),
        pinned_projects: record.pinned_projects,
        sidebar_hide_empty_projects: record.sidebar_hide_empty_projects,
        archive_expanded: record.archive_expanded,
    }
}

fn save_workspace_ui(ui: &WorkspaceUiState) -> Result<(), String> {
    let record = smelt_client_store::WorkspaceUiRecord {
        active_session: ui.active_session,
        active_session_id: ui.active_session_id.clone(),
        route: ui.outlet.clone().unwrap_or_default(),
        collapsed_projects: ui.collapsed_projects.clone(),
        collapsed_agents: ui.collapsed_agents.clone(),
        sidebar_grouping: crate::settings::enum_name(&ui.sidebar_grouping),
        pinned_projects: ui.pinned_projects.clone(),
        sidebar_hide_empty_projects: ui.sidebar_hide_empty_projects,
        archive_expanded: ui.archive_expanded,
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
    /// 客户端自己的那份（界面状态、各对话路由、侧栏顺序）只有一份，跟本机那份一起写；
    /// 本机目录还没收到时不写（活动会话的下标要按恢复上来的会话算）。
    ui: Option<ClientSnapshot>,
    /// 用户打开、关闭的项目，按发生的先后。跟在对话操作后面发。
    project_ops: Vec<ProjectOp>,
    conversation_ops: Vec<ConversationOp>,
}

/// 对一段对话的单条操作，在改动发生的那一刻发。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ConversationOp {
    /// 这边新开了一段对话：开的那一刻的样子（见 `Workspace::new_directory_conversation`）。
    Create(Box<WorkspaceConversation>),
    /// 终端对话里的格子变了（分屏、关掉一格）：只看 `terminals`，别的以库里的为准。
    SetTerminals(Box<WorkspaceConversation>),
    Archive {
        id: String,
        archived: bool,
    },
    Pin {
        id: String,
        pinned: bool,
    },
    Delete {
        id: String,
    },
}

impl ConversationOp {
    pub(crate) fn method(&self) -> ControlMethod {
        match self {
            Self::Create(_) => ControlMethod::WorkspaceConversationCreate,
            Self::SetTerminals(_) => ControlMethod::WorkspaceConversationUpdate,
            Self::Archive { .. } => ControlMethod::WorkspaceConversationArchive,
            Self::Pin { .. } => ControlMethod::WorkspaceConversationPin,
            Self::Delete { .. } => ControlMethod::WorkspaceConversationDelete,
        }
    }
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
            let mut ops = replaced.conversation_ops;
            ops.append(&mut job.conversation_ops);
            job.conversation_ops = ops;
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

/// 后台线程提交：客户端那份写客户端库，操作按发生的先后发给运行时。返回回执（主线程
/// 据此挡掉旧推送）和失败操作的错误文案。失败的不重发：目录保持运行时那边的样子。
fn persist_ws_snapshot(job: &SnapshotJob) -> (Receipts, Vec<String>) {
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
    let mut receipts = Receipts::default();
    let mut errors = Vec::new();
    for op in &job.conversation_ops {
        use smelt_model::control_api::{
            WorkspaceConversationArchive, WorkspaceConversationCreate, WorkspaceConversationDelete,
            WorkspaceConversationPin, WorkspaceConversationUpdate,
        };
        use smelt_model::workspace_directory::{
            WorkspaceConversationArchiveParams, WorkspaceConversationPinParams,
        };
        let client = job.runtime.client();
        let timeout = crate::runtime_call::RUNTIME_CALL_TIMEOUT;
        let result = match op {
            ConversationOp::Create(conversation) => {
                client.call::<WorkspaceConversationCreate>(conversation, timeout)
            }
            ConversationOp::SetTerminals(conversation) => {
                client.call::<WorkspaceConversationUpdate>(conversation, timeout)
            }
            ConversationOp::Archive { id, archived } => client
                .call::<WorkspaceConversationArchive>(
                    &WorkspaceConversationArchiveParams {
                        conversation_id: id.clone(),
                        archived: *archived,
                        target_workset_id: None,
                    },
                    timeout,
                ),
            ConversationOp::Pin { id, pinned } => client.call::<WorkspaceConversationPin>(
                &WorkspaceConversationPinParams {
                    conversation_id: id.clone(),
                    pinned: *pinned,
                },
                timeout,
            ),
            ConversationOp::Delete { id } => {
                let result = client.call::<WorkspaceConversationDelete>(
                    &WorkspaceConversationIdParams {
                        conversation_id: id.clone(),
                    },
                    timeout,
                );
                receipts.deletes_done.push(id.clone());
                result
            }
        };
        match result {
            Ok(revision) => receipts.confirm(revision),
            // 换格子时对话已经被别处关掉：推送随后会把它从本地摘掉。
            Err(ControlClientError::Protocol(error))
                if matches!(op, ConversationOp::SetTerminals(_))
                    && error.code == ControlErrorCode::NotFound => {}
            Err(error) => {
                errors.push(format!(
                    "{} 失败：{}",
                    op.method(),
                    crate::runtime_call::error_text(error)
                ));
            }
        }
    }
    errors.extend(send_project_ops(&job.runtime, &job.project_ops));
    (receipts, errors)
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

impl Workspace {
    /// 把客户端自己的界面状态（工作台、各对话的分屏和路由、侧栏顺序）写进客户端库。
    /// 目录不走这里：对话的改动在发生的那一刻发操作（见 [`ConversationOp`]）。
    ///
    /// 主线程只做纯内存快照（构建 + 序列化），读盘/写盘一律挪到后台线程——
    /// 历史上 open() 会被系统 EndpointSecurity 钩子（杀毒/EDR/打印管理等）拖住，
    /// 同步写盘会直接冻结 UI（崩溃报告 0805）。后台写盘是串行的：高频调用只
    /// 保留最新快照，旧快照不会后到覆盖新数据。
    pub(crate) fn save_state(&self, cx: &mut Context<Self>) {
        if self.ws_snapshot_finalizing {
            return;
        }
        let Some(job) = self.build_ws_snapshot(&RuntimeKey::Local, cx) else {
            return;
        };
        let jobs = [(RuntimeKey::Local, job)];
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
        let mut local_conversations = self
            .sessions
            .iter()
            .filter(|session| session.runtime(cx).key() == key)
            .map(|session| session.conversation_id.clone())
            .collect::<HashSet<_>>();
        let saved_order = self
            .saved_sidebar_orders
            .get(key)
            .cloned()
            .unwrap_or_default();
        let repositories = directory.repositories.clone();
        let worksets = directory.worksets.clone();
        let Some(space) = self.space_mut(key) else {
            return;
        };
        let version = directory.version();
        if space.directory.is_stale(&version) {
            return;
        }
        local_conversations.extend(
            space
                .restore_pending
                .iter()
                .map(|(_, state)| state.id.clone()),
        );
        // 项目列表就是这份目录里的，只按这台桌面的顺序排：第一份按库里记的，之后按侧栏
        // 现在的（拖过的顺序在这里），新开的排在最后。
        let order = if space.directory.loaded() {
            std::mem::take(&mut space.projects)
        } else {
            saved_order.projects.clone()
        };
        space.repositories = repositories;
        space.worksets = worksets;
        space
            .collapsed_worksets
            .retain(|id| space.worksets.iter().any(|workset| &workset.id == id));
        space.projects = order_by_saved(directory.projects, &order, String::as_str);
        let inbound =
            space
                .directory
                .receive(version, directory.conversations, &local_conversations);
        let runtime = space.runtime.clone();
        let archived_ids = space
            .directory
            .archived()
            .iter()
            .map(|c| c.id.clone())
            .collect::<HashSet<_>>();
        let archived_pending = space
            .restore_pending
            .iter()
            .filter(|(_, state)| archived_ids.contains(&state.id))
            .map(|(_, state)| (state.id.clone(), SavedConversationUi::of_state(state)))
            .collect::<Vec<_>>();

        space
            .restore_pending
            .retain(|(_, state)| !inbound.removed.contains(&state.id));

        for (id, saved) in archived_pending {
            self.saved_conversation_ui.insert((key.clone(), id), saved);
        }
        for id in &inbound.removed {
            if !archived_ids.contains(id) {
                self.saved_conversation_ui
                    .remove(&(key.clone(), id.clone()));
            }
            if let Some(ix) = self.sessions.iter().position(|session| {
                session.conversation_id == *id && session.runtime(cx).key() == key
            }) {
                if archived_ids.contains(id) {
                    let saved = self.saved_ui_of_session(&self.sessions[ix], cx);
                    self.saved_conversation_ui
                        .insert((key.clone(), id.clone()), saved);
                }
                self.remove_session(ix, false, cx);
            }
        }
        // 名字只从目录来（改名由运行时写、在跑的对话也由它同步给 agent），这里只更新显示。
        for (id, title) in inbound.titles {
            let Some(session) = self.sessions.iter_mut().find(|session| {
                session.conversation_id == id
                    && session.runtime(cx).key() == key
                    && session.custom_title != title
            }) else {
                continue;
            };
            session.custom_title = title;
            self.session_list_revision = self.session_list_revision.wrapping_add(1);
        }
        for (terminal_id, title) in inbound.terminal_titles {
            let pane = self
                .sessions
                .iter()
                .filter(|session| session.runtime(cx).key() == key)
                .flat_map(|session| session.term_leaves())
                .find(|pane| pane.read(cx).session_id() == terminal_id);
            if let Some(pane) = pane
                && pane.read(cx).custom_title() != title.as_deref()
            {
                pane.update(cx, |pane, _| pane.set_custom_title(title));
            }
        }
        // 运行时的库换过：本地开着、新库里没有的对话按现在的样子重新建进去。
        for id in inbound.recreate {
            let conversation = self
                .sessions
                .iter()
                .find(|session| session.conversation_id == id && session.runtime(cx).key() == key)
                .and_then(|session| self.new_directory_conversation(session, cx));
            if let Some(conversation) = conversation {
                self.request_conversation_op(
                    key,
                    ConversationOp::Create(Box::new(conversation)),
                    cx,
                );
            }
        }

        let mut added = inbound
            .added
            .into_iter()
            .map(session_state_from_conversation)
            .collect::<Vec<_>>();
        if inbound.first {
            added = order_by_saved(added, &saved_order.conversations, |state| state.id.as_str());
        }
        // 对话的界面状态（分屏、当前看哪个）存在客户端：按对话 id 填回来。
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
        self.save_state(cx);
        self.session_list_revision = self.session_list_revision.wrapping_add(1);
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
            let saved_id = self
                .saved_active_session_id
                .clone()
                .filter(|id| !id.is_empty());
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
            self.schedule_visible_refresh(cx);
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
                let (receipts, operation_errors) = executor
                    .spawn(async move { persist_ws_snapshot(&job) })
                    .await;
                let _ = workspace.update(cx, |this, cx| {
                    if let Some(space) = this.space_mut(&key) {
                        space.directory.record(receipts);
                    }
                    for error in operation_errors {
                        eprintln!("[workspace] {error}");
                        this.report_background_error(error);
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// 一段会话开的那一刻在运行时目录里的样子，随 [`ConversationOp::Create`] 发一次。
    /// 之后的变化各走各的：终端格子见 [`Self::sync_conversation_terminals`]，名字、归档、
    /// 置顶走各自的操作，ACP 对话的配置、首条消息、预设、活动时间由守护按它看到的写。
    /// ACP 视图没有守护会话 id 时返回 `None`：运行时按它认这段对话。
    pub(crate) fn new_directory_conversation(
        &self,
        session: &crate::Session,
        cx: &App,
    ) -> Option<WorkspaceConversation> {
        let last_updated_at = session.effective_updated_at(cx).min(i64::MAX as u64) as i64;
        let (terminals, acp) = match &session.kind {
            SessionKind::Term { .. } => (session_terminals(session, cx), None),
            SessionKind::Conversation(view) => {
                let view = view.read(cx);
                let saved = AcpSaved {
                    cwd: view.cwd(),
                    launch: view.launch_spec(),
                    profile_id: view.profile_id().map(str::to_string),
                    agent: Some(view.agent_kind().id().to_string()),
                    agent_definition_id: session.agent_definition_id.clone(),
                    history_session_id: view.history_session_id_for_save(),
                    sid: Some(view.session_id().to_string()),
                    refresh_launch_from_settings: view.refresh_launch_from_settings(),
                    conversation_binding: view.conversation_binding_for_save(),
                    config_values: view.config_values_for_save(),
                    pending_prompt: view.pending_prompt_for_save(),
                    pending_delivery_id: view.pending_delivery_id_for_save(),
                    pending_agent_preset: view.pending_agent_preset_for_save(),
                    automation_id: session.automation_id.clone(),
                    // agent 报的标题只由守护写进目录。
                    session_title: None,
                };
                (Vec::new(), Some(acp_directory(saved)?))
            }
        };
        // 名字不随新建写：只经 `workspace.rename` 写，见 `request_rename`。
        Some(WorkspaceConversation {
            id: session.conversation_id.clone(),
            custom_title: None,
            last_updated_at,
            terminals,
            acp,
            archived_at: None,
            pinned_at: None,
            workset_id: session.workset_id.clone(),
            closed_workset_root: None,
        })
    }

    /// 这边新开了 `ix` 这段会话：建进运行时目录。只在用户新开的那一刻调；恢复、别处
    /// 开的推来的都已经在目录里。
    pub(crate) fn announce_new_conversation(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get(ix) else {
            return;
        };
        let key = session.runtime(cx).key().clone();
        let Some(conversation) = self.new_directory_conversation(session, cx) else {
            eprintln!(
                "[workspace] 对话 {} 没有守护会话 id，没有建进目录",
                session.conversation_id
            );
            return;
        };
        self.request_conversation_op(&key, ConversationOp::Create(Box::new(conversation)), cx);
    }

    /// 终端对话里的格子变了（分屏、关掉一格、拖进拖出）：和目录里的不一样才发。
    /// 拖动分隔条、换位置之类格子没变的不发。目录里还没有这段（新建还没推回来）也发：
    /// 同一条队列里排在新建后面。
    pub(crate) fn sync_conversation_terminals(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get(ix) else {
            return;
        };
        if !matches!(session.kind, SessionKind::Term { .. }) {
            return;
        }
        let key = session.runtime(cx).key().clone();
        let terminals = session_terminals(session, cx);
        let unchanged = self.space(&key).is_none_or(|space| {
            space
                .directory
                .terminal_ids(&session.conversation_id)
                .is_some_and(|listed| {
                    listed
                        .iter()
                        .map(String::as_str)
                        .eq(terminals.iter().filter_map(|t| t.id.as_deref()))
                })
        });
        if unchanged {
            return;
        }
        let conversation = WorkspaceConversation {
            id: session.conversation_id.clone(),
            terminals,
            ..WorkspaceConversation::default()
        };
        self.request_conversation_op(
            &key,
            ConversationOp::SetTerminals(Box::new(conversation)),
            cx,
        );
    }

    /// 改名：交给运行时，名字写在哪由它定，改完随目录推送回来再显示。`terminal_id` 是给
    /// 对话里的这一格终端改名。
    pub(crate) fn request_rename(
        &mut self,
        runtime: Runtime,
        conversation_id: String,
        terminal_id: Option<String>,
        title: Option<String>,
        cx: &mut Context<Self>,
    ) {
        use smelt_model::control_api::WorkspaceRename;
        use smelt_model::workspace_directory::WorkspaceRenameParams;
        let params = WorkspaceRenameParams {
            conversation_id,
            terminal_id,
            title,
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    runtime
                        .client()
                        .call::<WorkspaceRename>(&params, crate::runtime_call::RUNTIME_CALL_TIMEOUT)
                })
                .await;
            if let Err(error) = result {
                let message = format!("改名失败：{}", crate::runtime_call::error_text(error));
                let _ = this.update(cx, |this, _| this.report_background_error(message));
            }
        })
        .detach();
    }

    pub(crate) fn request_workset_rename(
        &mut self,
        runtime: Runtime,
        params: smelt_model::workspace_directory::WorkspaceWorksetRenameParams,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::runtime_call::call_on::<
                            smelt_model::control_api::WorkspaceWorksetRename,
                        >(
                            runtime.client(),
                            params,
                            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                        )
                        .map_err(crate::runtime_call::error_text)
                })
                .await;
            if let Err(error) = result {
                let message = format!("重命名工作树失败：{error}");
                let _ = this.update(cx, |this, _| this.report_background_error(message));
            }
        })
        .detach();
    }

    pub(crate) fn request_workset_finish(
        &mut self,
        runtime: Runtime,
        params: smelt_model::workspace_directory::WorkspaceWorksetFinishParams,
        cx: &mut Context<Self>,
    ) {
        let runtime_key = runtime.key().clone();
        let workset_id = params.workset_id.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::runtime_call::call_on::<
                            smelt_model::control_api::WorkspaceWorksetFinish,
                        >(
                            runtime.client(),
                            params,
                            // 关闭前要 fetch 各仓库的远端，和新建工作树一样给足时间。
                            std::time::Duration::from_secs(30 * 60),
                        )
                        .map_err(crate::runtime_call::error_text)
                })
                .await;
            match result {
                Ok(result) => {
                    let _ = this.update(cx, |this, cx| {
                        apply_workset_finish_result(
                            &mut this.close_confirm,
                            &runtime_key,
                            &workset_id,
                            Ok(()),
                        );
                        let runtime_id = match &runtime_key {
                            RuntimeKey::Local => None,
                            RuntimeKey::Remote(id) => Some(id.as_str()),
                        };
                        match smelt_client_store::client_store().and_then(|store| {
                            store.set_workset_pinned(runtime_id, &workset_id, false)
                        }) {
                            Ok(()) => {
                                if let Some(space) = this.space_mut(&runtime_key) {
                                    space.pinned_worksets.remove(&workset_id);
                                }
                            }
                            Err(error) => this.report_background_error(format!(
                                "清理工作树置顶状态失败：{error}"
                            )),
                        }
                        cx.notify();
                    });
                    let mut message = "工作树已关闭".to_string();
                    if !result.retained_branches.is_empty() {
                        message.push_str(&format!(
                            "；保留复用分支：{}",
                            result.retained_branches.join("、")
                        ));
                    }
                    if !result.leftover_entries.is_empty() {
                        message.push_str(&format!(
                            "；未删除的目录内容：{}",
                            result
                                .leftover_entries
                                .iter()
                                .map(|path| format!("「{path}」"))
                                .collect::<Vec<_>>()
                                .join("、")
                        ));
                    }
                    crate::status_item::notify_success(message);
                }
                Err(error) => {
                    let message = format!("关闭工作树失败：{error}");
                    let _ = this.update(cx, |this, cx| {
                        if apply_workset_finish_result(
                            &mut this.close_confirm,
                            &runtime_key,
                            &workset_id,
                            Err(message.clone()),
                        ) {
                            cx.notify();
                        } else {
                            this.report_background_error(message);
                        }
                    });
                }
            }
        })
        .detach();
    }

    pub(crate) fn restore_archived_into_workset(
        &mut self,
        workset_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(crate::CloseConfirm::RestoreWorkset {
            runtime,
            id,
            pending,
            ..
        }) = self.close_confirm.as_mut()
        else {
            return;
        };
        if *pending {
            return;
        }
        let runtime_key = runtime.clone();
        let conversation_id = id.clone();
        let Some(runtime) = self.space(&runtime_key).map(|space| space.runtime.clone()) else {
            if let Some(crate::CloseConfirm::RestoreWorkset { error, .. }) = &mut self.close_confirm
            {
                *error = Some("运行时已断开，无法恢复会话".into());
            }
            cx.notify();
            return;
        };
        if let Some(crate::CloseConfirm::RestoreWorkset { pending, error, .. }) =
            &mut self.close_confirm
        {
            *pending = true;
            *error = None;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let request_id = conversation_id.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::runtime_call::call_on::<
                        smelt_model::control_api::WorkspaceConversationArchive,
                    >(
                        runtime.client(),
                        smelt_model::workspace_directory::WorkspaceConversationArchiveParams {
                            conversation_id: request_id,
                            archived: false,
                            target_workset_id: Some(workset_id),
                        },
                        crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                    )
                    .map_err(crate::runtime_call::error_text)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(crate::CloseConfirm::RestoreWorkset {
                    runtime,
                    id,
                    pending,
                    error,
                    ..
                }) = &mut this.close_confirm
                    && *runtime == runtime_key
                    && *id == conversation_id
                {
                    match result {
                        Ok(_) => this.close_confirm = None,
                        Err(message) => {
                            *pending = false;
                            *error = Some(format!("恢复失败：{message}"));
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
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

    /// 对话操作进这个运行时的写入队列，排在已经入队的操作后面。
    pub(crate) fn request_conversation_op(
        &mut self,
        key: &RuntimeKey,
        op: ConversationOp,
        cx: &mut Context<Self>,
    ) {
        let Some(mut job) = self.ops_job(key, cx) else {
            self.report_background_error("这台机器已经移除，无法修改对话".to_string());
            return;
        };
        job.conversation_ops.push(op);
        let epoch = self.ws_snapshot_epoch;
        self.enqueue_ws_snapshot(key, job, epoch, cx);
    }

    /// 项目操作和这一轮快照一起进这个运行时的写入队列，排在已经入队的改动后面。
    fn request_project_op(
        &mut self,
        key: &RuntimeKey,
        op: ProjectOp,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self
            .space(key)
            .is_some_and(|space| space.directory.loaded())
        {
            return false;
        }
        let Some(mut job) = self.ops_job(key, cx) else {
            return false;
        };
        job.project_ops.push(op);
        let epoch = self.ws_snapshot_epoch;
        let key = key.clone();
        // 和 save_state 一样等当前 effect cycle 结束再入队。
        let workspace = cx.weak_entity();
        cx.defer(move |cx| {
            if let Some(workspace) = workspace.upgrade() {
                workspace.update(cx, |this, cx| {
                    this.enqueue_ws_snapshot(&key, job, epoch, cx)
                });
            }
        });
        true
    }

    /// 本地关掉了一段对话：运行时目录里也关掉它（运行时顺带收进程）。删除回执回来之前，
    /// 推送里的旧目录不会把它带回来；没删成的报给用户，之后以推送为准。
    pub(crate) fn close_directory_conversation(
        &mut self,
        key: &RuntimeKey,
        conversation_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(space) = self.space_mut(key) else {
            return;
        };
        space.directory.close_conversation(conversation_id);
        self.request_conversation_op(
            key,
            ConversationOp::Delete {
                id: conversation_id.to_string(),
            },
            cx,
        );
    }

    /// 发操作用的一份待写任务：本机那份顺带写界面状态（目录到了才写）。
    fn ops_job(&self, key: &RuntimeKey, cx: &mut Context<Self>) -> Option<SnapshotJob> {
        if *key == RuntimeKey::Local
            && let Some(job) = self.build_ws_snapshot(key, cx)
        {
            return Some(job);
        }
        Some(SnapshotJob {
            runtime: self.space(key)?.runtime.clone(),
            ui: None,
            project_ops: Vec::new(),
            conversation_ops: Vec::new(),
        })
    }

    /// 主线程构建客户端界面状态那份待写快照（纯内存，不碰磁盘）。只有本机那份带；本机
    /// 目录还没到时不写：活动会话的下标要按恢复上来的会话算。
    fn build_ws_snapshot(&self, key: &RuntimeKey, cx: &mut Context<Self>) -> Option<SnapshotJob> {
        if *key != RuntimeKey::Local {
            return None;
        }
        let space = self.space(key)?;
        if !space.directory.loaded() {
            return None;
        }
        Some(SnapshotJob {
            runtime: space.runtime.clone(),
            ui: Some(ClientSnapshot {
                ui: self.workspace_ui_state(cx),
                routes: self.conversation_route_records(cx),
                orders: self.sidebar_order_records(cx),
            }),
            project_ops: Vec::new(),
            conversation_ops: Vec::new(),
        })
    }

    /// 客户端界面状态：折叠、置顶按运行时收齐。
    fn workspace_ui_state(&self, cx: &App) -> WorkspaceUiState {
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
            outlet: self.persisted_outlet(cx),
            collapsed_projects: Vec::new(),
            collapsed_agents: self.collapsed_agents.iter().cloned().collect(),
            sidebar_grouping: self.sidebar_grouping,
            pinned_projects: Vec::new(),
            sidebar_hide_empty_projects: self.sidebar_hide_empty_projects,
            archive_expanded: self.archive_expanded,
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
            let key = (
                session.runtime(cx).key().clone(),
                session.conversation_id.clone(),
            );
            records.insert(
                key.clone(),
                conversation_record(&key, &self.saved_ui_of_session(session, cx)),
            );
        }
        records.into_values().collect()
    }

    fn saved_ui_of_session(&self, session: &crate::Session, cx: &App) -> SavedConversationUi {
        let (dock_layout, active_terminal_id) = match &session.kind {
            SessionKind::Term { dock, active } => (
                dock.pane_state(cx)
                    .and_then(|layout| crate::conversation_layout::encode(&layout)),
                Some(active.read(cx).session_id().to_string()).filter(|id| !id.is_empty()),
            ),
            SessionKind::Conversation(_) => (None, None),
        };
        SavedConversationUi {
            dock_layout,
            active_terminal_id,
        }
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
        if let Some(job) = self.build_ws_snapshot(&RuntimeKey::Local, cx) {
            self.enqueue_ws_snapshot(&RuntimeKey::Local, job, epoch, cx);
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

fn apply_workset_finish_result(
    confirmation: &mut Option<crate::CloseConfirm>,
    runtime: &RuntimeKey,
    id: &str,
    result: Result<(), String>,
) -> bool {
    let Some(crate::CloseConfirm::Workset {
        project,
        id: current_id,
        pending,
        error,
        ..
    }) = confirmation
    else {
        return false;
    };
    if &project.runtime != runtime || current_id != id || !*pending {
        return false;
    }
    match result {
        Ok(()) => *confirmation = None,
        Err(message) => {
            *pending = false;
            *error = Some(message);
        }
    }
    true
}

#[cfg(test)]
mod workset_finish_feedback_tests {
    use super::apply_workset_finish_result;
    use crate::{CloseConfirm, ProjectRef, RuntimeKey};

    fn workset_confirmation(pending: bool) -> Option<CloseConfirm> {
        Some(CloseConfirm::Workset {
            project: ProjectRef {
                runtime: RuntimeKey::Local,
                root: "/project".into(),
            },
            id: "task".into(),
            title: "task".into(),
            pending,
            error: None,
        })
    }

    #[test]
    fn failed_close_remains_visible_and_blocks_confirmation_until_reopened() {
        let mut confirmation = workset_confirmation(true);
        assert!(confirmation.as_ref().unwrap().confirmation_blocked());
        assert!(apply_workset_finish_result(
            &mut confirmation,
            &RuntimeKey::Local,
            "task",
            Err("工作树仍绑定会话".into()),
        ));
        assert!(matches!(
            confirmation,
            Some(CloseConfirm::Workset { pending: false, error: Some(ref error), .. })
                if error.contains("仍绑定会话")
        ));
        assert!(confirmation.as_ref().unwrap().confirmation_blocked());
        let reopened = workset_confirmation(false);
        assert!(!reopened.as_ref().unwrap().confirmation_blocked());
    }

    #[test]
    fn success_closes_only_the_matching_confirmation() {
        let mut confirmation = workset_confirmation(true);
        assert!(!apply_workset_finish_result(
            &mut confirmation,
            &RuntimeKey::Remote("other".into()),
            "task",
            Ok(()),
        ));
        assert!(confirmation.is_some());
        assert!(apply_workset_finish_result(
            &mut confirmation,
            &RuntimeKey::Local,
            "task",
            Ok(()),
        ));
        assert!(confirmation.is_none());
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
            ui: Some(super::ClientSnapshot {
                ui: super::WorkspaceUiState {
                    active_session_id: Some(name.to_string()),
                    ..Default::default()
                },
                routes: Vec::new(),
                orders: Vec::new(),
            }),
            project_ops: Vec::new(),
            conversation_ops: Vec::new(),
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

    #[test]
    fn conversation_ops_are_serialized_and_survive_snapshot_replacement() {
        use super::ConversationOp;
        let mut queue = WorkspaceSnapshotWriteQueue::default();
        queue.enqueue(job("running"));
        queue.take_next_or_finish();
        let mut archive = job("archive");
        archive.conversation_ops.push(ConversationOp::Archive {
            id: "c1".into(),
            archived: true,
        });
        queue.enqueue(archive);
        let mut restore = job("restore");
        restore.conversation_ops.push(ConversationOp::Archive {
            id: "c1".into(),
            archived: false,
        });
        queue.enqueue(restore);
        queue.enqueue(job("ordinary-save"));
        assert_eq!(
            queue.take_next_or_finish().0.unwrap().conversation_ops,
            [
                ConversationOp::Archive {
                    id: "c1".into(),
                    archived: true
                },
                ConversationOp::Archive {
                    id: "c1".into(),
                    archived: false
                },
            ],
        );
    }

    fn name(job: SnapshotJob) -> String {
        job.ui.unwrap().ui.active_session_id.unwrap()
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
        conversation_from_record, conversation_record, project_key, project_roots_of,
        session_state_from_conversation,
    };
    use smelt_model::workspace_directory::{
        WorkspaceAcp, WorkspaceConversation, WorkspaceTerminal,
    };

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
            workset_id: Some("workset-1".into()),
            acp: Some(acp.clone()),
            ..WorkspaceConversation::default()
        });
        assert_eq!(state.id, "conv-space");
        assert_eq!(state.workset_id.as_deref(), Some("workset-1"));
        let saved = state.acp.expect("ACP 细节");
        assert_eq!(saved.cwd.as_deref(), Some(cwd));
        assert_eq!(saved.sid.as_deref(), Some("acp-global-1"));
        assert_eq!(saved.history_session_id.as_deref(), Some("pi-history-1"));
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

    fn leaf_ids(pane: &crate::PaneState) -> Vec<String> {
        match pane {
            crate::PaneState::Leaf { id, .. } => id.iter().cloned().collect(),
            crate::PaneState::Split { children, .. } | crate::PaneState::Tabs { children, .. } => {
                children.iter().flat_map(leaf_ids).collect()
            }
        }
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
        assert_eq!(leaf_ids(&state.layout), ["t-1", "t-2"]);
    }

    /// 对话的界面状态存进客户端库再读回来一样不少。
    #[test]
    fn conversation_ui_lives_in_the_client_store() {
        let key = (RuntimeKey::Remote("rt-linux".into()), "conv-1".to_string());
        let saved = SavedConversationUi {
            dock_layout: Some(r#"{"v":1,"center":{"terminal":"t-1"}}"#.into()),
            active_terminal_id: Some("t-1".into()),
        };

        let (back_key, back) = conversation_from_record(conversation_record(&key, &saved));
        assert_eq!(back_key, key);
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
