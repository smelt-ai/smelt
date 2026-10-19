//! smeltd 控制 RPC 的具体方法：参数、结果类型和 `ControlCall` 绑定。
//!
//! 信封、方法名清单和 `system.describe` 在 `smelt_runtime_client::protocol::control`，
//! 那边必须能编进移动端；这里的类型依赖桌面领域模型，留在 core。

use serde::{Deserialize, Serialize};

use smelt_runtime_client::protocol::control::{ControlCall, ControlMethod, EmptyParams};

use crate::agent_bus::AgentEndpoint;
use crate::agent_definition::AgentDefinition;
use crate::agent_definition_store::{AgentDefinitionCreate, AgentDefinitionPatch};
use crate::agent_host::{AgentHostEdit, AgentHostSettings};
use crate::appearance_settings::{SettingsPatch, SettingsResult};
use crate::automation::{Automation, AutomationCommandResult};
use crate::launch_settings::{LaunchEntry, LaunchSettings};

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

/// 项目行「+」菜单的快捷启动项。
pub enum LaunchGet {}

impl ControlCall for LaunchGet {
    type Params = EmptyParams;
    type Result = LaunchSettings;

    const METHOD: ControlMethod = ControlMethod::LaunchGet;
}

/// 整表替换启动项。库里已不是 `base` 时回 `conflict`。
pub enum LaunchReplace {}

impl ControlCall for LaunchReplace {
    type Params = LaunchReplaceParams;
    type Result = LaunchSettings;

    const METHOD: ControlMethod = ControlMethod::LaunchReplace;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LaunchReplaceParams {
    /// 客户端看到的那份。
    pub base: Vec<LaunchEntry>,
    pub entries: Vec<LaunchEntry>,
}

/// 在运行时那台机器上真发一次 `/embeddings` 请求。服务地址常是 `localhost`，说的
/// 就是运行时那台机器，所以测试必须在那边跑。
pub enum MemoryTestEmbedding {}

impl ControlCall for MemoryTestEmbedding {
    type Params = MemoryTestParams;
    type Result = MemoryTestResult;

