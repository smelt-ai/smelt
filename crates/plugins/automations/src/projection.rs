//! 每台机器的自动化投影，和运行完成时的系统通知。
//!
//! 运行时经单独订阅的 `automations.changed` 推整份库（订上和积压丢事件后各一份快照，之后
//! 每次变化一份），这里按机器各留一份，页面只读它。权威在运行时：改动一律发给运行时，等它
//! 推回来，不在这里改。
//!
//! 推来的新快照和上一份比，跑完的运行发系统通知（这条自动化关了「通知 App」就不发）。发出去
//! 的记在本插件的库里，运行记录被清掉或机器移除时撤掉。入口上的角标是启用着、最近一次运行
//! 失败的自动化条数。什么时候做全靠事件，没有定时器。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use api::model::automation::{AutomationFile, AutomationRunStatus};
use api::model::automation_projection::{MachineAutomations, decode_automations_payload};
use api::model::daemon_state::CORE_TOPIC_AUTOMATIONS_CHANGED;
use api::{HostEvent, NotificationClick, Notify, RuntimeKey, TopicMessage, WithdrawNotification};
use gpui::{App, BorrowAppContext as _, Global, Task};
use smelt_plugin_api as api;

use crate::notifications::{
    AutomationProjectionOrigin, AutomationRunNotification, automation_run_notifications,
    effective_projection_origin,
};
use crate::store::{Delivered, DeliveredStore};
use crate::{OWNER, VIEW_ID};

/// 各台机器推来的自动化。页面 `observe_global` 它，变了就重画、对账。
#[derive(Default)]
pub(crate) struct AutomationStore {
    pub(crate) machines: HashMap<RuntimeKey, MachineAutomations>,
}

impl Global for AutomationStore {}

impl AutomationStore {
    /// 那台机器推来的那份；还没收到是 `None`。
    pub(crate) fn on(&self, machine: &RuntimeKey) -> Option<&MachineAutomations> {
        self.machines.get(machine)
    }

    /// 入口上的角标：启用着、最近一次运行失败的自动化条数。
    fn failing(&self) -> usize {
        self.machines
            .values()
            .map(|machine| {
                machine
                    .automations
                    .iter()
                    .filter(|automation| {
                        automation.enabled
                            && machine
                                .latest_run(&automation.id)
                                .is_some_and(|run| run.status == AutomationRunStatus::Failed)
                    })
                    .count()
            })
            .sum()
    }
}

/// 各台机器的自动化。只读内存，render 里可以调。
pub(crate) fn automations(cx: &App) -> &AutomationStore {
    cx.global::<AutomationStore>()
}

/// 一份新快照换进去之后要做的通知。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Outcome {
    /// 要发的：标题已带上机器名。
    pub(crate) notify: Vec<AutomationRunNotification>,
    /// 跑完了、但这条自动化不通知 App 的运行。
    pub(crate) silenced: Vec<String>,
    /// 快照里还在的运行：不在的那些发过的通知该撤了。
    pub(crate) runs: HashSet<String>,
}

/// 把一份快照换进去。和手里那份一样（版本没变、是旧库的）是 `None`。`machine_name` 是远程
/// 机器的名字，通知标题前写上它；本机不写。
pub(crate) fn apply_snapshot(
    store: &mut AutomationStore,
    machine: &RuntimeKey,
    snapshot: AutomationFile,
    origin: AutomationProjectionOrigin,
    machine_name: Option<&str>,
) -> Option<Outcome> {
    let current = store.machines.entry(machine.clone()).or_default();
    let origin = effective_projection_origin(origin, &current.store_id, &snapshot.store_id);
    let notifications = automation_run_notifications(&current.runs, &snapshot.runs, origin);
    // 发不发只看这条自动化自己的「通知 App」：那是它的通知开关。
    let notifies_app = snapshot
        .automations
        .iter()
        .map(|automation| (automation.id.clone(), automation.notifies_app()))
        .collect::<HashMap<_, _>>();
    let runs = snapshot.runs.iter().map(|run| run.id.clone()).collect();
    if !current.replace_snapshot(snapshot) {
        return None;
    }
    let mut outcome = Outcome {
        runs,
        ..Outcome::default()
    };
    for mut notification in notifications {
        if notifies_app
            .get(&notification.automation_id)
            .copied()
            .unwrap_or(true)
        {
            if let Some(name) = machine_name {
                notification.subtitle = format!("{name} · {}", notification.subtitle);
            }
            outcome.notify.push(notification);
        } else {
            outcome.silenced.push(notification.run_id);
        }
    }
    Some(outcome)
}

struct Delivery {
    store: DeliveredStore,
    /// 发出去、还挂在通知中心里的，和库里那份一致。
    delivered: HashSet<Delivered>,
}

