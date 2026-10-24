//! 智能体配置（命令、环境变量、profile、定义、开关）的数据。存取和合并在运行时。

use crate::agent_definition::AgentDefinition;
use crate::agent_kind::{
    AcpProfile, CONVERSATION_AGENTS, ConversationAgentKind, ConversationLaunchSpec,
};
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
            agents: Vec::new(),
            default_conversation_agent: None,
        }
    }
}

impl AgentHostSettings {
    /// 某个 agent 种类当前生效的启动命令。没配的按出厂命令。
    pub fn acp_cmd_for(&self, agent: ConversationAgentKind) -> String {
        self.commands
            .get(agent.id())
            .cloned()
            .unwrap_or_else(|| agent.default_cmd())
    }

    /// 改某个 agent 的启动命令（设置页各条输入框共用）。
    pub fn set_acp_cmd_for(&mut self, agent: ConversationAgentKind, cmd: String) {
        self.commands.insert(agent.id().to_string(), cmd);
    }

    pub fn find_profile(&self, id: &str) -> Option<&AcpProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn find_agent(&self, id: &str) -> Option<&AgentDefinition> {
        self.agents.iter().find(|agent| agent.id == id)
    }

    /// 某个 agent 当前生效的额外环境变量。
    pub fn acp_env_for(&self, agent: ConversationAgentKind) -> BTreeMap<String, String> {
        self.env.get(agent.id()).cloned().unwrap_or_default()
    }

    /// 改某个 agent 的额外环境变量；清空即删键，别在配置里留一堆空对象。
    pub fn set_acp_env_for(&mut self, agent: ConversationAgentKind, env: BTreeMap<String, String>) {
        if env.is_empty() {
            self.env.remove(agent.id());
        } else {
            self.env.insert(agent.id().to_string(), env);
        }
    }

    /// 某个 agent 最近由用户选中的配置。返回去重后的副本，避免手改配置产生同一
    /// config id 多个值，恢复时连续发送互相覆盖的请求。
    pub fn remembered_acp_config(&self, agent: ConversationAgentKind) -> Vec<(String, String)> {
        let mut remembered = Vec::new();
        for (config_id, value_id) in self.config_memory.get(agent.id()).into_iter().flatten() {
            upsert_config_value(&mut remembered, config_id.clone(), value_id.clone());
        }
        remembered
    }

    /// 记录一次用户显式选择。同一配置只保留最后一个值；profile/cwd 不参与 key。
    /// 默认值归引擎自己配置的项（Pi 的模型、推理档位）不记，见 `remembers_config`。
    pub fn remember_acp_config_value(
        &mut self,
        agent: ConversationAgentKind,
        config_id: String,
        value_id: String,
    ) {
        if config_id.trim().is_empty()
            || value_id.trim().is_empty()
            || !agent.remembers_config(&config_id)
        {
            return;
        }
        let values = self
            .config_memory
            .entry(agent.id().to_string())
            .or_default();
        upsert_config_value(values, config_id, value_id);
    }

    /// 新会话的初始配置：先铺该 agent 的最近选择，再由会话自身存档逐项覆盖。
    /// 因此冷恢复不会把某条会话原本的模型顶掉，同时能继承它尚未显式设置的档位。
    pub fn initial_acp_config(
        &self,
        agent: ConversationAgentKind,
        session_values: &[(String, String)],
    ) -> Vec<(String, String)> {
        let mut values = self.remembered_acp_config(agent);
        for (config_id, value_id) in session_values {
            upsert_config_value(&mut values, config_id.clone(), value_id.clone());
        }
        values
    }

    /// 产品智能体由用户显式创建并长期托管，新开对话默认可自主执行。其余配置仍继承
    /// 最近选择，但权限不能继承普通对话里可能选过的逐次审批。
    pub fn initial_agent_conversation_config(
        &self,
        agent: ConversationAgentKind,
    ) -> Vec<(String, String)> {
        let mut values = self.initial_acp_config(agent, &[]);
        if let Some(mode) = agent.task_params().full_access_mode {
            upsert_config_value(&mut values, "mode".to_string(), mode.to_string());
        }
        values
    }

