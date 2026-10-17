//! 版本化短控制 RPC 的服务端分发。
//!
//! 领域逻辑留在各自模块；这里仅负责 wire 解码、能力发现、类型转换和统一错误。

use std::io::Write;
use std::os::unix::net::UnixStream;

use serde::Serialize;
use smelt_core::agent_definition_store::{self, AgentDefinitionError};
use smelt_core::agent_event::AGENT_EVENT_VERSION;
use smelt_core::appearance_settings::{self, SettingsError};
use smelt_core::automation::{Automation, AutomationCommand, redact_automation_credentials};
use smelt_core::automation_store::AutomationStore;
use smelt_core::control_api::{
    AgentDefinitionGetResult, AgentDefinitionIdParams, AgentDefinitionListResult,
    AgentDefinitionUpdateParams, AgentListParams, AgentMemoryRecordReadParams,
    AgentMemoryRecordWriteParams, AgentMemorySessionParams, AgentSendParams, AutomationGetResult,
    AutomationListResult, AutomationRunOnceResult, AutomationSetEnabledParams, SessionListResult,
};
use smelt_runtime_client::protocol::control::{
    CONTROL_API_MIN_SUPPORTED_VERSION, CONTROL_API_VERSION, ControlError, ControlErrorCode,
    ControlMethod, ControlProtocolDescription, ControlRequestEnvelope, ControlResponse,
    DaemonRuntimeInfo, EmptyParams, SUPPORTED_CONTROL_METHODS, SystemDescribeResult, decode_params,
    decode_request_envelope,
};

use super::session_catalog::published_session_states;
use super::{AcpSessions, EventHubHandle, Sessions};

fn error_value(
    control_version: u16,
    request_id: impl Into<String>,
    error: ControlError,
) -> serde_json::Value {
    serde_json::to_value(ControlResponse::<serde_json::Value>::error_with_version(
        control_version,
        request_id,
        error,
    ))
    .expect("control error response must serialize")
}

fn success_value<T: Serialize>(
    control_version: u16,
    request_id: &str,
    result: T,
) -> serde_json::Value {
    serde_json::to_value(ControlResponse::ok_with_version(
        control_version,
        request_id,
        result,
    ))
    .unwrap_or_else(|error| {
        error_value(
            control_version,
            request_id,
            ControlError::new(ControlErrorCode::Internal, error.to_string()),
        )
    })
}

fn params_or_error<T: serde::de::DeserializeOwned>(
    control_version: u16,
    request_id: &str,
    params: serde_json::Value,
) -> Result<T, serde_json::Value> {
    decode_params(params).map_err(|error| error_value(control_version, request_id, error))
}

fn control_error_for_peer_messaging(
    error: super::peer_messaging::PeerMessagingError,
) -> ControlError {
    let code = match error {
        super::peer_messaging::PeerMessagingError::InvalidArgument(_) => {
            ControlErrorCode::InvalidParams
        }
        super::peer_messaging::PeerMessagingError::PermissionDenied(_) => {
            ControlErrorCode::PermissionDenied
        }
        super::peer_messaging::PeerMessagingError::NotFound(_) => ControlErrorCode::NotFound,
        super::peer_messaging::PeerMessagingError::FailedPrecondition(_) => {
            ControlErrorCode::FailedPrecondition
        }
        super::peer_messaging::PeerMessagingError::Conflict(_) => ControlErrorCode::Conflict,
        super::peer_messaging::PeerMessagingError::Busy(_) => ControlErrorCode::Busy,
        super::peer_messaging::PeerMessagingError::TemporarilyUnavailable(_) => {
            ControlErrorCode::TemporarilyUnavailable
        }
        super::peer_messaging::PeerMessagingError::ResultUnknown(_) => {
            ControlErrorCode::ResultUnknown
        }
        super::peer_messaging::PeerMessagingError::OperationFailed(_) => {
            ControlErrorCode::OperationFailed
        }
    };
    ControlError::new(code, error.to_string())
}

fn operation_error(
    control_version: u16,
    request_id: &str,
    error: super::peer_messaging::PeerMessagingError,
) -> serde_json::Value {
    error_value(
        control_version,
        request_id,
        control_error_for_peer_messaging(error),
    )
}

