//! `pi.*` 控制方法：Pi 的配置文件在运行时那台机器上，读写、问端点、跑登录都在这里
//! 执行。业务规则在 `smelt_core::pi_service`。修改类方法都回带新的完整快照。

use smelt_core::control_api::*;
use smelt_core::pi_auth_service;
use smelt_core::pi_service::{self, PiConfig};
use smelt_runtime_client::protocol::control::{
    ControlCall, ControlError, ControlErrorCode, ControlMethod, EmptyParams, decode_params,
};

/// `method` 必须是 `pi.*` 之一。
pub(crate) fn dispatch(
    method: ControlMethod,
    params: serde_json::Value,
) -> Result<serde_json::Value, ControlError> {
    match method {
        ControlMethod::PiConfigGet => call::<PiConfigGet>(params, |_| Ok(pi_service::config())),
        ControlMethod::PiConfigEnsureFile => {
            call::<PiConfigEnsureFile>(params, pi_service::ensure_config_file)
        }
        ControlMethod::PiModelsSetThinkingLevel => {
            mutate::<PiModelsSetThinkingLevel>(params, pi_service::set_thinking_level)
        }
        ControlMethod::PiModelsSaveDefault => {
            mutate::<PiModelsSaveDefault>(params, pi_service::save_default_model)
        }
        ControlMethod::PiProvidersDiscoverModels => {
            call::<PiProvidersDiscoverModels>(params, pi_service::discover_models)
        }
        ControlMethod::PiProvidersSaveCustom => call::<PiProvidersSaveCustom>(params, |p| {
            let auto = pi_service::save_custom_provider(p)?;
            Ok(PiSaveCustomProviderResult {
                auto,
                config: pi_service::config(),
            })
        }),
        ControlMethod::PiProvidersRemoveCustom => {
            mutate::<PiProvidersRemoveCustom>(params, pi_service::remove_custom_provider)
        }
        ControlMethod::PiProvidersRefreshAuto => call::<PiProvidersRefreshAuto>(params, |_| {
            pi_service::refresh_auto_models();
            Ok(EmptyParams::default())
        }),
        ControlMethod::PiMcpAddPreset => {
            mutate::<PiMcpAddPreset>(params, pi_service::add_mcp_preset)
        }
        ControlMethod::PiMcpSetEnabled => {
            mutate::<PiMcpSetEnabled>(params, pi_service::set_mcp_enabled)
        }
        ControlMethod::PiMcpRemove => mutate::<PiMcpRemove>(params, pi_service::remove_mcp_server),
        ControlMethod::PiMcpLogin => mutate::<PiMcpLogin>(params, pi_service::login_mcp_server),
        ControlMethod::PiPluginsRemove => {
            mutate::<PiPluginsRemove>(params, pi_service::remove_plugin)
        }
        ControlMethod::PiPluginsImport => {
            mutate::<PiPluginsImport>(params, pi_service::import_plugin)
        }
        ControlMethod::PiAuthProviders => {
            call::<PiAuthProviders>(params, |_| pi_auth_service::providers())
        }
        ControlMethod::PiAuthModels => {
            call::<PiAuthModels>(params, |p| pi_auth_service::models(&p.provider_id))
        }
        ControlMethod::PiAuthLogout => call::<PiAuthLogout>(params, |p| {
            pi_auth_service::logout(&p.provider_id)?;
            Ok(EmptyParams::default())
        }),
        ControlMethod::PiAuthLoginStart => call::<PiAuthLoginStart>(params, |p| {
            Ok(pi_auth_service::LoginIdParams {
                login_id: pi_auth_service::start(p)?,
            })
        }),
        ControlMethod::PiAuthLoginEvents => {
            call::<PiAuthLoginEvents>(params, pi_auth_service::events)
        }
        ControlMethod::PiAuthLoginAnswer => call::<PiAuthLoginAnswer>(params, |p| {
            pi_auth_service::answer(p)?;
            Ok(EmptyParams::default())
        }),
        ControlMethod::PiAuthLoginCancel => call::<PiAuthLoginCancel>(params, |p| {
            pi_auth_service::cancel(p)?;
            Ok(EmptyParams::default())
        }),
        other => Err(ControlError::new(
            ControlErrorCode::Internal,
            format!("`{other}` is not a pi method"),
        )),
    }
}

/// 改完回带新快照。
fn mutate<C: ControlCall<Result = PiConfig>>(
    params: serde_json::Value,
    run: impl FnOnce(C::Params) -> Result<(), String>,
) -> Result<serde_json::Value, ControlError> {
    call::<C>(params, |p| {
        run(p)?;
        Ok(pi_service::config())
    })
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
