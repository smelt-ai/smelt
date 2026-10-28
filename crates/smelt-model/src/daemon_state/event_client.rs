//! `event_subscribe` 订阅的协议层：请求行怎么写、回来的每一行怎么解成
//! [`DaemonStateEvent`]。快照凑批、revision 去重、会话水位与删除 tombstone、lag 后
//! 重收快照都在这里；连接、读行、重连归调用方（桌面走 unix socket / iroh，手机走
//! iroh），所以这里只吃字符串、不碰 IO。

use std::collections::HashMap;
use std::fmt;

use smelt_event_protocol::{
    CORE_PROJECTION_VERSION, CORE_SESSION_PROJECTION_VERSION, CORE_SESSION_STATE_SCHEMA_VERSION,
    CORE_SNAPSHOT_AUTOMATIONS, CORE_SNAPSHOT_REMOTE_SESSIONS, CORE_SNAPSHOT_SESSIONS,
    CORE_SNAPSHOT_WORKSPACE, CORE_SNAPSHOT_WORKSPACE_MENU, CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED,
    CORE_TOPIC_AUTOMATIONS_CHANGED, CORE_TOPIC_CONVERSATION_WAKE_REQUESTED,
    CORE_TOPIC_PROJECT_CHANGED, CORE_TOPIC_REMOTE_SESSIONS_CHANGED, CORE_TOPIC_SESSION_REMOVED,
    CORE_TOPIC_SESSION_STATE_CHANGED, CORE_TOPIC_TERMINAL_NOTIFICATION,
    CORE_TOPIC_TERMINAL_PROGRESS, CORE_TOPIC_WORKSPACE_CHANGED, CORE_TOPIC_WORKSPACE_MENU_CHANGED,
    ConversationWakeRequested, CoreProjectionEvent, CoreSessionPhase, EVENT_BUS_PROTOCOL_VERSION,
    EventClientAuth, EventSource, EventSubscribeRequest, FirstPartyClientKind, ProtocolRange,
    SessionStateChanged, SessionStateRemoved, SessionStatesSnapshot, SubscribeErrorCode,
    SubscribeMessage, SubscribeRequest, SubscriptionDeclaration, SubscriptionId, Topic,
};
use smelt_runtime_client::protocol::daemon::DaemonOperation;

use super::{DaemonPhase, DaemonSessionState, DaemonStateEvent};
use crate::automation::AutomationFile;
use crate::session_control::RemoteSessionSnapshot;
use crate::workspace_directory::WorkspaceDirectory;
use crate::workspace_menu::WorkspaceMenuSnapshot;

#[derive(Clone, Copy)]
struct SessionWatermark {
    generation: u64,
    revision: u64,
    removed: bool,
}

/// 单条 event subscription 的 session generation/revision 水位。删除会留下 tombstone，
/// 因而同一 runtime generation 的迟到更新不能复活已删除会话。
#[derive(Default)]
struct DaemonStateRevisions {
    revisions: HashMap<String, SessionWatermark>,
}

impl DaemonStateRevisions {
    fn reset_from_snapshot(&mut self, sessions: &[DaemonSessionState]) {
        self.revisions = sessions
            .iter()
            .map(|state| {
                (
                    state.id.clone(),
                    SessionWatermark {
                        generation: state.generation,
                        revision: state.revision,
                        removed: false,
                    },
                )
            })
            .collect();
    }

    fn accept_update(&mut self, state: &DaemonSessionState) -> bool {
        let Some(current) = self.revisions.get(&state.id).copied() else {
            self.revisions.insert(
                state.id.clone(),
                SessionWatermark {
                    generation: state.generation,
                    revision: state.revision,
                    removed: false,
                },
            );
            return true;
        };
        let accepted = if state.generation > current.generation {
            true
        } else if state.generation < current.generation || current.removed {
            false
        } else if state.revision == 0 {
            current.revision == 0
        } else {
            state.revision > current.revision
        };
        if accepted {
            self.revisions.insert(
                state.id.clone(),
                SessionWatermark {
                    generation: state.generation,
                    revision: state.revision,
                    removed: false,
                },
            );
        }
        accepted
    }

    fn accept_removal(&mut self, removed: &SessionStateRemoved) -> bool {
        let Some(current) = self.revisions.get(&removed.session_id).copied() else {
            self.revisions.insert(
                removed.session_id.clone(),
                SessionWatermark {
                    generation: removed.generation,
                    revision: removed.last_revision,
                    removed: true,
                },
            );
            return true;
        };
        let accepted = if removed.generation > current.generation {
            true
        } else if removed.generation < current.generation || current.removed {
            false
        } else {
            removed.last_revision >= current.revision
        };
        if accepted {
            self.revisions.insert(
                removed.session_id.clone(),
                SessionWatermark {
                    generation: removed.generation,
                    revision: removed.last_revision.max(current.revision),
                    removed: true,
                },
            );
        }
        accepted
    }
}

/// 远程目录、桌面菜单、工作区目录、任务和自动化都是整表替换，因此各自只需一个 revision 水位。
#[derive(Default)]
struct ReplacementRevisions {
    revision: Option<u64>,
}

type RemoteSessionRevisions = ReplacementRevisions;
type WorkspaceMenuRevisions = ReplacementRevisions;
type WorkspaceRevisions = ReplacementRevisions;
type AutomationFileRevisions = ReplacementRevisions;

impl ReplacementRevisions {
    fn reset_from_snapshot(&mut self, revision: Option<u64>) {
        self.revision = revision.filter(|revision| *revision != 0);
    }

    fn accept_update(&mut self, revision: u64) -> bool {
        if revision == 0 {
            return self.revision.is_none();
        }
        if self.revision.is_some_and(|last| revision <= last) {
            return false;
        }
        self.revision = Some(revision);
        true
    }
}

/// 一条订阅收哪些东西。会话状态与删除总是收；其余各项决定订哪些 topic、快照批要等
/// 齐哪几份投影，topic 列表由它推出来，两边不会对不上。
///
/// 订阅本身就是对运行时的表态：运行时看有没有人订了唤醒请求来决定冷对话能不能被
/// 拉起来，所以兑现不了的请求类 topic 不能订。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubscriptionShape {
    pub remote_sessions: bool,
    pub workspace_menu: bool,
    /// 运行时的工作区目录（开着的项目和对话）。
    pub workspace: bool,
    pub automations: bool,
    /// 冷恢复的对话启动规格只在桌面端手里，唤醒请求只能由它兑现。
    pub conversation_wake: bool,
    /// 外观归桌面端，CLI 发来的外观改动由它来改。
    pub appearance_requests: bool,
    /// 关注的项目里文件变了：重取 git 状态和文件树。
    pub project_changes: bool,
    /// 关注的智能体的记忆目录里文件变了：记忆页开着就重取。
    pub agent_memory_changes: bool,
    /// 终端程序发的 OSC 通知，运行时扫好再推过来，桌面不再自己扫字节流。
    pub terminal_notifications: bool,
    /// 终端程序用 OSC 9;4 报的进度，侧栏画进度条。
    pub terminal_progress: bool,
}

