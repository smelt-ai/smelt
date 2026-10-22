//! 一台机器上的对话：订阅会话的快照流、翻更早的页、发送与各类动作、取图片。Dart 入口见
//! `crate::api::conversation`。机队里每个节点一份（见 `crate::fleet`），同一节点上可以同时
//! 订阅多个会话（平板双栏、逐张处理单据时预载下一张），按会话 id 各自独立。
//!
//! 同一会话可以有几个订阅者（同一会话的页面换下来时新页先订、旧页后退），各带调用方给的
//! 订阅编号：共用一条 watch，每个推送出口都收到视图；退订只摘自己的出口，最后一个摘掉
//! 才停。
//!
//! 订阅（`acp_watch`）是只读长连接：首帧整段快照，之后是整段帧或相对上一帧的增量帧，
//! 用 [`FrameMirror`] 还原。客户端只写首行请求，之后**不能往这条连接写任何东西，也不能
//! 半关闭写方向**（经 iroh 传过去的 EOF 会让运行时结束 watch）；停的时候 shutdown 整条
//! socket。镜像对不上增量帧时另开连接发 `Refresh`，等下一份整段帧。
//!
//! 一个订阅三条线：
//! - 读线程：等运行时连上 → 建 watch → 读帧、合并进窗口；断了就退避重订。
//! - 推送线程：窗口变了按间隔合并推一次视图；变化后过一会儿把尾部存进离线缓存。
//! - 连接状态监听（在状态机的锁里回调）：掉线或换代（同一台机器重新连过）就掐断当前
//!   watch，免得挂在一条已经死掉的旧链路上，等重新连上再带着手里的窗口续订；连接停下
//!   （未配对、被忘记）就停。
//!
//! 运行时、连接状态与离线缓存经 [`RuntimeSource`] 注入，测试用假运行时。

mod merge;
mod view;

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use smelt_model::acp_chat::AcpImage;
use smelt_model::acp_session::{AcpUserAction, ConversationSnapshot};
use smelt_model::acp_wire::{FrameDelta, FrameMirror};
use smelt_model::control_api::{ConversationImage, ConversationImageParams};
use smelt_model::conversation::{ConversationInput, ConversationSubmitErrorKind};
use smelt_model::session_control::parse_conversation_submit_response;
use smelt_runtime_client::protocol::control::{ControlClientError, ControlErrorCode};
use smelt_runtime_client::protocol::daemon::DaemonOperation;
use smelt_runtime_client::protocol::write_json_line;
use smelt_runtime_client::{RequestError, RuntimeClient};

use crate::api::conversation::{
    ConversationEntry, ConversationEvent, ConversationEventKind, ElicitationTextInput, ImageData,
    ImageInput, PendingActions, SendFailureKind, SendMessageError, SessionSkill, SessionSkills,
};
use crate::api::error::{SmeltError, SmeltErrorKind, error};
use crate::api::fleet::ConnectionState;
use crate::cache::Cache;
use crate::connection::{ConnectionEvent, CurrentRuntime, Listener};

/// 订阅时要的尾部条数，也是往前翻一页的条数和缓存里留的条数。
const PAGE_LIMIT: usize = 100;
/// 指挥台取待办只要快照头，条目带几条就够。
const PENDING_ACTIONS_LIMIT: usize = 8;
/// 单据上印几条上下文。取自待办那份快照头（[`PENDING_ACTIONS_LIMIT`] 条）的末尾。
pub(crate) const RECENT_CONTEXT: usize = 6;
/// 动作、翻页可能要先唤醒挂起的会话，比普通短请求宽一些。
const ACTION_TIMEOUT: Duration = Duration::from_secs(15);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(15);
const SKILLS_TIMEOUT: Duration = Duration::from_secs(10);
/// 发送等运行时确认的上限，和桌面一致。
const SUBMIT_TIMEOUT: Duration = Duration::from_secs(35);
const IMAGE_TIMEOUT: Duration = Duration::from_secs(30);

/// 这台机器的连接、连接状态与离线缓存的来源。生产用节点的 `NodeSource`（见
/// `crate::fleet`），测试注入假的。
pub(crate) trait RuntimeSource: Send + Sync {
    fn current(&self) -> Option<CurrentRuntime>;
    /// 见 `ConnectionHub::subscribe`：先同步回调一次当前状态。
    fn subscribe(&self, listener: Listener);
    fn cache(&self) -> Option<Arc<Cache>>;
}

/// 各种时限。生产值见 [`Timing::PRODUCTION`]，测试用毫秒级。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// watch 断了之后重订的退避。
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    /// 两次推视图的最小间隔（条目多时按比例放宽）。
    pub emit_interval: Duration,
    /// 窗口变化后多久存一次缓存。流式输出期间最多这么久存一次。
    pub save_delay: Duration,
}

impl Timing {
    pub(crate) const PRODUCTION: Self = Self {
        backoff_base: Duration::from_secs(1),
        backoff_max: Duration::from_secs(15),
        emit_interval: Duration::from_millis(50),
        save_delay: Duration::from_secs(1),
    };

    fn backoff(&self, failures: u32) -> Duration {
        let factor = 1u32 << failures.saturating_sub(1).min(16);
        self.backoff_base
            .saturating_mul(factor)
            .min(self.backoff_max)
    }

