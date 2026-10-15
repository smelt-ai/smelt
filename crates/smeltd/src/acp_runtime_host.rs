//! 独立 ACP session host。
//!
//! 主 daemon 只保存可重建的会话镜像；ACP SDK 的 connection future、JSON-RPC
//! callback、permission/elicitation responder 和 prompt 队列都留在这个独立子进程。
//! daemon `exec()` 时只交接一条 Unix socket，因此活跃回合不需要跨进程序列化。

use super::*;

use std::collections::HashMap;
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
    control: Mutex<Option<UnixStream>>,
    snapshot_rx: smol::channel::Receiver<HostedSnapshotEnvelope>,
    provider_pid: Arc<AtomicI32>,
    history_waiters:
        Arc<Mutex<HashMap<u64, mpsc::SyncSender<smelt_core::acp_session::ConversationSnapshot>>>>,
    next_history_request: AtomicU64,
    /// 交接前停掉快照读端，并把已经从内核预取的字节交出去。
    handoff: Arc<ReaderHandoff>,
}

impl Drop for HostInner {
    /// 交接停手的读线程在等回滚，句柄没了就叫它退出。
    fn drop(&mut self) {
        self.handoff.close();
    }
}

/// 主 daemon 持有的 session-host 控制句柄。控制 socket 的这一端要活过交接。
/// 快照读端用的是 dup。交接前必须先停掉这个读端，并把已经预取、还没交给
/// 继任者的字节当行首前缀传过去。停手之后旧读端不再读。两个进程同时读同一条
/// 流会把一行从中间撕开，继任者会把残行当成协议不兼容并永久拆掉镜像。
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
        let (parent, child) = UnixStream::pair()?;
        set_cloexec(parent.as_raw_fd(), true)?;
        set_cloexec(child.as_raw_fd(), true)?;

        let child_fd = unsafe { OwnedFd::from_raw_fd(child.into_raw_fd()) };
        let exe = session_host_executable()?;
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
            spawn_host_process(command)?
        };
        let pid = child.id() as i32;
        drop(child);

        let handle = Self::from_stream(pid, parent, None, Vec::new())?;
        if let Err(error) = handle.send_json(initial_open) {
            let _ = handle.shutdown_and_wait(ACP_SHUTDOWN_GRACE);
            return Err(error);
        }
        Ok(handle)
    }

    /// 从 daemon handoff 继承的 fd 重建句柄。session host 仍是同一个直接子进程，
    /// 所以新程序映像可以继续按原 pid waitpid。
    /// `pending_host_bytes` 是前任读端已经取走、继任者必须先读的字节。
    pub(crate) unsafe fn from_handoff(
        pid: i32,
        control_fd: RawFd,
        provider_pid: Option<i32>,
        pending_host_bytes: Vec<u8>,
    ) -> std::io::Result<Self> {
        set_cloexec(control_fd, true)?;
        // SAFETY: 调用方已校验 fd 有效且从 handoff 中取得唯一所有权。
        let control = unsafe { UnixStream::from_raw_fd(control_fd) };
        Self::from_stream(pid, control, provider_pid, pending_host_bytes)
    }

    fn from_stream(
        pid: i32,
        control: UnixStream,
        initial_provider_pid: Option<i32>,
        pending_host_bytes: Vec<u8>,
    ) -> std::io::Result<Self> {
        let reader = control.try_clone()?;
        let provider_pid = Arc::new(AtomicI32::new(initial_provider_pid.unwrap_or(0)));
        let history_waiters = Arc::new(Mutex::new(HashMap::new()));
        let handoff = ReaderHandoff::new();
        let snapshot_rx = start_snapshot_reader(
            reader,
            pending_host_bytes,
            Arc::clone(&handoff),
            Arc::clone(&provider_pid),
            pid,
            Arc::clone(&history_waiters),
        )?;
        Ok(Self {
            inner: Arc::new(HostInner {
                pid,
                control: Mutex::new(Some(control)),
                snapshot_rx,
                provider_pid,
                history_waiters,
                next_history_request: AtomicU64::new(0),
                handoff,
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

    pub(crate) fn control_fd(&self) -> Option<RawFd> {
        self.inner
            .control
            .lock()
            .unwrap()
            .as_ref()
            .map(AsRawFd::as_raw_fd)
    }

    /// 停掉快照读端并取走它已经预取的字节。调用方必须在把控制 fd 交给继任者之前
    /// 调用：返回之后这个句柄的读线程不再读 socket。
    /// 半行超过 [`HOST_HANDOFF_RESIDUAL_LIMIT`] 时失败，调用方不得把 fd 交出去。
    pub(crate) fn take_for_handoff(&self) -> std::io::Result<Vec<u8>> {
        self.inner.handoff.stop.store(true, Ordering::SeqCst);
        let mut park = self.inner.handoff.park.lock().unwrap();
        let deadline = Instant::now() + HANDOFF_READER_STOP_TIMEOUT;
        loop {
            if let Some(bytes) = park.handed.take() {
                if bytes.len() > HOST_HANDOFF_RESIDUAL_LIMIT {
                    return Err(std::io::Error::new(
                        ErrorKind::InvalidData,
                        "交接时读端手里的半行太长",
                    ));
                }
                return Ok(bytes);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(std::io::Error::new(
                    ErrorKind::TimedOut,
                    "快照读端没有在交接前停下来",
                ));
            }
            let (guard, wait) = self
                .inner
                .handoff
                .cv
                .wait_timeout(park, deadline - now)
                .unwrap();
            park = guard;
            if wait.timed_out() && park.handed.is_none() {
                return Err(std::io::Error::new(
                    ErrorKind::TimedOut,
                    "快照读端没有在交接前停下来",
                ));
            }
        }
    }

    /// 交接回滚后让停手的读端接着读：先读停手时取走的字节，再读 socket。
    /// 控制连接全程没动过，宿主和镜像都不用重开。
    pub(crate) fn resume_after_handoff_rollback(&self) {
        self.inner.handoff.resume();
    }

    #[cfg(test)]
    fn clone_control(&self) -> std::io::Result<UnixStream> {
        self.inner
            .control
            .lock()
            .unwrap()
            .as_ref()
            .ok_or_else(|| std::io::Error::other("ACP session host 已关闭"))?
            .try_clone()
    }

    pub(crate) fn pid(&self) -> i32 {
        self.inner.pid
    }

    pub(crate) fn provider_pid(&self) -> Option<i32> {
        let pid = self.inner.provider_pid.load(Ordering::SeqCst);
        (pid > 1).then_some(pid)
    }

    #[cfg(test)]
    pub(crate) fn test_stub() -> (Self, UnixStream) {
        let (control, peer) = UnixStream::pair().unwrap();
        (
            Self::from_stream(999_999, control, None, Vec::new()).unwrap(),
            peer,
        )
    }

    pub(crate) fn shutdown_and_wait(self, grace: Duration) -> bool {
        if let Some(control) = self.inner.control.lock().unwrap().take() {
            let _ = control.shutdown(Shutdown::Both);
        }
        self.inner.handoff.close();

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
                    unsafe {
                        libc::kill(-provider_pid, libc::SIGKILL);
                    }
                }
                if host_pid > 1 {
                    unsafe {
                        libc::kill(-host_pid, libc::SIGKILL);
                    }
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

/// 读端空闲时隔这么久看一次有没有被要求停手。数据到达会立刻返回，不额外延迟快照。
const SNAPSHOT_READ_POLL: Duration = Duration::from_millis(100);
/// 半行超过这个长度就不塞进交接清单。完整的超长行直接丢掉（继任者会要全量快照）；
/// 半行则继续读到换行再丢。读不到换行就让交接失败，会话走现有的退役路径。
/// 256KB 的字节数组展开成 JSON 后仍远小于 128MB 的 manifest 上限。
pub(crate) const HOST_HANDOFF_RESIDUAL_LIMIT: usize = 256 * 1024;
const HANDOFF_READER_STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// 交接时读端的停手与恢复。
///
/// 读线程停手后不退出，原地等：交接提交后进程退出，它跟着结束；回滚了就从停手处
/// 接着读；句柄关掉才退出。它一退出镜像通道就关了，会话的 drain 会当成宿主断线，
/// 在交接把控制 fd 发出去之前就拆掉宿主、关掉这个 fd。
struct ReaderHandoff {
    stop: AtomicBool,
    park: Mutex<ParkedReader>,
    cv: Condvar,
}

#[derive(Default)]
struct ParkedReader {
    parked: bool,
    /// 停手时读端已经从内核取走的字节，交接取走交给继任者。
    handed: Option<Vec<u8>>,
    /// 同一份字节留一份：回滚后读端先读它们，再接着读 socket。
    resume_prefix: Option<Vec<u8>>,
    closed: bool,
}

impl ReaderHandoff {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            stop: AtomicBool::new(false),
            park: Mutex::new(ParkedReader::default()),
            cv: Condvar::new(),
        })
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn park(&self, bytes: Vec<u8>) {
        let mut park = self.park.lock().unwrap();
        if !park.parked {
            park.parked = true;
            park.handed = Some(bytes.clone());
            park.resume_prefix = Some(bytes);
            self.cv.notify_all();
        }
    }

    /// 停手的读线程在这里等。回滚了返回要先读的字节；句柄关掉返回 `None`。
    fn wait_for_resume(&self) -> Option<Vec<u8>> {
        let mut park = self.park.lock().unwrap();
        loop {
            if park.closed {
                return None;
            }
            if !self.stopped() {
                park.parked = false;
                park.handed = None;
                return Some(park.resume_prefix.take().unwrap_or_default());
            }
            park = self.cv.wait(park).unwrap();
        }
    }

    /// 交接回滚：读端没停下就接着跑，停下了就醒来接着读。
    fn resume(&self) {
        let _park = self.park.lock().unwrap();
        self.stop.store(false, Ordering::SeqCst);
        self.cv.notify_all();
    }

    fn close(&self) {
        let mut park = self.park.lock().unwrap();
        park.closed = true;
        self.cv.notify_all();
    }
}

/// 继任者先读前任已经从内核取走的字节，再读 socket。
/// 这些字节若留在被丢掉的 `BufReader` 里，下一次读取会从一行的中间开始。
struct PrefixedRead<R> {
    prefix: Vec<u8>,
    pos: usize,
    inner: R,
}

impl<R> PrefixedRead<R> {
    fn new(prefix: Vec<u8>, inner: R) -> Self {
        Self {
            prefix,
            pos: 0,
            inner,
        }
    }

    fn into_inner(self) -> R {
        self.inner
    }

    fn take_unread_prefix(&mut self) -> Vec<u8> {
        let rest = self.prefix[self.pos..].to_vec();
        self.pos = self.prefix.len();
        rest
    }
}

impl<R: Read> Read for PrefixedRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos < self.prefix.len() {
            let n = (self.prefix.len() - self.pos).min(buf.len());
            buf[..n].copy_from_slice(&self.prefix[self.pos..self.pos + n]);
            self.pos += n;
            return Ok(n);
        }
        self.inner.read(buf)
    }
}

fn is_read_poll(error: &std::io::Error) -> bool {
    // macOS 上 SO_RCVTIMEO 到期经常报 WouldBlock，而不是 TimedOut。
    matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock)
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

/// 已经从流里取出、还没解析的字节。顺序是：读循环攒下的半行、BufReader 预取、
/// 还没被 BufReader 抽走的行首前缀。不能用 `read_line`：超时切在 UTF-8 字符中间时，
/// `String` 会把已经读走的字节丢掉。
fn take_residual(reader: &mut BufReader<PrefixedRead<UnixStream>>, pending: &[u8]) -> Vec<u8> {
    let mut out = pending.to_vec();
    let buffered = reader.buffer().to_vec();
    reader.consume(buffered.len());
    out.extend_from_slice(&buffered);
    out.extend(reader.get_mut().take_unread_prefix());
    out
}

fn stop_reader(
    handoff: &ReaderHandoff,
    reader: &mut BufReader<PrefixedRead<UnixStream>>,
    pending: &mut Vec<u8>,
) {
    let residual = take_residual(reader, pending);
    let complete = pending.last() == Some(&b'\n');
    pending.clear();
    if complete && residual.len() > HOST_HANDOFF_RESIDUAL_LIMIT {
        // 完整行本身已经对齐。超长的那一行不进清单；行尾之后的预取最多 8KB，
        // 继任者从下一行接着读，并会另要一份全量快照。
        let line_len = residual.iter().position(|byte| *byte == b'\n').unwrap_or(0) + 1;
        crate::dlog(&format!(
            "acp 交接丢弃过长的完整行（{line_len} 字节），只交接读缓冲里的后续字节"
        ));
        let mut rest = residual;
        rest.drain(..line_len.min(rest.len()));
        handoff.park(rest);
        return;
    }
    handoff.park(residual);
}

/// 交接停手：交出已取走的字节，原地等。回滚了就用同一条 socket 重建读端，先读
/// 那些字节；句柄关掉返回 `None`，读线程退出。
fn pause_reader(
    handoff: &ReaderHandoff,
    mut reader: BufReader<PrefixedRead<UnixStream>>,
    pending: &mut Vec<u8>,
) -> Option<BufReader<PrefixedRead<UnixStream>>> {
    stop_reader(handoff, &mut reader, pending);
    let prefix = handoff.wait_for_resume()?;
    let stream = reader.into_inner().into_inner();
    Some(BufReader::with_capacity(
        8192,
        PrefixedRead::new(prefix, stream),
    ))
}

fn reap_host_tasks(host_pid: i32) {
    if host_pid > 0 {
        smelt_core::background_tasks::reap_orphaned_task_groups(host_pid as u32);
    }
}

fn dispatch_snapshot_line(
    line: &[u8],
    tx: &smol::channel::Sender<HostedSnapshotEnvelope>,
    provider_pid: &AtomicI32,
    history_waiters: &Mutex<
        HashMap<u64, mpsc::SyncSender<smelt_core::acp_session::ConversationSnapshot>>,
    >,
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
    stream: UnixStream,
    prefix: Vec<u8>,
    handoff: Arc<ReaderHandoff>,
    provider_pid: Arc<AtomicI32>,
    host_pid: i32,
    history_waiters: Arc<
        Mutex<HashMap<u64, mpsc::SyncSender<smelt_core::acp_session::ConversationSnapshot>>>,
    >,
) -> std::io::Result<smol::channel::Receiver<HostedSnapshotEnvelope>> {
    stream.set_read_timeout(Some(SNAPSHOT_READ_POLL))?;
    let (tx, rx) = smol::channel::unbounded();
    thread::Builder::new()
        .name("smelt-acp-host-snapshots".to_string())
        .spawn(move || {
            let mut reader = BufReader::with_capacity(8192, PrefixedRead::new(prefix, stream));
            let mut pending: Vec<u8> = Vec::new();
            loop {
                // 半行还没超限就可以立刻停。超限的半行继续读到换行，避免把残行交出去。
                if handoff.stopped() && pending.len() <= HOST_HANDOFF_RESIDUAL_LIMIT {
                    match pause_reader(&handoff, reader, &mut pending) {
                        Some(resumed) => {
                            reader = resumed;
                            continue;
                        }
                        None => return,
                    }
                }
                let mut chunk = Vec::new();
                match reader.read_until(b'\n', &mut chunk) {
                    Err(error) if error.kind() == ErrorKind::Interrupted => {
                        pending.extend_from_slice(&chunk);
                    }
                    Err(error) if is_read_poll(&error) => {
                        pending.extend_from_slice(&chunk);
                        if handoff.stopped() && pending.len() <= HOST_HANDOFF_RESIDUAL_LIMIT {
                            match pause_reader(&handoff, reader, &mut pending) {
                                Some(resumed) => {
                                    reader = resumed;
                                    continue;
                                }
                                None => return,
                            }
                        }
                    }
                    Ok(0) => {
                        // 超时会把半行留在 pending 里。随后的 EOF 这次读不到新字节，
                        // 但 pending 仍是最后一行，不能直接丢掉。
                        if handoff.stopped() {
                            match pause_reader(&handoff, reader, &mut pending) {
                                Some(resumed) => {
                                    reader = resumed;
                                    continue;
                                }
                                None => return,
                            }
                        }
                        if !pending.is_empty()
                            && !dispatch_snapshot_line(
                                &pending,
                                &tx,
                                &provider_pid,
                                &history_waiters,
                            )
                        {
                            return;
                        }
                        reap_host_tasks(host_pid);
                        return;
                    }
                    Ok(_) => {
                        pending.extend_from_slice(&chunk);
                        let at_eof = pending.last() != Some(&b'\n');
                        if handoff.stopped() {
                            match pause_reader(&handoff, reader, &mut pending) {
                                Some(resumed) => {
                                    reader = resumed;
                                    continue;
                                }
                                None => return,
                            }
                        }
                        if at_eof {
                            if !pending.is_empty()
                                && !dispatch_snapshot_line(
                                    &pending,
                                    &tx,
                                    &provider_pid,
                                    &history_waiters,
                                )
                            {
                                return;
                            }
                            reap_host_tasks(host_pid);
                            return;
                        }
                        if !dispatch_snapshot_line(&pending, &tx, &provider_pid, &history_waiters) {
                            return;
                        }
                        pending.clear();
                    }
                    Err(_) => {
                        // 读端关掉才说明这条连接没了。解析失败不走这里：宿主还活着。
                        if handoff.stopped() {
                            // 回滚后读端接着读，同一个错误会再来一次，按断线收尾。
                            match pause_reader(&handoff, reader, &mut pending) {
                                Some(resumed) => {
                                    reader = resumed;
                                    continue;
                                }
                                None => return,
                            }
                        }
                        reap_host_tasks(host_pid);
                        return;
                    }
                }
            }
        })?;
    Ok(rx)
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

#[cfg(test)]
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
            daemon_side,
            Vec::new(),
            ReaderHandoff::new(),
            Arc::new(AtomicI32::new(0)),
            0,
            Arc::new(Mutex::new(std::collections::HashMap::new())),
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

    fn hosted_line(title: &str) -> Vec<u8> {
        let mut snapshot = smelt_core::acp_session::AcpSessionState::default().to_snapshot(false);
        snapshot.session_title = Some(title.to_string());
        let mut bytes = serde_json::to_vec(&serde_json::json!({ "snapshot": snapshot })).unwrap();
        bytes.push(b'\n');
        bytes
    }

    /// 回归：交接时前任 BufReader 已经预取了下一行的前缀。若继任者从 socket 中间接着读，
    /// 残行会被判定成协议不兼容，镜像永久断开，历史页就一直超时。
    /// 前任必须停手，并且不再读取停手之后才到达的字节。
    #[test]
    fn handoff_prefixes_the_line_the_previous_reader_already_took() {
        let (handle, mut peer) = HostedConversationHandle::test_stub();
        peer.write_all(&hosted_line("before")).unwrap();
        let first = smol::block_on(smol::future::race(
            async { handle.snapshot_rx().recv().await.ok() },
            async {
                smol::Timer::after(Duration::from_secs(2)).await;
                None
            },
        ))
        .expect("第一行完整快照应先被前任读端解析");
        assert_eq!(first.snapshot.session_title.as_deref(), Some("before"));

        let line = hosted_line("after");
        let prefix_len = line.len() / 2;
        assert!(prefix_len > 0 && line[prefix_len - 1] != b'\n');
        peer.write_all(&line[..prefix_len]).unwrap();

        let residual = handle.take_for_handoff().expect("读端应在交接前停下来");
        assert!(
            line[..prefix_len].starts_with(residual.as_slice()),
            "交接字节只能是已预取的半行前缀，实际 {} 字节",
            residual.len()
        );

        // 停手之后才写后半行。旧读端若还在读，会把这半行吃掉，继任者就拼不回来。
        peer.write_all(&line[prefix_len..]).unwrap();
        let stream = handle.clone_control().expect("控制连接应还在");
        let rx = start_snapshot_reader(
            stream,
            residual,
            ReaderHandoff::new(),
            Arc::new(AtomicI32::new(0)),
            0,
            Arc::new(Mutex::new(std::collections::HashMap::new())),
        )
        .unwrap();
        let second = smol::block_on(smol::future::race(async { rx.recv().await.ok() }, async {
            smol::Timer::after(Duration::from_secs(2)).await;
            None
        }))
        .expect("继任读端应把半行拼回完整快照");
        assert_eq!(second.snapshot.session_title.as_deref(), Some("after"));
        assert!(
            handle.snapshot_rx().try_recv().is_err(),
            "旧读端停手后不能再收到后半行"
        );
    }

    /// 回归：交接先停读端再发 fd。停手的读线程一退出，镜像通道就关了，会话的
    /// drain 把它当成宿主断线，抢在发 fd 之前关掉控制连接。交接报 EBADF 回滚，
    /// 会话被拆掉重开；守护每分钟重试一次交接，界面就每分钟重连一次。
    /// 停手期间通道必须保持打开；回滚后原读端从停手处接着读。
    #[test]
    fn rolled_back_handoff_resumes_the_paused_reader_without_closing_the_mirror() {
        let (handle, mut peer) = HostedConversationHandle::test_stub();
        let rx = handle.snapshot_rx();
        let line = hosted_line("after");
        let prefix_len = line.len() / 2;
        peer.write_all(&line[..prefix_len]).unwrap();

        let _residual = handle.take_for_handoff().expect("读端应在交接前停下来");
        std::thread::sleep(SNAPSHOT_READ_POLL * 3);
        assert!(
            !rx.is_closed(),
            "交接期间读端只是停手，镜像通道不能关，否则会话会把宿主当成断线拆掉"
        );

        handle.resume_after_handoff_rollback();
        peer.write_all(&line[prefix_len..]).unwrap();
        let resumed = smol::block_on(smol::future::race(async { rx.recv().await.ok() }, async {
            smol::Timer::after(Duration::from_secs(2)).await;
            None
        }))
        .expect("回滚后原读端接着读，把半行拼回完整快照");
        assert_eq!(resumed.snapshot.session_title.as_deref(), Some("after"));
    }
}
