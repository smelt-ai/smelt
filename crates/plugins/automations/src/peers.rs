//! 自动化用到的别的插件的约定：id 和字段照抄自权威定义，形状必须和那边一致。只抄用到的
//! 字段：发出去的参数里没抄的字段对方按默认值处理，收到的里没抄的忽略。

/// 对话（来源 `smelt.conversation`）的约定，权威定义在
/// `crates/plugins/conversation/src/contract.rs`。
pub(crate) mod conversation {
    use serde::Serialize;
    use smelt_plugin_api::model::conversation::ConversationBinding;
    use smelt_plugin_api::{RuntimeKey, machine_json};

    // 约定：smelt.conversation 的 VIEW。
    /// 一段对话，按实例建（`smelt_plugin_api::create_view`），参数是 [`ConversationArgs`]。
    /// 视图还没登记上时 `create_view` 当场报错，等 `HostEvent::ViewsChanged` 再建。
    pub(crate) const VIEW: &str = "smelt.conversation/conversation";

    /// [`VIEW`] 的参数。运行的会话不在工作区目录里，靠 `spec` 接；不抄 `start`（新拉起
    /// 目录里的对话才用）。
    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    pub(crate) struct ConversationArgs {
        #[serde(with = "machine_json")]
        pub machine: RuntimeKey,
        /// 守护会话 id。
        pub session_id: String,
        /// 不在工作区目录里的对话怎么接。
        pub spec: Option<ConversationSpec>,
        /// 接上之前占位处显示的话。
        pub placeholder: Option<String>,
    }

    /// 目录里没有的对话，接上要知道的。
    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    pub(crate) struct ConversationSpec {
        /// 跑这段对话的 agent（`ConversationAgentKind::id`）。
        pub agent: String,
        /// 那台机器上的工作目录。
        pub cwd: String,
        /// 这段对话是谁起的，对话里据此显示来历、决定输入怎么走。
        pub binding: ConversationBinding,
    }
}

/// 智能体（来源 `smelt.agents`）的约定，权威定义在 `crates/plugins/agents/src/contract.rs`。
pub(crate) mod agents {
    use serde::Deserialize;
    use smelt_plugin_api::model::agent_host::AgentHostSettings;
    use smelt_plugin_api::{Command, Event, RuntimeKey, machine_json};

    // 约定：smelt.agents 的 VIEW。
    /// 智能体页，参数 `{"machine": …}` 停在那台机器上。没装就不给去那里的入口。
    pub(crate) const VIEW: &str = "smelt.agents/main";

    // 约定：smelt.agents 的 AGENT_SETTINGS。
    /// 各台机器眼下的智能体配置，只含收到过推送的机器。之后的变化订
    /// [`AGENT_SETTINGS_CHANGED`]。
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
        pub(crate) machine: RuntimeKey,
        #[serde(flatten)]
        pub(crate) state: AgentSettingsState,
    }

    #[derive(Clone, Debug, Deserialize)]
    #[serde(tag = "state", rename_all = "snake_case")]
    pub(crate) enum AgentSettingsState {
        Ready {
            settings: AgentHostSettings,
        },
        /// 那台机器给不了，原因这里用不上。
        Unavailable {},
        /// 机器移除了。
        Absent,
    }
}
