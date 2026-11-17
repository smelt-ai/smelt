//! 对话要读的机器、账户一级的状态，权威都在智能体插件（来源 `smelt.agents`）：各台机器的
//! 智能体配置（自定义 Agent 的名字、记住的会话选项）、Pi 各家 provider 的额度（模型菜单里
//! 的括号）。这里是只读缓存：第一段对话打开时问一次整份，之后跟着它发的事件整份换，不自己
//! 读运行时、不先改本地。智能体插件没装、还没问到时是空的：不显示自定义 Agent 的名字和额度。
//!
//! 还有 Pi 配置变了的信号（[`PiCatalog`]）：模型目录只能问 Pi 进程自己，会话启动时拉一次；
//! 本机运行时推来 Pi 配置变了（登录、登出、改 provider），活着的对话据此重拉。
//!
//! 下面的 id 和字段照抄自 `crates/plugins/agents/src/contract.rs`，必须和那边一致。

use std::collections::HashMap;

use gpui::{App, Global, Task};
use serde::{Deserialize, Serialize};
use smelt_plugin_api::model::agent_host::AgentHostSettings;
use smelt_plugin_api::model::agent_kind::ConversationAgentKind;
use smelt_plugin_api::model::daemon_state::CORE_TOPIC_PI_CONFIG_CHANGED;
use smelt_plugin_api::model::provider_quota::ProviderQuotaData;
use smelt_plugin_api::model::quota_service::{QuotaStatus, QuotaTarget};
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

/// 本机运行时上各账户的额度。之后的变化订 [`QUOTA_CHANGED`]。
const QUOTA: Command<(), Quota> = Command::new("smelt.agents/quota");

/// 额度、查询范围或「查询中」变了，带整份。
const QUOTA_CHANGED: Event<Quota> = Event::new("smelt.agents/quota-changed");

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

/// 额度现状。这里只用查到的结果，别的字段忽略。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Quota {
    pub(crate) statuses: Vec<(QuotaTarget, QuotaStatus)>,
}

// ---- 缓存 ----

#[derive(Default)]
struct Cache {
    /// 每台机器的配置。给不了、移除了的机器不在里面。
    settings: HashMap<RuntimeKey, AgentHostSettings>,
    /// Pi 各家 provider 的额度短文本，按 provider id。
    pi_badges: HashMap<String, String>,
    fetch: Fetch,
    /// 收到过几次额度事件。问整份的回来时已经收到过事件，事件更新，丢掉问来的。
    quota_events: u64,
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
                } else if let Some(quota) = QUOTA_CHANGED.parse(event) {
                    let cache = cx.global_mut::<Cache>();
                    cache.quota_events += 1;
                    cache.pi_badges = pi_badges(&quota);
                    cx.refresh_windows();
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
    let quota_events = cache.quota_events;
    let settings = AGENT_SETTINGS.run(&(), cx);
    let quota = QUOTA.run(&(), cx);
    cx.spawn(async move |cx| {
        let settings = settings.await;
        let quota = quota.await;
        cx.update(|cx| {
            let cache = cx.global_mut::<Cache>();
            let mut loaded = true;
            match settings {
                // 问的这会儿到了事件的机器以事件为准。
                Ok(machines) => {
                    for machine in machines {
                        if let AgentSettingsState::Ready { settings } = machine.state {
                            cache.settings.entry(machine.machine).or_insert(settings);
                        }
                    }
                }
                Err(error) => {
                    eprintln!("[conversation] 取智能体配置失败：{error}");
                    loaded = false;
                }
            }
            match quota {
                Ok(quota) if cache.quota_events == quota_events => {
                    cache.pi_badges = pi_badges(&quota);
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!("[conversation] 取账户额度失败：{error}");
                    loaded = false;
                }
            }
            cache.fetch = if loaded { Fetch::Done } else { Fetch::Idle };
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

/// 额度里 Pi 各家 provider 的短文本。
fn pi_badges(quota: &Quota) -> HashMap<String, String> {
    quota
        .statuses
        .iter()
        .filter(|(target, _)| target.agent == ConversationAgentKind::Pi)
        .filter_map(|(target, status)| {
            Some((
                target.provider?.to_string(),
                status.quota.as_ref().map(ProviderQuotaData::short_text)?,
            ))
        })
        .collect()
}

/// 换上一份额度（测试用，等同收到一次 [`QUOTA_CHANGED`]）。
#[cfg(test)]
pub(crate) fn apply_quota(quota: &Quota, cx: &mut App) {
    cx.default_global::<Cache>().pi_badges = pi_badges(quota);
}

/// 那台机器的智能体配置；还没收到、给不了是 `None`。只读内存，render 里可以调。
pub(crate) fn settings<'a>(machine: &RuntimeKey, cx: &'a App) -> Option<&'a AgentHostSettings> {
    cx.try_global::<Cache>()?.settings.get(machine)
}

/// Pi 那家 provider 的额度短文本（如 `25%`）。查不到是 `None`。只读内存。
pub(crate) fn pi_quota_badge(provider_id: &str, cx: &App) -> Option<String> {
    cx.try_global::<Cache>()?
        .pi_badges
        .get(provider_id)
        .cloned()
}

/// 改那台机器的智能体配置：在手里那份上调 `f`，把改动交给智能体插件。配置还没收到时报错，
/// `f` 什么都没改时不发。交回的那份先换进缓存，接着改的以它为底；之后的推送照常整份换。
pub(crate) fn edit_settings(
    machine: &RuntimeKey,
    f: impl FnOnce(&mut AgentHostSettings),
    cx: &mut App,
) -> Task<Result<(), String>> {
    let Some(before) = settings(machine, cx).cloned() else {
        return Task::ready(Err("那台机器的智能体配置还没收到".to_string()));
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
