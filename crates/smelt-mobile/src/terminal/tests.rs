//! 终端单测：假运行时在 `RuntimeStream::pair()` 上按 wire 格式回包，假连接源控制运行时代号。

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use smelt_runtime_client::RuntimeStream;
use smelt_runtime_client::protocol::control::ControlResponse;
use smelt_runtime_client::protocol::write_json_line;

use super::*;

const FAST: Timing = Timing {
    header_timeout: Duration::from_secs(2),
    write_timeout: Duration::from_secs(1),
    input_timeout: Duration::from_secs(2),
    probe_timeout: Duration::from_secs(1),
    backoff_base: Duration::from_millis(20),
    backoff_max: Duration::from_millis(80),
};

const GEOMETRY: TerminalGeometry = TerminalGeometry {
    cols: 80,
    rows: 24,
    cell_width: 9,
    cell_height: 18,
};

/// 一条 watch 连接的服务端：第几次附着（从 0 起）、首帧、读端（可能已缓冲了首帧之后的字节）、写端。
type WatchHandler =
    Box<dyn Fn(usize, Value, BufReader<RuntimeStream>, RuntimeStream) + Send + Sync>;

struct FakeRuntime {
    online: AtomicBool,
    watch: WatchHandler,
    watches: AtomicUsize,
    /// 收到的 watch 首帧。
    watch_requests: Mutex<Vec<Value>>,
    inputs: Mutex<Vec<Value>>,
    input_reply: Mutex<Value>,
}

impl FakeRuntime {
    fn new(
        watch: impl Fn(usize, Value, BufReader<RuntimeStream>, RuntimeStream) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            online: AtomicBool::new(true),
            watch: Box::new(watch),
            watches: AtomicUsize::new(0),
            watch_requests: Mutex::new(Vec::new()),
            inputs: Mutex::new(Vec::new()),
            input_reply: Mutex::new(json!({ "ok": true })),
        })
    }

    fn set_online(&self, online: bool) {
        self.online.store(online, Ordering::SeqCst);
    }

    fn watches(&self) -> usize {
        self.watches.load(Ordering::SeqCst)
    }

    fn inputs(&self) -> Vec<Value> {
        self.inputs.lock().unwrap().clone()
    }

    fn client(self: &Arc<Self>) -> RuntimeClient {
        let runtime = Arc::clone(self);
        RuntimeClient::new(move || {
            if !runtime.online.load(Ordering::SeqCst) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
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
        let writer = server.try_clone().unwrap();
        let mut reader = BufReader::new(server);
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let request: Value = serde_json::from_str(&line).unwrap();
        match request["op"].as_str() {
            Some("watch") => {
                let attempt = self.watches.fetch_add(1, Ordering::SeqCst);
                self.watch_requests.lock().unwrap().push(request.clone());
                (self.watch)(attempt, request, reader, writer);
            }
            Some("input") => {
                self.inputs.lock().unwrap().push(request);
                let reply = self.input_reply.lock().unwrap().clone();
                let _ = write_json_line(&mut &writer, &reply);
            }
            Some("control") => {
                assert_eq!(request["method"], "system.describe");
                let request_id = request["request_id"].as_str().unwrap();
                let _ = write_json_line(
                    &mut &writer,
                    &ControlResponse::ok(request_id, describe_result()),
                );
            }
            other => panic!("假运行时不认识的 op：{other:?}"),
        }
    }
}

fn describe_result() -> Value {
    json!({
        "control": {
            "current_version": 1,
            "min_supported_version": 1,
            "max_supported_version": 1,
            "methods": ["system.describe"],
        },
        "daemon": {
            "version": "9.9.9",
            "exe_mtime": 0,
            "exe": null,
            "pid": 1,
            "started_at": 0,
            "session_count": 0,
            "agent_event_version": 1,
            "runtime_id": "runtime-a",
        },
    })
}

/// 写头行和关键帧。
fn send_header(writer: &RuntimeStream, cols: u32, rows: u32, replay: &[u8], history_lines: u64) {
    let header = json!({
        "cols": cols,
        "rows": rows,
        "replay_len": replay.len(),
        "history_lines": history_lines,
    });
    let mut bytes = serde_json::to_vec(&header).unwrap();
    bytes.push(b'\n');
    bytes.extend_from_slice(replay);
    (&*writer).write_all(&bytes).unwrap();
}

/// 一直读到客户端断开，读到的字节返回。
fn read_until_eof(mut reader: BufReader<RuntimeStream>) -> Vec<u8> {
    let mut rest = Vec::new();
    let _ = reader.read_to_end(&mut rest);
    rest
}

/// 假连接源：代号与当前客户端可换，状态变化在锁里回调监听（同 `ConnectionHub`）。
struct FakeSource {
    state: Mutex<FakeState>,
}

struct FakeState {
    generation: u64,
    client: Option<RuntimeClient>,
    listeners: Vec<Listener>,
}

impl FakeSource {
    fn new(client: RuntimeClient) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeState {
                generation: 1,
                client: Some(client),
                listeners: Vec::new(),
            }),
        })
    }

    /// 换代（重新连过，或连接停下）。
    fn switch(&self, client: Option<RuntimeClient>) {
        let mut state = self.state.lock().unwrap();
        state.generation += 1;
        state.client = client;
        let connected = state.client.is_some();
        publish(
            &mut state,
            if connected {
                ConnectionState::Connected
            } else {
                ConnectionState::Disconnected
            },
        );
    }

    fn publish(&self, connection: ConnectionState) {
        publish(&mut self.state.lock().unwrap(), connection);
    }

    /// 同 `ConnectionHub::connect` 开头：撤掉当前连接、换代、报「正在连」。
    fn begin(&self) {
        let mut state = self.state.lock().unwrap();
        state.generation += 1;
        state.client = None;
        publish(&mut state, ConnectionState::Connecting);
    }

    /// 同 `ConnectionHub::connect` 收尾：这一代连上了。
    fn connected(&self, client: RuntimeClient) {
        let mut state = self.state.lock().unwrap();
        state.client = Some(client);
        publish(&mut state, ConnectionState::Connected);
    }
}

