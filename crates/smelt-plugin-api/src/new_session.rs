//! 新建会话要的两条规则：新建目录里一项画什么图标，选了一项对话后按那台机器的智能体配置拼出
//! 启动规格。图标要和侧栏认 CLI 的规则一致，启动规格要和运行时解析智能体配置的规则一致，
//! 所以放在这里，Rust 插件直接调，JS 插件经同名函数拿。

use crate::model::agent_host::AgentHostSettings;
use crate::model::agent_kind::{ConversationAgentKind, TerminalAgentKind};
use crate::model::new_session::NewSessionTarget;
use crate::model::session_control::{
    agent_definition_id_from_option_id, profile_id_from_option_id,
};
use crate::model::workspace_directory::WorkspaceAcp;
use crate::ui::IconName;

/// 新建目录里一项行首的图标：引擎或终端 agent 自己的图标；自定义 Agent 一律画通用的，和侧栏
/// 一致；认不出的对话画机器人，认不出的终端命令画终端。
pub fn new_session_icon(target: &NewSessionTarget) -> String {
    match target {
        NewSessionTarget::Conversation {
            agent_option_id,
            agent_kind,
        } => profile_id_from_option_id(agent_option_id)
            .map(|_| ConversationAgentKind::Custom)
            .or_else(|| ConversationAgentKind::from_id(agent_kind))
            .map_or_else(
                || IconName::Bot.path().to_string(),
                |kind| kind.icon_asset(),
            ),
        NewSessionTarget::Terminal { command, provider } => provider
            .as_deref()
            .and_then(TerminalAgentKind::from_id)
            .or_else(|| TerminalAgentKind::from_command_prefix(command))
            .map_or_else(
                || IconName::Terminal.path().to_string(),
                |kind| kind.icon_asset(),
            ),
        NewSessionTarget::BlankTerminal => IconName::Terminal.path().to_string(),
    }
}

/// 按选项 id（新建目录里对话项的 `agentOptionId`）拼出新对话在目录里的那条：引擎、启动规格、
/// profile、智能体归属和模型。`sid` 由调用方分。配置里已经没有这个选项（删了、改坏了）是
/// `None`。
///
/// 只填这次新建明确要的会话选项（智能体定义的模型）；这个 agent 记住的选择由对话插件接上时
/// 合进去，这里不重复存一份。
pub fn conversation_acp(
    settings: &AgentHostSettings,
    option_id: &str,
    sid: String,
    cwd: String,
) -> Option<WorkspaceAcp> {
    let mut acp = WorkspaceAcp {
        cwd: Some(cwd),
        sid,
        ..WorkspaceAcp::default()
    };
    if let Some(definition_id) = agent_definition_id_from_option_id(option_id) {
        let definition = settings.find_agent(definition_id)?;
        let (kind, launch) = settings.resolve_agent(definition).ok()?;
        acp.agent = kind.id().to_string();
        acp.launch = launch;
        acp.agent_definition_id = Some(definition.id.clone());
        if let Some(model) = definition.model_config_value() {
            acp.config_values.push(("model".to_string(), model));
        }
        return Some(acp);
    }
    if let Some(profile_id) = profile_id_from_option_id(option_id) {
        let profile = settings.find_profile(profile_id)?;
        let kind = profile.kind()?;
        acp.agent = kind.id().to_string();
        acp.launch = settings.profile_launch_spec(profile).ok()?;
        acp.profile_id = Some(profile_id.to_string());
        return Some(acp);
    }
    let kind = ConversationAgentKind::from_id(option_id)?;
    acp.agent = kind.id().to_string();
    acp.launch = settings.acp_launch_for(kind);
    Some(acp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_agents_draw_the_generic_conversation_icon() {
        let profile = NewSessionTarget::Conversation {
            agent_option_id: "profile:mine".into(),
            agent_kind: ConversationAgentKind::ALL[0].id().into(),
        };
        assert_eq!(
            new_session_icon(&profile),
            ConversationAgentKind::Custom.icon_asset()
        );
    }

    #[test]
    fn unknown_terminal_commands_draw_a_terminal() {
        let terminal = NewSessionTarget::Terminal {
            command: "make dev".into(),
            provider: None,
        };
        let path = IconName::Terminal.path().to_string();
        assert_eq!(new_session_icon(&terminal), path);
        assert_eq!(new_session_icon(&NewSessionTarget::BlankTerminal), path);
    }

    #[test]
    fn a_bare_engine_launches_with_the_machines_command() {
        let settings = AgentHostSettings::fresh();
        let kind = ConversationAgentKind::ALL[0];
        let acp = conversation_acp(&settings, kind.id(), "acp-1".into(), "/repo".into()).unwrap();
        assert_eq!(acp.agent, kind.id());
        assert_eq!(acp.sid, "acp-1");
        assert_eq!(acp.cwd.as_deref(), Some("/repo"));
        assert_eq!(acp.launch, settings.acp_launch_for(kind));
        assert_eq!(acp.profile_id, None);
        assert_eq!(acp.agent_definition_id, None);
        assert!(acp.config_values.is_empty());
    }

    #[test]
    fn options_missing_from_the_settings_resolve_to_nothing() {
        let settings = AgentHostSettings::fresh();
        assert!(conversation_acp(&settings, "agent:gone", "s".into(), "/r".into()).is_none());
        assert!(conversation_acp(&settings, "profile:gone", "s".into(), "/r".into()).is_none());
        assert!(conversation_acp(&settings, "no-such-engine", "s".into(), "/r".into()).is_none());
    }
}
