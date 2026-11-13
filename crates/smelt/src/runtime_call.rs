//! GUI 调运行时控制方法的统一入口。`call` 发给本机运行时；要发给某台机器的，先在主线程
//! 用 [`target`] 按 `RuntimeKey` 取到它的客户端，再到后台 [`call_on`]。
//!
//! 必须在后台线程调用：一次调用就是一次 socket 往返（远程的还要过 iroh），运行时那边
//! 可能还要跑 git、遍历目录。

use std::time::Duration;

use smelt_runtime_client::RuntimeClient;
use smelt_runtime_client::protocol::control::{ControlCall, ControlClientError};

use crate::RuntimeKey;

/// 本机操作的默认上限。大仓库的 status、搜索也该在这个时间里回来。
pub(crate) const RUNTIME_CALL_TIMEOUT: Duration = smelt_plugin_api::CALL_TIMEOUT;

/// 给人看的失败原因。宿主和插件说法一致，定义在插件接口里。
pub(crate) use smelt_plugin_api::call_error_text as error_text;

/// 发给本机运行时。
pub(crate) fn call<C: ControlCall>(
    params: C::Params,
    timeout: Duration,
) -> Result<C::Result, ControlClientError> {
    call_on::<C>(&crate::runtimes::local(), params, timeout)
}

/// 发给 [`target`] 取到的那台机器。
pub(crate) fn call_on<C: ControlCall>(
    client: &RuntimeClient,
    params: C::Params,
    timeout: Duration,
) -> Result<C::Result, ControlClientError> {
    client.call::<C>(&params, timeout)
}

/// 机器已经移除时给人看的话。
pub(crate) fn removed_text() -> String {
    smelt_plugin_api::MACHINE_REMOVED.to_string()
}

/// 调用要发给的那台机器的客户端。要读桌面挂着哪些机器，只能在主线程取；取到的客户端
/// 带去后台用。机器已经移除时给出可以直接显示的原因。
pub(crate) fn target(key: &RuntimeKey, cx: &gpui::App) -> Result<RuntimeClient, String> {
    crate::runtimes::client(key, cx).ok_or_else(removed_text)
}
