//! Smelt 主窗口 NSH A2 外壳：
//! 单引擎线程 + 单界面树（`AppView` / `Core`）+ 每插件独立加固 QuickJS VM。
//!
//! 职责单一：
//! - 承载主窗口各页的 `nsh/base <DockArea>`（布局受控，与 `smelt_plugin_api::DockAreaState` 无损互转）；
//! - 统一发现、编译、热重载自带与用户 NSH 插件（`smelt-plugin.json`），隔离崩溃与资源配额；
//! - 向外壳和所有插件推送统一宿主状态（含 `safe_areas`、`js_views`、`shell` 快照）与离散事件；
//! - 不管理页面路由或 SQLite 布局存档——这些继续由 `crates/smelt` 的 `Workspace` 统一作为唯一权威。

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    rc::Rc,
};

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement as _, Render, Styled as _, Subscription, Task, Window, canvas, div,
};
use nsh_platform_gpui::{AppOptions, AppView};
use serde::Serialize;
use serde_json::{Value, json};
use smelt_plugin_api::{self as api, HostEvent};
use tokio::sync::mpsc;

use crate::{
    apply_theme,
    components::BackdropHooks,
    host,
    manifest::{LoadedPluginInfo, ShellLive, ShellSource, ShellUpdate, SourceDir},
    plugins::{PluginHost, PluginLifecycle},
    register_components,
    shell_layout::{
        ComputedShellView, PageLayout, WindowCorners, compute_shell_view, find_view_in_page,
        from_api_state, group_areas_json, layout_from_json, layout_to_json, panel_meta_json,
        panel_options_json, reveal_panel_in_layout, safe_areas_json, to_api_state,
    },
    store, ui_sets,
};

pub(crate) const MAIN_SHELL_BYTECODE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/main-shell/bundle.nshc"));

/// 传给主壳的本地化界面文案（由 `crates/smelt` 按当前语言填入）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellLabels {
    pub collapse_left: String,
    pub expand_left: String,
    pub collapse_right: String,
    pub expand_right: String,
    pub zoom: String,
    pub restore: String,
    pub settings_page_gone: String,
}

impl Default for ShellLabels {
    fn default() -> Self {
        Self {
            collapse_left: "Collapse left sidebar".into(),
            expand_left: "Expand left sidebar".into(),
            collapse_right: "Collapse right sidebar".into(),
            expand_right: "Expand right sidebar".into(),
            zoom: "Zoom".into(),
            restore: "Restore".into(),
            settings_page_gone: "Settings page unavailable".into(),
        }
    }
}

/// 一页出口中当前展示的目标（路由与参数）。
#[derive(Clone, Debug, PartialEq)]
pub struct OutletTarget {
    pub id: String,
    pub args: Value,
    pub seq: u64,
    /// `true` 表示这是宿主已建好的 Rust `instances: true` 视图（由 `smelt/outlet` 直接绘制，避免重复 `create_view`）。
    pub managed_rust_instance: bool,
}

/// 构造 [`MainShell`] 所需的宿主配置。
pub struct ShellConfig {
    pub sources: Vec<SourceDir>,
    /// 宿主私有插件字节码缓存目录（如 `~/.smelt/cache/plugins/`）；`None` 表示禁用磁盘缓存。
    pub cache_dir: Option<PathBuf>,
    pub backdrop: BackdropHooks,
    pub corners: WindowCorners,
    pub padding: f32,
    pub labels: ShellLabels,
}

/// 宿主退出确认对话框状态（推给主壳渲染）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct QuitDialogState {
    pub title: String,
    pub body: String,
    pub cancel_label: String,
    pub confirm_label: String,
    pub quitting: bool,
}

/// 远程目录选择器里的一行条目。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DirPickerEntryState {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub selected: bool,
}

/// 宿主远程目录选择器对话框状态（推给主壳渲染）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DirPickerDialogState {
    pub title: String,
    pub current_path: Option<String>,
    pub can_go_up: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub entries: Vec<DirPickerEntryState>,
    pub multiple: bool,
    pub selected_summary: Option<String>,
    pub confirm_label: String,
    pub can_confirm: bool,
    pub cancel_label: String,
    pub up_tooltip: String,
    pub home_label: String,
    pub loading_label: String,
    pub empty_label: String,
    pub select_tooltip: String,
    pub deselect_tooltip: String,
    /// 每次成功切到新目录递增，让前端列表滚回顶部。
    pub list_rev: u64,
}

/// 宿主通用确认对话框状态（如在线更新重启/退出/移动应用确认）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConfirmDialogState {
    pub title: String,
    pub description: String,
    pub note: Option<String>,
    pub note_bad: bool,
    pub cancel_label: String,
    pub confirm_label: String,
    pub danger: bool,
    pub busy: bool,
}

/// 宿主对话框快照（退出确认、远程目录选择器、通用确认）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct HostDialogsState {
    pub quit: Option<QuitDialogState>,
    pub dir_picker: Option<DirPickerDialogState>,
    pub confirm: Option<ConfirmDialogState>,
}

/// 主壳向宿主回报的对话框操作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostDialogAction {
    QuitConfirm,
    QuitCancel,
    DirPickerUp,
    DirPickerHome,
    DirPickerNavigate(String),
    DirPickerToggle(String),
    DirPickerConfirm,
    DirPickerCancel,
    ConfirmAccept,
    ConfirmCancel,
}

/// 主壳向 `Workspace` 汇报的事件。
#[derive(Clone, Debug)]
pub enum MainShellEvent {
    /// 某一页的 Dock 布局发生了用户改动（拖拽、改分栏、切标签、开合停靠区），请宿主存档并重算状态。
    LayoutChanged { page_key: String },
    /// 缩放、停靠区或视图状态变化，请宿主刷新可见集合与窗口材质。
    StateChanged,
    /// 插件列表或主题包更新。
    PluginsChanged,
    /// 启动扫描发现的首批脚本插件全部完成初始化（或失败 / 数量为零），可安全落位首页。
    InitialReady,
    /// 用户在宿主对话框里触发的操作。
    HostDialogAction(HostDialogAction),
    /// 用户在宿主设置导航或宿主设置页里触发的操作。
    HostSettingsAction(Value),
}

