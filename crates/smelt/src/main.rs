//! smelt 工作台 —— 基于 gpui-component 的桌面窗口。
//!
//! Workspace 管理多个终端标签（TerminalView）：顶部标签栏切换 / 新建 / 关闭，
//! 下方渲染当前活动终端。每个终端各自独立（PTY、IME、滚动、resize）。
//!
//! 运行： cargo run --bin smelt

// ACP 连接层已经搬进 smelt_core::acp_conn（给 smeltd 未来托管 ACP 会话铺路），
// 这里不再 mod acp;，用的地方直接引 smelt_core::acp_conn。
//
// acp_completion / acp_view / markdown_mermaid / ui_theme / json_store 同理都已
// 搬出主 crate（acp_view.rs 独立成 smelt-acp-view，其余几个是它和主 GUI 共用的
// UI 基建，搬进 smelt-ui / smelt-core，见各自文件头注释）。这里用同名 `use`
// 重新导出成原来的模块路径，全库既有的 `crate::ui_theme::x()` 之类写法不用
// 逐处改——跟 session_history.rs 对 claude_paths 的重导出是同一个套路。
pub(crate) use smelt_acp_view::acp_view;
pub(crate) use smelt_core::json_store;
pub(crate) use smelt_ui::markdown_mermaid;
pub(crate) use smelt_ui::ui_theme;

mod agent;
mod claude_memory;
mod dock;
mod file_tree;
mod git_log;
mod git_log_view;
mod git_panel;
mod inspector;
mod mem_usage;
use smelt_core::osc;
mod panel_transition;
mod pet;
mod session_history;
mod session_list;
mod settings;
mod skills;
mod stage;
mod status_item;
mod storage_cleanup;
mod tasks;
mod terminal;
mod terminal_view;
mod workspace_frame;

mod updater;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use gpui::InteractiveElement;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::color_picker::ColorPickerState;
use gpui_component::input::Input;
use gpui_component::list::{List, ListDelegate, ListEvent, ListItem, ListState};
use gpui_component::notification::Notification;
use gpui_component::resizable::{
    ResizablePanelEvent, ResizableState, h_resizable, resizable_panel, v_resizable,
};
use gpui_component::slider::SliderState;
use gpui_component::*;
use notify::RecommendedWatcher;
use terminal_view::TerminalView;

use file_tree::{DeleteFileTarget, OpenFile, SearchState};
use git_panel::{BranchList, DeleteWorktreeTarget, GitDiff, GitStatusData, RepoInfo};
use session_history::{HistoryListState, HistoryPane, history_view};
use settings::{Appearance, LlmInputs, load_appearance, load_launch_config};

// Cmd+Q 退出的应用级 action（gpui 无默认菜单栏，需自建菜单栏 + 键位绑定）。
gpui::actions!(
    smelt,
    [
        Quit,
        OpenSettings,
        CheckForUpdate,
        ReportIssue,
        SendSelectionToTerminal,
        NewTask,
        PrevSession,
        NextSession
    ]
);

/// 命令面板里的一个可执行动作。
#[derive(Clone)]
enum Cmd {
    NewTab,
    OpenProject,
    CloseTab,
    NextTab,
    PrevTab,
    SwitchTab(usize),
}

/// 命令面板的单个列表项：标签 + 选中态。
#[derive(IntoElement)]
struct CmdItem {
    base: ListItem,
    label: SharedString,
    selected: bool,
}

impl CmdItem {
    fn new(id: impl Into<ElementId>, label: SharedString, selected: bool) -> Self {
        Self {
            base: ListItem::new(id).selected(selected),
            label,
            selected,
        }
    }
}

impl Selectable for CmdItem {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for CmdItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let fg = if self.selected {
            cx.theme().accent_foreground
        } else {
            cx.theme().foreground
        };
        self.base
            .px_3()
            .py_1()
            .child(div().text_color(fg).child(self.label))
    }
}

/// 命令面板列表的数据源：全部命令 + 当前查询过滤结果。
/// 搜索输入、上下选择、回车确认、Esc 取消都由 `ListState` 负责。
struct CmdDelegate {
    all: Vec<(SharedString, Cmd)>,
    matched: Vec<(SharedString, Cmd)>,
    selected_index: Option<IndexPath>,
}

impl CmdDelegate {
    fn new(all: Vec<(SharedString, Cmd)>) -> Self {
        Self {
            matched: all.clone(),
            all,
            selected_index: Some(IndexPath::default()),
        }
    }
}

impl ListDelegate for CmdDelegate {
    type Item = CmdItem;

    fn items_count(&self, _section: usize, _: &App) -> usize {
        self.matched.len()
    }

    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        let q = query.to_lowercase();
        self.matched = self
            .all
            .iter()
            .filter(|(label, _)| q.is_empty() || label.to_lowercase().contains(&q))
            .cloned()
            .collect();
        Task::ready(())
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected_index = ix;
        cx.notify();
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let selected = Some(ix) == self.selected_index;
        self.matched
            .get(ix.row)
            .map(|(label, _)| CmdItem::new(ix, label.clone(), selected))
    }
}

/// 舞台覆盖页（stage_override 的取值）：任务总览 / 文件树 / Git /
/// 历史。曾是主区 TabBar 的互斥视图（含 Terminal 变体）；改版后终端
/// 舞台 = `stage_override == None`，这里只剩「盖在舞台上的全屏页」。
#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum MainView {
    /// 任务总览（卡片网格，内容只含任务）。
    Tasks,
    /// 「文件树 + 内容」双栏全宽（inspector FILES 面板 ⤢ 提升上来；此时面板收起）。
    Files,
    /// 「变更列表 + diff」双栏全宽（inspector GIT 面板 ⤢ 提升上来；此时面板收起）。
    /// 日志现在是 GIT 面板内部的子标签（见 GitTab），不再是独立变体。
    Git,
    Skills,
    History,
}

/// 左侧一级导航。任务页与当前 session 是并列 route，不借用 session 内部的
/// `stage_override`；两边状态都常驻，切换只改变当前显示哪一棵视图。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkspaceRoute {
    Tasks,
    #[default]
    Session,
}

/// 只用于读取旧 route 存档。运行时底部抽屉已经只有终端；旧 Files/Git 标签恢复时
/// 直接丢弃，避免删除功能后令整个 workspace.json 无法反序列化。
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArchivedDrawerTabKind {
    Terminal,
    Files,
    Git,
}

/// 底部抽屉的终端标签；每个标签自己持有一个终端进程。
struct DrawerTab {
    id: u64,
    terminal: Option<Entity<TerminalView>>,
    spawning: bool,
}

/// 一个长期存活的右侧路由实例。左侧 session 路由器只整体交换这个对象，不读取其中
/// 任何实现字段；缓存实体本身（例如底部终端）而不只缓存描述，切回来才能停在原来的
/// 进程、页面和尺寸上。
struct SessionUiState {
    restored_from_archive: bool,
    /// 冷恢复后首次激活 route 时重新打开；运行期打开完成后即清空。
    pending_restore_file: Option<String>,
    stage_override: Option<MainView>,
    inspector_tab: inspector::InspectorTab,
    inspector_open: bool,
    inspector_w: f32,
    bottom_drawer_open: bool,
    bottom_drawer_tabs: Vec<DrawerTab>,
    bottom_drawer_active: usize,
    bottom_drawer_next_id: u64,
    bottom_drawer_h: f32,
    expanded: HashSet<String>,
    file_tree_selected: Option<String>,
    open_file: Option<OpenFile>,
    file_tree_w: f32,
    pinned_roots: Vec<String>,
    collapsed_roots: HashSet<String>,
    git_tab: GitTab,
    git_diff: Option<GitDiff>,
    diff_selected: HashSet<usize>,
    active_hunk: Option<usize>,
    diff_split: bool,
    git_tree_collapsed: HashSet<String>,
    diff_scope: git_panel::DiffScope,
}

impl Default for SessionUiState {
    fn default() -> Self {
        Self {
            restored_from_archive: false,
            pending_restore_file: None,
            stage_override: None,
            inspector_tab: inspector::InspectorTab::Files,
            inspector_open: true,
            inspector_w: 344.0,
            bottom_drawer_open: false,
            bottom_drawer_tabs: Vec::new(),
            bottom_drawer_active: 0,
            bottom_drawer_next_id: 0,
            bottom_drawer_h: 260.0,
            expanded: HashSet::new(),
            file_tree_selected: None,
            open_file: None,
            file_tree_w: 260.0,
            pinned_roots: Vec::new(),
            collapsed_roots: HashSet::new(),
            git_tab: GitTab::Changes,
            git_diff: None,
            diff_selected: HashSet::new(),
            active_hunk: None,
            diff_split: false,
            git_tree_collapsed: HashSet::new(),
            diff_scope: git_panel::DiffScope::All,
        }
    }
}

/// `SessionUiState` 的跨进程镜像。这里没有 GPUI Entity；route 自己负责把路径、tab
/// 描述和尺寸重建成运行时对象，Workspace 只把这块不透明数据随 session 存取。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct SessionRouteArchive {
    version: u32,
    stage_override: Option<MainView>,
    inspector_tab: inspector::InspectorTab,
    inspector_open: bool,
    inspector_w: f32,
    bottom_drawer_open: bool,
    bottom_drawer_tabs: Vec<ArchivedDrawerTabKind>,
    bottom_drawer_active: usize,
    bottom_drawer_h: f32,
    expanded: HashSet<String>,
    file_tree_selected: Option<String>,
    open_file_path: Option<String>,
    file_tree_w: f32,
    pinned_roots: Vec<String>,
    collapsed_roots: HashSet<String>,
    git_tab: GitTab,
    diff_split: bool,
    git_tree_collapsed: HashSet<String>,
    diff_scope: git_panel::DiffScope,
}

impl Default for SessionRouteArchive {
    fn default() -> Self {
        Self {
            version: 1,
            stage_override: None,
            inspector_tab: inspector::InspectorTab::Files,
            inspector_open: true,
            inspector_w: 344.0,
            bottom_drawer_open: false,
            bottom_drawer_tabs: Vec::new(),
            bottom_drawer_active: 0,
            bottom_drawer_h: 260.0,
            expanded: HashSet::new(),
            file_tree_selected: None,
            open_file_path: None,
            file_tree_w: 260.0,
            pinned_roots: Vec::new(),
            collapsed_roots: HashSet::new(),
            git_tab: GitTab::Changes,
            diff_split: false,
            git_tree_collapsed: HashSet::new(),
            diff_scope: git_panel::DiffScope::All,
        }
    }
}

impl SessionUiState {
    fn archive(&self) -> SessionRouteArchive {
        SessionRouteArchive {
            // Tasks 是左侧一级导航，不属于某个 session 的右侧 route。只持久化
            // Inspector 全屏和历史等真正的 session 工作区页面。
            stage_override: self.stage_override.filter(|view| *view != MainView::Tasks),
            inspector_tab: self.inspector_tab,
            inspector_open: self.inspector_open,
            inspector_w: self.inspector_w,
            bottom_drawer_open: self.bottom_drawer_open,
            bottom_drawer_tabs: self
                .bottom_drawer_tabs
                .iter()
                .map(|_| ArchivedDrawerTabKind::Terminal)
                .collect(),
            bottom_drawer_active: self.bottom_drawer_active,
            bottom_drawer_h: self.bottom_drawer_h,
            expanded: self.expanded.clone(),
            file_tree_selected: self.file_tree_selected.clone(),
            open_file_path: self
                .open_file
                .as_ref()
                .map(|file| file.path.clone())
                .or_else(|| self.pending_restore_file.clone()),
            file_tree_w: self.file_tree_w,
            pinned_roots: self.pinned_roots.clone(),
            collapsed_roots: self.collapsed_roots.clone(),
            git_tab: self.git_tab,
            diff_split: self.diff_split,
            git_tree_collapsed: self.git_tree_collapsed.clone(),
            diff_scope: self.diff_scope,
            ..Default::default()
        }
    }

    fn restore(archive: SessionRouteArchive) -> Self {
        let tabs = archive
            .bottom_drawer_tabs
            .into_iter()
            .filter(|kind| *kind == ArchivedDrawerTabKind::Terminal)
            .enumerate()
            .map(|(id, _)| DrawerTab {
                id: id as u64,
                terminal: None,
                spawning: false,
            })
            .collect::<Vec<_>>();
        let active = archive
            .bottom_drawer_active
            .min(tabs.len().saturating_sub(1));
        Self {
            restored_from_archive: true,
            pending_restore_file: archive.open_file_path,
            stage_override: archive
                .stage_override
                .filter(|view| *view != MainView::Tasks),
            inspector_tab: archive.inspector_tab,
            inspector_open: archive.inspector_open,
            inspector_w: archive.inspector_w.max(280.0),
            bottom_drawer_open: archive.bottom_drawer_open,
            bottom_drawer_next_id: tabs.len() as u64,
            bottom_drawer_tabs: tabs,
            bottom_drawer_active: active,
            bottom_drawer_h: archive.bottom_drawer_h.clamp(120.0, 560.0),
            expanded: archive.expanded,
            file_tree_selected: archive.file_tree_selected,
            open_file: None,
            file_tree_w: archive.file_tree_w.max(160.0),
            pinned_roots: archive.pinned_roots,
            collapsed_roots: archive.collapsed_roots,
            git_tab: archive.git_tab,
            git_diff: None,
            diff_selected: HashSet::new(),
            active_hunk: None,
            diff_split: archive.diff_split,
            git_tree_collapsed: archive.git_tree_collapsed,
            diff_scope: archive.diff_scope,
        }
    }
}

fn swap_right_route(current: &mut SessionUiState, parked: &mut SessionUiState) {
    std::mem::swap(current, parked);
}

/// Git 页内部的子页。对标 JetBrains 的 Git 工具窗口——「提交」和「日志」是同一个
/// 窗口里的两个视图，不占两个顶层标签。
#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum GitTab {
    /// 提交历史 + 分支图。
    Log,
    /// 工作区改动：文件树 + diff + 暂存 / 提交。也是旧版已删除 tab 的迁移目标。
    #[serde(other)]
    Changes,
}

impl Default for GitTab {
    fn default() -> Self {
        Self::Changes
    }
}

// 会话里 agent 的状态（总览页状态徽章 / 侧栏状态点）：搬进 smelt-core（跟
// ui_theme 共用同一份判断，见 agent_status.rs），这里重导出成原来的裸名字，
// 全库既有的 `AgentStatus::x` 写法不用逐处改。
pub(crate) use smelt_core::agent_status::AgentStatus;

// DaemonStates（守护上报的会话状态镜像）/ AttentionGlobal（统一关注事件 store）：
// ACP 视图独立成 smelt-acp-view 后要跨
// crate 读写，搬进 smelt-ui（daemon_states_global.rs）共享，这里重导出成原来
// 的裸名字。
pub(crate) use smelt_ui::daemon_states_global::{AttentionGlobal, AttentionKind, DaemonStates};

/// 取某个 pane 对应的守护状态；没有全局单例（比如极早期尚未走到注册那一步）或
/// 那个 session id 还没有数据都返回 None。
fn daemon_state_for(view: &Entity<TerminalView>, cx: &App) -> Option<terminal::DaemonSessionState> {
    let id = view.read(cx).session_id().to_string();
    cx.try_global::<DaemonStates>()?
        .0
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
}

fn daemon_agent_status(
    session_id: &str,
    state: &terminal::DaemonSessionState,
    cx: &App,
) -> Option<AgentStatus> {
    let status = AgentStatus::from_daemon_phase(state.phase)?;
    if status != AgentStatus::Done {
        return Some(status);
    }
    cx.try_global::<AttentionGlobal>()
        .and_then(|store| {
            store
                .0
                .lock()
                .unwrap()
                .unread(session_id)
                .map(|item| item.kind)
        })
        .filter(|kind| *kind == AttentionKind::Success)
        .map(|_| AgentStatus::Done)
}

fn agent_notification_enabled(config: &settings::AgentUiConfig, kind: AttentionKind) -> bool {
    match kind {
        AttentionKind::Approval => config.notify_approval,
        AttentionKind::Input => config.notify_input,
        AttentionKind::Success => config.notify_success,
        AttentionKind::Failure => config.notify_failure,
        AttentionKind::Bell => config.notify_terminal_bell,
        AttentionKind::Notice => true,
    }
}

/// 主区终端分屏布局树：叶子是一个终端，内部 Split 把区域按某轴切成多块。
/// 每个 Split 各持一个 ResizableState 记住拖动比例；递归即可任意嵌套分屏。
enum Pane {
    Leaf(Entity<TerminalView>),
    Split {
        axis: Axis,
        state: Entity<ResizableState>,
        children: Vec<Pane>,
        /// 从存档恢复的各子块像素尺寸；新建分屏是空的（均分）。
        ///
        /// 渲染时当 `resizable_panel().size()` 的**初始值**传下去。每帧原样传也不会
        /// 冲掉用户拖出来的比例——gpui-component 里 initial_size 只在 panel 自己还
        /// 没有 size 时生效，一旦拖过就走 `panel_state.size` 那条分支（panel.rs）。
        init_sizes: Vec<f32>,
    },
}

/// 拖拽会话排序时跟随鼠标的小预览 chip（侧栏「项目内会话拖拽」用）。
#[derive(Clone)]
struct SessionDrag {
    id: EntityId,
    title: SharedString,
}

impl Render for SessionDrag {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        div()
            .id("session-drag-preview")
            .cursor_grab()
            .py_1()
            .px_3()
            .rounded_md()
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .text_xs()
            .text_color(t.foreground)
            .child(self.title.clone())
            // 拖起瞬间淡入，别让 chip "啪"地闪现
            .with_animation(
                "session-drag-in",
                Animation::new(std::time::Duration::from_millis(120)).with_easing(ease_out_quint()),
                |this, delta| this.opacity(delta),
            )
    }
}

/// 拖拽项目分组排序时跟随鼠标的小预览 chip。
/// 旧侧栏的项目行拖拽已撤；待接到项目 rail 后复活（收尾阶段定去留）。
#[derive(Clone)]
#[allow(dead_code)]
struct ProjectDrag {
    name: SharedString,
}

impl Render for ProjectDrag {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        div()
            .id("project-drag-preview")
            .cursor_grab()
            .py_1()
            .px_3()
            .rounded_md()
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .text_xs()
            .text_color(t.foreground)
            .child(self.name.clone())
            // 拖起瞬间淡入，同 SessionDrag
            .with_animation(
                "project-drag-in",
                Animation::new(std::time::Duration::from_millis(120)).with_easing(ease_out_quint()),
                |this, delta| this.opacity(delta),
            )
    }
}

/// 一个会话的内容形态。Term 是第一通道（PTY 分屏树），Acp 是第二通道（结构化
/// 消息流，见 docs/project-report.md 第 5 节）——后者不参与分屏，一会话一视图。
enum SessionKind {
    /// 终端会话 = 一棵独立分屏树 + 会话内当前活动 pane（终端）。
    Term {
        layout: Pane,
        active: Entity<TerminalView>,
    },
    /// ACP 消息流会话：单视图，不参与分屏。
    Acp(Entity<acp_view::AcpView>),
}

/// 侧栏每条对应一个会话；主区显示当前会话的内容（分屏树或 ACP 消息流）。
struct Session {
    /// 只用于把运行时 UI 快照稳定地绑到 session；拖拽排序和活动 pane 变化都不改它。
    ui_id: u64,
    kind: SessionKind,
    /// 用户手动改过的会话名（侧栏右键「重命名」）；None = 用下面 title() 的自动推导。
    custom_title: Option<String>,
    /// ACP 会话内容变化（AcpViewEvent::Changed）→ save_state 的订阅；Term 会话
    /// 没有（终端内容不经这条通道持久化，走 daemon session id 就够）。
    _acp_persist_sub: Option<gpui::Subscription>,
    /// 此 session 离开舞台时保存的完整右侧工作区。
    ui_state: SessionUiState,
}

impl Session {
    /// 单终端会话。
    fn single(view: Entity<TerminalView>) -> Self {
        Self {
            ui_id: next_session_ui_id(),
            kind: SessionKind::Term {
                layout: Pane::Leaf(view.clone()),
                active: view,
            },
            custom_title: None,
            _acp_persist_sub: None,
            ui_state: SessionUiState::default(),
        }
    }

    /// 会话身份锚点：侧栏选中态、拖拽、activate 等都拿它做「是同一个会话吗」比较。
    /// Term = 活动终端的 entity id。
    fn anchor_id(&self) -> EntityId {
        match &self.kind {
            SessionKind::Term { active, .. } => active.entity_id(),
            SessionKind::Acp(view) => view.entity_id(),
        }
    }

    /// 终端会话的活动 pane；ACP 会话返回 None（调用方借此天然跳过终端专属操作）。
    fn active_term(&self) -> Option<&Entity<TerminalView>> {
        match &self.kind {
            SessionKind::Term { active, .. } => Some(active),
            SessionKind::Acp(_) => None,
        }
    }

    /// ACP 会话的视图；终端会话返回 None（跟 `active_term` 反过来，供侧栏右键
    /// 的「强制重启」这类 ACP 专属操作用）。
    fn active_acp(&self) -> Option<&Entity<acp_view::AcpView>> {
        match &self.kind {
            SessionKind::Term { .. } => None,
            SessionKind::Acp(view) => Some(view),
        }
    }

    /// 切换终端会话的活动 pane；非终端会话是 no-op。
    fn set_active_term(&mut self, view: Entity<TerminalView>) {
        match &mut self.kind {
            SessionKind::Term { active, .. } => *active = view,
            SessionKind::Acp(_) => {}
        }
    }

    /// 终端会话的分屏树；ACP 会话没有。
    fn term_layout(&self) -> Option<&Pane> {
        match &self.kind {
            SessionKind::Term { layout, .. } => Some(layout),
            SessionKind::Acp(_) => None,
        }
    }

    fn term_layout_mut(&mut self) -> Option<&mut Pane> {
        match &mut self.kind {
            SessionKind::Term { layout, .. } => Some(layout),
            SessionKind::Acp(_) => None,
        }
    }

    /// 收集会话内全部终端叶子（ACP 会话得到空列表）。
    fn term_leaves(&self) -> Vec<Entity<TerminalView>> {
        let mut v = Vec::new();
        if let Some(layout) = self.term_layout() {
            collect_leaves(layout, &mut v);
        }
        v
    }

    /// 侧栏行图标：终端会话按启动方式（LaunchKind）对应，与「+」菜单图标一一对应。
    /// 新会话列表改用类型点（agent 紫圆 / 终端绿方），此图标暂时闲置——
    /// 收尾阶段决定是否用回行首或删除。
    #[allow(dead_code)]
    fn row_icon(&self, cx: &App) -> IconName {
        match &self.kind {
            SessionKind::Term { active, .. } => match active.read(cx).launch_kind() {
                terminal_view::LaunchKind::Claude => IconName::Asterisk,
                terminal_view::LaunchKind::Codex => IconName::Bot,
                terminal_view::LaunchKind::Copilot => IconName::Github,
                terminal_view::LaunchKind::Grok => IconName::Bot,
                terminal_view::LaunchKind::Terminal => IconName::SquareTerminal,
            },
            SessionKind::Acp(_) => IconName::Bot,
        }
    }

    /// 会话标题：用户重命名过就用那个；否则仅当终端标题是 Claude Code 风格（✳ 或
    /// Braille spinner 开头）时取它的任务名，再否则回退 cwd 末段——避免把普通 shell 的
    /// user@host:path 标题当任务名。
    fn title(&self, cx: &App) -> String {
        self.custom_title
            .clone()
            .unwrap_or_else(|| match &self.kind {
                SessionKind::Term { active, .. } => pane_auto_title(active, cx),
                SessionKind::Acp(view) => {
                    if view.read(cx).agent_kind() == settings::AcpAgentKind::Codex
                        && let Some(title) = view.read(cx).auto_title()
                    {
                        return title;
                    }
                    let dir = view
                        .read(cx)
                        .cwd()
                        .map(|c| c.rsplit('/').next().unwrap_or(&c).to_string());
                    let agent = view.read(cx).agent_kind().short_label();
                    match dir {
                        Some(d) if !d.is_empty() => format!("{agent} 对话 · {d}"),
                        _ => format!("{agent} 对话"),
                    }
                }
            })
    }

    /// 会话工作目录：活动终端的 cwd（侧栏分组用）。
    fn cwd(&self, cx: &App) -> Option<String> {
        match &self.kind {
            SessionKind::Term { active, .. } => active.read(cx).cwd(),
            SessionKind::Acp(view) => view.read(cx).cwd(),
        }
    }

    /// 会话内 pane 数（判断 Cmd+W 是关 pane 还是关整会话）。
    fn pane_count(&self) -> usize {
        match &self.kind {
            SessionKind::Term { .. } => self.term_leaves().len(),
            SessionKind::Acp(_) => 1,
        }
    }

    /// 会话状态：等审批 > 需要处理 > 运行中 > 刚完成未读 > 空闲（遍历全部 pane 取最高）。
    fn status(&self, cx: &App) -> AgentStatus {
        let active = match &self.kind {
            SessionKind::Term { active, .. } => active,
            // ACP 会话：相位是协议事实，直接问视图，不经推断链。
            SessionKind::Acp(view) => {
                let v = view.read(cx);
                if v.is_awaiting_approval() {
                    return AgentStatus::WaitingApproval;
                }
                if v.is_awaiting_choice() {
                    return AgentStatus::NeedsAttention;
                }
                if v.is_running() {
                    return AgentStatus::Running;
                }
                if v.completed_unread(cx) {
                    return AgentStatus::Done;
                }
                return AgentStatus::Idle;
            }
        };
        let v = self.term_leaves();
        let mut needs_attention = false;
        let mut running = false;
        let mut done = false;
        for t in &v {
            let daemon_state = daemon_state_for(t, cx);
            if let Some(state) = daemon_state.as_ref() {
                match state
                    .structured_events
                    .then(|| daemon_agent_status(t.read(cx).session_id(), state, cx))
                    .flatten()
                {
                    Some(AgentStatus::WaitingApproval) => return AgentStatus::WaitingApproval,
                    Some(AgentStatus::NeedsAttention) => needs_attention = true,
                    Some(AgentStatus::Running) => running = true,
                    Some(AgentStatus::Done) => done = true,
                    Some(AgentStatus::Idle) | None => {}
                }
            }
        }
        if needs_attention {
            return AgentStatus::NeedsAttention;
        }
        if running {
            return AgentStatus::Running;
        }
        if done {
            return AgentStatus::Done;
        }
        // Codex OSC 9 fallback 的 Stop 必须压过可能长期不复位的 spinner 标题。
        if v.iter().any(|t| t.read(cx).completed_unread(cx)) {
            return AgentStatus::Done;
        }
        // 没有结构化运行事实时才退化到标题 spinner。
        if !daemon_state_for(active, cx).is_some_and(|state| state.structured_events)
            && let Some(raw) = active.read(cx).agent_title()
        {
            if crate::osc::title_starts_with_spinner(raw.trim_start()) {
                return AgentStatus::Running;
            }
        }
        AgentStatus::Idle
    }
}

fn next_session_ui_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod session_route_tests {
    use super::{
        DrawerTab, GitTab, MainView, SessionRouteArchive, SessionUiState, WorkspaceRoute,
        inspector, swap_right_route,
    };

    #[test]
    fn opaque_route_swap_restores_layout_and_open_places() {
        let mut active = SessionUiState {
            stage_override: Some(MainView::Git),
            inspector_tab: inspector::InspectorTab::Git,
            inspector_open: false,
            inspector_w: 512.0,
            bottom_drawer_open: true,
            bottom_drawer_h: 318.0,
            git_tab: GitTab::Log,
            ..Default::default()
        };
        let mut parked = SessionUiState {
            stage_override: Some(MainView::Files),
            inspector_tab: inspector::InspectorTab::Files,
            inspector_w: 296.0,
            bottom_drawer_h: 180.0,
            ..Default::default()
        };

        swap_right_route(&mut active, &mut parked);

        assert!(matches!(active.stage_override, Some(MainView::Files)));
        assert_eq!(active.inspector_w, 296.0);
        assert_eq!(active.bottom_drawer_h, 180.0);
        assert!(matches!(parked.stage_override, Some(MainView::Git)));
        assert_eq!(parked.inspector_w, 512.0);
        assert!(parked.bottom_drawer_open);
        assert!(matches!(parked.git_tab, GitTab::Log));
    }

    #[test]
    fn route_archive_roundtrips_without_workspace_knowing_its_fields() {
        let route = SessionUiState {
            stage_override: Some(MainView::Git),
            inspector_tab: inspector::InspectorTab::Skills,
            inspector_w: 488.0,
            bottom_drawer_open: true,
            bottom_drawer_tabs: vec![DrawerTab {
                id: 7,
                terminal: None,
                spawning: false,
            }],
            bottom_drawer_h: 336.0,
            file_tree_selected: Some("/tmp/project/src/main.rs".into()),
            file_tree_w: 312.0,
            git_tab: GitTab::Log,
            ..Default::default()
        };

        let json = serde_json::to_string(&route.archive()).unwrap();
        let archive: SessionRouteArchive = serde_json::from_str(&json).unwrap();
        let restored = SessionUiState::restore(archive);

        assert!(matches!(restored.stage_override, Some(MainView::Git)));
        assert!(matches!(
            restored.inspector_tab,
            inspector::InspectorTab::Skills
        ));
        assert_eq!(restored.inspector_w, 488.0);
        assert_eq!(restored.bottom_drawer_tabs.len(), 1);
        assert_eq!(restored.bottom_drawer_h, 336.0);
        assert_eq!(restored.file_tree_w, 312.0);
        assert_eq!(
            restored.file_tree_selected.as_deref(),
            Some("/tmp/project/src/main.rs")
        );
        assert!(matches!(restored.git_tab, GitTab::Log));
    }

    #[test]
    fn removed_hotspot_tab_migrates_to_changes() {
        let archive: SessionRouteArchive = serde_json::from_value(serde_json::json!({
            "version": 1,
            "git_tab": "hotspot"
        }))
        .unwrap();

        assert!(matches!(archive.git_tab, GitTab::Changes));
    }

    #[test]
    fn legacy_file_and_git_drawer_tabs_are_discarded() {
        let archive: SessionRouteArchive = serde_json::from_value(serde_json::json!({
            "version": 1,
            "bottom_drawer_open": true,
            "bottom_drawer_tabs": ["files", "terminal", "git"]
        }))
        .unwrap();
        let restored = SessionUiState::restore(archive);

        assert_eq!(restored.bottom_drawer_tabs.len(), 1);
        assert!(restored.bottom_drawer_tabs[0].terminal.is_none());
    }

    #[test]
    fn tasks_page_is_not_owned_by_a_session_route() {
        let route = SessionUiState {
            stage_override: Some(MainView::Tasks),
            ..Default::default()
        };
        assert!(route.archive().stage_override.is_none());

        let restored = SessionUiState::restore(SessionRouteArchive {
            stage_override: Some(MainView::Tasks),
            ..Default::default()
        });
        assert!(restored.stage_override.is_none());
    }

    #[test]
    fn tasks_and_session_are_independent_top_level_routes() {
        assert_eq!(
            serde_json::to_string(&WorkspaceRoute::Tasks).unwrap(),
            "\"tasks\""
        );
        assert_eq!(
            serde_json::from_str::<WorkspaceRoute>("\"session\"").unwrap(),
            WorkspaceRoute::Session
        );
    }
}

/// 设置窗口 pages 列表里的页下标——调整 `render_settings_content` 末尾那个
/// `pages(vec![...])` 的顺序时必须同步改这里，否则应用菜单「检查更新…」会跳错页。
const SETTINGS_PAGE_APPEARANCE: usize = 0;
// appearance / 桌面宠物 / 启动 / Agent 集成 / 更新 / 远程
const SETTINGS_PAGE_UPDATE: usize = 4;

/// 重命名弹窗改的是谁：侧栏会话行改整个会话的名，分屏子行只改那一个 pane 的名。
#[derive(Clone)]
enum RenameTarget {
    Session(usize),
    Pane(Entity<TerminalView>),
}

/// 单个终端 pane 自动推导的标题：优先 agent 上报的任务名，其次快捷启动显示名，
/// 再回退建终端时的 cwd 名。不看用户改的名字——`Session::title` 靠它拿活动 pane
/// 的「客观」标题。
fn pane_auto_title(view: &Entity<TerminalView>, cx: &App) -> String {
    let t = view.read(cx);
    if let Some(raw) = t.agent_title() {
        let head = raw.trim_start();
        let is_agent = head.starts_with('✳') || crate::osc::title_starts_with_spinner(head);
        if is_agent {
            let task = strip_status(&raw);
            // agent 默认标题（"Claude Code" / "claude"）不算任务名，继续往下回退。
            if !task.is_empty() && task != "Claude Code" && task != "claude" {
                // 也别跟启动项显示名撞车（例如菜单叫 Claude Code，agent 也只报这个）。
                if t.launch_label().is_none_or(|l| l != task) {
                    return task;
                }
            }
        }
    }
    if let Some(label) = t.launch_label() {
        return label.to_string();
    }
    t.title().to_string()
}

/// 侧栏分屏子行显示的 pane 标题：用户改过名就用改的，否则走自动推导。
///
/// 跟 `pane_auto_title` 分开是有意的：`Session::title` 拿的是活动 pane 的自动标题，
/// 若这里的自定义名漏进去，给活动 pane 改名会连带改掉侧栏父行（会话名），切换
/// 活动 pane 后父行又跳回来——会话名和 pane 名得各归各的。
fn pane_title(view: &Entity<TerminalView>, cx: &App) -> String {
    view.read(cx)
        .custom_title()
        .map(str::to_string)
        .unwrap_or_else(|| pane_auto_title(view, cx))
}

/// 单个终端 pane 的状态：逻辑同 Session::status，但只看这一个 pane 自己
/// （Session::status 是取会话内所有 pane 的最高态）。
fn pane_status(view: &Entity<TerminalView>, cx: &App) -> AgentStatus {
    let daemon_state = daemon_state_for(view, cx);
    if let Some(state) = &daemon_state
        && state.structured_events
    {
        if let Some(status) = daemon_agent_status(view.read(cx).session_id(), state, cx) {
            return status;
        }
    }
    let t = view.read(cx);
    if t.completed_unread(cx) {
        return AgentStatus::Done;
    }
    if !daemon_state.is_some_and(|state| state.structured_events)
        && let Some(raw) = t.agent_title()
    {
        if crate::osc::title_starts_with_spinner(raw.trim_start()) {
            return AgentStatus::Running;
        }
    }
    AgentStatus::Idle
}

/// 去掉 agent 标题开头的状态符号（✳ / Braille spinner ⠂⠐ 等）+ 空白，保留任务名。
fn strip_status(title: &str) -> String {
    title
        .trim_start_matches(|c: char| {
            c.is_whitespace()
                || c == '✳'
                || c == '·'
                || c == '*'
                || ('\u{2800}'..='\u{28FF}').contains(&c) // Braille 盲文块（spinner 动画帧）
        })
        .trim()
        .to_string()
}

/// 收集布局树里所有叶子终端（clone 句柄，顺序 = 深度优先遍历序）。
fn collect_leaves(pane: &Pane, out: &mut Vec<Entity<TerminalView>>) {
    match pane {
        Pane::Leaf(t) => out.push(t.clone()),
        Pane::Split { children, .. } => {
            for c in children {
                collect_leaves(c, out);
            }
        }
    }
}

/// 在布局树里找到 target 终端所在叶子，就地替换成「原叶子 + 新叶子」的二分 Split。
/// 找到并替换返回 true；未命中返回 false。
fn split_leaf(
    pane: &mut Pane,
    target: EntityId,
    axis: Axis,
    state: Entity<ResizableState>,
    new_leaf: Entity<TerminalView>,
) -> bool {
    match pane {
        Pane::Leaf(t) if t.entity_id() == target => {
            let old = Pane::Leaf(t.clone());
            *pane = Pane::Split {
                axis,
                state,
                children: vec![old, Pane::Leaf(new_leaf)],
                // 新拆出来的分屏没有历史尺寸，均分。
                init_sizes: Vec::new(),
            };
            true
        }
        Pane::Leaf(_) => false,
        Pane::Split { children, .. } => children
            .iter_mut()
            .any(|c| split_leaf(c, target, axis, state.clone(), new_leaf.clone())),
    }
}

