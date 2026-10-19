use super::*;
use smelt_event_bus::Delivery;
use smelt_event_protocol::{
    CORE_TOPIC_SESSION_REMOVED, CORE_TOPIC_SESSION_STATE_CHANGED, PluginId, SessionStateChanged,
    SubscriptionId, Topic,
};
use std::collections::BTreeSet;

fn no_subscribers() -> EventHubHandle {
    new_event_hub()
}

fn session_events(event_hub: &EventHubHandle) -> smelt_event_bus::Subscription {
    event_hub
        .subscribe(
            PluginId::new("test.state-listener").unwrap(),
            SubscriptionId::new("sessions").unwrap(),
            [CORE_TOPIC_SESSION_STATE_CHANGED, CORE_TOPIC_SESSION_REMOVED]
                .into_iter()
                .map(|topic| Topic::new(topic).unwrap())
                .collect::<BTreeSet<_>>(),
            &core_read_capabilities(),
        )
        .unwrap()
}

fn next_session_event(
    subscription: &smelt_event_bus::Subscription,
) -> Arc<smelt_event_bus::StoredEvent> {
    loop {
        match subscription.recv().unwrap() {
            Delivery::Event(event) => return event,
            Delivery::Snapshot(_) => {}
            Delivery::Lag { dropped, .. } => panic!("unexpected lag: {dropped}"),
        }
    }
}

/// 一个输出接到管道的会话：先把 `script` 喂给守护的终端，再按泵的顺序写回颜色查询，
/// 返回写回 PTY 的全部字节。
fn color_replies_for(script: &[u8]) -> String {
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe() 失败");
    let mut probe = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    let master = unsafe { std::fs::File::from_raw_fd(fds[1]) };
    let state = Arc::new(Mutex::new(SessionState::default()));
    let color_replies = Arc::new(Mutex::new(VecDeque::new()));
    let listener = StateListener::with_color_replies(
        Arc::clone(&state),
        no_subscribers(),
        Arc::clone(&color_replies),
    );
    let mut term = new_daemon_term(24, 80, listener);
    let mut parser: Processor = Processor::new();

    parser.advance(&mut term, script);

    let session = Session {
        instance: next_session_instance(),
        geometry_token: "test".into(),
        child: TerminalChild::start(-1).unwrap(),
        ctl: Mutex::new(Ctl {
            master,
            jolt: false,
            cols: 80,
            rows: 24,
            cell_w: 0,
            cell_h: 0,
            remote_viewports: 0,
            remote_grace: 0,
            cwd: None,
        }),
        input_gate: Mutex::new(()),
        out: Mutex::new(Out {
            clients: Vec::new(),
            watchers: Vec::new(),
        }),
        output_gate: Mutex::new(()),
        color_replies,
        term: Mutex::new(term),
        state,
    };

    flush_terminal_color_replies(&session);

    let mut bytes = [0u8; 128];
    let n = probe.read(&mut bytes).expect("应收到颜色查询的回应");
    String::from_utf8_lossy(&bytes[..n]).into_owned()
}

/// Grok 会在启动时查询 `OSC 11`。这个用例刻意不创建任何 desktop client：守护
/// 先解析该查询、再由泵在网格锁释放后写回 PTY，覆盖首个 attachment 尚未挂上的竞态。
#[test]
fn osc11_is_answered_before_any_client_attaches() {
    let reply = color_replies_for(b"\x1b]11;?\x07");
    assert!(
        reply.starts_with("\x1b]11;rgb:"),
        "应为 OSC 11 RGB 回应，实际: {reply:?}"
    );
}

/// 程序自己用 OSC 11 换了底色再查，要回它换的那个色，不是主题色。
#[test]
fn color_query_reports_the_color_the_program_set() {
    let reply = color_replies_for(b"\x1b]11;rgb:01/02/03\x07\x1b]11;?\x07");
    assert!(
        reply.starts_with("\x1b]11;rgb:0101/0202/0303"),
        "应回程序改过的底色，实际: {reply:?}"
    );
}

#[test]
fn color_query_uses_terminal_theme_snapshot() {
    let theme = smelt_core::terminal_theme::TerminalThemeSnapshot {
        background: 0x11_22_33,
        foreground: 0x44_55_66,
        palette: vec![0xaa_bb_cc],
        ..Default::default()
    };
    let palette = resolve_terminal_theme_color(&theme, 0);
    assert_eq!((palette.r, palette.g, palette.b), (0xaa, 0xbb, 0xcc));

    let background = resolve_terminal_theme_color(&theme, NamedColor::Background as usize);
    assert_eq!(
        (background.r, background.g, background.b),
        (0x11, 0x22, 0x33)
    );

    let foreground = resolve_terminal_theme_color(&theme, usize::MAX);
    assert_eq!(
        (foreground.r, foreground.g, foreground.b),
        (0x44, 0x55, 0x66)
    );
}