struct PageEntry {
    key: String,
    layout: PageLayout,
    /// 宿主侧主动替换/改写布局（`load_page` / `show_view` / `unzoom_active`）时递增；
    /// 由前端 `<DockArea>` 上报的 `LayoutChanged` 不递增，外壳据此区分外部指令与自身乐观提交。
    layout_rev: u64,
    outlet: Option<OutletTarget>,
}

enum ShellInput {
    LayoutChanged {
        page_key: String,
        layout: Value,
    },
    ZoomChanged {
        page_key: String,
        group: Option<u64>,
    },
    HostDialog(HostDialogAction),
    HostSettings(Value),
    Lifecycle(PluginLifecycle),
}

/// 主窗口唯一的 NSH A2 外壳视图。
pub struct MainShell {
    app: Entity<AppView>,
    plugins: PluginHost,
    bridge: host::Bridge,
    pages: Vec<PageEntry>,
    active_page: String,
    settings_page: Option<(String, String)>,
    host_dialogs: HostDialogsState,
    host_settings: Value,
    corners: WindowCorners,
    padding: f32,
    labels: ShellLabels,
    rem: f32,
    shell_view: ComputedShellView,
    shared_shell_view: Rc<RefCell<ComputedShellView>>,
    /// 导出 `root` 的就绪插件 id（由外壳挂在 0×0 隐藏层）。
    plugin_roots: BTreeSet<String>,
    /// 最近一轮扫描得到的插件列表（含重复/编译失败），供设置「面板插件」页展示。
    loaded_plugins: Vec<LoadedPluginInfo>,
    /// 插件运行时致命错误（覆盖编译成功状态以便设置页展示）。
    runtime_errors: BTreeMap<String, String>,
    /// 已登记的主题文件来源，重载时整批替换。
    theme_owners: Vec<String>,
    /// 已登记的纯声明式页面与布局来源，重载时整批替换。
    layout_owners: Vec<String>,
    /// 首批启动扫描中尚未报就绪或失败的脚本插件 id；`None` 表示首批扫描尚未送达。
    initial_pending: Option<BTreeSet<String>>,
    /// 因覆写版启动崩溃正在自动回退到自带版的插件 id。
    pending_fallback: BTreeSet<String>,
    /// 首批脚本插件是否已全部落定（就绪、失败或无脚本插件）。
    initial_ready: bool,
    /// 当前绑定窗口的材质分块回调（关窗重开时通过 `attach_window` / `detach_window` 切换，不重建引擎）。
    backdrop_hooks: Rc<RefCell<BackdropHooks>>,
    /// 只有 `self.app` 本帧确实被通知重画时才清空上一帧的材质分块；避免 `MainShell` 单独重画而
    /// `self.app` 走 GPUI Fast 缓存时把 `Regions` 清空闪一帧实色。
    app_dirty: Rc<Cell<bool>>,
    /// 同一轮 GPUI 更新内多次状态变更与离散信号事件合并成一次按脏切片位掩码的增量推送 + 去重转发，
    /// 避免休眠唤醒或流式输出把无关切片连重算并推给所有插件 VM。
    dirty_slices: host::SliceMask,
    flush_scheduled: bool,
    pending_events: Vec<HostEvent>,
    /// 绑定在当前主窗口上的激活/尺寸/外观观察器；关窗时清空，重开窗口时挂到新窗口。
    window_subs: Vec<Subscription>,
    live: ShellLive,
    _tasks: [Task<()>; 3],
    _subscriptions: [Subscription; 3],
}

impl EventEmitter<MainShellEvent> for MainShell {}

impl Focusable for MainShell {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.app.focus_handle(cx)
    }
}

