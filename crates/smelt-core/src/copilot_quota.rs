//! Copilot 额度查询与解析。
//!
//! 额度不是 ACP 的 `UsageUpdate`：后者描述上下文窗口 token，用来画「上下文
//! 32%」。Copilot CLI/客户端会用本地 GitHub OAuth 凭据请求账户额度。

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CopilotQuotaUnit {
    AiCredits,
    PremiumRequests,
}

impl CopilotQuotaUnit {
    pub fn label(self) -> &'static str {
        match self {
            Self::AiCredits => "AI credits",
            Self::PremiumRequests => "premium requests",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CopilotQuota {
    pub unit: CopilotQuotaUnit,
    pub remaining: f64,
    pub limit: Option<f64>,
    pub used: Option<f64>,
}

/// 查询状态始终带时间，即使这次输出没有识别到额度；这样 UI 能结束「查询中」
/// 状态，也不会因为服务端返回了未知格式而反复查询。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CopilotQuotaStatus {
    pub quota: Option<CopilotQuota>,
    pub checked_at_ms: u64,
    /// 最近一次刷新失败原因；有 quota 时表示当前展示的是上一次成功结果。
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CopilotUserResponse {
    #[serde(default)]
    quota_snapshots: BTreeMap<String, CopilotQuotaSnapshot>,
}

#[derive(Debug, Deserialize)]
struct CopilotQuotaSnapshot {
    #[serde(default)]
    quota_remaining: Option<f64>,
    #[serde(default)]
    remaining: Option<f64>,
    #[serde(default)]
    entitlement: Option<f64>,
    #[serde(default)]
    credits_used: Option<f64>,
    #[serde(default)]
    premium_requests_used: Option<f64>,
    #[serde(default)]
    has_quota: Option<bool>,
    #[serde(default)]
    unlimited: Option<bool>,
    #[serde(default)]
    token_based_billing: Option<bool>,
}

const COPILOT_QUOTA_ENDPOINT: &str = "https://api.github.com/copilot_internal/user";

/// 使用本机 Copilot/GitHub 凭据查询当前账户额度。
///
/// 凭据只在本函数的栈和 reqwest 请求期间存在，不会返回、序列化或写入日志。
/// 只使用 Copilot CLI/IDE 的 `apps.json` 凭据，不读取通用 GitHub 环境变量，
/// 避免运行 Smelt 的宿主环境把无 Copilot 权限的 token 混进来。
pub async fn fetch_copilot_quota() -> anyhow::Result<Option<CopilotQuota>> {
    let token = local_copilot_token()?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()?;
    let response = client
        .get(COPILOT_QUOTA_ENDPOINT)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        // curl 会自动带 User-Agent；reqwest 不会。GitHub 对没有 User-Agent
        // 的请求可能返回 403，即使 OAuth token 本身有效。
        .header(reqwest::header::USER_AGENT, "Smelt")
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        // 不把响应 body 带进错误：内部接口有时会返回账户或认证细节，避免进入
        // smeltd 的 stderr / 调试日志。
        return Err(anyhow::anyhow!(
            "Copilot quota endpoint returned HTTP {status}"
        ));
    }
    let body = response.text().await?;
    parse_copilot_api_quota(&body)
}

/// 解析 Copilot 账户额度接口返回的 JSON。
pub fn parse_copilot_api_quota(body: &str) -> anyhow::Result<Option<CopilotQuota>> {
    let response: CopilotUserResponse = serde_json::from_str(body)?;
    Ok(select_api_quota(&response.quota_snapshots))
}

fn select_api_quota(snapshots: &BTreeMap<String, CopilotQuotaSnapshot>) -> Option<CopilotQuota> {
    // `premium_interactions` 是当前额度接口的稳定 key；保留旧 key 和兜底扫描，
    // 兼容 GitHub 仍处于 premium requests 计费的账号及未来新增的 quota key。
    let preferred = ["premium_interactions", "premium_requests", "ai_credits"];
    let snapshot = preferred
        .iter()
        .find_map(|key| snapshots.get(*key).map(|snapshot| (*key, snapshot)))
        .filter(|(_, snapshot)| usable_api_snapshot(snapshot))
        .or_else(|| {
            snapshots
                .iter()
                .find(|(_, snapshot)| usable_api_snapshot(snapshot))
                .map(|(key, snapshot)| (key.as_str(), snapshot))
        })?;

    let (_, snapshot) = snapshot;
    let remaining = snapshot.quota_remaining.or(snapshot.remaining)?.max(0.0);
    let limit = snapshot
        .entitlement
        .filter(|value| value.is_finite() && *value > 0.0);
    let used = snapshot
        .credits_used
        .or(snapshot.premium_requests_used)
        .or_else(|| limit.map(|limit| (limit - remaining).max(0.0)))
        .filter(|value| value.is_finite() && *value >= 0.0);
    let unit = if snapshot.token_based_billing == Some(true) {
        CopilotQuotaUnit::AiCredits
    } else {
        CopilotQuotaUnit::PremiumRequests
    };

    Some(CopilotQuota {
        unit,
        remaining,
        limit,
        used,
    })
}

fn usable_api_snapshot(snapshot: &CopilotQuotaSnapshot) -> bool {
    snapshot.unlimited != Some(true)
        && (snapshot.has_quota == Some(true)
            || snapshot.quota_remaining.is_some()
            || snapshot.remaining.is_some())
}

fn local_copilot_token() -> anyhow::Result<String> {
    for path in copilot_apps_paths() {
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
            continue;
        };
        if let Some(token) = find_apps_json_token(&value) {
            return Ok(token);
        }
    }

