//! 订阅与动作的单测：假运行时在 `RuntimeStream::pair()` 上按线格式回包，假连接状态源可以
//! 随时换代、掉线。

use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;

use serde_json::{Value, json};
use smelt_model::acp_chat::AcpEntry;
use smelt_model::acp_wire::FrameMirror;
use smelt_runtime_client::protocol::control::ControlResponse;

use super::merge::tests::{assistant, snapshot, snapshot_json};
use super::*;

const SESSION: &str = "acp-1";
const ENDPOINT: &str = "runtime-endpoint";
const WAIT: Duration = Duration::from_secs(3);

const FAST: Timing = Timing {
    backoff_base: Duration::from_millis(20),
    backoff_max: Duration::from_millis(80),
    emit_interval: Duration::from_millis(5),
    save_delay: Duration::from_millis(20),
};

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !condition() {
        assert!(Instant::now() < deadline, "等不到：{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---------------------------------------------------------------------------
// 假运行时
// ---------------------------------------------------------------------------

enum Reply {
    Line(Value),
    /// 读完请求就断开，不回一行。
    Close,
}

type Responder = Box<dyn Fn(&Value) -> Reply + Send>;

struct FakeRuntime {
    online: AtomicBool,
    requests: Mutex<Vec<Value>>,
    watchers: Mutex<Vec<Arc<FakeWatch>>>,
    responder: Mutex<Responder>,
}

/// 运行时这一端的一条 watch 连接。
struct FakeWatch {
    stream: RuntimeStream,
    mirror: Mutex<FrameMirror>,
    /// 客户端在首行之后又写了东西（协议不允许）。
    client_wrote: AtomicBool,
    /// 客户端那边关掉了（读到 EOF）。
    closed: AtomicBool,
}

impl FakeWatch {
    fn send(&self, value: &Value) {
        let _ = write_json_line(&mut &self.stream, value);
    }

    /// 整段帧：发送方镜像随之重建。
    fn full(&self, snapshot: Value) {
        let frame = json!({ "snapshot": snapshot });
        self.mirror.lock().unwrap().full(&frame);
        self.send(&frame);
    }

    /// 增量帧：`snapshot` 是这一帧完整的快照（条目从 0 起），只发相对镜像的变化。
    fn delta(&self, snapshot: Value) {
        let entries: Vec<AcpEntry> = serde_json::from_value(snapshot["entries"].clone()).unwrap();
        let frame = json!({
            "live": {"phase": snapshot["phase"], "history_len": 0},
            "snapshot": snapshot,
            "provider_pid": 7,
        });
        let delta = self
            .mirror
            .lock()
            .unwrap()
            .encode(&frame, 0, &entries, 0, entries.len(), 0)
            .unwrap();
        self.send(&json!({ "delta": delta }));
    }

    fn close(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

impl FakeRuntime {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            online: AtomicBool::new(true),
            requests: Mutex::new(Vec::new()),
            watchers: Mutex::new(Vec::new()),
            responder: Mutex::new(Box::new(|request| match request["op"].as_str() {
                Some("acp_action") => Reply::Line(json!({"ok": true})),
                _ => Reply::Close,
            })),
        })
    }

    fn respond(&self, responder: impl Fn(&Value) -> Reply + Send + 'static) {
        *self.responder.lock().unwrap() = Box::new(responder);
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
        let mut reader = BufReader::new(server.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
            return;
        }
        let request: Value = serde_json::from_str(&line).unwrap();
        self.requests.lock().unwrap().push(request.clone());
        if request["op"] == "acp_watch" {
            let watch = Arc::new(FakeWatch {
                stream: server,
                mirror: Mutex::new(FrameMirror::default()),
                client_wrote: AtomicBool::new(false),
                closed: AtomicBool::new(false),
            });
            self.watchers.lock().unwrap().push(Arc::clone(&watch));
            let mut rest = Vec::new();
            let _ = reader.read_to_end(&mut rest);
            if !rest.is_empty() {
                watch.client_wrote.store(true, Ordering::SeqCst);
            }
            watch.closed.store(true, Ordering::SeqCst);
            return;
        }
        let reply = (self.responder.lock().unwrap())(&request);
        if let Reply::Line(value) = reply {
            let _ = write_json_line(&mut &server, &value);
        }
    }

    fn requests(&self, op: &str) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request["op"] == op)
            .cloned()
            .collect()
    }

    fn actions(&self) -> Vec<Value> {
        self.requests("acp_action")
            .into_iter()
            .map(|request| request["action"].clone())
            .collect()
    }

    /// 第 `n` 条（从 1 数）watch 连接。
    fn watcher(&self, n: usize) -> Arc<FakeWatch> {
        wait_until(&format!("第 {n} 条 watch 连接"), || {
            self.watchers.lock().unwrap().len() >= n
        });
        Arc::clone(&self.watchers.lock().unwrap()[n - 1])
    }

    fn watch_count(&self) -> usize {
        self.watchers.lock().unwrap().len()
    }
}

// ---------------------------------------------------------------------------
// 假连接状态
// ---------------------------------------------------------------------------

struct FakeSource {
    inner: Mutex<SourceInner>,
    cache: Option<Arc<Cache>>,
}

