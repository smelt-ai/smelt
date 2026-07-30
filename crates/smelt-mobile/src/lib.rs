//! Smelt Mobile FFI Layer
//!
//! 通过 flutter_rust_bridge 暴露给 Flutter 的 API。
//! 核心职责：
//! - 连接管理（WebSocket + E2EE）
//! - ACP 协议处理
//! - 会话状态管理
//!
//! 与桌面端共享 `smelt-core` 的协议定义和解析逻辑。

mod frb_generated; /* AUTO INJECTED BY flutter_rust_bridge. */

pub mod api;
pub mod api_iroh;
pub mod crypto;
pub mod iroh_tunnel;
pub mod session;
pub mod transport;

pub use api::*;
