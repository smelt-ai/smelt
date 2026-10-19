//! GUI 调运行时控制方法的统一入口。目前只连本机运行时。
//!
//! 必须在后台线程调用：一次调用就是一次 socket 往返，运行时那边可能还要跑 git、
//! 遍历目录。

use std::time::Duration;

use smelt_runtime_client::protocol::control::{ControlCall, ControlClientError, ControlErrorCode};

/// 本机操作的默认上限。大仓库的 status、搜索也该在这个时间里回来。
pub(crate) const RUNTIME_CALL_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) fn call<C: ControlCall>(
    params: C::Params,
    timeout: Duration,
) -> Result<C::Result, ControlClientError> {
    smelt_client::daemon_state::local_runtime().call::<C>(&params, timeout)
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