struct SourceInner {
    generation: u64,
    event: ConnectionEvent,
    client: Option<RuntimeClient>,
    listeners: Vec<Listener>,
}

fn connection_event(state: ConnectionState) -> ConnectionEvent {
    ConnectionEvent {
        state,
        endpoint_id: Some(ENDPOINT.to_string()),
        link: None,
        error: None,
    }
}

impl FakeSource {
    fn connected(runtime: &Arc<FakeRuntime>, cache: Option<Arc<Cache>>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(SourceInner {
                generation: 1,
                event: connection_event(ConnectionState::Connected),
                client: Some(runtime.client()),
                listeners: Vec::new(),
            }),
            cache,
        })
    }

    fn disconnected(cache: Option<Arc<Cache>>) -> Arc<Self> {
        let mut event = connection_event(ConnectionState::Disconnected);
        event.error = Some(error(SmeltErrorKind::Unpaired, "这台设备还没有配对"));
        Arc::new(Self {
            inner: Mutex::new(SourceInner {
                generation: 1,
                event,
                client: None,
                listeners: Vec::new(),
            }),
            cache,
        })
    }

    fn publish(&self, generation: u64, state: ConnectionState) {
        let mut inner = self.inner.lock().unwrap();
        inner.generation = generation;
        inner.event = connection_event(state);
        let SourceInner {
            generation,
            event,
            listeners,
            ..
        } = &mut *inner;
        listeners.retain(|listener| listener(*generation, event));
    }
}

impl RuntimeSource for FakeSource {
    fn current(&self) -> Option<CurrentRuntime> {
        let inner = self.inner.lock().unwrap();
        inner.client.clone().map(|client| CurrentRuntime {
            generation: inner.generation,
            endpoint_id: ENDPOINT.to_string(),
            client,
        })
    }

    fn subscribe(&self, listener: Listener) {
        let mut inner = self.inner.lock().unwrap();
        if listener(inner.generation, &inner.event) {
            inner.listeners.push(listener);
        }
    }

    fn cache(&self) -> Option<Arc<Cache>> {
        self.cache.clone()
    }
}

// ---------------------------------------------------------------------------
// 订阅端
// ---------------------------------------------------------------------------

/// 一个订阅者收到的事件，带它的订阅编号。
struct Events(mpsc::Receiver<ConversationEvent>, u32);

impl Events {
    fn next(&self) -> ConversationEvent {
        self.0.recv_timeout(WAIT).expect("等不到事件")
    }

    /// 下一个满足条件的快照（跳过中间被合并掉的）。
    fn snapshot_where(
        &self,
        what: &str,
        accept: impl Fn(&ConversationEvent) -> bool,
    ) -> ConversationEvent {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = self
                .0
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("等不到快照：{what}"));
            if event.kind == ConversationEventKind::Snapshot && accept(&event) {
                return event;
            }
        }
    }

    fn quiet(&self) -> Vec<ConversationEvent> {
        std::thread::sleep(Duration::from_millis(100));
        self.0.try_iter().collect()
    }
}

fn texts(event: &ConversationEvent) -> Vec<String> {
    event
        .view
        .as_ref()
        .unwrap()
        .entries
        .iter()
        .map(|entry| entry.text.clone())
        .collect()
}

/// 订阅编号，测试之间也不重复。
fn next_watch_id() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn watch(conversations: &Conversations) -> Events {
    watch_session(conversations, SESSION)
}

fn watch_session(conversations: &Conversations, session_id: &str) -> Events {
    let (tx, rx) = mpsc::channel();
    let id = next_watch_id();
    conversations.watch(
        session_id.to_string(),
        id,
        Box::new(move |event| tx.send(event).is_ok()),
    );
    Events(rx, id)
}

fn labels(range: std::ops::Range<usize>) -> Vec<String> {
    range.map(|i| format!("e{i}")).collect()
}

fn entries(labels: &[String]) -> Vec<AcpEntry> {
    labels.iter().map(|label| assistant(label)).collect()
}

fn setup() -> (Arc<FakeRuntime>, Arc<FakeSource>, Conversations) {
    let runtime = FakeRuntime::new();
    let source = FakeSource::connected(&runtime, None);
    let conversations = Conversations::new(source.clone(), FAST);
    (runtime, source, conversations)
}

// ---------------------------------------------------------------------------
// 订阅
// ---------------------------------------------------------------------------

#[test]
fn full_and_delta_frames_restore_the_view() {
    let (runtime, _source, conversations) = setup();
    let events = watch(&conversations);
    let watcher = runtime.watcher(1);

    let request = &runtime.requests("acp_watch")[0];
    assert_eq!(request["id"], SESSION);
    assert_eq!(request["include_debug"], false);
    assert_eq!(request["image_refs"], true);
    assert_eq!(request["tail_limit"], 100);
    assert!(
        request.get("known_entries").is_none(),
        "没有缓存不带续传参数"
    );
    assert!(request.get("history_session_id").is_none());

    let mut list = vec![AcpEntry::User("你好".into()), assistant("第一句")];
    watcher.full(snapshot_json(0, 2, 1, &list));
    let event = events.snapshot_where("整段帧", |_| true);
    assert!(!event.cached);
    assert_eq!(event.saved_at_ms, None);
    assert_eq!(texts(&event), ["你好", "第一句"]);

    list[1] = assistant("第一句，第二句");
    list.push(assistant("新的一条"));
    let mut next = snapshot_json(0, 3, 2, &list);
    next["phase"] = "thinking".into();
    next["status_line"] = "正在写".into();
    watcher.delta(next);
    let event = events.snapshot_where("增量帧", |event| texts(event).len() == 3);
    assert_eq!(texts(&event), ["你好", "第一句，第二句", "新的一条"]);
    let view = event.view.unwrap();
    assert_eq!(
        view.phase,
        crate::api::conversation::ConversationPhase::Thinking
    );
    assert_eq!(view.status_line.as_deref(), Some("正在写"));
    assert!(runtime.actions().is_empty(), "镜像对得上不该要整段帧");
}

