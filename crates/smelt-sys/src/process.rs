//! 进程树。
//!
//! Unix 上根进程 `setpgid`，终止信号打到整个组。Windows 上根进程放进 Job Object，
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`：这个值丢掉、或者守护进程退出，组里的
//! 子进程和它们再拉起来的孙子一起结束。没有把「进程组」模拟成一串 `TerminateProcess`。

use std::io;
#[cfg(windows)]
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus};

/// 这个 pid 的进程还在。`pid == 0` 不是一个进程。
pub fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        if pid <= 0 {
            return false;
        }
        let signalled = unsafe { libc::kill(pid, 0) } == 0;
        signalled || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        windows::pid_is_alive(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

/// 结束这个 pid。Unix 是 `SIGKILL`，Windows 是 `TerminateProcess`。
/// 进程已经不在时返回 `Ok`。
pub fn terminate_pid(pid: u32) -> io::Result<()> {
    if pid == 0 {
        return Ok(());
    }
    #[cfg(unix)]
    {
        let pid = i32::try_from(pid).map_err(|_| io::Error::other("pid 超出范围"))?;
        if pid <= 0 {
            return Ok(());
        }
        if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
    #[cfg(windows)]
    {
        windows::terminate_pid(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "这个平台不能结束进程",
        ))
    }
}

/// 一个进程和它的后代。丢掉这个值时，Windows 上的 Job 句柄关闭，整棵树被杀掉。
/// Unix 上 drop 不发信号，和 `std::process::Child` 一样，调用方要自己 `kill`。
pub struct ProcessTree {
    child: Child,
    #[cfg(windows)]
    job: windows::OwnedHandle,
}

impl ProcessTree {
    /// 拉起 `command`。它和之后它自己拉起的子进程属于同一棵树。
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        Self::spawn_inner(command, false)
    }

    /// 和 [`spawn`] 一样，Windows 上额外带 `CREATE_NO_WINDOW`，不弹出控制台。
    pub fn spawn_no_window(command: &mut Command) -> io::Result<Self> {
        Self::spawn_inner(command, true)
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn id(&self) -> u32 {
        self.pid()
    }

    /// 请这棵树自己收尾。Unix 向进程组发 `SIGTERM`。Windows 没有对应的信号，
    /// 这里直接结束整棵树。
    pub fn terminate(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            let pid = i32::try_from(self.child.id()).unwrap_or(i32::MAX);
            if pid > 1 && unsafe { libc::kill(-pid, libc::SIGTERM) } != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            self.kill()
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::new(io::ErrorKind::Unsupported, "不能结束进程树"))
        }
    }

    /// 立刻结束整棵树。Unix 是 `SIGKILL` 进程组，Windows 是 `TerminateJobObject`。
    pub fn kill(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            let pid = i32::try_from(self.child.id()).unwrap_or(i32::MAX);
            if pid > 1 && unsafe { libc::kill(-pid, libc::SIGKILL) } != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            windows::terminate_job(self.job.raw())
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::new(io::ErrorKind::Unsupported, "不能结束进程树"))
        }
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    pub fn is_alive(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(Some(_)))
    }

    fn spawn_inner(command: &mut Command, no_window: bool) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let _ = no_window;
            let child = command.process_group(0).spawn()?;
            Ok(Self { child })
        }
        #[cfg(windows)]
        {
            windows::spawn_in_job(command, no_window)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (command, no_window);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "这个平台没有进程树",
            ))
        }
    }
}

/// 让一棵已经登记的进程树停下来。
///
/// Unix 打到进程组：`Interrupt` 是 `SIGTERM`，`Kill` 是 `SIGKILL`。
/// Windows 没有这两个信号。两种都结束已登记的 Job；没登记过的 pid 只结束它自己。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TreeSignal {
    Interrupt,
    Kill,
}

pub fn signal_tree(pid: u32, signal: TreeSignal) -> io::Result<()> {
    if pid <= 1 {
        return Ok(());
    }
    #[cfg(unix)]
    {
        let signo = match signal {
            TreeSignal::Interrupt => libc::SIGTERM,
            TreeSignal::Kill => libc::SIGKILL,
        };
        let pid = i32::try_from(pid).map_err(|_| io::Error::other("pid 超出范围"))?;
        if unsafe { libc::kill(-pid, signo) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
    #[cfg(windows)]
    {
        let _ = signal;
        windows::signal_tree(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = signal;
        Err(io::Error::new(io::ErrorKind::Unsupported, "不能结束进程树"))
    }
}

/// 放开这棵树的 Job。Windows 上句柄关掉之后，`KILL_ON_JOB_CLOSE` 会收掉还活着的
/// 后代。Unix 的进程组不靠这个值，调用是空的。
pub fn release_tree(pid: u32) {
    #[cfg(windows)]
    windows::release_tree(pid);
    #[cfg(not(windows))]
    {
        let _ = pid;
    }
}

/// 这个进程的创建时间。Windows 用 `GetProcessTimes`。其它平台不在这里查。
pub fn process_started_at(pid: u32) -> Option<std::time::SystemTime> {
    #[cfg(windows)]
    {
        windows::process_started_at(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        None
    }
}

/// 脱离当前控制台拉起一个进程，调用方不等它。守护进程用这个，避免被桌面的
/// Job 带着一起退出。
pub fn spawn_detached(command: &mut Command) -> io::Result<Child> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0).spawn()
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP
        const FLAGS: u32 = 0x0000_0008 | 0x0800_0000 | 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        const ERROR_ACCESS_DENIED: i32 = 5;
        match command
            .creation_flags(FLAGS | CREATE_BREAKAWAY_FROM_JOB)
            .spawn()
        {
            // 启动我们的终端、IDE 或 CI 把我们放进了不许脱离的 Job，带这个标志的
            // CreateProcess 直接拒绝。留在 Job 里也比起不来强，只是会随那个 Job 一起结束。
            Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
                command.creation_flags(FLAGS).spawn()
            }
            result => result,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = command;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "这个平台不能脱离启动进程",
        ))
    }
}

#[cfg(windows)]
pub fn af_unix_peer_pid(socket: std::os::windows::io::RawSocket) -> io::Result<u32> {
    windows::af_unix_peer_pid(socket)
}

#[cfg(windows)]
pub fn process_executable(pid: u32) -> io::Result<PathBuf> {
    windows::process_executable(pid)
}

#[cfg(windows)]
pub fn same_user_as_current(pid: u32) -> io::Result<bool> {
    windows::same_user_as_current(pid)
}

/// 一次 `OpenProcess` 同时取出「是不是当前用户」和映像路径。
/// 分两次打开的话，两次之间这个 pid 可能已经被复用。
#[cfg(windows)]
pub fn peer_user_and_executable(pid: u32) -> io::Result<(bool, PathBuf)> {
    windows::peer_user_and_executable(pid)
}

#[cfg(windows)]
pub fn current_user_key() -> io::Result<String> {
    windows::current_user_key()
}

/// 子进程必须已经带 `CREATE_SUSPENDED` 拉起。放进 Job、恢复线程，并按 pid 登记。
/// 登记失败时进程会被结束，不会留下一个挂起的、也不在 Job 里的进程。
#[cfg(windows)]
pub fn attach_suspended(pid: u32) -> io::Result<()> {
    windows::attach_suspended(pid)
}

/// 进程已经在跑。立刻放进 Job 并登记。来不及在 `CreateProcess` 之前挂起时用，
/// 例如 ConPTY 拉起的 shell。失败时进程还在，调用方决定要不要结束它。
#[cfg(windows)]
pub fn attach_running(pid: u32) -> io::Result<()> {
    windows::attach_running(pid)
}

#[cfg(windows)]
pub const CREATE_SUSPENDED: u32 = 0x0000_0004;

#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
mod windows {
    use super::{Command, ProcessTree};
    use std::ffi::OsString;
    use std::io;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, FILETIME, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
        WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Networking::WinSock::WSAIoctl;
    use windows_sys::Win32::Security::{
        GetLengthSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessTimes, INFINITE, OpenProcess,
        OpenProcessToken, OpenThread, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, QueryFullProcessImageNameW, ResumeThread,
        THREAD_SUSPEND_RESUME, TerminateProcess, WaitForSingleObject,
    };

    /// `_WSAIOR(IOC_VENDOR, 256)`，Winsock 文档里的 `SIO_AF_UNIX_GETPEERPID`。
    const SIO_AF_UNIX_GETPEERPID: u32 = 0x5800_0100;
    const CREATE_SUSPENDED: u32 = 0x0000_0004;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub struct OwnedHandle(HANDLE);

    unsafe impl Send for OwnedHandle {}
    unsafe impl Sync for OwnedHandle {}

    impl OwnedHandle {
        pub fn raw(&self) -> HANDLE {
            self.0
        }
    }

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    pub fn pid_is_alive(pid: u32) -> bool {
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, 0, pid) };
        if process.is_null() {
            return unsafe { GetLastError() } == ERROR_ACCESS_DENIED;
        }
        // 句柄带 SYNCHRONIZE。进程退出后这个句柄变成有信号。
        // `GetExitCodeProcess == STILL_ACTIVE` 会把真的以 259 退出的进程当成还活着。
        let running = unsafe { WaitForSingleObject(process, 0) } == WAIT_TIMEOUT;
        unsafe { CloseHandle(process) };
        running
    }

    pub fn terminate_pid(pid: u32) -> io::Result<()> {
        let process = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
        if process.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
                return Err(error);
            }
            return Ok(());
        }
        let killed = unsafe { TerminateProcess(process, 1) } != 0;
        if killed {
            unsafe { WaitForSingleObject(process, INFINITE) };
        }
        unsafe { CloseHandle(process) };
        if killed {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn terminate_job(job: HANDLE) -> io::Result<()> {
        if unsafe { TerminateJobObject(job, 1) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn spawn_in_job(command: &mut Command, no_window: bool) -> io::Result<ProcessTree> {
        let mut flags = CREATE_SUSPENDED;
        if no_window {
            flags |= CREATE_NO_WINDOW;
        }
        let mut child = command.creation_flags(flags).spawn()?;
        let job = match prepare_job() {
            Ok(job) => job,
            Err(error) => {
                let _ = terminate_pid(child.id());
                let _ = child.wait();
                return Err(error);
            }
        };
        let assigned = unsafe { AssignProcessToJobObject(job.raw(), child.as_raw_handle()) } != 0;
        let assign_error = if assigned {
            None
        } else {
            Some(io::Error::last_os_error())
        };
        if let Err(error) = resume_process(child.id()) {
            let _ = terminate_pid(child.id());
            let _ = child.wait();
            return Err(error);
        }
        if let Some(error) = assign_error {
            // 没放进 Job 的树，杀掉再报错。不能留下一个「调用方以为在 Job 里」的进程。
            let _ = terminate_pid(child.id());
            let _ = child.wait();
            return Err(error);
        }
        Ok(ProcessTree { child, job })
    }

    fn prepare_job() -> io::Result<OwnedHandle> {
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = OwnedHandle(job);
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&info) as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    fn resume_process(pid: u32) -> io::Result<()> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot.is_null() || snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let _snapshot = OwnedHandle(snapshot);
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut resumed = false;
        if unsafe { Thread32First(snapshot, &mut entry) } == 0 {
            return Err(io::Error::other("挂起的进程没有可恢复的线程"));
        }
        loop {
            if entry.th32OwnerProcessID == pid {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if !thread.is_null() {
                    let previous = unsafe { ResumeThread(thread) };
                    unsafe { CloseHandle(thread) };
                    if previous == u32::MAX {
                        return Err(io::Error::last_os_error());
                    }
                    resumed = true;
                }
            }
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            if unsafe { Thread32Next(snapshot, &mut entry) } == 0 {
                break;
            }
        }
        if resumed {
            Ok(())
        } else {
            Err(io::Error::other("挂起的进程没有可恢复的线程"))
        }
    }

    pub fn af_unix_peer_pid(socket: std::os::windows::io::RawSocket) -> io::Result<u32> {
        let mut pid = 0u32;
        let mut returned = 0u32;
        let rc = unsafe {
            WSAIoctl(
                socket as usize,
                SIO_AF_UNIX_GETPEERPID,
                std::ptr::null(),
                0,
                (&mut pid as *mut u32).cast(),
                std::mem::size_of::<u32>() as u32,
                &mut returned,
                std::ptr::null_mut(),
                None,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            return Err(io::Error::other("AF_UNIX 对端没有进程号"));
        }
        Ok(pid)
    }

    pub fn process_executable(pid: u32) -> io::Result<PathBuf> {
        let process = open_query(pid)?;
        let _process = OwnedHandle(process);
        image_path(process)
    }

    pub fn same_user_as_current(pid: u32) -> io::Result<bool> {
        Ok(peer_user_and_executable(pid)?.0)
    }

    pub fn peer_user_and_executable(pid: u32) -> io::Result<(bool, PathBuf)> {
        let process = open_query(pid)?;
        let _process = OwnedHandle(process);
        let current = user_sid_bytes(unsafe { GetCurrentProcess() })?;
        let peer = user_sid_bytes(process)?;
        let executable = image_path(process)?;
        Ok((current == peer, executable))
    }

    fn open_query(pid: u32) -> io::Result<HANDLE> {
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(process)
        }
    }

    fn image_path(process: HANDLE) -> io::Result<PathBuf> {
        let mut buf = vec![0u16; 32 * 1024];
        let mut len = buf.len() as u32;
        let ok = unsafe { QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut len) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(PathBuf::from(OsString::from_wide(&buf[..len as usize])))
    }

    pub fn current_user_key() -> io::Result<String> {
        let bytes = user_sid_bytes(unsafe { GetCurrentProcess() })?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    fn user_sid_bytes(process: HANDLE) -> io::Result<Vec<u8>> {
        let mut token: HANDLE = std::ptr::null_mut();
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let _token = OwnedHandle(token);
        let mut needed = 0u32;
        unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        }
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u8; needed as usize];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
        let sid = user.User.Sid;
        if sid.is_null() {
            return Err(io::Error::other("进程令牌没有用户 SID"));
        }
        let len = unsafe { GetLengthSid(sid) } as usize;
        if len == 0 {
            return Err(io::Error::other("进程令牌的用户 SID 长度为 0"));
        }
        let mut bytes = vec![0u8; len];
        unsafe {
            std::ptr::copy_nonoverlapping(sid.cast::<u8>(), bytes.as_mut_ptr(), len);
        }
        Ok(bytes)
    }

    fn jobs() -> &'static Mutex<HashMap<u32, OwnedHandle>> {
        static JOBS: OnceLock<Mutex<HashMap<u32, OwnedHandle>>> = OnceLock::new();
        JOBS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// 哪一步失败都结束进程。挂起的进程不会自己跑起来，留着只会让等它退出的人一直等。
    pub fn attach_suspended(pid: u32) -> io::Result<()> {
        match assign_and_resume(pid) {
            Ok(job) => {
                remember(pid, job);
                Ok(())
            }
            Err(error) => {
                let _ = terminate_pid(pid);
                Err(error)
            }
        }
    }

    fn assign_and_resume(pid: u32) -> io::Result<OwnedHandle> {
        let job = prepare_job()?;
        let process = open_for_job(pid)?;
        let assigned = unsafe { AssignProcessToJobObject(job.raw(), process) } != 0;
        let assign_error = (!assigned).then(io::Error::last_os_error);
        unsafe { CloseHandle(process) };
        if let Some(error) = assign_error {
            return Err(error);
        }
        resume_process(pid)?;
        Ok(job)
    }

    pub fn attach_running(pid: u32) -> io::Result<()> {
        let job = prepare_job()?;
        let process = open_for_job(pid)?;
        let assigned = unsafe { AssignProcessToJobObject(job.raw(), process) } != 0;
        unsafe { CloseHandle(process) };
        if !assigned {
            return Err(io::Error::last_os_error());
        }
        remember(pid, job);
        Ok(())
    }

    pub fn signal_tree(pid: u32) -> io::Result<()> {
        let guard = jobs().lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(job) = guard.get(&pid) {
            return terminate_job(job.raw());
        }
        drop(guard);
        terminate_pid(pid)
    }

    pub fn release_tree(pid: u32) {
        jobs()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&pid);
    }

    pub fn process_started_at(pid: u32) -> Option<SystemTime> {
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return None;
        }
        let _process = OwnedHandle(process);
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) } == 0
        {
            return None;
        }
        let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        // FILETIME 是从 1601-01-01 起的 100 纳秒。到 Unix 纪元是 11_644_473_600 秒。
        const EPOCH_DIFF: u64 = 11_644_473_600 * 10_000_000;
        let unix_100ns = ticks.checked_sub(EPOCH_DIFF)?;
        let secs = unix_100ns / 10_000_000;
        let nanos = (unix_100ns % 10_000_000) * 100;
        UNIX_EPOCH.checked_add(Duration::new(secs, nanos as u32))
    }

    fn open_for_job(pid: u32) -> io::Result<HANDLE> {
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(process)
        }
    }

    fn remember(pid: u32, job: OwnedHandle) {
        jobs()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(pid, job);
    }
}
