//! 能力库（来源 `smelt.capabilities`）：模型、Pi 插件、MCP、记忆。

/// 能力库页。打开时的参数都可以不给：
/// `{"tab": "models" | "plugins" | "mcp" | "memory", "machine": …}`。`machine` 不给或已经
/// 移除时停在本机。
pub const VIEW: &str = "smelt.capabilities/main";