impl SubscriptionShape {
    /// 桌面端：全收，并承担唤醒对话、改外观这些只有它能兑现的请求。
    pub const DESKTOP: Self = Self {
        remote_sessions: true,
        workspace_menu: true,
        workspace: true,
        automations: true,
        conversation_wake: true,
        appearance_requests: true,
        project_changes: true,
        agent_memory_changes: true,
        terminal_notifications: true,
        terminal_progress: true,
    };

    /// 只看目录（手机）：会话状态、远程目录、菜单和自动化投影。不订唤醒请求和外观
    /// 请求——手机兑现不了，订了会让运行时以为有人能拉起冷对话；项目文件变化和终端
    /// OSC 也用不上。
    pub const CATALOG: Self = Self {
        remote_sessions: true,
        workspace_menu: true,
        // 手机暂时还读菜单，之后改读工作区目录。
        workspace: false,
        automations: true,
        conversation_wake: false,
        appearance_requests: false,
        project_changes: false,
        agent_memory_changes: false,
        terminal_notifications: false,
        terminal_progress: false,
    };

    /// 这种形状要订的 topic，顺序固定。
    pub fn topics(self) -> Vec<&'static str> {
        [
            (true, CORE_TOPIC_SESSION_STATE_CHANGED),
            (true, CORE_TOPIC_SESSION_REMOVED),
            (self.remote_sessions, CORE_TOPIC_REMOTE_SESSIONS_CHANGED),
            (self.workspace_menu, CORE_TOPIC_WORKSPACE_MENU_CHANGED),
            (self.workspace, CORE_TOPIC_WORKSPACE_CHANGED),
            (self.automations, CORE_TOPIC_AUTOMATIONS_CHANGED),
            (
                self.conversation_wake,
                CORE_TOPIC_CONVERSATION_WAKE_REQUESTED,
            ),
            (
                self.appearance_requests,
                CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED,
            ),
            (self.project_changes, CORE_TOPIC_PROJECT_CHANGED),
            (
                self.agent_memory_changes,
                smelt_event_protocol::CORE_TOPIC_AGENT_MEMORY_CHANGED,
            ),
            (
                self.terminal_notifications,
                CORE_TOPIC_TERMINAL_NOTIFICATION,
            ),
            (self.terminal_progress, CORE_TOPIC_TERMINAL_PROGRESS),
        ]
        .into_iter()
        .filter_map(|(wanted, topic)| wanted.then_some(topic))
        .collect()
    }
}

#[derive(Default)]
struct PendingSnapshots {
    active: bool,
    sessions: Option<Vec<DaemonSessionState>>,
    remote_sessions_seen: bool,
    remote_sessions: Option<RemoteSessionSnapshot>,
    workspace_menu_seen: bool,
    workspace_menu: Option<WorkspaceMenuSnapshot>,
    workspace_seen: bool,
    workspace: Option<WorkspaceDirectory>,
    automations_seen: bool,
    automations: Option<AutomationFile>,
}

impl PendingSnapshots {
    fn begin(&mut self) {
        *self = Self {
            active: true,
            ..Self::default()
        };
    }

    fn complete(&self, shape: SubscriptionShape) -> bool {
        self.sessions.is_some()
            && (!shape.remote_sessions || self.remote_sessions_seen)
            && (!shape.workspace_menu || self.workspace_menu_seen)
            && (!shape.workspace || self.workspace_seen)
            && (!shape.automations || self.automations_seen)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonEventClientError {
    Protocol(String),
    Server {
        code: SubscribeErrorCode,
        message: String,
    },
}

impl fmt::Display for DaemonEventClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(message) => write!(f, "event subscription protocol error: {message}"),
            Self::Server { code, message } => {
                write!(f, "event subscription rejected ({code:?}): {message}")
            }
        }
    }
}

impl std::error::Error for DaemonEventClientError {}

/// 一条 `event_subscribe` 订阅的解码状态机。调用方把 [`Self::request_line`] 写进
/// 连接，然后把读到的每一行交给 [`Self::decode_line`]；连接断了就丢掉它，重连时新建
/// 一个（新快照会重置所有水位）。
pub struct DaemonStateEventClient {
    handshake: EventSubscribeRequest,
    shape: SubscriptionShape,
    pending: PendingSnapshots,
    revisions: DaemonStateRevisions,
    remote_revisions: RemoteSessionRevisions,
    workspace_menu_revisions: WorkspaceMenuRevisions,
    workspace_revisions: WorkspaceRevisions,
    automation_revisions: AutomationFileRevisions,
}

impl DaemonStateEventClient {
    /// 身份总是桌面第一方：本机连接按进程身份核验；已配对的远程设备（含手机）运行时
    /// 不看这个字段，一律按桌面第一方处理。手机和桌面的差别只在订阅形状。
    pub fn new(shape: SubscriptionShape) -> Self {
        let subscription_id =
            SubscriptionId::new(format!("desktop-{}", uuid::Uuid::new_v4().simple()))
                .expect("generated subscription id is valid");
        let handshake = EventSubscribeRequest {
            auth: EventClientAuth::FirstParty {
                kind: FirstPartyClientKind::Desktop,
            },
            request: SubscribeRequest {
                protocol: ProtocolRange {
                    min: EVENT_BUS_PROTOCOL_VERSION,
                    max: EVENT_BUS_PROTOCOL_VERSION,
                },
                subscription: SubscriptionDeclaration {
                    id: subscription_id,
                    topics: shape
                        .topics()
                        .into_iter()
                        .map(|topic| Topic::new(topic).expect("core topic is valid"))
                        .collect(),
                },
            },
        };
        let mut pending = PendingSnapshots::default();
        pending.begin();
        Self {
            handshake,
            shape,
            pending,
            revisions: DaemonStateRevisions::default(),
            remote_revisions: RemoteSessionRevisions::default(),
            workspace_menu_revisions: WorkspaceMenuRevisions::default(),
            workspace_revisions: WorkspaceRevisions::default(),
            automation_revisions: AutomationFileRevisions::default(),
        }
    }

    pub fn handshake(&self) -> &EventSubscribeRequest {
        &self.handshake
    }

    pub fn request_line(&self) -> Result<String, DaemonEventClientError> {
        let value = serde_json::json!({
            "op": DaemonOperation::EventSubscribe,
            "auth": &self.handshake.auth,
            "request": &self.handshake.request,
        });
        serde_json::to_string(&value)
            .map(|mut line| {
                line.push('\n');
                line
            })
            .map_err(|error| DaemonEventClientError::Protocol(error.to_string()))
    }

