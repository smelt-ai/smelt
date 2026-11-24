//! 调用的会话（来源 `smelt.sessions`）的约定：本模块用到的 id 和字段，与权威定义
//! （`plugins/sessions/contract.js`）保持一致。

use serde::Serialize;
use smelt_plugin_api::{Command, RuntimeKey, machine_json};

// 约定：smelt.sessions 的 ACTIVATE，权威定义在 plugins/sessions/contract.js。
/// 在主窗口里露出这个会话，把主窗口拿到前面来。通知和菜单栏点一条就执行它。
pub(super) const ACTIVATE: Command<Activate, ()> = Command::new("smelt.sessions/activate");

/// [`ACTIVATE`] 的参数：哪台机器上的哪段守护会话。
#[derive(Serialize)]
pub(super) struct Activate {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    pub session_id: String,
}