#[derive(Clone)]
struct TitleCapture(Arc<Mutex<Vec<String>>>);

impl EventListener for TitleCapture {
    fn send_event(&self, event: Event) {
        if let Event::Title(title) = event {
            self.0.lock().unwrap().push(title);
        }
    }
}

#[test]
fn osc_title_parser_preserves_the_complete_visible_payload() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut term = new_daemon_term(24, 80, TitleCapture(Arc::clone(&captured)));
    let mut parser: Processor = Processor::new();

    parser.advance(
        &mut term,
        "\x1b]0;⠼ - Preparing read_file... - grok\x07".as_bytes(),
    );
    parser.advance(&mut term, "\x1b]2;✳ 审查权限门闩\x1b\\".as_bytes());
    assert_eq!(
        captured.lock().unwrap().as_slice(),
        [
            "⠼ - Preparing read_file... - grok".to_string(),
            "✳ 审查权限门闩".to_string(),
        ]
    );

    let state = Arc::new(Mutex::new(SessionState::default()));
    let listener = StateListener::new(Arc::clone(&state), no_subscribers());
    let mut term = new_daemon_term(24, 80, listener);
    let mut parser: Processor = Processor::new();
    parser.advance(
        &mut term,
        "\x1b]0;⠼ - Preparing read_file... - grok\x07".as_bytes(),
    );
    parser.advance(&mut term, "\x1b]2;✳ 审查权限门闩\x1b\\".as_bytes());
    let st = state.lock().unwrap();
    assert_eq!(st.title, None, "OSC 原文不是会话名");
    assert_eq!(st.revision, 0);
    assert_eq!(st.updated_at, 0);
}

/// OSC 标题不进会话状态：不改 phase、活动时间、revision，也不写会话名。
#[test]
fn terminal_titles_never_change_phase_or_phase_since() {
    for phase in [
        Phase::Idle,
        Phase::Thinking,
        Phase::ExecutingTool,
        Phase::AwaitingApproval,
        Phase::WaitingForUser,
        Phase::Succeeded,
        Phase::Failed,
        Phase::Dead,
    ] {
        let state = Arc::new(Mutex::new(SessionState {
            phase,
            phase_since: 1,
            updated_at: 9,
            ..Default::default()
        }));
        let listener = StateListener::new(Arc::clone(&state), no_subscribers());
        listener.send_event(Event::Title(
            "Thinking - TSLA 趋势且入场与 put 判断 - grok".to_string(),
        ));
        listener.send_event(Event::Title("⠋ doing work".to_string()));

        let st = state.lock().unwrap();
        assert_eq!(st.phase, phase, "标题不该改变 {phase:?}");
        assert_eq!(st.phase_since, 1, "标题不该刷新 {phase:?} 的开始时间");
        assert_eq!(st.updated_at, 9, "标题帧不是会话活动");
        assert_eq!(st.title, None, "OSC 不写入会话名");
        assert_eq!(st.revision, 0);
    }
}

#[test]
fn terminal_title_does_not_replace_the_session_name() {
    let state = Arc::new(Mutex::new(SessionState {
        cwd: Some("/Users/me/code/smelt".into()),
        launch: Some("codex --dangerously-bypass-approvals-and-sandbox".into()),
        title: Some("修复 Codex 标题".into()),
        phase: Phase::Thinking,
        phase_since: 1,
        revision: 7,
        updated_at: 9,
        structured_events: true,
        turn_events: true,
        ..Default::default()
    }));
    let listener = StateListener::new(Arc::clone(&state), no_subscribers());
    listener.send_event(Event::Title("⠙ smelt".to_string()));

    let st = state.lock().unwrap();
    assert_eq!(st.title.as_deref(), Some("修复 Codex 标题"));
    assert_eq!(st.revision, 7, "OSC 不发布新会话名");
    assert_eq!(st.updated_at, 9, "OSC 标题帧不应刷新会话活动时间");
}

#[test]
fn terminal_title_does_not_rename_any_provider() {
    for provider in ["grok", "codex", "claude", "copilot", "opencode"] {
        let state = Arc::new(Mutex::new(SessionState {
            provider: Some(provider.into()),
            title: Some("UAT 问题修复".into()),
            revision: 3,
            updated_at: 9,
            ..Default::default()
        }));
        let listener = StateListener::new(Arc::clone(&state), no_subscribers());
        listener.send_event(Event::Title(
            "Action Required - 等你批准 - grok".to_string(),
        ));
        listener.send_event(Event::Title(
            "⠼ - Thinking - CPXM-1479 DVP UAT 前端修复并撤回后端 - grok".to_string(),
        ));

        let st = state.lock().unwrap();
        assert_eq!(st.title.as_deref(), Some("UAT 问题修复"));
        assert_eq!(st.revision, 3, "{provider} 的终端标题不是会话名");
        assert_eq!(st.updated_at, 9);
    }
}