    pub fn decode_line(
        &mut self,
        line: &str,
    ) -> Result<Option<DaemonStateEvent>, DaemonEventClientError> {
        let message = serde_json::from_str::<SubscribeMessage<serde_json::Value>>(line)
            .map_err(|error| DaemonEventClientError::Protocol(error.to_string()))?;
        self.decode_message(message)
    }

    pub fn decode_message(
        &mut self,
        message: SubscribeMessage<serde_json::Value>,
    ) -> Result<Option<DaemonStateEvent>, DaemonEventClientError> {
        match message {
            SubscribeMessage::Snapshot {
                kind,
                revision,
                payload,
                ..
            } => self.decode_snapshot(kind.0.as_str(), revision, payload),
            SubscribeMessage::Event { envelope, .. } => {
                if self.pending.active && !replacement_projection_topic(envelope.topic.as_str()) {
                    return Ok(None);
                }
                let schema_supported =
                    if envelope.topic.as_str() == CORE_TOPIC_SESSION_STATE_CHANGED {
                        matches!(
                            envelope.schema_version,
                            1..=CORE_SESSION_STATE_SCHEMA_VERSION
                        )
                    } else {
                        envelope.schema_version == 1
                    };
                if !schema_supported {
                    return Err(DaemonEventClientError::Protocol(format!(
                        "unsupported schema version {} for {}",
                        envelope.schema_version, envelope.topic
                    )));
                }
                if envelope.source != EventSource::Core {
                    return Err(DaemonEventClientError::Protocol(
                        "daemon projection event did not come from core".to_string(),
                    ));
                }
                self.decode_event(envelope.topic.as_str(), envelope.payload)
            }
            SubscribeMessage::Lag { .. } => {
                self.pending.begin();
                Ok(None)
            }
            SubscribeMessage::Error { code, message } => {
                Err(DaemonEventClientError::Server { code, message })
            }
        }
    }

    fn decode_snapshot(
        &mut self,
        kind: &str,
        revision: u64,
        payload: serde_json::Value,
    ) -> Result<Option<DaemonStateEvent>, DaemonEventClientError> {
        if !self.pending.active {
            if let Some(event) =
                self.decode_standalone_projection_snapshot(kind, revision, payload.clone())?
            {
                return Ok(Some(event));
            }
            self.pending.begin();
        }
        match kind {
            CORE_SNAPSHOT_SESSIONS => {
                let snapshot: SessionStatesSnapshot = decode_payload(payload, kind)?;
                if !matches!(
                    snapshot.projection_version,
                    1..=CORE_SESSION_PROJECTION_VERSION
                ) || snapshot.revision != revision
                {
                    return Err(DaemonEventClientError::Protocol(format!(
                        "invalid {kind} projection version or revision"
                    )));
                }
                self.pending.sessions = Some(
                    snapshot
                        .sessions
                        .into_iter()
                        .map(daemon_session_state)
                        .collect(),
                );
            }
            CORE_SNAPSHOT_REMOTE_SESSIONS if self.shape.remote_sessions => {
                self.pending.remote_sessions = decode_projection_snapshot(payload, kind, revision)?;
                self.pending.remote_sessions_seen = true;
            }
            CORE_SNAPSHOT_WORKSPACE_MENU if self.shape.workspace_menu => {
                self.pending.workspace_menu = decode_projection_snapshot(payload, kind, revision)?;
                self.pending.workspace_menu_seen = true;
            }
            CORE_SNAPSHOT_WORKSPACE if self.shape.workspace => {
                self.pending.workspace = decode_projection_snapshot(payload, kind, revision)?;
                self.pending.workspace_seen = true;
            }
            CORE_SNAPSHOT_AUTOMATIONS if self.shape.automations => {
                match decode_projection_snapshot(payload, kind, revision) {
                    Ok(file) => self.pending.automations = file,
                    Err(error) => {
                        eprintln!("[daemon-state] {error}");
                        self.pending.automations = None;
                    }
                }
                self.pending.automations_seen = true;
            }
            _ => {
                eprintln!("[daemon-state] ignoring unexpected snapshot kind {kind}");
            }
        }
        if !self.pending.complete(self.shape) {
            return Ok(None);
        }
        let pending = std::mem::take(&mut self.pending);
        let sessions = pending
            .sessions
            .expect("complete snapshot batch contains sessions");
        self.revisions.reset_from_snapshot(&sessions);
        self.remote_revisions
            .reset_from_snapshot(pending.remote_sessions.as_ref().map(|value| value.revision));
        self.workspace_menu_revisions
            .reset_from_snapshot(pending.workspace_menu.as_ref().map(|value| value.revision));
        self.workspace_revisions
            .reset_from_snapshot(pending.workspace.as_ref().map(|value| value.revision));
        self.automation_revisions
            .reset_from_snapshot(pending.automations.as_ref().map(|value| value.revision));
        Ok(Some(DaemonStateEvent::Snapshot {
            sessions,
            remote_sessions: pending.remote_sessions,
            workspace_menu: pending.workspace_menu,
            workspace: pending.workspace,
            automations: pending.automations,
        }))
    }

