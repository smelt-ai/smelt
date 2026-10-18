//! Pi 的配置业务（`pi.*`）：默认模型、自定义 provider、MCP 服务器、插件。
//!
//! Pi 的配置文件在运行时那台机器上（`~/.pi/agent/…`），读写、问端点、跑登录子进程都
//! 由运行时执行。客户端拿 [`PiConfig`] 快照显示，修改走这里的方法，每个修改返回新的
//! 快照。已保存的凭据只留在运行时：客户端只知道配没配，问端点时由运行时自己补上。

pub use smelt_model::pi_service::*;

use std::path::PathBuf;

use crate::pi_model_settings::{PiCustomModel, PiDefaultModelConfig};

fn config_file_path(file: PiConfigFile) -> PathBuf {
    {
        match file {
            PiConfigFile::Settings => crate::pi_model_settings::pi_settings_path(),
            PiConfigFile::Models => crate::pi_model_settings::pi_models_path(),
            PiConfigFile::Credentials => crate::pi_model_settings::pi_auth_path(),
            PiConfigFile::Mcp => crate::pi_mcp::pi_mcp_path(),
        }
    }
}

pub fn config() -> PiConfig {
    let text = |path: PathBuf| path.to_string_lossy().into_owned();
    PiConfig {
        model_settings: crate::pi_model_settings::load_pi_model_settings(),
        settings_summary: crate::pi_model_settings::pi_settings_summary(),
        mcp_servers: crate::pi_mcp::load_pi_mcp_servers(),
        plugins: crate::pi_plugin_catalog::discover_plugins(),
        model_choice_groups: crate::pi_model_settings::list_pi_model_choice_groups(),
        auto_provider_ids: crate::pi_auto_models::load().auto_providers(),
        paths: PiConfigPaths {
            settings: text(config_file_path(PiConfigFile::Settings)),
            models: text(config_file_path(PiConfigFile::Models)),
            auth: text(config_file_path(PiConfigFile::Credentials)),
            mcp: text(config_file_path(PiConfigFile::Mcp)),
        },
    }
}

/// 只改默认模型的推理强度。用户还没选过默认模型时拒绝：写进去的 provider/model
/// 会是 Pi 的兜底值，等于替用户做了选择。
pub fn set_thinking_level(params: ThinkingLevelParams) -> Result<(), String> {
    let summary = crate::pi_model_settings::pi_settings_summary()?;
    let Some((provider, model)) = summary.default_model.and_then(|model| {
        Some((
            model.provider.filter(|v| !v.trim().is_empty())?,
            model.model.filter(|v| !v.trim().is_empty())?,
        ))
    }) else {
        return Err("先选一个默认模型，再调推理强度".into());
    };
    let config = PiDefaultModelConfig {
        provider,
        model,
        base_url: String::new(),
        thinking_level: params.level,
    };
    crate::pi_model_settings::save_pi_default_model(&config, None)
}

pub fn save_default_model(params: SaveDefaultModelParams) -> Result<(), String> {
    crate::pi_model_settings::save_pi_default_model(&params.config, params.api_key.as_deref())?;
    if params.reset_base_url && params.config.base_url.is_empty() {
        crate::pi_model_settings::clear_pi_provider_base_url(&params.config.provider)?;
    }
    Ok(())
}

/// 问端点它提供哪些模型（一次 `GET /models`）。
pub fn discover_models(params: DiscoverModelsParams) -> Result<DiscoverModelsResult, String> {
    let request = discovery_request(params);
    crate::pi_model_discovery::models_endpoint(&request.base_url)?;
    Ok(DiscoverModelsResult {
        models: crate::pi_model_discovery::discover_models(&request)?,
    })
}

