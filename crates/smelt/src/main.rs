//! smelt 工作台 —— 基于 gpui-component 的桌面窗口。
//!
//! Workspace 管理多个会话（终端或 ACP 对话）：左侧会话列表切换 / 新建 / 关闭，
//! 舞台渲染当前活动会话。每个终端各自独立（PTY、IME、滚动、resize）。
//!
//! 运行： cargo run --bin smelt

// ACP 连接层已经搬进 smelt_core::acp_conn（给 smeltd 未来托管 ACP 会话铺路），
// 这里不再 mod acp;，用的地方直接引 smelt_core::acp_conn。
//
// acp_completion / acp_view / markdown_mermaid / ui_theme / sqlite_state 同理都已
// 搬出主 crate（acp_view.rs 独立成 smelt-acp-view，其余几个是它和主 GUI 共用的
// UI 基建，搬进 smelt-ui / smelt-core，见各自文件头注释）。这里用同名 `use`
// 重新导出成原来的模块路径，全库既有的 `crate::ui_theme::x()` 之类写法不用
// 逐处改——跟 session_history.rs 对 claude_paths 的重导出是同一个套路。
pub(crate) use smelt_acp_view::acp_view;
pub(crate) use smelt_core::sqlite_state;
pub(crate) use smelt_ui::markdown_mermaid;
pub(crate) use smelt_ui::ui_theme;

mod agents;
mod automation_notifications;
mod cli;
mod dock;
mod file_tree;
mod git_log;
mod git_log_view;
mod git_panel;
mod ide;
mod liquid_glass;
mod mem_usage;
mod overlay;
mod plugin_ui;
mod tool_panel;
use smelt_core::osc;
mod panel_transition;
mod provider_quota;
mod resizable_split;
mod session_history;
mod session_list;
mod settings;
mod sidebar_order;
mod stage;
mod status_item;
mod storage_cleanup;
mod terminal;
mod terminal_view;
mod workspace_attention;
mod workspace_frame;
mod workspace_nav;
mod workspace_palette;
mod workspace_persist;
mod workspace_prepare;
mod workspace_sessions;
mod workspace_update;
mod workspace_view;

mod worktree_inherit;

pub(crate) use sidebar_order::{indices_share_group, reorder_vec};
pub(crate) use workspace_nav::{WorkspaceNav, WorkspaceRoute};
pub(crate) use workspace_persist::{
    WorkspaceLoad, WorkspaceSnapshotWriteQueue, load_ws_state, normalize_saved_sessions,
    unproject_acp_when_remote_catalog_drops,
};
#[cfg(test)]
pub(crate) use workspace_persist::{WsState, merge_restore_pending, persisted_active_position};

mod updater;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use chrono::{Local, TimeZone};
use gpui::InteractiveElement;
use gpui::*;
use gpui_component::color_picker::ColorPickerState;
use gpui_component::input::{
    DeleteToBeginningOfLine, DeleteToEndOfLine, DeleteToPreviousWordStart,
};
use gpui_component::list::{List, ListDelegate, ListEvent, ListItem, ListState};
use gpui_component::resizable::{ResizablePanelEvent, ResizableState, resizable_panel};
use gpui_component::slider::SliderState;
use gpui_component::*;
use notify::RecommendedWatcher;
use resizable_split::h_resizable;
use terminal_view::TerminalView;

use file_tree::{DeleteFileTarget, OpenFile, SearchState};
use git_panel::{
    BranchList, DeleteWorktreeTarget, GitDiff, GitStatusData, NewWorktreeState, RepoInfo,
    WorktreeListState,
};
use settings::{
    Appearance, active_launch_entries, default_launch_entries, load_appearance, load_launch_config,
};

const MIN_SIDEBAR_WIDTH: f32 = 240.0;
const MIN_WORKSPACE_CONTENT_WIDTH: f32 = 400.0;
const MIN_TOOL_PANEL_WIDTH: f32 = 320.0;
const DEFAULT_SIDEBAR_WIDTH: f32 = MIN_SIDEBAR_WIDTH;
const DEFAULT_TOOL_PANEL_WIDTH: f32 = MIN_TOOL_PANEL_WIDTH;

/// 左侧导航是用户保存的固定像素宽度，窗口变宽时只能让右侧工作区吸收增量。
/// `ResizableState` 会在容器变化时按比例缩放所有列，导致运行时看到的宽度与
/// SQLite 中的 `sidebar_w` 分叉；冷启动再按存档恢复时就像“没有记忆”。
fn fixed_sidebar_columns(
    sidebar_width: Pixels,
    sidebar_min_width: Pixels,
    sidebar: AnyElement,
    content: AnyElement,
) -> Stateful<Div> {
    div()
        .id("workspace-columns")
        .size_full()
        .flex()
        .child(
            div()
                .h_full()
                .w(sidebar_width)
                .min_w(sidebar_min_width)
                .flex_none()
                .child(sidebar),
        )
        .child(
            div()
                .h_full()
                .w_0()
                .flex_1()
                .min_w(px(MIN_WORKSPACE_CONTENT_WIDTH))
                .min_h_0()
                .flex()
                .child(content),
        )
}

fn sidebar_width_for_viewport(preferred: f32, viewport_width: Pixels) -> f32 {
    let shell_inset = f32::from(ui_theme::shell_padding()) * 2.0;
    let available = (f32::from(viewport_width) - shell_inset - MIN_WORKSPACE_CONTENT_WIDTH)
        .max(MIN_SIDEBAR_WIDTH);
    preferred.max(MIN_SIDEBAR_WIDTH).min(available)
}

#[cfg(target_os = "macos")]
static QUIT_WATCHDOG_ARMED: AtomicBool = AtomicBool::new(false);

/// 请求正常退出；macOS 的 GPUI 实现会把 `terminate:` 投递回主队列。ACP 视图持续重绘
/// 时该队列可能被长期占用，因此仅在正常退出两秒后兜底结束当前 GUI 进程。会话由 smeltd
/// 托管，强制结束 GUI 不会中断后台会话。
pub(crate) fn request_app_quit(cx: &mut App) {
    cx.quit();

    #[cfg(target_os = "macos")]
    if !QUIT_WATCHDOG_ARMED.swap(true, Ordering::Relaxed) {
        thread::spawn(|| {
            thread::sleep(Duration::from_secs(2));
            eprintln!("[workspace] 优雅退出超时，强制结束 GUI 进程");
            std::process::exit(0);
        });
    }
}

/// App 全局持有退出订阅，避免 `app.run` 初始化闭包返回时自动取消它。
struct AppQuitSubscription {
    _subscription: Subscription,
}

impl Global for AppQuitSubscription {}

/// App 全局持有关注事件投递观察器。它不能挂在 Workspace render 生命周期上：
/// macOS 原生全屏窗口切到其它 Space 后会暂停帧回调，正是后台通知延迟到切回来
/// 才出现的根因。
struct AttentionDeliverySubscription {
    _subscription: Subscription,
}

impl Global for AttentionDeliverySubscription {}

struct DaemonStatesSubscription {
    _subscription: Subscription,
}

impl Global for DaemonStatesSubscription {}

/// smeltd 远程会话目录在 GUI 内的只读投影。唯一的 daemon `event_subscribe` 循环写入它；
/// 各个 Workspace 只观察并渲染，不能重新读写历史 JSON 文件或自行建第二条订阅连接。
#[derive(Clone, Default)]
struct RemoteSessionCatalogGlobal {
    snapshot: Arc<Mutex<Option<smelt_core::session_control::RemoteSessionSnapshot>>>,
    generation: u64,
}

impl Global for RemoteSessionCatalogGlobal {}

fn accepts_remote_catalog_incremental(
    current: Option<&smelt_core::session_control::RemoteSessionSnapshot>,
    incoming: &smelt_core::session_control::RemoteSessionSnapshot,
) -> bool {
    // `revision = 0` 表示尚未建立投影水位：不能因首帧也是 0 就把后续完整替换
    // 永久丢掉。
    incoming.revision == 0 || current.is_none_or(|current| incoming.revision > current.revision)
}

fn recognized_remote_acp_sessions(
    records: Vec<smelt_core::session_control::RemoteAcpSession>,
) -> Vec<(
    smelt_core::session_control::RemoteAcpSession,
    settings::ConversationAgentKind,
)> {
    records
        .into_iter()
        .filter_map(|record| {
            settings::ConversationAgentKind::from_id(&record.agent).map(|agent| (record, agent))
        })
        .collect()
}

impl RemoteSessionCatalogGlobal {
    fn reset_from_subscription(
        snapshot: Option<smelt_core::session_control::RemoteSessionSnapshot>,
        cx: &mut App,
    ) {
        let state = cx.global::<Self>().snapshot.clone();
        *state.lock().unwrap() = snapshot;
        cx.update_global::<Self, _>(|global, _| {
            global.generation = global.generation.wrapping_add(1);
        });
    }

    fn apply_incremental(
        snapshot: smelt_core::session_control::RemoteSessionSnapshot,
        cx: &mut App,
    ) {
        let state = cx.global::<Self>().snapshot.clone();
        let changed = {
            let mut state = state.lock().unwrap();
            if !accepts_remote_catalog_incremental(state.as_ref(), &snapshot) {
                false
            } else {
                *state = Some(snapshot);
                true
            }
        };
        if changed {
            cx.update_global::<Self, _>(|global, _| {
                global.generation = global.generation.wrapping_add(1);
            });
        }
    }
}

