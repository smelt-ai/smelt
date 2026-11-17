//! 调用的会话（来源 `smelt.sessions`）的约定：跳到某段会话、开新对话、分叉。照抄自
//! 权威定义 `crates/plugins/sessions/src/contract.rs`，id 和字段必须和那边一致。
//!
//! 能力库（来源 `smelt.capabilities`）的视图 id 照抄自 `crates/plugins/capabilities/src/
//! contract.rs`。

use serde::Serialize;
use smelt_plugin_api::model::acp_conn::AcpForkCut;
use smelt_plugin_api::{Command, RuntimeKey, machine_json};

/// 能力库页。参数 `{"tab": "models" | "plugins" | "mcp" | "memory", "machine": …}`。
pub(crate) const CAPABILITIES_VIEW: &str = "smelt.capabilities/main";

// 约定：smelt.sessions 的 ACTIVATE，权威定义在 crates/plugins/sessions/src/contract.rs。
/// 在主窗口里露出这个会话。
pub(crate) const ACTIVATE: Command<Activate, ()> = Command::new("smelt.sessions/activate");

/// 开一段新对话（跟一个智能体，或裸引擎）：开在那台机器上、`cwd` 里，露在主窗口里。
pub(crate) const START_CONVERSATION: Command<StartConversation, ()> =
    Command::new("smelt.sessions/start-conversation");

/// 从一段开着的对话分叉出一段新对话：同一台机器、同一个 agent 和工作目录，历史复制自
/// `provider_session_id`，按 `cut` 切。
pub(crate) const FORK: Command<Fork, ()> = Command::new("smelt.sessions/fork");

/// [`ACTIVATE`] 的参数。
#[derive(Serialize)]
pub(crate) struct Activate {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 守护会话 id。
    pub session_id: String,
}

/// [`START_CONVERSATION`] 的参数。
#[derive(Serialize)]
pub(crate) struct StartConversation {
    /// 智能体定义所在的机器，对话也开在那台机器上。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 那台机器上的工作目录。
    pub cwd: String,
    /// 跟谁开，JSON 里平铺。
    #[serde(flatten)]
    pub with: StartWith,
}

/// [`StartConversation`] 跟谁开。
#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum StartWith {
    /// 跟一个智能体。
    Agent { agent_definition_id: String },
    /// 裸引擎：基础 agent（`ConversationAgentKind::id`），或它的一个自定义 Agent。
    Engine {
        agent: String,
        profile_id: Option<String>,
    },
}

/// [`FORK`] 的参数。
#[derive(Serialize)]
pub(crate) struct Fork {
    /// 源对话在哪台机器上，新对话也开在那里。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 源对话的守护会话 id。
    pub session_id: String,
    /// 源对话在 agent 那边的会话 id。
    pub provider_session_id: String,
    /// 副本切到哪条用户消息之前（不含它）；`None` 是整份。
    pub cut: Option<AcpForkCut>,
    /// 预填进新对话输入框的话，不替用户发出；空的是不预填。
    pub prompt: String,
}
