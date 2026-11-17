//! 自动化：每台机器上的定时、间隔、webhook 任务，运行记录和完成提醒。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 和宿主打交道，宿主里没有它的类型。
//!
//! - 投影（[`projection`]）：各台机器经单独订阅的 `automations.changed` 推来的整份库，按机器
//!   各留一份；跑完的运行发系统通知，入口角标是最近一次失败的自动化条数。
//! - 页面（[`page`]）：视图 id 是 [`VIEW_ID`]，打开参数见 [`page`] 的模块文档。改动一律发给
//!   自动化所在的那台机器，等它推回来。
//! - 别的插件（[`peers`]）：运行现场是对话插件的实例视图；引用的智能体读智能体插件的配置，
//!   在 [`agents`] 留只读缓存。哪个没装，对应的部分就没有，自动化照常可用。

mod actions;
mod agents;
mod catalog;
mod editor;
mod format;
mod kinds;
mod notifications;
mod page;
mod peers;
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
pub(crate) const VIEW_ID: &str = "smelt.automations/main";

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    projection::install(cx);
    agents::install(cx);
    api::register_views(
        OWNER,
        vec![ViewSpec {
            name: "main".into(),
            title: "自动化".into(),
            icon: smelt_plugin_api::ui::IconName::Workflow.path(),
            draws_top_bar: false,
            instances: false,
            settings: None,
            build: Rc::new(|window, cx| {
                Rc::new(cx.new(|cx| AutomationsPage::new(window, cx))) as Rc<dyn ViewHandle>
            }),
        }],
        cx,
    )
    .expect("自动化的视图表写错了");
}
