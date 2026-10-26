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

// 这些模块只服务桌面的 SQLite 实现（cfg 与下面的 `imp` 保持一致）。移动端不编
// native SQLite，rusqlite/serde_json 在 iOS/Android 上根本没进依赖树，无条件声明
// 会让交叉编译直接挂在解析不到 crate 上。
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod automation;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod catalog;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod config;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod devices;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod history;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod launch;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod memory;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod migrate_v18;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod schema;
mod snapshots;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod workspace;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod workspace_document;

pub use snapshots::{
    AgentAcpConfigMemoryRecord, AgentAcpEnvRecord, AgentConversationCommandRecord,
    AgentDefinitionRecord, AgentProfileRecord, AgentUiSnapshot, HistoryTitleRecord,
    HistoryTitleSnapshot, LaunchEntryRecord, LaunchSnapshot, MemoryMetaSnapshot,
    MemorySettingsRecord, MemoryTrust, MemoryUsageRecord, PairedDevice, PublishedSessionRecord,
    PublishedSessionSnapshot, PublishedWorkspaceMenuSnapshot, QuotaCacheQuota, QuotaCacheSnapshot,
    QuotaCacheStatusRecord, QuotaCacheWindowRecord, RemoteAcpSessionRecord, RemoteConfigSnapshot,
    RemoteSessionCatalogSnapshot, RemoteTerminalSessionRecord, TerminalThemeSnapshot,
    WorkspaceAcpRecord, WorkspaceCatalogCommit, WorkspaceCatalogWriter, WorkspaceMenuProjectRecord,
    WorkspaceMenuSessionRecord, WorkspaceSessionRecord, WorkspaceSnapshot, WorkspaceTerminalRecord,
};

pub const DATABASE_FILE_NAME: &str = "smelt.sqlite3";

/// 智能体记忆元数据的 scope：某个智能体自己的那一层。
pub fn memory_scope_for_agent(agent_definition_id: &str) -> String {
    format!("agent:{agent_definition_id}")
}

/// 智能体记忆元数据的 scope：跨智能体的用户全局层。
pub const USER_MEMORY_SCOPE: &str = "user";
pub const STORE_SCHEMA_VERSION: u32 = 25;

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

#[derive(Clone)]
pub struct Store {
    backend: imp::Backend,
}

fn store_from_backend(backend: imp::Backend) -> Store {
    Store { backend }
}

impl std::fmt::Debug for Store {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Store").finish_non_exhaustive()
    }
}

/// `acp_detail.history_session_id` 的三种事实。没有行，和行在但列是空，不能合成同一个 `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredHistoryIdentity {
    /// 还没有 `acp_detail` 行。显式开会话可以带上选中的对话文件。
    NoRow,
    /// 行在，列是空。窗口缓存不能把旧 id 填回来。
    Unbound,
    /// 已绑定的对话身份：第一次落库时窗口带的，或守护进程之后确认的。
    Bound(String),
}

