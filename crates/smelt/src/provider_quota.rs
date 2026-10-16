//! 应用级 provider 额度服务：注册表、刷新循环、跨启动缓存。
//!
//! 额度是账户状态，不属于 ACP 会话。这里只负责「查」和「存」，查到的结果写进
//! [`ProviderStatus`] 全局单例；侧栏和对话模型菜单都从那里读，并靠
//! `observe_global` 收到更新。任何终端、ACP 或插件会话都不会因为额度查询收到
//! prompt 或被改变状态。
//!
//! 结果缓存到主库 `quota_cache` 作用域：启动时先恢复缓存直接显示，缓存新鲜
//! （`CACHE_STALE_AFTER` 内）就跳过启动首查，等定时器或用户手动点击。

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::future::Future;

use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::future::join_all;
use serde::{Deserialize, Serialize};

use smelt_core::agent_kind::ConversationAgentKind;
use smelt_core::copilot_quota::{
    CopilotQuotaStatus as CachedCopilotQuotaStatus, fetch_copilot_quota,
};
use smelt_core::pi_quota::{available_pi_quota_providers, fetch_pi_provider_quota};
use smelt_core::provider_quota::{
    ProviderQuotaStatus as CachedWindowQuotaStatus, fetch_claude_quota, fetch_codex_quota,
    fetch_cursor_quota, fetch_grok_quota,
};

pub(crate) use smelt_core::provider_quota::ProviderQuotaData;
pub(crate) use smelt_ui::provider_status_global::{ProviderStatus, QuotaStatus, QuotaTarget};

use crate::Workspace;

pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_secs(300);

