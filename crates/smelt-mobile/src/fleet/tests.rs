//! 机队单测：假传输按 endpoint 找假运行时（`RuntimeStream::pair()` 上按线格式回包），本机库和
//! 缓存用内存库。

use std::io::{BufRead, BufReader};
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use smelt_iroh::RelaySettings;
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode, ControlResponse};
use smelt_runtime_client::protocol::write_json_line;
use smelt_runtime_client::{RuntimeClient, RuntimeStream};

use super::*;
use crate::api::draft::MessageDraft;
use crate::api::fleet::{LinkMetrics, LinkPath};
use crate::api::pass::Pass;
use crate::connection::{Link, Route};
use crate::iroh_dialer::{Shape, shape_for};

const CODE: &str = "K7QX-M2PD";

fn fast_backoff(_attempt: u32) -> Duration {
    Duration::from_millis(50)
}

const FAST: Timings = Timings {
    connection: crate::connection::Timing {
        interval: Duration::from_millis(40),
        probe_timeout: Duration::from_millis(500),
        connect_timeout: Duration::from_millis(500),
        pair_timeout: Duration::from_millis(500),
        backoff_base: Duration::from_millis(20),
        backoff_max: Duration::from_millis(80),
    },
    catalog: crate::catalog::Timing {
        push_delay: Duration::from_millis(20),
        save_delay: Duration::from_millis(40),
        stable_after: Duration::from_secs(5),
        backoff: fast_backoff,
        sync_wait: Duration::from_millis(500),
        call_timeout: Duration::from_millis(500),
        slow_call_timeout: Duration::from_millis(500),
    },
    conversation: crate::conversation::Timing {
        backoff_base: Duration::from_millis(20),
        backoff_max: Duration::from_millis(80),
        emit_interval: Duration::from_millis(5),
        save_delay: Duration::from_millis(20),
    },
    terminal: crate::terminal::Timing {
        header_timeout: Duration::from_secs(1),
        write_timeout: Duration::from_secs(1),
        input_timeout: Duration::from_secs(1),
        probe_timeout: Duration::from_secs(1),
        backoff_base: Duration::from_millis(20),
        backoff_max: Duration::from_millis(80),
    },
    pass: crate::pass::Timing {
        retry_base: Duration::from_millis(20),
        retry_max: Duration::from_millis(80),
    },
};

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "等不到：{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn endpoint() -> String {
    iroh::SecretKey::generate().public().to_string()
}

// ---------------------------------------------------------------------------
// 假运行时与假传输
// ---------------------------------------------------------------------------

struct FakeRuntime {
    runtime_id: String,
    endpoint_id: String,
    hostname: String,
    paired: AtomicBool,
    online: AtomicBool,
    describes: AtomicUsize,
    links: AtomicUsize,
    /// 会话目录的订阅连接，挂着不回。
    subscriptions: Mutex<Vec<RuntimeStream>>,
}

impl FakeRuntime {
    fn new(runtime_id: &str, hostname: &str) -> Arc<Self> {
        Arc::new(Self {
            runtime_id: runtime_id.to_string(),
            endpoint_id: endpoint(),
            hostname: hostname.to_string(),
            paired: AtomicBool::new(false),
            online: AtomicBool::new(true),
            describes: AtomicUsize::new(0),
            links: AtomicUsize::new(0),
            subscriptions: Mutex::new(Vec::new()),
        })
    }

    fn info(&self, relay_url: Option<&str>) -> RuntimeInfo {
        RuntimeInfo {
            runtime_id: self.runtime_id.clone(),
            hostname: Some(self.hostname.clone()),
            os: Some("linux".to_string()),
            version: "9.9.9".to_string(),
            endpoint_id: self.endpoint_id.clone(),
            relay_url: relay_url.map(str::to_string),
        }
    }

    fn target(&self, relay_url: Option<&str>) -> Target {
        Target::new(self.endpoint_id.clone(), relay_url.map(str::to_string))
    }

