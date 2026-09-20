use std::collections::HashMap;
use std::fmt;
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex, OnceLock};

use alacritty_terminal::term::TermMode;
use sha2::{Digest, Sha256};
use smelt_core::agent_bus::{
    AgentBackend, AgentEndpoint, cross_agent_enabled, render_delivery, validate_message,
};
use smelt_core::agent_kind::ConversationAgentKind;
use smelt_core::control_api::{AgentListParams, AgentListResult, AgentSendParams, AgentSendResult};
use smelt_plugin_api::CommandId;

use super::{
    AcpSession, AcpSessions, EventHubHandle, Phase, Session, Sessions, acp_runtime_alive,
    apply_acp_user_action,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PeerMessagingError {
    InvalidArgument(String),
    PermissionDenied(String),
    NotFound(String),
    FailedPrecondition(String),
    Conflict(String),
    Busy(String),
    TemporarilyUnavailable(String),
    ResultUnknown(String),
    OperationFailed(String),
}

impl PeerMessagingError {
    fn message(&self) -> &str {
        match self {
            Self::InvalidArgument(message)
            | Self::PermissionDenied(message)
            | Self::NotFound(message)
            | Self::FailedPrecondition(message)
            | Self::Conflict(message)
            | Self::Busy(message)
            | Self::TemporarilyUnavailable(message)
            | Self::ResultUnknown(message)
            | Self::OperationFailed(message) => message,
        }
    }
}

impl fmt::Display for PeerMessagingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for PeerMessagingError {}

#[derive(Default)]
struct MessagingControl {
    upgrading: bool,
    active_operations: usize,
}

fn messaging_control() -> &'static Mutex<MessagingControl> {
    static CONTROL: OnceLock<Mutex<MessagingControl>> = OnceLock::new();
    CONTROL.get_or_init(|| Mutex::new(MessagingControl::default()))
}

fn delivery_gate() -> &'static Mutex<()> {
    // Delivery nests the source and target lifecycle locks. Serializing deliveries prevents
    // concurrent A -> B and B -> A messages from acquiring those locks in opposite orders.
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(()))
}

const TERMINAL_DELIVERY_DEDUPE_CAPACITY: usize = 1024;

#[derive(Default)]
struct TerminalDeliveryDedupe {
    order: std::collections::VecDeque<String>,
    ids: std::collections::HashSet<String>,
}

fn terminal_delivery_dedupe() -> &'static Mutex<HashMap<String, TerminalDeliveryDedupe>> {
    static DEDUPE: OnceLock<Mutex<HashMap<String, TerminalDeliveryDedupe>>> = OnceLock::new();
    DEDUPE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn reserve_terminal_delivery(target: &str, delivery_id: &str) -> bool {
    let mut all = terminal_delivery_dedupe().lock().unwrap();
    let dedupe = all.entry(target.to_string()).or_default();
    if !dedupe.ids.insert(delivery_id.to_string()) {
        return false;
    }
    dedupe.order.push_back(delivery_id.to_string());
    while dedupe.order.len() > TERMINAL_DELIVERY_DEDUPE_CAPACITY {
        if let Some(expired) = dedupe.order.pop_front() {
            dedupe.ids.remove(&expired);
        }
    }
    true
}

fn rollback_terminal_delivery(target: &str, delivery_id: &str) {
    let mut all = terminal_delivery_dedupe().lock().unwrap();
    let Some(dedupe) = all.get_mut(target) else {
        return;
    };
    dedupe.ids.remove(delivery_id);
    dedupe.order.retain(|id| id != delivery_id);
}

struct ActivityGuard;

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        let mut control = messaging_control().lock().unwrap();
        control.active_operations = control.active_operations.saturating_sub(1);
    }
}

fn begin_activity() -> Result<ActivityGuard, PeerMessagingError> {
    let mut control = messaging_control().lock().unwrap();
    if control.upgrading {
        return Err(PeerMessagingError::TemporarilyUnavailable(
            "smeltd is upgrading; retry the cross-agent operation".to_string(),
        ));
    }
    control.active_operations += 1;
    Ok(ActivityGuard)
}

fn ensure_cross_agent_enabled() -> Result<(), PeerMessagingError> {
    if cross_agent_enabled() {
        Ok(())
    } else {
        Err(PeerMessagingError::FailedPrecondition(
            "cross-agent messaging is disabled in settings".to_string(),
        ))
    }
}

