//! 进程级文件描述符上限的防线。
//!
//! macOS 从 Finder / launchd 启动的图形进程常见软上限只有 256。Smelt 的一个
//! 终端会话会同时用到 Unix socket、PTY 和子进程管道；即使没有泄漏，正常多会话
//! 也可能过早耗尽这个默认额度。这里只提升当前进程的软上限，不修改系统硬上限，
//! 且失败时静默降级。

/// 尽可能把当前进程的 NOFILE 软上限提高到一个合理值。
///
/// 不触碰硬上限；受限沙箱或系统策略拒绝时保持原样，让调用方继续正常启动。
#[cfg(unix)]
pub fn raise_fd_limit() {
    unsafe {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return;
        }
        // 部分 macOS 环境把硬上限报成 RLIM_INFINITY，直接照报的数申请反而会被拒绝
        // （内核对 NOFILE 另有一个不通过 rlimit 暴露的绝对上限），封顶到一个够用的数。
        let target = if lim.rlim_max == libc::RLIM_INFINITY {
            65536
        } else {
            lim.rlim_max.min(65536)
        };
        if target > lim.rlim_cur {
            lim.rlim_cur = target;
            let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &lim);
        }
    }
}

#[cfg(not(unix))]
pub fn raise_fd_limit() {}
