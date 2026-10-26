//! 机队：所有配对过的机器、各自的连接状态，以及登记信息的增删改。
//!
//! 内核同时连着所有登记过的机器，每台各有一个连接状态机，离线的机器自己退避重连，互不
//! 影响；没有「当前运行时」。阻塞的调用都是普通 `pub fn`：FRB 把它们放到 Rust 侧的工作
//! 线程池执行，Dart 拿到 Future；这里不用 `async fn`，因为 iroh 连接器内部 `block_on`，
//! 不能跑在 tokio 线程上。

use crate::api::error::SmeltError;
use crate::frb_generated::StreamSink;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    /// 已停止：未配对 / 被撤销、版本不兼容、配对失败，或还没开始连。
    Disconnected,
    /// 正在连（启动、重连、重新配对）。
    Connecting,
    /// 连着：握手或确认成功，之后传输层没报断开。
    Connected,
    /// 链路断了或确认失败，后台按指数退避重连中。
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
    /// QUIC 层的 RTT，连上、换路径、确认时取的快照。还没选出路径时没有。
    pub rtt_ms: Option<u32>,
}

/// 一台登记过的机器。没有 token：运行时按这台手机的设备密钥认人。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeRecord {
    pub runtime_id: String,
    /// 用户起的名字，默认是主机名。
    pub name: String,
    pub endpoint_id: String,
    pub relay_url: Option<String>,
    /// 运行时所在机器的主机名。老运行时、或从旧版导入还没连上过的机器没有。
    pub hostname: Option<String>,
    /// 如 `macos`、`linux`。
    pub os: Option<String>,
    /// 运行时版本，每次连上时更新。从旧版导入还没连上过时为空串。
    pub version: String,
    /// 灶台标签色在色板里的序号，`0..8`。
    pub accent: u32,
    /// 列表顺序，从 0 起连续。
    pub position: u32,
    /// 最近一次配对的时间（unix 毫秒）。
    pub paired_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeStatus {
    pub state: ConnectionState,
    /// 只有 `Connected` 时有。
    pub link: Option<LinkMetrics>,
    /// `Reconnecting` 时是最近一次失败；`Disconnected` 时是停下来的原因。
    /// `kind == Unpaired` 表示要重新配对。
    pub error: Option<SmeltError>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetRuntime {
    pub record: RuntimeRecord,
    pub status: RuntimeStatus,
}

/// 整份机队，按 `position` 排好。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetView {
    pub runtimes: Vec<FleetRuntime>,
}

/// 旧版（钥匙串里的 `smelt.runtimes.v1`）存的一台机器。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeImport {
    pub runtime_id: String,
    pub endpoint_id: String,
    pub relay_url: Option<String>,
    pub name: String,
}

/// 机队流：订阅时先推一次当前整份，之后登记或任何一台的连接状态变了就整份推一次。
pub fn fleet_events(sink: StreamSink<FleetView>) {
    crate::fleet::fleet().subscribe(Box::new(move |view| {
        // Dart 侧取消订阅后端口关闭，发送失败，监听随之移除。
        sink.add(view.clone()).is_ok()
    }));
}

/// 改名。名字去掉首尾空白后不能为空。
pub fn rename_runtime(runtime_id: String, name: String) -> Result<(), SmeltError> {
    crate::fleet::fleet().rename(&runtime_id, &name)
}

/// 改灶台标签色，`accent` 是色板序号 `0..8`。
pub fn set_runtime_accent(runtime_id: String, accent: u32) -> Result<(), SmeltError> {
    crate::fleet::fleet().set_accent(&runtime_id, accent)
}

/// 把一台机器挪到列表的第 `to_index` 位（超出末尾就放到末尾），其余顺延。
pub fn move_runtime(runtime_id: String, to_index: u32) -> Result<(), SmeltError> {
    crate::fleet::fleet().move_runtime(&runtime_id, to_index)
}

/// 忘记一台机器：断开它，删掉它的登记、草稿和离线缓存。运行时那边的设备授权不动
/// （要撤销在电脑上撤）。
pub fn forget_runtime(runtime_id: String) -> Result<(), SmeltError> {
    crate::fleet::fleet().forget(&runtime_id)
}

/// 立刻重新连一台机器（停在 `Disconnected` 时用）。阻塞到这次握手有结果；离线时返回
/// `Offline` 但会转入后台重连。
pub fn reconnect_runtime(runtime_id: String) -> Result<(), SmeltError> {
    crate::fleet::fleet().reconnect(&runtime_id)
}

/// 所有机器立刻确认一次链路（App 回到前台、下拉刷新时调用）。重连中的立刻重试并把退避归零。结果从
/// [`fleet_events`] 推出。
pub fn verify_connections() {
    crate::fleet::fleet().verify();
}

/// 一次性导入旧版存的机器，返回这次新登记的。已登记的（同一 `runtime_id` 或 endpoint）
/// 和不合法的跳过。导入的机器立刻开始连。
pub fn import_runtimes(runtimes: Vec<RuntimeImport>) -> Result<Vec<RuntimeRecord>, SmeltError> {
    crate::fleet::fleet().import(runtimes)
}
