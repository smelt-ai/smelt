//! 交接 v2 传输：socketpair 私有通道 + `SCM_RIGHTS` 传 fd + 行帧握手。
//!
//! 线协议（全小端）：
//! ```text
//! P→S  MANIFEST  MAGIC(4) + ver(u32) + len(u64) + sha256(32) + json[len]
//! P→S  FDS       1+ sendmsg(SCM_RIGHTS)，顺序 = manifest.fd_roles 声明顺序
//! P→S  GRIDS     逐个 len(u64) + bytes（并发读写，无死锁；各自校验）
//! S→P  READY {"restored_terminals":n,"restored_acp":m,"dropped_grids":[]} | ABORT {reason}
//! P→S  COMMIT | ABANDON（行 JSON）；EOF 语义见 [`CommitDecision`]
//! ```
//!
//! 超时（GUI 侧 upgrade 读超时 30s 全覆盖，无需改 GUI）：
//! - predecessor 等 READY：15s；
//! - successor 等 COMMIT：15s。
//!
//! 安全边界：声明长度先过 cap 再分配（防损坏长度 OOM）；收到的 fd 立即
//! 补 CLOEXEC（`SCM_RIGHTS` 新描述符默认不带，macOS 无 MSG_CMSG_CLOEXEC）。

use super::manifest::{
    GridRef, HANDOFF_FRAME_HEADER_LEN, HandoffManifest, decode_frame, encode_frame, sha256_hex,
};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// predecessor 等 successor READY 的预算：socketpair 传 MB 级 grid + 恢复
/// 会话正常在 1s 内完成，15s 是慷慨上限。
pub const READY_TIMEOUT: Duration = Duration::from_secs(15);
/// successor 等 predecessor COMMIT 的预算。
pub const COMMIT_TIMEOUT: Duration = Duration::from_secs(15);
/// 握手行已就绪后的读 backstop：行分片到达时防永久阻塞。
const HANDSHAKE_READ_BACKSTOP: Duration = Duration::from_secs(5);
/// manifest 声明长度上限：含 ACP 快照也远到不了 MB 级，128MB 纯防损坏长度。
pub const MAX_MANIFEST_BYTES: u64 = 128 * 1024 * 1024;
/// 单个 grid 上限：实测 400KB 级，64MB 纯防损坏长度。
pub const MAX_GRID_BYTES: u64 = 64 * 1024 * 1024;
/// 单次交接 fd 总数上限：会话数 × 每会话 1~2 个，到不了千级。
pub const MAX_FDS: usize = 4096;
/// 单条 sendmsg 最多塞的 fd 数：远低于各平台 SCM_RIGHTS 上限（通常 253）。
const FDS_PER_MESSAGE: usize = 128;

#[derive(Debug)]
pub enum TransportError {
    Io(std::io::Error),
    ManifestTooLarge(u64),
    TooManyFds(usize),
    Frame(super::manifest::FrameError),
    PeerAbort(String),
    Eof(&'static str),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Io(error) => write!(f, "交接通道 IO 失败：{error}"),
            TransportError::ManifestTooLarge(len) => {
                write!(f, "manifest 声明长度 {len} 超过上限 {MAX_MANIFEST_BYTES}")
            }
            TransportError::TooManyFds(n) => write!(f, "fd 数量 {n} 超过上限 {MAX_FDS}"),
            TransportError::Frame(error) => write!(f, "manifest 帧非法：{error}"),
            TransportError::PeerAbort(reason) => write!(f, "对端 ABORT：{reason}"),
            TransportError::Eof(what) => write!(f, "等{what}时对端已断开"),
        }
    }
}

impl std::error::Error for TransportError {}

impl From<std::io::Error> for TransportError {
    fn from(error: std::io::Error) -> Self {
        TransportError::Io(error)
    }
}

pub fn socketpair() -> std::io::Result<(UnixStream, UnixStream)> {
    UnixStream::pair()
}

// ---- P→S：MANIFEST ----