#[test]
fn a_mirror_gap_asks_for_one_refresh_and_waits_for_a_full_frame() {
    let (runtime, _source, conversations) = setup();
    let events = watch(&conversations);
    let watcher = runtime.watcher(1);
    watcher.full(snapshot_json(0, 1, 1, &[assistant("a")]));
    events.snapshot_where("首帧", |_| true);

    // 改的是镜像里没有的条目：接不上。
    let gap = json!({"delta": {"entries": {"from": 5, "end": 6, "keep_from": 0,
        "patches": [{"index": 5, "keep": 3, "tail": "xyz"}]}}});
    watcher.send(&gap);
    watcher.send(&gap);
    wait_until("发出 Refresh", || !runtime.actions().is_empty());
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        runtime.actions(),
        [json!("Refresh")],
        "等整段帧期间不重复要"
    );
    let refresh = &runtime.requests("acp_action")[0];
    assert_eq!(refresh["id"], SESSION);

    watcher.full(snapshot_json(0, 2, 3, &[assistant("a"), assistant("b")]));
    let event = events.snapshot_where("整段帧补上", |event| texts(event).len() == 2);
    assert_eq!(texts(&event), ["a", "b"]);

    watcher.send(&gap);
    wait_until("整段帧之后再接不上就再要", || {
        runtime.actions().len() == 2
    });
    assert!(
        !watcher.client_wrote.load(Ordering::SeqCst),
        "Refresh 不能写在 watch 连接上"
    );
}

#[test]
fn unavailable_is_reported_and_the_watch_stops() {
    let (runtime, _source, conversations) = setup();
    let events = watch(&conversations);
    let watcher = runtime.watcher(1);
    watcher.send(&json!({"unavailable": {"reason": "电脑上没开 Smelt"}}));
    watcher.close();
    let event = events.next();
    assert_eq!(event.kind, ConversationEventKind::Unavailable);
    assert_eq!(event.message.as_deref(), Some("电脑上没开 Smelt"));
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(runtime.watch_count(), 1, "接不上就不再重订");
}

#[test]
fn the_cache_is_shown_first_and_resumes_the_watch() {
    let runtime = FakeRuntime::new();
    let cache = Arc::new(Cache::in_memory());
    cache.save_snapshot(ENDPOINT, SESSION, &snapshot(0, 2, 7, &["a", "b"]));
    let source = FakeSource::connected(&runtime, Some(Arc::clone(&cache)));
    let conversations = Conversations::new(source, FAST);
    let events = watch(&conversations);

    let first = events.next();
    assert_eq!(first.kind, ConversationEventKind::Snapshot);
    assert!(first.cached);
    assert!(first.saved_at_ms.is_some());
    assert_eq!(texts(&first), ["a", "b"]);

    let watcher = runtime.watcher(1);
    let request = &runtime.requests("acp_watch")[0];
    assert_eq!(request["history_session_id"], "h1");
    assert_eq!(request["snapshot_revision"], 7);
    assert_eq!(request["known_entries"], 2);

    // 运行时从 known_entries 接着发。
    watcher.full(snapshot_json(2, 3, 8, &[assistant("c")]));
    let event = events.snapshot_where("实时数据", |event| !event.cached);
    assert_eq!(texts(&event), ["a", "b", "c"]);
    assert_eq!(event.saved_at_ms, None);

    wait_until("实时数据存进缓存", || {
        cache
            .load_snapshot::<ConversationSnapshot>(ENDPOINT, SESSION)
            .is_some_and(|cached| cached.value.snapshot_revision == 8)
    });
    let saved = cache
        .load_snapshot::<ConversationSnapshot>(ENDPOINT, SESSION)
        .unwrap()
        .value;
    assert_eq!(saved.entries.len(), 3);
}

#[test]
fn a_cache_that_does_not_reach_the_end_resumes_without_known_entries() {
    let runtime = FakeRuntime::new();
    let cache = Arc::new(Cache::in_memory());
    cache.save_snapshot(ENDPOINT, SESSION, &snapshot(0, 5, 7, &["a", "b"]));
    let source = FakeSource::connected(&runtime, Some(cache));
    let conversations = Conversations::new(source, FAST);
    let _events = watch(&conversations);
    runtime.watcher(1);
    let request = &runtime.requests("acp_watch")[0];
    assert_eq!(request["history_session_id"], "h1");
    assert_eq!(request["snapshot_revision"], 7);
    assert!(request.get("known_entries").is_none());
}

