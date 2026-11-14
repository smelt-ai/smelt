//! 文件：当前项目的文件树、项目内搜索，和打开的文件（查看、编辑、保存、Markdown / HTML
//! 预览、图片）。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 和宿主打交道。列目录、读写文件、搜索、git status
//! 都由文件所在那台机器的运行时执行，这里只管缓存和界面。
//!
//! 什么时候重取全靠事件：露出来、换项目、运行时推 `project.changed`（本插件自己登记的
//! 监听）、重新连上、窗口回到前台，以及自己做完的写操作。没有定时器。
//!
//! 界面现场（展开了哪些目录、选中、打开的文件）跟着项目走：换项目时停放，换回来接着用；
//! 也写进本插件自己的库，重启后还在（见 [`store`]）。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui::*;
use gpui_component::VirtualListScrollHandle;
use gpui_component::input::{InputEvent, InputState};
use smelt_plugin_api::{
    self as api, CommandSpec, Handled, Host, HostEvent, PluginView, RuntimeKey, ViewHandle,
    ViewSpec,
};

mod editor;
mod html_preview;
mod model;
mod store;
#[cfg(test)]
mod tests;
mod tree;
mod view;

use model::{DirCache, FileRef, GitMarks, OpenFile, SearchState};
use store::{FilesStore, SavedProject};

pub(crate) use api::ui::ui_theme;

const VIEW_ID: &str = "files";
/// 命令的来源，也是本插件库的名字。和搬出宿主之前一样，用户改过的快捷键还对得上。
const COMMAND_OWNER: &str = "smelt.files";
/// 整个视图的键位 context（保存在这里生效）。
const FILES_KEY_CONTEXT: &str = "Files";
/// 文件树的键位 context：方向键只在焦点在树里时归它。
const TREE_KEY_CONTEXT: &str = "FileTree";

/// 右边树那一列的宽度范围。树是导航区，不是主内容；限住最大值，编辑器至少留有可读的宽度。
const MIN_TREE_WIDTH: f32 = 190.0;
const MAX_TREE_WIDTH: f32 = 320.0;

actions!(
    smelt_files,
    [
        /// 文件树里选上一行。
        TreeUp,
        /// 文件树里选下一行。
        TreeDown,
        /// 收起目录，或选中上级目录。
        TreeLeft,
        /// 展开目录、进到第一个子项，或打开文件。
        TreeRight,
        /// 展开 / 收起目录，或打开文件。
        TreeActivate,
    ]
);

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    api::register_view(
        ViewSpec {
            id: VIEW_ID.into(),
            title: "文件".into(),
            icon: gpui_kit_assets::IconName::FolderTree.path(),
            per_machine: false,
            draws_top_bar: false,
            build: Rc::new(|window, cx| {
                Rc::new(cx.new(|cx| FilesPanel::new(window, cx))) as Rc<dyn ViewHandle>
            }),
        },
        cx,
    )
    .expect("文件的视图 id 不是宿主保留的");
    // 方向键不进命令表：命令的键要带 ⌘，方向键在别处（终端、输入框）另有用处。只绑在
    // 树自己的 context 上，焦点不在树里时不起作用。
    cx.bind_keys([
        KeyBinding::new("up", TreeUp, Some(TREE_KEY_CONTEXT)),
        KeyBinding::new("down", TreeDown, Some(TREE_KEY_CONTEXT)),
        KeyBinding::new("left", TreeLeft, Some(TREE_KEY_CONTEXT)),
        KeyBinding::new("right", TreeRight, Some(TREE_KEY_CONTEXT)),
        KeyBinding::new("enter", TreeActivate, Some(TREE_KEY_CONTEXT)),
    ]);
    let commands = vec![
        CommandSpec {
            name: "save".into(),
            title: "保存当前文件".into(),
            key: Some("cmd-s".into()),
            context: FILES_KEY_CONTEXT.into(),
            palette: true,
            requires_args: false,
            handler: Rc::new(|_, _, cx| {
                let Some(panel) = built_panel(cx) else {
                    return Handled::Pass;
                };
                panel.update(cx, |panel, cx| panel.save_open_file(cx));
                Handled::Done
            }),
        },
        // 文件打开者（见 `smelt_plugin_api::open_file`）：对话里的文件链接、变更视图的
        // 「打开文件」都经它进来。参数是 `FileLocation::to_args`，交给视图的 `open`。
        CommandSpec {
            name: "open".into(),
            title: "打开文件".into(),
            key: None,
            context: "Workspace".into(),
            palette: false,
            requires_args: true,
            handler: Rc::new(|args, window, cx| {
                if api::FileLocation::from_args(args).is_none() {
                    eprintln!("[files] smelt.files/open 参数不对：{args}");
                    return Handled::Done;
                }
                api::host(cx).open_view(VIEW_ID, args.clone(), window, cx);
                Handled::Done
            }),
        },
        // 编辑器右键菜单里的那一项，见 `send_open_file_selection`。
        CommandSpec {
            name: "send-selection".into(),
            title: "发送选中内容到会话".into(),
            key: None,
            context: FILES_KEY_CONTEXT.into(),
            palette: false,
            requires_args: false,
            handler: Rc::new(|_, window, cx| {
                let Some(panel) = built_panel(cx) else {
                    return Handled::Pass;
                };
                panel.update(cx, |panel, cx| panel.send_open_file_selection(window, cx));
                Handled::Done
            }),
        },
    ];
    api::register_commands(COMMAND_OWNER, "文件", commands, cx).expect("文件的命令表写错了");
    api::register_file_opener(COMMAND_OWNER, "open", cx);
}