/// 从布局树移除 target 终端的叶子；某 Split 移除后只剩一个子节点则塌缩掉这层。
fn remove_leaf(pane: &mut Pane, target: EntityId) {
    if let Pane::Split { children, .. } = pane {
        if let Some(pos) = children
            .iter()
            .position(|c| matches!(c, Pane::Leaf(t) if t.entity_id() == target))
        {
            children.remove(pos);
        } else {
            for c in children.iter_mut() {
                remove_leaf(c, target);
            }
        }
        if children.len() == 1 {
            *pane = children.remove(0);
        }
    }
}

/// 工作台的持久化状态：主区分屏布局树 + 活动叶子 + 侧栏宽度。
/// 存 ~/.smelt/workspace.json，启动时据此重建分屏（结构 / 嵌套 / 方向完整恢复）。
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct WsState {
    /// 所有会话（每个 = 一棵分屏树 + 会话内活动叶子遍历序）。
    #[serde(default)]
    sessions: Vec<SessionState>,
    /// 已打开的项目根目录（有序）。独立于会话存在，见 Workspace::projects。
    /// 旧存档没有这个字段 → 启动时从各会话 cwd 反推一份（见 Workspace::new）。
    #[serde(default)]
    projects: Vec<String>,
    /// PC 侧栏的跨端纯数据投影。移动端直接消费并过滤 ACP，不再重新推导菜单。
    #[serde(default)]
    menu: smelt_core::workspace_menu::WorkspaceMenuSnapshot,
    /// 当前活动会话索引。
    #[serde(default)]
    active_session: usize,
    /// 当前一级路由；旧存档默认回到 session。
    #[serde(default)]
    route: WorkspaceRoute,
    /// 会话侧栏拖出的宽度（px）；None = 用默认值。
    #[serde(default)]
    sidebar_w: Option<f32>,
    /// 会话侧栏上次是否展开；None（旧存档）= 默认展开。
    #[serde(default)]
    sidebar_open: Option<bool>,
    /// 右侧 inspector 面板拖出的宽度（px）；None = 用默认值。
    #[serde(default)]
    inspector_w: Option<f32>,
    /// 右侧 inspector 面板上次是否展开；None（旧存档）= 默认展开。
    #[serde(default)]
    inspector_open: Option<bool>,
    /// 文件树列拖出的宽度（px）；None = 用默认值。
    #[serde(default)]
    file_tree_w: Option<f32>,
    /// 文件树里额外 pin 进来的项目根（除当前活动项目外）；空 = 只看当前项目。
    #[serde(default)]
    pinned_file_tree_roots: Vec<String>,
    /// 文件树里被折叠起来的项目根（多根时）。
    #[serde(default)]
    collapsed_file_tree_roots: Vec<String>,
    /// 会话侧栏里被折叠起来的项目根。
    #[serde(default)]
    collapsed_projects: Vec<String>,
    /// 会话侧栏的分组方式；旧存档默认按项目。
    #[serde(default)]
    sidebar_grouping: SidebarGrouping,
    // --- 以下为旧存档兼容字段（读到就迁移，不再写出）---
    /// 旧格式：单棵分屏树。
    #[serde(default)]
    layout: Option<PaneState>,
    /// 更旧格式：终端 cwd 列表（每个迁移成一个独立会话）。
    #[serde(default)]
    tabs: Vec<Option<String>>,
    /// 旧格式的活动索引。
    #[serde(default)]
    active: usize,
}

/// 会话侧栏的组织方式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SidebarGrouping {
    None,
    Status,
    #[default]
    Project,
}

/// 单个会话的持久化镜像：分屏树 + 会话内活动叶子（遍历序）+ 用户重命名过的会话名。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct SessionState {
    layout: PaneState,
    active: usize,
    #[serde(default)]
    custom_title: Option<String>,
    /// Some = ACP 消息流会话（layout 只是占位叶子，旧版 smelt 读到会降级开普通
    /// 终端，不炸档）。恢复时建占位视图（会话进程不持久化，见方案「已知不做」）。
    #[serde(default)]
    acp: Option<AcpSaved>,
    /// 右侧 route 自己拥有的跨进程存档；旧 workspace 没有时沿用全局默认布局。
    #[serde(default)]
    route: Option<SessionRouteArchive>,
}

/// ACP 会话的存档元数据。agent session store 是消息历史的唯一持久化来源；Smelt
/// 只保存重新 load 所需的身份和启动信息。`entries` 仅用于读取旧版 workspace.json，
/// 新存档不再写出，避免和 agent transcript 形成两个数据源。
#[derive(Clone, serde::Serialize)]
struct AcpSaved {
    cwd: Option<String>,
    launch: smelt_core::agent_kind::AcpLaunchSpec,
    #[serde(default)]
    profile_id: Option<String>,
    /// agent 种类标识（`AcpAgentKind::id()`）。旧存档没有这个字段 → None，恢复时
    /// 按 launch 里的命令反推，反推不出就当 Claude（多 agent 之前只可能是它）。
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    history_session_id: Option<agent_client_protocol::schema::v1::SessionId>,
    /// smeltd 托管用的会话 id（`AcpView::session_id()`）。旧存档没有这个字段
    /// → None，恢复时退化成生成一个新 id——意味着即便 smeltd 里那个会话还
    /// 活着，GUI 重开后也接不上、只能按 history_session_id 重新 spawn 一次
    /// （旧版反正每次都是重新 spawn，行为不会比以前差，只是错过了"廉价
    /// attach"这个新能力）。有这个字段才能真正让 GUI 重开秒接上 smeltd 里
    /// 还在跑的会话，见 `acp_view::AcpView::placeholder` 的 `saved_sid` 参数。
    #[serde(default)]
    sid: Option<String>,
    /// 兼容标记：普通无 profile 会话重启时按当前设置刷新；只带旧 `cmd` 的历史
    /// 存档则保留原 launch，避免把旧 profile 覆盖掉。旧存档无此字段 → false。
    #[serde(default)]
    refresh_launch_from_settings: bool,
    #[serde(default)]
    fork_origin: Option<acp_view::AcpForkOrigin>,
}

#[derive(serde::Deserialize)]
struct AcpSavedWire {
    cwd: Option<String>,
    #[serde(default)]
    launch: Option<smelt_core::agent_kind::AcpLaunchSpec>,
    #[serde(default)]
    cmd: Option<String>,
    #[serde(default)]
    profile_id: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    /// 旧版曾把完整消息历史写进 workspace.json。只消费字段保证迁移可读，值不再
    /// 进入 `AcpSaved`，更不会在下一次保存时写回。
    #[serde(default, rename = "entries")]
    _legacy_entries: Option<serde::de::IgnoredAny>,
    #[serde(default, alias = "resume_session_id")]
    history_session_id: Option<agent_client_protocol::schema::v1::SessionId>,
    #[serde(default)]
    sid: Option<String>,
    #[serde(default)]
    refresh_launch_from_settings: Option<bool>,
    #[serde(default)]
    fork_origin: Option<acp_view::AcpForkOrigin>,
}

impl<'de> serde::Deserialize<'de> for AcpSaved {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = <AcpSavedWire as serde::Deserialize>::deserialize(deserializer)?;
        let refresh_launch_from_settings = wire
            .refresh_launch_from_settings
            .unwrap_or_else(|| wire.launch.is_some() && wire.profile_id.is_none());
        let launch = wire.launch.unwrap_or_else(|| {
            smelt_core::agent_kind::AcpLaunchSpec::from_command(wire.cmd.unwrap_or_default())
        });
        Ok(Self {
            cwd: wire.cwd,
            launch,
            profile_id: wire.profile_id,
            agent: wire.agent,
            history_session_id: wire.history_session_id,
            sid: wire.sid,
            refresh_launch_from_settings,
            fork_origin: wire.fork_origin,
        })
    }
}

impl AcpSaved {
    fn refresh_launch_from_settings(&self) -> bool {
        self.refresh_launch_from_settings
    }
}

/// 可序列化的分屏布局镜像：叶子存该终端 cwd + 守护会话 id，Split 存方向 + 子节点 +
/// 各子块尺寸。结构 / 嵌套 / 方向 / 拖出来的比例都完整恢复。
/// id 用于重开 GUI 时 reattach smeltd 里还活着的会话（旧存档无 id → 开新会话）。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
enum PaneState {
    Leaf {
        cwd: Option<String>,
        #[serde(default)]
        id: Option<String>,
        /// 用户给这个 pane 起的名字。旧存档没有这个字段 → None，行为不变。
        #[serde(default)]
        custom_title: Option<String>,
        /// 快捷启动项显示名。旧存档没有 → None，回退 cwd 末段。
        #[serde(default)]
        launch_label: Option<String>,
        /// 快捷启动实际命令行（硬重启守护 / 冷启动新建时用来重跑 agent）。
        /// 旧存档没有 → None，只开裸 shell。
        #[serde(default)]
        launch_cmd: Option<String>,
    },
    Split {
        axis: SplitAxis,
        children: Vec<PaneState>,
        /// 各子块的像素尺寸 —— 存盘那一刻 ResizableState 里的真实值（含用户拖拽结果）。
        /// 旧存档没有这个字段 → 空 vec → 按均分，跟以前行为一致。
        #[serde(default)]
        sizes: Vec<f32>,
    },
}

/// 侧栏的一个项目分组。**身份是 root（路径），不是 label**——两个不同目录的末段名
/// 可能一模一样（`~/a/smelt` 和 `~/b/smelt`），拿显示名当 key 会把它们认成同一个项目：
/// 第二个连行都不显示、会话挂错组、关一个连带关掉另一个的会话。
pub(crate) struct ProjectGroup {
    /// 项目根目录：唯一标识。active_project / collapsed_projects / close_project 全用它。
    pub root: String,
    /// 侧栏显示名。末段重名时往前补父目录段区分（`a · smelt` / `b · smelt`）。
    pub label: String,
    /// 组内会话在 `sessions` 里的下标（顺序 = 侧栏显示顺序）。
    pub sessions: Vec<usize>,
}

pub(crate) fn sidebar_groups(
    grouping: SidebarGrouping,
    project_groups: Vec<ProjectGroup>,
    statuses: &[AgentStatus],
    session_count: usize,
) -> Vec<ProjectGroup> {
    match grouping {
        SidebarGrouping::Project => project_groups,
        SidebarGrouping::Status => [
            (AgentStatus::WaitingApproval, "等你批准"),
            (AgentStatus::NeedsAttention, "需要处理"),
            (AgentStatus::Running, "运行中"),
            (AgentStatus::Done, "已完成"),
            (AgentStatus::Idle, "空闲"),
        ]
        .into_iter()
        .filter_map(|(status, label)| {
            let sessions = statuses
                .iter()
                .enumerate()
                .filter_map(|(ix, value)| (*value == status).then_some(ix))
                .collect::<Vec<_>>();
            (!sessions.is_empty()).then(|| ProjectGroup {
                root: format!("__status_{}", status.rank()),
                label: label.to_string(),
                sessions,
            })
        })
        .collect(),
        SidebarGrouping::None => vec![ProjectGroup {
            root: "__all_sessions".into(),
            label: String::new(),
            sessions: (0..session_count).collect(),
        }],
    }
}

/// 显示名撞车时往前补 `extra` 段父目录：`/a/b/smelt` + 1 → `b · smelt`。
/// base 是这一组本来的显示名（worktree 是「仓库 · 分支」，普通项目是目录末段）。
fn label_with_parents(root: &str, base: &str, extra: usize) -> String {
    if extra == 0 {
        return base.to_string();
    }
    let segs: Vec<&str> = root
        .trim_end_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let end = segs.len().saturating_sub(1); // base 已经代表末段
    let start = end.saturating_sub(extra);
    if start >= end {
        return base.to_string();
    }
    format!("{} · {}", segs[start..end].join("/"), base)
}

/// 给重名的分组逐步补父目录段，直到互不相同（或路径已经补到顶）。
fn disambiguate_labels(groups: &mut [ProjectGroup], bases: &[String]) {
    let mut extra = vec![0usize; groups.len()];
    // 每轮给所有重名组多补一段；路径最深也就那么几段，8 轮足够收敛。
    for _ in 0..8 {
        let mut dup: Vec<usize> = Vec::new();
        for i in 0..groups.len() {
            if groups
                .iter()
                .enumerate()
                .any(|(j, g)| j != i && g.label == groups[i].label)
            {
                dup.push(i);
            }
        }
        if dup.is_empty() {
            return;
        }
        let mut changed = false;
        for i in dup {
            let next = extra[i] + 1;
            let candidate = label_with_parents(&groups[i].root, &bases[i], next);
            if candidate != groups[i].label {
                extra[i] = next;
                groups[i].label = candidate;
                changed = true;
            }
        }
        // 全都补到路径顶了还重名（真同路径 / 只剩根）→ 认命，别空转。
        if !changed {
            return;
        }
    }
}

/// cwd 归属哪个项目根：cwd 就是根、或落在根之下（必须是完整路径段，`/a/bc` 不算
/// 落在 `/a/b` 下）。多个根都匹配时取最深的那个——`~/dev/a` 和 `~/dev/a/sub` 都打开
/// 过时，子目录的会话归后者。空 cwd 或谁都不沾 → None（调用方按 cwd 自建隐式组）。
fn project_root_of(projects: &[String], cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    let cwd = cwd.trim_end_matches('/');
    projects
        .iter()
        .map(|p| p.trim_end_matches('/'))
        .filter(|root| !root.is_empty() && (cwd == *root || cwd.starts_with(&format!("{root}/"))))
        .max_by_key(|root| root.len())
        .map(str::to_string)
}

fn remove_projects_under(projects: &mut Vec<String>, path: &str) {
    let path = path.trim_end_matches('/');
    let prefix = format!("{path}/");
    projects.retain(|project| {
        let project = project.trim_end_matches('/');
        project != path && !project.starts_with(&prefix)
    });
}

/// 存档里一个会话的代表 cwd：ACP 取自身 cwd，终端取分屏树里第一个有 cwd 的叶子。
/// 旧存档迁移（反推项目列表）用。
fn session_state_cwd(s: &SessionState) -> Option<String> {
    if let Some(acp) = &s.acp {
        return acp.cwd.clone();
    }
    fn first_cwd(p: &PaneState) -> Option<String> {
        match p {
            PaneState::Leaf { cwd, .. } => cwd.clone(),
            PaneState::Split { children, .. } => children.iter().find_map(first_cwd),
        }
    }
    first_cwd(&s.layout)
}

fn split_restore_queue(
    pending: Vec<SessionState>,
) -> (Vec<(usize, SessionState)>, Vec<(usize, SessionState)>) {
    pending
        .into_iter()
        .enumerate()
        .partition(|(_, session)| session.acp.is_some())
}

fn restored_insert_position(restored_indices: &[usize], original_index: usize) -> usize {
    restored_indices.partition_point(|index| *index < original_index)
}

fn planned_restore_insert_position(
    restored_indices: &[usize],
    original_index: usize,
    live_session_count: usize,
) -> Option<usize> {
    (restored_indices.len() == live_session_count)
        .then(|| restored_insert_position(restored_indices, original_index))
}

fn record_restored_index(
    restored_indices: &mut Vec<usize>,
    insert_at: usize,
    original_index: usize,
    restore_order_intact: bool,
) {
    if restore_order_intact {
        restored_indices.insert(insert_at, original_index);
    }
}

fn restored_active_position(restored_indices: &[usize], saved_active: usize) -> usize {
    if restored_indices.is_empty() {
        return 0;
    }
    restored_indices
        .binary_search(&saved_active)
        .unwrap_or_else(|position| position.min(restored_indices.len() - 1))
}

fn merge_restore_orphans(
    mut sessions: Vec<SessionState>,
    orphans: &[(usize, SessionState)],
) -> Vec<SessionState> {
    let mut orphans = orphans.to_vec();
    orphans.sort_by_key(|(index, _)| *index);
    for (index, session) in orphans {
        sessions.insert(index.min(sessions.len()), session);
    }
    sessions
}

fn persisted_active_position(
    active_session: usize,
    orphans: &[(usize, SessionState)],
    sessions_restored: bool,
) -> usize {
    if !sessions_restored {
        return active_session;
    }
    let mut position = active_session;
    let mut orphan_indices = orphans.iter().map(|(index, _)| *index).collect::<Vec<_>>();
    orphan_indices.sort_unstable();
    for index in orphan_indices {
        if index <= position {
            position += 1;
        }
    }
    position
}

fn should_auto_resume_active_acp(sessions_restored: bool) -> bool {
    sessions_restored
}

fn should_restore_saved_active(
    restore_order_intact: bool,
    current_list_revision: u64,
    restore_list_revision: u64,
    current_active_revision: u64,
    restore_active_revision: u64,
) -> bool {
    restore_order_intact
        && current_list_revision == restore_list_revision
        && current_active_revision == restore_active_revision
}

fn restore_path_is_cancelled(cwd: Option<&str>, cancelled_paths: &[String]) -> bool {
    let Some(cwd) = cwd.map(str::trim).filter(|cwd| !cwd.is_empty()) else {
        return false;
    };
    let cwd = cwd.trim_end_matches('/');
    cancelled_paths.iter().any(|path| {
        let path = path.trim_end_matches('/');
        cwd == path || cwd.starts_with(&format!("{path}/"))
    })
}

/// 新会话 id（uuid v4）：GUI 与 smeltd 之间的持久身份。
fn new_sid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Split 方向的可序列化镜像（gpui::Axis 无法直接序列化）。
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy)]
enum SplitAxis {
    H,
    V,
}

impl From<Axis> for SplitAxis {
    fn from(a: Axis) -> Self {
        if matches!(a, Axis::Horizontal) {
            SplitAxis::H
        } else {
            SplitAxis::V
        }
    }
}

impl From<SplitAxis> for Axis {
    fn from(a: SplitAxis) -> Self {
        match a {
            SplitAxis::H => Axis::Horizontal,
            SplitAxis::V => Axis::Vertical,
        }
    }
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

/// 后台线程里已经 spawn/reattach 好的叶子终端（尚未挂 GPUI Entity）。
struct SpawnedLeaf {
    terminal: terminal::Terminal,
    sid: String,
    cwd: Option<String>,
    launch: Option<String>,
    label: Option<String>,
    custom_title: Option<String>,
}

/// 阻塞：按 DFS 顺序 spawn 一棵布局树的全部叶子（**只**在后台线程调用）。
fn spawn_layout_leaves(ps: &PaneState) -> Result<Vec<SpawnedLeaf>, String> {
    let mut out = Vec::new();
    spawn_layout_leaves_rec(ps, &mut out)?;
    Ok(out)
}

fn spawn_layout_leaves_rec(ps: &PaneState, out: &mut Vec<SpawnedLeaf>) -> Result<(), String> {
    match ps {
        PaneState::Leaf {
            cwd,
            id,
            custom_title,
            launch_label,
            launch_cmd,
        } => {
            let sid = id.clone().unwrap_or_else(new_sid);
            let terminal =
                terminal::Terminal::spawn(24, 80, cwd.as_deref(), &sid, None).map_err(|e| {
                    eprintln!("[workspace] 恢复会话 {sid}（{cwd:?}）失败：{e:#}");
                    e.to_string()
                })?;
            out.push(SpawnedLeaf {
                terminal,
                sid,
                cwd: cwd.clone(),
                launch: launch_cmd.clone(),
                label: launch_label.clone(),
                custom_title: custom_title.clone(),
            });
            Ok(())
        }
        PaneState::Split { children, .. } => {
            for c in children {
                spawn_layout_leaves_rec(c, out)?;
            }
            Ok(())
        }
    }
}

/// 用已 spawn 的叶子（DFS 序）重建布局树；**只**在 UI 线程建 Entity。
fn rebuild_pane_ready(
    ps: &PaneState,
    leaves: &mut std::vec::IntoIter<SpawnedLeaf>,
    tabs: &mut Vec<Entity<TerminalView>>,
    cx: &mut Context<Workspace>,
) -> Option<Pane> {
    match ps {
        PaneState::Leaf { .. } => {
            let leaf = leaves.next()?;
            let v = cx.new(|cx| {
                let mut view = TerminalView::from_terminal(
                    cx,
                    leaf.terminal,
                    leaf.cwd,
                    leaf.sid,
                    leaf.launch.as_deref(),
                    leaf.label.as_deref(),
                );
                view.set_custom_title(leaf.custom_title);
                view
            });
            tabs.push(v.clone());
            Some(Pane::Leaf(v))
        }
        PaneState::Split {
            axis,
            children,
            sizes,
        } => {
            let mut kept: Vec<Pane> = children
                .iter()
                .filter_map(|c| rebuild_pane_ready(c, leaves, tabs, cx))
                .collect();
            match kept.len() {
                0 => None,
                1 => Some(kept.remove(0)),
                _ => Some(Pane::Split {
                    axis: (*axis).into(),
                    state: cx.new(|_| ResizableState::default()),
                    // 有子块没起来被丢掉时尺寸就对不上号了，宁可整组均分也不能错位——
                    // 错位会把 A 的宽度套到 B 头上，比均分更糟。
                    init_sizes: if kept.len() == children.len() && sizes.len() == kept.len() {
                        sizes.clone()
                    } else {
                        Vec::new()
                    },
                    children: kept,
                }),
            }
        }
    }
}

/// 把存档里的会话列表规范成 `Vec<SessionState>`（兼容旧 layout / tabs 字段）。
fn normalize_saved_sessions(s: &WsState) -> (Vec<SessionState>, usize) {
    if !s.sessions.is_empty() {
        return (s.sessions.clone(), s.active_session);
    }
    if let Some(ps) = &s.layout {
        return (
            vec![SessionState {
                layout: ps.clone(),
                active: s.active,
                custom_title: None,
                acp: None,
                route: None,
            }],
            0,
        );
    }
    let sessions: Vec<SessionState> = s
        .tabs
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
            custom_title: None,
            acp: None,
            route: None,
        })
        .collect();
    (sessions, s.active)
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
fn ws_state_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".smelt").join("workspace.json"))
}

fn load_ws_state_from_path(path: &std::path::Path) -> Option<WsState> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// 读取存档；文件不存在/损坏都返回 None，交由调用方回退默认。
fn load_ws_state() -> Option<WsState> {
    load_ws_state_from_path(&ws_state_path()?)
}

/// 工作台根视图：多标签终端管理器。
struct Workspace {
    /// 所有会话；每个会话 = 一棵独立分屏树 + 会话内活动 pane。
    sessions: Vec<Session>,
    /// 当前活动会话索引（主区显示它、侧栏高亮它）。
    active_session: usize,
    /// 当前显示任务 route 还是活动 session route。
    primary_route: WorkspaceRoute,
    /// 当前 Workspace 热字段属于哪个 session。每帧渲染前与 active session 对齐，
    /// 因而所有切换入口（点击、快捷键、任务跳转、恢复）都走同一套快照交换。
    ui_session_id: Option<u64>,
    /// 当前路由。Workspace 只切换这一整个对象，不理解其中有哪些页面或控件。
    right_route: SessionUiState,
    /// Inspector 的挂载与开合过渡。关闭动画结束后才卸载面板。
    inspector_transition: panel_transition::PanelTransition,
    /// 底部抽屉（快捷终端/文件/Git 面板，VS Code 那种从屏幕底边拉出来的面板）
    /// 是否展开。
    /// 底部抽屉的挂高过渡，跟 inspector_transition 同一套组件——关闭动画结束
    /// 后才真正卸载，展开/收起都有高度渐变，不是硬切换。
    bottom_drawer_transition: panel_transition::PanelTransition,
    /// 底部抽屉的标签页（可以同时开好几个：终端/文件/Git 混着开，对标 Codex
    /// 「+」新建菜单），懒创建（第一次展开才补一个默认终端标签）。关闭抽屉只是
    /// 隐藏，终端标签的进程不杀——下次展开还在原地。
    /// 当前高亮的标签页在 bottom_drawer_tabs 里的下标。
    /// 标签页 id 自增计数器（关闭/新建乱序时用来认哪个是哪个，不用下标，下标会变）。
    /// 文件树里已展开的文件夹绝对路径。
    /// 目录列表缓存（绝对路径 → 已排序过滤的直接子项 (名, 是否目录)）。后台读盘填充，
    /// render 只读；此前 file_tree 在 render 里同步 fs::read_dir，大目录会像 git
    /// status 那样掉帧，这里改用同款「后台刷新 + 缓存 + render 只读」模式修复。
    dir_cache: HashMap<String, (Instant, Rc<Vec<(String, bool)>>)>,
    /// 正在后台读取的目录（防重复并发 spawn）。
    dir_inflight: HashSet<String>,
    /// 文件树键盘选中的条目绝对路径（↑↓ 导航用）。
    /// 打开文件后要 reveal 的路径：祖先目录缓存齐了再 scroll_to_item。
    file_tree_pending_reveal: Option<String>,
    /// 当前在文件树里打开查看的文件（含预高亮的行数据）。
    /// ACP 消息图片的窗口级预览。放在 Workspace 而非 AcpView，遮罩才能覆盖侧栏、
    /// inspector 和输入区，且不受会话面板裁剪。
    acp_image_preview: Option<Arc<gpui::Image>>,
    /// 打开文件的自增序号：后台高亮完成时用它判断结果是否已过期（切了别的文件）。
    file_gen: u64,
    /// 当前文件有未保存改动时，用户又点了别的文件——先记下目标路径弹确认弹窗，
    /// 等用户选了"不保存"/"保存并切换"才真正打开，见 render_unsaved_file_confirm。
    pending_file_switch: Option<String>,
    /// 文件树右键「删除文件」的二次确认目标（None = 没在删）。
    delete_file_target: Option<DeleteFileTarget>,
    /// 「保存并切换」选择后，等这次 save_open_file 存盘成功再打开的目标路径；
    /// 存盘失败/冲突则放弃切换，留在当前文件上让用户处理。
    pending_switch_after_save: Option<String>,
    /// Git 视图里当前查看的文件 diff；None 表示未选中任何文件。
    /// 打开 diff 的自增序号（独立于 file_gen，避免和文件高亮任务互相取消）。
    diff_gen: u64,
    /// diff 是否用并排（split）视图；false 为统一（unified）视图。
    /// F7/Shift+F7 当前跳到第几个改动块（None = 还没跳过）。换文件重开 diff 时清空。
    /// Git 页变更文件树里被折叠的目录（存相对仓库根的路径）。默认全展开——改动
    /// 文件通常没几个，一进来就全看见比让人挨个点开更顺手。
    /// diff 看哪一层改动（全部 / 已暂存 / 未暂存）。默认全部，保持既有观感。
    /// 「日志」页（git 提交历史 + 分支图）的全部状态。
    git_log: git_log::GitLogState,
    /// Git 页当前在看哪个子页（改动 / 日志）。
    /// 正在推送（按钮显示「推送中…」并禁用，避免连点推两次）。
    pushing: bool,
    /// 正在确认删除的分支：(仓库根, 分支名, 是否远端分支)。
    delete_branch_target: Option<(String, String, bool)>,
    /// 日志页三栏（分支树 / 提交列表 / 详情）的拖拽状态。窗口窄时靠它腾地方。
    git_log_resize: Entity<ResizableState>,
    /// 工作台外层拖拽状态：会话列表 | 右侧区（舞台+inspector+抽屉）两栏。
    workspace_resize: Entity<ResizableState>,
    /// 舞台 | inspector 的内层拖拽状态——嵌在「右侧区」里，独立于 workspace_resize，
    /// 这样底部抽屉才能包住舞台+inspector 整体，一路铺到窗口最右边。
    stage_inspector_resize: Entity<ResizableState>,
    /// 底部抽屉（快捷终端）跟舞台内容的上下拖拽状态——真正的 resizable 组件，
    /// 不是手写死高度，用户可以拖边框调抽屉高度。不落盘（跟 git_log_resize
    /// 同理，重开就回默认高度，没必要为这个持久化）。
    bottom_drawer_resize: Entity<ResizableState>,
    /// 抽屉当前高度（拖拽后的值，动画展开/收起时按这个目标值渐变）。
    /// 左侧会话栏是否展开，以及对应的挂载过渡。
    sidebar_open: bool,
    sidebar_transition: panel_transition::PanelTransition,
    sidebar_w: f32,
    /// 交互式 diff：选中待评论的行号集合（对应 GitDiff.lines 下标），换文件/重开 diff 时清空。
    /// 交互式 diff 的评论输入框（懒创建，随 Git 视图渲染出待发送的 diff 时创建）。
    diff_comment_input: Option<Entity<gpui_component::input::InputState>>,
    /// Git 视图的 commit message 输入框（懒创建，随 Git 视图首次渲染时创建；跟
    /// diff_comment_input 是两个独立的框，一个针对选中的 diff 行，一个是整体提交信息）。
    commit_msg_input: Option<Entity<gpui_component::input::InputState>>,
    /// 「生成」按钮请求 LLM 生成 commit message 进行中（防连点、按钮显示"生成中…"）。
    commit_msg_generating: bool,
    /// 「关闭项目」二次确认弹窗：(显示名, root 路径, 会连带关掉的会话数)。Some = 弹窗开着。
    /// 显示名只用来写文案，真正关哪个项目认 root。空项目不走确认（无损），会话数恒 > 0。
    close_project_target: Option<(String, String, usize)>,
    /// 已打开的项目根目录（有序，就是侧栏分组的骨架）。项目是**独立于会话**的实体：
    /// 「打开项目」只往这里加一条、不建会话；项目下最后一个会话关掉了，这一条也还在
    /// （侧栏显示 0 个会话的空项目）。要它消失只能显式「关闭项目」。
    /// 会话按 cwd 挂到项目下（见 project_root_for_cwd）；挂不上的（比如 Finder 拖进来
    /// 之前的旧会话）仍按自己的 cwd 自建隐式分组，不污染这份列表。
    projects: Vec<String>,
    /// 活动项目的 **root 路径**（不是显示名——同名目录是两个项目，见 ProjectGroup）：
    /// 会话列表里高亮哪一组、顶栏显示谁、「+对话/+终端」新建到哪个 cwd。None 或该组
    /// 已消失时回退到活动会话所在组（见 active_project_root）。
    active_project: Option<String>,
    /// 会话列表里被折叠起来的项目（存 root 路径，同上）。
    collapsed_projects: HashSet<String>,
    /// 会话列表分组方式（默认按项目）。
    sidebar_grouping: SidebarGrouping,
    /// SKILLS 面板缓存：(取得时刻, 列表) + 扫的是哪个项目 + 是否正在后台扫。
    skills_cache: skills::SkillsCache,
    skills_cache_cwd: Option<String>,
    skills_inflight: bool,
    /// 「新建/编辑 skill」弹窗；`None` = 未打开。
    skill_modal: Option<skills::SkillModalState>,
    /// 「删除 skill」二次确认目标；`None` = 未在删。
    skill_delete_target: Option<skills::SkillEntry>,
    /// 「管理链接 / 应用到其他工具」弹窗；`None` = 未打开。
    skill_link_modal: Option<skills::SkillLinkModalState>,
    /// 命令面板（Cmd+K）；None 表示未打开。搜索/导航/确认由 ListState 负责。
    palette: Option<Entity<ListState<CmdDelegate>>>,
    /// 命令面板的事件订阅（确认/取消）；随面板关闭一并释放。
    _palette_sub: Option<Subscription>,
    /// 各滚动区的常驻滚动句柄——供 gpui-component Scrollbar 读取位置并绘制。
    /// 必须常驻（每帧新建会丢失滚动位置）。
    diff_scroll: UniformListScrollHandle,
    /// 文件树列表的滚动句柄（普通滚动，非虚拟滚动——见 file_tree 函数注释）。
    file_tree_scroll: ScrollHandle,
    /// 文件树列宽拖拽状态（对面板：文件树 + 右侧文件内容）；拖动完通过 save_state
    /// 落盘到 file_tree_w，重启后从存档恢复。
    file_tree_resize: Entity<ResizableState>,
    /// 文件树顶部的过滤输入框；首次渲染文件树时懒创建（需要 window）。
    file_filter: Option<Entity<gpui_component::input::InputState>>,
    /// 过滤框的变更订阅（键入即重渲染）；随视图存活。
    _file_filter_sub: Option<Subscription>,
    /// 总览任务区：标题 / prompt 输入（懒创建）。
    task_title_input: Option<Entity<gpui_component::input::InputState>>,
    task_body_input: Option<Entity<gpui_component::input::InputState>>,
    /// 定时任务：执行时间输入（`YYYY-MM-DD HH:MM`，懒创建）。
    task_run_at_input: Option<Entity<gpui_component::input::InputState>>,
    /// 新建任务类型（普通 / 单次定时）。
    task_kind: tasks::TaskKind,
    /// 新建任务是否允许系统自动执行（任务级 `auto_run`；定时强制 true）。
    task_auto_run: bool,
    /// 任务列表选中项 id。
    task_selected: Option<String>,
    /// 新建任务绑定的项目 cwd。
    task_bind_project: Option<String>,
    /// 新建任务选用的 launch 命令（与设置页启动项 command 对齐）。
    task_bind_launch: Option<String>,
    /// 在已有终端执行：Some(smeltd session id)；None = 新开终端。
    /// 由「终端/会话右键 → 新建任务」写入。
    task_bind_session: Option<String>,
    /// 任务总览状态筛选：None = 全部。
    task_column_filter: Option<tasks::TaskColumn>,
    /// 标题输入的 Enter 订阅（回车 = 创建并开跑）。
    _task_title_sub: Option<Subscription>,
    /// 新建任务弹窗（Cmd+Shift+N / 侧栏「新建任务」）。
    show_new_task_modal: bool,
    /// 弹窗处于「编辑」模式时的任务 id；None = 新建模式。
    task_editing: Option<String>,
    /// 定时任务扫描循环是否已启动（避免 render 重复 spawn）。
    task_schedule_started: bool,
    /// 文件树搜索结果（文件名 + 文件内容）：后台遍历项目填充，render 只读。
    /// query 非空时左栏由树形切换为扁平命中列表。
    search_results: Option<SearchState>,
    /// 搜索任务自增序号：后台遍历完成时用它丢弃过期结果（期间又改了查询）。
    search_gen: u64,
    /// 文件树列初始宽度（px）：启动时从存档恢复，作为 resizable_panel 的初始 size。
    /// 文件树列 resize 事件订阅（拖动完写回存档）；随视图存活。
    _file_tree_resize_sub: Subscription,
    _workspace_resize_sub: Subscription,
    _stage_inspector_resize_sub: Subscription,
    _bottom_drawer_resize_sub: Subscription,
    /// 宠物大脑（LLM）配置的输入框；首次打开设置面板时懒创建（需要 window）。
    llm_inputs: Option<LlmInputs>,
    /// 上面几个输入框的变更订阅（保活；随视图存活）。
    llm_subs: Vec<Subscription>,
    /// 启动项列表编辑器（设置页「启动」分组懒创建）。
    launch_inputs: Option<settings::LaunchInputs>,
    /// 手动添加 workspace 列表编辑器（设置页「Agent 集成」分组懒创建）。
    profile_inputs: Option<settings::ProfileInputs>,
    /// 设置面板的有状态组件（懒创建）：不透明度滑块 + 字体大小滑块 + 背景色 / 宠物色取色器。
    opacity_slider: Option<Entity<SliderState>>,
    font_size_slider: Option<Entity<SliderState>>,
    bg_color_picker: Option<Entity<ColorPickerState>>,
    pet_color_picker: Option<Entity<ColorPickerState>>,
    /// 上面三个组件的变更订阅。
    settings_subs: Vec<Subscription>,
    /// 上次应用到窗口的背景外观：不透明度 / 模糊改了要 window 才能切，故在 render 里同步。
    applied_window_bg: Option<WindowBackgroundAppearance>,
    /// git status 缓存（root → (取得时刻, 数据)）。Git 页后台刷新，render 只读，
    /// 避免每帧同步跑 git status（大仓要 ~90ms，是掉帧元凶）。
    git_status: HashMap<String, (Instant, GitStatusData)>,
    /// 正在后台刷新 status 的 root（防重复并发 spawn）。
    git_status_inflight: HashSet<String>,
    /// 分支列表缓存（root → (取得时刻, 数据)），Git 页头部分支切换下拉用；同
    /// git_status 一套只在 Git 页打开时后台刷新。
    branches: HashMap<String, (Instant, BranchList)>,
    /// 正在后台刷新分支列表的 root（防重复并发 spawn）。
    branches_inflight: HashSet<String>,
    /// 文件监听标脏的 root 集合：notify 的回调跑在独立系统线程上，故用 Arc<Mutex<..>>
    /// 跨线程共享；250ms 检查循环（见 ensure_git_watch）发现命中就清位 + 强制刷新。
    git_dirty: Arc<Mutex<HashSet<String>>>,
    /// 每个 root 常驻的文件监听器（root → watcher）。watcher 必须存活才会继续收事件，
    /// 故存在 Workspace 里跟应用同生命周期；只建一次，见 ensure_git_watch。
    git_watchers: HashMap<String, RecommendedWatcher>,
    /// 每个 root 上次「进 Git 页自动 fetch」的时刻：进 Git 页会主动 fetch 一次刷新
    /// ahead/behind，但 render 每帧都满足「在 Git 页」，靠这个时间戳去抖（同一 root
    /// 60s 内不重复自动 fetch），避免每帧狂发网络请求。
    git_autofetch_at: HashMap<String, Instant>,
    /// 历史会话列表缓存（`"{agent_id}:{cwd}"` → (取得时刻, 数据)）：后台扫描该 agent
    /// 在该项目下的本地存储，render 只读。key 带上 agent_id 是因为四家 agent 的历史
    /// 各存各的，同一个 cwd 换个 tab 就是完全不同的一份数据。
    /// 注意：总览卡片那边（`self.sessions` 渲染，展示"最近一次 Claude 活动"）也复用
    /// 这份缓存，固定传 `AcpAgentKind::Claude`——历史会话页加多 agent tab 不该改变
    /// 那个功能的行为，两处刻意共享同一套读写路径而不是各建一份。
    session_list: HashMap<String, (Instant, Rc<Vec<session_history::SessionSummary>>)>,
    /// 正在后台扫描历史会话列表的 key（同上 `"{agent_id}:{cwd}"`，防重复并发 spawn）。
    session_list_inflight: HashSet<String>,
    /// 当前选中查看的历史会话（路径 + 解析出的对话内容）；None 表示未选。
    session_detail: Option<(PathBuf, Rc<session_history::SessionDetail>)>,
    /// 历史会话右侧消息详情的可变高度虚拟列表状态。
    history_detail_list_state: gpui::ListState,
    /// 加载会话详情的自增序号：后台解析完成时用它判断结果是否已过期（切了别的会话）。
    session_detail_gen: u64,
    /// 历史会话页当前显示的是「会话」还是「记忆」（同一套左列表 + 右详情布局）。
    history_pane: HistoryPane,
    /// 历史会话页「会话」子页当前选中查看哪家 agent 的历史（Claude/Copilot/Codex/
    /// Grok 分 tab，各自存储格式不同，见 session_history.rs 头部注释）。
    history_agent: settings::AcpAgentKind,
    /// 选中的是手动添加的 workspace profile（而不是某个基础 agent 槽位）时是
    /// `Some(profile_id)`；`history_agent` 这时候是该 profile 底层接的种类。
    history_profile: Option<String>,
    /// 记忆列表缓存（cwd → (取得时刻, 数据)），跟 session_list 同一套 TTL 模板。
    memory_list: HashMap<String, (Instant, Rc<Vec<claude_memory::MemoryEntry>>)>,
    /// 正在后台扫描记忆的 cwd（防重复并发 spawn）。
    memory_list_inflight: HashSet<String>,
    /// 当前选中查看的记忆，存在列表里的下标；切项目/切列表时会被清掉。
    memory_selected: Option<usize>,
    /// 调试 HUD 开关（Cmd+Shift+F 切换）：开启时右上角显示帧率 + 帧耗时 + RSS。
    debug_hud: bool,
    /// 上一帧渲染时刻（算帧间隔用）。
    last_frame: Option<Instant>,
    /// 平滑后的帧率（EMA）。
    fps_ema: f32,
    /// 调试 HUD 上次采样的 RSS（字节）；约每秒刷新一次，避免每帧调系统 API。
    debug_mem_rss: Option<u64>,
    /// 调试 HUD 上次内存采样时刻。
    debug_mem_sampled_at: Option<Instant>,
    /// 退出确认拦截弹窗开关
    show_quit_confirm: bool,
    /// 在线更新状态机（检查/下载/暂存就绪），驱动设置页"更新"分区 + 齿轮强调色。
    update_status: updater::UpdateStatus,
    /// 设置窗口打开时要停在第几页（索引对应 `render_settings_content` 里 pages 的顺序）。
    settings_page_ix: usize,
    /// 每请求跳一次页就 +1，用来变更 `Settings` 元素的 id。
    ///
    /// `Settings` 把当前选中页存在 `use_keyed_state` 里，只有该 id 首次出现时才读
    /// `default_selected_index`——窗口已经开着时改字段是不起作用的。把这个自增序号
    /// 编进 id，就能强制它按新的 default 重建一次。不用页号本身当 id：用户手动切走后
    /// 再点同一个入口，页号没变，id 也就没变，照样跳不过去。
    settings_page_nonce: usize,
    /// 设置页「终端字体」下拉的选项，首次渲染时算一次就缓存住。
    ///
    /// `all_font_names()` 在 mac 上枚举的是全部字体 face 的 descriptor（本机 902 个），
    /// 再逐个 CopyAttribute 取 family name，实测约 50ms/次——远超 60fps 的 16.6ms 预算。
    /// 它原先直接写在 `render_settings_content` 里，设置窗口每帧都要重算一遍，下拉一
    /// 展开就肉眼可见掉帧。字体列表在进程生命周期内几乎不变，不值得每帧重扫。
    font_options: std::cell::OnceCell<Vec<(SharedString, SharedString)>>,
    /// 上次同步给 Dock 角标的「需要关注」会话数；None 强制首帧同步一次。
    /// 只在这个数变化时才调用 Cocoa API，避免每次 render 都发一遍。
    dock_badge_count: Option<usize>,
    /// 上次同步给菜单栏下拉菜单的会话快照；None 强制首帧同步一次。只在快照真的变化
    /// 时才重建 AppKit 菜单，避免每次 render 都拆了重建。
    status_menu_snapshot: Option<Vec<status_item::SessionEntry>>,
    /// 会话拖拽悬停中的插入位置：(目标会话, 插它前面?)。由 drop 层的 on_drag_move
    /// 维护，驱动插入指示条的出现动画；起拖时清空，避免上次拖拽的残留闪一帧。
    sess_drop_hint: Option<(EntityId, bool)>,
    /// 项目分组拖拽悬停中的目标项目名，作用同上。
    /// （项目拖拽待接到 rail，暂时闲置；见 ProjectDrag 注释。）
    #[allow(dead_code)]
    proj_drop_hint: Option<SharedString>,
    /// 正在重命名的对象 + 弹窗里的文本框（None = 没在重命名）。见
    /// `start_rename`/`confirm_rename`。
    rename_target: Option<RenameTarget>,
    rename_input: Option<Entity<gpui_component::input::InputState>>,
    /// 重命名文本框的事件订阅句柄，随 rename_input 一起换（回车/失焦提交）。
    _rename_sub: Option<Subscription>,
    /// 仓库身份缓存（cwd → git-dir/common-dir/分支）：判断某个会话是不是 worktree
    /// 检出、侧栏聚簇排序、拼「仓库名 · 分支名」标签都靠它。None = 探测过但不是
    /// git 仓库（比如临时终端落脚的 $HOME），不会重复无意义地重试。
    repo_info: HashMap<String, (Instant, Option<RepoInfo>)>,
    /// 正在后台探测仓库身份、避免重复起进程的 cwd 集合。
    repo_info_inflight: HashSet<String>,
    /// 正在新建的 worktree 目标 + 弹窗里的分支名文本框（None = 没在新建）。
    /// 正在确认删除的 worktree（None = 没在删）。
    delete_worktree_target: Option<DeleteWorktreeTarget>,
    /// 正在确认丢弃的 diff 块：(仓库根, hunk 下标)。丢弃直接改工作区文件且不进
    /// reflog，找不回来，所以必须过一道确认。
    discard_hunk_target: Option<(String, usize)>,
    /// 正在确认丢弃整个文件的改动：(仓库根, 相对路径, 是否未跟踪)。未跟踪文件是
    /// 直接删盘，比 restore 更狠，文案要分开写。
    discard_file_target: Option<(String, String, bool)>,
    /// 「丢弃全部改动」确认弹窗的目标仓库根（Some = 弹窗开着）。见 git_panel.rs。
    discard_all_target: Option<String>,
    /// git 远端同步 / stash 操作进行中：Some(操作名) = 正在跑，None = 空闲。
    /// 既做并发闸门（防连点抢 index.lock），也给 SOURCE CONTROL 头显示「拉取中…」
    /// 这类进行中反馈——否则点了按钮几秒内毫无动静。见 git_panel.rs run_git_op。
    git_op: Option<&'static str>,
    /// 各类后台操作（建/删 worktree、生成 commit message 等）失败时的提示，render
    /// 顶部取走并弹成通知；后台任务里没有 Window，弹不了通知，所以先暂存到这。
    background_error: Option<String>,
    /// 守护进程是否落后于磁盘上的 smeltd 二进制（重装/重编译后常见，需手动重启守护
    /// 才生效新代码）；None 表示还没查过，驱动设置页「更新」分区的重启提示。
    daemon_outdated: Option<bool>,
    /// 最近一次无缝升级的结果提示（设置页守护分区显示；None = 没试过）。
    daemon_upgrade_msg: Option<String>,
    /// 无缝升级进行中（按钮置灰防连点）。
    daemon_upgrading: bool,
    /// 守护自报的运行信息（PID / 启动时刻 / 会话数），设置页「更新」里展示。
    /// 跟 daemon_outdated 同一趟后台探测回填；守护没起 → None。
    daemon_info: Option<terminal::DaemonInfo>,
    /// 「重启守护进程」二次确认弹窗开关：点确定会断开所有当前终端会话。
    show_daemon_restart_confirm: bool,
    /// 「会话管理」弹窗开关：设置页「更新」tab 点开会话数详情用。守护进程持有
    /// 的会话不只 GUI 侧栏认领的那些——测试跑出来的孤儿、忘了关的临时会话都会
    /// 计进「N 个会话」里但从没在任何侧栏露过面，只有这里能看见并单独清理，
    /// 不用被迫走「重启守护进程」这种会误伤正常会话的核选项。
    session_manager_open: bool,
    /// 弹窗数据：最近一次 list 查询结果，None = 正在查/还没查过。
    session_manager_list: Option<Vec<terminal::DaemonSessionState>>,
    /// 启动时从存档恢复失败的会话（守护未就绪等）。仍写回 workspace.json，避免
    /// 「恢复失败 → 写空盘 → 会话永久蒸发」。侧栏本帧看不到它们，下次冷启动会重试。
    restore_orphans: Vec<(usize, SessionState)>,
    /// 用户在后台恢复期间删除的项目路径；尚未交货的恢复结果命中这些路径时直接丢弃。
    cancelled_restore_paths: Vec<String>,
    /// 会话列表被用户增删/重排的版本号。后台恢复只在版本未变化时按旧存档索引插入。
    session_list_revision: u64,
    /// 活动会话被用户切换的版本号。后台恢复只在版本未变化时恢复存档中的活动项。
    active_session_revision: u64,
    /// 根节点自己的焦点句柄：总览/文件树/Git/历史会话这些页面自身没有可
    /// 聚焦的元素，切过去后如果谁都不 focus，窗口的 focus 仍停在切走前那个（可能
    /// 已经不在当前渲染树里的）终端上——GPUI 找不到就把 focus 兜底纠正到 window 的
    /// 真正根节点，而 Workspace 这层的 on_key_down（Cmd+Shift+F 等全局快捷键）挂在
    /// Root 组件之下、并非那个根节点，于是收不到事件，表现为"切到别的页面后快捷键
    /// 全部失灵"。切到非终端页面时把 focus 显式认领到这个句柄上，保证 Workspace 的
    /// on_key_down 始终在 dispatch 路径上。
    focus_handle: FocusHandle,
    /// 冷启动的会话恢复流程是否已经跑完（没有待恢复会话时启动即为 true）。
    /// save_state 的抹盘安全阀靠它区分「还没恢复上来的空」和「用户真把会话全关了」。
    sessions_restored: bool,
}

