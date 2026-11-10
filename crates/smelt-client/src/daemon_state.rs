//! 状态通道（见 docs/archive/state-channel-plan.md）：GUI 侧订阅 smeltd 广播的会话状态
//! 镜像。数据模型和订阅的解码状态机在 `smelt_model::daemon_state`（手机也用同一份），
//! 这里只管阻塞的连接与读行；GUI 那边的 `Entity`/`Global` 包装留在 main crate。

pub use smelt_model::daemon_state::*;
pub use smelt_runtime_client::local::{local_runtime, smeltd_sock_path};

use std::io::{BufRead, BufReader, Write};

/// 阻塞：连接正式 `event_subscribe`，逐行解析稳定 DTO，直到连接断开、协议错误或
/// `emit` 返回 false（收的一方不要了）。Ephemeral 订阅不发送 ack；Lag 后只等待 daemon
/// 的恢复 snapshots。`appearance_requests` 只有本机桌面传 true：外观请求单播给本机，
/// 远程机器的订阅不订。
pub fn subscribe_daemon_states_blocking(
    runtime: &smelt_runtime_client::RuntimeClient,
    appearance_requests: bool,
    mut emit: impl FnMut(DaemonStateEvent) -> bool,
) {
    let Ok(mut s) = runtime.connect() else {
        return;
    };
    let mut client = DaemonStateEventClient::new(SubscriptionShape {
        appearance_requests,
        ..SubscriptionShape::DESKTOP
    });
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

/// 一条连好、请求已经写出去的单独订阅（见 [`TopicEventClient`]）。读放在调用方的线程里
/// （[`Self::run`]）；[`Self::closer`] 从别的线程关掉连接，读的那边随即返回。
pub struct TopicSubscription {
    stream: smelt_runtime_client::RuntimeStream,
    client: TopicEventClient,
}

/// 关掉一条单独订阅的连接。
pub struct TopicSubscriptionCloser(smelt_runtime_client::RuntimeStream);

impl TopicSubscriptionCloser {
    pub fn close(&self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

impl TopicSubscription {
    /// 连上并写出订阅请求。连不上、写不出去是 `Err`，原因给人看。
    pub fn open(
        runtime: &smelt_runtime_client::RuntimeClient,
        topics: Vec<&str>,
    ) -> Result<Self, String> {
        let client = TopicEventClient::new(topics);
        let request = client.request_line().map_err(|error| error.to_string())?;
        let mut stream = runtime.connect().map_err(|error| error.to_string())?;
        stream
            .write_all(request.as_bytes())
            .map_err(|error| error.to_string())?;
        Ok(Self { stream, client })
    }

    pub fn closer(&self) -> Result<TopicSubscriptionCloser, String> {
        self.stream
            .try_clone()
            .map(TopicSubscriptionCloser)
            .map_err(|error| error.to_string())
    }

    /// 阻塞读到连接断开、被关掉或 `emit` 返回 false。运行时拒掉订阅、协议出错是 `Err`。
    pub fn run(
        self,
        mut emit: impl FnMut(TopicMessage) -> bool,
    ) -> Result<(), DaemonEventClientError> {
        let reader = BufReader::new(self.stream);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(message) = self.client.decode_line(&line)?
                && !emit(message)
            {
                return Ok(());
            }
        }
        Ok(())
    }
}
