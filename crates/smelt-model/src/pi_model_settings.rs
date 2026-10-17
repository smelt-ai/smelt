//! Pi 模型配置的数据（默认模型、自定义 provider、可选模型）。读写 `~/.pi/agent` 在运行时。

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

use std::path::PathBuf;

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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PiDefaultModelSummary {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking_level: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PiSettingsSummary {
    pub path: PathBuf,
    pub default_model: Option<PiDefaultModelSummary>,
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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PiModelChoiceGroup {
    pub provider: String,
    pub provider_name: String,
    pub models: Vec<PiModelChoice>,
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
