//! 别的插件往这里的右键菜单登记的项（`smelt_plugin_api::register_context_menu_items`），画成
//! 菜单项。菜单 id 和点了并进命令参数的上下文见 `contract.rs`。

use std::rc::Rc;

use gpui::{App, SharedString};
use rust_i18n::t;
use smelt_plugin_api::ui::menu::{Menu, MenuItem};
use smelt_plugin_api::{self as api, ContextMenuAction, ContextMenuItem, RuntimeKey};

/// 点了一项之后怎么报：点下时 `None`（上次的失败收起，这是新的一次），没做成时是给人看的
/// 原因。失败挂在发起的地方（那条消息、那格终端），不发系统通知。
pub(crate) type Report = Rc<dyn Fn(Option<SharedString>, &mut App)>;

/// 在 `menu` 后面接上别的插件往 `id` 登记、给机器 `machine` 的项，前面隔一道线；一项都
/// 没有原样交回。
pub(crate) fn with_plugin_items(
    menu: Menu,
    id: &str,
    machine: &RuntimeKey,
    context: &impl serde::Serialize,
    report: Report,
    cx: &App,
) -> Menu {
    let context = match api::serde_json::to_value(context) {
        Ok(context) => context,
        Err(error) => {
            eprintln!("[terminal] 菜单上下文转不成 JSON：{error}");
            return menu;
        }
    };
    let items = api::context_menu_items(id, machine, cx);
    if items.is_empty() {
        return menu;
    }
    items
        .into_iter()
        .map(|item| plugin_item(item, &context, &report, cx))
        .fold(menu.separator(), Menu::item)
}

/// 一项登记的菜单项。有彩色图标（如应用自己的图标）画它，没有画线性图标。
fn plugin_item(
    item: ContextMenuItem,
    context: &api::serde_json::Value,
    report: &Report,
    cx: &App,
) -> MenuItem {
    let mut entry = MenuItem::new(item.label.get());
    if let Some(icon) = item.icon {
        entry = entry.icon_path(icon);
    }
    if let Some(image) = item.image {
        entry = entry.image(image);
    }
    match item.action {
        ContextMenuAction::Run { command, args } => {
            let args = api::context_menu_args(&command, &args, context, cx);
            let report = report.clone();
            entry.on_click(move |_, _, cx| run(&command, args.clone(), report.clone(), cx))
        }
        ContextMenuAction::Submenu(items) => entry.submenu(
            items
                .into_iter()
                .map(|item| plugin_item(item, context, report, cx))
                .fold(Menu::new(), Menu::item),
        ),
    }
}

/// 执行点中的那项，结果交给 `report`。
fn run(command: &str, args: api::serde_json::Value, report: Report, cx: &mut App) {
    report(None, cx);
    let command: SharedString = command.to_string().into();
    let ran = api::run(&command, args, cx);
    cx.spawn(async move |cx| {
        let Err(error) = ran.await else {
            return;
        };
        let reason = t!("error.command_failed", command = command, error = error).into_owned();
        eprintln!("[terminal] {reason}");
        cx.update(|cx| report(Some(reason.into()), cx));
    })
    .detach();
}
