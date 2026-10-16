//! Pi 自带 provider 的账户额度。
//!
//! Pi 和其它 agent 的形状不一样：别家是「一个 CLI 一个账户」，Pi 是一个 agent
//! 底下挂着若干 provider，凭据统一放在 `~/.pi/agent/auth.json`。所以这里不新写
//! 额度接口，只做两件事：把 `auth.json` 里**订阅型（OAuth）**的凭据取出来，再
//! 交给已有的 provider parser。
//!
//! 只覆盖 OAuth。API key 型（自建网关、按量计费的 key）没有「订阅额度」这个
//! 概念，各家余额接口私有且多数 key 没有账单读权限，查不到就不显示——和
//! OpenCode / Kiro 不注册额度是同一套处理。
//!
//! 凭据只在查询期间留在栈上，不返回、不序列化、不写日志。

use crate::copilot_quota::fetch_copilot_quota_with_token;
use crate::pi_model_settings::pi_auth_path;
use crate::provider_quota::{
    ProviderQuotaData, fetch_claude_quota_with_token, fetch_codex_quota_with_token,
    fetch_grok_quota_with_token,
};

/// `auth.json` 里哪个字段才是额度接口要的令牌。
///
/// 这个区分不是风格问题：Pi 的 `github-copilot` 把 GitHub OAuth 令牌存在
/// `refresh`，`access` 放的是 `copilot_internal/v2/token` 换来的短期推理令牌
/// （`tid=…`），拿后者打额度接口固定 401。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PiTokenField {
    /// 访问令牌，受 `expires` 约束。
    Access,
    /// 刷新令牌，长期有效，不看 `expires`。
    Refresh,
}

/// Pi 下一家能查额度的 OAuth provider。
pub struct PiQuotaProvider {
    /// `auth.json` 的键，也是 Pi 的 provider id。展示名由菜单那边的协议分组提供，
    /// 这里不再重复维护一份。
    pub id: &'static str,
    token_field: PiTokenField,
}

/// Pi 的 OAuth provider 里，有已知只读额度接口的那些。顺序即侧栏展示顺序。
///
/// 没列进来的（`kimi-coding`、`openrouter`、`meta`、`radius`、`openai-chatgpt`）
/// 不是不支持 OAuth，而是我们没有可复用的额度接口；以后要接只需加 parser 和
/// 这里的一行。
pub static PI_QUOTA_PROVIDERS: &[PiQuotaProvider] = &[
    PiQuotaProvider {
        id: "anthropic",
        // Pi 用的是 Claude Code 的 client_id 和 scope，令牌能直接查 claude.ai 额度。
        token_field: PiTokenField::Access,
    },
    PiQuotaProvider {
        id: "github-copilot",
        token_field: PiTokenField::Refresh,
    },
    PiQuotaProvider {
        id: "openai-codex",
        token_field: PiTokenField::Access,
    },
    PiQuotaProvider {
        id: "xai",
        // Pi 的 xAI 登录带 `grok-cli:access` scope，和 Grok CLI 的会话令牌同源。
        token_field: PiTokenField::Access,
    },
];

pub fn pi_quota_provider(id: &str) -> Option<&'static PiQuotaProvider> {
    PI_QUOTA_PROVIDERS.iter().find(|provider| provider.id == id)
}

/// 当前 `auth.json` 里真正登录过、且令牌还能用的 provider。
///
/// 结果用于决定侧栏画几行，所以必须保守：没登录、登录的是 API key、令牌已过期
/// 的都不返回，避免画出一行永远查不出来的空额度。
pub fn available_pi_quota_providers() -> Vec<&'static PiQuotaProvider> {
    let Some(auth) = read_auth_json() else {
        return Vec::new();
    };
    PI_QUOTA_PROVIDERS
        .iter()
        .filter(|provider| usable_token(&auth, provider).is_some())
        .collect()
}

