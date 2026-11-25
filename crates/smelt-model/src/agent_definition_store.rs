//! 智能体定义的增改请求，以及对话启动前请运行时补齐启动规格的线上数据。

use crate::agent_kind::ConversationLaunchSpec;
use serde::{Deserialize, Serialize};

/// 客户端打开一段对话前，要运行时替它把启动规格补齐成什么样。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LaunchPrep {
    /// 产品智能体的定义 id；裸引擎为空。
    pub agent_definition_id: Option<String>,
    /// 恢复存档里的会话：完好的快照原样沿用创建时的内容（见
    /// [`repair_agent_definition_launch`]），只补记忆目录。否则按当前定义重新组装。
    /// 带了 `restart` 时不看它。
    #[serde(default)]
    pub restore: bool,
    /// 用户点了「重新开始」：按运行时当前的设置和智能体定义重拼整份启动规格，传入的
    /// 规格只作拼不出来时的退路。旧运行时不认识这一项，会按 `restore` 处理，所以重新开始
    /// 时 `restore` 填 `true`：沿用原来的规格，不会拼出一份不对的。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<LaunchRestart>,
}

/// 重新开始时运行时重拼启动规格要的会话身份。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LaunchRestart {
    /// 执行引擎标识（`ConversationAgentKind::id()`）。运行时不认识就沿用原来的规格。
    pub engine_kind: String,
    /// 自定义 Agent 的 id。设置里已经删了就沿用原来的规格。
    pub profile_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LaunchPrepareParams {
    pub launch: ConversationLaunchSpec,
    pub prep: LaunchPrep,
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LaunchPrepareResult {
    pub launch: ConversationLaunchSpec,
    /// 这场会话会加载的技能，供对话头部展示。
    pub skills: Vec<crate::pi_plugin_catalog::PiPlugin>,
    /// 这场会话会连的 MCP 服务器。旧运行时不回这一项，按没有处理。
    #[serde(default)]
    pub mcp_servers: Vec<crate::pi_mcp::PiMcpServer>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LoadedSkillsParams {
    pub launch: ConversationLaunchSpec,
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LoadedSkillsResult {
    pub skills: Vec<crate::pi_plugin_catalog::PiPlugin>,
    /// 同 [`LaunchPrepareResult::mcp_servers`]。
    #[serde(default)]
    pub mcp_servers: Vec<crate::pi_mcp::PiMcpServer>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentDefinitionCreate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_kind_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_folders: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_links: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_enabled: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentDefinitionPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_kind_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugins: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_folders: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_links: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_enabled: Option<bool>,
}
