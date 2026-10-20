//! 会话目录单测：投影与 attention 直接喂状态；事件流、缓存、换代、各调用的线格式用
//! `UnixStream::pair()` 上的假运行时。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};

use serde_json::{Value, json};
use smelt_event_protocol::{
    CORE_PROJECTION_VERSION, CORE_SESSION_PROJECTION_VERSION, CORE_SNAPSHOT_AUTOMATIONS,
    CORE_SNAPSHOT_REMOTE_SESSIONS, CORE_SNAPSHOT_SESSIONS, CORE_SNAPSHOT_WORKSPACE_MENU,
    CORE_TOPIC_AUTOMATIONS_CHANGED, CORE_TOPIC_REMOTE_SESSIONS_CHANGED, CORE_TOPIC_SESSION_REMOVED,
    CORE_TOPIC_SESSION_STATE_CHANGED, CORE_TOPIC_WORKSPACE_MENU_CHANGED, CoreProjectionEvent,
    CoreSessionPhase, CorrelationId, EventEnvelope, EventId, EventSource, SessionStateChanged,
    SessionStatesSnapshot, SnapshotKind, SubscribeMessage, Topic,
};
use smelt_model::agent_definition::AgentDefinition;
use smelt_model::automation::{Automation, AutomationFile, AutomationRun};
use smelt_model::daemon_state::{DaemonPhase, DaemonSessionState};
use smelt_model::session_control::RemoteSessionSnapshot;
use smelt_model::workspace_menu::WorkspaceMenuSnapshot;
use smelt_runtime_client::protocol::control::{ControlError, ControlResponse};
use smelt_runtime_client::protocol::write_json_line;

use super::projection::{self, CatalogState};
use super::*;
use crate::api::catalog::{
    AttentionKind, AutomationActionKind, AutomationRunSource, AutomationRunStatus, LaunchTarget,
    ScheduleKind, SessionPhase, SessionStatus,
};

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

fn daemon(id: &str, phase: DaemonPhase) -> DaemonSessionState {
    DaemonSessionState {
        id: id.into(),
        generation: 1,
        phase,
        structured_events: true,
        turn_events: true,
        updated_at: 50,
        ..Default::default()
    }
}

fn menu(value: Value) -> WorkspaceMenuSnapshot {
    serde_json::from_value(value).unwrap()
}

fn menu_session(id: &str, kind: &str, project: Option<(&str, u32)>, order: u32) -> Value {
    json!({
        "id": id,
        "kind": kind,
        "title": format!("{id} 菜单名"),
        "cwd": project.map(|(root, _)| root),
        "project_root": project.map(|(root, _)| root),
        "project_title": project.map(|(root, _)| root.trim_start_matches('/')),
        "project_order": project.map_or(u32::MAX, |(_, order)| order),
        "session_order": order,
    })
}

fn one_project_menu(sessions: Vec<Value>) -> Value {
    json!({
        "version": 2,
        "revision": 3,
        "projects": [{"root": "/repo", "title": "repo", "order": 0}],
        "sessions": sessions,
    })
}

fn remote(value: Value) -> RemoteSessionSnapshot {
    serde_json::from_value(value).unwrap()
}

fn automation_file(value: Value) -> AutomationFile {
    serde_json::from_value(value).unwrap()
}

fn run_json(id: &str, automation_id: &str, session_id: Option<&str>, created_at: i64) -> Value {
    json!({
        "id": id,
        "automation_id": automation_id,
        "context": {
            "automation_name": "每日晨报",
            "action": {"type": "agent", "agent_definition_id": "d1"},
            "cwd": "/auto",
            "prompt": "  总结昨天  ",
            "engine_kind_id": "pi",
        },
        "source": "scheduled",
        "status": "awaiting_approval",
        "created_at": created_at,
        "session_id": session_id,
    })
}

fn snapshot(
    sessions: Vec<DaemonSessionState>,
    menu: Option<WorkspaceMenuSnapshot>,
    remote: Option<RemoteSessionSnapshot>,
    automations: Option<AutomationFile>,
) -> DaemonStateEvent {
    DaemonStateEvent::Snapshot {
        sessions,
        remote_sessions: remote,
        workspace_menu: menu,
        automations,
    }
}

fn find<'a>(sessions: &'a [SessionSummary], id: &str) -> &'a SessionSummary {
    sessions
        .iter()
        .find(|session| session.id == id)
        .unwrap_or_else(|| panic!("列表里没有 {id}"))
}

// ---------------------------------------------------------------------------
// 投影
// ---------------------------------------------------------------------------

#[test]
fn projection_merges_menu_remote_sessions_and_automation_runs() {
    let mut state = CatalogState::default();
    let menu = menu(one_project_menu(vec![
        menu_session("menu-acp", "acp", Some(("/repo", 0)), 1),
        menu_session("menu-term", "terminal", Some(("/repo", 0)), 0),
    ]));
    let remote = remote(json!({
        "revision": 2,
        "sessions": [
            {"kind": "acp", "id": "remote-acp", "cwd": "/work/other", "agent": "codex", "created_at": 1},
            {"kind": "terminal", "id": "remote-term", "cwd": "/repo", "created_at": 1},
            {"kind": "acp", "id": "remote-hidden", "cwd": "/repo", "created_at": 1, "hidden": true},
            {"kind": "acp", "id": "remote-creating", "cwd": "/repo", "created_at": 1, "lifecycle": "creating"},
            {"kind": "acp", "id": "run-session", "cwd": "/auto", "created_at": 1, "hidden": true},
        ],
    }));
    let automations = automation_file(json!({
        "schema_version": 2,
        "revision": 1,
        "runs": [run_json("run-1", "a1", Some("run-session"), 100)],
    }));
    state.apply(
        snapshot(Vec::new(), Some(menu), Some(remote), Some(automations)),
        Instant::now(),
    );

    let sessions = state.summaries();
    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "menu-term",
            "menu-acp",
            "remote-term",
            "remote-acp",
            "run-session"
        ],
        "按项目、菜单次序排；新项目排在已有项目后；自动化运行没有项目，沉到最后"
    );

    let remote_acp = find(&sessions, "remote-acp");
    assert_eq!(remote_acp.title, "codex conversation");
    assert_eq!(remote_acp.agent, "codex");
    assert_eq!(remote_acp.project_root.as_deref(), Some("/work/other"));
    assert_eq!(remote_acp.project_title.as_deref(), Some("other"));
    assert_eq!(remote_acp.project_order, 1);
    assert_eq!(find(&sessions, "remote-term").title, "Terminal");
    assert_eq!(find(&sessions, "remote-term").kind, SessionKind::Terminal);
    assert_eq!(
        find(&sessions, "remote-term").project_root.as_deref(),
        Some("/repo")
    );

    let run = find(&sessions, "run-session");
    assert_eq!(run.title, "每日晨报", "远程标题为空时用自动化名");
    assert_eq!(run.project_root, None);
    assert_eq!(run.project_order, u32::MAX);
    assert_eq!(run.agent, "pi");
    let source = run.automation.as_ref().unwrap();
    assert_eq!(source.automation_name, "每日晨报");
    assert_eq!(source.run_status, AutomationRunStatus::AwaitingApproval);
    assert_eq!(source.run_source, AutomationRunSource::Scheduled);
    assert_eq!(source.prompt.as_deref(), Some("总结昨天"));
    assert_eq!(source.started_at, Some(100), "没有开始时间时用创建时间");

    let projects: Vec<(String, u32)> = state
        .projects()
        .into_iter()
        .map(|project| (project.root, project.order))
        .collect();
    assert_eq!(
        projects,
        [("/repo".to_string(), 0), ("/work/other".to_string(), 1)]
    );
}