fn publish(state: &mut FakeState, connection: ConnectionState) {
    let generation = state.generation;
    let event = ConnectionEvent {
        state: connection,
        endpoint_id: None,
        link: None,
        error: None,
    };
    state
        .listeners
        .retain(|listener| listener(generation, &event));
}

impl RuntimeSource for FakeSource {
    fn current(&self) -> Option<(u64, RuntimeClient)> {
        let state = self.state.lock().unwrap();
        state
            .client
            .clone()
            .map(|client| (state.generation, client))
    }

    fn subscribe(&self, listener: Listener) {
        let mut state = self.state.lock().unwrap();
        let event = ConnectionEvent {
            state: ConnectionState::Connected,
            endpoint_id: None,
            link: None,
            error: None,
        };
        if listener(state.generation, &event) {
            state.listeners.push(listener);
        }
    }
}

/// 记下推出的事件；流结束（`Emit` 被丢掉）时置位。
#[derive(Clone)]
struct Recorder {
    events: Arc<Mutex<Vec<TerminalUpdate>>>,
    ended: Arc<AtomicBool>,
}

struct EndGuard(Arc<AtomicBool>);

impl Drop for EndGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Recorder {
    fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            ended: Arc::new(AtomicBool::new(false)),
        }
    }

    fn emit(&self) -> Emit {
        let events = Arc::clone(&self.events);
        let guard = EndGuard(Arc::clone(&self.ended));
        Box::new(move |event| {
            let _ = &guard;
            events.lock().unwrap().push(event);
            true
        })
    }

    fn events(&self) -> Vec<TerminalUpdate> {
        self.events.lock().unwrap().clone()
    }

    fn ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }

    fn count(&self, matches: impl Fn(&TerminalUpdate) -> bool) -> usize {
        self.events().iter().filter(|event| matches(event)).count()
    }

    fn readies(&self) -> usize {
        self.count(|event| matches!(event, TerminalUpdate::Ready { .. }))
    }

    fn wait_readies(&self, n: usize) {
        wait_until(&format!("第 {n} 个 Ready"), || self.readies() >= n);
    }

    fn wait_ended(&self) {
        wait_until("流结束", || self.ended());
    }
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "等不到：{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// 把 `Data` 按 `ReplayComplete` 分成关键帧和实时输出两段（只看第一个 Ready 之后）。
fn split_data(events: &[TerminalUpdate]) -> (Vec<u8>, Vec<u8>) {
    let mut replay = Vec::new();
    let mut live = Vec::new();
    let mut replayed = false;
    for event in events {
        match event {
            TerminalUpdate::Data(bytes) if replayed => live.extend_from_slice(bytes),
            TerminalUpdate::Data(bytes) => replay.extend_from_slice(bytes),
            TerminalUpdate::ReplayComplete => replayed = true,
            TerminalUpdate::Ready { .. } if replayed => break,
            _ => {}
        }
    }
    (replay, live)
}

