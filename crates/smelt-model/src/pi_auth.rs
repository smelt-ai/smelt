//! Pi 登录流程的数据：provider 现状、登录方式、助手推来的事件。跑凭据助手在运行时。


/// 一个 provider 当前的凭据状态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PiAuthStatus {
    /// 订阅登录过，`auth.json` 里是 OAuth 令牌。
    Oauth,
    /// 有 API key——存在 `auth.json` 里，或来自环境变量、云厂商的环境凭据。
    ApiKey,
    #[default]
    None,
}

impl PiAuthStatus {
    pub fn configured(self) -> bool {
        !matches!(self, PiAuthStatus::None)
    }
}

/// 一个内置 provider 的登录能力与现状。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiAuthProvider {
    pub id: String,
    pub name: String,
    /// OAuth 那条路的按钮文案，例如「Sign in with Kimi Code」。
    #[serde(default)]
    pub oauth_name: Option<String>,
    /// API key 那条路的名字，例如「Anthropic API key」。
    #[serde(default)]
    pub api_key_name: Option<String>,
    /// OAuth 走的是订阅（Claude Pro/Max、Copilot 这类），不是按量计费的 key。
    #[serde(default)]
    pub subscription: bool,
    #[serde(default)]
    pub supports_oauth: bool,
    #[serde(default)]
    pub supports_api_key: bool,
    #[serde(default)]
    pub status: PiAuthStatus,
    /// 凭据从哪儿来，例如 "OAuth"、"ANTHROPIC_API_KEY"、"~/.aws/credentials"。
    #[serde(default)]
    pub source: Option<String>,
}

/// 登录方式。`Models.login` 按它选 `provider.auth.oauth` 还是 `auth.apiKey`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PiLoginMethod {
    #[serde(rename = "oauth")]
    OAuth,
    #[serde(rename = "api_key")]
    ApiKey,
}

impl PiLoginMethod {
    pub fn as_arg(self) -> &'static str {
        match self {
            PiLoginMethod::OAuth => "oauth",
            PiLoginMethod::ApiKey => "api_key",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PiPromptKind {
    Text,
    Secret,
    Select,
    ManualCode,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PiPromptOption {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PiAuthLink {
    pub url: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// 登录流程推给宿主的一步。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum PiLoginEvent {
    Providers {
        providers: Vec<PiAuthProvider>,
    },
    /// 某个 provider 当前凭据能用的模型目录。
    Models {
        models: Vec<crate::provider_api::DiscoveredModel>,
    },
    /// 去浏览器完成授权。宿主负责打开它——pi-ai 自己不开浏览器。
    AuthUrl {
        url: String,
        #[serde(default)]
        instructions: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    DeviceCode {
        user_code: String,
        verification_uri: String,
        #[serde(default)]
        interval_seconds: Option<u64>,
        #[serde(default)]
        expires_in_seconds: Option<u64>,
    },
    Info {
        message: String,
        #[serde(default)]
        links: Vec<PiAuthLink>,
    },
    Progress {
        message: String,
    },
    #[serde(rename_all = "camelCase")]
    Prompt {
        id: String,
        prompt_type: PiPromptKind,
        message: String,
        #[serde(default)]
        placeholder: Option<String>,
        #[serde(default)]
        options: Vec<PiPromptOption>,
    },
    /// 这个提问不用答了（浏览器回调先到了）。界面必须据此把输入框收掉。
    PromptDone {
        id: String,
    },
    Done,
    Error {
        message: String,
    },
    /// 助手报了一种这个版本还不认识的事件。忽略它即可——运行时可以比宿主新，
    /// 一个新事件不该让正在跑的登录变成失败。
    #[serde(other)]
    Unknown,
}

/// 解析子进程的一行输出。非协议行返回 `None`。
///
/// 「解析不了就当噪声」只对**不是协议行**的东西成立：依赖库偶尔往 stdout 打
/// 日志，不该因此掐掉用户正在做的授权。但一行明明写着 `"event":"device_code"`
/// 却反序列化失败，是两侧协议对不上——静默丢掉它，界面就永远停在「稍后会给出
/// 授权链接」上，没有任何线索。所以这种情况必须变成一条看得见的错误。
pub fn parse_login_event(line: &str) -> Option<PiLoginEvent> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let name = value.get("event")?.as_str()?.to_string();
    match serde_json::from_value(value) {
        Ok(PiLoginEvent::Unknown) => None,
        Ok(event) => Some(event),
        Err(error) => Some(PiLoginEvent::Error {
            message: format!("无法解析登录事件 {name}：{error}"),
        }),
    }
}
