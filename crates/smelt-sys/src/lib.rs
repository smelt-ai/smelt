//! 桌面客户端和运行时共用的底层工具。见 Cargo.toml 的说明。

pub mod app_log;
pub mod block_on;
pub mod daemon_image;
pub mod fd_limit;
#[cfg(unix)]
pub mod pi_image;
pub mod process_wait;
pub mod tty_color;

/// 这台机器的主机名；读不到就是 `None`。
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
