//! `smelt mcp …`、`smelt plugin …`、`smelt model …`：能力库里 Pi 的 MCP 服务器、插件和默认
//! 模型。都是运行时那台机器上的全局配置，对所有智能体生效。

use crate::{control, flag_value, id_arg, respond, usage};
use smelt_model::control_api::{
    PiConfigGet, PiMcpAddPreset, PiMcpRemove, PiMcpSetEnabled, PiModelsSaveDefault,
    PiModelsSetThinkingLevel, PiPluginsImport, PiPluginsRemove,
};
use smelt_model::pi_mcp::{PI_MCP_PRESETS, find_preset};
use smelt_model::pi_model_settings::PiDefaultModelConfig;
use smelt_model::pi_plugin_catalog::PiPluginKind;
use smelt_model::pi_service::{
    ImportPluginParams, McpEnabledParams, McpPresetParams, McpServerParams, PiConfig,
    PluginIdParams, SaveDefaultModelParams, ThinkingLevelParams,
};
use smelt_runtime_client::protocol::control::EmptyParams;

/// Pi 认的推理强度。
const THINKING_LEVELS: [&str; 6] = ["off", "minimal", "low", "medium", "high", "xhigh"];

fn config() -> Result<PiConfig, String> {
    control::<PiConfigGet>(EmptyParams::default())
}

pub(crate) fn run_mcp(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("list") => respond(config().and_then(mcp_json)),
        Some("presets") => respond(Ok(presets_json())),
        Some("add") => match id_arg(rest) {
            Some(id) => respond(add_preset(id)),
            None => usage("smelt mcp add <预设id>（smelt mcp presets 查看）"),
        },
        Some(verb @ ("enable" | "disable")) => match id_arg(rest) {
            Some(name) => respond(
                control::<PiMcpSetEnabled>(McpEnabledParams {
                    name: name.to_string(),
                    enabled: verb == "enable",
                })
                .and_then(mcp_json),
            ),
            None => usage("smelt mcp enable|disable <服务器名>"),
        },
        Some("remove") => match id_arg(rest) {
            Some(name) => respond(
                control::<PiMcpRemove>(McpServerParams {
                    name: name.to_string(),
                })
                .and_then(mcp_json),
            ),
            None => usage("smelt mcp remove <服务器名>"),
        },
        Some(command) => usage(&format!("未知子命令: {command}")),
        None => usage("smelt mcp list|presets|add|enable|disable|remove"),
    }
}

fn mcp_json(config: PiConfig) -> Result<serde_json::Value, String> {
    let servers = config
        .mcp_servers
        .map_err(|error| format!("读不到 MCP 配置：{error}"))?;
    Ok(serde_json::json!({ "servers": servers }))
}

fn presets_json() -> serde_json::Value {
    let presets: Vec<_> = PI_MCP_PRESETS
        .iter()
        .map(|preset| {
            serde_json::json!({
                "id": preset.id,
                "name": preset.name,
                "summary": preset.summary,
                "needs_sign_in": preset.transport.needs_sign_in(),
                "extension_url": preset.extension_url,
            })
        })
        .collect();
    serde_json::json!({ "presets": presets })
}

/// 要登录的服务器加上以后还连不上，得让用户去设置里点「登录」（要开浏览器授权），
/// 命令行不代办。
fn add_preset(id: &str) -> Result<serde_json::Value, String> {
    let preset =
        find_preset(id).ok_or_else(|| format!("没有这个预设：{id}（smelt mcp presets 查看）"))?;
    let mut result =
        control::<PiMcpAddPreset>(McpPresetParams { id: id.to_string() }).and_then(mcp_json)?;
    if preset.transport.needs_sign_in() {
        result["next"] = "需要用户在 Smelt 设置的能力库里登录这个服务器".into();
    } else if let Some(url) = preset.extension_url {
        result["next"] = format!("需要用户在 Chrome 里装扩展：{url}").into();
    }
    Ok(result)
}

pub(crate) fn run_plugin(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("list") => respond(config().map(plugins_json)),
        Some("import") => match parse_import(rest) {
            Ok(params) => respond(control::<PiPluginsImport>(params).map(plugins_json)),
            Err(message) => usage(&message),
        },
        Some("remove") => match id_arg(rest) {
            Some(id) => respond(
                control::<PiPluginsRemove>(PluginIdParams { id: id.to_string() }).map(plugins_json),
            ),
            None => usage("smelt plugin remove <id>"),
        },
        Some(command) => usage(&format!("未知子命令: {command}")),
        None => usage("smelt plugin list|import|remove"),
    }
}

