//! 会话列表机器行的右键菜单（[`MACHINE_MENU`]）里远程机器的几项，以及它们开的对话框：
//! 重新连接、升级运行时、重命名…、中继…、移除…。本机不给：本机不用配对，不能移除。
//!
//! 跟着 `HostEvent::MachinesChanged` 整组重登：重新连接只给连不上、版本对不上的，升级
//! 只给能远程升级的，都看宿主给的连接表。没有定时器。

use api::ui::TextInput;
use api::ui::dialog::Confirm;
use api::{
    Connection, ContextMenuAction, ContextMenuItem, MachineTarget, RenameMachine, RuntimeKey,
    SetMachineRelay,
};
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Task, Window, div, rgb,
};
use smelt_plugin_api as api;
use smelt_plugin_api::ui::field::{InputEvent, InputState};

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

/// 菜单项点了执行的那几条命令，和本插件别的命令一起登记（见 `crate::register_commands`）。
pub(crate) fn commands() -> Vec<api::CommandSpec> {
    vec![
        RECONNECT.spec_async("重新连接机器", |args, _, cx| {
            let target = MachineTarget {
                machine: args.machine,
            };
            api::RECONNECT_MACHINE.run(&target, cx)
        }),
        UPGRADE.spec_async("升级机器的运行时", |args, _, cx| {
            let target = MachineTarget {
                machine: args.machine,
            };
            api::UPGRADE_MACHINE.run(&target, cx)
        }),
        RENAME.spec("重命名机器", |args, window, cx| {
            let (connection, window) = dialog_target(&args.machine, window, cx)?;
            rename(connection, window, cx);
            Ok(())
        }),
        EDIT_RELAY.spec("改机器的中继", |args, window, cx| {
            let (connection, window) = dialog_target(&args.machine, window, cx)?;
            edit_relay(connection, window, cx);
            Ok(())
        }),
        REMOVE.spec("移除机器", |args, window, cx| {
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
    let window = window.ok_or_else(|| "主窗口没开".to_string())?;
    let connection = connection(machine, cx).ok_or_else(|| api::MACHINE_REMOVED.to_string())?;
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
    let item = |label: &'static str, icon: &'static str, command: &str| ContextMenuItem {
        label: label.into(),
        image: None,
        icon: Some(icon.into()),
        machine: Some(connection.machine.clone()),
        action: ContextMenuAction::Run {
            command: command.to_string(),
            args: api::serde_json::Value::Null,
        },
    };
    let mut items = Vec::new();
    // 连不上、版本对不上时不必等退避：立刻再试。
    if connection.can_reconnect {
        items.push(item("重新连接", "icons/refresh-cw.svg", RECONNECT.id()));
    }
    // 连得上就能升，不管兼不兼容：升到这个桌面的版本。
    if connection.can_upgrade {
        items.push(item("升级运行时", "icons/arrow-up.svg", UPGRADE.id()));
    }
    items.push(item("重命名…", "smelt-icons/square-pen.svg", RENAME.id()));
    items.push(item("中继…", "icons/globe.svg", EDIT_RELAY.id()));
    items.push(item("移除…", "smelt-icons/trash.svg", REMOVE.id()));
    items
}

/// 命令在后台跑完，没成就发通知。
pub(crate) fn report_failure(what: &'static str, task: Task<Result<(), String>>, cx: &mut App) {
    cx.spawn(async move |cx| {
        if let Err(error) = task.await {
            cx.update(|cx| notify_failure(what, error, cx));
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
    Confirm::new("确定移除这台机器吗？", "移除", move |_, cx| {
        let target = MachineTarget {
            machine: machine.clone(),
        };
        let removed = api::REMOVE_MACHINE.run(&target, cx);
        report_failure("移除机器失败", removed, cx);
        true
    })
    .danger()
    .description(format!(
        "这台电脑不再连「{name}」，侧栏里它的项目和会话会一起移走。那台机器上的会话照常运行，重新配对后还能回来。"
    ))
    .open(window, cx);
}

/// 「重命名…」：名字只在这台电脑上生效。回车或点「重命名」提交。
fn rename(connection: Connection, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| RenameForm::new(connection, window, cx));
    let input = form.read(cx).input.clone();
    let body = input.clone();
    Confirm::new("重命名机器", "重命名", move |_, cx| {
        form.update(cx, |form, cx| form.submit(cx));
        true
    })
    .description("侧栏和设置里显示这个名字，只在这台电脑上生效。")
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
        report_failure("重命名机器失败", renamed, cx);
    }
}

/// 「中继…」：在线时要等那台机器改好才关，没改成就把原因留在框里。在线和离线改的东西不
/// 一样，说明里讲清楚。
fn edit_relay(connection: Connection, window: &mut Window, cx: &mut App) {
    let online = matches!(connection.state, api::ConnectionState::Online { .. });
    let hint = if online {
        "那台机器会改用这个中继，这台电脑也跟着改。填自建 iroh relay 的域名、IP 或 URL；留空用 n0 的公共 relay。"
    } else {
        "那台机器现在连不上，只改这台电脑这边：那台机器要已经用 smeltd relay 改成同一个。"
    };
    let title = format!("{} 的中继", connection.name);
    let form = cx.new(|cx| RelayForm::new(connection, window, cx));
    let save = form.clone();
    let body = form.clone();
    let closed = form.clone();
    Confirm::new(title, "保存", move |window, cx| {
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
                .placeholder("留空用 n0 的公共 relay")
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
                Err(error) if !shown => cx.update(|cx| notify_failure("改中继失败", error, cx)),
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
                    .child("正在保存…")
            }))
            .children(
                self.error
                    .clone()
                    .map(|error| div().type_style(TYPE.ui).text_color(bad).child(error)),
            )
    }
}
