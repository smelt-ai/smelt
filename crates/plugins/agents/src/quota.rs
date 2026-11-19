//! 账户额度：定时请本机运行时刷新，结果留在这里。额度那一条（[`crate::quota_view`]）直接读，
//! 别的插件经约定（[`contract::QUOTA`]、[`contract::QUOTA_CHANGED`]）来取。
//!
//! 额度是账户状态，不属于哪段会话。凭据在运行时那台机器上，查询和跨启动缓存都由运行时做
//! （`quota.*`，见 [`api::model::quota_service`]）。这里只管节奏：启动先显示运行时缓存里的
//! 结果，缓存不新鲜才立即查；之后每 [`REFRESH_INTERVAL`] 全量查一次，用户点哪家就单查哪家
//! （只有这时运行时才可能弹钥匙串窗口）。
//!
//! 每家查不查的开关也存在运行时，设置页经 [`QuotaSettings`] 读、经 [`set_enabled`] 改。

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use api::client::protocol::control::EmptyParams;
use api::model::control_api::{QuotaGet, QuotaRefresh, QuotaSetEnabled, QuotaSettingsGet};
use api::model::quota_service::{
    QuotaProviderSetting, QuotaRefreshParams, QuotaSetEnabledParams, QuotaStatus, QuotaTarget,
};
use api::{CommandSpec, RuntimeKey};
use gpui::{App, Global};
use smelt_plugin_api as api;

use crate::contract::{self, Quota, RefreshQuota};

const REFRESH_INTERVAL: Duration = Duration::from_secs(300);

/// 每家一次网络往返，并发查；给足余量。
const REFRESH_TIMEOUT: Duration = Duration::from_secs(120);

/// 本机运行时上开着查询的账户和查到的结果。
#[derive(Default)]
struct QuotaState {
    /// 开着查询的账户，也是展示顺序。顺序一半是动态的（Pi 登录了哪几家），在刷新时由运行时给。
    order: Vec<QuotaTarget>,
    statuses: HashMap<QuotaTarget, QuotaStatus>,
    refreshing: HashSet<QuotaTarget>,
    /// 用户点了才查的那一家，额度那一条据此给即时反馈。后台定时和启动首查不写这里，免得静默刷新
    /// 打断额度行原有的风险颜色。
    manual: HashSet<QuotaTarget>,
}

impl Global for QuotaState {}

/// 用户点了要单查的账户，排给刷新循环。
struct RefreshRequests(smol::channel::Sender<QuotaTarget>);

impl Global for RefreshRequests {}

/// 设置页「账户额度」：每家的查询开关和钥匙串拒绝状态。设置页露出来时和每轮刷新后（设置页
/// 还露着时）读一次，render 只读它。
#[derive(Clone, Default)]
pub(crate) struct QuotaSettings {
    pub(crate) providers: Vec<QuotaProviderSetting>,
    /// 最近一次读取或保存的失败原因。
    pub(crate) error: Option<String>,
}

impl Global for QuotaSettings {}

/// 起刷新循环。插件登记时调一次。
pub(crate) fn install(cx: &mut App) {
    cx.set_global(QuotaState::default());
    cx.set_global(QuotaSettings::default());
    start_refresh_loop(cx);
}

/// 对外的命令，和插件别的命令一起登记（同一来源的命令一次整组登记）。
pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        contract::QUOTA.spec(api::text!("quota.title"), |(), _, cx| Ok(snapshot(cx))),
        contract::REFRESH_QUOTA.spec(api::text!("quota.refresh"), |args: RefreshQuota, _, cx| {
            request_refresh(args.target, cx);
            Ok(())
        }),
    ]
}

/// 整份现状。额度那一条直接读，别的插件经 [`contract::QUOTA`] 拿同一份。
pub(crate) fn snapshot(cx: &App) -> Quota {
    let state = cx.global::<QuotaState>();
    Quota {
        targets: state.order.clone(),
        statuses: state
            .order
            .iter()
            .filter_map(|target| Some((*target, state.statuses.get(target)?.clone())))
            .collect(),
        refreshing: in_order(&state.order, &state.refreshing),
        manual: in_order(&state.order, &state.manual),
    }
}

/// 集合里的账户，按展示顺序。
fn in_order(order: &[QuotaTarget], set: &HashSet<QuotaTarget>) -> Vec<QuotaTarget> {
    order
        .iter()
        .copied()
        .filter(|target| set.contains(target))
        .collect()
}

