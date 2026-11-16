//! 应用菜单栏里插件加的项：macOS 顶部的「Smelt」菜单和「帮助」菜单。一项就是一条命令，
//! 点了执行它，快捷键照键表里那条命令的显示。菜单栏只有宿主设得了，宿主在命令登记变了时
//! 重设（[`crate::HostEvent::CommandsChanged`]），这里登记变了也发同一个事件。
//!
//! 只有 macOS 有菜单栏；别的系统登记了也不显示，入口要另外放（比如设置页里）。

use gpui::{App, Global, SharedString};

use crate::HostEvent;

/// 放进哪个菜单。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuSlot {
    /// 应用菜单（「Smelt」），排在版本号下面、「设置…」上面。
    App,
    /// 「帮助」菜单。没人往里放就没有这个菜单。
    Help,
}

/// 一个菜单项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuEntry {
    pub slot: MenuSlot,
    pub title: SharedString,
    /// 点了执行的命令 id（`<来源>/<名字>`），不带参数。
    pub command: SharedString,
}

#[derive(Default)]
struct MenuItems(Vec<(String, MenuEntry)>);

impl Global for MenuItems {}

/// 登记来源 `owner` 的菜单项，整组换掉它原来的那些；排在先登记的来源后面。
pub fn register_menu_items(owner: &str, entries: Vec<MenuEntry>, cx: &mut App) {
    let items = &mut cx.default_global::<MenuItems>().0;
    items.retain(|(entry, _)| entry != owner);
    items.extend(entries.into_iter().map(|entry| (owner.to_string(), entry)));
    crate::emit(HostEvent::CommandsChanged, cx);
}

/// 撤下来源 `owner` 的菜单项。
pub fn unregister_menu_items(owner: &str, cx: &mut App) {
    let Some(items) = cx.try_global::<MenuItems>() else {
        return;
    };
    if !items.0.iter().any(|(entry, _)| entry == owner) {
        return;
    }
    cx.global_mut::<MenuItems>()
        .0
        .retain(|(entry, _)| entry != owner);
    crate::emit(HostEvent::CommandsChanged, cx);
}

/// 宿主设菜单栏时读：登记过的全部菜单项，按登记的次序。
pub fn menu_items(cx: &App) -> Vec<MenuEntry> {
    cx.try_global::<MenuItems>()
        .map(|items| items.0.iter().map(|(_, entry)| entry.clone()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(slot: MenuSlot, title: &str) -> MenuEntry {
        MenuEntry {
            slot,
            title: title.to_string().into(),
            command: format!("probe/{title}").into(),
        }
    }

    #[gpui::test]
    fn items_are_replaced_per_owner(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_menu_items("a", vec![entry(MenuSlot::Help, "文档")], cx);
            register_menu_items("b", vec![entry(MenuSlot::App, "检查")], cx);
            register_menu_items("a", vec![entry(MenuSlot::Help, "反馈")], cx);
            let titles: Vec<_> = menu_items(cx)
                .into_iter()
                .map(|entry| entry.title.to_string())
                .collect();
            assert_eq!(titles, ["检查", "反馈"]);
            unregister_menu_items("b", cx);
            assert_eq!(menu_items(cx).len(), 1);
        });
    }
}
