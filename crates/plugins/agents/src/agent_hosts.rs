//! 每台机器的智能体配置（启动命令、env、自定义 Agent、智能体定义、hooks 开关）在桌面上的
//! 只读缓存，和改配置的队列。
//!
//! 配置只有运行时写，改完整份推过来（话题 `agent_host.changed`，订上时先给一份快照）；这里
//! 只收推送，不自己读、不先改本地。要改就把修改发给那台机器的运行时（[`edit`]），等它推回来。
//! 别的插件经 [`contract::AGENT_SETTINGS`] 取现状、订 [`contract::AGENT_SETTINGS_CHANGED`]
//! 跟着换，改用 [`contract::EDIT_AGENT_SETTINGS`]。

use std::collections::{BTreeMap, HashSet, VecDeque};

use api::model::agent_host::{AgentHostEdit, AgentHostSettings};
use api::model::control_api::{AgentHooksInstall, AgentHostApplyEdit};
use api::model::daemon_state::CORE_TOPIC_AGENT_HOST_CHANGED;
use api::{CommandSpec, HostEvent, RuntimeKey, TopicMessage, serde_json};
use gpui::{App, AsyncApp, Global, Task};
use rust_i18n::t;
use smelt_plugin_api as api;

use crate::contract::{self, AgentSettingsEdit, AgentSettingsState, MachineAgentSettings};

/// 发一次配置修改的上限。运行时那边一次合并写库，正常是毫秒级。
const EDIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 一台机器的配置收到了什么。还没收到推送的机器不在表里。
#[derive(Clone, Debug, PartialEq)]
enum MachineAgentHost {
    /// 运行时推来的整份配置。
    Ready(AgentHostSettings),
    /// 那台机器给不了：运行时太旧、不认识这个话题、推来的解不开。原因给人看。
    Unavailable(String),
}

/// 每台机器各一份智能体配置。变了就整份换，观察这个全局的视图跟着重画。
#[derive(Default)]
pub(crate) struct AgentHosts {
    machines: BTreeMap<RuntimeKey, MachineAgentHost>,
}

impl Global for AgentHosts {}

/// 那台机器的配置；还没收到、给不了都是 `None`。
pub(crate) fn settings<'a>(machine: &RuntimeKey, cx: &'a App) -> Option<&'a AgentHostSettings> {
    match cx.try_global::<AgentHosts>()?.machines.get(machine)? {
        MachineAgentHost::Ready(settings) => Some(settings),
        MachineAgentHost::Unavailable(_) => None,
    }
}

/// 那台机器给不了配置的原因；收到了或还没收到是 `None`。
pub(crate) fn unavailable<'a>(machine: &RuntimeKey, cx: &'a App) -> Option<&'a str> {
    match cx.try_global::<AgentHosts>()?.machines.get(machine)? {
        MachineAgentHost::Ready(_) => None,
        MachineAgentHost::Unavailable(reason) => Some(reason),
    }
}

/// 收推送。插件登记时调一次，在宿主开始转发运行时话题之前。
pub(crate) fn install(cx: &mut App) {
    cx.set_global(AgentHosts::default());
    cx.subscribe(&api::events(cx), |_, event: &HostEvent, cx| match event {
        HostEvent::RuntimeTopic {
            machine,
            topic,
            message,
        } if topic == CORE_TOPIC_AGENT_HOST_CHANGED => receive(machine, message, cx),
        HostEvent::MachinesChanged => forget_removed(cx),
        _ => {}
    })
    .detach();
}

/// 对外的命令，和插件别的命令一起登记（同一来源的命令一次整组登记）。
pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        contract::AGENT_SETTINGS.spec(api::text!("command.agent_settings"), |(), _, cx| {
            Ok(snapshot(cx))
        }),
        contract::EDIT_AGENT_SETTINGS
            .spec_async(api::text!("command.edit_agent_settings"), |edit, _, cx| {
                send_edit(edit, cx)
            }),
    ]
}

