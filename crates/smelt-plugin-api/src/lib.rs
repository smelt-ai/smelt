//! 插件和宿主之间唯一的接口。
//!
//! 宿主（`smelt` 主程序）启动时实现 [`Host`]，用 [`install`] 装成全局；插件用 [`host`] 取。
//! 插件 crate 只依赖本 crate 和 gpui 组件库：要用的协议类型、运行时客户端、共用界面经
//! 这里再导出（[`model`]、[`client`]、[`ui`]）。设计见 `docs/app-plugins.md`。
//!
//! 宿主到插件的通知是 gpui 事件（[`HostEvent`]），插件用 `cx.subscribe(&events(cx), …)`
//! 订阅，不轮询。

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{AnyWindowHandle, App, AppContext, Entity, EventEmitter, Global, Task};
use serde::{Deserialize, Serialize};
use smelt_runtime_client::RuntimeClient;
use smelt_runtime_client::protocol::control::{ControlCall, ControlClientError, ControlErrorCode};

pub use serde_json;
// 一台机器的运行时：哪台 + 连它守护的客户端。
pub use smelt_client::runtime::{Runtime, RuntimeKey};
// 发给运行时的短请求（守护协议），如改历史会话的名字。
pub use smelt_client::session_ops;
// 插件自己的库：每个来源一个 SQLite 文件，表由插件自己建、自己迁移。
pub use smelt_client_store::{PluginStore, plugin_store, rusqlite};
pub use smelt_model as model;
// 本机数据目录（`~/.smelt`）在哪。
pub use smelt_paths as paths;
pub use smelt_runtime_client as client;
// 量进程的 CPU 和内存（桌面进程自己在这里量，会话的由运行时报）。
pub use smelt_sys::process_usage;
// 应用日志（`app.log`）和在后台线程里跑一个要 tokio 的 future。
pub use smelt_sys::{app_log, block_on};
// 这个构建的版本号。
pub use smelt_version;
// 桌面的在线更新（macOS 换 .app 包，Windows 拉起安装程序），和 macOS 上 App 放的位置。
#[cfg(target_os = "macos")]
pub use smelt_client::app_location;
#[cfg(any(target_os = "macos", windows))]
pub use smelt_client::updater;
pub use smelt_ui as ui;
// 主题目录：内置主题和第三方主题按来源登记、卸下，同一套接口（见 smelt_ui::theme_catalog）。
pub use smelt_ui::theme_catalog::{
    ThemeInfo, ThemeRegisterError, ThemeSwatch, apply as apply_theme, contains as theme_contains,
    swatch as theme_swatch, themes,
};

// 接口库自己的界面文案（docs/i18n.md）。
rust_i18n::i18n!("locales", fallback = "en");

mod command;
mod connections;
mod context;
mod context_menu;
mod event;
mod file;
mod layout;
mod locale;
mod menu;
mod online;
mod page;
mod quit;
mod safe_area;
mod settings;
mod status_menu;
mod topic;
mod view;

#[cfg(feature = "js")]
pub mod js;