#[test]
fn a_runtime_without_a_desktop_menu_still_lists_remote_sessions() {
    let mut state = CatalogState::default();
    let remote = remote(json!({
        "revision": 1,
        "sessions": [{"kind": "terminal", "id": "t1", "cwd": "/srv/app", "title": "构建", "created_at": 1}],
    }));
    state.apply(
        snapshot(Vec::new(), None, Some(remote), None),
        Instant::now(),
    );
    let sessions = state.summaries();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title, "构建");
    assert_eq!(state.projects()[0].root, "/srv/app");
}

#[test]
fn title_prefers_the_custom_name_then_the_session_title_then_the_menu() {
    let mut custom = menu_session("custom", "acp", Some(("/repo", 0)), 0);
    custom["custom_title"] = json!(true);
    custom["title"] = json!("我起的名");
    let menu = menu(one_project_menu(vec![
        custom,
        menu_session("auto", "acp", Some(("/repo", 0)), 1),
        menu_session("untitled", "acp", Some(("/repo", 0)), 2),
    ]));
    let titled = |id: &str| DaemonSessionState {
        title: Some("agent 起的名".into()),
        ..daemon(id, DaemonPhase::Idle)
    };
    let mut state = CatalogState::default();
    state.apply(
        snapshot(
            vec![
                titled("custom"),
                titled("auto"),
                daemon("untitled", DaemonPhase::Idle),
            ],
            Some(menu),
            None,
            None,
        ),
        Instant::now(),
    );
    let sessions = state.summaries();
    assert_eq!(find(&sessions, "custom").title, "我起的名");
    assert_eq!(find(&sessions, "auto").title, "agent 起的名");
    assert_eq!(find(&sessions, "untitled").title, "untitled 菜单名");
}

#[test]
fn agent_falls_back_from_the_menu_to_provider_then_launch_command() {
    let mut with_agent = menu_session("menu", "acp", Some(("/repo", 0)), 0);
    with_agent["agent"] = json!("claude");
    let menu = menu(one_project_menu(vec![
        with_agent,
        menu_session("provider", "terminal", Some(("/repo", 0)), 1),
        menu_session("launch", "terminal", Some(("/repo", 0)), 2),
        menu_session("unknown", "terminal", Some(("/repo", 0)), 3),
        menu_session("idle", "terminal", Some(("/repo", 0)), 4),
    ]));
    let mut state = CatalogState::default();
    state.apply(
        snapshot(
            vec![
                DaemonSessionState {
                    provider: Some("codex".into()),
                    ..daemon("menu", DaemonPhase::Idle)
                },
                DaemonSessionState {
                    provider: Some("copilot".into()),
                    launch: Some("claude".into()),
                    ..daemon("provider", DaemonPhase::Idle)
                },
                DaemonSessionState {
                    launch: Some("claude --dangerously-skip-permissions".into()),
                    ..daemon("launch", DaemonPhase::Idle)
                },
                DaemonSessionState {
                    launch: Some("htop".into()),
                    ..daemon("unknown", DaemonPhase::Idle)
                },
            ],
            Some(menu),
            None,
            None,
        ),
        Instant::now(),
    );
    let sessions = state.summaries();
    assert_eq!(find(&sessions, "menu").agent, "claude");
    assert_eq!(find(&sessions, "provider").agent, "copilot");
    assert_eq!(find(&sessions, "launch").agent, "claude");
    assert_eq!(find(&sessions, "unknown").agent, "other");
    let idle = find(&sessions, "idle");
    assert_eq!(idle.agent, "other");
    assert_eq!(
        (idle.phase, idle.status, idle.updated_at),
        (SessionPhase::Idle, SessionStatus::Idle, 0),
        "没有运行态就是还没动过"
    );
}

#[test]
fn phase_and_status_follow_the_authoritative_daemon_phase() {
    let check = |state: DaemonSessionState| {
        (
            projection::session_phase(&state),
            projection::session_status(&state),
        )
    };
    assert_eq!(
        check(daemon("s", DaemonPhase::AwaitingApproval)),
        (SessionPhase::AwaitingApproval, SessionStatus::NeedsYou)
    );
    assert_eq!(
        check(daemon("s", DaemonPhase::WaitingForUser)),
        (SessionPhase::WaitingForUser, SessionStatus::NeedsYou)
    );
    assert_eq!(
        check(daemon("s", DaemonPhase::Failed)),
        (SessionPhase::Failed, SessionStatus::NeedsYou)
    );
    assert_eq!(
        check(daemon("s", DaemonPhase::ExecutingTool)),
        (SessionPhase::ExecutingTool, SessionStatus::Running)
    );
    assert_eq!(
        check(daemon("s", DaemonPhase::Succeeded)),
        (SessionPhase::Succeeded, SessionStatus::Idle)
    );
    assert_eq!(
        check(daemon("s", DaemonPhase::Dead)),
        (SessionPhase::Dead, SessionStatus::Idle)
    );
    assert_eq!(
        check(DaemonSessionState {
            turn_events: false,
            ..daemon("s", DaemonPhase::Thinking)
        }),
        (SessionPhase::Idle, SessionStatus::Idle),
        "没有回合级来源的活动阶段不可信"
    );
    assert_eq!(
        check(DaemonSessionState {
            runtime: false,
            ..daemon("s", DaemonPhase::AwaitingApproval)
        })
        .1,
        SessionStatus::Idle,
        "运行时已丢的会话不再要你处理"
    );

    let mut state = CatalogState::default();
    state.apply(
        snapshot(
            vec![DaemonSessionState {
                pending_question: Some("git push".into()),
                ..daemon("s", DaemonPhase::AwaitingApproval)
            }],
            Some(menu(one_project_menu(vec![menu_session(
                "s",
                "acp",
                Some(("/repo", 0)),
                0,
            )]))),
            None,
            None,
        ),
        Instant::now(),
    );
    let summary = &state.summaries()[0];
    assert_eq!(summary.detail.as_deref(), Some("⚠ git push"));
    assert_eq!(summary.updated_at, 50);
}

