//! Windows 命名管道：本机进程之间的连接都走它。桌面、命令行、`smelt-notify` 连守护，
//! Bun / Node 的 `net.connect(path)`（在 Windows 上连的就是 `\\.\pipe\...`），后台任务
//! 套接字，都是这里的管道。不用系统的 AF_UNIX：afunix.sys 在 Windows 10 上有内核里的
//! 释放后再用，桌面反复建连、断开时整机蓝屏（0x3B，#251）。
//!
//! 管道拒绝远程客户端，DACL 只给当前用户。我们作为客户端打开管道时只许服务端识别身份、
//! 不许冒用（[`CLIENT_SQOS`]）。对端 pid 用 `GetNamedPipeClientProcessId`（服务端）/
//! `GetNamedPipeServerProcessId`（客户端）拿。
//!
//! 读写带 `FILE_FLAG_OVERLAPPED`。每次操作自己的手动重置事件，和本地 `shutdown` 的事件、
//! 读写超时一起等。不切换 `PIPE_NOWAIT`：那个模式挂在管道实例上，同一实例上的读写共享
//! 同一份，一边为了写超时改成不等待，另一边正在进行的 `ReadFile` 会立刻以
//! `ERROR_NO_DATA` 失败。
//!
//! [`PipeStream`] 的语义照 std 的 `UnixStream`：`try_clone` 出来的各份共用一个句柄和
//! 读写超时、非阻塞、shutdown 状态；本地 `shutdown` 叫醒别的线程里等着的读写。管道没有
//! 半关闭，`shutdown(Both)` 等手上的读写都退出就关句柄：对端读完已经写进去的数据，再读到
//! EOF，和套接字的 FIN 一样。不用 `DisconnectNamedPipe`，它会把对端还没读的数据丢掉。

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_OPERATION_ABORTED,
    ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::{
    ACL_REVISION, AddAccessAllowedAce, GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor,
    SECURITY_ATTRIBUTES, SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, INFINITE, OpenProcessToken, ResetEvent, SetEvent,
    WaitForMultipleObjects, WaitForSingleObject,
};

use crate::process::current_user_key;

/// 实例都在忙时，客户端最多等多久才放弃。监听的接受线程每连上一个就先建好下一个实例，
/// 这段等待盖住中间那一下空档，以及队列攒满时等调用方取走的时间。服务没起来时
/// `CreateFile` 是「找不到」，不会耗这点时间。
const CONNECT_BUDGET: Duration = Duration::from_secs(2);

/// 我们作为客户端打开管道时带的安全服务质量：服务端只能识别我们是谁，不能冒用我们的身份。
/// 不带时默认可以冒用：同一用户下别的进程抢先建好同名管道，以管理员运行的 smelt 连上去，
/// 就把管理员令牌交给了它。我们的服务端认人只看对端 pid，用不着冒用。
const CLIENT_SQOS: u32 = SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION;

/// 当前用户专属的管道名，形如 `\\.\pipe\smelt-<sid>-<tag>`。
pub fn user_pipe_path(tag: &str) -> io::Result<String> {
    let key = current_user_key()?;
    let tag: String = tag
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    Ok(format!(r"\\.\pipe\smelt-{key}-{tag}"))
}

/// 守护监听的命名管道名，Rust 客户端和 Bun / Pi 连的都是它。和 `smeltd.sock` 这个路径
/// 一一对应（Windows 上没有这个文件，路径只用来起名）：管道名只看用户 SID 的话，同一用户
/// 下不同 `SMELT_HOME` 的守护会抢同一个名字。
pub fn daemon_pipe_path(sock: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    // Windows 路径不区分大小写，同一个目录换个写法也得到同一个名字。
    let digest = Sha256::digest(sock.to_string_lossy().to_lowercase().as_bytes());
    let short = hex::encode(&digest[..8]);
    user_pipe_path(&format!("daemon-{short}"))
}

/// 管道的监听。
///
/// 命名管道没有套接字那样的 backlog：只有正在等客户端的那一个实例能连，连上之后到建好
/// 下一个实例之间，后来的客户端只能干等。所以 `bind` 时就起一条接受线程，不停地建实例、
/// 等客户端，连上的放进有界队列，[`accept`](Self::accept) 从队列里取。调用方的接受循环
/// 晚一点起来也不耽误客户端连上，和套接字 `listen` 之后内核替你排队一样。队列满了接受
/// 线程停下来等，客户端在 `WaitNamedPipe` 上等空位，最多等 [`CONNECT_BUDGET`]。
pub struct PipeListener {
    shared: Arc<ListenerShared>,
}

/// 接受线程最多替调用方攒多少条已经连上、还没被取走的连接。
const BACKLOG: usize = 64;

