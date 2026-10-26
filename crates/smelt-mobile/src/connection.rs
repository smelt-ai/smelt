//! 一台机器的连接状态机。机队里每个节点一个（见 `crate::fleet`），彼此独立。
//!
//! ```text
//!            connect / pair
//! Disconnected ─────────────> Connecting ──成功──> Connected <──┐
//!      ^                          │                   │ 链路断开 / 确认失败 │ 重连成功
//!      │      未配对 / 不兼容 / pair 失败 / 其它失败    v           │
//!      ├──────────────────────────┤             Reconnecting ─────┘
//!      │                          └─connect 离线──>   │（退避 2s 起，翻倍，封顶 30s）
//!      └──── disconnect（任意状态）/ 重连未配对、不兼容 ──┘
//! ```
//!
//! 已连上时不定时探活：链路断开由传输层报（[`LinkChange::Closed`]：iroh 的 QUIC 每 5 秒保活，
//! 对端消失约 30 秒后空闲超时；运行时撤销设备、重启时直接关连接），这条链路上的长连接也同时
//! 读到 EOF。换传输路径时刷新路径和 RTT（[`LinkChange::Path`]）。回前台、下拉刷新时
//! [`ConnectionHub::verify`] 在现有链路上确认一次：系统在后台冻结过保活，网络可能已经换过。
//!
//! 每次 connect / pair / disconnect 换一代（generation）：旧会话的线程被叫停，它晚到的
//! 结果、以及被取代的 connect / pair 的结果，都按代号丢弃。一个状态机只连一台机器，换代
//! 只说明重新连过（重连、endpoint 换形态、重新配对），不说明换了机器。传输经 [`Dialer`]
//! 注入，测试用假运行时。

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use smelt_model::control_api::DevicePair;
use smelt_model::remote_config::PairParams;
use smelt_runtime_client::RuntimeClient;
use smelt_runtime_client::protocol::control::{
    ControlClientError, ControlErrorCode, EmptyParams, SystemDescribe, SystemDescribeResult,
};

use crate::api::error::{SmeltError, SmeltErrorKind, error};
use crate::api::fleet::{ConnectionState, LinkMetrics};

/// `system.describe` 给出的机器身份。客户端按 `runtime_id` 区分机器，按 `endpoint_id` +
/// `relay_url` 找到它。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeInfo {
    pub runtime_id: String,
    /// 运行时所在机器的主机名，登记时当默认名字。老运行时没有。
    pub hostname: Option<String>,
    /// 如 `macos`、`linux`。老运行时没有。
    pub os: Option<String>,
    pub version: String,
    pub endpoint_id: String,
    pub relay_url: Option<String>,
}

/// 状态机推出的一条状态。对 Dart 只露出其中的 `state` / `link` / `error`（见
/// `crate::api::fleet::RuntimeStatus`）；`endpoint_id` 给缓存按机器分库用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionEvent {
    pub state: ConnectionState,
    /// 这条状态针对的机器。主动断开后为空。
    pub endpoint_id: Option<String>,
    /// 只有 `Connected` 时有。
    pub link: Option<LinkMetrics>,
    /// `Reconnecting` 时是最近一次失败；`Disconnected` 时是停下来的原因（主动断开时为空）。
    /// `kind == Unpaired` 表示要重新配对。
    pub error: Option<SmeltError>,
}

/// 一个运行时的地址。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub endpoint_id: String,
    pub relay_url: Option<String>,
}

impl Target {
    /// Dart 传空串表示没有 relay。
    pub(crate) fn new(endpoint_id: String, relay_url: Option<String>) -> Self {
        Self {
            endpoint_id: endpoint_id.trim().to_string(),
            relay_url: relay_url
                .map(|url| url.trim().to_string())
                .filter(|url| !url.is_empty()),
        }
    }
}

