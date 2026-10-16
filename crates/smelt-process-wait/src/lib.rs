//! 等到一组 pid 里任意一个退出。
//!
//! 不收尸。空列表立刻返回 `None`。已经死掉的 pid 在注册时就会返回，不会空转。
//! 没有进程退出事件时，把剩余截止时间一次睡完再查，不再隔几毫秒问一次。

use std::io;
use std::thread;
use std::time::Duration;

/// 等到 `pids` 里任意一个退出。返回退出的那个 pid。
///
/// `pid <= 1` 立刻返回，避免对 init 注册退出事件。
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
