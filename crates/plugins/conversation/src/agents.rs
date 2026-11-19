//! 对话要读的机器一级的状态，权威都在智能体插件（来源 `smelt.agents`）：各台机器的智能体配置
//! （自定义 Agent 的名字、记住的会话选项）。这里是只读缓存：第一段对话打开时问一次整份，之后
//! 跟着它发的事件整份换，不自己读运行时、不先改本地。智能体插件没装、还没问到时是空的：不显示
//! 自定义 Agent 的名字。
//!
//! 还有 Pi 配置变了的信号（[`PiCatalog`]）：模型目录只能问 Pi 进程自己，会话启动时拉一次；
//! 本机运行时推来 Pi 配置变了（登录、登出、改 provider），活着的对话据此重拉。
//!
//! 下面的 id 和字段照抄自 `crates/plugins/agents/src/contract.rs`，必须和那边一致。

use rust_i18n::t;
use std::collections::HashMap;

use gpui::{App, Global, Task};
use serde::{Deserialize, Serialize};
use smelt_plugin_api::model::agent_host::AgentHostSettings;
use smelt_plugin_api::model::daemon_state::CORE_TOPIC_PI_CONFIG_CHANGED;
use smelt_plugin_api::{Command, Event, HostEvent, RuntimeKey, TopicMessage, machine_json};

// ---- 约定：照抄自 crates/plugins/agents/src/contract.rs ----

/// 智能体页。参数 `{"machine": …, "agent": "<id>", "tab": "config" | "memory"}`。
pub(crate) const VIEW: &str = "smelt.agents/main";

/// 各台机器眼下的智能体配置，只含收到过推送的机器。
const AGENT_SETTINGS: Command<(), Vec<MachineAgentSettings>> =
    Command::new("smelt.agents/agent-settings");

/// 一台机器的智能体配置换了（整份），或那台机器给不了、移除了。
const AGENT_SETTINGS_CHANGED: Event<MachineAgentSettings> =
    Event::new("smelt.agents/agent-settings-changed");

/// 改一台机器的智能体配置：交 `before` 和 `after`，运行时合并到最新的那份上，交回写好的整份。
const EDIT_AGENT_SETTINGS: Command<AgentSettingsEdit, AgentHostSettings> =
    Command::new("smelt.agents/edit-agent-settings");

/// 一台机器的智能体配置。
#[derive(Clone, Debug, Deserialize)]
struct MachineAgentSettings {
    #[serde(with = "machine_json")]
    machine: RuntimeKey,
    #[serde(flatten)]
    state: AgentSettingsState,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum AgentSettingsState {
    Ready {
        settings: AgentHostSettings,
    },
    /// 那台机器给不了，原因这里用不上。
    Unavailable {},
    Absent,
}

/// [`EDIT_AGENT_SETTINGS`] 的参数。
#[derive(Serialize)]
struct AgentSettingsEdit {
    #[serde(with = "machine_json")]
    machine: RuntimeKey,
    before: AgentHostSettings,
    after: AgentHostSettings,
}

// ---- 缓存 ----

#[derive(Default)]
struct Cache {
    /// 每台机器的配置。给不了、移除了的机器不在里面。
    settings: HashMap<RuntimeKey, AgentHostSettings>,
    fetch: Fetch,
}

#[derive(Default, PartialEq)]
enum Fetch {
    /// 还没问过，或上次没问到（智能体插件那时还没登记）：下一段对话打开时再问。
    #[default]
    Idle,
    Loading,
    Done,
}

impl Global for Cache {}

/// Pi 配置变了的信号，不带数据：每变一次重新放一份，观察它的对话视图就醒来。
pub(crate) struct PiCatalog;

impl Global for PiCatalog {}

/// 订智能体插件的事件和本机的 Pi 配置推送。插件登记时调一次。
pub(crate) fn install(cx: &mut App) {
    cx.set_global(Cache::default());
    cx.subscribe(
        &smelt_plugin_api::events(cx),
        |_, event: &HostEvent, cx| match event {
            HostEvent::RuntimeTopic {
                machine: RuntimeKey::Local,
                topic,
                message,
            } if topic == CORE_TOPIC_PI_CONFIG_CHANGED
                && !matches!(message, TopicMessage::Unavailable(_)) =>
            {
                // 积压丢了事件也当作变过：凭据或模型可能变了。会话眼下只跟本机的 Pi 目录走。
                cx.set_global(PiCatalog);
            }
            HostEvent::Plugin { .. } => {
                if let Some(changed) = AGENT_SETTINGS_CHANGED.parse(event) {
                    set_settings(changed, cx);
                }
            }
            _ => {}
        },
    )
    .detach();
}

/// 有对话要看了：还没问过就问智能体插件要一份整的。之后靠事件，不再问。
pub(crate) fn ensure_loaded(cx: &mut App) {
    if !cx.has_global::<Cache>() {
        return;
    }
    let cache = cx.global_mut::<Cache>();
    if cache.fetch != Fetch::Idle {
        return;
    }
    cache.fetch = Fetch::Loading;
    let settings = AGENT_SETTINGS.run(&(), cx);
    cx.spawn(async move |cx| {
        let settings = settings.await;
        cx.update(|cx| {
            let cache = cx.global_mut::<Cache>();
            cache.fetch = match settings {
                // 问的这会儿到了事件的机器以事件为准。
                Ok(machines) => {
                    for machine in machines {
                        if let AgentSettingsState::Ready { settings } = machine.state {
                            cache.settings.entry(machine.machine).or_insert(settings);
                        }
                    }
                    Fetch::Done
                }
                Err(error) => {
                    eprintln!("[conversation] 取智能体配置失败：{error}");
                    Fetch::Idle
                }
            };
            cx.refresh_windows();
        });
    })
    .detach();
}

fn set_settings(changed: MachineAgentSettings, cx: &mut App) {
    let cache = cx.global_mut::<Cache>();
    match changed.state {
        AgentSettingsState::Ready { settings } => {
            cache.settings.insert(changed.machine, settings);
        }
        AgentSettingsState::Unavailable {} | AgentSettingsState::Absent => {
            cache.settings.remove(&changed.machine);
        }
    }
    cx.refresh_windows();
}

/// 那台机器的智能体配置；还没收到、给不了是 `None`。只读内存，render 里可以调。
pub(crate) fn settings<'a>(machine: &RuntimeKey, cx: &'a App) -> Option<&'a AgentHostSettings> {
    cx.try_global::<Cache>()?.settings.get(machine)
}

/// 改那台机器的智能体配置：在手里那份上调 `f`，把改动交给智能体插件。配置还没收到时报错，
/// `f` 什么都没改时不发。交回的那份先换进缓存，接着改的以它为底；之后的推送照常整份换。
pub(crate) fn edit_settings(
    machine: &RuntimeKey,
    f: impl FnOnce(&mut AgentHostSettings),
    cx: &mut App,
) -> Task<Result<(), String>> {
    let Some(before) = settings(machine, cx).cloned() else {
        return Task::ready(Err(t!("error.settings_not_received").into_owned()));
    };
    let mut after = before.clone();
    f(&mut after);
    if after == before {
        return Task::ready(Ok(()));
    }
    let machine = machine.clone();
    let edit = EDIT_AGENT_SETTINGS.run(
        &AgentSettingsEdit {
            machine: machine.clone(),
            before,
            after,
        },
        cx,
    );
    cx.spawn(async move |cx| {
        let settings = edit.await?;
        cx.update(|cx| {
            cx.global_mut::<Cache>().settings.insert(machine, settings);
        });
        Ok(())
    })
}
