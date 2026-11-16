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
use std::time::Duration;

use gpui::{AnyWindowHandle, App, AppContext, Entity, EventEmitter, Global, Task, Window};
use serde::{Deserialize, Serialize};
use smelt_runtime_client::RuntimeClient;
use smelt_runtime_client::protocol::control::{ControlCall, ControlClientError, ControlErrorCode};

pub use serde_json;
pub use smelt_client::runtime::RuntimeKey;
// 发给运行时的短请求（守护协议），如改历史会话的名字。
pub use smelt_client::session_ops;
// 插件自己的库：每个来源一个 SQLite 文件，表由插件自己建、自己迁移。
pub use smelt_client_store::{PluginStore, plugin_store, rusqlite};
pub use smelt_model as model;
pub use smelt_runtime_client as client;
// 量进程的 CPU 和内存（桌面进程自己在这里量，会话的由运行时报）。
pub use smelt_sys::process_usage;
pub use smelt_ui as ui;
// 主题目录：内置主题和第三方主题按来源登记、卸下，同一套接口（见 smelt_ui::theme_catalog）。
pub use smelt_ui::theme_catalog::{
    ThemeInfo, ThemeRegisterError, ThemeSwatch, apply as apply_theme, contains as theme_contains,
    swatch as theme_swatch, themes,
};

mod command;
mod context;
mod file;
mod layout;
mod page;
pub mod sessions;
mod settings;
mod view;

#[cfg(feature = "js")]
pub mod js;

pub use command::{
    Command, CommandHandler, CommandInfo, CommandResult, CommandSpec, Handled, NO_MAIN_WINDOW,
    RunCommand, command, commands, invoke, register_commands, run, unregister_commands,
};
pub use context::{ActiveContext, ViewedSession, active_context, set_active_context};
pub use file::{FileLocation, open_file, register_file_opener, unregister_file_opener};
pub use layout::{
    DockAreaState, LAYOUT_VERSION, LayoutInfo, LayoutSource, layout, layouts, parse_layout,
    register_layouts, unregister_layouts,
};
pub use page::{
    OPEN_PAGE, OUTLET_VIEW, OpenPage, PageInfo, PageSource, home_page, page, pages, register_pages,
    unregister_pages,
};
pub use settings::{SettingsEntry, SettingsGroup, settings_pane};
pub use view::{
    OPEN_VIEW, OpenView, PluginView, SET_VIEW_BADGE, ViewBadge, ViewBuilder, ViewHandle, ViewInfo,
    ViewSpec, built_view, ensure_view, is_registered, register_views, set_view_badge,
    unregister_views, view, view_badge, views,
};

/// 桌面挂着的一台机器。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    pub key: RuntimeKey,
    pub name: String,
    /// 眼下用不了的原因（离线、版本不对），给人看的短语；能用是 `None`。
    pub unavailable: Option<String>,
    /// 装了远程桌面、眼下连着。
    pub has_desktop: bool,
    /// 那台机器的 `SMELT_HOME`，按那台机器的路径写。远程机器还没连上过（或运行时太旧
    /// 不报）是 `None`。智能体的 space 在它下面。
    pub smelt_home: Option<String>,
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

/// 设置菜单栏图标旁的文字和点开的菜单。菜单栏图标只有一个、是应用图标，后设的为准；没人
/// 设过就不显示。只有 macOS 有，别的系统什么都不做。
pub const SET_STATUS_MENU: Command<StatusMenu, ()> = Command::new("smelt.app/set-status-menu");

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusMenu {
    /// 图标右边的字；空的只显示图标。
    #[serde(default)]
    pub title: String,
    /// 鼠标停在图标上时的提示。
    #[serde(default)]
    pub tooltip: String,
    /// 点开的菜单，从上往下。
    #[serde(default)]
    pub items: Vec<StatusMenuItem>,
}

