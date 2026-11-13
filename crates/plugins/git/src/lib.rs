//! 变更：当前项目各仓库的改动、暂存、提交、推送、分支，和提交历史（「改动」「日志」两页）。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 和宿主打交道。git 命令都由项目所在那台机器的运行时
//! 执行（`git.*` 控制方法），这里只管缓存和界面。
//!
//! 什么时候重取全靠事件：露出来、换项目、运行时推 `project.changed`（本插件自己登记的
//! 监听）、重新连上、窗口回到前台，以及自己做完的写操作。唯一的定时是视图露着、窗口在
//! 前台时的自动 fetch：远端仓库变了没有事件可等。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::*;
use gpui_base::{ResizablePanelGroup, ResizeHandleRenderer, ResizeHandleState};
use gpui_component::{StyledExt as _, VirtualListScrollHandle};
use gpui_component::resizable::ResizableState;
use smelt_plugin_api::{
    self as api, CommandSpec, Handled, Host, HostEvent, PluginView, RuntimeKey, ViewHandle,
    ViewSpec,
};

mod log;
mod log_view;
mod panel;

use panel::{
    BranchList, Cached, DiffScope, GitDiff, GitDiffBody, GitSeenChange, GitStatusData, GitWrite,
    PendingGitIndexOp, RepoSet,
};

pub(crate) use api::ui::ui_theme;

const VIEW_ID: &str = "git";
/// 命令的来源。和搬出宿主之前一样，用户改过的快捷键还对得上。
const COMMAND_OWNER: &str = "smelt.git";

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    api::register_view(
        ViewSpec {
            id: VIEW_ID.into(),
            title: "变更".into(),
            icon: gpui_kit_assets::IconName::GitBranch.path(),
            per_machine: false,
            draws_top_bar: false,
            build: Rc::new(|window, cx| {
                Rc::new(cx.new(|cx| GitPanel::new(window, cx))) as Rc<dyn ViewHandle>
            }),
        },
        cx,
    )
    .expect("变更的视图 id 不是宿主保留的");
    let commands = vec![
        jump_hunk_command("prev-hunk", "上一个差异", "shift-f7", false),
        jump_hunk_command("next-hunk", "下一个差异", "f7", true),
    ];
    api::register_commands(COMMAND_OWNER, "改动", commands, cx).expect("变更的命令表写错了");
}

/// F7 / Shift+F7 跳到下一块 / 上一块改动。只在焦点在「改动」页里时生效：焦点在终端里，
/// 这两个键该交给终端。
fn jump_hunk_command(name: &str, title: &str, key: &str, forward: bool) -> CommandSpec {
    CommandSpec {
        name: name.into(),
        title: title.into(),
        key: Some(key.into()),
        context: CHANGES_KEY_CONTEXT.into(),
        palette: true,
        requires_args: false,
        handler: Rc::new(move |_, _, cx| {
            let Some(panel) = built_panel(cx) else {
                return Handled::Pass;
            };
            panel.update(cx, |panel, cx| panel.jump_hunk(forward, cx));
            Handled::ok()
        }),
    }
}

/// 建好了的那份视图。没露出来过就还没建。
fn built_panel(cx: &App) -> Option<Entity<GitPanel>> {
    api::built_view(VIEW_ID, cx)?
        .view()
        .downcast::<GitPanel>()
        .ok()
}

/// 「改动」页的键位 context；「日志」页是 [`LOG_KEY_CONTEXT`]。
const CHANGES_KEY_CONTEXT: &str = "GitChanges";
const LOG_KEY_CONTEXT: &str = "GitLog";

/// 某台机器上的一个目录：项目根，或项目里的仓库根。缓存都按它存，两台机器上同一路径是
/// 两回事。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ProjectRef {
    pub(crate) runtime: RuntimeKey,
    pub(crate) root: String,
}

impl ProjectRef {
    /// 同一台机器上的另一个目录。
    pub(crate) fn at(&self, root: impl Into<String>) -> ProjectRef {
        ProjectRef {
            runtime: self.runtime.clone(),
            root: root.into(),
        }
    }
}

/// 视图里的两页。对标 JetBrains 的 Git 工具窗口：「提交」和「日志」是同一个窗口里的
/// 两个视图，不占两个标签。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum GitTab {
    /// 提交历史 + 分支图。
    Log,
    /// 工作区改动：文件树 + diff + 暂存 / 提交。
    #[default]
    Changes,
}

