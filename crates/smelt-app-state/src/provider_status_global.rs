//! provider 账户额度的共享单例。
//!
//! 额度是**进程级账户状态**，不属于任何会话，但要被多处读：侧栏额度行、设置里的额度页，
//! 以及对话的模型菜单（宿主经 `smelt_acp_view::env` 转给对话组件，组件不直接读这里）。
//! 走 GPUI 的 Global + `revision` 唤醒 `observe_global`——和
//! [`crate::daemon_states_global`] 里那两个全局一个形状，不另起一套机制。
//!
//! 写入只走本文件的方法：它们在改完数据后统一 bump `revision`，观察者才会醒。
//! 直接改内层 `Mutex` 不会触发任何刷新。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use gpui::{App, BorrowAppContext, Global};

#[cfg(test)]
use smelt_model::agent_kind::ConversationAgentKind;
use smelt_model::pi_quota::pi_quota_provider;
use smelt_model::provider_quota::ProviderQuotaData;
pub use smelt_model::quota_service::{QuotaStatus, QuotaTarget};

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

    /// 换一份账户范围，并写入缓存里的结果。失败态不进缓存，所以这里只会写入有数据的项。
    /// 不在范围里的账户（用户关掉了、Pi 登出了）连同旧数字一起去掉。
    pub fn restore_quota(
        targets: &[QuotaTarget],
        cached: Vec<(QuotaTarget, QuotaStatus)>,
        cx: &mut App,
    ) {
        let targets = targets.to_vec();
        Self::update_data(cx, |data| {
            data.quota.retain(|target, _| targets.contains(target));
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

    fn quota(used_percent: f64) -> ProviderQuotaData {
        ProviderQuotaData::Windows(smelt_model::provider_quota::ProviderQuota {
            windows: vec![smelt_model::provider_quota::ProviderQuotaWindow {
                label: "5h".to_string(),
                used_percent,
                resets_at_ms: None,
                reset_after_seconds: None,
            }],
        })
    }

    /// 模型菜单里的额度括号：查到的 provider 给短文本；API key 型的 provider（modelsight
    /// 这类）没有额度查询，永远是 `None`，菜单上不画空括号。
    #[gpui::test]
    fn pi_quota_badge_is_the_short_text_of_a_known_provider(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert_eq!(ProviderStatus::pi_quota_badge("anthropic", cx), None);
            cx.set_global(ProviderStatus::default());
            ProviderStatus::apply_quota(
                vec![(
                    QuotaTarget::pi("anthropic"),
                    QuotaStatus {
                        quota: Some(quota(25.0)),
                        checked_at_ms: 1,
                        error: None,
                    },
                )],
                cx,
            );
            assert_eq!(
                ProviderStatus::pi_quota_badge("anthropic", cx).as_deref(),
                Some("25%")
            );
            assert_eq!(ProviderStatus::pi_quota_badge("modelsight", cx), None);
        });
    }
}
