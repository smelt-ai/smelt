//! 桌面客户端和运行时共用的底层工具。见 Cargo.toml 的说明。

pub mod agent_image;
pub mod app_log;
pub mod block_on;
pub mod daemon_image;
pub mod fd_limit;
pub mod file_lock;
#[cfg(windows)]
pub mod named_pipe;
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
    let ok =
        unsafe { GetComputerNameExW(ComputerNamePhysicalDnsHostname, buf.as_mut_ptr(), &mut len) };
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

/// 读取本机硬件序列号（macOS 读取 IOPlatformSerialNumber，Windows 读取 BIOS 序列号，Linux 读取 DMI/machine-id）。
#[cfg(target_os = "macos")]
pub fn hardware_serial() -> Option<String> {
    let output = std::process::Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if let Some(idx) = line.find("\"IOPlatformSerialNumber\"") {
            let rest = &line[idx + "\"IOPlatformSerialNumber\"".len()..];
            if let Some(eq_idx) = rest.find('=') {
                let val = rest[eq_idx + 1..].trim().trim_matches('"').trim();
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

#[cfg(windows)]
pub fn hardware_serial() -> Option<String> {
    use std::os::windows::process::CommandExt;

    fn is_valid_serial(s: &str) -> bool {
        let trimmed = s.trim();
        if trimmed.is_empty() || trimmed.len() < 3 {
            return false;
        }
        let lower = trimmed.to_ascii_lowercase();
        !matches!(
            lower.as_str(),
            "to be filled by o.e.m."
                | "default string"
                | "system serial number"
                | "none"
                | "0"
                | "chassis serial number"
        )
    }

    // Windows 11 24H2 已移除 wmic，直接通过 powershell 查询 Win32_BIOS；加 CREATE_NO_WINDOW 避免弹出控制台。
    let mut cmd = std::process::Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "(Get-CimInstance Win32_BIOS).SerialNumber",
    ]);
    cmd.creation_flags(crate::process::CREATE_NO_WINDOW);

    if let Ok(output) = cmd.output() {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let trimmed = text.trim();
            if is_valid_serial(trimmed) {
                return Some(trimmed.to_string());
            }
        }
    }

    None
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn hardware_serial() -> Option<String> {
    if let Ok(content) = std::fs::read_to_string("/sys/class/dmi/id/product_serial") {
        let trimmed = content.trim();
        if !trimmed.is_empty() && trimmed != "None" {
            return Some(trimmed.to_string());
        }
    }
    if let Ok(content) = std::fs::read_to_string("/etc/machine-id") {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

#[cfg(not(any(unix, windows)))]
pub fn hardware_serial() -> Option<String> {
    None
}

/// 桌面在跑的标记：Windows 上的命名互斥量 `SmeltDesktop`。安装程序
/// （`scripts/windows-installer.iss` 的 `AppMutex`）在安装和卸载开头查它，Smelt 还开着就
/// 提示先关掉。只要它存在，不做单实例：另一个实例已经建过也照样拿到句柄。句柄不关，
/// 进程退出时系统收回。
#[cfg(windows)]
pub fn mark_desktop_running() {
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = "SmeltDesktop".encode_utf16().chain(Some(0)).collect();
    // SAFETY: name 以 0 结尾，调用期间有效；默认安全属性，不要初始所有权。
    unsafe {
        CreateMutexW(std::ptr::null(), 0, name.as_ptr());
    }
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
fn set_modified_windows(
    path: &std::path::Path,
    when: std::time::SystemTime,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, OPEN_EXISTING, SetFileTime,
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
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
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
