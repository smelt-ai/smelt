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
//! 远程机器回来不会通知这台桌面，离线时的退避翻倍到分钟级；应用回到前台、打开设置窗口时
//! （[`probe_remotes`]）叫醒连不上的再探一次。
//!
//! 订阅线程是远程运行时连通性的唯一来源：每次探测的结果、订阅断开，都报回 [`RuntimeHub`]，
//! 侧栏的「离线」跟着它走，不是只在启动时探一次。
//!
//! 运行时比桌面旧时先请它升到桌面的版本（设计文档 D7，见 [`upgrade_then_probe`]），升完
//! 再探；每条订阅线程只请一次，升了还对不上就照常标不兼容，不反复请，人点「重新连接」
//! 才再请一次。人也可以随时点「升级运行时」（[`upgrade_remote`]）：连得上就请，不管兼不兼容。
//!
//! 别处要读运行时的（工作区目录、配置、额度、终端重连），用 [`wait_online`] 等这里探到
//! 在线再读，不各自拨号重试。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{App, AsyncApp, Global, Subscription};

use crate::runtimes::{IncompatibleFix, Runtime, RuntimeHub, RuntimeKey, RuntimeStatus};
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
    /// 立刻再探一次。退避不从头算：还是连不上就接着按原来的间隔等，应用每回前台一次
    /// 就只多探一次，不会又从 250ms 起密集地拨。
    Probe,
    /// 人点了「重新连接」：再探，退避从头算，运行时旧就再请它升级一次（每条线程本来
    /// 只请一次）。
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
// 轮询例外：版本对不上要等有人去升级某一边，升级完不会通知这台桌面，只能隔一阵再探；
// 周期 30s 起翻倍，上限 10min。
fn incompatible_backoff(attempt: u32) -> Duration {
    (Duration::from_secs(30) * 2u32.pow(attempt.min(5))).min(Duration::from_secs(600))
}

