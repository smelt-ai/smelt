//! 账户额度（`quota.*`）：注册表、查询、跨启动缓存。
//!
//! 额度是账户状态，凭据（`~/.codex/auth.json`、钥匙串、Pi 的 `auth.json`……）在运行时
//! 那台机器上，查询和缓存都由运行时做。客户端拿 [`QuotaSnapshot`] 先显示缓存，
//! 定时或用户点击时请运行时 [`refresh`]。任何终端、ACP 或插件会话都不会因为额度
//! 查询收到 prompt 或被改变状态。
//!
//! 结果缓存到运行时库的 `quota_cache*` 表：缓存新鲜（`CACHE_STALE_AFTER` 内）且
//! 覆盖了当前全部账户时，客户端可以跳过启动首查。

pub use smelt_model::quota_service::*;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::future::join_all;

use crate::agent_kind::ConversationAgentKind;
use crate::antigravity_quota::fetch_antigravity_quota;
use crate::copilot_quota::{CopilotQuotaStatus as CachedCopilotQuotaStatus, fetch_copilot_quota};
use crate::pi_quota::{available_pi_quota_providers, fetch_pi_provider_quota};
use crate::provider_quota::{
    ProviderQuotaData, ProviderQuotaStatus as CachedWindowQuotaStatus, fetch_claude_quota,
    fetch_codex_quota, fetch_cursor_quota, fetch_grok_quota,
};
use crate::quota_credentials::{CredentialAccess, KeychainBusy, forget_keychain_tokens};

/// 缓存比这个新就跳过启动首查。额度是慢变化的账户状态，1 小时内直接展示
/// 上次结果不会让用户看到明显过期的数据。
const CACHE_STALE_AFTER: Duration = Duration::from_secs(3600);

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

/// 跨启动的额度缓存（存进运行时库的 `quota_cache*` 表）。
#[derive(Clone)]
enum CachedQuotaStatus {
    Windows(CachedWindowQuotaStatus),
    Copilot(CachedCopilotQuotaStatus),
}

impl CachedQuotaStatus {
    fn from_status(status: &QuotaStatus) -> Option<Self> {
        match status.quota.as_ref()? {
            ProviderQuotaData::Windows(quota) => Some(Self::Windows(CachedWindowQuotaStatus {
                quota: Some(quota.clone()),
                checked_at_ms: status.checked_at_ms,
                error: status.error.clone(),
            })),
            ProviderQuotaData::Copilot(quota) => Some(Self::Copilot(CachedCopilotQuotaStatus {
                quota: Some(quota.clone()),
                checked_at_ms: status.checked_at_ms,
                error: status.error.clone(),
            })),
        }
    }

    fn into_status(self) -> QuotaStatus {
        match self {
            Self::Windows(status) => QuotaStatus {
                quota: status.quota.map(ProviderQuotaData::Windows),
                checked_at_ms: status.checked_at_ms,
                error: status.error,
            },
            Self::Copilot(status) => QuotaStatus {
                quota: status.quota.map(ProviderQuotaData::Copilot),
                checked_at_ms: status.checked_at_ms,
                error: status.error,
            },
        }
    }
}

#[derive(Clone, Default)]
struct QuotaCache {
    /// 上次写入时间（Unix 毫秒），用于判断缓存是否还新鲜。
    pub saved_at_ms: u64,
    /// 格式版本。和 [`QUOTA_CACHE_VERSION`] 不同就不认 `provider_ids`，启动照查一次。
    pub schema_version: u32,
    /// 有定论的账户：拿到了额度，或确认没有凭据。上次查失败的不在里面，下次启动就不跳过
    /// 首查，不然它要等一整个刷新周期才露面。用 BTreeSet 保证持久化顺序稳定。
    provider_ids: BTreeSet<String>,
    /// 真正拿到额度的项。键使用 `QuotaTarget::id()`。
    providers: BTreeMap<String, CachedQuotaStatus>,
}

