//! Windows 命名管道。Bun / Node 的 `net.connect(path)` 在 Windows 上连的是
//! `\\.\pipe\...`，不是文件系统上的 AF_UNIX。交给 Bun 的 `SMELT_SOCK` 和后台
//! 任务套接字都用这里，不把管道桥接进 AF_UNIX——桥的对端会变成 smeltd 自己，
//! 事件鉴权会拒绝。
//!
//! 管道拒绝远程客户端，DACL 只给当前用户。接受之后用
//! `GetNamedPipeClientProcessId` 拿对端 pid。字节管道没有套接字那种接收超时，
//! 超时靠 `PeekNamedPipe` 等到有字节、管道断开，或期限到了。

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::{
    ACL_REVISION, AddAccessAllowedAce, GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor,
    SECURITY_ATTRIBUTES, SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PeekNamedPipe, SetNamedPipeHandleState,
    PIPE_NOWAIT, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::process::current_user_key;

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
}

impl PipeListener {
    pub fn bind(name: impl Into<String>) -> io::Result<Self> {
        let name = name.into();
        if !name.starts_with(r"\\.\pipe\") && !name.starts_with(r"\\?\pipe\") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("命名管道路径必须以 \\\\.\\pipe\\ 开头：{name}"),
            ));
        }
        Ok(Self { name })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// 新建一个实例并阻塞到有客户端连上。每个连接一个实例。
    pub fn accept(&self) -> io::Result<PipeStream> {
        let handle = create_instance(&self.name)?;
        let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
        if connected == 0 {
            let error = unsafe { GetLastError() };
            if error != ERROR_PIPE_CONNECTED {
                unsafe { CloseHandle(handle) };
                return Err(io::Error::from_raw_os_error(error as i32));
            }
        }
        let file = unsafe { File::from_raw_handle(handle) };
        Ok(PipeStream {
            file,
            read_timeout: None,
            write_timeout: None,
        })
    }
}

pub struct PipeStream {
    file: File,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl PipeStream {
    /// 连上已经在听的管道。`name` 是 `\\.\pipe\...`。
    pub fn connect(name: &str) -> io::Result<Self> {
        let wide = wide(name);
        // GENERIC_READ | GENERIC_WRITE。客户端独占这一实例。
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                0x8000_0000 | 0x4000_0000,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            file: unsafe { File::from_raw_handle(handle) },
            read_timeout: None,
            write_timeout: None,
        })
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        let mut duplicated: HANDLE = std::ptr::null_mut();
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.file.as_raw_handle(),
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
        Ok(Self {
            file: unsafe { File::from_raw_handle(duplicated) },
            read_timeout: self.read_timeout,
            write_timeout: self.write_timeout,
        })
    }

    /// `PIPE_NOWAIT` 或 `PIPE_WAIT`。读模式保持字节流。
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        set_wait_mode(self.file.as_raw_handle(), nonblocking)
    }

    /// 存下读超时。有期限时改成 `PIPE_WAIT`：真正的等待是 `PeekNamedPipe`，
    /// 不是套接字接收超时。
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.read_timeout = timeout;
        if timeout.is_some() {
            self.set_nonblocking(false)?;
        }
        Ok(())
    }

    /// 存下写超时。有期限时改成 `PIPE_WAIT`。
    pub fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.write_timeout = timeout;
        if timeout.is_some() {
            self.set_nonblocking(false)?;
        }
        Ok(())
    }

    pub fn client_pid(&self) -> io::Result<u32> {
        let mut pid = 0u32;
        if unsafe { GetNamedPipeClientProcessId(self.file.as_raw_handle(), &mut pid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            return Err(io::Error::other("命名管道对端没有进程号"));
        }
        Ok(pid)
    }
}

impl AsRawHandle for PipeStream {
    fn as_raw_handle(&self) -> RawHandle {
        self.file.as_raw_handle()
    }
}

impl Read for PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if let Some(timeout) = self.read_timeout {
            // 有期限就先 Peek，到点、有字节或管道断开再 ReadFile。PIPE_WAIT 下
            // 没有数据时 ReadFile 会一直堵着。
            wait_until_readable(&self.file, timeout)?;
        }
        read_file(&self.file, buf)
    }
}

