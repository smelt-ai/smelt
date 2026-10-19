//! 已拆成关系表的领域快照。活路径走这些结构体，不经 JSON 文档入口。

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchSnapshot {
    pub version: u32,
    pub entries: Vec<LaunchEntryRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchEntryRecord {
    pub label: String,
    pub command: String,
    pub provider: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryTitleSnapshot {
    pub schema_version: u32,
    pub sessions: Vec<HistoryTitleRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryTitleRecord {
    pub agent: String,
    pub profile_id: String,
    pub resume_id: String,
    pub custom_title: String,
    pub updated_at_ms: i64,
}

/// 运行时持有的工作区目录：打开的项目和会话。侧栏宽度、当前页、折叠/固定这些
/// 界面状态归客户端，不在这里。
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceSnapshot {
    /// 项目根目录，按侧栏顺序。
    pub projects: Vec<String>,
    pub sessions: Vec<WorkspaceSessionRecord>,
}

/// 提交工作区目录的一方：客户端每次运行生成的 id，加上它自己每次提交递增的序号。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceCatalogWriter {
    pub id: String,
    pub seq: u64,
}

/// 一次工作区目录提交的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceCatalogCommit {
    Committed {
        revision: u64,
    },
    /// 基准版本之后有别人提交过，什么都没写。
    Conflict {
        current: u64,
    },
}

/// 一段对话：侧栏里的一条。里面有哪些终端、是不是一段 ACP 对话归运行时；终端怎么分屏、
/// 当前看的是哪个，是每台设备自己的界面状态，归客户端（设计文档 D1）。
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceSessionRecord {
    /// 对话的稳定 id，由新建它的客户端生成，之后不再变。客户端按它存这段对话的界面状态。
    pub id: String,
    pub custom_title: Option<String>,
    pub last_updated_at: i64,
    /// 这段对话里的终端，按客户端给的顺序。
    pub terminals: Vec<WorkspaceTerminalRecord>,
    pub acp: Option<WorkspaceAcpRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceTerminalRecord {
    /// 守护里的终端会话 id；还没起过的终端没有。
    pub id: Option<String>,
    pub cwd: Option<String>,
    pub custom_title: Option<String>,
    pub launch_label: Option<String>,
    pub launch_cmd: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceAcpRecord {
    pub cwd: Option<String>,
    pub sid: Option<String>,
    pub agent: String,
    pub profile_id: Option<String>,
    pub history_session_id: Option<String>,
    pub launch_command: String,
    /// 启动环境变量，按名字排序。
    pub launch_env: Vec<(String, String)>,
    pub refresh_launch_from_settings: bool,
    pub pending_prompt: Option<String>,
    pub pending_delivery_id: Option<String>,
    pub agent_definition_id: Option<String>,
    pub automation_id: Option<String>,
    /// 最后生效的 provider 配置：`(config_id, value_id)`，按设置顺序。
    pub config_values: Vec<(String, String)>,
    /// 绑定到自动化 Run 时是它的 id；普通对话为 `None`。
    pub binding_automation_run_id: Option<String>,
    pub pending_agent_preset: Option<String>,
    pub session_title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentUiSnapshot {
    pub agent_hooks_enabled: bool,
    pub commands: Vec<AgentConversationCommandRecord>,
    pub env: Vec<AgentAcpEnvRecord>,
    pub config_memory: Vec<AgentAcpConfigMemoryRecord>,
    pub agents: Vec<AgentDefinitionRecord>,
    pub profiles: Vec<AgentProfileRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentConversationCommandRecord {
    pub command_key: String,
    pub command: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentAcpEnvRecord {
    pub engine_kind_id: String,
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentAcpConfigMemoryRecord {
    pub engine_kind_id: String,
    pub config_id: String,
    pub value_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentDefinitionRecord {
    pub id: String,
    pub name: String,
    pub description: String,
    pub engine_kind_id: String,
    pub prompt: String,
    pub plugins_json: Vec<u8>,
    /// 勾选要连接的 MCP 服务器名（JSON 字符串数组）；空数组表示不连任何 MCP 服务器。
    pub mcp_servers_json: Vec<u8>,
    pub context_folders_json: Vec<u8>,
    pub context_links_json: Vec<u8>,
    pub model_provider: String,
    pub model_id: String,
    /// 关闭时不注入、不写入记忆；已有记忆文件保留。
    pub memory_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProfileRecord {
    pub id: String,
    pub kind_id: String,
    pub label: String,
    pub workspace_dir: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteSessionCatalogSnapshot {
    pub acp: Vec<RemoteAcpSessionRecord>,
    pub terminal: Vec<RemoteTerminalSessionRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteAcpSessionRecord {
    pub id: String,
    pub cwd: String,
    pub title: String,
    pub agent_option_id: String,
    pub agent: String,
    pub launch_command: String,
    pub launch_env_json: Vec<u8>,
    pub resume_id: Option<String>,
    pub created_at: i64,
    pub lifecycle: String,
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteTerminalSessionRecord {
    pub id: String,
    pub cwd: String,
    pub title: String,
    pub created_at: i64,
    pub lifecycle: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedSessionSnapshot {
    pub version: u32,
    pub sessions: Vec<PublishedSessionRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedSessionRecord {
    pub id: String,
    pub cwd: Option<String>,
    pub launch: Option<String>,
    pub provider: Option<String>,
    pub conversation_id: Option<String>,
    pub title: Option<String>,
    pub phase: String,
    pub phase_since: i64,
    pub updated_at: i64,
    pub structured_events: bool,
    pub turn_events: bool,
    pub agent_event_version: Option<i64>,
    pub tokens_used: Option<i64>,
    pub branch: Option<String>,
    pub dirty_files_json: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteConfigSnapshot {
    pub enabled: bool,
    pub iroh_relay: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalThemeSnapshot {
    pub version: u32,
    pub dark: bool,
    pub background: u32,
    pub foreground: u32,
    pub cursor: u32,
    pub selection: u32,
    pub palette: Vec<u32>,
    pub search_hit: u32,
    pub search_hit_current: u32,
}

/// 哪一家的自定义 provider 名单（「这个 provider 的模型目录由 Smelt 维护」）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoModelHarness {
    Dsh,
    Pi,
}

impl AutoModelHarness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dsh => "dsh",
            Self::Pi => "pi",
        }
    }
}

/// 跨启动的额度缓存。只缓存真正拿到额度的项。
#[derive(Clone, Debug, PartialEq)]
pub struct QuotaCacheSnapshot {
    pub saved_at_ms: i64,
    pub schema_version: u32,
    /// 生成这份缓存时注册过的全部 provider，包括没拿到额度的。
    pub provider_ids: Vec<String>,
    pub statuses: Vec<QuotaCacheStatusRecord>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QuotaCacheStatusRecord {
    pub provider_id: String,
    pub checked_at_ms: i64,
    pub error: Option<String>,
    pub quota: QuotaCacheQuota,
}

#[derive(Clone, Debug, PartialEq)]
pub enum QuotaCacheQuota {
    /// 按时间窗口计的额度（Claude、Codex 等）。
    Windows(Vec<QuotaCacheWindowRecord>),
    /// Copilot 的剩余量。`unit` 是 `ai_credits` 或 `premium_requests`。
    Copilot {
        unit: String,
        remaining: f64,
        limit: Option<f64>,
        used: Option<f64>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct QuotaCacheWindowRecord {
    pub label: String,
    pub used_percent: f64,
    pub resets_at_ms: Option<i64>,
    pub reset_after_seconds: Option<i64>,
}

/// 守护进程发布给移动端的侧栏菜单。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedWorkspaceMenuSnapshot {
    pub version: u32,
    pub revision: u64,
    pub source_id: String,
    pub source_revision: u64,
    pub projects: Vec<WorkspaceMenuProjectRecord>,
    pub sessions: Vec<WorkspaceMenuSessionRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceMenuProjectRecord {
    pub root: String,
    pub title: String,
    pub order: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceMenuSessionRecord {
    pub id: String,
    /// `terminal` 或 `acp`。
    pub kind: String,
    pub title: String,
    pub custom_title: bool,
    pub cwd: Option<String>,
    pub project_root: Option<String>,
    pub project_title: Option<String>,
    pub project_order: u32,
    pub session_order: u32,
    pub leaf_order: u32,
    pub agent: Option<String>,
}

/// 记忆召回的配置。空字符串表示没设。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemorySettingsRecord {
    pub embedding_base_url: String,
    pub embedding_model: String,
    pub embedding_api_key: String,
    pub rerank_model_provider: String,
    pub rerank_model_id: String,
}

/// 记忆的来源可信度。`pending`：读过外部内容的那一轮或无人值守的运行写的，不注入、不召回，
/// 用户采纳后变成 `trusted`。没有记录的记忆文件（用户手写、旧数据）按 `trusted` 处理。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryTrust {
    Trusted,
    Pending,
}

impl MemoryTrust {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Pending => "pending",
        }
    }
}

/// 一台已配对的远程设备。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairedDevice {
    /// 设备的 iroh endpoint id（公钥），白名单按它认。
    pub endpoint_id: String,
    pub name: String,
    pub paired_at_ms: i64,
    pub last_seen_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryUsageRecord {
    pub file_name: String,
    pub read_count: u64,
    pub last_read_at_ms: i64,
}

/// 一层记忆（一个 scope）的元数据。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryMetaSnapshot {
    pub usage: Vec<MemoryUsageRecord>,
    /// 待确认的文件名和原因。
    pub pending: Vec<(String, String)>,
}
