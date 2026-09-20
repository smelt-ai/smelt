//! Codex / Claude / Grok / Cursor 账户额度查询。
//!
//! 这些额度属于 provider 账户层，不是 ACP 的 `UsageUpdate`（后者只表示当前
//! 会话的上下文窗口）。查询只读取本机已有凭据并调用 provider 的只读接口，
//! 不启动 agent、不发送 prompt，也不把 token 写入日志或快照。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProviderQuotaWindow {
    /// 短显示名，例如 `5h`、`7d`。
    pub label: String,
    /// 当前窗口已使用百分比。
    pub used_percent: f64,
    /// 服务端给出的绝对重置时间（Unix 毫秒）。
    pub resets_at_ms: Option<u64>,
    /// 没有绝对时间时保留相对重置秒数。
    pub reset_after_seconds: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProviderQuota {
    pub windows: Vec<ProviderQuotaWindow>,
}

/// 查询结果总是有检查时间，即使 provider 返回未知格式或没有本地凭据；这样
/// UI 能结束「查询中」，不会在每个 Idle 快照上无限重试。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProviderQuotaStatus {
    pub quota: Option<ProviderQuota>,
    pub checked_at_ms: u64,
    /// 最近一次刷新失败原因；有 quota 时表示当前展示的是上一次成功结果。
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CodexUsageResponse {
    #[serde(default)]
    rate_limit: Option<CodexRateLimit>,
}

#[derive(Debug, Deserialize)]
struct CodexRateLimit {
    #[serde(default)]
    primary_window: Option<CodexRateLimitWindow>,
    #[serde(default)]
    secondary_window: Option<CodexRateLimitWindow>,
}

#[derive(Debug, Deserialize)]
struct CodexRateLimitWindow {
    #[serde(default)]
    used_percent: Option<f64>,
    #[serde(default)]
    limit_window_seconds: Option<u64>,
    #[serde(default)]
    reset_after_seconds: Option<u64>,
    #[serde(default)]
    reset_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ClaudeUsageResponse {
    #[serde(default)]
    five_hour: Option<ClaudeUsagePeriod>,
    #[serde(default)]
    seven_day: Option<ClaudeUsagePeriod>,
}

#[derive(Debug, Deserialize)]
struct ClaudeUsagePeriod {
    #[serde(default)]
    utilization: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
}

const CODEX_USAGE_ENDPOINT: &str = "https://chatgpt.com/backend-api/wham/usage";
const CLAUDE_USAGE_ENDPOINT: &str = "https://claude.ai/api/oauth/usage";
const GROK_BILLING_ENDPOINT: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const CURSOR_USAGE_ENDPOINT: &str =
    "https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage";

/// 使用 Codex CLI 的 ChatGPT OAuth 凭据查询账户限额。
pub async fn fetch_codex_quota() -> anyhow::Result<Option<ProviderQuota>> {
    let (token, account_id) = local_codex_credentials()?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()?;
    let mut request = client
        .get(CODEX_USAGE_ENDPOINT)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "codex-cli");
    if !account_id.is_empty() {
        request = request.header("ChatGPT-Account-ID", account_id);
    }
    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow::anyhow!(
            "Codex quota endpoint returned HTTP {status}"
        ));
    }
    parse_codex_quota(&response.text().await?)
}

/// 使用 Claude Code 的 OAuth 凭据查询账户限额。
pub async fn fetch_claude_quota() -> anyhow::Result<Option<ProviderQuota>> {
    // `ANTHROPIC_AUTH_TOKEN`/`ANTHROPIC_API_KEY` 可能属于自定义兼容网关，
    // 不能拿去请求 claude.ai 的订阅额度接口。这里只接受 Claude Code 的
    // 官方 OAuth 凭据；没有该凭据时表示当前账户没有可查询的官方额度。
    let Some(token) = local_claude_access_token() else {
        return Ok(None);
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()?;
    let response = client
        .get(CLAUDE_USAGE_ENDPOINT)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .header(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15",
        )
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow::anyhow!(
            "Claude quota endpoint returned HTTP {status}"
        ));
    }
    parse_claude_quota(&response.text().await?)
}

