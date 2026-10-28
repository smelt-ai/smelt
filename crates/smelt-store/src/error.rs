//! 持久化边界的错误类型。
//!
//! 库路径不再返回 `String`：调用方可以按种类分支，而不是 `contains("不存在")`。

use std::path::PathBuf;

/// `smelt-store` 的失败原因。
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("数据库不存在: {path}")]
    MissingDatabase { path: PathBuf },

    #[error("主库缺失但留下 WAL/SHM sidecar（{path}），拒绝建空库以免丢失未 checkpoint 的数据")]
    OrphanSidecars { path: PathBuf },

    #[error("数据库已存在，拒绝覆盖: {path}")]
    AlreadyExists { path: PathBuf },

    #[error("数据库路径不是普通文件: {path}")]
    NotRegularFile { path: PathBuf },

    #[error("打开 {path} 失败: {source}")]
    OpenFailed {
        path: PathBuf,
        #[source]
        source: OpenSource,
    },

    #[error("数据库连接锁已损坏: {path}")]
    ConnectionPoisoned { path: PathBuf },

    #[error("数据库 schema {observed} 新于当前支持的 {supported}，拒绝打开")]
    SchemaTooNew { observed: u32, supported: u32 },

    #[error("数据库 schema {version} 形状无法识别，拒绝打开以免清空已有数据")]
    SchemaUnrecognized { version: u32 },

    #[error("无法启用 WAL，SQLite 返回 journal_mode={mode}")]
    WalNotEnabled { mode: String },

    #[error("{label} 不能为空")]
    EmptyName { label: String },

    #[error("{label} 不能包含 NUL")]
    NameContainsNul { label: String },

    #[error("智能体定义的 id 不能在更新中修改: {from} -> {to}")]
    AgentDefinitionIdChanged { from: String, to: String },

    #[error("重复的自动化 id: {id}")]
    DuplicateAutomationId { id: String },

    #[error("重复的 Run id: {id}")]
    DuplicateRunId { id: String },

    #[error("delivery_attempts 不能为负")]
    NegativeDeliveryAttempts,

    #[error("KV scope {scope} 的根节点不是 object")]
    KvRootNotObject { scope: String },

    #[error("KV {path} 是二进制值，不能还原为结构化值")]
    KvBlobNotStructured { path: String },

    #[error("未知 KV value_type: {value_type}")]
    UnknownKvValueType { value_type: String },

    #[error("工作区第 {position} 段对话缺少 id")]
    ConversationMissingId { position: usize },

    #[error("工作区里有两段对话用了同一个 id：{id}")]
    DuplicateConversationId { id: String },

    #[error("对话 {id} 里既没有终端也不是 ACP 对话")]
    EmptyConversation { id: String },

    #[error("工作区里没有这段对话：{id}")]
    ConversationNotFound { id: String },

    #[error("ACP 对话 {id} 缺少会话 id")]
    AcpConversationMissingSid { id: String },

    #[error("会话 id 已被别的对话占用：{id}")]
    DuplicateSessionId { id: String },

    #[error("项目根目录不能为空")]
    EmptyProjectRoot,

    #[error("KV 数字 {path} 无效: {source}")]
    InvalidKvNumber {
        path: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("当前移动端构建未启用 SQLite store")]
    SqliteDisabledOnMobile,

    #[error(
        "数据库未初始化（空文件或空 schema），拒绝建空库。若这是主库丢失后的残留，请从备份恢复。"
    )]
    UninitializedDatabase,

    #[error("事务已中止")]
    TransactionAborted,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    // rusqlite::Error 本身就超过 Result 的体积阈值，留在枚举里会让每个
    // 返回 StoreError 的函数都触发 result_large_err。
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    #[error(transparent)]
    Sqlite(Box<rusqlite::Error>),
}

/// 打开数据库文件时的底层失败。移动端没有 rusqlite，用 io 兜住 Display。
#[derive(Debug, thiserror::Error)]
pub enum OpenSource {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    #[error(transparent)]
    Sqlite(Box<rusqlite::Error>),
}

impl StoreError {
    pub(crate) fn missing_database(path: impl Into<PathBuf>) -> Self {
        Self::MissingDatabase { path: path.into() }
    }

    pub(crate) fn orphan_sidecars(path: impl Into<PathBuf>) -> Self {
        Self::OrphanSidecars { path: path.into() }
    }

    pub(crate) fn empty_name(label: impl Into<String>) -> Self {
        Self::EmptyName {
            label: label.into(),
        }
    }

    pub(crate) fn name_contains_nul(label: impl Into<String>) -> Self {
        Self::NameContainsNul {
            label: label.into(),
        }
    }

    pub(crate) fn unrecognized_schema(version: u32) -> Self {
        Self::SchemaUnrecognized { version }
    }
}

#[cfg(not(any(target_os = "ios", target_os = "android")))]
impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(Box::new(error))
    }
}

#[cfg(not(any(target_os = "ios", target_os = "android")))]
impl From<rusqlite::Error> for OpenSource {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(Box::new(error))
    }
}

/// 上层仍返回 `String` 的过渡转换。sqlite_state 等迁完后删除。
impl From<StoreError> for String {
    fn from(error: StoreError) -> Self {
        error.to_string()
    }
}