    /// 某个 agent 当前生效的完整启动规格：命令 + 用户配的环境变量。
    ///
    /// 唯一接缝。会话侧一律走这里，不要再自己 `from_command(acp_cmd_for(..))`
    /// ——那样拼出来的规格永远少了环境变量，且漏一处就是"设置里填了不生效"。
    pub fn acp_launch_for(&self, agent: ConversationAgentKind) -> ConversationLaunchSpec {
        let mut launch = agent.default_launch();
        launch.command = self.acp_cmd_for(agent);
        // 用户显式配置优先于出厂覆盖；这样既默认全权限，也保留降权和自定义
        // OPENCODE_CONFIG_CONTENT 的出口。
        for (name, value) in self.acp_env_for(agent) {
            launch.env.insert(name, value);
        }
        launch
    }

    pub fn profile_launch_spec(
        &self,
        profile: &AcpProfile,
    ) -> Result<ConversationLaunchSpec, String> {
        let kind = profile.kind().ok_or_else(|| {
            format!(
                "workspace profile `{}` 使用了未知 ACP Agent `{}`",
                profile.id, profile.kind_id
            )
        })?;
        let mut launch = profile.launch_spec()?;
        launch.command = self.acp_cmd_for(kind);
        // workspace 目录那条环境变量是 profile 的立身之本，不能被用户配的同名
        // 变量顶掉；其余按 kind 继承并允许显式覆盖出厂环境，这样既能默认全权限，
        // 也能在设置里把某个 OpenCode workspace 降权。
        let workspace_env = profile.env_var()?;
        for (name, value) in self.acp_env_for(kind) {
            if name != workspace_env {
                launch.env.insert(name, value);
            }
        }
        Ok(launch)
    }

    /// 将智能体定义解析成执行引擎启动规格。工作目录、账号/profile 与模型属于
    /// Conversation/Run 的执行上下文，不能从定义里偷偷带进来。
    pub fn resolve_agent(
        &self,
        definition: &AgentDefinition,
    ) -> Result<(ConversationAgentKind, ConversationLaunchSpec), String> {
        let agent = definition.engine_kind().ok_or_else(|| {
            format!(
                "智能体 `{}` 使用了未知执行引擎 `{}`",
                definition.name, definition.engine_kind_id
            )
        })?;
        Ok((agent, self.acp_launch_for(agent)))
    }
}

fn upsert_config_value(values: &mut Vec<(String, String)>, config_id: String, value_id: String) {
    if let Some((_, current)) = values.iter_mut().find(|(id, _)| id == &config_id) {
        *current = value_id;
    } else {
        values.push((config_id, value_id));
    }
}