/// 缓存比这个新就跳过启动首查。额度是慢变化的账户状态，1 小时内直接展示
/// 上次结果不会让用户看到明显过期的数据。
pub(crate) const CACHE_STALE_AFTER: Duration = Duration::from_secs(3600);

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
pub(crate) fn quota_targets() -> Vec<QuotaTarget> {
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

/// 跨启动的额度缓存（序列化后存进主库 `quota_cache` 作用域）。
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct QuotaCache {
    /// 上次写入时间（Unix 毫秒），用于判断缓存是否还新鲜。
    pub saved_at_ms: u64,
    /// 格式版本只用于判断旧快照有没有覆盖当时的 provider。覆盖范围由
    /// `provider_ids` 决定，新增 provider 不再需要手工递增版本号。
    #[serde(default)]
    pub schema_version: u32,
    /// 本次缓存生成时注册过的全部 provider，包括没有凭据、未拿到额度的项。
    /// 用 BTreeSet 保证持久化顺序稳定。
    #[serde(default)]
    provider_ids: BTreeSet<String>,
    /// 真正拿到额度的项。键使用 `QuotaTarget::id()`。
    #[serde(default)]
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
    Some(smelt_store::QuotaCacheSnapshot {
        saved_at_ms: i64::try_from(cache.saved_at_ms).ok()?,
        schema_version: cache.schema_version,
        provider_ids: cache.provider_ids.iter().cloned().collect(),
        providers_json: serde_json::to_vec(&cache.providers).ok()?,
    })
}

fn cache_from_snapshot(snapshot: smelt_store::QuotaCacheSnapshot) -> Option<QuotaCache> {
    Some(QuotaCache {
        saved_at_ms: u64::try_from(snapshot.saved_at_ms.max(0)).ok()?,
        schema_version: snapshot.schema_version,
        provider_ids: snapshot.provider_ids.into_iter().collect(),
        providers: serde_json::from_slice(&snapshot.providers_json).ok()?,
    })
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

impl Workspace {
    /// 启动应用级额度刷新循环。它只依赖本地 provider 凭据和账户接口，和 ACP
    /// 是否存在、是否运行、当前会话是什么类型都无关。
    ///
    /// 启动先恢复本地缓存中的上次结果直接显示；缓存新鲜
    /// （`CACHE_STALE_AFTER` 内）就跳过首查，等定时器或用户手动点击刷新。
    pub(crate) fn start_provider_quota_watch(&mut self, cx: &mut gpui::Context<Self>) {
        if self.provider_quota_refresh_tx.is_some() {
            return;
        }
        let targets = quota_targets();
        let cache = load_quota_cache();
        let cache_fresh = cache
            .as_ref()
            .is_some_and(|cache| cache.can_skip_startup_fetch(&targets));
        ProviderStatus::restore_quota(
            &targets,
            cache.as_ref().map(QuotaCache::statuses).unwrap_or_default(),
            cx,
        );
        let (refresh_tx, refresh_rx) = smol::channel::unbounded::<QuotaTarget>();
        self.provider_quota_refresh_tx = Some(refresh_tx);
        cx.spawn(async move |this, cx| {
            // 缓存新鲜 → 首轮也等定时器/信号；过期或没有缓存才立即查一次。
            let mut first = !cache_fresh;
            loop {
                // 本轮要刷新的 provider：None = 全部（定时刷新）；Some = 只刷那家。
                let mut target: Option<QuotaTarget> = None;
                if !first {
                    // 唤醒：用户点击的信号或定时器。注意 race 里的 recv 会**消费**
                    // 信号，所以必须把 recv 拿到的值直接当作本轮信号用——不能像
                    // 原来那样只当唤醒再事后 try_recv（信号已被取走，会退化成
                    // 「点谁查谁 = 全部刷新」）。
                    // 返回值：Some(家) = 用户点的家，None = 定时器（→ 全部刷新）。
                    let wake = smol::future::race(async { refresh_rx.recv().await.ok() }, async {
                        smol::Timer::after(REFRESH_INTERVAL).await;
                        None
                    })
                    .await;
                    target = wake;
                    // 合并积压信号：race 只消费了第一个，剩下的补进来
                    // （最后点击优先）。
                    while let Ok(kind) = refresh_rx.try_recv() {
                        target = Some(kind);
                    }
                }
                first = false;

                if this
                    .update(cx, |_workspace, cx| {
                        ProviderStatus::begin_quota_refresh(&refresh_scope(target), target, cx);
                    })
                    .is_err()
                {
                    break;
                }

                let result = cx
                    .background_executor()
                    .spawn({
                        let target = target;
                        async move {
                            smelt_core::block_on::block_on_tokio(fetch_provider_quotas(target))
                        }
                    })
                    .await
                    .unwrap_or_else(|error| {
                        ProviderQuotaFetchResult::failed(target, error.to_string())
                    });

                // 写回全局单例（侧栏和对话菜单订阅它），再把当前快照落盘。
                // Workspace 没了就停掉轮询：它是这条循环的生命周期宿主。
                if this
                    .update(cx, |_workspace, cx| {
                        ProviderStatus::apply_quota(result.into_statuses(), cx);
                        let targets = ProviderStatus::quota_targets(cx);
                        let statuses = targets
                            .iter()
                            .filter_map(|target| {
                                Some((*target, ProviderStatus::quota_status(*target, cx)?))
                            })
                            .collect::<Vec<_>>();
                        save_quota_cache(&QuotaCache::from_statuses(&targets, &statuses));
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// 请求手动刷新一家的额度。后台的全量刷新只由本模块的启动和定时器发起，
    /// 因而能与侧栏的绿色手动反馈严格区分。
    /// 刷新进行中再点不会丢请求：信号排进队列，当前这轮结束后立刻处理
    /// （积压信号会被合并成一轮，不会连发）。
    pub(crate) fn request_provider_quota_refresh(&mut self, target: QuotaTarget) {
        if let Some(tx) = &self.provider_quota_refresh_tx {
            let _ = tx.try_send(target);
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
    use smelt_core::provider_quota::{ProviderQuota, ProviderQuotaWindow};

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
    fn retired_named_cache_fields_are_ignored() {
        let cache: QuotaCache = serde_json::from_str(
            r#"{
              "saved_at_ms": 1,
              "schema_version": 2,
              "claude": {
                "quota": {"windows": [{"label":"5h","used_percent":9.0,"resets_at_ms":null,"reset_after_seconds":null}]},
                "checked_at_ms": 7
              }
            }"#,
        )
        .unwrap();
        assert!(cache.statuses().is_empty());
        assert!(!cache.can_skip_startup_fetch(&agent_targets()));
    }

    #[test]
    fn old_quota_cache_without_grok_field_still_loads() {
        let cache: QuotaCache = serde_json::from_str(
            r#"{
              "saved_at_ms": 1,
              "copilot": null,
              "codex": null,
              "claude": {
                "quota": {"windows": [{"label":"5h","used_percent":9.0,"resets_at_ms":null,"reset_after_seconds":null}]},
                "checked_at_ms": 1
              }
            }"#,
        )
        .expect("缺字段不能让整份缓存作废");
        assert!(cache.statuses().is_empty());
        assert_eq!(cache.schema_version, 0);
        assert!(
            !cache.can_skip_startup_fetch(&agent_targets()),
            "升级前的缓存即使时间新鲜也要再查一次，才能在左下角画出 Grok"
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
    fn cache_round_trips_through_the_provider_map() {
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
        let json = serde_json::to_value(&cache).unwrap();
        assert!(json["providers"].get("claude").is_some());
        assert!(json.get("claude").is_none());
        let expected_provider_ids = agent_targets()
            .into_iter()
            .map(QuotaTarget::id)
            .collect::<BTreeSet<_>>();
        let actual_provider_ids = json["provider_ids"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            actual_provider_ids, expected_provider_ids,
            "缓存覆盖范围必须从 provider 注册表生成"
        );

        let cache: QuotaCache = serde_json::from_value(json).unwrap();
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
