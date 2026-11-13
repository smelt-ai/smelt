//! 能力库「MCP」页签背后的操作：改 `~/.pi/agent/mcp.json`。登录和 provider 登录共用一套，
//! 见 `auth.rs` 的 `start_pi_mcp_login`。
//!
//! 都由运行时执行（`pi.mcp.*`），每次修改回带新的 Pi 配置快照；这里只管视图状态。

use super::*;
use api::model::control_api::{PiMcpAddPreset, PiMcpRemove, PiMcpSetEnabled};
use api::model::pi_service::{McpEnabledParams, McpPresetParams, McpServerParams};

impl CapabilitiesPage {
    pub fn add_pi_mcp_preset(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.pi_mcp_error = None;
        self.run_pi_mutation::<PiMcpAddPreset>(
            McpPresetParams { id: id.to_string() },
            cx,
            |this, error, _| this.pi_mcp_error = Some(error),
        );
    }

    pub fn set_pi_mcp_server_enabled(
        &mut self,
        name: String,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        self.pi_mcp_error = None;
        self.run_pi_mutation::<PiMcpSetEnabled>(
            McpEnabledParams { name, enabled },
            cx,
            |this, error, _| this.pi_mcp_error = Some(error),
        );
    }

    pub fn request_pi_mcp_remove(&mut self, name: String, cx: &mut Context<Self>) {
        self.pi_mcp_pending_remove = Some(name);
        cx.notify();
    }

    pub fn cancel_pi_mcp_remove(&mut self, cx: &mut Context<Self>) {
        self.pi_mcp_pending_remove = None;
        cx.notify();
    }

    pub fn confirm_pi_mcp_remove(&mut self, name: String, cx: &mut Context<Self>) {
        self.pi_mcp_pending_remove = None;
        self.pi_mcp_error = None;
        self.run_pi_mutation::<PiMcpRemove>(McpServerParams { name }, cx, |this, error, _| {
            this.pi_mcp_error = Some(error)
        });
        cx.notify();
    }
}
