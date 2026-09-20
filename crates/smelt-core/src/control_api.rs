//! smeltd 短控制请求的版本化 wire contract。
//!
//! 这里只覆盖“一问一答”的控制 RPC。`open`/`watch`/`event_subscribe`/ACP 字节流仍由
//! [`crate::daemon_protocol`] 的首帧分流，不能为了表面统一塞进 RPC 信封。

use std::fmt;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::agent_bus::AgentEndpoint;
use crate::agent_definition::AgentDefinition;
use crate::agent_definition_store::{AgentDefinitionCreate, AgentDefinitionPatch};
use crate::appearance_settings::{SettingsPatch, SettingsResult};
use crate::automation::{Automation, AutomationCommandResult};
use crate::daemon_protocol::DaemonOperation;

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

pub enum SessionList {}

impl ControlCall for SessionList {
    type Params = EmptyParams;
    type Result = SessionListResult;

    const METHOD: ControlMethod = ControlMethod::SessionList;
}

pub enum AgentList {}

impl ControlCall for AgentList {
    type Params = AgentListParams;
    type Result = AgentListResult;

    const METHOD: ControlMethod = ControlMethod::AgentList;
}

pub enum AgentSend {}

impl ControlCall for AgentSend {
    type Params = AgentSendParams;
    type Result = AgentSendResult;

    const METHOD: ControlMethod = ControlMethod::AgentSend;
}

pub enum AgentDefinitionList {}

impl ControlCall for AgentDefinitionList {
    type Params = EmptyParams;
    type Result = AgentDefinitionListResult;

    const METHOD: ControlMethod = ControlMethod::AgentDefinitionList;
}

pub enum AgentDefinitionGet {}

impl ControlCall for AgentDefinitionGet {
    type Params = AgentDefinitionIdParams;
    type Result = AgentDefinitionGetResult;

    const METHOD: ControlMethod = ControlMethod::AgentDefinitionGet;
}

pub enum AgentDefinitionCreateCall {}

impl ControlCall for AgentDefinitionCreateCall {
    type Params = AgentDefinitionCreate;
    type Result = AgentDefinitionGetResult;

    const METHOD: ControlMethod = ControlMethod::AgentDefinitionCreate;
}

pub enum AgentDefinitionUpdateCall {}

impl ControlCall for AgentDefinitionUpdateCall {
    type Params = AgentDefinitionUpdateParams;
    type Result = AgentDefinitionGetResult;

    const METHOD: ControlMethod = ControlMethod::AgentDefinitionUpdate;
}

pub enum AgentDefinitionDelete {}

impl ControlCall for AgentDefinitionDelete {
    type Params = AgentDefinitionIdParams;
    type Result = AgentDefinitionIdParams;

    const METHOD: ControlMethod = ControlMethod::AgentDefinitionDelete;
}

pub enum SettingsGet {}

impl ControlCall for SettingsGet {
    type Params = EmptyParams;
    type Result = SettingsResult;

    const METHOD: ControlMethod = ControlMethod::SettingsGet;
}

pub enum SettingsUpdate {}

impl ControlCall for SettingsUpdate {
    type Params = SettingsPatch;
    type Result = SettingsResult;

    const METHOD: ControlMethod = ControlMethod::SettingsUpdate;
}

pub enum AutomationList {}

impl ControlCall for AutomationList {
    type Params = EmptyParams;
    type Result = AutomationListResult;

    const METHOD: ControlMethod = ControlMethod::AutomationList;
}

pub enum AutomationGet {}

impl ControlCall for AutomationGet {
    type Params = AgentDefinitionIdParams;
    type Result = AutomationGetResult;

    const METHOD: ControlMethod = ControlMethod::AutomationGet;
}

pub enum AutomationUpsertCall {}

impl ControlCall for AutomationUpsertCall {
    type Params = Automation;
    type Result = AutomationGetResult;

    const METHOD: ControlMethod = ControlMethod::AutomationUpsert;
}

pub enum AutomationDelete {}

impl ControlCall for AutomationDelete {
    type Params = AgentDefinitionIdParams;
    type Result = AgentDefinitionIdParams;

    const METHOD: ControlMethod = ControlMethod::AutomationDelete;
}

pub enum AutomationSetEnabled {}

impl ControlCall for AutomationSetEnabled {
    type Params = AutomationSetEnabledParams;
    type Result = AutomationGetResult;

    const METHOD: ControlMethod = ControlMethod::AutomationSetEnabled;
}