/// 收到过推送的每台机器眼下的配置。
fn snapshot(cx: &App) -> Vec<MachineAgentSettings> {
    cx.global::<AgentHosts>()
        .machines
        .iter()
        .map(|(machine, host)| MachineAgentSettings {
            machine: machine.clone(),
            state: state_of(Some(host)),
        })
        .collect()
}

fn state_of(host: Option<&MachineAgentHost>) -> AgentSettingsState {
    match host {
        Some(MachineAgentHost::Ready(settings)) => AgentSettingsState::Ready {
            settings: settings.clone(),
        },
        Some(MachineAgentHost::Unavailable(reason)) => AgentSettingsState::Unavailable {
            reason: reason.clone(),
        },
        None => AgentSettingsState::Absent,
    }
}

/// 那台机器推来的配置：整份换上。积压丢了事件不用管，后面跟着一份快照；订阅被拒（运行时
/// 太旧）就记成给不了。
fn receive(machine: &RuntimeKey, message: &TopicMessage, cx: &mut App) {
    let next = match message {
        TopicMessage::Event(payload) | TopicMessage::Snapshot(payload) => {
            match serde_json::from_value::<AgentHostSettings>(payload.clone()) {
                Ok(settings) => MachineAgentHost::Ready(settings),
                Err(error) => {
                    eprintln!("[agents] {machine:?} 推来的智能体配置解不开：{error}");
                    MachineAgentHost::Unavailable(
                        t!("error.settings_unreadable", error = error).into_owned(),
                    )
                }
            }
        }
        TopicMessage::Lagged => return,
        TopicMessage::Unavailable(reason) => MachineAgentHost::Unavailable(reason.clone()),
    };
    set(machine, Some(next), cx);
}

/// 移除了的机器丢掉它的配置，下次挂上时由快照重新给。
fn forget_removed(cx: &mut App) {
    let attached: HashSet<RuntimeKey> = api::host(cx)
        .machines(cx)
        .into_iter()
        .map(|machine| machine.key)
        .collect();
    let removed: Vec<RuntimeKey> = cx
        .global::<AgentHosts>()
        .machines
        .keys()
        .filter(|machine| !attached.contains(machine))
        .cloned()
        .collect();
    for machine in removed {
        set(&machine, None, cx);
    }
}

/// 换上那台机器的状态；同样的一份再来不算变化，不惊动观察者和别的插件。
fn set(machine: &RuntimeKey, next: Option<MachineAgentHost>, cx: &mut App) {
    let current = cx.global::<AgentHosts>().machines.get(machine);
    if current == next.as_ref() {
        return;
    }
    let first_local_snapshot =
        *machine == RuntimeKey::Local && !matches!(current, Some(MachineAgentHost::Ready(_)));
    let hosts = cx.global_mut::<AgentHosts>();
    match next.clone() {
        Some(host) => hosts.machines.insert(machine.clone(), host),
        None => hosts.machines.remove(machine),
    };
    contract::AGENT_SETTINGS_CHANGED.emit(
        &MachineAgentSettings {
            machine: machine.clone(),
            state: state_of(next.as_ref()),
        },
        cx,
    );
    if first_local_snapshot
        && let Some(MachineAgentHost::Ready(settings)) = &next
        && settings.hooks_enabled
    {
        reconcile_hooks(cx);
    }
}

/// 本机配置头一回推来（应用启动、运行时重启）时按用户的开关校正一次 hooks：新版本可能改了
/// hook 条目，配置也可能被别的工具改坏。安装由运行时做，不弹提示，失败只记日志。
fn reconcile_hooks(cx: &mut App) {
    let installed = api::call::<AgentHooksInstall>(
        &RuntimeKey::Local,
        Default::default(),
        api::CALL_TIMEOUT,
        cx,
    );
    cx.background_executor()
        .spawn(async move {
            if let Err(error) = installed.await {
                eprintln!("[agents] 自动安装 Agent hooks 失败：{error}");
            }
        })
        .detach();
}