impl MainShell {
    pub fn new(config: ShellConfig, window: &mut Window, cx: &mut Context<Self>) -> Self {
        nsh_platform_gpui::set_ui_sets(vec![nsh_base::registry(), crate::module::registry()]);

        let options = AppOptions {
            transparent: true,
            inline_root: true,
            ..AppOptions::default()
        };
        let app = cx.new(|cx| AppView::new(options, window, cx));
        let shared_shell_view = Rc::new(RefCell::new(ComputedShellView::default()));
        let safe_area_reader = shared_shell_view.clone();
        let backdrop_hooks = Rc::new(RefCell::new(config.backdrop));
        register_components(
            app.read(cx),
            backdrop_hooks.clone(),
            Some(Rc::new(move |id| {
                safe_area_reader.borrow().safe_area_of(id)
            })),
        );
        apply_theme(&app, cx);

        let plugins = PluginHost::new();
        let store_hub = store::StoreHub::global();
        store_hub.serve(&plugins);
        let bridge = host::Bridge::default();
        let mut actions = bridge.serve(&plugins);

        let (shell_tx, mut shell_rx) = mpsc::unbounded_channel::<ShellInput>();
        {
            let tx = shell_tx.clone();
            plugins.handle("shell.layout_changed", move |_plugin, mut call| {
                let mut data = call.take_data();
                let Some(page_key) = data["page"].as_str().map(str::to_owned) else {
                    return call.reject("shell.layout_changed：缺 page");
                };
                let layout = data["layout"].take();
                let _ = tx.send(ShellInput::LayoutChanged { page_key, layout });
                call.resolve(Value::Null);
            });
        }
        {
            let tx = shell_tx.clone();
            plugins.handle("shell.zoom_changed", move |_plugin, call| {
                let Some(page_key) = call.data()["page"].as_str().map(str::to_owned) else {
                    return call.reject("shell.zoom_changed：缺 page");
                };
                let group = call.data()["group"].as_u64();
                let _ = tx.send(ShellInput::ZoomChanged { page_key, group });
                call.resolve(Value::Null);
            });
        }
        {
            let tx = shell_tx.clone();
            plugins.handle("shell.host_dialog_action", move |plugin, call| {
                if plugin != crate::plugins::SHELL_OWNER {
                    return call.reject("shell.host_dialog_action：仅允许主壳调用");
                }
                let data = call.data();
                let Some(kind) = data["kind"].as_str() else {
                    return call.reject("shell.host_dialog_action：缺 kind");
                };
                let action = match kind {
                    "quit_confirm" => HostDialogAction::QuitConfirm,
                    "quit_cancel" => HostDialogAction::QuitCancel,
                    "dir_picker_up" => HostDialogAction::DirPickerUp,
                    "dir_picker_home" => HostDialogAction::DirPickerHome,
                    "dir_picker_navigate" => match data["path"].as_str() {
                        Some(path) => HostDialogAction::DirPickerNavigate(path.to_owned()),
                        None => return call.reject("shell.host_dialog_action：缺 path"),
                    },
                    "dir_picker_toggle" => match data["path"].as_str() {
                        Some(path) => HostDialogAction::DirPickerToggle(path.to_owned()),
                        None => return call.reject("shell.host_dialog_action：缺 path"),
                    },
                    "dir_picker_confirm" => HostDialogAction::DirPickerConfirm,
                    "dir_picker_cancel" => HostDialogAction::DirPickerCancel,
                    "confirm_accept" => HostDialogAction::ConfirmAccept,
                    "confirm_cancel" => HostDialogAction::ConfirmCancel,
                    other => {
                        let message = format!("shell.host_dialog_action：未知 kind {other}");
                        return call.reject(message);
                    }
                };
                let _ = tx.send(ShellInput::HostDialog(action));
                call.resolve(Value::Null);
            });
        }
        {
            let tx = shell_tx.clone();
            plugins.handle("shell.host_settings_action", move |plugin, mut call| {
                if plugin != crate::plugins::SHELL_OWNER {
                    return call.reject("shell.host_settings_action：仅允许主壳调用");
                }
                let _ = tx.send(ShellInput::HostSettings(call.take_data()));
                call.resolve(Value::Null);
            });
        }
        {
            plugins.set_lifecycle_observer(move |event| {
                let _ = shell_tx.send(ShellInput::Lifecycle(event));
            });
        }

        let err_host = plugins.clone();
        app.update(cx, |view, _| {
            view.on_app_error(move |error| {
                eprintln!("[smelt-nsh] {error}");
                err_host.on_app_error(error);
            });
        });

        let (updates_tx, mut updates_rx) = mpsc::unbounded_channel::<ShellUpdate>();
        let stage = std::env::temp_dir().join(format!("smelt-nsh-shell-{}", uuid::Uuid::new_v4()));
        let live = ShellLive::start(
            config.sources,
            stage,
            config.cache_dir,
            ShellSource::Bytecode(MAIN_SHELL_BYTECODE),
            ui_sets(),
            updates_tx,
        );

        let installing = cx.spawn(async move |this, cx| {
            while let Some(update) = updates_rx.recv().await {
                if this
                    .update(cx, |this, cx| this.apply_shell_update(update, cx))
                    .is_err()
                {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| this.mark_initial_ready(cx));
        });

        let action_bridge = bridge.clone();
        let action_plugins = plugins.clone();
        let serving = cx.spawn(async move |this, cx| {
            while let Some(action) = actions.recv().await {
                let mut batch = vec![action];
                while let Ok(more) = actions.try_recv() {
                    batch.push(more);
                }
                let mut any_view_reg = false;
                cx.update(|cx| {
                    for action in batch {
                        any_view_reg |= matches!(action, host::Action::RegisterViews { .. });
                        host::handle_action(action, &action_bridge, &action_plugins, cx);
                    }
                });
                if any_view_reg {
                    let _ = this.update(cx, |this, cx| {
                        this.schedule_publish_slices(
                            host::SliceMask::JS_VIEWS
                                | host::SliceMask::VIEWS
                                | host::SliceMask::SHELL,
                            cx,
                        );
                        cx.emit(MainShellEvent::PluginsChanged);
                    });
                }
            }
        });

        let handling_shell = cx.spawn(async move |this, cx| {
            while let Some(input) = shell_rx.recv().await {
                let mut batch = vec![input];
                while let Ok(more) = shell_rx.try_recv() {
                    batch.push(more);
                }
                if this
                    .update(cx, |this, cx| {
                        for input in batch {
                            this.handle_shell_input(input, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        let themed = cx.observe_global::<smelt_ui::theme_catalog::ActiveTheme>(|this, cx| {
            apply_theme(&this.app, cx);
            this.schedule_publish_slices(host::SliceMask::THEME, cx);
        });
        let events = cx.subscribe(&api::events(cx), |this, _, event: &HostEvent, cx| {
            if matches!(
                event,
                HostEvent::MachinesChanged | HostEvent::Reconnected(_)
            ) {
                this.bridge.sync_clients(cx);
            }
            let dirty = host::dirty_slices_for_event(event);
            if !dirty.is_empty() {
                if dirty.intersects(host::SliceMask::SHELL | host::SliceMask::SAFE_AREAS) {
                    this.recompute_shell_view(cx);
                }
                this.dirty_slices |= dirty;
            }
            this.enqueue_event(event.clone(), cx);
        });

        let app_dirty = Rc::new(Cell::new(true));
        let mark_app_dirty = app_dirty.clone();
        let app_obs = cx.observe(&app, move |_, _, cx| {
            mark_app_dirty.set(true);
            cx.notify();
        });

        bridge.sync_clients(cx);

        let rem = f32::from(window.rem_size());
        let shell = Self {
            app,
            plugins,
            bridge,
            pages: Vec::new(),
            active_page: String::new(),
            settings_page: None,
            host_dialogs: HostDialogsState::default(),
            host_settings: Value::Null,
            corners: config.corners,
            padding: config.padding,
            labels: config.labels,
            rem,
            shell_view: ComputedShellView::default(),
            shared_shell_view,
            plugin_roots: BTreeSet::new(),
            loaded_plugins: Vec::new(),
            runtime_errors: BTreeMap::new(),
            theme_owners: Vec::new(),
            layout_owners: Vec::new(),
            initial_pending: None,
            pending_fallback: BTreeSet::new(),
            initial_ready: false,
            backdrop_hooks,
            app_dirty,
            dirty_slices: host::SliceMask::EMPTY,
            flush_scheduled: false,
            pending_events: Vec::new(),
            window_subs: Vec::new(),
            live,
            _tasks: [installing, serving, handling_shell],
            _subscriptions: [themed, events, app_obs],
        };
        shell.publish_dirty(host::SliceMask::ALL, cx);
        shell
    }

    fn bind_window_observers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let app_act = self.app.clone();
        let act = cx.observe_window_activation(window, move |_, _, cx| {
            app_act.update(cx, |_, cx| cx.notify());
        });
        let app_bounds = self.app.clone();
        let bounds = cx.observe_window_bounds(window, move |_, _, cx| {
            app_bounds.update(cx, |_, cx| cx.notify());
        });
        let app_appearance = self.app.clone();
        let appearance = cx.observe_window_appearance(window, move |_, _, cx| {
            app_appearance.update(cx, |_, cx| cx.notify());
        });
        self.window_subs = vec![act, bounds, appearance];
    }

    /// 主窗口关闭后、重开新主窗口时把常驻壳体重新绑定到新窗口（引擎线程与所有插件 VM 全程不中断）。
    pub fn attach_window(
        &mut self,
        backdrop: BackdropHooks,
        corners: WindowCorners,
        padding: f32,
        labels: ShellLabels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        *self.backdrop_hooks.borrow_mut() = backdrop;
        self.corners = corners;
        self.padding = padding;
        self.labels = labels;
        self.rem = f32::from(window.rem_size());
        self.app_dirty.set(true);
        self.bind_window_observers(window, cx);
        self.recompute_shell_view(cx);
        self.app.update(cx, |_, cx| cx.notify());
        self.schedule_publish(cx);
    }

    /// 主窗口关闭时卸载全部页面与重组件，仅保留常驻引擎线程与插件无头根（`plugin_roots`）在后台运行。
    pub fn detach_window(&mut self, cx: &mut Context<Self>) {
        *self.backdrop_hooks.borrow_mut() = BackdropHooks::default();
        self.window_subs.clear();
        self.pages.clear();
        self.active_page.clear();
        self.settings_page = None;
        self.host_dialogs = HostDialogsState::default();
        self.host_settings = Value::Null;
        self.recompute_shell_view(cx);
        self.publish_dirty(host::SliceMask::SHELL | host::SliceMask::SAFE_AREAS, cx);
    }

    fn apply_shell_update(&mut self, update: ShellUpdate, cx: &mut Context<Self>) {
        // 先替换文件级主题包与纯声明式页面/布局，与旧插件系统保持一致。
        for owner in std::mem::take(&mut self.theme_owners) {
            smelt_ui::theme_catalog::unregister_themes(&owner, cx);
        }
        for owner in std::mem::take(&mut self.layout_owners) {
            api::unregister_layouts(&owner, cx);
            api::unregister_pages(&owner, cx);
        }
        let mut theme_errors = BTreeMap::new();
        for (id, sources) in update.themes {
            let owner = format!("files:{id}");
            let refs: Vec<&str> = sources.iter().map(String::as_str).collect();
            match smelt_ui::theme_catalog::register_theme_sets(&owner, &refs, cx) {
                Ok(_) => self.theme_owners.push(owner),
                Err(error) => {
                    eprintln!("[smelt-nsh] 插件 {id} 主题加载失败：{error}");
                    theme_errors.insert(id, error.to_string());
                }
            }
        }
        let mut layout_errors = BTreeMap::new();
        for (id, assets) in update.declarative_layouts {
            let pages: Vec<api::PageSource<'_>> = assets
                .pages
                .iter()
                .map(|p| api::PageSource {
                    name: &p.name,
                    title: p.title.clone(),
                    home: p.home,
                    index: p.index.as_deref(),
                })
                .collect();
            let layouts: Vec<api::LayoutSource<'_>> = assets
                .layouts
                .iter()
                .map(|l| api::LayoutSource {
                    page: &l.page,
                    name: &l.name,
                    title: l.title.clone(),
                    state: &l.state,
                    materials: l.materials,
                })
                .collect();
            let res = api::register_pages(&id, &pages, cx)
                .and_then(|()| api::register_layouts(&id, &layouts, cx));
            match res {
                Ok(()) => self.layout_owners.push(id),
                Err(error) => {
                    api::unregister_layouts(&id, cx);
                    api::unregister_pages(&id, cx);
                    eprintln!("[smelt-nsh] 插件 {id} 声明式布局加载失败：{error}");
                    layout_errors.insert(id, error);
                }
            }
        }

        for id in &update.removed_ids {
            self.plugin_roots.remove(id);
            self.runtime_errors.remove(id);
            self.bridge.unregister_plugin(id, cx);
            self.settle_initial_plugin(id, cx);
        }

        if let Some(initial_scripts) = update.initial_scripts
            && self.initial_pending.is_none()
        {
            self.initial_pending = Some(initial_scripts.into_iter().collect());
        }

        let mut installed_ids = BTreeSet::new();
        let mut pending_texts = update.texts;
        for change in update.changes {
            match &change {
                crate::plugins::Change::Start { .. } => {
                    for (id, texts) in std::mem::take(&mut pending_texts) {
                        self.bridge.set_texts(&id, texts);
                    }
                }
                crate::plugins::Change::Install(compiled) => {
                    let vm_id = compiled.id().to_owned();
                    let old_members = self.plugins.members_of(&vm_id);
                    let new_members = compiled.members().to_vec();
                    let mut affected = BTreeSet::new();
                    for member in old_members {
                        affected.insert(member);
                    }
                    for member in &new_members {
                        installed_ids.insert(member.clone());
                        affected.insert(member.clone());
                    }
                    // 重新装载前先撤掉旧一版的登记，避免重载去掉某个视图/命令/成员后残留；
                    // 随后立即写入新版文案，再启动新 VM，防止旧清理把刚读出的文案抹掉。
                    for member in &affected {
                        self.pending_fallback.remove(member);
                        self.runtime_errors.remove(member);
                        self.plugin_roots.remove(member);
                        self.bridge.unregister_plugin(member, cx);
                    }
                    self.bridge.forget_vm(&vm_id);
                    for member in &new_members {
                        if let Some(texts) = pending_texts.remove(member) {
                            self.bridge.set_texts(member, texts);
                        }
                    }
                }
                crate::plugins::Change::Remove(vm_id) => {
                    for member in self.plugins.members_of(vm_id) {
                        self.pending_fallback.remove(&member);
                        self.runtime_errors.remove(&member);
                        self.plugin_roots.remove(&member);
                        self.bridge.unregister_plugin(&member, cx);
                    }
                    self.bridge.forget_vm(vm_id);
                }
            }
            self.app.update(cx, |view, _| {
                self.plugins.apply(view, change);
            });
        }

        self.loaded_plugins = update
            .loaded
            .into_iter()
            .map(|mut info| {
                if info.fallback_to_builtin
                    && self.pending_fallback.remove(&info.id)
                    && !installed_ids.contains(&info.id)
                {
                    self.settle_initial_plugin(&info.id, cx);
                }
                if info.error.is_none()
                    && let Some(err) = theme_errors
                        .remove(&info.id)
                        .or_else(|| layout_errors.remove(&info.id))
                {
                    info.error = Some(err);
                }
                info
            })
            .collect();

        api::emit(HostEvent::ThemesChanged, cx);
        self.schedule_publish(cx);
        cx.emit(MainShellEvent::PluginsChanged);
        if self
            .initial_pending
            .as_ref()
            .is_some_and(BTreeSet::is_empty)
        {
            self.mark_initial_ready(cx);
        }
    }

    fn settle_initial_plugin(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(pending) = self.initial_pending.as_mut() {
            pending.remove(id);
            if pending.is_empty() {
                self.mark_initial_ready(cx);
            }
        }
    }

    fn mark_initial_ready(&mut self, cx: &mut Context<Self>) {
        if self.initial_ready {
            return;
        }
        self.initial_ready = true;
        self.initial_pending = Some(BTreeSet::new());
        cx.emit(MainShellEvent::InitialReady);
    }

    fn handle_shell_input(&mut self, input: ShellInput, cx: &mut Context<Self>) {
        match input {
            ShellInput::LayoutChanged { page_key, layout } => {
                let parsed = match layout_from_json(&layout) {
                    Ok(layout) => layout,
                    Err(error) => {
                        eprintln!("[smelt-nsh] 解析页 `{page_key}` 布局变更失败：{error}");
                        return;
                    }
                };
                let Some(entry) = self.pages.iter_mut().find(|p| p.key == page_key) else {
                    return;
                };
                if entry.layout.layout == parsed {
                    return;
                }
                entry.layout.layout = parsed;
                self.recompute_shell_view(cx);
                self.schedule_publish_slices(
                    host::SliceMask::SHELL | host::SliceMask::SAFE_AREAS,
                    cx,
                );
                cx.emit(MainShellEvent::LayoutChanged { page_key });
            }
            ShellInput::ZoomChanged { page_key, group } => {
                let Some(entry) = self.pages.iter_mut().find(|p| p.key == page_key) else {
                    return;
                };
                if entry.layout.zoomed == group {
                    return;
                }
                entry.layout.zoomed = group;
                self.recompute_shell_view(cx);
                self.schedule_publish_slices(
                    host::SliceMask::SHELL | host::SliceMask::SAFE_AREAS,
                    cx,
                );
                cx.emit(MainShellEvent::StateChanged);
            }
            ShellInput::HostDialog(action) => {
                cx.emit(MainShellEvent::HostDialogAction(action));
            }
            ShellInput::HostSettings(action) => {
                cx.emit(MainShellEvent::HostSettingsAction(action));
            }
            ShellInput::Lifecycle(PluginLifecycle::Ready { plugin, views }) => {
                let cleared_error = self.runtime_errors.remove(&plugin).is_some();
                let has_root = views.iter().any(|v| v == "root");
                let changed = if has_root {
                    self.plugin_roots.insert(plugin.clone())
                } else {
                    self.plugin_roots.remove(&plugin)
                };
                if changed {
                    self.schedule_publish_slices(host::SliceMask::SHELL, cx);
                }
                if cleared_error {
                    cx.emit(MainShellEvent::PluginsChanged);
                }
                self.settle_initial_plugin(&plugin, cx);
            }
            ShellInput::Lifecycle(PluginLifecycle::Fatal { plugin, message }) => {
                self.plugin_roots.remove(&plugin);
                self.bridge.unregister_plugin(&plugin, cx);
                let direct_fallback = self.loaded_plugins.iter().any(|info| {
                    info.id == plugin && info.overrides_builtin && !info.fallback_to_builtin
                });
                let module_fallback = if direct_fallback {
                    None
                } else {
                    self.loaded_plugins
                        .iter()
                        .find(|info| info.id == plugin)
                        .and_then(|info| {
                            info.used_modules.iter().find(|mod_id| {
                                self.loaded_plugins.iter().any(|m| {
                                    m.id == **mod_id
                                        && m.overrides_builtin
                                        && !m.fallback_to_builtin
                                })
                            })
                        })
                        .cloned()
                };
                if direct_fallback {
                    eprintln!(
                        "[smelt-nsh] 用户覆写插件 {plugin} 运行崩溃，已请求自动回退自带版本：{message}"
                    );
                    self.pending_fallback.insert(plugin.clone());
                    self.live.request_builtin_fallback(plugin, message);
                } else if let Some(mod_id) = module_fallback {
                    let reason = format!("使用该模块的插件 {plugin} 运行时加载失败：{message}");
                    eprintln!(
                        "[smelt-nsh] 用户覆写模块 {mod_id} 导致插件 {plugin} 崩溃，已请求自动回退自带版本：{message}"
                    );
                    if self.pending_fallback.insert(mod_id.clone()) {
                        self.live.request_builtin_fallback(mod_id, reason);
                    }
                } else {
                    self.runtime_errors.insert(plugin.clone(), message);
                    self.settle_initial_plugin(&plugin, cx);
                }
                self.schedule_publish(cx);
                cx.emit(MainShellEvent::PluginsChanged);
            }
        }
    }

    // ---- 供 Workspace 调用的主壳操作 ----

    /// 按 `DockAreaState` 装载或重装一页的布局。
    pub fn load_page(
        &mut self,
        page_key: String,
        state: &api::DockAreaState,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let page_layout = from_api_state(state, cx)?;
        if let Some(entry) = self.pages.iter_mut().find(|p| p.key == page_key) {
            entry.layout = page_layout;
            entry.layout_rev = entry.layout_rev.wrapping_add(1);
        } else {
            self.pages.push(PageEntry {
                key: page_key,
                layout: page_layout,
                layout_rev: 1,
                outlet: None,
            });
        }
        self.recompute_shell_view(cx);
        self.schedule_publish_slices(host::SliceMask::SHELL | host::SliceMask::SAFE_AREAS, cx);
        Ok(())
    }

    /// 导出某页的持久化 `DockAreaState`（写盘存档用）。
    pub fn dump_page(&self, page_key: &str) -> Option<api::DockAreaState> {
        let entry = self.pages.iter().find(|p| p.key == page_key)?;
        to_api_state(&entry.layout).ok()
    }

    /// 丢弃不再需要的页。
    pub fn remove_page(&mut self, page_key: &str, cx: &mut Context<Self>) {
        let before = self.pages.len();
        self.pages.retain(|p| p.key != page_key);
        if self.pages.len() != before {
            self.recompute_shell_view(cx);
            self.schedule_publish_slices(host::SliceMask::SHELL | host::SliceMask::SAFE_AREAS, cx);
        }
    }

    /// 切到某一页，并同步该页出口的内容。
    pub fn set_active_page(
        &mut self,
        page_key: String,
        outlet: Option<OutletTarget>,
        cx: &mut Context<Self>,
    ) {
        let page_changed = self.active_page != page_key;
        self.active_page = page_key.clone();
        let mut outlet_changed = false;
        if let Some(entry) = self.pages.iter_mut().find(|p| p.key == page_key)
            && entry.outlet != outlet
        {
            entry.outlet = outlet;
            outlet_changed = true;
        }
        if page_changed || outlet_changed {
            self.recompute_shell_view(cx);
            self.schedule_publish_slices(host::SliceMask::SHELL | host::SliceMask::SAFE_AREAS, cx);
        }
    }

    /// 更新指定页出口里的视图与参数。
    pub fn set_outlet(
        &mut self,
        page_key: &str,
        outlet: Option<OutletTarget>,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.pages.iter_mut().find(|p| p.key == page_key) else {
            return;
        };
        if entry.outlet == outlet {
            return;
        }
        entry.outlet = outlet;
        self.recompute_shell_view(cx);
        self.schedule_publish_slices(host::SliceMask::SHELL | host::SliceMask::SAFE_AREAS, cx);
    }

    /// 更新设置页右侧选中的具体设置视图（Rust 或 JS 视图皆可）。
    pub fn set_settings_page(&mut self, page: Option<(String, String)>, cx: &mut Context<Self>) {
        if self.settings_page == page {
            return;
        }
        self.settings_page = page;
        self.schedule_publish_slices(host::SliceMask::SHELL, cx);
    }

    /// 更新宿主对话框状态（退出确认、远程目录选择器）。状态未变时直接返回。
    pub fn set_host_dialogs(&mut self, dialogs: HostDialogsState, cx: &mut Context<Self>) {
        if self.host_dialogs == dialogs {
            return;
        }
        self.host_dialogs = dialogs;
        self.schedule_publish_slices(host::SliceMask::SHELL, cx);
    }

    /// 更新宿主设置导航与当前宿主设置页快照。状态未变时直接返回。
    pub fn set_host_settings(&mut self, settings: Value, cx: &mut Context<Self>) {
        if self.host_settings == settings {
            return;
        }
        self.host_settings = settings;
        self.schedule_publish_slices(host::SliceMask::SHELL, cx);
    }

    /// 更新窗口角占空（进出全屏时变化）。
    pub fn set_corners(&mut self, corners: WindowCorners, cx: &mut Context<Self>) {
        if self.corners == corners {
            return;
        }
        self.corners = corners;
        self.recompute_shell_view(cx);
        self.schedule_publish_slices(host::SliceMask::SHELL | host::SliceMask::SAFE_AREAS, cx);
    }

    /// 更新壳层外边距与本地化文案。
    pub fn set_chrome(&mut self, padding: f32, labels: ShellLabels, cx: &mut Context<Self>) {
        if (self.padding - padding).abs() < f32::EPSILON && self.labels == labels {
            return;
        }
        self.padding = padding;
        self.labels = labels;
        self.schedule_publish_slices(host::SliceMask::SHELL, cx);
    }

    /// 某页的布局里是否摆着指定视图（含 `smelt/stack` 内的叶子）。
    pub fn has_view(&self, page_key: &str, view_id: &str) -> bool {
        self.pages
            .iter()
            .find(|p| p.key == page_key)
            .is_some_and(|p| find_view_in_page(&p.layout, view_id).is_some())
    }

    /// 某页布局里当前所有已摆放的视图 id（不含未登记占位）。
    pub fn page_laid_out(&self, page_key: &str) -> std::collections::HashSet<String> {
        let Some(entry) = self.pages.iter().find(|p| p.key == page_key) else {
            return std::collections::HashSet::new();
        };
        let mut out = std::collections::HashSet::new();
        for leaf in entry.layout.leaves.values() {
            match leaf {
                crate::shell_layout::LeafMeta::View { id, .. } => {
                    out.insert(id.clone());
                }
                crate::shell_layout::LeafMeta::Stack { stack, .. } => {
                    out.extend(stack.views().into_iter().map(str::to_owned));
                }
                crate::shell_layout::LeafMeta::Placeholder { .. } => {}
            }
        }
        out
    }

    /// 在指定页展开停靠区、退出遮挡放大并切到指定视图。
    pub fn show_view(&mut self, page_key: &str, view_id: &str, cx: &mut Context<Self>) -> bool {
        let Some(entry) = self.pages.iter_mut().find(|p| p.key == page_key) else {
            return false;
        };
        let panel_id = if entry.outlet.as_ref().is_some_and(|o| o.id == view_id) {
            api::OUTLET_VIEW
        } else {
            view_id
        };
        let Some((panel_key, _, _)) = find_view_in_page(&entry.layout, panel_id) else {
            return false;
        };
        let was_zoomed = entry.layout.zoomed;
        let changed = reveal_panel_in_layout(&mut entry.layout, &panel_key);
        if was_zoomed.is_some() && entry.layout.zoomed.is_none() {
            self.plugins
                .send_to_shell("shell.unzoom", json!({ "page": page_key }));
        }
        if changed {
            entry.layout_rev = entry.layout_rev.wrapping_add(1);
            self.recompute_shell_view(cx);
            self.schedule_publish_slices(host::SliceMask::SHELL | host::SliceMask::SAFE_AREAS, cx);
        }
        true
    }

    /// Esc：还原当前页的放大组。
    pub fn unzoom_active(&mut self, cx: &mut Context<Self>) -> bool {
        let active = self.active_page.clone();
        let Some(entry) = self.pages.iter_mut().find(|p| p.key == active) else {
            return false;
        };
        if entry.layout.zoomed.is_none() {
            return false;
        }
        entry.layout.zoomed = None;
        entry.layout_rev = entry.layout_rev.wrapping_add(1);
        self.plugins
            .send_to_shell("shell.unzoom", json!({ "page": active }));
        self.recompute_shell_view(cx);
        self.schedule_publish_slices(host::SliceMask::SHELL | host::SliceMask::SAFE_AREAS, cx);
        true
    }

    /// 当前活跃页的快照（可见视图、安全区、停靠区开合状态）。
    pub fn shell_view(&self) -> &ComputedShellView {
        &self.shell_view
    }

    /// 判断视图 id 是不是由 NSH JS 插件提供的。
    pub fn is_js_view(&self, id: &str) -> bool {
        self.bridge.is_js_view(id)
    }

    /// 首批启动扫描发现的脚本插件是否已全部完成初始化。
    pub fn initial_plugins_settled(&self) -> bool {
        self.initial_ready
    }

    #[doc(hidden)]
    pub fn app_view(&self) -> &Entity<AppView> {
        &self.app
    }

    #[doc(hidden)]
    pub fn plugin_vm_ids(&self) -> Vec<String> {
        self.plugins.vm_ids()
    }

    /// 供设置「面板插件」页展示的已发现插件清单（含编译或运行错误及已登记视图）。
    pub fn installed_plugins(&self) -> Vec<LoadedPluginInfo> {
        self.loaded_plugins
            .iter()
            .cloned()
            .map(|mut info| {
                info.views = self.bridge.views_of(&info.id);
                if info.error.is_none()
                    && let Some(err) = self.runtime_errors.get(&info.id)
                {
                    info.error = Some(err.clone());
                }
                info
            })
            .collect()
    }

    /// 应用退出前清理全部 NSH 插件登记。
    pub fn shutdown(&mut self, cx: &mut Context<Self>) {
        for owner in std::mem::take(&mut self.theme_owners) {
            smelt_ui::theme_catalog::unregister_themes(&owner, cx);
        }
        let ids: Vec<String> = self
            .loaded_plugins
            .iter()
            .map(|info| info.id.clone())
            .collect();
        for id in ids {
            self.bridge.unregister_plugin(&id, cx);
        }
        self.bridge.forget_vm(crate::plugins::BUILTINS_VM_ID);
        self.plugin_roots.clear();
        self.dirty_slices = host::SliceMask::EMPTY;
        self.pending_events.clear();
    }

    fn schedule_publish(&mut self, cx: &mut Context<Self>) {
        self.schedule_publish_slices(host::SliceMask::ALL, cx);
    }

    fn schedule_publish_slices(&mut self, slices: host::SliceMask, cx: &mut Context<Self>) {
        self.dirty_slices |= slices;
        self.schedule_flush(cx);
    }

    fn enqueue_event(&mut self, event: HostEvent, cx: &mut Context<Self>) {
        push_coalesced_event(&mut self.pending_events, event);
        self.schedule_flush(cx);
    }

    fn schedule_flush(&mut self, cx: &mut Context<Self>) {
        if self.flush_scheduled {
            return;
        }
        self.flush_scheduled = true;
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = this.update(cx, |this, cx| this.flush_pending(cx));
        });
    }

    fn flush_pending(&mut self, cx: &mut Context<Self>) {
        self.flush_scheduled = false;
        let dirty = std::mem::replace(&mut self.dirty_slices, host::SliceMask::EMPTY);
        if !dirty.is_empty() {
            self.publish_dirty(dirty, cx);
        }
        let events = std::mem::take(&mut self.pending_events);
        for event in &events {
            self.bridge.forward_event(&self.plugins, event);
        }
    }

    fn recompute_shell_view(&mut self, cx: &App) {
        self.shell_view = match self.pages.iter().find(|p| p.key == self.active_page) {
            Some(entry) => compute_shell_view(
                &entry.layout,
                self.corners,
                entry.outlet.as_ref().map(|o| o.id.clone()),
                cx,
            ),
            None => ComputedShellView {
                corners: self.corners,
                ..Default::default()
            },
        };
        *self.shared_shell_view.borrow_mut() = self.shell_view.clone();
    }

    fn publish_dirty(&self, dirty: host::SliceMask, cx: &App) {
        let shell_json = dirty.contains(host::SliceMask::SHELL).then(|| {
            let pages_json: Vec<Value> = self
                .pages
                .iter()
                .map(|entry| {
                    let active = entry.key == self.active_page;
                    let outlet_id = entry.outlet.as_ref().map(|o| o.id.as_str());
                    let view = if active {
                        self.shell_view.clone()
                    } else {
                        compute_shell_view(
                            &entry.layout,
                            self.corners,
                            outlet_id.map(str::to_owned),
                            cx,
                        )
                    };
                    json!({
                        "key": entry.key,
                        "active": active,
                        "layout_rev": entry.layout_rev,
                        "layout": layout_to_json(&entry.layout.layout),
                        "panels": panel_options_json(&entry.layout, outlet_id, cx),
                        "meta": panel_meta_json(&entry.layout, outlet_id, cx),
                        "group_areas": group_areas_json(&view),
                        "outlet": entry.outlet.as_ref().map(|o| json!({
                            "id": o.id,
                            "args": o.args,
                            "seq": o.seq,
                            "managed_rust_instance": o.managed_rust_instance,
                        })),
                        "left_dock": view.left_dock,
                        "right_dock": view.right_dock,
                        "left_toggle_x": view.left_toggle_x(),
                        "zoomed": entry.layout.zoomed,
                    })
                })
                .collect();
            json!({
                "padding": self.padding,
                "has_backdrop": self.backdrop_hooks.borrow().record.is_some(),
                "pages": pages_json,
                "settings_page": self.settings_page.as_ref().map(|(id, title)| json!({
                    "id": id,
                    "title": title,
                })),
                "host_dialogs": self.host_dialogs,
                "host_settings": self.host_settings,
                "plugin_roots": self.plugin_roots.iter().cloned().collect::<Vec<_>>(),
                "labels": {
                    "collapse_left": self.labels.collapse_left,
                    "expand_left": self.labels.expand_left,
                    "collapse_right": self.labels.collapse_right,
                    "expand_right": self.labels.expand_right,
                    "zoom": self.labels.zoom,
                    "restore": self.labels.restore,
                    "settings_page_gone": self.labels.settings_page_gone,
                },
            })
        });
        let safe_areas = dirty
            .contains(host::SliceMask::SAFE_AREAS)
            .then(|| safe_areas_json(&self.shell_view));
        self.bridge
            .publish_dirty(&self.plugins, dirty, self.rem, safe_areas, shell_json, cx);
    }
}

impl Render for MainShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = f32::from(window.rem_size());
        if (self.rem - rem).abs() > f32::EPSILON {
            self.rem = rem;
            self.schedule_publish_slices(host::SliceMask::THEME, cx);
        }
        let reset = self.backdrop_hooks.borrow().reset.clone();
        let app_dirty = self.app_dirty.clone();
        div()
            .relative()
            .size_full()
            .child(
                canvas(
                    move |_, _, _| {
                        if app_dirty.replace(false)
                            && let Some(reset) = reset.as_ref()
                        {
                            reset();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_0(),
            )
            .child(self.app.clone())
    }
}

/// 纯信号型宿主事件：只提示“对应状态变了”，同一轮更新里重复到达可安全合并为一次。
fn is_signal_event(event: &HostEvent) -> bool {
    matches!(
        event,
        HostEvent::WindowActivated
            | HostEvent::OverlayChanged
            | HostEvent::MachinesChanged
            | HostEvent::ActiveContextChanged
            | HostEvent::SessionsChanged
            | HostEvent::NotificationPermissionChanged
            | HostEvent::ShownViewsChanged
            | HostEvent::LaidOutViewsChanged
            | HostEvent::ViewsChanged
            | HostEvent::ViewBadgesChanged
            | HostEvent::SafeAreaChanged
            | HostEvent::ThemesChanged
            | HostEvent::LayoutsChanged
            | HostEvent::PagesChanged
            | HostEvent::ContextMenuItemsChanged
            | HostEvent::CommandsChanged
            | HostEvent::LocaleChanged
    )
}

fn push_coalesced_event(queue: &mut Vec<HostEvent>, event: HostEvent) {
    if is_signal_event(&event) && queue.contains(&event) {
        return;
    }
    queue.push(event);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_signal_events_without_dropping_payload_events() {
        let mut queue = Vec::new();
        push_coalesced_event(&mut queue, HostEvent::SessionsChanged);
        push_coalesced_event(&mut queue, HostEvent::SessionsChanged);
        push_coalesced_event(
            &mut queue,
            HostEvent::Plugin {
                name: "demo/a".into(),
                payload: json!(1),
            },
        );
        push_coalesced_event(
            &mut queue,
            HostEvent::Plugin {
                name: "demo/a".into(),
                payload: json!(1),
            },
        );
        push_coalesced_event(&mut queue, HostEvent::MachinesChanged);
        push_coalesced_event(&mut queue, HostEvent::SessionsChanged);

        assert_eq!(queue.len(), 4);
        assert_eq!(queue[0], HostEvent::SessionsChanged);
        assert!(matches!(queue[1], HostEvent::Plugin { .. }));
        assert!(matches!(queue[2], HostEvent::Plugin { .. }));
        assert_eq!(queue[3], HostEvent::MachinesChanged);
    }
}
