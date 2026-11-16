//! 终端（来源 `smelt.terminal`）：每格终端是一个视图实例，接着守护里的一段终端会话。
//!
//! 标题、状态、目录、分支、退出都从守护推来的会话状态读（[`crate::Host::session_states`]），
//! 这里不发事件。结束终端是运行时的操作，由摆终端的一方发，不归终端。

use serde::{Deserialize, Serialize};

use crate::{Command, RuntimeKey, machine_json};

/// 一格终端，按实例建（[`crate::create_view`]），参数是 [`TerminalArgs`]。
pub const VIEW: &str = "smelt.terminal/terminal";

/// 把文字打进那格终端的输入，等于粘贴，不替用户回车。那格终端没有开着是失败。
pub const SEND_TEXT: Command<TerminalText, ()> = Command::new("smelt.terminal/send-text");

/// [`VIEW`] 的参数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalArgs {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 守护会话 id，由摆它的一方先分好。守护里有这段会话就接上，没有就按 `cwd`、`launch`
    /// 新起。
    pub session_id: String,
    /// 新起时的工作目录，按那台机器的路径写；`None` 是那台机器的默认目录。
    #[serde(default)]
    pub cwd: Option<String>,
    /// 新起时跑的命令；`None` 是那台机器的登录 shell。
    #[serde(default)]
    pub launch: Option<TerminalLaunch>,
}

/// 终端里跑的命令。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalLaunch {
    /// 命令行，如 `claude --resume abc`。
    pub cmd: String,
    /// 给人看的名字（如 agent 的名字）；`None` 按命令行猜。
    #[serde(default)]
    pub label: Option<String>,
}

/// [`SEND_TEXT`] 的参数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalText {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 那格终端的守护会话 id。
    pub session_id: String,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_args_attach_or_start_a_shell() {
        let args: TerminalArgs =
            serde_json::from_value(serde_json::json!({ "machine": "box", "session_id": "t" }))
                .unwrap();
        assert_eq!(args.machine, RuntimeKey::Remote("box".into()));
        assert_eq!((args.cwd, args.launch), (None, None));
    }
}
