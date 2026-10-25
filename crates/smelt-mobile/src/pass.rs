//! 指挥台投影：把所有机器的会话列表拼成单据与灶台。Dart 入口见 `crate::api::pass`。
//!
//! 「谁在等你」由各机器的会话列表直接给出；「在等什么」要对那个会话做一次
//! `pending_actions`（一问一答，不占对话订阅）。所以一张单据先带着会话摘要出现
//! （`details_loaded = false`），详情取回后再补全。会话摘要里和「在等什么」相关的字段
//! （[`fingerprint`]）一变就重取，旧单据不会挂着已经答过的问题。
//!
//! ```text
//! 各节点 Catalog ──Sessions──> PassHub::update ──> project() ──变了──> Dart 订阅者
//!                                     │ 缺详情的单据
//!                                     v
//!                     取详情线程（pending_actions）──> 记下详情、再投影一次
//! ```
//!
//! 取详情失败（断线、会话唤醒超时）就在取详情线程里退避重取，直到取到、单据没了或指纹
//! 变了：在等你的会话列表不会再变，等不来下一次推送替它重取。
//!
//! 锁的顺序：会话目录的锁 → 本模块的锁（`update` 在会话目录的监听里调）。持有本模块的
//! 锁时不调会话目录、不做网络 IO；取详情在单独的线程上做。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::api::catalog::{
    AttentionKind, SessionList, SessionPhase, SessionStatus, SessionSummary,
};
use crate::api::conversation::PendingActions;
use crate::api::error::SmeltError;
use crate::api::pass::{Pass, PassStation, PassTicket};

/// 灶台「最近跑过」一组最多几条。
pub(crate) const RECENT_LIMIT: usize = 5;

/// 取一个会话在等的审批和询问：`(runtime_id, session_id)`。阻塞，只在取详情线程上调。
pub(crate) type Fetch = Arc<dyn Fn(&str, &str) -> Result<PendingActions, SmeltError> + Send + Sync>;

/// 取详情失败后的重取退避。生产值见 [`Timing::PRODUCTION`]，测试用毫秒级。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    pub retry_base: Duration,
    pub retry_max: Duration,
}

impl Timing {
    pub(crate) const PRODUCTION: Self = Self {
        retry_base: Duration::from_secs(2),
        retry_max: Duration::from_secs(30),
    };

    fn retry_delay(&self, failures: u32) -> Duration {
        let factor = 1u32 << failures.saturating_sub(1).min(16);
        self.retry_base.saturating_mul(factor).min(self.retry_max)
    }
}

/// Dart 订阅者：返回 false 表示已取消。在本模块的锁里调用，不能阻塞。
pub(crate) type PassListener = Box<dyn Fn(&Pass) -> bool + Send>;

/// `(runtime_id, session_id)`。
type Key = (String, String);

/// 审批、输入、失败是要人处理的事；完成、响铃、通知只是告知。
fn requires_action(kind: AttentionKind) -> bool {
    matches!(
        kind,
        AttentionKind::Approval | AttentionKind::Input | AttentionKind::Failure
    )
}

/// 要你处理：有待处理的提醒，或状态是 `NeedsYou`。
pub(crate) fn needs_action(session: &SessionSummary) -> bool {
    session
        .attention
        .as_ref()
        .is_some_and(|attention| requires_action(attention.kind))
        || session.status == SessionStatus::NeedsYou
}

fn is_running(session: &SessionSummary) -> bool {
    session.status == SessionStatus::Running
}

/// 这台手机上还没读的提醒。
fn unread(session: &SessionSummary) -> bool {
    session.attention.is_some()
}

/// 刚完成：成功结束、还没看过。看过提醒就退出这一组。
fn is_done(session: &SessionSummary) -> bool {
    !needs_action(session)
        && !is_running(session)
        && unread(session)
        && session.phase == SessionPhase::Succeeded
}

/// 最近跑过：成功结束、看过了。接住从「刚完成」里掉出来的，免得一次跑完的会话在指挥台
/// 上凭空没了。运行时已结束的会话相位是 `Dead`，不在这一组。
fn is_recent(session: &SessionSummary) -> bool {
    !needs_action(session)
        && !is_running(session)
        && !is_done(session)
        && session.phase == SessionPhase::Succeeded
}

/// 会话摘要里跟「在等什么」有关的部分。变了就要重取详情。
pub(crate) fn fingerprint(session: &SessionSummary) -> String {
    let attention = session.attention.as_ref();
    [
        format!("{:?}", session.phase),
        format!("{:?}", session.status),
        session.updated_at.to_string(),
        session.detail.clone().unwrap_or_default(),
        attention.map_or(String::new(), |a| format!("{:?}", a.kind)),
        attention.map_or(String::new(), |a| a.message.clone()),
    ]
    .join("\u{1}")
}

/// 某一组：按最后活动时间从新到旧，同时间的保持列表次序（运行时按桌面菜单排好的，比
/// 随机顺序有意义；从没动过的会话时间都是 0，全靠它）。
fn section(
    sessions: &[SessionSummary],
    predicate: impl Fn(&SessionSummary) -> bool,
) -> Vec<SessionSummary> {
    let mut matched: Vec<SessionSummary> = sessions
        .iter()
        .filter(|session| predicate(session))
        .cloned()
        .collect();
    // 稳定排序：同时间的保持原序。
    matched.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
    matched
}

