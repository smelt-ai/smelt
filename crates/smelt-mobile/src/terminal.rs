//! 一台机器上的终端：附着终端会话的字节流、输入与尺寸。Dart 入口见 `crate::api::terminal`。
//! 机队里每个节点一份（见 `crate::fleet`）。
//!
//! 一次附着是一条 `watch` 长连接（`controls_geometry: true`）：
//!
//! ```text
//! 客户端 → {"op":"watch","id","controls_geometry":true,"cols","rows","cell_w","cell_h","max_scrollback_lines"?}\n
//! 运行时 → {"cols","rows","replay_len","history_lines"}\n  + replay_len 字节 ANSI 关键帧 + 原始 PTY 字节流
//! 客户端 → [1][len=16 u32 BE][cols][rows][cell_w][cell_h]（各 u32 BE）  只能写这种 resize 帧
//! ```
//!
//! 这条连接同时是 PTY 尺寸租约：断开后运行时进入宽限期。输入另走短连接
//! `{"op":"input","id","data"}` → `{"ok":true}` / `{"ok":false,"err"}`。
//!
//! 每个附着两条线程：读线程负责附着、读流、断线重连；输入线程按顺序发输入。
//!
//! 断线判别：iroh 桥在传输出错时也是关掉本地 socket，客户端读到的同样是 EOF，分不出
//! 「运行时关的」还是「链路断了」。所以 EOF 一律按断线处理、退避后重新附着；重新附着时
//! 运行时在头行前就关连接、且运行时本身可达（`system.describe` 能回），才说明会话没了：
//! 附着过的发 `Closed`，从没附着上的发 fatal `Error`。

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use smelt_runtime_client::protocol::control::{ControlClientError, EmptyParams, SystemDescribe};
use smelt_runtime_client::protocol::daemon::DaemonOperation;
use smelt_runtime_client::{RuntimeClient, RuntimeStream};

use crate::api::error::{SmeltError, SmeltErrorKind, error};
use crate::api::fleet::ConnectionState;
use crate::api::terminal::TerminalGeometry;
use crate::connection::{ConnectionEvent, Listener};

/// 运行时的上限（smeltd `MAX_SESSION_COLS` / `MAX_SESSION_ROWS` / `MAX_SESSION_CELL_PX`）。
const MAX_COLS: u32 = 1000;
const MAX_ROWS: u32 = 1000;
const MAX_CELL_PX: u32 = 256;
/// 运行时 `SNAPSHOT_MAX_LINES`：不传预算时就给这么多。
const MAX_SCROLLBACK_LINES: u32 = 10_000;
/// 关键帧上限。超过说明运行时回包不对，不为它分配内存、也不往界面灌。
const MAX_REPLAY_BYTES: u64 = 16 * 1024 * 1024;
/// 头行上限。正常头行几十字节。
const MAX_HEADER_BYTES: u64 = 4096;
/// `Data` 事件一块的上限，也是读缓冲大小。
const CHUNK_BYTES: usize = 64 * 1024;
/// 一次 `input` 请求的上限（旧网关同值）。
const MAX_INPUT_BYTES: usize = 64 * 1024;

/// 读写线程拿这台机器的连接用。生产用节点的 `NodeSource`（见 `crate::fleet`），测试注入
/// 假的。
pub(crate) trait RuntimeSource: Send + Sync {
    /// 当前连接的代号和客户端。
    fn current(&self) -> Option<(u64, RuntimeClient)>;
    /// 监听连接状态，语义同 `ConnectionHub::subscribe`。
    fn subscribe(&self, listener: Listener);
}

/// 各种时限。生产值见 [`Timing::PRODUCTION`]，测试用毫秒级。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// 写完 watch 首帧后等头行、以及读关键帧时每次读的时限。
    pub header_timeout: Duration,
    /// 附着连接上写 watch 首帧与 resize 帧的时限。resize 在 Dart UI 线程上写，要短。
    pub write_timeout: Duration,
    /// 一次 `input` 请求的读写时限。
    pub input_timeout: Duration,
    /// 头行前被关时探运行时是否可达的时限。
    pub probe_timeout: Duration,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
}

impl Timing {
    pub(crate) const PRODUCTION: Self = Self {
        header_timeout: Duration::from_secs(15),
        write_timeout: Duration::from_secs(1),
        input_timeout: Duration::from_secs(10),
        probe_timeout: Duration::from_secs(5),
        backoff_base: Duration::from_millis(500),
        backoff_max: Duration::from_secs(8),
    };

