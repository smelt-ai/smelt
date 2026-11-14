//! 通知：会话提醒的系统通知、通知设置页、菜单栏和 Dock 角标。
//!
//! 提醒由各台机器的运行时（smeltd）判定，经单独订阅的 `attention.changed` 推来，桌面、手机
//! 读同一份；这里只照它镜像（[`attention`]），不自己判定。推来的变化带「该响一下」的那条
//! 时发系统通知：
//!
//! - 设置里关了这类、或者人正看着这段会话（主窗口在前台、舞台上露着它、翻在最新处），不发，
//!   这段会话之前那条也撤掉；正看着的顺手告诉运行时看过了。
//! - 能标已读的会话发一条带 id 的通知，点了跳到那段会话；运行时说它没有未读了（哪台设备上
//!   看过、回过），撤掉。发过哪些记在库里，重启后也撤得掉。
//! - 没处标已读的（自动化运行、命令行发的）发一次性的，不挂着等撤。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 和宿主打交道：通知经 `smelt.app/notify`、
//! `smelt.app/withdraw-notification` 交给宿主发，看过了经 `attention.mark_read` 告诉运行时。
//! 什么时候做全靠事件，没有定时器。

mod attention;
mod menubar;
mod settings_page;
mod store;

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use api::model::control_api::{AttentionMarkRead, AttentionMarkReadParams};
use api::model::daemon_state::{AttentionItem, CORE_TOPIC_ATTENTION_CHANGED, SessionAttention};
use api::sessions::{self, Activate};
use api::{HostEvent, NotificationClick, Notify, RuntimeKey, TopicMessage, WithdrawNotification};
use gpui::{App, Task};
use smelt_plugin_api as api;

use attention::{Applied, Mirror};
use store::{Delivered, NotifyStore, Prefs};

/// 本插件的来源 id：库文件名、设置页、通知 id 都用它。
pub(crate) const OWNER: &str = "smelt.notifications";

pub(crate) struct Plugin {
    mirror: Mirror,
    store: NotifyStore,
    prefs: Prefs,
    /// 发出去、还挂在通知中心里的，和库里那份一致。
    delivered: HashSet<Delivered>,
    /// 已经告诉运行时看过、还没回话的：同一段不重复发。
    marking: HashSet<Delivered>,
    menubar: Rc<RefCell<menubar::Sent>>,
}

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    let store = NotifyStore::open();
    let plugin = Rc::new(RefCell::new(Plugin {
        mirror: Mirror::default(),
        prefs: store.prefs(),
        delivered: store.delivered(),
        store,
        marking: HashSet::new(),
        menubar: Rc::default(),
    }));
    settings_page::register(plugin.clone(), cx);
    let on_event = plugin.clone();
    let events = api::events(cx);
    cx.subscribe(&events, move |_, event: &HostEvent, cx| match event {
        HostEvent::RuntimeTopic {
            machine,
            topic,
            message,
        } if topic == CORE_TOPIC_ATTENTION_CHANGED => {
            on_attention(&on_event, machine, message, cx);
        }
        HostEvent::MachinesChanged => {
            let machines = api::host(cx).machines(cx);
            let known = |machine: &RuntimeKey| machines.iter().any(|known| known.key == *machine);
            on_event.borrow_mut().mirror.retain_machines(known);
            let gone: Vec<Delivered> = on_event
                .borrow()
                .delivered
                .iter()
                .filter(|(machine, _)| !known(machine))
                .cloned()
                .collect();
            for delivered in gone {
                withdraw(&on_event, delivered, cx);
            }
            refresh_menubar(&on_event, cx);
        }
        HostEvent::SessionsChanged | HostEvent::CommandsChanged => refresh_menubar(&on_event, cx),
        HostEvent::ActiveContextChanged | HostEvent::WindowActivated => {
            mark_viewed_read(&on_event, cx);
        }
        _ => {}
    })
    .detach();
    refresh_menubar(&plugin, cx);
}

fn on_attention(
    plugin: &Rc<RefCell<Plugin>>,
    machine: &RuntimeKey,
    message: &TopicMessage,
    cx: &mut App,
) {
    let applied = plugin.borrow_mut().mirror.apply(machine, message);
    match applied {
        Applied::Nothing => return,
        Applied::Replaced => {}
        Applied::Changed { session, alert } => {
            if let Some(alert) = alert {
                deliver(plugin, machine, &session, alert, cx);
            }
        }
    }
    withdraw_read(plugin, cx);
    mark_viewed_read(plugin, cx);
    refresh_menubar(plugin, cx);
}