// Cmd+Q 退出的应用级 action（gpui 无默认菜单栏，需自建菜单栏 + 键位绑定）。
gpui::actions!(
    smelt,
    [
        Quit,
        OpenSettings,
        CheckForUpdate,
        ReportIssue,
        SendSelectionToTerminal,
        PrevSession,
        NextSession,
        ToggleSidebar,
        ToggleToolPanel
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

/// 按本地日历把 unix 秒归入「今天 / 昨天 / 本周 / 更早」。
fn last_updated_bucket(timestamp: u64, now: chrono::DateTime<Local>) -> usize {
    let Some(updated) = Local
        .timestamp_opt(timestamp.min(i64::MAX as u64) as i64, 0)
        .single()
    else {
        return 3;
    };
    match (now.date_naive() - updated.date_naive()).num_days() {
        days if days <= 0 => 0,
        1 => 1,
        2..=6 => 2,
        _ => 3,
    }
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
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

/// 舞台覆盖页：盖在当前会话舞台上，不是一级导航（一级导航是 `WorkspaceRoute`）。
///
/// 存档 JSON 键仍是 `stage_override`；`ToolPanel` 的 wire 值仍是 `inspector`，
/// 避免旧版本读不了新存档。实际内容由 `tool_panel_tab` 决定，其余变体只为旧档兼容。
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum StageCover {
    /// Tool Panel 全屏；持久化沿用旧 wire 名 `inspector`。
    #[serde(rename = "inspector", alias = "tool_panel")]
    ToolPanel,
    // 旧版本把 Tool Panel 的具体内容写进覆盖页。读档时迁成 ToolPanel。
    /// 「文件树 + 内容」双栏全宽（旧档）。
    Files,
    /// 「变更列表 + diff」双栏全宽（旧档）。
    Git,
    /// 「技能」旧档；技能已迁成插件 tab。
    Skills,
    History,
    /// 未知覆盖页。读到即视为没有覆盖页。
    #[serde(other)]
    Unknown,
}

impl StageCover {
    /// 旧版具体舞台页对应的 Tool Panel tab；新的 `ToolPanel` 展示态不携带内容。
    pub(crate) fn legacy_tool_panel_tab(self) -> Option<tool_panel::ToolPanelTab> {
        match self {
            Self::Files => Some(tool_panel::ToolPanelTab::Files),
            Self::Git => Some(tool_panel::ToolPanelTab::Git),
            // 技能已经不是内置 tab；沿用 tab 反序列化那条同样的迁移路径。
            Self::Skills => Some(tool_panel::ToolPanelTab::migrated_skills()),
            Self::History => Some(tool_panel::ToolPanelTab::History),
            Self::ToolPanel | Self::Unknown => None,
        }
    }

    /// 是否是 Tool Panel 的舞台全屏（含旧存档中的具体内容变体）。
    pub(crate) fn is_tool_panel_cover(self) -> bool {
        matches!(
            self,
            Self::ToolPanel | Self::Files | Self::Git | Self::Skills | Self::History
        )
    }
}

/// 守护状态更新很频繁（agent 活跃时每步都推 phase/标题变化）。
/// 这里合并后只 notify 当前 Workspace，让侧栏/状态点重算；不用
/// `refresh_windows()` 把独立的 TerminalView 画布也一起标脏。纯 OSC 标题帧因此
/// 不再触发终端网格布局和文本重塑。
static STATE_REFRESH_SCHEDULED: AtomicBool = AtomicBool::new(false);

fn apply_automation_projection(
    snapshot: smelt_core::automation::AutomationFile,
    origin: automation_notifications::AutomationProjectionOrigin,
    current_ws: &Rc<RefCell<Option<WeakEntity<Workspace>>>>,
    cx: &mut gpui::App,
) {
    let mut config = cx.global::<settings::AgentHostState>().clone();
    let origin = automation_notifications::effective_projection_origin(
        origin,
        &config.automation_store_id,
        &snapshot.store_id,
    );
    let notifications = automation_notifications::automation_run_notifications(
        &config.automation_runs,
        &snapshot.runs,
        origin,
    );
    let app_notify_by_automation = snapshot
        .automations
        .iter()
        .map(|automation| (automation.id.clone(), automation.notifies_app()))
        .collect::<std::collections::HashMap<_, _>>();
    let retained_run_ids = snapshot
        .runs
        .iter()
        .map(|run| run.id.clone())
        .collect::<Vec<_>>();
    if !config.replace_automation_snapshot(snapshot) {
        return;
    }
    let notify_success = config.notify_success;
    let notify_failure = config.notify_failure;
    cx.set_global(config);

    status_item::retain_automation_notifications(&retained_run_ids);
    for notification in notifications {
        let enabled = match notification.kind {
            automation_notifications::AutomationRunNotificationKind::Success => notify_success,
            automation_notifications::AutomationRunNotificationKind::Failure => notify_failure,
        };
        let app_ok = app_notify_by_automation
            .get(&notification.automation_id)
            .copied()
            .unwrap_or(true);
        if enabled && app_ok {
            status_item::deliver_automation_notification(
                &notification.automation_id,
                &notification.run_id,
                &notification.subtitle,
                &notification.body,
            );
        } else {
            status_item::remove_automation_notification(&notification.run_id);
        }
    }

    // 自动化投影不是 ACP 相位那种高频流，不能走 300ms 合并刷新：
    // 保存回包已经 notify 过一次，若这里再延迟 notify，编辑器会隔一拍再闪。
    if let Some(workspace) = current_ws.borrow().clone() {
        let _ = workspace.update(cx, |_, cx| cx.notify());
    }
}

fn schedule_state_refresh(
    current_ws: &Rc<RefCell<Option<WeakEntity<Workspace>>>>,
    cx: &mut gpui::App,
) {
    if STATE_REFRESH_SCHEDULED.swap(true, Ordering::Relaxed) {
        return; // 已有一笔刷新在途，这次更新并入它
    }
    let current_ws = current_ws.clone();
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(std::time::Duration::from_millis(300))
            .await;
        STATE_REFRESH_SCHEDULED.store(false, Ordering::Relaxed);
        cx.update(|cx| {
            if let Some(workspace) = current_ws.borrow().clone() {
                let _ = workspace.update(cx, |_, cx| cx.notify());
            }
        });
    })
    .detach();
}

/// 一个长期存活的右侧路由实例。左侧 session 路由器只整体交换这个对象，不读取其中
/// 任何实现字段；缓存实体（例如 Tool Panel 的编辑器、diff 状态）而不只缓存描述，切回来才能停在原来的
/// 进程、页面和尺寸上。
struct SessionUiState {
    restored_from_archive: bool,
    /// 冷恢复后首次激活 route 时重新打开；运行期打开完成后即清空。
    pending_restore_file: Option<String>,
    stage_cover: Option<StageCover>,
    tool_panel_tab: tool_panel::ToolPanelTab,
    tool_panel_open: bool,
    tool_panel_w: f32,
    expanded: HashSet<String>,
    file_tree_selected: Option<String>,
    open_file: Option<OpenFile>,
    /// 当前打开文件编辑器的变更订阅；输入变化时让 Workspace 重渲染预览和脏标记。
    _file_editor_sub: Option<Subscription>,
    file_tree_w: f32,
    /// Files 内容区右上角的文件树显隐状态。
    file_tree_open: bool,
    pinned_roots: Vec<String>,
    collapsed_roots: HashSet<String>,
    git_tab: GitTab,
    git_diff: Option<GitDiff>,
    /// 打开 diff 的自增序号（独立于 file_gen，避免和文件高亮任务互相取消）。
    /// 异步 diff 回调用它判断结果是否已过期（换文件/切仓库/关视图都会 +1）。
    /// 与 `git_diff` 同属抽屉现场，跟随选中的项目，不跟每条会话。
    diff_gen: u64,
    /// 右侧 Git 文件树点击后，等待聚合 diff 加载完成再定位的文件。
    pending_diff_file: Option<String>,
    /// Diff 审查拖选的起点；用于计算连续选区范围。
    diff_selection_anchor: Option<usize>,
    /// 拖选当前指针所在的可评论行；松开后 `+` 显示在这里，而不是起点。
    diff_selection_cursor: Option<usize>,
    /// 指针从行号栏按下到松开的短暂状态；不能由 anchor 推断，因为松开后锚点仍要显示。
    diff_selection_dragging: bool,
    diff_selected: HashSet<usize>,
    /// 选区和评论器是两步操作：只在点击 `+` 后插入并聚焦评论卡。
    diff_comment_open: bool,
    git_collapsed_diff_files: HashSet<String>,
    /// 该折叠集合的变更序号，diff_derived 的缓存 key 之一（折叠/展开文件头时 +1）。
    git_collapsed_diff_files_gen: u64,
    active_hunk: Option<usize>,
    diff_split: bool,
    /// diff 视图派生数据缓存（gutter 宽 / 内容宽 / 展开行）：diff 内容与折叠状态
    /// 不变时每帧复用，避免大 diff 下每帧 O(n) 重算（掉帧主因之一）。
    diff_derived: Option<git_panel::DiffDerivedCache>,
    git_tree_collapsed: HashSet<String>,
    diff_scope: git_panel::DiffScope,
}

impl Default for SessionUiState {
    fn default() -> Self {
        Self {
            restored_from_archive: false,
            pending_restore_file: None,
            stage_cover: None,
            tool_panel_tab: tool_panel::ToolPanelTab::Files,
            // 新会话把舞台完整留给当前工作；需要工具时再从右上角打开。
            tool_panel_open: false,
            tool_panel_w: DEFAULT_TOOL_PANEL_WIDTH,
            expanded: HashSet::new(),
            file_tree_selected: None,
            open_file: None,
            _file_editor_sub: None,
            file_tree_w: tool_panel::MIN_FILE_TREE_WIDTH,
            file_tree_open: true,
            pinned_roots: Vec::new(),
            collapsed_roots: HashSet::new(),
            git_tab: GitTab::Changes,
            git_diff: None,
            diff_gen: 0,
            pending_diff_file: None,
            diff_selection_anchor: None,
            diff_selection_cursor: None,
            diff_selection_dragging: false,
            diff_selected: HashSet::new(),
            diff_comment_open: false,
            git_collapsed_diff_files: HashSet::new(),
            git_collapsed_diff_files_gen: 0,
            active_hunk: None,
            diff_split: false,
            diff_derived: None,
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
    #[serde(rename = "stage_override")]
    stage_cover: Option<StageCover>,
    #[serde(alias = "inspector_tab")]
    tool_panel_tab: tool_panel::ToolPanelTab,
    #[serde(alias = "inspector_open")]
    tool_panel_open: bool,
    #[serde(alias = "inspector_w")]
    tool_panel_w: f32,
    expanded: HashSet<String>,
    file_tree_selected: Option<String>,
    open_file_path: Option<String>,
    file_tree_w: f32,
    file_tree_open: bool,
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
            stage_cover: None,
            tool_panel_tab: tool_panel::ToolPanelTab::Files,
            // 兼容没有该字段的旧 route 时，也采用当前的新会话默认值。
            tool_panel_open: false,
            tool_panel_w: DEFAULT_TOOL_PANEL_WIDTH,
            expanded: HashSet::new(),
            file_tree_selected: None,
            open_file_path: None,
            file_tree_w: tool_panel::MIN_FILE_TREE_WIDTH,
            file_tree_open: true,
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
        let legacy_tool_panel_tab = self.stage_cover.and_then(StageCover::legacy_tool_panel_tab);
        SessionRouteArchive {
            // 只持久化 Tool Panel 全屏；旧的具体内容变体一律收成 ToolPanel。
            stage_cover: self
                .stage_cover
                .filter(|view| view.is_tool_panel_cover())
                .map(|_| StageCover::ToolPanel),
            tool_panel_tab: legacy_tool_panel_tab.unwrap_or(self.tool_panel_tab),
            tool_panel_open: self.tool_panel_open,
            tool_panel_w: self.tool_panel_w,
            expanded: self.expanded.clone(),
            file_tree_selected: self.file_tree_selected.clone(),
            open_file_path: self
                .open_file
                .as_ref()
                .map(|file| file.path.clone())
                .or_else(|| self.pending_restore_file.clone()),
            file_tree_w: self.file_tree_w,
            file_tree_open: self.file_tree_open,
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
        Self {
            restored_from_archive: true,
            pending_restore_file: archive.open_file_path,
            // 旧版把具体 Tool Panel 内容写进覆盖页；恢复时保留当前内容并
            // 统一迁到 ToolPanel。旧档 `tasks` 读成 Unknown，不占用 session 舞台。
            stage_cover: match archive.stage_cover {
                Some(StageCover::Unknown) | None => None,
                Some(_) => Some(StageCover::ToolPanel),
            },
            tool_panel_tab: archive
                .stage_cover
                .and_then(StageCover::legacy_tool_panel_tab)
                .unwrap_or(archive.tool_panel_tab),
            tool_panel_open: archive.tool_panel_open,
            tool_panel_w: archive.tool_panel_w.max(MIN_TOOL_PANEL_WIDTH),
            expanded: archive.expanded,
            file_tree_selected: archive.file_tree_selected,
            open_file: None,
            _file_editor_sub: None,
            file_tree_w: archive.file_tree_w.clamp(
                tool_panel::MIN_FILE_TREE_WIDTH,
                tool_panel::MAX_FILE_TREE_WIDTH,
            ),
            file_tree_open: archive.file_tree_open,
            pinned_roots: archive.pinned_roots,
            collapsed_roots: archive.collapsed_roots,
            git_tab: archive.git_tab,
            git_diff: None,
            diff_gen: 0,
            pending_diff_file: None,
            diff_selection_anchor: None,
            diff_selection_cursor: None,
            diff_selection_dragging: false,
            diff_selected: HashSet::new(),
            diff_comment_open: false,
            git_collapsed_diff_files: HashSet::new(),
            git_collapsed_diff_files_gen: 0,
            active_hunk: None,
            diff_split: archive.diff_split,
            diff_derived: None,
            git_tree_collapsed: archive.git_tree_collapsed,
            diff_scope: archive.diff_scope,
        }
    }
}

fn swap_session_ui_state(current: &mut SessionUiState, parked: &mut SessionUiState) {
    std::mem::swap(current, parked);
    current.tool_panel_w = current.tool_panel_w.max(MIN_TOOL_PANEL_WIDTH);
    parked.tool_panel_w = parked.tool_panel_w.max(MIN_TOOL_PANEL_WIDTH);
    current.file_tree_w = current.file_tree_w.clamp(
        tool_panel::MIN_FILE_TREE_WIDTH,
        tool_panel::MAX_FILE_TREE_WIDTH,
    );
    parked.file_tree_w = parked.file_tree_w.clamp(
        tool_panel::MIN_FILE_TREE_WIDTH,
        tool_panel::MAX_FILE_TREE_WIDTH,
    );
}

/// 右侧抽屉现场：Tool Panel / 文件树 / Git。跟选中的项目走，不跟每条会话。
struct ProjectUiState {
    pending_restore_file: Option<String>,
    stage_cover: Option<StageCover>,
    tool_panel_tab: tool_panel::ToolPanelTab,
    tool_panel_open: bool,
    tool_panel_w: f32,
    expanded: HashSet<String>,
    file_tree_selected: Option<String>,
    open_file: Option<OpenFile>,
    _file_editor_sub: Option<Subscription>,
    file_tree_w: f32,
    file_tree_open: bool,
    pinned_roots: Vec<String>,
    collapsed_roots: HashSet<String>,
    git_tab: GitTab,
    git_diff: Option<GitDiff>,
    diff_gen: u64,
    pending_diff_file: Option<String>,
    diff_selection_anchor: Option<usize>,
    diff_selection_cursor: Option<usize>,
    diff_selection_dragging: bool,
    diff_selected: HashSet<usize>,
    diff_comment_open: bool,
    git_collapsed_diff_files: HashSet<String>,
    git_collapsed_diff_files_gen: u64,
    active_hunk: Option<usize>,
    diff_split: bool,
    diff_derived: Option<git_panel::DiffDerivedCache>,
    git_tree_collapsed: HashSet<String>,
    diff_scope: git_panel::DiffScope,
}

impl Default for ProjectUiState {
    fn default() -> Self {
        SessionUiState::default().take_project_ui()
    }
}

impl SessionUiState {
    fn take_project_ui(&mut self) -> ProjectUiState {
        let defaults = SessionUiState::default();
        ProjectUiState {
            pending_restore_file: std::mem::take(&mut self.pending_restore_file),
            stage_cover: self.stage_cover.take(),
            tool_panel_tab: std::mem::replace(&mut self.tool_panel_tab, defaults.tool_panel_tab),
            tool_panel_open: std::mem::replace(&mut self.tool_panel_open, defaults.tool_panel_open),
            tool_panel_w: std::mem::replace(&mut self.tool_panel_w, defaults.tool_panel_w),
            expanded: std::mem::take(&mut self.expanded),
            file_tree_selected: self.file_tree_selected.take(),
            open_file: self.open_file.take(),
            _file_editor_sub: self._file_editor_sub.take(),
            file_tree_w: std::mem::replace(&mut self.file_tree_w, defaults.file_tree_w),
            file_tree_open: std::mem::replace(&mut self.file_tree_open, defaults.file_tree_open),
            pinned_roots: std::mem::take(&mut self.pinned_roots),
            collapsed_roots: std::mem::take(&mut self.collapsed_roots),
            git_tab: std::mem::replace(&mut self.git_tab, defaults.git_tab),
            git_diff: self.git_diff.take(),
            diff_gen: std::mem::take(&mut self.diff_gen),
            pending_diff_file: self.pending_diff_file.take(),
            diff_selection_anchor: self.diff_selection_anchor.take(),
            diff_selection_cursor: self.diff_selection_cursor.take(),
            diff_selection_dragging: std::mem::take(&mut self.diff_selection_dragging),
            diff_selected: std::mem::take(&mut self.diff_selected),
            diff_comment_open: std::mem::take(&mut self.diff_comment_open),
            git_collapsed_diff_files: std::mem::take(&mut self.git_collapsed_diff_files),
            git_collapsed_diff_files_gen: std::mem::take(&mut self.git_collapsed_diff_files_gen),
            active_hunk: self.active_hunk.take(),
            diff_split: std::mem::replace(&mut self.diff_split, defaults.diff_split),
            diff_derived: self.diff_derived.take(),
            git_tree_collapsed: std::mem::take(&mut self.git_tree_collapsed),
            diff_scope: std::mem::replace(&mut self.diff_scope, defaults.diff_scope),
        }
    }

    fn apply_project_ui(&mut self, ui: ProjectUiState) {
        self.pending_restore_file = ui.pending_restore_file;
        self.stage_cover = ui.stage_cover;
        self.tool_panel_tab = ui.tool_panel_tab;
        self.tool_panel_open = ui.tool_panel_open;
        self.tool_panel_w = ui.tool_panel_w.max(MIN_TOOL_PANEL_WIDTH);
        self.expanded = ui.expanded;
        self.file_tree_selected = ui.file_tree_selected;
        self.open_file = ui.open_file;
        self._file_editor_sub = ui._file_editor_sub;
        self.file_tree_w = ui.file_tree_w.clamp(
            tool_panel::MIN_FILE_TREE_WIDTH,
            tool_panel::MAX_FILE_TREE_WIDTH,
        );
        self.file_tree_open = ui.file_tree_open;
        self.pinned_roots = ui.pinned_roots;
        self.collapsed_roots = ui.collapsed_roots;
        self.git_tab = ui.git_tab;
        self.git_diff = ui.git_diff;
        self.diff_gen = ui.diff_gen;
        self.pending_diff_file = ui.pending_diff_file;
        self.diff_selection_anchor = ui.diff_selection_anchor;
        self.diff_selection_cursor = ui.diff_selection_cursor;
        self.diff_selection_dragging = ui.diff_selection_dragging;
        self.diff_selected = ui.diff_selected;
        self.diff_comment_open = ui.diff_comment_open;
        self.git_collapsed_diff_files = ui.git_collapsed_diff_files;
        self.git_collapsed_diff_files_gen = ui.git_collapsed_diff_files_gen;
        self.active_hunk = ui.active_hunk;
        self.diff_split = ui.diff_split;
        self.diff_derived = ui.diff_derived;
        self.git_tree_collapsed = ui.git_tree_collapsed;
        self.diff_scope = ui.diff_scope;
    }
}

impl ProjectUiState {
    /// 把可落盘的抽屉字段写回会话存档。打开的编辑器和 diff 缓存仍只停在热场。
    fn copy_persistable_into(&self, dest: &mut SessionUiState) {
        dest.pending_restore_file = self
            .open_file
            .as_ref()
            .map(|file| file.path.clone())
            .or_else(|| self.pending_restore_file.clone());
        dest.stage_cover = self.stage_cover;
        dest.tool_panel_tab = self.tool_panel_tab;
        dest.tool_panel_open = self.tool_panel_open;
        dest.tool_panel_w = self.tool_panel_w.max(MIN_TOOL_PANEL_WIDTH);
        dest.expanded = self.expanded.clone();
        dest.file_tree_selected = self.file_tree_selected.clone();
        dest.file_tree_w = self.file_tree_w.clamp(
            tool_panel::MIN_FILE_TREE_WIDTH,
            tool_panel::MAX_FILE_TREE_WIDTH,
        );
        dest.file_tree_open = self.file_tree_open;
        dest.pinned_roots = self.pinned_roots.clone();
        dest.collapsed_roots = self.collapsed_roots.clone();
        dest.git_tab = self.git_tab;
        dest.diff_split = self.diff_split;
        dest.git_tree_collapsed = self.git_tree_collapsed.clone();
        dest.diff_scope = self.diff_scope;
    }
}

/// 把当前热场抽屉按项目根停走。返回刚停放的 key，供写回被换出的会话存档。
fn park_project_ui(
    active: &mut SessionUiState,
    project_ui: &mut HashMap<String, ProjectUiState>,
    active_key: &mut Option<String>,
) -> Option<String> {
    let key = active_key.take()?;
    project_ui.insert(key.clone(), active.take_project_ui());
    Some(key)
}

/// 把目标项目已停放的抽屉盖回热场；没有停放记录时保留刚换上来的会话抽屉。
fn apply_project_ui_for_root(
    active: &mut SessionUiState,
    project_ui: &mut HashMap<String, ProjectUiState>,
    active_key: &mut Option<String>,
    new_key: Option<String>,
) {
    if let Some(key) = new_key.as_ref()
        && let Some(ui) = project_ui.remove(key)
    {
        active.apply_project_ui(ui);
    }
    *active_key = new_key;
}

/// 切会话时抽屉的正确顺序：先按项目根停走当前抽屉，再交换会话现场，再盖回目标项目。
/// 先换会话再停放会把新会话里那份过期抽屉当成该项目的现场。
#[cfg(test)]
fn resync_session_with_project_ui(
    active: &mut SessionUiState,
    old_session: &mut SessionUiState,
    new_session: &mut SessionUiState,
    project_ui: &mut HashMap<String, ProjectUiState>,
    active_key: &mut Option<String>,
    new_key: Option<String>,
) {
    let parked_key = park_project_ui(active, project_ui, active_key);
    swap_session_ui_state(active, old_session);
    if let Some(key) = parked_key.as_ref()
        && let Some(ui) = project_ui.get(key)
    {
        ui.copy_persistable_into(old_session);
    }
    swap_session_ui_state(active, new_session);
    apply_project_ui_for_root(active, project_ui, active_key, new_key);
}

/// Git 页内部的子页。对标 JetBrains 的 Git 工具窗口——「提交」和「日志」是同一个
/// 窗口里的两个视图，不占两个顶层标签。
#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
enum GitTab {
    /// 提交历史 + 分支图。
    Log,
    /// 工作区改动：文件树 + diff + 暂存 / 提交。也是旧版已删除 tab 的迁移目标。
    #[serde(other)]
    #[default]
    Changes,
}

// 会话里 agent 的状态（总览页状态徽章 / 侧栏状态点）：搬进 smelt-core（跟
// ui_theme 共用同一份判断，见 agent_status.rs），这里重导出成原来的裸名字，
// 全库既有的 `AgentStatus::x` 写法不用逐处改。
pub(crate) use smelt_core::agent_status::AgentStatus;

// DaemonStates（守护上报的会话状态镜像）/ AttentionGlobal（统一关注事件 store）：
// ACP 视图独立成 smelt-acp-view 后要跨
// crate 读写，搬进 smelt-ui（daemon_states_global.rs）共享，这里重导出成原来
// 的裸名字。
pub(crate) use smelt_ui::daemon_states_global::{
    AttentionGlobal, AttentionItem, AttentionKind, DaemonStates,
};
pub(crate) use smelt_ui::history_titles_global::HistoryTitles;

/// 按稳定 session id 读取守护状态镜像；终端 pane 和 ACP 会话必须共用这一个入口，
/// 否则状态栏会出现 attention 已完成、ACP 本地快照仍显示运行中的双事实源竞态。
fn daemon_state_for_session_id(session_id: &str, cx: &App) -> Option<terminal::DaemonSessionState> {
    DaemonStates::get(session_id, cx)
}

/// 取某个终端 pane 对应的守护状态；没有全局单例（比如极早期尚未注册）或那个
/// session id 还没有数据都返回 None。
fn daemon_state_for(view: &Entity<TerminalView>, cx: &App) -> Option<terminal::DaemonSessionState> {
    daemon_state_for_session_id(view.read(cx).session_id(), cx)
}

fn agent_notification_enabled(config: &settings::AgentHostState, kind: AttentionKind) -> bool {
    match kind {
        AttentionKind::Approval => config.notify_approval,
        AttentionKind::Input => config.notify_input,
        AttentionKind::Success => config.notify_success,
        AttentionKind::Failure => config.notify_failure,
        AttentionKind::Bell => config.notify_terminal_bell,
        AttentionKind::Notice => true,
    }
}

fn daemon_attention_suppressed(state: &terminal::DaemonSessionState, cx: &App) -> bool {
    cx.try_global::<settings::AgentHostState>()
        .is_some_and(|config| {
            automation_notifications::suppress_runtime_terminal_attention(
                &config.automation_runs,
                &state.id,
                state.effective_phase(),
            )
        })
}

fn apply_daemon_attention(
    previous: Option<&terminal::DaemonSessionState>,
    state: &terminal::DaemonSessionState,
    now: Instant,
    cx: &mut App,
) {
    if daemon_attention_suppressed(state, cx) {
        // AutomationRun is the notification authority for automation results. The hidden ACP runtime
        // disappears immediately after completion and cannot be a stable notification target.
        AttentionGlobal::remove_session(&state.id, cx);
        return;
    }
    AttentionGlobal::apply_daemon_transition(previous, state, now, cx);
}

fn apply_daemon_attention_baseline(state: &terminal::DaemonSessionState, cx: &mut App) {
    if daemon_attention_suppressed(state, cx) {
        AttentionGlobal::remove_session(&state.id, cx);
        return;
    }
    AttentionGlobal::apply_daemon_baseline(state, cx);
}

/// 新关注事件入队后的应用级协调出口。这里一次性完成角标、菜单和通知投递；
/// `Workspace::render` 不参与消费队列，因此原生全屏窗口在其它 Space 暂停绘制时，
/// hook/OSC/ACP 事件仍会立即变成系统通知。
fn dispatch_pending_attention(
    current_ws: &Rc<RefCell<Option<WeakEntity<Workspace>>>>,
    cx: &mut App,
) {
    let workspace = current_ws.borrow().clone();
    let app_active = status_item::is_app_active();
    let Some(store) = cx
        .try_global::<AttentionGlobal>()
        .map(|global| global.0.clone())
    else {
        return;
    };
    // The batch leaves AttentionStore here, but system-channel payloads are synchronously copied
    // into the native bridge's retry queue before any asynchronous UserNotifications call starts.
    let (badge_count, batch, unread_session_ids) = {
        let mut store = store.lock().unwrap();
        let unread_session_ids = store
            .unread_items()
            .into_iter()
            .map(|item| item.session_id)
            .collect::<Vec<_>>();
        (
            store.badge_count(),
            store.drain_deliveries(),
            unread_session_ids,
        )
    };
    dock::set_badge(badge_count);
    status_item::set_attention_count(badge_count);
    status_item::retain_system_notifications(&unread_session_ids);

    // `NSApplication.isActive` 只回答“Smelt 是否在前台”；应用内可能还有设置窗口。
    // 只有主工作区窗口本身也拿到焦点，逻辑上的当前 pane 才算用户真的正在看。
    // 未查看的事件一律走系统通知；后台分流仍只依赖 app_active，避免原生全屏窗口
    // 留着 key-window 状态时被误判成正在看。
    let workspace_window_active = app_active
        && workspace
            .as_ref()
            .and_then(|workspace| {
                workspace
                    .update_in(cx, |_, window, _| window.is_window_active())
                    .ok()
            })
            .unwrap_or(false);
    let contexts = workspace
        .as_ref()
        .and_then(|workspace| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.sync_notification_surfaces(cx);
                    if workspace_window_active {
                        workspace.mark_visible_session_read(cx);
                    }
                    batch
                        .iter()
                        .map(|notification| workspace.attention_context(notification, cx))
                        .collect::<Vec<_>>()
                })
                .ok()
        })
        .unwrap_or_else(|| vec![(false, None); batch.len()]);
    let notify_config = cx
        .try_global::<settings::AgentHostState>()
        .cloned()
        .unwrap_or_default();
    for (notification, (is_current_view, session_title)) in batch.into_iter().zip(contexts) {
        let display_title = session_title
            .map(|session| format!("{} · {session}", notification.title))
            .unwrap_or_else(|| notification.title.clone());
        let enabled = agent_notification_enabled(&notify_config, notification.kind);
        match smelt_core::attention::delivery_channel(
            enabled,
            app_active,
            workspace_window_active,
            is_current_view,
        ) {
            smelt_core::attention::DeliveryChannel::Suppress => {
                status_item::remove_system_notification(&notification.session_id);
            }
            smelt_core::attention::DeliveryChannel::System => {
                status_item::deliver_notification(
                    &notification.session_id,
                    &display_title,
                    &notification.message,
                );
            }
        }
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

/// 一个会话的内容形态。Term 是第一通道（PTY 分屏树），Acp 是第二通道（结构化
/// 消息流，见 docs/archive/project-report.md 第 5 节）——后者不参与分屏，一会话一视图。
enum SessionKind {
    /// 终端会话 = 一棵独立分屏树 + 会话内当前活动 pane（终端）。
    Term {
        layout: Pane,
        active: Entity<TerminalView>,
    },
    /// 结构化对话会话：单视图，不参与分屏（ACP / Pi RPC 等运行时都走这里）。
    Conversation(Entity<acp_view::AcpView>),
}

/// 侧栏每条对应一个会话；主区显示当前会话的内容（分屏树或 ACP 消息流）。
struct Session {
    /// 只用于把运行时 UI 快照稳定地绑到 session；拖拽排序和活动 pane 变化都不改它。
    ui_id: u64,
    kind: SessionKind,
    /// 最近一次会话内容或运行状态变化的 unix 秒时间戳。
    last_updated_at: u64,
    /// 用户手动改过的会话名（侧栏右键「重命名」）；None = 用下面 title() 的自动推导。
    custom_title: Option<String>,
    /// 创建这段对话时选择的产品级智能体。智能体本身不是会话；同一个定义可以被
    /// 任意多段会话引用。None 表示用户直接选择了底层 provider。
    agent_definition_id: Option<String>,
    /// 由智能体触发器创建的 Run。有值时这不是用户手动开的对话。
    automation_id: Option<String>,
    /// 由移动端创建、最初挂在 smeltd 远程目录上的会话。终端 PTY 仍以远程目录为准
    /// （进程没了就该拆投影）。ACP 对话一旦出现在侧栏，就由工作区快照记住，
    /// 不能再把远程目录当成唯一账本——daemon 冷启动会清掉没有 live runtime 的条目。
    remote_owned: bool,
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
            last_updated_at: unix_now_secs(),
            custom_title: None,
            agent_definition_id: None,
            automation_id: None,
            remote_owned: false,
            _acp_persist_sub: None,
            ui_state: SessionUiState::default(),
        }
    }

    /// 会话身份锚点：侧栏选中态、拖拽、activate 等都拿它做「是同一个会话吗」比较。
    /// Term = 活动终端的 entity id。
    fn anchor_id(&self) -> EntityId {
        match &self.kind {
            SessionKind::Term { active, .. } => active.entity_id(),
            SessionKind::Conversation(view) => view.entity_id(),
        }
    }

    /// 终端会话的活动 pane；ACP 会话返回 None（调用方借此天然跳过终端专属操作）。
    fn active_term(&self) -> Option<&Entity<TerminalView>> {
        match &self.kind {
            SessionKind::Term { active, .. } => Some(active),
            SessionKind::Conversation(_) => None,
        }
    }

    /// ACP 会话的视图；终端会话返回 None（跟 `active_term` 反过来，供侧栏右键
    /// 的「强制重启」这类 ACP 专属操作用）。
    fn active_acp(&self) -> Option<&Entity<acp_view::AcpView>> {
        match &self.kind {
            SessionKind::Term { .. } => None,
            SessionKind::Conversation(view) => Some(view),
        }
    }

    /// 产品级智能体对话 / 自动化 Run 属于工作台对话区，不进项目会话列表。
    fn is_product_conversation(&self, cx: &App) -> bool {
        let acp_session_id = self
            .active_acp()
            .map(|view| view.read(cx).session_id().to_string());
        smelt_core::session_control::is_product_conversation(
            self.automation_id.as_deref(),
            self.agent_definition_id.as_deref(),
            acp_session_id.as_deref(),
            self.cwd(cx).as_deref(),
        )
    }

    fn is_agent_conversation(&self, cx: &App) -> bool {
        smelt_core::session_control::is_agent_conversation(
            self.automation_id.as_deref(),
            self.agent_definition_id.as_deref(),
            self.cwd(cx).as_deref(),
        )
    }

    /// 切换终端会话的活动 pane；非终端会话是 no-op。
    fn set_active_term(&mut self, view: Entity<TerminalView>) {
        match &mut self.kind {
            SessionKind::Term { active, .. } => *active = view,
            SessionKind::Conversation(_) => {}
        }
    }

    /// 终端状态通道的更新时间比 GUI 创建时间更准确；ACP 使用自身的持久化时间。
    fn effective_updated_at(&self, cx: &App) -> u64 {
        let daemon_updated_at = self
            .term_leaves()
            .iter()
            .filter_map(|view| daemon_state_for(view, cx))
            .map(|state| state.updated_at)
            .max()
            .unwrap_or_default();
        self.last_updated_at.max(daemon_updated_at)
    }

    /// 终端会话的分屏树；ACP 会话没有。
    fn term_layout(&self) -> Option<&Pane> {
        match &self.kind {
            SessionKind::Term { layout, .. } => Some(layout),
            SessionKind::Conversation(_) => None,
        }
    }

    fn term_layout_mut(&mut self) -> Option<&mut Pane> {
        match &mut self.kind {
            SessionKind::Term { layout, .. } => Some(layout),
            SessionKind::Conversation(_) => None,
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

    /// ACP 会话的 agent 身份；终端会话返回 None。侧栏据此给出标签与图标，
    /// 避免经 `provider_kind` 的终端映射丢掉只有 ACP 的那几家。
    pub(crate) fn acp_kind(&self, cx: &App) -> Option<settings::ConversationAgentKind> {
        match &self.kind {
            SessionKind::Conversation(view) => Some(view.read(cx).agent_kind()),
            SessionKind::Term { .. } => None,
        }
    }

    /// 产品级智能体展示身份。它来自插件 Agent contribution，与下面的 ACP
    /// provider 身份分开；普通 ACP/终端会话返回 None。
    pub(crate) fn plugin_agent_presentation(
        &self,
        cx: &App,
    ) -> Option<plugin_ui::PluginAgentPresentation> {
        let SessionKind::Conversation(view) = &self.kind else {
            return None;
        };
        view.read(cx)
            .agent_session()
            .as_ref()
            .and_then(plugin_ui::agent_presentation)
    }

    /// 状态栏菜单项的 logo 标识。ACP 身份优先：一个只能 ACP 跑的 agent（dsh）
    /// 在终端表里没有变体，`provider_kind` 对它只能给 None，用它选图标就会把
    /// dsh 显示成通用终端方块。两张表的 id 是同一套（有不变量测试守着），所以
    /// 同一家 agent 无论哪条路径跑，拿到的都是同一枚 logo。
    pub(crate) fn agent_icon_id(&self, cx: &App) -> Option<&'static str> {
        self.acp_kind(cx)
            .map(|kind| kind.id())
            .or_else(|| self.provider_kind(cx).map(|kind| kind.id()))
    }

    /// 侧栏行的 agent 身份：ACP 直接取协议会话的 agent；终端取当前活动 pane
    /// 的有效 provider。明确的快捷启动身份优先；裸终端里手动启动 agent 时，
    /// 回退到 daemon 从结构化 hook 检测到的 provider。
    pub(crate) fn provider_kind(&self, cx: &App) -> Option<settings::TerminalAgentKind> {
        match &self.kind {
            SessionKind::Term { active, .. } => pane_provider_kind(active, cx),
            // 只有 ACP 的 agent（dsh）在终端表里没有对应项。这里返回 None 而不是
            // 硬凑一个，侧栏的标签与图标由 `acp_kind()` 单独给出。
            SessionKind::Conversation(view) => view.read(cx).agent_kind().terminal(),
        }
    }

    /// 会话标题：用户重命名最高优先；单 pane 终端继续使用该 pane 的自定义名，
    /// 多 pane 时会话名与各 pane 名保持分离。ACP 走协议标题/首条消息兜底。
    fn title(&self, cx: &App) -> String {
        match &self.kind {
            SessionKind::Term { active, .. } => {
                let pane_custom = active.read(cx).custom_title().map(str::to_string);
                let conversation_custom = pane_conversation_custom_title(active, cx);
                terminal_session_display_title(
                    conversation_custom.as_deref(),
                    self.custom_title.as_deref(),
                    self.pane_count(),
                    pane_custom.as_deref(),
                    pane_auto_title(active, cx),
                )
            }
            SessionKind::Conversation(view) => self.custom_title.clone().unwrap_or_else(|| {
                let view = view.read(cx);
                let title = view.auto_title();
                let cwd = view.cwd();
                let fallback = self
                    .agent_definition_id
                    .as_deref()
                    .and_then(|id| {
                        cx.try_global::<settings::AgentHostState>()?
                            .agents
                            .iter()
                            .find(|definition| definition.id == id)
                    })
                    .map(|definition| definition.name.trim())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| view.agent_kind().short_label());
                if self.agent_definition_id.is_some() {
                    title
                        .as_deref()
                        .map(str::trim)
                        .filter(|title| !title.is_empty())
                        .unwrap_or(fallback)
                        .to_string()
                } else {
                    smelt_core::session_title::acp_display_title(
                        title.as_deref(),
                        fallback,
                        cwd.as_deref(),
                    )
                }
            }),
        }
    }

    /// 会话工作目录：活动终端的 cwd（侧栏分组用）。
    fn cwd(&self, cx: &App) -> Option<String> {
        match &self.kind {
            SessionKind::Term { active, .. } => active.read(cx).cwd(),
            SessionKind::Conversation(view) => view.read(cx).cwd(),
        }
    }

    /// 会话内 pane 数（判断 Cmd+W 是关 pane 还是关整会话）。
    fn pane_count(&self) -> usize {
        match &self.kind {
            SessionKind::Term { .. } => self.term_leaves().len(),
            SessionKind::Conversation(_) => 1,
        }
    }

    /// 会话状态：两类会话先把各自事实源适配成共享三态，再统一按
    /// 要你 > 运行中 > 空闲聚合。
    fn status(&self, cx: &App) -> AgentStatus {
        match &self.kind {
            // ACP 协议视图与守护镜像可能先后到达；两边都先给出完整 AgentStatus，
            // 再走同一优先级，避免派发或失败的同步窗口短暂变灰。
            SessionKind::Conversation(view) => {
                let session_id = view.read(cx).session_id().to_string();
                let daemon = daemon_state_for_session_id(&session_id, cx)
                    .and_then(|state| AgentStatus::from_daemon_state(&state))
                    .unwrap_or(AgentStatus::Idle);
                let v = view.read(cx);
                AgentStatus::highest([daemon, v.agent_status()])
            }
            // 每个 pane 只读自己的结构化状态；父会话再做来源无关的聚合。
            SessionKind::Term { .. } => {
                let leaves = self.term_leaves();
                AgentStatus::highest(leaves.iter().map(|view| pane_status(view, cx)))
            }
        }
    }
}