    /// 连续失败 `failures`（≥1）次后，下一次重试前等多久。
    fn backoff(&self, failures: u32) -> Duration {
        let factor = 1u32 << failures.saturating_sub(1).min(16);
        self.backoff_base
            .saturating_mul(factor)
            .min(self.backoff_max)
    }
}

/// 附着的事件。对 Dart 平铺成 [`TerminalEvent`]（见 `crate::api::terminal`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalUpdate {
    /// 附着成功（含自动重新附着）。之后先来 `replay_bytes` 字节的关键帧（`Data`），
    /// 再来 `ReplayComplete`，然后是实时输出。每次 `Ready` 都是一整帧新画面，界面要重建终端。
    Ready {
        /// 运行时 PTY 当前的列数，按它建终端再喂关键帧。
        cols: u32,
        rows: u32,
        /// 关键帧字节数。为 0 时紧跟 `ReplayComplete`。
        replay_bytes: u32,
        /// 会话侧总共有多少行可取。大于 `scrollback_lines` 说明上面还有更老的内容。
        history_lines: u32,
        /// 这次附着实际要的 scrollback 行数（没传预算时是运行时的上限 10000）。
        scrollback_lines: u32,
    },
    /// 终端字节：关键帧和实时输出都走这个，一块不超过 64 KB。关键帧与实时输出不会混在同一块里。
    Data(Vec<u8>),
    /// 关键帧放完了。
    ReplayComplete,
    /// 终端会话已结束（shell 退出或被关掉）。流随之结束。
    Closed,
    /// `fatal` 时流随之结束：会话不存在、连接停下（未配对、机器被忘记）、运行时回包不对。
    /// 非 fatal 是断线或输入发送失败，后台会按退避自动重新附着，成功后再来 `Ready`。
    Error { message: String, fatal: bool },
}

/// 推事件给 Dart，返回 false 表示对面已取消订阅。丢掉它就结束 Dart 侧的流。
pub(crate) type Emit = Box<dyn Fn(TerminalUpdate) -> bool + Send + Sync>;

pub(crate) struct TerminalHub {
    inner: Arc<HubInner>,
}

struct HubInner {
    source: Arc<dyn RuntimeSource>,
    timing: Timing,
    /// 会话 id → 当前附着。锁顺序：本锁在前、附着的状态锁在后；拿着它不调 `source`
    /// （连接状态机在它的锁里回调我们的监听）。
    attachments: Mutex<HashMap<String, Arc<Attachment>>>,
}

impl HubInner {
    fn attachments(&self) -> MutexGuard<'_, HashMap<String, Arc<Attachment>>> {
        self.attachments.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn get(&self, session_id: &str) -> Result<Arc<Attachment>, SmeltError> {
        self.attachments().get(session_id).cloned().ok_or_else(|| {
            error(
                SmeltErrorKind::Other,
                format!("没有附着终端会话 {session_id}"),
            )
        })
    }

    /// 从表里摘掉，前提是表里还是它（可能已被新的附着替换）。
    fn forget(&self, attachment: &Arc<Attachment>) {
        let mut attachments = self.attachments();
        if attachments
            .get(&attachment.session_id)
            .is_some_and(|current| Arc::ptr_eq(current, attachment))
        {
            attachments.remove(&attachment.session_id);
        }
    }

    /// 连接状态变化。在连接状态机的锁里调用：只改内存、推事件，不阻塞。
    fn on_connection(&self, generation: u64, event: &ConnectionEvent) {
        let attachments: Vec<_> = self.attachments().values().cloned().collect();
        for attachment in attachments {
            match attachment.generation() {
                // 连接停下（未配对、被忘记、主动断开）：终端跟着停。
                Some(_) if event.state == ConnectionState::Disconnected => {
                    attachment.fail(RUNTIME_GONE);
                }
                // 同一台机器重新连过：旧一代链路上的附着连接掐掉，新一代连上后在新连接上重新附着。
                Some(bound) if bound != generation => attachment.follow(generation),
                // 重新连上：别等退避，立刻重新附着。
                Some(_) if event.state == ConnectionState::Connected => attachment.retry_now(),
                _ => {}
            }
        }
    }

