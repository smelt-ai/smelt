//! `agent.memory.*` 控制方法：替 Pi 读写智能体记忆的元数据（使用统计、来源）。
//!
//! Pi 进程不碰数据库。它带着会话 id 和 daemon 发给该会话的 capability token 调这里，
//! 由主 daemon 经 `smelt-store` 读写主库。记忆层取自会话的
//! 启动规格，调用方只能操作自己这个会话的两层记忆。见 `docs/agent-memory.md`。

use std::path::PathBuf;

use smelt_core::agent_memory::{self, MemoryRequestError, MemoryScope};
use smelt_core::control_api::{
    AgentMemoryLayer, AgentMemoryMetaResult, AgentMemoryRecordReadParams,
    AgentMemoryRecordWriteParams, AgentMemorySessionParams,
};
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode};

use super::{AcpSessions, Phase, acp_runtime_alive};

/// 一次通过认证的会话：它的两层记忆目录。
struct MemoryLaunch {
    agent_dir: Option<PathBuf>,
    user_dir: Option<PathBuf>,
}

fn authenticate(
    acp_sessions: &AcpSessions,
    session_id: &str,
    token: &str,
) -> Result<MemoryLaunch, ControlError> {
    let denied = || {
        ControlError::new(
            ControlErrorCode::PermissionDenied,
            "session capability is invalid or expired",
        )
    };
    let slot = acp_sessions.get(session_id).ok_or_else(denied)?;
    acp_sessions
        .with_current(session_id, &slot, |session| {
            let state = session.state.lock();
            let valid = !token.is_empty()
                && state.agent_token == token
                && state.phase != Phase::Dead
                && acp_runtime_alive(session);
            drop(state);
            if !valid {
                return Err(denied());
            }
            let launch = session.launch_spec.lock();
            let dir = |name: &str| {
                launch
                    .as_ref()
                    .and_then(|launch| launch.env.get(name))
                    .map(PathBuf::from)
            };
            Ok(MemoryLaunch {
                agent_dir: dir(smelt_core::agent_kind::SMELT_AGENT_MEMORY_DIR_ENV),
                user_dir: dir(smelt_core::agent_kind::SMELT_USER_MEMORY_DIR_ENV),
            })
        })
        .unwrap_or_else(|| Err(denied()))
}

fn store() -> Result<smelt_store::Store, ControlError> {
    smelt_core::sqlite_state::default_sqlite_store()
        .map_err(|error| ControlError::new(ControlErrorCode::TemporarilyUnavailable, error))
}

fn request_error(error: MemoryRequestError) -> ControlError {
    let code = match error {
        MemoryRequestError::NoSuchLayer => ControlErrorCode::FailedPrecondition,
        MemoryRequestError::InvalidFile => ControlErrorCode::InvalidParams,
        MemoryRequestError::Disabled => ControlErrorCode::FailedPrecondition,
        MemoryRequestError::Store(_) => ControlErrorCode::OperationFailed,
    };
    ControlError::new(code, error.to_string())
}

/// 会话启动后用户可能关掉了记忆：关着就不再记任何东西。
fn ensure_enabled(store: &smelt_store::Store, launch: &MemoryLaunch) -> Result<(), ControlError> {
    if agent_memory::launch_memory_enabled(store, launch.agent_dir.as_deref()) {
        Ok(())
    } else {
        Err(request_error(MemoryRequestError::Disabled))
    }
}