/// 菜单里的一项。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StatusMenuItem {
    /// 点了执行 `command`（带 `args`），失败由宿主报给用户。
    Action {
        label: String,
        #[serde(default)]
        icon: Option<StatusIcon>,
        command: String,
        #[serde(default)]
        args: serde_json::Value,
    },
    Separator,
}

/// 菜单项左边的小图标：只取 PNG 的透明度当形状，涂成 `color`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusIcon {
    pub png: Vec<u8>,
    /// `0xRRGGBB`。
    pub color: u32,
}

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

/// 改一台机器的智能体配置（[`Host::agent_settings`]），给 JS 插件用；Rust 插件用
/// [`edit_agent_settings`]。`before` 是改之前读到的那份，`after` 是改成的那份；那台机器的
/// 运行时按两者的差别合并，只写真正改了的字段，这期间别处改的其它字段不会被盖掉。改完由
/// 运行时推回来（发 [`HostEvent::AgentSettingsChanged`]），宿主不先改本地。
pub const EDIT_AGENT_SETTINGS: Command<AgentSettingsEdit, ()> =
    Command::new("smelt.app/edit-agent-settings");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSettingsEdit {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    pub before: model::agent_host::AgentHostSettings,
    pub after: model::agent_host::AgentHostSettings,
}

/// 发一次智能体配置修改的上限。运行时那边一次合并写库，正常是毫秒级。
pub const AGENT_SETTINGS_TIMEOUT: Duration = Duration::from_secs(10);

/// 把 `edit` 发给那台机器的运行时，见 [`EDIT_AGENT_SETTINGS`]。返回运行时合并后的那份。
pub fn send_agent_settings_edit(
    edit: AgentSettingsEdit,
    cx: &App,
) -> Task<Result<model::agent_host::AgentHostSettings, String>> {
    call::<model::control_api::AgentHostApplyEdit>(
        &edit.machine,
        model::agent_host::AgentHostEdit {
            before: edit.before,
            after: edit.after,
        },
        AGENT_SETTINGS_TIMEOUT,
        cx,
    )
}

/// 改那台机器的智能体配置：在 [`Host::agent_settings`] 那份上调 `f`，把改动发给它的运行时
/// （见 [`EDIT_AGENT_SETTINGS`]）。改完由运行时推回来，这里不先改本地；配置还没收到、
/// 给不了时报错，`f` 什么都没改时不发。
///
/// 每台机器一条队列，一次只发一份，轮到时才调 `f`：连着调几次就按调用的顺序都改上，后一份
/// 看得到前一份的结果。前一份改完时推送可能还没到，所以接着改的以运行时回的那份为底。
pub fn edit_agent_settings(
    machine: &RuntimeKey,
    f: impl FnOnce(&mut model::agent_host::AgentHostSettings) + 'static,
    cx: &mut App,
) -> Task<Result<(), String>> {
    let (done, result) = futures_channel::oneshot::channel();
    let edits = cx.default_global::<AgentSettingsEdits>();
    let idle = !edits.0.contains_key(machine);
    edits
        .0
        .entry(machine.clone())
        .or_default()
        .push_back(AgentSettingsEditJob {
            edit: Box::new(f),
            done,
        });
    if idle {
        let machine = machine.clone();
        cx.spawn(async move |cx| {
            let mut last = None;
            while let Some((job, before)) =
                cx.update(|cx| next_agent_settings_edit(&machine, last.take(), cx))
            {
                let _ = job.done.send(
                    apply_agent_settings_edit(&machine, job.edit, before, &mut last, cx).await,
                );
            }
        })
        .detach();
    }
    cx.spawn(async move |_| {
        result
            .await
            .unwrap_or_else(|_| Err("智能体配置没有改完".to_string()))
    })
}

/// 每台机器等着发的配置修改。有队列就是那台机器正在发，排空了就拿掉。
#[derive(Default)]
struct AgentSettingsEdits(
    std::collections::BTreeMap<RuntimeKey, std::collections::VecDeque<AgentSettingsEditJob>>,
);

