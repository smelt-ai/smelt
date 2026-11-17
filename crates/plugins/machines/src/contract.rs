//! 远程机器（来源 `smelt.machines`）对外的约定：设置「远程」页的视图 id，和去添加一台远程
//! 机器的命令。
//!
//! 这里是权威定义。插件之间不互相依赖，插件接口也不认识具体功能：调用方各自照抄用到的
//! id，注明出处（JS 插件按同样的字符串调）。改这里就是改线上形状，同一次改动里把照抄的
//! 各处一起改掉（`grep -rn 'smelt.machines/' crates plugins`）。
//!
//! 会话列表机器行右键菜单里的几条命令（重新连接、改名这些）只给本插件自己的菜单项用，
//! 不是约定，在 `menu.rs` 里。

use smelt_plugin_api::Command;

/// 设置里的「远程」页：连出去的机器（添加、看状态、移除），和让别的设备连进来。不带参数。
pub(crate) const VIEW: &str = "smelt.machines/remote";

/// 去添加一台远程机器：打开「远程」页，配对串在那里粘。不带参数，命令面板里也有。
///
/// 真正配对是宿主的 `smelt.app/add-machine`（带配对串），这条只是把人带到能粘配对串的地方。
pub(crate) const ADD_MACHINE: Command<(), ()> = Command::new("smelt.machines/add-machine");

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_plugin_api::serde_json::{self, Value};

    /// 键位、菜单、命令面板执行时参数是 `null`，调用方按 `Command<(), ()>` 照抄也发 `null`，
    /// 结果同样是 `null`。
    #[test]
    fn add_machine_is_a_bare_command() {
        assert_eq!(ADD_MACHINE.id(), "smelt.machines/add-machine");
        assert_eq!(ADD_MACHINE.name(), "add-machine");
        assert_eq!(serde_json::to_value(()).unwrap(), Value::Null);
        let () = serde_json::from_value(Value::Null).expect("null 收得成不带参数");
    }

    #[test]
    fn ids_share_the_plugin_source() {
        for id in [VIEW, ADD_MACHINE.id()] {
            assert_eq!(
                id.split_once('/').map(|(owner, _)| owner),
                Some(crate::OWNER)
            );
        }
    }
}