pub use command::{
    Command, CommandHandler, CommandInfo, CommandResult, CommandSpec, Handled, RunCommand,
    arg_fields, command, commands, invoke, no_main_window, register_commands, run,
    unregister_commands,
};
pub use connections::{
    ADD_MACHINE, AddMachine, AddedMachine, Connection, ConnectionLink, ConnectionState,
    Connections, LinkRoute, MachineTarget, RECONNECT_MACHINE, REFRESH_CONNECTIONS, REMOVE_MACHINE,
    RENAME_MACHINE, RenameMachine, SET_MACHINE_RELAY, SetMachineRelay, UPGRADE_MACHINE,
};
pub use context::{ActiveContext, ViewedSession, active_context, set_active_context};
pub use context_menu::{
    ContextMenuAction, ContextMenuItem, context_menu_args, context_menu_items,
    register_context_menu_items, unregister_context_menu_items,
};
pub use event::Event;
pub use file::{FileLocation, open_file, register_file_opener, unregister_file_opener};
pub use layout::{
    DockAreaState, LAYOUT_VERSION, LayoutInfo, LayoutMaterials, LayoutSource, Material, PanelInfo,
    PanelState, STACK_PANEL, Stack, StackAxis, StackContent, StackItem, layout, layout_stacks,
    layouts, parse_layout, register_layouts, unregister_layouts,
};
pub use locale::{
    FALLBACK_LOCALE, LOCALES, Text, locale, set_locale, supported_locale, system_locale,
};
pub use menu::{MenuEntry, MenuSlot, menu_items, register_menu_items, unregister_menu_items};
pub use online::wait_online;
pub use page::{
    OPEN_PAGE, OUTLET_VIEW, OpenPage, PageInfo, PageSource, home_page, page, pages, register_pages,
    unregister_pages,
};
pub use quit::{QUIT_NOW, QuitGuard, quit_guarded, register_quit_guard, unregister_quit_guard};
pub use safe_area::{Adapt, SafeArea, safe_area, set_safe_area};
pub use settings::{SettingsEntry, SettingsGroup};
pub use status_menu::{
    StatusIcon, StatusImage, StatusMenu, StatusMenuItem, observe_status_menus, set_status_menu,
    status_menus,
};
pub use topic::{register_topics, subscribed_topics, unregister_topics};
pub use view::{
    OPEN_VIEW, OpenView, PluginView, SET_VIEW_BADGE, ViewBadge, ViewBuilder, ViewHandle, ViewInfo,
    ViewInstance, ViewSpec, built_view, create_view, ensure_view, is_registered, register_views,
    set_view_badge, slot_views, unregister_views, view, view_badge, views,
};

/// 桌面挂着的一台机器。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    pub key: RuntimeKey,
    pub name: String,
    /// 眼下用不了的原因（离线、版本不对），给人看的短语；能用是 `None`。
    pub unavailable: Option<String>,
}

/// 一个项目：在哪台机器上、根目录（那台机器上的路径）。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Project {
    pub machine: RuntimeKey,
    pub root: String,
}

// ---------------------------------------------------------------- 宿主的动作
//
// 宿主做的事都是命令（见 [`Command`]），和插件互调、快捷键、命令面板走同一条路。

/// 发送系统通知；是否显示由系统的通知权限和设置决定。
pub const NOTIFY: Command<Notify, ()> = Command::new("smelt.app/notify");

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notify {
    /// 给了 id 的通知留在通知中心里由插件管：同一个 id 再发换掉上一条，用
    /// [`WITHDRAW_NOTIFICATION`] 撤回，应用重启后也认得。id 以插件 id 开头（`<插件 id>/…`），
    /// 免得和别的插件撞上。不给是一次性的提示，投递完就不管了。
    #[serde(default)]
    pub id: Option<String>,
    pub subtitle: String,
    pub body: String,
    /// 点这条通知时执行的命令。不给就只是把应用拿到前面来。
    #[serde(default)]
    pub on_click: Option<NotificationClick>,
}

/// 点通知执行的命令。存在通知里，应用重启后点也照样执行。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationClick {
    pub command: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

/// 撤回 [`NOTIFY`] 发过的、给了 id 的通知：还没投递的不再投递，通知中心里的撤掉。没有这条
/// 通知也不算错。
pub const WITHDRAW_NOTIFICATION: Command<WithdrawNotification, ()> =
    Command::new("smelt.app/withdraw-notification");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WithdrawNotification {
    pub id: String,
}

/// 用系统文本编辑器打开这台电脑上的文件。
pub const OPEN_IN_TEXT_EDITOR: Command<OpenInTextEditor, ()> =
    Command::new("smelt.app/open-in-text-editor");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenInTextEditor {
    /// 这台电脑上的绝对路径。
    pub path: PathBuf,
}