pub fn send_manifest(sock: &UnixStream, manifest: &HandoffManifest) -> Result<(), TransportError> {
    let frame = encode_frame(manifest).map_err(|error| {
        TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("manifest 序列化失败：{error}"),
        ))
    })?;
    let mut sock = sock;
    sock.write_all(&frame)?;
    sock.flush()?;
    Ok(())
}

pub fn recv_manifest(sock: &UnixStream) -> Result<HandoffManifest, TransportError> {
    let mut header = [0u8; HANDOFF_FRAME_HEADER_LEN];
    read_exact_or_eof(sock, &mut header, "manifest 头")?;
    // 先验长度 cap 再分配：声明 2^64-1 时不许真的去 alloc。
    let declared = u64::from_le_bytes(header[8..16].try_into().expect("8 字节长度"));
    if declared > MAX_MANIFEST_BYTES {
        return Err(TransportError::ManifestTooLarge(declared));
    }
    let len: usize = declared.try_into().unwrap_or(usize::MAX);
    let mut body = vec![0u8; len];
    read_exact_or_eof(sock, &mut body, "manifest 体")?;
    let mut full = Vec::with_capacity(HANDOFF_FRAME_HEADER_LEN + len);
    full.extend_from_slice(&header);
    full.extend_from_slice(&body);
    let (manifest, _) = decode_frame(&full).map_err(TransportError::Frame)?;
    Ok(manifest)
}

fn read_exact_or_eof(
    sock: &UnixStream,
    mut buf: &mut [u8],
    what: &'static str,
) -> Result<(), TransportError> {
    while !buf.is_empty() {
        let mut sock = sock;
        match sock.read(buf) {
            Ok(0) => return Err(TransportError::Eof(what)),
            Ok(n) => buf = &mut buf[n..],
            Err(error) => return Err(TransportError::Io(error)),
        }
    }
    Ok(())
}

// ---- P→S：FDS（SCM_RIGHTS） ----

/// 经 `SCM_RIGHTS` 发送 fd（dup 份，发送方原件保留到 commit）。
/// 每条消息带 1 字节哑 payload：裸 control、无 data 的 sendmsg 在部分
/// 平台行为不一致，1 字节是最稳的形状。
pub fn send_fds(sock: &UnixStream, fds: &[std::os::fd::RawFd]) -> Result<(), TransportError> {
    if fds.len() > MAX_FDS {
        return Err(TransportError::TooManyFds(fds.len()));
    }
    for chunk in fds.chunks(FDS_PER_MESSAGE) {
        send_fds_one_message(sock.as_raw_fd(), chunk)?;
    }
    Ok(())
}

fn send_fds_one_message(
    sock_fd: std::os::fd::RawFd,
    fds: &[std::os::fd::RawFd],
) -> std::io::Result<()> {
    let dummy: [u8; 1] = [0x46];
    let mut iov = libc::iovec {
        iov_base: dummy.as_ptr() as *mut libc::c_void,
        iov_len: dummy.len(),
    };
    let cmsg_len = unsafe { libc::CMSG_SPACE(std::mem::size_of_val(fds) as libc::c_uint) as usize };
    let mut cmsg_buf = vec![0u8; cmsg_len];
    let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
    header.msg_iov = &mut iov;
    header.msg_iovlen = 1;
    header.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    header.msg_controllen = cmsg_buf.len() as libc::socklen_t;
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&header);
        if cmsg.is_null() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "CMSG_FIRSTHDR 为空",
            ));
        }
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len =
            libc::CMSG_LEN(std::mem::size_of_val(fds) as libc::c_uint) as libc::socklen_t;
        std::ptr::copy_nonoverlapping(
            fds.as_ptr(),
            libc::CMSG_DATA(cmsg) as *mut std::os::fd::RawFd,
            fds.len(),
        );
        header.msg_controllen = (*cmsg).cmsg_len as libc::socklen_t;
        let sent = libc::sendmsg(sock_fd, &header, 0);
        if sent < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// 收齐 `expect` 个 fd。返回顺序 = 发送顺序 = manifest.fd_roles 声明顺序。
