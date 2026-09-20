//! Smelt 的统一本地持久化边界。
//!
//! 桌面端默认实现是 `~/.smelt/smelt.sqlite3` 关系库。已映射领域走类型化快照，
//! 不经 JSON 文档入口。上层不直接拼 SQL；测试可以给 [`Store`] 传独立路径。
//!
//! 开库默认不建库：[`Store::open`] 只打开已有文件。真正的空位才允许
//! [`Store::create`] / [`Store::open_or_create`]。主文件没了但 WAL/SHM 还在时拒绝建空库。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

mod error;
pub use error::{OpenSource, StoreError};

// 这两个模块只服务桌面的 SQLite 实现（cfg 与下面的 `imp` 保持一致）。移动端不编
// native SQLite，rusqlite/serde_json 在 iOS/Android 上根本没进依赖树，无条件声明
// 会让交叉编译直接挂在解析不到 crate 上。
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod catalog;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod config;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod documents;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod schema;
mod snapshots;

pub use snapshots::{
    AgentAcpConfigMemoryRecord, AgentAcpEnvRecord, AgentConversationCommandRecord,
    AgentDefinitionRecord, AgentProfileRecord, AgentUiSnapshot, AppearanceSnapshot,
    AutoModelSnapshot, HistoryTitleRecord, HistoryTitleSnapshot, LaunchEntryRecord, LaunchSnapshot,
    PublishedSessionRecord, PublishedSessionSnapshot, PublishedWorkspaceMenuSnapshot,
    QuotaCacheSnapshot, RemoteAcpSessionRecord, RemoteConfigSnapshot, RemoteSessionCatalogSnapshot,
    RemoteTerminalSessionRecord, TerminalThemeSnapshot, UpdateSettingsSnapshot,
    UpdateStateSnapshot, WorkspaceAcpRecord, WorkspaceLayoutNode, WorkspaceProjectRecord,
    WorkspaceSessionRecord, WorkspaceSnapshot, WorktreeInheritSnapshot,
};

pub const DATABASE_FILE_NAME: &str = "smelt.sqlite3";
pub const STORE_SCHEMA_VERSION: u32 = 12;

/// 主库文件与 WAL sidecar 的存在情况。
///
/// 建库的唯一合法前提是 [`DatabasePresence::Absent`]。主文件没了但 `-wal`/`-shm`
/// 还在，说明上次没干净关掉，里面可能有未 checkpoint 的数据——这时建空库等于丢数据。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatabasePresence {
    /// 主文件和 sidecar 都不在，才允许 `create` / `open_or_create` 建库。
    Absent,
    /// 主文件在，按已有库打开。
    Ready,
    /// 主文件不在，但留下了 `-wal` 或 `-shm`。
    OrphanSidecars,
}

/// 看主库路径上现在是已有库、空位，还是事故残留 sidecar。
pub fn database_presence(path: impl AsRef<Path>) -> DatabasePresence {
    inspect_database_presence(path.as_ref())
}

fn inspect_database_presence(path: &Path) -> DatabasePresence {
    let main_ready = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => true,
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    };
    let sidecar = database_sidecar_path(path, "-wal").exists()
        || database_sidecar_path(path, "-shm").exists();
    match (main_ready, sidecar) {
        (false, false) => DatabasePresence::Absent,
        (false, true) => DatabasePresence::OrphanSidecars,
        (true, _) => DatabasePresence::Ready,
    }
}

fn database_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", path.as_os_str().to_string_lossy()))
}

fn missing_database_error(path: &Path) -> StoreError {
    StoreError::missing_database(path)
}

fn orphan_sidecar_error(path: &Path) -> StoreError {
    StoreError::orphan_sidecars(path)
}