/// 在某台机器上选路径。本机弹系统选择框，远程机器由宿主画一个浏览那台机器的选择框。
/// 取消了结果是 `None`。
pub const PICK_PATHS: Command<PathRequest, Option<Vec<String>>> =
    Command::new("smelt.app/pick-paths");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathRequest {
    /// 在哪台机器上选。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 选择框标题，如「选择要导入的插件」。
    pub title: String,
    /// 确认按钮上的动词，如「导入」。
    pub confirm: String,
    /// 除了目录还能选文件。
    #[serde(default)]
    pub files: bool,
    #[serde(default)]
    pub multiple: bool,
}

/// 把主窗口拿到前面来；已经关了就新开一扇。
pub const SHOW_MAIN_WINDOW: Command<(), ()> = Command::new("smelt.app/show-main-window");

/// 打开系统设置里本应用的通知设置，用户在那里开关通知权限。只有 macOS 有，别的系统什么
/// 都不做。
pub const OPEN_NOTIFICATION_SETTINGS: Command<(), ()> =
    Command::new("smelt.app/open-notification-settings");

/// 还没问过用户（[`NotificationPermission::NotRequested`]）时请系统问一次，不必等第一条通知。
/// 问过了、别的系统上什么都不做；结果从 [`HostEvent::NotificationPermissionChanged`] 来。
pub const REQUEST_NOTIFICATION_PERMISSION: Command<(), ()> =
    Command::new("smelt.app/request-notification-permission");

/// 系统通知的授权（[`Host::notification_permission`]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationPermission {
    /// 正在读系统设置。
    Checking,
    /// 还没问过用户；第一次发通知时系统会问，也可以用 [`REQUEST_NOTIFICATION_PERMISSION`] 先问。
    NotRequested,
    /// 允许了。各项是用户在系统设置里给这个应用开着的，关掉的那项系统不做。
    Granted {
        /// 弹横幅或提醒（提醒样式不是「无」）。关了只进通知中心。
        banners: bool,
        sound: bool,
        /// Dock 图标上的角标（[`SET_DOCK_BADGE`]）。
        badge: bool,
    },
    /// 用户在系统设置里关了，用 [`OPEN_NOTIFICATION_SETTINGS`] 带他去开。
    Denied,
    /// 系统报错，读不到；带给人看的原因。
    Unavailable(String),
    /// 这个系统上没有系统通知。
    Unsupported,
}

/// 设置 Dock 图标上的角标。只有 macOS 有，别的系统什么都不做。
pub const SET_DOCK_BADGE: Command<DockBadge, ()> = Command::new("smelt.app/set-dock-badge");

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockBadge {
    /// 角标上的字；`None` 清掉。
    #[serde(default)]
    pub label: Option<String>,
}

/// 用系统文件管理器（访达等）打开这台电脑上的一个目录。
pub const OPEN_IN_FILE_MANAGER: Command<OpenInFileManager, ()> =
    Command::new("smelt.app/open-in-file-manager");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenInFileManager {
    /// 这台电脑上目录的绝对路径。
    pub path: PathBuf,
}

