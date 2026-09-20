//! `input` 端到端：无 phase 门闩——agent 忙也能写（跟 action 最关键的差异）；
//! 这是「远程 = 工作延续」的契约，回归测试必须盯死。

use super::*;

fn make_pipe_session(rows: u16, cols: u16, phase: Phase) -> (Arc<Session>, std::fs::File) {
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe() 失败");
    let read_end = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    let master = unsafe { std::fs::File::from_raw_fd(fds[1]) };
    let child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id() as i32;
    drop(child);
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

fn call_input(sessions: &Sessions, id: &str, data: &str) -> serde_json::Value {
    let (server, client) = UnixStream::pair().unwrap();
    let remote_state = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
    let iroh_state: IrohState = Arc::new(Mutex::new(None));
    let subscribers = new_event_hub();
    let mut client = client;
    writeln!(
        client,
        "{}",
        serde_json::json!({ "op": "input", "id": id, "data": data })
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
fn input_writes_even_when_agent_is_thinking() {
    let (sess, mut read_end) = make_pipe_session(24, 80, Phase::Thinking);
    let sessions = new_sessions();
    sessions.insert_for_test("busy", sess);

    // Ctrl+C（0x03）：json! 直接嵌 char，serde 编进 JSON 字符串
    let ctrl_c = "\u{0003}";
    let resp = call_input(&sessions, "busy", ctrl_c);
    assert_eq!(resp["ok"], true, "resp={resp}");

    let mut buf = [0u8; 8];
    let n = read_end.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"\x03");
}

#[test]
fn input_writes_text_while_idle() {
    let (sess, mut read_end) = make_pipe_session(24, 80, Phase::Idle);
    let sessions = new_sessions();
    sessions.insert_for_test("idle", sess);

    let resp = call_input(&sessions, "idle", "ls -la\r");
    assert_eq!(resp["ok"], true, "resp={resp}");

    let mut buf = [0u8; 64];
    let n = read_end.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"ls -la\r");
}

#[test]
fn a_full_pty_input_queue_does_not_hold_ctl_lock() {
    let (sess, read_end) = make_pipe_session(24, 80, Phase::Thinking);
    let fd = sess.ctl.lock().unwrap().master.as_raw_fd();
    set_fd_nonblocking(fd, true).unwrap();

    // 先把模拟 PTY 的写队列填满，确保后面的调用会进入 poll，而不是瞬间成功。
    let filler = [0u8; 8192];
    loop {
        let result = sess.ctl.lock().unwrap().master.write(&filler);
        match result {
            Ok(n) if n > 0 => {}
            Ok(_) => break,
            Err(error) if error.kind() == ErrorKind::WouldBlock => break,
            Err(error) => panic!("填充测试管道失败: {error}"),
        }
    }

    let sess_for_writer = Arc::clone(&sess);
    let writer = thread::spawn(move || write_session_input(&sess_for_writer, b"x"));
    let entered_deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < entered_deadline && sess.input_gate.try_lock().is_ok() {
        thread::yield_now();
    }
    assert!(
        sess.input_gate.try_lock().is_err(),
        "写入线程应已进入 input_gate"
    );

    let ctl_free_deadline = Instant::now() + Duration::from_secs(1);
    let mut ctl_free = false;
    while Instant::now() < ctl_free_deadline {
        if let Ok(guard) = sess.ctl.try_lock() {
            drop(guard);
            ctl_free = true;
            break;
        }
        thread::yield_now();
    }
    assert!(ctl_free, "PTY 写入等待时不应继续持有 ctl 锁");

    // 关闭读端让 poll 立即收到 HUP，避免测试无谓等待完整超时窗口。
    drop(read_end);
    assert!(writer.join().unwrap().is_err());
}

#[test]
fn empty_input_is_rejected() {
    let (sess, mut read_end) = make_pipe_session(24, 80, Phase::Idle);
    let sessions = new_sessions();
    sessions.insert_for_test("e", sess);

    let resp = call_input(&sessions, "e", "");
    assert_eq!(resp["ok"], false);
    assert!(
        resp["err"].as_str().unwrap().contains("data"),
        "resp={resp}"
    );

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
    assert!(read_end.read(&mut buf).is_err(), "空 input 不该写字节");
}

#[test]
fn input_on_unknown_session_is_rejected() {
    let sessions = new_sessions();
    let resp = call_input(&sessions, "nope", "x");
    assert_eq!(resp["ok"], false);
}