/// 调运行时。客户端在主线程用 [`target`] 取，带去后台 [`call_on`]：一次调用是一次 socket
/// 往返，运行时那边还要跑 git。
mod rt {
    use std::time::Duration;

    use smelt_plugin_api::client::RuntimeClient;
    use smelt_plugin_api::client::protocol::control::{ControlCall, ControlClientError};
    use smelt_plugin_api::{MACHINE_REMOVED, RuntimeKey};

    pub(crate) use smelt_plugin_api::call_error_text as error_text;

    /// 本机操作的默认上限。
    pub(crate) const RUNTIME_CALL_TIMEOUT: Duration = smelt_plugin_api::CALL_TIMEOUT;

    pub(crate) fn target(key: &RuntimeKey, cx: &gpui::App) -> Result<RuntimeClient, String> {
        smelt_plugin_api::host(cx)
            .client(key, cx)
            .ok_or_else(|| MACHINE_REMOVED.to_string())
    }

    pub(crate) fn call_on<C: ControlCall>(
        client: &RuntimeClient,
        params: C::Params,
        timeout: Duration,
    ) -> Result<C::Result, ControlClientError> {
        client.call::<C>(&params, timeout)
    }
}

/// 那台机器上的路径怎么切。
pub(crate) fn path_style(machine: &RuntimeKey, cx: &App) -> api::model::runtime_path::PathStyle {
    api::host(cx).path_style(machine, cx)
}

/// diff、提交号这些等宽文字用的字体：和终端同一个设置。
pub(crate) fn mono_font() -> String {
    api::model::font_config::font_family()
}

/// 后台失败：日志里留一份，再发系统通知。
pub(crate) fn notify_error(message: &str, cx: &mut App) {
    eprintln!("[git] {message}");
    let notify = api::Notify {
        subtitle: "错误".to_string(),
        body: message.to_string(),
    };
    api::NOTIFY.run(&notify, cx).detach();
}

/// 在 Finder 里选中本机的一个文件；Linux 打开所在目录。
pub(crate) fn reveal_in_finder(path: &str) {
    #[cfg(target_os = "macos")]
    let spawned = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
    #[cfg(not(target_os = "macos"))]
    let spawned = match api::model::runtime_path::PathStyle::LOCAL.parent(path) {
        Some(parent) => std::process::Command::new("xdg-open").arg(parent).spawn(),
        None => return,
    };
    if let Err(error) = spawned {
        eprintln!("[git] 在文件管理器里显示 {path} 失败：{error}");
    }
}

