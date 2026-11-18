//! 远程机器（来源 `smelt.machines`）。
//!
//! - 连出去：这台电脑配对过的远程机器。看连接状态、添加、移除在设置「远程」页（[`page`]）；
//!   重新连接、升级运行时、重命名、改中继、移除也挂在会话列表机器行的右键菜单里（[`menu`]）。
//!   连接表归宿主（它据此拨号、订阅各台机器），这里经 `Host::connections` 读、经
//!   `smelt.app/*-machine` 命令改，自己不另存一份。
//! - 连进来：让配对过的电脑和手机经 iroh 连这台机器。开关、自建 relay、配对新设备、撤销
//!   设备都在「远程」页；配置和设备白名单归本机运行时，这里只取回来显示、把改动交过去。
//!
//! 有机器要处理（离线、版本不对、走中继很慢、机器表读不出来）时在「远程」页挂角标，宿主
//! 据此在侧栏「设置」和设置导航上标红点。本插件不存数据。
//!
//! 别的插件要把人带去添加机器（比如没开会话时的空舞台），执行 [`contract::ADD_MACHINE`]。

mod contract;
mod menu;
mod page;
mod status;

use api::HostEvent;
use contract::VIEW;
use gpui::App;
use smelt_plugin_api as api;

rust_i18n::i18n!("locales", fallback = "en");

/// 本插件的来源 id。
const OWNER: &str = "smelt.machines";

/// 内置插件的入口：宿主启动时按它登记（见 [`smelt_plugin_api::BuiltinPlugin`]）。
pub const PLUGIN: smelt_plugin_api::BuiltinPlugin = smelt_plugin_api::BuiltinPlugin {
    id: OWNER,
    register,
};

/// 宿主启动时调一次。
fn register(cx: &mut App) {
    page::register(cx);
    register_commands(cx);
    menu::publish(cx);
    update_badge(cx);
    let events = api::events(cx);
    cx.subscribe(&events, |_, event: &HostEvent, cx| {
        if *event == HostEvent::MachinesChanged {
            menu::publish(cx);
            update_badge(cx);
        }
    })
    .detach();
}

/// 登记本插件的全部命令。登记按来源整套替换，菜单项的命令和「添加远程机器」要一起交。
fn register_commands(cx: &mut App) {
    let mut specs = menu::commands();
    specs.push(page::add_machine_command());
    if let Err(error) = api::register_commands(OWNER, api::text!("commands"), specs, cx) {
        api::app_log::warn("machines", &format!("远程机器的命令登记失败：{error}"));
    }
}

/// 有机器要处理就在「远程」页上挂角标，没有就清掉。
fn update_badge(cx: &mut App) {
    let attention = status::needs_attention(&api::host(cx).connections(cx));
    api::set_view_badge(VIEW, attention.then(|| "!".into()), cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 整套命令过得了登记的校验（一条不合法整套都登不上），「添加远程机器」进命令面板、
    /// 不带参数也能执行。
    #[gpui::test]
    fn commands_register_with_add_machine_in_the_palette(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_commands(cx);
            let add = api::command(contract::ADD_MACHINE.id(), cx).expect("添加远程机器登记上了");
            assert!(add.palette);
            assert!(!add.requires_args);
            assert_eq!(add.title.get(), "Add Remote Machine");
            assert!(api::command("smelt.machines/remove", cx).is_some());
        });
    }
}