fn scope(launch: &MemoryLaunch, layer: AgentMemoryLayer) -> Result<MemoryScope, ControlError> {
    let result = match layer {
        AgentMemoryLayer::Agent => agent_memory::launch_scope(launch.agent_dir.as_deref(), false),
        AgentMemoryLayer::User => agent_memory::launch_scope(launch.user_dir.as_deref(), true),
    };
    result.map_err(request_error)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub(crate) fn meta(
    params: &AgentMemorySessionParams,
    acp_sessions: &AcpSessions,
) -> Result<AgentMemoryMetaResult, ControlError> {
    let launch = authenticate(acp_sessions, &params.session_id, &params.token)?;
    agent_memory::launch_memory_meta(
        &store()?,
        launch.agent_dir.as_deref(),
        launch.user_dir.as_deref(),
    )
    .map_err(request_error)
}

pub(crate) fn record_read(
    params: &AgentMemoryRecordReadParams,
    acp_sessions: &AcpSessions,
) -> Result<(), ControlError> {
    let launch = authenticate(acp_sessions, &params.session_id, &params.token)?;
    let scope = scope(&launch, params.layer)?;
    let store = store()?;
    ensure_enabled(&store, &launch)?;
    agent_memory::record_launch_read(&store, &scope, &params.file, now_ms()).map_err(request_error)
}

pub(crate) fn record_write(
    params: &AgentMemoryRecordWriteParams,
    acp_sessions: &AcpSessions,
) -> Result<(), ControlError> {
    let launch = authenticate(acp_sessions, &params.session_id, &params.token)?;
    let scope = scope(&launch, params.layer)?;
    let store = store()?;
    ensure_enabled(&store, &launch)?;
    agent_memory::record_launch_write(
        &store,
        &scope,
        &params.file,
        params.trust,
        &params.reason,
        &params.session_id,
        now_ms(),
    )
    .map_err(request_error)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn make_session(registry: &AcpSessions, id: &str, token: &str, memory: bool) {
        let (slot, _) = registry.reserve_with(id, || {
            super::super::make_acp_session(
                id,
                None,
                Some(smelt_core::conversation::ConversationBinding::Direct),
                None,
            )
        });
        {
            let mut state = slot.value.state.lock();
            state.agent_token = token.to_string();
            state.phase = Phase::Idle;
        }
        let mut launch = smelt_core::agent_kind::ConversationAgentKind::Pi.default_launch();
        if memory {
            smelt_core::agent_definition_store::attach_agent_memory_dirs(
                &mut launch,
                Some("writer"),
            );
        }
        *slot.value.launch_spec.lock() = Some(launch);
        let (cmd_tx, _cmd_rx) = smol::channel::unbounded();
        let (_event_tx, event_rx) = smol::channel::unbounded();
        *slot.value.handle.lock() = Some(smelt_core::acp_conn::ConversationHandle {
            cmd_tx,
            event_rx,
            stdio: Arc::new(Mutex::new(None)),
            in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            supports_mid_turn_input: false,
            supports_compaction: false,
            supports_native_queue: false,
            supports_rewind: false,
        });
    }

    #[test]
    fn only_the_session_holding_the_token_reaches_its_own_memory_layers() {
        let acp_sessions = super::super::new_test_acp_sessions();
        make_session(&acp_sessions, "acp-agent", "token-1", true);
        make_session(&acp_sessions, "acp-plain", "token-2", false);

        for (id, token) in [
            ("acp-agent", "wrong"),
            ("acp-agent", ""),
            ("missing", "token-1"),
        ] {
            let error = authenticate(&acp_sessions, id, token).err().unwrap();
            assert_eq!(
                error.code,
                ControlErrorCode::PermissionDenied,
                "{id}/{token}"
            );
        }

        let launch = authenticate(&acp_sessions, "acp-agent", "token-1").unwrap();
        assert_eq!(
            scope(&launch, AgentMemoryLayer::Agent).unwrap(),
            MemoryScope::Agent("writer".into())
        );
        assert_eq!(
            scope(&launch, AgentMemoryLayer::User).unwrap(),
            MemoryScope::User
        );

        // 普通会话的 token 是真的，但它没有记忆层。
        let plain = authenticate(&acp_sessions, "acp-plain", "token-2").unwrap();
        let error = scope(&plain, AgentMemoryLayer::Agent).unwrap_err();
        assert_eq!(error.code, ControlErrorCode::FailedPrecondition);
    }

    #[test]
    fn a_dead_session_loses_its_memory_capability() {
        let acp_sessions = super::super::new_test_acp_sessions();
        make_session(&acp_sessions, "acp-agent", "token-1", true);
        acp_sessions
            .get("acp-agent")
            .unwrap()
            .value
            .state
            .lock()
            .phase = Phase::Dead;
        assert!(authenticate(&acp_sessions, "acp-agent", "token-1").is_err());
    }
}