fn dispatch_control(
    envelope: ControlRequestEnvelope,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    automations: &AutomationStore,
    event_hub: &EventHubHandle,
    exe_mtime: u64,
) -> serde_json::Value {
    let ControlRequestEnvelope {
        control_version,
        request_id,
        method,
        params,
    } = envelope;
    let Some(known_method) = ControlMethod::from_wire_name(&method) else {
        return error_value(
            control_version,
            request_id,
            ControlError::new(
                ControlErrorCode::MethodNotFound,
                format!("unknown control method `{method}`"),
            ),
        );
    };
    match known_method {
        ControlMethod::SystemDescribe => {
            let _: EmptyParams = match params_or_error(control_version, &request_id, params) {
                Ok(params) => params,
                Err(response) => return response,
            };
            let result = SystemDescribeResult {
                control: ControlProtocolDescription {
                    current_version: CONTROL_API_VERSION,
                    min_supported_version: CONTROL_API_MIN_SUPPORTED_VERSION,
                    max_supported_version: CONTROL_API_VERSION,
                    methods: SUPPORTED_CONTROL_METHODS
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                },
                daemon: DaemonRuntimeInfo {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    exe_mtime,
                    exe: super::daemon_executable_path()
                        .ok()
                        .map(|path| path.to_string_lossy().into_owned()),
                    pid: std::process::id(),
                    started_at: super::started_at(),
                    session_count: published_session_states(sessions, acp_sessions).len(),
                    agent_event_version: AGENT_EVENT_VERSION,
                    runtime_id: super::runtime_identity::runtime_id().map(|id| id.to_string()),
                    os: Some(std::env::consts::OS.to_string()),
                    arch: Some(std::env::consts::ARCH.to_string()),
                },
            };
            success_value(control_version, &request_id, result)
        }
        git @ (ControlMethod::GitDiscover
        | ControlMethod::GitRepoInfo
        | ControlMethod::GitStatus
        | ControlMethod::GitBranches
        | ControlMethod::GitIsDirty
        | ControlMethod::GitCheckout
        | ControlMethod::GitBranchDelete
        | ControlMethod::GitMerge
        | ControlMethod::GitStage
        | ControlMethod::GitUnstage
        | ControlMethod::GitDiscardFile
        | ControlMethod::GitApplyPatch
        | ControlMethod::GitDiscardAll
        | ControlMethod::GitCommit
        | ControlMethod::GitPush
        | ControlMethod::GitFetch
        | ControlMethod::GitPull
        | ControlMethod::GitStashPush
        | ControlMethod::GitStashPop
        | ControlMethod::GitWorktreeList
        | ControlMethod::GitWorktreeCreate
        | ControlMethod::GitWorktreeRemove
        | ControlMethod::GitWorktreePrune
        | ControlMethod::GitFileDiff
        | ControlMethod::GitAllDiff
        | ControlMethod::GitLog
        | ControlMethod::GitCommitFiles
        | ControlMethod::GitCommitFileDiff) => match super::git_control::dispatch(git, params) {
            Ok(result) => success_value(control_version, &request_id, result),
            Err(error) => error_value(control_version, request_id, error),
        },
        files @ (ControlMethod::ProjectTree
        | ControlMethod::ProjectSearch
        | ControlMethod::ProjectDeleteEntry
        | ControlMethod::FileOpen
        | ControlMethod::FileSave
        | ControlMethod::SessionCompleteFiles) => {
            match super::files_control::dispatch(files, params) {
                Ok(result) => success_value(control_version, &request_id, result),
                Err(error) => error_value(control_version, request_id, error),
            }
        }
        history @ (ControlMethod::HistoryList
        | ControlMethod::HistoryDetail
        | ControlMethod::HistoryDelete
        | ControlMethod::HistoryTitles
        | ControlMethod::HistoryRememberAgent) => {
            match super::history_control::dispatch(history, params) {
                Ok(result) => success_value(control_version, &request_id, result),
                Err(error) => error_value(control_version, request_id, error),
            }
        }
        pi @ (ControlMethod::PiConfigGet
        | ControlMethod::PiConfigEnsureFile
        | ControlMethod::PiModelsSetThinkingLevel
        | ControlMethod::PiModelsSaveDefault
        | ControlMethod::PiProvidersDiscoverModels
        | ControlMethod::PiProvidersSaveCustom
        | ControlMethod::PiProvidersRemoveCustom
        | ControlMethod::PiProvidersRefreshAuto
        | ControlMethod::PiMcpAddPreset
        | ControlMethod::PiMcpSetEnabled
        | ControlMethod::PiMcpRemove
        | ControlMethod::PiMcpLogin
        | ControlMethod::PiPluginsRemove
        | ControlMethod::PiPluginsImport) => match super::pi_control::dispatch(pi, params) {
            Ok(result) => success_value(control_version, &request_id, result),
            Err(error) => error_value(control_version, request_id, error),
        },
        ControlMethod::AgentCliInspect => {
            let _: EmptyParams = match params_or_error(control_version, &request_id, params) {
                Ok(params) => params,
                Err(response) => return response,
            };
            success_value(
                control_version,
                &request_id,
                smelt_core::acp_conn::inspect_acp_runtime(),
            )
        }
        ControlMethod::AgentCliInstall => {
            let params: smelt_core::control_api::AgentCliInstallParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            let Some(agent) = smelt_core::agent_kind::ConversationAgentKind::from_id(&params.agent)
            else {
                return error_value(
                    control_version,
                    request_id,
                    ControlError::new(
                        ControlErrorCode::InvalidParams,
                        format!("未知的 agent：{}", params.agent),
                    ),
                );
            };
            match smelt_core::acp_conn::install_acp_cli(agent) {
                Ok(report) => success_value(
                    control_version,
                    &request_id,
                    smelt_core::control_api::AgentCliInstallResult {
                        installer: report.installer.to_string(),
                        diagnostics: smelt_core::acp_conn::inspect_acp_runtime(),
                    },
                ),
                Err(error) => error_value(
                    control_version,
                    request_id,
                    ControlError::new(ControlErrorCode::OperationFailed, error),
                ),
            }
        }
        ControlMethod::AgentHooksStatus => {
            let _: EmptyParams = match params_or_error(control_version, &request_id, params) {
                Ok(params) => params,
                Err(response) => return response,
            };
            success_value(
                control_version,
                &request_id,
                smelt_core::agent_hooks::hooks_status(),
            )
        }
        ControlMethod::AgentHooksInstall | ControlMethod::AgentHooksUninstall => {
            let _: EmptyParams = match params_or_error(control_version, &request_id, params) {
                Ok(params) => params,
                Err(response) => return response,
            };
            let result = if known_method == ControlMethod::AgentHooksInstall {
                smelt_core::agent_hooks::install_agent_hooks()
            } else {
                smelt_core::agent_hooks::uninstall_agent_hooks()
            };
            match result {
                Ok(()) => success_value(
                    control_version,
                    &request_id,
                    smelt_core::agent_hooks::hooks_status(),
                ),
                Err(error) => error_value(
                    control_version,
                    request_id,
                    ControlError::new(ControlErrorCode::OperationFailed, error),
                ),
            }
        }
        ControlMethod::MigrationTakeClientState => {
            let _: EmptyParams = match params_or_error(control_version, &request_id, params) {
                Ok(params) => params,
                Err(response) => return response,
            };
            match smelt_core::client_migration::take() {
                Ok(state) => success_value(control_version, &request_id, state),
                Err(error) => error_value(
                    control_version,
                    request_id,
                    ControlError::new(ControlErrorCode::OperationFailed, error),
                ),
            }
        }
        ControlMethod::AgentSpaceEnsure => {
            let params: AgentDefinitionIdParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match smelt_core::agent_definition_store::ensure_agent_space(&params.id) {
                Some(path) => success_value(
                    control_version,
                    &request_id,
                    smelt_core::control_api::AgentSpaceResult {
                        path: path.to_string_lossy().into_owned(),
                    },
                ),
                None => error_value(
                    control_version,
                    request_id,
                    ControlError::new(ControlErrorCode::OperationFailed, "无法创建智能体工作区"),
                ),
            }
        }
        ControlMethod::AutomationRunDetail => {
            let params: smelt_core::control_api::AutomationRunDetailParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            success_value(
                control_version,
                &request_id,
                smelt_core::control_api::AutomationRunDetailResult {
                    archived_run: params
                        .include_archived_run
                        .then(|| smelt_core::automation_store::load_run(&params.run_id))
                        .flatten(),
                    transcript: smelt_core::automation_transcript::load_run_transcript(
                        &params.run_id,
                    )
                    .unwrap_or_default(),
                },
            )
        }
        ControlMethod::AgentMemoryList => {
            let params: AgentDefinitionIdParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            success_value(
                control_version,
                &request_id,
                smelt_core::agent_memory::list_for_agent(&params.id),
            )
        }
        ControlMethod::AgentMemoryDelete | ControlMethod::AgentMemoryApprove => {
            let params: smelt_core::control_api::AgentMemoryFileParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            let result = if known_method == ControlMethod::AgentMemoryDelete {
                smelt_core::agent_memory::delete_for_agent(
                    &params.agent_id,
                    params.layer,
                    &params.file_name,
                )
            } else {
                smelt_core::agent_memory::approve_for_agent(
                    &params.agent_id,
                    params.layer,
                    &params.file_name,
                )
            };
            match result {
                Ok(()) => success_value(control_version, &request_id, EmptyParams::default()),
                Err(error) => error_value(
                    control_version,
                    request_id,
                    ControlError::new(ControlErrorCode::OperationFailed, error.to_string()),
                ),
            }
        }
        ControlMethod::WorkspaceLoad => {
            let _: EmptyParams = match params_or_error(control_version, &request_id, params) {
                Ok(params) => params,
                Err(response) => return response,
            };
            match smelt_core::workspace_catalog::load() {
                Ok(result) => success_value(control_version, &request_id, result),
                Err(error) => error_value(
                    control_version,
                    request_id,
                    ControlError::new(ControlErrorCode::OperationFailed, error.to_string()),
                ),
            }
        }
        ControlMethod::WorkspaceSave => {
            let params: smelt_runtime_client::protocol::workspace::WorkspaceSaveParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match smelt_core::workspace_catalog::save(params.base_revision, &params.document) {
                Ok(revision) => success_value(
                    control_version,
                    &request_id,
                    smelt_runtime_client::protocol::workspace::WorkspaceSaveResult { revision },
                ),
                Err(error) => {
                    let code = match error {
                        smelt_core::workspace_catalog::CatalogError::Conflict { .. } => {
                            ControlErrorCode::Conflict
                        }
                        smelt_core::workspace_catalog::CatalogError::Invalid(_) => {
                            ControlErrorCode::InvalidParams
                        }
                        smelt_core::workspace_catalog::CatalogError::Store(_) => {
                            ControlErrorCode::OperationFailed
                        }
                    };
                    error_value(
                        control_version,
                        request_id,
                        ControlError::new(code, error.to_string()),
                    )
                }
            }
        }
        ControlMethod::LaunchGet => {
            let _: EmptyParams = match params_or_error(control_version, &request_id, params) {
                Ok(params) => params,
                Err(response) => return response,
            };
            match smelt_core::launch_settings::load() {
                Ok(settings) => success_value(control_version, &request_id, settings),
                Err(error) => error_value(
                    control_version,
                    request_id,
                    ControlError::new(ControlErrorCode::OperationFailed, error),
                ),
            }
        }
        ControlMethod::LaunchReplace => {
            let params: smelt_core::control_api::LaunchReplaceParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match smelt_core::launch_settings::replace_entries(&params.base, params.entries) {
                Ok(settings) => success_value(control_version, &request_id, settings),
                Err(error) => {
                    let code = match error {
                        smelt_core::launch_settings::LaunchError::Conflict => {
                            ControlErrorCode::Conflict
                        }
                        smelt_core::launch_settings::LaunchError::Store(_) => {
                            ControlErrorCode::OperationFailed
                        }
                    };
                    error_value(
                        control_version,
                        request_id,
                        ControlError::new(code, error.to_string()),
                    )
                }
            }
        }
        ControlMethod::MemoryTestEmbedding => {
            let params: smelt_core::control_api::MemoryTestParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match memory_test_embedding(params) {
                Ok(result) => success_value(control_version, &request_id, result),
                Err(error) => error_value(
                    control_version,
                    request_id,
                    ControlError::new(ControlErrorCode::OperationFailed, error),
                ),
            }
        }
        ControlMethod::AgentHostGet => {
            let _: EmptyParams = match params_or_error(control_version, &request_id, params) {
                Ok(params) => params,
                Err(response) => return response,
            };
            match smelt_core::agent_host::load() {
                Ok(settings) => success_value(control_version, &request_id, settings),
                Err(error) => error_value(
                    control_version,
                    request_id,
                    ControlError::new(ControlErrorCode::OperationFailed, error),
                ),
            }
        }
        ControlMethod::AgentHostApplyEdit => {
            let edit: smelt_core::agent_host::AgentHostEdit =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match smelt_core::agent_host::apply_edit(&edit) {
                Ok(settings) => success_value(control_version, &request_id, settings),
                Err(error) => error_value(
                    control_version,
                    request_id,
                    ControlError::new(ControlErrorCode::OperationFailed, error),
                ),
            }
        }
        ControlMethod::SessionList => {
            let _: EmptyParams = match params_or_error(control_version, &request_id, params) {
                Ok(params) => params,
                Err(response) => return response,
            };
            let mut session_ids: Vec<_> = published_session_states(sessions, acp_sessions)
                .into_iter()
                .map(|state| state.id)
                .collect();
            session_ids.sort();
            success_value(
                control_version,
                &request_id,
                SessionListResult {
                    sessions: session_ids,
                },
            )
        }
        ControlMethod::AgentList => {
            let params: AgentListParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match super::peer_messaging::list_agents(&params, sessions, acp_sessions) {
                Ok(result) => success_value(control_version, &request_id, result),
                Err(error) => operation_error(control_version, &request_id, error),
            }
        }
        ControlMethod::AgentMemoryMeta => {
            let params: AgentMemorySessionParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match super::agent_memory_control::meta(&params, acp_sessions) {
                Ok(result) => success_value(control_version, &request_id, result),
                Err(error) => error_value(control_version, request_id, error),
            }
        }
        ControlMethod::AgentMemoryRecordRead => {
            let params: AgentMemoryRecordReadParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match super::agent_memory_control::record_read(&params, acp_sessions) {
                Ok(()) => success_value(control_version, &request_id, EmptyParams::default()),
                Err(error) => error_value(control_version, request_id, error),
            }
        }
        ControlMethod::AgentMemoryRecordWrite => {
            let params: AgentMemoryRecordWriteParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match super::agent_memory_control::record_write(&params, acp_sessions) {
                Ok(()) => success_value(control_version, &request_id, EmptyParams::default()),
                Err(error) => error_value(control_version, request_id, error),
            }
        }
        ControlMethod::AgentSend => {
            let params: AgentSendParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match super::peer_messaging::send_message(&params, sessions, acp_sessions, event_hub) {
                Ok(result) => success_value(control_version, &request_id, result),
                Err(error) => operation_error(control_version, &request_id, error),
            }
        }
        ControlMethod::AgentDefinitionList => {
            let _: EmptyParams = match params_or_error(control_version, &request_id, params) {
                Ok(params) => params,
                Err(response) => return response,
            };
            match definition_store().and_then(|store| {
                agent_definition_store::list_agent_definitions_on(&store)
                    .map_err(control_error_for_definition)
            }) {
                Ok(agents) => success_value(
                    control_version,
                    &request_id,
                    AgentDefinitionListResult { agents },
                ),
                Err(error) => error_value(control_version, request_id, error),
            }
        }
        ControlMethod::AgentDefinitionGet => {
            let params: AgentDefinitionIdParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match definition_store().and_then(|store| {
                agent_definition_store::get_agent_definition_on(&store, &params.id)
                    .map_err(control_error_for_definition)
            }) {
                Ok(agent) => success_value(
                    control_version,
                    &request_id,
                    AgentDefinitionGetResult { agent },
                ),
                Err(error) => error_value(control_version, request_id, error),
            }
        }
        ControlMethod::AgentDefinitionCreate => {
            let params: agent_definition_store::AgentDefinitionCreate =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match definition_store().and_then(|store| {
                agent_definition_store::create_agent_definition_on(&store, params)
                    .map_err(control_error_for_definition)
            }) {
                Ok(agent) => success_value(
                    control_version,
                    &request_id,
                    AgentDefinitionGetResult { agent },
                ),
                Err(error) => error_value(control_version, request_id, error),
            }
        }
        ControlMethod::AgentDefinitionUpdate => {
            let params: AgentDefinitionUpdateParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match definition_store().and_then(|store| {
                agent_definition_store::update_agent_definition_on(&store, &params.id, params.patch)
                    .map_err(control_error_for_definition)
            }) {
                Ok(agent) => success_value(
                    control_version,
                    &request_id,
                    AgentDefinitionGetResult { agent },
                ),
                Err(error) => error_value(control_version, request_id, error),
            }
        }
        ControlMethod::AgentDefinitionDelete => {
            let params: AgentDefinitionIdParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match definition_store().and_then(|store| {
                agent_definition_store::delete_agent_definition_on(&store, &params.id)
                    .map_err(control_error_for_definition)
            }) {
                Ok(()) => success_value(control_version, &request_id, params),
                Err(error) => error_value(control_version, request_id, error),
            }
        }
        ControlMethod::SettingsGet => {
            let _: EmptyParams = match params_or_error(control_version, &request_id, params) {
                Ok(params) => params,
                Err(response) => return response,
            };
            match appearance_settings::get_settings().map_err(control_error_for_settings) {
                Ok(result) => success_value(control_version, &request_id, result),
                Err(error) => error_value(control_version, request_id, error),
            }
        }
        ControlMethod::SettingsUpdate => {
            let params: appearance_settings::SettingsPatch =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            // 外观归桌面端：没有在线的桌面端就整单拒绝，别让记忆配置写了一半。
            let wants_appearance = params
                .settings
                .keys()
                .any(|key| !smelt_core::memory_settings::is_memory_key(key));
            if wants_appearance && !event_hub.has_appearance_listener().unwrap_or(false) {
                return error_value(
                    control_version,
                    request_id,
                    ControlError::new(
                        ControlErrorCode::FailedPrecondition,
                        "外观属于桌面客户端，现在没有打开的桌面客户端来接收这次修改",
                    ),
                );
            }
            match appearance_settings::update_settings(params).map_err(control_error_for_settings) {
                Ok(update) => {
                    if !update.appearance.is_empty()
                        && let Err(error) = event_hub.request_appearance_change(update.appearance)
                    {
                        return error_value(
                            control_version,
                            request_id,
                            ControlError::new(ControlErrorCode::OperationFailed, error.to_string()),
                        );
                    }
                    success_value(control_version, &request_id, update.result)
                }
                Err(error) => error_value(control_version, request_id, error),
            }
        }
        ControlMethod::AutomationList => {
            let _: EmptyParams = match params_or_error(control_version, &request_id, params) {
                Ok(params) => params,
                Err(response) => return response,
            };
            let snapshot = redact_automation_credentials(automations.lock().unwrap().snapshot());
            success_value(
                control_version,
                &request_id,
                AutomationListResult {
                    automations: snapshot.automations,
                },
            )
        }
        ControlMethod::AutomationGet => {
            let params: AgentDefinitionIdParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match find_automation(automations, &params.id) {
                Ok(automation) => success_value(
                    control_version,
                    &request_id,
                    AutomationGetResult { automation },
                ),
                Err(error) => error_value(control_version, request_id, error),
            }
        }
        ControlMethod::AutomationUpsert => {
            let automation: Automation = match params_or_error(control_version, &request_id, params)
            {
                Ok(params) => params,
                Err(response) => return response,
            };
            let automation_id = automation.id.clone();
            match apply_automation(
                automations,
                event_hub,
                AutomationCommand::Upsert {
                    automation: Box::new(automation),
                },
            )
            .and_then(|_| find_automation(automations, &automation_id))
            {
                Ok(automation) => success_value(
                    control_version,
                    &request_id,
                    AutomationGetResult { automation },
                ),
                Err(error) => error_value(control_version, request_id, error),
            }
        }
        ControlMethod::AutomationDelete => {
            let params: AgentDefinitionIdParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match apply_automation(
                automations,
                event_hub,
                AutomationCommand::Delete {
                    automation_id: params.id.clone(),
                },
            ) {
                Ok(_) => success_value(control_version, &request_id, params),
                Err(error) => error_value(control_version, request_id, error),
            }
        }
        ControlMethod::AutomationSetEnabled => {
            let params: AutomationSetEnabledParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match apply_automation(
                automations,
                event_hub,
                AutomationCommand::SetEnabled {
                    automation_id: params.id.clone(),
                    enabled: params.enabled,
                },
            )
            .and_then(|_| find_automation(automations, &params.id))
            {
                Ok(automation) => success_value(
                    control_version,
                    &request_id,
                    AutomationGetResult { automation },
                ),
                Err(error) => error_value(control_version, request_id, error),
            }
        }
        ControlMethod::AutomationRunOnce => {
            let params: AgentDefinitionIdParams =
                match params_or_error(control_version, &request_id, params) {
                    Ok(params) => params,
                    Err(response) => return response,
                };
            match run_automation_once(automations, event_hub, &params.id) {
                Ok(result) => success_value(
                    control_version,
                    &request_id,
                    AutomationRunOnceResult { result },
                ),
                Err(error) => error_value(control_version, request_id, error),
            }
        }
    }
}

