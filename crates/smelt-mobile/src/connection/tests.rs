//! 状态机单测：假运行时按 wire 格式在 `UnixStream::pair()` 上回包。

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use serde_json::{Value, json};
use smelt_runtime_client::protocol::control::{ControlError, ControlResponse};
use smelt_runtime_client::protocol::write_json_line;

use super::*;
use crate::api::connection::LinkPath;

const FAST: Timing = Timing {
    interval: Duration::from_millis(30),
    probe_timeout: Duration::from_millis(500),
    connect_timeout: Duration::from_millis(500),
    pair_timeout: Duration::from_millis(500),
    backoff_base: Duration::from_millis(20),
    backoff_max: Duration::from_millis(80),
};

/// 假运行时：在线与否、是否已配对可以随时改。
struct FakeRuntime {
    runtime_id: String,
    code: String,
    online: Mutex<bool>,
    paired: Mutex<bool>,
    /// 收到的 `device.pair` 设备名。
    paired_names: Mutex<Vec<String>>,
    describes: AtomicUsize,
    links: AtomicUsize,
}

impl FakeRuntime {
    fn new(runtime_id: &str) -> Arc<Self> {
        Arc::new(Self {
            runtime_id: runtime_id.to_string(),
            code: "K7QX-M2PD".to_string(),
            online: Mutex::new(true),
            paired: Mutex::new(false),
            paired_names: Mutex::new(Vec::new()),
            describes: AtomicUsize::new(0),
            links: AtomicUsize::new(0),
        })
    }

    fn paired(runtime_id: &str) -> Arc<Self> {
        let runtime = Self::new(runtime_id);
        runtime.set_paired(true);
        runtime
    }

    fn set_online(&self, online: bool) {
        *self.online.lock().unwrap() = online;
    }

    fn set_paired(&self, paired: bool) {
        *self.paired.lock().unwrap() = paired;
    }

    fn describes(&self) -> usize {
        self.describes.load(Ordering::SeqCst)
    }

    fn respond(&self, request: Value) -> Value {
        let request_id = request["request_id"].as_str().unwrap_or_default();
        let denied = |message: &str| {
            serde_json::to_value(ControlResponse::<()>::error(
                request_id,
                ControlError::new(ControlErrorCode::PermissionDenied, message),
            ))
            .unwrap()
        };
        match request["method"].as_str() {
            Some("device.pair") => {
                if request["params"]["code"] != self.code.as_str() {
                    return denied("配对码无效或已过期");
                }
                self.set_paired(true);
                self.paired_names
                    .lock()
                    .unwrap()
                    .push(request["params"]["name"].as_str().unwrap().to_string());
                serde_json::to_value(ControlResponse::ok(request_id, json!({}))).unwrap()
            }
            Some("system.describe") => {
                if !*self.paired.lock().unwrap() {
                    return denied("这台设备还没有配对");
                }
                self.describes.fetch_add(1, Ordering::SeqCst);
                serde_json::to_value(ControlResponse::ok(
                    request_id,
                    json!({
                        "control": {
                            "current_version": 1,
                            "min_supported_version": 1,
                            "max_supported_version": 1,
                            "methods": ["system.describe", "device.pair"],
                        },
                        "daemon": {
                            "version": "9.9.9",
                            "exe_mtime": 0,
                            "exe": null,
                            "pid": 1,
                            "started_at": 0,
                            "session_count": 0,
                            "agent_event_version": 1,
                            "runtime_id": self.runtime_id,
                            "os": "linux",
                            "hostname": "linux-box",
                        },
                    }),
                ))
                .unwrap()
            }
            other => panic!("假运行时不认识的方法：{other:?}"),
        }
    }
}

struct FakeRoute(Arc<FakeRuntime>);

impl Route for FakeRoute {
    fn link(&self) -> Link {
        self.0.links.fetch_add(1, Ordering::SeqCst);
        let runtime = Arc::clone(&self.0);
        Link {
            client: RuntimeClient::new(move || {
                if !*runtime.online.lock().unwrap() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "连不上运行时（超时）",
                    ));
                }
                let (client, server) = UnixStream::pair()?;
                let runtime = Arc::clone(&runtime);
                std::thread::spawn(move || {
                    let mut line = String::new();
                    if BufReader::new(&server).read_line(&mut line).is_err() {
                        return;
                    }
                    let response = runtime.respond(serde_json::from_str(&line).unwrap());
                    let _ = write_json_line(&mut &server, &response);
                });
                Ok(client)
            }),
            metrics: Arc::new(|| LinkMetrics {
                path: LinkPath::Unknown,
                rtt_ms: None,
            }),
        }
    }
}