    fn client(self: &Arc<Self>) -> RuntimeClient {
        let runtime = Arc::clone(self);
        RuntimeClient::new(move || {
            if !runtime.online.load(Ordering::SeqCst) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "连不上运行时（超时）",
                ));
            }
            let (client, server) = RuntimeStream::pair()?;
            let runtime = Arc::clone(&runtime);
            std::thread::spawn(move || runtime.serve(server));
            Ok(client)
        })
    }

    fn serve(&self, server: RuntimeStream) {
        let mut line = String::new();
        if BufReader::new(&server).read_line(&mut line).is_err() || line.is_empty() {
            return;
        }
        let request: Value = serde_json::from_str(&line).unwrap();
        match request["op"].as_str() {
            Some("control") => {
                let response = self.control(&request);
                let _ = write_json_line(&mut &server, &response);
            }
            Some("event_subscribe") => self.subscriptions.lock().unwrap().push(server),
            // 别的请求这里用不到：读完就关。
            _ => {}
        }
    }

    fn control(&self, request: &Value) -> Value {
        let id = request["request_id"].as_str().unwrap();
        let denied = |message: &str| {
            serde_json::to_value(ControlResponse::<()>::error(
                id,
                ControlError::new(ControlErrorCode::PermissionDenied, message),
            ))
            .unwrap()
        };
        match request["method"].as_str() {
            Some("device.pair") => {
                if request["params"]["code"] != CODE {
                    return denied("配对码无效或已过期");
                }
                self.paired.store(true, Ordering::SeqCst);
                serde_json::to_value(ControlResponse::ok(id, json!({}))).unwrap()
            }
            Some("system.describe") => {
                if !self.paired.load(Ordering::SeqCst) {
                    return denied("这台设备还没有配对");
                }
                self.describes.fetch_add(1, Ordering::SeqCst);
                serde_json::to_value(ControlResponse::ok(
                    id,
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
                            "hostname": self.hostname,
                        },
                    }),
                ))
                .unwrap()
            }
            Some("session.agent_bindings") => {
                serde_json::to_value(ControlResponse::ok(id, json!({"bindings": {}}))).unwrap()
            }
            other => serde_json::to_value(ControlResponse::<()>::error(
                id,
                ControlError::new(ControlErrorCode::MethodNotFound, other.unwrap_or("")),
            ))
            .unwrap(),
        }
    }
}

struct FakeRoute(Arc<FakeRuntime>);

impl Route for FakeRoute {
    fn link(&self) -> Link {
        self.0.links.fetch_add(1, Ordering::SeqCst);
        Link {
            client: self.0.client(),
            metrics: Arc::new(|| LinkMetrics {
                path: LinkPath::Lan,
                rtt_ms: Some(3),
            }),
        }
    }
}

/// 按 endpoint 找假运行时；形态按真实的 [`shape_for`] 算，记下每次调整。
struct FakeTransport {
    runtimes: Mutex<Vec<Arc<FakeRuntime>>>,
    shape: Mutex<Shape>,
    changes: AtomicUsize,
}

impl FakeTransport {
    fn new(runtimes: &[&Arc<FakeRuntime>]) -> Arc<Self> {
        Arc::new(Self {
            runtimes: Mutex::new(runtimes.iter().map(|r| Arc::clone(r)).collect()),
            shape: Mutex::new(Shape::Preset),
            changes: AtomicUsize::new(0),
        })
    }

    fn shape(&self) -> Shape {
        self.shape.lock().unwrap().clone()
    }
}

impl Dialer for FakeTransport {
    fn route(&self, target: &Target) -> Result<Arc<dyn Route>, SmeltError> {
        self.runtimes
            .lock()
            .unwrap()
            .iter()
            .find(|runtime| runtime.endpoint_id == target.endpoint_id)
            .map(|runtime| Arc::new(FakeRoute(Arc::clone(runtime))) as Arc<dyn Route>)
            .ok_or_else(|| error(SmeltErrorKind::InvalidPairing, "endpoint 不合法"))
    }
}