/// 用表单里的地址和模型真发一次 `/embeddings` 请求；没给密钥就用库里存的。
fn memory_test_embedding(
    params: smelt_core::control_api::MemoryTestParams,
) -> Result<smelt_core::control_api::MemoryTestResult, String> {
    let store = smelt_core::sqlite_state::default_sqlite_store()?;
    let stored = smelt_core::memory_settings::load_on(&store).map_err(|error| error.to_string())?;
    let settings = smelt_core::memory_settings::EmbeddingSettings {
        base_url: params.base_url.trim().to_string(),
        model: params.model.trim().to_string(),
        api_key: params
            .api_key
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
            .unwrap_or(stored.api_key),
    };
    let probe = smelt_core::block_on::block_on_tokio(smelt_core::memory_settings::probe(&settings))
        .map_err(|error| error.to_string())??;
    Ok(smelt_core::control_api::MemoryTestResult {
        dimensions: probe.dimensions,
        elapsed_ms: u64::try_from(probe.elapsed_ms).unwrap_or(u64::MAX),
    })
}

fn definition_store() -> Result<smelt_store::Store, ControlError> {
    smelt_core::sqlite_state::default_sqlite_store()
        .map_err(|error| ControlError::new(ControlErrorCode::TemporarilyUnavailable, error))
}