#[test]
fn first_snapshot_is_a_baseline_and_later_changes_are_edges() {
    let menu = menu(one_project_menu(
        ["approval", "failed", "busy", "later"]
            .iter()
            .enumerate()
            .map(|(i, id)| menu_session(id, "acp", Some(("/repo", 0)), i as u32))
            .collect(),
    ));
    let mut state = CatalogState::default();
    let now = Instant::now();

    let applied = state.apply(
        snapshot(
            vec![
                daemon("approval", DaemonPhase::AwaitingApproval),
                daemon("failed", DaemonPhase::Failed),
                daemon("busy", DaemonPhase::Thinking),
            ],
            Some(menu.clone()),
            None,
            None,
        ),
        now,
    );
    assert!(applied.attention.is_empty(), "冷启动前就在等的不算新提醒");
    let sessions = state.summaries();
    let approval = find(&sessions, "approval");
    assert_eq!(approval.attention, None);
    assert_eq!(
        approval.status,
        SessionStatus::NeedsYou,
        "列表上照样显示要你处理"
    );

    let applied = state.apply(
        DaemonStateEvent::Update(daemon("approval", DaemonPhase::Thinking)),
        now,
    );
    assert_eq!(
        applied.resolved,
        ["approval"],
        "基线里的待处理事项过去了也要通知"
    );
    assert!(applied.attention.is_empty());

    let applied = state.apply(
        DaemonStateEvent::Update(daemon("busy", DaemonPhase::AwaitingApproval)),
        now,
    );
    assert_eq!(applied.attention.len(), 1);
    assert_eq!(applied.attention[0].session_id, "busy");
    let busy = state.summaries();
    let busy = find(&busy, "busy");
    assert_eq!(
        busy.attention.as_ref().unwrap().kind,
        AttentionKind::Approval
    );

    assert!(state.mark_read("busy"));
    assert!(!state.mark_read("busy"));
    assert_eq!(find(&state.summaries(), "busy").attention, None);

    // 订阅重连后的快照按边沿算：期间新进审批的提醒，消失的待处理事项算过去。
    let applied = state.apply(
        snapshot(
            vec![
                daemon("busy", DaemonPhase::AwaitingApproval),
                daemon("later", DaemonPhase::WaitingForUser),
            ],
            Some(menu),
            None,
            None,
        ),
        now,
    );
    let notified: Vec<&str> = applied
        .attention
        .iter()
        .map(|item| item.session_id.as_str())
        .collect();
    assert_eq!(notified, ["later"], "没变的 busy 不重复提醒");
    assert!(applied.resolved.contains(&"failed".to_string()));
    assert!(!applied.resolved.contains(&"busy".to_string()));
}

#[test]
fn removing_a_session_drops_it_from_the_menu_and_remote_catalog() {
    let mut state = CatalogState::default();
    state.apply(
        snapshot(
            vec![daemon("gone", DaemonPhase::AwaitingApproval)],
            Some(menu(one_project_menu(vec![menu_session(
                "gone",
                "acp",
                Some(("/repo", 0)),
                0,
            )]))),
            Some(remote(json!({"sessions": [
                {"kind": "terminal", "id": "gone", "cwd": "/repo", "created_at": 1}
            ]}))),
            None,
        ),
        Instant::now(),
    );
    let applied = state.apply(
        DaemonStateEvent::Removed { id: "gone".into() },
        Instant::now(),
    );
    assert_eq!(applied.resolved, ["gone"]);
    assert!(state.summaries().is_empty());
}

fn automation(value: Value) -> Automation {
    serde_json::from_value(value).unwrap()
}

fn sample_automations() -> Vec<Automation> {
    vec![
        automation(json!({
            "id": "a1",
            "name": "晨报",
            "enabled": true,
            "trigger": {
                "schedules": [
                    {"type": "daily", "hour": 9, "minute": 30},
                    {"type": "during", "minutes": 15, "days": 31, "start_hour": 9,
                     "start_minute": 0, "end_hour": 18, "end_minute": 30},
                ],
                "webhooks": [{"endpoint": "<redacted>"}],
                "events": [{"topic": "build.done"}],
            },
            "action": {"type": "agent", "agent_definition_id": "d1"},
        })),
        automation(json!({
            "id": "a2",
            "name": "清理",
            "enabled": false,
            "trigger": {"schedules": [{"type": "every_hours", "hours": 6}]},
            "action": {"type": "shell", "command": "rm", "args": ["-rf", "tmp"]},
        })),
        automation(json!({
            "id": "a3",
            "name": "孤儿",
            "enabled": true,
            "trigger": {"schedules": [{"type": "every_minutes", "minutes": 5}]},
            "action": {"type": "agent", "agent_definition_id": "deleted"},
        })),
    ]
}

fn sample_definitions() -> Vec<AgentDefinition> {
    vec![AgentDefinition {
        id: "d1".into(),
        name: "研究员".into(),
        description: "查资料".into(),
        engine_kind_id: "pi".into(),
        prompt: "认真".into(),
        plugins: vec!["skill:web".into()],
        context_folders: vec!["/notes".into()],
        context_links: vec!["https://x".into()],
        ..Default::default()
    }]
}

