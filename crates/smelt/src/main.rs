#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! smelt 工作台宿主：开主窗口，给插件提供机制和服务（视图注册表、页和路由、命令与快捷键、
//! 运行时连接和本机守护、外观、设置页的框架），自己不认识具体功能。功能都是插件，在
//! `plugin_host::install` 里登记。
//!
//! 运行： cargo run --bin smelt

// 主题判断在组件库 smelt-ui 里，这里重导出成宿主一直用的模块路径。
pub(crate) use smelt_ui::ui_theme;

mod bundled_helpers;
mod daemon_states;
mod dir_picker;
mod dock;
mod host_dialog;
mod host_views;
mod keymap;
mod local_daemon;
mod overlay;
mod plugin_host;
mod plugins;
mod router;
mod runtime_events;
mod runtimes;
pub(crate) use daemon_states::DaemonStates;
pub(crate) use runtimes::RuntimeKey;
mod settings;
mod status_item;
mod system_open;
mod window_backdrop;
mod workspace_commands;
mod workspace_frame;
mod workspace_layout;
mod workspace_palette;
mod workspace_persist;
mod workspace_prepare;
mod workspace_shell;
mod workspace_update;
mod workspace_view;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use gpui::InteractiveElement;
use gpui::*;
use smelt_client::daemon_state::DaemonStateEvent;

use settings::{Appearance, load_appearance};