fn discovery_request(
    params: DiscoverModelsParams,
) -> crate::pi_model_discovery::ModelDiscoveryRequest {
    let mut api_key = params.api_key;
    if api_key.is_empty()
        && let Some(provider_id) = params
            .saved_provider_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        && let Some(key) = crate::pi_model_settings::read_pi_provider_api_key_at(
            &crate::pi_model_settings::pi_agent_dir(),
            provider_id,
        )
    {
        api_key = key;
    }
    crate::pi_model_discovery::ModelDiscoveryRequest {
        base_url: params.base_url,
        api: params.api,
        api_key,
    }
}

/// 保存自定义 provider，连同「自动」标记一起改：改名要把标记带过去，否则用户会得到
/// 一个以为自动、其实再也不刷新的 provider。
pub fn save_custom_provider(params: SaveCustomProviderParams) -> Result<bool, String> {
    let SaveCustomProviderParams {
        previous_id,
        mut provider,
        api_key,
        set_default,
    } = params;
    crate::pi_model_settings::validate_custom_provider_id(&provider.id)?;
    let auto = provider.models.is_empty();
    if auto {
        // Pi 从 models.json 解析模型，写一份空列表等于这个 provider 一个模型都选不了。
        let discovered = discover_models(DiscoverModelsParams {
            base_url: provider.base_url.clone(),
            api: provider.api.clone(),
            api_key: api_key.clone().unwrap_or_default(),
            saved_provider_id: previous_id.clone(),
        })
        .map_err(|error| format!("{error}\n未能自动获取模型，请手动填写至少一个模型"))?
        .models;
        if discovered.is_empty() {
            return Err("该端点没有报告任何模型，请手动填写至少一个模型".into());
        }
        provider.models = discovered
            .into_iter()
            .map(|model| PiCustomModel {
                name: model.label().to_string(),
                id: model.id,
                context_window: model.context_window,
                max_tokens: model.max_tokens,
            })
            .collect();
    }
    crate::pi_model_settings::save_pi_custom_provider_replacing(
        previous_id.as_deref(),
        &provider,
        api_key.as_deref(),
        set_default,
    )?;
    crate::pi_auto_models::update(|store| {
        if let Some(previous) = &previous_id
            && previous != &provider.id
        {
            store.set_auto(previous, false);
        }
        store.set_auto(&provider.id, auto);
    });
    Ok(auto)
}

pub fn remove_custom_provider(params: ProviderIdParams) -> Result<(), String> {
    crate::pi_model_settings::remove_pi_custom_provider(&params.provider_id)?;
    crate::pi_auto_models::update(|store| store.set_auto(&params.provider_id, false));
    Ok(())
}

/// 确保配置文件存在（不存在就建一个空对象，只给本人读写：`auth.json` 之外，用户手改
/// `models.json`、`mcp.json` 时也常直接写令牌），返回路径，供用户用编辑器手改。
pub fn ensure_config_file(
    params: EnsureConfigFileParams,
) -> Result<EnsureConfigFileResult, String> {
    use std::io::Write as _;

    let path = config_file_path(params.file);
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建 Pi 配置目录失败：{error}"))?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(mut file) => {
                file.write_all(b"{\n}\n")
                    .map_err(|error| format!("创建 Pi 配置文件失败：{error}"))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("创建 Pi 配置文件失败：{error}")),
        }
    }
    Ok(EnsureConfigFileResult {
        path: path.to_string_lossy().into_owned(),
    })
}

pub fn add_mcp_preset(params: McpPresetParams) -> Result<(), String> {
    crate::pi_mcp::add_pi_mcp_preset(&params.id)
}

pub fn set_mcp_enabled(params: McpEnabledParams) -> Result<(), String> {
    crate::pi_mcp::set_pi_mcp_server_enabled(&params.name, params.enabled)
}

pub fn remove_mcp_server(params: McpServerParams) -> Result<(), String> {
    crate::pi_mcp::remove_pi_mcp_server(&params.name)
}

/// 跑 `pi mcp login <name>`：需要授权时 Pi 打开浏览器，等回调写好令牌。
/// 浏览器开在运行时那台机器上；远程运行时上的 OAuth 登录要等登录地址能转给客户端。
pub fn login_mcp_server(params: McpServerParams) -> Result<(), String> {
    crate::pi_mcp::login_pi_mcp_server(&params.name, &|_| {}).map(|_| ())
}

