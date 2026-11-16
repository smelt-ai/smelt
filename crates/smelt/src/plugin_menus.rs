//! 右键菜单里插件登记的项（`smelt_plugin_api::register_context_menu_items`）怎么画：如会话
//! 列表项目行的「用其他应用打开」。画菜单的地方只给菜单 id、机器和上下文，照登记的画，点了
//! 执行登记的命令，失败报出来。
//!
//! 开着的子菜单记在这里（只持弱引用），登记变了（插件探测完、图标读到了）原地重建；顶层的
//! 项要等下次打开菜单才跟上。第一次画出子菜单时才订登记变化。

use gpui::*;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::*;
use smelt_plugin_api::{ContextMenuAction, ContextMenuItem, RuntimeKey};

/// 菜单画在哪：哪个菜单（如 `contracts::sessions::PROJECT_MENU`）、右键的东西在哪台机器上、
/// 点了交给命令的上下文（形状见那个菜单的契约）。
#[derive(Clone)]
pub(crate) struct PluginMenuTarget {
    pub menu: &'static str,
    pub machine: RuntimeKey,
    pub context: serde_json::Value,
}

/// 把插件登记在 `target.menu` 下的项接在 `menu` 后面；`separated` 时有项才先隔一条线。只读
/// 内存。
pub(crate) fn plugin_menu_items(
    menu: PopupMenu,
    target: PluginMenuTarget,
    separated: bool,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let items = smelt_plugin_api::context_menu_items(target.menu, &target.machine, cx);
    let menu = if separated && !items.is_empty() {
        menu.separator()
    } else {
        menu
    };
    entries(menu, items, &target, &[], window, cx)
}

/// 开着的一层子菜单。
struct OpenSubmenu {
    popup: WeakEntity<PopupMenu>,
    window: AnyWindowHandle,
    target: PluginMenuTarget,
    /// 从菜单顶层到这层子菜单一路的项名，登记变了按它重新找到这层的项。
    path: Vec<SharedString>,
}

#[derive(Default)]
struct OpenSubmenus {
    open: Vec<OpenSubmenu>,
    _watch: Option<Subscription>,
}

impl Global for OpenSubmenus {}

/// 把一层的项排进菜单。`path` 是这一层的位置。同一层里有一项带彩色图标，这一层的命令项都
/// 自己画图标，对得齐；不然照宿主的项挂线性图标。
fn entries(
    mut menu: PopupMenu,
    items: Vec<ContextMenuItem>,
    target: &PluginMenuTarget,
    path: &[SharedString],
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let own_icons = items.iter().any(|item| item.image.is_some());
    for item in items {
        let icon = item.icon.clone().map(|path| Icon::default().path(path));
        match item.action {
            ContextMenuAction::Run { command, args } => {
                let args = smelt_plugin_api::context_menu_args(&args, &target.context);
                let entry = if own_icons {
                    let image = item.image;
                    let label = item.label;
                    // ElementItem 不经过 PopupMenu 的 Icon/SVG 通道，彩色图标直接作为菜单行的
                    // 一部分画出来。
                    PopupMenuItem::element(move |_window, _cx| {
                        let row = h_flex().flex_1().items_center().gap_x_2();
                        let row = match (image.clone(), icon.clone()) {
                            (Some(image), _) => {
                                row.child(img(image).size(px(17.)).object_fit(ObjectFit::Contain))
                            }
                            (None, Some(icon)) => row.child(icon.size(px(16.))),
                            (None, None) => row,
                        };
                        row.child(label.clone())
                    })
                } else {
                    match icon {
                        Some(icon) => PopupMenuItem::new(item.label).icon(icon),
                        None => PopupMenuItem::new(item.label),
                    }
                };
                menu = menu.item(entry.on_click(move |_ev, _window, cx| {
                    let ran = smelt_plugin_api::run(&command, args.clone(), cx);
                    crate::workspace_commands::report_failure(ran, cx);
                }));
            }
            ContextMenuAction::Submenu(children) => {
                let mut path = path.to_vec();
                path.push(item.label.clone());
                let target = target.clone();
                menu = menu.submenu_with_icon(
                    icon,
                    item.label,
                    window,
                    cx,
                    move |menu, window, cx| {
                        track(
                            OpenSubmenu {
                                popup: cx.entity().downgrade(),
                                window: window.window_handle(),
                                target: target.clone(),
                                path: path.clone(),
                            },
                            cx,
                        );
                        entries(menu, children.clone(), &target, &path, window, cx)
                    },
                );
            }
        }
    }
    menu
}