/// 远程机器连不上时隔多久再探：250ms 起翻倍，封顶 5 分钟。刚断的（升级重启、换网络）
/// 几秒内就能接回；关机好几天的，一小时只拨十来次。
// 轮询例外：远程机器开机、联网后不会通知这台桌面，连上之前收不到它的任何东西，只能隔一阵
// 拨一次；每次拨号都要过 relay 和地址发现，关机时一直勤探白白耗电耗流量，所以封顶拉到分钟
// 级，应用回到前台、打开设置窗口（[`probe_remotes`]）和「重新连接」会立刻叫醒；周期 250ms
// 起翻倍，上限 5min。
fn remote_offline_backoff(attempt: u32) -> Duration {
    (Duration::from_millis(250) * 2u32.pow(attempt.min(11))).min(Duration::from_secs(300))
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

/// 人点了「升级运行时」：不管现在兼不兼容，连得上就请它升到这个桌面的版本。
///
/// 不经订阅线程：在线时它正阻塞在事件流上，叫不醒。升级走守护协议的 `runtime_upgrade`，
/// 另开一条连接发，版本对不上也发得过去。升上去了运行时会重启，订阅线程自己察觉断线、
/// 重连、再探；没升成就当场再探一次，把状态恢复成探到的样子，原因记在这台机器上。
pub(crate) fn upgrade_remote(key: &RuntimeKey, cx: &mut App) {
    use crate::runtimes::{DESKTOP_VERSION, PushUpgrade};

    let Some(runtime) = crate::runtimes::runtime(key, cx) else {
        return;
    };
    let Some(status_tx) = cx
        .try_global::<RuntimeSubscriptions>()
        .map(|subscriptions| subscriptions.status.clone())
    else {
        return;
    };
    crate::runtimes::set_upgrade_note(key, None, cx);
    let _ = status_tx.try_send((
        key.clone(),
        RuntimeStatus::Upgrading(format!("正在升级到 v{DESKTOP_VERSION}")),
    ));
    let key = key.clone();
    cx.spawn(async move |cx| {
        let client = runtime.client().clone();
        let (outcome, status) = cx
            .background_executor()
            .spawn(async move {
                let outcome = crate::runtimes::push_upgrade(&client);
                // 升上去了它正在重启，这会儿探只会是离线，留给订阅线程。
                let status = (outcome != PushUpgrade::Restarting)
                    .then(|| crate::runtimes::status_of(&client));
                (outcome, status)
            })
            .await;
        let note = match outcome {
            PushUpgrade::Restarting => None,
            PushUpgrade::Current => Some(format!("已经是 v{DESKTOP_VERSION}，没有可升的")),
            PushUpgrade::Staged(reason) => Some(format!("新版本已装好，{reason}")),
            PushUpgrade::Unsupported => Some(format!(
                "这台运行时太旧，不会自动升级，到那台机器上执行一次：{}",
                crate::runtimes::manual_upgrade_command()
            )),
            PushUpgrade::Failed(error) => Some(format!("升级失败：{error}")),
        };
        let restarting = note.is_none();
        let _ = status_tx.try_send((
            key.clone(),
            status.unwrap_or_else(|| RuntimeStatus::Upgrading("正在重启".to_string())),
        ));
        cx.update(|cx| {
            crate::runtimes::set_upgrade_note(&key, note, cx);
            // 版本对不上时订阅线程睡在退避里，可能要等好几分钟：叫醒它照新版本再探。
            if restarting
                && let Some(handle) = cx
                    .try_global::<RuntimeSubscriptions>()
                    .and_then(|subscriptions| subscriptions.remotes.get(&key))
            {
                handle.wake(Wake::Probe);
            }
        });
    })
    .detach();
}

/// 请连不上、版本对不上的远程运行时立刻再探一次（打开设置窗口、应用回到前台时），不重新
/// 请求升级。在线的连着订阅，断了自己会知道；连接中、升级中的正在探，都不用叫。
pub(crate) fn probe_remotes(cx: &App) {
    if let Some(subscriptions) = cx.try_global::<RuntimeSubscriptions>() {
        for (key, handle) in &subscriptions.remotes {
            if crate::runtimes::runtime_can_reconnect(key, cx) {
                handle.wake(Wake::Probe);
            }
        }
    }
}

/// 读运行时之前等它在线。连通性由订阅线程探、记在 [`RuntimeHub`] 上，这里只看着它：在线
/// 了才往下走，等的时候自己不拨号，远程机器关着也只有订阅线程按它的节奏去探。
///
/// `failures` 是这次之前已经连着失败了几次：不是 0 就先退避一会儿再看在不在线，在线却
/// 失败（刚断线订阅还没察觉、运行时那边出错、会话又断了）时不会对着它连环重试。运行时被
/// 移除了（远程机器删掉了）或应用要退出了返回 `false`，调用方就此停下。
pub(crate) async fn wait_online(key: &RuntimeKey, failures: u32, cx: &AsyncApp) -> bool {
    if let Some(previous) = failures.checked_sub(1) {
        // 轮询例外：运行时在线、上一次却没成，没有「可以再试了」的事件可等；离线时不走这里，
        // 退避完回去等在线；周期 250ms 起翻倍，上限 8s。
        cx.background_executor()
            .timer(crate::terminal::daemon_reconnect_backoff(previous))
            .await;
    }
    let key = key.clone();
    let ready = move |cx: &App| online_or_removed(&key, cx);
    smelt_ui::global_wait::wait_for_global::<RuntimeHub, bool>(cx, ready)
        .await
        .unwrap_or(false)
}

/// 在线是 `Some(true)`，移除了是 `Some(false)`；还没探过、离线、版本对不上、升级中都是
/// `None`，接着等。
fn online_or_removed(key: &RuntimeKey, cx: &App) -> Option<bool> {
    if crate::runtimes::runtime_online(key, cx) {
        Some(true)
    } else if crate::runtimes::client(key, cx).is_none() {
        Some(false)
    } else {
        None
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
                    fix,
                } = &status
                    && fix.runtime_outdated()
                {
                    let (version, detail, fix) = (version.clone(), detail.clone(), *fix);
                    if runtime.is_local() {
                        status = upgrade_local_then_probe(
                            &runtime, &version, &detail, fix, &status_tx, &stop,
                        );
                    } else if fix == IncompatibleFix::UpdateSmeltThere {
                        // 请不动它：说清到哪儿去更新，不发注定失败的升级请求。
                        status = RuntimeStatus::Incompatible {
                            version,
                            detail: format!(
                                "{detail}。这台运行时随那台机器上的 Smelt 一起更新，不能远程升级：\
                                 到那台机器上把 Smelt 更新到 v{}",
                                crate::runtimes::DESKTOP_VERSION
                            ),
                            fix,
                        };
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
                        let pause = if runtime.is_local() {
                            // 轮询例外：本机守护拉不起来时没有「现在能起了」的事件，探一次就是
                            // 再拉一次；周期 250ms 起翻倍，上限 8s。
                            crate::terminal::daemon_reconnect_backoff(reconnect_attempt)
                        } else {
                            remote_offline_backoff(reconnect_attempt)
                        };
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
                        // 轮询例外：订阅断了，对端重新能连时不会通知我们，只能隔一会儿再探；
                        // 周期 250ms 起翻倍，上限 8s（再探仍连不上就转到上面离线的退避）。
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
                // 攒着的几次叫醒并成一次，有人点过「重新连接」就按它算。只是再探一次
                // （回到前台、开设置窗口）的不动退避，见 [`Wake::Probe`]。
                let reconnect = std::iter::once(wake)
                    .chain(woken.try_iter())
                    .any(|wake| wake == Wake::Reconnect);
                if reconnect {
                    reconnect_attempt = 0;
                    incompatible_attempt = 0;
                    upgrade_requested = false;
                    // 拨号连不上要等到拨号时限才知道，这期间灯还停在「离线」，人会以为没点上。
                    let _ = status_tx.try_send((key.clone(), RuntimeStatus::Connecting));
                }
            }
        })
        .expect("spawn smelt-events 线程");
    handle
}