    /// 每次推的是整个窗口：条目越多，Dart 解码越贵，间隔按每 200 条一倍放宽，最多十倍。
    fn emit_interval_for(&self, entries: usize) -> Duration {
        let factor = (entries / 200).clamp(1, 10) as u32;
        self.emit_interval.saturating_mul(factor)
    }
}

/// 推给 Dart 的事件出口。返回 false 表示 Dart 已取消订阅。
pub(crate) type EventSink = Box<dyn Fn(ConversationEvent) -> bool + Send>;

pub(crate) struct Conversations {
    source: Arc<dyn RuntimeSource>,
    timing: Timing,
    /// 会话 id → 订阅。停下的订阅（运行时接不上、Dart 取消、连接停下）留在表里直到被替换
    /// 或 unwatch，查到时按已停处理。
    watches: Mutex<HashMap<String, Arc<Watch>>>,
}

impl Conversations {
    pub(crate) fn new(source: Arc<dyn RuntimeSource>, timing: Timing) -> Self {
        Self {
            source,
            timing,
            watches: Mutex::new(HashMap::new()),
        }
    }

    fn watches(&self) -> MutexGuard<'_, HashMap<String, Arc<Watch>>> {
        self.watches.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 订阅一个会话，`watch_id` 是调用方给这个订阅者的编号，退订时原样带回。已经在订（且
    /// 没停）时只挂上新出口，把手里的视图补推一次；否则新开一个订阅。不阻塞在网络上（缓存
    /// 读写除外）。
    pub(crate) fn watch(&self, session_id: String, watch_id: u32, sink: EventSink) {
        // 查和插在同一把表锁下：同一会话同时来两次订阅也只开一条 watch。
        let (watch, previous) = {
            let mut watches = self.watches();
            let sink = match watches.get(&session_id) {
                Some(existing) => match existing.add_sink(watch_id, sink) {
                    Ok(()) => return,
                    Err(sink) => sink,
                },
                None => sink,
            };
            let watch = Arc::new(Watch::new(session_id.clone(), watch_id, sink, self.timing));
            let previous = watches.insert(session_id, Arc::clone(&watch));
            (watch, previous)
        };
        let cache = self.source.cache();
        if let Some(previous) = previous {
            previous.stop();
            previous.flush(cache.as_deref());
        }

        // 首次回调同步发生：记下当前代号和运行时。
        let weak = Arc::downgrade(&watch);
        self.source.subscribe(Box::new(move |generation, event| {
            weak.upgrade()
                .is_some_and(|watch| watch.on_connection(generation, event))
        }));

        let endpoint_id = watch.lock().endpoint_id.clone();
        if let (Some(cache), Some(endpoint_id)) = (&cache, &endpoint_id)
            && let Some(cached) =
                cache.load_snapshot::<ConversationSnapshot>(endpoint_id, &watch.session_id)
        {
            let mut snapshot = cached.value;
            merge::normalize(&mut snapshot);
            let event =
                snapshot_event(&watch.session_id, &snapshot, true, Some(cached.saved_at_ms));
            {
                let mut state = watch.lock();
                state.snapshot = Some(snapshot);
                state.saved_at_ms = Some(cached.saved_at_ms);
            }
            watch.emit(event);
        }

        let stopped = {
            let state = watch.lock();
            state.stopped.then(|| {
                state
                    .stop_reason
                    .clone()
                    .unwrap_or_else(|| "没有连接这台机器".to_string())
            })
        };
        if let Some(reason) = stopped {
            watch.emit(message_event(ConversationEventKind::Error, reason));
            return;
        }

        let reader = {
            let watch = Arc::clone(&watch);
            let source = Arc::clone(&self.source);
            std::thread::Builder::new()
                .name("smelt-conversation-watch".to_string())
                .spawn(move || watch.run_reader(source.as_ref()))
        };
        let publisher = reader.and_then(|_| {
            let watch = Arc::clone(&watch);
            std::thread::Builder::new()
                .name("smelt-conversation-publish".to_string())
                .spawn(move || watch.run_publisher(cache))
        });
        if let Err(spawn_error) = publisher {
            watch.stop();
            watch.emit(message_event(
                ConversationEventKind::Error,
                format!("起不了订阅线程：{spawn_error}"),
            ));
        }
    }

    /// 摘掉一个订阅者。同一会话还有别的订阅者时订阅照常；这是最后一个就停掉这个会话的
    /// 订阅，其它会话不受影响。编号对不上（已经摘过）时不动别人的出口。
    pub(crate) fn unwatch(&self, session_id: &str, watch_id: u32) {
        let previous = {
            let mut watches = self.watches();
            let Some(watch) = watches.get(session_id) else {
                return;
            };
            if !watch.remove_sink(watch_id) {
                return;
            }
            watches.remove(session_id)
        };
        if let Some(previous) = previous {
            previous.flush(self.source.cache().as_deref());
        }
    }

    /// 撤掉这台机器时：停掉全部订阅，手里没存的数据也不再写缓存（缓存随后整台删掉）。
    pub(crate) fn close(&self) {
        let watches: Vec<_> = self.watches().drain().map(|(_, watch)| watch).collect();
        for watch in watches {
            watch.discard();
        }
    }

