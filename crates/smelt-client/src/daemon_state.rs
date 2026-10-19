//! 状态通道（见 docs/archive/state-channel-plan.md）：GUI 侧订阅 smeltd 广播的会话状态
//! 镜像。数据模型和订阅的解码状态机在 `smelt_model::daemon_state`（手机也用同一份），
//! 这里只管阻塞的连接与读行；GUI 那边的 `Entity`/`Global` 包装留在 main crate。

pub use smelt_model::daemon_state::*;
pub use smelt_runtime_client::local::{local_runtime, smeltd_sock_path};

use std::io::{BufRead, BufReader, Write};

/// 阻塞：连接正式 `event_subscribe`，逐行解析稳定 DTO，直到连接断开、协议错误或
/// `emit` 返回 false（收的一方不要了）。Ephemeral 订阅不发送 ack；Lag 后只等待 daemon
/// 的恢复 snapshots。
pub fn subscribe_daemon_states_blocking(
    runtime: &smelt_runtime_client::RuntimeClient,
    mut emit: impl FnMut(DaemonStateEvent) -> bool,
) {
    let Ok(mut s) = runtime.connect() else {
        return;
    };
    let mut client = DaemonStateEventClient::new(SubscriptionShape::DESKTOP);
    let Ok(request) = client.request_line() else {
        return;
    };
    if s.write_all(request.as_bytes()).is_err() {
        return;
    }
    let reader = BufReader::new(s);
    for line in reader.lines().map_while(Result::ok) {
        let event = match client.decode_line(&line) {
            Ok(Some(event)) => event,
            Ok(None) => continue,
            Err(error) => {
                eprintln!("[daemon-state] event subscribe decode failed: {error}");
                return;
            }
        };
        if !emit(event) {
            return;
        }
    }
}
