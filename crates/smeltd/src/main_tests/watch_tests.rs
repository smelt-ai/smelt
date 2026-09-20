use super::*;
use std::time::Instant;

/// 造一个不依赖真实 shell 的会话：`Ctl.master` 指向 `/dev/null`（测试不发输入帧，
/// 用不上真正的 PTY 写端），`pid` 用一个立即退出的真实子进程，验证 Session 的
/// 精确 PID owner 能独立回收，不借用 -1 或随便一个不相关的 pid。
pub(crate) fn make_dummy_session(rows: u16, cols: u16) -> Arc<Session> {
    let master = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .unwrap();
    let child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id() as i32;
    drop(child); // Child::drop 不 wait()；Session 构造后立即交给 TerminalChild

    let instance = next_session_instance();
    let state = Arc::new(Mutex::new(SessionState {
        instance,
        ..Default::default()
    }));
    let subscribers = new_event_hub();
    let color_replies = Arc::new(Mutex::new(VecDeque::new()));
    let listener = StateListener::with_color_replies(
        Arc::clone(&state),
        subscribers,
        Arc::clone(&color_replies),
    );
    Arc::new(Session {
        instance,
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
    })
}

/// 读一行 JSON 尺寸头 + `replay_len` 字节快照——跟真实客户端的 attach 协议一致。
pub(crate) fn read_header_and_snapshot_pub(br: &mut BufReader<UnixStream>) -> serde_json::Value {
    read_header_and_snapshot(br)
}

fn read_header_and_snapshot(br: &mut BufReader<UnixStream>) -> serde_json::Value {
    let mut line = String::new();
    br.read_line(&mut line).unwrap();
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    let replay_len = v["replay_len"].as_u64().unwrap() as usize;
    let mut snap = vec![0u8; replay_len];
    br.read_exact(&mut snap).unwrap();
    v
}

#[test]
fn remote_watch_owns_geometry_until_its_connection_closes() {
    let sess = make_dummy_session(59, 181);
    let sessions = new_sessions();
    sessions.insert_for_test("t", Arc::clone(&sess));
    let (server, mut client) = UnixStream::pair().unwrap();
    let sessions_for_watch = Arc::clone(&sessions);
    let watch = thread::spawn(move || {
        let reader = BufReader::new(server.try_clone().unwrap());
        handle_watch(
            server,
            reader,
            &serde_json::json!({
                "id": "t",
                "controls_geometry": true,
                "cols": 49,
                "rows": 47,
                "cell_w": 8,
                "cell_h": 15,
            }),
            sessions_for_watch,
        );
    });

    let mut reader = BufReader::new(client.try_clone().unwrap());
    let header = read_header_and_snapshot(&mut reader);
    assert_eq!(header["cols"], 49);
    assert_eq!(header["rows"], 47);
    assert_eq!(sess.ctl.lock().unwrap().remote_viewports, 1);

    // A focused desktop may try to reassert its large viewport. The
    // daemon must keep the mobile canonical grid while the lease lives.
    resize_session(&sess, 181, 59, 9, 18);
    {
        let ctl = sess.ctl.lock().unwrap();
        assert_eq!((ctl.cols, ctl.rows), (49, 47));
    }

    let geometry = TerminalGeometryParamsForTest {
        cols: 55,
        rows: 40,
        cell_w: 8,
        cell_h: 15,
    };
    client.write_all(&geometry.frame()).unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    watch.join().unwrap();

    // 断开后尺寸不立刻还给桌面：先进宽限（归还信号见
    // `an_unannounced_drop_keeps_the_remote_geometry_for_a_grace_period`）。
    {
        let ctl = sess.ctl.lock().unwrap();
        assert_eq!((ctl.cols, ctl.rows), (55, 40));
        assert_eq!(ctl.remote_viewports, 0);
        assert_ne!(ctl.remote_grace, 0);
    }
    cancel_remote_viewport_grace(&sess);
    resize_session(&sess, 181, 59, 9, 18);
    let ctl = sess.ctl.lock().unwrap();
    assert_eq!((ctl.cols, ctl.rows), (181, 59));
}

