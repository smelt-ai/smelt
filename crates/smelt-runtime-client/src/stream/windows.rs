//! Windows 的本机流：连守护走命名管道，进程内的一对流在内存里接上。不用系统的 AF_UNIX。
//!
//! afunix.sys 在 Windows 10 上有内核里的释放后再用：桌面进程反复建连、断开，整机蓝屏
//! （0x3B，崩在驱动里一次引用计数递减上，#251）。用户态绕不开驱动的 bug，只能不走它。
//! 命名管道是 Windows 本机进程间通信的正路，Bun / Pi 本来就连守护的这条管道。
//! `pair()` 的两端都在本进程里，用不着内核对象，见 [`super::memory`]。
//!
//! 两种流对外的方法和 Unix 的 `UnixStream` 一致。命名管道有两处差别：没有半关闭，
//! `shutdown(Write)` 报 `Unsupported`，连守护的连接不半关闭，要半关闭的 iroh 桥接用的是
//! `pair()`；写超时之后连接作废（见 `smelt_sys::named_pipe`），用到写超时的调用方本来
//! 就在写失败时断开。

use std::fmt;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use smelt_sys::named_pipe::{
    PipeListener, PipeListenerStop, PipeStream, daemon_pipe_path, is_pipe_path,
};

use super::memory::MemoryStream;

/// 一条连接。`try_clone` 出来的各份共享读写超时、非阻塞设置和 shutdown 状态，和 Unix 上
/// dup 出来的描述符一样。
pub struct RuntimeStream {
    /// 这条连接在本进程里的编号，各份克隆相同。
    id: u64,
    kind: Kind,
}

enum Kind {
    Pipe(PipeStream),
    Memory(MemoryStream),
}

impl RuntimeStream {
    /// 连 `path` 对应的命名管道，见 [`pipe_name`]。守护没在听时立刻报找不到；所有实例都
    /// 正忙时最多等两秒。
    pub fn connect<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let stream = PipeStream::connect(&pipe_name(path.as_ref())?)?;
        Ok(Self::new(Kind::Pipe(stream)))
    }

    /// 一对连好的流，两端都在本进程里。
    pub fn pair() -> io::Result<(Self, Self)> {
        let (a, b) = MemoryStream::pair();
        Ok((Self::new(Kind::Memory(a)), Self::new(Kind::Memory(b))))
    }

    fn new(kind: Kind) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            kind,
        }
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        let kind = match &self.kind {
            Kind::Pipe(stream) => Kind::Pipe(stream.try_clone()?),
            Kind::Memory(stream) => Kind::Memory(stream.try_clone()),
        };
        Ok(Self { id: self.id, kind })
    }

    /// 管道上的 `Shutdown::Write` 报 `Unsupported`，见模块说明。
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        match &self.kind {
            Kind::Pipe(stream) => stream.shutdown(how),
            Kind::Memory(stream) => {
                stream.shutdown(how);
                Ok(())
            }
        }
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match &self.kind {
            Kind::Pipe(stream) => stream.set_read_timeout(timeout),
            Kind::Memory(stream) => stream.set_read_timeout(timeout),
        }
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match &self.kind {
            Kind::Pipe(stream) => stream.set_write_timeout(timeout),
            Kind::Memory(stream) => stream.set_write_timeout(timeout),
        }
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        match &self.kind {
            Kind::Pipe(stream) => stream.read_timeout(),
            Kind::Memory(stream) => Ok(stream.read_timeout()),
        }
    }

    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        match &self.kind {
            Kind::Pipe(stream) => stream.write_timeout(),
            Kind::Memory(stream) => Ok(stream.write_timeout()),
        }
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        match &self.kind {
            Kind::Pipe(stream) => stream.set_nonblocking(nonblocking),
            Kind::Memory(stream) => {
                stream.set_nonblocking(nonblocking);
                Ok(())
            }
        }
    }

    /// 这条连接在本进程里的编号，各份克隆相同。
    pub fn id(&self) -> u64 {
        self.id
    }

    /// 对端的进程号。`pair()` 的对端就是本进程，和 socketpair 一样。
    pub fn peer_pid(&self) -> io::Result<u32> {
        match &self.kind {
            Kind::Pipe(stream) => stream.peer_pid(),
            Kind::Memory(_) => Ok(std::process::id()),
        }
    }

    fn read_bytes(&self, buf: &mut [u8]) -> io::Result<usize> {
        match &self.kind {
            Kind::Pipe(stream) => Read::read(&mut &*stream, buf),
            Kind::Memory(stream) => stream.read_bytes(buf),
        }
    }

    fn write_bytes(&self, buf: &[u8]) -> io::Result<usize> {
        match &self.kind {
            Kind::Pipe(stream) => Write::write(&mut &*stream, buf),
            Kind::Memory(stream) => stream.write_bytes(buf),
        }
    }
}

impl fmt::Debug for RuntimeStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("RuntimeStream");
        debug.field("id", &self.id);
        match &self.kind {
            Kind::Pipe(stream) => debug.field("pipe", stream),
            Kind::Memory(stream) => debug.field("memory", stream),
        };
        debug.finish()
    }
}

impl Read for RuntimeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_bytes(buf)
    }
}

impl Read for &RuntimeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_bytes(buf)
    }
}

impl Write for RuntimeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_bytes(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Write for &RuntimeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_bytes(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 守护的监听：`path` 对应的命名管道，见 [`pipe_name`]。接受进来的是 [`RuntimeStream`]。
pub struct RuntimeListener(PipeListener);

impl RuntimeListener {
    /// 名字已经有人在听就失败，和 Unix 上路径被占时 `bind` 报错一样。
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        PipeListener::bind(pipe_name(path.as_ref())?).map(Self)
    }

    /// 接手名字，见 [`PipeListener::bind_taking_over`]。调用方必须另有单实例保证。
    pub fn bind_taking_over<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        PipeListener::bind_taking_over(pipe_name(path.as_ref())?).map(Self)
    }

    /// 一个个接受进来的连接，被 [`stopper`](Self::stopper) 叫停后结束。交出来的错误接受
    /// 线程已经退避过，调用方记日志就行。
    pub fn incoming(&self) -> impl Iterator<Item = io::Result<RuntimeStream>> + '_ {
        self.0
            .incoming()
            .map(|stream| stream.map(|stream| RuntimeStream::new(Kind::Pipe(stream))))
    }

    /// 别的线程用它停止接新连接，相当于 Unix 上摘掉 socket 路径。
    pub fn stopper(&self) -> PipeListenerStop {
        self.0.stopper()
    }
}

impl fmt::Debug for RuntimeListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// `\\.\pipe\` 开头的照原样用（交给子进程的 `SMELT_SOCK` 就是这种）；其余是 `smeltd.sock`
/// 这样的路径，换成它对应的管道名。Windows 上不建这个文件，路径只用来起名。
fn pipe_name(path: &Path) -> io::Result<String> {
    match path.to_str() {
        Some(name) if is_pipe_path(name) => Ok(name.to_string()),
        _ => daemon_pipe_path(path),
    }
}
