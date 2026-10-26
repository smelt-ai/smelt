//! 指挥台投影单测：排序、分段、指纹变了重取详情、非实时列表不出单据。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::*;
use crate::api::catalog::{AttentionItem, SessionKind};
use crate::api::conversation::{
    ApprovalDetails, ApprovalKind, PermissionOption, PermissionOptionKind, PermissionRequest,
};
use crate::api::error::{SmeltErrorKind, error};

fn session(
    id: &str,
    status: SessionStatus,
    phase: SessionPhase,
    updated_at: i64,
) -> SessionSummary {
    SessionSummary {
        id: id.to_string(),
        kind: SessionKind::Acp,
        title: format!("{id} 标题"),
        phase,
        status,
        agent: "claude".to_string(),
        cwd: None,
        project_root: None,
        project_title: None,
        project_order: 0,
        updated_at,
        detail: None,
        attention: None,
        automation: None,
        agent_definition_id: None,
    }
}

fn with_attention(mut session: SessionSummary, kind: AttentionKind) -> SessionSummary {
    session.attention = Some(AttentionItem {
        session_id: session.id.clone(),
        title: session.title.clone(),
        message: format!("{kind:?}"),
        kind,
    });
    session
}

fn waiting(id: &str, updated_at: i64) -> SessionSummary {
    with_attention(
        session(
            id,
            SessionStatus::NeedsYou,
            SessionPhase::AwaitingApproval,
            updated_at,
        ),
        AttentionKind::Approval,
    )
}

fn live(sessions: Vec<SessionSummary>) -> SessionList {
    SessionList {
        sessions,
        cached: false,
        saved_at_ms: None,
    }
}

fn permission(tool_call_id: &str) -> PermissionRequest {
    PermissionRequest {
        tool_call_id: tool_call_id.to_string(),
        question: format!("可以跑 {tool_call_id} 吗"),
        options: vec![PermissionOption {
            option_id: "allow".to_string(),
            name: "允许".to_string(),
            kind: PermissionOptionKind::AllowOnce,
        }],
        details: ApprovalDetails {
            kind: ApprovalKind::Generic,
            command: None,
            cwd: None,
            reason: None,
            grant_root: None,
            summary: None,
        },
    }
}

fn actions(tool_call_ids: &[&str]) -> PendingActions {
    PendingActions {
        permissions: tool_call_ids.iter().map(|id| permission(id)).collect(),
        elicitation: None,
        recent: Vec::new(),
    }
}

fn key(runtime: &str, session: &str) -> Key {
    (runtime.to_string(), session.to_string())
}

struct World {
    order: Vec<String>,
    lists: HashMap<String, SessionList>,
    details: HashMap<Key, Loaded>,
    first_seen: HashMap<Key, i64>,
}

impl World {
    fn new(runtimes: &[&str]) -> Self {
        Self {
            order: runtimes.iter().map(|id| id.to_string()).collect(),
            lists: HashMap::new(),
            details: HashMap::new(),
            first_seen: HashMap::new(),
        }
    }

    fn list(&mut self, runtime: &str, list: SessionList) {
        self.lists.insert(runtime.to_string(), list);
    }

    fn project_at(&mut self, now_ms: i64) -> Pass {
        project(Inputs {
            order: &self.order,
            lists: &self.lists,
            details: &self.details,
            first_seen: &mut self.first_seen,
            now_ms,
        })
    }

    fn project(&mut self) -> Pass {
        self.project_at(1)
    }
}

fn ticket_ids(pass: &Pass) -> Vec<(String, String)> {
    pass.tickets
        .iter()
        .map(|ticket| (ticket.runtime_id.clone(), ticket.session.id.clone()))
        .collect()
}

fn ids(sessions: &[SessionSummary]) -> Vec<&str> {
    sessions.iter().map(|session| session.id.as_str()).collect()
}

// ---------------------------------------------------------------------------
// 判定
// ---------------------------------------------------------------------------

#[test]
fn what_needs_action_and_what_is_just_news() {
    let idle = session("s", SessionStatus::Idle, SessionPhase::Idle, 1);
    for kind in [
        AttentionKind::Approval,
        AttentionKind::Input,
        AttentionKind::Failure,
    ] {
        assert!(
            needs_action(&with_attention(idle.clone(), kind)),
            "{kind:?}"
        );
    }
    for kind in [
        AttentionKind::Success,
        AttentionKind::Bell,
        AttentionKind::Notice,
    ] {
        assert!(
            !needs_action(&with_attention(idle.clone(), kind)),
            "{kind:?}"
        );
    }
    assert!(needs_action(&session(
        "s",
        SessionStatus::NeedsYou,
        SessionPhase::Failed,
        1
    )));
    assert!(!needs_action(&idle));
}