    fn active_watch(&self, session_id: &str) -> Option<Arc<Watch>> {
        self.watches().get(session_id).cloned()
    }

    fn client(&self) -> Result<RuntimeClient, SmeltError> {
        self.source
            .current()
            .map(|runtime| runtime.client)
            .ok_or_else(|| error(SmeltErrorKind::Offline, "没有连接这台机器"))
    }

    pub(crate) fn load_older(&self, session_id: &str) -> Result<bool, SmeltError> {
        let Some(watch) = self.active_watch(session_id) else {
            return Ok(false);
        };
        let (before, history) = {
            let mut state = watch.lock();
            let Some(snapshot) = state.snapshot.as_ref() else {
                return Ok(false);
            };
            if state.stopped || state.loading_older || snapshot.entries_offset == 0 {
                return Ok(false);
            }
            let before = snapshot.entries_offset;
            let history = merge::stable_history_id(snapshot).map(str::to_string);
            state.loading_older = true;
            (before, history)
        };
        let page = self
            .client()
            .and_then(|client| history_page(&client, session_id, before, PAGE_LIMIT));
        let mut state = watch.lock();
        state.loading_older = false;
        let page = page?;
        if state.stopped {
            return Ok(false);
        }
        // 翻页期间换了一段对话（重新开始）：这一页不属于手里的窗口。
        if let (Some(current), Some(paged)) = (history.as_deref(), merge::stable_history_id(&page))
            && current != paged
        {
            return Ok(false);
        }
        let Some(current) = state.snapshot.take() else {
            return Ok(false);
        };
        state.snapshot = Some(merge::merge(current, page));
        state.emit_pending = true;
        drop(state);
        watch.wake.notify_all();
        Ok(true)
    }

    pub(crate) fn pending_actions(&self, session_id: &str) -> Result<PendingActions, SmeltError> {
        let client = self.client()?;
        let snapshot = history_page(&client, session_id, usize::MAX, PENDING_ACTIONS_LIMIT)?;
        Ok(PendingActions {
            permissions: view::permissions(&snapshot.pending_permissions),
            elicitation: snapshot.pending_elicitation.as_ref().map(view::elicitation),
            recent: recent_context(&snapshot),
        })
    }

    pub(crate) fn send_message(
        &self,
        session_id: &str,
        request_id: &str,
        text: String,
        images: Vec<ImageInput>,
    ) -> Result<(), SendMessageError> {
        if request_id.trim().is_empty() {
            return Err(send_error(SendFailureKind::Rejected, "缺少请求 id"));
        }
        if text.trim().is_empty() && images.is_empty() {
            return Err(send_error(SendFailureKind::Rejected, "消息是空的"));
        }
        let client = self
            .source
            .current()
            .map(|runtime| runtime.client)
            .ok_or_else(|| send_error(SendFailureKind::Rejected, "没有连接这台机器"))?;
        // submission_id 用客户端的请求 id：重发同一份草稿时运行时能认出是同一次提交。
        let input = ConversationInput {
            submission_id: request_id.to_string(),
            text,
            images: images
                .into_iter()
                .map(|image| AcpImage::new(image.mime, BASE64.encode(image.bytes)))
                .collect(),
        };
        let request = json!({
            "op": DaemonOperation::AcpSubmitInput,
            "id": session_id,
            "input": input,
        });
        submit(&client, &request)
    }

    fn action(&self, session_id: &str, action: &AcpUserAction) -> Result<(), SmeltError> {
        action_on(&self.client()?, session_id, action)
    }

    pub(crate) fn cancel_turn(&self, session_id: &str) -> Result<(), SmeltError> {
        self.action(session_id, &AcpUserAction::Cancel)
    }

    pub(crate) fn set_config_option(
        &self,
        session_id: &str,
        config_id: String,
        value_id: String,
        boolean: Option<bool>,
    ) -> Result<(), SmeltError> {
        self.action(
            session_id,
            &AcpUserAction::SetConfigOption {
                config_id,
                value_id,
                boolean,
            },
        )
    }

    pub(crate) fn respond_approval(
        &self,
        session_id: &str,
        tool_call_id: String,
        option_id: String,
    ) -> Result<(), SmeltError> {
        self.action(
            session_id,
            &AcpUserAction::PermissionSelect {
                tool_call_id,
                option_id,
            },
        )
    }

    pub(crate) fn choose_elicitation(
        &self,
        session_id: &str,
        field_index: u32,
        option_index: u32,
    ) -> Result<(), SmeltError> {
        self.action(
            session_id,
            &AcpUserAction::ElicitationChoose {
                field_ix: field_index as usize,
                opt_ix: option_index as usize,
            },
        )
    }

    pub(crate) fn update_elicitation_text(
        &self,
        session_id: &str,
        field_index: u32,
        value: String,
    ) -> Result<(), SmeltError> {
        self.action(
            session_id,
            &AcpUserAction::ElicitationText {
                field_ix: field_index as usize,
                value,
            },
        )
    }

