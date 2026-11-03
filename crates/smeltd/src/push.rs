//! 手机推送：关注事件经推送中继（APNs）发给登记过的手机。
//!
//! 判定和桌面、手机同源：进程内订阅自己的会话状态，用客户端同一个解码器和
//! `smelt_model::attention` 算提醒边沿，不重新解释 phase。推等批准、等输入、失败、完成四种，
//! 每台手机按自己登记的类型过滤（`push_registration` 表）。推送带会话名、项目名和一行摘要，
//! 摘要就是会话状态里的 `pending_question`（桌面通知、手机会话列表读的同一份）。
//!
//! 提醒一产生就进内存 outbox。发之前看人在不在这台 Mac 前（`crate::presence`）：
//! - 不在：立刻发。
//! - 在：Mac 上已经弹了系统通知，手机不响。等批准、等回复先扣着，人一离开（锁屏、本机桌面
//!   App 断开、键鼠闲置满 `presence::AWAY_AFTER`）还没处理就补推；完成、失败不推。
//!
//! 发送失败按退避重试，超过 [`TTL`] 丢弃；重试期间事情已经处理掉（行动项解决、会话删除）就
//! 不再重试。outbox 不落盘：smeltd 重启后首份快照只建基线，不补发重启前的提醒。
//!
//! 两条线程：`smelt-push-watch` 阻塞在事件总线上，`smelt-push-send` 阻塞在 outbox 的 Condvar
//! 上，只在有提醒要发、重试到点、扣着的提醒该复查、或人离开时醒来。没有待发的提醒时两条线程
//! 都不醒。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::presence::Presence;
use smelt_core::attention::{self, AttentionKind, AttentionStore};
use smelt_core::daemon_state::{
    DaemonSessionState, DaemonStateEvent, DaemonStateEventClient, SubscriptionShape,
};
use smelt_core::remote_config::PUSH_PAYLOAD_VERSION;
use smelt_event_bus::Delivery;
use smelt_event_protocol::{
    CORE_CAPABILITY_SESSION_READ, CORE_TOPIC_SESSION_REMOVED, CORE_TOPIC_SESSION_STATE_CHANGED,
    Capability, PluginId, SubscribeMessage, SubscriptionId, Topic,
};
use smelt_push::{
    DETAIL_MAX_CHARS, PushKind, RELAY_SEND_PATH, SUBTITLE_MAX_CHARS, SendRequest, TITLE_MAX_CHARS,
    excerpt,
};

use crate::event_hub::{self, EventHubHandle};

/// 从提醒产生算起，超过这么久还没送出去就丢掉：过时的「等你批准」不如不推。
const TTL: Duration = Duration::from_secs(10 * 60);
/// 失败后第一次重试的间隔，之后翻倍，封顶 [`BACKOFF_MAX`]。
const BACKOFF_BASE: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(2 * 60);
/// 一次发给中继的请求的时限。
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

const PLUGIN_ID: &str = "core.push";
const SUBSCRIPTION_ID: &str = "push";

/// 只订会话状态：工作区、自动化、请求类 topic 都用不上。
const SUBSCRIPTION: SubscriptionShape = SubscriptionShape {
    workspace: false,
    automations: false,
    conversation_wake: false,
    appearance_requests: false,
    project_changes: false,
    agent_memory_changes: false,
    terminal_notifications: false,
    terminal_progress: false,
};

pub(crate) fn spawn(event_hub: &EventHubHandle) {
    // 测试进程没有运行时库，也就没有登记，不必起线程。
    if !smelt_core::sqlite_state::is_sqlite_state_enabled() {
        return;
    }
    let shared = Arc::new(Shared::default());
    let away_shared = Arc::clone(&shared);
    crate::presence::on_away(move || away_shared.release_held());
    let watcher_hub = Arc::clone(event_hub);
    let watcher_shared = Arc::clone(&shared);
    if let Err(error) = std::thread::Builder::new()
        .name("smelt-push-watch".to_string())
        .spawn(move || run_watcher(&watcher_hub, &watcher_shared))
    {
        crate::dlog(&format!("推送：起不了订阅线程：{error}"));
        return;
    }
    if let Err(error) = std::thread::Builder::new()
        .name("smelt-push-send".to_string())
        .spawn(move || run_sender(&shared))
    {
        crate::dlog(&format!("推送：起不了发送线程：{error}"));
    }
}

// ---------------------------------------------------------------------------
// 判定：会话状态 → 要推 / 不用推了
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Command {
    /// 这段会话有了一条要推的提醒，替换它之前还没推出去的那条。
    Notify {
        session_id: String,
        kind: PushKind,
        content: Content,
    },
    /// 这段会话的事已经处理掉（或会话没了），还没推出去的不推了。
    Cancel { session_id: String },
}

