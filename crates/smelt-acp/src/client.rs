//! 对话客户端 → `smeltd` 的 ACP 会话连接层：连 `acp_open`、收 `ConversationSnapshot` 流、
//! 还原 `FrameDelta` 镜像、发 `AcpUserAction`、分页拉历史、强制重启卡死会话。
//!
//! 纯非 UI、零文案依赖：所有错误通过 [`AcpClientError`] 返回稳定错误码与英文细节，
//! 由上层 JS 按当前语言翻译展示。

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use smelt_plugin_api::client::protocol::daemon::DaemonOperation;
use smelt_plugin_api::client::{RuntimeClient, RuntimeStream};
use smelt_plugin_api::model::acp_chat::ACP_ENTRY_VERSION;
use smelt_plugin_api::model::acp_conn::AcpForkCut;
use smelt_plugin_api::model::acp_session::{
    AcpEndKind, AcpInitialPrompt, AcpUserAction, ConversationSnapshot, SessionLive,
};
use smelt_plugin_api::model::acp_wire::{FrameDelta, FrameMirror};
use smelt_plugin_api::model::agent_kind::{ConversationAgentKind, ConversationLaunchSpec};
use smelt_plugin_api::model::conversation::ConversationBinding;
use smelt_plugin_api::model::daemon_state::DaemonPhase;

pub const ACP_INITIAL_TAIL_LIMIT: usize = 100;
pub const ACP_HISTORY_PAGE_LIMIT: usize = 100;
const ACP_ACTION_QUEUE_CAPACITY: usize = 256;

/// 短生命周期 ACP 控制请求的超时上限。
const ACP_CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AcpClientError {
    #[error("connect failed: {0}")]
    Connect(String),
    #[error("failed to clone duplex stream")]
    Duplex,
    #[error("failed to send acp_open handshake")]
    OpenFailed,
    #[error("session not found")]
    NoSession,
    #[error("failed to send history request: {0}")]
    HistorySendFailed(String),
    #[error("failed to read history response: {0}")]
    HistoryReadFailed(String),
    #[error("failed to parse history response")]
    HistoryParseFailed,
    #[error("invalid history snapshot payload")]
    HistoryInvalid,
    #[error("failed to write request: {0}")]
    WriteFailed(String),
    #[error("failed to read reply: {0}")]
    ReplyReadFailed(String),
    #[error("failed to parse reply")]
    ReplyParseFailed,
    #[error("failed to restart session")]
    RestartFailed,
    #[error("{0}")]
    Remote(String),
}

impl AcpClientError {
    /// 供 JS 层按稳定错误码映射本地化文案。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Connect(_) => "connect",
            Self::Duplex => "duplex",
            Self::OpenFailed => "open_failed",
            Self::NoSession => "no_session",
            Self::HistorySendFailed(_) => "history_send_failed",
            Self::HistoryReadFailed(_) => "history_read_failed",
            Self::HistoryParseFailed => "history_parse_failed",
            Self::HistoryInvalid => "history_invalid",
            Self::WriteFailed(_) => "write_failed",
            Self::ReplyReadFailed(_) => "reply_read_failed",
            Self::ReplyParseFailed => "reply_parse_failed",
            Self::RestartFailed => "restart_failed",
            Self::Remote(_) => "remote",
        }
    }
}

fn connect_acp_control(runtime: &RuntimeClient) -> Result<RuntimeStream, AcpClientError> {
    runtime
        .connect_with_timeout(ACP_CONTROL_TIMEOUT)
        .map_err(|e| AcpClientError::Connect(e.to_string()))
}

mod engine_kind_serde {
    use serde::{Deserialize, Deserializer, Serializer, de};
    use smelt_plugin_api::model::agent_kind::ConversationAgentKind;