impl std::ops::Deref for Workspace {
    type Target = SessionUiState;

    fn deref(&self) -> &Self::Target {
        &self.right_route
    }
}

impl std::ops::DerefMut for Workspace {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.right_route
    }
}

impl Workspace {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 存档只读元数据；**不**在 UI 线程同步 Terminal::spawn（会 beachball 数秒）。
        // 会话 reattach 丢后台线程，窗口先起来用户即可点侧栏/设置。
        let saved = load_ws_state();
        let file_tree_w = saved.as_ref().and_then(|s| s.file_tree_w).unwrap_or(260.);
        // 旧版开合动画曾把过渡中的接近零宽度误存为用户偏好；加载时按侧栏
        // 可拖拽下限修复这类状态，避免下一次展开仍以错误宽度为目标。
        let sidebar_w = saved
            .as_ref()
            .and_then(|s| s.sidebar_w)
            .unwrap_or(280.)
            .max(200.);
        let sidebar_open = saved.as_ref().and_then(|s| s.sidebar_open).unwrap_or(true);
        let inspector_w = saved.as_ref().and_then(|s| s.inspector_w).unwrap_or(344.);
        let inspector_open = saved
            .as_ref()
            .and_then(|s| s.inspector_open)
            .unwrap_or(true);

        let (pending_sessions, active_session) = saved
            .as_ref()
            .map(normalize_saved_sessions)
            .unwrap_or_default();
        // 恢复完成前先放进 orphans：save_state 会合并 orphans，避免空 sessions 窗口期抹盘。
        let restore_orphans = pending_sessions.iter().cloned().enumerate().collect();
        let sessions: Vec<Session> = Vec::new();

        // 项目列表：新存档直接读；旧存档（没有 projects 字段）从各会话 cwd 反推一份，
        // 保证升级后侧栏分组跟升级前长得一样，之后这些项目就独立于会话活着了。
        let projects: Vec<String> = match saved.as_ref() {
            Some(s) if !s.projects.is_empty() => s.projects.clone(),
            _ => {
                let mut seen: Vec<String> = Vec::new();
                for cwd in pending_sessions.iter().filter_map(session_state_cwd) {
                    if !cwd.is_empty() && !seen.contains(&cwd) {
                        seen.push(cwd);
                    }
                }
                seen
            }
        };

        // 文件树列 resize：拖动完 emit Resized，写回存档持久化宽度。
        let file_tree_resize = cx.new(|_| ResizableState::default());
        let _file_tree_resize_sub = cx.subscribe(
            &file_tree_resize,
            |this, state, _e: &ResizablePanelEvent, cx| {
                if let Some(size) = state.read(cx).sizes().first() {
                    this.file_tree_w = f32::from(*size);
                }
                this.save_state(cx);
            },
        );
        // 日志页三栏 resize（不落盘：日志是临时查看，没必要持久化）。
        let git_log_resize = cx.new(|_| ResizableState::default());
        let workspace_resize = cx.new(|_| ResizableState::default());
        let _workspace_resize_sub = cx.subscribe(
            &workspace_resize,
            |this, state, _e: &ResizablePanelEvent, cx| {
                // inspector 入场动画期间也是靠 resize_panel 逐帧改宽度触发这个事件，
                // 但那是过渡态的中间值，不是用户真的拖出来的宽度——跳过同步/落盘，
                // 避免把动画过程中的小尺寸误存成偏好，也避免每帧都写一次配置文件。
                if this.inspector_transition.is_animating()
                    || this.sidebar_transition.is_animating()
                {
                    return;
                }
                let sizes = state.read(cx).sizes();
                if this.sidebar_open
                    && let Some(size) = sizes.first()
                {
                    this.sidebar_w = f32::from(*size);
                }
                this.save_state(cx);
            },
        );
        let stage_inspector_resize = cx.new(|_| ResizableState::default());
        let _stage_inspector_resize_sub = cx.subscribe(
            &stage_inspector_resize,
            |this, state, _e: &ResizablePanelEvent, cx| {
                // 跟上面 workspace_resize 同理：inspector 入场动画期间也靠 resize_panel
                // 逐帧改宽度触发这个事件，那是过渡态中间值，不是用户真的拖出来的宽度。
                if this.inspector_transition.is_animating() {
                    return;
                }
                if let Some(size) = state.read(cx).sizes().get(1) {
                    this.inspector_w = f32::from(*size);
                }
                this.save_state(cx);
            },
        );
        let bottom_drawer_resize = cx.new(|_| ResizableState::default());
        let _bottom_drawer_resize_sub = cx.subscribe(
            &bottom_drawer_resize,
            |this, state, _e: &ResizablePanelEvent, cx| {
                // 跟上面 workspace_resize 同理：动画过程中的 resize_panel 调用别当成
                // 用户手动拖拽，只在真正静止时才采信。
                if this.bottom_drawer_transition.is_animating() {
                    return;
                }
                if let Some(size) = state.read(cx).sizes().get(1) {
                    this.bottom_drawer_h = f32::from(*size);
                }
            },
        );

        let mut right_route = SessionUiState::default();
        right_route.inspector_open = inspector_open;
        right_route.inspector_w = inspector_w;
        right_route.file_tree_w = file_tree_w;
        right_route.collapsed_roots = saved
            .as_ref()
            .map(|s| s.collapsed_file_tree_roots.iter().cloned().collect())
            .unwrap_or_default();
        right_route.pinned_roots = saved
            .as_ref()
            .map(|s| s.pinned_file_tree_roots.clone())
            .unwrap_or_default();

