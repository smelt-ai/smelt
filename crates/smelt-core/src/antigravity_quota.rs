//! Antigravity（`agy`）账户额度查询与解析。
//!
//! 凭据从 `agy` 写入系统钥匙串的 OAuth 记录读取（`go-keyring-base64:` 编码的 JSON），
//! 优先请求 AICode 只读接口 `v1alpha:fetchQuotaStatus` 取 `5h` + `7d`（weekly）汇总窗口；
//! 若服务端未下发汇总（或非 AICode 路由账号），回退到按模型桶归并，或回退到
//! `v1internal:retrieveUserQuota`。令牌仅留在内存和单次请求期间，不写入日志或快照。

use base64::Engine as _;
use reqwest::StatusCode;
use serde::Deserialize;

use crate::provider_quota::{ProviderQuota, ProviderQuotaWindow, parse_rfc3339_ms};
use crate::quota_credentials::{
    CredentialAccess, KeychainLookup, KeychainRead, find_generic_password, keychain_token,
    keychain_token_after_rejection,
};
use crate::quota_http::{QuotaEndpointError, is_unauthorized, quota_http_client};

/// 钥匙串熔断状态用的账户 id，和 `QuotaTarget::id()` 一致。
pub(crate) const ANTIGRAVITY_QUOTA_PROVIDER: &str = "antigravity";

const ANTIGRAVITY_KEYCHAIN_SERVICE: &str = "gemini";
const ANTIGRAVITY_KEYCHAIN_ACCOUNT: &str = "antigravity";
const GO_KEYRING_BASE64_PREFIX: &str = "go-keyring-base64:";
const ANTIGRAVITY_AICODE_QUOTA_ENDPOINT: &str =
    "https://aicode.googleapis.com/v1alpha:fetchQuotaStatus";