#[test]
fn only_the_last_page_is_cached() {
    let runtime = FakeRuntime::new();
    let cache = Arc::new(Cache::in_memory());
    let source = FakeSource::connected(&runtime, Some(Arc::clone(&cache)));
    let conversations = Conversations::new(source, FAST);
    let events = watch(&conversations);
    let all = labels(0..150);
    runtime
        .watcher(1)
        .full(snapshot_json(0, 150, 4, &entries(&all)));
    events.snapshot_where("整段帧", |_| true);
    wait_until("存缓存", || {
        cache
            .load_snapshot::<ConversationSnapshot>(ENDPOINT, SESSION)
            .is_some()
    });
    let saved = cache
        .load_snapshot::<ConversationSnapshot>(ENDPOINT, SESSION)
        .unwrap()
        .value;
    assert_eq!(saved.entries_offset, 50);
    assert_eq!(saved.entries.len(), 100);
    assert_eq!(saved.entries_total, 150);
}

#[test]
fn load_older_merges_the_previous_page_into_the_stream() {
    let (runtime, _source, conversations) = setup();
    let events = watch(&conversations);
    let tail = labels(100..102);
    runtime
        .watcher(1)
        .full(snapshot_json(100, 102, 5, &entries(&tail)));
    let event = events.snapshot_where("尾部", |_| true);
    let view = event.view.unwrap();
    assert!(view.has_more_before);
    assert_eq!(view.entries_offset, 100);

    runtime.respond(|request| match request["op"].as_str() {
        Some("acp_snapshot") => Reply::Line(json!({
            "snapshot": snapshot_json(0, 102, 5, &entries(&labels(0..100))),
        })),
        _ => Reply::Close,
    });
    assert_eq!(conversations.load_older(SESSION), Ok(true));
    let request = &runtime.requests("acp_snapshot")[0];
    assert_eq!(request["id"], SESSION);
    assert_eq!(request["before"], 100);
    assert_eq!(request["limit"], 100);
    assert_eq!(request["include_debug"], false);
    assert_eq!(request["image_refs"], true);

    let event = events.snapshot_where("合并后的窗口", |event| texts(event).len() == 102);
    assert_eq!(texts(&event), labels(0..102));
    let view = event.view.unwrap();
    assert!(!view.has_more_before);
    assert_eq!(view.entries_offset, 0);

    assert_eq!(conversations.load_older(SESSION), Ok(false), "到头了");
    assert_eq!(
        conversations.load_older("acp-other"),
        Ok(false),
        "不是当前订阅"
    );
}

#[test]
fn a_page_from_another_history_is_dropped() {
    let (runtime, _source, conversations) = setup();
    let events = watch(&conversations);
    runtime
        .watcher(1)
        .full(snapshot_json(10, 11, 5, &[assistant("tail")]));
    events.snapshot_where("尾部", |_| true);
    runtime.respond(|_| {
        let mut page = snapshot_json(0, 11, 5, &entries(&labels(0..10)));
        page["history_session_id"] = "h2".into();
        Reply::Line(json!({ "snapshot": page }))
    });
    assert_eq!(conversations.load_older(SESSION), Ok(false));
}

#[test]
fn a_history_id_change_resets_the_window() {
    let (runtime, _source, conversations) = setup();
    let events = watch(&conversations);
    let watcher = runtime.watcher(1);
    watcher.full(snapshot_json(0, 3, 1, &entries(&labels(0..3))));
    events.snapshot_where("旧对话", |_| true);
    let mut fresh = snapshot_json(2, 3, 2, &[assistant("新对话")]);
    fresh["history_session_id"] = "h2".into();
    watcher.full(fresh);
    let event = events.snapshot_where("新对话", |event| texts(event) == ["新对话"]);
    assert_eq!(event.view.unwrap().entries_offset, 2);
}

#[test]
fn a_dropped_watch_is_resumed_from_what_we_have() {
    let (runtime, _source, conversations) = setup();
    let events = watch(&conversations);
    let first = runtime.watcher(1);
    first.full(snapshot_json(0, 2, 3, &[assistant("a"), assistant("b")]));
    events.snapshot_where("首帧", |_| true);
    first.close();

    let second = runtime.watcher(2);
    let request = &runtime.requests("acp_watch")[1];
    assert_eq!(request["known_entries"], 2);
    assert_eq!(request["snapshot_revision"], 3);
    assert_eq!(request["history_session_id"], "h1");
    second.full(snapshot_json(2, 3, 4, &[assistant("c")]));
    let event = events.snapshot_where("续上", |event| texts(event).len() == 3);
    assert_eq!(texts(&event), ["a", "b", "c"]);
}

#[test]
fn losing_the_link_drops_the_watch_until_it_is_back() {
    let (runtime, source, conversations) = setup();
    let events = watch(&conversations);
    let first = runtime.watcher(1);
    first.full(snapshot_json(0, 1, 1, &[assistant("a")]));
    events.snapshot_where("首帧", |_| true);

    source.publish(1, ConnectionState::Reconnecting);
    wait_until("旧链路上的 watch 被掐掉", || {
        first.closed.load(Ordering::SeqCst)
    });
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(runtime.watch_count(), 1, "没连上之前不重订");

    source.publish(1, ConnectionState::Connected);
    runtime.watcher(2);
    assert!(!first.client_wrote.load(Ordering::SeqCst));
}

