//! Pi 原生模型设置。
//!
//! 管理 Pi 位于 `~/.pi/agent/` 下的配置：
//! - `settings.json`：默认 provider、默认 model、默认 thinking level 等全局选项
//! - `models.json`：自定义 provider、网关端点、兼容模式及模型定义
//! - `auth.json`：各 provider 的 API key 等凭据（采用 0600 权限保护）

use std::path::{Path, PathBuf};

/// 获取 Pi Agent 配置根目录。优先读取 `PI_CODING_AGENT_DIR` 环境变量，
/// 默认退回 `~/.pi/agent`。
pub fn pi_agent_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("PI_CODING_AGENT_DIR")
        && !dir.trim().is_empty()
    {
        return PathBuf::from(crate::workspace_override::expand_tilde(&dir));
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".pi")
        .join("agent")
}

pub fn pi_settings_path() -> PathBuf {
    pi_agent_dir().join("settings.json")
}

pub fn pi_models_path() -> PathBuf {
    pi_agent_dir().join("models.json")
}

pub fn pi_auth_path() -> PathBuf {
    pi_agent_dir().join("auth.json")
}

/// `auth.json` 里一条凭据的形态。
///
/// Pi 在同一个文件、同一个 provider 键下存两种东西：`/login` 选 API key 存的是
/// `{"type":"api_key","key":...}`，选订阅登录存的是
/// `{"type":"oauth","access":...,"refresh":...,"expires":...}`。只看 `key` 字段
/// 会把所有 OAuth 登录过的 provider 报成「缺少凭据」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiCredentialKind {
    ApiKey,
    OAuth,
}

/// 判断一条 `auth.json` 条目算不算「已配置凭据」，并给出它是哪一种。
///
/// 认 `type` 字段，但不强求：Pi 早期写下的条目可能只有 `key`。OAuth 条目以
/// `refresh` 或 `access` 存在为准——过期与否这里不判，刷新是 Pi 的事，而把一
/// 个只是 access token 过期的账号显示成「未登录」会让人以为要重新登录。
pub fn pi_credential_kind(entry: &serde_json::Value) -> Option<PiCredentialKind> {
    let non_empty = |field: &str| {
        entry
            .get(field)
            .and_then(|v| v.as_str())
            .is_some_and(|v| !v.trim().is_empty())
    };
    match entry.get("type").and_then(|t| t.as_str()) {
        Some("oauth") => {
            (non_empty("refresh") || non_empty("access")).then_some(PiCredentialKind::OAuth)
        }
        Some("api_key") | None => non_empty("key").then_some(PiCredentialKind::ApiKey),
        // 认不出的 type 也可能带着一份能用的凭据（Pi 更新过、我们还没跟上）。
        Some(_) => {
            if non_empty("key") {
                Some(PiCredentialKind::ApiKey)
            } else if non_empty("refresh") || non_empty("access") {
                Some(PiCredentialKind::OAuth)
            } else {
                None
            }
        }
    }
}