    fn decode_event(
        &mut self,
        topic: &str,
        payload: serde_json::Value,
    ) -> Result<Option<DaemonStateEvent>, DaemonEventClientError> {
        match topic {
            CORE_TOPIC_SESSION_STATE_CHANGED => {
                let state = daemon_session_state(decode_payload(payload, topic)?);
                Ok(self
                    .revisions
                    .accept_update(&state)
                    .then_some(DaemonStateEvent::Update(state)))
            }
            CORE_TOPIC_SESSION_REMOVED => {
                let removed: SessionStateRemoved = decode_payload(payload, topic)?;
                Ok(self
                    .revisions
                    .accept_removal(&removed)
                    .then_some(DaemonStateEvent::Removed {
                        id: removed.session_id,
                    }))
            }
            CORE_TOPIC_REMOTE_SESSIONS_CHANGED if self.shape.remote_sessions => {
                let snapshot: RemoteSessionSnapshot = decode_projection_event(payload, topic)?;
                Ok(self
                    .remote_revisions
                    .accept_update(snapshot.revision)
                    .then_some(DaemonStateEvent::RemoteSessions(snapshot)))
            }
            CORE_TOPIC_WORKSPACE_MENU_CHANGED if self.shape.workspace_menu => {
                let snapshot: WorkspaceMenuSnapshot = decode_projection_event(payload, topic)?;
                Ok(self
                    .workspace_menu_revisions
                    .accept_update(snapshot.revision)
                    .then_some(DaemonStateEvent::WorkspaceMenu(snapshot)))
            }
            CORE_TOPIC_WORKSPACE_CHANGED if self.shape.workspace => {
                let directory: WorkspaceDirectory = decode_projection_event(payload, topic)?;
                Ok(self
                    .workspace_revisions
                    .accept_update(directory.revision)
                    .then_some(DaemonStateEvent::Workspace(directory)))
            }
            CORE_TOPIC_AUTOMATIONS_CHANGED if self.shape.automations => {
                let snapshot: AutomationFile = decode_projection_event(payload, topic)?;
                Ok(self
                    .automation_revisions
                    .accept_update(snapshot.revision)
                    .then_some(DaemonStateEvent::Automations(snapshot)))
            }
            CORE_TOPIC_CONVERSATION_WAKE_REQUESTED if self.shape.conversation_wake => {
                let request: ConversationWakeRequested = decode_payload(payload, topic)?;
                Ok(Some(DaemonStateEvent::ConversationWakeRequested {
                    session_id: request.session_id,
                }))
            }
            CORE_TOPIC_PROJECT_CHANGED if self.shape.project_changes => {
                let changed: smelt_event_protocol::ProjectChanged = decode_payload(payload, topic)?;
                Ok(Some(DaemonStateEvent::ProjectChanged {
                    root: changed.root,
                }))
            }
            smelt_event_protocol::CORE_TOPIC_AGENT_MEMORY_CHANGED
                if self.shape.agent_memory_changes =>
            {
                let changed: smelt_event_protocol::AgentMemoryChanged =
                    decode_payload(payload, topic)?;
                Ok(Some(DaemonStateEvent::AgentMemoryChanged {
                    agent_id: changed.agent_id,
                }))
            }
            CORE_TOPIC_TERMINAL_NOTIFICATION if self.shape.terminal_notifications => {
                let notification: smelt_event_protocol::TerminalNotification =
                    decode_payload(payload, topic)?;
                Ok(Some(DaemonStateEvent::TerminalNotification {
                    session_id: notification.session_id,
                    message: notification.message,
                }))
            }
            CORE_TOPIC_TERMINAL_PROGRESS if self.shape.terminal_progress => {
                let changed: smelt_event_protocol::TerminalProgressChanged =
                    decode_payload(payload, topic)?;
                Ok(Some(DaemonStateEvent::TerminalProgress {
                    session_id: changed.session_id,
                    progress: changed.progress,
                }))
            }
            CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED if self.shape.appearance_requests => {
                let request: smelt_event_protocol::AppearanceChangeRequested =
                    decode_payload(payload, topic)?;
                Ok(Some(DaemonStateEvent::AppearanceChangeRequested {
                    settings: request.settings,
                }))
            }
            _ => Err(DaemonEventClientError::Protocol(format!(
                "unexpected event topic {topic}"
            ))),
        }
    }

    fn decode_standalone_projection_snapshot(
        &mut self,
        kind: &str,
        revision: u64,
        payload: serde_json::Value,
    ) -> Result<Option<DaemonStateEvent>, DaemonEventClientError> {
        match kind {
            CORE_SNAPSHOT_AUTOMATIONS if self.shape.automations => {
                let snapshot: Option<AutomationFile> =
                    match decode_projection_snapshot(payload, kind, revision) {
                        Ok(snapshot) => snapshot,
                        Err(error) => {
                            eprintln!("[daemon-state] {error}");
                            return Ok(None);
                        }
                    };
                let Some(snapshot) = snapshot else {
                    return Ok(None);
                };
                Ok(self
                    .automation_revisions
                    .accept_update(snapshot.revision)
                    .then_some(DaemonStateEvent::Automations(snapshot)))
            }
            CORE_SNAPSHOT_REMOTE_SESSIONS if self.shape.remote_sessions => {
                let Some(snapshot): Option<RemoteSessionSnapshot> =
                    decode_projection_snapshot(payload, kind, revision)?
                else {
                    return Ok(None);
                };
                Ok(self
                    .remote_revisions
                    .accept_update(snapshot.revision)
                    .then_some(DaemonStateEvent::RemoteSessions(snapshot)))
            }
            CORE_SNAPSHOT_WORKSPACE_MENU if self.shape.workspace_menu => {
                let Some(snapshot): Option<WorkspaceMenuSnapshot> =
                    decode_projection_snapshot(payload, kind, revision)?
                else {
                    return Ok(None);
                };
                Ok(self
                    .workspace_menu_revisions
                    .accept_update(snapshot.revision)
                    .then_some(DaemonStateEvent::WorkspaceMenu(snapshot)))
            }
            CORE_SNAPSHOT_WORKSPACE if self.shape.workspace => {
                let Some(directory): Option<WorkspaceDirectory> =
                    decode_projection_snapshot(payload, kind, revision)?
                else {
                    return Ok(None);
                };
                Ok(self
                    .workspace_revisions
                    .accept_update(directory.revision)
                    .then_some(DaemonStateEvent::Workspace(directory)))
            }
            _ => Ok(None),
        }
    }
}

fn replacement_projection_topic(topic: &str) -> bool {
    matches!(
        topic,
        CORE_TOPIC_AUTOMATIONS_CHANGED
            | CORE_TOPIC_REMOTE_SESSIONS_CHANGED
            | CORE_TOPIC_WORKSPACE_MENU_CHANGED
            | CORE_TOPIC_WORKSPACE_CHANGED
    )
}

fn decode_payload<T: serde::de::DeserializeOwned>(
    payload: serde_json::Value,
    label: &str,
) -> Result<T, DaemonEventClientError> {
    serde_json::from_value(payload).map_err(|error| {
        DaemonEventClientError::Protocol(format!("invalid {label} payload: {error}"))
    })
}

fn decode_projection(
    payload: serde_json::Value,
    label: &str,
) -> Result<CoreProjectionEvent, DaemonEventClientError> {
    let projection: CoreProjectionEvent = decode_payload(payload, label)?;
    if projection.projection_version != CORE_PROJECTION_VERSION {
        return Err(DaemonEventClientError::Protocol(format!(
            "unsupported {label} projection version {}",
            projection.projection_version
        )));
    }
    Ok(projection)
}

fn decode_projection_snapshot<T: serde::de::DeserializeOwned>(
    payload: serde_json::Value,
    label: &str,
    revision: u64,
) -> Result<Option<T>, DaemonEventClientError> {
    let projection = decode_projection(payload, label)?;
    if projection.revision != revision {
        return Err(DaemonEventClientError::Protocol(format!(
            "{label} snapshot revision mismatch"
        )));
    }
    if projection.data.is_null() {
        Ok(None)
    } else {
        decode_payload(projection.data, label).map(Some)
    }
}

fn decode_projection_event<T: serde::de::DeserializeOwned>(
    payload: serde_json::Value,
    label: &str,
) -> Result<T, DaemonEventClientError> {
    let projection = decode_projection(payload, label)?;
    if projection.data.is_null() {
        return Err(DaemonEventClientError::Protocol(format!(
            "{label} event contains a null projection"
        )));
    }
    decode_payload(projection.data, label)
}