/// 链路上的变化，见 [`Link::watch`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkChange {
    /// 换了传输路径，路径和 RTT 要刷新。
    Path,
    /// 连接断了，这条链路上的长连接都已读到 EOF。
    Closed,
}

/// 链路变化的回调。在传输层的线程上调：不能阻塞，也不能在里面放掉 [`Link`]（iroh 客户端
/// 的 Drop 会 `block_on`）。
pub(crate) type OnLinkChange = Box<dyn Fn(LinkChange) + Send + Sync>;

/// 一条到运行时的链路：发请求用的客户端，读当前传输路径、盯连接变化的方法。
#[derive(Clone)]
pub(crate) struct Link {
    pub client: RuntimeClient,
    /// 阻塞（iroh 连接器内部 `block_on`），只在普通线程上调。
    pub metrics: Arc<dyn Fn() -> LinkMetrics + Send + Sync>,
    /// 盯着这条链路当前的连接，返回的守卫丢掉就不再盯。还没连上或已经断了时立刻报
    /// [`LinkChange::Closed`]。阻塞（`block_on`），只在普通线程上调。
    pub watch: Arc<dyn Fn(OnLinkChange) -> Box<dyn Send> + Send + Sync>,
}

/// 到某个运行时的路由，能反复建新链路。
pub(crate) trait Route: Send + Sync {
    /// 新建一条链路，不复用旧连接：重连时旧 QUIC 连接可能已僵死但还没察觉。
    fn link(&self) -> Link;
}

/// 按地址找路由。
pub(crate) trait Dialer: Send + Sync {
    /// 阻塞（可能要绑 iroh endpoint），只在普通线程上调。
    fn route(&self, target: &Target) -> Result<Arc<dyn Route>, SmeltError>;
}

/// 各种时限。生产值见 [`Timing::PRODUCTION`]，测试用毫秒级。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// 确认链路（回前台、重连）时 `system.describe` 的读写时限。
    pub probe_timeout: Duration,
    /// connect 的 `system.describe` 时限。拨号自身另有 iroh 的 20 秒上限。
    pub connect_timeout: Duration,
    /// `device.pair` 时限：要走公网拨号，比本机调用慢得多。
    pub pair_timeout: Duration,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
}

impl Timing {
    pub(crate) const PRODUCTION: Self = Self {
        probe_timeout: Duration::from_secs(5),
        connect_timeout: Duration::from_secs(10),
        pair_timeout: Duration::from_secs(30),
        backoff_base: Duration::from_secs(2),
        backoff_max: Duration::from_secs(30),
    };

    /// 连续失败 `failures`（≥1）次后，下一次重试前等多久。
    fn backoff(&self, failures: u32) -> Duration {
        let factor = 1u32 << failures.saturating_sub(1).min(16);
        self.backoff_base
            .saturating_mul(factor)
            .min(self.backoff_max)
    }
}

/// 会话目录、对话、终端拿这台机器当前的连接用。
#[derive(Clone, Debug)]
pub(crate) struct CurrentRuntime {
    /// 当前代号。和 [`Listener`] 收到的不同就说明重新连过（或断开过）。
    pub generation: u64,
    pub endpoint_id: String,
    /// 重连成功后会换成新链路的客户端，长连接断了重试时重新取。
    pub client: RuntimeClient,
}

/// 状态监听：参数是当前代号和事件，返回 false 表示不再监听。在状态锁里调用，不能阻塞，
/// 也不能回调 [`ConnectionHub`]。
pub(crate) type Listener = Box<dyn Fn(u64, &ConnectionEvent) -> bool + Send>;

pub(crate) struct ConnectionHub {
    shared: Arc<Shared>,
    timing: Timing,
}

struct Shared {
    inner: Mutex<Inner>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

struct Inner {
    generation: u64,
    session: Option<Arc<Session>>,
    event: ConnectionEvent,
    listeners: Vec<Listener>,
}

impl Inner {
    fn publish(&mut self, event: ConnectionEvent) {
        if self.event == event {
            return;
        }
        self.event = event;
        let generation = self.generation;
        let event = &self.event;
        self.listeners
            .retain(|listener| listener(generation, event));
    }

