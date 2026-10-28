//! Windows 的 AF_UNIX：从不在驱动上挂阻塞的调用。
//!
//! 系统驱动 afunix.sys 有个竞态：阻塞的 recv 刚挂上时对端恰好写入或关闭，这次唤醒偶尔
//! 丢掉，recv 一直挂着；设了读超时就以 10060 超时返回，其实数据或 EOF 早就到了。本地
//! `shutdown` 也叫不醒另一条线程里阻塞着的 recv。
//!
//! 所以套接字从建出来起就是非阻塞的，读、写、accept、connect 都先试，遇到 WouldBlock
//! 就用 AFD poll（WSAPoll 底下的那层，就绪报得准）等它就绪，再试。poll 是一次可取消的
//! 重叠请求，和本地 `shutdown` 的事件、读写超时一起等，到了就取消它。对外的阻塞语义
//! （读写超时、`set_nonblocking`、`try_clone` 共享这些设置、`shutdown` 叫醒等着的读写）
//! 和 std 的 `UnixStream` 一致。

use std::ffi::c_void;
use std::fmt;
use std::io::{self, Read, Write};
use std::mem::{size_of, zeroed};
use std::net::Shutdown;
use std::os::windows::io::{
    AsRawHandle, AsRawSocket, FromRawHandle, FromRawSocket, OwnedHandle, OwnedSocket, RawSocket,
};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{FILE_OPEN, NtCancelIoFileEx, NtCreateFile};
use windows_sys::Wdk::System::IO::NtDeviceIoControlFile;
use windows_sys::Win32::Foundation::{
    HANDLE, HANDLE_FLAG_INHERIT, NTSTATUS, RtlNtStatusToDosError, STATUS_PENDING, STATUS_SUCCESS,
    SetHandleInformation, UNICODE_STRING, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Networking::WinSock::{
    AF_UNIX, FIONBIO, INVALID_SOCKET, SD_BOTH, SD_RECEIVE, SD_SEND, SIO_BASE_HANDLE, SO_ERROR,
    SOCK_STREAM, SOCKADDR_UN, SOCKET, SOCKET_ERROR, SOL_SOCKET, WSA_FLAG_NO_HANDLE_INHERIT,
    WSA_FLAG_OVERLAPPED, WSADATA, WSAETIMEDOUT, WSAGetLastError, WSAIoctl, WSASocketW, WSAStartup,
    accept, bind, connect, getsockopt, ioctlsocket, listen, recv, send, shutdown,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE};
use windows_sys::Win32::System::IO::{IO_STATUS_BLOCK, IO_STATUS_BLOCK_0};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};

/// 一条 AF_UNIX 连接。`try_clone` 出来的各份共用读写超时、非阻塞设置和 shutdown 状态，
/// 和 Unix 上 dup 出来的描述符一样。
pub struct UnixStream {
    socket: OwnedSocket,
    /// AFD poll 要的是基础套接字句柄，不是可能被分层服务包过的那个。
    base: SOCKET,
    shared: Arc<Shared>,
}

struct Shared {
    read_timeout: Mutex<Option<Duration>>,
    write_timeout: Mutex<Option<Duration>>,
    nonblocking: AtomicBool,
    read_shut: Signal,
    write_shut: Signal,
}

impl Shared {
    fn new() -> io::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            read_timeout: Mutex::new(None),
            write_timeout: Mutex::new(None),
            nonblocking: AtomicBool::new(false),
            read_shut: Signal::new()?,
            write_shut: Signal::new()?,
        }))
    }
}

/// 本地 `shutdown` 了某个方向：置位后不再复位，等着的读或写由它叫醒。
struct Signal {
    set: AtomicBool,
    /// 手动复位事件，和 poll 一起等。
    event: OwnedHandle,
}

impl Signal {
    fn new() -> io::Result<Self> {
        Ok(Self {
            set: AtomicBool::new(false),
            event: new_event()?,
        })
    }

    fn set(&self) {
        // 先置标志再置事件：读写在循环开头看标志，看漏了也会被事件叫醒。
        self.set.store(true, Ordering::SeqCst);
        // SAFETY: 事件句柄由本结构持有，活到结构丢掉。
        unsafe { SetEvent(self.event.as_raw_handle()) };
    }

