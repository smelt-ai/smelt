//! 控制 RPC 的版本化信封，以及每个客户端握手都要用的 `system.describe`。
//!
//! 这里只覆盖“一问一答”的控制 RPC。`open`/`watch`/`event_subscribe`/ACP 字节流仍由
//! [`super::daemon`] 的首帧分流，不能为了表面统一塞进 RPC 信封。具体方法的参数与
//! 结果类型由使用方定义（桌面在 `smelt_core::control_api`），这里只放所有客户端
//! 共享、且必须能编进移动端的部分。

use std::fmt;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use super::daemon::DaemonOperation;

pub const CONTROL_API_VERSION: u16 = 1;
pub const CONTROL_API_MIN_SUPPORTED_VERSION: u16 = 1;
pub const MAX_CONTROL_REQUEST_ID_BYTES: usize = 128;

macro_rules! define_control_methods {
    ($( $variant:ident => ($constant:ident, $wire:literal) ),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum ControlMethod {
            $( $variant, )+
        }

        impl ControlMethod {
            pub const ALL: &'static [Self] = &[$( Self::$variant, )+];

            pub const fn as_str(self) -> &'static str {
                match self {
                    $( Self::$variant => $wire, )+
                }
            }

            pub fn from_wire_name(value: &str) -> Option<Self> {
                match value {
                    $( $wire => Some(Self::$variant), )+
                    _ => None,
                }
            }
        }

        impl fmt::Display for ControlMethod {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        $( pub const $constant: &str = $wire; )+
    };
}

