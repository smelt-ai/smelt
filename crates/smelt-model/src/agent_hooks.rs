//! agent hooks 接入状态。检测、安装在运行时。

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookProviderStatus {
    pub label: String,
    pub installed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHooksStatus {
    pub providers: Vec<HookProviderStatus>,
    /// hooks 调用的 `smelt-notify` 在运行时那台机器上的路径。
    pub notify_path: String,
}
