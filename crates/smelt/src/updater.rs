//! GUI 对无 UI 更新事务核心的薄 re-export。
//!
//! 安装 helper 与桌面进程必须共享同一份状态机，禁止在 GUI crate 内复制事务逻辑。

pub use smelt_core::updater::*;