/// 查 Pi 下某个 provider 的额度。
///
/// 凭据不可用（没登录 / 是 API key / access token 过期）时返回 `Ok(None)`：这是
/// 「没有可查的额度」，不是查询失败，不该在侧栏留下一条错误。
pub async fn fetch_pi_provider_quota(id: &str) -> anyhow::Result<Option<ProviderQuotaData>> {
    let Some(provider) = pi_quota_provider(id) else {
        return Err(anyhow::anyhow!("unknown Pi quota provider {id}"));
    };
    let Some(auth) = read_auth_json() else {
        return Ok(None);
    };
    let Some(token) = usable_token(&auth, provider) else {
        return Ok(None);
    };

    match provider.id {
        "anthropic" => Ok(fetch_claude_quota_with_token(&token)
            .await?
            .map(ProviderQuotaData::Windows)),
        "github-copilot" => Ok(fetch_copilot_quota_with_token(&token)
            .await?
            .map(ProviderQuotaData::Copilot)),
        "openai-codex" => {
            // Pi 登录时就把 JWT 里的 `chatgpt_account_id` 落到了 `accountId`，
            // 不用在这里再解一次令牌。
            let account_id = auth
                .get(provider.id)
                .and_then(|entry| entry.get("accountId"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            Ok(fetch_codex_quota_with_token(&token, account_id)
                .await?
                .map(ProviderQuotaData::Windows))
        }
        "xai" => Ok(fetch_grok_quota_with_token(&token)
            .await?
            .map(ProviderQuotaData::Windows)),
        other => Err(anyhow::anyhow!("Pi quota provider {other} has no fetcher")),
    }
}

fn read_auth_json() -> Option<serde_json::Map<String, serde_json::Value>> {
    let body = std::fs::read_to_string(pi_auth_path()).ok()?;
    match serde_json::from_str::<serde_json::Value>(&body).ok()? {
        serde_json::Value::Object(map) => Some(map),
        _ => None,
    }
}

/// 取出该 provider 当前可用的令牌。
fn usable_token(
    auth: &serde_json::Map<String, serde_json::Value>,
    provider: &PiQuotaProvider,
) -> Option<String> {
    let entry = auth.get(provider.id)?;
    if entry.get("type").and_then(serde_json::Value::as_str) != Some("oauth") {
        return None;
    }
    if provider.token_field == PiTokenField::Access && access_token_expired(entry) {
        // 刷新是 Pi 的事：它下次用这个 provider 时会自己换新令牌并写回文件。
        // 我们拿过期令牌去查只会换来一条 401。
        return None;
    }
    let field = match provider.token_field {
        PiTokenField::Access => "access",
        PiTokenField::Refresh => "refresh",
    };
    entry
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

/// `expires` 是 access token 的到期时间（Unix 毫秒）。字段缺失按「没有到期信息」
/// 处理，照常尝试。
fn access_token_expired(entry: &serde_json::Value) -> bool {
    let Some(expires_ms) = entry.get("expires").and_then(serde_json::Value::as_i64) else {
        return false;
    };
    expires_ms <= now_ms()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(json: &str) -> serde_json::Map<String, serde_json::Value> {
        match serde_json::from_str(json).unwrap() {
            serde_json::Value::Object(map) => map,
            _ => panic!("test fixture must be an object"),
        }
    }

    #[test]
    fn copilot_uses_the_github_oauth_token_not_the_inference_token() {
        let auth = auth(
            r#"{"github-copilot":{"type":"oauth","access":"tid=short-lived","refresh":"ghu_long_lived","expires":1}}"#,
        );
        let provider = pi_quota_provider("github-copilot").unwrap();

        assert_eq!(
            usable_token(&auth, provider).as_deref(),
            Some("ghu_long_lived"),
            "额度接口要的是 GitHub OAuth 令牌；短期 Copilot 令牌打它固定 401"
        );
    }

    #[test]
    fn expired_access_token_is_not_reported_as_available() {
        let auth = auth(r#"{"anthropic":{"type":"oauth","access":"a","expires":1}}"#);
        let provider = pi_quota_provider("anthropic").unwrap();

        assert_eq!(usable_token(&auth, provider), None);
    }

    /// Copilot 的 `expires` 描述的是短期推理令牌，和我们要用的 `refresh` 无关，
    /// 不能因为它过期就把这一家判成不可用。
    #[test]
    fn expired_access_does_not_disable_a_refresh_token_provider() {
        let auth = auth(
            r#"{"github-copilot":{"type":"oauth","access":"tid=x","refresh":"ghu_x","expires":1}}"#,
        );
        let provider = pi_quota_provider("github-copilot").unwrap();

        assert_eq!(usable_token(&auth, provider).as_deref(), Some("ghu_x"));
    }

    #[test]
    fn api_key_credentials_have_no_subscription_quota() {
        let auth = auth(r#"{"anthropic":{"type":"api_key","key":"sk-ant-x"}}"#);
        let provider = pi_quota_provider("anthropic").unwrap();

        assert_eq!(usable_token(&auth, provider), None);
    }

    #[test]
    fn unknown_providers_are_not_registered() {
        assert!(pi_quota_provider("modelsight").is_none());
        assert!(pi_quota_provider("volces").is_none());
    }

    #[test]
    fn blank_tokens_count_as_missing() {
        let auth = auth(r#"{"xai":{"type":"oauth","access":"   "}}"#);
        let provider = pi_quota_provider("xai").unwrap();

        assert_eq!(usable_token(&auth, provider), None);
    }

    #[test]
    fn future_expiry_keeps_the_access_token() {
        let future = now_ms() + 600_000;
        let auth = auth(&format!(
            r#"{{"anthropic":{{"type":"oauth","access":"live","expires":{future}}}}}"#
        ));
        let provider = pi_quota_provider("anthropic").unwrap();

        assert_eq!(usable_token(&auth, provider).as_deref(), Some("live"));
    }
}
