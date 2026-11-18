//! 对话（来源 `smelt.conversation`）对外的约定：每段对话是一个视图实例，接着守护里的一段
//! 智能体对话。对话的显示、输入、权限确认、重连都归它。
//!
//! 这里是权威定义。插件之间不互相依赖，插件接口也不认识具体功能：调用方各自照抄用到的
//! id 和字段，注明出处（JS 插件按同样的字符串和 JSON 调）。改这里就是改线上形状，同一次
//! 改动里把照抄的各处一起改掉（`grep -rn 'smelt.conversation/' crates plugins`）。

use serde::{Deserialize, Serialize};
use smelt_plugin_api::model::acp_conn::AcpForkCut;
use smelt_plugin_api::model::conversation::ConversationBinding;
use smelt_plugin_api::{Command, Event, RuntimeKey, machine_json};

/// 一段对话，按实例建（`smelt_plugin_api::create_view`），参数是 [`ConversationArgs`]。
pub(crate) const VIEW: &str = "smelt.conversation/conversation";

/// 把文字填进那段对话的输入框，不替用户发出。那段对话没有开着是失败。
pub(crate) const INSERT_TEXT: Command<ConversationText, ()> =
    Command::new("smelt.conversation/insert-text");

/// 一段对话的「翻在最新处」变了：往上翻着看历史时是 `false`。实例建好时在最新处，之后
/// 每变一次发一次。摆对话的一方据此报用户眼下是不是在看最新的
/// （`smelt_plugin_api::ActiveContext`）。
pub(crate) const AT_LATEST_CHANGED: Event<AtLatest> =
    Event::new("smelt.conversation/at-latest-changed");

/// 消息上的右键菜单。别的插件往这个 id 登记项（`smelt_plugin_api::register_context_menu_items`），
/// 排在自带的「复制」「分叉」后面；点了执行的命令参数里并进 [`MessageMenuContext`] 的字段。
pub(crate) const MESSAGE_MENU: &str = "smelt.conversation/message";

/// [`MESSAGE_MENU`] 的上下文：右键的是哪段对话里的哪条消息。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MessageMenuContext {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 守护会话 id。
    pub session_id: String,
    pub role: MessageRole,
    /// 整条消息的文字（Markdown 原文）。
    pub text: String,
    /// 右键时在对话里选中的文字；没选中是 `None`。
    pub selection: Option<String>,
}

/// 消息是谁说的。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MessageRole {
    User,
    Assistant,
}

/// [`VIEW`] 的参数。
///
/// 建实例就是有人要看它：工作区目录里的对话按目录里那条接上（守护里还活着就是廉价的
/// attach，不在了按目录续上），只想列出对话不要建实例。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConversationArgs {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 守护会话 id。
    pub session_id: String,
    /// 不在工作区目录里的对话（如自动化运行的现场）怎么接。`None` 按 `session_id` 读那台
    /// 机器的工作区目录；目录里还没有这一条就等它推到。
    #[serde(default)]
    pub spec: Option<ConversationSpec>,
    /// 接上之前占位处显示的话，如「正在打开运行现场…」。
    #[serde(default)]
    pub placeholder: Option<String>,
    /// 调用方刚把这段对话写进目录（新对话、分叉）：一建好就按目录里那条拉起，不当成要续的
    /// 旧对话。和 `spec` 不一起给。
    #[serde(default)]
    pub start: Option<ConversationStart>,
}

/// 新拉起一段对话，见 [`ConversationArgs::start`]。启动规格、配置、首条消息都在目录里那条。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConversationStart {
    /// 从另一段对话分叉（Pi 原生 `--fork`）：复制源会话再接着聊。
    #[serde(default)]
    pub fork: Option<ConversationFork>,
    /// 预填进输入框、不发出的话（从较早的一问分叉时，那一问留给用户改了重发）。
    #[serde(default)]
    pub prefill: String,
}

/// 分叉的来源。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConversationFork {
    /// 源会话在 agent 那边的会话 id。
    pub provider_session_id: String,
    /// 副本切到哪一问之前；`None` 整份照抄。
    #[serde(default)]
    pub cut: Option<AcpForkCut>,
}

/// 目录里没有的对话，接上要知道的。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConversationSpec {
    /// 跑这段对话的 agent（`ConversationAgentKind::id`）。
    pub agent: String,
    /// 那台机器上的工作目录。
    pub cwd: String,
    /// 这段对话是谁起的（自动化运行等），对话里据此显示来历、决定输入怎么走。
    #[serde(default)]
    pub binding: ConversationBinding,
}

/// [`INSERT_TEXT`] 的参数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConversationText {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 那段对话的守护会话 id。
    pub session_id: String,
    pub text: String,
}

/// [`AT_LATEST_CHANGED`] 的载荷。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AtLatest {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 那段对话的守护会话 id。
    pub session_id: String,
    pub at_latest: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_without_spec_read_the_directory() {
        let args: ConversationArgs =
            serde_json::from_value(serde_json::json!({ "machine": null, "session_id": "c" }))
                .unwrap();
        assert_eq!(args.machine, RuntimeKey::Local);
        assert_eq!(
            (args.spec, args.placeholder, args.start),
            (None, None, None)
        );
    }

    #[test]
    fn start_carries_fork_and_prefill() {
        let args: ConversationArgs = serde_json::from_value(serde_json::json!({
            "machine": "box",
            "session_id": "c",
            "start": {
                "fork": { "provider_session_id": "p", "cut": { "text": "hi", "occurrence": 1 } },
                "prefill": "hi",
            },
        }))
        .unwrap();
        let start = args.start.unwrap();
        assert_eq!(start.prefill, "hi");
        let fork = start.fork.unwrap();
        assert_eq!(fork.provider_session_id, "p");
        assert_eq!(fork.cut.map(|cut| cut.occurrence), Some(1));
        let plain: ConversationStart = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(plain, ConversationStart::default());
    }

    #[test]
    fn at_latest_round_trips_through_plugin_events() {
        let payload = AtLatest {
            machine: RuntimeKey::Remote("box".into()),
            session_id: "c".into(),
            at_latest: false,
        };
        let event = smelt_plugin_api::HostEvent::Plugin {
            name: AT_LATEST_CHANGED.id().into(),
            payload: serde_json::to_value(&payload).unwrap(),
        };
        assert_eq!(AT_LATEST_CHANGED.parse(&event), Some(payload));
    }

    /// 菜单上下文是 JS 插件照着读的线上形状。
    #[test]
    fn message_menu_context_shape() {
        let context = MessageMenuContext {
            machine: RuntimeKey::Local,
            session_id: "s".into(),
            role: MessageRole::Assistant,
            text: "全文".into(),
            selection: None,
        };
        assert_eq!(
            serde_json::to_value(&context).unwrap(),
            serde_json::json!({
                "machine": null,
                "session_id": "s",
                "role": "assistant",
                "text": "全文",
                "selection": null,
            })
        );
    }
}