fn plugins_json(config: PiConfig) -> serde_json::Value {
    let plugins: Vec<_> = config
        .plugins
        .into_iter()
        .map(|plugin| {
            serde_json::json!({
                "id": plugin.id,
                "kind": plugin.kind.as_str(),
                "name": plugin.name,
                "description": plugin.description,
                "origin": plugin.origin,
                "builtin": plugin.builtin,
                "broken": plugin.broken,
            })
        })
        .collect();
    serde_json::json!({ "plugins": plugins })
}

/// 运行时在本机，相对路径按当前目录补全（运行时的工作目录不是这里）。
fn parse_import(args: &[String]) -> Result<ImportPluginParams, String> {
    const USAGE: &str = "smelt plugin import --skill <目录> | --extension <路径>";
    let kind = match args.first().map(String::as_str) {
        Some("--skill") => PiPluginKind::Skill,
        Some("--extension") => PiPluginKind::Extension,
        _ => return Err(USAGE.into()),
    };
    if args.len() != 2 {
        return Err(USAGE.into());
    }
    let source = flag_value(args, 0)?;
    let source =
        std::path::absolute(&source).map_err(|error| format!("路径无效：{source}：{error}"))?;
    Ok(ImportPluginParams {
        kind,
        source: source.to_string_lossy().into_owned(),
    })
}

pub(crate) fn run_model(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("list") => respond(config().and_then(models_json)),
        Some("default") => match id_arg(rest) {
            Some(model) => respond(set_default(model)),
            None => usage("smelt model default <provider>/<模型id>（smelt model list 查看）"),
        },
        Some("thinking") => match id_arg(rest) {
            Some(level) if THINKING_LEVELS.contains(&level) => respond(
                control::<PiModelsSetThinkingLevel>(ThinkingLevelParams {
                    level: level.to_string(),
                })
                .and_then(models_json),
            ),
            _ => usage(&format!(
                "smelt model thinking {}",
                THINKING_LEVELS.join("|")
            )),
        },
        Some(command) => usage(&format!("未知子命令: {command}")),
        None => usage("smelt model list|default|thinking"),
    }
}

/// 只给默认模型和可选模型，不带 provider 配置（里面可能有地址和凭据状态）。
fn models_json(config: PiConfig) -> Result<serde_json::Value, String> {
    let summary = config
        .settings_summary
        .map_err(|error| format!("读不到 Pi 设置：{error}"))?;
    Ok(serde_json::json!({
        "default": summary.default_model,
        "models": config.model_choice_groups,
    }))
}

pub(crate) fn has_model(config: &PiConfig, provider: &str, model: &str) -> bool {
    config.model_choice_groups.iter().any(|group| {
        group
            .models
            .iter()
            .any(|choice| choice.provider == provider && choice.id == model)
    })
}

/// 和设置里从菜单挑模型一样：不动地址、不换 key；推理强度沿用现在的。
fn set_default(value: &str) -> Result<serde_json::Value, String> {
    let (provider, model) = value
        .split_once('/')
        .filter(|(provider, model)| !provider.is_empty() && !model.is_empty())
        .ok_or("格式为 <provider>/<模型id>")?;
    let current = config()?;
    if !has_model(&current, provider, model) {
        return Err(format!(
            "没有这个模型：{provider}/{model}（smelt model list 查看）"
        ));
    }
    let thinking_level = current
        .settings_summary
        .ok()
        .and_then(|summary| summary.default_model)
        .and_then(|model| model.thinking_level)
        .unwrap_or_default();
    control::<PiModelsSaveDefault>(SaveDefaultModelParams {
        config: PiDefaultModelConfig {
            provider: provider.to_string(),
            model: model.to_string(),
            base_url: String::new(),
            thinking_level,
        },
        api_key: None,
        reset_base_url: false,
    })
    .and_then(models_json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args;

    #[test]
    fn import_resolves_relative_paths_here() {
        let params = parse_import(&args(&["--skill", "skills/writer"])).unwrap();
        assert_eq!(params.kind, PiPluginKind::Skill);
        assert!(std::path::Path::new(&params.source).is_absolute());
        assert!(params.source.ends_with("writer"));
        assert!(parse_import(&args(&["--skill"])).is_err());
        assert!(parse_import(&args(&["--theme", "x"])).is_err());
        assert!(parse_import(&args(&["--extension", "a", "b"])).is_err());
    }

    #[test]
    fn every_preset_can_be_found_by_id() {
        let presets = presets_json();
        for preset in presets["presets"].as_array().unwrap() {
            assert!(find_preset(preset["id"].as_str().unwrap()).is_some());
        }
    }
}