        let mut ws = Self {
            sessions,
            active_session,
            primary_route: saved.as_ref().map(|state| state.route).unwrap_or_default(),
            ui_session_id: None,
            right_route,
            inspector_transition: panel_transition::PanelTransition::new(inspector_open),
            bottom_drawer_transition: panel_transition::PanelTransition::new(false),
            bottom_drawer_resize,
            dir_cache: HashMap::new(),
            dir_inflight: HashSet::new(),
            file_tree_pending_reveal: None,
            acp_image_preview: None,
            file_gen: 0,
            pending_file_switch: None,
            delete_file_target: None,
            pending_switch_after_save: None,
            diff_gen: 0,
            git_log: git_log::GitLogState::default(),
            pushing: false,
            delete_branch_target: None,
            git_log_resize,
            workspace_resize,
            stage_inspector_resize,
            sidebar_open,
            sidebar_transition: panel_transition::PanelTransition::new(sidebar_open),
            sidebar_w,
            diff_comment_input: None,
            commit_msg_input: None,
            commit_msg_generating: false,
            // 没有待恢复会话 → 一开始就算「恢复完毕」，否则等 schedule_session_restore 置位。
            sessions_restored: pending_sessions.is_empty(),
            close_project_target: None,
            projects,
            active_project: None,
            collapsed_projects: saved
                .as_ref()
                .map(|s| s.collapsed_projects.iter().cloned().collect())
                .unwrap_or_default(),
            sidebar_grouping: saved
                .as_ref()
                .map(|s| s.sidebar_grouping)
                .unwrap_or_default(),
            skills_cache: None,
            skills_cache_cwd: None,
            skills_inflight: false,
            skill_modal: None,
            skill_delete_target: None,
            skill_link_modal: None,
            palette: None,
            _palette_sub: None,
            diff_scroll: UniformListScrollHandle::new(),
            file_tree_scroll: ScrollHandle::new(),
            file_tree_resize,
            file_filter: None,
            _file_filter_sub: None,
            task_title_input: None,
            task_body_input: None,
            task_run_at_input: None,
            task_kind: tasks::TaskKind::Once,
            task_auto_run: true,
            task_selected: None,
            task_bind_project: None,
            task_bind_launch: None,
            task_bind_session: None,
            task_column_filter: None,
            _task_title_sub: None,
            show_new_task_modal: false,
            task_editing: None,
            task_schedule_started: false,
            search_results: None,
            search_gen: 0,
            _file_tree_resize_sub,
            _workspace_resize_sub,
            _stage_inspector_resize_sub,
            _bottom_drawer_resize_sub,
            git_status: HashMap::new(),
            git_status_inflight: HashSet::new(),
            branches: HashMap::new(),
            branches_inflight: HashSet::new(),
            git_dirty: Arc::new(Mutex::new(HashSet::new())),
            git_watchers: HashMap::new(),
            git_autofetch_at: HashMap::new(),
            session_list: HashMap::new(),
            session_list_inflight: HashSet::new(),
            session_detail: None,
            history_detail_list_state: gpui::ListState::new(0, gpui::ListAlignment::Top, px(800.)),
            session_detail_gen: 0,
            history_pane: HistoryPane::Sessions,
            history_agent: settings::AcpAgentKind::Claude,
            history_profile: None,
            memory_list: HashMap::new(),
            memory_list_inflight: HashSet::new(),
            memory_selected: None,
            llm_inputs: None,
            llm_subs: Vec::new(),
            launch_inputs: None,
            profile_inputs: None,
            opacity_slider: None,
            font_size_slider: None,
            bg_color_picker: None,
            pet_color_picker: None,
            settings_subs: Vec::new(),
            applied_window_bg: None,
            debug_hud: false,
            last_frame: None,
            debug_mem_rss: None,
            debug_mem_sampled_at: None,
            fps_ema: 0.0,
            show_quit_confirm: false,
            update_status: updater::UpdateStatus::default(),
            settings_page_ix: 0,
            settings_page_nonce: 0,
            font_options: std::cell::OnceCell::new(),
            dock_badge_count: None,
            status_menu_snapshot: None,
            sess_drop_hint: None,
            proj_drop_hint: None,
            rename_target: None,
            rename_input: None,
            _rename_sub: None,
            repo_info: HashMap::new(),
            repo_info_inflight: HashSet::new(),
            delete_worktree_target: None,
            discard_hunk_target: None,
            discard_file_target: None,
            discard_all_target: None,
            git_op: None,
            background_error: None,
            daemon_outdated: None,
            daemon_upgrade_msg: None,
            daemon_upgrading: false,
            daemon_info: None,
            show_daemon_restart_confirm: false,
            session_manager_open: false,
            session_manager_list: None,
            restore_orphans,
            cancelled_restore_paths: Vec::new(),
            session_list_revision: 0,
            active_session_revision: 0,
            focus_handle: cx.focus_handle(),
        };
        // orphans 已挂上全部待恢复会话 → 写盘不会抹掉存档。
        ws.save_state(cx);
        updater::cleanup_stale_backup();
        ws.check_for_update(true, cx);
        // 有待恢复会话：ensure+reattach 在 restore 线程串行做完后再 check_daemon_outdated，
        // 避免与 ensure handoff 三线并行踩踏。无会话则直接查守护状态。
        if !pending_sessions.is_empty() {
            eprintln!(
                "[workspace] 后台恢复 {} 个会话（不堵 UI）…",
                pending_sessions.len()
            );
            ws.schedule_session_restore(pending_sessions, active_session, window, cx);
        } else {
            ws.check_daemon_outdated(cx);
        }
        ws
    }

    /// 冷启动：专用 OS 线程里 **先 ensure managed 守护，再 reattach 全部会话**。
    /// 完成后才 `check_daemon_outdated`（不与 restore 并行 upgrade）。
    fn schedule_session_restore(
        &mut self,
        pending: Vec<SessionState>,
        active_session: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let restore_revision = self.session_list_revision;
        let restore_active_revision = self.active_session_revision;
        // ACP 与终端都由 smeltd 托管。先拆开只是因为两者的 GUI 重建方式不同；
        // 两边都必须等 managed daemon 完成 ensure/handoff 后才能开始连接。
        let (acp_saved, pending) = split_restore_queue(pending);
        // 逐个交货，别攒成一整包：会话之间互不依赖，攒一包等于让窗口空等最慢的那次
        // attach——表现为「冷启动后一个会话都不显示，过一会才全部冒出来」。改成恢复好
        // 一个就发一个，第一个会话立刻上屏，其余陆续补齐。unbounded 保证后台线程不会
        // 因为 UI 还没来得及收而卡住。
        let (tx, rx) = smol::channel::unbounded();
        let (daemon_ready_tx, daemon_ready_rx) = smol::channel::bounded(1);
        std::thread::Builder::new()
            .name("smelt-restore-sessions".into())
            .spawn(move || {
                // 1) 完整 ensure（可能 handoff）→ 2) 再 reattach。禁止与 UI 侧并行 upgrade。
                let _ = terminal::ensure_managed_daemon_current();
                terminal::ensure_daemon_running();
                // ACP 占位只有收到这道闸门后才会在 UI 线程创建；否则当前活动会话
                // 会立刻 auto-resume，随后 ensure/handoff 重启守护，把刚建的 socket
                // 踢成「与 smeltd 的连接已断开」。
                if daemon_ready_tx.send_blocking(()).is_err() {
                    return;
                }
                let mut daemon_ok = true;
                for (original_index, ss) in pending {
                    let outcome = if daemon_ok {
                        match spawn_layout_leaves(&ss.layout) {
                            Ok(leaves) => Ok(leaves),
                            Err(e) => {
                                if e.contains("smeltd 未就绪") {
                                    daemon_ok = false;
                                }
                                Err(e)
                            }
                        }
                    } else {
                        Err("smeltd 未就绪（先前会话已失败）".to_string())
                    };
                    // 接收端没了（窗口已关）就别再白跑剩下的
                    if tx.send_blocking((original_index, ss, outcome)).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn smelt-restore-sessions 线程");

        cx.spawn_in(window, async move |this, cx| {
            let mut restored = 0usize;
            let mut restored_order = Vec::new();
            let mut restore_order_intact = true;

            // managed daemon 已经稳定后再把 ACP 会话放回视图树。当前活动 ACP 随后
            // 触发 maybe_auto_resume 时，连接面对的是最终守护进程，不会刚接上又断。
            if daemon_ready_rx.recv().await.is_err() {
                return;
            }
            if this
                .update_in(cx, |this, window, cx| {
                    for (original_index, ss) in acp_saved {
                        if restore_path_is_cancelled(
                            session_state_cwd(&ss).as_deref(),
                            &this.cancelled_restore_paths,
                        ) {
                            this.restore_orphans
                                .retain(|(index, _)| *index != original_index);
                            continue;
                        }
                        let Some(saved) = ss.acp else { continue };
                        let reason = "正在恢复上次的对话…";
                        let agent = saved
                            .agent
                            .as_deref()
                            .and_then(settings::AcpAgentKind::from_id)
                            .unwrap_or_else(|| acp_agent_from_cmd(&saved.launch.command));
                        let refresh_launch_from_settings = saved.refresh_launch_from_settings();
                        let fork_origin = saved.fork_origin.clone();
                        let view = cx.new(|cx| {
                            acp_view::AcpView::placeholder(
                                cx,
                                agent,
                                saved.launch,
                                refresh_launch_from_settings,
                                saved.profile_id,
                                saved.cwd,
                                reason.to_string(),
                                Vec::new(),
                                saved.history_session_id,
                                saved.sid,
                            )
                        });
                        view.update(cx, |view, _cx| view.set_fork_origin(fork_origin));
                        let _acp_persist_sub = Some(this.subscribe_acp_persist(&view, window, cx));
                        if this.session_list_revision != restore_revision {
                            restore_order_intact = false;
                        }
                        let insert_at = if restore_order_intact {
                            planned_restore_insert_position(
                                &restored_order,
                                original_index,
                                this.sessions.len(),
                            )
                            .unwrap_or_else(|| {
                                restore_order_intact = false;
                                this.sessions.len()
                            })
                        } else {
                            this.sessions.len()
                        };
                        this.sessions.insert(
                            insert_at,
                            Session {
                                ui_id: next_session_ui_id(),
                                kind: SessionKind::Acp(view),
                                custom_title: ss.custom_title,
                                _acp_persist_sub,
                                ui_state: ss
                                    .route
                                    .clone()
                                    .map(SessionUiState::restore)
                                    .unwrap_or_default(),
                            },
                        );
                        record_restored_index(
                            &mut restored_order,
                            insert_at,
                            original_index,
                            restore_order_intact,
                        );
                        this.restore_orphans
                            .retain(|(index, _)| *index != original_index);
                        restored += 1;
                    }
                    cx.notify();
                })
                .is_err()
            {
                return;
            }

            // 收一个渲染一个。后台线程跑完会 drop sender，recv 报错即代表全部处理完。
            while let Ok((original_index, ss, result)) = rx.recv().await {
                let outcome = this.update_in(cx, |this, _window, cx| {
                    if restore_path_is_cancelled(
                        session_state_cwd(&ss).as_deref(),
                        &this.cancelled_restore_paths,
                    ) {
                        if let Ok(leaves) = result {
                            for leaf in leaves {
                                terminal::kill_remote(&leaf.sid);
                            }
                        }
                        this.restore_orphans
                            .retain(|(index, _)| *index != original_index);
                        return false;
                    }
                    let leaves = match result {
                        Ok(leaves) => leaves,
                        Err(e) => {
                            eprintln!("[workspace] 会话恢复失败，保留 orphan：{e}");
                            return false;
                        }
                    };
                    let mut leaf_iter = leaves.into_iter();
                    let mut tabs = Vec::new();
                    let Some(layout) =
                        rebuild_pane_ready(&ss.layout, &mut leaf_iter, &mut tabs, cx)
                    else {
                        return false;
                    };
                    let Some(active) = tabs.get(ss.active).or_else(|| tabs.first()).cloned() else {
                        return false;
                    };
                    if this.session_list_revision != restore_revision {
                        restore_order_intact = false;
                    }
                    let insert_at = if restore_order_intact {
                        planned_restore_insert_position(
                            &restored_order,
                            original_index,
                            this.sessions.len(),
                        )
                        .unwrap_or_else(|| {
                            restore_order_intact = false;
                            this.sessions.len()
                        })
                    } else {
                        this.sessions.len()
                    };
                    this.sessions.insert(
                        insert_at,
                        Session {
                            ui_id: next_session_ui_id(),
                            kind: SessionKind::Term { layout, active },
                            custom_title: ss.custom_title,
                            _acp_persist_sub: None,
                            ui_state: ss
                                .route
                                .clone()
                                .map(SessionUiState::restore)
                                .unwrap_or_default(),
                        },
                    );
                    record_restored_index(
                        &mut restored_order,
                        insert_at,
                        original_index,
                        restore_order_intact,
                    );
                    this.restore_orphans
                        .retain(|(index, _)| *index != original_index);
                    // 让这一个立刻上屏，不等其余的
                    cx.notify();
                    true
                });
                match outcome {
                    Ok(true) => restored += 1,
                    Ok(false) => {}
                    Err(_) => return, // 窗口已关，收摊
                }
            }

            let _ = this.update_in(cx, |this, window, cx| {
                this.sessions_restored = true;
                if should_restore_saved_active(
                    restore_order_intact,
                    this.session_list_revision,
                    restore_revision,
                    this.active_session_revision,
                    restore_active_revision,
                ) {
                    this.active_session = restored_active_position(&restored_order, active_session);
                }
                if let Some(Session {
                    kind: SessionKind::Acp(view),
                    ..
                }) = this.sessions.get(this.active_session)
                {
                    view.update(cx, |view, cx| view.maybe_auto_resume(window, cx));
                }
                this.save_state(cx);
                if !this.restore_orphans.is_empty() {
                    eprintln!(
                        "[workspace] {} 个会话未能恢复，已保留在存档中，下次启动会重试",
                        this.restore_orphans.len()
                    );
                }
                eprintln!(
                    "[workspace] 后台恢复完成：成功 {restored}，失败 {}",
                    this.restore_orphans.len()
                );
                // restore 完成后再查/升级守护，避免与 reattach 并行 handoff
                this.check_daemon_outdated(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 当前活动会话（不可变引用）。
    fn cur(&self) -> Option<&Session> {
        self.sessions.get(self.active_session)
    }

    /// 活动 session 变化时，完整交换右侧工作区。用 session 自己的稳定 ui_id 判断，
    /// 不依赖数组下标，所以拖拽排序、关闭前面的 session 都不会把快照串给别人。
    fn sync_session_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(new_id) = self.cur().map(|session| session.ui_id) else {
            self.ui_session_id = None;
            return;
        };
        if self.ui_session_id == Some(new_id) {
            return;
        }
        let Some(old_id) = self.ui_session_id else {
            if let Some(new_ix) = self.sessions.iter().position(|s| s.ui_id == new_id)
                && self.sessions[new_ix].ui_state.restored_from_archive
            {
                swap_right_route(&mut self.right_route, &mut self.sessions[new_ix].ui_state);
            }
            self.ui_session_id = Some(new_id);
            self.restore_active_route_runtime(window, cx);
            return;
        };
        if let Some(old_ix) = self.sessions.iter().position(|s| s.ui_id == old_id) {
            swap_right_route(&mut self.right_route, &mut self.sessions[old_ix].ui_state);
        }
        if let Some(new_ix) = self.sessions.iter().position(|s| s.ui_id == new_id) {
            swap_right_route(&mut self.right_route, &mut self.sessions[new_ix].ui_state);
        }
        self.ui_session_id = Some(new_id);
        self.restore_active_route_runtime(window, cx);
    }

    fn restore_active_route_runtime(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.right_route.restored_from_archive = false;
        self.inspector_transition.set_open(self.inspector_open);
        self.bottom_drawer_transition
            .set_open(self.bottom_drawer_open);
        self.stage_inspector_resize.update(cx, |state, cx| {
            state.resize_panel(1, px(self.inspector_w), window, cx);
        });
        self.bottom_drawer_resize.update(cx, |state, cx| {
            state.resize_panel(1, px(self.bottom_drawer_h), window, cx);
        });
        self.file_tree_resize.update(cx, |state, cx| {
            state.resize_panel(0, px(self.file_tree_w), window, cx);
        });
        let terminal_ids = self
            .bottom_drawer_tabs
            .iter()
            .filter(|tab| tab.terminal.is_none())
            .map(|tab| tab.id)
            .collect::<Vec<_>>();
        for id in terminal_ids {
            self.spawn_bottom_drawer_terminal(id, cx);
        }
        if let Some(path) = self.pending_restore_file.take() {
            self.view_file(path, window, cx);
        }
    }

    /// 某个 cwd 归属哪个已打开项目（见 project_root_of）。
    fn project_root_for_cwd(&self, cwd: &str) -> Option<String> {
        project_root_of(&self.projects, cwd)
    }

    /// 确保一个会话 cwd 背后的项目是实体，而不是只靠 `project_groups` 临时推导出来的
    /// “隐式组”。否则最后一个会话关闭后，组会跟着 cwd 一起消失，看起来就像删会话
    /// 顺手删了项目。
    ///
    /// 已落在某个已打开项目之下时不新增子项目；只有完全无主的 cwd 才成为项目根。
    fn remember_session_project(&mut self, cwd: Option<&str>) {
        let Some(cwd) = cwd.map(str::trim).filter(|cwd| !cwd.is_empty()) else {
            return;
        };
        let root = cwd.trim_end_matches('/');
        if self.project_root_for_cwd(root).is_none()
            && !self
                .projects
                .iter()
                .any(|p| p.trim_end_matches('/') == root)
        {
            self.projects.push(root.to_string());
        }
    }

    /// 侧栏分组（见 ProjectGroup）。骨架是 `self.projects`——项目独立于会话存在，所以
    /// 一个会话都没有的项目照样出现（sessions 为空）。会话按 cwd 挂到所属项目下；挂不上
    /// 的（旧会话、临时目录）仍按自己的 cwd 自建隐式组接在后面。
    ///
    /// 分组身份一律用 **root 路径**：末段同名的两个目录是两个项目，各占一行、会话不混。
    /// 显示名重复时由 disambiguate_labels 补父目录段区分。
    ///
    /// 侧栏渲染和拖拽排序共用同一份算法，避免两处各算一遍、行为跑偏。worktree 检出
    /// 显示「仓库名 · 分支名」（见 group_info_for_cwd），且跟主仓库、其余 worktree 聚在
    /// 一起排序，不会散落在列表各处——组间相对顺序按「同一簇里最早出现的组」的先后来
    /// （stable_sort，不会无意义打乱手动拖拽过的顺序）。
    pub(crate) fn project_groups(&self, cx: &App) -> Vec<ProjectGroup> {
        let mut groups: Vec<ProjectGroup> = Vec::new();
        // 聚簇 key（worktree 与主仓库共享 common-dir）与消歧用的原始显示名，按组下标存。
        let mut clusters: Vec<Option<String>> = Vec::new();
        let mut bases: Vec<String> = Vec::new();
        let same_root = |a: &str, b: &str| a.trim_end_matches('/') == b.trim_end_matches('/');

        // 骨架：已打开的项目按列表顺序占位，先不管有没有会话。
        for root in &self.projects {
            if groups.iter().any(|g| same_root(&g.root, root)) {
                continue;
            }
            let (label, cluster) = self.group_info_for_cwd(root);
            bases.push(label.clone());
            clusters.push(cluster);
            groups.push(ProjectGroup {
                root: root.clone(),
                label,
                sessions: Vec::new(),
            });
        }
        // 会话挂到所属项目下；无主的按自己的 cwd 自建一组。
        for (ix, s) in self.sessions.iter().enumerate() {
            let cwd = s.cwd(cx).unwrap_or_default();
            let owner = self.project_root_for_cwd(&cwd);
            let root = owner.unwrap_or(cwd);
            match groups.iter_mut().find(|g| same_root(&g.root, &root)) {
                Some(g) => g.sessions.push(ix),
                None => {
                    let (label, cluster) = self.group_info_for_cwd(&root);
                    bases.push(label.clone());
                    clusters.push(cluster);
                    groups.push(ProjectGroup {
                        root,
                        label,
                        sessions: vec![ix],
                    });
                }
            }
        }

        // 同仓库（主仓库 + 各 worktree）聚到一起：按「这一簇里最早出现的组」排。
        let key_of = |i: usize| {
            clusters[i]
                .clone()
                .unwrap_or_else(|| groups[i].root.clone())
        };
        let mut first_seen: HashMap<String, usize> = HashMap::new();
        for i in 0..groups.len() {
            first_seen.entry(key_of(i)).or_insert(i);
        }
        let mut order: Vec<usize> = (0..groups.len()).collect();
        order.sort_by_key(|&i| first_seen[&key_of(i)]);
        let mut sorted: Vec<ProjectGroup> = Vec::with_capacity(groups.len());
        let mut sorted_bases: Vec<String> = Vec::with_capacity(groups.len());
        for i in order {
            sorted.push(ProjectGroup {
                root: groups[i].root.clone(),
                label: groups[i].label.clone(),
                sessions: groups[i].sessions.clone(),
            });
            sorted_bases.push(bases[i].clone());
        }
        disambiguate_labels(&mut sorted, &sorted_bases);
        sorted
    }

    /// 拖拽排序：把 dragged 会话挪到 target 会话旁边（before=true 插到它前面，否则插到
    /// 它后面）。只在同一项目内生效——这是「项目内排序」，不是「跨项目挪会话」，
    /// dragged/target 分属不同项目时直接不动。用 entity_id 找位置而非缓存的下标：拖拽
    /// 跨越多帧，下标可能因为其间的关会话等操作失效。
    fn move_session_near(
        &mut self,
        dragged: EntityId,
        target: EntityId,
        before: bool,
        cx: &mut Context<Self>,
    ) {
        if dragged == target {
            return;
        }
        let groups = self.project_groups(cx);
        let group_of = |id: EntityId| {
            groups.iter().position(|g| {
                g.sessions
                    .iter()
                    .any(|&ix| self.sessions[ix].anchor_id() == id)
            })
        };
        let (Some(dragged_group), Some(target_group)) = (group_of(dragged), group_of(target))
        else {
            return;
        };
        if dragged_group != target_group {
            return;
        }
        let Some(from_ix) = self.sessions.iter().position(|s| s.anchor_id() == dragged) else {
            return;
        };
        let Some(target_ix) = self.sessions.iter().position(|s| s.anchor_id() == target) else {
            return;
        };

        let active_id = self.cur().map(|s| s.anchor_id());
        let session = self.sessions.remove(from_ix);
        let adjusted_target_ix = if from_ix < target_ix {
            target_ix - 1
        } else {
            target_ix
        };
        let insert_at = adjusted_target_ix + if before { 0 } else { 1 };
        self.sessions.insert(insert_at, session);
        self.session_list_revision = self.session_list_revision.wrapping_add(1);

        if let Some(id) = active_id {
            if let Some(ix) = self.sessions.iter().position(|s| s.anchor_id() == id) {
                self.active_session = ix;
            }
        }
        self.save_state(cx);
        cx.notify();
    }

    /// 拖拽排序：把 from 项目的所有会话（保持相对顺序）整体挪到 to 项目最前面。
    /// 认 **root 路径**不认显示名（同名目录是两个项目，见 ProjectGroup）。
    /// （项目拖拽待接到 UI，暂时闲置；见 ProjectDrag 注释。）
    #[allow(dead_code)]
    fn move_project_near(&mut self, from_root: &str, to_root: &str, cx: &mut Context<Self>) {
        let same_root = |a: &str, b: &str| a.trim_end_matches('/') == b.trim_end_matches('/');
        if same_root(from_root, to_root) {
            return;
        }
        let groups = self.project_groups(cx);
        let Some(from) = groups.iter().find(|g| same_root(&g.root, from_root)) else {
            return;
        };
        if !groups.iter().any(|g| same_root(&g.root, to_root)) {
            return;
        }
        let mut from_ixs = from.sessions.clone();
        from_ixs.sort_unstable();

        let active_id = self.cur().map(|s| s.anchor_id());
        // 降序 remove 保证前面下标不受后面删除影响；收集完再倒回原相对顺序。
        let mut moved: Vec<Session> = from_ixs
            .iter()
            .rev()
            .map(|&ix| self.sessions.remove(ix))
            .collect();
        moved.reverse();

        let insert_at = self
            .sessions
            .iter()
            .position(|s| {
                // 归属判定必须跟 project_groups 用同一套（project_root_for_cwd），
                // 否则这里永远找不到目标组、挪动直接失效。
                let cwd = s.cwd(cx).unwrap_or_default();
                self.project_root_for_cwd(&cwd)
                    .is_some_and(|r| same_root(&r, to_root))
            })
            .unwrap_or(self.sessions.len());
        for (i, s) in moved.into_iter().enumerate() {
            self.sessions.insert(insert_at + i, s);
        }
        self.session_list_revision = self.session_list_revision.wrapping_add(1);

        if let Some(id) = active_id {
            if let Some(ix) = self.sessions.iter().position(|s| s.anchor_id() == id) {
                self.active_session = ix;
            }
        }
        self.save_state(cx);
        cx.notify();
    }

    /// 「+」/新建：开一个独立新会话（单终端），并切过去。
    fn add_session(&mut self, cwd: Option<String>, cx: &mut Context<Self>) {
        self.add_session_with_launch(cwd, None, cx);
    }

    /// ACP 会话内容变化（AcpViewEvent::Changed）→ 立即 save_state。与侧栏/文件树
    /// resize 订阅同一惯用法（main.rs::new 里的 _resize_sub）。
    fn subscribe_acp_persist(
        &mut self,
        view: &Entity<acp_view::AcpView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Subscription {
        cx.subscribe_in(
            view,
            window,
            |this: &mut Self, _view, ev: &acp_view::AcpViewEvent, window, cx| match ev {
                acp_view::AcpViewEvent::Changed => this.save_state(cx),
                acp_view::AcpViewEvent::PreviewImage(image) => {
                    this.acp_image_preview = Some(image.clone());
                    cx.notify();
                }
                acp_view::AcpViewEvent::ContinueInNewSession(request) => {
                    this.add_acp_handoff_session(request.clone(), window, cx);
                }
                acp_view::AcpViewEvent::NavigateToSession(session_id) => {
                    if let Some(ix) = this
                        .sessions
                        .iter()
                        .position(|session| match &session.kind {
                            SessionKind::Acp(view) => view.read(cx).session_id() == session_id,
                            SessionKind::Term { .. } => false,
                        })
                    {
                        this.activate(ix, window, cx);
                    }
                }
            },
        )
    }

    fn add_acp_handoff_session(
        &mut self,
        request: acp_view::AcpHandoffRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remember_session_project(request.cwd.as_deref());
        let title = format!("继续：{}", request.source.title);
        let view = cx.new(|cx| acp_view::AcpView::start_with_handoff(window, cx, request));
        let _acp_persist_sub = Some(self.subscribe_acp_persist(&view, window, cx));
        self.sessions.push(Session {
            ui_id: next_session_ui_id(),
            kind: SessionKind::Acp(view),
            custom_title: Some(title),
            _acp_persist_sub,
            ui_state: SessionUiState::default(),
        });
        self.session_list_revision = self.session_list_revision.wrapping_add(1);
        self.active_session = self.sessions.len() - 1;
        self.save_state(cx);
        cx.notify();
    }

    /// 「+」菜单「对话 · smelt 原生界面」下那几项：新建 ACP 会话（第二种会话类型，
    /// 结构化消息流）。`agent` 决定接哪家（Claude / Copilot / Codex），命令从对应的
    /// 全局配置取。spawn_acp 只起线程立即返回，不需要 add_session_with_launch 那套
    /// 后台三段舞。
    fn add_acp_session(
        &mut self,
        agent: settings::AcpAgentKind,
        launch_override: Option<smelt_core::agent_kind::AcpLaunchSpec>,
        profile_id: Option<String>,
        cwd: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remember_session_project(cwd.as_deref());
        let launch = launch_override.unwrap_or_else(|| {
            smelt_core::agent_kind::AcpLaunchSpec::from_command(settings::acp_cmd_for(agent, cx))
        });
        let view =
            cx.new(|cx| acp_view::AcpView::start(window, cx, agent, launch, profile_id, cwd));
        let _acp_persist_sub = Some(self.subscribe_acp_persist(&view, window, cx));
        self.sessions.push(Session {
            ui_id: next_session_ui_id(),
            kind: SessionKind::Acp(view),
            custom_title: None,
            _acp_persist_sub,
            ui_state: SessionUiState::default(),
        });
        self.session_list_revision = self.session_list_revision.wrapping_add(1);
        self.active_session = self.sessions.len() - 1;
        self.save_state(cx);
        cx.notify();
    }

    /// 找当前已开的、匹配某个 agent+cwd+具体 session id 的 ACP 会话下标——「继续」
    /// 点击时和后台加载完成时各查一次，两处逻辑必须完全一致，抽出来避免漂移。
    fn find_open_acp_session(
        &self,
        agent: settings::AcpAgentKind,
        cwd: &str,
        target_id: &agent_client_protocol::schema::v1::SessionId,
        cx: &App,
    ) -> Option<usize> {
        self.sessions.iter().position(|s| match &s.kind {
            SessionKind::Acp(view) => {
                let v = view.read(cx);
                v.agent_kind() == agent
                    && v.cwd().as_deref() == Some(cwd)
                    && v.history_session_id_for_save().as_ref() == Some(target_id)
            }
            _ => false,
        })
    }

    /// 历史会话页「继续」：同一条 agent session 已经开着就直接跳过去，否则建
    /// 一个空的运行时投影并带上 `history_session_id`。激活后由 `session/load` 让
    /// agent 重放历史，Smelt 不再从各家私有 transcript 预填第二份消息快照。
    pub fn resume_acp_session(
        &mut self,
        agent: settings::AcpAgentKind,
        launch_override: Option<smelt_core::agent_kind::AcpLaunchSpec>,
        profile_id: Option<String>,
        cwd: String,
        resume_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 已经开着的必须是**这一条**历史会话（比对 agent + cwd + 具体 session id），
        // 不能只认 agent+cwd——同项目同 agent 可能同时开着好几条不同的历史会话，
        // 之前只按 agent+cwd 找，点哪条「继续」都会跳到"第一个凑巧匹配的"那条。
        let target_id = agent_client_protocol::schema::v1::SessionId::new(resume_id.clone());
        if let Some(ix) = self.find_open_acp_session(agent, &cwd, &target_id, cx) {
            let view = match &self.sessions[ix].kind {
                SessionKind::Acp(view) => view.clone(),
                _ => unreachable!("find_open_acp_session only returns ACP sessions"),
            };
            self.activate(ix, window, cx);
            // “继续”是一次显式恢复请求，不能只激活可能已经断线、内容为空的旧
            // View。重新 attach 会让 smeltd 发送 offset=0 的完整权威快照。
            view.update(cx, |view, cx| view.reattach_to_daemon(window, cx));
            return;
        }

        let launch = launch_override.unwrap_or_else(|| {
            smelt_core::agent_kind::AcpLaunchSpec::from_command(settings::acp_cmd_for(agent, cx))
        });
        let view = cx.new(|cx| {
            acp_view::AcpView::placeholder(
                cx,
                agent,
                launch,
                profile_id.is_none(),
                profile_id,
                Some(cwd),
                "正在加载历史会话…".to_string(),
                Vec::new(),
                Some(target_id),
                // 新起 smeltd 托管连接，靠 agent session id 做 session/load；
                // 它不是已存在的守护会话，因此不能沿用 smeltd id。
                None,
            )
        });
        let _acp_persist_sub = Some(self.subscribe_acp_persist(&view, window, cx));
        self.sessions.push(Session {
            ui_id: next_session_ui_id(),
            kind: SessionKind::Acp(view),
            custom_title: None,
            _acp_persist_sub,
            ui_state: SessionUiState::default(),
        });
        self.session_list_revision = self.session_list_revision.wrapping_add(1);
        let ix = self.sessions.len() - 1;
        self.activate(ix, window, cx);
        self.save_state(cx);
    }

    /// 项目行「+」下拉菜单的快捷入口：`launch` 编进 shell 的启动命令行（见
    /// terminal.rs::spawn / smeltd.rs::spawn_session），`label` 用作侧栏初始显示名。
    ///
    /// **禁止**在 UI/`update`/拖放 FFI 回调里同步 `Terminal::spawn`：连守护 + 握手
    /// 含 sleep/超时，拖文件夹进窗口会整窗 beachball（见 `confirm_restart_daemon`）。
    /// 专用 OS 线程做阻塞 spawn，主线程只接结果建 Entity（比塞进 async executor 更稳）。
    fn add_session_with_launch(
        &mut self,
        cwd: Option<String>,
        entry: Option<settings::LaunchEntry>,
        cx: &mut Context<Self>,
    ) {
        self.remember_session_project(cwd.as_deref());
        // spawn 在后台；先把项目落盘，即使进程启动失败也不能让用户刚选中的项目消失。
        self.save_state(cx);
        let sid = new_sid();
        let cwd_bg = cwd.clone();
        let launch_owned = entry.as_ref().map(|entry| entry.command.clone());
        let label_owned = entry.as_ref().map(|entry| entry.label.clone());
        let sid_bg = sid.clone();
        let launch_bg = launch_owned.clone();
        eprintln!("[workspace] 新建会话 cwd={cwd:?} launch={launch_owned:?} sid={sid}");
        cx.notify();

        let (tx, rx) = smol::channel::bounded(1);
        std::thread::Builder::new()
            .name("smelt-spawn-session".into())
            .spawn(move || {
                let r = terminal::Terminal::spawn(
                    24,
                    80,
                    cwd_bg.as_deref(),
                    &sid_bg,
                    launch_bg.as_deref(),
                );
                let _ = tx.send_blocking(r);
            })
            .expect("spawn smelt-spawn-session 线程");

        cx.spawn(async move |this, cx| {
            let result = match rx.recv().await {
                Ok(r) => r,
                Err(_) => {
                    let _ = this.update(cx, |this, cx| {
                        this.background_error = Some("新建会话内部通道断开，请重试".into());
                        cx.notify();
                    });
                    return;
                }
            };
            let terminal = match result {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("[workspace] 新建会话失败（{cwd:?}）：{e:#}");
                    let msg = format!("新建会话失败：{e:#}");
                    let _ = this.update(cx, |this, cx| {
                        this.background_error = Some(msg);
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                let view = cx.new(|cx| {
                    TerminalView::from_terminal(
                        cx,
                        terminal,
                        cwd.clone(),
                        sid,
                        launch_owned.as_deref(),
                        label_owned.as_deref(),
                    )
                });
                this.sessions.push(Session::single(view.clone()));
                this.session_list_revision = this.session_list_revision.wrapping_add(1);
                this.active_session = this.sessions.len() - 1;
                this.save_state(cx);
                eprintln!(
                    "[workspace] 新建会话成功，当前共 {} 个",
                    this.sessions.len()
                );
                cx.notify();
            });
        })
        .detach();
    }

    /// 在当前会话的活动 pane 上分屏：Horizontal=右侧并排，Vertical=下方堆叠。
    /// ACP 会话没有分屏树，直接忽略。
    fn split_active(&mut self, axis: Axis, cx: &mut Context<Self>) {
        let Some(sess) = self.cur() else { return };
        let Some(active) = sess.active_term() else {
            return;
        };
        let cwd = active.read(cx).cwd().or_else(current_dir);
        let old = sess.anchor_id();
        let session_ix = self.active_session;
        let sid = new_sid();
        let cwd_bg = cwd.clone();
        let sid_bg = sid.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    terminal::Terminal::spawn(24, 80, cwd_bg.as_deref(), &sid_bg, None)
                })
                .await;
            let terminal = match result {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("[workspace] 分屏失败（{cwd:?}）：{e:#}");
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                // 分屏目标会话可能在握手期间被关掉——对不上就丢弃这个终端。
                if session_ix >= this.sessions.len() {
                    eprintln!("[workspace] 分屏目标会话已不存在，丢弃");
                    return;
                }
                let view =
                    cx.new(|cx| TerminalView::from_terminal(cx, terminal, cwd, sid, None, None));
                let state = cx.new(|_| ResizableState::default());
                let sess = &mut this.sessions[session_ix];
                // old 叶子若已被拆掉/关掉，split_leaf 找不到就不动。
                let Some(layout) = sess.term_layout_mut() else {
                    eprintln!("[workspace] 分屏目标会话不是终端会话，丢弃");
                    return;
                };
                if !split_leaf(layout, old, axis, state, view.clone()) {
                    eprintln!("[workspace] 分屏目标 pane 已不存在，丢弃");
                    return;
                }
                sess.set_active_term(view);
                this.save_state(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 把所有会话（各自分屏树 + 活动叶子遍历序）+ 侧栏宽度 + 文件树列宽写入
    /// workspace.json（失败静默忽略）。
    fn save_state(&self, cx: &mut Context<Self>) {
        let Some(path) = ws_state_path() else { return };
        let menu = self.workspace_menu_snapshot(cx);
        let mut sessions: Vec<SessionState> = self
            .sessions
            .iter()
            .map(|s| {
                let route = if self.ui_session_id == Some(s.ui_id) {
                    &self.right_route
                } else {
                    &s.ui_state
                };
                match &s.kind {
                    SessionKind::Term { layout: l, .. } => {
                        let layout = pane_to_state(l, cx);
                        let mut ids = Vec::new();
                        collect_leaf_ids(l, &mut ids);
                        let active = ids.iter().position(|x| *x == s.anchor_id()).unwrap_or(0);
                        SessionState {
                            layout,
                            active,
                            custom_title: s.custom_title.clone(),
                            acp: None,
                            route: Some(route.archive()),
                        }
                    }
                    SessionKind::Acp(view) => {
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
                            custom_title: s.custom_title.clone(),
                            acp: Some(AcpSaved {
                                cwd: v.cwd(),
                                launch: v.launch_spec(),
                                profile_id: v.profile_id().map(str::to_string),
                                agent: Some(v.agent_kind().id().to_string()),
                                history_session_id: v.history_session_id_for_save(),
                                sid: Some(v.session_id().to_string()),
                                refresh_launch_from_settings: v.refresh_launch_from_settings(),
                                fork_origin: v.fork_origin(),
                            }),
                            route: Some(route.archive()),
                        }
                    }
                }
            })
            .collect();
        // 启动时恢复失败的会话按原位置写回，下次冷启动重试。
        sessions = merge_restore_orphans(sessions, &self.restore_orphans);

        // 安全阀：内存里一个会话都没有、也没有 orphan，但磁盘上还有旧存档 → 绝不
        // 用空列表覆盖（历史上「守护未就绪 → 恢复全失败 → save_state 抹盘」会把
        // 用户所有侧栏会话永久清掉）。
        //
        // 但「用户自己把会话全关了」是合法状态（项目实体化后侧栏还有项目撑着），
        // 那种情况必须允许写空，否则重启又全恢复回来。靠 sessions_restored 区分：
        // 恢复流程跑完之前为 false，此时的空 = 还没恢复上来，护住；跑完之后为 true，
        // 空就是用户真的关光了。
        if sessions.is_empty() && !self.sessions_restored {
            if let Some(existing) = load_ws_state() {
                let had = !existing.sessions.is_empty()
                    || existing.layout.is_some()
                    || !existing.tabs.is_empty();
                if had {
                    eprintln!(
                        "[workspace] 内存会话为空但磁盘存档有数据，跳过写盘以免抹掉 workspace.json"
                    );
                    return;
                }
            }
        }

        let file_tree_w = self
            .file_tree_resize
            .read(cx)
            .sizes()
            .first()
            .copied()
            .map(f32::from);
        let state = WsState {
            sessions,
            projects: self.projects.clone(),
            menu,
            active_session: persisted_active_position(
                self.active_session,
                &self.restore_orphans,
                self.sessions_restored,
            ),
            route: self.primary_route,
            sidebar_w: Some(self.sidebar_w),
            sidebar_open: Some(self.sidebar_open),
            inspector_w: Some(self.inspector_w),
            inspector_open: Some(self.inspector_open),
            file_tree_w,
            pinned_file_tree_roots: self.pinned_roots.clone(),
            collapsed_file_tree_roots: self.collapsed_roots.iter().cloned().collect(),
            collapsed_projects: self.collapsed_projects.iter().cloned().collect(),
            sidebar_grouping: self.sidebar_grouping,
            ..Default::default()
        };
        if let Ok(json) = serde_json::to_string_pretty(&state) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, json);
        }
    }

    /// 把 PC 当前真实侧栏投影成无 UI 的共享菜单。项目标签使用 project_groups 的
    /// 消歧结果，会话标题直接使用 Session::title，因此移动端无需复制任何显示规则。
    fn workspace_menu_snapshot(
        &self,
        cx: &App,
    ) -> smelt_core::workspace_menu::WorkspaceMenuSnapshot {
        use smelt_core::workspace_menu::{
            WorkspaceMenuProject, WorkspaceMenuSession, WorkspaceMenuSessionKind,
            WorkspaceMenuSnapshot,
        };

        let groups = self.project_groups(cx);
        let projects = groups
            .iter()
            .enumerate()
            .map(|(order, group)| WorkspaceMenuProject {
                root: group.root.clone(),
                title: group.label.clone(),
                order: order.min(u32::MAX as usize) as u32,
            })
            .collect();
        let mut membership = vec![None; self.sessions.len()];
        for (project_order, group) in groups.iter().enumerate() {
            for &session_index in &group.sessions {
                if let Some(slot) = membership.get_mut(session_index) {
                    *slot = Some((project_order, group));
                }
            }
        }

        let sessions = self
            .sessions
            .iter()
            .enumerate()
            .filter_map(|(session_order, session)| {
                let (id, kind, agent) = match &session.kind {
                    SessionKind::Term { active, .. } => (
                        active.read(cx).session_id().to_string(),
                        WorkspaceMenuSessionKind::Terminal,
                        None,
                    ),
                    SessionKind::Acp(view) => {
                        let view = view.read(cx);
                        (
                            view.session_id().to_string(),
                            WorkspaceMenuSessionKind::Acp,
                            Some(view.agent_kind().id().to_string()),
                        )
                    }
                };
                let project = membership.get(session_order).and_then(|value| *value);
                Some(WorkspaceMenuSession {
                    id,
                    kind,
                    title: session.title(cx),
                    custom_title: session.custom_title.is_some(),
                    cwd: session.cwd(cx),
                    project_root: project.map(|(_, group)| group.root.clone()),
                    project_title: project.map(|(_, group)| group.label.clone()),
                    project_order: project
                        .map(|(order, _)| order.min(u32::MAX as usize) as u32)
                        .unwrap_or(u32::MAX),
                    session_order: session_order.min(u32::MAX as usize) as u32,
                    agent,
                })
            })
            .collect();

        WorkspaceMenuSnapshot::current(projects, sessions)
    }

    /// 「+」新建会话：继承当前会话活动终端的目录。
    fn new_tab(&mut self, cx: &mut Context<Self>) {
        let cwd = self.cur().and_then(|s| s.cwd(cx)).or_else(current_dir);
        self.add_session(cwd, cx);
    }

    /// 顶栏「底部抽屉」开关：展开时若还没有任何标签就补一个默认终端标签
    /// （跟旧「终端」按钮语义一样），收起只是隐藏——已开的标签（含终端进程）
    /// 都留着，下次展开还在原地。这样它不进 self.sessions/侧栏项目列表，纯粹
    /// 是一个独立于会话舞台之外的快捷面板（VS Code 底部面板那种）。
    fn toggle_bottom_drawer(&mut self, cx: &mut Context<Self>) {
        self.bottom_drawer_open = !self.bottom_drawer_open;
        self.bottom_drawer_transition
            .set_open(self.bottom_drawer_open);
        if self.bottom_drawer_open && self.bottom_drawer_tabs.is_empty() {
            self.add_bottom_drawer_terminal(cx);
        }
        self.save_state(cx);
        cx.notify();
    }

    /// 「+」新建一个底部终端标签，并切到它。
    fn add_bottom_drawer_terminal(&mut self, cx: &mut Context<Self>) {
        let id = self.bottom_drawer_next_id;
        self.bottom_drawer_next_id += 1;
        self.bottom_drawer_tabs.push(DrawerTab {
            id,
            terminal: None,
            spawning: false,
        });
        self.bottom_drawer_active = self.bottom_drawer_tabs.len() - 1;
        self.spawn_bottom_drawer_terminal(id, cx);
        if !self.bottom_drawer_open {
            self.bottom_drawer_open = true;
            self.bottom_drawer_transition.set_open(true);
        }
        self.save_state(cx);
        cx.notify();
    }

    /// 关掉一个标签：终端标签只是从抽屉里摘掉（不额外杀进程——smeltd 那边照常
    /// 跑，跟主会话终端关闭策略保持一致，这里的抽屉终端本来就是"轻量、用完即弃"
    /// 的临时终端，没有另外持久化到侧栏，摘掉即彻底释放）。摘掉最后一个标签时
    /// 顺手把整个抽屉收起来，省得留一个空面板。
    fn close_bottom_drawer_tab(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        let Some(ix) = self.bottom_drawer_tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        self.bottom_drawer_tabs.remove(ix);
        if self.bottom_drawer_tabs.is_empty() {
            self.bottom_drawer_open = false;
            self.bottom_drawer_transition.set_open(false);
            self.bottom_drawer_active = 0;
        } else if self.bottom_drawer_active >= self.bottom_drawer_tabs.len() {
            self.bottom_drawer_active = self.bottom_drawer_tabs.len() - 1;
        } else if ix < self.bottom_drawer_active {
            self.bottom_drawer_active -= 1;
        }
        self.save_state(cx);
        cx.notify();
    }

    fn spawn_bottom_drawer_terminal(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        if let Some(tab) = self.bottom_drawer_tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.spawning = true;
        }
        // 底部终端属于当前 session 的右侧 route，启动目录也应跟随该 session：
        // 优先项目根，隐式/尚未分组的会话退回自身 cwd，最后才落到 HOME。
        let cwd = self
            .active_project_root(cx)
            .or_else(|| self.cur().and_then(|session| session.cwd(cx)))
            .or_else(scratch_dir);
        let sid = new_sid();
        let cwd_bg = cwd.clone();
        let sid_bg = sid.clone();
        let (tx, rx) = smol::channel::bounded(1);
        std::thread::Builder::new()
            .name("smelt-bottom-drawer".into())
            .spawn(move || {
                let r = terminal::Terminal::spawn(24, 80, cwd_bg.as_deref(), &sid_bg, None);
                let _ = tx.send_blocking(r);
            })
            .expect("spawn smelt-bottom-drawer 线程");

        cx.spawn(async move |this, cx| {
            let result = match rx.recv().await {
                Ok(r) => r,
                Err(_) => {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(tab) =
                            this.bottom_drawer_tabs.iter_mut().find(|t| t.id == tab_id)
                        {
                            tab.spawning = false;
                        }
                        this.background_error = Some("底部终端内部通道断开，请重试".into());
                        cx.notify();
                    });
                    return;
                }
            };
            let terminal = match result {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("[workspace] 底部终端 spawn 失败：{e:#}");
                    let _ = this.update(cx, |this, cx| {
                        if let Some(tab) =
                            this.bottom_drawer_tabs.iter_mut().find(|t| t.id == tab_id)
                        {
                            tab.spawning = false;
                        }
                        this.background_error = Some(format!("底部终端启动失败：{e:#}"));
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                let view =
                    cx.new(|cx| TerminalView::from_terminal(cx, terminal, cwd, sid, None, None));
                if let Some(tab) = this.bottom_drawer_tabs.iter_mut().find(|t| t.id == tab_id) {
                    tab.terminal = Some(view);
                    tab.spawning = false;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 底部抽屉：停靠面板，跟三栏区域同层参与 flex_col 布局——展开时真实占用
    /// 高度，把上面内容顶起来（不是浮在内容上面的悬浮卡片）。头部是真正的多
    /// 标签页签条（参考 Codex 底部面板 + VS Code 终端面板）：终端可以同时开好几个，
    /// 「+」直接新建终端，页签条最右边另有一个跟标签无关
    /// 的「X」——收起整个抽屉（区别于每个标签自己的关闭 x，那个只关一个标签）。
    fn render_bottom_drawer(
        &mut self,
        mounted: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !mounted {
            return None;
        }
        let active_ix = self
            .bottom_drawer_active
            .min(self.bottom_drawer_tabs.len().saturating_sub(1));
        let active_tab = self.bottom_drawer_tabs.get(active_ix).map(|t| t.id);

        let body: AnyElement = match active_tab {
            Some(id) => {
                let tab = self.bottom_drawer_tabs.iter().find(|t| t.id == id);
                if let Some(view) = tab.and_then(|t| t.terminal.clone()) {
                    view.into_any_element()
                } else {
                    let spawning = tab.is_some_and(|t| t.spawning);
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(rgb(ui_theme::text_faint()))
                        .child(if spawning {
                            "终端启动中…"
                        } else {
                            "终端未就绪"
                        })
                        .into_any_element()
                }
            }
            None => div().flex_1().into_any_element(),
        };

        // 页签条：每个标签自己的图标/文案/关闭 x；后面跟一个「+」新建菜单；
        // 再用一个 flex_1 撑开空隙，把「收起整个抽屉」的 X 顶到最右边。
        let mut tab_bar = div()
            .flex_shrink_0()
            .h(px(32.))
            .flex()
            .items_center()
            .px(px(6.))
            .gap_1()
            .bg(rgb(ui_theme::bg_rail()))
            .border_b_1()
            .border_color(rgb(ui_theme::border_dim()));

        for (ix, tab) in self.bottom_drawer_tabs.iter().enumerate() {
            let tab_id = tab.id;
            let active = ix == active_ix;
            let e_select = cx.entity();
            let e_close = cx.entity();
            tab_bar = tab_bar.child(
                div()
                    .id(("bottom-drawer-tab", tab_id as usize))
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(26.))
                    .px_2()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(active, |s| {
                        s.bg(ui_theme::glass_card())
                            .border_t_2()
                            .border_color(rgb(ui_theme::accent()))
                    })
                    .when(!active, |s| s.hover(|s| s.bg(ui_theme::overlay(0x10))))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        cx.stop_propagation();
                        e_select.update(cx, |this, cx| {
                            this.bottom_drawer_active = ix;
                            cx.notify();
                        });
                    })
                    .child(
                        Icon::new(IconName::SquareTerminal)
                            .size(px(12.))
                            .text_color(rgb(ui_theme::text_mid())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(if active {
                                ui_theme::text_bright()
                            } else {
                                ui_theme::text_mid()
                            }))
                            .child("终端"),
                    )
                    .child(
                        div()
                            .id(("bottom-drawer-tab-close", tab_id as usize))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(14.))
                            .rounded(px(3.))
                            .cursor_pointer()
                            .text_color(rgb(ui_theme::text_faint()))
                            .hover(|s| {
                                s.bg(ui_theme::overlay(0x18))
                                    .text_color(rgb(ui_theme::text_bright()))
                            })
                            .child(Icon::new(IconName::Close).size(px(10.)))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                cx.stop_propagation();
                                e_close.update(cx, |this, cx| {
                                    this.close_bottom_drawer_tab(tab_id, cx);
                                });
                            }),
                    ),
            );
        }

        // 底部只支持终端，「+」直接新建，不再弹出与右侧 Inspector 重复的文件/Git 菜单。
        let e_add = cx.entity();
        let add_button = Button::new("bottom-drawer-add")
            .ghost()
            .xsmall()
            .icon(IconName::Plus)
            .tooltip("新建终端")
            .on_click(move |_, _, cx| {
                e_add.update(cx, |this, cx| {
                    this.add_bottom_drawer_terminal(cx);
                });
            });

        let e_close_all = cx.entity();
        tab_bar = tab_bar.child(add_button).child(div().flex_1()).child(
            div()
                .id("bottom-drawer-close-all")
                .flex()
                .items_center()
                .justify_center()
                .size(px(22.))
                .rounded(px(5.))
                .cursor_pointer()
                .text_color(rgb(ui_theme::text_faint()))
                .hover(|s| {
                    s.bg(ui_theme::overlay(0x18))
                        .text_color(rgb(ui_theme::text_bright()))
                })
                .child(Icon::new(IconName::Close).size(px(13.)))
                .tooltip(|window, cx| {
                    gpui_component::tooltip::Tooltip::new("收起底部面板").build(window, cx)
                })
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    cx.stop_propagation();
                    e_close_all.update(cx, |this, cx| {
                        this.bottom_drawer_open = false;
                        this.bottom_drawer_transition.set_open(false);
                        this.save_state(cx);
                        cx.notify();
                    });
                }),
        );

        Some(
            div()
                .size_full()
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(rgb(ui_theme::border_mid()))
                .bg(ui_theme::glass_card())
                .overflow_hidden()
                .child(tab_bar)
                .child(div().flex_1().min_h_0().flex().child(body))
                .into_any_element(),
        )
    }

    /// 把一个目录加进项目列表并设为活动项目。已经在列表里就只切活动，不重复加。
    /// **不建会话**——项目和会话是两回事，建会话是调用方另外的事。
    fn add_project(&mut self, root: String, cx: &mut Context<Self>) {
        if root.is_empty() {
            return;
        }
        let root = root.trim_end_matches('/').to_string();
        if !self
            .projects
            .iter()
            .any(|p| p.trim_end_matches('/') == root)
        {
            self.projects.push(root.clone());
        }
        self.active_project = Some(root);
        // 打开项目 = 想看这个项目，收掉盖在舞台上的覆盖页。
        self.stage_override = None;
        self.save_state(cx);
        cx.notify();
    }

    /// 侧栏右键「关闭项目」：底下还有会话就先弹确认——关项目会连带 kill 掉那些 shell，
    /// 正在跑的活儿就没了，且找不回来。空项目无损，直接关。
    /// `root` 是项目根路径（分组身份，见 ProjectGroup）。
    fn start_close_project(&mut self, root: String, cx: &mut Context<Self>) {
        let Some(g) = self
            .project_groups(cx)
            .into_iter()
            .find(|g| g.root.trim_end_matches('/') == root.trim_end_matches('/'))
        else {
            return;
        };
        if g.sessions.is_empty() {
            self.close_project(&root, cx);
        } else {
            self.close_project_target = Some((g.label, root, g.sessions.len()));
            cx.notify();
        }
    }

    fn cancel_close_project(&mut self, cx: &mut Context<Self>) {
        self.close_project_target = None;
        cx.notify();
    }

    fn confirm_close_project(&mut self, cx: &mut Context<Self>) {
        let Some((_, root, _)) = self.close_project_target.take() else {
            return;
        };
        self.close_project(&root, cx);
    }

    /// 「关闭项目」确认弹窗：说清会连带关掉几个会话（视觉同删 Worktree 那套危险配色）。
    fn render_close_project_confirm(&self, cx: &mut Context<Self>) -> Div {
        let Some((label, _, n)) = self.close_project_target.clone() else {
            return div();
        };
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);

        let content = v_flex()
            .child(div().font_bold().text_color(fg).text_lg().child("确定关闭这个项目吗？"))
            .child(div().text_sm().text_color(muted).child(format!(
                "「{label}」下的 {n} 个会话会被一起关掉，终端里正在跑的东西会被终止。项目本身只是从工作台移走，磁盘上的目录不动。"
            )))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-close-project",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_close_project(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-close-project",
                        "关闭项目",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_close_project(cx),
                        cx,
                    )),
            );
        Self::modal_shell(380., true, content, cx)
    }

    /// 关闭项目：从列表移除，并连带关掉挂在它下面的所有会话。
    /// 认 **root 路径**（分组身份，见 ProjectGroup）——用显示名的话，末段同名的另一个
    /// 项目会被一起误伤。
    fn close_project(&mut self, root: &str, cx: &mut Context<Self>) {
        let root = root.trim_end_matches('/').to_string();
        let Some(g) = self
            .project_groups(cx)
            .into_iter()
            .find(|g| g.root.trim_end_matches('/') == root)
        else {
            return;
        };
        // 降序关闭：前面的下标不受后面 remove 影响。
        let mut ixs = g.sessions;
        ixs.sort_unstable_by(|a, b| b.cmp(a));
        for ix in ixs {
            self.close_session(ix, cx);
        }
        self.projects.retain(|p| p.trim_end_matches('/') != root);
        if self
            .active_project
            .as_deref()
            .map(|p| p.trim_end_matches('/'))
            == Some(root.as_str())
        {
            self.active_project = None;
        }
        self.collapsed_projects.remove(&root);
        self.save_state(cx);
        cx.notify();
    }

    /// 「打开项目」：弹原生选择框选一个目录，加进项目列表。**不自动建会话**——
    /// 打开项目只是把它放上工作台，要开终端还是开对话由分组行的「+」决定。
    fn open_project(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择项目目录".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(dir) = paths.into_iter().next() {
                    if let Some(dir) = dir.to_str().map(String::from) {
                        this.update(cx, |this, cx| this.add_project(dir, cx)).ok();
                    }
                }
            }
        })
        .detach();
    }

    /// 从 Finder 拖入的路径 = 打开项目：文件夹直接用，文件取其父目录，各加一条项目。
    /// **不自动建会话**——跟「+ 打开项目」同一套语义，开终端还是开对话由项目行的
    /// 「+」决定（两条路结果不一致的话，"打开项目"到底会发生什么就说不清了）。
    ///
    /// 路径判定（is_dir 要 stat）仍丢后台：`on_drop` / `on_open_urls` 在 ObjC FFI 栈
    /// 上，拖一大把文件时同步 stat 会把窗口卡成 beachball。
    fn open_paths(&mut self, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        if paths.is_empty() {
            eprintln!("[workspace] open_paths: 空路径列表，忽略");
            return;
        }
        eprintln!(
            "[workspace] open_paths: 收到 {} 条路径 {:?}",
            paths.len(),
            paths
        );
        self.stage_override = None;
        cx.notify();

        let paths: Vec<std::path::PathBuf> = paths.to_vec();
        let (tx, rx) = smol::channel::bounded(1);
        std::thread::Builder::new()
            .name("smelt-open-paths".into())
            .spawn(move || {
                let mut out: Vec<String> = Vec::with_capacity(paths.len());
                for p in paths {
                    let dir = if p.is_dir() {
                        p
                    } else {
                        match p.parent() {
                            Some(parent) => parent.to_path_buf(),
                            None => continue,
                        }
                    };
                    let Some(cwd) = dir.to_str().map(str::to_string) else {
                        continue;
                    };
                    // 一次拖进同目录的一堆文件 → 只加一条项目。
                    if !out.contains(&cwd) {
                        out.push(cwd);
                    }
                }
                let _ = tx.send_blocking(out);
            })
            .expect("spawn smelt-open-paths 线程");

        cx.spawn(async move |this, cx| {
            let dirs = match rx.recv().await {
                Ok(v) => v,
                Err(_) => {
                    let _ = this.update(cx, |this, cx| {
                        this.background_error = Some("打开路径内部通道断开".into());
                        cx.notify();
                    });
                    return;
                }
            };

            let _ = this.update(cx, |this, cx| {
                if dirs.is_empty() {
                    this.background_error = Some("拖入的路径无法作为项目目录打开".into());
                } else {
                    for dir in dirs {
                        this.add_project(dir, cx);
                    }
                }

                cx.notify();
            });
        })
        .detach();
    }

    /// 关闭第 ix 个会话。用户主动关 → 让守护杀掉这些 shell（区别于退出 GUI：
    /// 那时不杀，会话在 smeltd 里持久活着）。
    ///
    /// 允许关到一个会话都不剩：项目独立于会话存在，侧栏还有项目行撑着不会空白，
    /// 舞台落到「还没有会话」引导页。（以前硬性拒绝关最后一个，那是项目还没实体化、
    /// 关光就整个侧栏空掉的年代留下的保护。）
    fn close_session(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.sessions.len() {
            return;
        }
        // 兼容旧存档/旧创建路径留下的隐式项目：在移除最后一个能提供 cwd 的会话前，
        // 先把它对应的项目实体化。这样“关闭会话”和“关闭项目”始终是两件事。
        let project_root = self
            .project_groups(cx)
            .into_iter()
            .find(|g| g.sessions.contains(&ix))
            .map(|g| g.root);
        self.remember_session_project(project_root.as_deref());
        let attention_ids: Vec<String> = match &self.sessions[ix].kind {
            SessionKind::Term { .. } => self.sessions[ix]
                .term_leaves()
                .iter()
                .map(|t| t.read(cx).session_id().to_string())
                .collect(),
            SessionKind::Acp(view) => vec![view.read(cx).session_id().to_string()],
        };
        for t in &self.sessions[ix].term_leaves() {
            terminal::kill_remote(t.read(cx).session_id());
        }
        if let SessionKind::Acp(view) = &self.sessions[ix].kind {
            view.update(cx, |v, cx| v.shutdown(cx));
        }
        if let Some(store) = cx.try_global::<AttentionGlobal>() {
            let mut store = store.0.lock().unwrap();
            for id in attention_ids {
                store.remove_session(&id);
            }
        }
        self.sessions.remove(ix);
        self.session_list_revision = self.session_list_revision.wrapping_add(1);
        // 空列表时 active_session 归 0（各处都是 sessions.get(ix) 取，取不到就是无会话态）。
        if self.sessions.is_empty() {
            self.active_session = 0;
        } else if self.active_session >= self.sessions.len() {
            self.active_session = self.sessions.len() - 1;
        } else if self.active_session > ix {
            self.active_session -= 1;
        }
        self.save_state(cx);
        cx.notify();
    }

    /// 删 worktree 前先清掉 cwd 落在 `path`（或它子目录）下的所有会话，不然会留下
    /// 指向即将被删除目录的死会话。顺带把这个目录从项目列表里摘掉——目录都要没了，
    /// 留一条指向不存在路径的项目没有意义。
    fn close_sessions_under(&mut self, path: &str, cx: &mut Context<Self>) {
        let path = path.trim_end_matches('/').to_string();
        if !self
            .cancelled_restore_paths
            .iter()
            .any(|existing| existing == &path)
        {
            self.cancelled_restore_paths.push(path.clone());
        }
        self.restore_orphans.retain(|(_, session)| {
            !restore_path_is_cancelled(
                session_state_cwd(session).as_deref(),
                std::slice::from_ref(&path),
            )
        });
        let prefix = format!("{path}/");
        let mut ixs: Vec<usize> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let cwd = s.cwd(cx).unwrap_or_default();
                cwd == path || cwd.starts_with(&prefix)
            })
            .map(|(ix, _)| ix)
            .collect();
        // 降序关闭：前面的下标不受后面 remove 影响（同 move_project_near 的做法）。
        ixs.sort_unstable_by(|a, b| b.cmp(a));
        for ix in ixs {
            self.close_session(ix, cx);
        }
        remove_projects_under(&mut self.projects, &path);
        self.save_state(cx);
        cx.notify();
    }

    /// 关掉第 ix 个会话里的指定 pane：会话内还有别的 pane 就只拆这一个（守护真正杀掉
    /// 这个 shell，剩下的 pane 不受影响），只剩它一个时才退化成关整个会话。
    /// 侧栏 pane 行的 × 和 Cmd+W 都走这里。
    fn close_session_pane(
        &mut self,
        ix: usize,
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = view.entity_id();
        let Some((count, was_active)) = self
            .sessions
            .get(ix)
            .map(|s| (s.pane_count(), s.anchor_id() == target))
        else {
            return;
        };
        if count <= 1 {
            self.close_session(ix, cx);
            self.focus_active(window, cx);
            return;
        }
        // 用户主动关 pane → 守护真正杀掉该 shell（区别于退出 GUI：那时保活）。
        let closed_session_id = view.read(cx).session_id().to_string();
        terminal::kill_remote(&closed_session_id);
        if let Some(store) = cx.try_global::<AttentionGlobal>() {
            store.0.lock().unwrap().remove_session(&closed_session_id);
        }
        let sess = &mut self.sessions[ix];
        if let Some(layout) = sess.term_layout_mut() {
            remove_leaf(layout, target);
        }
        // 关掉的正是活动 pane 才需要改指向，关别的 pane 时当前视图不该跳走。
        if was_active {
            if let Some(first) = sess.term_leaves().first().cloned() {
                sess.set_active_term(first);
            }
        }
        self.focus_active(window, cx);
        self.save_state(cx);
        cx.notify();
    }

    /// Cmd+W：会话内多 pane 时关掉活动 pane（切到相邻），否则关整个会话。
    fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.cur().and_then(|s| s.active_term().cloned()) {
            Some(view) => self.close_session_pane(self.active_session, view, window, cx),
            // ACP 会话没有分屏树，只能整个关。
            None => {
                self.close_session(self.active_session, cx);
                self.focus_active(window, cx);
            }
        }
    }

    /// 点击 pane：把它设为当前会话的活动 pane 并聚焦（不换会话）。
    fn activate_pane(
        &mut self,
        e: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(sess) = self.sessions.get_mut(self.active_session) {
            sess.set_active_term(e.clone());
        }
        e.update(cx, |terminal, cx| terminal.mark_read(cx));
        let h = e.read(cx).focus_handle();
        window.focus(&h, cx);
        self.save_state(cx);
        cx.notify();
    }

    /// 聚焦当前会话的活动终端（ACP 会话的聚焦走视图自身，这里跳过）。
    /// 设置/收回舞台覆盖页并处理焦点：全屏页自己没有可聚焦元素，焦点认领到根
    /// 让全局快捷键仍收得到；收回时把焦点还给活动会话（终端 pane / ACP 输入框）。
    pub(crate) fn set_stage_override(
        &mut self,
        v: Option<MainView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if v.is_some() {
            self.primary_route = WorkspaceRoute::Session;
        }
        self.stage_override = v;
        match v {
            Some(_) => window.focus(&self.focus_handle, cx),
            None => self.focus_active_stage(window, cx),
        }
        cx.notify();
    }

    pub(crate) fn activate_tasks_route(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.primary_route = WorkspaceRoute::Tasks;
        window.focus(&self.focus_handle, cx);
        self.save_state(cx);
        cx.notify();
    }

    /// 焦点还给活动会话：Term → 活动 pane；ACP → 消息流输入框。
    fn focus_active_stage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Session {
            kind: SessionKind::Acp(view),
            ..
        }) = self.sessions.get(self.active_session)
        {
            let view = view.clone();
            view.update(cx, |v, cx| v.focus_input(window, cx));
        } else {
            self.focus_active(window, cx);
        }
    }

    fn focus_active(&self, window: &mut Window, cx: &mut App) {
        if let Some(active) = self.cur().and_then(|s| s.active_term()) {
            let h = active.read(cx).focus_handle();
            window.focus(&h, cx);
        }
    }

    /// 侧栏展开会话看到的分屏子行：点击某个 pane → 切到它所在会话，并把该 pane
    /// 设为会话内的活动 pane（分屏树本身不变，只是换了「当前看哪个」）。
    fn activate_session_pane(
        &mut self,
        ix: usize,
        pane: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate(ix, window, cx);
        self.activate_pane(&pane, window, cx);
    }

    /// 切换到第 ix 个会话并聚焦。
    /// 按侧栏「分组展平后的视觉顺序」切上/下一个会话（delta=-1/1），到头循环。
    /// 快捷键 cmd-up / cmd-down；顺序跟眼睛看到的一致（跨项目一路顺下去），
    /// 不是 self.sessions 的数组序——那个跟侧栏显示序未必一致，切起来会乱跳。
    fn cycle_session(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let statuses = self
            .sessions
            .iter()
            .map(|session| session.status(cx))
            .collect::<Vec<_>>();
        let order: Vec<usize> = sidebar_groups(
            self.sidebar_grouping,
            self.project_groups(cx),
            &statuses,
            self.sessions.len(),
        )
        .iter()
        .flat_map(|g| g.sessions.iter().copied())
        .collect();
        if order.is_empty() {
            return;
        }
        let cur = order
            .iter()
            .position(|&ix| ix == self.active_session)
            .unwrap_or(0);
        let n = order.len() as isize;
        let next = (cur as isize + delta).rem_euclid(n) as usize;
        self.activate(order[next], window, cx);
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.sessions.len() {
            self.active_session_revision = self.active_session_revision.wrapping_add(1);
            self.active_session = ix;
            // 活动项目跟着活动会话走：切到别的项目的会话时侧栏高亮同步换过去。
            if let Some(g) = self
                .project_groups(cx)
                .into_iter()
                .find(|g| g.sessions.contains(&ix))
            {
                self.active_project = Some(g.root);
            }
            self.sync_session_ui(window, cx);
            self.primary_route = WorkspaceRoute::Session;
            let route_has_overlay = self.stage_override.is_some();
            // 切到会话即视为看过一次性通知；结构化等待状态仍由 daemon phase 保留。
            if let SessionKind::Acp(view) = &self.sessions[ix].kind {
                view.update(cx, |v, cx| {
                    // 冷恢复占位第一次被切到 → 自动启动（免手点「重新开始」）。
                    if should_auto_resume_active_acp(self.sessions_restored) {
                        v.maybe_auto_resume(window, cx);
                    }
                    v.mark_read(cx);
                    if !route_has_overlay {
                        v.focus_input(window, cx);
                    }
                });
            } else {
                if let Some(view) = self.sessions[ix].active_term().cloned() {
                    view.update(cx, |terminal, cx| terminal.mark_read(cx));
                }
                if !route_has_overlay {
                    self.focus_active(window, cx);
                }
            }
            if route_has_overlay {
                window.focus(&self.focus_handle, cx);
            }
            self.save_state(cx);
            cx.notify();
        }
    }

    fn next_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.sessions.len();
        if n > 0 {
            self.activate((self.active_session + 1) % n, window, cx);
        }
    }

    /// 侧栏右键「强制重启」：ACP 会话卡死（`session/cancel` 打不断正在跑的
    /// 工具调用）时的兜底，见 `AcpView::force_restart` 注释。非 ACP 会话
    /// （`active_acp` 返回 None）是 no-op，调用方（右键菜单）也只在 ACP
    /// 会话上才显示这一项。
    pub(crate) fn force_restart_acp_session(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(view) = self.sessions.get(ix).and_then(|s| s.active_acp()).cloned() {
            view.update(cx, |v, cx| v.force_restart(cx));
        }
    }

    fn prev_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.sessions.len();
        if n > 0 {
            self.activate((self.active_session + n - 1) % n, window, cx);
        }
    }

    /// Cmd+[ / Cmd+] 在当前会话的分屏树里循环切换活动 pane（对齐 iTerm2 默认键位：
    /// 这两个键管「同一会话内切哪个格子」，会话本身的切换交给 Cmd+1~9）。
    /// 只有一个 pane（没分屏）时什么都不做。
    fn cycle_pane(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sess) = self.cur() else { return };
        let leaves = sess.term_leaves();
        if leaves.len() < 2 {
            return;
        }
        let cur_id = sess.anchor_id();
        let Some(ix) = leaves.iter().position(|l| l.entity_id() == cur_id) else {
            return;
        };
        let n = leaves.len() as i32;
        let next = (ix as i32 + delta).rem_euclid(n) as usize;
        let target = leaves[next].clone();
        self.activate_pane(&target, window, cx);
    }

    /// 侧栏右键「重命名」：弹出文本框，预填当前标题。回车 / 点「确定」提交，见
    /// `confirm_rename`；提交前的输入放在独立的 rename_input，不影响目标对象
    /// 本身，点「取消」（走 cancel_rename）就等于什么都没发生。
    ///
    /// 注意：这里故意不监听 `InputEvent::Blur` 去自动提交——点「取消」按钮本身会先
    /// 让输入框失焦，若失焦也提交，「取消」就会在关闭前先把文本框里的内容存下来，
    /// 跟按钮的字面意思相反。所以提交只认 Enter 或显式点「确定」。
    fn start_rename(&mut self, target: RenameTarget, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::{InputEvent, InputState};
        let current = match &target {
            RenameTarget::Session(ix) => {
                let Some(s) = self.sessions.get(*ix) else {
                    return;
                };
                s.title(cx)
            }
            RenameTarget::Pane(view) => pane_title(view, cx),
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        input.update(cx, |s, cx| s.focus(window, cx));
        self._rename_sub = Some(cx.subscribe_in(
            &input,
            window,
            |this, _input, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.confirm_rename(window, cx);
                }
            },
        ));
        self.rename_target = Some(target);
        self.rename_input = Some(input);
        cx.notify();
    }

    /// 提交重命名：空输入等于清掉自定义名，回退到自动推导的标题。
    fn confirm_rename(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.rename_target.take() else {
            return;
        };
        let Some(input) = self.rename_input.take() else {
            return;
        };
        self._rename_sub = None;
        let text = input.read(cx).value().trim().to_string();
        match target {
            RenameTarget::Session(ix) => {
                if let Some(s) = self.sessions.get_mut(ix) {
                    s.custom_title = (!text.is_empty()).then_some(text);
                }
            }
            RenameTarget::Pane(view) => {
                view.update(cx, |t, _| t.set_custom_title(Some(text)));
            }
        }
        self.save_state(cx);
        cx.notify();
    }

    /// 取消重命名：不落地任何改动。
    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.rename_target = None;
        self.rename_input = None;
        self._rename_sub = None;
        cx.notify();
    }

    /// 设置入口与设置页共用它决定要不要显示更新提示。
    fn update_available(&self) -> bool {
        matches!(
            self.update_status,
            updater::UpdateStatus::Downloading { .. }
                | updater::UpdateStatus::Installing { .. }
                | updater::UpdateStatus::ReadyToInstall { .. }
        )
    }

    /// 检查是否有新版本。`silent` 区分启动时的后台静默检查（离线/失败时不打扰用户，
    /// 悄悄退回 Idle）和设置页手动点「检查更新」（失败要如实展示原因）。
    /// 发现新版本会直接接上后台静默下载，不需要用户二次确认——这是"全自动静默更新"承诺的一环。
    fn check_for_update(&mut self, silent: bool, cx: &mut Context<Self>) {
        self.update_status = updater::UpdateStatus::Checking;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    smelt_core::block_on::block_on_tokio(updater::fetch_latest()).and_then(|r| r)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok((version, url))
                        if updater::is_newer(&version, env!("CARGO_PKG_VERSION")) =>
                    {
                        this.start_update_download(version, url, cx);
                        return; // start_update_download 里已经 notify 过
                    }
                    Ok(_) => this.update_status = updater::UpdateStatus::UpToDate,
                    Err(e) => {
                        this.update_status = if silent {
                            updater::UpdateStatus::Idle
                        } else {
                            updater::UpdateStatus::Failed(e.to_string())
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 后台静默下载新版 dmg 并暂存好 `.app`，完成后置 `ReadyToInstall`（不重启、不打断）。
    /// 下载线程通过 channel 往回推字节进度，UI 线程照单刷新状态；发送端随下载任务结束而
    /// drop，`recv` 收到 Err 即代表下载收尾，此时再 `await` 任务拿最终结果。
    fn start_update_download(&mut self, version: String, url: String, cx: &mut Context<Self>) {
        self.update_status = updater::UpdateStatus::Downloading {
            version: version.clone(),
            received: 0,
            total: None,
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (tx, rx) = smol::channel::unbounded::<updater::DownloadProgress>();
            let v = version.clone();
            let task = cx.background_executor().spawn(async move {
                smelt_core::block_on::block_on_tokio(updater::download_and_stage(&url, &v, |p| {
                    let _ = tx.try_send(p);
                }))
                .and_then(|r| r)
            });

            while let Ok(progress) = rx.recv().await {
                let version = version.clone();
                let _ = this.update(cx, |this, cx| {
                    this.update_status = match progress {
                        updater::DownloadProgress::Bytes { received, total } => {
                            updater::UpdateStatus::Downloading {
                                version,
                                received,
                                total,
                            }
                        }
                        updater::DownloadProgress::Installing => {
                            updater::UpdateStatus::Installing { version }
                        }
                    };
                    cx.notify();
                });
            }

            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.update_status = match result {
                    Ok(staged_app) => updater::UpdateStatus::ReadyToInstall {
                        version,
                        staged_app,
                    },
                    Err(e) => updater::UpdateStatus::Failed(e.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// 后台查一次守护是否落后于磁盘上的 smeltd 二进制，决定要不要在设置页/齿轮上
    /// 给出「重启守护」提示。本地 Unix socket 往返很快，但仍走后台线程，跟
    /// check_for_update 同款结构，别在 UI 线程里做阻塞 IO。
    fn check_daemon_outdated(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            // 只探测状态，不再在此 ensure/handoff（冷启动 ensure 由 restore 线程串行做完）。
            // 仍落后则无缝升级到磁盘最新 smeltd。
            let (outdated, info) = cx
                .background_executor()
                .spawn(async { (terminal::daemon_outdated(), terminal::daemon_info()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.daemon_info = info;
                this.daemon_outdated = Some(outdated);
                if outdated {
                    this.upgrade_daemon_seamless(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 逐 pane 调 reconnect()：会话 id 都还在，走正常 reattach + 重放恢复画面。
    /// 无缝升级（Upgraded/Failed，见下）和硬重启都要用同一套。
    fn reconnect_all_terminals(&self, cx: &mut Context<Self>) {
        for sess in &self.sessions {
            let leaves = sess.term_leaves();
            for leaf in leaves {
                leaf.update(cx, |view, cx| view.reconnect(cx));
            }
        }
    }

    /// 无缝升级守护：守护 exec 新二进制、PTY fd 原地交接，会话不中断（smeltd.rs 头注释）。
    /// 成功后逐 pane reconnect——会话 id 都还在，走正常 reattach + 重放，画面最多闪一下。
    /// 正在跑的守护太旧不认识 upgrade op 时提示改用下面的硬重启。
    fn upgrade_daemon_seamless(&mut self, cx: &mut Context<Self>) {
        if self.daemon_upgrading {
            return;
        }
        self.daemon_upgrading = true;
        self.daemon_upgrade_msg = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome =
                cx.background_executor().spawn(async { terminal::upgrade_daemon() }).await;
            // exec 换代后 PID / 启动时刻都变了，跟版本一起重新问一遍。
            let (outdated, info) = cx
                .background_executor()
                .spawn(async { (terminal::daemon_outdated(), terminal::daemon_info()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.daemon_upgrading = false;
                this.daemon_info = info;
                this.daemon_outdated = Some(outdated);
                this.daemon_upgrade_msg = Some(match outcome {
                    terminal::UpgradeOutcome::Upgraded => {
                        // 交接后守护侧 jolt 要等客户端 resize；略延迟再 reconnect，
                        // 避免刚 exec 完就 attach 撞上空 Term + jolt 还没完成。
                        this.schedule_reconnect_all_terminals(cx);
                        "已无缝升级，所有会话保持运行。".to_string()
                    }
                    terminal::UpgradeOutcome::Unsupported => {
                        // 守护完全没认这个 op，控制连接以外的东西没被碰过，各 pane
                        // 的流式连接照常连着，不需要重连。
                        "正在跑的守护版本过旧，不支持无缝升级；请用「重启守护进程」（会断开会话）。"
                            .to_string()
                    }
                    terminal::UpgradeOutcome::Failed => {
                        // 守护回了 ok:true 才会 exec：只要走到这一步，exec 大概率已经
                        // 发生、旧连接已经随之断开，只是我们没能在轮询窗口内确认新
                        // 进程的 mtime 追平——按"可能已断"保守重连，好过让用户以为
                        // 终端只是卡了一下、实际连接早就死了却不知道要重开。
                        this.schedule_reconnect_all_terminals(cx);
                        "升级结果未确认（可能已生效但检测超时），已尝试重连各终端；如仍无响应可重试或改用重启。".to_string()
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// GUI 侧栏当前认领的全部 session id（Term 会话；ACP 会话是 GUI 直接 spawn
    /// 的子进程，根本不经过 smeltd，不出现在 list 结果里，不用管）。跟 list
    /// 查回来的全量做差集，剩下的就是「守护持有但没有任何侧栏在追踪」的孤儿
    /// ——测试跑出来的、忘了关的临时会话，都会落在这一类。
    /// 会话管理弹窗判断「孤儿」的依据：守护/smeltd 那边的会话 id 有没有被
    /// 某个侧栏标签认领。终端会话看 `term_leaves`；ACP 会话现在也托管在
    /// smeltd 里、用同一份 `list` 汇总（见 smeltd「ACP 会话托管」一节），
    /// 不把它们的 id 也算进「已认领」，正常开着的 ACP 对话会被误标成孤儿。
    fn tracked_session_ids(&self, cx: &App) -> std::collections::HashSet<String> {
        self.sessions
            .iter()
            .flat_map(|s| s.term_leaves())
            .map(|t| t.read(cx).session_id().to_string())
            .chain(self.sessions.iter().filter_map(|s| match &s.kind {
                SessionKind::Acp(view) => Some(view.read(cx).session_id().to_string()),
                _ => None,
            }))
            .collect()
    }

    /// 打开「会话管理」弹窗并触发一次查询（每次打开都重新拉最新数据，不复用
    /// 上次缓存——孤儿是不是还在、有没有新泄漏，都得是当下的事实）。
    fn open_session_manager(&mut self, cx: &mut Context<Self>) {
        self.session_manager_open = true;
        self.session_manager_list = None;
        cx.notify();
        self.refresh_session_manager(cx);
    }

    fn refresh_session_manager(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let list = cx
                .background_executor()
                .spawn(async { terminal::list_daemon_sessions() })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.session_manager_list = Some(list);
                cx.notify();
            });
        })
        .detach();
    }

    /// 按 id 前缀分流到对应的 kill op——ACP 会话（`acp-` 前缀）只在
    /// `AcpSessions` 表里，终端的 `kill` op 认不出这个 id，会静默什么都不做
    /// 却照样回 `{"ok":true}`（真实教训：两条表分开存，用错 op 表面上「成功」
    /// 实际上会话根本没被杀掉，圈了个大坑）。
    fn kill_daemon_session(id: &str) {
        if id.starts_with("acp-") {
            smelt_core::acp_client::kill_acp_session(id);
        } else {
            terminal::kill_remote(id);
        }
    }

    /// 关掉守护进程里的一个会话（真杀底层 shell / ACP 子进程），关完刷新列表。
    fn kill_session_in_manager(&mut self, id: String, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .spawn({
                    let id = id.clone();
                    async move { Self::kill_daemon_session(&id) }
                })
                .await;
            let _ = this.update(cx, |this, cx| this.refresh_session_manager(cx));
        })
        .detach();
    }

    /// 批量清理「没有任何侧栏在追踪」的孤儿——不碰任何 GUI 认领的正常会话，
    /// 不需要走「重启守护进程」那种连坐所有会话的核选项。
    fn kill_all_orphans_in_manager(&mut self, cx: &mut Context<Self>) {
        let tracked = self.tracked_session_ids(cx);
        let Some(list) = self.session_manager_list.clone() else {
            return;
        };
        let orphan_ids: Vec<String> = list
            .into_iter()
            .map(|s| s.id)
            .filter(|id| !tracked.contains(id))
            .collect();
        if orphan_ids.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .spawn(async move {
                    for id in &orphan_ids {
                        Self::kill_daemon_session(id);
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| this.refresh_session_manager(cx));
        })
        .detach();
    }

    /// 「会话管理」弹窗：列出守护进程持有的全部会话，标出哪些是孤儿（没有任何
    /// 侧栏在追踪），逐个/批量清理。入口和弹层都在设置窗口里。
    pub(crate) fn render_session_manager(&self, cx: &mut Context<Self>) -> Div {
        let (fg, muted, border) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground, t.border)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);
        let tracked = self.tracked_session_ids(cx);

        let body: AnyElement = match &self.session_manager_list {
            None => div()
                .text_sm()
                .text_color(muted)
                .child("查询中…")
                .into_any_element(),
            Some(list) if list.is_empty() => div()
                .text_sm()
                .text_color(muted)
                .child("守护进程当前没有任何会话。")
                .into_any_element(),
            Some(list) => {
                let orphan_count = list.iter().filter(|s| !tracked.contains(&s.id)).count();
                let mut rows = v_flex()
                    .id("session-manager-list")
                    .gap_1()
                    .max_h(px(360.))
                    .overflow_y_scroll();
                for s in list {
                    let is_orphan = !tracked.contains(&s.id);
                    let is_acp = s.id.starts_with("acp-");
                    let label = s
                        .cwd
                        .clone()
                        .or_else(|| s.title.clone())
                        .unwrap_or_else(|| s.id.clone());
                    let id_for_kill = s.id.clone();
                    rows = rows.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .justify_between()
                            .py_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .min_w_0()
                                    .child(div().size_2().rounded_full().bg(if is_orphan {
                                        rgb(ui_theme::red())
                                    } else {
                                        rgb(ui_theme::green())
                                    }))
                                    .child(
                                        div()
                                            .text_xs()
                                            .flex_shrink_0()
                                            .text_color(muted)
                                            .child(if is_acp { "对话" } else { "终端" }),
                                    )
                                    .child(div().text_sm().text_color(fg).truncate().child(label))
                                    .children(is_orphan.then(|| {
                                        div()
                                            .text_xs()
                                            .text_color(rgb(ui_theme::red()))
                                            .child("孤儿（无侧栏追踪）")
                                    })),
                            )
                            .child(Self::modal_button(
                                "kill-session-in-manager",
                                "关闭",
                                neutral_bg,
                                neutral_hover,
                                fg,
                                move |this, _, _, cx| {
                                    this.kill_session_in_manager(id_for_kill.clone(), cx);
                                },
                                cx,
                            )),
                    );
                }
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("共 {} 个，{orphan_count} 个孤儿", list.len())),
                    )
                    .child(rows)
                    .into_any_element()
            }
        };

        let has_orphans = self
            .session_manager_list
            .as_ref()
            .map(|l| l.iter().any(|s| !tracked.contains(&s.id)))
            .unwrap_or(false);

        let content = v_flex()
            .child(div().font_bold().text_color(fg).text_lg().child("会话管理"))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("守护进程持有的全部会话；孤儿是没有被任何窗口侧栏追踪的（测试跑出来的、忘了关的临时会话），清理它们不影响正常使用中的会话。"),
            )
            .child(div().border_t_1().border_color(border).pt_3().child(body))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "close-session-manager",
                        "关闭",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| {
                            this.session_manager_open = false;
                            cx.notify();
                        },
                        cx,
                    ))
                    .when(has_orphans, |el| {
                        el.child(Self::modal_button(
                            "kill-all-orphans",
                            "清理全部孤儿",
                            tint,
                            hover,
                            accent_text,
                            |this, _, _, cx| {
                                this.kill_all_orphans_in_manager(cx);
                            },
                            cx,
                        ))
                    }),
            );
        Self::modal_shell(420., true, content, cx)
    }

    /// upgrade 完成后延迟 reattach：给守护 handoff 泵线程 / jolt 一点时间。
    fn schedule_reconnect_all_terminals(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(400))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.reconnect_all_terminals(cx);
            });
        })
        .detach();
    }

    /// 用户在弹窗里点了「确定重启」：让守护退出（断开所有会话）、拉起磁盘上最新的
    /// smeltd、再刷新状态。
    ///
    /// **禁止**在 `update` 里同步 `Terminal::spawn`：握手含 sleep/轮询，多 pane 会把
    /// UI 卡死（「点重启守护就假死」）。流程：后台杀+拉起守护 → 后台按 cwd/sid 建
    /// Terminal → 主线程 `adopt_terminal` 挂回各 pane。
    fn confirm_restart_daemon(&mut self, cx: &mut Context<Self>) {
        self.show_daemon_restart_confirm = false;
        self.daemon_outdated = None;
        // 收集重建参数（Entity 可 Clone；真正 spawn 扔后台）。
        // 硬重启会清掉守护里的会话。终端 agent 不做语义恢复，缺失会话统一重开 shell。
        let mut jobs: Vec<(Entity<TerminalView>, Option<String>, String)> = Vec::new();
        for sess in &self.sessions {
            let leaves = sess.term_leaves();
            for leaf in leaves {
                let view = leaf.read(cx);
                let cwd = view.cwd();
                let sid = view.session_id().to_string();
                jobs.push((leaf, cwd, sid));
            }
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            // 硬重启后是全新进程，PID / 启动时刻 / 会话数都得重问。
            let (outdated, info) = cx
                .background_executor()
                .spawn(async {
                    terminal::restart_daemon();
                    terminal::ensure_daemon_running();
                    (terminal::daemon_outdated(), terminal::daemon_info())
                })
                .await;

            // 握手/重试全在后台；主线程只接结果
            let built = cx
                .background_executor()
                .spawn(async move {
                    let mut out = Vec::with_capacity(jobs.len());
                    for (entity, cwd, sid) in jobs {
                        let term = terminal::Terminal::spawn(24, 80, cwd.as_deref(), &sid, None);
                        out.push((entity, term));
                    }
                    out
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.daemon_info = info;
                this.daemon_outdated = Some(outdated);
                let mut failed = 0usize;
                for (entity, term) in built {
                    match term {
                        Ok(t) => {
                            entity.update(cx, |view, cx| view.adopt_terminal(t, cx));
                        }
                        Err(e) => {
                            failed += 1;
                            eprintln!("[workspace] 硬重启后重开终端失败：{e:#}");
                        }
                    }
                }
                if failed > 0 {
                    this.background_error = Some(format!(
                        "守护已重启，但有 {failed} 个终端没能重开（侧栏会话仍在，可关了再开）"
                    ));
                } else {
                    this.daemon_upgrade_msg =
                        Some("守护已硬重启，会话已按原目录/启动命令重建。".into());
                }
                // 布局没变，写盘刷新 launch_cmd 等字段即可。
                this.save_state(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 跳到某条通知：切到该会话 + 聚焦该 pane。
    fn goto_notification(
        &mut self,
        session_ix: usize,
        pane: Option<&Entity<TerminalView>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate(session_ix, window, cx);
        if let Some(pane) = pane {
            self.activate_pane(pane, window, cx);
        }
        cx.notify();
    }

    /// Toast / 系统通知只保存稳定的守护会话 id；点击时再查当前侧栏位置，避免通知
    /// 出现后用户重排、关闭会话导致旧索引跳错目标。
    fn goto_notification_session(
        &mut self,
        session_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self
            .sessions
            .iter()
            .enumerate()
            .find_map(|(session_ix, session)| match &session.kind {
                SessionKind::Term { .. } => session
                    .term_leaves()
                    .into_iter()
                    .find(|pane| pane.read(cx).session_id() == session_id)
                    .map(|pane| (session_ix, Some(pane))),
                SessionKind::Acp(view) if view.read(cx).session_id() == session_id => {
                    Some((session_ix, None))
                }
                SessionKind::Acp(_) => None,
            });
        if let Some((session_ix, pane)) = target {
            self.goto_notification(session_ix, pane.as_ref(), window, cx);
        }
    }

    /// 弹窗遮罩 + 居中卡片壳：宽度 `width`，颜色取当前主题。`content` 是调用方已经
    /// 拼好的标题/正文/按钮行（`v_flex().child(...)...`），这里只负责外层半透明遮罩
    /// 和卡片本身的边框/圆角/阴影/内边距——是所有确认弹窗共享的视觉容器。
    ///
    /// `heavy` 控制遮罩压暗程度：真正不可逆/高后果的操作（退出、删除 worktree、
    /// 重启守护进程、丢弃未保存改动）用 `true`——全屏压暗，明确打断当前操作；
    /// 纯输入类的低风险操作（重命名）用 `false`——只留一层很淡的遮罩防止误点
    /// 背景，不用完全打断视觉，跟操作本身的后果对齐（见交互设计讨论）。
    fn modal_shell(width: f32, heavy: bool, content: Div, cx: &mut Context<Self>) -> Div {
        let border = {
            let t = cx.theme();
            t.border
        };
        let backdrop = ui_theme::glass_scrim(heavy);
        div()
            .absolute()
            .inset_0()
            .bg(backdrop)
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                content
                    .w(px(width))
                    .p_5()
                    .bg(ui_theme::glass_floating())
                    .border_1()
                    .border_color(border)
                    .rounded_lg()
                    .shadow_lg()
                    .gap_4(),
            )
    }

    /// 弹窗按钮的中性/强调配色：(中性底色, 中性 hover, 强调底色, 强调 hover, 强调文字色)。
    /// `danger=true` 强调色用红（危险操作，如删除/重启），`false` 用蓝（普通确认）。
    fn modal_accent_colors(danger: bool) -> (Hsla, Hsla, Hsla, Hsla, Hsla) {
        let neutral_bg: Hsla = ui_theme::overlay(0x0a).into();
        let neutral_hover: Hsla = ui_theme::overlay(0x1f).into();
        if danger {
            (
                neutral_bg,
                neutral_hover,
                ui_theme::tint(ui_theme::red(), 0x24).into(),
                ui_theme::tint(ui_theme::red(), 0x40).into(),
                Hsla::from(rgb(ui_theme::red())),
            )
        } else {
            // 主操作（确定退出 / 提交 等）用**实心品牌色 blurple + 白字**，不再是
            // 「薄底 + 彩字」那种轮廓感——之前还错用了青蓝 blue()，既不突出也不是
            // 色板的强调色。danger 保持红薄底（危险操作克制警示）。
            (
                neutral_bg,
                neutral_hover,
                Hsla::from(rgb(ui_theme::accent())),
                ui_theme::tint(ui_theme::accent(), 0xdd).into(),
                Hsla::from(rgb(ui_theme::on_accent())),
            )
        }
    }

    /// 弹窗按钮的基础样式（尺寸/圆角/字号/底色/文字色/label），不含点击行为——大部分
    /// 调用方直接用 [`Self::modal_button`]；`render_delete_worktree_confirm` 的
    /// 「检查中…」禁用态需要条件性挂 hover/on_click，才会单独调这个再自己 `.when(...)`。
    fn modal_button_base(
        id: &'static str,
        label: &'static str,
        bg: Hsla,
        text_color: Hsla,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .px_3()
            .py(px(5.))
            .rounded_lg()
            .bg(bg)
            .border_1()
            .border_color(ui_theme::overlay(0x12))
            .text_sm()
            .text_color(text_color)
            .child(label)
    }

    /// 弹窗按钮：基础样式 + hover 变色 + 点击行为，覆盖绝大多数弹窗按钮的用法。
    fn modal_button(
        id: &'static str,
        label: &'static str,
        bg: Hsla,
        hover_bg: Hsla,
        text_color: Hsla,
        on_click: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        Self::modal_button_base(id, label, bg, text_color)
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .on_click(cx.listener(on_click))
    }

    /// 渲染无条件退出确认弹层：磨砂遮罩 + 确认退出/取消按钮。
    fn render_quit_confirm(&self, cx: &mut Context<Self>) -> Div {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) =
            Self::modal_accent_colors(false);

        let content = v_flex()
            .child(
                div()
                    .font_bold()
                    .text_color(fg)
                    .text_lg()
                    .child("退出 Smelt？"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("后台会话将继续运行，当前连接会断开。"),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-quit",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| {
                            this.show_quit_confirm = false;
                            cx.notify();
                        },
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-quit",
                        "退出",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| {
                            // 有暂存好的新版本就在退出前落盘替换；失败静默忽略，
                            // 不能因为自更新出岔子就把用户堵在退出流程里。
                            if let updater::UpdateStatus::ReadyToInstall { staged_app, .. } =
                                &this.update_status
                            {
                                // 与设置页「立即重启更新」相同：先 handoff 守护再换包。
                                let _ =
                                    crate::terminal::install_app_preserving_sessions(staged_app);
                            }
                            cx.quit();
                        },
                        cx,
                    )),
            );
        Self::modal_shell(320., true, content, cx)
    }

    /// 侧栏「重命名」弹层：与 render_quit_confirm 同款视觉（居中卡片 + 半透明遮罩），
    /// 正文换成预填当前标题的文本框。仅在 self.rename_input 就绪时被调用（见
    /// start_rename/上面 .children(self.rename_target.is_some()...)）。
    fn render_rename_session(&self, cx: &mut Context<Self>) -> Div {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) =
            Self::modal_accent_colors(false);
        let Some(input) = self.rename_input.as_ref() else {
            return div();
        };
        // 会话行和分屏子行共用这个弹窗，标题得说清改的是哪个。
        let heading = match self.rename_target {
            Some(RenameTarget::Pane(_)) => "重命名终端",
            _ => "重命名会话",
        };

        let content = v_flex()
            .child(div().font_bold().text_color(fg).text_lg().child(heading))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("留空则恢复自动识别的标题。"),
            )
            .child(Input::new(input))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-rename",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_rename(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-rename",
                        "确定",
                        tint,
                        hover,
                        accent_text,
                        |this, _, window, cx| this.confirm_rename(window, cx),
                        cx,
                    )),
            );
        Self::modal_shell(320., false, content, cx)
    }

    /// 「重启守护进程」二次确认弹窗：明确告知会断开所有当前终端会话。与
    /// render_quit_confirm 同款视觉（居中卡片 + 半透明遮罩）。
    ///
    /// 入口只在设置窗「更新」页；弹层挂在设置窗上（见 `SettingsWindow::render`），
    /// 不再画到主窗口，避免「按钮在设置、确认框跑到主界面」的割裂感。
    pub(crate) fn render_daemon_restart_confirm(&self, cx: &mut Context<Self>) -> Div {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);

        let content = v_flex()
            .child(div().font_bold().text_color(fg).text_lg().child("确定重启守护进程吗？"))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("守护进程升级后才会生效新版本。重启会立即断开并终止当前所有终端会话（包括正在跑的 agent），且无法恢复。"),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-daemon-restart",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| {
                            this.show_daemon_restart_confirm = false;
                            cx.notify();
                        },
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-daemon-restart",
                        "确定重启",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_restart_daemon(cx),
                        cx,
                    )),
            );
        Self::modal_shell(320., true, content, cx)
    }

    /// 当前文件有未保存改动、又点了别的文件时弹的确认弹窗：取消 / 不保存直接切换 /
    /// 保存并切换。与 render_quit_confirm 同款视觉（居中卡片 + 半透明遮罩）。
    fn render_unsaved_file_confirm(&self, target: String, cx: &mut Context<Self>) -> Div {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) =
            Self::modal_accent_colors(false);
        let cur_name = self
            .open_file
            .as_ref()
            .map(|of| {
                of.path
                    .rsplit('/')
                    .next()
                    .unwrap_or(of.path.as_str())
                    .to_string()
            })
            .unwrap_or_default();
        let target_name = target
            .rsplit('/')
            .next()
            .unwrap_or(target.as_str())
            .to_string();

        let content = v_flex()
            .child(
                div()
                    .font_bold()
                    .text_color(fg)
                    .text_lg()
                    .child(format!("「{cur_name}」有未保存的改动")),
            )
            .child(div().text_sm().text_color(muted).child(format!(
                "要切换到「{target_name}」了，这些改动还没保存，要怎么处理？"
            )))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "unsaved-cancel",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| {
                            this.pending_file_switch = None;
                            cx.notify();
                        },
                        cx,
                    ))
                    .child(Self::modal_button(
                        "unsaved-discard",
                        "不保存，直接切换",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, window, cx| {
                            if let Some(target) = this.pending_file_switch.take() {
                                this.open_file_now(target, None, window, cx);
                            }
                        },
                        cx,
                    ))
                    .child(Self::modal_button(
                        "unsaved-save-switch",
                        "保存并切换",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| {
                            if let Some(target) = this.pending_file_switch.take() {
                                this.pending_switch_after_save = Some(target);
                                this.save_open_file(cx);
                            }
                        },
                        cx,
                    )),
            );
        Self::modal_shell(360., true, content, cx)
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let all: Vec<(SharedString, Cmd)> = self
            .all_commands(cx)
            .into_iter()
            .map(|(label, cmd)| (label.into(), cmd))
            .collect();
        let state = cx.new(|cx| ListState::new(CmdDelegate::new(all), window, cx).searchable(true));
        // 确认（回车/点击）执行命令；取消（Esc）关闭面板。
        self._palette_sub = Some(cx.subscribe_in(
            &state,
            window,
            |this, state, ev: &ListEvent, window, cx| match ev {
                ListEvent::Confirm(ix) => {
                    let cmd = state
                        .read(cx)
                        .delegate()
                        .matched
                        .get(ix.row)
                        .map(|(_, c)| c.clone());
                    if let Some(cmd) = cmd {
                        this.exec_cmd(cmd, window, cx);
                    }
                }
                ListEvent::Cancel => this.close_palette(window, cx),
                _ => {}
            },
        ));
        state.update(cx, |s, cx| s.focus(window, cx));
        self.palette = Some(state);
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self._palette_sub = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 全部命令（含逐会话切换）。
    fn all_commands(&self, cx: &App) -> Vec<(String, Cmd)> {
        let mut v = vec![
            ("新建会话".to_string(), Cmd::NewTab),
            ("打开项目…".to_string(), Cmd::OpenProject),
            ("关闭当前会话/窗格".to_string(), Cmd::CloseTab),
            ("下一个会话".to_string(), Cmd::NextTab),
            ("上一个会话".to_string(), Cmd::PrevTab),
        ];
        for (i, s) in self.sessions.iter().enumerate() {
            v.push((format!("切换到: {}", s.title(cx)), Cmd::SwitchTab(i)));
        }
        v
    }

    fn exec_cmd(&mut self, cmd: Cmd, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
        match cmd {
            Cmd::NewTab => self.new_tab(cx),
            Cmd::OpenProject => self.open_project(cx),
            Cmd::CloseTab => self.close_active(window, cx),
            Cmd::NextTab => self.next_active(window, cx),
            Cmd::PrevTab => self.prev_active(window, cx),
            Cmd::SwitchTab(i) => self.activate(i, window, cx),
        }
    }

    /// 递归渲染分屏布局树：Leaf 渲染一个终端（活动 pane 描边 + 点击聚焦），
    /// Split 用 h/v_resizable 把子节点排成可拖拽的并排 / 堆叠。
    fn render_pane(&self, pane: &Pane, path: &str, cx: &mut Context<Self>) -> AnyElement {
        match pane {
            Pane::Leaf(t) => {
                let active = self.cur().is_some_and(|s| s.anchor_id() == t.entity_id());
                // 不给任何 pane 描边（iTerm2 也不描，之前的蓝框提醒也拿掉了）：分屏时靠
                // 「压暗非活动 pane」区分谁是活动的就够了；单 pane 时压根没有别的 pane
                // 可比，不需要任何叠加层。
                let multi_pane = self.cur().is_some_and(|s| s.pane_count() > 1);
                let overlay = if !multi_pane || active {
                    div().absolute().inset_0()
                } else {
                    div().absolute().inset_0().bg(hsla(0., 0., 0., 0.28))
                };
                let te = t.clone();
                div()
                    .id(SharedString::from(path.to_string()))
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    // 点击 pane 即设为当前会话的活动 pane（终端自身也会抢焦点，二者一致）。
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, window, cx| {
                            this.activate_pane(&te, window, cx)
                        }),
                    )
                    .child(t.clone())
                    .child(overlay)
                    .into_any_element()
            }
            Pane::Split {
                axis,
                state,
                children,
                init_sizes,
            } => {
                let id = SharedString::from(path.to_string());
                let mut group = if matches!(axis, Axis::Horizontal) {
                    h_resizable(id)
                } else {
                    v_resizable(id)
                }
                .with_state(state);
                for (i, c) in children.iter().enumerate() {
                    let el = self.render_pane(c, &format!("{path}-{i}"), cx);
                    // 存档尺寸只作 initial_size：拖过之后 panel 自己的 size 会盖过它
                    //（见 Pane::Split::init_sizes 注释），所以每帧原样传是安全的。
                    let mut panel = resizable_panel().child(el);
                    if let Some(s) = init_sizes.get(i).copied().filter(|s| *s > 0.) {
                        panel = panel.size(px(s));
                    }
                    group = group.child(panel);
                }
                group.into_any_element()
            }
        }
    }
}