/// 改状态，再把整份发给订着的插件。
fn update(cx: &mut App, change: impl FnOnce(&mut QuotaState)) {
    change(cx.global_mut::<QuotaState>());
    let quota = snapshot(cx);
    contract::QUOTA_CHANGED.emit(&quota, cx);
}

/// 换一份账户范围，并写入缓存里的结果。不在范围里的账户（用户关掉了、Pi 登出了）连同旧数字
/// 一起去掉。
fn restore(targets: Vec<QuotaTarget>, cached: Vec<(QuotaTarget, QuotaStatus)>, cx: &mut App) {
    update(cx, |state| {
        state.statuses.retain(|target, _| targets.contains(target));
        state.order = targets;
        state.statuses.extend(cached);
    });
}

/// 一轮刷新开始。全量刷新顺带记下范围（`order` 不动，范围就是它）。
fn begin_refresh(manual: Option<QuotaTarget>, cx: &mut App) {
    update(cx, |state| {
        state.refreshing = match manual {
            Some(target) => HashSet::from([target]),
            None => state.order.iter().copied().collect(),
        };
        state.manual = manual.into_iter().collect();
    });
}

/// 写入一轮查询结果。只覆盖这一轮实际查到的账户：单查一家时别家保持原样。
fn apply(results: Vec<(QuotaTarget, QuotaStatus)>, cx: &mut App) {
    update(cx, |state| {
        state.statuses.extend(results);
        state.refreshing.clear();
        state.manual.clear();
    });
}

/// 请刷新循环单查一家。刷新进行中再点不会丢：排进队列，这一轮结束后合并成一轮处理。
pub(crate) fn request_refresh(target: QuotaTarget, cx: &App) {
    if let Some(requests) = cx.try_global::<RefreshRequests>() {
        let _ = requests.0.try_send(target);
    }
}

/// 刷新循环。只依赖本机运行时，和有没有会话、会话是什么类型都无关；跟应用一样长。
fn start_refresh_loop(cx: &mut App) {
    let (requests, received) = smol::channel::unbounded::<QuotaTarget>();
    cx.set_global(RefreshRequests(requests));
    cx.spawn(async move |cx| {
        // 运行时可能还在起来：等它在线再读，读失败就退避后回去等。
        let mut failures = 0u32;
        let cached = loop {
            if !api::wait_online(&RuntimeKey::Local, failures, cx).await {
                return;
            }
            let fetched = cx
                .update(|cx| {
                    api::call::<QuotaGet>(
                        &RuntimeKey::Local,
                        EmptyParams::default(),
                        api::CALL_TIMEOUT,
                        cx,
                    )
                })
                .await;
            match fetched {
                Ok(cached) => break cached,
                Err(error) => eprintln!("[agents] 读额度缓存失败：{error}"),
            }
            failures = failures.saturating_add(1);
        };
        let fresh = cached.fresh;
        cx.update(|cx| restore(cached.targets, cached.statuses, cx));
        // 缓存新鲜：首轮也等定时器或点击；过期或没有缓存才立即查一次。
        let mut first = !fresh;
        loop {
            // 本轮查哪家：`None` 是全部（定时），`Some` 是用户点的那家。
            let mut target = None;
            if !first {
                // 全关了就没有要定时查的，只等用户重新打开时发来的单查。
                let nothing_to_poll = cx.update(|cx| cx.global::<QuotaState>().order.is_empty());
                target = if nothing_to_poll {
                    match received.recv().await {
                        Ok(target) => Some(target),
                        Err(_) => return,
                    }
                } else {
                    // 轮询例外：额度在远端账户上变化，没有事件可等；周期 5 分钟，用户关掉全部
                    // 账户后不再醒。
                    let timer = cx.background_executor().timer(REFRESH_INTERVAL);
                    smol::future::race(async { received.recv().await.ok() }, async {
                        timer.await;
                        None
                    })
                    .await
                };
                // race 只取走了第一个点击，积压的补进来，最后点的优先。
                while let Ok(queued) = received.try_recv() {
                    target = Some(queued);
                }
            }
            first = false;
            cx.update(|cx| begin_refresh(target, cx));
            let result = cx
                .update(|cx| {
                    api::call::<QuotaRefresh>(
                        &RuntimeKey::Local,
                        QuotaRefreshParams { target },
                        REFRESH_TIMEOUT,
                        cx,
                    )
                })
                .await;
            cx.update(|cx| {
                match result {
                    Ok(result) => {
                        // 全量刷新顺带更新范围：用户在 Pi 里新登录一家订阅 provider 后，这是
                        // 它出现的唯一入口。
                        if target.is_none() {
                            restore(result.targets, Vec::new(), cx);
                        }
                        apply(result.statuses, cx);
                    }
                    // 连不上运行时不是额度失效：保留上次的数字，只结束「查询中」。
                    Err(error) => {
                        eprintln!("[agents] 刷新额度失败：{error}");
                        apply(Vec::new(), cx);
                    }
                }
                // 这一轮可能触发或解除钥匙串熔断，设置页露着就跟着重读。
                if api::host(cx).view_shown(contract::SETTINGS_VIEW, cx) {
                    refresh_settings(cx);
                }
            });
        }
    })
    .detach();
}