impl Global for AgentSettingsEdits {}

type AgentSettingsChange = Box<dyn FnOnce(&mut model::agent_host::AgentHostSettings)>;

struct AgentSettingsEditJob {
    edit: AgentSettingsChange,
    done: futures_channel::oneshot::Sender<Result<(), String>>,
}

/// 取那台机器的下一份修改和它的底稿：`last` 是上一份改完运行时回的，没有就用眼下收到的
/// 那份。队列空了就拿掉它，返回 `None`。
fn next_agent_settings_edit(
    machine: &RuntimeKey,
    last: Option<model::agent_host::AgentHostSettings>,
    cx: &mut App,
) -> Option<(
    AgentSettingsEditJob,
    Result<model::agent_host::AgentHostSettings, String>,
)> {
    let edits = cx.global_mut::<AgentSettingsEdits>();
    let Some(job) = edits.0.get_mut(machine).and_then(|queue| queue.pop_front()) else {
        edits.0.remove(machine);
        return None;
    };
    let before = match last {
        Some(last) => Ok(last),
        None => host(cx)
            .agent_settings(machine, cx)
            .cloned()
            .ok_or_else(|| {
                host(cx)
                    .agent_settings_unavailable(machine, cx)
                    .unwrap_or_else(|| "这台机器的智能体配置还没收到，稍后再改".to_string())
            }),
    };
    Some((job, before))
}

/// 在 `before` 上改，发出去；改上了把运行时回的那份记进 `last`。
async fn apply_agent_settings_edit(
    machine: &RuntimeKey,
    edit: AgentSettingsChange,
    before: Result<model::agent_host::AgentHostSettings, String>,
    last: &mut Option<model::agent_host::AgentHostSettings>,
    cx: &mut gpui::AsyncApp,
) -> Result<(), String> {
    let before = before?;
    let mut after = before.clone();
    edit(&mut after);
    if after == before {
        *last = Some(before);
        return Ok(());
    }
    let sent = cx.update(|cx| {
        send_agent_settings_edit(
            AgentSettingsEdit {
                machine: machine.clone(),
                before,
                after,
            },
            cx,
        )
    });
    *last = Some(sent.await?);
    Ok(())
}