/// 4 起 `provider_ids` 只记有定论的账户（3 及以前是当时注册过的全部）。新增 provider 不用改它。
const QUOTA_CACHE_VERSION: u32 = 4;

impl QuotaCache {
    fn new(settled: BTreeSet<String>, statuses: &[(QuotaTarget, QuotaStatus)]) -> Self {
        Self {
            saved_at_ms: unix_time_ms(),
            schema_version: QUOTA_CACHE_VERSION,
            provider_ids: settled,
            // 只缓存真正拿到 quota 的项；失败态（quota 为 None）不落盘。
            providers: statuses
                .iter()
                .filter_map(|(target, status)| {
                    CachedQuotaStatus::from_status(status).map(|status| (target.id(), status))
                })
                .collect(),
        }
    }

    fn is_fresh(&self) -> bool {
        let now = unix_time_ms();
        self.saved_at_ms
            .saturating_add(CACHE_STALE_AFTER.as_millis() as u64)
            >= now
    }

    /// 时间新鲜、当前每个账户都有定论时，才跳过启动首查。
    fn can_skip_startup_fetch(&self, targets: &[QuotaTarget]) -> bool {
        self.is_fresh()
            && self.schema_version == QUOTA_CACHE_VERSION
            && targets
                .iter()
                .all(|target| self.provider_ids.contains(&target.id()))
    }

    fn statuses(&self) -> Vec<(QuotaTarget, QuotaStatus)> {
        self.providers
            .iter()
            .filter_map(|(id, status)| {
                QuotaTarget::from_id(id).map(|target| (target, status.clone().into_status()))
            })
            .collect()
    }
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