/// 客户端一次修改：它看到的那份和它改成的那份。只有两者不同的字段会被写入。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHostEdit {
    pub before: AgentHostSettings,
    pub after: AgentHostSettings,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> AgentHostSettings {
        AgentHostSettings::fresh()
    }

    #[test]
    fn definitions_resolve_to_their_engine() {
        let mut config = settings();
        config.agents.push(AgentDefinition {
            id: "reviewer".into(),
            name: "代码审查".into(),
            engine_kind_id: ConversationAgentKind::Codex.id().into(),
            ..Default::default()
        });
        let definition = config.find_agent("reviewer").expect("刚加的定义");
        assert_eq!(
            config.resolve_agent(definition).unwrap().0,
            ConversationAgentKind::Codex
        );
        assert!(config.find_agent("missing").is_none());
    }

    /// 设置页填的环境变量必须真的跟着会话走。之前会话侧是自己
    /// `from_command(acp_cmd_for(..))` 拼的规格，环境变量在那条路上会被整段丢掉，
    /// 而症状要等到真发一轮才暴露成"key 没配"。
    #[test]
    fn launch_spec_carries_the_env_from_settings() {
        let mut config = settings();
        config.set_acp_env_for(
            ConversationAgentKind::Codex,
            BTreeMap::from([("OPENAI_API_KEY".into(), "sk-abc".into())]),
        );

        let launch = config.acp_launch_for(ConversationAgentKind::Codex);
        assert_eq!(
            launch.command,
            config.acp_cmd_for(ConversationAgentKind::Codex)
        );
        assert_eq!(
            launch.env.get("OPENAI_API_KEY").map(String::as_str),
            Some("sk-abc")
        );

        // 没配的那家不能被别家的变量污染。
        assert!(
            config
                .acp_launch_for(ConversationAgentKind::Claude)
                .env
                .is_empty()
        );
    }

    #[test]
    fn opencode_acp_defaults_to_process_local_full_permissions() {
        let launch = settings().acp_launch_for(ConversationAgentKind::OpenCode);

        assert_eq!(launch.command, "opencode acp");
        assert_eq!(
            launch
                .env
                .get("OPENCODE_CONFIG_CONTENT")
                .map(String::as_str),
            Some(r#"{"permission":"allow"}"#)
        );
    }

    #[test]
    fn remembered_acp_config_is_keyed_by_agent_not_profile_or_project() {
        let mut config = settings();
        config.remember_acp_config_value(
            ConversationAgentKind::Grok,
            "model".into(),
            "grok-4".into(),
        );
        config.remember_acp_config_value(
            ConversationAgentKind::OpenCode,
            "model".into(),
            "deepseek-v4".into(),
        );
        config.remember_acp_config_value(
            ConversationAgentKind::Codex,
            "reasoning_effort".into(),
            "high".into(),
        );

        assert_eq!(
            config.remembered_acp_config(ConversationAgentKind::Grok),
            vec![("model".into(), "grok-4".into())]
        );
        assert_eq!(
            config.remembered_acp_config(ConversationAgentKind::OpenCode),
            vec![("model".into(), "deepseek-v4".into())]
        );
        assert_eq!(
            config.remembered_acp_config(ConversationAgentKind::Codex),
            vec![("reasoning_effort".into(), "high".into())]
        );
        assert!(
            config
                .remembered_acp_config(ConversationAgentKind::Claude)
                .is_empty()
        );
    }

    #[test]
    fn session_config_overrides_agent_memory_and_inherits_missing_values() {
        let mut config = settings();
        config.remember_acp_config_value(
            ConversationAgentKind::Codex,
            "model".into(),
            "gpt-5.6".into(),
        );
        config.remember_acp_config_value(
            ConversationAgentKind::Codex,
            "reasoning_effort".into(),
            "medium".into(),
        );

        assert_eq!(
            config.initial_acp_config(ConversationAgentKind::Codex, &[]),
            vec![
                ("model".into(), "gpt-5.6".into()),
                ("reasoning_effort".into(), "medium".into()),
            ]
        );
        assert_eq!(
            config.initial_acp_config(
                ConversationAgentKind::Codex,
                &[("reasoning_effort".into(), "high".into())],
            ),
            vec![
                ("model".into(), "gpt-5.6".into()),
                ("reasoning_effort".into(), "high".into()),
            ]
        );
    }

    #[test]
    fn product_agent_conversation_forces_full_access_without_losing_recent_config() {
        let mut config = settings();
        config.remember_acp_config_value(
            ConversationAgentKind::Pi,
            "mode".into(),
            "default".into(),
        );
        config.remember_acp_config_value(ConversationAgentKind::Pi, "codemode".into(), "on".into());

        assert!(
            config
                .initial_acp_config(ConversationAgentKind::Pi, &[])
                .contains(&("mode".into(), "default".into()))
        );
        assert_eq!(
            config.initial_agent_conversation_config(ConversationAgentKind::Pi),
            vec![
                ("mode".into(), "bypassPermissions".into()),
                ("codemode".into(), "on".into()),
            ]
        );
    }

    /// Pi 新对话的模型和推理档位只认 `~/.pi/agent/settings.json`。在对话里换模型不能
    /// 变成第二份默认值：否则新对话先按 settings.json 起，Idle 后又被改成上次的选择，
    /// 模型胶囊闪一下。别家没有这份引擎默认值，照旧记最近选择。
    #[test]
    fn pi_model_and_thinking_choices_stay_in_their_conversation() {
        let mut config = settings();
        config.remember_acp_config_value(
            ConversationAgentKind::Pi,
            "model".into(),
            "commandcode/deepseek/deepseek-v4.1-flash-fast".into(),
        );
        config.remember_acp_config_value(
            ConversationAgentKind::Pi,
            "thought_level".into(),
            "max".into(),
        );
        config.remember_acp_config_value(
            ConversationAgentKind::Codex,
            "model".into(),
            "gpt-5.6".into(),
        );

        assert!(
            config
                .remembered_acp_config(ConversationAgentKind::Pi)
                .is_empty()
        );
        assert_eq!(
            config.initial_agent_conversation_config(ConversationAgentKind::Pi),
            vec![("mode".into(), "bypassPermissions".into())]
        );
        assert_eq!(
            config.remembered_acp_config(ConversationAgentKind::Codex),
            vec![("model".into(), "gpt-5.6".into())]
        );
    }

    /// 清空即删键：配置里不该留一堆空对象，否则每次存盘都在长。
    #[test]
    fn clearing_the_env_drops_the_key_instead_of_storing_an_empty_map() {
        let mut config = settings();
        config.set_acp_env_for(
            ConversationAgentKind::Codex,
            BTreeMap::from([("K".to_string(), "v".to_string())]),
        );
        config.set_acp_env_for(ConversationAgentKind::Codex, BTreeMap::new());

        assert!(config.env.is_empty());
    }

    /// workspace profile 继承 kind 的环境变量（给某家配一次 key，它的每个
    /// workspace 都能用），但 profile 自己那条数据目录变量不能被顶掉。
    #[test]
    fn profile_inherits_kind_env_without_losing_its_workspace_dir() {
        let profile = AcpProfile {
            id: "p1".into(),
            kind_id: ConversationAgentKind::Claude.id().into(),
            label: "Claude Quant".into(),
            workspace_dir: "/tmp/quant".into(),
        };
        let dir_var = profile
            .env_var()
            .expect("Claude 支持 workspace")
            .to_string();
        let mut config = settings();
        config.set_acp_env_for(
            ConversationAgentKind::Claude,
            BTreeMap::from([
                ("ANTHROPIC_BASE_URL".to_string(), "https://gw".to_string()),
                (dir_var.clone(), "/tmp/hijacked".to_string()),
            ]),
        );

        let launch = config.profile_launch_spec(&profile).expect("有效 profile");
        assert_eq!(
            launch.env.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some("https://gw")
        );
        assert_eq!(
            launch.env.get(&dir_var).map(String::as_str),
            Some("/tmp/quant"),
            "profile 的数据目录是它的立身之本，不能被同名变量顶掉"
        );
    }

    /// 运行时给的配置缺某家的命令时用出厂命令。
    #[test]
    fn missing_command_falls_back_to_descriptor_default() {
        let mut config = settings();
        config.commands.clear();
        for kind in ConversationAgentKind::ALL {
            assert_eq!(config.acp_cmd_for(kind), kind.default_cmd());
        }
    }

    #[test]
    fn profile_launch_uses_configured_agent_command_and_workspace_env() {
        let mut config = settings();
        config.set_acp_cmd_for(ConversationAgentKind::Claude, "claude-custom --acp".into());
        let profile = AcpProfile {
            id: "quant".into(),
            kind_id: "claude".into(),
            label: "Quant".into(),
            workspace_dir: "~/Claude Workspaces/quant".into(),
        };

        let launch = config.profile_launch_spec(&profile).expect("有效 profile");

        assert_eq!(launch.command, "claude-custom --acp");
        assert_eq!(
            launch.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("~/Claude Workspaces/quant")
        );
    }

    #[test]
    fn unknown_profile_cannot_resolve_a_launch_spec() {
        let profile = AcpProfile {
            id: "future-profile".into(),
            kind_id: "future-agent".into(),
            label: "Future Agent".into(),
            workspace_dir: "~/.future-agent".into(),
        };

        let error = settings()
            .profile_launch_spec(&profile)
            .expect_err("未知 Agent 不能借用默认启动规格");
        assert!(error.contains("future-agent"));
    }
}