/// 宿主转给插件的事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostEvent {
    /// 那台机器推了一条运行时话题（核心订阅以外的那些，如
    /// [`model::daemon_state::CORE_TOPIC_PI_CONFIG_CHANGED`]、
    /// [`model::daemon_state::CORE_TOPIC_ATTENTION_CHANGED`]）。宿主原样转发，不解读。
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
    /// 活动上下文变了：当前项目或舞台上露着的会话（读 [`active_context`]）。
    ActiveContextChanged,
    /// 有机器的会话状态或工作区目录变了（读 [`Host::session_states`]、
    /// [`Host::workspace_directory`]）。
    SessionsChanged,
    /// 系统通知的授权变了（读 [`Host::notification_permission`]）。
    NotificationPermissionChanged,
    /// 某台机器的智能体配置变了（读 [`Host::agent_settings`]）。
    AgentSettingsChanged,
    /// 看得见的视图变了：有的露出来、有的被挡住或收起，设置换了页、开了或关了（读
    /// [`Host::view_shown`]）。
    ShownViewsChanged,
    /// 视图登记变了：加了、替换了或撤下了，设置页也算（见 [`register_views`]）。
    ViewsChanged,
    /// 有视图的角标变了（读 [`view_badge`]）。
    ViewBadgesChanged,
    /// 主题登记变了：宿主应重新应用当前主题并更新设置页。
    ThemesChanged,
    /// 布局登记变了（见 [`register_layouts`]）：选的布局可能刚登记上或撤下了。
    LayoutsChanged,
    /// 页登记变了（见 [`register_pages`]）：首页可能换了，正开着的页可能撤下了。
    PagesChanged,
    /// 命令登记变了（见 [`register_commands`]）：快捷键、菜单、命令面板要跟着变。
    CommandsChanged,
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
    /// 那台机器的智能体配置：自定义 Agent（profile）、智能体定义、环境变量。运行时推来的
    /// 那一份，各处都读它；还没收到、给不了是 `None`。只读，改用 [`edit_agent_settings`]；
    /// 变了发 [`HostEvent::AgentSettingsChanged`]。
    fn agent_settings<'a>(
        &self,
        machine: &RuntimeKey,
        cx: &'a App,
    ) -> Option<&'a model::agent_host::AgentHostSettings>;
    /// 那台机器给不了智能体配置的原因（运行时太旧等），给人看；收到了、还没收到是 `None`。
    /// 变了发 [`HostEvent::AgentSettingsChanged`]。
    fn agent_settings_unavailable(&self, machine: &RuntimeKey, cx: &App) -> Option<String>;
    /// 那个视图此刻看得见：摆在布局里且所在组显示着它，或在舞台上开成了当前页；设置页
    /// 是设置开着、停在这一页。视图
    /// 被挡住、切走后还活着，要靠它知道自己露没露着，看不见时不做只为显示的事（采样、刷新）。
    /// 变了发 [`HostEvent::ShownViewsChanged`]。
    fn view_shown(&self, id: &str, cx: &App) -> bool;
    /// 主窗口（工作台那扇）；还没开或已经关了是 `None`。命令在这里执行（见 [`run`]）。
    fn main_window(&self, cx: &App) -> Option<AnyWindowHandle>;
    /// 主窗口此刻在前台、人在用。回到前台发 [`HostEvent::WindowActivated`]；去后台不发，
    /// 定时做事的在醒来时问这里，不在前台就停。窗口自己的激活位在 macOS 全屏切 Space 时
    /// 不准，插件不要自己问窗口。
    fn window_active(&self, cx: &App) -> bool;
    /// 主窗口里宿主自己的遮罩层（确认框、命令面板、大图预览）开着。原生子视图（网页视图）
    /// 贴在窗口上、盖得住这些层，开着时要让开；开关时发 [`HostEvent::OverlayChanged`]。
    /// 插件自己用 `open_dialog` 开的对话框不算在内，用 `Window::has_active_dialog` 看。
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
    /// 宿主的对话视图，接上守护里正在跑的那段对话，嵌进插件自己的页面（如自动化运行的
    /// 现场）。对话的显示、输入、权限确认、重连都归宿主；它不进侧栏和工作区目录。守护里
    /// 已经没有这段会话就接不上，先用 [`Host::session_states`] 看。只给 Rust 插件。
    fn embed_conversation(
        &self,
        request: EmbedConversation,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<EmbeddedConversation, String>;
}

/// 要嵌哪段对话，见 [`Host::embed_conversation`]。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbedConversation {
    pub machine: RuntimeKey,
    /// 守护会话 id。
    pub session_id: String,
    /// 跑这段对话的 agent（`ConversationAgentKind::id`）。
    pub agent: String,
    pub cwd: String,
    /// 这段对话是谁起的（自动化运行等），对话里据此显示来历。
    pub binding: model::conversation::ConversationBinding,
    /// 接上之前占位处显示的话，如「正在打开运行现场…」。
    pub reason: String,
}

/// 嵌着的对话。留着它视图才跟宿主连着；扔掉就断开（会话本身不受影响）。
pub struct EmbeddedConversation {
    pub view: gpui::AnyView,
    pub session_id: String,
    pub _subscription: gpui::Subscription,
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

/// 目标机器已经从这台桌面移除时报的话。
pub const MACHINE_REMOVED: &str = "这台机器已经移除";

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
        let client = client.ok_or_else(|| MACHINE_REMOVED.to_string())?;
        client.call::<C>(&params, timeout).map_err(call_error_text)
    })
}