    fn into_statuses(self) -> Vec<(QuotaTarget, QuotaStatus)> {
        let checked_at_ms = unix_time_ms();
        self.0
            .into_iter()
            // 还在等钥匙串授权：这一轮没有结果，不写状态，上次的额度和缓存保持原样。
            .filter(|(_, outcome)| {
                !matches!(outcome, Err(error) if error.downcast_ref::<KeychainBusy>().is_some())
            })
            .map(|(target, outcome)| (target, apply_quota_result(outcome, checked_at_ms)))
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

fn load_quota_cache() -> Option<QuotaCache> {
    let store = crate::sqlite_state::default_sqlite_store().ok()?;
    cache_from_snapshot(store.get_quota_cache_snapshot().ok().flatten()?)
}

fn save_quota_cache(cache: &QuotaCache) {
    let Some(snapshot) = snapshot_from_cache(cache) else {
        return;
    };
    if let Ok(store) = crate::sqlite_state::default_sqlite_store() {
        let _ = store.put_quota_cache_snapshot(&snapshot);
    }
}

fn snapshot_from_cache(cache: &QuotaCache) -> Option<smelt_store::QuotaCacheSnapshot> {
    let statuses = cache
        .providers
        .iter()
        .filter_map(|(provider_id, status)| {
            let (checked_at_ms, error, quota) = match status {
                CachedQuotaStatus::Windows(status) => (
                    status.checked_at_ms,
                    status.error.clone(),
                    smelt_store::QuotaCacheQuota::Windows(
                        status
                            .quota
                            .as_ref()?
                            .windows
                            .iter()
                            .map(|window| smelt_store::QuotaCacheWindowRecord {
                                label: window.label.clone(),
                                used_percent: window.used_percent,
                                resets_at_ms: window
                                    .resets_at_ms
                                    .and_then(|ms| i64::try_from(ms).ok()),
                                reset_after_seconds: window
                                    .reset_after_seconds
                                    .and_then(|seconds| i64::try_from(seconds).ok()),
                            })
                            .collect(),
                    ),
                ),
                CachedQuotaStatus::Copilot(status) => {
                    let quota = status.quota.as_ref()?;
                    (
                        status.checked_at_ms,
                        status.error.clone(),
                        smelt_store::QuotaCacheQuota::Copilot {
                            unit: copilot_unit_name(quota.unit).to_string(),
                            remaining: quota.remaining,
                            limit: quota.limit,
                            used: quota.used,
                        },
                    )
                }
            };
            Some(smelt_store::QuotaCacheStatusRecord {
                provider_id: provider_id.clone(),
                checked_at_ms: i64::try_from(checked_at_ms).ok()?,
                error,
                quota,
            })
        })
        .collect();
    Some(smelt_store::QuotaCacheSnapshot {
        saved_at_ms: i64::try_from(cache.saved_at_ms).ok()?,
        schema_version: cache.schema_version,
        provider_ids: cache.provider_ids.iter().cloned().collect(),
        statuses,
    })
}

fn cache_from_snapshot(snapshot: smelt_store::QuotaCacheSnapshot) -> Option<QuotaCache> {
    let providers = snapshot
        .statuses
        .into_iter()
        .filter_map(|record| {
            let checked_at_ms = u64::try_from(record.checked_at_ms.max(0)).ok()?;
            let status = match record.quota {
                smelt_store::QuotaCacheQuota::Windows(windows) => {
                    CachedQuotaStatus::Windows(CachedWindowQuotaStatus {
                        quota: Some(crate::provider_quota::ProviderQuota {
                            windows: windows
                                .into_iter()
                                .map(|window| crate::provider_quota::ProviderQuotaWindow {
                                    label: window.label,
                                    used_percent: window.used_percent,
                                    resets_at_ms: window
                                        .resets_at_ms
                                        .and_then(|ms| u64::try_from(ms).ok()),
                                    reset_after_seconds: window
                                        .reset_after_seconds
                                        .and_then(|seconds| u64::try_from(seconds).ok()),
                                })
                                .collect(),
                        }),
                        checked_at_ms,
                        error: record.error,
                    })
                }
                smelt_store::QuotaCacheQuota::Copilot {
                    unit,
                    remaining,
                    limit,
                    used,
                } => CachedQuotaStatus::Copilot(CachedCopilotQuotaStatus {
                    quota: Some(crate::copilot_quota::CopilotQuota {
                        unit: copilot_unit_from_name(&unit)?,
                        remaining,
                        limit,
                        used,
                    }),
                    checked_at_ms,
                    error: record.error,
                }),
            };
            Some((record.provider_id, status))
        })
        .collect();
    Some(QuotaCache {
        saved_at_ms: u64::try_from(snapshot.saved_at_ms.max(0)).ok()?,
        schema_version: snapshot.schema_version,
        provider_ids: snapshot.provider_ids.into_iter().collect(),
        providers,
    })
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

fn apply_quota_result(
    result: anyhow::Result<Option<ProviderQuotaData>>,
    checked_at_ms: u64,
) -> QuotaStatus {
    match result {
        Ok(quota) => QuotaStatus {
            quota,
            checked_at_ms,
            error: None,
        },
        // 失败即失效：不沿用上一次的成功结果（凭据丢失、换号时旧数字是错的），
        // 侧栏不显示这一项，缓存里也随之去掉。
        Err(error) => QuotaStatus {
            quota: None,
            checked_at_ms,
            error: Some(error.to_string()),
        },
    }
}

pub fn snapshot() -> QuotaSnapshot {
    let targets = quota_targets();
    let cache = load_quota_cache();
    QuotaSnapshot {
        fresh: cache
            .as_ref()
            .is_some_and(|cache| cache.can_skip_startup_fetch(&targets)),
        statuses: cache
            .as_ref()
            .map(QuotaCache::statuses)
            .unwrap_or_default()
            .into_iter()
            .filter(|(target, _)| targets.contains(target))
            .collect(),
        targets,
    }
}

/// 查一轮额度并更新缓存：只覆盖这一轮查到的账户，其他账户的缓存保持原样。
/// 每家一次网络往返，几秒。
pub fn refresh(params: QuotaRefreshParams) -> QuotaRefreshResult {
    let target = params.target;
    let result = crate::block_on::block_on_tokio(fetch_provider_quotas(target))
        .unwrap_or_else(|error| ProviderQuotaFetchResult::failed(target, error.to_string()));
    let statuses = result.into_statuses();
    log_failures(&statuses);
    let targets = quota_targets();
    let previous = load_quota_cache();
    let settled = settled_after_round(previous.as_ref(), &statuses, &targets);
    let mut merged: BTreeMap<String, QuotaStatus> = previous
        .map(|cache| cache.statuses())
        .unwrap_or_default()
        .into_iter()
        .map(|(target, status)| (target.id(), status))
        .collect();
    for (target, status) in &statuses {
        merged.insert(target.id(), status.clone());
    }
    let merged = merged
        .into_iter()
        .filter_map(|(id, status)| Some((QuotaTarget::from_id(&id)?, status)))
        // 关掉的账户不留在缓存里，下次打开时不会先露出一个旧数字。
        .filter(|(target, _)| targets.contains(target))
        .collect::<Vec<_>>();
    save_quota_cache(&QuotaCache::new(settled, &merged));
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

/// 这一轮之后有定论的账户：这一轮查了的按结果定（没报错就算），没查的沿用上次的，关掉的去掉。
fn settled_after_round(
    previous: Option<&QuotaCache>,
    round: &[(QuotaTarget, QuotaStatus)],
    targets: &[QuotaTarget],
) -> BTreeSet<String> {
    let mut settled = previous
        .filter(|cache| cache.schema_version == QUOTA_CACHE_VERSION)
        .map(|cache| cache.provider_ids.clone())
        .unwrap_or_default();
    for (target, status) in round {
        if status.error.is_none() {
            settled.insert(target.id());
        } else {
            settled.remove(&target.id());
        }
    }
    settled.retain(|id| targets.iter().any(|target| target.id() == *id));
    settled
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

    fn ids(targets: &[QuotaTarget]) -> BTreeSet<String> {
        targets.iter().map(|target| target.id()).collect()
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

    #[test]
    fn fresh_pre_grok_cache_still_triggers_startup_fetch() {
        let cache = QuotaCache {
            saved_at_ms: unix_time_ms(),
            schema_version: 0,
            ..QuotaCache::default()
        };
        assert!(cache.is_fresh());
        assert!(!cache.can_skip_startup_fetch(&agent_targets()));
    }

    #[test]
    fn fresh_pre_cursor_cache_still_triggers_startup_fetch() {
        let cache = QuotaCache {
            saved_at_ms: unix_time_ms(),
            schema_version: 1,
            ..QuotaCache::default()
        };
        assert!(cache.is_fresh());
        assert!(
            !cache.can_skip_startup_fetch(&agent_targets()),
            "升级到 Cursor 额度后，schema 1 的新鲜缓存也要再查一次"
        );
    }

    /// 刷新失败就是失效：不拿上一次的成功结果冒充当前额度（凭据丢了、账号换了时
    /// 旧数字是错的），也不写进缓存。
    #[test]
    fn failed_refresh_drops_the_previous_quota() {
        let codex = QuotaTarget::agent(ConversationAgentKind::Codex);

        let statuses = ProviderQuotaFetchResult(vec![(codex, Err(anyhow::anyhow!("offline")))])
            .into_statuses();

        assert_eq!(statuses[0].1.quota, None);
        assert_eq!(statuses[0].1.error.as_deref(), Some("offline"));
        let cache = QuotaCache::new(ids(&agent_targets()), &statuses);
        assert!(
            !cache.providers.contains_key("codex"),
            "失效的额度不能留在缓存里，下次启动又显示出来"
        );
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
        .into_statuses();

        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].0, codex);
    }

    #[test]
    fn cache_round_trips_through_the_store_snapshot() {
        let claude = QuotaTarget::agent(ConversationAgentKind::Claude);
        let statuses = vec![(
            claude,
            QuotaStatus {
                quota: Some(window_quota(12.0)),
                checked_at_ms: 9,
                error: Some("stale".into()),
            },
        )];

        let cache = QuotaCache::new(ids(&agent_targets()), &statuses);
        let snapshot = snapshot_from_cache(&cache).unwrap();
        assert_eq!(snapshot.statuses.len(), 1);
        assert_eq!(snapshot.statuses[0].provider_id, "claude");
        assert_eq!(
            snapshot
                .provider_ids
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>(),
            ids(&agent_targets()),
        );

        let cache = cache_from_snapshot(snapshot).unwrap();
        assert_eq!(cache.statuses(), statuses);
    }

    /// Pi 的 provider 和同名的独立 CLI 是两个账户，缓存键不能撞。
    #[test]
    fn pi_targets_get_their_own_cache_keys() {
        let targets = vec![
            QuotaTarget::agent(ConversationAgentKind::Copilot),
            QuotaTarget::pi("github-copilot"),
        ];
        let statuses = vec![
            (targets[0], quota_status(window_quota(10.0))),
            (targets[1], quota_status(window_quota(90.0))),
        ];

        let cache = QuotaCache::new(ids(&targets), &statuses);

        assert!(cache.providers.contains_key("copilot"));
        assert!(cache.providers.contains_key("pi:github-copilot"));
        assert_eq!(cache.statuses().len(), 2);
    }

    fn quota_status(quota: ProviderQuotaData) -> QuotaStatus {
        QuotaStatus {
            quota: Some(quota),
            checked_at_ms: 1,
            error: None,
        }
    }

    /// 上次查失败的账户没有定论：下次启动不跳过首查，不然它要等一整个刷新周期才露面。没凭据
    /// 是定论；这一轮没查的沿用上次；关掉的去掉；旧格式的范围不认。
    #[test]
    fn a_failed_account_is_not_settled_and_forces_the_startup_fetch() {
        let targets = agent_targets();
        let [codex, claude, copilot, ..] = targets[..] else {
            panic!("注册表里至少有三家");
        };
        let mut previous = QuotaCache::new(ids(&[codex, claude, copilot]), &[]);
        let failed = QuotaStatus {
            quota: None,
            checked_at_ms: 1,
            error: Some("HTTP 401".into()),
        };
        let logged_out = QuotaStatus {
            quota: None,
            checked_at_ms: 1,
            error: None,
        };
        let round = vec![
            (claude, failed),
            (QuotaTarget::agent(ConversationAgentKind::Grok), logged_out),
        ];

        let settled = settled_after_round(Some(&previous), &round, &targets[..4]);
        assert_eq!(
            settled,
            ids(&[
                codex,
                copilot,
                QuotaTarget::agent(ConversationAgentKind::Grok)
            ])
        );
        let mut cache = QuotaCache::new(settled, &[]);
        cache.saved_at_ms = unix_time_ms();
        assert!(!cache.can_skip_startup_fetch(&targets[..4]));

        previous.schema_version = 3;
        assert_eq!(
            settled_after_round(Some(&previous), &[], &targets),
            BTreeSet::new()
        );
    }

    #[test]
    fn startup_skip_requires_every_registered_provider_id() {
        let mut cache = QuotaCache::new(ids(&agent_targets()), &[]);
        cache.saved_at_ms = unix_time_ms();
        assert!(cache.can_skip_startup_fetch(&agent_targets()));

        let missing = agent_targets().into_iter().next().unwrap();
        cache.provider_ids.remove(&missing.id());
        assert!(
            !cache.can_skip_startup_fetch(&agent_targets()),
            "注册表新增 provider 后，旧缓存必须自动触发首查"
        );
    }

    /// Pi 里新登录一家订阅 provider 后，覆盖范围变大，旧缓存再新鲜也得重查。
    #[test]
    fn a_newly_logged_in_pi_provider_invalidates_the_startup_skip() {
        let mut cache = QuotaCache::new(ids(&agent_targets()), &[]);
        cache.saved_at_ms = unix_time_ms();

        let mut targets = agent_targets();
        targets.push(QuotaTarget::pi("anthropic"));

        assert!(!cache.can_skip_startup_fetch(&targets));
    }
}
