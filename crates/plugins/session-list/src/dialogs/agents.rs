//! 新建对话的启动规格：按那台机器的智能体配置（约定见 [`crate::peers::agents`]）现算。
//! 新建目录里只有选项 id，命令和环境变量在出线时就去掉了。

use smelt_plugin_api::model::agent_host::AgentHostSettings;
use smelt_plugin_api::model::agent_kind::ConversationAgentKind;
use smelt_plugin_api::model::session_control::{
    agent_definition_id_from_option_id, profile_id_from_option_id,
};
use smelt_plugin_api::model::workspace_directory::WorkspaceAcp;

/// 按选项 id 拼出新对话在目录里的那条：引擎、启动规格、profile、智能体归属和模型。`sid`
/// 由调用方分。配置里已经没有这个选项（删了、改坏了）是 `None`。
///
/// 只填这次新建明确要的会话选项（智能体定义的模型）；这个 agent 记住的选择由对话插件
/// 接上时合进去，这里不重复存一份。
pub(super) fn conversation_acp(
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
