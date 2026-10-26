//! 独立 ACP session host。
//!
//! 主 daemon 只保存可重建的会话镜像；ACP SDK 的 connection future、JSON-RPC
//! callback、permission/elicitation responder 和 prompt 队列都留在这个独立子进程。
//! 守护重启会停掉这个子进程。会话正文在 agent 自己的历史里，下次用 `session/load` 续上。

use super::*;

use std::collections::HashMap;
#[cfg(unix)]
use std::os::fd::{IntoRawFd, OwnedFd};
use std::process::Stdio;
use std::sync::atomic::{AtomicI32, AtomicU64};
use std::sync::mpsc;

const SESSION_HOST_ARG: &str = "--acp-session-host";

/// `Command::spawn` 返回后，`Command` 仍握着交给子进程的 stdin fd。
/// 必须在写启动帧之前丢掉它：子进程一旦退出，这个没人读的读端会让
/// `write_all` 把套接字写满后永久阻塞。
fn spawn_host_process(mut command: std::process::Command) -> std::io::Result<std::process::Child> {
    let child = command.spawn()?;
    drop(command);
    Ok(child)
}

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

/// 历史页不是快照。读者必须先认这条，再按快照解析；认错会把镜像拆掉。
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct HistoryPageLine {
    pub(crate) history_page: HistoryPageBody,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct HistoryPageBody {
    pub(crate) request_id: u64,
    pub(crate) snapshot: smelt_core::acp_session::ConversationSnapshot,
}

struct HostInner {
    pid: i32,
    control: Mutex<Option<ClientConn>>,
    snapshot_rx: smol::channel::Receiver<HostedSnapshotEnvelope>,
    /// 宿主转上来的 Pi 会话互发请求（`peer_request` 行的内容）。
    peer_rx: smol::channel::Receiver<serde_json::Value>,
    provider_pid: Arc<AtomicI32>,
    history_waiters:
        Arc<Mutex<HashMap<u64, mpsc::SyncSender<smelt_core::acp_session::ConversationSnapshot>>>>,
    next_history_request: AtomicU64,
}

/// 主 daemon 持有的 session-host 控制句柄。
/// 克隆只增加内部引用，分页可以在生命周期锁外等回包。
#[derive(Clone)]
pub(crate) struct HostedConversationHandle {
    inner: Arc<HostInner>,
}

impl HostedConversationHandle {
    pub(crate) fn spawn(
        initial_open: &serde_json::Value,
        spawn_gate: &Arc<RwLock<()>>,
    ) -> std::io::Result<Self> {
        #[cfg(unix)]
        let (parent, child_stdio) = {
            let (parent, child) = UnixStream::pair()?;
            set_cloexec(parent.as_raw_fd(), true)?;
            set_cloexec(child.as_raw_fd(), true)?;
            let child_fd = unsafe { OwnedFd::from_raw_fd(child.into_raw_fd()) };
            (parent, Stdio::from(child_fd))
        };
        #[cfg(windows)]
        let pipe_name = {
            let tag = format!("acp-host-{}-{}", std::process::id(), uuid::Uuid::new_v4().simple());
            smelt_sys::named_pipe::user_pipe_path(&tag)?
        };
        #[cfg(windows)]
        let pipe_listener = smelt_sys::named_pipe::PipeListener::bind(&pipe_name)?;
        let exe = session_host_executable()?;
        let mut command = std::process::Command::new(exe);
        command
            .arg(SESSION_HOST_ARG)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.stdin(child_stdio).process_group(0);
        }
        #[cfg(windows)]
        {
            command
                .stdin(Stdio::null())
                .env("SMELT_HOST_PIPE", &pipe_name);
        }

        let child = {
            let _spawn = spawn_gate.read().unwrap();
            spawn_host_process(command)?
        };
        let pid = child.id() as i32;
        drop(child);
        #[cfg(windows)]
        if let Err(error) = smelt_sys::process::attach_running(pid as u32) {
            dlog(&format!(
                "acp host pid={pid} 没能放进 Job（{error}），结束时改为结束这个 pid"
            ));
        }

        #[cfg(unix)]
        let control = ClientConn::from_unix(parent);
        #[cfg(windows)]
        let control = ClientConn::from_pipe(pipe_listener.accept().map_err(|error| {
            let _ = smelt_sys::process::terminate_pid(pid as u32);
            error
        })?);
        let handle = Self::from_stream(pid, control, None)?;
        if let Err(error) = handle.send_json(initial_open) {
            let _ = handle.shutdown_and_wait(ACP_SHUTDOWN_GRACE);
            return Err(error);
        }
        Ok(handle)
    }

    fn from_stream(
        pid: i32,
        control: ClientConn,
        initial_provider_pid: Option<i32>,
    ) -> std::io::Result<Self> {
        let reader = control.try_clone()?;
        let provider_pid = Arc::new(AtomicI32::new(initial_provider_pid.unwrap_or(0)));
        let history_waiters = Arc::new(Mutex::new(HashMap::new()));
        let (peer_tx, peer_rx) = smol::channel::unbounded();
        let snapshot_rx = start_snapshot_reader(
            reader,
            Arc::clone(&provider_pid),
            pid,
            Arc::clone(&history_waiters),
            peer_tx,
        )?;
        Ok(Self {
            inner: Arc::new(HostInner {
                pid,
                control: Mutex::new(Some(control)),
                snapshot_rx,
                peer_rx,
                provider_pid,
                history_waiters,
                next_history_request: AtomicU64::new(0),
            }),
        })
    }

    pub(crate) fn send_action(
        &self,
        action: &smelt_core::acp_session::AcpUserAction,
    ) -> std::io::Result<()> {
        self.send_json(action)
    }

    /// 向宿主要绝对下标 `before` 之前的一页。
    /// 超时、断线或旧宿主不认识这个动作，都只让这一次失败。
    /// 父镜像继续只留当前回合，不把整段历史放回常驻内存。
    pub(crate) fn request_history_page(
        &self,
        before: usize,
        limit: usize,
    ) -> Result<smelt_core::acp_session::ConversationSnapshot, String> {
        self.request_history_page_within(before, limit, Duration::from_secs(4))
    }

    fn request_history_page_within(
        &self,
        before: usize,
        limit: usize,
        timeout: Duration,
    ) -> Result<smelt_core::acp_session::ConversationSnapshot, String> {
        let request_id = self
            .inner
            .next_history_request
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        let (tx, rx) = mpsc::sync_channel(1);
        self.inner
            .history_waiters
            .lock()
            .unwrap()
            .insert(request_id, tx);
        let action = smelt_core::acp_session::AcpUserAction::HistoryPage {
            request_id,
            before,
            limit,
        };
        if let Err(error) = self.send_action(&action) {
            self.inner
                .history_waiters
                .lock()
                .unwrap()
                .remove(&request_id);
            return Err(error.to_string());
        }
        match rx.recv_timeout(timeout) {
            Ok(snapshot) => Ok(snapshot),
            Err(error) => {
                self.inner
                    .history_waiters
                    .lock()
                    .unwrap()
                    .remove(&request_id);
                Err(format!("读取宿主历史失败：{error}"))
            }
        }
    }

    /// 把宿主上 `[start, end)` 的正文按页拼回来。拼不到起点就失败，避免把半截当成全文。
    pub(crate) fn read_snapshot_covering(
        &self,
        start: usize,
        end: usize,
    ) -> Result<smelt_core::acp_session::ConversationSnapshot, String> {
        if end <= start {
            return Err("历史范围是空的".to_string());
        }
        let mut newest: Option<smelt_core::acp_session::ConversationSnapshot> = None;
        let mut chunks_newest_first: Vec<Vec<smelt_core::acp_chat::AcpEntry>> = Vec::new();
        let mut before = end;
        let mut covered = end;
        let mut total_entries = 0usize;
        loop {
            let limit = before.saturating_sub(start).clamp(1, 500);
            let mut page = self.request_history_page(before, limit)?;
            if page.entries.is_empty() || page.entries_offset >= before {
                break;
            }
            covered = page.entries_offset;
            total_entries = total_entries.saturating_add(page.entries.len());
            chunks_newest_first.push(std::mem::take(&mut page.entries));
            if newest.is_none() {
                newest = Some(page);
            }
            if covered <= start || total_entries > 20_000 {
                break;
            }
            before = covered;
        }
        let mut snapshot = newest.ok_or_else(|| "宿主没有返回历史".to_string())?;
        if covered > start {
            return Err(format!("宿主历史没有覆盖到 {start}，只到 {covered}"));
        }
        let mut entries = Vec::new();
        for chunk in chunks_newest_first.into_iter().rev() {
            entries.extend(chunk);
        }
        if covered < start {
            let drop_n = start - covered;
            if drop_n >= entries.len() {
                return Err("宿主历史页对不齐请求的起点".to_string());
            }
            entries.drain(0..drop_n);
            snapshot.entries_offset = start;
        } else {
            snapshot.entries_offset = covered;
        }
        snapshot.entries = entries;
        Ok(snapshot)
    }

    pub(crate) fn request_full_snapshot(&self) -> std::io::Result<()> {
        self.send_action(&smelt_core::acp_session::AcpUserAction::Refresh)
    }

    fn send_json(&self, value: &impl serde::Serialize) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
        let mut control = self.inner.control.lock().unwrap();
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
        self.inner.snapshot_rx.clone()
    }

    /// 宿主转上来的 Pi 会话互发请求。来源就是这个句柄所在的注册表槽。
    pub(crate) fn peer_requests(&self) -> smol::channel::Receiver<serde_json::Value> {
        self.inner.peer_rx.clone()
    }

    pub(crate) fn pid(&self) -> i32 {
        self.inner.pid
    }

    #[cfg(test)]
    pub(crate) fn test_stub() -> (Self, UnixStream) {
        let (control, peer) = UnixStream::pair().unwrap();
        (
            Self::from_stream(999_999, ClientConn::from_unix(control), None).unwrap(),
            peer,
        )
    }

    pub(crate) fn shutdown_and_wait(self, grace: Duration) -> bool {
        if let Some(control) = self.inner.control.lock().unwrap().take() {
            let _ = control.shutdown(Shutdown::Both);
        }

        // host 收到 EOF 后会先让其 ACP connection 正常收尾。它内部的 provider
        // shutdown 最坏还包含一次 kill+reap 宽限，因此这里至少给两倍再加 250ms。
        // 到点还没退出，再读当时的 provider pid 并杀掉那一组，同时杀掉 host 组。
        // 等的线程独占这个 pid 的 waitpid；干净退出不会去动 provider。
        // 杀过之后还要证明组已经空了。证明只用非阻塞 waitpid，因为阻塞的那条
        // 已经返回，不会再有第二条堵在同一个 pid 上。
        let provider = Arc::clone(&self.inner.provider_pid);
        let host_pid = self.inner.pid;
        let killed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&killed);
        let reaped = smelt_core::process_wait::wait_for_pid(
            host_pid,
            grace + grace + Duration::from_millis(250),
            || {},
            move || {
                flag.store(true, Ordering::SeqCst);
                let provider_pid = provider.load(Ordering::SeqCst);
                if provider_pid > 1 {
                    let _ = smelt_sys::process::signal_tree(
                        provider_pid as u32,
                        smelt_sys::process::TreeSignal::Kill,
                    );
                    smelt_sys::process::release_tree(provider_pid as u32);
                }
                if host_pid > 1 {
                    let _ = smelt_sys::process::signal_tree(
                        host_pid as u32,
                        smelt_sys::process::TreeSignal::Kill,
                    );
                    smelt_sys::process::release_tree(host_pid as u32);
                }
            },
            grace,
        );
        // 宿主被 SIGKILL 时不会跑 Drop，任务目录里的进程组要在这里收。
        // 干净退出时宿主自己已经收过，再扫一次对不上启动时间就不会动手。
        if host_pid > 0 {
            smelt_core::background_tasks::reap_orphaned_task_groups(host_pid as u32);
        }
        if !killed.load(Ordering::SeqCst) || !reaped {
            return reaped;
        }
        smelt_core::acp_conn::prove_process_group_dead(host_pid, grace)
    }
}

