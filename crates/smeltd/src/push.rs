//! 手机推送：提醒经推送中继（APNs）发给登记过的手机。
//!
//! 推什么由 `crate::attention` 定（和桌面、手机读的同一份提醒）：等批准、等输入、失败、完成
//! 四种响一下时推，待办处理掉、会话删掉时撤（[`apply`]）。每台手机按自己登记的类型过滤
//! （`push_registration` 表）。推送带会话名、项目名和一行摘要，摘要就是会话状态里的
//! `pending_question`（桌面通知、手机会话列表读的同一份）。
//!
//! 提醒一产生就进内存 outbox。发之前看人在不在这台 Mac 前（`crate::presence`）：
//! - 不在：立刻发。
//! - 在：Mac 上已经弹了系统通知，手机不响。等批准、等回复先扣着，人一离开（锁屏、本机桌面
//!   App 断开、键鼠闲置满 `presence::AWAY_AFTER`）还没处理就补推；完成、失败不推。
//!
//! 智能体主动发的消息（[`send_message`]，见 `crate::agent_notify`）不看人在不在、立刻发；每条
//! 各占一个位置，不被同一会话后来的提醒替换，会话结束、被删也不撤。
//!
//! 发送失败按退避重试，超过 [`TTL`] 丢弃；重试期间事情已经处理掉（待办解决、会话删除）就
//! 不再重试。outbox 不落盘：smeltd 重启后不补发重启前的提醒。
//!
//! 发送线程 `smelt-push-send` 阻塞在 outbox 的 Condvar 上，只在有提醒要发、重试到点、扣着的
//! 提醒该复查、或人离开时醒来。没有待发的提醒时不醒。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crate::presence::Presence;
use smelt_core::daemon_state::DaemonSessionState;
use smelt_core::remote_config::PUSH_PAYLOAD_VERSION;
use smelt_push::{
    DETAIL_MAX_CHARS, MESSAGE_MAX_CHARS, PushKind, RELAY_SEND_PATH, SUBTITLE_MAX_CHARS,
    SendRequest, TITLE_MAX_CHARS, excerpt,
};

/// 从提醒产生算起，超过这么久还没送出去就丢掉：过时的「等你批准」不如不推。
const TTL: Duration = Duration::from_secs(10 * 60);
/// 失败后第一次重试的间隔，之后翻倍，封顶 [`BACKOFF_MAX`]。
const BACKOFF_BASE: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(2 * 60);
/// 一次发给中继的请求的时限。
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// 发送线程起来之后才有：测试进程、没有运行时库时为空，智能体消息只弹桌面。
static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

pub(crate) fn spawn() {
    // 测试进程没有运行时库，也就没有登记，不必起线程。
    if !smelt_core::sqlite_state::is_sqlite_state_enabled() {
        return;
    }
    let shared = Arc::new(Shared::default());
    let _ = SHARED.set(Arc::clone(&shared));
    let away_shared = Arc::clone(&shared);
    crate::presence::on_away(move || away_shared.release_held());
    if let Err(error) = std::thread::Builder::new()
        .name("smelt-push-send".to_string())
        .spawn(move || run_sender(&shared))
    {
        crate::dlog(&format!("推送：起不了发送线程：{error}"));
    }
}

/// 提醒响了、处理掉了：交给 outbox。推送没在跑（测试进程、没有运行时库）时什么都不做。
pub(crate) fn apply(command: Command) {
    if let Some(shared) = SHARED.get() {
        shared.apply(vec![command]);
    }
}

/// 智能体主动发的一条消息：立刻推给登记了「智能体消息」的手机，返回有几台。推送没在跑、
/// 没有手机要它时返回 0，不排队。
///
/// `session_id` 用于手机上按会话分组和点开跳转；没有会话（命令行发的）时为空串。
pub(crate) fn send_message(
    session_id: &str,
    title: Option<&str>,
    subtitle: Option<&str>,
    body: &str,
) -> usize {
    let Some(shared) = SHARED.get() else {
        return 0;
    };
    let phones = match registered_targets(PushKind::Message) {
        Ok(targets) => targets.len(),
        Err(error) => {
            crate::dlog(&format!("推送：读登记失败：{error}"));
            return 0;
        }
    };
    if phones == 0 {
        return 0;
    }
    let content = Content {
        title: title.and_then(|title| excerpt(title, TITLE_MAX_CHARS)),
        subtitle: subtitle.and_then(|subtitle| excerpt(subtitle, SUBTITLE_MAX_CHARS)),
        detail: excerpt(body, MESSAGE_MAX_CHARS),
    };
    shared.apply(vec![Command::Message {
        session_id: session_id.to_string(),
        content,
    }]);
    phones
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// 这段会话有了一条要推的提醒，替换它之前还没推出去的那条。
    Notify {
        session_id: String,
        kind: PushKind,
        content: Content,
    },
    /// 这段会话的事已经处理掉（或会话没了），还没推出去的不推了。
    Cancel { session_id: String },
    /// 智能体主动发的消息，见 [`send_message`]。
    Message {
        session_id: String,
        content: Content,
    },
}

/// 推送里显示的文字，取自提醒产生那一刻的会话状态，已按 `smelt_push` 的上限截好。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Content {
    /// 会话名：用户起的名字优先，其次是智能体报的标题。
    title: Option<String>,
    /// 项目名：工作目录的最后一级。
    subtitle: Option<String>,
    /// 一行摘要：等批准的命令、等回复的问题、失败原因、最后一条回复的开头。
    detail: Option<String>,
}