fn daemon_session_state(state: SessionStateChanged) -> DaemonSessionState {
    DaemonSessionState {
        id: state.session_id,
        generation: state.generation,
        revision: state.revision,
        phase: match state.phase {
            CoreSessionPhase::Connecting => DaemonPhase::Connecting,
            CoreSessionPhase::Thinking => DaemonPhase::Thinking,
            CoreSessionPhase::ExecutingTool => DaemonPhase::ExecutingTool,
            CoreSessionPhase::AwaitingApproval => DaemonPhase::AwaitingApproval,
            CoreSessionPhase::WaitingForUser => DaemonPhase::WaitingForUser,
            CoreSessionPhase::Succeeded => DaemonPhase::Succeeded,
            CoreSessionPhase::Failed => DaemonPhase::Failed,
            CoreSessionPhase::Idle => DaemonPhase::Idle,
            CoreSessionPhase::Dead => DaemonPhase::Dead,
        },
        pending_question: state.pending_question,
        title: state.title,
        launch: state.launch,
        provider: state.provider,
        conversation_id: state.conversation_id,
        cwd: state.cwd,
        phase_since: state.phase_since_unix_seconds,
        updated_at: state.updated_at_unix_seconds,
        structured_events: state.structured_events,
        turn_events: state.turn_events,
        runtime: state.runtime,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desktop_client() -> DaemonStateEventClient {
        DaemonStateEventClient::new(SubscriptionShape::DESKTOP)
    }

    fn state(id: &str, revision: u64) -> DaemonSessionState {
        DaemonSessionState {
            id: id.into(),
            generation: 1,
            revision,
            ..Default::default()
        }
    }

    fn session_dto(id: &str, generation: u64, revision: u64) -> SessionStateChanged {
        SessionStateChanged {
            session_id: id.to_string(),
            generation,
            revision,
            cwd: Some("/workspace".to_string()),
            launch: Some("agent".to_string()),
            provider: Some("codex".to_string()),
            conversation_id: Some(format!("conv-{id}")),
            title: Some(id.to_string()),
            phase: CoreSessionPhase::Thinking,
            phase_since_unix_seconds: 10,
            pending_question: None,
            tokens_used: None,
            branch: None,
            dirty_files: Vec::new(),
            updated_at_unix_seconds: 11,
            structured_events: true,
            turn_events: true,
            agent_event_version: Some(1),
            runtime: true,
        }
    }

    fn sessions_snapshot(
        revision: u64,
        sessions: Vec<SessionStateChanged>,
    ) -> SubscribeMessage<serde_json::Value> {
        SubscribeMessage::Snapshot {
            kind: smelt_event_protocol::SnapshotKind(CORE_SNAPSHOT_SESSIONS.to_string()),
            watermark: 1,
            revision,
            payload: serde_json::to_value(SessionStatesSnapshot {
                projection_version: CORE_SESSION_PROJECTION_VERSION,
                revision,
                sessions,
            })
            .unwrap(),
        }
    }

    fn projection_snapshot(
        kind: &str,
        revision: u64,
        data: serde_json::Value,
    ) -> SubscribeMessage<serde_json::Value> {
        SubscribeMessage::Snapshot {
            kind: smelt_event_protocol::SnapshotKind(kind.to_string()),
            watermark: 1,
            revision,
            payload: serde_json::to_value(CoreProjectionEvent {
                projection_version: CORE_PROJECTION_VERSION,
                revision,
                data,
            })
            .unwrap(),
        }
    }

    fn event_message(
        topic: &str,
        payload: serde_json::Value,
    ) -> SubscribeMessage<serde_json::Value> {
        SubscribeMessage::Event {
            sequence: Some(1),
            envelope: smelt_event_protocol::EventEnvelope {
                event_id: smelt_event_protocol::EventId::new("event-1").unwrap(),
                topic: Topic::new(topic).unwrap(),
                schema_version: 1,
                occurred_at_ms: 1,
                aggregate: None,
                aggregate_revision: None,
                source: EventSource::Core,
                correlation_id: smelt_event_protocol::CorrelationId::new("correlation-1").unwrap(),
                causation_id: None,
                hop_count: 0,
                payload,
            },
        }
    }

    fn complete_snapshot(
        client: &mut DaemonStateEventClient,
        revision: u64,
        sessions: Vec<SessionStateChanged>,
    ) -> DaemonStateEvent {
        assert!(
            client
                .decode_message(sessions_snapshot(revision, sessions))
                .unwrap()
                .is_none()
        );
        for kind in [
            CORE_SNAPSHOT_REMOTE_SESSIONS,
            CORE_SNAPSHOT_WORKSPACE_MENU,
            CORE_SNAPSHOT_WORKSPACE,
            CORE_SNAPSHOT_AUTOMATIONS,
        ] {
            let event = client
                .decode_message(projection_snapshot(kind, 0, serde_json::Value::Null))
                .unwrap();
            if kind == CORE_SNAPSHOT_AUTOMATIONS {
                return event.expect("last expected snapshot completes the batch");
            }
            assert!(event.is_none());
        }
        unreachable!()
    }

    #[test]
    fn revision_tracker_rejects_duplicate_and_older_updates() {
        let mut tracker = DaemonStateRevisions::default();
        tracker.reset_from_snapshot(&[state("a", 10), state("b", 3)]);

        assert!(!tracker.accept_update(&state("a", 10)));
        assert!(!tracker.accept_update(&state("a", 9)));
        assert!(tracker.accept_update(&state("a", 11)));
        assert!(tracker.accept_update(&state("b", 4)));
        assert!(tracker.accept_update(&state("legacy", 0)));
    }

    #[test]
    fn a_new_subscription_snapshot_resets_the_daemon_epoch() {
        let mut tracker = DaemonStateRevisions::default();
        tracker.reset_from_snapshot(&[state("a", 100)]);
        tracker.reset_from_snapshot(&[state("a", 2)]);

        assert!(tracker.accept_update(&state("a", 3)));
    }

    #[test]
    fn removal_tombstone_rejects_late_generation_and_preserves_other_revisions() {
        let mut revisions = DaemonStateRevisions::default();
        revisions.reset_from_snapshot(&[state("keep", 10), state("gone", 4)]);

        assert!(revisions.accept_removal(&SessionStateRemoved {
            session_id: "gone".to_string(),
            generation: 1,
            last_revision: 4,
            removed_at_ms: 1,
        }));
        assert!(
            !revisions.accept_update(&state("keep", 10)),
            "删除别人不能把 keep 的水位清掉"
        );
        let late = state("gone", 5);
        assert!(
            !revisions.accept_update(&late),
            "同一 generation 的迟到更新不能复活删除会话"
        );
        let mut recreated = state("gone", 1);
        recreated.generation = 2;
        assert!(revisions.accept_update(&recreated));
    }

    #[test]
    fn desktop_handshake_uses_formal_protocol_and_unique_ephemeral_identity() {
        let first = desktop_client();
        let second = desktop_client();
        let line: serde_json::Value =
            serde_json::from_str(first.request_line().unwrap().trim()).unwrap();
        assert_eq!(line["op"], "event_subscribe");
        assert_eq!(
            line["auth"],
            serde_json::json!({
                "type": "first_party",
                "kind": "desktop"
            })
        );
        assert!(line["request"]["subscription"].get("delivery").is_none());
        assert!(line["request"].get("cursor").is_none());
        let topics = line["request"]["subscription"]["topics"]
            .as_array()
            .unwrap();
        assert_eq!(topics.len(), 12);
        assert!(
            topics
                .iter()
                .any(|topic| topic == CORE_TOPIC_AUTOMATIONS_CHANGED)
        );
        assert!(
            topics
                .iter()
                .any(|topic| topic == CORE_TOPIC_CONVERSATION_WAKE_REQUESTED),
            "桌面端是冷恢复对话的唯一启动方，必须接唤醒请求"
        );
        assert!(
            topics
                .iter()
                .any(|topic| topic == CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED),
            "外观归桌面端，CLI 发来的外观修改只能交给它"
        );
        assert!(
            topics
                .iter()
                .any(|topic| topic == CORE_TOPIC_TERMINAL_NOTIFICATION),
            "终端 OSC 通知由运行时扫好推来，桌面不再自己扫"
        );
        assert!(
            topics
                .iter()
                .any(|topic| topic == CORE_TOPIC_WORKSPACE_CHANGED),
            "桌面端读运行时的工作区目录"
        );
        assert_ne!(
            first.handshake().request.subscription.id,
            second.handshake().request.subscription.id
        );
        assert!(line["auth"].get("capabilities").is_none());
    }

    #[test]
    fn catalog_subscribes_only_to_projections_and_never_to_requests() {
        let client = DaemonStateEventClient::new(SubscriptionShape::CATALOG);
        let line: serde_json::Value =
            serde_json::from_str(client.request_line().unwrap().trim()).unwrap();
        assert_eq!(line["op"], "event_subscribe");
        // 远程设备的 auth 运行时不看；身份仍是桌面第一方，不另立网关身份。
        assert_eq!(
            line["auth"],
            serde_json::json!({ "type": "first_party", "kind": "desktop" })
        );
        let topics: Vec<&str> = line["request"]["subscription"]["topics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|topic| topic.as_str().unwrap())
            .collect();
        assert_eq!(
            topics,
            [
                CORE_TOPIC_SESSION_STATE_CHANGED,
                CORE_TOPIC_SESSION_REMOVED,
                CORE_TOPIC_REMOTE_SESSIONS_CHANGED,
                CORE_TOPIC_WORKSPACE_MENU_CHANGED,
                CORE_TOPIC_AUTOMATIONS_CHANGED,
            ]
        );
        for request in [
            CORE_TOPIC_CONVERSATION_WAKE_REQUESTED,
            CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED,
            CORE_TOPIC_PROJECT_CHANGED,
            // 手机还没改读工作区目录，先不订。
            CORE_TOPIC_WORKSPACE_CHANGED,
        ] {
            assert!(
                !topics.contains(&request),
                "只看目录的订阅不能订 {request}：订了运行时就以为有人能兑现"
            );
        }
    }

    #[test]
    fn catalog_completes_the_same_snapshot_batch_and_rejects_request_events() {
        let mut client = DaemonStateEventClient::new(SubscriptionShape::CATALOG);
        let event = complete_snapshot(&mut client, 1, vec![session_dto("s1", 1, 1)]);
        assert!(matches!(
            event,
            DaemonStateEvent::Snapshot { sessions, .. } if sessions.len() == 1
        ));
        assert!(matches!(
            client
                .decode_message(event_message(
                    CORE_TOPIC_SESSION_STATE_CHANGED,
                    serde_json::to_value(session_dto("s1", 1, 2)).unwrap(),
                ))
                .unwrap(),
            Some(DaemonStateEvent::Update(state)) if state.revision == 2
        ));
        // 没订的 topic 出现在流里说明运行时和客户端对订阅的理解不一致，按协议错误断开。
        assert!(
            client
                .decode_message(event_message(
                    CORE_TOPIC_CONVERSATION_WAKE_REQUESTED,
                    serde_json::json!({ "session_id": "acp-dormant" }),
                ))
                .is_err()
        );
    }

    #[test]
    fn remote_revision_tracker_rejects_duplicate_and_older_replacements() {
        let mut tracker = RemoteSessionRevisions::default();
        tracker.reset_from_snapshot(Some(10));

        assert!(!tracker.accept_update(10));
        assert!(!tracker.accept_update(9));
        assert!(tracker.accept_update(11));

        tracker.reset_from_snapshot(None);
        assert!(tracker.accept_update(1));
        assert!(!tracker.accept_update(0));

        let mut legacy = RemoteSessionRevisions::default();
        assert!(legacy.accept_update(0));
        assert!(legacy.accept_update(0));
    }

    #[test]
    fn session_projection_carries_the_conversation_id_across_versions() {
        let mut client = desktop_client();
        let event = complete_snapshot(&mut client, 7, vec![session_dto("session-1", 3, 4)]);
        assert!(
            matches!(
                event,
                DaemonStateEvent::Snapshot { sessions, .. }
                    if sessions[0].conversation_id.as_deref() == Some("conv-session-1")
            ),
            "快照必须把 provider 的对话 id 带到 UI"
        );

        // 旧 daemon（未带对话 id、schema 停在上一版）仍要能解码，否则升级期订阅会整条断掉。
        let mut older = serde_json::to_value(session_dto("session-1", 3, 5)).unwrap();
        older.as_object_mut().unwrap().remove("conversation_id");
        let mut message = event_message(CORE_TOPIC_SESSION_STATE_CHANGED, older);
        if let SubscribeMessage::Event { envelope, .. } = &mut message {
            envelope.schema_version = CORE_SESSION_STATE_SCHEMA_VERSION - 1;
        }
        assert!(matches!(
            client.decode_message(message).unwrap(),
            Some(DaemonStateEvent::Update(state))
                if state.revision == 5 && state.conversation_id.is_none()
        ));
    }

    #[test]
    fn formal_messages_map_snapshot_events_and_revisions() {
        let mut legacy_value = serde_json::to_value(session_dto("legacy", 1, 1)).unwrap();
        legacy_value.as_object_mut().unwrap().remove("provider");
        let legacy: SessionStateChanged = serde_json::from_value(legacy_value).unwrap();
        assert_eq!(
            legacy.provider, None,
            "v1 daemon 没有 provider 时必须可读取"
        );

        let mut client = desktop_client();
        let event = complete_snapshot(&mut client, 7, vec![session_dto("session-1", 3, 4)]);
        assert!(matches!(
            event,
            DaemonStateEvent::Snapshot { sessions, .. }
                if sessions.len() == 1
                    && sessions[0].generation == 3
                    && sessions[0].revision == 4
                    && sessions[0].provider.as_deref() == Some("codex")
        ));

        let update = event_message(
            CORE_TOPIC_SESSION_STATE_CHANGED,
            serde_json::to_value(session_dto("session-1", 3, 5)).unwrap(),
        );
        assert!(matches!(
            client.decode_message(update).unwrap(),
            Some(DaemonStateEvent::Update(state))
                if state.revision == 5 && state.provider.as_deref() == Some("codex")
        ));
        let duplicate = event_message(
            CORE_TOPIC_SESSION_STATE_CHANGED,
            serde_json::to_value(session_dto("session-1", 3, 5)).unwrap(),
        );
        assert!(client.decode_message(duplicate).unwrap().is_none());

        let automations = AutomationFile {
            revision: 10,
            ..Default::default()
        };
        let projection = CoreProjectionEvent {
            projection_version: CORE_PROJECTION_VERSION,
            revision: 10,
            data: serde_json::to_value(&automations).unwrap(),
        };
        assert!(matches!(
            client
                .decode_message(event_message(
                    CORE_TOPIC_AUTOMATIONS_CHANGED,
                    serde_json::to_value(projection).unwrap(),
                ))
                .unwrap(),
            Some(DaemonStateEvent::Automations(snapshot)) if snapshot.revision == 10
        ));
    }

    #[test]
    fn desktop_decodes_conversation_wake_requests() {
        let wake = || {
            event_message(
                CORE_TOPIC_CONVERSATION_WAKE_REQUESTED,
                serde_json::json!({ "session_id": "acp-dormant" }),
            )
        };
        let mut desktop = desktop_client();
        let _ = complete_snapshot(&mut desktop, 7, vec![]);
        assert!(matches!(
            desktop.decode_message(wake()).unwrap(),
            Some(DaemonStateEvent::ConversationWakeRequested { session_id })
                if session_id == "acp-dormant"
        ));
    }

    #[test]
    fn desktop_decodes_agent_memory_changes() {
        let mut desktop = desktop_client();
        let _ = complete_snapshot(&mut desktop, 7, vec![]);
        assert!(matches!(
            desktop
                .decode_message(event_message(
                    smelt_event_protocol::CORE_TOPIC_AGENT_MEMORY_CHANGED,
                    serde_json::json!({ "agent_id": "writer" }),
                ))
                .unwrap(),
            Some(DaemonStateEvent::AgentMemoryChanged { agent_id: Some(id) }) if id == "writer"
        ));
        assert!(matches!(
            desktop
                .decode_message(event_message(
                    smelt_event_protocol::CORE_TOPIC_AGENT_MEMORY_CHANGED,
                    serde_json::json!({ "agent_id": null }),
                ))
                .unwrap(),
            Some(DaemonStateEvent::AgentMemoryChanged { agent_id: None })
        ));
    }

    #[test]
    fn desktop_decodes_terminal_notifications() {
        let mut desktop = desktop_client();
        let _ = complete_snapshot(&mut desktop, 7, vec![]);
        assert!(matches!(
            desktop
                .decode_message(event_message(
                    CORE_TOPIC_TERMINAL_NOTIFICATION,
                    serde_json::json!({ "session_id": "s1", "message": "构建完成" }),
                ))
                .unwrap(),
            Some(DaemonStateEvent::TerminalNotification { session_id, message })
                if session_id == "s1" && message == "构建完成"
        ));
    }

    #[test]
    fn desktop_decodes_terminal_progress() {
        let mut desktop = desktop_client();
        let _ = complete_snapshot(&mut desktop, 7, vec![]);
        assert!(matches!(
            desktop
                .decode_message(event_message(
                    CORE_TOPIC_TERMINAL_PROGRESS,
                    serde_json::json!({
                        "session_id": "s1",
                        "progress": { "state": "normal", "percent": 42 },
                    }),
                ))
                .unwrap(),
            Some(DaemonStateEvent::TerminalProgress { session_id, progress: Some(progress) })
                if session_id == "s1" && progress.percent == Some(42)
        ));
        assert!(matches!(
            desktop
                .decode_message(event_message(
                    CORE_TOPIC_TERMINAL_PROGRESS,
                    serde_json::json!({ "session_id": "s1", "progress": null }),
                ))
                .unwrap(),
            Some(DaemonStateEvent::TerminalProgress { progress: None, .. })
        ));
    }

    #[test]
    fn lag_waits_for_all_recovery_snapshots_before_resuming_events() {
        let mut client = desktop_client();
        let _ = complete_snapshot(&mut client, 7, vec![session_dto("session-1", 1, 10)]);
        assert!(
            client
                .decode_message(SubscribeMessage::Lag {
                    dropped: 2,
                    resume_after: Some(10),
                })
                .unwrap()
                .is_none()
        );
        let ignored = event_message(
            CORE_TOPIC_SESSION_STATE_CHANGED,
            serde_json::to_value(session_dto("session-1", 1, 11)).unwrap(),
        );
        assert!(client.decode_message(ignored).unwrap().is_none());

        let recovered = complete_snapshot(&mut client, 2, vec![session_dto("session-1", 1, 2)]);
        assert!(matches!(recovered, DaemonStateEvent::Snapshot { .. }));
        let next = event_message(
            CORE_TOPIC_SESSION_STATE_CHANGED,
            serde_json::to_value(session_dto("session-1", 1, 3)).unwrap(),
        );
        assert!(matches!(
            client.decode_message(next).unwrap(),
            Some(DaemonStateEvent::Update(state)) if state.revision == 3
        ));
    }

    #[test]
    fn error_and_legacy_frames_are_not_treated_as_state_events() {
        let mut client = desktop_client();
        let Err(error) = client.decode_message(SubscribeMessage::Error {
            code: SubscribeErrorCode::Rejected,
            message: "denied".to_string(),
        }) else {
            panic!("server error must terminate the subscription");
        };
        assert!(matches!(
            error,
            DaemonEventClientError::Server {
                code: SubscribeErrorCode::Rejected,
                ..
            }
        ));
        let Err(error) = client.decode_line(r#"{"sessions":[]}"#) else {
            panic!("legacy frame must not decode");
        };
        assert!(error.to_string().contains("protocol error"));
    }

    fn automation_file(revision: u64, store_id: &str) -> AutomationFile {
        AutomationFile {
            schema_version: 1,
            store_id: store_id.into(),
            revision,
            ..Default::default()
        }
    }

    fn automations_changed(file: &AutomationFile) -> SubscribeMessage<serde_json::Value> {
        event_message(
            CORE_TOPIC_AUTOMATIONS_CHANGED,
            serde_json::to_value(CoreProjectionEvent {
                projection_version: CORE_PROJECTION_VERSION,
                revision: file.revision,
                data: serde_json::to_value(file).unwrap(),
            })
            .unwrap(),
        )
    }

    #[test]
    fn desktop_snapshot_batch_delivers_automation_file() {
        let file = automation_file(1440, "store-a");
        let mut client = desktop_client();
        assert!(
            client
                .decode_message(sessions_snapshot(1, vec![session_dto("s1", 1, 1)]))
                .unwrap()
                .is_none()
        );
        for kind in [
            CORE_SNAPSHOT_REMOTE_SESSIONS,
            CORE_SNAPSHOT_WORKSPACE_MENU,
            CORE_SNAPSHOT_WORKSPACE,
        ] {
            assert!(
                client
                    .decode_message(projection_snapshot(kind, 0, serde_json::Value::Null))
                    .unwrap()
                    .is_none()
            );
        }
        let event = client
            .decode_message(projection_snapshot(
                CORE_SNAPSHOT_AUTOMATIONS,
                file.revision,
                serde_json::to_value(&file).unwrap(),
            ))
            .unwrap();
        match event {
            Some(DaemonStateEvent::Snapshot {
                automations: Some(got),
                ..
            }) => {
                assert_eq!(got.store_id, file.store_id);
                assert_eq!(got.revision, 1440);
            }
            _ => panic!("expected Snapshot with automations"),
        }
    }

    #[test]
    fn automations_events_are_not_dropped_during_incomplete_snapshot_batch() {
        let mut client = desktop_client();
        assert!(
            client
                .decode_message(sessions_snapshot(1, vec![session_dto("s1", 1, 1)]))
                .unwrap()
                .is_none()
        );
        let file = automation_file(12, "store-b");
        match client.decode_message(automations_changed(&file)).unwrap() {
            Some(DaemonStateEvent::Automations(got)) => {
                assert_eq!(got.store_id, "store-b");
                assert_eq!(got.revision, 12);
            }
            _ => panic!("automations.changed must not wait for the rest of the snapshot batch"),
        }
    }

    #[test]
    fn standalone_automations_snapshot_after_batch_is_an_incremental_event() {
        let mut client = desktop_client();
        let _ = complete_snapshot(&mut client, 1, vec![session_dto("s1", 1, 1)]);
        let file = automation_file(20, "store-c");
        match client
            .decode_message(projection_snapshot(
                CORE_SNAPSHOT_AUTOMATIONS,
                file.revision,
                serde_json::to_value(&file).unwrap(),
            ))
            .unwrap()
        {
            Some(DaemonStateEvent::Automations(got)) => assert_eq!(got.revision, 20),
            _ => panic!("late automations snapshot must not start a new batch"),
        }
    }

    fn workspace_directory(revision: u64) -> WorkspaceDirectory {
        WorkspaceDirectory {
            revision,
            projects: vec!["/workspace".to_string()],
            conversations: Vec::new(),
        }
    }

    fn workspace_changed(directory: &WorkspaceDirectory) -> SubscribeMessage<serde_json::Value> {
        event_message(
            CORE_TOPIC_WORKSPACE_CHANGED,
            serde_json::to_value(CoreProjectionEvent {
                projection_version: CORE_PROJECTION_VERSION,
                revision: directory.revision,
                data: serde_json::to_value(directory).unwrap(),
            })
            .unwrap(),
        )
    }

    #[test]
    fn desktop_snapshot_batch_waits_for_and_delivers_workspace_directory() {
        let directory = workspace_directory(5);
        let mut client = desktop_client();
        assert!(
            client
                .decode_message(sessions_snapshot(1, vec![session_dto("s1", 1, 1)]))
                .unwrap()
                .is_none()
        );
        for kind in [
            CORE_SNAPSHOT_REMOTE_SESSIONS,
            CORE_SNAPSHOT_WORKSPACE_MENU,
            CORE_SNAPSHOT_AUTOMATIONS,
        ] {
            assert!(
                client
                    .decode_message(projection_snapshot(kind, 0, serde_json::Value::Null))
                    .unwrap()
                    .is_none(),
                "桌面端的快照批要等到工作区目录才算齐"
            );
        }
        match client
            .decode_message(projection_snapshot(
                CORE_SNAPSHOT_WORKSPACE,
                directory.revision,
                serde_json::to_value(&directory).unwrap(),
            ))
            .unwrap()
        {
            Some(DaemonStateEvent::Snapshot {
                workspace: Some(got),
                ..
            }) => assert_eq!(got, directory),
            _ => panic!("expected Snapshot with workspace directory"),
        }
    }

    #[test]
    fn null_workspace_snapshot_decodes_to_none() {
        let mut client = desktop_client();
        match complete_snapshot(&mut client, 1, vec![]) {
            DaemonStateEvent::Snapshot { workspace, .. } => assert!(workspace.is_none()),
            _ => panic!("expected Snapshot"),
        }
    }

    #[test]
    fn workspace_changed_replaces_and_drops_stale_revisions() {
        let mut client = desktop_client();
        let _ = complete_snapshot(&mut client, 1, vec![]);
        assert!(matches!(
            client.decode_message(workspace_changed(&workspace_directory(3))).unwrap(),
            Some(DaemonStateEvent::Workspace(got)) if got.revision == 3
        ));
        assert!(
            client
                .decode_message(workspace_changed(&workspace_directory(3)))
                .unwrap()
                .is_none(),
            "重复的 revision 要丢掉"
        );
        assert!(
            client
                .decode_message(workspace_changed(&workspace_directory(2)))
                .unwrap()
                .is_none(),
            "更旧的 revision 要丢掉"
        );
        // 批后再来的单独快照按增量处理，同样受水位约束。
        assert!(matches!(
            client
                .decode_message(projection_snapshot(
                    CORE_SNAPSHOT_WORKSPACE,
                    4,
                    serde_json::to_value(workspace_directory(4)).unwrap(),
                ))
                .unwrap(),
            Some(DaemonStateEvent::Workspace(got)) if got.revision == 4
        ));
        assert!(
            client
                .decode_message(projection_snapshot(
                    CORE_SNAPSHOT_WORKSPACE,
                    4,
                    serde_json::to_value(workspace_directory(4)).unwrap(),
                ))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn workspace_snapshot_resets_the_watermark() {
        let mut client = desktop_client();
        let _ = complete_snapshot(&mut client, 1, vec![]);
        assert!(
            client
                .decode_message(workspace_changed(&workspace_directory(9)))
                .unwrap()
                .is_some()
        );
        // lag 后重收快照，新快照里的 revision 成为新水位。
        assert!(
            client
                .decode_message(SubscribeMessage::Lag {
                    dropped: 1,
                    resume_after: None,
                })
                .unwrap()
                .is_none()
        );
        let _ = complete_snapshot(&mut client, 2, vec![]);
        assert!(matches!(
            client.decode_message(workspace_changed(&workspace_directory(1))).unwrap(),
            Some(DaemonStateEvent::Workspace(got)) if got.revision == 1
        ));
    }
}