fn next_session_ui_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// 重命名弹窗改的是谁：侧栏会话行改整个会话的名，分屏子行只改那一个 pane 的名。
#[derive(Clone)]
enum RenameTarget {
    Session(usize),
    Pane(Entity<TerminalView>),
    WorkspaceSurface(String),
    History {
        agent: settings::HistorySourceKind,
        profile_id: Option<String>,
        cwd: String,
        resume_id: String,
        current_title: String,
    },
}

/// 单个终端 pane 自动推导的标题：优先 daemon/OSC 任务名，其次快捷启动显示名，
/// 再回退建终端时的 cwd 名。不看用户改的名字——`Session::title` 靠它拿活动 pane
/// 的「客观」标题。
fn pane_auto_title(view: &Entity<TerminalView>, cx: &App) -> String {
    let t = view.read(cx);
    let daemon_title = daemon_state_for(view, cx).and_then(|state| state.title);
    let agent_title = daemon_title.or_else(|| t.agent_title());
    smelt_core::session_title::display_title(
        None,
        agent_title.as_deref(),
        t.launch_label(),
        Some(t.title()),
    )
}

/// 侧栏会话行显示名。
///
/// 「当前对话的用户命名」排在最前：终端里的 agent 换一段对话，这一行就该跟着
/// 换名字，而不是继续顶着上一段对话的标题。会话级/pane 级的 pin 只服务于没有
/// 对话身份的终端（裸 shell、hook 不带 id 的 provider）。
fn terminal_session_display_title(
    conversation_custom: Option<&str>,
    session_custom: Option<&str>,
    pane_count: usize,
    pane_custom: Option<&str>,
    auto_title: String,
) -> String {
    nonempty_title(conversation_custom)
        .or_else(|| nonempty_title(session_custom))
        .or_else(|| {
            (pane_count == 1)
                .then(|| nonempty_title(pane_custom))
                .flatten()
        })
        .map(str::to_string)
        .unwrap_or(auto_title)
}

fn nonempty_title(title: Option<&str>) -> Option<&str> {
    title.map(str::trim).filter(|title| !title.is_empty())
}

fn resolve_terminal_provider(
    launch_provider: Option<settings::TerminalAgentKind>,
    detected_provider: Option<&str>,
) -> Option<settings::TerminalAgentKind> {
    launch_provider.or_else(|| detected_provider.and_then(settings::TerminalAgentKind::from_id))
}

fn pane_provider_kind(
    view: &Entity<TerminalView>,
    cx: &App,
) -> Option<settings::TerminalAgentKind> {
    let launch_provider = view.read(cx).launch_kind().agent_kind();
    let detected_provider = daemon_state_for(view, cx).and_then(|state| state.provider);
    resolve_terminal_provider(launch_provider, detected_provider.as_deref())
}

/// 终端 pane 此刻挂着的那段 provider 对话身份：`(历史命名空间, 对话 id)`。
///
/// 只认 hook 上报的 provider——对话 id 就是它发来的，命名空间必须同源，否则
/// 用户改了个名会写到另一家 agent 的历史存档上。因此这里**不**回退到启动命令
/// 猜出来的 provider。
///
/// 终端会话不承载 workspace profile，历史命名空间固定是 `profile = None`。
/// 这个终端 pane 当前在跟哪家 agent 的哪段对话。
///
/// 返回的是**历史来源身份**而不是 ACP 种类：标题覆盖层按 `id()` 存取，跟对方
/// 有没有 ACP 无关。之前这里用 `.acp()?` 把纯终端 agent（Antigravity）静默
/// 丢掉，结果是给它的会话改名没有任何反应。
fn pane_conversation_identity(
    view: &Entity<TerminalView>,
    cx: &App,
) -> Option<(settings::HistorySourceKind, String)> {
    let state = daemon_state_for(view, cx)?;
    let conversation_id = state
        .conversation_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())?;
    let terminal = settings::TerminalAgentKind::from_id(state.provider.as_deref()?)?;
    Some((terminal.into(), conversation_id))
}

/// 用户给这段对话起过的名字（历史页 / 侧栏改名写的是同一份覆盖层）。
fn pane_conversation_custom_title(view: &Entity<TerminalView>, cx: &App) -> Option<String> {
    let (agent, conversation_id) = pane_conversation_identity(view, cx)?;
    HistoryTitles::get(agent.id(), None, &conversation_id, cx)
}

/// 侧栏分屏子行显示的 pane 标题：用户改过名就用改的，否则走自动推导。
///
/// 跟 `pane_auto_title` 分开是有意的：`Session::title` 拿的是活动 pane 的自动标题，
/// 若这里的自定义名漏进去，给活动 pane 改名会连带改掉侧栏父行（会话名），切换
/// 活动 pane 后父行又跳回来——会话名和 pane 名得各归各的。
fn pane_title(view: &Entity<TerminalView>, cx: &App) -> String {
    pane_conversation_custom_title(view, cx)
        .or_else(|| view.read(cx).custom_title().map(str::to_string))
        .unwrap_or_else(|| pane_auto_title(view, cx))
}

/// 单个终端 pane 的状态：逻辑同 Session::status，但只看这一个 pane 自己
/// （Session::status 是取会话内所有 pane 的最高态）。
fn pane_status(view: &Entity<TerminalView>, cx: &App) -> AgentStatus {
    let daemon_state = daemon_state_for(view, cx);
    smelt_core::agent_status::terminal_agent_status(daemon_state.as_ref())
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

/// 会话侧栏的组织方式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SidebarGrouping {
    None,
    Status,
    LastUpdated,
    #[default]
    Project,
}

/// 侧栏正在拖哪一类。身份跟 drop payload 一致：会话用 ui_id，项目用 root。
#[derive(Clone, Debug, PartialEq, Eq)]
enum SidebarDrag {
    Session(u64),
    Project(String),
}

/// 单个会话的持久化镜像：分屏树 + 会话内活动叶子（遍历序）+ 用户重命名过的会话名。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SessionState {
    layout: PaneState,
    active: usize,
    /// 最近一次会话内容变化的 unix 秒时间戳；旧存档缺失时按当前时间迁移。
    #[serde(default = "unix_now_secs")]
    last_updated_at: u64,
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
/// 保存重新 load 所需的身份、启动信息和非敏感 ACP 配置。`entries` 仅用于读取旧版
/// workspace.json，新存档不再写出，避免和 agent transcript 形成两个数据源。
#[derive(Clone, serde::Serialize)]
struct AcpSaved {
    cwd: Option<String>,
    launch: smelt_core::agent_kind::ConversationLaunchSpec,
    #[serde(default)]
    profile_id: Option<String>,
    /// agent 种类标识（`ConversationAgentKind::id()`）。旧存档没有这个字段 → None，恢复时
    /// 按 launch 里的命令反推，反推不出就当 Claude（多 agent 之前只可能是它）。
    #[serde(default)]
    agent: Option<String>,
    /// 产品级智能体定义引用；只描述“这段对话由谁创建”，不改变底层 provider。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_definition_id: Option<String>,
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
    /// 通用交互输入路由。agent/profile 只决定 ACP 执行器，不决定用户消息送往哪里。
    #[serde(default)]
    conversation_binding: smelt_core::conversation::ConversationBinding,
    /// 产品级智能体/控制器/实例身份。与上面的 ACP provider 和输入路由各自独立。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_session: Option<smelt_plugin_api::AgentSessionBinding>,
    /// 当前 ACP 会话最后生效的 provider 配置（模型、推理、快速模式、权限等）。
    /// 不含 custom_env/token 等凭据；旧存档缺失时沿用 provider 默认值。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    config_values: Vec<(String, String)>,
    /// ACP 握手完成前尚未发送的首包。旧存档没有该字段时按 None 迁移。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_prompt: Option<String>,
    /// 待发首包对应的外部投递 id；普通对话为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_delivery_id: Option<String>,
    /// 尚未装饰首条交互输入的本地智能体预设。与 `pending_prompt` 不同，它不会
    /// 在 ACP Idle 时自行发送，由 daemon 在第一次 `acp_submit_input` 成功后消费。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_agent_preset: Option<String>,
    /// 由智能体触发器创建的 Run。旧存档没有该字段时按普通对话迁移。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    automation_id: Option<String>,
    /// 上次生效的会话标题（agent 上报的，或由首条用户消息推导的）。冷启动恢复
    /// 出来的视图没有消息，缺了它侧栏只能退回「`<agent>` 对话 · `<目录名>`」，
    /// 同一智能体的多段对话就全长一个样。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_title: Option<String>,
}

#[derive(serde::Deserialize)]
struct AcpSavedWire {
    cwd: Option<String>,
    #[serde(default)]
    launch: Option<smelt_core::agent_kind::ConversationLaunchSpec>,
    #[serde(default)]
    cmd: Option<String>,
    #[serde(default)]
    profile_id: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    agent_definition_id: Option<String>,
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
    #[serde(default)]
    conversation_binding: Option<smelt_core::conversation::ConversationBinding>,
    #[serde(default)]
    agent_session: Option<smelt_plugin_api::AgentSessionBinding>,
    #[serde(default)]
    config_values: Vec<(String, String)>,
    #[serde(default)]
    pending_prompt: Option<String>,
    #[serde(default)]
    pending_delivery_id: Option<String>,
    #[serde(default)]
    pending_agent_preset: Option<String>,
    #[serde(default)]
    automation_id: Option<String>,
    #[serde(default)]
    session_title: Option<String>,
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
            smelt_core::agent_kind::ConversationLaunchSpec::from_command(
                wire.cmd.unwrap_or_default(),
            )
        });
        let conversation_binding = wire.conversation_binding.unwrap_or_default();
        Ok(Self {
            cwd: wire.cwd,
            launch,
            profile_id: wire.profile_id,
            agent: wire.agent,
            agent_definition_id: wire.agent_definition_id,
            history_session_id: wire.history_session_id,
            sid: wire.sid,
            refresh_launch_from_settings,
            fork_origin: wire.fork_origin,
            conversation_binding,
            agent_session: wire.agent_session,
            config_values: wire.config_values,
            pending_prompt: wire.pending_prompt,
            pending_delivery_id: wire.pending_delivery_id,
            pending_agent_preset: wire.pending_agent_preset,
            automation_id: wire.automation_id,
            session_title: wire.session_title,
        })
    }
}

impl AcpSaved {
    fn refresh_launch_from_settings(&self) -> bool {
        self.refresh_launch_from_settings
    }
}

/// 0.8.0 开发版曾把每个智能体写成一条单例运行。新模型中它只用于读取并迁移旧
/// `agent_runs`；写盘会统一落成可多开的普通对话 Session。
#[derive(Clone, serde::Deserialize)]
pub(crate) struct LegacyAgentRunState {
    pub(crate) definition_id: String,
    pub(crate) acp: AcpSaved,
}

