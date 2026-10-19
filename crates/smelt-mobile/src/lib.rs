//! Smelt 手机客户端内核，通过 flutter_rust_bridge 暴露给 Flutter。
//!
//! - [`api`]：FRB 唯一扫描的模块，只放 Dart 用的视图模型和入口。
//! - `client`：进程级单例（设备身份、当前运行时的连接状态机）。
//! - `connection`：连接状态机本体，传输可注入，便于测试。
//! - `iroh_dialer`：真实传输，经 iroh `smelt/runtime/1` 连运行时。
// flutter_rust_bridge 的 `#[frb]` 属性宏内部使用 `frb_expand` cfg 展开,
// 对 Rust 新版属于「未知 cfg」,这里显式允许(宏自身维护,非手写代码)。
#![allow(unexpected_cfgs)]

pub mod api;
mod client;
mod connection;
mod frb_generated; /* AUTO INJECTED BY flutter_rust_bridge. */
mod iroh_dialer;
