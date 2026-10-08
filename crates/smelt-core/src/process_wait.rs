//! 等一个子进程结束。
//!
//! 截止时间只表示「到这时还没结束就改发信号」。进程何时退出由 `wait` / `waitpid`
//! 告诉调用方。中间不再隔几十毫秒问一次「死了没有」——那个空档里状态已经变了，
//! 问的人还在睡。
//!
//! 返回时，要么这个 pid 已经被收回，要么等的那条线程仍是它唯一的 `waitpid`。
//! 调用方不能再开第二个 `waitpid`。

use std::io::{self, Read};
use std::process::{Child, ExitStatus};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

/// 调用方已经发过终止信号。阻塞到 `waitpid` 收回这个直属子进程。
///
/// `ECHILD` 表示已经被收过。`EINTR` 重试。其它错误直接返回，不再空转。
pub fn reap_direct_child(pid: i32) -> bool {
    if pid <= 1 {
        return true;
    }
    loop {
        let mut status = 0;
        let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
        if waited == pid {
            return true;
        }
        match io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::ECHILD) => return true,
            _ => return false,
        }
    }
}

/// 等直属子进程退出。
///
/// 等的线程先进入 `waitpid`，然后才调用 `on_armed`（一般是 SIGTERM）。这样
/// 进程如果马上退出，会停在僵尸上，直到同一个 `waitpid` 把它收回。
/// `grace` 用尽后执行 `on_timeout`（一般是 SIGKILL），再给 `reap_grace`。
/// 两次都是截止，中间不轮询。第二次仍未返回时函数返回 `false`，等的那条线程
/// 继续停在这个 pid 的 `waitpid` 上。调用方不能再开第二个。
pub fn wait_for_pid(
    pid: i32,
    grace: Duration,
    on_armed: impl FnOnce(),
    on_timeout: impl FnOnce() + Send + 'static,
    reap_grace: Duration,
) -> bool {
    if pid <= 1 {
        return true;
    }
    let (tx, rx) = mpsc::channel();
    let started = thread::Builder::new()
        .name("smelt-waitpid".to_string())
        .spawn(move || {
            let _ = tx.send(reap_direct_child(pid));
        });
    if started.is_err() {
        on_timeout();
        return reap_direct_child(pid);
    }
    on_armed();
    match rx.recv_timeout(grace) {
        Ok(reaped) => reaped,
        Err(RecvTimeoutError::Disconnected) => false,
        Err(RecvTimeoutError::Timeout) => {
            on_timeout();
            rx.recv_timeout(reap_grace).unwrap_or_default()
        }
    }
}

/// 等到 `child` 退出并拿到状态。
///
/// 到 `grace` 还没退出就调用 `on_timeout`，然后一直等到同一个 `wait` 返回。
/// 函数返回时没有线程还停在这个子进程的 `wait` 上。
pub fn wait_child_for(
    child: Child,
    grace: Duration,
    on_timeout: impl FnOnce(u32) + Send + 'static,
) -> io::Result<ExitStatus> {
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    let started = thread::Builder::new()
        .name("smelt-child-wait".to_string())
        .spawn(move || {
            let mut child = child;
            let _ = tx.send(child.wait());
        });
    if started.is_err() {
        return Err(io::Error::other("无法启动进程等待线程"));
    }
    match rx.recv_timeout(grace) {
        Ok(result) => result,
        Err(RecvTimeoutError::Disconnected) => Err(io::Error::other("进程等待线程在收尸前退出")),
        Err(RecvTimeoutError::Timeout) => {
            on_timeout(pid);
            rx.recv()
                .map_err(|_| io::Error::other("进程等待线程在收尸前退出"))?
        }
    }
}