const ANTIGRAVITY_CLOUDCODE_QUOTA_ENDPOINT: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AntigravityQuotaResponse {
    #[serde(default)]
    buckets: Vec<AntigravityQuotaBucket>,
    #[serde(default)]
    quota_summary: Option<AntigravityQuotaSummary>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AntigravityQuotaSummary {
    #[serde(default)]
    groups: Vec<AntigravityQuotaSummaryGroup>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AntigravityQuotaSummaryGroup {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    buckets: Vec<AntigravitySummaryBucket>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AntigravitySummaryBucket {
    #[serde(default)]
    bucket_id: Option<String>,
    #[serde(default)]
    refresh_window_label: Option<String>,
    #[serde(default)]
    remaining_fraction: Option<f64>,
    #[serde(default)]
    reset_time: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AntigravityQuotaBucket {
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    model: Option<AntigravityBucketModel>,
    #[serde(default)]
    remaining_fraction: Option<f64>,
    #[serde(default)]
    reset_time: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AntigravityBucketModel {
    #[serde(default)]
    model_id: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SummaryWindow {
    FiveHour,
    Weekly,
}

impl SummaryWindow {
    const ALL: [Self; 2] = [Self::FiveHour, Self::Weekly];

    fn from_bucket(bucket: &AntigravitySummaryBucket) -> Option<Self> {
        let label = bucket
            .refresh_window_label
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let id = bucket
            .bucket_id
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if label == "5h" || id.ends_with("-5h") {
            Some(Self::FiveHour)
        } else if label == "weekly" || label == "7d" || id.ends_with("-weekly") {
            Some(Self::Weekly)
        } else {
            None
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::FiveHour => "5h",
            Self::Weekly => "7d",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::FiveHour => 0,
            Self::Weekly => 1,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModelPool {
    Gemini,
    Partner,
    Other,
}

impl ModelPool {
    const ALL: [Self; 3] = [Self::Gemini, Self::Partner, Self::Other];

    fn from_model_id(model_id: &str) -> Option<Self> {
        let lower = model_id.trim().to_ascii_lowercase();
        if lower.is_empty() || lower.starts_with("chat_") || lower.starts_with("tab_") {
            return None;
        }
        if lower.starts_with("gemini") {
            Some(Self::Gemini)
        } else if lower.starts_with("claude") || lower.starts_with("gpt") {
            Some(Self::Partner)
        } else {
            Some(Self::Other)
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Gemini => "Gemini",
            Self::Partner => "Claude / GPT",
            Self::Other => "Other",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Gemini => 0,
            Self::Partner => 1,
            Self::Other => 2,
        }
    }
}

#[derive(Clone, Copy)]
struct MergedQuota {
    used_percent: f64,
    resets_at_ms: Option<u64>,
}

/// 使用本机 `agy` 钥匙串里的 Google OAuth 凭据查询账户额度。
pub async fn fetch_antigravity_quota(
    access: CredentialAccess,
) -> anyhow::Result<Option<ProviderQuota>> {
    let Some(token) = local_antigravity_credential(access)? else {
        return Ok(None);
    };
    let result = fetch_antigravity_quota_with_token(&token).await;
    match result {
        // 缓存的令牌过期了：可能开了新 `agy` 会话写回新令牌，重读一次。
        Err(error) if is_unauthorized(&error) => match keychain_token_after_rejection(
            ANTIGRAVITY_QUOTA_PROVIDER,
            ANTIGRAVITY_KEYCHAIN_SERVICE,
            &token,
            read_antigravity_keychain,
        ) {
            Some(refreshed) => fetch_antigravity_quota_with_token(&refreshed).await,
            None => Err(error),
        },
        other => other,
    }
}

/// 用调用方给定的 Google OAuth access token 查询 Antigravity 额度。
pub async fn fetch_antigravity_quota_with_token(
    token: &str,
) -> anyhow::Result<Option<ProviderQuota>> {
    let client = quota_http_client()?;
    let response = client
        .get(ANTIGRAVITY_AICODE_QUOTA_ENDPOINT)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "antigravity")
        .send()
        .await?;
    let status = response.status();
    if status.is_success() {
        return parse_antigravity_quota(&response.text().await?);
    }
    if !matches!(status, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN) {
        return Err(QuotaEndpointError {
            provider: "Antigravity",
            status,
        }
        .into());
    }

    // 未切换到 AICode 路由的账号走旧 Cloud Code Assist 接口。
    let fallback = client
        .post(ANTIGRAVITY_CLOUDCODE_QUOTA_ENDPOINT)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::USER_AGENT, "antigravity")
        .body("{}")
        .send()
        .await?;
    let fallback_status = fallback.status();
    if !fallback_status.is_success() {
        return Err(QuotaEndpointError {
            provider: "Antigravity",
            status: fallback_status,
        }
        .into());
    }
    parse_antigravity_quota(&fallback.text().await?)
}

/// 解析 `fetchQuotaStatus` / `retrieveUserQuota` 返回的 JSON。
///
/// 优先从 `quotaSummary.groups` 抽取标准的 `5h` 与 `7d` 窗口，每组模型各一套，带组名；
/// 没有汇总时退回按模型池归并 `buckets`，一池一个不知道时长的窗口。
pub fn parse_antigravity_quota(body: &str) -> anyhow::Result<Option<ProviderQuota>> {
    let response: AntigravityQuotaResponse = serde_json::from_str(body)?;
    if let Some(summary) = response.quota_summary.as_ref() {
        let windows = parse_summary_windows(summary);
        if !windows.is_empty() {
            return Ok(Some(ProviderQuota { windows }));
        }
    }
    let windows = parse_bucket_windows(&response.buckets);
    Ok((!windows.is_empty()).then_some(ProviderQuota { windows }))
}

fn parse_summary_windows(summary: &AntigravityQuotaSummary) -> Vec<ProviderQuotaWindow> {
    // 按组出现的先后排；没有组名的几组并成整个账户的一套。
    let mut groups: Vec<(Option<String>, [Option<MergedQuota>; 2])> = Vec::new();
    for group in &summary.groups {
        let scope = group.display_name.as_deref().and_then(group_scope);
        let at = match groups.iter().position(|(name, _)| *name == scope) {
            Some(at) => at,
            None => {
                groups.push((scope, [None; 2]));
                groups.len() - 1
            }
        };
        for bucket in &group.buckets {
            let Some(kind) = SummaryWindow::from_bucket(bucket) else {
                continue;
            };
            let Some(entry) =
                quota_from_fraction(bucket.remaining_fraction, bucket.reset_time.as_deref())
            else {
                continue;
            };
            merge_slot(&mut groups[at].1[kind.index()], entry);
        }
    }
    groups
        .into_iter()
        .flat_map(|(scope, slots)| {
            SummaryWindow::ALL.into_iter().filter_map(move |kind| {
                let quota = slots[kind.index()]?;
                Some(ProviderQuotaWindow {
                    label: kind.label().to_string(),
                    used_percent: quota.used_percent,
                    resets_at_ms: quota.resets_at_ms,
                    reset_after_seconds: None,
                    scope: scope.clone(),
                })
            })
        })
        .collect()
}

/// 汇总里的组名（「Gemini Models」「Claude and GPT models」）去掉末尾的「models」，留下模型名。
fn group_scope(display_name: &str) -> Option<String> {
    let name = display_name.trim();
    let name = match name.len().checked_sub(" models".len()) {
        Some(cut) if name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(" models") => {
            name[..cut].trim_end()
        }
        _ => name,
    };
    (!name.is_empty()).then(|| name.to_string())
}

fn parse_bucket_windows(buckets: &[AntigravityQuotaBucket]) -> Vec<ProviderQuotaWindow> {
    let mut pools: [Option<MergedQuota>; 3] = [None; 3];
    for bucket in buckets {
        let Some(model_id) = bucket
            .model_id
            .as_deref()
            .or_else(|| bucket.model.as_ref().and_then(|m| m.model_id.as_deref()))
        else {
            continue;
        };
        let Some(pool) = ModelPool::from_model_id(model_id) else {
            continue;
        };
        let Some(entry) =
            quota_from_fraction(bucket.remaining_fraction, bucket.reset_time.as_deref())
        else {
            continue;
        };
        merge_slot(&mut pools[pool.index()], entry);
    }
    ModelPool::ALL
        .into_iter()
        .filter_map(|pool| {
            let quota = pools[pool.index()]?;
            // 按模型的桶不说窗口多长，只知道什么时候重置。
            Some(ProviderQuotaWindow {
                label: "period".to_string(),
                used_percent: quota.used_percent,
                resets_at_ms: quota.resets_at_ms,
                reset_after_seconds: None,
                scope: Some(pool.label().to_string()),
            })
        })
        .collect()
}

fn quota_from_fraction(
    remaining_fraction: Option<f64>,
    reset_time: Option<&str>,
) -> Option<MergedQuota> {
    let resets_at_ms = reset_time.and_then(parse_rfc3339_ms);
    let used_percent = match (remaining_fraction, resets_at_ms) {
        (Some(fraction), _) if fraction.is_finite() => {
            ((1.0 - fraction.clamp(0.0, 1.0)) * 100.0).clamp(0.0, 100.0)
        }
        // proto3 json `omitempty` 会省略为 0.0 的 remainingFraction，但仍带 resetTime。
        (None, Some(_)) => 100.0,
        _ => return None,
    };
    Some(MergedQuota {
        used_percent,
        resets_at_ms,
    })
}

fn merge_slot(slot: &mut Option<MergedQuota>, next: MergedQuota) {
    *slot = Some(match *slot {
        None => next,
        Some(prev) => merge_quota(prev, next),
    });
}

fn merge_quota(prev: MergedQuota, next: MergedQuota) -> MergedQuota {
    if next.used_percent > prev.used_percent {
        MergedQuota {
            used_percent: next.used_percent,
            resets_at_ms: next.resets_at_ms.or(prev.resets_at_ms),
        }
    } else if (next.used_percent - prev.used_percent).abs() < f64::EPSILON {
        let earliest = match (prev.resets_at_ms, next.resets_at_ms) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        MergedQuota {
            used_percent: prev.used_percent,
            resets_at_ms: earliest,
        }
    } else {
        prev
    }
}

fn local_antigravity_credential(access: CredentialAccess) -> anyhow::Result<Option<String>> {
    if !cfg!(target_os = "macos") {
        return Ok(None);
    }
    let lookup = keychain_token(
        ANTIGRAVITY_QUOTA_PROVIDER,
        ANTIGRAVITY_KEYCHAIN_SERVICE,
        access,
        read_antigravity_keychain,
    );
    if let KeychainLookup::Token(token) = lookup {
        return Ok(Some(token));
    }
    if let Some(error) = lookup.blocked_error() {
        return Err(error);
    }
    Ok(None)
}

fn read_antigravity_keychain() -> KeychainRead {
    match find_generic_password(
        ANTIGRAVITY_KEYCHAIN_SERVICE,
        Some(ANTIGRAVITY_KEYCHAIN_ACCOUNT),
    ) {
        KeychainRead::Secret(raw) => antigravity_token_from_keyring_secret(&raw)
            .map_or(KeychainRead::NotFound, KeychainRead::Secret),
        other => other,
    }
}

/// 解出 `go-keyring-base64:` 或明文 JSON 里的 OAuth `access_token`。
/// API key 登录没有订阅额度接口，直接忽略。
fn antigravity_token_from_keyring_secret(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let decoded;
    let json_bytes = if let Some(encoded) = trimmed.strip_prefix(GO_KEYRING_BASE64_PREFIX) {
        decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .ok()?;
        decoded.as_slice()
    } else {
        trimmed.as_bytes()
    };
    let value: serde_json::Value = serde_json::from_slice(json_bytes).ok()?;
    if value
        .get("auth_method")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|method| method.to_ascii_lowercase().contains("api-key"))
    {
        return None;
    }
    value
        .pointer("/token/access_token")
        .or_else(|| value.get("access_token"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aicode_summary_five_hour_and_weekly_windows() {
        let body = r#"{
          "buckets": [
            {
              "remainingFraction": 1,
              "resetTime": "2026-11-18T20:35:11Z",
              "model": {"modelId": "claude-opus-5-5"}
            }
          ],
          "quotaSummary": {
            "groups": [
              {
                "displayName": "Gemini Models",
                "buckets": [
                  {
                    "bucketId": "gemini-weekly",
                    "refreshWindowLabel": "weekly",
                    "remainingFraction": 0.88,
                    "resetTime": "2026-11-25T05:19:37Z"
                  },
                  {
                    "bucketId": "gemini-5h",
                    "refreshWindowLabel": "5h",
                    "remainingFraction": 0.72,
                    "resetTime": "2026-11-18T17:32:49Z"
                  }
                ]
              },
              {
                "displayName": "Claude and GPT models",
                "buckets": [
                  {
                    "bucketId": "3p-weekly",
                    "refreshWindowLabel": "weekly",
                    "remainingFraction": 0.5,
                    "resetTime": "2026-11-25T15:35:11Z"
                  },
                  {
                    "bucketId": "3p-5h",
                    "refreshWindowLabel": "5h",
                    "remainingFraction": 1,
                    "resetTime": "2026-11-18T20:35:11Z"
                  }
                ]
              }
            ]
          }
        }"#;

        let quota = parse_antigravity_quota(body).unwrap().unwrap();
        let windows: Vec<(Option<&str>, &str, u32)> = quota
            .windows
            .iter()
            .map(|window| {
                (
                    window.scope.as_deref(),
                    window.label.as_str(),
                    window.used_percent.round() as u32,
                )
            })
            .collect();
        assert_eq!(
            windows,
            [
                (Some("Gemini"), "5h", 28),
                (Some("Gemini"), "7d", 12),
                (Some("Claude and GPT"), "5h", 0),
                (Some("Claude and GPT"), "7d", 50),
            ]
        );
        assert_eq!(
            quota.windows[0].resets_at_ms,
            parse_rfc3339_ms("2026-11-18T17:32:49Z")
        );
        assert_eq!(
            quota.windows[3].resets_at_ms,
            parse_rfc3339_ms("2026-11-25T15:35:11Z")
        );
    }

    #[test]
    fn falls_back_to_model_pool_buckets_when_summary_is_missing() {
        let body = r#"{
          "buckets": [
            {"tokenType": "WTUS", "modelId": "chat_20706", "remainingFraction": 1},
            {"tokenType": "WTUS", "modelId": "tab_flash_lite_preview", "remainingFraction": 1},
            {
              "resetTime": "2026-11-18T17:32:49Z",
              "tokenType": "WTUS",
              "modelId": "gemini-4-hydrogen-sonar",
              "remainingFraction": 0.8
            },
            {
              "resetTime": "2026-11-18T20:19:06Z",
              "tokenType": "WTUS",
              "modelId": "claude-opus-5-5-high",
              "remainingFraction": 0.5
            }
          ]
        }"#;

        let quota = parse_antigravity_quota(body).unwrap().unwrap();
        assert_eq!(quota.windows.len(), 2);
        assert_eq!(quota.windows[0].scope.as_deref(), Some("Gemini"));
        assert_eq!(quota.windows[0].label, "period");
        assert!((quota.windows[0].used_percent - 20.0).abs() < 0.001);
        assert_eq!(quota.windows[1].scope.as_deref(), Some("Claude / GPT"));
        assert!((quota.windows[1].used_percent - 50.0).abs() < 0.001);
    }

    #[test]
    fn treats_missing_remaining_fraction_with_reset_time_as_exhausted() {
        let body = r#"{
          "quotaSummary": {
            "groups": [
              {
                "buckets": [
                  {
                    "bucketId": "gemini-5h",
                    "refreshWindowLabel": "5h",
                    "resetTime": "2026-11-18T17:32:49Z"
                  }
                ]
              }
            ]
          }
        }"#;

        let quota = parse_antigravity_quota(body).unwrap().unwrap();
        assert_eq!(quota.windows.len(), 1);
        assert_eq!(quota.windows[0].label, "5h");
        // 没有组名的并成整个账户的。
        assert_eq!(quota.windows[0].scope, None);
        assert_eq!(quota.windows[0].used_percent, 100.0);
        assert!(quota.windows[0].resets_at_ms.is_some());
    }

    #[test]
    fn ignores_empty_or_internal_only_buckets() {
        assert_eq!(parse_antigravity_quota(r#"{"buckets":[]}"#).unwrap(), None);
        assert_eq!(
            parse_antigravity_quota(
                r#"{"buckets":[{"modelId":"chat_20706","remainingFraction":1}]}"#
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn extracts_token_from_go_keyring_base64_and_plain_json() {
        let payload = r#"{"token":{"access_token":"ya29.test-token"},"auth_method":"consumer"}"#;
        let encoded = format!(
            "go-keyring-base64:{}",
            base64::engine::general_purpose::STANDARD.encode(payload)
        );
        assert_eq!(
            antigravity_token_from_keyring_secret(&encoded).as_deref(),
            Some("ya29.test-token")
        );
        assert_eq!(
            antigravity_token_from_keyring_secret(payload).as_deref(),
            Some("ya29.test-token")
        );
    }

    #[test]
    fn ignores_non_oauth_api_key_keyring_payloads() {
        let payload = r#"{"token":{"access_token":"AIza-key"},"auth_method":"gemini-api-key"}"#;
        assert_eq!(antigravity_token_from_keyring_secret(payload), None);
    }
}
