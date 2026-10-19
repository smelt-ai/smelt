//! 当前运行时的连接：切换、断开、探活和状态流。
//!
//! 手机一次只连一个「当前运行时」。阻塞的网络调用都是普通 `pub fn`：FRB 把它们放到
//! Rust 侧的工作线程池执行，Dart 拿到 Future；这里不用 `async fn`，因为 iroh 连接器
//! 内部 `block_on`，不能跑在 tokio 线程上。

use crate::api::error::SmeltError;
use crate::connection::Target;
use crate::frb_generated::StreamSink;

/// 一个运行时的身份。客户端按 `runtime_id` 区分运行时，按 `endpoint_id` + `relay_url`
/// 找到它；保存运行时只需要这几项，没有 token。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeInfo {
    pub runtime_id: String,
    /// 运行时所在机器的主机名，界面拿它当默认名字。老运行时没有。
    pub hostname: Option<String>,
    /// 如 `macos`、`linux`。老运行时没有。
    pub os: Option<String>,
    /// 运行时版本。
    pub version: String,
    pub endpoint_id: String,
    pub relay_url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    /// 没有当前运行时，或已停止（主动断开、未配对、版本不兼容、连接请求失败）。
    Disconnected,
    /// connect / pair 进行中。
    Connecting,
    /// 最近一次探活成功。
    Connected,
    /// 探活失败，后台按指数退避重连中。
    Reconnecting,
}

/// iroh 当前选中的传输路径。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkPath {
    /// 直连，对端是私网 / 链路本地 / 回环地址。
    Lan,
    /// 跨网直连（P2P，打洞成功）。变体不叫 `P2p`：FRB 会把它转成 Dart 的 `p2P`。
    PeerToPeer,
    /// 经 relay 中继。
    Relay,
    /// 还没选出路径，或路径类型认不出。
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkMetrics {
    pub path: LinkPath,
    /// QUIC 层的 RTT。还没选出路径时没有。
    pub rtt_ms: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionEvent {
    pub state: ConnectionState,
    /// 这条状态针对的运行时。主动断开后为空。
    pub endpoint_id: Option<String>,
    /// 只有 `Connected` 时有。
    pub link: Option<LinkMetrics>,
    /// `Reconnecting` 时是最近一次失败；`Disconnected` 时是停下来的原因（主动断开时为空）。
    /// `kind == Unpaired` 表示要重新配对。
    pub error: Option<SmeltError>,
}

/// 切换当前运行时并做一次 `system.describe`。
///
/// 成功后后台每隔几秒探活一次。运行时离线时返回 `Offline` 错误，但状态机不放弃：
/// 进入 `Reconnecting` 在后台重试，连上后发 `Connected`。未配对 / 被撤销时停在
/// `Disconnected`。
pub fn connect(endpoint_id: String, relay_url: Option<String>) -> Result<RuntimeInfo, SmeltError> {
    let dialer = crate::client::dialer()?;
    crate::client::hub().connect(dialer.as_ref(), Target::new(endpoint_id, relay_url))
}

/// 断开当前运行时，停止探活与重连。
pub fn disconnect() {
    crate::client::hub().disconnect();
}

/// 立刻探活一次（App 回到前台时调用）。重连中则立刻重试并把退避归零。结果从
/// [`connection_events`] 推出。没有当前运行时时什么也不做。
pub fn verify_connection() {
    crate::client::hub().verify();
}

/// 连接状态流。订阅时先推一次当前状态，之后只在状态、链路或错误变化时推。
pub fn connection_events(sink: StreamSink<ConnectionEvent>) {
    crate::client::hub().subscribe(Box::new(move |_generation, event| {
        // Dart 侧取消订阅后端口关闭，发送失败，监听随之移除。
        sink.add(event.clone()).is_ok()
    }));
}