    const METHOD: ControlMethod = ControlMethod::MemoryTestEmbedding;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryTestParams {
    pub base_url: String,
    pub model: String,
    /// `None` 表示用已存的密钥。客户端看不到已存的密钥本身。
    pub api_key: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryTestResult {
    pub dimensions: usize,
    pub elapsed_ms: u64,
}

/// 设置页列出某个智能体的两层记忆（`id` 是智能体定义 id）。
pub enum AgentMemoryList {}

impl ControlCall for AgentMemoryList {
    type Params = AgentDefinitionIdParams;
    type Result = crate::agent_memory::AgentMemories;

    const METHOD: ControlMethod = ControlMethod::AgentMemoryList;
}

/// 设置页删除一条记忆。删了就没了，用户全局层的还会从所有智能体里消失。
pub enum AgentMemoryDelete {}

impl ControlCall for AgentMemoryDelete {
    type Params = AgentMemoryFileParams;
    type Result = EmptyParams;

    const METHOD: ControlMethod = ControlMethod::AgentMemoryDelete;
}

/// 设置页采纳一条待确认的记忆。
pub enum AgentMemoryApprove {}

impl ControlCall for AgentMemoryApprove {
    type Params = AgentMemoryFileParams;
    type Result = EmptyParams;

    const METHOD: ControlMethod = ControlMethod::AgentMemoryApprove;
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMemoryFileParams {
    pub agent_id: String,
    pub layer: AgentMemoryLayer,
    pub file_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMemoryDirParams {
    pub agent_id: String,
    pub layer: AgentMemoryLayer,
}

/// 确保某层记忆目录存在，返回它在运行时那台机器上的路径（给人在文件管理器里打开）。
pub enum AgentMemoryEnsureDir {}

impl ControlCall for AgentMemoryEnsureDir {
    type Params = AgentMemoryDirParams;
    type Result = AgentSpaceResult;

    const METHOD: ControlMethod = ControlMethod::AgentMemoryEnsureDir;
}

/// 一次自动化运行的详情：归档的完整运行（实时投影里裁掉了大字段时才取）和对话记录。
pub enum AutomationRunDetail {}

impl ControlCall for AutomationRunDetail {
    type Params = AutomationRunDetailParams;
    type Result = AutomationRunDetailResult;

    const METHOD: ControlMethod = ControlMethod::AutomationRunDetail;
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutomationRunDetailParams {
    pub run_id: String,
    /// 客户端手上的运行缺输出或指令时才要归档那份。
    pub include_archived_run: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AutomationRunDetailResult {
    pub archived_run: Option<crate::automation::AutomationRun>,
    pub transcript: Vec<crate::acp_chat::AcpEntry>,
}

/// 确保智能体的工作目录存在并返回它（运行时那台机器上的路径）。产品智能体的对话
/// 都开在这里。
pub enum AgentSpaceEnsure {}

impl ControlCall for AgentSpaceEnsure {
    type Params = AgentDefinitionIdParams;
    type Result = AgentSpaceResult;

    const METHOD: ControlMethod = ControlMethod::AgentSpaceEnsure;
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSpaceResult {
    pub path: String,
}

/// 探测运行时那台机器上各家 agent CLI 装没装、什么版本。只跑 `--version`。
pub enum AgentCliInspect {}

impl ControlCall for AgentCliInspect {
    type Params = EmptyParams;
    type Result = crate::acp_conn::AcpRuntimeDiagnostics;

    const METHOD: ControlMethod = ControlMethod::AgentCliInspect;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCliInstallParams {
    /// `ConversationAgentKind::id`。
    pub agent: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCliInstallResult {
    /// 用的包管理器（`brew` / `npm`）。
    pub installer: String,
    /// 装完重新探测的结果。
    pub diagnostics: crate::acp_conn::AcpRuntimeDiagnostics,
}

/// 用运行时那台机器上的包管理器安装某家 agent 的 CLI。装失败时返回业务错误。
pub enum AgentCliInstall {}

impl ControlCall for AgentCliInstall {
    type Params = AgentCliInstallParams;
    type Result = AgentCliInstallResult;

    const METHOD: ControlMethod = ControlMethod::AgentCliInstall;
}

/// 各家 agent 的 hooks 接入状态。
pub enum AgentHooksStatus {}

impl ControlCall for AgentHooksStatus {
    type Params = EmptyParams;
    type Result = crate::agent_hooks::AgentHooksStatus;

    const METHOD: ControlMethod = ControlMethod::AgentHooksStatus;
}

/// 给各家 agent 装上 Smelt 的 hooks（幂等，也用来校正被改坏的配置）。返回装完后的状态。
pub enum AgentHooksInstall {}

impl ControlCall for AgentHooksInstall {
    type Params = EmptyParams;
    type Result = crate::agent_hooks::AgentHooksStatus;

    const METHOD: ControlMethod = ControlMethod::AgentHooksInstall;
}

/// 移除各家 agent 配置里的 Smelt hooks，别人的条目原样保留。返回移除后的状态。
pub enum AgentHooksUninstall {}

impl ControlCall for AgentHooksUninstall {
    type Params = EmptyParams;
    type Result = crate::agent_hooks::AgentHooksStatus;

    const METHOD: ControlMethod = ControlMethod::AgentHooksUninstall;
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
    fn session_create_params_are_optional_except_the_project() {
        let params = decode_params::<SessionCreateParams>(serde_json::json!({
            "project_root": "/repo",
            "kind": "terminal",
            "request_id": "6f1c2b8e-4d5a-4f3b-9c2d-1e0f9a8b7c6d",
        }))
        .unwrap();
        assert_eq!(params.kind, Some(SessionCreateKind::Terminal));
        assert_eq!(params.agent_option_id, None);
        assert_eq!(
            serde_json::to_value(SessionCreateParams::default()).unwrap(),
            serde_json::json!({"project_root": ""})
        );
        assert!(decode_params::<SessionCreateParams>(serde_json::json!({"kind": "vnc"})).is_err());
    }

    #[test]
    fn new_session_catalog_decodes_on_the_client() {
        let wire = serde_json::json!({
            "control_version": CONTROL_API_VERSION,
            "request_id": "catalog-1",
            "status": "ok",
            "result": {
                "agents": [
                    {"id": "agent:quant", "kind": "pi", "label": "量化", "profile": false,
                     "agentDefinitionId": "quant"},
                    {"id": "claude", "kind": "claude", "label": "Claude Code", "profile": false}
                ],
                "sections": {
                    "common": [],
                    "terminal": [
                        {"key": "terminal:claude:claude", "label": "Claude Code", "kind": "terminal",
                         "pinned": false, "target": "terminal", "command": "claude",
                         "provider": "claude"},
                        {"key": "terminal:blank", "label": "空白终端", "kind": "terminal",
                         "pinned": true, "target": "blankTerminal"}
                    ],
                    "conversation": [
                        {"key": "conversation:agent:quant", "label": "量化", "kind": "conversation",
                         "pinned": false, "target": "conversation", "agentOptionId": "agent:quant",
                         "agentKind": "pi"}
                    ]
                }
            }
        });
        let catalog = decode_response::<NewSessionCatalog>("catalog-1", wire).unwrap();
        assert_eq!(catalog.agents[0].agent_definition_id(), Some("quant"));
        assert_eq!(catalog.agents[1].agent_definition_id, None);
        assert_eq!(
            catalog.sections.conversation[0].agent_option_id(),
            Some("agent:quant")
        );
        assert_eq!(
            catalog.sections.terminal[1].target,
            crate::new_session::NewSessionTarget::BlankTerminal
        );
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

/// 修改 Pi 配置的方法都返回改完之后的完整快照，客户端整份换掉缓存。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PiSaveCustomProviderResult {
    /// 模型目录是否由 Smelt 照端点维护。
    pub auto: bool,
    pub config: crate::pi_service::PiConfig,
}

macro_rules! pi_calls {
    ($( $(#[$doc:meta])* $call:ident => $method:ident ($params:ty) -> $result:ty; )+) => {
        $(
            $(#[$doc])*
            pub enum $call {}

            impl ControlCall for $call {
                type Params = $params;
                type Result = $result;

                const METHOD: ControlMethod = ControlMethod::$method;
            }
        )+
    };
}

pi_calls! {
    /// 能力库里 Pi 相关的全部展示数据。
    PiConfigGet => PiConfigGet(EmptyParams) -> crate::pi_service::PiConfig;
    /// 确保某个配置文件存在，返回它在运行时那台机器上的路径。
    PiConfigEnsureFile => PiConfigEnsureFile(crate::pi_service::EnsureConfigFileParams)
        -> crate::pi_service::EnsureConfigFileResult;
    PiModelsSetThinkingLevel => PiModelsSetThinkingLevel(crate::pi_service::ThinkingLevelParams)
        -> crate::pi_service::PiConfig;
    PiModelsSaveDefault => PiModelsSaveDefault(crate::pi_service::SaveDefaultModelParams)
        -> crate::pi_service::PiConfig;
    /// 问端点它提供哪些模型。没现填 key 时用已存的那份，客户端看不到它。
    PiProvidersDiscoverModels => PiProvidersDiscoverModels(crate::pi_service::DiscoverModelsParams)
        -> crate::pi_service::DiscoverModelsResult;
    /// 保存自定义 provider；模型列表为空时先问端点，并标成自动维护。
    PiProvidersSaveCustom => PiProvidersSaveCustom(crate::pi_service::SaveCustomProviderParams)
        -> PiSaveCustomProviderResult;
    PiProvidersRemoveCustom => PiProvidersRemoveCustom(crate::pi_service::ProviderIdParams)
        -> crate::pi_service::PiConfig;
    /// 照端点刷新所有自动维护的模型目录。
    PiProvidersRefreshAuto => PiProvidersRefreshAuto(EmptyParams) -> EmptyParams;
    PiMcpAddPreset => PiMcpAddPreset(crate::pi_service::McpPresetParams) -> crate::pi_service::PiConfig;
    PiMcpSetEnabled => PiMcpSetEnabled(crate::pi_service::McpEnabledParams) -> crate::pi_service::PiConfig;
    PiMcpRemove => PiMcpRemove(crate::pi_service::McpServerParams) -> crate::pi_service::PiConfig;
    /// 跑 `pi mcp login`，可能要等用户在浏览器里授权。
    PiMcpLogin => PiMcpLogin(crate::pi_service::McpServerParams) -> crate::pi_service::PiConfig;
    PiPluginsRemove => PiPluginsRemove(crate::pi_service::PluginIdParams) -> crate::pi_service::PiConfig;
    PiPluginsImport => PiPluginsImport(crate::pi_service::ImportPluginParams) -> crate::pi_service::PiConfig;
    /// 内置 provider 及其凭据现状（起一次凭据助手子进程）。
    PiAuthProviders => PiAuthProviders(EmptyParams) -> crate::pi_auth_service::ProvidersResult;
    /// 某个 provider 当前凭据能用的模型。
    PiAuthModels => PiAuthModels(crate::pi_service::ProviderIdParams) -> crate::pi_auth_service::ModelsResult;
    PiAuthLogout => PiAuthLogout(crate::pi_service::ProviderIdParams) -> EmptyParams;
    /// 开始一次登录，返回登录 id。之后用 `pi.auth.login_events` 长轮询事件。
    PiAuthLoginStart => PiAuthLoginStart(crate::pi_auth_service::LoginStartParams)
        -> crate::pi_auth_service::LoginIdParams;
    /// 取登录事件；没有新事件时最多等 20 秒。
    PiAuthLoginEvents => PiAuthLoginEvents(crate::pi_auth_service::LoginEventsParams)
        -> crate::pi_auth_service::LoginEventsResult;
    PiAuthLoginAnswer => PiAuthLoginAnswer(crate::pi_auth_service::LoginAnswerParams) -> EmptyParams;
    PiAuthLoginCancel => PiAuthLoginCancel(crate::pi_auth_service::LoginIdParams) -> EmptyParams;
    /// 能力库里 DeepSeek Harness 相关的全部展示数据。
    DshConfigGet => DshConfigGet(EmptyParams) -> crate::dsh_service::DshConfig;
    DshConfigEnsureFile => DshConfigEnsureFile(crate::dsh_service::EnsureConfigFileParams)
        -> crate::dsh_service::EnsureConfigFileResult;
    /// 每条路由支持哪些推理强度（起一个完整 dsh 运行时，几秒）。
    DshModelsCapabilities => DshModelsCapabilities(EmptyParams) -> crate::agent_kind::DshModelCapabilities;
    DshModelsSaveDefault => DshModelsSaveDefault(crate::dsh_service::SaveDefaultModelParams)
        -> crate::dsh_service::DshConfig;
    DshProvidersDiscoverModels => DshProvidersDiscoverModels(crate::agent_kind::DshDiscoveryRequest)
        -> crate::agent_kind::DshDiscoveryReport;
    /// 保存自定义 provider；模型列表为空时先问端点，并标成自动维护。
    DshProvidersSaveCustom => DshProvidersSaveCustom(crate::dsh_service::SaveCustomProviderParams)
        -> DshSaveCustomProviderResult;
    DshProvidersRefreshAuto => DshProvidersRefreshAuto(EmptyParams) -> EmptyParams;
    DshPluginsList => DshPluginsList(crate::dsh_service::ProfileParams) -> crate::dsh_service::PluginsResult;
    /// 安装、更新或卸载一个插件（pnpm，可能要几分钟），完了回带清单。
    DshPluginsRun => DshPluginsRun(crate::dsh_service::PluginActionParams)
        -> crate::dsh_service::PluginActionResult;
    /// 当前全部账户和缓存里的额度。
    QuotaGet => QuotaGet(EmptyParams) -> crate::quota_service::QuotaSnapshot;
    /// 查一轮额度（全部或一家）并更新缓存。每家一次网络往返。
    QuotaRefresh => QuotaRefresh(crate::quota_service::QuotaRefreshParams)
        -> crate::quota_service::QuotaRefreshResult;
    /// 远程访问的开关和自建 relay。存在运行时库，守护按它自愈。
    RemoteConfigGet => RemoteConfigGet(EmptyParams) -> crate::remote_config::RemoteConfig;
    /// 保存并照着开关 endpoint；绑定在后台进行，回的是保存后的状态。
    RemoteConfigSet => RemoteConfigSet(crate::remote_config::RemoteConfig)
        -> crate::remote_config::RemoteAccessState;
    RemoteStatus => RemoteStatus(EmptyParams) -> crate::remote_config::RemoteAccessState;
    /// 生成一次性配对串。远程访问要先开着。
    DevicePairBegin => DevicePairBegin(EmptyParams) -> crate::remote_config::PairingOffer;
    /// 新设备提交配对码，通过后这台设备进白名单。
    DevicePair => DevicePair(crate::remote_config::PairParams) -> EmptyParams;
    DeviceList => DeviceList(EmptyParams) -> crate::remote_config::DeviceList;
    /// 撤销并立即断开这台设备现有的全部连接。
    DeviceRevoke => DeviceRevoke(crate::remote_config::DeviceIdParams)
        -> crate::remote_config::RevokeResult;
    /// 新建菜单里用户标为「常用」的动作。桌面和手机共用这一份。
    NewSessionPinsGet => NewSessionPinsGet(EmptyParams) -> crate::new_session::NewSessionPreferences;
    NewSessionPinsToggle => NewSessionPinsToggle(crate::new_session::PinParams)
        -> crate::new_session::NewSessionPreferences;
    /// 桌面当前生效的终端配色，运行时转给移动端：手机渲染同一份 PTY 字节流，TUI 用
    /// OSC 11 查到的底色得和桌面实际底色一致。
    TerminalThemePublish => TerminalThemePublish(crate::terminal_theme::TerminalThemeSnapshot)
        -> EmptyParams;
    /// 把教智能体用 `smelt` 命令行的技能写到运行时那台机器上（内容没变就不写）。
    SkillsSyncControl => SkillsSyncControl(EmptyParams) -> EmptyParams;
    /// 用产品智能体定义开一场对话（`smelt agent start`）。
    ConversationCreateForAgent => ConversationCreateForAgent(AgentDefinitionIdParams)
        -> crate::session_control::CreatedAgentConversation;
    /// 新建会话的选择目录：可开对话的 agent 选项，和按「常用 / 终端 / 对话」分好组的
    /// 启动动作（含 Pin）。CLI 装没装按运行时的探测缓存过滤。
    NewSessionCatalog => NewSessionCatalog(EmptyParams) -> NewSessionCatalogResult;
    /// 在运行时上新建一个会话（对话或终端），会话进远程目录，桌面侧栏也看得到。
    SessionCreate => SessionCreate(SessionCreateParams) -> SessionCreateResult;
    /// 删除一个会话。运行时自己判断是终端还是对话；自动化运行的会话拒绝删除。
    SessionDelete => SessionDelete(SessionIdParams) -> EmptyParams;
    /// 会话 id → 它所属的产品智能体定义 id。
    SessionAgentBindings => SessionAgentBindings(EmptyParams) -> SessionAgentBindingsResult;
    /// 打开一段对话前补齐启动规格（智能体指令、插件、MCP、模型、记忆目录），顺带给出
    /// 会加载的技能。
    SessionLaunchPrepare => SessionLaunchPrepare(crate::agent_definition_store::LaunchPrepareParams)
        -> crate::agent_definition_store::LaunchPrepareResult;
    /// 一场 Pi 会话会加载哪些技能。
    SessionLoadedSkills => SessionLoadedSkills(crate::agent_definition_store::LoadedSkillsParams)
        -> crate::agent_definition_store::LoadedSkillsResult;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewSessionCatalogResult {
    /// 产品智能体在前，裸引擎与 profile 在后，与桌面新建菜单同序。不按探测结果过滤：
    /// 历史列表等地方要靠它把 option id 还原成引擎和 profile。
    pub agents: Vec<crate::session_control::AcpAgentOption>,
    pub sections: crate::new_session::NewSessionSections,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCreateKind {
    #[default]
    Acp,
    Terminal,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCreateParams {
    /// 必须是工作区菜单里的项目。对话可以留空：选项属于产品智能体时开在智能体自己的
    /// space，否则报错。
    #[serde(default)]
    pub project_root: String,
    /// 对话用哪个 agent 选项（`new_session.catalog` 的 `agents[].id`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_option_id: Option<String>,
    /// 新建目录里那一行的 key。给了它就由运行时决定开对话还是哪种终端，
    /// `kind` / `agent_option_id` 不再看。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_key: Option<String>,
    /// 续接哪段历史（只对对话有效）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_id: Option<String>,
    /// 不给就是对话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<SessionCreateKind>,
    /// 客户端为一次新建生成、重试时复用的 UUID。会话 id 由它确定，重试落到同一个会话；
    /// 同一个 `request_id` 带着不同参数再来回 `conflict`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCreateResult {
    pub session_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionIdParams {
    pub session_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAgentBindingsResult {
    /// 会话 id → 智能体定义 id。
    pub bindings: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DshSaveCustomProviderResult {
    pub auto: bool,
    pub config: crate::dsh_service::DshConfig,
}