    /// 停掉当前会话并换代。
    fn retire(&mut self) {
        self.generation += 1;
        if let Some(session) = self.session.take() {
            session.signals.stop();
        }
    }

    fn is_current(&self, session: &Arc<Session>) -> bool {
        self.session
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, session))
    }
}

/// 一次连接会话：从 connect / pair 成功（或 connect 离线转入重连）到被取代或停下。
struct Session {
    target: Target,
    route: Arc<dyn Route>,
    link: Mutex<Link>,
    /// 链路回调只碰它、不碰会话：会话持有链路，在传输层的线程上放掉链路会 `block_on`。
    signals: Arc<Signals>,
}

struct Signals {
    state: Mutex<Signal>,
    wake: Condvar,
}

#[derive(Default)]
struct Signal {
    stop: bool,
    probe_now: bool,
    /// 当前盯着的链路的编号。换过链路后，旧链路晚到的回调按它丢弃。
    watching: u64,
    path_changed: bool,
    link_closed: bool,
}

enum Wake {
    Stop,
    Now,
    Path,
    Closed,
    Timeout,
}

impl Session {
    fn link(&self) -> Link {
        self.link.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 盯着当前链路，之前盯的作废。阻塞，只在会话线程上调。
    fn watch(&self) -> Box<dyn Send> {
        let watching = {
            let mut signal = self.signals.lock();
            signal.watching += 1;
            signal.path_changed = false;
            signal.link_closed = false;
            signal.watching
        };
        let signals = Arc::clone(&self.signals);
        let link = self.link();
        let guard = (link.watch)(Box::new(move |change| signals.on_link(watching, change)));
        // 读完路径到盯上之间换过路径的话收不到事件：登记后补读一次（没变不会推）。
        self.signals.lock().path_changed = true;
        guard
    }
}

impl Signals {
    fn lock(&self) -> MutexGuard<'_, Signal> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn stop(&self) {
        self.lock().stop = true;
        self.wake.notify_all();
    }

    fn probe_now(&self) {
        self.lock().probe_now = true;
        self.wake.notify_all();
    }

    fn on_link(&self, watching: u64, change: LinkChange) {
        let mut signal = self.lock();
        if signal.watching != watching {
            return;
        }
        match change {
            LinkChange::Path => signal.path_changed = true,
            LinkChange::Closed => signal.link_closed = true,
        }
        drop(signal);
        self.wake.notify_all();
    }

    /// 已连上时等：叫停、链路断了、要求确认、换了路径。不设超时，全靠事件叫醒。
    fn wait_connected(&self) -> Wake {
        let signal = self.lock();
        let mut signal = self
            .wake
            .wait_while(signal, |s| {
                !s.stop && !s.link_closed && !s.probe_now && !s.path_changed
            })
            .unwrap_or_else(|e| e.into_inner());
        if signal.stop {
            Wake::Stop
        } else if std::mem::take(&mut signal.link_closed) {
            // 反正要建新链路重连，攒下的确认和换路径一并作废。
            signal.probe_now = false;
            signal.path_changed = false;
            Wake::Closed
        } else if std::mem::take(&mut signal.probe_now) {
            Wake::Now
        } else {
            signal.path_changed = false;
            Wake::Path
        }
    }

    /// 重连的退避：等 `delay`，叫停或要求立刻重试时提前返回。
    fn wait_backoff(&self, delay: Duration) -> Wake {
        let signal = self.lock();
        let (mut signal, _) = self
            .wake
            .wait_timeout_while(signal, delay, |s| !s.stop && !s.probe_now)
            .unwrap_or_else(|e| e.into_inner());
        if signal.stop {
            Wake::Stop
        } else if std::mem::take(&mut signal.probe_now) {
            Wake::Now
        } else {
            Wake::Timeout
        }
    }
}