fn reap_host_tasks(host_pid: i32) {
    if host_pid > 0 {
        smelt_core::background_tasks::reap_orphaned_task_groups(host_pid as u32);
    }
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map(|index| index + 1)
        .unwrap_or(bytes.len());
    if start >= end {
        &[]
    } else {
        &bytes[start..end]
    }
}

fn dispatch_snapshot_line(
    line: &[u8],
    mirror: &mut smelt_core::acp_wire::FrameMirror,
    tx: &smol::channel::Sender<HostedSnapshotEnvelope>,
    provider_pid: &AtomicI32,
    history_waiters: &Mutex<
        HashMap<u64, mpsc::SyncSender<smelt_core::acp_session::ConversationSnapshot>>,
    >,
    peer_requests: &smol::channel::Sender<serde_json::Value>,
) -> bool {
    let trimmed = trim_ascii(line);
    let value: serde_json::Value = match serde_json::from_slice(trimmed) {
        Ok(value) => value,
        Err(error) => {
            crate::dlog(&format!(
                "acp 宿主快照无法解析，判定协议不兼容并断开镜像：{error}（{} 字节）",
                line.len()
            ));
            return false;
        }
    };
    // 历史页回包没有顶层 `snapshot` 字段。先分流，避免被下面的快照解析当成协议不兼容。
    if value.get("history_page").is_some() {
        if let Ok(page) = serde_json::from_value::<HistoryPageLine>(value)
            && let Some(waiter) = history_waiters
                .lock()
                .unwrap()
                .remove(&page.history_page.request_id)
        {
            let _ = waiter.try_send(page.history_page.snapshot);
        }
        return true;
    }
    // 宿主转上来的 Pi 会话互发请求，同样不是快照。
    if let Some(request) = value.get("peer_request") {
        let _ = peer_requests.try_send(request.clone());
        return true;
    }
    // 增量帧经镜像还原成完整的快照帧，和整段帧一样交给合并。镜像接不上只会发生在
    // 交接后接手的读端上，父进程这时已经要了整段帧，丢掉这一帧等它即可。
    let value = match value.get("delta") {
        Some(delta) => {
            let restored = serde_json::from_value(delta.clone())
                .ok()
                .and_then(|delta| mirror.apply(&delta).ok());
            match restored {
                Some(frame) => frame,
                None => return true,
            }
        }
        None => {
            mirror.full(&value);
            value
        }
    };
    let parsed = match serde_json::from_value::<HostedSnapshotLine>(value) {
        Ok(parsed) => parsed,
        Err(error) => {
            // 快照解析失败几乎只有一个原因：host 与主 daemon 不同版本，快照 schema
            // 对不上（比如 host 发了新增的 enum 变体）。这不是瞬时错误，不会自愈；
            // 旧实现在这里 `continue`，结果是主 daemon（进而 GUI）的镜像永久冻在
            // 最后一份可解析的快照上，且没有任何日志——会话看起来「只恢复了一半」
            // 并永远停在「正在建立连接」。宁可断开镜像让上层落成可重连的终态，
            // 也不能静默冻结。半行被撕开也走这里，所以交接必须在停手时把预取字节交出去。
            crate::dlog(&format!(
                "acp 宿主快照无法解析，判定协议不兼容并断开镜像：{error}（{} 字节）",
                line.len()
            ));
            return false;
        }
    };
    let live_provider_pid = parsed.provider_pid.filter(|pid| *pid > 1);
    // None 也是事实：provider 已被宿主收尸后必须清掉旧 pid，避免该
    // pid 被系统复用后，后续异常清理误杀无关进程组。
    provider_pid.store(live_provider_pid.unwrap_or(0), Ordering::SeqCst);
    tx.try_send(HostedSnapshotEnvelope {
        snapshot: parsed.snapshot,
        provider_pid: live_provider_pid,
    })
    .is_ok()
}