fn setup(runtime: &Arc<FakeRuntime>) -> (TerminalHub, Arc<FakeSource>) {
    let source = FakeSource::new(runtime.client());
    let hub = TerminalHub::new(Arc::clone(&source) as Arc<dyn RuntimeSource>, FAST);
    (hub, source)
}

/// 附着后发头行与空关键帧，然后挂着直到客户端断开。
fn idle_watch(_: usize, _: Value, reader: BufReader<RuntimeStream>, writer: RuntimeStream) {
    send_header(&writer, 80, 24, b"", 0);
    read_until_eof(reader);
}

#[test]
fn watch_request_declares_geometry_and_clamps_to_runtime_limits() {
    let runtime = FakeRuntime::new(idle_watch);
    let (hub, _source) = setup(&runtime);
    let recorder = Recorder::new();

    let oversized = TerminalGeometry {
        cols: 5000,
        rows: 40,
        cell_width: 300,
        cell_height: 18,
    };
    hub.attach("t1".into(), oversized, Some(20_000), recorder.emit())
        .unwrap();
    recorder.wait_readies(1);
    hub.attach("t2".into(), GEOMETRY, None, recorder.emit())
        .unwrap();
    recorder.wait_readies(2);

    let requests = runtime.watch_requests.lock().unwrap().clone();
    assert_eq!(
        requests[0],
        json!({
            "op": "watch",
            "id": "t1",
            "controls_geometry": true,
            "cols": 1000,
            "rows": 40,
            "cell_w": 256,
            "cell_h": 18,
            "max_scrollback_lines": 10_000,
        })
    );
    // 不传预算就不带这个字段：运行时给全量。
    assert_eq!(
        requests[1],
        json!({
            "op": "watch",
            "id": "t2",
            "controls_geometry": true,
            "cols": 80,
            "rows": 24,
            "cell_w": 9,
            "cell_h": 18,
        })
    );
    hub.detach_all();
}

#[test]
fn attach_rejects_empty_geometry_and_session_id() {
    let runtime = FakeRuntime::new(idle_watch);
    let (hub, _source) = setup(&runtime);
    let zero = TerminalGeometry {
        cols: 0,
        ..GEOMETRY
    };

    assert!(
        hub.attach("t1".into(), zero, None, Recorder::new().emit())
            .is_err()
    );
    assert!(
        hub.attach(String::new(), GEOMETRY, None, Recorder::new().emit())
            .is_err()
    );
    assert!(
        hub.resize("t1", GEOMETRY).is_err(),
        "没附着的会话不能 resize"
    );
    assert!(hub.input("t1", "x".into()).is_err(), "没附着的会话不能输入");
    assert_eq!(runtime.watches(), 0);
}

#[test]
fn header_keyframe_and_live_bytes_arrive_in_order() {
    // 关键帧跨好几个读块，尾巴和实时输出在同一次写里到。
    let replay: Vec<u8> = (0..150_000u32).map(|i| (i % 251) as u8).collect();
    let live = b"\x1b[31mlive output\x1b[0m".to_vec();
    let (replay_server, live_server) = (replay.clone(), live.clone());
    let runtime = FakeRuntime::new(move |attempt, _, reader, writer| {
        if attempt > 0 {
            // 会话已结束：头行前就关。
            return;
        }
        let header = json!({
            "cols": 120,
            "rows": 40,
            "replay_len": replay_server.len(),
            "history_lines": 9_000,
        });
        write_json_line(&mut &writer, &header).unwrap();
        (&writer).write_all(&replay_server[..70_000]).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        (&writer)
            .write_all(&replay_server[70_000..140_000])
            .unwrap();
        std::thread::sleep(Duration::from_millis(30));
        let mut tail = replay_server[140_000..].to_vec();
        tail.extend_from_slice(&live_server);
        (&writer).write_all(&tail).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        // shell 退出：运行时关掉连接。
        drop(writer);
        drop(reader);
    });
    let (hub, _source) = setup(&runtime);
    let recorder = Recorder::new();

    hub.attach("t1".into(), GEOMETRY, Some(600), recorder.emit())
        .unwrap();
    recorder.wait_ended();

    let events = recorder.events();
    assert_eq!(
        events[0],
        TerminalUpdate::Ready {
            cols: 120,
            rows: 40,
            replay_bytes: 150_000,
            history_lines: 9_000,
            scrollback_lines: 600,
        }
    );
    assert_eq!(events.last(), Some(&TerminalUpdate::Closed));
    let (got_replay, got_live) = split_data(&events);
    assert_eq!(got_replay, replay, "ReplayComplete 必须正好落在关键帧末尾");
    assert_eq!(got_live, live);
    let chunks: Vec<usize> = events
        .iter()
        .filter_map(|event| match event {
            TerminalUpdate::Data(bytes) => Some(bytes.len()),
            _ => None,
        })
        .collect();
    assert!(chunks.iter().all(|len| *len <= CHUNK_BYTES), "{chunks:?}");
    assert!(chunks.len() >= 4, "关键帧应分好几块：{chunks:?}");
    assert_eq!(recorder.readies(), 1);
    assert_eq!(recorder.count(|e| *e == TerminalUpdate::ReplayComplete), 1);
    // EOF 后重新附着一次，运行时头行前就关、自身可达，确认会话已结束。
    assert_eq!(runtime.watches(), 2);
}