fn control_error_for_definition(error: AgentDefinitionError) -> ControlError {
    let code = match error {
        AgentDefinitionError::Invalid(_) => ControlErrorCode::InvalidParams,
        AgentDefinitionError::NotFound(_) => ControlErrorCode::NotFound,
        AgentDefinitionError::Conflict(_) => ControlErrorCode::Conflict,
        AgentDefinitionError::FailedPrecondition(_) => ControlErrorCode::FailedPrecondition,
        AgentDefinitionError::Store(_) => ControlErrorCode::OperationFailed,
    };
    ControlError::new(code, error.to_string())
}

fn control_error_for_settings(error: SettingsError) -> ControlError {
    match error {
        SettingsError::Invalid(message) => {
            ControlError::new(ControlErrorCode::InvalidParams, message)
        }
        SettingsError::Store(message) => {
            ControlError::new(ControlErrorCode::OperationFailed, message)
        }
    }
}

fn control_error_for_automation(error: String) -> ControlError {
    let code = if error.contains("不存在") {
        ControlErrorCode::NotFound
    } else if error.contains("运行中") {
        ControlErrorCode::Busy
    } else {
        ControlErrorCode::FailedPrecondition
    };
    ControlError::new(code, error)
}

fn find_automation(automations: &AutomationStore, id: &str) -> Result<Automation, ControlError> {
    let snapshot = redact_automation_credentials(automations.lock().unwrap().snapshot());
    snapshot
        .automations
        .into_iter()
        .find(|automation| automation.id == id)
        .ok_or_else(|| ControlError::new(ControlErrorCode::NotFound, format!("自动化不存在: {id}")))
}

