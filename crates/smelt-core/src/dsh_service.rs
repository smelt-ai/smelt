//! DeepSeek Harness 的配置业务（`dsh.*`）：默认模型、自定义 provider、profile 插件。
//!
//! dsh 的配置（`$DSH_HOME/settings.yaml`、凭据、profile）和它的 Node 桥都在运行时那台
//! 机器上：读写、问端点、跑 pnpm 都由运行时执行。客户端拿 [`DshConfig`] 快照显示，
//! 修改走这里的方法。模型设置的具体形状由桥决定（`dsh-model-settings --action read`
//! 的输出），这里原样转交，客户端按桥的协议解析。

pub use smelt_model::dsh_service::*;

use serde::Deserialize;

use crate::agent_kind::{
    DshDiscoveryReport, DshDiscoveryRequest, DshModelCapabilities, SMELT_HOST_VERSION,
};

pub fn config() -> DshConfig {
    let profiles = crate::agent_kind::native_dsh_profiles();
    let missing_profiles = crate::agent_kind::dsh_profile_names()
        .into_iter()
        .filter(|name| {
            !profiles
                .iter()
                .any(|profile| profile.workspace_dir == *name)
        })
        .map(|name| DshMissingProfile {
            has_bridge: crate::agent_kind::dsh_bridge_entry(&name).is_some(),
            name,
        })
        .collect();
    let text =
        |path: Option<std::path::PathBuf>| path.map(|path| path.to_string_lossy().into_owned());
    DshConfig {
        settings_summary: crate::agent_kind::dsh_settings_summary(),
        model_settings: read_model_settings(),
        profiles,
        missing_profiles,
        auto_provider_ids: crate::dsh_auto_models::load().auto_providers(),
        paths: DshConfigPaths {
            settings: text(crate::agent_kind::dsh_settings_path()),
            credentials: text(crate::agent_kind::dsh_credentials_path()),
        },
    }
}

fn read_model_settings() -> Result<serde_json::Value, String> {
    let output = crate::agent_kind::dsh_model_settings("read", None)?;
    serde_json::from_slice(&output).map_err(|error| format!("读取 DSH 模型设置响应失败：{error}"))
}

/// 能回答能力查询、模型发现的 profile。桥要用 `--profile` 把 dsh 装配成那个 profile，
/// 能力答案本来就是 profile 相关的。
fn capabilities_profile() -> Result<String, String> {
    crate::agent_kind::native_dsh_profiles()
        .into_iter()
        .find(|profile| {
            crate::agent_kind::dsh_model_capabilities_entry(&profile.workspace_dir).is_some()
        })
        .map(|profile| profile.workspace_dir)
        .ok_or_else(|| {
            format!("未找到模型能力查询器，请把 Smelt Host bundle 更新到 {SMELT_HOST_VERSION}")
        })
}

/// 每条路由支持哪些推理强度。要起一个完整的 dsh 运行时，几秒。
pub fn capabilities() -> Result<DshModelCapabilities, String> {
    crate::agent_kind::dsh_model_capabilities(&capabilities_profile()?)
}

/// 问端点它提供哪些模型。
pub fn discover_models(request: DshDiscoveryRequest) -> Result<DshDiscoveryReport, String> {
    if request.base_url.is_none() && request.provider.is_none() {
        return Err("请先填写 API 地址，模型列表要从那个端点读取".into());
    }
    let profile = capabilities_profile().map_err(|_| {
        format!("未找到模型发现器，请把 Smelt Host bundle 更新到 {SMELT_HOST_VERSION}")
    })?;
    crate::agent_kind::dsh_discover_models(&profile, &request)
}

#[derive(Deserialize)]
struct SaveResponse {
    ok: bool,
}

fn run_save(action: &str, payload: &serde_json::Value) -> Result<(), String> {
    let output = crate::agent_kind::dsh_model_settings(action, Some(payload))?;
    let response: SaveResponse = serde_json::from_slice(&output)
        .map_err(|error| format!("读取 DSH 模型管理器响应失败：{error}"))?;
    response
        .ok
        .then_some(())
        .ok_or_else(|| "DSH 模型管理器未确认保存".to_string())
}

pub fn save_default_model(params: SaveDefaultModelParams) -> Result<(), String> {
    run_save("save", &params.payload)
}

