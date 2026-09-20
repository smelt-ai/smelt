//! 应用级 provider 额度服务。
//!
//! 额度是账户状态，不属于 ACP 会话。这里单独维护刷新循环和展示状态，任何终端、
//! ACP 或插件会话都只是读取它，不会因为额度查询收到 prompt 或被改变状态。
//!
//! 查询结果缓存到主库 `quota_cache` 作用域：启动时先恢复缓存直接显示，
//! 缓存新鲜（`CACHE_STALE_AFTER` 内）就跳过启动首查，等定时器或用户手动点击。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::future::Future;

use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::future::join_all;
use serde::{Deserialize, Serialize};

use smelt_core::agent_kind::ConversationAgentKind;
use smelt_core::copilot_quota::{
    CopilotQuota, CopilotQuotaStatus as CachedCopilotQuotaStatus, fetch_copilot_quota,
};
use smelt_core::provider_quota::{
    ProviderQuota, ProviderQuotaStatus as CachedWindowQuotaStatus, fetch_claude_quota,
    fetch_codex_quota, fetch_cursor_quota, fetch_grok_quota,
};

use crate::Workspace;

pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_secs(300);

/// 缓存比这个新就跳过启动首查。额度是慢变化的账户状态，1 小时内直接展示
/// 上次结果不会让用户看到明显过期的数据。
pub(crate) const CACHE_STALE_AFTER: Duration = Duration::from_secs(3600);

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ProviderQuotaData {
    Windows(ProviderQuota),
    Copilot(CopilotQuota),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QuotaStatus {
    pub(crate) quota: Option<ProviderQuotaData>,
    pub(crate) checked_at_ms: u64,
    pub(crate) error: Option<String>,
}

impl QuotaStatus {
    fn from_window_cache(status: CachedWindowQuotaStatus) -> Self {
        Self {
            quota: status.quota.map(ProviderQuotaData::Windows),
            checked_at_ms: status.checked_at_ms,
            error: status.error,
        }
    }

    fn from_copilot_cache(status: CachedCopilotQuotaStatus) -> Self {
        Self {
            quota: status.quota.map(ProviderQuotaData::Copilot),
            checked_at_ms: status.checked_at_ms,
            error: status.error,
        }
    }

    fn to_window_cache(&self) -> Option<CachedWindowQuotaStatus> {
        let ProviderQuotaData::Windows(quota) = self.quota.as_ref()? else {
            return None;
        };
        Some(CachedWindowQuotaStatus {
            quota: Some(quota.clone()),
            checked_at_ms: self.checked_at_ms,
            error: self.error.clone(),
        })
    }

    fn to_copilot_cache(&self) -> Option<CachedCopilotQuotaStatus> {
        let ProviderQuotaData::Copilot(quota) = self.quota.as_ref()? else {
            return None;
        };
        Some(CachedCopilotQuotaStatus {
            quota: Some(quota.clone()),
            checked_at_ms: self.checked_at_ms,
            error: self.error.clone(),
        })
    }

    fn from_cache(status: CachedQuotaStatus) -> Self {
        match status {
            CachedQuotaStatus::Windows(status) => Self::from_window_cache(status),
            CachedQuotaStatus::Copilot(status) => Self::from_copilot_cache(status),
        }
    }

    fn to_cache(&self) -> Option<CachedQuotaStatus> {
        match self.quota.as_ref()? {
            ProviderQuotaData::Windows(_) => self.to_window_cache().map(CachedQuotaStatus::Windows),
            ProviderQuotaData::Copilot(_) => {
                self.to_copilot_cache().map(CachedQuotaStatus::Copilot)
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct ProviderQuotaState {
    statuses: HashMap<ConversationAgentKind, QuotaStatus>,
    /// 正在查询的 provider 集合（空 = 没在查询）。它用于首次没有缓存时的
    /// 「额度查询中…」占位，不承担视觉反馈语义。
    pub refreshing: HashSet<ConversationAgentKind>,
    /// 由用户点击触发、应在侧栏中给出绿色反馈的 provider。后台定时和启动首查
    /// 不写入这里，避免静默刷新打断额度行原有的风险颜色。
    pub manual_refreshing: HashSet<ConversationAgentKind>,
}

type QuotaFetchFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<Option<ProviderQuotaData>>> + Send + 'static>>;
type QuotaFetcher = fn() -> QuotaFetchFuture;

struct QuotaProvider {
    agent: ConversationAgentKind,
    fetch: QuotaFetcher,
}

/// 账户额度 Provider 注册表，同时决定侧栏展示顺序。没有已知只读额度接口的
/// Agent 不注册；以后接入时只需增加 fetcher 和这一行，不再修改刷新/状态/UI。
static QUOTA_PROVIDERS: &[QuotaProvider] = &[
    QuotaProvider {
        agent: ConversationAgentKind::Codex,
        fetch: fetch_codex_quota_data,
    },
    QuotaProvider {
        agent: ConversationAgentKind::Claude,
        fetch: fetch_claude_quota_data,
    },
    QuotaProvider {
        agent: ConversationAgentKind::Copilot,
        fetch: fetch_copilot_quota_data,
    },
    QuotaProvider {
        agent: ConversationAgentKind::Grok,
        fetch: fetch_grok_quota_data,
    },
    QuotaProvider {
        agent: ConversationAgentKind::Cursor,
        fetch: fetch_cursor_quota_data,
    },
];

pub(crate) fn quota_provider_kinds() -> impl Iterator<Item = ConversationAgentKind> {
    QUOTA_PROVIDERS.iter().map(|provider| provider.agent)
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

/// 跨启动的额度缓存（序列化到本地 JSON 缓存文件）。
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CachedQuotaStatus {
    Windows(CachedWindowQuotaStatus),
    Copilot(CachedCopilotQuotaStatus),
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct QuotaCache {
    /// 上次写入时间（Unix 毫秒），用于判断缓存是否还新鲜。
    pub saved_at_ms: u64,
    /// 格式版本仅用于迁移旧命名字段；provider 覆盖范围由 `provider_ids` 决定，
    /// 新增 provider 不再需要手工递增版本号。
    #[serde(default)]
    pub schema_version: u32,
    /// 本次缓存生成时注册过的全部 provider，包括没有凭据、未拿到额度的项。
    /// 用 BTreeSet 保证持久化顺序稳定。
    #[serde(default)]
    provider_ids: BTreeSet<String>,
    /// 真正拿到额度的项。键使用 agent 的稳定存档 id。
    #[serde(default)]
    providers: BTreeMap<String, CachedQuotaStatus>,
    /// 以下字段只读旧格式，新缓存不再写出。
    #[serde(default, skip_serializing)]
    pub copilot: Option<CachedCopilotQuotaStatus>,
    #[serde(default, skip_serializing)]
    pub codex: Option<CachedWindowQuotaStatus>,
    #[serde(default, skip_serializing)]
    pub claude: Option<CachedWindowQuotaStatus>,
    /// 旧缓存文件没有这个字段；缺省时按没查过 Grok 处理，不能让整份缓存作废。
    #[serde(default, skip_serializing)]
    pub grok: Option<CachedWindowQuotaStatus>,
    /// 旧缓存没有这个字段；缺省按没查过 Cursor 处理。
    #[serde(default, skip_serializing)]
    pub cursor: Option<CachedWindowQuotaStatus>,
}

/// 当前格式开始使用动态 provider map。以后新增 provider 不需要改这个版本。
const QUOTA_CACHE_VERSION: u32 = 3;

/// 旧 schema 的覆盖范围是历史契约，不能用当前注册表倒推，否则新增 provider 后
/// 会误把旧缓存当成已覆盖。
const LEGACY_QUOTA_PROVIDER_IDS_V0: &[&str] = &["codex", "claude", "copilot"];
const LEGACY_QUOTA_PROVIDER_IDS_V1: &[&str] = &["codex", "claude", "copilot", "grok"];
const LEGACY_QUOTA_PROVIDER_IDS_V2: &[&str] = &["codex", "claude", "copilot", "grok", "cursor"];

impl QuotaCache {
    fn is_fresh(&self) -> bool {
        let now = unix_time_ms();
        self.saved_at_ms
            .saturating_add(CACHE_STALE_AFTER.as_millis() as u64)
            >= now
    }

    /// 时间新鲜且已经覆盖当前所有 provider 时，才跳过启动首查。
    fn can_skip_startup_fetch(&self) -> bool {
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
        quota_provider_kinds().all(|provider| covered.contains(provider.id()))
    }

    fn statuses(&self) -> Vec<(ConversationAgentKind, QuotaStatus)> {
        let mut statuses = [
            self.codex
                .clone()
                .map(QuotaStatus::from_window_cache)
                .map(|status| (ConversationAgentKind::Codex, status)),
            self.claude
                .clone()
                .map(QuotaStatus::from_window_cache)
                .map(|status| (ConversationAgentKind::Claude, status)),
            self.copilot
                .clone()
                .map(QuotaStatus::from_copilot_cache)
                .map(|status| (ConversationAgentKind::Copilot, status)),
            self.grok
                .clone()
                .map(QuotaStatus::from_window_cache)
                .map(|status| (ConversationAgentKind::Grok, status)),
            self.cursor
                .clone()
                .map(QuotaStatus::from_window_cache)
                .map(|status| (ConversationAgentKind::Cursor, status)),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        statuses.extend(self.providers.iter().filter_map(|(id, status)| {
            ConversationAgentKind::from_id(id)
                .map(|provider| (provider, QuotaStatus::from_cache(status.clone())))
        }));
        statuses
    }
}

struct ProviderQuotaFetchResult(
    Vec<(
        ConversationAgentKind,
        anyhow::Result<Option<ProviderQuotaData>>,
    )>,
);

impl ProviderQuotaFetchResult {
    #[cfg(test)]
    fn one(
        provider: ConversationAgentKind,
        result: anyhow::Result<Option<ProviderQuotaData>>,
    ) -> Self {
        Self(vec![(provider, result)])
    }

    fn failed(target: Option<ConversationAgentKind>, error: String) -> Self {
        Self(
            QUOTA_PROVIDERS
                .iter()
                .filter(|provider| target.is_none() || target == Some(provider.agent))
                .map(|provider| (provider.agent, Err(anyhow::anyhow!(error.clone()))))
                .collect(),
        )
    }
}

async fn fetch_provider_quotas(target: Option<ConversationAgentKind>) -> ProviderQuotaFetchResult {
    let results = join_all(
        QUOTA_PROVIDERS
            .iter()
            .filter(|provider| target.is_none() || target == Some(provider.agent))
            .map(|provider| async move { (provider.agent, (provider.fetch)().await) }),
    )
    .await;
    ProviderQuotaFetchResult(results)
}

impl ProviderQuotaState {
    pub(crate) fn status(&self, provider: ConversationAgentKind) -> Option<&QuotaStatus> {
        self.statuses.get(&provider)
    }

    /// 记录一轮查询的实际范围和是否需要用户可见反馈。`Some` 只能来自额度行的
    /// 手动点击；`None` 仅代表启动首查或定时静默刷新。
    fn begin_refresh(&mut self, target: Option<ConversationAgentKind>) {
        self.refreshing = match target {
            Some(kind) => [kind].into_iter().collect(),
            None => quota_provider_kinds().collect(),
        };
        self.manual_refreshing = target.into_iter().collect();
    }

    /// 结果项自带 provider 身份，只覆盖这一轮实际查询的状态；手动点谁查谁时，
    /// 其他 provider 不会出现在结果中，自然保持原样。
    fn apply(&mut self, result: ProviderQuotaFetchResult) {
        let checked_at_ms = unix_time_ms();
        for (provider, outcome) in result.0 {
            let previous = self.statuses.remove(&provider);
            self.statuses.insert(
                provider,
                apply_quota_result(previous, outcome, checked_at_ms),
            );
        }
        self.refreshing.clear();
        self.manual_refreshing.clear();
    }

    /// 启动时用缓存恢复各 provider 的上次结果（只覆盖有数据的项，失败态
    /// 不缓存——上次查询失败不该让下次启动也显示失败）。
    fn restore(&mut self, cache: &QuotaCache) {
        for (provider, status) in cache.statuses() {
            self.statuses.insert(provider, status);
        }
    }

    fn to_cache(&self) -> QuotaCache {
        let provider_ids = quota_provider_kinds()
            .map(|provider| provider.id().to_string())
            .collect();
        let providers = quota_provider_kinds()
            .filter_map(|provider| {
                self.status(provider)
                    .and_then(QuotaStatus::to_cache)
                    .map(|status| (provider.id().to_string(), status))
            })
            .collect();
        QuotaCache {
            saved_at_ms: unix_time_ms(),
            schema_version: QUOTA_CACHE_VERSION,
            provider_ids,
            // 只缓存真正拿到 quota 的项；失败态（quota 为 None）不落盘。
            providers,
            copilot: None,
            codex: None,
            claude: None,
            grok: None,
            cursor: None,
        }
    }
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
        copilot: None,
        codex: None,
        claude: None,
        grok: None,
        cursor: None,
    })
}

fn apply_quota_result(
    previous: Option<QuotaStatus>,
    result: anyhow::Result<Option<ProviderQuotaData>>,
    checked_at_ms: u64,
) -> QuotaStatus {
    match result {
        Ok(quota) => QuotaStatus {
            quota,
            checked_at_ms,
            error: None,
        },
        Err(error) => QuotaStatus {
            quota: previous.and_then(|status| status.quota),
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
        let cache = load_quota_cache();
        let cache_fresh = cache
            .as_ref()
            .is_some_and(QuotaCache::can_skip_startup_fetch);
        if let Some(cache) = &cache {
            self.provider_quota_state.restore(cache);
        }
        let (refresh_tx, refresh_rx) = smol::channel::unbounded::<ConversationAgentKind>();
        self.provider_quota_refresh_tx = Some(refresh_tx);
        cx.spawn(async move |this, cx| {
            // 缓存新鲜 → 首轮也等定时器/信号；过期或没有缓存才立即查一次。
            let mut first = !cache_fresh;
            loop {
                // 本轮要刷新的 provider：None = 全部（定时刷新）；Some = 只刷那家。
                let mut target: Option<ConversationAgentKind> = None;
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
                    .update(cx, |workspace, cx| {
                        workspace.provider_quota_state.begin_refresh(target);
                        cx.notify();
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

                if this
                    .update(cx, |workspace, cx| {
                        workspace.provider_quota_state.apply(result);
                        save_quota_cache(&workspace.provider_quota_state.to_cache());
                        cx.notify();
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
    pub(crate) fn request_provider_quota_refresh(&mut self, provider: ConversationAgentKind) {
        if let Some(tx) = &self.provider_quota_refresh_tx {
            let _ = tx.try_send(provider);
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

    #[test]
    fn quota_registry_preserves_the_existing_display_order() {
        assert_eq!(
            quota_provider_kinds().collect::<Vec<_>>(),
            vec![
                ConversationAgentKind::Codex,
                ConversationAgentKind::Claude,
                ConversationAgentKind::Copilot,
                ConversationAgentKind::Grok,
                ConversationAgentKind::Cursor,
            ]
        );
    }

    #[test]
    fn legacy_named_cache_restores_into_the_provider_index() {
        let cache: QuotaCache = serde_json::from_str(
            r#"{
              "saved_at_ms": 1,
              "schema_version": 2,
              "copilot": null,
              "codex": null,
              "claude": {
                "quota": {"windows": [{"label":"5h","used_percent":9.0,"resets_at_ms":null,"reset_after_seconds":null}]},
                "checked_at_ms": 7
              },
              "grok": null,
              "cursor": null
            }"#,
        )
        .unwrap();
        let mut state = ProviderQuotaState::default();

        state.restore(&cache);

        let status = state.status(ConversationAgentKind::Claude).unwrap();
        assert_eq!(status.checked_at_ms, 7);
        assert!(matches!(status.quota, Some(ProviderQuotaData::Windows(_))));
    }

    #[test]
    fn silent_refresh_keeps_manual_feedback_empty() {
        let mut state = ProviderQuotaState::default();

        state.begin_refresh(None);

        assert_eq!(state.refreshing.len(), quota_provider_kinds().count());
        assert!(state.manual_refreshing.is_empty());
    }

    #[test]
    fn manual_refresh_marks_only_clicked_provider() {
        let mut state = ProviderQuotaState::default();

        state.begin_refresh(Some(ConversationAgentKind::Claude));

        assert_eq!(state.refreshing.len(), 1);
        assert!(state.refreshing.contains(&ConversationAgentKind::Claude));
        assert_eq!(state.manual_refreshing.len(), 1);
        assert!(
            state
                .manual_refreshing
                .contains(&ConversationAgentKind::Claude)
        );
    }

    #[test]
    fn apply_grok_only_does_not_touch_other_providers() {
        let mut state = ProviderQuotaState::default();
        state.statuses.insert(
            ConversationAgentKind::Claude,
            QuotaStatus {
                quota: None,
                checked_at_ms: 1,
                error: None,
            },
        );

        state.apply(ProviderQuotaFetchResult::one(
            ConversationAgentKind::Grok,
            Ok(Some(ProviderQuotaData::Windows(ProviderQuota {
                windows: vec![smelt_core::provider_quota::ProviderQuotaWindow {
                    label: "7d".into(),
                    used_percent: 2.0,
                    resets_at_ms: Some(1_787_491_666_482),
                    reset_after_seconds: None,
                }],
            }))),
        ));

        assert_eq!(
            state
                .status(ConversationAgentKind::Claude)
                .unwrap()
                .checked_at_ms,
            1
        );
        let grok = state
            .status(ConversationAgentKind::Grok)
            .and_then(|status| status.quota.as_ref());
        assert_eq!(
            grok.and_then(|quota| match quota {
                ProviderQuotaData::Windows(quota) => Some(quota.windows[0].used_percent),
                ProviderQuotaData::Copilot(_) => None,
            }),
            Some(2.0)
        );
        assert!(state.refreshing.is_empty());
        assert!(state.manual_refreshing.is_empty());
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
        .expect("missing grok field must not reject old cache");
        assert!(cache.grok.is_none());
        assert!(
            cache
                .claude
                .as_ref()
                .and_then(|status| status.quota.as_ref())
                .is_some()
        );
        assert_eq!(cache.schema_version, 0);
        assert!(
            !cache.can_skip_startup_fetch(),
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
        assert!(!cache.can_skip_startup_fetch());
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
            !cache.can_skip_startup_fetch(),
            "升级到 Cursor 额度后，schema 1 的新鲜缓存也要再查一次"
        );
    }

    #[test]
    fn apply_cursor_only_does_not_touch_other_providers() {
        let mut state = ProviderQuotaState::default();
        state.statuses.insert(
            ConversationAgentKind::Grok,
            QuotaStatus {
                quota: None,
                checked_at_ms: 1,
                error: None,
            },
        );

        state.apply(ProviderQuotaFetchResult::one(
            ConversationAgentKind::Cursor,
            Ok(Some(ProviderQuotaData::Windows(ProviderQuota {
                windows: vec![smelt_core::provider_quota::ProviderQuotaWindow {
                    label: "31d".into(),
                    used_percent: 3.075,
                    resets_at_ms: Some(1_789_566_331_000),
                    reset_after_seconds: None,
                }],
            }))),
        ));

        assert_eq!(
            state
                .status(ConversationAgentKind::Grok)
                .unwrap()
                .checked_at_ms,
            1
        );
        let cursor = state
            .status(ConversationAgentKind::Cursor)
            .and_then(|status| status.quota.as_ref());
        assert_eq!(
            cursor.and_then(|quota| match quota {
                ProviderQuotaData::Windows(quota) => Some(quota.windows[0].used_percent),
                ProviderQuotaData::Copilot(_) => None,
            }),
            Some(3.075)
        );
    }

    #[test]
    fn failed_refresh_keeps_the_previous_successful_quota() {
        let mut state = ProviderQuotaState::default();
        let quota = ProviderQuotaData::Windows(ProviderQuota {
            windows: vec![smelt_core::provider_quota::ProviderQuotaWindow {
                label: "5h".into(),
                used_percent: 40.0,
                resets_at_ms: None,
                reset_after_seconds: None,
            }],
        });
        state.apply(ProviderQuotaFetchResult::one(
            ConversationAgentKind::Codex,
            Ok(Some(quota.clone())),
        ));

        state.apply(ProviderQuotaFetchResult::one(
            ConversationAgentKind::Codex,
            Err(anyhow::anyhow!("offline")),
        ));

        let status = state.status(ConversationAgentKind::Codex).unwrap();
        assert_eq!(status.quota, Some(quota));
        assert_eq!(status.error.as_deref(), Some("offline"));
    }

    #[test]
    fn unified_state_round_trips_through_the_provider_map() {
        let mut state = ProviderQuotaState::default();
        state.statuses.insert(
            ConversationAgentKind::Claude,
            QuotaStatus {
                quota: Some(ProviderQuotaData::Windows(ProviderQuota {
                    windows: vec![smelt_core::provider_quota::ProviderQuotaWindow {
                        label: "7d".into(),
                        used_percent: 12.0,
                        resets_at_ms: None,
                        reset_after_seconds: Some(60),
                    }],
                })),
                checked_at_ms: 9,
                error: Some("stale".into()),
            },
        );

        let cache = state.to_cache();
        let json = serde_json::to_value(&cache).unwrap();
        assert!(json["providers"].get("claude").is_some());
        assert!(json.get("claude").is_none());
        let expected_provider_ids = quota_provider_kinds()
            .map(|provider| provider.id())
            .collect::<BTreeSet<_>>();
        let actual_provider_ids = json["provider_ids"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            actual_provider_ids, expected_provider_ids,
            "缓存覆盖范围必须从 provider 注册表生成"
        );

        let cache: QuotaCache = serde_json::from_value(json).unwrap();
        let mut restored = ProviderQuotaState::default();
        restored.restore(&cache);
        assert_eq!(
            restored.status(ConversationAgentKind::Claude),
            state.status(ConversationAgentKind::Claude)
        );
    }

    #[test]
    fn startup_skip_requires_every_registered_provider_id() {
        let state = ProviderQuotaState::default();
        let mut cache = state.to_cache();
        cache.saved_at_ms = unix_time_ms();
        assert!(cache.can_skip_startup_fetch());

        let missing = quota_provider_kinds().next().unwrap();
        cache.provider_ids.remove(missing.id());
        assert!(
            !cache.can_skip_startup_fetch(),
            "注册表新增 provider 后，旧缓存必须自动触发首查"
        );
    }
}
