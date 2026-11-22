//! 会话列表机器行的右键菜单（[`MACHINE_MENU`]）里远程机器的几项，以及它们开的对话框：
//! 重新连接、升级运行时、重命名…、中继…、移除…。本机不给：本机不用配对，不能移除。
//!
//! 跟着 `HostEvent::MachinesChanged` 整组重登：重新连接只给连不上、版本对不上的，升级
//! 只给能远程升级的，都看宿主给的连接表。没有定时器。

use api::ui::dialog::Confirm;
use api::ui::{IconName, TextInput};
use api::{
    Connection, ContextMenuAction, ContextMenuItem, MachineTarget, RenameMachine, RuntimeKey,
    SetMachineRelay,
};
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Task, Window, div, rgb,
};
use rust_i18n::t;
use smelt_plugin_api::ui::field::{InputEvent, InputState};
use smelt_plugin_api::{self as api, text};

use crate::OWNER;
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

// 约定：smelt.sessions 的 MACHINE_MENU 和它的上下文，权威定义在
// crates/plugins/sessions/src/contract.rs。
const MACHINE_MENU: &str = "smelt.sessions/machine";

/// [`MACHINE_MENU`] 点了交给命令的上下文：右键的是哪台机器。
#[derive(serde::Deserialize)]
struct MachineMenuContext {
    #[serde(with = "api::machine_json")]
    machine: RuntimeKey,
}

/// 菜单项点了执行的命令。参数是菜单的上下文。
const RECONNECT: api::Command<MachineMenuContext, ()> =
    api::Command::new("smelt.machines/reconnect");
const UPGRADE: api::Command<MachineMenuContext, ()> = api::Command::new("smelt.machines/upgrade");
const RENAME: api::Command<MachineMenuContext, ()> = api::Command::new("smelt.machines/rename");
const EDIT_RELAY: api::Command<MachineMenuContext, ()> =
    api::Command::new("smelt.machines/edit-relay");
const REMOVE: api::Command<MachineMenuContext, ()> = api::Command::new("smelt.machines/remove");
const AGENT_ACCESS: api::Command<MachineMenuContext, ()> =
    api::Command::new("smelt.machines/agent-access");

/// 菜单项点了执行的那几条命令，和本插件别的命令一起登记（见 `crate::register_commands`）。
pub(crate) fn commands() -> Vec<api::CommandSpec> {
    vec![
        RECONNECT.spec_async(text!("command.reconnect"), |args, _, cx| {
            let target = MachineTarget {
                machine: args.machine,
            };
            api::RECONNECT_MACHINE.run(&target, cx)
        }),
        UPGRADE.spec_async(text!("command.upgrade"), |args, _, cx| {
            let target = MachineTarget {
                machine: args.machine,
            };
            api::UPGRADE_MACHINE.run(&target, cx)
        }),
        RENAME.spec(text!("command.rename"), |args, window, cx| {
            let (connection, window) = dialog_target(&args.machine, window, cx)?;
            rename(connection, window, cx);
            Ok(())
        }),
        EDIT_RELAY.spec(text!("command.edit_relay"), |args, window, cx| {
            let (connection, window) = dialog_target(&args.machine, window, cx)?;
            edit_relay(connection, window, cx);
            Ok(())
        }),
        // 翻转：允许 ↔ 不允许。还没迁到本机运行时的机器没有这一项。
        AGENT_ACCESS.spec_async(text!("command.agent_access"), |args, _, cx| {
            let Some(allowed) = connection(&args.machine, cx).and_then(|c| c.agent_access) else {
                return Task::ready(Err(api::machine_removed()));
            };
            let target = api::SetMachineAgentAccess {
                machine: args.machine,
                allowed: !allowed,
            };
            let changed = api::SET_MACHINE_AGENT_ACCESS.run(&target, cx);
            cx.spawn(async move |cx| {
                let result = changed.await;
                if let Err(error) = &result {
                    cx.update(|cx| notify_failure(&t!("agent_access.failed"), error.clone(), cx));
                }
                result
            })
        }),
        REMOVE.spec(text!("command.remove"), |args, window, cx| {
            let (connection, window) = dialog_target(&args.machine, window, cx)?;
            confirm_remove(connection.machine, connection.name, window, cx);
            Ok(())
        }),
    ]
}

/// 开对话框要的：那台机器眼下的连接，和主窗口。
fn dialog_target<'w>(
    machine: &RuntimeKey,
    window: Option<&'w mut Window>,
    cx: &App,
) -> Result<(Connection, &'w mut Window), String> {
    let window = window.ok_or_else(|| t!("error.no_window").into_owned())?;
    let connection = connection(machine, cx).ok_or_else(api::machine_removed)?;
    Ok((connection, window))
}