impl Workspace {
    /// 舞台覆盖页（旧全屏页）：总览 / 任务 / 文件树 / Git / 历史。
    /// 原主区 TabBar 分派的 match 各臂原样搬入，由 stage_override 驱动。
    fn render_stage_override(
        &mut self,
        v: MainView,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (c_border, c_muted, c_fg) = {
            let t = cx.theme();
            (t.border, t.muted_foreground, t.foreground)
        };
        let _ = (c_border, c_muted, c_fg);
        match v {
            MainView::Tasks => unreachable!("Tasks 使用独立 WorkspaceRoute，不走 session 舞台"),
            // Files / Git 展开态已在调用侧（render 顶部的 stage_override 分派）
            // 特判，直接复用停靠面板的 render_inspector_rail + render_inspector_*，
            // 不会把这两个变体传进这个函数。diff 已经内嵌在改动页里，不再需要
            // 单独的舞台页。
            MainView::Files => unreachable!("MainView::Files 在调用侧已特判，不会走到这里"),
            MainView::Git => unreachable!("MainView::Git 在调用侧已特判，不会走到这里"),
            MainView::Skills => {
                unreachable!("MainView::Skills 在调用侧已特判，不会走到这里")
            }
            MainView::History => {
                let cwd = self.active_project_root(cx);
                let list_key = cwd.as_ref().map(|c| {
                    session_history::session_list_key(
                        self.history_agent,
                        self.history_profile.as_deref(),
                        c,
                    )
                });

                let sessions = list_key
                    .as_ref()
                    .and_then(|k| self.session_list.get(k).map(|(_, d)| d.clone()));
                let list_state = match sessions {
                    None => HistoryListState::Loading,
                    Some(s) if s.is_empty() => HistoryListState::Empty,
                    Some(s) => HistoryListState::Ready(s),
                };
                // 没选项目时给 Some(空表)，走「还没有记忆」而不是一直转圈。
                let memories = match &cwd {
                    Some(root) => self.memory_list.get(root).map(|(_, d)| d.clone()),
                    None => Some(Rc::new(Vec::new())),
                };
                history_view(
                    self.history_pane,
                    self.history_agent,
                    self.history_profile.clone(),
                    cwd,
                    list_state,
                    &self.session_detail,
                    self.history_detail_list_state.clone(),
                    memories,
                    self.memory_selected,
                    cx,
                )
                .into_any_element()
            }
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_session_ui(window, cx);
        let active = self.active_session;

        // 当前正在看的 pane 每帧确认未读，覆盖“查看期间刚到”的事件；行动项只标已读，
        // 仍由 daemon 后续 phase 转换 resolve。
        if let Some(session) = self.sessions.get(active) {
            let viewed_sid = match &session.kind {
                SessionKind::Term { active, .. } => Some(active.read(cx).session_id().to_string()),
                SessionKind::Acp(view) => Some(view.read(cx).session_id().to_string()),
            };
            if let (Some(store), Some(sid)) = (cx.try_global::<AttentionGlobal>(), viewed_sid) {
                store.0.lock().unwrap().mark_read(&sid);
            }
        }

        // Dock 角标 + 菜单栏图标角标/下拉菜单：行动项计数来自统一 store，
        // 变了才调 Cocoa API 更新（避免每次 render 都发一遍）。
        let statuses: Vec<AgentStatus> = self.sessions.iter().map(|s| s.status(cx)).collect();
        let known_session_ids: Vec<String> = self
            .sessions
            .iter()
            .flat_map(|session| match &session.kind {
                SessionKind::Term { .. } => session
                    .term_leaves()
                    .iter()
                    .map(|pane| pane.read(cx).session_id().to_string())
                    .collect(),
                SessionKind::Acp(view) => vec![view.read(cx).session_id().to_string()],
            })
            .collect();
        let attention_count = cx
            .try_global::<AttentionGlobal>()
            .map(|store| {
                let store = store.0.lock().unwrap();
                known_session_ids
                    .iter()
                    .filter(|id| store.has_unresolved_action(id))
                    .count()
            })
            .unwrap_or(0);
        if self.dock_badge_count != Some(attention_count) {
            self.dock_badge_count = Some(attention_count);
            dock::set_badge(attention_count);
            status_item::set_badge(attention_count);
        }

        // 菜单栏下拉菜单：按状态优先级排的会话列表（等审批 > 需要处理 > 运行中 >
        // 刚完成 > 空闲），跟总览页卡片同一套排序/配色口径。
        let mut menu_order: Vec<usize> = (0..self.sessions.len()).collect();
        menu_order.sort_by_key(|&ix| statuses[ix].rank());
        let menu_snapshot: Vec<status_item::SessionEntry> = menu_order
            .into_iter()
            .map(|ix| {
                let color = ui_theme::agent_status_rgb8(statuses[ix]);
                let status_text = match statuses[ix] {
                    AgentStatus::WaitingApproval => "等你批准",
                    AgentStatus::NeedsAttention => "需要处理",
                    AgentStatus::Running => "运行中",
                    AgentStatus::Done => "已完成",
                    AgentStatus::Idle => "空闲",
                };
                status_item::SessionEntry {
                    session_ix: ix,
                    title: self.sessions[ix].title(cx),
                    status_text,
                    color,
                }
            })
            .collect();
        if self.status_menu_snapshot.as_ref() != Some(&menu_snapshot) {
            status_item::update_menu(&menu_snapshot);
            self.status_menu_snapshot = Some(menu_snapshot);
        }

        // 定时任务扫描：启动后约 2s 首扫，之后 30s 一轮；到期 → run_task。
        if !self.task_schedule_started {
            self.task_schedule_started = true;
            cx.spawn_in(window, async move |this, cx| {
                smol::Timer::after(std::time::Duration::from_secs(2)).await;
                loop {
                    let alive = this
                        .update_in(cx, |this, window, cx| {
                            this.tick_scheduled_tasks(window, cx);
                        })
                        .is_ok();
                    if !alive {
                        break;
                    }
                    smol::Timer::after(std::time::Duration::from_secs(30)).await;
                }
            })
            .detach();
        }

        // 侧栏 GIT 角标全页面实时：保证当前项目的文件监听已建立。角标常驻显示，但
        // git_status 数据原本只在 Files/Git 页 render 时刷新，切到终端等页面就冻结。
        // ensure_git_watch 建监听后，仓库一有改动就主动重拉 git_status（见其 250ms 检查
        // 循环），角标在任何页面都即时跟手——事件驱动，文件不变时零开销，不搞轮询。
        // 内部按 root 去重，每帧调只是一次 HashMap 查找。
        if let Some(root) = self.cur().and_then(|s| s.cwd(cx)) {
            self.ensure_git_watch(root, cx);
        }

        // 捞「任务完成 → 自动续跑」挂旗（先收集再处理，避免 run 时改 sessions）。
        let mut task_continues: Vec<(String, String)> = Vec::new();
        for sess in &self.sessions {
            let leaves = sess.term_leaves();
            for leaf in &leaves {
                let cont_cwd = leaf.update(cx, |t, _cx| t.take_pending_task_continue());
                if let Some(cwd) = cont_cwd {
                    let sid = leaf.read(cx).session_id().to_string();
                    task_continues.push((sid, cwd));
                }
            }
        }
        for (sid, cwd) in task_continues {
            self.on_session_task_idle(&sid, &cwd, window, cx);
        }

        // 侧栏项目分组：后台刷新每个会话 cwd 的仓库身份（是不是 worktree + 分支名），
        // 让 worktree 的会话能跟主仓库聚在一起显示、标签带上分支名。侧栏一直显示
        // 全部项目，不像 git status 那样只关心当前打开的那个，所以对
        // self.sessions 里出现过的所有 cwd 都要探测，而不是只探测 self.cur()。
        let repo_cwds: HashSet<String> = self.sessions.iter().filter_map(|s| s.cwd(cx)).collect();
        for cwd in repo_cwds {
            self.ensure_repo_info(cwd, cx);
        }

        // 各类后台操作（建/删 worktree、生成 commit message）失败时，错误信息暂存在
        // 这个字段（后台任务里没有 Window，弹不了通知），render 一开始就取走弹成通知。
        if let Some(msg) = self.background_error.take() {
            window.push_notification(Notification::error(msg), cx);
        }
        // 所有 producer 共用的投递出口：前台 toast，后台 macOS 通知；应用在前台时，
        // 正在看的 pane 一律不弹。设置开关只控制打扰渠道，不影响 store 的状态与铃铛记录。
        let workspace = cx.entity();
        if let Some(store) = cx.try_global::<AttentionGlobal>() {
            let batch = store.0.lock().unwrap().drain_deliveries();
            let notify_config = cx
                .try_global::<settings::AgentUiConfig>()
                .cloned()
                .unwrap_or_default();
            for notification in batch {
                let mut is_current_view = false;
                let mut session_title = None;
                for (ix, sess) in self.sessions.iter().enumerate() {
                    let anchor = sess.anchor_id();
                    let matches_acp = matches!(
                        &sess.kind,
                        SessionKind::Acp(view)
                            if view.read(cx).session_id() == notification.session_id
                    );
                    if matches_acp {
                        is_current_view = ix == active;
                        session_title = Some(sess.title(cx));
                    }
                    for leaf in sess.term_leaves() {
                        if leaf.read(cx).session_id() == notification.session_id {
                            session_title = Some(sess.title(cx));
                            if ix == active && leaf.entity_id() == anchor {
                                is_current_view = true;
                            }
                        }
                    }
                }
                let display_title = session_title
                    .map(|session| format!("{} · {session}", notification.title))
                    .unwrap_or_else(|| notification.title.clone());
                let message = notification.message.clone();
                match smelt_core::attention::delivery_channel(
                    agent_notification_enabled(&notify_config, notification.kind),
                    window.is_window_active(),
                    is_current_view,
                ) {
                    smelt_core::attention::DeliveryChannel::Suppress => {}
                    smelt_core::attention::DeliveryChannel::Toast => {
                        let session_id = notification.session_id.clone();
                        let workspace = workspace.clone();
                        let toast = match notification.kind {
                            AttentionKind::Approval => Notification::warning(message),
                            AttentionKind::Input => Notification::info(message),
                            AttentionKind::Success => Notification::success(message),
                            AttentionKind::Failure => Notification::error(message),
                            AttentionKind::Bell | AttentionKind::Notice => {
                                Notification::info(message)
                            }
                        }
                        .title(display_title)
                        .on_click(move |_, window, cx| {
                            workspace.update(cx, |this, cx| {
                                this.goto_notification_session(&session_id, window, cx);
                            });
                        });
                        window.push_notification(toast, cx);
                    }
                    smelt_core::attention::DeliveryChannel::System => {
                        status_item::deliver_notification(
                            &notification.session_id,
                            &display_title,
                            &message,
                        )
                    }
                }
            }
        }

        // Git 页：后台刷新改动列表 + 分支列表（git status/for-each-ref 慢，绝不在
        // render 里同步跑）。
        if matches!(self.stage_override, Some(MainView::Git))
            || (self.inspector_open && self.inspector_tab == inspector::InspectorTab::Git)
        {
            if let Some(root) = self.cur().and_then(|s| s.cwd(cx)) {
                // 进 Git 页主动 fetch 一次，让 ahead/behind 反映远端最新。render 每帧都会
                // 进这个分支，靠 git_autofetch_at 去抖——同一仓库 60s 内只自动 fetch 一次，
                // 避免每帧狂发网络请求。fetch 成功后 run_git_op 会 invalidate_git_status，
                // 顺带把 ahead/behind 重算出来。
                let fetch_due = self
                    .git_autofetch_at
                    .get(&root)
                    .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(60));
                if fetch_due {
                    self.git_autofetch_at.insert(root.clone(), Instant::now());
                    self.git_fetch_silent(cx);
                }
                self.ensure_git_watch(root.clone(), cx);
                self.ensure_git_status(root.clone(), cx);
                self.ensure_branches(root, cx);
            }
        }

        // 历史会话页：后台刷新当前项目的会话列表 / 记忆列表（看当前是哪个子页）。
        if self.stage_override == Some(MainView::History) {
            if let Some(root) = self.active_project_root(cx) {
                match self.history_pane {
                    HistoryPane::Sessions => {
                        let pid = self.history_profile.clone();
                        self.ensure_session_list(self.history_agent, pid, root, cx)
                    }
                    HistoryPane::Memories => self.ensure_memory_list(root, cx),
                }
            }
        }

        // 文件树页：后台刷新根目录 + 所有已展开目录的直接子项列表（fs::read_dir 绝不
        // 在 render 里同步跑）。展开新目录时它会先落空，下一帧缓存到位后自动出现。
        if matches!(self.stage_override, Some(MainView::Files))
            || (self.inspector_open && self.inspector_tab == inspector::InspectorTab::Files)
        {
            // 搜索输入框懒创建（需要 window）：键入即 notify，触发文件名 + 内容搜索。
            if self.file_filter.is_none() {
                use gpui_component::input::{InputEvent, InputState};
                let state =
                    cx.new(|cx| InputState::new(window, cx).placeholder("搜索文件名 / 内容…"));
                self._file_filter_sub = Some(cx.subscribe(&state, |_, _, ev: &InputEvent, cx| {
                    if matches!(ev, InputEvent::Change) {
                        cx.notify();
                    }
                }));
                self.file_filter = Some(state);
            }
            let query = self
                .file_filter
                .as_ref()
                .map(|s| s.read(cx).value().trim().to_string())
                .unwrap_or_default();
            // 多根工作区：文件树同时挂着所有项目根，后台刷新要覆盖每个根。改动文件
            // M/A/D 标要用 git status；不强制用户先去过 Git 页才有数据，Files 页自己
            // 也确保各根缓存新鲜（ensure_git_status 内部已有 TTL）。
            let roots = self.workspace_roots(cx);
            if query.is_empty() {
                // 无查询：正常树形浏览，清空上一次搜索结果。
                self.search_results = None;
                for root in &roots {
                    self.ensure_git_status(root.clone(), cx);
                    self.ensure_dir_listing(root.clone(), cx);
                }
                // 展开的子目录是绝对路径、跟属于哪个根无关，一次性全刷。
                for dir in self.expanded.clone() {
                    self.ensure_dir_listing(dir, cx);
                }
            } else if let Some(root) = self.cur().and_then(|s| s.cwd(cx)) {
                // 有查询：搜索先只在当前会话根做（跨根搜索留作后续）；顺带刷一份该根的
                // git status 给结果视图用。
                self.ensure_git_status(root.clone(), cx);
                self.ensure_search(root, query, cx);
            }
        }

        // 同步窗口背景外观：不透明度 / 模糊改了（可能来自 slider/取色器的无 window 回调）
        // → 这里用 window 切换透明/模糊。仅在变化时调，避免每帧重复。
        let want_bg = cx.global::<Appearance>().window_bg();
        if self.applied_window_bg != Some(want_bg) {
            window.set_background_appearance(want_bg);
            self.applied_window_bg = Some(want_bg);
        }

        // 调试 HUD：开启时用 request_animation_frame 驱动连续渲染，测真实帧率
        // （连续重绘会重跑整窗布局/绘制，diff 面板卡不卡直接反映到帧耗时上）。
        if self.debug_hud {
            let now = Instant::now();
            if let Some(prev) = self.last_frame {
                let dt = now.duration_since(prev).as_secs_f32();
                if dt > 0.0 {
                    let inst = 1.0 / dt;
                    self.fps_ema = if self.fps_ema <= 0.0 {
                        inst
                    } else {
                        self.fps_ema * 0.9 + inst * 0.1
                    };
                }
            }
            self.last_frame = Some(now);
            let mem_due = self
                .debug_mem_sampled_at
                .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1));
            if mem_due {
                self.debug_mem_rss = mem_usage::current_rss_bytes();
                self.debug_mem_sampled_at = Some(now);
            }
            window.request_animation_frame();
        } else {
            self.last_frame = None;
            self.debug_mem_rss = None;
            self.debug_mem_sampled_at = None;
        }

        // 主题色 token（跟随 gpui-component 主题，替代硬编码）
        let (c_border, c_muted, c_fg) = {
            let t = cx.theme();
            (t.border, t.muted_foreground, t.foreground)
        };
        // “毛玻璃”打开时，窗口合成器负责真实背景模糊；这里必须同步把应用外壳
        // 和各块表面改成半透明，否则一层不透明主题底会把 vibrancy 完全盖住。
        // 关闭时仍使用接近实色的表面，保证普通窗口下的文字对比度。
        // 无边框玻璃需要一层稳定的深色底板来显出面板间隙。
        // 三层不透明度拉到 0xF0 附近：之前 0x78~0x96 的低透明度在同一块深色背板上
        // 互相叠加后，bg_rail/bg_elev/bg_panel 原本的色阶差会被腰斩到几乎不可辨
        // （见设计讨论：三层实测混成 15/24/32 这种挤在一起的窄区间）。留一点点
        // 透明度只为不完全吃掉毛玻璃开启时的原生 vibrancy，层级主要靠色板本身
        // 的色阶差来表达，不能指望透明度叠加自己「让色差露出来」。
        let shell_surface: Hsla = ui_theme::tint(ui_theme::bg_rail(), 0xf4).into();
        let sidebar_surface: Hsla = ui_theme::tint(ui_theme::bg_elev(), 0xf0).into();
        let stage_surface: Hsla = ui_theme::tint(ui_theme::bg_panel(), 0xf0).into();

        let sidebar_motion = self.sidebar_transition.frame();
        if sidebar_motion.animating {
            window.request_animation_frame();
        }

        // 会话列表：单列按项目上下分组（替代旧 gpui-component Sidebar 两级菜单；
        // 设计稿的「rail + 列表」左右两列实测割裂，见 session_list.rs 文件头）。
        let list_el = self.render_session_list(window, cx);
        // 提升到舞台的那个 tab 不再停靠一份（见 inspector_panel_promoted）。
        let inspector_motion = self.inspector_transition.frame();
        if inspector_motion.animating {
            window.request_animation_frame();
        }
        let inspector_panel_el = (inspector_motion.mounted && !self.inspector_panel_promoted())
            .then(|| self.render_inspector_panel(window, cx));
        // inspector 没停靠在旁边时，舞台/返回条/展开的 inspector rail 都会变成
        // 贴着窗口右边缘那一块，得给右上角浮着的全屏/终端抽屉/侧边面板 3 颗
        // 图标让位置；停靠时那颗图标条自己在右边接管，这几处就不用多留。
        // `right_edge` 给「提升到舞台」的 3 个 inspector rail 用——那几处
        // inspector_panel_el 永远是 None（`inspector_panel_promoted` 保证的），
        // 不存在中途开合动画，直接给个常量布尔就够。
        let right_edge = inspector_panel_el.is_none();
        // `right_reserve` 给舞台头/返回条用：inspector 停靠面板本身有挂载/收起
        // 动画（inspector_motion.progress 从 0→1 或反过来），如果这里用上面那种
        // 布尔一刀切，`mounted` 在动画刚开始（progress 还是 0）就先变 true，
        // 右边距瞬间从 100px 掉到 16px，标题栏内容会先冲到最右边，
        // 再被展开中的面板挤回来——这就是「先到最右边再反弹回来」的原因。
        // 改成跟 progress 同步插值，不留档突变的那一刻。
        let right_reserve = if self.inspector_panel_promoted() {
            px(100.)
        } else {
            px(100. - (100. - 16.) * inspector_motion.progress.clamp(0., 1.))
        };
        // 底部抽屉（快捷终端）同一套挂载过渡；具体高度交给下面真正的
        // v_resizable/resizable_panel 组件去管（拖拽 + 动画期间的程序化改宽度
        // 都走那一套，不再自己手算 opacity/height）。
        let bottom_drawer_motion = self.bottom_drawer_transition.frame();
        if bottom_drawer_motion.animating {
            window.request_animation_frame();
        }
        let bottom_drawer_el = self.render_bottom_drawer(bottom_drawer_motion.mounted, window, cx);
        // ACP 冷恢复会话「上屏即续接」：挂在这里而不是只挂 activate()——冷启动
        // 后停在哪个会话上，那个会话压根不会收到 activate 调用，只挂那边的话
        // 「重开 GUI 后当前这个 ACP 会话仍要手点重新开始」。maybe_auto_resume
        // 自带一次性闸门，每帧调无副作用。
        if should_auto_resume_active_acp(self.sessions_restored)
            && let Some(SessionKind::Acp(view)) =
                self.sessions.get(self.active_session).map(|s| &s.kind)
        {
            let view = view.clone();
            view.update(cx, |v, cx| v.maybe_auto_resume(window, cx));
        }

        // 主内容（会话舞台）：统一舞台头 + 当前会话（分屏树 / ACP 消息流）。
        // 需 .flex()，否则单 pane 的叶子 flex_1 不生效、塌缩到内容高度（边框不到底）。
        // 旧右侧「结构面板」已被 inspector + 舞台头承接，不再渲染。
        let content = if self.sessions.get(self.active_session).is_some() {
            let stage_header = self.render_stage_header(!self.sidebar_open, right_reserve, cx);
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .children(stage_header)
                .child(
                    div().flex_1().min_w_0().min_h_0().flex().child(
                        match &self.sessions[self.active_session].kind {
                            SessionKind::Term { .. } => self.render_pane(
                                self.sessions[self.active_session]
                                    .term_layout()
                                    .expect("Term 会话必有 layout"),
                                "pane",
                                cx,
                            ),
                            // ACP 会话：整块主区就是消息流视图（无分屏树）。
                            SessionKind::Acp(view) => view.clone().into_any_element(),
                        },
                    ),
                )
        } else {
            // 空状态：引导用户新建会话 / 打开项目。
            let btn = |id: &'static str, label: &'static str| {
                div()
                    .id(id)
                    .px_3()
                    .py(px(6.))
                    .rounded_md()
                    .cursor_pointer()
                    .border_1()
                    .border_color(c_border)
                    .text_color(c_fg)
                    .text_sm()
                    .hover(|s| s.bg(c_border))
                    .child(label.to_string())
            };
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_4()
                .child(
                    Icon::new(IconName::SquareTerminal)
                        .size(px(40.))
                        .text_color(c_muted),
                )
                .child(div().text_color(c_muted).child("还没有会话"))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(btn("empty-new", "+ 新建会话").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _w, cx| this.new_tab(cx)),
                        ))
                        .child(btn("empty-open", "打开项目…").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _w, cx| this.open_project(cx)),
                        )),
                )
        };

        let stage_content: AnyElement = if self.primary_route == WorkspaceRoute::Tasks {
            self.render_tasks_page(window, cx).into_any_element()
        } else {
            match self.stage_override {
                // FILES 展开：跟停靠态完全同一份 UI（tab 横条 + EXPLORER 面板），
                // 只是占了舞台的宽度、会话内容不见了——不再换成另一套「返回条」
                // 组件，避免头部在展开前后判若两个应用。
                Some(MainView::Files) => v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_inspector_rail(!self.sidebar_open, right_edge, cx))
                    .child(self.render_inspector_files(window, cx))
                    .into_any_element(),
                // GIT 同理：展开只是复用停靠面板（rail + git_narrow_panel）铺满
                // 舞台宽度，改动列表 + diff / 日志子标签都还是同一份 UI，
                // 不再切到旧的「变更 + diff」返回条全屏页。
                Some(MainView::Git) => v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_inspector_rail(!self.sidebar_open, right_edge, cx))
                    .child(self.git_narrow_panel(window, cx))
                    .into_any_element(),
                Some(MainView::Skills) => v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_inspector_rail(!self.sidebar_open, right_edge, cx))
                    .child(self.render_inspector_skills(cx))
                    .into_any_element(),
                Some(v) => v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_stage_back_bar(v, !self.sidebar_open, right_reserve, cx))
                    .child(self.render_stage_override(v, window, cx))
                    .into_any_element(),
                None => content.into_any_element(),
            }
        };
        let stage = div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(stage_content);
        // 左栏始终保留为第 0 列。若关闭后卸载，再展开时 ResizableState 会把
        // 原第 0 列（舞台）的缓存短暂复用给新插入的左栏，造成最右侧闪过舞台残影。
        // 压到 1px 可以释放可见空间，同时保持三列身份与绘制缓存稳定。
        let sidebar_w = (sidebar_motion.progress * self.sidebar_w).max(1.);
        let sidebar_gap = if sidebar_motion.progress > 0.01 {
            px(4.)
        } else {
            px(0.)
        };
        let sidebar_min_w = if sidebar_motion.animating || !self.sidebar_open {
            px(1.)
        } else {
            px(200.)
        };
        let mut workspace_columns = h_resizable("workspace-columns")
            .with_state(&self.workspace_resize)
            .child(
                resizable_panel()
                    .size(px(sidebar_w))
                    .size_range(sidebar_min_w..Pixels::MAX)
                    .flex_none()
                    .pr(sidebar_gap)
                    .child(
                        workspace_frame::card(sidebar_surface)
                            // 内容避开浮在玻璃上的 macOS 交通灯；面板背景本身继续
                            // 延伸到窗口顶边，形成参考应用的一体化侧栏。
                            .pt(px(34.))
                            .child(
                                // 交通灯安全区也使用标题栏表面；否则只有左栏圆角里
                                // 露出较暗的侧栏底色，跟舞台/Inspector 的亮顶栏形成
                                // 不同的内轮廓，看起来就像三块用了不同半径。
                                workspace_frame::top_bar()
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .right_0()
                                    .h(px(34.)),
                            )
                            .opacity(sidebar_motion.progress.max(0.01))
                            .child(list_el),
                    ),
            );
        if sidebar_motion.animating {
            self.workspace_resize.update(cx, |state, cx| {
                state.resize_panel(0, px(sidebar_w), window, cx);
            });
        }
        // 舞台卡片（跟之前一样的玻璃卡片外观），作为 stage/inspector 内层分栏
        // 的左侧一块。
        // sidebar 收起时舞台卡片会变成贴着窗口最左边那块，真交通灯浮在它上面；
        // 这个安全区不再由这里整条 34px 往下挤（那样会把头栏推到单独一行，
        // 平白多出一截空白——之前踩过），而是交给舞台头/返回条/inspector 横条
        // 自己按 corner_guard 在同一行左边让出交通灯宽度，见 stage.rs /
        // inspector.rs 里 corner_guard 参数的用法。
        let stage_card = workspace_frame::card(stage_surface).child(stage);

        // 舞台 + inspector 用自己独立的一层 h_resizable（stage_inspector_resize），
        // 跟外层 sidebar|右侧区 的 workspace_resize 是两棵分开的树。这样底部抽屉
        // 才能包住"舞台+inspector"整体、一路铺到窗口最右边，而不只是舞台自己那一列。
        let stage_inspector: AnyElement = if let Some(inspector) = inspector_panel_el {
            let inspector_w = self.inspector_w;
            let progress = inspector_motion.progress;
            let panel_w = (progress * inspector_w).max(1.);
            let min_w = if inspector_motion.animating {
                px(1.)
            } else {
                px(280.)
            };

            let inspector_card = workspace_frame::card(sidebar_surface)
                // 同 stage_card：不再预留交通灯安全区，tab 横条直接顶到卡片顶边。
                .opacity(progress.max(0.01))
                .child(inspector);

            // 真正把中间区推走的一步：programmatically 顶宽，而不是只改这块自己
            // 的 flex_basis 建议值（那只对"首次插入"这一帧生效，见 gpui-component
            // resizable::panel 的 initial_size 规则）。
            if inspector_motion.animating {
                self.stage_inspector_resize.update(cx, |state, cx| {
                    state.resize_panel(1, px(panel_w), window, cx);
                });
            }

            h_resizable("stage-inspector-split")
                .with_state(&self.stage_inspector_resize)
                .child(
                    resizable_panel()
                        .size_range(px(400.)..Pixels::MAX)
                        .px(px(4.))
                        .child(stage_card),
                )
                .child(
                    resizable_panel()
                        .size(px(panel_w))
                        .size_range(min_w..Pixels::MAX)
                        .flex_none()
                        .pl(px(4.))
                        .child(inspector_card),
                )
                .into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .px(px(4.))
                .child(stage_card)
                .into_any_element()
        };

        // 底部抽屉包住"舞台+inspector"整个右侧区域，用真正的 v_resizable/
        // resizable_panel 上下分栏——用户能拖边框调抽屉高度，跟其余分栏是同一套
        // 组件/手感，不是自己算 opacity/height 画一个假的。只顶起右侧区自己的
        // 高度，左侧会话栏不受影响；宽度上则一路铺到窗口最右边。
        let right_region: AnyElement = if let Some(drawer) = bottom_drawer_el {
            let drawer_h = (bottom_drawer_motion.progress * self.bottom_drawer_h).max(1.);
            let min_h = if bottom_drawer_motion.animating {
                px(1.)
            } else {
                px(120.)
            };
            // 展开/收起动画期间靠 resize_panel 逐帧程序化改高度（跟 inspector
            // 入场动画同一招）；动画结束后交还给用户真实拖拽。
            if bottom_drawer_motion.animating {
                self.bottom_drawer_resize.update(cx, |state, cx| {
                    state.resize_panel(1, px(drawer_h), window, cx);
                });
            }
            v_resizable("bottom-drawer-split")
                .with_state(&self.bottom_drawer_resize)
                .child(
                    resizable_panel()
                        .size_range(px(80.)..Pixels::MAX)
                        // 跟左右分栏的 px(4.)/pl(4.) 同理：给拖拽手柄留一道死区缓冲，
                        // 不然舞台卡片贴着抽屉零缝隙，手柄命中区只有 ~9px 高，稍微
                        // 手抖点空就落到下面终端/聊天文本上触发原生文字选区（蓝底）。
                        .pb(px(4.))
                        .child(stage_inspector),
                )
                .child(
                    resizable_panel()
                        .size(px(drawer_h))
                        .size_range(min_h..px(560.))
                        .pt(px(4.))
                        .child(drawer),
                )
                .into_any_element()
        } else {
            stage_inspector
        };

        workspace_columns = workspace_columns.child(
            resizable_panel()
                .size_range(px(400.)..Pixels::MAX)
                .child(right_region),
        );

        // 命令面板弹层：搜索框 + 候选列表全部由 ListState 渲染。
        let palette_overlay = self.palette.as_ref().map(|state| {
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(80.))
                // 点背景空白处关闭面板
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_palette(window, cx)),
                )
                .child(
                    div()
                        // 点面板内部不冒泡到背景，避免误关
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w(px(480.))
                        .h(px(360.))
                        .flex()
                        .flex_col()
                        .bg(ui_theme::glass_floating())
                        .border_1()
                        .border_color(c_border)
                        .rounded_lg()
                        .shadow_lg()
                        .child(List::new(state).search_placeholder("输入命令…")),
                )
        });

        let image_preview_overlay = self.acp_image_preview.as_ref().map(|image| {
            let image = image.clone();
            div()
                .id("workspace-image-preview-backdrop")
                .absolute()
                .inset_0()
                .bg(rgba(0x000000d9))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.acp_image_preview = None;
                    cx.notify();
                }))
                .child(
                    div()
                        .id("workspace-image-preview-content")
                        .w(relative(0.86))
                        .h(relative(0.84))
                        .cursor_default()
                        .on_click(|_ev, _window, cx| cx.stop_propagation())
                        .child(
                            img(image)
                                .size_full()
                                .object_fit(ObjectFit::Contain)
                                .rounded_md(),
                        ),
                )
                .child(
                    div()
                        .id("workspace-image-preview-close")
                        .absolute()
                        .top(px(48.))
                        .right(px(48.))
                        .size(px(34.))
                        .rounded_full()
                        .border_1()
                        .border_color(rgba(0xffffff33))
                        .bg(rgba(0x181818ee))
                        .text_color(rgb(0xffffff))
                        .text_lg()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|d| d.bg(rgba(0x303030ff)))
                        .child("×")
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            this.acp_image_preview = None;
                            cx.notify();
                        })),
                )
        });

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(shell_surface)
            .font_family(terminal_view::font_family())
            // 见 focus_handle 字段注释：非终端页面没有可聚焦的子元素时，靠这个把
            // window 的 focus 兜底钉在这层，保证下面的全局 on_key_down 收得到事件。
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Quit, _window, cx| {
                this.show_quit_confirm = true;
                cx.notify();
            }))
            // Cmd+, / 应用菜单「设置…」：跟齿轮图标共用同一个独立设置窗口。
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                if this.llm_inputs.is_none() {
                    this.init_llm_inputs(window, cx);
                }
                // 不动 nonce：窗口已开着就保持用户当前所在页，只是把它提到前台；
                // 但下次新开窗口得回到外观页，不能停在「检查更新…」跳过去的那页。
                this.settings_page_ix = SETTINGS_PAGE_APPEARANCE;
                this.open_settings_window(cx);
            }))
            // Cmd+Shift+N：全局新建任务（侧栏任务列表 + 弹窗）。
            .on_action(cx.listener(|this, _: &NewTask, window, cx| {
                this.open_new_task_modal(window, cx);
            }))
            // Cmd+↑ / Cmd+↓：上/下一个会话（按侧栏视觉顺序）。
            .on_action(cx.listener(|this, _: &PrevSession, window, cx| {
                this.cycle_session(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextSession, window, cx| {
                this.cycle_session(1, window, cx);
            }))
            // 应用菜单「检查更新…」：顺手发起一次检查，再把设置窗口开到「更新」页看进度。
            .on_action(cx.listener(|this, _: &CheckForUpdate, window, cx| {
                if this.llm_inputs.is_none() {
                    this.init_llm_inputs(window, cx);
                }
                if !matches!(
                    this.update_status,
                    updater::UpdateStatus::Checking
                        | updater::UpdateStatus::Downloading { .. }
                        | updater::UpdateStatus::Installing { .. }
                ) {
                    this.check_for_update(false, cx);
                }
                this.settings_page_ix = SETTINGS_PAGE_UPDATE;
                this.settings_page_nonce += 1;
                this.open_settings_window(cx);
            }))
            // 应用菜单「反馈问题…」：跳 GitHub issue 模板选择页。
            .on_action(cx.listener(|_this, _: &ReportIssue, _window, cx| {
                cx.open_url("https://github.com/smelt-ai/smelt/issues/new/choose");
            }))
            // 文件内容视图右键菜单里的「发送选中内容到终端」，见 send_open_file_selection。
            .on_action(
                cx.listener(|this, _: &SendSelectionToTerminal, _window, cx| {
                    this.send_open_file_selection(cx);
                }),
            )
            // 全局快捷键：Cmd+K 面板 / Cmd+B 侧栏 / Cmd+[ ] 切当前会话内的 pane /
            // Cmd+1~9 跳到第 N 个会话（键位分工对齐 iTerm2）
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                let ks = &ev.keystroke;
                if ks.key == "escape" && this.acp_image_preview.take().is_some() {
                    cx.notify();
                    return;
                }
                // 文件树键盘导航：搜索框 / 编辑器聚焦时不抢键。
                if matches!(this.stage_override, Some(MainView::Files))
                    && !ks.modifiers.platform
                    && !ks.modifiers.control
                {
                    use gpui::Focusable;
                    let search_focused = this
                        .file_filter
                        .as_ref()
                        .is_some_and(|s| s.read(cx).focus_handle(cx).is_focused(window));
                    let editor_focused = this
                        .open_file
                        .as_ref()
                        .is_some_and(|of| of.editor.read(cx).focus_handle(cx).is_focused(window));
                    if !search_focused && !editor_focused {
                        match ks.key.as_str() {
                            "up" => {
                                this.file_tree_move_selection(-1, cx);
                                return;
                            }
                            "down" => {
                                this.file_tree_move_selection(1, cx);
                                return;
                            }
                            "left" => {
                                this.file_tree_key_left(cx);
                                return;
                            }
                            "right" => {
                                this.file_tree_key_right(window, cx);
                                return;
                            }
                            "enter" => {
                                this.file_tree_key_enter(window, cx);
                                return;
                            }
                            _ => {}
                        }
                    }
                }
                // Git 页 F7 / Shift+F7：在改动块之间跳（对齐 JetBrains 的 next/previous
                // difference）。不带 Cmd，所以要赶在下面的 platform 判断之前处理。
                // diff 现在停靠 / 展开态都能内嵌显示，这个快捷键两种态都该生效。
                if (matches!(this.stage_override, Some(MainView::Git))
                    || (this.inspector_open && this.inspector_tab == inspector::InspectorTab::Git))
                    && this.git_tab == GitTab::Changes
                    && ks.key == "f7"
                    && !ks.modifiers.platform
                {
                    this.jump_hunk(!ks.modifiers.shift, cx);
                    return;
                }
                // Esc：只收掉 session route 内的钻取/展开页；任务是独立一级 route。
                if ks.key == "escape"
                    && this.primary_route == WorkspaceRoute::Session
                    && this.stage_override.is_some()
                    && this.palette.is_none()
                    && this.rename_target.is_none()
                    && !this.show_new_task_modal
                    && !this.show_quit_confirm
                    && this.delete_worktree_target.is_none()
                    && this.close_project_target.is_none()
                {
                    this.set_stage_override(None, window, cx);
                    return;
                }
                if !ks.modifiers.platform {
                    return;
                }
                match ks.key.as_str() {
                    "k" => {
                        if this.palette.is_some() {
                            this.close_palette(window, cx);
                        } else {
                            this.open_palette(window, cx);
                        }
                    }
                    // Cmd+B：inspector 面板显隐（旧语义是左侧栏；会话列表现在常驻）。
                    "b" => {
                        this.set_inspector_open(!this.inspector_open);
                        this.save_state(cx);
                        cx.notify();
                    }
                    // 切当前会话内的活动 pane（分屏），不是切会话——切会话见下面的 Cmd+1~9。
                    "[" => this.cycle_pane(-1, window, cx),
                    "]" => this.cycle_pane(1, window, cx),
                    // Cmd+1~9：跳到会话列表里第 N 个会话——按列表显示顺序（各项目
                    // 分组依次铺平）数，所见即所得；超出会话数就什么都不做。
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" => {
                        let n = (ks.key.as_bytes()[0] - b'1') as usize;
                        let visible: Vec<usize> = this
                            .project_groups(cx)
                            .into_iter()
                            .flat_map(|g| g.sessions)
                            .collect();
                        if let Some(&ix) = visible.get(n) {
                            this.activate(ix, window, cx);
                        }
                    }
                    // Cmd+D 竖切（右侧并排）/ Cmd+Shift+D 横切（下方堆叠）
                    "d" => {
                        let axis = if ks.modifiers.shift {
                            Axis::Vertical
                        } else {
                            Axis::Horizontal
                        };
                        this.split_active(axis, cx);
                    }
                    // Cmd+W 关闭当前 pane；会话只剩一个 pane 时关掉整个会话（至少留一个会话）
                    "w" => this.close_active(window, cx),
                    // Cmd+S：保存文件树里打开的文件（仅 Files 页，避免切到别的
                    // 视图时背着用户悄悄写盘）。
                    "s" if matches!(this.stage_override, Some(MainView::Files)) => {
                        this.save_open_file(cx)
                    }
                    // Cmd+Shift+F 切换调试 HUD（右上角帧率 + 内存）
                    "f" if ks.modifiers.shift => {
                        this.debug_hud = !this.debug_hud;
                        this.fps_ema = 0.0;
                        this.last_frame = None;
                        this.debug_mem_rss = None;
                        this.debug_mem_sampled_at = None;
                        cx.notify();
                    }
                    // Cmd+Q 退出交给应用菜单的 Quit action（全局绑定，见 main）
                    _ => {}
                }
            }))
            // 主体先绘制；透明标题栏随后覆盖在顶部 safe area 上，避免面板背景
            // 把铃铛和 Inspector 开关压成若隐若现的轮廓。
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .p(px(8.))
                    .bg(shell_surface)
                    .child(
                        div()
                            .w_0()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .flex()
                            .relative()
                            .child(workspace_columns),
                    ),
            )
            // 无独立标题栏：透明拖拽层浮在三栏玻璃卡片上。红绿灯由 macOS 原生绘制，
            // 位置在 open_workspace_window 里配置；外层整体加了 8px 顶部外边距
            // （四周对称，见上面 shell 的 `.p(px(8.))`），卡片顶边比窗口顶边低了
            // 8px，这层悬浮层跟着往下多留 8px（34→42），红绿灯/图标才能继续落在
            // 卡片里同一个相对位置，而不是飘进新让出来的外边距空白里。
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(42.))
                    .pl(px(80.))
                    .flex()
                    .items_center()
                    .justify_end()
                    .window_control_area(WindowControlArea::Drag)
                    .on_mouse_down(MouseButton::Left, |event, window, _| {
                        if event.click_count == 2 {
                            window.titlebar_double_click();
                        } else {
                            // `window_control_area(Drag)` 只是给平台一个提示，实际拖动
                            // 还是得手动起一个原生窗口移动会话（同 gpui_component::TitleBar
                            // 的做法），不然按住鼠标完全不会动窗口。
                            window.start_window_move();
                        }
                    })
                    .bg(gpui::transparent_black())
                    .child(
                        div()
                            .id("sidebar-toggle")
                            .absolute()
                            .left(px(92.))
                            // 侧栏展开时悬浮在 session_list 的 34px 顶部导航行上；
                            // 收起时悬浮在 stage 头上——两种状态现在都是 34px 高，
                            // 5px 本来就居中；卡片整体下移了 8px（见上面注释），
                            // 这里跟着 +8。
                            .top(px(13.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size_6()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(rgb(ui_theme::text_mid()))
                            .hover(|s| s.bg(ui_theme::overlay(0x18)))
                            .child(
                                Icon::new(if self.sidebar_open {
                                    IconName::PanelLeftClose
                                } else {
                                    IconName::PanelLeftOpen
                                })
                                .size_4(),
                            )
                            .tooltip(|window, cx| {
                                gpui_component::tooltip::Tooltip::new("切换左侧栏")
                                    .build(window, cx)
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _window, cx| {
                                    cx.stop_propagation();
                                    this.sidebar_open = !this.sidebar_open;
                                    this.sidebar_transition.set_open(this.sidebar_open);
                                    this.save_state(cx);
                                    cx.notify();
                                }),
                            ),
                    )
                    .child(
                        // 右侧：面板入口与侧边面板开关。stop_propagation 避免触发拖拽。
                        h_flex()
                            .h_full()
                            .items_center()
                            .gap_1()
                            // 留出右侧呼吸间距，别让按钮贴到窗口边缘。跟左边红绿灯
                            // 同一套「离卡片边缘 10px」的间距（卡片边缘=8px shell
                            // 外边距，紧贴 8px 会显得太靠边），原来的 8px 太紧。
                            .pr(px(10.))
                            .child({
                                let promoted = self.inspector_panel_promoted();
                                div()
                                    .id("inspector-fullscreen-toggle")
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size_6()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_color(rgb(ui_theme::text_mid()))
                                    .when(promoted, |s| s.bg(ui_theme::overlay(0x18)))
                                    .hover(|s| s.bg(ui_theme::overlay(0x18)))
                                    .child(
                                        Icon::new(if promoted {
                                            IconName::Minimize
                                        } else {
                                            IconName::Maximize
                                        })
                                        .size_4(),
                                    )
                                    .tooltip(move |window, cx| {
                                        gpui_component::tooltip::Tooltip::new(if promoted {
                                            "收回右侧面板"
                                        } else {
                                            "全屏显示右侧面板"
                                        })
                                        .build(window, cx)
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            if promoted {
                                                this.set_stage_override(None, window, cx);
                                                this.set_inspector_open(true);
                                            } else if let Some(view) =
                                                this.inspector_tab.stage_view()
                                            {
                                                this.set_stage_override(Some(view), window, cx);
                                            }
                                            this.save_state(cx);
                                            cx.notify();
                                        }),
                                    )
                            })
                            .child({
                                // 底部抽屉（快捷终端）开关：跟右侧面板开关同一排，
                                // 图标用 PanelBottom 系列区分「从下面拉出来」。
                                let drawer_open = self.bottom_drawer_open;
                                let e_drawer = cx.entity();
                                div()
                                    .id("bottom-drawer-toggle")
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size_6()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_color(rgb(ui_theme::text_mid()))
                                    .when(drawer_open, |s| s.bg(ui_theme::overlay(0x18)))
                                    .hover(|s| s.bg(ui_theme::overlay(0x18)))
                                    .child(
                                        // panel-bottom.svg 没有箭头，只有一根贴着按钮
                                        // 自身圆角边框的横线，16px 下几乎看不出来
                                        // （之前"展开后 icon 消失"就是这个）。统一用
                                        // 带箭头的 panel-bottom-open 图标，展开时把箭头
                                        // 转 180°变成朝下（收起提示），跟右侧面板开关的
                                        // 双图标一样全程都有清晰可辨的箭头。
                                        Icon::new(IconName::PanelBottomOpen)
                                            .size_4()
                                            .when(drawer_open, |icon| {
                                                icon.rotate(gpui::Radians(std::f32::consts::PI))
                                            }),
                                    )
                                    .tooltip(|window, cx| {
                                        gpui_component::tooltip::Tooltip::new("终端面板")
                                            .build(window, cx)
                                    })
                                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                        cx.stop_propagation();
                                        e_drawer.update(cx, |this, cx| {
                                            this.toggle_bottom_drawer(cx);
                                        });
                                    })
                            })
                            .child({
                                // 面板概念上就是「侧边栏」：展开到舞台全屏只是变宽，
                                // 内容本质没变——所以高亮态看「是否有内容在显示」，
                                // 停靠和全屏都算，不再因为提升到舞台就显示成关闭态。
                                let panel_visible =
                                    self.inspector_open || self.inspector_panel_promoted();
                                div()
                                    .id("inspector-toggle")
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size_6()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_color(rgb(ui_theme::text_mid()))
                                    .when(panel_visible, |s| s.bg(ui_theme::overlay(0x18)))
                                    .hover(|s| s.bg(ui_theme::overlay(0x18)))
                                    .child(
                                        Icon::new(if panel_visible {
                                            IconName::PanelRightClose
                                        } else {
                                            IconName::PanelRightOpen
                                        })
                                        .size_4(),
                                    )
                                    .tooltip(|window, cx| {
                                        gpui_component::tooltip::Tooltip::new("切换侧边面板  ⌘B")
                                            .build(window, cx)
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            cx.stop_propagation();
                                            // 全屏展开时点这颗按钮 = 直接完全收起（退出
                                            // 全屏 + 关闭面板），不是退回停靠态——它本质
                                            // 还是「侧边栏开关」，点了就该整个消失。
                                            if this.inspector_panel_promoted() {
                                                this.set_stage_override(None, window, cx);
                                                this.set_inspector_open(false);
                                            } else {
                                                this.set_inspector_open(!this.inspector_open);
                                            }
                                            this.save_state(cx);
                                            cx.notify();
                                        }),
                                    )
                            }),
                    ),
            )
            // 命令面板（最上层）
            .children(palette_overlay)
            // 退出确认拦截弹层
            .children(self.show_quit_confirm.then(|| self.render_quit_confirm(cx)))
            // 会话重命名拦截弹层
            .children(
                self.rename_target
                    .is_some()
                    .then(|| self.render_rename_session(cx)),
            )
            // 新建任务弹窗
            .children(
                self.show_new_task_modal
                    .then(|| self.render_new_task_modal(cx)),
            )
            // 删除 Worktree 确认拦截弹层
            .children(
                self.delete_worktree_target
                    .is_some()
                    .then(|| self.render_delete_worktree_confirm(cx)),
            )
            // 关闭项目确认拦截弹层（会连带 kill 掉它下面的会话）
            .children(
                self.close_project_target
                    .is_some()
                    .then(|| self.render_close_project_confirm(cx)),
            )
            .children(
                self.discard_hunk_target
                    .is_some()
                    .then(|| self.render_discard_hunk_confirm(cx)),
            )
            .children(
                self.discard_file_target
                    .is_some()
                    .then(|| self.render_discard_file_confirm(cx)),
            )
            .children(
                self.discard_all_target
                    .is_some()
                    .then(|| self.render_discard_all_confirm(cx)),
            )
            .children(
                self.delete_branch_target
                    .is_some()
                    .then(|| self.render_delete_branch_confirm(cx)),
            )
            // 删除文件二次确认拦截弹层
            .children(
                self.delete_file_target
                    .is_some()
                    .then(|| self.render_delete_file_confirm(cx)),
            )
            // 新建/编辑 skill 弹窗
            .children(
                self.skill_modal
                    .is_some()
                    .then(|| self.render_skill_modal(cx)),
            )
            // 删除 skill 二次确认拦截弹层
            .children(
                self.skill_delete_target
                    .is_some()
                    .then(|| self.render_delete_skill_confirm(cx)),
            )
            // 管理 skill 在各 agent 下的链接。
            .children(
                self.skill_link_modal
                    .is_some()
                    .then(|| self.render_skill_link_modal(cx)),
            )
            // 重启守护确认弹层改挂在设置窗（SettingsWindow::render），不在主窗口画。
            // Finder 拖文件/文件夹：只在有拖拽时叠全窗 drop 层。
            // 常驻 hitbox 会盖住按钮（「新建终端」像没反应）；对齐「有 drag 才出现」。
            // 终端 hitbox 会挡住根 on_drop，所以必须用上层目标接 ExternalPaths。
            .when(
                cx.has_active_drag()
                    && !matches!(
                        cx.active_drag_cursor_style(),
                        Some(CursorStyle::ResizeColumn | CursorStyle::ResizeLeftRight)
                    ),
                |root| {
                    root.child(
                        div()
                            .id("file-drop-overlay")
                            .absolute()
                            .inset_0()
                            .bg(ui_theme::tint(ui_theme::blue(), 0x28))
                            .border_2()
                            .border_color(rgb(ui_theme::blue()))
                            .on_drop::<ExternalPaths>(cx.listener(
                                |this, ep: &ExternalPaths, _window, cx| {
                                    this.open_paths(ep.paths(), cx);
                                },
                            )),
                    )
                },
            )
            // 文件未保存切换确认拦截弹层
            .children(
                self.pending_file_switch
                    .clone()
                    .map(|target| self.render_unsaved_file_confirm(target, cx)),
            )
            // gpui-component 的 Root 负责保存 toast 队列，但当前版本不会在 Root::render
            // 里自动挂通知层。显式画到工作区最外层，否则 push_notification 成功后只会
            // 留在队列里：toast 弹不出来。
            .children(gpui_component::Root::render_notification_layer(window, cx))
            // 调试 HUD：右上角帧率 + 帧耗时 + RSS（Cmd+Shift+F 切换）
            .children(self.debug_hud.then(|| {
                let fps = self.fps_ema;
                let ms = if fps > 0.0 { 1000.0 / fps } else { 0.0 };
                let mem = self
                    .debug_mem_rss
                    .map(mem_usage::format_rss)
                    .unwrap_or_else(|| "—".into());
                // 帧率健康度着色：≥55 绿、≥30 黄、否则红。
                let color = if fps >= 55.0 {
                    rgb(ui_theme::green())
                } else if fps >= 30.0 {
                    rgb(ui_theme::yellow())
                } else {
                    rgb(ui_theme::red())
                };
                div()
                    .absolute()
                    .top(px(40.))
                    .right(px(12.))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(ui_theme::tint(ui_theme::bg_card(), 0xcc))
                    .border_1()
                    .border_color(ui_theme::overlay(0x22))
                    .font_family(terminal_view::font_family())
                    .text_xs()
                    .text_color(color)
                    .child(format!("{fps:.0} FPS · {ms:.1} ms · RSS {mem}"))
            }))
            // 图片预览必须最后挂载，覆盖标题栏、两侧栏、通知和调试层。
            .children(image_preview_overlay)
    }
}

