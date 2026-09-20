//! 收养子进程的退出监控：fork-exec 交接后，被交接的子进程重定父到
//! launchd，新守护不再是父进程、不能 waitpid。用能观察非亲生进程的原语
//! 精确感知退出：kqueue `EVFILT_PROC`（macOS）/ pidfd（Linux）/
//! kill 轮询（其它）。
//!
//! 为什么不用 PTY EOF 代替：EOF 只代表 slave 全关，后台后代持有 PTY 时
//! shell 早退会留僵尸（见 `TerminalChild` 的注释）。监控器给出精确的退出
//! 事件，僵尸由 init 回收（孤儿进程退出时 launchd 自动 reap）。
//!
//! 窗口封闭性：successor 在 predecessor 仍存活（仍是父进程、waiter 被
//! handoff 锁挡住无法 reap）时建监控，PID 在 EV_ADD 前不可能被复用；
//! 交接窗口内退出的由 predecessor reap 后写进 manifest（ExitedDuringHandoff），
//! 根本不进监控。ESRCH 分支只是防御性兜底。
//!
//! 退出码零损失：`TerminalChildExit` 本来就不存 code，只有"已回收与否"。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex};
#[cfg(test)]
use std::time::Duration;

/// 单个被监控 pid 的退出状态。`TerminalChild::Adopted` 持有它。
pub struct MonitoredState {
    exited: Mutex<bool>,
    changed: Condvar,
}

impl MonitoredState {
    fn new(exited: bool) -> Self {
        Self {
            exited: Mutex::new(exited),
            changed: Condvar::new(),
        }
    }

    fn mark_exited(&self) {
        let mut exited = self.exited.lock().unwrap_or_else(|e| e.into_inner());
        if !*exited {
            *exited = true;
            self.changed.notify_all();
        }
    }

    pub fn is_exited(&self) -> bool {
        *self.exited.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn wait(&self) {
        let guard = self
            .changed
            .wait_while(
                self.exited.lock().unwrap_or_else(|e| e.into_inner()),
                |exited| !*exited,
            )
            .unwrap_or_else(|e| e.into_inner());
        drop(guard);
    }

    /// true = 已退出，false = 超时仍活着。生产侧用阻塞 [`MonitoredState::wait`]
    /// （等待线程模型与 `TerminalChild::start` 镜像）；超时语义只给单测用。
    #[cfg(test)]
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let (guard, _) = self
            .changed
            .wait_timeout_while(
                self.exited.lock().unwrap_or_else(|e| e.into_inner()),
                timeout,
                |exited| !*exited,
            )
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }
}

struct MonitorInner {
    states: Mutex<HashMap<i32, Arc<MonitoredState>>>,
}

impl MonitorInner {
    fn mark_exited(&self, pid: i32) {
        if let Some(state) = self
            .states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&pid)
        {
            state.mark_exited();
        }
    }
}

/// 收养监控器：创建时一次性给出全部 pid（恢复时一次建好，无需动态增删），
/// 后台线程逐个确认退出。全部确认后线程自行结束。
pub struct ChildMonitor {
    inner: Arc<MonitorInner>,
}

impl ChildMonitor {
    pub fn spawn(pids: &[i32]) -> Arc<Self> {
        let mut states = HashMap::new();
        let mut live: HashSet<i32> = HashSet::new();
        for &pid in pids {
            if pid <= 1 {
                // 非法 pid 永不监控，直接记已退出（与 TerminalChild::restore 一致）。
                states.insert(pid, Arc::new(MonitoredState::new(true)));
            } else if !live.contains(&pid) {
                live.insert(pid);
                states.insert(pid, Arc::new(MonitoredState::new(false)));
            }
        }
        let monitor = Arc::new(Self {
            inner: Arc::new(MonitorInner {
                states: Mutex::new(states),
            }),
        });
        if !live.is_empty() {
            backend::spawn_watcher(live.into_iter().collect(), Arc::clone(&monitor.inner));
        }
        monitor
    }

    /// 取 pid 的等待句柄。pid 不在监控集合里 → None（调用方按已退出处理）。
    pub fn waiter(&self, pid: i32) -> Option<Arc<MonitoredState>> {
        self.inner
            .states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&pid)
            .cloned()
    }
}

#[cfg(target_os = "macos")]
mod backend {
    use super::MonitorInner;
    use std::collections::HashSet;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::sync::Arc;

    pub fn spawn_watcher(pids: Vec<i32>, inner: Arc<MonitorInner>) {
        std::thread::Builder::new()
            .name("smeltd-adopted-reaper".into())
            .spawn(move || watch(pids, &inner))
            .ok();
    }

    fn watch(pids: Vec<i32>, inner: &MonitorInner) {
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            // kqueue 建不出来是灾难性环境问题：宁可全部记已退出（会话走 EOF
            // 自然结束），也不能让 Adopted 会话永远卡在 Running。
            for pid in pids {
                inner.mark_exited(pid);
            }
            return;
        }
        let _kq = unsafe { std::os::fd::OwnedFd::from_raw_fd(kq) };

