//! 连接器：给定一个运行时，返回一条新的双向字节流。
//!
//! 所有连接器都交出 [`RuntimeStream`]。本机直接连 socket；远程 iroh 连接器会在后台把
//! QUIC 流和 `RuntimeStream::pair()` 的一端对接，和运行时那侧接 iroh 的做法相同。
//! 这样上层的阻塞读写、`try_clone`、读写超时对两种传输完全一样，协议代码不用分叉。

use std::io;
use std::path::{Path, PathBuf};

use crate::RuntimeStream;

pub trait Connector: Send + Sync {
    /// 每次调用都新开一条连接。smeltd 按连接分流首帧，一条连接只承载一个 op。
    fn connect(&self) -> io::Result<RuntimeStream>;
}

/// 测试和进程内桥接用：任何返回新流的闭包都是连接器。
impl<F> Connector for F
where
    F: Fn() -> io::Result<RuntimeStream> + Send + Sync,
{
    fn connect(&self) -> io::Result<RuntimeStream> {
        self()
    }
}

/// 本机运行时：直接连它的 unix socket。
#[derive(Clone, Debug)]
pub struct LocalConnector {
    socket: PathBuf,
}

impl LocalConnector {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Connector for LocalConnector {
    fn connect(&self) -> io::Result<RuntimeStream> {
        RuntimeStream::connect(&self.socket)
    }
}