/// 按 endpoint id 找假运行时。
struct FakeDialer(Vec<(String, Arc<FakeRuntime>)>);

impl Dialer for FakeDialer {
    fn route(&self, target: &Target) -> Result<Arc<dyn Route>, SmeltError> {
        self.0
            .iter()
            .find(|(endpoint, _)| *endpoint == target.endpoint_id)
            .map(|(_, runtime)| Arc::new(FakeRoute(Arc::clone(runtime))) as Arc<dyn Route>)
            .ok_or_else(|| error(SmeltErrorKind::InvalidPairing, "endpoint 不合法"))
    }
}

fn target(endpoint: &str) -> Target {
    Target::new(endpoint.to_string(), None)
}

/// 记下推出的全部事件。
fn record(hub: &ConnectionHub) -> Arc<Mutex<Vec<ConnectionEvent>>> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    hub.subscribe(Box::new(move |_, event| {
        sink.lock().unwrap().push(event.clone());
        true
    }));
    events
}

fn last(events: &Mutex<Vec<ConnectionEvent>>) -> ConnectionEvent {
    events.lock().unwrap().last().cloned().unwrap()
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "等不到：{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_state(events: &Mutex<Vec<ConnectionEvent>>, state: ConnectionState) -> ConnectionEvent {
    wait_until(&format!("{state:?}"), || last(events).state == state);
    last(events)
}

fn states(events: &Mutex<Vec<ConnectionEvent>>) -> Vec<ConnectionState> {
    events.lock().unwrap().iter().map(|e| e.state).collect()
}

#[test]
fn pair_redeems_the_code_then_describes_and_stays_connected() {
    let runtime = FakeRuntime::new("runtime-a");
    let dialer = FakeDialer(vec![("ep-a".into(), Arc::clone(&runtime))]);
    let hub = ConnectionHub::new(FAST);
    let events = record(&hub);

    let info = hub
        .pair(
            &dialer,
            target("ep-a"),
            "K7QX-M2PD".into(),
            "我的手机".into(),
        )
        .unwrap();

    assert_eq!(
        info,
        RuntimeInfo {
            runtime_id: "runtime-a".into(),
            hostname: Some("linux-box".into()),
            os: Some("linux".into()),
            version: "9.9.9".into(),
            endpoint_id: "ep-a".into(),
            relay_url: None,
        }
    );
    assert_eq!(*runtime.paired_names.lock().unwrap(), vec!["我的手机"]);
    assert_eq!(
        states(&events),
        vec![
            ConnectionState::Disconnected,
            ConnectionState::Connecting,
            ConnectionState::Connected,
        ]
    );
    let connected = last(&events);
    assert_eq!(connected.endpoint_id.as_deref(), Some("ep-a"));
    assert_eq!(connected.link.unwrap().path, LinkPath::Unknown);
    assert_eq!(hub.current().unwrap().endpoint_id, "ep-a");

    // 心跳在跑。
    let before = runtime.describes();
    wait_until("探活", || runtime.describes() >= before + 2);
    hub.disconnect();
}

#[test]
fn pair_with_a_wrong_code_is_an_invalid_pairing_and_stops() {
    let runtime = FakeRuntime::new("runtime-a");
    let dialer = FakeDialer(vec![("ep-a".into(), Arc::clone(&runtime))]);
    let hub = ConnectionHub::new(FAST);
    let events = record(&hub);

    let error = hub
        .pair(&dialer, target("ep-a"), "WRONG".into(), "手机".into())
        .unwrap_err();

    assert_eq!(error.kind, SmeltErrorKind::InvalidPairing);
    let stopped = last(&events);
    assert_eq!(stopped.state, ConnectionState::Disconnected);
    assert_eq!(stopped.error.unwrap().kind, SmeltErrorKind::InvalidPairing);
    assert!(hub.current().is_none());
}

#[test]
fn pair_while_offline_does_not_retry() {
    let runtime = FakeRuntime::new("runtime-a");
    runtime.set_online(false);
    let dialer = FakeDialer(vec![("ep-a".into(), Arc::clone(&runtime))]);
    let hub = ConnectionHub::new(FAST);
    let events = record(&hub);

    let error = hub
        .pair(&dialer, target("ep-a"), "K7QX-M2PD".into(), "手机".into())
        .unwrap_err();

    assert_eq!(error.kind, SmeltErrorKind::Offline);
    assert_eq!(last(&events).state, ConnectionState::Disconnected);
    assert!(hub.current().is_none());
}