struct ListenerShared {
    name: String,
    /// 已经建好、等着下一个客户端的实例。`bind` 时就放上，避免调用方一返回客户端就
    /// `ERROR_FILE_NOT_FOUND`。
    pending: Mutex<Option<OwnedHandle>>,
    /// 已经连上、还没被 `accept` 取走的连接，和接受线程里出的错。
    backlog: Mutex<VecDeque<io::Result<PipeStream>>>,
    /// `backlog` 有了新连接、腾出了空位，或者叫停了。
    backlog_changed: Condvar,
    stop: Signal,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl ListenerShared {
    /// 叫停：之后不再建实例；在等客户端的实例、队列里还没被取走的连接都关掉，那些客户端
    /// 读到 EOF，会去重连。等接受线程退出才返回，返回之后这个监听手里不再有实例，新客户端
    /// 连不上。已经被 `accept` 取走的连接不受影响。
    fn stop_listening(&self) {
        // 先置标志再收走实例：`ensure_pending` 在锁里看标志，不会在这之后再建一个。
        self.stop.set();
        drop(lock(&self.pending).take());
        let queued = std::mem::take(&mut *lock(&self.backlog));
        self.backlog_changed.notify_all();
        drop(queued);
        let thread = lock(&self.thread).take();
        if let Some(thread) = thread
            && thread.thread().id() != std::thread::current().id()
        {
            let _ = thread.join();
        }
    }

    fn ensure_pending(&self) {
        let mut slot = lock(&self.pending);
        if slot.is_none()
            && !self.stop.is_set()
            && let Ok(handle) = create_instance(&self.name, false)
        {
            *slot = Some(handle);
        }
    }

    /// 放进队列，满了等空位。叫停了返回 `false`，连接随之关掉。
    fn enqueue(&self, stream: PipeStream) -> bool {
        let mut backlog = lock(&self.backlog);
        loop {
            if self.stop.is_set() {
                return false;
            }
            if backlog.len() < BACKLOG {
                backlog.push_back(Ok(stream));
                self.backlog_changed.notify_all();
                return true;
            }
            backlog = self
                .backlog_changed
                .wait(backlog)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// 错误交给调用方记日志。队列满了就丢掉，不为报错挤占连接的位置。
    fn report(&self, error: io::Error) {
        let mut backlog = lock(&self.backlog);
        if !self.stop.is_set() && backlog.len() < BACKLOG {
            backlog.push_back(Err(error));
            self.backlog_changed.notify_all();
        }
    }

    /// 接受线程：等客户端，连上就先建好下一个实例，再把这条放进队列。
    fn serve(&self) {
        let mut failures = 0u32;
        while !self.stop.is_set() {
            let instance = lock(&self.pending).take();
            let connected = match instance {
                Some(handle) => Ok(handle),
                None => create_instance(&self.name, false),
            }
            .and_then(|handle| wait_for_client(handle, &self.stop));
            // 这一实例用掉了（连上了，或者出了错）。下一个先建好，紧跟着来的客户端才有
            // 实例可连，退避的这段时间也一样。
            self.ensure_pending();
            let accepted = connected.and_then(|handle| {
                handle
                    .map(|handle| PipeStream::from_handle(handle, End::Server))
                    .transpose()
            });
            let error = match accepted {
                Ok(None) => return,
                Ok(Some(stream)) => {
                    failures = 0;
                    if !self.enqueue(stream) {
                        return;
                    }
                    continue;
                }
                // 客户端连上就走了，不是监听的毛病，接着等下一个。
                Err(error) if error.raw_os_error() == Some(ERROR_NO_DATA as i32) => continue,
                Err(error) => error,
            };
            self.report(error);
            // 轮询例外：建不出实例、等客户端出错之后没有「可以再试」的事件；周期 50 毫秒起
            // 翻倍，上限 5 秒，成功一次就回到起点。叫停会立刻叫醒。
            let delay = Duration::from_millis(50)
                .saturating_mul(1u32.checked_shl(failures).unwrap_or(u32::MAX))
                .min(Duration::from_secs(5));
            if self.stop.wait(delay) {
                return;
            }
            failures = failures.saturating_add(1);
        }
    }
}

/// 叫停一个 [`PipeListener`]，见 `ListenerShared::stop_listening`：返回之后新客户端连不上，
/// 还没被取走的连接都关掉，已经接受的连接不受影响。
#[derive(Clone)]
pub struct PipeListenerStop(Arc<ListenerShared>);

impl PipeListenerStop {
    pub fn stop(&self) {
        self.0.stop_listening();
    }
}

impl PipeListener {
    /// 这个名字已经有人在听（或者还有没关掉的实例）就失败，不悄悄挂成同一管道的另一个
    /// 实例，否则客户端会随机连到另一个进程。
    pub fn bind(name: impl Into<String>) -> io::Result<Self> {
        Self::bind_with(name.into(), true)
    }

    /// 接手这个名字：已有实例时照样建，成为同一管道的又一个实例。
    ///
    /// 只给另有单实例保证的调用方用（守护握着实例锁）。前任退出之后，它的客户端还没关掉
    /// 的那几端会让名字继续存在，[`bind`](Self::bind) 会一直被拒，直到那些客户端都关掉。
    pub fn bind_taking_over(name: impl Into<String>) -> io::Result<Self> {
        Self::bind_with(name.into(), false)
    }

    fn bind_with(name: String, first: bool) -> io::Result<Self> {
        if !is_pipe_path(&name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("命名管道路径必须以 \\\\.\\pipe\\ 开头：{name}"),
            ));
        }
        let pending = create_instance(&name, first)?;
        let shared = Arc::new(ListenerShared {
            name,
            pending: Mutex::new(Some(pending)),
            backlog: Mutex::new(VecDeque::new()),
            backlog_changed: Condvar::new(),
            stop: Signal::new()?,
            thread: Mutex::new(None),
        });
        let serving = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("smelt-pipe-accept".into())
            .spawn(move || serving.serve())?;
        *lock(&shared.thread) = Some(thread);
        Ok(Self { shared })
    }

    pub fn name(&self) -> &str {
        &self.shared.name
    }

    /// 别的线程用它叫停监听，见 [`PipeListenerStop`]。
    pub fn stopper(&self) -> PipeListenerStop {
        PipeListenerStop(Arc::clone(&self.shared))
    }

    /// 取一条已经连上的连接，没有就等。叫停之后报 `ConnectionAborted`。
    pub fn accept(&self) -> io::Result<PipeStream> {
        let shared = &self.shared;
        let mut backlog = lock(&shared.backlog);
        loop {
            if shared.stop.is_set() {
                return Err(stopped());
            }
            if let Some(next) = backlog.pop_front() {
                shared.backlog_changed.notify_all();
                return next;
            }
            backlog = shared
                .backlog_changed
                .wait(backlog)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// 一个个接受进来的连接，叫停之后结束。
    pub fn incoming(&self) -> impl Iterator<Item = io::Result<PipeStream>> + '_ {
        std::iter::from_fn(move || match self.accept() {
            Err(_) if self.shared.stop.is_set() => None,
            result => Some(result),
        })
    }
}

impl Drop for PipeListener {
    /// 监听没了就不再接客户端，接受线程随之退出。
    fn drop(&mut self) {
        self.shared.stop_listening();
    }
}

impl fmt::Debug for PipeListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PipeListener")
            .field("name", &self.shared.name)
            .finish()
    }
}