#[test]
fn automation_catalog_projects_triggers_states_and_the_last_run() {
    let file = automation_file(json!({
        "schema_version": 2,
        "revision": 4,
        "states": [{"automation_id": "a1", "next_run_at": 900, "last_run_id": "r-old"}],
        "runs": [
            run_json("r-old", "a1", Some("s-old"), 10),
            run_json("r-new", "a1", Some("s-new"), 20),
            run_json("r-a2", "a2", None, 30),
        ],
    }));
    let catalog = projection::automation_catalog(
        &sample_automations(),
        Some(&file),
        &sample_definitions(),
        None,
    );
    let ids: Vec<&str> = catalog
        .automations
        .iter()
        .map(|automation| automation.id.as_str())
        .collect();
    assert_eq!(ids, ["a1", "a2", "a3"], "与定义顺序一致");

    let a1 = &catalog.automations[0];
    assert_eq!(a1.action, AutomationActionKind::Agent);
    assert_eq!(a1.agent_name.as_deref(), Some("研究员"));
    assert!(a1.webhook);
    assert_eq!(a1.event_topics, ["build.done"]);
    assert_eq!(a1.next_run_at, Some(900));
    assert_eq!(a1.schedules[0].kind, ScheduleKind::Daily);
    assert_eq!((a1.schedules[0].hour, a1.schedules[0].minute), (9, 30));
    let during = a1.schedules[1];
    assert_eq!(during.kind, ScheduleKind::During);
    assert_eq!(
        (
            during.minutes,
            during.days,
            during.hour,
            during.end_hour,
            during.end_minute
        ),
        (15, 31, 9, 18, 30)
    );
    let last = a1.last_run.as_ref().unwrap();
    assert_eq!(
        last.session_id.as_deref(),
        Some("s-old"),
        "登记的 last_run_id 优先"
    );
    assert_eq!(last.status, AutomationRunStatus::AwaitingApproval);
    assert_eq!(last.source, AutomationRunSource::Scheduled);

    let a2 = &catalog.automations[1];
    assert_eq!(a2.action, AutomationActionKind::Shell);
    assert_eq!(a2.agent_name, None);
    assert!(!a2.webhook);
    assert_eq!(a2.schedules[0].kind, ScheduleKind::EveryHours);
    assert_eq!(a2.schedules[0].hours, 6);
    assert_eq!(a2.next_run_at, None);
    assert_eq!(
        a2.last_run.as_ref().unwrap().started_at,
        Some(30),
        "没有登记就退回最近创建的那次"
    );

    assert_eq!(catalog.automations[2].agent_name, None, "定义删了不编名字");

    let agent = &catalog.agents[0];
    assert_eq!(agent.engine_kind_id, "pi");
    assert_eq!(agent.plugins, ["skill:web"]);
    assert_eq!(agent.context_folders, ["/notes"]);
    assert_eq!(agent.context_links, ["https://x"]);

    // 没有订阅到的投影：照样给目录，只是没有下次和上次。
    let bare =
        projection::automation_catalog(&sample_automations(), None, &sample_definitions(), None);
    assert!(bare.automations.iter().all(|a| a.last_run.is_none()));
    // 刚触发的运行算作上次运行。
    let fresh: AutomationRun = serde_json::from_value(run_json("r-now", "a3", None, 99)).unwrap();
    let with_fresh = projection::automation_catalog(
        &sample_automations(),
        Some(&file),
        &sample_definitions(),
        Some(&fresh),
    );
    assert_eq!(
        with_fresh.automations[2]
            .last_run
            .as_ref()
            .unwrap()
            .started_at,
        Some(99)
    );
}

// ---------------------------------------------------------------------------
// 假运行时与连接状态
// ---------------------------------------------------------------------------

const RUNTIME: &str = "runtime-a";

const FAST: Timing = Timing {
    push_delay: Duration::from_millis(60),
    save_delay: Duration::from_millis(80),
    stable_after: Duration::from_secs(5),
    backoff: fast_backoff,
    sync_wait: Duration::from_secs(3),
    call_timeout: Duration::from_secs(3),
    slow_call_timeout: Duration::from_secs(3),
};

fn fast_backoff(_attempt: u32) -> Duration {
    Duration::from_millis(20)
}

#[derive(Default)]
struct FakeRuntime {
    /// 收到的全部首帧请求。
    requests: Mutex<Vec<Value>>,
    /// event_subscribe 连接的服务端。
    subscriptions: Mutex<Vec<UnixStream>>,
    /// Control 方法 → 结果。没登记的回 method_not_found。
    results: Mutex<HashMap<String, Value>>,
}

impl FakeRuntime {
    fn new() -> Arc<Self> {
        let runtime = Arc::new(Self::default());
        runtime.result("session.agent_bindings", json!({"bindings": {}}));
        runtime
    }

    fn result(&self, method: &str, result: Value) {
        self.results
            .lock()
            .unwrap()
            .insert(method.to_string(), result);
    }

    fn client(self: &Arc<Self>) -> RuntimeClient {
        let runtime = Arc::clone(self);
        RuntimeClient::new(move || {
            let (client, server) = UnixStream::pair()?;
            let runtime = Arc::clone(&runtime);
            std::thread::spawn(move || runtime.serve(server));
            Ok(client)
        })
    }

    fn serve(&self, server: UnixStream) {
        let mut line = String::new();
        if BufReader::new(&server).read_line(&mut line).is_err() || line.is_empty() {
            return;
        }
        let request: Value = serde_json::from_str(&line).unwrap();
        self.requests.lock().unwrap().push(request.clone());
        let response = match request["op"].as_str() {
            Some("event_subscribe") => {
                self.subscriptions.lock().unwrap().push(server);
                return;
            }
            Some("control") => {
                let id = request["request_id"].as_str().unwrap();
                let method = request["method"].as_str().unwrap();
                match self.results.lock().unwrap().get(method) {
                    Some(result) => serde_json::to_value(ControlResponse::ok(id, result.clone())),
                    None => serde_json::to_value(ControlResponse::<()>::error(
                        id,
                        ControlError::new(ControlErrorCode::MethodNotFound, method),
                    )),
                }
                .unwrap()
            }
            Some("history_rename") => json!({"ok": true}),
            other => panic!("假运行时不认识的 op：{other:?}"),
        };
        let _ = write_json_line(&mut &server, &response);
    }

    fn subscription_count(&self) -> usize {
        self.subscriptions.lock().unwrap().len()
    }

    fn wait_subscriptions(&self, count: usize) {
        wait_until(&format!("第 {count} 条订阅"), || {
            self.subscription_count() >= count
        });
    }

    fn send(&self, index: usize, lines: &str) {
        let subscriptions = self.subscriptions.lock().unwrap();
        (&subscriptions[index]).write_all(lines.as_bytes()).unwrap();
    }

    /// 订阅连接被客户端关掉（读到 EOF）。
    fn wait_closed(&self, index: usize) {
        let stream = self.subscriptions.lock().unwrap()[index]
            .try_clone()
            .unwrap();
        // 对端已经关掉时 macOS 上 setsockopt 会报 EINVAL，此时读会直接返回 EOF。
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let mut buf = [0u8; 64];
        assert!(
            matches!((&stream).read(&mut buf), Ok(0)),
            "订阅连接应已被关掉"
        );
    }

    fn controls(&self, method: &str) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request["op"] == "control" && request["method"] == method)
            .map(|request| request["params"].clone())
            .collect()
    }
}

struct FakeSource {
    inner: Mutex<FakeInner>,
    cache: Option<Arc<Cache>>,
}

struct FakeInner {
    generation: u64,
    current: Option<CurrentRuntime>,
    event: ConnectionEvent,
    listeners: Vec<Listener>,
}

