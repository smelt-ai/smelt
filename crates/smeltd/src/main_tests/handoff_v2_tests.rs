//! 交接 v2 端到端（除 fork/spawn 外全链路）：predecessor 快照→typed→
//! socketpair→successor 恢复→READY→COMMIT。
//!
//! fork+spawn 由真机演练覆盖（CI 里起完整 smeltd 双进程不稳定）；这里锁死
//! 协议+恢复语义：会话内容过得去、fd 真被接管、收养监控真能观察退出、
//! 坏输入干净 ABORT、grid 损坏只隔离不丢会话。

use super::*;
use crate::handoff_v2::manifest::{
    FdRole, GridRef, HandoffManifest, ProducerInfo, TerminalChildHandoff, TerminalHandoff,
    sha256_hex,
};
use crate::handoff_v2::{predecessor, successor, transport};
use alacritty_terminal::vte::ansi::Processor;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};

fn test_sock_path(name: &str) -> std::path::PathBuf {
    // sock 路径不得超过 SUN_LEN(104)：短哈希命名（与旧测试同一招）。
    use std::hash::{DefaultHasher, Hash, Hasher};
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/smeltd-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let mut hasher = DefaultHasher::new();
    (name, std::process::id()).hash(&mut hasher);
    std::fs::canonicalize(dir)
        .unwrap()
        .join(format!("v2-{:x}.sock", hasher.finish()))
}

fn visible_text<T: alacritty_terminal::event::EventListener>(term: &Term<T>) -> String {
    term.renderable_content()
        .display_iter
        .map(|i| i.cell.c)
        .filter(|c| *c != '\0')
        .collect::<String>()
}

/// 建一个活终端会话：pipe 当 master（读端给会话，写端测试方留着验 fd 接管），
/// 真 sleep 子进程（验收养监控），term 里灌一行字（验 grid）。
struct LiveSession {
    write_end: std::fs::File,
    child: std::process::Child,
    pid: i32,
}

