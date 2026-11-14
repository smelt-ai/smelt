//! 自动化：每台机器上的定时、间隔、webhook 任务，运行记录和完成提醒。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 和宿主打交道，宿主里没有它的类型。
//!
//! - 投影（[`projection`]）：各台机器经单独订阅的 `automations.changed` 推来的整份库，按机器
//!   各留一份；跑完的运行发系统通知，入口角标是最近一次失败的自动化条数。
//! - 页面（[`page`]）：视图 id 是 [`VIEW_ID`]，打开参数见 [`page`] 的模块文档。改动一律发给
//!   自动化所在的那台机器，等它推回来。

mod actions;
mod catalog;
mod editor;
mod format;
mod kinds;
mod notifications;
mod page;
mod prelude;
mod projection;
mod run_detail;
mod store;
mod templates;
#[cfg(test)]
mod tests;
mod tools;
mod view;
mod widgets;

use std::rc::Rc;

use gpui::{App, AppContext as _};
use smelt_plugin_api::{self as api, ViewHandle, ViewSpec};

use page::AutomationsPage;

/// 本插件的来源 id：库文件名、通知 id 都用它。
pub(crate) const OWNER: &str = "smelt.automations";

/// 自动化页的视图 id。
pub(crate) const VIEW_ID: &str = "automations";

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    projection::install(cx);
    api::register_view(
        ViewSpec {
            id: VIEW_ID.into(),
            title: "自动化".into(),
            icon: gpui_kit_assets::IconName::Workflow.path(),
            per_machine: false,
            draws_top_bar: false,
            build: Rc::new(|window, cx| {
                Rc::new(cx.new(|cx| AutomationsPage::new(window, cx))) as Rc<dyn ViewHandle>
            }),
        },
        cx,
    )
    .expect("自动化的视图 id 不是宿主保留的");
}