pub(crate) struct UpgradeGuard;

impl Drop for UpgradeGuard {
    fn drop(&mut self) {
        messaging_control().lock().unwrap().upgrading = false;
    }
}

pub(crate) fn begin_upgrade() -> Result<UpgradeGuard, usize> {
    let mut control = messaging_control().lock().unwrap();
    if control.upgrading || control.active_operations != 0 {
        return Err(control.active_operations);
    }
    control.upgrading = true;
    Ok(UpgradeGuard)
}

enum ResolvedTarget {
    Terminal(String, Arc<super::terminal_registry::TerminalSlot<Session>>),
    Acp(String, Arc<super::acp_registry::AcpSlot<AcpSession>>),
}

enum AuthenticatedSource {
    Terminal {
        id: String,
        token: String,
        slot: Arc<super::terminal_registry::TerminalSlot<Session>>,
    },
    Acp {
        id: String,
        token: String,
        slot: Arc<super::acp_registry::AcpSlot<AcpSession>>,
    },
}

impl AuthenticatedSource {
    fn id(&self) -> &str {
        match self {
            Self::Terminal { id, .. } | Self::Acp { id, .. } => id,
        }
    }

    fn authorize_delivery<R>(
        &self,
        sessions: &Sessions,
        acp_sessions: &AcpSessions,
        delivery: impl FnOnce() -> Result<R, PeerMessagingError>,
    ) -> Result<R, PeerMessagingError> {
        let mut delivery = Some(delivery);
        let result = match self {
            Self::Terminal { id, token, slot } => sessions.with_current(id, slot, |session| {
                let state = session.state.lock().unwrap().clone();
                let active = state.structured_events && terminal_agent_is_foreground(session);
                if !source_capability_valid(&state, active, token) {
                    return Err(PeerMessagingError::PermissionDenied(
                        "source session capability is invalid or expired".to_string(),
                    ));
                }
                delivery.take().unwrap()()
            }),
            Self::Acp { id, token, slot } => acp_sessions.with_current(id, slot, |session| {
                let state = session.state.lock().unwrap().clone();
                let active = acp_runtime_alive(session);
                if !source_capability_valid(&state, active, token) {
                    return Err(PeerMessagingError::PermissionDenied(
                        "source session capability is invalid or expired".to_string(),
                    ));
                }
                delivery.take().unwrap()()
            }),
        };
        result.unwrap_or_else(|| {
            Err(PeerMessagingError::PermissionDenied(
                "source session capability is invalid or expired".to_string(),
            ))
        })
    }
}

impl ResolvedTarget {
    fn id(&self) -> &str {
        match self {
            Self::Terminal(id, _) | Self::Acp(id, _) => id,
        }
    }
}

fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Connecting => "connecting",
        Phase::Thinking => "thinking",
        Phase::ExecutingTool => "executing_tool",
        Phase::AwaitingApproval => "awaiting_approval",
        Phase::WaitingForUser => "waiting_for_user",
        Phase::Succeeded => "succeeded",
        Phase::Failed => "failed",
        Phase::Idle => "idle",
        Phase::Dead => "dead",
    }
}

fn provider_for(launch: Option<&str>) -> Option<String> {
    launch
        .and_then(ConversationAgentKind::from_command_loose)
        .map(|kind| kind.id().to_string())
}

fn terminal_endpoint(id: &str, session: &Session) -> AgentEndpoint {
    let state = session.state.lock().unwrap().clone();
    AgentEndpoint {
        session_id: id.to_string(),
        backend: AgentBackend::Terminal,
        provider: provider_for(state.launch.as_deref()),
        title: state.title,
        cwd: state.cwd,
        phase: phase_name(state.phase).to_string(),
        available: state.agent_mcp
            && state.structured_events
            && terminal_agent_is_foreground(session)
            && matches!(state.phase, Phase::Idle | Phase::Succeeded | Phase::Failed),
    }
}

fn acp_accepts_peer_input(session: &AcpSession) -> bool {
    !matches!(
        &*session.conversation_binding.lock().unwrap(),
        Some(smelt_core::conversation::ConversationBinding::Automation { .. })
    )
}

