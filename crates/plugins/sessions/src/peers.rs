//! 会话用到的别的插件（摆的格子、智能体、会话列表）的约定：本插件用到的 id 和字段，
//! 照抄自权威定义，形状必须和那边一致。只抄用到的字段：发出去的参数里没抄的字段对方按默认值
//! 处理。

/// 终端（来源 `smelt.terminal`）的约定，权威定义在 `crates/plugins/terminal/src/contract.rs`。
pub(crate) mod terminal {
    use serde::Serialize;
    use smelt_plugin_api::{Command, RuntimeKey, machine_json};

    // 约定：smelt.terminal 的 VIEW。
    /// 一格终端，按实例建，参数是 [`TerminalArgs`]。视图还没登记上时 `create_view` 当场
    /// 报错，等 `HostEvent::ViewsChanged` 再建。
    pub(crate) const VIEW: &str = "smelt.terminal/terminal";

    // 约定：smelt.terminal 的 SEND_TEXT。
    /// 把文字打进那格终端的输入，等于粘贴，不替用户回车。
    pub(crate) const SEND_TEXT: Command<TerminalText, ()> =
        Command::new("smelt.terminal/send-text");

    /// [`VIEW`] 的参数。
    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    pub(crate) struct TerminalArgs {
        #[serde(with = "machine_json")]
        pub machine: RuntimeKey,
        /// 守护会话 id，由这边先分好。守护里有就接上，没有就按 `cwd`、`launch` 新起。
        pub session_id: String,
        /// 新起时的工作目录；`None` 是那台机器的默认目录。
        pub cwd: Option<String>,
        /// 新起时先跑的命令行；`None` 是登录 shell。
        pub launch: Option<String>,
    }

    /// [`SEND_TEXT`] 的参数。
    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    pub(crate) struct TerminalText {
        #[serde(with = "machine_json")]
        pub machine: RuntimeKey,
        pub session_id: String,
        pub text: String,
    }
}

/// 对话（来源 `smelt.conversation`）的约定，权威定义在
/// `crates/plugins/conversation/src/contract.rs`。
pub(crate) mod conversation {
    use serde::{Deserialize, Serialize};
    use smelt_plugin_api::model::acp_conn::AcpForkCut;
    use smelt_plugin_api::{Command, Event, RuntimeKey, machine_json};

    // 约定：smelt.conversation 的 VIEW。
    /// 一段对话，按实例建，参数是 [`ConversationArgs`]。
    pub(crate) const VIEW: &str = "smelt.conversation/conversation";

    // 约定：smelt.conversation 的 INSERT_TEXT。
    /// 把文字填进那段对话的输入框，不替用户发出。
    pub(crate) const INSERT_TEXT: Command<ConversationText, ()> =
        Command::new("smelt.conversation/insert-text");

    // 约定：smelt.conversation 的 AT_LATEST_CHANGED。
    /// 一段对话的「翻在最新处」变了。实例建好时在最新处，之后每变一次发一次。
    pub(crate) const AT_LATEST_CHANGED: Event<AtLatest> =
        Event::new("smelt.conversation/at-latest-changed");

    /// [`VIEW`] 的参数。会话里的对话都在工作区目录里，按 `session_id` 读目录接上；不抄
    /// `spec`（目录外的对话才用）。
    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    pub(crate) struct ConversationArgs {
        #[serde(with = "machine_json")]
        pub machine: RuntimeKey,
        /// 守护会话 id（目录里那条的 `acp.sid`）。
        pub session_id: String,
        /// 接上之前占位处显示的话。
        pub placeholder: Option<String>,
        /// 刚写进目录的新对话、分叉：一建好就拉起，不当成要续的旧对话。
        pub start: Option<ConversationStart>,
    }

    /// 新拉起一段对话，见 [`ConversationArgs::start`]。
    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    pub(crate) struct ConversationStart {
        /// 从另一段对话分叉。
        pub fork: Option<ConversationFork>,
        /// 预填进输入框、不发出的话。
        pub prefill: String,
    }