fn stopped() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "命名管道监听已停止")
}

/// 一条管道连接。`try_clone` 出来的各份共用同一个句柄和设置，见模块说明。
pub struct PipeStream {
    shared: Arc<StreamShared>,
}

struct StreamShared {
    handle: Mutex<HandleSlot>,
    end: End,
    read_timeout: Mutex<Option<Duration>>,
    write_timeout: Mutex<Option<Duration>>,
    nonblocking: AtomicBool,
    read_shut: Signal,
    write_shut: Signal,
}

struct HandleSlot {
    /// `shutdown(Both)` 之后、手上的读写都退出时关掉，变成 `None`。
    handle: Option<OwnedHandle>,
    /// 正在用句柄的读写数。不为零时不关句柄，免得别的线程拿着的句柄值被系统复用。
    users: usize,
    closing: bool,
}

#[derive(Clone, Copy, Debug)]
enum End {
    /// 监听方接受的那一端。
    Server,
    /// `CreateFile` 连上去的那一端。
    Client,
}

/// 借着句柄做一次读写。活着时句柄不会被关掉。
struct HandleUse<'a> {
    shared: &'a StreamShared,
    raw: HANDLE,
}

impl Drop for HandleUse<'_> {
    fn drop(&mut self) {
        let mut slot = lock(&self.shared.handle);
        slot.users -= 1;
        if slot.closing && slot.users == 0 {
            drop(slot.handle.take());
        }
    }
}

impl StreamShared {
    fn use_handle(&self) -> Option<HandleUse<'_>> {
        let mut slot = lock(&self.handle);
        let raw = slot.handle.as_ref()?.as_raw_handle();
        slot.users += 1;
        Some(HandleUse { shared: self, raw })
    }
}

impl PipeStream {
    /// 连上已经在听的管道。`name` 是 `\\.\pipe\...`。实例正忙时等到有空位或预算用完。
    pub fn connect(name: &str) -> io::Result<Self> {
        let wide = wide(name);
        let deadline = std::time::Instant::now() + CONNECT_BUDGET;
        loop {
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    0x8000_0000 | 0x4000_0000,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED | CLIENT_SQOS,
                    std::ptr::null_mut(),
                )
            };
            if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                return Self::from_handle(
                    unsafe { OwnedHandle::from_raw_handle(handle) },
                    End::Client,
                );
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) {
                return Err(error);
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "命名管道正忙"));
            }
            let millis = u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX - 1).max(1);
            let waited = unsafe { WaitNamedPipeW(wide.as_ptr(), millis) };
            if waited == 0 && std::time::Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "命名管道正忙"));
            }
        }
    }

    fn from_handle(handle: OwnedHandle, end: End) -> io::Result<Self> {
        Ok(Self {
            shared: Arc::new(StreamShared {
                handle: Mutex::new(HandleSlot {
                    handle: Some(handle),
                    users: 0,
                    closing: false,
                }),
                end,
                read_timeout: Mutex::new(None),
                write_timeout: Mutex::new(None),
                nonblocking: AtomicBool::new(false),
                read_shut: Signal::new()?,
                write_shut: Signal::new()?,
            }),
        })
    }

    /// 和这一份共用句柄与设置的另一份，见模块说明。
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            shared: Arc::clone(&self.shared),
        })
    }

    /// `Read`：之后的读都是 EOF，等着的读立刻返回。只影响本端。
    /// `Both`：读写都停下，等手上的读写退出就关句柄，对端读完数据后看到 EOF。
    /// `Write`：管道没有半关闭，报 `Unsupported`。
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        match how {
            Shutdown::Read => {
                self.shared.read_shut.set();
                Ok(())
            }
            Shutdown::Write => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "命名管道不能只关写方向",
            )),
            Shutdown::Both => {
                self.shared.read_shut.set();
                self.shared.write_shut.set();
                let mut slot = lock(&self.shared.handle);
                slot.closing = true;
                if slot.users == 0 {
                    drop(slot.handle.take());
                }
                Ok(())
            }
        }
    }

    /// 为真时没有可读的数据、写不进去就直接报 `WouldBlock`。只改本进程里的语义，
    /// 不改管道实例的等待模式，所以不会碰到正在进行的读写。
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.shared.nonblocking.store(nonblocking, Ordering::SeqCst);
        Ok(())
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        *lock(&self.shared.read_timeout) = checked_timeout(timeout)?;
        Ok(())
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        *lock(&self.shared.write_timeout) = checked_timeout(timeout)?;
        Ok(())
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(*lock(&self.shared.read_timeout))
    }

    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(*lock(&self.shared.write_timeout))
    }

    /// 对端进程号。
    pub fn peer_pid(&self) -> io::Result<u32> {
        let Some(handle) = self.shared.use_handle() else {
            return Err(io::ErrorKind::NotConnected.into());
        };
        let mut pid = 0u32;
        let ok = unsafe {
            match self.shared.end {
                End::Server => GetNamedPipeClientProcessId(handle.raw, &mut pid),
                End::Client => GetNamedPipeServerProcessId(handle.raw, &mut pid),
            }
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            return Err(io::Error::other("命名管道对端没有进程号"));
        }
        Ok(pid)
    }

    fn wait_for(&self, timeout: &Mutex<Option<Duration>>) -> Wait {
        if self.shared.nonblocking.load(Ordering::SeqCst) {
            Wait::Poll
        } else if let Some(timeout) = *lock(timeout) {
            Wait::Budget(timeout)
        } else {
            Wait::Forever
        }
    }

    fn read_bytes(&self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.shared.read_shut.is_set() {
            return Ok(0);
        }
        let Some(handle) = self.shared.use_handle() else {
            return Ok(0);
        };
        let wait = self.wait_for(&self.shared.read_timeout);
        match overlapped_transfer(
            handle.raw,
            buf.as_mut_ptr(),
            buf.len(),
            false,
            wait,
            &self.shared.read_shut,
        )? {
            Transfer::Done(read) => Ok(read),
            Transfer::Shut => Ok(0),
        }
    }

    fn write_bytes(&self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.shared.write_shut.is_set() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let Some(handle) = self.shared.use_handle() else {
            return Err(io::ErrorKind::BrokenPipe.into());
        };
        let wait = self.wait_for(&self.shared.write_timeout);
        match overlapped_transfer(
            handle.raw,
            buf.as_ptr() as *mut u8,
            buf.len(),
            true,
            wait,
            &self.shared.write_shut,
        )? {
            Transfer::Done(written) => Ok(written),
            Transfer::Shut => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }
}