impl Transport for FakeTransport {
    fn configure(&self, relays: &[Option<String>]) -> Result<bool, SmeltError> {
        let relays: Vec<Option<RelaySettings>> = relays
            .iter()
            .map(|relay| relay.as_deref().map(|r| RelaySettings::parse(r).unwrap()))
            .collect();
        let shape = shape_for(&relays);
        let mut current = self.shape.lock().unwrap();
        if *current == shape {
            return Ok(false);
        }
        *current = shape;
        self.changes.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

struct Harness {
    fleet: Fleet,
    store: Arc<Store>,
    cache: Arc<Cache>,
    transport: Arc<FakeTransport>,
    views: Arc<Mutex<Vec<FleetView>>>,
}

/// `registered`：启动前已登记（且运行时那边已配对）的机器和它的 relay。
fn harness(
    registered: &[(&Arc<FakeRuntime>, Option<&str>)],
    others: &[&Arc<FakeRuntime>],
) -> Harness {
    let store = Arc::new(Store::in_memory());
    let cache = Arc::new(Cache::in_memory());
    let mut all: Vec<&Arc<FakeRuntime>> = registered.iter().map(|(runtime, _)| *runtime).collect();
    all.extend(others);
    let transport = FakeTransport::new(&all);
    for (i, (runtime, relay)) in registered.iter().enumerate() {
        runtime.paired.store(true, Ordering::SeqCst);
        store
            .record_pairing(&runtime.info(*relay), i as i64)
            .unwrap();
    }
    let fleet = Fleet::new(FAST);
    let views = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&views);
    fleet.subscribe(Box::new(move |view| {
        sink.lock().unwrap().push(view.clone());
        true
    }));
    fleet
        .start(Env {
            data_dir: std::env::temp_dir(),
            device_id: "device".to_string(),
            store: Arc::clone(&store),
            cache: Some(Arc::clone(&cache)),
            transport: transport.clone(),
        })
        .unwrap();
    Harness {
        fleet,
        store,
        cache,
        transport,
        views,
    }
}

impl Harness {
    fn view(&self) -> FleetView {
        self.views.lock().unwrap().last().cloned().unwrap()
    }

    fn state(&self, runtime_id: &str) -> Option<ConnectionState> {
        self.view()
            .runtimes
            .iter()
            .find(|runtime| runtime.record.runtime_id == runtime_id)
            .map(|runtime| runtime.status.state)
    }

    fn wait_state(&self, runtime_id: &str, state: ConnectionState) {
        wait_until(&format!("{runtime_id} {state:?}"), || {
            self.state(runtime_id) == Some(state)
        });
    }

    fn ids(&self) -> Vec<String> {
        self.view()
            .runtimes
            .iter()
            .map(|runtime| runtime.record.runtime_id.clone())
            .collect()
    }

    fn generation(&self, runtime_id: &str) -> u64 {
        self.fleet
            .node(runtime_id)
            .unwrap()
            .hub
            .current()
            .unwrap()
            .generation
    }