impl Store {
    /// 打开已有库。文件不在、或只剩 WAL/SHM sidecar 时失败，绝不建空库。
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        let backend = imp::Backend::open(&path)?;
        Ok(store_from_backend(backend))
    }

    /// 在空位上建新库。主文件或 sidecar 已在则失败。
    pub fn create(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        let backend = imp::Backend::create(&path)?;
        Ok(store_from_backend(backend))
    }

    /// 已有则打开，主文件与 sidecar 都不在才建库。orphan sidecar 拒绝建空库。
    pub fn open_or_create(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        let backend = imp::Backend::open_or_create(&path)?;
        Ok(store_from_backend(backend))
    }

    /// 只读库的 `user_version`，不建库、不迁移。交接 tripwire 用：
    /// 文件不在/损坏一律 None（无信息≠已迁移，按可回滚处理）。
    pub fn schema_version(path: &std::path::Path) -> Option<u32> {
        imp::read_user_version(path)
    }

    /// 把 WAL 合并回主库并截断。停机时调用，避免退出后还留着 `-wal` 占用。
    pub fn checkpoint_wal_truncate(&self) -> Result<(), StoreError> {
        imp::checkpoint_wal_truncate(&self.backend)
    }

    /// 运行时身份。只有 smeltd 第一次启动时写一次。
    pub fn runtime_id(&self) -> Result<Option<String>, StoreError> {
        imp::runtime_id(&self.backend)
    }

    pub fn put_runtime_id(&self, runtime_id: &str) -> Result<(), StoreError> {
        validate_name("runtime_id", runtime_id)?;
        imp::put_runtime_id(&self.backend, runtime_id)
    }

    /// 工作区目录的版本号，每次保存加一。从没保存过是 0。
    pub fn workspace_catalog_revision(&self) -> Result<u64, StoreError> {
        imp::workspace_catalog_revision(&self.backend)
    }

    /// 基于 `base_revision` 提交整份工作区目录。比对版本、写快照、推进版本、记下写入方
    /// 在同一个事务里，中途失败什么都不留下。
    ///
    /// 基准就是当前版本时照常提交。基准落后时，只有当前版本正是同一个写入方用更小的
    /// 序号提交的，才算它自己的提交：回执在路上丢了，客户端还停在旧版本，这不是冲突。
    /// 写入方的版本只在收到回执或重新读取时前进，它基于旧版本而当前版本是它写的，
    /// 中间那些版本也都是它写的；别人写过一次，最后的写入方就不是它了。序号挡住超时后
    /// 才到的旧请求，免得它盖掉后发先到的新提交。
    pub fn commit_workspace_snapshot(
        &self,
        base_revision: u64,
        writer: &WorkspaceCatalogWriter,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<WorkspaceCatalogCommit, StoreError> {
        validate_name("workspace catalog writer", &writer.id)?;
        imp::commit_workspace_snapshot(&self.backend, base_revision, writer, snapshot)
    }

    /// 某条历史会话由哪个智能体定义创建。`profile_id` 为空表示没有 profile。
    pub fn history_agent_binding(
        &self,
        agent: &str,
        profile_id: &str,
        resume_id: &str,
    ) -> Result<Option<String>, StoreError> {
        imp::history_agent_binding(&self.backend, agent, profile_id, resume_id)
    }

    /// 某引擎、某 profile 下全部历史会话的智能体绑定：`resume_id` → 智能体定义 id。
    pub fn history_agent_bindings(
        &self,
        agent: &str,
        profile_id: &str,
    ) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
        imp::history_agent_bindings(&self.backend, agent, profile_id)
    }

    pub fn put_history_agent_binding(
        &self,
        agent: &str,
        profile_id: &str,
        resume_id: &str,
        agent_definition_id: &str,
    ) -> Result<(), StoreError> {
        validate_name("agent", agent)?;
        validate_name("resume_id", resume_id)?;
        validate_name("agent_definition_id", agent_definition_id)?;
        imp::put_history_agent_binding(
            &self.backend,
            agent,
            profile_id,
            resume_id,
            agent_definition_id,
        )
    }

    /// 侧栏「新对话」默认用哪个智能体。
    pub fn default_conversation_agent(&self) -> Result<Option<String>, StoreError> {
        imp::default_conversation_agent(&self.backend)
    }

    /// `None` 清掉默认智能体。
    pub fn put_default_conversation_agent(
        &self,
        agent_definition_id: Option<&str>,
    ) -> Result<(), StoreError> {
        if let Some(id) = agent_definition_id {
            validate_name("agent_definition_id", id)?;
        }
        imp::put_default_conversation_agent(&self.backend, agent_definition_id)
    }

    /// 新建菜单里置顶的动作 key，按置顶顺序。
    pub fn new_session_pins(&self) -> Result<Vec<String>, StoreError> {
        imp::new_session_pins(&self.backend)
    }

    pub fn put_new_session_pins(&self, pins: &[String]) -> Result<(), StoreError> {
        imp::put_new_session_pins(&self.backend, pins)
    }

    /// 记忆召回的配置；没设过时各项都是空字符串。
    pub fn memory_settings(&self) -> Result<MemorySettingsRecord, StoreError> {
        imp::memory_settings(&self.backend)
    }

    /// 在同一个 IMMEDIATE 事务里读-改-写记忆配置，返回提交后的值。CLI、Control API
    /// 各改一项时不会拿旧整行把对方刚写的那项盖掉；`modify` 返回错误时整笔回滚。
    pub fn modify_memory_settings<E: From<StoreError>>(
        &self,
        modify: impl FnOnce(&mut MemorySettingsRecord) -> Result<(), E>,
    ) -> Result<MemorySettingsRecord, E> {
        imp::modify_memory_settings(&self.backend, modify)
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

    pub fn get_workspace_snapshot(&self) -> Result<Option<WorkspaceSnapshot>, StoreError> {
        imp::get_workspace_snapshot(&self.backend)
    }

    /// 会话 id → 智能体定义 id。
    pub fn session_agent_definition_ids(
        &self,
    ) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
        imp::session_agent_definition_ids(&self.backend)
    }

    pub fn put_workspace_snapshot(&self, snapshot: &WorkspaceSnapshot) -> Result<(), StoreError> {
        imp::put_workspace_snapshot(&self.backend, snapshot)
    }

    /// 守护进程确认了一份可恢复的对话身份。行还不存在时返回 `false`，不建行。
    pub fn remember_history_session_id(
        &self,
        session_id: &str,
        history_id: &str,
    ) -> Result<bool, StoreError> {
        imp::remember_history_session_id(&self.backend, session_id, history_id)
    }

    /// 读这份会话已经确认的对话身份。没有行、或还没确认过，都是 `None`。
    pub fn history_session_id(&self, session_id: &str) -> Result<Option<String>, StoreError> {
        imp::history_session_id(&self.backend, session_id)
    }

    /// 没有行、行在但还没确认、已经确认。恢复时这三件事要分开。
    pub fn stored_history_identity(
        &self,
        session_id: &str,
    ) -> Result<StoredHistoryIdentity, StoreError> {
        imp::stored_history_identity(&self.backend, session_id)
    }

    /// 守护进程明确放弃这份身份（历史文件已经读不出来）。窗口存档不能把它填回来。
    pub fn clear_history_session_id(&self, session_id: &str) -> Result<bool, StoreError> {
        imp::clear_history_session_id(&self.backend, session_id)
    }

    /// `workspace.save` 发来的工作区文档转成类型化快照。文档只是传输格式。
    pub fn workspace_snapshot_from_json(raw: &[u8]) -> Result<WorkspaceSnapshot, StoreError> {
        imp::workspace_snapshot_from_json(raw)
    }

    /// 类型化快照转成 `workspace.load` 回给客户端的文档。
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

    /// 在同一个 IMMEDIATE 事务里读-改-写一条智能体定义，返回提交后的行。
    ///
    /// GUI 与 smeltd（Control API / CLI）都会改定义；只提供这一条更新入口，避免任一方
    /// 拿事务外读到的旧整行回写。行不存在（已被别人删除）返回 `Ok(None)`，不会建行；
    /// `modify` 不得改 `id`，返回错误时整笔回滚。
    pub fn modify_agent_definition<E: From<StoreError>>(
        &self,
        id: &str,
        modify: impl FnOnce(&mut AgentDefinitionRecord) -> Result<(), E>,
    ) -> Result<Option<AgentDefinitionRecord>, E> {
        imp::modify_agent_definition(&self.backend, id, modify)
    }

    pub fn delete_agent_definition(&self, id: &str) -> Result<bool, StoreError> {
        imp::delete_agent_definition(&self.backend, id)
    }

    /// 智能体记忆：模型读了一次某条记忆。`scope` 见 [`memory_scope_for_agent`]。
    pub fn record_memory_read(
        &self,
        scope: &str,
        file_name: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        imp::record_memory_read(&self.backend, scope, file_name, now_ms)
    }

    /// 智能体记忆：一次成功写入的来源。
    pub fn record_memory_write(
        &self,
        scope: &str,
        file_name: &str,
        trust: MemoryTrust,
        reason: &str,
        session_id: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        imp::record_memory_write(
            &self.backend,
            scope,
            file_name,
            trust,
            reason,
            session_id,
            now_ms,
        )
    }

    /// 用户采纳一条待确认的记忆。返回是否真有一条待确认被改成可信。
    pub fn approve_memory(&self, scope: &str, file_name: &str) -> Result<bool, StoreError> {
        imp::approve_memory(&self.backend, scope, file_name)
    }

    /// 删掉一条记忆的元数据（文件已删，或同名重建时不该继承）。
    pub fn forget_memory(&self, scope: &str, file_name: &str) -> Result<(), StoreError> {
        imp::forget_memory(&self.backend, scope, file_name)
    }

    pub fn memory_meta(&self, scope: &str) -> Result<MemoryMetaSnapshot, StoreError> {
        imp::memory_meta(&self.backend, scope)
    }

    pub fn list_paired_devices(&self) -> Result<Vec<PairedDevice>, StoreError> {
        imp::list_paired_devices(&self.backend)
    }

    pub fn paired_device(&self, endpoint_id: &str) -> Result<Option<PairedDevice>, StoreError> {
        imp::paired_device(&self.backend, endpoint_id)
    }

    /// 配对成功：记进白名单。同一台设备重新配对就覆盖。
    pub fn put_paired_device(&self, device: &PairedDevice) -> Result<(), StoreError> {
        imp::put_paired_device(&self.backend, device)
    }

    /// 撤销。返回是否真有这台设备。
    pub fn delete_paired_device(&self, endpoint_id: &str) -> Result<bool, StoreError> {
        imp::delete_paired_device(&self.backend, endpoint_id)
    }

    pub fn touch_paired_device(&self, endpoint_id: &str, now_ms: i64) -> Result<(), StoreError> {
        imp::touch_paired_device(&self.backend, endpoint_id, now_ms)
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

    pub fn get_remote_config_snapshot(&self) -> Result<Option<RemoteConfigSnapshot>, StoreError> {
        imp::get_remote_config_snapshot(&self.backend)
    }

    pub fn put_remote_config_snapshot(
        &self,
        snapshot: &RemoteConfigSnapshot,
    ) -> Result<(), StoreError> {
        imp::put_remote_config_snapshot(&self.backend, snapshot)
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

    /// 模型目录由 Smelt 维护的 Pi 自定义 provider，按 id 排序。
    pub fn auto_model_providers(&self) -> Result<Vec<String>, StoreError> {
        imp::auto_model_providers(&self.backend)
    }

    pub fn put_auto_model_providers(&self, providers: &[String]) -> Result<(), StoreError> {
        imp::put_auto_model_providers(&self.backend, providers)
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
    use super::{HashSet, OpenSource, STORE_SCHEMA_VERSION, StoreError};
    use rusqlite::{
        Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior, params,
    };
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
    use std::time::{Duration, Instant};

    const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
    const BUSY_RETRY_DELAY: Duration = Duration::from_millis(10);

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
        if matches!(mode, OpenMode::OpenOrCreate) {
            return open_or_create_backend(path);
        }
        let presence = super::inspect_database_presence(path);
        match (mode, presence) {
            (OpenMode::Existing, super::DatabasePresence::Ready) => open_existing_backend(path),
            (OpenMode::Existing, super::DatabasePresence::Absent) => {
                Err(super::missing_database_error(path))
            }
            (OpenMode::CreateNew, super::DatabasePresence::Absent) => create_new_backend(path),
            (OpenMode::CreateNew, super::DatabasePresence::Ready) => {
                Err(StoreError::AlreadyExists {
                    path: path.to_path_buf(),
                })
            }
            (_, super::DatabasePresence::OrphanSidecars) => Err(super::orphan_sidecar_error(path)),
            (OpenMode::OpenOrCreate, _) => unreachable!("open_or_create 走独立路径"),
        }
    }

    /// 并发首次建库不用等：建库方在旁边的临时文件里把 schema 建完，才挂到正式路径上
    /// （见 [`publish_new_database`]），正式路径上不会出现建了一半的库。抢输的一方拿到
    /// `AlreadyExists`，直接打开对方建好的那份。
    fn open_or_create_backend(path: &Path) -> Result<Backend, StoreError> {
        match super::inspect_database_presence(path) {
            super::DatabasePresence::Ready => open_existing_backend(path),
            super::DatabasePresence::Absent => match create_new_backend(path) {
                Err(StoreError::AlreadyExists { .. }) => open_existing_backend(path),
                other => other,
            },
            super::DatabasePresence::OrphanSidecars => Err(super::orphan_sidecar_error(path)),
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
        publish_new_database(path)?;
        open_existing_backend(path)
    }

    /// 在同目录的临时文件里建好当前 schema，再用硬链接挂到 `path`。
    ///
    /// 直接在 `path` 上建库时，从 SQLite 创建空文件到建库方拿到写锁之间有一段空窗：别的
    /// 进程这时把路径看成已有库，打开后只看到空 schema，又不许 bootstrap，只能报
    /// `UninitializedDatabase` 或者反复重试。硬链接不覆盖已有文件，`path` 一出现就是建完的库；
    /// 已经有人挂上了就报 `AlreadyExists`。临时库关闭时 SQLite 把 WAL checkpoint 回主文件并
    /// 删掉 sidecar，挂上去的只有一个完整的主文件。
    fn publish_new_database(path: &Path) -> Result<(), StoreError> {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(super::DATABASE_FILE_NAME);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let staging = path.with_file_name(format!(
            "{file_name}.creating-{}-{nonce}",
            std::process::id()
        ));
        let built = (|| {
            let mut connection = open_connection(&staging, OPEN_CREATE_FLAGS, true)?;
            initialize_schema(&mut connection, true, &staging)?;
            // 挂上去的必须是自足的主文件：硬链接只带走主文件，内容还留在 `-wal` 里的话，
            // 对方打开看到的是空 schema。关闭时的 checkpoint 拿不到锁会悄悄跳过（Windows
            // CI 上撞见过），这里显式做完并核对。库保持 WAL：要是退回 DELETE 再挂上去，
            // 每个打开方都得抢排它锁切回 WAL，并发首次打开会撞 `database is locked`。
            let (busy, log, checkpointed): (i64, i64, i64) =
                connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
            if busy != 0 || (log != -1 && log != checkpointed) {
                return Err(std::io::Error::other(format!(
                    "新库没能把 WAL 写回主文件（busy={busy} log={log} checkpointed={checkpointed}）"
                ))
                .into());
            }
            connection.close().map_err(|(_, error)| StoreError::from(error))?;
            set_owner_only(&staging)
        })();
        let published = built.and_then(|()| match std::fs::hard_link(&staging, path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(StoreError::AlreadyExists {
                    path: path.to_path_buf(),
                })
            }
            Err(error) => Err(error.into()),
        });
        // 挂上了的话正式路径是另一个目录项，删临时名不影响它。
        let _ = std::fs::remove_file(&staging);
        for suffix in ["-wal", "-shm", "-journal"] {
            let _ = std::fs::remove_file(super::database_sidecar_path(&staging, suffix));
        }
        published
    }

    fn lock_connection(backend: &Backend) -> Result<MutexGuard<'_, Connection>, StoreError> {
        backend
            .connection
            .lock()
            .map_err(|_| StoreError::ConnectionPoisoned {
                path: backend.path.clone(),
            })
    }

    fn read<T>(
        backend: &Backend,
        read: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let connection = lock_connection(backend)?;
        read(&connection)
    }

    fn write<T>(
        backend: &Backend,
        write: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = write(&transaction)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(result)
    }

    pub(super) fn runtime_id(backend: &Backend) -> Result<Option<String>, StoreError> {
        read(backend, crate::config::read_runtime_id)
    }

    pub(super) fn checkpoint_wal_truncate(backend: &Backend) -> Result<(), StoreError> {
        let connection = lock_connection(backend)?;
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    pub(super) fn put_runtime_id(backend: &Backend, runtime_id: &str) -> Result<(), StoreError> {
        write(backend, |transaction| {
            crate::config::write_runtime_id(transaction, runtime_id)
        })
    }

    pub(super) fn workspace_catalog_revision(backend: &Backend) -> Result<u64, StoreError> {
        read(backend, crate::config::read_workspace_catalog_revision)
    }

    pub(super) fn commit_workspace_snapshot(
        backend: &Backend,
        base_revision: u64,
        writer: &super::WorkspaceCatalogWriter,
        snapshot: &super::WorkspaceSnapshot,
    ) -> Result<super::WorkspaceCatalogCommit, StoreError> {
        write(backend, |transaction| {
            let head = crate::config::read_workspace_catalog_head(transaction)?;
            if !head.accepts(base_revision, writer) {
                return Ok(super::WorkspaceCatalogCommit::Conflict {
                    current: head.revision,
                });
            }
            crate::workspace::write_workspace_snapshot(transaction, snapshot)?;
            let revision = head.revision + 1;
            crate::config::write_workspace_catalog_head(transaction, revision, writer)?;
            Ok(super::WorkspaceCatalogCommit::Committed { revision })
        })
    }

    pub(super) fn history_agent_binding(
        backend: &Backend,
        agent: &str,
        profile_id: &str,
        resume_id: &str,
    ) -> Result<Option<String>, StoreError> {
        read(backend, |connection| {
            crate::history::read_history_agent_binding(connection, agent, profile_id, resume_id)
        })
    }

    pub(super) fn history_agent_bindings(
        backend: &Backend,
        agent: &str,
        profile_id: &str,
    ) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
        read(backend, |connection| {
            crate::history::read_history_agent_bindings(connection, agent, profile_id)
        })
    }

    pub(super) fn put_history_agent_binding(
        backend: &Backend,
        agent: &str,
        profile_id: &str,
        resume_id: &str,
        agent_definition_id: &str,
    ) -> Result<(), StoreError> {
        write(backend, |transaction| {
            crate::history::write_history_agent_binding(
                transaction,
                agent,
                profile_id,
                resume_id,
                agent_definition_id,
            )
        })
    }

    pub(super) fn default_conversation_agent(
        backend: &Backend,
    ) -> Result<Option<String>, StoreError> {
        read(backend, crate::config::read_default_conversation_agent)
    }

    pub(super) fn put_default_conversation_agent(
        backend: &Backend,
        agent_definition_id: Option<&str>,
    ) -> Result<(), StoreError> {
        write(backend, |transaction| {
            crate::config::write_default_conversation_agent(transaction, agent_definition_id)
        })
    }

    pub(super) fn new_session_pins(backend: &Backend) -> Result<Vec<String>, StoreError> {
        read(backend, crate::config::read_new_session_pins)
    }

    pub(super) fn put_new_session_pins(
        backend: &Backend,
        pins: &[String],
    ) -> Result<(), StoreError> {
        write(backend, |transaction| {
            crate::config::write_new_session_pins(transaction, pins)
        })
    }

    pub(super) fn memory_settings(
        backend: &Backend,
    ) -> Result<super::MemorySettingsRecord, StoreError> {
        read(backend, crate::config::read_memory_settings)
    }

    pub(super) fn modify_memory_settings<E: From<StoreError>>(
        backend: &Backend,
        modify: impl FnOnce(&mut super::MemorySettingsRecord) -> Result<(), E>,
    ) -> Result<super::MemorySettingsRecord, E> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::from)?;
        let mut record = crate::config::read_memory_settings(&transaction)?;
        modify(&mut record)?;
        crate::config::write_memory_settings(&transaction, &record)?;
        transaction.commit().map_err(StoreError::from)?;
        secure_sidecar_permissions(&backend.path);
        Ok(record)
    }

    pub(super) fn auto_model_providers(backend: &Backend) -> Result<Vec<String>, StoreError> {
        read(backend, |connection| {
            crate::config::read_auto_model_providers(connection)
        })
    }

    pub(super) fn put_auto_model_providers(
        backend: &Backend,
        providers: &[String],
    ) -> Result<(), StoreError> {
        write(backend, |transaction| {
            crate::config::write_auto_model_providers(transaction, providers)
        })
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
        crate::automation::read_automation_snapshot(&connection)
    }

    pub(super) fn put_automation_snapshot(
        backend: &Backend,
        snapshot: &super::AutomationSnapshot,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::automation::write_automation_snapshot(&transaction, snapshot)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn get_launch_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::LaunchSnapshot>, StoreError> {
        let connection = lock_connection(backend)?;
        crate::launch::read_launch_snapshot(&connection)
    }

    pub(super) fn put_launch_snapshot(
        backend: &Backend,
        snapshot: &super::LaunchSnapshot,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::launch::write_launch_snapshot(&transaction, snapshot)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn get_history_title_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::HistoryTitleSnapshot>, StoreError> {
        let connection = lock_connection(backend)?;
        crate::history::read_history_title_snapshot(&connection)
    }

    pub(super) fn put_history_title_snapshot(
        backend: &Backend,
        snapshot: &super::HistoryTitleSnapshot,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::history::write_history_title_snapshot(&transaction, snapshot)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn get_workspace_snapshot(
        backend: &Backend,
    ) -> Result<Option<super::WorkspaceSnapshot>, StoreError> {
        let connection = lock_connection(backend)?;
        crate::workspace::read_workspace_snapshot(&connection)
    }

    pub(super) fn session_agent_definition_ids(
        backend: &Backend,
    ) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
        let connection = lock_connection(backend)?;
        crate::workspace::read_session_agent_definition_ids(&connection)
    }

    pub(super) fn put_workspace_snapshot(
        backend: &Backend,
        snapshot: &super::WorkspaceSnapshot,
    ) -> Result<(), StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::workspace::write_workspace_snapshot(&transaction, snapshot)?;
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(())
    }

    pub(super) fn remember_history_session_id(
        backend: &Backend,
        session_id: &str,
        history_id: &str,
    ) -> Result<bool, StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated =
            crate::workspace::remember_history_session_id(&transaction, session_id, history_id)?;
        transaction.commit()?;
        Ok(updated)
    }

    pub(super) fn history_session_id(
        backend: &Backend,
        session_id: &str,
    ) -> Result<Option<String>, StoreError> {
        let connection = lock_connection(backend)?;
        crate::workspace::history_session_id(&connection, session_id)
    }

    pub(super) fn stored_history_identity(
        backend: &Backend,
        session_id: &str,
    ) -> Result<super::StoredHistoryIdentity, StoreError> {
        let connection = lock_connection(backend)?;
        crate::workspace::stored_history_identity(&connection, session_id)
    }

    pub(super) fn clear_history_session_id(
        backend: &Backend,
        session_id: &str,
    ) -> Result<bool, StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = crate::workspace::clear_history_session_id(&transaction, session_id)?;
        transaction.commit()?;
        Ok(updated)
    }

    pub(super) fn workspace_snapshot_from_json(
        raw: &[u8],
    ) -> Result<super::WorkspaceSnapshot, StoreError> {
        let value: serde_json::Value = serde_json::from_slice(raw)?;
        crate::workspace_document::workspace_from_document(&value)
    }

    pub(super) fn workspace_snapshot_to_json(
        snapshot: &super::WorkspaceSnapshot,
    ) -> Result<Vec<u8>, StoreError> {
        let value = crate::workspace_document::json_from_workspace_snapshot(snapshot);
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

    pub(super) fn modify_agent_definition<E: From<StoreError>>(
        backend: &Backend,
        id: &str,
        modify: impl FnOnce(&mut super::AgentDefinitionRecord) -> Result<(), E>,
    ) -> Result<Option<super::AgentDefinitionRecord>, E> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::from)?;
        let Some(mut record) = crate::catalog::query_agent_definition(&transaction, id)? else {
            return Ok(None);
        };
        modify(&mut record)?;
        if record.id != id {
            return Err(StoreError::AgentDefinitionIdChanged {
                from: id.to_string(),
                to: record.id,
            }
            .into());
        }
        crate::catalog::update_agent_definition(&transaction, &record)?;
        transaction.commit().map_err(StoreError::from)?;
        secure_sidecar_permissions(&backend.path);
        Ok(Some(record))
    }

    pub(super) fn delete_agent_definition(backend: &Backend, id: &str) -> Result<bool, StoreError> {
        let mut connection = lock_connection(backend)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = crate::catalog::delete_agent_definition(&transaction, id)?;
        if changed {
            // 记忆跟着智能体走：定义删了，它的记忆元数据一起删。
            crate::memory::forget_scope(&transaction, &super::memory_scope_for_agent(id))?;
        }
        transaction.commit()?;
        secure_sidecar_permissions(&backend.path);
        Ok(changed)
    }

    pub(super) fn record_memory_read(
        backend: &Backend,
        scope: &str,
        file_name: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        write(backend, |transaction| {
            crate::memory::record_read(transaction, scope, file_name, now_ms)
        })
    }

    pub(super) fn record_memory_write(
        backend: &Backend,
        scope: &str,
        file_name: &str,
        trust: super::MemoryTrust,
        reason: &str,
        session_id: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        write(backend, |transaction| {
            crate::memory::record_write(
                transaction,
                scope,
                file_name,
                trust,
                reason,
                session_id,
                now_ms,
            )
        })
    }

    pub(super) fn approve_memory(
        backend: &Backend,
        scope: &str,
        file_name: &str,
    ) -> Result<bool, StoreError> {
        write(backend, |transaction| {
            crate::memory::approve(transaction, scope, file_name)
        })
    }

    pub(super) fn forget_memory(
        backend: &Backend,
        scope: &str,
        file_name: &str,
    ) -> Result<(), StoreError> {
        write(backend, |transaction| {
            crate::memory::forget(transaction, scope, file_name)
        })
    }

    pub(super) fn memory_meta(
        backend: &Backend,
        scope: &str,
    ) -> Result<super::MemoryMetaSnapshot, StoreError> {
        let connection = lock_connection(backend)?;
        crate::memory::read_meta(&connection, scope)
    }

    pub(super) fn list_paired_devices(
        backend: &Backend,
    ) -> Result<Vec<super::PairedDevice>, StoreError> {
        let connection = lock_connection(backend)?;
        crate::devices::list(&connection)
    }

    pub(super) fn paired_device(
        backend: &Backend,
        endpoint_id: &str,
    ) -> Result<Option<super::PairedDevice>, StoreError> {
        let connection = lock_connection(backend)?;
        crate::devices::get(&connection, endpoint_id)
    }

    pub(super) fn put_paired_device(
        backend: &Backend,
        device: &super::PairedDevice,
    ) -> Result<(), StoreError> {
        write(backend, |transaction| {
            crate::devices::put(transaction, device)
        })
    }

    pub(super) fn delete_paired_device(
        backend: &Backend,
        endpoint_id: &str,
    ) -> Result<bool, StoreError> {
        write(backend, |transaction| {
            crate::devices::delete(transaction, endpoint_id)
        })
    }

    pub(super) fn touch_paired_device(
        backend: &Backend,
        endpoint_id: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        write(backend, |transaction| {
            crate::devices::touch(transaction, endpoint_id, now_ms)
        })
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
                    // 轮询例外：改 WAL 时 SQLite 先开读事务再升写锁，升级失败直接回 SQLITE_BUSY、不调 busy handler（见 sqlite3BtreeSetVersion，防死锁），没有能等的事件；周期 10ms，上限 5s。
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
        if current == 24 {
            apply_v25_cli_exited(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            // 没有冻结的 v24 形状：补上 cli_exited 后与当前形状逐字一致，就说明原来正是 v24。
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(current));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 23 {
            apply_v24_drop_deepseek_harness(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            // 没有冻结的 v23 形状：删完 DSH 数据、重建完 auto_model_provider 后与当前形状逐字一致，就说明原来正是 v23。
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(current));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 22 {
            apply_v23_drop_cross_agent_messaging(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            // 没有冻结的 v22 形状：删完跨 Agent 消息的表和列后与当前形状逐字一致，就说明原来正是 v22。
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(current));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 21 {
            apply_v22_conversation_terminals(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            // 没有冻结的 v21 形状：删完布局树、会话补完位置后与当前形状逐字一致，就说明原来正是 v21。
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(current));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 20 {
            apply_v21_layout_tabs(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            // 没有冻结的 v20 形状：重建完 layout_node 再升到当前版本后与当前形状逐字一致，就说明原来正是 v20。
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(current));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 19 {
            apply_v20_drop_worktree_inherit(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            // 没有冻结的 v19 形状：删完两张表、重建完 layout_node 后与当前形状逐字一致，就说明原来正是 v19。
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(current));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 18 {
            apply_v19_workspace_catalog_writer(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            // 没有冻结的 v18 形状：补完两列、删完两张表、重建完 layout_node 后与当前形状逐字一致，
            // 就说明原来正是 v18。
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(current));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 17 {
            if !schema_matches_v17(&transaction)? {
                return Err(unrecognized_schema_error(current));
            }
            apply_v18_runtime_tables(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 16 {
            if !schema_matches_v16(&transaction)? {
                return Err(unrecognized_schema_error(current));
            }
            apply_v17_paired_device(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 15 {
            if !schema_matches_v15(&transaction)? {
                return Err(unrecognized_schema_error(current));
            }
            apply_v16_agent_mcp_servers_default(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 14 {
            if !schema_matches_v14(&transaction)? {
                return Err(unrecognized_schema_error(current));
            }
            apply_v15_agent_mcp_servers(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 13 {
            if !schema_matches_v13(&transaction)? {
                return Err(unrecognized_schema_error(current));
            }
            apply_v14_agent_memory_switch(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
            }
            transaction.commit()?;
            return Ok(());
        }
        if current == 12 {
            if !schema_matches_v12(&transaction)? {
                return Err(unrecognized_schema_error(current));
            }
            apply_v13_memory_tables(&transaction)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
            if !schema_matches_current(&transaction)? {
                return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
            }
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
            crate::automation::rebuild_automation_run_transcript_fk(transaction)?;
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
        apply_v11_engine_kind_columns(transaction)
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
        if !table_columns(transaction, "agent_definition")?
            .iter()
            .any(|column| column == "model_provider")
        {
            transaction.execute_batch(crate::schema::MIGRATE_V11_TO_V12)?;
        }
        apply_v13_memory_tables(transaction)
    }

    fn apply_v13_memory_tables(transaction: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
        if !table_names(transaction)?
            .iter()
            .any(|table| table == "agent_memory_usage")
        {
            transaction.execute_batch(crate::schema::MIGRATE_V12_TO_V13)?;
        }
        apply_v14_agent_memory_switch(transaction)
    }

    fn apply_v14_agent_memory_switch(
        transaction: &rusqlite::Transaction<'_>,
    ) -> Result<(), StoreError> {
        if !table_columns(transaction, "agent_definition")?
            .iter()
            .any(|column| column == "memory_enabled")
        {
            transaction.execute_batch(crate::schema::MIGRATE_V13_TO_V14)?;
        }
        apply_v15_agent_mcp_servers(transaction)
    }

    fn apply_v15_agent_mcp_servers(
        transaction: &rusqlite::Transaction<'_>,
    ) -> Result<(), StoreError> {
        if !table_columns(transaction, "agent_definition")?
            .iter()
            .any(|column| column == "mcp_servers_json")
        {
            transaction.execute_batch(crate::schema::MIGRATE_V14_TO_V15)?;
        }
        apply_v16_agent_mcp_servers_default(transaction)
    }

    /// v15 建的列默认值是文本 `'[]'`，与 BLOB 列类型不一致；v16 改成 `X'5B5D'`。
    /// 看列的默认值决定要不要重建，已经是 BLOB 默认值就跳过。
    fn apply_v16_agent_mcp_servers_default(
        transaction: &rusqlite::Transaction<'_>,
    ) -> Result<(), StoreError> {
        let default: Option<String> = transaction.query_row(
            "SELECT dflt_value FROM pragma_table_info('agent_definition')
             WHERE name = 'mcp_servers_json'",
            [],
            |row| row.get(0),
        )?;
        if default.as_deref() == Some("'[]'") {
            transaction.execute_batch(crate::schema::MIGRATE_V15_TO_V16)?;
        }
        apply_v17_paired_device(transaction)
    }

    fn apply_v17_paired_device(transaction: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
        if !table_names(transaction)?
            .iter()
            .any(|table| table == "paired_device")
        {
            transaction.execute_batch(crate::schema::MIGRATE_V16_TO_V17)?;
        }
        apply_v18_runtime_tables(transaction)
    }

    /// v18 去掉通用 `kv`：先把运行时自己的数据读进内存，改完结构再写进新表。
    /// 已经没有 `kv` 的库（新形状的夹具）跳过这一步，接着补 v19。
    fn apply_v18_runtime_tables(transaction: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
        if table_names(transaction)?.iter().any(|table| table == "kv") {
            // 只改认得的 v17 形状；前面哪一步没收敛到 v17，就按认不出的库拒绝，整笔回滚。
            if !schema_matches_v17(transaction)? {
                return Err(unrecognized_schema_error(STORE_SCHEMA_VERSION));
            }
            let state = crate::migrate_v18::read_v17_state(transaction)?;
            transaction.execute_batch(crate::schema::MIGRATE_V17_TO_V18)?;
            crate::migrate_v18::write_v18_state(transaction, state)?;
        }
        apply_v19_workspace_catalog_writer(transaction)
    }

    /// v19 给工作区目录补上写入方两列。已经有这两列的库跳过，接着补 v20。
    fn apply_v19_workspace_catalog_writer(
        transaction: &rusqlite::Transaction<'_>,
    ) -> Result<(), StoreError> {
        if !table_columns(transaction, "workspace_catalog")?
            .iter()
            .any(|column| column == "writer")
        {
            transaction.execute_batch(crate::schema::MIGRATE_V18_TO_V19)?;
        }
        apply_v20_drop_worktree_inherit(transaction)
    }

    /// v20 删掉没人读的 worktree 继承两张表，接着补 v21。已经没有这两张表的库跳过；
    /// 只剩其中一张的库不认，交给后面的形状校验拒绝。
    fn apply_v20_drop_worktree_inherit(
        transaction: &rusqlite::Transaction<'_>,
    ) -> Result<(), StoreError> {
        let tables = table_names(transaction)?;
        if tables.iter().any(|table| table == "worktree_inherit")
            && tables
                .iter()
                .any(|table| table == "worktree_inherit_skip_pattern")
        {
            transaction.execute_batch(crate::schema::MIGRATE_V19_TO_V20)?;
        }
        apply_v21_layout_tabs(transaction)
    }

    /// v21 让布局树能记标签组：layout_node 整表重建。已经有 `active_index` 列的库跳过。
    fn apply_v21_layout_tabs(transaction: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
        if !table_columns(transaction, "layout_node")?
            .iter()
            .any(|column| column == "active_index")
        {
            transaction.execute_batch(crate::schema::MIGRATE_V20_TO_V21)?;
        }
        apply_v22_conversation_terminals(transaction)
    }

    /// v22 把终端分屏交给客户端：按旧布局树给对话里的会话排好位置，再删掉布局树和
    /// 「当前看的是哪个」。已经没有 layout_node 的库跳过。
    fn apply_v22_conversation_terminals(
        transaction: &rusqlite::Transaction<'_>,
    ) -> Result<(), StoreError> {
        if table_names(transaction)?
            .iter()
            .any(|table| table == "layout_node")
        {
            transaction.execute_batch(crate::schema::MIGRATE_V21_TO_V22_ADD_POSITION)?;
            for (session_id, position) in v21_session_positions(transaction)? {
                transaction.execute(
                    "UPDATE session SET position = ?1 WHERE id = ?2",
                    params![position, session_id],
                )?;
            }
            transaction.execute_batch(crate::schema::MIGRATE_V21_TO_V22_FINISH)?;
        }
        apply_v23_drop_cross_agent_messaging(transaction)
    }

    /// v23 删掉跨 Agent 消息的投递记账表和开关列。已经没有记账表的库跳过。
    fn apply_v23_drop_cross_agent_messaging(
        transaction: &rusqlite::Transaction<'_>,
    ) -> Result<(), StoreError> {
        if table_names(transaction)?
            .iter()
            .any(|table| table == "peer_message_command")
        {
            transaction.execute_batch(crate::schema::MIGRATE_V22_TO_V23)?;
        }
        apply_v24_drop_deepseek_harness(transaction)
    }

    /// v24 删掉 DeepSeek Harness 的全部数据，`auto_model_provider` 去掉 `harness` 列。
    /// 已经没有 `harness` 列的库跳过。
    fn apply_v24_drop_deepseek_harness(
        transaction: &rusqlite::Transaction<'_>,
    ) -> Result<(), StoreError> {
        if table_columns(transaction, "auto_model_provider")?
            .iter()
            .any(|column| column == "harness")
        {
            transaction.execute_batch(crate::schema::MIGRATE_V23_TO_V24)?;
        }
        apply_v25_cli_exited(transaction)
    }

    /// v25 记下 CLI 是否已经退出。对话 id 退出后仍留着，续接与否看这一列。
    /// 已经有 `cli_exited` 列的库跳过。
    fn apply_v25_cli_exited(transaction: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
        if !table_columns(transaction, "published_session")?
            .iter()
            .any(|column| column == "cli_exited")
        {
            transaction.execute_batch(crate::schema::MIGRATE_V24_TO_V25)?;
        }
        Ok(())
    }

    /// 旧库里每段对话的会话顺序：ACP 会话在前，终端按布局树深度优先，布局树里没挂上的
    /// 终端（正常不会有）排在最后。
    fn v21_session_positions(
        transaction: &rusqlite::Transaction<'_>,
    ) -> Result<Vec<(String, i64)>, StoreError> {
        use std::collections::HashMap;

        let mut leaf_session: HashMap<String, String> = HashMap::new();
        let mut children: HashMap<String, Vec<(i64, String)>> = HashMap::new();
        let mut roots = Vec::new();
        {
            let mut statement = transaction.prepare(
                "SELECT id, parent_id, position, session_id FROM layout_node ORDER BY id",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?;
            for row in rows {
                let (id, parent_id, position, session_id) = row?;
                if let Some(session_id) = session_id {
                    leaf_session.insert(id.clone(), session_id);
                }
                match parent_id {
                    Some(parent_id) => children.entry(parent_id).or_default().push((position, id)),
                    None => roots.push(id),
                }
            }
        }
        let mut layout_rank: HashMap<String, usize> = HashMap::new();
        for root in roots {
            let mut stack = vec![root];
            while let Some(node) = stack.pop() {
                if let Some(session_id) = leaf_session.get(&node) {
                    let rank = layout_rank.len();
                    layout_rank.entry(session_id.clone()).or_insert(rank);
                }
                if let Some(kids) = children.get_mut(&node) {
                    kids.sort_by_key(|(position, _)| *position);
                    stack.extend(kids.iter().rev().map(|(_, id)| id.clone()));
                }
            }
        }

        // (不是 ACP, 布局树里的序, 落库顺序, id)：排序后就是对话里的位置。
        let mut by_group: HashMap<String, Vec<(bool, usize, usize, String)>> = HashMap::new();
        {
            let mut statement =
                transaction.prepare("SELECT id, group_id, kind FROM session ORDER BY rowid")?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for (index, row) in rows.enumerate() {
                let (id, group_id, kind) = row?;
                let rank = layout_rank.get(&id).copied().unwrap_or(usize::MAX);
                by_group
                    .entry(group_id)
                    .or_default()
                    .push((kind != "acp", rank, index, id));
            }
        }
        let mut positions = Vec::new();
        for (_, mut sessions) in by_group {
            sessions.sort();
            positions.extend(
                sessions
                    .into_iter()
                    .enumerate()
                    .map(|(position, (_, _, _, id))| (id, position as i64)),
            );
        }
        Ok(positions)
    }

    fn unrecognized_schema_error(version: u32) -> StoreError {
        StoreError::unrecognized_schema(version)
    }

    fn schema_matches_current(connection: &Connection) -> Result<bool, StoreError> {
        Ok(
            table_columns(connection, "acp_detail")? == CURRENT_ACP_COLUMNS
                && table_columns(connection, "automation_run_transcript")?
                    == ["run_id", "entries_json", "updated_at_ms"]
                && schema_matches_shape_outside_acp_table(
                    connection,
                    &crate::schema::TABLES,
                    false,
                )?
                && schema_objects(connection)?
                    == expected_schema_objects(SchemaGeneration::Current)?,
        )
    }

    fn schema_matches_v17(connection: &Connection) -> Result<bool, StoreError> {
        Ok(table_columns(connection, "acp_detail")? == V17_ACP_COLUMNS
            && table_columns(connection, "automation_run_transcript")?
                == ["run_id", "entries_json", "updated_at_ms"]
            && schema_matches_shape_outside_acp_table(
                connection,
                &crate::schema::V17_TABLES,
                true,
            )?
            && schema_objects(connection)? == expected_schema_objects(SchemaGeneration::V17)?)
    }

    fn schema_matches_v16(connection: &Connection) -> Result<bool, StoreError> {
        Ok(table_columns(connection, "acp_detail")? == V17_ACP_COLUMNS
            && table_columns(connection, "automation_run_transcript")?
                == ["run_id", "entries_json", "updated_at_ms"]
            && schema_matches_shape_outside_acp_table(
                connection,
                &crate::schema::V16_TABLES,
                true,
            )?
            && schema_objects(connection)? == expected_schema_objects(SchemaGeneration::V16)?)
    }

    fn schema_matches_v15(connection: &Connection) -> Result<bool, StoreError> {
        Ok(table_columns(connection, "acp_detail")? == V17_ACP_COLUMNS
            && table_columns(connection, "automation_run_transcript")?
                == ["run_id", "entries_json", "updated_at_ms"]
            && schema_matches_shape_outside_acp_table(
                connection,
                &crate::schema::V16_TABLES,
                true,
            )?
            && schema_objects(connection)? == expected_schema_objects(SchemaGeneration::V15)?)
    }

    fn schema_matches_v14(connection: &Connection) -> Result<bool, StoreError> {
        Ok(table_columns(connection, "acp_detail")? == V17_ACP_COLUMNS
            && table_columns(connection, "automation_run_transcript")?
                == ["run_id", "entries_json", "updated_at_ms"]
            && schema_matches_shape_outside_acp_table(
                connection,
                &crate::schema::V16_TABLES,
                true,
            )?
            && schema_objects(connection)? == expected_schema_objects(SchemaGeneration::V14)?)
    }

    fn schema_matches_v13(connection: &Connection) -> Result<bool, StoreError> {
        Ok(table_columns(connection, "acp_detail")? == V17_ACP_COLUMNS
            && table_columns(connection, "automation_run_transcript")?
                == ["run_id", "entries_json", "updated_at_ms"]
            && schema_matches_shape_outside_acp_table(
                connection,
                &crate::schema::V16_TABLES,
                true,
            )?
            && schema_objects(connection)? == expected_schema_objects(SchemaGeneration::V13)?)
    }

    fn schema_matches_v12(connection: &Connection) -> Result<bool, StoreError> {
        Ok(table_columns(connection, "acp_detail")? == V17_ACP_COLUMNS
            && table_columns(connection, "automation_run_transcript")?
                == ["run_id", "entries_json", "updated_at_ms"]
            && schema_matches_shape_outside_acp_table(
                connection,
                &crate::schema::V12_TABLES,
                true,
            )?
            && schema_objects(connection)? == expected_schema_objects(SchemaGeneration::V12)?)
    }

    fn schema_matches_v11(connection: &Connection) -> Result<bool, StoreError> {
        Ok(table_columns(connection, "acp_detail")? == V17_ACP_COLUMNS
            && table_columns(connection, "automation_run_transcript")?
                == ["run_id", "entries_json", "updated_at_ms"]
            && schema_matches_shape_outside_acp_table(
                connection,
                &crate::schema::V12_TABLES,
                true,
            )?
            && schema_objects(connection)? == expected_schema_objects(SchemaGeneration::V11)?)
    }

    fn schema_matches_v10(connection: &Connection) -> Result<bool, StoreError> {
        Ok(table_columns(connection, "acp_detail")? == V17_ACP_COLUMNS
            && table_columns(connection, "automation_run_transcript")?
                == ["run_id", "entries_json", "updated_at_ms"]
            && schema_matches_shape_outside_acp_table(
                connection,
                &crate::schema::V12_TABLES,
                true,
            )?
            && schema_objects(connection)? == expected_schema_objects(SchemaGeneration::V10)?)
    }

    /// v5 与 v6 只差 `acp_detail` 一张表，其余结构必须完全一致才允许就地重建。
    fn schema_matches_v5(connection: &Connection) -> Result<bool, StoreError> {
        Ok(
            table_columns(connection, "acp_detail")? == LEGACY_ACP_COLUMNS
                && schema_matches_shape_outside_acp_table(
                    connection,
                    &crate::schema::V7_TABLES,
                    true,
                )?
                && schema_objects_without_acp_table(connection)?
                    == expected_schema_objects(SchemaGeneration::V7WithoutAcpTable)?,
        )
    }

    /// v6 与 v7 同样只差 `acp_detail` 一张表。
    fn schema_matches_v6(connection: &Connection) -> Result<bool, StoreError> {
        Ok(table_columns(connection, "acp_detail")? == V6_ACP_COLUMNS
            && schema_matches_shape_outside_acp_table(connection, &crate::schema::V7_TABLES, true)?
            && schema_objects_without_acp_table(connection)?
                == expected_schema_objects(SchemaGeneration::V7WithoutAcpTable)?)
    }

    fn schema_matches_v7(connection: &Connection) -> Result<bool, StoreError> {
        Ok(table_columns(connection, "acp_detail")? == V17_ACP_COLUMNS
            && schema_matches_shape_outside_acp_table(connection, &crate::schema::V7_TABLES, true)?
            && schema_objects(connection)? == expected_schema_objects(SchemaGeneration::V7)?)
    }

    fn schema_matches_v9(connection: &Connection) -> Result<bool, StoreError> {
        Ok(table_columns(connection, "acp_detail")? == V17_ACP_COLUMNS
            && table_columns(connection, "automation_run_transcript")?
                == ["run_id", "entries_json", "updated_at_ms"]
            && schema_matches_shape_outside_acp_table(connection, &crate::schema::V9_TABLES, true)?
            && schema_objects(connection)? == expected_schema_objects(SchemaGeneration::V9)?)
    }

    fn schema_matches_v8(connection: &Connection) -> Result<bool, StoreError> {
        Ok(table_columns(connection, "acp_detail")? == V17_ACP_COLUMNS
            && table_columns(connection, "automation_run_transcript")?
                == ["run_id", "entries_json", "updated_at_ms"]
            && schema_matches_shape_outside_acp_table(connection, &crate::schema::V8_TABLES, true)?
            && schema_objects_excluding_transcript(connection)?
                == expected_schema_objects(SchemaGeneration::V8WithoutTranscript)?)
    }

    /// `legacy`：v17 及更早的形状，还有 `kv` 表和带 `collapsed` 列的 `project`。
    fn schema_matches_shape_outside_acp_table(
        connection: &Connection,
        expected_tables: &[&str],
        legacy: bool,
    ) -> Result<bool, StoreError> {
        let tables = table_names(connection)?;
        if tables != expected_tables || !schema_matches_shared_columns(connection, legacy)? {
            return Ok(false);
        }
        // 事件表和跨 Agent 消息表只在旧版本里有（v23 删掉），清单里有才比对。
        let event_tables: [(&str, &[&str]); 7] = [
            (
                "event_log",
                &[
                    "sequence",
                    "event_id",
                    "topic",
                    "aggregate_kind",
                    "aggregate_id",
                    "aggregate_revision",
                    "occurred_at_ms",
                    "envelope_json",
                ],
            ),
            (
                "event_outbox",
                &[
                    "outbox_id",
                    "event_id",
                    "envelope_json",
                    "created_at_ms",
                    "dispatched_sequence",
                ],
            ),
            (
                "subscription_cursor",
                &[
                    "plugin_id",
                    "subscription_id",
                    "last_acked_sequence",
                    "declaration_fingerprint",
                    "updated_at_ms",
                ],
            ),
            (
                "event_dead_letter",
                &[
                    "dead_letter_id",
                    "plugin_id",
                    "subscription_id",
                    "sequence",
                    "event_id",
                    "attempts",
                    "reason",
                    "failed_at_ms",
                ],
            ),
            (
                "event_delivery_retry",
                &[
                    "plugin_id",
                    "subscription_id",
                    "sequence",
                    "event_id",
                    "attempts",
                    "next_attempt_at_ms",
                    "reason",
                ],
            ),
            (
                "command_dedupe",
                &["plugin_id", "command_id", "completed_at_ms", "result_json"],
            ),
            (
                "peer_message_command",
                &[
                    "source_session_id",
                    "command_id",
                    "request_fingerprint",
                    "completed_at_ms",
                    "message_id",
                    "target_session_id",
                ],
            ),
        ];
        for (table, columns) in event_tables {
            if expected_tables.contains(&table) && table_columns(connection, table)? != columns {
                return Ok(false);
            }
        }
        Ok(true)
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
                || acp_columns == V17_ACP_COLUMNS)
            && schema_matches_shared_columns(connection, true)?
            && v1_schema_objects(connection)? == expected_schema_objects(SchemaGeneration::V1)?)
    }

    const CURRENT_ACP_COLUMNS: [&str; 13] = [
        "session_id",
        "agent",
        "profile_id",
        "history_session_id",
        "launch_command",
        "refresh_launch_from_settings",
        "pending_prompt",
        "pending_delivery_id",
        "agent_definition_id",
        "automation_id",
        "binding_automation_run_id",
        "pending_agent_preset",
        "session_title",
    ];

    /// v7..v17 的 `acp_detail` 形状：还带着分叉列，会话的其余字段散在 `kv` 里。
    const V17_ACP_COLUMNS: [&str; 15] = [
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

    fn schema_matches_shared_columns(
        connection: &Connection,
        legacy: bool,
    ) -> Result<bool, StoreError> {
        let (kv_columns, project_columns): (&[&str], &[&str]) = if legacy {
            (
                &["scope", "key", "value", "value_type", "updated_at_ms"],
                &["root", "position", "collapsed"],
            )
        } else {
            (&[], &["root", "position"])
        };
        // v22 起布局树归客户端：没有 layout_node，会话多一列 `position`，会话组去掉
        // `active_session_id`。v17 及更早的形状里都还在。
        let (layout_node_columns, session_columns, session_group_columns): (
            &[&str],
            &[&str],
            &[&str],
        ) = if legacy {
            (
                &[
                    "id",
                    "group_id",
                    "parent_id",
                    "position",
                    "kind",
                    "axis",
                    "session_id",
                    "size_px",
                ],
                &[
                    "id",
                    "group_id",
                    "kind",
                    "cwd",
                    "custom_title",
                    "launch_label",
                    "launch_command",
                ],
                &[
                    "id",
                    "project_root",
                    "position",
                    "active_session_id",
                    "custom_title",
                    "last_updated_at",
                ],
            )
        } else {
            (
                &[],
                &[
                    "id",
                    "group_id",
                    "kind",
                    "cwd",
                    "custom_title",
                    "launch_label",
                    "launch_command",
                    "position",
                ],
                &[
                    "id",
                    "project_root",
                    "position",
                    "custom_title",
                    "last_updated_at",
                ],
            )
        };
        Ok(table_columns(connection, "history_title")?
            == [
                "agent",
                "profile_id",
                "resume_id",
                "custom_title",
                "updated_at_ms",
            ]
            && table_columns(connection, "kv")? == kv_columns
            && table_columns(connection, "launch_entry")?
                == ["position", "label", "command", "provider"]
            && table_columns(connection, "layout_node")? == layout_node_columns
            && table_columns(connection, "project")? == project_columns
            && table_columns(connection, "session")? == session_columns
            && table_columns(connection, "session_group")? == session_group_columns)
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

    fn is_v13_memory_schema_object(object: &SchemaObject) -> bool {
        matches!(
            object.table_name.as_str(),
            "agent_memory_provenance" | "agent_memory_usage"
        )
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
        V12,
        V13,
        V14,
        V15,
        V16,
        V17,
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
        // v16 及更早都没有 v17 的设备表，在各版本自己的差异之前统一去掉。
        let objects = map(schema_objects_of(crate::schema::V17_SCHEMA)?
            .into_iter()
            .filter(|object| object.table_name != "paired_device")
            .collect());
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
        static V12_OBJECTS: OnceLock<Vec<SchemaObject>> = OnceLock::new();
        static V13_OBJECTS: OnceLock<Vec<SchemaObject>> = OnceLock::new();
        static V14_OBJECTS: OnceLock<Vec<SchemaObject>> = OnceLock::new();
        static V15_OBJECTS: OnceLock<Vec<SchemaObject>> = OnceLock::new();
        static V16_OBJECTS: OnceLock<Vec<SchemaObject>> = OnceLock::new();
        static V17_OBJECTS: OnceLock<Vec<SchemaObject>> = OnceLock::new();
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
                            && !is_v13_memory_schema_object(object)
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
                                && !is_v13_memory_schema_object(object)
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
                                && !is_v13_memory_schema_object(object)
                        })
                        .collect()
                })
            }
            SchemaGeneration::V9 => cached_schema_objects(&V9_OBJECTS, |objects| {
                objects
                    .into_iter()
                    .filter(|object| {
                        !is_v10_catalog_schema_object(object)
                            && !is_v13_memory_schema_object(object)
                    })
                    .collect()
            }),
            SchemaGeneration::V10 => cached_schema_objects(&V10_OBJECTS, |objects| {
                let mut objects: Vec<_> = objects
                    .into_iter()
                    .filter(|object| !is_v13_memory_schema_object(object))
                    .collect();
                strip_agent_definition_mcp_column(&mut objects);
                strip_agent_definition_memory_column(&mut objects);
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
            SchemaGeneration::V11 => cached_schema_objects(&V11_OBJECTS, |objects| {
                let mut objects: Vec<_> = objects
                    .into_iter()
                    .filter(|object| !is_v13_memory_schema_object(object))
                    .collect();
                strip_agent_definition_mcp_column(&mut objects);
                strip_agent_definition_memory_column(&mut objects);
                strip_agent_definition_model_columns(&mut objects);
                objects
            }),
            SchemaGeneration::V12 => cached_schema_objects(&V12_OBJECTS, |objects| {
                let mut objects: Vec<_> = objects
                    .into_iter()
                    .filter(|object| !is_v13_memory_schema_object(object))
                    .collect();
                strip_agent_definition_mcp_column(&mut objects);
                strip_agent_definition_memory_column(&mut objects);
                objects
            }),
            SchemaGeneration::V13 => cached_schema_objects(&V13_OBJECTS, |mut objects| {
                strip_agent_definition_mcp_column(&mut objects);
                strip_agent_definition_memory_column(&mut objects);
                objects
            }),
            SchemaGeneration::V14 => cached_schema_objects(&V14_OBJECTS, |mut objects| {
                strip_agent_definition_mcp_column(&mut objects);
                objects
            }),
            SchemaGeneration::V15 => cached_schema_objects(&V15_OBJECTS, |mut objects| {
                for object in &mut objects {
                    if object.name == "agent_definition" {
                        object.sql = object
                            .sql
                            .replace(MCP_COLUMN_NORMALIZED, MCP_COLUMN_V15_NORMALIZED);
                    }
                }
                objects
            }),
            SchemaGeneration::V16 => cached_schema_objects(&V16_OBJECTS, |objects| objects),
            SchemaGeneration::V17 => frozen_schema_objects(&V17_OBJECTS, crate::schema::V17_SCHEMA),
            SchemaGeneration::Current => {
                frozen_schema_objects(&CURRENT_OBJECTS, crate::schema::CREATE_SCHEMA)
            }
        }
    }

    fn frozen_schema_objects(
        slot: &OnceLock<Vec<SchemaObject>>,
        schema: &str,
    ) -> Result<Vec<SchemaObject>, StoreError> {
        if let Some(objects) = slot.get() {
            return Ok(objects.clone());
        }
        let objects = schema_objects_of(schema)?;
        Ok(slot.get_or_init(|| objects.clone()).clone())
    }

    fn schema_objects_of(schema: &str) -> Result<Vec<SchemaObject>, StoreError> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(schema)?;
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

    /// `mcp_servers_json` 列定义去空白、转小写后的样子：当前（v16 起）和 v15。
    const MCP_COLUMN_NORMALIZED: &str = "mcp_servers_jsonblobnotnulldefaultx'5b5d',";
    const MCP_COLUMN_V15_NORMALIZED: &str = "mcp_servers_jsonblobnotnulldefault'[]',";

    fn strip_agent_definition_mcp_column(objects: &mut [SchemaObject]) {
        for object in objects {
            if object.name == "agent_definition" {
                object.sql = object.sql.replace(MCP_COLUMN_NORMALIZED, "");
            }
        }
    }

    fn strip_agent_definition_memory_column(objects: &mut [SchemaObject]) {
        const NEEDLE: &str = "memory_enabledintegernotnulldefault1check(memory_enabledin(0,1)),";
        for object in objects {
            if object.name == "agent_definition" {
                object.sql = object.sql.replace(NEEDLE, "");
            }
        }
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
    use super::{HashSet, StoreError};
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

    pub(super) fn runtime_id(_backend: &Backend) -> Result<Option<String>, StoreError> {
        unsupported()
    }
    pub(super) fn checkpoint_wal_truncate(_backend: &Backend) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn put_runtime_id(_backend: &Backend, _runtime_id: &str) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn workspace_catalog_revision(_backend: &Backend) -> Result<u64, StoreError> {
        unsupported()
    }
    pub(super) fn commit_workspace_snapshot(
        _backend: &Backend,
        _base_revision: u64,
        _writer: &super::WorkspaceCatalogWriter,
        _snapshot: &super::WorkspaceSnapshot,
    ) -> Result<super::WorkspaceCatalogCommit, StoreError> {
        unsupported()
    }
    pub(super) fn history_agent_binding(
        _backend: &Backend,
        _agent: &str,
        _profile_id: &str,
        _resume_id: &str,
    ) -> Result<Option<String>, StoreError> {
        unsupported()
    }
    pub(super) fn history_agent_bindings(
        _backend: &Backend,
        _agent: &str,
        _profile_id: &str,
    ) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
        unsupported()
    }
    pub(super) fn put_history_agent_binding(
        _backend: &Backend,
        _agent: &str,
        _profile_id: &str,
        _resume_id: &str,
        _agent_definition_id: &str,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn default_conversation_agent(
        _backend: &Backend,
    ) -> Result<Option<String>, StoreError> {
        unsupported()
    }
    pub(super) fn put_default_conversation_agent(
        _backend: &Backend,
        _agent_definition_id: Option<&str>,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn new_session_pins(_backend: &Backend) -> Result<Vec<String>, StoreError> {
        unsupported()
    }
    pub(super) fn put_new_session_pins(
        _backend: &Backend,
        _pins: &[String],
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn memory_settings(
        _backend: &Backend,
    ) -> Result<super::MemorySettingsRecord, StoreError> {
        unsupported()
    }
    pub(super) fn modify_memory_settings<E: From<StoreError>>(
        _backend: &Backend,
        _modify: impl FnOnce(&mut super::MemorySettingsRecord) -> Result<(), E>,
    ) -> Result<super::MemorySettingsRecord, E> {
        Err(StoreError::SqliteDisabledOnMobile.into())
    }
    pub(super) fn auto_model_providers(_backend: &Backend) -> Result<Vec<String>, StoreError> {
        unsupported()
    }
    pub(super) fn put_auto_model_providers(
        _backend: &Backend,
        _providers: &[String],
    ) -> Result<(), StoreError> {
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
    pub(super) fn get_workspace_snapshot(
        _backend: &Backend,
    ) -> Result<Option<super::WorkspaceSnapshot>, StoreError> {
        unsupported()
    }
    pub(super) fn session_agent_definition_ids(
        _backend: &Backend,
    ) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
        unsupported()
    }
    pub(super) fn put_workspace_snapshot(
        _backend: &Backend,
        _snapshot: &super::WorkspaceSnapshot,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn remember_history_session_id(
        _backend: &Backend,
        _session_id: &str,
        _history_id: &str,
    ) -> Result<bool, StoreError> {
        unsupported()
    }
    pub(super) fn history_session_id(
        _backend: &Backend,
        _session_id: &str,
    ) -> Result<Option<String>, StoreError> {
        unsupported()
    }
    pub(super) fn stored_history_identity(
        _backend: &Backend,
        _session_id: &str,
    ) -> Result<super::StoredHistoryIdentity, StoreError> {
        unsupported()
    }
    pub(super) fn clear_history_session_id(
        _backend: &Backend,
        _session_id: &str,
    ) -> Result<bool, StoreError> {
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
    pub(super) fn modify_agent_definition<E: From<StoreError>>(
        _backend: &Backend,
        _id: &str,
        _modify: impl FnOnce(&mut super::AgentDefinitionRecord) -> Result<(), E>,
    ) -> Result<Option<super::AgentDefinitionRecord>, E> {
        Err(StoreError::SqliteDisabledOnMobile.into())
    }
    pub(super) fn delete_agent_definition(
        _backend: &Backend,
        _id: &str,
    ) -> Result<bool, StoreError> {
        unsupported()
    }
    pub(super) fn record_memory_read(
        _backend: &Backend,
        _scope: &str,
        _file_name: &str,
        _now_ms: i64,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn record_memory_write(
        _backend: &Backend,
        _scope: &str,
        _file_name: &str,
        _trust: super::MemoryTrust,
        _reason: &str,
        _session_id: &str,
        _now_ms: i64,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn approve_memory(
        _backend: &Backend,
        _scope: &str,
        _file_name: &str,
    ) -> Result<bool, StoreError> {
        unsupported()
    }
    pub(super) fn forget_memory(
        _backend: &Backend,
        _scope: &str,
        _file_name: &str,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn memory_meta(
        _backend: &Backend,
        _scope: &str,
    ) -> Result<super::MemoryMetaSnapshot, StoreError> {
        unsupported()
    }
    pub(super) fn list_paired_devices(
        _backend: &Backend,
    ) -> Result<Vec<super::PairedDevice>, StoreError> {
        unsupported()
    }
    pub(super) fn paired_device(
        _backend: &Backend,
        _endpoint_id: &str,
    ) -> Result<Option<super::PairedDevice>, StoreError> {
        unsupported()
    }
    pub(super) fn put_paired_device(
        _backend: &Backend,
        _device: &super::PairedDevice,
    ) -> Result<(), StoreError> {
        unsupported()
    }
    pub(super) fn delete_paired_device(
        _backend: &Backend,
        _endpoint_id: &str,
    ) -> Result<bool, StoreError> {
        unsupported()
    }
    pub(super) fn touch_paired_device(
        _backend: &Backend,
        _endpoint_id: &str,
        _now_ms: i64,
    ) -> Result<(), StoreError> {
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
}

#[cfg(all(test, not(any(target_os = "ios", target_os = "android"))))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> TempDir {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/smelt-store-tests")
            .join(format!("{name}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    /// 测试结束再删目录。库连接比这个值后声明，会先关掉；Windows 不能删还开着的数据库文件。
    struct TempDir {
        path: PathBuf,
    }

    impl std::ops::Deref for TempDir {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
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
    }

    /// v17 的完整结构（冻结的 `V17_SCHEMA`）。
    fn create_v17_schema(connection: &rusqlite::Connection) {
        connection.execute_batch(crate::schema::V17_SCHEMA).unwrap();
    }

    /// v16 的完整结构：v17 去掉设备表。更早版本的夹具在它上面再改。
    fn create_v16_schema(connection: &rusqlite::Connection) {
        connection.execute_batch(crate::schema::V17_SCHEMA).unwrap();
        connection
            .execute_batch("DROP TABLE paired_device;")
            .unwrap();
    }

    fn v1_schema() -> &'static str {
        static SCHEMA: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        SCHEMA
            .get_or_init(|| {
                let base = crate::schema::V17_SCHEMA
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
        let schema = crate::schema::V17_SCHEMA;
        let cut = schema
            .find("CREATE TABLE agent_ui_pref")
            .expect("v10 catalog tables follow automation tables in V17_SCHEMA");
        schema[..cut].to_string()
    }

    fn v10_schema() -> String {
        format!("{}\n{}", v9_schema(), crate::schema::MIGRATE_V9_TO_V10)
    }

    #[test]
    fn memory_metadata_round_trip_and_follows_the_agent() {
        let dir = temp_dir("memory-meta");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        let agent = memory_scope_for_agent("agent-1");
        store
            .insert_agent_definition(&AgentDefinitionRecord {
                id: "agent-1".into(),
                name: "写作".into(),
                description: String::new(),
                engine_kind_id: "pi".into(),
                prompt: String::new(),
                plugins_json: b"[]".to_vec(),
                mcp_servers_json: b"[]".to_vec(),
                context_folders_json: b"[]".to_vec(),
                context_links_json: b"[]".to_vec(),
                model_provider: String::new(),
                model_id: String::new(),
                memory_enabled: true,
            })
            .unwrap();

        store.record_memory_read(&agent, "fact.md", 1_000).unwrap();
        store.record_memory_read(&agent, "fact.md", 2_000).unwrap();
        store
            .record_memory_write(
                &agent,
                "rule.md",
                MemoryTrust::Pending,
                "外部内容",
                "s1",
                3_000,
            )
            .unwrap();
        store
            .record_memory_write(
                USER_MEMORY_SCOPE,
                "me.md",
                MemoryTrust::Pending,
                "外部内容",
                "s1",
                3_000,
            )
            .unwrap();
        let meta = store.memory_meta(&agent).unwrap();
        assert_eq!(
            meta.usage,
            [MemoryUsageRecord {
                file_name: "fact.md".into(),
                read_count: 2,
                last_read_at_ms: 2_000
            }]
        );
        assert_eq!(
            meta.pending,
            [("rule.md".to_string(), "外部内容".to_string())]
        );

        assert!(store.approve_memory(&agent, "rule.md").unwrap());
        assert!(
            !store.approve_memory(&agent, "rule.md").unwrap(),
            "已经可信的不算再采纳一次"
        );
        assert!(store.memory_meta(&agent).unwrap().pending.is_empty());

        store.forget_memory(&agent, "fact.md").unwrap();
        assert!(store.memory_meta(&agent).unwrap().usage.is_empty());

        store.record_memory_read(&agent, "fact.md", 4_000).unwrap();
        assert!(store.delete_agent_definition("agent-1").unwrap());
        assert_eq!(
            store.memory_meta(&agent).unwrap(),
            MemoryMetaSnapshot::default()
        );
        assert_eq!(
            store.memory_meta(USER_MEMORY_SCOPE).unwrap().pending.len(),
            1,
            "全局层不属于任何一个智能体"
        );
    }

    #[test]
    fn migrates_v13_by_adding_the_agent_memory_switch_defaulting_on() {
        let dir = temp_dir("migrate-v13-memory-switch");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        create_v16_schema(&connection);
        connection
            .execute_batch(
                "ALTER TABLE agent_definition RENAME TO agent_definition_v14;
                 CREATE TABLE agent_definition (
                   id TEXT PRIMARY KEY,
                   name TEXT NOT NULL,
                   description TEXT NOT NULL,
                   engine_kind_id TEXT NOT NULL,
                   prompt TEXT NOT NULL,
                   plugins_json BLOB NOT NULL,
                   context_folders_json BLOB NOT NULL,
                   context_links_json BLOB NOT NULL,
                   model_provider TEXT NOT NULL DEFAULT '',
                   model_id TEXT NOT NULL DEFAULT '',
                   position INTEGER NOT NULL CHECK (position >= 0)
                 );
                 DROP TABLE agent_definition_v14;
                 INSERT INTO agent_definition VALUES
                   ('writer', '写作', '', 'pi', '先列提纲', X'5B5D', X'5B5D', X'5B5D', 'p', 'm', 0);",
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 13).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        let agents = store.get_agent_ui_snapshot().unwrap().unwrap().agents;
        assert_eq!(agents[0].prompt, "先列提纲");
        assert_eq!(agents[0].model_id, "m");
        assert!(agents[0].memory_enabled, "已有智能体默认开着记忆");

        store
            .modify_agent_definition::<StoreError>("writer", |agent| {
                agent.memory_enabled = false;
                Ok(())
            })
            .unwrap()
            .unwrap();
        let agents = store.get_agent_ui_snapshot().unwrap().unwrap().agents;
        assert!(!agents[0].memory_enabled);
        drop(store);
        assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
    }

    #[test]
    fn migrates_v14_by_adding_agent_mcp_servers_defaulting_empty() {
        let dir = temp_dir("migrate-v14-mcp-servers");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        create_v16_schema(&connection);
        connection
            .execute_batch(
                "ALTER TABLE agent_definition RENAME TO agent_definition_v15;
                 CREATE TABLE agent_definition (
                   id TEXT PRIMARY KEY,
                   name TEXT NOT NULL,
                   description TEXT NOT NULL,
                   engine_kind_id TEXT NOT NULL,
                   prompt TEXT NOT NULL,
                   plugins_json BLOB NOT NULL,
                   context_folders_json BLOB NOT NULL,
                   context_links_json BLOB NOT NULL,
                   model_provider TEXT NOT NULL DEFAULT '',
                   model_id TEXT NOT NULL DEFAULT '',
                   memory_enabled INTEGER NOT NULL DEFAULT 1 CHECK (memory_enabled IN (0, 1)),
                   position INTEGER NOT NULL CHECK (position >= 0)
                 );
                 DROP TABLE agent_definition_v15;
                 INSERT INTO agent_definition VALUES
                   ('writer', '写作', '', 'pi', '先列提纲', X'5B5D', X'5B5D', X'5B5D', 'p', 'm', 0, 0);",
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 14).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        let agents = store.get_agent_ui_snapshot().unwrap().unwrap().agents;
        assert_eq!(agents[0].prompt, "先列提纲");
        assert!(!agents[0].memory_enabled, "迁移保留原有的记忆开关");
        assert_eq!(
            agents[0].mcp_servers_json,
            b"[]".to_vec(),
            "已有智能体默认不连任何 MCP 服务器"
        );

        store
            .modify_agent_definition::<StoreError>("writer", |agent| {
                agent.mcp_servers_json = br#"["github"]"#.to_vec();
                Ok(())
            })
            .unwrap()
            .unwrap();
        let agents = store.get_agent_ui_snapshot().unwrap().unwrap().agents;
        assert_eq!(agents[0].mcp_servers_json, br#"["github"]"#.to_vec());
        drop(store);
        assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
    }

    /// v16 的库：v17 多一张已配对设备表，再到 v18 把偏好搬进按字段的表。
    #[test]
    fn migrates_v16_through_paired_devices_to_typed_preferences() {
        let dir = temp_dir("migrate-v16-paired-device");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        create_v16_schema(&connection);
        connection
            .execute_batch(
                "INSERT INTO agent_ui_pref VALUES ('agent_hooks_enabled', '0');
                 INSERT INTO agent_ui_pref VALUES ('notify_success', '0');",
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 16).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        assert!(store.list_paired_devices().unwrap().is_empty());
        let prefs = store.get_agent_ui_snapshot().unwrap().unwrap();
        assert!(!prefs.agent_hooks_enabled);
        drop(store);

        assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
    }

    /// 照开发者真实 v16/v17 库的 `kv` 形状（已脱敏）造一份旧库：运行时自己的数据要落进新表，
    /// 客户端的界面状态和没人读的作用域随 `kv` 一起删掉。
    fn create_owner_kv_database(path: &Path, version: u32) {
        let connection = rusqlite::Connection::open(path).unwrap();
        if version == 16 {
            create_v16_schema(&connection);
        } else {
            create_v17_schema(&connection);
        }
        let kv = |scope: &str, key: &str, value_type: &str, value: Option<&str>| {
            connection
                .execute(
                    "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, 1)",
                    rusqlite::params![scope, key, value, value_type],
                )
                .unwrap();
        };
        let blob = |scope: &str, key: &str, value: &str| {
            connection
                .execute(
                    "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                     VALUES (?1, ?2, ?3, 'blob', 1)",
                    rusqlite::params![scope, key, value.as_bytes()],
                )
                .unwrap();
        };
        // 客户端的：外观、更新状态、侧栏、会话 route、旧客户端数据暂存、旧插件开关。
        kv("appearance", "$", "object", Some(""));
        kv("appearance", "theme_mode", "text", Some("dark"));
        kv("appearance", "opacity", "real", Some("0.95"));
        kv("update_state", "$", "object", Some(""));
        kv("update_state", "staged", "null", None);
        kv("workspace", "$", "object", Some(""));
        kv("workspace", "route", "text", Some("session"));
        kv("workspace", "sidebar_grouping", "text", Some("project"));
        kv("session_group:group:0:term-1", "route", "object", Some(""));
        kv(
            "session_group:group:0:term-1",
            "route/tool_panel_w",
            "real",
            Some("626.0"),
        );
        blob(
            "client_migration",
            "staged",
            r#"{"appearance":{"theme_mode":"dark"}}"#,
        );
        blob(
            "plugins",
            "enablement",
            r#"{"disabled":["com.example.browser"]}"#,
        );

        // 运行时的。
        blob(
            "runtime",
            "runtime_id",
            "4df5dd5c-434f-4c2c-b8da-38795277fb20",
        );
        blob("workspace_catalog", "revision", "174");
        blob("history_agent", "pi\u{1f}\u{1f}resume-1", "agent-a");
        blob("history_agent", "pi\u{1f}work\u{1f}resume-2", "agent-b");
        blob(
            "preferences",
            "default_conversation_agent",
            r#"{"agent_id":"agent-a"}"#,
        );
        blob(
            "preferences",
            "new_session_pins",
            r#"{"pinned":["conversation:agent:agent-a","terminal:claude:claude"]}"#,
        );
        blob("memory", "embedding_base_url", "http://localhost:11434/v1");
        blob("memory", "embedding_model", "bge-m3");
        kv("collab", "$", "object", Some(""));
        kv("collab", "enabled", "boolean", Some("1"));
        kv(
            "collab",
            "iroh_relay",
            "text",
            Some("https://relay.example"),
        );
        kv("worktree_inherit", "$", "object", Some(""));
        kv("worktree_inherit", "enabled", "boolean", Some("1"));
        kv("worktree_inherit", "skip_patterns", "array", Some(""));
        kv(
            "worktree_inherit",
            "skip_patterns/0",
            "text",
            Some("node_modules"),
        );
        kv("pi_auto_models", "$", "object", Some(""));
        kv("pi_auto_models", "providers", "array", Some(""));
        kv("pi_auto_models", "providers/0", "text", Some("deepseek"));
        kv("pi_auto_models", "providers/1", "text", Some("token-x"));
        kv("terminal_theme", "$", "object", Some(""));
        kv("terminal_theme", "version", "integer", Some("1"));
        kv("terminal_theme", "dark", "boolean", Some("1"));
        kv("terminal_theme", "background", "integer", Some("657930"));
        kv("terminal_theme", "foreground", "integer", Some("14211288"));
        kv("terminal_theme", "cursor", "integer", Some("14211288"));
        kv("terminal_theme", "selection", "integer", Some("802170"));
        kv("terminal_theme", "search_hit", "integer", Some("8018976"));
        kv(
            "terminal_theme",
            "search_hit_current",
            "integer",
            Some("13934615"),
        );
        kv("terminal_theme", "palette", "array", Some(""));
        // key 按字符串排是 0、1、10、2：顺序必须按下标还原。
        for index in 0..11 {
            kv(
                "terminal_theme",
                &format!("palette/{index}"),
                "integer",
                Some(&(100 + index).to_string()),
            );
        }
        kv("quota_cache", "$", "object", Some(""));
        kv(
            "quota_cache",
            "saved_at_ms",
            "integer",
            Some("1792250484008"),
        );
        kv("quota_cache", "schema_version", "integer", Some("3"));
        kv("quota_cache", "provider_ids", "array", Some(""));
        kv("quota_cache", "provider_ids/0", "text", Some("claude"));
        kv(
            "quota_cache",
            "provider_ids/1",
            "text",
            Some("pi:github-copilot"),
        );
        kv("quota_cache", "providers", "object", Some(""));
        kv("quota_cache", "providers/claude", "object", Some(""));
        kv(
            "quota_cache",
            "providers/claude/windows",
            "object",
            Some(""),
        );
        kv(
            "quota_cache",
            "providers/claude/windows/checked_at_ms",
            "integer",
            Some("1792250484008"),
        );
        kv(
            "quota_cache",
            "providers/claude/windows/error",
            "null",
            None,
        );
        kv(
            "quota_cache",
            "providers/claude/windows/quota",
            "object",
            Some(""),
        );
        kv(
            "quota_cache",
            "providers/claude/windows/quota/windows",
            "array",
            Some(""),
        );
        kv(
            "quota_cache",
            "providers/claude/windows/quota/windows/0",
            "object",
            Some(""),
        );
        kv(
            "quota_cache",
            "providers/claude/windows/quota/windows/0/label",
            "text",
            Some("5h"),
        );
        kv(
            "quota_cache",
            "providers/claude/windows/quota/windows/0/reset_after_seconds",
            "null",
            None,
        );
        kv(
            "quota_cache",
            "providers/claude/windows/quota/windows/0/resets_at_ms",
            "integer",
            Some("1792258200463"),
        );
        kv(
            "quota_cache",
            "providers/claude/windows/quota/windows/0/used_percent",
            "real",
            Some("37.0"),
        );
        kv(
            "quota_cache",
            "providers/pi:github-copilot",
            "object",
            Some(""),
        );
        kv(
            "quota_cache",
            "providers/pi:github-copilot/copilot",
            "object",
            Some(""),
        );
        kv(
            "quota_cache",
            "providers/pi:github-copilot/copilot/checked_at_ms",
            "integer",
            Some("1792250484008"),
        );
        kv(
            "quota_cache",
            "providers/pi:github-copilot/copilot/error",
            "null",
            None,
        );
        kv(
            "quota_cache",
            "providers/pi:github-copilot/copilot/quota",
            "object",
            Some(""),
        );
        kv(
            "quota_cache",
            "providers/pi:github-copilot/copilot/quota/limit",
            "real",
            Some("43000.0"),
        );
        kv(
            "quota_cache",
            "providers/pi:github-copilot/copilot/quota/remaining",
            "real",
            Some("11347.0"),
        );
        kv(
            "quota_cache",
            "providers/pi:github-copilot/copilot/quota/unit",
            "text",
            Some("AiCredits"),
        );
        kv(
            "quota_cache",
            "providers/pi:github-copilot/copilot/quota/used",
            "real",
            Some("31653.0"),
        );
        kv("workspace_menu", "$", "object", Some(""));
        kv("workspace_menu", "version", "integer", Some("2"));
        kv("workspace_menu", "revision", "integer", Some("12132"));
        kv("workspace_menu", "source_id", "text", Some("c8acdab2"));
        kv("workspace_menu", "source_revision", "integer", Some("133"));
        kv("workspace_menu", "projects", "array", Some(""));
        kv("workspace_menu", "projects/0", "object", Some(""));
        kv("workspace_menu", "projects/0/order", "integer", Some("0"));
        kv("workspace_menu", "projects/0/root", "text", Some("/repo"));
        kv("workspace_menu", "projects/0/title", "text", Some("repo"));
        kv("workspace_menu", "sessions", "array", Some(""));
        kv("workspace_menu", "sessions/0", "object", Some(""));
        kv("workspace_menu", "sessions/0/agent", "text", Some("claude"));
        kv(
            "workspace_menu",
            "sessions/0/custom_title",
            "boolean",
            Some("0"),
        );
        kv("workspace_menu", "sessions/0/cwd", "text", Some("/repo"));
        kv("workspace_menu", "sessions/0/id", "text", Some("term-1"));
        kv(
            "workspace_menu",
            "sessions/0/kind",
            "text",
            Some("terminal"),
        );
        kv(
            "workspace_menu",
            "sessions/0/leaf_order",
            "integer",
            Some("0"),
        );
        kv(
            "workspace_menu",
            "sessions/0/project_order",
            "integer",
            Some("0"),
        );
        kv(
            "workspace_menu",
            "sessions/0/project_root",
            "text",
            Some("/repo"),
        );
        kv(
            "workspace_menu",
            "sessions/0/project_title",
            "text",
            Some("repo"),
        );
        kv(
            "workspace_menu",
            "sessions/0/session_order",
            "integer",
            Some("0"),
        );
        kv(
            "workspace_menu",
            "sessions/0/title",
            "text",
            Some("你知道 Godot 吗"),
        );
        kv("launch", "version", "integer", Some("14"));
        // ACP 会话散在 `acp:<id>` 里的字段。
        kv("acp:acp-1", "env", "object", Some(""));
        kv(
            "acp:acp-1",
            "env/SMELT_AGENT_MEMORY_DIR",
            "text",
            Some("/home/me/memory"),
        );
        kv("acp:acp-1", "config_values", "array", Some(""));
        kv("acp:acp-1", "config_values/0", "array", Some(""));
        kv("acp:acp-1", "config_values/0/0", "text", Some("model"));
        kv(
            "acp:acp-1",
            "config_values/0/1",
            "text",
            Some("deepseek/flash"),
        );
        kv("acp:acp-1", "config_values/1", "array", Some(""));
        kv("acp:acp-1", "config_values/1/0", "text", Some("mode"));
        kv(
            "acp:acp-1",
            "config_values/1/1",
            "text",
            Some("bypassPermissions"),
        );
        kv("acp:acp-1", "conversation_binding", "object", Some(""));
        kv(
            "acp:acp-1",
            "conversation_binding/type",
            "text",
            Some("direct"),
        );
        kv("acp:acp-1", "session_title", "text", Some("季度复盘"));
        kv(
            "acp:gone",
            "session_title",
            "text",
            Some("目录里已经没有的会话"),
        );

        connection
            .execute_batch(
                "BEGIN;
                 INSERT INTO agent_ui_pref VALUES ('agent_hooks_enabled', '1');
                 INSERT INTO agent_ui_pref VALUES ('cross_agent_enabled', '0');
                 INSERT INTO agent_ui_pref VALUES ('notify_success', '0');
                 INSERT INTO project(root, position, collapsed) VALUES ('/repo', 0, 1);
                 INSERT INTO session_group(id, project_root, position, active_session_id)
                 VALUES ('group:0:term-1', '/repo', 0, 'term-1'),
                        ('group:1:acp-1', '/repo', 1, 'acp-1');
                 INSERT INTO session(id, group_id, kind, cwd)
                 VALUES ('term-1', 'group:0:term-1', 'terminal', '/repo'),
                        ('acp-1', 'group:1:acp-1', 'acp', '/repo');
                 INSERT INTO layout_node(id, group_id, parent_id, position, kind, session_id)
                 VALUES ('group:0:term-1:node:0', 'group:0:term-1', NULL, 0, 'leaf', 'term-1'),
                        ('group:1:acp-1:node:0', 'group:1:acp-1', NULL, 0, 'leaf', 'acp-1');
                 INSERT INTO acp_detail(
                   session_id, agent, history_session_id, launch_command,
                   refresh_launch_from_settings, agent_definition_id
                 ) VALUES ('acp-1', 'pi', 'pi-history-1', 'smelt-pi-agent', 1, 'agent-a');
                 COMMIT;",
            )
            .unwrap();
        connection
            .pragma_update(None, "user_version", version)
            .unwrap();
    }

    #[test]
    fn migrates_owner_kv_data_into_typed_tables() {
        for version in [16, 17] {
            let dir = temp_dir(&format!("migrate-owner-v{version}"));
            let path = dir.join(DATABASE_FILE_NAME);
            create_owner_kv_database(&path, version);

            let store = Store::open_or_create(&path).unwrap();
            assert_eq!(
                store.runtime_id().unwrap().as_deref(),
                Some("4df5dd5c-434f-4c2c-b8da-38795277fb20")
            );
            assert_eq!(store.workspace_catalog_revision().unwrap(), 174);
            assert_eq!(
                store
                    .history_agent_binding("pi", "", "resume-1")
                    .unwrap()
                    .as_deref(),
                Some("agent-a")
            );
            assert_eq!(
                store.history_agent_bindings("pi", "work").unwrap(),
                std::collections::BTreeMap::from([("resume-2".to_string(), "agent-b".to_string())])
            );
            assert_eq!(
                store.default_conversation_agent().unwrap().as_deref(),
                Some("agent-a")
            );
            assert_eq!(
                store.new_session_pins().unwrap(),
                ["conversation:agent:agent-a", "terminal:claude:claude"]
            );
            let memory = store.memory_settings().unwrap();
            assert_eq!(memory.embedding_base_url, "http://localhost:11434/v1");
            assert_eq!(memory.embedding_model, "bge-m3");
            assert_eq!(memory.embedding_api_key, "");
            assert_eq!(
                store.get_remote_config_snapshot().unwrap(),
                Some(RemoteConfigSnapshot {
                    enabled: true,
                    iroh_relay: "https://relay.example".into(),
                })
            );
            assert_eq!(
                store.auto_model_providers().unwrap(),
                ["deepseek", "token-x"]
            );
            let theme = store.get_terminal_theme_snapshot().unwrap().unwrap();
            assert_eq!(theme.background, 657930);
            assert_eq!(theme.search_hit_current, 13934615);
            assert_eq!(theme.palette, (100..111).collect::<Vec<u32>>());
            let quota = store.get_quota_cache_snapshot().unwrap().unwrap();
            assert_eq!(quota.saved_at_ms, 1792250484008);
            assert_eq!(quota.provider_ids, ["claude", "pi:github-copilot"]);
            assert_eq!(
                quota.statuses,
                [
                    QuotaCacheStatusRecord {
                        provider_id: "claude".into(),
                        checked_at_ms: 1792250484008,
                        error: None,
                        quota: QuotaCacheQuota::Windows(vec![QuotaCacheWindowRecord {
                            label: "5h".into(),
                            used_percent: 37.0,
                            resets_at_ms: Some(1792258200463),
                            reset_after_seconds: None,
                        }]),
                    },
                    QuotaCacheStatusRecord {
                        provider_id: "pi:github-copilot".into(),
                        checked_at_ms: 1792250484008,
                        error: None,
                        quota: QuotaCacheQuota::Copilot {
                            unit: "ai_credits".into(),
                            remaining: 11347.0,
                            limit: Some(43000.0),
                            used: Some(31653.0),
                        },
                    },
                ]
            );
            let menu = store.get_workspace_menu_snapshot().unwrap().unwrap();
            assert_eq!((menu.version, menu.revision), (2, 12132));
            assert_eq!(menu.projects[0].root, "/repo");
            assert_eq!(menu.sessions[0].title, "你知道 Godot 吗");
            assert_eq!(menu.sessions[0].kind, "terminal");
            assert_eq!(store.get_launch_snapshot().unwrap().unwrap().version, 14);
            let prefs = store.get_agent_ui_snapshot().unwrap().unwrap();
            assert!(prefs.agent_hooks_enabled);

            let workspace = store.get_workspace_snapshot().unwrap().unwrap();
            assert_eq!(workspace.projects, ["/repo"]);
            let acp = workspace.sessions[1].acp.as_ref().unwrap();
            assert_eq!(acp.sid.as_deref(), Some("acp-1"));
            assert_eq!(acp.history_session_id.as_deref(), Some("pi-history-1"));
            assert_eq!(acp.agent_definition_id.as_deref(), Some("agent-a"));
            assert_eq!(
                acp.launch_env,
                [(
                    "SMELT_AGENT_MEMORY_DIR".to_string(),
                    "/home/me/memory".to_string()
                )]
            );
            assert_eq!(
                acp.config_values,
                [
                    ("model".to_string(), "deepseek/flash".to_string()),
                    ("mode".to_string(), "bypassPermissions".to_string()),
                ]
            );
            assert_eq!(acp.binding_automation_run_id, None);
            assert_eq!(acp.session_title.as_deref(), Some("季度复盘"));
            drop(store);

            assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
            let connection = rusqlite::Connection::open(&path).unwrap();
            // worktree 继承随 git worktree 功能一起下线：v18 建过这两张表，v20 又删掉了。
            for table in [
                "kv",
                "agent_ui_pref",
                "acp_detail_v17",
                "worktree_inherit",
                "worktree_inherit_skip_pattern",
            ] {
                let exists: bool = connection
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = ?1)",
                        [table],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert!(!exists, "v{version}: {table} 应该已经删掉");
            }
            let project_columns = connection
                .prepare("PRAGMA table_info(project)")
                .unwrap()
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(project_columns, ["root", "position"], "折叠状态归客户端");
            drop(connection);
        }
    }

    #[test]
    fn paired_devices_round_trip_and_repairing_resets_last_seen() {
        let dir = temp_dir("paired-devices");
        let path = dir.join(DATABASE_FILE_NAME);
        let store = Store::open_or_create(&path).unwrap();
        let laptop = PairedDevice {
            endpoint_id: "aa".to_string(),
            name: "MacBook".to_string(),
            paired_at_ms: 10,
            last_seen_at_ms: None,
        };
        store.put_paired_device(&laptop).unwrap();
        store.touch_paired_device("aa", 20).unwrap();
        assert_eq!(
            store.paired_device("aa").unwrap().unwrap().last_seen_at_ms,
            Some(20)
        );

        store
            .put_paired_device(&PairedDevice {
                name: "MacBook Pro".to_string(),
                paired_at_ms: 30,
                ..laptop
            })
            .unwrap();
        let devices = store.list_paired_devices().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "MacBook Pro");
        assert_eq!(devices[0].last_seen_at_ms, None);

        assert!(store.delete_paired_device("aa").unwrap());
        assert!(!store.delete_paired_device("aa").unwrap());
        assert!(store.paired_device("aa").unwrap().is_none());
        drop(store);
    }

    /// v15 发出去的库：`mcp_servers_json` 默认值是文本 `'[]'`。升到 v16 后默认值改成 BLOB，
    /// 已勾选的 MCP 服务器和其余字段原样保留。
    #[test]
    fn migrates_v15_by_switching_mcp_servers_default_to_blob() {
        let dir = temp_dir("migrate-v15-mcp-default");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        create_v16_schema(&connection);
        connection
            .execute_batch(
                "DROP TABLE agent_definition;
                 CREATE TABLE agent_definition (
                   id TEXT PRIMARY KEY,
                   name TEXT NOT NULL,
                   description TEXT NOT NULL,
                   engine_kind_id TEXT NOT NULL,
                   prompt TEXT NOT NULL,
                   plugins_json BLOB NOT NULL,
                   mcp_servers_json BLOB NOT NULL DEFAULT '[]',
                   context_folders_json BLOB NOT NULL,
                   context_links_json BLOB NOT NULL,
                   model_provider TEXT NOT NULL DEFAULT '',
                   model_id TEXT NOT NULL DEFAULT '',
                   memory_enabled INTEGER NOT NULL DEFAULT 1 CHECK (memory_enabled IN (0, 1)),
                   position INTEGER NOT NULL CHECK (position >= 0)
                 );
                 INSERT INTO agent_definition VALUES
                   ('writer', '写作', '', 'pi', '先列提纲', X'5B5D', CAST('[\"figma\"]' AS BLOB),
                    X'5B5D', X'5B5D', 'p', 'm', 0, 0);",
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 15).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        let agents = store.get_agent_ui_snapshot().unwrap().unwrap().agents;
        assert_eq!(agents[0].prompt, "先列提纲");
        assert!(!agents[0].memory_enabled);
        assert_eq!(agents[0].mcp_servers_json, br#"["figma"]"#.to_vec());
        drop(store);

        assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
        let connection = rusqlite::Connection::open(&path).unwrap();
        let default: String = connection
            .query_row(
                "SELECT dflt_value FROM pragma_table_info('agent_definition')
                 WHERE name = 'mcp_servers_json'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(default, "X'5B5D'");
        drop(connection);
    }

    #[test]
    fn migrates_v12_by_adding_memory_tables() {
        let dir = temp_dir("migrate-v12-memory");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        create_v16_schema(&connection);
        connection
            .execute_batch(
                "DROP TABLE agent_memory_usage; DROP TABLE agent_memory_provenance;
                 DROP TABLE agent_definition;
                 CREATE TABLE agent_definition (
                   id TEXT PRIMARY KEY,
                   name TEXT NOT NULL,
                   description TEXT NOT NULL,
                   engine_kind_id TEXT NOT NULL,
                   prompt TEXT NOT NULL,
                   plugins_json BLOB NOT NULL,
                   context_folders_json BLOB NOT NULL,
                   context_links_json BLOB NOT NULL,
                   model_provider TEXT NOT NULL DEFAULT '',
                   model_id TEXT NOT NULL DEFAULT '',
                   position INTEGER NOT NULL CHECK (position >= 0)
                 );",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO project(root, position, collapsed) VALUES ('/repo', 0, 0)",
                [],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 12).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        let workspace = store.get_workspace_snapshot().unwrap().unwrap();
        assert_eq!(workspace.projects[0], "/repo");
        store.record_memory_read("user", "a.md", 1).unwrap();
        drop(store);
        assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
    }

    #[test]
    fn new_database_contains_exactly_the_current_tables() {
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
        assert!(
            !tables.iter().any(|table| table == "kv"),
            "运行时库不再有通用 kv 表"
        );

        drop(connection);
    }

    #[test]
    fn v2_database_migrates_to_current_with_a_backup() {
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
            0,
            "v2 建的跨 Agent 消息表在 v23 删掉"
        );
        drop(connection);
    }

    #[test]
    fn v22_database_drops_cross_agent_messaging_and_durable_events() {
        let dir = temp_dir("v22-migration");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(crate::schema::CREATE_SCHEMA)
            .unwrap();
        // 当前形状加回 v22 才有的跨 Agent 消息表、开关列和持久事件表，就是一个 v22 库。
        connection
            .execute_batch(
                "CREATE TABLE event_log (
                   sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                   event_id TEXT NOT NULL UNIQUE,
                   topic TEXT NOT NULL,
                   aggregate_kind TEXT,
                   aggregate_id TEXT,
                   aggregate_revision INTEGER,
                   occurred_at_ms INTEGER NOT NULL,
                   envelope_json BLOB NOT NULL
                 );
                 CREATE TABLE event_outbox (
                   outbox_id INTEGER PRIMARY KEY AUTOINCREMENT,
                   event_id TEXT NOT NULL UNIQUE,
                   envelope_json BLOB NOT NULL,
                   created_at_ms INTEGER NOT NULL,
                   dispatched_sequence INTEGER REFERENCES event_log(sequence)
                 );
                 CREATE TABLE subscription_cursor (
                   plugin_id TEXT NOT NULL,
                   subscription_id TEXT NOT NULL,
                   last_acked_sequence INTEGER NOT NULL DEFAULT 0,
                   declaration_fingerprint TEXT NOT NULL,
                   updated_at_ms INTEGER NOT NULL,
                   PRIMARY KEY (plugin_id, subscription_id)
                 ) WITHOUT ROWID;
                 CREATE TABLE event_dead_letter (
                   dead_letter_id INTEGER PRIMARY KEY AUTOINCREMENT,
                   plugin_id TEXT NOT NULL,
                   subscription_id TEXT NOT NULL,
                   sequence INTEGER NOT NULL,
                   event_id TEXT NOT NULL,
                   attempts INTEGER NOT NULL,
                   reason TEXT NOT NULL,
                   failed_at_ms INTEGER NOT NULL
                 );
                 CREATE TABLE event_delivery_retry (
                   plugin_id TEXT NOT NULL,
                   subscription_id TEXT NOT NULL,
                   sequence INTEGER NOT NULL REFERENCES event_log(sequence),
                   event_id TEXT NOT NULL,
                   attempts INTEGER NOT NULL,
                   next_attempt_at_ms INTEGER NOT NULL,
                   reason TEXT NOT NULL,
                   PRIMARY KEY (plugin_id, subscription_id),
                   FOREIGN KEY (plugin_id, subscription_id)
                     REFERENCES subscription_cursor(plugin_id, subscription_id) ON DELETE CASCADE
                 ) WITHOUT ROWID;
                 CREATE TABLE command_dedupe (
                   plugin_id TEXT NOT NULL,
                   command_id TEXT NOT NULL,
                   completed_at_ms INTEGER NOT NULL,
                   result_json BLOB NOT NULL,
                   PRIMARY KEY (plugin_id, command_id)
                 ) WITHOUT ROWID;
                 CREATE TABLE peer_message_command (
                   source_session_id TEXT NOT NULL,
                   command_id TEXT NOT NULL,
                   request_fingerprint TEXT NOT NULL,
                   completed_at_ms INTEGER NOT NULL,
                   message_id TEXT NOT NULL,
                   target_session_id TEXT NOT NULL,
                   PRIMARY KEY (source_session_id, command_id)
                 ) WITHOUT ROWID;
                 ALTER TABLE agent_host_pref ADD COLUMN
                   cross_agent_enabled INTEGER NOT NULL DEFAULT 1 CHECK (cross_agent_enabled IN (0, 1));
                 INSERT INTO agent_host_pref(id, hooks_enabled, cross_agent_enabled) VALUES (1, 0, 0);",
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 22).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        let prefs = store.get_agent_ui_snapshot().unwrap().unwrap();
        assert!(!prefs.agent_hooks_enabled, "别的偏好原样保留");
        drop(store);

        assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
        let connection = rusqlite::Connection::open(&path).unwrap();
        let peer_tables: u32 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name = 'peer_message_command'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(peer_tables, 0);
        let pref_columns: Vec<String> = connection
            .prepare("SELECT name FROM pragma_table_info('agent_host_pref') ORDER BY cid")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(pref_columns, ["id", "hooks_enabled"]);
        let event_tables: u32 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name IN
                   ('event_log', 'event_outbox', 'subscription_cursor', 'event_dead_letter',
                    'event_delivery_retry', 'command_dedupe')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(event_tables, 0);
        drop(connection);
    }

    /// v24 及更早的 `published_session`：还没有 `cli_exited` 列。
    const V24_PUBLISHED_SESSION: &str = "
DROP TABLE published_session;
CREATE TABLE published_session (
  id TEXT PRIMARY KEY,
  cwd TEXT,
  launch TEXT,
  provider TEXT,
  conversation_id TEXT,
  title TEXT,
  phase TEXT NOT NULL,
  phase_since INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  structured_events INTEGER NOT NULL CHECK (structured_events IN (0, 1)),
  turn_events INTEGER NOT NULL CHECK (turn_events IN (0, 1)),
  agent_event_version INTEGER,
  tokens_used INTEGER,
  branch TEXT,
  dirty_files_json BLOB NOT NULL
);
";

    #[test]
    fn v23_database_drops_deepseek_harness_data() {
        let dir = temp_dir("v23-migration");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(crate::schema::CREATE_SCHEMA)
            .unwrap();
        connection.execute_batch(V24_PUBLISHED_SESSION).unwrap();
        // 再把 auto_model_provider 换回带 harness 列的形状，就是一个 v23 库。
        connection
            .execute_batch(
                "DROP TABLE auto_model_provider;
                 CREATE TABLE auto_model_provider (
                   harness TEXT NOT NULL CHECK (harness IN ('dsh', 'pi')),
                   provider TEXT NOT NULL,
                   PRIMARY KEY (harness, provider)
                 ) WITHOUT ROWID;
                 INSERT INTO auto_model_provider(harness, provider)
                   VALUES ('dsh', 'sub2api'), ('pi', 'token-x');
                 INSERT INTO session_group(id, position) VALUES ('g-dsh', 0), ('g-pi', 1);
                 INSERT INTO session(id, group_id, kind, position) VALUES
                   ('s-dsh', 'g-dsh', 'acp', 0),
                   ('t-dsh', 'g-dsh', 'terminal', 1),
                   ('s-pi', 'g-pi', 'acp', 0);
                 INSERT INTO acp_detail(session_id, agent, launch_command) VALUES
                   ('s-dsh', 'dsh', 'dsh-acp-rich'),
                   ('s-pi', 'pi', 'smelt-pi');
                 INSERT INTO history_title(agent, resume_id, custom_title, updated_at_ms) VALUES
                   ('dsh', 'r1', 'DSH', 1), ('pi', 'r2', 'Pi', 1);
                 INSERT INTO agent_acp_command(command_key, command) VALUES
                   ('acp_dsh_cmd', 'dsh-acp-rich'), ('acp_pi_cmd', 'smelt-pi');
                 INSERT INTO agent_acp_env(engine_kind_id, name, value) VALUES
                   ('dsh', 'K', 'v'), ('pi', 'K', 'v');
                 INSERT INTO agent_profile(id, kind_id, label, workspace_dir, position) VALUES
                   ('dsh-native-team', 'dsh', 'team', '', 0);
                 INSERT INTO agent_definition(
                   id, name, description, engine_kind_id, prompt, plugins_json,
                   context_folders_json, context_links_json, position
                 ) VALUES
                   ('a-dsh', 'D', '', 'dsh', '', X'5B5D', X'5B5D', X'5B5D', 0),
                   ('a-pi', 'P', '', 'pi', '', X'5B5D', X'5B5D', X'5B5D', 1);
                 INSERT INTO default_conversation_agent(id, agent_definition_id) VALUES (1, 'a-dsh');
                 INSERT INTO new_session_pin(position, action_key) VALUES
                   (0, 'conversation:dsh'),
                   (1, 'conversation:dsh:profile:dsh-native-team'),
                   (2, 'conversation:agent:a-dsh'),
                   (3, 'conversation:agent:a-pi');",
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 23).unwrap();
        drop(connection);

        drop(Store::open_or_create(&path).unwrap());
        assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
        let connection = rusqlite::Connection::open(&path).unwrap();
        let strings = |sql: &str| -> Vec<String> {
            connection
                .prepare(sql)
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(
            strings("SELECT id FROM session_group ORDER BY id"),
            ["g-pi"],
            "DSH 对话连同里面的终端整段删掉"
        );
        assert_eq!(strings("SELECT id FROM session ORDER BY id"), ["s-pi"]);
        assert_eq!(strings("SELECT agent FROM history_title"), ["pi"]);
        assert_eq!(
            strings("SELECT command_key FROM agent_acp_command"),
            ["acp_pi_cmd"]
        );
        assert_eq!(strings("SELECT engine_kind_id FROM agent_acp_env"), ["pi"]);
        assert!(strings("SELECT id FROM agent_profile").is_empty());
        assert_eq!(strings("SELECT id FROM agent_definition"), ["a-pi"]);
        assert!(strings("SELECT agent_definition_id FROM default_conversation_agent").is_empty());
        assert_eq!(
            strings("SELECT action_key FROM new_session_pin"),
            ["conversation:agent:a-pi"]
        );
        assert_eq!(
            strings("SELECT provider FROM auto_model_provider"),
            ["token-x"],
            "Pi 的名单保留，DSH 的删掉"
        );
        assert!(
            strings("SELECT name FROM pragma_table_info('published_session')")
                .contains(&"cli_exited".to_string()),
            "v23 一路升到当前，v25 的列也要补上"
        );
        drop(connection);
    }

    #[test]
    fn v24_database_adds_cli_exited_to_published_sessions() {
        let dir = temp_dir("v24-migration");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(crate::schema::CREATE_SCHEMA)
            .unwrap();
        connection.execute_batch(V24_PUBLISHED_SESSION).unwrap();
        connection
            .execute_batch(
                "INSERT INTO published_session(
                   id, cwd, launch, provider, conversation_id, title, phase, phase_since,
                   updated_at, structured_events, turn_events, agent_event_version, tokens_used,
                   branch, dirty_files_json
                 ) VALUES (
                   't1', '/work', 'claude', 'claude', 'conv-1', '旧会话', 'succeeded', 1,
                   2, 1, 1, 1, NULL, NULL, X'5B5D'
                 );",
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 24).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
        let sessions = store
            .get_published_session_snapshot()
            .unwrap()
            .expect("迁移不能丢掉已发布会话")
            .sessions;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].conversation_id.as_deref(), Some("conv-1"));
        assert!(!sessions[0].cli_exited, "旧库里的会话按 CLI 未退出迁移");
    }

    #[test]
    fn v3_database_with_a_cursor_row_migrates_to_current() {
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
        connection.pragma_update(None, "user_version", 3).unwrap();
        drop(connection);

        drop(Store::open_or_create(&path).unwrap());
        assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
        let cursors: u32 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name = 'subscription_cursor'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cursors, 0, "持久投递在 v23 整个删掉");
    }

    #[test]
    fn v4_database_migrates_to_current() {
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
            0
        );
        drop(connection);
    }

    fn kept_history_title(store: &Store) -> String {
        store
            .get_history_title_snapshot()
            .unwrap()
            .expect("迁移不能丢掉历史标题")
            .sessions
            .remove(0)
            .custom_title
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
                 INSERT INTO history_title(agent, profile_id, resume_id, custom_title, updated_at_ms)
                 VALUES ('codex', '', 'r1', 'keep', 1);",
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 5).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        assert_eq!(kept_history_title(&store), "keep");
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
    }

    #[test]
    fn migrates_recognized_v1_schema_without_losing_rows() {
        let dir = temp_dir("migrate-v1");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(v1_schema()).unwrap();
        connection
            .execute(
                "INSERT INTO history_title(agent, profile_id, resume_id, custom_title, updated_at_ms)
                 VALUES ('codex', '', 'r1', 'keep', 1)",
                [],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        assert_eq!(kept_history_title(&store), "keep");
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
    }

    /// v7 的库一路迁到当前版本。v9 之前的自动化只存在 `kv` 文档里，那套导入已经删了
    /// （产品没发布过，真实库都在 v16 之后），这里只保证结构收敛、`kv` 不留。
    #[test]
    fn migrates_v7_schema_to_current_without_kv() {
        let dir = temp_dir("migrate-v7");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(&v7_schema()).unwrap();
        connection
            .execute_batch(
                "INSERT INTO kv(scope, key, value, value_type, updated_at_ms)
                 VALUES ('automation_run_transcript', 'run-1', X'5B5D', 'blob', 9);
                 INSERT INTO history_title(agent, profile_id, resume_id, custom_title, updated_at_ms)
                 VALUES ('codex', '', 'r1', 'keep', 1);",
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 7).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        assert_eq!(kept_history_title(&store), "keep");
        assert_eq!(
            store.get_automation_run_transcript("run-1").unwrap(),
            None,
            "没有对应 Run 的对话记录随外键重建丢掉"
        );
        drop(store);
        assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
        let connection = rusqlite::Connection::open(&path).unwrap();
        let kv_exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'kv')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!kv_exists);
        drop(connection);
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
    }

    #[test]
    fn rejects_v5_schema_with_unrecognized_acp_extensions() {
        let dir = temp_dir("migrate-v5-acp-detail");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(crate::schema::V17_SCHEMA).unwrap();
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
                        .put_paired_device(&PairedDevice {
                            endpoint_id: format!("worker-{index}"),
                            name: "ready".into(),
                            paired_at_ms: 1,
                            last_seen_at_ms: None,
                        })
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }
        let store = Store::open_or_create(path).unwrap();
        assert_eq!(store.list_paired_devices().unwrap().len(), 8);
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
                "INSERT INTO project(root, position) VALUES ('/keep-me', 0)",
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
    }

    #[test]
    fn reopening_current_schema_keeps_rows() {
        let dir = temp_dir("reopen-keeps-rows");
        let path = dir.join(DATABASE_FILE_NAME);
        let store = Store::open_or_create(&path).unwrap();
        store
            .put_launch_snapshot(&LaunchSnapshot {
                version: 7,
                entries: vec![LaunchEntryRecord {
                    label: "Claude".into(),
                    command: "claude".into(),
                    provider: Some("claude".into()),
                }],
            })
            .unwrap();
        drop(store);

        let store = Store::open_or_create(&path).unwrap();
        let launch = store.get_launch_snapshot().unwrap().unwrap();
        assert_eq!(launch.entries[0].label, "Claude");
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
                    "id": "conv-terminals",
                    "terminals": [
                        {"cwd": "/repo", "id": "term-1"},
                        {
                            "cwd": "/repo/src",
                            "id": "term-2",
                            "custom_title": "tests",
                            "launch_label": "Codex",
                            "launch_cmd": "codex"
                        }
                    ],
                    "last_updated_at": 42,
                    "custom_title": "terminal group",
                    "route": {"expanded": ["/repo/src"], "tool_panel_open": true}
                },
                {
                    "id": "conv-acp",
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
                            "type": "automation",
                            "run_id": "run-1"
                        }
                    },
                    "route": null
                }
            ]
        });
        put_workspace_json(&store, &serde_json::to_vec(&workspace).unwrap());

        let connection = rusqlite::Connection::open(&path).unwrap();
        for (table, expected) in [
            ("project", 1i64),
            ("session_group", 2),
            ("session", 3),
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
                "SELECT value FROM acp_launch_env WHERE session_id = 'acp-1' AND name = 'PROFILE'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(profile, "work");
        let (model, binding): (String, String) = connection
            .query_row(
                "SELECT c.value_id, d.binding_automation_run_id
                 FROM acp_config_value c JOIN acp_detail d USING (session_id)
                 WHERE c.session_id = 'acp-1' AND c.config_id = 'model'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((model.as_str(), binding.as_str()), ("sonnet", "run-1"));
        let old_payload_tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name IN ('workspace_meta', 'workspace_session')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_payload_tables, 0);

        let restored: serde_json::Value = workspace_json(&store);
        assert_eq!(restored["sessions"][0]["id"], "conv-terminals");
        assert_eq!(restored["sessions"][0]["terminals"][1]["id"], "term-2");
        assert_eq!(
            restored["sessions"][0]["terminals"][1]["launch_cmd"],
            "codex"
        );
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
            restored["sessions"][1]["acp"]["conversation_binding"]["run_id"],
            "run-1"
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
                "pending_prompt",
                "pending_delivery_id",
                "agent_definition_id",
                "automation_id",
                "binding_automation_run_id",
                "pending_agent_preset",
                "session_title",
            ]
        );
        assert!(
            restored.get("collapsed_projects").is_none() && restored.get("sidebar_w").is_none(),
            "界面状态归客户端，运行时不存"
        );
        assert!(restored["sessions"][0].get("route").is_none());
        drop(connection);
        put_workspace_json(&store, &serde_json::to_vec(&workspace).unwrap());
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
    }

    fn put_workspace_json(store: &Store, raw: &[u8]) {
        store
            .put_workspace_snapshot(&Store::workspace_snapshot_from_json(raw).unwrap())
            .unwrap();
    }

    fn workspace_json(store: &Store) -> serde_json::Value {
        let snapshot = store.get_workspace_snapshot().unwrap().unwrap();
        serde_json::from_slice(&Store::workspace_snapshot_to_json(&snapshot).unwrap()).unwrap()
    }

    fn acp_workspace(history_session_id: Option<&str>) -> Vec<u8> {
        let history = match history_session_id {
            Some(id) => serde_json::json!(id),
            None => serde_json::Value::Null,
        };
        serde_json::to_vec(&serde_json::json!({
            "sessions": [{
                "id": "conv-acp",
                "acp": {
                    "sid": "acp-1",
                    "cwd": "/repo",
                    "agent": "pi",
                    "launch": {"command": "smelt-pi-agent"},
                    "history_session_id": history
                }
            }]
        }))
        .unwrap()
    }

    fn stored_history_id(store: &Store) -> Option<String> {
        workspace_json(store)["sessions"][0]["acp"]["history_session_id"]
            .as_str()
            .map(str::to_string)
    }

    /// 历史页「继续」新开的槽位：窗口存档常常比 acp_open 先落库。第一次落库必须
    /// 留下这次选中的 id，否则守护进程读到「行在、列空」，会当成已放弃、开一段空白会话。
    #[test]
    fn first_workspace_save_keeps_the_resumed_history_id() {
        let dir = temp_dir("history-first-save");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        put_workspace_json(&store, &acp_workspace(Some("pi-picked")));
        assert_eq!(
            store.stored_history_identity("acp-1").unwrap(),
            StoredHistoryIdentity::Bound("pi-picked".into())
        );
        put_workspace_json(&store, &acp_workspace(Some("gui-stale")));
        assert_eq!(
            stored_history_id(&store).as_deref(),
            Some("pi-picked"),
            "只有第一次落库采用窗口的 id"
        );
    }

    #[test]
    fn daemon_confirmed_history_id_survives_workspace_rewrite() {
        let dir = temp_dir("history-owner");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        assert!(
            !store
                .remember_history_session_id("acp-1", "pi-confirmed")
                .unwrap()
        );
        assert_eq!(
            store.stored_history_identity("acp-1").unwrap(),
            StoredHistoryIdentity::NoRow
        );
        put_workspace_json(&store, &acp_workspace(None));
        assert_eq!(
            store.stored_history_identity("acp-1").unwrap(),
            StoredHistoryIdentity::Unbound
        );
        assert!(
            store
                .remember_history_session_id("acp-1", "pi-confirmed")
                .unwrap()
        );
        assert_eq!(
            store.stored_history_identity("acp-1").unwrap(),
            StoredHistoryIdentity::Bound("pi-confirmed".into())
        );
        assert!(!store.remember_history_session_id("acp-1", "  ").unwrap());

        put_workspace_json(&store, &acp_workspace(None));
        assert_eq!(
            stored_history_id(&store).as_deref(),
            Some("pi-confirmed"),
            "窗口存档没带恢复身份时，不能把守护进程已经确认的 id 写成空"
        );
        put_workspace_json(&store, &acp_workspace(Some("gui-stale")));
        assert_eq!(
            stored_history_id(&store).as_deref(),
            Some("pi-confirmed"),
            "行已经存在后，窗口快照不能换成另一份 id"
        );

        assert!(
            store
                .remember_history_session_id("acp-1", "pi-next")
                .unwrap()
        );
        put_workspace_json(&store, &acp_workspace(None));
        assert_eq!(stored_history_id(&store).as_deref(), Some("pi-next"));

        assert!(store.clear_history_session_id("acp-1").unwrap());
        put_workspace_json(&store, &acp_workspace(Some("gui-stale")));
        assert_eq!(
            stored_history_id(&store),
            None,
            "守护进程放弃的身份，窗口旧值不能再写回来"
        );
        assert_eq!(store.history_session_id("acp-1").unwrap(), None);
        assert_eq!(
            store.stored_history_identity("acp-1").unwrap(),
            StoredHistoryIdentity::Unbound
        );
        assert_eq!(
            store.stored_history_identity("acp-missing").unwrap(),
            StoredHistoryIdentity::NoRow
        );
    }

    #[test]
    fn catalog_snapshots_round_trip_through_relational_tables() {
        let dir = temp_dir("catalog-snapshots");
        let path = dir.join(DATABASE_FILE_NAME);
        let store = Store::open_or_create(&path).unwrap();
        let agent_ui = AgentUiSnapshot {
            agent_hooks_enabled: true,
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
                mcp_servers_json: b"[]".to_vec(),
                context_folders_json: b"[]".to_vec(),
                context_links_json: b"[]".to_vec(),
                model_provider: String::new(),
                model_id: String::new(),
                memory_enabled: true,
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
            mcp_servers_json: b"[]".to_vec(),
            context_folders_json: b"[]".to_vec(),
            context_links_json: b"[]".to_vec(),
            model_provider: String::new(),
            model_id: String::new(),
            memory_enabled: true,
        };
        store.insert_agent_definition(&extra).unwrap();
        store.put_agent_ui_prefs(&agent_ui).unwrap();
        let after_prefs = store.get_agent_ui_snapshot().unwrap().unwrap();
        assert_eq!(after_prefs.agents.len(), 2);
        assert_eq!(after_prefs.agents[1].id, "agent-2");

        let renamed = store
            .modify_agent_definition::<StoreError>("agent-2", |record| {
                record.name = "改名".into();
                Ok(())
            })
            .unwrap()
            .unwrap();
        assert_eq!(renamed.name, "改名");
        assert!(
            store
                .modify_agent_definition::<StoreError>("missing", |_| Ok(()))
                .unwrap()
                .is_none()
        );
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
                cli_exited: false,
            }],
        };
        store.put_published_session_snapshot(&published).unwrap();
        assert_eq!(
            store.get_published_session_snapshot().unwrap().as_ref(),
            Some(&published)
        );

        let connection = rusqlite::Connection::open(&path).unwrap();
        for (table, expected) in [
            ("agent_host_pref", 1i64),
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
    }

    /// v9 的库建出 v10 起的目录表，一路迁到当前版本；已有的关系表数据原样保留。
    /// v9 之前放在 `kv` 文档里的目录不再导入（同 v7 的说明）。
    #[test]
    fn migrates_v9_schema_to_current_keeping_relational_rows() {
        let dir = temp_dir("migrate-v9");
        let path = dir.join(DATABASE_FILE_NAME);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(&v9_schema()).unwrap();
        connection
            .execute_batch(
                "INSERT INTO automation(
                   id, name, enabled, workspace_dir, position, trigger_json, action_json, sinks_json
                 ) VALUES ('auto-1', 'Morning', 1, NULL, 0, X'7B7D', X'7B7D', X'5B5D');
                 INSERT INTO automation_meta(key, value) VALUES ('revision', '3');
                 INSERT INTO history_title(agent, profile_id, resume_id, custom_title, updated_at_ms)
                 VALUES ('codex', '', 'r1', 'keep', 1);",
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 9).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        let automations = store.get_automation_snapshot().unwrap().unwrap();
        assert_eq!(automations.revision, 3);
        assert_eq!(automations.automations[0].name, "Morning");
        assert_eq!(kept_history_title(&store), "keep");
        assert!(store.get_agent_ui_snapshot().unwrap().is_none());
        drop(store);
        assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
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
        assert_eq!(workspace.projects[0], "/repo");
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
    }

    fn catalog_writer(id: &str, seq: u64) -> WorkspaceCatalogWriter {
        WorkspaceCatalogWriter {
            id: id.to_string(),
            seq,
        }
    }

    fn catalog_snapshot(project: &str) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            projects: vec![project.to_string()],
            sessions: Vec::new(),
        }
    }

    fn catalog_projects(store: &Store) -> Vec<String> {
        store.get_workspace_snapshot().unwrap().unwrap().projects
    }

    /// 回执丢了：运行时已经提交，客户端还停在旧版本。它下一次提交基于旧版本，
    /// 但当前版本正是它自己写的，必须照常提交，不能当成别的设备改过。
    #[test]
    fn same_writer_resending_on_a_stale_base_after_a_lost_reply_commits() {
        let dir = temp_dir("catalog-lost-reply");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        let writer = |seq| catalog_writer("client-a", seq);

        assert_eq!(
            store
                .commit_workspace_snapshot(0, &writer(1), &catalog_snapshot("/a1"))
                .unwrap(),
            WorkspaceCatalogCommit::Committed { revision: 1 }
        );
        // 回执丢了，客户端仍以 0 为基准，接连又丢了一次。
        assert_eq!(
            store
                .commit_workspace_snapshot(0, &writer(2), &catalog_snapshot("/a2"))
                .unwrap(),
            WorkspaceCatalogCommit::Committed { revision: 2 }
        );
        assert_eq!(
            store
                .commit_workspace_snapshot(0, &writer(3), &catalog_snapshot("/a3"))
                .unwrap(),
            WorkspaceCatalogCommit::Committed { revision: 3 }
        );
        assert_eq!(store.workspace_catalog_revision().unwrap(), 3);
        assert_eq!(catalog_projects(&store), ["/a3"]);
    }

    /// 别的写入方提交过一次，基于旧版本的提交就是真冲突，哪怕更早的版本是自己写的。
    #[test]
    fn stale_base_after_another_writer_committed_is_a_conflict() {
        let dir = temp_dir("catalog-other-writer");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();

        store
            .commit_workspace_snapshot(0, &catalog_writer("client-a", 1), &catalog_snapshot("/a"))
            .unwrap();
        store
            .commit_workspace_snapshot(1, &catalog_writer("client-b", 1), &catalog_snapshot("/b"))
            .unwrap();

        assert_eq!(
            store
                .commit_workspace_snapshot(
                    0,
                    &catalog_writer("client-a", 2),
                    &catalog_snapshot("/mine")
                )
                .unwrap(),
            WorkspaceCatalogCommit::Conflict { current: 2 }
        );
        // 新的写入方（比如重启后的客户端）基于旧版本也一样。
        assert_eq!(
            store
                .commit_workspace_snapshot(
                    1,
                    &catalog_writer("client-c", 1),
                    &catalog_snapshot("/mine")
                )
                .unwrap(),
            WorkspaceCatalogCommit::Conflict { current: 2 }
        );
        assert_eq!(store.workspace_catalog_revision().unwrap(), 2);
        assert_eq!(catalog_projects(&store), ["/b"]);
    }

    /// 超时的旧请求在新提交之后才到：序号更小，不能盖掉更新的那份。
    #[test]
    fn late_request_from_the_same_writer_cannot_overwrite_a_newer_commit() {
        let dir = temp_dir("catalog-late-request");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();

        store
            .commit_workspace_snapshot(0, &catalog_writer("client-a", 2), &catalog_snapshot("/new"))
            .unwrap();
        assert_eq!(
            store
                .commit_workspace_snapshot(
                    0,
                    &catalog_writer("client-a", 1),
                    &catalog_snapshot("/old")
                )
                .unwrap(),
            WorkspaceCatalogCommit::Conflict { current: 1 }
        );
        assert_eq!(catalog_projects(&store), ["/new"]);
    }

    /// v18、v19 都有、v20 删掉的 worktree 继承两张表，原样照 v18 的建表语句。
    /// 用当前结构的库退回旧版本做夹具时补上。
    const V19_WORKTREE_INHERIT_TABLES: &str = "
CREATE TABLE worktree_inherit (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  enabled INTEGER NOT NULL CHECK (enabled IN (0, 1))
);
CREATE TABLE worktree_inherit_skip_pattern (
  position INTEGER PRIMARY KEY CHECK (position >= 0),
  pattern TEXT NOT NULL
);
";

    /// 库里各结构对象的建表语句，去掉空白后按名字排好，用来比较两份库的形状。
    fn normalized_schema(path: &Path) -> Vec<(String, String, String)> {
        rusqlite::Connection::open(path)
            .unwrap()
            .prepare(
                "SELECT type, name, sql FROM sqlite_schema
                 WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
            )
            .unwrap()
            .query_map([], |row| {
                let sql: Option<String> = row.get(2)?;
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    sql.unwrap_or_default()
                        .chars()
                        .filter(|ch| !ch.is_whitespace())
                        .collect(),
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    /// v18 的库：工作区目录补上写入方两列，原来的版本号留着；接着删掉 worktree 继承两张表。
    #[test]
    fn migrates_v18_workspace_catalog_to_record_its_writer() {
        let dir = temp_dir("migrate-v18-catalog-writer");
        let path = dir.join(DATABASE_FILE_NAME);
        drop(Store::open_or_create(&path).unwrap());
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(&format!(
                "{V21_SESSION_TABLES}
                 {V21_LAYOUT_NODE_TABLE}
                 ALTER TABLE workspace_catalog DROP COLUMN writer_seq;
                 ALTER TABLE workspace_catalog DROP COLUMN writer;
                 INSERT INTO workspace_catalog(id, revision) VALUES (1, 41);
                 {V19_WORKTREE_INHERIT_TABLES}
                 INSERT INTO worktree_inherit(id, enabled) VALUES (1, 1);"
            ))
            .unwrap();
        connection.pragma_update(None, "user_version", 18).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        assert_eq!(store.workspace_catalog_revision().unwrap(), 41);
        // 迁移来的版本没有写入方，只认基于当前版本的提交。
        assert_eq!(
            store
                .commit_workspace_snapshot(
                    40,
                    &catalog_writer("client-a", 1),
                    &catalog_snapshot("/a")
                )
                .unwrap(),
            WorkspaceCatalogCommit::Conflict { current: 41 }
        );
        assert_eq!(
            store
                .commit_workspace_snapshot(
                    41,
                    &catalog_writer("client-a", 1),
                    &catalog_snapshot("/a")
                )
                .unwrap(),
            WorkspaceCatalogCommit::Committed { revision: 42 }
        );
        drop(store);
        assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
        let fresh = dir.join("fresh.sqlite3");
        drop(Store::open_or_create(&fresh).unwrap());
        assert_eq!(
            normalized_schema(&path),
            normalized_schema(&fresh),
            "从 v18 升上来的库要和新建的库一个形状"
        );
    }

    /// v19 的库：worktree 继承两张表随 git worktree 功能下线删掉，其余数据原样留着，
    /// 升级前留一份 `.bak-v19`。
    #[test]
    fn migrates_v19_drops_worktree_inherit_tables() {
        let dir = temp_dir("migrate-v19-worktree-inherit");
        let path = dir.join(DATABASE_FILE_NAME);
        let store = Store::open_or_create(&path).unwrap();
        store.put_runtime_id("runtime-1").unwrap();
        store
            .commit_workspace_snapshot(0, &catalog_writer("client-a", 1), &catalog_snapshot("/a"))
            .unwrap();
        store
            .put_remote_config_snapshot(&RemoteConfigSnapshot {
                enabled: true,
                iroh_relay: "relay.example".into(),
            })
            .unwrap();
        drop(store);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(&format!(
                "{V21_SESSION_TABLES}
                 {V21_LAYOUT_NODE_TABLE}
                 {V19_WORKTREE_INHERIT_TABLES}
                 INSERT INTO worktree_inherit(id, enabled) VALUES (1, 1);
                 INSERT INTO worktree_inherit_skip_pattern(position, pattern)
                 VALUES (0, 'node_modules'), (1, 'target');"
            ))
            .unwrap();
        connection.pragma_update(None, "user_version", 19).unwrap();
        drop(connection);

        let store = Store::open_or_create(&path).unwrap();
        assert_eq!(store.runtime_id().unwrap().as_deref(), Some("runtime-1"));
        assert_eq!(store.workspace_catalog_revision().unwrap(), 1);
        assert_eq!(catalog_projects(&store), ["/a"]);
        assert_eq!(
            store.get_remote_config_snapshot().unwrap(),
            Some(RemoteConfigSnapshot {
                enabled: true,
                iroh_relay: "relay.example".into(),
            })
        );
        // 写入方跟着留下：同一写入方基于旧版本的下一次提交，仍认得出是自己的。
        assert_eq!(
            store
                .commit_workspace_snapshot(
                    0,
                    &catalog_writer("client-a", 2),
                    &catalog_snapshot("/b")
                )
                .unwrap(),
            WorkspaceCatalogCommit::Committed { revision: 2 }
        );
        drop(store);

        assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
        let connection = rusqlite::Connection::open(&path).unwrap();
        for table in ["worktree_inherit", "worktree_inherit_skip_pattern"] {
            let exists: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = ?1)",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(!exists, "{table} 应该已经删掉");
        }
        drop(connection);
        let fresh = dir.join("fresh.sqlite3");
        drop(Store::open_or_create(&fresh).unwrap());
        assert_eq!(
            normalized_schema(&path),
            normalized_schema(&fresh),
            "从 v19 升上来的库要和新建的库一个形状"
        );

        let backup = path.with_file_name(format!("{DATABASE_FILE_NAME}.bak-v19"));
        let backup = rusqlite::Connection::open(&backup).unwrap();
        assert_eq!(
            backup
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            19
        );
        assert_eq!(
            backup
                .query_row(
                    "SELECT COUNT(*) FROM worktree_inherit_skip_pattern",
                    [],
                    |row| row.get::<_, u32>(0)
                )
                .unwrap(),
            2,
            "备份里留着删表前的数据"
        );
        drop(backup);
    }

    /// 一段终端对话、一段 ACP 对话。终端对话里三个终端故意不按 id 排。
    fn conversations_json() -> serde_json::Value {
        serde_json::json!({
            "projects": ["/repo"],
            "sessions": [
                {
                    "id": "conv-terminals",
                    "custom_title": "terminal group",
                    "last_updated_at": 7,
                    "terminals": [
                        {"id": "term-3", "cwd": "/repo"},
                        {"id": "term-1", "cwd": "/repo/src", "custom_title": "tests",
                         "launch_label": "Codex", "launch_cmd": "codex"},
                        {"id": "term-2", "cwd": "/repo"}
                    ]
                },
                {
                    "id": "conv-acp",
                    "last_updated_at": 8,
                    "acp": {"sid": "acp-1", "cwd": "/repo", "agent": "pi",
                            "launch": {"command": "smelt-pi-agent"}}
                }
            ]
        })
    }

    #[test]
    fn conversations_keep_their_id_and_terminal_order() {
        let dir = temp_dir("conversation-terminals");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        put_workspace_json(&store, &serde_json::to_vec(&conversations_json()).unwrap());

        let restored = workspace_json(&store);
        let terminals = &restored["sessions"][0];
        assert_eq!(terminals["id"], "conv-terminals");
        assert_eq!(terminals["custom_title"], "terminal group");
        let ids = terminals["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|terminal| terminal["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["term-3", "term-1", "term-2"], "顺序是客户端给的");
        assert_eq!(terminals["terminals"][1]["launch_cmd"], "codex");
        assert_eq!(restored["sessions"][1]["id"], "conv-acp");
        assert_eq!(restored["sessions"][1]["acp"]["sid"], "acp-1");
        assert!(
            restored["sessions"][1]["terminals"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        // 分屏和当前看的是哪个归客户端，运行时不收也不吐。
        assert!(terminals.get("layout").is_none());
        assert!(terminals.get("active").is_none());
    }

    #[test]
    fn documents_without_a_stable_conversation_id_are_rejected() {
        let mut missing = conversations_json();
        missing["sessions"][1].as_object_mut().unwrap().remove("id");
        assert!(matches!(
            Store::workspace_snapshot_from_json(&serde_json::to_vec(&missing).unwrap()),
            Err(StoreError::ConversationMissingId { position: 1 })
        ));

        let mut duplicate = conversations_json();
        duplicate["sessions"][1]["id"] = "conv-terminals".into();
        assert!(matches!(
            Store::workspace_snapshot_from_json(&serde_json::to_vec(&duplicate).unwrap()),
            Err(StoreError::DuplicateConversationId { id }) if id == "conv-terminals"
        ));

        let dir = temp_dir("empty-conversation");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();
        let mut empty = conversations_json();
        empty["sessions"][0]["terminals"] = serde_json::json!([]);
        let snapshot =
            Store::workspace_snapshot_from_json(&serde_json::to_vec(&empty).unwrap()).unwrap();
        assert!(matches!(
            store.put_workspace_snapshot(&snapshot),
            Err(StoreError::EmptyConversation { id }) if id == "conv-terminals"
        ));
    }

    /// v21 的会话和会话组：会话还没有 `position`，会话组还记着当前看的是哪个。从新建的库
    /// 倒回去用。
    const V21_SESSION_TABLES: &str = "
DROP INDEX session_group_position_idx;
ALTER TABLE session DROP COLUMN position;
CREATE INDEX session_group_id_idx ON session(group_id);
ALTER TABLE session_group ADD COLUMN active_session_id TEXT
  REFERENCES session(id) ON DELETE SET NULL DEFERRABLE INITIALLY DEFERRED;
";

    /// v21 的布局树：能记标签组。
    const V21_LAYOUT_NODE_TABLE: &str = "
CREATE TABLE layout_node (
  id TEXT PRIMARY KEY,
  group_id TEXT NOT NULL REFERENCES session_group(id) ON DELETE CASCADE,
  parent_id TEXT REFERENCES layout_node(id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK (position >= 0),
  kind TEXT NOT NULL CHECK (kind IN ('split', 'tabs', 'leaf')),
  axis TEXT CHECK (axis IN ('horizontal', 'vertical') OR axis IS NULL),
  session_id TEXT UNIQUE REFERENCES session(id) ON DELETE CASCADE,
  size_px REAL CHECK (size_px IS NULL OR size_px >= 0),
  active_index INTEGER CHECK (active_index IS NULL OR active_index >= 0),
  UNIQUE (group_id, parent_id, position),
  CHECK (
    (kind = 'split' AND axis IS NOT NULL AND session_id IS NULL AND active_index IS NULL)
    OR (kind = 'tabs' AND axis IS NULL AND session_id IS NULL AND active_index IS NOT NULL)
    OR (kind = 'leaf' AND axis IS NULL AND session_id IS NOT NULL AND active_index IS NULL)
  )
);
CREATE UNIQUE INDEX layout_node_root_idx
  ON layout_node(group_id)
  WHERE parent_id IS NULL;
";

    /// v20 的 layout_node：只有 split / leaf 两种节点，没有 `active_index`。
    const V20_LAYOUT_NODE_TABLE: &str = "
CREATE TABLE layout_node (
  id TEXT PRIMARY KEY,
  group_id TEXT NOT NULL REFERENCES session_group(id) ON DELETE CASCADE,
  parent_id TEXT REFERENCES layout_node(id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK (position >= 0),
  kind TEXT NOT NULL CHECK (kind IN ('split', 'leaf')),
  axis TEXT CHECK (axis IN ('horizontal', 'vertical') OR axis IS NULL),
  session_id TEXT UNIQUE REFERENCES session(id) ON DELETE CASCADE,
  size_px REAL CHECK (size_px IS NULL OR size_px >= 0),
  UNIQUE (group_id, parent_id, position),
  CHECK (
    (kind = 'split' AND axis IS NOT NULL AND session_id IS NULL)
    OR (kind = 'leaf' AND axis IS NULL AND session_id IS NOT NULL)
  )
);
CREATE UNIQUE INDEX layout_node_root_idx
  ON layout_node(group_id)
  WHERE parent_id IS NULL;
";

    /// 旧库里的两段对话：终端那段是「左格上下分 term-1、term-2，右格 term-3」，三个终端落库的
    /// 顺序故意打乱成 3、1、2；另一段是 ACP 对话。
    const OLD_LAYOUT_ROWS: &str = "
INSERT INTO project(root, position) VALUES ('/repo', 0);
INSERT INTO session_group(id, project_root, position, custom_title, last_updated_at, active_session_id)
VALUES ('group:0:term-1', '/repo', 0, 'terminal group', 7, 'term-2'),
       ('group:1:acp-1', '/repo', 1, NULL, 8, 'acp-1');
INSERT INTO session(id, group_id, kind, cwd)
VALUES ('term-3', 'group:0:term-1', 'terminal', '/repo'),
       ('term-1', 'group:0:term-1', 'terminal', '/repo'),
       ('term-2', 'group:0:term-1', 'terminal', '/repo/src'),
       ('acp-1', 'group:1:acp-1', 'acp', '/repo');
INSERT INTO acp_detail(session_id, agent, launch_command) VALUES ('acp-1', 'pi', 'smelt-pi-agent');
INSERT INTO layout_node(id, group_id, parent_id, position, kind, axis, session_id, size_px)
VALUES ('g0:0', 'group:0:term-1', NULL, 0, 'split', 'horizontal', NULL, NULL),
       ('g0:1', 'group:0:term-1', 'g0:0', 0, 'split', 'vertical', NULL, 400.0),
       ('g0:2', 'group:0:term-1', 'g0:1', 0, 'leaf', NULL, 'term-1', 300.0),
       ('g0:3', 'group:0:term-1', 'g0:1', 1, 'leaf', NULL, 'term-2', 300.0),
       ('g0:4', 'group:0:term-1', 'g0:0', 1, 'leaf', NULL, 'term-3', 600.0),
       ('g1:0', 'group:1:acp-1', NULL, 0, 'leaf', NULL, 'acp-1', NULL);
";

    /// v21 和 v20 的库：布局树删掉，对话里的终端按原来布局树的深度优先序排（不看落库顺序），
    /// 老对话的 id 原样留下当作稳定 id，ACP 对话原样留着。升完和新建的库一个形状，升级前
    /// 留一份备份。
    #[test]
    fn migrates_layout_trees_into_terminal_order() {
        for (version, layout_node_table) in
            [(21u32, V21_LAYOUT_NODE_TABLE), (20, V20_LAYOUT_NODE_TABLE)]
        {
            let dir = temp_dir(&format!("migrate-v{version}-terminal-order"));
            let path = dir.join(DATABASE_FILE_NAME);
            drop(Store::open_or_create(&path).unwrap());
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection
                .execute_batch(&format!(
                    "{V21_SESSION_TABLES}
                     {layout_node_table}
                     BEGIN;
                     {OLD_LAYOUT_ROWS}
                     COMMIT;"
                ))
                .unwrap();
            connection
                .pragma_update(None, "user_version", version)
                .unwrap();
            drop(connection);

            let store = Store::open_or_create(&path).unwrap();
            let workspace = store.get_workspace_snapshot().unwrap().unwrap();
            assert_eq!(workspace.sessions.len(), 2, "v{version}");
            let terminals = &workspace.sessions[0];
            assert_eq!(terminals.id, "group:0:term-1", "v{version}");
            assert_eq!(terminals.custom_title.as_deref(), Some("terminal group"));
            assert_eq!(
                terminals
                    .terminals
                    .iter()
                    .map(|terminal| terminal.id.as_deref().unwrap())
                    .collect::<Vec<_>>(),
                ["term-1", "term-2", "term-3"],
                "v{version}：按布局树深度优先，不按落库顺序"
            );
            assert_eq!(terminals.terminals[1].cwd.as_deref(), Some("/repo/src"));
            let acp = &workspace.sessions[1];
            assert_eq!(acp.id, "group:1:acp-1");
            assert!(acp.terminals.is_empty());
            assert_eq!(acp.acp.as_ref().unwrap().sid.as_deref(), Some("acp-1"));
            drop(store);

            assert_eq!(Store::schema_version(&path), Some(STORE_SCHEMA_VERSION));
            let fresh = dir.join("fresh.sqlite3");
            drop(Store::open_or_create(&fresh).unwrap());
            assert_eq!(
                normalized_schema(&path),
                normalized_schema(&fresh),
                "从 v{version} 升上来的库要和新建的库一个形状"
            );
            assert!(
                path.with_file_name(format!("{DATABASE_FILE_NAME}.bak-v{version}"))
                    .exists()
            );
        }
    }

    #[test]
    fn runtime_settings_round_trip_through_typed_tables() {
        let dir = temp_dir("runtime-settings");
        let store = Store::open_or_create(dir.join(DATABASE_FILE_NAME)).unwrap();

        assert_eq!(store.runtime_id().unwrap(), None);
        store.put_runtime_id("runtime-1").unwrap();
        assert_eq!(store.runtime_id().unwrap().as_deref(), Some("runtime-1"));

        assert_eq!(store.workspace_catalog_revision().unwrap(), 0);
        store
            .commit_workspace_snapshot(0, &catalog_writer("client-a", 1), &catalog_snapshot("/a"))
            .unwrap();
        assert_eq!(store.workspace_catalog_revision().unwrap(), 1);

        store
            .put_remote_config_snapshot(&RemoteConfigSnapshot {
                enabled: true,
                iroh_relay: "relay.example".into(),
            })
            .unwrap();
        let loaded = store.get_remote_config_snapshot().unwrap().unwrap();
        assert!(loaded.enabled);
        assert_eq!(loaded.iroh_relay, "relay.example");

        store
            .put_auto_model_providers(&["sub2api".into(), "a".into()])
            .unwrap();
        assert_eq!(store.auto_model_providers().unwrap(), ["a", "sub2api"]);

        let theme = TerminalThemeSnapshot {
            version: 1,
            dark: false,
            background: 0xffffff,
            foreground: 0x111111,
            cursor: 0x222222,
            selection: 0x333333,
            palette: vec![1, 2, 3],
            search_hit: 4,
            search_hit_current: 5,
        };
        store.put_terminal_theme_snapshot(&theme).unwrap();
        assert_eq!(store.get_terminal_theme_snapshot().unwrap(), Some(theme));

        let quota = QuotaCacheSnapshot {
            saved_at_ms: 1_000,
            schema_version: 3,
            provider_ids: vec!["claude".into(), "copilot".into()],
            statuses: vec![
                QuotaCacheStatusRecord {
                    provider_id: "claude".into(),
                    checked_at_ms: 900,
                    error: None,
                    quota: QuotaCacheQuota::Windows(vec![QuotaCacheWindowRecord {
                        label: "5h".into(),
                        used_percent: 37.5,
                        resets_at_ms: Some(2_000),
                        reset_after_seconds: None,
                    }]),
                },
                QuotaCacheStatusRecord {
                    provider_id: "copilot".into(),
                    checked_at_ms: 900,
                    error: None,
                    quota: QuotaCacheQuota::Copilot {
                        unit: "ai_credits".into(),
                        remaining: 11.0,
                        limit: Some(43.0),
                        used: None,
                    },
                },
            ],
        };
        store.put_quota_cache_snapshot(&quota).unwrap();
        assert_eq!(store.get_quota_cache_snapshot().unwrap(), Some(quota));

        let menu = PublishedWorkspaceMenuSnapshot {
            version: 2,
            revision: 12,
            source_id: "desktop".into(),
            source_revision: 3,
            projects: vec![WorkspaceMenuProjectRecord {
                root: "/repo".into(),
                title: "repo".into(),
                order: 0,
            }],
            sessions: vec![WorkspaceMenuSessionRecord {
                id: "s1".into(),
                kind: "acp".into(),
                title: "对话".into(),
                custom_title: true,
                cwd: Some("/repo".into()),
                project_root: Some("/repo".into()),
                project_title: Some("repo".into()),
                project_order: 0,
                session_order: 1,
                leaf_order: 0,
                agent: Some("pi".into()),
            }],
        };
        store.put_workspace_menu_snapshot(&menu).unwrap();
        assert_eq!(store.get_workspace_menu_snapshot().unwrap(), Some(menu));

        assert_eq!(
            store.memory_settings().unwrap(),
            MemorySettingsRecord::default()
        );
        store
            .modify_memory_settings::<StoreError>(|settings| {
                settings.embedding_model = "bge-m3".into();
                Ok(())
            })
            .unwrap();
        assert_eq!(store.memory_settings().unwrap().embedding_model, "bge-m3");

        store
            .put_default_conversation_agent(Some("writer"))
            .unwrap();
        assert_eq!(
            store.default_conversation_agent().unwrap().as_deref(),
            Some("writer")
        );
        store.put_default_conversation_agent(None).unwrap();
        assert_eq!(store.default_conversation_agent().unwrap(), None);

        store
            .put_new_session_pins(&["b".into(), "a".into(), "b".into()])
            .unwrap();
        assert_eq!(store.new_session_pins().unwrap(), ["b", "a"]);

        store
            .put_history_agent_binding("pi", "", "resume-1", "writer")
            .unwrap();
        store
            .put_history_agent_binding("pi", "work", "resume-2", "coder")
            .unwrap();
        assert_eq!(
            store
                .history_agent_binding("pi", "", "resume-1")
                .unwrap()
                .as_deref(),
            Some("writer")
        );
        assert_eq!(
            store.history_agent_bindings("pi", "work").unwrap(),
            std::collections::BTreeMap::from([("resume-2".to_string(), "coder".to_string())])
        );
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
    }

    #[test]
    fn create_refuses_to_overwrite_an_existing_database() {
        let dir = temp_dir("create-exists");
        let path = dir.join(DATABASE_FILE_NAME);
        Store::create(&path).unwrap();
        let error = Store::create(&path).unwrap_err();
        assert!(matches!(error, StoreError::AlreadyExists { .. }), "{error}");
    }

    /// 几个进程同时首次建库：都拿到建好的库，没人撞见建了一半的空 schema，临时文件不留。
    #[test]
    fn concurrent_open_or_create_all_see_a_complete_database() {
        let dir = temp_dir("concurrent-create");
        let path = dir.join(DATABASE_FILE_NAME);
        let openers = 8;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(openers));
        let handles: Vec<_> = (0..openers)
            .map(|_| {
                let path = path.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    Store::open_or_create(&path).map(drop)
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        let connection = rusqlite::Connection::open(&path).unwrap();
        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, STORE_SCHEMA_VERSION);
        drop(connection);
        let leftovers: Vec<_> = std::fs::read_dir(&*dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".creating-"))
            .collect();
        assert!(leftovers.is_empty(), "临时库没删干净：{leftovers:?}");
    }

    /// 正式路径上的空文件不会是别人建了一半的库（建库只挂建完的），立刻拒绝，不等。
    #[test]
    fn open_or_create_refuses_an_empty_file_at_once() {
        let dir = temp_dir("empty-file-open-or-create");
        let path = dir.join(DATABASE_FILE_NAME);
        std::fs::write(&path, []).unwrap();
        let error = Store::open_or_create(&path).unwrap_err();
        assert!(matches!(error, StoreError::UninitializedDatabase), "{error}");
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
    }
}
