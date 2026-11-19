//! 账户额度（`quota.*`）：注册表、查询、跨启动缓存。
//!
//! 额度是账户状态，凭据（`~/.codex/auth.json`、钥匙串、Pi 的 `auth.json`……）在运行时
//! 那台机器上，查询和缓存都由运行时做。客户端拿 [`QuotaSnapshot`] 先显示缓存，
//! 定时或用户点击时请运行时 [`refresh`]。任何终端、ACP 或插件会话都不会因为额度
//! 查询收到 prompt 或被改变状态。
//!
//! 拿到的数字缓存到运行时库的 `quota_cache*` 表，客户端启动时先显示，同时查一轮。数字最多
//! 沿用 [`QUOTA_MAX_AGE`]。

pub use smelt_model::quota_service::*;

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::future::join_all;

use crate::agent_kind::ConversationAgentKind;
use crate::antigravity_quota::fetch_antigravity_quota;
use crate::copilot_quota::fetch_copilot_quota;
use crate::pi_quota::{available_pi_quota_providers, fetch_pi_provider_quota};
use crate::provider_quota::{
    ProviderQuotaData, fetch_claude_quota, fetch_codex_quota, fetch_cursor_quota, fetch_grok_quota,
};
use crate::quota_credentials::{CredentialAccess, KeychainBusy, forget_keychain_tokens};
use crate::quota_http::is_transient;

/// 额度数字最多沿用这么久：查询临时失败时沿用的、启动时先显示的缓存，都不超过它。
const QUOTA_MAX_AGE: Duration = Duration::from_secs(3600);

type QuotaFetchFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<Option<ProviderQuotaData>>> + Send + 'static>>;
type QuotaFetcher = fn(CredentialAccess) -> QuotaFetchFuture;

struct StaticQuotaProvider {
    agent: ConversationAgentKind,
    fetch: QuotaFetcher,
}

/// 账户额度 Provider 注册表，同时决定侧栏展示顺序。没有已知只读额度接口的
/// Agent 不注册；以后接入时只需增加 fetcher 和这一行，不再修改刷新/状态/UI。
static QUOTA_PROVIDERS: &[StaticQuotaProvider] = &[
    StaticQuotaProvider {
        agent: ConversationAgentKind::Codex,
        fetch: fetch_codex_quota_data,
    },
    StaticQuotaProvider {
        agent: ConversationAgentKind::Claude,
        fetch: fetch_claude_quota_data,
    },
    StaticQuotaProvider {
        agent: ConversationAgentKind::Copilot,
        fetch: fetch_copilot_quota_data,
    },
    StaticQuotaProvider {
        agent: ConversationAgentKind::Grok,
        fetch: fetch_grok_quota_data,
    },
    StaticQuotaProvider {
        agent: ConversationAgentKind::Cursor,
        fetch: fetch_cursor_quota_data,
    },
    StaticQuotaProvider {
        agent: ConversationAgentKind::Antigravity,
        fetch: fetch_antigravity_quota_data,
    },
];

/// 开着查询的账户：[`all_quota_targets`] 去掉用户关掉的。只在刷新路径上调用，不进渲染。
pub fn quota_targets() -> Vec<QuotaTarget> {
    settings().enabled_targets()
}

/// 全部账户的开关和钥匙串拒绝状态。
pub fn settings() -> QuotaSettings {
    settings_from(all_quota_targets(), &load_prefs())
}

/// 打开或关掉一家的额度查询，返回更新后的设置。
///
/// 关掉时丢掉内存里的令牌：这家之后不读凭据、不发请求。打开时不查，由客户端按用户手动
/// 刷新发起（可能要弹钥匙串窗口，得是用户的操作）。
pub fn set_enabled(params: QuotaSetEnabledParams) -> Result<QuotaSettings, String> {
    let provider = params.target.id();
    let store = crate::sqlite_state::default_sqlite_store()?;
    store
        .set_quota_provider_enabled(&provider, params.enabled)
        .map_err(|error| error.to_string())?;
    if !params.enabled {
        forget_keychain_tokens(&provider);
    }
    Ok(settings())
}

