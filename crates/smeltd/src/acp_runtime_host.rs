//! 独立 ACP session host。
//!
//! 主 daemon 只保存可重建的会话镜像；ACP SDK 的 connection future、JSON-RPC
//! callback、permission/elicitation responder 和 prompt 队列都留在这个独立子进程。
//! daemon `exec()` 时只交接一条 Unix socket，因此活跃回合不需要跨进程序列化。

use super::*;

use std::os::fd::{IntoRawFd, OwnedFd};
use std::process::Stdio;
use std::sync::atomic::AtomicI32;

const SESSION_HOST_ARG: &str = "--acp-session-host";

#[derive(Debug)]
pub(crate) struct HostedSnapshotEnvelope {
    pub(crate) snapshot: smelt_core::acp_session::ConversationSnapshot,
    pub(crate) provider_pid: Option<i32>,
}

#[derive(serde::Deserialize)]
struct HostedSnapshotLine {
    snapshot: smelt_core::acp_session::ConversationSnapshot,
    #[serde(default)]
    provider_pid: Option<i32>,
}

/// 主 daemon 持有的 session-host 控制句柄。控制 socket 的这一端是 handoff 时
/// 唯一需要活过 exec 的 fd；reader 使用 dup 出来的 fd，旧线程随 exec 消失后，
/// 新 daemon 会从控制 fd 再建 reader 并请求一次全量快照。
pub(crate) struct HostedConversationHandle {
    pid: i32,
    control: Mutex<Option<UnixStream>>,
    snapshot_rx: smol::channel::Receiver<HostedSnapshotEnvelope>,
    provider_pid: Arc<AtomicI32>,
}

impl HostedConversationHandle {
    pub(crate) fn spawn(
        initial_open: &serde_json::Value,
        spawn_gate: &Arc<RwLock<()>>,
    ) -> std::io::Result<Self> {
        let (parent, child) = UnixStream::pair()?;
        set_cloexec(parent.as_raw_fd(), true);
        set_cloexec(child.as_raw_fd(), true);

        let child_fd = unsafe { OwnedFd::from_raw_fd(child.into_raw_fd()) };
        let exe = daemon_executable_path()?;
        let mut command = std::process::Command::new(exe);
        command
            .arg(SESSION_HOST_ARG)
            .stdin(Stdio::from(child_fd))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        // 与主 daemon 的 upgrade 独占锁互斥，保证新 host 不会在 fd 收集过程中
        // fork 出来并意外继承已清 CLOEXEC 的其它会话描述符。
        let child = {
            let _spawn = spawn_gate.read().unwrap();
            command.spawn()?
        };
        let pid = child.id() as i32;
        drop(child);

        let handle = Self::from_stream(pid, parent, None)?;
        if let Err(error) = handle.send_json(initial_open) {
            let _ = handle.shutdown_and_wait(ACP_SHUTDOWN_GRACE);
            return Err(error);
        }
        Ok(handle)
    }

    /// 从 daemon handoff 继承的 fd 重建句柄。session host 仍是同一个直接子进程，
    /// 所以新程序映像可以继续按原 pid waitpid。
    pub(crate) unsafe fn from_handoff(
        pid: i32,
        control_fd: RawFd,
        provider_pid: Option<i32>,
    ) -> std::io::Result<Self> {
        set_cloexec(control_fd, true);
        // SAFETY: 调用方已校验 fd 有效且从 handoff 中取得唯一所有权。
        let control = unsafe { UnixStream::from_raw_fd(control_fd) };
        Self::from_stream(pid, control, provider_pid)
    }

    fn from_stream(
        pid: i32,
        control: UnixStream,
        initial_provider_pid: Option<i32>,
    ) -> std::io::Result<Self> {
        let reader = control.try_clone()?;
        let provider_pid = Arc::new(AtomicI32::new(initial_provider_pid.unwrap_or(0)));
        let snapshot_rx = start_snapshot_reader(reader, Arc::clone(&provider_pid))?;
        Ok(Self {
            pid,
            control: Mutex::new(Some(control)),
            snapshot_rx,
            provider_pid,
        })
    }

    pub(crate) fn send_action(
        &self,
        action: &smelt_core::acp_session::AcpUserAction,
    ) -> std::io::Result<()> {
        self.send_json(action)
    }

    pub(crate) fn request_full_snapshot(&self) -> std::io::Result<()> {
        self.send_action(&smelt_core::acp_session::AcpUserAction::Refresh)
    }

