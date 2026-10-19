//! 智能体配置（命令、环境变量、profile、定义、开关）的数据。存取和合并在运行时。

use crate::agent_definition::AgentDefinition;
use crate::agent_kind::{AcpProfile, CONVERSATION_AGENTS};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHostSettings {
    /// 是否由 Smelt 管理各 CLI agent 的结构化 hooks。
    pub hooks_enabled: bool,
    /// agent id → 启动命令。没有的按该 agent 的出厂命令。
    pub commands: BTreeMap<String, String>,
    /// agent id → 额外注入的环境变量。
    pub env: BTreeMap<String, BTreeMap<String, String>>,
    /// agent id → 用户最近显式选择的配置 `(config_id, value_id)`。
    pub config_memory: BTreeMap<String, Vec<(String, String)>>,
    /// 手动添加的 workspace profile。
    pub profiles: Vec<AcpProfile>,
    /// 运行时那台机器上 `$DSH_HOME/profiles` 里发现的 profile。是事实不是设置，只读。
    pub native_dsh_profiles: Vec<AcpProfile>,
    pub agents: Vec<AgentDefinition>,
    /// 侧栏「新对话」默认用哪个智能体。指向这台运行时上的定义，所以跟着运行时走。
    pub default_conversation_agent: Option<String>,
}

impl AgentHostSettings {
    /// 新装：每家都按出厂命令填一份，hook 默认开启。
    pub fn fresh() -> Self {
        Self {
            hooks_enabled: true,
            commands: CONVERSATION_AGENTS
                .iter()
                .map(|d| (d.id.to_string(), (d.default_cmd)()))
                .collect(),
            env: BTreeMap::new(),
            config_memory: BTreeMap::new(),
            profiles: Vec::new(),
            native_dsh_profiles: Vec::new(),
            agents: Vec::new(),
            default_conversation_agent: None,
        }
    }
}

/// 客户端一次修改：它看到的那份和它改成的那份。只有两者不同的字段会被写入。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHostEdit {
    pub before: AgentHostSettings,
    pub after: AgentHostSettings,
}