    /// 先按顺序写文字框再提交，同一个客户端串行发，运行时按这个顺序处理。
    pub(crate) fn submit_elicitation(
        &self,
        session_id: &str,
        text_values: Vec<ElicitationTextInput>,
    ) -> Result<(), SmeltError> {
        let client = self.client()?;
        for input in text_values {
            action_on(
                &client,
                session_id,
                &AcpUserAction::ElicitationText {
                    field_ix: input.field_index as usize,
                    value: input.value,
                },
            )?;
        }
        action_on(&client, session_id, &AcpUserAction::ElicitationSubmit)
    }

    pub(crate) fn dismiss_elicitation(&self, session_id: &str) -> Result<(), SmeltError> {
        self.action(session_id, &AcpUserAction::ElicitationDismiss)
    }

    pub(crate) fn list_skills(&self, session_id: &str) -> Result<SessionSkills, SmeltError> {
        let client = self.client()?;
        let request = json!({ "op": DaemonOperation::AcpSkills, "id": session_id });
        let response = client
            .request(&request, SKILLS_TIMEOUT)
            .map_err(request_error)?;
        ok_response(&response)?;
        let skills = response
            .get("skills")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|skill| {
                Some(SessionSkill {
                    name: skill.get("name")?.as_str()?.to_string(),
                    description: skill
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                })
            })
            .collect();
        Ok(SessionSkills {
            supported: response
                .get("supported")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            skills,
        })
    }

    pub(crate) fn load_image(
        &self,
        session_id: &str,
        entry: i64,
        digest: String,
        max_edge: Option<u32>,
    ) -> Result<ImageData, SmeltError> {
        let entry = u64::try_from(entry)
            .map_err(|_| error(SmeltErrorKind::Other, "图片引用的条目下标无效"))?;
        let client = self.client()?;
        let image = client
            .call::<ConversationImage>(
                &ConversationImageParams {
                    session_id: session_id.to_string(),
                    entry,
                    digest,
                    max_edge,
                },
                IMAGE_TIMEOUT,
            )
            .map_err(control_error)?;
        let bytes = BASE64
            .decode(image.data_b64.as_bytes())
            .map_err(|e| error(SmeltErrorKind::Other, format!("图片数据无效：{e}")))?;
        Ok(ImageData {
            mime: image.mime,
            bytes,
        })
    }
}

// ---------------------------------------------------------------------------
// 订阅
// ---------------------------------------------------------------------------

struct Watch {
    session_id: String,
    timing: Timing,
    state: Mutex<WatchState>,
    wake: Condvar,
    /// 各订阅者的推送出口，带订阅编号。全空了订阅就停。和状态锁一起拿时先拿状态锁。
    sinks: Mutex<Vec<(u32, EventSink)>>,
}

#[derive(Default)]
struct WatchState {
    stopped: bool,
    /// 停下的原因（连接断开时状态机给的），没连上就开始订阅时报给界面。
    stop_reason: Option<String>,
    /// 正在跟的连接代号。换代（同一台机器重新连过）就掐掉旧链路上的 watch，跟上新的一代。
    generation: Option<u64>,
    endpoint_id: Option<String>,
    /// 这一代连接当前是 Connected。
    connected: bool,
    /// 刚从别的状态回到 Connected：退避等待立即结束。
    reconnected: bool,
    /// 正在读的 watch 连接，留一份给别的线程 shutdown 打断阻塞的读。
    conn: Option<UnixStream>,
    /// 手里的连续窗口（实时尾部 + 翻出来的更早页），线格式。
    snapshot: Option<ConversationSnapshot>,
    /// 收到过运行时的实时数据。之前的窗口只是离线缓存。
    live: bool,
    saved_at_ms: Option<i64>,
    emit_pending: bool,
    save_due: Option<Instant>,
    loading_older: bool,
}

enum StreamEnd {
    /// 运行时说接不上这段对话。
    Unavailable(String),
    /// 连接断了或建不起来。`frames` 是这次收到的有效帧数，没收到时 `error` 说明原因。
    Closed {
        frames: usize,
        error: Option<String>,
    },
}

impl Watch {
    fn new(session_id: String, watch_id: u32, sink: EventSink, timing: Timing) -> Self {
        Self {
            session_id,
            timing,
            state: Mutex::new(WatchState::default()),
            wake: Condvar::new(),
            sinks: Mutex::new(vec![(watch_id, sink)]),
        }
    }