/// 宿主转给插件的事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostEvent {
    /// 那台机器推了一条运行时话题：有插件用 [`register_topics`] 登记要的那些，按并集订，
    /// 推来的广播给所有 Rust 插件，按 `topic` 认自己要的（JS 插件只收自己登记的）。宿主原样
    /// 转发，不解读。读哪个话题就自己登记哪个，别的插件登记了也一样：靠别人登记的，那个插件
    /// 一删这里就断了。
    RuntimeTopic {
        machine: RuntimeKey,
        topic: String,
        message: TopicMessage,
    },
    /// 重新订阅上那台机器：断开期间错过的推送要自己重读，它替你建的监听（如
    /// `project.watch`）也要重新登记。
    Reconnected(RuntimeKey),
    /// 那台机器上 `project.watch` 过的项目里有文件变了（一批变化合并成一次）。只带项目根，
    /// 分不出是哪个文件。
    ProjectChanged { machine: RuntimeKey, root: String },
    /// 主窗口回到前台。监听有收不到的时候（Linux 的 inotify 用完、登记失败），在这里补一次。
    WindowActivated,
    /// 主窗口里宿主的遮罩层开了或关了（读 [`Host::overlay_open`]）。
    OverlayChanged,
    /// 挂着的机器变了：加了、移除了、改了名或连接状态。
    MachinesChanged,
    /// 活动上下文变了：当前项目或露着的会话（读 [`active_context`]）。
    ActiveContextChanged,
    /// 有机器的会话状态或工作区目录变了（读 [`Host::session_states`]、
    /// [`Host::workspace_directory`]）。
    SessionsChanged,
    /// 系统通知的授权变了（读 [`Host::notification_permission`]）。
    NotificationPermissionChanged,
    /// 看得见的视图变了：有的露出来、有的被挡住或收起，设置换了页、开了或关了（读
    /// [`Host::view_shown`]）。
    ShownViewsChanged,
    /// 摆在页的布局里的视图变了：页、布局登记变了，或外观里换了布局（读
    /// [`Host::view_laid_out`]）。
    LaidOutViewsChanged,
    /// 视图登记变了：加了、替换了或撤下了，设置页也算（见 [`register_views`]）。
    ViewsChanged,
    /// 有视图的角标变了（读 [`view_badge`]）。
    ViewBadgesChanged,
    /// 有视图的 safe area 变了（读 [`safe_area`]）：变了的视图已经重画，摆着子视图的据此重分。
    SafeAreaChanged,
    /// 主题登记变了：宿主应重新应用当前主题并更新设置页。
    ThemesChanged,
    /// 布局登记变了（见 [`register_layouts`]）：选的布局可能刚登记上或撤下了。
    LayoutsChanged,
    /// 页登记变了（见 [`register_pages`]）：首页可能换了，正开着的页可能撤下了。
    PagesChanged,
    /// 要订的运行时话题变了（见 [`register_topics`]）：宿主按新的并集增减订阅。
    TopicsChanged,
    /// 右键菜单里登记的项变了（见 [`register_context_menu_items`]）。菜单在打开时按
    /// [`context_menu_items`] 取，下次打开就是新的；自己缓存了菜单项的跟着重取。
    ContextMenuItemsChanged,
    /// 命令登记变了（见 [`register_commands`]）：快捷键、菜单、命令面板要跟着变。
    CommandsChanged,
    /// 界面语言换了（读 [`locale`]）：窗口已经重画，存在视图状态里的字（建好的菜单、
    /// 列表行、取过的 [`views`] 这些登记快照）要重建。
    LocaleChanged,
    /// 插件发的事件（见 [`Event`]）：`name` 是 `<来源>/<名字>`，载荷的形状由发的插件定，用
    /// [`Event::parse`] 认。
    Plugin {
        name: gpui::SharedString,
        payload: serde_json::Value,
    },
    /// 要在那台机器上打开这些路径：拖到 Dock 图标上的、Finder 里「用 Smelt 打开」的。路径
    /// 是那台机器上的绝对路径，可能是目录也可能是文件。谁认谁开，宿主自己不开。
    OpenPaths {
        machine: RuntimeKey,
        paths: Vec<String>,
    },
}

/// 一条运行时话题推来的东西。载荷的形状见协议里那个话题的类型。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TopicMessage {
    /// 一条事件。
    Event(serde_json::Value),
    /// 带快照的话题的整份状态：订上（含重连）时一份，积压丢了事件后再一份。收到就整份换掉
    /// 自己留的那份。
    Snapshot(serde_json::Value),
    /// 断开或积压，可能错过了事件。没有快照的话题要自己重读；带快照的话题后面跟着一份快照。
    Lagged,
    /// 那台机器这一轮连接上不会再推这个话题：运行时拒掉了订阅（多半是比这个话题旧），或推来的
    /// 解不开。原因给人看。重新连上时还会再订，订上了照常推。
    Unavailable(String),
}

