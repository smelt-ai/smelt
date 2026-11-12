//! 起会话的命令：会话归宿主管，插件要开一段会话就用 [`crate::run`] 执行这里的命令，参数
//! 是对应结构的 JSON（JS 插件传同样形状的对象）。
//!
//! 启动命令、环境变量、初始配置都由宿主按那台机器的智能体配置现算，参数里只说开什么。

use serde::{Deserialize, Serialize};

use crate::RuntimeKey;

/// 用那家 agent 自己的续接（ACP `session/load`）接着一段历史会话；同一段已经开着就
/// 切过去。参数 [`Resume`]。
pub const RESUME: &str = "smelt.sessions/resume";
/// 在终端里起那家 agent 的 CLI 恢复同一段历史会话（`--resume` 之类）。参数 [`Resume`]，
/// 不看 `agent_definition_id`。
pub const RESUME_IN_TERMINAL: &str = "smelt.sessions/resume-in-terminal";
/// 起一段新会话，第一句话是交接说明（把别家的对话重述给它）。参数 [`Handoff`]。
pub const HANDOFF: &str = "smelt.sessions/handoff";

/// 续接一段历史会话。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resume {
    /// 存档在哪台机器上：本机 `null`，远程是 runtime id。会话也开在那台机器上。
    pub machine: Option<String>,
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
    /// 新会话开在哪台机器上：本机 `null`，远程是 runtime id。
    pub machine: Option<String>,
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

/// 参数里的机器写法 → [`RuntimeKey`]。
pub fn machine_key(machine: &Option<String>) -> RuntimeKey {
    match machine {
        None => RuntimeKey::Local,
        Some(id) => RuntimeKey::Remote(id.clone()),
    }
}

/// [`RuntimeKey`] → 参数里的机器写法。
pub fn machine_arg(machine: &RuntimeKey) -> Option<String> {
    match machine {
        RuntimeKey::Local => None,
        RuntimeKey::Remote(id) => Some(id.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_args_round_trip_and_tolerate_missing_definition() {
        let args = Resume {
            machine: Some("box".into()),
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
        assert_eq!(machine_key(&parsed.machine), RuntimeKey::Local);
    }

    #[test]
    fn machine_arg_round_trips() {
        for machine in [RuntimeKey::Local, RuntimeKey::Remote("box".into())] {
            assert_eq!(machine_key(&machine_arg(&machine)), machine);
        }
    }
}