    fn lock(&self) -> MutexGuard<'_, WatchState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn sinks(&self) -> MutexGuard<'_, Vec<(u32, EventSink)>> {
        self.sinks.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wait<'a>(&self, state: MutexGuard<'a, WatchState>) -> MutexGuard<'a, WatchState> {
        self.wake.wait(state).unwrap_or_else(|e| e.into_inner())
    }

    fn wait_timeout<'a>(
        &self,
        state: MutexGuard<'a, WatchState>,
        timeout: Duration,
    ) -> MutexGuard<'a, WatchState> {
        self.wake
            .wait_timeout(state, timeout)
            .unwrap_or_else(|e| e.into_inner())
            .0
    }

    fn stop(&self) {
        let mut state = self.lock();
        stop_locked(&mut state);
        drop(state);
        self.wake.notify_all();
    }

    /// 停下且丢掉待存的缓存：和推送线程的收尾存盘在同一把锁下互斥，之后不会再写。
    fn discard(&self) {
        let mut state = self.lock();
        state.save_due = None;
        stop_locked(&mut state);
        drop(state);
        self.wake.notify_all();
    }

    fn is_stopped(&self) -> bool {
        self.lock().stopped
    }

    /// 挂上一个订阅者（同一会话又被订阅一次），并让推送线程把手里的视图补推一次（已有的
    /// 出口也会再收到同一份视图）。同一编号已挂着就换掉它的出口。已经停了就原样还回出口，
    /// 由调用方新开订阅。
    fn add_sink(&self, watch_id: u32, sink: EventSink) -> Result<(), EventSink> {
        let mut state = self.lock();
        if state.stopped {
            return Err(sink);
        }
        // 先挂出口再放状态锁：推送线程推之前要拿状态锁，补推一定落到新出口上。
        {
            let mut sinks = self.sinks();
            sinks.retain(|(id, _)| *id != watch_id);
            sinks.push((watch_id, sink));
        }
        state.emit_pending = state.snapshot.is_some();
        drop(state);
        self.wake.notify_all();
        Ok(())
    }

    /// 摘掉一个订阅者的出口。没有出口剩下时停掉订阅并返回 true。
    fn remove_sink(&self, watch_id: u32) -> bool {
        let mut state = self.lock();
        let empty = {
            let mut sinks = self.sinks();
            sinks.retain(|(id, _)| *id != watch_id);
            sinks.is_empty()
        };
        if empty {
            stop_locked(&mut state);
        }
        drop(state);
        self.wake.notify_all();
        empty
    }

    /// 推一个事件给每个出口。Dart 取消了的出口摘掉，全摘光了就停。不能在持有状态锁时调。
    fn emit(&self, event: ConversationEvent) {
        let emptied = {
            let mut sinks = self.sinks();
            // 只有一个出口（常态）时不克隆视图：最后一个出口拿走原件。
            let count = sinks.len();
            let mut pending = Some(event);
            let mut seen = 0;
            sinks.retain(|(_, sink)| {
                seen += 1;
                let event = if seen == count {
                    pending.take()
                } else {
                    pending.clone()
                };
                event.is_some_and(sink)
            });
            sinks.is_empty()
        };
        if emptied {
            // 放掉出口锁之后到这里之间可能又挂上了新出口：在状态锁下再看一次，和
            // `add_sink` 互斥。
            let mut state = self.lock();
            if self.sinks().is_empty() {
                stop_locked(&mut state);
            }
            drop(state);
            self.wake.notify_all();
        }
    }

    /// 还没存的实时数据立刻存掉（被顶掉或取消订阅时）。
    fn flush(&self, cache: Option<&Cache>) {
        let (tail, endpoint_id) = {
            let mut state = self.lock();
            if state.save_due.take().is_none() {
                return;
            }
            (state.tail_to_save(), state.endpoint_id.clone())
        };
        if let (Some(cache), Some(tail), Some(endpoint_id)) = (cache, tail, endpoint_id) {
            cache.save_snapshot(&endpoint_id, &self.session_id, &tail);
        }
    }

    /// 连接状态回调，在状态机的锁里调：不阻塞（shutdown 是一次系统调用）。
    fn on_connection(&self, generation: u64, event: &ConnectionEvent) -> bool {
        let mut state = self.lock();
        if state.stopped {
            return false;
        }
        match state.generation {
            None => {
                state.generation = Some(generation);
                state.endpoint_id = event.endpoint_id.clone();
            }
            Some(current) if current != generation => {
                // 同一台机器重新连过：旧一代链路上的 watch 不能再用，掐掉，等新的一代连上再续订。
                log::info!("连接换代，对话 {} 等新连接续订", self.session_id);
                state.generation = Some(generation);
                if event.endpoint_id.is_some() {
                    state.endpoint_id = event.endpoint_id.clone();
                }
                state.connected = false;
                if let Some(conn) = state.conn.take() {
                    let _ = conn.shutdown(std::net::Shutdown::Both);
                }
            }
            Some(_) => {}
        }
        match event.state {
            ConnectionState::Connected => {
                if !state.connected {
                    state.reconnected = true;
                }
                state.connected = true;
            }
            ConnectionState::Disconnected => {
                state.stop_reason = event.error.as_ref().map(|error| error.message.clone());
                stop_locked(&mut state);
                drop(state);
                self.wake.notify_all();
                return false;
            }
            ConnectionState::Connecting | ConnectionState::Reconnecting => {
                // 探活失败：旧链路上的 watch 可能已经僵死但还没察觉，掐掉，重连后再订。
                state.connected = false;
                if let Some(conn) = state.conn.take() {
                    let _ = conn.shutdown(std::net::Shutdown::Both);
                }
            }
        }
        drop(state);
        self.wake.notify_all();
        true
    }

    /// 等一会儿再重订。停了返回 true；期间重新连上就立刻返回。
    fn pause(&self, delay: Duration) -> bool {
        let deadline = Instant::now() + delay;
        let mut state = self.lock();
        loop {
            if state.stopped {
                return true;
            }
            if state.reconnected {
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = self.wait_timeout(state, deadline - now);
        }
    }

    fn run_reader(self: &Arc<Self>, source: &dyn RuntimeSource) {
        let mut failures = 0u32;
        let mut reported = false;
        loop {
            let generation = {
                let mut state = self.lock();
                loop {
                    if state.stopped {
                        return;
                    }
                    if state.connected {
                        break;
                    }
                    state = self.wait(state);
                }
                if std::mem::take(&mut state.reconnected) {
                    failures = 0;
                }
                state.generation
            };
            // 状态机的锁和本订阅的锁不能嵌套着拿：先放掉自己的再问当前连接。
            let runtime = match source.current() {
                Some(runtime) if Some(runtime.generation) == generation => runtime,
                // 刚换代、正在连：等这一代连上（监听会叫醒）。
                _ => {
                    failures = failures.saturating_add(1);
                    if self.pause(self.timing.backoff(failures)) {
                        return;
                    }
                    continue;
                }
            };
            let request = watch_request(&self.session_id, self.lock().snapshot.as_ref());
            let end = self.stream(&runtime.client, &request);
            // 客户端在这条普通线程上释放（iroh 客户端的 Drop 会 block_on）。
            drop(runtime);
            match end {
                StreamEnd::Unavailable(reason) => {
                    self.stop();
                    self.emit(message_event(ConversationEventKind::Unavailable, reason));
                    return;
                }
                StreamEnd::Closed { frames, error } => {
                    if self.is_stopped() {
                        return;
                    }
                    if frames > 0 {
                        failures = 0;
                        reported = false;
                    }
                    failures = failures.saturating_add(1);
                    // 掉线（状态机已不在 Connected）由连接状态告诉界面，这里只报连着却订不上的。
                    // 先取值再判断：条件里的锁守卫会活到块尾，而 emit 可能回头拿这把锁。
                    let connected = self.lock().connected;
                    if let Some(error) = error
                        && !reported
                        && connected
                    {
                        reported = true;
                        self.emit(message_event(ConversationEventKind::Error, error));
                    }
                    if self.pause(self.timing.backoff(failures)) {
                        return;
                    }
                }
            }
        }
    }

    /// 建一条 watch 连接并读到它结束。
    fn stream(&self, client: &RuntimeClient, request: &Value) -> StreamEnd {
        let closed = |error: String| StreamEnd::Closed {
            frames: 0,
            error: Some(error),
        };
        let mut stream = match client.connect() {
            Ok(stream) => stream,
            Err(e) => return closed(format!("连不上运行时：{e}")),
        };
        // 这条连接上只写这一行。之后既不写也不半关闭写方向，见文件头。
        if let Err(e) = write_json_line(&mut stream, request) {
            return closed(format!("订阅请求发不出去：{e}"));
        }
        {
            let mut state = self.lock();
            if state.stopped || !state.connected {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return StreamEnd::Closed {
                    frames: 0,
                    error: None,
                };
            }
            match stream.try_clone() {
                Ok(handle) => state.conn = Some(handle),
                Err(e) => return closed(format!("订阅连接不可用：{e}")),
            }
        }
        let end = self.read_frames(client, stream);
        self.lock().conn = None;
        end
    }

    fn read_frames(&self, client: &RuntimeClient, stream: UnixStream) -> StreamEnd {
        let mut reader = BufReader::new(stream);
        let mut mirror = FrameMirror::default();
        let mut awaiting_full = false;
        let mut frames = 0usize;
        let mut line = Vec::new();
        loop {
            line.clear();
            let closed = match reader.read_until(b'\n', &mut line) {
                Ok(0) => Some("运行时关闭了订阅".to_string()),
                Err(e) => Some(format!("订阅连接断开：{e}")),
                Ok(_) => None,
            };
            if let Some(message) = closed {
                return StreamEnd::Closed {
                    frames,
                    error: (frames == 0).then_some(message),
                };
            }
            let Ok(mut value) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            if let Some(reason) = unavailable_reason(&value) {
                return StreamEnd::Unavailable(reason);
            }
            let frame = if let Some(delta) = value.get_mut("delta").map(Value::take) {
                let restored = serde_json::from_value::<FrameDelta>(delta)
                    .ok()
                    .and_then(|delta| mirror.apply(&delta).ok());
                match restored {
                    Some(frame) => frame,
                    None => {
                        // 镜像对不上：这一帧作废，要一份整段帧。等它来之前不重复要。
                        if !awaiting_full {
                            awaiting_full = true;
                            if let Err(e) =
                                action_on(client, &self.session_id, &AcpUserAction::Refresh)
                            {
                                log::warn!("请求整段快照失败：{}", e.message);
                                awaiting_full = false;
                            }
                        }
                        continue;
                    }
                }
            } else if value.get("snapshot").is_some() {
                mirror.full(&value);
                awaiting_full = false;
                value
            } else {
                continue;
            };
            let Some(snapshot) = decode_frame(frame) else {
                continue;
            };
            frames += 1;
            self.apply_live(snapshot);
        }
    }

    fn apply_live(&self, snapshot: ConversationSnapshot) {
        let mut state = self.lock();
        if state.stopped {
            return;
        }
        state.snapshot = Some(match state.snapshot.take() {
            Some(current) => merge::merge(current, snapshot),
            None => snapshot,
        });
        state.live = true;
        state.saved_at_ms = None;
        state.emit_pending = true;
        if state.save_due.is_none() {
            state.save_due = Some(Instant::now() + self.timing.save_delay);
        }
        drop(state);
        self.wake.notify_all();
    }

    /// 推送线程：窗口变了就按间隔推视图，攒一会儿存一次缓存。
    fn run_publisher(self: &Arc<Self>, cache: Option<Arc<Cache>>) {
        let mut last_emit: Option<Instant> = None;
        loop {
            let mut state = self.lock();
            loop {
                if state.stopped {
                    break;
                }
                let now = Instant::now();
                let interval = self.timing.emit_interval_for(
                    state
                        .snapshot
                        .as_ref()
                        .map_or(0, |snapshot| snapshot.entries.len()),
                );
                let emit_at = state
                    .emit_pending
                    .then(|| last_emit.map_or(now, |last| last + interval));
                match [emit_at, state.save_due].into_iter().flatten().min() {
                    Some(deadline) if deadline <= now => break,
                    Some(deadline) => state = self.wait_timeout(state, deadline - now),
                    None => state = self.wait(state),
                }
            }
            if state.stopped {
                drop(state);
                self.flush(cache.as_deref());
                return;
            }
            let now = Instant::now();
            let interval = self.timing.emit_interval_for(
                state
                    .snapshot
                    .as_ref()
                    .map_or(0, |snapshot| snapshot.entries.len()),
            );
            let mut event = None;
            if state.emit_pending && last_emit.is_none_or(|last| now >= last + interval) {
                state.emit_pending = false;
                event = state.snapshot.as_ref().map(|snapshot| {
                    snapshot_event(&self.session_id, snapshot, !state.live, state.saved_at_ms)
                });
            }
            let mut save = None;
            if state.save_due.is_some_and(|due| due <= now) {
                state.save_due = None;
                save = state.tail_to_save().zip(state.endpoint_id.clone());
            }
            drop(state);
            if let Some(event) = event {
                last_emit = Some(now);
                self.emit(event);
            }
            if let (Some(cache), Some((tail, endpoint_id))) = (cache.as_deref(), save) {
                cache.save_snapshot(&endpoint_id, &self.session_id, &tail);
            }
        }
    }
}

