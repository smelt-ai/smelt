//! Windows 命名管道。Bun / Node 的 `net.connect(path)` 在 Windows 上连的是
//! `\\.\pipe\...`，不是文件系统上的 AF_UNIX。交给 Bun 的 `SMELT_SOCK` 和后台
//! 任务套接字都用这里，不把管道桥接进 AF_UNIX——桥的对端会变成 smeltd 自己，
//! 事件鉴权会拒绝。
//!
//! 管道拒绝远程客户端，DACL 只给当前用户。接受之后用
//! `GetNamedPipeClientProcessId` 拿对端 pid。
//!
//! 读写带 `FILE_FLAG_OVERLAPPED`。每次操作自己的手动重置事件，超时靠
//! `WaitForSingleObject`。不切换 `PIPE_NOWAIT`：那个模式挂在管道实例上，
//! `DuplicateHandle` 出来的句柄共享同一份，一边为了写超时改成不等待，
//! 另一边正在进行的 `ReadFile` 会立刻以 `ERROR_NO_DATA` 失败。

use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA,
    ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::{
    ACL_REVISION, AddAccessAllowedAce, GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor,
    SECURITY_ATTRIBUTES, SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, INFINITE, OpenProcessToken, ResetEvent, SetEvent,
    WaitForMultipleObjects, WaitForSingleObject,
};

use crate::process::current_user_key;

/// 实例都在忙时，客户端最多等多久才放弃。监听方在 `accept` 返回前会先建好下一个实例，
/// 这段等待盖住中间那一下空档。服务没起来时 `CreateFile` 是「找不到」，不会耗这点时间。
const CONNECT_BUDGET: Duration = Duration::from_secs(2);

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

/// 守护给 Bun / Pi 的命名管道名。和 AF_UNIX 的 `smeltd.sock` 一一对应：
/// 管道名只看用户 SID 的话，同一用户下不同 `SMELT_HOME` 的守护会抢同一个名字。
pub fn daemon_pipe_path(sock: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    // Windows 路径不区分大小写，同一个目录换个写法也得到同一个名字。
    let digest = Sha256::digest(sock.to_string_lossy().to_lowercase().as_bytes());
    let short: String = digest[..8].iter().map(|byte| format!("{byte:02x}")).collect();
    user_pipe_path(&format!("daemon-{short}"))
}

pub struct PipeListener {
    name: String,
    /// 已经建好、正在等客户端的那一个实例。`bind` 时就放上，避免调用方一返回
    /// 客户端就 `ERROR_FILE_NOT_FOUND`。
    pending: Mutex<Option<OwnedHandle>>,
}

