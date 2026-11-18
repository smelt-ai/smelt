//! 调用的会话（来源 `smelt.sessions`）的约定：恢复以后跳到那段会话。照抄自会话插件的
//! 权威定义 `crates/plugins/sessions/src/contract.rs`，id 和字段必须和那边一致。

use serde::Serialize;
use smelt_plugin_api::model::workspace_directory::WorkspaceRevision;
use smelt_plugin_api::{Command, RuntimeKey, machine_json};

// 约定：smelt.sessions 的 ACTIVATE，权威定义在 crates/plugins/sessions/src/contract.rs。
/// 在主窗口里露出这个会话，把主窗口拿到前面来。带上恢复那次回的目录版本（`after`），目录
/// 推到那一版之前会话插件等着，不会因为目录还没推到就找不到。
pub(crate) const ACTIVATE: Command<Activate, ()> = Command::new("smelt.sessions/activate");

#[derive(Serialize)]
pub(crate) struct Activate {
    /// 会话在哪台机器上。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 守护会话 id，同 `smelt_plugin_api::Host::session_states` 里的。
    pub session_id: String,
    /// 刚写进目录时那次调用回的目录版本。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<WorkspaceRevision>,
}