/// 推送里显示的文字，取自提醒产生那一刻的会话状态，已按 `smelt_push` 的上限截好。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Content {
    /// 会话名：用户起的名字优先，其次是智能体报的标题。
    title: Option<String>,
    /// 项目名：工作目录的最后一级。
    subtitle: Option<String>,
    /// 一行摘要：等批准的命令、等回复的问题、失败原因、最后一条回复的开头。
    detail: Option<String>,
}

impl Content {
    fn of(state: &DaemonSessionState) -> Self {
        let title = [state.custom_title.as_deref(), state.title.as_deref()]
            .into_iter()
            .flatten()
            .find_map(|title| excerpt(title, TITLE_MAX_CHARS));
        let subtitle = state
            .cwd
            .as_deref()
            .and_then(|cwd| Path::new(cwd).file_name())
            .and_then(|name| excerpt(&name.to_string_lossy(), SUBTITLE_MAX_CHARS));
        let detail = state
            .phase_is_authoritative()
            .then_some(state.pending_question.as_deref())
            .flatten()
            .and_then(|question| excerpt(question, DETAIL_MAX_CHARS));
        Self {
            title,
            subtitle,
            detail,
        }
    }
}

fn push_kind(kind: AttentionKind) -> Option<PushKind> {
    match kind {
        AttentionKind::Approval => Some(PushKind::Approval),
        AttentionKind::Input => Some(PushKind::Input),
        AttentionKind::Failure => Some(PushKind::Failure),
        AttentionKind::Success => Some(PushKind::Success),
        AttentionKind::Bell | AttentionKind::Notice => None,
    }
}

/// 和手机会话目录（`smelt-mobile` 的 `CatalogState`）同样的边沿判定，只留推送要的部分。
#[derive(Default)]
struct Tracker {
    /// 收到过首份快照。首份只建基线：smeltd 启动前就在等批准的会话不算新提醒。
    primed: bool,
    sessions: HashMap<String, DaemonSessionState>,
    attention: AttentionStore,
}

impl Tracker {
    fn apply(&mut self, event: DaemonStateEvent, now: Instant) -> Vec<Command> {
        let mut commands = Vec::new();
        match event {
            DaemonStateEvent::Snapshot { sessions, .. } => {
                let baseline = !self.primed;
                for state in &sessions {
                    let had_action = self.attention.has_unresolved_action(&state.id);
                    if baseline {
                        attention::apply_daemon_baseline(&mut self.attention, state);
                    } else {
                        attention::apply_daemon_transition(
                            &mut self.attention,
                            self.sessions.get(&state.id),
                            state,
                            now,
                        );
                    }
                    if had_action && !self.attention.has_unresolved_action(&state.id) {
                        commands.push(Command::Cancel {
                            session_id: state.id.clone(),
                        });
                    }
                }
                let ids: HashSet<&str> = sessions.iter().map(|state| state.id.as_str()).collect();
                let stale: Vec<String> = self
                    .sessions
                    .keys()
                    .filter(|id| !ids.contains(id.as_str()))
                    .cloned()
                    .collect();
                for id in stale {
                    self.attention.remove_session(&id);
                    commands.push(Command::Cancel { session_id: id });
                }
                self.sessions = sessions
                    .into_iter()
                    .map(|state| (state.id.clone(), state))
                    .collect();
                self.primed = true;
            }
            DaemonStateEvent::Update(state) => {
                let had_action = self.attention.has_unresolved_action(&state.id);
                attention::apply_daemon_transition(
                    &mut self.attention,
                    self.sessions.get(&state.id),
                    &state,
                    now,
                );
                if had_action && !self.attention.has_unresolved_action(&state.id) {
                    commands.push(Command::Cancel {
                        session_id: state.id.clone(),
                    });
                }
                self.sessions.insert(state.id.clone(), state);
            }
            DaemonStateEvent::Removed { id } => {
                self.attention.remove_session(&id);
                self.sessions.remove(&id);
                commands.push(Command::Cancel { session_id: id });
            }
            _ => {}
        }
        for item in self.attention.drain_deliveries() {
            if let Some(kind) = push_kind(item.kind) {
                let content = self
                    .sessions
                    .get(&item.session_id)
                    .map(Content::of)
                    .unwrap_or_default();
                commands.push(Command::Notify {
                    session_id: item.session_id,
                    kind,
                    content,
                });
            }
        }
        commands
    }
}