    /// 这台机器当前连接的客户端。从没连上过就让附着以 fatal 收尾；附着过、新一代正在连
    /// （换代时状态机先撤掉旧连接再连）就等它连上。被叫停返回 None。
    fn runtime_for(&self, attachment: &Attachment) -> Option<RuntimeClient> {
        loop {
            match self.source.current() {
                // 记下这次用的代号：之后换代由监听掐掉这条连接、叫读线程重新附着。监听已经
                // 跟到更新的一代时手里这个客户端是旧的，重新取。
                Some((generation, client)) => {
                    if attachment.bind(generation) {
                        return Some(client);
                    }
                }
                None if attachment.generation().is_none() => {
                    attachment.fail(NO_RUNTIME);
                    return None;
                }
                // 连上时监听叫醒；连接停下由监听收尾。
                None => {
                    if let Wake::Stop = attachment.wait(self.timing.backoff_max) {
                        return None;
                    }
                }
            }
        }
    }

    /// 读线程：附着、读流、断线后退避重连，直到被叫停或得出结论。
    fn run(&self, attachment: &Arc<Attachment>) {
        let mut failures = 0u32;
        let mut attached = false;
        loop {
            let Some(client) = self.runtime_for(attachment) else {
                break;
            };
            match self.attempt(attachment, &client) {
                Attempt::Stopped => break,
                Attempt::NoSession => {
                    if attached {
                        attachment.close();
                    } else {
                        attachment.fail("终端会话不存在或已结束");
                    }
                    break;
                }
                Attempt::Fatal(message) => {
                    attachment.fail(&message);
                    break;
                }
                Attempt::Dropped(reason) => {
                    attached = true;
                    failures = 1;
                    if let Some(reason) = reason {
                        attachment.notify_error(format!("终端连接断开：{reason}"));
                    }
                }
                Attempt::Failed(reason) => {
                    failures = failures.saturating_add(1);
                    attachment.notify_error(reason);
                }
            }
            match attachment.wait(self.timing.backoff(failures)) {
                Wake::Stop => break,
                Wake::Now => failures = 0,
                Wake::Timeout => {}
            }
        }
        self.forget(attachment);
    }

    /// 一次附着：连、写首帧、读头行和关键帧，再读实时流直到断开。
    fn attempt(&self, attachment: &Attachment, client: &RuntimeClient) -> Attempt {
        let stream = match client.connect() {
            Ok(stream) => stream,
            Err(error) => return Attempt::Failed(format!("连不上运行时：{error}")),
        };
        let configured = stream
            .set_read_timeout(Some(self.timing.header_timeout))
            .and_then(|()| stream.set_write_timeout(Some(self.timing.write_timeout)));
        if let Err(error) = configured {
            return Attempt::Failed(format!("终端连接不可用：{error}"));
        }
        let scrollback_lines = match attachment.start(&stream) {
            Ok(Some(lines)) => lines,
            Ok(None) => return Attempt::Stopped,
            Err(error) => return Attempt::Failed(format!("终端附着请求没发出去：{error}")),
        };
        let outcome = self.read_stream(attachment, client, stream, scrollback_lines);
        attachment.finish();
        if attachment.is_stopped() {
            return Attempt::Stopped;
        }
        outcome
    }

