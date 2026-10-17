//! 能力库「MCP」页签背后的操作：改 `~/.pi/agent/mcp.json`、跑 `pi mcp login`。
//!
//! 都由运行时执行（`pi.mcp.*`），每次修改回带新的 Pi 配置快照；这里只管视图状态。

use super::*;
use smelt_core::control_api::{PiMcpAddPreset, PiMcpLogin, PiMcpRemove, PiMcpSetEnabled};
use smelt_core::pi_service::{McpEnabledParams, McpPresetParams, McpServerParams};

/// 登录要等用户在浏览器里授权。
const MCP_LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

impl Workspace {
    pub fn add_pi_mcp_preset(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.pi_mcp_error = None;
        self.run_pi_mutation::<PiMcpAddPreset>(
            McpPresetParams { id: id.to_string() },
            false,
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
            false,
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
        self.run_pi_mutation::<PiMcpRemove>(
            McpServerParams { name },
            false,
            cx,
            |this, error, _| this.pi_mcp_error = Some(error),
        );
        cx.notify();
    }

    /// 请运行时跑 `pi mcp login <name>`：需要授权时 Pi 自己打开浏览器，等回调写好令牌。
    /// 同一时间只登录一台服务器。
    pub fn login_pi_mcp_server(&mut self, name: String, cx: &mut Context<Self>) {
        if self.pi_mcp_login.is_some() {
            return;
        }
        self.pi_mcp_error = None;
        self.pi_mcp_login = Some(name.clone());
        cx.notify();
        self.run_pi_call::<PiMcpLogin>(
            McpServerParams { name: name.clone() },
            MCP_LOGIN_TIMEOUT,
            cx,
            move |this, outcome, cx| {
                this.pi_mcp_login = None;
                match outcome {
                    Ok(config) => {
                        this.adopt_pi_config(config, false, cx);
                        crate::status_item::notify_success(format!("MCP 服务器 {name} 已登录"));
                    }
                    Err(error) => this.pi_mcp_error = Some(error),
                }
                cx.notify();
            },
        );
    }
}