#[test]
fn a_new_generation_resumes_the_watch_on_the_new_link() {
    let (runtime, source, conversations) = setup();
    let events = watch(&conversations);
    let first = runtime.watcher(1);
    first.full(snapshot_json(0, 2, 3, &[assistant("a"), assistant("b")]));
    events.snapshot_where("首帧", |_| true);

    // 同一台机器重新连过（重连、endpoint 换形态）：先换代进入 Connecting，不在新链路连上前重订。
    source.publish(2, ConnectionState::Connecting);
    wait_until("旧一代的 watch 被掐掉", || {
        first.closed.load(Ordering::SeqCst)
    });
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(runtime.watch_count(), 1, "新的一代没连上之前不重订");

    source.publish(2, ConnectionState::Connected);
    let second = runtime.watcher(2);
    let request = &runtime.requests("acp_watch")[1];
    assert_eq!(request["known_entries"], 2, "带着手里的窗口续订");
    second.full(snapshot_json(2, 3, 4, &[assistant("c")]));
    let event = events.snapshot_where("续上", |event| texts(event).len() == 3);
    assert_eq!(texts(&event), ["a", "b", "c"]);
    assert!(!first.client_wrote.load(Ordering::SeqCst));
}

#[test]
fn a_stopped_connection_stops_the_watch() {
    let (runtime, source, conversations) = setup();
    let events = watch(&conversations);
    let first = runtime.watcher(1);
    first.full(snapshot_json(0, 1, 1, &[assistant("a")]));
    events.snapshot_where("首帧", |_| true);

    source.publish(2, ConnectionState::Disconnected);
    wait_until("watch 关掉", || first.closed.load(Ordering::SeqCst));
    source.publish(2, ConnectionState::Connected);
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(runtime.watch_count(), 1, "停下就不再续订");
}

#[test]
fn unwatch_closes_without_writing_and_flushes_the_cache() {
    let runtime = FakeRuntime::new();
    let cache = Arc::new(Cache::in_memory());
    let source = FakeSource::connected(&runtime, Some(Arc::clone(&cache)));
    // 存缓存要等很久：只有 unwatch 时的立即落盘能让下面的断言成立。
    let timing = Timing {
        save_delay: Duration::from_secs(600),
        ..FAST
    };
    let conversations = Conversations::new(source, timing);
    let events = watch(&conversations);
    let watcher = runtime.watcher(1);
    watcher.full(snapshot_json(0, 1, 1, &[assistant("a")]));
    events.snapshot_where("首帧", |_| true);

    conversations.unwatch(SESSION, events.1);
    wait_until("watch 关掉", || watcher.closed.load(Ordering::SeqCst));
    assert!(!watcher.client_wrote.load(Ordering::SeqCst));
    assert!(
        cache
            .load_snapshot::<ConversationSnapshot>(ENDPOINT, SESSION)
            .is_some()
    );
    assert!(events.quiet().is_empty(), "停了就不再推");
}

#[test]
fn several_sessions_are_watched_at_once_and_unwatched_one_by_one() {
    let (runtime, _source, conversations) = setup();
    let a = watch_session(&conversations, "acp-a");
    let first = runtime.watcher(1);
    let b = watch_session(&conversations, "acp-b");
    let second = runtime.watcher(2);
    let ids: Vec<Value> = runtime
        .requests("acp_watch")
        .iter()
        .map(|request| request["id"].clone())
        .collect();
    assert_eq!(ids, [json!("acp-a"), json!("acp-b")]);

    first.full(snapshot_json(0, 1, 1, &[assistant("给 a")]));
    second.full(snapshot_json(0, 1, 1, &[assistant("给 b")]));
    let a_view = a.snapshot_where("a 的帧", |_| true);
    let b_view = b.snapshot_where("b 的帧", |_| true);
    assert_eq!(a_view.view.as_ref().unwrap().session_id, "acp-a");
    assert_eq!(texts(&a_view), ["给 a"]);
    assert_eq!(b_view.view.as_ref().unwrap().session_id, "acp-b");
    assert_eq!(texts(&b_view), ["给 b"]);

    conversations.unwatch("acp-a", a.1);
    wait_until("a 的 watch 关掉", || first.closed.load(Ordering::SeqCst));
    assert!(!second.closed.load(Ordering::SeqCst), "b 不受影响");
    second.full(snapshot_json(
        0,
        2,
        2,
        &[assistant("给 b"), assistant("还在")],
    ));
    b.snapshot_where("b 还在推", |event| texts(event).len() == 2);
    assert!(a.quiet().is_empty(), "a 停了就不再推");
}