/// 边读 stdout 边等进程退出。
///
/// 管道容量有限，没人读的话子进程会卡在写上，`wait` 也永远看不到它退出。
/// 超时则 SIGKILL，等同一个 `wait` 返回，并且不把读到一半的输出交出去。
/// `Ok(None)` 表示到截止还没自己结束，已经被杀掉。进程自己退出时，哪怕退出码
/// 不是 0，也把已经读完的 stdout 交回来——`lsof` 找不到监听者时就是这样退出的。
pub fn wait_child_stdout(mut child: Child, grace: Duration) -> io::Result<Option<Vec<u8>>> {
    let mut stdout = child.stdout.take();
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    let started = thread::Builder::new()
        .name("smelt-child-output".to_string())
        .spawn(move || {
            let mut buf = Vec::new();
            if let Some(out) = stdout.as_mut() {
                let _ = out.read_to_end(&mut buf);
            }
            let mut child = child;
            let status = child.wait();
            let _ = tx.send((status, buf));
        });
    if started.is_err() {
        return Err(io::Error::other("无法启动进程输出线程"));
    }
    match rx.recv_timeout(grace) {
        Ok((_status, buf)) => Ok(Some(buf)),
        Err(RecvTimeoutError::Disconnected) => Err(io::Error::other("进程输出线程在收尸前退出")),
        Err(RecvTimeoutError::Timeout) => {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
            let _ = rx.recv();
            Ok(None)
        }
    }
}

/// 等到 `pid` 退出，或者到了 `timeout`。
///
/// 不收尸。调用方要收回自己的子进程，得再 `waitpid`。已经不在的 pid 立刻返回
/// `true`。没有「进程退出」事件的平台，只把这段截止时间一次睡完再查，不再隔
/// 几毫秒问一次。
pub fn wait_until_exit(pid: i32, timeout: Duration) -> bool {
    if pid <= 1 {
        return true;
    }
    wait_until_any_exit(&[pid], timeout).is_some()
}

/// 等到 `pids` 里任意一个退出。返回退出的那个 pid。
///
/// 不收尸。空列表立刻返回 `None`，调用方自己决定还要不要把剩余截止时间一次
/// 睡完。已经死掉的 pid 在注册时就会返回，不会空转。
pub fn wait_until_any_exit(pids: &[i32], timeout: Duration) -> Option<i32> {
    if let Some(pid) = pids.iter().copied().find(|pid| *pid <= 1) {
        return Some(pid);
    }
    let live: Vec<i32> = pids.iter().copied().filter(|pid| *pid > 1).collect();
    if live.is_empty() {
        return None;
    }
    wait_until_any_exit_os(&live, timeout)
}

fn pid_is_gone(pid: i32) -> bool {
    let sent = unsafe { libc::kill(pid, 0) };
    sent != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(target_os = "macos")]
fn wait_until_any_exit_os(pids: &[i32], timeout: Duration) -> Option<i32> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let kq = unsafe { libc::kqueue() };
    if kq < 0 {
        return wait_by_parking(pids, timeout);
    }
    let _kq = unsafe { std::os::fd::OwnedFd::from_raw_fd(kq) };
    let mut changes = Vec::with_capacity(pids.len());
    for &pid in pids {
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        event.ident = pid as usize;
        event.filter = libc::EVFILT_PROC;
        event.flags = libc::EV_ADD | libc::EV_ONESHOT;
        event.fflags = libc::NOTE_EXIT;
        event.udata = pid as *mut libc::c_void;
        changes.push(event);
    }
    let mut events = vec![unsafe { std::mem::zeroed::<libc::kevent>() }; changes.len().max(1)];
    let deadline = std::time::Instant::now() + timeout;
    let mut registering = true;
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return None;
        }
        let remaining = deadline - now;
        let timespec = libc::timespec {
            tv_sec: remaining.as_secs() as libc::time_t,
            tv_nsec: remaining.subsec_nanos() as libc::c_long,
        };
        let fired = unsafe {
            libc::kevent(
                _kq.as_raw_fd(),
                if registering {
                    changes.as_ptr()
                } else {
                    std::ptr::null()
                },
                if registering {
                    changes.len() as libc::c_int
                } else {
                    0
                },
                events.as_mut_ptr(),
                events.len() as libc::c_int,
                &timespec,
            )
        };
        registering = false;
        if fired < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                registering = true;
                continue;
            }
            return wait_by_parking(
                pids,
                deadline.saturating_duration_since(std::time::Instant::now()),
            );
        }
        if fired == 0 {
            return None;
        }
        for event in events.iter().take(fired as usize) {
            let pid = event.udata as i32;
            if event.flags & libc::EV_ERROR != 0 {
                if event.data == libc::ESRCH as isize {
                    return Some(pid);
                }
                continue;
            }
            return Some(pid);
        }
    }
}

