//! Antigravity（`agy`）账户额度查询与解析。
//!
//! 凭据从 `agy` 写入系统钥匙串的 OAuth 记录读取（`go-keyring-base64:` 编码的 JSON）。
//! 查询路径与 `agy` 一致：优先经 Cloud Code（`daily-cloudcode-pa.googleapis.com`）先调
//! `v1internal:loadCodeAssist` 拿到账户的 `cloudaicompanionProject`，再带上它请求
//! `v1internal:retrieveUserQuotaSummary` 取真实的 `5h` + `7d`（weekly）汇总窗口；
//! 汇总不可用时依次回退到 `v1internal:retrieveUserQuota`（按模型桶归并），以及
//! 无 Cloud Code 权限的账号走 AICode `v1alpha:fetchQuotaStatus`。
//! 令牌仅留在内存和单次请求期间，不写入日志或快照。

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

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
const LOAD_CODE_ASSIST_BODY: &str = r#"{"metadata":{"ideType":"ANTIGRAVITY"}}"#;
/// 服务端按 `User-Agent: antigravity/cli/<semver>` 的版本号过滤汇总组（旧版本或裸 `antigravity`
/// 拿不到新版 Gemini 模型组）；找不到本机 `agy` 时用这个已知可用版本兜底。
const FALLBACK_ANTIGRAVITY_CLI_VERSION: &str = "1.5.19";

struct CachedCliVersion {
    path: PathBuf,
    modified: Option<SystemTime>,
    version: String,
}

static CLI_VERSION_CACHE: LazyLock<Mutex<Option<CachedCliVersion>>> = LazyLock::new(Mutex::default);

struct AntigravityEndpoints<'a> {
    load_code_assist: &'a str,
    cloudcode_summary: &'a str,
    cloudcode_buckets: &'a str,
    aicode_status: &'a str,
}

const DEFAULT_ENDPOINTS: AntigravityEndpoints<'static> = AntigravityEndpoints {
    load_code_assist: "https://daily-cloudcode-pa.googleapis.com/v1internal:loadCodeAssist",
    cloudcode_summary: "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary",
    cloudcode_buckets: "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota",
    aicode_status: "https://aicode.googleapis.com/v1alpha:fetchQuotaStatus",
};

