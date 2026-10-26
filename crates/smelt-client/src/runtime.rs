//! 客户端挂着的运行时：它是谁，怎么连（设计文档 D6）。
//!
//! 本机运行时的身份是 `Local`（设计文档 D3）。远程运行时按它永久不变的
//! `runtime_id` 认。会话、视图各自记着自己属于哪个运行时，调用和连接都发给它。

use smelt_runtime_client::RuntimeClient;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RuntimeKey {
    Local,
    Remote(String),
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

    /// 本机运行时。连本机 socket，不负责拉起守护。桌面支持的平台上都有。
    pub fn local() -> Self {
        static LOCAL: std::sync::OnceLock<RuntimeClient> = std::sync::OnceLock::new();
        Self {
            key: RuntimeKey::Local,
            client: LOCAL
                .get_or_init(smelt_runtime_client::local::local_runtime)
                .clone(),
        }
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
