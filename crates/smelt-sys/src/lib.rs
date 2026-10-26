//! 桌面客户端和运行时共用的底层工具。见 Cargo.toml 的说明。

pub mod app_log;
pub mod block_on;
pub mod daemon_image;
pub mod fd_limit;
pub mod file_lock;
#[cfg(windows)]
pub mod named_pipe;
pub mod pi_image;
pub mod process;
pub mod process_usage;
pub mod process_wait;
pub mod ready_pipe;
pub mod tty_color;

/// 这台机器的主机名；读不到就是 `None`。
#[cfg(unix)]
pub fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: buf 可写，长度如实传入；gethostname 最多写这么多字节。
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return None;
    }
    let end = buf.iter().position(|byte| *byte == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Windows 没有 `gethostname`。`ComputerNamePhysicalDnsHostname` 是这台机器自己的
/// 名字，不是群集名。
#[cfg(windows)]
pub fn hostname() -> Option<String> {
    use windows_sys::Win32::System::SystemInformation::{
        ComputerNamePhysicalDnsHostname, GetComputerNameExW,
    };

    let mut len = 0u32;
    // 第一次只问需要多大的缓冲（含结尾的 0）。
    unsafe {
        GetComputerNameExW(
            ComputerNamePhysicalDnsHostname,
            std::ptr::null_mut(),
            &mut len,
        );
    }
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    let ok = unsafe {
        GetComputerNameExW(
            ComputerNamePhysicalDnsHostname,
            buf.as_mut_ptr(),
            &mut len,
        )
    };
    if ok == 0 {
        return None;
    }
    // 成功时 `len` 不含结尾的 0。
    let name = String::from_utf16_lossy(&buf[..len as usize])
        .trim()
        .to_string();
    (!name.is_empty()).then_some(name)
}

#[cfg(not(any(unix, windows)))]
pub fn hostname() -> Option<String> {
    None
}

/// 把文件或目录的修改时间改成 `when`。
///
/// Windows 上用普通的 `File::open` 打不开目录（没有 `FILE_FLAG_BACKUP_SEMANTICS`
/// 时 `CreateFile` 返回拒绝访问），所以目录要单独开。
pub fn set_modified(path: &std::path::Path, when: std::time::SystemTime) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(path)?.set_modified(when)
    }
    #[cfg(windows)]
    {
        set_modified_windows(path, when)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, when);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "这个平台不能改修改时间",
        ))
    }
}

#[cfg(windows)]
fn set_modified_windows(path: &std::path::Path, when: std::time::SystemTime) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, SetFileTime, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, OPEN_EXISTING,
    };

    let ticks = when
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?
        .as_nanos()
        / 100
        // 1601-01-01 到 1970-01-01 是 11_644_473_600 秒。FILETIME 的一格是 100 纳秒。
        + 11_644_473_600 * 10_000_000;
    let stamp = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_WRITE_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let _guard = HandleGuard(handle);
    let ok = unsafe { SetFileTime(handle, std::ptr::null(), std::ptr::null(), &stamp) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
struct HandleGuard(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for HandleGuard {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod mtime_tests {
    use std::time::{Duration, SystemTime};

    #[test]
    fn set_modified_backdates_a_directory() {
        let dir = std::env::temp_dir().join(format!(
            "smelt-mtime-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let stale = SystemTime::now() - Duration::from_secs(60);
        super::set_modified(&dir, stale).unwrap();
        let modified = std::fs::metadata(&dir).unwrap().modified().unwrap();
        let skew = modified
            .duration_since(stale)
            .unwrap_or_else(|error| error.duration());
        assert!(
            skew < Duration::from_secs(2),
            "目录修改时间应被改到过期：{modified:?} vs {stale:?}"
        );
        let _ = std::fs::remove_dir(&dir);
    }
}