    fn pair(
        &self,
        runtime: &FakeRuntime,
        relay: Option<&str>,
    ) -> Result<RuntimeRecord, SmeltError> {
        self.fleet.pair(
            runtime.target(relay),
            CODE.to_string(),
            "我的手机".to_string(),
        )
    }
}

// ---------------------------------------------------------------------------
// 启动与配对
// ---------------------------------------------------------------------------

#[test]
fn start_connects_every_registered_runtime() {
    let a = FakeRuntime::new("ra", "mac-a");
    let b = FakeRuntime::new("rb", "mac-b");
    b.online.store(false, Ordering::SeqCst);
    let h = harness(&[(&a, None), (&b, None)], &[]);
    assert_eq!(h.ids(), ["ra", "rb"]);
    h.wait_state("ra", ConnectionState::Connected);
    h.wait_state("rb", ConnectionState::Reconnecting);
    let a_status = &h.view().runtimes[0].status;
    assert_eq!(a_status.link.unwrap().path, LinkPath::Lan);

    // 离线的那台自己退避重连，不影响另一台。
    b.online.store(true, Ordering::SeqCst);
    h.wait_state("rb", ConnectionState::Connected);
    assert_eq!(h.state("ra"), Some(ConnectionState::Connected));
}

#[test]
fn pairing_a_new_runtime_keeps_the_others_connected() {
    let a = FakeRuntime::new("ra", "mac-a");
    let b = FakeRuntime::new("rb", "mac-b");
    let h = harness(&[(&a, None)], &[&b]);
    h.wait_state("ra", ConnectionState::Connected);
    let generation = h.generation("ra");
    let seen = h.views.lock().unwrap().len();

    let record = h.pair(&b, None).unwrap();

    assert_eq!(record.runtime_id, "rb");
    assert_eq!(record.name, "mac-b");
    assert_eq!((record.accent, record.position), (1, 1));
    assert_eq!(h.ids(), ["ra", "rb"]);
    h.wait_state("rb", ConnectionState::Connected);
    assert_eq!(h.generation("ra"), generation, "别的机器没重连");
    let views = h.views.lock().unwrap();
    assert!(
        views[seen..]
            .iter()
            .all(|view| view.runtimes[0].status.state == ConnectionState::Connected),
        "配对期间别的机器一直连着"
    );
    assert_eq!(h.transport.changes.load(Ordering::SeqCst), 0);
}

#[test]
fn pairing_a_known_endpoint_updates_the_entry_and_keeps_the_node() {
    let a = FakeRuntime::new("ra", "mac-a");
    let h = harness(&[(&a, None)], &[]);
    h.wait_state("ra", ConnectionState::Connected);
    h.fleet.rename("ra", "工作机").unwrap();
    h.fleet.set_accent("ra", 6).unwrap();
    let node = h.fleet.node("ra").unwrap();

    // 被撤销后重新配对。
    a.paired.store(false, Ordering::SeqCst);
    h.wait_state("ra", ConnectionState::Disconnected);
    let record = h.pair(&a, None).unwrap();

    assert_eq!(h.store.runtimes().unwrap().len(), 1, "更新而不是新增");
    assert_eq!((record.name.as_str(), record.accent), ("工作机", 6));
    assert!(
        Arc::ptr_eq(&node, &h.fleet.node("ra").unwrap()),
        "节点还是原来那个"
    );
    h.wait_state("ra", ConnectionState::Connected);
}

/// 机器重装后换了 `runtime_id`、endpoint 没变：重新配对替换旧登记，按 endpoint 存的旧缓存
/// 一并删掉，新登记不会把重装前的会话当成自己的。
#[test]
fn re_pairing_a_reinstalled_runtime_drops_the_old_cache() {
    let a = FakeRuntime::new("ra", "mac-a");
    let b = FakeRuntime::new("rb", "mac-b");
    let h = harness(&[(&a, None), (&b, None)], &[]);
    h.wait_state("ra", ConnectionState::Connected);
    h.cache
        .save_session_list(&a.endpoint_id, &vec!["重装前的会话".to_string()]);
    h.cache.save_snapshot(&a.endpoint_id, "s1", &1);

    let reinstalled = Arc::new(FakeRuntime {
        runtime_id: "ra2".to_string(),
        endpoint_id: a.endpoint_id.clone(),
        hostname: "mac-a".to_string(),
        paired: AtomicBool::new(false),
        online: AtomicBool::new(true),
        describes: AtomicUsize::new(0),
        links: AtomicUsize::new(0),
        subscriptions: Mutex::new(Vec::new()),
    });
    {
        let mut runtimes = h.transport.runtimes.lock().unwrap();
        runtimes.retain(|runtime| runtime.runtime_id != "ra");
        runtimes.push(Arc::clone(&reinstalled));
    }
    let record = h.pair(&reinstalled, None).unwrap();

    assert_eq!(record.runtime_id, "ra2");
    assert!(h.fleet.node("ra").is_err(), "旧登记撤掉了");
    assert!(
        h.cache
            .load_session_list::<Vec<String>>(&a.endpoint_id)
            .is_none()
    );
    assert!(h.cache.load_snapshot::<i32>(&a.endpoint_id, "s1").is_none());
    let positions: Vec<u32> = h
        .view()
        .runtimes
        .iter()
        .map(|runtime| runtime.record.position)
        .collect();
    assert_eq!(positions, [0, 1], "序号连续，不重号");
}

#[test]
fn a_failed_pairing_leaves_nothing_behind() {
    let a = FakeRuntime::new("ra", "mac-a");
    let b = FakeRuntime::new("rb", "mac-b");
    let h = harness(&[(&a, Some("relay.example"))], &[&b]);
    h.wait_state("ra", ConnectionState::Connected);
    assert_eq!(
        h.transport.shape(),
        Shape::Relay(RelaySettings::parse("relay.example").unwrap())
    );

    let failure = h
        .fleet
        .pair(b.target(None), "WRONG".to_string(), "手机".to_string())
        .unwrap_err();

    assert_eq!(failure.kind, SmeltErrorKind::InvalidPairing);
    assert_eq!(h.ids(), ["ra"]);
    assert!(h.fleet.node("rb").is_err());
    assert_eq!(
        h.transport.shape(),
        Shape::Relay(RelaySettings::parse("relay.example").unwrap()),
        "恢复原形态"
    );
    h.wait_state("ra", ConnectionState::Connected);
}

#[test]
fn a_shape_change_reconnects_every_runtime() {
    let a = FakeRuntime::new("ra", "mac-a");
    let b = FakeRuntime::new("rb", "mac-b");
    let h = harness(&[(&a, Some("relay.example"))], &[&b]);
    h.wait_state("ra", ConnectionState::Connected);
    let generation = h.generation("ra");

    // 一台用自建 relay、一台不指定：换成 n0 预设，所有节点重新连。
    h.pair(&b, None).unwrap();
    assert_eq!(h.transport.shape(), Shape::Preset);
    wait_until("ra 重新连过", || {
        h.fleet
            .node("ra")
            .unwrap()
            .hub
            .current()
            .is_some_and(|current| current.generation > generation)
    });
    h.wait_state("ra", ConnectionState::Connected);
    h.wait_state("rb", ConnectionState::Connected);

    // 忘掉不指定 relay 的那台：回到只用自建 relay。
    let generation = h.generation("ra");
    h.fleet.forget("rb").unwrap();
    assert_eq!(
        h.transport.shape(),
        Shape::Relay(RelaySettings::parse("relay.example").unwrap())
    );
    wait_until("ra 又重新连过", || {
        h.fleet
            .node("ra")
            .unwrap()
            .hub
            .current()
            .is_some_and(|current| current.generation > generation)
    });
    h.wait_state("ra", ConnectionState::Connected);
}

#[test]
fn connecting_refreshes_the_description() {
    let a = FakeRuntime::new("ra", "renamed-host");
    let store_info = RuntimeInfo {
        hostname: Some("old-host".to_string()),
        version: "0.1.0".to_string(),
        ..a.info(None)
    };
    let store = Arc::new(Store::in_memory());
    store.record_pairing(&store_info, 1).unwrap();
    a.paired.store(true, Ordering::SeqCst);
    let fleet = Fleet::new(FAST);
    let views = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&views);
    fleet.subscribe(Box::new(move |view| {
        sink.lock().unwrap().push(view.clone());
        true
    }));
    fleet
        .start(Env {
            data_dir: std::env::temp_dir(),
            device_id: "device".to_string(),
            store: Arc::clone(&store),
            cache: None,
            transport: FakeTransport::new(&[&a]),
        })
        .unwrap();
    wait_until("描述刷新", || {
        views.lock().unwrap().last().is_some_and(|view| {
            let record = &view.runtimes[0].record;
            record.hostname.as_deref() == Some("renamed-host") && record.version == "9.9.9"
        })
    });
    assert_eq!(
        store.runtimes().unwrap()[0].name,
        "old-host",
        "名字不跟着变"
    );
}

