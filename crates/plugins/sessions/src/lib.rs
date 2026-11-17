//! 内置插件「会话」（来源 `smelt.sessions`）：会话列表、会话视图（对话加终端分屏）、按工作区
//! 目录建和扔会话、打开某段会话，以及侧栏的置顶、排序、折叠和分屏布局。
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
//! - 会话在侧栏哪儿只看目录（[`model::placement`]）：绑了工作集的进工作集，工作目录在
//!   打开的项目里的进项目，跟智能体的对话进「对话」一段，别的按工作目录自成一组；归档的、
//!   自动化跑的不进侧栏。不靠工作目录在不在 `~/.smelt` 下猜身份。
//!
//! # 状态与权威
//!
//! 见 [`model`] 的模块文档。一句话：目录和会话状态是宿主推来的只读缓存，用时现取，不另存；
//! 插件自己的数据只有置顶、顺序、折叠、分屏、上次选中（[`store`]，插件自己的库），以及
//! 只在内存里的选中（[`model::Sessions`]，全局唯一一份）。建好的会话（分屏、视图实例）绑着
//! 窗口，归会话视图。
//!
//! 「去某段会话」都是打开会话视图（`OPEN_VIEW` 带 [`contract::SessionArgs`]）：列表点行、
//! 上一段 / 下一段、`ACTIVATE`、新建之后同一条路，没有绕开视图改选中的旁路。
//!
//! # 文件和工作包
//!
//! 各包只写自己的文件；`lib.rs`、`Cargo.toml`、[`contract`]、[`model`]、[`store`]、
//! [`peers`] 归 S0，要改先回 S0 统一改，不在各包里私改。
//!
//! | 包 | 文件 | 内容 |
//! |---|---|---|
//! | S0 | `lib.rs` `contract.rs` `model.rs` `store.rs` `peers.rs` | 骨架、对外约定、状态、库 |
//! | S1 | `list/**` | 会话列表视图、侧栏行的计算、项目和机器行的右键菜单 |
//! | S2 | `session_view.rs` `split.rs` `split_layout.rs` | 会话视图、分屏、分屏存档 |
//! | S3 | `lifecycle/**` | 跟着目录维护选中、打开会话、目录操作、活动上下文 |
//! | S4 | `dialogs/**` | 新建会话、项目的仓库 |
//! | S5 | `commands.rs` | 所有命令的登记和处理 |
//!
//! S1–S5 的文件现在是 S0 留的桩：签名定死、函数体是空的（不用 `todo!`，整个 crate 照样
//! 编得过）。各包换掉自己的函数体、补私有的字段和函数；改跨包签名先和用到它的包对齐。
//!
//! # 各包之间的接口
//!
//! 谁调谁（被调的一方定义）：
//!
//! - S0 `model`
//!   - `Sessions::install(cx) -> Entity<Sessions>`、`Sessions::global(cx) -> Entity<Sessions>`。
//!   - 侧栏状态：`Sessions::{set_pinned(Pin, bool), set_collapsed(Fold, bool),
//!     set_project_order(Vec<ProjectKey>), set_conversation_order(&RuntimeKey,
//!     Vec<String>)}`，改缓存、写库、重算侧栏一步做完（S1 调）。项目顺序是所有机器排在一起
//!     的一份，会话顺序按机器分；读用
//!     `SidebarUi::{machine, is_pinned, is_collapsed}`。
//!   - `Sessions::rebuild_sidebar(cx)`、`Sessions::prune(machine, style, &directory)`、
//!     `Sessions::forget_machine(machine)`（S3 调）。
//!   - 纯函数：`session_ids`、`conversation_of_session`、`status`、`last_activity`、`title`、
//!     `agent_of`、`cwd_of`、`placement`、`known_ids`、`shown_conversation`、`locate`（会话
//!     视图打开时在目录里找，要不要等目录推到）。
//! - S0 `store`：`SessionsStore::{layout, save_layout}`（S2 读写分屏存档，`SavedLayout`
//!   里的 JSON 格式归 S2 的 `split_layout`）、`save_selection`（S3）。侧栏那部分只经
//!   `Sessions` 的方法写，不直接调。
//! - S1 `list`：`Sidebar::build(&Sessions, &App)`（S0 的 `rebuild_sidebar` 调）、
//!   `Sidebar::order()`（S5 的上一段 / 下一段）、`build_view`（`register`；`FOCUS_LIST` 是
//!   带 `{"focus": true}` 打开它）。
//! - S2 `session_view`：`build_view`（`register`）、`split_pane`、`cycle_pane`、`close_pane`
//!   （S5，经 `api::built_view` 找到会话视图上建好的分屏）。会话视图打开到某段会话时调 S3
//!   的 `select`，眼下那一格推进 `Sessions::active_pane`。
//! - S3 `lifecycle`：`install`（`register`）、`open_session` / `open_conversation`（S1 点行，
//!   S5 的上一段 / 下一段、`ACTIVATE`，新建之后）、`select`（S2 打开到某段会话时）、
//!   `publish_context`（S2 换格时）、
//!   `create_conversation`（S4、S5）、`update_conversation`（S2 加减终端、S1 改名）、
//!   `close_conversation`（S1、S2）、`archive_conversation`（S1）、`open_project`
//!   （S5 的 `OPEN_PROJECT`、拖进来的目录）、`close_project`（S1）。工作集、仓库的
//!   目录操作只有一个包用，各自直接 `api::call`。
//! - S4 `dialogs`：`open_new_session`、`open_project_repositories`（S1 的行按钮和菜单、
//!   S5 的 `NEW_SESSION`）。
//! - S5 `commands`：`register`（`register`）。
//!
//! 出错给人看：[`notify_error`]、[`report_failure`]。
//!
//! # 约束
//!
//! - render 只读内存：侧栏读 `Sessions::sidebar`，会话视图读自己建好的分屏；读写库、
//!   `create_view`、发调用都在事件里。
//! - 等待一律事件驱动：刚写进目录的会话按写目录回的版本等目录推到
//!   （`HostEvent::SessionsChanged`），冷启动等 `HostEvent::ViewsChanged` 再建实例；不起
//!   定时器、不轮询重试。
//! - 要窗口的事（建分屏、给焦点）只在会话视图里做，用的是它自己的窗口，不去找主窗口、不
//!   挪到下一轮。
//! - 界面用 `smelt_plugin_api::ui`（smelt-ui），不用 gpui-component。