/// 刚来的一条提醒：发系统通知，或者不发。
fn deliver(
    plugin: &Rc<RefCell<Plugin>>,
    machine: &RuntimeKey,
    session: &SessionAttention,
    alert: AttentionItem,
    cx: &mut App,
) {
    let key: Delivered = (machine.clone(), session.session_id.clone());
    let enabled = plugin.borrow().prefs.allows(alert.kind);
    if !enabled || viewed_session(cx).as_ref() == Some(&key) {
        // 这段会话又有了新情况，之前那条已经过时，一起撤掉。
        withdraw(plugin, key, cx);
        return;
    }
    let subtitle = match menubar::session_title(machine, &session.session_id, cx) {
        Some(title) => format!("{} · {title}", alert.title),
        None => alert.title,
    };
    if session.unread.is_none() {
        // 运行时没记未读：没处标已读（自动化运行、命令行发的），发一次性的。
        run_logged(
            "发通知",
            api::NOTIFY.run(
                &Notify {
                    id: None,
                    subtitle,
                    body: alert.message,
                    on_click: None,
                },
                cx,
            ),
            cx,
        );
        return;
    }
    let notify = Notify {
        id: Some(notification_id(&key)),
        subtitle,
        body: alert.message,
        on_click: Some(NotificationClick {
            command: sessions::ACTIVATE.id().to_string(),
            args: api::serde_json::to_value(Activate {
                session_id: session.session_id.clone(),
            })
            .unwrap_or_default(),
        }),
    };
    run_logged("发通知", api::NOTIFY.run(&notify, cx), cx);
    let mut plugin = plugin.borrow_mut();
    if plugin.delivered.insert(key.clone()) {
        plugin.store.add_delivered(&key);
    }
}

/// 撤掉运行时说已经没有未读的那些：别的设备上看过、在终端里打过字、回过。还没收到快照的
/// 机器不算，说不准。
fn withdraw_read(plugin: &Rc<RefCell<Plugin>>, cx: &mut App) {
    let read: Vec<Delivered> = {
        let plugin = plugin.borrow();
        plugin
            .delivered
            .iter()
            .filter(|(machine, session_id)| {
                plugin.mirror.primed(machine) && !plugin.mirror.has_unread(machine, session_id)
            })
            .cloned()
            .collect()
    };
    for delivered in read {
        withdraw(plugin, delivered, cx);
    }
}

fn withdraw(plugin: &Rc<RefCell<Plugin>>, delivered: Delivered, cx: &mut App) {
    {
        let mut plugin = plugin.borrow_mut();
        if !plugin.delivered.remove(&delivered) {
            return;
        }
        plugin.store.remove_delivered(&delivered);
    }
    let withdraw = WithdrawNotification {
        id: notification_id(&delivered),
    };
    run_logged("撤通知", api::WITHDRAW_NOTIFICATION.run(&withdraw, cx), cx);
}

/// 人正看着的那段会话有未读：告诉它所在的运行时看过了。运行时清掉后推来变化，通知随之撤掉、
/// 角标跟着少。
fn mark_viewed_read(plugin: &Rc<RefCell<Plugin>>, cx: &mut App) {
    let Some(key) = viewed_session(cx) else {
        return;
    };
    {
        let mut plugin = plugin.borrow_mut();
        if !plugin.mirror.has_unread(&key.0, &key.1) || !plugin.marking.insert(key.clone()) {
            return;
        }
    }
    let params = AttentionMarkReadParams {
        session_id: key.1.clone(),
    };
    let task = api::call::<AttentionMarkRead>(&key.0, params, api::CALL_TIMEOUT, cx);
    let plugin = plugin.clone();
    cx.spawn(async move |cx| {
        if let Err(error) = task.await {
            eprintln!("[notifications] 标已读失败：{error}");
        }
        cx.update(|_| plugin.borrow_mut().marking.remove(&key));
    })
    .detach();
}

/// 人正看着的会话：主窗口在前台、舞台上露着、翻在最新处。
fn viewed_session(cx: &App) -> Option<Delivered> {
    if !api::host(cx).window_active(cx) {
        return None;
    }
    let session = api::active_context(cx).session?;
    session.at_latest.then_some((session.machine, session.id))
}

fn refresh_menubar(plugin: &Rc<RefCell<Plugin>>, cx: &mut App) {
    let (sent, attention) = {
        let plugin = plugin.borrow();
        (plugin.menubar.clone(), plugin.mirror.by_session())
    };
    menubar::refresh(&sent, &attention, cx);
}

/// 通知 id：`smelt.notifications/<机器>/<会话>`，同一段会话再来就换掉上一条。
fn notification_id((machine, session_id): &Delivered) -> String {
    let machine = match machine {
        RuntimeKey::Local => "local".to_string(),
        RuntimeKey::Remote(id) => format!("remote:{id}"),
    };
    format!("{OWNER}/{machine}/{session_id}")
}

fn run_logged(what: &'static str, task: Task<Result<(), String>>, cx: &mut App) {
    cx.spawn(async move |_| {
        if let Err(error) = task.await {
            eprintln!("[notifications] {what}失败：{error}");
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_ids_keep_machines_apart() {
        assert_eq!(
            notification_id(&(RuntimeKey::Local, "s1".to_string())),
            "smelt.notifications/local/s1"
        );
        assert_eq!(
            notification_id(&(RuntimeKey::Remote("rt".to_string()), "s1".to_string())),
            "smelt.notifications/remote:rt/s1"
        );
    }
}
