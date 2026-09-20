//! `resume_handoff` 的行为——这是「无缝升级」的落地点，也是全文件最该被守住的一段：
//! 它一旦出错，用户正在跑的 agent 会话会在升级瞬间集体消失，且没有任何补救。
//! 此前这里**一个测试都没有**，几条要命的不变量全靠注释。

use super::*;
use crate::terminal_snapshot::snapshot_ansi_for_handoff;
use alacritty_terminal::index::Column;
use alacritty_terminal::term::TermMode;

fn no_subs() -> EventHubHandle {
    new_event_hub()
}

fn test_artifact_path(name: &str, extension: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/smeltd-tests");
    std::fs::create_dir_all(&dir).unwrap();
    if extension == "sock" {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        name.hash(&mut hasher);
        return std::fs::canonicalize(dir).unwrap().join(format!(
            "s-{}-{:x}.sock",
            std::process::id(),
            hasher.finish()
        ));
    }
    dir.join(format!(
        "smelt-test-{name}-{}.{}",
        std::process::id(),
        extension
    ))
}

/// 每个用例一个独立文件名：测试是多线程并行跑的，共用路径会互相踩。
fn tmp_handoff(name: &str) -> String {
    test_artifact_path(&format!("handoff-{name}"), "json")
        .to_string_lossy()
        .into_owned()
}

#[test]
fn missing_file_returns_none() {
    let p = tmp_handoff("missing");
    let _ = std::fs::remove_file(&p);
    assert!(resume_handoff(&p, &no_subs()).is_none());
}

#[test]
fn malformed_json_returns_none() {
    let p = tmp_handoff("malformed");
    std::fs::write(&p, "{ this is not json").unwrap();
    assert!(
        resume_handoff(&p, &no_subs()).is_none(),
        "解析失败必须走全新启动，而不是 panic 把守护带走"
    );
    let _ = std::fs::remove_file(&p);
}

/// 读到手就删：文件残留下来会被下次启动误认成「有交接要恢复」，
/// 那时里面的 fd 早已属于别的东西。失败路径也必须删。
#[test]
fn consumes_handoff_file_even_when_parse_fails() {
    let p = tmp_handoff("consume");
    std::fs::write(&p, "{ not json").unwrap();
    let _ = resume_handoff(&p, &no_subs());
    assert!(
        !std::path::Path::new(&p).exists(),
        "handoff 文件读完必须删掉，无论恢复成功与否"
    );
}

#[test]
fn missing_listen_fd_returns_none() {
    let p = tmp_handoff("no-listen-fd");
    std::fs::write(&p, r#"{"sessions":[]}"#).unwrap();
    assert!(resume_handoff(&p, &no_subs()).is_none());
    let _ = std::fs::remove_file(&p);
}

/// exec 前忘了清 CLOEXEC 的话，这里拿到的就是无效 fd——必须识别出来走全新启动，
/// 而不是把一个野 fd 当监听 socket 用。
/// 一个绝不会被分配到的 fd 号：远超 ulimit -n，fcntl 必然 EBADF。
///
/// 不能用「open 一个再 close，拿它的号当无效 fd」——测试是多线程并行跑的，
/// 号一释放就会被别的用例的 pipe() 拿去，于是「无效 fd」其实是别人的活 fd，
/// resume_handoff 接管后 close 掉，对面就 double close：
/// `IO Safety violation: owned file descriptor already closed`。这里踩过。
const NEVER_VALID_FD: RawFd = 1_000_000;

#[test]
fn invalid_listen_fd_returns_none() {
    let p = tmp_handoff("bad-listen-fd");
    std::fs::write(
        &p,
        format!(r#"{{"listen_fd":{NEVER_VALID_FD},"sessions":[]}}"#),
    )
    .unwrap();
    assert!(resume_handoff(&p, &no_subs()).is_none());
    let _ = std::fs::remove_file(&p);
}

/// 造一个能被 resume_handoff 认领的监听 fd。
fn make_listen_fd(name: &str) -> RawFd {
    let sock = test_artifact_path(name, "sock");
    let _ = std::fs::remove_file(&sock);
    let l = UnixListener::bind(&sock).unwrap();
    let _ = std::fs::remove_file(&sock); // 已 bind，文件可以立刻删
    std::os::unix::io::IntoRawFd::into_raw_fd(l)
}

/// 造一个「PTY master」替身：openpty 的真 master，泵线程会真的从它读。
/// 不能用 pipe 写端——泵线程从写端读立即 EBADF，被当成「会话结束」从
/// sessions 表移除，恢复后的断言单线程下也会偶发扑空（恢复与泵线程
/// 移除是竞态）。slave 端由测试保管：测试期间 master 读不到 EOF，会话
/// 稳定留在表里；测试结束 slave drop，泵线程读到 EOF 自然退出并收尸。
/// 返回 (master_fd, slave 保管者, pid)。
fn make_fake_pty() -> (RawFd, std::fs::File, i32) {
    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0,
        "openpty 失败"
    );
    // 用一个真实存在过的 pid：让泵线程结束时的 waitpid 有合法目标，不借用 -1
    let child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id() as i32;
    drop(child); // 留成 zombie，交给泵收尸
    (master, unsafe { std::fs::File::from_raw_fd(slave) }, pid)
}

/// 与 `make_fake_pty` 相同，但直接子进程保持存活，用来验证恢复坏条目时不会误杀稍后
/// 被有效条目认领的同一个 PID。
fn make_live_fake_pty() -> (RawFd, std::fs::File, i32) {
    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0,
        "openpty 失败"
    );
    let child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id() as i32;
    drop(child); // 唯一 TerminalChild owner 会 wait；这里仅保留 PID 作为测试输入。
    (master, unsafe { std::fs::File::from_raw_fd(slave) }, pid)
}

