//! 内置插件「会话列表」（来源 `smelt.session-list`）：按机器、项目、工作集分组列出各台机器上
//! 的会话，置顶、排序、折叠，新建会话的菜单，项目和会话行的操作。
//!
//! 对外只公开 [`register`] 和 [`OWNER`]。别的插件按 [`contract`] 里的字符串和 JSON 调。
//!
//! # 和会话插件的分工
//!
//! 会话本身归会话插件（`smelt.sessions`，约定照抄在 [`core`]）：会话视图、选中哪段会话或
//! 哪个项目、把新会话写进目录再打开。这边只画、只发请求：
//!
//! - 点会话行是打开会话视图（`OPEN_VIEW` 带 `core::SessionArgs`），会话视图打开到那段时
//!   由会话插件改选中，再经 `core::SELECTION_CHANGED` 告诉这边。
//! - 点项目行发 `core::SELECT_PROJECT`；新建会话发 `core::CREATE_CONVERSATION`。
//! - 关会话、归档、打开 / 关掉项目、工作集和仓库的操作直接调那台机器的运行时（[`ops`]、
//!   [`worksets`]、[`dialogs`]），目录推回来列表跟着变。
//!
//! # 状态与权威
//!
//! 见 [`model`] 的模块文档。目录和会话状态是宿主推来的只读缓存，用时现取，不另存；自己的
//! 数据只有置顶、顺序、折叠、行首头像用哪个（[`store`]，插件自己的库）。
//!
//! 出错给人看：跟某段会话、项目、工作树有关的，挂在列表的那一行上（[`report_row_failure`]、
//! [`fail_row`]，不管是行上、菜单还是快捷键发起的）；找不到那一行的（添加项目、拖放、
//! 别的插件调过来的）才发系统通知（[`notify_error`]、[`report_failure`]）。
//!
//! # 约束
//!
//! - render 只读内存：侧栏读 `SessionList::sidebar`；读写库、`create_view`、发调用都在事件里。
//! - 等待一律事件驱动（[`sync`]），不起定时器、不轮询重试。
//! - 界面用 `smelt_plugin_api::ui`（smelt-ui），不用 gpui-component。

mod attention;
mod commands;
mod contract;
mod core;
mod dialogs;
mod list;
mod model;
mod ops;
mod peers;
mod settings_page;
mod store;
mod sync;
mod worksets;

use rust_i18n::t;
use std::borrow::Cow;

use gpui::{App, Task};
use smelt_plugin_api as api;

rust_i18n::i18n!("locales", fallback = "en");

/// 来源 id，也是本插件库的名字。
pub const OWNER: &str = "smelt.session-list";

/// 宿主启动时调一次，排在会话插件之后（起来时要问它选中了什么）。
pub fn register(cx: &mut App) {
    let list = model::SessionList::install(cx);
    sync::install(&list, cx);

    let views = vec![
        api::ViewSpec {
            name: view_name(contract::LIST).into(),
            title: api::text!("view.list"),
            icon: api::ui::IconName::MessagesSquare.path(),
            draws_top_bar: true,
            settings: None,
            instances: false,
            fills: Vec::new(),
            build: std::rc::Rc::new(list::build_view),
        },
        settings_page::spec(),
    ];
    if let Err(error) = api::register_views(OWNER, views, cx) {
        eprintln!("[session-list] 视图登记失败：{error}");
    }
    commands::register(cx);
}

/// 视图 id 里来源后面那段。
fn view_name(id: &'static str) -> &'static str {
    id.split_once('/').map_or(id, |(_, name)| name)
}

/// 出错给人看：记日志，发一条系统通知。只给找不到落点的失败用（快捷键、拖放、别的插件调
/// 过来的命令）；列表里某一行发起的走 [`report_row_failure`]。
pub(crate) fn notify_error(message: &str, cx: &mut App) {
    eprintln!("[session-list] {message}");
    let notify = api::Notify {
        subtitle: t!("error.title").into_owned(),
        body: message.to_string(),
        ..Default::default()
    };
    api::NOTIFY.run(&notify, cx).detach();
}

/// 列表里某一行发起的操作：先摘掉这一行上次的失败（这是新的一次），等它做完，失败了挂在
/// 那一行上，不发系统通知——人就在看这一行。
pub(crate) fn report_row_failure<T: 'static>(
    target: model::FailTarget,
    what: Cow<'static, str>,
    ran: Task<Result<T, String>>,
    cx: &mut App,
) {
    clear_row_failure(target.clone(), cx);
    cx.spawn(async move |cx| {
        if let Err(error) = ran.await {
            cx.update(|cx| fail_row(target, what, error, cx));
        }
    })
    .detach();
}

/// 把一次失败挂到列表的那一行上（记日志，不发系统通知）。
pub(crate) fn fail_row(
    target: model::FailTarget,
    what: Cow<'static, str>,
    error: String,
    cx: &mut App,
) {
    eprintln!(
        "[session-list] {}",
        t!("error.failed", what = what, error = error)
    );
    let failure = model::Failure {
        what: what.into_owned().into(),
        error: error.into(),
    };
    model::SessionList::global(cx)
        .update(cx, |list, cx| list.set_failure(target, Some(failure), cx));
}

/// 这一行又发起了一次操作：上次的失败不再算数。
pub(crate) fn clear_row_failure(target: model::FailTarget, cx: &mut App) {
    model::SessionList::global(cx).update(cx, |list, cx| list.set_failure(target, None, cx));
}

/// 等一件事做完，失败了按 `what`（「关会话」这类）报给人看。
pub(crate) fn report_failure<T: 'static>(
    what: Cow<'static, str>,
    ran: Task<Result<T, String>>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        if let Err(error) = ran.await {
            cx.update(|cx| notify_error(&t!("error.failed", what = what, error = error), cx));
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_names_drop_the_owner() {
        assert_eq!(view_name(contract::LIST), "main");
        assert!(contract::LIST.starts_with(&format!("{OWNER}/")));
    }
}