fn event(
    state: ConnectionState,
    target: Option<&Target>,
    link: Option<LinkMetrics>,
    error: Option<SmeltError>,
) -> ConnectionEvent {
    ConnectionEvent {
        state,
        endpoint_id: target.map(|target| target.endpoint_id.clone()),
        link,
        error,
    }
}

impl ConnectionHub {
    pub(crate) fn new(timing: Timing) -> Self {
        Self {
            shared: Arc::new(Shared {
                inner: Mutex::new(Inner {
                    generation: 0,
                    session: None,
                    event: event(ConnectionState::Disconnected, None, None, None),
                    listeners: Vec::new(),
                }),
            }),
            timing,
        }
    }

    /// 订阅状态：先推一次当前状态，之后每次变化推一次。
    pub(crate) fn subscribe(&self, listener: Listener) {
        let mut inner = self.shared.lock();
        if listener(inner.generation, &inner.event) {
            inner.listeners.push(listener);
        }
    }

    pub(crate) fn current(&self) -> Option<CurrentRuntime> {
        let inner = self.shared.lock();
        inner.session.as_ref().map(|session| CurrentRuntime {
            generation: inner.generation,
            endpoint_id: session.target.endpoint_id.clone(),
            client: session.link().client,
        })
    }

    /// （重新）连这台机器并握手。阻塞。
    pub(crate) fn connect(
        &self,
        dialer: &dyn Dialer,
        target: Target,
    ) -> Result<RuntimeInfo, SmeltError> {
        let generation = self.begin(&target);
        let route = match dialer.route(&target) {
            Ok(route) => route,
            Err(error) => return self.fail(generation, &target, error),
        };
        let link = route.link();
        let outcome = describe(&link, &target, self.timing.connect_timeout);
        self.finish(generation, target, route, link, outcome, true)
    }

    /// 配对并连上。阻塞。
    pub(crate) fn pair(
        &self,
        dialer: &dyn Dialer,
        target: Target,
        code: String,
        device_name: String,
    ) -> Result<RuntimeInfo, SmeltError> {
        let generation = self.begin(&target);
        let route = match dialer.route(&target) {
            Ok(route) => route,
            Err(error) => return self.fail(generation, &target, error),
        };
        let link = route.link();
        let outcome = link
            .client
            .call::<DevicePair>(
                &PairParams {
                    code,
                    name: device_name,
                },
                self.timing.pair_timeout,
            )
            .map_err(|e| control_error(e, SmeltErrorKind::InvalidPairing))
            .and_then(|_| describe(&link, &target, self.timing.connect_timeout));
        self.finish(generation, target, route, link, outcome, false)
    }

    pub(crate) fn disconnect(&self) {
        let mut inner = self.shared.lock();
        inner.retire();
        inner.publish(event(ConnectionState::Disconnected, None, None, None));
    }

    /// 立刻确认一次链路；重连中的立刻重试。
    pub(crate) fn verify(&self) {
        if let Some(session) = self.shared.lock().session.as_ref() {
            session.signals.probe_now();
        }
    }

    fn begin(&self, target: &Target) -> u64 {
        let mut inner = self.shared.lock();
        inner.retire();
        inner.publish(event(ConnectionState::Connecting, Some(target), None, None));
        inner.generation
    }

    fn fail(
        &self,
        generation: u64,
        target: &Target,
        error: SmeltError,
    ) -> Result<RuntimeInfo, SmeltError> {
        let mut inner = self.shared.lock();
        if inner.generation != generation {
            return Err(superseded());
        }
        inner.publish(event(
            ConnectionState::Disconnected,
            Some(target),
            None,
            Some(error.clone()),
        ));
        Err(error)
    }

