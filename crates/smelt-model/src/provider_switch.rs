//! 跨 provider 的线上请求和可共享上下文。原生会话 ID 由运行时按身份查询。

use crate::workspace_directory::WorkspaceAcp;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSwitchParams {
    pub session_id: String,
    /// 乐观并发前置条件：客户端眼下看到的 agent option id。
    pub expected_provider: String,
    pub agent_option_id: String,
}

pub fn provider_key(acp: &WorkspaceAcp) -> String {
    if let Some(id) = &acp.agent_definition_id {
        format!("agent:{id}")
    } else if let Some(id) = &acp.profile_id {
        format!("profile:{id}")
    } else {
        acp.agent.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_request_round_trips_and_rejects_unknown_fields() {
        let request = ProviderSwitchParams {
            session_id: "s".into(),
            expected_provider: "codex".into(),
            agent_option_id: "claude".into(),
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(
            serde_json::from_value::<ProviderSwitchParams>(value.clone()).unwrap(),
            request
        );
        let mut bad = value;
        bad["native_session_id"] = "wrong-provider-id".into();
        assert!(serde_json::from_value::<ProviderSwitchParams>(bad).is_err());
    }

    #[test]
    fn identity_includes_profile_and_product_agent() {
        let mut acp = WorkspaceAcp {
            agent: "codex".into(),
            ..Default::default()
        };
        assert_eq!(provider_key(&acp), "codex");
        acp.profile_id = Some("work".into());
        assert_eq!(provider_key(&acp), "profile:work");
        acp.agent_definition_id = Some("reviewer".into());
        assert_eq!(provider_key(&acp), "agent:reviewer");
    }
}
