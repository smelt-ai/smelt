//! 各台机器上的智能体配置，权威在智能体插件（来源 `smelt.agents`，约定见 [`peer`]）。插件
//! 页签上「谁在用」从这里数：只读缓存，智能体插件登记上就问一次整份，之后跟着它发的事件
//! 整份换，不自己读运行时。智能体插件没装、还没问到时是空的，「谁在用」就不写。
//!
//! 全靠事件：插件启动时智能体插件可能还没登记，等 `HostEvent::CommandsChanged` 看到它的命令
//! 登记上了再问；它撤下了就清空。

use std::collections::{HashMap, HashSet};

use api::model::agent_host::AgentHostSettings;
use api::{HostEvent, RuntimeKey};
use gpui::{App, BorrowAppContext as _, Global};
use smelt_plugin_api as api;

/// 智能体（来源 `smelt.agents`）的约定，权威定义在 `crates/plugins/agents/src/contract.rs`。
pub(crate) mod peer {
    use serde::Deserialize;
    use smelt_plugin_api::model::agent_host::AgentHostSettings;
    use smelt_plugin_api::{Command, Event, RuntimeKey, machine_json};

    // 约定：smelt.agents 的 AGENT_SETTINGS。
    /// 各台机器眼下的智能体配置，只含收到过推送的机器。
    pub(crate) const AGENT_SETTINGS: Command<(), Vec<MachineAgentSettings>> =
        Command::new("smelt.agents/agent-settings");

    // 约定：smelt.agents 的 AGENT_SETTINGS_CHANGED。
    /// 一台机器的智能体配置换了（整份），或那台机器给不了、移除了。
    pub(crate) const AGENT_SETTINGS_CHANGED: Event<MachineAgentSettings> =
        Event::new("smelt.agents/agent-settings-changed");

    /// 一台机器的智能体配置。
    #[derive(Clone, Debug, Deserialize)]
    pub(crate) struct MachineAgentSettings {
        #[serde(with = "machine_json")]
        pub machine: RuntimeKey,
        #[serde(flatten)]
        pub state: AgentSettingsState,
    }

    #[derive(Clone, Debug, Deserialize)]
    #[serde(tag = "state", rename_all = "snake_case")]
    pub(crate) enum AgentSettingsState {
        Ready {
            settings: AgentHostSettings,
        },
        /// 那台机器给不了。
        Unavailable {},
        /// 机器移除了。
        Absent,
    }
}

use peer::{AGENT_SETTINGS, AGENT_SETTINGS_CHANGED, AgentSettingsState, MachineAgentSettings};

/// 各台机器上的智能体配置。页面 `observe_global` 它，变了就重画。
#[derive(Default)]
pub(crate) struct AgentSettingsCache {
    /// 给不了、移除了、还没收到的机器不在里面。
    machines: HashMap<RuntimeKey, AgentHostSettings>,
    fetch: Fetch,
    /// 问整份期间收到过事件的机器：事件更新，问来的那份对它们作废。
    touched: HashSet<RuntimeKey>,
}

impl Global for AgentSettingsCache {}

#[derive(Clone, Copy, Default, PartialEq)]
enum Fetch {
    /// 还没问过、智能体插件没登记，或上次没问到：等命令登记再问。
    #[default]
    Idle,
    Loading,
    Done,
}

/// 插件启动时调一次：订上智能体插件的事件，它已经登记了就当场问一份。
pub(crate) fn install(cx: &mut App) {
    cx.set_global(AgentSettingsCache::default());
    cx.subscribe(&api::events(cx), |_, event: &HostEvent, cx| match event {
        HostEvent::Plugin { .. } => {
            if let Some(changed) = AGENT_SETTINGS_CHANGED.parse(event) {
                apply(changed, cx);
            }
        }
        HostEvent::CommandsChanged => on_commands_changed(cx),
        _ => {}
    })
    .detach();
    on_commands_changed(cx);
}

fn on_commands_changed(cx: &mut App) {
    let registered = api::command(AGENT_SETTINGS.id(), cx).is_some();
    let fetch = cx.global::<AgentSettingsCache>().fetch;
    match (registered, fetch) {
        (true, Fetch::Idle) => load(cx),
        // 智能体插件撤下了：没有权威了，手里那份作废。
        (false, Fetch::Done) => cx.update_global::<AgentSettingsCache, _>(|cache, _| {
            *cache = AgentSettingsCache::default();
        }),
        _ => {}
    }
}

/// 问智能体插件要一份整的。
fn load(cx: &mut App) {
    cx.update_global::<AgentSettingsCache, _>(|cache, _| {
        cache.fetch = Fetch::Loading;
        cache.touched.clear();
    });
    let settings = AGENT_SETTINGS.run(&(), cx);
    cx.spawn(async move |cx| {
        let settings = settings.await;
        cx.update(|cx| {
            cx.update_global::<AgentSettingsCache, _>(|cache, _| match settings {
                Ok(machines) => {
                    for machine in machines {
                        if cache.touched.contains(&machine.machine) {
                            continue;
                        }
                        if let AgentSettingsState::Ready { settings } = machine.state {
                            cache.machines.insert(machine.machine, settings);
                        }
                    }
                    cache.touched.clear();
                    cache.fetch = Fetch::Done;
                }
                Err(error) => {
                    eprintln!("[capabilities] 取智能体配置失败：{error}");
                    cache.fetch = Fetch::Idle;
                }
            });
        });
    })
    .detach();
}

fn apply(changed: MachineAgentSettings, cx: &mut App) {
    cx.update_global::<AgentSettingsCache, _>(|cache, _| {
        if cache.fetch == Fetch::Loading {
            cache.touched.insert(changed.machine.clone());
        }
        match changed.state {
            AgentSettingsState::Ready { settings } => {
                cache.machines.insert(changed.machine, settings);
            }
            AgentSettingsState::Unavailable {} | AgentSettingsState::Absent => {
                cache.machines.remove(&changed.machine);
            }
        }
    });
}

/// 那台机器的智能体配置；还没收到、给不了是 `None`。只读内存，render 里可以调。
pub(crate) fn settings_on<'a>(machine: &RuntimeKey, cx: &'a App) -> Option<&'a AgentHostSettings> {
    cx.try_global::<AgentSettingsCache>()?.machines.get(machine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_parse_from_the_providers_shape() {
        let ready: MachineAgentSettings = api::serde_json::from_value(api::serde_json::json!({
            "machine": "box",
            "state": "ready",
            "settings": AgentHostSettings::fresh(),
        }))
        .unwrap();
        assert_eq!(ready.machine, RuntimeKey::Remote("box".into()));
        assert!(matches!(ready.state, AgentSettingsState::Ready { .. }));

        let unavailable: MachineAgentSettings =
            api::serde_json::from_value(api::serde_json::json!({
                "machine": null, "state": "unavailable", "reason": "运行时太旧",
            }))
            .unwrap();
        assert!(matches!(
            unavailable.state,
            AgentSettingsState::Unavailable {}
        ));
    }
}