/// 可序列化的分屏布局镜像：叶子存该终端 cwd + 守护会话 id，Split 存方向 + 子节点 +
/// 各子块尺寸。结构 / 嵌套 / 方向 / 拖出来的比例都完整恢复。
/// id 用于重开 GUI 时 reattach smeltd 里还活着的会话（旧存档无 id → 开新会话）。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) enum PaneState {
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
    last_updated_at: &[u64],
    session_count: usize,
) -> Vec<ProjectGroup> {
    match grouping {
        SidebarGrouping::Project => project_groups,
        SidebarGrouping::Status => [
            (AgentStatus::NeedsYou, "需要你"),
            (AgentStatus::Running, "运行中"),
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
        SidebarGrouping::LastUpdated => {
            let now = Local::now();
            let mut buckets: [Vec<usize>; 4] = std::array::from_fn(|_| Vec::new());
            for ix in 0..session_count {
                let bucket =
                    last_updated_bucket(last_updated_at.get(ix).copied().unwrap_or_default(), now);
                buckets[bucket].push(ix);
            }
            let labels = ["今天", "昨天", "本周", "更早"];
            buckets
                .into_iter()
                .enumerate()
                .filter_map(|(bucket, mut sessions)| {
                    if sessions.is_empty() {
                        return None;
                    }
                    sessions.sort_by(|a, b| {
                        last_updated_at
                            .get(*b)
                            .copied()
                            .unwrap_or_default()
                            .cmp(&last_updated_at.get(*a).copied().unwrap_or_default())
                            .then_with(|| a.cmp(b))
                    });
                    Some(ProjectGroup {
                        root: format!("__last_updated_{bucket}"),
                        label: labels[bucket].to_string(),
                        sessions,
                    })
                })
                .collect()
        }
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

type IndexedSessionState = (usize, SessionState);

fn split_indexed_restore_queue(
    pending: Vec<IndexedSessionState>,
) -> (Vec<IndexedSessionState>, Vec<IndexedSessionState>) {
    pending
        .into_iter()
        .partition(|(_, session)| session.acp.is_some())
}

/// 冷启动恢复失败后还要不要再试。握手超时（EAGAIN）时侧栏是空的，
/// 只等下次冷启动等于让用户以为对话被抹掉了。
pub(crate) const RESTORE_RETRY_LIMIT: u32 = 3;

pub(crate) fn should_retry_failed_restore(failed: usize, completed_attempts: u32) -> bool {
    failed > 0 && completed_attempts < RESTORE_RETRY_LIMIT
}

pub(crate) fn restore_retry_delay() -> Duration {
    Duration::from_secs(2)
}

/// 这类错误说明守护这轮答不了 Open。剩下的会话再各自空等 26s 只会把侧栏拖空。
pub(crate) fn restore_error_blocks_remaining(error: &str) -> bool {
    error.contains("smeltd 未就绪")
        || error.contains("Resource temporarily unavailable")
        || error.contains("os error 35")
        || error.contains("timed out")
        || error.contains("WouldBlock")
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

fn pane_state_leaf_ids(pane: &PaneState) -> Vec<String> {
    match pane {
        PaneState::Leaf { id, .. } => id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(|id| vec![id.to_string()])
            .unwrap_or_default(),
        PaneState::Split { children, .. } => {
            children.iter().flat_map(pane_state_leaf_ids).collect()
        }
    }
}

/// 存档里一段会话的稳定身份：ACP 用 `sid`，终端用活动叶子的 smeltd id。
/// 活动项必须按这个认人，不能按数组下标——ACP 先恢复、终端后到时下标会对到别人。
pub(crate) fn session_state_persist_id(state: &SessionState) -> Option<String> {
    if let Some(sid) = state
        .acp
        .as_ref()
        .and_then(|acp| acp.sid.as_deref())
        .filter(|sid| !sid.is_empty())
    {
        return Some(sid.to_string());
    }
    let ids = pane_state_leaf_ids(&state.layout);
    ids.get(state.active)
        .cloned()
        .or_else(|| ids.into_iter().next())
}

pub(crate) fn live_session_persist_id(session: &Session, cx: &App) -> Option<String> {
    let id = match &session.kind {
        SessionKind::Conversation(view) => view.read(cx).session_id().to_string(),
        SessionKind::Term { active, .. } => active.read(cx).session_id().to_string(),
    };
    (!id.is_empty()).then_some(id)
}

/// 恢复完成后的活动会话：先按稳定 id 认人，认不到再退回存档下标映射。
/// 恢复期间用户已经点过会话时，不要用存档盖掉。
pub(crate) fn resolve_restored_active_session(
    live_ids: &[Option<String>],
    saved_id: Option<&str>,
    saved_index: usize,
    restored_original_indices: &[usize],
    restore_order_intact: bool,
    user_changed_selection: bool,
    current_index: usize,
) -> usize {
    if live_ids.is_empty() {
        return 0;
    }
    let clamp = |index: usize| index.min(live_ids.len() - 1);
    if user_changed_selection {
        return clamp(current_index);
    }
    if let Some(id) = saved_id.filter(|id| !id.is_empty())
        && let Some(ix) = live_ids.iter().position(|live| live.as_deref() == Some(id))
    {
        return ix;
    }
    if restore_order_intact {
        return clamp(restored_active_position(
            restored_original_indices,
            saved_index,
        ));
    }
    clamp(current_index)
}

fn should_auto_resume_active_acp(sessions_restored: bool) -> bool {
    sessions_restored
}

/// 冷恢复后是否该把工作台路由对齐到活动的智能体对话。
///
/// 只做一次。智能体面的子页（目录 / 编辑器 / 对话）不落盘，冷启动后恢复出来的
/// 是根目录页，这里补位把它带回用户上次看的对话。但这只是「冷启动补位」，不是
/// 持续同步：每帧都做会把用户点侧栏「智能体」返回目录的动作在下一帧顶回对话页，
/// 表现就是点了完全没反应。
/// 恢复出来的路由不在工作台上（比如任务页、插件页）就别抢，用户看的是别处。
fn should_open_restored_agent_conversation(
    sessions_restored: bool,
    already_aligned: bool,
    active_is_agent_conversation: bool,
    route: &WorkspaceRoute,
) -> bool {
    sessions_restored
        && !already_aligned
        && active_is_agent_conversation
        && matches!(route, WorkspaceRoute::Agents)
}

/// 智能体对话占着项目舞台是不一致状态，必须纠正。
///
/// 项目舞台画的是 `sessions[active_session]`，而智能体对话不属于项目会话列表；
/// 让它落在这条路由上会把工作台对话渲染进项目舞台。这条是不变量，和上面的一次性
/// 补位不同，得一直守着。
fn agent_conversation_occupies_project_stage(
    route: &WorkspaceRoute,
    active_is_agent_conversation: bool,
) -> bool {
    active_is_agent_conversation && matches!(route, WorkspaceRoute::Session)
}

fn route_views_session(route: &WorkspaceRoute, agent_conversation_sid: Option<&str>) -> bool {
    route.is_session()
        || (matches!(route, WorkspaceRoute::Agents) && agent_conversation_sid.is_some())
}

fn should_defer_notification_session_jump(sessions_restored: bool, target_found: bool) -> bool {
    !sessions_restored && !target_found
}

/// 恢复完成前 `active_session` 仍是存档下标，ACP 先插入时会对到别人。
/// 不能在窗口期按这个下标把舞台拽去「对话」。
fn should_correct_agent_conversation_on_project_stage(
    sessions_restored: bool,
    route: &WorkspaceRoute,
    active_is_agent_conversation: bool,
) -> bool {
    sessions_restored
        && agent_conversation_occupies_project_stage(route, active_is_agent_conversation)
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
pub(crate) enum SplitAxis {
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
                terminal::Terminal::spawn(24, 80, cwd.as_deref(), &sid, launch_cmd.as_deref())
                    .map_err(|e| {
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

/// 工作台根视图：多标签终端管理器。
struct Workspace {
    /// 所有会话；每个会话 = 一棵独立分屏树 + 会话内活动 pane。
    sessions: Vec<Session>,
    /// 当前活动会话的**运行时下标**（主区显示它、侧栏高亮它）。
    /// 跨进程身份在 `saved_active_session_id` / 各会话 sid，不能靠这个下标落盘认人。
    active_session: usize,
    /// 冷启动要恢复的活动会话稳定 id。恢复完成前 `active_session` 还不是活列表下标。
    saved_active_session_id: Option<String>,
    /// 一级 Tab：智能体 / 自动化 / 任务 / 会话 / 插件工作台。
    nav: WorkspaceNav,
    /// 智能体 / 自动化各自持有编辑器和瞬时错误；Workspace 只组合，不理解控件。
    agent_surface: agents::AgentsSurface,
    automation_surface: agents::AutomationsSurface,
    /// 当前 Workspace 热字段属于哪个 session。每帧渲染前与 active session 对齐，
    /// 因而所有切换入口（点击、快捷键、任务跳转、恢复）都走同一套快照交换。
    ui_session_id: Option<u64>,
    /// 当前会话的 UI 状态。Workspace 只切换这一整个对象，不理解其中有哪些页面或控件。
    active_session_ui: SessionUiState,
    /// 各项目的抽屉现场。切会话时覆盖回当前项目，避免每个对话各带一份文件树/Git。
    project_ui: HashMap<String, ProjectUiState>,
    /// `project_ui` 里正在热场的那个项目根。
    active_project_ui_key: Option<String>,
    /// Tool Panel 的挂载与开合过渡。关闭动画结束后才卸载面板。
    tool_panel_transition: panel_transition::PanelTransition,
    /// 鼠标/键盘事件后的一次性下一帧插件面板显隐同步闸门。
    plugin_surface_sync_scheduled: bool,

    /// 用户给工作台起的名字；缺省用 contribution 声明的 title。
    workspace_surface_titles: HashMap<String, String>,
    /// 右键 ContextMenu 没有进入 gpui-component 的全局 popup 注册表，
    /// 因而由 Workspace 自己持有一个有界的临时遮挡 lease。
    context_menu_suppressed: bool,
    context_menu_generation: u64,
    /// 文件树里已展开的文件夹绝对路径。
    /// 目录列表缓存（绝对路径 → 已排序过滤的直接子项 (名, 是否目录)）。后台读盘填充，
    /// render 只读；此前 file_tree 在 render 里同步 fs::read_dir，大目录会像 git
    /// status 那样掉帧，这里改用同款「后台刷新 + 缓存 + render 只读」模式修复。
    dir_cache: file_tree::DirCache,
    /// 正在后台读取的目录（防重复并发 spawn）。
    dir_inflight: HashSet<String>,
    /// 文件树键盘选中的条目绝对路径（↑↓ 导航用）。
    /// 打开文件后要 reveal 的路径：祖先目录缓存齐了再 scroll_to_item。
    file_tree_pending_reveal: Option<String>,
    /// 当前在文件树里打开查看的文件（含预高亮的行数据）。
    /// ACP 消息图片的窗口级预览。放在 Workspace 而非 AcpView，遮罩才能覆盖 Session
    /// Panel、Tool Panel 和输入区，且不受会话面板裁剪。解码/降采样走
    /// `smelt_ui::image` 的内容寻址缓存：渲染时查 `cached()` 取当前这张图的结果，
    /// 不用再单独持有解码后的 RenderImage 字段。
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
    /// Stage | Tool Panel 的内层拖拽状态——嵌在固定像素侧栏右边的工作区里。
    stage_tool_panel_resize: Entity<ResizableState>,
    /// 左侧会话栏是否展开，以及对应的挂载过渡。
    sidebar_open: bool,
    sidebar_transition: panel_transition::PanelTransition,
    sidebar_w: f32,
    /// 左侧栏拖拽起点：(鼠标 x, 开始时固定像素宽度)。
    sidebar_drag_start: Option<(f32, f32)>,
    /// 交互式 diff：选中待评论的行号集合（对应 GitDiff.lines 下标），换文件/重开 diff 时清空。
    /// 交互式 diff 的评论输入框（懒创建，随 Git 视图渲染出待发送的 diff 时创建）。
    diff_comment_input: Option<Entity<gpui_component::input::TextareaState>>,
    /// Git 视图的 commit message 输入框，按**仓库根**分开。
    ///
    /// 多仓工作区里提交信息天然属于某一个仓库：共用一个框会把写给子仓的
    /// 描述带到父仓去。懒建，仓库从发现结果里消失时跟着回收。
    commit_msg_inputs: HashMap<String, Entity<gpui_component::input::TextareaState>>,
    /// 提交/推送失败的原因，按**仓库根**分开，直接显示在该仓库的提交框下方。
    ///
    /// 提交失败必须在用户正盯着的地方说清楚：它是对一次明确点击的直接回应。
    /// 走 `background_error` 只会弹一条系统通知，通知被静音或错过时，界面上
    /// 「文件还在暂存区、什么都没发生」和成功毫无区别——pre-commit hook 因
    /// PATH 缺失而失败正是这样被当成「按钮没反应」。
    /// 下次对同一仓库发起提交/推送时清掉，不留过期错误。
    commit_errors: HashMap<String, String>,
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
    /// 侧栏「对话」里被折叠起来的智能体（存定义 id）。
    collapsed_agents: HashSet<String>,
    /// 侧栏被固定的项目根。固定后即使开了「隐藏无会话」也留在列表里。
    pinned_projects: HashSet<String>,
    /// 会话列表分组方式（默认按项目）。
    sidebar_grouping: SidebarGrouping,
    /// 按项目分组时是否把无会话的项目从侧栏藏起来（固定的 / 当前项目除外）。
    sidebar_hide_empty_projects: bool,
    /// 会话拖拽悬停中的插入位置：(目标会话 ui_id, 插它前面?)。
    sess_drop_hint: Option<(u64, bool)>,
    /// 项目拖拽悬停中的插入位置：(目标项目 root, 插它前面?)。
    proj_drop_hint: Option<(String, bool)>,
    /// 当前正在拖的侧栏项。用来压暗原位、以及只在「我们自己的拖」时改抓握光标。
    sidebar_drag: Option<SidebarDrag>,
    /// 侧栏会话列表的滚动位置；拖到顶底边缘时靠它自动滚。
    sidebar_scroll: ScrollHandle,
    sidebar_auto_scroll: gpui_component::scroll::AutoScroll,
    /// 已发现 IDE 的按需缓存。没有项目目录时完全不触发；首次打开 IDE 菜单才后台
    /// 填充，之后菜单只读这里的快照。
    ide_catalog: ide::IdeCatalog,
    /// 首次扫描尚未完成时打开的 IDE 菜单。弱引用不会把已经关闭的弹层留在内存里；
    /// 完成后统一原地替换其加载项。
    ide_popup_waiters: Vec<stage::IdePopupWaiter>,
    /// 命令面板（Cmd+K）；None 表示未打开。搜索/导航/确认由 ListState 负责。
    palette: Option<Entity<ListState<CmdDelegate>>>,
    /// 命令面板的事件订阅（确认/取消）；随面板关闭一并释放。
    _palette_sub: Option<Subscription>,
    /// 各滚动区的常驻滚动句柄——供 gpui-component Scrollbar 读取位置并绘制。
    /// 必须常驻（每帧新建会丢失滚动位置）。
    diff_scroll: VirtualListScrollHandle,
    /// Diff 代码正文专用的横向滚动位置。纵向虚拟列表不能复用它：行号栏和
    /// 行内评论器是审查轨道，必须始终钉在视口左侧。
    diff_code_scroll: ScrollHandle,
    /// 变更列表的滚动位置。列表里混着仓库行、提交框和文件行，高度不一，
    /// 用不了等高虚拟列表，所以是常规滚动容器。
    git_list_scroll: ScrollHandle,
    /// 文件树列表的滚动句柄（普通滚动，非虚拟滚动——见 file_tree 函数注释）。
    file_tree_scroll: ScrollHandle,
    /// Files / Git 右侧树列正在拖拽时的起点：(鼠标 x，树列宽)。树列不能使用
    /// `ResizableState`：那个组件会在父容器变宽/变窄时保持百分比，违反“只在拖
    /// 自己分隔条时改宽”的约定。这个瞬态状态只驱动固定像素宽度的分隔条。
    file_tree_drag_start: Option<(f32, f32)>,
    /// 文件树顶部的过滤输入框；首次渲染文件树时懒创建（需要 window）。
    file_filter: Option<Entity<gpui_component::input::InputState>>,
    /// 过滤框的变更订阅（键入即重渲染）；随视图存活。
    _file_filter_sub: Option<Subscription>,
    /// 文件树搜索结果（文件名 + 文件内容）：后台遍历项目填充，render 只读。
    /// query 非空时左栏由树形切换为扁平命中列表。
    search_results: Option<SearchState>,
    /// 搜索任务自增序号：后台遍历完成时用它丢弃过期结果（期间又改了查询）。
    search_gen: u64,
    _stage_tool_panel_resize_sub: Subscription,
    /// 启动项列表编辑器（设置页「会话与 Agent」分组懒创建）。
    launch_inputs: Option<settings::LaunchInputs>,
    /// 手动添加 workspace 列表编辑器（设置页「Agent 集成」分组懒创建）。
    profile_inputs: Option<settings::ProfileInputs>,
    /// 原生 DSH 凭据写入表单；值只驻留内存，保存后立即清空。
    /// 原生 DSH 插件管理面板；未展开时为 None。
    dsh_plugin_manager: Option<settings::DshPluginManager>,
    /// 原生 DSH DeepSeek 模型设置表单；API key 仅在输入控件内存中保留至保存或取消。
    dsh_model_editor: Option<settings::DshModelEditor>,
    /// 原生 DSH 自定义 Provider 表单；与官方 DeepSeek 配置分开呈现。
    dsh_custom_provider_editor: Option<settings::DshCustomProviderEditor>,
    /// 原生 DSH Provider 摘要缓存；仅在打开设置或显式修改后刷新。
    dsh_native_model_settings: Option<Result<settings::NativeDshModelSettings, String>>,
    /// 交互式原生 DSH 模型表单无法打开或保存时的持久可见错误。
    dsh_model_editor_error: Option<String>,
    /// 原生 Pi 模型设置表单。
    pi_model_editor: Option<settings::PiModelEditor>,
    /// 原生 Pi 自定义 Provider 表单。
    pi_custom_provider_editor: Option<settings::PiCustomProviderEditor>,
    /// Pi 内置 provider 的凭据现状缓存；打开凭据面板时才异步填。
    pi_auth_providers: Option<settings::PiAuthProvidersState>,
    /// 正在进行的订阅/API key 登录；持有登录子进程。
    pi_login: Option<settings::PiLoginView>,
    /// 正在为哪个 provider 挑默认模型。
    pi_auth_model_picker: Option<settings::PiAuthModelPicker>,
    /// Pi 凭据操作（列表、注销）的最近一次错误。
    pi_auth_error: Option<String>,
    /// 凭据面板是否展开到全部内置 provider（默认只列可订阅登录的和已配好的）。
    pi_auth_show_all: bool,
    /// 原生 Pi 模型设置缓存。渲染只读缓存，避免每帧读磁盘；刷新时清空重读。
    ///
    /// 懒加载而不是「打开设置页时主动拉一次」：设置页有好几个入口（齿轮、智能体
    /// 页的配置按钮、命令面板），任何一个忘了拉都会让用户对着一屏没读过盘的界面。
    pi_model_settings:
        std::cell::OnceCell<Result<smelt_core::pi_model_settings::PiModelSettings, String>>,
    /// 交互式原生 Pi 模型表单错误。
    pi_model_editor_error: Option<String>,
    /// Pi 插件目录扫描缓存。渲染只读缓存，避免每帧读磁盘；刷新时整体清空重扫。
    pi_plugins: std::cell::OnceCell<Vec<smelt_core::pi_plugin_catalog::PiPlugin>>,
    /// 正在等二次确认删除的插件 id。
    pi_plugin_pending_delete: Option<String>,
    /// 插件导入 / 删除的最近一次错误。
    pi_plugin_error: Option<String>,
    /// 每条路由实际支持的推理强度。`None` 表示还没问过。
    ///
    /// 问一次要起一个完整 dsh 运行时（数秒），所以是异步填的，界面在填好前不画
    /// 强度选择器——宁可晚几秒出现，也不先摆一排可能发一次失败一次的档位。
    dsh_model_capabilities: Option<settings::NativeDshCapabilityState>,
    /// 设置面板的有状态组件（懒创建）：不透明度滑块 + 界面字号滑块 + 终端字号滑块 + 背景色取色器
    /// + 背景图透明度滑块。
    opacity_slider: Option<Entity<SliderState>>,
    ui_font_size_slider: Option<Entity<SliderState>>,
    font_size_slider: Option<Entity<SliderState>>,
    bg_image_opacity_slider: Option<Entity<SliderState>>,
    bg_color_picker: Option<Entity<ColorPickerState>>,
    /// 上面组件的变更订阅。
    settings_subs: Vec<Subscription>,
    /// 上次应用到窗口的原生背景外观；外观改动时在 render 里同步。
    applied_window_bg: Option<WindowBackgroundAppearance>,
    /// 上次应用到原生 NSWindow 的整体透明度，避免每帧重复发送 AppKit 消息。
    applied_window_opacity: Option<f32>,
    /// 上次安装/更新的液态玻璃材质档位；配置变化时 render 里重新 setStyle。
    applied_glass_style: Option<liquid_glass::GlassStyle>,
    /// 上次应用到窗口的界面基准字号（rem_size）。
    applied_ui_font_px: Option<u32>,
    /// git status 缓存（root → (取得时刻, 数据)）。Git 页后台刷新，render 只读，
    /// 避免每帧同步跑 git status（大仓要 ~90ms，是掉帧元凶）。
    ///
    /// key 是**仓库**根而不是项目根：一个项目里可能有多个仓库（submodule、
    /// vendor 里的独立仓库、被 .gitignore 忽略的仓库），每个都独立跑自己的 status。
    git_status: HashMap<String, (Instant, GitStatusData)>,
    /// 项目根 → 该项目里发现的仓库列表（含发现是否被上限截断）。
    /// 发现本身要跑 git，所以跟 status 一样走缓存 + 后台刷新。
    git_repos: HashMap<String, (Instant, git_panel::RepoSet)>,
    /// 正在后台做仓库发现的项目根（防重复并发 spawn）。
    git_repos_inflight: HashSet<String>,
    /// 当前提交操作的目标仓库（仓库根）。多仓工作区里，提交框、推送、分支菜单
    /// 都作用于它；None = 跟随项目根。
    ///
    /// 没有它的话写操作只能落回项目根，于是子仓的改动能暂存却永远提交不出去。
    active_git_repo: Option<String>,
    /// 变更栏里被折起来的仓库根。多仓工作区里不折叠就是一面墙的文件。
    git_repo_collapsed: HashSet<String>,
    /// root → status 失效代数。请求只可提交与当前代数一致的结果；操作过程中迟到的
    /// 旧回包会被丢弃并自动补拉，不能把新索引状态覆盖回去。
    git_status_generation: HashMap<String, u64>,
    /// 正在后台刷新 status 的 root（防重复并发 spawn）。
    git_status_inflight: HashSet<String>,
    /// root → 连续读取失败次数。失败数据不能冒充“工作区干净”；前三次按短退避
    /// 自动重试，持续失败则等文件事件或后续界面交互再尝试。
    git_status_failures: HashMap<String, u8>,
    /// 单文件 git add/reset 的即时 UI 状态。操作完成后继续保留到一次操作后发起的
    /// 权威 status 回包，避免复选框先回弹再延迟移动分组。
    git_index_pending: HashMap<(String, String), git_panel::PendingGitIndexOp>,
    /// 分支列表缓存（root → (取得时刻, 数据)），Git 页头部分支切换下拉用；同
    /// git_status 一套只在 Git 页打开时后台刷新。
    branches: HashMap<String, (Instant, BranchList)>,
    /// 正在后台刷新分支列表的 root（防重复并发 spawn）。
    branches_inflight: HashSet<String>,
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
    /// 这份缓存，固定传 `ConversationAgentKind::Claude`——历史会话页加多 agent tab 不该改变
    /// 那个功能的行为，两处刻意共享同一套读写路径而不是各建一份。
    session_list: HashMap<String, (Instant, Rc<Vec<session_history::SessionSummary>>)>,
    /// 正在后台扫描历史会话列表的 key（同上 `"{agent_id}:{cwd}"`，防重复并发 spawn）。
    session_list_inflight: HashSet<String>,
    /// 扫描期间发生删除等变更的 key；旧扫描完成后会自动再扫一次，避免旧快照回写。
    session_list_invalidated: HashSet<String>,
    /// 历史会话标题搜索框；只过滤已加载列表，不触发 transcript 重扫。
    history_filter: Option<Entity<gpui_component::input::InputState>>,
    _history_filter_sub: Option<Subscription>,
    /// 用户在 agent 尚未返回 resume id 前改名的 ACP 会话。只记录本次显式改名，
    /// 因此不会把旧 workspace.json 里的 custom_title 迁移进历史元数据仓库。
    pending_history_title_persist: HashSet<u64>,
    /// 上一次把用户命名覆盖层读进 `HistoryTitles` 缓存的时刻。侧栏每帧都要用它，
    /// 但改名本身会写穿缓存，所以这里只是低频兜底（覆盖移动端/历史页的远端改名）。
    history_titles_at: Option<Instant>,
    /// 是否已有一次后台重读在路上，防止逐帧重复 spawn。
    history_titles_inflight: bool,
    /// 当前选中查看的历史会话（路径 + 解析出的对话内容）；None 表示未选。
    session_detail: Option<(PathBuf, Rc<session_history::SessionDetail>)>,
    /// 历史会话右键删除的二次确认目标（None = 没在删除）。
    delete_history_target: Option<session_history::DeleteHistoryTarget>,
    /// 历史会话右侧消息详情的可变高度虚拟列表状态。
    history_detail_list_state: gpui::ListState,
    /// 加载会话详情的自增序号：后台解析完成时用它判断结果是否已过期（切了别的会话）。
    session_detail_gen: u64,
    /// 历史会话页当前选中查看哪家 agent 的历史（Claude/Copilot/Codex/
    /// Grok 分 tab，各自存储格式不同，见 session_history.rs 头部注释）。
    history_agent: settings::HistorySourceKind,
    /// 选中的是手动添加的 workspace profile（而不是某个基础 agent 槽位）时是
    /// `Some(profile_id)`；`history_agent` 这时候是该 profile 底层接的种类。
    history_profile: Option<String>,
    /// 上一次渲染历史时使用的项目根。项目切换后在下一次历史渲染前失效旧详情。
    history_project_root: Option<String>,
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
    /// 用户已确认退出；防止异步更新收尾期间重复提交退出动作。
    quit_requested: bool,
    /// 在线更新状态机（检查/下载/暂存就绪），驱动设置页"更新"分区 + 齿轮强调色。
    update_status: updater::UpdateStatus,
    /// 每次开始或取消安装等待时递增，防止旧的后台重试在新请求上继续执行。
    update_install_generation: u64,
    /// 设置窗口打开时要停在哪一页（对应左侧子菜单的独立内容页）。
    settings_section: settings::SettingsSection,
    /// 每请求跳一次页就 +1，用来变更 `Settings` 元素的 id。
    ///
    /// `Settings` 把当前选中页存在 `use_keyed_state` 里，只有该 id 首次出现时才读
    /// `default_selected_index`——窗口已经开着时改字段是不起作用的。把这个自增序号
    /// 编进 id，就能强制它按新的 default 重建一次。不用页号本身当 id：用户手动切走后
    /// 再点同一个入口，页号没变，id 也就没变，照样跳不过去。
    settings_page_nonce: usize,
    /// 设置页字体下拉的选项，首次渲染时算一次就缓存住。
    ///
    /// `all_font_names()` 在 mac 上枚举的是全部字体 face 的 descriptor（本机 902 个），
    /// 再逐个 CopyAttribute 取 family name，实测约 50ms/次——远超 60fps 的 16.6ms 预算。
    /// 它原先直接写在 `render_settings_content` 里，设置窗口每帧都要重算一遍，下拉一
    /// 展开就肉眼可见掉帧。字体列表在进程生命周期内几乎不变，不值得每帧重扫。
    font_options: std::cell::OnceCell<std::sync::Arc<Vec<(SharedString, SharedString)>>>,
    /// 上次同步给 Dock 的待关注会话数；None 强制首次同步。
    dock_badge_count: Option<usize>,
    /// 上次同步给菜单栏图标的运行中会话数；None 强制首次同步。
    status_running_count: Option<usize>,
    /// 上次同步给菜单栏下拉菜单的会话快照；None 强制首次同步。只在快照真的变化
    /// 时才重建 AppKit 菜单，避免高频 daemon 状态事件反复拆建。
    status_menu_snapshot: Option<Vec<status_item::SessionEntry>>,
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
    /// 「关联 Worktree」弹窗状态（Some = 弹窗开着）。见 git_panel 模块。
    worktree_list: Option<WorktreeListState>,
    /// 「新建 Worktree」弹窗状态（Some = 弹窗开着）。见 git_panel 模块。
    new_worktree: Option<NewWorktreeState>,
    /// 正在确认丢弃的 diff 块：(仓库根, hunk 下标)。丢弃直接改工作区文件且不进
    /// reflog，找不回来，所以必须过一道确认。
    discard_hunk_target: Option<(String, usize)>,
    /// 正在确认丢弃整个文件的改动：(仓库根, 相对路径, 是否未跟踪)。未跟踪文件是
    /// 直接删盘，比 restore 更狠，文案要分开写。
    discard_file_target: Option<(String, String, bool)>,
    /// 「丢弃全部改动」确认弹窗的目标仓库根（Some = 弹窗开着）。见 git_panel 模块。
    discard_all_target: Option<String>,
    /// git 远端同步 / stash 操作进行中：Some(操作名) = 正在跑，None = 空闲。
    /// 既做并发闸门（防连点抢 index.lock），也给 SOURCE CONTROL 头显示「拉取中…」
    /// 这类进行中反馈——否则点了按钮几秒内毫无动静。见 git_panel 模块 run_git_op。
    git_op: Option<&'static str>,
    /// 各类后台操作（建/删 worktree、生成 commit message 等）失败时的提示，render
    /// 顶部取走并弹成通知；后台任务里没有 Window，弹不了通知，所以先暂存到这。
    background_error: Option<String>,
    /// 应用级 provider 额度快照。它不属于任何 ACP 会话，终端和 ACP 头栏都只读这份
    /// 状态；刷新由 provider_quota 模块自己的后台循环负责。
    provider_quota_state: provider_quota::ProviderQuotaState,
    provider_quota_refresh_tx: Option<smol::channel::Sender<settings::ConversationAgentKind>>,

    /// 守护进程是否落后于磁盘上的 smeltd 二进制（重装/重编译后常见，需手动重启守护
    /// 才生效新代码）；None 表示还没查过，驱动设置页「更新」分区的重启提示。
    daemon_outdated: Option<bool>,
    /// 最近一次无缝升级的结果提示（设置页守护分区显示；None = 没试过）。
    daemon_upgrade_msg: Option<String>,
    /// 无缝升级进行中（按钮置灰防连点）。
    daemon_upgrading: bool,
    /// 检测到守护已过期、但有 agent 正在跑，升级挂起等空闲：true = 待升级。
    /// 空闲后（或用户手动点升级）会清掉，转成真正的 `upgrade_daemon_seamless`。
    daemon_upgrade_pending: bool,
    /// 守护自报的运行信息（PID / 启动时刻 / 会话数），设置页「更新」里展示。
    /// 跟 daemon_outdated 同一趟后台探测回填；守护没起 → None。
    daemon_info: Option<terminal::DaemonInfo>,
    /// 「重启守护进程」二次确认弹窗开关：点确定会断开所有当前终端会话。
    show_daemon_restart_confirm: bool,
    /// 「会话管理」弹窗开关：设置页「更新」tab 点开会话数详情用。守护进程持有
    /// 的会话不只 GUI 侧栏认领的那些——测试跑出来的游离会话、忘了关的临时会话都会
    /// 计进「N 个会话」里但从没在任何侧栏露过面，只有这里能看见并单独清理，
    /// 不用被迫走「重启守护进程」这种会误伤正常会话的核选项。
    session_manager_open: bool,
    /// 弹窗数据：最近一次身份镜像快照，None = 镜像未到。
    session_manager_list: Option<Vec<terminal::DaemonSessionState>>,
    /// 刚杀掉、subscribe 尚未确认消失的 id。刷新时滤掉，避免 Update 把条目刷回来。
    session_manager_tombstones: HashSet<String>,
    /// 上一帧工作区窗口是否在前台。上升沿用来清正在看的会话未读。
    workspace_window_active: bool,
    /// 启动时从存档恢复失败的会话（守护未就绪等）。仍写回 SQLite
    /// 工作区快照，避免「恢复失败 → 写空盘 → 会话永久蒸发」。侧栏本帧
    /// 看不到它们；当前进程会自动重试，用尽次数后下次冷启动再试。
    restore_pending: Vec<(usize, SessionState)>,
    /// 已经完成的失败恢复轮次。用来限制握手超时后的自动重试。
    restore_retry_attempt: u32,
    /// 用户在后台恢复期间删除的项目路径；尚未交货的恢复结果命中这些路径时直接丢弃。
    cancelled_restore_paths: Vec<String>,
    /// 远程目录来自 app 级唯一 daemon 订阅；随 Workspace 释放，避免创建第二条 socket
    /// 订阅或后台轮询线程。
    _remote_sessions_sub: Option<Subscription>,
    /// 自动化目录读 AgentHostState；不观察的话，投影写进 global 后当前页不会重绘。
    _agent_host_sub: Option<Subscription>,
    /// 正在后台 reattach 中、尚未插入侧栏的移动端终端会话 id。不去重会对同一 id
    /// 并发 open 两次，smeltd 第二次会顶掉前一个连接。
    remote_terminal_reattach_pending: HashSet<String>,
    /// 最近一次 subscribe 快照里仍 Active 的远程终端 id。延迟 reattach 完成时用它校验，
    /// 目录已经撤回就丢弃结果，不要插回幽灵会话。
    remote_projected_terminal_ids: HashSet<String>,
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
    /// 文件树的专属键盘焦点。文件树的方向键导航只在这个焦点生效，不能让终端、
    /// ACP 输入框或其它控件冒泡出来的按键误操作右侧面板。
    file_tree_focus_handle: FocusHandle,
    /// 冷启动的会话恢复流程是否已经跑完（没有待恢复会话时启动即为 true）。
    /// save_state 的抹盘安全阀靠它区分「还没恢复上来的空」和「用户真把会话全关了」。
    sessions_restored: bool,
    /// 主窗口关闭期间点击系统通知时，会话还在异步恢复。先保留稳定 sid，目标装回
    /// 列表后再消费，不能退化成只打开一扇空窗口。
    pending_notification_session_id: Option<String>,
    /// 冷恢复后的「回到上次那段智能体对话」是否已经补位过。
    ///
    /// 一次性闸门。智能体面的子页不落盘，补位只该在恢复完成后做一次；持续做会把
    /// 用户返回智能体目录的操作顶回对话页。
    restored_agent_conversation_route: bool,
    /// workspace 文档存在但无法解析时，禁止空快照覆盖原数据；有新会话后才允许
    /// 用新的有效快照替换损坏内容。
    workspace_state_load_failed: bool,
    /// workspace 文档的唯一写入队列。退出也必须排到这里，不能旁路另起写者。
    ws_write_queue: WorkspaceSnapshotWriteQueue,
    /// deferred 保存携带的代际。退出 flush 切换代际，已排队但尚未进入写队列的旧快照
    /// 会被淘汰，不能在最终快照之后反向覆盖。
    ws_snapshot_epoch: u64,
    ws_snapshot_finalizing: bool,
    /// 每个桌面 Workspace 是菜单快照的唯一发布者；source revision 用来拒绝同一发布者
    /// 因重连、超时重试而迟到的旧菜单。
    workspace_menu_source_id: String,
    next_workspace_menu_source_revision: AtomicU64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpdateInstallTrigger {
    Launch,
    User,
    Quit,
}

impl UpdateInstallTrigger {
    fn retries_while_busy(self) -> bool {
        !matches!(self, Self::Quit)
    }

    fn relaunches(self) -> bool {
        !matches!(self, Self::Quit)
    }
}

/// 自动重连循环每轮的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AcpReconnectStep {
    /// 普通交互会话已恢复到可用相位。
    Recovered,
    /// provider 仍在启动；保持当前重连预算。
    Waiting,
    /// 本次发起了重连，继续退避等待。
    Retrying,
    /// 预算耗尽或 handle 状态不允许再重连；由调用方做终态判定。
    GiveUp,
}

fn daemon_owns_acp_delivery_session(delivery_id: Option<&str>) -> bool {
    delivery_id.is_some()
}

/// 字段归属约定（写新代码前先读）：
/// - 右侧抽屉（Tool Panel / 文件树 / Git）跟**选中的项目**走，热场在
///   `active_session_ui` 里，按项目根停到 `project_ui`。
/// - `Workspace` 放跨会话共享的缓存与后台任务：git status、watcher、目录缓存。
impl std::ops::Deref for Workspace {
    type Target = SessionUiState;

    fn deref(&self) -> &Self::Target {
        &self.active_session_ui
    }
}

impl std::ops::DerefMut for Workspace {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.active_session_ui
    }
}

impl Workspace {
    /// Files 既可能展开到舞台，也可能停靠在 Tool Panel；两种状态都视为文件视图可见。
    fn files_view_visible(&self) -> bool {
        self.tool_panel_stage_active(tool_panel::ToolPanelTab::Files)
            || (self.tool_panel_open && self.tool_panel_tab == tool_panel::ToolPanelTab::Files)
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 插件 tab 注册表必须先于存档读取建好：存档里的 tab 以插件 key 记录，
        // 注册表为空时会全部降级成 Files，用户上次停在插件面板就再也回不去。
        plugin_ui::refresh_once(cx);
        // 存档只读元数据；**不**在 UI 线程同步 Terminal::spawn（会 beachball 数秒）。
        // 会话 reattach 丢后台线程，窗口先起来用户即可点侧栏/设置。
        let (saved, workspace_state_load_failed, workspace_load_error) = match load_ws_state() {
            WorkspaceLoad::Loaded(state) => (Some(state), false, None),
            WorkspaceLoad::Missing => (None, false, None),
            WorkspaceLoad::Failed(error) => {
                eprintln!("[workspace] workspace 存档无法读取: {error}");
                (None, true, Some(error))
            }
        };
        if workspace_state_load_failed {
            eprintln!("[workspace] 暂停空快照写入，避免覆盖原数据");
        }
        // 旧版开合动画曾把过渡中的接近零宽度误存为用户偏好；加载时按侧栏
        // 可拖拽下限修复这类状态，避免下一次展开仍以错误宽度为目标。
        let sidebar_w = saved
            .as_ref()
            .and_then(|s| s.sidebar_w)
            .unwrap_or(DEFAULT_SIDEBAR_WIDTH)
            .max(MIN_SIDEBAR_WIDTH);
        let sidebar_open = saved.as_ref().and_then(|s| s.sidebar_open).unwrap_or(true);
        let (pending_sessions, active_session) = saved
            .as_ref()
            .map(normalize_saved_sessions)
            .unwrap_or_default();
        let saved_active_session_id = saved.as_ref().and_then(|state| {
            state
                .active_session_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    pending_sessions
                        .get(active_session)
                        .and_then(session_state_persist_id)
                })
        });
        // 恢复完成前先放进 pending：save_state 会合并 pending，避免空 sessions 窗口期抹盘。
        let restore_pending = pending_sessions.iter().cloned().enumerate().collect();
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
        // 日志页三栏 resize（不落盘：日志是临时查看，没必要持久化）。
        let git_log_resize = cx.new(|_| ResizableState::default());
        let stage_tool_panel_resize = cx.new(|_| ResizableState::default());
        let _stage_tool_panel_resize_sub = cx.subscribe(
            &stage_tool_panel_resize,
            |this, state, _e: &ResizablePanelEvent, cx| {
                // Tool Panel 入场动画期间也靠 resize_panel 逐帧改宽度触发这个事件，
                // 那是过渡态中间值，不是用户真的拖出来的宽度。
                if this.tool_panel_transition.is_animating() {
                    return;
                }
                if let Some(size) = state.read(cx).sizes().get(1) {
                    this.tool_panel_w = f32::from(*size);
                }
                this.save_state(cx);
            },
        );
        // 没有会话时右侧不继承全局偏好；新会话使用 SessionUiState 的默认收起状态。
        let initial_session_ui = SessionUiState::default();

        let mut ws = Self {
            sessions,
            active_session,
            nav: WorkspaceNav::from_persisted(
                saved
                    .as_ref()
                    .map(|state| state.route.clone())
                    .unwrap_or_default(),
                saved
                    .as_ref()
                    .and_then(|state| state.active_workspace_surface.clone())
                    .filter(|key| plugin_ui::workspace_surface_by_key(key).is_some()),
            ),
            agent_surface: agents::AgentsSurface::from_persisted(
                saved
                    .as_ref()
                    .and_then(|state| state.selected_agent_id.clone()),
            ),
            automation_surface: agents::AutomationsSurface::default(),
            ui_session_id: None,
            active_session_ui: initial_session_ui,
            project_ui: HashMap::new(),
            active_project_ui_key: None,
            tool_panel_transition: panel_transition::PanelTransition::new(false),
            plugin_surface_sync_scheduled: false,
            workspace_surface_titles: saved
                .as_ref()
                .map(|state| state.workspace_surface_titles.clone())
                .unwrap_or_default(),
            context_menu_suppressed: false,
            context_menu_generation: 0,
            dir_cache: HashMap::new(),
            dir_inflight: HashSet::new(),
            file_tree_pending_reveal: None,
            acp_image_preview: None,
            file_gen: 0,
            pending_file_switch: None,
            delete_file_target: None,
            pending_switch_after_save: None,
            git_log: git_log::GitLogState::default(),
            pushing: false,
            delete_branch_target: None,
            git_log_resize,
            stage_tool_panel_resize,
            sidebar_open,
            sidebar_transition: panel_transition::PanelTransition::new(sidebar_open),
            sidebar_w,
            sidebar_drag_start: None,
            diff_comment_input: None,
            commit_msg_inputs: HashMap::new(),
            commit_errors: HashMap::new(),
            // 没有待恢复会话 → 一开始就算「恢复完毕」，否则等 schedule_session_restore 置位。
            // 存档读失败时绝不能算恢复完毕：那会让空快照被当成「用户关光了所有会话」。
            sessions_restored: pending_sessions.is_empty() && !workspace_state_load_failed,
            pending_notification_session_id: None,
            restored_agent_conversation_route: false,
            workspace_state_load_failed,
            ws_write_queue: WorkspaceSnapshotWriteQueue::default(),
            ws_snapshot_epoch: 0,
            ws_snapshot_finalizing: false,
            workspace_menu_source_id: uuid::Uuid::new_v4().to_string(),
            next_workspace_menu_source_revision: AtomicU64::new(1),
            close_project_target: None,
            projects,
            active_project: None,
            collapsed_projects: saved
                .as_ref()
                .map(|s| s.collapsed_projects.iter().cloned().collect())
                .unwrap_or_default(),
            collapsed_agents: saved
                .as_ref()
                .map(|s| s.collapsed_agents.iter().cloned().collect())
                .unwrap_or_default(),
            pinned_projects: saved
                .as_ref()
                .map(|s| {
                    s.pinned_projects
                        .iter()
                        .map(|root| crate::sidebar_order::normalize_project_root(root))
                        .filter(|root| !root.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            sidebar_grouping: saved
                .as_ref()
                .map(|s| s.sidebar_grouping)
                .unwrap_or_default(),
            sidebar_hide_empty_projects: saved
                .as_ref()
                .map(|s| s.sidebar_hide_empty_projects)
                .unwrap_or(false),
            sess_drop_hint: None,
            proj_drop_hint: None,
            sidebar_drag: None,
            sidebar_scroll: ScrollHandle::new(),
            sidebar_auto_scroll: gpui_component::scroll::AutoScroll::default(),
            ide_catalog: ide::IdeCatalog::default(),
            ide_popup_waiters: Vec::new(),
            palette: None,
            _palette_sub: None,
            diff_scroll: VirtualListScrollHandle::new(),
            diff_code_scroll: ScrollHandle::new(),
            git_list_scroll: ScrollHandle::new(),
            file_tree_scroll: ScrollHandle::new(),
            file_tree_drag_start: None,
            file_filter: None,
            _file_filter_sub: None,
            search_results: None,
            search_gen: 0,
            _stage_tool_panel_resize_sub,
            git_status: HashMap::new(),
            git_repos: HashMap::new(),
            git_repos_inflight: HashSet::new(),
            active_git_repo: None,
            git_repo_collapsed: HashSet::new(),
            git_status_generation: HashMap::new(),
            git_status_inflight: HashSet::new(),
            git_status_failures: HashMap::new(),
            git_index_pending: HashMap::new(),
            branches: HashMap::new(),
            branches_inflight: HashSet::new(),
            git_watchers: HashMap::new(),
            git_autofetch_at: HashMap::new(),
            session_list: HashMap::new(),
            session_list_inflight: HashSet::new(),
            session_list_invalidated: HashSet::new(),
            history_filter: None,
            _history_filter_sub: None,
            pending_history_title_persist: HashSet::new(),
            history_titles_at: None,
            history_titles_inflight: false,
            session_detail: None,
            delete_history_target: None,
            history_detail_list_state: gpui::ListState::new(0, gpui::ListAlignment::Top, px(800.)),
            session_detail_gen: 0,
            history_agent: settings::HistorySourceKind::Conversation(
                settings::ConversationAgentKind::Claude,
            ),
            history_profile: None,
            history_project_root: None,
            launch_inputs: None,
            profile_inputs: None,
            dsh_plugin_manager: None,
            dsh_model_editor: None,
            dsh_custom_provider_editor: None,
            dsh_native_model_settings: None,
            dsh_model_editor_error: None,
            dsh_model_capabilities: None,
            pi_model_editor: None,
            pi_custom_provider_editor: None,
            pi_auth_providers: None,
            pi_login: None,
            pi_auth_model_picker: None,
            pi_auth_error: None,
            pi_auth_show_all: false,
            pi_model_settings: std::cell::OnceCell::new(),
            pi_model_editor_error: None,
            pi_plugins: std::cell::OnceCell::new(),
            pi_plugin_pending_delete: None,
            pi_plugin_error: None,
            opacity_slider: None,
            ui_font_size_slider: None,
            font_size_slider: None,
            bg_image_opacity_slider: None,
            bg_color_picker: None,
            settings_subs: Vec::new(),
            applied_window_bg: None,
            applied_window_opacity: None,
            applied_glass_style: None,
            applied_ui_font_px: None,
            debug_hud: false,
            last_frame: None,
            debug_mem_rss: None,
            debug_mem_sampled_at: None,
            fps_ema: 0.0,
            show_quit_confirm: false,
            quit_requested: false,
            update_status: updater::UpdateStatus::default(),
            update_install_generation: 0,
            settings_section: settings::SettingsSection::default(),
            settings_page_nonce: 0,
            font_options: std::cell::OnceCell::new(),
            dock_badge_count: None,
            status_running_count: None,
            status_menu_snapshot: None,
            rename_target: None,
            rename_input: None,
            _rename_sub: None,
            repo_info: HashMap::new(),
            repo_info_inflight: HashSet::new(),
            delete_worktree_target: None,
            worktree_list: None,
            new_worktree: None,
            discard_hunk_target: None,
            discard_file_target: None,
            discard_all_target: None,
            git_op: None,
            background_error: workspace_load_error
                .map(|error| format!("工作区存档无法打开，已停止写入以免覆盖原会话。{error}")),
            provider_quota_state: provider_quota::ProviderQuotaState::default(),
            provider_quota_refresh_tx: None,

            daemon_outdated: None,
            daemon_upgrade_msg: None,
            daemon_upgrading: false,
            daemon_upgrade_pending: false,
            daemon_info: None,
            show_daemon_restart_confirm: false,
            session_manager_open: false,
            session_manager_list: None,
            session_manager_tombstones: HashSet::new(),
            workspace_window_active: false,
            restore_pending,
            restore_retry_attempt: 0,
            saved_active_session_id: saved_active_session_id.clone(),
            cancelled_restore_paths: Vec::new(),
            _remote_sessions_sub: None,
            _agent_host_sub: None,
            remote_terminal_reattach_pending: HashSet::new(),
            remote_projected_terminal_ids: HashSet::new(),
            session_list_revision: 0,
            active_session_revision: 0,
            focus_handle: cx.focus_handle(),
            file_tree_focus_handle: cx.focus_handle(),
        };
        // 只预取一个已知的系统 Finder 图标，不枚举第三方应用；真实 IDE 列表由
        // stage.rs 在有项目舞台显示后的空闲期再低优先级预热。
        ws.preload_file_manager_icon(window, cx);
        // 额度是应用级账户状态，启动后独立刷新，不依赖任何 ACP 会话是否创建。
        ws.start_provider_quota_watch(cx);
        // 侧栏「+」菜单按本机已装 CLI 过滤；启动后立刻探一次，免得等用户打开设置。
        ws.refresh_acp_runtime(cx);
        // pending 已挂上全部待恢复会话 → 写盘不会抹掉存档。
        // 存档读失败时连这次启动首帧保存也跳过，避免空快照被排队。
        if !ws.workspace_state_load_failed {
            ws.save_state(cx);
        }
        // 状态恢复、交换事务收敛和旧包清理由 updater 在同一把锁内完成；恢复出待安装
        // 作业后走与用户点击完全相同的安装驱动，没有待办才做常规静默检查。
        ws.recover_update_at_launch(true, cx);
        // 有待恢复会话：ensure+reattach 在 restore 线程串行做完后再 check_daemon_outdated，
        // 避免与 ensure handoff 三线并行踩踏。无会话则直接查守护状态。
        if !pending_sessions.is_empty() {
            eprintln!(
                "[workspace] 后台恢复 {} 个会话（不堵 UI）…",
                pending_sessions.len()
            );
            ws.schedule_session_restore(
                pending_sessions,
                active_session,
                saved_active_session_id,
                window,
                cx,
            );
        } else {
            ws.check_daemon_outdated(cx);
            std::thread::Builder::new()
                .name("smelt-plugin-reload".into())
                .spawn(|| {
                    let _ = terminal::ensure_managed_daemon_current();
                    let _ = terminal::plugin_reload();
                })
                .ok();
        }
        ws.start_daemon_outdated_watch(cx);
        ws.reconcile_remote_catalog_projection(window, cx);
        ws._remote_sessions_sub = Some(
            cx.observe_global_in::<RemoteSessionCatalogGlobal>(window, |this, window, cx| {
                this.reconcile_remote_catalog_projection(window, cx)
            }),
        );
        ws._agent_host_sub =
            Some(cx.observe_global_in::<settings::AgentHostState>(window, |_, _, cx| cx.notify()));
        ws
    }

    /// 每 60s 探一次守护是否落后于磁盘上的 smeltd。手工装 App / dev 重编译后
    /// 没有「更新事件」可挂,只能靠检测发现;探到落后就走 `check_daemon_outdated`
    /// 的空闲门控——agent 忙就挂起,空闲才升,不打扰使用(连接路径的 ensure 只
    /// 对齐磁盘不再偷跑 exec,这里负责兜底把守护在空闲时跟上)。
    /// 探测是本地 socket 往返(毫秒级),60s 一次对用户完全无感。
    fn start_daemon_outdated_watch(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(60))
                    .await;
                let ok = this.update(cx, |this, cx| {
                    this.check_daemon_outdated(cx);
                });
                if ok.is_err() {
                    break; // Workspace 已销毁
                }
            }
        })
        .detach();
    }

    /// 将 app 级 daemon 目录投影到当前窗口。全局状态由唯一的 subscribe 循环写入，
    /// 此处不会轮询文件或额外建立 socket。
    fn reconcile_remote_catalog_projection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(global) = cx.try_global::<RemoteSessionCatalogGlobal>() else {
            return;
        };
        // 尚未收到 daemon 快照时，不能把启动空档误判成权威空目录，进而拆掉刚恢复的投影。
        if global.generation == 0 {
            return;
        }
        // `None` 表示 daemon 无法读取持久目录，不是空目录。保留现有投影，等一次可用
        // 快照再对账，避免损坏文档或短暂 I/O 故障把 GUI 中的远程会话错误移除。
        let Some(snapshot) = global
            .snapshot
            .lock()
            .ok()
            .and_then(|snapshot| snapshot.clone())
        else {
            return;
        };
        let acp_records = snapshot
            .sessions
            .iter()
            .filter_map(smelt_core::session_control::RemoteSessionRecord::as_acp)
            .collect::<Vec<_>>();
        let term_records = snapshot
            .sessions
            .iter()
            .filter(|record| record.is_visible())
            .filter_map(smelt_core::session_control::RemoteSessionRecord::as_terminal)
            .collect::<Vec<_>>();
        self.reconcile_remote_sessions(acp_records, window, cx);
        self.reconcile_remote_terminal_sessions(term_records, window, cx);
    }

    fn reconcile_remote_sessions(
        &mut self,
        records: Vec<smelt_core::session_control::RemoteAcpSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let records = recognized_remote_acp_sessions(records);
        let stale = self
            .sessions
            .iter()
            .enumerate()
            .filter_map(|(ix, session)| {
                if !session.remote_owned {
                    return None;
                }
                let SessionKind::Conversation(view) = &session.kind else {
                    return None;
                };
                let session_id = view.read(cx).session_id();
                let auto_projected_background = session.automation_id.is_none()
                    && smelt_core::session_control::is_background_acp_session_id(session_id);
                unproject_acp_when_remote_catalog_drops(
                    session.remote_owned,
                    auto_projected_background,
                )
                .then_some(ix)
            })
            .collect::<Vec<_>>();
        for ix in stale.into_iter().rev() {
            self.unproject_remote_session(ix, cx);
        }

        let existing = self
            .sessions
            .iter()
            .filter_map(|session| match &session.kind {
                SessionKind::Conversation(view) => Some(view.read(cx).session_id().to_string()),
                SessionKind::Term { .. } => None,
            })
            .collect::<HashSet<_>>();
        let mut changed = false;
        for (record, agent) in records {
            if existing.contains(&record.id) {
                continue;
            }
            if !record.is_visible()
                || smelt_core::session_control::is_background_acp_session_id(&record.id)
            {
                continue;
            }
            if record.lifecycle != smelt_core::session_control::RemoteSessionLifecycle::Active {
                continue;
            }
            self.project_remote_acp_session(record, agent, window, cx);
            changed = true;
        }
        if changed {
            self.session_list_revision = self.session_list_revision.wrapping_add(1);
            self.save_state(cx);
            cx.notify();
        }
    }

    fn project_remote_acp_session(
        &mut self,
        record: smelt_core::session_control::RemoteAcpSession,
        agent: settings::ConversationAgentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let profile_id = record
            .agent_option_id
            .strip_prefix("profile:")
            .map(String::from);
        let stored_title = record.resume_id.as_deref().and_then(|resume_id| {
            smelt_core::session_metadata::custom_title(
                agent.into(),
                profile_id.as_deref(),
                resume_id,
            )
        });
        // 手机上用产品智能体开的对话必须带着归属落地，否则接管后它会掉进项目
        // 会话列表，而不是侧栏「对话」栏。
        let agent_definition_id = record.agent_definition_id().map(str::to_string);
        let resume_id = record
            .resume_id
            .map(agent_client_protocol::schema::v1::SessionId::new);
        let cwd = Some(record.cwd.clone());
        if !smelt_core::session_control::is_agent_conversation(
            None,
            agent_definition_id.as_deref(),
            cwd.as_deref(),
        ) {
            self.remember_session_project(cwd.as_deref());
        }
        let view = cx.new(|cx| {
            acp_view::AcpView::placeholder(
                cx,
                acp_view::AcpViewOrigin {
                    agent,
                    launch: record.launch,
                    refresh_launch_from_settings: false,
                    profile_id,
                    cwd,
                    reason: "正在连接移动端创建的会话…".to_string(),
                    entries: Vec::new(),
                    resume_session_id: resume_id,
                    saved_sid: Some(record.id),
                },
            )
        });
        let _acp_persist_sub = Some(self.subscribe_acp_persist(&view, window, cx));
        self.sessions.push(Session {
            ui_id: next_session_ui_id(),
            kind: SessionKind::Conversation(view),
            last_updated_at: unix_now_secs(),
            custom_title: stored_title
                .or_else(|| (!record.title.trim().is_empty()).then_some(record.title)),
            agent_definition_id,
            automation_id: None,
            remote_owned: false,
            _acp_persist_sub,
            ui_state: SessionUiState::default(),
        });
    }

    pub(crate) fn remote_acp_record(
        &self,
        session_id: &str,
        cx: &App,
    ) -> Option<smelt_core::session_control::RemoteAcpSession> {
        let global = cx.try_global::<RemoteSessionCatalogGlobal>()?;
        let snapshot = global
            .snapshot
            .lock()
            .ok()
            .and_then(|snapshot| snapshot.clone())?;
        snapshot
            .sessions
            .iter()
            .filter_map(smelt_core::session_control::RemoteSessionRecord::as_acp)
            .find(|record| record.is_present() && record.id == session_id)
    }

    /// 跟 `reconcile_remote_sessions` 是同一套思路，对应移动端新建/删除的终端会话：
    /// 手机让 smeltd 建 PTY 并更新 daemon 自己的远程目录；这里消费同一条 subscribe
    /// 投影，把它 reattach 成 PC 侧栏里能看/能操作的一个真实 `Session`。
    fn reconcile_remote_terminal_sessions(
        &mut self,
        records: Vec<smelt_core::session_control::RemoteTerminalSession>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids = records
            .iter()
            .map(|record| record.id.clone())
            .collect::<HashSet<_>>();
        self.remote_projected_terminal_ids = ids.clone();
        let stale = self
            .sessions
            .iter()
            .enumerate()
            .filter_map(|(ix, session)| {
                if !session.remote_owned {
                    return None;
                }
                let SessionKind::Term { active, .. } = &session.kind else {
                    return None;
                };
                (!ids.contains(active.read(cx).session_id())).then_some(ix)
            })
            .collect::<Vec<_>>();
        for ix in stale.into_iter().rev() {
            self.unproject_remote_session(ix, cx);
        }
        // 目录撤回后清掉在途标记，避免同 id 再出现时被当成重复而跳过。
        // 已经 detach 出去的 reattach 线程不会因此取消；完成回调必须再查投影集。
        self.remote_terminal_reattach_pending
            .retain(|id| ids.contains(id));

        let existing = self
            .sessions
            .iter()
            .filter_map(|session| match &session.kind {
                SessionKind::Term { active, .. } => Some(active.read(cx).session_id().to_string()),
                SessionKind::Conversation(_) => None,
            })
            .collect::<HashSet<_>>();
        for record in records {
            if existing.contains(&record.id) {
                continue;
            }
            if record.lifecycle != smelt_core::session_control::RemoteSessionLifecycle::Active {
                continue;
            }
            // 目录更新可能在一次 reattach 还没完成前再次到达；不去重会对同一 id
            // 并发 open 两次——smeltd 对同 id 第二次 open 会顶掉前一个连接，造成两条
            // 侧栏项目（一条僵死一条存活）。
            if !self
                .remote_terminal_reattach_pending
                .insert(record.id.clone())
            {
                continue;
            }
            self.remember_session_project(Some(record.cwd.as_str()));
            let (tx, rx) = smol::channel::bounded(1);
            let cwd_bg = record.cwd.clone();
            let sid_bg = record.id.clone();
            std::thread::Builder::new()
                .name("smelt-remote-term-reattach".into())
                .spawn(move || {
                    let r = terminal::Terminal::reattach(24, 80, Some(&cwd_bg), &sid_bg);
                    let _ = tx.send_blocking(r);
                })
                .expect("spawn smelt-remote-term-reattach 线程");
            let title = (!record.title.trim().is_empty()).then_some(record.title.clone());
            let cwd = record.cwd.clone();
            let sid = record.id.clone();
            cx.spawn(async move |this, cx| {
                let result = match rx.recv().await {
                    Ok(r) => r,
                    Err(_) => {
                        let _ = this.update(cx, |this, _cx| {
                            this.remote_terminal_reattach_pending.remove(&sid);
                        });
                        return;
                    }
                };
                let terminal = match result {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("[workspace] 移动端终端会话 reattach 失败（{sid}）：{e:#}");
                        let _ = this.update(cx, |this, _cx| {
                            this.remote_terminal_reattach_pending.remove(&sid);
                        });
                        return;
                    }
                };
                let _ = this.update(cx, |this, cx| {
                    this.remote_terminal_reattach_pending.remove(&sid);
                    let local_ids = this
                        .sessions
                        .iter()
                        .filter_map(|session| match &session.kind {
                            SessionKind::Term { active, .. } => {
                                Some(active.read(cx).session_id().to_string())
                            }
                            SessionKind::Conversation(_) => None,
                        })
                        .collect::<HashSet<_>>();
                    if !smelt_core::session_control::accept_remote_terminal_projection(
                        &sid,
                        &this.remote_projected_terminal_ids,
                        &local_ids,
                    ) {
                        return;
                    }
                    let view = cx.new(|cx| {
                        TerminalView::from_terminal(cx, terminal, Some(cwd), sid, None, None)
                    });
                    let mut session = Session::single(view);
                    session.remote_owned = true;
                    session.custom_title = title;
                    this.sessions.push(session);
                    this.session_list_revision = this.session_list_revision.wrapping_add(1);
                    this.save_state(cx);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Daemon 状态事件直接分发给所有终端 pane；不能依赖 TerminalView::render，
    /// 因为非当前顶层会话不会被挂进当前窗口的渲染树。
    fn handle_daemon_state_event(
        &mut self,
        state: &terminal::DaemonSessionState,
        cx: &mut Context<Self>,
    ) {
        let panes: Vec<Entity<TerminalView>> = self
            .sessions
            .iter()
            .flat_map(Session::term_leaves)
            .collect();
        // 状态/相位变化不直接标脏 TerminalView：由
        // schedule_state_refresh 合并 notify Workspace，只更新侧栏与徽标。
        // 终端内容本身走 TerminalView::drive_redraws 的事件通道即时重绘，
        // 不依赖这里。
        for pane in panes {
            if pane.read(cx).session_id() == state.id {
                pane.update(cx, |terminal, cx| terminal.handle_daemon_state(state, cx));
            }
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 数据准备与页面绘制分开：缓存/控件/窗口材质在 prepare_frame，下面只组页面。
        self.prepare_frame(window, cx);

        // 插件 WebView 面板是原生 view，不随 GPUI 停止渲染而消失：每帧显式对齐
        // 它的显隐，并把页面发来的消息交给宿主处理。
        plugin_ui::refresh_once(cx);
        self.sync_plugin_panels(window, cx);
        self.drain_plugin_panel_messages(cx);

        // 主题色 token（跟随 gpui-component 主题，替代硬编码）
        let (c_muted, c_fg) = {
            let t = cx.theme();
            (t.muted_foreground, t.foreground)
        };
        // 内容区按 Grok Bot 画不透明实底。顶部导航的系统玻璃是窗口铬，不往分栏上叠透明度。
        let shell_surface: Hsla = rgb(ui_theme::bg_rail()).into();
        let sidebar_surface: Hsla = rgb(ui_theme::bg_column()).into();
        let stage_surface: Hsla = rgb(ui_theme::bg_stage()).into();
        let shell_padding = ui_theme::shell_padding();

        let sidebar_motion = self.sidebar_transition.frame();
        if sidebar_motion.animating {
            window.request_animation_frame();
        }
        let animate_sidebar_status =
            self.workspace_window_active && (self.sidebar_open || sidebar_motion.animating);

        // 一帧只读取一次各会话状态，避免侧栏重复遍历 Terminal pane / ACP 证据。
        let session_statuses: Vec<AgentStatus> = self
            .sessions
            .iter()
            .map(|session| session.status(cx))
            .collect();

        // 会话列表：单列按项目上下分组（替代旧 gpui-component Sidebar 两级菜单；
        // 设计稿的「rail + 列表」左右两列实测割裂，见 session_list.rs 文件头）。
        let list_el =
            self.render_session_list(cx.entity(), cx, &session_statuses, animate_sidebar_status);
        // 提升到舞台的那个 tab 不再停靠一份（见 tool_panel_promoted）。
        let tool_panel_motion = self.tool_panel_transition.frame();
        if tool_panel_motion.animating {
            window.request_animation_frame();
        }
        let tool_panel_el = (tool_panel_motion.mounted && !self.tool_panel_promoted())
            .then(|| self.render_tool_panel(window, cx));
        // 左侧让位宽度（stage.rs / tool_panel.rs 的 corner_guard 语义，改由调用方
        // 算好宽度传进去）：sidebar 收起时舞台/展开的 Tool Panel rail 会变成
        // 贴着窗口最左边那块，头栏要让出红绿灯 + 悬浮「切换左侧栏」按钮的宽度。
        // 全屏时红绿灯被 macOS 隐藏、切换按钮也移到 left(18px)（见 sidebar-toggle
        // 那里的注释），让位宽度跟着缩小：128px（非全屏，红绿灯 ~78px + 按钮
        // 92+24=116px 再加余量）→ 48px（全屏，按钮 18+24=42px 相对卡片左边缘
        // 9px 只剩 33px，加 15px 余量）。
        let left_guard = if self.sidebar_open {
            px(0.)
        } else if window.is_fullscreen() {
            px(48.)
        } else {
            px(128.)
        };

        let stage_content: AnyElement = match self.active_tab() {
            WorkspaceRoute::Plugin { .. } => self.render_workspace_surface(window, cx),
            WorkspaceRoute::Agents => self.render_agents_page(window, cx),
            WorkspaceRoute::Automations => self.render_automations_page(window, cx),
            WorkspaceRoute::Session => {
                if self.tool_panel_promoted() {
                    self.render_tool_panel_stage(left_guard, window, cx)
                } else {
                    // 只有实际显示会话舞台时才构造终端/ACP 内容。历史页、工具页等
                    // stage override 不应先渲染一棵随后被丢弃的交互树，否则 GPUI
                    // 会在同一布局周期重复取用元素状态。
                    let background_image = cx.global::<Appearance>().bg_image.clone();
                    let content = if self
                        .sessions
                        .get(self.active_session)
                        .is_some_and(|session| !session.is_product_conversation(cx))
                    {
                        // 共用顶栏高度留给右侧窗口铬；舞台不再自己画一条访达/标题。
                        div().flex_1().min_w_0().min_h_0().flex().flex_col().child(
                            div().flex_1().min_w_0().min_h_0().flex().child(
                                match &self.sessions[self.active_session].kind {
                                    SessionKind::Term { .. } => self.render_pane(
                                        self.sessions[self.active_session]
                                            .term_layout()
                                            .expect("Term 会话必有 layout"),
                                        "pane",
                                        cx,
                                    ),
                                    // ACP 会话与终端共用背景图；AcpView 根面板本身保持透明，
                                    // 所以图片会自然透过消息间隙，不需要把外观配置耦合进子 crate。
                                    // 容器先垫主题面板底色，半透明图片合成在底色上（同终端 bg_layer
                                    // 的做法），避免图片半透明后直接透到窗口材质显得杂乱。
                                    SessionKind::Conversation(view) => {
                                        let bg_image_opacity =
                                            cx.global::<Appearance>().bg_image_opacity;
                                        div()
                                            .relative()
                                            .flex_1()
                                            .min_w_0()
                                            .min_h_0()
                                            .flex()
                                            .bg(rgb(ui_theme::bg_stage()))
                                            .children(background_image.as_deref().map(|p| {
                                                workspace_frame::background_image_layer(
                                                    p,
                                                    bg_image_opacity,
                                                )
                                            }))
                                            .child(view.clone())
                                            .into_any_element()
                                    }
                                },
                            ),
                        )
                    } else {
                        // 空舞台跟 Grok 空对话同一套：大标题 + 一句说明 + 胶囊主操作。
                        // 建会话仍然走项目行的「+」，这里只负责把项目请进来。
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap_4()
                            .child(
                                div()
                                    .text_size(px(28.))
                                    .font_semibold()
                                    .text_color(c_fg)
                                    .child("Smelt"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(c_muted)
                                    .child("打开一个项目，开始对话或终端"),
                            )
                            .child(
                                div()
                                    .id("empty-open")
                                    .h(px(36.))
                                    .px_5()
                                    .rounded_full()
                                    .bg(rgb(ui_theme::action_fill()))
                                    .text_color(rgb(ui_theme::action_on()))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_sm()
                                    .font_semibold()
                                    .cursor_pointer()
                                    .hover(|s| s.opacity(0.88))
                                    .child("打开项目")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _w, cx| this.open_project(cx)),
                                    ),
                            )
                    };
                    content.into_any_element()
                }
            }
        };
        let stage = div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(stage_content);
        // 左栏始终挂载，收起时只压到 1px，避免重建整棵会话列表。展开目标是存档
        // 中的固定像素宽度；窗口太窄时只临时约束显示值，不反写用户偏好。
        let sidebar_target_w =
            sidebar_width_for_viewport(self.sidebar_w, window.viewport_size().width);
        let sidebar_w = (sidebar_motion.progress * sidebar_target_w).max(1.);
        let sidebar_gap = if sidebar_motion.progress > 0.01 {
            ui_theme::chrome_gap()
        } else {
            px(0.)
        };
        let sidebar_min_w = if sidebar_motion.animating || !self.sidebar_open {
            px(1.)
        } else {
            px(MIN_SIDEBAR_WIDTH)
        };
        let sidebar_column = div()
            .size_full()
            .pr(sidebar_gap)
            .child(
                workspace_frame::card(sidebar_surface)
                    // 内容避开浮在标题栏上的 macOS 交通灯；面板背景本身继续
                    // 延伸到窗口顶边，形成 Grok Bot 那种贴边侧栏。
                    .pt(workspace_frame::TOP_BAR_HEIGHT)
                    .opacity(sidebar_motion.progress.max(0.01))
                    .child(list_el)
                    .child(workspace_frame::with_window_drag(
                        // 左栏真正的 WORKSPACES 头在共用顶栏安全区下面；
                        // 安全区本身也要有与其余两栏一致的拖动/双击行为。
                        // 从 80px 开始，避免覆盖原生红绿灯的命中范围。
                        div()
                            .absolute()
                            .top_0()
                            .left(px(80.))
                            .right_0()
                            .h(workspace_frame::TOP_BAR_HEIGHT),
                    )),
            )
            .into_any_element();
        // 舞台分栏：实底矩形。交通灯让位交给右侧共用顶栏的 left_guard。
        let stage_card = workspace_frame::card(stage_surface).child(stage);

        // Stage + Tool Panel 仍使用自己的 h_resizable；外层左侧栏改为固定像素 flex，
        // 窗口尺寸变化只交给右侧区吸收，不再按比例改写侧栏的视觉宽度。
        let stage_and_tool_panel: AnyElement = if let Some(tool_panel) = tool_panel_el {
            let tool_panel_w = self.tool_panel_w;
            let progress = tool_panel_motion.progress;
            let panel_w = (progress * tool_panel_w).max(1.);
            let min_w = if tool_panel_motion.animating {
                px(1.)
            } else {
                px(MIN_TOOL_PANEL_WIDTH)
            };

            let tool_panel_card = workspace_frame::card(sidebar_surface)
                .opacity(progress.max(0.01))
                .child(tool_panel);

            // 真正把中间区推走的一步：programmatically 顶宽，而不是只改这块自己
            // 的 flex_basis 建议值（那只对"首次插入"这一帧生效，见 gpui-component
            // resizable::panel 的 initial_size 规则）。
            if tool_panel_motion.animating {
                self.stage_tool_panel_resize.update(cx, |state, cx| {
                    state.resize_panel(1, px(panel_w), window, cx);
                });
            }

            h_resizable("stage-tool-panel-split")
                .with_state(&self.stage_tool_panel_resize)
                .child(
                    resizable_panel()
                        .size_range(px(MIN_WORKSPACE_CONTENT_WIDTH)..Pixels::MAX)
                        .child(stage_card),
                )
                .child(
                    resizable_panel()
                        .size(px(panel_w))
                        .size_range(min_w..Pixels::MAX)
                        .flex_none()
                        // 发丝只加在右栏左侧，避免跟舞台右侧再垫一次变成 2px 槽。
                        .pl(ui_theme::chrome_gap())
                        .child(tool_panel_card),
                )
                .into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .child(stage_card)
                .into_any_element()
        };

        // Stage + Tool Panel 的水平分栏（stage_tool_panel_resize）最终作为右侧区
        // 主体，左侧会话栏不受影响；宽度铺到窗口最右边。
        let right_region: AnyElement = stage_and_tool_panel;

        let shared_chrome = self.render_shared_right_chrome(left_guard, cx);
        let right_column = div()
            .size_full()
            .flex()
            .flex_col()
            .child(shared_chrome)
            .child(div().flex_1().min_h_0().child(right_region));
        let workspace_columns = fixed_sidebar_columns(
            px(sidebar_w),
            sidebar_min_w,
            sidebar_column,
            right_column.into_any_element(),
        );
        let sidebar_resize_handle = (self.sidebar_open && !sidebar_motion.animating)
            .then(|| self.sidebar_resize_handle(sidebar_w, cx));
        let sidebar_resize_listener = self.sidebar_resize_listener(cx);
        // ContextMenu 的 deferred PopupMenu 可能拥有焦点并停止元素事件传播，
        // 因而只靠 Workspace 根节点收不到“菜单项点击/外部点击/Escape”。在
        // paint 阶段注册窗口级监听，先释放本地 WebView suppression lease，再让
        // 组件库继续执行自己的 dismiss/focus-restore 逻辑。
        let context_menu_event_bridge = {
            let workspace = cx.entity().downgrade();
            canvas(
                |_, _, _| {},
                move |_bounds, _, window, _cx| {
                    let workspace_for_mouse = workspace.clone();
                    window.on_mouse_event(move |event: &MouseDownEvent, phase, _window, cx| {
                        if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                            let _ = workspace_for_mouse.update(cx, |workspace, cx| {
                                workspace.end_context_menu_suppression(cx);
                            });
                        }
                    });
                    let workspace_for_key = workspace;
                    window.on_key_event(move |event: &KeyDownEvent, phase, _window, cx| {
                        if phase == DispatchPhase::Capture && event.keystroke.key == "escape" {
                            let _ = workspace_for_key.update(cx, |workspace, cx| {
                                workspace.end_context_menu_suppression(cx);
                            });
                        }
                    });
                },
            )
            .absolute()
            .inset_0()
            .into_any_element()
        };

        // 壳层走界面字体（默认系统 UI）；终端/diff/代码块各自绑代码字体。
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(shell_surface)
            .font_family(settings::resolved_ui_font_family(
                cx.global::<Appearance>(),
            ))
            .drag_over::<ExternalPaths>(|style, _paths, _window, _cx| {
                style
                    .bg(ui_theme::tint(ui_theme::blue(), 0x28))
                    .border_2()
                    .border_color(rgb(ui_theme::blue()))
            })
            .on_drop::<ExternalPaths>(cx.listener(
                |this, paths: &ExternalPaths, _window, cx| {
                    this.open_paths(paths.paths(), cx);
                },
            ))
            // Popover/ContextMenu 的打开状态通常在当前鼠标事件的 deferred 阶段
            // 才落地；下一帧再同步一次插件 WebView 显隐，避免它盖住 GPUI 弹层。
            // 左右键都纳入，右键菜单没有统一的全局状态注册。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    // A context-menu item or an outside click is dispatched as a
                    // normal left press. Release the temporary WebView suppression
                    // before the next frame restores the active plugin panel.
                    this.end_context_menu_suppression(cx);
                    this.schedule_plugin_surface_sync(window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, window, cx| {
                    this.begin_context_menu_suppression(window, cx);
                    this.schedule_plugin_surface_sync(window, cx);
                }),
            )
            // 见 focus_handle 字段注释：非终端页面没有可聚焦的子元素时，靠这个把
            // window 的 focus 兜底钉在这层，保证下面的全局 on_key_down 收得到事件。
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Quit, _window, cx| {
                if this.quit_requested {
                    return;
                }
                this.show_quit_confirm = true;
                cx.notify();
            }))
            // Cmd+, / 应用菜单「设置…」：跟齿轮图标共用同一个独立设置窗口。
            .on_action(cx.listener(|this, _: &OpenSettings, _window, cx| {
                // 不动 nonce：窗口已开着就保持用户当前所在页，只是把它提到前台；
                // 但下次新开窗口得回到外观页，不能停在「检查更新…」跳过去的那页。
                this.settings_section = settings::SettingsSection::appearance();
                this.open_settings_window(cx);
            }))
            // Cmd+↑ / Cmd+↓：上/下一个会话（按侧栏视觉顺序）。
            .on_action(cx.listener(|this, _: &PrevSession, window, cx| {
                this.cycle_session(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextSession, window, cx| {
                this.cycle_session(1, window, cx);
            }))
            // 应用菜单「检查更新…」：顺手发起一次检查，再把设置窗口开到「更新」页看进度。
            .on_action(cx.listener(|this, _: &CheckForUpdate, _window, cx| {
                if this.update_status.can_check() || this.update_status.can_retry_recovery() {
                    this.check_or_recover_update(false, cx);
                }
                this.open_settings_section(settings::SettingsSection::maintenance_update(), cx);
            }))
            // 应用菜单「反馈问题…」：跳内部飞书反馈群。
            .on_action(cx.listener(|_this, _: &ReportIssue, _window, cx| {
                cx.open_url(settings::FEEDBACK_URL);
            }))
            // 文件内容视图右键菜单里的「发送选中内容到终端」，见 send_open_file_selection。
            .on_action(
                cx.listener(|this, _: &SendSelectionToTerminal, _window, cx| {
                    this.send_open_file_selection(cx);
                }),
            )
            // 左右侧边栏切换（全局 Action，焦点在子组件或输入框内也能响应）
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| {
                this.toggle_sidebar(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleToolPanel, window, cx| {
                this.toggle_tool_panel(window, cx);
            }))
            // 输入控件可能消费 Escape，先在捕获阶段关闭最上层 Workspace 弹层。
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    this.schedule_plugin_surface_sync(window, cx);
                    this.end_context_menu_suppression(cx);
                    if this.dismiss_workspace_overlay(window, cx) {
                        cx.stop_propagation();
                    }
                }
            }))
            // 全局快捷键：Cmd+K 面板 / Cmd+B 侧栏 / Cmd+[ ] 切当前会话内的 pane /
            // Cmd+1~9 跳到第 N 个会话（键位分工对齐 iTerm2）
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.schedule_plugin_surface_sync(window, cx);
                let ks = &ev.keystroke;
                // 文件树导航只在树本身拥有焦点时处理。此前这里只排除了搜索框和编辑器，
                // 终端 / ACP 输入等子视图的按键会继续冒泡，导致右侧树跟着移动。
                let file_search_active = this
                    .file_filter
                    .as_ref()
                    .is_some_and(|input| !input.read(cx).value().trim().is_empty());
                if this.files_view_visible()
                    && this.file_tree_open
                    && !file_search_active
                    && this.file_tree_focus_handle.is_focused(window)
                    && !ks.modifiers.platform
                    && !ks.modifiers.control
                {
                    match ks.key.as_str() {
                        "up" => {
                            this.file_tree_move_selection(-1, cx);
                            cx.stop_propagation();
                            return;
                        }
                        "down" => {
                            this.file_tree_move_selection(1, cx);
                            cx.stop_propagation();
                            return;
                        }
                        "left" => {
                            this.file_tree_key_left(cx);
                            cx.stop_propagation();
                            return;
                        }
                        "right" => {
                            this.file_tree_key_right(window, cx);
                            cx.stop_propagation();
                            return;
                        }
                        "enter" => {
                            this.file_tree_key_enter(window, cx);
                            cx.stop_propagation();
                            return;
                        }
                        _ => {}
                    }
                }
                // Git 页 F7 / Shift+F7：在改动块之间跳（对齐 JetBrains 的 next/previous
                // difference）。不带 Cmd，所以要赶在下面的 platform 判断之前处理。
                // diff 现在停靠 / 展开态都能内嵌显示，这个快捷键两种态都该生效。
                if (this.tool_panel_stage_active(tool_panel::ToolPanelTab::Git)
                    || (this.tool_panel_open
                        && this.tool_panel_tab == tool_panel::ToolPanelTab::Git))
                    && this.git_tab == GitTab::Changes
                    && ks.key == "f7"
                    && !ks.modifiers.platform
                {
                    this.jump_hunk(!ks.modifiers.shift, cx);
                    return;
                }
                // Esc：先关插件 Tab，再收掉 session 内的舞台覆盖页。
                if ks.key == "escape"
                    && this.palette.is_none()
                    && this.rename_target.is_none()
                    && !this.show_quit_confirm
                    && this.delete_worktree_target.is_none()
                    && this.close_project_target.is_none()
                {
                    if this.active_tab().is_plugin() {
                        this.close_workspace_surface(window, cx);
                        return;
                    }
                    if this.active_tab().is_session() && this.stage_cover.is_some() {
                        this.set_stage_cover(None, window, cx);
                        return;
                    }
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
                    // Cmd+S：保存 Files 里打开的文件。Files 既可以展开到舞台，也可以
                    // 停靠在 Tool Panel；两种显示方式都必须支持保存。
                    "s" if this.files_view_visible() => this.save_open_file(cx),
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
            // 把铃铛和 Tool Panel 开关压成若隐若现的轮廓。
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .p(shell_padding)
                    .bg(shell_surface)
                    .child(
                        div()
                            .w_0()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .flex()
                            .relative()
                            .child(workspace_columns)
                            .children(sidebar_resize_handle)
                            .child(sidebar_resize_listener),
                    ),
            )
            // 浮层只留左侧栏开关。窗口开关在右侧共用顶栏，跟 tab 同一条 flex。
            .child(
                div()
                    .absolute()
                    // 跟主体内的共用顶栏使用同一个纵向原点；外壳内边距若以后
                    // 恢复，左侧开关与原生交通灯都会一起移动。
                    .top(shell_padding)
                    .left_0()
                    .right_0()
                    .h(workspace_frame::TOP_BAR_HEIGHT)
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .id("sidebar-toggle")
                            // 非全屏时红绿灯常驻，左边留 92px 让位；macOS 全屏会
                            // 隐藏红绿灯，这 92px 就全空了，按钮回到窗口左侧
                            // 18px 的统一视觉基准。纵向不再手算 top：由共用顶栏的
                            // flex 居中，UI 字号改变、按钮尺寸随 rem 缩放时也不会偏。
                            .ml(px(if window.is_fullscreen() { 18. } else { 92. }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size_6()
                            .rounded_full()
                            .cursor_pointer()
                            .text_color(rgb(ui_theme::text_mid()))
                            .hover(|s| s.bg(ui_theme::overlay(0x18)))
                            .child(
                                // 跟右侧两个开关同一套「细线 ↔ 实心色块」区分开合：
                                // 收起态用 bundled 的细线 PanelLeft，展开态换成
                                // panel-left-filled 实心色块，不再用带箭头的
                                // -open/-close 变体（对齐 Codex 工具栏风格）。
                                if self.sidebar_open {
                                    Icon::empty().path("smelt-icons/panel-left-filled.svg")
                                } else {
                                    Icon::new(IconName::PanelLeft)
                                }
                                .size_4(),
                            )
                            .tooltip(|window, cx| {
                                gpui_component::tooltip::Tooltip::new("切换左侧栏  ⌘B")
                                    .build(window, cx)
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.toggle_sidebar(window, cx);
                                }),
                            ),
                    )
                    // 中间点穿。窗口开关在右侧共用顶栏里，不在这条浮层。
                    .child(div().flex_1().min_w_0()),
            )
            // 确认框、命令面板和图片预览都留在当前 Workspace 的 GPUI 树内。
            .child(self.render_overlay_layers(window, cx))
            .child(context_menu_event_bridge)
    }
}

/// 居中空状态/加载中占位视图（供文件树、Git 提交列表、历史会话等在无数据或加载时复用）。
fn placeholder_view(text: &str, muted: Hsla) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .px_4()
        .child(
            div()
                .text_sm()
                .font_medium()
                .text_center()
                .text_color(muted)
                .child(text.to_string()),
        )
}

/// 旧存档没记 agent 种类时，从启动命令反推一把（命令里出现过 copilot / codex
/// 字样就归给它们）；认不出当 Claude——多 agent 之前的存档只可能是它。判断本体
/// 是 `ConversationAgentKind::from_command_loose`，跟 `smelt_remote_gateway::agent_from_launch`
/// （mobile 网关展示名）共用同一份「命令里出现哪家关键字就算哪家」逻辑。
fn acp_agent_from_cmd(cmd: &str) -> settings::ConversationAgentKind {
    settings::ConversationAgentKind::from_command_loose(cmd)
        .unwrap_or(settings::ConversationAgentKind::Claude)
}

/// 返回 shell 中第一个真正的程序 token，跳过 `VAR=value` 前缀和 `env`。
fn command_program(command: &str) -> Option<&str> {
    let mut tokens = command.split_whitespace();
    for token in tokens.by_ref() {
        if smelt_core::workspace_override::split_env_assignment(token).is_some() || token == "env" {
            continue;
        }
        return std::path::Path::new(token)
            .file_name()
            .and_then(|name| name.to_str());
    }
    None
}

fn command_matches_agent(command: &str, agent: settings::HistorySourceKind) -> bool {
    // 没有终端对应项的 agent（dsh）永远匹配不上任何启动项——不是"匹配所有"。
    agent
        .terminal()
        .is_some_and(|terminal| command_program(command) == Some(terminal.cli_program()))
}

/// 选择 CLI/TUI 启动命令：优先使用设置中的同 agent 启动项，缺失时回退出厂命令。
fn cli_launch_entry_for_agent(
    agent: settings::HistorySourceKind,
    cx: &App,
) -> settings::LaunchEntry {
    active_launch_entries(cx)
        .into_iter()
        .chain(default_launch_entries())
        .find(|entry| command_matches_agent(&entry.command, agent))
        .unwrap_or_else(|| settings::LaunchEntry {
            label: agent.label().to_string(),
            command: agent.id().to_string(),
            provider: Some(agent.id().to_string()),
        })
}

/// 单引号包裹 shell 参数；历史 session id 和 profile 路径都不能直接拼进命令。
fn shell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

/// 各家 CLI/TUI 的历史恢复语法不同，但都使用同一个 canonical session id。
///
/// 返回 None 表示这家压根没有可在终端里继续的 CLI（dsh 只有 ACP 桥），调用方
/// 该把入口藏起来，而不是拿一条拼不出来的命令去起终端。
fn cli_resume_command(
    agent: settings::HistorySourceKind,
    base_command: &str,
    resume_id: &str,
) -> Option<String> {
    let terminal = agent.terminal()?;
    let base = base_command.trim();
    let id = shell_quote(resume_id);
    let suffix = agent.cli_resume_syntax()?.suffix(&id);
    Some(if base.is_empty() {
        format!("{} {suffix}", terminal.cli_program())
    } else {
        format!("{base} {suffix}")
    })
}

/// 当前工作目录字符串。
fn current_dir() -> Option<String> {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
}

/// cwd → 侧栏项目分组显示名，统一取目录末段，不为特定 cwd 做额外分支。
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
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).ok()?, 16)
        {
            bytes.push(v);
            i += 3;
            continue;
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

/// 开一扇主工作台窗口（Workspace + Root 包装），返回其 weak 引用。
/// 首启和「点 Dock 图标重开」共用这一份：`Workspace::new` 本来就会从存档 + smeltd
/// 重新拼出会话布局，跟正常重启应用效果一致。
fn open_workspace_window(
    cx: &mut App,
    window_bg: WindowBackgroundAppearance,
) -> WeakEntity<Workspace> {
    let window_options = WindowOptions {
        // 透明标题栏：红绿灯浮在内容上，拖拽 / 双击最大化由自绘顶栏接管。
        // 纵向位置复用 gpui-component 与 34px TitleBar 配套的契约，再统一叠加
        // 外壳顶部内边距；不要在这里写死旧布局偏移。横向保留 Smelt 的 18px 基准。
        titlebar: Some(workspace_frame::titlebar_options(ui_theme::shell_padding())),
        // 透明/模糊背景（跟随外观设置；终端底色带 alpha 时桌面透出）。
        window_background: window_bg,
        ..Default::default()
    };
    let mut workspace = None;
    cx.open_window(window_options, |window, cx| {
        // 界面文字（侧边栏/标签页/状态栏等）用的都是 text_xs/text_sm 这类相对 rem
        // 单位。根据外观设置里的 ui_font_px（默认 16px）统一设置，全局跟着等比例缩放。
        // 终端内容本身的字号另由 terminal_view.rs 的 FONT_PX 控制，不受这个影响。
        let ui_font_px = cx.global::<Appearance>().ui_font_px;
        window.set_rem_size(px(ui_font_px as f32));
        let view = cx.new(|cx| Workspace::new(window, cx));
        workspace = Some(view.clone());
        // 顶层视图必须包一层 Root（组件库的主题/遮罩系统要求）。
        cx.new(|cx| Root::new(view, window, cx).bg(gpui::transparent_black()))
    })
    .expect("打开窗口失败");
    workspace.expect("回调里一定会设置 workspace").downgrade()
}

/// gpui-component 自带的图标集里没有「当前分支」这类专属 git 图标（只有品牌向的
/// github.svg），舞台头 git 状态胶囊之前借用 github 图标凑数，看着像「这是个 GitHub
/// 仓库」而不是「当前分支状态」。这里把 gpui-component 的图标资源跟 smelt 自带的一枚
/// git-branch.svg（Lucide 同款线条风格，跟其余图标一致）拼起来，多出的图标走
/// `smelt-icons/` 前缀，不会跟组件库以后新增的资源撞名。
/// agent 身份图标：资源名按稳定的存档 id 拼（`smelt-icons/agent-<id>.svg`），
/// 所以这里也按 id 建表，而不是一串手写的 `if path == ...`。
///
/// 那串 if 链是「加一家 agent 要改的第 N 个地方」里最容易漏的一个：漏了不报错、
/// 不 panic，只是侧栏那一格空着——dsh 接进来时正是这么漏的。查表 + 下面那条
/// 遍历所有 agent 的不变量测试，把「漏登记」从上线后才发现变成编译测试失败。
const AGENT_ICON_SVG: &[(&str, &[u8])] = &[
    ("claude", include_bytes!("../assets/icons/agent-claude.svg")),
    ("codex", include_bytes!("../assets/icons/agent-codex.svg")),
    (
        "copilot",
        include_bytes!("../assets/icons/agent-copilot.svg"),
    ),
    ("grok", include_bytes!("../assets/icons/agent-grok.svg")),
    (
        "antigravity",
        include_bytes!("../assets/icons/agent-antigravity.svg"),
    ),
    ("cursor", include_bytes!("../assets/icons/agent-cursor.svg")),
    (
        "opencode",
        include_bytes!("../assets/icons/agent-opencode.svg"),
    ),
    ("kiro", include_bytes!("../assets/icons/agent-kiro.svg")),
    ("pi", include_bytes!("../assets/icons/agent-pi.svg")),
    ("crush", include_bytes!("../assets/icons/agent-crush.svg")),
    ("dsh", include_bytes!("../assets/icons/agent-dsh.svg")),
];

/// `smelt-icons/agent-<id>.svg` → 图标字节；不是 agent 图标路径返回 None。
fn agent_icon_svg(path: &str) -> Option<&'static [u8]> {
    let id = path
        .strip_prefix("smelt-icons/agent-")?
        .strip_suffix(".svg")?;
    AGENT_ICON_SVG
        .iter()
        .find(|(name, _)| *name == id)
        .map(|(_, bytes)| *bytes)
}