/// 主区占位视图（文件树 / Git 尚未实现）。
fn placeholder_view(text: &str, muted: Hsla) -> Div {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_color(muted)
        .child(text.to_string())
}

/// 旧存档没记 agent 种类时，从启动命令反推一把（命令里出现过 copilot / codex
/// 字样就归给它们）；认不出当 Claude——多 agent 之前的存档只可能是它。
fn acp_agent_from_cmd(cmd: &str) -> settings::AcpAgentKind {
    let c = cmd.to_ascii_lowercase();
    if c.contains("copilot") {
        settings::AcpAgentKind::Copilot
    } else if c.contains("codex") {
        settings::AcpAgentKind::Codex
    } else if c.contains("grok") {
        settings::AcpAgentKind::Grok
    } else {
        settings::AcpAgentKind::Claude
    }
}

/// 当前工作目录字符串。
fn current_dir() -> Option<String> {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
}

/// 临时终端的落脚目录：固定用 $HOME，跟任何项目区分开、且多个临时终端共享同一
/// 目录字符串，侧栏才能按 cwd 分组把它们聚成一组（见 render 里的 `is_scratch_cwd`）。
fn scratch_dir() -> Option<String> {
    dirs::home_dir().and_then(|p| p.to_str().map(String::from))
}

/// cwd → 侧栏项目分组显示名，统一取目录末段——scratch_dir 就是 $HOME，末段天然是
/// 用户名（比如 c.chen），不用再特判成「临时终端」这种跟其他项目组风格不一致的名字。
/// Workspace::project_groups（侧栏渲染）和拖拽排序（找会话/插入点归属的项目）共用。
fn project_name_for_cwd(cwd: &str) -> String {
    cwd.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("项目")
        .to_string()
}

