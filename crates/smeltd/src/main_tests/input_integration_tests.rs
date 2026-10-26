//! `input` 端到端：无 phase 门闩——agent 忙也能写（跟 action 最关键的差异）；
//! 这是「远程 = 工作延续」的契约，回归测试必须盯死。

use super::*;

fn call_input(sessions: &Sessions, id: &str, data: &str) -> serde_json::Value {
    let (server, client) = UnixStream::pair().unwrap();
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
        ServerShared {
            sessions: Arc::clone(sessions),
            acp_sessions: new_test_acp_sessions(),
            remote_sessions: new_test_remote_sessions(),
            workspace_menu: new_test_workspace_menu(),
            automations: new_test_automation_store(),
            exe_mtime: 0,
            daemon_fingerprint: None,
            event_hub: subscribers,
        }
        .context(Principal::Local),
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
