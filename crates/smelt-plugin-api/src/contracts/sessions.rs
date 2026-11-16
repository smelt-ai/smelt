//! 会话（来源 `smelt.sessions`）：会话列表、会话视图，开会话、跳到会话、往会话里递话的
//! 命令（JS 插件用 `run` 传同样形状的对象），和会话列表里右键菜单的 id。会话现在还归宿主
//! 管，这些由宿主登记。
//!
//! 启动命令、环境变量、初始配置都由提供方按那台机器的智能体配置现算，参数里只说开什么。

use serde::{Deserialize, Serialize};

use crate::{Command, RuntimeKey, machine_json};

/// 会话列表的视图 id。
pub const LIST_VIEW: &str = "smelt.sessions/list";
/// 会话视图的 id：选中的那段会话（对话加上它的终端分屏），工作台出口的默认内容。
pub const SESSION_VIEW: &str = "smelt.sessions/session";

/// 会话列表里项目行的右键菜单（见 [`crate::register_context_menu_items`]）。点了交给命令的
/// 上下文是 [`ProjectMenuContext`]。
pub const PROJECT_MENU: &str = "smelt.sessions/project";
/// 会话列表里机器行的右键菜单。点了交给命令的上下文是 [`MachineMenuContext`]。
pub const MACHINE_MENU: &str = "smelt.sessions/machine";

/// [`PROJECT_MENU`] 的上下文：右键的是哪个项目。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectMenuContext {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 项目根目录，按那台机器的路径写。
    pub path: String,
}

/// [`MACHINE_MENU`] 的上下文：右键的是哪台机器。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineMenuContext {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
}

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

/// 从一段开着的对话分叉出一段新对话：同一台机器、同一个 agent 和工作目录，启动命令、配置、
/// 绑定照源对话（工作区目录里的那条），历史复制自 `provider_session_id`，按 `cut` 切。
/// 新对话露在主窗口里。agent 不支持分叉、源对话不在了都算失败。
pub const FORK: Command<Fork, ()> = Command::new("smelt.sessions/fork");

/// 跟一个智能体开一段新对话：开在那台机器上、`cwd` 里，按智能体定义的引擎、模型和启动
/// 命令起，露在主窗口里。智能体不在了、还不能对话、引擎不开放都算失败。
pub const START_CONVERSATION: Command<StartConversation, ()> =
    Command::new("smelt.sessions/start-conversation");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartConversation {
    /// 智能体定义所在的机器，对话也开在那台机器上。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 智能体定义的 id（[`crate::Host::agent_settings`] 里的 `agents`）。
    pub agent_definition_id: String,
    /// 那台机器上的工作目录。智能体自己的工作区用 `agent.space.ensure` 取。
    pub cwd: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activate {
    /// 会话在哪台机器上。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 守护会话 id，同 [`crate::Host::session_states`] 里的。
    pub session_id: String,
}

/// 分叉一段对话，见 [`FORK`]。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fork {
    /// 源对话在哪台机器上，新对话也开在那里（agent 的会话文件在那台机器上）。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 源对话的守护会话 id，新对话记它作来历。
    pub session_id: String,
    /// 源对话在 agent 那边的会话 id：新对话复制它的历史（Pi 的 `--fork`）。
    pub provider_session_id: String,
    /// 副本切到哪条用户消息之前（不含它）；`None` 是整份。
    #[serde(default)]
    pub cut: Option<ForkCut>,
    /// 预填进新对话输入框的话，不替用户发出；空的是不预填。
    #[serde(default)]
    pub prompt: String,
}

/// 分叉副本的切点：第 `occurrence` 条（从 0 数）原文是 `text` 的用户消息。
pub use crate::model::acp_conn::AcpForkCut as ForkCut;

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
    fn activate_and_fork_carry_the_machine() {
        let activate = Activate {
            machine: RuntimeKey::Remote("box".into()),
            session_id: "s".into(),
        };
        let value = serde_json::to_value(&activate).unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "machine": "box", "session_id": "s" })
        );
        assert_eq!(serde_json::from_value::<Activate>(value).unwrap(), activate);

        let fork: Fork = serde_json::from_value(serde_json::json!({
            "machine": null, "session_id": "s", "provider_session_id": "p",
        }))
        .unwrap();
        assert_eq!(fork.machine, RuntimeKey::Local);
        assert_eq!((fork.cut, fork.prompt.as_str()), (None, ""));
    }

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