/// 给人看的失败原因：运行时报的业务错误原样给出，连不上运行时则是传输错误。
pub fn call_error_text(error: ControlClientError) -> String {
    match error {
        // 应用刚更新、守护还没换到新版时会这样：守护要等空闲才升级。
        ControlClientError::Protocol(error) if error.code == ControlErrorCode::MethodNotFound => {
            "正在运行的 smeltd 版本较旧，还不支持这个操作；升级守护进程后重试".to_string()
        }
        ControlClientError::Protocol(error) => error.message,
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
    fn status_menu_items_are_tagged_for_scripts() {
        let menu = StatusMenu {
            title: "1@".into(),
            tooltip: String::new(),
            items: vec![
                StatusMenuItem::Action {
                    label: "会话".into(),
                    icon: Some(StatusIcon {
                        png: vec![1, 2],
                        color: 0x112233,
                    }),
                    command: "smelt.sessions/activate".into(),
                    args: serde_json::json!({ "session_id": "s" }),
                },
                StatusMenuItem::Separator,
            ],
        };
        let value = serde_json::to_value(&menu).unwrap();
        assert_eq!(value["items"][0]["type"], "action");
        assert_eq!(value["items"][0]["icon"]["png"], serde_json::json!([1, 2]));
        assert_eq!(
            value["items"][1],
            serde_json::json!({ "type": "separator" })
        );
        assert_eq!(serde_json::from_value::<StatusMenu>(value).unwrap(), menu);
        // 脚本可以只给菜单项，项里可以不带图标和参数。
        let bare: StatusMenu = serde_json::from_value(serde_json::json!({
            "items": [{ "type": "action", "label": "打开", "command": "smelt.app/show-main-window" }],
        }))
        .unwrap();
        assert_eq!(bare.title, "");
        assert!(matches!(
            &bare.items[0],
            StatusMenuItem::Action {
                icon: None,
                args: serde_json::Value::Null,
                ..
            }
        ));
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

    /// 只有智能体配置的宿主：推来的配置记在全局上，客户端连着一个假运行时。
    struct SettingsHost(FakeRuntime);

    struct FakeSettings(model::agent_host::AgentHostSettings);

    impl Global for FakeSettings {}

    /// 假运行时：`agent.host.apply_edit` 记下收到的修改，把 `after` 存成它的配置并回给客户端；
    /// 不推送，看得出客户端有没有先改本地。
    #[derive(Clone, Default)]
    struct FakeRuntime {
        edits: std::sync::Arc<std::sync::Mutex<Vec<model::agent_host::AgentHostEdit>>>,
    }

    impl FakeRuntime {
        fn edits(&self) -> Vec<model::agent_host::AgentHostEdit> {
            self.edits.lock().unwrap().clone()
        }

        /// 每次连接给出一对 socket，另一端在新线程里答一条控制请求。
        fn client(&self) -> RuntimeClient {
            use smelt_runtime_client::protocol::control::ControlResponse;
            use std::io::{BufRead as _, Write as _};

            let fake = self.clone();
            RuntimeClient::new(move || {
                let (client, server) = smelt_runtime_client::RuntimeStream::pair()?;
                let fake = fake.clone();
                std::thread::spawn(move || {
                    let mut line = String::new();
                    std::io::BufReader::new(server.try_clone().unwrap())
                        .read_line(&mut line)
                        .unwrap();
                    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                    assert_eq!(request["method"], "agent.host.apply_edit");
                    let request_id = request["request_id"].as_str().unwrap().to_string();
                    let edit: model::agent_host::AgentHostEdit =
                        serde_json::from_value(request["params"].clone()).unwrap();
                    let after = edit.after.clone();
                    fake.edits.lock().unwrap().push(edit);
                    let response =
                        serde_json::to_value(ControlResponse::ok(request_id, after)).unwrap();
                    writeln!(&server, "{response}").unwrap();
                });
                Ok(client)
            })
        }
    }

    impl Host for SettingsHost {
        fn machines(&self, _: &App) -> Vec<Machine> {
            Vec::new()
        }
        fn client(&self, _: &RuntimeKey, _: &App) -> Option<RuntimeClient> {
            Some(self.0.client())
        }
        fn agent_settings<'a>(
            &self,
            _: &RuntimeKey,
            cx: &'a App,
        ) -> Option<&'a model::agent_host::AgentHostSettings> {
            Some(&cx.global::<FakeSettings>().0)
        }
        fn agent_settings_unavailable(&self, _: &RuntimeKey, _: &App) -> Option<String> {
            None
        }
        fn view_shown(&self, _: &str, _: &App) -> bool {
            false
        }
        fn main_window(&self, _: &App) -> Option<AnyWindowHandle> {
            None
        }
        fn window_active(&self, _: &App) -> bool {
            false
        }
        fn overlay_open(&self, _: &App) -> bool {
            false
        }
        fn path_style(&self, _: &RuntimeKey, _: &App) -> model::runtime_path::PathStyle {
            model::runtime_path::PathStyle::Posix
        }
        fn session_states(
            &self,
            _: &RuntimeKey,
            _: &App,
        ) -> Vec<model::daemon_state::DaemonSessionState> {
            Vec::new()
        }
        fn workspace_directory(
            &self,
            _: &RuntimeKey,
            _: &App,
        ) -> Option<model::workspace_directory::WorkspaceDirectory> {
            None
        }
        fn notification_permission(&self, _: &App) -> NotificationPermission {
            NotificationPermission::Checking
        }
        fn embed_conversation(
            &self,
            _: EmbedConversation,
            _: &mut Window,
            _: &mut App,
        ) -> Result<EmbeddedConversation, String> {
            Err("没有对话".to_string())
        }
    }

    /// 同一拍里连着改两次：按顺序都发出去，第二份以运行时回的第一份为底（推送还没到也看得到
    /// 第一份的结果），本地那份一直不动，等运行时推回来。
    #[gpui::test]
    fn edits_in_one_tick_all_apply_in_order(cx: &mut gpui::TestAppContext) {
        let runtime = FakeRuntime::default();
        let fresh = model::agent_host::AgentHostSettings::fresh();
        let (first, second) = cx.update(|cx| {
            cx.set_global(FakeSettings(fresh.clone()));
            install(SettingsHost(runtime.clone()), cx);
            let first = edit_agent_settings(
                &RuntimeKey::Local,
                |settings| settings.default_conversation_agent = Some("a".to_string()),
                cx,
            );
            let second = edit_agent_settings(
                &RuntimeKey::Local,
                |settings| {
                    assert_eq!(settings.default_conversation_agent.as_deref(), Some("a"));
                    settings.hooks_enabled = false;
                },
                cx,
            );
            (first, second)
        });
        cx.run_until_parked();
        assert_eq!(cx.foreground_executor().block_test(first), Ok(()));
        assert_eq!(cx.foreground_executor().block_test(second), Ok(()));
        let edits = runtime.edits();
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].before, fresh);
        assert_eq!(edits[1].before, edits[0].after);
        assert!(!edits[1].after.hooks_enabled);
        cx.update(|cx| assert_eq!(cx.global::<FakeSettings>().0, fresh, "本地不先改"));
    }

    /// 什么都没改的不发。
    #[gpui::test]
    fn an_edit_that_changes_nothing_is_not_sent(cx: &mut gpui::TestAppContext) {
        let runtime = FakeRuntime::default();
        let unchanged = cx.update(|cx| {
            cx.set_global(FakeSettings(model::agent_host::AgentHostSettings::fresh()));
            install(SettingsHost(runtime.clone()), cx);
            edit_agent_settings(&RuntimeKey::Local, |_| {}, cx)
        });
        cx.run_until_parked();
        assert_eq!(cx.foreground_executor().block_test(unchanged), Ok(()));
        assert!(runtime.edits().is_empty());
    }
}
