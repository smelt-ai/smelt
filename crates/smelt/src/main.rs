//! smelt 工作台 —— 基于 gpui-component 的桌面窗口。
//!
//! Workspace 管理多个会话（终端或 ACP 对话）：左侧会话列表切换 / 新建 / 关闭，
//! 舞台渲染当前活动会话。每个终端各自独立（PTY、IME、滚动、resize）。
//!
//! 运行： cargo run --bin smelt

// ACP 连接层已经搬进 smelt_model::acp_conn（给 smeltd 未来托管 ACP 会话铺路），
// 这里不再 mod acp;，用的地方直接引 smelt_model::acp_conn。
//
// acp_completion / acp_view / markdown_mermaid / ui_theme 同理都已
// 搬出主 crate（acp_view.rs 独立成 smelt-acp-view，其余几个是它和主 GUI 共用的
// UI 基建，搬进 smelt-ui / smelt-core，见各自文件头注释）。这里用同名 `use`
// 重新导出成原来的模块路径，全库既有的 `crate::ui_theme::x()` 之类写法不用
// 逐处改——跟 session_history.rs 对 claude_paths 的重导出是同一个套路。
pub(crate) use smelt_acp_view::acp_view;
pub(crate) use smelt_ui::markdown_mermaid;
pub(crate) use smelt_ui::ui_theme;

pub(crate) mod agents;
mod automation_notifications;
mod cli;
mod conversation_layout;
mod desktop_view;
mod dir_picker;
mod dock;
mod file_tree;
mod git_log;
mod git_log_view;
mod git_panel;
mod ide;
mod liquid_glass;
mod overlay;
mod plugins;
mod provider_quota;
mod resizable_split;
mod runtime_call;
mod runtime_events;
mod runtime_space;
mod runtimes;
mod tool_panel;
pub(crate) use runtime_space::ProjectRef;
pub(crate) use runtimes::RuntimeKey;
mod session_history;
mod session_list;
mod settings;
mod sidebar_order;
mod stage;
mod status_item;
mod storage_cleanup;
mod term_dock;
mod terminal;
mod terminal_view;
mod workspace_attention;
mod workspace_frame;
mod workspace_nav;
mod workspace_palette;
mod workspace_persist;
mod workspace_prepare;
mod workspace_sessions;
mod workspace_shell;
mod workspace_update;
mod workspace_view;

pub(crate) use sidebar_order::{indices_share_group, reorder_vec};
pub(crate) use workspace_nav::{CapabilitiesTab, WorkspaceNav, WorkspaceRoute};
#[cfg(test)]
pub(crate) use workspace_persist::{WsState, merge_restore_pending, persisted_active_position};
pub(crate) use workspace_persist::{
    load_workspace_ui, normalize_saved_sessions, pending_restore_session_ids,
    unproject_acp_when_remote_catalog_drops,
};

mod updater;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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
use gpui_component::resizable::ResizableState;
use gpui_component::slider::SliderState;
use gpui_component::*;
use terminal_view::TerminalView;

use file_tree::{DeleteFileTarget, OpenFile, SearchState};
use git_panel::{BranchList, GitDiff, GitStatusData, RepoInfo};
use settings::{Appearance, active_launch_entries, default_launch_entries, load_appearance};

const MIN_SIDEBAR_WIDTH: f32 = 240.0;
/// 左栏和右栏拖到这条以下松开就收起。停靠层最窄只到 100。
const DOCK_COLLAPSE_WIDTH: f32 = 160.0;
const MIN_WORKSPACE_CONTENT_WIDTH: f32 = 400.0;
const MIN_TOOL_PANEL_WIDTH: f32 = 320.0;
const DEFAULT_SIDEBAR_WIDTH: f32 = MIN_SIDEBAR_WIDTH;
const DEFAULT_TOOL_PANEL_WIDTH: f32 = MIN_TOOL_PANEL_WIDTH;

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

/// 主窗口关闭时关掉设置窗口。订阅必须由 app 持有，否则初始化闭包返回就取消了。
struct MainWindowClosedSubscription {
    _subscription: Subscription,
}

impl Global for MainWindowClosedSubscription {}

/// smeltd 远程会话目录在 GUI 内的只读投影。唯一的 daemon `event_subscribe` 循环写入它；
/// 各个 Workspace 只观察并渲染，不能重新读写历史 JSON 文件或自行建第二条订阅连接。
#[derive(Clone, Default)]
struct RemoteSessionCatalogGlobal {
    snapshot: Arc<Mutex<Option<smelt_model::session_control::RemoteSessionSnapshot>>>,
    generation: u64,
}

impl Global for RemoteSessionCatalogGlobal {}

fn accepts_remote_catalog_incremental(
    current: Option<&smelt_model::session_control::RemoteSessionSnapshot>,
    incoming: &smelt_model::session_control::RemoteSessionSnapshot,
) -> bool {
    // `revision = 0` 表示尚未建立投影水位：不能因首帧也是 0 就把后续完整替换
    // 永久丢掉。
    incoming.revision == 0 || current.is_none_or(|current| incoming.revision > current.revision)
}

fn recognized_remote_acp_sessions(
    records: Vec<smelt_model::session_control::RemoteAcpSession>,
) -> Vec<(
    smelt_model::session_control::RemoteAcpSession,
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
        snapshot: Option<smelt_model::session_control::RemoteSessionSnapshot>,
        cx: &mut App,
    ) {
        let state = cx.global::<Self>().snapshot.clone();
        *state.lock().unwrap() = snapshot;
        cx.update_global::<Self, _>(|global, _| {
            global.generation = global.generation.wrapping_add(1);
        });
    }

    fn apply_incremental(
        snapshot: smelt_model::session_control::RemoteSessionSnapshot,
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
// 工作区快捷键也走 Action：键位在 `app_key_bindings`，处理在 Workspace 根节点。
// 不绑 `None`——无 context 的键位和最内层同深度，后注册会盖过 Input。
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
        ToggleToolPanel,
        ToggleCommandPalette,
        CyclePanePrev,
        CyclePaneNext,
        SplitBeside,
        SplitBelow,
        ClosePane,
        SaveFile,
        ToggleDebugHud,
        NextHunk,
        PrevHunk,
        DismissWorkspace,
        FileTreeMoveUp,
        FileTreeMoveDown,
        FileTreeMoveLeft,
        FileTreeMoveRight,
        FileTreeActivate,
    ]
);

/// Cmd+1…9：按侧栏视觉顺序激活第 N 个会话（0 起）。
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = smelt, no_json)]
pub struct ActivateSession {
    pub index: usize,
}

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

/// 舞台覆盖页：盖在当前会话舞台上，不是一级导航（一级导航是 `WorkspaceRoute`）。
///
/// 存档 JSON 键仍是 `stage_override`；`ToolPanel` 的 wire 值仍是 `inspector`。
/// 实际内容由 `tool_panel_tab` 决定。旧的 `files`/`git`/`skills`/`history` 不再识别，
/// 读到即视为没有覆盖页。
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum StageCover {
    /// Tool Panel 全屏；持久化沿用旧 wire 名 `inspector`。
    #[serde(rename = "inspector", alias = "tool_panel")]
    ToolPanel,
    /// 未知覆盖页。读到即视为没有覆盖页。
    #[serde(other)]
    Unknown,
}

impl StageCover {
    /// 是否是 Tool Panel 的舞台全屏。
    pub(crate) fn is_tool_panel_cover(self) -> bool {
        matches!(self, Self::ToolPanel)
    }
}

/// 当前主工作台。窗口在不在只看 `window`：设置窗口强持有同一份 `Workspace`，
/// 主窗口关掉之后 `workspace` 仍能 `upgrade`。
#[derive(Clone)]
struct MainWindow {
    window: WindowHandle<Root>,
    workspace: WeakEntity<Workspace>,
}

fn current_workspace(current: &Rc<RefCell<Option<MainWindow>>>) -> Option<WeakEntity<Workspace>> {
    current.borrow().as_ref().map(|main| main.workspace.clone())
}

/// 窗口还在窗口表里。`is_active` 返回 `None` 表示已关，或这扇窗口正在被借用。
/// 不要在它自己的 update 里调用。
fn window_handle_is_open<V: 'static + Render>(window: &WindowHandle<V>, cx: &mut App) -> bool {
    window.is_active(cx).is_some()
}

fn close_settings_window(cx: &mut App) {
    let Some(handle) = cx
        .try_global::<settings::SettingsWindowHandle>()
        .map(|handle| handle.0)
    else {
        return;
    };
    let _ = handle.update(cx, |_, window, _| window.remove_window());
}

fn close_settings_if_main_window_closed(
    main_id: Option<WindowId>,
    closed_id: WindowId,
    cx: &mut App,
) {
    if main_id == Some(closed_id) {
        close_settings_window(cx);
    }
}

/// 主窗口还在就原样返回。已经关掉时，先关掉仍绑着旧 Workspace 的设置窗口，再开一扇。
/// 设置窗口若留着，新窗口会从磁盘装出第二份 Workspace，设置页仍改着旧的那份。
/// 不要在主窗口自己的 update 里调用：窗口被借用时 `is_active` 也是 `None`。
fn ensure_main_window(current: &Rc<RefCell<Option<MainWindow>>>, cx: &mut App) -> MainWindow {
    let existing = current.borrow().clone();
    if let Some(main) = existing
        && window_handle_is_open(&main.window, cx)
    {
        return main;
    }
    close_settings_window(cx);
    let window_bg = cx
        .try_global::<Appearance>()
        .map(|appearance| appearance.window_bg())
        .unwrap_or(WindowBackgroundAppearance::Opaque);
    let main = open_workspace_window(cx, window_bg);
    *current.borrow_mut() = Some(main.clone());
    main
}

