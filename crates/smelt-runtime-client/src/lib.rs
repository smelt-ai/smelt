//! smeltd 协议客户端，桌面和手机共用一份。
//!
//! - [`protocol`]：wire 契约（首帧 op 名、控制 RPC 信封、`system.describe`）。服务端
//!   也引用这里的定义，两边不会各写一份。
//! - [`connector`]：给定运行时，返回一条新连接。本机是 unix socket；远程走 iroh。
//! - [`client`]：在连接器之上的帧收发和带类型的控制调用。
//! - [`browser`]：常驻浏览器的画面流（看、接管）。
//!
//! 依赖约束：必须能编进 iOS / Android，不许引 GPUI、原生 SQLite 或 `smelt-core`。

pub mod browser;
pub mod client;
pub mod connector;
pub mod local;
pub mod protocol;

pub use client::{DEFAULT_REQUEST_TIMEOUT, RequestError, RuntimeClient};
pub use connector::{Connector, LocalConnector};