/// 保存自定义 provider，连同「自动」标记一起改。返回模型目录是否自动维护。
pub fn save_custom_provider(params: SaveCustomProviderParams) -> Result<bool, String> {
    let auto = params.models.is_empty();
    let models = if auto {
        let report = discover_models(DshDiscoveryRequest {
            provider: Some(params.id.clone()),
            base_url: Some(params.base_url.clone()),
            api: Some(params.api.clone()),
            api_key: (!params.api_key.is_empty()).then(|| params.api_key.clone()),
        })
        .map_err(|error| format!("{error}\n未能自动获取模型，请手动填写至少一个模型"))?;
        if report.models.is_empty() {
            return Err("该端点没有报告任何模型，请手动填写至少一个模型".into());
        }
        report
            .models
            .iter()
            .map(|model| {
                serde_json::json!({
                    "id": model.id,
                    "name": model.name,
                    "contextWindow": model.context_window,
                    "maxTokens": model.max_tokens,
                })
            })
            .collect()
    } else {
        params.models
    };
    run_save(
        "save-custom",
        &serde_json::json!({
            "previousId": params.previous_id,
            "id": params.id,
            "displayName": params.display_name,
            "api": params.api,
            "baseURL": params.base_url,
            "apiKeyEnv": params.api_key_env,
            "apiKey": (!params.api_key.is_empty()).then_some(&params.api_key),
            "models": models,
            "setDefault": params.set_default,
        }),
    )?;
    crate::dsh_auto_models::update(|store| {
        if let Some(previous) = params.previous_id.as_deref() {
            store.rename(previous, &params.id);
        }
        store.set_auto(&params.id, auto);
    });
    Ok(auto)
}

/// 确保配置文件存在（新建的只给本人读写），返回路径，供用户用编辑器手改。
pub fn ensure_config_file(
    params: EnsureConfigFileParams,
) -> Result<EnsureConfigFileResult, String> {
    let path = if params.credentials {
        crate::agent_kind::dsh_credentials_path()
    } else {
        crate::agent_kind::dsh_settings_path()
    }
    .ok_or_else(|| "无法定位 DSH_HOME".to_string())?;
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建 DSH 配置目录失败：{error}"))?;
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                        .map_err(|error| format!("保护 DSH 配置文件失败：{error}"))?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("创建 DSH 配置文件失败：{error}")),
        }
    }
    Ok(EnsureConfigFileResult {
        path: path.to_string_lossy().into_owned(),
    })
}

pub fn plugins(params: ProfileParams) -> PluginsResult {
    PluginsResult {
        plugins: crate::agent_kind::dsh_profile_plugins(&params.profile),
    }
}

/// 安装、更新或卸载一个插件（pnpm，要走网络，几秒到几分钟），完了重读清单。
pub fn plugin_action(params: PluginActionParams) -> PluginActionResult {
    let outcome = match params.action {
        PluginAction::Install => crate::agent_kind::dsh_plugin_add(&params.profile, &params.target),
        PluginAction::Update => {
            crate::agent_kind::dsh_plugin_update(&params.profile, &params.target)
        }
        PluginAction::Remove => {
            crate::agent_kind::dsh_plugin_remove(&params.profile, &params.target)
        }
    }
    .map(|_| ());
    PluginActionResult {
        outcome,
        plugins: crate::agent_kind::dsh_profile_plugins(&params.profile),
    }
}

/// 照端点刷新所有「自动」provider 的模型目录。桌面启动时请运行时做一次；要起 Node
/// 子进程，几秒级。
pub fn refresh_auto_models() {
    for outcome in crate::dsh_auto_models::refresh_auto_model_catalogs() {
        match outcome {
            crate::dsh_auto_models::AutoRefreshOutcome::Refreshed { provider, models } => {
                eprintln!("[dsh] {provider} 的模型目录已刷新：{models} 个模型");
            }
            crate::dsh_auto_models::AutoRefreshOutcome::Kept { provider, reason } => {
                eprintln!("[dsh] {provider} 的模型目录保持不变：{reason}");
            }
            crate::dsh_auto_models::AutoRefreshOutcome::Forgotten { provider } => {
                eprintln!("[dsh] {provider} 已不在配置中，不再自动刷新其模型目录");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_needs_an_endpoint_or_a_known_route() {
        let error = discover_models(DshDiscoveryRequest::default()).unwrap_err();
        assert!(error.contains("API 地址"), "{error}");
    }

    /// 桥的模型设置原样转交；读不出的部分各自带错误。
    #[test]
    fn config_snapshot_round_trips() {
        let config = DshConfig {
            settings_summary: Err("无法定位 DSH_HOME".into()),
            model_settings: Ok(serde_json::json!({"credentialConfigured": true})),
            profiles: Vec::new(),
            missing_profiles: vec![DshMissingProfile {
                name: "default".into(),
                has_bridge: true,
            }],
            auto_provider_ids: vec!["gateway".into()],
            paths: DshConfigPaths::default(),
        };
        let decoded: DshConfig =
            serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
        assert_eq!(decoded, config);
    }
}