/// Bell 只更新时间戳，不改 phase——单独响铃太不可靠，只能当辅助信号。
#[test]
fn bell_touches_timestamp_without_changing_phase() {
    let state = Arc::new(Mutex::new(SessionState {
        phase: Phase::Idle,
        ..Default::default()
    }));
    let listener = StateListener::new(Arc::clone(&state), no_subscribers());
    listener.send_event(Event::Bell);

    let st = state.lock().unwrap();
    assert_eq!(st.phase, Phase::Idle);
    assert!(st.updated_at > 0);
}

/// 广播：state 变化后，所有订阅者都该收到一行 `{"session": ...}`。
#[test]
fn send_event_broadcasts_to_subscribers() {
    let subscribers = new_event_hub();
    let events = session_events(&subscribers);
    let state = Arc::new(Mutex::new(SessionState {
        id: "t".into(),
        ..Default::default()
    }));
    let listener = StateListener::new(state, subscribers);
    listener.send_event(Event::Bell);

    let event = next_session_event(&events);
    let state: SessionStateChanged =
        serde_json::from_value(event.envelope.payload.clone()).unwrap();
    assert_eq!(state.session_id, "t");
    assert_eq!(state.phase, smelt_event_protocol::CoreSessionPhase::Idle);
    assert_eq!(state.title, None);
    assert!(state.updated_at_unix_seconds > 0);
}

#[test]
fn osc_title_broadcast_is_not_persisted_in_identity_directory() {
    let _guard = session_directory::DIRECTORY_TEST_LOCK.lock().unwrap();
    let temp =
        std::env::temp_dir().join(format!("smelt-title-transient-test-{}", std::process::id()));
    let previous_dir = std::env::var_os("SMELT_GHOST_SESSIONS_DIR");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    unsafe { std::env::set_var("SMELT_GHOST_SESSIONS_DIR", &temp) };
    set_session_directory_for_test(session_directory::SessionDirectory::new());

    let state = Arc::new(Mutex::new(SessionState {
        id: "title-only".into(),
        instance: 1,
        ..Default::default()
    }));
    let listener = StateListener::new(state, new_event_hub());
    listener.send_event(Event::Title("⠙ exact OSC title".to_string()));
    let directory_snapshot =
        with_session_directory(|directory| directory.snapshot()).unwrap_or_default();

    clear_session_directory_for_test();
    match previous_dir {
        Some(path) => unsafe { std::env::set_var("SMELT_GHOST_SESSIONS_DIR", path) },
        None => unsafe { std::env::remove_var("SMELT_GHOST_SESSIONS_DIR") },
    }
    let _ = std::fs::remove_dir_all(&temp);

    assert!(
        directory_snapshot.is_empty(),
        "OSC 标题不进会话状态，也不进崩溃恢复目录"
    );
}

#[test]
fn broadcast_rejects_an_older_revision_after_a_newer_one() {
    let subscribers = new_event_hub();
    let events = session_events(&subscribers);

    let newer = SessionState {
        id: "ordered".into(),
        phase: Phase::Succeeded,
        revision: 20,
        ..Default::default()
    };
    let older = SessionState {
        phase: Phase::Thinking,
        revision: 19,
        ..newer.clone()
    };
    broadcast_state(&subscribers, &newer);
    broadcast_state(&subscribers, &older);

    let event = next_session_event(&events);
    let state: SessionStateChanged =
        serde_json::from_value(event.envelope.payload.clone()).unwrap();
    assert_eq!(
        state.phase,
        smelt_event_protocol::CoreSessionPhase::Succeeded
    );
    assert!(
        matches!(events.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
        "旧 revision 不应再广播"
    );
}

#[test]
fn retired_runtime_cannot_resurrect_or_remove_a_replacement() {
    let subscribers = new_event_hub();
    let retired = SessionState {
        id: "reused-id".into(),
        instance: 41,
        revision: 10,
        phase: Phase::Thinking,
        ..Default::default()
    };
    broadcast_state(&subscribers, &retired);
    subscribers
        .remove_session("reused-id", retired.instance)
        .unwrap();

    // 一个已持有旧 Arc 的 listener 可能在删除后才完成回调。它不能重新把旧
    // snapshot 插入总线，也不能在稍后删除同 ID 的新 runtime。
    let late_old = SessionState {
        revision: 11,
        phase: Phase::Succeeded,
        ..retired.clone()
    };
    broadcast_state(&subscribers, &late_old);
    assert!(
        subscribers.legacy_snapshot().unwrap()["sessions"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let replacement = SessionState {
        instance: 42,
        revision: 1,
        phase: Phase::Idle,
        ..retired.clone()
    };
    broadcast_state(&subscribers, &replacement);
    subscribers
        .remove_session("reused-id", retired.instance)
        .unwrap();

    let snapshot = subscribers.legacy_snapshot().unwrap();
    let sessions = snapshot["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["generation"], replacement.instance);
    assert_eq!(sessions[0]["phase"], "idle");
}