fn load_prefs() -> Vec<smelt_store::QuotaProviderPref> {
    crate::sqlite_state::default_sqlite_store()
        .ok()
        .and_then(|store| store.quota_provider_prefs().ok())
        .unwrap_or_default()
}

/// 没有偏好行就是默认：开启、未被拒绝。
fn settings_from(
    targets: Vec<QuotaTarget>,
    prefs: &[smelt_store::QuotaProviderPref],
) -> QuotaSettings {
    QuotaSettings {
        providers: targets
            .into_iter()
            .map(|target| {
                let id = target.id();
                let pref = prefs.iter().find(|pref| pref.provider_id == id);
                QuotaProviderSetting {
                    target,
                    enabled: pref.is_none_or(|pref| pref.enabled),
                    keychain_denied: pref.is_some_and(|pref| pref.keychain_denied),
                }
            })
            .collect(),
    }
}

/// 全部账户：固定的几家 CLI，加上 Pi 当前登录过的订阅型 provider。
///
/// Pi 那部分要读 `auth.json`，所以这个函数只在刷新路径上调用，不进渲染。
fn all_quota_targets() -> Vec<QuotaTarget> {
    QUOTA_PROVIDERS
        .iter()
        .map(|provider| QuotaTarget::agent(provider.agent))
        .chain(
            available_pi_quota_providers()
                .into_iter()
                .map(|provider| QuotaTarget::pi(provider.id)),
        )
        .collect()
}

fn fetch_quota(target: QuotaTarget, access: CredentialAccess) -> QuotaFetchFuture {
    if let Some(provider) = target.provider {
        return Box::pin(async move { fetch_pi_provider_quota(provider).await });
    }
    match QUOTA_PROVIDERS
        .iter()
        .find(|provider| provider.agent == target.agent)
    {
        Some(provider) => (provider.fetch)(access),
        None => {
            let id = target.id();
            Box::pin(async move { Err(anyhow::anyhow!("no quota fetcher registered for {id}")) })
        }
    }
}

fn fetch_codex_quota_data(_access: CredentialAccess) -> QuotaFetchFuture {
    Box::pin(async { Ok(fetch_codex_quota().await?.map(ProviderQuotaData::Windows)) })
}

fn fetch_claude_quota_data(access: CredentialAccess) -> QuotaFetchFuture {
    Box::pin(async move {
        Ok(fetch_claude_quota(access)
            .await?
            .map(ProviderQuotaData::Windows))
    })
}

fn fetch_copilot_quota_data(access: CredentialAccess) -> QuotaFetchFuture {
    Box::pin(async move {
        Ok(fetch_copilot_quota(access)
            .await?
            .map(ProviderQuotaData::Copilot))
    })
}

fn fetch_grok_quota_data(_access: CredentialAccess) -> QuotaFetchFuture {
    Box::pin(async { Ok(fetch_grok_quota().await?.map(ProviderQuotaData::Windows)) })
}

fn fetch_cursor_quota_data(access: CredentialAccess) -> QuotaFetchFuture {
    Box::pin(async move {
        Ok(fetch_cursor_quota(access)
            .await?
            .map(ProviderQuotaData::Windows))
    })
}

fn fetch_antigravity_quota_data(access: CredentialAccess) -> QuotaFetchFuture {
    Box::pin(async move {
        Ok(fetch_antigravity_quota(access)
            .await?
            .map(ProviderQuotaData::Windows))
    })
}

struct ProviderQuotaFetchResult(Vec<(QuotaTarget, anyhow::Result<Option<ProviderQuotaData>>)>);

impl ProviderQuotaFetchResult {
    fn failed(target: Option<QuotaTarget>, error: String) -> Self {
        Self(
            refresh_scope(target)
                .into_iter()
                .map(|target| (target, Err(anyhow::anyhow!(error.clone()))))
                .collect(),
        )
    }

    /// `previous` 是缓存里上次的结果，临时失败时沿用。
    fn into_statuses(
        self,
        previous: &BTreeMap<String, QuotaStatus>,
        now_ms: u64,
    ) -> Vec<(QuotaTarget, QuotaStatus)> {
        self.0
            .into_iter()
            // 还在等钥匙串授权：这一轮没有结果，不写状态，上次的额度和缓存保持原样。
            .filter(|(_, outcome)| {
                !matches!(outcome, Err(error) if error.downcast_ref::<KeychainBusy>().is_some())
            })
            .map(|(target, outcome)| {
                let status = apply_quota_result(outcome, previous.get(&target.id()), now_ms);
                (target, status)
            })
            .collect()
    }
}

