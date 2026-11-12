//! `pi.*` 控制方法：Pi 的配置文件在运行时那台机器上，读写、问端点、跑登录都在这里
//! 执行。业务规则在 `smelt_core::pi_service`。修改类方法都回带新的完整快照。
//!
//! 改了配置的方法成功后发 `pi_config.changed`（见 [`changes_config`]），谁改的都发：
//! 别处开着的编辑器、模型下拉、对话据此重读，不靠改的一方去通知。

use smelt_core::control_api::*;
use smelt_core::pi_auth_service;
use smelt_core::pi_service::{self, PiConfig};
use smelt_runtime_client::protocol::control::{
    ControlCall, ControlError, ControlErrorCode, ControlMethod, EmptyParams, decode_params,
};

use crate::event_hub::EventHubHandle;

/// `method` 必须是 `pi.*` 之一。
pub(crate) fn dispatch(
    method: ControlMethod,
    params: serde_json::Value,
    event_hub: &EventHubHandle,
) -> Result<serde_json::Value, ControlError> {
    let result = run(method, params, event_hub);
    // 失败的也发：一次调用可能改了一半（如删插件时勾选已清、实体没删掉），多发一次只是
    // 让别处重读，漏发则别处一直拿着旧的。
    if changes_config(method) {
        notify_changed(event_hub);
    }
    result
}

/// 这类调用会改配置（成功时一定改了，失败时可能改了一半）。改没改要看结果的（自动模型
/// 目录、登录）不在这里，在 `run` 里各自判断。
fn changes_config(method: ControlMethod) -> bool {
    matches!(
        method,
        ControlMethod::PiConfigEnsureFile
            | ControlMethod::PiModelsSetThinkingLevel
            | ControlMethod::PiModelsSaveDefault
            | ControlMethod::PiProvidersSaveCustom
            | ControlMethod::PiProvidersRemoveCustom
            | ControlMethod::PiMcpAddPreset
            | ControlMethod::PiMcpSetEnabled
            | ControlMethod::PiMcpRemove
            | ControlMethod::PiMcpLogin
            | ControlMethod::PiPluginsRemove
            | ControlMethod::PiPluginsImport
            | ControlMethod::PiAuthLogout
    )
}

fn notify_changed(event_hub: &EventHubHandle) {
    if let Err(error) = event_hub.publish_pi_config_changed() {
        eprintln!("[pi] 通知 Pi 配置变化失败：{error}");
    }
}

fn run(
    method: ControlMethod,
    params: serde_json::Value,
    event_hub: &EventHubHandle,
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
        // 桌面每次启动都请一次，多半什么也没问到：没写就不发，免得别处白白重读。
        ControlMethod::PiProvidersRefreshAuto => call::<PiProvidersRefreshAuto>(params, |_| {
            if pi_service::refresh_auto_models() {
                notify_changed(event_hub);
            }
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
        // 成功失败都发：失败时别处重读一次也无妨。没人来取事件的登录会被当成没人要清掉，
        // 不会改配置。
        ControlMethod::PiAuthLoginEvents => call::<PiAuthLoginEvents>(params, |p| {
            let result = pi_auth_service::events(p)?;
            if result.finished {
                notify_changed(event_hub);
            }
            Ok(result)
        }),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 只读的方法不发：别处收到就会来读，读一次发一次会来回打转。
    #[test]
    fn reads_never_announce_a_change() {
        for method in [
            ControlMethod::PiConfigGet,
            ControlMethod::PiProvidersDiscoverModels,
            ControlMethod::PiAuthProviders,
            ControlMethod::PiAuthModels,
            ControlMethod::PiAuthLoginStart,
            ControlMethod::PiAuthLoginEvents,
            ControlMethod::PiAuthLoginAnswer,
            ControlMethod::PiAuthLoginCancel,
            ControlMethod::PiProvidersRefreshAuto,
        ] {
            assert!(!changes_config(method), "{method}");
        }
        for method in [
            ControlMethod::PiModelsSaveDefault,
            ControlMethod::PiProvidersSaveCustom,
            ControlMethod::PiMcpSetEnabled,
            ControlMethod::PiPluginsRemove,
            ControlMethod::PiAuthLogout,
        ] {
            assert!(changes_config(method), "{method}");
        }
    }
}
