//! 设置页「Agent hooks」：状态、安装、卸载都在运行时执行（`agent.hooks.*`），各家
//! agent 的配置文件在运行时那台机器上。这里只缓存最近一次结果给 render 读。

use gpui::{App, Global};
use smelt_core::agent_hooks::AgentHooksStatus;
use smelt_core::control_api::{AgentHooksInstall, AgentHooksUninstall};
use smelt_runtime_client::protocol::control::{ControlCall, EmptyParams};

use crate::runtime_call::{RUNTIME_CALL_TIMEOUT, call, error_text};

#[derive(Default)]
pub struct AgentHooksCache {
    /// `None`：还没从运行时取到。
    pub status: Option<AgentHooksStatus>,
    /// 安装 / 卸载进行中。
    pub busy: bool,
}

impl Global for AgentHooksCache {}

/// 重新取一次状态。打开设置窗口时调；外部手改了配置，重开设置就能看到。
pub fn refresh_agent_hooks_status(cx: &mut App) {
    run::<smelt_core::control_api::AgentHooksStatus>(None, cx);
}

pub fn install_agent_hooks(cx: &mut App) {
    run::<AgentHooksInstall>(Some(("Agent hooks 已安装", "安装失败")), cx);
}

pub fn uninstall_agent_hooks(cx: &mut App) {
    run::<AgentHooksUninstall>(Some(("Smelt hooks 已移除", "移除失败")), cx);
}

/// 启动时按用户的开关校正一次：新版本可能改了 hook 条目，配置也可能被别的工具改坏。
/// 不弹提示，失败只记日志。
pub fn reconcile_agent_hooks(cx: &mut App) {
    cx.background_executor()
        .spawn(async {
            if let Err(error) =
                call::<AgentHooksInstall>(EmptyParams::default(), RUNTIME_CALL_TIMEOUT)
            {
                eprintln!(
                    "[workspace] 自动安装 Agent hooks 失败：{}",
                    error_text(error)
                );
            }
        })
        .detach();
}

fn run<C>(notice: Option<(&'static str, &'static str)>, cx: &mut App)
where
    C: ControlCall<Params = EmptyParams, Result = AgentHooksStatus> + 'static,
{
    if notice.is_some() {
        cx.default_global::<AgentHooksCache>().busy = true;
        cx.refresh_windows();
    }
    let task = cx
        .background_executor()
        .spawn(async { call::<C>(EmptyParams::default(), RUNTIME_CALL_TIMEOUT) });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            let failed = result.is_err();
            let cache = cx.default_global::<AgentHooksCache>();
            if notice.is_some() {
                cache.busy = false;
            }
            match result {
                Ok(status) => {
                    cache.status = Some(status);
                    if let Some((done, _)) = notice {
                        crate::status_item::notify_success(done);
                    }
                }
                Err(error) => {
                    let text = error_text(error);
                    eprintln!("[workspace] agent hooks：{text}");
                    if let Some((_, failed_prefix)) = notice {
                        crate::status_item::notify_error(format!("{failed_prefix}：{text}"));
                    }
                }
            }
            // 装 / 卸到一半失败时有的 provider 已经改了，重新取一次真实状态。
            if failed && notice.is_some() {
                refresh_agent_hooks_status(cx);
            }
            cx.refresh_windows();
        });
    })
    .detach();
}
