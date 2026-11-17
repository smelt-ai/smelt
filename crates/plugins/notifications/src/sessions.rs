//! 调用的会话（来源 `smelt.sessions`）的约定：本插件用到的 id 和字段，照抄自权威定义
//! （`crates/plugins/sessions/src/contract.rs`），形状必须和那边一致。

use serde::Serialize;
use smelt_plugin_api::{Command, RuntimeKey, machine_json};

// 约定：smelt.sessions 的 ACTIVATE，权威定义在 crates/plugins/sessions/src/contract.rs。
/// 在主窗口里露出这个会话，把主窗口拿到前面来。通知和菜单栏点一条就执行它。
pub(crate) const ACTIVATE: Command<Activate, ()> = Command::new("smelt.sessions/activate");

/// [`ACTIVATE`] 的参数：哪台机器上的哪段守护会话。
#[derive(Serialize)]
pub(crate) struct Activate {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    pub session_id: String,
}