fn insert_live_session(
    sessions: &Sessions,
    event_hub: &EventHubHandle,
    id: &str,
    text: &[u8],
) -> LiveSession {
    let mut pair = [0; 2];
    assert_eq!(unsafe { libc::pipe(pair.as_mut_ptr()) }, 0);
    let master = unsafe { std::fs::File::from_raw_fd(pair[0]) };
    let write_end = unsafe { std::fs::File::from_raw_fd(pair[1]) };
    let child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn sleep");
    let pid = child.id() as i32;

    let state = Arc::new(Mutex::new(SessionState {
        id: id.to_string(),
        instance: next_session_instance(),
        cwd: Some("/repo".to_string()),
        launch: Some("fish".to_string()),
        agent_mcp: false,
        agent_token: "tok".to_string(),
        ..Default::default()
    }));
    let color_replies = Arc::new(Mutex::new(VecDeque::new()));
    let listener = StateListener::with_color_replies(
        Arc::clone(&state),
        Arc::clone(event_hub),
        Arc::clone(&color_replies),
    );
    let mut term = new_daemon_term(24, 80, listener);
    let mut parser: Processor = Processor::new();
    parser.advance(&mut term, text);

    let (slot, created) = sessions.reserve(id);
    assert!(created);
    let sess = Arc::new(Session {
        instance: next_session_instance(),
        geometry_token: "test".into(),
        child: TerminalChild::start(pid).unwrap(),
        ctl: Mutex::new(Ctl {
            master,
            jolt: false,
            cols: 80,
            rows: 24,
            cell_w: 0,
            cell_h: 0,
            remote_viewports: 0,
            remote_grace: 0,
            cwd: Some("/repo".to_string()),
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
    });
    assert!(sessions.commit_if_current(id, &slot, sess));
    // child 句柄只留着最后 reap；会话侧 owner 是 TerminalChild。
    LiveSession {
        write_end,
        child,
        pid,
    }
}

impl LiveSession {
    fn kill_and_reap(mut self) {
        unsafe {
            libc::kill(self.pid, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

/// 全链路：真快照→真传输→真恢复→COMMIT→接管验证（内容/fd/收养/listener）。
#[test]
fn import_loopback_restores_live_session() {
    let hub_pre = new_event_hub();
    let sessions = new_sessions();
    let mut live = insert_live_session(&sessions, &hub_pre, "v2t1", b"hello-import-v2");

    let sock_path = test_sock_path("loopback");
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).unwrap();
    let listen_fd = listener.as_raw_fd();

    let (pre_sock, imp_sock) = transport::socketpair().unwrap();
    let hub_imp = new_event_hub();
    let successor = std::thread::Builder::new()
        .name("v2-test-successor".into())
        .spawn(move || successor::run_import(&imp_sock, &hub_imp, None, false))
        .unwrap();

    // predecessor（主线程，guards 持有中发完三段，与生产同锁行为）。
    let acp_sessions = new_acp_sessions();
    let (ready_sessions, manifest) =
        predecessor::with_snapshot(&sessions, &acp_sessions, listen_fd, |snapshot| {
            let staged = &snapshot.staged;
            assert_eq!(staged.manifest.sessions.len(), 1);
            assert_eq!(staged.manifest.fd_roles[0], FdRole::Listen);
            assert_eq!(staged.fds.len(), staged.manifest.fd_roles.len());
            assert_eq!(staged.manifest.grids.len(), 1);
            transport::send_manifest(&pre_sock, &staged.manifest).unwrap();
            transport::send_fds(&pre_sock, &staged.fds).unwrap();
            transport::send_grids(&pre_sock, &staged.manifest.grids, &staged.grid_blobs).unwrap();
            let ready = transport::await_ready(&pre_sock).unwrap();
            transport::send_commit(&pre_sock).unwrap();
            (ready.restored_terminals, staged.manifest.clone())
        });
    assert_eq!(ready_sessions, 1);
    assert_eq!(manifest.sessions[0].id, "v2t1");

    let outcome = successor.join().unwrap();
    let successor::ImportOutcome::Restored {
        listener: adopted,
        sessions: restored,
        acp_sessions: restored_acp,
        ready,
    } = outcome
    else {
        panic!("应成功接管");
    };
    assert_eq!(ready.restored_terminals, 1);
    assert_eq!(ready.restored_acp, 0);
    assert!(ready.dropped_grids.is_empty());
    assert_eq!(restored_acp.snapshot().len(), 0);

    // 内容：grid keyframe 穿过去了。
    let sess = restored.live("v2t1").expect("会话应恢复");
    let term = sess.term.lock().unwrap();
    assert!(
        visible_text(&term).contains("hello-import-v2"),
        "grid 内容应恢复"
    );
    assert_eq!(sess.ctl.lock().unwrap().cols, 80);
    drop(term);

    // fd 接管：往写端写，adopt 的 master 能读出来。按住 output_gate 让 pump
    // 暂停（门闩的本来用途），排除 pump 抢读 race，断言完全确定。
    live.write_end.write_all(b"ping").unwrap();
    let _pump_hold = sess.output_gate.lock().unwrap();
    let master = sess.ctl.lock().unwrap().master.try_clone().unwrap();
    let mut buf = [0u8; 4];
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match (&master).read(&mut buf) {
            Ok(4) => break,
            Ok(_) => panic!("pipe 字节不应被拆读"),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "adopt 的 master 读不到 predecessor 写端的字节"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("adopt 的 master 读失败：{error}"),
        }
    }
    assert_eq!(&buf, b"ping");
    drop(_pump_hold);
    drop(sess);

    // listener 接管：新连接能进 backlog 且被 adopt 的 fd accept。
    let adopted_thread = std::thread::spawn(move || adopted.accept().unwrap());
    let _conn = UnixStream::connect(&sock_path).unwrap();
    let (accepted, _) = adopted_thread.join().unwrap();
    accepted.shutdown(std::net::Shutdown::Both).ok();

    // 收养监控：杀掉 sleep，adopted owner 必须观察到退出。
    let sess = restored.live("v2t1").unwrap();
    assert!(
        !sess.child.wait_reaped(Duration::from_millis(100)),
        "杀之前应为 Running"
    );
    live.kill_and_reap();
    assert!(
        sess.child.wait_reaped(Duration::from_secs(5)),
        "收养的子进程退出必须被观察到"
    );

    drop(restored);
    let _ = std::fs::remove_file(&sock_path);
}

/// with_snapshot：pid<=1 跳过、已退出照带 fd（恢复成"已结束"）。
#[test]
fn snapshot_skips_childless_and_carries_exited() {
    let hub = new_event_hub();
    let sessions = new_sessions();
    let live = insert_live_session(&sessions, &hub, "t-live", b"x");

    // 无子进程会话（spawn 未成）：master 有效但 pid 非法 → 跳过。
    {
        let mut pair = [0; 2];
        assert_eq!(unsafe { libc::pipe(pair.as_mut_ptr()) }, 0);
        let master = unsafe { std::fs::File::from_raw_fd(pair[0]) };
        let write_end = unsafe { std::fs::File::from_raw_fd(pair[1]) };
        let state = Arc::new(Mutex::new(SessionState::default()));
        let color_replies = Arc::new(Mutex::new(VecDeque::new()));
        let listener =
            StateListener::with_color_replies(Arc::clone(&state), hub.clone(), color_replies);
        let (slot, _) = sessions.reserve("t-nochild");
        let sess = Arc::new(Session {
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
            color_replies: Arc::new(Mutex::new(VecDeque::new())),
            term: Mutex::new(new_daemon_term(24, 80, listener)),
            state,
        });
        assert!(sessions.commit_if_current("t-nochild", &slot, sess));
        drop(write_end);
    }
    // 已退出会话（stale pid + 有效 master）：必须进 manifest（带 fd 角色）。
    {
        let mut pair = [0; 2];
        assert_eq!(unsafe { libc::pipe(pair.as_mut_ptr()) }, 0);
        let master = unsafe { std::fs::File::from_raw_fd(pair[0]) };
        let write_end = unsafe { std::fs::File::from_raw_fd(pair[1]) };
        let state = Arc::new(Mutex::new(SessionState::default()));
        let color_replies = Arc::new(Mutex::new(VecDeque::new()));
        let listener =
            StateListener::with_color_replies(Arc::clone(&state), hub.clone(), color_replies);
        let (slot, _) = sessions.reserve("t-exited");
        let sess = Arc::new(Session {
            instance: next_session_instance(),
            geometry_token: "test".into(),
            child: TerminalChild::finished(424242, TerminalChildExit::AlreadyReaped),
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
            color_replies: Arc::new(Mutex::new(VecDeque::new())),
            term: Mutex::new(new_daemon_term(24, 80, listener)),
            state,
        });
        assert!(sessions.commit_if_current("t-exited", &slot, sess));
        drop(write_end);
    }

    let sock_path = test_sock_path("snapshot");
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).unwrap();
    let acp_sessions = new_acp_sessions();
    predecessor::with_snapshot(&sessions, &acp_sessions, listener.as_raw_fd(), |snapshot| {
        let manifest = &snapshot.staged.manifest;
        // snapshot() 是 HashMap 顺序：排序后断言（角色/fd 同序组装，对齐不受影响）。
        let mut ids: Vec<&str> = manifest.sessions.iter().map(|s| s.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["t-exited", "t-live"]);
        // 每个终端条目恰好一个 master 角色（已退出也一样）。
        let masters = manifest
            .fd_roles
            .iter()
            .filter(|r| matches!(r, FdRole::TerminalMaster { .. }))
            .count();
        assert_eq!(masters, 2);
        assert_eq!(snapshot.staged.fds.len(), manifest.fd_roles.len());
    });

    live.kill_and_reap();
    let _ = std::fs::remove_file(&sock_path);
}

fn tiny_live_manifest(session_id: &str, pid: i32) -> HandoffManifest {
    HandoffManifest {
        producer: ProducerInfo {
            version: "0.9.0".to_string(),
            pid: 1,
        },
        snapshot_wall_ms: 0,
        fd_roles: vec![
            FdRole::Listen,
            FdRole::TerminalMaster {
                session_id: session_id.to_string(),
            },
        ],
        sessions: vec![TerminalHandoff {
            id: session_id.to_string(),
            child: TerminalChildHandoff::Live { pid },
            cols: 80,
            rows: 24,
            cwd: None,
            launch: None,
            agent_mcp: false,
            agent_token: String::new(),
            alt_screen: false,
        }],
        acp: Vec::new(),
        menu_gui_pids: Vec::new(),
        grids: Vec::new(),
    }
}

/// 坏 manifest→干净 ABORT：两端都收到明确信号，无会话恢复、无 hangs。
#[test]
fn import_aborts_on_corrupt_manifest() {
    let sock_path = test_sock_path("bad-manifest");
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).unwrap();
    let (pre_sock, imp_sock) = transport::socketpair().unwrap();
    let hub_imp = new_event_hub();
    let successor =
        std::thread::spawn(move || successor::run_import(&imp_sock, &hub_imp, None, false));

    let manifest = tiny_live_manifest("t1", 123456);
    let mut frame = crate::handoff_v2::manifest::encode_frame(&manifest).unwrap();
    // 翻正文最后一个字节：长度合法、校验必炸。
    let last = frame.len() - 1;
    frame[last] ^= 0xff;
    let mut pre = pre_sock.try_clone().unwrap();
    pre.write_all(&frame).unwrap();
    drop(pre);

    // successor 应回 ABORT（而不是默默 EOF）。
    let abort = transport::await_ready(&pre_sock);
    assert!(matches!(
        abort,
        Err(transport::TransportError::PeerAbort(_))
    ));
    drop(pre_sock);
    assert!(matches!(
        successor.join().unwrap(),
        successor::ImportOutcome::Aborted { .. }
    ));
    drop(listener);
    let _ = std::fs::remove_file(&sock_path);
}

/// 坏 grid 只隔离不丢会话：两会话都恢复，坏的进 dropped。
#[test]
fn import_isolates_bad_grid() {
    let sock_path = test_sock_path("bad-grid");
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).unwrap();

    let mut pair1 = [0; 2];
    let mut pair2 = [0; 2];
    assert_eq!(unsafe { libc::pipe(pair1.as_mut_ptr()) }, 0);
    assert_eq!(unsafe { libc::pipe(pair2.as_mut_ptr()) }, 0);
    let r1 = unsafe { std::fs::File::from_raw_fd(pair1[0]) };
    let w1 = unsafe { std::fs::File::from_raw_fd(pair1[1]) };
    let r2 = unsafe { std::fs::File::from_raw_fd(pair2[0]) };
    let w2 = unsafe { std::fs::File::from_raw_fd(pair2[1]) };

    let mut child1 = std::process::Command::new("/bin/sleep")
        .arg("30")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid1 = child1.id() as i32;
    let mut child2 = std::process::Command::new("/bin/sleep")
        .arg("30")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid2 = child2.id() as i32;

    let mut manifest = tiny_live_manifest("t1", pid1);
    manifest.fd_roles.push(FdRole::TerminalMaster {
        session_id: "t2".to_string(),
    });
    manifest.sessions.push(TerminalHandoff {
        id: "t2".to_string(),
        child: TerminalChildHandoff::Live { pid: pid2 },
        cols: 80,
        rows: 24,
        cwd: None,
        launch: None,
        agent_mcp: false,
        agent_token: String::new(),
        alt_screen: false,
    });
    manifest.grids.push(GridRef {
        session_id: "t1".to_string(),
        len: 5,
        sha256: sha256_hex(b"hello"),
    });
    manifest.grids.push(GridRef {
        session_id: "t2".to_string(),
        len: 5,
        sha256: sha256_hex(b"world"),
    });

    let (pre_sock, imp_sock) = transport::socketpair().unwrap();
    let hub_imp = new_event_hub();
    let successor =
        std::thread::spawn(move || successor::run_import(&imp_sock, &hub_imp, None, false));

    transport::send_manifest(&pre_sock, &manifest).unwrap();
    transport::send_fds(
        &pre_sock,
        &[listener.as_raw_fd(), r1.as_raw_fd(), r2.as_raw_fd()],
    )
    .unwrap();
    // t2 的字节故意发错：声明 world 实发 WORLD。
    transport::send_grids(
        &pre_sock,
        &manifest.grids,
        &[b"hello".to_vec(), b"WORLD".to_vec()],
    )
    .unwrap();
    let ready = transport::await_ready(&pre_sock).unwrap();
    assert_eq!(ready.restored_terminals, 2);
    assert_eq!(ready.dropped_grids, vec!["t2".to_string()]);
    transport::send_commit(&pre_sock).unwrap();

    let successor::ImportOutcome::Restored {
        sessions, ready, ..
    } = successor.join().unwrap()
    else {
        panic!("grid 损坏不应 ABORT 整个事务");
    };
    assert_eq!(ready.dropped_grids, vec!["t2".to_string()]);
    assert!(sessions.live("t1").is_some());
    assert!(sessions.live("t2").is_some());
    // t1 内容恢复，t2 空 Term（不含 world）。
    let t1 = sessions.live("t1").unwrap();
    assert!(
        visible_text(&t1.term.lock().unwrap()).contains("hello"),
        "好 grid 应恢复"
    );
    drop(t1);
    let t2 = sessions.live("t2").unwrap();
    assert!(
        !visible_text(&t2.term.lock().unwrap()).contains("world"),
        "坏 grid 不得灌入"
    );
    assert!(
        !visible_text(&t2.term.lock().unwrap()).contains("WORLD"),
        "坏 grid 字节不得灌入"
    );
    drop(t2);

    unsafe {
        libc::kill(pid1, libc::SIGKILL);
        libc::kill(pid2, libc::SIGKILL);
    }
    let _ = child1.wait();
    let _ = child2.wait();
    drop((r1, w1, r2, w2, sessions, listener));
    let _ = std::fs::remove_file(&sock_path);
}

/// ABANDON 后不接管：successor 安静退出（调用方 exit，无 TooManyFds 之忧）。
#[test]
fn import_abandon_is_not_takeover() {
    let sock_path = test_sock_path("abandon");
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).unwrap();
    let mut pair = [0; 2];
    assert_eq!(unsafe { libc::pipe(pair.as_mut_ptr()) }, 0);
    let r = unsafe { std::fs::File::from_raw_fd(pair[0]) };
    let w = unsafe { std::fs::File::from_raw_fd(pair[1]) };
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id() as i32;

    let manifest = tiny_live_manifest("t1", pid);
    let (pre_sock, imp_sock) = transport::socketpair().unwrap();
    let hub_imp = new_event_hub();
    let successor =
        std::thread::spawn(move || successor::run_import(&imp_sock, &hub_imp, None, false));

    transport::send_manifest(&pre_sock, &manifest).unwrap();
    transport::send_fds(&pre_sock, &[listener.as_raw_fd(), r.as_raw_fd()]).unwrap();
    transport::send_grids(&pre_sock, &manifest.grids, &[]).unwrap();
    transport::await_ready(&pre_sock).unwrap();
    transport::send_abandon(&pre_sock).unwrap();

    assert!(matches!(
        successor.join().unwrap(),
        successor::ImportOutcome::Aborted { .. }
    ));

    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    let _ = child.wait();
    drop((r, w, listener));
    let _ = std::fs::remove_file(&sock_path);
}

