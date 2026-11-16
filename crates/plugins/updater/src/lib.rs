//! 更新与帮助：设置「系统 → 关于」那一页（版本、在线更新、帮助与反馈），在线更新的状态机，
//! 以及「检查更新」「帮助文档」「反馈问题」几条命令和菜单项。
//!
//! 在线更新只在 macOS 和 Windows 上有（[`update`]）：启动时恢复上次没装完的更新、静默检查，
//! 有新版就在「关于」页挂角标，宿主据此在「设置」入口上标红点。退出时有待装的包，经退出
//! 守卫（[`api::register_quit_guard`]）接手，装上再退。别的系统上这一页只有版本和帮助。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 和宿主打交道。

mod page;
#[cfg(any(target_os = "macos", windows))]
mod store;
#[cfg(any(target_os = "macos", windows))]
mod update;

use std::rc::Rc;

use api::{CommandSpec, Handled, MenuEntry, MenuSlot};
use gpui::App;
use smelt_plugin_api as api;

/// 本插件的来源 id。
const OWNER: &str = "smelt.updater";
/// 「关于」那一页，`<来源>/<名字>`。
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
const ABOUT_VIEW: &str = "smelt.updater/about";

/// 「反馈问题」跳转的链接。打包时设 `SMELT_FEEDBACK_URL` 换成自己的反馈渠道。
const FEEDBACK_URL: &str = build_time_url(
    option_env!("SMELT_FEEDBACK_URL"),
    "https://github.com/smelt-ai/smelt/issues/new/choose",
);

/// 「帮助文档」的链接。打包时设 `SMELT_DOCS_URL` 换掉。
const DOCS_URL: &str = build_time_url(option_env!("SMELT_DOCS_URL"), "https://smelt.onoo.io/");

/// 源码仓库。
const SOURCE_URL: &str = "https://github.com/smelt-ai/smelt";

/// 打包时没设或设成空串，都用开源版的默认地址。
const fn build_time_url(value: Option<&'static str>, default: &'static str) -> &'static str {
    match value {
        Some(url) if !url.is_empty() => url,
        _ => default,
    }
}

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    #[cfg(any(target_os = "macos", windows))]
    update::register(cx);
    page::register(cx);
    register_commands(cx);
}

fn command(name: &str, title: &str, run: impl Fn(&mut App) + 'static) -> CommandSpec {
    CommandSpec {
        name: name.into(),
        title: title.into(),
        key: None,
        context: "Workspace".into(),
        palette: true,
        requires_args: false,
        handler: Rc::new(move |_, _, cx| {
            run(cx);
            Handled::ok()
        }),
    }
}

fn register_commands(cx: &mut App) {
    let mut commands = Vec::new();
    // 顺手发起一次检查，再打开「关于」页看进度。
    #[cfg(any(target_os = "macos", windows))]
    commands.push(command("check-update", "检查更新", |cx| {
        if let Some(updater) = update::updater(cx) {
            updater.update(cx, |updater, cx| updater.request_check(cx));
        }
        open_about(cx);
    }));
    commands.push(command("docs", "帮助文档", |cx| cx.open_url(DOCS_URL)));
    commands.push(command("report-issue", "反馈问题", |cx| {
        cx.open_url(FEEDBACK_URL)
    }));
    api::register_commands(OWNER, "更新与帮助", commands, cx).expect("更新与帮助的命令表写错了");

    let mut menu = Vec::new();
    #[cfg(target_os = "macos")]
    menu.push(MenuEntry {
        slot: MenuSlot::App,
        title: "检查更新…".into(),
        command: "smelt.updater/check-update".into(),
    });
    menu.push(MenuEntry {
        slot: MenuSlot::Help,
        title: "帮助文档".into(),
        command: "smelt.updater/docs".into(),
    });
    menu.push(MenuEntry {
        slot: MenuSlot::Help,
        title: "反馈问题".into(),
        command: "smelt.updater/report-issue".into(),
    });
    api::register_menu_items(OWNER, menu, cx);
}

/// 打开「关于」页。
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