/// 某个 provider 在 `auth.json` 里有没有凭据。
fn auth_entry_configured(auth_json: &serde_json::Value, provider_id: &str) -> bool {
    auth_json
        .get(provider_id)
        .and_then(pi_credential_kind)
        .is_some()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiDefaultModelSummary {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking_level: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiSettingsSummary {
    pub path: PathBuf,
    pub default_model: Option<PiDefaultModelSummary>,
}

/// 读取 `settings.json` 概况用于展示当前默认模型与文件位置。
pub fn pi_settings_summary() -> Result<PiSettingsSummary, String> {
    pi_settings_summary_at(&pi_agent_dir())
}

pub fn pi_settings_summary_at(agent_dir: &Path) -> Result<PiSettingsSummary, String> {
    let path = agent_dir.join("settings.json");
    if !path.is_file() {
        return Ok(PiSettingsSummary {
            path,
            default_model: None,
        });
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) => return Err(format!("读取 {} 失败：{e}", path.display())),
    };
    if raw.trim().is_empty() {
        return Ok(PiSettingsSummary {
            path,
            default_model: None,
        });
    }
    let val: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("解析 {} 失败：{e}", path.display()))?;
    let provider = val
        .get("defaultProvider")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let model = val
        .get("defaultModel")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let thinking_level = val
        .get("defaultThinkingLevel")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let default_model = if provider.is_some() || model.is_some() || thinking_level.is_some() {
        Some(PiDefaultModelSummary {
            provider,
            model,
            thinking_level,
        })
    } else {
        None
    };

    Ok(PiSettingsSummary {
        path,
        default_model,
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiDefaultModelConfig {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub thinking_level: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiCustomModel {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiCustomProvider {
    pub id: String,
    pub display_name: String,
    pub api: String,
    #[serde(rename = "baseUrl", default)]
    pub base_url: String,
    #[serde(default)]
    pub credential_configured: bool,
    #[serde(default)]
    pub models: Vec<PiCustomModel>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiModelSettings {
    pub default_model: PiDefaultModelConfig,
    pub credential_configured: bool,
    #[serde(default)]
    pub custom_providers: Vec<PiCustomProvider>,
}

/// 智能体定义页下拉用的一条模型。不依赖正在跑的 Pi 会话。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiModelChoice {
    pub provider: String,
    pub provider_name: String,
    pub id: String,
    pub name: String,
}

impl PiModelChoice {
    pub fn label(&self) -> String {
        if self.name.trim().is_empty() || self.name == self.id {
            format!("{} / {}", self.provider_name, self.id)
        } else {
            format!("{} / {}", self.provider_name, self.name)
        }
    }

    pub fn short_label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}

/// 下拉里按 provider 分组，避免把所有模型摊成一条长名单。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiModelChoiceGroup {
    pub provider: String,
    pub provider_name: String,
    pub models: Vec<PiModelChoice>,
}

/// 从 `models.json` 和 Pi 的 `models-store.json` 拼一份可选模型目录。
/// `models-store` 只收已经配了凭据或自定义过的 provider，缓存里的其它目录不进名单。
pub fn list_pi_model_choices() -> Vec<PiModelChoice> {
    list_pi_model_choices_at(&pi_agent_dir())
}

pub fn list_pi_model_choice_groups() -> Vec<PiModelChoiceGroup> {
    group_pi_model_choices(list_pi_model_choices())
}

pub fn group_pi_model_choices(choices: Vec<PiModelChoice>) -> Vec<PiModelChoiceGroup> {
    let mut groups: Vec<PiModelChoiceGroup> = Vec::new();
    for choice in choices {
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.provider == choice.provider)
        {
            group.models.push(choice);
        } else {
            groups.push(PiModelChoiceGroup {
                provider: choice.provider.clone(),
                provider_name: choice.provider_name.clone(),
                models: vec![choice],
            });
        }
    }
    groups
}

pub fn list_pi_model_choices_at(agent_dir: &Path) -> Vec<PiModelChoice> {
    let mut choices = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut push = |choice: PiModelChoice| {
        if choice.provider.trim().is_empty() || choice.id.trim().is_empty() {
            return;
        }
        if seen.insert((choice.provider.clone(), choice.id.clone())) {
            choices.push(choice);
        }
    };

    let mut store_providers = std::collections::BTreeSet::new();
    if let Ok(settings) = load_pi_model_settings_at(agent_dir) {
        for provider in settings.custom_providers {
            store_providers.insert(provider.id.clone());
            let provider_name = if provider.display_name.trim().is_empty() {
                provider.id.clone()
            } else {
                provider.display_name.clone()
            };
            for model in provider.models {
                let name = if model.name.trim().is_empty() {
                    model.id.clone()
                } else {
                    model.name.clone()
                };
                push(PiModelChoice {
                    provider: provider.id.clone(),
                    provider_name: provider_name.clone(),
                    id: model.id,
                    name,
                });
            }
        }
        let default = &settings.default_model;
        if !default.provider.trim().is_empty() && !default.model.trim().is_empty() {
            store_providers.insert(default.provider.clone());
            push(PiModelChoice {
                provider: default.provider.clone(),
                provider_name: default.provider.clone(),
                id: default.model.clone(),
                name: default.model.clone(),
            });
        }
    }

    let auth_path = agent_dir.join("auth.json");
    if let Ok(raw) = std::fs::read_to_string(&auth_path)
        && let Ok(serde_json::Value::Object(auth)) = serde_json::from_str::<serde_json::Value>(&raw)
    {
        for (provider_id, entry) in auth {
            if pi_credential_kind(&entry).is_some() {
                store_providers.insert(provider_id);
            }
        }
    }

    let store_path = agent_dir.join("models-store.json");
    if let Ok(raw) = std::fs::read_to_string(&store_path)
        && let Ok(serde_json::Value::Object(root)) = serde_json::from_str::<serde_json::Value>(&raw)
    {
        for (provider_id, entry) in root {
            if !store_providers.contains(&provider_id) {
                continue;
            }
            let provider_name = entry
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(&provider_id)
                .to_string();
            let Some(models) = entry.get("models").and_then(|v| v.as_array()) else {
                continue;
            };
            for model in models {
                let Some(id) = model.get("id").and_then(|v| v.as_str()).map(str::trim) else {
                    continue;
                };
                if id.is_empty() {
                    continue;
                }
                let name = model
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id)
                    .to_string();
                push(PiModelChoice {
                    provider: provider_id.clone(),
                    provider_name: provider_name.clone(),
                    id: id.to_string(),
                    name,
                });
            }
        }
    }

    choices.sort_by(|left, right| {
        left.provider_name
            .to_ascii_lowercase()
            .cmp(&right.provider_name.to_ascii_lowercase())
            .then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            })
    });
    choices
}

/// 读取全部 Pi 模型设置，包含默认模型、凭据配置状态与自定义 Provider 列表。
pub fn load_pi_model_settings() -> Result<PiModelSettings, String> {
    load_pi_model_settings_at(&pi_agent_dir())
}