#[test]
fn empty_keyframe_completes_right_after_ready() {
    let runtime = FakeRuntime::new(idle_watch);
    let (hub, _source) = setup(&runtime);
    let recorder = Recorder::new();

    hub.attach("t1".into(), GEOMETRY, None, recorder.emit())
        .unwrap();
    wait_until("ReplayComplete", || recorder.events().len() >= 2);

    let events = recorder.events();
    assert!(matches!(
        events[0],
        TerminalUpdate::Ready {
            replay_bytes: 0,
            scrollback_lines: MAX_SCROLLBACK_LINES,
            ..
        }
    ));
    assert_eq!(events[1], TerminalUpdate::ReplayComplete);
    hub.detach("t1");
}

#[test]
fn missing_session_is_fatal_and_does_not_retry() {
    // 会话不存在：运行时读完首帧直接关。
    let runtime = FakeRuntime::new(|_, _, _, _| {});
    let (hub, _source) = setup(&runtime);
    let recorder = Recorder::new();

    hub.attach("gone".into(), GEOMETRY, None, recorder.emit())
        .unwrap();
    recorder.wait_ended();

    let events = recorder.events();
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(
        &events[0],
        TerminalUpdate::Error { fatal: true, .. }
    ));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(runtime.watches(), 1);
    assert!(
        hub.input("gone", "x".into()).is_err(),
        "结束的附着已从表里摘掉"
    );
}

#[test]
fn oversized_keyframe_is_fatal() {
    let runtime = FakeRuntime::new(|_, _, reader, writer| {
        let header = json!({ "cols": 80, "rows": 24, "replay_len": MAX_REPLAY_BYTES + 1 });
        write_json_line(&mut &writer, &header).unwrap();
        read_until_eof(reader);
    });
    let (hub, _source) = setup(&runtime);
    let recorder = Recorder::new();

    hub.attach("t1".into(), GEOMETRY, None, recorder.emit())
        .unwrap();
    recorder.wait_ended();

    let events = recorder.events();
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(
        &events[0],
        TerminalUpdate::Error { fatal: true, .. }
    ));
    assert_eq!(runtime.watches(), 1);
}