#[cfg(target_os = "linux")]
fn wait_until_any_exit_os(pids: &[i32], timeout: Duration) -> Option<i32> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    let mut watched: Vec<(i32, OwnedFd)> = Vec::new();
    for &pid in pids {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as libc::c_int };
        if fd < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return Some(pid);
            }
            continue;
        }
        watched.push((pid, unsafe { OwnedFd::from_raw_fd(fd) }));
    }
    if watched.is_empty() {
        return wait_by_parking(pids, timeout);
    }
    let mut fds: Vec<libc::pollfd> = watched
        .iter()
        .map(|(_, fd)| libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return None;
        }
        let millis = (deadline - now).as_millis().min(i32::MAX as u128) as libc::c_int;
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
        if ready < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return wait_by_parking(
                pids,
                deadline.saturating_duration_since(std::time::Instant::now()),
            );
        }
        if ready == 0 {
            return None;
        }
        for (index, (pid, _)) in watched.iter().enumerate() {
            if fds[index].revents != 0 {
                return Some(*pid);
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn wait_until_any_exit_os(pids: &[i32], timeout: Duration) -> Option<i32> {
    wait_by_parking(pids, timeout)
}

/// 没有进程退出事件时，把剩余截止时间一次睡完再查。中途被叫醒就再睡剩下的，
/// 不会改成几毫秒一轮。
fn wait_by_parking(pids: &[i32], timeout: Duration) -> Option<i32> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(pid) = pids.iter().copied().find(|pid| pid_is_gone(*pid)) {
            return Some(pid);
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return None;
        }
        thread::park_timeout(deadline - now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    #[test]
    fn child_that_exits_is_collected_before_the_deadline() {
        let child = Command::new("sh")
            .args(["-c", "sleep 0.2"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let started = Instant::now();
        let status = wait_child_for(child, Duration::from_secs(5), |_| {
            panic!("子进程会自己退出，不该杀掉");
        })
        .expect("wait");
        assert!(status.success());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "退出后应马上被 wait 收回，实际 {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn deadline_kills_and_the_same_wait_reaps() {
        let child = Command::new("sh")
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let pid = child.id() as i32;
        let started = Instant::now();
        let status = wait_child_for(child, Duration::from_millis(200), |pid| unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        })
        .expect("wait");
        assert!(!status.success());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "到截止就该杀掉并收回，实际 {:?}",
            started.elapsed()
        );
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    #[test]
    fn stdout_is_read_while_the_child_is_still_writing() {
        let child = Command::new("sh")
            .args(["-c", "dd if=/dev/zero bs=1024 count=256 2>/dev/null"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let output = wait_child_stdout(child, Duration::from_secs(5))
            .expect("wait")
            .expect("子进程应在截止前自己结束");
        assert_eq!(output.len(), 256 * 1024);
    }

    #[test]
    fn stdout_is_kept_when_the_child_exits_nonzero() {
        let child = Command::new("sh")
            .args(["-c", "printf 'pid-9\\n'; exit 1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let output = wait_child_stdout(child, Duration::from_secs(5))
            .expect("wait")
            .expect("非零退出也要交出已经读完的 stdout");
        assert_eq!(output, b"pid-9\n");
    }

    #[test]
    fn exit_is_observed_before_the_deadline_without_reaping() {
        let child = Command::new("sh")
            .args(["-c", "sleep 0.2"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let pid = child.id() as i32;
        let started = Instant::now();
        assert!(
            wait_until_exit(pid, Duration::from_secs(5)),
            "进程退出后应马上被事件等到"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "不应干等到截止，实际 {:?}",
            started.elapsed()
        );
        let _ = child.wait_with_output();
    }

    #[test]
    fn still_running_pid_waits_out_the_deadline() {
        let child = Command::new("sh")
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let pid = child.id() as i32;
        let started = Instant::now();
        assert!(!wait_until_exit(pid, Duration::from_millis(200)));
        assert!(started.elapsed() < Duration::from_secs(2));
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        let _ = child.wait_with_output();
    }
}