    pub fn serialize<S: Serializer>(
        kind: &ConversationAgentKind,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(kind.id())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ConversationAgentKind, D::Error> {
        let id = String::deserialize(deserializer)?;
        ConversationAgentKind::from_id(&id)
            .ok_or_else(|| de::Error::custom(format!("unknown ConversationAgentKind: {id:?}")))
    }
}

/// 一次 `acp_open` 的启动参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationClientLaunch {
    pub id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    pub launch: ConversationLaunchSpec,
    #[serde(with = "engine_kind_serde")]
    pub engine_kind: ConversationAgentKind,
    #[serde(default)]
    pub ephemeral_env: BTreeMap<String, String>,
    #[serde(default)]
    pub resume_id: Option<String>,
    #[serde(default)]
    pub fork_id: Option<String>,
    #[serde(default)]
    pub fork_cut: Option<AcpForkCut>,
    #[serde(default)]
    pub retained_entries_end: usize,
    #[serde(default)]
    pub conversation_binding: ConversationBinding,
    #[serde(default)]
    pub pending_agent_preset: Option<String>,
    #[serde(default)]
    pub initial_config: Vec<(String, String)>,
    #[serde(default)]
    pub initial_prompt: Option<AcpInitialPrompt>,
}

fn acp_open_request(launch: &ConversationClientLaunch) -> serde_json::Value {
    let mut req = serde_json::json!({
        "op": DaemonOperation::AcpOpen,
        "id": &launch.id,
        "cwd": &launch.cwd,
        "launch": &launch.launch,
        "agent": launch.engine_kind.id(),
        "ephemeral_env": &launch.ephemeral_env,
        "resume_id": &launch.resume_id,
        "fork_id": &launch.fork_id,
        "fork_cut": &launch.fork_cut,
        "tail_limit": ACP_INITIAL_TAIL_LIMIT,
        "omit_history": true,
        "include_debug": false,
        "entry_version": ACP_ENTRY_VERSION,
        "conversation_binding": &launch.conversation_binding,
    });
    if let Some(prompt) = &launch.pending_agent_preset {
        req["pending_agent_preset"] = serde_json::Value::String(prompt.clone());
    }
    if !launch.initial_config.is_empty() {
        req["initial_config"] = serde_json::json!(&launch.initial_config);
    }
    if let Some(prompt) = &launch.initial_prompt {
        req["initial_prompt"] = serde_json::json!(prompt);
    }
    req
}

/// 连接线程交给上层的事件。
#[derive(Debug)]
pub enum ConversationClientEvent {
    Snapshot(Box<ConversationSnapshot>),
    /// 打开请求带了首条消息，守护回的第一帧却没确认收到：旧运行时未发出该消息。
    InitialPromptUnsupported,
}

pub struct ConversationClientHandle {
    pub action_tx: smol::channel::Sender<AcpUserAction>,
    pub event_rx: smol::channel::Receiver<ConversationClientEvent>,
    conn_cell: Arc<Mutex<Option<RuntimeStream>>>,
}

impl ConversationClientHandle {
    /// 供单测构造不连运行时的句柄：返回 `(handle, action_rx, event_tx)`。
    #[cfg(test)]
    pub(crate) fn detached() -> (
        Self,
        smol::channel::Receiver<AcpUserAction>,
        smol::channel::Sender<ConversationClientEvent>,
    ) {
        let (action_tx, action_rx) = smol::channel::unbounded();
        let (event_tx, event_rx) = smol::channel::unbounded();
        (
            Self {
                action_tx,
                event_rx,
                conn_cell: Arc::new(Mutex::new(None)),
            },
            action_rx,
            event_tx,
        )
    }
}