impl fmt::Debug for PipeStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PipeStream")
            .field("end", &self.shared.end)
            .finish()
    }
}

impl Read for PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_bytes(buf)
    }
}

impl Read for &PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_bytes(buf)
    }
}

impl Write for PipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_bytes(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Write for &PipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_bytes(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn checked_timeout(timeout: Option<Duration>) -> io::Result<Option<Duration>> {
    if timeout == Some(Duration::ZERO) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot set a 0 duration timeout",
        ));
    }
    Ok(timeout)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 置位后不再复位的信号：标志给循环开头看，事件和读写一起等。
struct Signal {
    set: AtomicBool,
    event: ManualEvent,
}

impl Signal {
    fn new() -> io::Result<Self> {
        Ok(Self {
            set: AtomicBool::new(false),
            event: ManualEvent::new()?,
        })
    }

    fn set(&self) {
        // 先置标志再置事件：看漏了标志的等待也会被事件叫醒。
        self.set.store(true, Ordering::SeqCst);
        unsafe { SetEvent(self.event.raw()) };
    }

    fn is_set(&self) -> bool {
        self.set.load(Ordering::SeqCst)
    }

    /// 等到置位或超时，返回是否已置位。
    fn wait(&self, timeout: Duration) -> bool {
        self.is_set()
            || unsafe { WaitForSingleObject(self.event.raw(), duration_millis(timeout)) }
                == WAIT_OBJECT_0
    }
}

/// 手动复位的事件，代替「写一个字节叫醒对方」的唤醒管道：`set` 是叫醒，`reset` 是读空。
/// 克隆出来的是同一个事件。
#[derive(Clone)]
pub struct WakeEvent(std::sync::Arc<ManualEvent>);

impl WakeEvent {
    pub fn new() -> io::Result<Self> {
        Ok(Self(std::sync::Arc::new(ManualEvent::new()?)))
    }

    pub fn set(&self) {
        unsafe { SetEvent(self.0.raw()) };
    }

    pub fn reset(&self) {
        unsafe { ResetEvent(self.0.raw()) };
    }
}

/// [`wait_readable`] 能等的东西。
pub enum Waitable<'a> {
    /// 有数据可读，或者对端已经关掉。
    Pipe(&'a PipeStream),
    /// 已置位。
    Event(&'a WakeEvent),
}

/// 阻塞到其中一个就绪，或者超时，返回每一个是否就绪。不取走管道里的数据。
///
/// 管道上挂一个零字节的重叠读：有数据或对端关掉时它完成。返回前没完成的都取消并等它结束，
/// 不会留下一个还挂在栈外缓冲上的读。本端 shutdown 了读方向的管道算就绪（之前或等的时候
/// 都算），读会报出 EOF。
pub fn wait_readable(sources: &[Waitable<'_>], timeout: Option<Duration>) -> io::Result<Vec<bool>> {
    struct ZeroRead<'a> {
        index: usize,
        handle: HandleUse<'a>,
        event: ManualEvent,
        overlapped: OVERLAPPED,
        byte: u8,
    }
    let mut ready = vec![false; sources.len()];
    // 先把会失败的事件都建好，再发起读，中途不会带着挂起的读提前返回。
    let mut reads = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        if let Waitable::Pipe(pipe) = source {
            let handle = if pipe.shared.read_shut.is_set() {
                None
            } else {
                pipe.shared.use_handle()
            };
            let Some(handle) = handle else {
                ready[index] = true;
                continue;
            };
            let event = ManualEvent::new()?;
            reads.push(Box::new(ZeroRead {
                index,
                handle,
                overlapped: OVERLAPPED {
                    hEvent: event.raw(),
                    ..OVERLAPPED::default()
                },
                event,
                byte: 0,
            }));
        }
    }
    let mut pending = Vec::new();
    for read in &mut reads {
        let ok = unsafe {
            ReadFile(
                read.handle.raw,
                &mut read.byte,
                0,
                std::ptr::null_mut(),
                &mut read.overlapped,
            )
        };
        if ok == 0 && unsafe { GetLastError() } == ERROR_IO_PENDING {
            pending.push(read.event.raw());
        } else {
            // 立刻完成是有数据；立刻失败（多半是对端关了）也交给调用方去读，读会报出原因。
            ready[read.index] = true;
        }
    }
    let mut handles = pending;
    // 等的时候本地 shutdown 了读方向也要醒：之后读就是 EOF，算就绪。
    for read in &reads {
        if !ready[read.index] {
            handles.push(read.handle.shared.read_shut.event.raw());
        }
    }
    for source in sources {
        if let Waitable::Event(event) = source {
            handles.push(event.0.raw());
        }
    }
    let millis = if ready.iter().any(|ready| *ready) {
        0
    } else {
        timeout.map_or(INFINITE, duration_millis)
    };
    // 管道都已就绪、又没有事件时没有可等的句柄，空数组会让等待直接失败。
    let wait_error = if handles.is_empty() {
        None
    } else {
        let status = unsafe {
            WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, millis)
        };
        (status == WAIT_FAILED).then(io::Error::last_os_error)
    };
    for read in &mut reads {
        if ready[read.index] {
            continue;
        }
        unsafe { CancelIoEx(read.handle.raw, &read.overlapped) };
        let mut transferred = 0u32;
        let ok = unsafe {
            GetOverlappedResult(read.handle.raw, &read.overlapped, &mut transferred, 1)
        };
        let completed = ok != 0 || unsafe { GetLastError() } != ERROR_OPERATION_ABORTED;
        ready[read.index] = completed || read.handle.shared.read_shut.is_set();
    }
    if let Some(error) = wait_error {
        return Err(error);
    }
    for (index, source) in sources.iter().enumerate() {
        if let Waitable::Event(event) = source {
            ready[index] = unsafe { WaitForSingleObject(event.0.raw(), 0) } == WAIT_OBJECT_0;
        }
    }
    Ok(ready)
}