    fn send_json(&self, value: &impl serde::Serialize) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
        let mut control = self.control.lock().unwrap();
        let Some(control) = control.as_mut() else {
            return Err(std::io::Error::new(
                ErrorKind::BrokenPipe,
                "ACP session host 已关闭",
            ));
        };
        control.write_all(&bytes)?;
        control.write_all(b"\n")?;
        control.flush()
    }

    pub(crate) fn snapshot_rx(&self) -> smol::channel::Receiver<HostedSnapshotEnvelope> {
        self.snapshot_rx.clone()
    }

    pub(crate) fn control_fd(&self) -> Option<RawFd> {
        self.control
            .lock()
            .unwrap()
            .as_ref()
            .map(AsRawFd::as_raw_fd)
    }

    pub(crate) fn pid(&self) -> i32 {
        self.pid
    }

    pub(crate) fn provider_pid(&self) -> Option<i32> {
        let pid = self.provider_pid.load(Ordering::SeqCst);
        (pid > 1).then_some(pid)
    }

    #[cfg(test)]
    pub(crate) fn test_stub() -> (Self, UnixStream) {
        let (control, peer) = UnixStream::pair().unwrap();
        (Self::from_stream(999_999, control, None).unwrap(), peer)
    }

    pub(crate) fn shutdown_and_wait(self, grace: Duration) -> bool {
        if let Some(control) = self.control.lock().unwrap().take() {
            let _ = control.shutdown(Shutdown::Both);
        }

        // host 收到 EOF 后会先让其 ACP connection 正常收尾。它内部的 provider
        // shutdown 最坏还包含一次 kill+reap 宽限，因此这里至少给两倍窗口。
        let deadline = Instant::now() + grace + grace + Duration::from_millis(250);
        loop {
            match waitpid_nonblocking(self.pid) {
                HostWait::Exited | HostWait::NotOurChild => return true,
                HostWait::Running if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(5));
                }
                HostWait::Running => break,
            }
        }

        // host 本身异常卡住时，provider 是它的独立进程组，不能只杀 host 组。
        if let Some(provider_pid) = self.provider_pid() {
            unsafe {
                libc::kill(-provider_pid, libc::SIGKILL);
            }
        }
        smelt_core::acp_conn::kill_and_reap_process_group(self.pid, grace)
    }
}

fn start_snapshot_reader(
    stream: UnixStream,
    provider_pid: Arc<AtomicI32>,
) -> std::io::Result<smol::channel::Receiver<HostedSnapshotEnvelope>> {
    let (tx, rx) = smol::channel::unbounded();
    thread::Builder::new()
        .name("smelt-acp-host-snapshots".to_string())
        .spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let Ok(parsed) = serde_json::from_str::<HostedSnapshotLine>(line.trim()) else {
                    continue;
                };
                let live_provider_pid = parsed.provider_pid.filter(|pid| *pid > 1);
                // None 也是事实：provider 已被宿主收尸后必须清掉旧 pid，避免该
                // pid 被系统复用后，后续异常清理误杀无关进程组。
                provider_pid.store(live_provider_pid.unwrap_or(0), Ordering::SeqCst);
                if tx
                    .try_send(HostedSnapshotEnvelope {
                        snapshot: parsed.snapshot,
                        provider_pid: live_provider_pid,
                    })
                    .is_err()
                {
                    break;
                }
            }
        })?;
    Ok(rx)
}

enum HostWait {
    Running,
    Exited,
    NotOurChild,
}

fn waitpid_nonblocking(pid: i32) -> HostWait {
    loop {
        let waited = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
        if waited == pid {
            return HostWait::Exited;
        }
        if waited == 0 {
            return HostWait::Running;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::ECHILD) => return HostWait::NotOurChild,
            _ => return HostWait::Running,
        }
    }
}

pub(crate) fn is_session_host_process() -> bool {
    std::env::args_os().any(|arg| arg == SESSION_HOST_ARG)
}

/// session-host 模式不绑定全局 daemon socket。stdin 是主 daemon 传入的双向
/// Unix socket：首行是内部 acp_open，后续沿用正常 `AcpUserAction` wire；输出
/// 仍复用 `handle_acp_open` 的 snapshot attachment。
pub(crate) fn run_session_host() {
    if let Some(home) = std::env::var_os("HOME") {
        let _ = std::env::set_current_dir(home);
    }
    smelt_core::fd_limit::raise_fd_limit();

    let fd = unsafe { libc::dup(libc::STDIN_FILENO) };
    if fd < 0 {
        return;
    }
    let conn = unsafe { UnixStream::from_raw_fd(fd) };
    let Ok(reader_stream) = conn.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let mut first_line = String::new();
    match reader.read_line(&mut first_line) {
        Ok(0) | Err(_) => return,
        Ok(_) => {}
    }
    let Ok(open) = serde_json::from_str::<serde_json::Value>(first_line.trim()) else {
        return;
    };
    let Some(req) = parse_acp_open_request(&open) else {
        return;
    };

    let sessions = new_sessions();
    let acp_sessions = new_acp_sessions();
    let event_hub = event_hub::DaemonEventHub::in_memory();

    // 主 daemon 的镜像作为启动种子：既保留本地历史与投递去重表，也让
    // FreshOnMissing/Strict 的恢复判断与拆进程前完全一致。
    let seed = open.get("host_seed_snapshot").cloned().and_then(|value| {
        serde_json::from_value::<smelt_core::acp_session::ConversationSnapshot>(value).ok()
    });
    let agent_token = open["host_agent_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .map(String::from);
    let (_, created) = acp_sessions.reserve_with(&req.id, || {
        let session = make_acp_session(
            &req.id,
            req.cwd.clone(),
            req.agent_needs_transcript_check,
            req.conversation_binding.clone(),
            req.agent_session.clone(),
            req.pending_agent_preset.clone(),
        );
        if let Some(seed) = seed {
            *session.reduced.lock().unwrap() =
                smelt_core::acp_session::AcpSessionState::from_snapshot(seed);
        }
        if let Some(agent_token) = agent_token {
            session.state.lock().unwrap().agent_token = agent_token;
        }
        session
    });
    if !created {
        return;
    }

    handle_acp_open(
        conn,
        reader,
        &open,
        sessions,
        Arc::clone(&acp_sessions),
        event_hub,
    );

    // 控制 socket EOF 就是宿主生命周期结束。先正常停止 provider 再退出，避免
    // daemon kill/restart 留下独立进程组的孤儿 agent。
    for (_, slot) in acp_sessions.snapshot() {
        let _lifecycle = slot.lifecycle.lock().unwrap();
        let _ = retire_acp_runtime(&slot.value);
    }
}
