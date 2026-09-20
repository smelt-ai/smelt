//! 手机 reattach 到底会不会把 PTY 抖一遍：拿真 PTY + 真子进程数 SIGWINCH。
//!
//! 「切出去再切回来，整段对话又滚一遍」的唯一可能来源是进程被 SIGWINCH 逼着重排/重印
//! （不切备用屏的 CLI 会把整段 scrollback 重画）。快照是一条整帧消息，不会产生这种
//! 持续流。所以这里直接盯住信号次数。

use super::*;
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt as _;
use std::time::Instant;

use super::watch_tests::read_header_and_snapshot_pub;

/// 真 PTY + 一个在从端上跑、收到 SIGWINCH 就打一个 `W` 的子进程。
/// 返回 (master File, 子进程 pid)。
fn spawn_winch_reporter(cols: u16, rows: u16) -> (std::fs::File, i32) {
    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &ws as *const libc::winsize as *mut libc::winsize,
            )
        },
        0,
        "openpty 失败"
    );

    let slave_fd: RawFd = slave;
    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg("trap 'printf W' WINCH; while :; do sleep 0.05; done");
    unsafe {
        command.pre_exec(move || {
            // 独立会话 + 把从端设成控制终端，SIGWINCH 才会送到这个前台进程组。
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(slave_fd, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            for target in [0, 1, 2] {
                if libc::dup2(slave_fd, target) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            if slave_fd > 2 {
                libc::close(slave_fd);
            }
            Ok(())
        });
    }
    let child = command.spawn().unwrap();
    let pid = child.id() as i32;
    std::mem::forget(child); // 收尸交给 TerminalChild
    unsafe { libc::close(slave) };
    (unsafe { std::fs::File::from_raw_fd(master) }, pid)
}

fn make_live_session(master: std::fs::File, pid: i32, rows: u16, cols: u16) -> Arc<Session> {
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
            cell_w: 8,
            cell_h: 15,
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

/// 挂一个 watcher，读掉快照，返回 (watch 线程, 客户端 socket, reader)。
fn attach_mobile_watcher(
    sessions: &Sessions,
    cols: u16,
    rows: u16,
) -> (thread::JoinHandle<()>, UnixStream, BufReader<UnixStream>) {
    let (server, client) = UnixStream::pair().unwrap();
    let sessions_for_watch = Arc::clone(sessions);
    let watch = thread::spawn(move || {
        let reader = BufReader::new(server.try_clone().unwrap());
        handle_watch(
            server,
            reader,
            &serde_json::json!({
                "id": "t",
                "controls_geometry": true,
                "cols": cols,
                "rows": rows,
                "cell_w": 8,
                "cell_h": 15,
            }),
            sessions_for_watch,
        );
    });
    let mut reader = BufReader::new(client.try_clone().unwrap());
    read_header_and_snapshot_pub(&mut reader);
    (watch, client, reader)
}

/// 数一段时间内 watcher 收到的 `W`（= 子进程收到的 SIGWINCH 次数）。
fn count_winch(reader: &mut BufReader<UnixStream>, client: &UnixStream, window: Duration) -> usize {
    client
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let deadline = Instant::now() + window;
    let mut seen = 0;
    while Instant::now() < deadline {
        let mut buffer = [0u8; 4096];
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => seen += buffer[..read].iter().filter(|b| **b == b'W').count(),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    seen
}

#[test]
fn a_reattach_at_the_same_geometry_never_signals_the_child() {
    let (master, pid) = spawn_winch_reporter(181, 59);
    let sess = make_live_session(master.try_clone().unwrap(), pid, 59, 181);
    let sessions = new_sessions();
    sessions.insert_for_test("t", Arc::clone(&sess));
    let slot = sessions.get("t").unwrap();
    start_pty_pump(
        Arc::clone(&sess),
        Box::new(master),
        "t".to_string(),
        Arc::clone(&sessions),
        new_event_hub(),
        None,
        slot,
    );
    // 让 sh 起来把 trap 装好。
    thread::sleep(Duration::from_millis(300));

    // 第一次进入：手机尺寸和桌面不同，抖是应该的（要盖掉 reflow 残帧）。
    let (watch, client, mut reader) = attach_mobile_watcher(&sessions, 49, 47);
    let first = count_winch(&mut reader, &client, Duration::from_millis(1200));
    assert!(first > 0, "首次进入换了尺寸，子进程必须收到 SIGWINCH");
    client.shutdown(Shutdown::Write).unwrap();
    watch.join().unwrap();

    // 切后台再切回来：同尺寸续租，一次 SIGWINCH 都不该有。
    let (watch, client, mut reader) = attach_mobile_watcher(&sessions, 49, 47);
    let again = count_winch(&mut reader, &client, Duration::from_millis(1200));
    assert_eq!(
        again, 0,
        "同尺寸 reattach 不该惊动子进程——每次 SIGWINCH 都会让主屏 CLI 把整段对话重印一遍"
    );
    client.shutdown(Shutdown::Write).unwrap();
    watch.join().unwrap();

    // 用户最常做的动作：退出对话页、再点进来。桌面这段时间仍被租约锁着（GUI 只有在
    // 终端获得焦点时才会 claim），所以 PTY 尺寸原封不动，进来就是原样续租。
    {
        let ctl = sess.ctl.lock().unwrap();
        assert_eq!((ctl.cols, ctl.rows), (49, 47));
        assert_ne!(ctl.remote_grace, 0);
    }
    let (watch, client, mut reader) = attach_mobile_watcher(&sessions, 49, 47);
    let third = count_winch(&mut reader, &client, Duration::from_millis(1200));
    assert_eq!(
        third, 0,
        "退出再进入不该产生 SIGWINCH，否则每次进来都要吃一次整段重印"
    );
    client.shutdown(Shutdown::Write).unwrap();
    watch.join().unwrap();

    unsafe { libc::kill(pid, libc::SIGKILL) };
}