/// file:// URL → 本地路径（percent 解码，支持中文 / 空格目录名）。
fn file_url_to_path(url: &str) -> Option<std::path::PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // 跳过可能的 host 段（file://localhost/…），从首个 '/' 起才是路径。
    let path = &rest[rest.find('/')?..];
    let b = path.as_bytes();
    let mut bytes = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).ok()?, 16) {
                bytes.push(v);
                i += 3;
                continue;
            }
        }
        bytes.push(b[i]);
        i += 1;
    }
    Some(std::path::PathBuf::from(String::from_utf8(bytes).ok()?))
}

/// ACP Markdown 的内部文件链接。使用独立 scheme，避免 `file://` 被 macOS
/// LaunchServices 交给外部应用；`#L42` 可选片段用于定位行号。
fn smelt_file_url_to_target(url: &str) -> Option<(String, Option<usize>)> {
    let rest = url.strip_prefix("smelt-file://")?;
    let (path_part, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let path = file_url_to_path(&format!("file://{path_part}"))?;
    let path = path.to_str()?.to_string();
    let line = fragment
        .strip_prefix('L')
        .and_then(|value| value.parse().ok());
    Some((path, line))
}

#[cfg(test)]
mod internal_file_url_tests {
    use super::smelt_file_url_to_target;

    #[test]
    fn decodes_path_and_optional_line() {
        assert_eq!(
            smelt_file_url_to_target("smelt-file:///tmp/a%20b.rs#L42"),
            Some(("/tmp/a b.rs".into(), Some(42)))
        );
        assert!(smelt_file_url_to_target("https://example.com/a.rs").is_none());
    }
}

/// 开一扇主工作台窗口（Workspace + Root 包装），返回其 weak 引用。
/// 首启和「点 Dock 图标重开」共用这一份：`Workspace::new` 本来就会从存档 + smeltd
/// 重新拼出会话布局，跟正常重启应用效果一致。
fn open_workspace_window(
    cx: &mut App,
    window_bg: WindowBackgroundAppearance,
) -> WeakEntity<Workspace> {
    let window_options = WindowOptions {
        // 透明标题栏：红绿灯浮在内容上，拖拽 / 双击最大化由自定义 TitleBar 接管。
        // 不直接用 TitleBar::title_bar_options()（它的 traffic_light_position
        // 是 (9, 9)，假设卡片贴着窗口顶边、且左边距很窄）——我们整体加了 8px
        // 顶部外边距后卡片下移了 8px，红绿灯跟着往下挪 8px；同时把 x 从 9
        // 加大到 18，让它在卡片左边缘有跟侧栏内容（mx_2 + px_2 = 16px）
        // 相近的留白，别紧贴卡片边框显得太靠左。
        titlebar: Some(TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: Some(gpui::point(px(18.0), px(17.0))),
        }),
        // 透明/模糊背景（跟随外观设置；终端底色带 alpha 时桌面透出）。
        window_background: window_bg,
        ..Default::default()
    };
    let mut workspace = None;
    cx.open_window(window_options, |window, cx| {
        // 界面文字（侧边栏/标签页/状态栏等）用的都是 text_xs/text_sm 这类相对 rem
        // 单位，默认 rem_size=16px 偏小；这里统一调大，全局跟着等比例放大，不用
        // 逐个改 .text_xs()/.text_sm()。终端内容本身的字号另由 terminal_view.rs
        // 的 FONT_PX 控制，不受这个影响。
        window.set_rem_size(px(19.));
        let view = cx.new(|cx| Workspace::new(window, cx));
        workspace = Some(view.clone());
        // 顶层视图必须包一层 Root（组件库的主题/遮罩系统要求）。
        cx.new(|cx| Root::new(view, window, cx))
    })
    .expect("打开窗口失败");
    workspace.expect("回调里一定会设置 workspace").downgrade()
}

