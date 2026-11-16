//! 对话（来源 `smelt.conversation`）：每段对话是一个视图实例，接着守护里的一段智能体对话。
//! 对话的显示、输入、权限确认、重连都归它。

use serde::{Deserialize, Serialize};

use crate::{Command, Event, RuntimeKey, machine_json};

/// 一段对话，按实例建（[`crate::create_view`]），参数是 [`ConversationArgs`]。
pub const VIEW: &str = "smelt.conversation/conversation";

/// 把文字填进那段对话的输入框，不替用户发出。那段对话没有开着是失败。
pub const INSERT_TEXT: Command<ConversationText, ()> =
    Command::new("smelt.conversation/insert-text");

/// 一段对话的「翻在最新处」变了：往上翻着看历史时是 `false`。实例建好时在最新处，之后
/// 每变一次发一次。摆对话的一方据此报用户眼下是不是在看最新的（[`crate::ActiveContext`]）。
pub const AT_LATEST_CHANGED: Event<AtLatest> = Event::new("smelt.conversation/at-latest-changed");

/// [`VIEW`] 的参数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationArgs {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 守护会话 id。
    pub session_id: String,
    /// 不在工作区目录里的对话（如自动化运行的现场）怎么接。`None` 按 `session_id` 读那台
    /// 机器的工作区目录。
    #[serde(default)]
    pub spec: Option<ConversationSpec>,
    /// 接上之前占位处显示的话，如「正在打开运行现场…」。
    #[serde(default)]
    pub placeholder: Option<String>,
}

/// 目录里没有的对话，接上要知道的。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSpec {
    /// 跑这段对话的 agent（`ConversationAgentKind::id`）。
    pub agent: String,
    /// 那台机器上的工作目录。
    pub cwd: String,
    /// 这段对话是谁起的（自动化运行等），对话里据此显示来历、决定输入怎么走。
    #[serde(default)]
    pub binding: crate::model::conversation::ConversationBinding,
}

/// [`INSERT_TEXT`] 的参数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationText {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 那段对话的守护会话 id。
    pub session_id: String,
    pub text: String,
}

/// [`AT_LATEST_CHANGED`] 的载荷。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtLatest {
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
        assert_eq!((args.spec, args.placeholder), (None, None));
    }

    #[test]
    fn at_latest_round_trips_through_plugin_events() {
        let payload = AtLatest {
            machine: RuntimeKey::Remote("box".into()),
            session_id: "c".into(),
            at_latest: false,
        };
        let event = crate::HostEvent::Plugin {
            name: AT_LATEST_CHANGED.id().into(),
            payload: serde_json::to_value(&payload).unwrap(),
        };
        assert_eq!(AT_LATEST_CHANGED.parse(&event), Some(payload));
    }
}
