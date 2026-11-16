//! 对话组件向使用方要的外部数据。
//!
//! 这些是机器、账户一级的状态，不属于哪段对话，也不归组件管：每台机器的智能体配置（运行时
//! 推来的只读缓存）、Pi 各家 provider 的额度。组件只说要什么（[`ConversationEnv`]），由使用方
//! 启动时装一份（[`install`]），所有对话视图共用；没装（测试、启动早期）就当什么都没有：
//! 重新开始时沿用原来的启动规格，不显示自定义 Agent 的名字和额度。
//!
//! Pi 的模型目录只能问 Pi 进程自己，会话启动时拉一次。使用方知道 Pi 配置变了（登录、登出、
//! 改 provider）时调 [`pi_config_changed`]，活着的会话据此重拉自己的目录。

use gpui::{App, Global};
use smelt_client::runtime::RuntimeKey;
use smelt_model::agent_host::AgentHostSettings;

/// 对话视图要读的外部数据。只读内存：视图在 render 里也会调。
pub trait ConversationEnv: 'static {
    /// 那台机器的智能体配置。重新开始时按它重拼启动规格，自定义 Agent 的对话按它显示名字。
    /// 还没收到、给不了是 `None`。
    fn agent_settings<'a>(
        &self,
        machine: &RuntimeKey,
        cx: &'a App,
    ) -> Option<&'a AgentHostSettings>;
    /// Pi 那家 provider 的额度短文本（如 `25%`），跟在模型菜单里 provider 名字后面。查不到是
    /// `None`。
    fn pi_quota_badge(&self, provider_id: &str, cx: &App) -> Option<String>;
}

struct EnvGlobal(Box<dyn ConversationEnv>);

impl Global for EnvGlobal {}

/// Pi 配置变了的信号，不带数据：每变一次重新放一份，观察它的对话视图就醒来。
pub(crate) struct PiCatalog;

impl Global for PiCatalog {}

/// 使用方装上外部数据，建任何对话视图之前调一次。
pub fn install(env: impl ConversationEnv, cx: &mut App) {
    cx.set_global(EnvGlobal(Box::new(env)));
}

/// 本机的 Pi 配置变了（凭据、provider、模型）：活着的对话重拉模型目录。
pub fn pi_config_changed(cx: &mut App) {
    cx.set_global(PiCatalog);
}

pub(crate) fn agent_settings<'a>(
    machine: &RuntimeKey,
    cx: &'a App,
) -> Option<&'a AgentHostSettings> {
    cx.try_global::<EnvGlobal>()?.0.agent_settings(machine, cx)
}

pub(crate) fn pi_quota_badge(provider_id: &str, cx: &App) -> Option<String> {
    cx.try_global::<EnvGlobal>()?
        .0
        .pi_quota_badge(provider_id, cx)
}
