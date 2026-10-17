//! `history.*` 控制方法：各家 agent 的历史存档在运行时那台机器上，扫描、解析、删除
//! 都在这里执行。业务规则在 `smelt_core::history_service`。

use smelt_core::history_service::{self, HistoryError};
use smelt_runtime_client::protocol::control::{
    ControlCall, ControlError, ControlErrorCode, ControlMethod, EmptyParams, decode_params,
};
use smelt_runtime_client::protocol::history::*;

/// `method` 必须是 `history.*` 之一。
pub(crate) fn dispatch(
    method: ControlMethod,
    params: serde_json::Value,
) -> Result<serde_json::Value, ControlError> {
    match method {
        ControlMethod::HistoryList => {
            call::<HistoryListCall>(params, |p| history_service::list(&p))
        }
        ControlMethod::HistoryDetail => call::<HistoryDetailCall>(params, |p| {
            Ok(HistoryDetailResult {
                detail: history_service::detail(&p.agent, &p.path)?,
            })
        }),
        ControlMethod::HistoryDelete => call::<HistoryDeleteCall>(params, |p| {
            history_service::delete(&p)?;
            Ok(EmptyParams::default())
        }),
        ControlMethod::HistoryTitles => {
            call::<HistoryTitlesCall>(params, |_| Ok(history_service::titles()))
        }
        other => Err(ControlError::new(
            ControlErrorCode::Internal,
            format!("`{other}` is not a history method"),
        )),
    }
}

fn call<C: ControlCall>(
    params: serde_json::Value,
    run: impl FnOnce(C::Params) -> Result<C::Result, HistoryError>,
) -> Result<serde_json::Value, ControlError> {
    let params = decode_params::<C::Params>(params)?;
    let result = run(params).map_err(|error| {
        let code = match error {
            HistoryError::UnknownAgent(_) => ControlErrorCode::InvalidParams,
            HistoryError::NotInHistory(_) => ControlErrorCode::NotFound,
            HistoryError::Io(_) => ControlErrorCode::OperationFailed,
        };
        ControlError::new(code, error.to_string())
    })?;
    serde_json::to_value(result)
        .map_err(|error| ControlError::new(ControlErrorCode::Internal, error.to_string()))
}