enum EndpointOutcome {
    Ok(String),
    Unsupported(StatusCode),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoadCodeAssistResponse {
    #[serde(default)]
    cloudaicompanion_project: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AntigravityQuotaResponse {
    #[serde(default)]
    groups: Vec<AntigravityQuotaSummaryGroup>,
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
    window: Option<String>,
    #[serde(default)]
    refresh_window_label: Option<String>,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    remaining_fraction: Option<f64>,
    #[serde(default)]
    reset_time: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AntigravityQuotaBucket {
    #[serde(default)]
    bucket_id: Option<String>,
    #[serde(default)]
    window: Option<String>,
    #[serde(default)]
    refresh_window_label: Option<String>,
    #[serde(default)]
    disabled: bool,
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
        Self::from_fields(
            bucket.window.as_deref(),
            bucket.refresh_window_label.as_deref(),
            bucket.bucket_id.as_deref(),
        )
    }

    fn from_fields(
        window: Option<&str>,
        refresh_window_label: Option<&str>,
        bucket_id: Option<&str>,
    ) -> Option<Self> {
        let label = window
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                refresh_window_label
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_default()
            .to_ascii_lowercase();
        let id = bucket_id.unwrap_or_default().trim().to_ascii_lowercase();
        if label == "5h" || id.ends_with("-5h") {
            Some(Self::FiveHour)
        } else if label == "weekly"
            || label == "7d"
            || id.ends_with("-weekly")
            || id.ends_with("-7d")
        {
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
    let user_agent = antigravity_user_agent();
    fetch_antigravity_quota_with_endpoints(&client, token, &user_agent, &DEFAULT_ENDPOINTS).await
}

async fn fetch_antigravity_quota_with_endpoints(
    client: &reqwest::Client,
    token: &str,
    user_agent: &str,
    endpoints: &AntigravityEndpoints<'_>,
) -> anyhow::Result<Option<ProviderQuota>> {
    let project = match post_cloudcode(
        client,
        endpoints.load_code_assist,
        token,
        user_agent,
        LOAD_CODE_ASSIST_BODY,
    )
    .await?
    {
        EndpointOutcome::Ok(body) => Some(parse_load_code_assist_project(&body)),
        EndpointOutcome::Unsupported(_) => None,
    };

    let mut ccpa_succeeded = false;
    if let Some(project) = project {
        let request_body = project_request_body(project.as_deref());
        if let EndpointOutcome::Ok(body) = post_cloudcode(
            client,
            endpoints.cloudcode_summary,
            token,
            user_agent,
            &request_body,
        )
        .await?
        {
            ccpa_succeeded = true;
            if let Some(quota) = parse_antigravity_quota(&body)? {
                return Ok(Some(quota));
            }
        }
        if let EndpointOutcome::Ok(body) = post_cloudcode(
            client,
            endpoints.cloudcode_buckets,
            token,
            user_agent,
            &request_body,
        )
        .await?
        {
            ccpa_succeeded = true;
            if let Some(quota) = parse_antigravity_quota(&body)? {
                return Ok(Some(quota));
            }
        }
    }

    // 无 Cloud Code 权限或未返回有效额度时，回退到 AICode 额度接口。
    match get_aicode(client, endpoints.aicode_status, token, user_agent).await? {
        EndpointOutcome::Ok(body) => parse_antigravity_quota(&body),
        EndpointOutcome::Unsupported(_) if ccpa_succeeded => Ok(None),
        EndpointOutcome::Unsupported(status) => Err(QuotaEndpointError {
            provider: "Antigravity",
            status,
        }
        .into()),
    }
}

async fn post_cloudcode(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    user_agent: &str,
    body: &str,
) -> anyhow::Result<EndpointOutcome> {
    let response = client
        .post(url)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::USER_AGENT, user_agent)
        .body(body.to_string())
        .send()
        .await?;
    classify_endpoint_response(response).await
}

async fn get_aicode(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    user_agent: &str,
) -> anyhow::Result<EndpointOutcome> {
    let response = client
        .get(url)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, user_agent)
        .send()
        .await?;
    classify_endpoint_response(response).await
}

fn antigravity_user_agent() -> String {
    let version = local_antigravity_cli_version()
        .unwrap_or_else(|| FALLBACK_ANTIGRAVITY_CLI_VERSION.to_string());
    format!("antigravity/cli/{version}")
}

fn local_antigravity_cli_version() -> Option<String> {
    let path = resolve_agy_binary()?;
    let modified = std::fs::metadata(&path)
        .ok()
        .and_then(|meta| meta.modified().ok());
    {
        let cache = CLI_VERSION_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = cache.as_ref()
            && entry.path == path
            && entry.modified == modified
        {
            return Some(entry.version.clone());
        }
    }
    let output = Command::new(&path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let version = parse_cli_version(&stdout)?;
    let mut cache = CLI_VERSION_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *cache = Some(CachedCliVersion {
        path,
        modified,
        version: version.clone(),
    });
    Some(version)
}

fn resolve_agy_binary() -> Option<PathBuf> {
    let path = crate::login_env::login_path();
    crate::acp_conn::resolve_in_path("agy", &path)
        .map(PathBuf::from)
        .or_else(|| {
            dirs::home_dir()
                .map(|home| home.join(".local/bin/agy"))
                .filter(|candidate| candidate.is_file())
        })
}

fn parse_cli_version(stdout: &str) -> Option<String> {
    let token = stdout
        .split_whitespace()
        .next()?
        .trim_start_matches(['v', 'V']);
    let mut parts = token.split('.');
    let major = parts.next()?;
    let minor = parts.next()?;
    if major.is_empty()
        || minor.is_empty()
        || !major.bytes().all(|b| b.is_ascii_digit())
        || !minor.bytes().all(|b| b.is_ascii_digit())
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
    {
        return None;
    }
    Some(token.to_string())
}

async fn classify_endpoint_response(
    response: reqwest::Response,
) -> anyhow::Result<EndpointOutcome> {
    let status = response.status();
    if status.is_success() {
        return Ok(EndpointOutcome::Ok(response.text().await?));
    }
    if matches!(status, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN) {
        return Ok(EndpointOutcome::Unsupported(status));
    }
    Err(QuotaEndpointError {
        provider: "Antigravity",
        status,
    }
    .into())
}

fn project_request_body(project: Option<&str>) -> String {
    match project {
        Some(project) => serde_json::json!({ "project": project }).to_string(),
        None => "{}".to_string(),
    }
}

fn parse_load_code_assist_project(body: &str) -> Option<String> {
    let response: LoadCodeAssistResponse = serde_json::from_str(body).ok()?;
    let value = response.cloudaicompanion_project?;
    let project = match &value {
        serde_json::Value::String(id) => Some(id.as_str()),
        serde_json::Value::Object(map) => map.get("id").and_then(serde_json::Value::as_str),
        _ => None,
    };
    project
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// 解析 `retrieveUserQuotaSummary` / `fetchQuotaStatus` / `retrieveUserQuota` 返回的 JSON。
///
/// 优先从顶层 `groups` 或 `quotaSummary.groups` 抽取标准的 `5h` 与 `7d` 窗口，每组模型各一套，
/// 带组名；其次尝试顶层汇总桶；没有汇总时退回按模型池归并 `buckets`，一池一个不知道时长的窗口。
pub fn parse_antigravity_quota(body: &str) -> anyhow::Result<Option<ProviderQuota>> {
    let response: AntigravityQuotaResponse = serde_json::from_str(body)?;
    if !response.groups.is_empty() {
        let windows = parse_summary_groups(&response.groups);
        if !windows.is_empty() {
            return Ok(Some(ProviderQuota { windows }));
        }
    }
    if let Some(summary) = response.quota_summary.as_ref() {
        let windows = parse_summary_groups(&summary.groups);
        if !windows.is_empty() {
            return Ok(Some(ProviderQuota { windows }));
        }
    }
    let flat_summary = parse_flat_summary_buckets(&response.buckets);
    if !flat_summary.is_empty() {
        return Ok(Some(ProviderQuota {
            windows: flat_summary,
        }));
    }
    let windows = parse_bucket_windows(&response.buckets);
    Ok((!windows.is_empty()).then_some(ProviderQuota { windows }))
}

fn parse_summary_groups(
    summary_groups: &[AntigravityQuotaSummaryGroup],
) -> Vec<ProviderQuotaWindow> {
    // 按组出现的先后排；没有组名的几组并成整个账户的一套。
    let mut groups: Vec<(Option<String>, [Option<MergedQuota>; 2])> = Vec::new();
    for group in summary_groups {
        let scope = group.display_name.as_deref().and_then(group_scope);
        let at = match groups.iter().position(|(name, _)| *name == scope) {
            Some(at) => at,
            None => {
                groups.push((scope, [None; 2]));
                groups.len() - 1
            }
        };
        for bucket in &group.buckets {
            if bucket.disabled {
                continue;
            }
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

fn parse_flat_summary_buckets(buckets: &[AntigravityQuotaBucket]) -> Vec<ProviderQuotaWindow> {
    let mut slots: [Option<MergedQuota>; 2] = [None; 2];
    for bucket in buckets {
        if bucket.disabled || bucket.model_id.is_some() || bucket.model.is_some() {
            continue;
        }
        let Some(kind) = SummaryWindow::from_fields(
            bucket.window.as_deref(),
            bucket.refresh_window_label.as_deref(),
            bucket.bucket_id.as_deref(),
        ) else {
            continue;
        };
        let Some(entry) =
            quota_from_fraction(bucket.remaining_fraction, bucket.reset_time.as_deref())
        else {
            continue;
        };
        merge_slot(&mut slots[kind.index()], entry);
    }
    SummaryWindow::ALL
        .into_iter()
        .filter_map(|kind| {
            let quota = slots[kind.index()]?;
            Some(ProviderQuotaWindow {
                label: kind.label().to_string(),
                used_percent: quota.used_percent,
                resets_at_ms: quota.resets_at_ms,
                reset_after_seconds: None,
                scope: None,
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
        if bucket.disabled {
            continue;
        }
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
    // proto3 零值 Timestamp（1970-01-01T00:00:00Z）表示未设置，不算有效重置时间。
    let resets_at_ms = reset_time.and_then(parse_rfc3339_ms).filter(|&ms| ms > 0);
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
    fn parses_cloudcode_quota_summary_with_top_level_groups_and_window_field() {
        let body = r#"{
          "groups": [
            {
              "displayName": "Gemini Models",
              "buckets": [
                {
                  "bucketId": "gemini-weekly",
                  "displayName": "Weekly Limit Remaining",
                  "window": "weekly",
                  "remainingFraction": 0.91,
                  "resetTime": "2026-12-03T07:03:00Z"
                },
                {
                  "bucketId": "gemini-5h",
                  "displayName": "Five Hour Limit Remaining",
                  "window": "5h",
                  "remainingFraction": 0.93,
                  "resetTime": "2026-11-28T01:32:55Z"
                }
              ]
            },
            {
              "displayName": "Claude and GPT models",
              "buckets": [
                {
                  "bucketId": "3p-weekly",
                  "displayName": "Weekly Limit Remaining",
                  "window": "weekly",
                  "remainingFraction": 1,
                  "resetTime": "1970-01-01T00:00:00Z"
                },
                {
                  "bucketId": "3p-5h",
                  "displayName": "Five Hour Limit Remaining",
                  "window": "5h",
                  "remainingFraction": 1
                },
                {
                  "bucketId": "disabled-5h",
                  "window": "5h",
                  "disabled": true,
                  "resetTime": "2026-11-28T05:00:00Z"
                }
              ]
            }
          ]
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
                (Some("Gemini"), "5h", 7),
                (Some("Gemini"), "7d", 9),
                (Some("Claude and GPT"), "5h", 0),
                (Some("Claude and GPT"), "7d", 0),
            ]
        );
        assert_eq!(
            quota.windows[0].resets_at_ms,
            parse_rfc3339_ms("2026-11-28T01:32:55Z")
        );
        assert_eq!(
            quota.windows[1].resets_at_ms,
            parse_rfc3339_ms("2026-12-03T07:03:00Z")
        );
        assert_eq!(quota.windows[2].resets_at_ms, None);
        assert_eq!(quota.windows[3].resets_at_ms, None);
    }

    #[test]
    fn parses_flat_summary_buckets_without_groups() {
        let body = r#"{
          "buckets": [
            {
              "bucketId": "gemini-5h",
              "window": "5h",
              "remainingFraction": 0.65,
              "resetTime": "2026-11-28T01:32:55Z"
            },
            {
              "bucketId": "gemini-weekly",
              "window": "weekly",
              "remainingFraction": 0.8
            }
          ]
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
        assert_eq!(windows, [(None, "5h", 35), (None, "7d", 20)]);
    }

    #[test]
    fn extracts_cloudaicompanion_project_from_string_or_object() {
        assert_eq!(
            parse_load_code_assist_project(r#"{"cloudaicompanionProject":"proj-alpha"}"#)
                .as_deref(),
            Some("proj-alpha")
        );
        assert_eq!(
            parse_load_code_assist_project(r#"{"cloudaicompanionProject":{"id":"proj-beta"}}"#)
                .as_deref(),
            Some("proj-beta")
        );
        assert_eq!(
            parse_load_code_assist_project(r#"{"cloudaicompanionProject":"  "}"#),
            None
        );
    }

    struct RecordedRequest {
        path: String,
        user_agent: String,
        body: String,
    }

    type RecordedRequests = std::sync::Arc<std::sync::Mutex<Vec<RecordedRequest>>>;

    fn mock_antigravity_server(
        routes: Vec<(&'static str, &'static str, &'static str)>,
    ) -> (String, RecordedRequests) {
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let expected = routes.len();
        std::thread::spawn(move || {
            for _ in 0..expected {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                let mut header_end = None;
                let mut content_length = 0usize;
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    if header_end.is_none()
                        && let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n")
                    {
                        header_end = Some(pos + 4);
                        let header = String::from_utf8_lossy(&raw[..pos]);
                        for line in header.lines() {
                            if let Some((name, value)) = line.split_once(':')
                                && name.eq_ignore_ascii_case("content-length")
                            {
                                content_length = value.trim().parse().unwrap_or(0);
                            }
                        }
                    }
                    if let Some(end) = header_end
                        && raw.len() >= end + content_length
                    {
                        break;
                    }
                }
                let head_end = header_end.unwrap_or(raw.len());
                let head = String::from_utf8_lossy(&raw[..head_end]);
                let path = head
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or_default()
                    .to_string();
                let user_agent = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("user-agent")
                            .then(|| value.trim().to_string())
                    })
                    .unwrap_or_default();
                let body = String::from_utf8_lossy(&raw[head_end..]).to_string();
                recorded.lock().unwrap().push(RecordedRequest {
                    path: path.clone(),
                    user_agent,
                    body,
                });
                let (status, resp_body) = routes
                    .iter()
                    .find(|(route_path, _, _)| *route_path == path)
                    .map(|(_, status, resp_body)| (*status, *resp_body))
                    .unwrap_or(("404 Not Found", "{}"));
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{resp_body}",
                    resp_body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (format!("http://{address}"), requests)
    }

    fn local_test_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    #[test]
    fn parses_agy_cli_version_output() {
        assert_eq!(parse_cli_version("1.5.19\n").as_deref(), Some("1.5.19"));
        assert_eq!(
            parse_cli_version("v2.0.0-rc.1 (build 42)").as_deref(),
            Some("2.0.0-rc.1")
        );
        assert_eq!(parse_cli_version("not-a-version"), None);
        assert_eq!(parse_cli_version(""), None);
    }

    #[test]
    fn prefers_cloudcode_quota_summary_with_loaded_project_over_aicode() {
        let (base, requests) = mock_antigravity_server(vec![
            (
                "/v1internal:loadCodeAssist",
                "200 OK",
                r#"{"cloudaicompanionProject":"user-proj-1"}"#,
            ),
            (
                "/v1internal:retrieveUserQuotaSummary",
                "200 OK",
                r#"{"groups":[{"displayName":"Gemini Models","buckets":[{"bucketId":"gemini-5h","window":"5h","remainingFraction":0.93,"resetTime":"2026-11-28T01:32:55Z"},{"bucketId":"gemini-weekly","window":"weekly","remainingFraction":0.91,"resetTime":"2026-12-03T07:03:00Z"}]}]}"#,
            ),
        ]);
        let endpoints = AntigravityEndpoints {
            load_code_assist: &format!("{base}/v1internal:loadCodeAssist"),
            cloudcode_summary: &format!("{base}/v1internal:retrieveUserQuotaSummary"),
            cloudcode_buckets: &format!("{base}/v1internal:retrieveUserQuota"),
            aicode_status: &format!("{base}/v1alpha:fetchQuotaStatus"),
        };

        let quota = crate::block_on::block_on_tokio(fetch_antigravity_quota_with_endpoints(
            &local_test_client(),
            "ya29.test",
            "antigravity/cli/1.5.19",
            &endpoints,
        ))
        .unwrap()
        .unwrap()
        .unwrap();

        assert_eq!(quota.windows.len(), 2);
        assert_eq!(quota.windows[0].label, "5h");
        assert_eq!(quota.windows[0].used_percent.round() as u32, 7);
        assert_eq!(quota.windows[1].label, "7d");
        assert_eq!(quota.windows[1].used_percent.round() as u32, 9);

        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].path, "/v1internal:loadCodeAssist");
        assert_eq!(recorded[0].user_agent, "antigravity/cli/1.5.19");
        assert_eq!(recorded[0].body, LOAD_CODE_ASSIST_BODY);
        assert_eq!(recorded[1].path, "/v1internal:retrieveUserQuotaSummary");
        assert_eq!(recorded[1].user_agent, "antigravity/cli/1.5.19");
        assert_eq!(recorded[1].body, r#"{"project":"user-proj-1"}"#);
    }

    #[test]
    fn falls_back_from_summary_to_cloudcode_buckets_and_from_ccpa_forbidden_to_aicode() {
        let (base, requests) = mock_antigravity_server(vec![
            (
                "/v1internal:loadCodeAssist",
                "200 OK",
                r#"{"cloudaicompanionProject":"user-proj-2"}"#,
            ),
            (
                "/v1internal:retrieveUserQuotaSummary",
                "404 Not Found",
                "{}",
            ),
            (
                "/v1internal:retrieveUserQuota",
                "200 OK",
                r#"{"buckets":[{"modelId":"gemini-4-hydrogen-sonar","remainingFraction":0.94,"resetTime":"2026-11-28T01:32:55Z"}]}"#,
            ),
        ]);
        let endpoints = AntigravityEndpoints {
            load_code_assist: &format!("{base}/v1internal:loadCodeAssist"),
            cloudcode_summary: &format!("{base}/v1internal:retrieveUserQuotaSummary"),
            cloudcode_buckets: &format!("{base}/v1internal:retrieveUserQuota"),
            aicode_status: &format!("{base}/v1alpha:fetchQuotaStatus"),
        };
        let quota = crate::block_on::block_on_tokio(fetch_antigravity_quota_with_endpoints(
            &local_test_client(),
            "ya29.test",
            "antigravity/cli/1.5.19",
            &endpoints,
        ))
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(quota.windows.len(), 1);
        assert_eq!(quota.windows[0].used_percent.round() as u32, 6);
        assert_eq!(
            requests.lock().unwrap()[2].body,
            r#"{"project":"user-proj-2"}"#
        );

        let (base, requests) = mock_antigravity_server(vec![
            ("/v1internal:loadCodeAssist", "403 Forbidden", "{}"),
            (
                "/v1alpha:fetchQuotaStatus",
                "200 OK",
                r#"{"quotaSummary":{"groups":[{"displayName":"Gemini Models","buckets":[{"bucketId":"gemini-5h","refreshWindowLabel":"5h","remainingFraction":0.75}]}]}}"#,
            ),
        ]);
        let endpoints = AntigravityEndpoints {
            load_code_assist: &format!("{base}/v1internal:loadCodeAssist"),
            cloudcode_summary: &format!("{base}/v1internal:retrieveUserQuotaSummary"),
            cloudcode_buckets: &format!("{base}/v1internal:retrieveUserQuota"),
            aicode_status: &format!("{base}/v1alpha:fetchQuotaStatus"),
        };
        let quota = crate::block_on::block_on_tokio(fetch_antigravity_quota_with_endpoints(
            &local_test_client(),
            "ya29.test",
            "antigravity/cli/1.5.19",
            &endpoints,
        ))
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(quota.windows[0].used_percent.round() as u32, 25);
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn propagates_unauthorized_and_transient_cloudcode_errors_without_falling_back_to_aicode() {
        for (status_line, expected) in [
            ("401 Unauthorized", StatusCode::UNAUTHORIZED),
            ("503 Service Unavailable", StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let (base, requests) =
                mock_antigravity_server(vec![("/v1internal:loadCodeAssist", status_line, "{}")]);
            let endpoints = AntigravityEndpoints {
                load_code_assist: &format!("{base}/v1internal:loadCodeAssist"),
                cloudcode_summary: &format!("{base}/v1internal:retrieveUserQuotaSummary"),
                cloudcode_buckets: &format!("{base}/v1internal:retrieveUserQuota"),
                aicode_status: &format!("{base}/v1alpha:fetchQuotaStatus"),
            };
            let error = crate::block_on::block_on_tokio(fetch_antigravity_quota_with_endpoints(
                &local_test_client(),
                "ya29.test",
                "antigravity/cli/1.5.19",
                &endpoints,
            ))
            .unwrap()
            .unwrap_err();
            assert_eq!(
                error.downcast_ref::<QuotaEndpointError>().unwrap().status,
                expected
            );
            assert_eq!(requests.lock().unwrap().len(), 1);
        }
    }

    #[test]
    fn ignores_non_oauth_api_key_keyring_payloads() {
        let payload = r#"{"token":{"access_token":"AIza-key"},"auth_method":"gemini-api-key"}"#;
        assert_eq!(antigravity_token_from_keyring_secret(payload), None);
    }
}
