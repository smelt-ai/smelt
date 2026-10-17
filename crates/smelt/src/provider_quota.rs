//! 应用级 provider 额度：定时请运行时刷新，结果写进 [`ProviderStatus`]。
//!
//! 额度是账户状态，不属于 ACP 会话。凭据在运行时那台机器上，查询和跨启动缓存都由
//! 运行时做（`quota.*`，见 [`smelt_core::quota_service`]）。这里只管节奏：启动先显示
//! 运行时缓存里的结果，缓存不新鲜才立即查；之后每 [`REFRESH_INTERVAL`] 全量查一次，
//! 用户点哪家就单查哪家。侧栏和对话模型菜单都从 `ProviderStatus` 读，并靠
//! `observe_global` 收到更新。

use std::time::Duration;

use smelt_core::control_api::{QuotaGet, QuotaRefresh};
use smelt_core::quota_service::QuotaRefreshParams;
use smelt_runtime_client::protocol::control::EmptyParams;

pub(crate) use smelt_core::provider_quota::ProviderQuotaData;
pub(crate) use smelt_ui::provider_status_global::{ProviderStatus, QuotaStatus, QuotaTarget};

use crate::Workspace;

pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_secs(300);

/// 每家一次网络往返，并发查；给足余量。
const REFRESH_TIMEOUT: Duration = Duration::from_secs(120);

impl Workspace {
    /// 启动应用级额度刷新循环。它只依赖运行时，和 ACP 是否存在、是否运行、当前会话
    /// 是什么类型都无关。
    pub(crate) fn start_provider_quota_watch(&mut self, cx: &mut gpui::Context<Self>) {
        if self.provider_quota_refresh_tx.is_some() {
            return;
        }
        let (refresh_tx, refresh_rx) = smol::channel::unbounded::<QuotaTarget>();
        self.provider_quota_refresh_tx = Some(refresh_tx);
        cx.spawn(async move |this, cx| {
            // 运行时可能还在起来：按断线重连的节奏等到它答话。
            let mut attempt = 0u32;
            let snapshot = loop {
                let fetched = cx
                    .background_executor()
                    .spawn(async {
                        crate::runtime_call::call::<QuotaGet>(
                            EmptyParams::default(),
                            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                        )
                    })
                    .await;
                if let Ok(snapshot) = fetched {
                    break snapshot;
                }
                cx.background_executor()
                    .timer(smelt_core::daemon_state::daemon_reconnect_backoff(attempt))
                    .await;
                attempt = attempt.saturating_add(1);
            };
            if this
                .update(cx, |_workspace, cx| {
                    ProviderStatus::restore_quota(&snapshot.targets, snapshot.statuses, cx);
                })
                .is_err()
            {
                return;
            }
            // 缓存新鲜 → 首轮也等定时器/信号；过期或没有缓存才立即查一次。
            let mut first = !snapshot.fresh;
            loop {
                // 本轮要刷新的账户：None = 全部（定时刷新）；Some = 只刷那家。
                let mut target: Option<QuotaTarget> = None;
                if !first {
                    // 唤醒：用户点击的信号或定时器。race 里的 recv 会**消费**信号，
                    // 所以必须把 recv 拿到的值直接当作本轮信号用，不能事后再 try_recv
                    // （信号已被取走，会退化成「点谁查谁 = 全部刷新」）。
                    let wake = smol::future::race(async { refresh_rx.recv().await.ok() }, async {
                        smol::Timer::after(REFRESH_INTERVAL).await;
                        None
                    })
                    .await;
                    target = wake;
                    // 合并积压信号：race 只消费了第一个，剩下的补进来（最后点击优先）。
                    while let Ok(kind) = refresh_rx.try_recv() {
                        target = Some(kind);
                    }
                }
                first = false;

                if this
                    .update(cx, |_workspace, cx| {
                        let scope = match target {
                            Some(target) => vec![target],
                            None => ProviderStatus::quota_targets(cx),
                        };
                        ProviderStatus::begin_quota_refresh(&scope, target, cx);
                    })
                    .is_err()
                {
                    break;
                }

                let result = cx
                    .background_executor()
                    .spawn(async move {
                        crate::runtime_call::call::<QuotaRefresh>(
                            QuotaRefreshParams { target },
                            REFRESH_TIMEOUT,
                        )
                    })
                    .await;

                // 写回全局单例。Workspace 没了就停掉轮询：它是这条循环的生命周期宿主。
                if this
                    .update(cx, |_workspace, cx| match result {
                        Ok(result) => {
                            // 全量刷新顺带更新展示顺序：用户在 Pi 里新登录一家订阅
                            // provider 后，这是它出现的唯一入口。
                            if target.is_none() {
                                ProviderStatus::restore_quota(&result.targets, Vec::new(), cx);
                            }
                            ProviderStatus::apply_quota(result.statuses, cx);
                        }
                        // 连不上运行时不是额度失效：保留上次的数字，只结束「查询中」。
                        Err(error) => {
                            eprintln!(
                                "[quota] 刷新额度失败：{}",
                                crate::runtime_call::error_text(error)
                            );
                            ProviderStatus::apply_quota(Vec::new(), cx);
                        }
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
