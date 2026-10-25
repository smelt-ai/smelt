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
    GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
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
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId, PeekNamedPipe,
    WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, INFINITE, OpenProcessToken, WaitForSingleObject,
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
        let pending = create_instance(&name)?;
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
                None => create_instance(&self.name)?,
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
            && let Ok(handle) = create_instance(&self.name)
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

/// 匿名管道的写端也要能改成非阻塞，免得唤醒写在缓冲满时卡住持锁的线程。
/// 这是管道两端各自的句柄，不是 `PipeStream` 那种同一实例上复制出来的一对。
pub fn set_nonblocking_handle(handle: RawHandle, nonblocking: bool) -> io::Result<()> {
    use windows_sys::Win32::System::Pipes::{SetNamedPipeHandleState, PIPE_NOWAIT};
    let mode = PIPE_READMODE_BYTE | if nonblocking { PIPE_NOWAIT } else { PIPE_WAIT };
    let ok = unsafe { SetNamedPipeHandleState(handle, &mode, std::ptr::null(), std::ptr::null()) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// `PeekNamedPipe` 的结果。字节管道上等可读就靠这个，不把管道当成套接字。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PipeAvailability {
    Empty,
    Ready(u32),
    Broken,
}

pub fn peek_available(handle: RawHandle) -> io::Result<PipeAvailability> {
    match peek_pipe(handle)? {
        PipePeek::Empty => Ok(PipeAvailability::Empty),
        PipePeek::Ready(available) => Ok(PipeAvailability::Ready(available)),
        PipePeek::Broken => Ok(PipeAvailability::Broken),
    }
}

enum PipePeek {
    Empty,
    Ready(u32),
    Broken,
}

enum Wait {
    Forever,
    Budget(Duration),
    Poll,
}

fn peek_pipe(handle: RawHandle) -> io::Result<PipePeek> {
    let mut available = 0u32;
    let ok = unsafe {
        PeekNamedPipe(
            handle,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let error = unsafe { GetLastError() };
        if error == ERROR_BROKEN_PIPE || error == ERROR_PIPE_NOT_CONNECTED {
            return Ok(PipePeek::Broken);
        }
        if error == ERROR_NO_DATA {
            return Ok(PipePeek::Empty);
        }
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    if available == 0 {
        Ok(PipePeek::Empty)
    } else {
        Ok(PipePeek::Ready(available))
    }
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
        return overlapped_result(handle, &overlapped, write);
    }
    if status == WAIT_TIMEOUT {
        unsafe { CancelIoEx(handle, &overlapped) };
        unsafe { WaitForSingleObject(event.raw(), INFINITE) };
        return match overlapped_result(handle, &overlapped, write) {
            Ok(written) => Ok(written),
            Err(error) if error.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32) => {
                if matches!(wait, Wait::Poll) {
                    Err(io::Error::new(io::ErrorKind::WouldBlock, "命名管道没有数据"))
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        if write {
                            "命名管道写入超时"
                        } else {
                            "命名管道读取超时"
                        },
                    ))
                }
            }
            Err(error) => Err(error),
        };
    }
    Err(io::Error::last_os_error())
}

fn overlapped_result(handle: HANDLE, overlapped: &OVERLAPPED, write: bool) -> io::Result<usize> {
    let mut transferred = 0u32;
    let ok = unsafe { GetOverlappedResult(handle, overlapped, &mut transferred, 0) };
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

fn create_instance(name: &str) -> io::Result<OwnedHandle> {
    let wide = wide(name);
    let mut security = UserOnlySecurity::new()?;
    security.attributes.lpSecurityDescriptor = security.descriptor.as_mut_ptr().cast();
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
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