/// 本机守护版本对不上：停掉旧进程并拉起 App 里的那份，再探一次。
/// 进行中的回合会中断。只试一次，不再因为忙碌反复等。
fn upgrade_local_then_probe(
    runtime: &Runtime,
    version: &str,
    detail: &str,
    fix: IncompatibleFix,
    status_tx: &smol::channel::Sender<(RuntimeKey, RuntimeStatus)>,
    _stop: &AtomicBool,
) -> RuntimeStatus {
    use crate::local_daemon::UpgradeOutcome;

    let _ = status_tx.try_send((
        runtime.key().clone(),
        RuntimeStatus::Upgrading("正在重启本机运行时".to_string()),
    ));
    match crate::local_daemon::upgrade_daemon() {
        UpgradeOutcome::Upgraded => crate::runtimes::local_status(runtime.client()),
        UpgradeOutcome::Failed => RuntimeStatus::Incompatible {
            version: version.to_string(),
            detail: format!("{detail}。本机运行时没有换成当前版本，可在设置里重启守护进程"),
            fix,
        },
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
        fix: IncompatibleFix::UpgradeRuntime,
    };
    report(format!("正在升级到 v{DESKTOP_VERSION}"));
    match push_upgrade(runtime.client()) {
        PushUpgrade::Restarting => {}
        // 老运行时正忙时会先装好、等空闲再换。
        PushUpgrade::Staged(reason) => {
            return still_incompatible(reason);
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
    }
    // 重启时旧进程连同 iroh 端点一起退出，新进程重新上线要几秒。
    const RESTART_GRACE: Duration = Duration::from_secs(120);
    let started = Instant::now();
    loop {
        // 轮询例外：重启中的运行时连同 iroh 端点一起下线，新进程上线不会通知这台桌面，只能
        // 隔一会儿探一次；周期 2s，上限 120s。
        std::thread::sleep(Duration::from_secs(2));
        let status = status_of(runtime.client());
        // 连不上就是还在重启；连上了还是旧版本，就是没换成。
        let pending =
            matches!(status, RuntimeStatus::Offline(_)) && started.elapsed() < RESTART_GRACE;
        if !pending || stop.load(Ordering::Acquire) {
            return match status {
                RuntimeStatus::Incompatible {
                    version,
                    detail,
                    fix,
                } => RuntimeStatus::Incompatible {
                    version,
                    detail: format!(
                        "{detail}。请它升到 v{DESKTOP_VERSION} 后仍对不上，详见那台机器的 ~/.smelt/daemon.log"
                    ),
                    fix,
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

    #[test]
    fn remote_offline_backoff_doubles_up_to_five_minutes() {
        let pauses: Vec<u128> = [0, 1, 2, 5, 9, 10, 11, 12, 100, u32::MAX]
            .into_iter()
            .map(|attempt| remote_offline_backoff(attempt).as_millis())
            .collect();
        assert_eq!(
            pauses,
            [250, 500, 1_000, 8_000, 128_000, 256_000, 300_000, 300_000, 300_000, 300_000]
        );
    }
}