#[test]
fn connect_to_an_unpaired_runtime_asks_for_pairing() {
    let runtime = FakeRuntime::new("runtime-a");
    let dialer = FakeDialer(vec![("ep-a".into(), Arc::clone(&runtime))]);
    let hub = ConnectionHub::new(FAST);
    let events = record(&hub);

    let error = hub.connect(&dialer, target("ep-a")).unwrap_err();

    assert_eq!(error.kind, SmeltErrorKind::Unpaired);
    let stopped = last(&events);
    assert_eq!(stopped.state, ConnectionState::Disconnected);
    assert_eq!(stopped.endpoint_id.as_deref(), Some("ep-a"));
    assert_eq!(stopped.error.unwrap().kind, SmeltErrorKind::Unpaired);
    assert!(hub.current().is_none());
}

#[test]
fn connect_with_a_bad_target_fails_without_a_session() {
    let hub = ConnectionHub::new(FAST);
    let events = record(&hub);

    let error = hub
        .connect(&FakeDialer(Vec::new()), target("nope"))
        .unwrap_err();

    assert_eq!(error.kind, SmeltErrorKind::InvalidPairing);
    assert_eq!(last(&events).state, ConnectionState::Disconnected);
    assert!(hub.current().is_none());
}

#[test]
fn heartbeat_reconnects_on_a_fresh_link_after_going_offline() {
    let runtime = FakeRuntime::paired("runtime-a");
    let dialer = FakeDialer(vec![("ep-a".into(), Arc::clone(&runtime))]);
    let hub = ConnectionHub::new(FAST);
    let events = record(&hub);
    hub.connect(&dialer, target("ep-a")).unwrap();
    let generation = hub.current().unwrap().generation;
    let links = runtime.links.load(Ordering::SeqCst);

    runtime.set_online(false);
    let reconnecting = wait_state(&events, ConnectionState::Reconnecting);
    assert_eq!(reconnecting.error.unwrap().kind, SmeltErrorKind::Offline);
    assert!(reconnecting.link.is_none());
    // 重连中仍有当前运行时，代号不变。
    assert_eq!(hub.current().unwrap().generation, generation);

    runtime.set_online(true);
    let connected = wait_state(&events, ConnectionState::Connected);
    assert!(connected.error.is_none());
    // 重连用的是新链路，不复用可能僵死的旧连接。
    assert!(runtime.links.load(Ordering::SeqCst) > links);
    assert_eq!(hub.current().unwrap().generation, generation);
    hub.disconnect();
}

#[test]
fn connect_while_offline_returns_offline_but_keeps_retrying() {
    let runtime = FakeRuntime::paired("runtime-a");
    runtime.set_online(false);
    let dialer = FakeDialer(vec![("ep-a".into(), Arc::clone(&runtime))]);
    let hub = ConnectionHub::new(FAST);
    let events = record(&hub);

    let error = hub.connect(&dialer, target("ep-a")).unwrap_err();
    assert_eq!(error.kind, SmeltErrorKind::Offline);
    assert_eq!(last(&events).state, ConnectionState::Reconnecting);

    runtime.set_online(true);
    wait_state(&events, ConnectionState::Connected);
    hub.disconnect();
}

#[test]
fn revocation_during_heartbeat_stops_with_unpaired() {
    let runtime = FakeRuntime::paired("runtime-a");
    let dialer = FakeDialer(vec![("ep-a".into(), Arc::clone(&runtime))]);
    let hub = ConnectionHub::new(FAST);
    let events = record(&hub);
    hub.connect(&dialer, target("ep-a")).unwrap();
    let generation = hub.current().unwrap().generation;
    let generations = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&generations);
    hub.subscribe(Box::new(move |generation, _| {
        seen.lock().unwrap().push(generation);
        true
    }));

    runtime.set_paired(false);
    let stopped = wait_state(&events, ConnectionState::Disconnected);

    assert_eq!(stopped.error.unwrap().kind, SmeltErrorKind::Unpaired);
    assert_eq!(stopped.endpoint_id.as_deref(), Some("ep-a"));
    assert!(hub.current().is_none());
    // 会话结束换了代，后续阶段据此丢掉旧状态。
    assert!(generations.lock().unwrap().last().unwrap() > &generation);
    // 停下后不再探活。
    let attempts = runtime.links.load(Ordering::SeqCst);
    std::thread::sleep(FAST.interval * 4);
    assert_eq!(runtime.links.load(Ordering::SeqCst), attempts);
}

