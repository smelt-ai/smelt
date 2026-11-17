//! 列表上的菜单：会话、项目、工作树、机器（挂在「项目」标题上），「添加项目」选机器，和
//! 「筛选与分组」。项目、机器菜单里还有别的插件登记进来的项
//! （[`contract::PROJECT_MENU`]、[`contract::MACHINE_MENU`]，如「在 IDE 里打开」、机器的重连、
//! 改名），点了按登记的命令执行，参数并上菜单的上下文。

use gpui::{App, ClipboardItem, Entity, SharedString};
use gpui_kit_assets::IconName;
use smelt_plugin_api::ui::menu::{Menu, MenuItem};
use smelt_plugin_api::{self as api, ContextMenuAction, ContextMenuItem, RuntimeKey};

use super::actions;
use super::view::{ListView, RenameTarget};
use super::{ProjectGroup, SessionRow, Sidebar, WorksetGroup};
use crate::contract::{self, MachineMenuContext, ProjectMenuContext};
use crate::dialogs;
use crate::model::{Sessions, SidebarGrouping};
use crate::worksets::WorksetKey;

/// 会话行的菜单。
pub(super) fn session_menu(row: &SessionRow, view: Entity<ListView>) -> Menu {
    let pin = {
        let key = row.key.clone();
        let group = row.group.clone();
        let pinned = row.pinned;
        MenuItem::new(if pinned { "取消置顶" } else { "置顶" })
            .icon(if pinned {
                IconName::PinOff
            } else {
                IconName::Pin
            })
            .on_click(move |_, _, cx| actions::pin_session(&key, &group, !pinned, cx))
    };
    let rename = {
        let key = row.key.clone();
        let title = row.title.to_string();
        MenuItem::new("重命名")
            .icon(IconName::Pencil)
            .on_click(move |_, window, cx| {
                let (key, title) = (key.clone(), title.clone());
                view.update(cx, |view, cx| {
                    view.start_rename(RenameTarget::Conversation(key), title, window, cx)
                });
            })
    };
    let archive = {
        let (key, title, status) = (row.key.clone(), row.title.clone(), row.status);
        MenuItem::new("归档")
            .icon(IconName::Archive)
            .on_click(move |_, window, cx| {
                actions::archive_session(key.clone(), title.clone(), status, window, cx)
            })
    };
    let copy_id = {
        let id = row.session_id.clone();
        MenuItem::new("复制会话 ID")
            .icon(IconName::Copy)
            .disabled(id.is_none())
            .on_click(move |_, _, cx| {
                if let Some(id) = &id {
                    cx.write_to_clipboard(ClipboardItem::new_string(id.clone()));
                }
            })
    };
    let close = {
        let (key, title, status) = (row.key.clone(), row.title.clone(), row.status);
        MenuItem::new("关闭")
            .icon(IconName::Close)
            .on_click(move |_, window, cx| {
                actions::close_session(key.clone(), title.clone(), status, window, cx)
            })
    };
    Menu::new()
        .item(pin)
        .item(rename)
        .separator()
        .item(archive)
        .item(copy_id)
        .separator()
        .item(close)
}

/// 项目行的菜单。`root` 为空的那组（没有工作目录的会话）只有置顶。
pub(super) fn project_menu(project: &ProjectGroup, cx: &App) -> Menu {
    let machine = project.machine.clone();
    let root = project.root.clone();
    let mut menu = Menu::new();
    if !root.is_empty() {
        let new_session = {
            let (machine, root) = (machine.clone(), root.clone());
            MenuItem::new("新建会话")
                .icon(IconName::Plus)
                .on_click(move |_, window, cx| {
                    dialogs::open_new_session(Some(machine.clone()), Some(root.clone()), window, cx)
                })
        };
        let repositories = {
            let (machine, root) = (machine.clone(), root.clone());
            MenuItem::new("管理项目仓库…")
                .icon(IconName::FolderGit2)
                .on_click(move |_, window, cx| {
                    dialogs::open_project_repositories(machine.clone(), root.clone(), window, cx)
                })
        };
        let new_worktree = {
            let (machine, root) = (machine.clone(), root.clone());
            MenuItem::new("新建工作树…")
                .icon(IconName::GitBranch)
                .on_click(move |_, window, cx| {
                    dialogs::open_new_worktree(machine.clone(), root.clone(), window, cx)
                })
        };
        menu = menu.item(new_session).item(new_worktree).item(repositories);
        let context = ProjectMenuContext {
            machine: machine.clone(),
            path: root.clone(),
        };
        let plugin_items = plugin_items(contract::PROJECT_MENU, &machine, &context, cx);
        if !plugin_items.is_empty() {
            menu = menu.separator();
            for item in plugin_items {
                menu = menu.item(item);
            }
        }
        let copy_path = {
            let root = root.clone();
            MenuItem::new("复制项目路径")
                .icon(IconName::Copy)
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(root.clone()))
                })
        };
        menu = menu.separator().item(copy_path);
        if let Some(branch) = project.branch.clone() {
            menu = menu.item(
                MenuItem::new("复制分支名")
                    .icon(IconName::GitBranch)
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(branch.to_string()))
                    }),
            );
        }
    }
    // 远程项目带上那台机器的菜单：机器连不上时它的项目变灰，右键就能重新连接。
    if let Some(name) = &project.machine_name
        && let Some(machine_menu) = machine_menu(&machine, cx)
    {
        menu = menu
            .separator()
            .item(MenuItem::new(format!("机器「{name}」")).submenu(machine_menu));
    }
    let pin = {
        let (machine, root, pinned) = (machine.clone(), root.clone(), project.pinned);
        MenuItem::new(if pinned { "取消置顶" } else { "置顶" })
            .icon(if pinned {
                IconName::PinOff
            } else {
                IconName::Pin
            })
            .on_click(move |_, _, cx| actions::pin_project(&machine, &root, !pinned, cx))
    };
    menu = menu.separator().item(pin);
    if !root.is_empty() {
        let (name, sessions) = (project.name.clone(), project.sessions);
        menu = menu.separator().item(
            MenuItem::new(if project.opened {
                "关闭项目"
            } else {
                "关闭这组会话"
            })
            .icon(IconName::Close)
            .on_click(move |_, window, cx| {
                actions::close_project(
                    machine.clone(),
                    root.clone(),
                    name.clone(),
                    sessions,
                    window,
                    cx,
                )
            }),
        );
    }
    menu
}