// ---------------------------------------------------------------------------
// 单据
// ---------------------------------------------------------------------------

#[test]
fn tickets_from_every_runtime_oldest_first() {
    let mut world = World::new(&["a", "b"]);
    world.list(
        "a",
        live(vec![
            waiting("a-new", 300),
            session("a-run", SessionStatus::Running, SessionPhase::Thinking, 400),
            waiting("a-old", 100),
        ]),
    );
    world.list(
        "b",
        live(vec![
            waiting("b-mid", 200),
            with_attention(
                session("b-fail", SessionStatus::Idle, SessionPhase::Failed, 100),
                AttentionKind::Failure,
            ),
        ]),
    );
    let pass = world.project();
    assert_eq!(
        ticket_ids(&pass),
        [
            key("a", "a-old"),
            key("b", "b-fail"),
            key("b", "b-mid"),
            key("a", "a-new"),
        ],
        "先来先处理；同时间按机器顺序、再按列表次序"
    );
    assert_eq!(pass.tickets[0].waiting_since, 100_000, "unix 秒换成毫秒");
    assert!(pass.tickets.iter().all(|ticket| !ticket.details_loaded));
}

#[test]
fn a_ticket_without_activity_time_waits_since_it_was_first_seen() {
    let mut world = World::new(&["a"]);
    world.list("a", live(vec![waiting("s", 0)]));
    assert_eq!(world.project_at(5_000).tickets[0].waiting_since, 5_000);
    assert_eq!(
        world.project_at(9_000).tickets[0].waiting_since,
        5_000,
        "还在等就不变"
    );
    world.list("a", live(Vec::new()));
    world.project_at(10_000);
    world.list("a", live(vec![waiting("s", 0)]));
    assert_eq!(
        world.project_at(12_000).tickets[0].waiting_since,
        12_000,
        "处理完又来的是新单据"
    );
}

#[test]
fn details_only_count_for_the_same_fingerprint() {
    let mut world = World::new(&["a"]);
    let first = waiting("s", 100);
    world.list("a", live(vec![first.clone()]));
    world.details.insert(
        key("a", "s"),
        Loaded {
            fingerprint: fingerprint(&first),
            actions: actions(&["tool-1", "tool-2"]),
        },
    );
    let ticket = &world.project().tickets[0];
    assert!(ticket.details_loaded);
    assert_eq!(
        ticket.permission.as_ref().unwrap().tool_call_id,
        "tool-1",
        "带第一个待批的审批"
    );

    // 新的审批来了：摘要变了，旧详情不能再用（按钮会批到已经答过的那个）。
    let mut next = first.clone();
    next.updated_at = 200;
    next.detail = Some("rm -rf build".to_string());
    world.list("a", live(vec![next]));
    let ticket = &world.project().tickets[0];
    assert!(!ticket.details_loaded);
    assert!(ticket.permission.is_none());
}

#[test]
fn fingerprint_tracks_only_what_the_session_waits_for() {
    let base = waiting("s", 100);
    let mut renamed = base.clone();
    renamed.title = "改了名".to_string();
    renamed.project_order = 7;
    assert_eq!(fingerprint(&base), fingerprint(&renamed));
    for change in [
        |s: &mut SessionSummary| s.phase = SessionPhase::WaitingForUser,
        |s: &mut SessionSummary| s.status = SessionStatus::Idle,
        |s: &mut SessionSummary| s.updated_at = 101,
        |s: &mut SessionSummary| s.detail = Some("x".to_string()),
        |s: &mut SessionSummary| s.attention.as_mut().unwrap().kind = AttentionKind::Input,
        |s: &mut SessionSummary| s.attention.as_mut().unwrap().message = "另一个问题".to_string(),
    ] {
        let mut changed = base.clone();
        change(&mut changed);
        assert_ne!(fingerprint(&base), fingerprint(&changed));
    }
}