impl WatchState {
    /// 要存进缓存的尾部。只存从运行时收到的实时数据，缓存读回来的不回写。
    fn tail_to_save(&self) -> Option<ConversationSnapshot> {
        self.snapshot
            .as_ref()
            .filter(|_| self.live)
            .map(|snapshot| merge::cache_tail(snapshot, PAGE_LIMIT))
    }
}

fn stop_locked(state: &mut WatchState) {
    state.stopped = true;
    if let Some(conn) = state.conn.take() {
        let _ = conn.shutdown(std::net::Shutdown::Both);
    }
}

/// `acp_watch` 首帧。手里有窗口就带上续传参数：恢复身份和修订号都对得上时运行时从
/// `known_entries` 接着发（窗口到末尾才带），否则按 `tail_limit` 只发末尾。
fn watch_request(session_id: &str, known: Option<&ConversationSnapshot>) -> Value {
    let mut request = json!({
        "op": DaemonOperation::AcpWatch,
        "id": session_id,
        "include_debug": false,
        "image_refs": true,
        "tail_limit": PAGE_LIMIT,
    });
    if let Some(snapshot) = known {
        if let Some(history) = merge::stable_history_id(snapshot) {
            request["history_session_id"] = history.into();
        }
        request["snapshot_revision"] = snapshot.snapshot_revision.into();
        let end = merge::entries_end(snapshot);
        if end == snapshot.entries_total {
            request["known_entries"] = end.into();
        }
    }
    request
}