/// 使用 Grok CLI 的 grok.com OIDC 凭据查询 SuperGrok 订阅额度。
///
/// 只认 `~/.grok/auth.json`（可用 `GROK_HOME` 覆盖）里浏览器登录写下的
/// bearer，不读 `XAI_API_KEY`：后者是 console.x.ai 的按量 API key，打不开
/// CLI-proxy 的订阅额度接口。没有官方登录凭据时返回 `Ok(None)`，表示当前
/// 账户没有可查询的官方额度。
pub async fn fetch_grok_quota() -> anyhow::Result<Option<ProviderQuota>> {
    let Some(token) = local_grok_access_token() else {
        return Ok(None);
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()?;
    let response = client
        .get(GROK_BILLING_ENDPOINT)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "Smelt")
        // Grok CLI / CodexBar 都带这个头，用来标明这是 grok.com 会话 token，
        // 不是 console.x.ai 的 API key。
        .header("x-xai-token-auth", "xai-grok-cli")
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow::anyhow!(
            "Grok quota endpoint returned HTTP {status}"
        ));
    }
    parse_grok_quota(&response.text().await?)
}

/// 使用 Cursor IDE 本地登录态查询套餐额度。
///
/// Cursor 没有官方个人额度 API；这里打的是 Dashboard 同款、未文档化的
/// Connect RPC。凭据只读 Cursor 的 `state.vscdb`（`cursorAuth/accessToken`），
/// 不读 `CURSOR_API_KEY`（那是团队 Admin Key，鉴权模型不同）。没有 IDE
/// 登录态时返回 `Ok(None)`。
pub async fn fetch_cursor_quota() -> anyhow::Result<Option<ProviderQuota>> {
    let Some(token) = local_cursor_access_token() else {
        return Ok(None);
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()?;
    let response = client
        .post(CURSOR_USAGE_ENDPOINT)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::USER_AGENT, "Smelt")
        .header("Connect-Protocol-Version", "1")
        .body("{}")
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow::anyhow!(
            "Cursor quota endpoint returned HTTP {status}"
        ));
    }
    parse_cursor_quota(&response.text().await?)
}

pub fn parse_codex_quota(body: &str) -> anyhow::Result<Option<ProviderQuota>> {
    let response: CodexUsageResponse = serde_json::from_str(body)?;
    let Some(rate_limit) = response.rate_limit else {
        return Ok(None);
    };

    let mut windows = Vec::new();
    if let Some(window) = rate_limit.primary_window {
        push_codex_window(&mut windows, "主", window);
    }
    if let Some(window) = rate_limit.secondary_window {
        push_codex_window(&mut windows, "次", window);
    }
    Ok((!windows.is_empty()).then_some(ProviderQuota { windows }))
}

pub fn parse_claude_quota(body: &str) -> anyhow::Result<Option<ProviderQuota>> {
    let response: ClaudeUsageResponse = serde_json::from_str(body)?;
    let mut windows = Vec::new();
    if let Some(period) = response.five_hour {
        push_claude_window(&mut windows, "5h", period);
    }
    if let Some(period) = response.seven_day {
        push_claude_window(&mut windows, "7d", period);
    }
    Ok((!windows.is_empty()).then_some(ProviderQuota { windows }))
}

