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

    #[error("legacy source 不能为空")]
    EmptyLegacySource,

    #[error("legacy source 不能包含 NUL")]
    LegacySourceContainsNul,

    #[error("未知 legacy import 状态: {status}")]
    UnknownLegacyStatus { status: String },

    #[error(
        "legacy source {legacy_source} 已映射到 {mapped_namespace}/{mapped_key}，拒绝{action}为 {namespace}/{key}"
    )]
    LegacyMappingMismatch {
        legacy_source: String,
        mapped_namespace: String,
        mapped_key: String,
        namespace: String,
        key: String,
        action: &'static str,
    },

    #[error("legacy source {legacy_source} 的 KV 元数据不完整")]
    IncompleteLegacyMetadata { legacy_source: String },

    #[error("重复的自动化 id: {id}")]
    DuplicateAutomationId { id: String },

    #[error("重复的 Run id: {id}")]
    DuplicateRunId { id: String },

    #[error("delivery_attempts 不能为负")]
    NegativeDeliveryAttempts,

    #[error("隔离目标不能与原 KV 相同")]
    QuarantineSameKey,

    #[error("KV scope {scope} 的根节点不是 object")]
    KvRootNotObject { scope: String },

    #[error("KV {path} 是二进制值，不能还原为结构化值")]
    KvBlobNotStructured { path: String },

    #[error("未知 KV value_type: {value_type}")]
    UnknownKvValueType { value_type: String },

    #[error("会话组 {group_id} 没有布局叶子")]
    SessionGroupMissingLeaf { group_id: String },

    #[error("布局叶子 {node_id} 缺少 session")]
    LayoutLeafMissingSession { node_id: String },

    #[error("KV 数字 {path} 无效: {source}")]
    InvalidKvNumber {
        path: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("duplicate event_id")]
    DuplicateEventId,

    #[error("event_id {event_id} already exists with a different envelope")]
    EventIdConflict { event_id: String },

    #[error("aggregate revision moved backwards: {kind}:{id}")]
    AggregateRevisionMovedBackwards { kind: String, id: String },

    #[error("cursor declaration must be bound before it is stored")]
    CursorUnboundOnStore,

    #[error("cursor declaration must be bound before dead-lettering")]
    CursorUnboundOnDeadLetter,

    #[error("failed to append outbox event")]
    OutboxAppendFailed,

    #[error("durable subscription declaration conflicts with its stored cursor")]
    CursorDeclarationConflict,

    #[error("当前移动端构建未启用 SQLite store")]
    SqliteDisabledOnMobile,

    #[error(
        "数据库未初始化（空文件或空 schema），拒绝建空库。若这是主库丢失后的残留，请从备份恢复。"
    )]
    UninitializedDatabase,

    #[error(transparent)]
    PluginApi(#[from] smelt_plugin_api::ValidationError),

    #[error("事务已中止")]
    TransactionAborted,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// 打开数据库文件时的底层失败。移动端没有 rusqlite，用 io 兜住 Display。
#[derive(Debug, thiserror::Error)]
pub enum OpenSource {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
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

/// 上层仍返回 `String` 的过渡转换。sqlite_state 等迁完后删除。
impl From<StoreError> for String {
    fn from(error: StoreError) -> Self {
        error.to_string()
    }
}