/// 一轮刷新实际覆盖的账户：`Some` = 用户点的那一个，`None` = 开着查询的全部。关掉的账户
/// 就算点名也不查：请求可能是开关改动之前发出的。
fn refresh_scope(target: Option<QuotaTarget>) -> Vec<QuotaTarget> {
    let enabled = quota_targets();
    match target {
        Some(target) => enabled.into_iter().filter(|it| *it == target).collect(),
        None => enabled,
    }
}

/// 点名查一家才算用户发起，只有这时才可能弹钥匙串窗口。
fn credential_access(target: Option<QuotaTarget>) -> CredentialAccess {
    match target {
        Some(_) => CredentialAccess::UserInitiated,
        None => CredentialAccess::Background,
    }
}

async fn fetch_provider_quotas(target: Option<QuotaTarget>) -> ProviderQuotaFetchResult {
    let access = credential_access(target);
    let results = join_all(
        refresh_scope(target)
            .into_iter()
            .map(|target| async move { (target, fetch_quota(target, access).await) }),
    )
    .await;
    ProviderQuotaFetchResult(results)
}

/// 缓存里的额度，按 [`QuotaTarget::id`] 排。只有拿到了数字的账户。
fn load_cache() -> BTreeMap<String, QuotaStatus> {
    crate::sqlite_state::default_sqlite_store()
        .ok()
        .and_then(|store| store.quota_cache().ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(status_from_record)
        .collect()
}

fn save_cache(statuses: &BTreeMap<String, QuotaStatus>) {
    let records = statuses
        .iter()
        .filter_map(|(id, status)| record_from_status(id, status))
        .collect::<Vec<_>>();
    if let Ok(store) = crate::sqlite_state::default_sqlite_store() {
        let _ = store.put_quota_cache(&records);
    }
}

/// 没有数字的状态不落盘。
fn record_from_status(
    id: &str,
    status: &QuotaStatus,
) -> Option<smelt_store::QuotaCacheStatusRecord> {
    let quota = match status.quota.as_ref()? {
        ProviderQuotaData::Windows(quota) => smelt_store::QuotaCacheQuota::Windows(
            quota
                .windows
                .iter()
                .map(|window| smelt_store::QuotaCacheWindowRecord {
                    label: window.label.clone(),
                    used_percent: window.used_percent,
                    resets_at_ms: window.resets_at_ms.and_then(|ms| i64::try_from(ms).ok()),
                    reset_after_seconds: window
                        .reset_after_seconds
                        .and_then(|seconds| i64::try_from(seconds).ok()),
                })
                .collect(),
        ),
        ProviderQuotaData::Copilot(quota) => smelt_store::QuotaCacheQuota::Copilot {
            unit: copilot_unit_name(quota.unit).to_string(),
            remaining: quota.remaining,
            limit: quota.limit,
            used: quota.used,
        },
    };
    Some(smelt_store::QuotaCacheStatusRecord {
        provider_id: id.to_string(),
        checked_at_ms: i64::try_from(status.checked_at_ms).ok()?,
        error: status.error.clone(),
        quota,
    })
}

fn status_from_record(
    record: smelt_store::QuotaCacheStatusRecord,
) -> Option<(String, QuotaStatus)> {
    let quota = match record.quota {
        smelt_store::QuotaCacheQuota::Windows(windows) => {
            ProviderQuotaData::Windows(crate::provider_quota::ProviderQuota {
                windows: windows
                    .into_iter()
                    .map(|window| crate::provider_quota::ProviderQuotaWindow {
                        label: window.label,
                        used_percent: window.used_percent,
                        resets_at_ms: window.resets_at_ms.and_then(|ms| u64::try_from(ms).ok()),
                        reset_after_seconds: window
                            .reset_after_seconds
                            .and_then(|seconds| u64::try_from(seconds).ok()),
                    })
                    .collect(),
            })
        }
        smelt_store::QuotaCacheQuota::Copilot {
            unit,
            remaining,
            limit,
            used,
        } => ProviderQuotaData::Copilot(crate::copilot_quota::CopilotQuota {
            unit: copilot_unit_from_name(&unit)?,
            remaining,
            limit,
            used,
        }),
    };
    let status = QuotaStatus {
        quota: Some(quota),
        checked_at_ms: u64::try_from(record.checked_at_ms).ok()?,
        error: record.error,
    };
    Some((record.provider_id, status))
}

/// 库里 `copilot_unit` 列的取值。
fn copilot_unit_name(unit: crate::copilot_quota::CopilotQuotaUnit) -> &'static str {
    match unit {
        crate::copilot_quota::CopilotQuotaUnit::AiCredits => "ai_credits",
        crate::copilot_quota::CopilotQuotaUnit::PremiumRequests => "premium_requests",
    }
}

