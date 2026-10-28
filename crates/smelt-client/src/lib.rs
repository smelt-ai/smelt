//! 桌面客户端访问运行时的部分：状态订阅连接、ACP 会话客户端、会话和自动化操作。
//! 只走协议，不碰运行时的库。

pub mod acp_client;
pub mod compat;
pub mod daemon_state;
pub mod daemon_stop;
pub mod runtime;
pub mod runtime_path;
pub mod session_kill;
pub mod session_ops;
// 桌面的在线更新：macOS 换 .app 包，Windows 拉起安装程序。
#[cfg(any(target_os = "macos", windows))]
pub mod updater;
// 放的位置决定能不能自更新：被系统转移、只读的要先移到「应用程序」。
#[cfg(target_os = "macos")]
pub mod app_location;
