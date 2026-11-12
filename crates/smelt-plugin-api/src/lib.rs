//! 插件和宿主之间唯一的接口。
//!
//! 宿主（`smelt` 主程序）启动时实现 [`Host`]，用 [`install`] 装成全局；插件用 [`host`] 取。
//! 插件 crate 只依赖本 crate 和 gpui 组件库：要用的协议类型、运行时客户端、共用界面经
//! 这里再导出（[`model`]、[`client`]、[`ui`]）。设计见 `docs/app-plugins.md`。
//!
//! 宿主到插件的通知是 gpui 事件（[`HostEvent`]），插件用 `cx.subscribe(&events(cx), …)`
//! 订阅，不轮询。

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, AppContext, Div, Entity, EventEmitter, Global, Task, Window};
use smelt_runtime_client::RuntimeClient;
use smelt_runtime_client::protocol::control::{ControlCall, ControlClientError, ControlErrorCode};

pub use serde_json;
pub use smelt_client::runtime::RuntimeKey;
pub use smelt_model as model;
pub use smelt_runtime_client as client;
pub use smelt_ui as ui;
// 主题目录：内置主题和第三方主题按来源登记、卸下，同一套接口（见 smelt_ui::theme_catalog）。
pub use smelt_ui::theme_catalog::{
    ThemeInfo, ThemeRegisterError, ThemeSwatch, apply as apply_theme, contains as theme_contains,
    swatch as theme_swatch, themes,
};

mod layout;
mod view;

#[cfg(feature = "js")]
pub mod js;

pub use layout::{
    DockAreaState, LAYOUT_VERSION, LayoutInfo, LayoutSource, layout, layouts, parse_layout,
    register_layouts, unregister_layouts,
};
pub use view::{
    PluginView, RESERVED_VIEW_IDS, ViewBuilder, ViewHandle, ViewSpec, built_view, ensure_view,
    is_registered, register_view, unregister_view, views,
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
}

/// 选好路径后交回给插件的回调。用户取消就不调。
pub type PathsPicked = Box<dyn FnOnce(Vec<String>, &mut App)>;

/// 在某台机器上选路径。本机弹系统选择框，远程机器由宿主画一个浏览那台机器的选择框。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathRequest {
    /// 选择框标题，如「选择要导入的插件」。
    pub title: String,
    /// 确认按钮上的动词，如「导入」。
    pub confirm: String,
    /// 除了目录还能选文件。
    pub files: bool,
    pub multiple: bool,
}

/// 宿主转给插件的事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostEvent {
    /// 那台机器推了一条运行时话题（核心订阅以外的那些，如
    /// [`model::daemon_state::CORE_TOPIC_PI_CONFIG_CHANGED`]）。宿主只转发话题名，不解读；
    /// 积压丢了事件时，每个话题都当作推过一次。
    RuntimeTopic { machine: RuntimeKey, topic: String },
    /// 重新订阅上那台机器：断开期间错过的推送要自己重读。
    Reconnected(RuntimeKey),
    /// 挂着的机器变了：加了、移除了、改了名或连接状态。
    MachinesChanged,
    /// 工作台的当前项目变了（读 [`Host::active_project`]）。
    ActiveProjectChanged,
    /// 视图登记变了：加了、替换了或撤下了（见 [`register_view`]）。
    ViewsChanged,
    /// 主题登记变了：宿主应重新应用当前主题并更新设置页。
    ThemesChanged,
    /// 布局登记变了（见 [`register_layouts`]）：选的布局可能刚登记上或撤下了。
    LayoutsChanged,
}

/// [`HostEvent`] 的发送方。宿主装好，插件订阅。
pub struct HostEvents;

impl EventEmitter<HostEvent> for HostEvents {}

/// 宿主给插件的服务。只有宿主实现它。
pub trait Host: 'static {
    /// 桌面挂着的机器，本机在最前。
    fn machines(&self, cx: &App) -> Vec<Machine>;
    /// 那台机器的运行时客户端；机器已经移除是 `None`。
    fn client(&self, machine: &RuntimeKey, cx: &App) -> Option<RuntimeClient>;
    /// 发送系统通知；是否显示由系统的通知权限和设置决定。
    fn notify(&self, subtitle: &str, body: &str);
    /// 用系统文本编辑器打开这台电脑上的文件。
    fn open_in_text_editor(&self, path: &Path) -> Result<(), String>;
    /// 在那台机器上选路径。取消了就不调 `done`。
    fn pick_paths(
        &self,
        machine: &RuntimeKey,
        request: PathRequest,
        done: PathsPicked,
        cx: &mut App,
    );
    /// 工作台的当前项目，只算这台电脑上的；当前是远程项目或没有项目时是 `None`。
    fn active_project(&self, cx: &App) -> Option<PathBuf>;
    /// 页面的页头底板：留好拖窗口条、交通灯和右上角开关的位置，页面往里放标题和按钮。
    fn page_header(&self, window: &Window, cx: &App) -> Div;
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
}
