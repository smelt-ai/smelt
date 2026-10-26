//! 每个运行时一条常驻事件订阅（设计文档 P2）。阻塞的 socket 读循环放专门的 OS 线程，
//! 断线、运行时没起来就退避重连；事件打上所属运行时，送进同一个处理循环。本机那条随
//! 应用常驻，远程的跟着 [`RuntimeHub`] 增删。
//!
//! 每次连之前先对版本：对不上就不订阅，把原因记到 [`RuntimeHub`] 上给人看，丢掉它的会话
//! 状态镜像，隔一阵再探，对上了才订阅。不然订阅解不出投影就断，断了又连，会话状态永远
//! 到不了，也没人知道为什么。
//!
//! 本机那条还看着本机守护：没在跑（没起来过，或者运行中退出了）就按 App 里的版本拉起；
//! 版本对不上（App 更新跨过了协议版本，旧守护还在跑）就立刻请它交接到 App 里的版本，
//! 不等空闲——对不上时桌面什么都用不了，等也没有意义。
//!
//! 连不上和对不上都退避重探；「重新连接」（[`reconnect`]）叫醒线程立刻再试，退避从头算。
//!
//! 订阅线程是远程运行时连通性的唯一来源：每次探测的结果、订阅断开，都报回 [`RuntimeHub`]，
//! 侧栏的「离线」跟着它走，不是只在启动时探一次。
//!
//! 运行时比桌面旧时先请它升到桌面的版本（设计文档 D7，见 [`upgrade_then_probe`]），升完
//! 再探；每条订阅线程只请一次，升了还对不上就照常标不兼容，不反复请，人点「重新连接」
//! 才再请一次。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{App, Global, Subscription};

use crate::runtimes::{Runtime, RuntimeHub, RuntimeKey, RuntimeStatus};
use crate::terminal::DaemonStateEvent;

pub(crate) type RuntimeEvent = (RuntimeKey, DaemonStateEvent);

struct RuntimeSubscriptions {
    sender: smol::channel::Sender<RuntimeEvent>,
    /// 订阅线程把远程运行时的连通性报给主线程：哪个运行时、现在怎样。
    status: smol::channel::Sender<(RuntimeKey, RuntimeStatus)>,
    /// 本机那条。
    _local: SubscriptionHandle,
    remotes: HashMap<RuntimeKey, SubscriptionHandle>,
    _hub_watch: Option<Subscription>,
}

/// 叫醒退避中的订阅线程。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wake {
    /// 立刻再探一次，退避从头算。
    Probe,
    /// 人点了「重新连接」：再探，运行时旧就再请它升级一次（每条线程本来只请一次）。
    Reconnect,
}

/// 一条订阅线程的把手：叫醒它，或停下它。把手丢了线程也停。
pub(crate) struct SubscriptionHandle {
    stop: Arc<AtomicBool>,
    wake: std::sync::mpsc::Sender<Wake>,
}

impl SubscriptionHandle {
    fn wake(&self, wake: Wake) {
        let _ = self.wake.send(wake);
    }