/// 手机切后台时连接就没了，和「关页面」在链路上长得一样。若此时立刻归还几何租约，
/// 桌面会马上把 PTY 抢回自己的大尺寸，手机切回来再抢一次——不切备用屏的 CLI
/// （整段对话都在 scrollback 里）每次 SIGWINCH 都把对话重排重印一遍，手机上就是
/// 「一进来又滚很久」。所以没告别的断开要留一段宽限期。
#[test]
fn an_unannounced_drop_keeps_the_remote_geometry_for_a_grace_period() {
    let sess = make_dummy_session(59, 181);
    let sessions = new_sessions();
    sessions.insert_for_test("t", Arc::clone(&sess));
    let (server, client) = UnixStream::pair().unwrap();
    let sessions_for_watch = Arc::clone(&sessions);
    let watch = thread::spawn(move || {
        let reader = BufReader::new(server.try_clone().unwrap());
        handle_watch(
            server,
            reader,
            &serde_json::json!({
                "id": "t",
                "controls_geometry": true,
                "cols": 49,
                "rows": 47,
                "cell_w": 8,
                "cell_h": 15,
            }),
            sessions_for_watch,
        );
    });
    let mut reader = BufReader::new(client.try_clone().unwrap());
    read_header_and_snapshot(&mut reader);

    // 没有归还帧，直接断（= 切后台 / 掉线）。
    client.shutdown(Shutdown::Write).unwrap();
    watch.join().unwrap();

    {
        let ctl = sess.ctl.lock().unwrap();
        assert_eq!(ctl.remote_viewports, 0);
        assert_ne!(ctl.remote_grace, 0, "断开后应该进入宽限期");
        assert_eq!(
            (ctl.cols, ctl.rows),
            (49, 47),
            "宽限期内尺寸留在手机这边，等它切回来原样续租"
        );
    }

    // 手机切回来：同尺寸续租，不挂 jolt（= 不惊动子进程）。
    let (server, client) = UnixStream::pair().unwrap();
    let sessions_for_watch = Arc::clone(&sessions);
    let watch = thread::spawn(move || {
        let reader = BufReader::new(server.try_clone().unwrap());
        handle_watch(
            server,
            reader,
            &serde_json::json!({
                "id": "t",
                "controls_geometry": true,
                "cols": 49,
                "rows": 47,
                "cell_w": 8,
                "cell_h": 15,
            }),
            sessions_for_watch,
        );
    });
    let mut reader = BufReader::new(client.try_clone().unwrap());
    read_header_and_snapshot(&mut reader);
    {
        let ctl = sess.ctl.lock().unwrap();
        assert_eq!(ctl.remote_viewports, 1);
        assert_eq!(ctl.remote_grace, 0, "续租应该把宽限收掉");
        assert!(!ctl.jolt, "同尺寸续租不该挂 jolt");
    }
    client.shutdown(Shutdown::Write).unwrap();
    watch.join().unwrap();

    // 人回到 PC 敲键盘：宽限立即作废，尺寸还给桌面。
    cancel_remote_viewport_grace(&sess);
    resize_session(&sess, 181, 59, 9, 18);
    let ctl = sess.ctl.lock().unwrap();
    assert_eq!((ctl.cols, ctl.rows), (181, 59));
    assert_eq!(ctl.remote_grace, 0);
}

/// 另一个归还信号：桌面终端获得焦点时发的那一帧 resize（focus claim）。
/// 手机正在看时它抢不走；手机走了（宽限期）它立刻生效。
#[test]
fn a_desktop_focus_claim_ends_the_grace_but_never_preempts_a_live_viewer() {
    let sess = make_dummy_session(59, 181);
    let sessions = new_sessions();
    sessions.insert_for_test("t", Arc::clone(&sess));
    let (server, client) = UnixStream::pair().unwrap();
    let sessions_for_watch = Arc::clone(&sessions);
    let watch = thread::spawn(move || {
        let reader = BufReader::new(server.try_clone().unwrap());
        handle_watch(
            server,
            reader,
            &serde_json::json!({
                "id": "t",
                "controls_geometry": true,
                "cols": 49,
                "rows": 47,
                "cell_w": 8,
                "cell_h": 15,
            }),
            sessions_for_watch,
        );
    });
    let mut reader = BufReader::new(client.try_clone().unwrap());
    read_header_and_snapshot(&mut reader);

    // 手机正在看：焦点 claim 也抢不走。
    resize_session(&sess, 181, 59, 9, 18);
    {
        let ctl = sess.ctl.lock().unwrap();
        assert_eq!((ctl.cols, ctl.rows), (49, 47));
    }

    client.shutdown(Shutdown::Write).unwrap();
    watch.join().unwrap();

    // 手机走了（宽限期）：这次 claim 生效，并把宽限一并收掉。
    resize_session(&sess, 181, 59, 9, 18);
    let ctl = sess.ctl.lock().unwrap();
    assert_eq!((ctl.cols, ctl.rows), (181, 59));
    assert_eq!(ctl.remote_grace, 0, "claim 必须把宽限一并收掉");
}

