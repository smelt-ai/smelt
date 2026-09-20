use super::*;

/// `Ctl.master` 用一根真管道（不是 /dev/null）：这样能从另一头读回真正写
/// 进去的字节，验证 action 落地的到底是不是预期的按键序列，不是只看 `ok`。
fn make_pipe_session(rows: u16, cols: u16, phase: Phase) -> (Arc<Session>, std::fs::File) {
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe() 失败");
    let read_end = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    let master = unsafe { std::fs::File::from_raw_fd(fds[1]) };

    let child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id() as i32;
    drop(child); // 退出状态交给下面 Session 的唯一 TerminalChild owner

    let state = Arc::new(Mutex::new(SessionState {
        phase,
        ..Default::default()
    }));
    let subscribers = new_event_hub();
    let color_replies = Arc::new(Mutex::new(VecDeque::new()));
    let listener = StateListener::with_color_replies(
        Arc::clone(&state),
        subscribers,
        Arc::clone(&color_replies),
    );
    let sess = Arc::new(Session {
        instance: next_session_instance(),
        geometry_token: uuid::Uuid::new_v4().simple().to_string(),
        child: TerminalChild::start(pid).unwrap(),
        ctl: Mutex::new(Ctl {
            master,
            jolt: false,
            cols,
            rows,
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
        term: Mutex::new(new_daemon_term(rows, cols, listener)),
        state,
    });
    (sess, read_end)
}

/// 直接走 handle_conn 的真实分发（不是绕过去调内部函数）：action 是一次性
/// 请求-响应，不像 watch/open 那样要开线程陪它跑一辈子。
fn call_action(sessions: &Sessions, id: &str, kind: &str) -> serde_json::Value {
    let (server, client) = UnixStream::pair().unwrap();
    let remote_state = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
    let iroh_state: IrohState = Arc::new(Mutex::new(None));
    let subscribers = new_event_hub();
    let mut client = client;
    writeln!(
        client,
        "{}",
        serde_json::json!({ "op": "action", "id": id, "kind": kind })
    )
    .unwrap();
    handle_conn(
        server,
        ServerContext {
            sessions: Arc::clone(sessions),
            acp_sessions: new_test_acp_sessions(),
            remote_sessions: new_test_remote_sessions(),
            workspace_menu: new_test_workspace_menu(),
            automations: new_test_automation_store(),
            exe_mtime: 0,
            daemon_fingerprint: None,
            listen_fd: -1,
            remote_state,
            iroh_state,
            iroh_connections: new_iroh_connections(),
            event_hub: subscribers,
        },
    );
    let mut resp = String::new();
    BufReader::new(client).read_line(&mut resp).unwrap();
    serde_json::from_str(&resp).unwrap()
}

#[test]
fn approve_writes_bare_enter_when_awaiting_approval() {
    let (sess, mut read_end) = make_pipe_session(24, 80, Phase::AwaitingApproval);
    let sessions = new_sessions();
    sessions.insert_for_test("a", sess);

    let resp = call_action(&sessions, "a", "approve");
    assert_eq!(resp["ok"], true, "resp={resp}");

    let mut buf = [0u8; 8];
    let n = read_end.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"\r");
}

#[test]
fn deny_writes_bare_escape_when_waiting_for_user() {
    let (sess, mut read_end) = make_pipe_session(24, 80, Phase::WaitingForUser);
    let sessions = new_sessions();
    sessions.insert_for_test("b", sess);

    let resp = call_action(&sessions, "b", "deny");
    assert_eq!(resp["ok"], true, "resp={resp}");

    let mut buf = [0u8; 8];
    let n = read_end.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"\x1b");
}

/// 门闩：phase 是 Thinking（agent 正忙）时，action 必须被拒绝，且**真的没有
/// 写入任何字节**——不能只是回错误但底下偷偷写了。
#[test]
fn action_rejected_and_no_bytes_written_when_agent_busy() {
    let (sess, mut read_end) = make_pipe_session(24, 80, Phase::Thinking);
    let sessions = new_sessions();
    sessions.insert_for_test("c", sess);

    let resp = call_action(&sessions, "c", "approve");
    assert_eq!(resp["ok"], false);
    assert!(
        resp["err"].as_str().unwrap().contains("不是在等你"),
        "resp={resp}"
    );

    // 管道写端没收到任何字节：把它设成非阻塞读一下，读不到东西才对。
    use std::os::fd::AsRawFd;
    unsafe {
        let flags = libc::fcntl(read_end.as_raw_fd(), libc::F_GETFL);
        libc::fcntl(
            read_end.as_raw_fd(),
            libc::F_SETFL,
            flags | libc::O_NONBLOCK,
        );
    }
    let mut buf = [0u8; 8];
    let result = read_end.read(&mut buf);
    assert!(result.is_err(), "门闩失效：agent 忙的时候还是写进去了字节");
}

#[test]
fn action_on_unknown_session_is_rejected() {
    let sessions = new_sessions();
    let resp = call_action(&sessions, "does-not-exist", "approve");
    assert_eq!(resp["ok"], false);
}