/// 取回的详情和取的时候会话摘要的指纹。
#[derive(Clone, Debug)]
pub(crate) struct Loaded {
    pub fingerprint: String,
    pub actions: PendingActions,
}

/// 一次投影的输入。
pub(crate) struct Inputs<'a> {
    /// 机器顺序。
    pub order: &'a [String],
    pub lists: &'a HashMap<String, SessionList>,
    pub details: &'a HashMap<Key, Loaded>,
    /// 单据第一次出现的时间（unix 毫秒）。新单据记进去，不再是单据的移出去。
    pub first_seen: &'a mut HashMap<Key, i64>,
    pub now_ms: i64,
}

/// 投影出整份指挥台。缺详情（或详情已过时）的单据 `details_loaded = false`。
pub(crate) fn project(inputs: Inputs<'_>) -> Pass {
    let empty = SessionList {
        sessions: Vec::new(),
        cached: true,
        saved_at_ms: None,
    };
    // 排序键：等待起点，再按机器顺序、列表次序。
    let mut tickets: Vec<((i64, usize, usize), PassTicket)> = Vec::new();
    let mut stations = Vec::with_capacity(inputs.order.len());
    let mut current = HashSet::new();
    for (runtime_index, runtime_id) in inputs.order.iter().enumerate() {
        let list = inputs.lists.get(runtime_id).unwrap_or(&empty);
        if !list.cached {
            for (session_index, session) in list.sessions.iter().enumerate() {
                if !needs_action(session) {
                    continue;
                }
                let key = (runtime_id.clone(), session.id.clone());
                let seen = *inputs
                    .first_seen
                    .entry(key.clone())
                    .or_insert(inputs.now_ms);
                let waiting_since = if session.updated_at > 0 {
                    session.updated_at.saturating_mul(1000)
                } else {
                    seen
                };
                let fingerprint = fingerprint(session);
                let loaded = inputs
                    .details
                    .get(&key)
                    .filter(|loaded| loaded.fingerprint == fingerprint);
                current.insert(key);
                tickets.push((
                    (waiting_since, runtime_index, session_index),
                    PassTicket {
                        runtime_id: runtime_id.clone(),
                        session: session.clone(),
                        permission: loaded
                            .and_then(|loaded| loaded.actions.permissions.first().cloned()),
                        elicitation: loaded.and_then(|loaded| loaded.actions.elicitation.clone()),
                        details_loaded: loaded.is_some(),
                        recent: loaded
                            .map(|loaded| loaded.actions.recent.clone())
                            .unwrap_or_default(),
                        waiting_since,
                    },
                ));
            }
        }
        let mut recent = section(&list.sessions, is_recent);
        recent.truncate(RECENT_LIMIT);
        stations.push(PassStation {
            runtime_id: runtime_id.clone(),
            running: section(&list.sessions, is_running),
            done: section(&list.sessions, is_done),
            recent,
            cached: list.cached,
            saved_at_ms: list.saved_at_ms,
        });
    }
    inputs.first_seen.retain(|key, _| current.contains(key));
    tickets.sort_by_key(|(order, _)| *order);
    Pass {
        tickets: tickets.into_iter().map(|(_, ticket)| ticket).collect(),
        stations,
    }
}

/// 进程里一份（挂在机队上），接各节点的会话列表、推给 Dart。
pub(crate) struct PassHub {
    shared: Arc<Shared>,
}