    fn is_set(&self) -> bool {
        self.set.load(Ordering::SeqCst)
    }
}

impl UnixStream {
    pub fn connect<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let pending = Connecting::start(path.as_ref())?;
        pending.finish()
    }

    /// 一对连好的流。Windows 没有 socketpair：在临时目录里起一次性的监听，连上就删。
    pub fn pair() -> io::Result<(Self, Self)> {
        let path = Unlink(std::env::temp_dir().join(format!(
            "smelt-{:016x}.sock",
            uuid::Uuid::new_v4().as_u64_pair().0
        )));
        let listener = UnixListener::bind(&path.0)?;
        // connect 不等对端 accept 也可能先完成，也可能要等；先发起，accept 之后再收尾。
        let connecting = Connecting::start(&path.0)?;
        let accepted = listener.accept()?;
        Ok((accepted, connecting.finish()?))
    }

    fn from_socket(socket: OwnedSocket, shared: Arc<Shared>) -> io::Result<Self> {
        let raw = socket.as_raw_socket() as SOCKET;
        set_nonblocking(raw)?;
        Ok(Self {
            base: base_socket(raw)?,
            socket,
            shared,
        })
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Self::from_socket(self.socket.try_clone()?, Arc::clone(&self.shared))
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        let how_raw = match how {
            Shutdown::Read => SD_RECEIVE,
            Shutdown::Write => SD_SEND,
            Shutdown::Both => SD_BOTH,
        };
        // SAFETY: 套接字由本结构持有。
        let result = unsafe { shutdown(self.raw(), how_raw) };
        // 连接已经断了时系统 shutdown 会报错，等着的读写仍要叫醒。
        if matches!(how, Shutdown::Read | Shutdown::Both) {
            self.shared.read_shut.set();
        }
        if matches!(how, Shutdown::Write | Shutdown::Both) {
            self.shared.write_shut.set();
        }
        if result == SOCKET_ERROR {
            return Err(last_wsa_error());
        }
        Ok(())
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        *self.shared.read_timeout.lock().unwrap() = checked_timeout(timeout)?;
        Ok(())
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        *self.shared.write_timeout.lock().unwrap() = checked_timeout(timeout)?;
        Ok(())
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(*self.shared.read_timeout.lock().unwrap())
    }

    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(*self.shared.write_timeout.lock().unwrap())
    }

    /// 只改对外的语义：为真时读写不等，直接报 WouldBlock。套接字本身一直是非阻塞的。
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.shared.nonblocking.store(nonblocking, Ordering::SeqCst);
        Ok(())
    }

    fn raw(&self) -> SOCKET {
        self.socket.as_raw_socket() as SOCKET
    }

    fn deadline(timeout: &Mutex<Option<Duration>>) -> Option<Instant> {
        timeout
            .lock()
            .unwrap()
            .map(|timeout| Instant::now() + timeout)
    }

    fn read_bytes(&self, buf: &mut [u8]) -> io::Result<usize> {
        let deadline = Self::deadline(&self.shared.read_timeout);
        let len = buf.len().min(i32::MAX as usize) as i32;
        loop {
            // 和 Unix 一致：本地关了读方向，读到的就是 EOF。
            if self.shared.read_shut.is_set() {
                return Ok(0);
            }
            // SAFETY: buf 可写，长度如实传入。
            let read = unsafe { recv(self.raw(), buf.as_mut_ptr(), len, 0) };
            if read != SOCKET_ERROR {
                return Ok(read as usize);
            }
            let error = last_wsa_error();
            if error.kind() != io::ErrorKind::WouldBlock
                || self.shared.nonblocking.load(Ordering::SeqCst)
            {
                return Err(error);
            }
            match wait(self.base, READABLE, Some(&self.shared.read_shut), deadline)? {
                Wake::Ready => {}
                Wake::Shut => return Ok(0),
                Wake::TimedOut => return Err(io::Error::from_raw_os_error(WSAETIMEDOUT)),
            }
        }
    }

    fn write_bytes(&self, buf: &[u8]) -> io::Result<usize> {
        let deadline = Self::deadline(&self.shared.write_timeout);
        let len = buf.len().min(i32::MAX as usize) as i32;
        loop {
            if self.shared.write_shut.is_set() {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            // SAFETY: buf 可读，长度如实传入。
            let sent = unsafe { send(self.raw(), buf.as_ptr(), len, 0) };
            if sent != SOCKET_ERROR {
                return Ok(sent as usize);
            }
            let error = last_wsa_error();
            if error.kind() != io::ErrorKind::WouldBlock
                || self.shared.nonblocking.load(Ordering::SeqCst)
            {
                return Err(error);
            }
            match wait(self.base, WRITABLE, Some(&self.shared.write_shut), deadline)? {
                Wake::Ready => {}
                Wake::Shut => return Err(io::ErrorKind::BrokenPipe.into()),
                Wake::TimedOut => return Err(io::Error::from_raw_os_error(WSAETIMEDOUT)),
            }
        }
    }
}