    /// 收尾 connect / pair。`retry_offline`：离线时仍装上会话，转入后台重连。
    fn finish(
        &self,
        generation: u64,
        target: Target,
        route: Arc<dyn Route>,
        link: Link,
        outcome: Result<RuntimeInfo, SmeltError>,
        retry_offline: bool,
    ) -> Result<RuntimeInfo, SmeltError> {
        // 读路径会 block_on，放在锁外。
        let metrics = outcome.is_ok().then(|| (link.metrics)());
        let failures = match &outcome {
            Ok(_) => 0,
            Err(error) if retry_offline && error.kind == SmeltErrorKind::Offline => 1,
            Err(error) => return self.fail(generation, &target, error.clone()),
        };
        let mut inner = self.shared.lock();
        if inner.generation != generation {
            return Err(superseded());
        }
        let session = Arc::new(Session {
            target: target.clone(),
            route,
            link: Mutex::new(link),
            signals: Arc::new(Signals {
                state: Mutex::new(Signal::default()),
                wake: Condvar::new(),
            }),
        });
        inner.session = Some(Arc::clone(&session));
        match &outcome {
            Ok(_) => inner.publish(event(
                ConnectionState::Connected,
                Some(&target),
                metrics,
                None,
            )),
            Err(error) => inner.publish(event(
                ConnectionState::Reconnecting,
                Some(&target),
                None,
                Some(error.clone()),
            )),
        }
        let shared = Arc::clone(&self.shared);
        let timing = self.timing;
        let spawned = std::thread::Builder::new()
            .name("smelt-connection".to_string())
            .spawn(move || supervise(shared, session, timing, failures));
        if let Err(spawn_error) = spawned {
            inner.retire();
            let error = error(
                SmeltErrorKind::Other,
                format!("起不了连接线程：{spawn_error}"),
            );
            inner.publish(event(
                ConnectionState::Disconnected,
                Some(&target),
                None,
                Some(error.clone()),
            ));
            return Err(error);
        }
        outcome
    }
}

/// 会话线程：已连上时等链路的事件（不定时探活）；链路断了或确认失败就建新链路按退避重连。
/// 未配对、不兼容就停。
fn supervise(shared: Arc<Shared>, session: Arc<Session>, timing: Timing, mut failures: u32) {
    // 盯链路的守卫只在这条普通线程上建和放。
    let mut watch = (failures == 0).then(|| session.watch());
    loop {
        // 这一轮用不用新链路：重连不复用可能已经僵死的旧连接。
        let fresh = if failures == 0 {
            match session.signals.wait_connected() {
                Wake::Stop => return,
                // 回前台、下拉刷新：在现有链路上确认一下。
                Wake::Now | Wake::Timeout => false,
                Wake::Path => {
                    // 读路径会 block_on，放在锁外。
                    let metrics = (session.link().metrics)();
                    let mut inner = shared.lock();
                    if !inner.is_current(&session) {
                        return;
                    }
                    inner.publish(event(
                        ConnectionState::Connected,
                        Some(&session.target),
                        Some(metrics),
                        None,
                    ));
                    continue;
                }
                Wake::Closed => {
                    watch = None;
                    let mut inner = shared.lock();
                    if !inner.is_current(&session) {
                        return;
                    }
                    log::info!("到运行时的连接断了：{}", session.target.endpoint_id);
                    // 先告诉界面在重连：新链路拨号可能要十几秒。
                    inner.publish(event(
                        ConnectionState::Reconnecting,
                        Some(&session.target),
                        None,
                        Some(error(SmeltErrorKind::Offline, "到运行时的连接断了")),
                    ));
                    true
                }
            }
        } else {
            // 轮询例外：对端回来不会通知我们，连上之前收不到它的任何东西，只能定时重拨（回前台、
            // 下拉刷新经 `verify` 立刻重试并把退避归零）；周期 2 秒起逐次翻倍，上限 30 秒。
            match session.signals.wait_backoff(timing.backoff(failures)) {
                Wake::Stop => return,
                // 回前台：网络多半变了，退避从头算。
                Wake::Now => failures = failures.min(1),
                Wake::Path | Wake::Closed | Wake::Timeout => {}
            }
            true
        };
        let link = if fresh {
            session.route.link()
        } else {
            session.link()
        };
        let outcome =
            describe(&link, &session.target, timing.probe_timeout).map(|_| (link.metrics)());

        let mut inner = shared.lock();
        if !inner.is_current(&session) {
            return;
        }
        let target = Some(&session.target);
        match outcome {
            Ok(metrics) => {
                if fresh {
                    *session.link.lock().unwrap_or_else(|e| e.into_inner()) = link;
                    log::info!("运行时已重新连上：{}", session.target.endpoint_id);
                }
                failures = 0;
                inner.publish(event(
                    ConnectionState::Connected,
                    target,
                    Some(metrics),
                    None,
                ));
                drop(inner);
                if watch.is_none() {
                    watch = Some(session.watch());
                }
            }
            Err(error) if is_terminal(error.kind) => {
                log::warn!("运行时连接停止：{}", error.message);
                inner.retire();
                inner.publish(event(
                    ConnectionState::Disconnected,
                    target,
                    None,
                    Some(error),
                ));
                return;
            }
            Err(error) => {
                log::info!("连不上运行时（第 {} 次）：{}", failures + 1, error.message);
                failures = failures.saturating_add(1);
                inner.publish(event(
                    ConnectionState::Reconnecting,
                    target,
                    None,
                    Some(error),
                ));
                drop(inner);
                watch = None;
            }
        }
    }
}

