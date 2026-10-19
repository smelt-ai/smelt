//! 守护状态的数据在 `smelt_model::daemon_state`，本机运行时的连接入口在
//! `smelt_runtime_client::local`；订阅连接本身在桌面客户端（smelt-client）。

pub use smelt_model::daemon_state::*;
pub use smelt_runtime_client::local::{local_runtime, smeltd_sock_path};