#[derive(Debug, Deserialize)]
struct GrokBillingResponse {
    #[serde(default)]
    config: Option<GrokCreditsConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrokCreditsConfig {
    #[serde(default)]
    credit_usage_percent: Option<f64>,
    #[serde(default)]
    current_period: Option<GrokUsagePeriod>,
    #[serde(default)]
    billing_period_start: Option<String>,
    #[serde(default)]
    billing_period_end: Option<String>,
    #[serde(default)]
    on_demand_cap: Option<GrokCreditsAmount>,
    #[serde(default)]
    on_demand_used: Option<GrokCreditsAmount>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrokUsagePeriod {
    #[serde(default, rename = "type")]
    period_type: Option<String>,
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GrokCreditsAmount {
    #[serde(default)]
    val: Option<f64>,
}

pub fn parse_grok_quota(body: &str) -> anyhow::Result<Option<ProviderQuota>> {
    let response: GrokBillingResponse = serde_json::from_str(body)?;
    let Some(config) = response.config else {
        return Ok(None);
    };

    let used_percent = grok_used_percent(&config);
    let resets_at = config
        .current_period
        .as_ref()
        .and_then(|period| period.end.as_deref())
        .or(config.billing_period_end.as_deref())
        .and_then(parse_rfc3339_ms);
    // 没有百分比、也推不出重置时间，就当这份 payload 不是额度数据。
    if used_percent.is_none() && resets_at.is_none() {
        return Ok(None);
    }

    let label = grok_window_label(&config);
    Ok(Some(ProviderQuota {
        windows: vec![ProviderQuotaWindow {
            label,
            used_percent: used_percent.unwrap_or(0.0),
            resets_at_ms: resets_at,
            reset_after_seconds: None,
        }],
    }))
}

fn grok_used_percent(config: &GrokCreditsConfig) -> Option<f64> {
    if let Some(percent) = finite_percent(config.credit_usage_percent) {
        return Some(percent);
    }
    let cap = config
        .on_demand_cap
        .as_ref()
        .and_then(|amount| amount.val)
        .filter(|value| value.is_finite() && *value > 0.0)?;
    let used = config
        .on_demand_used
        .as_ref()
        .and_then(|amount| amount.val)
        .filter(|value| value.is_finite() && *value >= 0.0)?;
    Some(((used / cap) * 100.0).clamp(0.0, 100.0))
}

fn grok_window_label(config: &GrokCreditsConfig) -> String {
    if let Some(period_type) = config
        .current_period
        .as_ref()
        .and_then(|period| period.period_type.as_deref())
    {
        let kind = period_type.to_ascii_uppercase();
        if kind.contains("WEEKLY") {
            return "7d".to_string();
        }
        if kind.contains("MONTHLY") {
            return "30d".to_string();
        }
        if kind.contains("DAILY") {
            return "1d".to_string();
        }
    }
    let start = config
        .current_period
        .as_ref()
        .and_then(|period| period.start.as_deref())
        .or(config.billing_period_start.as_deref())
        .and_then(parse_rfc3339_ms);
    let end = config
        .current_period
        .as_ref()
        .and_then(|period| period.end.as_deref())
        .or(config.billing_period_end.as_deref())
        .and_then(parse_rfc3339_ms);
    if let (Some(start), Some(end)) = (start, end)
        && end > start
    {
        return format_window_seconds((end - start) / 1000);
    }
    "额度".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorUsageResponse {
    #[serde(default)]
    plan_usage: Option<CursorPlanUsage>,
    #[serde(default, deserialize_with = "deserialize_opt_millis")]
    billing_cycle_start: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_opt_millis")]
    billing_cycle_end: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorPlanUsage {
    #[serde(default)]
    total_spend: Option<f64>,
    #[serde(default)]
    included_spend: Option<f64>,
    #[serde(default)]
    remaining: Option<f64>,
    #[serde(default)]
    limit: Option<f64>,
}

pub fn parse_cursor_quota(body: &str) -> anyhow::Result<Option<ProviderQuota>> {
    let response: CursorUsageResponse = serde_json::from_str(body)?;
    let used_percent = response.plan_usage.as_ref().and_then(cursor_used_percent);
    let resets_at_ms = response.billing_cycle_end;
    if used_percent.is_none() && resets_at_ms.is_none() {
        return Ok(None);
    }
    let label = cursor_window_label(response.billing_cycle_start, response.billing_cycle_end);
    Ok(Some(ProviderQuota {
        windows: vec![ProviderQuotaWindow {
            label,
            used_percent: used_percent.unwrap_or(0.0),
            resets_at_ms,
            reset_after_seconds: None,
        }],
    }))
}

fn cursor_used_percent(usage: &CursorPlanUsage) -> Option<f64> {
    let limit = usage
        .limit
        .filter(|value| value.is_finite() && *value > 0.0)?;
    let used = usage
        .total_spend
        .or(usage.included_spend)
        .or_else(|| {
            usage
                .remaining
                .filter(|value| value.is_finite())
                .map(|remaining| (limit - remaining).max(0.0))
        })
        .filter(|value| value.is_finite() && *value >= 0.0)?;
    Some(((used / limit) * 100.0).clamp(0.0, 100.0))
}

fn cursor_window_label(start_ms: Option<u64>, end_ms: Option<u64>) -> String {
    if let (Some(start), Some(end)) = (start_ms, end_ms)
        && end > start
    {
        return format_window_seconds((end - start) / 1000);
    }
    "月".to_string()
}

fn deserialize_opt_millis<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(json_to_millis))
}

fn json_to_millis(value: serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(number) => number
            .as_u64()
            .or_else(|| number.as_i64().and_then(|value| u64::try_from(value).ok()))
            .or_else(|| {
                number
                    .as_f64()
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .map(|value| value as u64)
            }),
        serde_json::Value::String(raw) => raw.trim().parse().ok(),
        _ => None,
    }
}

fn push_codex_window(
    windows: &mut Vec<ProviderQuotaWindow>,
    fallback_label: &str,
    window: CodexRateLimitWindow,
) {
    let Some(used_percent) = finite_percent(window.used_percent) else {
        return;
    };
    let label = window
        .limit_window_seconds
        .map(format_window_seconds)
        .unwrap_or_else(|| fallback_label.to_string());
    windows.push(ProviderQuotaWindow {
        label,
        used_percent,
        resets_at_ms: window.reset_at.and_then(unix_seconds_to_ms),
        reset_after_seconds: window.reset_after_seconds,
    });
}

fn push_claude_window(
    windows: &mut Vec<ProviderQuotaWindow>,
    label: &str,
    period: ClaudeUsagePeriod,
) {
    let Some(used_percent) = finite_percent(period.utilization) else {
        return;
    };
    windows.push(ProviderQuotaWindow {
        label: label.to_string(),
        used_percent,
        resets_at_ms: period.resets_at.as_deref().and_then(parse_rfc3339_ms),
        reset_after_seconds: None,
    });
}

fn finite_percent(value: Option<f64>) -> Option<f64> {
    value
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 100.0))
}