#[test]
fn watching_the_same_session_again_shares_the_watch() {
    let (runtime, _source, conversations) = setup();
    let old = watch(&conversations);
    let first = runtime.watcher(1);
    first.full(snapshot_json(0, 1, 1, &[assistant("a")]));
    old.snapshot_where("首帧", |_| true);

    let new = watch(&conversations);
    let replayed = new.snapshot_where("新出口马上拿到手里的视图", |_| true);
    assert_eq!(texts(&replayed), ["a"]);
    assert!(!replayed.cached);
    std::thread::sleep(Duration::from_millis(100));
    assert!(!first.closed.load(Ordering::SeqCst), "不重开 watch");
    assert_eq!(runtime.watch_count(), 1);

    first.full(snapshot_json(0, 2, 2, &[assistant("a"), assistant("b")]));
    new.snapshot_where("后续帧推给新出口", |event| texts(event).len() == 2);
    old.snapshot_where("旧出口也照常收到", |event| texts(event).len() == 2);
}

/// 同一会话的页面换下来：新页先订，旧页后退。旧页的退订不能把新页的订阅一起停掉。
#[test]
fn the_old_page_leaving_keeps_the_new_page_updated() {
    let runtime = FakeRuntime::new();
    let cache = Arc::new(Cache::in_memory());
    let source = FakeSource::connected(&runtime, Some(Arc::clone(&cache)));
    let timing = Timing {
        save_delay: Duration::from_secs(600),
        ..FAST
    };
    let conversations = Conversations::new(source, timing);
    let old = watch(&conversations);
    let first = runtime.watcher(1);
    first.full(snapshot_json(0, 1, 1, &[assistant("a")]));
    old.snapshot_where("首帧", |_| true);
    let new = watch(&conversations);
    new.snapshot_where("补推", |_| true);
    // 补推同一份视图也会推给旧出口，先收掉。
    old.quiet();

    conversations.unwatch(SESSION, old.1);
    std::thread::sleep(Duration::from_millis(100));
    assert!(!first.closed.load(Ordering::SeqCst), "还有订阅者，watch 不停");
    first.full(snapshot_json(0, 2, 2, &[assistant("a"), assistant("b")]));
    new.snapshot_where("新页照常更新", |event| texts(event).len() == 2);
    assert!(old.quiet().is_empty(), "退了的出口不再推");

    // 编号已经退过：再退一次不碰新页。
    conversations.unwatch(SESSION, old.1);
    std::thread::sleep(Duration::from_millis(50));
    assert!(!first.closed.load(Ordering::SeqCst));

    conversations.unwatch(SESSION, new.1);
    wait_until("最后一个退了才关", || first.closed.load(Ordering::SeqCst));
    assert!(
        cache
            .load_snapshot::<ConversationSnapshot>(ENDPOINT, SESSION)
            .is_some(),
        "最后一个退订时落盘"
    );
}

#[test]
fn one_cancelled_stream_does_not_stop_the_others() {
    let (runtime, _source, conversations) = setup();
    let events = watch(&conversations);
    let first = runtime.watcher(1);
    conversations.watch(SESSION.to_string(), next_watch_id(), Box::new(|_| false));
    first.full(snapshot_json(0, 1, 1, &[assistant("a")]));
    events.snapshot_where("首帧", |_| true);
    first.full(snapshot_json(0, 2, 2, &[assistant("a"), assistant("b")]));
    events.snapshot_where("取消了的出口摘掉，别的照推", |event| {
        texts(event).len() == 2
    });
    assert!(!first.closed.load(Ordering::SeqCst));
}

#[test]
fn watching_a_stopped_session_again_starts_a_new_watch() {
    let (runtime, _source, conversations) = setup();
    conversations.watch(SESSION.to_string(), next_watch_id(), Box::new(|_| false));
    let first = runtime.watcher(1);
    first.full(snapshot_json(0, 1, 1, &[assistant("a")]));
    wait_until("Dart 取消后 watch 关掉", || {
        first.closed.load(Ordering::SeqCst)
    });
    let _events = watch(&conversations);
    runtime.watcher(2);
}

#[test]
fn close_stops_every_watch_without_saving() {
    let runtime = FakeRuntime::new();
    let cache = Arc::new(Cache::in_memory());
    let source = FakeSource::connected(&runtime, Some(Arc::clone(&cache)));
    let timing = Timing {
        save_delay: Duration::from_secs(600),
        ..FAST
    };
    let conversations = Conversations::new(source, timing);
    let a = watch_session(&conversations, "acp-a");
    let first = runtime.watcher(1);
    let _b = watch_session(&conversations, "acp-b");
    let second = runtime.watcher(2);
    first.full(snapshot_json(0, 1, 1, &[assistant("a")]));
    a.snapshot_where("首帧", |_| true);

    conversations.close();
    wait_until("全部关掉", || {
        first.closed.load(Ordering::SeqCst) && second.closed.load(Ordering::SeqCst)
    });
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        cache
            .load_snapshot::<ConversationSnapshot>(ENDPOINT, "acp-a")
            .is_none(),
        "机器要被删了，不再写缓存"
    );
}

#[test]
fn dart_cancelling_the_stream_stops_the_watch() {
    let (runtime, _source, conversations) = setup();
    conversations.watch(SESSION.to_string(), next_watch_id(), Box::new(|_| false));
    let watcher = runtime.watcher(1);
    watcher.full(snapshot_json(0, 1, 1, &[assistant("a")]));
    wait_until("watch 关掉", || watcher.closed.load(Ordering::SeqCst));
}