/// 收到的每个 fd 立即补 CLOEXEC（平时所有会话 fd 都带此标记，不泄漏给
/// 后续 spawn 的子进程；adopt 后沿用同一条纪律）。
pub fn recv_fds(sock: &UnixStream, expect: usize) -> Result<Vec<OwnedFd>, TransportError> {
    if expect > MAX_FDS {
        return Err(TransportError::TooManyFds(expect));
    }
    let mut out = Vec::with_capacity(expect);
    while out.len() < expect {
        let mut batch = recv_fds_one_message(sock.as_raw_fd())?;
        if batch.is_empty() {
            return Err(TransportError::Eof("fd 交接"));
        }
        for fd in batch.drain(..) {
            set_cloexec(fd.as_raw_fd(), true)?;
            out.push(fd);
            if out.len() == expect {
                break;
            }
        }
        if out.len() < expect && batch.is_empty() {
            // 本条消息已无 control 但数量未齐：继续收下一条。
        }
    }
    Ok(out)
}

fn recv_fds_one_message(sock_fd: std::os::fd::RawFd) -> std::io::Result<Vec<OwnedFd>> {
    let mut dummy = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: dummy.as_mut_ptr() as *mut libc::c_void,
        iov_len: dummy.len(),
    };
    let cmsg_len = unsafe {
        libc::CMSG_SPACE((FDS_PER_MESSAGE * size_of::<std::os::fd::RawFd>()) as libc::c_uint)
            as usize
    };
    let mut cmsg_buf = vec![0u8; cmsg_len];
    let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
    header.msg_iov = &mut iov;
    header.msg_iovlen = 1;
    header.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    header.msg_controllen = cmsg_buf.len() as libc::socklen_t;
    let received = unsafe { libc::recvmsg(sock_fd, &mut header, 0) };
    if received < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if received == 0 {
        return Ok(Vec::new()); // EOF：对端已关闭，无 control。
    }
    let mut out = Vec::new();
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&header);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let data_len = (*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                let fd_count = data_len / size_of::<std::os::fd::RawFd>();
                let data = libc::CMSG_DATA(cmsg) as *const std::os::fd::RawFd;
                for i in 0..fd_count {
                    let fd = *data.add(i);
                    if fd >= 0 {
                        out.push(OwnedFd::from_raw_fd(fd));
                    }
                }
            }
            cmsg = libc::CMSG_NXTHDR(&header, cmsg);
        }
    }
    Ok(out)
}

fn set_cloexec(fd: std::os::fd::RawFd, cloexec: bool) -> std::io::Result<()> {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let flags = if cloexec {
            flags | libc::FD_CLOEXEC
        } else {
            flags & !libc::FD_CLOEXEC
        };
        if libc::fcntl(fd, libc::F_SETFD, flags) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

// ---- P→S：GRIDS（best-effort，逐个校验） ----

/// 发送 grid 字节：顺序必须与 manifest.grids 一致。
/// `blobs[i]` 对应 `refs[i]`；调用方保证字节与 `refs[i].sha256` 一致。
pub fn send_grids(
    sock: &UnixStream,
    refs: &[GridRef],
    blobs: &[Vec<u8>],
) -> Result<(), TransportError> {
    assert_eq!(refs.len(), blobs.len(), "grid 引用与字节必须一一对应");
    let mut sock = sock;
    for blob in blobs {
        sock.write_all(&(blob.len() as u64).to_le_bytes())?;
        sock.write_all(blob)?;
    }
    sock.flush()?;
    Ok(())
}

/// 接收 grid：逐个校验 sha/len。**单个失败只丢该 grid**（会话走"无 grid
/// 空 Term + jolt"），绝不 ABORT 整个事务——这就是 BEST-EFFORT 分级的含义。
/// 帧定界永远以线上的实际 len 为准：声明与实际不符时仍按实际消费，保证
/// 流不錯位、后续 grid 不受牵连。
pub fn recv_grids(sock: &UnixStream, refs: &[GridRef]) -> Vec<RecvGrid> {
    let mut out = Vec::with_capacity(refs.len());
    for reference in refs {
        out.push(recv_one_grid(sock, reference));
    }
    out
}

#[derive(Debug)]
pub struct RecvGrid {
    pub session_id: String,
    pub result: Result<Vec<u8>, GridError>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum GridError {
    Eof,
    TooLarge(u64),
    LengthMismatch { declared: u64, actual: u64 },
    ChecksumMismatch,
    Io(String),
}

impl std::fmt::Display for GridError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GridError::Eof => write!(f, "grid 字节流提前结束"),
            GridError::TooLarge(len) => write!(f, "grid 长度 {len} 超过上限"),
            GridError::LengthMismatch { declared, actual } => {
                write!(f, "grid 长度声明 {declared} 实到 {actual}")
            }
            GridError::ChecksumMismatch => write!(f, "grid sha256 校验失败"),
            GridError::Io(error) => write!(f, "grid 读取失败：{error}"),
        }
    }
}