fn unix_seconds_to_ms(value: i64) -> Option<u64> {
    u64::try_from(value)
        .ok()
        .and_then(|value| value.checked_mul(1000))
}

fn parse_rfc3339_ms(value: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.timestamp_millis())
        .and_then(|millis| u64::try_from(millis).ok())
}

fn format_window_seconds(seconds: u64) -> String {
    if seconds.is_multiple_of(86_400) {
        return format!("{}d", seconds / 86_400);
    }
    if seconds.is_multiple_of(3_600) {
        return format!("{}h", seconds / 3_600);
    }
    if seconds.is_multiple_of(60) {
        return format!("{}m", seconds / 60);
    }
    format!("{}s", seconds)
}

fn local_codex_credentials() -> anyhow::Result<(String, String)> {
    if let Some(token) = non_empty_env("CODEX_ACCESS_TOKEN") {
        return Ok((token, non_empty_env("CODEX_ACCOUNT_ID").unwrap_or_default()));
    }

    let mut paths = Vec::new();
    if let Some(codex_home) = env::var_os("CODEX_HOME") {
        paths.push(PathBuf::from(codex_home).join("auth.json"));
    }
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".codex/auth.json"));
    }
    for path in paths {
        let Ok(body) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
            continue;
        };
        let token = value
            .pointer("/tokens/access_token")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty());
        let Some(token) = token else { continue };
        let account_id = value
            .pointer("/tokens/account_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        return Ok((token.to_string(), account_id));
    }
    Err(anyhow::anyhow!("Codex local credentials were not found"))
}