// ---------------------------------------------------------------------------
// 登记的增删改
// ---------------------------------------------------------------------------

#[test]
fn forgetting_cleans_up_everything_of_that_runtime() {
    let a = FakeRuntime::new("ra", "mac-a");
    let b = FakeRuntime::new("rb", "mac-b");
    let h = harness(&[(&a, None), (&b, None)], &[]);
    h.wait_state("ra", ConnectionState::Connected);
    h.wait_state("rb", ConnectionState::Connected);
    let catalog_events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&catalog_events);
    h.fleet.subscribe_catalog(Box::new(move |event| {
        sink.lock().unwrap().push(event.clone());
        true
    }));
    let passes: Arc<Mutex<Vec<Pass>>> = Arc::new(Mutex::new(Vec::new()));
    let pass_sink = Arc::clone(&passes);
    h.fleet.subscribe_pass(Box::new(move |pass| {
        pass_sink.lock().unwrap().push(pass.clone());
        true
    }));
    let draft = MessageDraft {
        content: "没发完".to_string(),
        images: Vec::new(),
        request_id: None,
    };
    h.store.save_draft("ra", "s1", &draft, 1).unwrap();
    h.store.save_draft("rb", "s1", &draft, 1).unwrap();
    h.cache
        .save_session_list(&a.endpoint_id, &vec!["x".to_string()]);
    h.cache.save_snapshot(&a.endpoint_id, "s1", &1);
    let node = h.fleet.node("ra").unwrap();

    h.fleet.forget("ra").unwrap();

    assert_eq!(h.ids(), ["rb"]);
    assert_eq!(h.view().runtimes[0].record.position, 0);
    assert!(h.fleet.node("ra").is_err());
    assert!(h.store.load_draft("ra", "s1").unwrap().is_none());
    assert!(
        h.store.load_draft("rb", "s1").unwrap().is_some(),
        "别的机器的草稿还在"
    );
    assert!(
        h.cache
            .load_session_list::<Vec<String>>(&a.endpoint_id)
            .is_none()
    );
    assert!(h.cache.load_snapshot::<i32>(&a.endpoint_id, "s1").is_none());
    assert!(node.hub.current().is_none(), "断开了");
    let removed = catalog_events
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|event| event.runtime_id == "ra")
        .cloned()
        .unwrap();
    let list = removed.sessions.unwrap();
    assert!(
        list.sessions.is_empty() && list.cached,
        "告诉目录订阅者这台的列表空了"
    );
    let stations: Vec<String> = passes
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .stations
        .iter()
        .map(|station| station.runtime_id.clone())
        .collect();
    assert_eq!(stations, ["rb"]);

    // 停下后不再探活，也不再开订阅。
    std::thread::sleep(FAST.connection.interval * 4);
    let after = a.describes.load(Ordering::SeqCst);
    std::thread::sleep(FAST.connection.interval * 4);
    assert_eq!(a.describes.load(Ordering::SeqCst), after);
    assert!(h.fleet.forget("ra").is_err());
}