#[test]
fn a_list_that_is_not_live_gives_no_tickets() {
    let mut world = World::new(&["a"]);
    world.list(
        "a",
        SessionList {
            sessions: vec![waiting("s", 100)],
            cached: true,
            saved_at_ms: Some(42),
        },
    );
    let pass = world.project();
    assert!(pass.tickets.is_empty());
    assert!(pass.stations[0].cached);
    assert_eq!(pass.stations[0].saved_at_ms, Some(42));
}

// ---------------------------------------------------------------------------
// 灶台
// ---------------------------------------------------------------------------

#[test]
fn stations_split_running_done_and_recent() {
    let mut world = World::new(&["a", "b"]);
    let succeeded =
        |id: &str, at: i64| session(id, SessionStatus::Idle, SessionPhase::Succeeded, at);
    let mut sessions = vec![
        session(
            "run-old",
            SessionStatus::Running,
            SessionPhase::Thinking,
            10,
        ),
        session(
            "run-new",
            SessionStatus::Running,
            SessionPhase::ExecutingTool,
            20,
        ),
        with_attention(succeeded("done", 30), AttentionKind::Success),
        waiting("needs", 40),
        // 成功但要处理的失败提醒：算单据，不算刚完成。
        with_attention(succeeded("failed-later", 41), AttentionKind::Failure),
        session("idle", SessionStatus::Idle, SessionPhase::Idle, 50),
        session("dead", SessionStatus::Idle, SessionPhase::Dead, 60),
    ];
    for i in 0..7 {
        sessions.push(succeeded(&format!("read-{i}"), 100 + i));
    }
    // 从没动过的两条：时间都是 0，按列表次序。
    sessions.push(succeeded("zero-1", 0));
    sessions.push(succeeded("zero-2", 0));
    world.list("a", live(sessions));

    let pass = world.project();
    assert_eq!(pass.stations.len(), 2, "每台登记的机器一组，没有列表也占位");
    let a = &pass.stations[0];
    assert_eq!(a.runtime_id, "a");
    assert_eq!(ids(&a.running), ["run-new", "run-old"]);
    assert_eq!(ids(&a.done), ["done"]);
    assert_eq!(
        ids(&a.recent),
        ["read-6", "read-5", "read-4", "read-3", "read-2"],
        "最近跑过最多 5 条，新的在前"
    );
    assert!(!a.cached);
    assert_eq!(
        ticket_ids(&pass),
        [key("a", "needs"), key("a", "failed-later")]
    );

    let b = &pass.stations[1];
    assert_eq!(b.runtime_id, "b");
    assert!(b.running.is_empty() && b.done.is_empty() && b.recent.is_empty());
    assert!(b.cached);
    assert_eq!(b.saved_at_ms, None);

    let mut world = World::new(&["a"]);
    world.list(
        "a",
        live(vec![succeeded("zero-1", 0), succeeded("zero-2", 0)]),
    );
    assert_eq!(
        ids(&world.project().stations[0].recent),
        ["zero-1", "zero-2"],
        "同时间保持列表次序"
    );
}

// ---------------------------------------------------------------------------
// PassHub：推送与取详情
// ---------------------------------------------------------------------------

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "等不到：{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct Recorded(Arc<Mutex<Vec<Pass>>>);

impl Recorded {
    fn last(&self) -> Pass {
        self.0
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("推过至少一次")
    }

    fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    fn wait(&self, what: &str, accept: impl Fn(&Pass) -> bool) -> Pass {
        wait_until(what, || self.0.lock().unwrap().last().is_some_and(&accept));
        self.last()
    }
}

fn record(hub: &PassHub) -> Recorded {
    let passes = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&passes);
    hub.subscribe(Box::new(move |pass| {
        sink.lock().unwrap().push(pass.clone());
        true
    }));
    Recorded(passes)
}

/// 重取退避用毫秒级。
const FAST: Timing = Timing {
    retry_base: Duration::from_millis(20),
    retry_max: Duration::from_millis(40),
};

fn new_hub(fetch: Fetch) -> PassHub {
    PassHub::new(fetch, FAST)
}

/// 每次取详情都记一笔；`replies` 决定回什么。
fn fetcher(
    calls: Arc<Mutex<Vec<Key>>>,
    reply: impl Fn(&str, &str) -> Result<PendingActions, SmeltError> + Send + Sync + 'static,
) -> Fetch {
    Arc::new(move |runtime_id, session_id| {
        calls
            .lock()
            .unwrap()
            .push((runtime_id.to_string(), session_id.to_string()));
        reply(runtime_id, session_id)
    })
}

