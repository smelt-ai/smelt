//! provider 账户状态的共享单例：额度快照 + Pi 模型目录版本。
//!
//! 额度和「Pi 现在有哪些 provider」都是**进程级账户状态**，不属于任何会话，但
//! 需要被多处读：侧栏额度行（主 GUI）、对话的模型菜单（`smelt-acp-view`）。两
//! 个 crate 不能互相依赖，所以状态放在共享层，走 GPUI 的 Global + `revision`
//! 唤醒 `observe_global`——和 [`crate::daemon_states_global`] 里那两个全局一个
//! 形状，不另起一套机制。
//!
//! 写入只走本文件的方法：它们在改完数据后统一 bump `revision`，观察者才会醒。
//! 直接改内层 `Mutex` 不会触发任何刷新。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use gpui::{App, BorrowAppContext, Global};

use smelt_core::agent_kind::ConversationAgentKind;
use smelt_core::pi_quota::pi_quota_provider;
use smelt_core::provider_quota::ProviderQuotaData;

/// 一份额度对应的账户。
///
/// 多数 agent 一个 CLI 对一个账户，`provider` 为空即可。Pi 不是：它底下挂着若干
/// provider，各自是独立账户，所以键必须带上 provider 身份，否则几家会互相覆盖。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QuotaTarget {
    pub agent: ConversationAgentKind,
    /// 该 agent 下的 provider id（目前只有 Pi 用到）。`None` = agent 自己的账户。
    pub provider: Option<&'static str>,
}

impl QuotaTarget {
    pub fn agent(agent: ConversationAgentKind) -> Self {
        Self {
            agent,
            provider: None,
        }
    }

    pub fn pi(provider: &'static str) -> Self {
        Self {
            agent: ConversationAgentKind::Pi,
            provider: Some(provider),
        }
    }

    /// 缓存键与 UI 元素 id。单账户的形式保持和过去一致（`codex`），所以升级后
    /// 旧缓存照样认得出来。
    pub fn id(self) -> String {
        match self.provider {
            Some(provider) => format!("{}:{provider}", self.agent.id()),
            None => self.agent.id().to_string(),
        }
    }

    /// 缓存里的键反解。provider 段必须落在当前白名单里，否则这条缓存已经没有
    /// 对应的查询入口，直接丢弃。
    pub fn from_id(id: &str) -> Option<Self> {
        let Some((agent, provider)) = id.split_once(':') else {
            return ConversationAgentKind::from_id(id).map(Self::agent);
        };
        let agent = ConversationAgentKind::from_id(agent)?;
        if agent != ConversationAgentKind::Pi {
            return None;
        }
        pi_quota_provider(provider).map(|provider| Self::pi(provider.id))
    }

    /// 侧栏文案里的名字。
    pub fn label(self) -> &'static str {
        self.agent.short_label()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct QuotaStatus {
    pub quota: Option<ProviderQuotaData>,
    pub checked_at_ms: u64,
    pub error: Option<String>,
}

#[derive(Default)]
struct ProviderStatusData {
    quota: HashMap<QuotaTarget, QuotaStatus>,
    /// 侧栏展示顺序。注册表一半是动态的（Pi 登录了哪几家要读 `auth.json`），
    /// 而渲染每帧都会走一遍，所以顺序在刷新时算好存下来，不在渲染里碰磁盘。
    order: Vec<QuotaTarget>,
    refreshing: HashSet<QuotaTarget>,
    /// 由用户点击触发、应在侧栏给出绿色反馈的那一家。后台定时和启动首查不写
    /// 这里，避免静默刷新打断额度行原有的风险颜色。
    manual_refreshing: HashSet<QuotaTarget>,
    /// Pi 的凭据/模型配置版本号：登录、登出、改 `models.json` 各 +1。
    ///
    /// Pi 的模型目录只能问 Pi 进程自己（`get_available_models`），而那是会话级
    /// 的、启动时拉一次。会话订阅这个版本号，变了就重拉自己的目录——这样新登录
    /// 的 provider 不必等用户重开会话才出现。
    pi_catalog_revision: u64,
}

/// provider 账户状态全局单例。`revision` 只为让 GPUI 觉得 Global 变了。
#[derive(Clone, Default)]
pub struct ProviderStatus {
    data: Arc<Mutex<ProviderStatusData>>,
    revision: u64,
}

impl Global for ProviderStatus {}

impl ProviderStatus {
    /// 改完数据后唤醒观察者。所有写入方法末尾都要走它。
    fn signal(cx: &mut App) {
        cx.update_global::<Self, _>(|global, _| {
            global.revision = global.revision.wrapping_add(1);
        });
    }

    fn with_data<R>(cx: &App, read: impl FnOnce(&ProviderStatusData) -> R) -> Option<R> {
        let global = cx.try_global::<Self>()?;
        let data = global.data.lock().ok()?;
        Some(read(&data))
    }

    fn update_data(cx: &mut App, write: impl FnOnce(&mut ProviderStatusData)) {
        let Some(global) = cx.try_global::<Self>() else {
            return;
        };
        let data = global.data.clone();
        {
            let Ok(mut data) = data.lock() else {
                return;
            };
            write(&mut data);
        }
        Self::signal(cx);
    }