#[test]
fn rename_accent_and_order_show_up_in_the_view() {
    let a = FakeRuntime::new("ra", "mac-a");
    let b = FakeRuntime::new("rb", "mac-b");
    let c = FakeRuntime::new("rc", "mac-c");
    let h = harness(&[(&a, None), (&b, None), (&c, None)], &[]);
    h.fleet.rename("rb", "书房").unwrap();
    h.fleet.set_accent("rb", 7).unwrap();
    h.fleet.move_runtime("rc", 0).unwrap();
    assert_eq!(h.ids(), ["rc", "ra", "rb"]);
    let rb = h.view().runtimes[2].record.clone();
    assert_eq!((rb.name.as_str(), rb.accent, rb.position), ("书房", 7, 2));
    assert!(h.fleet.set_accent("rb", 8).is_err());
    assert!(h.fleet.rename("nope", "x").is_err());
}

#[test]
fn import_registers_new_runtimes_and_connects_them() {
    let a = FakeRuntime::new("ra", "mac-a");
    let b = FakeRuntime::new("rb", "mac-b");
    b.paired.store(true, Ordering::SeqCst);
    let h = harness(&[(&a, None)], &[&b]);

    let imported = h
        .fleet
        .import(vec![
            RuntimeImport {
                runtime_id: "ra".to_string(),
                endpoint_id: a.endpoint_id.clone(),
                relay_url: None,
                name: "已登记".to_string(),
            },
            RuntimeImport {
                runtime_id: "bad".to_string(),
                endpoint_id: "not-an-endpoint".to_string(),
                relay_url: None,
                name: "坏的".to_string(),
            },
            RuntimeImport {
                runtime_id: "rb".to_string(),
                endpoint_id: b.endpoint_id.clone(),
                relay_url: Some(" ".to_string()),
                name: "旧名字".to_string(),
            },
        ])
        .unwrap();

    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0].runtime_id, "rb");
    assert_eq!(imported[0].name, "旧名字");
    assert_eq!(imported[0].relay_url, None, "空 relay 当没有");
    assert_eq!(h.ids(), ["ra", "rb"]);
    h.wait_state("rb", ConnectionState::Connected);
    wait_until("连上后补全描述", || {
        h.view().runtimes[1].record.version == "9.9.9"
    });
    assert!(h.fleet.import(Vec::new()).unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// 事件流
// ---------------------------------------------------------------------------

#[test]
fn catalog_events_cover_every_runtime_including_new_ones() {
    let a = FakeRuntime::new("ra", "mac-a");
    let b = FakeRuntime::new("rb", "mac-b");
    let h = harness(&[(&a, None)], &[&b]);
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    h.fleet.subscribe_catalog(Box::new(move |event| {
        sink.lock().unwrap().push(event.clone());
        true
    }));
    let runtimes = |events: &Mutex<Vec<CatalogEvent>>| -> Vec<String> {
        events
            .lock()
            .unwrap()
            .iter()
            .map(|event| event.runtime_id.clone())
            .collect()
    };
    let first = runtimes(&events);
    assert!(
        !first.is_empty() && first.iter().all(|id| id == "ra"),
        "订阅时每台先推一次当前列表"
    );

    h.pair(&b, None).unwrap();
    wait_until("新机器的事件也推过来", || {
        runtimes(&events).iter().any(|id| id == "rb")
    });

    // 取消订阅后各节点都不再推。
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancelled);
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    h.fleet.subscribe_catalog(Box::new(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        !flag.load(Ordering::SeqCst)
    }));
    assert!(calls.load(Ordering::SeqCst) >= 2, "两台各推一次");
    cancelled.store(true, Ordering::SeqCst);
    h.fleet.forget("ra").unwrap();
    let after = calls.load(Ordering::SeqCst);
    let c = FakeRuntime::new("rc", "mac-c");
    h.transport.runtimes.lock().unwrap().push(Arc::clone(&c));
    h.pair(&c, None).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        after,
        "取消的订阅者不再挂到新机器上"
    );
}

