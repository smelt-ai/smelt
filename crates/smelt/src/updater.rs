//! GUI 对无 UI 更新事务核心的薄 re-export。
//!
//! 安装 helper 与桌面进程必须共享同一份状态机，禁止在 GUI crate 内复制事务逻辑。

/// macOS 换 .app 包，Windows 拉起安装程序。只有这两个平台有桌面。
#[cfg(any(target_os = "macos", windows))]
pub use smelt_client::updater::*;