/// 删 `~/.pi` 下的插件实体。引用它的智能体勾选由调用方经智能体配置清掉。
pub fn remove_plugin(params: PluginIdParams) -> Result<(), String> {
    let plugin = crate::pi_plugin_catalog::discover_plugins()
        .into_iter()
        .find(|plugin| plugin.id == params.id)
        .ok_or_else(|| format!("插件不存在：{}", params.id))?;
    crate::pi_plugin_catalog::remove_plugin(&plugin)
}

pub fn import_plugin(params: ImportPluginParams) -> Result<(), String> {
    crate::pi_plugin_catalog::import_plugin(params.kind, std::path::Path::new(&params.source))
        .map(|_| ())
}

/// 照端点刷新所有「自动」provider 的模型目录。桌面启动时请运行时做一次；每个
/// provider 一次网络往返，单个失败不影响其余，也不改动它的目录。
pub fn refresh_auto_models() {
    for outcome in crate::pi_auto_models::refresh_auto_model_catalogs() {
        match outcome {
            crate::pi_auto_models::AutoRefreshOutcome::Refreshed { provider, models } => {
                eprintln!("[pi] {provider} 的模型目录已刷新：{models} 个模型");
            }
            crate::pi_auto_models::AutoRefreshOutcome::Kept { provider, reason } => {
                eprintln!("[pi] {provider} 的模型目录保持不变：{reason}");
            }
            crate::pi_auto_models::AutoRefreshOutcome::Forgotten { provider } => {
                eprintln!("[pi] {provider} 已不在配置中，不再自动刷新其模型目录");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pi_mcp::PiMcpServer;
    use crate::pi_model_settings::{PiCustomProvider, PiSettingsSummary};
    use crate::pi_plugin_catalog::{PiPlugin, PiPluginKind};

    /// 快照里各块各自带错误：一块读不出，其余照常显示，传到客户端也还是这个样子。
    #[test]
    fn config_snapshot_round_trips_with_partial_errors() {
        let config = PiConfig {
            model_settings: Err("解析 models.json 失败".into()),
            settings_summary: Ok(PiSettingsSummary {
                path: PathBuf::from("/home/me/.pi/agent/settings.json"),
                default_model: None,
            }),
            mcp_servers: Ok(vec![PiMcpServer {
                name: "docs".into(),
                transport: crate::pi_mcp::PiMcpTransport::Http {
                    url: "https://example.com/mcp".into(),
                },
                enabled: true,
                description: None,
                signed_in: false,
            }]),
            plugins: vec![PiPlugin {
                id: "skill:review".into(),
                kind: PiPluginKind::Skill,
                name: "review".into(),
                description: String::new(),
                path: PathBuf::from("/home/me/.pi/agent/skills/review"),
                origin: "~/.pi/agent/skills".into(),
                broken: None,
            }],
            model_choice_groups: Vec::new(),
            auto_provider_ids: vec!["gateway".into()],
            paths: PiConfigPaths::default(),
        };
        let wire = serde_json::to_value(&config).unwrap();
        assert_eq!(wire["plugins"][0]["kind"], "skill");
        assert_eq!(wire["mcp_servers"]["Ok"][0]["transport"]["type"], "http");
        let decoded: PiConfig = serde_json::from_value(wire).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn invalid_provider_id_is_rejected_before_touching_the_endpoint() {
        let error = save_custom_provider(SaveCustomProviderParams {
            provider: PiCustomProvider {
                id: "has space".into(),
                base_url: "http://127.0.0.1:9/v1".into(),
                ..PiCustomProvider::default()
            },
            ..SaveCustomProviderParams::default()
        })
        .unwrap_err();
        assert!(!error.contains("未能自动获取模型"), "{error}");
    }
}
