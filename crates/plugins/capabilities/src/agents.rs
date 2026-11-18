//! 各台机器上的智能体配置，权威在智能体插件（来源 `smelt.agents`，约定见 [`peer`]）。技能、
//! 扩展、MCP 每行的「谁在用」从这里数：只读缓存，智能体插件登记上就问一次整份，之后跟着它
//! 发的事件整份换，不自己读运行时。智能体插件没装、还没问到时是空的，「谁在用」就不写。
//! 「给智能体用」勾选时经智能体插件的命令改（[`set_use`]），不自己写配置。
//!
//! 全靠事件：插件启动时智能体插件可能还没登记，等 `HostEvent::CommandsChanged` 看到它的命令
//! 登记上了再问；它撤下了就清空。

use std::collections::{HashMap, HashSet};

use api::model::agent_definition::AgentDefinition;
use api::model::agent_host::AgentHostSettings;
use api::{HostEvent, RuntimeKey};
use gpui::{App, BorrowAppContext as _, Global, Task};
use smelt_plugin_api as api;

/// 智能体（来源 `smelt.agents`）的约定，权威定义在 `crates/plugins/agents/src/contract.rs`。
pub(crate) mod peer {
    use serde::{Deserialize, Serialize};
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

    // 约定：smelt.agents 的 EDIT_AGENT_SETTINGS。
    /// 改一台机器的智能体配置：交 `before` 到 `after` 的改动，交回写好的整份。
    pub(crate) const EDIT_AGENT_SETTINGS: Command<AgentSettingsEdit, AgentHostSettings> =
        Command::new("smelt.agents/edit-agent-settings");

    /// [`EDIT_AGENT_SETTINGS`] 的参数。
    #[derive(Clone, Debug, Serialize)]
    pub(crate) struct AgentSettingsEdit {
        #[serde(with = "machine_json")]
        pub machine: RuntimeKey,
        pub before: AgentHostSettings,
        pub after: AgentHostSettings,
    }

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

use peer::{
    AGENT_SETTINGS, AGENT_SETTINGS_CHANGED, AgentSettingsEdit, AgentSettingsState,
    EDIT_AGENT_SETTINGS, MachineAgentSettings,
};

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

/// 能勾给智能体用的一项：技能或扩展（Pi 插件 id），或 MCP 服务器名。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Usable {
    Plugin(String),
    Mcp(String),
}

impl Usable {
    fn list<'a>(&self, agent: &'a AgentDefinition) -> &'a [String] {
        match self {
            Self::Plugin(_) => &agent.plugins,
            Self::Mcp(_) => &agent.mcp_servers,
        }
    }

    fn list_mut<'a>(&self, agent: &'a mut AgentDefinition) -> &'a mut Vec<String> {
        match self {
            Self::Plugin(_) => &mut agent.plugins,
            Self::Mcp(_) => &mut agent.mcp_servers,
        }
    }

    fn id(&self) -> &str {
        match self {
            Self::Plugin(id) | Self::Mcp(id) => id,
        }
    }

    /// 这个智能体勾了它没有。
    pub(crate) fn used_by(&self, agent: &AgentDefinition) -> bool {
        self.list(agent).iter().any(|id| id == self.id())
    }

    /// 那台机器上勾了它的智能体名，按智能体的顺序。
    pub(crate) fn user_names(&self, machine: &RuntimeKey, cx: &App) -> Vec<String> {
        settings_on(machine, cx)
            .map(|settings| {
                settings
                    .agents
                    .iter()
                    .filter(|agent| self.used_by(agent))
                    .map(|agent| agent.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// 把 `settings` 里那个智能体对 `usable` 的勾选改成 `on`；智能体不在、本来就是这样时 `None`。
fn with_use(
    settings: &AgentHostSettings,
    usable: &Usable,
    agent_id: &str,
    on: bool,
) -> Option<AgentHostSettings> {
    let mut after = settings.clone();
    let agent = after.agents.iter_mut().find(|agent| agent.id == agent_id)?;
    if usable.used_by(agent) == on {
        return None;
    }
    let list = usable.list_mut(agent);
    if on {
        list.push(usable.id().to_string());
    } else {
        list.retain(|id| id != usable.id());
    }
    Some(after)
}

/// 让那台机器上的一个智能体用 / 不用这一项。经智能体插件改，写好的整份当场换进缓存（随后
/// 的事件带的是同一份）。没有可改的（配置还没收到、智能体不在、已经是这样）时 `None`。
///
/// 改动按「改前、改后」交给运行时三方合并，同一张列表上的两次改动并发会互相盖掉，调用方
/// 要等上一次回来再发下一次。
pub(crate) fn set_use(
    machine: &RuntimeKey,
    usable: &Usable,
    agent_id: &str,
    on: bool,
    cx: &mut App,
) -> Option<Task<Result<(), String>>> {
    let before = settings_on(machine, cx)?.clone();
    let after = with_use(&before, usable, agent_id, on)?;
    let machine = machine.clone();
    let edit = EDIT_AGENT_SETTINGS.run(
        &AgentSettingsEdit {
            machine: machine.clone(),
            before,
            after,
        },
        cx,
    );
    Some(cx.spawn(async move |cx| {
        let settings = edit.await?;
        cx.update(|cx| {
            cx.update_global::<AgentSettingsCache, _>(|cache, _| {
                cache.machines.insert(machine, settings);
            });
        });
        Ok(())
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, plugins: &[&str], mcp: &[&str]) -> AgentDefinition {
        let mut agent = AgentDefinition {
            id: id.into(),
            name: id.into(),
            ..AgentDefinition::default()
        };
        agent.plugins = plugins.iter().map(|id| id.to_string()).collect();
        agent.mcp_servers = mcp.iter().map(|id| id.to_string()).collect();
        agent
    }

    #[test]
    fn with_use_toggles_only_that_agent_and_list() {
        let mut settings = AgentHostSettings::fresh();
        settings.agents = vec![
            agent("a", &["skill:x"], &[]),
            agent("b", &["skill:x"], &["github"]),
        ];
        let skill = Usable::Plugin("skill:y".into());

        let after = with_use(&settings, &skill, "a", true).unwrap();
        assert_eq!(after.agents[0].plugins, ["skill:x", "skill:y"]);
        assert_eq!(after.agents[1], settings.agents[1]);

        let mcp = Usable::Mcp("github".into());
        let after = with_use(&settings, &mcp, "b", false).unwrap();
        assert!(after.agents[1].mcp_servers.is_empty());
        assert_eq!(after.agents[1].plugins, ["skill:x"]);
    }

    #[test]
    fn with_use_has_nothing_to_do_when_already_so_or_agent_missing() {
        let mut settings = AgentHostSettings::fresh();
        settings.agents = vec![agent("a", &["skill:x"], &[])];
        let skill = Usable::Plugin("skill:x".into());
        assert!(with_use(&settings, &skill, "a", true).is_none());
        assert!(with_use(&settings, &skill, "gone", false).is_none());
    }

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
