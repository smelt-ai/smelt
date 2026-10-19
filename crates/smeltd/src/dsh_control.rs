//! `dsh.*` 控制方法：DeepSeek Harness 的配置、Node 桥和 pnpm 都在运行时那台机器上。
//! 业务规则在 `smelt_core::dsh_service`。

use smelt_core::control_api::*;
use smelt_core::dsh_service;
use smelt_runtime_client::protocol::control::{
    ControlCall, ControlError, ControlErrorCode, ControlMethod, EmptyParams, decode_params,
};

/// `method` 必须是 `dsh.*` 之一。
pub(crate) fn dispatch(
    method: ControlMethod,
    params: serde_json::Value,
) -> Result<serde_json::Value, ControlError> {
    match method {
        ControlMethod::DshConfigGet => call::<DshConfigGet>(params, |_| Ok(dsh_service::config())),
        ControlMethod::DshConfigEnsureFile => {
            call::<DshConfigEnsureFile>(params, dsh_service::ensure_config_file)
        }
        ControlMethod::DshModelsCapabilities => {
            call::<DshModelsCapabilities>(params, |_| dsh_service::capabilities())
        }
        ControlMethod::DshModelsSaveDefault => call::<DshModelsSaveDefault>(params, |p| {
            dsh_service::save_default_model(p)?;
            Ok(dsh_service::config())
        }),
        ControlMethod::DshProvidersDiscoverModels => {
            call::<DshProvidersDiscoverModels>(params, dsh_service::discover_models)
        }
        ControlMethod::DshProvidersSaveCustom => call::<DshProvidersSaveCustom>(params, |p| {
            let auto = dsh_service::save_custom_provider(p)?;
            Ok(DshSaveCustomProviderResult {
                auto,
                config: dsh_service::config(),
            })
        }),
        ControlMethod::DshProvidersRefreshAuto => call::<DshProvidersRefreshAuto>(params, |_| {
            dsh_service::refresh_auto_models();
            Ok(EmptyParams::default())
        }),
        ControlMethod::DshPluginsList => {
            call::<DshPluginsList>(params, |p| Ok(dsh_service::plugins(p)))
        }
        ControlMethod::DshPluginsRun => {
            call::<DshPluginsRun>(params, |p| Ok(dsh_service::plugin_action(p)))
        }
        other => Err(ControlError::new(
            ControlErrorCode::Internal,
            format!("`{other}` is not a dsh method"),
        )),
    }
}

fn call<C: ControlCall>(
    params: serde_json::Value,
    run: impl FnOnce(C::Params) -> Result<C::Result, String>,
) -> Result<serde_json::Value, ControlError> {
    let params = decode_params::<C::Params>(params)?;
    let result =
        run(params).map_err(|error| ControlError::new(ControlErrorCode::OperationFailed, error))?;
    serde_json::to_value(result)
        .map_err(|error| ControlError::new(ControlErrorCode::Internal, error.to_string()))
}
