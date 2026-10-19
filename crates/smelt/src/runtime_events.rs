//! 每个运行时一条常驻事件订阅（设计文档 P2）。阻塞的 socket 读循环放专门的 OS 线程，
//! 断线、运行时没起来就退避重连；事件打上所属运行时，送进同一个处理循环。本机那条随
//! 应用常驻，远程的跟着 [`RuntimeHub`] 增删。
//!
//! 远程运行时每次连之前先对版本（本机守护由桌面自己拉起和升级，版本总是对的）：对不上
//! 就不订阅，把原因记到 [`RuntimeHub`] 上给人看，这条订阅停下，直到再探测时对上了才重起。
//! 不然订阅解不出投影就断，断了又连，会话状态永远到不了，也没人知道为什么。
//!
//! 订阅线程是远程运行时连通性的唯一来源：每次探测的结果、订阅断开，都报回 [`RuntimeHub`]，
//! 侧栏的「离线」跟着它走，不是只在启动时探一次。
//!
//! 运行时比桌面旧时先请它升到桌面的版本（设计文档 D7，见 [`upgrade_then_probe`]），升完
//! 再探；每条订阅线程只请一次，升了还对不上就照常标不兼容，不反复请。

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
    /// 远程运行时各自的停止标记。
    remotes: HashMap<RuntimeKey, Arc<AtomicBool>>,
    _hub_watch: Option<Subscription>,
}

impl Global for RuntimeSubscriptions {}

/// 起本机和已登记远程运行时的订阅，返回事件的接收端。要在 [`RuntimeHub`] 装好之后调。
pub(crate) fn start(cx: &mut App) -> smol::channel::Receiver<RuntimeEvent> {
    let (sender, receiver) = smol::channel::unbounded();
    let (status, status_rx) = smol::channel::unbounded::<(RuntimeKey, RuntimeStatus)>();
    cx.spawn(async move |cx| {
        while let Ok((key, status)) = status_rx.recv().await {
            cx.update(|cx| crate::runtimes::set_status(&key, status, cx));
        }
    })
    .detach();
    spawn_subscription(
        Runtime::local(),
        sender.clone(),
        status.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    cx.set_global(RuntimeSubscriptions {
        sender,
        status,
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
    stop: Arc<AtomicBool>,
) {
    let key = runtime.key().clone();
    std::thread::Builder::new()
        .name(format!("smelt-events-{key:?}"))
        .spawn(move || {
            let mut reconnect_attempt = 0u32;
            let mut upgrade_requested = false;
            while !stop.load(Ordering::Acquire) {
                let mut status =
                    (!runtime.is_local()).then(|| crate::runtimes::status_of(runtime.client()));
                if !upgrade_requested
                    && let Some(RuntimeStatus::Incompatible {
                        detail,
                        runtime_outdated: true,
                    }) = &status
                {
                    upgrade_requested = true;
                    let detail = detail.clone();
                    status = Some(upgrade_then_probe(&runtime, &detail, &status_tx, &stop));
                }
                let connected_at = Instant::now();
                if let Some(status) = &status {
                    let _ = status_tx.try_send((key.clone(), status.clone()));
                }
                match status {
                    Some(RuntimeStatus::Incompatible { .. }) => return,
                    // 连不上就不再拨一次订阅，直接退避。
                    Some(RuntimeStatus::Offline(_)) => {}
                    _ => crate::terminal::subscribe_daemon_states_blocking(
                        runtime.client(),
                        |event| {
                            !stop.load(Ordering::Acquire)
                                && sender.try_send((key.clone(), event)).is_ok()
                        },
                    ),
                }
                if stop.load(Ordering::Acquire) {
                    return;
                }
                // 订阅断了：远程运行时这会儿算连不上，退避后再探测时连上了会报回在线。
                if !runtime.is_local() {
                    let _ = status_tx
                        .try_send((key.clone(), RuntimeStatus::Offline("连接断开".to_string())));
                }
                let _ = sender.try_send((key.clone(), DaemonStateEvent::Disconnected));
                // 短暂可用后断开（daemon 升级）从首档开始；连续失败则指数退避并封顶，
                // 避免运行时没起来时固定节拍反复去连。
                if connected_at.elapsed() >= Duration::from_secs(5) {
                    reconnect_attempt = 0;
                }
                std::thread::sleep(crate::terminal::daemon_reconnect_backoff(reconnect_attempt));
                reconnect_attempt = reconnect_attempt.saturating_add(1);
            }
        })
        .expect("spawn smelt-events 线程");
}

/// 请运行时升到这个桌面的版本，等它换好再探一次，返回探到的状态。升级没成就在原来的
/// 不兼容说明后面写上为什么、该怎么办。
fn upgrade_then_probe(
    runtime: &Runtime,
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
                    detail,
                    runtime_outdated,
                } => RuntimeStatus::Incompatible {
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
/// 版本不兼容的和移除的一样处理；之后再探测对上了，就当新添加的重起一条。
fn sync_remote_subscriptions(cx: &mut App) {
    let Some(hub) = cx.try_global::<RuntimeHub>() else {
        return;
    };
    let remotes: Vec<Runtime> = hub
        .remotes
        .iter()
        .filter(|remote| !matches!(remote.status, RuntimeStatus::Incompatible { .. }))
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
        if let Some(stop) = subscriptions.remotes.remove(key) {
            stop.store(true, Ordering::Release);
        }
    }
    for runtime in remotes {
        if subscriptions.remotes.contains_key(runtime.key()) {
            continue;
        }
        let stop = Arc::new(AtomicBool::new(false));
        subscriptions
            .remotes
            .insert(runtime.key().clone(), Arc::clone(&stop));
        spawn_subscription(
            runtime,
            subscriptions.sender.clone(),
            subscriptions.status.clone(),
            stop,
        );
    }
    for key in removed {
        for id in crate::DaemonStates::forget_runtime(&key, cx) {
            crate::AttentionGlobal::remove_session(&id, cx);
        }
    }
}