/// 居中的空状态 / 加载中。
pub(crate) fn placeholder_view(text: &str, muted: Hsla) -> Div {
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

/// 左右拆分（日志页的提交列表和详情），分隔条是和侧栏同一根发丝。
///
/// 组件库悬停时只在中间亮一小段药丸；这里整根从 `border_dim` 亮到 `text_faint`。命中和
/// 拖拽仍由组件库负责。
pub(crate) fn h_resizable(id: impl Into<ElementId>) -> ResizablePanelGroup {
    gpui_base::h_resizable(id).with_handle_appearance(hairline_handle())
}

/// 拖拽一两像素后指针就离开命中带，组件库自己的 hover 会灭；颜色跟
/// [`ResizeHandleState`] 走，按下和拖拽也保持亮着。
fn hairline_handle() -> ResizeHandleRenderer {
    Rc::new(|handle, _, _| {
        let color = rgb(if handle.state() == ResizeHandleState::Idle {
            ui_theme::border_dim()
        } else {
            ui_theme::text_faint()
        });
        let line = div().flex_none().bg(color);
        let line = match handle.axis() {
            Axis::Horizontal => line.w(px(1.)).h_full(),
            Axis::Vertical => line.h(px(1.)).w_full(),
        };
        Some(line.into_any_element())
    })
}

/// 改动页右边那列（改动树 + 提交框）的宽度范围。
const MIN_TREE_WIDTH: f32 = 190.0;
/// 树是导航区，不是主内容；限住最大值，diff 至少留有可读的宽度。
const MAX_TREE_WIDTH: f32 = 320.0;

/// 同一项目两次自动 fetch 至少隔这么久。
const GIT_AUTOFETCH_INTERVAL: Duration = Duration::from_secs(60);

/// 离下一次自动 fetch 还有多久；`ZERO` 是现在就该 fetch。
fn git_autofetch_wait(last: Option<Instant>, now: Instant) -> Duration {
    last.map_or(Duration::ZERO, |at| {
        GIT_AUTOFETCH_INTERVAL.saturating_sub(now.saturating_duration_since(at))
    })
}

/// 跟着项目走的那部分界面：哪一页、打开着的 diff、折叠了哪些。换项目时停到
/// [`GitPanel::parked`]，换回来原样接着用。
#[derive(Default)]
struct ProjectGitUi {
    git_tab: GitTab,
    git_diff: Option<GitDiff>,
    pending_diff_file: Option<String>,
    diff_comment_open: bool,
    git_collapsed_diff_files: HashSet<String>,
    diff_split: bool,
    diff_view: Option<Entity<gpui_component::diff::DiffState>>,
    diff_view_patch: String,
    diff_comment_range: Option<gpui_component::diff::DiffLineRange>,
    diff_view_sub: Option<Subscription>,
    diff_parse_error: Option<String>,
    git_tree_collapsed: HashSet<String>,
    diff_scope: Option<DiffScope>,
}

pub(crate) struct GitPanel {
    host: Rc<dyn Host>,
    focus_handle: FocusHandle,
    /// 当前项目。随 [`HostEvent::ActiveProjectChanged`] 更新，render 只读这份。
    project: Option<ProjectRef>,
    /// 别的项目停着的界面。
    parked: HashMap<ProjectRef, ProjectGitUi>,

    // ---- 跟着当前项目走的（见 ProjectGitUi） ----
    git_tab: GitTab,
    git_diff: Option<GitDiff>,
    /// 右侧改动树点了某个文件，等聚合 diff 读完再定位到它。
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
    diff_scope: DiffScope,

    // ---- 不分项目的 ----
    /// 打开 diff 的序号。异步读回来用它判断结果过没过期（换文件、换仓库、换项目都会
    /// 加一）。不跟项目停放：停着的项目读回来的结果不能落到另一个项目上。
    diff_gen: u64,
    git_log: log::GitLogState,
    git_log_resize: Entity<ResizableState>,
    diff_comment_input: Option<Entity<gpui_component::input::TextareaState>>,
    diff_body: Option<Entity<GitDiffBody>>,
    /// 每个仓库一个提交框：提交信息属于仓库。
    commit_msg_inputs: HashMap<ProjectRef, Entity<gpui_component::input::TextareaState>>,
    commit_errors: HashMap<ProjectRef, String>,
    git_list_scroll: VirtualListScrollHandle,
    /// 改动页右边那列的宽度，和拖它的分隔条时按下的位置。
    tree_w: f32,
    tree_drag_start: Option<(f32, f32)>,
    git_status: HashMap<ProjectRef, Cached<GitStatusData>>,
    git_repos: HashMap<ProjectRef, Cached<RepoSet>>,
    /// 正在发现仓库的项目；值是途中有没有又被标旧。
    git_repos_inflight: HashMap<ProjectRef, bool>,
    /// 用户选的提交目标仓库；没选过是 `None`。
    active_git_repo: Option<ProjectRef>,
    git_repo_collapsed: HashSet<ProjectRef>,
    git_status_generation: HashMap<ProjectRef, u64>,
    git_status_inflight: HashSet<ProjectRef>,
    git_status_failures: HashMap<ProjectRef, u8>,
    git_index_pending: HashMap<(ProjectRef, String), PendingGitIndexOp>,
    branches: HashMap<ProjectRef, Cached<BranchList>>,
    branches_generation: HashMap<ProjectRef, u64>,
    branches_inflight: HashSet<ProjectRef>,
    /// 已经请运行时监听过的项目。
    watches: HashSet<ProjectRef>,
    git_autofetch_at: HashMap<ProjectRef, Instant>,
    git_autofetch_running: bool,
    discard_file_target: Option<(ProjectRef, GitSeenChange)>,
    discard_all_target: Option<(ProjectRef, Vec<GitSeenChange>)>,
    discard_hunk_target: Option<(ProjectRef, usize)>,
    delete_branch_target: Option<(ProjectRef, String, bool)>,
    /// 各仓库正在跑的写操作：同一个仓库同时只跑一个。
    git_writes: HashMap<ProjectRef, GitWrite>,
    refresh_scheduled: bool,
    _events: Subscription,
}

impl GitPanel {
    fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let host = api::host(cx);
        let events = api::events(cx);
        let _events = cx.subscribe(&events, |this, _, event: &HostEvent, cx| {
            this.on_host_event(event, cx);
        });
        let project = host.active_project(cx).map(|project| ProjectRef {
            runtime: project.machine,
            root: project.root,
        });
        let mut panel = Self {
            host,
            focus_handle: cx.focus_handle(),
            project,
            parked: HashMap::new(),
            git_tab: GitTab::default(),
            git_diff: None,
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
            diff_scope: DiffScope::All,
            diff_gen: 0,
            git_log: log::GitLogState::default(),
            git_log_resize: cx.new(|_| ResizableState::default()),
            diff_comment_input: None,
            diff_body: None,
            commit_msg_inputs: HashMap::new(),
            commit_errors: HashMap::new(),
            git_list_scroll: VirtualListScrollHandle::new(),
            tree_w: MIN_TREE_WIDTH,
            tree_drag_start: None,
            git_status: HashMap::new(),
            git_repos: HashMap::new(),
            git_repos_inflight: HashMap::new(),
            active_git_repo: None,
            git_repo_collapsed: HashSet::new(),
            git_status_generation: HashMap::new(),
            git_status_inflight: HashSet::new(),
            git_status_failures: HashMap::new(),
            git_index_pending: HashMap::new(),
            branches: HashMap::new(),
            branches_generation: HashMap::new(),
            branches_inflight: HashSet::new(),
            watches: HashSet::new(),
            git_autofetch_at: HashMap::new(),
            git_autofetch_running: false,
            discard_file_target: None,
            discard_all_target: None,
            discard_hunk_target: None,
            delete_branch_target: None,
            git_writes: HashMap::new(),
            refresh_scheduled: false,
            _events,
        };
        // 布局恢复时一建出来就看得见，不会再有「露出来」的事件。建的过程中实体还没落地，
        // 排到这一轮之后再取。
        panel.schedule_visible_refresh(cx);
        panel
    }

    fn on_host_event(&mut self, event: &HostEvent, cx: &mut Context<Self>) {
        match event {
            HostEvent::ActiveProjectChanged => {
                self.sync_project(cx);
                self.schedule_visible_refresh(cx);
            }
            HostEvent::ShownViewsChanged | HostEvent::MachinesChanged => {
                self.schedule_visible_refresh(cx);
            }
            HostEvent::ProjectChanged { machine, root } => {
                let root = ProjectRef {
                    runtime: machine.clone(),
                    root: root.clone(),
                };
                self.on_project_changed(root, cx);
            }
            HostEvent::Reconnected(machine) => self.on_runtime_resubscribed_git(machine, cx),
            HostEvent::WindowActivated => self.on_window_refocused(cx),
            _ => {}
        }
    }

    /// 当前项目，随事件更新的那份。
    pub(crate) fn active_project_ref(&self, _cx: &App) -> Option<ProjectRef> {
        self.project.clone()
    }

    /// 重读当前项目。换了就把这个项目的界面停走，换上新项目停着的那份（没有就是空的）。
    fn sync_project(&mut self, cx: &mut Context<Self>) {
        let project = self.host.active_project(cx).map(|project| ProjectRef {
            runtime: project.machine,
            root: project.root,
        });
        if project == self.project {
            return;
        }
        if let Some(old) = self.project.take() {
            let ui = self.take_project_ui();
            self.parked.insert(old, ui);
        }
        let ui = project
            .as_ref()
            .and_then(|project| self.parked.remove(project))
            .unwrap_or_default();
        self.apply_project_ui(ui);
        self.project = project;
        // 停着时读到一半的 diff 不会再回来（序号已经变了），换回来要重读。
        if self
            .git_diff
            .as_ref()
            .is_some_and(|diff| diff.patch.is_none())
        {
            self.reset_git_diff_view();
        }
        self.diff_gen = self.diff_gen.wrapping_add(1);
        self.publish_diff_body(cx);
        cx.notify();
    }

    fn take_project_ui(&mut self) -> ProjectGitUi {
        ProjectGitUi {
            git_tab: std::mem::take(&mut self.git_tab),
            git_diff: self.git_diff.take(),
            pending_diff_file: self.pending_diff_file.take(),
            diff_comment_open: std::mem::take(&mut self.diff_comment_open),
            git_collapsed_diff_files: std::mem::take(&mut self.git_collapsed_diff_files),
            diff_split: std::mem::take(&mut self.diff_split),
            diff_view: self.diff_view.take(),
            diff_view_patch: std::mem::take(&mut self.diff_view_patch),
            diff_comment_range: self.diff_comment_range.take(),
            diff_view_sub: self._diff_view_sub.take(),
            diff_parse_error: self.diff_parse_error.take(),
            git_tree_collapsed: std::mem::take(&mut self.git_tree_collapsed),
            diff_scope: Some(std::mem::replace(&mut self.diff_scope, DiffScope::All)),
        }
    }

    fn apply_project_ui(&mut self, ui: ProjectGitUi) {
        self.git_tab = ui.git_tab;
        self.git_diff = ui.git_diff;
        self.pending_diff_file = ui.pending_diff_file;
        self.diff_comment_open = ui.diff_comment_open;
        self.git_collapsed_diff_files = ui.git_collapsed_diff_files;
        self.diff_split = ui.diff_split;
        self.diff_view = ui.diff_view;
        self.diff_view_patch = ui.diff_view_patch;
        self.diff_comment_range = ui.diff_comment_range;
        self._diff_view_sub = ui.diff_view_sub;
        self.diff_parse_error = ui.diff_parse_error;
        self.git_tree_collapsed = ui.git_tree_collapsed;
        self.diff_scope = ui.diff_scope.unwrap_or(DiffScope::All);
    }

    /// 后台失败当时就送出系统通知。
    pub(crate) fn report_background_error(&self, message: impl Into<String>, cx: &mut App) {
        notify_error(&message.into(), cx);
    }

    /// 把一次刷新排到这一轮之后。同一轮里多次调用合并成一次；刷新要窗口（建输入框），
    /// 在主窗口里跑。
    pub(crate) fn schedule_visible_refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh_scheduled {
            return;
        }
        let Some(window) = self.host.main_window(cx) else {
            return;
        };
        self.refresh_scheduled = true;
        let view = cx.weak_entity();
        cx.defer(move |cx| {
            let refreshed = cx
                .update_window(window, |_, window, cx| {
                    view.update(cx, |this, cx| this.refresh(window, cx)).is_ok()
                })
                .unwrap_or(false);
            // 窗口已经关了：把排队标记清掉，不然以后的请求都以为已经排上了。
            if !refreshed {
                let _ = view.update(cx, |this, _| this.refresh_scheduled = false);
            }
        });
    }

    /// 把看得见的数据补齐：当前项目一直有监听、各仓库的 status 一直备着（标签上的角标要
    /// 用）；视图露着时再取分支、准备改动页、跑自动 fetch。只取被标旧了的。
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_scheduled = false;
        let Some(project) = self.project.clone() else {
            return;
        };
        self.ensure_watch(project.clone(), cx);
        self.ensure_git_repos(project.clone(), cx);
        if !self.host.view_shown(VIEW_ID, cx) {
            return;
        }
        if self.host.window_active(cx) {
            self.ensure_git_autofetch(cx);
        }
        let repo = self.active_git_repo_root(&project);
        self.ensure_branches(repo, cx);
        self.prepare_git_panel(window, cx);
    }

    /// 视图露着、窗口在前台时，当前项目每 60 秒自动 fetch 一次，刷新领先 / 落后。
    ///
    /// 一个任务按到期时刻睡，醒来时视图看不见了、窗口到后台了就结束，下次刷新再起。
    fn ensure_git_autofetch(&mut self, cx: &mut Context<Self>) {
        if self.git_autofetch_running {
            return;
        }
        self.git_autofetch_running = true;
        cx.spawn(async move |this, cx| {
            loop {
                let wait = this
                    .update(cx, |this, cx| this.git_autofetch_tick(cx))
                    .ok()
                    .flatten();
                let Some(wait) = wait else {
                    break;
                };
                // 轮询例外：远端仓库变了没有事件可等；周期 60 秒，上限是视图露着且窗口在
                // 前台的这段时间，离开后最多再醒一次就结束。
                cx.background_executor().timer(wait).await;
            }
            let _ = this.update(cx, |this, _| this.git_autofetch_running = false);
        })
        .detach();
    }

    /// 到期就 fetch，返回离下一次还有多久；不该再跑了返回 None。
    fn git_autofetch_tick(&mut self, cx: &mut Context<Self>) -> Option<Duration> {
        if !self.host.view_shown(VIEW_ID, cx) || !self.host.window_active(cx) {
            return None;
        }
        let root = self.project.clone()?;
        let now = Instant::now();
        let wait = git_autofetch_wait(self.git_autofetch_at.get(&root).copied(), now);
        if !wait.is_zero() {
            return Some(wait);
        }
        self.git_autofetch_at.insert(root, now);
        self.git_fetch_silent(cx);
        Some(GIT_AUTOFETCH_INTERVAL)
    }

    /// 改动页里 diff 和右边那列之间的分隔条。拖它才改右边那列的宽度，拖外层面板只让
    /// diff 伸缩。
    pub(crate) fn tree_resize_handle(
        &mut self,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(id)
            .relative()
            .w(px(6.))
            .h_full()
            .flex_none()
            .cursor_col_resize()
            .group(id)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(3.))
                    .w(px(1.))
                    .h_full()
                    .bg(rgb(ui_theme::border_dim()))
                    .group_hover(id, |line| line.bg(rgb(ui_theme::text_faint()))),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    this.tree_drag_start = Some((f32::from(event.position.x), this.tree_w));
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    /// 窗口级监听：拖动时指针越过 6px 的分隔条也接着生效。
    pub(crate) fn tree_resize_listener(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        canvas(
            |_, _, _| {},
            move |_bounds, _, window, _cx| {
                let move_view = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
                    if !phase.bubble() || event.pressed_button != Some(MouseButton::Left) {
                        return;
                    }
                    move_view.update(cx, |this, cx| {
                        let Some((start_x, start_w)) = this.tree_drag_start else {
                            return;
                        };
                        let width = (start_w + start_x - f32::from(event.position.x))
                            .clamp(MIN_TREE_WIDTH, MAX_TREE_WIDTH);
                        if (this.tree_w - width).abs() > 0.5 {
                            this.tree_w = width;
                            cx.notify();
                        }
                    });
                });
                let up_view = view;
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _window, cx| {
                    if !phase.bubble() {
                        return;
                    }
                    up_view.update(cx, |this, cx| {
                        if this.tree_drag_start.take().is_some() {
                            cx.notify();
                        }
                    });
                });
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }
}