fn copilot_unit_from_name(name: &str) -> Option<crate::copilot_quota::CopilotQuotaUnit> {
    match name {
        "ai_credits" => Some(crate::copilot_quota::CopilotQuotaUnit::AiCredits),
        "premium_requests" => Some(crate::copilot_quota::CopilotQuotaUnit::PremiumRequests),
        _ => None,
    }
}

/// 一家这一轮的结果。失败分两种：
/// - 临时的（连不上、超时、对方 5xx 或限流，见 [`is_transient`]）：上次的数字多半还对，
///   不超过 [`QUOTA_MAX_AGE`] 就沿用，带上这次的错误。
/// - 别的（令牌被拒、凭据读不到、返回认不出）：凭据丢了、换了账号时旧数字是错的，清掉，
///   侧栏不显示这一家，缓存里也随之去掉。
fn apply_quota_result(
    result: anyhow::Result<Option<ProviderQuotaData>>,
    previous: Option<&QuotaStatus>,
    now_ms: u64,
) -> QuotaStatus {
    match result {
        Ok(quota) => QuotaStatus {
            quota,
            checked_at_ms: now_ms,
            error: None,
        },
        Err(error) => match previous.filter(|previous| {
            previous.quota.is_some() && is_recent(previous, now_ms) && is_transient(&error)
        }) {
            Some(previous) => QuotaStatus {
                error: Some(error.to_string()),
                ..previous.clone()
            },
            None => QuotaStatus {
                quota: None,
                checked_at_ms: now_ms,
                error: Some(error.to_string()),
            },
        },
    }
}

/// 数字是 [`QUOTA_MAX_AGE`] 以内查到的。
fn is_recent(status: &QuotaStatus, now_ms: u64) -> bool {
    status
        .checked_at_ms
        .saturating_add(QUOTA_MAX_AGE.as_millis() as u64)
        >= now_ms
}

/// 开着查询的账户和缓存里还不旧的结果。客户端启动时先显示这些，同时查一轮。
pub fn snapshot() -> QuotaSnapshot {
    let targets = quota_targets();
    let now_ms = unix_time_ms();
    let statuses = load_cache()
        .into_iter()
        .filter_map(|(id, status)| Some((QuotaTarget::from_id(&id)?, status)))
        .filter(|(target, status)| targets.contains(target) && is_recent(status, now_ms))
        .collect();
    QuotaSnapshot { targets, statuses }
}

/// 查一轮额度并更新缓存：只覆盖这一轮查到的账户，其他账户的缓存保持原样。
/// 每家一次网络往返，几秒。
pub fn refresh(params: QuotaRefreshParams) -> QuotaRefreshResult {
    let target = params.target;
    let result = crate::block_on::block_on_tokio(fetch_provider_quotas(target))
        .unwrap_or_else(|error| ProviderQuotaFetchResult::failed(target, error.to_string()));
    let mut cache = load_cache();
    let statuses = result.into_statuses(&cache, unix_time_ms());
    log_failures(&statuses);
    let targets = quota_targets();
    for (target, status) in &statuses {
        cache.insert(target.id(), status.clone());
    }
    // 关掉的账户不留在缓存里，下次打开时不会先露出一个旧数字。
    cache.retain(|id, _| targets.iter().any(|target| target.id() == *id));
    save_cache(&cache);
    QuotaRefreshResult { targets, statuses }
}