impl Drop for ConversationClientHandle {
    fn drop(&mut self) {
        let conn_cell = Arc::clone(&self.conn_cell);
        let close = std::thread::Builder::new()
            .name("smelt-acp-client-close".into())
            .spawn(move || {
                if let Some(c) = conn_cell.lock().ok().and_then(|mut g| g.take()) {
                    let _ = c.shutdown(std::net::Shutdown::Both);
                }
            });
        if close.is_err()
            && let Some(c) = self
                .conn_cell
                .lock()
                .ok()
                .and_then(|mut guard| guard.take())
        {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
    }
}

pub(crate) fn fallback_snapshot(reason: &str, entries_offset: usize) -> ConversationSnapshot {
    ConversationSnapshot {
        entries_offset,
        entries_total: entries_offset,
        snapshot_revision: 0,
        session_title: None,
        replaying_history: false,
        entries: Vec::new(),
        tool_debug: Default::default(),
        runtime_debug: Default::default(),
        phase: DaemonPhase::Dead,
        end_reason: reason.to_string(),
        end_kind: AcpEndKind::TransportDisconnected,
        end_code: None,
        accepted_delivery_ids: Default::default(),
        active_delivery_id: None,
        completed_delivery_id: None,
        pending_permissions: Vec::new(),
        pending_elicitation: None,
        status_line: None,
        acp_session_id: None,
        history_session_id: None,
        history_abandoned: false,
        supports_image: true,
        available_commands: Vec::new(),
        mcp_servers: Vec::new(),
        usage: None,
        usage_cached_read: None,
        usage_cost: None,
        usage_breakdown: None,
        plan: None,
        background_tasks: Vec::new(),
        subagents: Vec::new(),
        peer_inbox: Vec::new(),
        model: None,
        config_options: Vec::new(),
        conversation_state: None,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
        compacting: false,
        provider_retry: None,
        notice: None,
        memory_changes: None,
        pending_prompts: Vec::new(),
        queued_steering: Vec::new(),
        queued_follow_up: Vec::new(),
        composer_restore_revision: 0,
        composer_restore_texts: Vec::new(),
        composer_restore_images: Vec::new(),
        turn_started_at_ms: None,
        turn_timings: Vec::new(),
        completed_unread: false,
        turn_outcome: None,
        should_persist: true,
        history_error: None,
        history_omitted: false,
        envelope_omitted: false,
    }
}

fn snapshot_from_live(
    live: SessionLive,
    entries_offset: usize,
    history_error: Option<String>,
) -> ConversationSnapshot {
    let mut snapshot = fallback_snapshot(&live.end_reason, entries_offset);
    snapshot.phase = live.phase;
    snapshot.end_kind = live.end_kind;
    snapshot.end_code = live.end_code;
    snapshot.acp_session_id = live.acp_session_id;
    snapshot.history_session_id = live.history_session_id;
    snapshot.session_title = live.session_title;
    snapshot.status_line = live.status_line;
    snapshot.history_error = history_error;
    snapshot.should_persist = false;
    if let Some(history_len) = live.history_len {
        snapshot.entries_offset = history_len;
        snapshot.entries = live.open_turn;
        snapshot.entries_total = history_len.saturating_add(snapshot.entries.len());
    }
    if !matches!(snapshot.phase, DaemonPhase::Dead | DaemonPhase::Failed) {
        snapshot.end_kind = AcpEndKind::Unknown;
        snapshot.end_code = None;
    }
    snapshot
}

enum SnapshotFrame {
    Ignore,
    Snapshot(Box<ConversationSnapshot>),
    NeedsFullFrame,
    Unreadable(Box<ConversationSnapshot>),
}

#[derive(Deserialize)]
struct DecodedAcpSnapshotFrame {
    #[serde(default)]
    live: Option<SessionLive>,
    snapshot: Option<ConversationSnapshot>,
}

fn read_snapshot_frame(
    line: &str,
    entries_offset: usize,
    mirror: &mut FrameMirror,
) -> SnapshotFrame {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return SnapshotFrame::Ignore;
    };
    let restored = match value.get("delta") {
        Some(delta) => match FrameDelta::deserialize(delta)
            .ok()
            .and_then(|delta| mirror.apply(&delta).ok())
        {
            Some(text) => Some(text),
            None => return SnapshotFrame::NeedsFullFrame,
        },
        None => None,
    };
    let (text, value) = match &restored {
        Some(text) => (text.as_str(), None),
        None => {
            if value.get("snapshot").is_some() {
                mirror.full(&value);
            }
            (line.trim(), Some(value))
        }
    };
    if let Ok(frame) = serde_json::from_str::<DecodedAcpSnapshotFrame>(text) {
        if let Some(mut snapshot) = frame.snapshot {
            if snapshot.history_session_id.is_none()
                && let Some(history_session_id) = frame
                    .live
                    .and_then(|live| live.history_session_id)
                    .filter(|id| !id.trim().is_empty())
            {
                snapshot.history_session_id = Some(history_session_id);
            }
            return SnapshotFrame::Snapshot(Box::new(snapshot));
        }
        if let Some(live) = frame.live {
            return SnapshotFrame::Snapshot(Box::new(snapshot_from_live(
                live,
                entries_offset,
                None,
            )));
        }
        return SnapshotFrame::Ignore;
    }
    let value = match value {
        Some(value) => value,
        None => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(value) => value,
            Err(_) => {
                mirror.invalidate();
                return SnapshotFrame::NeedsFullFrame;
            }
        },
    };
    let live = value
        .get("live")
        .and_then(|live| SessionLive::from_json(live).ok())
        .or_else(|| {
            value
                .get("snapshot")
                .and_then(|snapshot| SessionLive::from_json(snapshot).ok())
        });
    if let Some(snapshot) = value.get("snapshot") {
        return match serde_json::from_value::<ConversationSnapshot>(snapshot.clone()) {
            Ok(snapshot) => SnapshotFrame::Snapshot(Box::new(snapshot)),
            Err(error) => {
                if let Some(live) = live {
                    return SnapshotFrame::Snapshot(Box::new(snapshot_from_live(
                        live,
                        entries_offset,
                        Some(format!("history unreadable: {error}")),
                    )));
                }
                let mut terminal =
                    fallback_snapshot(&format!("snapshot invalid: {error}"), entries_offset);
                terminal.end_kind = AcpEndKind::Unknown;
                SnapshotFrame::Unreadable(Box::new(terminal))
            }
        };
    }
    if let Some(live) = live {
        return SnapshotFrame::Snapshot(Box::new(snapshot_from_live(live, entries_offset, None)));
    }
    SnapshotFrame::Ignore
}