/// 工作树行的菜单。
pub(super) fn workset_menu(workset: &WorksetGroup, view: Entity<ListView>) -> Menu {
    let machine = workset.machine.clone();
    let id = workset.id.clone();
    let new_session = {
        let (machine, path) = (machine.clone(), workset.path.clone());
        MenuItem::new("新建会话")
            .icon(IconName::Plus)
            .on_click(move |_, window, cx| {
                dialogs::open_new_session(Some(machine.clone()), Some(path.clone()), window, cx)
            })
    };
    let pin = {
        let (machine, id, pinned) = (machine.clone(), id.clone(), workset.pinned);
        MenuItem::new(if pinned { "取消置顶" } else { "置顶" })
            .icon(if pinned {
                IconName::PinOff
            } else {
                IconName::Pin
            })
            .on_click(move |_, _, cx| actions::pin_workset(&machine, &id, !pinned, cx))
    };
    let rename = {
        let (machine, id, task) = (machine.clone(), id.clone(), workset.task.to_string());
        MenuItem::new("重命名工作树…")
            .icon(IconName::Pencil)
            .on_click(move |_, window, cx| {
                let target = RenameTarget::Workset {
                    machine: machine.clone(),
                    id: id.clone(),
                };
                let task = task.clone();
                view.update(cx, |view, cx| view.start_rename(target, task, window, cx));
            })
    };
    let pull_request = {
        let open = |machine: RuntimeKey, root: String| {
            move |_: &gpui::ClickEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
                actions::open_pull_request(machine.clone(), root.clone(), cx)
            }
        };
        let item = MenuItem::new("在浏览器中创建 PR").icon(IconName::ExternalLink);
        match workset.repositories.as_slice() {
            [] => item.disabled(true),
            [(_, root)] => item.on_click(open(machine.clone(), root.clone())),
            repositories => {
                item.submenu(repositories.iter().fold(Menu::new(), |menu, (name, root)| {
                    menu.item(
                        MenuItem::new(name.clone()).on_click(open(machine.clone(), root.clone())),
                    )
                }))
            }
        }
    };
    let check = {
        let key = WorksetKey {
            machine: machine.clone(),
            id: id.clone(),
        };
        MenuItem::new(if workset.checking {
            "正在检查合并状态…"
        } else {
            "检查合并状态"
        })
        .icon(IconName::RefreshCw)
        .disabled(workset.checking)
        .on_click(move |_, _, cx| actions::check_workset(key.clone(), cx))
    };
    let finish = {
        let task = workset.task.clone();
        MenuItem::new("关闭工作树…")
            .icon(IconName::Close)
            .on_click(move |_, window, cx| {
                actions::finish_workset(machine.clone(), id.clone(), task.clone(), window, cx)
            })
    };
    Menu::new()
        .item(new_session)
        .separator()
        .item(pin)
        .item(rename)
        .separator()
        .item(pull_request)
        .item(check)
        .item(finish)
}

/// 「筛选与分组」：分组依据（项目、状态、时间）和显示哪些项目（全部、有会话），当前的打勾。
pub(super) fn grouping_menu(grouping: SidebarGrouping, hide_empty: bool) -> Menu {
    let group = |mode: SidebarGrouping, label: &'static str| {
        MenuItem::new(label)
            .checked(grouping == mode)
            .on_click(move |_, _, cx| {
                Sessions::global(cx).update(cx, |sessions, cx| sessions.set_grouping(mode, cx))
            })
    };
    let show = |hide: bool, label: &'static str| {
        MenuItem::new(label)
            .checked(hide_empty == hide)
            .on_click(move |_, _, cx| {
                Sessions::global(cx).update(cx, |sessions, cx| {
                    sessions.set_hide_empty_projects(hide, cx)
                })
            })
    };
    Menu::new()
        .label("分组依据")
        .item(group(SidebarGrouping::Project, "项目"))
        .item(group(SidebarGrouping::Status, "状态"))
        .item(group(SidebarGrouping::LastUpdated, "时间"))
        .separator()
        .label("显示")
        .item(show(false, "全部"))
        .item(show(true, "有会话"))
}