/// 读一次每家的开关和拒绝状态。
pub(crate) fn refresh_settings(cx: &mut App) {
    let fetched = api::call::<QuotaSettingsGet>(
        &RuntimeKey::Local,
        EmptyParams::default(),
        api::CALL_TIMEOUT,
        cx,
    );
    cx.spawn(async move |cx| {
        let fetched = fetched.await;
        cx.update(|cx| match fetched {
            Ok(settings) => apply_settings(settings.providers, cx),
            Err(error) => settings_failed(error, cx),
        });
    })
    .detach();
}

/// 打开或关掉一家。范围立刻更新；打开时单查一次，可能弹钥匙串窗口，这正是用户刚做的操作。
pub(crate) fn set_enabled(target: QuotaTarget, enabled: bool, cx: &mut App) {
    let saved = api::call::<QuotaSetEnabled>(
        &RuntimeKey::Local,
        QuotaSetEnabledParams { target, enabled },
        api::CALL_TIMEOUT,
        cx,
    );
    cx.spawn(async move |cx| {
        let saved = saved.await;
        cx.update(|cx| match saved {
            Ok(settings) => {
                restore(settings.enabled_targets(), Vec::new(), cx);
                apply_settings(settings.providers, cx);
                if enabled {
                    request_refresh(target, cx);
                }
            }
            Err(error) => settings_failed(error, cx),
        });
    })
    .detach();
}

fn apply_settings(providers: Vec<QuotaProviderSetting>, cx: &mut App) {
    cx.set_global(QuotaSettings {
        providers,
        error: None,
    });
}

/// 失败时保留上次的列表：连不上运行时不等于开关变了。
fn settings_failed(error: String, cx: &mut App) {
    cx.global_mut::<QuotaSettings>().error = Some(error);
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::model::agent_kind::ConversationAgentKind;

    fn status(checked_at_ms: u64) -> QuotaStatus {
        QuotaStatus {
            quota: None,
            checked_at_ms,
            error: None,
        }
    }

    /// 换范围时丢掉不在范围里的旧数字；单查一家的结果只覆盖那一家。
    #[gpui::test]
    fn restore_drops_targets_out_of_scope_and_apply_keeps_the_rest(cx: &mut gpui::TestAppContext) {
        let claude = QuotaTarget::agent(ConversationAgentKind::Claude);
        let codex = QuotaTarget::agent(ConversationAgentKind::Codex);
        cx.update(|cx| {
            cx.set_global(QuotaState::default());
            restore(
                vec![claude, codex],
                vec![(claude, status(1)), (codex, status(1))],
                cx,
            );
            begin_refresh(Some(codex), cx);
            let quota = snapshot(cx);
            assert_eq!(quota.refreshing, vec![codex]);
            assert_eq!(quota.manual, vec![codex]);
            apply(vec![(codex, status(2))], cx);
            restore(vec![codex], Vec::new(), cx);
            let quota = snapshot(cx);
            assert_eq!(quota.targets, vec![codex]);
            assert_eq!(quota.statuses, vec![(codex, status(2))]);
            assert!(quota.refreshing.is_empty() && quota.manual.is_empty());
        });
    }

    /// 后台全量刷新查的是整个范围，但不算用户点的。
    #[gpui::test]
    fn a_background_round_is_not_manual(cx: &mut gpui::TestAppContext) {
        let claude = QuotaTarget::agent(ConversationAgentKind::Claude);
        cx.update(|cx| {
            cx.set_global(QuotaState::default());
            restore(vec![claude], Vec::new(), cx);
            begin_refresh(None, cx);
            let quota = snapshot(cx);
            assert_eq!(quota.refreshing, vec![claude]);
            assert!(quota.manual.is_empty());
        });
    }
}