    fn read_stream(
        &self,
        attachment: &Attachment,
        client: &RuntimeClient,
        stream: RuntimeStream,
        scrollback_lines: u32,
    ) -> Attempt {
        let mut reader = BufReader::with_capacity(CHUNK_BYTES, stream);
        let header = match read_header(&mut reader) {
            Ok(Some(header)) => header,
            Ok(None) => {
                if attachment.is_stopped() {
                    return Attempt::Stopped;
                }
                // 头行前就关：会话不存在，或者链路断了。运行时能应答才算前者。
                return if reachable(client, self.timing.probe_timeout) {
                    Attempt::NoSession
                } else {
                    Attempt::Failed("连不上运行时".to_string())
                };
            }
            Err(HeaderError::Io(error)) => {
                return Attempt::Failed(format!("等终端头失败：{error}"));
            }
            Err(HeaderError::Invalid(message)) => return Attempt::Fatal(message),
        };
        if header.replay_len > MAX_REPLAY_BYTES {
            return Attempt::Fatal(format!(
                "终端关键帧太大（{} 字节），超过 {} 字节上限",
                header.replay_len, MAX_REPLAY_BYTES
            ));
        }
        let ready = TerminalUpdate::Ready {
            cols: header.cols,
            rows: header.rows,
            replay_bytes: header.replay_len as u32,
            history_lines: u32::try_from(header.history_lines).unwrap_or(u32::MAX),
            scrollback_lines,
        };
        if !attachment.emit(ready) {
            return Attempt::Stopped;
        }

        let mut buf = vec![0u8; CHUNK_BYTES];
        // 关键帧：每次读不越过它的末尾，`ReplayComplete` 正好落在它和实时输出之间。
        let mut remaining = header.replay_len as usize;
        while remaining > 0 {
            let want = remaining.min(CHUNK_BYTES);
            match read_some(&mut reader, &mut buf[..want]) {
                Ok(0) => return Attempt::Dropped(Some("关键帧没传完".to_string())),
                Ok(n) => {
                    remaining -= n;
                    if !attachment.emit(TerminalUpdate::Data(buf[..n].to_vec())) {
                        return Attempt::Stopped;
                    }
                }
                Err(error) => return Attempt::Dropped(Some(error.to_string())),
            }
        }
        if !attachment.emit(TerminalUpdate::ReplayComplete) {
            return Attempt::Stopped;
        }

        // 实时流可以长时间没有输出，不设读时限。链路死了由 iroh 的空闲超时关掉本地 socket。
        if let Err(error) = reader.get_ref().set_read_timeout(None) {
            return Attempt::Dropped(Some(error.to_string()));
        }
        loop {
            match read_some(&mut reader, &mut buf) {
                Ok(0) => return Attempt::Dropped(None),
                Ok(n) => {
                    if !attachment.emit(TerminalUpdate::Data(buf[..n].to_vec())) {
                        return Attempt::Stopped;
                    }
                }
                Err(error) => return Attempt::Dropped(Some(error.to_string())),
            }
        }
    }

    /// 输入线程：按顺序把排队的输入发出去。
    fn pump_input(&self, attachment: &Attachment) {
        while let Some(chunk) = attachment.next_input() {
            let result = match self.source.current() {
                Some((_, client)) => send_input(
                    &client,
                    &attachment.session_id,
                    &chunk,
                    self.timing.input_timeout,
                ),
                None => Err(RUNTIME_GONE.to_string()),
            };
            if let Err(message) = result {
                attachment.input_failed(format!("终端输入没有发出：{message}"));
            }
        }
    }
}

const NO_RUNTIME: &str = "还没有连上这台机器";
const RUNTIME_GONE: &str = "和这台机器的连接已停止，终端已停止";

impl TerminalHub {
    pub(crate) fn new(source: Arc<dyn RuntimeSource>, timing: Timing) -> Self {
        let inner = Arc::new(HubInner {
            source: Arc::clone(&source),
            timing,
            attachments: Mutex::new(HashMap::new()),
        });
        // 弱引用：监听存在连接状态机里，不该反过来吊住终端表。
        let weak: Weak<HubInner> = Arc::downgrade(&inner);
        source.subscribe(Box::new(move |generation, event| {
            let Some(inner) = weak.upgrade() else {
                return false;
            };
            inner.on_connection(generation, event);
            true
        }));
        Self { inner }
    }

    /// 附着（替换同一会话的旧附着）。只登记、起线程，不做网络 IO。
    pub(crate) fn attach(
        &self,
        session_id: String,
        geometry: TerminalGeometry,
        max_scrollback_lines: Option<u32>,
        emit: Emit,
    ) -> Result<(), SmeltError> {
        if session_id.is_empty() {
            return Err(error(SmeltErrorKind::Other, "终端会话 id 为空"));
        }
        let geometry = normalize(geometry)?;
        let attachment = Arc::new(Attachment {
            session_id: session_id.clone(),
            emit,
            state: Mutex::new(State {
                stopped: false,
                retry_now: false,
                generation: None,
                writer: None,
                desired: geometry,
                sent: None,
                max_scrollback_lines: max_scrollback_lines
                    .map(|lines| lines.clamp(1, MAX_SCROLLBACK_LINES)),
                input: String::new(),
            }),
            wake: Condvar::new(),
        });
        let replaced = self
            .inner
            .attachments()
            .insert(session_id, Arc::clone(&attachment));
        if let Some(replaced) = replaced {
            replaced.stop();
        }

        let reader = {
            let inner = Arc::clone(&self.inner);
            let attachment = Arc::clone(&attachment);
            std::thread::Builder::new()
                .name("smelt-terminal".to_string())
                .spawn(move || inner.run(&attachment))
        };
        let writer = reader.and_then(|_| {
            let inner = Arc::clone(&self.inner);
            let attachment = Arc::clone(&attachment);
            std::thread::Builder::new()
                .name("smelt-terminal-input".to_string())
                .spawn(move || inner.pump_input(&attachment))
        });
        if let Err(spawn_error) = writer {
            attachment.stop();
            self.inner.forget(&attachment);
            return Err(error(
                SmeltErrorKind::Other,
                format!("起不了终端线程：{spawn_error}"),
            ));
        }
        Ok(())
    }

