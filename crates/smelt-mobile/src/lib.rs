//! Smelt Mobile FFI Layer
//!
//! 通过 flutter_rust_bridge 暴露给 Flutter 的 API。
//! 核心职责：
//! - 通过 iroh 隧道连接桌面端网关
//! - 向 Flutter 暴露隧道生命周期和传输状态 API
// flutter_rust_bridge 的 `#[frb]` 属性宏内部使用 `frb_expand` cfg 展开,
// 对 Rust 新版属于「未知 cfg」,这里显式允许(宏自身维护,非手写代码)。
#![allow(unexpected_cfgs)]

mod frb_generated; /* AUTO INJECTED BY flutter_rust_bridge. */

pub mod api_iroh;
mod iroh_tunnel;