/// grid 读错映射：EOF 与 IO 错误分开——BEST-EFFORT 通道里两者都只丢
/// 该 grid，但 dlog 要能区分"对端死了"和"读超时/坏了"。
fn grid_read_error(error: TransportError) -> GridError {
    match error {
        TransportError::Eof(_) => GridError::Eof,
        TransportError::Io(error) => GridError::Io(error.to_string()),
        other => GridError::Io(other.to_string()),
    }
}

fn recv_one_grid(sock: &UnixStream, reference: &GridRef) -> RecvGrid {
    let session_id = reference.session_id.clone();
    let fail = |result: GridError| RecvGrid {
        session_id: session_id.clone(),
        result: Err(result),
    };
    let mut len_buf = [0u8; 8];
    if let Err(error) = read_exact_or_eof(sock, &mut len_buf, "grid 长度") {
        return fail(grid_read_error(error));
    }
    let actual = u64::from_le_bytes(len_buf);
    if actual > MAX_GRID_BYTES {
        return fail(GridError::TooLarge(actual));
    }
    let len: usize = actual.try_into().unwrap_or(usize::MAX);
    let mut blob = vec![0u8; len];
    if let Err(error) = read_exact_or_eof(sock, &mut blob, "grid 字节") {
        return fail(grid_read_error(error));
    }
    if actual != reference.len {
        return fail(GridError::LengthMismatch {
            declared: reference.len,
            actual,
        });
    }
    if sha256_hex(&blob) != reference.sha256 {
        return fail(GridError::ChecksumMismatch);
    }
    RecvGrid {
        session_id,
        result: Ok(blob),
    }
}

// ---- S→P / P→S：握手 ----

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadyInfo {
    pub restored_terminals: usize,
    pub restored_acp: usize,
    pub dropped_grids: Vec<String>,
}

pub fn send_ready(sock: &UnixStream, info: &ReadyInfo) -> std::io::Result<()> {
    let mut sock = sock;
    writeln!(
        sock,
        "{}",
        serde_json::json!({
            "status": "ready",
            "restored_terminals": info.restored_terminals,
            "restored_acp": info.restored_acp,
            "dropped_grids": info.dropped_grids,
        })
    )?;
    sock.flush()
}

pub fn send_abort(sock: &UnixStream, reason: &str) -> std::io::Result<()> {
    let mut sock = sock;
    writeln!(
        sock,
        "{}",
        serde_json::json!({ "status": "abort", "reason": reason })
    )?;
    sock.flush()
}

