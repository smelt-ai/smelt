//! 右键菜单里别的插件加的项，如会话列表里项目行的「用其他应用打开」。
//!
//! 菜单归画它的插件：它定菜单 id（`<来源>/<名字>`）和点了交给命令的上下文（项目在哪台
//! 机器、根目录这些），写进它自己对外的约定里。别的
//! 插件往这个 id 下登记项，画菜单的一方只按登记的画，不认识登记者。
//!
//! 一项点了执行一条命令，参数是登记时给的 `args` 加上画菜单那一方给的上下文（见
//! [`context_menu_args`]）。执行照常进命令队列，失败由宿主报。
//!
//! 按（来源，菜单）整组登记，再登记是整组替换：插件探测完、图标读到了，就把整组再登记
//! 一次；变了画菜单的一方把开着的菜单原地重建（见 [`observe_context_menu_items`]）。

use std::sync::Arc;

use gpui::{App, Global, SharedString, Subscription};

use crate::{RuntimeKey, Text};

/// 菜单里的一项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextMenuItem {
    /// 菜单上的字，跟着界面语言变（见 [`crate::Text`]）。
    pub label: Text,
    /// 彩色图标（如应用自己的图标），已经解码好：菜单直接画，不读文件。没有时画 `icon`。
    pub image: Option<Arc<gpui::Image>>,
    /// 线性图标的资源路径，如 `IconName::FolderOpen.path()`；都没有就不画图标。
    pub icon: Option<SharedString>,
    /// 只在这台机器的菜单里列（如只认本机目录的应用）；`None` 是哪台都列。
    pub machine: Option<RuntimeKey>,
    pub action: ContextMenuAction,
}

/// 点了做什么。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextMenuAction {
    /// 执行命令 `command`（`<来源>/<名字>`）。`args` 是 JSON 对象或 `null`，点的时候并上
    /// 菜单的上下文（见 [`context_menu_args`]）。命令撤下了这一项就不列。
    Run {
        command: String,
        args: serde_json::Value,
    },
    /// 展开一层子菜单。里面一项都列不出时这一项也不列。
    Submenu(Vec<ContextMenuItem>),
}

/// 一组登记：哪个来源往哪个菜单里放的。
struct Registration {
    owner: String,
    menu: String,
    items: Vec<ContextMenuItem>,
}

#[derive(Default)]
struct ContextMenuItems(Vec<Registration>);

impl Global for ContextMenuItems {}

/// 按（来源，菜单）整组登记：来源 `owner` 在菜单 `menu` 里原来的项全换成这一组，空的就是
/// 全撤下。第一次登记排在这个菜单的最后，再登记留在原来的位置。和原来一样就什么都不做，
/// 不惊动开着的菜单。有一项不合法（没有字、参数不是对象）整组不登记，原来的不动。
pub fn register_context_menu_items(
    owner: &str,
    menu: &str,
    items: Vec<ContextMenuItem>,
    cx: &mut App,
) -> Result<(), String> {
    if owner.is_empty() || owner.contains('/') {
        return Err(format!("菜单项来源「{owner}」不能为空，也不能含 `/`"));
    }
    if !menu.contains('/') {
        return Err(format!("菜单 id「{menu}」要写成 `<来源>/<名字>`"));
    }
    validate(&items)?;
    let current = cx.try_global::<ContextMenuItems>().and_then(|registry| {
        registry
            .0
            .iter()
            .find(|entry| entry.owner == owner && entry.menu == menu)
    });
    let unchanged = match current {
        Some(current) => current.items == items,
        None => items.is_empty(),
    };
    if unchanged {
        return Ok(());
    }
    let registry = &mut cx.default_global::<ContextMenuItems>().0;
    match registry
        .iter()
        .position(|entry| entry.owner == owner && entry.menu == menu)
    {
        Some(at) if items.is_empty() => {
            registry.remove(at);
        }
        Some(at) => registry[at].items = items,
        None => registry.push(Registration {
            owner: owner.to_string(),
            menu: menu.to_string(),
            items,
        }),
    }
    Ok(())
}