impl fmt::Debug for UnixStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnixStream")
            .field("socket", &self.socket.as_raw_socket())
            .finish()
    }
}

impl AsRawSocket for UnixStream {
    fn as_raw_socket(&self) -> RawSocket {
        self.socket.as_raw_socket()
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
        self.write_bytes(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Write for &UnixStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_bytes(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 发起了、还没确认连上的 connect。
struct Connecting {
    socket: OwnedSocket,
    base: SOCKET,
    /// connect 当场就成功了，不用再等。
    connected: bool,
}

impl Connecting {
    fn start(path: &Path) -> io::Result<Self> {
        let (addr, len) = sockaddr_un(path)?;
        let socket = new_socket()?;
        let raw = socket.as_raw_socket() as SOCKET;
        let base = base_socket(raw)?;
        // SAFETY: addr 是按 len 填好的 sockaddr_un。
        let result = unsafe { connect(raw, (&addr as *const SOCKADDR_UN).cast(), len) };
        let connected = if result == SOCKET_ERROR {
            let error = last_wsa_error();
            if error.kind() != io::ErrorKind::WouldBlock {
                return Err(error);
            }
            false
        } else {
            true
        };
        Ok(Self {
            socket,
            base,
            connected,
        })
    }

    /// 等连上（可写或连接失败），失败原因从 SO_ERROR 取。
    fn finish(self) -> io::Result<UnixStream> {
        if !self.connected {
            wait(self.base, CONNECTED, None, None)?;
            let raw = self.socket.as_raw_socket() as SOCKET;
            let mut error = 0i32;
            let mut len = size_of::<i32>() as i32;
            // SAFETY: 输出缓冲是一个 i32，长度如实传入。
            let result = unsafe {
                getsockopt(
                    raw,
                    SOL_SOCKET,
                    SO_ERROR,
                    (&mut error as *mut i32).cast(),
                    &mut len,
                )
            };
            if result == SOCKET_ERROR {
                return Err(last_wsa_error());
            }
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error));
            }
        }
        UnixStream::from_socket(self.socket, Shared::new()?)
    }
}

/// 监听端。接受进来的连接是本模块的 [`UnixStream`]。
pub struct UnixListener {
    socket: OwnedSocket,
    base: SOCKET,
}

impl UnixListener {
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let (addr, len) = sockaddr_un(path.as_ref())?;
        let socket = new_socket()?;
        let raw = socket.as_raw_socket() as SOCKET;
        // SAFETY: addr 是按 len 填好的 sockaddr_un。
        if unsafe { bind(raw, (&addr as *const SOCKADDR_UN).cast(), len) } == SOCKET_ERROR {
            return Err(last_wsa_error());
        }
        // SAFETY: 刚绑定的套接字。
        if unsafe { listen(raw, 128) } == SOCKET_ERROR {
            return Err(last_wsa_error());
        }
        Ok(Self {
            base: base_socket(raw)?,
            socket,
        })
    }