#[test]
fn the_pass_has_a_station_per_runtime() {
    let a = FakeRuntime::new("ra", "mac-a");
    let b = FakeRuntime::new("rb", "mac-b");
    let h = harness(&[(&a, None), (&b, None)], &[]);
    let passes: Arc<Mutex<Vec<Pass>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&passes);
    h.fleet.subscribe_pass(Box::new(move |pass| {
        sink.lock().unwrap().push(pass.clone());
        true
    }));
    let stations = |passes: &Mutex<Vec<Pass>>| -> Vec<String> {
        passes
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .stations
            .iter()
            .map(|station| station.runtime_id.clone())
            .collect()
    };
    assert_eq!(stations(&passes), ["ra", "rb"]);
    h.fleet.move_runtime("rb", 0).unwrap();
    assert_eq!(stations(&passes), ["rb", "ra"]);
}

#[test]
fn calls_need_an_initialized_fleet_and_a_known_runtime() {
    let fleet = Fleet::new(FAST);
    assert!(fleet.node("ra").is_err());
    assert!(fleet.store().is_err());
    assert!(fleet.rename("ra", "x").is_err());
    // 订阅可以先于初始化。
    let views = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&views);
    fleet.subscribe(Box::new(move |view| {
        sink.lock().unwrap().push(view.clone());
        true
    }));
    assert_eq!(
        views.lock().unwrap().last().unwrap(),
        &FleetView {
            runtimes: Vec::new()
        }
    );

    let a = FakeRuntime::new("ra", "mac-a");
    let h = harness(&[(&a, None)], &[]);
    let missing = h.fleet.node("nope").err().unwrap();
    assert_eq!(missing.kind, SmeltErrorKind::Other);
    assert!(h.fleet.reconnect("nope").is_err());
}

#[test]
fn reconnect_and_verify_reach_the_runtime() {
    let a = FakeRuntime::new("ra", "mac-a");
    let h = harness(&[(&a, None)], &[]);
    h.wait_state("ra", ConnectionState::Connected);
    a.paired.store(false, Ordering::SeqCst);
    h.wait_state("ra", ConnectionState::Disconnected);

    a.paired.store(true, Ordering::SeqCst);
    h.fleet.reconnect("ra").unwrap();
    assert_eq!(h.state("ra"), Some(ConnectionState::Connected));

    let describes = a.describes.load(Ordering::SeqCst);
    h.fleet.verify();
    wait_until("立刻探活", || {
        a.describes.load(Ordering::SeqCst) > describes
    });
}

#[test]
fn init_client_is_idempotent_per_directory() {
    let dir = std::env::temp_dir().join(format!(
        "smelt-mobile-fleet-{}-{}",
        std::process::id(),
        crate::store::now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let fleet = Fleet::new(FAST);
    let device = fleet.init_client(dir.clone()).unwrap();
    assert_eq!(fleet.init_client(dir.clone()).unwrap(), device);
    assert!(
        fleet.init_client(dir.join("other")).is_err(),
        "身份不能悄悄换掉"
    );
    assert!(dir.join(crate::store::DATABASE_FILE).exists());
    assert!(fleet.store().unwrap().runtimes().unwrap().is_empty());
    // 同一目录换一个进程（新的机队）读出同一身份。
    assert_eq!(Fleet::new(FAST).init_client(dir.clone()).unwrap(), device);
    let _ = std::fs::remove_dir_all(&dir);
}
