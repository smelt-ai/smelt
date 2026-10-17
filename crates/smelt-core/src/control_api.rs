//! smeltd 控制 RPC 的具体方法：参数、结果类型和 `ControlCall` 绑定。
//!
//! 信封、方法名清单和 `system.describe` 在 `smelt_runtime_client::protocol::control`，
//! 那边必须能编进移动端；这里的类型依赖桌面领域模型，留在 core。

use serde::{Deserialize, Serialize};

use smelt_runtime_client::protocol::control::{ControlCall, ControlMethod, EmptyParams};

use crate::agent_bus::AgentEndpoint;
use crate::agent_host::{AgentHostEdit, AgentHostSettings};
use crate::agent_definition::AgentDefinition;
use crate::agent_definition_store::{AgentDefinitionCreate, AgentDefinitionPatch};
use crate::appearance_settings::{SettingsPatch, SettingsResult};
use crate::automation::{Automation, AutomationCommandResult};

/// 运行时的智能体宿主配置（各家 CLI 命令、环境变量、profile、开关、智能体定义）。
pub enum AgentHostGet {}

impl ControlCall for AgentHostGet {
    type Params = EmptyParams;
    type Result = AgentHostSettings;

    const METHOD: ControlMethod = ControlMethod::AgentHostGet;
}

/// 把客户端的一次修改合并进运行时配置，返回合并后的整份。
pub enum AgentHostApplyEdit {}

impl ControlCall for AgentHostApplyEdit {
    type Params = AgentHostEdit;
    type Result = AgentHostSettings;

    const METHOD: ControlMethod = ControlMethod::AgentHostApplyEdit;
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

/// 智能体记忆的元数据（使用统计、来源）在主库，由主 daemon 代 Pi 读写。调用方用会话 id
/// 和 daemon 发给该会话的 capability token 证明身份；记忆层由 daemon 按会话的启动规格
/// 决定，调用方只能操作自己这个会话的两层记忆。见 `docs/agent-memory.md`。
pub enum AgentMemoryMeta {}

impl ControlCall for AgentMemoryMeta {
    type Params = AgentMemorySessionParams;
    type Result = AgentMemoryMetaResult;

    const METHOD: ControlMethod = ControlMethod::AgentMemoryMeta;
}

pub enum AgentMemoryRecordRead {}

impl ControlCall for AgentMemoryRecordRead {
    type Params = AgentMemoryRecordReadParams;
    type Result = EmptyParams;

    const METHOD: ControlMethod = ControlMethod::AgentMemoryRecordRead;
}

pub enum AgentMemoryRecordWrite {}

impl ControlCall for AgentMemoryRecordWrite {
    type Params = AgentMemoryRecordWriteParams;
    type Result = EmptyParams;

    const METHOD: ControlMethod = ControlMethod::AgentMemoryRecordWrite;
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
    pub command_id: smelt_event_protocol::CommandId,
    pub target: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSendResult {
    pub message_id: String,
    pub target_session_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMemorySessionParams {
    pub session_id: String,
    pub token: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentMemoryLayer {
    /// 这个智能体自己的记忆。
    Agent,
    /// 跨智能体的用户全局记忆。
    User,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentMemoryTrust {
    Trusted,
    Pending,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMemoryRecordReadParams {
    pub session_id: String,
    pub token: String,
    pub layer: AgentMemoryLayer,
    pub file: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMemoryRecordWriteParams {
    pub session_id: String,
    pub token: String,
    pub layer: AgentMemoryLayer,
    pub file: String,
    pub trust: AgentMemoryTrust,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMemoryUsageEntry {
    pub file: String,
    pub read_count: u64,
    pub last_read_at_ms: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMemoryLayerMeta {
    pub usage: Vec<AgentMemoryUsageEntry>,
    /// 待确认的文件名。
    pub pending: Vec<String>,
}

/// 会话没有某一层记忆（普通会话）时，那一层为空。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMemoryMetaResult {
    /// 智能体当前是否开着记忆。会话启动后用户可能关掉，关掉后不注入、不写入。
    pub enabled: bool,
    pub agent: AgentMemoryLayerMeta,
    pub user: AgentMemoryLayerMeta,
    /// 用户配置的向量检索服务；没配为空，Pi 退回关键词 + 模型重排。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<AgentMemoryEmbedding>,
    /// 用户指定的重排模型；没指定为空，Pi 自动挑。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_model: Option<AgentMemoryRerankModel>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMemoryRerankModel {
    pub provider: String,
    pub id: String,
}

/// OpenAI 兼容的 `/embeddings` 服务。只发给通过认证的会话。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMemoryEmbedding {
    pub base_url: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use smelt_runtime_client::protocol::control::*;

    /// Pi 启动器（`packages/pi-agent/src/memory-host.ts`）按这份样例解析 daemon 的回复。
    /// 两边共用同一个文件，任何一边改了线上格式都会让对方的测试失败。
    #[test]
    fn agent_memory_meta_response_matches_the_pi_client_fixture() {
        let response = ControlResponse::ok(
            "req-1",
            AgentMemoryMetaResult {
                enabled: true,
                embedding: None,
                rerank_model: None,
                agent: AgentMemoryLayerMeta {
                    usage: vec![AgentMemoryUsageEntry {
                        file: "fact.md".into(),
                        read_count: 2,
                        last_read_at_ms: 1000,
                    }],
                    pending: vec!["x.md".into()],
                },
                user: AgentMemoryLayerMeta::default(),
            },
        );
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../packages/pi-agent/tests/fixtures/agent-memory-meta-response.json"
        ))
        .unwrap();
        assert_eq!(serde_json::to_value(response).unwrap(), fixture);
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
