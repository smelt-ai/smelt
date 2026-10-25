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
pub mod process_wait;
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
