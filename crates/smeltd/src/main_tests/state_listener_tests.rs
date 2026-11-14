use super::*;
use smelt_event_bus::Delivery;
use smelt_event_protocol::{
    CORE_TOPIC_SESSION_REMOVED, CORE_TOPIC_SESSION_STATE_CHANGED, PluginId, SessionStateChanged,
    SubscriptionId, Topic,
};
use std::collections::BTreeSet;

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

/// 一个输出接到管道的会话：先把 `script` 喂给守护的终端，返回会话和管道读端（写回 PTY 的
/// 字节从这里读）。`cell` 是桌面报来的格子像素，0 表示还没报。
fn session_for(script: &[u8], cell: (u16, u16)) -> (Session, std::fs::File) {
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe() 失败");
    let probe = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    let master = unsafe { std::fs::File::from_raw_fd(fds[1]) };
    let state = Arc::new(Mutex::new(SessionState::default()));
    let replies = Arc::new(Mutex::new(VecDeque::new()));
    let bell = Arc::new(AtomicBool::new(false));
    let listener = StateListener::new(Arc::clone(&replies), Arc::clone(&bell));
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
            cell_w: cell.0,
            cell_h: cell.1,
            remote_viewports: 0,
            remote_grace: 0,
            waiting_for_cell: Vec::new(),
        }),
        input_gate: Mutex::new(()),
        out: Mutex::new(Out {
            clients: Vec::new(),
            watchers: Vec::new(),
        }),
        output_gate: Mutex::new(()),
        replies,
        bell,
        term: Mutex::new(term),
        graphics: Mutex::default(),
        state,
    };
    (session, probe)
}

/// 关掉会话（写端），读出写回 PTY 的全部字节。没有应答时读到 EOF 而不是一直等。
fn written(session: Session, mut probe: std::fs::File) -> String {
    drop(session);
    let mut bytes = Vec::new();
    probe.read_to_end(&mut bytes).expect("读应答");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// 喂完 `script` 按泵的顺序写回查询的应答，返回写回 PTY 的全部字节。
fn replies_for(script: &[u8], cell: (u16, u16)) -> String {
    let (session, probe) = session_for(script, cell);
    flush_terminal_replies(&session);
    written(session, probe)
}

fn color_replies_for(script: &[u8]) -> String {
    replies_for(script, (0, 0))
}

/// 设备属性、光标位置、行列数由守护的网格答，不等桌面：桌面开几个都只答一次，没有桌面
/// 连着也有人答。按程序问的先后答。
#[test]
fn device_queries_are_answered_by_the_daemon_grid() {
    let reply = replies_for(b"ab\x1b[6n\x1b[c\x1b[18t", (10, 20));
    assert_eq!(reply, "\x1b[1;3R\x1b[?6c\x1b[8;24;80t");
}

/// 文字区像素（CSI 14 t）按桌面报来的格子尺寸算；还没报过就先不答，别的查询照答。
#[test]
fn text_area_pixels_wait_for_a_reported_cell_size() {
    assert_eq!(replies_for(b"\x1b[14t", (10, 20)), "\x1b[4;480;800t");
    assert_eq!(replies_for(b"\x1b[14t\x1b[c", (0, 0)), "\x1b[?6c");
}

/// 会话按 24×80、没有像素先起，Pi 一启动就问格子像素，赶在桌面排版之前。记下来，桌面报来
/// 格子尺寸时按问的先后补答，只答一次。
#[test]
fn pixel_queries_are_answered_once_the_desktop_reports_a_cell_size() {
    let (session, probe) = session_for(b"\x1b[14t", (0, 0));
    flush_terminal_replies(&session);
    let early = session.ctl.lock().unwrap().answer_or_wait(PixelQuery::Cell);
    assert!(early.is_none(), "格子尺寸还不知道，不能答");

    resize_session(&session, 80, 24, 10, 20);
    resize_session(&session, 80, 24, 10, 20);
    assert_eq!(
        written(session, probe),
        "\x1b[4;480;800t\x1b[6;20;10t"
    );
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
}

/// 监听器只记响铃，留给泵交给提醒。标题的 BEL 结束符不算响铃。
#[test]
fn bell_is_recorded_for_the_pump() {
    let replies = Arc::new(Mutex::new(VecDeque::new()));
    let bell = Arc::new(AtomicBool::new(false));
    let mut term = new_daemon_term(24, 80, StateListener::new(replies, Arc::clone(&bell)));
    let mut parser: Processor = Processor::new();
    parser.advance(&mut term, b"\x1b]0;title\x07");
    assert!(!bell.load(Ordering::Acquire));
    parser.advance(&mut term, b"done\x07");
    assert!(bell.load(Ordering::Acquire));
}

/// 终端通知和响铃只在没有 hook 的会话上进提醒：有 hook 的智能体自己也会响铃、发 OSC。
#[test]
fn terminal_notices_become_attention_only_without_hooks() {
    let event_hub = new_event_hub();
    for (id, structured) in [("plain", false), ("hooked", true)] {
        let state = SessionState {
            id: id.into(),
            instance: 1,
            revision: 1,
            structured_events: structured,
            ..Default::default()
        };
        event_hub.publish_session(&state).unwrap();
        publish_terminal_notifications(
            &Mutex::new(state),
            id,
            &event_hub,
            vec!["构建完成".into()],
            true,
        );
    }
    let snapshot = event_hub
        .runtime()
        .snapshot(&smelt_event_protocol::SnapshotKind(
            smelt_event_protocol::CORE_SNAPSHOT_ATTENTION.into(),
        ))
        .unwrap();
    let snapshot: smelt_event_protocol::AttentionSnapshot =
        serde_json::from_value(snapshot.payload).unwrap();
    assert_eq!(snapshot.sessions.len(), 1);
    assert_eq!(snapshot.sessions[0].session_id, "plain");
    assert_eq!(
        snapshot.sessions[0].unread.as_ref().map(|item| item.kind),
        Some(smelt_event_protocol::AttentionKind::Bell),
        "同一批里的通知先记，响铃后记"
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