pub fn load_pi_model_settings_at(agent_dir: &Path) -> Result<PiModelSettings, String> {
    let settings_path = agent_dir.join("settings.json");
    let models_path = agent_dir.join("models.json");
    let auth_path = agent_dir.join("auth.json");

    let settings_json: serde_json::Value = if settings_path.is_file() {
        let raw = std::fs::read_to_string(&settings_path)
            .map_err(|e| format!("读取 {} 失败：{e}", settings_path.display()))?;
        serde_json::from_str(&raw).unwrap_or(serde_json::Value::Object(Default::default()))
    } else {
        serde_json::Value::Object(Default::default())
    };

    let auth_json: serde_json::Value = if auth_path.is_file() {
        let raw = std::fs::read_to_string(&auth_path)
            .map_err(|e| format!("读取 {} 失败：{e}", auth_path.display()))?;
        serde_json::from_str(&raw).unwrap_or(serde_json::Value::Object(Default::default()))
    } else {
        serde_json::Value::Object(Default::default())
    };

    let models_json: serde_json::Value = if models_path.is_file() {
        let raw = std::fs::read_to_string(&models_path)
            .map_err(|e| format!("读取 {} 失败：{e}", models_path.display()))?;
        serde_json::from_str(&raw).unwrap_or(serde_json::Value::Object(Default::default()))
    } else {
        serde_json::Value::Object(Default::default())
    };

    let default_provider = settings_json
        .get("defaultProvider")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("deepseek")
        .to_string();

    let default_model = settings_json
        .get("defaultModel")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("deepseek-chat")
        .to_string();

    let default_thinking = settings_json
        .get("defaultThinkingLevel")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("medium")
        .to_string();

    // 检查默认 Provider 是否已配置凭据
    let mut credential_configured = auth_entry_configured(&auth_json, &default_provider);
    // 环境变量兜底
    if !credential_configured {
        credential_configured = match default_provider.as_str() {
            "deepseek" => std::env::var("DEEPSEEK_API_KEY").is_ok_and(|k| !k.trim().is_empty()),
            "anthropic" => std::env::var("ANTHROPIC_API_KEY").is_ok_and(|k| !k.trim().is_empty()),
            "openai" => std::env::var("OPENAI_API_KEY").is_ok_and(|k| !k.trim().is_empty()),
            "google" => std::env::var("GEMINI_API_KEY").is_ok_and(|k| !k.trim().is_empty()),
            _ => false,
        };
    }

    // 从 models.json 提取默认 Provider 的 baseUrl（若有覆写）
    let mut default_base_url = String::new();
    if let Some(providers_obj) = models_json.get("providers").and_then(|p| p.as_object())
        && let Some(provider_entry) = providers_obj.get(&default_provider)
    {
        if let Some(b) = provider_entry.get("baseUrl").and_then(|b| b.as_str()) {
            default_base_url = b.to_string();
        }
        if !credential_configured
            && let Some(k) = provider_entry.get("apiKey").and_then(|k| k.as_str())
            && !k.trim().is_empty()
        {
            credential_configured = true;
        }
    }

    // 从 models.json 提取自定义 Provider 列表
    let mut custom_providers = Vec::new();
    if let Some(providers_obj) = models_json.get("providers").and_then(|p| p.as_object()) {
        for (provider_id, provider_val) in providers_obj {
            let is_builtin = matches!(
                provider_id.as_str(),
                "deepseek" | "anthropic" | "openai" | "google"
            );
            let models_array = provider_val.get("models").and_then(|m| m.as_array());
            if is_builtin && models_array.is_none() {
                // 仅是对内置官方 provider 的 baseUrl 覆写，不作为自定义 provider 呈现
                continue;
            }
            let display_name = provider_val
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or(provider_id.as_str())
                .to_string();
            let api = provider_val
                .get("api")
                .and_then(|a| a.as_str())
                .unwrap_or("openai-completions")
                .to_string();
            let base_url = provider_val
                .get("baseUrl")
                .and_then(|b| b.as_str())
                .unwrap_or("")
                .to_string();

            let mut cred_configured = auth_entry_configured(&auth_json, provider_id);
            if !cred_configured
                && let Some(k) = provider_val.get("apiKey").and_then(|k| k.as_str())
                && !k.trim().is_empty()
            {
                cred_configured = true;
            }

            let models = models_array
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| {
                            let id = m.get("id")?.as_str()?.to_string();
                            let name = m
                                .get("name")
                                .and_then(|n| n.as_str())
                                .filter(|n| !n.trim().is_empty())
                                .unwrap_or(&id)
                                .to_string();
                            let context_window = m.get("contextWindow").and_then(|c| c.as_u64());
                            let max_tokens = m.get("maxTokens").and_then(|c| c.as_u64());
                            Some(PiCustomModel {
                                id,
                                name,
                                context_window,
                                max_tokens,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

            custom_providers.push(PiCustomProvider {
                id: provider_id.clone(),
                display_name,
                api,
                base_url,
                credential_configured: cred_configured,
                models,
            });
        }
    }

    Ok(PiModelSettings {
        default_model: PiDefaultModelConfig {
            provider: default_provider,
            model: default_model,
            base_url: default_base_url,
            thinking_level: default_thinking,
        },
        credential_configured,
        custom_providers,
    })
}

fn read_or_create_json(path: &Path) -> Result<serde_json::Value, String> {
    if path.is_file() {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
        serde_json::from_str(&raw).map_err(|e| format!("解析 {} 失败：{e}", path.display()))
    } else {
        Ok(serde_json::Value::Object(Default::default()))
    }
}

fn write_json_file(path: &Path, value: &serde_json::Value, secure: bool) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录 {} 失败：{e}", parent.display()))?;
    }
    let content =
        serde_json::to_string_pretty(value).map_err(|e| format!("序列化 JSON 失败：{e}"))?;

    let tmp_path = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        use std::io::Write as _;
        let mut file = std::fs::File::create(&tmp_path)
            .map_err(|e| format!("创建临时文件 {} 失败：{e}", tmp_path.display()))?;
        #[cfg(unix)]
        if secure {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        file.write_all(content.as_bytes())
            .map_err(|e| format!("写入临时文件 {} 失败：{e}", tmp_path.display()))?;
        file.flush()
            .map_err(|e| format!("刷新临时文件 {} 失败：{e}", tmp_path.display()))?;
    }
    std::fs::rename(&tmp_path, path)
        .map_err(|e| format!("覆盖写入 {} 失败：{e}", path.display()))?;
    Ok(())
}

/// 保存默认模型配置及可选 API Key。
pub fn save_pi_default_model(
    config: &PiDefaultModelConfig,
    api_key: Option<&str>,
) -> Result<(), String> {
    save_pi_default_model_at(&pi_agent_dir(), config, api_key)
}

pub fn save_pi_default_model_at(
    agent_dir: &Path,
    config: &PiDefaultModelConfig,
    api_key: Option<&str>,
) -> Result<(), String> {
    let settings_path = agent_dir.join("settings.json");
    let auth_path = agent_dir.join("auth.json");
    let models_path = agent_dir.join("models.json");

    let mut settings = read_or_create_json(&settings_path)?;
    if let Some(obj) = settings.as_object_mut() {
        obj.insert(
            "defaultProvider".to_string(),
            serde_json::json!(config.provider.trim()),
        );
        obj.insert(
            "defaultModel".to_string(),
            serde_json::json!(config.model.trim()),
        );
        if !config.thinking_level.trim().is_empty() {
            obj.insert(
                "defaultThinkingLevel".to_string(),
                serde_json::json!(config.thinking_level.trim()),
            );
        }
    }
    write_json_file(&settings_path, &settings, false)?;

    if let Some(key) = api_key {
        let key = key.trim();
        if !key.is_empty() {
            let mut auth = read_or_create_json(&auth_path)?;
            if let Some(obj) = auth.as_object_mut() {
                obj.insert(
                    config.provider.trim().to_string(),
                    serde_json::json!({
                        "type": "api_key",
                        "key": key
                    }),
                );
            }
            write_json_file(&auth_path, &auth, true)?;
        }
    }

    let base_url = config.base_url.trim();
    let mut models = read_or_create_json(&models_path)?;
    let mut models_changed = false;
    if let Some(models_obj) = models.as_object_mut() {
        let providers = models_obj
            .entry("providers")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(providers_obj) = providers.as_object_mut() {
            if !base_url.is_empty() {
                let entry = providers_obj
                    .entry(config.provider.trim())
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(entry_obj) = entry.as_object_mut() {
                    entry_obj.insert("baseUrl".to_string(), serde_json::json!(base_url));
                    models_changed = true;
                }
            } else if let Some(entry) = providers_obj.get_mut(config.provider.trim())
                && let Some(entry_obj) = entry.as_object_mut()
            {
                if entry_obj.remove("baseUrl").is_some() {
                    models_changed = true;
                }
                if entry_obj.is_empty() {
                    providers_obj.remove(config.provider.trim());
                    models_changed = true;
                }
            }
        }
    }
    if models_changed {
        write_json_file(&models_path, &models, false)?;
    }

    Ok(())
}

/// 自定义 Provider 在 `models.json` 里的键名。
///
/// Pi 把这个字符串当对象键用，大小写敏感、本身几乎不限制字符。我们只拦空键、空白、
/// 以及会把 JSON 键弄乱的符号；`Token-X` 这种大小写混写必须能过——用户填的就是这个。
pub fn validate_custom_provider_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("请填写 Provider ID".to_string());
    }
    let valid = id.chars().next().is_some_and(|ch| ch.is_ascii_alphabetic())
        && id
            .split('-')
            .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_alphanumeric()));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "Provider ID「{id}」无效：须以字母开头，只能含字母、数字和短连字符"
        ))
    }
}