/// 文件事件里有没有名叫 `name` 的文件。
fn event_touches_file(event: &notify::Event, name: &std::ffi::OsStr) -> bool {
    event
        .paths
        .iter()
        .any(|path| path.file_name() == Some(name))
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

struct DaemonStatesSubscription {
    _subscription: Subscription,
}

impl Global for DaemonStatesSubscription {}

/// 各运行时推来的工作区目录（项目、对话的名册）。唯一的 daemon 订阅循环写入：首帧整份
/// 替换，之后的推送只收版本更新的（库换过的算新的，见 `WorkspaceRevision::supersedes`）。
/// 插件经宿主接口读（见 `SmeltHost::workspace_directory`），变了发 `HostEvent::SessionsChanged`。
#[derive(Default)]
struct WorkspaceDirectoriesGlobal {
    received: HashMap<RuntimeKey, smelt_model::workspace_directory::WorkspaceDirectory>,
}

impl Global for WorkspaceDirectoriesGlobal {}

impl WorkspaceDirectoriesGlobal {
    fn receive(
        key: &RuntimeKey,
        directory: smelt_model::workspace_directory::WorkspaceDirectory,
        snapshot: bool,
        cx: &mut App,
    ) {
        cx.update_global::<Self, _>(|global, _| {
            if !snapshot
                && global
                    .received
                    .get(key)
                    .is_some_and(|current| !directory.version().supersedes(&current.version()))
            {
                return;
            }
            global.received.insert(key.clone(), directory);
        });
    }
}

// 工作台级的动作都是命令（见 `workspace_commands`），快捷键和菜单绑 `RunCommand`。这里只剩
// 意思随焦点变的 Esc，键位在 `keymap`。
// 不绑 `None`——无 context 的键位和最内层同深度，后注册会盖过 Input。
gpui::actions!(smelt, [DismissWorkspace]);

/// 当前主工作台。窗口在不在只看 `window`：主窗口关掉之后，别处（还没跑完的任务）可能
/// 还握着 `Workspace`，它仍能 `upgrade`。
#[derive(Clone)]
struct MainWindow {
    window: WindowHandle<gpui_base::Root>,
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

/// 主窗口还在就原样返回，已经关掉就再开一扇。
/// 不要在主窗口自己的 update 里调用：窗口被借用时 `is_active` 也是 `None`。
fn ensure_main_window(current: &Rc<RefCell<Option<MainWindow>>>, cx: &mut App) -> MainWindow {
    let existing = current.borrow().clone();
    if let Some(main) = existing
        && window_handle_is_open(&main.window, cx)
    {
        return main;
    }
    let main = open_workspace_window(cx);
    *current.borrow_mut() = Some(main.clone());
    main
}

/// 工作台根视图：主窗口里的页（每页一块 Dock，功能的视图都是里面的面板）、宿主自己的
/// 对话框和遮罩层、外观落到窗口上、本机守护换代。
struct Workspace {
    /// 主窗口开过的页，每页一块 Dock，视图都是里面的面板（见 `workspace_layout`、
    /// `workspace_shell`）。开着的那页总在里面。
    pages: Vec<workspace_layout::PageDock>,
    /// 主窗口的路由：开着哪页、来路、各页出口里放着什么（见 `router`）。
    router: router::Router,
    /// 上次退出时首页出口里放着的视图。视图登记好、进首页时放回去（见 `open_home_page`）。
    startup_outlet: Option<String>,
    /// 上次写进客户端库的首页出口，没变就不再写（见 `save_state`）。`None` 是这次还没写过。
    saved_outlet: Option<Option<String>>,
    /// 各页出口里放着的按实例建的视图：路由只记 id，这一份归工作台留着（见
    /// `Workspace::open_instance`），出口换了内容就放掉。
    outlet_instances: HashMap<router::PageKey, smelt_plugin_api::ViewInstance>,
    /// 开着的那页摆着什么、看得见什么的快照。Dock 一变、换页就重算，render 只读它。
    pub(crate) shell_view: workspace_shell::ShellView,
    /// 红黄绿按钮此刻占的宽度（进出全屏时变），算 [`Self::shell_view`] 用。
    traffic_lights: Pixels,
    _window_bounds_sub: Option<Subscription>,
    _layout_choice_sub: Option<Subscription>,
    /// 开着的命令面板（⌘K），见 `workspace_palette`。
    palette: Option<smelt_ui::dialog::DialogId>,
    /// 上次设到窗口上的材质种类和明暗。换材质、换主题、系统换深浅时才再发给系统。
    applied_backdrop: Option<(window_backdrop::WindowMaterial, bool)>,
    /// 上次应用到窗口的界面基准字号（rem_size）。
    applied_ui_font_px: Option<u32>,
    /// 主窗口。没有 Window 的变更用它在本轮更新结束后、绘制前刷新外观。
    window_handle: AnyWindowHandle,
    /// 已经排队一次外观刷新。同一轮里的多次变更合并成一次。
    appearance_sync_scheduled: bool,
    /// 正在刷新外观。这段里的变更不再排队第二次。
    appearance_sync_running: bool,
    /// 调试 HUD 开关，见 `overlay::debug_hud`。
    debug_hud: bool,
    /// 退出确认拦截弹窗开关
    show_quit_confirm: bool,
    /// 用户已确认退出；防止重复提交退出动作。
    quit_requested: bool,
    /// 上次告诉插件的遮罩层开关（见 `note_overlay`），只用来判断变没变。
    plugin_overlay_open: bool,
    /// 远程机器上的目录选择器：插件要在远程机器上选路径时开（见 `dir_picker`）。
    dir_picker: Option<dir_picker::DirPickerState>,
    /// 守护落后时的自动重启进行中，防止重入。
    daemon_upgrading: bool,
    /// 上次看到主窗口在不在前台（应用在前台、窗口是 key）。变成在前台时告诉插件人回来了。
    workspace_window_active: bool,
    /// 主窗口是不是 key window，窗口激活变化时和每帧记下。插件问窗口在不在前台时和现问的
    /// 应用前台合起来答（见 `SmeltHost::window_active`）。
    main_window_key: bool,
    /// 宿主转给插件的事件里，工作台自己要管的（见 `subscribe_host_events`）。
    _host_events_sub: Option<Subscription>,
    /// 每次通知后让宿主对话框跟上状态，再看遮罩层开关变没变（见 `sync_host_dialogs`、
    /// `note_overlay`）。
    _overlay_sub: Option<Subscription>,
    /// 外观全局变化。
    _appearance_sub: Option<Subscription>,
    /// 主窗口激活时告诉插件、补查守护是否落后。
    _window_activation_sub: Option<Subscription>,
    /// 系统换深浅时把窗口材质的深浅设回 smelt 的主题，见 [`Self::sync_window_backdrop`]。
    _system_appearance_sub: Option<Subscription>,
    /// 本机运行时最近一次是不是在线。从不在线变成在线，就是守护（重新）连上了。
    local_runtime_online: bool,
    /// 这个窗口见过本机运行时在线没有，见 [`Self::start_daemon_outdated_watch`]。
    local_runtime_seen_online: bool,
    /// 本机运行时连上时查守护是否落后，见 [`Self::start_daemon_outdated_watch`]。
    _local_runtime_sub: Option<Subscription>,
    /// 盯着 App 里的 smeltd 所在目录：重编译、覆盖安装后查一次守护是否落后。
    _daemon_binary_watcher: Option<notify::RecommendedWatcher>,
    /// 守护落后，但有会话在跑没重启：等会话都停下来再查一次。
    daemon_restart_waiting_idle: bool,
    /// 根节点自己的焦点句柄：有的页面自身没有可聚焦的元素，切过去后如果谁都不 focus，
    /// 窗口的 focus 仍停在切走前那个（可能已经不在当前渲染树里的）视图上——GPUI 找不到
    /// 就把 focus 兜底纠正到 window 的真正根节点，而 Workspace 的键位 context 挂在 Root
    /// 组件之下、并非那个根节点，于是匹配不到，表现为"切到别的页面后快捷键全部失灵"。
    /// 切到这类页面时把 focus 显式认领到这个句柄上，保证 Workspace context 始终在 dispatch
    /// 路径上。
    focus_handle: FocusHandle,
    /// 开着的宿主对话框，按打开先后（见 `host_dialog`）。
    host_dialogs: Vec<(host_dialog::HostDialog, smelt_ui::dialog::DialogId)>,
}

impl Workspace {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 自带命令先于插件登记：出厂键位撞了先登记的留着，自带的键不让插件抢走。
        workspace_commands::register(cx.weak_entity(), cx);
        // 面板插件在进首页之前挂好：布局和出口按视图 id 摆，注册表里没有的摆不上。
        plugins::mount_all(window, cx);

        let mut ws = Self {
            pages: Vec::new(),
            router: Default::default(),
            startup_outlet: workspace_persist::load_outlet(),
            saved_outlet: None,
            outlet_instances: HashMap::new(),
            shell_view: Default::default(),
            traffic_lights: workspace_frame::traffic_lights_width(window),
            _window_bounds_sub: None,
            _layout_choice_sub: None,
            palette: None,
            applied_backdrop: None,
            applied_ui_font_px: None,
            window_handle: window.window_handle(),
            appearance_sync_scheduled: false,
            appearance_sync_running: false,
            debug_hud: false,
            show_quit_confirm: false,
            quit_requested: false,
            plugin_overlay_open: false,
            dir_picker: None,
            daemon_upgrading: false,
            workspace_window_active: false,
            main_window_key: false,
            _host_events_sub: None,
            _overlay_sub: None,
            _appearance_sub: None,
            _window_activation_sub: None,
            _system_appearance_sub: None,
            local_runtime_online: false,
            local_runtime_seen_online: false,
            _local_runtime_sub: None,
            _daemon_binary_watcher: None,
            daemon_restart_waiting_idle: false,
            focus_handle: cx.focus_handle(),
            host_dialogs: Vec::new(),
        };
        // 宿主自己的视图（设置）和插件登记在同一张视图注册表里，布局按 id 摆它们。先登记再
        // 订阅：这几次登记不用重装布局，布局本来就还没装。
        host_views::register(cx.weak_entity(), cx);
        ws._host_events_sub = Some(ws.subscribe_host_events(window, cx));
        // 对话框和遮罩层的开关散在各处（确认框、命令面板），开关时都会通知工作台。
        ws._overlay_sub = Some(cx.observe_self(|this, cx| {
            this.sync_host_dialogs(cx);
            this.note_overlay(cx);
        }));
        // 主窗口关了，工作台里的视图就都看不见了。
        cx.on_release(|_, cx| plugin_host::set_window_shown(HashSet::new(), cx))
            .detach();
        // 进首页之前先有一页空的，render 总有 Dock 可画。
        ws.pages.push(Self::new_page_dock(None, window, cx));
        ws._layout_choice_sub = Some(Self::watch_layout_choice(window, cx));
        // 装布局要用 `cx.entity()` 背后的工作台（宿主自己的视图画它），等它建好。
        cx.defer_in(window, |this, window, cx| {
            this.open_home_page(window, cx);
        });
        ws.start_daemon_outdated_watch(cx);
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            this.note_window_activation(window, cx);
            if window.is_window_active() {
                this.check_daemon_outdated_on_event(cx);
            }
        });
        ws._window_activation_sub = Some(activation);
        // 进出全屏时红黄绿按钮出现、消失，贴着左上角的视图跟着让位。全屏切换会改窗口大小。
        ws._window_bounds_sub = Some(cx.observe_window_bounds(window, |this, window, cx| {
            let lights = workspace_frame::traffic_lights_width(window);
            if lights != this.traffic_lights {
                this.traffic_lights = lights;
                this.sync_shell_view(cx);
            }
        }));
        // Windows 上系统换深浅时，GPUI 先通知这里，再把窗口的深浅标记改回跟系统，Mica 和
        // 系统标题栏就跟主题对不上了。所以等这条系统消息处理完，再按主题设一次。
        ws._system_appearance_sub =
            Some(cx.observe_window_appearance(window, |this, window, cx| {
                this.applied_backdrop = None;
                cx.spawn_in(window, async move |this, cx| {
                    this.update_in(cx, |this, window, cx| this.sync_window_backdrop(window, cx))
                        .ok();
                })
                .detach();
            }));
        ws._appearance_sub = Some(
            cx.observe_global_in::<Appearance>(window, |this, _window, cx| {
                this.schedule_appearance_sync(cx)
            }),
        );
        // 首帧绘制前就把窗口材质和界面字号对上。观察器只在之后的外观变化时再排一次。
        ws.sync_appearance(window, cx);
        ws
    }

    /// 记下主窗口在不在前台。从不在前台变成在前台（人回来了）时，刷新系统通知授权、立刻探
    /// 一次离线的远程机器，再告诉插件。
    pub(crate) fn note_window_activation(&mut self, window: &Window, cx: &mut Context<Self>) {
        let application_active = status_item::is_app_active();
        smelt_ui::motion::set_ambient_application_active(cx, application_active);
        self.main_window_key = window.is_window_active();
        let active = application_active && self.main_window_key;
        let became_active = active && !self.workspace_window_active;
        self.workspace_window_active = active;
        if became_active {
            status_item::refresh_system_notification_authorization();
            // 离线的远程机器隔几分钟才探一次；人回到这里时多半要用，立刻探一次。
            crate::runtime_events::probe_remotes(cx);
            // 监听收不到的变化（监听登记失败、旧运行时）由插件在人回来时补上。这里可能在
            // 绘制路径上，事件挪到这一帧之后发。
            cx.defer(|cx| {
                smelt_plugin_api::emit(smelt_plugin_api::HostEvent::WindowActivated, cx);
            });
        }
    }

    /// 守护是否落后于 App 里的 smeltd 没有专门的「更新事件」，下面这几件事发生时各查一次；
    /// 探到落后交给 `check_daemon_outdated`：没有进行中的会话才自动重启，有的话等会话都
    /// 停下来再查（见 [`Self::retry_daemon_restart_when_idle`]）。
    /// - App 里的 smeltd 变了（dev 重编译、`make install`、覆盖安装）：盯着它所在的目录；
    /// - 主窗口激活（在 `new` 里挂）：整个 `.app` 被换掉时换的是上一级目录项，目录监听
    ///   跟不上，人切回来时补查；
    /// - 本机运行时连上：这个窗口第一次看到它在线时照常查（启动时还在跑的旧守护在这里换掉）；
    ///   之后的重新连上多半就是一次换代的结果，只刷新提示，不在这里重启——换完还落后时再接着
    ///   重启，会和重连互相触发、停不下来。
    fn start_daemon_outdated_watch(&mut self, cx: &mut Context<Self>) {
        let reconnects = cx.observe_global::<crate::runtimes::RuntimeHub>(|this, cx| {
            let online = crate::runtimes::runtime_online(&RuntimeKey::Local, cx);
            if online && !this.local_runtime_online {
                let first = !this.local_runtime_seen_online;
                this.local_runtime_seen_online = true;
                this.probe_daemon_outdated(first, cx);
            }
            this.local_runtime_online = online;
        });
        self._local_runtime_sub = Some(reconnects);
        self._daemon_binary_watcher = Self::watch_bundled_daemon(cx);
    }

    /// 盯着分发物 smeltd 所在的目录，只认 smeltd 这个名字上的变化。
    fn watch_bundled_daemon(cx: &mut Context<Self>) -> Option<notify::RecommendedWatcher> {
        use notify::Watcher as _;
        /// 拷贝、链接一个映像会连着来一串事件：静下来这么久才算写完，免得哈希半截文件、
        /// 拿半截映像去换守护。
        const QUIET: Duration = Duration::from_secs(2);

        let dir = local_daemon::bundled_daemon_dir()?;
        let name = std::ffi::OsString::from(local_daemon::bundled_daemon_file_name());
        // 容量 1：一串事件只需要一次唤醒。
        let (changed_tx, changed_rx) = smol::channel::bounded::<()>(1);
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if event.is_ok_and(|event| event_touches_file(&event, &name)) {
                let _ = changed_tx.try_send(());
            }
        });
        let mut watcher = match watcher {
            Ok(watcher) => watcher,
            Err(error) => {
                eprintln!("[daemon] 监听 {} 失败：{error}", dir.display());
                return None;
            }
        };
        if let Err(error) = watcher.watch(&dir, notify::RecursiveMode::NonRecursive) {
            eprintln!("[daemon] 监听 {} 失败：{error}", dir.display());
            return None;
        }
        // 监听器随 Workspace 丢掉时发送端跟着没了，循环随之结束。
        cx.spawn(async move |this, cx| {
            while changed_rx.recv().await.is_ok() {
                loop {
                    let quiet = cx.background_executor().timer(QUIET);
                    let changed = async { changed_rx.recv().await.is_ok() };
                    let settled = async {
                        quiet.await;
                        false
                    };
                    if !smol::future::or(changed, settled).await {
                        break;
                    }
                }
                if this
                    .update(cx, |this, cx| this.check_daemon_outdated_on_event(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Some(watcher)
    }

    /// 事件触发的落后检查。本机运行时连上之前不查：拉起守护（见 `runtimes::local_status`）
    /// 可能还没做完，不能和它并行换守护；连上时自己会查一次。
    fn check_daemon_outdated_on_event(&mut self, cx: &mut Context<Self>) {
        if self.local_runtime_online {
            self.check_daemon_outdated(cx);
        }
    }

    /// 守护落后、因为有会话在跑没重启的话，会话状态一变就看看是不是都停下来了，停了就再查
    /// 一次（查到还落后就重启）。只看内存里的会话状态。
    pub(crate) fn retry_daemon_restart_when_idle(&mut self, cx: &mut Context<Self>) {
        if self.daemon_restart_waiting_idle && !self.local_has_busy_work(cx) {
            self.daemon_restart_waiting_idle = false;
            self.check_daemon_outdated(cx);
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 工作台每帧都会重跑这里：debug 构建下，期间读写 SQLite 会被报出来。
        let _render_scope = smelt_client_store::render_guard::RenderScope::enter();
        let _timing = smelt_ui::frame_stats::RenderTiming::start("Workspace");
        // 帧上只认前台。窗口材质、界面字号在外观变化时刷新。
        self.prepare_frame(window, cx);

        // 整窗只在这里管底，有窗口材质时不铺、拿毛玻璃做底；里面的分栏和视图不铺页面底。
        // 背景图也在这一层，铺满整扇窗口，压在材质上、所有内容下面。
        let shell_surface = workspace_frame::window_surface();
        let shell_padding = ui_theme::shell_padding();
        let appearance = cx.global::<Appearance>();
        let background_image = appearance.bg_image.as_deref().map(|path| {
            workspace_frame::background_image_layer(path, appearance.bg_image_opacity, cx)
        });

        // 壳层走界面字体（默认系统 UI）；终端/diff/代码块各自绑代码字体。
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(shell_surface)
            .children(background_image)
            .font_family(settings::resolved_ui_font_family(
                cx.global::<Appearance>(),
            ))
            // 见 focus_handle 字段注释：非终端页面没有可聚焦的子元素时，靠这个把
            // window 的 focus 兜底钉在这层，保证 Workspace 键位 context 在 dispatch 路径上。
            .track_focus(&self.focus_handle)
            // 挂在这一层才浅于 Terminal / Input。
            .key_context("Workspace")
            // 快捷键、应用菜单、右键菜单执行命令都走这里，见 `workspace_commands`。
            // 命令眼下不适用（`Pass`）就交给下一条绑定，失败了报出来。
            .on_action(|action: &smelt_plugin_api::RunCommand, window, cx| {
                let handled = smelt_plugin_api::invoke(&action.command, &action.args, Some(window), cx);
                if workspace_commands::finish(handled, cx) {
                    cx.propagate();
                }
            })
            // Esc：还原放大的面板组。对话框、命令面板在窗口根的对话框层上，自己处理 Esc；
            // 自己要 Esc 的视图（终端里的 vim 等）在更深的 context 上绑走，都到不了这里。
            .on_action(cx.listener(|this, _: &DismissWorkspace, window, cx| {
                if this.unzoom_layout(window, cx) {
                    return;
                }
                cx.propagate();
            }))
            // 开着的那页是一块 Dock，功能的视图和设置都是里面的面板。
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .p(shell_padding)
                    .child(self.shell_dock().clone())
                    .children(self.render_dock_toggles(shell_padding)),
            )
            // 调试 HUD 叠在内容上面；对话框和命令面板在窗口根的对话框层上。
            .child(self.render_overlay_layers(window, cx))
    }
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

/// 开一扇主工作台窗口（Workspace + Root 包装）。
/// 首启和「点 Dock 图标重开」共用这一份：`Workspace::new` 本来就会从存档重新摆出布局，
/// 功能的状态在插件和运行时里，跟正常重启应用效果一致。
/// 返回值同时留下窗口句柄：主窗口还在不在看它，不看 Workspace 还能不能升级。
fn open_workspace_window(cx: &mut App) -> MainWindow {
    let window_options = workspace_frame::workspace_window_options(
        ui_theme::shell_padding(),
        cx.global::<Appearance>().material,
    );
    let mut workspace = None;
    let window_handle = cx
        .open_window(window_options, |window, cx| {
            // 界面文字（侧边栏/标签页/状态栏等）用的都是 text_xs/text_sm 这类相对 rem
            // 单位。根据外观设置里的 ui_font_px（默认 16px）统一设置，全局跟着等比例缩放。
            // 终端内容的字号归终端插件自己管，不受这个影响。
            let ui_font_px = cx.global::<Appearance>().ui_font_px;
            window.set_rem_size(px(ui_font_px as f32));
            let view = cx.new(|cx| Workspace::new(window, cx));
            workspace = Some(view.downgrade());
            // 顶层视图包一层 Root：smelt-ui 的对话框、菜单这些窗口层都登记在它上面。
            cx.new(|cx| gpui_base::Root::new(view, window, cx).bg(gpui::transparent_black()))
        })
        .expect("打开窗口失败");
    MainWindow {
        window: window_handle,
        workspace: workspace.expect("回调里一定会设置 workspace"),
    }
}

/// gpui-kit 的图标集里没有的几枚图标（agent 身份、git 分支、机器系统等）由 smelt 自带，走
/// `smelt-icons/` 前缀，不会跟 gpui-kit 以后新增的资源撞名。插件按路径取用。
/// agent 身份图标：资源名按稳定的存档 id 拼（`smelt-icons/agent-<id>.svg`），
/// 所以这里也按 id 建表，而不是一串手写的 `if path == ...`。
///
/// 那串 if 链是「加一家 agent 要改的第 N 个地方」里最容易漏的一个：漏了不报错、
/// 不 panic，只是图标那一格空着。查表 + 遍历所有 agent 的不变量测试，把「漏登记」从
/// 上线后才发现变成编译测试失败。
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
    ("omp", include_bytes!("../assets/icons/agent-omp.svg")),
    ("crush", include_bytes!("../assets/icons/agent-crush.svg")),
    // 通用 ACP 的自定义 Agent 名字为空时的兜底；有名字时画首字徽标。
    ("custom", include_bytes!("../assets/icons/agent-custom.svg")),
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

// 图标用 gpui-kit 的全套目录（`AllAssets`，Lucide 全部 + gpui-kit 自己的），不只打包组件
// 用到的那一百来个：插件（包括用户写的 JS 插件）按 Lucide 名字取图标，只编一部分进来，没编进来
// 的图标位置就空着。全套 SVG 约 0.7 MB。
struct SmeltAssets;

impl gpui::AssetSource for SmeltAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if let Some(bytes) = agent_icon_svg(path) {
            return Ok(Some(std::borrow::Cow::Borrowed(bytes)));
        }
        if path == "smelt-icons/git-branch.svg" {
            return Ok(Some(std::borrow::Cow::Borrowed(
                include_bytes!("../assets/icons/git-branch.svg").as_slice(),
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
        if path == "smelt-icons/machine-linux.svg" {
            return Ok(Some(std::borrow::Cow::Borrowed(
                include_bytes!("../assets/icons/machine-linux.svg").as_slice(),
            )));
        }
        if path == "smelt-icons/machine-windows.svg" {
            return Ok(Some(std::borrow::Cow::Borrowed(
                include_bytes!("../assets/icons/machine-windows.svg").as_slice(),
            )));
        }
        // 停靠区开合按钮的展开态：分隔线外侧加实心色块。自带的 panel-left / panel-right
        // 只有一根细线，18px 下开合两态分不清。
        if path == "smelt-icons/panel-left-filled.svg" {
            return Ok(Some(std::borrow::Cow::Borrowed(
                include_bytes!("../assets/icons/panel-left-filled.svg").as_slice(),
            )));
        }
        if path == "smelt-icons/panel-right-filled.svg" {
            return Ok(Some(std::borrow::Cow::Borrowed(
                include_bytes!("../assets/icons/panel-right-filled.svg").as_slice(),
            )));
        }
        gpui_kit_assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<gpui::SharedString>> {
        gpui_kit_assets::AllAssets.list(path)
    }
}

fn main() {
    if let Some(code) = smelt_cli::maybe_run(std::env::args()) {
        std::process::exit(code);
    }
    // 整个 GUI 生命周期持共享 lease。installer 只有等所有 GUI 退出并拿到独占 lease
    // 后才可交换 Bundle；若交换已经开始，新 GUI fail-fast，避免从旧 vnode 启动。
    // 这把锁保护的是 macOS .app 交换，其它平台没有安装事务（设计文档 D4）。
    #[cfg(target_os = "macos")]
    let _app_runtime_lease = match smelt_client::updater::acquire_app_runtime_lease() {
        Ok(lease) => lease,
        Err(error) => {
            eprintln!("Smelt 正在更新，暂时无法启动：{error:#}");
            std::process::exit(75);
        }
    };
    // Windows 安装程序靠它认出 Smelt 还开着，覆盖装和卸载前先请用户关掉。
    #[cfg(windows)]
    smelt_sys::mark_desktop_running();
    // GUI 启动失败时用户通常看不到终端输出；把原始 panic 先落到统一日志，
    // 尤其能保留 GPUI 清理阶段二次 panic 之前的第一条错误。
    smelt_sys::app_log::install_panic_hook("smelt");
    smelt_sys::app_log::tee_stderr("smelt");
    // 插件的 console 输出和脚本错误是 tracing 事件，不接就看不见。
    plugins::install_log();
    // Finder / launchd 启动的 GUI 默认软上限常是 256；在创建窗口、socket 和终端
    // 线程之前先抬高，避免正常多会话也让更新检查等新连接触发 EMFILE。
    smelt_sys::fd_limit::raise_fd_limit();
    // with_assets 注册图标资源，IconName 的 svg 才能渲染；见上面 SmeltAssets。
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

    // 当前主窗口。窗口在不在看 WindowHandle，不看 Workspace 弱句柄：主窗口关掉后，
    // 还没跑完的任务可能还握着那份实体，upgrade 仍然成功。
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
        // gpui-base：输入框、富文本、滚动条、分栏、窗口根这些行为层和它们的键位。
        gpui_base::init(cx);
        // smelt 自己的组件：菜单键位、窗口对话框层（挂在窗口根上，要在开窗口之前装）。
        smelt_ui::init(cx);
        // 消息里点中的文件链接交给登记的文件打开者。链接带着文件所在的机器：远程的就去
        // 那台机器读，不能打开本机同路径的文件。
        smelt_ui::markdown_links::set_file_link_handler(
            |target, cx| {
                let location = smelt_plugin_api::FileLocation {
                    machine: target.runtime.map_or(RuntimeKey::Local, RuntimeKey::Remote),
                    path: target.path,
                    line: target.line.map(|line| line as usize),
                };
                workspace_commands::report_failure(smelt_plugin_api::open_file(location, cx), cx);
            },
            cx,
        );
        // 内嵌终端默认字体 Maple Mono NF（Regular/Bold，v7.9）。Ghostty 同款思路：
        // 默认字体自己带，不赌用户装没装——任何机器上默认字体族都能解析成功，
        // 杜绝"没装字体 → 测量/渲染各自 fallback 到不同字体 → 列宽错乱"。它是打过
        // Nerd Font 补丁的完整版，自带全部图标码位，兼任图标 fallback（用户在设置页
        // 自选的字体缺图标时落到它，见终端插件的 `terminal_font`）。
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
        // 外观设置：读盘设为全局单例。
        // 主题模式在建窗口之前落地，首帧就是对的，不会先闪一下深色再变浅。
        // 主题先登记再恢复外观，插件主题在首帧前即可生效。
        smelt_themes::register(cx);
        plugins::register_initial_themes(cx);
        let appearance = load_appearance();
        cx.set_global(appearance.clone());
        smelt_ui::set_code_font(&appearance.font_family, cx);
        settings::apply_named_theme(&appearance.theme_name, cx);

        // 原生通知/菜单栏桥必须先于任何 Attention producer 就绪。daemon 快照可能在
        // 首帧前到达；若先启动观察器再 setup，应用级消费者会正确 drain 事件，但
        // UNUserNotificationCenter 尚无 delegate/队列，通知仍会在启动竞态里丢失。
        status_item::setup(status_tx);

        // 状态通道：每个运行时一条常驻 subscribe（见 `runtime_events`），把会话状态写进
        // DaemonStates、工作区目录写进 WorkspaceDirectoriesGlobal，插件经宿主接口读。
        cx.set_global(DaemonStates::default());
        cx.set_global(WorkspaceDirectoriesGlobal::default());
        let current_ws_for_daemon_states = current_ws.clone();
        let daemon_states_subscription = cx.observe_global::<DaemonStates>(move |cx| {
            if let Some(ws) = current_workspace(&current_ws_for_daemon_states) {
                let _ = ws.update(cx, |ws, cx| ws.retry_daemon_restart_when_idle(cx));
            }
        });
        cx.set_global(DaemonStatesSubscription {
            _subscription: daemon_states_subscription,
        });
        // 先把 App 内最新版 smelt-notify 同步到稳定的 managed 路径，智能体的 hook 命令指着它。
        // 失败时保留旧 helper，不阻塞 GUI。
        if let Err(error) = bundled_helpers::sync_bundled_smelt_notify() {
            eprintln!("[workspace] 同步 smelt-notify 失败：{error}");
        }
        // 登记过的远程运行时：同步读客户端库，iroh 推迟到第一次连接时在后台绑。
        runtimes::start_runtime_hub(cx);
        // 插件要读机器列表，在 RuntimeHub 之后、开主窗口之前装。
        plugin_host::install(current_ws.clone(), cx);
        // 快捷键（命令的出厂键位 + 用户改过的）和应用菜单栏，见 `keymap`。要订阅宿主事件，
        // 在插件宿主之后装；命令随后由工作台和插件登记，登记一次重装一次键表。
        keymap::init(cx);
        // 每个运行时一条常驻 subscribe。手机建的会话目录、自动化、外观请求这些投影
        // 目前只认本机；会话状态、项目文件变化、智能体记忆变化各运行时都有。
        let runtime_events::RuntimeEventReceivers {
            states: daemon_state_rx,
            topics: topic_rx,
        } = runtime_events::start(cx);
        // 单独订阅的话题，原样转给插件。
        cx.spawn(async move |cx| {
            use smelt_client::daemon_state::TopicMessage;
            while let Ok((runtime, topic, message)) = topic_rx.recv().await {
                let message = match message {
                    TopicMessage::Event { payload, .. } => {
                        smelt_plugin_api::TopicMessage::Event(payload)
                    }
                    TopicMessage::Snapshot { payload, .. } => {
                        smelt_plugin_api::TopicMessage::Snapshot(payload)
                    }
                    TopicMessage::Lagged => smelt_plugin_api::TopicMessage::Lagged,
                    TopicMessage::Unavailable(reason) => {
                        smelt_plugin_api::TopicMessage::Unavailable(reason)
                    }
                };
                cx.update(|cx| {
                    smelt_plugin_api::emit(
                        smelt_plugin_api::HostEvent::RuntimeTopic {
                            machine: runtime,
                            topic: topic.to_string(),
                            message,
                        },
                        cx,
                    )
                });
            }
        })
        .detach();
        cx.spawn(async move |cx| {
            while let Ok((runtime, event)) = daemon_state_rx.recv().await {
                let is_local = runtime == RuntimeKey::Local;
                // v7 旧运行时仍用无回执的外观广播；本机桌面继续按原协议处理。
                if let DaemonStateEvent::AppearanceChangeRequested { settings } = &event {
                    if is_local {
                        let result = cx.update(|cx| settings::apply_requested_appearance(settings, cx));
                        if let Err(error) = result {
                            crate::status_item::notify_error(format!("外观修改失败：{error}"));
                        }
                    }
                    continue;
                }
                if let DaemonStateEvent::AppearanceApplyRequested { request_id, settings } = &event {
                    if is_local {
                        let result = cx.update(|cx| settings::apply_requested_appearance(settings, cx));
                        let outcome = match result {
                            Ok(()) => smelt_event_protocol::AppearanceApplyOutcome::Applied,
                            Err(smelt_model::appearance_settings::SettingsError::Invalid(message)) => {
                                smelt_event_protocol::AppearanceApplyOutcome::Invalid { message }
                            }
                            Err(smelt_model::appearance_settings::SettingsError::Store(message)) => {
                                smelt_event_protocol::AppearanceApplyOutcome::Failed { message }
                            }
                        };
                        let receipt = smelt_event_protocol::AppearanceApplyReceipt {
                            request_id: request_id.clone(),
                            outcome,
                        };
                        cx.background_executor().spawn(async move {
                            let request = serde_json::json!({
                                "op": smelt_runtime_client::protocol::daemon::DaemonOperation::AppearanceReceipt,
                                "receipt": receipt,
                            });
                            if let Err(error) = smelt_client::daemon_state::local_runtime().request_ok(
                                &request, std::time::Duration::from_secs(4),
                            ) {
                                eprintln!("[appearance] 发送外观修改回执失败：{error}");
                            }
                        }).detach();
                    }
                    continue;
                }
                cx.update(|cx| match event {
                    DaemonStateEvent::Snapshot {
                        sessions,
                        workspace,
                        ..
                    } => {
                        // 重新订阅上：插件据「重新连上」重取自己的（运行时换了进程，它替插件建的
                        // 监听也没了）。
                        smelt_plugin_api::emit(
                            smelt_plugin_api::HostEvent::Reconnected(runtime.clone()),
                            cx,
                        );
                        // 工作区目录首帧整份替换，之后只收版本更新的。
                        if let Some(directory) = workspace {
                            WorkspaceDirectoriesGlobal::receive(&runtime, directory, true, cx);
                        }
                        DaemonStates::replace_all(&runtime, sessions, cx);
                    }
                    DaemonStateEvent::Update(state) => DaemonStates::upsert(&runtime, state, cx),
                    DaemonStateEvent::Removed { id } => DaemonStates::remove(&runtime, &id, cx),
                    DaemonStateEvent::Workspace(directory) => {
                        WorkspaceDirectoriesGlobal::receive(&runtime, directory, false, cx)
                    }
                    // 项目文件在报事件的那台机器上。
                    DaemonStateEvent::ProjectChanged { root } => smelt_plugin_api::emit(
                        smelt_plugin_api::HostEvent::ProjectChanged {
                            machine: runtime.clone(),
                            root,
                        },
                        cx,
                    ),
                    DaemonStateEvent::Disconnected => {
                        DaemonStates::mark_subscription_disconnected(&runtime, cx)
                    }
                    // 外观请求上面已经处理。
                    DaemonStateEvent::AppearanceApplyRequested { .. }
                    | DaemonStateEvent::AppearanceChangeRequested { .. } => {}
                    // 桌面不在核心订阅里收自动化投影（归自动化插件），也不订唤醒请求和终端进度
                    // （见 `SubscriptionShape::DESKTOP`）。
                    DaemonStateEvent::Automations(_)
                    | DaemonStateEvent::ConversationWakeRequested { .. }
                    | DaemonStateEvent::TerminalProgress { .. }
                    | DaemonStateEvent::TerminalNotification { .. } => {}
                });
            }
        })
        .detach();

        // 会话不需要在退出时做任何事：终端和 agent 子进程都是 smeltd 托管的，GUI 这边只是
        // 个薄客户端，Cmd+Q 直接杀整个 GUI 进程也不会带走子进程。远程访问同理跑在 smeltd
        // 里，GUI 退出不影响远程设备。
        // 首启主窗口，记入 current_ws（reopen 回调 / URL 投递循环都靠它判断当前主窗口）。
        *current_ws.borrow_mut() = Some(open_workspace_window(cx));

        // 退出时的收尾。订阅必须由 app global 持有，否则 app.run 的初始化闭包返回时会被
        // Subscription::drop 自动取消。
        let quit_subscription = cx.on_app_quit(move |cx| {
            // 插件要在这里释放，别留到线程局部变量析构时（见 `plugins::shutdown`）。
            plugins::shutdown(cx);
            async {}
        });
        cx.set_global(AppQuitSubscription {
            _subscription: quit_subscription,
        });

        // 消费 Dock / Finder 投递的路径：交给插件（`HostEvent::OpenPaths`，怎么开由插件定）。
        // 循环常驻到应用退出。主窗口没了先重开，路径要开在用户看得见的地方。
        cx.spawn(async move |cx| {
            while let Ok(urls) = url_rx.recv().await {
                let paths: Vec<String> = urls
                    .iter()
                    .filter_map(|u| file_url_to_path(u))
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect();
                if paths.is_empty() {
                    continue;
                }
                cx.update(|cx| {
                    ensure_main_window(&current_ws, cx);
                    smelt_plugin_api::emit(
                        smelt_plugin_api::HostEvent::OpenPaths {
                            machine: RuntimeKey::Local,
                            paths,
                        },
                        cx,
                    );
                });
            }
        })
        .detach();

        // 菜单栏下拉菜单和系统通知发回的事件：权限变化推给插件，带的命令照常执行，开不开
        // 窗口由命令自己定。
        cx.spawn(async move |cx| {
            let mut notification_permission = status_item::notification_permission();
            while let Ok(event) = status_rx.recv().await {
                match event {
                    status_item::StatusItemEvent::SystemNotificationStateChanged => {
                        status_item::sync_system_notifications();
                        let notification_status = status_item::system_notification_status();
                        for error in status_item::take_system_notification_errors() {
                            eprintln!(
                                "system notification ({:?}, {} pending): {error}",
                                notification_status.authorization,
                                notification_status.pending_count
                            );
                        }
                        let permission = status_item::notification_permission();
                        if permission != notification_permission {
                            notification_permission = permission;
                            cx.update(|cx| {
                                smelt_plugin_api::emit(
                                    smelt_plugin_api::HostEvent::NotificationPermissionChanged,
                                    cx,
                                )
                            });
                        }
                    }
                    status_item::StatusItemEvent::RunCommand { command, args } => {
                        cx.update(|cx| {
                            let result = smelt_plugin_api::run(&command, args, cx);
                            workspace_commands::report_failure(result, cx);
                        });
                    }
                }
            }
        })
        .detach();
    });
}

#[cfg(test)]
mod main_tests;