impl Content {
    pub(crate) fn of(state: &DaemonSessionState) -> Self {
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
    /// 在 outbox 里占的位置。会话提醒是会话 id：同一会话只留最新一条，处理掉就撤；智能体
    /// 消息每条各占一个，不被替换、不被撤。
    key: String,
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
    /// 每个位置（[`Job::key`]）当前有效的那条。
    live: HashMap<String, u64>,
    next_serial: u64,
}

impl Outbox {
    /// 新提醒立刻到点。同一会话还在重试的旧提醒作废：积压恢复后只推最新的那条。
    fn notify(&mut self, session_id: String, kind: PushKind, content: Content, now: Instant) {
        self.jobs.retain(|job| job.key != session_id);
        let apns_id = uuid::Uuid::new_v4().to_string();
        self.enqueue(session_id.clone(), session_id, kind, content, apns_id, now);
    }

    /// 智能体的消息立刻到点，自己占一个位置。
    fn message(&mut self, session_id: String, content: Content, now: Instant) {
        let apns_id = uuid::Uuid::new_v4().to_string();
        let key = format!("message:{apns_id}");
        self.enqueue(key, session_id, PushKind::Message, content, apns_id, now);
    }

    fn enqueue(
        &mut self,
        key: String,
        session_id: String,
        kind: PushKind,
        content: Content,
        apns_id: String,
        now: Instant,
    ) {
        let serial = self.next_serial;
        self.next_serial += 1;
        self.live.insert(key.clone(), serial);
        self.jobs.push(Job {
            serial,
            key,
            session_id,
            kind,
            content,
            apns_id,
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
        if self.live.get(&job.key) != Some(&job.serial) {
            return;
        }
        job.held = true;
        job.due = until;
        self.jobs.push(job);
    }

    /// 不发了（人在 Mac 前，完成 / 失败已经在 Mac 上看到）。
    fn discard(&mut self, job: Job) {
        if self.live.get(&job.key) == Some(&job.serial) {
            self.live.remove(&job.key);
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

    /// 撤掉这段会话的提醒。智能体消息不在会话的位置上，不受影响。
    fn cancel(&mut self, session_id: &str) {
        self.jobs.retain(|job| job.key != session_id);
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
        if self.live.get(&job.key) != Some(&job.serial) {
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
        self.live.remove(&job.key);
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
                Command::Message {
                    session_id,
                    content,
                } => outbox.message(session_id, content, now),
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
/// 智能体的消息是它判断过值得打扰才发的，人在不在都发。
fn gate(kind: PushKind, presence: Presence) -> Gate {
    match presence {
        _ if kind == PushKind::Message => Gate::Send,
        Presence::Away => Gate::Send,
        Presence::Here { away_in } if kind.is_request() => Gate::Hold(away_in),
        Presence::Here { .. } => Gate::Skip,
    }
}

/// 发给一台手机的请求。旧 App 不带摘要：它的文案文件里没有带摘要的 loc-key。智能体消息的
/// 正文走 `body`，不经文案文件，都带。
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
        detail: job
            .content
            .detail
            .clone()
            .filter(|_| target.rich || job.kind == PushKind::Message),
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
            PushKind::Message => registration.notify_message,
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

    /// 文字取提醒产生那一刻的会话状态：会话名优先用户起的，项目名取目录末段，摘要是问句。
    #[test]
    fn content_comes_from_the_session_state() {
        let mut waiting = state("a", DaemonPhase::AwaitingApproval);
        waiting.title = Some("智能体起的名".into());
        waiting.custom_title = Some("修推送".into());
        waiting.cwd = Some("/Users/cc/wawa/smelt/".into());
        waiting.pending_question = Some("rm -rf\n  build".into());
        assert_eq!(
            Content::of(&waiting),
            Content {
                title: Some("修推送".into()),
                subtitle: Some("smelt".into()),
                detail: Some("rm -rf build".into()),
            }
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
        assert_eq!(
            gate(PushKind::Message, here),
            Gate::Send,
            "智能体判断过值得打扰才发，人在 Mac 前也推"
        );
        assert_eq!(gate(PushKind::Message, Presence::Away), Gate::Send);
    }

    /// 智能体消息各占一个位置：不替换同一会话的提醒，也不被它替换；会话的提醒处理掉、会话
    /// 被删时不撤。
    #[test]
    fn messages_are_independent_of_session_alerts() {
        let mut outbox = Outbox::default();
        let start = Instant::now();
        outbox.notify("a".into(), PushKind::Approval, Content::default(), start);
        outbox.message("a".into(), Content::default(), start);
        outbox.message("a".into(), Content::default(), start);
        outbox.notify("a".into(), PushKind::Success, Content::default(), start);
        outbox.cancel("a");
        let due = outbox.take_due(start);
        assert_eq!(due.len(), 2, "两条消息都在，会话提醒撤了");
        assert!(due.iter().all(|job| job.kind == PushKind::Message));
        assert_ne!(due[0].apns_id, due[1].apns_id);
        assert!(due.iter().all(|job| job.session_id == "a"));
        for job in due {
            assert!(!outbox.finish(Job {
                targets: Some(Vec::new()),
                ..job
            }, start));
        }
        assert!(outbox.live.is_empty());
    }

    /// 消息正文走 `body`，旧 App 也显示得了：都带上。
    #[test]
    fn message_body_goes_to_legacy_targets_too() {
        let mut outbox = Outbox::default();
        let start = Instant::now();
        let content = Content {
            title: Some("巡检".into()),
            subtitle: None,
            detail: Some("3 台机器磁盘告警".into()),
        };
        outbox.message("a".into(), content, start);
        let job = outbox.take_due(start).pop().unwrap();
        let mut legacy = target("old");
        legacy.rich = false;
        let request = send_request(&job, &legacy, "runtime");
        assert_eq!(request.kind, PushKind::Message);
        assert_eq!(request.detail.as_deref(), Some("3 台机器磁盘告警"));
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