#[test]
fn disconnect_stops_probing() {
    let runtime = FakeRuntime::paired("runtime-a");
    let dialer = FakeDialer(vec![("ep-a".into(), Arc::clone(&runtime))]);
    let hub = ConnectionHub::new(FAST);
    let events = record(&hub);
    hub.connect(&dialer, target("ep-a")).unwrap();

    hub.disconnect();

    assert_eq!(
        last(&events),
        event(ConnectionState::Disconnected, None, None, None)
    );
    assert!(hub.current().is_none());
    // 允许一次已在路上的探活落地，之后不再增加。
    std::thread::sleep(FAST.interval * 2);
    let describes = runtime.describes();
    std::thread::sleep(FAST.interval * 4);
    assert_eq!(runtime.describes(), describes);
    assert_eq!(last(&events).state, ConnectionState::Disconnected);
}

#[test]
fn switching_runtimes_retires_the_old_session() {
    let a = FakeRuntime::paired("runtime-a");
    let b = FakeRuntime::paired("runtime-b");
    let dialer = FakeDialer(vec![
        ("ep-a".into(), Arc::clone(&a)),
        ("ep-b".into(), Arc::clone(&b)),
    ]);
    let hub = ConnectionHub::new(FAST);
    let events = record(&hub);
    hub.connect(&dialer, target("ep-a")).unwrap();
    let first = hub.current().unwrap().generation;

    let info = hub.connect(&dialer, target("ep-b")).unwrap();

    assert_eq!(info.runtime_id, "runtime-b");
    let current = hub.current().unwrap();
    assert_eq!(current.endpoint_id, "ep-b");
    assert!(current.generation > first);
    // 旧运行时的探活停了，且它的结果不会再改状态。
    std::thread::sleep(FAST.interval * 2);
    let a_describes = a.describes();
    a.set_online(false);
    std::thread::sleep(FAST.interval * 4);
    assert_eq!(a.describes(), a_describes);
    let now = last(&events);
    assert_eq!(now.state, ConnectionState::Connected);
    assert_eq!(now.endpoint_id.as_deref(), Some("ep-b"));
    hub.disconnect();
}

#[test]
fn verify_probes_immediately() {
    let runtime = FakeRuntime::paired("runtime-a");
    let dialer = FakeDialer(vec![("ep-a".into(), Arc::clone(&runtime))]);
    let hub = ConnectionHub::new(Timing {
        interval: Duration::from_secs(3600),
        ..FAST
    });
    hub.connect(&dialer, target("ep-a")).unwrap();
    let describes = runtime.describes();

    hub.verify();

    wait_until("立刻探活", || runtime.describes() > describes);
    hub.disconnect();
}

#[test]
fn verify_retries_a_backed_off_reconnect_right_away() {
    let runtime = FakeRuntime::paired("runtime-a");
    runtime.set_online(false);
    let dialer = FakeDialer(vec![("ep-a".into(), Arc::clone(&runtime))]);
    let hub = ConnectionHub::new(Timing {
        backoff_base: Duration::from_secs(3600),
        backoff_max: Duration::from_secs(3600),
        ..FAST
    });
    let events = record(&hub);
    hub.connect(&dialer, target("ep-a")).unwrap_err();
    runtime.set_online(true);

    hub.verify();

    wait_state(&events, ConnectionState::Connected);
    hub.disconnect();
}

#[test]
fn subscribe_pushes_the_current_state_and_drops_closed_listeners() {
    let runtime = FakeRuntime::paired("runtime-a");
    let dialer = FakeDialer(vec![("ep-a".into(), Arc::clone(&runtime))]);
    let hub = ConnectionHub::new(FAST);
    hub.connect(&dialer, target("ep-a")).unwrap();

    let events = record(&hub);
    assert_eq!(states(&events), vec![ConnectionState::Connected]);

    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    hub.subscribe(Box::new(move |_, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        false
    }));
    hub.disconnect();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(last(&events).state, ConnectionState::Disconnected);
}

#[test]
fn backoff_doubles_from_the_base_up_to_the_cap() {
    let timing = Timing::PRODUCTION;
    let delays: Vec<u64> = (1..=6).map(|n| timing.backoff(n).as_secs()).collect();
    assert_eq!(delays, vec![2, 4, 8, 16, 30, 30]);
    assert_eq!(timing.backoff(u32::MAX).as_secs(), 30);
}

#[test]
fn target_treats_a_blank_relay_as_none() {
    assert_eq!(
        Target::new(" ep ".into(), Some("  ".into())),
        Target {
            endpoint_id: "ep".into(),
            relay_url: None
        }
    );
}