fn start_snapshot_reader(
    stream: ClientConn,
    provider_pid: Arc<AtomicI32>,
    host_pid: i32,
    history_waiters: Arc<
        Mutex<HashMap<u64, mpsc::SyncSender<smelt_core::acp_session::ConversationSnapshot>>>,
    >,
    peer_requests: smol::channel::Sender<serde_json::Value>,
) -> std::io::Result<smol::channel::Receiver<HostedSnapshotEnvelope>> {
    let (tx, rx) = smol::channel::unbounded();
    thread::Builder::new()
        .name("smelt-acp-host-snapshots".to_string())
        .spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut mirror = smelt_core::acp_wire::FrameMirror::default();
            loop {
                let mut line = Vec::new();
                match reader.read_until(b'\n', &mut line) {
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Ok(0) => {
                        reap_host_tasks(host_pid);
                        break;
                    }
                    Ok(_) => {
                        if !dispatch_snapshot_line(
                            &line,
                            &mut mirror,
                            &tx,
                            &provider_pid,
                            &history_waiters,
                            &peer_requests,
                        ) {
                            break;
                        }
                    }
                    Err(_) => {
                        reap_host_tasks(host_pid);
                        break;
                    }
                }
            }
            // 宿主那头断了，等着的历史页不会再来。丢掉发送端，等的人立刻收到断开，
            // 不用等满超时。
            history_waiters.lock().unwrap().clear();
        })?;
    Ok(rx)
}

