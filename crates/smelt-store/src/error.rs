//! 持久化边界的错误类型。
//!
//! 库路径不再返回 `String`：调用方可以按种类分支，而不是比对报错原话。

use std::path::PathBuf;

/// `smelt-store` 的失败原因。
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("The database doesn't exist: {path}")]
    MissingDatabase { path: PathBuf },

    #[error(
        "The database is missing but its WAL/SHM sidecars remain ({path}); refusing to create an empty one so un-checkpointed data isn't lost"
    )]
    OrphanSidecars { path: PathBuf },

    #[error("The database already exists; refusing to overwrite it: {path}")]
    AlreadyExists { path: PathBuf },

    #[error("The database path isn't a regular file: {path}")]
    NotRegularFile { path: PathBuf },

    #[error("Couldn't open {path}: {source}")]
    OpenFailed {
        path: PathBuf,
        #[source]
        source: OpenSource,
    },

    #[error("The database connection lock is poisoned: {path}")]
    ConnectionPoisoned { path: PathBuf },

    #[error(
        "Database schema {observed} is newer than the supported {supported}; refusing to open it"
    )]
    SchemaTooNew { observed: u32, supported: u32 },

    #[error(
        "Database schema {version} has an unrecognized shape; refusing to open it so existing data isn't wiped"
    )]
    SchemaUnrecognized { version: u32 },

    #[error("Couldn't enable WAL; SQLite returned journal_mode={mode}")]
    WalNotEnabled { mode: String },

    #[error("{label} can't be empty")]
    EmptyName { label: String },

    #[error("{label} can't contain NUL")]
    NameContainsNul { label: String },

    #[error("{label} is repeated")]
    Duplicate { label: String },

    #[error("{label} isn't UTF-8")]
    NotUtf8 { label: String },

    #[error("An agent definition's id can't change in an update: {from} -> {to}")]
    AgentDefinitionIdChanged { from: String, to: String },

    #[error("Duplicate automation id: {id}")]
    DuplicateAutomationId { id: String },

    #[error("Duplicate run id: {id}")]
    DuplicateRunId { id: String },

    #[error("delivery_attempts can't be negative")]
    NegativeDeliveryAttempts,

    #[error("The root of KV scope {scope} isn't an object")]
    KvRootNotObject { scope: String },

    #[error("KV {path} is a binary value and can't be restored as a structured value")]
    KvBlobNotStructured { path: String },

    #[error("Unknown KV value_type: {value_type}")]
    UnknownKvValueType { value_type: String },

    #[error("The conversation has no id")]
    ConversationMissingId,

    #[error("Conversation {id} has no terminals and isn't an ACP conversation")]
    EmptyConversation { id: String },

    #[error("The workspace has no conversation {id}")]
    ConversationNotFound { id: String },

    #[error("The conversation is archived; restore it first: {id}")]
    ConversationArchived { id: String },

    #[error("Agent group not found: {id}")]
    AgentGroupNotFound { id: String },

    #[error("Agent group member not found: {id}")]
    AgentGroupMemberNotFound { id: String },

    #[error("ACP conversation {id} has no session id")]
    AcpConversationMissingSid { id: String },

    #[error("The session id is already used by another conversation: {id}")]
    DuplicateSessionId { id: String },

    #[error("The project root can't be empty")]
    EmptyProjectRoot,

    #[error("The worktree doesn't exist: {id}")]
    WorkspaceWorksetNotFound { id: String },

    #[error("The worktree still has sessions and can't be deleted: {id}")]
    WorkspaceWorksetInUse { id: String },

    #[error("The worktree id is already used for something else: {id}")]
    WorkspaceWorksetConflict { id: String },

    #[error("The original worktree was closed; pick or recreate a worktree, then restore: {id}")]
    ClosedWorksetRestoreRequired { id: String },

    #[error("Couldn't restore the archived session: {message}")]
    InvalidWorksetRestore { message: String },

    #[error("Session {id}'s working directory isn't inside its worktree")]
    WorksetCwdOutside { id: String },

    #[error("KV number {path} is invalid: {source}")]
    InvalidKvNumber {
        path: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("This mobile build doesn't include the SQLite store")]
    SqliteDisabledOnMobile,

    #[error(
        "The database isn't initialized (empty file or empty schema); refusing to create an empty one. If this is left over from a lost database, restore it from a backup."
    )]
    UninitializedDatabase,

    #[error("The transaction was aborted")]
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

    pub(crate) fn duplicate(label: impl Into<String>) -> Self {
        Self::Duplicate {
            label: label.into(),
        }
    }

    pub(crate) fn not_utf8(label: impl Into<String>) -> Self {
        Self::NotUtf8 {
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