impl FakeSource {
    fn new(cache: Option<Arc<Cache>>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(FakeInner {
                generation: 0,
                current: None,
                event: ConnectionEvent {
                    state: ConnectionState::Disconnected,
                    endpoint_id: None,
                    link: None,
                    error: None,
                },
                listeners: Vec::new(),
            }),
            cache,
        })
    }

    /// 和连接状态机一样在自己的锁里通知监听者。
    fn set(
        &self,
        generation: u64,
        state: ConnectionState,
        endpoint: Option<&str>,
        client: Option<RuntimeClient>,
    ) {
        let mut inner = self.inner.lock().unwrap();
        inner.generation = generation;
        inner.current = match (state, client, endpoint) {
            (
                ConnectionState::Connected | ConnectionState::Reconnecting,
                Some(client),
                Some(ep),
            ) => Some(CurrentRuntime {
                generation,
                endpoint_id: ep.to_string(),
                client,
            }),
            _ => None,
        };
        inner.event = ConnectionEvent {
            state,
            endpoint_id: endpoint.map(str::to_string),
            link: None,
            error: None,
        };
        let event = inner.event.clone();
        inner
            .listeners
            .retain(|listener| listener(generation, &event));
    }

    fn connect(&self, generation: u64, endpoint: &str, runtime: &Arc<FakeRuntime>) {
        self.set(
            generation,
            ConnectionState::Connecting,
            Some(endpoint),
            None,
        );
        self.set(
            generation,
            ConnectionState::Connected,
            Some(endpoint),
            Some(runtime.client()),
        );
    }

    fn set_state(&self, state: ConnectionState) {
        let (generation, endpoint, client) = {
            let inner = self.inner.lock().unwrap();
            (
                inner.generation,
                inner.event.endpoint_id.clone(),
                inner.current.as_ref().map(|current| current.client.clone()),
            )
        };
        self.set(generation, state, endpoint.as_deref(), client);
    }
}

impl RuntimeSource for FakeSource {
    fn current(&self) -> Option<CurrentRuntime> {
        self.inner.lock().unwrap().current.clone()
    }

    fn subscribe(&self, listener: Listener) {
        let mut inner = self.inner.lock().unwrap();
        let (generation, event) = (inner.generation, inner.event.clone());
        if listener(generation, &event) {
            inner.listeners.push(listener);
        }
    }

    fn cache(&self) -> Option<Arc<Cache>> {
        self.cache.clone()
    }
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "等不到：{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

type Events = Arc<Mutex<Vec<CatalogEvent>>>;

fn record(catalog: &Catalog) -> Events {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    catalog.subscribe(Box::new(move |event| {
        sink.lock().unwrap().push(event.clone());
        true
    }));
    events
}

fn lists(events: &Events) -> Vec<SessionList> {
    events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| event.sessions.clone())
        .collect()
}

fn last_list(events: &Events) -> SessionList {
    lists(events).pop().expect("至少推过一次列表")
}

fn wait_list(events: &Events, what: &str, condition: impl Fn(&SessionList) -> bool) -> SessionList {
    wait_until(what, || lists(events).last().is_some_and(&condition));
    last_list(events)
}

fn ids(list: &SessionList) -> Vec<&str> {
    list.sessions.iter().map(|s| s.id.as_str()).collect()
}

fn of_kind(events: &Events, kind: CatalogEventKind) -> Vec<CatalogEvent> {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.kind == kind)
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// 线格式
// ---------------------------------------------------------------------------

fn dto(id: &str, revision: u64, phase: CoreSessionPhase) -> SessionStateChanged {
    SessionStateChanged {
        session_id: id.to_string(),
        generation: 1,
        revision,
        cwd: Some("/repo".to_string()),
        launch: None,
        provider: None,
        conversation_id: None,
        title: Some(format!("{id} r{revision}")),
        phase,
        phase_since_unix_seconds: 1,
        pending_question: None,
        tokens_used: None,
        branch: None,
        dirty_files: Vec::new(),
        updated_at_unix_seconds: revision,
        structured_events: true,
        turn_events: true,
        agent_event_version: Some(1),
        runtime: true,
    }
}

fn line(message: SubscribeMessage<Value>) -> String {
    let mut line = serde_json::to_string(&message).unwrap();
    line.push('\n');
    line
}

fn projection_snapshot(kind: &str, data: Value) -> String {
    let revision = data["revision"].as_u64().unwrap_or(0);
    line(SubscribeMessage::Snapshot {
        kind: SnapshotKind(kind.to_string()),
        watermark: 1,
        revision,
        payload: serde_json::to_value(CoreProjectionEvent {
            projection_version: CORE_PROJECTION_VERSION,
            revision,
            data,
        })
        .unwrap(),
    })
}

/// 完整的一批快照：会话、远程目录、菜单、自动化。
fn snapshot_lines(sessions: Vec<SessionStateChanged>, menu: Value) -> String {
    let sessions = line(SubscribeMessage::Snapshot {
        kind: SnapshotKind(CORE_SNAPSHOT_SESSIONS.to_string()),
        watermark: 1,
        revision: 1,
        payload: serde_json::to_value(SessionStatesSnapshot {
            projection_version: CORE_SESSION_PROJECTION_VERSION,
            revision: 1,
            sessions,
        })
        .unwrap(),
    });
    [
        sessions,
        projection_snapshot(CORE_SNAPSHOT_REMOTE_SESSIONS, Value::Null),
        projection_snapshot(CORE_SNAPSHOT_WORKSPACE_MENU, menu),
        projection_snapshot(CORE_SNAPSHOT_AUTOMATIONS, Value::Null),
    ]
    .concat()
}

fn event_line(topic: &str, payload: Value) -> String {
    line(SubscribeMessage::Event {
        sequence: Some(1),
        envelope: EventEnvelope {
            event_id: EventId::new("event-1").unwrap(),
            topic: Topic::new(topic).unwrap(),
            schema_version: 1,
            occurred_at_ms: 1,
            aggregate: None,
            aggregate_revision: None,
            source: EventSource::Core,
            correlation_id: CorrelationId::new("correlation-1").unwrap(),
            causation_id: None,
            hop_count: 0,
            payload,
        },
    })
}

fn update_line(state: SessionStateChanged) -> String {
    event_line(
        CORE_TOPIC_SESSION_STATE_CHANGED,
        serde_json::to_value(state).unwrap(),
    )
}

fn menu_line(menu: Value) -> String {
    let revision = menu["revision"].as_u64().unwrap();
    event_line(
        CORE_TOPIC_WORKSPACE_MENU_CHANGED,
        serde_json::to_value(CoreProjectionEvent {
            projection_version: CORE_PROJECTION_VERSION,
            revision,
            data: menu,
        })
        .unwrap(),
    )
}

fn repo_menu(ids: &[&str]) -> Value {
    one_project_menu(
        ids.iter()
            .enumerate()
            .map(|(i, id)| menu_session(id, "acp", Some(("/repo", 0)), i as u32))
            .collect(),
    )
}

/// 连上一个假运行时并收完首个快照。
fn live(
    sessions: Vec<SessionStateChanged>,
    menu: Value,
) -> (Arc<FakeSource>, Arc<FakeRuntime>, Catalog, Events) {
    let source = FakeSource::new(Some(Arc::new(Cache::in_memory())));
    let runtime = FakeRuntime::new();
    let catalog = Catalog::new(RUNTIME.to_string(), source.clone(), FAST);
    let events = record(&catalog);
    source.connect(1, "ep-a", &runtime);
    runtime.wait_subscriptions(1);
    runtime.send(0, &snapshot_lines(sessions, menu));
    wait_list(&events, "实时列表", |list| !list.cached);
    (source, runtime, catalog, events)
}