fn track(submenu: OpenSubmenu, cx: &mut App) {
    if cx
        .try_global::<OpenSubmenus>()
        .is_none_or(|submenus| submenus._watch.is_none())
    {
        let watch = smelt_plugin_api::observe_context_menu_items(cx, rebuild);
        cx.default_global::<OpenSubmenus>()._watch = Some(watch);
    }
    let submenus = &mut cx.default_global::<OpenSubmenus>().open;
    submenus.retain(|open| open.popup.upgrade().is_some());
    submenus.push(submenu);
}

/// 登记的菜单项变了：开着的子菜单按新的原地重建，关掉的丢掉。那层整个撤下了就留着旧的，
/// 点了照常报错。重建时里面的子菜单会重新登记自己，所以先把整张表取出来。
fn rebuild(cx: &mut App) {
    let open = std::mem::take(&mut cx.default_global::<OpenSubmenus>().open);
    let mut kept = Vec::new();
    for submenu in open {
        let Some(popup) = submenu.popup.upgrade() else {
            continue;
        };
        let items =
            smelt_plugin_api::context_menu_items(submenu.target.menu, &submenu.target.machine, cx);
        if let Some(items) = submenu_items(items, &submenu.path) {
            let _ = submenu.window.update(cx, |_, window, cx| {
                popup.update(cx, |menu, cx| {
                    menu.rebuild(window, cx, |menu, window, cx| {
                        entries(menu, items, &submenu.target, &submenu.path, window, cx)
                    });
                });
            });
        }
        kept.push(submenu);
    }
    cx.default_global::<OpenSubmenus>().open.extend(kept);
}

/// 按 `path` 一层层找到那层子菜单的项；那层没了是 `None`。
fn submenu_items(
    mut items: Vec<ContextMenuItem>,
    path: &[SharedString],
) -> Option<Vec<ContextMenuItem>> {
    for label in path {
        let item = items.into_iter().find(|item| item.label == *label)?;
        let ContextMenuAction::Submenu(children) = item.action else {
            return None;
        };
        items = children;
    }
    Some(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(label: &str) -> ContextMenuItem {
        ContextMenuItem {
            label: label.to_string().into(),
            image: None,
            icon: None,
            machine: None,
            action: ContextMenuAction::Run {
                command: "test/run".to_string(),
                args: serde_json::Value::Null,
            },
        }
    }

    fn submenu(label: &str, children: Vec<ContextMenuItem>) -> ContextMenuItem {
        ContextMenuItem {
            action: ContextMenuAction::Submenu(children),
            ..run(label)
        }
    }

    fn labels(items: Option<Vec<ContextMenuItem>>) -> Option<Vec<String>> {
        items.map(|items| {
            items
                .into_iter()
                .map(|item| item.label.to_string())
                .collect()
        })
    }

    #[test]
    fn a_submenu_is_found_again_by_the_labels_on_its_path() {
        let items = vec![
            run("复制"),
            submenu(
                "打开",
                vec![submenu("编辑器", vec![run("Zed"), run("VS Code")])],
            ),
        ];
        assert_eq!(
            labels(submenu_items(
                items.clone(),
                &["打开".into(), "编辑器".into()]
            )),
            Some(vec!["Zed".to_string(), "VS Code".to_string()])
        );
        assert_eq!(
            labels(submenu_items(items.clone(), &[])).map(|top| top.len()),
            Some(2)
        );
        // 那层没了、或那一项不再是子菜单，都找不到。
        assert_eq!(labels(submenu_items(items.clone(), &["关闭".into()])), None);
        assert_eq!(labels(submenu_items(items, &["复制".into()])), None);
    }
}