pub enum AutomationRunOnce {}

impl ControlCall for AutomationRunOnce {
    type Params = AgentDefinitionIdParams;
    type Result = AutomationRunOnceResult;

    const METHOD: ControlMethod = ControlMethod::AutomationRunOnce;
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
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemDescribeResult {
    pub control: ControlProtocolDescription,
    pub daemon: DaemonRuntimeInfo,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionListResult {
    pub sessions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentListParams {
    pub source_session_id: String,
    pub source_token: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentListResult {
    pub agents: Vec<AgentEndpoint>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSendParams {
    pub source_session_id: String,
    pub source_token: String,
    pub command_id: smelt_plugin_api::CommandId,
    pub target: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSendResult {
    pub message_id: String,
    pub target_session_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentDefinitionListResult {
    pub agents: Vec<AgentDefinition>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentDefinitionIdParams {
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentDefinitionGetResult {
    pub agent: AgentDefinition,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentDefinitionUpdateParams {
    pub id: String,
    #[serde(flatten)]
    pub patch: AgentDefinitionPatch,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutomationListResult {
    pub automations: Vec<Automation>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutomationGetResult {
    pub automation: Automation,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutomationSetEnabledParams {
    pub id: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutomationRunOnceResult {
    pub result: AutomationCommandResult,
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
        assert_eq!(names.len(), 17);
    }

    #[test]
    fn typed_response_round_trips_and_checks_request_identity() {
        let response = ControlResponse::ok(
            "request-1",
            SessionListResult {
                sessions: vec!["acp-1".into(), "term-1".into()],
            },
        );
        let value = serde_json::to_value(response).expect("response should serialize");

        let result = decode_response::<SessionList>("request-1", value.clone())
            .expect("matching response should decode");
        assert_eq!(result.sessions, ["acp-1", "term-1"]);

        let error = decode_response::<SessionList>("another-request", value)
            .expect_err("a response from another request must not be accepted");
        assert!(matches!(error, ControlClientError::InvalidResponse(_)));
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
    fn typed_client_decodes_an_error_without_a_result_payload() {
        let response = ControlResponse::<serde_json::Value>::error(
            "send-1",
            ControlError::new(ControlErrorCode::OperationFailed, "target is busy"),
        );

        let error = decode_response::<AgentSend>(
            "send-1",
            serde_json::to_value(response).expect("response should serialize"),
        )
        .expect_err("operation failure must reach the caller");

        assert!(matches!(
            error,
            ControlClientError::Protocol(ControlError {
                code: ControlErrorCode::OperationFailed,
                ..
            })
        ));
    }

    #[test]
    fn supported_older_version_round_trips_in_the_requested_version() {
        let envelope = decode_request_envelope_in_range(
            serde_json::json!({
                "control_version": 1,
                "request_id": "older-client",
                "method": "session.list",
                "params": {},
            }),
            1,
            2,
        )
        .expect("version 1 should remain accepted by a 1..=2 server");
        let response = ControlResponse::ok_with_version(
            envelope.control_version,
            &envelope.request_id,
            SessionListResult {
                sessions: vec!["session-1".to_string()],
            },
        );

        let result = decode_response_with_version::<SessionList>(
            1,
            "older-client",
            serde_json::to_value(response).unwrap(),
        )
        .expect("the older client should decode a response in its requested version");

        assert_eq!(result.sessions, ["session-1"]);
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

    #[test]
    fn future_optional_request_fields_are_ignored() {
        let params = decode_params::<AgentSendParams>(serde_json::json!({
            "source_session_id": "source",
            "source_token": "token",
            "command_id": "agent-send-1",
            "target": "target",
            "message": "hello",
            "future_optional_hint": true,
        }))
        .expect("additive optional fields must not break an older v1 server");

        assert_eq!(params.message, "hello");
        assert_eq!(params.command_id.as_str(), "agent-send-1");
    }

    #[test]
    fn future_error_codes_degrade_to_unknown() {
        let error = decode_response::<AgentSend>(
            "future-error",
            serde_json::json!({
                "control_version": CONTROL_API_VERSION,
                "request_id": "future-error",
                "status": "error",
                "error": {
                    "code": "future_error_code",
                    "message": "a newer server error"
                }
            }),
        )
        .expect_err("an error response must remain an error");

        assert!(matches!(
            error,
            ControlClientError::Protocol(ControlError {
                code: ControlErrorCode::Unknown,
                ..
            })
        ));
    }
}
