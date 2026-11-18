//! `smelt mcp …`、`smelt pi skill|extension …`、`smelt model …`：能力库里 Pi 的 MCP 服务器、
//! 技能和扩展、默认模型。都是运行时那台机器上的全局配置，对所有智能体生效。
//!
//! Pi 的技能和扩展放在 `smelt pi` 下，不叫插件：`smelt plugin` 是 Smelt 自己的界面插件
//! （桌面程序里的命令，见 `smelt` 的 `plugins::cli`）。

use crate::{control, id_arg, respond, usage};
use rust_i18n::t;
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
            None => usage(&t!("mcp.add_usage")),
        },
        Some(verb @ ("enable" | "disable")) => match id_arg(rest) {
            Some(name) => respond(
                control::<PiMcpSetEnabled>(McpEnabledParams {
                    name: name.to_string(),
                    enabled: verb == "enable",
                })
                .and_then(mcp_json),
            ),
            None => usage(&t!("mcp.toggle_usage")),
        },
        Some("remove") => match id_arg(rest) {
            Some(name) => respond(
                control::<PiMcpRemove>(McpServerParams {
                    name: name.to_string(),
                })
                .and_then(mcp_json),
            ),
            None => usage(&t!("mcp.remove_usage")),
        },
        Some(command) => usage(&t!("unknown_subcommand", command = command)),
        None => usage("smelt mcp list|presets|add|enable|disable|remove"),
    }
}

fn mcp_json(config: PiConfig) -> Result<serde_json::Value, String> {
    let servers = config
        .mcp_servers
        .map_err(|error| t!("mcp_config_unreadable", error = error).into_owned())?;
    Ok(serde_json::json!({ "servers": servers }))
}

fn presets_json() -> serde_json::Value {
    let presets: Vec<_> = PI_MCP_PRESETS
        .iter()
        .map(|preset| {
            serde_json::json!({
                "id": preset.id,
                "name": preset.name(),
                "summary": preset.summary(),
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
    let preset = find_preset(id).ok_or_else(|| t!("mcp.no_such_preset", id = id).into_owned())?;
    let mut result =
        control::<PiMcpAddPreset>(McpPresetParams { id: id.to_string() }).and_then(mcp_json)?;
    if preset.transport.needs_sign_in() {
        result["next"] = t!("mcp.next_sign_in").into_owned().into();
    } else if let Some(url) = preset.extension_url {
        result["next"] = t!("mcp.next_extension", url = url).into_owned().into();
    }
    Ok(result)
}

/// `smelt pi skill|extension …`：Pi 的技能（带 SKILL.md 的目录）和扩展（注册工具、命令的
/// TypeScript 模块）。两类分开管，各自只列、只删自己那类。
pub(crate) fn run(args: &[String]) -> i32 {
    let kind = match args.first().map(String::as_str) {
        Some("skill") => PiPluginKind::Skill,
        Some("extension") => PiPluginKind::Extension,
        Some(command) => return usage(&t!("unknown_subcommand", command = command)),
        None => return usage("smelt pi skill|extension list|import|remove"),
    };
    let rest = args.get(2..).unwrap_or_default();
    match args.get(1).map(String::as_str) {
        Some("list") => respond(config().map(|config| catalog_json(config, kind))),
        Some("import") => match import_params(kind, rest) {
            Ok(params) => {
                respond(control::<PiPluginsImport>(params).map(|config| catalog_json(config, kind)))
            }
            Err(message) => usage(&message),
        },
        Some("remove") => match id_arg(rest) {
            Some(name) => respond(
                control::<PiPluginsRemove>(PluginIdParams {
                    id: catalog_id(kind, name),
                })
                .map(|config| catalog_json(config, kind)),
            ),
            None => usage(&format!("smelt pi {} remove <name>", kind.as_str())),
        },
        Some(command) => usage(&t!("unknown_subcommand", command = command)),
        None => usage(&format!("smelt pi {} list|import|remove", kind.as_str())),
    }
}

/// 目录里的 id 是 `<类别>:<名字>`；命令行写名字或整个 id 都行。
fn catalog_id(kind: PiPluginKind, name: &str) -> String {
    let prefix = format!("{}:", kind.as_str());
    if name.starts_with(&prefix) {
        name.to_string()
    } else {
        format!("{prefix}{name}")
    }
}

/// 只列这一类。`id` 就是给智能体 `--plugin` 用的那个。
fn catalog_json(config: PiConfig, kind: PiPluginKind) -> serde_json::Value {
    let items: Vec<_> = config
        .plugins
        .into_iter()
        .filter(|item| item.kind == kind)
        .map(|item| {
            serde_json::json!({
                "id": item.id,
                "name": item.name,
                "description": item.description,
                "origin": item.origin,
                "builtin": item.builtin,
                "broken": item.broken,
            })
        })
        .collect();
    let key = match kind {
        PiPluginKind::Skill => "skills",
        PiPluginKind::Extension => "extensions",
    };
    serde_json::json!({ key: items })
}

/// 运行时在本机，相对路径按当前目录补全（运行时的工作目录不是这里）。
fn import_params(kind: PiPluginKind, args: &[String]) -> Result<ImportPluginParams, String> {
    let [source] = args else {
        return Err(format!("smelt pi {} import <path>", kind.as_str()));
    };
    let source = std::path::absolute(source)
        .map_err(|error| t!("pi.invalid_path", path = source, error = error).into_owned())?;
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
            None => usage(&t!("model.default_usage")),
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
        Some(command) => usage(&t!("unknown_subcommand", command = command)),
        None => usage("smelt model list|default|thinking"),
    }
}

/// 只给默认模型和可选模型，不带 provider 配置（里面可能有地址和凭据状态）。
fn models_json(config: PiConfig) -> Result<serde_json::Value, String> {
    let summary = config
        .settings_summary
        .map_err(|error| t!("model.settings_unreadable", error = error).into_owned())?;
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
        .ok_or_else(|| t!("model.format").into_owned())?;
    let current = config()?;
    if !has_model(&current, provider, model) {
        return Err(t!("no_such_model", provider = provider, model = model).into_owned());
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
        let params = import_params(PiPluginKind::Skill, &args(&["skills/writer"])).unwrap();
        assert_eq!(params.kind, PiPluginKind::Skill);
        assert!(std::path::Path::new(&params.source).is_absolute());
        assert!(params.source.ends_with("writer"));
        assert!(import_params(PiPluginKind::Skill, &args(&[])).is_err());
        assert!(import_params(PiPluginKind::Extension, &args(&["a", "b"])).is_err());
    }

    #[test]
    fn remove_takes_a_name_or_the_whole_id() {
        assert_eq!(catalog_id(PiPluginKind::Skill, "writer"), "skill:writer");
        assert_eq!(
            catalog_id(PiPluginKind::Skill, "skill:writer"),
            "skill:writer"
        );
        assert_eq!(
            catalog_id(PiPluginKind::Extension, "skill:x"),
            "extension:skill:x"
        );
    }

    #[test]
    fn every_preset_can_be_found_by_id() {
        let presets = presets_json();
        for preset in presets["presets"].as_array().unwrap() {
            assert!(find_preset(preset["id"].as_str().unwrap()).is_some());
        }
    }
}