    pub(crate) fn resize(
        &self,
        session_id: &str,
        geometry: TerminalGeometry,
    ) -> Result<(), SmeltError> {
        let geometry = normalize(geometry)?;
        self.inner.get(session_id)?.resize(geometry);
        Ok(())
    }

    pub(crate) fn input(&self, session_id: &str, data: String) -> Result<(), SmeltError> {
        if data.is_empty() {
            return Ok(());
        }
        let attachment = self.inner.get(session_id)?;
        if attachment.queue_input(&data) {
            Ok(())
        } else {
            Err(error(SmeltErrorKind::Other, "终端已断开"))
        }
    }

    pub(crate) fn detach(&self, session_id: &str) {
        let removed = self.inner.attachments().remove(session_id);
        if let Some(attachment) = removed {
            attachment.stop();
        }
    }

    /// 断开全部附着（切后台、机器被忘记、节点被替换时）。
    pub(crate) fn detach_all(&self) {
        let removed: Vec<_> = self.inner.attachments().drain().map(|(_, a)| a).collect();
        for attachment in removed {
            attachment.stop();
        }
    }
}

/// 一次 [`TerminalHub::attach`]：从登记到被叫停（detach、替换、Dart 取消订阅）或得出结论
/// （会话结束、连接停下、fatal）。
struct Attachment {
    session_id: String,
    emit: Emit,
    state: Mutex<State>,
    /// 读线程等退避、输入线程等输入，共用一个。
    wake: Condvar,
}

struct State {
    stopped: bool,
    /// 运行时刚重新连上，读线程别等退避了。
    retry_now: bool,
    /// 当前附着用的连接代号。没拿到过连接时为空。
    generation: Option<u64>,
    /// 当前附着连接的写端（同一 socket 的 dup），写 resize 帧、叫停时 shutdown。
    writer: Option<RuntimeStream>,
    /// 界面要的尺寸；重新附着按它来。
    desired: TerminalGeometry,
    /// 当前连接上最后发出的尺寸。
    sent: Option<TerminalGeometry>,
    max_scrollback_lines: Option<u32>,
    /// 排队待发的输入。
    input: String,
}

enum Wake {
    Stop,
    Now,
    Timeout,
}

enum Attempt {
    /// 被叫停。
    Stopped,
    /// 拿到头行之后断了。`None` 是读到 EOF。
    Dropped(Option<String>),
    /// 没拿到头行就失败了，可重试。
    Failed(String),
    /// 运行时在头行前就关了连接，而它本身可达：会话不存在。
    NoSession,
    /// 重试也没用。
    Fatal(String),
}

impl Attachment {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn generation(&self) -> Option<u64> {
        self.lock().generation
    }

    /// 记下这次用的代号。监听已经跟到更新的一代时不倒退，返回 false。
    fn bind(&self, generation: u64) -> bool {
        let mut state = self.lock();
        if state.generation.is_some_and(|bound| bound > generation) {
            return false;
        }
        state.generation = Some(generation);
        true
    }

    /// 连接换代：掐掉旧链路上的附着连接（读线程随之返回），并让它别等退避立刻重新附着。
    fn follow(&self, generation: u64) {
        let mut state = self.lock();
        if state.stopped {
            return;
        }
        state.generation = Some(generation);
        state.retry_now = true;
        if let Some(writer) = state.writer.take() {
            let _ = writer.shutdown(Shutdown::Both);
        }
        state.sent = None;
        drop(state);
        self.wake.notify_all();
    }

    fn is_stopped(&self) -> bool {
        self.lock().stopped
    }

    /// 推事件。在状态锁里推：叫停之后保证不再有事件出去。
    fn emit(&self, event: TerminalUpdate) -> bool {
        let mut state = self.lock();
        if state.stopped {
            return false;
        }
        if (self.emit)(event) {
            return true;
        }
        // Dart 已取消订阅。
        stop_locked(&mut state);
        drop(state);
        self.wake.notify_all();
        false
    }

