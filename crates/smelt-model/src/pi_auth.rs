//! Pi 登录流程的数据：provider 现状、登录方式、助手推来的事件。跑凭据助手在运行时。

use rust_i18n::t;

/// 一个 provider 当前的凭据状态。
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
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
#[derive(
    Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
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
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
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

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PiPromptKind {
    Text,
    Secret,
    Select,
    ManualCode,
}

#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct PiPromptOption {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct PiAuthLink {
    pub url: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// 登录流程推给宿主的一步。
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<PiLoginNote>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<PiLoginNote>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<PiLoginNote>,
    },
    /// 这个提问不用答了（浏览器回调先到了）。界面必须据此把输入框收掉。
    PromptDone {
        id: String,
    },
    Done,
    Error {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<PiLoginNote>,
    },
    /// 助手报了一种这个版本还不认识的事件。忽略它即可——运行时可以比宿主新，
    /// 一个新事件不该让正在跑的登录变成失败。
    #[serde(other)]
    Unknown,
}

/// 登录助手自己说的话（MCP 登录的提示、替用户代答的说明）的种类，客户端按自己的语言说。
/// 同一事件的 `message` / `instructions` 是助手按英文说的同一句，给不认识种类的客户端兜底。
/// Pi 和 provider 自己的话没有种类，原样显示。
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PiLoginNote {
    /// 授权后浏览器打不开回调页时，把地址栏里的地址贴回来。
    PasteCallback,
    /// 去浏览器登录并同意授权。
    SignInInBrowser,
    /// 收到贴回来的地址，正在完成登录。
    CallbackReceived,
    /// 回调不收这个地址。
    CallbackRejected { status: u16 },
    /// 转交回调没成；`error` 是底层原话。
    CallbackFailed { error: String },
    /// 授权链接里没有回调地址，只能在这台机器的浏览器里授权。
    NoRedirect,
    /// 贴的不是完整地址。
    NotFullAddress,
    /// 贴的不是这次登录的回调地址。
    WrongCallback { expected: String },
    /// 地址里没有授权结果。
    NoAuthorizationResult,
    /// 登录没成，助手没留下原因。
    LoginFailed { code: i32 },
    /// 替用户答了 GitHub 企业版域名：留空，按 github.com 登录。
    AnsweredGithubDotCom { question: String },
    /// 替用户选了用浏览器登录。
    AnsweredBrowser { question: String },
    /// 凭据助手没报结果就退出了；`detail` 是它 stderr 的最后几行。
    HelperExited { detail: Option<String> },
    /// 比这个客户端新的助手发来的种类。
    #[serde(other)]
    Other,
}

impl PiLoginNote {
    /// 按当前语言说；不认识的种类返回 `None`，由调用方改说 `message`。
    pub fn text(&self) -> Option<String> {
        let text = match self {
            Self::PasteCallback => t!("pi_login.paste_callback"),
            Self::SignInInBrowser => t!("pi_login.sign_in_in_browser"),
            Self::CallbackReceived => t!("pi_login.callback_received"),
            Self::CallbackRejected { status } => {
                t!("pi_login.callback_rejected", status = status)
            }
            Self::CallbackFailed { error } => t!("pi_login.callback_failed", error = error),
            Self::NoRedirect => t!("pi_login.no_redirect"),
            Self::NotFullAddress => t!("pi_login.not_full_address"),
            Self::WrongCallback { expected } => {
                t!("pi_login.wrong_callback", expected = expected)
            }
            Self::NoAuthorizationResult => t!("pi_login.no_authorization_result"),
            Self::LoginFailed { code } => t!("pi_login.login_failed", code = code),
            Self::AnsweredGithubDotCom { question } => {
                t!("pi_login.answered_github_dot_com", question = question)
            }
            Self::AnsweredBrowser { question } => {
                t!("pi_login.answered_browser", question = question)
            }
            Self::HelperExited { detail: None } => t!("pi_login.helper_exited"),
            Self::HelperExited {
                detail: Some(detail),
            } => t!("pi_login.helper_exited_with", detail = detail),
            Self::Other => return None,
        };
        Some(text.into_owned())
    }
}

/// 登录面板上显示的一句：带着认识的种类就按当前语言说，否则原样给 `message`。
pub fn login_text(message: String, note: Option<&PiLoginNote>) -> String {
    note.and_then(PiLoginNote::text).unwrap_or(message)
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
            message: format!("couldn't parse the login event {name}: {error}"),
            note: None,
        }),
    }
}