fn snapshot_after_stream_disconnect(
    last_phase: Option<DaemonPhase>,
    entries_offset: usize,
    session_title: Option<String>,
) -> Option<ConversationSnapshot> {
    if matches!(last_phase, Some(DaemonPhase::Dead | DaemonPhase::Failed)) {
        return None;
    }

    let mut disconnected = fallback_snapshot("disconnected", entries_offset);
    disconnected.session_title = session_title;
    Some(disconnected)
}

fn acp_snapshot_request(id: &str, before: usize, limit: usize) -> serde_json::Value {
    serde_json::json!({
        "op": DaemonOperation::AcpSnapshot,
        "id": id,
        "before": before,
        "limit": limit,
        "include_debug": false,
        "entry_version": ACP_ENTRY_VERSION,
    })
}

fn history_snapshot_from_response(response: &str) -> Result<ConversationSnapshot, AcpClientError> {
    let trimmed = response.trim();
    if trimmed.is_empty() {
        return Err(AcpClientError::NoSession);
    }
    let value: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|_| AcpClientError::HistoryParseFailed)?;
    if let Some(snapshot) = value.get("snapshot") {
        return serde_json::from_value(snapshot.clone())
            .map_err(|_| AcpClientError::HistoryInvalid);
    }
    if let Some(error) = value
        .get("error")
        .and_then(|error| error.as_str())
        .filter(|error| !error.is_empty())
    {
        return Err(AcpClientError::Remote(error.to_string()));
    }
    Err(AcpClientError::NoSession)
}

/// 分页拉取 `before` 之前的历史条目。阻塞，调用方放到后台线程执行。
pub fn load_acp_history(
    runtime: &RuntimeClient,
    id: &str,
    before: usize,
    limit: usize,
) -> Result<ConversationSnapshot, AcpClientError> {
    let mut stream = connect_acp_control(runtime)?;
    smelt_plugin_api::client::protocol::write_json_line(
        &mut stream,
        &acp_snapshot_request(id, before, limit),
    )
    .map_err(|e| AcpClientError::HistorySendFailed(e.to_string()))?;
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|e| AcpClientError::HistoryReadFailed(e.to_string()))?;
    history_snapshot_from_response(&response)
}

