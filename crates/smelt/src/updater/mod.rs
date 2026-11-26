//! 应用在线更新与关于（沿用 `smelt.updater` 以保留 `~/.smelt/plugin-data/smelt.updater/store.sqlite3`）：
//! 设置「系统 → 关于」页、应用/帮助菜单项、更新命令、退出守卫与「关于」页角标。

mod page;
#[cfg(any(target_os = "macos", windows))]
mod store;
#[cfg(any(target_os = "macos", windows))]
mod update;

use std::rc::Rc;

use gpui::App;
use smelt_plugin_api::{self as api, CommandSpec, Handled, MenuEntry, MenuSlot, text};

pub(crate) use page::{handle_action, snapshot};
#[cfg(any(target_os = "macos", windows))]
pub(crate) use update::{
    cancel_pending_dialog, confirm_pending_dialog, pending_confirm_dialog, updater,
};

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn pending_confirm_dialog(_cx: &App) -> Option<smelt_nsh::ConfirmDialogState> {
    None
}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn confirm_pending_dialog(_cx: &mut App) {}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn cancel_pending_dialog(_cx: &mut App) {}

/// 更新偏好库、视图与退出守卫的来源 id（沿用 `smelt.updater` 保留已有用户偏好库）。
pub(crate) const OWNER: &str = "smelt.updater";
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
const ABOUT_VIEW: &str = "smelt.updater/about";

/// 「反馈问题」跳转的链接。打包时设 `SMELT_FEEDBACK_URL` 换成自己的反馈渠道。
pub(crate) const FEEDBACK_URL: &str = build_time_url(
    option_env!("SMELT_FEEDBACK_URL"),
    "https://github.com/smelt-ai/smelt/issues/new/choose",
);

/// 「帮助文档」的链接。打包时设 `SMELT_DOCS_URL` 换掉。
pub(crate) const DOCS_URL: &str =
    build_time_url(option_env!("SMELT_DOCS_URL"), "https://smelt.onoo.io/");

/// 源码仓库。
pub(crate) const SOURCE_URL: &str = "https://github.com/smelt-ai/smelt";

const fn build_time_url(value: Option<&'static str>, default: &'static str) -> &'static str {
    match value {
        Some(url) if !url.is_empty() => url,
        _ => default,
    }
}

/// 宿主启动时调一次（在 `api::install` 之后、开主窗口之前）：启动在线更新生命周期并登记「关于」页与帮助菜单。
pub(crate) fn register(cx: &mut App) {
    #[cfg(any(target_os = "macos", windows))]
    update::register(cx);
    page::register(cx);
    register_commands(cx);
}

fn command(name: &str, title: api::Text, run: impl Fn(&mut App) + 'static) -> CommandSpec {
    CommandSpec {
        name: name.into(),
        title,
        key: None,
        context: "Workspace".into(),
        requires_args: false,
        args: Some(Vec::new()),
        cli: false,
        handler: Rc::new(move |_, _, cx| {
            run(cx);
            Handled::ok()
        }),
    }
}

fn register_commands(cx: &mut App) {
    let mut commands = Vec::new();
    #[cfg(any(target_os = "macos", windows))]
    commands.push(command("check-update", text!("updater.check.idle"), |cx| {
        if let Some(updater) = update::updater(cx) {
            updater.update(cx, |updater, cx| updater.request_check(cx));
        }
        open_about(cx);
    }));
    commands.push(command("docs", text!("updater.docs"), |cx| {
        cx.open_url(DOCS_URL)
    }));
    commands.push(command(
        "report-issue",
        text!("updater.report_issue"),
        |cx| cx.open_url(FEEDBACK_URL),
    ));
    if let Err(error) = api::register_commands(OWNER, text!("updater.group"), commands, cx) {
        api::app_log::error("updater", &format!("更新与帮助命令登记失败：{error}"));
    }

    let mut menu = Vec::new();
    #[cfg(target_os = "macos")]
    menu.push(MenuEntry {
        slot: MenuSlot::App,
        title: text!("updater.check_menu"),
        command: "smelt.updater/check-update".into(),
    });
    menu.push(MenuEntry {
        slot: MenuSlot::Help,
        title: text!("updater.docs"),
        command: "smelt.updater/docs".into(),
    });
    menu.push(MenuEntry {
        slot: MenuSlot::Help,
        title: text!("updater.report_issue"),
        command: "smelt.updater/report-issue".into(),
    });
    api::register_menu_items(OWNER, menu, cx);
}

#[cfg(any(target_os = "macos", windows))]
fn open_about(cx: &mut App) {
    let opened = api::OPEN_VIEW.run(
        &api::OpenView {
            view: ABOUT_VIEW.to_string(),
            args: api::serde_json::Value::Null,
        },
        cx,
    );
    cx.spawn(async move |_: &mut gpui::AsyncApp| {
        if let Err(error) = opened.await {
            api::app_log::error("updater", &format!("打开「关于」页失败：{error}"));
        }
    })
    .detach();
}