fn unavailable_reason(value: &Value) -> Option<String> {
    let unavailable = value.get("unavailable")?;
    Some(
        unavailable
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("运行时接不上这段对话")
            .to_string(),
    )
}

/// 一份完整的快照帧 → 快照。快照里缺恢复身份时用活状态头里的那份。
fn decode_frame(mut frame: Value) -> Option<ConversationSnapshot> {
    let live_history = frame
        .get("live")
        .and_then(|live| live.get("history_session_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string);
    let snapshot = frame.get_mut("snapshot")?.take();
    let mut snapshot: ConversationSnapshot = match serde_json::from_value(snapshot) {
        Ok(snapshot) => snapshot,
        Err(e) => {
            log::warn!("对话快照解析失败：{e}");
            return None;
        }
    };
    if snapshot.history_session_id.is_none() {
        snapshot.history_session_id = live_history;
    }
    merge::normalize(&mut snapshot);
    Some(snapshot)
}

fn snapshot_event(
    session_id: &str,
    snapshot: &ConversationSnapshot,
    cached: bool,
    saved_at_ms: Option<i64>,
) -> ConversationEvent {
    ConversationEvent {
        kind: ConversationEventKind::Snapshot,
        view: Some(view::conversation_view(session_id, snapshot)),
        cached,
        saved_at_ms: if cached { saved_at_ms } else { None },
        message: None,
    }
}

fn message_event(kind: ConversationEventKind, message: String) -> ConversationEvent {
    ConversationEvent {
        kind,
        view: None,
        cached: false,
        saved_at_ms: None,
        message: Some(message),
    }
}

// ---------------------------------------------------------------------------
// 一问一答的请求
// ---------------------------------------------------------------------------

/// 快照头末尾的几条，带上它们在整段对话里的序号。
fn recent_context(snapshot: &ConversationSnapshot) -> Vec<ConversationEntry> {
    let start = snapshot.entries.len().saturating_sub(RECENT_CONTEXT);
    snapshot.entries[start..]
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            view::conversation_entry(view::to_i64(snapshot.entries_offset + start + i), entry)
        })
        .collect()
}

