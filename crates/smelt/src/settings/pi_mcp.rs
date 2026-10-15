//! 能力库「MCP」页签背后的操作：改 `~/.pi/agent/mcp.json`、跑 `pi mcp login`。
//!
//! 文件读写和登录子进程都在 [`smelt_core::pi_mcp`]；这里只把结果折进 `Workspace` 的
//! 视图状态。列表本身不缓存，设置快照每次刷新时重读，所以写完只要 `cx.notify()`。

use super::*;

impl Workspace {
    pub fn add_pi_mcp_preset(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.pi_mcp_error = smelt_core::pi_mcp::add_pi_mcp_preset(id).err();
        cx.notify();
    }

    pub fn set_pi_mcp_server_enabled(
        &mut self,
        name: String,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        self.pi_mcp_error = smelt_core::pi_mcp::set_pi_mcp_server_enabled(&name, enabled).err();
        cx.notify();
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
        self.pi_mcp_error = smelt_core::pi_mcp::remove_pi_mcp_server(&name).err();
        cx.notify();
    }

    /// 后台跑 `pi mcp login <name>`：需要授权时 Pi 自己打开浏览器，等回调写好令牌。
    /// 同一时间只登录一台服务器，结束后重读列表（快照刷新时自然会读到新令牌）。
    pub fn login_pi_mcp_server(&mut self, name: String, cx: &mut Context<Self>) {
        if self.pi_mcp_login.is_some() {
            return;
        }
        self.pi_mcp_error = None;
        self.pi_mcp_login = Some(name.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn({
                    let name = name.clone();
                    async move { smelt_core::pi_mcp::login_pi_mcp_server(&name, &|_| {}) }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.pi_mcp_login = None;
                match outcome {
                    Ok(_) => {
                        crate::status_item::notify_success(format!("MCP 服务器 {name} 已登录"))
                    }
                    Err(error) => this.pi_mcp_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }
}