/// 撤下来源 `owner` 在各个菜单里登记的项（插件卸下时宿主调）。
pub fn unregister_context_menu_items(owner: &str, cx: &mut App) {
    let Some(registry) = cx.try_global::<ContextMenuItems>() else {
        return;
    };
    if !registry.0.iter().any(|entry| entry.owner == owner) {
        return;
    }
    cx.global_mut::<ContextMenuItems>()
        .0
        .retain(|entry| entry.owner != owner);
}

fn validate(items: &[ContextMenuItem]) -> Result<(), String> {
    for item in items {
        if item.label.get().trim().is_empty() {
            return Err("菜单项没有字".to_string());
        }
        match &item.action {
            ContextMenuAction::Run { command, args } => {
                if !command.contains('/') {
                    return Err(format!(
                        "菜单项「{}」的命令「{command}」要写成 `<来源>/<名字>`",
                        item.label
                    ));
                }
                if !(args.is_object() || args.is_null()) {
                    return Err(format!("菜单项「{}」的参数要是对象", item.label));
                }
            }
            ContextMenuAction::Submenu(children) => validate(children)?,
        }
    }
    Ok(())
}

/// 菜单 `menu` 画在机器 `machine` 的东西上时列哪些项：按登记顺序，跳过别的机器的、命令
/// 已经撤下的、空了的子菜单。只读内存，render 里可以调。
pub fn context_menu_items(menu: &str, machine: &RuntimeKey, cx: &App) -> Vec<ContextMenuItem> {
    let Some(registry) = cx.try_global::<ContextMenuItems>() else {
        return Vec::new();
    };
    let items: Vec<ContextMenuItem> = registry
        .0
        .iter()
        .filter(|entry| entry.menu == menu)
        .flat_map(|entry| entry.items.iter().cloned())
        .collect();
    shown(items, machine, cx)
}

fn shown(items: Vec<ContextMenuItem>, machine: &RuntimeKey, cx: &App) -> Vec<ContextMenuItem> {
    items
        .into_iter()
        .filter(|item| item.machine.as_ref().is_none_or(|only| only == machine))
        .filter_map(|mut item| {
            match &mut item.action {
                ContextMenuAction::Run { command, .. } => {
                    crate::command(command, cx)?;
                }
                ContextMenuAction::Submenu(children) => {
                    *children = shown(std::mem::take(children), machine, cx);
                    if children.is_empty() {
                        return None;
                    }
                }
            }
            Some(item)
        })
        .collect()
}

