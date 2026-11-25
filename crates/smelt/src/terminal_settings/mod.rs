//! 终端全局配置与主题同步（沿用 `smelt.terminal` 以保留 `~/.smelt/plugin-data/smelt.terminal/store.sqlite3`）：
//! - 读取保存的终端字号写入 `smelt_terminal` 原子量；
//! - 登记设置页「通用 → 终端」；
//! - 监听主题变化与本机重连，向本机 smeltd 发布 `terminal_theme.publish`。

mod page;
mod store;
mod theme;

use std::rc::Rc;

use gpui::App;
use smelt_plugin_api as api;

const OWNER: &str = "smelt.terminal";

pub(crate) fn register(cx: &mut App) {
    let store = Rc::new(store::TerminalStore::open());
    smelt_terminal::set_font_px(store.font_px());
    if let Err(error) = api::register_views(OWNER, vec![page::spec(store)], cx) {
        eprintln!("[terminal] 设置页登记失败：{error}");
    }
    theme::register(cx);
}