    /// 等下一个连接。没有超时，也不能被叫停：守护的接受循环就是一直等。
    fn accept(&self) -> io::Result<UnixStream> {
        let raw = self.socket.as_raw_socket() as SOCKET;
        loop {
            // SAFETY: 不要对端地址，两个指针都给空。
            let accepted = unsafe { accept(raw, null_mut(), null_mut()) };
            if accepted != INVALID_SOCKET {
                // SAFETY: accept 刚交出的套接字，归 OwnedSocket 关闭。
                let socket = unsafe { OwnedSocket::from_raw_socket(accepted as RawSocket) };
                // accept 出来的句柄不保证继承监听端的「不可继承」，子进程不该拿到它。
                // SAFETY: 句柄有效。
                if unsafe { SetHandleInformation(accepted as HANDLE, HANDLE_FLAG_INHERIT, 0) } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                return UnixStream::from_socket(socket, Shared::new()?);
            }
            let error = last_wsa_error();
            if error.kind() != io::ErrorKind::WouldBlock {
                return Err(error);
            }
            wait(self.base, ACCEPTABLE, None, None)?;
        }
    }

    pub fn incoming(&self) -> impl Iterator<Item = io::Result<UnixStream>> + '_ {
        std::iter::repeat_with(|| self.accept())
    }
}

impl fmt::Debug for UnixListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnixListener")
            .field("socket", &self.socket.as_raw_socket())
            .finish()
    }
}

impl AsRawSocket for UnixListener {
    fn as_raw_socket(&self) -> RawSocket {
        self.socket.as_raw_socket()
    }
}

/// `pair` 的一次性监听文件，连好就删。
struct Unlink(PathBuf);

impl Drop for Unlink {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// 建一个不可继承、可重叠、非阻塞的 AF_UNIX 套接字。
fn new_socket() -> io::Result<OwnedSocket> {
    startup()?;
    // SAFETY: 不带协议信息，参数都是常量。
    let raw = unsafe {
        WSASocketW(
            i32::from(AF_UNIX),
            SOCK_STREAM,
            0,
            null(),
            0,
            WSA_FLAG_OVERLAPPED | WSA_FLAG_NO_HANDLE_INHERIT,
        )
    };
    if raw == INVALID_SOCKET {
        return Err(last_wsa_error());
    }
    // SAFETY: 刚建出来的套接字，归 OwnedSocket 关闭。
    let socket = unsafe { OwnedSocket::from_raw_socket(raw as RawSocket) };
    set_nonblocking(raw)?;
    Ok(socket)
}

/// Winsock 用之前要初始化一次。只做一次，进程退出前不清理。
fn startup() -> io::Result<()> {
    static STARTED: OnceLock<i32> = OnceLock::new();
    let code = *STARTED.get_or_init(|| {
        // SAFETY: WSADATA 是纯数据，全零合法，由 WSAStartup 填写。
        let mut data: WSADATA = unsafe { zeroed() };
        // SAFETY: data 可写；要 Winsock 2.2。
        unsafe { WSAStartup(0x202, &mut data) }
    });
    if code != 0 {
        return Err(io::Error::from_raw_os_error(code));
    }
    Ok(())
}

fn set_nonblocking(socket: SOCKET) -> io::Result<()> {
    let mut on = 1u32;
    // SAFETY: socket 是调用方持有的有效套接字。
    if unsafe { ioctlsocket(socket, FIONBIO, &mut on) } == SOCKET_ERROR {
        return Err(last_wsa_error());
    }
    Ok(())
}

/// Winsock 的 `sun_path` 是 UTF-8 的 Win32 路径，以 0 结尾。
fn sockaddr_un(path: &Path) -> io::Result<(SOCKADDR_UN, i32)> {
    let bytes = path.to_str().map(str::as_bytes).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path is not valid UTF-8",
        )
    })?;
    let mut addr = SOCKADDR_UN {
        sun_family: AF_UNIX,
        ..SOCKADDR_UN::default()
    };
    if bytes.contains(&0) || bytes.len() >= addr.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path has a nul byte or is too long",
        ));
    }
    for (dst, src) in addr.sun_path.iter_mut().zip(bytes) {
        *dst = *src as i8;
    }
    let len = size_of::<u16>() + bytes.len() + 1;
    Ok((addr, len as i32))
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

fn last_wsa_error() -> io::Error {
    // SAFETY: 只读本线程最近一次 Winsock 错误。
    io::Error::from_raw_os_error(unsafe { WSAGetLastError() })
}