/// 把修改发给那台机器的运行时。返回运行时合并后的那份；本地不先改，等它推回来。
fn send_edit(edit: AgentSettingsEdit, cx: &App) -> Task<Result<AgentHostSettings, String>> {
    api::call::<AgentHostApplyEdit>(
        &edit.machine,
        AgentHostEdit {
            before: edit.before,
            after: edit.after,
        },
        EDIT_TIMEOUT,
        cx,
    )
}

/// 改那台机器的智能体配置：在收到的那份上调 `f`，把改动发给它的运行时。改完由运行时推回来，
/// 这里不先改本地；配置还没收到、给不了时报错，`f` 什么都没改时不发。
///
/// 每台机器一条队列，一次只发一份，轮到时才调 `f`：连着调几次就按调用的顺序都改上，后一份
/// 看得到前一份的结果。前一份改完时推送可能还没到，所以接着改的以运行时回的那份为底。
pub(crate) fn edit(
    machine: &RuntimeKey,
    f: impl FnOnce(&mut AgentHostSettings) + 'static,
    cx: &mut App,
) -> Task<Result<(), String>> {
    let (done, result) = smol::channel::bounded(1);
    let edits = cx.default_global::<Edits>();
    let idle = !edits.0.contains_key(machine);
    edits
        .0
        .entry(machine.clone())
        .or_default()
        .push_back(EditJob {
            change: Box::new(f),
            done,
        });
    if idle {
        let machine = machine.clone();
        cx.spawn(async move |cx| {
            let mut last = None;
            while let Some((job, before)) = cx.update(|cx| next_edit(&machine, last.take(), cx)) {
                let applied = apply_edit(&machine, job.change, before, &mut last, cx).await;
                let _ = job.done.try_send(applied);
            }
        })
        .detach();
    }
    cx.spawn(async move |_| {
        result
            .recv()
            .await
            .unwrap_or_else(|_| Err(t!("error.edit_incomplete").into_owned()))
    })
}

/// 每台机器等着发的配置修改。有队列就是那台机器正在发，排空了就拿掉。和 [`AgentHosts`]
/// 分开放：排队不是配置变了，不该叫醒看配置的视图。
#[derive(Default)]
struct Edits(BTreeMap<RuntimeKey, VecDeque<EditJob>>);

impl Global for Edits {}

type Change = Box<dyn FnOnce(&mut AgentHostSettings)>;

struct EditJob {
    change: Change,
    done: smol::channel::Sender<Result<(), String>>,
}

/// 取那台机器的下一份修改和它的底稿：`last` 是上一份改完运行时回的，没有就用眼下收到的那份。
/// 队列空了就拿掉它，返回 `None`。
fn next_edit(
    machine: &RuntimeKey,
    last: Option<AgentHostSettings>,
    cx: &mut App,
) -> Option<(EditJob, Result<AgentHostSettings, String>)> {
    let edits = cx.global_mut::<Edits>();
    let Some(job) = edits.0.get_mut(machine).and_then(VecDeque::pop_front) else {
        edits.0.remove(machine);
        return None;
    };
    let before = match last {
        Some(last) => Ok(last),
        None => settings(machine, cx).cloned().ok_or_else(|| {
            unavailable(machine, cx)
                .map(str::to_string)
                .unwrap_or_else(|| t!("error.settings_not_received").into_owned())
        }),
    };
    Some((job, before))
}