fn open_reply_acknowledges_initial(line: &str) -> bool {
    #[derive(Deserialize)]
    struct OpenReply {
        #[serde(default)]
        initial_ack: bool,
    }
    serde_json::from_str::<OpenReply>(line.trim()).is_ok_and(|reply| reply.initial_ack)
}

/// 连 `smeltd` 的 `acp_open`，起读写线程并立即返回句柄。
pub fn spawn_acp_client(
    runtime: RuntimeClient,
    launch: ConversationClientLaunch,
) -> ConversationClientHandle {
    let (action_tx, action_rx) = smol::channel::bounded::<AcpUserAction>(ACP_ACTION_QUEUE_CAPACITY);
    let (event_tx, event_rx) = smol::channel::unbounded::<ConversationClientEvent>();
    let conn_cell: Arc<Mutex<Option<RuntimeStream>>> = Arc::new(Mutex::new(None));
    let conn_cell_for_thread = Arc::clone(&conn_cell);
    let refresh_tx = action_tx.clone();

    let thread_name = format!("smelt-acp-cli-{}", &launch.id[..launch.id.len().min(12)]);
    std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let mut known_entries_end = launch.retained_entries_end;
            let mut known_session_title = None;
            let mut last_phase = None;
            let mut awaiting_initial_ack = launch.initial_prompt.is_some();
            let conn = match runtime.connect() {
                Ok(c) => c,
                Err(e) => {
                    let _ = event_tx.try_send(ConversationClientEvent::Snapshot(Box::new(
                        fallback_snapshot(&format!("connect failed: {e}"), known_entries_end),
                    )));
                    return;
                }
            };
            let Ok(mut writer) = conn.try_clone() else {
                let _ = event_tx.try_send(ConversationClientEvent::Snapshot(Box::new(
                    fallback_snapshot("failed to clone duplex stream", known_entries_end),
                )));
                return;
            };
            let _ = writer.set_write_timeout(Some(ACP_CONTROL_TIMEOUT));
            let req = acp_open_request(&launch);
            if smelt_plugin_api::client::protocol::write_json_line(&mut writer, &req).is_err() {
                let _ = event_tx.try_send(ConversationClientEvent::Snapshot(Box::new(
                    fallback_snapshot("failed to send acp_open handshake", known_entries_end),
                )));
                return;
            }
            if let Ok(mut cell) = conn_cell_for_thread.lock() {
                *cell = conn.try_clone().ok();
            }

            std::thread::spawn(move || {
                smol::block_on(async move {
                    while let Ok(action) = action_rx.recv().await {
                        let Ok(line) = serde_json::to_string(&action) else {
                            continue;
                        };
                        if writeln!(writer, "{line}").is_err() {
                            return;
                        }
                    }
                });
            });

            let mut reader = BufReader::new(conn);
            let mut line = String::new();
            let mut mirror = FrameMirror::default();
            let mut full_frame_requested = false;
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let frame = match read_snapshot_frame(&line, known_entries_end, &mut mirror) {
                    SnapshotFrame::NeedsFullFrame => {
                        if !full_frame_requested {
                            full_frame_requested =
                                refresh_tx.try_send(AcpUserAction::Refresh).is_ok();
                        }
                        continue;
                    }
                    SnapshotFrame::Snapshot(snapshot) => {
                        full_frame_requested = false;
                        SnapshotFrame::Snapshot(snapshot)
                    }
                    other => other,
                };
                match frame {
                    SnapshotFrame::NeedsFullFrame | SnapshotFrame::Ignore => continue,
                    SnapshotFrame::Unreadable(terminal) => {
                        let _ = event_tx.try_send(ConversationClientEvent::Snapshot(terminal));
                        return;
                    }
                    SnapshotFrame::Snapshot(snap) => {
                        if std::mem::take(&mut awaiting_initial_ack)
                            && !open_reply_acknowledges_initial(&line)
                            && event_tx
                                .try_send(ConversationClientEvent::InitialPromptUnsupported)
                                .is_err()
                        {
                            return;
                        }
                        known_entries_end = snap.entries_offset.saturating_add(snap.entries.len());
                        known_session_title = snap.session_title.clone();
                        last_phase = Some(snap.phase);
                        if event_tx
                            .try_send(ConversationClientEvent::Snapshot(snap))
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
            if let Some(disconnected) =
                snapshot_after_stream_disconnect(last_phase, known_entries_end, known_session_title)
            {
                let _ =
                    event_tx.try_send(ConversationClientEvent::Snapshot(Box::new(disconnected)));
            }
        })
        .expect("spawn acp client thread");

    ConversationClientHandle {
        action_tx,
        event_rx,
        conn_cell,
    }
}