fn local_grok_access_token() -> Option<String> {
    for path in grok_credential_paths() {
        let Ok(body) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
            continue;
        };
        if let Some(token) = grok_token_from_auth_json(&value) {
            return Some(token);
        }
    }
    None
}

fn grok_credential_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = crate::login_env::grok_home() {
        paths.push(PathBuf::from(home).join("auth.json"));
    }
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".grok/auth.json"));
    }
    paths.sort();
    paths.dedup();
    paths
}

/// 从 Grok CLI 的 `auth.json` 里挑出 grok.com 会话 bearer。
///
/// 优先 `https://auth.x.ai::<client-id>`（当前 SuperGrok OIDC），再退回
/// 旧版 `https://accounts.x.ai/sign-in`，最后才接受任意带 `key` 的条目。
fn local_cursor_access_token() -> Option<String> {
    for path in cursor_state_db_paths() {
        if let Some(token) = read_cursor_token_from_db(&path) {
            return Some(token);
        }
    }
    None
}

fn cursor_state_db_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb"));
        paths.push(home.join(".config/Cursor/User/globalStorage/state.vscdb"));
    }
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME") {
        paths.push(PathBuf::from(xdg).join("Cursor/User/globalStorage/state.vscdb"));
    }
    if let Some(app_data) = env::var_os("APPDATA") {
        paths.push(PathBuf::from(app_data).join("Cursor/User/globalStorage/state.vscdb"));
    }
    paths.sort();
    paths.dedup();
    paths
}

/// 只读打开 Cursor 的 VS Code 状态库，取出登录 accessToken。用系统
/// `sqlite3`，避免给 smelt-core 再加一份 native sqlite 依赖。
fn read_cursor_token_from_db(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    let uri = format!("file:{}?mode=ro", path.display());
    let output = Command::new("sqlite3")
        .args([
            "-batch",
            "-noheader",
            &uri,
            "SELECT value FROM ItemTable WHERE key='cursorAuth/accessToken' LIMIT 1;",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8(output.stdout).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn grok_token_from_auth_json(value: &serde_json::Value) -> Option<String> {
    let object = value.as_object()?;
    let mut legacy = None;
    let mut fallback = None;
    for (key, entry) in object {
        let token = entry
            .get("key")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_string);
        let Some(token) = token else {
            continue;
        };
        if key.starts_with("https://auth.x.ai::") {
            return Some(token);
        }
        if key.eq_ignore_ascii_case("https://accounts.x.ai/sign-in") {
            legacy = Some(token);
            continue;
        }
        if fallback.is_none() {
            fallback = Some(token);
        }
    }
    legacy.or(fallback)
}

fn local_claude_access_token() -> Option<String> {
    if let Some(token) = non_empty_env("CLAUDE_CODE_OAUTH_TOKEN") {
        return Some(token);
    }

    // Claude Code macOS builds keep this JSON in the login keychain. `security -w`
    // prints only the selected secret to stdout; we parse it in memory and never log it.
    // 兼容 cc-statusline 和旧版 Claude Code：先按 service 查询，再按当前用户
    // account 查询新版 Claude Code 写入的条目。
    if cfg!(target_os = "macos") {
        let account = env::var("USER")
            .ok()
            .filter(|value| {
                !value.is_empty()
                    && value.chars().all(|character| {
                        character.is_ascii_alphanumeric() || "._-".contains(character)
                    })
            })
            .unwrap_or_else(|| "claude-code-user".to_string());
        if let Some(token) =
            read_claude_keychain_token(None).or_else(|| read_claude_keychain_token(Some(&account)))
        {
            return Some(token);
        }
    }

    for path in claude_credential_paths() {
        let Ok(body) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
            continue;
        };
        for pointer in [
            "/claudeAiOauth/accessToken",
            "/accessToken",
            "/oauth/accessToken",
        ] {
            if let Some(token) = value
                .pointer(pointer)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|token| !token.is_empty())
            {
                return Some(token.to_string());
            }
        }
    }

    if cfg!(target_os = "linux") {
        let output = Command::new("timeout")
            .args([
                "2",
                "secret-tool",
                "lookup",
                "service",
                "Claude Code-credentials",
            ])
            .output();
        if let Ok(output) = output
            && output.status.success()
            && let Some(token) = claude_token_from_json(&output.stdout)
        {
            return Some(token);
        }
    }

    None
}