// ---------------------------------------------------------------------------
// 事件流
// ---------------------------------------------------------------------------

#[test]
fn subscribes_with_the_catalog_shape_and_pushes_the_live_list() {
    let source = FakeSource::new(None);
    let runtime = FakeRuntime::new();
    runtime.result("session.agent_bindings", json!({"bindings": {"s1": "d1"}}));
    let catalog = Catalog::new(RUNTIME.to_string(), source.clone(), FAST);
    let events = record(&catalog);
    assert_eq!(
        last_list(&events),
        SessionList {
            sessions: Vec::new(),
            cached: true,
            saved_at_ms: None
        },
        "订阅时先推当前已知的列表"
    );

    source.connect(1, "ep-a", &runtime);
    runtime.wait_subscriptions(1);
    let request = runtime.requests.lock().unwrap()[0].clone();
    assert_eq!(request["op"], "event_subscribe");
    let topics: Vec<&str> = request["request"]["subscription"]["topics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|topic| topic.as_str().unwrap())
        .collect();
    assert_eq!(
        topics,
        [
            CORE_TOPIC_SESSION_STATE_CHANGED,
            CORE_TOPIC_SESSION_REMOVED,
            CORE_TOPIC_REMOTE_SESSIONS_CHANGED,
            CORE_TOPIC_WORKSPACE_MENU_CHANGED,
            CORE_TOPIC_AUTOMATIONS_CHANGED,
        ],
        "不订唤醒请求和外观请求"
    );

    runtime.send(
        0,
        &snapshot_lines(
            vec![dto("s1", 1, CoreSessionPhase::Thinking)],
            repo_menu(&["s1"]),
        ),
    );
    let list = wait_list(&events, "带归属的实时列表", |list| {
        !list.cached
            && list
                .sessions
                .first()
                .and_then(|s| s.agent_definition_id.as_deref())
                == Some("d1")
    });
    assert_eq!(list.saved_at_ms, None);
    assert_eq!(list.sessions[0].status, SessionStatus::Running);
    assert_eq!(runtime.controls("session.agent_bindings").len(), 1);

    // 菜单变了要重取归属；内容没变就不再推列表。
    let pushed = lists(&events).len();
    let mut changed = repo_menu(&["s1"]);
    changed["revision"] = json!(4);
    runtime.send(0, &menu_line(changed));
    wait_until("菜单变化后重取归属", || {
        runtime.controls("session.agent_bindings").len() == 2
    });
    std::thread::sleep(FAST.push_delay * 3);
    assert_eq!(lists(&events).len(), pushed, "列表内容没变不推");
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .all(|event| event.runtime_id == RUNTIME),
        "事件都带着这台机器的 id"
    );
}

#[test]
fn close_stops_the_subscription_and_every_listener() {
    let (source, runtime, catalog, events) = live(
        vec![dto("s1", 1, CoreSessionPhase::Thinking)],
        repo_menu(&["s1"]),
    );
    catalog.close();
    runtime.wait_closed(0);
    let pushed = events.lock().unwrap().len();
    source.set_state(ConnectionState::Reconnecting);
    source.set_state(ConnectionState::Connected);
    std::thread::sleep(FAST.push_delay * 3);
    assert_eq!(runtime.subscription_count(), 1, "关了就不再订阅");
    assert_eq!(events.lock().unwrap().len(), pushed, "关了就不再推");
    let late = record(&catalog);
    assert!(
        late.lock().unwrap().is_empty(),
        "关了之后的订阅者什么也收不到"
    );
}

#[test]
fn rapid_changes_are_merged_into_one_push() {
    let (_source, runtime, _catalog, events) = live(
        vec![dto("s1", 1, CoreSessionPhase::Thinking)],
        repo_menu(&["s1"]),
    );
    let before = lists(&events).len();
    let burst: String = (2..=6)
        .map(|revision| update_line(dto("s1", revision, CoreSessionPhase::Thinking)))
        .collect();
    runtime.send(0, &burst);
    let list = wait_list(&events, "合并后的列表", |list| {
        list.sessions[0].title == "s1 r6"
    });
    std::thread::sleep(FAST.push_delay * 3);
    assert_eq!(lists(&events).len(), before + 1, "五次变化只推一次");
    assert_eq!(list.sessions[0].updated_at, 6);
}

#[test]
fn attention_arrives_after_the_list_and_mark_read_stays_local() {
    let (_source, runtime, catalog, events) = live(
        vec![dto("s1", 1, CoreSessionPhase::Thinking)],
        repo_menu(&["s1"]),
    );
    runtime.send(
        0,
        &update_line(dto("s1", 2, CoreSessionPhase::AwaitingApproval)),
    );
    wait_until("提醒", || {
        !of_kind(&events, CatalogEventKind::Attention).is_empty()
    });
    {
        let events = events.lock().unwrap();
        let at = events
            .iter()
            .position(|event| event.kind == CatalogEventKind::Attention)
            .unwrap();
        let item = events[at].attention.as_ref().unwrap();
        assert_eq!(
            (item.session_id.as_str(), item.kind),
            ("s1", AttentionKind::Approval)
        );
        let list_before = events[..at]
            .iter()
            .rev()
            .find_map(|event| event.sessions.as_ref())
            .unwrap();
        assert!(
            list_before.sessions[0].attention.is_some(),
            "提醒到达前列表已经是新的"
        );
    }

    let requests = runtime.requests.lock().unwrap().len();
    catalog.mark_read("s1");
    wait_list(&events, "已读", |list| {
        list.sessions[0].attention.is_none()
    });
    assert_eq!(
        runtime.requests.lock().unwrap().len(),
        requests,
        "已读只在本机"
    );

    runtime.send(0, &update_line(dto("s1", 3, CoreSessionPhase::Thinking)));
    wait_until("审批已过", || {
        of_kind(&events, CatalogEventKind::AttentionResolved)
            .iter()
            .any(|event| event.session_id.as_deref() == Some("s1"))
    });
}