/// 插件启动时调一次：装好投影，订上宿主事件。
pub(crate) fn install(cx: &mut App) {
    cx.set_global(AutomationStore::default());
    let store = DeliveredStore::open();
    let delivery = Rc::new(RefCell::new(Delivery {
        delivered: store.delivered(),
        store,
    }));
    let events = api::events(cx);
    cx.subscribe(&events, move |_, event: &HostEvent, cx| match event {
        HostEvent::RuntimeTopic {
            machine,
            topic,
            message,
        } if topic == CORE_TOPIC_AUTOMATIONS_CHANGED => {
            on_topic(&delivery, machine, message, cx);
        }
        HostEvent::MachinesChanged => on_machines_changed(&delivery, cx),
        _ => {}
    })
    .detach();
}

fn on_topic(
    delivery: &Rc<RefCell<Delivery>>,
    machine: &RuntimeKey,
    message: &TopicMessage,
    cx: &mut App,
) {
    let (payload, origin) = match message {
        TopicMessage::Snapshot(payload) => (payload, AutomationProjectionOrigin::InitialSnapshot),
        TopicMessage::Event(payload) => (payload, AutomationProjectionOrigin::Incremental),
        // 带快照的话题：后面跟着一份快照，等它。
        TopicMessage::Lagged => return,
    };
    let snapshot = match decode_automations_payload(payload.clone()) {
        Ok(Some(snapshot)) => snapshot,
        // 运行时还没有自动化库。
        Ok(None) => return,
        Err(error) => {
            eprintln!("[automations] {machine:?} 推来的自动化认不出：{error}");
            return;
        }
    };
    let machine_name = match machine {
        RuntimeKey::Local => None,
        RuntimeKey::Remote(_) => Some(crate::page::machine_name(machine, cx)),
    };
    let Some(outcome) = cx.update_global::<AutomationStore, _>(|store, _| {
        apply_snapshot(store, machine, snapshot, origin, machine_name.as_deref())
    }) else {
        return;
    };
    let pruned: Vec<Delivered> = delivery
        .borrow()
        .delivered
        .iter()
        .filter(|(owner, run_id)| owner == machine && !outcome.runs.contains(run_id))
        .cloned()
        .collect();
    for delivered in pruned {
        withdraw(delivery, delivered, cx);
    }
    for run_id in outcome.silenced {
        withdraw(delivery, (machine.clone(), run_id), cx);
    }
    for notification in outcome.notify {
        deliver(delivery, machine, notification, cx);
    }
    refresh_badge(cx);
}

/// 移除了的机器：它的自动化不再列，发过的通知撤掉。
fn on_machines_changed(delivery: &Rc<RefCell<Delivery>>, cx: &mut App) {
    let machines = api::host(cx).machines(cx);
    let known = |machine: &RuntimeKey| machines.iter().any(|known| known.key == *machine);
    let removed = automations(cx)
        .machines
        .keys()
        .any(|machine| !known(machine));
    if removed {
        cx.update_global::<AutomationStore, _>(|store, _| {
            store.machines.retain(|machine, _| known(machine));
        });
    }
    let gone: Vec<Delivered> = delivery
        .borrow()
        .delivered
        .iter()
        .filter(|(machine, _)| !known(machine))
        .cloned()
        .collect();
    for delivered in gone {
        withdraw(delivery, delivered, cx);
    }
    refresh_badge(cx);
}

fn deliver(
    delivery: &Rc<RefCell<Delivery>>,
    machine: &RuntimeKey,
    notification: AutomationRunNotification,
    cx: &mut App,
) {
    let key: Delivered = (machine.clone(), notification.run_id.clone());
    let notify = Notify {
        id: Some(notification_id(&key)),
        subtitle: notification.subtitle,
        body: notification.body,
        on_click: Some(NotificationClick {
            command: api::OPEN_VIEW.id().to_string(),
            args: api::serde_json::to_value(api::OpenView {
                view: VIEW_ID.to_string(),
                args: crate::page::run_args(machine, &notification.automation_id, &key.1),
            })
            .unwrap_or_default(),
        }),
    };
    run_logged("发通知", api::NOTIFY.run(&notify, cx), cx);
    let mut delivery = delivery.borrow_mut();
    if delivery.delivered.insert(key.clone()) {
        delivery.store.add(&key);
    }
}

fn withdraw(delivery: &Rc<RefCell<Delivery>>, delivered: Delivered, cx: &mut App) {
    {
        let mut delivery = delivery.borrow_mut();
        if !delivery.delivered.remove(&delivered) {
            return;
        }
        delivery.store.remove(&delivered);
    }
    let withdraw = WithdrawNotification {
        id: notification_id(&delivered),
    };
    run_logged("撤通知", api::WITHDRAW_NOTIFICATION.run(&withdraw, cx), cx);
}

fn refresh_badge(cx: &mut App) {
    let failing = automations(cx).failing();
    let label = (failing > 0).then(|| failing.to_string().into());
    api::set_view_badge(VIEW_ID, label, cx);
}

/// 通知 id：`smelt.automations/<机器>/<运行>`。
fn notification_id((machine, run_id): &Delivered) -> String {
    let machine = match machine {
        RuntimeKey::Local => "local".to_string(),
        RuntimeKey::Remote(id) => format!("remote:{id}"),
    };
    format!("{OWNER}/{machine}/{run_id}")
}

