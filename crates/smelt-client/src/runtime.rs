//! 客户端挂着的运行时：它是谁，怎么连（设计文档 D6）。
//!
//! 有本机运行时的平台上，本机身份是 `Local`（设计文档 D3）。远程运行时按它永久不变的
//! `runtime_id` 认。会话、视图各自记着自己属于哪个运行时，调用和连接都发给它。

use smelt_runtime_client::RuntimeClient;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RuntimeKey {
    Local,
    Remote(String),
}

/// 这一版桌面能不能在本机跑 smeltd。第 1 步是 Unix；Windows 只连远程运行时。
pub fn local_runtime_supported() -> bool {
    // 手机端是 unix，本来就会走本机运行时。Windows 桌面从这一版起同样拉起本机 smeltd。
    cfg!(any(unix, windows))
}

#[derive(Clone)]
pub struct Runtime {
    key: RuntimeKey,
    client: RuntimeClient,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Runtime").field(&self.key).finish()
    }
}

impl Runtime {
    pub fn new(key: RuntimeKey, client: RuntimeClient) -> Self {
        Self { key, client }
    }

    /// 本机运行时。连本机 socket，不负责拉起守护。
    ///
    /// 第 1 步只有 Unix 有本机运行时。没有的平台返回 `None`，调用方不能造一个离线的
    /// 本机空间来顶上。
    pub fn local() -> Option<Self> {
        if !local_runtime_supported() {
            return None;
        }
        static LOCAL: std::sync::OnceLock<RuntimeClient> = std::sync::OnceLock::new();
        Some(Self {
            key: RuntimeKey::Local,
            client: LOCAL
                .get_or_init(smelt_runtime_client::local::local_runtime)
                .clone(),
        })
    }

    pub fn key(&self) -> &RuntimeKey {
        &self.key
    }

    pub fn is_local(&self) -> bool {
        self.key == RuntimeKey::Local
    }

    pub fn client(&self) -> &RuntimeClient {
        &self.client
    }
}
