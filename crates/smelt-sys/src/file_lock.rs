//! 跨进程的独占文件锁。锁随这个值的 drop 释放，进程退出时句柄关掉，锁也没了。
//!
//! Unix 用 `flock`。Windows 用 `LockFileEx`，锁住整个文件范围。两边都是建议锁里
//! 我们自己遵守的那一种：没拿到锁的人不会去改同一份状态。

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

pub struct ExclusiveFile {
    file: File,
}

impl Drop for ExclusiveFile {
    fn drop(&mut self) {
        // 句柄关掉时锁一起释放。读一次字段，锁文件在这个值活着的时候一直开着。
        let _ = &self.file;
    }
}

impl ExclusiveFile {
    /// 阻塞直到拿到独占锁。
    pub fn lock(path: &Path) -> io::Result<Self> {
        let file = open_lock_file(path)?;
        lock_file(&file, true)?;
        Ok(Self { file })
    }

    /// 拿不到就立刻返回 `Ok(None)`，不排队。
    pub fn try_lock(path: &Path) -> io::Result<Option<Self>> {
        let file = open_lock_file(path)?;
        match lock_file(&file, false) {
            Ok(()) => Ok(Some(Self { file })),
            Err(error) if is_would_block(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
}

fn is_would_block(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::EWOULDBLOCK) || error.raw_os_error() == Some(libc::EAGAIN)
    }
    #[cfg(windows)]
    {
        // ERROR_LOCK_VIOLATION
        error.raw_os_error() == Some(33)
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

#[cfg(unix)]
fn lock_file(file: &File, blocking: bool) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let mut flags = libc::LOCK_EX;
    if !blocking {
        flags |= libc::LOCK_NB;
    }
    if unsafe { libc::flock(file.as_raw_fd(), flags) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn lock_file(file: &File, blocking: bool) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let mut flags = LOCKFILE_EXCLUSIVE_LOCK;
    if !blocking {
        flags |= LOCKFILE_FAIL_IMMEDIATELY;
    }
    // 锁的起点在 OVERLAPPED 里。空指针会被直接解引用，进程以访问违规退出。
    // 文件不是重叠 IO 打开的，调用会阻塞到拿到锁；hEvent 保持空。
    let mut overlapped = OVERLAPPED::default();
    let ok = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            flags,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn lock_file(_file: &File, _blocking: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "这个平台没有文件锁",
    ))
}
