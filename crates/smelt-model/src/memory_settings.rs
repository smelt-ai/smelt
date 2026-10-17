//! 记忆向量检索配置的键名和重排模型。存取、连接测试在运行时。

pub const EMBEDDING_BASE_URL: &str = "memory.embedding_base_url";

pub const EMBEDDING_MODEL: &str = "memory.embedding_model";

pub const EMBEDDING_API_KEY: &str = "memory.embedding_api_key";

/// 召回重排用的模型（Pi 的提供方 + 模型 id）。都空表示自动挑当前提供方里最便宜的。
pub const RERANK_MODEL_PROVIDER: &str = "memory.rerank_model_provider";

pub const RERANK_MODEL_ID: &str = "memory.rerank_model_id";

pub const KEYS: [&str; 5] = [
    EMBEDDING_BASE_URL,
    EMBEDDING_MODEL,
    EMBEDDING_API_KEY,
    RERANK_MODEL_PROVIDER,
    RERANK_MODEL_ID,
];

/// 用户指定的重排模型；两项都填才算指定。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RerankModel {
    pub provider: String,
    pub id: String,
}

pub fn is_memory_key(key: &str) -> bool {
    KEYS.contains(&key)
}