    /// 分叉的来源。
    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    pub(crate) struct ConversationFork {
        /// 源会话在 agent 那边的会话 id。
        pub provider_session_id: String,
        /// 副本切到哪一问之前；`None` 整份照抄。
        pub cut: Option<AcpForkCut>,
    }

    /// [`INSERT_TEXT`] 的参数。
    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    pub(crate) struct ConversationText {
        #[serde(with = "machine_json")]
        pub machine: RuntimeKey,
        pub session_id: String,
        pub text: String,
    }

    /// [`AT_LATEST_CHANGED`] 的载荷。
    #[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
    pub(crate) struct AtLatest {
        #[serde(with = "machine_json")]
        pub machine: RuntimeKey,
        pub session_id: String,
        pub at_latest: bool,
    }
}

/// 智能体（来源 `smelt.agents`）的约定，权威定义在 `crates/plugins/agents/src/contract.rs`。
/// 开对话的启动命令、环境变量、模型都按那台机器的这份配置现算。
pub(crate) mod agents {
    use serde::Deserialize;
    use smelt_plugin_api::model::agent_host::AgentHostSettings;
    use smelt_plugin_api::{Command, RuntimeKey, machine_json};

    // 约定：smelt.agents 的 AGENT_SETTINGS。
    /// 各台机器眼下的智能体配置，只含收到过推送的机器。
    pub(crate) const AGENT_SETTINGS: Command<(), Vec<MachineAgentSettings>> =
        Command::new("smelt.agents/agent-settings");

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

/// 会话列表（来源 `smelt.session-list`）的约定，权威定义在
/// `plugins/session-list/contract.js`。
pub(crate) mod session_list {
    use serde::Serialize;
    use smelt_plugin_api::{Command, RuntimeKey, machine_json};

    // 约定：smelt.session-list 的 REVEAL_IN_LIST。
    /// 在会话列表里找到这段会话：展开、滚过去、闪一下。不在列表里是失败。
    pub(crate) const REVEAL_IN_LIST: Command<RevealInList, ()> =
        Command::new("smelt.session-list/reveal-in-list");

    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    pub(crate) struct RevealInList {
        #[serde(with = "machine_json")]
        pub machine: RuntimeKey,
        /// 守护会话 id。
        pub session_id: String,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_plugin_api::RuntimeKey;

    #[test]
    fn args_serialize_to_the_providers_shape() {
        let terminal = terminal::TerminalArgs {
            machine: RuntimeKey::Remote("box".into()),
            session_id: "t".into(),
            cwd: Some("/repo".into()),
            launch: None,
        };
        assert_eq!(
            serde_json::to_value(&terminal).unwrap(),
            serde_json::json!({
                "machine": "box", "session_id": "t", "cwd": "/repo", "launch": null,
            })
        );

        let conversation = conversation::ConversationArgs {
            machine: RuntimeKey::Local,
            session_id: "c".into(),
            placeholder: None,
            start: Some(conversation::ConversationStart {
                fork: Some(conversation::ConversationFork {
                    provider_session_id: "p".into(),
                    cut: None,
                }),
                prefill: "hi".into(),
            }),
        };
        assert_eq!(
            serde_json::to_value(&conversation).unwrap(),
            serde_json::json!({
                "machine": null,
                "session_id": "c",
                "placeholder": null,
                "start": {
                    "fork": { "provider_session_id": "p", "cut": null },
                    "prefill": "hi",
                },
            })
        );
    }

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

    #[test]
    fn at_latest_parses_from_the_plugin_event() {
        let event = smelt_plugin_api::HostEvent::Plugin {
            name: conversation::AT_LATEST_CHANGED.id().into(),
            payload: serde_json::json!({ "machine": "box", "session_id": "c", "at_latest": false }),
        };
        let parsed = conversation::AT_LATEST_CHANGED.parse(&event).unwrap();
        assert_eq!(parsed.machine, RuntimeKey::Remote("box".into()));
        assert!(!parsed.at_latest);
    }
}