impl Focusable for GitPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl PluginView for GitPanel {
    /// 打开时看的是当前项目，参数没有用处。
    fn open(&mut self, _: &api::serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}

    /// 当前项目里**所有**仓库的改动文件数。只数项目根会让子仓里的改动在角标上隐形。
    fn badge(&self, _cx: &App) -> Option<SharedString> {
        let project = self.project.as_ref()?;
        let count: usize = self
            .project_repo_roots(project)
            .iter()
            .filter_map(|root| self.git_status.get(root))
            .map(|cached| cached.value.files.len())
            .sum();
        (count > 0).then(|| count.to_string().into())
    }
}

impl Render for GitPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let key_context = match self.git_tab {
            GitTab::Changes => CHANGES_KEY_CONTEXT,
            GitTab::Log => LOG_KEY_CONTEXT,
        };
        let focus = self.focus_handle.clone();
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .track_focus(&self.focus_handle)
            .key_context(key_context)
            // 点了视图里没有焦点的地方（列表行、diff）也把焦点拿进来，F7 才认这里。里面的
            // 输入框自己拿了焦点就不抢。
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                if !focus.contains_focused(window, cx) {
                    window.focus(&focus, cx);
                }
            })
            .child(self.git_narrow_panel(window, cx))
    }
}

#[cfg(test)]
mod split_tests;

#[cfg(test)]
mod autofetch_tests {
    use super::git_autofetch_wait;
    use std::time::{Duration, Instant};

    #[test]
    fn git_autofetch_runs_at_once_the_first_time_then_every_sixty_seconds() {
        let now = Instant::now();
        assert_eq!(git_autofetch_wait(None, now), Duration::ZERO);
        assert_eq!(git_autofetch_wait(Some(now), now), Duration::from_secs(60));
        let recent = now
            .checked_sub(Duration::from_secs(59))
            .expect("test clock");
        assert_eq!(
            git_autofetch_wait(Some(recent), now),
            Duration::from_secs(1)
        );
        let due = now
            .checked_sub(Duration::from_secs(61))
            .expect("test clock");
        assert_eq!(git_autofetch_wait(Some(due), now), Duration::ZERO);
    }
}