    fn notify_error(&self, message: String) {
        self.emit(TerminalUpdate::Error {
            message,
            fatal: false,
        });
    }

    fn stop(&self) {
        stop_locked(&mut self.lock());
        self.wake.notify_all();
    }

    /// 以 fatal 错误收尾。已经叫停的不再推。
    fn fail(&self, message: &str) {
        self.conclude(TerminalUpdate::Error {
            message: message.to_string(),
            fatal: true,
        });
    }

    fn close(&self) {
        self.conclude(TerminalUpdate::Closed);
    }

    fn conclude(&self, event: TerminalUpdate) {
        let mut state = self.lock();
        if !state.stopped {
            let _ = (self.emit)(event);
            stop_locked(&mut state);
        }
        drop(state);
        self.wake.notify_all();
    }

    fn retry_now(&self) {
        self.lock().retry_now = true;
        self.wake.notify_all();
    }

    fn wait(&self, delay: Duration) -> Wake {
        let state = self.lock();
        let (mut state, _) = self
            .wake
            .wait_timeout_while(state, delay, |s| !s.stopped && !s.retry_now)
            .unwrap_or_else(|e| e.into_inner());
        if state.stopped {
            Wake::Stop
        } else if std::mem::take(&mut state.retry_now) {
            Wake::Now
        } else {
            Wake::Timeout
        }
    }

    /// 登记新连接并写 watch 首帧。返回这次的 scrollback 预算；已叫停时返回 None。
    ///
    /// 首帧在状态锁里写：并发的 resize 只能排在它后面，不会抢在首帧前写进连接。
    fn start(&self, stream: &RuntimeStream) -> io::Result<Option<u32>> {
        let mut state = self.lock();
        if state.stopped {
            let _ = stream.shutdown(Shutdown::Both);
            return Ok(None);
        }
        state.retry_now = false;
        let geometry = state.desired;
        let request = watch_request(&self.session_id, geometry, state.max_scrollback_lines);
        smelt_runtime_client::protocol::write_json_line(&mut &*stream, &request)?;
        state.writer = Some(stream.try_clone()?);
        state.sent = Some(geometry);
        Ok(Some(
            state.max_scrollback_lines.unwrap_or(MAX_SCROLLBACK_LINES),
        ))
    }

    /// 连接结束，放掉写端。
    fn finish(&self) {
        let mut state = self.lock();
        state.writer = None;
        state.sent = None;
    }

    fn resize(&self, geometry: TerminalGeometry) {
        let mut state = self.lock();
        state.desired = geometry;
        if state.stopped || state.sent == Some(geometry) {
            return;
        }
        let Some(writer) = state.writer.as_ref() else {
            // 正在重连：下次附着按新尺寸来。
            return;
        };
        match (&*writer).write_all(&resize_frame(geometry)) {
            Ok(()) => state.sent = Some(geometry),
            Err(error) => {
                // 写不进去说明连接坏了：关掉它让读线程重新附着，新尺寸随附着带过去。
                log::info!("终端 resize 帧写失败，重新附着：{error}");
                if let Some(writer) = state.writer.take() {
                    let _ = writer.shutdown(Shutdown::Both);
                }
                state.sent = None;
            }
        }
    }

    /// 排队一段输入，返回 false 表示附着已停。
    fn queue_input(&self, data: &str) -> bool {
        let mut state = self.lock();
        if state.stopped {
            return false;
        }
        state.input.push_str(data);
        drop(state);
        self.wake.notify_all();
        true
    }

    /// 取下一块要发的输入：在途期间攒下的合并发送，单块不超过 64 KB。叫停后返回 None。
    fn next_input(&self) -> Option<String> {
        let state = self.lock();
        let mut state = self
            .wake
            .wait_while(state, |s| !s.stopped && s.input.is_empty())
            .unwrap_or_else(|e| e.into_inner());
        if state.stopped {
            return None;
        }
        Some(take_chunk(&mut state.input, MAX_INPUT_BYTES))
    }

    /// 输入发送失败：丢掉还没发的（断线后再补发旧按键比丢掉更危险），并告诉界面。
    fn input_failed(&self, message: String) {
        self.lock().input.clear();
        self.notify_error(message);
    }
}

