//! 会话列表用到的别的插件（智能体、变更）的约定：本插件用到的 id 和字段，
//! 照抄自权威定义，形状必须和那边一致。只抄用到的字段：发出去的参数里没抄的字段对方按默认值
//! 处理。

/// 智能体（来源 `smelt.agents`）的约定，权威定义在 `crates/plugins/agents/src/contract.rs`。
/// 新建会话的菜单按它列出能开的对话，会话行按它标跟哪个智能体开的。
pub(crate) mod agents {
    use serde::Deserialize;
    use smelt_plugin_api::model::agent_host::AgentHostSettings;
    use smelt_plugin_api::{Command, Event, RuntimeKey, machine_json};

    // 约定：smelt.agents 的 AGENT_SETTINGS。
    /// 各台机器眼下的智能体配置，只含收到过推送的机器。
    pub(crate) const AGENT_SETTINGS: Command<(), Vec<MachineAgentSettings>> =
        Command::new("smelt.agents/agent-settings");

    // 约定：smelt.agents 的 AGENT_SETTINGS_CHANGED。
    /// 一台机器的智能体配置换了（整份），或那台机器给不了、移除了。
    pub(crate) const AGENT_SETTINGS_CHANGED: Event<MachineAgentSettings> =
        Event::new("smelt.agents/agent-settings-changed");

    /// 一台机器的智能体配置。
    #[derive(Clone, Debug, Deserialize)]
    pub(crate) struct MachineAgentSettings {
        #[serde(with = "machine_json")]
        pub machine: RuntimeKey,
        #[serde(flatten)]
        pub state: AgentSettingsState,
    }

    #[derive(Clone, Debug, Deserialize)]
    #[serde(tag = "state", rename_all = "snake_case")]
    pub(crate) enum AgentSettingsState {
        Ready {
            settings: AgentHostSettings,
        },
        /// 那台机器给不了，原因给人看。
        Unavailable {
            reason: String,
        },
        /// 机器移除了。
        Absent,
    }
}

/// 变更（来源 `smelt.git`）的约定，权威定义在 `crates/plugins/git/src/contract.rs`。
pub(crate) mod git {
    use serde::Serialize;
    use smelt_plugin_api::{Command, RuntimeKey, machine_json};

    // 约定：smelt.git 的 OPEN_PULL_REQUEST。
    /// 在浏览器里打开那个仓库当前分支的新建 PR 页。开不了时命令失败，原因给人看。
    pub(crate) const OPEN_PULL_REQUEST: Command<OpenPullRequest, ()> =
        Command::new("smelt.git/open-pull-request");

    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    pub(crate) struct OpenPullRequest {
        #[serde(with = "machine_json")]
        pub machine: RuntimeKey,
        /// 仓库根目录：工作树里就是那个仓库的 worktree 目录。
        pub root: String,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_plugin_api::RuntimeKey;

    #[test]
    fn agent_settings_parse_from_the_providers_shape() {
        use smelt_plugin_api::model::agent_host::AgentHostSettings;

        let ready: agents::MachineAgentSettings = serde_json::from_value(serde_json::json!({
            "machine": "box",
            "state": "ready",
            "settings": AgentHostSettings::fresh(),
        }))
        .unwrap();
        assert_eq!(ready.machine, RuntimeKey::Remote("box".into()));
        assert!(matches!(
            ready.state,
            agents::AgentSettingsState::Ready { .. }
        ));

        let unavailable: agents::MachineAgentSettings = serde_json::from_value(
            serde_json::json!({ "machine": null, "state": "unavailable", "reason": "运行时太旧" }),
        )
        .unwrap();
        assert_eq!(unavailable.machine, RuntimeKey::Local);
        assert!(matches!(
            unavailable.state,
            agents::AgentSettingsState::Unavailable { reason } if reason == "运行时太旧"
        ));
    }
}