fn connection(machine: &RuntimeKey, cx: &App) -> Option<Connection> {
    api::host(cx)
        .connections(cx)
        .remotes
        .into_iter()
        .find(|connection| connection.machine == *machine)
}

/// 每台远程机器一组，按宿主列的顺序。和上次一样时宿主什么都不惊动。机器表变了再调。
pub(crate) fn publish(cx: &mut App) {
    let items: Vec<ContextMenuItem> = api::host(cx)
        .connections(cx)
        .remotes
        .into_iter()
        .flat_map(machine_items)
        .collect();
    if let Err(error) = api::register_context_menu_items(OWNER, MACHINE_MENU, items, cx) {
        api::app_log::warn("machines", &format!("机器菜单项登记失败：{error}"));
    }
}

fn machine_items(connection: Connection) -> Vec<ContextMenuItem> {
    let item = |label: api::Text, icon: IconName, command: &str| ContextMenuItem {
        label,
        image: None,
        icon: Some(icon.path()),
        machine: Some(connection.machine.clone()),
        action: ContextMenuAction::Run {
            command: command.to_string(),
            args: api::serde_json::Value::Null,
        },
    };
    let mut items = Vec::new();
    // 连不上、版本对不上时不必等退避：立刻再试。
    if connection.can_reconnect {
        items.push(item(
            text!("menu.reconnect"),
            IconName::Refresh,
            RECONNECT.id(),
        ));
    }
    // 连得上就能升，不管兼不兼容：升到这个桌面的版本。
    if connection.can_upgrade {
        items.push(item(text!("menu.upgrade"), IconName::ArrowUp, UPGRADE.id()));
    }
    // 本机智能体经 `smelt --machine` 用它，默认不许；文字说的是点了会怎样。
    match connection.agent_access {
        Some(false) => items.push(item(
            text!("menu.allow_agents"),
            IconName::Bot,
            AGENT_ACCESS.id(),
        )),
        Some(true) => items.push(item(
            text!("menu.disallow_agents"),
            IconName::Bot,
            AGENT_ACCESS.id(),
        )),
        None => {}
    }
    items.push(item(text!("menu.rename"), IconName::SquarePen, RENAME.id()));
    items.push(item(text!("menu.relay"), IconName::Globe, EDIT_RELAY.id()));
    items.push(item(text!("menu.remove"), IconName::Trash, REMOVE.id()));
    items
}

/// 命令在后台跑完，没成就发通知。
pub(crate) fn report_failure(what: String, task: Task<Result<(), String>>, cx: &mut App) {
    cx.spawn(async move |cx| {
        if let Err(error) = task.await {
            cx.update(|cx| notify_failure(&what, error, cx));
        }
    })
    .detach();
}

/// 没成：记日志，发一条通知。
pub(crate) fn notify_failure(what: &str, error: String, cx: &mut App) {
    api::app_log::warn("machines", &format!("{what}：{error}"));
    let notify = api::Notify {
        subtitle: what.to_string(),
        body: error,
        ..Default::default()
    };
    api::NOTIFY.run(&notify, cx).detach();
}

/// 移除前问一句。设置「远程」页的「移除」也走这里。
pub(crate) fn confirm_remove(machine: RuntimeKey, name: String, window: &mut Window, cx: &mut App) {
    Confirm::new(t!("remove.title"), t!("remove.action"), move |_, cx| {
        let target = MachineTarget {
            machine: machine.clone(),
        };
        let removed = api::REMOVE_MACHINE.run(&target, cx);
        report_failure(t!("remove.failed").into_owned(), removed, cx);
        true
    })
    .danger()
    .description(t!("remove.description", name = name))
    .open(window, cx);
}

/// 「重命名…」：名字只在这台电脑上生效。回车或点「重命名」提交。
fn rename(connection: Connection, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| RenameForm::new(connection, window, cx));
    let input = form.read(cx).input.clone();
    let body = input.clone();
    Confirm::new(t!("rename.title"), t!("rename.action"), move |_, cx| {
        form.update(cx, |form, cx| form.submit(cx));
        true
    })
    .description(t!("rename.description"))
    .body(move |_, _| TextInput::new(&body).into_any_element())
    .open(window, cx);
    focus_input(input, window, cx);
}

/// 对话框开好了再聚焦，名字整段选中：直接打字就是换掉，按方向键才是在原名上改。
fn focus_input(input: Entity<InputState>, window: &mut Window, cx: &mut App) {
    window.defer(cx, move |window, cx| {
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
    });
}