/// 建好了的那份视图。没露出来过就还没建。
fn built_panel(cx: &App) -> Option<Entity<FilesPanel>> {
    api::built_view(VIEW_ID, cx)?
        .view()
        .downcast::<FilesPanel>()
        .ok()
}

/// 某台机器上的一个目录：项目根，或树里的一个目录。缓存都按它存，两台机器上同一路径是
/// 两回事。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ProjectRef {
    pub(crate) runtime: RuntimeKey,
    pub(crate) root: String,
}

/// 调运行时。客户端在主线程用 [`target`] 取，带去后台 [`call_on`]：一次调用是一次 socket
/// 往返。
mod rt {
    use std::time::Duration;

    use smelt_plugin_api::client::RuntimeClient;
    use smelt_plugin_api::client::protocol::control::{ControlCall, ControlClientError};
    use smelt_plugin_api::{MACHINE_REMOVED, RuntimeKey};

    pub(crate) use smelt_plugin_api::call_error_text as error_text;

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

/// 跟着项目走的那部分界面。换项目时停到 [`FilesPanel::parked`]，换回来原样接着用。
#[derive(Default)]
struct ProjectFilesUi {
    expanded: HashSet<String>,
    collapsed_roots: HashSet<String>,
    selected: Option<String>,
    open_file: Option<OpenFile>,
    editor_sub: Option<Subscription>,
    pending_reveal: Option<String>,
    restore_file: Option<String>,
}

pub(crate) struct FilesPanel {
    host: Rc<dyn Host>,
    store: FilesStore,
    focus_handle: FocusHandle,
    /// 文件树自己的焦点：点进树里，方向键才归树。
    tree_focus: FocusHandle,
    /// 当前项目。随 [`HostEvent::ActiveProjectChanged`] 更新，render 只读这份。
    project: Option<ProjectRef>,
    /// 别的项目停着的界面。
    parked: HashMap<ProjectRef, ProjectFilesUi>,

    // ---- 跟着当前项目走的（见 ProjectFilesUi） ----
    expanded: HashSet<String>,
    /// 多根时用户收起的根。根默认展开。
    collapsed_roots: HashSet<String>,
    /// 键盘选中的条目（左边一道强调色，区别于打开着的文件的底色）。
    selected: Option<String>,
    open_file: Option<OpenFile>,
    _editor_sub: Option<Subscription>,
    /// 等祖先目录列出来后要滚到的那一行。
    pending_reveal: Option<String>,
    /// 从库里读回来、还没打开的文件。打开要窗口，下一次刷新时打开。
    restore_file: Option<String>,

