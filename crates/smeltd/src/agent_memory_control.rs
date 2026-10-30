//! `agent.memory.*` 控制方法：替 Pi 读写智能体记忆的元数据（使用统计、来源）。
//!
//! Pi 进程不碰数据库。它带着会话 id 和 daemon 发给该会话的 capability token 调这里，
//! 由主 daemon 经 `smelt-store` 读写主库。记忆层取自会话的
//! 启动规格，调用方只能操作自己这个会话的两层记忆。见 `docs/agent-memory.md`。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

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

/// 本会话读过某个记忆文件的时间。键是 `(session_id, scope, file)`，避免别的会话的 read 给这次覆盖放行。
const READ_WINDOW_MS: i64 = 5 * 60 * 1000;

fn recent_reads() -> &'static Mutex<HashMap<(String, String, String), i64>> {
    static READS: LazyLock<Mutex<HashMap<(String, String, String), i64>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    &READS
}

fn note_read(session_id: &str, scope: &str, file: &str, now: i64) {
    let mut reads = recent_reads()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    reads.insert(
        (session_id.to_string(), scope.to_string(), file.to_string()),
        now,
    );
}

fn has_recent_read(session_id: &str, scope: &str, file: &str, now: i64) -> bool {
    let reads = recent_reads()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    reads
        .get(&(session_id.to_string(), scope.to_string(), file.to_string()))
        .is_some_and(|read_at| now >= *read_at && now.saturating_sub(*read_at) <= READ_WINDOW_MS)
}

fn consume_read(session_id: &str, scope: &str, file: &str) {
    let mut reads = recent_reads()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    reads.remove(&(session_id.to_string(), scope.to_string(), file.to_string()));
}

/// 覆盖已有来源时，不能只信客户端的 `overwrote_existing`。
fn reject_record_write(
    client_overwrote: bool,
    existing_trusted: bool,
    has_existing: bool,
    trust: smelt_core::control_api::AgentMemoryTrust,
    recent_read: bool,
) -> Option<&'static str> {
    use smelt_core::control_api::AgentMemoryTrust;
    if existing_trusted && trust == AgentMemoryTrust::Pending {
        return Some("不能把已可信的记忆降级为待确认");
    }
    let overwrote = client_overwrote || has_existing;
    if trust == AgentMemoryTrust::Trusted && overwrote && !recent_read {
        return Some("覆盖已有记忆前必须先在本会话读取");
    }
    None
}

fn require_peer(
    acp_sessions: &AcpSessions,
    session_id: &str,
    peer_pid: Option<u32>,
) -> Result<(), ControlError> {
    // 单元测试里没有真实 Unix 对端时跳过；生产路径必须带上 peer，否则 bash 子孙可冒用 token。
    let Some(peer_pid) = peer_pid else {
        if cfg!(test) {
            return Ok(());
        }
        return Err(ControlError::new(
            ControlErrorCode::PermissionDenied,
            "cannot authenticate memory control peer",
        ));
    };
    crate::memory_peer_auth::require_session_process(acp_sessions, session_id, peer_pid)
}

pub(crate) fn meta(
    params: &AgentMemorySessionParams,
    acp_sessions: &AcpSessions,
    peer_pid: Option<u32>,
) -> Result<AgentMemoryMetaResult, ControlError> {
    let launch = authenticate(acp_sessions, &params.session_id, &params.token)?;
    require_peer(acp_sessions, &params.session_id, peer_pid)?;
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
    peer_pid: Option<u32>,
) -> Result<(), ControlError> {
    let launch = authenticate(acp_sessions, &params.session_id, &params.token)?;
    require_peer(acp_sessions, &params.session_id, peer_pid)?;
    let scope = scope(&launch, params.layer)?;
    let store = store()?;
    ensure_enabled(&store, &launch)?;
    let now = now_ms();
    agent_memory::record_launch_read(&store, &scope, &params.file, now).map_err(request_error)?;
    if let Some(file) = agent_memory::canonical_memory_file_name(&params.file) {
        note_read(&params.session_id, &scope.key(), &file, now);
    }
    Ok(())
}

pub(crate) fn record_write(
    params: &AgentMemoryRecordWriteParams,
    acp_sessions: &AcpSessions,
    peer_pid: Option<u32>,
) -> Result<(), ControlError> {
    let launch = authenticate(acp_sessions, &params.session_id, &params.token)?;
    require_peer(acp_sessions, &params.session_id, peer_pid)?;
    let scope = scope(&launch, params.layer)?;
    let Some(file) = agent_memory::canonical_memory_file_name(&params.file) else {
        return Err(request_error(MemoryRequestError::InvalidFile));
    };
    let store = store()?;
    ensure_enabled(&store, &launch)?;
    let existing = store
        .get_memory_provenance(&scope.key(), &file)
        .map_err(|error| ControlError::new(ControlErrorCode::OperationFailed, error.to_string()))?;
    let now = now_ms();
    if let Some(reason) = reject_record_write(
        params.overwrote_existing,
        existing
            .as_ref()
            .is_some_and(|row| row.trust == smelt_store::MemoryTrust::Trusted),
        existing.is_some(),
        params.trust,
        has_recent_read(&params.session_id, &scope.key(), &file, now),
    ) {
        return Err(ControlError::new(
            ControlErrorCode::FailedPrecondition,
            reason,
        ));
    }
    agent_memory::record_launch_write(
        &store,
        &scope,
        &file,
        params.trust,
        &params.reason,
        &params.session_id,
        now,
    )
    .map_err(request_error)?;
    if params.overwrote_existing || existing.is_some() {
        consume_read(&params.session_id, &scope.key(), &file);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use smelt_core::control_api::AgentMemoryTrust;

    use super::*;

    #[test]
    fn record_write_refuses_a_downgrade_and_scopes_the_recent_read() {
        assert_eq!(
            reject_record_write(false, true, true, AgentMemoryTrust::Pending, true),
            Some("不能把已可信的记忆降级为待确认")
        );
        assert_eq!(
            reject_record_write(true, true, true, AgentMemoryTrust::Trusted, false),
            Some("覆盖已有记忆前必须先在本会话读取")
        );
        assert!(
            reject_record_write(false, false, true, AgentMemoryTrust::Trusted, false).is_some(),
            "库里已有行也算覆盖，不能只看客户端的 overwrote_existing"
        );
        assert!(
            reject_record_write(false, false, false, AgentMemoryTrust::Trusted, false).is_none()
        );
        assert!(
            reject_record_write(true, false, false, AgentMemoryTrust::Pending, false).is_none()
        );
        assert!(reject_record_write(false, true, true, AgentMemoryTrust::Trusted, true).is_none());

        let now = 1_700_000_000_000;
        note_read("session-a", "agent:writer", "rule.md", now);
        assert!(has_recent_read(
            "session-a",
            "agent:writer",
            "rule.md",
            now + 1_000
        ));
        assert!(!has_recent_read(
            "session-b",
            "agent:writer",
            "rule.md",
            now + 1_000
        ));
        assert!(!has_recent_read(
            "session-a",
            "user",
            "rule.md",
            now + 1_000
        ));
        assert!(!has_recent_read(
            "session-a",
            "agent:writer",
            "rule.md",
            now + READ_WINDOW_MS + 1
        ));
        consume_read("session-a", "agent:writer", "rule.md");
        assert!(!has_recent_read(
            "session-a",
            "agent:writer",
            "rule.md",
            now + 1_000
        ));
    }

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