/// 保存或添加自定义 Provider 配置。
pub fn save_pi_custom_provider(
    provider: &PiCustomProvider,
    api_key: Option<&str>,
    set_default: bool,
) -> Result<(), String> {
    save_pi_custom_provider_at(&pi_agent_dir(), provider, api_key, set_default)
}

pub fn save_pi_custom_provider_at(
    agent_dir: &Path,
    provider: &PiCustomProvider,
    api_key: Option<&str>,
    set_default: bool,
) -> Result<(), String> {
    let models_path = agent_dir.join("models.json");
    let auth_path = agent_dir.join("auth.json");
    let settings_path = agent_dir.join("settings.json");

    let mut models = read_or_create_json(&models_path)?;
    if let Some(models_obj) = models.as_object_mut() {
        let providers = models_obj
            .entry("providers")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(providers_obj) = providers.as_object_mut() {
            let model_items: Vec<serde_json::Value> = provider
                .models
                .iter()
                .map(|m| {
                    let id = m.id.trim();
                    let name = if m.name.trim().is_empty() {
                        id
                    } else {
                        m.name.trim()
                    };
                    let mut item = serde_json::json!({
                        "id": id,
                        "name": name
                    });
                    if let Some(cw) = m.context_window {
                        item.as_object_mut()
                            .unwrap()
                            .insert("contextWindow".to_string(), serde_json::json!(cw));
                    }
                    if let Some(mt) = m.max_tokens {
                        item.as_object_mut()
                            .unwrap()
                            .insert("maxTokens".to_string(), serde_json::json!(mt));
                    }
                    item
                })
                .collect();

            let display_name = if provider.display_name.trim().is_empty() {
                provider.id.trim()
            } else {
                provider.display_name.trim()
            };
            let mut entry = serde_json::json!({
                "name": display_name,
                "api": provider.api.trim(),
                "models": model_items
            });
            if !provider.base_url.trim().is_empty() {
                entry.as_object_mut().unwrap().insert(
                    "baseUrl".to_string(),
                    serde_json::json!(provider.base_url.trim()),
                );
            }
            providers_obj.insert(provider.id.trim().to_string(), entry);
        }
    }
    write_json_file(&models_path, &models, false)?;

    if let Some(key) = api_key {
        let key = key.trim();
        if !key.is_empty() {
            let mut auth = read_or_create_json(&auth_path)?;
            if let Some(obj) = auth.as_object_mut() {
                obj.insert(
                    provider.id.trim().to_string(),
                    serde_json::json!({
                        "type": "api_key",
                        "key": key
                    }),
                );
            }
            write_json_file(&auth_path, &auth, true)?;
        }
    }

    let should_set_default = set_default || !settings_path.is_file() || {
        read_or_create_json(&settings_path)
            .ok()
            .and_then(|val| {
                val.get("defaultProvider")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().is_empty())
            })
            .unwrap_or(true)
    };

    if should_set_default {
        let mut settings = read_or_create_json(&settings_path)?;
        if let Some(obj) = settings.as_object_mut() {
            obj.insert(
                "defaultProvider".to_string(),
                serde_json::json!(provider.id.trim()),
            );
            if let Some(first_model) = provider.models.first() {
                obj.insert(
                    "defaultModel".to_string(),
                    serde_json::json!(first_model.id.trim()),
                );
            }
        }
        write_json_file(&settings_path, &settings, false)?;
    }

    Ok(())
}