/// 重试也不会好的失败：要用户重新配对或升级。
fn is_terminal(kind: SmeltErrorKind) -> bool {
    matches!(
        kind,
        SmeltErrorKind::Unpaired | SmeltErrorKind::Incompatible
    )
}

fn superseded() -> SmeltError {
    error(SmeltErrorKind::Cancelled, "已切换到别的连接请求")
}

fn describe(link: &Link, target: &Target, timeout: Duration) -> Result<RuntimeInfo, SmeltError> {
    let described = link
        .client
        .call::<SystemDescribe>(&EmptyParams::default(), timeout)
        .map_err(|e| control_error(e, SmeltErrorKind::Unpaired))?;
    runtime_info(target, described)
}

fn runtime_info(
    target: &Target,
    described: SystemDescribeResult,
) -> Result<RuntimeInfo, SmeltError> {
    let daemon = described.daemon;
    let runtime_id = daemon.runtime_id.ok_or_else(|| {
        error(
            SmeltErrorKind::Incompatible,
            "这个运行时太旧，没有 runtime_id，先升级它",
        )
    })?;
    Ok(RuntimeInfo {
        runtime_id,
        hostname: daemon.hostname,
        os: daemon.os,
        version: daemon.version,
        endpoint_id: target.endpoint_id.clone(),
        relay_url: target.relay_url.clone(),
    })
}

/// Control 调用失败归类。`denied`：PermissionDenied 算哪类——一般调用是未配对 / 被撤销，
/// `device.pair` 是配对码无效或过期。
fn control_error(error: ControlClientError, denied: SmeltErrorKind) -> SmeltError {
    match error {
        ControlClientError::Transport(message) => {
            crate::api::error::error(SmeltErrorKind::Offline, format!("连不上运行时：{message}"))
        }
        ControlClientError::Protocol(error) => {
            let kind = match error.code {
                ControlErrorCode::PermissionDenied => denied,
                ControlErrorCode::UnsupportedVersion | ControlErrorCode::MethodNotFound => {
                    SmeltErrorKind::Incompatible
                }
                _ => SmeltErrorKind::Other,
            };
            crate::api::error::error(kind, error.message)
        }
        ControlClientError::InvalidRequest(message)
        | ControlClientError::InvalidResponse(message) => {
            crate::api::error::error(SmeltErrorKind::Other, message)
        }
    }
}

#[cfg(test)]
mod tests;