/// 可读性等待：true=可读或对端已关闭，false=超时。
///
/// 不用 `SO_RCVTIMEO` 做主超时：macOS 上对端关闭后 setsockopt(SO_RCVTIMEO)
/// 直接失败（实测），而握手等待恰恰要在"对端可能已死"时给出正确方向
/// （READY 前死=失败，COMMIT 前死=接管）。`poll` 无此 quirk。
fn poll_readable(fd: std::os::fd::RawFd, timeout: Duration) -> std::io::Result<bool> {
    let millis: libc::c_int = timeout.as_millis().try_into().unwrap_or(libc::c_int::MAX);
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    loop {
        let ready = unsafe { libc::poll(&mut pfd, 1, millis) };
        if ready > 0 {
            return Ok(true);
        }
        if ready == 0 {
            return Ok(false);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(error);
    }
}

fn timed_out(what: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, format!("等{what}超时"))
}

/// predecessor 等 READY：15s 超时；ABORT/EOF/超时一律视为事务失败→回滚。
pub fn await_ready(sock: &UnixStream) -> Result<ReadyInfo, TransportError> {
    await_ready_with_timeout(sock, READY_TIMEOUT)
}

fn await_ready_with_timeout(
    sock: &UnixStream,
    timeout: Duration,
) -> Result<ReadyInfo, TransportError> {
    if !poll_readable(sock.as_raw_fd(), timeout)? {
        return Err(TransportError::Io(timed_out("READY")));
    }
    // backstop 失败忽略：大概率对端已关闭，读操作会直接给出 EOF。
    let _ = sock.set_read_timeout(Some(HANDSHAKE_READ_BACKSTOP));
    let mut line = String::new();
    let mut reader = BufReader::new(sock);
    match reader.read_line(&mut line) {
        Ok(0) => return Err(TransportError::Eof("READY")),
        Ok(_) => {}
        Err(error) => return Err(TransportError::Io(error)),
    }
    let value: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|_| TransportError::Eof("READY 解析"))?;
    match value["status"].as_str() {
        Some("ready") => Ok(ReadyInfo {
            restored_terminals: value["restored_terminals"].as_u64().unwrap_or(0) as usize,
            restored_acp: value["restored_acp"].as_u64().unwrap_or(0) as usize,
            dropped_grids: value["dropped_grids"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        }),
        Some("abort") => Err(TransportError::PeerAbort(
            value["reason"].as_str().unwrap_or("未说明").to_string(),
        )),
        _ => Err(TransportError::Eof("READY 解析")),
    }
}

pub fn send_commit(sock: &UnixStream) -> std::io::Result<()> {
    let mut sock = sock;
    writeln!(sock, "{}", serde_json::json!({ "decision": "commit" }))?;
    sock.flush()
}

