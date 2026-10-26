//! 守护接受的客户端连接。
//!
//! Rust 客户端走 AF_UNIX（`smeltd.sock`）。Windows 上 Bun / Node 的 `net.connect(path)`
//! 连的是命名管道，不是这个文件。两条监听各自 accept，交给同一个连接处理函数。
//! 不把管道桥进 AF_UNIX：桥的对端会变成 smeltd 自己，事件鉴权会拒绝。

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::time::Duration;

use smelt_runtime_client::UnixStream;

#[cfg(unix)]
pub type ConnId = std::os::unix::io::RawFd;
/// 套接字句柄和管道连接的序号是两个各自的号段，混成一个整数会撞号。
#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConnId {
    Socket(u64),
    Pipe(u64),
}

/// 一条已经接受的客户端连接。协议代码只依赖 [`ClientSocket`]。
pub enum ClientConn {
    Unix(UnixStream),
    /// `id` 只用来在本进程里认出这条连接（确认字节、摘掉 attachment）。
    /// 管道没有可以和 AF_UNIX 对齐的文件描述符。
    #[cfg(windows)]
    Pipe {
        stream: smelt_sys::named_pipe::PipeStream,
        id: ConnId,
    },
}

impl ClientConn {
    pub fn from_unix(stream: UnixStream) -> Self {
        Self::Unix(stream)
    }

    #[cfg(windows)]
    pub fn from_pipe(stream: smelt_sys::named_pipe::PipeStream) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self::Pipe {
            stream,
            id: ConnId::Pipe(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)),
        }
    }
}

/// 守护协议用到的流操作。`UnixStream` 和 [`ClientConn`] 都实现它，测试仍可把
/// `UnixStream::pair()` 的一端直接交给处理函数。
pub trait ClientSocket: Read + Write + Send + Sized + 'static {
    fn try_clone(&self) -> io::Result<Self>;
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()>;
    fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()>;
    fn shutdown(&self, how: Shutdown) -> io::Result<()>;
    fn connection_id(&self) -> ConnId;
    fn into_client_conn(self) -> ClientConn;
}

impl ClientSocket for UnixStream {
    fn try_clone(&self) -> io::Result<Self> {
        UnixStream::try_clone(self)
    }

    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        UnixStream::set_read_timeout(self, timeout)
    }

    fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        UnixStream::set_write_timeout(self, timeout)
    }

    fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        UnixStream::shutdown(self, how)
    }

    fn connection_id(&self) -> ConnId {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            self.as_raw_fd()
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            ConnId::Socket(self.as_raw_socket())
        }
    }

    fn into_client_conn(self) -> ClientConn {
        ClientConn::Unix(self)
    }
}

impl ClientSocket for ClientConn {
    fn try_clone(&self) -> io::Result<Self> {
        match self {
            Self::Unix(stream) => Ok(Self::Unix(stream.try_clone()?)),
            #[cfg(windows)]
            Self::Pipe { stream, id } => Ok(Self::Pipe {
                stream: stream.try_clone()?,
                id: *id,
            }),
        }
    }

    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Unix(stream) => stream.set_read_timeout(timeout),
            #[cfg(windows)]
            Self::Pipe { stream, .. } => stream.set_read_timeout(timeout),
        }
    }

    fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Unix(stream) => stream.set_write_timeout(timeout),
            #[cfg(windows)]
            Self::Pipe { stream, .. } => stream.set_write_timeout(timeout),
        }
    }

    fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        match self {
            Self::Unix(stream) => stream.shutdown(how),
            // 管道没有套接字那种 shutdown。断开这一实例，堵在另一份句柄上的
            // 重叠读写会结束。不改 PIPE_NOWAIT，那会连正在读的句柄一起打断。
            #[cfg(windows)]
            Self::Pipe { stream, .. } => {
                let _ = how;
                stream.disconnect()
            }
        }
    }

    fn connection_id(&self) -> ConnId {
        match self {
            Self::Unix(stream) => ClientSocket::connection_id(stream),
            #[cfg(windows)]
            Self::Pipe { id, .. } => *id,
        }
    }

    fn into_client_conn(self) -> ClientConn {
        self
    }
}

impl Read for ClientConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Unix(stream) => stream.read(buf),
            #[cfg(windows)]
            Self::Pipe { stream, .. } => stream.read(buf),
        }
    }
}

impl Write for ClientConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Unix(stream) => stream.write(buf),
            #[cfg(windows)]
            Self::Pipe { stream, .. } => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Unix(stream) => stream.flush(),
            #[cfg(windows)]
            Self::Pipe { stream, .. } => stream.flush(),
        }
    }
}

/// 交给 Bun / Pi / shell 的 `SMELT_SOCK`。Unix 是 sock 文件。Windows 是命名管道名。
pub fn child_smelt_sock() -> std::ffi::OsString {
    #[cfg(windows)]
    {
        if let Some(name) = DAEMON_PIPE_NAME.get() {
            return name.into();
        }
        if let Ok(name) = smelt_sys::named_pipe::daemon_pipe_path(&crate::sock_path()) {
            return name.into();
        }
    }
    crate::sock_path().into()
}

#[cfg(windows)]
pub static DAEMON_PIPE_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

#[cfg(windows)]
pub fn remember_daemon_pipe(name: String) {
    let _ = DAEMON_PIPE_NAME.set(name);
}