#[test]
fn cached_list_shows_until_the_live_snapshot_then_the_live_list_is_saved() {
    let cache = Arc::new(Cache::in_memory());
    let old = SessionSummary {
        id: "old".into(),
        kind: SessionKind::Acp,
        title: "上次看到的".into(),
        phase: SessionPhase::Idle,
        status: SessionStatus::Idle,
        agent: "claude".into(),
        cwd: None,
        project_root: None,
        project_title: None,
        project_order: 0,
        updated_at: 0,
        detail: None,
        attention: None,
        automation: None,
        agent_definition_id: None,
    };
    cache.save_session_list("ep-a", &vec![old.clone()]);
    let source = FakeSource::new(Some(Arc::clone(&cache)));
    let runtime = FakeRuntime::new();
    let catalog = Catalog::new(RUNTIME.to_string(), source.clone(), FAST);
    let events = record(&catalog);

    source.connect(1, "ep-a", &runtime);
    let cached = wait_list(&events, "缓存列表", |list| list.saved_at_ms.is_some());
    assert!(cached.cached);
    assert_eq!(cached.sessions, vec![old.clone()]);

    runtime.wait_subscriptions(1);
    runtime.send(
        0,
        &snapshot_lines(
            vec![dto("s1", 1, CoreSessionPhase::AwaitingApproval)],
            repo_menu(&["s1"]),
        ),
    );
    let live = wait_list(&events, "实时列表", |list| !list.cached);
    assert_eq!(ids(&live), ["s1"]);
    assert_eq!(live.saved_at_ms, None);
    wait_until("存缓存", || {
        cache
            .load_session_list::<Vec<SessionSummary>>("ep-a")
            .is_some_and(|saved| saved.value == live.sessions)
    });

    // 别的运行时没有缓存：先推空占位。
    source.connect(2, "ep-b", &runtime);
    let empty = wait_list(&events, "换到没缓存的运行时", |list| {
        list.sessions.is_empty()
    });
    assert!(empty.cached);
    assert_eq!(empty.saved_at_ms, None);
}

#[test]
fn switching_runtime_drops_the_old_state_and_subscription() {
    let (source, runtime_a, catalog, events) = live(
        vec![dto("a1", 1, CoreSessionPhase::AwaitingApproval)],
        repo_menu(&["a1"]),
    );
    let runtime_b = FakeRuntime::new();
    source.connect(2, "ep-b", &runtime_b);
    runtime_a.wait_closed(0);
    let placeholder = wait_list(&events, "旧运行时的列表撤掉", |list| list.cached);
    assert!(placeholder.sessions.is_empty());

    runtime_b.wait_subscriptions(1);
    runtime_b.send(
        0,
        &snapshot_lines(
            vec![dto("b1", 1, CoreSessionPhase::AwaitingApproval)],
            repo_menu(&["b1"]),
        ),
    );
    let list = wait_list(&events, "新运行时的列表", |list| !list.cached);
    assert_eq!(ids(&list), ["b1"]);
    assert!(
        of_kind(&events, CatalogEventKind::Attention).is_empty(),
        "新运行时的首个快照同样是基线"
    );
    assert_eq!(runtime_a.subscription_count(), 1, "旧运行时不再重连");
    drop(catalog);
}

#[test]
fn reconnecting_pauses_the_subscription_and_resumes_on_a_fresh_one() {
    let (source, runtime, _catalog, events) = live(
        vec![dto("s1", 1, CoreSessionPhase::Thinking)],
        repo_menu(&["s1"]),
    );
    source.set_state(ConnectionState::Reconnecting);
    runtime.wait_closed(0);
    std::thread::sleep(FAST.push_delay * 2);
    assert!(!last_list(&events).cached, "探活失败期间保留最后的列表");
    assert_eq!(runtime.subscription_count(), 1, "探活失败期间不重连订阅");

    source.set_state(ConnectionState::Connected);
    runtime.wait_subscriptions(2);
    // 同一代的再次快照按边沿算：断线期间进入审批要提醒。
    runtime.send(
        1,
        &snapshot_lines(
            vec![dto("s1", 1, CoreSessionPhase::AwaitingApproval)],
            repo_menu(&["s1"]),
        ),
    );
    wait_until("断线期间的提醒", || {
        !of_kind(&events, CatalogEventKind::Attention).is_empty()
    });
}

#[test]
fn subscription_reconnects_when_the_runtime_closes_it() {
    let (_source, runtime, _catalog, _events) = live(Vec::new(), repo_menu(&[]));
    runtime.subscriptions.lock().unwrap()[0]
        .shutdown(Shutdown::Both)
        .unwrap();
    runtime.wait_subscriptions(2);
}

#[test]
fn protocol_errors_are_reported_once_and_the_subscription_retries() {
    let (_source, runtime, _catalog, events) = live(Vec::new(), repo_menu(&[]));
    runtime.send(0, "这不是 JSON\n");
    runtime.wait_subscriptions(2);
    runtime.send(1, "这不是 JSON\n");
    runtime.wait_subscriptions(3);
    let errors = of_kind(&events, CatalogEventKind::Error);
    assert_eq!(errors.len(), 1, "同样的错误不重复推");
    assert!(errors[0].message.as_deref().unwrap().contains("protocol"));
}

// ---------------------------------------------------------------------------
// 调用
// ---------------------------------------------------------------------------

fn new_session_catalog() -> Value {
    json!({
        "agents": [
            {"id": "agent:quant", "kind": "pi", "label": "量化", "profile": false,
             "agentDefinitionId": "quant"},
            {"id": "profile:work", "kind": "claude", "label": "Claude（工作）", "profile": true},
        ],
        "sections": {
            "common": [
                {"key": "terminal:blank", "label": "空白终端", "kind": "terminal",
                 "pinned": true, "target": "blankTerminal"}
            ],
            "terminal": [
                {"key": "terminal:claude:claude", "label": "Claude Code", "kind": "terminal",
                 "pinned": false, "target": "terminal", "command": "claude", "provider": "claude"}
            ],
            "conversation": [
                {"key": "conversation:agent:quant", "label": "量化", "kind": "conversation",
                 "pinned": false, "target": "conversation", "agentOptionId": "agent:quant",
                 "agentKind": "pi"}
            ]
        }
    })
}

#[test]
fn calls_without_a_runtime_are_offline() {
    let catalog = Catalog::new(RUNTIME.to_string(), FakeSource::new(None), FAST);
    assert_eq!(
        catalog.workspace_catalog().unwrap_err().kind,
        SmeltErrorKind::Offline
    );
    assert_eq!(
        catalog.delete_session("x").unwrap_err().kind,
        SmeltErrorKind::Offline
    );
}

#[test]
fn workspace_catalog_combines_projects_with_the_new_session_catalog() {
    let (_source, runtime, catalog, _events) = live(Vec::new(), repo_menu(&[]));
    runtime.result("new_session.catalog", new_session_catalog());

    let workspace = catalog.workspace_catalog().unwrap();

    assert_eq!(workspace.projects[0].root, "/repo");
    assert_eq!(
        workspace.agents[0].agent_definition_id.as_deref(),
        Some("quant")
    );
    assert_eq!(workspace.agents[1].agent_definition_id, None);
    assert_eq!(workspace.agents[1].kind, "claude");
    let common = &workspace.sections.common[0];
    assert_eq!(
        (common.target, common.agent.as_str()),
        (LaunchTarget::BlankTerminal, "")
    );
    let terminal = &workspace.sections.terminal[0];
    assert_eq!(
        (terminal.target, terminal.agent.as_str()),
        (LaunchTarget::Terminal, "claude")
    );
    let conversation = &workspace.sections.conversation[0];
    assert_eq!(
        (
            conversation.key.as_str(),
            conversation.target,
            conversation.agent.as_str()
        ),
        ("conversation:agent:quant", LaunchTarget::Conversation, "pi")
    );
}