fn acp_endpoint(id: &str, session: &AcpSession) -> AgentEndpoint {
    let state = session.state.lock().unwrap().clone();
    let running = acp_runtime_alive(session);
    let accepts_peer_input = acp_accepts_peer_input(session);
    AgentEndpoint {
        session_id: id.to_string(),
        backend: AgentBackend::Acp,
        provider: provider_for(state.launch.as_deref()),
        title: state.title,
        cwd: state.cwd,
        phase: phase_name(state.phase).to_string(),
        available: running && accepts_peer_input && state.agent_mcp && state.phase != Phase::Dead,
    }
}

fn list_endpoints(sessions: &Sessions, acp_sessions: &AcpSessions) -> Vec<AgentEndpoint> {
    let terminal_sessions = sessions.snapshot();
    let mut endpoints: Vec<_> = terminal_sessions
        .iter()
        .map(|(id, session)| terminal_endpoint(id, session))
        .collect();
    endpoints.extend(
        acp_sessions
            .snapshot()
            .into_iter()
            .map(|(id, slot)| acp_endpoint(&id, &slot.value)),
    );
    endpoints.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    endpoints
}

fn source_capability_valid(state: &super::SessionState, active: bool, token: &str) -> bool {
    active && state.agent_mcp && state.phase != Phase::Dead && state.agent_token == token
}

fn authenticate_source(
    source: &str,
    token: &str,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
) -> Result<AuthenticatedSource, PeerMessagingError> {
    if let Some(slot) = sessions.get(source) {
        let valid = sessions
            .with_current(source, &slot, |session| {
                let state = session.state.lock().unwrap().clone();
                let active = state.structured_events && terminal_agent_is_foreground(session);
                source_capability_valid(&state, active, token)
            })
            .unwrap_or(false);
        if !valid {
            return Err(PeerMessagingError::PermissionDenied(
                "source session capability is invalid or expired".to_string(),
            ));
        }
        return Ok(AuthenticatedSource::Terminal {
            id: source.to_string(),
            token: token.to_string(),
            slot,
        });
    }
    if let Some(slot) = acp_sessions.get(source) {
        let valid = acp_sessions
            .with_current(source, &slot, |session| {
                let state = session.state.lock().unwrap().clone();
                let active = acp_runtime_alive(session);
                source_capability_valid(&state, active, token)
            })
            .unwrap_or(false);
        if !valid {
            return Err(PeerMessagingError::PermissionDenied(
                "source session capability is invalid or expired".to_string(),
            ));
        }
        return Ok(AuthenticatedSource::Acp {
            id: source.to_string(),
            token: token.to_string(),
            slot,
        });
    }
    Err(PeerMessagingError::PermissionDenied(
        "source session does not exist".to_string(),
    ))
}

fn terminal_agent_is_foreground(session: &Session) -> bool {
    let control = session.ctl.lock().unwrap();
    let foreground_group = unsafe { libc::tcgetpgrp(control.master.as_raw_fd()) };
    // portable_pty 在 child 里先 setsid，因此初始 shell 的 PID 就是稳定的进程组 ID。
    // 唯一 reaper 现在会在 PTY EOF 前回收已退出 shell；此后再 getpgid(pid) 不但会
    // ESRCH，还可能撞上复用后的无关 PID。只比较保存下来的组号，不再查询进程表。
    let shell_group = session.child.pid();
    foreground_group > 0 && shell_group > 0 && foreground_group != shell_group
}

