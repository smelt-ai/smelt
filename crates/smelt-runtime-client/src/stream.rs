//! 到运行时的一条连接的流类型，各平台一种，协议代码不分叉。
//!
//! 本机连接和远程（iroh 把 QUIC 流接到一对套接字的一端）交出的都是这个类型，上层的
//! 阻塞读写、`try_clone`、读写超时、`shutdown` 对两种传输完全一样。
//!
//! - Unix：`std::os::unix::net::UnixStream` / `UnixListener`。
//! - Windows：`uds_windows` 的同名类型，即系统的 AF_UNIX（Windows 10 1803 起支持）。
//!   方法和 std 的一致，`pair()`、`bind`、`incoming` 也有。Tokio 在 Windows 上没有
//!   AF_UNIX，守护的接受循环是同步的，不需要把这条监听塞进 tokio。

#[cfg(unix)]
pub use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
pub use uds_windows::{UnixListener, UnixStream};

pub type RuntimeStream = UnixStream;