#[test]
fn watching_without_a_runtime_shows_the_cache_then_an_error() {
    let cache = Arc::new(Cache::in_memory());
    cache.save_snapshot(ENDPOINT, SESSION, &snapshot(0, 1, 1, &["a"]));
    let conversations = Conversations::new(FakeSource::disconnected(Some(cache)), FAST);
    let events = watch(&conversations);
    let first = events.next();
    assert!(first.cached);
    let error = events.next();
    assert_eq!(error.kind, ConversationEventKind::Error);
    assert_eq!(error.message.as_deref(), Some("这台设备还没有配对"));
}

#[test]
fn a_watch_the_runtime_keeps_refusing_reports_one_error_and_retries() {
    let (runtime, _source, conversations) = setup();
    // 运行时读完 watch 请求就关：每次都没有帧。
    let events = watch(&conversations);
    let first = runtime.watcher(1);
    first.close();
    let error = events.next();
    assert_eq!(error.kind, ConversationEventKind::Error);
    runtime.watcher(2).close();
    runtime.watcher(3);
    assert!(
        events
            .quiet()
            .iter()
            .all(|event| event.kind != ConversationEventKind::Error),
        "同一段失败只报一次"
    );
}

// ---------------------------------------------------------------------------
// 动作
// ---------------------------------------------------------------------------

#[test]
fn actions_use_the_acp_action_wire_format() {
    let (runtime, _source, conversations) = setup();
    conversations.cancel_turn(SESSION).unwrap();
    conversations
        .set_config_option(SESSION, "model".into(), "openai/gpt".into(), None)
        .unwrap();
    conversations
        .set_config_option(SESSION, "fast".into(), String::new(), Some(true))
        .unwrap();
    conversations
        .respond_approval(SESSION, "t1".into(), "allow".into())
        .unwrap();
    conversations.choose_elicitation(SESSION, 1, 2).unwrap();
    conversations
        .update_elicitation_text(SESSION, 0, "草稿".into())
        .unwrap();
    conversations
        .submit_elicitation(
            SESSION,
            vec![ElicitationTextInput {
                field_index: 3,
                value: "小明".into(),
            }],
        )
        .unwrap();
    conversations.dismiss_elicitation(SESSION).unwrap();
    assert_eq!(
        runtime.actions(),
        [
            json!("Cancel"),
            json!({"SetConfigOption": {"config_id": "model", "value_id": "openai/gpt", "boolean": null}}),
            json!({"SetConfigOption": {"config_id": "fast", "value_id": "", "boolean": true}}),
            json!({"PermissionSelect": {"tool_call_id": "t1", "option_id": "allow"}}),
            json!({"ElicitationChoose": {"field_ix": 1, "opt_ix": 2}}),
            json!({"ElicitationText": {"field_ix": 0, "value": "草稿"}}),
            json!({"ElicitationText": {"field_ix": 3, "value": "小明"}}),
            json!("ElicitationSubmit"),
            json!("ElicitationDismiss"),
        ]
    );
    assert!(
        runtime
            .requests("acp_action")
            .iter()
            .all(|request| request["id"] == SESSION)
    );
}

#[test]
fn a_refused_action_reports_the_runtime_error() {
    let (runtime, _source, conversations) = setup();
    runtime.respond(|_| {
        Reply::Line(json!({"ok": false, "error": "automation Run action is daemon-owned"}))
    });
    let failure = conversations.cancel_turn(SESSION).unwrap_err();
    assert_eq!(failure.kind, SmeltErrorKind::Other);
    assert_eq!(failure.message, "automation Run action is daemon-owned");

    runtime.online.store(false, Ordering::SeqCst);
    assert_eq!(
        conversations.cancel_turn(SESSION).unwrap_err().kind,
        SmeltErrorKind::Offline
    );
}

#[test]
fn actions_without_a_runtime_are_offline() {
    let conversations = Conversations::new(FakeSource::disconnected(None), FAST);
    assert_eq!(
        conversations.cancel_turn(SESSION).unwrap_err().kind,
        SmeltErrorKind::Offline
    );
}

#[test]
fn send_message_reuses_the_request_id_as_submission_id() {
    let (runtime, _source, conversations) = setup();
    runtime.respond(|_| Reply::Line(json!({"ok": true})));
    let send = || {
        conversations.send_message(
            SESSION,
            "req-1",
            "看看这张图".into(),
            vec![ImageInput {
                mime: "image/png".into(),
                bytes: b"png".to_vec(),
            }],
        )
    };
    send().unwrap();
    send().unwrap();
    let requests = runtime.requests("acp_submit_input");
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request["id"], SESSION);
        assert_eq!(
            request["input"],
            json!({
                "submission_id": "req-1",
                "text": "看看这张图",
                "images": [{"mime": "image/png", "data_b64": BASE64.encode(b"png")}],
            })
        );
    }
}