fn nt_error(status: NTSTATUS) -> io::Error {
    // SAFETY: 纯转换，不碰内存。
    io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32)
}

fn new_event() -> io::Result<OwnedHandle> {
    // SAFETY: 无名的手动复位事件，初始未置位。
    let event = unsafe { CreateEventW(null(), 1, 0, null()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: 刚建出来的句柄，交给 OwnedHandle 关闭。
    Ok(unsafe { OwnedHandle::from_raw_handle(event) })
}

fn base_socket(socket: SOCKET) -> io::Result<SOCKET> {
    let mut base: SOCKET = 0;
    let mut returned = 0u32;
    // SAFETY: 输出缓冲是一个 SOCKET 大小，同步调用。
    let result = unsafe {
        WSAIoctl(
            socket,
            SIO_BASE_HANDLE,
            null(),
            0,
            (&mut base as *mut SOCKET).cast(),
            size_of::<SOCKET>() as u32,
            &mut returned,
            null_mut(),
            None,
        )
    };
    if result == SOCKET_ERROR {
        return Err(last_wsa_error());
    }
    Ok(base)
}

// AFD poll：Winsock 的 WSAPoll、mio、libuv、wepoll 用的都是它。结构和常量没有公开头文件，
// 取值与 mio 的 `sys/windows/afd.rs` 一致。
const IOCTL_AFD_POLL: u32 = 0x0001_2024;
const AFD_POLL_RECEIVE: u32 = 0x0001;
const AFD_POLL_SEND: u32 = 0x0004;
const AFD_POLL_DISCONNECT: u32 = 0x0008;
const AFD_POLL_ABORT: u32 = 0x0010;
const AFD_POLL_LOCAL_CLOSE: u32 = 0x0020;
const AFD_POLL_ACCEPT: u32 = 0x0080;
const AFD_POLL_CONNECT_FAIL: u32 = 0x0100;

/// 有数据、对端写完、连接出错，读都该醒来再试。
const READABLE: u32 =
    AFD_POLL_RECEIVE | AFD_POLL_DISCONNECT | AFD_POLL_ABORT | AFD_POLL_LOCAL_CLOSE;
/// 写不等 DISCONNECT：对端只是写完时它一直置位，缓冲仍满就会空转。
const WRITABLE: u32 = AFD_POLL_SEND | AFD_POLL_ABORT | AFD_POLL_LOCAL_CLOSE;
/// 连上了就可写；连不上报 CONNECT_FAIL，原因在 SO_ERROR 里。
const CONNECTED: u32 =
    AFD_POLL_SEND | AFD_POLL_CONNECT_FAIL | AFD_POLL_ABORT | AFD_POLL_LOCAL_CLOSE;
const ACCEPTABLE: u32 = AFD_POLL_ACCEPT | AFD_POLL_LOCAL_CLOSE;

#[repr(C)]
struct AfdPollHandleInfo {
    handle: HANDLE,
    events: u32,
    status: NTSTATUS,
}

#[repr(C)]
struct AfdPollInfo {
    timeout: i64,
    number_of_handles: u32,
    exclusive: u32,
    handles: [AfdPollHandleInfo; 1],
}

/// 进程共用的 AFD 设备句柄，poll 请求都发到它上面。打不开时每次等待都报这个错。
fn afd() -> io::Result<HANDLE> {
    static AFD: OnceLock<Result<OwnedHandle, i32>> = OnceLock::new();
    match AFD.get_or_init(open_afd) {
        Ok(handle) => Ok(handle.as_raw_handle()),
        Err(code) => Err(io::Error::from_raw_os_error(*code)),
    }
}

fn open_afd() -> Result<OwnedHandle, i32> {
    let name: Vec<u16> = r"\Device\Afd\Smelt".encode_utf16().collect();
    let bytes = (name.len() * size_of::<u16>()) as u16;
    let object_name = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: name.as_ptr() as *mut u16,
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: null_mut(),
        ObjectName: &object_name,
        Attributes: 0,
        SecurityDescriptor: null(),
        SecurityQualityOfService: null(),
    };
    let mut handle: HANDLE = null_mut();
    let mut iosb = IO_STATUS_BLOCK::default();
    // SAFETY: 各指针指向本函数里活着的局部变量；不带同步 I/O 选项，句柄上的请求是异步的。
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            SYNCHRONIZE,
            &attributes,
            &mut iosb,
            null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_OPEN,
            0,
            null(),
            0,
        )
    };
    if status != STATUS_SUCCESS {
        return Err(nt_error(status).raw_os_error().unwrap_or_default());
    }
    // SAFETY: 刚打开的句柄，交给 OwnedHandle 关闭。
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

