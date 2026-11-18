//! 调用的会话（来源 `smelt.sessions`）的约定：续接、交接历史会话的命令和参数，照抄自权威
//! 定义（`crates/plugins/sessions/src/contract.rs`），id 和字段必须和那边一致。

use serde::Serialize;
use smelt_plugin_api::{Command, RuntimeKey, machine_json};

// 约定：smelt.sessions 的 RESUME / RESUME_IN_TERMINAL / HANDOFF，权威定义在
// crates/plugins/sessions/src/contract.rs。
/// 用那家 agent 自己的续接接着一段历史会话；同一段已经开着就切过去。
pub(crate) const RESUME: Command<Resume, ()> = Command::new("smelt.sessions/resume");
/// 在终端里起那家 agent 的 CLI 恢复同一段历史会话。不看 `agent_definition_id`。
pub(crate) const RESUME_IN_TERMINAL: Command<Resume, ()> =
    Command::new("smelt.sessions/resume-in-terminal");
/// 起一段新会话，第一句话是交接说明。
pub(crate) const HANDOFF: Command<Handoff, ()> = Command::new("smelt.sessions/handoff");

/// 续接一段历史会话。
#[derive(Serialize)]
pub(crate) struct Resume {
    /// 存档在哪台机器上，会话也开在那台机器上。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 历史来源的 id（`HistorySourceKind::id`）。
    pub agent: String,
    /// 自定义 Agent 的 id；基础 agent 是 `None`。
    pub profile_id: Option<String>,
    /// 那台机器上的工作目录。
    pub cwd: String,
    /// agent 自己认的会话 id。
    pub resume_id: String,
    /// 这段会话属于哪个智能体定义；有的话按那个智能体续接。
    pub agent_definition_id: Option<String>,
}

/// 把一段对话交给另一家 agent。
#[derive(Serialize)]
pub(crate) struct Handoff {
    /// 新会话开在哪台机器上。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    pub cwd: String,
    /// 目标 agent（`ConversationAgentKind::id`）。
    pub agent: String,
    /// 目标是自定义 Agent 时它的 id。
    pub profile_id: Option<String>,
    /// 新会话的第一句话。
    pub prompt: String,
}