#[test]
fn details_are_fetched_once_per_fingerprint_and_pushed() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let hub = new_hub(fetcher(Arc::clone(&calls), |_, session| {
        Ok(actions(&[session]))
    }));
    hub.set_order(vec!["a".to_string()]);
    let passes = record(&hub);
    assert_eq!(passes.last().stations.len(), 1, "订阅时先推一次当前整份");

    let first = waiting("s", 100);
    hub.update("a", live(vec![first.clone()]));
    let pass = passes.wait("详情到了", |pass| {
        pass.tickets.first().is_some_and(|t| t.details_loaded)
    });
    assert_eq!(
        pass.tickets[0].permission.as_ref().unwrap().tool_call_id,
        "s"
    );
    // 摘要里无关的字段变了：不重取。
    let mut renamed = first.clone();
    renamed.title = "新名字".to_string();
    hub.update("a", live(vec![renamed]));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(calls.lock().unwrap().len(), 1);

    // 在等的东西变了：重取，取回前 details_loaded = false。
    let mut next = first.clone();
    next.updated_at = 200;
    hub.update("a", live(vec![next]));
    passes.wait("重取后又到了", |pass| {
        pass.tickets[0].details_loaded && pass.tickets[0].session.updated_at == 200
    });
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert!(
        passes.0.lock().unwrap().iter().any(|pass| pass
            .tickets
            .first()
            .is_some_and(|t| { t.session.updated_at == 200 && !t.details_loaded })),
        "取回前推过一次没有详情的"
    );
}

/// 在等你的会话列表不会再变：连着却取不到时要自己重取，不能等下一次推列表。
#[test]
fn failed_fetches_are_retried_with_backoff() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let hub = new_hub(fetcher(Arc::clone(&calls), move |_, _| {
        if counter.fetch_add(1, Ordering::SeqCst) < 2 {
            Err(error(SmeltErrorKind::Offline, "断线"))
        } else {
            Ok(actions(&["t"]))
        }
    }));
    hub.set_order(vec!["a".to_string()]);
    hub.set_connected("a", true);
    let passes = record(&hub);
    hub.update("a", live(vec![waiting("s", 100)]));
    passes.wait("不推新列表也重取成功", |pass| {
        pass.tickets.first().is_some_and(|ticket| ticket.details_loaded)
    });
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[test]
fn retrying_stops_once_the_ticket_is_gone() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let hub = new_hub(fetcher(Arc::clone(&calls), |_, _| {
        Err(error(SmeltErrorKind::Offline, "断线"))
    }));
    hub.set_order(vec!["a".to_string()]);
    hub.set_connected("a", true);
    let _passes = record(&hub);
    hub.update("a", live(vec![waiting("s", 100)]));
    wait_until("重取过", || calls.lock().unwrap().len() >= 2);

    hub.update("a", live(Vec::new()));
    // 等在睡的那一轮醒来、看到单据没了退出。
    std::thread::sleep(Duration::from_millis(100));
    let settled = calls.lock().unwrap().len();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(calls.lock().unwrap().len(), settled, "单据没了就不再取");
}

/// 重取的退避长到测试等不到：能取到只能是事件叫醒的。
const SLOW: Timing = Timing {
    retry_base: Duration::from_secs(3600),
    retry_max: Duration::from_secs(3600),
};

/// 第一次失败，之后都成功。
fn fails_once(attempts: &Arc<AtomicUsize>) -> Fetch {
    let counter = Arc::clone(attempts);
    Arc::new(move |_, _| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(error(SmeltErrorKind::Offline, "断线"))
        } else {
            Ok(actions(&["t"]))
        }
    })
}

/// 机器没连上时不重取，等它连上立刻重取。
#[test]
fn an_offline_failure_waits_for_the_runtime_to_connect() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let hub = PassHub::new(fails_once(&attempts), SLOW);
    hub.set_order(vec!["a".to_string()]);
    let passes = record(&hub);
    hub.update("a", live(vec![waiting("s", 100)]));
    wait_until("取过一次", || attempts.load(Ordering::SeqCst) == 1);

    hub.set_connected("a", true);

    passes.wait("连上后重取成功", |pass| {
        pass.tickets.first().is_some_and(|ticket| ticket.details_loaded)
    });
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