struct Shared {
    fetch: Fetch,
    timing: Timing,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    order: Vec<String>,
    lists: HashMap<String, SessionList>,
    /// 已取回的详情。会话从列表里消失时丢掉；只是暂时不是单据（机器重连中）的留着。
    details: HashMap<Key, Loaded>,
    /// 正在取的单据和取的时候的指纹，避免同一状态反复请求。
    in_flight: HashMap<Key, String>,
    first_seen: HashMap<Key, i64>,
    listeners: Vec<PassListener>,
    last: Option<Pass>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl PassHub {
    pub(crate) fn new(fetch: Fetch, timing: Timing) -> Self {
        Self {
            shared: Arc::new(Shared {
                fetch,
                timing,
                inner: Mutex::new(Inner::default()),
            }),
        }
    }

    /// 订阅：先推一次当前整份。
    pub(crate) fn subscribe(&self, listener: PassListener) {
        let mut inner = self.shared.lock();
        let pass = refresh(&mut inner);
        if listener(&pass) {
            inner.listeners.push(listener);
        }
        // 之前没人订阅时不取详情，现在有了。
        let requests = claim_missing(&mut inner, &pass);
        drop(inner);
        spawn_fetches(&self.shared, requests);
    }

    /// 机器顺序变了（配对、忘记、挪动）。不在顺序里的机器的列表一并丢掉。
    pub(crate) fn set_order(&self, order: Vec<String>) {
        let mut inner = self.shared.lock();
        inner
            .lists
            .retain(|runtime_id, _| order.contains(runtime_id));
        inner.order = order;
        let requests = update_locked(&mut inner);
        drop(inner);
        spawn_fetches(&self.shared, requests);
    }

    /// 一台机器推来新的会话列表。在会话目录的锁里调用：只改内存、推事件、起线程。
    pub(crate) fn update(&self, runtime_id: &str, list: SessionList) {
        let mut inner = self.shared.lock();
        if !inner.order.iter().any(|id| id == runtime_id) {
            return;
        }
        inner.lists.insert(runtime_id.to_string(), list);
        let requests = update_locked(&mut inner);
        drop(inner);
        spawn_fetches(&self.shared, requests);
    }
}

/// 重新投影，变了就推，返回要发出的取详情请求。
fn update_locked(inner: &mut Inner) -> Vec<(Key, String)> {
    let pass = refresh(inner);
    claim_missing(inner, &pass)
}

/// 重新投影并推送（和上次一样就不推），返回这次的整份。
fn refresh(inner: &mut Inner) -> Pass {
    let Inner {
        order,
        lists,
        details,
        in_flight,
        first_seen,
        listeners,
        last,
    } = inner;
    let pass = project(Inputs {
        order,
        lists,
        details,
        first_seen,
        now_ms: crate::store::now_ms(),
    });
    let tickets: HashSet<Key> = pass
        .tickets
        .iter()
        .map(|ticket| (ticket.runtime_id.clone(), ticket.session.id.clone()))
        .collect();
    // 详情留到会话从列表里消失：机器重连（换代）时列表先退回缓存、单据暂时下架，连回来
    // 指纹没变就直接用，不用整台重取一遍。指纹带着最后活动时间，变过就会重取。
    let sessions: HashSet<Key> = lists
        .iter()
        .flat_map(|(runtime_id, list)| {
            list.sessions
                .iter()
                .map(move |session| (runtime_id.clone(), session.id.clone()))
        })
        .collect();
    details.retain(|key, _| sessions.contains(key));
    in_flight.retain(|key, _| tickets.contains(key));
    if last.as_ref() != Some(&pass) {
        *last = Some(pass.clone());
        listeners.retain(|listener| listener(&pass));
    }
    pass
}

/// 挑出缺详情、又没在用同一指纹取的单据，记为在取。没有订阅者时不取。
fn claim_missing(inner: &mut Inner, pass: &Pass) -> Vec<(Key, String)> {
    if inner.listeners.is_empty() {
        return Vec::new();
    }
    let missing = pass
        .tickets
        .iter()
        .filter(|ticket| !ticket.details_loaded)
        .map(|ticket| {
            (
                (ticket.runtime_id.clone(), ticket.session.id.clone()),
                fingerprint(&ticket.session),
            )
        });
    let mut requests = Vec::new();
    for (key, fingerprint) in missing {
        if inner.in_flight.get(&key) == Some(&fingerprint) {
            continue;
        }
        inner.in_flight.insert(key.clone(), fingerprint.clone());
        requests.push((key, fingerprint));
    }
    requests
}

fn spawn_fetches(shared: &Arc<Shared>, requests: Vec<(Key, String)>) {
    for (key, fingerprint) in requests {
        let thread_shared = Arc::clone(shared);
        let thread_key = key.clone();
        let spawned = std::thread::Builder::new()
            .name("smelt-pass-details".to_string())
            .spawn(move || fetch(&thread_shared, thread_key, fingerprint));
        if let Err(spawn_error) = spawned {
            log::error!("起不了取单据详情的线程：{spawn_error}");
            shared.lock().in_flight.remove(&key);
        }
    }
}

/// 取一张单据的详情，失败就退避重取。期间一直占着 `in_flight`：同一指纹不会另起请求。
fn fetch(shared: &Arc<Shared>, key: Key, fingerprint: String) {
    let mut failures = 0u32;
    loop {
        // 睡醒（或线程刚起来）先看单据还在不在。先打再看的话，单据已经清掉的那一轮
        // 仍会发出去。
        if shared.lock().in_flight.get(&key) != Some(&fingerprint) {
            return;
        }
        let result = (shared.fetch)(&key.0, &key.1);
        let mut inner = shared.lock();
        if inner.in_flight.get(&key) != Some(&fingerprint) {
            // 取的期间指纹又变了（已有新的请求），或单据没了：这份作废。
            return;
        }
        match result {
            Ok(actions) => {
                inner.in_flight.remove(&key);
                inner.details.insert(
                    key,
                    Loaded {
                        fingerprint,
                        actions,
                    },
                );
                let requests = update_locked(&mut inner);
                drop(inner);
                spawn_fetches(shared, requests);
                return;
            }
            Err(failure) => {
                drop(inner);
                failures = failures.saturating_add(1);
                log::info!(
                    "取单据详情失败（{}，第 {failures} 次）：{}",
                    key.1,
                    failure.message
                );
                std::thread::sleep(shared.timing.retry_delay(failures));
            }
        }
    }
}

#[cfg(test)]
mod tests;
