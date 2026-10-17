//! 每个运行时一条常驻事件订阅（设计文档 P2）。阻塞的 socket 读循环放专门的 OS 线程，
//! 断线、运行时没起来就退避重连；事件打上所属运行时，送进同一个处理循环。本机那条随
//! 应用常驻，远程的跟着 [`RuntimeHub`] 增删。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{App, Global, Subscription};

use crate::runtimes::{Runtime, RuntimeHub, RuntimeKey};
use crate::terminal::DaemonStateEvent;

pub(crate) type RuntimeEvent = (RuntimeKey, DaemonStateEvent);

struct RuntimeSubscriptions {
    sender: smol::channel::Sender<RuntimeEvent>,
    /// 远程运行时各自的停止标记。
    remotes: HashMap<RuntimeKey, Arc<AtomicBool>>,
    _hub_watch: Option<Subscription>,
}

impl Global for RuntimeSubscriptions {}

/// 起本机和已登记远程运行时的订阅，返回事件的接收端。要在 [`RuntimeHub`] 装好之后调。
pub(crate) fn start(cx: &mut App) -> smol::channel::Receiver<RuntimeEvent> {
    let (sender, receiver) = smol::channel::unbounded();
    spawn_subscription(
        Runtime::local(),
        sender.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    cx.set_global(RuntimeSubscriptions {
        sender,
        remotes: HashMap::new(),
        _hub_watch: None,
    });
    sync_remote_subscriptions(cx);
    let watch = cx.observe_global::<RuntimeHub>(sync_remote_subscriptions);
    cx.global_mut::<RuntimeSubscriptions>()._hub_watch = Some(watch);
    receiver
}

fn spawn_subscription(
    runtime: Runtime,
    sender: smol::channel::Sender<RuntimeEvent>,
    stop: Arc<AtomicBool>,
) {
    let key = runtime.key().clone();
    std::thread::Builder::new()
        .name(format!("smelt-events-{key:?}"))
        .spawn(move || {
            let mut reconnect_attempt = 0u32;
            while !stop.load(Ordering::Acquire) {
                let connected_at = Instant::now();
                crate::terminal::subscribe_daemon_states_blocking(runtime.client(), |event| {
                    !stop.load(Ordering::Acquire) && sender.try_send((key.clone(), event)).is_ok()
                });
                if stop.load(Ordering::Acquire) {
                    return;
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

/// 远程运行时的订阅跟着 RuntimeHub 走：新添加的起一条，移除的停掉并丢掉它的状态镜像。
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
        spawn_subscription(runtime, subscriptions.sender.clone(), stop);
    }
    for key in removed {
        for id in crate::DaemonStates::forget_runtime(&key, cx) {
            crate::AttentionGlobal::remove_session(&id, cx);
        }
    }
}
