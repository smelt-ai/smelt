//! 桌面挂着的运行时（设计文档 D6）。本机运行时总在；远程运行时来自客户端库里登记的
//! 连接。会话、视图各自记着自己属于哪个运行时，调用、连接都发给它。

use std::sync::OnceLock;

use smelt_runtime_client::RuntimeClient;

/// 本机运行时的客户端。连本机 socket；守护的拉起和升级另由 `terminal` 管。
pub(crate) fn local() -> RuntimeClient {
    static LOCAL: OnceLock<RuntimeClient> = OnceLock::new();
    LOCAL
        .get_or_init(smelt_runtime_client::local::local_runtime)
        .clone()
}