        let mut changes = Vec::with_capacity(pids.len());
        for pid in &pids {
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            event.ident = *pid as usize;
            event.filter = libc::EVFILT_PROC;
            event.flags = libc::EV_ADD | libc::EV_ONESHOT;
            event.fflags = libc::NOTE_EXIT;
            event.udata = *pid as *mut libc::c_void;
            changes.push(event);
        }
        // 先 EV_ADD 全部：已死的 pid 在返回事件里带 EV_ERROR/ESRCH。
        let mut events = vec![unsafe { std::mem::zeroed::<libc::kevent>() }; changes.len().max(1)];
        let mut pending: HashSet<i32> = pids.into_iter().collect();
        // 注册与首轮收割同一调用：kevent(changelist 非空、eventlist 非空) 原子完成。
        loop {
            let fired = unsafe {
                libc::kevent(
                    _kq.as_raw_fd(),
                    changes.as_ptr(),
                    changes.len() as libc::c_int,
                    events.as_mut_ptr(),
                    events.len() as libc::c_int,
                    std::ptr::null(),
                )
            };
            changes.clear(); // 仅首轮带 changelist，后续纯收割。
            if fired < 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                // kevent 持续失败：同 kqueue 建失败，全部记退出、不卡会话。
                for pid in pending {
                    inner.mark_exited(pid);
                }
                return;
            }
            for event in events.iter().take(fired as usize) {
                let pid = event.udata as i32;
                // EV_ERROR（ESRCH：注册时已死）与 NOTE_EXIT 都=已退出。
                // EV_ONESHOT 保证每个 pid 只到一次。
                inner.mark_exited(pid);
                pending.remove(&pid);
            }
            if pending.is_empty() {
                return;
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod backend {
    use super::MonitorInner;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::sync::Arc;

    pub fn spawn_watcher(pids: Vec<i32>, inner: Arc<MonitorInner>) {
        std::thread::Builder::new()
            .name("smeltd-adopted-reaper".into())
            .spawn(move || watch(pids, &inner))
            .ok();
    }

    fn watch(pids: Vec<i32>, inner: &MonitorInner) {
        // pidfd_open(ESRCH=已死) + poll(POLLIN=已退出)，与 kqueue 同构。
        let mut watched: Vec<(i32, OwnedFd)> = Vec::new();
        for pid in pids {
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as libc::c_int };
            if fd < 0 {
                inner.mark_exited(pid);
                continue;
            }
            watched.push((pid, unsafe { OwnedFd::from_raw_fd(fd) }));
        }
        let mut fds: Vec<libc::pollfd> = watched
            .iter()
            .map(|(_, fd)| libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let mut pending = watched.len();
        while pending > 0 {
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
            if ready < 0 {
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                break;
            }
            for (index, (pid, _)) in watched.iter().enumerate() {
                if fds[index].revents != 0 {
                    fds[index].revents = 0;
                    fds[index].fd = -1; // 只收一次。
                    inner.mark_exited(*pid);
                    pending -= 1;
                }
            }
        }
        // poll 异常退出：剩余未确认的记退出、不卡会话。
        for (index, (pid, _)) in watched.iter().enumerate() {
            if fds[index].fd != -1 {
                inner.mark_exited(*pid);
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod backend {
    use super::MonitorInner;
    use std::sync::Arc;
    use std::time::Duration;

    pub fn spawn_watcher(pids: Vec<i32>, inner: Arc<MonitorInner>) {
        std::thread::Builder::new()
            .name("smeltd-adopted-reaper".into())
            .spawn(move || {
                let mut pending: Vec<i32> = pids;
                while !pending.is_empty() {
                    pending.retain(|pid| {
                        // kill(pid, 0)：ESRCH=已死；0/EPERM=活着。
                        let alive = unsafe { libc::kill(*pid, 0) == 0 }
                            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
                        if alive {
                            return true;
                        }
                        inner.mark_exited(*pid);
                        false
                    });
                    if !pending.is_empty() {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            })
            .ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn spawn_sleeper(seconds: &str) -> (u32, std::process::Child) {
        let child = std::process::Command::new("/bin/sleep")
            .arg(seconds)
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        (pid, child)
    }

    #[test]
    fn adopted_exit_is_observed() {
        let (pid, mut child) = spawn_sleeper("0.2");
        let monitor = ChildMonitor::spawn(&[pid as i32]);
        let waiter = monitor.waiter(pid as i32).expect("应有等待句柄");
        assert!(!waiter.is_exited(), "sleep 0.2 不应瞬间退出");
        assert!(
            waiter.wait_timeout(Duration::from_secs(5)),
            "子进程退出后必须被观察到"
        );
        assert!(waiter.is_exited());
        let _ = child.wait(); // 测试进程是父进程，负责 reap。
    }

    #[test]
    fn multiple_pids_all_resolve() {
        let (pid1, mut child1) = spawn_sleeper("0.1");
        let (pid2, mut child2) = spawn_sleeper("0.3");
        let monitor = ChildMonitor::spawn(&[pid1 as i32, pid2 as i32, pid1 as i32]);
        let w1 = monitor.waiter(pid1 as i32).unwrap();
        let w2 = monitor.waiter(pid2 as i32).unwrap();
        assert!(w1.wait_timeout(Duration::from_secs(5)));
        assert!(w2.wait_timeout(Duration::from_secs(5)));
        let _ = child1.wait();
        let _ = child2.wait();
    }

    #[test]
    fn invalid_pid_is_immediately_exited() {
        let monitor = ChildMonitor::spawn(&[0, 1, -5]);
        for pid in [0, 1, -5] {
            let waiter = monitor.waiter(pid).expect("非法 pid 也有句柄");
            assert!(waiter.is_exited(), "pid={pid} 应直接记已退出");
            assert!(waiter.wait_timeout(Duration::from_millis(10)));
        }
        assert!(monitor.waiter(123456).is_none());
    }
}