#[test]
fn workspace_catalog_waits_for_the_first_snapshot() {
    let source = FakeSource::new(None);
    let runtime = FakeRuntime::new();
    runtime.result("new_session.catalog", new_session_catalog());
    let catalog = Arc::new(Catalog::new(RUNTIME.to_string(), source.clone(), FAST));
    source.connect(1, "ep-a", &runtime);
    let pending = {
        let catalog = Arc::clone(&catalog);
        std::thread::spawn(move || catalog.workspace_catalog())
    };
    runtime.wait_subscriptions(1);
    runtime.send(0, &snapshot_lines(Vec::new(), repo_menu(&[])));
    assert_eq!(pending.join().unwrap().unwrap().projects.len(), 1);
}

#[test]
fn history_uses_the_engine_and_profile_of_the_agent_option() {
    let (_source, runtime, catalog, _events) = live(Vec::new(), repo_menu(&[]));
    runtime.result("new_session.catalog", new_session_catalog());
    runtime.result(
        "history.list",
        json!({
            "sessions": [{
                "path": "/home/me/.claude/x.jsonl",
                "resume_id": "r1",
                "title": "原始标题",
                "custom_title": "新名字",
                "started_at_ms": 1,
                "last_active_at_ms": 2000,
                "message_count": 12,
                "total_tokens": 99,
            }],
            "agent_definition_ids": {},
        }),
    );

    let history = catalog.list_history("/repo", "profile:work").unwrap();

    assert_eq!(
        runtime.controls("history.list"),
        [json!({"agent": "claude", "profile_id": "work", "cwd": "/repo"})]
    );
    assert_eq!(
        history,
        [HistorySessionSummary {
            resume_id: "r1".into(),
            title: "原始标题".into(),
            custom_title: Some("新名字".into()),
            last_active_at_ms: Some(2000),
            message_count: 12,
        }]
    );

    let renamed = catalog
        .rename_history("/repo", "profile:work", " r1 ", Some(" 新名字 "))
        .unwrap();
    assert_eq!(
        renamed,
        HistoryRenameResult {
            title: "新名字".into(),
            custom_title: Some("新名字".into()),
        }
    );
    catalog
        .rename_history("/repo", "profile:work", "r1", Some("  "))
        .unwrap();
    let renames: Vec<Value> = runtime
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["op"] == "history_rename")
        .cloned()
        .collect();
    assert_eq!(
        renames,
        [
            json!({"op": "history_rename", "agent": "claude", "profile_id": "work",
                   "resume_id": "r1", "title": "新名字", "cwd": "/repo"}),
            json!({"op": "history_rename", "agent": "claude", "profile_id": "work",
                   "resume_id": "r1", "title": null, "cwd": "/repo"}),
        ],
        "空标题表示恢复原始标题"
    );
    assert_eq!(
        runtime.controls("new_session.catalog").len(),
        1,
        "选项按这一代缓存，不每次重取"
    );
    assert_eq!(
        catalog.list_history("/repo", "nope").unwrap_err().kind,
        SmeltErrorKind::Other
    );
}

#[test]
fn create_session_sends_only_the_given_fields() {
    let (_source, runtime, catalog, _events) = live(Vec::new(), repo_menu(&[]));
    runtime.result("session.create", json!({"session_id": "acp-mobile-1"}));

    let id = catalog
        .create_session(CreateSession {
            project_root: "/repo".into(),
            agent_option_id: Some("profile:work".into()),
            launch_key: Some(" ".into()),
            resume_id: Some("r1".into()),
            kind: Some(SessionKind::Acp),
            request_id: Some("6f1c2b8e-4d5a-4f3b-9c2d-1e0f9a8b7c6d".into()),
        })
        .unwrap();
    catalog
        .create_session(CreateSession {
            project_root: "/repo".into(),
            agent_option_id: None,
            launch_key: None,
            resume_id: None,
            kind: Some(SessionKind::Terminal),
            request_id: None,
        })
        .unwrap();

    assert_eq!(id, "acp-mobile-1");
    assert_eq!(
        runtime.controls("session.create"),
        [
            json!({"project_root": "/repo", "agent_option_id": "profile:work", "resume_id": "r1",
                   "kind": "acp", "request_id": "6f1c2b8e-4d5a-4f3b-9c2d-1e0f9a8b7c6d"}),
            json!({"project_root": "/repo", "kind": "terminal"}),
        ]
    );
}

#[test]
fn delete_session_removes_it_locally_and_drops_its_snapshot_cache() {
    let (source, runtime, catalog, events) = live(
        vec![dto("x", 1, CoreSessionPhase::Idle)],
        repo_menu(&["x", "y"]),
    );
    runtime.result("session.delete", json!({}));
    let cache = source.cache().unwrap();
    cache.save_snapshot("ep-a", "x", &"快照");

    catalog.delete_session("x").unwrap();

    assert_eq!(
        runtime.controls("session.delete"),
        [json!({"session_id": "x"})]
    );
    assert_eq!(ids(&last_list(&events)), ["y"], "不等下一份菜单就划掉");
    assert!(cache.load_snapshot::<String>("ep-a", "x").is_none());
}

#[test]
fn automation_commands_return_the_refreshed_catalog() {
    let (_source, runtime, catalog, _events) = live(Vec::new(), repo_menu(&[]));
    let automations = serde_json::to_value(sample_automations()).unwrap();
    runtime.result("automation.list", json!({"automations": automations}));
    runtime.result(
        "agent.definition.list",
        json!({"agents": serde_json::to_value(sample_definitions()).unwrap()}),
    );
    runtime.result(
        "automation.set_enabled",
        json!({"automation": automations[1].clone()}),
    );
    runtime.result(
        "automation.run_once",
        json!({"result": {"result": "run", "value": run_json("r-now", "a2", Some("s-now"), 77)}}),
    );

    let listed = catalog.automation_catalog().unwrap();
    assert_eq!(listed.automations.len(), 3);
    assert_eq!(listed.agents[0].name, "研究员");

    catalog.set_automation_enabled("a2", true).unwrap();
    assert_eq!(
        runtime.controls("automation.set_enabled"),
        [json!({"id": "a2", "enabled": true})]
    );

    let ran = catalog.run_automation_once("a2").unwrap();
    assert_eq!(
        runtime.controls("automation.run_once"),
        [json!({"id": "a2"})]
    );
    assert_eq!(
        ran.automations[1]
            .last_run
            .as_ref()
            .unwrap()
            .session_id
            .as_deref(),
        Some("s-now"),
        "刚触发的运行不等投影就出现在目录里"
    );
}