/// [`HostEvent`] 的发送方。宿主装好，插件订阅。
pub struct HostEvents;

impl EventEmitter<HostEvent> for HostEvents {}

/// 宿主给插件的服务里读的那一半：查状态，只读内存，render 里可以调。要宿主做事用命令
/// （[`NOTIFY`] 这些）。只有宿主实现它。
pub trait Host: 'static {
    /// 桌面挂着的机器，本机在最前。
    fn machines(&self, cx: &App) -> Vec<Machine>;
    /// 那台机器的运行时客户端；机器已经移除是 `None`。
    fn client(&self, machine: &RuntimeKey, cx: &App) -> Option<RuntimeClient>;
    /// 开到那台机器运行时的长连接（终端、对话的字节流）用的连接器：在主线程上要，带去后台
    /// 线程连，每次 `connect` 新开一条、阻塞。本机守护没在跑时由它拉起；远程的经 iroh 拨号，
    /// 连不上就是连不上，调用方按断线处理（[`wait_online`] 等它回来再连）。短请求用
    /// [`Host::client`]。机器已经移除是 `None`。
    fn connector(&self, machine: &RuntimeKey, cx: &App) -> Option<Arc<dyn client::Connector>>;
    /// 那台机器此刻在线：最近一次探测连得上、版本对得上。本机守护重启、升级的那一会儿也
    /// 不在线。变了发 [`HostEvent::MachinesChanged`]；要等它在线用 [`wait_online`]。
    fn machine_online(&self, machine: &RuntimeKey, cx: &App) -> bool;
    /// 远程机器的连接表：配对了哪些、各自连得怎样。只读内存；改用 [`ADD_MACHINE`]、
    /// [`REMOVE_MACHINE`] 这些命令。变了发 [`HostEvent::MachinesChanged`]。
    fn connections(&self, cx: &App) -> Connections;
    /// 那个视图此刻看得见：摆在布局里且所在组显示着它，或开在页的出口里；设置页是设置
    /// 开着、停在这一页。视图被挡住、切走后还活着，要靠它知道自己露没露着，看不见时不做
    /// 只为显示的事（采样、刷新）。
    /// 变了发 [`HostEvent::ShownViewsChanged`]。
    fn view_shown(&self, id: &str, cx: &App) -> bool;
    /// 那个视图摆在某一页眼下装的布局里（哪页都算，不管露没露着）。没摆的视图打开时放进
    /// 出口；启动器之类的入口据此列出要靠它们才打得开的视图。只看登记和选的布局，用户在
    /// Dock 里拖动不增减视图。变了发 [`HostEvent::LaidOutViewsChanged`]。
    fn view_laid_out(&self, id: &str, cx: &App) -> bool;
    /// 主窗口（工作台那扇）；还没开或已经关了是 `None`。命令在这里执行（见 [`run`]）。
    fn main_window(&self, cx: &App) -> Option<AnyWindowHandle>;
    /// 主窗口此刻在前台、人在用。回到前台发 [`HostEvent::WindowActivated`]；去后台不发，
    /// 定时做事的在醒来时问这里，不在前台就停。窗口自己的激活位在 macOS 全屏切 Space 时
    /// 不准，插件不要自己问窗口。
    fn window_active(&self, cx: &App) -> bool;
    /// 主窗口里宿主自己的遮罩层（确认框、命令面板、大图预览）开着。原生子视图（网页视图）
    /// 贴在窗口上、盖得住这些层，开着时要让开；开关时发 [`HostEvent::OverlayChanged`]。
    /// 插件自己开的对话框不算在内，用 [`ui::dialog::is_open`] 看。
    fn overlay_open(&self, cx: &App) -> bool;
    /// 那台机器上的路径怎么切（`/` 还是 `\`）。远程机器还没连上过时按 POSIX。
    fn path_style(&self, machine: &RuntimeKey, cx: &App) -> model::runtime_path::PathStyle;
    /// 那台机器的运行时报的会话状态：每格终端、每段对话一条，id 是守护会话 id。没连上过
    /// 是空的。主窗口关着也照常更新；变了发 [`HostEvent::SessionsChanged`]。
    fn session_states(
        &self,
        machine: &RuntimeKey,
        cx: &App,
    ) -> Vec<model::daemon_state::DaemonSessionState>;
    /// 那台机器当前这一轮会话快照里，这个会话的运行时还在不在；订阅还没就绪、判断不了是
    /// `None`。断线重连前先问它：没了就不再接，只剩身份的按原 id 重新打开。变了发
    /// [`HostEvent::SessionsChanged`]。
    fn session_presence(
        &self,
        machine: &RuntimeKey,
        session_id: &str,
        cx: &App,
    ) -> Option<model::daemon_state::RuntimePresence>;
    /// 那台机器的工作区目录：开着的项目和对话，是会话的名册（会话状态里没有的对话是还没
    /// 起过）。还没收到是 `None`。变了发 [`HostEvent::SessionsChanged`]。
    fn workspace_directory(
        &self,
        machine: &RuntimeKey,
        cx: &App,
    ) -> Option<model::workspace_directory::WorkspaceDirectory>;
    /// 系统通知的授权。应用回到前台时重查；变了发
    /// [`HostEvent::NotificationPermissionChanged`]。
    fn notification_permission(&self, cx: &App) -> NotificationPermission;
}