fn run_watcher(event_hub: &EventHubHandle, shared: &Shared) {
    let plugin_id = PluginId::new(PLUGIN_ID).expect("push plugin id is valid");
    let subscription_id = SubscriptionId::new(SUBSCRIPTION_ID).expect("push subscription id");
    let topics: BTreeSet<Topic> = [CORE_TOPIC_SESSION_STATE_CHANGED, CORE_TOPIC_SESSION_REMOVED]
        .into_iter()
        .map(|topic| Topic::new(topic).expect("core topic is valid"))
        .collect();
    let capabilities = BTreeSet::from([
        Capability::new(CORE_CAPABILITY_SESSION_READ).expect("core capability is valid")
    ]);
    let subscription = match event_hub.subscribe(
        plugin_id.clone(),
        subscription_id.clone(),
        topics,
        &capabilities,
    ) {
        Ok(subscription) => subscription,
        Err(error) => {
            crate::dlog(&format!("推送：订阅会话状态失败，不推送：{error}"));
            return;
        }
    };
    let mut decoder = DaemonStateEventClient::new(SUBSCRIPTION);
    let mut tracker = Tracker::default();
    // 总线关掉（守护停机）时 recv 出错，线程随之结束。
    while let Ok(delivery) = subscription.recv() {
        let mut messages: Vec<SubscribeMessage> = Vec::new();
        if let Delivery::Lag {
            dropped,
            resume_after,
        } = delivery
        {
            // 落后了：丢掉通道里剩下的旧增量，按快照重新对齐。快照之后的变化照常比较边沿。
            subscription.discard_pending();
            messages.push(SubscribeMessage::Lag {
                dropped,
                resume_after,
            });
            match event_hub
                .runtime()
                .recover_snapshots(&plugin_id, &subscription_id)
            {
                Ok(snapshots) => messages.extend(snapshots.into_iter().map(|snapshot| {
                    event_hub::delivery_to_subscribe_message(Delivery::Snapshot(snapshot))
                })),
                Err(error) => {
                    crate::dlog(&format!("推送：落后后重取快照失败，停止推送：{error}"));
                    return;
                }
            }
        } else {
            messages.push(event_hub::delivery_to_subscribe_message(delivery));
        }
        for message in messages {
            match decoder.decode_message(message) {
                Ok(Some(event)) => shared.apply(tracker.apply(event, Instant::now())),
                Ok(None) => {}
                Err(error) => crate::dlog(&format!("推送：解不开会话状态：{error}")),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// outbox：扣着、取消、退避重试
// ---------------------------------------------------------------------------

/// 推给一台手机要的东西，取自它的登记。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    device_endpoint_id: String,
    handle: String,
    relay_url: String,
    /// 手机认得带摘要的载荷和「完成」（登记的载荷版本够新，见
    /// `smelt_core::remote_config::PUSH_PAYLOAD_VERSION`）。旧 App 只发类型，不然它会把
    /// 认不得的 loc-key 原样显示出来。
    rich: bool,
}

/// 一条待推的提醒。
#[derive(Debug)]
struct Job {
    /// 区分同一会话先后的提醒：被取消或被新提醒替换后，发送中的旧 Job 回来不能再排队。
    serial: u64,
    session_id: String,
    kind: PushKind,
    content: Content,
    /// 同一条提醒重试时不变，collapse id 也由它算出，重发的顶掉可能已经送到的那条。
    apns_id: String,
    /// 过期（[`TTL`]）从这里算：第一次真正发送时重设，扣着的时间不算。
    created: Instant,
    due: Instant,
    attempt: u32,
    /// 人在 Mac 前，先扣着；到 `due` 再看一次人在不在，人离开时提前放出来。
    held: bool,
    /// 人离开（锁屏、桌面 App 断开）时放出来的：直接发，不再看在不在。锁屏通知和会话里的
    /// 锁屏标记谁先谁后没有保证，不能指望复查时一定读到「锁了」。
    released: bool,
    /// 还没送到的手机。`None` 表示还没到第一次发送：到点时按那时的登记展开。
    targets: Option<Vec<Target>>,
}

#[derive(Default)]
struct Outbox {
    jobs: Vec<Job>,
    /// 每段会话当前有效的那条提醒。
    live: HashMap<String, u64>,
    next_serial: u64,
}

impl Outbox {
    /// 新提醒立刻到点。同一会话还在重试的旧提醒作废：积压恢复后只推最新的那条。
    fn notify(&mut self, session_id: String, kind: PushKind, content: Content, now: Instant) {
        self.jobs.retain(|job| job.session_id != session_id);
        let serial = self.next_serial;
        self.next_serial += 1;
        self.live.insert(session_id.clone(), serial);
        self.jobs.push(Job {
            serial,
            session_id,
            kind,
            content,
            apns_id: uuid::Uuid::new_v4().to_string(),
            created: now,
            due: now,
            attempt: 0,
            held: false,
            released: false,
            targets: None,
        });
    }

    /// 扣着到 `until` 再看。期间被取消、被替换的不再回来。
    fn hold(&mut self, mut job: Job, until: Instant) {
        if self.live.get(&job.session_id) != Some(&job.serial) {
            return;
        }
        job.held = true;
        job.due = until;
        self.jobs.push(job);
    }

    /// 不发了（人在 Mac 前，完成 / 失败已经在 Mac 上看到）。
    fn discard(&mut self, job: Job) {
        if self.live.get(&job.session_id) == Some(&job.serial) {
            self.live.remove(&job.session_id);
        }
    }

    /// 人离开了：扣着的立刻到点。有扣着的返回 true。
    fn release_held(&mut self, now: Instant) -> bool {
        let mut released = false;
        for job in self.jobs.iter_mut().filter(|job| job.held) {
            job.due = now;
            job.released = true;
            released = true;
        }
        released
    }

    fn cancel(&mut self, session_id: &str) {
        self.jobs.retain(|job| job.session_id != session_id);
        self.live.remove(session_id);
    }

    fn next_due(&self) -> Option<Instant> {
        self.jobs.iter().map(|job| job.due).min()
    }

    /// 取出到点的提醒。它们仍算有效，发完交回 [`Self::finish`]。
    fn take_due(&mut self, now: Instant) -> Vec<Job> {
        let (due, waiting): (Vec<Job>, Vec<Job>) = std::mem::take(&mut self.jobs)
            .into_iter()
            .partition(|job| job.due <= now);
        self.jobs = waiting;
        due
    }

    /// 发完一轮：还有手机没送到就按退避排下一次，送完、过期或期间被取消 / 替换就收掉。
    /// 过期时还有手机没送到，返回 true。
    fn finish(&mut self, mut job: Job, now: Instant) -> bool {
        if self.live.get(&job.session_id) != Some(&job.serial) {
            return false;
        }
        let remaining = job
            .targets
            .as_ref()
            .is_none_or(|targets| !targets.is_empty());
        if remaining {
            job.attempt += 1;
            job.due = now + backoff(job.attempt);
            if job.due <= job.created + TTL {
                self.jobs.push(job);
                return false;
            }
        }
        self.live.remove(&job.session_id);
        remaining
    }
}

fn backoff(attempt: u32) -> Duration {
    BACKOFF_BASE
        .saturating_mul(1 << attempt.saturating_sub(1).min(16))
        .min(BACKOFF_MAX)
}

#[derive(Default)]
struct Shared {
    outbox: Mutex<Outbox>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Outbox> {
        self.outbox.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn apply(&self, commands: Vec<Command>) {
        if commands.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut outbox = self.lock();
        for command in commands {
            match command {
                Command::Notify {
                    session_id,
                    kind,
                    content,
                } => outbox.notify(session_id, kind, content, now),
                Command::Cancel { session_id } => outbox.cancel(&session_id),
            }
        }
        drop(outbox);
        self.wake.notify_one();
    }

    /// 人离开了：扣着的提醒马上复查。
    fn release_held(&self) {
        if self.lock().release_held(Instant::now()) {
            self.wake.notify_one();
        }
    }

    /// 阻塞到有提醒到点。outbox 空着时一直等新提醒；有待发的就等到最早那条的时间点，
    /// 新提醒、取消、人离开会提前叫醒重算。
    ///
    /// 轮询例外：扣着的提醒要在键鼠闲置满 `presence::AWAY_AFTER` 时复查，系统不为「闲置满
    /// 几分钟」发事件；只在有扣着的提醒时定这一次闹钟，人还在就按新的剩余时间再定，周期不超过
    /// `AWAY_AFTER`，提醒被处理、替换或补推后就没有了。
    fn wait_due(&self) -> Vec<Job> {
        let mut outbox = self.lock();
        loop {
            let now = Instant::now();
            let due = outbox.take_due(now);
            if !due.is_empty() {
                return due;
            }
            outbox = match outbox.next_due() {
                Some(at) => {
                    self.wake
                        .wait_timeout(outbox, at.saturating_duration_since(now))
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
                None => self.wake.wait(outbox).unwrap_or_else(|e| e.into_inner()),
            };
        }
    }
}

// ---------------------------------------------------------------------------
// 发送
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Delivered,
    /// 句柄或令牌失效：删掉这台手机的登记。
    Gone,
    /// 请求本身不对，重试也没用。
    Rejected,
    Retry,
}

fn outcome_of(status: u16) -> Outcome {
    match status {
        200..=299 => Outcome::Delivered,
        410 => Outcome::Gone,
        429 | 500..=599 => Outcome::Retry,
        _ => Outcome::Rejected,
    }
}

fn run_sender(shared: &Shared) {
    let client = match reqwest::blocking::Client::builder()
        .timeout(SEND_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            crate::dlog(&format!("推送：建不了 HTTP 客户端，不推送：{error}"));
            return;
        }
    };
    loop {
        for mut job in shared.wait_due() {
            if job.attempt == 0 {
                let now = Instant::now();
                let presence = if job.released {
                    Presence::Away
                } else {
                    crate::presence::current()
                };
                match gate(job.kind, presence) {
                    Gate::Hold(wait) => {
                        if !job.held {
                            crate::dlog(&format!(
                                "推送：{:?} 会话 {} 人在 Mac 前，先扣着",
                                job.kind,
                                short(&job.session_id)
                            ));
                        }
                        // 至少隔一秒再看：剩余时间按浮点算，别在临界点上空转。
                        shared
                            .lock()
                            .hold(job, now + wait.max(Duration::from_secs(1)));
                        continue;
                    }
                    Gate::Skip => {
                        crate::dlog(&format!(
                            "推送：{:?} 会话 {} 人在 Mac 前，不推",
                            job.kind,
                            short(&job.session_id)
                        ));
                        shared.lock().discard(job);
                        continue;
                    }
                    Gate::Send => {
                        if job.held {
                            crate::dlog(&format!(
                                "推送：{:?} 会话 {} 人离开了还没处理，补推",
                                job.kind,
                                short(&job.session_id)
                            ));
                        }
                        job.held = false;
                        job.created = now;
                    }
                }
            }
            let job = deliver(&client, job);
            let (kind, session) = (job.kind, short(&job.session_id).to_string());
            if shared.lock().finish(job, Instant::now()) {
                crate::dlog(&format!(
                    "推送：{kind:?} 会话 {session} 重试超过 {} 分钟，放弃",
                    TTL.as_secs() / 60
                ));
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Gate {
    Send,
    /// 扣着，这么久之后再看。
    Hold(Duration),
    Skip,
}

/// 第一次发之前：人不在就发；人在时，要你处理的扣到可能离开的那一刻，结果类的不推。
fn gate(kind: PushKind, presence: Presence) -> Gate {
    match presence {
        Presence::Away => Gate::Send,
        Presence::Here { away_in } if kind.is_request() => Gate::Hold(away_in),
        Presence::Here { .. } => Gate::Skip,
    }
}

/// 发给一台手机的请求。旧 App 不带摘要：它的文案文件里没有带摘要的 loc-key。
fn send_request(job: &Job, target: &Target, runtime_id: &str) -> SendRequest {
    SendRequest {
        collapse_id: smelt_push::collapse_id(&target.handle, &job.apns_id),
        handle: target.handle.clone(),
        kind: job.kind,
        runtime_id: runtime_id.to_string(),
        session_id: job.session_id.clone(),
        apns_id: job.apns_id.clone(),
        title: job.content.title.clone(),
        subtitle: job.content.subtitle.clone(),
        detail: job.content.detail.clone().filter(|_| target.rich),
    }
}

/// 第一次发送时按当时的登记展开目标；送到、失效、被拒的手机从目标里拿掉，剩下的等重试。
fn deliver(client: &reqwest::blocking::Client, mut job: Job) -> Job {
    let Some(runtime_id) = crate::runtime_identity::runtime_id() else {
        // 运行时身份暂时读不出：手机认不出是哪台机器，等下一次。
        return job;
    };
    let targets = match job.targets.take() {
        Some(targets) => targets,
        None => match registered_targets(job.kind) {
            Ok(targets) => targets,
            Err(error) => {
                crate::dlog(&format!("推送：读登记失败：{error}"));
                return job;
            }
        },
    };
    let mut remaining = Vec::new();
    for target in targets {
        let request = send_request(&job, &target, &runtime_id);
        let url = format!(
            "{}{RELAY_SEND_PATH}",
            target.relay_url.trim_end_matches('/')
        );
        let outcome = match client.post(&url).json(&request).send() {
            Ok(response) => {
                let status = response.status().as_u16();
                let outcome = outcome_of(status);
                if outcome != Outcome::Delivered {
                    crate::dlog(&format!("推送：中继 {} 回 {status}", target.relay_url));
                }
                outcome
            }
            Err(error) => {
                crate::dlog(&format!("推送：连不上中继 {}：{error}", target.relay_url));
                Outcome::Retry
            }
        };
        match outcome {
            Outcome::Delivered => crate::dlog(&format!(
                "推送：{:?} 会话 {} 已送到设备 {}",
                job.kind,
                short(&job.session_id),
                short(&target.device_endpoint_id)
            )),
            Outcome::Gone => {
                crate::dlog(&format!(
                    "推送：设备 {} 的推送登记已失效，删掉",
                    target.device_endpoint_id
                ));
                if let Err(error) =
                    smelt_core::sqlite_state::default_sqlite_store().and_then(|store| {
                        store
                            .delete_push_registration(&target.device_endpoint_id)
                            .map_err(|error| error.to_string())
                    })
                {
                    crate::dlog(&format!("推送：删失效登记失败：{error}"));
                }
            }
            Outcome::Rejected => crate::dlog(&format!(
                "推送：中继 {} 拒收了给设备 {} 的推送",
                target.relay_url, target.device_endpoint_id
            )),
            Outcome::Retry => remaining.push(target),
        }
    }
    job.targets = Some(remaining);
    job
}

/// 日志里的 id 只留前 8 位，够对上又不刷屏。
fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn registered_targets(kind: PushKind) -> Result<Vec<Target>, String> {
    let registrations = smelt_core::sqlite_state::default_sqlite_store()?
        .list_push_registrations()
        .map_err(|error| error.to_string())?;
    Ok(registrations
        .into_iter()
        .filter(|registration| match kind {
            PushKind::Approval => registration.notify_approval,
            PushKind::Input => registration.notify_input,
            PushKind::Failure => registration.notify_failure,
            // 旧 App 不认「完成」的 loc-key。
            PushKind::Success => {
                registration.notify_success && registration.payload_version >= PUSH_PAYLOAD_VERSION
            }
        })
        .map(|registration| Target {
            device_endpoint_id: registration.device_endpoint_id,
            handle: registration.handle,
            relay_url: registration.relay_url,
            rich: registration.payload_version >= PUSH_PAYLOAD_VERSION,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_core::daemon_state::DaemonPhase;

    fn state(id: &str, phase: DaemonPhase) -> DaemonSessionState {
        DaemonSessionState {
            id: id.into(),
            phase,
            structured_events: true,
            turn_events: true,
            ..Default::default()
        }
    }

    fn snapshot(sessions: Vec<DaemonSessionState>) -> DaemonStateEvent {
        DaemonStateEvent::Snapshot {
            sessions,
            workspace: None,
            automations: None,
        }
    }

    fn notify(id: &str, kind: PushKind) -> Command {
        Command::Notify {
            session_id: id.into(),
            kind,
            content: Content::default(),
        }
    }

    fn cancel(id: &str) -> Command {
        Command::Cancel {
            session_id: id.into(),
        }
    }

    /// 启动前就在等批准的会话只是基线，不推；之后新进入的等待才推。
    #[test]
    fn first_snapshot_is_a_baseline() {
        let mut tracker = Tracker::default();
        let now = Instant::now();
        let commands = tracker.apply(
            snapshot(vec![state("a", DaemonPhase::AwaitingApproval)]),
            now,
        );
        assert!(commands.is_empty(), "{commands:?}");

        let commands = tracker.apply(
            DaemonStateEvent::Update(state("b", DaemonPhase::WaitingForUser)),
            now,
        );
        assert_eq!(commands, vec![notify("b", PushKind::Input)]);
    }

    #[test]
    fn approval_is_pushed_and_cancelled_once_resolved() {
        let mut tracker = Tracker::default();
        let now = Instant::now();
        tracker.apply(snapshot(vec![state("a", DaemonPhase::Thinking)]), now);

        let commands = tracker.apply(
            DaemonStateEvent::Update(state("a", DaemonPhase::AwaitingApproval)),
            now,
        );
        assert_eq!(commands, vec![notify("a", PushKind::Approval)]);

        let commands = tracker.apply(
            DaemonStateEvent::Update(state("a", DaemonPhase::ExecutingTool)),
            now,
        );
        assert_eq!(commands, vec![cancel("a")]);
    }

    /// 完成、失败都推；会话删掉就取消。
    #[test]
    fn success_and_failure_are_pushed_and_removal_cancels() {
        let mut tracker = Tracker::default();
        let now = Instant::now();
        tracker.apply(snapshot(vec![state("a", DaemonPhase::Thinking)]), now);

        let commands = tracker.apply(
            DaemonStateEvent::Update(state("a", DaemonPhase::Succeeded)),
            now,
        );
        assert_eq!(commands, vec![notify("a", PushKind::Success)]);

        let commands = tracker.apply(
            DaemonStateEvent::Update(state("a", DaemonPhase::Failed)),
            now,
        );
        assert_eq!(commands, vec![notify("a", PushKind::Failure)]);

        let commands = tracker.apply(DaemonStateEvent::Removed { id: "a".into() }, now);
        assert_eq!(commands, vec![cancel("a")]);
    }

    /// 文字取提醒产生那一刻的会话状态：会话名优先用户起的，项目名取目录末段，摘要是问句。
    #[test]
    fn content_comes_from_the_session_state() {
        let mut tracker = Tracker::default();
        let now = Instant::now();
        tracker.apply(snapshot(vec![state("a", DaemonPhase::Thinking)]), now);

        let mut waiting = state("a", DaemonPhase::AwaitingApproval);
        waiting.title = Some("智能体起的名".into());
        waiting.custom_title = Some("修推送".into());
        waiting.cwd = Some("/Users/cc/wawa/smelt/".into());
        waiting.pending_question = Some("rm -rf\n  build".into());
        let commands = tracker.apply(DaemonStateEvent::Update(waiting), now);
        assert_eq!(
            commands,
            vec![Command::Notify {
                session_id: "a".into(),
                kind: PushKind::Approval,
                content: Content {
                    title: Some("修推送".into()),
                    subtitle: Some("smelt".into()),
                    detail: Some("rm -rf build".into()),
                },
            }]
        );
    }

    #[test]
    fn content_falls_back_and_truncates() {
        let mut session = state("a", DaemonPhase::Succeeded);
        session.custom_title = Some("  ".into());
        session.title = Some("字".repeat(TITLE_MAX_CHARS + 10));
        session.pending_question = Some("  ".into());
        let content = Content::of(&session);
        assert_eq!(
            content.title.as_deref().map(|t| t.chars().count()),
            Some(TITLE_MAX_CHARS),
            "用户名字是空白就用智能体的标题，超长截断"
        );
        assert_eq!(content.subtitle, None);
        assert_eq!(content.detail, None);

        session.turn_events = false;
        session.pending_question = Some("猜出来的问句".into());
        assert_eq!(Content::of(&session).detail, None, "相位不权威时不带摘要");
    }

    /// 断线重收的快照照常比较边沿：期间进入等待的会话要推，消失的会话要取消。
    #[test]
    fn later_snapshots_compare_edges() {
        let mut tracker = Tracker::default();
        let now = Instant::now();
        tracker.apply(
            snapshot(vec![
                state("a", DaemonPhase::Thinking),
                state("b", DaemonPhase::Thinking),
            ]),
            now,
        );
        let commands = tracker.apply(
            snapshot(vec![state("a", DaemonPhase::AwaitingApproval)]),
            now,
        );
        assert_eq!(commands, vec![cancel("b"), notify("a", PushKind::Approval)]);
    }

    fn target(device: &str) -> Target {
        Target {
            device_endpoint_id: device.into(),
            handle: format!("handle-{device}"),
            relay_url: "https://push.example.test".into(),
            rich: true,
        }
    }

    /// 旧 App 只拿到类型和会话名：带摘要的 loc-key 它不认得。
    #[test]
    fn legacy_targets_get_no_detail() {
        let mut outbox = Outbox::default();
        let start = Instant::now();
        let content = Content {
            title: Some("修推送".into()),
            subtitle: Some("smelt".into()),
            detail: Some("rm -rf build".into()),
        };
        outbox.notify("a".into(), PushKind::Approval, content, start);
        let job = outbox.take_due(start).pop().unwrap();

        let rich = send_request(&job, &target("new"), "runtime");
        assert_eq!(rich.detail.as_deref(), Some("rm -rf build"));
        assert_eq!(rich.title.as_deref(), Some("修推送"));

        let mut legacy = target("old");
        legacy.rich = false;
        let legacy = send_request(&job, &legacy, "runtime");
        assert_eq!(legacy.detail, None);
        assert_eq!(legacy.title.as_deref(), Some("修推送"));
    }

    #[test]
    fn presence_gates_the_first_send() {
        let here = Presence::Here {
            away_in: Duration::from_secs(90),
        };
        assert_eq!(gate(PushKind::Approval, Presence::Away), Gate::Send);
        assert_eq!(gate(PushKind::Success, Presence::Away), Gate::Send);
        assert_eq!(
            gate(PushKind::Approval, here),
            Gate::Hold(Duration::from_secs(90))
        );
        assert_eq!(
            gate(PushKind::Input, here),
            Gate::Hold(Duration::from_secs(90))
        );
        assert_eq!(gate(PushKind::Failure, here), Gate::Skip);
        assert_eq!(gate(PushKind::Success, here), Gate::Skip);
    }

    /// 扣着的提醒到点复查；人离开时提前放出来，并且直接发。
    #[test]
    fn held_jobs_wait_until_due_or_released() {
        let mut outbox = Outbox::default();
        let start = Instant::now();
        outbox.notify("a".into(), PushKind::Approval, Content::default(), start);
        let job = outbox.take_due(start).pop().unwrap();
        let recheck = start + Duration::from_secs(120);
        outbox.hold(job, recheck);
        assert!(outbox.take_due(start).is_empty());
        assert_eq!(outbox.next_due(), Some(recheck));

        let later = start + Duration::from_secs(10);
        assert!(outbox.release_held(later));
        let job = outbox.take_due(later).pop().unwrap();
        assert!(job.held && job.released);
        assert_eq!(job.attempt, 0, "还没发过");
        assert!(!outbox.release_held(later), "没有扣着的了");
    }

    /// 扣着期间处理掉（取消）或被新提醒替换的，不再回来。
    #[test]
    fn cancelled_or_replaced_jobs_are_not_held_again() {
        let mut outbox = Outbox::default();
        let start = Instant::now();
        outbox.notify("a".into(), PushKind::Approval, Content::default(), start);
        let job = outbox.take_due(start).pop().unwrap();
        outbox.cancel("a");
        outbox.hold(job, start + Duration::from_secs(60));
        assert_eq!(outbox.next_due(), None);

        outbox.notify("b".into(), PushKind::Approval, Content::default(), start);
        let job = outbox.take_due(start).pop().unwrap();
        outbox.hold(job, start + Duration::from_secs(60));
        outbox.cancel("b");
        assert_eq!(outbox.next_due(), None, "扣着时处理掉就不补推");
    }

    #[test]
    fn discarded_jobs_are_forgotten() {
        let mut outbox = Outbox::default();
        let start = Instant::now();
        outbox.notify("a".into(), PushKind::Success, Content::default(), start);
        let job = outbox.take_due(start).pop().unwrap();
        outbox.discard(job);
        assert!(outbox.live.is_empty());
        assert_eq!(outbox.next_due(), None);
    }

    #[test]
    fn jobs_are_due_immediately() {
        let mut outbox = Outbox::default();
        let start = Instant::now();
        outbox.notify("a".into(), PushKind::Approval, Content::default(), start);
        assert_eq!(outbox.next_due(), Some(start));
        let due = outbox.take_due(start);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].session_id, "a");
    }

    /// 还没发出去就处理掉的不推；同一会话的新提醒替换旧的。
    #[test]
    fn cancel_and_replace_before_sending() {
        let mut outbox = Outbox::default();
        let start = Instant::now();
        outbox.notify("a".into(), PushKind::Approval, Content::default(), start);
        outbox.cancel("a");
        assert!(outbox.take_due(start).is_empty());
        assert_eq!(outbox.next_due(), None);

        outbox.notify("b".into(), PushKind::Approval, Content::default(), start);
        outbox.notify("b".into(), PushKind::Input, Content::default(), start);
        let due = outbox.take_due(start);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].kind, PushKind::Input);
    }

    #[test]
    fn failed_targets_retry_with_backoff_until_the_ttl() {
        let mut outbox = Outbox::default();
        let start = Instant::now();
        outbox.notify("a".into(), PushKind::Approval, Content::default(), start);
        let mut job = outbox.take_due(start).pop().unwrap();
        let apns_id = job.apns_id.clone();

        let mut now = start;
        let mut attempts = 0;
        loop {
            job.targets = Some(vec![target("phone")]);
            let expired = outbox.finish(job, now);
            let Some(due) = outbox.next_due() else {
                assert!(expired, "过期放弃时报出来");
                break;
            };
            assert!(!expired);
            attempts += 1;
            assert!(due <= start + TTL);
            now = due;
            job = outbox.take_due(now).pop().unwrap();
            assert_eq!(job.apns_id, apns_id, "重试用同一个 apns-id");
        }
        assert!(attempts >= 5, "{attempts}");
        assert!(outbox.live.is_empty());
    }

    #[test]
    fn delivered_or_cancelled_jobs_are_not_requeued() {
        let mut outbox = Outbox::default();
        let start = Instant::now();
        outbox.notify("a".into(), PushKind::Approval, Content::default(), start);
        let mut job = outbox.take_due(start).pop().unwrap();
        job.targets = Some(Vec::new());
        assert!(!outbox.finish(job, start));
        assert_eq!(outbox.next_due(), None);
        assert!(outbox.live.is_empty());

        outbox.notify("b".into(), PushKind::Approval, Content::default(), start);
        let mut job = outbox.take_due(start).pop().unwrap();
        outbox.cancel("b");
        job.targets = Some(vec![target("phone")]);
        assert!(!outbox.finish(job, start));
        assert_eq!(outbox.next_due(), None, "发送期间取消的不再排队");
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff(1), BACKOFF_BASE);
        assert_eq!(backoff(2), BACKOFF_BASE * 2);
        assert_eq!(backoff(3), BACKOFF_BASE * 4);
        assert_eq!(backoff(30), BACKOFF_MAX);
    }

    #[test]
    fn relay_statuses_map_to_outcomes() {
        assert_eq!(outcome_of(200), Outcome::Delivered);
        assert_eq!(outcome_of(410), Outcome::Gone);
        assert_eq!(outcome_of(400), Outcome::Rejected);
        assert_eq!(outcome_of(429), Outcome::Retry);
        assert_eq!(outcome_of(502), Outcome::Retry);
    }
}