    // ---- 不分项目的 ----
    /// 钉进树里的本机目录，切到别的本机项目也一直挂着。
    pinned_roots: Vec<String>,
    tree_w: f32,
    tree_open: bool,
    tree_drag_start: Option<(f32, f32)>,
    /// 下一次打开文件的编号。
    next_file_id: u64,
    /// 「保存并切换」：那个文件保存成功后打开这个目标。
    pending_switch_after_save: Option<(u64, FileRef)>,
    /// 停手后再把编辑器内容交给预览；换掉就丢掉上一次。
    preview_task: Option<Task<()>>,
    filter: Entity<InputState>,
    _filter_sub: Subscription,
    search: Option<SearchState>,
    search_gen: u64,
    search_cancel: Option<Arc<AtomicBool>>,
    search_task: Option<Task<()>>,
    /// 各台机器的目录列表。
    dir_cache: HashMap<RuntimeKey, DirCache>,
    dir_inflight: HashSet<ProjectRef>,
    /// 目录列表的代次：列目录期间被标旧、被删，回来的那份就不算新。
    dir_generation: HashMap<ProjectRef, u64>,
    git_marks: HashMap<ProjectRef, GitMarks>,
    git_inflight: HashSet<ProjectRef>,
    git_generation: HashMap<ProjectRef, u64>,
    /// 已经请运行时监听过的根。
    watches: HashSet<ProjectRef>,
    tree_scroll: VirtualListScrollHandle,
    search_scroll: ScrollHandle,
    /// 宿主的遮罩层开着：HTML 预览的原生网页视图要让开。随事件更新。
    overlay_open: bool,
    refresh_scheduled: bool,
    _events: Subscription,
}

impl FilesPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let host = api::host(cx);
        let events = api::events(cx);
        let _events = cx.subscribe(&events, |this, _, event: &HostEvent, cx| {
            this.on_host_event(event, cx);
        });
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("搜索文件名 / 内容…"));
        let _filter_sub = cx.subscribe(&filter, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.schedule_visible_refresh(cx);
                cx.notify();
            }
        });
        let store = FilesStore::open();
        let settings = store.settings();
        let overlay_open = host.overlay_open(cx);
        let mut panel = Self {
            host,
            store,
            focus_handle: cx.focus_handle(),
            tree_focus: cx.focus_handle(),
            project: None,
            parked: HashMap::new(),
            expanded: HashSet::new(),
            collapsed_roots: HashSet::new(),
            selected: None,
            open_file: None,
            _editor_sub: None,
            pending_reveal: None,
            restore_file: None,
            pinned_roots: settings.pinned,
            tree_w: settings.tree_width,
            tree_open: settings.tree_open,
            tree_drag_start: None,
            next_file_id: 0,
            pending_switch_after_save: None,
            preview_task: None,
            filter,
            _filter_sub,
            search: None,
            search_gen: 0,
            search_cancel: None,
            search_task: None,
            dir_cache: HashMap::new(),
            dir_inflight: HashSet::new(),
            dir_generation: HashMap::new(),
            git_marks: HashMap::new(),
            git_inflight: HashSet::new(),
            git_generation: HashMap::new(),
            watches: HashSet::new(),
            tree_scroll: VirtualListScrollHandle::new(),
            search_scroll: ScrollHandle::new(),
            overlay_open,
            refresh_scheduled: false,
            _events,
        };
        panel.sync_project(cx);
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
            HostEvent::Reconnected(machine) => self.on_runtime_resubscribed(machine, cx),
            HostEvent::WindowActivated => self.on_window_refocused(cx),
            HostEvent::OverlayChanged => {
                let open = self.host.overlay_open(cx);
                if self.overlay_open != open {
                    self.overlay_open = open;
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    /// 文件树显示的是哪台机器上的目录：跟着当前项目走，没有项目时是本机。
    pub(crate) fn tree_runtime(&self) -> RuntimeKey {
        self.project
            .as_ref()
            .map(|project| project.runtime.clone())
            .unwrap_or(RuntimeKey::Local)
    }

    /// 那台机器上的路径怎么切。
    pub(crate) fn path_style(
        &self,
        machine: &RuntimeKey,
        cx: &App,
    ) -> api::model::runtime_path::PathStyle {
        self.host.path_style(machine, cx)
    }

    /// 重读当前项目。换了就把这个项目的界面停走，换上新项目停着的那份；没停过就从库里读。
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
        let ui = match &project {
            Some(project) => match self.parked.remove(project) {
                Some(ui) => ui,
                None => self.load_project_ui(project),
            },
            None => ProjectFilesUi::default(),
        };
        self.project = project;
        self.apply_project_ui(ui, cx);
        // 搜索结果属于上一个项目。
        self.cancel_file_search();
        self.search = None;
        cx.notify();
    }

    fn load_project_ui(&self, project: &ProjectRef) -> ProjectFilesUi {
        let saved = self.store.project(project);
        ProjectFilesUi {
            expanded: saved.expanded,
            collapsed_roots: saved.collapsed_roots,
            selected: saved.selected,
            restore_file: saved.open_file,
            ..ProjectFilesUi::default()
        }
    }

    fn take_project_ui(&mut self) -> ProjectFilesUi {
        // 「保存并切换」的目标是在这个项目里点的，换走了就不再切。
        self.pending_switch_after_save = None;
        ProjectFilesUi {
            expanded: std::mem::take(&mut self.expanded),
            collapsed_roots: std::mem::take(&mut self.collapsed_roots),
            selected: self.selected.take(),
            open_file: self.open_file.take(),
            editor_sub: self._editor_sub.take(),
            pending_reveal: self.pending_reveal.take(),
            restore_file: self.restore_file.take(),
        }
    }

    fn apply_project_ui(&mut self, ui: ProjectFilesUi, cx: &mut Context<Self>) {
        self.expanded = ui.expanded;
        self.collapsed_roots = ui.collapsed_roots;
        self.selected = ui.selected;
        self.open_file = ui.open_file;
        self._editor_sub = ui.editor_sub;
        self.pending_reveal = ui.pending_reveal;
        self.restore_file = ui.restore_file;
        // 停着的时候文件可能在别处改过，停放期间的变化通知只发给了当前项目。版本没变时
        // 什么也不动。
        if self.open_file.as_ref().is_some_and(|of| of.readable) {
            self.reload_open_file(cx);
        }
    }

    /// 把当前项目的现场写进库。
    pub(crate) fn persist_project(&self) {
        let Some(project) = &self.project else {
            return;
        };
        // 别的机器上的文件（对话里点的链接）不记：重启后按项目所在机器打开会打错。
        let open_file = self
            .open_file
            .as_ref()
            .filter(|of| of.runtime == project.runtime)
            .map(|of| of.path.clone());
        self.store.save_project(
            project,
            &SavedProject {
                selected: self.selected.clone(),
                open_file,
                expanded: self.expanded.clone(),
                collapsed_roots: self.collapsed_roots.clone(),
            },
        );
    }

    pub(crate) fn persist_tree_layout(&self) {
        self.store.save_tree_layout(self.tree_w, self.tree_open);
    }

    pub(crate) fn persist_pinned(&self) {
        self.store.save_pinned(&self.pinned_roots);
    }

    /// 后台失败：日志里留一份，再发系统通知。
    pub(crate) fn report_background_error(&self, message: impl Into<String>) {
        let message = message.into();
        eprintln!("[files] {message}");
        self.host.notify("错误", &message);
    }

    /// 把一次刷新排到这一轮之后。同一轮里多次调用合并成一次；刷新要窗口（打开文件要建
    /// 编辑器），在主窗口里跑。
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

    /// 视图露着时把看得见的数据补齐：每个挂出来的根都有监听、git 标记和目录列表，展开的
    /// 目录都列出来；搜索框有字时跑搜索。只取被标旧了的。
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_scheduled = false;
        if !self.host.view_shown(VIEW_ID, cx) {
            return;
        }
        if let Some(path) = self.restore_file.take()
            && self.open_file.is_none()
        {
            let file = FileRef {
                runtime: self.tree_runtime(),
                path,
            };
            self.open_file_now(file, None, window, cx);
        }
        let query = self.filter.read(cx).value().trim().to_string();
        if query.is_empty() {
            self.search = None;
            self.cancel_file_search();
            self.refresh_tree(cx);
        } else if let Some(project) = self.project.clone() {
            self.ensure_search(project, query, cx);
        }
    }
}

impl Focusable for FilesPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl PluginView for FilesPanel {
    /// 参数是 [`api::FileLocation::to_args`] 时打开那个文件；别的（从布局、菜单打开）只是
    /// 露出来。
    fn open(&mut self, args: &api::serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(location) = api::FileLocation::from_args(args) {
            let file = FileRef {
                runtime: location.machine,
                path: location.path,
            };
            self.view_file_on(file, location.line, window, cx);
        }
    }
}

impl Render for FilesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.focus_handle.clone();
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .track_focus(&self.focus_handle)
            .key_context(FILES_KEY_CONTEXT)
            // 点了视图里没有焦点的地方（预览、空白）也把焦点拿进来，保存键才认这里。里面
            // 的编辑器、输入框自己拿了焦点就不抢。
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                if !focus.contains_focused(window, cx) {
                    window.focus(&focus, cx);
                }
            })
            .child(self.render_panel(window, cx))
    }
}