struct HostGlobal {
    host: Rc<dyn Host>,
    events: Entity<HostEvents>,
}

impl Global for HostGlobal {}

/// 宿主启动时调一次，在注册任何插件之前。
pub fn install(host: impl Host, cx: &mut App) {
    let events = cx.new(|_| HostEvents);
    cx.set_global(HostGlobal {
        host: Rc::new(host),
        events,
    });
}

/// 宿主服务。宿主还没 [`install`] 就调用是编程错误。
pub fn host(cx: &App) -> Rc<dyn Host> {
    cx.global::<HostGlobal>().host.clone()
}

/// 订阅 [`HostEvent`] 用的实体。
pub fn events(cx: &App) -> Entity<HostEvents> {
    cx.global::<HostGlobal>().events.clone()
}

/// [`events`]，宿主还没装（测试里）是 `None`。
#[cfg(feature = "js")]
pub(crate) fn try_events(cx: &App) -> Option<Entity<HostEvents>> {
    Some(cx.try_global::<HostGlobal>()?.events.clone())
}

/// 宿主发一条事件给所有订阅的插件。宿主还没装（测试里）就没人订阅，什么都不做。
pub fn emit(event: HostEvent, cx: &mut App) {
    if let Some(events) = cx
        .try_global::<HostGlobal>()
        .map(|host| host.events.clone())
    {
        events.update(cx, |_, cx| cx.emit(event));
    }
}

/// 按来源整组登记主题；成功后通知宿主和插件更新目录。
pub fn register_theme_sets(
    owner: &str,
    sources: &[&str],
    cx: &mut App,
) -> Result<Vec<gpui::SharedString>, ThemeRegisterError> {
    let names = smelt_ui::theme_catalog::register_theme_sets(owner, sources, cx)?;
    emit(HostEvent::ThemesChanged, cx);
    Ok(names)
}

/// 撤下来源的主题，并在目录变化时通知宿主和插件。
pub fn unregister_themes(owner: &str, cx: &mut App) -> Vec<gpui::SharedString> {
    let removed = smelt_ui::theme_catalog::unregister_themes(owner, cx);
    if !removed.is_empty() {
        emit(HostEvent::ThemesChanged, cx);
    }
    removed
}