/// 子进程输出用的管道。我们这端是能重叠读的 [`PipeStream`]，可以和别的来源一起等；
/// 子进程那端是普通的同步写句柄，交给 `Stdio`。std 的 `Stdio::piped()` 这端能不能重叠读
/// 是它的内部实现，不依赖它。
pub fn output_pipe() -> io::Result<(PipeStream, OwnedHandle)> {
    let name = user_pipe_path(&format!(
        "out-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ))?;
    let ours = create_instance(&name, true)?;
    let wide = wide(&name);
    // GENERIC_WRITE | FILE_READ_ATTRIBUTES
    let theirs = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0x4000_0000 | 0x0000_0080,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | CLIENT_SQOS,
            std::ptr::null_mut(),
        )
    };
    if theirs.is_null() || theirs == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let theirs = unsafe { OwnedHandle::from_raw_handle(theirs) };
    Ok((PipeStream::from_handle(ours, End::Server)?, theirs))
}

enum Wait {
    Forever,
    Budget(Duration),
    Poll,
}

enum Transfer {
    Done(usize),
    /// 等着的时候本地 shutdown 了这个方向，这次读写取消掉了。
    Shut,
}

fn overlapped_transfer(
    handle: HANDLE,
    buf: *mut u8,
    len: usize,
    write: bool,
    wait: Wait,
    shut: &Signal,
) -> io::Result<Transfer> {
    let len = u32::try_from(len).unwrap_or(u32::MAX);
    let event = ManualEvent::new()?;
    let mut overlapped = OVERLAPPED {
        hEvent: event.raw(),
        ..OVERLAPPED::default()
    };
    let mut transferred = 0u32;
    let ok = unsafe {
        if write {
            WriteFile(handle, buf, len, &mut transferred, &mut overlapped)
        } else {
            ReadFile(handle, buf, len, &mut transferred, &mut overlapped)
        }
    };
    if ok != 0 {
        return Ok(Transfer::Done(transferred as usize));
    }
    let error = unsafe { GetLastError() };
    if error != ERROR_IO_PENDING {
        return map_completed_error(error, write).map(Transfer::Done);
    }
    let millis = match wait {
        Wait::Forever => INFINITE,
        Wait::Budget(duration) => duration_millis(duration),
        Wait::Poll => 0,
    };
    let handles = [event.raw(), shut.event.raw()];
    // 同时就绪时返回下标小的，即读写完成。
    let status = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, millis) };
    let finished = status == WAIT_OBJECT_0;
    let interrupted = status == WAIT_OBJECT_0 + 1;
    let wait_error =
        (!finished && !interrupted && status != WAIT_TIMEOUT).then(io::Error::last_os_error);
    if !finished {
        // 超时、被 shutdown 叫醒，或等待本身失败。缓冲和 OVERLAPPED 都在调用方的栈上，
        // 取消之后下面要等这次读写真的结束才返回。
        unsafe { CancelIoEx(handle, &overlapped) };
    }
    let mut transferred = 0u32;
    let ok = unsafe { GetOverlappedResult(handle, &overlapped, &mut transferred, 1) };
    let error = (ok == 0).then(|| unsafe { GetLastError() });
    // 传了字节就报字节数，出的错留给下一次读写：取消之前已经读到的数据不能丢，写出去的
    // 字节也不能当没写。和 std 的 `Read::read`、`Write::write` 一样，报错就是一个字节也
    // 没传。
    let Some(error) = error.filter(|_| transferred == 0) else {
        return Ok(Transfer::Done(transferred as usize));
    };
    if finished || error != ERROR_OPERATION_ABORTED {
        return map_completed_error(error, write).map(Transfer::Done);
    }
    if let Some(error) = wait_error {
        return Err(error);
    }
    if interrupted {
        return Ok(Transfer::Shut);
    }
    Err(if matches!(wait, Wait::Poll) {
        io::Error::new(io::ErrorKind::WouldBlock, "命名管道没有数据")
    } else {
        io::Error::new(
            io::ErrorKind::TimedOut,
            if write {
                "命名管道写入超时"
            } else {
                "命名管道读取超时"
            },
        )
    })
}

fn map_completed_error(error: u32, write: bool) -> io::Result<usize> {
    if !write && matches!(error, ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | 0) {
        return Ok(0);
    }
    if error == ERROR_NO_DATA {
        return Err(io::Error::new(io::ErrorKind::WouldBlock, "命名管道没有数据"));
    }
    Err(io::Error::from_raw_os_error(error as i32))
}

fn duration_millis(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX - 1)
}

struct ManualEvent(HANDLE);