impl PipeListener {
    pub fn bind(name: impl Into<String>) -> io::Result<Self> {
        let name = name.into();
        if !is_pipe_path(&name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("命名管道路径必须以 \\\\.\\pipe\\ 开头：{name}"),
            ));
        }
        let pending = create_instance(&name, true)?;
        Ok(Self {
            name,
            pending: Mutex::new(Some(pending)),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// 阻塞到有客户端连上。返回已连接的流之前，先建好下一个实例。
    pub fn accept(&self) -> io::Result<PipeStream> {
        let current = {
            let mut slot = self.pending.lock().unwrap_or_else(|error| error.into_inner());
            match slot.take() {
                Some(handle) => handle,
                None => create_instance(&self.name, false)?,
            }
        };
        let connected = match wait_for_client(current) {
            Ok(handle) => handle,
            Err(error) => {
                self.ensure_pending();
                return Err(error);
            }
        };
        // 客户端已经占用了这一实例。下一个必须在把流交出去之前就存在，
        // 否则紧跟着来的连接会落到「没有实例」。
        self.ensure_pending();
        Ok(PipeStream::from_handle(connected))
    }

    fn ensure_pending(&self) {
        let mut slot = self.pending.lock().unwrap_or_else(|error| error.into_inner());
        if slot.is_none()
            && let Ok(handle) = create_instance(&self.name, false)
        {
            *slot = Some(handle);
        }
    }
}

pub struct PipeStream {
    handle: OwnedHandle,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    /// 只影响这一份句柄之后的操作。不改内核里的管道模式。
    nonblocking: AtomicBool,
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
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                    std::ptr::null_mut(),
                )
            };
            if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                return Ok(Self::from_handle(unsafe { OwnedHandle::from_raw_handle(handle) }));
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) {
                return Err(error);
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "命名管道正忙",
                ));
            }
            let millis = u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX - 1).max(1);
            let waited = unsafe { WaitNamedPipeW(wide.as_ptr(), millis) };
            if waited == 0 && std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "命名管道正忙",
                ));
            }
        }
    }

    fn from_handle(handle: OwnedHandle) -> Self {
        Self {
            handle,
            read_timeout: None,
            write_timeout: None,
            nonblocking: AtomicBool::new(false),
        }
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        let mut duplicated: HANDLE = std::ptr::null_mut();
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.handle.as_raw_handle(),
                GetCurrentProcess(),
                &mut duplicated,
                0,
                0,
                0x0000_0002,
            )
        };
        if ok == 0 || duplicated.is_null() {
            return Err(io::Error::last_os_error());
        }
        let cloned = Self::from_handle(unsafe { OwnedHandle::from_raw_handle(duplicated) });
        cloned
            .nonblocking
            .store(self.nonblocking.load(Ordering::Relaxed), Ordering::Relaxed);
        Ok(Self {
            read_timeout: self.read_timeout,
            write_timeout: self.write_timeout,
            ..cloned
        })
    }

    /// 之后没有单独设超时的读写立刻返回 `WouldBlock`。不改管道实例的等待模式，
    /// 所以不会碰到同一实例上另一份句柄正在进行的读写。
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.nonblocking.store(nonblocking, Ordering::Relaxed);
        Ok(())
    }

    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.read_timeout = timeout;
        Ok(())
    }

    pub fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.write_timeout = timeout;
        Ok(())
    }

    /// 断开服务端这一实例。堵在另一份句柄上的重叠读写会结束，读端看到管道断开。
    pub fn disconnect(&self) -> io::Result<()> {
        if unsafe { DisconnectNamedPipe(self.handle.as_raw_handle()) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn client_pid(&self) -> io::Result<u32> {
        let mut pid = 0u32;
        if unsafe { GetNamedPipeClientProcessId(self.handle.as_raw_handle(), &mut pid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            return Err(io::Error::other("命名管道对端没有进程号"));
        }
        Ok(pid)
    }

    fn wait_for(&self, timeout: Option<Duration>) -> Wait {
        if let Some(timeout) = timeout {
            Wait::Budget(timeout)
        } else if self.nonblocking.load(Ordering::Relaxed) {
            Wait::Poll
        } else {
            Wait::Forever
        }
    }
}

impl AsRawHandle for PipeStream {
    fn as_raw_handle(&self) -> RawHandle {
        self.handle.as_raw_handle()
    }
}

impl Read for PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let wait = self.wait_for(self.read_timeout);
        overlapped_transfer(
            self.handle.as_raw_handle(),
            buf.as_mut_ptr(),
            buf.len(),
            false,
            wait,
        )
    }
}