    // ---- 额度 ----

    /// 一轮刷新开始：记录范围和是否需要用户可见反馈。
    ///
    /// 全量刷新（`targets` 来自注册表）顺带重算展示顺序：用户在 Pi 里新登录一家
    /// 订阅 provider 后，这是它出现在侧栏的唯一入口。
    pub fn begin_quota_refresh(targets: &[QuotaTarget], manual: Option<QuotaTarget>, cx: &mut App) {
        let targets = targets.to_vec();
        Self::update_data(cx, |data| {
            if manual.is_none() {
                data.order = targets.clone();
            }
            data.refreshing = targets.into_iter().collect();
            data.manual_refreshing = manual.into_iter().collect();
        });
    }

    /// 写入一轮查询结果。只覆盖这一轮实际查到的账户：手动点谁查谁时，其他账户
    /// 不在结果里，自然保持原样。
    pub fn apply_quota(results: Vec<(QuotaTarget, QuotaStatus)>, cx: &mut App) {
        Self::update_data(cx, |data| {
            for (target, status) in results {
                data.quota.insert(target, status);
            }
            data.refreshing.clear();
            data.manual_refreshing.clear();
        });
    }

    /// 启动时用缓存恢复上次结果。失败态不进缓存，所以这里只会写入有数据的项。
    pub fn restore_quota(
        targets: &[QuotaTarget],
        cached: Vec<(QuotaTarget, QuotaStatus)>,
        cx: &mut App,
    ) {
        let targets = targets.to_vec();
        Self::update_data(cx, |data| {
            data.order = targets;
            for (target, status) in cached {
                data.quota.insert(target, status);
            }
        });
    }

    pub fn quota_status(target: QuotaTarget, cx: &App) -> Option<QuotaStatus> {
        Self::with_data(cx, |data| data.quota.get(&target).cloned())?
    }

    /// 刷新范围内的全部账户，含 Pi；缓存按它落盘。
    pub fn quota_targets(cx: &App) -> Vec<QuotaTarget> {
        Self::with_data(cx, |data| data.order.clone()).unwrap_or_default()
    }

    /// 侧栏额度行只收单账户的 agent。Pi 底下的 provider 额度跟在对话模型菜单的
    /// provider 名字后面：那里才说得清这个数字属于哪个 provider，堆在侧栏只是
    /// 一排分不出谁是谁的 Pi 图标。
    pub fn sidebar_targets(cx: &App) -> Vec<QuotaTarget> {
        Self::with_data(cx, |data| {
            data.order
                .iter()
                .copied()
                .filter(|target| target.provider.is_none())
                .collect()
        })
        .unwrap_or_default()
    }

    pub fn is_manually_refreshing(target: QuotaTarget, cx: &App) -> bool {
        Self::with_data(cx, |data| data.manual_refreshing.contains(&target)).unwrap_or(false)
    }

    pub fn is_refreshing(cx: &App) -> bool {
        Self::with_data(cx, |data| !data.refreshing.is_empty()).unwrap_or(false)
    }

    /// 模型菜单里跟在 provider 名字后面的括号文本（如 `25%`）。查不到就是
    /// `None`，调用方不画括号。
    pub fn pi_quota_badge(provider_id: &str, cx: &App) -> Option<String> {
        let provider = pi_quota_provider(provider_id)?;
        Self::with_data(cx, |data| {
            data.quota
                .get(&QuotaTarget::pi(provider.id))
                .and_then(|status| status.quota.as_ref())
                .map(ProviderQuotaData::short_text)
        })?
    }

    // ---- Pi 模型目录 ----

    /// Pi 的凭据或模型配置变了（登录、登出、改 `models.json`）。活着的 Pi 会话
    /// 会据此重拉自己的模型目录。
    pub fn bump_pi_catalog(cx: &mut App) {
        Self::update_data(cx, |data| {
            data.pi_catalog_revision = data.pi_catalog_revision.wrapping_add(1);
        });
    }

    pub fn pi_catalog_revision(cx: &App) -> u64 {
        Self::with_data(cx, |data| data.pi_catalog_revision).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_target_id_round_trips_in_the_legacy_form() {
        let target = QuotaTarget::agent(ConversationAgentKind::Codex);
        assert_eq!(target.id(), "codex");
        assert_eq!(QuotaTarget::from_id("codex"), Some(target));
    }

    #[test]
    fn pi_provider_target_id_round_trips() {
        let target = QuotaTarget::pi("anthropic");
        assert_eq!(target.id(), "pi:anthropic");
        assert_eq!(QuotaTarget::from_id("pi:anthropic"), Some(target));
    }

    /// 缓存里残留的 provider（白名单删了、或 Pi 改了 id）没有查询入口，恢复时
    /// 必须丢掉，不能变成一行永远不刷新的僵尸额度。
    #[test]
    fn unknown_pi_provider_ids_are_dropped() {
        assert_eq!(QuotaTarget::from_id("pi:modelsight"), None);
        assert_eq!(QuotaTarget::from_id("claude:anthropic"), None);
    }
}
