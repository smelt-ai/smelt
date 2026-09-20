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

#[derive(Clone, Debug, PartialEq)]
pub struct AppearanceSnapshot {
    pub bg_color: u32,
    pub bg_image: Option<String>,
    pub bg_image_opacity: f32,
    pub opacity: f32,
    pub blur: bool,
    pub glass_style: String,
    pub theme_mode: String,
    pub ui_font_px: u32,
    pub ui_font_family: String,
    pub font_px: u32,
    pub font_family: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceSnapshot {
    pub active_session: u64,
    pub active_session_id: Option<String>,
    pub route_json: Option<Vec<u8>>,
    pub selected_agent_id: Option<String>,
    pub active_workspace_surface: Option<String>,
    pub workspace_surface_titles_json: Option<Vec<u8>>,
    pub sidebar_w: Option<f64>,
    pub sidebar_open: Option<bool>,
    pub sidebar_grouping: Option<String>,
    pub collapsed_agents: Vec<String>,
    pub pinned_projects: Vec<String>,
    pub sidebar_hide_empty_projects: bool,
    pub projects: Vec<WorkspaceProjectRecord>,
    pub sessions: Vec<WorkspaceSessionRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceProjectRecord {
    pub root: String,
    pub collapsed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceSessionRecord {
    pub custom_title: Option<String>,
    pub last_updated_at: i64,
    pub active: usize,
    pub layout: WorkspaceLayoutNode,
    pub acp: Option<WorkspaceAcpRecord>,
    pub route_json: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WorkspaceLayoutNode {
    Leaf {
        cwd: Option<String>,
        id: Option<String>,
        custom_title: Option<String>,
        launch_label: Option<String>,
        launch_cmd: Option<String>,
    },
    Split {
        vertical: bool,
        children: Vec<WorkspaceLayoutNode>,
        sizes: Vec<f64>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceAcpRecord {
    pub cwd: Option<String>,
    pub sid: Option<String>,
    pub agent: String,
    pub profile_id: Option<String>,
    pub history_session_id: Option<String>,
    pub launch_command: String,
    pub launch_env_json: Option<Vec<u8>>,
    pub refresh_launch_from_settings: bool,
    pub fork_session_id: Option<String>,
    pub fork_title: Option<String>,
    pub fork_agent: Option<String>,
    pub fork_profile_label: Option<String>,
    pub fork_from_history: bool,
    pub pending_prompt: Option<String>,
    pub pending_delivery_id: Option<String>,
    pub agent_definition_id: Option<String>,
    pub automation_id: Option<String>,
    pub config_values_json: Option<Vec<u8>>,
    pub conversation_binding_json: Option<Vec<u8>>,
    pub agent_session_json: Option<Vec<u8>>,
    pub pending_agent_preset_json: Option<Vec<u8>>,
    pub session_title_json: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentUiSnapshot {
    pub agent_hooks_enabled: bool,
    pub cross_agent_enabled: bool,
    pub notify_approval: bool,
    pub notify_input: bool,
    pub notify_success: bool,
    pub notify_failure: bool,
    pub notify_terminal_bell: bool,
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
    pub context_folders_json: Vec<u8>,
    pub context_links_json: Vec<u8>,
    pub model_provider: String,
    pub model_id: String,
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
    pub write_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateSettingsSnapshot {
    pub channel: String,
    pub auto_install: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateStateSnapshot {
    pub current_url: Option<String>,
    pub staged_json: Option<Vec<u8>>,
    pub cleanup_app: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeInheritSnapshot {
    pub enabled: bool,
    pub skip_patterns: Vec<String>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoModelSnapshot {
    pub providers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuotaCacheSnapshot {
    pub saved_at_ms: i64,
    pub schema_version: u32,
    pub provider_ids: Vec<String>,
    pub providers_json: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedWorkspaceMenuSnapshot {
    pub version: u32,
    pub revision: u64,
    pub source_id: String,
    pub source_revision: u64,
    pub projects_json: Vec<u8>,
    pub sessions_json: Vec<u8>,
}