fn apply_automation_projection(
    snapshot: smelt_model::automation::AutomationFile,
    origin: automation_notifications::AutomationProjectionOrigin,
    current_ws: &Rc<RefCell<Option<MainWindow>>>,
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

    // 自动化投影不在会话事实里（见 SessionFacts），这里直接通知。
    if let Some(workspace) = current_workspace(current_ws) {
        let _ = workspace.update(cx, |_, cx| cx.notify());
    }
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
    /// 点「+」之后才展开评论。拖选期间不往 Diff 里加注释，列表高度不变。
    diff_comment_open: bool,
    /// 「全部更改」里用户收起过的文件。其余文件默认展开。
    git_collapsed_diff_files: HashSet<String>,
    diff_split: bool,
    /// 变更栏正文。补丁原文没变时不调用 `set_files`，滚动和折叠才留得住。
    diff_view: Option<Entity<gpui_component::diff::DiffState>>,
    /// 当前装进 `diff_view` 的补丁原文。
    diff_view_patch: String,
    /// 评论卡锚在这个源码范围的末行。
    diff_comment_range: Option<gpui_component::diff::DiffLineRange>,
    _diff_view_sub: Option<Subscription>,
    /// `DiffFile::parse` 失败时的说明。git 报错、只有子模块这类非 diff 文本走这里。
    diff_parse_error: Option<String>,
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
            diff_comment_open: false,
            git_collapsed_diff_files: HashSet::new(),
            diff_split: false,
            diff_view: None,
            diff_view_patch: String::new(),
            diff_comment_range: None,
            _diff_view_sub: None,
            diff_parse_error: None,
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
        SessionRouteArchive {
            stage_cover: self
                .stage_cover
                .filter(|view| view.is_tool_panel_cover())
                .map(|_| StageCover::ToolPanel),
            tool_panel_tab: self.tool_panel_tab,
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
            // 不认识的覆盖页（含已删除的 files/git/skills/history）不占用舞台。
            stage_cover: match archive.stage_cover {
                Some(StageCover::ToolPanel) => Some(StageCover::ToolPanel),
                Some(StageCover::Unknown) | None => None,
            },
            tool_panel_tab: archive.tool_panel_tab,
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
            diff_comment_open: false,
            git_collapsed_diff_files: HashSet::new(),
            diff_split: archive.diff_split,
            diff_view: None,
            diff_view_patch: String::new(),
            diff_comment_range: None,
            _diff_view_sub: None,
            diff_parse_error: None,
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
    diff_comment_open: bool,
    git_collapsed_diff_files: HashSet<String>,
    diff_split: bool,
    diff_view: Option<Entity<gpui_component::diff::DiffState>>,
    diff_view_patch: String,
    diff_comment_range: Option<gpui_component::diff::DiffLineRange>,
    _diff_view_sub: Option<Subscription>,
    diff_parse_error: Option<String>,
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
            diff_comment_open: std::mem::take(&mut self.diff_comment_open),
            git_collapsed_diff_files: std::mem::take(&mut self.git_collapsed_diff_files),
            diff_split: std::mem::replace(&mut self.diff_split, defaults.diff_split),
            diff_view: self.diff_view.take(),
            diff_view_patch: std::mem::take(&mut self.diff_view_patch),
            diff_comment_range: self.diff_comment_range.take(),
            _diff_view_sub: self._diff_view_sub.take(),
            diff_parse_error: self.diff_parse_error.take(),
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
        self.diff_comment_open = ui.diff_comment_open;
        self.git_collapsed_diff_files = ui.git_collapsed_diff_files;
        self.diff_split = ui.diff_split;
        self.diff_view = ui.diff_view;
        self.diff_view_patch = ui.diff_view_patch;
        self.diff_comment_range = ui.diff_comment_range;
        self._diff_view_sub = ui._diff_view_sub;
        self.diff_parse_error = ui.diff_parse_error;
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
pub(crate) use smelt_model::agent_status::AgentStatus;

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
fn daemon_state_for_session_id(
    runtime: &RuntimeKey,
    session_id: &str,
    cx: &App,
) -> Option<terminal::DaemonSessionState> {
    DaemonStates::get(runtime, session_id, cx)
}

/// 取某个终端 pane 对应的守护状态；没有全局单例（比如极早期尚未注册）或那个
/// session id 还没有数据都返回 None。
fn daemon_state_for(view: &Entity<TerminalView>, cx: &App) -> Option<terminal::DaemonSessionState> {
    let view = view.read(cx);
    daemon_state_for_session_id(view.runtime().key(), view.session_id(), cx)
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
fn dispatch_pending_attention(current_ws: &Rc<RefCell<Option<MainWindow>>>, cx: &mut App) {
    let workspace = current_workspace(current_ws);
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
    let main_window = current_ws.borrow().as_ref().map(|main| main.window);
    let workspace_window_active = app_active
        && main_window
            .and_then(|window| window.is_active(cx))
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
        match smelt_model::attention::delivery_channel(
            enabled,
            app_active,
            workspace_window_active,
            is_current_view,
        ) {
            smelt_model::attention::DeliveryChannel::Suppress => {
                status_item::remove_system_notification(&notification.session_id);
            }
            smelt_model::attention::DeliveryChannel::System => {
                status_item::deliver_notification(
                    &notification.session_id,
                    &display_title,
                    &notification.message,
                );
            }
        }
    }
}

/// 一个会话的内容形态。Term 是第一通道（PTY 分屏树），Acp 是第二通道（结构化
/// 消息流，见 docs/archive/project-report.md 第 5 节）——后者不参与分屏，一会话一视图。
enum SessionKind {
    /// 终端会话 = 一棵独立的分屏 Dock + 会话内当前活动 pane（终端）。
    Term {
        dock: term_dock::TermDock,
        active: Entity<TerminalView>,
    },
    /// 结构化对话会话：单视图，不参与分屏（ACP / Pi RPC 等运行时都走这里）。
    Conversation(Entity<acp_view::AcpView>),
}

/// 侧栏每条对应一个会话；主区显示当前会话的内容（分屏树或 ACP 消息流）。
struct Session {
    /// 只用于把运行时 UI 快照稳定地绑到 session；拖拽排序和活动 pane 变化都不改它。
    ui_id: u64,
    /// 对话的稳定 id：跨进程、跨设备都认它。运行时目录和客户端存的界面状态都按它对上。
    conversation_id: String,
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
    fn single(
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Self {
        Self {
            ui_id: next_session_ui_id(),
            conversation_id: new_conversation_id(),
            kind: SessionKind::Term {
                dock: term_dock::TermDock::single(view.clone(), window, cx),
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
        smelt_model::session_control::is_product_conversation(
            self.automation_id.as_deref(),
            self.agent_definition_id.as_deref(),
            acp_session_id.as_deref(),
            self.cwd(cx).as_deref(),
        )
    }

    fn is_agent_conversation(&self, cx: &App) -> bool {
        smelt_model::session_control::is_agent_conversation(
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

    /// 终端会话的分屏 Dock；ACP 会话没有。
    fn term_dock(&self) -> Option<&term_dock::TermDock> {
        match &self.kind {
            SessionKind::Term { dock, .. } => Some(dock),
            SessionKind::Conversation(_) => None,
        }
    }

    fn term_dock_mut(&mut self) -> Option<&mut term_dock::TermDock> {
        match &mut self.kind {
            SessionKind::Term { dock, .. } => Some(dock),
            SessionKind::Conversation(_) => None,
        }
    }

    /// 会话内全部终端叶子，深度优先序（ACP 会话得到空列表）。
    fn term_leaves(&self) -> Vec<Entity<TerminalView>> {
        self.term_dock()
            .map(|dock| dock.leaves().to_vec())
            .unwrap_or_default()
    }

    /// ACP 会话的 agent 身份；终端会话返回 None。侧栏据此给出标签与图标，
    /// 避免经 `provider_kind` 的终端映射丢掉只有 ACP 的那几家。
    pub(crate) fn acp_kind(&self, cx: &App) -> Option<settings::ConversationAgentKind> {
        match &self.kind {
            SessionKind::Conversation(view) => Some(view.read(cx).agent_kind()),
            SessionKind::Term { .. } => None,
        }
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
                    smelt_model::session_title::acp_display_title(
                        title.as_deref(),
                        fallback,
                        cwd.as_deref(),
                    )
                }
            }),
        }
    }

    /// 会话所属的运行时：从它的视图读（终端叶子、ACP 视图各自记着），不另存一份。
    fn runtime(&self, cx: &App) -> crate::runtimes::Runtime {
        match &self.kind {
            SessionKind::Term { active, .. } => active.read(cx).runtime().clone(),
            SessionKind::Conversation(view) => view.read(cx).runtime().clone(),
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
        // 远程运行时连不上时，守护镜像停在断线那一刻，「运行中」不能当真：按运行时已丢算。
        if crate::runtimes::runtime_unavailable_label(self.runtime(cx).key(), cx).is_some() {
            return AgentStatus::Idle;
        }
        match &self.kind {
            // ACP 协议视图与守护镜像可能先后到达；两边都先给出完整 AgentStatus，
            // 再走同一优先级，避免派发或失败的同步窗口短暂变灰。
            SessionKind::Conversation(view) => {
                let v = view.read(cx);
                let daemon = daemon_state_for_session_id(v.runtime().key(), v.session_id(), cx)
                    .and_then(|state| AgentStatus::from_daemon_state(&state))
                    .unwrap_or(AgentStatus::Idle);
                AgentStatus::highest([daemon, v.agent_status()])
            }
            // 每个 pane 只读自己的结构化状态；父会话再做来源无关的聚合。
            SessionKind::Term { .. } => {
                let leaves = self.term_leaves();
                AgentStatus::highest(leaves.iter().map(|view| pane_status(view, cx)))
            }
        }
    }

    fn facts(&self, cx: &App) -> SessionFacts {
        SessionFacts {
            title: self.title(cx),
            status: self.status(cx),
            updated_at: self.effective_updated_at(cx),
            cwd: self.cwd(cx),
            session_id: live_daemon_session_id(self, cx),
            icon: self.agent_icon_id(cx),
            model: self
                .active_acp()
                .and_then(|view| view.read(cx).model_name()),
            panes: self
                .term_leaves()
                .iter()
                .map(|view| PaneFacts {
                    title: pane_title(view, cx),
                    status: pane_status(view, cx),
                    updated_at: daemon_state_for(view, cx)
                        .map(|state| state.updated_at)
                        .unwrap_or_default(),
                    icon: pane_provider_kind(view, cx).map(|kind| kind.id()),
                    progress: {
                        let view = view.read(cx);
                        DaemonStates::progress(view.runtime().key(), view.session_id(), cx)
                    },
                })
                .collect(),
        }
    }
}

/// 会话露在工作台上的事实：侧栏行和分屏子行、工具面板绑定的项目、舞台标题、
/// 最近对话。
///
/// 对话每来一条增量、守护每走一步都会推一次，这些事实却很少变。侧栏和工具面板
/// 跟着 Workspace 的通知整块重画，所以只有它们变了才通知（见
/// `Workspace::notify_if_session_facts_changed`）。对话正文由 AcpView 自己
/// 通知，终端画面走 TerminalView 自己的重绘通道，都不经过这里。
#[derive(Clone, PartialEq, Eq, Debug)]
struct SessionFacts {
    title: String,
    status: AgentStatus,
    /// 留到秒：侧栏按时间分组和最近对话都按它排序，不只显示到分钟。
    updated_at: u64,
    cwd: Option<String>,
    session_id: Option<String>,
    icon: Option<&'static str>,
    model: Option<String>,
    /// 终端会话的各个分屏，ACP 会话为空。
    panes: Vec<PaneFacts>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct PaneFacts {
    title: String,
    status: AgentStatus,
    updated_at: u64,
    icon: Option<&'static str>,
    progress: Option<smelt_client::daemon_state::TerminalProgress>,
}

/// 换上这次看到的事实，返回它们变没变。
fn replace_changed_facts(shown: &mut Vec<SessionFacts>, current: Vec<SessionFacts>) -> bool {
    if *shown == current {
        return false;
    }
    *shown = current;
    true
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
    /// 侧栏和机器面板里一台远程机器的名字，只在这台电脑上生效。
    Runtime(String),
    History {
        agent: settings::HistorySourceKind,
        profile_id: Option<String>,
        cwd: String,
        resume_id: String,
        current_title: String,
    },
}

/// 单个终端 pane 自动推导的标题。
///
/// 会话名只来自守护已经发布的那份（provider 标题或首条 prompt 兜底）。
/// 终端标题栏的 OSC 对所有 provider 都不是会话名；没有守护状态时回退到
/// 启动项或目录名。不看用户改的名字。
fn pane_auto_title(view: &Entity<TerminalView>, cx: &App) -> String {
    let t = view.read(cx);
    let session_title = daemon_state_for(view, cx).and_then(|state| state.title);
    smelt_model::session_title::display_title(
        None,
        session_title.as_deref(),
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
    smelt_model::agent_status::terminal_agent_status(daemon_state.as_ref())
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

/// 侧栏正在拖哪一类。身份跟 drop payload 一致：会话用 ui_id，项目用运行时加 root。
#[derive(Clone, Debug, PartialEq, Eq)]
enum SidebarDrag {
    Session(u64),
    Project(ProjectRef),
}

/// 单个会话（侧栏一条、一段对话）的持久化镜像。
///
/// 存档分两处（设计文档 D1）：对话 id、有哪些终端、ACP 会话这些进运行时的工作区目录，
/// 见 [`SessionStateWire`]；终端怎么分屏、当前看哪个、右侧 route 存在客户端库，读回来
/// 时由 `workspace_persist` 按对话 id 填进 `layout` / `active` / `route`。
#[derive(Clone)]
pub(crate) struct SessionState {
    /// 对话的稳定 id：新建时生成，之后不再变。客户端按它存这段对话的界面状态。
    id: String,
    /// 分屏树。刚从运行时读回来、客户端库还没填之前，是终端的默认摆法。
    layout: PaneState,
    /// 当前看的终端在 `layout` 里的位置（深度优先序）。
    active: usize,
    /// 最近一次会话内容变化的 unix 秒时间戳。
    last_updated_at: u64,
    custom_title: Option<String>,
    /// Some = ACP 消息流会话（layout 只是占位叶子）。恢复时建占位视图（会话进程不持久化，
    /// 见方案「已知不做」）。
    acp: Option<AcpSaved>,
    /// 右侧 route，存在客户端库。
    route: Option<SessionRouteArchive>,
}

/// 运行时工作区目录里的一段对话：对话 id、有哪些终端（按顺序）、ACP 会话。分屏和当前看
/// 哪个归客户端，不交给运行时。
#[derive(serde::Serialize, serde::Deserialize)]
struct SessionStateWire {
    #[serde(default)]
    id: String,
    #[serde(default = "unix_now_secs")]
    last_updated_at: u64,
    #[serde(default)]
    custom_title: Option<String>,
    #[serde(default)]
    terminals: Vec<TerminalSaved>,
    #[serde(default)]
    acp: Option<AcpSaved>,
}

/// 对话里的一个终端在运行时目录里的样子。
#[derive(serde::Serialize, serde::Deserialize)]
struct TerminalSaved {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    custom_title: Option<String>,
    #[serde(default)]
    launch_label: Option<String>,
    #[serde(default)]
    launch_cmd: Option<String>,
}

impl serde::Serialize for SessionState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // ACP 会话的 layout 只是占位叶子，不是终端。
        let terminals = if self.acp.is_some() {
            Vec::new()
        } else {
            conversation_layout::terminals_of(&self.layout)
                .into_iter()
                .filter_map(|leaf| match leaf {
                    PaneState::Leaf {
                        cwd,
                        id,
                        custom_title,
                        launch_label,
                        launch_cmd,
                    } => Some(TerminalSaved {
                        id,
                        cwd,
                        custom_title,
                        launch_label,
                        launch_cmd,
                    }),
                    _ => None,
                })
                .collect()
        };
        let wire = SessionStateWire {
            id: self.id.clone(),
            last_updated_at: self.last_updated_at,
            custom_title: self.custom_title.clone(),
            terminals,
            acp: self.acp.clone(),
        };
        <SessionStateWire as serde::Serialize>::serialize(&wire, serializer)
    }
}

impl<'de> serde::Deserialize<'de> for SessionState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = <SessionStateWire as serde::Deserialize>::deserialize(deserializer)?;
        let layout = match &wire.acp {
            Some(acp) => PaneState::Leaf {
                cwd: acp.cwd.clone(),
                id: None,
                custom_title: None,
                launch_label: None,
                launch_cmd: None,
            },
            None => conversation_layout::default_layout(
                wire.terminals
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
        Ok(Self {
            id: wire.id,
            layout,
            active: 0,
            last_updated_at: wire.last_updated_at,
            custom_title: wire.custom_title,
            acp: wire.acp,
            route: None,
        })
    }
}

/// ACP 会话的存档元数据。agent session store 是消息历史的唯一持久化来源；Smelt
/// 保存重新 load 所需的身份、启动信息和非敏感 ACP 配置。`entries` 仅用于读取旧版
/// workspace.json，新存档不再写出，避免和 agent transcript 形成两个数据源。
#[derive(Clone, serde::Serialize)]
struct AcpSaved {
    cwd: Option<String>,
    launch: smelt_model::agent_kind::ConversationLaunchSpec,
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
    /// 通用交互输入路由。agent/profile 只决定 ACP 执行器，不决定用户消息送往哪里。
    #[serde(default)]
    conversation_binding: smelt_model::conversation::ConversationBinding,
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
    launch: Option<smelt_model::agent_kind::ConversationLaunchSpec>,
    #[serde(default)]
    cmd: Option<String>,
    #[serde(default)]
    profile_id: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    agent_definition_id: Option<String>,
    #[serde(default, alias = "resume_session_id")]
    history_session_id: Option<agent_client_protocol::schema::v1::SessionId>,
    #[serde(default)]
    sid: Option<String>,
    #[serde(default)]
    refresh_launch_from_settings: Option<bool>,
    #[serde(default)]
    conversation_binding: Option<smelt_model::conversation::ConversationBinding>,
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
            smelt_model::agent_kind::ConversationLaunchSpec::from_command(
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
            conversation_binding,
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
/// 各子块尺寸，Tabs 存叠在同一格里的终端和当前显示的那个。结构 / 嵌套 / 方向 / 拖出来的
/// 比例都完整恢复。id 用于重开 GUI 时 reattach smeltd 里还活着的会话（旧存档无 id → 开新会话）。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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
        /// 各子块的像素尺寸 —— 存盘那一刻 Dock 上量到的真实值（含用户拖拽结果）。
        /// 旧存档没有这个字段 → 空 vec → 按均分，跟以前行为一致。
        #[serde(default)]
        sizes: Vec<f32>,
    },
    /// 叠在同一格里的几个终端（子节点都是叶子），`active` 是正在显示的那个。
    /// 只有一个终端的格子写成 `Leaf`，不写成一个元素的 `Tabs`。
    Tabs {
        active: usize,
        children: Vec<PaneState>,
    },
}

/// 关闭前要确认的东西。关掉就找不回来：终端里正在跑的命令、对话里正在跑的回合都会被终止。
#[derive(Clone)]
pub(crate) enum CloseConfirm {
    /// 关项目，连带关掉下面的全部会话。显示名只用来写文案，真正关哪个项目认 root。
    /// 空项目不走确认（无损），会话数恒 > 0。
    Project {
        label: String,
        project: ProjectRef,
        sessions: usize,
    },
    /// 关一个正在跑的会话。用 ui_id 认会话：弹窗开着时侧栏可能重排，下标会变。
    /// `pane` 有值时只关这一个分屏。
    Session {
        ui_id: u64,
        title: String,
        pane: Option<Entity<TerminalView>>,
    },
    /// 移除一台远程机器：这台电脑不再连它。
    Runtime { runtime_id: String, name: String },
}

/// 侧栏的一个项目分组。**身份是 root（路径），不是 label**——两个不同目录的末段名
/// 可能一模一样（`~/a/smelt` 和 `~/b/smelt`），拿显示名当 key 会把它们认成同一个项目：
/// 第二个连行都不显示、会话挂错组、关一个连带关掉另一个的会话。
pub(crate) struct ProjectGroup {
    /// 项目所在的运行时。根路径只在一个运行时里唯一。
    pub runtime: crate::runtimes::RuntimeKey,
    /// 项目根目录：在所属运行时里唯一。active_project / collapsed_projects / close_project
    /// 全用它（连同运行时）。
    pub root: String,
    /// 侧栏显示名。末段重名时往前补父目录段区分（`a · smelt` / `b · smelt`）。
    pub label: String,
    /// 组内会话在 `sessions` 里的下标（顺序 = 侧栏显示顺序）。
    pub sessions: Vec<usize>,
}

/// 一个运行时那段侧栏的分组。`members` 是属于这个运行时的会话下标（升序）；按状态、
/// 时间、不分组时只在它们里面排。
pub(crate) fn sidebar_groups(
    runtime: &crate::runtimes::RuntimeKey,
    grouping: SidebarGrouping,
    project_groups: Vec<ProjectGroup>,
    statuses: &[AgentStatus],
    last_updated_at: &[u64],
    members: &[usize],
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
            let sessions = members
                .iter()
                .copied()
                .filter(|ix| statuses.get(*ix) == Some(&status))
                .collect::<Vec<_>>();
            (!sessions.is_empty()).then(|| ProjectGroup {
                runtime: runtime.clone(),
                root: format!("__status_{}", status.rank()),
                label: label.to_string(),
                sessions,
            })
        })
        .collect(),
        SidebarGrouping::LastUpdated => {
            let now = Local::now();
            let mut buckets: [Vec<usize>; 4] = std::array::from_fn(|_| Vec::new());
            for &ix in members {
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
                        runtime: runtime.clone(),
                        root: format!("__last_updated_{bucket}"),
                        label: labels[bucket].to_string(),
                        sessions,
                    })
                })
                .collect()
        }
        SidebarGrouping::None => vec![ProjectGroup {
            runtime: runtime.clone(),
            root: "__all_sessions".into(),
            label: String::new(),
            sessions: members.to_vec(),
        }],
    }
}