fn wait_until_reaped_without_stealing_status(pid: i32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn handoff_lock_keeps_exit_status_owned_until_exec_boundary() {
    let child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id() as i32;
    drop(child);
    let owner = TerminalChild::start(pid).unwrap();

    let handoff_state = owner.lock_for_handoff();
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    waitid_exact_child(pid, libc::WEXITED | libc::WNOWAIT)
        .expect("退出状态应保持可等待但尚未被消费");

    assert!(
        matches!(
            crate::handoff_v2::predecessor::terminal_child_handoff(&handoff_state, pid),
            Some(crate::handoff_v2::manifest::TerminalChildHandoff::Live { .. })
        ),
        "handoff 快照必须记录新映像仍需接管退出状态"
    );
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        0,
        "handoff 锁持有期间 waiter 不能释放 PID 槽位"
    );

    drop(handoff_state);
    assert!(
        owner.wait_reaped(Duration::from_secs(1)),
        "handoff 失败/回滚释放锁后，原 owner 必须完成回收"
    );
}

#[test]
fn restored_terminal_reaps_its_shell_before_pty_eof() {
    let p = tmp_handoff("reap-before-pty-eof");
    let listen_fd = make_listen_fd("reap-before-pty-eof");
    let (master_fd, slave, pid) = make_fake_pty();
    std::fs::write(
        &p,
        serde_json::json!({
            "listen_fd": listen_fd,
            "sessions": [{
                "id": "reap-before-pty-eof",
                "fd": master_fd,
                "pid": pid,
                "cols": 80,
                "rows": 24,
            }]
        })
        .to_string(),
    )
    .unwrap();

    let (_listener, sessions, _acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
    assert!(
        sessions.live("reap-before-pty-eof").is_some(),
        "slave 仍开着时 PTY 会话应保持可恢复"
    );
    let reaped = wait_until_reaped_without_stealing_status(pid, Duration::from_secs(1));
    if !reaped {
        unsafe {
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
    }
    drop(slave);
    assert!(
        reaped,
        "shell 已退出时必须立即由精确 pid 回收，不能等仍被后代持有的 PTY 出现 EOF"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn restored_menu_gui_pid_gets_an_exact_reaper_owner() {
    let p = tmp_handoff("menu-gui-reaper");
    let listen_fd = make_listen_fd("menu-gui-reaper");
    let child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
    let pid = child.id() as i32;
    drop(child);
    std::fs::write(
        &p,
        serde_json::json!({
            "listen_fd": listen_fd,
            "sessions": [],
            "menu_gui_pids": [pid],
        })
        .to_string(),
    )
    .unwrap();

    let (_listener, _sessions, _acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
    let reaped = wait_until_reaped_without_stealing_status(pid, Duration::from_secs(1));
    if !reaped {
        unsafe {
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
    }
    assert!(
        reaped,
        "exec 会销毁旧 GUI reaper 线程，新映像必须按 handoff PID 重建精确 owner"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn reaped_terminal_pid_does_not_claim_a_reused_menu_child() {
    let p = tmp_handoff("reaped-terminal-reused-menu-pid");
    let listen_fd = make_listen_fd("reaped-terminal-reused-menu-pid");
    let child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id() as i32;
    drop(child);
    std::fs::write(
        &p,
        serde_json::json!({
            "listen_fd": listen_fd,
            "sessions": [{
                "id": "already-reaped-terminal",
                "fd": NEVER_VALID_FD,
                "pid": pid,
                "child_needs_reaper": false,
            }],
            "menu_gui_pids": [pid],
        })
        .to_string(),
    )
    .unwrap();

    let (_listener, sessions, _acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
    assert!(sessions.live("already-reaped-terminal").is_none());
    let stayed_alive = unsafe { libc::kill(pid, 0) == 0 };

    // 无论断言结果如何都清理测试进程；修复后由菜单栏的精确 PID owner 回收。
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    let cleanup_ok = wait_until_reaped_without_stealing_status(pid, Duration::from_secs(1));

    assert!(cleanup_ok, "测试子进程应由唯一 owner 回收");
    assert!(
        stayed_alive,
        "已经回收的终端 PID 只是历史数值，不能抢占并杀死复用该 PID 的菜单子进程"
    );
}

fn term_text_of(sess: &Arc<Session>) -> String {
    let term = sess.term.lock().unwrap();
    smelt_core::term_text::text_lines(&term).join("\n")
}

/// **永不 feed ring**——本文件头号不变量，此前只有注释在守。
///
/// 旧版 handoff 文件会带 `"buf"`（每会话的环形原始字节）。环形缓冲是按容量截断的，
/// 截断点可能正落在一条 CSI 序列中间，feed 进去必然花屏。所以画面只认从常驻 Term
/// 导出的 `grid` keyframe，`buf` 即便存在也必须被忽略。
///
/// 这条一旦被「顺手优化」掉（比如有人觉得「没 grid 时用 buf 兜底也行」），
/// 症状是升级后终端花屏，且只在带旧交接文件的机器上出现——极难复现。
#[test]
fn never_feeds_legacy_ring_buffer_even_when_present() {
    let p = tmp_handoff("no-feed-ring");
    let listen_fd = make_listen_fd("no-feed-ring");
    let (master_fd, _read_end, pid) = make_fake_pty();

    let handoff = serde_json::json!({
        "listen_fd": listen_fd,
        "sessions": [{
            "id": "s1",
            "fd": master_fd,
            "pid": pid,
            "cols": 80,
            "rows": 24,
            // 旧字段：必须被忽略
            "buf": hex_encode(b"RINGBUF-MUST-NOT-RENDER"),
            // 唯一信源
            "grid": hex_encode(b"GRIDKEYFRAME-OK"),
        }]
    });
    std::fs::write(&p, handoff.to_string()).unwrap();

    let (_listener, sessions, _acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
    let sess = sessions.live("s1").expect("会话 s1 应存在");
    let text = term_text_of(&sess);

    assert!(
        text.contains("GRIDKEYFRAME-OK"),
        "grid keyframe 应被 feed：{text:?}"
    );
    assert!(
        !text.contains("RINGBUF"),
        "buf（环形原始字节）绝不能被 feed——它可能在 CSI 中间腰斩，feed 必花屏：{text:?}"
    );
}

/// **没有 grid、只有 buf 时，仍然不许 feed buf**——这才是「永不 feed ring」真正
/// 会被破坏的地方：老版交接文件就是只有 buf 没有 grid，一旦有人觉得
/// 「没 grid 时拿 buf 兜一下也行」，花屏就回来了。
///
/// 上面那条 `never_feeds_legacy_ring_buffer_even_when_present` 挡不住这种改法
/// （它的用例里 grid 存在，走不到兜底分支）——变异测试实测漏过。两条都要有。
#[test]
fn ignores_buf_when_grid_absent() {
    let p = tmp_handoff("buf-no-grid");
    let listen_fd = make_listen_fd("buf-no-grid");
    let (master_fd, _read_end, pid) = make_fake_pty();

    let handoff = serde_json::json!({
        "listen_fd": listen_fd,
        "sessions": [{
            "id": "s1", "fd": master_fd, "pid": pid, "cols": 80, "rows": 24,
            // 老版交接文件的形态：只有 buf，没有 grid
            "buf": hex_encode(b"LEGACYRING-MUST-NOT-RENDER"),
        }]
    });
    std::fs::write(&p, handoff.to_string()).unwrap();

    let (_l, sessions, _acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
    let sess = sessions.live("s1").unwrap();
    let text = term_text_of(&sess);
    assert!(
        !text.contains("LEGACYRING"),
        "没有 grid 时也不能拿 buf 兜底——环形字节可能在 CSI 中间腰斩，feed 必花屏。\
         宁可空屏 + jolt 让进程自绘：{text:?}"
    );
}

/// handoff 必须保存主屏的 scrollback：Codex 的启动命令不能把它降级成 viewport。
/// 这里从真实 Term 生成 keyframe，再经 resume_handoff 重建，覆盖升级后的完整链路。
#[test]
fn handoff_restores_codex_main_screen_scrollback_from_grid_keyframe() {
    let size = DaemonTermSize { rows: 3, cols: 40 };
    let mut source = Term::new(daemon_term_config(), &size, VoidListener);
    let mut source_parser: Processor = Processor::new();
    for i in 0..10 {
        source_parser.advance(
            &mut source,
            format!("handoff-codex-line-{i:02}\r\n").as_bytes(),
        );
    }
    assert!(!source.mode().contains(TermMode::ALT_SCREEN));

    let grid = snapshot_ansi_for_handoff(
        &source,
        Some("codex --dangerously-bypass-approvals-and-sandbox"),
    );
    assert!(
        String::from_utf8_lossy(&grid).contains("handoff-codex-line-00"),
        "主屏 handoff keyframe 必须含早期历史"
    );

    let p = tmp_handoff("codex-history");
    let listen_fd = make_listen_fd("codex-history");
    let (master_fd, _slave, pid) = make_fake_pty();
    let handoff = serde_json::json!({
        "listen_fd": listen_fd,
        "sessions": [{
            "id": "codex-history",
            "fd": master_fd,
            "pid": pid,
            "cols": 40,
            "rows": 3,
            "launch": "codex --dangerously-bypass-approvals-and-sandbox",
            "alt_screen": false,
            "grid": hex_encode(&grid),
        }]
    });
    std::fs::write(&p, handoff.to_string()).unwrap();

    let (_listener, sessions, _acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
    let sess = sessions.live("codex-history").expect("会话应存在");
    let term = sess.term.lock().unwrap();
    assert!(
        term.history_size() > 0,
        "handoff 后必须保留主屏 scrollback，history_size={}",
        term.history_size()
    );

    let mut all = String::new();
    let mut line = term.topmost_line();
    let bottom = term.bottommost_line();
    while line <= bottom {
        for col in 0..term.columns() {
            let c = term.grid()[line][Column(col)].c;
            if c != '\0' {
                all.push(c);
            }
        }
        all.push('\n');
        line += 1;
    }
    assert!(
        all.contains("handoff-codex-line-00"),
        "早期历史丢失: {all:?}"
    );
    assert!(
        all.contains("handoff-codex-line-09"),
        "最新输出丢失: {all:?}"
    );
}

/// fd 已失效的会话只跳过它自己，不能拖垮整次恢复——其余会话必须照常回来。
#[test]
fn skips_session_with_dead_fd_but_keeps_the_rest() {
    let p = tmp_handoff("dead-fd");
    let listen_fd = make_listen_fd("dead-fd");
    let (good_fd, _read_end, pid) = make_fake_pty();

    let handoff = serde_json::json!({
        "listen_fd": listen_fd,
        "sessions": [
            { "id": "dead", "fd": NEVER_VALID_FD, "pid": pid, "cols": 80, "rows": 24 },
            { "id": "good", "fd": good_fd, "pid": pid, "cols": 80, "rows": 24,
              "grid": hex_encode(b"ALIVE") },
        ]
    });
    std::fs::write(&p, handoff.to_string()).unwrap();

    let (_l, sessions, _acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
    assert!(sessions.live("dead").is_none(), "fd 失效的会话应被跳过");
    assert!(
        sessions.live("good").is_some(),
        "其余会话必须照常恢复，不能被坏的那个拖垮"
    );
}

#[test]
fn dead_duplicate_entry_cannot_kill_pid_claimed_by_valid_entry() {
    let p = tmp_handoff("dead-fd-shared-live-pid");
    let listen_fd = make_listen_fd("dead-fd-shared-live-pid");
    let (good_fd, slave, pid) = make_live_fake_pty();

    let handoff = serde_json::json!({
        "listen_fd": listen_fd,
        "sessions": [
            { "id": "dead", "fd": NEVER_VALID_FD, "pid": pid, "cols": 80, "rows": 24 },
            { "id": "good", "fd": good_fd, "pid": pid, "cols": 80, "rows": 24 },
        ]
    });
    std::fs::write(&p, handoff.to_string()).unwrap();

    let (_listener, sessions, _acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
    let session = sessions.live("good").expect("有效条目必须恢复");
    let stayed_alive = !session.child.wait_reaped(Duration::from_millis(100));

    // 先清理再断言，确保旧实现下的预期失败也不遗留 sleep 子进程。
    let cleanup_ok = session
        .child
        .terminate_and_wait(TERMINAL_CHILD_REAP_TIMEOUT);
    drop(slave);
    assert!(cleanup_ok, "测试子进程应由唯一 owner 回收");
    assert!(
        stayed_alive,
        "前面的坏 fd 条目不能杀死稍后由有效会话认领的同一个 PID"
    );
}

/// 无 grid、且交接前在备用屏：注 1049h 让 TUI 自己重画，
/// 而不是把它留在主屏上（那样 agent 的界面会叠在 shell 历史上）。
#[test]
fn without_grid_alt_screen_flag_enters_alt_mode() {
    let p = tmp_handoff("alt-no-grid");
    let listen_fd = make_listen_fd("alt-no-grid");
    let (master_fd, _read_end, pid) = make_fake_pty();

    let handoff = serde_json::json!({
        "listen_fd": listen_fd,
        "sessions": [{
            "id": "s1", "fd": master_fd, "pid": pid, "cols": 80, "rows": 24,
            "alt_screen": true,
        }]
    });
    std::fs::write(&p, handoff.to_string()).unwrap();

    let (_l, sessions, _acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
    let sess = sessions.live("s1").unwrap();
    let term = sess.term.lock().unwrap();
    assert!(
        term.mode().contains(TermMode::ALT_SCREEN),
        "交接前在备用屏、又没有 grid 时，应只注 1049h 把 Term 切回备用屏"
    );
}

/// 恢复的会话一律挂 jolt：有 grid 时用于对齐真实 cell 尺寸，无 grid 时逼进程自绘。
#[test]
fn restored_session_is_marked_for_jolt() {
    let p = tmp_handoff("jolt");
    let listen_fd = make_listen_fd("jolt");
    let (master_fd, _read_end, pid) = make_fake_pty();

    let handoff = serde_json::json!({
        "listen_fd": listen_fd,
        "sessions": [{
            "id": "s1", "fd": master_fd, "pid": pid, "cols": 80, "rows": 24,
            "grid": hex_encode(b"X"),
        }]
    });
    std::fs::write(&p, handoff.to_string()).unwrap();

    let (_l, sessions, _acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
    let sess = sessions.live("s1").unwrap();
    assert!(sess.ctl.lock().unwrap().jolt, "恢复的会话必须挂 jolt");
}

/// 造一对能被 resume_handoff 接管的假 stdin/stdout fd（管道即可，不需要
/// 真的能跑 JSON-RPC——resume_acp_from_fds 只是起个线程去读它，本测试不
/// 关心那条线程后续读到什么，只关心 resume_handoff 这一步的解析/建表
/// 逻辑对不对）。返回 (stdin_fd, stdout_fd, 两端读写口保管者, pid)。
fn make_fake_acp_stdio() -> (RawFd, RawFd, (std::fs::File, std::fs::File), i32) {
    let mut in_fds = [0i32; 2];
    let mut out_fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(in_fds.as_mut_ptr()) }, 0);
    assert_eq!(unsafe { libc::pipe(out_fds.as_mut_ptr()) }, 0);
    let child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id() as i32;
    drop(child); // 留成 zombie，交给 resume_acp_from_fds 内部的 KillProcessGroupOnDrop 收尾
    // 写端/读端各自保管一份，避免管道另一头因为「没人拿着」直接 EOF。
    let stdin_fd = in_fds[1]; // 交给 resume_handoff 接管（当作"守护写向 agent"）
    let stdout_fd = out_fds[0]; // 交给 resume_handoff 接管（当作"从 agent 读"）
    let keep_alive = unsafe {
        (
            std::fs::File::from_raw_fd(in_fds[0]),
            std::fs::File::from_raw_fd(out_fds[1]),
        )
    };
    (stdin_fd, stdout_fd, keep_alive, pid)
}

fn sample_snapshot(acp_session_id: &str) -> smelt_core::acp_session::ConversationSnapshot {
    let mut state = smelt_core::acp_session::AcpSessionState::placeholder(
        vec![smelt_core::acp_chat::AcpEntry::User("hi".into())],
        Some(acp_session_id.to_string()),
        String::new(),
    );
    state.acp_session_id = Some(acp_session_id.to_string());
    state.to_snapshot(false)
}

#[test]
fn handoff_only_recovers_a_running_turn_with_an_active_start_marker() {
    let mut snapshot = sample_snapshot("sid");
    snapshot.phase = smelt_core::daemon_state::DaemonPhase::Thinking;
    snapshot.turn_started_at_ms = None;
    assert!(!snapshot_has_active_turn(&snapshot));

    snapshot.turn_started_at_ms = Some(123);
    assert!(snapshot_has_active_turn(&snapshot));

    snapshot.phase = smelt_core::daemon_state::DaemonPhase::Idle;
    snapshot.turn_started_at_ms = None;
    snapshot
        .entries
        .push(smelt_core::acp_chat::AcpEntry::ToolCall {
            id: "tool-1".into(),
            title: "navigate".into(),
            kind: smelt_core::acp_chat::ToolKind::Fetch,
            status: smelt_core::acp_chat::ToolCallStatus::InProgress,
            output: Vec::new(),
            children: Vec::new(),
        });
    assert!(snapshot_has_active_turn(&snapshot));
}

#[test]
fn handoff_dangling_tool_is_not_recovered_as_an_active_turn() {
    let mut snapshot = sample_snapshot("sid");
    snapshot.phase = smelt_core::daemon_state::DaemonPhase::Idle;
    snapshot.turn_started_at_ms = None;
    snapshot.completed_unread = true;
    snapshot
        .entries
        .push(smelt_core::acp_chat::AcpEntry::ToolCall {
            id: "tool-1".into(),
            title: "Reading shell output".into(),
            kind: smelt_core::acp_chat::ToolKind::Execute,
            status: smelt_core::acp_chat::ToolCallStatus::InProgress,
            output: Vec::new(),
            children: Vec::new(),
        });
    assert!(snapshot_has_active_turn(&snapshot));

    let restored = smelt_core::acp_session::AcpSessionState::from_snapshot(snapshot);
    assert!(
        !smelt_core::acp_chat::has_unfinished_tool_call(&restored.entries),
        "已结束回合的悬空工具必须在恢复时收尾"
    );
    assert!(
        !state_has_active_turn(&restored),
        "收尾后的状态不能再让恢复路径保留 phantom in-flight RPC"
    );
}

#[test]
fn missing_handoff_conversation_binding_stays_unknown() {
    let item = serde_json::json!({
        "id": "acp-legacy-binding",
        "stdin_fd": 10,
        "stdout_fd": 11,
        "pid": 42,
        "snapshot": sample_snapshot("sid-legacy-binding"),
    });

    let AcpHandoffItemValidation::Restore(validated) = validate_acp_handoff_item(&item, |_| true)
    else {
        panic!("缺少 conversation_binding 的旧 handoff 仍应能恢复");
    };
    assert!(
        validated.conversation_binding.is_none(),
        "缺失字段必须保持未知，attach 才能采用客户端 Plugin"
    );
}

#[test]
fn missing_acp_snapshot_requires_owned_resource_cleanup() {
    let item = serde_json::json!({
        "id": "acp-missing-snapshot",
        "stdin_fd": 10,
        "stdout_fd": 11,
        "pid": 42,
    });

    let AcpHandoffItemValidation::CleanupRequired(owned) =
        validate_acp_handoff_item(&item, |_| true)
    else {
        panic!("missing snapshot must reject with transferred ownership");
    };
    assert_eq!(
        owned,
        OwnedAcpHandoff {
            pid: 42,
            stdin_fd: 10,
            stdout_fd: 11,
        }
    );
}

#[test]
fn malformed_acp_snapshot_requires_owned_resource_cleanup() {
    let item = serde_json::json!({
        "id": "acp-malformed-snapshot",
        "stdin_fd": 20,
        "stdout_fd": 21,
        "pid": 84,
        "snapshot": {"not": "an ACP snapshot"},
    });

    let AcpHandoffItemValidation::CleanupRequired(owned) =
        validate_acp_handoff_item(&item, |_| true)
    else {
        panic!("malformed snapshot must reject with transferred ownership");
    };
    assert_eq!(
        owned,
        OwnedAcpHandoff {
            pid: 84,
            stdin_fd: 20,
            stdout_fd: 21,
        }
    );
}

#[test]
fn handoff_launch_prefers_structured_spec_over_legacy_cmd() {
    let launch = launch_spec_from_handoff_item(&serde_json::json!({
        "launch": {
            "command": "codex",
            "env": { "FOO": "1" }
        },
        "cmd": "ignored"
    }));
    assert_eq!(launch.command, "codex");
    assert_eq!(launch.env.get("FOO").map(String::as_str), Some("1"));

    let legacy = launch_spec_from_handoff_item(&serde_json::json!({
        "cmd": "claude --print"
    }));
    assert_eq!(legacy.command, "claude --print");
    assert!(legacy.env.is_empty());
}

#[test]
fn hosted_handoff_accepts_an_active_snapshot_without_sdk_reconstruction() {
    let mut snapshot = sample_snapshot("sid-hosted");
    snapshot.phase = smelt_core::daemon_state::DaemonPhase::AwaitingApproval;
    snapshot.turn_started_at_ms = Some(42);
    let item = serde_json::json!({
        "runtime": "hosted",
        "id": "acp-hosted",
        "host_fd": 40,
        "host_pid": 400,
        "provider_pid": 401,
        "host_snapshot_revision": 9,
        "snapshot": snapshot,
        "cmd": "provider --acp",
        "agent_token": "token",
        "agent_mcp": true,
    });

    let HostedAcpHandoffItemValidation::Restore(validated) =
        validate_hosted_acp_handoff_item(&item, |_| true)
    else {
        panic!("独立宿主的活跃快照必须可直接接管");
    };
    assert!(
        validated.conversation_binding.is_none(),
        "旧 handoff 缺失 binding 必须保持未知，不能默认 Direct"
    );
    assert_eq!(validated.id, "acp-hosted");
    assert_eq!(validated.launch.command, "provider --acp");
    assert_eq!(validated.host_snapshot_revision, 9);
    assert_eq!(
        validated.owned,
        OwnedHostedAcpHandoff {
            host_pid: 400,
            host_fd: 40,
            provider_pid: Some(401),
        }
    );
}

#[test]
fn malformed_hosted_snapshot_keeps_cleanup_ownership() {
    let item = serde_json::json!({
        "runtime": "hosted",
        "id": "acp-hosted-bad",
        "host_fd": 50,
        "host_pid": 500,
        "provider_pid": 501,
        "snapshot": {"invalid": true},
    });

    let HostedAcpHandoffItemValidation::CleanupRequired(owned) =
        validate_hosted_acp_handoff_item(&item, |_| true)
    else {
        panic!("损坏的宿主镜像必须连同进程/fd 清理所有权一起拒绝");
    };
    assert_eq!(
        owned,
        OwnedHostedAcpHandoff {
            host_pid: 500,
            host_fd: 50,
            provider_pid: Some(501),
        }
    );
}

#[test]
fn pid_one_is_never_accepted_as_owned_cleanup_target() {
    let item = serde_json::json!({
        "id": "acp-pid-one",
        "stdin_fd": 30,
        "stdout_fd": 31,
        "pid": 1,
        "snapshot": sample_snapshot("sid-pid-one"),
    });

    let validation = validate_acp_handoff_item(&item, |_| true);
    let AcpHandoffItemValidation::CloseDescriptors {
        stdin_fd,
        stdout_fd,
    } = validation
    else {
        panic!("pid 1 must never be accepted as an owned cleanup target");
    };
    assert_eq!((stdin_fd, stdout_fd), (30, 31));
}

#[test]
fn rejected_acp_handoff_closes_owned_fds_and_reaps_agent() {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let p = tmp_handoff("acp-rejected-cleanup");
    let listen_fd = make_listen_fd("acp-rejected-cleanup");
    // 先 spawn 替身子进程、再创建 pipes，让本用例自己的子进程不可能持有
    // 本用例的 fd。不过 fd 表是进程级的（测试线程间共享），其他并行测试线程
    // spawn 的子进程（macOS posix_spawn 的 fork→exec 窗口会拷贝父进程当前
    // 全部 fd）仍可能在 fork 瞬间带走本用例 pipe 的副本——所以下面的断言
    // 不能依赖"pipe 对端感知"（EPIPE/EOF），详见断言处的注释。cat 以 piped
    // stdin 阻塞存活，充当被拒绝的 ACP agent；即便它提前退出也只是变 zombie，
    // 由 cleanup 的 waitpid 收尸。
    let child = std::process::Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = child.id() as i32;

    let mut stdin_pipe = [0; 2];
    let mut stdout_pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(stdin_pipe.as_mut_ptr()) }, 0);
    assert_eq!(unsafe { libc::pipe(stdout_pipe.as_mut_ptr()) }, 0);
    let stdin_read = unsafe { std::fs::File::from_raw_fd(stdin_pipe[0]) };
    let stdin_write = unsafe { std::fs::File::from_raw_fd(stdin_pipe[1]) };
    let stdout_read = unsafe { std::fs::File::from_raw_fd(stdout_pipe[0]) };
    let stdout_write = unsafe { std::fs::File::from_raw_fd(stdout_pipe[1]) };
    let inherited_stdin_fd = unsafe { libc::dup(stdin_write.as_raw_fd()) };
    let inherited_stdout_fd = unsafe { libc::dup(stdout_read.as_raw_fd()) };
    assert!(inherited_stdin_fd >= 0 && inherited_stdout_fd >= 0);
    // 记录两个 pipe 各一端的 dev+ino，供断言比对：dup 副本与原始 fd 指向
    // 同一个 open file description，fstat 返回相同的 dev+ino；fd 号被并行用例
    // 复用后 fstat 会返回新对象的 dev+ino（不可能恰好相等）。
    let mut sd_st: libc::stat = unsafe { std::mem::zeroed() };
    let mut rd_st: libc::stat = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::fstat(stdin_pipe[1], &mut sd_st) }, 0);
    assert_eq!(unsafe { libc::fstat(stdout_pipe[0], &mut rd_st) }, 0);
    let (sd_dev, sd_ino) = (sd_st.st_dev, sd_st.st_ino);
    let (rd_dev, rd_ino) = (rd_st.st_dev, rd_st.st_ino);
    // 原始端提前释放，只留 dup 副本交给 handoff 由 cleanup 关闭。
    drop(stdin_write);
    drop(stdout_read);

    let handoff = serde_json::json!({
        "listen_fd": listen_fd,
        "sessions": [],
        "acp_sessions": [{
            "id": "acp-rejected",
            "stdin_fd": inherited_stdin_fd,
            "stdout_fd": inherited_stdout_fd,
            "pid": pid,
        }],
    });
    std::fs::write(&p, handoff.to_string()).unwrap();

    let (_listener, _sessions, acp) =
        resume_handoff(&p, &no_subs()).expect("handoff should remain globally valid");
    assert!(acp.get("acp-rejected").is_none());
    // cleanup 必须关闭两个 dup 副本。断言方式：fstat(inherited_fd) 的
    // dev+ino 不再等于原 pipe 的 dev+ino。cleanup 若漏关，fd 号不会释放，
    // fstat 必然仍匹配；fd 号一旦释放（无论是否被并行用例复用），fstat
    // 要么 EBADF、要么返回新对象，dev+ino 不可能恰好等于原 pipe。因此该
    // 断言与外部进程完全无关，是确定性的。不能用 write EPIPE / read EOF
    // 断言"对端感知"：其他并行测试线程 spawn 的子进程（fd 表进程级共享，
    // macOS posix_spawn 的 fork→exec 窗口会拷贝父进程全部 fd）可能短暂握着
    // pipe 对端副本，让 EPIPE/EOF 推迟到那个子进程退出才出现——这正是此前
    // 该用例在并行负载下偶发失败的根因。
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstat(inherited_stdout_fd, &mut st) };
    assert!(
        rc != 0 || st.st_dev != rd_dev || st.st_ino != rd_ino,
        "cleanup must close the inherited stdout fd (fstat rc={rc} dev={:#x} ino={:#x})",
        if rc == 0 { st.st_dev } else { 0 },
        if rc == 0 { st.st_ino } else { 0 }
    );
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstat(inherited_stdin_fd, &mut st) };
    assert!(
        rc != 0 || st.st_dev != sd_dev || st.st_ino != sd_ino,
        "cleanup must close the inherited stdin fd (fstat rc={rc} dev={:#x} ino={:#x})",
        if rc == 0 { st.st_dev } else { 0 },
        if rc == 0 { st.st_ino } else { 0 }
    );
    assert_eq!(
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
        -1,
        "cleanup must reap the rejected ACP child"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD),
        "waitpid must fail specifically because no unreaped child remains"
    );

    drop((child, stdin_read, stdout_write));
}

#[test]
fn acp_session_with_valid_fds_and_session_id_is_recovered() {
    let p = tmp_handoff("acp-ok");
    let listen_fd = make_listen_fd("acp-ok");
    let (stdin_fd, stdout_fd, _keep_alive, pid) = make_fake_acp_stdio();

    let handoff = serde_json::json!({
        "listen_fd": listen_fd,
        "sessions": [],
        "acp_sessions": [{
            "id": "acp-1",
            "stdin_fd": stdin_fd,
            "stdout_fd": stdout_fd,
            "pid": pid,
            "cwd": "/tmp/proj",
            "cmd": "claude --dangerously-skip-permissions",
            "agent_needs_transcript_check": true,
            "snapshot": sample_snapshot("sid-1"),
            "pending_raw_line": null,
        }],
    });
    std::fs::write(&p, handoff.to_string()).unwrap();

    let (_l, _sessions, acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
    let slot = acp.get("acp-1").expect("acp-1 应被恢复");
    let sess = &slot.value;
    assert_eq!(sess.cwd.as_deref(), Some("/tmp/proj"));
    assert!(sess.agent_needs_transcript_check);
    assert!(
        sess.handle.lock().unwrap().is_some(),
        "应该已经起了 resume 连接"
    );
    let reduced = sess.reduced.lock().unwrap();
    assert_eq!(reduced.entries.len(), 1);
    assert_eq!(reduced.acp_session_id.as_deref(), Some("sid-1"));
    assert_eq!(reduced.history_session_id.as_deref(), Some("sid-1"));
}

/// 没有 agent 侧 session id 就没法 attach_session——理论上不该发生，但
/// 交接文件是外部数据，得防御性地跳过而不是 panic 或者留一条永远连不上
/// 的死会话。
#[test]
fn acp_session_without_session_id_is_skipped() {
    let p = tmp_handoff("acp-no-sid");
    let listen_fd = make_listen_fd("acp-no-sid");
    let (stdin_fd, stdout_fd, _keep_alive, pid) = make_fake_acp_stdio();

    let mut snapshot = sample_snapshot("whatever");
    snapshot.acp_session_id = None;
    let handoff = serde_json::json!({
        "listen_fd": listen_fd,
        "sessions": [],
        "acp_sessions": [{
            "id": "acp-2",
            "stdin_fd": stdin_fd,
            "stdout_fd": stdout_fd,
            "pid": pid,
            "cwd": null,
            "cmd": "claude",
            "agent_needs_transcript_check": true,
            "snapshot": snapshot,
            "pending_raw_line": null,
        }],
    });
    std::fs::write(&p, handoff.to_string()).unwrap();

    let (_l, _sessions, acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
    assert!(acp.get("acp-2").is_none());
}

/// fd 号本身失效（exec 前忘了清 CLOEXEC 之类）：必须跳过，不能把野 fd
/// 当成活的 stdin/stdout 去用。
#[test]
fn acp_session_with_invalid_fd_is_skipped() {
    let p = tmp_handoff("acp-bad-fd");
    let listen_fd = make_listen_fd("acp-bad-fd");

    let handoff = serde_json::json!({
        "listen_fd": listen_fd,
        "sessions": [],
        "acp_sessions": [{
            "id": "acp-3",
            "stdin_fd": NEVER_VALID_FD,
            "stdout_fd": NEVER_VALID_FD,
            "pid": 1,
            "cwd": null,
            "cmd": "claude",
            "agent_needs_transcript_check": true,
            "snapshot": sample_snapshot("sid-3"),
            "pending_raw_line": null,
        }],
    });
    std::fs::write(&p, handoff.to_string()).unwrap();

    let (_l, _sessions, acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
    assert!(acp.get("acp-3").is_none());
}

/// 正卡着一张审批卡片时，pending_raw_line 应该原样透传进 resume_handoff
/// （具体会不会被正确回放是 acp_conn::resume_acp_from_fds 的职责，这里
/// 只验证 resume_handoff 这一层没有把它弄丢/挡在门外）。
#[test]
fn acp_session_with_pending_raw_line_still_recovers() {
    let p = tmp_handoff("acp-pending");
    let listen_fd = make_listen_fd("acp-pending");
    let (stdin_fd, stdout_fd, _keep_alive, pid) = make_fake_acp_stdio();

    let handoff = serde_json::json!({
        "listen_fd": listen_fd,
        "sessions": [],
        "acp_sessions": [{
            "id": "acp-4",
            "stdin_fd": stdin_fd,
            "stdout_fd": stdout_fd,
            "pid": pid,
            "cwd": null,
            "cmd": "claude",
            "agent_needs_transcript_check": true,
            "snapshot": sample_snapshot("sid-4"),
            "pending_raw_line": r#"{"jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{}}"#,
        }],
    });
    std::fs::write(&p, handoff.to_string()).unwrap();

    let (_l, _sessions, acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
    assert!(acp.get("acp-4").is_some());
}

/// 遗留 ACP runtime 的「搬进独立宿主」决策：只允许在完全静默的边界上动手。
///
/// 这段此前写成 `!cfg!(test) && ...`，测试构建里恒为 false——分支按构造不可达，
/// 六个条件一条都没被覆盖过。改成纯谓词 + 调用方注入后才有了这些用例。
#[test]
fn legacy_rehome_only_happens_on_a_quiet_boundary() {
    let mut quiet = sample_snapshot("sid");
    quiet.phase = smelt_core::daemon_state::DaemonPhase::Idle;
    quiet.turn_started_at_ms = None;
    // 基线：静默 + 记得住重启命令 → 可以迁移。
    assert!(legacy_rehome_is_safe(&quiet, None, "claude"));

    // 1. 相位不是 Idle：回合还在跑，杀连接会把用户正在等的输出丢掉。
    let mut thinking = quiet.clone();
    thinking.phase = smelt_core::daemon_state::DaemonPhase::Thinking;
    assert!(!legacy_rehome_is_safe(&thinking, None, "claude"));

    // 2. 相位已 Idle 但回合起始标记还在：归约尚未收尾，不能当静默。
    let mut marked = quiet.clone();
    marked.turn_started_at_ms = Some(1);
    assert!(!legacy_rehome_is_safe(&marked, None, "claude"));

    // 3. 卡着一张审批卡片：迁移会让用户点过的审批凭空消失。
    let mut approving = quiet.clone();
    approving
        .pending_permissions
        .push(smelt_core::acp_session::PendingPermission {
            question: "允许写文件？".into(),
            tool_call_id: "tool-1".into(),
            options: Vec::new(),
            details: smelt_core::acp_session::ApprovalDetailsView::default(),
        });
    assert!(!legacy_rehome_is_safe(&approving, None, "claude"));

    // 4. 卡着一次追问：同上，等着用户回话的状态不能丢。
    let mut asking = quiet.clone();
    asking.pending_elicitation = Some(smelt_core::acp_session::PendingElicitation {
        message: "选一个分支".into(),
        fields: Vec::new(),
        chosen: std::collections::BTreeMap::new(),
        text_values: std::collections::BTreeMap::new(),
    });
    assert!(!legacy_rehome_is_safe(&asking, None, "claude"));

    // 5. 还有半行没解析完的 provider 输出：重连后这半行拼不回去。
    assert!(!legacy_rehome_is_safe(
        &quiet,
        Some("{\"jsonrpc\":"),
        "claude"
    ));

    // 6. 重启命令为空/全空白：迁移后起不来，宁可维持原样等下一轮。
    assert!(!legacy_rehome_is_safe(&quiet, None, ""));
    assert!(!legacy_rehome_is_safe(&quiet, None, "   \t "));
}

/// 身份门：快照前生的原进程放行，快照后生的（复用号）与死号一律拦下。
#[test]
fn handoff_kill_gate_checks_birth_against_snapshot() {
    use std::os::unix::process::CommandExt;

    let mut old = std::process::Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let old_pid = old.id() as i32;
    std::thread::sleep(std::time::Duration::from_millis(50));
    let wall = std::time::SystemTime::now();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let mut new = std::process::Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let new_pid = new.id() as i32;

    assert!(
        handoff_pid_predates_snapshot(old_pid, wall),
        "快照前生的必须放行"
    );
    assert!(
        !handoff_pid_predates_snapshot(new_pid, wall),
        "快照后生的（复用号）必须拦下"
    );
    assert!(!handoff_pid_predates_snapshot(i32::MAX, wall));
    assert!(!handoff_pid_predates_snapshot(0, wall));
    assert!(!handoff_pid_predates_snapshot(-3, wall));

    unsafe {
        libc::kill(-old_pid, libc::SIGKILL);
        libc::kill(-new_pid, libc::SIGKILL);
    }
    let _ = old.wait();
    let _ = new.wait();
}

/// 清理服从门：复用号只关 fd 不杀人；原进程照杀。
#[test]
fn cleanup_kills_original_but_spares_reused_pid() {
    use std::os::unix::process::CommandExt;

    fn null_fd() -> std::os::fd::RawFd {
        unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) }
    }
    fn alive(pid: i32) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }

    let mut original = std::process::Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let original_pid = original.id() as i32;
    std::thread::sleep(std::time::Duration::from_millis(50));
    let wall = std::time::SystemTime::now();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let mut reused = std::process::Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let reused_pid = reused.id() as i32;

    // 复用号：fd 照关（传 /dev/null 占位），进程必须活着。
    cleanup_rejected_acp_handoff(
        OwnedAcpHandoff {
            pid: reused_pid,
            stdin_fd: null_fd(),
            stdout_fd: null_fd(),
        },
        wall,
    );
    assert!(alive(reused_pid), "复用号进程必须活着");

    // 原进程：照杀（亲生，waitpid 收尸，确定性）。
    cleanup_rejected_acp_handoff(
        OwnedAcpHandoff {
            pid: original_pid,
            stdin_fd: null_fd(),
            stdout_fd: null_fd(),
        },
        wall,
    );
    assert!(!alive(original_pid), "原进程必须被杀掉");

    // 收尾：复用号那个 sleep 自己杀掉。
    unsafe {
        libc::kill(-reused_pid, libc::SIGKILL);
    }
    let _ = original.try_wait();
    let _ = reused.wait();
}