struct SmeltAssets;

impl gpui::AssetSource for SmeltAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if let Some(bytes) = plugin_ui::plugin_agent_asset(path) {
            return Ok(Some(std::borrow::Cow::Owned(bytes)));
        }
        if let Some(bytes) = agent_icon_svg(path) {
            return Ok(Some(std::borrow::Cow::Borrowed(bytes)));
        }
        if path == "smelt-icons/git-branch.svg" {
            return Ok(Some(std::borrow::Cow::Borrowed(
                include_bytes!("../assets/icons/git-branch.svg").as_slice(),
            )));
        }
        if path == "smelt-icons/folder-plus.svg" {
            return Ok(Some(std::borrow::Cow::Borrowed(
                include_bytes!("../assets/icons/folder-plus.svg").as_slice(),
            )));
        }
        if path == "smelt-icons/git-commit.svg" {
            return Ok(Some(std::borrow::Cow::Borrowed(
                include_bytes!("../assets/icons/git-commit.svg").as_slice(),
            )));
        }
        if path == "smelt-icons/square-pen.svg" {
            return Ok(Some(std::borrow::Cow::Borrowed(
                include_bytes!("../assets/icons/square-pen.svg").as_slice(),
            )));
        }
        // panel-right-filled / panel-left-filled：bundled 的 panel-right /
        // panel-left 只是一根细分隔线，18px 图标下开合两态区分度太低；这两枚在
        // 分隔线的另一侧加了实心色块，开启态一眼可辨，跟 Codex 工具栏那种「大色块
        // 表示已展开」的简洁风格对齐，不用带箭头的 -open/-close 变体。
        if path == "smelt-icons/panel-right-filled.svg" {
            return Ok(Some(std::borrow::Cow::Borrowed(
                include_bytes!("../assets/icons/panel-right-filled.svg").as_slice(),
            )));
        }
        // panel-left-filled：左侧栏开关的开启态，跟 panel-right-filled 同一套
        // 「细线外框 + 实心色块」风格（右侧两个开关已统一，左侧这枚补上对齐）。
        if path == "smelt-icons/panel-left-filled.svg" {
            return Ok(Some(std::borrow::Cow::Borrowed(
                include_bytes!("../assets/icons/panel-left-filled.svg").as_slice(),
            )));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<gpui::SharedString>> {
        gpui_component_assets::Assets.list(path)
    }
}

