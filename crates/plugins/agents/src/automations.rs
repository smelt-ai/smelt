//! 各台机器上挂着每个智能体的自动化有几条：卡片上显示，删除按钮据此置灰。
//!
//! 只拿来显示。挂着自动化不让删由运行时在删除时判定（`delete_agent_definition_on`），
//! 这里没读到、读晚了都不影响对错。
//!
//! 运行时只在订上 `automations.changed` 时推一份快照，之后库变了才推。智能体页是打开时
//! 才建的，等它建好那份快照早就过去了，所以投影在插件启动时装好，页面只读它。和自动化
//! 插件各留一份，插件之间不互相依赖。

use std::collections::{HashMap, HashSet};

use api::model::automation_projection::{MachineAutomations, decode_automations_payload};
use api::model::daemon_state::CORE_TOPIC_AUTOMATIONS_CHANGED;
use gpui::{App, BorrowAppContext as _, Global};
use smelt_plugin_api::{self as api, HostEvent, RuntimeKey, TopicMessage};

use crate::AgentRef;

/// 页面 `observe_global` 它，变了就重画。
#[derive(Default)]
pub(crate) struct AgentAutomations {
    machines: HashMap<RuntimeKey, MachineAutomations>,
}

impl Global for AgentAutomations {}

impl AgentAutomations {
    /// 挂着它的自动化有几条；那台机器没有自动化库、或还没推来时算 0。
    pub(crate) fn count(&self, agent: &AgentRef) -> usize {
        self.machines
            .get(&agent.runtime)
            .map_or(0, |machine| machine.automations_for(&agent.id).len())
    }

    /// 挂着它的自动化的名字，按库里的顺序；没起名的是 `None`。
    pub(crate) fn names(&self, agent: &AgentRef) -> Vec<Option<String>> {
        self.machines
            .get(&agent.runtime)
            .map_or_else(Vec::new, |machine| {
                machine
                    .automations_for(&agent.id)
                    .into_iter()
                    .map(|automation| {
                        Some(automation.name.trim())
                            .filter(|name| !name.is_empty())
                            .map(str::to_string)
                    })
                    .collect()
            })
    }

    /// 那台机器推来的一份库；运行时还没有库是 `None`。
    pub(crate) fn apply(&mut self, machine: &RuntimeKey, payload: &api::serde_json::Value) {
        match decode_automations_payload(payload.clone()) {
            Ok(Some(file)) => {
                self.machines
                    .entry(machine.clone())
                    .or_default()
                    .replace_snapshot(file);
            }
            Ok(None) => {
                self.machines.remove(machine);
            }
            Err(error) => eprintln!("[agents] 自动化的推送认不出：{error}"),
        }
    }

    /// 移除了的机器不再记它的自动化。
    pub(crate) fn retain_machines(&mut self, machines: &HashSet<RuntimeKey>) {
        self.machines
            .retain(|machine, _| machines.contains(machine));
    }
}

/// 插件启动时调一次：装好投影，订上宿主事件。
pub(crate) fn install(cx: &mut App) {
    cx.set_global(AgentAutomations::default());
    let events = api::events(cx);
    cx.subscribe(&events, |_, event: &HostEvent, cx| match event {
        HostEvent::RuntimeTopic {
            machine,
            topic,
            message: TopicMessage::Event(payload) | TopicMessage::Snapshot(payload),
        } if topic == CORE_TOPIC_AUTOMATIONS_CHANGED => {
            cx.update_global::<AgentAutomations, _>(|store, _| store.apply(machine, payload));
        }
        // `Lagged` 后面跟着一份快照。
        HostEvent::MachinesChanged => {
            let machines: HashSet<RuntimeKey> = api::host(cx)
                .machines(cx)
                .into_iter()
                .map(|machine| machine.key)
                .collect();
            cx.update_global::<AgentAutomations, _>(|store, _| store.retain_machines(&machines));
        }
        _ => {}
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use api::model::automation::{Automation, AutomationAction, AutomationFile, AutomationTrigger};
    use api::serde_json::{Value, json};

    use super::*;

    fn payload(store_id: &str, revision: u64, agents: &[&str]) -> Value {
        let file = AutomationFile {
            store_id: store_id.into(),
            revision,
            automations: agents
                .iter()
                .enumerate()
                .map(|(ix, agent)| Automation {
                    id: format!("automation-{ix}"),
                    name: format!("自动化 {ix}"),
                    enabled: true,
                    workspace_dir: None,
                    trigger: AutomationTrigger::webhook("hook", None),
                    action: AutomationAction::agent(*agent, None),
                    sinks: Vec::new(),
                })
                .collect(),
            ..Default::default()
        };
        json!({ "revision": revision, "data": file })
    }

    fn agent(runtime: RuntimeKey, id: &str) -> AgentRef {
        AgentRef::new(runtime, id.to_string())
    }

    /// 页面建好之前推来的快照也算数：投影不跟页面同生同死。
    #[test]
    fn counts_come_from_whatever_the_machine_last_pushed() {
        let mut store = AgentAutomations::default();
        let remote = RuntimeKey::Remote("box".into());
        store.apply(&RuntimeKey::Local, &payload("s", 1, &["a", "a", "b"]));
        store.apply(&remote, &payload("r", 1, &["a"]));
        assert_eq!(store.count(&agent(RuntimeKey::Local, "a")), 2);
        assert_eq!(store.count(&agent(RuntimeKey::Local, "b")), 1);
        assert_eq!(store.count(&agent(RuntimeKey::Local, "c")), 0);
        assert_eq!(store.count(&agent(remote.clone(), "a")), 1, "按机器分开数");

        store.apply(&RuntimeKey::Local, &payload("s", 2, &["b"]));
        assert_eq!(store.count(&agent(RuntimeKey::Local, "a")), 0);
        store.apply(&RuntimeKey::Local, &payload("s", 1, &["a"]));
        assert_eq!(
            store.count(&agent(RuntimeKey::Local, "a")),
            0,
            "旧版本迟到不收"
        );

        store.apply(&remote, &json!({ "revision": 0, "data": null }));
        assert_eq!(store.count(&agent(remote, "a")), 0, "运行时没有库就是 0 条");
    }

    #[test]
    fn removed_machines_are_forgotten() {
        let mut store = AgentAutomations::default();
        let remote = RuntimeKey::Remote("box".into());
        store.apply(&RuntimeKey::Local, &payload("s", 1, &["a"]));
        store.apply(&remote, &payload("r", 1, &["a"]));
        store.retain_machines(&HashSet::from([RuntimeKey::Local]));
        assert_eq!(store.count(&agent(RuntimeKey::Local, "a")), 1);
        assert_eq!(store.count(&agent(remote, "a")), 0);
    }

    #[test]
    fn names_follow_the_store_and_leave_blank_ones_unnamed() {
        let mut store = AgentAutomations::default();
        let mut file = payload("s", 1, &["a", "b", "a"]);
        file["data"]["automations"][2]["name"] = json!("  ");
        store.apply(&RuntimeKey::Local, &file);
        assert_eq!(
            store.names(&agent(RuntimeKey::Local, "a")),
            vec![Some("自动化 0".to_string()), None]
        );
        assert!(
            store
                .names(&agent(RuntimeKey::Remote("box".into()), "a"))
                .is_empty()
        );
    }
}