fn main() {
    // with_assets 注册组件库图标资源，Sidebar 的 IconName svg 才能渲染。
    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);
    // Dock / Finder「打开」投递的 file:// URL（拖文件夹到 Dock 图标、右键用 Smelt 打开）。
    // 回调里没有 cx，经 channel 转发；unbounded 会缓存首启动时窗口建好前到达的 URL。
    let (url_tx, url_rx) = smol::channel::unbounded::<Vec<String>>();
    app.on_open_urls(move |urls| {
        let _ = url_tx.send_blocking(urls);
    });
    // 菜单栏常驻图标/下拉菜单点击：见 status_item.rs 顶部注释，回调发生在纯 AppKit 层
    // （没有 GPUI 的 cx），一样经 channel 转发到下面 run() 里 drain。
    let (status_tx, status_rx) = smol::channel::unbounded::<status_item::StatusItemEvent>();

    // 当前存活的主窗口（weak，随窗口关闭自然失效）。首启时在 run() 里写入；
    // URL 投递循环和「点 Dock 图标重开」都读它判断当前有没有主窗口。
    // on_reopen 得在 run() 之前挂在 Application builder 上（跟 on_open_urls 一样），
    // 但它触发时 run() 早已跑起来，Rc 到时候已经被 run() 里的首启逻辑填过了。
    let current_ws: Rc<RefCell<Option<WeakEntity<Workspace>>>> = Rc::new(RefCell::new(None));
    {
        let current_ws = current_ws.clone();
        // 点 Dock 图标 / 双击程序图标重开：GPUI 只在系统判定「没有可见窗口」时才会调这个
        // 回调（宠物浮窗一直挂着，是否会被系统计入可见窗口未经验证，这里做好兜底：
        // 主窗口还活着就什么都不做，只有真的没了才重新开一扇）。
        app.on_reopen(move |cx| {
            let alive = current_ws
                .borrow()
                .as_ref()
                .is_some_and(|w| w.upgrade().is_some());
            if !alive {
                let window_bg = cx
                    .try_global::<Appearance>()
                    .map(|a| a.window_bg())
                    .unwrap_or(WindowBackgroundAppearance::Opaque);
                let ws = open_workspace_window(cx, window_bg);
                *current_ws.borrow_mut() = Some(ws);
            }
        });
    }

    app.run(move |cx| {
        // 用任何 gpui-component 功能前必须先初始化。
        gpui_component::init(cx);
        // Markdown 中的本地文件链接使用 smelt-file://，由 on_open_urls 回流到
        // Workspace 的内置编辑器。打包时 Info.plist 也声明该 scheme；运行时注册
        // 用于已安装应用升级后无需重启 LaunchServices 数据库。
        let register_file_scheme = cx.register_url_scheme("smelt-file");
        cx.spawn(async move |_cx| {
            if let Err(error) = register_file_scheme.await {
                eprintln!("[workspace] 注册 smelt-file URL scheme 失败：{error}");
            }
        })
        .detach();
        // 内嵌终端默认字体 JetBrainsMono Nerd Font Mono（Regular/Bold），Ghostty 同款
        // 思路：默认字体自己带，不赌用户装没装——任何机器上默认字体族都能解析成功，
        // 杜绝"没装字体 → 测量/渲染各自 fallback 到不同字体 → 列宽错乱"。它是打过
        // Nerd Font 补丁的完整版，自带全部图标码位，兼任图标 fallback（用户在设置页
        // 自选的字体缺图标时落到它，见 terminal_view::terminal_font）。
        cx.text_system()
            .add_fonts(vec![
                std::borrow::Cow::Borrowed(
                    include_bytes!("../../../assets/fonts/JetBrainsMonoNerdFontMono-Regular.ttf")
                        .as_slice(),
                ),
                std::borrow::Cow::Borrowed(
                    include_bytes!("../../../assets/fonts/JetBrainsMonoNerdFontMono-Bold.ttf")
                        .as_slice(),
                ),
            ])
            .expect("加载内嵌字体失败");
        // 应用菜单栏：macOS 顶部「Smelt」菜单，含「设置… ⌘,」+「退出 Smelt ⌘Q」
        // （跟齿轮图标一样开独立设置窗口，符合 mac 惯例——系统偏好设置一般都在这）。
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new("cmd-shift-n", NewTask, None),
            // 会话上/下切换（跨项目按侧栏视觉顺序，到头循环）。用 cmd+方向键：
            // 比 cmd-shift-[ 好按，且不会串进 shell——终端的 keystroke_to_bytes 开头
            // 就是 `if m.platform { return None }`，带 cmd 的键一律不发给 PTY；
            // gpui-component 输入框也只绑了裸 up/down，没绑 cmd-up/cmd-down。
            // 原有的 cmd-[ / cmd-] 保持原样，不覆盖、不接管。
            KeyBinding::new("cmd-up", PrevSession, None),
            KeyBinding::new("cmd-down", NextSession, None),
            // 把 Tab/Shift-Tab 从 gpui-component Root 的全局焦点跳转手里要回来，
            // 终端聚焦时改发给 shell（见 terminal_view.rs 里 TerminalTab 的注释）。
            KeyBinding::new("tab", terminal_view::TerminalTab, Some("Terminal")),
            KeyBinding::new(
                "shift-tab",
                terminal_view::TerminalBackTab,
                Some("Terminal"),
            ),
        ]);
        cx.set_menus(vec![Menu::new("Smelt").items([
            MenuItem::action("新建任务…", NewTask),
            MenuItem::action("检查更新…", CheckForUpdate),
            MenuItem::Separator,
            MenuItem::action("设置…", OpenSettings),
            MenuItem::action("反馈问题…", ReportIssue),
            MenuItem::Separator,
            MenuItem::action("退出 Smelt", Quit),
        ])]);

        // 外观设置：读盘设为全局单例，据此确定窗口背景外观（透明 / 模糊）。
        // 主题模式在建窗口之前落地，首帧就是对的，不会先闪一下深色再变浅。
        let appearance = load_appearance();
        let window_bg = appearance.window_bg();
        settings::apply_theme_mode(appearance.theme_mode, cx);
        terminal_view::set_font_px(appearance.font_px);
        terminal_view::set_font_family(&appearance.font_family);
        cx.set_global(appearance);
        cx.set_global(load_launch_config());

        // 桌面宠物：配置 + 播报邮箱 + LLM 大脑配置（跨窗口全局单例），再开独立透明浮窗。
        cx.set_global(pet::load_pet_config());
        cx.set_global(pet::PetMailbox::default());
        cx.set_global(agent::load_llm_config());
        pet::open_pet_window(cx);

        // 状态通道：常驻订阅守护的 subscribe，维护 DaemonStates 全局单例，
        // Session::status/pane_status 靠它把"猜"换成"读事实"（见
        // docs/state-channel-plan.md）。阻塞的 socket 读循环放专门的 OS 线程，
        // 断线/守护没起来就等一下重连；smol::channel 两头都能用（OS 线程用
        // try_send，GPUI 任务用 async recv），跟 terminal.rs 的 redraw_tx/rx
        // 是同一个搭桥模式。
        let daemon_states = DaemonStates::default();
        cx.set_global(daemon_states.clone());
        let attention = AttentionGlobal::default();
        cx.set_global(attention.clone());
        let agent_ui_config = settings::load_agent_ui_config();
        let agent_hooks_enabled = agent_ui_config.agent_hooks_enabled;
        cx.set_global(agent_ui_config);
        // hook 配置和 helper 必须作为一个版本单元升级：先把 App 内最新版同步到稳定
        // managed 路径，再改写各 provider 的 hook 命令。失败时保留旧 helper，不阻塞 GUI。
        if let Err(error) = settings::sync_bundled_smelt_notify() {
            eprintln!("[workspace] 同步 smelt-notify 失败：{error}");
        }
        // 新安装和缺少开关的旧配置都默认开启托管 hooks；只有用户明确关闭时跳过。
        // 安装含 Codex app-server 信任握手和文件 IO，全部放后台，不能阻塞首帧。
        if agent_hooks_enabled {
            thread::spawn(|| {
                if let Err(error) = settings::install_agent_hooks() {
                    eprintln!("[workspace] 自动安装 Agent hooks 失败：{error}");
                }
            });
        }
        let (daemon_state_tx, daemon_state_rx) =
            smol::channel::unbounded::<terminal::DaemonStateEvent>();
        thread::spawn(move || {
            loop {
                terminal::subscribe_daemon_states_blocking(&daemon_state_tx);
                thread::sleep(Duration::from_secs(2)); // 断线/连不上，等一下重试
            }
        });
        cx.spawn(async move |cx| {
            while let Ok(event) = daemon_state_rx.recv().await {
                let _ = cx.update(|cx| {
                    let states = cx.global::<DaemonStates>().0.clone();
                    let attention = cx.global::<AttentionGlobal>().0.clone();
                    {
                        let mut map = states.lock().unwrap();
                        match event {
                            terminal::DaemonStateEvent::Snapshot(list) => {
                                for s in &list {
                                    smelt_core::attention::apply_daemon_transition(
                                        &mut attention.lock().unwrap(),
                                        map.get(&s.id).map(|p| p.phase),
                                        s,
                                        Instant::now(),
                                    );
                                }
                                // 只清守护侧条目：`acp-` 前缀是 GUI 内 ACP 会话自己
                                // 维护的状态，smeltd 重连发快照时不能把它们抹掉。
                                map.retain(|k, _| k.starts_with("acp-"));
                                for s in list {
                                    map.insert(s.id.clone(), s);
                                }
                            }
                            terminal::DaemonStateEvent::Update(s) => {
                                smelt_core::attention::apply_daemon_transition(
                                    &mut attention.lock().unwrap(),
                                    map.get(&s.id).map(|p| p.phase),
                                    &s,
                                    Instant::now(),
                                );
                                map.insert(s.id.clone(), s);
                            }
                        }
                    }
                    cx.refresh_windows(); // 状态点跟着这次变化重绘
                });
            }
        })
        .detach();

        // 远程操作网关：只记「用户上次希望它开着」这个开关；真去问/让守护开的部分
        // 扔进后台任务——涉及连 unix socket、可能要等守护自己起来（最坏几秒），
        // 不能卡首帧渲染。settings.rs 的「远程」设置页读 RemoteRuntimeState 展示。
        //
        // 网关和隧道**串在同一条后台任务**里对齐：先问守护现状（幂等 hydrate），
        // 没有再 start。以前两条 spawn 并行时，隧道可能先回 URL、token 还是空的，
        // UI 会拼出 `?token=` 的死链。
        let remote_config = settings::load_remote_config();
        let want_remote = remote_config.enabled;
        let want_write = remote_config.write_enabled;
        cx.set_global(remote_config);
        cx.set_global(settings::RemoteRuntimeState::default());
        // ACP 会话不需要在退出时做任何事：agent 子进程现在是 smeltd 托管的
        // （见 smelt_core::acp_client），GUI 这边只是个薄客户端，Cmd+Q 直接杀
        // 整个 GUI 进程也不会带走子进程——这正是托管这一层要解决的问题。
        // iroh 隧道同理跑在 smeltd 里，GUI 退出不影响手机端连接。
        if want_remote {
            cx.spawn(async move |cx| {
                let remote_rt = cx
                    .background_executor()
                    .spawn(async move {
                        terminal::ensure_daemon_running();

                        // 本机网关：已在跑就复用 token，否则按配置 start。
                        // iroh 也需要本机网关 token（配对码里带着它）。
                        let existing = terminal::remote_status();
                        let remote_rt = if existing.running
                            && existing.token.as_ref().is_some_and(|t| !t.is_empty())
                        {
                            settings::RemoteRuntimeState { error: None }
                        } else {
                            match terminal::remote_start("127.0.0.1", want_write) {
                                Ok(_) => settings::RemoteRuntimeState { error: None },
                                Err(e) => settings::RemoteRuntimeState { error: Some(e) },
                            }
                        };

                        remote_rt
                    })
                    .await;
                let _ = cx.update(|cx| {
                    cx.set_global(remote_rt);
                    // 网关 token 就绪后再拉 iroh：配对码要把 token 拼进去，早拉会拿到空的
                    settings::spawn_iroh_start_public(cx);
                });
            })
            .detach();
        }
        // 菜单栏常驻图标：点击唤出/前置主窗口，见 status_item.rs。
        status_item::setup(status_tx);

        // 首启主窗口，记入 current_ws（reopen 回调 / URL 投递循环都靠它判断当前主窗口）。
        *current_ws.borrow_mut() = Some(open_workspace_window(cx, window_bg));

        // 消费 Dock / Finder 投递的目录：每个开一个会话（文件取父目录）。常驻到应用退出，
        // 不因主窗口一度被关掉而停——重开窗口后应继续能接文件投递。
        let current_ws_status = current_ws.clone();
        cx.spawn(async move |cx| {
            while let Ok(urls) = url_rx.recv().await {
                let internal_files: Vec<(String, Option<usize>)> = urls
                    .iter()
                    .filter_map(|url| smelt_file_url_to_target(url))
                    .collect();
                let paths: Vec<std::path::PathBuf> =
                    urls.iter().filter_map(|u| file_url_to_path(u)).collect();
                if paths.is_empty() && internal_files.is_empty() {
                    continue;
                }
                let ws = current_ws.borrow().clone();
                if let Some(ws) = ws {
                    if !paths.is_empty() {
                        let _ = ws.update(cx, |ws, cx| ws.open_paths(&paths, cx));
                    }
                    for (path, line) in internal_files {
                        let _ = ws.update_in(cx, |ws, window, cx| {
                            ws.view_file_at(path, line, window, cx)
                        });
                    }
                }
            }
        })
        .detach();

        // 菜单栏图标/下拉菜单事件：主窗口还活着就前置 app（跳会话时顺带切过去），
        // 没了就跟 on_reopen 一样重开一扇（此时会话下标已经没意义，只重开窗口）。
        cx.spawn(async move |cx| {
            while let Ok(event) = status_rx.recv().await {
                let alive = current_ws_status
                    .borrow()
                    .as_ref()
                    .is_some_and(|w| w.upgrade().is_some());
                if alive {
                    let ws = current_ws_status.borrow().clone();
                    if let Some(ws) = ws {
                        let _ = ws.update(cx, |ws, cx| {
                            let target = match &event {
                                status_item::StatusItemEvent::JumpToSession(ix) => {
                                    (*ix < ws.sessions.len()).then_some((*ix, None))
                                }
                                status_item::StatusItemEvent::JumpToDaemonSession(id) => {
                                    ws.sessions.iter().enumerate().find_map(|(ix, session)| {
                                        match &session.kind {
                                            SessionKind::Term { .. } => {
                                                session.term_leaves().into_iter().find_map(|pane| {
                                                    (pane.read(cx).session_id() == id)
                                                        .then_some((ix, Some(pane)))
                                                })
                                            }
                                            SessionKind::Acp(view) => (view.read(cx).session_id()
                                                == id)
                                                .then_some((ix, None)),
                                        }
                                    })
                                }
                                status_item::StatusItemEvent::ActivateMain => None,
                            };
                            if let Some((ix, pane)) = target {
                                ws.active_session = ix;
                                if let Some(pane) = pane {
                                    ws.sessions[ix].set_active_term(pane);
                                }
                                if let Some(group) = ws
                                    .project_groups(cx)
                                    .into_iter()
                                    .find(|group| group.sessions.contains(&ix))
                                {
                                    ws.active_project = Some(group.root);
                                }
                                ws.save_state(cx);
                                cx.notify();
                            }
                        });
                    }
                    status_item::activate_app();
                } else {
                    cx.update(|cx| {
                        let window_bg = cx
                            .try_global::<Appearance>()
                            .map(|a| a.window_bg())
                            .unwrap_or(WindowBackgroundAppearance::Opaque);
                        let ws = open_workspace_window(cx, window_bg);
                        *current_ws_status.borrow_mut() = Some(ws);
                    });
                }
            }
        })
        .detach();
    });
}

#[cfg(test)]
mod project_tests {
    use super::{
        AcpSaved, PaneState, ProjectGroup, SessionState, SplitAxis, disambiguate_labels,
        project_root_of, remove_projects_under, session_state_cwd,
    };

    fn group(root: &str, label: &str) -> ProjectGroup {
        ProjectGroup {
            root: root.into(),
            label: label.into(),
            sessions: Vec::new(),
        }
    }

    /// 跑一遍消歧，取回显示名（base 就是各组当前的 label）。
    fn labels(mut groups: Vec<ProjectGroup>) -> Vec<String> {
        let bases: Vec<String> = groups.iter().map(|g| g.label.clone()).collect();
        disambiguate_labels(&mut groups, &bases);
        groups.into_iter().map(|g| g.label).collect()
    }

    /// 不重名就别乱加前缀（大多数情况该保持干净的目录名）。
    #[test]
    fn unique_labels_are_left_alone() {
        assert_eq!(
            labels(vec![group("/a/smelt", "smelt"), group("/a/other", "other")]),
            vec!["smelt", "other"]
        );
    }

    /// 末段同名的两个项目必须区分得开——否则侧栏并排两个一模一样的「smelt」，
    /// 用户根本分不清哪个是哪个。
    #[test]
    fn duplicate_labels_get_parent_segments() {
        assert_eq!(
            labels(vec![
                group("/x/dev/smelt", "smelt"),
                group("/y/work/smelt", "smelt")
            ]),
            vec!["dev · smelt", "work · smelt"]
        );
    }

    /// 补一段还撞车就继续往上补，直到分开。
    #[test]
    fn keeps_climbing_until_unique() {
        assert_eq!(
            labels(vec![
                group("/a/dev/smelt", "smelt"),
                group("/b/dev/smelt", "smelt")
            ]),
            vec!["a/dev · smelt", "b/dev · smelt"]
        );
    }

    /// worktree 的显示名本来就是「仓库 · 分支」，消歧时整体当末段，前缀补在最前。
    #[test]
    fn worktree_labels_keep_their_branch_suffix() {
        assert_eq!(
            labels(vec![
                group("/x/wt/smelt", "smelt · feat"),
                group("/y/wt/smelt", "smelt · feat"),
            ]),
            vec!["x/wt · smelt · feat", "y/wt · smelt · feat"]
        );
    }

    /// 路径补到顶还重名（真·同路径）→ 必须收敛退出，不能在循环里空转。
    #[test]
    fn gives_up_instead_of_looping_forever() {
        assert_eq!(
            labels(vec![group("/smelt", "smelt"), group("/smelt", "smelt")]),
            vec!["smelt", "smelt"]
        );
    }

    fn leaf(cwd: &str) -> PaneState {
        PaneState::Leaf {
            cwd: Some(cwd.into()),
            id: None,
            custom_title: None,
            launch_label: None,
            launch_cmd: None,
        }
    }

    fn projects(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// cwd 就是项目根、或落在项目根之下，都该归这个项目。
    #[test]
    fn cwd_belongs_to_its_project_root() {
        let p = projects(&["/Users/me/dev/smelt"]);
        assert_eq!(
            project_root_of(&p, "/Users/me/dev/smelt").as_deref(),
            Some("/Users/me/dev/smelt")
        );
        assert_eq!(
            project_root_of(&p, "/Users/me/dev/smelt/crates/smeltd").as_deref(),
            Some("/Users/me/dev/smelt")
        );
        assert_eq!(project_root_of(&p, "/Users/me/dev/other"), None);
        assert_eq!(project_root_of(&p, ""), None);
    }

    /// 前缀必须卡在完整路径段上：`/a/smelt-old` 不是 `/a/smelt` 的子目录，
    /// 否则名字相近的两个项目会互相吞会话。
    #[test]
    fn prefix_must_be_a_whole_path_segment() {
        let p = projects(&["/a/smelt"]);
        assert_eq!(project_root_of(&p, "/a/smelt-old"), None);
        assert_eq!(project_root_of(&p, "/a/smeltd"), None);
        assert_eq!(
            project_root_of(&p, "/a/smelt/sub").as_deref(),
            Some("/a/smelt")
        );
    }

    /// 父子项目都打开着时，会话归最深的那个（不然子项目永远空着）。
    #[test]
    fn deepest_matching_project_wins() {
        let p = projects(&["/a", "/a/b", "/a/b/c"]);
        assert_eq!(project_root_of(&p, "/a/b/c/x").as_deref(), Some("/a/b/c"));
        assert_eq!(project_root_of(&p, "/a/b/x").as_deref(), Some("/a/b"));
        assert_eq!(project_root_of(&p, "/a/x").as_deref(), Some("/a"));
    }

    /// 结尾斜杠是路径写法差异，不该影响归属判定。
    #[test]
    fn trailing_slashes_are_ignored() {
        let p = projects(&["/a/b/"]);
        assert_eq!(project_root_of(&p, "/a/b").as_deref(), Some("/a/b"));
        assert_eq!(project_root_of(&p, "/a/b/").as_deref(), Some("/a/b"));
        assert_eq!(project_root_of(&p, "/a/b/c").as_deref(), Some("/a/b"));
    }

    #[test]
    fn deleting_worktree_removes_project_even_if_session_close_readded_it() {
        let mut p = projects(&[
            "/repo",
            "/repo-worktrees/feature",
            "/repo-worktrees/feature/sub",
        ]);

        remove_projects_under(&mut p, "/repo-worktrees/feature/");

        assert_eq!(p, projects(&["/repo"]));
    }

    /// 旧存档迁移：项目列表从会话 cwd 反推，终端会话取分屏树里第一个叶子的 cwd。
    #[test]
    fn legacy_archive_cwd_comes_from_first_leaf() {
        let ss = SessionState {
            layout: PaneState::Split {
                axis: SplitAxis::H,
                children: vec![leaf("/a/proj"), leaf("/b/other")],
                sizes: Vec::new(),
            },
            active: 0,
            custom_title: None,
            acp: None,
            route: None,
        };
        assert_eq!(session_state_cwd(&ss).as_deref(), Some("/a/proj"));
    }

    /// ACP 会话的 cwd 存在自己的元数据里，layout 只是占位叶子，别取错。
    #[test]
    fn acp_archive_cwd_comes_from_acp_meta() {
        let ss = SessionState {
            layout: leaf("/placeholder"),
            active: 0,
            custom_title: None,
            acp: Some(AcpSaved {
                cwd: Some("/a/acp-proj".into()),
                launch: smelt_core::agent_kind::AcpLaunchSpec::from_command("claude --acp"),
                profile_id: None,
                agent: None,
                history_session_id: None,
                sid: None,
                refresh_launch_from_settings: false,
                fork_origin: None,
            }),
            route: None,
        };
        assert_eq!(session_state_cwd(&ss).as_deref(), Some("/a/acp-proj"));
    }
}

#[cfg(test)]
mod daemon_state_notification_tests {
    use super::{AttentionKind, agent_notification_enabled};

    #[test]
    fn notification_switches_are_independent() {
        let mut config = crate::settings::AgentUiConfig::default();
        config.notify_success = false;

        assert!(!agent_notification_enabled(&config, AttentionKind::Success));
        assert!(agent_notification_enabled(&config, AttentionKind::Approval));
        assert!(agent_notification_enabled(&config, AttentionKind::Input));
        assert!(agent_notification_enabled(&config, AttentionKind::Failure));
    }
}

#[cfg(test)]
mod pane_state_tests {
    use super::PaneState;

    /// pane 自定义名必须能跟着 Leaf 存下来、读回来（否则重开 GUI 就丢名字）。
    #[test]
    fn leaf_custom_title_roundtrips() {
        let leaf = PaneState::Leaf {
            cwd: Some("/tmp/x".into()),
            id: Some("sid-1".into()),
            custom_title: Some("跑测试的终端".into()),
            launch_label: Some("Claude Code".into()),
            launch_cmd: Some("claude --dangerously-skip-permissions".into()),
        };
        let json = serde_json::to_string(&leaf).unwrap();
        let back: PaneState = serde_json::from_str(&json).unwrap();
        match back {
            PaneState::Leaf {
                custom_title,
                launch_label,
                launch_cmd,
                id,
                cwd,
            } => {
                assert_eq!(custom_title.as_deref(), Some("跑测试的终端"));
                assert_eq!(launch_label.as_deref(), Some("Claude Code"));
                assert_eq!(
                    launch_cmd.as_deref(),
                    Some("claude --dangerously-skip-permissions")
                );
                assert_eq!(id.as_deref(), Some("sid-1"));
                assert_eq!(cwd.as_deref(), Some("/tmp/x"));
            }
            _ => panic!("应当反序列化成 Leaf"),
        }
    }

    #[test]
    fn old_archive_without_custom_title_still_loads() {
        let old = r#"{"Leaf":{"cwd":"/tmp/x","id":"sid-1"}}"#;
        let back: PaneState = serde_json::from_str(old).unwrap();
        match back {
            PaneState::Leaf {
                custom_title,
                launch_label,
                launch_cmd,
                id,
                ..
            } => {
                assert!(custom_title.is_none(), "旧存档不该凭空冒出自定义名");
                assert!(launch_label.is_none(), "旧存档不该凭空冒出启动项名");
                assert!(launch_cmd.is_none(), "旧存档不该凭空冒出启动命令");
                assert_eq!(id.as_deref(), Some("sid-1"));
            }
            _ => panic!("应当反序列化成 Leaf"),
        }
    }
}

#[cfg(test)]
mod workspace_state_tests {
    use super::{SidebarGrouping, WsState};
    use smelt_core::workspace_menu::{
        WorkspaceMenuSession, WorkspaceMenuSessionKind, WorkspaceMenuSnapshot,
    };

    #[test]
    fn collapsed_projects_roundtrip() {
        let state = WsState {
            collapsed_projects: vec!["/repo/smelt".into(), "/repo/pulse".into()],
            ..Default::default()
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: WsState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.collapsed_projects, state.collapsed_projects);
    }

    #[test]
    fn shared_workspace_menu_roundtrips() {
        let state = WsState {
            menu: WorkspaceMenuSnapshot::current(
                vec![],
                vec![WorkspaceMenuSession {
                    id: "acp-stable".into(),
                    kind: WorkspaceMenuSessionKind::Acp,
                    title: "会话文本".into(),
                    custom_title: true,
                    cwd: Some("/repo".into()),
                    project_root: Some("/repo".into()),
                    project_title: Some("repo".into()),
                    project_order: 0,
                    session_order: 0,
                    agent: Some("codex".into()),
                }],
            ),
            ..Default::default()
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: WsState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.menu, state.menu);
    }

    #[cfg(test)]
    mod sidebar_group_tests {
        use crate::{AgentStatus, ProjectGroup, SidebarGrouping, sidebar_groups};

        fn group(root: &str, sessions: &[usize]) -> ProjectGroup {
            ProjectGroup {
                root: root.into(),
                label: root.into(),
                sessions: sessions.to_vec(),
            }
        }

        #[test]
        fn status_grouping_order_matches_rendered_status_buckets() {
            let projects = vec![group("a", &[0, 1]), group("b", &[2, 3])];
            let statuses = vec![
                AgentStatus::Idle,
                AgentStatus::Running,
                AgentStatus::WaitingApproval,
                AgentStatus::Done,
            ];

            let groups = sidebar_groups(SidebarGrouping::Status, projects, &statuses, 4);
            let order = groups
                .iter()
                .flat_map(|group| group.sessions.iter().copied())
                .collect::<Vec<_>>();

            assert_eq!(order, vec![2, 1, 3, 0]);
        }

        #[test]
        fn project_grouping_keeps_project_order() {
            let projects = vec![group("a", &[2, 0]), group("b", &[1])];

            let groups = sidebar_groups(
                SidebarGrouping::Project,
                projects,
                &[AgentStatus::Idle; 3],
                3,
            );

            assert_eq!(groups[0].sessions, vec![2, 0]);
            assert_eq!(groups[1].sessions, vec![1]);
        }
    }

    #[cfg(test)]
    mod restore_order_tests {
        use crate::{
            AcpSaved, PaneState, SessionState, merge_restore_orphans, persisted_active_position,
            planned_restore_insert_position, record_restored_index, restore_path_is_cancelled,
            restored_active_position, restored_insert_position, should_auto_resume_active_acp,
            should_restore_saved_active, split_restore_queue,
        };

        fn state(name: &str, acp: bool) -> SessionState {
            SessionState {
                layout: PaneState::Leaf {
                    cwd: Some(format!("/{name}")),
                    id: None,
                    custom_title: None,
                    launch_label: None,
                    launch_cmd: None,
                },
                active: 0,
                custom_title: Some(name.into()),
                acp: acp.then(|| AcpSaved {
                    cwd: Some(format!("/{name}")),
                    launch: smelt_core::agent_kind::AcpLaunchSpec::from_command("claude"),
                    profile_id: None,
                    agent: Some("claude".into()),
                    history_session_id: None,
                    sid: Some(format!("sid-{name}")),
                    refresh_launch_from_settings: false,
                    fork_origin: None,
                }),
                route: None,
            }
        }

        #[test]
        fn split_restore_queue_retains_original_indices() {
            let pending = vec![
                state("term-0", false),
                state("acp-1", true),
                state("term-2", false),
                state("acp-3", true),
            ];

            let (acp, terminals) = split_restore_queue(pending);

            assert_eq!(
                acp.iter().map(|(ix, _)| *ix).collect::<Vec<_>>(),
                vec![1, 3]
            );
            assert_eq!(
                terminals.iter().map(|(ix, _)| *ix).collect::<Vec<_>>(),
                vec![0, 2]
            );
        }

        #[test]
        fn incremental_restore_inserts_sessions_in_saved_order() {
            let mut restored = vec![1, 3];

            let pos = restored_insert_position(&restored, 0);
            restored.insert(pos, 0);
            let pos = restored_insert_position(&restored, 2);
            restored.insert(pos, 2);

            assert_eq!(restored, vec![0, 1, 2, 3]);
            assert_eq!(restored_active_position(&restored, 2), 2);
        }

        #[test]
        fn missing_active_session_falls_back_to_last_restored_session() {
            assert_eq!(restored_active_position(&[0, 1, 3], 2), 2);
        }

        #[test]
        fn restore_orphans_are_merged_at_their_saved_indices() {
            let live = vec![state("acp-1", true), state("acp-3", true)];
            let orphans = vec![(0, state("term-0", false)), (2, state("term-2", false))];

            let merged = merge_restore_orphans(live, &orphans);
            let names = merged
                .iter()
                .map(|session| session.custom_title.as_deref().unwrap())
                .collect::<Vec<_>>();

            assert_eq!(names, vec!["term-0", "acp-1", "term-2", "acp-3"]);
            assert_eq!(persisted_active_position(1, &orphans, true), 3);
        }

        #[test]
        fn active_acp_waits_for_full_restore_before_auto_resuming() {
            assert!(!should_auto_resume_active_acp(false));
            assert!(should_auto_resume_active_acp(true));
        }

        #[test]
        fn user_session_mutation_disables_saved_index_insertion() {
            assert_eq!(planned_restore_insert_position(&[1, 3], 2, 2), Some(1));
            assert_eq!(planned_restore_insert_position(&[1, 3], 2, 0), None);
            assert_eq!(planned_restore_insert_position(&[1], 2, 2), None);

            let mut restored = vec![1, 3];
            record_restored_index(&mut restored, 4, 2, false);
            assert_eq!(restored, vec![1, 3]);
        }

        #[test]
        fn user_selection_prevents_saved_active_session_from_overwriting_it() {
            assert!(should_restore_saved_active(true, 4, 4, 7, 7));
            assert!(!should_restore_saved_active(true, 4, 4, 8, 7));
            assert!(!should_restore_saved_active(false, 4, 4, 7, 7));
        }

        #[test]
        fn deleting_worktree_cancels_nested_pending_restores() {
            assert!(restore_path_is_cancelled(
                Some("/repo-worktrees/feature/sub"),
                &["/repo-worktrees/feature".into()]
            ));
            assert!(!restore_path_is_cancelled(
                Some("/repo-worktrees/feature-old"),
                &["/repo-worktrees/feature".into()]
            ));
        }
    }

    #[test]
    fn old_archive_without_collapsed_projects_still_loads() {
        let restored: WsState = serde_json::from_str(r#"{"projects":["/repo/smelt"]}"#).unwrap();

        assert!(restored.collapsed_projects.is_empty());
        assert_eq!(restored.sidebar_grouping, SidebarGrouping::Project);
    }

    #[test]
    fn sidebar_grouping_roundtrip() {
        let state = WsState {
            sidebar_grouping: SidebarGrouping::Status,
            ..Default::default()
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: WsState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.sidebar_grouping, SidebarGrouping::Status);
    }
}

#[cfg(test)]
mod acp_agent_tests {
    use super::{AcpSaved, acp_agent_from_cmd};
    use crate::settings::AcpAgentKind;

    /// 多 agent 之前的 ACP 存档没有 `agent` 字段：必须读得进来（None），
    /// 不能整条会话解析失败——那等于用户重开 GUI 少一个会话。
    #[test]
    fn old_acp_archive_without_agent_field_still_loads() {
        let old =
            r#"{"cwd":"/tmp/x","cmd":"bunx --bun @agentclientprotocol/claude-agent-acp@0.59.0"}"#;
        let back: AcpSaved = serde_json::from_str(old).unwrap();
        assert!(back.agent.is_none(), "旧存档不该凭空冒出 agent 字段");
        assert_eq!(
            acp_agent_from_cmd(&back.launch.command),
            AcpAgentKind::Claude
        );
    }

    #[test]
    fn acp_saved_round_trip_preserves_profile_and_launch_spec() {
        let saved = AcpSaved {
            cwd: Some("/repo".into()),
            launch: smelt_core::agent_kind::AcpLaunchSpec::from_command("claude")
                .with_env("CLAUDE_CONFIG_DIR", "~/Claude Workspaces/quant"),
            profile_id: Some("quant".into()),
            agent: Some("claude".into()),
            history_session_id: Some(agent_client_protocol::schema::v1::SessionId::new(
                "canonical-history",
            )),
            sid: Some("acp-1".into()),
            refresh_launch_from_settings: false,
            fork_origin: None,
        };

        let value = serde_json::to_value(&saved).unwrap();
        assert!(value.get("cmd").is_none(), "新存档不该再写旧 cmd 字段");
        assert!(
            value.get("entries").is_none(),
            "agent transcript 是历史唯一来源，新存档不该再写 ACP entries"
        );
        assert_eq!(value["history_session_id"], "canonical-history");
        assert!(value.get("resume_session_id").is_none());
        let restored: AcpSaved = serde_json::from_value(value).unwrap();

        assert_eq!(restored.profile_id.as_deref(), Some("quant"));
        assert_eq!(
            restored.history_session_id.as_ref().map(|id| id.0.as_ref()),
            Some("canonical-history")
        );
        assert_eq!(
            restored
                .launch
                .env
                .get("CLAUDE_CONFIG_DIR")
                .map(String::as_str),
            Some("~/Claude Workspaces/quant")
        );
    }

    #[test]
    fn legacy_resume_session_id_migrates_to_history_session_id() {
        let restored: AcpSaved = serde_json::from_value(serde_json::json!({
            "cwd": "/repo",
            "launch": { "command": "claude", "env": {} },
            "agent": "claude",
            "resume_session_id": "legacy-canonical"
        }))
        .unwrap();

        assert_eq!(
            restored.history_session_id.as_ref().map(|id| id.0.as_ref()),
            Some("legacy-canonical")
        );
        let migrated = serde_json::to_value(restored).unwrap();
        assert_eq!(migrated["history_session_id"], "legacy-canonical");
        assert!(migrated.get("resume_session_id").is_none());
    }

    #[test]
    fn legacy_entries_are_readable_but_not_written_back() {
        let legacy: AcpSaved = serde_json::from_value(serde_json::json!({
            "cwd": "/repo",
            "launch": { "command": "claude", "env": {} },
            "agent": "claude",
            "entries": [
                { "User": "old question" },
                { "Assistant": { "text": "old answer", "thought": false } }
            ]
        }))
        .expect("旧 entries 字段不能让整个 ACP 会话存档解析失败");

        let migrated = serde_json::to_value(&legacy).unwrap();
        assert!(
            migrated.get("entries").is_none(),
            "读取旧存档后必须停止写回本地历史副本"
        );
    }

    #[test]
    fn legacy_acp_saved_cmd_deserializes_into_launch_spec() {
        let restored: AcpSaved = serde_json::from_value(serde_json::json!({
            "cwd": "/repo",
            "cmd": "claude --flag",
            "agent": "claude"
        }))
        .unwrap();

        assert_eq!(restored.launch.command, "claude --flag");
        assert!(restored.profile_id.is_none());
    }

    #[test]
    fn legacy_cmd_archive_keeps_saved_launch_on_restart() {
        let restored: AcpSaved = serde_json::from_value(serde_json::json!({
            "cwd": "/repo",
            "cmd": "CLAUDE_CONFIG_DIR=~/Claude Workspaces/quant claude",
            "agent": "claude"
        }))
        .unwrap();

        assert!(
            !restored.refresh_launch_from_settings(),
            "旧 cmd 存档缺少 profile_id 时也不能被当成普通会话刷新成当前默认命令"
        );
    }

    #[test]
    fn structured_launch_without_profile_id_refreshes_from_settings() {
        let restored: AcpSaved = serde_json::from_value(serde_json::json!({
            "cwd": "/repo",
            "launch": { "command": "claude", "env": {} },
            "agent": "claude"
        }))
        .unwrap();

        assert!(
            restored.refresh_launch_from_settings(),
            "新存档的普通会话仍应沿用按当前设置刷新的行为"
        );
    }

    #[test]
    fn legacy_cmd_archive_round_trip_preserves_restart_refresh_behavior() {
        let legacy: AcpSaved = serde_json::from_value(serde_json::json!({
            "cwd": "/repo",
            "cmd": "CLAUDE_CONFIG_DIR=~/Claude Workspaces/quant claude",
            "agent": "claude"
        }))
        .unwrap();

        let value = serde_json::to_value(&legacy).unwrap();
        let restored: AcpSaved = serde_json::from_value(value).unwrap();

        assert!(
            !restored.refresh_launch_from_settings(),
            "旧 cmd 存档升级成新格式后也必须继续保留原 launch，不得在下一次重启时退化成按默认设置刷新"
        );
    }

    /// 旧存档反推：命令里带 copilot / codex 字样的归给对应 agent，其余当 Claude。
    #[test]
    fn agent_inferred_from_legacy_cmd() {
        assert_eq!(acp_agent_from_cmd("copilot --acp"), AcpAgentKind::Copilot);
        assert_eq!(
            acp_agent_from_cmd("bunx --bun @zed-industries/codex-acp"),
            AcpAgentKind::Codex
        );
        assert_eq!(
            acp_agent_from_cmd("bunx --bun @agentclientprotocol/codex-acp"),
            AcpAgentKind::Codex
        );
        assert_eq!(acp_agent_from_cmd("some-other-agent"), AcpAgentKind::Claude);
    }

    /// 存档标识必须往返得回来（改了 id 就等于把用户的会话认成别家 agent）。
    #[test]
    fn agent_id_roundtrips() {
        for a in AcpAgentKind::ALL {
            assert_eq!(AcpAgentKind::from_id(a.id()), Some(a));
        }
        assert_eq!(AcpAgentKind::from_id("gemini"), None);
    }
}