fn resolve_target(
    target: &str,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
) -> Result<ResolvedTarget, PeerMessagingError> {
    let target = target.trim();
    if target.is_empty() {
        return Err(PeerMessagingError::InvalidArgument(
            "target must not be empty".to_string(),
        ));
    }
    if let Some(slot) = sessions.get(target) {
        let available = sessions
            .with_current(target, &slot, |session| {
                let state = session.state.lock().unwrap().clone();
                state.agent_mcp && state.structured_events && terminal_agent_is_foreground(session)
            })
            .ok_or_else(|| {
                PeerMessagingError::NotFound("target disappeared while resolving it".to_string())
            })?;
        if !available {
            return Err(PeerMessagingError::FailedPrecondition(
                "terminal target does not have the Smelt MCP tools injected".to_string(),
            ));
        }
        return Ok(ResolvedTarget::Terminal(target.to_string(), slot));
    }
    if let Some(slot) = acp_sessions.get(target) {
        let available = acp_sessions
            .with_current(target, &slot, |session| {
                let state = session.state.lock().unwrap().clone();
                let accepts_peer_input = acp_accepts_peer_input(session);
                accepts_peer_input
                    && state.agent_mcp
                    && state.phase != Phase::Dead
                    && acp_runtime_alive(session)
            })
            .ok_or_else(|| {
                PeerMessagingError::NotFound("target disappeared while resolving it".to_string())
            })?;
        if !available {
            return Err(PeerMessagingError::FailedPrecondition(
                "ACP target does not have an active Smelt MCP connection".to_string(),
            ));
        }
        return Ok(ResolvedTarget::Acp(target.to_string(), slot));
    }

    let provider = target
        .strip_prefix("provider:")
        .unwrap_or(target)
        .to_ascii_lowercase();
    let matches: Vec<_> = list_endpoints(sessions, acp_sessions)
        .into_iter()
        .filter(|endpoint| endpoint.available && endpoint.provider.as_deref() == Some(&provider))
        .collect();
    match matches.as_slice() {
        [] => Err(PeerMessagingError::NotFound(format!(
            "no available agent matches target `{target}`"
        ))),
        [endpoint] => resolve_target(&endpoint.session_id, sessions, acp_sessions),
        _ => Err(PeerMessagingError::Conflict(format!(
            "target `{target}` is ambiguous; use a session ID: {}",
            matches
                .iter()
                .map(|endpoint| endpoint.session_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn terminal_input(text: &str, bracketed: bool) -> Vec<u8> {
    // 对目标 agent 来说这是普通用户提示，不是终端控制通道。保留换行和 tab，剔除
    // ESC/C0/C1 控制字符，避免 peer 文本意外触发 TUI 快捷键或注入新的转义序列。
    let cleaned: String = text
        .chars()
        .filter(|ch| matches!(*ch, '\n' | '\t') || (!ch.is_control() && *ch != '\u{7f}'))
        .collect();
    if bracketed {
        let mut bytes = Vec::with_capacity(cleaned.len() + 13);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(cleaned.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~\r");
        bytes
    } else {
        let single_line = cleaned.replace(['\r', '\n'], " ");
        let mut bytes = single_line.into_bytes();
        bytes.push(b'\r');
        bytes
    }
}

fn deliver(
    target: &ResolvedTarget,
    delivery_id: &str,
    text: String,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    event_hub: &EventHubHandle,
) -> Result<(), PeerMessagingError> {
    match target {
        ResolvedTarget::Acp(id, slot) => acp_sessions
            .with_current(id, slot, |session| {
                apply_acp_user_action(
                    session,
                    smelt_core::acp_session::AcpUserAction::Prompt {
                        text,
                        images: Vec::new(),
                        delivery_id: Some(delivery_id.to_string()),
                    },
                    event_hub,
                )
                .map_err(|error| PeerMessagingError::TemporarilyUnavailable(error.to_string()))
            })
            .unwrap_or_else(|| {
                Err(PeerMessagingError::NotFound(
                    "ACP target was replaced before delivery".to_string(),
                ))
            }),
        ResolvedTarget::Terminal(id, slot) => sessions
            .with_current(id, slot, |session| {
                let state = session.state.lock().unwrap().clone();
                if !state.agent_mcp
                    || !state.structured_events
                    || !terminal_agent_is_foreground(session)
                {
                    return Err(PeerMessagingError::FailedPrecondition(
                        "terminal target is no longer an active MCP-enabled agent".to_string(),
                    ));
                }
                let phase = state.phase;
                if !matches!(phase, Phase::Idle | Phase::Succeeded | Phase::Failed) {
                    return Err(PeerMessagingError::Busy(format!(
                        "terminal target is busy ({})",
                        phase_name(phase)
                    )));
                }
                let bracketed = session
                    .term
                    .lock()
                    .unwrap()
                    .mode()
                    .contains(TermMode::BRACKETED_PASTE);
                let payload = terminal_input(&text, bracketed);
                if !reserve_terminal_delivery(id, delivery_id) {
                    return Ok(());
                }
                if let Err(error) = super::write_session_input(session, &payload) {
                    rollback_terminal_delivery(id, delivery_id);
                    return Err(PeerMessagingError::OperationFailed(error.to_string()));
                }
                Ok(())
            })
            .unwrap_or_else(|| {
                Err(PeerMessagingError::NotFound(
                    "terminal target was replaced before delivery".to_string(),
                ))
            }),
    }
}

fn peer_message_request_fingerprint(source: &str, target: &str, message: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"smelt.peer-message.request.v1\0");
    for value in [source, target, message] {
        let bytes = value.as_bytes();
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    format!("sha256:{:x}", digest.finalize())
}

fn peer_message_delivery_id(source: &str, command_id: &CommandId) -> String {
    let mut digest = Sha256::new();
    digest.update(b"smelt.peer-message.delivery.v1\0");
    for value in [source, command_id.as_str()] {
        let bytes = value.as_bytes();
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    format!("message-{:x}", digest.finalize())
}

fn matching_peer_message_result(
    result: smelt_store::PeerMessageCommandResult,
    request_fingerprint: &str,
) -> Result<smelt_store::PeerMessageCommandResult, PeerMessagingError> {
    if result.request_fingerprint != request_fingerprint {
        return Err(PeerMessagingError::Conflict(
            "command_id is already bound to a different peer message request".to_string(),
        ));
    }
    Ok(result)
}

pub(crate) fn list_agents(
    params: &AgentListParams,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
) -> Result<AgentListResult, PeerMessagingError> {
    ensure_cross_agent_enabled()?;
    let source = required_non_empty(&params.source_session_id, "source_session_id")?;
    let token = required_non_empty(&params.source_token, "source_token")?;
    authenticate_source(source, token, sessions, acp_sessions)?;
    Ok(AgentListResult {
        agents: list_endpoints(sessions, acp_sessions),
    })
}

pub(crate) fn send_message(
    params: &AgentSendParams,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    event_hub: &EventHubHandle,
) -> Result<AgentSendResult, PeerMessagingError> {
    ensure_cross_agent_enabled()?;
    let _activity = begin_activity()?;
    let source_session_id = required_non_empty(&params.source_session_id, "source_session_id")?;
    let source_token = required_non_empty(&params.source_token, "source_token")?;
    let command_id = &params.command_id;
    let requested_target = required_non_empty(&params.target, "target")?;
    let message = required_non_empty(&params.message, "message")?;
    let source = authenticate_source(source_session_id, source_token, sessions, acp_sessions)?;
    let request_fingerprint =
        peer_message_request_fingerprint(source.id(), &params.target, &params.message);
    validate_message(message)
        .map_err(|error| PeerMessagingError::InvalidArgument(error.to_string()))?;
    if !event_hub.supports_peer_message_persistence() {
        return Err(PeerMessagingError::TemporarilyUnavailable(
            "durable peer message store is unavailable; message was not delivered".to_string(),
        ));
    }
    if let Some(result) = event_hub
        .peer_message_result(source.id(), command_id)
        .map_err(|error| PeerMessagingError::TemporarilyUnavailable(error.to_string()))?
    {
        let result = matching_peer_message_result(result, &request_fingerprint)?;
        return Ok(AgentSendResult {
            message_id: result.message_id,
            target_session_id: result.target_session_id,
        });
    }
    let target = resolve_target(requested_target, sessions, acp_sessions)?;
    if source.id() == target.id() {
        return Err(PeerMessagingError::InvalidArgument(
            "source and target sessions must be different".to_string(),
        ));
    }
    let message_id = peer_message_delivery_id(source.id(), command_id);
    let delivery = render_delivery(source.id(), &message_id, message);
    let _delivery_gate = delivery_gate().lock().unwrap();
    if let Some(result) = event_hub
        .peer_message_result(source.id(), command_id)
        .map_err(|error| {
            PeerMessagingError::ResultUnknown(format!(
                "a prior delivery may have succeeded but its durable result is not yet available: {error}"
            ))
        })?
    {
        let result = matching_peer_message_result(result, &request_fingerprint)?;
        return Ok(AgentSendResult {
            message_id: result.message_id,
            target_session_id: result.target_session_id,
        });
    }
    source.authorize_delivery(sessions, acp_sessions, || {
        deliver(
            &target,
            &message_id,
            delivery,
            sessions,
            acp_sessions,
            event_hub,
        )
    })?;
    // PTY/provider delivery is an external side effect and cannot be committed atomically with
    // SQLite. A process crash in the few instructions between that write and this ledger insert
    // remains an unavoidable exactly-once gap; normal retries are covered by the in-memory ledger,
    // target delivery_id dedupe, and the atomic command-result + outbox transaction below.
    let persisted = event_hub
        .record_peer_message_delivery(smelt_store::PeerMessageCommandResult {
            source_session_id: source.id().to_string(),
            command_id: command_id.clone(),
            request_fingerprint: request_fingerprint.clone(),
            completed_at_ms: now_ms(),
            message_id,
            target_session_id: target.id().to_string(),
        })
        .map_err(|error| {
            PeerMessagingError::ResultUnknown(format!(
                "message delivery succeeded but its command result and durable fact are pending persistence: {error}"
            ))
        })?;
    let persisted = matching_peer_message_result(persisted, &request_fingerprint)?;
    Ok(AgentSendResult {
        message_id: persisted.message_id,
        target_session_id: persisted.target_session_id,
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn required_non_empty<'a>(value: &'a str, key: &str) -> Result<&'a str, PeerMessagingError> {
    if value.trim().is_empty() {
        Err(PeerMessagingError::InvalidArgument(format!(
            "{key} must not be empty"
        )))
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_event_bus::Delivery;
    use smelt_plugin_api::{
        AgentMessageDelivered, CORE_CAPABILITY_AGENT_MESSAGE_READ,
        CORE_TOPIC_AGENT_MESSAGE_DELIVERED, Capability, DeliveryClass, PluginId, SubscriptionId,
        Topic,
    };
    use std::collections::BTreeSet;

    fn make_acp_endpoint(
        registry: &AcpSessions,
        id: &str,
        token: &str,
    ) -> smol::channel::Receiver<smelt_core::acp_conn::ConversationCommand> {
        let (slot, _) = registry.reserve_with(id, || {
            super::super::make_acp_session(
                id,
                None,
                false,
                Some(smelt_core::conversation::ConversationBinding::Direct),
                None,
                None,
            )
        });
        {
            let mut state = slot.value.state.lock().unwrap();
            state.launch = Some("codex".to_string());
            state.agent_mcp = true;
            state.agent_token = token.to_string();
            state.phase = Phase::Idle;
        }
        let (cmd_tx, cmd_rx) = smol::channel::unbounded();
        let (_event_tx, event_rx) = smol::channel::unbounded();
        *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
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
        cmd_rx
    }

    #[test]
    fn terminal_input_submits_one_bracketed_paste() {
        assert_eq!(
            terminal_input("one\ntwo", true),
            b"\x1b[200~one\ntwo\x1b[201~\r"
        );
    }

    #[test]
    fn terminal_input_without_bracketed_mode_stays_on_one_line() {
        assert_eq!(terminal_input("one\ntwo", false), b"one two\r");
    }

    #[test]
    fn terminal_input_removes_control_sequences() {
        assert_eq!(
            terminal_input("one\u{1b}[31m\u{7f}\u{009b}two", true),
            b"\x1b[200~one[31mtwo\x1b[201~\r"
        );
    }

    #[test]
    fn automation_acp_session_is_not_advertised_as_a_peer_target() {
        let sessions = super::super::new_sessions();
        let acp_sessions = super::super::new_test_acp_sessions();
        let _rx = make_acp_endpoint(&acp_sessions, "automation", "automation-token");
        let slot = acp_sessions.get("automation").unwrap();
        *slot.value.conversation_binding.lock().unwrap() =
            Some(smelt_core::conversation::ConversationBinding::Automation {
                run_id: "run-1".into(),
            });

        assert!(!acp_endpoint("automation", &slot.value).available);
        assert!(matches!(
            resolve_target("automation", &sessions, &acp_sessions),
            Err(PeerMessagingError::FailedPrecondition(_))
        ));
    }

    #[test]
    fn peer_can_reply_with_the_same_send_operation() {
        let sessions = super::super::new_sessions();
        let acp_sessions = super::super::new_test_acp_sessions();
        let source_rx = make_acp_endpoint(&acp_sessions, "source", "source-token");
        let target_rx = make_acp_endpoint(&acp_sessions, "target", "target-token");
        let event_hub = super::super::new_event_hub();

        send_message(
            &AgentSendParams {
                source_session_id: "source".to_string(),
                source_token: "source-token".to_string(),
                command_id: CommandId::new("agent-send-request").unwrap(),
                target: "target".to_string(),
                message: "please review".to_string(),
            },
            &sessions,
            &acp_sessions,
            &event_hub,
        )
        .unwrap();

        let command = target_rx.recv_blocking().unwrap();
        let smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } = command else {
            panic!("target did not receive a prompt");
        };
        assert!(text.contains("source_session: source"));
        assert!(text.contains("smelt_send_message"));

        send_message(
            &AgentSendParams {
                source_session_id: "target".to_string(),
                source_token: "target-token".to_string(),
                command_id: CommandId::new("agent-send-reply").unwrap(),
                target: "source".to_string(),
                message: "review complete".to_string(),
            },
            &sessions,
            &acp_sessions,
            &event_hub,
        )
        .unwrap();
        let command = source_rx.recv_blocking().unwrap();
        let smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } = command else {
            panic!("source did not receive the peer reply");
        };
        assert!(text.contains("review complete"));
        assert!(text.contains("source_session: target"));
    }

    #[test]
    fn successful_delivery_publishes_durable_fact() {
        let sessions = super::super::new_sessions();
        let acp_sessions = super::super::new_test_acp_sessions();
        let _source_rx = make_acp_endpoint(&acp_sessions, "source", "source-token");
        let target_rx = make_acp_endpoint(&acp_sessions, "target", "target-token");
        let event_hub = super::super::new_event_hub();
        let subscription = event_hub
            .subscribe(
                PluginId::new("test.peer-messaging").unwrap(),
                SubscriptionId::new("delivered").unwrap(),
                BTreeSet::from([Topic::new(CORE_TOPIC_AGENT_MESSAGE_DELIVERED).unwrap()]),
                DeliveryClass::Durable,
                &BTreeSet::from([Capability::new(CORE_CAPABILITY_AGENT_MESSAGE_READ).unwrap()]),
            )
            .unwrap();

        let params = AgentSendParams {
            source_session_id: "source".to_string(),
            source_token: "source-token".to_string(),
            command_id: CommandId::new("agent-send-durable").unwrap(),
            target: "target".to_string(),
            message: "please review".to_string(),
        };
        let result = send_message(&params, &sessions, &acp_sessions, &event_hub).unwrap();
        let _ = target_rx.recv_blocking().unwrap();
        let repeated = send_message(&params, &sessions, &acp_sessions, &event_hub).unwrap();
        assert_eq!(repeated, result);
        assert!(
            target_rx.try_recv().is_err(),
            "the same source + command_id must not reach the target twice"
        );
        let Delivery::Event(event) = subscription.recv().unwrap() else {
            panic!("expected delivered fact");
        };
        assert_eq!(
            event.envelope.topic.as_str(),
            CORE_TOPIC_AGENT_MESSAGE_DELIVERED
        );
        let delivered: AgentMessageDelivered =
            serde_json::from_value(event.envelope.payload.clone()).unwrap();
        assert_eq!(delivered.message_id, result.message_id);
        assert_eq!(delivered.source_session_id, "source");
        assert_eq!(delivered.target_session_id, "target");
    }

    #[test]
    fn target_delivery_ids_are_scoped_by_source_session() {
        let command_id = CommandId::new("shared-command-id").unwrap();
        let first = peer_message_delivery_id("source-one", &command_id);
        let repeated = peer_message_delivery_id("source-one", &command_id);
        let second = peer_message_delivery_id("source-two", &command_id);
        assert_eq!(first, repeated);
        assert_ne!(first, second);
    }

    #[test]
    fn command_id_is_bound_to_the_original_target_literal_and_message() {
        let sessions = super::super::new_sessions();
        let acp_sessions = super::super::new_test_acp_sessions();
        let _source_rx = make_acp_endpoint(&acp_sessions, "source", "source-token");
        let target_rx = make_acp_endpoint(&acp_sessions, "target", "target-token");
        let event_hub = super::super::new_event_hub();
        let original = AgentSendParams {
            source_session_id: "source".to_string(),
            source_token: "source-token".to_string(),
            command_id: CommandId::new("agent-send-bound-request").unwrap(),
            target: "target".to_string(),
            message: "original message".to_string(),
        };
        send_message(&original, &sessions, &acp_sessions, &event_hub).unwrap();
        let _ = target_rx.recv_blocking().unwrap();

        for changed in [
            AgentSendParams {
                target: "missing-target".to_string(),
                ..original.clone()
            },
            AgentSendParams {
                message: "changed message".to_string(),
                ..original
            },
        ] {
            assert!(matches!(
                send_message(&changed, &sessions, &acp_sessions, &event_hub),
                Err(PeerMessagingError::Conflict(_))
            ));
        }
        assert!(target_rx.try_recv().is_err());
    }

    #[test]
    fn wrong_source_token_is_rejected() {
        let sessions = super::super::new_sessions();
        let acp_sessions = super::super::new_test_acp_sessions();
        let _source_rx = make_acp_endpoint(&acp_sessions, "source", "real-token");
        let _target_rx = make_acp_endpoint(&acp_sessions, "target", "target-token");
        let error = send_message(
            &AgentSendParams {
                source_session_id: "source".to_string(),
                source_token: "wrong-token".to_string(),
                command_id: CommandId::new("agent-send-wrong-token").unwrap(),
                target: "target".to_string(),
                message: "must not be delivered".to_string(),
            },
            &sessions,
            &acp_sessions,
            &super::super::new_event_hub(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("capability"));
    }

    #[test]
    fn resolved_target_does_not_follow_same_id_replacement() {
        let sessions = super::super::new_sessions();
        let acp_sessions = super::super::new_test_acp_sessions();
        let old_rx = make_acp_endpoint(&acp_sessions, "target", "old-token");
        let target = resolve_target("target", &sessions, &acp_sessions).unwrap();
        let old_slot = acp_sessions.get("target").unwrap();
        {
            let _lifecycle = old_slot.lifecycle.lock().unwrap();
            assert!(acp_sessions.remove_if_same("target", &old_slot).is_some());
        }
        let replacement_rx = make_acp_endpoint(&acp_sessions, "target", "new-token");

        let error = deliver(
            &target,
            "agent-send-replaced",
            "must not cross generations".to_string(),
            &sessions,
            &acp_sessions,
            &super::super::new_event_hub(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("replaced"));
        assert!(old_rx.try_recv().is_err());
        assert!(replacement_rx.try_recv().is_err());
    }

    #[test]
    fn authenticated_source_is_rechecked_before_delivery() {
        let sessions = super::super::new_sessions();
        let acp_sessions = super::super::new_test_acp_sessions();
        let _source_rx = make_acp_endpoint(&acp_sessions, "source", "old-token");
        let target_rx = make_acp_endpoint(&acp_sessions, "target", "target-token");
        let source = authenticate_source("source", "old-token", &sessions, &acp_sessions).unwrap();
        let target = resolve_target("target", &sessions, &acp_sessions).unwrap();
        let old_slot = acp_sessions.get("source").unwrap();
        {
            let _lifecycle = old_slot.lifecycle.lock().unwrap();
            assert!(acp_sessions.remove_if_same("source", &old_slot).is_some());
        }
        let _replacement_rx = make_acp_endpoint(&acp_sessions, "source", "new-token");

        let error = source
            .authorize_delivery(&sessions, &acp_sessions, || {
                deliver(
                    &target,
                    "agent-send-recheck",
                    "must not be delivered".to_string(),
                    &sessions,
                    &acp_sessions,
                    &super::super::new_event_hub(),
                )
            })
            .unwrap_err();

        assert!(error.to_string().contains("expired"));
        assert!(target_rx.try_recv().is_err());
    }

    #[test]
    fn list_requires_a_valid_source_token() {
        let sessions = super::super::new_sessions();
        let acp_sessions = super::super::new_test_acp_sessions();
        let _source_rx = make_acp_endpoint(&acp_sessions, "source", "real-token");

        let error = list_agents(
            &AgentListParams {
                source_session_id: "source".to_string(),
                source_token: "wrong-token".to_string(),
            },
            &sessions,
            &acp_sessions,
        )
        .unwrap_err();
        assert!(error.to_string().contains("capability"));
    }
}
