//! 守护接受的客户端连接。
//!
//! 只有一条监听：Unix 是 `smeltd.sock`（AF_UNIX），Windows 是它对应的命名管道
//! （`smelt_sys::named_pipe::daemon_pipe_path`）。桌面、命令行、`smelt-notify` 和
//! Bun / Pi 连的都是它，接受进来都是 [`RuntimeStream`]。

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::time::Duration;

use smelt_runtime_client::RuntimeStream;

/// 在本进程里认出一条连接（确认字节、摘掉 attachment）。
#[cfg(unix)]
pub type ConnId = std::os::unix::io::RawFd;
/// 在本进程里认出一条连接（确认字节、摘掉 attachment）。各份克隆相同。
#[cfg(windows)]
pub type ConnId = u64;

/// 一条已经接受的客户端连接。协议代码只依赖 [`ClientSocket`]。
pub type ClientConn = RuntimeStream;

/// 守护协议用到的流操作。测试可以把 `RuntimeStream::pair()` 的一端直接交给处理函数。
pub trait ClientSocket: Read + Write + Send + Sized + 'static {
    fn try_clone(&self) -> io::Result<Self>;
    fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()>;
    fn shutdown(&self, how: Shutdown) -> io::Result<()>;
    fn connection_id(&self) -> ConnId;
    fn into_client_conn(self) -> ClientConn;
}

impl ClientSocket for RuntimeStream {
    fn try_clone(&self) -> io::Result<Self> {
        RuntimeStream::try_clone(self)
    }

    fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        RuntimeStream::set_write_timeout(self, timeout)
    }

    fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        RuntimeStream::shutdown(self, how)
    }

    fn connection_id(&self) -> ConnId {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            self.as_raw_fd()
        }
        #[cfg(windows)]
        {
            self.id()
        }
    }

    fn into_client_conn(self) -> ClientConn {
        self
    }
}

/// 交给 Bun / Pi / shell 的 `SMELT_SOCK`。Unix 是 sock 文件。Windows 是命名管道名：
/// Node 的 `net.connect(path)` 在 Windows 上只认管道。
pub fn child_smelt_sock() -> std::ffi::OsString {
    #[cfg(windows)]
    if let Ok(name) = smelt_sys::named_pipe::daemon_pipe_path(&crate::sock_path()) {
        return name.into();
    }
    crate::sock_path().into()
}
