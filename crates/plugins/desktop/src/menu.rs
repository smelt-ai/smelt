//! 机器菜单里的「远程桌面」（[`MACHINE_MENU`]）：只给装了远程桌面、眼下连着的远程机器
//! 登记一项，点了在出口里打开那台机器的画面。
//!
//! 装没装是问那台机器的运行时（`system.describe` 报的 `desktop`，安装时
//! `install.sh --with-desktop` 定的）。跟着 [`HostEvent::MachinesChanged`] 走：一台机器连上
//! 了问一次，掉线、移除了就忘掉，下次连上再问。重装、升级运行时都要断开重连，装没装只会在
//! 那时变，所以连着时不再问。没有定时器。本机不给：看的是别处那台机器的屏幕。

use std::collections::HashMap;

use api::client::DEFAULT_REQUEST_TIMEOUT;
use api::client::protocol::control::{EmptyParams, SystemDescribe};
use api::{ContextMenuAction, ContextMenuItem, HostEvent, RuntimeKey};
use gpui::{App, Global, Task};
use rust_i18n::t;
use smelt_plugin_api::{self as api, text};

use crate::{ICON, OWNER, VIEW};

// 约定：smelt.sessions 的 MACHINE_MENU 和它的上下文，权威定义在
// crates/plugins/sessions/src/contract.rs。
const MACHINE_MENU: &str = "smelt.sessions/machine";

/// [`MACHINE_MENU`] 点了交给命令的上下文：右键的是哪台机器。
#[derive(serde::Deserialize)]
struct MachineMenuContext {
    #[serde(with = "api::machine_json")]
    machine: RuntimeKey,
}

/// 菜单项点了执行的命令。参数是菜单的上下文：右键的是哪台机器。
const OPEN: api::Command<MachineMenuContext, ()> = api::Command::new("smelt.desktop/open");

/// 在线的远程机器装没装远程桌面。不在线的、移除了的不在表里。
#[derive(Default)]
struct Machines {
    probes: HashMap<RuntimeKey, Probe>,
    /// 下一次询问的编号。
    next_ask: u64,
}

impl Global for Machines {}

enum Probe {
    /// 问了还没回来。带着编号：掉线又连上后，上一次连着时问的晚回来，认得出来丢掉。
    Asking(u64),
    Known {
        desktop: bool,
    },
}

/// 插件登记时调一次。
pub(crate) fn register(cx: &mut App) {
    cx.set_global(Machines::default());
    api::register_commands(
        OWNER,
        text!("title"),
        vec![OPEN.spec_async(text!("command.open"), |args, _, cx| open(args, cx))],
        cx,
    )
    .expect("远程桌面的命令表写错了");
    sync(cx);
    let events = api::events(cx);
    cx.subscribe(&events, |_, event: &HostEvent, cx| {
        if matches!(event, HostEvent::MachinesChanged) {
            sync(cx);
        }
    })
    .detach();
}

fn open(args: MachineMenuContext, cx: &mut App) -> Task<Result<(), String>> {
    let opened = api::OPEN_VIEW.run(
        &api::OpenView {
            view: VIEW.to_string(),
            args: api::serde_json::json!({ "machine": api::machine_to_json(&args.machine) }),
        },
        cx,
    );
    cx.spawn(async move |_| match opened.await? {
        true => Ok(()),
        false => Err(t!("error.no_place").into_owned()),
    })
}

/// 对上眼下在线的远程机器：新连上的去问，掉线、移除了的忘掉，再把菜单项登记一遍。
fn sync(cx: &mut App) {
    let host = api::host(cx);
    let online: Vec<RuntimeKey> = host
        .machines(cx)
        .into_iter()
        .map(|machine| machine.key)
        .filter(|key| *key != RuntimeKey::Local && host.machine_online(key, cx))
        .collect();
    let machines = cx.global_mut::<Machines>();
    machines.probes.retain(|key, _| online.contains(key));
    let mut asks = Vec::new();
    for key in online {
        if !machines.probes.contains_key(&key) {
            machines.next_ask += 1;
            machines
                .probes
                .insert(key.clone(), Probe::Asking(machines.next_ask));
            asks.push((key, machines.next_ask));
        }
    }
    for (key, ask) in asks {
        ask_desktop(key, ask, cx);
    }
    publish(cx);
}

/// 在后台问那台机器装没装远程桌面，回来记下、重新登记菜单项。问不到当没装：机器眼下
/// 连着，问不到多半是正在掉线，掉线再连上会重问。
fn ask_desktop(machine: RuntimeKey, ask: u64, cx: &mut App) {
    let described = api::call::<SystemDescribe>(
        &machine,
        EmptyParams::default(),
        DEFAULT_REQUEST_TIMEOUT,
        cx,
    );
    cx.spawn(async move |cx| {
        let desktop = match described.await {
            Ok(described) => described.daemon.desktop == Some(true),
            Err(error) => {
                api::app_log::warn("desktop", &format!("问不到机器装没装远程桌面：{error}"));
                false
            }
        };
        cx.update(|cx| {
            let Some(probe) = cx.global_mut::<Machines>().probes.get_mut(&machine) else {
                return;
            };
            // 问的这段时间里掉线了、又连上重问了：这一份没人要了。
            if !matches!(probe, Probe::Asking(asked) if *asked == ask) {
                return;
            }
            *probe = Probe::Known { desktop };
            publish(cx);
        });
    })
    .detach();
}

/// 装了远程桌面的机器各一项，按宿主列机器的顺序。和上次一样时什么都不惊动。
fn publish(cx: &mut App) {
    let machines = cx.global::<Machines>();
    let items: Vec<ContextMenuItem> = api::host(cx)
        .machines(cx)
        .into_iter()
        .filter(|machine| {
            matches!(
                machines.probes.get(&machine.key),
                Some(Probe::Known { desktop: true })
            )
        })
        .map(|machine| ContextMenuItem {
            label: text!("title"),
            image: None,
            icon: Some(ICON.into()),
            machine: Some(machine.key),
            action: ContextMenuAction::Run {
                command: OPEN.id().to_string(),
                args: api::serde_json::Value::Null,
            },
        })
        .collect();
    api::register_context_menu_items(OWNER, MACHINE_MENU, items, cx)
        .expect("远程桌面的菜单项写错了");
}