/// 连着却取不到时退避；期间重新连上过（换了链路）就不等退避。
#[test]
fn a_reconnect_cuts_the_retry_backoff_short() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let hub = PassHub::new(fails_once(&attempts), SLOW);
    hub.set_order(vec!["a".to_string()]);
    hub.set_connected("a", true);
    let passes = record(&hub);
    hub.update("a", live(vec![waiting("s", 100)]));
    wait_until("取过一次", || attempts.load(Ordering::SeqCst) == 1);

    hub.set_connected("a", false);
    hub.set_connected("a", true);

    passes.wait("重新连上后重取成功", |pass| {
        pass.tickets.first().is_some_and(|ticket| ticket.details_loaded)
    });
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

/// 机器重连（换代）时列表先退回缓存、单据暂时下架；连回来状态没变就不重取。
#[test]
fn details_survive_a_reconnect() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let hub = new_hub(fetcher(Arc::clone(&calls), |_, _| Ok(actions(&["t"]))));
    hub.set_order(vec!["a".to_string()]);
    let passes = record(&hub);
    hub.update("a", live(vec![waiting("s", 100)]));
    passes.wait("取到详情", |pass| {
        pass.tickets.first().is_some_and(|ticket| ticket.details_loaded)
    });

    hub.update(
        "a",
        SessionList {
            sessions: vec![waiting("s", 100)],
            cached: true,
            saved_at_ms: Some(1),
        },
    );
    assert!(passes.last().tickets.is_empty(), "重连中：缓存里的不算单据");
    hub.update("a", live(vec![waiting("s", 100)]));
    assert!(passes.last().tickets[0].details_loaded, "连回来直接带着详情");

    hub.update("a", live(vec![waiting("s", 200)]));
    passes.wait("状态变了才重取", |pass| {
        pass.tickets.first().is_some_and(|ticket| ticket.details_loaded)
    });
    assert_eq!(calls.lock().unwrap().len(), 2);
}

#[test]
fn nothing_is_fetched_without_a_subscriber() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let hub = new_hub(fetcher(Arc::clone(&calls), |_, _| Ok(actions(&[]))));
    hub.set_order(vec!["a".to_string()]);
    hub.update("a", live(vec![waiting("s", 100)]));
    std::thread::sleep(Duration::from_millis(50));
    assert!(calls.lock().unwrap().is_empty());

    let passes = record(&hub);
    passes.wait("有人订阅后取详情", |pass| {
        pass.tickets[0].details_loaded
    });
    assert_eq!(*calls.lock().unwrap(), [key("a", "s")]);
}

#[test]
fn order_changes_and_unknown_runtimes() {
    let hub = new_hub(Arc::new(|_, _| Ok(actions(&[]))));
    let passes = record(&hub);
    hub.update("a", live(vec![waiting("s", 1)]));
    assert!(passes.last().stations.is_empty(), "没登记的机器不算");

    hub.set_order(vec!["a".to_string(), "b".to_string()]);
    hub.update("a", live(vec![waiting("s", 1)]));
    hub.update("b", live(vec![waiting("t", 2)]));
    let order = |pass: &Pass| -> Vec<String> {
        pass.stations.iter().map(|s| s.runtime_id.clone()).collect()
    };
    assert_eq!(order(&passes.last()), ["a", "b"]);

    hub.set_order(vec!["b".to_string(), "a".to_string()]);
    assert_eq!(order(&passes.last()), ["b", "a"]);

    let before = passes.count();
    hub.set_order(vec!["b".to_string()]);
    let pass = passes.last();
    assert_eq!(order(&pass), ["b"]);
    assert_eq!(ticket_ids(&pass), [key("b", "t")], "忘记的机器的单据跟着走");
    assert!(passes.count() > before);

    // 没变化不推（先等取详情的线程都推完）。
    passes.wait("详情都到了", |pass| {
        pass.tickets.iter().all(|ticket| ticket.details_loaded)
    });
    let before = passes.count();
    hub.set_order(vec!["b".to_string()]);
    assert_eq!(passes.count(), before);
}

#[test]
fn a_cancelled_subscriber_is_dropped() {
    let hub = new_hub(Arc::new(|_, _| Ok(actions(&[]))));
    let (tx, rx) = mpsc::channel();
    hub.subscribe(Box::new(move |pass| tx.send(pass.clone()).is_ok()));
    rx.recv_timeout(Duration::from_secs(1)).unwrap();
    drop(rx);
    hub.set_order(vec!["a".to_string()]);
    hub.set_order(vec!["b".to_string()]);
    assert!(hub.shared.lock().listeners.is_empty());
}