fn schedule_obsolete_file_cleanup() {
    // 启动时只删已经没有任何读写路径的废弃文件；仍存在的 git worktree 要用户确认。
    let _ = thread::spawn(|| {
        let removed = storage_cleanup::remove_obsolete_files_at_startup();
        if removed > 0 {
            eprintln!("[storage] 启动时清理了 {removed} 个历史残留文件");
        }
    });
}

fn main() {
    if let Some(code) = terminal::maybe_run_install_app(std::env::args()) {
        std::process::exit(code);
    }
    if let Some(code) = cli::maybe_run(std::env::args()) {
        std::process::exit(code);
    }
    smelt_core::sqlite_state::enable_sqlite_state();
    // GUI 启动失败时用户通常看不到终端输出；把原始 panic 先落到统一日志，
    // 尤其能保留 GPUI 清理阶段二次 panic 之前的第一条错误。
    smelt_core::app_log::install_panic_hook("smelt");
    smelt_core::app_log::tee_stderr("smelt");
    schedule_obsolete_file_cleanup();
    // Finder / launchd 启动的 GUI 默认软上限常是 256；在创建窗口、socket 和终端
    // 线程之前先抬高，避免正常多会话也让更新检查等新连接触发 EMFILE。
    smelt_core::fd_limit::raise_fd_limit();
    // with_assets 注册图标资源，Sidebar 的 IconName svg 才能渲染；见上面 SmeltAssets。
    let app = gpui_platform::application().with_assets(SmeltAssets);
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
        // 回调。这里做好兜底：主窗口还活着就什么都不做，只有真的没了才重新开一扇。
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
        // 内嵌终端默认字体 Maple Mono NF（Regular/Bold，v7.9）。Ghostty 同款思路：
        // 默认字体自己带，不赌用户装没装——任何机器上默认字体族都能解析成功，
        // 杜绝"没装字体 → 测量/渲染各自 fallback 到不同字体 → 列宽错乱"。它是打过
        // Nerd Font 补丁的完整版，自带全部图标码位，兼任图标 fallback（用户在设置页
        // 自选的字体缺图标时落到它，见 terminal_view::terminal_font）。
        cx.text_system()
            .add_fonts(vec![
                std::borrow::Cow::Borrowed(
                    include_bytes!("../../../assets/fonts/MapleMono-NF-Regular.ttf").as_slice(),
                ),
                std::borrow::Cow::Borrowed(
                    include_bytes!("../../../assets/fonts/MapleMono-NF-Bold.ttf").as_slice(),
                ),
            ])
            .expect("加载内嵌字体失败");
        // 应用菜单栏：macOS 顶部「Smelt」菜单，含当前版本、「设置… ⌘,」+「退出 Smelt ⌘Q」
        // （跟齿轮图标一样开独立设置窗口，符合 mac 惯例——系统偏好设置一般都在这）。
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new("cmd-b", ToggleSidebar, None),
            KeyBinding::new("alt-cmd-b", ToggleToolPanel, None),
            // 会话上/下切换（跨项目按侧栏视觉顺序，到头循环）。用 cmd+方向键：
            // 比 cmd-shift-[ 好按。输入框聚焦时 Input 自己的 cmd-up/down（文首/
            // 文末）更具体，会盖过这两条；终端聚焦时才会切会话。
            // 原有的 cmd-[ / cmd-] 保持原样，不覆盖、不接管。
            KeyBinding::new("cmd-up", PrevSession, None),
            KeyBinding::new("cmd-down", NextSession, None),
            // readline / Grok TUI 行编辑：Ctrl+U 删到行首、Ctrl+K 删到行尾、
            // Ctrl+W 删词。绑在 Input context，所有输入框（ACP composer 等）生效。
            KeyBinding::new("ctrl-u", DeleteToBeginningOfLine, Some("Input")),
            KeyBinding::new("ctrl-k", DeleteToEndOfLine, Some("Input")),
            KeyBinding::new("ctrl-w", DeleteToPreviousWordStart, Some("Input")),
            // cmux：Ctrl+Backspace / Ctrl+Delete 也是删行（^U / ^K），不是删词。
            #[cfg(target_os = "macos")]
            KeyBinding::new("ctrl-backspace", DeleteToBeginningOfLine, Some("Input")),
            #[cfg(target_os = "macos")]
            KeyBinding::new("ctrl-delete", DeleteToEndOfLine, Some("Input")),
            // 把 Tab/Shift-Tab 从 gpui-component Root 的全局焦点跳转手里要回来，
            // 终端聚焦时改发给 shell（见 terminal_view.rs 里 TerminalTab 的注释）。
            KeyBinding::new("tab", terminal_view::TerminalTab, Some("Terminal")),
            KeyBinding::new(
                "shift-tab",
                terminal_view::TerminalBackTab,
                Some("Terminal"),
            ),
        ]);
        cx.set_menus(vec![
            Menu::new("Smelt").items([
                MenuItem::action(concat!("v", env!("CARGO_PKG_VERSION")), gpui::NoAction)
                    .disabled(true),
                MenuItem::Separator,
                MenuItem::action("检查更新…", CheckForUpdate),
                MenuItem::Separator,
                MenuItem::action("设置…", OpenSettings),
                MenuItem::action("反馈问题…", ReportIssue),
                MenuItem::Separator,
                MenuItem::action("退出 Smelt", Quit),
            ]),
        ]);

        // 外观设置：读盘设为全局单例，据此确定窗口背景外观（透明 / 模糊）。
        // 主题模式在建窗口之前落地，首帧就是对的，不会先闪一下深色再变浅。
        let appearance = load_appearance();
        let window_bg = appearance.window_bg();
        cx.set_global(appearance.clone());
        // 代码字体必须在 apply_theme_mode 之前落地：Theme.mono_font_family 从这里读。
        terminal_view::set_font_px(appearance.font_px);
        terminal_view::set_font_family(&appearance.font_family);
        settings::apply_theme_mode(appearance.theme_mode, cx);
        // 用户自选终端底色要在首帧前生效，并连同主题一起发布给守护（移动端配色）。
        settings::apply_bg_color(&appearance);
        cx.set_global(load_launch_config());
        cx.set_global(settings::load_update_settings());
        cx.set_global(worktree_inherit::load_settings());

        cx.set_global(settings::PluginEnablementState::load());

        // 原生通知/菜单栏桥必须先于任何 Attention producer 就绪。daemon 快照可能在
        // 首帧前到达；若先启动观察器再 setup，应用级消费者会正确 drain 事件，但
        // UNUserNotificationCenter 尚无 delegate/队列，通知仍会在启动竞态里丢失。
        status_item::setup(status_tx);

        // 状态通道：常驻订阅守护的 subscribe，维护 DaemonStates 全局单例，
        // Session::status/pane_status 靠它把"猜"换成"读事实"（见
        // docs/archive/state-channel-plan.md）。阻塞的 socket 读循环放专门的 OS 线程，
        // 断线/守护没起来就等一下重连；smol::channel 两头都能用（OS 线程用
        // try_send，GPUI 任务用 async recv），跟 terminal.rs 的 redraw_tx/rx
        // 是同一个搭桥模式。
        let daemon_states = DaemonStates::default();
        cx.set_global(daemon_states);
        cx.set_global(RemoteSessionCatalogGlobal::default());
        cx.set_global(HistoryTitles::default());
        let attention = AttentionGlobal::default();
        cx.set_global(attention);
        let current_ws_for_attention = current_ws.clone();
        let attention_delivery_subscription = cx.observe_global::<AttentionGlobal>(move |cx| {
            dispatch_pending_attention(&current_ws_for_attention, cx);
        });
        cx.set_global(AttentionDeliverySubscription {
            _subscription: attention_delivery_subscription,
        });
        let current_ws_for_daemon_states = current_ws.clone();
        let daemon_states_subscription = cx.observe_global::<DaemonStates>(move |cx| {
            let states = DaemonStates::drain_pending(cx);
            if let Some(ws) = current_ws_for_daemon_states.borrow().clone() {
                let _ = ws.update(cx, |ws, cx| {
                    for state in &states {
                        ws.handle_daemon_state_event(state, cx);
                    }
                    ws.sync_notification_surfaces(cx);
                    if ws.session_manager_open {
                        ws.refresh_session_manager(cx);
                    }
                    ws.maybe_flush_pending_daemon_upgrade(cx);
                });
            }
            schedule_state_refresh(&current_ws_for_daemon_states, cx);
        });
        cx.set_global(DaemonStatesSubscription {
            _subscription: daemon_states_subscription,
        });
        let agent_ui_config = settings::load_agent_host_state();
        let agent_hooks_enabled = agent_ui_config.agent_hooks_enabled;
        cx.set_global(agent_ui_config);
        cx.set_global(settings::AcpRuntimeState::default());
        // hook 配置和 helper 必须作为一个版本单元升级：先把 App 内最新版同步到稳定
        // managed 路径，再改写各 provider 的 hook 命令。失败时保留旧 helper，不阻塞 GUI。
        if let Err(error) = settings::sync_bundled_smelt_notify() {
            eprintln!("[workspace] 同步 smelt-notify 失败：{error}");
        }
        if let Err(error) = settings::sync_bundled_smelt_agent_mcp() {
            eprintln!("[workspace] 同步 smelt-agent-mcp 失败：{error}");
        }
        if let Err(error) = crate::cli::sync_control_skill() {
            eprintln!("[workspace] 同步 smelt skill 失败：{error}");
        }
        // 受管 bun 由 Smelt 代用户升级（启动时后台同步锁定版本）。下载约 25MB，
        // 不能挡首帧；与 smeltd 并发时靠 runtime 目录锁串行。首次就位后要重建 GUI
        // 的插件清单：refresh_once 可能已经在“没有 bun”的窗口里把脚本 tab 跳过了。
        let had_managed_bun = smelt_core::acp_conn::managed_bun_if_ready().is_some();
        let bun_sync = cx.background_executor().spawn(async move {
            smelt_core::acp_conn::sync_managed_bun(&|message| {
                smelt_core::app_log::info("bun", message)
            })
        });
        cx.spawn(async move |cx| match bun_sync.await {
            Ok(path) => {
                smelt_core::app_log::info("bun", &format!("受管 bun 已就绪：{}", path.display()));
                if !had_managed_bun {
                    cx.update(|cx| {
                        cx.set_global(settings::PluginEnablementState::load());
                        plugin_ui::refresh(cx);
                        cx.refresh_windows();
                    });
                }
            }
            Err(error) => {
                smelt_core::app_log::warn("bun", &format!("同步受管 bun 失败：{error}"));
            }
        })
        .detach();
        // 新安装和缺少开关的旧配置都默认开启托管 hooks；只有用户明确关闭时跳过。
        // 安装含 Codex app-server 信任握手和文件 IO，全部放后台，不能阻塞首帧。
        if agent_hooks_enabled {
            thread::spawn(|| {
                if let Err(error) = settings::install_agent_hooks() {
                    eprintln!("[workspace] 自动安装 Agent hooks 失败：{error}");
                }
            });
        }
        // 标记为「自动」的 dsh 自定义 provider：照端点上的模型刷新一遍目录。用户没
        // 手填模型就是把这份目录交给我们维护，中转网关上下线模型不该要他再来点一次。
        // 起 Node 子进程、几秒级，所以和 hooks 一样放后台；没标记任何 provider 时
        // 这个函数立刻返回，不起进程。
        thread::spawn(|| {
            for outcome in smelt_core::dsh_auto_models::refresh_auto_model_catalogs() {
                match outcome {
                    smelt_core::dsh_auto_models::AutoRefreshOutcome::Refreshed {
                        provider,
                        models,
                    } => {
                        eprintln!("[dsh] {provider} 的模型目录已刷新：{models} 个模型");
                    }
                    // 沿用上次那份目录，会话照常可用，所以只记一行，不打扰用户。
                    smelt_core::dsh_auto_models::AutoRefreshOutcome::Kept { provider, reason } => {
                        eprintln!("[dsh] {provider} 的模型目录保持不变：{reason}");
                    }
                    smelt_core::dsh_auto_models::AutoRefreshOutcome::Forgotten { provider } => {
                        eprintln!("[dsh] {provider} 已不在配置中，不再自动刷新其模型目录");
                    }
                }
            }
        });
        // Pi 侧同理，只是不必起运行时：直接问端点的 /models。
        thread::spawn(|| {
            for outcome in smelt_core::pi_auto_models::refresh_auto_model_catalogs() {
                match outcome {
                    smelt_core::pi_auto_models::AutoRefreshOutcome::Refreshed {
                        provider,
                        models,
                    } => {
                        eprintln!("[pi] {provider} 的模型目录已刷新：{models} 个模型");
                    }
                    smelt_core::pi_auto_models::AutoRefreshOutcome::Kept { provider, reason } => {
                        eprintln!("[pi] {provider} 的模型目录保持不变：{reason}");
                    }
                    smelt_core::pi_auto_models::AutoRefreshOutcome::Forgotten { provider } => {
                        eprintln!("[pi] {provider} 已不在配置中，不再自动刷新其模型目录");
                    }
                }
            }
        });
        let (daemon_state_tx, daemon_state_rx) =
            smol::channel::unbounded::<terminal::DaemonStateEvent>();
        thread::spawn(move || {
            let mut reconnect_attempt = 0u32;
            loop {
                let connected_at = Instant::now();
                terminal::subscribe_daemon_states_blocking(&daemon_state_tx);
                let _ = daemon_state_tx.try_send(terminal::DaemonStateEvent::Disconnected);
                // 短暂可用后断开（daemon 升级）从首档开始；连续失败则指数退避并封顶，
                // 避免 GUI 与网关在 daemon 未起时固定节拍反复争抢同一 socket。
                if connected_at.elapsed() >= Duration::from_secs(5) {
                    reconnect_attempt = 0;
                }
                thread::sleep(terminal::daemon_reconnect_backoff(reconnect_attempt));
                reconnect_attempt = reconnect_attempt.saturating_add(1);
            }
        });
        let current_ws_for_menu_republish = current_ws.clone();
        cx.spawn(async move |cx| {
            while let Ok(event) = daemon_state_rx.recv().await {
                cx.update(|cx| {
                    match &event {
                        terminal::DaemonStateEvent::Snapshot {
                            remote_sessions,
                            automations,
                            ..
                        } => {
                            // A fresh event snapshot starts a new daemon epoch, so it is allowed
                            // to reset a lower revision after a daemon restart.
                            RemoteSessionCatalogGlobal::reset_from_subscription(
                                remote_sessions.clone(),
                                cx,
                            );
                            // subscribe 的首帧意味着刚连上一个 daemon epoch。把桌面端
                            // 自己持有的最新菜单重新放进同一保存队列，覆盖 daemon 重启时
                            // 从旧 dedicated 文件恢复的副本；source revision 会过滤迟到重发。
                            if let Some(ws) = current_ws_for_menu_republish.borrow().clone() {
                                let _ = ws.update(cx, |ws, cx| ws.save_state(cx));
                            }
                            if let Some(automations) = automations {
                                apply_automation_projection(
                                    automations.clone(),
                                    automation_notifications::AutomationProjectionOrigin::InitialSnapshot,
                                    &current_ws_for_menu_republish,
                                    cx,
                                );
                            }
                        }
                        terminal::DaemonStateEvent::RemoteSessions(snapshot) => {
                            RemoteSessionCatalogGlobal::apply_incremental(snapshot.clone(), cx);
                        }
                        terminal::DaemonStateEvent::Automations(automations) => {
                            apply_automation_projection(
                                automations.clone(),
                                automation_notifications::AutomationProjectionOrigin::Incremental,
                                &current_ws_for_menu_republish,
                                cx,
                            );
                        }
                        terminal::DaemonStateEvent::Update(_) => {}
                        terminal::DaemonStateEvent::Removed { .. } => {}
                        terminal::DaemonStateEvent::WorkspaceMenu(_) => {}
                        terminal::DaemonStateEvent::Disconnected => {}
                    }
                    match event {
                        terminal::DaemonStateEvent::Snapshot { sessions: list, .. } => {
                            let initial_snapshot = !DaemonStates::is_primed(cx);
                            // attention 必须在 signal 前算完：先读旧镜像，写完
                            // 再 replace_all 唤醒观察器做 pane 副作用和合并刷新。
                            let previous = {
                                let map = cx.global::<DaemonStates>().map.lock().unwrap();
                                list.iter()
                                    .map(|state| map.get(&state.id).cloned())
                                    .collect::<Vec<_>>()
                            };
                            let now = Instant::now();
                            for (state, previous) in list.iter().zip(previous.iter()) {
                                if initial_snapshot {
                                    apply_daemon_attention_baseline(state, cx);
                                } else {
                                    apply_daemon_attention(previous.as_ref(), state, now, cx);
                                }
                            }
                            let stale_attention_ids = DaemonStates::replace_all(list, cx);
                            for id in stale_attention_ids {
                                AttentionGlobal::remove_session(&id, cx);
                            }
                        }
                        terminal::DaemonStateEvent::Update(s) => {
                            let previous = cx
                                .global::<DaemonStates>()
                                .map
                                .lock()
                                .unwrap()
                                .get(&s.id)
                                .cloned();
                            apply_daemon_attention(
                                previous.as_ref(),
                                &s,
                                Instant::now(),
                                cx,
                            );
                            DaemonStates::upsert(s, cx);
                        }
                        terminal::DaemonStateEvent::Removed { id } => {
                            DaemonStates::remove(&id, cx);
                            AttentionGlobal::remove_session(&id, cx);
                        }
                        terminal::DaemonStateEvent::RemoteSessions(_) => {}
                        terminal::DaemonStateEvent::WorkspaceMenu(_) => {}
                        terminal::DaemonStateEvent::Automations(_) => {}
                        terminal::DaemonStateEvent::Disconnected => {
                            DaemonStates::mark_subscription_disconnected(cx);
                        }
                    }
                });
            }
        })
        .detach();

        // 远程操作网关：只记「用户上次希望它开着」这个开关；真去问/让守护开的部分
        // 扔进后台任务——涉及连 unix socket、可能要等守护自己起来（最坏几秒），
        // 不能卡首帧渲染。settings/remote.rs 的「远程」设置页读 RemoteRuntimeState 展示。
        //
        // 网关和隧道**串在同一条后台任务**里对齐：先问守护现状（幂等 hydrate），
        // 没有再 start。以前两条 spawn 并行时，隧道可能先回 URL、token 还是空的，
        // UI 会拼出 `?token=` 的死链。
        let remote_config = settings::load_remote_config();
        cx.set_global(remote_config);
        cx.set_global(settings::RemoteRuntimeState::default());
        // ACP 会话不需要在退出时做任何事：agent 子进程现在是 smeltd 托管的
        // （见 smelt_core::acp_client），GUI 这边只是个薄客户端，Cmd+Q 直接杀
        // 整个 GUI 进程也不会带走子进程——这正是托管这一层要解决的问题。
        // iroh 隧道同理跑在 smeltd 里，GUI 退出不影响手机端连接。
        settings::spawn_remote_bootstrap(cx);
        // 之后由看门狗兜底：守护若被单独重启/升级过，远程会自动恢复，UI 也不会
        // 继续挂着一个早已失效的配对码。
        settings::spawn_remote_watchdog(cx);
        // 首启主窗口，记入 current_ws（reopen 回调 / URL 投递循环都靠它判断当前主窗口）。
        *current_ws.borrow_mut() = Some(open_workspace_window(cx, window_bg));

        // 退出前尽量提交最后一次 workspace 文档。订阅必须由 app global 持有，否则
        // app.run 的初始化闭包返回时会被 Subscription::drop 自动取消。
        let current_ws_quit = current_ws.clone();
        let quit_subscription = cx.on_app_quit(move |cx| {
            let flush_done = current_ws_quit
                .borrow()
                .as_ref()
                .and_then(|w| w.upgrade())
                .map(|ws| ws.update(cx, |ws, cx| ws.flush_ws_state_on_quit(cx)));
            async move {
                if let Some(done) = flush_done {
                    let _ = done.recv().await;
                }
            }
        });
        cx.set_global(AppQuitSubscription {
            _subscription: quit_subscription,
        });

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

        // 菜单栏图标/下拉菜单事件：主窗口还活着就前置 app（跳会话时顺带切过去）。
        // 没了就跟 on_reopen 一样重开；稳定的 daemon/run id 在恢复后仍可继续导航，
        // 只有菜单临时下标在新窗口里已经失去意义。
        cx.spawn(async move |cx| {
            while let Ok(event) = status_rx.recv().await {
                if matches!(
                    &event,
                    status_item::StatusItemEvent::SystemNotificationStateChanged
                ) {
                    status_item::sync_system_notifications();
                    let notification_status = status_item::system_notification_status();
                    for error in status_item::take_system_notification_errors() {
                        eprintln!(
                            "system notification ({:?}, {} pending): {error}",
                            notification_status.authorization, notification_status.pending_count
                        );
                    }
                    if let Some(ws) = current_ws_status.borrow().clone() {
                        let _ = ws.update(cx, |_, cx| cx.notify());
                    }
                    continue;
                }
                let existing_ws = current_ws_status.borrow().clone();
                let handled_by_existing_window = existing_ws.is_some_and(|ws| {
                    ws.update_in(cx, |ws, window, cx| {
                            match &event {
                                status_item::StatusItemEvent::JumpToSession(ix) => {
                                    if *ix < ws.sessions.len() {
                                        ws.goto_notification(*ix, None, window, cx);
                                    }
                                }
                                status_item::StatusItemEvent::JumpToDaemonSession(id) => {
                                    ws.request_notification_session_jump(
                                        id.clone(),
                                        window,
                                        cx,
                                    );
                                }
                                status_item::StatusItemEvent::JumpToAutomationRun {
                                    automation_id,
                                    run_id,
                                } => {
                                    ws.navigate_to_automation_run(
                                        automation_id.clone(),
                                        run_id.clone(),
                                        cx,
                                    );
                                }
                                status_item::StatusItemEvent::ActivateMain
                                | status_item::StatusItemEvent::SystemNotificationStateChanged => {}
                            }
                            // activateIgnoringOtherApps 负责前置应用；这里再显式激活主窗口，
                            // 让 macOS 切回它所在的原生全屏 Space，而不是只激活菜单栏。
                            window.activate_window();
                        })
                        .is_ok()
                });
                if handled_by_existing_window {
                    status_item::activate_app();
                } else {
                    cx.update(|cx| {
                        let window_bg = cx
                            .try_global::<Appearance>()
                            .map(|a| a.window_bg())
                            .unwrap_or(WindowBackgroundAppearance::Opaque);
                        let ws = open_workspace_window(cx, window_bg);
                        match &event {
                            status_item::StatusItemEvent::JumpToDaemonSession(id) => {
                                let _ = ws.update_in(cx, |ws, window, cx| {
                                    ws.request_notification_session_jump(
                                        id.clone(),
                                        window,
                                        cx,
                                    );
                                });
                            }
                            status_item::StatusItemEvent::JumpToAutomationRun {
                                automation_id,
                                run_id,
                            } => {
                                let _ = ws.update(cx, |ws, cx| {
                                    ws.navigate_to_automation_run(
                                        automation_id.clone(),
                                        run_id.clone(),
                                        cx,
                                    );
                                });
                            }
                            status_item::StatusItemEvent::ActivateMain
                            | status_item::StatusItemEvent::JumpToSession(_)
                            | status_item::StatusItemEvent::SystemNotificationStateChanged => {}
                        }
                        *current_ws_status.borrow_mut() = Some(ws);
                    });
                    status_item::activate_app();
                }
            }
        })
        .detach();
    });
}

#[cfg(test)]
mod main_tests;