impl Write for PipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let wait = self.wait_for(self.write_timeout);
        overlapped_transfer(
            self.handle.as_raw_handle(),
            buf.as_ptr() as *mut u8,
            buf.len(),
            true,
            wait,
        )
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
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
/// 不会留下一个还挂在栈外缓冲上的读。
pub fn wait_readable(sources: &[Waitable<'_>], timeout: Option<Duration>) -> io::Result<Vec<bool>> {
    struct ZeroRead {
        index: usize,
        handle: HANDLE,
        event: ManualEvent,
        overlapped: OVERLAPPED,
        byte: u8,
    }
    let mut ready = vec![false; sources.len()];
    // 先把会失败的事件都建好，再发起读，中途不会带着挂起的读提前返回。
    let mut reads = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        if let Waitable::Pipe(pipe) = source {
            let event = ManualEvent::new()?;
            reads.push(Box::new(ZeroRead {
                index,
                handle: pipe.handle.as_raw_handle(),
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
                read.handle,
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
        unsafe { CancelIoEx(read.handle, &read.overlapped) };
        let mut transferred = 0u32;
        let ok = unsafe { GetOverlappedResult(read.handle, &read.overlapped, &mut transferred, 1) };
        ready[read.index] =
            ok != 0 || unsafe { GetLastError() } != ERROR_OPERATION_ABORTED;
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
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if theirs.is_null() || theirs == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok((PipeStream::from_handle(ours), unsafe {
        OwnedHandle::from_raw_handle(theirs)
    }))
}

enum Wait {
    Forever,
    Budget(Duration),
    Poll,
}

fn overlapped_transfer(
    handle: HANDLE,
    buf: *mut u8,
    len: usize,
    write: bool,
    wait: Wait,
) -> io::Result<usize> {
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
        return Ok(transferred as usize);
    }
    let error = unsafe { GetLastError() };
    if error != ERROR_IO_PENDING {
        return map_completed_error(error, write);
    }
    let millis = match wait {
        Wait::Forever => INFINITE,
        Wait::Budget(duration) => duration_millis(duration),
        Wait::Poll => 0,
    };
    let status = unsafe { WaitForSingleObject(event.raw(), millis) };
    if status == WAIT_OBJECT_0 {
        return overlapped_result(handle, &overlapped, write, false);
    }
    // 超时或等待本身失败。缓冲和 OVERLAPPED 都在调用方的栈上，返回前取消并等这次读写真的结束。
    let wait_error = (status != WAIT_TIMEOUT).then(io::Error::last_os_error);
    unsafe { CancelIoEx(handle, &overlapped) };
    match overlapped_result(handle, &overlapped, write, true) {
        Ok(transferred) => Ok(transferred),
        Err(error) if error.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32) => {
            Err(wait_error.unwrap_or_else(|| {
                if matches!(wait, Wait::Poll) {
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
                }
            }))
        }
        Err(error) => Err(error),
    }
}

/// `wait` 为真时等到这次读写结束，取消之后用。
fn overlapped_result(
    handle: HANDLE,
    overlapped: &OVERLAPPED,
    write: bool,
    wait: bool,
) -> io::Result<usize> {
    let mut transferred = 0u32;
    let ok = unsafe {
        GetOverlappedResult(handle, overlapped, &mut transferred, i32::from(wait))
    };
    if ok != 0 {
        return Ok(transferred as usize);
    }
    map_completed_error(unsafe { GetLastError() }, write)
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

fn wait_for_client(handle: OwnedHandle) -> io::Result<OwnedHandle> {
    let event = ManualEvent::new()?;
    let mut overlapped = OVERLAPPED {
        hEvent: event.raw(),
        ..OVERLAPPED::default()
    };
    let connected = unsafe { ConnectNamedPipe(handle.as_raw_handle(), &mut overlapped) };
    if connected != 0 {
        return Ok(handle);
    }
    let error = unsafe { GetLastError() };
    if error == ERROR_PIPE_CONNECTED {
        return Ok(handle);
    }
    if error != ERROR_IO_PENDING {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let status = unsafe { WaitForSingleObject(event.raw(), INFINITE) };
    if status != WAIT_OBJECT_0 {
        unsafe { CancelIoEx(handle.as_raw_handle(), &overlapped) };
        unsafe { WaitForSingleObject(event.raw(), INFINITE) };
        return Err(io::Error::last_os_error());
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
    Ok(handle)
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

fn is_pipe_path(name: &str) -> bool {
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
        let name = user_pipe_path(&format!("test-first-{}", uuid::Uuid::new_v4().simple())).unwrap();
        let _first = PipeListener::bind(&name).unwrap();
        assert!(PipeListener::bind(&name).is_err());
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