#[test]
fn send_failures_say_whether_a_retry_is_safe() {
    let (runtime, _source, conversations) = setup();
    let send = || conversations.send_message(SESSION, "req-1", "hi".into(), Vec::new());

    runtime.respond(|_| {
        Reply::Line(
            json!({"ok": false, "error": {"kind": "rejected", "message": "ACP session not found"}}),
        )
    });
    let failure = send().unwrap_err();
    assert_eq!(failure.kind, SendFailureKind::Rejected);
    assert_eq!(failure.message, "ACP session not found");

    runtime.respond(|_| {
        Reply::Line(json!({"ok": false, "error": {"kind": "unknown", "message": "host lost"}}))
    });
    assert_eq!(send().unwrap_err().kind, SendFailureKind::Unknown);

    runtime.respond(|_| Reply::Close);
    assert_eq!(
        send().unwrap_err().kind,
        SendFailureKind::Unknown,
        "发出去了但没等到确认"
    );

    runtime.online.store(false, Ordering::SeqCst);
    assert_eq!(
        send().unwrap_err().kind,
        SendFailureKind::Rejected,
        "连接都没建起来，请求没发出去"
    );

    assert_eq!(
        conversations
            .send_message(SESSION, "req-2", "  ".into(), Vec::new())
            .unwrap_err()
            .kind,
        SendFailureKind::Rejected
    );
    let offline = Conversations::new(FakeSource::disconnected(None), FAST);
    assert_eq!(
        offline
            .send_message(SESSION, "req-1", "hi".into(), Vec::new())
            .unwrap_err()
            .kind,
        SendFailureKind::Rejected
    );
}

#[test]
fn skills_images_and_pending_actions() {
    let (runtime, _source, conversations) = setup();
    let image = BASE64.encode(b"jpeg-bytes");
    runtime.respond(move |request| match request["op"].as_str() {
        Some("acp_skills") => Reply::Line(json!({
            "ok": true, "agent": "pi", "supported": true,
            "skills": [{"name": "review", "description": "代码审查"}, {"name": "bare"}],
        })),
        Some("control") => {
            assert_eq!(request["method"], "conversation.image");
            assert_eq!(
                request["params"],
                json!({"session_id": SESSION, "entry": 3, "digest": "abc", "max_edge": 320})
            );
            let request_id = request["request_id"].as_str().unwrap();
            Reply::Line(
                serde_json::to_value(ControlResponse::ok(
                    request_id,
                    json!({"mime": "image/jpeg", "data_b64": image}),
                ))
                .unwrap(),
            )
        }
        Some("acp_snapshot") => {
            let mut snapshot = snapshot_json(0, 1, 1, &[assistant("a")]);
            snapshot["phase"] = "awaiting_approval".into();
            snapshot["pending_permissions"] = json!([{
                "question": "跑命令？", "tool_call_id": "t1",
                "options": [{"option_id": "a", "name": "允许", "kind": "AllowOnce"}],
                "details": {"kind": "command", "command": "ls", "cwd": null, "reason": null},
            }]);
            Reply::Line(json!({ "snapshot": snapshot }))
        }
        _ => Reply::Close,
    });

    let skills = conversations.list_skills(SESSION).unwrap();
    assert!(skills.supported);
    assert_eq!(skills.skills.len(), 2);
    assert_eq!(skills.skills[0].description, "代码审查");
    assert_eq!(skills.skills[1].description, "");

    let loaded = conversations
        .load_image(SESSION, 3, "abc".into(), Some(320))
        .unwrap();
    assert_eq!(loaded.mime, "image/jpeg");
    assert_eq!(loaded.bytes, b"jpeg-bytes");
    assert!(
        conversations
            .load_image(SESSION, -1, "abc".into(), None)
            .is_err()
    );

    let pending = conversations.pending_actions(SESSION).unwrap();
    assert_eq!(pending.permissions.len(), 1);
    assert_eq!(pending.permissions[0].tool_call_id, "t1");
    assert!(pending.elicitation.is_none());
    let request = &runtime.requests("acp_snapshot")[0];
    assert_eq!(request["before"].as_u64(), Some(usize::MAX as u64));
    assert_eq!(request["limit"], 8);
    assert_eq!(request["image_refs"], true);
}

#[test]
fn snapshot_errors_are_passed_through() {
    let (runtime, _source, conversations) = setup();
    runtime.respond(|_| Reply::Line(json!({"ok": false, "error": "ACP 会话不存在"})));
    assert_eq!(
        conversations.pending_actions(SESSION).unwrap_err().message,
        "ACP 会话不存在"
    );
    runtime.respond(|_| Reply::Line(json!({"ok": true, "supported": false, "skills": []})));
    assert!(!conversations.list_skills(SESSION).unwrap().supported);
}

#[test]
fn pending_actions_carry_the_last_few_entries_as_context() {
    let (runtime, _source, conversations) = setup();
    runtime.respond(|request| match request["op"].as_str() {
        Some("acp_snapshot") => {
            // 快照头 8 条，前面还有 40 条：单据只印最后 6 条，序号是整段对话里的序号。
            let entries: Vec<_> = (0..8).map(|i| assistant(&format!("e{i}"))).collect();
            Reply::Line(json!({ "snapshot": snapshot_json(40, 48, 1, &entries) }))
        }
        _ => Reply::Close,
    });

    let pending = conversations.pending_actions(SESSION).unwrap();
    assert_eq!(pending.recent.len(), RECENT_CONTEXT);
    assert_eq!(pending.recent.first().unwrap().index, 42);
    assert_eq!(pending.recent.last().unwrap().index, 47);
    assert_eq!(pending.recent.last().unwrap().text, "e7");
}