/// 显示名撞车时往前补 `extra` 段父目录：`/a/b/smelt` + 1 → `b · smelt`。
/// base 是这一组本来的显示名（目录末段）。
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
fn disambiguate_labels(groups: &mut [ProjectGroup]) {
    let bases: Vec<String> = groups.iter().map(|group| group.label.clone()).collect();
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

/// 存档里一个会话的代表 cwd：ACP 取自身 cwd，终端取分屏树里第一个有 cwd 的叶子。
/// 旧存档迁移（反推项目列表）用。
fn session_state_cwd(s: &SessionState) -> Option<String> {
    if let Some(acp) = &s.acp {
        return acp.cwd.clone();
    }
    fn first_cwd(p: &PaneState) -> Option<String> {
        match p {
            PaneState::Leaf { cwd, .. } => cwd.clone(),
            PaneState::Split { children, .. } | PaneState::Tabs { children, .. } => {
                children.iter().find_map(first_cwd)
            }
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

pub(crate) fn pane_state_leaf_ids(pane: &PaneState) -> Vec<String> {
    match pane {
        PaneState::Leaf { id, .. } => id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(|id| vec![id.to_string()])
            .unwrap_or_default(),
        PaneState::Split { children, .. } | PaneState::Tabs { children, .. } => {
            children.iter().flat_map(pane_state_leaf_ids).collect()
        }
    }
}

/// 存档里一段会话的稳定身份：对话 id。活动项必须按这个认人，不能按数组下标——ACP 先
/// 恢复、终端后到时下标会对到别人。
pub(crate) fn session_state_persist_id(state: &SessionState) -> Option<String> {
    (!state.id.is_empty()).then(|| state.id.clone())
}

/// 新对话的稳定 id。
pub(crate) fn new_conversation_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// 会话当前在守护里的会话 id：ACP 是对话进程，终端是活动的那一格。侧栏、通知按它找进程；
/// 落盘认人用对话 id（[`Session::conversation_id`]），不用它。
pub(crate) fn live_daemon_session_id(session: &Session, cx: &App) -> Option<String> {
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
/// 恢复出来的路由不在工作台上（比如任务页）就别抢，用户看的是别处。
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

/// 新会话 id（uuid v4）：GUI 与 smeltd 之间的持久身份。
fn new_sid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Split 方向的可序列化镜像（gpui::Axis 无法直接序列化）。
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq)]
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

/// 阻塞：按 DFS 顺序在运行时上 spawn 一棵布局树的全部叶子（**只**在后台线程调用）。
fn spawn_layout_leaves(
    runtime: &crate::runtimes::Runtime,
    ps: &PaneState,
) -> Result<Vec<SpawnedLeaf>, String> {
    let mut out = Vec::new();
    spawn_layout_leaves_rec(runtime, ps, &mut out)?;
    Ok(out)
}

fn spawn_layout_leaves_rec(
    runtime: &crate::runtimes::Runtime,
    ps: &PaneState,
    out: &mut Vec<SpawnedLeaf>,
) -> Result<(), String> {
    match ps {
        PaneState::Leaf {
            cwd,
            id,
            custom_title,
            launch_label,
            launch_cmd,
        } => {
            let sid = id.clone().unwrap_or_else(new_sid);
            let terminal = terminal::Terminal::spawn(
                runtime,
                24,
                80,
                cwd.as_deref(),
                &sid,
                launch_cmd.as_deref(),
            )
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
                spawn_layout_leaves_rec(runtime, c, out)?;
            }
            Ok(())
        }
        // 标签组里只认叶子，搭 Dock 时也一样跳过（见 `term_dock::dock_layout_from_state`），
        // 两边的深度优先序才对得上。
        PaneState::Tabs { children, .. } => {
            for c in children
                .iter()
                .filter(|c| matches!(c, PaneState::Leaf { .. }))
            {
                spawn_layout_leaves_rec(runtime, c, out)?;
            }
            Ok(())
        }
    }
}

/// 用已 spawn 的叶子（DFS 序）搭回分屏 Dock 的布局；**只**在 UI 线程建 Entity。
/// 建好的终端按同样的顺序推进 `views`，调用方拿它认活动 pane。
fn rebuild_pane_ready(
    runtime: &crate::runtimes::Runtime,
    ps: &PaneState,
    leaves: &mut std::vec::IntoIter<SpawnedLeaf>,
    views: &mut Vec<Entity<TerminalView>>,
    cx: &mut Context<Workspace>,
) -> Option<gpui_component::dock::DockLayout> {
    term_dock::dock_layout_from_state(
        ps,
        &mut |_: &PaneState, cx: &mut Context<Workspace>| {
            let leaf = leaves.next()?;
            let view = cx.new(|cx| {
                let mut view = TerminalView::from_terminal(
                    cx,
                    runtime.clone(),
                    leaf.terminal,
                    leaf.cwd,
                    leaf.sid,
                    leaf.launch.as_deref(),
                    leaf.label.as_deref(),
                );
                view.set_custom_title(leaf.custom_title);
                view
            });
            views.push(view.clone());
            Some(view)
        },
        cx,
    )
}

/// 工作台根视图：多标签终端管理器。
struct Workspace {
    /// 所有会话；每个会话 = 一棵独立分屏树 + 会话内活动 pane。
    sessions: Vec<Session>,
    /// 当前活动会话的**运行时下标**（主区显示它、侧栏高亮它）。
    /// 跨进程身份在 `saved_active_session_id` / 各会话的对话 id，不能靠这个下标落盘认人。
    active_session: usize,
    /// 冷启动要恢复的活动会话（对话 id）。恢复完成前 `active_session` 还不是活列表下标。
    saved_active_session_id: Option<String>,
    /// 客户端库里记着、还没认领的对话界面状态（分屏、当前终端、右侧 route）。运行时的目录
    /// 到了，按对话 id 填回待恢复的会话；目录还没到的运行时，保存时原样写回。
    saved_conversation_ui:
        HashMap<workspace_persist::ConversationKey, workspace_persist::SavedConversationUi>,
    /// 一级 Tab：智能体 / 自动化 / 任务 / 会话。
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
    /// 已见过的远程目录标题。目录标题变了才覆盖侧栏，避免把本地改名冲掉。
    catalog_titles: HashMap<String, String>,
    /// 文件树里已展开的文件夹绝对路径。
    /// 目录列表缓存（绝对路径 → 已排序过滤的直接子项 (名, 是否目录)）。后台读盘填充，
    /// render 只读；此前 file_tree 在 render 里同步 fs::read_dir，大目录会像 git
    /// status 那样掉帧，这里改用同款「后台刷新 + 缓存 + render 只读」模式修复。
    dir_cache: file_tree::DirCache,
    /// 正在后台读取的目录（防重复并发 spawn）。
    dir_inflight: HashSet<String>,
    /// 目录列表失效代数。删除发生在读取期间时，旧回包不得再把缓存标成新鲜。
    dir_generation: HashMap<String, u64>,
    /// 文件树键盘选中的条目绝对路径（↑↓ 导航用）。
    /// 打开文件后要 reveal 的路径：祖先目录缓存齐了再 scroll_to_item。
    file_tree_pending_reveal: Option<String>,
    /// 当前在文件树里打开查看的文件（含预高亮的行数据）。
    /// ACP 消息图片的窗口级预览。放在 Workspace 而非 AcpView，遮罩才能覆盖 Session
    /// Panel、Tool Panel 和输入区，且不受会话面板裁剪。解码/降采样走
    /// `smelt_ui::image` 按图片 id 缓存：渲染时查 `cached()` 取当前这张图的结果，
    /// 不用再单独持有解码后的 RenderImage 字段。
    acp_image_preview: Option<Arc<gpui::Image>>,
    /// 打开文件的自增序号：后台高亮完成时用它判断结果是否已过期（切了别的文件）。
    file_gen: u64,
    /// Markdown 预览刷新世代。连续按键只保留最后一次，停手后再解析。
    markdown_preview_epoch: u64,
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

    /// 正在确认删除的分支：(仓库根, 分支名, 是否远端分支)。
    delete_branch_target: Option<(String, String, bool)>,
    /// 日志页三栏（分支树 / 提交列表 / 详情）的拖拽状态。窗口窄时靠它腾地方。
    git_log_resize: Entity<ResizableState>,
    /// 左侧会话栏是否展开。
    sidebar_open: bool,
    sidebar_w: f32,
    /// 工作台三栏。开合和宽度仍由上面的字段决定，渲染前对齐进这棵 Dock。
    shell_dock: Entity<gpui_component::dock::DockArea>,
    /// 上次写进 Dock 的宽度。和 Dock 上的实际宽度不一致时，是用户拖了分隔条。
    shell_applied_sidebar: f32,
    shell_applied_tool: f32,
    /// 分隔条拖动改过宽度，松手时落盘。
    shell_needs_save: bool,
    /// 左键还按着。这段时间不把 Dock 宽度推回去，免得和拖动对着干。
    shell_pointer_down: bool,
    /// 这次按下时的左右栏宽度。拖过收起线再松开就回到这个宽度，不把拖动中的夹紧值存下来。
    shell_gesture_sidebar_w: Option<f32>,
    shell_gesture_tool_w: Option<f32>,
    /// 交互式 diff：选中待评论的行号集合（对应 GitDiff.lines 下标），换文件/重开 diff 时清空。
    /// 交互式 diff 的评论输入框（懒创建，随 Git 视图渲染出待发送的 diff 时创建）。
    diff_comment_input: Option<Entity<gpui_component::input::TextareaState>>,
    /// 变更栏 diff 正文。和右侧文件列表分开缓存：列表滚动不再重排整份 diff。
    diff_body: Option<Entity<git_panel::GitDiffBody>>,
    /// Git 视图的 commit message 输入框，按**仓库根**分开。
    ///
    /// 多仓工作区里提交信息天然属于某一个仓库：共用一个框会把写给子仓的
    /// 描述带到父仓去。懒建，仓库从发现结果里消失时跟着回收。
    commit_msg_inputs: HashMap<String, Entity<gpui_component::input::TextareaState>>,
    /// 提交/推送失败的原因，按**仓库根**分开，直接显示在该仓库的提交框下方。
    ///
    /// 提交失败必须在用户正盯着的地方说清楚：它是对一次明确点击的直接回应。
    /// 走系统通知只会弹一条，通知被静音或错过时，界面上
    /// 「文件还在暂存区、什么都没发生」和成功毫无区别——pre-commit hook 因
    /// PATH 缺失而失败正是这样被当成「按钮没反应」。
    /// 下次对同一仓库发起提交/推送时清掉，不留过期错误。
    commit_errors: HashMap<String, String>,
    /// 关闭前的二次确认弹窗。Some = 弹窗开着。
    close_confirm: Option<CloseConfirm>,
    /// 远程桌面页正在看的那台运行时（`WorkspaceRoute::Desktop`）。离开这一页就丢掉，
    /// 网页视图和连接随之关掉，不在后台推画面。
    desktop: Option<Entity<desktop_view::DesktopView>>,
    /// 已打开的项目根目录（有序，就是侧栏分组的骨架）。项目是**独立于会话**的实体：
    /// 活动项目的 **root 路径**（不是显示名——同名目录是两个项目，见 ProjectGroup）：
    /// 会话列表里高亮哪一组、顶栏显示谁、「+对话/+终端」新建到哪个 cwd。None 或该组
    /// 已消失时回退到活动会话所在组（见 active_project_root）。
    active_project: Option<ProjectRef>,
    /// 本机运行时在侧栏里的那一份：项目、置顶、折叠、目录同步、冷启动恢复。
    local: crate::runtime_space::RuntimeSpace,
    /// 添加过的远程运行时各一份，顺序同运行时登记表。
    remote_spaces: Vec<crate::runtime_space::RuntimeSpace>,
    /// 侧栏「对话」里被折叠起来的智能体（存定义 id）。
    collapsed_agents: HashSet<String>,
    /// 会话列表分组方式（默认按项目）。
    sidebar_grouping: SidebarGrouping,
    /// 按项目分组时是否把无会话的项目从侧栏藏起来（固定的 / 当前项目除外）。
    sidebar_hide_empty_projects: bool,
    /// 侧栏「最近对话」是否展开全部。只在本次运行内记住。
    sidebar_recent_conversations_expanded: bool,
    /// 会话拖拽悬停中的插入位置：(目标会话 ui_id, 插它前面?)。
    sess_drop_hint: Option<(u64, bool)>,
    /// 项目拖拽悬停中的插入位置：(目标项目 root, 插它前面?)。
    proj_drop_hint: Option<(ProjectRef, bool)>,
    /// 当前正在拖的侧栏项。用来压暗原位、以及只在「我们自己的拖」时改抓握光标。
    sidebar_drag: Option<SidebarDrag>,
    /// 侧栏拖拽排序生效一次就加一。各行据此在下一帧从旧位置滑到新位置，见 `flip_slot`。
    sidebar_reorder_generation: u64,
    /// 上一次比较时各会话露在工作台上的事实，见 `notify_if_session_facts_changed`。
    shown_session_facts: Vec<SessionFacts>,
    /// 最近一次拖拽排序里被拖的那一项，和松手时拖拽浮层的高度。它从这里落到新位置。
    sidebar_drop_origin: Option<(SidebarDrag, Pixels)>,
    /// 侧栏会话列表的滚动位置；拖到顶底边缘时靠它自动滚。
    sidebar_scroll: ScrollHandle,
    sidebar_auto_scroll: gpui_component::scroll::AutoScroll,
    /// 已发现 IDE 的按需缓存。没有项目目录时完全不触发；首次打开 IDE 菜单才后台
    /// 填充，之后菜单只读这里的快照。
    ide_catalog: ide::IdeCatalog,
    /// 首次扫描尚未完成时打开的 IDE 菜单。弱引用不会把已经关闭的弹层留在内存里；
    /// 完成后统一原地替换其加载项。
    ide_popup_waiters: Vec<stage::IdePopupWaiter>,
    /// 命令面板（Cmd+K）；None 表示未打开。查询和选中在 CommandState 里。
    palette: Option<Entity<gpui_component::command::CommandState>>,
    /// 变更列表的滚动位置。列表里混着仓库行、提交框和文件行，高度不一，
    /// 用不了等高虚拟列表，所以是常规滚动容器。
    git_list_scroll: VirtualListScrollHandle,
    /// 文件树虚拟列表的滚动句柄。行高由文件树自己给定，见 file_tree。
    file_tree_scroll: VirtualListScrollHandle,
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
    /// 进行中的文件搜索。换查询或清空输入时替换它，取消还没 poll 到的那一轮。
    search_task: Option<Task<()>>,
    /// 正在跑的搜索在每个文件边界读这个标志。drop `search_task` 停不掉已经进入
    /// 同步遍历的那一次。
    search_cancel: Option<Arc<AtomicBool>>,

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
    /// 运行时给的 DeepSeek Harness 配置快照。渲染只读它。
    dsh_config: Option<smelt_model::dsh_service::DshConfig>,
    dsh_config_error: Option<String>,
    /// 读取的代数：晚回来的旧读取不能盖掉更新的快照。
    dsh_config_generation: u64,
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
    /// 正在登录的 MCP 服务器名。登录跑在后台子进程里，期间禁用同一行的按钮。
    pi_mcp_login: Option<String>,
    /// 等用户确认移除的 MCP 服务器名。移除会删掉用户手写的配置，所以要两步。
    pi_mcp_pending_remove: Option<String>,
    /// MCP 配置操作（添加、启停、移除、登录）的最近一次错误。
    pi_mcp_error: Option<String>,
    /// 运行时给的 Pi 配置快照（模型、MCP、插件……）。渲染只读它；打开设置窗口、进
    /// 能力库、开智能体编辑器时刷新，修改由运行时回带新快照。
    pi_config: Option<smelt_model::pi_service::PiConfig>,
    /// 最近一次读取失败的原因。
    pi_config_error: Option<String>,
    /// 读取的代数：晚回来的旧读取不能盖掉更新的快照。
    pi_config_generation: u64,
    /// 能力库在配哪台机器，以及那台机器（不是本机时）的快照。
    pi_library: settings::PiLibrary,
    /// 交互式原生 Pi 模型表单错误。
    pi_model_editor_error: Option<String>,
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
    /// 上次应用到窗口的原生背景外观。外观变化时才再发给 AppKit。
    applied_window_bg: Option<WindowBackgroundAppearance>,
    /// 上次应用到原生 NSWindow 的整体透明度，避免重复发送 AppKit 消息。
    applied_window_opacity: Option<f32>,
    /// 上次安装/更新的液态玻璃材质档位。配置变化时才重新 setStyle。
    applied_glass_style: Option<liquid_glass::GlassStyle>,
    /// 上次应用到窗口的界面基准字号（rem_size）。
    applied_ui_font_px: Option<u32>,
    /// 主窗口。没有 Window 的变更用它在本轮更新结束后、绘制前刷新可见数据。
    window_handle: AnyWindowHandle,
    /// 已经排队一次可见数据刷新。同一轮里的多次变更合并成一次。
    visible_refresh_scheduled: bool,
    /// 正在刷新可见数据。这段里的切会话不再排队第二次。
    visible_refresh_running: bool,
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
    /// 单文件 git add/reset 的即时 UI 状态。进程是否在跑看 `git_writes`，
    /// 这里只在进程返回后保留到权威 status 回包，避免复选框先回弹再延迟移动分组。
    git_index_pending: HashMap<(String, String), git_panel::PendingGitIndexOp>,
    /// 分支列表缓存（root → (取得时刻, 数据)），Git 页头部分支切换下拉用；同
    /// git_status 一套只在 Git 页打开时后台刷新。
    branches: HashMap<String, (Instant, BranchList)>,
    /// 分支列表失效代数。删除分支发生在 `for-each-ref` 期间时，旧回包不得再写回。
    branches_generation: HashMap<String, u64>,
    /// 正在后台刷新分支列表的 root（防重复并发 spawn）。
    branches_inflight: HashSet<String>,
    /// 已经请运行时监听的项目根，见 ensure_git_watch。运行时重连后清空重新登记。
    watched_projects: HashSet<String>,
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
    session_list: HashMap<String, (Instant, session_history::HistoryListSnapshot)>,
    /// 正在后台扫描历史会话列表的 key（同上 `"{agent_id}:{cwd}"`，防重复并发 spawn）。
    session_list_inflight: HashSet<String>,
    /// 扫描期间发生删除等变更的 key；旧扫描完成后会自动再扫一次，避免旧快照回写。
    session_list_invalidated: HashSet<String>,
    /// 历史会话绑定的智能体（同上 key → `resume_id` → 智能体 id）。跟列表在同一次后台
    /// 扫描里读出：它要逐条查 SQLite，放进 render 会随每帧重绘重复读库。
    history_agent_bindings: HashMap<String, Rc<HashMap<String, String>>>,
    /// 历史会话标题搜索框；只过滤已加载列表，不触发 transcript 重扫。
    history_filter: Option<Entity<gpui_component::input::InputState>>,
    _history_filter_sub: Option<Subscription>,
    /// 用户在 agent 尚未返回 resume id 前改名的 ACP 会话。只记录本次显式改名，
    /// 因此不会把旧 workspace.json 里的 custom_title 迁移进历史元数据仓库。
    pending_history_title_persist: HashSet<u64>,
    /// 上一次把用户命名覆盖层读进 `HistoryTitles` 缓存的时刻。侧栏每帧都要用它，
    /// 但改名本身会写穿缓存，所以这里只是低频兜底（覆盖移动端/历史页的远端改名）。
    /// 缓存变了由 `_history_titles_sub` 重绘，这里不直接 notify。
    history_titles_at: Option<Instant>,
    /// 是否已有一次后台重读在路上，防止逐帧重复 spawn。
    history_titles_inflight: bool,
    /// 当前选中查看的历史会话；None 表示未选。
    session_detail: Option<session_history::OpenedHistory>,
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
    /// 调试 HUD 开关（Cmd+Shift+F 切换），见 `overlay::debug_hud`。
    debug_hud: bool,
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
    /// 能力库正文。只在能力库路由可见时存在，见 `sync_capabilities_view`。
    capabilities_view: Option<Entity<settings::SettingsContentView>>,
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
    /// 仓库身份缓存（cwd → 当前分支）：侧栏项目行和会话行显示分支名靠它。None = 探测过
    /// 但不是 git 仓库（比如临时终端落脚的 $HOME），不会重复无意义地重试。
    repo_info: HashMap<String, (Instant, Option<RepoInfo>)>,
    /// 正在后台探测仓库身份、避免重复起进程的 cwd 集合。
    repo_info_inflight: HashSet<String>,
    /// 运行时目录选择器（打开项目、绑定目录、导入插件）。
    dir_picker: Option<dir_picker::DirPickerState>,
    /// 正在确认丢弃的 diff 块：(仓库根, hunk 下标)。丢弃直接改工作区文件且不进
    /// reflog，找不回来，所以必须过一道确认。
    discard_hunk_target: Option<(String, usize)>,
    /// 正在确认丢弃整个文件的改动：(仓库根, 用户点丢弃时看到的那条改动)。未跟踪
    /// 文件是直接删盘，比 restore 更狠，文案要分开写。
    discard_file_target: Option<(String, git_panel::GitSeenChange)>,
    /// 「丢弃全部改动」确认弹窗（Some = 弹窗开着）：(仓库根, 用户点丢弃时看到的整份
    /// 改动列表)。见 git_panel 模块。
    discard_all_target: Option<(String, Vec<git_panel::GitSeenChange>)>,
    /// 每个仓库正在跑的 git 写入。键是这次命令的仓库根。
    ///
    /// `index.lock` 属于这一个仓库，所以另一个仓库可以同时写。同一个根上的
    /// 提交、推送、获取、暂存都进这一张表，由 `spawn_git` 占用和释放。
    git_writes: HashMap<String, git_panel::GitWrite>,
    /// 额度刷新循环的手动信号通道。额度状态本身不在 Workspace，而在
    /// `ProviderStatus` 全局单例：侧栏和对话模型菜单都订阅它。
    provider_quota_refresh_tx: Option<smol::channel::Sender<provider_quota::QuotaTarget>>,

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
    /// 远程目录来自 app 级唯一 daemon 订阅；随 Workspace 释放，避免创建第二条 socket
    /// 订阅或后台轮询线程。
    _remote_sessions_sub: Option<Subscription>,
    /// 远程运行时增删时同步侧栏的空间。
    _runtime_hub_sub: Option<Subscription>,
    /// 自动化目录读 AgentHostState；不观察的话，投影写进 global 后当前页不会重绘。
    _agent_host_sub: Option<Subscription>,
    /// provider 账户状态（额度）变化时重绘侧栏额度行。
    _provider_status_sub: Option<Subscription>,
    /// 侧栏 pane 标题在 render 里读 `HistoryTitles`。读 Global 不会自动订阅，
    /// 后台覆盖层刷新、另一扇窗口和手机上的改名都靠这里把工作台标脏。
    _history_titles_sub: Option<Subscription>,
    /// 外观全局变化。不能放进 `settings_subs`：设置滑块重建时会清掉那个 vec。
    _appearance_sub: Option<Subscription>,
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
    /// 真正根节点，而 Workspace 的键位 context 挂在 Root 组件之下、并非那个根节点，
    /// 于是匹配不到，表现为"切到别的页面后快捷键全部失灵"。切到非终端页面时把
    /// focus 显式认领到这个句柄上，保证 Workspace context 始终在 dispatch 路径上。
    focus_handle: FocusHandle,
    /// 文件树的专属键盘焦点。文件树的方向键导航只在这个焦点生效，不能让终端、
    /// ACP 输入框或其它控件冒泡出来的按键误操作右侧面板。
    file_tree_focus_handle: FocusHandle,
    /// 确认弹窗共用的焦点陷阱。`Dialog::new` 每次都会新造一个句柄，不能用那个。
    modal_focus: FocusHandle,
    /// 主窗口弹窗已经记下打开前的焦点。嵌套弹窗不再覆盖。
    main_modal_guard: bool,
    main_modal_restore: Option<WeakFocusHandle>,
    /// 这一帧的打开动作会自己把焦点放进输入框，sync 不要再抢回对话框。
    main_modal_hold: bool,
    /// 当前最上层确认框的层级。变了才把焦点交给新的那一层。
    main_modal_rank: Option<u8>,
    /// 设置窗口的弹窗单独记一份。主窗口的 prepare 不能把焦点抢到另一扇窗。
    settings_modal_guard: bool,
    settings_modal_restore: Option<WeakFocusHandle>,
    settings_modal_rank: Option<u8>,
    /// 主窗口关闭期间点击系统通知时，会话还在异步恢复。先保留稳定 sid，目标装回
    /// 列表后再消费，不能退化成只打开一扇空窗口。
    pending_notification_session_id: Option<String>,
    /// 手机在冷恢复把对话占位装回列表之前就要看的对话。装回后照常唤醒，
    /// 恢复跑完仍找不到的就丢掉——daemon 那边会超时告诉手机。
    pending_conversation_wakes: Vec<String>,
    /// 冷恢复后的「回到上次那段智能体对话」是否已经补位过。
    ///
    /// 一次性闸门。智能体面的子页不落盘，补位只该在恢复完成后做一次；持续做会把
    /// 用户返回智能体目录的操作顶回对话页。
    restored_agent_conversation_route: bool,
    /// 客户端记下的上次活动会话位置。目录到了以后，按 id 找不到时用它兜底。
    startup_active_session: usize,
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
    User,
    Quit,
}

impl UpdateInstallTrigger {
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

fn conversation_session_id(session: &Session, cx: &App) -> Option<String> {
    match &session.kind {
        SessionKind::Conversation(view) => Some(view.read(cx).session_id().to_string()),
        SessionKind::Term { .. } => None,
    }
}

impl Workspace {
    /// Files 既可能展开到舞台，也可能停靠在 Tool Panel；两种状态都视为文件视图可见。
    fn files_view_visible(&self) -> bool {
        self.tool_panel_stage_active(tool_panel::ToolPanelTab::Files)
            || (self.tool_panel_open && self.tool_panel_tab == tool_panel::ToolPanelTab::Files)
    }

    /// Git「改动」页正在显示（舞台展开或停靠）。F7 的键位 context 只在这时挂上。
    fn git_changes_shown(&self) -> bool {
        (self.tool_panel_stage_active(tool_panel::ToolPanelTab::Git)
            || (self.tool_panel_open && self.tool_panel_tab == tool_panel::ToolPanelTab::Git))
            && self.git_tab == GitTab::Changes
    }

    /// 某个终端网格自己的焦点句柄正被聚焦（搜索框等子元素不算）。
    /// F7 / Esc 这类终端会编码进 PTY 的键，网格聚焦时必须让回去。
    fn terminal_grid_focused(&self, window: &Window, cx: &App) -> bool {
        self.sessions.iter().any(|session| {
            session.term_dock().is_some_and(|dock| {
                dock.leaves()
                    .iter()
                    .any(|view| view.read(cx).focus_handle().is_focused(window))
            })
        })
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 插件 tab 必须先于存档读取挂好：存档里的 tab 以插件 id 记录，
        // 注册表为空时会降级成 Files，用户上次停在插件面板就再也回不去。
        plugins::mount_all(window, cx);
        // 客户端界面状态（侧栏、当前页、Dock 布局）存在本机，同步读，窗口一次成型。
        // 项目和会话属于运行时，窗口起来后异步加载，到了再恢复会话
        // （见 start_workspace_catalog_load）。
        let ui = load_workspace_ui();
        let startup_active_session = ui.active_session;
        let collapsed_projects = ui.collapsed_projects.clone();
        let pinned_projects = ui.pinned_projects.clone();
        let saved = Some(ui.into_state());
        // 折叠、置顶按运行时分到各自的空间。
        let space_with_ui = |runtime: crate::runtimes::Runtime| {
            let mut space = crate::runtime_space::RuntimeSpace::new(runtime);
            let key = space.runtime.key().clone();
            space.collapsed_projects =
                workspace_persist::project_roots_of(&collapsed_projects, &key)
                    .into_iter()
                    .collect();
            space.pinned_projects = workspace_persist::project_roots_of(&pinned_projects, &key)
                .iter()
                .map(|root| crate::sidebar_order::normalize_project_root(root))
                .filter(|root| !root.is_empty())
                .collect();
            space
        };
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
        // 没有会话时右侧不继承全局偏好；新会话使用 SessionUiState 的默认收起状态。
        let initial_session_ui = SessionUiState::default();
        let shell = workspace_shell::install(cx.weak_entity(), window, cx);

        let mut ws = Self {
            sessions,
            active_session,
            nav: WorkspaceNav::from_persisted(
                saved
                    .as_ref()
                    .map(|state| state.route.clone())
                    .unwrap_or_default(),
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
            catalog_titles: HashMap::new(),
            dir_cache: HashMap::new(),
            dir_inflight: HashSet::new(),
            dir_generation: HashMap::new(),
            file_tree_pending_reveal: None,
            acp_image_preview: None,
            file_gen: 0,
            markdown_preview_epoch: 0,
            pending_file_switch: None,
            delete_file_target: None,
            pending_switch_after_save: None,
            git_log: git_log::GitLogState::default(),
            delete_branch_target: None,
            git_log_resize,
            sidebar_open,
            sidebar_w,
            shell_dock: shell.area,
            shell_applied_sidebar: shell.applied_sidebar,
            shell_applied_tool: shell.applied_tool,
            shell_needs_save: false,
            shell_pointer_down: false,
            shell_gesture_sidebar_w: None,
            shell_gesture_tool_w: None,
            diff_comment_input: None,
            diff_body: None,
            commit_msg_inputs: HashMap::new(),
            commit_errors: HashMap::new(),
            pending_notification_session_id: None,
            pending_conversation_wakes: Vec::new(),
            restored_agent_conversation_route: false,
            startup_active_session,
            ws_snapshot_epoch: 0,
            ws_snapshot_finalizing: false,
            workspace_menu_source_id: uuid::Uuid::new_v4().to_string(),
            next_workspace_menu_source_revision: AtomicU64::new(1),
            close_confirm: None,
            desktop: None,
            local: {
                let mut space = space_with_ui(crate::runtimes::Runtime::local());
                space.projects = projects;
                space.restore_pending = restore_pending;
                space
            },
            remote_spaces: cx
                .try_global::<crate::runtimes::RuntimeHub>()
                .map(|hub| {
                    hub.remotes
                        .iter()
                        .map(|remote| space_with_ui(remote.runtime.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            active_project: None,
            collapsed_agents: saved
                .as_ref()
                .map(|s| s.collapsed_agents.iter().cloned().collect())
                .unwrap_or_default(),
            sidebar_grouping: saved
                .as_ref()
                .map(|s| s.sidebar_grouping)
                .unwrap_or_default(),
            sidebar_hide_empty_projects: saved
                .as_ref()
                .map(|s| s.sidebar_hide_empty_projects)
                .unwrap_or(false),
            sidebar_recent_conversations_expanded: false,
            sess_drop_hint: None,
            proj_drop_hint: None,
            sidebar_drag: None,
            sidebar_reorder_generation: 0,
            shown_session_facts: Vec::new(),
            sidebar_drop_origin: None,
            sidebar_scroll: ScrollHandle::new(),
            sidebar_auto_scroll: gpui_component::scroll::AutoScroll::default(),
            ide_catalog: ide::IdeCatalog::default(),
            ide_popup_waiters: Vec::new(),
            palette: None,
            git_list_scroll: VirtualListScrollHandle::new(),
            file_tree_scroll: VirtualListScrollHandle::new(),
            file_tree_drag_start: None,
            file_filter: None,
            _file_filter_sub: None,
            search_results: None,
            search_gen: 0,
            search_task: None,
            search_cancel: None,

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
            branches_generation: HashMap::new(),
            branches_inflight: HashSet::new(),
            watched_projects: HashSet::new(),
            git_autofetch_at: HashMap::new(),
            session_list: HashMap::new(),
            session_list_inflight: HashSet::new(),
            session_list_invalidated: HashSet::new(),
            history_agent_bindings: HashMap::new(),
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
            dsh_config: None,
            dsh_config_error: None,
            dsh_config_generation: 0,
            dsh_model_editor_error: None,
            dsh_model_capabilities: None,
            pi_model_editor: None,
            pi_custom_provider_editor: None,
            pi_auth_providers: None,
            pi_login: None,
            pi_auth_model_picker: None,
            pi_auth_error: None,
            pi_auth_show_all: false,
            pi_mcp_login: None,
            pi_mcp_pending_remove: None,
            pi_mcp_error: None,
            pi_config: None,
            pi_config_error: None,
            pi_library: Default::default(),
            pi_config_generation: 0,
            pi_model_editor_error: None,
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
            window_handle: window.window_handle(),
            visible_refresh_scheduled: false,
            visible_refresh_running: false,
            debug_hud: false,
            show_quit_confirm: false,
            quit_requested: false,
            update_status: updater::UpdateStatus::default(),
            update_install_generation: 0,
            settings_section: settings::SettingsSection::default(),
            settings_page_nonce: 0,
            capabilities_view: None,
            font_options: std::cell::OnceCell::new(),
            dock_badge_count: None,
            status_running_count: None,
            status_menu_snapshot: None,
            rename_target: None,
            rename_input: None,
            _rename_sub: None,
            repo_info: HashMap::new(),
            repo_info_inflight: HashSet::new(),
            dir_picker: None,
            discard_hunk_target: None,
            discard_file_target: None,
            discard_all_target: None,
            git_writes: HashMap::new(),
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
            saved_active_session_id,
            saved_conversation_ui: workspace_persist::load_conversation_ui(),
            _remote_sessions_sub: None,
            _runtime_hub_sub: None,
            _agent_host_sub: None,
            _provider_status_sub: None,
            _history_titles_sub: None,
            _appearance_sub: None,
            remote_terminal_reattach_pending: HashSet::new(),
            remote_projected_terminal_ids: HashSet::new(),
            session_list_revision: 0,
            active_session_revision: 0,
            focus_handle: cx.focus_handle(),
            file_tree_focus_handle: cx.focus_handle(),
            modal_focus: cx.focus_handle(),
            main_modal_guard: false,
            main_modal_restore: None,
            main_modal_hold: false,
            main_modal_rank: None,
            settings_modal_guard: false,
            settings_modal_restore: None,
            settings_modal_rank: None,
        };
        // 只预取一个已知的系统 Finder 图标，不枚举第三方应用；真实 IDE 列表由
        // stage.rs 在有项目舞台显示后的空闲期再低优先级预热。
        ws.preload_file_manager_icon(window, cx);
        // 额度是应用级账户状态，启动后独立刷新，不依赖任何 ACP 会话是否创建。
        ws.start_provider_quota_watch(cx);
        // 侧栏「+」菜单按本机已装 CLI 过滤；启动后立刻探一次，免得等用户打开设置。
        ws.refresh_acp_runtime(cx);
        // 状态恢复、交换事务收敛和旧包清理由 updater 在同一把锁内完成；恢复出待安装
        // 作业后走与用户点击完全相同的安装驱动，没有待办才做常规静默检查。
        ws.recover_update_at_launch(true, cx);
        ws.start_workspace_catalog_load(window, cx);
        for space in &ws.remote_spaces {
            let runtime = space.runtime.clone();
            cx.defer_in(window, move |this, window, cx| {
                this.start_space_catalog_load(runtime, window, cx)
            });
        }
        ws._runtime_hub_sub = Some(
            cx.observe_global_in::<crate::runtimes::RuntimeHub>(window, |this, window, cx| {
                this.sync_remote_spaces(window, cx)
            }),
        );
        ws.start_daemon_outdated_watch(cx);
        ws.start_date_change_watch(cx);
        ws.reconcile_remote_catalog_projection(window, cx);
        ws._remote_sessions_sub = Some(
            cx.observe_global_in::<RemoteSessionCatalogGlobal>(window, |this, window, cx| {
                this.reconcile_remote_catalog_projection(window, cx)
            }),
        );
        ws._agent_host_sub =
            Some(cx.observe_global_in::<settings::AgentHostState>(window, |_, _, cx| cx.notify()));
        ws._provider_status_sub = Some(
            cx.observe_global_in::<smelt_ui::provider_status_global::ProviderStatus>(
                window,
                |_, _, cx| cx.notify(),
            ),
        );
        ws._history_titles_sub =
            Some(cx.observe_global_in::<HistoryTitles>(window, |_, _, cx| cx.notify()));
        ws._appearance_sub = Some(
            cx.observe_global_in::<Appearance>(window, |this, _window, cx| {
                this.schedule_visible_refresh(cx)
            }),
        );
        ws.start_visible_data_watch(cx);
        // 首帧绘制前就把输入框和缓存任务备好。观察器只在之后的外观变化时再排一次。
        ws.refresh_visible_caches(window, cx);
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

    /// 侧栏的「今天 / 昨天」和按时间分组的分桶都按当前日期算，日期一变就得重画，
    /// 哪怕没有任何会话事实变化。每分钟看一次日期，而不是排一个到零点的长计时器：
    /// 计时器按开机时长走，合盖睡眠那段不算，醒来后会晚很久才响。
    fn start_date_change_watch(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut today = Local::now().date_naive();
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(60))
                    .await;
                let date = Local::now().date_naive();
                let changed = date != today;
                today = date;
                let ok = this.update(cx, |_, cx| {
                    if changed {
                        cx.notify();
                    }
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
        // 工作区目录还没到时不知道哪些会话会被恢复，这时投影远程目录会把同一场会话
        // 再投一份。目录到了以后会补做一次。
        if !self.local.catalog_loaded {
            return;
        }
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
            .filter_map(smelt_model::session_control::RemoteSessionRecord::as_acp)
            .collect::<Vec<_>>();
        let term_records = snapshot
            .sessions
            .iter()
            .filter(|record| record.is_visible())
            .filter_map(smelt_model::session_control::RemoteSessionRecord::as_terminal)
            .collect::<Vec<_>>();
        self.reconcile_remote_sessions(acp_records, window, cx);
        self.reconcile_remote_terminal_sessions(term_records, window, cx);
    }

    /// 远程目录里的对话标题变了，就写进已经投影出来的侧栏行。
    ///
    /// 第一次见到某场对话只记下目录标题，不覆盖本地名。之后目录标题变了才是
    /// 控制面改名，这时才替换。
    fn apply_remote_conversation_titles(
        &mut self,
        records: &[smelt_model::session_control::RemoteAcpSession],
        cx: &mut Context<Self>,
    ) -> bool {
        let mut changed = false;
        for record in records {
            let title = record.title.trim();
            if title.is_empty() {
                continue;
            }
            let title = title.to_string();
            match self.catalog_titles.get(&record.id).map(String::as_str) {
                Some(seen) if seen == title => continue,
                Some(_) => {}
                None => {
                    let differs = self.sessions.iter().any(|session| {
                        conversation_session_id(session, cx).as_deref() == Some(record.id.as_str())
                            && session.custom_title.as_deref() != Some(title.as_str())
                            && session.custom_title.is_some()
                    });
                    self.catalog_titles.insert(record.id.clone(), title.clone());
                    if !differs {
                        continue;
                    }
                }
            }
            let Some(ix) = self.sessions.iter().position(|session| {
                conversation_session_id(session, cx).as_deref() == Some(record.id.as_str())
            }) else {
                self.catalog_titles.insert(record.id.clone(), title);
                continue;
            };
            if self.sessions[ix].custom_title.as_deref() != Some(title.as_str()) {
                self.sessions[ix].custom_title = Some(title.clone());
                if let SessionKind::Conversation(view) = &self.sessions[ix].kind {
                    let view = view.clone();
                    view.update(cx, |view, _| view.rename_session(Some(title.clone())));
                }
                changed = true;
            }
            self.catalog_titles.insert(record.id.clone(), title);
        }
        changed
    }

    fn reconcile_remote_sessions(
        &mut self,
        records: Vec<smelt_model::session_control::RemoteAcpSession>,
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
                    && smelt_model::session_control::is_background_acp_session_id(session_id);
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
        // 正在恢复的存档会话已经拿了这些 id，只是还没插进 `sessions`。不计进来就会
        // 把同一场对话再投影一份，两个视图抢同一个 smeltd 会话。
        let pending = crate::pending_restore_session_ids(&self.local.restore_pending);
        let mut changed = false;
        let titled = records
            .iter()
            .map(|(record, _)| record.clone())
            .collect::<Vec<_>>();
        for (record, agent) in records {
            if existing.contains(&record.id) || pending.contains(&record.id) {
                continue;
            }
            if !record.is_visible()
                || smelt_model::session_control::is_background_acp_session_id(&record.id)
            {
                continue;
            }
            if record.lifecycle != smelt_model::session_control::RemoteSessionLifecycle::Active {
                continue;
            }
            self.project_remote_acp_session(record, agent, window, cx);
            changed = true;
        }
        if self.apply_remote_conversation_titles(&titled, cx) {
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
        record: smelt_model::session_control::RemoteAcpSession,
        agent: settings::ConversationAgentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let profile_id = record
            .agent_option_id
            .strip_prefix("profile:")
            .map(String::from);
        let stored_title = record.resume_id.as_deref().and_then(|resume_id| {
            HistoryTitles::get(
                settings::HistorySourceKind::from(agent).id(),
                profile_id.as_deref(),
                resume_id,
                cx,
            )
        });
        // 手机上用产品智能体开的对话必须带着归属落地，否则接管后它会掉进项目
        // 会话列表，而不是侧栏「对话」栏。
        let agent_definition_id = record.agent_definition_id().map(str::to_string);
        let resume_id = record
            .resume_id
            .map(agent_client_protocol::schema::v1::SessionId::new);
        let cwd = Some(record.cwd.clone());
        if !smelt_model::session_control::is_agent_conversation(
            None,
            agent_definition_id.as_deref(),
            cwd.as_deref(),
        ) {
            self.remember_session_project(&RuntimeKey::Local, cwd.as_deref());
        }
        let view = cx.new(|cx| {
            acp_view::AcpView::placeholder(
                cx,
                acp_view::AcpViewOrigin {
                    runtime: crate::runtimes::Runtime::local(),
                    launch_prep: None,
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
            conversation_id: new_conversation_id(),
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
    ) -> Option<smelt_model::session_control::RemoteAcpSession> {
        let global = cx.try_global::<RemoteSessionCatalogGlobal>()?;
        let snapshot = global
            .snapshot
            .lock()
            .ok()
            .and_then(|snapshot| snapshot.clone())?;
        snapshot
            .sessions
            .iter()
            .filter_map(smelt_model::session_control::RemoteSessionRecord::as_acp)
            .find(|record| record.is_present() && record.id == session_id)
    }

    /// 跟 `reconcile_remote_sessions` 是同一套思路，对应移动端新建/删除的终端会话：
    /// 手机让 smeltd 建 PTY 并更新 daemon 自己的远程目录；这里消费同一条 subscribe
    /// 投影，把它 reattach 成 PC 侧栏里能看/能操作的一个真实 `Session`。
    fn reconcile_remote_terminal_sessions(
        &mut self,
        records: Vec<smelt_model::session_control::RemoteTerminalSession>,
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
        // 和 ACP 那边同理：存档里还在排队恢复的终端也已经报了这些 id。
        let pending_restore = crate::pending_restore_session_ids(&self.local.restore_pending);
        for record in records {
            if existing.contains(&record.id) || pending_restore.contains(&record.id) {
                continue;
            }
            if record.lifecycle != smelt_model::session_control::RemoteSessionLifecycle::Active {
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
            self.remember_session_project(&RuntimeKey::Local, Some(record.cwd.as_str()));
            let (tx, rx) = smol::channel::bounded(1);
            let cwd_bg = record.cwd.clone();
            let sid_bg = record.id.clone();
            std::thread::Builder::new()
                .name("smelt-remote-term-reattach".into())
                .spawn(move || {
                    let r = terminal::Terminal::reattach(
                        &crate::runtimes::Runtime::local(),
                        24,
                        80,
                        Some(&cwd_bg),
                        &sid_bg,
                    );
                    let _ = tx.send_blocking(r);
                })
                .expect("spawn smelt-remote-term-reattach 线程");
            let title = (!record.title.trim().is_empty()).then_some(record.title.clone());
            let cwd = record.cwd.clone();
            let sid = record.id.clone();
            let window_handle = self.window_handle;
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
                let _ = cx.update_window(window_handle, |_, window, cx| {
                    this.update(cx, |this, cx| {
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
                        if !smelt_model::session_control::accept_remote_terminal_projection(
                            &sid,
                            &this.remote_projected_terminal_ids,
                            &local_ids,
                        ) {
                            return;
                        }
                        let view = cx.new(|cx| {
                            TerminalView::from_terminal(
                                cx,
                                crate::runtimes::Runtime::local(),
                                terminal,
                                Some(cwd),
                                sid,
                                None,
                                None,
                            )
                        });
                        let mut session = Session::single(view, window, cx);
                        session.remote_owned = true;
                        session.custom_title = title;
                        this.sessions.push(session);
                        this.session_list_revision = this.session_list_revision.wrapping_add(1);
                        this.save_state(cx);
                        this.schedule_visible_refresh(cx);
                        cx.notify();
                    })
                });
            })
            .detach();
        }
    }

    /// Daemon 状态事件直接分发给所有终端 pane；不能依赖 TerminalView::render，
    /// 因为非当前顶层会话不会被挂进当前窗口的渲染树。
    fn handle_daemon_state_event(
        &mut self,
        runtime: &RuntimeKey,
        state: &terminal::DaemonSessionState,
        cx: &mut Context<Self>,
    ) {
        let panes: Vec<Entity<TerminalView>> = self
            .sessions
            .iter()
            .flat_map(Session::term_leaves)
            .collect();
        // 状态/相位变化不直接标脏 TerminalView：侧栏与徽标由
        // notify_if_session_facts_changed 刷新。终端内容本身走
        // TerminalView::drive_redraws 的事件通道即时重绘，不依赖这里。
        for pane in panes {
            let pane_view = pane.read(cx);
            if pane_view.session_id() == state.id && pane_view.runtime().key() == runtime {
                pane.update(cx, |terminal, cx| terminal.handle_daemon_state(state, cx));
            }
        }
    }

    /// 对话增量和守护状态推得很频繁，侧栏、工具面板却跟着 Workspace 的通知整块
    /// 重画。两条路径都只在会话露在工作台上的事实（见 `SessionFacts`）变了才
    /// 通知，变了就立刻通知，不靠定时合并：同一帧里的多次通知 GPUI 本来就只画一次。
    pub(crate) fn notify_if_session_facts_changed(&mut self, cx: &mut Context<Self>) {
        let current = self
            .sessions
            .iter()
            .map(|session| session.facts(cx))
            .collect();
        if replace_changed_facts(&mut self.shown_session_facts, current) {
            cx.notify();
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 工作台每帧都会重跑这里：debug 构建下，期间读写 SQLite 会被报出来。
        let _render_scope = smelt_client_store::render_guard::RenderScope::enter();
        let _timing = smelt_ui::frame_stats::RenderTiming::start("Workspace");
        // 帧上只清拖拽、认前台。目录、git、历史和窗口材质在选中与数据变化时刷新。
        self.prepare_frame(window, cx);

        // 主题色 token（跟随 gpui-component 主题，替代硬编码）
        // 内容区按 Grok Bot 画不透明实底。顶部导航的系统玻璃是窗口铬，不往分栏上叠透明度。
        let shell_surface: Hsla = rgb(ui_theme::bg_rail()).into();
        let shell_padding = ui_theme::shell_padding();

        self.align_shell_dock(window, cx);
        // 顶带横跨舞台和右栏。停靠缝从这条下面才把两列切开；开关贴在窗口最右，压在页签上。
        let tool_promoted = self.tool_panel_promoted();
        let (sidebar_span, tool_span) = {
            let dock = self.shell_dock.read(cx);
            let sidebar_span = if dock.is_dock_open(gpui_component::dock::DockPlacement::Left) {
                dock.dock_size(gpui_component::dock::DockPlacement::Left)
                    .unwrap_or(px(0.))
            } else {
                px(0.)
            };
            let tool_span = if !tool_promoted
                && dock.is_dock_open(gpui_component::dock::DockPlacement::Right)
            {
                dock.dock_size(gpui_component::dock::DockPlacement::Right)
                    .unwrap_or(px(0.))
            } else {
                px(0.)
            };
            (sidebar_span, tool_span)
        };
        let shell_chrome = self.render_shared_right_chrome(
            self.chrome_left_guard(window),
            tool_span,
            window.is_fullscreen(),
            cx,
        );
        // 外壳分栏拖动要在窗口级听按下和松开：弹出菜单等元素会拿走焦点、停止事件
        // 传播，只靠 Workspace 根节点收不全。在 paint 阶段注册窗口级监听。
        let shell_pointer_bridge = {
            let workspace = cx.entity().downgrade();
            canvas(
                |_, _, _| {},
                move |_bounds, _, window, _cx| {
                    let workspace_for_mouse = workspace.clone();
                    window.on_mouse_event(move |event: &MouseDownEvent, phase, _window, cx| {
                        if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                            let _ = workspace_for_mouse.update(cx, |workspace, _cx| {
                                if !workspace.shell_pointer_down {
                                    workspace.shell_gesture_sidebar_w = Some(workspace.sidebar_w);
                                    workspace.shell_gesture_tool_w = Some(workspace.tool_panel_w);
                                }
                                workspace.shell_pointer_down = true;
                            });
                        }
                    });
                    let workspace_for_up = workspace;
                    window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
                        if !phase.bubble() {
                            return;
                        }
                        let viewport = window.viewport_size().width;
                        let _ = workspace_for_up.update(cx, |workspace, cx| {
                            workspace.commit_shell_pointer(window, viewport, cx);
                        });
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
            // 见 focus_handle 字段注释：非终端页面没有可聚焦的子元素时，靠这个把
            // window 的 focus 兜底钉在这层，保证 Workspace 键位 context 在 dispatch 路径上。
            .track_focus(&self.focus_handle)
            // 挂在这一层才浅于 Terminal / Input。Files、GitChanges 是同一节点上的
            // 条件标识，谓词只看焦点路径上的 context，看不到任意 Rust 状态。
            .key_context(workspace_key_context(
                self.files_view_visible(),
                self.git_changes_shown(),
            ))
            .on_action(cx.listener(|this, _: &Quit, window, cx| {
                if this.quit_requested {
                    return;
                }
                this.begin_main_modal_focus(window, cx);
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
            // 文件内容视图右键菜单里的「发送选中内容到会话」，见 send_open_file_selection。
            .on_action(
                cx.listener(|this, _: &SendSelectionToTerminal, window, cx| {
                    this.send_open_file_selection(window, cx);
                }),
            )
            // 左右侧边栏切换（全局 Action，焦点在子组件或输入框内也能响应）
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| {
                this.toggle_sidebar(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleToolPanel, window, cx| {
                this.toggle_tool_panel(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                if this.palette.is_some() {
                    this.close_palette(window, cx);
                } else {
                    this.open_palette(window, cx);
                }
            }))
            // 切当前会话内的活动 pane（分屏）。编辑器里的 Outdent/Indent 绑在
            // 更深的 Input 上，焦点在输入框时由它们接手。
            .on_action(cx.listener(|this, _: &CyclePanePrev, window, cx| {
                this.cycle_pane(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CyclePaneNext, window, cx| {
                this.cycle_pane(1, window, cx);
            }))
            // Cmd+1~9：按侧栏视觉顺序（各项目分组依次铺平）跳到第 N 个会话。
            .on_action(cx.listener(|this, action: &ActivateSession, window, cx| {
                let visible = this.sidebar_session_order(cx);
                if let Some(&ix) = visible.get(action.index) {
                    this.activate(ix, window, cx);
                }
            }))
            // Cmd+D 右侧并排 / Cmd+Shift+D 下方堆叠。
            .on_action(cx.listener(|this, _: &SplitBeside, _window, cx| {
                this.split_active(Axis::Horizontal, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitBelow, _window, cx| {
                this.split_active(Axis::Vertical, cx);
            }))
            // Cmd+W 关闭当前 pane；会话只剩一个 pane 时关掉整个会话（至少留一个会话）。
            .on_action(cx.listener(|this, _: &ClosePane, window, cx| {
                this.close_active(window, cx);
            }))
            // Cmd+S：保存 Files 里打开的文件。键位只在 Files context 上，两种显示方式都算。
            .on_action(cx.listener(|this, _: &SaveFile, _window, cx| {
                this.save_open_file(cx);
            }))
            // Cmd+Shift+F 切换调试 HUD（帧耗时、GPU/CPU/内存）。
            // 编辑器把同一组合键绑成 Replace，更深的 Input 优先。
            .on_action(cx.listener(|this, _: &ToggleDebugHud, _window, cx| {
                this.debug_hud = !this.debug_hud;
                cx.notify();
            }))
            // F7 / Shift+F7：改动块之间跳。终端网格会把这两键编码进 PTY，
            // 网格聚焦时让出，交给终端自己的 on_key_down。
            .on_action(cx.listener(|this, _: &NextHunk, window, cx| {
                if this.terminal_grid_focused(window, cx) {
                    cx.propagate();
                    return;
                }
                this.jump_hunk(true, cx);
            }))
            .on_action(cx.listener(|this, _: &PrevHunk, window, cx| {
                if this.terminal_grid_focused(window, cx) {
                    cx.propagate();
                    return;
                }
                this.jump_hunk(false, cx);
            }))
            // Esc：先关工作区弹层；没有弹层时收掉 session 舞台覆盖。
            // 终端网格聚焦时 Esc 属于 PTY（vim 等），不关覆盖页。
            .on_action(cx.listener(|this, _: &DismissWorkspace, window, cx| {
                if this.dismiss_workspace_overlay(window, cx) {
                    return;
                }
                if !this.terminal_grid_focused(window, cx)
                    && this.active_tab().is_session()
                    && this.stage_cover.is_some()
                {
                    this.set_stage_cover(None, window, cx);
                    return;
                }
                cx.propagate();
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
                            .child(self.shell_dock.clone())
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left(sidebar_span)
                                    .right_0()
                                    // 停靠把手的发丝贴在分栏边上，从窗口顶画到窗口底。
                                    // 顶带这两像素盖成壳的底色，分栏从顶带下面才看得见。
                                    // 盖层不接鼠标，拖分隔条仍落在原来的把手上。
                                    .children(
                                        (sidebar_span > px(0.))
                                            .then(|| seam_cap(shell_surface).left(px(-1.))),
                                    )
                                    .children(
                                        (tool_span > px(0.)).then(|| {
                                            seam_cap(shell_surface).right(tool_span - px(1.))
                                        }),
                                    )
                                    .child(shell_chrome),
                            ),
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
                        // 这一条浮在拖窗口的顶带上：按下先拦住，不让它变成拖窗口。
                        div()
                            // 非全屏时红绿灯常驻，左边留 92px 让位；macOS 全屏会
                            // 隐藏红绿灯，这 92px 就全空了，按钮回到窗口左侧
                            // 18px 的统一视觉基准。纵向不再手算 top：由共用顶栏的
                            // flex 居中，UI 字号改变、按钮尺寸随 rem 缩放时也不会偏。
                            .ml(px(if window.is_fullscreen() { 18. } else { 92. }))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(
                                smelt_ui::Button::new("sidebar-toggle")
                                    .quiet()
                                    .compact()
                                    // 跟右侧两个开关同一套「细线 ↔ 实心色块」区分开合：
                                    // 收起态用 bundled 的细线 PanelLeft，展开态换成
                                    // panel-left-filled 实心色块，不再用带箭头的
                                    // -open/-close 变体（对齐 Codex 工具栏风格）。
                                    .icon(if self.sidebar_open {
                                        Icon::empty().path("smelt-icons/panel-left-filled.svg")
                                    } else {
                                        Icon::new(IconName::PanelLeft)
                                    })
                                    .accessibility_label("切换左侧栏")
                                    .tooltip_with_action("切换左侧栏", &ToggleSidebar, None)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_sidebar(window, cx);
                                    })),
                            ),
                    )
                    // 中间点穿。窗口开关在右侧共用顶栏里，不在这条浮层。
                    .child(div().flex_1().min_w_0()),
            )
            // 确认框、命令面板和图片预览都留在当前 Workspace 的 GPUI 树内。
            .child(self.render_overlay_layers(window, cx))
            .child(shell_pointer_bridge)
    }
}

/// 盖住顶带里的 1px 停靠发丝，并向分栏两侧各探 1px，避免像素对齐后露出一条缝。
/// 不接鼠标：拖分隔条仍命中下面的把手。
fn seam_cap(surface: Hsla) -> Div {
    div()
        .absolute()
        .top_0()
        .w(px(2.))
        .h(workspace_frame::TOP_BAR_HEIGHT)
        .bg(surface)
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
/// 是 `ConversationAgentKind::from_command_loose`。
fn acp_agent_from_cmd(cmd: &str) -> settings::ConversationAgentKind {
    settings::ConversationAgentKind::from_command_loose(cmd)
        .unwrap_or(settings::ConversationAgentKind::Claude)
}

/// 返回 shell 中第一个真正的程序 token，跳过 `VAR=value` 前缀和 `env`。
fn command_program(command: &str) -> Option<&str> {
    let mut tokens = command.split_whitespace();
    for token in tokens.by_ref() {
        if smelt_model::workspace_override::split_env_assignment(token).is_some() || token == "env"
        {
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
/// LaunchServices 交给外部应用；`#L42` 可选片段用于定位行号。远程运行时上的文件
/// 以运行时 id 作 host（`smelt-file://<id>/path`），本机的不带 host。
#[derive(Debug, PartialEq)]
struct InternalFileLink {
    /// 远程运行时的 id；本机为 None。
    runtime: Option<String>,
    path: String,
    line: Option<usize>,
}

fn smelt_file_url_to_target(url: &str) -> Option<InternalFileLink> {
    let rest = url.strip_prefix("smelt-file://")?;
    let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (runtime, path_part) = match rest.find('/')? {
        0 => (None, rest),
        host_end => (Some(rest[..host_end].to_string()), &rest[host_end..]),
    };
    let path = file_url_to_path(&format!("file://{path_part}"))?;
    let path = path.to_str()?.to_string();
    let line = fragment
        .strip_prefix('L')
        .and_then(|value| value.parse().ok());
    Some(InternalFileLink {
        runtime,
        path,
        line,
    })
}

/// 开一扇主工作台窗口（Workspace + Root 包装）。
/// 首启和「点 Dock 图标重开」共用这一份：`Workspace::new` 本来就会从存档 + smeltd
/// 重新拼出会话布局，跟正常重启应用效果一致。
/// 返回值同时留下窗口句柄：主窗口还在不在看它，不看 Workspace 还能不能升级。
fn open_workspace_window(cx: &mut App, window_bg: WindowBackgroundAppearance) -> MainWindow {
    let window_options =
        workspace_frame::workspace_window_options(ui_theme::shell_padding(), window_bg);
    let mut workspace = None;
    let window_handle = cx
        .open_window(window_options, |window, cx| {
            // 界面文字（侧边栏/标签页/状态栏等）用的都是 text_xs/text_sm 这类相对 rem
            // 单位。根据外观设置里的 ui_font_px（默认 16px）统一设置，全局跟着等比例缩放。
            // 终端内容本身的字号另由 terminal_view.rs 的 FONT_PX 控制，不受这个影响。
            let ui_font_px = cx.global::<Appearance>().ui_font_px;
            window.set_rem_size(px(ui_font_px as f32));
            let view = cx.new(|cx| Workspace::new(window, cx));
            workspace = Some(view.downgrade());
            // 顶层视图必须包一层 Root（组件库的主题/遮罩系统要求）。
            cx.new(|cx| Root::new(view, window, cx).bg(gpui::transparent_black()))
        })
        .expect("打开窗口失败");
    MainWindow {
        window: window_handle,
        workspace: workspace.expect("回调里一定会设置 workspace"),
    }
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

/// 组件库没有垃圾桶：`Delete` 是带 X 的退格键，`CircleX` 是带圈的关。
/// 删除/移除统一用这枚 Lucide trash-2。
pub(crate) fn trash_icon() -> Icon {
    Icon::empty().path("smelt-icons/trash.svg")
}

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

// 组件库的 `Assets` 只打包组件自己用的那一百来个图标；目录里其余的 Lucide 图标要在这里
// 点名才会编进来，没点名的路径加载不到，图标位置空着。用时写 `gpui_kit_assets::IconName`。
gpui_kit_assets::icon_assets!(LucideExtras, [Server]);

struct SmeltAssets;

impl gpui::AssetSource for SmeltAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if let Some(bytes) = agent_icon_svg(path) {
            return Ok(Some(std::borrow::Cow::Borrowed(bytes)));
        }
        if let Some(bytes) = LucideExtras.load(path)? {
            return Ok(Some(bytes));
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
        if path == "smelt-icons/trash.svg" {
            return Ok(Some(std::borrow::Cow::Borrowed(
                include_bytes!("../assets/icons/trash.svg").as_slice(),
            )));
        }
        if path == "smelt-icons/monitor.svg" {
            return Ok(Some(std::borrow::Cow::Borrowed(
                include_bytes!("../assets/icons/monitor.svg").as_slice(),
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
        gpui_kit_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<gpui::SharedString>> {
        gpui_kit_assets::Assets.list(path)
    }
}

fn schedule_obsolete_file_cleanup() {
    // 启动时只删已经没有任何读写路径的废弃文件；旧版 worktree 目录要用户在设置页确认。
    let _ = thread::spawn(|| {
        let removed = storage_cleanup::remove_obsolete_files_at_startup();
        if removed > 0 {
            eprintln!("[storage] 启动时清理了 {removed} 个历史残留文件");
        }
    });
}

/// Workspace 根节点的键位 context。Files / GitChanges 只在对应面板显示时出现，
/// 而且和 Workspace 写在同一个节点上：谓词匹配的是焦点路径上的标识，不是任意状态。
fn workspace_key_context(files_visible: bool, git_changes_shown: bool) -> &'static str {
    match (files_visible, git_changes_shown) {
        (false, false) => "Workspace",
        (true, false) => "Workspace Files",
        (false, true) => "Workspace GitChanges",
        (true, true) => "Workspace Files GitChanges",
    }
}

/// 应用键位。必须在 `set_menus` 之前注册，菜单项才会带上快捷键。
///
/// 新的工作区组合键绑在 `Workspace` 或更具体、但仍浅于 Terminal / Input 的
/// context 上。`None` 与最内层同深度，后注册会抢走输入框的键，这里不再新增。
/// Cmd+C / Cmd+V / Cmd+F 留在终端自己的按键监听里，不在这里绑。
fn app_key_bindings() -> Vec<KeyBinding> {
    let mut bindings = vec![
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("alt-cmd-b", ToggleToolPanel, None),
        // 会话上/下切换（跨项目按侧栏视觉顺序，到头循环）。
        // None 和最内层 context 同深度，后注册会盖过 Input 的 cmd-up/down。
        // 绑在 Workspace 上更浅：输入框里是文首/文末，其余工作区切会话。
        KeyBinding::new("cmd-up", PrevSession, Some("Workspace")),
        KeyBinding::new("cmd-down", NextSession, Some("Workspace")),
        KeyBinding::new("cmd-k", ToggleCommandPalette, Some("Workspace")),
        KeyBinding::new("cmd-[", CyclePanePrev, Some("Workspace")),
        KeyBinding::new("cmd-]", CyclePaneNext, Some("Workspace")),
        KeyBinding::new("cmd-d", SplitBeside, Some("Workspace")),
        KeyBinding::new("shift-cmd-d", SplitBelow, Some("Workspace")),
        KeyBinding::new("cmd-w", ClosePane, Some("Workspace")),
        KeyBinding::new("cmd-s", SaveFile, Some("Files")),
        KeyBinding::new("cmd-shift-f", ToggleDebugHud, Some("Workspace")),
        KeyBinding::new("f7", NextHunk, Some("GitChanges")),
        KeyBinding::new("shift-f7", PrevHunk, Some("GitChanges")),
        KeyBinding::new("escape", DismissWorkspace, Some("Workspace")),
        // 文件树只在自己的宿主上挂 FileTree。树没挂载（文件页不可见、树收起、
        // 搜索框有内容）时这条 context 不在焦点路径上。
        KeyBinding::new("up", FileTreeMoveUp, Some("FileTree")),
        KeyBinding::new("down", FileTreeMoveDown, Some("FileTree")),
        KeyBinding::new("left", FileTreeMoveLeft, Some("FileTree")),
        KeyBinding::new("right", FileTreeMoveRight, Some("FileTree")),
        KeyBinding::new("enter", FileTreeActivate, Some("FileTree")),
        // readline / Grok TUI 行编辑：Ctrl+U 删到行首、Ctrl+K 删到行尾、
        // Ctrl+W 删词。绑在 Input context，所有输入框（ACP composer 等）生效。
        KeyBinding::new("ctrl-u", DeleteToBeginningOfLine, Some("Input")),
        KeyBinding::new("ctrl-k", DeleteToEndOfLine, Some("Input")),
        KeyBinding::new("ctrl-w", DeleteToPreviousWordStart, Some("Input")),
        // 把 Tab/Shift-Tab 从 gpui-component Root 的全局焦点跳转手里要回来，
        // 终端聚焦时改发给 shell（见 terminal_view.rs 里 TerminalTab 的注释）。
        KeyBinding::new("tab", terminal_view::TerminalTab, Some("Terminal")),
        KeyBinding::new(
            "shift-tab",
            terminal_view::TerminalBackTab,
            Some("Terminal"),
        ),
    ];
    for (index, key) in [
        "cmd-1", "cmd-2", "cmd-3", "cmd-4", "cmd-5", "cmd-6", "cmd-7", "cmd-8", "cmd-9",
    ]
    .into_iter()
    .enumerate()
    {
        bindings.push(KeyBinding::new(
            key,
            ActivateSession { index },
            Some("Workspace"),
        ));
    }
    // cmux：Ctrl+Backspace / Ctrl+Delete 也是删行（^U / ^K），不是删词。
    #[cfg(target_os = "macos")]
    {
        bindings.push(KeyBinding::new(
            "ctrl-backspace",
            DeleteToBeginningOfLine,
            Some("Input"),
        ));
        bindings.push(KeyBinding::new(
            "ctrl-delete",
            DeleteToEndOfLine,
            Some("Input"),
        ));
    }
    bindings
}

fn main() {
    if let Some(code) = terminal::maybe_run_install_app(std::env::args()) {
        std::process::exit(code);
    }
    if let Some(code) = cli::maybe_run(std::env::args()) {
        std::process::exit(code);
    }
    // 整个 GUI 生命周期持共享 lease。installer 只有等所有 GUI 退出并拿到独占 lease
    // 后才可交换 Bundle；若交换已经开始，新 GUI fail-fast，避免从旧 vnode 启动。
    let _app_runtime_lease = match updater::acquire_app_runtime_lease() {
        Ok(lease) => lease,
        Err(error) => {
            eprintln!("Smelt 正在更新，暂时无法启动：{error:#}");
            std::process::exit(75);
        }
    };
    // GUI 启动失败时用户通常看不到终端输出；把原始 panic 先落到统一日志，
    // 尤其能保留 GPUI 清理阶段二次 panic 之前的第一条错误。
    smelt_sys::app_log::install_panic_hook("smelt");
    smelt_sys::app_log::tee_stderr("smelt");
    schedule_obsolete_file_cleanup();
    // Finder / launchd 启动的 GUI 默认软上限常是 256；在创建窗口、socket 和终端
    // 线程之前先抬高，避免正常多会话也让更新检查等新连接触发 EMFILE。
    smelt_sys::fd_limit::raise_fd_limit();
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

    // 当前主窗口。窗口在不在看 WindowHandle，不看 Workspace 弱句柄：设置窗口强持有
    // 那份实体，主窗口关掉后 upgrade 仍然成功。
    // on_reopen 得在 run() 之前挂在 Application builder 上（跟 on_open_urls 一样），
    // 但它触发时 run() 早已跑起来，Rc 到时候已经被 run() 里的首启逻辑填过了。
    let current_ws: Rc<RefCell<Option<MainWindow>>> = Rc::new(RefCell::new(None));
    {
        let current_ws = current_ws.clone();
        // 点 Dock 图标 / 双击程序图标重开：GPUI 只在系统判定「没有可见窗口」时才会调这个
        // 回调。主窗口的句柄还在就什么都不做，只有窗口真的没了才重新开一扇。
        app.on_reopen(move |cx| {
            ensure_main_window(&current_ws, cx);
        });
    }

    app.run(move |cx| {
        // 用任何 gpui-component 功能前必须先初始化。
        gpui_component::init(cx);
        // 组件自带的文案（对话框的确定/取消、命令面板、日历等）跟界面一起用中文。
        // 不设就是组件库默认的英文。
        gpui_component::set_locale("zh-CN");
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
        cx.bind_keys(app_key_bindings());
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
        // 代码字体必须在装主题之前落地：Theme.mono_font_family 从这里读。
        terminal_view::set_font_px(appearance.font_px);
        terminal_view::set_font_family(&appearance.font_family);
        settings::apply_named_theme(&appearance.theme_name, cx);
        // 用户自选终端底色要在首帧前生效，并连同主题一起发布给守护（移动端配色）。
        settings::apply_bg_color(&appearance);
        cx.set_global(settings::LaunchConfig::default());
        settings::start_launch_config_load(cx);
        cx.set_global(settings::load_update_settings());

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
        // provider 账户状态（额度 + Pi 模型目录版本）：刷新循环写，侧栏和对话
        // 模型菜单读。必须在建 Workspace 之前装好，否则启动首查无处可写。
        cx.set_global(smelt_ui::provider_status_global::ProviderStatus::default());
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
            if let Some(ws) = current_workspace(&current_ws_for_daemon_states) {
                let _ = ws.update(cx, |ws, cx| {
                    for (runtime, state) in &states {
                        ws.handle_daemon_state_event(runtime, state, cx);
                    }
                    let states: Vec<_> = states.into_iter().map(|(_, state)| state).collect();
                    ws.refresh_agent_memories_after_turns(&states, cx);
                    ws.sync_notification_surfaces(cx);
                    if ws.session_manager_open {
                        ws.refresh_session_manager(cx);
                    }
                    ws.maybe_flush_pending_daemon_upgrade(cx);
                    ws.notify_if_session_facts_changed(cx);
                });
            }
        });
        cx.set_global(DaemonStatesSubscription {
            _subscription: daemon_states_subscription,
        });
        // 智能体配置归运行时：先放客户端那部分（通知偏好），运行时那部分在后台加载，
        // 好了之后才按 hook 开关决定要不要安装。
        cx.set_global(settings::initial_agent_host_state());
        cx.set_global(settings::AcpRuntimeState::default());
        // hook 配置和 helper 必须作为一个版本单元升级：先把 App 内最新版同步到稳定
        // managed 路径，再改写各 provider 的 hook 命令。失败时保留旧 helper，不阻塞 GUI。
        if let Err(error) = settings::sync_bundled_smelt_notify() {
            eprintln!("[workspace] 同步 smelt-notify 失败：{error}");
        }
        // 新安装和缺少开关的旧配置都默认开启托管 hooks；只有用户明确关闭时跳过。
        // 安装由运行时执行（含 Codex app-server 信任握手），这里只发请求。
        settings::start_agent_host_load(cx, |cx| {
            if cx.global::<settings::AgentHostState>().agent_hooks_enabled {
                settings::reconcile_agent_hooks(cx);
            }
            // 运行时已经连上：请它照端点刷新 Pi、dsh 里标记为「自动」的自定义 provider
            // 的模型目录。用户没手填模型就是把这份目录交给我们维护，中转网关上下线模型
            // 不该要他再来点一次。dsh 要起 Node 子进程，几秒级，都放后台。
            cx.background_executor()
                .spawn(async {
                    use smelt_runtime_client::protocol::control::EmptyParams;
                    let timeout = std::time::Duration::from_secs(300);
                    if let Err(error) = runtime_call::call::<
                        smelt_model::control_api::SkillsSyncControl,
                    >(EmptyParams::default(), runtime_call::RUNTIME_CALL_TIMEOUT)
                    {
                        eprintln!("[workspace] 同步 smelt skill 失败：{}", runtime_call::error_text(error));
                    }
                    if let Err(error) = runtime_call::call::<
                        smelt_model::control_api::PiProvidersRefreshAuto,
                    >(EmptyParams::default(), timeout)
                    {
                        eprintln!("[pi] 刷新自动模型目录失败：{}", runtime_call::error_text(error));
                    }
                    if let Err(error) = runtime_call::call::<
                        smelt_model::control_api::DshProvidersRefreshAuto,
                    >(EmptyParams::default(), timeout)
                    {
                        eprintln!("[dsh] 刷新自动模型目录失败：{}", runtime_call::error_text(error));
                    }
                })
                .detach();
        });
        // 登记过的远程运行时：同步读客户端库，iroh 推迟到第一次连接时在后台绑。
        runtimes::start_runtime_hub(cx);
        // 每个运行时一条常驻 subscribe。手机建的会话目录、自动化、外观请求、项目文件
        // 变化这些投影目前只认本机；各运行时都有的是会话状态。
        let daemon_state_rx = runtime_events::start(cx);
        let current_ws_for_menu_republish = current_ws.clone();
        cx.spawn(async move |cx| {
            while let Ok((runtime, event)) = daemon_state_rx.recv().await {
                let is_local = runtime == RuntimeKey::Local;
                // 唤醒要拿窗口去重启对话视图，走 update_in；它不是状态投影，后面不用再看。
                // 主窗口已经关掉就放弃这次唤醒，不另开一扇。
                if let terminal::DaemonStateEvent::ConversationWakeRequested { session_id } = &event {
                    let workspace = current_workspace(&current_ws_for_menu_republish);
                    if let Some(ws) = workspace {
                        let _ = ws.update_in(cx, |ws, window, cx| {
                            ws.wake_conversation_for_remote(session_id.clone(), window, cx);
                        });
                    }
                    continue;
                }
                // 运行时换了进程，它替我们建的项目监听也没了：清空，下次刷新重新登记。
                if is_local
                    && matches!(event, terminal::DaemonStateEvent::Disconnected)
                    && let Some(ws) = current_workspace(&current_ws_for_menu_republish)
                {
                    let _ = ws.update(cx, |ws, _| ws.watched_projects.clear());
                }
                if let terminal::DaemonStateEvent::ProjectChanged { root } = &event {
                    if is_local && let Some(ws) = current_workspace(&current_ws_for_menu_republish) {
                        let root = root.clone();
                        let _ = ws.update(cx, |ws, cx| ws.on_project_changed(root, cx));
                    }
                    continue;
                }
                // 终端程序的 OSC 通知：运行时已滤掉有结构化 hook 的会话，这里进同一个
                // 关注 store，正在看的会话由 store 标已读、不弹。
                if let terminal::DaemonStateEvent::TerminalNotification { session_id, message } =
                    &event
                {
                    let item = AttentionItem {
                        session_id: session_id.clone(),
                        title: "终端通知".to_string(),
                        message: message.clone(),
                        kind: AttentionKind::Notice,
                    };
                    cx.update(|cx| {
                        AttentionGlobal::publish_terminal_notification(item, Instant::now(), cx);
                    });
                    continue;
                }
                if let terminal::DaemonStateEvent::TerminalProgress {
                    session_id,
                    progress,
                } = &event
                {
                    cx.update(|cx| DaemonStates::set_progress(&runtime, session_id, *progress, cx));
                    continue;
                }
                if let terminal::DaemonStateEvent::AppearanceChangeRequested { settings } = &event {
                    if is_local {
                        let settings = settings.clone();
                        cx.update(|cx| settings::apply_requested_appearance(&settings, cx));
                    }
                    continue;
                }
                cx.update(|cx| {
                    match &event {
                        // 刚连上一个运行时：把桌面持有的那份目录和菜单重新交给它（见下）。
                        terminal::DaemonStateEvent::Snapshot { .. } if !is_local => {
                            if let Some(ws) = current_workspace(&current_ws_for_menu_republish) {
                                let _ = ws.update(cx, |ws, cx| ws.save_state(cx));
                            }
                        }
                        terminal::DaemonStateEvent::RemoteSessions(_)
                        | terminal::DaemonStateEvent::Automations(_)
                            if !is_local => {}
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
                            if let Some(ws) = current_workspace(&current_ws_for_menu_republish) {
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
                        terminal::DaemonStateEvent::ConversationWakeRequested { .. } => {}
                        terminal::DaemonStateEvent::AppearanceChangeRequested { .. } => {}
                        terminal::DaemonStateEvent::ProjectChanged { .. } => {}
                        terminal::DaemonStateEvent::TerminalNotification { .. } => {}
                        terminal::DaemonStateEvent::TerminalProgress { .. } => {}
                        terminal::DaemonStateEvent::Disconnected => {}
                    }
                    match event {
                        terminal::DaemonStateEvent::Snapshot { sessions: list, .. } => {
                            let initial_snapshot = !DaemonStates::is_primed(&runtime, cx);
                            // attention 必须在 signal 前算完：先读旧镜像，写完
                            // 再 replace_all 唤醒观察器做 pane 副作用和侧栏刷新。
                            let previous = list
                                .iter()
                                .map(|state| DaemonStates::get(&runtime, &state.id, cx))
                                .collect::<Vec<_>>();
                            let now = Instant::now();
                            for (state, previous) in list.iter().zip(previous.iter()) {
                                if initial_snapshot {
                                    apply_daemon_attention_baseline(state, cx);
                                } else {
                                    apply_daemon_attention(previous.as_ref(), state, now, cx);
                                }
                            }
                            let stale_attention_ids =
                                DaemonStates::replace_all(&runtime, list, cx);
                            for id in stale_attention_ids {
                                AttentionGlobal::remove_session(&id, cx);
                            }
                        }
                        terminal::DaemonStateEvent::Update(s) => {
                            let previous = DaemonStates::get(&runtime, &s.id, cx);
                            apply_daemon_attention(
                                previous.as_ref(),
                                &s,
                                Instant::now(),
                                cx,
                            );
                            DaemonStates::upsert(&runtime, s, cx);
                        }
                        terminal::DaemonStateEvent::Removed { id } => {
                            DaemonStates::remove(&runtime, &id, cx);
                            AttentionGlobal::remove_session(&id, cx);
                        }
                        terminal::DaemonStateEvent::RemoteSessions(_) => {}
                        terminal::DaemonStateEvent::WorkspaceMenu(_) => {}
                        terminal::DaemonStateEvent::Automations(_) => {}
                        terminal::DaemonStateEvent::ConversationWakeRequested { .. } => {}
                        terminal::DaemonStateEvent::AppearanceChangeRequested { .. } => {}
                        terminal::DaemonStateEvent::ProjectChanged { .. } => {}
                        terminal::DaemonStateEvent::TerminalNotification { .. } => {}
                        terminal::DaemonStateEvent::TerminalProgress { .. } => {}
                        terminal::DaemonStateEvent::Disconnected => {
                            DaemonStates::mark_subscription_disconnected(&runtime, cx);
                        }
                    }
                });
            }
        })
        .detach();

        // 远程访问由运行时自己按配置开关；这里只取回配置给设置页显示，后台取，
        // 不卡首帧。
        cx.set_global(settings::RemoteConfig::default());
        cx.set_global(settings::RemoteSettingsState::default());
        // ACP 会话不需要在退出时做任何事：agent 子进程现在是 smeltd 托管的
        // （见 smelt_client::acp_client），GUI 这边只是个薄客户端，Cmd+Q 直接杀
        // 整个 GUI 进程也不会带走子进程——这正是托管这一层要解决的问题。
        // 远程访问同理跑在 smeltd 里，GUI 退出不影响远程设备。
        settings::start_remote_config_load(cx);
        // 首启主窗口，记入 current_ws（reopen 回调 / URL 投递循环都靠它判断当前主窗口）。
        *current_ws.borrow_mut() = Some(open_workspace_window(cx, window_bg));

        // 主窗口关掉后，设置窗口不能继续强持有那份 Workspace。回调发生时窗口已经不在，
        // 只能比 window id。订阅必须由 app global 持有，否则初始化闭包返回就取消了。
        let current_ws_closed = current_ws.clone();
        let main_window_closed = cx.on_window_closed(move |cx, closed_id| {
            let main_id = current_ws_closed
                .borrow()
                .as_ref()
                .map(|main| main.window.window_id());
            close_settings_if_main_window_closed(main_id, closed_id, cx);
        });
        cx.set_global(MainWindowClosedSubscription {
            _subscription: main_window_closed,
        });

        // 退出前尽量提交最后一次 workspace 文档。订阅必须由 app global 持有，否则
        // app.run 的初始化闭包返回时会被 Subscription::drop 自动取消。
        let current_ws_quit = current_ws.clone();
        let quit_subscription = cx.on_app_quit(move |cx| {
            // 插件要在这里释放，别留到线程局部变量析构时（见 `plugins::shutdown`）。
            plugins::shutdown();
            let flush_done = current_ws_quit
                .borrow()
                .as_ref()
                .and_then(|main| main.workspace.upgrade())
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

        // 消费 Dock / Finder 投递的目录：每个开一个项目（文件取父目录）。循环常驻到应用退出。
        // 主窗口没了就先关掉设置窗口再重开，避免把路径写进用户看不见的那份 Workspace。
        let current_ws_status = current_ws.clone();
        cx.spawn(async move |cx| {
            while let Ok(urls) = url_rx.recv().await {
                let internal_files: Vec<InternalFileLink> = urls
                    .iter()
                    .filter_map(|url| smelt_file_url_to_target(url))
                    .collect();
                let paths: Vec<std::path::PathBuf> =
                    urls.iter().filter_map(|u| file_url_to_path(u)).collect();
                if paths.is_empty() && internal_files.is_empty() {
                    continue;
                }
                let workspace = cx.update(|cx| {
                    let main = ensure_main_window(&current_ws, cx);
                    if !paths.is_empty() {
                        let _ = main
                            .workspace
                            .update(cx, |ws, cx| ws.open_paths(&paths, cx));
                    }
                    main.workspace
                });
                for link in internal_files {
                    let _ = workspace.update_in(cx, |ws, window, cx| match link.runtime {
                        // 内置编辑器只读本机文件。远程的文件在另一台机器上，打开本机同路径
                        // 的文件是错的，先说明；面板按运行时分以后改成去那台机器读。
                        Some(runtime) => {
                            let name = runtimes::runtime_name(&RuntimeKey::Remote(runtime), cx);
                            ws.report_background_error(format!(
                                "「{name}」上的文件暂时不能在这里打开：{}",
                                link.path
                            ));
                        }
                        None => ws.view_file_at(link.path, link.line, window, cx),
                    });
                }
            }
        })
        .detach();

        // 菜单栏图标/下拉菜单事件：主窗口还在就前置它并跳过去。窗口句柄失效时先关掉
        // 设置窗口再重开，避免设置页留在旧 Workspace 上。稳定的 daemon/run id 在恢复后
        // 仍可继续导航，只有菜单临时下标在新窗口里已经失去意义。
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
                    if let Some(ws) = current_workspace(&current_ws_status) {
                        let _ = ws.update(cx, |_, cx| cx.notify());
                    }
                    continue;
                }
                // 窗口句柄失效时先关掉设置窗口再重开，避免菜单栏跳出第二份 Workspace。
                let workspace =
                    cx.update(|cx| ensure_main_window(&current_ws_status, cx).workspace);
                let _ = workspace.update_in(cx, |ws, window, cx| {
                    match &event {
                        status_item::StatusItemEvent::JumpToDaemonSession(id) => {
                            ws.request_notification_session_jump(id.clone(), window, cx);
                        }
                        status_item::StatusItemEvent::JumpToAutomationRun {
                            automation_id,
                            run_id,
                        } => {
                            ws.navigate_to_automation_run(
                                automation_id.clone(),
                                run_id.clone(),
                                window,
                                cx,
                            );
                        }
                        status_item::StatusItemEvent::ActivateMain
                        | status_item::StatusItemEvent::SystemNotificationStateChanged => {}
                    }
                    // activateIgnoringOtherApps 负责前置应用；这里再显式激活主窗口，
                    // 让 macOS 切回它所在的原生全屏 Space，而不是只激活菜单栏。
                    window.activate_window();
                });
                status_item::activate_app();
            }
        })
        .detach();
    });
}

#[cfg(test)]
mod main_tests;
