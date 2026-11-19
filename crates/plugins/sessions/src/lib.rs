//! 内置插件「会话」（来源 `smelt.sessions`）：会话视图（对话加终端分屏）、按工作区目录建和扔
//! 会话、打开某段会话、选中，以及分屏布局。会话列表是另一个插件（`smelt.session-list`），只经
//! [`contract`] 和这边打交道。
//!
//! 对外只公开 [`register`] 和 [`OWNER`]。别的插件按 [`contract`] 里的字符串和 JSON 调。
//!
//! # 概念
//!
//! - **会话**是那台机器工作区目录里的一条对话（`WorkspaceConversation`），身份是
//!   [`model::ConversationKey`]（机器 + 目录里的对话 id）。要么是一段 ACP 对话，要么是一组
//!   终端；模型上统一成「可选的一段对话 + 一个终端分屏」，不分两种会话。
//! - **格**是会话里的一个守护会话（ACP 对话的 `sid`，或一格终端的 `id`），画出来是终端
//!   插件、对话插件登记的视图实例（`create_view`，约定照抄在 [`peers`]）。
//! - 会话在侧栏哪儿只看目录（[`model::placement`]）：归档的、自动化跑的不进侧栏，不能选、
//!   不能露。
//!
//! # 状态与权威
//!
//! 见 [`model`] 的模块文档。一句话：目录和会话状态是宿主推来的只读缓存，用时现取，不另存；
//! 插件自己的数据只有分屏和上次选中（[`store`]，插件自己的库），以及只在内存里的选中
//! （[`model::Sessions`]，全局唯一一份）。建好的会话（分屏、视图实例）绑着窗口，归会话视图。
//!
//! 「去某段会话」都是打开会话视图（`OPEN_VIEW` 带 [`contract::SessionArgs`]）：列表点行、
//! 上一段 / 下一段、`ACTIVATE`、新建之后同一条路，没有绕开视图改选中的旁路。
//!
//! # 文件
//!
//! | 文件 | 内容 |
//! |---|---|
//! | `contract.rs` `peers.rs` | 对外约定、照抄的别的插件的约定 |
//! | `model.rs` `store.rs` | 状态、纯函数、库 |
//! | `session_view.rs` `split.rs` `split_layout.rs` | 会话视图、分屏、分屏存档 |
//! | `lifecycle/**` | 跟着目录维护选中、打开会话、写新会话、活动上下文 |
//! | `commands.rs` `commands/**` | 所有命令的登记和处理，新会话在目录里那条怎么拼 |
//!
//! 出错给人看：会话视图里发起的画在会话视图里（`session_view`）；别的插件调过来的命令把
//! 失败交回调用方；都没有落点的才发系统通知（[`notify_error`]）。
//!
//! # 约束
//!
//! - render 只读内存：会话视图读自己建好的分屏；读写库、`create_view`、发调用都在事件里。
//! - 等待一律事件驱动：刚写进目录的会话按写目录回的版本等目录推到
//!   （`HostEvent::SessionsChanged`），冷启动等 `HostEvent::ViewsChanged` 再建实例；不起
//!   定时器、不轮询重试。
//! - 要窗口的事（建分屏、给焦点）只在会话视图里做，用的是它自己的窗口。
//! - 界面用 `smelt_plugin_api::ui`（smelt-ui），不用 gpui-component。

mod commands;
mod contract;
mod lifecycle;
mod model;
mod peers;
mod session_view;
mod split;
mod split_layout;
mod store;

use rust_i18n::t;

use gpui::App;
use smelt_plugin_api as api;

rust_i18n::i18n!("locales", fallback = "en");

/// 来源 id，也是本插件库的名字。
pub const OWNER: &str = "smelt.sessions";

/// 宿主启动时调一次，排在终端、对话插件之后（它们的视图先登记上，建会话时不用等）。
pub fn register(cx: &mut App) {
    let sessions = model::Sessions::install(cx);
    lifecycle::install(&sessions, cx);

    let views = vec![api::ViewSpec {
        name: view_name(contract::SESSION).into(),
        title: api::text!("view.session"),
        icon: api::ui::IconName::AppWindow.path(),
        draws_top_bar: true,
        settings: None,
        instances: false,
        fills: Vec::new(),
        build: std::rc::Rc::new(session_view::build_view),
    }];
    if let Err(error) = api::register_views(OWNER, views, cx) {
        eprintln!("[sessions] 视图登记失败：{error}");
    }
    commands::register(cx);
}

/// 视图 id 里来源后面那段。
fn view_name(id: &'static str) -> &'static str {
    id.split_once('/').map_or(id, |(_, name)| name)
}

/// 出错给人看：记日志，发一条系统通知。只给找不到落点的失败用。
pub(crate) fn notify_error(message: &str, cx: &mut App) {
    eprintln!("[sessions] {message}");
    let notify = api::Notify {
        subtitle: t!("error.title").into_owned(),
        body: message.to_string(),
        ..Default::default()
    };
    api::NOTIFY.run(&notify, cx).detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_names_drop_the_owner() {
        assert_eq!(view_name(contract::SESSION), "session");
        assert!(contract::SESSION.starts_with(&format!("{OWNER}/")));
    }
}
