//! 机器能做的事：侧栏里每台机器那一段标题的 `⋯` 和右键菜单。
//!
//! 添加、移除机器和看连接状态在设置的「机器」页（`SettingsSection::CollaborationMachines`）。

use std::rc::Rc;

use gpui::*;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::{Icon, IconName};

use crate::runtimes::Runtime;
use crate::{RenameTarget, RuntimeKey, Workspace};

type ActionHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// 机器的一项操作。
struct MachineAction {
    label: &'static str,
    icon: Icon,
    /// 和前一项之间隔一条线。
    separated: bool,
    run: ActionHandler,
}

/// 一台机器能做的事。远程机器多出重命名和移除。
fn machine_actions(
    runtime: Runtime,
    workspace: Entity<Workspace>,
    cx: &App,
) -> Vec<MachineAction> {
    let key = runtime.key().clone();
    let mut actions = vec![
        MachineAction {
            label: "打开项目…",
            icon: Icon::empty().path("smelt-icons/folder-plus.svg"),
            separated: false,
            run: Rc::new({
                let runtime = runtime.clone();
                let workspace = workspace.clone();
                move |_window, cx| {
                    let runtime = runtime.clone();
                    workspace.update(cx, |ws, cx| ws.open_project_on(runtime, cx));
                }
            }),
        },
        MachineAction {
            label: "能力库",
            icon: IconName::LayoutDashboard.into(),
            separated: false,
            run: Rc::new({
                let key = key.clone();
                let workspace = workspace.clone();
                move |window, cx| {
                    let key = key.clone();
                    workspace.update(cx, |ws, cx| ws.open_capabilities_on(key, window, cx));
                }
            }),
        },
    ];
    // 只有装了远程桌面、眼下连着的机器才有。
    if crate::runtimes::runtime_has_desktop(&key, cx) {
        actions.push(MachineAction {
            label: "远程桌面",
            icon: Icon::empty().path("smelt-icons/monitor.svg"),
            separated: false,
            run: Rc::new({
                let workspace = workspace.clone();
                move |window, cx| {
                    let runtime = runtime.clone();
                    workspace.update(cx, |ws, cx| ws.open_desktop(runtime, window, cx));
                }
            }),
        });
    }
    // 连不上、版本对不上时不必等退避：立刻再试，运行时旧就再请它升级一次。
    if crate::runtimes::runtime_can_reconnect(&key, cx) {
        actions.insert(
            0,
            MachineAction {
                label: "重新连接",
                icon: IconName::Redo.into(),
                separated: false,
                run: Rc::new({
                    let key = key.clone();
                    move |_window, cx| crate::runtime_events::reconnect(&key, cx)
                }),
            },
        );
    }
    // 连得上就能升，不管兼不兼容：升到这个桌面的版本。
    if crate::runtimes::runtime_can_upgrade(&key, cx) {
        actions.push(MachineAction {
            label: "升级运行时",
            icon: IconName::ArrowUp.into(),
            separated: false,
            run: Rc::new({
                let key = key.clone();
                move |_window, cx| crate::runtime_events::upgrade_remote(&key, cx)
            }),
        });
    }
    let RuntimeKey::Remote(runtime_id) = key else {
        return actions;
    };
    actions.push(MachineAction {
        label: "重命名…",
        icon: Icon::empty().path("smelt-icons/square-pen.svg"),
        separated: true,
        run: Rc::new({
            let runtime_id = runtime_id.clone();
            let workspace = workspace.clone();
            move |window, cx| {
                let target = RenameTarget::Runtime(runtime_id.clone());
                workspace.update(cx, |ws, cx| ws.start_rename(target, window, cx));
            }
        }),
    });
    actions.push(MachineAction {
        label: "中继…",
        icon: IconName::Globe.into(),
        separated: false,
        run: Rc::new({
            let runtime_id = runtime_id.clone();
            let workspace = workspace.clone();
            move |window, cx| {
                let runtime_id = runtime_id.clone();
                workspace.update(cx, |ws, cx| ws.start_relay_edit(runtime_id, window, cx));
            }
        }),
    });
    actions.push(MachineAction {
        label: "移除…",
        icon: Icon::empty().path("smelt-icons/trash.svg"),
        separated: false,
        run: Rc::new(move |_window, cx| {
            let runtime_id = runtime_id.clone();
            workspace.update(cx, |ws, cx| ws.request_remove_runtime(runtime_id, cx));
        }),
    });
    actions
}

/// 侧栏分段标题的 `⋯` 和右键用的机器菜单。
pub(crate) fn machine_menu(
    menu: PopupMenu,
    runtime: Runtime,
    workspace: Entity<Workspace>,
    cx: &App,
) -> PopupMenu {
    machine_actions(runtime, workspace, cx)
        .into_iter()
        .fold(menu, |menu, action| {
            let menu = if action.separated {
                menu.separator()
            } else {
                menu
            };
            let run = action.run;
            menu.item(
                PopupMenuItem::new(action.label)
                    .icon(action.icon)
                    .on_click(move |_ev, window, cx| run(window, cx)),
            )
        })
}