mod commands;
mod contract;
mod dialogs;
mod lifecycle;
mod list;
mod model;
mod peers;
mod session_view;
mod split;
mod split_layout;
mod store;
mod worksets;

use gpui::{App, Task};
use smelt_plugin_api as api;

/// 来源 id，也是本插件库的名字。
pub const OWNER: &str = "smelt.sessions";

/// 宿主启动时调一次，排在终端、对话插件之后（它们的视图先登记上，建会话时不用等）。
pub fn register(cx: &mut App) {
    let sessions = model::Sessions::install(cx);
    lifecycle::install(&sessions, cx);

    let views = vec![
        api::ViewSpec {
            name: view_name(contract::LIST).into(),
            title: "会话列表".into(),
            icon: gpui_kit_assets::IconName::MessagesSquare.path(),
            draws_top_bar: true,
            settings: None,
            instances: false,
            build: std::rc::Rc::new(list::build_view),
        },
        api::ViewSpec {
            name: view_name(contract::SESSION).into(),
            title: "会话".into(),
            icon: gpui_kit_assets::IconName::AppWindow.path(),
            draws_top_bar: true,
            settings: None,
            instances: false,
            build: std::rc::Rc::new(session_view::build_view),
        },
    ];
    if let Err(error) = api::register_views(OWNER, views, cx) {
        eprintln!("[sessions] 视图登记失败：{error}");
    }
    commands::register(cx);
}

/// 视图 id 里来源后面那段。
fn view_name(id: &'static str) -> &'static str {
    id.split_once('/').map_or(id, |(_, name)| name)
}

/// 出错给人看：记日志，发一条系统通知。
pub(crate) fn notify_error(message: &str, cx: &mut App) {
    eprintln!("[sessions] {message}");
    let notify = api::Notify {
        subtitle: "错误".to_string(),
        body: message.to_string(),
        ..Default::default()
    };
    api::NOTIFY.run(&notify, cx).detach();
}

/// 等一件事做完，失败了按 `what`（「关会话」这类）报给人看。
pub(crate) fn report_failure<T: 'static>(
    what: &'static str,
    ran: Task<Result<T, String>>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        if let Err(error) = ran.await {
            cx.update(|cx| notify_error(&format!("{what}失败：{error}"), cx));
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_names_drop_the_owner() {
        assert_eq!(view_name(contract::LIST), "list");
        assert_eq!(view_name(contract::SESSION), "session");
        assert!(contract::LIST.starts_with(&format!("{OWNER}/")));
    }
}