    Err(anyhow::anyhow!("Copilot local credentials were not found"))
}

fn copilot_apps_paths() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME") {
        paths.push(PathBuf::from(xdg).join("github-copilot/apps.json"));
    }
    // macOS 上 GitHub Copilot 目前实际使用 ~/.config/github-copilot；把它放在
    // dirs::config_dir() 之前，避免平台默认 Application Support 路径遮住它。
    paths.push(home.join(".config/github-copilot/apps.json"));
    if let Some(config) = dirs::config_dir() {
        paths.push(config.join("github-copilot/apps.json"));
    }
    paths.push(home.join("Library/Application Support/GitHub Copilot/apps.json"));
    paths.sort();
    paths.dedup();
    paths
}

fn find_apps_json_token(value: &serde_json::Value) -> Option<String> {
    let object = value.as_object()?;
    let preferred_host = env::var("COPILOT_GH_HOST")
        .or_else(|_| env::var("GH_HOST"))
        .unwrap_or_else(|_| "github.com".to_string());

    let mut fallback = None;
    for (key, entry) in object {
        let host = key.split(':').next().unwrap_or_default();
        let token = entry
            .get("oauth_token")
            .and_then(serde_json::Value::as_str)
            .filter(|token| !token.trim().is_empty())
            .map(str::to_string);
        let Some(token) = token else { continue };
        if host.eq_ignore_ascii_case(&preferred_host) {
            return Some(token);
        }
        if fallback.is_none() && host.eq_ignore_ascii_case("github.com") {
            fallback = Some(token);
        }
    }
    fallback
}

pub fn format_amount(value: f64) -> String {
    if (value - value.round()).abs() < 0.005 {
        return value.round().to_string();
    }
    let mut formatted = format!("{value:.2}");
    while formatted.ends_with('0') {
        formatted.pop();
    }
    if formatted.ends_with('.') {
        formatted.pop();
    }
    formatted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_direct_api_token_based_quota() {
        let body = r#"
        {
          "quota_snapshots": {
            "chat": {"unlimited": true, "has_quota": true},
            "premium_interactions": {
              "quota_remaining": 13189.5,
              "remaining": 13189,
              "entitlement": 23000,
              "credits_used": 9810,
              "has_quota": true,
              "unlimited": null,
              "token_based_billing": true
            }
          }
        }
        "#;
        let quota = parse_copilot_api_quota(body)
            .expect("API JSON should parse")
            .expect("premium interaction quota should be selected");
        assert_eq!(quota.unit, CopilotQuotaUnit::AiCredits);
        assert_eq!(quota.remaining, 13189.5);
        assert_eq!(quota.limit, Some(23000.0));
        assert_eq!(quota.used, Some(9810.0));
    }

    #[test]
    fn accepts_nullable_quota_flags_from_github() {
        let body = r#"
        {"quota_snapshots": {
          "premium_interactions": {
            "quota_remaining": 12,
            "entitlement": 20,
            "has_quota": true,
            "unlimited": null,
            "token_based_billing": null
          }
        }}
        "#;
        let quota = parse_copilot_api_quota(body)
            .expect("nullable GitHub fields should parse")
            .expect("premium interaction quota should be selected");
        assert_eq!(quota.unit, CopilotQuotaUnit::PremiumRequests);
        assert_eq!(quota.remaining, 12.0);
        assert_eq!(quota.limit, Some(20.0));
    }

    #[test]
    fn parses_direct_api_legacy_premium_request_quota() {
        let body = r#"
        {
          "quota_snapshots": {
            "premium_requests": {
              "quota_remaining": 297,
              "entitlement": 300,
              "has_quota": true,
              "unlimited": false,
              "token_based_billing": false
            }
          }
        }
        "#;
        let quota = parse_copilot_api_quota(body)
            .expect("API JSON should parse")
            .expect("legacy quota should be selected");
        assert_eq!(quota.unit, CopilotQuotaUnit::PremiumRequests);
        assert_eq!(quota.remaining, 297.0);
        assert_eq!(quota.limit, Some(300.0));
        assert_eq!(quota.used, Some(3.0));
    }

    #[test]
    fn ignores_only_unlimited_api_snapshots() {
        let body = r#"
        {"quota_snapshots": {
          "chat": {"quota_remaining": 0, "has_quota": true, "unlimited": true},
          "completions": {"quota_remaining": 0, "has_quota": true, "unlimited": true}
        }}
        "#;
        assert_eq!(parse_copilot_api_quota(body).unwrap(), None);
    }
}