#[test]
fn abnormal_drop_reattaches_with_a_fresh_keyframe() {
    let runtime = FakeRuntime::new(|attempt, _, reader, writer| {
        match attempt {
            0 => {
                send_header(&writer, 80, 24, b"first", 10);
                (&writer).write_all(b"x").unwrap();
                std::thread::sleep(Duration::from_millis(20));
                // 链路断了：本地 socket 被关，客户端读到的也是 EOF。
                drop(writer);
                drop(reader);
            }
            _ => {
                send_header(&writer, 80, 24, b"second", 10);
                read_until_eof(reader);
            }
        }
    });
    let (hub, _source) = setup(&runtime);
    let recorder = Recorder::new();

    hub.attach("t1".into(), GEOMETRY, None, recorder.emit())
        .unwrap();
    recorder.wait_readies(2);
    wait_until("第二份关键帧", || {
        recorder.count(|e| *e == TerminalUpdate::ReplayComplete) >= 2
    });

    let data: Vec<u8> = recorder
        .events()
        .iter()
        .filter_map(|event| match event {
            TerminalUpdate::Data(bytes) => Some(bytes.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(data, b"firstxsecond");
    assert!(!recorder.ended());
    assert_eq!(runtime.watches(), 2);
    hub.detach("t1");
    recorder.wait_ended();
}

#[test]
fn close_before_header_while_unreachable_is_retried_not_fatal() {
    let runtime = Arc::new(Mutex::new(None::<Arc<FakeRuntime>>));
    let handle = Arc::clone(&runtime);
    let fake = FakeRuntime::new(move |attempt, _, reader, writer| {
        if attempt == 0 {
            // 链路断在头行前：连接被关，而且运行时此刻连不上（探活也失败）。
            handle.lock().unwrap().as_ref().unwrap().set_online(false);
            return;
        }
        send_header(&writer, 80, 24, b"ok", 0);
        read_until_eof(reader);
    });
    *runtime.lock().unwrap() = Some(Arc::clone(&fake));
    let (hub, _source) = setup(&fake);
    let recorder = Recorder::new();

    hub.attach("t1".into(), GEOMETRY, None, recorder.emit())
        .unwrap();
    wait_until("非 fatal 的断线错误", || {
        recorder.count(|e| matches!(e, TerminalUpdate::Error { fatal: false, .. })) >= 2
    });
    assert_eq!(recorder.readies(), 0);
    fake.set_online(true);
    recorder.wait_readies(1);

    assert!(!recorder.ended());
    assert_eq!(
        recorder.count(|e| matches!(e, TerminalUpdate::Error { fatal: true, .. })),
        0
    );
    hub.detach("t1");
    *runtime.lock().unwrap() = None;
}

#[test]
fn reconnect_backs_off_and_retries_at_once_when_the_runtime_comes_back() {
    let fake = FakeRuntime::new(idle_watch);
    fake.set_online(false);
    let source = FakeSource::new(fake.client());
    let slow = Timing {
        backoff_base: Duration::from_secs(30),
        backoff_max: Duration::from_secs(30),
        ..FAST
    };
    let hub = TerminalHub::new(Arc::clone(&source) as Arc<dyn RuntimeSource>, slow);
    let recorder = Recorder::new();

    hub.attach("t1".into(), GEOMETRY, None, recorder.emit())
        .unwrap();
    wait_until("连不上的错误", || {
        recorder.count(|e| matches!(e, TerminalUpdate::Error { fatal: false, .. })) >= 1
    });
    // 退避 30 秒：只有「运行时重新连上」能让它立刻重试。
    fake.set_online(true);
    source.publish(ConnectionState::Connected);
    recorder.wait_readies(1);
    hub.detach("t1");
}

#[test]
fn detach_closes_the_watch_and_stops_reconnecting() {
    let (closed_tx, closed_rx) = mpsc::channel();
    let closed_tx = Mutex::new(closed_tx);
    let runtime = FakeRuntime::new(move |_, _, reader, writer| {
        send_header(&writer, 80, 24, b"", 0);
        read_until_eof(reader);
        closed_tx.lock().unwrap().send(()).unwrap();
    });
    let (hub, _source) = setup(&runtime);
    let recorder = Recorder::new();

    hub.attach("t1".into(), GEOMETRY, None, recorder.emit())
        .unwrap();
    recorder.wait_readies(1);
    hub.detach("t1");

    closed_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("运行时那头读到 EOF（归还几何租约）");
    recorder.wait_ended();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(runtime.watches(), 1, "detach 后不再重连");
    let events = recorder.events();
    assert!(
        events
            .iter()
            .all(|e| !matches!(e, TerminalUpdate::Error { .. } | TerminalUpdate::Closed)),
        "{events:?}"
    );
    assert!(hub.input("t1", "x".into()).is_err());
}

#[test]
fn detach_all_closes_every_attachment() {
    let runtime = FakeRuntime::new(idle_watch);
    let (hub, _source) = setup(&runtime);
    let (a, b) = (Recorder::new(), Recorder::new());

    hub.attach("t1".into(), GEOMETRY, None, a.emit()).unwrap();
    hub.attach("t2".into(), GEOMETRY, None, b.emit()).unwrap();
    a.wait_readies(1);
    b.wait_readies(1);
    hub.detach_all();

    a.wait_ended();
    b.wait_ended();
}

#[test]
fn a_new_generation_reattaches_on_the_new_link() {
    let (closed_tx, closed_rx) = mpsc::channel();
    let closed_tx = Mutex::new(closed_tx);
    let old = FakeRuntime::new(move |_, _, reader, writer| {
        send_header(&writer, 80, 24, b"", 0);
        read_until_eof(reader);
        closed_tx.lock().unwrap().send(()).unwrap();
    });
    let new = FakeRuntime::new(idle_watch);
    let source = FakeSource::new(old.client());
    // 退避很长：只有「换代立刻重新附着」能让下面的断言成立。
    let hub = TerminalHub::new(
        Arc::clone(&source) as Arc<dyn RuntimeSource>,
        Timing {
            backoff_base: Duration::from_secs(3600),
            backoff_max: Duration::from_secs(3600),
            ..FAST
        },
    );
    let recorder = Recorder::new();

    hub.attach("t1".into(), GEOMETRY, None, recorder.emit())
        .unwrap();
    recorder.wait_readies(1);
    // 同一台机器重新连过（重连、endpoint 换形态）：旧链路上的附着掐掉，立刻在新连接上重新附着。
    source.switch(Some(new.client()));

    closed_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("旧一代链路上的 watch 被关掉");
    recorder.wait_readies(2);
    assert_eq!(new.watches(), 1, "在新连接上重新附着");
    assert_eq!(old.watches(), 1);
    assert!(!recorder.ended());
    assert!(
        !recorder
            .events()
            .iter()
            .any(|event| matches!(event, TerminalUpdate::Error { fatal: true, .. }))
    );
}

/// 真的状态机换代时先撤掉旧连接、报「正在连」，连上才有新客户端：中间这段没有当前连接，
/// 终端要等新一代连上，不能当成连接停了。
#[test]
fn a_new_generation_still_connecting_is_waited_for() {
    let old = FakeRuntime::new(|_, _, reader, writer| {
        send_header(&writer, 80, 24, b"", 0);
        read_until_eof(reader);
    });
    let new = FakeRuntime::new(idle_watch);
    let source = FakeSource::new(old.client());
    let hub = TerminalHub::new(
        Arc::clone(&source) as Arc<dyn RuntimeSource>,
        Timing {
            backoff_base: Duration::from_secs(3600),
            backoff_max: Duration::from_secs(3600),
            ..FAST
        },
    );
    let recorder = Recorder::new();
    hub.attach("t1".into(), GEOMETRY, None, recorder.emit())
        .unwrap();
    recorder.wait_readies(1);

    source.begin();
    std::thread::sleep(Duration::from_millis(100));
    assert!(!recorder.ended(), "正在连不是连接停了");
    source.connected(new.client());

    recorder.wait_readies(2);
    assert_eq!(new.watches(), 1, "在新连接上重新附着");
    assert!(
        !recorder
            .events()
            .iter()
            .any(|event| matches!(event, TerminalUpdate::Error { fatal: true, .. }))
    );
}

#[test]
fn a_stopped_connection_while_reconnecting_stops_retrying() {
    let runtime = FakeRuntime::new(idle_watch);
    runtime.set_online(false);
    let (hub, source) = setup(&runtime);
    let recorder = Recorder::new();

    hub.attach("t1".into(), GEOMETRY, None, recorder.emit())
        .unwrap();
    wait_until("连不上的错误", || !recorder.events().is_empty());
    source.switch(None);

    recorder.wait_ended();
    assert!(matches!(
        recorder.events().last(),
        Some(TerminalUpdate::Error { fatal: true, .. })
    ));
    runtime.set_online(true);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(runtime.watches(), 0);
}

#[test]
fn attach_without_a_runtime_is_fatal() {
    let runtime = FakeRuntime::new(idle_watch);
    let (hub, source) = setup(&runtime);
    source.switch(None);
    let recorder = Recorder::new();

    hub.attach("t1".into(), GEOMETRY, None, recorder.emit())
        .unwrap();
    recorder.wait_ended();

    assert_eq!(
        recorder.events(),
        vec![TerminalUpdate::Error {
            message: NO_RUNTIME.to_string(),
            fatal: true,
        }]
    );
}

#[test]
fn attaching_again_replaces_the_old_stream() {
    let runtime = FakeRuntime::new(idle_watch);
    let (hub, _source) = setup(&runtime);
    let (first, second) = (Recorder::new(), Recorder::new());

    hub.attach("t1".into(), GEOMETRY, Some(144), first.emit())
        .unwrap();
    first.wait_readies(1);
    hub.attach("t1".into(), GEOMETRY, Some(5_000), second.emit())
        .unwrap();

    first.wait_ended();
    second.wait_readies(1);
    assert!(matches!(
        second.events()[0],
        TerminalUpdate::Ready {
            scrollback_lines: 5_000,
            ..
        }
    ));
    let requests = runtime.watch_requests.lock().unwrap().clone();
    assert_eq!(requests[1]["max_scrollback_lines"], 5_000);
    // 旧附着收尾时不能把新附着从表里摘掉。
    std::thread::sleep(Duration::from_millis(100));
    assert!(hub.resize("t1", GEOMETRY).is_ok());
    hub.detach("t1");
    second.wait_ended();
}

#[test]
fn cancelled_dart_stream_stops_the_attachment() {
    let (closed_tx, closed_rx) = mpsc::channel();
    let closed_tx = Mutex::new(closed_tx);
    let runtime = FakeRuntime::new(move |_, _, reader, writer| {
        send_header(&writer, 80, 24, b"", 0);
        (&writer).write_all(b"output").unwrap();
        read_until_eof(reader);
        closed_tx.lock().unwrap().send(()).unwrap();
    });
    let (hub, _source) = setup(&runtime);

    // Dart 已取消订阅：每次推送都失败。
    hub.attach("t1".into(), GEOMETRY, None, Box::new(|_| false))
        .unwrap();

    closed_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("取消订阅后关掉 watch");
    wait_until("附着被摘掉", || hub.input("t1", "x".into()).is_err());
    assert_eq!(runtime.watches(), 1);
}

#[test]
fn resize_writes_a_type_one_frame_only_when_the_size_changes() {
    let (frames_tx, frames_rx) = mpsc::channel::<Vec<u8>>();
    let frames_tx = Mutex::new(frames_tx);
    let runtime = FakeRuntime::new(move |_, _, mut reader, writer| {
        send_header(&writer, 80, 24, b"", 0);
        let mut frame = [0u8; 21];
        while reader.read_exact(&mut frame).is_ok() {
            frames_tx.lock().unwrap().send(frame.to_vec()).unwrap();
        }
    });
    let (hub, _source) = setup(&runtime);
    let recorder = Recorder::new();
    hub.attach("t1".into(), GEOMETRY, None, recorder.emit())
        .unwrap();
    recorder.wait_readies(1);

    let narrow = TerminalGeometry {
        cols: 40,
        rows: 30,
        cell_width: 10,
        cell_height: 20,
    };
    let huge = TerminalGeometry {
        cols: 70_000,
        rows: 2,
        cell_width: 1_000,
        cell_height: 0,
    };
    hub.resize("t1", GEOMETRY).unwrap(); // 和附着时一样：不写
    hub.resize("t1", narrow).unwrap();
    hub.resize("t1", narrow).unwrap(); // 没变：不写
    hub.resize("t1", huge).unwrap();
    assert!(
        hub.resize("t1", TerminalGeometry { rows: 0, ..narrow })
            .is_err()
    );

    let recv = || frames_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        recv(),
        vec![
            1, 0, 0, 0, 16, // type 1，长度 16
            0, 0, 0, 40, // cols
            0, 0, 0, 30, // rows
            0, 0, 0, 10, // cell_w
            0, 0, 0, 20, // cell_h
        ]
    );
    // 夹到运行时上限：cols 1000，cell_w 256。
    assert_eq!(
        recv(),
        vec![
            1, 0, 0, 0, 16, 0, 0, 3, 232, 0, 0, 0, 2, 0, 0, 1, 0, 0, 0, 0, 0
        ]
    );
    assert!(frames_rx.recv_timeout(Duration::from_millis(100)).is_err());
    hub.detach("t1");
}

#[test]
fn resize_while_reconnecting_is_applied_by_the_next_attach() {
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let gate_rx = Mutex::new(gate_rx);
    let runtime = FakeRuntime::new(move |attempt, _, reader, writer| {
        if attempt == 0 {
            send_header(&writer, 80, 24, b"", 0);
            // 等测试放行再断。
            let _ = gate_rx.lock().unwrap().recv();
            return;
        }
        send_header(&writer, 100, 50, b"", 0);
        read_until_eof(reader);
    });
    let (hub, _source) = setup(&runtime);
    let recorder = Recorder::new();
    hub.attach("t1".into(), GEOMETRY, None, recorder.emit())
        .unwrap();
    recorder.wait_readies(1);

    runtime.set_online(false);
    gate_tx.send(()).unwrap();
    wait_until("断线后重连失败", || {
        recorder.count(|e| matches!(e, TerminalUpdate::Error { fatal: false, .. })) >= 1
    });
    let bigger = TerminalGeometry {
        cols: 100,
        rows: 50,
        ..GEOMETRY
    };
    hub.resize("t1", bigger).unwrap();
    runtime.set_online(true);
    recorder.wait_readies(2);

    let requests = runtime.watch_requests.lock().unwrap().clone();
    assert_eq!(requests[1]["cols"], 100);
    assert_eq!(requests[1]["rows"], 50);
    hub.detach("t1");
}

#[test]
fn input_uses_the_input_op_and_reports_err() {
    let runtime = FakeRuntime::new(idle_watch);
    let (hub, _source) = setup(&runtime);
    let recorder = Recorder::new();
    hub.attach("t1".into(), GEOMETRY, None, recorder.emit())
        .unwrap();
    recorder.wait_readies(1);

    hub.input("t1", String::new()).unwrap(); // 空串忽略
    hub.input("t1", "ls -la\r".into()).unwrap();
    wait_until("收到输入", || runtime.inputs().len() == 1);
    assert_eq!(
        runtime.inputs()[0],
        json!({ "op": "input", "id": "t1", "data": "ls -la\r" })
    );

    *runtime.input_reply.lock().unwrap() = json!({ "ok": false, "err": "会话不存在" });
    hub.input("t1", "x".into()).unwrap();
    wait_until("输入失败的错误", || {
        recorder.count(|e| matches!(e, TerminalUpdate::Error { .. })) == 1
    });
    let error = recorder
        .events()
        .into_iter()
        .find(|e| matches!(e, TerminalUpdate::Error { .. }))
        .unwrap();
    let TerminalUpdate::Error { message, fatal } = error else {
        unreachable!()
    };
    assert!(!fatal);
    assert!(message.contains("会话不存在"), "{message}");
    assert!(!recorder.ended(), "输入失败不影响附着");
    hub.detach("t1");
}

#[test]
fn inputs_are_sent_in_order() {
    let runtime = FakeRuntime::new(idle_watch);
    let (hub, _source) = setup(&runtime);
    let recorder = Recorder::new();
    hub.attach("t1".into(), GEOMETRY, None, recorder.emit())
        .unwrap();

    let expected: String = (0..200)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    for c in expected.chars() {
        hub.input("t1", c.to_string()).unwrap();
    }
    wait_until("全部输入到达", || {
        runtime
            .inputs()
            .iter()
            .map(|input| input["data"].as_str().unwrap().len())
            .sum::<usize>()
            == expected.len()
    });
    let received: String = runtime
        .inputs()
        .iter()
        .map(|input| input["data"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(received, expected);
    hub.detach("t1");
}

#[test]
fn large_input_is_split_on_utf8_boundaries() {
    let runtime = FakeRuntime::new(idle_watch);
    let (hub, _source) = setup(&runtime);
    let recorder = Recorder::new();
    hub.attach("t1".into(), GEOMETRY, None, recorder.emit())
        .unwrap();

    // 3 字节的「中」：65536 不是 3 的倍数，按字节硬切会切坏字符。
    let data = "中".repeat(30_000);
    hub.input("t1", data.clone()).unwrap();
    wait_until("分块到达", || {
        runtime
            .inputs()
            .iter()
            .map(|input| input["data"].as_str().unwrap().len())
            .sum::<usize>()
            == data.len()
    });
    let chunks: Vec<String> = runtime
        .inputs()
        .iter()
        .map(|input| input["data"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(chunks.len(), 2);
    assert!(chunks.iter().all(|chunk| chunk.len() <= MAX_INPUT_BYTES));
    assert_eq!(chunks.concat(), data);
    hub.detach("t1");
}

#[test]
fn take_chunk_never_splits_a_character() {
    let mut buf = format!("{}😀tail", "a".repeat(9));
    // 😀 占第 9..13 字节：上限 10 落在它中间。
    assert_eq!(take_chunk(&mut buf, 10), "a".repeat(9));
    assert_eq!(take_chunk(&mut buf, 10), "😀tail");
    assert!(buf.is_empty());

    let mut short = "短".to_string();
    assert_eq!(take_chunk(&mut short, 10), "短");
    assert!(short.is_empty());
}

#[test]
fn backoff_doubles_from_500ms_up_to_8s() {
    let timing = Timing::PRODUCTION;
    let delays: Vec<u64> = (1..=7)
        .map(|failures| timing.backoff(failures).as_millis() as u64)
        .collect();
    assert_eq!(delays, vec![500, 1000, 2000, 4000, 8000, 8000, 8000]);
}