fn apply_automation(
    automations: &AutomationStore,
    event_hub: &EventHubHandle,
    command: AutomationCommand,
) -> Result<smelt_core::automation::AutomationFile, ControlError> {
    super::recover_locked_automations(automations, event_hub);
    let mut owner = automations.lock().unwrap();
    let applied = owner
        .apply(command, chrono::Local::now())
        .map_err(control_error_for_automation)?;
    let changed = applied.changed;
    if changed
        && let Err(error) = event_hub
            .publish_automations(&super::webhook::annotate_snapshot(applied.snapshot.clone()))
    {
        eprintln!("[control] 发布自动化投影失败: {error}");
    }
    if changed {
        crate::automation_runtime::nudge_automation_driver();
    }
    Ok(redact_automation_credentials(applied.snapshot))
}

fn run_automation_once(
    automations: &AutomationStore,
    event_hub: &EventHubHandle,
    automation_id: &str,
) -> Result<smelt_core::automation::AutomationCommandResult, ControlError> {
    super::recover_locked_automations(automations, event_hub);
    let mut owner = automations.lock().unwrap();
    let applied = owner
        .run_once(automation_id.to_string(), chrono::Local::now())
        .map_err(control_error_for_automation)?;
    let changed = applied.changed;
    if changed
        && let Err(error) = event_hub
            .publish_automations(&super::webhook::annotate_snapshot(applied.snapshot.clone()))
    {
        eprintln!("[control] 发布自动化投影失败: {error}");
    }
    if changed {
        crate::automation_runtime::nudge_automation_driver();
    }
    Ok(applied.result)
}