/// 点了一项交给命令 `command` 的参数：登记时给的 `args` 并上画菜单那一方给的 `context`（一个
/// 对象，形状见那个菜单的契约），同名字段以 `context` 为准。
///
/// 上下文是菜单知道的全部，命令只要其中几样：命令声明了认的参数（[`crate::CommandSpec::args`]）
/// 时只并它认的字段，免得多出来的字段让命令拒掉。不带参数的命令什么都不并，交 `null`。
pub fn context_menu_args(
    command: &str,
    args: &serde_json::Value,
    context: &serde_json::Value,
    cx: &App,
) -> serde_json::Value {
    let fields = crate::command(command, cx).and_then(|info| info.args);
    let mut merged = match args {
        serde_json::Value::Object(args) => args.clone(),
        _ => serde_json::Map::new(),
    };
    if let serde_json::Value::Object(context) = context {
        merged.extend(
            context
                .iter()
                .filter(|(key, _)| {
                    fields
                        .as_ref()
                        .is_none_or(|fields| fields.iter().any(|field| field == *key))
                })
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    if merged.is_empty() && fields.is_some_and(|fields| fields.is_empty()) {
        return serde_json::Value::Null;
    }
    serde_json::Value::Object(merged)
}

/// 登记的菜单项变了就调 `f`（哪个菜单的都算）。留着返回的订阅才一直收得到。
pub fn observe_context_menu_items(cx: &mut App, f: impl FnMut(&mut App) + 'static) -> Subscription {
    cx.observe_global::<ContextMenuItems>(f)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::{CommandSpec, Handled, register_commands, unregister_commands};

    const MENU: &str = "host/project";

    fn run(owner: &str, label: &str) -> ContextMenuItem {
        ContextMenuItem {
            label: label.to_string().into(),
            image: None,
            icon: Some("icons/folder-open.svg".into()),
            machine: None,
            action: ContextMenuAction::Run {
                command: format!("{owner}/open-with"),
                args: serde_json::json!({ "app": label }),
            },
        }
    }

    fn submenu(label: &str, children: Vec<ContextMenuItem>) -> ContextMenuItem {
        ContextMenuItem {
            label: label.to_string().into(),
            image: None,
            icon: None,
            machine: None,
            action: ContextMenuAction::Submenu(children),
        }
    }

    fn register_command(owner: &str, cx: &mut App) {
        let spec = CommandSpec {
            name: "open-with".to_string(),
            title: "用其他应用打开".into(),
            key: None,
            context: "Workspace".to_string(),
            palette: false,
            requires_args: true,
            args: None,
            handler: Rc::new(|_, _, _| Handled::ok()),
        };
        register_commands(owner, owner.to_string(), vec![spec], cx).unwrap();
    }

    fn labels(items: &[ContextMenuItem]) -> Vec<String> {
        items.iter().map(|item| item.label.to_string()).collect()
    }

    fn local(cx: &App) -> Vec<String> {
        labels(&context_menu_items(MENU, &RuntimeKey::Local, cx))
    }

    #[gpui::test]
    fn items_are_registered_per_owner_and_menu(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_command("ide", cx);
            register_command("mine", cx);
            register_context_menu_items("ide", MENU, vec![run("ide", "Finder")], cx).unwrap();
            register_context_menu_items("mine", MENU, vec![run("mine", "Vim")], cx).unwrap();
            register_context_menu_items("mine", "host/machine", vec![run("mine", "SSH")], cx)
                .unwrap();
            assert_eq!(local(cx), ["Finder", "Vim"]);

            // 再登记是整组替换，位置不变；别的菜单不受影响。
            let again = vec![run("ide", "Zed"), run("ide", "Finder")];
            register_context_menu_items("ide", MENU, again, cx).unwrap();
            assert_eq!(local(cx), ["Zed", "Finder", "Vim"]);
            let machine = context_menu_items("host/machine", &RuntimeKey::Local, cx);
            assert_eq!(labels(&machine), ["SSH"]);

            // 命令撤了就不列。
            unregister_commands("mine", cx);
            assert_eq!(local(cx), ["Zed", "Finder"]);

            register_context_menu_items("ide", MENU, Vec::new(), cx).unwrap();
            assert!(local(cx).is_empty());
        });
    }

    #[gpui::test]
    fn unregistering_an_owner_clears_it_from_every_menu(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_command("ide", cx);
            register_command("mine", cx);
            register_context_menu_items("ide", MENU, vec![run("ide", "Finder")], cx).unwrap();
            register_context_menu_items("mine", MENU, vec![run("mine", "Vim")], cx).unwrap();
            register_context_menu_items("mine", "host/machine", vec![run("mine", "SSH")], cx)
                .unwrap();
            unregister_context_menu_items("mine", cx);
            assert_eq!(local(cx), ["Finder"]);
            assert!(context_menu_items("host/machine", &RuntimeKey::Local, cx).is_empty());
        });
    }

    #[gpui::test]
    fn items_for_other_machines_and_empty_submenus_are_left_out(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_command("ide", cx);
            let mut only_local = run("ide", "Zed");
            only_local.machine = Some(RuntimeKey::Local);
            let items = vec![
                submenu("用其他应用打开", vec![only_local]),
                submenu("空的", vec![run("gone", "Vim")]),
                run("ide", "Finder"),
            ];
            register_context_menu_items("ide", MENU, items, cx).unwrap();

            let here = context_menu_items(MENU, &RuntimeKey::Local, cx);
            assert_eq!(labels(&here), ["用其他应用打开", "Finder"]);
            let ContextMenuAction::Submenu(children) = &here[0].action else {
                panic!("应是子菜单");
            };
            assert_eq!(labels(children), ["Zed"]);

            let remote = RuntimeKey::Remote("box".to_string());
            assert_eq!(labels(&context_menu_items(MENU, &remote, cx)), ["Finder"]);
        });
    }

    #[gpui::test]
    fn an_invalid_item_rejects_the_whole_set(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_command("ide", cx);
            register_context_menu_items("ide", MENU, vec![run("ide", "Zed")], cx).unwrap();
            let mut bad_args = run("ide", "Vim");
            bad_args.action = ContextMenuAction::Run {
                command: "ide/open-with".to_string(),
                args: serde_json::json!([1]),
            };
            let nested = submenu("更多", vec![run("ide", " ")]);
            for items in [vec![bad_args], vec![nested]] {
                assert!(register_context_menu_items("ide", MENU, items, cx).is_err());
                assert_eq!(local(cx), ["Zed"]);
            }
            assert!(register_context_menu_items("ide", "project", Vec::new(), cx).is_err());
            assert!(register_context_menu_items("a/b", MENU, Vec::new(), cx).is_err());
        });
    }

    #[gpui::test]
    fn only_real_changes_reach_observers(cx: &mut gpui::TestAppContext) {
        let changes = Rc::new(Cell::new(0));
        let _subscription = cx.update(|cx| {
            register_command("ide", cx);
            let seen = changes.clone();
            observe_context_menu_items(cx, move |_| seen.set(seen.get() + 1))
        });
        cx.update(|cx| {
            register_context_menu_items("ide", MENU, vec![run("ide", "Zed")], cx).unwrap()
        });
        cx.run_until_parked();
        assert_eq!(changes.get(), 1);

        cx.update(|cx| {
            register_context_menu_items("ide", MENU, vec![run("ide", "Zed")], cx).unwrap();
            // 没登记过的撤下也不算变。
            register_context_menu_items("nobody", MENU, Vec::new(), cx).unwrap();
        });
        cx.run_until_parked();
        assert_eq!(changes.get(), 1);
    }

    #[gpui::test]
    fn context_fields_win_over_registered_args(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            // 没声明参数的命令拿到全部上下文。
            register_command("ide", cx);
            let args = serde_json::json!({ "app": "zed", "path": "stale" });
            let context = serde_json::json!({ "machine": null, "path": "/work" });
            assert_eq!(
                context_menu_args("ide/open-with", &args, &context, cx),
                serde_json::json!({ "app": "zed", "machine": null, "path": "/work" })
            );
            assert_eq!(
                context_menu_args("ide/open-with", &serde_json::Value::Null, &context, cx),
                context
            );
        });
    }

    #[gpui::test]
    fn context_is_cut_to_the_fields_a_command_takes(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let declared = |name: &str, fields: &[&str]| CommandSpec {
                name: name.to_string(),
                title: name.to_string().into(),
                key: None,
                context: "Workspace".to_string(),
                palette: false,
                requires_args: false,
                args: Some(fields.iter().map(|field| field.to_string()).collect()),
                handler: Rc::new(|_, _, _| Handled::ok()),
            };
            register_commands(
                "ide",
                "ide",
                vec![
                    declared("open-with", &["app", "path"]),
                    declared("refresh", &[]),
                ],
                cx,
            )
            .unwrap();
            let context = serde_json::json!({ "machine": null, "path": "/work", "name": "w" });
            assert_eq!(
                context_menu_args(
                    "ide/open-with",
                    &serde_json::json!({ "app": "zed" }),
                    &context,
                    cx
                ),
                serde_json::json!({ "app": "zed", "path": "/work" })
            );
            assert_eq!(
                context_menu_args("ide/refresh", &serde_json::Value::Null, &context, cx),
                serde_json::Value::Null
            );
        });
    }
}