/// 读一个自定义 provider 的 API key。
///
/// `auth.json` 是 Pi 存凭据的正路，但 `models.json` 的 provider 节点上也可以直接
/// 写 `apiKey`（Pi 两处都认）。自动刷新目录时要拿它去请求端点，两处都得看，否则
/// 只在 `models.json` 里配了 key 的用户会得到一次 401。
pub fn read_pi_provider_api_key_at(agent_dir: &Path, provider_id: &str) -> Option<String> {
    let read = |path: PathBuf| -> Option<serde_json::Value> {
        let raw = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&raw).ok()
    };
    let from_auth = read(agent_dir.join("auth.json")).and_then(|auth| {
        auth.get(provider_id)?
            .get("key")?
            .as_str()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string)
    });
    from_auth.or_else(|| {
        read(agent_dir.join("models.json")).and_then(|models| {
            models
                .get("providers")?
                .get(provider_id)?
                .get("apiKey")?
                .as_str()
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .map(str::to_string)
        })
    })
}

/// 只替换一个已存在 provider 的 `models` 数组，其余字段（名字、协议、baseUrl、
/// apiKey）原样保留。
///
/// 自动刷新用它。**不会**新建 provider：provider 已经不在配置里时刷新是无意义
/// 的，凭一份端点回答把它重新造出来更糟。
pub fn replace_pi_provider_models_at(
    agent_dir: &Path,
    provider_id: &str,
    models: &[crate::provider_api::DiscoveredModel],
) -> Result<(), String> {
    let models_path = agent_dir.join("models.json");
    let mut root = read_or_create_json(&models_path)?;
    let entry = root
        .get_mut("providers")
        .and_then(|providers| providers.get_mut(provider_id))
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| format!("{provider_id} 不在 {} 中", models_path.display()))?;
    let items: Vec<serde_json::Value> = models
        .iter()
        .map(|model| {
            let mut item = serde_json::json!({ "id": model.id, "name": model.label() });
            let obj = item.as_object_mut().expect("just built an object");
            // 网关不报告输入能力（OpenAI 兼容协议没有这个字段），Pi 对未声明 `input`
            // 的模型默认按纯文本处理、直接丢弃图片。这里统一写 [`INPUT_TEXT_IMAGE`]
            // 兼容声明：拦截在客户端解除，模型真实能力由服务端回答，报错时用户能
            // 看到明确信息而不是图片被静默省略。
            obj.insert(
                "input".to_string(),
                serde_json::json!(crate::provider_api::INPUT_TEXT_IMAGE),
            );
            if let Some(context_window) = model.context_window {
                obj.insert(
                    "contextWindow".to_string(),
                    serde_json::json!(context_window),
                );
            }
            if let Some(max_tokens) = model.max_tokens {
                obj.insert("maxTokens".to_string(), serde_json::json!(max_tokens));
            }
            item
        })
        .collect();
    entry.insert("models".to_string(), serde_json::Value::Array(items));
    write_json_file(&models_path, &root, false)
}

