//! Unix 的 AF_UNIX：读从不阻塞在内核的 recv 里，先 poll 等可读再非阻塞地读。
//!
//! macOS / iOS 的内核（xnu）有个竞态：阻塞的 recv 在 `soreceive` 里查完「对端关了写」
//! 准备睡下之前，要调一次 `uipc_rcvd`，它为了按地址顺序加锁会把本端的锁放掉一下。
//! 对端恰好在这个空当 `shutdown(SHUT_WR)`，置位和唤醒都落了空，接着检查的只有接收缓冲
//! 有没有数据，没有数据就睡下：EOF 其实已经到了，读却一直挂到读超时，没设超时就永远挂着。
//! 整条关闭不受影响（断开前会等 `UNP_DONTDISCONNECT`），只有半关闭会丢。iroh 桥接把远端
//! 写完转成本地半关闭，读这一端的客户端就会卡住。
//!
//! poll 在本端的锁里查状态并挂上等待，不经过 `uipc_rcvd`，没有这个空当。所以读先用
//! `MSG_DONTWAIT` 试，遇到 WouldBlock 就 poll 等可读，再试。Linux 没有这个竞态，但走同
//! 一条路，各 Unix 的读不分叉。写和其余方法直接用 std 的 `UnixStream`，读写超时、
//! `set_nonblocking` 都存在套接字上，`try_clone` 出来的各份自然共享，和 std 一致。

use std::fmt;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::os::unix::net;
use std::path::Path;
use std::time::{Duration, Instant};

/// 一条 AF_UNIX 连接。
pub struct UnixStream(net::UnixStream);

impl UnixStream {
    pub fn connect<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        net::UnixStream::connect(path).map(Self)
    }

    pub fn pair() -> io::Result<(Self, Self)> {
        let (a, b) = net::UnixStream::pair()?;
        Ok((Self(a), Self(b)))
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.0.shutdown(how)
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_read_timeout(timeout)
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_write_timeout(timeout)
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        self.0.read_timeout()
    }

    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        self.0.write_timeout()
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
    }

    fn read_bytes(&self, buf: &mut [u8]) -> io::Result<usize> {
        let fd = self.0.as_raw_fd();
        // 读超时从第一次要等时算起，和 SO_RCVTIMEO 一样。
        let mut deadline = None;
        loop {
            // SAFETY: buf 可写，长度如实传入。
            let read =
                unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), libc::MSG_DONTWAIT) };
            if read >= 0 {
                return Ok(read as usize);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::WouldBlock || is_nonblocking(fd)? {
                return Err(error);
            }
            if deadline.is_none() {
                deadline = Some(
                    self.0
                        .read_timeout()?
                        .map(|timeout| Instant::now() + timeout),
                );
            }
            if !wait_readable(fd, deadline.flatten())? {
                // 和 std 的读超时一样报 EAGAIN，调用方按 WouldBlock 认。
                return Err(io::Error::from_raw_os_error(libc::EAGAIN));
            }
        }
    }
}

/// 套接字是不是被 `set_nonblocking(true)` 过：是就照 std 的语义直接报 WouldBlock，不等。
fn is_nonblocking(fd: RawFd) -> io::Result<bool> {
    // SAFETY: 只读描述符的状态标志。
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(flags & libc::O_NONBLOCK != 0)
}

/// 等到可读（含对端关闭、出错，交给接下来的 recv 去报）返回 `true`，到了期限返回 `false`。
/// 本地 `shutdown` 了读方向也算可读，等着的读随之读到 EOF。
fn wait_readable(fd: RawFd, deadline: Option<Instant>) -> io::Result<bool> {
    loop {
        let timeout_ms = match deadline {
            None => -1,
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Ok(false);
                }
                // 向上取整到毫秒，免得差一点没到就醒来空转。
                left.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32
            }
        };
        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: 只传一个 pollfd，指针在调用期间有效。
        match unsafe { libc::poll(&mut pollfd, 1, timeout_ms) } {
            -1 => {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
            0 => {}
            _ => return Ok(true),
        }
    }
}

impl fmt::Debug for UnixStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl AsRawFd for UnixStream {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl AsFd for UnixStream {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

/// 交给 tokio 之类要 std 类型的地方（`tokio::net::UnixStream::from_std`）。
impl From<UnixStream> for net::UnixStream {
    fn from(stream: UnixStream) -> Self {
        stream.0
    }
}

impl Read for UnixStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_bytes(buf)
    }
}

impl Read for &UnixStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_bytes(buf)
    }
}

impl Write for UnixStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&self.0).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Write for &UnixStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&self.0).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct UnixListener(net::UnixListener);

impl UnixListener {
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        net::UnixListener::bind(path).map(Self)
    }

    pub fn incoming(&self) -> impl Iterator<Item = io::Result<UnixStream>> + '_ {
        self.0.incoming().map(|stream| stream.map(UnixStream))
    }
}

impl fmt::Debug for UnixListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl AsRawFd for UnixListener {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}