pub(crate) fn is_session_host_process() -> bool {
    std::env::args_os().any(|arg| arg == SESSION_HOST_ARG)
}

/// session-host 模式不绑定全局 daemon socket。stdin 是主 daemon 传入的双向
/// Unix socket：首行是内部 acp_open，后续沿用正常 `AcpUserAction` wire；输出
/// 仍复用 `handle_acp_open` 的 snapshot attachment。
fn session_host_conn() -> Option<ClientConn> {
    #[cfg(unix)]
    {
        let fd = unsafe { libc::dup(libc::STDIN_FILENO) };
        if fd < 0 {
            return None;
        }
        let conn = unsafe { UnixStream::from_raw_fd(fd) };
        return Some(ClientConn::from_unix(conn));
    }
    #[cfg(windows)]
    {
        let name = std::env::var("SMELT_HOST_PIPE").ok()?;
        let pipe = smelt_sys::named_pipe::PipeStream::connect(&name).ok()?;
        Some(ClientConn::from_pipe(pipe))
    }
}

pub(crate) fn run_session_host() {
    if let Some(home) = dirs::home_dir() {
        let _ = std::env::set_current_dir(home);
    }
    smelt_core::fd_limit::raise_fd_limit();

    let Some(conn) = session_host_conn() else {
        return;
    };
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
    let event_hub = event_hub::DaemonEventHub::new().unwrap();

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
            req.conversation_binding.clone(),
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
    crate::acp_host::install_peer_uplink(Arc::clone(&acp_sessions), req.id);

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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
    use std::process::Stdio;
    use std::time::Instant;

    /// 回归：session host 子进程如果在读启动帧之前就退出，父进程不能卡在 write。
    /// `Command` 在 spawn 之后仍占着 stdin 那端；不丢掉它的话，对端已经是僵尸，
    /// 写大于缓冲的帧也会永远阻塞，外层 `ensure_acp_session` 的生命周期锁一起冻住。
    #[test]
    fn open_frame_write_fails_fast_when_host_already_exited() {
        let (mut parent, child_sock) = UnixStream::pair().unwrap();
        let child_fd = unsafe { OwnedFd::from_raw_fd(child_sock.into_raw_fd()) };
        let mut command = std::process::Command::new("/usr/bin/true");
        command
            .stdin(Stdio::from(child_fd))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        parent
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut child = spawn_host_process(command).unwrap();
        let _ = child.wait();
        let payload = vec![b'x'; 1024 * 1024];
        let started = Instant::now();
        let error = parent
            .write_all(&payload)
            .expect_err("已退出的宿主不能把启动帧写成功");
        assert!(
            started.elapsed() < Duration::from_millis(900),
            "写启动帧不能堵住，实际 {error:?} 用时 {:?}",
            started.elapsed()
        );
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe,
            "子进程已退出应立刻 EPIPE，实际 {error:?}"
        );
    }

    /// 回归：host 与主 daemon 版本不一致时，快照 JSON 会解析失败。旧实现在这里
    /// 静默 `continue`，主 daemon 的镜像就永久冻结在最后一份可解析的快照上——
    /// 会话表现为「只恢复了一部分、永远停在正在建立连接」，且没有任何日志。
    /// 现在必须断开镜像，让上层落成可重连的终态。
    #[test]
    fn unparsable_snapshot_line_disconnects_the_mirror() {
        let (host_side, daemon_side) = UnixStream::pair().unwrap();
        let rx = start_snapshot_reader(
            crate::client_conn::ClientConn::Unix(daemon_side),
            Arc::new(AtomicI32::new(0)),
            0,
            Arc::new(Mutex::new(std::collections::HashMap::new())),
            smol::channel::unbounded().0,
        )
        .unwrap();

        let mut host_side = host_side;
        // schema 对不上的一行（新版 host 发了旧版 daemon 不认识的变体）。
        host_side
            .write_all(b"{\"snapshot\":{\"entries\":[{\"ToolCall\":{\"output\":[{\"NewVariant\":{}}]}}]}}\n")
            .unwrap();
        host_side.flush().unwrap();

        assert!(
            smol::block_on(smol::future::race(
                async { rx.recv().await.map(|_| ()) },
                async {
                    smol::Timer::after(std::time::Duration::from_secs(5)).await;
                    // 旧实现静默 `continue`，channel 永远不关，这里会走到超时分支；
                    // 用 Ok(()) 让断言失败，而不是把测试挂死。
                    Ok(())
                },
            ))
            .is_err(),
            "解析不了的快照必须断开镜像，不能静默丢弃"
        );
    }

    /// 历史页回包和快照走同一条 socket。它不能被当成坏快照把镜像拆掉。
    #[test]
    fn history_page_reply_reaches_the_waiter_without_dropping_the_mirror() {
        let (handle, mut peer) = HostedConversationHandle::test_stub();
        thread::spawn(move || {
            let mut reader = BufReader::new(peer.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let action: smelt_core::acp_session::AcpUserAction =
                serde_json::from_str(line.trim()).unwrap();
            let smelt_core::acp_session::AcpUserAction::HistoryPage {
                request_id,
                before,
                limit,
            } = action
            else {
                panic!("父进程应发出历史页动作，实际 {line}");
            };
            assert_eq!(before, 10);
            assert_eq!(limit, 2);
            let mut snapshot =
                smelt_core::acp_session::AcpSessionState::default().to_snapshot(false);
            snapshot.entries = vec![smelt_core::acp_chat::AcpEntry::User("old".into())];
            snapshot.entries_offset = 8;
            snapshot.entries_total = 10;
            let reply = serde_json::json!({
                "history_page": {
                    "request_id": request_id,
                    "snapshot": snapshot,
                }
            });
            writeln!(peer, "{reply}").unwrap();
            // 镜像还在：再写一行坏快照才会拆掉。这里停住，证明历史页本身没有拆连接。
            thread::sleep(Duration::from_millis(50));
        });

        let page = handle
            .request_history_page(10, 2)
            .expect("历史页应回到等待方");
        assert_eq!(page.entries_offset, 8);
        assert_eq!(page.entries_total, 10);
        assert!(handle.snapshot_rx().is_empty(), "历史页不能进快照合并通道");
    }

    /// 旧宿主不认识历史页，新宿主也可能这一页超过等待时间。两种都只让这一次失败。
    /// 不能接着发 Refresh，否则下一帧会把整段历史留在父进程里。
    #[test]
    fn history_page_timeout_does_not_request_a_resident_full_copy() {
        let (handle, peer) = HostedConversationHandle::test_stub();
        let waiting = thread::spawn(move || {
            handle.request_history_page_within(10, 2, Duration::from_millis(40))
        });

        let mut reader = BufReader::new(peer.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let action: smelt_core::acp_session::AcpUserAction =
            serde_json::from_str(line.trim()).unwrap();
        assert!(
            matches!(
                action,
                smelt_core::acp_session::AcpUserAction::HistoryPage { .. }
            ),
            "第一行应是历史页，实际 {line}"
        );

        peer.set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut extra = String::new();
        let second = reader.read_line(&mut extra);
        assert!(
            second.is_err(),
            "超时后不能再要整段快照，实际读到 {extra:?}"
        );
        let error = waiting.join().unwrap().expect_err("这一页应失败");
        assert!(
            error.contains("读取宿主历史失败"),
            "失败应留在这一次调用里，实际 {error}"
        );
    }

    /// 宿主转上来的互发请求和快照走同一条 socket，不能被当成坏快照拆掉镜像。
    #[test]
    fn peer_request_reaches_the_gate_without_dropping_the_mirror() {
        let (handle, mut peer) = HostedConversationHandle::test_stub();
        let requests = handle.peer_requests();
        let request = serde_json::json!({
            "peer_request": {"request_id": 7, "op": "session_list", "body": {}}
        });
        writeln!(peer, "{request}").unwrap();
        peer.write_all(&hosted_line("still")).unwrap();

        let forwarded = smol::block_on(smol::future::race(
            async { requests.recv().await.ok() },
            async {
                smol::Timer::after(Duration::from_secs(2)).await;
                None
            },
        ))
        .expect("请求应交给门卫");
        assert_eq!(forwarded["request_id"], 7);
        assert_eq!(forwarded["op"], "session_list");
        let next = smol::block_on(smol::future::race(
            async { handle.snapshot_rx().recv().await.ok() },
            async {
                smol::Timer::after(Duration::from_secs(2)).await;
                None
            },
        ))
        .expect("后面的快照照常到达");
        assert_eq!(next.snapshot.session_title.as_deref(), Some("still"));
    }

    fn hosted_line(title: &str) -> Vec<u8> {
        let mut snapshot = smelt_core::acp_session::AcpSessionState::default().to_snapshot(false);
        snapshot.session_title = Some(title.to_string());
        let mut bytes = serde_json::to_vec(&serde_json::json!({ "snapshot": snapshot })).unwrap();
        bytes.push(b'\n');
        bytes
    }
}
