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

use gpui::{AnyWindowHandle, App, AppContext, Div, Entity, EventEmitter, Global, Task, Window};
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
pub use smelt_ui as ui;
// 主题目录：内置主题和第三方主题按来源登记、卸下，同一套接口（见 smelt_ui::theme_catalog）。
pub use smelt_ui::theme_catalog::{
    ThemeInfo, ThemeRegisterError, ThemeSwatch, apply as apply_theme, contains as theme_contains,
    swatch as theme_swatch, themes,
};

mod command;
mod file;
mod layout;
pub mod sessions;
mod settings;
mod view;

#[cfg(feature = "js")]
pub mod js;

pub use command::{
    Command, CommandHandler, CommandInfo, CommandResult, CommandSpec, Handled, NO_MAIN_WINDOW,
    RunCommand, command, commands, invoke, register_commands, run, unregister_commands,
};
pub use file::{FileLocation, open_file, register_file_opener, unregister_file_opener};
pub use layout::{
    DockAreaState, LAYOUT_VERSION, LayoutInfo, LayoutSource, layout, layouts, parse_layout,
    register_layouts, unregister_layouts,
};
pub use settings::{
    SettingsGroup, SettingsPage, SettingsPageBuilder, SettingsPageSpec, register_settings_pages,
    settings_page, settings_pages, settings_pane, unregister_settings_pages,
};
pub use view::{
    OPEN_VIEW, OpenView, PluginView, RESERVED_VIEW_IDS, ViewBuilder, ViewHandle, ViewSpec,
    built_view, ensure_view, is_registered, register_view, unregister_view, views,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notify {
    pub subtitle: String,
    pub body: String,
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

/// 设置 Dock 图标上的角标。只有 macOS 有，别的系统什么都不做。
pub const SET_DOCK_BADGE: Command<DockBadge, ()> = Command::new("smelt.app/set-dock-badge");

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockBadge {
    /// 角标上的字；`None` 清掉。
    #[serde(default)]
    pub label: Option<String>,
}

/// 一个要人看一眼的会话：有没看的提醒，或者还等人处理（批准、输入、失败）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attention {
    /// 守护会话 id（终端是一格终端，对话是整段对话）。
    pub session_id: String,
    /// 还等人处理：看过也留着，到 agent 接着跑才消。`false` 是看过就消的提醒。
    pub requires_action: bool,
}

/// 宿主转给插件的事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostEvent {
    /// 那台机器推了一条运行时话题（核心订阅以外的那些，如
    /// [`model::daemon_state::CORE_TOPIC_PI_CONFIG_CHANGED`]）。宿主只转发话题名，不解读；
    /// 积压丢了事件时，每个话题都当作推过一次。
    RuntimeTopic { machine: RuntimeKey, topic: String },
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
    /// 工作台的当前项目变了（读 [`Host::active_project`]）。
    ActiveProjectChanged,
    /// 有机器的会话状态或工作区目录变了（读 [`Host::session_states`]、
    /// [`Host::workspace_directory`]）。
    SessionsChanged,
    /// 要人看一眼的会话变了（读 [`Host::attention`]）。
    AttentionChanged,
    /// 某台机器的智能体配置变了（读 [`Host::agent_settings`]）。
    AgentSettingsChanged,
    /// 看得见的视图变了：有的露出来、有的被挡住或收起（读 [`Host::view_shown`]）。
    ShownViewsChanged,
    /// 视图登记变了：加了、替换了或撤下了（见 [`register_view`]）。
    ViewsChanged,
    /// 主题登记变了：宿主应重新应用当前主题并更新设置页。
    ThemesChanged,
    /// 布局登记变了（见 [`register_layouts`]）：选的布局可能刚登记上或撤下了。
    LayoutsChanged,
    /// 命令登记变了（见 [`register_commands`]）：快捷键、菜单、命令面板要跟着变。
    CommandsChanged,
    /// 设置页登记变了（见 [`register_settings_pages`]）：设置侧栏要跟着变。
    SettingsPagesChanged,
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
    /// 工作台的当前项目，可能在远程机器上；没有项目时是 `None`。
    fn active_project(&self, cx: &App) -> Option<Project>;
    /// 那台机器的智能体配置：自定义 Agent（profile）、智能体定义、环境变量。还没读到是
    /// `None`。只读；变了发 [`HostEvent::AgentSettingsChanged`]。
    fn agent_settings<'a>(
        &self,
        machine: &RuntimeKey,
        cx: &'a App,
    ) -> Option<&'a model::agent_host::AgentHostSettings>;
    /// 那个视图此刻看得见：摆在布局里且所在组显示着它，或在舞台上开成了当前页。变了发
    /// [`HostEvent::ShownViewsChanged`]。
    fn view_shown(&self, id: &str, cx: &App) -> bool;
    /// 页面的页头底板：留好拖窗口条、交通灯和右上角开关的位置，页面往里放标题和按钮。
    fn page_header(&self, window: &Window, cx: &App) -> Div;
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
    /// 要人看一眼的会话，所有机器的，按会话 id 排。变了发 [`HostEvent::AttentionChanged`]。
    fn attention(&self, cx: &App) -> Vec<Attention>;
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
}
