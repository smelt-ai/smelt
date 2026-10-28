//! 运行时的工作区目录：开着哪些项目、有哪些对话。
//!
//! 运行时是唯一的写入方。客户端用单条命令改它（`workspace.conversation.*`、
//! `workspace.project.*`），经 `workspace.changed` 收整份投影，订阅首帧就带着当前这份。
//! 侧栏怎么排、折叠和固定、终端怎么分屏、当前看哪个，是每台客户端自己的界面状态，
//! 不在这里。

use serde::{Deserialize, Serialize};

use crate::agent_kind::ConversationLaunchSpec;
use crate::conversation::ConversationBinding;

/// 整份目录。`revision` 每次有改动加一。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDirectory {
    pub revision: u64,
    /// 打开的项目根目录，按打开的先后。
    #[serde(default)]
    pub projects: Vec<String>,
    /// 按新建的先后。
    #[serde(default)]
    pub conversations: Vec<WorkspaceConversation>,
}

/// 一段对话：侧栏里的一条。要么是一段 ACP 对话，要么是一组终端。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConversation {
    /// 新建它的客户端生成，之后不再变。
    pub id: String,
    #[serde(default)]
    pub custom_title: Option<String>,
    /// 最近一次内容变化的 unix 秒。
    #[serde(default)]
    pub last_updated_at: i64,
    /// 这段对话里的终端，按客户端给的顺序。ACP 对话没有。
    #[serde(default)]
    pub terminals: Vec<WorkspaceTerminal>,
    #[serde(default)]
    pub acp: Option<WorkspaceAcp>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceTerminal {
    /// 守护里的终端会话 id。没起过的终端没有，运行时用对话 id 派生一个占位。
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub custom_title: Option<String>,
    #[serde(default)]
    pub launch_label: Option<String>,
    #[serde(default)]
    pub launch_cmd: Option<String>,
}

/// ACP 对话的启动参数和尚未交付的状态。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceAcp {
    pub cwd: Option<String>,
    /// 守护托管用的会话 id。写入时必填。
    pub sid: String,
    /// agent 种类标识（`ConversationAgentKind::id()`）。
    pub agent: String,
    #[serde(default)]
    pub profile_id: Option<String>,
    /// 续接用的 provider 对话 id。守护确认过之后以守护为准，客户端带来的旧值不覆盖它。
    #[serde(default)]
    pub history_session_id: Option<String>,
    pub launch: ConversationLaunchSpec,
    #[serde(default)]
    pub refresh_launch_from_settings: bool,
    #[serde(default)]
    pub conversation_binding: ConversationBinding,
    /// 最后生效的 provider 配置：`(config_id, value_id)`，按设置顺序。
    #[serde(default)]
    pub config_values: Vec<(String, String)>,
    #[serde(default)]
    pub pending_prompt: Option<String>,
    #[serde(default)]
    pub pending_delivery_id: Option<String>,
    #[serde(default)]
    pub pending_agent_preset: Option<String>,
    #[serde(default)]
    pub session_title: Option<String>,
    #[serde(default)]
    pub agent_definition_id: Option<String>,
    #[serde(default)]
    pub automation_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConversationIdParams {
    pub conversation_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceProjectParams {
    pub root: String,
}

/// 命令执行后目录的版本。没改动时是原来的版本。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRevision {
    pub revision: u64,
}