/// 强制重启卡死的 ACP 会话（杀掉子进程组并换新进程继续）。阻塞，放到后台线程执行。
pub fn restart_acp_session(runtime: &RuntimeClient, id: &str) -> Result<(), AcpClientError> {
    let mut s = connect_acp_control(runtime)?;
    smelt_plugin_api::client::protocol::write_json_line(
        &mut s,
        &serde_json::json!({ "op": DaemonOperation::AcpRestart, "id": id }),
    )
    .map_err(|e| AcpClientError::WriteFailed(e.to_string()))?;
    let mut resp = String::new();
    BufReader::new(s)
        .read_line(&mut resp)
        .map_err(|e| AcpClientError::ReplyReadFailed(e.to_string()))?;
    let v: serde_json::Value =
        serde_json::from_str(resp.trim()).map_err(|_| AcpClientError::ReplyParseFailed)?;
    if v["ok"].as_bool() == Some(true) {
        Ok(())
    } else if let Some(err) = v["error"].as_str().filter(|s| !s.is_empty()) {
        Err(AcpClientError::Remote(err.to_string()))
    } else {
        Err(AcpClientError::RestartFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_plugin_api::model::acp_chat::{AcpEntry, AcpImage};
    use smelt_plugin_api::model::acp_session::AcpSnapshotFrame;

    fn wire_frame(sender: &mut FrameMirror, snapshot: &ConversationSnapshot, full: bool) -> String {
        let live = SessionLive::from_snapshot(snapshot);
        let mut header = snapshot.clone();
        header.entries.clear();
        let frame = serde_json::to_value(AcpSnapshotFrame {
            live: &live,
            snapshot: if full { snapshot } else { &header },
            provider_pid: Some(7),
            initial_ack: false,
        })
        .unwrap();
        if full {
            sender.full(&frame);
            return frame.to_string();
        }
        let delta = sender
            .encode(&frame, 0, &snapshot.entries, 0, snapshot.entries.len(), 0)
            .expect("delta encodes");
        serde_json::json!({ "delta": delta }).to_string()
    }

    #[test]
    fn acp_open_request_serializes_structured_launch() {
        let req = acp_open_request(&ConversationClientLaunch {
            id: "acp-1".into(),
            cwd: Some("/repo".into()),
            launch: ConversationLaunchSpec::from_command("claude --print")
                .with_env("CLAUDE_CONFIG_DIR", "~/Claude Workspaces/quant"),
            engine_kind: ConversationAgentKind::Claude,
            ephemeral_env: [("SESSION_TOKEN".into(), "transient".into())]
                .into_iter()
                .collect(),
            resume_id: Some("resume-1".into()),
            fork_id: None,
            fork_cut: None,
            retained_entries_end: 0,
            conversation_binding: ConversationBinding::Direct,
            pending_agent_preset: None,
            initial_config: Vec::new(),
            initial_prompt: None,
        });

        assert_eq!(req["op"], "acp_open");
        assert_eq!(req["resume_id"], "resume-1");
        assert!(req["fork_id"].is_null());
        assert_eq!(req["tail_limit"], ACP_INITIAL_TAIL_LIMIT);
        assert_eq!(req["include_debug"], false);
        assert_eq!(req["launch"]["command"], "claude --print");
    }

    #[test]
    fn acp_open_request_carries_initial_config_and_prompt() {
        let req = acp_open_request(&ConversationClientLaunch {
            id: "acp-1".into(),
            cwd: Some("/repo".into()),
            launch: ConversationLaunchSpec::from_command("pi"),
            engine_kind: ConversationAgentKind::Pi,
            ephemeral_env: BTreeMap::new(),
            resume_id: None,
            fork_id: None,
            fork_cut: None,
            retained_entries_end: 0,
            conversation_binding: ConversationBinding::Direct,
            pending_agent_preset: None,
            initial_config: vec![("model".into(), "sonnet".into())],
            initial_prompt: Some(AcpInitialPrompt {
                text: "hello".into(),
                images: vec![AcpImage::new("image/png", "AA==")],
            }),
        });

        assert_eq!(
            req["initial_config"],
            serde_json::json!([["model", "sonnet"]])
        );
        assert_eq!(req["initial_prompt"]["text"], "hello");
    }

    #[test]
    fn history_request_is_bounded_before_the_loaded_prefix() {
        let req = acp_snapshot_request("acp-1", 900, 100);
        assert_eq!(req["op"], "acp_snapshot");
        assert_eq!(req["before"], 900);
        assert_eq!(req["limit"], 100);
        assert_eq!(req["include_debug"], false);
    }

    #[test]
    fn blank_history_response_is_no_session() {
        assert_eq!(
            history_snapshot_from_response("").unwrap_err(),
            AcpClientError::NoSession
        );
    }

    #[test]
    fn history_error_string_is_remote_error() {
        let resp = serde_json::json!({ "ok": false, "error": "boom" }).to_string();
        assert_eq!(
            history_snapshot_from_response(&resp).unwrap_err(),
            AcpClientError::Remote("boom".into())
        );
    }

    #[test]
    fn open_reply_ack_is_read_from_first_frame() {
        assert!(open_reply_acknowledges_initial(r#"{"initial_ack":true}"#));
        assert!(!open_reply_acknowledges_initial(r#"{"snapshot":{}}"#));
    }

    #[test]
    fn stream_eof_does_not_overwrite_provider_failure() {
        assert!(snapshot_after_stream_disconnect(Some(DaemonPhase::Failed), 5, None).is_none());
        let snap =
            snapshot_after_stream_disconnect(Some(DaemonPhase::Thinking), 5, Some("t".into()))
                .expect("active disconnect emits fallback");
        assert_eq!(snap.phase, DaemonPhase::Dead);
        assert_eq!(snap.end_kind, AcpEndKind::TransportDisconnected);
        assert_eq!(snap.entries_offset, 5);
        assert_eq!(snap.session_title.as_deref(), Some("t"));
    }

    #[test]
    fn delta_frames_restore_the_same_snapshot() {
        let mut sender = FrameMirror::default();
        let mut receiver = FrameMirror::default();
        let mut snap = fallback_snapshot("", 0);
        snap.phase = DaemonPhase::Thinking;
        snap.entries = vec![AcpEntry::User("hi".into())];
        snap.entries_total = 1;

        let line1 = wire_frame(&mut sender, &snap, true);
        let SnapshotFrame::Snapshot(first) = read_snapshot_frame(&line1, 0, &mut receiver) else {
            panic!("expected full snapshot");
        };
        assert_eq!(first.entries.len(), 1);

        snap.entries.push(AcpEntry::Assistant {
            text: "hello world".into(),
            thought: false,
        });
        snap.entries_total = 2;
        snap.snapshot_revision = 2;

        let line2 = wire_frame(&mut sender, &snap, false);
        let SnapshotFrame::Snapshot(second) = read_snapshot_frame(&line2, 1, &mut receiver) else {
            panic!("expected delta snapshot");
        };
        assert_eq!(second.entries_offset, 1);
        assert_eq!(second.entries_total, 2);
        assert_eq!(second.entries.len(), 1);
        assert_eq!(second.snapshot_revision, 2);
    }
}