/// 自动化域的持久化快照。嵌套配置仍以 JSON 字节放在对应列里。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomationSnapshot {
    pub schema_version: u32,
    pub store_id: String,
    pub revision: u64,
    pub timezone_fingerprint: String,
    pub automations: Vec<AutomationRecord>,
    pub states: Vec<AutomationStateRecord>,
    pub runs: Vec<AutomationRunRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomationRecord {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub workspace_dir: Option<String>,
    pub trigger_json: Vec<u8>,
    pub action_json: Vec<u8>,
    pub sinks_json: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomationStateRecord {
    pub automation_id: String,
    pub next_run_at: Option<i64>,
    pub last_run_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomationRunRecord {
    pub id: String,
    pub automation_id: String,
    pub source: String,
    pub status: String,
    pub created_at: i64,
    pub scheduled_for: Option<i64>,
    pub started_at: Option<i64>,
    pub delivery_attempt_at: Option<i64>,
    pub delivery_attempts: i64,
    pub finished_at: Option<i64>,
    pub session_id: Option<String>,
    pub provider_session_id: Option<String>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub runtime_released_at: Option<i64>,
    pub context_json: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutboxEvent {
    pub outbox_id: u64,
    pub envelope: smelt_plugin_api::EventEnvelope<serde_json::Value>,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerMessageCommandResult {
    pub source_session_id: String,
    pub command_id: smelt_plugin_api::CommandId,
    pub request_fingerprint: String,
    pub completed_at_ms: u64,
    pub message_id: String,
    pub target_session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerMessageCommitOutcome {
    Inserted,
    Existing(PeerMessageCommandResult),
    Conflict(PeerMessageCommandResult),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorDeclaration {
    pub last_acked_sequence: u64,
    pub declaration_fingerprint: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegacyState {
    Pending,
    Imported,
    Deleted,
}

#[derive(Clone)]
pub struct Store {
    backend: imp::Backend,
}

#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub struct StoreTransaction<'a> {
    transaction: &'a rusqlite::Transaction<'a>,
}

#[cfg(any(target_os = "ios", target_os = "android"))]
pub struct StoreTransaction<'a> {
    marker: std::marker::PhantomData<&'a ()>,
}

impl StoreTransaction<'_> {
    pub fn put(&mut self, namespace: &str, key: &str, value: &[u8]) -> Result<(), StoreError> {
        validate_name("namespace", namespace)?;
        validate_name("key", key)?;
        imp::transaction_put(self, namespace, key, value)
    }

    pub fn delete(&mut self, namespace: &str, key: &str) -> Result<bool, StoreError> {
        validate_name("namespace", namespace)?;
        validate_name("key", key)?;
        imp::transaction_delete(self, namespace, key)
    }

    pub fn append_outbox_batch(
        &mut self,
        events: &[smelt_plugin_api::EventEnvelope<serde_json::Value>],
    ) -> Result<(), StoreError> {
        imp::transaction_append_outbox(self, events)
    }
}

impl std::fmt::Debug for Store {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    /// 打开已有库。文件不在、或只剩 WAL/SHM sidecar 时失败，绝不建空库。
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        let backend = imp::Backend::open(&path)?;
        Ok(Self { backend })
    }

    /// 在空位上建新库。主文件或 sidecar 已在则失败。
    pub fn create(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        let backend = imp::Backend::create(&path)?;
        Ok(Self { backend })
    }

    /// 已有则打开，主文件与 sidecar 都不在才建库。orphan sidecar 拒绝建空库。
    pub fn open_or_create(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        let backend = imp::Backend::open_or_create(&path)?;
        Ok(Self { backend })
    }

    /// 只读库的 `user_version`，不建库、不迁移。交接 tripwire 用：
    /// 文件不在/损坏一律 None（无信息≠已迁移，按可回滚处理）。
    pub fn schema_version(path: &std::path::Path) -> Option<u32> {
        imp::read_user_version(path)
    }

    pub fn get(&self, namespace: &str, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        validate_name("namespace", namespace)?;
        validate_name("key", key)?;
        imp::get(&self.backend, namespace, key)
    }

    pub fn put(&self, namespace: &str, key: &str, value: &[u8]) -> Result<(), StoreError> {
        validate_name("namespace", namespace)?;
        validate_name("key", key)?;
        imp::put(&self.backend, namespace, key, value)
    }

    /// 读取一个明确作用域的二进制状态值，不经过 JSON 文档映射或 legacy 路由。
    pub fn get_blob(&self, namespace: &str, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        validate_name("namespace", namespace)?;
        validate_name("key", key)?;
        imp::get_blob(&self.backend, namespace, key)
    }

    /// 写入一个明确作用域的二进制状态值，不生成或读取任何 JSON 文件。
    pub fn put_blob(&self, namespace: &str, key: &str, value: &[u8]) -> Result<(), StoreError> {
        validate_name("namespace", namespace)?;
        validate_name("key", key)?;
        imp::put_blob(&self.backend, namespace, key, value)
    }

    /// 列出一个 blob 作用域下的全部 key。与 [`Self::keys`] 不同：后者走 JSON 文档目录。
    pub fn blob_keys(&self, namespace: &str) -> Result<Vec<String>, StoreError> {
        validate_name("namespace", namespace)?;
        imp::blob_keys(&self.backend, namespace)
    }

    /// 删除一个 blob。与 [`Self::delete`] 不同：后者会走 JSON 文档删除语义。
    pub fn delete_blob(&self, namespace: &str, key: &str) -> Result<bool, StoreError> {
        validate_name("namespace", namespace)?;
        validate_name("key", key)?;
        imp::delete_blob(&self.backend, namespace, key)
    }

    pub fn put_automation_run_transcript(
        &self,
        run_id: &str,
        entries_json: &[u8],
    ) -> Result<(), StoreError> {
        validate_name("run_id", run_id)?;
        imp::put_automation_run_transcript(&self.backend, run_id, entries_json)
    }

    pub fn get_automation_run_transcript(
        &self,
        run_id: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        validate_name("run_id", run_id)?;
        imp::get_automation_run_transcript(&self.backend, run_id)
    }

    pub fn retain_automation_run_transcripts(
        &self,
        keep: &HashSet<String>,
    ) -> Result<(), StoreError> {
        for run_id in keep {
            validate_name("run_id", run_id)?;
        }
        imp::retain_automation_run_transcripts(&self.backend, keep)
    }

    pub fn get_automation_snapshot(&self) -> Result<Option<AutomationSnapshot>, StoreError> {
        imp::get_automation_snapshot(&self.backend)
    }

    pub fn put_automation_snapshot(&self, snapshot: &AutomationSnapshot) -> Result<(), StoreError> {
        validate_automation_snapshot(snapshot)?;
        imp::put_automation_snapshot(&self.backend, snapshot)
    }

    pub fn get_launch_snapshot(&self) -> Result<Option<LaunchSnapshot>, StoreError> {
        imp::get_launch_snapshot(&self.backend)
    }

    pub fn put_launch_snapshot(&self, snapshot: &LaunchSnapshot) -> Result<(), StoreError> {
        imp::put_launch_snapshot(&self.backend, snapshot)
    }

    pub fn get_history_title_snapshot(&self) -> Result<Option<HistoryTitleSnapshot>, StoreError> {
        imp::get_history_title_snapshot(&self.backend)
    }

    pub fn put_history_title_snapshot(
        &self,
        snapshot: &HistoryTitleSnapshot,
    ) -> Result<(), StoreError> {
        imp::put_history_title_snapshot(&self.backend, snapshot)
    }

    pub fn get_appearance_snapshot(&self) -> Result<Option<AppearanceSnapshot>, StoreError> {
        imp::get_appearance_snapshot(&self.backend)
    }

    pub fn put_appearance_snapshot(&self, snapshot: &AppearanceSnapshot) -> Result<(), StoreError> {
        imp::put_appearance_snapshot(&self.backend, snapshot)
    }

    pub fn get_workspace_snapshot(&self) -> Result<Option<WorkspaceSnapshot>, StoreError> {
        imp::get_workspace_snapshot(&self.backend)
    }

    pub fn put_workspace_snapshot(&self, snapshot: &WorkspaceSnapshot) -> Result<(), StoreError> {
        imp::put_workspace_snapshot(&self.backend, snapshot)
    }

    /// 残留 workspace JSON 文档导入成类型化快照。活路径不要再走 JSON 文档入口。
    pub fn workspace_snapshot_from_json(raw: &[u8]) -> Result<WorkspaceSnapshot, StoreError> {
        imp::workspace_snapshot_from_json(raw)
    }

    pub fn workspace_snapshot_to_json(snapshot: &WorkspaceSnapshot) -> Result<Vec<u8>, StoreError> {
        imp::workspace_snapshot_to_json(snapshot)
    }

    pub fn get_agent_ui_snapshot(&self) -> Result<Option<AgentUiSnapshot>, StoreError> {
        imp::get_agent_ui_snapshot(&self.backend)
    }

    pub fn put_agent_ui_snapshot(&self, snapshot: &AgentUiSnapshot) -> Result<(), StoreError> {
        imp::put_agent_ui_snapshot(&self.backend, snapshot)
    }

    /// 写界面偏好和启动命令，不碰 `agent_definition`。
    pub fn put_agent_ui_prefs(&self, snapshot: &AgentUiSnapshot) -> Result<(), StoreError> {
        imp::put_agent_ui_prefs(&self.backend, snapshot)
    }

    pub fn insert_agent_definition(
        &self,
        record: &AgentDefinitionRecord,
    ) -> Result<(), StoreError> {
        imp::insert_agent_definition(&self.backend, record)
    }

    pub fn update_agent_definition(
        &self,
        record: &AgentDefinitionRecord,
    ) -> Result<bool, StoreError> {
        imp::update_agent_definition(&self.backend, record)
    }

    pub fn delete_agent_definition(&self, id: &str) -> Result<bool, StoreError> {
        imp::delete_agent_definition(&self.backend, id)
    }

    pub fn get_remote_session_snapshot(
        &self,
    ) -> Result<Option<RemoteSessionCatalogSnapshot>, StoreError> {
        imp::get_remote_session_snapshot(&self.backend)
    }

    pub fn put_remote_session_snapshot(
        &self,
        snapshot: &RemoteSessionCatalogSnapshot,
    ) -> Result<(), StoreError> {
        imp::put_remote_session_snapshot(&self.backend, snapshot)
    }

    pub fn get_published_session_snapshot(
        &self,
    ) -> Result<Option<PublishedSessionSnapshot>, StoreError> {
        imp::get_published_session_snapshot(&self.backend)
    }

    pub fn put_published_session_snapshot(
        &self,
        snapshot: &PublishedSessionSnapshot,
    ) -> Result<(), StoreError> {
        imp::put_published_session_snapshot(&self.backend, snapshot)
    }

    pub fn agent_ui_snapshot_from_json(
        value: &serde_json::Value,
    ) -> Result<AgentUiSnapshot, StoreError> {
        imp::agent_ui_snapshot_from_json(value)
    }

    pub fn get_remote_config_snapshot(&self) -> Result<Option<RemoteConfigSnapshot>, StoreError> {
        imp::get_remote_config_snapshot(&self.backend)
    }

    pub fn put_remote_config_snapshot(
        &self,
        snapshot: &RemoteConfigSnapshot,
    ) -> Result<(), StoreError> {
        imp::put_remote_config_snapshot(&self.backend, snapshot)
    }

    pub fn get_update_settings_snapshot(
        &self,
    ) -> Result<Option<UpdateSettingsSnapshot>, StoreError> {
        imp::get_update_settings_snapshot(&self.backend)
    }

    pub fn put_update_settings_snapshot(
        &self,
        snapshot: &UpdateSettingsSnapshot,
    ) -> Result<(), StoreError> {
        imp::put_update_settings_snapshot(&self.backend, snapshot)
    }

    pub fn get_update_state_snapshot(&self) -> Result<Option<UpdateStateSnapshot>, StoreError> {
        imp::get_update_state_snapshot(&self.backend)
    }

    pub fn put_update_state_snapshot(
        &self,
        snapshot: &UpdateStateSnapshot,
    ) -> Result<(), StoreError> {
        imp::put_update_state_snapshot(&self.backend, snapshot)
    }

    pub fn get_worktree_inherit_snapshot(
        &self,
    ) -> Result<Option<WorktreeInheritSnapshot>, StoreError> {
        imp::get_worktree_inherit_snapshot(&self.backend)
    }

    pub fn put_worktree_inherit_snapshot(
        &self,
        snapshot: &WorktreeInheritSnapshot,
    ) -> Result<(), StoreError> {
        imp::put_worktree_inherit_snapshot(&self.backend, snapshot)
    }

    pub fn get_terminal_theme_snapshot(&self) -> Result<Option<TerminalThemeSnapshot>, StoreError> {
        imp::get_terminal_theme_snapshot(&self.backend)
    }

    pub fn put_terminal_theme_snapshot(
        &self,
        snapshot: &TerminalThemeSnapshot,
    ) -> Result<(), StoreError> {
        imp::put_terminal_theme_snapshot(&self.backend, snapshot)
    }

    pub fn get_quota_cache_snapshot(&self) -> Result<Option<QuotaCacheSnapshot>, StoreError> {
        imp::get_quota_cache_snapshot(&self.backend)
    }

    pub fn put_quota_cache_snapshot(
        &self,
        snapshot: &QuotaCacheSnapshot,
    ) -> Result<(), StoreError> {
        imp::put_quota_cache_snapshot(&self.backend, snapshot)
    }

    pub fn get_dsh_auto_model_snapshot(&self) -> Result<Option<AutoModelSnapshot>, StoreError> {
        imp::get_dsh_auto_model_snapshot(&self.backend)
    }

    pub fn put_dsh_auto_model_snapshot(
        &self,
        snapshot: &AutoModelSnapshot,
    ) -> Result<(), StoreError> {
        imp::put_dsh_auto_model_snapshot(&self.backend, snapshot)
    }

    pub fn get_pi_auto_model_snapshot(&self) -> Result<Option<AutoModelSnapshot>, StoreError> {
        imp::get_pi_auto_model_snapshot(&self.backend)
    }

    pub fn put_pi_auto_model_snapshot(
        &self,
        snapshot: &AutoModelSnapshot,
    ) -> Result<(), StoreError> {
        imp::put_pi_auto_model_snapshot(&self.backend, snapshot)
    }

    pub fn get_workspace_menu_snapshot(
        &self,
    ) -> Result<Option<PublishedWorkspaceMenuSnapshot>, StoreError> {
        imp::get_workspace_menu_snapshot(&self.backend)
    }

    pub fn put_workspace_menu_snapshot(
        &self,
        snapshot: &PublishedWorkspaceMenuSnapshot,
    ) -> Result<(), StoreError> {
        imp::put_workspace_menu_snapshot(&self.backend, snapshot)
    }

    pub fn delete(&self, namespace: &str, key: &str) -> Result<bool, StoreError> {
        validate_name("namespace", namespace)?;
        validate_name("key", key)?;
        imp::delete(&self.backend, namespace, key)
    }

    pub fn keys(&self, namespace: &str) -> Result<Vec<String>, StoreError> {
        validate_name("namespace", namespace)?;
        imp::keys(&self.backend, namespace)
    }

    /// Runs document mutations and outbox insertion in one database transaction.
    ///
    /// The transaction exposes only typed store operations, never arbitrary SQL.
    pub fn transaction<R>(
        &self,
        operation: impl FnOnce(&mut StoreTransaction<'_>) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        imp::store_transaction(&self.backend, operation)
    }

    pub fn legacy_state(&self, source: &str) -> Result<LegacyState, StoreError> {
        validate_legacy_source(source)?;
        imp::legacy_state(&self.backend, source)
    }

    /// 在同一个 SQLite 快照中读取 KV 和对应 legacy source 的处理状态。
    pub fn get_with_legacy_state(
        &self,
        namespace: &str,
        key: &str,
        source: &str,
    ) -> Result<(Option<Vec<u8>>, LegacyState), StoreError> {
        validate_name("namespace", namespace)?;
        validate_name("key", key)?;
        validate_legacy_source(source)?;
        imp::get_with_legacy_state(&self.backend, namespace, key, source)
    }

    /// 删除 KV，并在同一事务中记录该 legacy source 已被明确删除。
    pub fn delete_with_legacy_tombstone(
        &self,
        source: &str,
        namespace: &str,
        key: &str,
    ) -> Result<bool, StoreError> {
        validate_legacy_source(source)?;
        validate_name("namespace", namespace)?;
        validate_name("key", key)?;
        imp::delete_with_legacy_tombstone(&self.backend, source, namespace, key)
    }

    /// 只记录 legacy source 已被明确处理，不删除当前 KV。
    ///
    /// 用于文件系统 payload 已成功移入隔离区后的第二阶段提交；期间并发写入的新值
    /// 必须保留。
    pub fn mark_legacy_deleted(
        &self,
        source: &str,
        namespace: &str,
        key: &str,
    ) -> Result<(), StoreError> {
        validate_legacy_source(source)?;
        validate_name("namespace", namespace)?;
        validate_name("key", key)?;
        imp::mark_legacy_deleted(&self.backend, source, namespace, key)
    }

    /// 把 KV 原样移入隔离 namespace，并在同一事务中写入 legacy 删除墓碑。
    pub fn quarantine_with_legacy_tombstone(
        &self,
        source: &str,
        namespace: &str,
        key: &str,
        quarantine_namespace: &str,
        quarantine_key: &str,
    ) -> Result<bool, StoreError> {
        validate_legacy_source(source)?;
        validate_name("namespace", namespace)?;
        validate_name("key", key)?;
        validate_name("quarantine namespace", quarantine_namespace)?;
        validate_name("quarantine key", quarantine_key)?;
        if namespace == quarantine_namespace && key == quarantine_key {
            return Err(StoreError::QuarantineSameKey);
        }
        imp::quarantine_with_legacy_tombstone(
            &self.backend,
            source,
            namespace,
            key,
            quarantine_namespace,
            quarantine_key,
        )
    }

    /// Adds durable events to the domain transaction's outbox in one commit.
    pub fn append_outbox_batch(
        &self,
        events: &[smelt_plugin_api::EventEnvelope<serde_json::Value>],
    ) -> Result<(), StoreError> {
        imp::append_outbox_batch(&self.backend, events)
    }

    pub fn load_pending_outbox(&self, limit: usize) -> Result<Vec<OutboxEvent>, StoreError> {
        imp::load_pending_outbox(&self.backend, limit)
    }

    pub fn mark_outbox_dispatched(&self, dispatched: &[(u64, u64)]) -> Result<(), StoreError> {
        imp::mark_outbox_dispatched(&self.backend, dispatched)
    }

    /// Atomically inserts pending outbox events into the event log (or reuses an existing
    /// event_id) and marks the outbox rows dispatched.
    pub fn dispatch_pending_outbox(
        &self,
        limit: usize,
    ) -> Result<Vec<smelt_event_bus::StoredEvent>, StoreError> {
        imp::dispatch_pending_outbox(&self.backend, limit)
    }

    pub fn bind_cursor_declaration(
        &self,
        plugin_id: &smelt_plugin_api::PluginId,
        subscription_id: &smelt_plugin_api::SubscriptionId,
        declaration_fingerprint: &str,
    ) -> Result<CursorDeclaration, StoreError> {
        validate_name("declaration fingerprint", declaration_fingerprint)?;
        imp::bind_cursor_declaration(
            &self.backend,
            plugin_id,
            subscription_id,
            declaration_fingerprint,
        )
    }

    pub fn load_cursor_declaration(
        &self,
        plugin_id: &smelt_plugin_api::PluginId,
        subscription_id: &smelt_plugin_api::SubscriptionId,
    ) -> Result<Option<CursorDeclaration>, StoreError> {
        imp::load_cursor_declaration(&self.backend, plugin_id, subscription_id)
    }

    pub fn get_peer_message_command_result(
        &self,
        source_session_id: &str,
        command_id: &smelt_plugin_api::CommandId,
    ) -> Result<Option<PeerMessageCommandResult>, StoreError> {
        validate_name("source session id", source_session_id)?;
        imp::get_peer_message_command_result(&self.backend, source_session_id, command_id)
    }

    /// Commits the idempotent peer-message result and its durable delivery fact atomically.
    pub fn commit_peer_message_delivery(
        &self,
        result: &PeerMessageCommandResult,
        event: &smelt_plugin_api::EventEnvelope<serde_json::Value>,
    ) -> Result<PeerMessageCommitOutcome, StoreError> {
        validate_name("source session id", &result.source_session_id)?;
        validate_name("request fingerprint", &result.request_fingerprint)?;
        validate_name("message id", &result.message_id)?;
        validate_name("target session id", &result.target_session_id)?;
        imp::commit_peer_message_delivery(&self.backend, result, event)
    }
}

impl smelt_event_bus::EventStore for Store {
    fn append_batch(
        &self,
        events: &[smelt_event_bus::StoredEvent],
    ) -> Result<Vec<smelt_event_bus::StoredEvent>, smelt_event_bus::EventBusError> {
        imp::append_event_batch(&self.backend, events).map_err(smelt_event_bus::EventBusError::new)
    }

    fn append_batch_idempotent(
        &self,
        events: &[smelt_event_bus::StoredEvent],
    ) -> Result<Vec<smelt_event_bus::StoredEvent>, smelt_event_bus::EventBusError> {
        imp::append_event_batch_idempotent(&self.backend, events)
            .map_err(smelt_event_bus::EventBusError::new)
    }

    fn load_after_topics(
        &self,
        sequence: u64,
        topics: &std::collections::BTreeSet<smelt_plugin_api::Topic>,
        limit: usize,
    ) -> Result<Vec<smelt_event_bus::StoredEvent>, smelt_event_bus::EventBusError> {
        imp::load_events_after_topics(&self.backend, sequence, topics, limit)
            .map_err(smelt_event_bus::EventBusError::new)
    }

    fn high_watermark(&self) -> Result<u64, smelt_event_bus::EventBusError> {
        imp::event_high_watermark(&self.backend).map_err(smelt_event_bus::EventBusError::new)
    }

    fn bind_cursor_declaration(
        &self,
        plugin_id: &smelt_plugin_api::PluginId,
        subscription_id: &smelt_plugin_api::SubscriptionId,
        declaration_fingerprint: &str,
    ) -> Result<smelt_event_bus::CursorDeclaration, smelt_event_bus::EventBusError> {
        self.bind_cursor_declaration(plugin_id, subscription_id, declaration_fingerprint)
            .map(|declaration| smelt_event_bus::CursorDeclaration {
                last_acked_sequence: declaration.last_acked_sequence,
                declaration_fingerprint: declaration.declaration_fingerprint,
            })
            .map_err(smelt_event_bus::EventBusError::new)
    }

    fn load_cursor_declaration(
        &self,
        plugin_id: &smelt_plugin_api::PluginId,
        subscription_id: &smelt_plugin_api::SubscriptionId,
    ) -> Result<Option<smelt_event_bus::CursorDeclaration>, smelt_event_bus::EventBusError> {
        self.load_cursor_declaration(plugin_id, subscription_id)
            .map(|declaration| {
                declaration.map(|declaration| smelt_event_bus::CursorDeclaration {
                    last_acked_sequence: declaration.last_acked_sequence,
                    declaration_fingerprint: declaration.declaration_fingerprint,
                })
            })
            .map_err(smelt_event_bus::EventBusError::new)
    }

    fn store_cursors(
        &self,
        cursors: &[(
            smelt_plugin_api::PluginId,
            smelt_plugin_api::SubscriptionId,
            u64,
        )],
    ) -> Result<(), smelt_event_bus::EventBusError> {
        imp::store_cursors(&self.backend, cursors).map_err(smelt_event_bus::EventBusError::new)
    }

    fn put_dead_letter(
        &self,
        dead_letter: &smelt_event_bus::DeadLetter,
    ) -> Result<(), smelt_event_bus::EventBusError> {
        imp::put_dead_letter(&self.backend, dead_letter)
            .map_err(smelt_event_bus::EventBusError::new)
    }

    fn load_dead_letters(
        &self,
        plugin_id: &smelt_plugin_api::PluginId,
        subscription_id: &smelt_plugin_api::SubscriptionId,
        limit: usize,
    ) -> Result<Vec<smelt_event_bus::DeadLetter>, smelt_event_bus::EventBusError> {
        imp::load_dead_letters(&self.backend, plugin_id, subscription_id, limit)
            .map_err(smelt_event_bus::EventBusError::new)
    }

    fn load_retry(
        &self,
        plugin_id: &smelt_plugin_api::PluginId,
        subscription_id: &smelt_plugin_api::SubscriptionId,
    ) -> Result<Option<smelt_event_bus::DeliveryRetry>, smelt_event_bus::EventBusError> {
        imp::load_retry(&self.backend, plugin_id, subscription_id)
            .map_err(smelt_event_bus::EventBusError::new)
    }

    fn put_retry(
        &self,
        retry: &smelt_event_bus::DeliveryRetry,
    ) -> Result<(), smelt_event_bus::EventBusError> {
        imp::put_retry(&self.backend, retry).map_err(smelt_event_bus::EventBusError::new)
    }

    fn delete_retry(
        &self,
        plugin_id: &smelt_plugin_api::PluginId,
        subscription_id: &smelt_plugin_api::SubscriptionId,
    ) -> Result<(), smelt_event_bus::EventBusError> {
        imp::delete_retry(&self.backend, plugin_id, subscription_id)
            .map_err(smelt_event_bus::EventBusError::new)
    }

    fn get_command_result(
        &self,
        plugin_id: &smelt_plugin_api::PluginId,
        command_id: &smelt_plugin_api::CommandId,
    ) -> Result<Option<smelt_event_bus::CommandResult>, smelt_event_bus::EventBusError> {
        imp::get_command_result(&self.backend, plugin_id, command_id)
            .map_err(smelt_event_bus::EventBusError::new)
    }

    fn put_command_result(
        &self,
        result: &smelt_event_bus::CommandResult,
    ) -> Result<bool, smelt_event_bus::EventBusError> {
        imp::put_command_result(&self.backend, result).map_err(smelt_event_bus::EventBusError::new)
    }
}

fn validate_legacy_source(source: &str) -> Result<(), StoreError> {
    if source.trim().is_empty() {
        Err(StoreError::EmptyLegacySource)
    } else if source.as_bytes().contains(&0) {
        Err(StoreError::LegacySourceContainsNul)
    } else {
        Ok(())
    }
}

fn validate_name(label: &str, value: &str) -> Result<(), StoreError> {
    if value.trim().is_empty() {
        Err(StoreError::empty_name(label))
    } else if value.as_bytes().contains(&0) {
        Err(StoreError::name_contains_nul(label))
    } else {
        Ok(())
    }
}

fn validate_automation_snapshot(snapshot: &AutomationSnapshot) -> Result<(), StoreError> {
    let mut automation_ids = HashSet::new();
    for automation in &snapshot.automations {
        validate_name("automation.id", &automation.id)?;
        if !automation_ids.insert(automation.id.as_str()) {
            return Err(StoreError::DuplicateAutomationId {
                id: automation.id.clone(),
            });
        }
    }
    let mut run_ids = HashSet::new();
    for run in &snapshot.runs {
        validate_name("automation_run.id", &run.id)?;
        validate_name("automation_run.automation_id", &run.automation_id)?;
        if !run_ids.insert(run.id.as_str()) {
            return Err(StoreError::DuplicateRunId { id: run.id.clone() });
        }
        if run.delivery_attempts < 0 {
            return Err(StoreError::NegativeDeliveryAttempts);
        }
    }
    for state in &snapshot.states {
        validate_name("automation_state.automation_id", &state.automation_id)?;
    }
    Ok(())
}

#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod imp {
    use super::{HashSet, LegacyState, OpenSource, STORE_SCHEMA_VERSION, StoreError};
    use rusqlite::{
        Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior, params,
    };
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
    use std::time::{Duration, Instant};

    const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
    const BUSY_RETRY_DELAY: Duration = Duration::from_millis(10);
    const QUARANTINE_SCOPE: &str = "quarantine";
    const LEGACY_SCOPE_PREFIX: &str = "legacy:";
    const LEGACY_V3_FINGERPRINT: &str = "legacy-v3-unbound";

    const OPEN_EXISTING_FLAGS: OpenFlags =
        OpenFlags::SQLITE_OPEN_READ_WRITE.union(OpenFlags::SQLITE_OPEN_NO_MUTEX);
    const OPEN_CREATE_FLAGS: OpenFlags = OpenFlags::SQLITE_OPEN_READ_WRITE
        .union(OpenFlags::SQLITE_OPEN_CREATE)
        .union(OpenFlags::SQLITE_OPEN_NO_MUTEX);

    #[derive(Clone)]
    pub(super) struct Backend {
        path: std::path::PathBuf,
        connection: Arc<Mutex<Connection>>,
    }

    impl Backend {
        pub(super) fn open(path: &Path) -> Result<Self, StoreError> {
            open_backend(path, OpenMode::Existing)
        }

        pub(super) fn create(path: &Path) -> Result<Self, StoreError> {
            open_backend(path, OpenMode::CreateNew)
        }

        pub(super) fn open_or_create(path: &Path) -> Result<Self, StoreError> {
            open_backend(path, OpenMode::OpenOrCreate)
        }
    }

    /// 只读 user_version，不建库、不迁移、不改任何字节。
    /// 交接 tripwire 用：任何失败都给 None（=无信息，按可回滚处理）。
    pub(super) fn read_user_version(path: &Path) -> Option<u32> {
        let connection =
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
            .ok()
    }

    #[derive(Clone, Copy)]
    enum OpenMode {
        Existing,
        CreateNew,
        OpenOrCreate,
    }

    fn open_backend(path: &Path, mode: OpenMode) -> Result<Backend, StoreError> {
        let presence = super::inspect_database_presence(path);
        match (mode, presence) {
            (OpenMode::Existing, super::DatabasePresence::Ready)
            | (OpenMode::OpenOrCreate, super::DatabasePresence::Ready) => {
                open_existing_backend(path)
            }
            (OpenMode::Existing, super::DatabasePresence::Absent) => {
                Err(super::missing_database_error(path))
            }
            (OpenMode::CreateNew, super::DatabasePresence::Absent)
            | (OpenMode::OpenOrCreate, super::DatabasePresence::Absent) => create_new_backend(path),
            (OpenMode::CreateNew, super::DatabasePresence::Ready) => {
                Err(StoreError::AlreadyExists {
                    path: path.to_path_buf(),
                })
            }
            (_, super::DatabasePresence::OrphanSidecars) => Err(super::orphan_sidecar_error(path)),
        }
    }

    fn open_existing_backend(path: &Path) -> Result<Backend, StoreError> {
        let mut connection = open_connection(path, OPEN_EXISTING_FLAGS, false)?;
        initialize_schema(&mut connection, false, path)?;
        secure_sidecar_permissions(path);
        Ok(Backend {
            path: path.to_path_buf(),
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    fn create_new_backend(path: &Path) -> Result<Backend, StoreError> {
        let mut connection = open_connection(path, OPEN_CREATE_FLAGS, true)?;
        initialize_schema(&mut connection, true, path)?;
        secure_sidecar_permissions(path);
        Ok(Backend {
            path: path.to_path_buf(),
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    fn lock_connection(backend: &Backend) -> Result<MutexGuard<'_, Connection>, StoreError> {
        backend
            .connection
            .lock()
            .map_err(|_| StoreError::ConnectionPoisoned {
                path: backend.path.clone(),
            })
    }

    pub(super) fn store_transaction<R>(
        backend: &Backend,
        operation: impl FnOnce(&mut super::StoreTransaction<'_>) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = operation(&mut super::StoreTransaction {
            transaction: &transaction,
        })?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(result)
    }

    pub(super) fn transaction_put(
        transaction: &mut super::StoreTransaction<'_>,
        namespace: &str,
        key: &str,
        value: &[u8],
    ) -> Result<(), StoreError> {
        match namespace {
            "quarantine" => upsert_kv_blob(transaction.transaction, QUARANTINE_SCOPE, key, value),
            _ => crate::documents::write_document(transaction.transaction, key, value),
        }
    }

    pub(super) fn transaction_delete(
        transaction: &mut super::StoreTransaction<'_>,
        namespace: &str,
        key: &str,
    ) -> Result<bool, StoreError> {
        match namespace {
            "quarantine" => transaction
                .transaction
                .execute(
                    "DELETE FROM kv WHERE scope = ?1 AND key = ?2",
                    params![QUARANTINE_SCOPE, key],
                )
                .map(|changed| changed > 0)
                .map_err(StoreError::from),
            _ => crate::documents::delete_document(transaction.transaction, key),
        }
    }

    pub(super) fn transaction_append_outbox(
        transaction: &mut super::StoreTransaction<'_>,
        events: &[smelt_plugin_api::EventEnvelope<serde_json::Value>],
    ) -> Result<(), StoreError> {
        insert_outbox_events(transaction.transaction, events)
    }

    pub(super) fn get(
        backend: &Backend,
        namespace: &str,
        key: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let connection = lock_connection(backend)?;
        match namespace {
            "quarantine" => connection
                .query_row(
                    "SELECT value FROM kv
                     WHERE scope = ?1 AND key = ?2 AND value_type = 'blob'",
                    params![QUARANTINE_SCOPE, key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StoreError::from),
            _ => crate::documents::read_document(&connection, key),
        }
    }

    pub(super) fn put(
        backend: &Backend,
        namespace: &str,
        key: &str,
        value: &[u8],
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        match namespace {
            "quarantine" => {
                upsert_kv_blob(&transaction, QUARANTINE_SCOPE, key, value)?;
            }
            _ => crate::documents::write_document(&transaction, key, value)?,
        }
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn get_blob(
        backend: &Backend,
        namespace: &str,
        key: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let connection = lock_connection(backend)?;
        connection
            .query_row(
                "SELECT value FROM kv
                 WHERE scope = ?1 AND key = ?2 AND value_type = 'blob'",
                params![namespace, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub(super) fn put_blob(
        backend: &Backend,
        namespace: &str,
        key: &str,
        value: &[u8],
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        upsert_kv_blob(&transaction, namespace, key, value)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn blob_keys(backend: &Backend, namespace: &str) -> Result<Vec<String>, StoreError> {
        let connection = lock_connection(backend)?;
        let mut statement = connection.prepare(
            "SELECT key FROM kv
                 WHERE scope = ?1 AND value_type = 'blob'
                 ORDER BY key",
        )?;
        let rows = statement.query_map(params![namespace], |row| row.get(0))?;
        rows.collect::<Result<Vec<String>, _>>()
            .map_err(StoreError::from)
    }

    pub(super) fn delete_blob(
        backend: &Backend,
        namespace: &str,
        key: &str,
    ) -> Result<bool, StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "DELETE FROM kv WHERE scope = ?1 AND key = ?2 AND value_type = 'blob'",
            params![namespace, key],
        )?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(changed > 0)
    }

    pub(super) fn put_automation_run_transcript(
        backend: &Backend,
        run_id: &str,
        entries_json: &[u8],
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO automation_run_transcript(run_id, entries_json, updated_at_ms)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(run_id) DO UPDATE SET
                   entries_json = excluded.entries_json,
                   updated_at_ms = excluded.updated_at_ms",
            params![run_id, entries_json, now_ms()],
        )?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn get_automation_run_transcript(
        backend: &Backend,
        run_id: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let connection = lock_connection(backend)?;
        connection
            .query_row(
                "SELECT entries_json FROM automation_run_transcript WHERE run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub(super) fn get_automation_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::AutomationSnapshot>, StoreError> {
        let connection = lock_connection(backend)?;
        crate::documents::read_automation_snapshot(&connection)
    }

    pub(super) fn put_automation_snapshot(
        backend: &Backend,
        snapshot: &super::AutomationSnapshot,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::documents::write_automation_snapshot(&transaction, snapshot)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn get_launch_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::LaunchSnapshot>, StoreError> {
        let connection = lock_connection(backend)?;
        crate::documents::read_launch_snapshot(&connection)
    }

    pub(super) fn put_launch_snapshot(
        backend: &Backend,
        snapshot: &super::LaunchSnapshot,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::documents::write_launch_snapshot(&transaction, snapshot)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn get_history_title_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::HistoryTitleSnapshot>, StoreError> {
        let connection = lock_connection(backend)?;
        crate::documents::read_history_title_snapshot(&connection)
    }

    pub(super) fn put_history_title_snapshot(
        backend: &Backend,
        snapshot: &super::HistoryTitleSnapshot,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::documents::write_history_title_snapshot(&transaction, snapshot)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn get_appearance_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::AppearanceSnapshot>, StoreError> {
        let connection = lock_connection(backend)?;
        crate::documents::read_appearance_snapshot(&connection)
    }

    pub(super) fn put_appearance_snapshot(
        backend: &Backend,
        snapshot: &super::AppearanceSnapshot,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::documents::write_appearance_snapshot(&transaction, snapshot)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn get_workspace_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::WorkspaceSnapshot>, StoreError> {
        let connection = lock_connection(backend)?;
        crate::documents::read_workspace_snapshot(&connection)
    }

    pub(super) fn put_workspace_snapshot(
        backend: &Backend,
        snapshot: &super::WorkspaceSnapshot,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::documents::write_workspace_snapshot(&transaction, snapshot)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn workspace_snapshot_from_json(
        raw: &[u8],
    ) -> Result<super::WorkspaceSnapshot, StoreError> {
        let value: serde_json::Value = serde_json::from_slice(raw)?;
        crate::documents::workspace_from_document(&value)
    }

    pub(super) fn workspace_snapshot_to_json(
        snapshot: &super::WorkspaceSnapshot,
    ) -> Result<Vec<u8>, StoreError> {
        let value = crate::documents::json_from_workspace_snapshot(snapshot)?;
        serde_json::to_vec(&value).map_err(StoreError::from)
    }

    pub(super) fn get_agent_ui_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::AgentUiSnapshot>, StoreError> {
        let connection = lock_connection(backend)?;
        crate::catalog::read_agent_ui_snapshot(&connection)
    }

    pub(super) fn put_agent_ui_snapshot(
        backend: &Backend,
        snapshot: &super::AgentUiSnapshot,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::catalog::write_agent_ui_snapshot(&transaction, snapshot)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn put_agent_ui_prefs(
        backend: &Backend,
        snapshot: &super::AgentUiSnapshot,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::catalog::write_agent_ui_prefs(&transaction, snapshot)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn insert_agent_definition(
        backend: &Backend,
        record: &super::AgentDefinitionRecord,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::catalog::insert_agent_definition(&transaction, record)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn update_agent_definition(
        backend: &Backend,
        record: &super::AgentDefinitionRecord,
    ) -> Result<bool, StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = crate::catalog::update_agent_definition(&transaction, record)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(changed)
    }

    pub(super) fn delete_agent_definition(backend: &Backend, id: &str) -> Result<bool, StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = crate::catalog::delete_agent_definition(&transaction, id)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(changed)
    }

    pub(super) fn get_remote_session_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::RemoteSessionCatalogSnapshot>, StoreError> {
        let connection = lock_connection(backend)?;
        crate::catalog::read_remote_session_snapshot(&connection)
    }

    pub(super) fn put_remote_session_snapshot(
        backend: &Backend,
        snapshot: &super::RemoteSessionCatalogSnapshot,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::catalog::write_remote_session_snapshot(&transaction, snapshot)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn get_published_session_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::PublishedSessionSnapshot>, StoreError> {
        let connection = lock_connection(backend)?;
        crate::catalog::read_published_session_snapshot(&connection)
    }

    pub(super) fn agent_ui_snapshot_from_json(
        value: &serde_json::Value,
    ) -> Result<super::AgentUiSnapshot, StoreError> {
        crate::catalog::agent_ui_from_document(value)
    }

    pub(super) fn put_published_session_snapshot(
        backend: &Backend,
        snapshot: &super::PublishedSessionSnapshot,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::catalog::write_published_session_snapshot(&transaction, snapshot)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    fn get_config<T>(
        backend: &Backend,
        read: impl FnOnce(&rusqlite::Connection) -> Result<Option<T>, StoreError>,
    ) -> Result<Option<T>, StoreError> {
        let connection = lock_connection(backend)?;
        read(&connection)
    }

    fn put_config<T>(
        backend: &Backend,
        snapshot: &T,
        write: impl FnOnce(&rusqlite::Transaction<'_>, &T) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        write(&transaction, snapshot)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn get_remote_config_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::RemoteConfigSnapshot>, StoreError> {
        get_config(backend, crate::config::read_remote_config_snapshot)
    }

    pub(super) fn put_remote_config_snapshot(
        backend: &Backend,
        snapshot: &super::RemoteConfigSnapshot,
    ) -> Result<(), StoreError> {
        put_config(
            backend,
            snapshot,
            crate::config::write_remote_config_snapshot,
        )
    }

    pub(super) fn get_update_settings_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::UpdateSettingsSnapshot>, StoreError> {
        get_config(backend, crate::config::read_update_settings_snapshot)
    }

    pub(super) fn put_update_settings_snapshot(
        backend: &Backend,
        snapshot: &super::UpdateSettingsSnapshot,
    ) -> Result<(), StoreError> {
        put_config(
            backend,
            snapshot,
            crate::config::write_update_settings_snapshot,
        )
    }

    pub(super) fn get_update_state_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::UpdateStateSnapshot>, StoreError> {
        get_config(backend, crate::config::read_update_state_snapshot)
    }

    pub(super) fn put_update_state_snapshot(
        backend: &Backend,
        snapshot: &super::UpdateStateSnapshot,
    ) -> Result<(), StoreError> {
        put_config(
            backend,
            snapshot,
            crate::config::write_update_state_snapshot,
        )
    }

    pub(super) fn get_worktree_inherit_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::WorktreeInheritSnapshot>, StoreError> {
        get_config(backend, crate::config::read_worktree_inherit_snapshot)
    }

    pub(super) fn put_worktree_inherit_snapshot(
        backend: &Backend,
        snapshot: &super::WorktreeInheritSnapshot,
    ) -> Result<(), StoreError> {
        put_config(
            backend,
            snapshot,
            crate::config::write_worktree_inherit_snapshot,
        )
    }

    pub(super) fn get_terminal_theme_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::TerminalThemeSnapshot>, StoreError> {
        get_config(backend, crate::config::read_terminal_theme_snapshot)
    }

    pub(super) fn put_terminal_theme_snapshot(
        backend: &Backend,
        snapshot: &super::TerminalThemeSnapshot,
    ) -> Result<(), StoreError> {
        put_config(
            backend,
            snapshot,
            crate::config::write_terminal_theme_snapshot,
        )
    }

    pub(super) fn get_quota_cache_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::QuotaCacheSnapshot>, StoreError> {
        get_config(backend, crate::config::read_quota_cache_snapshot)
    }

    pub(super) fn put_quota_cache_snapshot(
        backend: &Backend,
        snapshot: &super::QuotaCacheSnapshot,
    ) -> Result<(), StoreError> {
        put_config(backend, snapshot, crate::config::write_quota_cache_snapshot)
    }

    pub(super) fn get_dsh_auto_model_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::AutoModelSnapshot>, StoreError> {
        get_config(backend, crate::config::read_dsh_auto_model_snapshot)
    }

    pub(super) fn put_dsh_auto_model_snapshot(
        backend: &Backend,
        snapshot: &super::AutoModelSnapshot,
    ) -> Result<(), StoreError> {
        put_config(
            backend,
            snapshot,
            crate::config::write_dsh_auto_model_snapshot,
        )
    }

    pub(super) fn get_pi_auto_model_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::AutoModelSnapshot>, StoreError> {
        get_config(backend, crate::config::read_pi_auto_model_snapshot)
    }

    pub(super) fn put_pi_auto_model_snapshot(
        backend: &Backend,
        snapshot: &super::AutoModelSnapshot,
    ) -> Result<(), StoreError> {
        put_config(
            backend,
            snapshot,
            crate::config::write_pi_auto_model_snapshot,
        )
    }

    pub(super) fn get_workspace_menu_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::PublishedWorkspaceMenuSnapshot>, StoreError> {
        get_config(backend, crate::config::read_workspace_menu_snapshot)
    }

    pub(super) fn put_workspace_menu_snapshot(
        backend: &Backend,
        snapshot: &super::PublishedWorkspaceMenuSnapshot,
    ) -> Result<(), StoreError> {
        put_config(
            backend,
            snapshot,
            crate::config::write_workspace_menu_snapshot,
        )
    }

    pub(super) fn retain_automation_run_transcripts(
        backend: &Backend,
        keep: &HashSet<String>,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = {
            let mut statement =
                transaction.prepare("SELECT run_id FROM automation_run_transcript")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<String>, _>>()?
        };
        for run_id in existing {
            if keep.contains(&run_id) {
                continue;
            }
            transaction.execute(
                "DELETE FROM automation_run_transcript WHERE run_id = ?1",
                params![run_id],
            )?;
        }
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn delete(
        backend: &Backend,
        namespace: &str,
        key: &str,
    ) -> Result<bool, StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = match namespace {
            "quarantine" => {
                transaction.execute(
                    "DELETE FROM kv WHERE scope = ?1 AND key = ?2",
                    params![QUARANTINE_SCOPE, key],
                )? > 0
            }
            _ => crate::documents::delete_document(&transaction, key)?,
        };
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(changed)
    }

    pub(super) fn keys(backend: &Backend, namespace: &str) -> Result<Vec<String>, StoreError> {
        let connection = lock_connection(backend)?;
        if namespace == "quarantine" {
            let mut statement =
                connection.prepare("SELECT key FROM kv WHERE scope = ?1 ORDER BY key")?;
            let rows = statement.query_map([QUARANTINE_SCOPE], |row| row.get(0))?;
            return rows
                .collect::<Result<Vec<String>, _>>()
                .map_err(StoreError::from);
        }
        crate::documents::list_document_keys(&connection)
    }

    pub(super) fn legacy_state(backend: &Backend, source: &str) -> Result<LegacyState, StoreError> {
        let connection = lock_connection(backend)?;
        let status = read_legacy_record(&connection, source)?.map(|(_, _, status)| status);
        match status.as_deref() {
            None => Ok(LegacyState::Pending),
            Some("imported") => Ok(LegacyState::Imported),
            Some("deleted") => Ok(LegacyState::Deleted),
            Some(other) => Err(StoreError::UnknownLegacyStatus {
                status: other.to_string(),
            }),
        }
    }

    pub(super) fn get_with_legacy_state(
        backend: &Backend,
        namespace: &str,
        key: &str,
        source: &str,
    ) -> Result<(Option<Vec<u8>>, LegacyState), StoreError> {
        let connection = lock_connection(backend)?;
        let value = crate::documents::read_document(&connection, key)?;
        let state = match read_legacy_record(&connection, source)? {
            None => LegacyState::Pending,
            Some((mapped_namespace, mapped_key, status)) => {
                if mapped_namespace != namespace || mapped_key != key {
                    return Err(StoreError::LegacyMappingMismatch {
                        legacy_source: source.to_string(),
                        mapped_namespace,
                        mapped_key,
                        namespace: namespace.to_string(),
                        key: key.to_string(),
                        action: "读取",
                    });
                }
                match status.as_str() {
                    "imported" => LegacyState::Imported,
                    "deleted" => LegacyState::Deleted,
                    other => {
                        return Err(StoreError::UnknownLegacyStatus {
                            status: other.to_string(),
                        });
                    }
                }
            }
        };
        Ok((value, state))
    }

    pub(super) fn delete_with_legacy_tombstone(
        backend: &Backend,
        source: &str,
        namespace: &str,
        key: &str,
    ) -> Result<bool, StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_legacy_mapping(&transaction, source, namespace, key)?;
        upsert_deleted_tombstone(&transaction, source, namespace, key)?;
        let changed = crate::documents::delete_document(&transaction, key)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(changed)
    }

    pub(super) fn quarantine_with_legacy_tombstone(
        backend: &Backend,
        source: &str,
        namespace: &str,
        key: &str,
        quarantine_namespace: &str,
        quarantine_key: &str,
    ) -> Result<bool, StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_legacy_mapping(&transaction, source, namespace, key)?;
        let _ = quarantine_namespace;
        let payload = crate::documents::read_document(&transaction, key)?;
        let moved = if let Some(payload) = payload {
            upsert_kv_blob(&transaction, QUARANTINE_SCOPE, quarantine_key, &payload)?;
            crate::documents::delete_document(&transaction, key)?;
            upsert_deleted_tombstone(&transaction, source, namespace, key)?;
            true
        } else {
            false
        };
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(moved)
    }

    pub(super) fn mark_legacy_deleted(
        backend: &Backend,
        source: &str,
        namespace: &str,
        key: &str,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_legacy_mapping(&transaction, source, namespace, key)?;
        upsert_deleted_tombstone(&transaction, source, namespace, key)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    fn validate_legacy_mapping(
        transaction: &rusqlite::Transaction<'_>,
        source: &str,
        namespace: &str,
        key: &str,
    ) -> Result<(), StoreError> {
        let mapped = read_legacy_record(transaction, source)?;
        if let Some((existing_namespace, existing_key, _)) = mapped
            && (existing_namespace != namespace || existing_key != key)
        {
            return Err(StoreError::LegacyMappingMismatch {
                legacy_source: source.to_string(),
                mapped_namespace: existing_namespace,
                mapped_key: existing_key,
                namespace: namespace.to_string(),
                key: key.to_string(),
                action: "改写",
            });
        }
        Ok(())
    }

    fn upsert_deleted_tombstone(
        transaction: &rusqlite::Transaction<'_>,
        source: &str,
        namespace: &str,
        key: &str,
    ) -> Result<(), StoreError> {
        upsert_legacy_record(transaction, source, namespace, key, "deleted")
    }

    fn legacy_scope(source: &str) -> String {
        format!("{LEGACY_SCOPE_PREFIX}{source}")
    }

    fn read_legacy_record(
        connection: &Connection,
        source: &str,
    ) -> Result<Option<(String, String, String)>, StoreError> {
        let scope = legacy_scope(source);
        let row = connection.query_row(
            "SELECT
                   MAX(CASE WHEN key = 'namespace' THEN CAST(value AS TEXT) END),
                   MAX(CASE WHEN key = 'key' THEN CAST(value AS TEXT) END),
                   MAX(CASE WHEN key = 'status' THEN CAST(value AS TEXT) END)
                 FROM kv WHERE scope = ?1",
            [&scope],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )?;
        match row {
            (None, None, None) => Ok(None),
            (Some(namespace), Some(key), Some(status)) => Ok(Some((namespace, key, status))),
            _ => Err(StoreError::IncompleteLegacyMetadata {
                legacy_source: source.to_string(),
            }),
        }
    }

    fn upsert_legacy_record(
        transaction: &rusqlite::Transaction<'_>,
        source: &str,
        namespace: &str,
        key: &str,
        status: &str,
    ) -> Result<(), StoreError> {
        let scope = legacy_scope(source);
        for (field, value) in [("namespace", namespace), ("key", key), ("status", status)] {
            upsert_kv_text(transaction, &scope, field, value)?;
        }
        Ok(())
    }

    fn upsert_kv_text(
        transaction: &rusqlite::Transaction<'_>,
        scope: &str,
        key: &str,
        value: &str,
    ) -> Result<(), StoreError> {
        transaction.execute(
            "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                 VALUES (?1, ?2, ?3, 'text', ?4)
                 ON CONFLICT(scope, key) DO UPDATE SET
                   value = excluded.value,
                   value_type = 'text',
                   updated_at_ms = excluded.updated_at_ms",
            params![scope, key, value, now_ms()],
        )?;
        Ok(())
    }

    fn upsert_kv_blob(
        transaction: &rusqlite::Transaction<'_>,
        scope: &str,
        key: &str,
        value: &[u8],
    ) -> Result<(), StoreError> {
        transaction.execute(
            "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                 VALUES (?1, ?2, ?3, 'blob', ?4)
                 ON CONFLICT(scope, key) DO UPDATE SET
                   value = excluded.value,
                   value_type = 'blob',
                   updated_at_ms = excluded.updated_at_ms",
            params![scope, key, value, now_ms()],
        )?;
        Ok(())
    }

    pub(super) fn append_event_batch(
        backend: &Backend,
        events: &[smelt_event_bus::StoredEvent],
    ) -> Result<Vec<smelt_event_bus::StoredEvent>, StoreError> {
        append_event_batch_mode(backend, events, false)
    }

    pub(super) fn append_event_batch_idempotent(
        backend: &Backend,
        events: &[smelt_event_bus::StoredEvent],
    ) -> Result<Vec<smelt_event_bus::StoredEvent>, StoreError> {
        append_event_batch_mode(backend, events, true)
    }

    fn append_event_batch_mode(
        backend: &Backend,
        events: &[smelt_event_bus::StoredEvent],
        idempotent: bool,
    ) -> Result<Vec<smelt_event_bus::StoredEvent>, StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transfer_pending_outbox(&transaction, None)?;
        let appended = if events.is_empty() {
            Vec::new()
        } else {
            insert_event_batch(&transaction, events, idempotent)?
        };
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(appended)
    }

    fn insert_event_batch(
        transaction: &rusqlite::Transaction<'_>,
        events: &[smelt_event_bus::StoredEvent],
        idempotent: bool,
    ) -> Result<Vec<smelt_event_bus::StoredEvent>, StoreError> {
        let mut appended = Vec::with_capacity(events.len());
        let mut statement = transaction.prepare_cached(
            "INSERT INTO event_log(
                   event_id, topic, aggregate_kind, aggregate_id, aggregate_revision,
                   occurred_at_ms, envelope_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for event in events {
            let existing = transaction
                .query_row(
                    "SELECT sequence, envelope_json FROM event_log WHERE event_id = ?1",
                    [event.envelope.event_id.as_str()],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .optional()?;
            if let Some((sequence, wire_bytes)) = existing {
                if !idempotent {
                    return Err(StoreError::DuplicateEventId);
                }
                let envelope = serde_json::from_slice(&wire_bytes)?;
                if envelope != event.envelope {
                    return Err(StoreError::EventIdConflict {
                        event_id: event.envelope.event_id.to_string(),
                    });
                }
                appended.push(smelt_event_bus::StoredEvent {
                    sequence: sequence as u64,
                    envelope,
                    wire_bytes,
                });
                continue;
            }
            let aggregate = event.envelope.aggregate.as_ref();
            if let (Some(aggregate), Some(revision)) =
                (aggregate, event.envelope.aggregate_revision)
            {
                let current = transaction.query_row(
                    "SELECT MAX(aggregate_revision) FROM event_log
                         WHERE aggregate_kind = ?1 AND aggregate_id = ?2",
                    params![aggregate.kind, aggregate.id],
                    |row| row.get::<_, Option<i64>>(0),
                )?;
                if current.is_some_and(|current| revision <= current as u64) {
                    return Err(StoreError::AggregateRevisionMovedBackwards {
                        kind: aggregate.kind.clone(),
                        id: aggregate.id.clone(),
                    });
                }
            }
            statement.execute(params![
                event.envelope.event_id.as_str(),
                event.envelope.topic.as_str(),
                aggregate.map(|value| value.kind.as_str()),
                aggregate.map(|value| value.id.as_str()),
                event.envelope.aggregate_revision.map(|value| value as i64),
                event.envelope.occurred_at_ms as i64,
                event.wire_bytes,
            ])?;
            let mut stored = event.clone();
            stored.sequence = transaction.last_insert_rowid() as u64;
            appended.push(stored);
        }
        Ok(appended)
    }

    pub(super) fn load_events_after_topics(
        backend: &Backend,
        sequence: u64,
        topics: &std::collections::BTreeSet<smelt_plugin_api::Topic>,
        limit: usize,
    ) -> Result<Vec<smelt_event_bus::StoredEvent>, StoreError> {
        if topics.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let connection = lock_connection(backend)?;
        let placeholders = std::iter::repeat_n("?", topics.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT sequence, envelope_json FROM event_log
             WHERE sequence > ? AND topic IN ({placeholders})
             ORDER BY sequence LIMIT ?"
        );
        let mut statement = connection.prepare(&sql)?;
        let mut parameters = Vec::with_capacity(topics.len() + 2);
        parameters.push(rusqlite::types::Value::Integer(sequence as i64));
        parameters.extend(
            topics
                .iter()
                .map(|topic| rusqlite::types::Value::Text(topic.as_str().to_owned())),
        );
        parameters.push(rusqlite::types::Value::Integer(limit as i64));
        let rows = statement.query_map(rusqlite::params_from_iter(parameters), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        rows.map(|row| {
            let (sequence, wire_bytes) = row?;
            let envelope = serde_json::from_slice(&wire_bytes)?;
            Ok(smelt_event_bus::StoredEvent {
                sequence: sequence as u64,
                envelope,
                wire_bytes,
            })
        })
        .collect()
    }

    pub(super) fn event_high_watermark(backend: &Backend) -> Result<u64, StoreError> {
        let connection = lock_connection(backend)?;
        connection
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) FROM event_log",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|value| value as u64)
            .map_err(StoreError::from)
    }

    pub(super) fn append_outbox_batch(
        backend: &Backend,
        events: &[smelt_plugin_api::EventEnvelope<serde_json::Value>],
    ) -> Result<(), StoreError> {
        if events.is_empty() {
            return Ok(());
        }
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_outbox_events(&transaction, events)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    fn insert_outbox_events(
        transaction: &rusqlite::Transaction<'_>,
        events: &[smelt_plugin_api::EventEnvelope<serde_json::Value>],
    ) -> Result<(), StoreError> {
        let mut statement = transaction.prepare_cached(
            "INSERT INTO event_outbox(event_id, envelope_json, created_at_ms)
                 VALUES (?1, ?2, ?3)",
        )?;
        for event in events {
            let encoded = serde_json::to_vec(event)?;
            statement.execute(params![event.event_id.as_str(), encoded, now_ms()])?;
        }
        Ok(())
    }

    pub(super) fn get_peer_message_command_result(
        backend: &Backend,
        source_session_id: &str,
        command_id: &smelt_plugin_api::CommandId,
    ) -> Result<Option<super::PeerMessageCommandResult>, StoreError> {
        let connection = lock_connection(backend)?;
        connection
            .query_row(
                "SELECT request_fingerprint, completed_at_ms, message_id, target_session_id
                 FROM peer_message_command
                 WHERE source_session_id = ?1 AND command_id = ?2",
                params![source_session_id, command_id.as_str()],
                |row| {
                    Ok(super::PeerMessageCommandResult {
                        source_session_id: source_session_id.to_string(),
                        command_id: command_id.clone(),
                        request_fingerprint: row.get(0)?,
                        completed_at_ms: row.get::<_, i64>(1)? as u64,
                        message_id: row.get(2)?,
                        target_session_id: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub(super) fn commit_peer_message_delivery(
        backend: &Backend,
        result: &super::PeerMessageCommandResult,
        event: &smelt_plugin_api::EventEnvelope<serde_json::Value>,
    ) -> Result<super::PeerMessageCommitOutcome, StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO peer_message_command(
                   source_session_id, command_id, request_fingerprint, completed_at_ms,
                   message_id, target_session_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                result.source_session_id.as_str(),
                result.command_id.as_str(),
                result.request_fingerprint.as_str(),
                result.completed_at_ms as i64,
                result.message_id.as_str(),
                result.target_session_id.as_str(),
            ],
        )? > 0;
        if inserted {
            insert_outbox_events(&transaction, std::slice::from_ref(event))?;
            transaction.commit()?;
            secure_sidecar_permissions(&backend.path);
            return Ok(super::PeerMessageCommitOutcome::Inserted);
        }
        let existing = transaction.query_row(
            "SELECT request_fingerprint, completed_at_ms, message_id, target_session_id
                 FROM peer_message_command
                 WHERE source_session_id = ?1 AND command_id = ?2",
            params![
                result.source_session_id.as_str(),
                result.command_id.as_str()
            ],
            |row| {
                Ok(super::PeerMessageCommandResult {
                    source_session_id: result.source_session_id.clone(),
                    command_id: result.command_id.clone(),
                    request_fingerprint: row.get(0)?,
                    completed_at_ms: row.get::<_, i64>(1)? as u64,
                    message_id: row.get(2)?,
                    target_session_id: row.get(3)?,
                })
            },
        )?;
        transaction.commit()?;
        if existing.request_fingerprint == result.request_fingerprint {
            Ok(super::PeerMessageCommitOutcome::Existing(existing))
        } else {
            Ok(super::PeerMessageCommitOutcome::Conflict(existing))
        }
    }

    pub(super) fn load_pending_outbox(
        backend: &Backend,
        limit: usize,
    ) -> Result<Vec<super::OutboxEvent>, StoreError> {
        let connection = lock_connection(backend)?;
        let mut statement = connection.prepare_cached(
            "SELECT outbox_id, envelope_json, created_at_ms FROM event_outbox
                 WHERE dispatched_sequence IS NULL ORDER BY outbox_id LIMIT ?1",
        )?;
        let rows = statement.query_map([limit as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (outbox_id, encoded, created_at_ms) = row?;
            Ok(super::OutboxEvent {
                outbox_id: outbox_id as u64,
                envelope: serde_json::from_slice(&encoded)?,
                created_at_ms: created_at_ms as u64,
            })
        })
        .collect()
    }

    pub(super) fn mark_outbox_dispatched(
        backend: &Backend,
        dispatched: &[(u64, u64)],
    ) -> Result<(), StoreError> {
        if dispatched.is_empty() {
            return Ok(());
        }
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut statement = transaction.prepare_cached(
                "UPDATE event_outbox SET dispatched_sequence = ?2
                     WHERE outbox_id = ?1 AND dispatched_sequence IS NULL",
            )?;
            for (outbox_id, sequence) in dispatched {
                statement.execute(params![*outbox_id as i64, *sequence as i64])?;
            }
        }
        transaction.commit().map_err(StoreError::from)
    }

    pub(super) fn dispatch_pending_outbox(
        backend: &Backend,
        limit: usize,
    ) -> Result<Vec<smelt_event_bus::StoredEvent>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let dispatched = transfer_pending_outbox(&transaction, Some(limit))?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(dispatched)
    }

    fn transfer_pending_outbox(
        transaction: &rusqlite::Transaction<'_>,
        limit: Option<usize>,
    ) -> Result<Vec<smelt_event_bus::StoredEvent>, StoreError> {
        let pending = {
            let mut statement = transaction.prepare(
                "SELECT outbox_id, envelope_json FROM event_outbox
                     WHERE dispatched_sequence IS NULL ORDER BY outbox_id LIMIT ?1",
            )?;
            statement
                .query_map([limit.map_or(i64::MAX, |limit| limit as i64)], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut dispatched = Vec::with_capacity(pending.len());
        for (outbox_id, wire_bytes) in pending {
            let envelope: smelt_plugin_api::EventEnvelope<serde_json::Value> =
                serde_json::from_slice(&wire_bytes)?;
            let existing = transaction
                .query_row(
                    "SELECT sequence, envelope_json FROM event_log WHERE event_id = ?1",
                    [envelope.event_id.as_str()],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .optional()?;
            let stored = if let Some((sequence, existing_wire)) = existing {
                let existing_envelope = serde_json::from_slice(&existing_wire)?;
                if existing_envelope != envelope {
                    return Err(StoreError::EventIdConflict {
                        event_id: envelope.event_id.to_string(),
                    });
                }
                smelt_event_bus::StoredEvent {
                    sequence: sequence as u64,
                    envelope: existing_envelope,
                    wire_bytes: existing_wire,
                }
            } else {
                insert_event_batch(
                    transaction,
                    std::slice::from_ref(&smelt_event_bus::StoredEvent {
                        sequence: 0,
                        envelope,
                        wire_bytes,
                    }),
                    false,
                )?
                .pop()
                .ok_or(StoreError::OutboxAppendFailed)?
            };
            transaction.execute(
                "UPDATE event_outbox SET dispatched_sequence = ?2
                     WHERE outbox_id = ?1 AND dispatched_sequence IS NULL",
                params![outbox_id, stored.sequence as i64],
            )?;
            dispatched.push(stored);
        }
        Ok(dispatched)
    }

    pub(super) fn bind_cursor_declaration(
        backend: &Backend,
        plugin_id: &smelt_plugin_api::PluginId,
        subscription_id: &smelt_plugin_api::SubscriptionId,
        declaration_fingerprint: &str,
    ) -> Result<super::CursorDeclaration, StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT last_acked_sequence, declaration_fingerprint
                 FROM subscription_cursor
                 WHERE plugin_id = ?1 AND subscription_id = ?2",
                params![plugin_id.as_str(), subscription_id.as_str()],
                |row| {
                    Ok(super::CursorDeclaration {
                        last_acked_sequence: row.get::<_, i64>(0)? as u64,
                        declaration_fingerprint: row.get(1)?,
                    })
                },
            )
            .optional()?;
        let declaration = match existing {
            Some(mut declaration)
                if declaration.declaration_fingerprint == LEGACY_V3_FINGERPRINT =>
            {
                transaction.execute(
                    "UPDATE subscription_cursor
                         SET last_acked_sequence = 0,
                             declaration_fingerprint = ?3,
                             updated_at_ms = ?4
                         WHERE plugin_id = ?1 AND subscription_id = ?2
                           AND declaration_fingerprint = ?5",
                    params![
                        plugin_id.as_str(),
                        subscription_id.as_str(),
                        declaration_fingerprint,
                        now_ms(),
                        LEGACY_V3_FINGERPRINT,
                    ],
                )?;
                declaration.last_acked_sequence = 0;
                declaration.declaration_fingerprint = declaration_fingerprint.to_string();
                declaration
            }
            Some(declaration) if declaration.declaration_fingerprint != declaration_fingerprint => {
                return Err(StoreError::CursorDeclarationConflict);
            }
            Some(declaration) => declaration,
            None => {
                transaction.execute(
                    "INSERT INTO subscription_cursor(
                           plugin_id, subscription_id, last_acked_sequence,
                           declaration_fingerprint, updated_at_ms
                         ) VALUES (?1, ?2, 0, ?3, ?4)",
                    params![
                        plugin_id.as_str(),
                        subscription_id.as_str(),
                        declaration_fingerprint,
                        now_ms()
                    ],
                )?;
                super::CursorDeclaration {
                    last_acked_sequence: 0,
                    declaration_fingerprint: declaration_fingerprint.to_string(),
                }
            }
        };
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(declaration)
    }

    pub(super) fn load_cursor_declaration(
        backend: &Backend,
        plugin_id: &smelt_plugin_api::PluginId,
        subscription_id: &smelt_plugin_api::SubscriptionId,
    ) -> Result<Option<super::CursorDeclaration>, StoreError> {
        let connection = lock_connection(backend)?;
        connection
            .query_row(
                "SELECT last_acked_sequence, declaration_fingerprint
                 FROM subscription_cursor
                 WHERE plugin_id = ?1 AND subscription_id = ?2",
                params![plugin_id.as_str(), subscription_id.as_str()],
                |row| {
                    Ok(super::CursorDeclaration {
                        last_acked_sequence: row.get::<_, i64>(0)? as u64,
                        declaration_fingerprint: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub(super) fn store_cursors(
        backend: &Backend,
        cursors: &[(
            smelt_plugin_api::PluginId,
            smelt_plugin_api::SubscriptionId,
            u64,
        )],
    ) -> Result<(), StoreError> {
        if cursors.is_empty() {
            return Ok(());
        }
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut statement = transaction.prepare_cached(
                "UPDATE subscription_cursor SET
                       last_acked_sequence = MAX(last_acked_sequence, ?3),
                       updated_at_ms = ?4
                     WHERE plugin_id = ?1 AND subscription_id = ?2",
            )?;
            for (plugin, subscription, sequence) in cursors {
                let changed = statement.execute(params![
                    plugin.as_str(),
                    subscription.as_str(),
                    *sequence as i64,
                    now_ms()
                ])?;
                if changed == 0 {
                    return Err(StoreError::CursorUnboundOnStore);
                }
            }
        }
        transaction.commit().map_err(StoreError::from)
    }

    pub(super) fn put_dead_letter(
        backend: &Backend,
        dead: &smelt_event_bus::DeadLetter,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO event_dead_letter(
                   plugin_id, subscription_id, sequence, event_id, attempts, reason, failed_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(plugin_id, subscription_id, sequence) DO UPDATE SET
                   attempts = excluded.attempts,
                   reason = excluded.reason,
                   failed_at_ms = excluded.failed_at_ms",
            params![
                dead.plugin_id.as_str(),
                dead.subscription_id.as_str(),
                dead.sequence as i64,
                dead.event_id.as_str(),
                dead.attempts as i64,
                dead.reason,
                dead.failed_at_ms as i64,
            ],
        )?;
        transaction.execute(
            "DELETE FROM event_delivery_retry
                 WHERE plugin_id = ?1 AND subscription_id = ?2",
            params![dead.plugin_id.as_str(), dead.subscription_id.as_str()],
        )?;
        let changed = transaction.execute(
            "UPDATE subscription_cursor SET
                   last_acked_sequence = MAX(last_acked_sequence, ?3),
                   updated_at_ms = ?4
                 WHERE plugin_id = ?1 AND subscription_id = ?2",
            params![
                dead.plugin_id.as_str(),
                dead.subscription_id.as_str(),
                dead.sequence as i64,
                now_ms(),
            ],
        )?;
        if changed == 0 {
            return Err(StoreError::CursorUnboundOnDeadLetter);
        }
        transaction.commit()?;
        Ok(())
    }

    pub(super) fn load_dead_letters(
        backend: &Backend,
        plugin_id: &smelt_plugin_api::PluginId,
        subscription_id: &smelt_plugin_api::SubscriptionId,
        limit: usize,
    ) -> Result<Vec<smelt_event_bus::DeadLetter>, StoreError> {
        let connection = lock_connection(backend)?;
        let mut statement = connection.prepare_cached(
            "SELECT sequence, event_id, attempts, reason, failed_at_ms
                 FROM event_dead_letter
                 WHERE plugin_id = ?1 AND subscription_id = ?2
                 ORDER BY dead_letter_id LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![plugin_id.as_str(), subscription_id.as_str(), limit as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (sequence, event_id, attempts, reason, failed_at_ms) = row?;
            Ok(smelt_event_bus::DeadLetter {
                plugin_id: plugin_id.clone(),
                subscription_id: subscription_id.clone(),
                sequence: sequence as u64,
                event_id: smelt_plugin_api::EventId::new(event_id)?,
                attempts: attempts as u32,
                reason,
                failed_at_ms: failed_at_ms as u64,
            })
        })
        .collect()
    }

    pub(super) fn load_retry(
        backend: &Backend,
        plugin_id: &smelt_plugin_api::PluginId,
        subscription_id: &smelt_plugin_api::SubscriptionId,
    ) -> Result<Option<smelt_event_bus::DeliveryRetry>, StoreError> {
        let connection = lock_connection(backend)?;
        let row = connection
            .query_row(
                "SELECT sequence, event_id, attempts, next_attempt_at_ms, reason
                 FROM event_delivery_retry
                 WHERE plugin_id = ?1 AND subscription_id = ?2",
                params![plugin_id.as_str(), subscription_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        row.map(
            |(sequence, event_id, attempts, next_attempt_at_ms, reason)| {
                Ok(smelt_event_bus::DeliveryRetry {
                    plugin_id: plugin_id.clone(),
                    subscription_id: subscription_id.clone(),
                    sequence: sequence as u64,
                    event_id: smelt_plugin_api::EventId::new(event_id)?,
                    attempts: attempts as u32,
                    next_attempt_at_ms: next_attempt_at_ms as u64,
                    reason,
                })
            },
        )
        .transpose()
    }

    pub(super) fn put_retry(
        backend: &Backend,
        retry: &smelt_event_bus::DeliveryRetry,
    ) -> Result<(), StoreError> {
        let connection = lock_connection(backend)?;
        connection.execute(
            "INSERT INTO event_delivery_retry(
                   plugin_id, subscription_id, sequence, event_id, attempts,
                   next_attempt_at_ms, reason
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(plugin_id, subscription_id) DO UPDATE SET
                   sequence = excluded.sequence,
                   event_id = excluded.event_id,
                   attempts = excluded.attempts,
                   next_attempt_at_ms = excluded.next_attempt_at_ms,
                   reason = excluded.reason",
            params![
                retry.plugin_id.as_str(),
                retry.subscription_id.as_str(),
                retry.sequence as i64,
                retry.event_id.as_str(),
                retry.attempts as i64,
                retry.next_attempt_at_ms as i64,
                retry.reason,
            ],
        )?;
        Ok(())
    }

    pub(super) fn delete_retry(
        backend: &Backend,
        plugin_id: &smelt_plugin_api::PluginId,
        subscription_id: &smelt_plugin_api::SubscriptionId,
    ) -> Result<(), StoreError> {
        let connection = lock_connection(backend)?;
        connection.execute(
            "DELETE FROM event_delivery_retry
                 WHERE plugin_id = ?1 AND subscription_id = ?2",
            params![plugin_id.as_str(), subscription_id.as_str()],
        )?;
        Ok(())
    }

    pub(super) fn get_command_result(
        backend: &Backend,
        plugin_id: &smelt_plugin_api::PluginId,
        command_id: &smelt_plugin_api::CommandId,
    ) -> Result<Option<smelt_event_bus::CommandResult>, StoreError> {
        let connection = lock_connection(backend)?;
        let row = connection
            .query_row(
                "SELECT completed_at_ms, result_json FROM command_dedupe
                 WHERE plugin_id = ?1 AND command_id = ?2",
                params![plugin_id.as_str(), command_id.as_str()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        row.map(|(completed_at_ms, encoded)| {
            Ok(smelt_event_bus::CommandResult {
                plugin_id: plugin_id.clone(),
                command_id: command_id.clone(),
                completed_at_ms: completed_at_ms as u64,
                result: serde_json::from_slice(&encoded)?,
            })
        })
        .transpose()
    }

    pub(super) fn put_command_result(
        backend: &Backend,
        result: &smelt_event_bus::CommandResult,
    ) -> Result<bool, StoreError> {
        let encoded = serde_json::to_vec(&result.result)?;
        let connection = lock_connection(backend)?;
        connection
            .execute(
                "INSERT OR IGNORE INTO command_dedupe(
                   plugin_id, command_id, completed_at_ms, result_json
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    result.plugin_id.as_str(),
                    result.command_id.as_str(),
                    result.completed_at_ms as i64,
                    encoded
                ],
            )
            .map(|changed| changed > 0)
            .map_err(StoreError::from)
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0)
    }

    fn open_connection(
        path: &Path,
        flags: OpenFlags,
        create: bool,
    ) -> Result<Connection, StoreError> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        if create {
            ensure_absent_or_create_private_file(path)?;
        } else {
            require_regular_private_file(path)?;
        }
        let connection =
            Connection::open_with_flags(path, flags).map_err(|error| StoreError::OpenFailed {
                path: path.to_path_buf(),
                source: OpenSource::from(error),
            })?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        // journal_mode 会持久修改数据库。先做只读版本检查，确保旧二进制拒绝未知
        // 未来 schema 时还没有改变它的存储属性。
        let observed =
            connection.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?;
        if observed > STORE_SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                observed,
                supported: STORE_SCHEMA_VERSION,
            });
        }
        enable_wal(&connection)?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        Ok(connection)
    }

    fn enable_wal(connection: &Connection) -> Result<(), StoreError> {
        let deadline = Instant::now() + BUSY_TIMEOUT;
        loop {
            let result = connection.pragma_update_and_check(None, "journal_mode", "WAL", |row| {
                row.get::<_, String>(0)
            });
            match result {
                Ok(mode) if mode.eq_ignore_ascii_case("wal") => return Ok(()),
                Ok(mode) => return Err(StoreError::WalNotEnabled { mode }),
                Err(error)
                    if matches!(
                        error.sqlite_error_code(),
                        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
                    ) && Instant::now() < deadline =>
                {
                    std::thread::sleep(BUSY_RETRY_DELAY);
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn schema_backup_path(path: &Path, from_version: u32) -> PathBuf {
        path.with_file_name(format!(
            "{}.bak-v{from_version}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(super::DATABASE_FILE_NAME)
        ))
    }

    fn backup_database_before_migration(
        connection: &Connection,
        path: &Path,
        from_version: u32,
    ) -> Result<(), StoreError> {
        let backup = schema_backup_path(path, from_version);
        if backup.exists() {
            std::fs::remove_file(&backup)?;
        }
        let quoted = backup.to_string_lossy().replace('\'', "''");
        connection.execute(&format!("VACUUM INTO '{quoted}'"), [])?;
        Ok(())
    }

    fn initialize_schema(
        connection: &mut Connection,
        allow_bootstrap: bool,
        path: &Path,
    ) -> Result<(), StoreError> {
        let observed =
            connection.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?;
        if observed > 0 && observed < STORE_SCHEMA_VERSION {
            backup_database_before_migration(connection, path, observed)?;
        }
        if observed > STORE_SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                observed,
                supported: STORE_SCHEMA_VERSION,
            });
        } else if observed == STORE_SCHEMA_VERSION {
            if schema_matches_current(connection)? {
                return Ok(());
            }
            return Err(unrecognized_schema_error(observed));
        }
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 另一个进程可能在上面的无锁观察之后先完成了初始化。拿到写锁后必须
        // 重读版本，不能继续按旧观察重复 CREATE TABLE。
        let current =
            transaction.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?;
        if current > STORE_SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                observed: current,
                supported: STORE_SCHEMA_VERSION,
            });
        }
        if current == STORE_SCHEMA_VERSION {
            if schema_matches_current(&transaction)? {
                transaction.commit()?;
                return Ok(());
            }
            return Err(unrecognized_schema_error(current));
        }
        if current == 1 {
            if !schema_matches_v1(&transaction)? {
                return Err(unrecognized_schema_error(current));
            }
            let migration = format!(
                "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
                crate::schema::MIGRATE_V1_TO_V2,
                crate::schema::MIGRATE_V2_TO_V3,
                crate::schema::MIGRATE_V3_TO_V4,
                crate::schema::MIGRATE_V4_TO_V5,
                crate::schema::MIGRATE_V5_TO_V6,
                crate::schema::MIGRATE_V6_TO_V7,
                crate::schema::MIGRATE_V7_TO_V8,
                crate::schema::MIGRATE_V8_TO_V9,
            );
            migrate_v1_schema(&transaction, &migration)?;
            transaction.commit()?;
            return Ok(());
        }
        if current == 2 {
            transaction.execute_batch(&format!(
                "{}\n{}\n{}\n{}\n{}\n{}\n{}",
                crate::schema::MIGRATE_V2_TO_V3,
                crate::schema::MIGRATE_V3_TO_V4,
                crate::schema::MIGRATE_V4_TO_V5,
                crate::schema::MIGRATE_V5_TO_V6,
                crate::schema::MIGRATE_V6_TO_V7,
                crate::schema::MIGRATE_V7_TO_V8,
                crate::schema::MIGRATE_V8_TO_V9,
            ))?;
            apply_v9_automation_domain(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 3 {
            transaction.execute_batch(&format!(
                "{}\n{}\n{}\n{}\n{}\n{}",
                crate::schema::MIGRATE_V3_TO_V4,
                crate::schema::MIGRATE_V4_TO_V5,
                crate::schema::MIGRATE_V5_TO_V6,
                crate::schema::MIGRATE_V6_TO_V7,
                crate::schema::MIGRATE_V7_TO_V8,
                crate::schema::MIGRATE_V8_TO_V9,
            ))?;
            apply_v9_automation_domain(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 4 {
            transaction.execute_batch(&format!(
                "{}\n{}\n{}\n{}\n{}",
                crate::schema::MIGRATE_V4_TO_V5,
                crate::schema::MIGRATE_V5_TO_V6,
                crate::schema::MIGRATE_V6_TO_V7,
                crate::schema::MIGRATE_V7_TO_V8,
                crate::schema::MIGRATE_V8_TO_V9,
            ))?;
            apply_v9_automation_domain(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 5 {
            // 线上 v5 库的 acp_detail 仍带 multica_* 列，必须先重建成新形状；
            // 已经是新形状的库（自建/测试夹具）只补版本号。
            if !schema_matches_current(&transaction)? {
                if !schema_matches_v5(&transaction)? {
                    return Err(unrecognized_schema_error(current));
                }
                transaction.execute_batch(&format!(
                    "{}\n{}\n{}\n{}",
                    crate::schema::MIGRATE_V5_TO_V6,
                    crate::schema::MIGRATE_V6_TO_V7,
                    crate::schema::MIGRATE_V7_TO_V8,
                    crate::schema::MIGRATE_V8_TO_V9,
                ))?;
                apply_v9_automation_domain(&transaction)?;
                if !schema_matches_current(&transaction)? {
                    return Err(unrecognized_schema_error(current));
                }
            }
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            transaction.commit()?;
            return Ok(());
        }
        if current == 6 {
            // 线上 v6 库的 acp_detail 还没有智能体归属列；已经是新形状的库
            // （自建/测试夹具）只补版本号。
            if !schema_matches_current(&transaction)? {
                if !schema_matches_v6(&transaction)? {
                    return Err(unrecognized_schema_error(current));
                }
                transaction.execute_batch(&format!(
                    "{}\n{}\n{}",
                    crate::schema::MIGRATE_V6_TO_V7,
                    crate::schema::MIGRATE_V7_TO_V8,
                    crate::schema::MIGRATE_V8_TO_V9,
                ))?;
                apply_v9_automation_domain(&transaction)?;
                if !schema_matches_current(&transaction)? {
                    return Err(unrecognized_schema_error(current));
                }
            }
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            transaction.commit()?;
            return Ok(());
        }
        if current == 7 {
            if !schema_matches_current(&transaction)? {
                if !schema_matches_v7(&transaction)? {
                    return Err(unrecognized_schema_error(current));
                }
                transaction.execute_batch(&format!(
                    "{}\n{}",
                    crate::schema::MIGRATE_V7_TO_V8,
                    crate::schema::MIGRATE_V8_TO_V9,
                ))?;
                apply_v9_automation_domain(&transaction)?;
                if !schema_matches_current(&transaction)? {
                    return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
                }
            }
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            transaction.commit()?;
            return Ok(());
        }
        if current == 8 {
            if !schema_matches_current(&transaction)? {
                if !schema_matches_v8(&transaction)? {
                    return Err(unrecognized_schema_error(current));
                }
                transaction.execute_batch(crate::schema::MIGRATE_V8_TO_V9)?;
                apply_v9_automation_domain(&transaction)?;
                if !schema_matches_current(&transaction)? {
                    return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
                }
            }
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            transaction.commit()?;
            return Ok(());
        }
        if current == 11 {
            if !schema_matches_v11(&transaction)? {
                return Err(unrecognized_schema_error(current));
            }
            apply_v12_agent_model_columns(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 10 {
            if !schema_matches_v10(&transaction)? {
                return Err(unrecognized_schema_error(current));
            }
            apply_v11_engine_kind_columns(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 9 {
            if !schema_matches_current(&transaction)? {
                if !schema_matches_v9(&transaction)? {
                    return Err(unrecognized_schema_error(current));
                }
                apply_v10_catalog_domain(&transaction)?;
                if !schema_matches_current(&transaction)? {
                    return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
                }
            }
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            transaction.commit()?;
            return Ok(());
        }
        if current != 0 || !table_names(&transaction)?.is_empty() {
            return Err(unrecognized_schema_error(current));
        }
        if !allow_bootstrap {
            return Err(StoreError::UninitializedDatabase);
        }
        create_current_schema(&transaction)?;
        transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
        if !schema_matches_current(&transaction)? {
            return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
        }
        transaction.commit().map_err(StoreError::from)
    }

    pub(super) fn migrate_v1_schema(
        transaction: &rusqlite::Transaction<'_>,
        migration_sql: &str,
    ) -> Result<(), StoreError> {
        transaction.execute_batch(migration_sql)?;
        apply_v9_automation_domain(transaction)?;
        transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
        if !schema_matches_current(transaction)? {
            return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
        }
        Ok(())
    }

    fn apply_v9_automation_domain(
        transaction: &rusqlite::Transaction<'_>,
    ) -> Result<(), StoreError> {
        let has_automation: bool = transaction.query_row(
            "SELECT EXISTS(
                   SELECT 1 FROM sqlite_schema
                   WHERE type = 'table' AND name = 'automation'
                 )",
            [],
            |row| row.get(0),
        )?;
        if has_automation {
            crate::documents::migrate_automations_from_kv_document(transaction)?;
            crate::documents::rebuild_automation_run_transcript_fk(transaction)?;
        }
        apply_v10_catalog_domain(transaction)
    }

    fn apply_v10_catalog_domain(transaction: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
        let has_agent_ui: bool = transaction.query_row(
            "SELECT EXISTS(
                   SELECT 1 FROM sqlite_schema
                   WHERE type = 'table' AND name = 'agent_ui_pref'
                 )",
            [],
            |row| row.get(0),
        )?;
        if !has_agent_ui {
            transaction.execute_batch(crate::schema::MIGRATE_V9_TO_V10)?;
        }
        apply_v11_engine_kind_columns(transaction)?;
        crate::catalog::migrate_catalog_from_kv_documents(transaction)
    }

    fn apply_v11_engine_kind_columns(
        transaction: &rusqlite::Transaction<'_>,
    ) -> Result<(), StoreError> {
        if !table_columns(transaction, "agent_definition")?
            .iter()
            .any(|column| column == "engine_kind_id")
        {
            transaction.execute_batch(crate::schema::MIGRATE_V10_TO_V11)?;
        }
        apply_v12_agent_model_columns(transaction)
    }

    fn apply_v12_agent_model_columns(
        transaction: &rusqlite::Transaction<'_>,
    ) -> Result<(), StoreError> {
        if table_columns(transaction, "agent_definition")?
            .iter()
            .any(|column| column == "model_provider")
        {
            return Ok(());
        }
        transaction
            .execute_batch(crate::schema::MIGRATE_V11_TO_V12)
            .map_err(StoreError::from)
    }

    fn unrecognized_schema_error(version: u32) -> StoreError {
        StoreError::unrecognized_schema(version)
    }

    fn schema_matches_current(connection: &Connection) -> Result<bool, StoreError> {
        Ok(
            table_columns(connection, "acp_detail")? == CURRENT_ACP_COLUMNS
                && table_columns(connection, "automation_run_transcript")?
                    == ["run_id", "entries_json", "updated_at_ms"]
                && schema_matches_shape_outside_acp_table(connection, &crate::schema::TABLES)?
                && schema_objects(connection)?
                    == expected_schema_objects(SchemaGeneration::Current)?,
        )
    }

    fn schema_matches_v11(connection: &Connection) -> Result<bool, StoreError> {
        Ok(
            table_columns(connection, "acp_detail")? == CURRENT_ACP_COLUMNS
                && table_columns(connection, "automation_run_transcript")?
                    == ["run_id", "entries_json", "updated_at_ms"]
                && schema_matches_shape_outside_acp_table(connection, &crate::schema::TABLES)?
                && schema_objects(connection)? == expected_schema_objects(SchemaGeneration::V11)?,
        )
    }

    fn schema_matches_v10(connection: &Connection) -> Result<bool, StoreError> {
        Ok(
            table_columns(connection, "acp_detail")? == CURRENT_ACP_COLUMNS
                && table_columns(connection, "automation_run_transcript")?
                    == ["run_id", "entries_json", "updated_at_ms"]
                && schema_matches_shape_outside_acp_table(connection, &crate::schema::TABLES)?
                && schema_objects(connection)? == expected_schema_objects(SchemaGeneration::V10)?,
        )
    }

    /// v5 与 v6 只差 `acp_detail` 一张表，其余结构必须完全一致才允许就地重建。
    fn schema_matches_v5(connection: &Connection) -> Result<bool, StoreError> {
        Ok(
            table_columns(connection, "acp_detail")? == LEGACY_ACP_COLUMNS
                && schema_matches_shape_outside_acp_table(connection, &crate::schema::V7_TABLES)?
                && schema_objects_without_acp_table(connection)?
                    == expected_schema_objects(SchemaGeneration::V7WithoutAcpTable)?,
        )
    }

    /// v6 与 v7 同样只差 `acp_detail` 一张表。
    fn schema_matches_v6(connection: &Connection) -> Result<bool, StoreError> {
        Ok(table_columns(connection, "acp_detail")? == V6_ACP_COLUMNS
            && schema_matches_shape_outside_acp_table(connection, &crate::schema::V7_TABLES)?
            && schema_objects_without_acp_table(connection)?
                == expected_schema_objects(SchemaGeneration::V7WithoutAcpTable)?)
    }

    fn schema_matches_v7(connection: &Connection) -> Result<bool, StoreError> {
        Ok(
            table_columns(connection, "acp_detail")? == CURRENT_ACP_COLUMNS
                && schema_matches_shape_outside_acp_table(connection, &crate::schema::V7_TABLES)?
                && schema_objects(connection)? == expected_schema_objects(SchemaGeneration::V7)?,
        )
    }

    fn schema_matches_v9(connection: &Connection) -> Result<bool, StoreError> {
        Ok(
            table_columns(connection, "acp_detail")? == CURRENT_ACP_COLUMNS
                && table_columns(connection, "automation_run_transcript")?
                    == ["run_id", "entries_json", "updated_at_ms"]
                && schema_matches_shape_outside_acp_table(connection, &crate::schema::V9_TABLES)?
                && schema_objects(connection)? == expected_schema_objects(SchemaGeneration::V9)?,
        )
    }

    fn schema_matches_v8(connection: &Connection) -> Result<bool, StoreError> {
        Ok(
            table_columns(connection, "acp_detail")? == CURRENT_ACP_COLUMNS
                && table_columns(connection, "automation_run_transcript")?
                    == ["run_id", "entries_json", "updated_at_ms"]
                && schema_matches_shape_outside_acp_table(connection, &crate::schema::V8_TABLES)?
                && schema_objects_excluding_transcript(connection)?
                    == expected_schema_objects(SchemaGeneration::V8WithoutTranscript)?,
        )
    }

    fn schema_matches_shape_outside_acp_table(
        connection: &Connection,
        expected_tables: &[&str],
    ) -> Result<bool, StoreError> {
        let tables = table_names(connection)?;
        Ok(tables == expected_tables
            && schema_matches_shared_columns(connection)?
            && table_columns(connection, "event_log")?
                == [
                    "sequence",
                    "event_id",
                    "topic",
                    "aggregate_kind",
                    "aggregate_id",
                    "aggregate_revision",
                    "occurred_at_ms",
                    "envelope_json",
                ]
            && table_columns(connection, "event_outbox")?
                == [
                    "outbox_id",
                    "event_id",
                    "envelope_json",
                    "created_at_ms",
                    "dispatched_sequence",
                ]
            && table_columns(connection, "subscription_cursor")?
                == [
                    "plugin_id",
                    "subscription_id",
                    "last_acked_sequence",
                    "declaration_fingerprint",
                    "updated_at_ms",
                ]
            && table_columns(connection, "event_dead_letter")?
                == [
                    "dead_letter_id",
                    "plugin_id",
                    "subscription_id",
                    "sequence",
                    "event_id",
                    "attempts",
                    "reason",
                    "failed_at_ms",
                ]
            && table_columns(connection, "event_delivery_retry")?
                == [
                    "plugin_id",
                    "subscription_id",
                    "sequence",
                    "event_id",
                    "attempts",
                    "next_attempt_at_ms",
                    "reason",
                ]
            && table_columns(connection, "command_dedupe")?
                == ["plugin_id", "command_id", "completed_at_ms", "result_json"]
            && table_columns(connection, "peer_message_command")?
                == [
                    "source_session_id",
                    "command_id",
                    "request_fingerprint",
                    "completed_at_ms",
                    "message_id",
                    "target_session_id",
                ])
    }

    fn schema_matches_v1(connection: &Connection) -> Result<bool, StoreError> {
        let acp_columns = table_columns(connection, "acp_detail")?;
        Ok(table_names(connection)?
            == [
                "acp_detail",
                "history_title",
                "kv",
                "launch_entry",
                "layout_node",
                "project",
                "session",
                "session_group",
            ]
            // 真实 v1 库带 multica_* 列；重建过的库可能已经是 v6 或当前形状，
            // 三者都要放行，后续迁移会把它们收敛到同一形状。
            && (acp_columns == LEGACY_ACP_COLUMNS
                || acp_columns == V6_ACP_COLUMNS
                || acp_columns == CURRENT_ACP_COLUMNS)
            && schema_matches_shared_columns(connection)?
            && v1_schema_objects(connection)? == expected_schema_objects(SchemaGeneration::V1)?)
    }

    const CURRENT_ACP_COLUMNS: [&str; 15] = [
        "session_id",
        "agent",
        "profile_id",
        "history_session_id",
        "launch_command",
        "refresh_launch_from_settings",
        "fork_session_id",
        "fork_title",
        "fork_agent",
        "fork_profile_label",
        "fork_from_history",
        "pending_prompt",
        "pending_delivery_id",
        "agent_definition_id",
        "automation_id",
    ];

    /// v6 的 `acp_detail` 形状：还没有智能体对话与自动化的归属列。
    const V6_ACP_COLUMNS: [&str; 13] = [
        "session_id",
        "agent",
        "profile_id",
        "history_session_id",
        "launch_command",
        "refresh_launch_from_settings",
        "fork_session_id",
        "fork_title",
        "fork_agent",
        "fork_profile_label",
        "fork_from_history",
        "pending_prompt",
        "pending_delivery_id",
    ];

    /// v1..v5 共用的 `acp_detail` 形状，带已下线的 Multica 专用列。
    const LEGACY_ACP_COLUMNS: [&str; 19] = [
        "session_id",
        "agent",
        "profile_id",
        "history_session_id",
        "launch_command",
        "refresh_launch_from_settings",
        "fork_session_id",
        "fork_title",
        "fork_agent",
        "fork_profile_label",
        "fork_from_history",
        "multica_issue_id",
        "multica_workspace_id",
        "multica_issue_title",
        "multica_task_id",
        "multica_parent_comment_id",
        "multica_trigger_comment",
        "pending_prompt",
        "pending_delivery_id",
    ];

    fn schema_matches_shared_columns(connection: &Connection) -> Result<bool, StoreError> {
        Ok(table_columns(connection, "history_title")?
            == [
                "agent",
                "profile_id",
                "resume_id",
                "custom_title",
                "updated_at_ms",
            ]
            && table_columns(connection, "kv")?
                == ["scope", "key", "value", "value_type", "updated_at_ms"]
            && table_columns(connection, "launch_entry")?
                == ["position", "label", "command", "provider"]
            && table_columns(connection, "layout_node")?
                == [
                    "id",
                    "group_id",
                    "parent_id",
                    "position",
                    "kind",
                    "axis",
                    "session_id",
                    "size_px",
                ]
            && table_columns(connection, "project")? == ["root", "position", "collapsed"]
            && table_columns(connection, "session")?
                == [
                    "id",
                    "group_id",
                    "kind",
                    "cwd",
                    "custom_title",
                    "launch_label",
                    "launch_command",
                ]
            && table_columns(connection, "session_group")?
                == [
                    "id",
                    "project_root",
                    "position",
                    "active_session_id",
                    "custom_title",
                    "last_updated_at",
                ])
    }

    fn v1_schema_objects(connection: &Connection) -> Result<Vec<SchemaObject>, StoreError> {
        Ok(schema_objects(connection)?
            .into_iter()
            .filter(|object| {
                object.name != "acp_detail"
                    && matches!(
                        object.table_name.as_str(),
                        "acp_detail"
                            | "history_title"
                            | "kv"
                            | "launch_entry"
                            | "layout_node"
                            | "project"
                            | "session"
                            | "session_group"
                    )
            })
            .collect())
    }

    fn schema_objects_without_acp_table(
        connection: &Connection,
    ) -> Result<Vec<SchemaObject>, StoreError> {
        Ok(schema_objects(connection)?
            .into_iter()
            .filter(|object| object.name != "acp_detail")
            .collect())
    }

    fn schema_objects_excluding_transcript(
        connection: &Connection,
    ) -> Result<Vec<SchemaObject>, StoreError> {
        Ok(schema_objects(connection)?
            .into_iter()
            .filter(|object| object.table_name != "automation_run_transcript")
            .collect())
    }

    fn is_v9_automation_schema_object(object: &SchemaObject) -> bool {
        matches!(
            object.table_name.as_str(),
            "automation"
                | "automation_meta"
                | "automation_run"
                | "automation_state"
                | "automation_run_transcript"
        ) || object.name == "automation_run_automation_created_idx"
    }

    fn is_v10_catalog_schema_object(object: &SchemaObject) -> bool {
        matches!(
            object.table_name.as_str(),
            "agent_acp_command"
                | "agent_acp_config_memory"
                | "agent_acp_env"
                | "agent_definition"
                | "agent_profile"
                | "agent_ui_pref"
                | "published_session"
                | "remote_acp_session"
                | "remote_terminal_session"
        )
    }

    #[derive(Clone, Copy)]
    enum SchemaGeneration {
        V1,
        V7,
        V7WithoutAcpTable,
        V8WithoutTranscript,
        V9,
        V10,
        V11,
        Current,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct SchemaObject {
        object_type: String,
        name: String,
        table_name: String,
        sql: String,
    }

    fn cached_schema_objects(
        slot: &OnceLock<Vec<SchemaObject>>,
        map: impl FnOnce(Vec<SchemaObject>) -> Vec<SchemaObject>,
    ) -> Result<Vec<SchemaObject>, StoreError> {
        if let Some(objects) = slot.get() {
            return Ok(objects.clone());
        }
        let objects = map(build_expected_schema_objects()?);
        Ok(slot.get_or_init(|| objects.clone()).clone())
    }

    fn expected_schema_objects(
        generation: SchemaGeneration,
    ) -> Result<Vec<SchemaObject>, StoreError> {
        static V1_OBJECTS: OnceLock<Vec<SchemaObject>> = OnceLock::new();
        static V7_OBJECTS: OnceLock<Vec<SchemaObject>> = OnceLock::new();
        static V7_OBJECTS_WITHOUT_ACP: OnceLock<Vec<SchemaObject>> = OnceLock::new();
        static V8_OBJECTS_WITHOUT_TRANSCRIPT: OnceLock<Vec<SchemaObject>> = OnceLock::new();
        static V9_OBJECTS: OnceLock<Vec<SchemaObject>> = OnceLock::new();
        static V10_OBJECTS: OnceLock<Vec<SchemaObject>> = OnceLock::new();
        static V11_OBJECTS: OnceLock<Vec<SchemaObject>> = OnceLock::new();
        static CURRENT_OBJECTS: OnceLock<Vec<SchemaObject>> = OnceLock::new();
        match generation {
            SchemaGeneration::V1 => cached_schema_objects(&V1_OBJECTS, |objects| {
                objects
                    .into_iter()
                    .filter(|object| {
                        object.name != "acp_detail"
                            && matches!(
                                object.table_name.as_str(),
                                "acp_detail"
                                    | "history_title"
                                    | "kv"
                                    | "launch_entry"
                                    | "layout_node"
                                    | "project"
                                    | "session"
                                    | "session_group"
                            )
                    })
                    .collect()
            }),
            SchemaGeneration::V7 => cached_schema_objects(&V7_OBJECTS, |objects| {
                objects
                    .into_iter()
                    .filter(|object| {
                        !is_v9_automation_schema_object(object)
                            && !is_v10_catalog_schema_object(object)
                    })
                    .collect()
            }),
            SchemaGeneration::V7WithoutAcpTable => {
                cached_schema_objects(&V7_OBJECTS_WITHOUT_ACP, |objects| {
                    objects
                        .into_iter()
                        .filter(|object| {
                            object.name != "acp_detail"
                                && !is_v9_automation_schema_object(object)
                                && !is_v10_catalog_schema_object(object)
                        })
                        .collect()
                })
            }
            SchemaGeneration::V8WithoutTranscript => {
                cached_schema_objects(&V8_OBJECTS_WITHOUT_TRANSCRIPT, |objects| {
                    objects
                        .into_iter()
                        .filter(|object| {
                            object.table_name != "automation_run_transcript"
                                && !matches!(
                                    object.table_name.as_str(),
                                    "automation"
                                        | "automation_meta"
                                        | "automation_run"
                                        | "automation_state"
                                )
                                && object.name != "automation_run_automation_created_idx"
                                && !is_v10_catalog_schema_object(object)
                        })
                        .collect()
                })
            }
            SchemaGeneration::V9 => cached_schema_objects(&V9_OBJECTS, |objects| {
                objects
                    .into_iter()
                    .filter(|object| !is_v10_catalog_schema_object(object))
                    .collect()
            }),
            SchemaGeneration::V10 => cached_schema_objects(&V10_OBJECTS, |mut objects| {
                strip_agent_definition_model_columns(&mut objects);
                for object in &mut objects {
                    if matches!(
                        object.table_name.as_str(),
                        "agent_acp_env" | "agent_acp_config_memory" | "agent_definition"
                    ) {
                        object.sql = object.sql.replace("engine_kind_id", "agent_id");
                    }
                }
                objects
            }),
            SchemaGeneration::V11 => cached_schema_objects(&V11_OBJECTS, |mut objects| {
                strip_agent_definition_model_columns(&mut objects);
                objects
            }),
            SchemaGeneration::Current => cached_schema_objects(&CURRENT_OBJECTS, |objects| objects),
        }
    }

    fn build_expected_schema_objects() -> Result<Vec<SchemaObject>, StoreError> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(crate::schema::CREATE_SCHEMA)?;
        schema_objects(&connection)
    }

    fn schema_objects(connection: &Connection) -> Result<Vec<SchemaObject>, StoreError> {
        let mut statement = connection.prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_schema
                 WHERE name NOT LIKE 'sqlite_%'
                   AND type IN ('table', 'index')
                   AND sql IS NOT NULL
                 ORDER BY type, name",
        )?;
        statement
            .query_map([], |row| {
                Ok(SchemaObject {
                    object_type: row.get(0)?,
                    name: row.get(1)?,
                    table_name: row.get(2)?,
                    sql: normalize_schema_sql(&row.get::<_, String>(3)?),
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    fn normalize_schema_sql(sql: &str) -> String {
        sql.chars()
            .filter(|character| !character.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect()
    }

    fn strip_agent_definition_model_columns(objects: &mut [SchemaObject]) {
        const NEEDLE: &str = "model_providertextnotnulldefault'',model_idtextnotnulldefault'',";
        for object in objects {
            if object.name == "agent_definition" {
                object.sql = object.sql.replace(NEEDLE, "");
            }
        }
    }

    fn table_names(connection: &Connection) -> Result<Vec<String>, StoreError> {
        let mut statement = connection.prepare(
            "SELECT name FROM sqlite_schema
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                 ORDER BY name",
        )?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(super) fn table_columns(
        connection: &Connection,
        table: &str,
    ) -> Result<Vec<String>, StoreError> {
        let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(columns)
    }

    fn create_current_schema(transaction: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
        transaction
            .execute_batch(crate::schema::CREATE_SCHEMA)
            .map_err(StoreError::from)
    }

    fn require_regular_private_file(path: &Path) -> Result<(), StoreError> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => set_owner_only(path),
            Ok(_) => Err(StoreError::NotRegularFile {
                path: path.to_path_buf(),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(super::missing_database_error(path))
            }
            Err(error) => Err(error.into()),
        }
    }

    fn ensure_absent_or_create_private_file(path: &Path) -> Result<(), StoreError> {
        // 不预建 0 字节文件。SQLite 自己 CREATE；预建空文件正是事故里空库的来源。
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => set_owner_only(path),
            Ok(_) => Err(StoreError::NotRegularFile {
                path: path.to_path_buf(),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn secure_sidecar_permissions(path: &Path) {
        let _ = set_owner_only(path);
        let raw = path.as_os_str().to_string_lossy();
        for suffix in ["-wal", "-shm"] {
            let sidecar = std::path::PathBuf::from(format!("{raw}{suffix}"));
            if sidecar.exists() {
                let _ = set_owner_only(&sidecar);
            }
        }
    }

    #[cfg(unix)]
    fn set_owner_only(path: &Path) -> Result<(), StoreError> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(StoreError::from)
    }

    #[cfg(not(unix))]
    fn set_owner_only(_path: &Path) -> Result<(), StoreError> {
        Ok(())
    }
}

#[cfg(any(target_os = "ios", target_os = "android"))]
mod imp {
    use super::{HashSet, LegacyState, StoreError};
    use std::path::Path;

    #[derive(Clone)]
    pub(super) struct Backend;

    impl Backend {
        pub(super) fn open(_path: &Path) -> Result<Self, StoreError> {
            unsupported()
        }

        pub(super) fn create(_path: &Path) -> Result<Self, StoreError> {
            unsupported()
        }

        pub(super) fn open_or_create(_path: &Path) -> Result<Self, StoreError> {
            unsupported()
        }
    }

    pub(super) fn read_user_version(_path: &Path) -> Option<u32> {
        None
    }

    fn unsupported<T>() -> Result<T, StoreError> {
        Err(StoreError::SqliteDisabledOnMobile)
    }

    pub(super) fn store_transaction<R>(
        _backend: &Backend,
        _operation: impl FnOnce(&mut super::StoreTransaction<'_>) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        unsupported()
    }
    pub(super) fn transaction_put(
        _transaction: &mut super::StoreTransaction<'_>,
        _namespace: &str,
        _key: &str,
        _value: &[u8],
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn transaction_delete(
        _transaction: &mut super::StoreTransaction<'_>,
        _namespace: &str,
        _key: &str,
    ) -> Result<bool, StoreError> {
        unsupported()
    }
    pub(super) fn transaction_append_outbox(
        _transaction: &mut super::StoreTransaction<'_>,
        _events: &[smelt_plugin_api::EventEnvelope<serde_json::Value>],
    ) -> Result<(), StoreError> {
        unsupported()
    }

    pub(super) fn get(
        _backend: &Backend,
        _namespace: &str,
        _key: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        unsupported()
    }
    pub(super) fn put(
        _backend: &Backend,
        _namespace: &str,
        _key: &str,
        _value: &[u8],
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn get_blob(
        _backend: &Backend,
        _namespace: &str,
        _key: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        unsupported()
    }
    pub(super) fn put_blob(
        _backend: &Backend,
        _namespace: &str,
        _key: &str,
        _value: &[u8],
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn blob_keys(
        _backend: &Backend,
        _namespace: &str,
    ) -> Result<Vec<String>, StoreError> {
        unsupported()
    }
    pub(super) fn delete_blob(
        _backend: &Backend,
        _namespace: &str,
        _key: &str,
    ) -> Result<bool, StoreError> {
        unsupported()
    }
    pub(super) fn put_automation_run_transcript(
        _backend: &Backend,
        _run_id: &str,
        _entries_json: &[u8],
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn get_automation_run_transcript(
        _backend: &Backend,
        _run_id: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        unsupported()
    }
    pub(super) fn get_automation_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::AutomationSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_automation_snapshot(
        _backend: &Backend,
        _snapshot: &super::AutomationSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn get_launch_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::LaunchSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_launch_snapshot(
        _backend: &Backend,
        _snapshot: &super::LaunchSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn get_history_title_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::HistoryTitleSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_history_title_snapshot(
        _backend: &Backend,
        _snapshot: &super::HistoryTitleSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn get_appearance_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::AppearanceSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_appearance_snapshot(
        _backend: &Backend,
        _snapshot: &super::AppearanceSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn get_workspace_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::WorkspaceSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_workspace_snapshot(
        _backend: &Backend,
        _snapshot: &super::WorkspaceSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn workspace_snapshot_from_json(
        _raw: &[u8],
    ) -> Result<super::WorkspaceSnapshot, StoreError> {
        unsupported()
    }
    pub(super) fn workspace_snapshot_to_json(
        _snapshot: &super::WorkspaceSnapshot,
    ) -> Result<Vec<u8>, StoreError> {
        unsupported()
    }
    pub(super) fn get_agent_ui_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::AgentUiSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_agent_ui_snapshot(
        _backend: &Backend,
        _snapshot: &super::AgentUiSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn put_agent_ui_prefs(
        _backend: &Backend,
        _snapshot: &super::AgentUiSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn insert_agent_definition(
        _backend: &Backend,
        _record: &super::AgentDefinitionRecord,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn update_agent_definition(
        _backend: &Backend,
        _record: &super::AgentDefinitionRecord,
    ) -> Result<bool, StoreError> {
        unsupported()
    }
    pub(super) fn delete_agent_definition(
        _backend: &Backend,
        _id: &str,
    ) -> Result<bool, StoreError> {
        unsupported()
    }
    pub(super) fn get_remote_session_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::RemoteSessionCatalogSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_remote_session_snapshot(
        _backend: &Backend,
        _snapshot: &super::RemoteSessionCatalogSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn get_published_session_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::PublishedSessionSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_published_session_snapshot(
        _backend: &Backend,
        _snapshot: &super::PublishedSessionSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn agent_ui_snapshot_from_json(
        _value: &serde_json::Value,
    ) -> Result<super::AgentUiSnapshot, StoreError> {
        unsupported()
    }
    pub(super) fn get_remote_config_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::RemoteConfigSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_remote_config_snapshot(
        _backend: &Backend,
        _snapshot: &super::RemoteConfigSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn get_update_settings_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::UpdateSettingsSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_update_settings_snapshot(
        _backend: &Backend,
        _snapshot: &super::UpdateSettingsSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn get_update_state_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::UpdateStateSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_update_state_snapshot(
        _backend: &Backend,
        _snapshot: &super::UpdateStateSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn get_worktree_inherit_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::WorktreeInheritSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_worktree_inherit_snapshot(
        _backend: &Backend,
        _snapshot: &super::WorktreeInheritSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn get_terminal_theme_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::TerminalThemeSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_terminal_theme_snapshot(
        _backend: &Backend,
        _snapshot: &super::TerminalThemeSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn get_quota_cache_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::QuotaCacheSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_quota_cache_snapshot(
        _backend: &Backend,
        _snapshot: &super::QuotaCacheSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn get_dsh_auto_model_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::AutoModelSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_dsh_auto_model_snapshot(
        _backend: &Backend,
        _snapshot: &super::AutoModelSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn get_pi_auto_model_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::AutoModelSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_pi_auto_model_snapshot(
        _backend: &Backend,
        _snapshot: &super::AutoModelSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn get_workspace_menu_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::PublishedWorkspaceMenuSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn put_workspace_menu_snapshot(
        _backend: &Backend,
        _snapshot: &super::PublishedWorkspaceMenuSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn retain_automation_run_transcripts(
        _backend: &Backend,
        _keep: &HashSet<String>,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn delete(
        _backend: &Backend,
        _namespace: &str,
        _key: &str,
    ) -> Result<bool, StoreError> {
        unsupported()
    }
    pub(super) fn keys(_backend: &Backend, _namespace: &str) -> Result<Vec<String>, StoreError> {
        unsupported()
    }
    pub(super) fn legacy_state(
        _backend: &Backend,
        _source: &str,
    ) -> Result<LegacyState, StoreError> {
        unsupported()
    }
    pub(super) fn get_with_legacy_state(
        _backend: &Backend,
        _namespace: &str,
        _key: &str,
        _source: &str,
    ) -> Result<(Option<Vec<u8>>, LegacyState), StoreError> {
        unsupported()
    }
    pub(super) fn delete_with_legacy_tombstone(
        _backend: &Backend,
        _source: &str,
        _namespace: &str,
        _key: &str,
    ) -> Result<bool, StoreError> {
        unsupported()
    }
    pub(super) fn mark_legacy_deleted(
        _backend: &Backend,
        _source: &str,
        _namespace: &str,
        _key: &str,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn quarantine_with_legacy_tombstone(
        _backend: &Backend,
        _source: &str,
        _namespace: &str,
        _key: &str,
        _quarantine_namespace: &str,
        _quarantine_key: &str,
    ) -> Result<bool, StoreError> {
        unsupported()
    }

    pub(super) fn append_event_batch(
        _backend: &Backend,
        _events: &[smelt_event_bus::StoredEvent],
    ) -> Result<Vec<smelt_event_bus::StoredEvent>, StoreError> {
        unsupported()
    }
    pub(super) fn append_event_batch_idempotent(
        _backend: &Backend,
        _events: &[smelt_event_bus::StoredEvent],
    ) -> Result<Vec<smelt_event_bus::StoredEvent>, StoreError> {
        unsupported()
    }
    pub(super) fn load_events_after_topics(
        _backend: &Backend,
        _sequence: u64,
        _topics: &std::collections::BTreeSet<smelt_plugin_api::Topic>,
        _limit: usize,
    ) -> Result<Vec<smelt_event_bus::StoredEvent>, StoreError> {
        unsupported()
    }
    pub(super) fn event_high_watermark(_backend: &Backend) -> Result<u64, StoreError> {
        unsupported()
    }
    pub(super) fn append_outbox_batch(
        _backend: &Backend,
        _events: &[smelt_plugin_api::EventEnvelope<serde_json::Value>],
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn load_pending_outbox(
        _backend: &Backend,
        _limit: usize,
    ) -> Result<Vec<super::OutboxEvent>, StoreError> {
        unsupported()
    }
    pub(super) fn mark_outbox_dispatched(
        _backend: &Backend,
        _dispatched: &[(u64, u64)],
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn dispatch_pending_outbox(
        _backend: &Backend,
        _limit: usize,
    ) -> Result<Vec<smelt_event_bus::StoredEvent>, StoreError> {
        unsupported()
    }
    pub(super) fn get_peer_message_command_result(
        _backend: &Backend,
        _source_session_id: &str,
        _command_id: &smelt_plugin_api::CommandId,
    ) -> Result<Option<super::PeerMessageCommandResult>, StoreError> {
        unsupported()
    }
    pub(super) fn commit_peer_message_delivery(
        _backend: &Backend,
        _result: &super::PeerMessageCommandResult,
        _event: &smelt_plugin_api::EventEnvelope<serde_json::Value>,
    ) -> Result<super::PeerMessageCommitOutcome, StoreError> {
        unsupported()
    }
    pub(super) fn bind_cursor_declaration(
        _backend: &Backend,
        _plugin_id: &smelt_plugin_api::PluginId,
        _subscription_id: &smelt_plugin_api::SubscriptionId,
        _declaration_fingerprint: &str,
    ) -> Result<super::CursorDeclaration, StoreError> {
        unsupported()
    }
    pub(super) fn load_cursor_declaration(
        _backend: &Backend,
        _plugin_id: &smelt_plugin_api::PluginId,
        _subscription_id: &smelt_plugin_api::SubscriptionId,
    ) -> Result<Option<super::CursorDeclaration>, StoreError> {
        unsupported()
    }
    pub(super) fn store_cursors(
        _backend: &Backend,
        _cursors: &[(
            smelt_plugin_api::PluginId,
            smelt_plugin_api::SubscriptionId,
            u64,
        )],
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn put_dead_letter(
        _backend: &Backend,
        _dead: &smelt_event_bus::DeadLetter,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn load_dead_letters(
        _backend: &Backend,
        _plugin_id: &smelt_plugin_api::PluginId,
        _subscription_id: &smelt_plugin_api::SubscriptionId,
        _limit: usize,
    ) -> Result<Vec<smelt_event_bus::DeadLetter>, StoreError> {
        unsupported()
    }
    pub(super) fn load_retry(
        _backend: &Backend,
        _plugin_id: &smelt_plugin_api::PluginId,
        _subscription_id: &smelt_plugin_api::SubscriptionId,
    ) -> Result<Option<smelt_event_bus::DeliveryRetry>, StoreError> {
        unsupported()
    }
    pub(super) fn put_retry(
        _backend: &Backend,
        _retry: &smelt_event_bus::DeliveryRetry,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn delete_retry(
        _backend: &Backend,
        _plugin_id: &smelt_plugin_api::PluginId,
        _subscription_id: &smelt_plugin_api::SubscriptionId,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn get_command_result(
        _backend: &Backend,
        _plugin_id: &smelt_plugin_api::PluginId,
        _command_id: &smelt_plugin_api::CommandId,
    ) -> Result<Option<smelt_event_bus::CommandResult>, StoreError> {
        unsupported()
    }
    pub(super) fn put_command_result(
        _backend: &Backend,
        _result: &smelt_event_bus::CommandResult,
    ) -> Result<bool, StoreError> {
        unsupported()
    }
}

#[cfg(all(test, not(any(target_os = "ios", target_os = "android"))))]
mod tests {
    use super::*;
    use serde_json::json;
    use smelt_event_bus::EventStore;
    use smelt_plugin_api::{
        AggregateRef, CommandId, CorrelationId, EventEnvelope, EventId, EventSource, PluginId,
        SubscriptionId, Topic,
    };
    use std::sync::{Arc, Barrier};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/smelt-store-tests")
            .join(format!("{name}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn schema_version_reads_without_creating_or_migrating() {
        // 不在的文件：None，且不许建库。
        let dir = temp_dir("schema-version-missing");
        let missing = dir.join(DATABASE_FILE_NAME);
        assert_eq!(Store::schema_version(&missing), None);
        assert!(!missing.exists());

        // 建库后读到当前版本；读操作不改变 user_version。
        let path = dir.join("v.sqlite3");
        let before = Store::open_or_create(&path).unwrap();
        drop(before);
        assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
        assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn v1_schema() -> &'static str {
        static SCHEMA: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        SCHEMA
            .get_or_init(|| {
                let base = crate::schema::CREATE_SCHEMA
                    .split("CREATE TABLE event_log")
                    .next()
                    .unwrap();
                // v1..v5 的 acp_detail 带 Multica 专用列，夹具必须复刻真实旧库，
                // 否则迁移路径等于没被测到。
                // v7 才引入的智能体归属列同理要先摘掉。
                let base = base.replace(
                    "  pending_delivery_id TEXT,\n  agent_definition_id TEXT,\n  automation_id TEXT,\n",
                    "  pending_delivery_id TEXT,\n",
                );
                assert!(
                    !base.contains("agent_definition_id"),
                    "acp_detail fixture patch missed the v7 columns"
                );
                let with_columns = base.replace(
                    "  fork_from_history INTEGER NOT NULL DEFAULT 0 CHECK (fork_from_history IN (0, 1)),\n  pending_prompt TEXT,",
                    "  fork_from_history INTEGER NOT NULL DEFAULT 0 CHECK (fork_from_history IN (0, 1)),\n\
                     \x20 multica_issue_id TEXT,\n\
                     \x20 multica_workspace_id TEXT,\n\
                     \x20 multica_issue_title TEXT,\n\
                     \x20 multica_task_id TEXT,\n\
                     \x20 multica_parent_comment_id TEXT,\n\
                     \x20 multica_trigger_comment TEXT,\n\
                     \x20 pending_prompt TEXT,",
                );
                assert_ne!(with_columns, base, "acp_detail fixture patch missed columns");
                let patched = with_columns.replace(
                    "    OR (fork_session_id IS NOT NULL AND fork_title IS NOT NULL)\n  )\n);",
                    "    OR (fork_session_id IS NOT NULL AND fork_title IS NOT NULL)\n  ),\n\
                     \x20 CHECK (\n\
                     \x20   (multica_issue_id IS NULL AND multica_workspace_id IS NULL)\n\
                     \x20   OR (multica_issue_id IS NOT NULL AND multica_workspace_id IS NOT NULL)\n\
                     \x20 )\n);",
                );
                assert_ne!(patched, with_columns, "acp_detail fixture patch missed check");
                patched
            })
            .as_str()
    }

    fn v2_schema() -> String {
        format!("{}\n{}", v1_schema(), crate::schema::MIGRATE_V1_TO_V2)
    }

    fn v3_schema() -> String {
        format!(
            "{}\n{}\n{}",
            v1_schema(),
            crate::schema::MIGRATE_V1_TO_V2,
            crate::schema::MIGRATE_V2_TO_V3
        )
    }

    fn v4_schema() -> String {
        format!("{}\n{}", v3_schema(), crate::schema::MIGRATE_V3_TO_V4)
    }

    fn v5_schema() -> String {
        format!("{}\n{}", v4_schema(), crate::schema::MIGRATE_V4_TO_V5)
    }

    fn v6_schema() -> String {
        format!("{}\n{}", v5_schema(), crate::schema::MIGRATE_V5_TO_V6)
    }

    fn v7_schema() -> String {
        format!("{}\n{}", v6_schema(), crate::schema::MIGRATE_V6_TO_V7)
    }

    fn v9_schema() -> String {
        let schema = crate::schema::CREATE_SCHEMA;
        let cut = schema
            .find("CREATE TABLE agent_ui_pref")
            .expect("v10 catalog tables follow automation tables in CREATE_SCHEMA");
        schema[..cut].to_string()
    }

    fn v10_schema() -> String {
        format!("{}\n{}", v9_schema(), crate::schema::MIGRATE_V9_TO_V10)
    }

    #[test]
    fn kv_round_trip_scan_and_delete() {
        let dir = temp_dir("kv");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        assert_eq!(store.get("json", "appearance.json").unwrap(), None);
        store
            .put("json", "appearance.json", br#"{"dark":true}"#)
            .unwrap();
        store
            .put("json", "appearance.json", br#"{"dark":false}"#)
            .unwrap();
        assert_eq!(
            store.get("json", "appearance.json").unwrap().unwrap(),
            br#"{"dark":false}"#
        );
        assert_eq!(store.keys("json").unwrap(), vec!["appearance.json"]);
        assert!(store.delete("json", "appearance.json").unwrap());
        assert!(!store.delete("json", "appearance.json").unwrap());
        assert!(store.get("json", "appearance.json").unwrap().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn scoped_blob_is_independent_from_json_documents_and_other_scopes() {
        let dir = temp_dir("scoped-blob");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();

        store
            .put_blob("preferences", "new_session_pins", b"{\"pinned\":[]}")
            .unwrap();
        store
            .put_blob("other", "new_session_pins", b"other-value")
            .unwrap();

        assert_eq!(
            store.get_blob("preferences", "new_session_pins").unwrap(),
            Some(br#"{"pinned":[]}"#.to_vec())
        );
        assert_eq!(
            store.get_blob("other", "new_session_pins").unwrap(),
            Some(b"other-value".to_vec())
        );
        let mut keys = store.blob_keys("preferences").unwrap();
        keys.sort();
        assert_eq!(keys, ["new_session_pins"]);
        assert!(
            store
                .delete_blob("preferences", "new_session_pins")
                .unwrap()
        );
        assert!(
            store
                .get_blob("preferences", "new_session_pins")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_blob("other", "new_session_pins")
                .unwrap()
                .is_some()
        );
        assert_eq!(
            store.get("json", "new_session_pins").unwrap(),
            None,
            "作用域 blob 不应通过 JSON 文档入口可见"
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn new_database_contains_exactly_the_eight_state_tables() {
        let dir = temp_dir("minimal-schema");
        let path = dir.join(DATABASE_FILE_NAME);
        drop(Store::open_or_create(&path).unwrap());

        let connection = rusqlite::Connection::open(&path).unwrap();
        let tables = connection
            .prepare(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(tables, crate::schema::TABLES);
        let kv_columns = connection
            .prepare("PRAGMA table_info(kv)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            kv_columns,
            ["scope", "key", "value", "value_type", "updated_at_ms"]
        );

        drop(connection);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v2_database_migrates_peer_message_command_table() {
        let dir = temp_dir("v2-migration");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(&v2_schema()).unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        drop(connection);

        drop(Store::open_or_create(&path).unwrap());
        let backup = path.with_file_name(format!(
            "{}.bak-v2",
            path.file_name().and_then(|name| name.to_str()).unwrap()
        ));
        assert!(backup.is_file(), "升级 schema 前必须留下一份可恢复的备份");
        let backup_version: u32 = rusqlite::Connection::open(&backup)
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(backup_version, 2);
        let connection = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            STORE_SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema
                     WHERE type = 'table' AND name = 'peer_message_command'",
                    [],
                    |row| row.get::<_, u32>(0),
                )
                .unwrap(),
            1
        );
        drop(connection);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v3_database_resets_cursor_when_legacy_declaration_is_first_bound() {
        let dir = temp_dir("v3-migration");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(&v3_schema()).unwrap();
        connection
            .execute(
                "INSERT INTO subscription_cursor(
                   plugin_id, subscription_id, last_acked_sequence, updated_at_ms
                 ) VALUES ('com.example', 'tasks', 17, 10)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO peer_message_command(
                   source_session_id, command_id, completed_at_ms, message_id, target_session_id
                 ) VALUES ('source', 'command-1', 10, 'message-1', 'target')",
                [],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 3).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        let plugin = PluginId::new("com.example").unwrap();
        let subscription = SubscriptionId::new("tasks").unwrap();
        assert_eq!(
            store
                .load_cursor_declaration(&plugin, &subscription)
                .unwrap(),
            Some(CursorDeclaration {
                last_acked_sequence: 17,
                declaration_fingerprint: "legacy-v3-unbound".to_string(),
            })
        );
        assert_eq!(
            store
                .bind_cursor_declaration(&plugin, &subscription, "sha256:new-declaration")
                .unwrap(),
            CursorDeclaration {
                last_acked_sequence: 0,
                declaration_fingerprint: "sha256:new-declaration".to_string(),
            }
        );
        assert_eq!(
            store
                .load_cursor_declaration(&plugin, &subscription)
                .unwrap(),
            Some(CursorDeclaration {
                last_acked_sequence: 0,
                declaration_fingerprint: "sha256:new-declaration".to_string(),
            })
        );
        let command = store
            .get_peer_message_command_result(
                "source",
                &smelt_plugin_api::CommandId::new("command-1").unwrap(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(command.request_fingerprint, "legacy-v3-unbound");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v4_database_adds_durable_retry_state() {
        let dir = temp_dir("v4-migration");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(&v4_schema()).unwrap();
        connection.pragma_update(None, "user_version", 4).unwrap();
        drop(connection);

        drop(Store::open_or_create(&path).unwrap());
        let connection = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            STORE_SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema
                     WHERE type = 'table' AND name = 'event_delivery_retry'",
                    [],
                    |row| row.get::<_, u32>(0),
                )
                .unwrap(),
            1
        );
        drop(connection);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn event(id: &str) -> EventEnvelope<serde_json::Value> {
        EventEnvelope {
            event_id: EventId::new(id).unwrap(),
            topic: Topic::new("example.changed").unwrap(),
            schema_version: 1,
            occurred_at_ms: 10,
            aggregate: None,
            aggregate_revision: None,
            source: EventSource::Core,
            correlation_id: CorrelationId::new("chain-1").unwrap(),
            causation_id: None,
            hop_count: 0,
            payload: json!({"id": id}),
        }
    }

    fn versioned_event(id: &str, revision: u64) -> EventEnvelope<serde_json::Value> {
        let mut event = event(id);
        event.aggregate = Some(AggregateRef::new("example", "item-1").unwrap());
        event.aggregate_revision = Some(revision);
        event
    }

    #[test]
    fn peer_message_result_and_outbox_fact_commit_atomically_and_dedupe() {
        let dir = temp_dir("peer-message-command");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        let result = PeerMessageCommandResult {
            source_session_id: "source".to_string(),
            command_id: CommandId::new("agent-send-1").unwrap(),
            request_fingerprint: "sha256:request-1".to_string(),
            completed_at_ms: 10,
            message_id: "agent-send-1".to_string(),
            target_session_id: "target".to_string(),
        };
        assert_eq!(
            store
                .commit_peer_message_delivery(&result, &event("agent-send-event-1"))
                .unwrap(),
            PeerMessageCommitOutcome::Inserted
        );
        assert_eq!(
            store
                .get_peer_message_command_result("source", &result.command_id)
                .unwrap(),
            Some(result.clone())
        );
        assert_eq!(store.load_pending_outbox(10).unwrap().len(), 1);
        assert_eq!(
            store
                .commit_peer_message_delivery(&result, &event("agent-send-event-2"))
                .unwrap(),
            PeerMessageCommitOutcome::Existing(result.clone())
        );
        assert_eq!(
            store.load_pending_outbox(10).unwrap().len(),
            1,
            "duplicate command must not enqueue another delivered fact"
        );
        let changed = PeerMessageCommandResult {
            request_fingerprint: "sha256:different-request".to_string(),
            ..result.clone()
        };
        assert_eq!(
            store
                .commit_peer_message_delivery(&changed, &event("agent-send-event-3"))
                .unwrap(),
            PeerMessageCommitOutcome::Conflict(result)
        );
        assert_eq!(store.load_pending_outbox(10).unwrap().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn event_store_batches_cursors_dead_letters_and_commands() {
        let dir = temp_dir("event-store");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        let first = smelt_event_bus::StoredEvent::new(0, event("event-1")).unwrap();
        let second = smelt_event_bus::StoredEvent::new(0, event("event-2")).unwrap();
        let appended = store.append_batch(&[first, second]).unwrap();
        assert_eq!(
            appended
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(
            store
                .load_after_topics(
                    0,
                    &std::collections::BTreeSet::from([Topic::new("example.changed").unwrap()]),
                    10,
                )
                .unwrap(),
            appended
        );
        assert_eq!(store.high_watermark().unwrap(), 2);

        let plugin = PluginId::new("com.example").unwrap();
        let subscription = SubscriptionId::new("tasks").unwrap();
        store
            .bind_cursor_declaration(&plugin, &subscription, "sha256:tasks-durable")
            .unwrap();
        store
            .store_cursors(&[(plugin.clone(), subscription.clone(), 1)])
            .unwrap();
        store
            .store_cursors(&[(plugin.clone(), subscription.clone(), 0)])
            .unwrap();
        assert_eq!(store.load_cursor(&plugin, &subscription).unwrap(), 1);

        let retry = smelt_event_bus::DeliveryRetry {
            plugin_id: plugin.clone(),
            subscription_id: subscription.clone(),
            sequence: 2,
            event_id: EventId::new("event-2").unwrap(),
            attempts: 2,
            next_attempt_at_ms: 25,
            reason: "temporary".into(),
        };
        store.put_retry(&retry).unwrap();
        assert_eq!(
            store.load_retry(&plugin, &subscription).unwrap(),
            Some(retry.clone())
        );
        store.delete_retry(&plugin, &subscription).unwrap();
        assert_eq!(store.load_retry(&plugin, &subscription).unwrap(), None);
        store.put_retry(&retry).unwrap();

        let dead = smelt_event_bus::DeadLetter {
            plugin_id: plugin.clone(),
            subscription_id: subscription.clone(),
            sequence: 2,
            event_id: EventId::new("event-2").unwrap(),
            attempts: 3,
            reason: "permanent".into(),
            failed_at_ms: 20,
        };
        store.put_dead_letter(&dead).unwrap();
        assert_eq!(
            store.load_dead_letters(&plugin, &subscription, 10).unwrap(),
            [dead]
        );
        assert_eq!(store.load_cursor(&plugin, &subscription).unwrap(), 2);
        assert_eq!(store.load_retry(&plugin, &subscription).unwrap(), None);

        let result = smelt_event_bus::CommandResult {
            plugin_id: plugin.clone(),
            command_id: smelt_plugin_api::CommandId::new("command-1").unwrap(),
            completed_at_ms: 30,
            result: json!({"created": true}),
        };
        assert!(store.put_command_result(&result).unwrap());
        assert!(!store.put_command_result(&result).unwrap());
        assert_eq!(
            store
                .get_command_result(&plugin, &result.command_id)
                .unwrap(),
            Some(result)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_dead_letter_commit_leaves_no_partial_row() {
        let dir = temp_dir("dead-letter-rollback");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        store
            .append_batch(&[smelt_event_bus::StoredEvent::new(0, event("event-1")).unwrap()])
            .unwrap();
        let plugin = PluginId::new("com.example.unbound").unwrap();
        let subscription = SubscriptionId::new("tasks").unwrap();
        let dead = smelt_event_bus::DeadLetter {
            plugin_id: plugin.clone(),
            subscription_id: subscription.clone(),
            sequence: 1,
            event_id: EventId::new("event-1").unwrap(),
            attempts: 1,
            reason: "permanent".into(),
            failed_at_ms: 20,
        };

        assert!(store.put_dead_letter(&dead).is_err());
        assert!(
            store
                .load_dead_letters(&plugin, &subscription, 10)
                .unwrap()
                .is_empty()
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn outbox_transfer_is_atomic_and_idempotent_by_event_id() {
        let dir = temp_dir("outbox");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        let existing = smelt_event_bus::StoredEvent::new(0, event("outbox-1")).unwrap();
        let existing = store.append_batch(&[existing]).unwrap().remove(0);
        store
            .append_outbox_batch(&[event("outbox-1"), event("outbox-2")])
            .unwrap();
        let dispatched = store.dispatch_pending_outbox(10).unwrap();
        assert_eq!(dispatched.len(), 2);
        assert_eq!(dispatched[0].sequence, existing.sequence);
        assert!(store.load_pending_outbox(10).unwrap().is_empty());
        assert!(store.dispatch_pending_outbox(10).unwrap().is_empty());
        assert_eq!(store.high_watermark().unwrap(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn direct_append_drains_all_older_outbox_events_before_assigning_sequences() {
        let dir = temp_dir("outbox-before-direct");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        store
            .append_outbox_batch(&[versioned_event("revision-1", 1)])
            .unwrap();

        let direct = store
            .append_batch(&[
                smelt_event_bus::StoredEvent::new(0, versioned_event("revision-2", 2)).unwrap(),
            ])
            .unwrap();

        assert_eq!(direct.len(), 1, "only direct events are returned");
        assert_eq!(direct[0].sequence, 2);
        let loaded = store
            .load_after_topics(
                0,
                &std::collections::BTreeSet::from([Topic::new("example.changed").unwrap()]),
                10,
            )
            .unwrap();
        assert_eq!(
            loaded
                .iter()
                .map(|event| event.envelope.event_id.as_str())
                .collect::<Vec<_>>(),
            ["revision-1", "revision-2"]
        );
        assert!(store.load_pending_outbox(10).unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn store_transaction_commits_domain_write_and_outbox_together() {
        let dir = temp_dir("domain-outbox-transaction");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        let failed: Result<(), StoreError> = store.transaction(|transaction| {
            transaction.put("json", "test.json", br#"{"value":1}"#)?;
            transaction.append_outbox_batch(&[event("rolled-back")])?;
            Err(StoreError::TransactionAborted)
        });
        assert!(failed.is_err());
        assert_eq!(store.get("json", "test.json").unwrap(), None);
        assert!(store.load_pending_outbox(10).unwrap().is_empty());

        store
            .transaction(|transaction| {
                transaction.put("json", "test.json", br#"{"value":2}"#)?;
                transaction.append_outbox_batch(&[event("committed")])
            })
            .unwrap();
        assert_eq!(
            store.get("json", "test.json").unwrap(),
            Some(br#"{"value":2}"#.to_vec())
        );
        assert_eq!(store.load_pending_outbox(10).unwrap().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn durable_revision_survives_restart_and_serializes_publishers() {
        let dir = temp_dir("durable-revision");
        let path = dir.join(DATABASE_FILE_NAME);
        let store = Store::open_or_create(&path).unwrap();
        store
            .append_batch(&[
                smelt_event_bus::StoredEvent::new(0, versioned_event("revision-2", 2)).unwrap(),
            ])
            .unwrap();
        drop(store);
        let store = Store::open_or_create(&path).unwrap();
        assert!(
            store
                .append_batch(&[smelt_event_bus::StoredEvent::new(
                    0,
                    versioned_event("revision-1", 1),
                )
                .unwrap()])
                .is_err()
        );
        drop(store);

        let path = Arc::new(path);
        let barrier = Arc::new(Barrier::new(3));
        let threads = ["same-revision-a", "same-revision-b"]
            .into_iter()
            .map(|id| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let store = Store::open_or_create(path.as_ref()).unwrap();
                    barrier.wait();
                    store.append_batch(&[smelt_event_bus::StoredEvent::new(
                        0,
                        versioned_event(id, 3),
                    )
                    .unwrap()])
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let successes = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(Result::is_ok)
            .count();
        assert_eq!(successes, 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn topic_filtered_load_skips_unrelated_first_page() {
        let dir = temp_dir("topic-filtered-load");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        let mut events = (0..130)
            .map(|index| {
                let mut event = event(&format!("other-{index}"));
                event.topic = Topic::new("other.changed").unwrap();
                smelt_event_bus::StoredEvent::new(0, event).unwrap()
            })
            .collect::<Vec<_>>();
        events.push(smelt_event_bus::StoredEvent::new(0, event("wanted")).unwrap());
        store.append_batch(&events).unwrap();
        let loaded = store
            .load_after_topics(
                0,
                &std::collections::BTreeSet::from([Topic::new("example.changed").unwrap()]),
                1,
            )
            .unwrap();
        assert_eq!(loaded[0].envelope.event_id.as_str(), "wanted");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn migrates_released_v5_schema_dropping_multica_columns() {
        let dir = temp_dir("migrate-v5-release");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(&v5_schema()).unwrap();
        connection
            .execute_batch(
                "INSERT INTO session_group(id, position) VALUES ('g1', 0);
                 INSERT INTO session(id, group_id, kind) VALUES ('s1', 'g1', 'acp');
                 INSERT INTO acp_detail(
                   session_id, agent, launch_command, multica_issue_id, multica_workspace_id
                 ) VALUES ('s1', 'codex', 'codex acp', 'issue-1', 'ws-1');
                 INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                 VALUES ('document:test.json', '$', X'6B656570', 'blob', 1);",
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 5).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        assert_eq!(
            store.get("json", "test.json").unwrap(),
            Some(b"keep".to_vec())
        );
        drop(store);

        let connection = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            STORE_SCHEMA_VERSION
        );
        let columns = connection
            .prepare("PRAGMA table_info(acp_detail)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(!columns.iter().any(|column| column.starts_with("multica_")));
        assert_eq!(
            connection
                .query_row(
                    "SELECT launch_command FROM acp_detail WHERE session_id = 's1'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "codex acp"
        );
        drop(connection);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn migrates_recognized_v1_schema_without_losing_rows() {
        let dir = temp_dir("migrate-v1");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(v1_schema()).unwrap();
        connection
            .execute(
                "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                 VALUES ('document:test.json', '$', X'6B656570', 'blob', 1)",
                [],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        assert_eq!(
            store.get("json", "test.json").unwrap(),
            Some(b"keep".to_vec())
        );
        assert_eq!(store.high_watermark().unwrap(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn migrates_released_v6_schema_adding_agent_ownership_columns() {
        let dir = temp_dir("migrate-v6-release");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(&v6_schema()).unwrap();
        connection
            .execute_batch(
                "INSERT INTO session_group(id, position) VALUES ('g1', 0);
                 INSERT INTO session(id, group_id, kind) VALUES ('s1', 'g1', 'acp');
                 INSERT INTO acp_detail(session_id, agent, launch_command)
                 VALUES ('s1', 'pi', 'pi acp');",
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 6).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        drop(store);

        let connection = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            STORE_SCHEMA_VERSION
        );
        // 旧行必须原样保留，新列以 NULL 补齐。
        let row: (String, Option<String>, Option<String>) = connection
            .query_row(
                "SELECT agent, agent_definition_id, automation_id FROM acp_detail
                 WHERE session_id = 's1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, ("pi".to_string(), None, None));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn migrates_v7_schema_and_moves_kv_transcripts_into_the_table() {
        let dir = temp_dir("migrate-v7-transcript");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(&v7_schema()).unwrap();
        let snapshot = serde_json::to_vec(&json!({
            "schema_version": 1,
            "store_id": "s",
            "revision": 1,
            "timezone_fingerprint": "UTC",
            "automations": [{
                "id": "auto-1",
                "name": "A",
                "enabled": true,
                "trigger": {},
                "action": {"type": "shell", "command": "true", "args": []},
                "sinks": []
            }],
            "states": [],
            "runs": [{
                "id": "run-1",
                "automation_id": "auto-1",
                "source": "manual",
                "status": "completed",
                "created_at": 1,
                "delivery_attempts": 0,
                "context": {
                    "automation_name": "A",
                    "cwd": "/",
                    "permission_mode": "full_access",
                    "action": {"type": "shell", "command": "true", "args": []}
                }
            }]
        }))
        .unwrap();
        connection
            .execute(
                "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                 VALUES ('document:automations.json', '$', ?1, 'blob', 1)",
                rusqlite::params![snapshot],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                 VALUES ('automation_run_transcript', 'run-1', X'5B5D', 'blob', 9)",
                [],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 7).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        assert_eq!(
            store.get_automation_run_transcript("run-1").unwrap(),
            Some(b"[]".to_vec())
        );
        drop(store);

        let connection = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            STORE_SCHEMA_VERSION
        );
        let leftover: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM kv WHERE scope = 'automation_run_transcript'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(leftover, 0);
        drop(connection);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn json_bytes(value: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&value).unwrap()
    }

    fn shell_action_json() -> Vec<u8> {
        json_bytes(json!({"type": "shell", "command": "true", "args": []}))
    }

    fn run_record(
        id: &str,
        source: &str,
        status: &str,
        created_at: i64,
        delivery_attempts: i64,
        name: &str,
    ) -> AutomationRunRecord {
        AutomationRunRecord {
            id: id.into(),
            automation_id: "auto-1".into(),
            source: source.into(),
            status: status.into(),
            created_at,
            scheduled_for: None,
            started_at: None,
            delivery_attempt_at: None,
            delivery_attempts,
            finished_at: None,
            session_id: None,
            provider_session_id: None,
            output: None,
            error: None,
            runtime_released_at: None,
            context_json: json_bytes(json!({
                "automation_name": name,
                "cwd": "/",
                "permission_mode": "full_access",
                "action": {"type": "shell", "command": "true", "args": []}
            })),
        }
    }

    #[test]
    fn automation_run_transcripts_round_trip_and_retain() {
        let dir = temp_dir("automation-transcript");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        store
            .put_automation_snapshot(&AutomationSnapshot {
                schema_version: 1,
                store_id: "s".into(),
                revision: 1,
                timezone_fingerprint: "UTC".into(),
                automations: vec![AutomationRecord {
                    id: "auto-1".into(),
                    name: "A".into(),
                    enabled: true,
                    workspace_dir: None,
                    trigger_json: b"{}".to_vec(),
                    action_json: shell_action_json(),
                    sinks_json: b"[]".to_vec(),
                }],
                states: Vec::new(),
                runs: vec![
                    run_record("keep", "manual", "completed", 1, 0, "A"),
                    run_record("drop", "manual", "failed", 2, 0, "A"),
                ],
            })
            .unwrap();
        store.put_automation_run_transcript("keep", b"[1]").unwrap();
        store.put_automation_run_transcript("drop", b"[2]").unwrap();
        store
            .retain_automation_run_transcripts(&HashSet::from(["keep".to_string()]))
            .unwrap();
        assert_eq!(
            store.get_automation_run_transcript("keep").unwrap(),
            Some(b"[1]".to_vec())
        );
        assert_eq!(store.get_automation_run_transcript("drop").unwrap(), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn automations_snapshot_round_trips_through_relational_tables_and_cascades_transcripts() {
        let dir = temp_dir("automations-domain");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        let mut snapshot = AutomationSnapshot {
            schema_version: 1,
            store_id: "store-1".into(),
            revision: 4,
            timezone_fingerprint: "UTC:0".into(),
            automations: vec![AutomationRecord {
                id: "auto-1".into(),
                name: "Morning".into(),
                enabled: true,
                workspace_dir: Some("/tmp/auto".into()),
                trigger_json: json_bytes(json!({"schedules": []})),
                action_json: shell_action_json(),
                sinks_json: b"[]".to_vec(),
            }],
            states: vec![AutomationStateRecord {
                automation_id: "auto-1".into(),
                next_run_at: Some(10),
                last_run_id: Some("run-keep".into()),
            }],
            runs: vec![
                run_record("run-keep", "manual", "completed", 1, 1, "Morning"),
                run_record("run-drop", "scheduled", "failed", 2, 0, "Morning"),
            ],
        };
        store.put_automation_snapshot(&snapshot).unwrap();
        store
            .put_automation_run_transcript("run-keep", b"[1]")
            .unwrap();
        store
            .put_automation_run_transcript("run-drop", b"[2]")
            .unwrap();

        snapshot.runs.pop();
        snapshot.revision = 5;
        store.put_automation_snapshot(&snapshot).unwrap();

        let loaded = store.get_automation_snapshot().unwrap().unwrap();
        assert_eq!(loaded.revision, 5);
        assert_eq!(loaded.automations[0].name, "Morning");
        assert_eq!(loaded.runs.len(), 1);
        assert_eq!(loaded.runs[0].id, "run-keep");
        assert_eq!(
            store.get_automation_run_transcript("run-keep").unwrap(),
            Some(b"[1]".to_vec())
        );
        assert_eq!(
            store.get_automation_run_transcript("run-drop").unwrap(),
            None
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_v5_schema_with_unrecognized_acp_extensions() {
        let dir = temp_dir("migrate-v5-acp-detail");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(crate::schema::CREATE_SCHEMA)
            .unwrap();
        connection
            .execute_batch("ALTER TABLE acp_detail ADD COLUMN obsolete_context TEXT")
            .unwrap();
        connection.pragma_update(None, "user_version", 5).unwrap();
        drop(connection);

        let error = Store::open_or_create(&path).unwrap_err();
        assert!(
            error.to_string().contains("数据库 schema 5 形状无法识别"),
            "unexpected error: {error}"
        );
        let connection = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            5
        );
        let columns = connection
            .prepare("PRAGMA table_info(acp_detail)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(columns.contains(&"obsolete_context".to_string()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_source_validation_is_consistent() {
        let dir = temp_dir("legacy-source-validation");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        assert!(store.legacy_state("").is_err());
        assert!(store.legacy_state("bad\0source").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn kv_and_legacy_state_are_read_from_one_snapshot() {
        let dir = temp_dir("legacy-lookup");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        assert_eq!(
            store
                .get_with_legacy_state("json", "appearance.json", "json:appearance.json")
                .unwrap(),
            (None, LegacyState::Pending)
        );
        store.put("json", "appearance.json", b"legacy").unwrap();
        assert_eq!(
            store
                .get_with_legacy_state("json", "appearance.json", "json:appearance.json")
                .unwrap(),
            (Some(b"legacy".to_vec()), LegacyState::Pending)
        );
        store
            .delete_with_legacy_tombstone("json:appearance.json", "json", "appearance.json")
            .unwrap();
        assert_eq!(
            store
                .get_with_legacy_state("json", "appearance.json", "json:appearance.json")
                .unwrap(),
            (None, LegacyState::Deleted)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_first_open_initializes_once() {
        let dir = temp_dir("concurrent-open");
        let path = dir.join(DATABASE_FILE_NAME);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads = (0..8)
            .map(|index| {
                let barrier = std::sync::Arc::clone(&barrier);
                let path = path.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let store = Store::open_or_create(path).unwrap();
                    store
                        .put("json", &format!("worker-{index}.json"), b"ready")
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }
        let store = Store::open_or_create(path).unwrap();
        assert_eq!(store.keys("json").unwrap().len(), 8);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn newer_schema_is_rejected() {
        let dir = temp_dir("newer-schema");
        let path = dir.join(DATABASE_FILE_NAME);
        let store = Store::open_or_create(&path).unwrap();
        drop(store);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "journal_mode", "DELETE")
            .unwrap();
        connection.pragma_update(None, "user_version", 99).unwrap();
        drop(connection);
        let error = Store::open_or_create(&path).unwrap_err();
        assert!(
            matches!(
                error,
                StoreError::SchemaTooNew {
                    observed: 99,
                    supported: STORE_SCHEMA_VERSION
                }
            ),
            "{error}"
        );
        let connection = rusqlite::Connection::open(&path).unwrap();
        let journal_mode = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap();
        assert_eq!(journal_mode, "delete", "拒绝未来 schema 时不得先改库");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_schema_is_rejected_without_dropping_rows() {
        let dir = temp_dir("unknown-v1");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE mystery (
                   id TEXT PRIMARY KEY,
                   payload TEXT NOT NULL
                 );
                 INSERT INTO mystery(id, payload) VALUES ('keep-me', 'user-data');
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        drop(connection);

        let error = Store::open_or_create(&path).unwrap_err();
        assert!(error.to_string().contains("形状无法识别"), "{error}");
        assert!(error.to_string().contains("以免清空"), "{error}");

        let connection = rusqlite::Connection::open(&path).unwrap();
        let payload: String = connection
            .query_row(
                "SELECT payload FROM mystery WHERE id = 'keep-me'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(payload, "user-data");
        let rebuilt: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'task')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!rebuilt, "拒绝打开时不得重建当前 schema");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_current_schema_is_rejected_without_dropping_rows() {
        let dir = temp_dir("malformed-current");
        let path = dir.join(DATABASE_FILE_NAME);
        let store = Store::open_or_create(&path).unwrap();
        drop(store);

        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute(
                "INSERT INTO project(root, position, collapsed) VALUES ('/keep-me', 0, 0)",
                [],
            )
            .unwrap();
        connection
            .execute("ALTER TABLE project ADD COLUMN unexpected TEXT", [])
            .unwrap();
        drop(connection);

        let error = Store::open_or_create(&path).unwrap_err();
        assert!(error.to_string().contains("形状无法识别"), "{error}");

        let connection = rusqlite::Connection::open(&path).unwrap();
        let root: String = connection
            .query_row("SELECT root FROM project WHERE position = 0", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(root, "/keep-me");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn every_v1_table_column_shape_is_validated() {
        for table in [
            "acp_detail",
            "history_title",
            "kv",
            "launch_entry",
            "layout_node",
            "project",
            "session",
            "session_group",
        ] {
            let dir = temp_dir(&format!("malformed-v1-{table}"));
            let path = dir.join(DATABASE_FILE_NAME);
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection.execute_batch(v1_schema()).unwrap();
            connection
                .execute(
                    "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                     VALUES ('document:test.json', '$', X'6B656570', 'blob', 1)",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    &format!("ALTER TABLE {table} ADD COLUMN unexpected TEXT"),
                    [],
                )
                .unwrap();
            connection.pragma_update(None, "user_version", 1).unwrap();
            drop(connection);

            let error = Store::open_or_create(&path).unwrap_err();
            assert!(
                error.to_string().contains("形状无法识别"),
                "{table}: {error}"
            );
            let connection = rusqlite::Connection::open(&path).unwrap();
            let value: Vec<u8> = connection
                .query_row(
                    "SELECT value FROM kv WHERE scope = 'document:test.json' AND key = '$'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(value, b"keep", "{table}");
            assert_eq!(
                connection
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                    .unwrap(),
                1,
                "{table}"
            );
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn v1_indexes_foreign_keys_and_checks_are_validated() {
        let variants = [
            (
                "session-index",
                v1_schema().replace(
                    "CREATE INDEX session_group_id_idx ON session(group_id);",
                    "",
                ),
            ),
            (
                "layout-root-index",
                v1_schema().replace(
                    "CREATE UNIQUE INDEX layout_node_root_idx\n  ON layout_node(group_id)\n  WHERE parent_id IS NULL;",
                    "",
                ),
            ),
            (
                "session-foreign-key",
                v1_schema().replacen(
                    "group_id TEXT NOT NULL REFERENCES session_group(id) ON DELETE CASCADE,",
                    "group_id TEXT NOT NULL,",
                    1,
                ),
            ),
            (
                "session-kind-check",
                v1_schema().replace(
                    "kind TEXT NOT NULL CHECK (kind IN ('terminal', 'acp')),",
                    "kind TEXT NOT NULL,",
                ),
            ),
        ];
        for (name, schema) in variants {
            let dir = temp_dir(&format!("malformed-v1-{name}"));
            let path = dir.join(DATABASE_FILE_NAME);
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection.execute_batch(&schema).unwrap();
            connection.pragma_update(None, "user_version", 1).unwrap();
            drop(connection);

            let error = Store::open_or_create(&path).unwrap_err();
            assert!(
                error.to_string().contains("形状无法识别"),
                "{name}: {error}"
            );
            let connection = rusqlite::Connection::open(&path).unwrap();
            assert_eq!(
                connection
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                    .unwrap(),
                1,
                "{name}"
            );
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn migration_shape_failure_rolls_back_schema_version_and_data() {
        let dir = temp_dir("migration-shape-rollback");
        let path = dir.join(DATABASE_FILE_NAME);
        let mut connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(v1_schema()).unwrap();
        connection
            .execute(
                "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                 VALUES ('document:test.json', '$', X'6B656570', 'blob', 1)",
                [],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        let malformed_migration = crate::schema::MIGRATE_V1_TO_V2.replace(
            "result_json BLOB NOT NULL,\n  PRIMARY KEY",
            "result_json BLOB NOT NULL,\n  unexpected TEXT,\n  PRIMARY KEY",
        );
        {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .unwrap();
            let error = imp::migrate_v1_schema(&transaction, &malformed_migration).unwrap_err();
            assert!(error.to_string().contains("形状无法识别"), "{error}");
        }

        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            1
        );
        let value: Vec<u8> = connection
            .query_row(
                "SELECT value FROM kv WHERE scope = 'document:test.json' AND key = '$'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, b"keep");
        let event_log_exists: bool = connection
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'event_log'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!event_log_exists);
        drop(connection);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reopening_current_schema_keeps_rows() {
        let dir = temp_dir("reopen-keeps-rows");
        let path = dir.join(DATABASE_FILE_NAME);
        let store = Store::open_or_create(&path).unwrap();
        store
            .put(
                "json",
                "launch.json",
                br#"{"version":7,"entries":[{"label":"Claude","command":"claude","provider":"claude"}]}"#,
            )
            .unwrap();
        drop(store);

        let store = Store::open_or_create(&path).unwrap();
        let raw = store.get("json", "launch.json").unwrap().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(value["entries"][0]["label"], "Claude");
        drop(store);

        let mut connection = rusqlite::Connection::open(&path).unwrap();
        let transaction = connection.transaction().unwrap();
        transaction
            .execute(
                "ALTER TABLE launch_entry ADD COLUMN note TEXT NOT NULL DEFAULT ''",
                [],
            )
            .unwrap();
        let label: String = transaction
            .query_row(
                "SELECT label FROM launch_entry WHERE position = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(label, "Claude");
        let note: String = transaction
            .query_row(
                "SELECT note FROM launch_entry WHERE position = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(note, "");
        transaction.commit().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn appearance_theme_mode_is_a_kv_row() {
        let dir = temp_dir("appearance-kv");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        store
            .put(
                "json",
                "appearance.json",
                br#"{"theme_mode":"dark","opacity":0.95}"#,
            )
            .unwrap();
        let connection = rusqlite::Connection::open(dir.join(DATABASE_FILE_NAME)).unwrap();
        let mode: String = connection
            .query_row(
                "SELECT CAST(value AS TEXT) FROM kv
                 WHERE scope = 'appearance' AND key = 'theme_mode'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mode, "dark");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn workspace_is_split_across_the_relational_state_tables() {
        let dir = temp_dir("relational-workspace");
        let path = dir.join(DATABASE_FILE_NAME);
        let store = Store::open_or_create(&path).unwrap();
        let workspace = serde_json::json!({
            "projects": ["/repo"],
            "active_session": 1,
            "route": "session",
            "sidebar_w": 280.0,
            "collapsed_projects": ["/repo"],
            "collapsed_agents": ["agent-def-1"],
            "sessions": [
                {
                    "layout": {"Split": {
                        "axis": "H",
                        "children": [
                            {"Leaf": {"cwd": "/repo", "id": "term-1"}},
                            {"Leaf": {
                                "cwd": "/repo/src",
                                "id": "term-2",
                                "custom_title": "tests",
                                "launch_label": "Codex",
                                "launch_cmd": "codex"
                            }}
                        ],
                        "sizes": [420.0, 580.0]
                    }},
                    "active": 1,
                    "last_updated_at": 42,
                    "custom_title": "terminal group",
                    "route": {"expanded": ["/repo/src"], "tool_panel_open": true}
                },
                {
                    "layout": {"Leaf": {"cwd": "/repo", "id": null}},
                    "active": 0,
                    "last_updated_at": 84,
                    "custom_title": "agent group",
                    "acp": {
                        "cwd": "/repo",
                        "launch": {"command": "claude --acp", "env": {"PROFILE": "work"}},
                        "profile_id": "profile-1",
                        "agent": "claude",
                        "history_session_id": "history-1",
                        "sid": "acp-1",
                        "agent_definition_id": "agent-def-1",
                        "session_title": "季度复盘",
                        "refresh_launch_from_settings": true,
                        "config_values": [["model", "sonnet"]],
                        "pending_agent_preset": "先检查正确性和回归风险",
                        "conversation_binding": {
                            "type": "plugin",
                            "plugin_id": "com.example.quant",
                            "route": {
                                "contribution_id": "strategy-input",
                                "context": {"strategy_id": "strategy-1"}
                            }
                        },
                        "agent_session": {
                            "agent": {
                                "plugin_id": "com.example.quant",
                                "contribution_id": "quant-agent"
                            },
                            "controller": {
                                "plugin_id": "com.example.quant",
                                "contribution_id": "quant-session"
                            },
                            "instance": {
                                "plugin_id": "com.example.quant",
                                "resource_type": "strategy",
                                "resource_id": "strategy-1"
                            }
                        }
                    },
                    "route": null
                }
            ]
        });
        store
            .put(
                "json",
                "workspace.json",
                &serde_json::to_vec(&workspace).unwrap(),
            )
            .unwrap();

        let connection = rusqlite::Connection::open(&path).unwrap();
        for (table, expected) in [
            ("project", 1i64),
            ("session_group", 2),
            ("session", 3),
            ("layout_node", 4),
            ("acp_detail", 1),
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, expected, "{table}");
        }
        let profile: String = connection
            .query_row(
                "SELECT CAST(value AS TEXT) FROM kv
                 WHERE scope = 'acp:acp-1' AND key = 'env/PROFILE'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(profile, "work");
        let old_payload_tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name IN ('workspace_meta', 'workspace_session')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_payload_tables, 0);

        let restored: serde_json::Value =
            serde_json::from_slice(&store.get("json", "workspace.json").unwrap().unwrap()).unwrap();
        assert_eq!(restored["sessions"][0]["layout"]["Split"]["axis"], "H");
        assert_eq!(restored["sessions"][0]["active"], 1);
        assert_eq!(restored["sessions"][1]["acp"]["sid"], "acp-1");
        assert_eq!(
            restored["sessions"][1]["acp"]["config_values"][0][1],
            "sonnet"
        );
        assert_eq!(
            restored["sessions"][1]["acp"]["pending_agent_preset"],
            "先检查正确性和回归风险"
        );
        assert_eq!(
            restored["sessions"][1]["acp"]["conversation_binding"]["route"]["context"]["strategy_id"],
            "strategy-1"
        );
        assert_eq!(
            restored["sessions"][1]["acp"]["agent_session"]["controller"]["contribution_id"],
            "quant-session"
        );
        // 智能体归属必须活过落盘：丢了这一列，重启后对话会退化成按 cwd 成组的项目。
        assert_eq!(
            restored["sessions"][1]["acp"]["agent_definition_id"],
            "agent-def-1"
        );
        assert!(restored["sessions"][1]["acp"]["automation_id"].is_null());
        // 标题同样要活过落盘，否则重启后同一智能体的多段对话名字全一样。
        assert_eq!(restored["sessions"][1]["acp"]["session_title"], "季度复盘");
        let acp_columns = connection
            .prepare("PRAGMA table_info(acp_detail)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            acp_columns,
            [
                "session_id",
                "agent",
                "profile_id",
                "history_session_id",
                "launch_command",
                "refresh_launch_from_settings",
                "fork_session_id",
                "fork_title",
                "fork_agent",
                "fork_profile_label",
                "fork_from_history",
                "pending_prompt",
                "pending_delivery_id",
                "agent_definition_id",
                "automation_id",
            ]
        );
        assert_eq!(restored["collapsed_projects"], serde_json::json!(["/repo"]));
        assert_eq!(
            restored["collapsed_agents"],
            serde_json::json!(["agent-def-1"])
        );
        drop(connection);
        store
            .put(
                "json",
                "workspace.json",
                &serde_json::to_vec(&workspace).unwrap(),
            )
            .unwrap();
        let connection = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM session", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            3,
            "重复快照应原子替换，不能累积旧 session"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn workspace_snapshot_keeps_live_sidebar_fields() {
        let raw = serde_json::to_vec(&serde_json::json!({
            "active_session": 1,
            "active_session_id": "sid-1",
            "collapsed_agents": ["research", "quant"],
            "pinned_projects": ["/repo/smelt", "/repo/pulse"],
            "sidebar_hide_empty_projects": true,
            "projects": ["/repo/smelt"],
            "sessions": []
        }))
        .unwrap();
        let snapshot = Store::workspace_snapshot_from_json(&raw).unwrap();
        assert_eq!(snapshot.active_session, 1);
        assert_eq!(snapshot.active_session_id.as_deref(), Some("sid-1"));
        assert_eq!(
            snapshot.collapsed_agents,
            vec!["research".to_string(), "quant".to_string()]
        );
        assert_eq!(
            snapshot.pinned_projects,
            vec!["/repo/smelt".to_string(), "/repo/pulse".to_string()]
        );
        assert!(snapshot.sidebar_hide_empty_projects);

        let dir = temp_dir("workspace-live-fields");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        store.put_workspace_snapshot(&snapshot).unwrap();
        let restored = store.get_workspace_snapshot().unwrap().unwrap();
        assert_eq!(restored.active_session_id.as_deref(), Some("sid-1"));
        assert_eq!(restored.collapsed_agents, snapshot.collapsed_agents);
        assert_eq!(restored.pinned_projects, snapshot.pinned_projects);
        assert!(restored.sidebar_hide_empty_projects);
        let json: serde_json::Value =
            serde_json::from_slice(&Store::workspace_snapshot_to_json(&restored).unwrap()).unwrap();
        assert_eq!(json["active_session_id"], "sid-1");
        assert_eq!(json["collapsed_agents"][1], "quant");
        assert_eq!(json["pinned_projects"][1], "/repo/pulse");
        assert_eq!(json["sidebar_hide_empty_projects"], true);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn workspace_snapshot_reads_leftover_generic_document() {
        let dir = temp_dir("workspace-generic-doc");
        let path = dir.join(DATABASE_FILE_NAME);
        drop(Store::open_or_create(&path).unwrap());
        let raw = serde_json::to_vec(&serde_json::json!({
            "active_session_id": "sid-legacy",
            "projects": ["/repo"],
            "sessions": [{
                "layout": {"Leaf": {"cwd": "/repo", "id": "sid-legacy"}},
                "active": 0
            }]
        }))
        .unwrap();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute(
                "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                 VALUES ('document:workspace.json', '$', ?1, 'blob', 1)",
                rusqlite::params![raw],
            )
            .unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        let snapshot = store
            .get_workspace_snapshot()
            .unwrap()
            .expect("旧 json 文档应回退读出工作区");
        assert_eq!(snapshot.active_session_id.as_deref(), Some("sid-legacy"));
        assert_eq!(snapshot.projects[0].root, "/repo");
        assert_eq!(snapshot.sessions.len(), 1);

        store.put_workspace_snapshot(&snapshot).unwrap();
        let leftover: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM kv WHERE scope = 'document:workspace.json'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(leftover, 0, "写入类型化快照后应删掉旧 json 文档");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn appearance_snapshot_uses_domain_defaults_for_missing_fields() {
        let dir = temp_dir("appearance-defaults");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        store
            .put("json", "appearance.json", br#"{"theme_mode":"light"}"#)
            .unwrap();
        let snapshot = store.get_appearance_snapshot().unwrap().unwrap();
        assert!(
            (snapshot.bg_image_opacity - 0.25).abs() < f32::EPSILON,
            "bg_image_opacity={}",
            snapshot.bg_image_opacity
        );
        assert_eq!(snapshot.ui_font_px, 16);
        assert_eq!(snapshot.font_px, 13);
        assert_eq!(snapshot.theme_mode, "light");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn agent_ui_missing_hooks_key_defaults_to_enabled() {
        let snapshot = Store::agent_ui_snapshot_from_json(&serde_json::json!({})).unwrap();
        assert!(snapshot.agent_hooks_enabled);
        let explicit = Store::agent_ui_snapshot_from_json(&serde_json::json!({
            "agent_hooks_enabled": false
        }))
        .unwrap();
        assert!(!explicit.agent_hooks_enabled);
    }

    #[test]
    fn catalog_snapshots_round_trip_without_kv_documents() {
        let dir = temp_dir("catalog-snapshots");
        let path = dir.join(DATABASE_FILE_NAME);
        let store = Store::open_or_create(&path).unwrap();
        let agent_ui = AgentUiSnapshot {
            agent_hooks_enabled: true,
            cross_agent_enabled: false,
            notify_approval: false,
            notify_input: true,
            notify_success: false,
            notify_failure: true,
            notify_terminal_bell: false,
            commands: vec![AgentConversationCommandRecord {
                command_key: "acp_cmd".into(),
                command: "claude --acp".into(),
            }],
            env: vec![AgentAcpEnvRecord {
                engine_kind_id: "claude".into(),
                name: "ANTHROPIC_API_KEY".into(),
                value: "sk-test".into(),
            }],
            config_memory: vec![AgentAcpConfigMemoryRecord {
                engine_kind_id: "claude".into(),
                config_id: "model".into(),
                value_id: "sonnet".into(),
            }],
            agents: vec![AgentDefinitionRecord {
                id: "agent-1".into(),
                name: "交易助手".into(),
                description: "盯盘".into(),
                engine_kind_id: "pi".into(),
                prompt: "先核对成交".into(),
                plugins_json: b"[]".to_vec(),
                context_folders_json: b"[]".to_vec(),
                context_links_json: b"[]".to_vec(),
                model_provider: String::new(),
                model_id: String::new(),
            }],
            profiles: vec![AgentProfileRecord {
                id: "profile-1".into(),
                kind_id: "claude".into(),
                label: "量化".into(),
                workspace_dir: "/tmp/quant".into(),
            }],
        };
        store.put_agent_ui_snapshot(&agent_ui).unwrap();
        assert_eq!(
            store.get_agent_ui_snapshot().unwrap().as_ref(),
            Some(&agent_ui)
        );

        let extra = AgentDefinitionRecord {
            id: "agent-2".into(),
            name: "写作".into(),
            description: String::new(),
            engine_kind_id: "pi".into(),
            prompt: "先列提纲".into(),
            plugins_json: b"[]".to_vec(),
            context_folders_json: b"[]".to_vec(),
            context_links_json: b"[]".to_vec(),
            model_provider: String::new(),
            model_id: String::new(),
        };
        store.insert_agent_definition(&extra).unwrap();
        store.put_agent_ui_prefs(&agent_ui).unwrap();
        let after_prefs = store.get_agent_ui_snapshot().unwrap().unwrap();
        assert_eq!(after_prefs.agents.len(), 2);
        assert_eq!(after_prefs.agents[1].id, "agent-2");
        assert!(!after_prefs.cross_agent_enabled);

        let mut renamed = extra;
        renamed.name = "改名".into();
        assert!(store.update_agent_definition(&renamed).unwrap());
        assert!(store.delete_agent_definition("agent-2").unwrap());
        assert!(!store.delete_agent_definition("agent-2").unwrap());
        let after_delete = store.get_agent_ui_snapshot().unwrap().unwrap();
        assert_eq!(after_delete.agents.len(), 1);
        assert_eq!(after_delete.agents[0].id, "agent-1");

        let remote = RemoteSessionCatalogSnapshot {
            acp: vec![RemoteAcpSessionRecord {
                id: "acp-1".into(),
                cwd: "/work".into(),
                title: "远程 Claude".into(),
                agent_option_id: "claude".into(),
                agent: "claude".into(),
                launch_command: "claude --acp".into(),
                launch_env_json: b"{}".to_vec(),
                resume_id: Some("resume-1".into()),
                created_at: 42,
                lifecycle: "active".into(),
                hidden: false,
            }],
            terminal: vec![RemoteTerminalSessionRecord {
                id: "term-1".into(),
                cwd: "/work".into(),
                title: "远程终端".into(),
                created_at: 43,
                lifecycle: "creating".into(),
            }],
        };
        store.put_remote_session_snapshot(&remote).unwrap();
        assert_eq!(
            store.get_remote_session_snapshot().unwrap().as_ref(),
            Some(&remote)
        );

        let published = PublishedSessionSnapshot {
            version: 1,
            sessions: vec![PublishedSessionRecord {
                id: "s1".into(),
                cwd: Some("/work".into()),
                launch: Some("claude".into()),
                provider: Some("claude".into()),
                conversation_id: Some("conv-1".into()),
                title: Some("崩溃前".into()),
                phase: "awaiting_approval".into(),
                phase_since: 100,
                updated_at: 200,
                structured_events: true,
                turn_events: true,
                agent_event_version: Some(1),
                tokens_used: Some(42),
                branch: Some("feat/store".into()),
                dirty_files_json: b"[\"a.rs\"]".to_vec(),
            }],
        };
        store.put_published_session_snapshot(&published).unwrap();
        assert_eq!(
            store.get_published_session_snapshot().unwrap().as_ref(),
            Some(&published)
        );

        let connection = rusqlite::Connection::open(&path).unwrap();
        for (table, expected) in [
            ("agent_ui_pref", 7i64),
            ("agent_acp_command", 1),
            ("agent_acp_env", 1),
            ("agent_acp_config_memory", 1),
            ("agent_definition", 1),
            ("agent_profile", 1),
            ("remote_acp_session", 1),
            ("remote_terminal_session", 1),
            ("published_session", 1),
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, expected, "{table}");
        }
        let leftover: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM kv WHERE scope IN (
                   'document:agent_ui.json',
                   'document:remote_acp_sessions.json',
                   'document:remote_terminal_sessions.json',
                   'document:sessions.json'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(leftover, 0, "活路径不得再写 kv 文档");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn migrates_v9_catalog_kv_documents_into_relational_tables() {
        let dir = temp_dir("migrate-v9-catalog");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(&v9_schema()).unwrap();
        connection
            .execute(
                "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                 VALUES ('document:agent_ui.json', '$', ?1, 'blob', 1)",
                rusqlite::params![
                    serde_json::to_vec(&json!({
                        "agent_hooks_enabled": true,
                        "acp_cmd": "claude-custom --acp",
                        "acp_env": {"claude": {"TOKEN": "secret"}},
                        "agents": [{
                            "id": "agent-1",
                            "name": "助手",
                            "description": "",
                            "agent_id": "pi",
                            "prompt": "hello",
                            "plugins": [],
                            "context_folders": [],
                            "context_links": []
                        }]
                    }))
                    .unwrap()
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                 VALUES ('document:remote_acp_sessions.json', '$', ?1, 'blob', 1)",
                rusqlite::params![
                    serde_json::to_vec(&json!({
                        "sessions": [{
                            "id": "acp-1",
                            "cwd": "/work",
                            "title": "远程",
                            "agent_option_id": "claude",
                            "agent": "claude",
                            "launch": {"command": "claude --acp", "env": {}},
                            "created_at": 1,
                            "lifecycle": "active",
                            "hidden": false
                        }]
                    }))
                    .unwrap()
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                 VALUES ('document:remote_terminal_sessions.json', '$', ?1, 'blob', 1)",
                rusqlite::params![
                    serde_json::to_vec(&json!({
                        "sessions": [{
                            "id": "term-1",
                            "cwd": "/work",
                            "title": "终端",
                            "created_at": 2,
                            "lifecycle": "active"
                        }]
                    }))
                    .unwrap()
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                 VALUES ('document:sessions.json', '$', ?1, 'blob', 1)",
                rusqlite::params![
                    serde_json::to_vec(&json!({
                        "version": 1,
                        "sessions": [{
                            "id": "s1",
                            "phase": "idle",
                            "phase_since": 0,
                            "updated_at": 0,
                            "structured_events": false,
                            "turn_events": false,
                            "dirty_files": []
                        }]
                    }))
                    .unwrap()
                ],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 9).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        let agent_ui = store.get_agent_ui_snapshot().unwrap().unwrap();
        assert!(
            agent_ui.agent_hooks_enabled,
            "hooks={:?} commands={:?} agents={:?} env={:?}",
            agent_ui.agent_hooks_enabled, agent_ui.commands, agent_ui.agents, agent_ui.env
        );
        assert_eq!(
            agent_ui
                .commands
                .iter()
                .map(|command| command.command.as_str())
                .collect::<Vec<_>>(),
            vec!["claude-custom --acp"],
            "{agent_ui:#?}"
        );
        assert_eq!(
            agent_ui
                .agents
                .iter()
                .map(|agent| agent.id.as_str())
                .collect::<Vec<_>>(),
            vec!["agent-1"],
            "{agent_ui:#?}"
        );
        let remote = store.get_remote_session_snapshot().unwrap().unwrap();
        assert_eq!(
            remote
                .acp
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec!["acp-1"],
            "{remote:#?}"
        );
        assert_eq!(
            remote
                .terminal
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec!["term-1"],
            "{remote:#?}"
        );
        let published = store.get_published_session_snapshot().unwrap().unwrap();
        assert_eq!(
            published
                .sessions
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec!["s1"],
            "{published:#?}"
        );
        drop(store);

        let connection = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            STORE_SCHEMA_VERSION
        );
        let leftover: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM kv WHERE scope IN (
                   'document:agent_ui.json',
                   'document:remote_acp_sessions.json',
                   'document:remote_terminal_sessions.json',
                   'document:sessions.json'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(leftover, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn migrates_v10_agent_engine_columns_without_losing_data() {
        let dir = temp_dir("migrate-v10-engine-kind");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(&v10_schema()).unwrap();
        connection
            .execute(
                "INSERT INTO agent_acp_env(agent_id, name, value) VALUES ('pi', 'TOKEN', 'secret')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO agent_acp_config_memory(agent_id, config_id, value_id, position)
                 VALUES ('pi', 'model', 'default', 0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO agent_definition(
                   id, name, description, agent_id, prompt, plugins_json,
                   context_folders_json, context_links_json, position
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    "agent-1",
                    "助手",
                    "",
                    "pi",
                    "hello",
                    b"[]".as_slice(),
                    b"[]".as_slice(),
                    b"[]".as_slice(),
                    0,
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO project(root, position, collapsed) VALUES ('/repo', 0, 0)",
                [],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 10).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        let workspace = store
            .get_workspace_snapshot()
            .unwrap()
            .expect("v10→v11 不得丢掉已有项目");
        assert_eq!(workspace.projects[0].root, "/repo");
        let snapshot = store.get_agent_ui_snapshot().unwrap().unwrap();
        assert_eq!(snapshot.env[0].engine_kind_id, "pi");
        assert_eq!(snapshot.config_memory[0].engine_kind_id, "pi");
        assert_eq!(snapshot.agents[0].engine_kind_id, "pi");
        assert_eq!(snapshot.agents[0].model_provider, "");
        assert_eq!(snapshot.agents[0].model_id, "");
        drop(store);

        let connection = rusqlite::Connection::open(&path).unwrap();
        for table in [
            "agent_acp_env",
            "agent_acp_config_memory",
            "agent_definition",
        ] {
            let columns = imp::table_columns(&connection, table).unwrap();
            assert!(columns.iter().any(|column| column == "engine_kind_id"));
            assert!(!columns.iter().any(|column| column == "agent_id"));
        }
        let agent_columns = imp::table_columns(&connection, "agent_definition").unwrap();
        assert!(
            agent_columns
                .iter()
                .any(|column| column == "model_provider")
        );
        assert!(agent_columns.iter().any(|column| column == "model_id"));
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            STORE_SCHEMA_VERSION
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn config_snapshots_round_trip_without_kv_documents() {
        let dir = temp_dir("config-snapshots");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        store
            .put_remote_config_snapshot(&RemoteConfigSnapshot {
                enabled: true,
                iroh_relay: "relay.example".into(),
                write_enabled: true,
            })
            .unwrap();
        store
            .put_dsh_auto_model_snapshot(&AutoModelSnapshot {
                providers: vec!["sub2api".into()],
            })
            .unwrap();
        let loaded = store.get_remote_config_snapshot().unwrap().unwrap();
        assert!(loaded.enabled);
        assert_eq!(loaded.iroh_relay, "relay.example");
        assert_eq!(
            store
                .get_dsh_auto_model_snapshot()
                .unwrap()
                .unwrap()
                .providers,
            vec!["sub2api".to_string()]
        );
        let leftover: i64 = rusqlite::Connection::open(dir.join(DATABASE_FILE_NAME))
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM kv WHERE scope IN (
                   'document:collab.json',
                   'document:dsh-auto-models.json'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(leftover, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn leftover_config_kv_documents_are_visible_to_typed_snapshots() {
        let dir = temp_dir("config-leftover");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(crate::schema::CREATE_SCHEMA)
            .unwrap();
        connection
            .pragma_update(None, "user_version", STORE_SCHEMA_VERSION)
            .unwrap();
        connection
            .execute(
                "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                 VALUES ('document:collab.json', '$', ?1, 'blob', 1)",
                rusqlite::params![br#"{"enabled":true,"iroh_relay":"old","write_enabled":true}"#],
            )
            .unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        let loaded = store.get_remote_config_snapshot().unwrap().unwrap();
        assert!(loaded.enabled);
        assert_eq!(loaded.iroh_relay, "old");
        store.put_remote_config_snapshot(&loaded).unwrap();
        let leftover: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM kv WHERE scope = 'document:collab.json'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(leftover, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn launch_lands_in_relational_tables() {
        let dir = temp_dir("relational-docs");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        store
            .put(
                "json",
                "launch.json",
                br#"{"version":7,"entries":[{"label":"Claude","command":"claude","provider":"claude"}]}"#,
            )
            .unwrap();

        let connection = rusqlite::Connection::open(dir.join(DATABASE_FILE_NAME)).unwrap();
        let label: String = connection
            .query_row(
                "SELECT label FROM launch_entry WHERE position = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(label, "Claude");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn database_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("permissions");
        let path = dir.join(DATABASE_FILE_NAME);
        Store::open_or_create(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        drop(Store::open(&path).unwrap());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_missing_database_does_not_create_it() {
        let dir = temp_dir("open-missing");
        let path = dir.join(DATABASE_FILE_NAME);
        let error = Store::open(&path).unwrap_err();
        assert!(
            matches!(error, StoreError::MissingDatabase { .. }),
            "{error}"
        );
        assert!(!path.exists(), "open 缺失路径不得建库");
        assert!(!super::database_sidecar_path(&path, "-wal").exists());
        assert!(!super::database_sidecar_path(&path, "-shm").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_or_create_refuses_orphan_sidecars() {
        let dir = temp_dir("orphan-sidecar");
        let path = dir.join(DATABASE_FILE_NAME);
        std::fs::write(super::database_sidecar_path(&path, "-wal"), b"leftover").unwrap();

        let error = Store::open_or_create(&path).unwrap_err();
        assert!(
            matches!(error, StoreError::OrphanSidecars { .. }),
            "{error}"
        );
        assert!(!path.exists(), "有 sidecar 残留时不得建空主库");

        let error = Store::create(&path).unwrap_err();
        assert!(
            matches!(error, StoreError::OrphanSidecars { .. }),
            "{error}"
        );
        assert!(!path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn create_refuses_to_overwrite_an_existing_database() {
        let dir = temp_dir("create-exists");
        let path = dir.join(DATABASE_FILE_NAME);
        Store::create(&path).unwrap();
        let error = Store::create(&path).unwrap_err();
        assert!(matches!(error, StoreError::AlreadyExists { .. }), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_refuses_to_bootstrap_an_empty_existing_file() {
        let dir = temp_dir("empty-file");
        let path = dir.join(DATABASE_FILE_NAME);
        std::fs::write(&path, []).unwrap();
        let error = Store::open(&path).unwrap_err();
        assert!(
            error.to_string().contains("未初始化")
                || error.to_string().contains("拒绝建空库")
                || error.to_string().contains("打开"),
            "{error}"
        );
        if path.exists() {
            let connection = rusqlite::Connection::open(&path).unwrap();
            let version: u32 = connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(version, 0, "open 不得把空文件初始化成当前 schema");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn database_presence_classifies_absent_ready_and_orphan() {
        let dir = temp_dir("presence");
        let path = dir.join(DATABASE_FILE_NAME);
        assert_eq!(database_presence(&path), DatabasePresence::Absent);
        Store::open_or_create(&path).unwrap();
        assert_eq!(database_presence(&path), DatabasePresence::Ready);
        std::fs::remove_file(&path).unwrap();
        let _ = std::fs::remove_file(super::database_sidecar_path(&path, "-wal"));
        let _ = std::fs::remove_file(super::database_sidecar_path(&path, "-shm"));
        std::fs::write(super::database_sidecar_path(&path, "-shm"), b"x").unwrap();
        assert_eq!(database_presence(&path), DatabasePresence::OrphanSidecars);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