fn stop_locked(state: &mut State) {
    state.stopped = true;
    state.input.clear();
    if let Some(writer) = state.writer.take() {
        // 读线程的 read 随之返回；运行时读到 EOF，归还几何租约（进入宽限期）。
        let _ = writer.shutdown(Shutdown::Both);
    }
}

fn normalize(geometry: TerminalGeometry) -> Result<TerminalGeometry, SmeltError> {
    if geometry.cols == 0 || geometry.rows == 0 {
        return Err(error(SmeltErrorKind::Other, "终端行列必须大于 0"));
    }
    Ok(TerminalGeometry {
        cols: geometry.cols.min(MAX_COLS),
        rows: geometry.rows.min(MAX_ROWS),
        cell_width: geometry.cell_width.min(MAX_CELL_PX),
        cell_height: geometry.cell_height.min(MAX_CELL_PX),
    })
}

fn watch_request(
    session_id: &str,
    geometry: TerminalGeometry,
    max_scrollback_lines: Option<u32>,
) -> Value {
    let mut request = json!({
        "op": DaemonOperation::Watch,
        "id": session_id,
        "controls_geometry": true,
        "cols": geometry.cols,
        "rows": geometry.rows,
        "cell_w": geometry.cell_width,
        "cell_h": geometry.cell_height,
    });
    if let Some(lines) = max_scrollback_lines {
        request["max_scrollback_lines"] = lines.into();
    }
    request
}

/// `[1][len=16 u32 BE][cols][rows][cell_w][cell_h]`，各 u32 BE。
fn resize_frame(geometry: TerminalGeometry) -> [u8; 21] {
    let mut frame = [0u8; 21];
    frame[0] = 1;
    frame[1..5].copy_from_slice(&16u32.to_be_bytes());
    frame[5..9].copy_from_slice(&geometry.cols.to_be_bytes());
    frame[9..13].copy_from_slice(&geometry.rows.to_be_bytes());
    frame[13..17].copy_from_slice(&geometry.cell_width.to_be_bytes());
    frame[17..21].copy_from_slice(&geometry.cell_height.to_be_bytes());
    frame
}

#[derive(Debug, Deserialize)]
struct WatchHeader {
    cols: u32,
    rows: u32,
    replay_len: u64,
    #[serde(default)]
    history_lines: u64,
}

enum HeaderError {
    Io(io::Error),
    Invalid(String),
}

/// 读头行。运行时在头行前就关了连接时返回 None。
fn read_header(reader: &mut BufReader<RuntimeStream>) -> Result<Option<WatchHeader>, HeaderError> {
    let mut line = Vec::new();
    loop {
        match reader
            .by_ref()
            .take(MAX_HEADER_BYTES)
            .read_until(b'\n', &mut line)
        {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(HeaderError::Io(error)),
        }
    }
    if line.is_empty() {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(if line.len() as u64 >= MAX_HEADER_BYTES {
            HeaderError::Invalid("运行时回的终端头太长".to_string())
        } else {
            HeaderError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "终端头没读完连接就断了",
            ))
        });
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|e| HeaderError::Invalid(format!("运行时回的终端头不对：{e}")))
}

fn read_some(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match reader.read(buf) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

/// 运行时能不能应答。回了错误也算能应答。
fn reachable(client: &RuntimeClient, timeout: Duration) -> bool {
    !matches!(
        client.call::<SystemDescribe>(&EmptyParams::default(), timeout),
        Err(ControlClientError::Transport(_))
    )
}

/// 从队首取不超过 `max` 字节，切在 UTF-8 字符边界上。
fn take_chunk(buf: &mut String, max: usize) -> String {
    if buf.len() <= max {
        return std::mem::take(buf);
    }
    let mut end = max;
    while !buf.is_char_boundary(end) {
        end -= 1;
    }
    let rest = buf.split_off(end);
    std::mem::replace(buf, rest)
}

fn send_input(
    client: &RuntimeClient,
    session_id: &str,
    data: &str,
    timeout: Duration,
) -> Result<(), String> {
    let request = json!({
        "op": DaemonOperation::Input,
        "id": session_id,
        "data": data,
    });
    let response = client
        .request(&request, timeout)
        .map_err(|error| error.to_string())?;
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    // 终端 op 的失败字段叫 `err`，不是 `error`。
    Err(response
        .get("err")
        .and_then(Value::as_str)
        .unwrap_or("运行时拒绝了终端输入")
        .to_string())
}

#[cfg(test)]
mod tests;