pub(crate) fn handle_control(
    mut conn: UnixStream,
    payload: serde_json::Value,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    automations: &AutomationStore,
    event_hub: &EventHubHandle,
    exe_mtime: u64,
) {
    let response = match decode_request_envelope(payload) {
        Ok(envelope) => dispatch_control(
            envelope,
            sessions,
            acp_sessions,
            automations,
            event_hub,
            exe_mtime,
        ),
        Err(failure) => error_value(failure.response_version, failure.request_id, failure.error),
    };
    let _ = writeln!(conn, "{response}");
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;

    use smelt_runtime_client::protocol::control::{
        CONTROL_API_VERSION, ControlErrorCode, ControlOutcome, ControlResponse, EmptyParams,
        SystemDescribe, SystemDescribeResult, decode_request_envelope_in_range, encode_request,
    };

    use super::*;

    #[test]
    fn describe_is_a_typed_short_rpc_and_echoes_request_id() {
        let sessions = super::super::new_sessions();
        let acp_sessions = super::super::new_test_acp_sessions();
        let event_hub = super::super::new_event_hub();
        let automations = smelt_core::automation_store::new_automation_store();
        let request = encode_request::<SystemDescribe>("describe-1", &EmptyParams::default())
            .expect("request should encode");
        let payload = request
            .as_object()
            .expect("request should be an object")
            .iter()
            .filter(|(key, _)| key.as_str() != "op")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let (server, client) = UnixStream::pair().unwrap();

        handle_control(
            server,
            serde_json::Value::Object(payload),
            &sessions,
            &acp_sessions,
            &automations,
            &event_hub,
            123,
        );

        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        let response: ControlResponse<SystemDescribeResult> =
            serde_json::from_str(&line).expect("response should follow the contract");
        assert_eq!(response.control_version, CONTROL_API_VERSION);
        assert_eq!(response.request_id, "describe-1");
        let ControlOutcome::Ok { result } = response.outcome else {
            panic!("describe should succeed");
        };
        assert_eq!(result.daemon.exe_mtime, 123);
        assert_eq!(
            result.control.methods,
            SUPPORTED_CONTROL_METHODS
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn dispatcher_responds_in_the_validated_request_version() {
        let sessions = super::super::new_sessions();
        let acp_sessions = super::super::new_test_acp_sessions();
        let event_hub = super::super::new_event_hub();
        let automations = smelt_core::automation_store::new_automation_store();
        let envelope = decode_request_envelope_in_range(
            serde_json::json!({
                "control_version": 2,
                "request_id": "version-2",
                "method": "session.list",
                "params": {},
            }),
            1,
            2,
        )
        .unwrap();

        let response = dispatch_control(
            envelope,
            &sessions,
            &acp_sessions,
            &automations,
            &event_hub,
            0,
        );

        assert_eq!(response["control_version"], 2);
        assert_eq!(response["request_id"], "version-2");
        assert_eq!(response["status"], "ok");
    }

    #[test]
    fn protocol_errors_are_structured_and_correlated() {
        let sessions = super::super::new_sessions();
        let acp_sessions = super::super::new_test_acp_sessions();
        let event_hub = super::super::new_event_hub();
        let automations = smelt_core::automation_store::new_automation_store();
        let (server, client) = UnixStream::pair().unwrap();

        handle_control(
            server,
            serde_json::json!({
                "control_version": CONTROL_API_VERSION + 1,
                "request_id": "newer-client",
                "method": "system.describe",
                "params": {},
            }),
            &sessions,
            &acp_sessions,
            &automations,
            &event_hub,
            0,
        );

        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        let response: ControlResponse<serde_json::Value> = serde_json::from_str(&line).unwrap();
        assert_eq!(response.request_id, "newer-client");
        let ControlOutcome::Error { error } = response.outcome else {
            panic!("unsupported version should fail");
        };
        assert_eq!(error.code, ControlErrorCode::UnsupportedVersion);
    }

    #[test]
    fn unknown_method_does_not_fall_through_to_a_legacy_handler() {
        let sessions = super::super::new_sessions();
        let acp_sessions = super::super::new_test_acp_sessions();
        let event_hub = super::super::new_event_hub();
        let automations = smelt_core::automation_store::new_automation_store();
        let (server, client) = UnixStream::pair().unwrap();

        handle_control(
            server,
            serde_json::json!({
                "control_version": CONTROL_API_VERSION,
                "request_id": "unknown-1",
                "method": "plugin.guess",
                "params": {},
            }),
            &sessions,
            &acp_sessions,
            &automations,
            &event_hub,
            0,
        );

        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        let response: ControlResponse<serde_json::Value> = serde_json::from_str(&line).unwrap();
        let ControlOutcome::Error { error } = response.outcome else {
            panic!("unknown method should fail");
        };
        assert_eq!(error.code, ControlErrorCode::MethodNotFound);
    }

    #[test]
    fn peer_messaging_errors_keep_machine_readable_categories() {
        let cases = [
            (
                super::super::peer_messaging::PeerMessagingError::InvalidArgument(
                    "bad input".into(),
                ),
                ControlErrorCode::InvalidParams,
            ),
            (
                super::super::peer_messaging::PeerMessagingError::PermissionDenied(
                    "bad token".into(),
                ),
                ControlErrorCode::PermissionDenied,
            ),
            (
                super::super::peer_messaging::PeerMessagingError::NotFound("missing target".into()),
                ControlErrorCode::NotFound,
            ),
            (
                super::super::peer_messaging::PeerMessagingError::Busy("target busy".into()),
                ControlErrorCode::Busy,
            ),
            (
                super::super::peer_messaging::PeerMessagingError::TemporarilyUnavailable(
                    "daemon upgrading".into(),
                ),
                ControlErrorCode::TemporarilyUnavailable,
            ),
            (
                super::super::peer_messaging::PeerMessagingError::ResultUnknown(
                    "delivery result unknown".into(),
                ),
                ControlErrorCode::ResultUnknown,
            ),
        ];

        for (domain_error, expected_code) in cases {
            assert_eq!(
                control_error_for_peer_messaging(domain_error).code,
                expected_code
            );
        }
    }
}
