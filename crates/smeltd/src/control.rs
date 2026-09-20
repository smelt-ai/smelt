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
    AgentDefinitionUpdateParams, AgentListParams, AgentSendParams, AutomationGetResult,
    AutomationListResult, AutomationRunOnceResult, AutomationSetEnabledParams,
    CONTROL_API_MIN_SUPPORTED_VERSION, CONTROL_API_VERSION, ControlError, ControlErrorCode,
    ControlMethod, ControlProtocolDescription, ControlRequestEnvelope, ControlResponse,
    DaemonRuntimeInfo, EmptyParams, SUPPORTED_CONTROL_METHODS, SessionListResult,
    SystemDescribeResult, decode_params, decode_request_envelope,
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
                },
            };
            success_value(control_version, &request_id, result)
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
            match appearance_settings::update_settings(params).map_err(control_error_for_settings) {
                Ok(result) => success_value(control_version, &request_id, result),
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
    if applied.changed
        && let Err(error) = event_hub
            .publish_automations(&super::webhook::annotate_snapshot(applied.snapshot.clone()))
    {
        eprintln!("[control] 发布自动化投影失败: {error}");
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
    if applied.changed
        && let Err(error) = event_hub
            .publish_automations(&super::webhook::annotate_snapshot(applied.snapshot.clone()))
    {
        eprintln!("[control] 发布自动化投影失败: {error}");
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

    use smelt_core::control_api::{
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