fn read_claude_keychain_token(account: Option<&str>) -> Option<String> {
    let mut command = Command::new("security");
    command.args(["find-generic-password"]);
    if let Some(account) = account {
        command.args(["-a", account]);
    }
    let output = command
        .args(["-s", "Claude Code-credentials", "-w"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    claude_token_from_json(&output.stdout)
}

fn claude_token_from_json(body: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    value
        .pointer("/claudeAiOauth/accessToken")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

fn claude_credential_paths() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    [
        home.join(".claude/.credentials.json"),
        home.join(".claude/credentials.json"),
    ]
    .into_iter()
    .collect()
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_codex_primary_and_secondary_windows() {
        let quota = parse_codex_quota(
            r#"{
              "rate_limit": {
                "allowed": true,
                "primary_window": {
                  "used_percent": 11,
                  "limit_window_seconds": 604800,
                  "reset_after_seconds": 596789,
                  "reset_at": 1786847399
                },
                "secondary_window": {
                  "used_percent": 42,
                  "limit_window_seconds": 3600,
                  "reset_after_seconds": 1200,
                  "reset_at": 1786250000
                }
              }
            }"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(quota.windows.len(), 2);
        assert_eq!(quota.windows[0].label, "7d");
        assert_eq!(quota.windows[0].used_percent, 11.0);
        assert_eq!(quota.windows[0].resets_at_ms, Some(1_786_847_399_000));
        assert_eq!(quota.windows[1].label, "1h");
    }

    #[test]
    fn parses_claude_usage_periods() {
        let quota = parse_claude_quota(
            r#"{
              "five_hour": {"utilization": 9, "resets_at": "2026-08-09T12:50:00.184117+00:00"},
              "seven_day": {"utilization": 34, "resets_at": "2026-08-15T15:59:59.184137+00:00"}
            }"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(quota.windows.len(), 2);
        assert_eq!(quota.windows[0].label, "5h");
        assert_eq!(quota.windows[0].used_percent, 9.0);
        assert!(quota.windows[0].resets_at_ms.is_some());
        assert_eq!(quota.windows[1].label, "7d");
    }

    #[test]
    fn ignores_unknown_or_invalid_windows() {
        assert_eq!(parse_codex_quota(r#"{"rate_limit": null}"#).unwrap(), None);
        let quota = parse_claude_quota(
            r#"{"five_hour":{"utilization":null},"seven_day":{"utilization":150}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(quota.windows[0].used_percent, 100.0);
    }

    #[test]
    fn parses_grok_weekly_credit_usage() {
        let quota = parse_grok_quota(
            r#"{
              "config": {
                "currentPeriod": {
                  "type": "USAGE_PERIOD_TYPE_WEEKLY",
                  "start": "2026-08-16T13:27:46.482716+00:00",
                  "end": "2026-08-23T13:27:46.482716+00:00"
                },
                "creditUsagePercent": 2.0,
                "onDemandCap": {"val": 0},
                "onDemandUsed": {"val": 0},
                "billingPeriodStart": "2026-08-16T13:27:46.482716+00:00",
                "billingPeriodEnd": "2026-08-23T13:27:46.482716+00:00"
              }
            }"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(quota.windows.len(), 1);
        assert_eq!(quota.windows[0].label, "7d");
        assert_eq!(quota.windows[0].used_percent, 2.0);
        assert_eq!(quota.windows[0].resets_at_ms, Some(1_787_491_666_482));
    }

    #[test]
    fn parses_grok_on_demand_ratio_when_percent_missing() {
        let quota = parse_grok_quota(
            r#"{
              "config": {
                "currentPeriod": {"type": "USAGE_PERIOD_TYPE_MONTHLY", "end": "2026-09-01T00:00:00Z"},
                "onDemandCap": {"val": 200},
                "onDemandUsed": {"val": 50}
              }
            }"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(quota.windows[0].label, "30d");
        assert_eq!(quota.windows[0].used_percent, 25.0);
    }

    #[test]
    fn treats_grok_period_without_usage_as_zero() {
        let quota =
            parse_grok_quota(r#"{"config":{"currentPeriod":{"end":"2026-08-23T00:00:00Z"}}}"#)
                .unwrap()
                .unwrap();
        assert_eq!(quota.windows[0].label, "额度");
        assert_eq!(quota.windows[0].used_percent, 0.0);
        assert!(quota.windows[0].resets_at_ms.is_some());
    }

    #[test]
    fn ignores_grok_payload_without_config() {
        assert_eq!(parse_grok_quota(r#"{"config":null}"#).unwrap(), None);
        assert_eq!(parse_grok_quota(r#"{}"#).unwrap(), None);
    }

    #[test]
    fn prefers_current_grok_oidc_token_over_legacy() {
        let value = serde_json::json!({
            "https://accounts.x.ai/sign-in": {"key": "legacy-token"},
            "https://auth.x.ai::client-id": {"key": "oidc-token"},
            "other": {"key": "fallback-token"}
        });
        assert_eq!(
            grok_token_from_auth_json(&value).as_deref(),
            Some("oidc-token")
        );
    }

    #[test]
    fn falls_back_to_legacy_grok_sign_in_token() {
        let value = serde_json::json!({
            "https://accounts.x.ai/sign-in": {"key": "legacy-token"},
            "other": {"key": "fallback-token"}
        });
        assert_eq!(
            grok_token_from_auth_json(&value).as_deref(),
            Some("legacy-token")
        );
    }

    #[test]
    fn parses_cursor_plan_usage_from_used_and_limit() {
        let quota = parse_cursor_quota(
            r#"{
              "billingCycleStart": "1786887931000",
              "billingCycleEnd": "1789566331000",
              "planUsage": {
                "totalSpend": 1230,
                "includedSpend": 1230,
                "remaining": 38770,
                "limit": 40000,
                "autoPercentUsed": 0.615,
                "apiPercentUsed": 0,
                "totalPercentUsed": 0.492
              }
            }"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(quota.windows.len(), 1);
        assert_eq!(quota.windows[0].label, "31d");
        assert!((quota.windows[0].used_percent - 3.075).abs() < 0.0001);
        assert_eq!(quota.windows[0].resets_at_ms, Some(1_789_566_331_000));
    }

    #[test]
    fn parses_cursor_billing_cycle_as_number() {
        let quota = parse_cursor_quota(
            r#"{
              "billingCycleEnd": 1789566331000,
              "planUsage": {"includedSpend": 50, "limit": 100}
            }"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(quota.windows[0].used_percent, 50.0);
        assert_eq!(quota.windows[0].resets_at_ms, Some(1_789_566_331_000));
        assert_eq!(quota.windows[0].label, "月");
    }

    #[test]
    fn ignores_cursor_payload_without_usage() {
        assert_eq!(parse_cursor_quota(r#"{}"#).unwrap(), None);
        assert_eq!(parse_cursor_quota(r#"{"planUsage":{}}"#).unwrap(), None);
    }
}