/// 移除指定的自定义 Provider。
pub fn remove_pi_custom_provider(provider_id: &str) -> Result<(), String> {
    remove_pi_custom_provider_at(&pi_agent_dir(), provider_id)
}

pub fn remove_pi_custom_provider_at(agent_dir: &Path, provider_id: &str) -> Result<(), String> {
    let models_path = agent_dir.join("models.json");
    let auth_path = agent_dir.join("auth.json");

    let mut models = read_or_create_json(&models_path)?;
    if let Some(models_obj) = models.as_object_mut()
        && let Some(providers) = models_obj
            .get_mut("providers")
            .and_then(|p| p.as_object_mut())
    {
        providers.remove(provider_id);
    }
    write_json_file(&models_path, &models, false)?;

    if auth_path.is_file() {
        let mut auth = read_or_create_json(&auth_path)?;
        if let Some(obj) = auth.as_object_mut()
            && obj.remove(provider_id).is_some()
        {
            write_json_file(&auth_path, &auth, true)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::provider_api::DiscoveredModel;

    /// `/login` 选订阅登录写下的是 OAuth 令牌，不是 `key`。只认 `key` 会把每个
    /// 登录过 GitHub Copilot、Claude Pro 的用户都报成「缺少凭据」。
    #[test]
    fn an_oauth_login_counts_as_a_configured_credential() {
        let oauth = serde_json::json!({
            "type": "oauth",
            "refresh": "r",
            "access": "a",
            "expires": 1_760_000_000_000u64
        });
        assert_eq!(pi_credential_kind(&oauth), Some(PiCredentialKind::OAuth));
        // access 过期与否不在这里判：刷新是 Pi 的事，把只是令牌过期的账号显示
        // 成「未登录」会让人以为要重新登录。
        let expired =
            serde_json::json!({ "type": "oauth", "refresh": "r", "access": "", "expires": 1 });
        assert_eq!(pi_credential_kind(&expired), Some(PiCredentialKind::OAuth));
    }

    #[test]
    fn api_keys_and_empty_entries_are_told_apart() {
        assert_eq!(
            pi_credential_kind(&serde_json::json!({ "type": "api_key", "key": "sk-x" })),
            Some(PiCredentialKind::ApiKey)
        );
        // 早期条目可能没有 type。
        assert_eq!(
            pi_credential_kind(&serde_json::json!({ "key": "sk-x" })),
            Some(PiCredentialKind::ApiKey)
        );
        assert_eq!(
            pi_credential_kind(&serde_json::json!({ "type": "api_key", "key": "  " })),
            None
        );
        assert_eq!(
            pi_credential_kind(&serde_json::json!({ "type": "oauth" })),
            None
        );
        assert_eq!(pi_credential_kind(&serde_json::json!({})), None);
        // 认不出的 type 也可能带着一份能用的凭据（Pi 更新过、我们还没跟上）。
        assert_eq!(
            pi_credential_kind(&serde_json::json!({ "type": "device", "access": "a" })),
            Some(PiCredentialKind::OAuth)
        );
    }

    #[test]
    fn settings_report_oauth_logins_as_configured() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("settings.json"),
            r#"{"defaultProvider":"github-copilot","defaultModel":"gpt-5"}"#,
        )
        .unwrap();
        std::fs::write(
            temp.path().join("auth.json"),
            r#"{"github-copilot":{"type":"oauth","refresh":"r","access":"a","expires":1}}"#,
        )
        .unwrap();
        let settings = load_pi_model_settings_at(temp.path()).unwrap();
        assert!(settings.credential_configured);
    }

    fn discovered(id: &str) -> DiscoveredModel {
        DiscoveredModel {
            id: id.to_string(),
            name: None,
            context_window: None,
            max_tokens: None,
        }
    }

    /// 刷新目录只能动 `models`。把用户填的协议/地址/凭据一起覆盖掉，等于每次
    /// 启动都悄悄改坏他的配置。
    #[test]
    fn refreshing_models_keeps_every_other_provider_field() {
        let temp = tempfile::tempdir().unwrap();
        let provider = PiCustomProvider {
            id: "gw".to_string(),
            display_name: "网关".to_string(),
            api: "openai-responses".to_string(),
            base_url: "https://gw.example.com/v1".to_string(),
            credential_configured: false,
            models: vec![PiCustomModel {
                id: "old".to_string(),
                name: "Old".to_string(),
                context_window: None,
                max_tokens: None,
            }],
        };
        save_pi_custom_provider_at(temp.path(), &provider, Some("sk-test"), false).unwrap();

        let fresh = vec![
            DiscoveredModel {
                id: "new-a".to_string(),
                name: Some("New A".to_string()),
                context_window: Some(1_000_000),
                max_tokens: Some(16384),
            },
            discovered("new-b"),
        ];
        replace_pi_provider_models_at(temp.path(), "gw", &fresh).unwrap();

        let reloaded = load_pi_model_settings_at(temp.path()).unwrap();
        let entry = reloaded
            .custom_providers
            .iter()
            .find(|candidate| candidate.id == "gw")
            .expect("provider survives a refresh");
        assert_eq!(entry.api, "openai-responses");
        assert_eq!(entry.base_url, "https://gw.example.com/v1");
        let ids: Vec<&str> = entry.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["new-a", "new-b"]);
        assert_eq!(entry.models[0].context_window, Some(1_000_000));
        // 端点不报告输入能力，落盘也要带上乐观的 input 声明，否则 Pi 会把图片丢在客户端。
        let raw = std::fs::read_to_string(temp.path().join("models.json")).unwrap();
        let root: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let persisted = &root["providers"]["gw"]["models"];
        for model in persisted.as_array().unwrap() {
            assert_eq!(
                model["input"],
                serde_json::json!(["text", "image"]),
                "落盘的每个模型都应携带 input 声明：{}",
                model["id"]
            );
        }
        // 端点没给名字的，退回 id，不能落一个空名字。
        assert_eq!(entry.models[1].name, "new-b");
        assert_eq!(
            read_pi_provider_api_key_at(temp.path(), "gw").as_deref(),
            Some("sk-test")
        );
    }

    /// provider 已经被删掉了就不该被一次刷新重新造出来。
    #[test]
    fn refreshing_an_unknown_provider_is_an_error_not_an_insert() {
        let temp = tempfile::tempdir().unwrap();
        assert!(replace_pi_provider_models_at(temp.path(), "ghost", &[discovered("m")]).is_err());
    }

    /// key 直接写在 models.json 的 provider 节点上也算配了。
    #[test]
    fn api_key_is_read_from_models_json_too() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("models.json"),
            r#"{"providers":{"gw":{"apiKey":"sk-inline"}}}"#,
        )
        .unwrap();
        assert_eq!(
            read_pi_provider_api_key_at(temp.path(), "gw").as_deref(),
            Some("sk-inline")
        );
        assert_eq!(read_pi_provider_api_key_at(temp.path(), "other"), None);
    }

    #[test]
    fn summary_of_nonexistent_returns_none() {
        let temp = tempfile::tempdir().unwrap();
        let summary = pi_settings_summary_at(temp.path()).unwrap();
        assert_eq!(summary.default_model, None);
        assert_eq!(summary.path, temp.path().join("settings.json"));
    }

    #[test]
    fn loads_defaults_and_saves_default_model() {
        let temp = tempfile::tempdir().unwrap();
        let settings = load_pi_model_settings_at(temp.path()).unwrap();
        assert_eq!(settings.default_model.provider, "deepseek");
        assert_eq!(settings.default_model.model, "deepseek-chat");
        assert_eq!(settings.default_model.thinking_level, "medium");
        let environment_credential_configured =
            std::env::var("DEEPSEEK_API_KEY").is_ok_and(|key| !key.trim().is_empty());
        assert_eq!(
            settings.credential_configured,
            environment_credential_configured
        );

        let config = PiDefaultModelConfig {
            provider: "deepseek".to_string(),
            model: "deepseek-reasoner".to_string(),
            base_url: "https://api.deepseek.com".to_string(),
            thinking_level: "high".to_string(),
        };
        save_pi_default_model_at(temp.path(), &config, Some("sk-test-123456")).unwrap();

        let updated = load_pi_model_settings_at(temp.path()).unwrap();
        assert_eq!(updated.default_model.provider, "deepseek");
        assert_eq!(updated.default_model.model, "deepseek-reasoner");
        assert_eq!(updated.default_model.base_url, "https://api.deepseek.com");
        assert_eq!(updated.default_model.thinking_level, "high");
        assert!(updated.credential_configured);

        let summary = pi_settings_summary_at(temp.path()).unwrap();
        let dm = summary.default_model.unwrap();
        assert_eq!(dm.provider.as_deref(), Some("deepseek"));
        assert_eq!(dm.model.as_deref(), Some("deepseek-reasoner"));
        assert_eq!(dm.thinking_level.as_deref(), Some("high"));
    }

    #[test]
    fn manages_custom_providers() {
        let temp = tempfile::tempdir().unwrap();
        let provider = PiCustomProvider {
            id: "my-gateway".to_string(),
            display_name: "My Gateway".to_string(),
            api: "openai-completions".to_string(),
            base_url: "https://gateway.example.com/v1".to_string(),
            credential_configured: false,
            models: vec![
                PiCustomModel {
                    id: "model-a".to_string(),
                    name: "Model A".to_string(),
                    context_window: Some(128000),
                    max_tokens: Some(4096),
                },
                PiCustomModel {
                    id: "model-b".to_string(),
                    name: "Model B".to_string(),
                    context_window: None,
                    max_tokens: None,
                },
            ],
        };

        save_pi_custom_provider_at(temp.path(), &provider, Some("key-gw-999"), true).unwrap();

        let loaded = load_pi_model_settings_at(temp.path()).unwrap();
        assert_eq!(loaded.default_model.provider, "my-gateway");
        assert_eq!(loaded.default_model.model, "model-a");
        assert_eq!(loaded.custom_providers.len(), 1);
        let cp = &loaded.custom_providers[0];
        assert_eq!(cp.id, "my-gateway");
        assert_eq!(cp.display_name, "My Gateway");
        assert_eq!(cp.api, "openai-completions");
        assert_eq!(cp.base_url, "https://gateway.example.com/v1");
        assert!(cp.credential_configured);
        assert_eq!(cp.models.len(), 2);
        assert_eq!(cp.models[0].id, "model-a");
        assert_eq!(cp.models[0].context_window, Some(128000));

        // Now remove provider
        remove_pi_custom_provider_at(temp.path(), "my-gateway").unwrap();
        let after_remove = load_pi_model_settings_at(temp.path()).unwrap();
        assert_eq!(after_remove.custom_providers.len(), 0);
    }

    #[test]
    fn custom_model_empty_name_falls_back_to_id() {
        let temp = tempfile::tempdir().unwrap();
        let provider = PiCustomProvider {
            id: "test-provider".to_string(),
            display_name: String::new(),
            api: "openai-completions".to_string(),
            base_url: String::new(),
            credential_configured: false,
            models: vec![PiCustomModel {
                id: "custom-m1".to_string(),
                name: String::new(),
                context_window: None,
                max_tokens: None,
            }],
        };

        save_pi_custom_provider_at(temp.path(), &provider, None, false).unwrap();

        // Check raw models.json
        let raw = std::fs::read_to_string(temp.path().join("models.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            val["providers"]["test-provider"]["name"].as_str(),
            Some("test-provider")
        );
        assert_eq!(
            val["providers"]["test-provider"]["models"][0]["name"].as_str(),
            Some("custom-m1")
        );

        let loaded = load_pi_model_settings_at(temp.path()).unwrap();
        assert_eq!(loaded.custom_providers[0].display_name, "test-provider");
        assert_eq!(loaded.custom_providers[0].models[0].name, "custom-m1");
    }

    #[test]
    fn lists_custom_provider_models_for_the_agent_picker() {
        let temp = tempfile::tempdir().unwrap();
        let provider = PiCustomProvider {
            id: "Token-X".to_string(),
            display_name: "Token-X".to_string(),
            api: "openai-responses".to_string(),
            base_url: "https://model.example/v1".to_string(),
            credential_configured: false,
            models: vec![PiCustomModel {
                id: "Claude-Opus-5".to_string(),
                name: "Claude Opus 5".to_string(),
                context_window: None,
                max_tokens: None,
            }],
        };
        save_pi_custom_provider_at(temp.path(), &provider, None, false).unwrap();
        let choices = list_pi_model_choices_at(temp.path());
        assert!(
            choices
                .iter()
                .any(|choice| { choice.provider == "Token-X" && choice.id == "Claude-Opus-5" }),
            "{choices:?}"
        );
        let groups = group_pi_model_choices(choices);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].provider, "Token-X");
        assert_eq!(groups[0].models.len(), 1);
    }

    #[test]
    fn model_store_catalogs_without_credentials_stay_out_of_the_picker() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("auth.json"),
            r#"{"github-copilot":{"type":"oauth","refresh":"r","access":"a"}}"#,
        )
        .unwrap();
        std::fs::write(
            temp.path().join("models-store.json"),
            r#"{
                "github-copilot":{"name":"GitHub Copilot","models":[{"id":"gpt-5","name":"GPT-5"}]},
                "anthropic":{"name":"Anthropic","models":[{"id":"claude-opus-4.5","name":"Opus"}]}
            }"#,
        )
        .unwrap();
        let choices = list_pi_model_choices_at(temp.path());
        assert!(
            choices
                .iter()
                .any(|choice| choice.provider == "github-copilot" && choice.id == "gpt-5"),
            "{choices:?}"
        );
        assert!(
            choices.iter().all(|choice| choice.provider != "anthropic"),
            "{choices:?}"
        );
    }

    #[test]
    fn custom_provider_id_allows_mixed_case_hyphenated_slugs() {
        assert!(validate_custom_provider_id("acme-gateway").is_ok());
        assert!(validate_custom_provider_id("Token-X").is_ok());
        assert!(validate_custom_provider_id("Acme").is_ok());
        assert!(validate_custom_provider_id("a").is_ok());
        assert!(validate_custom_provider_id("gpt4").is_ok());
        assert!(validate_custom_provider_id("a1-b2").is_ok());

        assert!(validate_custom_provider_id("").is_err());
        assert!(validate_custom_provider_id("acme_gateway").is_err());
        assert!(validate_custom_provider_id("1acme").is_err());
        assert!(validate_custom_provider_id("-acme").is_err());
        assert!(validate_custom_provider_id("acme-").is_err());
        assert!(validate_custom_provider_id("acme--gw").is_err());
        assert!(validate_custom_provider_id("token x").is_err());
    }
}
