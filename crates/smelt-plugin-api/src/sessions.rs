//! 会话的命令：会话归宿主管，插件要开一段会话、往会话里递话都执行这里的命令（JS 插件用
//! `run` 传同样形状的对象）。
//!
//! 启动命令、环境变量、初始配置都由宿主按那台机器的智能体配置现算，参数里只说开什么。

use serde::{Deserialize, Serialize};

use crate::{Command, RuntimeKey, machine_json};

/// 用那家 agent 自己的续接（ACP `session/load`）接着一段历史会话；同一段已经开着就
/// 切过去。
pub const RESUME: Command<Resume, ()> = Command::new("smelt.sessions/resume");
/// 在终端里起那家 agent 的 CLI 恢复同一段历史会话（`--resume` 之类）。不看
/// `agent_definition_id`。
pub const RESUME_IN_TERMINAL: Command<Resume, ()> =
    Command::new("smelt.sessions/resume-in-terminal");
/// 起一段新会话，第一句话是交接说明（把别家的对话重述给它）。
pub const HANDOFF: Command<Handoff, ()> = Command::new("smelt.sessions/handoff");
/// 把文字交给当前会话：终端会话打进输入，对话填进输入框，都不替用户发出。没有能接的
/// 会话时失败。
pub const SEND_TEXT: Command<SendText, ()> = Command::new("smelt.sessions/send-text");

/// 在主窗口里露出这个会话（终端是那一格），把主窗口拿到前面来；主窗口关着就先开。会话还在
/// 恢复的话等恢复完再跳，恢复完还没有就算了。
pub const ACTIVATE: Command<Activate, ()> = Command::new("smelt.sessions/activate");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activate {
    /// 守护会话 id，同 [`crate::Host::session_states`] 里的。
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendText {
    pub text: String,
}

/// 续接一段历史会话。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resume {
    /// 存档在哪台机器上，会话也开在那台机器上。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 历史来源的 id（`HistorySourceKind::id`）。
    pub agent: String,
    /// 自定义 Agent 的 id；基础 agent 是 `None`。启动命令和环境变量按它取。
    pub profile_id: Option<String>,
    /// 那台机器上的工作目录。
    pub cwd: String,
    /// agent 自己认的会话 id。
    pub resume_id: String,
    /// 这段会话属于哪个智能体定义（运行时随历史列表给的）。有的话按那个智能体续接。
    #[serde(default)]
    pub agent_definition_id: Option<String>,
}

/// 把一段对话交给另一家 agent。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    /// 新会话开在哪台机器上。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    pub cwd: String,
    /// 目标 agent（`ConversationAgentKind::id`）。
    pub agent: String,
    /// 目标是自定义 Agent 时它的 id。
    pub profile_id: Option<String>,
    /// 新会话的第一句话。
    pub prompt: String,
    /// 源会话的 id，只作溯源记进新会话，不发给目标 agent。
    pub source_session_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_args_round_trip_and_tolerate_missing_definition() {
        let args = Resume {
            machine: RuntimeKey::Remote("box".into()),
            agent: "claude".into(),
            profile_id: None,
            cwd: "/repo".into(),
            resume_id: "abc".into(),
            agent_definition_id: Some("writer".into()),
        };
        let value = serde_json::to_value(&args).unwrap();
        assert_eq!(serde_json::from_value::<Resume>(value).unwrap(), args);

        let bare = serde_json::json!({
            "machine": null, "agent": "codex", "profile_id": null,
            "cwd": "/repo", "resume_id": "x",
        });
        let parsed: Resume = serde_json::from_value(bare).unwrap();
        assert_eq!(parsed.agent_definition_id, None);
        assert_eq!(parsed.machine, RuntimeKey::Local);
    }
}