/// 在 `before` 上改，发出去；改上了把运行时回的那份记进 `last`。
async fn apply_edit(
    machine: &RuntimeKey,
    change: Change,
    before: Result<AgentHostSettings, String>,
    last: &mut Option<AgentHostSettings>,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let before = before?;
    let mut after = before.clone();
    change(&mut after);
    if after == before {
        *last = Some(before);
        return Ok(());
    }
    let sent = cx.update(|cx| {
        send_edit(
            AgentSettingsEdit {
                machine: machine.clone(),
                before,
                after,
            },
            cx,
        )
    });
    *last = Some(sent.await?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// hooks 关着：本机头一份配置不去调运行时装 hooks（测试里没有宿主）。
    /// 用默认智能体的 id 当记号区分各份配置。
    fn named(name: &str) -> AgentHostSettings {
        let mut settings = AgentHostSettings::fresh();
        settings.hooks_enabled = false;
        settings.default_conversation_agent = Some(name.into());
        settings
    }

    fn name_of(settings: Option<&AgentHostSettings>) -> Option<&str> {
        settings?.default_conversation_agent.as_deref()
    }

    fn snapshot_message(settings: &AgentHostSettings) -> TopicMessage {
        TopicMessage::Snapshot(serde_json::to_value(settings).unwrap())
    }

    /// 各台机器各收各的：推给远程那台只换远程的，本机不受影响。
    #[gpui::test]
    fn each_machine_keeps_what_it_was_pushed(cx: &mut gpui::TestAppContext) {
        let far = RuntimeKey::Remote("far".into());
        cx.update(|cx| {
            cx.set_global(AgentHosts::default());
            receive(&RuntimeKey::Local, &snapshot_message(&named("本机的")), cx);
            receive(&far, &snapshot_message(&named("远程的")), cx);
            receive(&far, &snapshot_message(&named("改过的远程")), cx);
            assert_eq!(name_of(settings(&RuntimeKey::Local, cx)), Some("本机的"));
            assert_eq!(name_of(settings(&far, cx)), Some("改过的远程"));
        });
    }

    /// 给不了时丢掉原先那份，原因留着给人看；之后再推来就恢复。积压不算变化。
    #[gpui::test]
    fn unavailable_drops_the_old_copy_until_pushed_again(cx: &mut gpui::TestAppContext) {
        let far = RuntimeKey::Remote("far".into());
        cx.update(|cx| {
            cx.set_global(AgentHosts::default());
            receive(&far, &snapshot_message(&named("远程的")), cx);
            receive(
                &far,
                &TopicMessage::Unavailable("升级运行时后可用".into()),
                cx,
            );
            assert!(settings(&far, cx).is_none());
            assert_eq!(unavailable(&far, cx), Some("升级运行时后可用"));
            receive(&far, &TopicMessage::Lagged, cx);
            assert_eq!(unavailable(&far, cx), Some("升级运行时后可用"));
            receive(&far, &snapshot_message(&named("升级后的")), cx);
            assert_eq!(name_of(settings(&far, cx)), Some("升级后的"));
            assert!(unavailable(&far, cx).is_none());
        });
    }

    /// 推来的解不开时记成给不了，不留旧的那份冒充现状。
    #[gpui::test]
    fn an_unreadable_push_is_unavailable(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(AgentHosts::default());
            receive(
                &RuntimeKey::Local,
                &TopicMessage::Event(serde_json::json!({ "agents": 1 })),
                cx,
            );
            assert!(settings(&RuntimeKey::Local, cx).is_none());
            assert!(unavailable(&RuntimeKey::Local, cx).is_some());
        });
    }

    /// 同样的一份再推来不算变化：不惊动观察这个全局的界面。
    ///
    /// 每次推送各占一轮 `update`：GPUI 一轮里同一个全局的通知只留一份，挤在一轮里就算不去重
    /// 也只数到 1，测不出来。
    #[gpui::test]
    fn the_same_push_twice_is_not_a_change(cx: &mut gpui::TestAppContext) {
        let changes = std::rc::Rc::new(std::cell::Cell::new(0));
        let far = RuntimeKey::Remote("far".into());
        cx.update(|cx| {
            cx.set_global(AgentHosts::default());
            let counted = changes.clone();
            cx.observe_global::<AgentHosts>(move |_| counted.set(counted.get() + 1))
                .detach();
        });
        cx.update(|cx| receive(&far, &snapshot_message(&named("远程的")), cx));
        cx.update(|cx| receive(&far, &snapshot_message(&named("远程的")), cx));
        cx.update(|cx| set(&RuntimeKey::Remote("没有".into()), None, cx));
        cx.run_until_parked();
        assert_eq!(changes.get(), 1);
    }
}
