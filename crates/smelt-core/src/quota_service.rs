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

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::future::join_all;

use crate::agent_kind::ConversationAgentKind;
use crate::copilot_quota::{CopilotQuotaStatus as CachedCopilotQuotaStatus, fetch_copilot_quota};
use crate::pi_quota::{available_pi_quota_providers, fetch_pi_provider_quota};
use crate::provider_quota::{
    ProviderQuotaData, ProviderQuotaStatus as CachedWindowQuotaStatus, fetch_claude_quota,
    fetch_codex_quota, fetch_cursor_quota, fetch_grok_quota,
};

/// 缓存比这个新就跳过启动首查。额度是慢变化的账户状态，1 小时内直接展示
/// 上次结果不会让用户看到明显过期的数据。
const CACHE_STALE_AFTER: Duration = Duration::from_secs(3600);

type QuotaFetchFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<Option<ProviderQuotaData>>> + Send + 'static>>;
type QuotaFetcher = fn() -> QuotaFetchFuture;

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
];

/// 本次要查的全部账户：固定的几家 CLI，加上 Pi 当前登录过的订阅型 provider。
///
/// Pi 那部分要读 `auth.json`，所以这个函数只在刷新路径上调用，不进渲染。
pub fn quota_targets() -> Vec<QuotaTarget> {
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

fn fetch_quota(target: QuotaTarget) -> QuotaFetchFuture {
    if let Some(provider) = target.provider {
        return Box::pin(async move { fetch_pi_provider_quota(provider).await });
    }
    match QUOTA_PROVIDERS
        .iter()
        .find(|provider| provider.agent == target.agent)
    {
        Some(provider) => (provider.fetch)(),
        None => {
            let id = target.id();
            Box::pin(async move { Err(anyhow::anyhow!("no quota fetcher registered for {id}")) })
        }
    }
}

fn fetch_codex_quota_data() -> QuotaFetchFuture {
    Box::pin(async { Ok(fetch_codex_quota().await?.map(ProviderQuotaData::Windows)) })
}

fn fetch_claude_quota_data() -> QuotaFetchFuture {
    Box::pin(async { Ok(fetch_claude_quota().await?.map(ProviderQuotaData::Windows)) })
}

fn fetch_copilot_quota_data() -> QuotaFetchFuture {
    Box::pin(async { Ok(fetch_copilot_quota().await?.map(ProviderQuotaData::Copilot)) })
}

fn fetch_grok_quota_data() -> QuotaFetchFuture {
    Box::pin(async { Ok(fetch_grok_quota().await?.map(ProviderQuotaData::Windows)) })
}

fn fetch_cursor_quota_data() -> QuotaFetchFuture {
    Box::pin(async { Ok(fetch_cursor_quota().await?.map(ProviderQuotaData::Windows)) })
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
    /// 格式版本只用于判断旧快照有没有覆盖当时的 provider。覆盖范围由
    /// `provider_ids` 决定，新增 provider 不再需要手工递增版本号。
    pub schema_version: u32,
    /// 本次缓存生成时注册过的全部 provider，包括没有凭据、未拿到额度的项。
    /// 用 BTreeSet 保证持久化顺序稳定。
    provider_ids: BTreeSet<String>,
    /// 真正拿到额度的项。键使用 `QuotaTarget::id()`。
    providers: BTreeMap<String, CachedQuotaStatus>,
}

/// 当前格式开始使用动态 provider map。以后新增 provider 不需要改这个版本。
const QUOTA_CACHE_VERSION: u32 = 3;

/// 旧 schema 的覆盖范围是历史契约，不能用当前注册表倒推，否则新增 provider 后
/// 会误把旧缓存当成已覆盖。
const LEGACY_QUOTA_PROVIDER_IDS_V0: &[&str] = &["codex", "claude", "copilot"];
const LEGACY_QUOTA_PROVIDER_IDS_V1: &[&str] = &["codex", "claude", "copilot", "grok"];
const LEGACY_QUOTA_PROVIDER_IDS_V2: &[&str] = &["codex", "claude", "copilot", "grok", "cursor"];

impl QuotaCache {
    fn from_statuses(targets: &[QuotaTarget], statuses: &[(QuotaTarget, QuotaStatus)]) -> Self {
        Self {
            saved_at_ms: unix_time_ms(),
            schema_version: QUOTA_CACHE_VERSION,
            provider_ids: targets.iter().map(|target| target.id()).collect(),
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

    /// 时间新鲜且已经覆盖当前所有 provider 时，才跳过启动首查。
    fn can_skip_startup_fetch(&self, targets: &[QuotaTarget]) -> bool {
        if !self.is_fresh() {
            return false;
        }
        let covered = if self.schema_version >= QUOTA_CACHE_VERSION {
            self.provider_ids.iter().map(String::as_str).collect()
        } else {
            let legacy = match self.schema_version {
                0 => LEGACY_QUOTA_PROVIDER_IDS_V0,
                1 => LEGACY_QUOTA_PROVIDER_IDS_V1,
                _ => LEGACY_QUOTA_PROVIDER_IDS_V2,
            };
            legacy.iter().copied().collect::<HashSet<_>>()
        };
        targets
            .iter()
            .all(|target| covered.contains(target.id().as_str()))
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
            .map(|(target, outcome)| (target, apply_quota_result(outcome, checked_at_ms)))
            .collect()
    }
}

/// 一轮刷新实际覆盖的账户：`Some` = 用户点的那一个，`None` = 当前注册的全部。
fn refresh_scope(target: Option<QuotaTarget>) -> Vec<QuotaTarget> {
    match target {
        Some(target) => vec![target],
        None => quota_targets(),
    }
}

async fn fetch_provider_quotas(target: Option<QuotaTarget>) -> ProviderQuotaFetchResult {
    let results = join_all(
        refresh_scope(target)
            .into_iter()
            .map(|target| async move { (target, fetch_quota(target).await) }),
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
        statuses: cache.as_ref().map(QuotaCache::statuses).unwrap_or_default(),
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
    let targets = quota_targets();
    let mut merged: BTreeMap<String, QuotaStatus> = load_quota_cache()
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
        .collect::<Vec<_>>();
    save_quota_cache(&QuotaCache::from_statuses(&targets, &merged));
    QuotaRefreshResult { targets, statuses }
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
        let cache = QuotaCache::from_statuses(&agent_targets(), &statuses);
        assert!(
            !cache.providers.contains_key("codex"),
            "失效的额度不能留在缓存里，下次启动又显示出来"
        );
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

        let cache = QuotaCache::from_statuses(&agent_targets(), &statuses);
        let snapshot = snapshot_from_cache(&cache).unwrap();
        assert_eq!(snapshot.statuses.len(), 1);
        assert_eq!(snapshot.statuses[0].provider_id, "claude");
        let expected_provider_ids = agent_targets()
            .into_iter()
            .map(QuotaTarget::id)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            snapshot
                .provider_ids
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>(),
            expected_provider_ids,
            "缓存覆盖范围必须从 provider 注册表生成"
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

        let cache = QuotaCache::from_statuses(&targets, &statuses);

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

    #[test]
    fn startup_skip_requires_every_registered_provider_id() {
        let mut cache = QuotaCache::from_statuses(&agent_targets(), &[]);
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
        let mut cache = QuotaCache::from_statuses(&agent_targets(), &[]);
        cache.saved_at_ms = unix_time_ms();

        let mut targets = agent_targets();
        targets.push(QuotaTarget::pi("anthropic"));

        assert!(!cache.can_skip_startup_fetch(&targets));
    }
}
