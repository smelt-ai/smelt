//! CLI agent hooks 归一化后的稳定事件协议。
//!
//! provider 适配器只负责把各家的 hook 名称/字段翻译成这里的语义事件；smeltd
//! 再据此归约会话状态。协议带版本号，避免 helper 与守护升级不同步时静默误判。

pub const AGENT_EVENT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentEvent {
    pub version: u32,
    pub provider: String,
    pub kind: AgentEventKind,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
}

impl AgentEvent {
    pub fn new(provider: impl Into<String>, kind: AgentEventKind) -> Self {
        Self {
            version: AGENT_EVENT_VERSION,
            provider: provider.into(),
            kind,
            message: None,
            tool_name: None,
            tool_use_id: None,
            agent_id: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventKind {
    SessionStarted,
    PromptSubmitted,
    ToolStarted,
    ToolFinished,
    ToolFailed,
    ApprovalRequested,
    InputRequested,
    SubagentStarted,
    SubagentStopped,
    TurnSucceeded,
    TurnFailed,
    SessionEnded,
}