// 这份声明同时生成 method enum、wire name、解析器和能力清单。服务端对 enum 做穷举
// match；新增方法若没有 handler 会直接编译失败，避免 describe 与分发器各维护一张表。
define_control_methods! {
    SystemDescribe => (SYSTEM_DESCRIBE_METHOD, "system.describe"),
    SessionList => (SESSION_LIST_METHOD, "session.list"),
    AgentList => (AGENT_LIST_METHOD, "agent.list"),
    AgentSend => (AGENT_SEND_METHOD, "agent.send"),
    AgentMemoryMeta => (AGENT_MEMORY_META_METHOD, "agent.memory.meta"),
    AgentMemoryRecordRead => (AGENT_MEMORY_RECORD_READ_METHOD, "agent.memory.record_read"),
    AgentMemoryRecordWrite => (AGENT_MEMORY_RECORD_WRITE_METHOD, "agent.memory.record_write"),
    AgentDefinitionList => (AGENT_DEFINITION_LIST_METHOD, "agent.definition.list"),
    AgentDefinitionGet => (AGENT_DEFINITION_GET_METHOD, "agent.definition.get"),
    AgentDefinitionCreate => (AGENT_DEFINITION_CREATE_METHOD, "agent.definition.create"),
    AgentDefinitionUpdate => (AGENT_DEFINITION_UPDATE_METHOD, "agent.definition.update"),
    AgentDefinitionDelete => (AGENT_DEFINITION_DELETE_METHOD, "agent.definition.delete"),
    SettingsGet => (SETTINGS_GET_METHOD, "settings.get"),
    SettingsUpdate => (SETTINGS_UPDATE_METHOD, "settings.update"),
    AutomationList => (AUTOMATION_LIST_METHOD, "automation.list"),
    AutomationGet => (AUTOMATION_GET_METHOD, "automation.get"),
    AutomationUpsert => (AUTOMATION_UPSERT_METHOD, "automation.upsert"),
    AutomationDelete => (AUTOMATION_DELETE_METHOD, "automation.delete"),
    AutomationSetEnabled => (AUTOMATION_SET_ENABLED_METHOD, "automation.set_enabled"),
    AutomationRunOnce => (AUTOMATION_RUN_ONCE_METHOD, "automation.run_once"),
    GitDiscover => (GIT_DISCOVER_METHOD, "git.discover"),
    GitRepoInfo => (GIT_REPO_INFO_METHOD, "git.repo_info"),
    GitStatus => (GIT_STATUS_METHOD, "git.status"),
    GitBranches => (GIT_BRANCHES_METHOD, "git.branches"),
    GitIsDirty => (GIT_IS_DIRTY_METHOD, "git.is_dirty"),
    GitCheckout => (GIT_CHECKOUT_METHOD, "git.checkout"),
    GitBranchDelete => (GIT_BRANCH_DELETE_METHOD, "git.branch_delete"),
    GitMerge => (GIT_MERGE_METHOD, "git.merge"),
    GitStage => (GIT_STAGE_METHOD, "git.stage"),
    GitUnstage => (GIT_UNSTAGE_METHOD, "git.unstage"),
    GitDiscardFile => (GIT_DISCARD_FILE_METHOD, "git.discard_file"),
    GitApplyPatch => (GIT_APPLY_PATCH_METHOD, "git.apply_patch"),
    GitDiscardAll => (GIT_DISCARD_ALL_METHOD, "git.discard_all"),
    GitCommit => (GIT_COMMIT_METHOD, "git.commit"),
    GitPush => (GIT_PUSH_METHOD, "git.push"),
    GitFetch => (GIT_FETCH_METHOD, "git.fetch"),
    GitPull => (GIT_PULL_METHOD, "git.pull"),
    GitStashPush => (GIT_STASH_PUSH_METHOD, "git.stash_push"),
    GitStashPop => (GIT_STASH_POP_METHOD, "git.stash_pop"),
    GitWorktreeList => (GIT_WORKTREE_LIST_METHOD, "git.worktree.list"),
    GitWorktreeCreate => (GIT_WORKTREE_CREATE_METHOD, "git.worktree.create"),
    GitWorktreeRemove => (GIT_WORKTREE_REMOVE_METHOD, "git.worktree.remove"),
    GitWorktreePrune => (GIT_WORKTREE_PRUNE_METHOD, "git.worktree.prune"),
    GitFileDiff => (GIT_FILE_DIFF_METHOD, "git.diff.file"),
    GitAllDiff => (GIT_ALL_DIFF_METHOD, "git.diff.all"),
    GitLog => (GIT_LOG_METHOD, "git.log"),
    GitCommitFiles => (GIT_COMMIT_FILES_METHOD, "git.commit.files"),
    GitCommitFileDiff => (GIT_COMMIT_FILE_DIFF_METHOD, "git.commit.file_diff"),
    ProjectTree => (PROJECT_TREE_METHOD, "project.tree"),
    ProjectSearch => (PROJECT_SEARCH_METHOD, "project.search"),
    ProjectDeleteEntry => (PROJECT_DELETE_ENTRY_METHOD, "project.delete_entry"),
    FileOpen => (FILE_OPEN_METHOD, "file.open"),
    FileSave => (FILE_SAVE_METHOD, "file.save"),
    SessionCompleteFiles => (SESSION_COMPLETE_FILES_METHOD, "session.complete_files"),
    HistoryList => (HISTORY_LIST_METHOD, "history.list"),
    HistoryDetail => (HISTORY_DETAIL_METHOD, "history.detail"),
    HistoryDelete => (HISTORY_DELETE_METHOD, "history.delete"),
    HistoryTitles => (HISTORY_TITLES_METHOD, "history.titles"),
    AgentHostGet => (AGENT_HOST_GET_METHOD, "agent.host.get"),
    AgentHostApplyEdit => (AGENT_HOST_APPLY_EDIT_METHOD, "agent.host.apply_edit"),
    LaunchGet => (LAUNCH_GET_METHOD, "launch.get"),
    LaunchReplace => (LAUNCH_REPLACE_METHOD, "launch.replace"),
    MemoryTestEmbedding => (MEMORY_TEST_EMBEDDING_METHOD, "memory.test_embedding"),
    WorkspaceLoad => (WORKSPACE_LOAD_METHOD, "workspace.load"),
    WorkspaceSave => (WORKSPACE_SAVE_METHOD, "workspace.save"),
    AgentMemoryList => (AGENT_MEMORY_LIST_METHOD, "agent.memory.list"),
    AgentMemoryDelete => (AGENT_MEMORY_DELETE_METHOD, "agent.memory.delete"),
    AgentMemoryApprove => (AGENT_MEMORY_APPROVE_METHOD, "agent.memory.approve"),
    AutomationRunDetail => (AUTOMATION_RUN_DETAIL_METHOD, "automation.run_detail"),
    AgentSpaceEnsure => (AGENT_SPACE_ENSURE_METHOD, "agent.space.ensure"),
    HistoryRememberAgent => (HISTORY_REMEMBER_AGENT_METHOD, "history.remember_agent"),
    AgentHooksStatus => (AGENT_HOOKS_STATUS_METHOD, "agent.hooks.status"),
    AgentHooksInstall => (AGENT_HOOKS_INSTALL_METHOD, "agent.hooks.install"),
    AgentHooksUninstall => (AGENT_HOOKS_UNINSTALL_METHOD, "agent.hooks.uninstall"),
    AgentCliInspect => (AGENT_CLI_INSPECT_METHOD, "agent.cli.inspect"),
    AgentCliInstall => (AGENT_CLI_INSTALL_METHOD, "agent.cli.install"),
    PiConfigGet => (PI_CONFIG_GET_METHOD, "pi.config.get"),
    PiConfigEnsureFile => (PI_CONFIG_ENSURE_FILE_METHOD, "pi.config.ensure_file"),
    PiModelsSetThinkingLevel => (PI_MODELS_SET_THINKING_LEVEL_METHOD, "pi.models.set_thinking_level"),
    PiModelsSaveDefault => (PI_MODELS_SAVE_DEFAULT_METHOD, "pi.models.save_default"),
    PiProvidersDiscoverModels => (PI_PROVIDERS_DISCOVER_MODELS_METHOD, "pi.providers.discover_models"),
    PiProvidersSaveCustom => (PI_PROVIDERS_SAVE_CUSTOM_METHOD, "pi.providers.save_custom"),
    PiProvidersRemoveCustom => (PI_PROVIDERS_REMOVE_CUSTOM_METHOD, "pi.providers.remove_custom"),
    PiProvidersRefreshAuto => (PI_PROVIDERS_REFRESH_AUTO_METHOD, "pi.providers.refresh_auto"),
    PiMcpAddPreset => (PI_MCP_ADD_PRESET_METHOD, "pi.mcp.add_preset"),
    PiMcpSetEnabled => (PI_MCP_SET_ENABLED_METHOD, "pi.mcp.set_enabled"),
    PiMcpRemove => (PI_MCP_REMOVE_METHOD, "pi.mcp.remove"),
    PiMcpLogin => (PI_MCP_LOGIN_METHOD, "pi.mcp.login"),
    PiPluginsRemove => (PI_PLUGINS_REMOVE_METHOD, "pi.plugins.remove"),
    PiPluginsImport => (PI_PLUGINS_IMPORT_METHOD, "pi.plugins.import"),
    PiAuthProviders => (PI_AUTH_PROVIDERS_METHOD, "pi.auth.providers"),
    PiAuthModels => (PI_AUTH_MODELS_METHOD, "pi.auth.models"),
    PiAuthLogout => (PI_AUTH_LOGOUT_METHOD, "pi.auth.logout"),
    PiAuthLoginStart => (PI_AUTH_LOGIN_START_METHOD, "pi.auth.login_start"),
    PiAuthLoginEvents => (PI_AUTH_LOGIN_EVENTS_METHOD, "pi.auth.login_events"),
    PiAuthLoginAnswer => (PI_AUTH_LOGIN_ANSWER_METHOD, "pi.auth.login_answer"),
    PiAuthLoginCancel => (PI_AUTH_LOGIN_CANCEL_METHOD, "pi.auth.login_cancel"),
    DshConfigGet => (DSH_CONFIG_GET_METHOD, "dsh.config.get"),
    DshConfigEnsureFile => (DSH_CONFIG_ENSURE_FILE_METHOD, "dsh.config.ensure_file"),
    DshModelsCapabilities => (DSH_MODELS_CAPABILITIES_METHOD, "dsh.models.capabilities"),
    DshModelsSaveDefault => (DSH_MODELS_SAVE_DEFAULT_METHOD, "dsh.models.save_default"),
    DshProvidersDiscoverModels => (DSH_PROVIDERS_DISCOVER_MODELS_METHOD, "dsh.providers.discover_models"),
    DshProvidersSaveCustom => (DSH_PROVIDERS_SAVE_CUSTOM_METHOD, "dsh.providers.save_custom"),
    DshProvidersRefreshAuto => (DSH_PROVIDERS_REFRESH_AUTO_METHOD, "dsh.providers.refresh_auto"),
    DshPluginsList => (DSH_PLUGINS_LIST_METHOD, "dsh.plugins.list"),
    DshPluginsRun => (DSH_PLUGINS_RUN_METHOD, "dsh.plugins.run"),
    QuotaGet => (QUOTA_GET_METHOD, "quota.get"),
    QuotaRefresh => (QUOTA_REFRESH_METHOD, "quota.refresh"),
    RemoteConfigGet => (REMOTE_CONFIG_GET_METHOD, "remote.config.get"),
    RemoteConfigSet => (REMOTE_CONFIG_SET_METHOD, "remote.config.set"),
    RemoteStatus => (REMOTE_STATUS_METHOD, "remote.status"),
    DevicePairBegin => (DEVICE_PAIR_BEGIN_METHOD, "device.pair_begin"),
    DevicePair => (DEVICE_PAIR_METHOD, "device.pair"),
    DeviceList => (DEVICE_LIST_METHOD, "device.list"),
    DeviceRevoke => (DEVICE_REVOKE_METHOD, "device.revoke"),
    NewSessionPinsGet => (NEW_SESSION_PINS_GET_METHOD, "new_session.pins.get"),
    NewSessionPinsToggle => (NEW_SESSION_PINS_TOGGLE_METHOD, "new_session.pins.toggle"),
    TerminalThemePublish => (TERMINAL_THEME_PUBLISH_METHOD, "terminal_theme.publish"),
    SessionLaunchPrepare => (SESSION_LAUNCH_PREPARE_METHOD, "session.launch.prepare"),
    SessionLoadedSkills => (SESSION_LOADED_SKILLS_METHOD, "session.loaded_skills"),
    AgentMemoryEnsureDir => (AGENT_MEMORY_ENSURE_DIR_METHOD, "agent.memory.ensure_dir"),
    ProjectWatch => (PROJECT_WATCH_METHOD, "project.watch"),
    SkillsSyncControl => (SKILLS_SYNC_CONTROL_METHOD, "skills.sync_control"),
    WorkspaceBrowseDirs => (WORKSPACE_BROWSE_DIRS_METHOD, "workspace.browse_dirs"),
    ConversationCreateForAgent => (CONVERSATION_CREATE_FOR_AGENT_METHOD, "conversation.create_for_agent"),
}

