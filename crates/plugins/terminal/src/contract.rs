//! 终端（来源 `smelt.terminal`）对外的约定：每格终端是一个视图实例，接着守护里的一段终端
//! 会话。标题、状态、目录、分支、退出都从守护推来的会话状态读
//! （`smelt_plugin_api::Host::session_states`），这里不发事件。结束终端是运行时的操作，由
//! 摆终端的一方发，不归终端。
//!
//! 这里是权威定义。插件之间不互相依赖，插件接口也不认识具体功能：调用方各自照抄用到的
//! id 和字段，注明出处（JS 插件按同样的字符串和 JSON 调）。改这里就是改线上形状，同一次
//! 改动里把照抄的各处一起改掉（`grep -rn 'smelt.terminal/' crates plugins`）。
//!
//! 视图还没登记上时（插件没装、登记顺序在后）`create_view` 当场报「没有视图」，摆终端的
//! 一方等 `HostEvent::ViewsChanged` 再建。

use serde::{Deserialize, Serialize};
use smelt_plugin_api::{Command, RuntimeKey, machine_json};

/// 一格终端，按实例建（`smelt_plugin_api::create_view`），参数是 [`TerminalArgs`]。
pub(crate) const VIEW: &str = "smelt.terminal/terminal";

/// 把文字打进那格终端的输入，等于粘贴，不替用户回车。那格终端没有开着是失败。
pub(crate) const SEND_TEXT: Command<TerminalText, ()> = Command::new("smelt.terminal/send-text");

/// 终端的右键菜单。别的插件往这个 id 登记项（`smelt_plugin_api::register_context_menu_items`），
/// 排在自带的「复制」「粘贴」后面；点了执行的命令参数里并进 [`TerminalMenuContext`] 的字段。
/// 应用开了鼠标上报（全屏 TUI）时右键交给应用，按住 Shift 才出菜单。
pub(crate) const MENU: &str = "smelt.terminal/menu";

/// [`MENU`] 的上下文：右键的是哪格终端。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TerminalMenuContext {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 那格终端的守护会话 id。
    pub session_id: String,
    /// 右键时选中的文字；没选中是 `None`。
    pub selection: Option<String>,
}

/// [`VIEW`] 的参数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TerminalArgs {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 守护会话 id，由摆它的一方先分好。守护里有这段会话就接上，没有就按 `cwd`、`launch`
    /// 新起。
    pub session_id: String,
    /// 新起时的工作目录，按那台机器的路径写；`None` 是那台机器的默认目录。
    #[serde(default)]
    pub cwd: Option<String>,
    /// 新起时先跑的命令行，如 `claude --resume abc`；`None` 是那台机器的登录 shell。接上已有
    /// 的会话时不用。给人看的名字、侧栏图标归摆它的一方，不在这里。
    #[serde(default)]
    pub launch: Option<String>,
}

/// [`SEND_TEXT`] 的参数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TerminalText {
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

    /// 菜单上下文是 JS 插件照着读的线上形状。
    #[test]
    fn menu_context_shape() {
        let context = TerminalMenuContext {
            machine: RuntimeKey::Remote("box".into()),
            session_id: "t".into(),
            selection: Some("ls".into()),
        };
        assert_eq!(
            serde_json::to_value(&context).unwrap(),
            serde_json::json!({ "machine": "box", "session_id": "t", "selection": "ls" })
        );
    }
}
