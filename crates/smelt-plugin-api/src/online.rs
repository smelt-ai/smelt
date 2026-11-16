//! 等一台机器上线：长连接断了以后先等它回来再连，等的时候不拨号。

use futures_channel::oneshot;
use gpui::{App, AsyncApp};

use crate::{HostEvent, RuntimeKey};

/// 等那台机器在线（[`crate::Host::machine_online`]）。在线了是 `true`；机器被移除了
/// （[`crate::Host::client`] 给不出）或应用要退出了是 `false`，调用方就此停下。等的时候
/// 跟着 [`HostEvent::MachinesChanged`] 醒，自己不拨号：远程机器关着也只有宿主按它的节奏去探。
///
/// `failures` 是在线以后又连失败的次数：大于 0 时先按它退避再看，在线却连不上（刚断线
/// 还没探出来、运行时那边出错、会话又断了）时不会对着它连环重试。
pub async fn wait_online(machine: &RuntimeKey, failures: u32, cx: &AsyncApp) -> bool {
    if let Some(previous) = failures.checked_sub(1) {
        // 轮询例外：运行时在线、上一次却没连上，没有「可以再试了」的事件可等；离线时不走
        // 这里，退避完回去等在线；周期 250ms 起翻倍，上限 8s。
        cx.background_executor()
            .timer(crate::model::daemon_state::daemon_reconnect_backoff(
                previous,
            ))
            .await;
    }
    let machine = machine.clone();
    let (sender, receiver) = oneshot::channel();
    // 先看、没结果再订，在同一拍里做完：中间漏不掉变化。
    let _watch = cx.update(|cx| {
        if let Some(online) = online_or_removed(&machine, cx) {
            let _ = sender.send(online);
            return None;
        }
        let mut sender = Some(sender);
        Some(
            cx.subscribe(&crate::events(cx), move |_, event: &HostEvent, cx| {
                if *event != HostEvent::MachinesChanged {
                    return;
                }
                if let Some(online) = online_or_removed(&machine, cx)
                    && let Some(sender) = sender.take()
                {
                    let _ = sender.send(online);
                }
            }),
        )
    });
    receiver.await.unwrap_or(false)
}

/// 在线是 `Some(true)`，移除了是 `Some(false)`；还没探过、离线、版本对不上、升级中都是
/// `None`，接着等。
fn online_or_removed(machine: &RuntimeKey, cx: &App) -> Option<bool> {
    let host = crate::host(cx);
    if host.machine_online(machine, cx) {
        Some(true)
    } else if host.client(machine, cx).is_none() {
        Some(false)
    } else {
        None
    }
}