/// 「项目」标题上的机器菜单。挂着远程机器时每台机器一个子菜单（没有项的不列）；只有本机
/// 时直接是本机的项。一项都没有是 `None`，不画「⋯」。
pub(super) fn machines_menu(sidebar: &Sidebar, cx: &App) -> Option<Menu> {
    if !sidebar.has_remote() {
        return machine_menu(&RuntimeKey::Local, cx);
    }
    let items: Vec<MenuItem> = sidebar
        .machines
        .iter()
        .filter_map(|machine| {
            let menu = machine_menu(&machine.key, cx)?;
            Some(
                MenuItem::new(machine.name.clone())
                    .icon_path(machine.os_icon.clone())
                    .icon_color(machine.link.light.color())
                    .submenu(menu),
            )
        })
        .collect();
    (!items.is_empty()).then(|| items.into_iter().fold(Menu::new(), Menu::item))
}

/// 有没有机器菜单（画不画「⋯」、标题接不接右键）。只查登记表，不建菜单。
pub(super) fn has_machines_menu(sidebar: &Sidebar, cx: &App) -> bool {
    sidebar
        .machines
        .iter()
        .any(|machine| has_machine_menu(&machine.key, cx))
}

/// 挂着远程机器时「添加项目」先选在哪台机器上打开。连不上的机器挑不了目录，灰着。
pub(super) fn open_project_menu(sidebar: &Sidebar) -> Menu {
    sidebar.machines.iter().fold(
        Menu::new().label("在哪台机器上打开"),
        |menu, machine| {
            let key = machine.key.clone();
            menu.item(
                MenuItem::new(machine.name.clone())
                    .icon_path(machine.os_icon.clone())
                    .icon_color(machine.link.light.color())
                    .disabled(machine.link.unavailable.is_some())
                    .on_click(move |_, _, cx| actions::open_project(key.clone(), cx)),
            )
        },
    )
}

/// 一台机器的菜单：全是别的插件登记的（机器插件的重连、升级、改名、移除，能力库、远程桌面
/// 等）。一项都没有是 `None`，不画「⋯」。
pub(super) fn machine_menu(machine: &RuntimeKey, cx: &App) -> Option<Menu> {
    let context = MachineMenuContext {
        machine: machine.clone(),
    };
    let items = plugin_items(contract::MACHINE_MENU, machine, &context, cx);
    if items.is_empty() {
        return None;
    }
    Some(items.into_iter().fold(Menu::new(), Menu::item))
}

/// 这台机器有没有菜单。只查登记表，不建菜单。
pub(super) fn has_machine_menu(machine: &RuntimeKey, cx: &App) -> bool {
    !api::context_menu_items(contract::MACHINE_MENU, machine, cx).is_empty()
}

/// 别的插件往 `menu` 里登记的项，按这台机器筛过。
fn plugin_items(
    menu: &str,
    machine: &RuntimeKey,
    context: &impl serde::Serialize,
    cx: &App,
) -> Vec<MenuItem> {
    let context = match serde_json::to_value(context) {
        Ok(context) => context,
        Err(error) => {
            eprintln!("[sessions] 菜单上下文转不成 JSON：{error}");
            return Vec::new();
        }
    };
    api::context_menu_items(menu, machine, cx)
        .into_iter()
        .map(|item| plugin_item(item, &context))
        .collect()
}

/// 一项登记的菜单项。有彩色图标（如应用自己的图标）画它，没有画线性图标。
fn plugin_item(item: ContextMenuItem, context: &serde_json::Value) -> MenuItem {
    let mut entry = MenuItem::new(item.label);
    if let Some(icon) = item.icon {
        entry = entry.icon_path(icon);
    }
    if let Some(image) = item.image {
        entry = entry.image(image);
    }
    match item.action {
        ContextMenuAction::Run { command, args } => {
            let args = api::context_menu_args(&args, context);
            entry.on_click(move |_, _, cx| run_plugin_command(&command, args.clone(), cx))
        }
        ContextMenuAction::Submenu(items) => entry.submenu(
            items
                .into_iter()
                .map(|item| plugin_item(item, context))
                .fold(Menu::new(), Menu::item),
        ),
    }
}

fn run_plugin_command(command: &str, args: serde_json::Value, cx: &mut App) {
    let command: SharedString = command.to_string().into();
    let ran = api::run(&command, args, cx);
    cx.spawn(async move |cx| {
        if let Err(error) = ran.await {
            cx.update(|cx| crate::notify_error(&format!("「{command}」没做成：{error}"), cx));
        }
    })
    .detach();
}