impl Write for PipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if let Some(timeout) = self.write_timeout {
            // 满缓冲时阻塞的 WriteFile 会越过期限。Peek 确认管道还在，再以
            // PIPE_NOWAIT 写；缓冲满就停 15ms 重试。期限请求本身把句柄留在
            // PIPE_WAIT，尝试前后改回去。
            let deadline = Instant::now() + timeout;
            self.set_nonblocking(true)?;
            let result = loop {
                match peek_pipe(self.file.as_raw_handle()) {
                    Ok(PipePeek::Broken) => {
                        break Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "命名管道已断开",
                        ));
                    }
                    Ok(_) => {}
                    Err(error) => break Err(error),
                }
                match write_file(&self.file, buf) {
                    Ok(written) => break Ok(written),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            break Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "命名管道写入超时",
                            ));
                        }
                        thread::park_timeout(Duration::from_millis(15));
                    }
                    Err(error) => break Err(error),
                }
            };
            let _ = self.set_nonblocking(false);
            return result;
        }
        write_file(&self.file, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// 匿名管道的写端也要能改成非阻塞，免得唤醒写在缓冲满时卡住持锁的线程。
pub fn set_nonblocking_handle(handle: RawHandle, nonblocking: bool) -> io::Result<()> {
    set_wait_mode(handle, nonblocking)
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

fn set_wait_mode(handle: RawHandle, nonblocking: bool) -> io::Result<()> {
    let mode = PIPE_READMODE_BYTE | if nonblocking { PIPE_NOWAIT } else { PIPE_WAIT };
    let ok = unsafe {
        SetNamedPipeHandleState(handle, &mode, std::ptr::null(), std::ptr::null())
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

enum PipePeek {
    Empty,
    Ready(u32),
    Broken,
}

fn wait_until_readable(file: &File, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match peek_pipe(file.as_raw_handle())? {
            PipePeek::Ready(_) | PipePeek::Broken => return Ok(()),
            PipePeek::Empty => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "命名管道读取超时"));
                }
                thread::park_timeout(Duration::from_millis(15));
            }
        }
    }
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

fn read_file(file: &File, buf: &mut [u8]) -> io::Result<usize> {
    let mut transferred = 0u32;
    let ok = unsafe {
        ReadFile(
            file.as_raw_handle(),
            buf.as_mut_ptr(),
            buf.len().min(u32::MAX as usize) as u32,
            &mut transferred,
            std::ptr::null_mut(),
        )
    };
    map_transfer(ok, transferred, false)
}

fn write_file(file: &File, buf: &[u8]) -> io::Result<usize> {
    let mut transferred = 0u32;
    let ok = unsafe {
        WriteFile(
            file.as_raw_handle(),
            buf.as_ptr(),
            buf.len().min(u32::MAX as usize) as u32,
            &mut transferred,
            std::ptr::null_mut(),
        )
    };
    map_transfer(ok, transferred, true)
}

fn map_transfer(ok: i32, transferred: u32, write: bool) -> io::Result<usize> {
    if ok == 0 {
        let error = unsafe { GetLastError() };
        if error == ERROR_NO_DATA {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "命名管道没有数据",
            ));
        }
        if !write && (error == ERROR_BROKEN_PIPE || error == ERROR_PIPE_NOT_CONNECTED || error == 0)
        {
            return Ok(0);
        }
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    Ok(transferred as usize)
}

fn create_instance(name: &str) -> io::Result<HANDLE> {
    let wide = wide(name);
    let mut security = UserOnlySecurity::new()?;
    security.attributes.lpSecurityDescriptor = security.descriptor.as_mut_ptr().cast();
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX,
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
    Ok(handle)
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

fn wide(value: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    Path::new(value).as_os_str().encode_wide().chain(Some(0)).collect()
}

#[allow(dead_code)]
fn _raw(file: &File) -> RawHandle {
    file.as_raw_handle()
}