/// ACP 活体灾备 drill：hosted 会话正跑 tool call（Thinking + turn 已开始 + 未完成
/// tool + prompt 闸门置位）时直接演练完整 v2 回环。锁三件事：
/// 1. 正常升级 blockers 会拒绝主动切换；
/// 2. 底层 handoff 原语仍能把 mid-turn 状态带过去（相位/turn 标记/tool 记录原样）；
/// 3. 恢复后控制通道双向活着：daemon→host 的 Refresh 能到，host→daemon 的
///    快照能被 drain merge（revision 落盘）。
///
/// fork+真宿主进程由真机演练覆盖；这里用 test_stub 演宿主，锁死协议+恢复语义。
#[test]
fn import_loopback_restores_hosted_acp_mid_turn() {
    use smelt_core::acp_chat::{AcpEntry, ToolCallStatus, ToolKind};
    use smelt_core::acp_session::AcpSessionState;
    use smelt_core::daemon_state::DaemonPhase;

    // 被测会话：test_stub 当 session host（peer 端测试方握着演活宿主）。
    let acp_sessions = new_acp_sessions();
    let mut running = AcpSessionState::default();
    running.phase = DaemonPhase::Thinking;
    running.turn_started_at_ms = Some(1);
    running.entries.push(AcpEntry::ToolCall {
        id: "t-drill".into(),
        title: "Read".into(),
        kind: ToolKind::Read,
        status: ToolCallStatus::InProgress,
        output: Vec::new(),
        children: Vec::new(),
    });
    let (slot, _) =
        acp_sessions.reserve_with("acp-drill", || make_acp_session_value("acp-drill", running));
    // prompt 闸门也置位：正常升级必须等待回合结束；下面直接调用 handoff
    // 原语，验证异常接管/灾备时仍能恢复 mid-turn。
    slot.value.prompt_in_flight.store(true, Ordering::SeqCst);
    let (hosted, mut peer) = acp_runtime_host::HostedConversationHandle::test_stub();
    *slot.value.hosted_handle.lock().unwrap() = Some(hosted);

    // 1. 门控：恢复能力不能成为正常升级主动切断回合的理由。
    assert_eq!(
        acp_upgrade_blockers(&acp_sessions),
        vec!["acp-drill"],
        "hosted mid-turn 必须阻止正常升级"
    );

    // 2. 回环：终端空表，只带 ACP。
    let sessions = new_sessions();
    let sock_path = test_sock_path("acp-drill");
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).unwrap();
    let listen_fd = listener.as_raw_fd();

    let hub_imp = new_event_hub();
    let (pre_sock, imp_sock) = transport::socketpair().unwrap();
    let successor =
        std::thread::spawn(move || successor::run_import(&imp_sock, &hub_imp, None, false));

    predecessor::with_snapshot(&sessions, &acp_sessions, listen_fd, |snapshot| {
        let staged = &snapshot.staged;
        assert_eq!(staged.manifest.acp.len(), 1);
        assert!(
            staged.manifest.fd_roles.contains(&FdRole::AcpHost {
                session_id: "acp-drill".to_string()
            }),
            "hosted 会话必须带控制 fd 角色"
        );
        transport::send_manifest(&pre_sock, &staged.manifest).unwrap();
        transport::send_fds(&pre_sock, &staged.fds).unwrap();
        transport::send_grids(&pre_sock, &staged.manifest.grids, &staged.grid_blobs).unwrap();
        let ready = transport::await_ready(&pre_sock).unwrap();
        assert_eq!(ready.restored_acp, 1);
        transport::send_commit(&pre_sock).unwrap();
    });

    let successor::ImportOutcome::Restored {
        acp_sessions: restored_acp,
        ..
    } = successor.join().unwrap()
    else {
        panic!("ACP 交接应成功接管");
    };
    let restored_slot = restored_acp.get("acp-drill").expect("ACP 会话应恢复");
    let restored = &restored_slot.value;
    {
        let reduced = restored.reduced.lock().unwrap();
        assert_eq!(reduced.phase, DaemonPhase::Thinking);
        assert_eq!(reduced.turn_started_at_ms, Some(1));
        assert!(
            smelt_core::acp_chat::has_unfinished_tool_call(&reduced.entries),
            "未完成 tool 必须带过去"
        );
    }
    assert!(
        restored.hosted_handle.lock().unwrap().is_some(),
        "恢复后应重建宿主句柄"
    );
    assert!(
        restored.prompt_in_flight.load(Ordering::SeqCst),
        "mid-turn 闸门应保持置位"
    );

    // 3a. 方向 daemon→host：经恢复后句柄发 Refresh，peer 端必须读到行
    // （恢复时已发过一行，读一行即可——两行都走 adopt 后的 fd）。
    restored
        .hosted_handle
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .request_full_snapshot()
        .unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut line = String::new();
    BufReader::new(&peer).read_line(&mut line).unwrap();
    assert!(
        line.contains("Refresh") || line.contains("refresh"),
        "peer 应收到 Refresh 动作，实际 {line:?}"
    );

    // 3b. 方向 host→daemon：peer 发快照行，drain merge 后 revision 落盘。
    // 快照取恢复后状态自身序列化（连续性天然成立），只抬 revision。
    //
    // 回环特有处理：predecessor 表还活着，它的旧 reader 线程会跟 successor
    // 的新 reader 抢同一条 socket（生产里前任 COMMIT 后即 exit，不存在此 race）。
    // 先 drop 前任表+slot：旧句柄的 channel 一死，旧 reader 偷到一行就退出；
    // 再用发—查—重试循环：第一行若被旧 reader 偷走（它随即死），第二行必达。
    drop(slot);
    drop(acp_sessions);
    let snapshot = restored.reduced.lock().unwrap().to_snapshot(false);
    let mut snapshot_value = serde_json::to_value(&snapshot).unwrap();
    snapshot_value["snapshot_revision"] = serde_json::json!(100u64);
    let marker = format!(
        "{}\n",
        serde_json::json!({"snapshot": snapshot_value, "provider_pid": null})
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if restored.host_snapshot_revision.load(Ordering::SeqCst) == 100 {
            break;
        }
        assert!(Instant::now() < deadline, "宿主快照应被 drain 线程 merge");
        peer.write_all(marker.as_bytes()).unwrap();
        peer.flush().unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }

    drop(restored_slot);
    drop(restored_acp);
    drop(listener);
    let _ = std::fs::remove_file(&sock_path);
}