    /// 见 [`reconnect`]。
    pub(crate) fn reconnect(&self) {
        self.wake(Wake::Reconnect);
    }

    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// 版本对不上要等人去升级一边，不必勤探：从 30 秒起翻倍，封顶 10 分钟。
fn incompatible_backoff(attempt: u32) -> Duration {
    (Duration::from_secs(30) * 2u32.pow(attempt.min(5))).min(Duration::from_secs(600))
}

/// 请这台运行时的订阅线程立刻重连（机器菜单的「重新连接」）。
pub(crate) fn reconnect(key: &RuntimeKey, cx: &App) {
    if let Some(handle) = cx
        .try_global::<RuntimeSubscriptions>()
        .and_then(|subscriptions| subscriptions.remotes.get(key))
    {
        handle.reconnect();
    }
}

/// 请各台远程运行时立刻再探一次（打开设置窗口时），不重新请求升级。
pub(crate) fn probe_remotes(cx: &App) {
    if let Some(subscriptions) = cx.try_global::<RuntimeSubscriptions>() {
        for handle in subscriptions.remotes.values() {
            handle.wake(Wake::Probe);
        }
    }
}

impl Global for RuntimeSubscriptions {}

/// 起本机和已登记远程运行时的订阅，返回事件的接收端。要在 [`RuntimeHub`] 装好之后调。
pub(crate) fn start(cx: &mut App) -> smol::channel::Receiver<RuntimeEvent> {
    let (sender, receiver) = smol::channel::unbounded();
    let (status, status_rx) = smol::channel::unbounded::<(RuntimeKey, RuntimeStatus)>();
    cx.spawn(async move |cx| {
        while let Ok((key, status)) = status_rx.recv().await {
            cx.update(|cx| {
                // 刚变成对不上：不订阅了，它的会话状态镜像和移除时一样丢掉，对上了重新订阅再建。
                if matches!(status, RuntimeStatus::Incompatible { .. })
                    && !crate::runtimes::runtime_incompatible(&key, cx)
                {
                    forget_states(&key, cx);
                }
                crate::runtimes::set_status(&key, status, cx);
            });
        }
    })
    .detach();
    let local = spawn_subscription(Runtime::local(), sender.clone(), status.clone());
    cx.set_global(RuntimeSubscriptions {
        sender,
        status,
        _local: local,
        remotes: HashMap::new(),
        _hub_watch: None,
    });
    sync_remote_subscriptions(cx);
    let watch = cx.observe_global::<RuntimeHub>(sync_remote_subscriptions);
    cx.global_mut::<RuntimeSubscriptions>()._hub_watch = Some(watch);
    receiver
}

pub(crate) fn spawn_subscription(
    runtime: Runtime,
    sender: smol::channel::Sender<RuntimeEvent>,
    status_tx: smol::channel::Sender<(RuntimeKey, RuntimeStatus)>,
) -> SubscriptionHandle {
    let key = runtime.key().clone();
    let stop = Arc::new(AtomicBool::new(false));
    let (wake, woken) = std::sync::mpsc::channel();
    let handle = SubscriptionHandle {
        stop: Arc::clone(&stop),
        wake,
    };
    std::thread::Builder::new()
        .name(format!("smelt-events-{key:?}"))
        .spawn(move || {
            let mut reconnect_attempt = 0u32;
            let mut incompatible_attempt = 0u32;
            let mut upgrade_requested = false;
            while !stop.load(Ordering::Acquire) {
                let mut status = if runtime.is_local() {
                    crate::runtimes::local_status(runtime.client())
                } else {
                    crate::runtimes::status_of(runtime.client())
                };
                if let RuntimeStatus::Incompatible {
                    version,
                    detail,
                    runtime_outdated: true,
                } = &status
                {
                    let (version, detail) = (version.clone(), detail.clone());
                    if runtime.is_local() {
                        status = upgrade_local_then_probe(
                            &runtime, &version, &detail, &status_tx, &stop,
                        );
                    } else if !upgrade_requested {
                        upgrade_requested = true;
                        status = upgrade_then_probe(&runtime, &version, &detail, &status_tx, &stop);
                    }
                }
                let connected_at = Instant::now();
                let _ = status_tx.try_send((key.clone(), status.clone()));
                let pause = match status {
                    RuntimeStatus::Incompatible { .. } => {
                        reconnect_attempt = 0;
                        let pause = incompatible_backoff(incompatible_attempt);
                        incompatible_attempt = incompatible_attempt.saturating_add(1);
                        pause
                    }
                    // 连不上就不再拨一次订阅，直接退避。
                    RuntimeStatus::Offline(_) => {
                        incompatible_attempt = 0;
                        let pause = crate::terminal::daemon_reconnect_backoff(reconnect_attempt);
                        reconnect_attempt = reconnect_attempt.saturating_add(1);
                        pause
                    }
                    _ => {
                        incompatible_attempt = 0;
                        crate::terminal::subscribe_daemon_states_blocking(
                            runtime.client(),
                            |event| {
                                !stop.load(Ordering::Acquire)
                                    && sender.try_send((key.clone(), event)).is_ok()
                            },
                        );
                        if stop.load(Ordering::Acquire) {
                            return;
                        }
                        // 订阅断了：这会儿算连不上，退避后再探测时连上了会报回在线。本机守护
                        // 是退出了的话，下一轮探测会把它拉起来。
                        let _ = status_tx.try_send((
                            key.clone(),
                            RuntimeStatus::Offline("连接断开".to_string()),
                        ));
                        let _ = sender.try_send((key.clone(), DaemonStateEvent::Disconnected));
                        // 短暂可用后断开（daemon 升级）从首档开始；连续失败则指数退避并封顶，
                        // 避免运行时没起来时固定节拍反复去连。
                        if connected_at.elapsed() >= Duration::from_secs(5) {
                            reconnect_attempt = 0;
                        }
                        let pause = crate::terminal::daemon_reconnect_backoff(reconnect_attempt);
                        reconnect_attempt = reconnect_attempt.saturating_add(1);
                        pause
                    }
                };
                // 退避等待可被叫醒；把手丢了（运行时被移除）就停。
                let wake = match woken.recv_timeout(pause) {
                    Ok(wake) => wake,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                };
                // 攒着的几次叫醒并成一次，有人点过「重新连接」就按它算。
                let reconnect = std::iter::once(wake)
                    .chain(woken.try_iter())
                    .any(|wake| wake == Wake::Reconnect);
                reconnect_attempt = 0;
                incompatible_attempt = 0;
                if reconnect {
                    upgrade_requested = false;
                }
            }
        })
        .expect("spawn smelt-events 线程");
    handle
}

/// 本机守护版本对不上：请它交接到 App 里的版本（会话保留），换好再探一次。它有回合没跑完
/// 时拒绝交接，隔一阵再请，直到换成或者停下；期间报「升级中」，界面上看得到在等什么。
fn upgrade_local_then_probe(
    runtime: &Runtime,
    version: &str,
    detail: &str,
    status_tx: &smol::channel::Sender<(RuntimeKey, RuntimeStatus)>,
    stop: &AtomicBool,
) -> RuntimeStatus {
    use crate::local_daemon::UpgradeOutcome;

    const BUSY_RETRY: Duration = Duration::from_secs(15);
    let report = |progress: &str| {
        let _ = status_tx.try_send((
            runtime.key().clone(),
            RuntimeStatus::Upgrading(progress.to_string()),
        ));
    };
    report("正在升级本机运行时");
    loop {
        match crate::local_daemon::upgrade_daemon() {
            UpgradeOutcome::Busy => {
                report("本机运行时有回合没跑完，跑完就升级");
                std::thread::sleep(BUSY_RETRY);
                if stop.load(Ordering::Acquire) {
                    return crate::runtimes::status_of(runtime.client());
                }
            }
            UpgradeOutcome::Unsupported => {
                return RuntimeStatus::Incompatible {
                    version: version.to_string(),
                    detail: format!(
                        "{detail}。本机运行时太旧，不能无缝升级：在设置里重启守护进程（会断开会话）"
                    ),
                    runtime_outdated: true,
                };
            }
            // 换成了，或者没换成（新守护没起来、交接回滚），都以重新探到的为准。
            UpgradeOutcome::Upgraded | UpgradeOutcome::Failed => {
                return crate::runtimes::local_status(runtime.client());
            }
        }
    }
}

/// 请运行时升到这个桌面的版本，等它换好再探一次，返回探到的状态。升级没成就在原来的
/// 不兼容说明后面写上为什么、该怎么办。
fn upgrade_then_probe(
    runtime: &Runtime,
    version: &str,
    detail: &str,
    status_tx: &smol::channel::Sender<(RuntimeKey, RuntimeStatus)>,
    stop: &AtomicBool,
) -> RuntimeStatus {
    use crate::runtimes::{DESKTOP_VERSION, PushUpgrade, push_upgrade, status_of};

    let key = runtime.key().clone();
    let report = |progress: String| {
        let _ = status_tx.try_send((key.clone(), RuntimeStatus::Upgrading(progress)));
    };
    let still_incompatible = |note: String| RuntimeStatus::Incompatible {
        version: version.to_string(),
        detail: format!("{detail}。{note}"),
        runtime_outdated: true,
    };
    report(format!("正在升级到 v{DESKTOP_VERSION}"));
    let waiting_for_idle = match push_upgrade(runtime.client()) {
        PushUpgrade::HandedOff => false,
        PushUpgrade::Staged(reason) => {
            report(format!(
                "v{DESKTOP_VERSION} 已装好，运行时正忙（{reason}），空闲后自己换过去"
            ));
            true
        }
        PushUpgrade::Current => {
            return still_incompatible(format!(
                "运行时已经是 v{DESKTOP_VERSION}，仍对不上：这个桌面多半是还没发布的开发构建"
            ));
        }
        PushUpgrade::Unsupported => {
            return still_incompatible(format!(
                "这台运行时太旧，不会自动升级，到那台机器上执行一次：{}",
                crate::runtimes::manual_upgrade_command()
            ));
        }
        PushUpgrade::Failed(error) => {
            return still_incompatible(format!("自动升级失败：{error}"));
        }
    };
    // 交接时旧进程连同 iroh 端点一起退出，新进程重新上线要几秒；正忙的要等到它空闲。
    const HANDOFF_GRACE: Duration = Duration::from_secs(120);
    let started = Instant::now();
    loop {
        std::thread::sleep(if waiting_for_idle {
            Duration::from_secs(30)
        } else {
            Duration::from_secs(2)
        });
        let status = status_of(runtime.client());
        let pending = match &status {
            RuntimeStatus::Offline(_) => !waiting_for_idle && started.elapsed() < HANDOFF_GRACE,
            // 还是旧进程：正忙的接着等它空闲，交接过的就是没换成。
            RuntimeStatus::Incompatible { .. } => waiting_for_idle,
            _ => false,
        };
        if !pending || stop.load(Ordering::Acquire) {
            return match status {
                RuntimeStatus::Incompatible {
                    version,
                    detail,
                    runtime_outdated,
                } => RuntimeStatus::Incompatible {
                    version,
                    detail: format!(
                        "{detail}。请它升到 v{DESKTOP_VERSION} 后仍对不上，详见那台机器的 ~/.smelt/daemon.log"
                    ),
                    runtime_outdated,
                },
                status => status,
            };
        }
    }
}

/// 远程运行时的订阅跟着 RuntimeHub 走：新添加的起一条，移除的停掉并丢掉它的状态镜像。
/// 版本不兼容的线程不停，它自己隔一阵再探（见 [`spawn_subscription`]）。
fn sync_remote_subscriptions(cx: &mut App) {
    let Some(hub) = cx.try_global::<RuntimeHub>() else {
        return;
    };
    let remotes: Vec<Runtime> = hub
        .remotes
        .iter()
        .map(|remote| remote.runtime.clone())
        .collect();
    let subscriptions = cx.global_mut::<RuntimeSubscriptions>();
    let removed: Vec<RuntimeKey> = subscriptions
        .remotes
        .keys()
        .filter(|key| !remotes.iter().any(|runtime| runtime.key() == *key))
        .cloned()
        .collect();
    for key in &removed {
        if let Some(handle) = subscriptions.remotes.remove(key) {
            handle.stop();
        }
    }
    for runtime in remotes {
        if subscriptions.remotes.contains_key(runtime.key()) {
            continue;
        }
        let key = runtime.key().clone();
        let handle = spawn_subscription(
            runtime,
            subscriptions.sender.clone(),
            subscriptions.status.clone(),
        );
        subscriptions.remotes.insert(key, handle);
    }
    for key in removed {
        forget_states(&key, cx);
    }
}

/// 丢掉一个运行时的会话状态镜像，连同上面会话的提醒。
fn forget_states(key: &RuntimeKey, cx: &mut App) {
    for id in crate::DaemonStates::forget_runtime(key, cx) {
        crate::AttentionGlobal::remove_session(&id, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_backoff_doubles_up_to_ten_minutes() {
        let pauses: Vec<u64> = (0..8)
            .map(|attempt| incompatible_backoff(attempt).as_secs())
            .collect();
        assert_eq!(pauses, [30, 60, 120, 240, 480, 600, 600, 600]);
    }
}