/// 宽限期内原尺寸续租（手机切回来）：一次 SIGWINCH 都不能有。jolt 的作用是盖掉
/// reflow 后的旧尺寸残帧；尺寸没变就没有 reflow，快照本身就是准确画面，而白抖一下
/// 会让主屏 CLI 把整段对话重印一遍。
#[test]
fn mobile_watch_does_not_jolt_when_the_geometry_is_unchanged() {
    let sess = make_dummy_session(47, 49);
    {
        let mut ctl = sess.ctl.lock().unwrap();
        ctl.cell_w = 8;
        ctl.cell_h = 15;
    }
    let sessions = new_sessions();
    sessions.insert_for_test("t", Arc::clone(&sess));

    let (desktop_server, desktop_client) = UnixStream::pair().unwrap();
    let desktop_attachment =
        OutputAttachment::new(desktop_server, Vec::new(), "t", "attachment").unwrap();
    sess.out.lock().unwrap().clients.push(desktop_attachment);

    let (server, client) = UnixStream::pair().unwrap();
    let sessions_for_watch = Arc::clone(&sessions);
    let watch = thread::spawn(move || {
        let reader = BufReader::new(server.try_clone().unwrap());
        handle_watch(
            server,
            reader,
            &serde_json::json!({
                "id": "t",
                "controls_geometry": true,
                "cols": 49,
                "rows": 47,
                "cell_w": 8,
                "cell_h": 15,
            }),
            sessions_for_watch,
        );
    });

    let mut reader = BufReader::new(client.try_clone().unwrap());
    read_header_and_snapshot(&mut reader);
    assert!(!sess.ctl.lock().unwrap().jolt, "同尺寸续租不应该挂 jolt");

    let token = sess.geometry_token.as_bytes().to_vec();
    let mut desktop_client = desktop_client;
    desktop_client
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(900);
    while Instant::now() < deadline {
        let mut buffer = [0u8; 4096];
        match desktop_client.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => seen.extend_from_slice(&buffer[..read]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    let markers = seen.windows(token.len()).filter(|w| *w == token).count();
    assert_eq!(
        markers, 1,
        "几何没变时只该有 attach 那一条几何标记（不补抖），实际 {markers} 条"
    );

    client.shutdown(Shutdown::Write).unwrap();
    watch.join().unwrap();
}

#[test]
fn mobile_watch_jolts_once_when_geometry_changes() {
    // 事件驱动一次根治：几何变化的那一次 SIGWINCH 已在 `begin_remote_viewport`
    // 内打过，watcher 挂载后不再补抖。快照即准确画面，重绘随实时流到达；
    // 旧“挂载后补抖两次”已删除，白抖会把不切备用屏 CLI 的整段对话重排重印。
    // 若某 TUI 半屏重排复现，正确修法是客户端首绘后主动再发一次 resize，
    // 不是守护端睡 300ms+400ms 猜。
    let sess = make_dummy_session(59, 181);
    let sessions = new_sessions();
    sessions.insert_for_test("t", Arc::clone(&sess));

    // 每次 resize_session_* 都会给桌面客户端写一条 geometry OSC 标记——用它数抖动次数。
    let (desktop_server, desktop_client) = UnixStream::pair().unwrap();
    let desktop_attachment =
        OutputAttachment::new(desktop_server, Vec::new(), "t", "attachment").unwrap();
    sess.out.lock().unwrap().clients.push(desktop_attachment);

    let (server, client) = UnixStream::pair().unwrap();
    let sessions_for_watch = Arc::clone(&sessions);
    let watch = thread::spawn(move || {
        let reader = BufReader::new(server.try_clone().unwrap());
        handle_watch(
            server,
            reader,
            &serde_json::json!({
                "id": "t",
                "controls_geometry": true,
                "cols": 49,
                "rows": 47,
                "cell_w": 8,
                "cell_h": 15,
            }),
            sessions_for_watch,
        );
    });

    let mut reader = BufReader::new(client.try_clone().unwrap());
    read_header_and_snapshot(&mut reader);

    desktop_client
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let token = sess.geometry_token.as_bytes().to_vec();
    let mut seen = Vec::new();
    let mut desktop_client = desktop_client;
    let deadline = Instant::now() + Duration::from_millis(900);
    while Instant::now() < deadline {
        let mut buffer = [0u8; 4096];
        match desktop_client.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => seen.extend_from_slice(&buffer[..read]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }

    let markers = seen.windows(token.len()).filter(|w| *w == token).count();
    assert_eq!(
        markers, 1,
        "几何变化时只该有 begin_remote_viewport 的一条几何标记（不再补抖），实际 {markers} 条"
    );

    client.shutdown(Shutdown::Write).unwrap();
    watch.join().unwrap();
}

struct TerminalGeometryParamsForTest {
    cols: u16,
    rows: u16,
    cell_w: u16,
    cell_h: u16,
}

impl TerminalGeometryParamsForTest {
    fn frame(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(16);
        for value in [self.cols, self.rows, self.cell_w, self.cell_h] {
            payload.extend_from_slice(&u32::from(value).to_be_bytes());
        }
        let mut frame = vec![1];
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&payload);
        frame
    }
}

#[test]
fn attach_only_open_does_not_create_a_missing_session() {
    let sessions = new_sessions();
    let subscribers = new_event_hub();
    let remote_sessions = new_test_remote_sessions();
    let (server, client) = UnixStream::pair().unwrap();
    let reader = BufReader::new(server.try_clone().unwrap());

    handle_open(
        server,
        reader,
        &serde_json::json!({
            "id": "missing",
            "cols": 80,
            "rows": 24,
            "create_if_missing": false,
        }),
        Arc::clone(&sessions),
        new_test_acp_sessions(),
        subscribers,
        remote_sessions,
    );

    let mut line = String::new();
    BufReader::new(client).read_line(&mut line).unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["ok"], false);
    assert!(response["err"].as_str().unwrap().contains("不存在"));
    assert_eq!(sessions.len(), 0);
}

#[test]
fn runtime_ids_cannot_be_shared_across_terminal_and_acp() {
    let sessions = new_sessions();
    let acp = new_test_acp_sessions();
    sessions.insert_for_test("shared", make_dummy_session(24, 80));
    assert!(
        runtime_id_taken_by_other_kind("shared", &sessions, &acp, RemoteSessionKind::Acp).is_some()
    );
    assert!(
        runtime_id_taken_by_other_kind("shared", &sessions, &acp, RemoteSessionKind::Terminal)
            .is_none()
    );
}

#[test]
fn multiple_opens_and_watch_receive_the_same_output_independently() {
    let sess = make_dummy_session(24, 80);
    let sessions = new_sessions();
    sessions.insert_for_test("t", Arc::clone(&sess));
    let slot = sessions.get("t").unwrap();

    // 模拟 PTY：pump 从一端读，测试从另一端写，模拟"shell 产生了输出"。
    let (pty_reader_end, mut pty_writer_end) = UnixStream::pair().unwrap();
    start_pty_pump(
        Arc::clone(&sess),
        Box::new(pty_reader_end),
        "t".to_string(),
        Arc::clone(&sessions),
        new_event_hub(),
        None,
        slot,
    );

    // 第一路：桌面 attachment。
    let (open_server, open_client) = UnixStream::pair().unwrap();
    let sessions_a = Arc::clone(&sessions);
    let subscribers_a = new_event_hub();
    let remote_sessions_a = new_test_remote_sessions();
    thread::spawn(move || {
        let reader = BufReader::new(open_server.try_clone().unwrap());
        handle_open(
            open_server,
            reader,
            &serde_json::json!({"id":"t","cols":80,"rows":24}),
            sessions_a,
            new_test_acp_sessions(),
            subscribers_a,
            remote_sessions_a,
        );
    });
    let mut open_br = BufReader::new(open_client.try_clone().unwrap());
    let open_header = read_header_and_snapshot(&mut open_br);
    assert_eq!(open_header["geometry_token"], sess.geometry_token.as_str());

    // 第二路：另一个交互 attachment。同 id 并行 open 不该顶掉第一路。
    let (open2_server, open2_client) = UnixStream::pair().unwrap();
    let sessions_b = Arc::clone(&sessions);
    let subscribers_b = new_event_hub();
    let remote_sessions_b = new_test_remote_sessions();
    thread::spawn(move || {
        let reader = BufReader::new(open2_server.try_clone().unwrap());
        handle_open(
            open2_server,
            reader,
            &serde_json::json!({
                "id": "t",
                "cols": 80,
                "rows": 24,
                "create_if_missing": false,
            }),
            sessions_b,
            new_test_acp_sessions(),
            subscribers_b,
            remote_sessions_b,
        );
    });
    let mut open2_br = BufReader::new(open2_client.try_clone().unwrap());
    read_header_and_snapshot(&mut open2_br);

    // 第三路：watch（只读旁观）。同样不影响两个 open attachment。
    let (watch_server, watch_client) = UnixStream::pair().unwrap();
    let sessions_c = Arc::clone(&sessions);
    thread::spawn(move || {
        let reader = BufReader::new(watch_server.try_clone().unwrap());
        handle_watch(
            watch_server,
            reader,
            &serde_json::json!({"id":"t"}),
            sessions_c,
        );
    });
    let mut watch_br = BufReader::new(watch_client.try_clone().unwrap());
    read_header_and_snapshot(&mut watch_br);

    let out = sess.out.lock().unwrap();
    assert_eq!(out.clients.len(), 2);
    assert_eq!(out.watchers.len(), 1);
    drop(out);

    // 模拟 shell 输出一行字节，两个 open 和 watch 都该收到同一份转发。
    pty_writer_end.write_all(b"hello\r\n").unwrap();

    let mut open_buf = [0u8; 7];
    open_br.read_exact(&mut open_buf).unwrap();
    assert_eq!(
        &open_buf, b"hello\r\n",
        "open 没收到转发——watch 的接入可能把它顶掉了"
    );

    let mut open2_buf = [0u8; 7];
    open2_br.read_exact(&mut open2_buf).unwrap();
    assert_eq!(
        &open2_buf, b"hello\r\n",
        "第二个 open 没收到转发——可能仍在执行单 client 顶替"
    );

    let mut watch_buf = [0u8; 7];
    watch_br.read_exact(&mut watch_buf).unwrap();
    assert_eq!(&watch_buf, b"hello\r\n", "watch 没收到转发");

    // watcher 断开，不该影响两路 open 继续收转发（惰性清理：写失败即摘除，
    // 不依赖 handle_watch 自己那个线程的清理时序）。
    drop(watch_br);
    drop(watch_client);

    pty_writer_end.write_all(b"world!\n").unwrap();
    let mut open_buf2 = [0u8; 7];
    open_br.read_exact(&mut open_buf2).unwrap();
    assert_eq!(
        &open_buf2, b"world!\n",
        "watcher 断线后不该影响 open 那一路的转发"
    );
    let mut open2_buf2 = [0u8; 7];
    open2_br.read_exact(&mut open2_buf2).unwrap();
    assert_eq!(
        &open2_buf2, b"world!\n",
        "watcher 断线后不该影响第二路 open 的转发"
    );

    // 收尾：关掉模拟 PTY 的写端，触发 pump 把会话表项移除。child 退出状态由
    // `TerminalChild` 从构造时就独立回收，不再依赖这里的 EOF。
    drop(pty_writer_end);
    let mut removed = false;
    for _ in 0..50 {
        if sessions.get("t").is_none() {
            removed = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(removed, "pump 应在 PTY EOF 后把会话从表里摘掉");

    drop(open_br);
    drop(open_client);
    drop(open2_br);
    drop(open2_client);
}
