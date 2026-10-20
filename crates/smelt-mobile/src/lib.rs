//! Smelt 手机客户端内核，通过 flutter_rust_bridge 暴露给 Flutter。
//!
//! - [`api`]：FRB 唯一扫描的模块，只放 Dart 用的视图模型和入口。
//! - `fleet`：进程级单例「机队」：设备身份、登记的机器，每台一个节点同时连着。
//! - `connection`：一台机器的连接状态机，传输可注入，便于测试。
//! - `iroh_dialer`：真实传输，经 iroh `smelt/runtime/1` 连运行时，按所有机器的 relay 定形态。
//! - `store`：本机业务数据（SQLite，按版本迁移）：机器登记、消息草稿。
//! - `cache`：离线缓存（SQLite，坏了重建）。
//! - `catalog`：一台机器的事件订阅 → 会话摘要、attention、工作区、自动化、历史、新建删除。
//! - `conversation`：一台机器的 acp_watch 镜像 → 对话视图模型，分页、动作、图片。
//! - `terminal`：一台机器的终端 watch 字节流、输入、尺寸。
//! - `pass`：跨机器投影（指挥台的单据与灶台）。
// flutter_rust_bridge 的 `#[frb]` 属性宏内部使用 `frb_expand` cfg 展开,
// 对 Rust 新版属于「未知 cfg」,这里显式允许(宏自身维护,非手写代码)。
#![allow(unexpected_cfgs)]

pub mod api;
mod cache;
mod catalog;
mod connection;
mod conversation;
mod fleet;
mod frb_generated; /* AUTO INJECTED BY flutter_rust_bridge. */
mod iroh_dialer;
mod pass;
mod store;
mod terminal;
