//! 调用的会话（来源 `smelt.sessions`）的约定：跳到某段会话、开新对话、分叉。照抄自
//! 权威定义 `crates/plugins/sessions/src/contract.rs`，id 和字段必须和那边一致。
//!
//! 能力库（来源 `smelt.capabilities`）的视图 id 照抄自 `crates/plugins/capabilities/src/
//! contract.rs`。

use serde::Serialize;
use smelt_plugin_api::model::acp_conn::AcpForkCut;
use smelt_plugin_api::{Command, RuntimeKey, machine_json};

/// 能力库页。参数 `{"tab": "skills" | "extensions" | "mcp", "machine": …}`。
pub(crate) const CAPABILITIES_VIEW: &str = "smelt.capabilities/main";

// 约定：smelt.sessions 的 ACTIVATE，权威定义在 crates/plugins/sessions/src/contract.rs。
/// 在主窗口里露出这个会话。
pub(crate) const ACTIVATE: Command<Activate, ()> = Command::new("smelt.sessions/activate");

// 约定：smelt.sessions 的 REVEAL_IN_LIST，权威定义在 crates/plugins/sessions/src/contract.rs。
/// 在会话列表里找出这段会话：侧栏收着就打开，展开所在的组，滚过去闪一下。
pub(crate) const REVEAL_IN_LIST: Command<RevealInList, ()> =
    Command::new("smelt.sessions/reveal-in-list");

/// 开一段新对话（跟一个智能体，或裸引擎）：开在那台机器上、`cwd` 里，露在主窗口里。
pub(crate) const START_CONVERSATION: Command<StartConversation, ()> =
    Command::new("smelt.sessions/start-conversation");

/// 从一段开着的对话分叉出一段新对话：同一台机器、同一个 agent 和工作目录，历史复制自
/// `provider_session_id`，按 `cut` 切。
pub(crate) const FORK: Command<Fork, ()> = Command::new("smelt.sessions/fork");

// 约定：smelt.sessions 的 PREV_SESSION / NEXT_SESSION，权威定义在
// crates/plugins/sessions/src/contract.rs。
/// 按会话列表的顺序切到上一段会话，到头绕回。
pub(crate) const PREV_SESSION: Command<(), ()> = Command::new("smelt.sessions/prev");
/// 按会话列表的顺序切到下一段会话，到头绕回。
pub(crate) const NEXT_SESSION: Command<(), ()> = Command::new("smelt.sessions/next");

/// [`ACTIVATE`] 的参数。
#[derive(Serialize)]
pub(crate) struct Activate {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 守护会话 id。
    pub session_id: String,
}

/// [`REVEAL_IN_LIST`] 的参数。
#[derive(Serialize)]
pub(crate) struct RevealInList {
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