pub fn send_abandon(sock: &UnixStream) -> std::io::Result<()> {
    let mut sock = sock;
    writeln!(sock, "{}", serde_json::json!({ "decision": "abandon" }))?;
    sock.flush()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitDecision {
    /// predecessor 明确提交：接管并开始服务。
    Commit,
    /// predecessor 明确放弃：安静退出。
    Abandon,
    /// 等 COMMIT 时读到 EOF：predecessor 已死但 successor 活着——必须有人
    /// 服务，接管（predecessor 的 listen 副本随死亡关闭，无双主）。
    PredecessorGone,
    /// 15s 无回应且对端活着：predecessor 卡住，successor 退出，
    /// predecessor 侧发 COMMIT 会吃 EPIPE 走回滚。绝不擅自服务。
    Timeout,
}

/// successor 等 COMMIT。注意与 [`await_ready`] 的不对称：这里的 EOF 是接管
/// 信号（见 [`CommitDecision::PredecessorGone`]），不是失败。
pub fn await_commit(sock: &UnixStream) -> CommitDecision {
    await_commit_with_timeout(sock, COMMIT_TIMEOUT)
}

fn await_commit_with_timeout(sock: &UnixStream, timeout: Duration) -> CommitDecision {
    match poll_readable(sock.as_raw_fd(), timeout) {
        Ok(true) => {}
        Ok(false) => return CommitDecision::Timeout,
        Err(_) => return CommitDecision::PredecessorGone,
    }
    // backstop 失败忽略：大概率对端已关闭，读操作会直接给出 EOF。
    let _ = sock.set_read_timeout(Some(HANDSHAKE_READ_BACKSTOP));
    let mut line = String::new();
    let mut reader = BufReader::new(sock);
    match reader.read_line(&mut line) {
        Ok(0) => return CommitDecision::PredecessorGone,
        Ok(_) => {}
        // poll 已就绪后仍读超时：虚假唤醒 + 对端沉默。按 Timeout（退出）而非
        // 接管处理——predecessor 活着时擅自服务会双主；它随后发 COMMIT 会吃
        // EPIPE 走自己的回滚，无 wedge。
        Err(error)
            if error.kind() == std::io::ErrorKind::WouldBlock
                || error.kind() == std::io::ErrorKind::TimedOut =>
        {
            return CommitDecision::Timeout;
        }
        Err(_) => return CommitDecision::PredecessorGone,
    }
    let value: serde_json::Value = match serde_json::from_str(line.trim()) {
        Ok(value) => value,
        Err(_) => return CommitDecision::Abandon,
    };
    match value["decision"].as_str() {
        Some("commit") => CommitDecision::Commit,
        Some("abandon") => CommitDecision::Abandon,
        // 乱行：保守放弃。predecessor 发 COMMIT 后就退出，乱行只可能来自
        // 通道损坏——此时擅自服务可能双主。
        _ => CommitDecision::Abandon,
    }
}

#[cfg(test)]
mod tests {
    use super::super::manifest::{
        HandoffManifest, ProducerInfo, TerminalChildHandoff, TerminalHandoff,
    };
    use super::*;
    use std::os::fd::IntoRawFd;

    fn tiny_manifest() -> HandoffManifest {
        HandoffManifest {
            producer: ProducerInfo {
                version: "0.9.0".to_string(),
                pid: 1,
            },
            snapshot_wall_ms: 0,
            fd_roles: vec![super::super::manifest::FdRole::Listen],
            sessions: vec![TerminalHandoff {
                id: "t1".to_string(),
                child: TerminalChildHandoff::Live { pid: 100 },
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

    #[test]
    fn manifest_frame_round_trips_over_socketpair() {
        // 注意：t1 Live 却无 master 角色——recv 侧 validate 会拒。
        // 回环测传输层 framing，这里先补齐角色。
        let mut manifest = tiny_manifest();
        manifest
            .fd_roles
            .push(super::super::manifest::FdRole::TerminalMaster {
                session_id: "t1".to_string(),
            });
        let (a, b) = socketpair().unwrap();
        send_manifest(&a, &manifest).unwrap();
        let decoded = recv_manifest(&b).unwrap();
        assert_eq!(
            serde_json::to_value(&decoded).unwrap(),
            serde_json::to_value(&manifest).unwrap()
        );
    }

    #[test]
    fn oversized_manifest_len_is_rejected_before_alloc() {
        let (mut a, b) = socketpair().unwrap();
        let mut header = Vec::new();
        header.extend_from_slice(super::super::manifest::HANDOFF_MAGIC);
        header.extend_from_slice(&super::super::manifest::HANDOFF_PROTO_VERSION.to_le_bytes());
        header.extend_from_slice(&u64::MAX.to_le_bytes());
        header.extend_from_slice(&[0u8; 32]);
        a.write_all(&header).unwrap();
        drop(a);
        assert!(matches!(
            recv_manifest(&b),
            Err(TransportError::ManifestTooLarge(_))
        ));
    }

    #[test]
    fn fds_pass_over_scm_rights_and_stay_usable() {
        let (a, b) = socketpair().unwrap();
        // 用 pipe 当"被交接的 fd"：发写端，收到的必须可写。
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let write_end = unsafe { OwnedFd::from_raw_fd(fds[1]) };

        send_fds(&a, &[write_end.as_raw_fd()]).unwrap();
        drop(write_end); // 原件关闭：收到的 dup 必须独立存活。
        let received = recv_fds(&b, 1).unwrap();
        assert_eq!(received.len(), 1);
        // CLOEXEC 纪律：收到的 fd 立即补标记。
        let flags = unsafe { libc::fcntl(received[0].as_raw_fd(), libc::F_GETFD) };
        assert!(flags & libc::FD_CLOEXEC != 0);

        let mut writer = unsafe {
            std::fs::File::from_raw_fd(received.into_iter().next().unwrap().into_raw_fd())
        };
        writer.write_all(b"ping").unwrap();
        drop(writer);
        let mut reader = unsafe { std::fs::File::from_raw_fd(read_end.into_raw_fd()) };
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ping");
    }

    #[test]
    fn fd_shortfall_is_an_error_not_a_hang() {
        let (a, b) = socketpair().unwrap();
        b.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        drop(a); // 对端直接消失：声明 2 个实到 0 个。
        let result = recv_fds(&b, 2);
        assert!(matches!(result, Err(TransportError::Eof(_))));
    }

    #[test]
    fn grids_round_trip_and_corrupt_grid_is_isolated() {
        let refs = vec![
            GridRef {
                session_id: "t1".to_string(),
                len: 5,
                sha256: sha256_hex(b"hello"),
            },
            GridRef {
                session_id: "t2".to_string(),
                len: 5,
                sha256: sha256_hex(b"world"),
            },
        ];
        let (a, b) = socketpair().unwrap();
        // 第二个故意发错字节：声明 world 实发 WORLD。
        send_grids(&a, &refs, &[b"hello".to_vec(), b"WORLD".to_vec()]).unwrap();
        let results = recv_grids(&b, &refs);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].result.as_deref(), Ok(b"hello".as_slice()));
        assert!(matches!(
            results[1].result,
            Err(GridError::ChecksumMismatch)
        ));
    }

    #[test]
    fn ready_abort_handshake() {
        let (a, b) = socketpair().unwrap();
        send_ready(
            &a,
            &ReadyInfo {
                restored_terminals: 3,
                restored_acp: 1,
                dropped_grids: vec!["t9".to_string()],
            },
        )
        .unwrap();
        let info = await_ready(&b).unwrap();
        assert_eq!(info.restored_terminals, 3);
        assert_eq!(info.dropped_grids, vec!["t9".to_string()]);

        let (a, b) = socketpair().unwrap();
        send_abort(&a, "nope").unwrap();
        assert!(matches!(await_ready(&b), Err(TransportError::PeerAbort(_))));

        // READY 前 EOF = 失败（与 COMMIT 前 EOF 的接管语义相反，见下）。
        let (a, b) = socketpair().unwrap();
        drop(a);
        assert!(matches!(await_ready(&b), Err(TransportError::Eof(_))));
    }

    #[test]
    fn commit_decision_matrix() {
        let (a, b) = socketpair().unwrap();
        send_commit(&a).unwrap();
        assert_eq!(await_commit(&b), CommitDecision::Commit);

        let (a, b) = socketpair().unwrap();
        send_abandon(&a).unwrap();
        assert_eq!(await_commit(&b), CommitDecision::Abandon);

        // COMMIT 前 EOF = predecessor 已死 = 接管信号。
        let (a, b) = socketpair().unwrap();
        drop(a);
        assert_eq!(await_commit(&b), CommitDecision::PredecessorGone);

        // 乱行保守放弃，绝不擅自服务。
        let (mut a, b) = socketpair().unwrap();
        a.write_all(b"{not json\n").unwrap();
        assert_eq!(await_commit(&b), CommitDecision::Abandon);
    }

    #[test]
    fn handshake_waits_time_out_without_hanging() {
        // 对端活着但沉默：短超时必须返回 Timeout/Err 而不是卡住。
        let (_a, b) = socketpair().unwrap();
        let fast = Duration::from_millis(50);
        assert!(await_ready_with_timeout(&b, fast).is_err());
        assert_eq!(await_commit_with_timeout(&b, fast), CommitDecision::Timeout);
    }
}
