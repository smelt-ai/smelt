//! 终端：每格终端是一个视图实例（[`contract`] 里的 `smelt.terminal/terminal`），接着守护里
//! 的一段终端会话。PTY 和 shell 在 smeltd 里，这里连守护拿字节流，alacritty_terminal 解析
//! 成网格，GPUI 画出来；断线自己接回。
//!
//! 内置插件（来源 id [`OWNER`]），只依赖 [`smelt_plugin_api`] 和第三方库。摆终端、标题、
//! 侧栏、结束会话归摆它的一方（会话插件），经实例视图和 [`contract`] 里的命令用这里。
//!
//! 本插件还管：终端字号（自己的设置页、自己的库）、终端配色发给本机运行时（[`theme`]）。

mod contract;
mod instance;
mod redraw;
mod settings_page;
mod store;
mod terminal;
mod terminal_images;
mod terminal_view;
mod theme;

use std::rc::Rc;

use gpui::{App, KeyBinding};
use smelt_plugin_api::{self as api, text};

rust_i18n::i18n!("locales", fallback = "en");

/// 来源 id，也是本插件库的名字。
pub const OWNER: &str = "smelt.terminal";

/// 终端的图标：实例视图和设置页共用。
const ICON: &str = api::ui::IconName::Terminal.asset_path();

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    let store = Rc::new(store::TerminalStore::open());
    terminal_view::set_font_px(store.font_px());

    // 把 Tab/Shift-Tab 从组件库 Root 的焦点跳转手里、Esc 从工作台手里要回来，终端聚焦时
    // 发给 shell（见 terminal_view 里 `TerminalTab` 的注释）。
    cx.bind_keys([
        KeyBinding::new("tab", terminal_view::TerminalTab, Some("Terminal")),
        KeyBinding::new(
            "shift-tab",
            terminal_view::TerminalBackTab,
            Some("Terminal"),
        ),
        KeyBinding::new("escape", terminal_view::TerminalEscape, Some("Terminal")),
    ]);

    if let Err(error) = api::register_views(
        OWNER,
        vec![instance::spec(), settings_page::spec(store)],
        cx,
    ) {
        eprintln!("[terminal] 视图登记失败：{error}");
    }
    if let Err(error) = api::register_commands(
        OWNER,
        text!("title"),
        vec![
            contract::SEND_TEXT.spec(text!("command.send_text"), |text, _, cx| {
                instance::send_text(text, cx)
            }),
        ],
        cx,
    ) {
        eprintln!("[terminal] 命令登记失败：{error}");
    }
    theme::register(cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_ids_are_under_this_owner() {
        for id in [contract::VIEW, contract::SEND_TEXT.id(), contract::MENU] {
            assert_eq!(
                id.split_once('/').map(|(owner, _)| owner),
                Some(OWNER),
                "{id}"
            );
        }
    }
}
