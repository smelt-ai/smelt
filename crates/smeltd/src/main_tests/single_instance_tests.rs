use super::*;

fn test_socket(name: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("smeltd-{name}-{}-{nonce}.sock", std::process::id()))
}

#[test]
fn concurrent_starters_leave_exactly_one_listener() {
    const STARTERS: usize = 8;
    let path = test_socket("single-instance");
    let barrier = Arc::new(std::sync::Barrier::new(STARTERS));
    let mut starters = Vec::new();

    for _ in 0..STARTERS {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        starters.push(thread::spawn(move || {
            let listener = bind_single_instance(&path, true).expect("并发启动不应 bind 失败");
            barrier.wait();
            listener.is_some()
        }));
    }

    let listeners = starters
        .into_iter()
        .map(|starter| starter.join().unwrap())
        .filter(|has_listener| *has_listener)
        .count();
    assert_eq!(listeners, 1, "并发启动只能有一个进程取得 listener");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("lock"));
}

#[test]
fn stale_socket_path_is_replaced_while_holding_startup_lock() {
    let path = test_socket("stale");
    std::fs::write(&path, b"stale").unwrap();

    let listener = bind_single_instance(&path, true)
        .expect("僵尸 socket 应可恢复")
        .expect("没有活实例时应取得 listener");
    assert!(UnixStream::connect(&path).is_ok());

    drop(listener);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("lock"));
}

#[test]
fn existing_daemon_keeps_live_handoff_file() {
    let path = test_socket("live-handoff");
    let handoff = path.with_extension("handoff");
    let listener = UnixListener::bind(&path).unwrap();
    std::fs::write(&handoff, b"live upgrade snapshot").unwrap();

    let result = bind_fresh_daemon(&path, &handoff, true).unwrap();

    assert!(result.is_none(), "已有 daemon 时竞争启动者必须退出");
    assert!(
        handoff.is_file(),
        "竞争启动者不能删除已有 daemon 正在使用的 handoff"
    );
    drop(listener);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&handoff);
    let _ = std::fs::remove_file(path.with_extension("lock"));
}

#[test]
fn fresh_daemon_removes_stale_handoff_file() {
    let path = test_socket("stale-handoff");
    let handoff = path.with_extension("handoff");
    std::fs::write(&handoff, b"stale").unwrap();

    let listener = bind_fresh_daemon(&path, &handoff, true)
        .unwrap()
        .expect("没有已有 daemon 时应取得 listener");

    assert!(!handoff.exists(), "新 daemon 应清理陈旧 handoff");
    drop(listener);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("lock"));
}

#[test]
fn handoff_boundary_reaps_exited_children_without_touching_live_children() {
    const HELPER_ENV: &str = "SMELTD_REAP_BOUNDARY_TEST_HELPER";
    if std::env::var_os(HELPER_ENV).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(
                "main_tests::single_instance_tests::handoff_boundary_reaps_exited_children_without_touching_live_children",
            )
            .env(HELPER_ENV, "1")
            .status()
            .unwrap();
        assert!(status.success(), "隔离的 exec 边界回收验证失败");
        return;
    }

    // 只在隔离出来的测试进程里调用 fork：子进程立刻 _exit，不接触 Rust 锁。
    // waitid + WNOWAIT 等它成为确定的僵尸，但不抢走待验证的退出状态。
    let exited_pid = unsafe { libc::fork() };
    assert!(exited_pid >= 0, "fork 失败");
    if exited_pid == 0 {
        unsafe { libc::_exit(0) };
    }
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    assert_eq!(
        unsafe {
            libc::waitid(
                libc::P_PID,
                exited_pid as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT,
            )
        },
        0,
        "应先造出一个不被测试本身回收的僵尸"
    );

    let mut live = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let reaped_count = reap_inherited_exited_children_at_exec_boundary();
    let exited_is_reaped = unsafe {
        libc::waitpid(exited_pid, std::ptr::null_mut(), libc::WNOHANG) < 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD)
    };
    let live_is_running = live.try_wait().unwrap().is_none();

    if !exited_is_reaped {
        unsafe {
            libc::waitpid(exited_pid, std::ptr::null_mut(), 0);
        }
    }
    let _ = live.kill();
    let _ = live.wait();

    assert_eq!(reaped_count, 1, "边界清理应只收掉已经退出的直接子进程");
    assert!(exited_is_reaped, "历史僵尸必须在新映像启动边界被清掉");
    assert!(live_is_running, "仍存活的会话子进程不能被边界清理误伤");
}