pub const SUPPORTED_CONTROL_METHODS: &[ControlMethod] = ControlMethod::ALL;

/// 一个编译期 RPC 描述：调用方不能把 A 方法的参数配到 B 方法，也不能把响应解成
/// 另一种类型。非 Rust 客户端使用同一组公开 method 常量与 JSON 契约。
pub trait ControlCall {
    type Params: Serialize + DeserializeOwned;
    type Result: Serialize + DeserializeOwned;

    const METHOD: ControlMethod;
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmptyParams {}

pub enum SystemDescribe {}

impl ControlCall for SystemDescribe {
    type Params = EmptyParams;
    type Result = SystemDescribeResult;

    const METHOD: ControlMethod = ControlMethod::SystemDescribe;
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlProtocolDescription {
    pub current_version: u16,
    pub min_supported_version: u16,
    pub max_supported_version: u16,
    /// 字符串而非枚举：旧客户端看到未来新增方法时仍能解析 describe 响应。
    pub methods: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonRuntimeInfo {
    pub version: String,
    pub exe_mtime: u64,
    pub exe: Option<String>,
    pub pid: u32,
    pub started_at: u64,
    pub session_count: usize,
    pub agent_event_version: u32,
    /// 运行时第一次启动时生成、之后永久不变的身份。和传输无关：socket 路径、iroh
    /// endpoint id 都只是「怎么到达它」，客户端按它区分运行时。
    ///
    /// `system.describe` 是冻结契约：客户端要先读懂旧运行时的回复，才能决定推送
    /// 升级。所以新字段都是 `Option`，`None` 只表示运行时早于这个字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    /// `std::env::consts::OS`，如 `macos`、`linux`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// `std::env::consts::ARCH`，如 `aarch64`、`x86_64`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// 运行时所在机器的主机名，客户端添加运行时时拿它当默认名字。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemDescribeResult {
    pub control: ControlProtocolDescription,
    pub daemon: DaemonRuntimeInfo,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlErrorCode {
    InvalidRequest,
    UnsupportedVersion,
    MethodNotFound,
    InvalidParams,
    PermissionDenied,
    NotFound,
    FailedPrecondition,
    Conflict,
    Busy,
    TemporarilyUnavailable,
    ResultUnknown,
    OperationFailed,
    Internal,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlVersionRange {
    pub min_supported_version: u16,
    pub max_supported_version: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlError {
    pub code: ControlErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_versions: Option<ControlVersionRange>,
}

impl ControlError {
    pub fn new(code: ControlErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            supported_versions: None,
        }
    }

    pub fn unsupported_version(requested: u16, min_supported: u16, max_supported: u16) -> Self {
        Self {
            code: ControlErrorCode::UnsupportedVersion,
            message: format!(
                "control version {requested} is unsupported; supported range is {min_supported}..={max_supported}"
            ),
            supported_versions: Some(ControlVersionRange {
                min_supported_version: min_supported,
                max_supported_version: max_supported,
            }),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlResponse<T> {
    pub control_version: u16,
    pub request_id: String,
    #[serde(flatten)]
    pub outcome: ControlOutcome<T>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ControlOutcome<T> {
    Ok { result: T },
    Error { error: ControlError },
}

impl<T> ControlResponse<T> {
    pub fn ok(request_id: impl Into<String>, result: T) -> Self {
        Self::ok_with_version(CONTROL_API_VERSION, request_id, result)
    }

    pub fn ok_with_version(control_version: u16, request_id: impl Into<String>, result: T) -> Self {
        Self {
            control_version,
            request_id: request_id.into(),
            outcome: ControlOutcome::Ok { result },
        }
    }

    pub fn error(request_id: impl Into<String>, error: ControlError) -> Self {
        Self::error_with_version(CONTROL_API_VERSION, request_id, error)
    }

    pub fn error_with_version(
        control_version: u16,
        request_id: impl Into<String>,
        error: ControlError,
    ) -> Self {
        Self {
            control_version,
            request_id: request_id.into(),
            outcome: ControlOutcome::Error { error },
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ControlRequestEnvelope {
    pub control_version: u16,
    pub request_id: String,
    pub method: String,
    pub params: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlRequestFailure {
    pub response_version: u16,
    pub request_id: String,
    pub error: ControlError,
}

impl ControlRequestFailure {
    fn new(
        response_version: u16,
        request_id: String,
        code: ControlErrorCode,
        message: impl Into<String>,
    ) -> Self {
        Self::from_error(
            response_version,
            request_id,
            ControlError::new(code, message),
        )
    }

    fn from_error(response_version: u16, request_id: String, error: ControlError) -> Self {
        Self {
            response_version,
            request_id,
            error,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlClientError {
    InvalidRequest(String),
    InvalidResponse(String),
    Protocol(ControlError),
    Transport(String),
}

impl fmt::Display for ControlClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message) => write!(f, "invalid control request: {message}"),
            Self::InvalidResponse(message) => write!(f, "invalid control response: {message}"),
            Self::Protocol(error) => write!(f, "control {:?}: {}", error.code, error.message),
            Self::Transport(message) => write!(f, "control transport: {message}"),
        }
    }
}

impl std::error::Error for ControlClientError {}

fn valid_request_id(request_id: &str) -> bool {
    !request_id.trim().is_empty()
        && request_id.len() <= MAX_CONTROL_REQUEST_ID_BYTES
        && !request_id.chars().any(char::is_control)
}

/// 构造包含旧首帧 `op` 的完整请求。老守护不认识 `control` 时会直接断开，新客户端
/// 可在只读能力探测后选择 legacy 路径；有副作用的调用不得在结果未知时自动重放。
pub fn encode_request<C: ControlCall>(
    request_id: &str,
    params: &C::Params,
) -> Result<Value, ControlClientError> {
    encode_request_with_version::<C>(CONTROL_API_VERSION, request_id, params)
}

pub fn encode_request_with_version<C: ControlCall>(
    control_version: u16,
    request_id: &str,
    params: &C::Params,
) -> Result<Value, ControlClientError> {
    if !valid_request_id(request_id) {
        return Err(ControlClientError::InvalidRequest(format!(
            "request_id must contain 1..={MAX_CONTROL_REQUEST_ID_BYTES} non-control bytes"
        )));
    }
    let params = serde_json::to_value(params)
        .map_err(|error| ControlClientError::InvalidRequest(error.to_string()))?;
    Ok(serde_json::json!({
        "op": DaemonOperation::Control,
        "control_version": control_version,
        "request_id": request_id,
        "method": C::METHOD.as_str(),
        "params": params,
    }))
}

/// `DaemonRequest` 已经拿掉了 `op`，服务端从剩余 payload 解析控制信封。
pub fn decode_request_envelope(
    value: Value,
) -> Result<ControlRequestEnvelope, ControlRequestFailure> {
    decode_request_envelope_in_range(
        value,
        CONTROL_API_MIN_SUPPORTED_VERSION,
        CONTROL_API_VERSION,
    )
}

pub fn decode_request_envelope_in_range(
    value: Value,
    min_supported_version: u16,
    max_supported_version: u16,
) -> Result<ControlRequestEnvelope, ControlRequestFailure> {
    let candidate_response_version = value
        .get("control_version")
        .and_then(Value::as_u64)
        .and_then(|version| u16::try_from(version).ok())
        .filter(|version| (min_supported_version..=max_supported_version).contains(version))
        .unwrap_or(max_supported_version);
    let candidate_request_id = value
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|request_id| valid_request_id(request_id))
        .unwrap_or_default()
        .to_string();
    let envelope: ControlRequestEnvelope = serde_json::from_value(value).map_err(|error| {
        ControlRequestFailure::new(
            candidate_response_version,
            candidate_request_id.clone(),
            ControlErrorCode::InvalidRequest,
            error.to_string(),
        )
    })?;
    if !valid_request_id(&envelope.request_id) {
        return Err(ControlRequestFailure::new(
            candidate_response_version,
            String::new(),
            ControlErrorCode::InvalidRequest,
            format!("request_id must contain 1..={MAX_CONTROL_REQUEST_ID_BYTES} non-control bytes"),
        ));
    }
    if !(min_supported_version..=max_supported_version).contains(&envelope.control_version) {
        let error = ControlError::unsupported_version(
            envelope.control_version,
            min_supported_version,
            max_supported_version,
        );
        return Err(ControlRequestFailure::from_error(
            max_supported_version,
            envelope.request_id,
            error,
        ));
    }
    if envelope.method.trim().is_empty() || envelope.method.chars().any(char::is_control) {
        return Err(ControlRequestFailure::new(
            envelope.control_version,
            envelope.request_id,
            ControlErrorCode::InvalidRequest,
            "method must be a non-empty printable string",
        ));
    }
    if !envelope.params.is_object() {
        return Err(ControlRequestFailure::new(
            envelope.control_version,
            envelope.request_id,
            ControlErrorCode::InvalidParams,
            "params must be a JSON object",
        ));
    }
    Ok(envelope)
}

pub fn decode_params<T: DeserializeOwned>(params: Value) -> Result<T, ControlError> {
    serde_json::from_value(params)
        .map_err(|error| ControlError::new(ControlErrorCode::InvalidParams, error.to_string()))
}

pub fn decode_response<C: ControlCall>(
    expected_request_id: &str,
    value: Value,
) -> Result<C::Result, ControlClientError> {
    decode_response_with_version::<C>(CONTROL_API_VERSION, expected_request_id, value)
}

pub fn decode_response_with_version<C: ControlCall>(
    expected_control_version: u16,
    expected_request_id: &str,
    value: Value,
) -> Result<C::Result, ControlClientError> {
    let response: ControlResponse<C::Result> = serde_json::from_value(value)
        .map_err(|error| ControlClientError::InvalidResponse(error.to_string()))?;
    if response.request_id != expected_request_id {
        return Err(ControlClientError::InvalidResponse(format!(
            "response request_id {:?} does not match {:?}",
            response.request_id, expected_request_id
        )));
    }
    if let ControlOutcome::Error { ref error } = response.outcome
        && error.code == ControlErrorCode::UnsupportedVersion
    {
        return Err(ControlClientError::Protocol(error.clone()));
    }
    if response.control_version != expected_control_version {
        return Err(ControlClientError::InvalidResponse(format!(
            "response uses control version {}, expected {}",
            response.control_version, expected_control_version
        )));
    }
    match response.outcome {
        ControlOutcome::Ok { result } => Ok(result),
        ControlOutcome::Error { error } => Err(ControlClientError::Protocol(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_shape_is_versioned_and_self_identifying() {
        let request = encode_request::<SystemDescribe>("request-1", &EmptyParams::default())
            .expect("request should serialize");

        assert_eq!(
            request,
            serde_json::json!({
                "op": "control",
                "control_version": 1,
                "request_id": "request-1",
                "method": "system.describe",
                "params": {},
            })
        );
    }

    #[test]
    fn method_registry_names_are_unique_and_round_trip() {
        let mut names = std::collections::HashSet::new();
        for method in ControlMethod::ALL {
            assert!(names.insert(method.as_str()));
            assert_eq!(
                ControlMethod::from_wire_name(method.as_str()),
                Some(*method)
            );
        }
        assert_eq!(names.len(), 125);
    }

    #[test]
    fn unsupported_version_preserves_request_identity_in_the_error() {
        let failure = decode_request_envelope(serde_json::json!({
            "control_version": CONTROL_API_VERSION + 1,
            "request_id": "request-newer",
            "method": "system.describe",
            "params": {},
        }))
        .expect_err("a newer protocol version must be rejected");

        assert_eq!(failure.request_id, "request-newer");
        assert_eq!(failure.error.code, ControlErrorCode::UnsupportedVersion);
        assert_eq!(
            failure.error.supported_versions,
            Some(ControlVersionRange {
                min_supported_version: CONTROL_API_MIN_SUPPORTED_VERSION,
                max_supported_version: CONTROL_API_VERSION,
            })
        );
    }

    #[test]
    fn malformed_supported_request_uses_its_version_for_the_error() {
        let failure = decode_request_envelope_in_range(
            serde_json::json!({
                "control_version": 1,
                "request_id": "bad-params",
                "method": "session.list",
                "params": [],
            }),
            1,
            2,
        )
        .expect_err("params must be an object");

        assert_eq!(failure.response_version, 1);
        assert_eq!(failure.error.code, ControlErrorCode::InvalidParams);
    }

    #[test]
    fn unsupported_version_error_is_readable_across_versions() {
        let response = ControlResponse::<serde_json::Value>::error_with_version(
            1,
            "newer-client",
            ControlError::unsupported_version(2, 1, 1),
        );

        let error = decode_response_with_version::<SystemDescribe>(
            2,
            "newer-client",
            serde_json::to_value(response).unwrap(),
        )
        .expect_err("the server cannot execute version 2");

        let ControlClientError::Protocol(error) = error else {
            panic!("the client should preserve the structured protocol error");
        };
        assert_eq!(error.code, ControlErrorCode::UnsupportedVersion);
        assert_eq!(
            error.supported_versions,
            Some(ControlVersionRange {
                min_supported_version: 1,
                max_supported_version: 1,
            })
        );
    }
}
