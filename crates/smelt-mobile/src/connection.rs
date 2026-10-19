//! 当前运行时的连接状态机。
//!
//! ```text
//!            connect / pair
//! Disconnected ─────────────> Connecting ──成功──> Connected <──┐
//!      ^                          │                   │ 探活失败  │ 重连成功
//!      │      未配对 / 不兼容 / pair 失败 / 其它失败    v           │
//!      ├──────────────────────────┤             Reconnecting ─────┘
//!      │                          └─connect 离线──>   │（退避 2s 起，翻倍，封顶 30s）
//!      └──── disconnect（任意状态）/ 探活未配对、不兼容 ──┘
//! ```
//!
//! 每次 connect / pair / disconnect 换一代（generation）：旧会话的探活线程被叫停，它晚到的
//! 结果、以及被取代的 connect / pair 的结果，都按代号丢弃。传输经 [`Dialer`] 注入，测试用假
//! 运行时。

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use smelt_model::control_api::DevicePair;
use smelt_model::remote_config::PairParams;
use smelt_runtime_client::RuntimeClient;
use smelt_runtime_client::protocol::control::{
    ControlClientError, ControlErrorCode, EmptyParams, SystemDescribe, SystemDescribeResult,
};

use crate::api::connection::{ConnectionEvent, ConnectionState, LinkMetrics, RuntimeInfo};
use crate::api::error::{SmeltError, SmeltErrorKind, error};

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

/// 一条到运行时的链路：发请求用的客户端，和读当前传输路径的方法。
#[derive(Clone)]
pub(crate) struct Link {
    pub client: RuntimeClient,
    /// 阻塞（iroh 连接器内部 `block_on`），只在普通线程上调。
    pub metrics: Arc<dyn Fn() -> LinkMetrics + Send + Sync>,
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
    /// 已连接时的探活间隔。
    pub interval: Duration,
    /// 探活请求的读写时限。
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
        interval: Duration::from_secs(5),
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

/// 后续阶段拿当前运行时用。
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct CurrentRuntime {
    /// 当前代号。和 [`Listener`] 收到的不同就说明运行时换了（或断开过），要丢掉旧状态。
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
            session.stop();
        }
    }

    fn is_current(&self, session: &Arc<Session>) -> bool {
        self.session
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, session))
    }
}

/// 一个当前运行时的会话：从 connect / pair 成功（或 connect 离线转入重连）到被取代或停下。
struct Session {
    target: Target,
    route: Arc<dyn Route>,
    link: Mutex<Link>,
    signal: Mutex<Signal>,
    wake: Condvar,
}

#[derive(Default)]
struct Signal {
    stop: bool,
    probe_now: bool,
}

enum Wake {
    Stop,
    Now,
    Timeout,
}

impl Session {
    fn link(&self) -> Link {
        self.link.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn stop(&self) {
        self.signal.lock().unwrap_or_else(|e| e.into_inner()).stop = true;
        self.wake.notify_all();
    }

    fn probe_now(&self) {
        self.signal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .probe_now = true;
        self.wake.notify_all();
    }

    fn wait(&self, delay: Duration) -> Wake {
        let signal = self.signal.lock().unwrap_or_else(|e| e.into_inner());
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

    #[allow(dead_code)]
    pub(crate) fn current(&self) -> Option<CurrentRuntime> {
        let inner = self.shared.lock();
        inner.session.as_ref().map(|session| CurrentRuntime {
            generation: inner.generation,
            endpoint_id: session.target.endpoint_id.clone(),
            client: session.link().client,
        })
    }

    /// 切换当前运行时并握手。阻塞。
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

    /// 配对并设为当前运行时。阻塞。
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

    /// 立刻探活 / 重试一次。
    pub(crate) fn verify(&self) {
        if let Some(session) = self.shared.lock().session.as_ref() {
            session.probe_now();
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
            signal: Mutex::new(Signal::default()),
            wake: Condvar::new(),
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
            .name("smelt-heartbeat".to_string())
            .spawn(move || heartbeat(shared, session, timing, failures));
        if let Err(spawn_error) = spawned {
            inner.retire();
            let error = error(
                SmeltErrorKind::Other,
                format!("起不了探活线程：{spawn_error}"),
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

/// 探活循环：已连接时按间隔探，失败后建新链路按退避重试。未配对、不兼容就停。
fn heartbeat(shared: Arc<Shared>, session: Arc<Session>, timing: Timing, mut failures: u32) {
    loop {
        let delay = if failures == 0 {
            timing.interval
        } else {
            timing.backoff(failures)
        };
        match session.wait(delay) {
            Wake::Stop => return,
            // 回前台：网络多半变了，退避从头算。
            Wake::Now => failures = failures.min(1),
            Wake::Timeout => {}
        }
        let link = if failures == 0 {
            session.link()
        } else {
            session.route.link()
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
                if failures > 0 {
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
                log::info!("探活失败（第 {} 次）：{}", failures + 1, error.message);
                failures = failures.saturating_add(1);
                inner.publish(event(
                    ConnectionState::Reconnecting,
                    target,
                    None,
                    Some(error),
                ));
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