/// 每个账户上次记进日志的失败原因。失败原因只在客户端内存里，事后查「额度怎么没了」要靠日志；
/// 同一个原因连着失败只记一次，不然熔断后每 5 分钟一行。
static LOGGED_FAILURES: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Mutex::default);

fn log_failures(statuses: &[(QuotaTarget, QuotaStatus)]) {
    let mut logged = LOGGED_FAILURES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (target, status) in statuses {
        let id = target.id();
        match &status.error {
            Some(error) => {
                if logged.get(&id) != Some(error) {
                    eprintln!("[quota] {id} 查询失败：{error}");
                    logged.insert(id, error.clone());
                }
            }
            None => {
                if logged.remove(&id).is_some() {
                    eprintln!("[quota] {id} 恢复");
                }
            }
        }
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_quota::{ProviderQuota, ProviderQuotaWindow};

    fn agent_targets() -> Vec<QuotaTarget> {
        QUOTA_PROVIDERS
            .iter()
            .map(|provider| QuotaTarget::agent(provider.agent))
            .collect()
    }

    fn window_quota(used_percent: f64) -> ProviderQuotaData {
        ProviderQuotaData::Windows(ProviderQuota {
            windows: vec![ProviderQuotaWindow {
                label: "5h".into(),
                used_percent,
                resets_at_ms: None,
                reset_after_seconds: None,
            }],
        })
    }

    fn pref(id: &str, enabled: bool, keychain_denied: bool) -> smelt_store::QuotaProviderPref {
        smelt_store::QuotaProviderPref {
            provider_id: id.to_string(),
            enabled,
            keychain_denied,
        }
    }

    /// 熔断状态按这个 id 存，和设置页按 `QuotaTarget::id()` 读的必须是同一个。
    #[test]
    fn keychain_provider_ids_match_their_quota_targets() {
        assert_eq!(
            QuotaTarget::agent(ConversationAgentKind::Claude).id(),
            crate::provider_quota::CLAUDE_QUOTA_PROVIDER
        );
        assert_eq!(
            QuotaTarget::agent(ConversationAgentKind::Copilot).id(),
            crate::copilot_quota::COPILOT_QUOTA_PROVIDER
        );
        assert_eq!(
            QuotaTarget::agent(ConversationAgentKind::Antigravity).id(),
            crate::antigravity_quota::ANTIGRAVITY_QUOTA_PROVIDER
        );
    }

    #[test]
    fn providers_default_to_enabled_and_not_denied() {
        let settings = settings_from(agent_targets(), &[]);
        assert_eq!(settings.enabled_targets(), agent_targets());
        assert!(settings.providers.iter().all(|it| !it.keychain_denied));
    }

    #[test]
    fn a_disabled_provider_stays_listed_but_leaves_the_enabled_targets() {
        let settings = settings_from(
            agent_targets(),
            &[pref("claude", false, false), pref("copilot", true, true)],
        );
        let claude = QuotaTarget::agent(ConversationAgentKind::Claude);
        let copilot = QuotaTarget::agent(ConversationAgentKind::Copilot);
        assert_eq!(settings.providers.len(), agent_targets().len());
        assert!(!settings.enabled_targets().contains(&claude));
        assert!(settings.enabled_targets().contains(&copilot));
        let row = |target| {
            settings
                .providers
                .iter()
                .find(|it| it.target == target)
                .unwrap()
        };
        assert!(row(copilot).keychain_denied);
        assert!(!row(claude).keychain_denied);
    }

    /// 只有点名一家才可能弹窗；定时的全量刷新不行。
    #[test]
    fn only_a_targeted_refresh_may_prompt() {
        assert_eq!(credential_access(None), CredentialAccess::Background);
        assert_eq!(
            credential_access(Some(QuotaTarget::agent(ConversationAgentKind::Claude))),
            CredentialAccess::UserInitiated
        );
    }

    #[test]
    fn quota_registry_preserves_the_existing_display_order() {
        assert_eq!(
            agent_targets(),
            vec![
                QuotaTarget::agent(ConversationAgentKind::Codex),
                QuotaTarget::agent(ConversationAgentKind::Claude),
                QuotaTarget::agent(ConversationAgentKind::Copilot),
                QuotaTarget::agent(ConversationAgentKind::Grok),
                QuotaTarget::agent(ConversationAgentKind::Cursor),
                QuotaTarget::agent(ConversationAgentKind::Antigravity),
            ]
        );
    }

    fn endpoint_error(status: reqwest::StatusCode) -> anyhow::Error {
        crate::quota_http::QuotaEndpointError {
            provider: "Codex",
            status,
        }
        .into()
    }

    fn fetched_at(checked_at_ms: u64) -> QuotaStatus {
        QuotaStatus {
            quota: Some(window_quota(40.0)),
            checked_at_ms,
            error: None,
        }
    }

    const NOW: u64 = 10_000_000;

    /// 令牌被拒这类失败：旧数字可能是别的账户的，清掉。
    #[test]
    fn a_definitive_failure_drops_the_previous_quota() {
        let previous = fetched_at(NOW - 60_000);
        let status = apply_quota_result(
            Err(endpoint_error(reqwest::StatusCode::UNAUTHORIZED)),
            Some(&previous),
            NOW,
        );
        assert_eq!(status.quota, None);
        assert_eq!(status.checked_at_ms, NOW);
        assert!(status.error.is_some());
    }

    /// 对方 5xx、限流这类临时失败：一小时内的数字照旧显示，时间还是查到它的那次，带上这次的错误。
    #[test]
    fn a_transient_failure_keeps_a_recent_quota() {
        let previous = fetched_at(NOW - 60_000);
        for code in [
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
        ] {
            let status = apply_quota_result(Err(endpoint_error(code)), Some(&previous), NOW);
            assert_eq!(status.quota, previous.quota);
            assert_eq!(status.checked_at_ms, previous.checked_at_ms);
            assert!(status.error.is_some());
        }
    }

    #[test]
    fn a_transient_failure_does_not_keep_a_quota_past_the_max_age() {
        let max_age = QUOTA_MAX_AGE.as_millis() as u64;
        let old = fetched_at(NOW - max_age - 1);
        let status = apply_quota_result(
            Err(endpoint_error(reqwest::StatusCode::BAD_GATEWAY)),
            Some(&old),
            NOW,
        );
        assert_eq!(status.quota, None);

        let status = apply_quota_result(
            Err(endpoint_error(reqwest::StatusCode::BAD_GATEWAY)),
            None,
            NOW,
        );
        assert_eq!(status.quota, None);
        assert_eq!(status.checked_at_ms, NOW);
    }

    /// 等授权窗口不是额度失效：这一家不出现在结果里，客户端和缓存都保留上次的数字。
    #[test]
    fn waiting_for_keychain_approval_keeps_the_previous_quota() {
        let claude = QuotaTarget::agent(ConversationAgentKind::Claude);
        let codex = QuotaTarget::agent(ConversationAgentKind::Codex);

        let statuses = ProviderQuotaFetchResult(vec![
            (claude, Err(KeychainBusy.into())),
            (codex, Err(anyhow::anyhow!("offline"))),
        ])
        .into_statuses(&BTreeMap::new(), NOW);

        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].0, codex);
    }

    #[test]
    fn cached_quota_round_trips_through_the_store_record() {
        let windows = QuotaStatus {
            quota: Some(window_quota(12.0)),
            checked_at_ms: 9,
            error: Some("stale".into()),
        };
        let copilot = QuotaStatus {
            quota: Some(ProviderQuotaData::Copilot(
                crate::copilot_quota::CopilotQuota {
                    unit: crate::copilot_quota::CopilotQuotaUnit::AiCredits,
                    remaining: 1.0,
                    limit: Some(2.0),
                    used: None,
                },
            )),
            checked_at_ms: 10,
            error: None,
        };
        for (id, status) in [("claude", windows), ("pi:github-copilot", copilot)] {
            let record = record_from_status(id, &status).unwrap();
            assert_eq!(status_from_record(record), Some((id.to_string(), status)));
        }

        let without_quota = QuotaStatus {
            quota: None,
            checked_at_ms: 9,
            error: Some("HTTP 401".into()),
        };
        assert_eq!(record_from_status("codex", &without_quota), None);
    }
}