fn run_logged(what: &'static str, task: Task<Result<(), String>>, cx: &mut App) {
    cx.spawn(async move |_| {
        if let Err(error) = task.await {
            eprintln!("[automations] {what}失败：{error}");
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use api::model::automation::{
        Automation, AutomationAction, AutomationRun, AutomationRunContext, AutomationRunSource,
        AutomationSink, AutomationTrigger,
    };

    use super::*;

    fn automation(id: &str, sinks: Vec<AutomationSink>) -> Automation {
        Automation {
            id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            workspace_dir: None,
            trigger: AutomationTrigger::default(),
            action: AutomationAction::shell("true".to_string(), Vec::new()),
            sinks,
        }
    }

    fn run(id: &str, automation_id: &str, status: AutomationRunStatus) -> AutomationRun {
        AutomationRun {
            id: id.to_string(),
            automation_id: automation_id.to_string(),
            context: AutomationRunContext {
                automation_name: automation_id.to_string(),
                action: AutomationAction::shell("true".to_string(), Vec::new()),
                cwd: "/tmp".to_string(),
                trigger_payload: None,
                prompt: None,
                agent_definition_name: None,
                engine_kind_id: None,
                agent_instructions: None,
            },
            source: AutomationRunSource::Manual,
            scheduled_for: None,
            status,
            created_at: 1,
            started_at: Some(1),
            delivery_attempt_at: None,
            delivery_attempts: 0,
            finished_at: None,
            session_id: None,
            provider_session_id: None,
            output: None,
            error: None,
            runtime_released_at: None,
        }
    }

    fn file(
        revision: u64,
        automations: Vec<Automation>,
        runs: Vec<AutomationRun>,
    ) -> AutomationFile {
        AutomationFile {
            store_id: "store".to_string(),
            revision,
            automations,
            runs,
            ..AutomationFile::default()
        }
    }

    #[test]
    fn finished_runs_notify_with_the_remote_machine_name() {
        let machine = RuntimeKey::Remote("rt".to_string());
        let mut store = AutomationStore::default();
        let auto = automation("a", vec![AutomationSink::Local]);
        let first = file(
            1,
            vec![auto.clone()],
            vec![run("r", "a", AutomationRunStatus::Running)],
        );
        let outcome = apply_snapshot(
            &mut store,
            &machine,
            first,
            AutomationProjectionOrigin::InitialSnapshot,
            Some("工作站"),
        )
        .unwrap();
        assert!(outcome.notify.is_empty());

        let done = file(
            2,
            vec![auto],
            vec![run("r", "a", AutomationRunStatus::Completed)],
        );
        let outcome = apply_snapshot(
            &mut store,
            &machine,
            done,
            AutomationProjectionOrigin::Incremental,
            Some("工作站"),
        )
        .unwrap();
        assert_eq!(outcome.notify.len(), 1);
        assert!(outcome.notify[0].subtitle.starts_with("工作站 · "));
        assert_eq!(outcome.runs, HashSet::from(["r".to_string()]));
    }

    #[test]
    fn automations_without_app_notification_stay_quiet() {
        let mut store = AutomationStore::default();
        let auto = automation("a", vec![AutomationSink::Off]);
        assert!(!auto.notifies_app());
        let running = file(
            1,
            vec![auto.clone()],
            vec![run("r", "a", AutomationRunStatus::Running)],
        );
        apply_snapshot(
            &mut store,
            &RuntimeKey::Local,
            running,
            AutomationProjectionOrigin::InitialSnapshot,
            None,
        );
        let failed = file(
            2,
            vec![auto],
            vec![run("r", "a", AutomationRunStatus::Failed)],
        );
        let outcome = apply_snapshot(
            &mut store,
            &RuntimeKey::Local,
            failed,
            AutomationProjectionOrigin::Incremental,
            None,
        )
        .unwrap();
        assert!(outcome.notify.is_empty());
        assert_eq!(outcome.silenced, vec!["r".to_string()]);
        // 失败的那条算进角标。
        assert_eq!(store.failing(), 1);
    }

    #[test]
    fn same_revision_changes_nothing() {
        let mut store = AutomationStore::default();
        let snapshot = file(1, Vec::new(), Vec::new());
        assert!(
            apply_snapshot(
                &mut store,
                &RuntimeKey::Local,
                snapshot.clone(),
                AutomationProjectionOrigin::InitialSnapshot,
                None,
            )
            .is_some()
        );
        assert!(
            apply_snapshot(
                &mut store,
                &RuntimeKey::Local,
                snapshot,
                AutomationProjectionOrigin::Incremental,
                None,
            )
            .is_none()
        );
    }

    #[test]
    fn notification_ids_keep_machines_apart() {
        assert_eq!(
            notification_id(&(RuntimeKey::Local, "r".to_string())),
            "smelt.automations/local/r"
        );
        assert_eq!(
            notification_id(&(RuntimeKey::Remote("rt".to_string()), "r".to_string())),
            "smelt.automations/remote:rt/r"
        );
    }
}
