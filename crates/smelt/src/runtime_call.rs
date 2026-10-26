//! GUI 调运行时控制方法的统一入口。`call` 发给本机运行时；要发给某台机器的，先在主线程
//! 用 [`target`] 按 `RuntimeKey` 取到它的客户端，再到后台 [`call_on`]。
//!
//! 必须在后台线程调用：一次调用就是一次 socket 往返（远程的还要过 iroh），运行时那边
//! 可能还要跑 git、遍历目录。

use std::time::Duration;

use smelt_runtime_client::RuntimeClient;
use smelt_runtime_client::protocol::control::{ControlCall, ControlClientError, ControlErrorCode};

use crate::RuntimeKey;

/// 本机操作的默认上限。大仓库的 status、搜索也该在这个时间里回来。
pub(crate) const RUNTIME_CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// 目标机器已经从这台桌面移除时报的话。
const RUNTIME_REMOVED: &str = "这台机器已经移除";

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
    RUNTIME_REMOVED.to_string()
}

/// 调用要发给的那台机器的客户端。要读桌面挂着哪些机器，只能在主线程取；取到的客户端
/// 带去后台用。机器已经移除时给出可以直接显示的原因。
pub(crate) fn target(key: &RuntimeKey, cx: &gpui::App) -> Result<RuntimeClient, String> {
    crate::runtimes::client(key, cx).ok_or_else(removed_text)
}

/// 给人看的失败原因：运行时报的业务错误原样给出，连不上运行时则是传输错误。
pub(crate) fn error_text(error: ControlClientError) -> String {
    match error {
        // 应用刚更新、守护还没换到新版时会这样：守护要等空闲才升级。
        ControlClientError::Protocol(error) if error.code == ControlErrorCode::MethodNotFound => {
            "正在运行的 smeltd 版本较旧，还不支持这个操作；升级守护进程后重试".to_string()
        }
        ControlClientError::Protocol(error) => error.message,
        other => other.to_string(),
    }
}