/// 一次运行时调用的默认上限。大仓库的 status、搜索也该在这个时间里回来。
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// 目标机器已经从这台桌面移除时报的话，按当前语言。
pub fn machine_removed() -> String {
    rust_i18n::t!("machine_removed").into_owned()
}

/// 在后台发一次控制调用给那台机器。客户端在主线程取好再带去后台：一次调用是一次 socket
/// 往返（远程的还要过 iroh），不能卡住界面。失败给的是可以直接显示的原因。
pub fn call<C>(
    machine: &RuntimeKey,
    params: C::Params,
    timeout: Duration,
    cx: &App,
) -> Task<Result<C::Result, String>>
where
    C: ControlCall + 'static,
    C::Params: Send + 'static,
    C::Result: Send + 'static,
{
    let client = host(cx).client(machine, cx);
    cx.background_executor().spawn(async move {
        let client = client.ok_or_else(machine_removed)?;
        client.call::<C>(&params, timeout).map_err(call_error_text)
    })
}

/// 给人看的失败原因：运行时报的业务错误原样给出，连不上运行时则是传输错误。
pub fn call_error_text(error: ControlClientError) -> String {
    match error {
        // 应用刚更新、守护还没换到新版时会这样：守护要等空闲才升级。
        ControlClientError::Protocol(error) if error.code == ControlErrorCode::MethodNotFound => {
            rust_i18n::t!("daemon_outdated").into_owned()
        }
        ControlClientError::Protocol(error) => error.text(),
        other => other.to_string(),
    }
}

/// 页面参数里的机器：本机是 `null`，远程机器是它的 runtime id。
pub fn machine_to_json(machine: &RuntimeKey) -> serde_json::Value {
    match machine {
        RuntimeKey::Local => serde_json::Value::Null,
        RuntimeKey::Remote(id) => serde_json::Value::String(id.clone()),
    }
}

/// 命令参数里的机器字段：`#[serde(with = "smelt_plugin_api::machine_json")]`，写法同
/// [`machine_to_json`]。
pub mod machine_json {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::RuntimeKey;

    pub fn serialize<S: Serializer>(
        machine: &RuntimeKey,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match machine {
            RuntimeKey::Local => serializer.serialize_none(),
            RuntimeKey::Remote(id) => serializer.serialize_some(id),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<RuntimeKey, D::Error> {
        Ok(Option::<String>::deserialize(deserializer)?
            .map_or(RuntimeKey::Local, RuntimeKey::Remote))
    }
}

/// [`machine_to_json`] 的反过来；不是这两种形状是 `None`。
pub fn machine_from_json(value: &serde_json::Value) -> Option<RuntimeKey> {
    match value {
        serde_json::Value::Null => Some(RuntimeKey::Local),
        serde_json::Value::String(id) => Some(RuntimeKey::Remote(id.clone())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_round_trips_through_page_args() {
        for machine in [RuntimeKey::Local, RuntimeKey::Remote("abc".to_string())] {
            assert_eq!(machine_from_json(&machine_to_json(&machine)), Some(machine));
        }
        assert_eq!(machine_from_json(&serde_json::json!(1)), None);
    }

    #[test]
    fn command_args_write_machines_like_page_args() {
        for machine in [RuntimeKey::Local, RuntimeKey::Remote("abc".to_string())] {
            let request = PathRequest {
                machine: machine.clone(),
                title: "选择".into(),
                confirm: "导入".into(),
                files: true,
                multiple: false,
            };
            let value = serde_json::to_value(&request).unwrap();
            assert_eq!(value["machine"], machine_to_json(&machine));
            assert_eq!(
                serde_json::from_value::<PathRequest>(value).unwrap(),
                request
            );
        }
        // JS 插件可以不写开关。
        let bare = serde_json::json!({ "machine": null, "title": "选择", "confirm": "导入" });
        let request: PathRequest = serde_json::from_value(bare).unwrap();
        assert!(!request.files && !request.multiple);
    }
}