// 事件是内核对象，句柄可以在线程间传递、同时使用。
unsafe impl Send for ManualEvent {}
unsafe impl Sync for ManualEvent {}

impl ManualEvent {
    fn new() -> io::Result<Self> {
        let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }

    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for ManualEvent {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// 等客户端连上这一实例。叫停了返回 `None`，实例随之关掉。
fn wait_for_client(handle: OwnedHandle, stop: &Signal) -> io::Result<Option<OwnedHandle>> {
    let event = ManualEvent::new()?;
    let mut overlapped = OVERLAPPED {
        hEvent: event.raw(),
        ..OVERLAPPED::default()
    };
    let connected = unsafe { ConnectNamedPipe(handle.as_raw_handle(), &mut overlapped) };
    if connected != 0 {
        return Ok(Some(handle));
    }
    let error = unsafe { GetLastError() };
    if error == ERROR_PIPE_CONNECTED {
        return Ok(Some(handle));
    }
    if error != ERROR_IO_PENDING {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let handles = [event.raw(), stop.event.raw()];
    let status = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
    if status != WAIT_OBJECT_0 {
        let wait_error = (status != WAIT_OBJECT_0 + 1).then(io::Error::last_os_error);
        unsafe { CancelIoEx(handle.as_raw_handle(), &overlapped) };
        unsafe { WaitForSingleObject(event.raw(), INFINITE) };
        // 叫停时恰好连上的客户端也一起断开：监听已经停了，不再接新连接。
        return match wait_error {
            Some(error) => Err(error),
            None => Ok(None),
        };
    }
    let mut transferred = 0u32;
    let ok = unsafe {
        GetOverlappedResult(handle.as_raw_handle(), &overlapped, &mut transferred, 0)
    };
    if ok == 0 {
        let error = unsafe { GetLastError() };
        if error != ERROR_PIPE_CONNECTED {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
    }
    Ok(Some(handle))
}

/// `FILE_FLAG_FIRST_PIPE_INSTANCE`：这个名字已经有人在听时建不出来（拒绝访问）。
const FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;

/// `first` 是监听方建的第一个实例。名字已被别人占着时失败，不悄悄挂成同一管道的另一个实例，
/// 否则客户端会随机连到另一个进程。
fn create_instance(name: &str, first: bool) -> io::Result<OwnedHandle> {
    let wide = wide(name);
    let mut security = UserOnlySecurity::new()?;
    security.attributes.lpSecurityDescriptor = security.descriptor.as_mut_ptr().cast();
    let open_mode =
        PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | if first { FIRST_PIPE_INSTANCE } else { 0 };
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            &security.attributes,
        )
    };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // 安全描述符只在 CreateNamedPipe 期间使用。
    security.detach_without_freeing_acl_memory();
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

struct UserOnlySecurity {
    descriptor: Vec<u8>,
    acl: Vec<u8>,
    attributes: SECURITY_ATTRIBUTES,
}

impl UserOnlySecurity {
    fn new() -> io::Result<Self> {
        let mut token: HANDLE = std::ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut needed = 0u32;
        unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        }
        let mut token_buf = vec![0u8; needed.max(1) as usize];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                token_buf.as_mut_ptr().cast(),
                token_buf.len() as u32,
                &mut needed,
            )
        } == 0
        {
            unsafe { CloseHandle(token) };
            return Err(io::Error::last_os_error());
        }
        unsafe { CloseHandle(token) };
        let sid = unsafe { (*(token_buf.as_ptr() as *const TOKEN_USER)).User.Sid };
        if sid.is_null() {
            return Err(io::Error::other("当前进程没有用户 SID"));
        }

        let mut acl = vec![0u8; 256];
        if unsafe { InitializeAcl(acl.as_mut_ptr().cast(), acl.len() as u32, ACL_REVISION) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { AddAccessAllowedAce(acl.as_mut_ptr().cast(), ACL_REVISION, 0x1000_0000, sid) } == 0
        {
            return Err(io::Error::last_os_error());
        }

        let mut descriptor = vec![0u8; 128];
        if unsafe { InitializeSecurityDescriptor(descriptor.as_mut_ptr().cast(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            SetSecurityDescriptorDacl(
                descriptor.as_mut_ptr().cast(),
                1,
                acl.as_ptr().cast(),
                0,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            descriptor,
            acl,
            attributes: SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: std::ptr::null_mut(),
                bInheritHandle: 0,
            },
        })
    }

    fn detach_without_freeing_acl_memory(&mut self) {
        // descriptor 里的 DACL 指针指向 `acl`。CreateNamedPipe 返回后内核已经
        // 复制了安全描述符，这里可以一起丢掉。
        self.descriptor.clear();
        self.acl.clear();
        self.attributes.lpSecurityDescriptor = std::ptr::null_mut();
    }
}

/// `\\.\pipe\...` 或 `\\?\pipe\...` 形式的名字。
pub fn is_pipe_path(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 9
        && bytes[0] == b'\\'
        && bytes[1] == b'\\'
        && (bytes[2] == b'.' || bytes[2] == b'?')
        && bytes[3] == b'\\'
        && bytes[4..8].eq_ignore_ascii_case(b"pipe")
        && bytes[8] == b'\\'
}

fn wide(value: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    Path::new(value).as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    const ARRIVES: Duration = Duration::from_secs(5);

    fn test_pipe_name(tag: &str) -> String {
        user_pipe_path(&format!("test-{tag}-{}", uuid::Uuid::new_v4().simple())).unwrap()
    }

    /// 监听方和客户端各一端，已经连好。
    fn connected_pair(tag: &str) -> (PipeListener, PipeStream, PipeStream) {
        let listener = PipeListener::bind(test_pipe_name(tag)).unwrap();
        let client = PipeStream::connect(listener.name()).unwrap();
        let server = listener.accept().unwrap();
        server.set_read_timeout(Some(ARRIVES)).unwrap();
        client.set_read_timeout(Some(ARRIVES)).unwrap();
        (listener, server, client)
    }

    #[test]
    fn daemon_pipe_follows_the_socket_path() {
        let first = daemon_pipe_path(Path::new(r"C:\Users\me\.smelt\smeltd.sock")).unwrap();
        let other_home = daemon_pipe_path(Path::new(r"C:\Temp\dev\smeltd.sock")).unwrap();
        let other_case = daemon_pipe_path(Path::new(r"c:\users\ME\.smelt\smeltd.sock")).unwrap();
        assert_ne!(first, other_home);
        assert_eq!(first, other_case);
    }

    /// 同一个名字只能有一个监听方。第二个 `bind` 要失败，不能悄悄变成同一管道的另一个实例。
    #[test]
    fn second_listener_on_the_same_name_is_refused() {
        let name = test_pipe_name("first");
        let _first = PipeListener::bind(&name).unwrap();
        assert!(PipeListener::bind(&name).is_err());
    }

    /// 前任的客户端还握着自己那端时，名字还在，接手的监听照样建得出来、接得到新客户端。
    #[test]
    fn taking_over_works_while_a_client_of_the_old_listener_is_still_open() {
        let name = test_pipe_name("takeover");
        let old = PipeListener::bind(&name).unwrap();
        let lingering = PipeStream::connect(&name).unwrap();
        let accepted = old.accept().unwrap();
        drop(accepted);
        drop(old);

        let new = PipeListener::bind_taking_over(&name).unwrap();
        let client = PipeStream::connect(&name).unwrap();
        let server = new.accept().unwrap();
        (&client).write_all(b"z").unwrap();
        let mut byte = [0u8; 1];
        server.set_read_timeout(Some(ARRIVES)).unwrap();
        (&server).read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"z");
        drop(lingering);
    }

    /// 叫停之后等着的 accept 返回，`incoming` 结束，新客户端连不上。
    #[test]
    fn stopping_a_listener_ends_incoming_and_refuses_new_clients() {
        let listener = PipeListener::bind(test_pipe_name("stop")).unwrap();
        let name = listener.name().to_string();
        let stop = listener.stopper();
        let accepting = thread::spawn(move || listener.incoming().count());
        thread::sleep(Duration::from_millis(50));
        stop.stop();
        assert_eq!(accepting.join().unwrap(), 0);
        assert!(PipeStream::connect(&name).is_err());
    }

    /// 调用方还没开始 accept 时连上来的客户端都排上队，不会因为只有一个空闲实例而等到
    /// 超时。守护绑好管道就报就绪，接受循环要等初始化做完才起来，靠的就是这个。
    #[test]
    fn clients_connecting_before_accept_are_queued() {
        let listener = PipeListener::bind(test_pipe_name("backlog")).unwrap();
        let clients: Vec<PipeStream> = (0..8u8)
            .map(|index| {
                let client = PipeStream::connect(listener.name()).unwrap();
                (&client).write_all(&[index]).unwrap();
                client
            })
            .collect();
        let mut seen = Vec::new();
        for _ in 0..clients.len() {
            let server = listener.accept().unwrap();
            server.set_read_timeout(Some(ARRIVES)).unwrap();
            let mut byte = [0u8; 1];
            (&server).read_exact(&mut byte).unwrap();
            seen.push(byte[0]);
        }
        seen.sort_unstable();
        assert_eq!(seen, (0..8).collect::<Vec<u8>>());
    }

    /// 叫停时还没被取走的连接一起关掉：客户端读到 EOF，好去重连接班的守护。
    #[test]
    fn stopping_closes_connections_nobody_accepted() {
        let listener = PipeListener::bind(test_pipe_name("drain")).unwrap();
        let client = PipeStream::connect(listener.name()).unwrap();
        client.set_read_timeout(Some(ARRIVES)).unwrap();
        listener.stopper().stop();
        let mut buf = [0u8; 1];
        assert_eq!((&client).read(&mut buf).unwrap(), 0);
    }

    /// 两端都能拿到对方的进程号。同一进程里就是自己。
    #[test]
    fn both_ends_see_the_peer_pid() {
        let (_listener, server, client) = connected_pair("pid");
        assert_eq!(server.peer_pid().unwrap(), std::process::id());
        assert_eq!(client.peer_pid().unwrap(), std::process::id());
    }

    /// 本地 `shutdown` 要叫醒另一份克隆上等着的读。
    #[test]
    fn local_shutdown_wakes_a_read_waiting_on_a_clone() {
        let (_listener, server, _client) = connected_pair("wake");
        let reading = server.try_clone().unwrap();
        let reader = thread::spawn(move || {
            let mut buf = [0u8; 8];
            (&reading).read(&mut buf).map_err(|error| error.kind())
        });
        thread::sleep(Duration::from_millis(50));
        server.shutdown(Shutdown::Both).unwrap();
        assert_eq!(reader.join().unwrap(), Ok(0));
    }

    /// 和套接字的 FIN 一样：`shutdown(Both)` 之前写进去的字节对端都读得到，然后是 EOF，
    /// 哪怕本端还有别的克隆没丢。守护靠这个先把终态快照写完再断开。
    #[test]
    fn shutdown_delivers_written_bytes_then_eof_while_a_clone_is_alive() {
        let (_listener, server, client) = connected_pair("fin");
        let _still_held = server.try_clone().unwrap();
        (&server).write_all(b"final").unwrap();
        server.shutdown(Shutdown::Both).unwrap();
        let mut received = Vec::new();
        (&client).read_to_end(&mut received).unwrap();
        assert_eq!(received, b"final");
        assert_eq!(
            (&server).write(b"more").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    /// 克隆共用读超时和非阻塞设置，和 dup 出来的套接字一样。
    #[test]
    fn clones_share_timeouts_and_nonblocking() {
        let (_listener, server, _client) = connected_pair("shared");
        let clone = server.try_clone().unwrap();
        server
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        assert_eq!(
            clone.read_timeout().unwrap(),
            Some(Duration::from_millis(50))
        );
        let mut buf = [0u8; 8];
        assert_eq!(
            (&clone).read(&mut buf).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        server.set_nonblocking(true).unwrap();
        assert_eq!(
            (&clone).read(&mut buf).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    /// 写超时截断的一次写，报回的字节数要和对端收到的一致：报错就是一个字节也没写进去，
    /// 和 std 的 `Write::write` 约定一样。
    #[test]
    fn a_write_cut_short_by_its_timeout_reports_what_the_peer_got() {
        let (_listener, server, client) = connected_pair("partial");
        // 比管道缓冲（64 KiB）大得多，对端不读就写不完。
        let payload = vec![7u8; 1024 * 1024];
        server
            .set_write_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let writer = thread::spawn(move || {
            let written = match (&server).write(&payload) {
                Ok(written) => written,
                Err(error) if error.kind() == io::ErrorKind::TimedOut => 0,
                Err(error) => panic!("写失败：{error}"),
            };
            drop(server);
            written
        });
        // 只读走一部分，剩下的让写超时。
        let mut taken = vec![0u8; 200 * 1024];
        (&client).read_exact(&mut taken).unwrap();
        let written = writer.join().unwrap();
        let mut rest = Vec::new();
        (&client).read_to_end(&mut rest).unwrap();
        assert_eq!(written, taken.len() + rest.len());
    }

    /// 我们作为客户端连上去，服务端只能识别身份、不能冒用：同一用户下别的进程抢先建了
    /// 同名管道，以管理员运行的 smelt 连上去也交不出能用的令牌。
    #[test]
    fn servers_can_identify_clients_but_not_impersonate_them() {
        use windows_sys::Win32::Security::{
            RevertToSelf, SecurityIdentification, TokenImpersonationLevel,
        };
        use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
        use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

        fn impersonation_level() -> io::Result<i32> {
            let mut token: HANDLE = std::ptr::null_mut();
            if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let token = unsafe { OwnedHandle::from_raw_handle(token) };
            let mut level = 0i32;
            let mut needed = 0u32;
            let ok = unsafe {
                GetTokenInformation(
                    token.as_raw_handle(),
                    TokenImpersonationLevel,
                    std::ptr::from_mut(&mut level).cast(),
                    std::mem::size_of::<i32>() as u32,
                    &mut needed,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(level)
        }

        let (_listener, server, client) = connected_pair("sqos");
        // 服务端要先从管道里读到数据，才能冒用客户端。
        (&client).write_all(b"x").unwrap();
        let mut byte = [0u8; 1];
        (&server).read_exact(&mut byte).unwrap();
        // 在单独的线程里冒用，不弄脏跑测试的线程。
        let level = thread::spawn(move || {
            let handle = server.shared.use_handle().expect("连接还开着");
            if unsafe { ImpersonateNamedPipeClient(handle.raw) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let level = impersonation_level();
            unsafe { RevertToSelf() };
            level
        })
        .join()
        .unwrap()
        .unwrap();
        assert_eq!(level, SecurityIdentification);
    }

    /// 等可读的时候本地 shutdown 了读方向，要叫醒它、算作就绪（接着读就是 EOF），和等着
    /// 的读一样。
    #[test]
    fn local_shutdown_wakes_wait_readable() {
        let (_listener, server, _client) = connected_pair("wait-shut");
        let waiting = server.try_clone().unwrap();
        let waiter = thread::spawn(move || {
            wait_readable(&[Waitable::Pipe(&waiting)], Some(ARRIVES)).unwrap()
        });
        thread::sleep(Duration::from_millis(50));
        server.shutdown(Shutdown::Read).unwrap();
        assert_eq!(waiter.join().unwrap(), [true]);
    }

    #[test]
    fn shutdown_write_alone_is_unsupported() {
        let (_listener, server, _client) = connected_pair("half");
        assert_eq!(
            server.shutdown(Shutdown::Write).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }

    #[test]
    fn wake_event_is_ready_only_while_set() {
        let event = WakeEvent::new().unwrap();
        let short = Some(Duration::from_millis(20));
        assert_eq!(wait_readable(&[Waitable::Event(&event)], short).unwrap(), [false]);
        event.clone().set();
        assert_eq!(wait_readable(&[Waitable::Event(&event)], None).unwrap(), [true]);
        event.reset();
        assert_eq!(wait_readable(&[Waitable::Event(&event)], short).unwrap(), [false]);
    }

    /// 等可读不能取走数据，超时要按时返回，对端关掉也算可读。
    #[test]
    fn output_pipe_wakes_waiters_without_consuming_data() {
        let (mut ours, theirs) = output_pipe().unwrap();
        ours.set_nonblocking(true).unwrap();
        let mut theirs = std::fs::File::from(theirs);
        let wake = WakeEvent::new().unwrap();
        let short = Some(Duration::from_millis(20));
        let sources = [Waitable::Pipe(&ours), Waitable::Event(&wake)];
        assert_eq!(wait_readable(&sources, short).unwrap(), [false, false]);

        theirs.write_all(b"hello").unwrap();
        assert_eq!(wait_readable(&sources, None).unwrap(), [true, false]);
        let mut buf = [0u8; 16];
        assert_eq!(ours.read(&mut buf).unwrap(), 5);
        assert_eq!(&buf[..5], b"hello");

        let sources = [Waitable::Pipe(&ours), Waitable::Event(&wake)];
        assert_eq!(wait_readable(&sources, short).unwrap(), [false, false]);
        drop(theirs);
        assert_eq!(wait_readable(&sources, None).unwrap(), [true, false]);
        assert_eq!(ours.read(&mut buf).unwrap(), 0);
    }
}
