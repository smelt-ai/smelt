//! 桌面客户端访问运行时的部分：状态订阅连接、ACP 会话客户端、会话和自动化操作。
//! 只走协议，不碰运行时的库。

pub mod acp_client;
pub mod daemon_state;
pub mod session_ops;
pub mod updater;