struct RenameForm {
    machine: RuntimeKey,
    current: String,
    input: Entity<InputState>,
    /// 回车和对话框的确认可能前后各来一次，只提交一次。
    submitted: bool,
    _enter: Subscription,
}

impl RenameForm {
    fn new(connection: Connection, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(connection.name.clone()));
        let enter = cx.subscribe_in(&input, window, |form, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                form.submit(cx);
                api::ui::dialog::close(window, cx);
            }
        });
        Self {
            machine: connection.machine,
            current: connection.name,
            input,
            submitted: false,
            _enter: enter,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.submitted {
            return;
        }
        self.submitted = true;
        let name = self.input.read(cx).value().trim().to_string();
        // 空的不收，和原来一样的不必写。
        if name.is_empty() || name == self.current {
            return;
        }
        let renamed = api::RENAME_MACHINE.run(
            &RenameMachine {
                machine: self.machine.clone(),
                name,
            },
            cx,
        );
        report_failure(t!("rename.failed").into_owned(), renamed, cx);
    }
}

/// 「中继…」：在线时要等那台机器改好才关，没改成就把原因留在框里。在线和离线改的东西不
/// 一样，说明里讲清楚。
fn edit_relay(connection: Connection, window: &mut Window, cx: &mut App) {
    let online = matches!(connection.state, api::ConnectionState::Online { .. });
    let hint = if online {
        t!("relay.hint_online")
    } else {
        t!("relay.hint_offline")
    };
    let title = t!("relay.title", name = connection.name);
    let form = cx.new(|cx| RelayForm::new(connection, window, cx));
    let save = form.clone();
    let body = form.clone();
    let closed = form.clone();
    Confirm::new(title, t!("relay.save"), move |window, cx| {
        save.update(cx, |form, cx| form.save(window, cx));
        // 等那台机器回话，改成了由 [`RelayForm::save`] 关。
        false
    })
    .description(hint)
    .body(move |_, _| body.clone().into_any_element())
    .on_close(move |_, cx| closed.update(cx, |form, _| form.open = false))
    .open(window, cx);
    focus_input(form.read(cx).input.clone(), window, cx);
}

struct RelayForm {
    machine: RuntimeKey,
    input: Entity<InputState>,
    saving: bool,
    error: Option<String>,
    /// 对话框还开着。关了以后等回来的结果不再去关它（那时最上面可能是别的对话框）。
    open: bool,
    _enter: Subscription,
}

impl RelayForm {
    fn new(connection: Connection, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("relay.placeholder"))
                .default_value(connection.relay.clone().unwrap_or_default())
        });
        let enter = cx.subscribe_in(&input, window, |form, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                form.save(window, cx);
            }
        });
        Self {
            machine: connection.machine,
            input,
            saving: false,
            error: None,
            open: true,
            _enter: enter,
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.saving = true;
        self.error = None;
        let saved = api::SET_MACHINE_RELAY.run(
            &SetMachineRelay {
                machine: self.machine.clone(),
                relay: self.input.read(cx).value().to_string(),
            },
            cx,
        );
        let dialog_window = window.window_handle();
        cx.notify();
        cx.spawn(async move |form, cx| {
            let result = saved.await;
            // 框还开着：成了关掉，没成把原因留在框里。框已经关了（点了取消），没成就发通知。
            let shown = form
                .update(cx, |form, cx| {
                    form.saving = false;
                    if !form.open {
                        return false;
                    }
                    match &result {
                        Ok(()) => form.open = false,
                        Err(error) => form.error = Some(error.clone()),
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            match result {
                Ok(()) if shown => {
                    let _ = dialog_window
                        .update(cx, |_, window, cx| api::ui::dialog::close(window, cx));
                }
                Ok(()) => {}
                Err(error) if !shown => {
                    cx.update(|cx| notify_failure(&t!("relay.failed"), error, cx))
                }
                Err(_) => {}
            }
        })
        .detach();
    }
}

impl Render for RelayForm {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let muted = rgb(api::ui::ui_theme::muted());
        let bad = rgb(api::ui::ui_theme::bad());
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(TextInput::new(&self.input).disabled(self.saving))
            .children(self.saving.then(|| {
                div()
                    .type_style(TYPE.ui)
                    .text_color(muted)
                    .child(t!("relay.saving").into_owned())
            }))
            .children(
                self.error
                    .clone()
                    .map(|error| div().type_style(TYPE.ui).text_color(bad).child(error)),
            )
    }
}