enum Wake {
    /// 有事件了（也可能是假醒），调用方再试一次读写。
    Ready,
    Shut,
    TimedOut,
}

/// 等 `base` 上出现 `events` 里的事件，或 `shut` 置位，或到 `deadline`。
fn wait(
    base: SOCKET,
    events: u32,
    shut: Option<&Signal>,
    deadline: Option<Instant>,
) -> io::Result<Wake> {
    let afd = afd()?;
    let done = new_event()?;
    let mut info = AfdPollInfo {
        timeout: i64::MAX,
        number_of_handles: 1,
        exclusive: 0,
        handles: [AfdPollHandleInfo {
            handle: base as HANDLE,
            events,
            status: 0,
        }],
    };
    let mut iosb = IO_STATUS_BLOCK {
        Anonymous: IO_STATUS_BLOCK_0 {
            Status: STATUS_PENDING,
        },
        Information: 0,
    };
    let info_ptr: *mut c_void = (&mut info as *mut AfdPollInfo).cast();
    let iosb_ptr: *mut IO_STATUS_BLOCK = &mut iosb;
    // SAFETY: info 和 iosb 在请求完成前一直活着：下面每条路径要么等到它完成，要么先
    // 取消再等到它完成，才离开本函数。
    let status = unsafe {
        NtDeviceIoControlFile(
            afd,
            done.as_raw_handle(),
            None,
            null(),
            iosb_ptr,
            IOCTL_AFD_POLL,
            info_ptr,
            size_of::<AfdPollInfo>() as u32,
            info_ptr,
            size_of::<AfdPollInfo>() as u32,
        )
    };
    if status != STATUS_PENDING {
        return if status < 0 {
            Err(nt_error(status))
        } else {
            Ok(Wake::Ready)
        };
    }

    let waited = match shut {
        Some(shut) => {
            let handles = [done.as_raw_handle(), shut.event.as_raw_handle()];
            // SAFETY: 两个句柄都活着。同时就绪时返回下标小的，即 poll 完成。
            unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, millis_until(deadline)) }
        }
        // SAFETY: 句柄活着。
        None => unsafe { WaitForSingleObject(done.as_raw_handle(), millis_until(deadline)) },
    };
    let outcome = match waited {
        WAIT_OBJECT_0 => {
            // SAFETY: 请求已完成，iosb 不再被驱动写。
            let status = unsafe { (*iosb_ptr).Anonymous.Status };
            return if status < 0 {
                Err(nt_error(status))
            } else {
                Ok(Wake::Ready)
            };
        }
        WAIT_TIMEOUT => Ok(Wake::TimedOut),
        _ if waited == WAIT_OBJECT_0 + 1 => Ok(Wake::Shut),
        _ => Err(io::Error::last_os_error()),
    };
    let mut cancel = IO_STATUS_BLOCK::default();
    // SAFETY: iosb_ptr 是还挂着（或刚完成）的那个请求；已经完成时返回 STATUS_NOT_FOUND，
    // 不影响。之后等它真正完成，info 和 iosb 才能出作用域。
    unsafe {
        NtCancelIoFileEx(afd, iosb_ptr, &mut cancel);
        WaitForSingleObject(done.as_raw_handle(), INFINITE);
    }
    outcome
}

fn millis_until(deadline: Option<Instant>) -> u32 {
    let Some(deadline) = deadline else {
        return INFINITE;
    };
    let left = deadline.saturating_duration_since(Instant::now());
    // 向上取整，免得差不到一毫秒时醒得太早又回来等。
    left.as_nanos()
        .div_ceil(1_000_000)
        .min(u128::from(INFINITE - 1)) as u32
}