/// `acp_snapshot`：`before` 之前的 `limit` 条，和完整的快照头。
fn history_page(
    client: &RuntimeClient,
    session_id: &str,
    before: usize,
    limit: usize,
) -> Result<ConversationSnapshot, SmeltError> {
    let request = json!({
        "op": DaemonOperation::AcpSnapshot,
        "id": session_id,
        "before": before,
        "limit": limit,
        "include_debug": false,
        "image_refs": true,
    });
    let mut response = client
        .request(&request, SNAPSHOT_TIMEOUT)
        .map_err(request_error)?;
    if let Some(snapshot) = response.get_mut("snapshot").map(Value::take) {
        let mut snapshot: ConversationSnapshot = serde_json::from_value(snapshot)
            .map_err(|e| error(SmeltErrorKind::Other, format!("历史快照内容无效：{e}")))?;
        merge::normalize(&mut snapshot);
        return Ok(snapshot);
    }
    Err(error(
        SmeltErrorKind::Other,
        response
            .get("error")
            .and_then(Value::as_str)
            .filter(|message| !message.is_empty())
            .unwrap_or("会话不存在"),
    ))
}

fn action_on(
    client: &RuntimeClient,
    session_id: &str,
    action: &AcpUserAction,
) -> Result<(), SmeltError> {
    let request = json!({
        "op": DaemonOperation::AcpAction,
        "id": session_id,
        "action": action,
    });
    let response = client
        .request(&request, ACTION_TIMEOUT)
        .map_err(request_error)?;
    ok_response(&response)
}

fn ok_response(response: &Value) -> Result<(), SmeltError> {
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    Err(error(
        SmeltErrorKind::Other,
        response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("运行时没有执行这个操作"),
    ))
}

/// 发送：连接建不起来时请求没发出去，可以放心重发；写出之后的任何失败都可能已经送达。
fn submit(client: &RuntimeClient, request: &Value) -> Result<(), SendMessageError> {
    let unknown = |message: String| send_error(SendFailureKind::Unknown, message);
    let mut stream = client
        .connect_with_timeout(SUBMIT_TIMEOUT)
        .map_err(|e| send_error(SendFailureKind::Rejected, format!("连不上运行时：{e}")))?;
    write_json_line(&mut stream, request)
        .map_err(|e| unknown(format!("发送中断，不确定运行时是否收到：{e}")))?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| unknown(format!("没等到运行时确认：{e}")))?;
    if line.trim().is_empty() {
        return Err(unknown("运行时没有确认就断开了".to_string()));
    }
    let response: Value = serde_json::from_str(line.trim())
        .map_err(|e| unknown(format!("运行时的确认读不懂：{e}")))?;
    parse_conversation_submit_response(response).map_err(|failure| SendMessageError {
        kind: match failure.kind {
            ConversationSubmitErrorKind::Rejected => SendFailureKind::Rejected,
            ConversationSubmitErrorKind::Unknown => SendFailureKind::Unknown,
        },
        message: failure.message,
    })
}

fn send_error(kind: SendFailureKind, message: impl Into<String>) -> SendMessageError {
    SendMessageError {
        kind,
        message: message.into(),
    }
}

fn request_error(failure: RequestError) -> SmeltError {
    match failure {
        RequestError::Transport(e) => error(SmeltErrorKind::Offline, format!("连不上运行时：{e}")),
        RequestError::Closed => error(
            SmeltErrorKind::Other,
            "运行时没有回应这个请求（会话不存在或运行时版本不支持）",
        ),
        RequestError::InvalidResponse(message) => error(SmeltErrorKind::Other, message),
    }
}

fn control_error(failure: ControlClientError) -> SmeltError {
    match failure {
        ControlClientError::Transport(message) => {
            error(SmeltErrorKind::Offline, format!("连不上运行时：{message}"))
        }
        ControlClientError::Protocol(failure) => {
            let kind = match failure.code {
                ControlErrorCode::PermissionDenied => SmeltErrorKind::Unpaired,
                ControlErrorCode::UnsupportedVersion | ControlErrorCode::MethodNotFound => {
                    SmeltErrorKind::Incompatible
                }
                _ => SmeltErrorKind::Other,
            };
            error(kind, failure.message)
        }
        ControlClientError::InvalidRequest(message)
        | ControlClientError::InvalidResponse(message) => error(SmeltErrorKind::Other, message),
    }
}

#[cfg(test)]
mod tests;
