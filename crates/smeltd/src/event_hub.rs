//! smeltd domain projections backed by the single process-wide `smelt_event_bus::EventHub`.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use smelt_event_bus::{
    Delivery, EventBusError, EventHub, Snapshot, SnapshotProvider, StoredEvent, TopicRegistry,
};
use smelt_event_protocol::{
    AggregateRef, CORE_PROJECTION_VERSION, CORE_SESSION_PROJECTION_VERSION,
    CORE_SNAPSHOT_AUTOMATIONS, CORE_SNAPSHOT_REMOTE_SESSIONS, CORE_SNAPSHOT_SESSIONS,
    CORE_SNAPSHOT_WORKSPACE, CORE_SNAPSHOT_WORKSPACE_MENU, CORE_TOPIC_AUTOMATIONS_CHANGED,
    CORE_TOPIC_CONVERSATION_WAKE_REQUESTED, CORE_TOPIC_WORKSPACE_CHANGED,
    CORE_TOPIC_REMOTE_SESSIONS_CHANGED, CORE_TOPIC_SESSION_REMOVED,
    CORE_TOPIC_SESSION_STATE_CHANGED, CORE_TOPIC_WORKSPACE_MENU_CHANGED, Capability,
    ConversationWakeRequested, CoreProjectionEvent, CoreSessionPhase, CorrelationId,
    EventEnvelope, EventId, EventSource, PluginId, PublisherIdentity, SessionStateChanged,
    SessionStateRemoved, SessionStatesSnapshot, SnapshotKind, SubscriptionId, Topic,
};

use super::{
    AutomationFile, DaemonProjectionSeed, Phase, RemoteSessionSnapshot, SessionState,
    WorkspaceMenuSnapshot,
};

const MAILBOX_CAPACITY: usize = 256;

pub(crate) type EventHubHandle = Arc<DaemonEventHub>;

#[derive(Default)]
struct ProjectionState {
    sessions: BTreeMap<String, SessionState>,
    sessions_revision: u64,
    retired_instances: BTreeMap<String, u64>,
    remote_sessions: Option<CoreProjectionEvent>,
    workspace_menu: Option<CoreProjectionEvent>,
    workspace: Option<CoreProjectionEvent>,
    automations: Option<CoreProjectionEvent>,
}

struct ProjectionSnapshotProvider {
    projection: Arc<Mutex<ProjectionState>>,
}

impl SnapshotProvider for ProjectionSnapshotProvider {
    fn snapshot(&self, kind: &SnapshotKind, watermark: u64) -> Result<Snapshot, EventBusError> {
        let projection = self
            .projection
            .lock()
            .map_err(|_| EventBusError::new("daemon projection lock poisoned"))?;
        let (revision, payload) = match kind.0.as_str() {
            CORE_SNAPSHOT_SESSIONS => {
                let sessions = projection
                    .sessions
                    .values()
                    .map(session_dto)
                    .collect::<Vec<_>>();
                (
                    projection.sessions_revision,
                    serde_json::to_value(SessionStatesSnapshot {
                        projection_version: CORE_SESSION_PROJECTION_VERSION,
                        revision: projection.sessions_revision,
                        sessions,
                    })
                    .map_err(|error| EventBusError::new(error.to_string()))?,
                )
            }
            CORE_SNAPSHOT_REMOTE_SESSIONS => projection_value(&projection.remote_sessions),
            CORE_SNAPSHOT_WORKSPACE_MENU => projection_value(&projection.workspace_menu),
            CORE_SNAPSHOT_WORKSPACE => projection_value(&projection.workspace),
            CORE_SNAPSHOT_AUTOMATIONS => projection_value(&projection.automations),
            _ => {
                return Err(EventBusError::new(format!(
                    "unknown daemon projection snapshot kind: {}",
                    kind.0
                )));
            }
        };
        Ok(Snapshot {
            kind: kind.clone(),
            watermark,
            revision,
            payload,
        })
    }
}

fn projection_value(projection: &Option<CoreProjectionEvent>) -> (u64, serde_json::Value) {
    match projection {
        Some(projection) => (
            projection.revision,
            serde_json::to_value(projection).unwrap_or(serde_json::Value::Null),
        ),
        None => (
            0,
            serde_json::to_value(CoreProjectionEvent {
                projection_version: CORE_PROJECTION_VERSION,
                revision: 0,
                data: serde_json::Value::Null,
            })
            .unwrap_or(serde_json::Value::Null),
        ),
    }
}

pub(crate) struct DaemonEventHub {
    hub: Arc<EventHub>,
    projection: Arc<Mutex<ProjectionState>>,
    /// 每个终端正在显示的进度条（会话 id → 实例号、进度）。只为新订阅者补发：
    /// `terminal.progress` 没有快照，后连上的客户端靠它看到已经在跑的进度条。
    terminal_progress: Mutex<BTreeMap<String, (u64, smelt_event_protocol::TerminalProgress)>>,
}

impl DaemonEventHub {
    pub(crate) fn new() -> Result<EventHubHandle, String> {
        let mut registry = TopicRegistry::default();
        for descriptor in smelt_event_protocol::core_topic_descriptors()
            .map_err(|error| format!("invalid core topic descriptor: {error}"))?
        {
            registry
                .register(descriptor)
                .map_err(|error| format!("register core topic: {error}"))?;
        }
        let hub = Arc::new(EventHub::new(registry));
        let projection = Arc::new(Mutex::new(ProjectionState::default()));
        register_snapshot_providers(&hub, &projection)
            .map_err(|error| format!("register core snapshot provider: {error}"))?;
        Ok(Arc::new(Self {
            hub,
            projection,
            terminal_progress: Mutex::new(BTreeMap::new()),
        }))
    }

    pub(crate) fn runtime(&self) -> &Arc<EventHub> {
        &self.hub
    }

    pub(crate) fn synchronize_projection(&self, seed: DaemonProjectionSeed) {
        let mut projection = self.projection.lock().unwrap();
        let mut sessions = seed
            .sessions
            .into_iter()
            .filter(|state| {
                state.instance == 0
                    || !projection
                        .retired_instances
                        .get(&state.id)
                        .is_some_and(|retired| state.instance <= *retired)
            })
            .map(|state| (state.id.clone(), state))
            .collect::<BTreeMap<_, _>>();
        for (id, next) in &mut sessions {
            if let Some(current) = projection.sessions.get(id)
                && session_state_is_newer(current, next)
            {
                *next = current.clone();
            }
        }
        if session_projection_changed(&projection.sessions, &sessions) {
            projection.sessions = sessions;
            projection.sessions_revision = projection.sessions_revision.saturating_add(1);
        }
        merge_projection(
            &mut projection.remote_sessions,
            seed.remote_sessions.map(projection_event),
        );
        merge_projection(
            &mut projection.workspace_menu,
            seed.workspace_menu.map(projection_event),
        );
        merge_projection(
            &mut projection.workspace,
            seed.workspace.map(projection_event),
        );
        merge_projection(
            &mut projection.automations,
            seed.automations
                .map(|file| projection_event(file.live_projection())),
        );
    }

    pub(crate) fn publish_session(&self, state: &SessionState) -> Result<bool, EventBusError> {
        let mut projection = self
            .projection
            .lock()
            .map_err(|_| EventBusError::new("daemon projection lock poisoned"))?;
        if state.instance != 0
            && projection
                .retired_instances
                .get(&state.id)
                .is_some_and(|retired| state.instance <= *retired)
        {
            return Ok(false);
        }
        let changed = match projection.sessions.get(&state.id) {
            Some(current)
                if state.instance != 0
                    && current.instance != 0
                    && state.instance < current.instance =>
            {
                false
            }
            Some(current)
                if state.instance != 0
                    && current.instance != 0
                    && state.instance > current.instance =>
            {
                true
            }
            Some(current) if state.revision != 0 && state.revision <= current.revision => false,
            Some(_) | None => true,
        };
        if !changed {
            return Ok(false);
        }
        let dto = session_dto(state);
        let aggregate = session_aggregate(state);
        self.publish_core(
            CORE_TOPIC_SESSION_STATE_CHANGED,
            serde_json::to_value(&dto)
                .map_err(|error| EventBusError::new(format!("serialize session event: {error}")))?,
            aggregate,
        )?;
        projection.sessions.insert(state.id.clone(), state.clone());
        projection.sessions_revision = projection.sessions_revision.saturating_add(1);
        Ok(true)
    }

    pub(crate) fn remove_session(&self, id: &str, instance: u64) -> Result<bool, EventBusError> {
        let mut projection = self
            .projection
            .lock()
            .map_err(|_| EventBusError::new("daemon projection lock poisoned"))?;
        if instance != 0 {
            let retired = projection
                .retired_instances
                .entry(id.to_string())
                .or_default();
            *retired = (*retired).max(instance);
        }
        let Some(current) = projection.sessions.get(id).cloned() else {
            return Ok(true);
        };
        let blocked = if instance == 0 {
            current.instance != 0
        } else {
            current.instance != 0 && current.instance > instance
        };
        if blocked {
            return Ok(false);
        }
        let removed_at_ms = now_ms();
        let payload = SessionStateRemoved {
            session_id: id.to_string(),
            generation: current.instance,
            last_revision: current.revision,
            removed_at_ms,
        };
        let aggregate_revision = current.revision.checked_add(1);
        let aggregate = aggregate_revision.map(|revision| {
            (
                AggregateRef::new("session", format!("{}:{}", current.id, current.instance))
                    .expect("daemon session aggregate is valid"),
                revision,
            )
        });
        self.publish_core(
            CORE_TOPIC_SESSION_REMOVED,
            serde_json::to_value(payload)
                .map_err(|error| EventBusError::new(format!("serialize removal event: {error}")))?,
            aggregate,
        )?;
        projection.sessions.remove(id);
        projection.sessions_revision = projection.sessions_revision.saturating_add(1);
        Ok(true)
    }

    pub(crate) fn publish_remote_sessions(
        &self,
        snapshot: &RemoteSessionSnapshot,
    ) -> Result<bool, EventBusError> {
        self.publish_projection(
            CORE_TOPIC_REMOTE_SESSIONS_CHANGED,
            snapshot.revision,
            serde_json::to_value(snapshot).map_err(|error| {
                EventBusError::new(format!("serialize remote sessions: {error}"))
            })?,
            ProjectionTarget::RemoteSessions,
        )
    }

    pub(crate) fn publish_workspace_menu(
        &self,
        snapshot: &WorkspaceMenuSnapshot,
    ) -> Result<bool, EventBusError> {
        self.publish_projection(
            CORE_TOPIC_WORKSPACE_MENU_CHANGED,
            snapshot.revision,
            serde_json::to_value(snapshot).map_err(|error| {
                EventBusError::new(format!("serialize workspace menu: {error}"))
            })?,
            ProjectionTarget::WorkspaceMenu,
        )
    }

    /// 工作区目录改完之后推整份。版本不比已推出去的新就不推：并发的两次改动各自读回
    /// 整份目录，后读的那份一定带着更大的版本。
    pub(crate) fn publish_workspace(
        &self,
        directory: &smelt_core::workspace_directory::WorkspaceDirectory,
    ) -> Result<bool, EventBusError> {
        self.publish_projection(
            CORE_TOPIC_WORKSPACE_CHANGED,
            directory.revision,
            serde_json::to_value(directory).map_err(|error| {
                EventBusError::new(format!("serialize workspace directory: {error}"))
            })?,
            ProjectionTarget::Workspace,
        )
    }

    pub(crate) fn publish_automations(
        &self,
        snapshot: &AutomationFile,
    ) -> Result<bool, EventBusError> {
        let snapshot = snapshot.live_projection();
        self.publish_projection(
            CORE_TOPIC_AUTOMATIONS_CHANGED,
            snapshot.revision,
            serde_json::to_value(&snapshot)
                .map_err(|error| EventBusError::new(format!("serialize automations: {error}")))?,
            ProjectionTarget::Automations,
        )
    }

    fn publish_projection(
        &self,
        topic: &str,
        revision: u64,
        data: serde_json::Value,
        target: ProjectionTarget,
    ) -> Result<bool, EventBusError> {
        let mut projection = self
            .projection
            .lock()
            .map_err(|_| EventBusError::new("daemon projection lock poisoned"))?;
        let current = match target {
            ProjectionTarget::RemoteSessions => &projection.remote_sessions,
            ProjectionTarget::WorkspaceMenu => &projection.workspace_menu,
            ProjectionTarget::Workspace => &projection.workspace,
            ProjectionTarget::Automations => &projection.automations,
        };
        if current
            .as_ref()
            .is_some_and(|current| revision != 0 && revision <= current.revision)
        {
            return Ok(false);
        }
        let event = CoreProjectionEvent {
            projection_version: CORE_PROJECTION_VERSION,
            revision,
            data,
        };
        let aggregate = (revision != 0).then(|| {
            (
                AggregateRef::new("projection", topic.replace('.', "-"))
                    .expect("core projection aggregate is valid"),
                revision,
            )
        });
        self.publish_core(
            topic,
            serde_json::to_value(&event).map_err(|error| {
                EventBusError::new(format!("serialize core projection event: {error}"))
            })?,
            aggregate,
        )?;
        match target {
            ProjectionTarget::RemoteSessions => projection.remote_sessions = Some(event),
            ProjectionTarget::WorkspaceMenu => projection.workspace_menu = Some(event),
            ProjectionTarget::Workspace => projection.workspace = Some(event),
            ProjectionTarget::Automations => projection.automations = Some(event),
        }
        Ok(true)
    }

    /// 项目目录里的文件变了。没人订阅就不发。
    pub(crate) fn publish_project_changed(&self, root: &str) -> Result<(), EventBusError> {
        let topic = Topic::new(smelt_event_protocol::CORE_TOPIC_PROJECT_CHANGED)
            .expect("registered core topic is valid");
        if !self.hub.has_subscriber(&topic)? {
            return Ok(());
        }
        let payload = smelt_event_protocol::ProjectChanged {
            root: root.to_string(),
        };
        self.publish_core(
            smelt_event_protocol::CORE_TOPIC_PROJECT_CHANGED,
            serde_json::to_value(payload).map_err(|error| {
                EventBusError::new(format!("serialize project change: {error}"))
            })?,
            None,
        )?;
        Ok(())
    }

    /// 智能体的记忆目录里有文件变了（`agent_id` 为 `None` 是用户全局记忆）。没人订阅就不发。
    pub(crate) fn publish_agent_memory_changed(
        &self,
        agent_id: Option<&str>,
    ) -> Result<(), EventBusError> {
        let topic = Topic::new(smelt_event_protocol::CORE_TOPIC_AGENT_MEMORY_CHANGED)
            .expect("registered core topic is valid");
        if !self.hub.has_subscriber(&topic)? {
            return Ok(());
        }
        let payload = smelt_event_protocol::AgentMemoryChanged {
            agent_id: agent_id.map(str::to_string),
        };
        self.publish_core(
            smelt_event_protocol::CORE_TOPIC_AGENT_MEMORY_CHANGED,
            serde_json::to_value(payload).map_err(|error| {
                EventBusError::new(format!("serialize agent memory change: {error}"))
            })?,
            None,
        )?;
        Ok(())
    }

    /// 终端里的程序发了一条 OSC 通知。没人订阅就不发：通知是一次性的，不补发。
    pub(crate) fn publish_terminal_notification(
        &self,
        session_id: &str,
        message: &str,
    ) -> Result<(), EventBusError> {
        let topic = Topic::new(smelt_event_protocol::CORE_TOPIC_TERMINAL_NOTIFICATION)
            .expect("registered core topic is valid");
        if !self.hub.has_subscriber(&topic)? {
            return Ok(());
        }
        let payload = smelt_event_protocol::TerminalNotification {
            session_id: session_id.to_string(),
            message: message.to_string(),
        };
        self.publish_core(
            smelt_event_protocol::CORE_TOPIC_TERMINAL_NOTIFICATION,
            serde_json::to_value(payload).map_err(|error| {
                EventBusError::new(format!("serialize terminal notification: {error}"))
            })?,
            None,
        )?;
        Ok(())
    }

    /// 记下并广播一个终端的进度变化（`None` = 清掉）。记账和广播在同一把锁里：订阅时的补发不会
    /// 插在中间，把旧值发到新值后面。
    pub(crate) fn publish_terminal_progress(
        &self,
        session_id: &str,
        instance: u64,
        progress: Option<smelt_event_protocol::TerminalProgress>,
    ) -> Result<(), EventBusError> {
        let mut current = self
            .terminal_progress
            .lock()
            .map_err(|_| EventBusError::new("terminal progress lock poisoned"))?;
        match progress {
            Some(progress) => {
                current.insert(session_id.to_string(), (instance, progress));
            }
            None => {
                current.remove(session_id);
            }
        }
        self.broadcast_terminal_progress(session_id, progress)
    }

    /// 新订阅了 `terminal.progress` 的客户端连上后调：把正在显示的进度条再发一遍。
    /// 广播会让老订阅者也收到一份，进度是状态，重复无害。
    pub(crate) fn replay_terminal_progress(&self) -> Result<(), EventBusError> {
        let current = self
            .terminal_progress
            .lock()
            .map_err(|_| EventBusError::new("terminal progress lock poisoned"))?;
        for (session_id, (_, progress)) in current.iter() {
            self.broadcast_terminal_progress(session_id, Some(*progress))?;
        }
        Ok(())
    }

    /// 这个终端实例结束了，不再补发它的进度条。同 id 的新实例不受影响。
    pub(crate) fn forget_terminal_progress(&self, session_id: &str, instance: u64) {
        let Ok(mut current) = self.terminal_progress.lock() else {
            return;
        };
        if current
            .get(session_id)
            .is_some_and(|(owner, _)| *owner == instance)
        {
            current.remove(session_id);
        }
    }

    /// 没人订阅就不发。
    fn broadcast_terminal_progress(
        &self,
        session_id: &str,
        progress: Option<smelt_event_protocol::TerminalProgress>,
    ) -> Result<(), EventBusError> {
        let topic = Topic::new(smelt_event_protocol::CORE_TOPIC_TERMINAL_PROGRESS)
            .expect("registered core topic is valid");
        if !self.hub.has_subscriber(&topic)? {
            return Ok(());
        }
        let payload = smelt_event_protocol::TerminalProgressChanged {
            session_id: session_id.to_string(),
            progress,
        };
        self.publish_core(
            smelt_event_protocol::CORE_TOPIC_TERMINAL_PROGRESS,
            serde_json::to_value(payload).map_err(|error| {
                EventBusError::new(format!("serialize terminal progress: {error}"))
            })?,
            None,
        )?;
        Ok(())
    }

    /// 有没有在线的桌面端能接外观改动。
    pub(crate) fn has_appearance_listener(&self) -> Result<bool, EventBusError> {
        let topic = Topic::new(smelt_event_protocol::CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED)
            .expect("registered core topic is valid");
        self.hub.has_subscriber(&topic)
    }

    /// 把 CLI / Control API 发来的外观改动转给桌面端。返回 `false` 表示没有在线的
    /// 桌面端，改动无处生效。
    pub(crate) fn request_appearance_change(
        &self,
        settings: std::collections::BTreeMap<String, serde_json::Value>,
    ) -> Result<bool, EventBusError> {
        let topic = Topic::new(smelt_event_protocol::CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED)
            .expect("registered core topic is valid");
        if !self.hub.has_subscriber(&topic)? {
            return Ok(false);
        }
        let payload = smelt_event_protocol::AppearanceChangeRequested { settings };
        self.publish_core(
            smelt_event_protocol::CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED,
            serde_json::to_value(payload).map_err(|error| {
                EventBusError::new(format!("serialize appearance change request: {error}"))
            })?,
            None,
        )?;
        Ok(true)
    }

    /// 请桌面端把一段冷恢复的对话拉起来。返回 `false` 表示此刻没有任何一方
    /// 能兑现这个请求（桌面端没开），调用方应立刻放弃而不是空等。
    pub(crate) fn request_conversation_wake(
        &self,
        session_id: &str,
    ) -> Result<bool, EventBusError> {
        let topic = Topic::new(CORE_TOPIC_CONVERSATION_WAKE_REQUESTED)
            .expect("registered core topic is valid");
        if !self.hub.has_subscriber(&topic)? {
            return Ok(false);
        }
        let payload = ConversationWakeRequested {
            session_id: session_id.to_string(),
        };
        self.publish_core(
            CORE_TOPIC_CONVERSATION_WAKE_REQUESTED,
            serde_json::to_value(payload).map_err(|error| {
                EventBusError::new(format!("serialize conversation wake request: {error}"))
            })?,
            None,
        )?;
        Ok(true)
    }

    fn publish_core(
        &self,
        topic: &str,
        payload: serde_json::Value,
        aggregate: Option<(AggregateRef, u64)>,
    ) -> Result<Arc<StoredEvent>, EventBusError> {
        let event_id = EventId::new(format!("event-{}", uuid::Uuid::new_v4().simple()))
            .expect("generated event id is valid");
        let envelope = core_envelope(
            event_id,
            topic,
            payload,
            aggregate,
            now_ms(),
            CorrelationId::new(format!("correlation-{}", uuid::Uuid::new_v4().simple()))
                .expect("generated correlation id is valid"),
        );
        self.hub
            .publish(&PublisherIdentity::Core, envelope, &BTreeSet::new())
    }

    pub(crate) fn subscribe(
        &self,
        plugin_id: PluginId,
        subscription_id: SubscriptionId,
        topics: BTreeSet<Topic>,
        capabilities: &BTreeSet<Capability>,
    ) -> Result<smelt_event_bus::Subscription, EventBusError> {
        self.hub.subscribe(
            plugin_id,
            subscription_id,
            topics,
            capabilities,
            MAILBOX_CAPACITY,
        )
    }

    #[cfg(test)]
    pub(crate) fn legacy_snapshot(&self) -> Result<serde_json::Value, EventBusError> {
        let sessions = self
            .hub
            .snapshot(&SnapshotKind(CORE_SNAPSHOT_SESSIONS.to_string()))?;
        let sessions: SessionStatesSnapshot = serde_json::from_value(sessions.payload)
            .map_err(|error| EventBusError::new(format!("decode session snapshot: {error}")))?;
        let mut root = serde_json::Map::from_iter([(
            "sessions".to_string(),
            serde_json::to_value(sessions.sessions)
                .map_err(|error| EventBusError::new(error.to_string()))?,
        )]);
        for (field, kind) in [
            ("remote_sessions", CORE_SNAPSHOT_REMOTE_SESSIONS),
            ("workspace_menu", CORE_SNAPSHOT_WORKSPACE_MENU),
            ("workspace", CORE_SNAPSHOT_WORKSPACE),
            ("automations", CORE_SNAPSHOT_AUTOMATIONS),
        ] {
            let snapshot = self.hub.snapshot(&SnapshotKind(kind.to_string()))?;
            let projection: CoreProjectionEvent = serde_json::from_value(snapshot.payload)
                .map_err(|error| EventBusError::new(format!("decode {field} snapshot: {error}")))?;
            if !projection.data.is_null() {
                root.insert(field.to_string(), projection.data);
            }
        }
        Ok(serde_json::Value::Object(root))
    }
}

fn core_envelope(
    event_id: EventId,
    topic: &str,
    payload: serde_json::Value,
    aggregate: Option<(AggregateRef, u64)>,
    occurred_at_ms: u64,
    correlation_id: CorrelationId,
) -> EventEnvelope<serde_json::Value> {
    let (aggregate, aggregate_revision) = match aggregate {
        Some((aggregate, revision)) => (Some(aggregate), Some(revision)),
        None => (None, None),
    };
    EventEnvelope {
        event_id,
        topic: Topic::new(topic).expect("registered core topic is valid"),
        schema_version: smelt_event_protocol::core_topic_schema_version(topic),
        occurred_at_ms,
        aggregate,
        aggregate_revision,
        source: EventSource::Core,
        correlation_id,
        causation_id: None,
        hop_count: 0,
        payload,
    }
}

enum ProjectionTarget {
    RemoteSessions,
    WorkspaceMenu,
    Workspace,
    Automations,
}

fn register_snapshot_providers(
    hub: &EventHub,
    projection: &Arc<Mutex<ProjectionState>>,
) -> Result<(), EventBusError> {
    let projection_provider: Arc<dyn SnapshotProvider> = Arc::new(ProjectionSnapshotProvider {
        projection: Arc::clone(projection),
    });
    for kind in [
        CORE_SNAPSHOT_SESSIONS,
        CORE_SNAPSHOT_REMOTE_SESSIONS,
        CORE_SNAPSHOT_WORKSPACE_MENU,
        CORE_SNAPSHOT_WORKSPACE,
        CORE_SNAPSHOT_AUTOMATIONS,
    ] {
        hub.register_snapshot_provider(
            SnapshotKind(kind.to_string()),
            Arc::clone(&projection_provider),
        )?;
    }
    Ok(())
}

fn projection_event<T: serde::Serialize>(value: T) -> CoreProjectionEvent {
    let data = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    let revision = data
        .get("revision")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    CoreProjectionEvent {
        projection_version: CORE_PROJECTION_VERSION,
        revision,
        data,
    }
}

fn session_projection_changed(
    current: &BTreeMap<String, SessionState>,
    next: &BTreeMap<String, SessionState>,
) -> bool {
    current.len() != next.len()
        || current.iter().any(|(id, current)| {
            next.get(id).is_none_or(|next| {
                current.instance != next.instance || current.revision != next.revision
            })
        })
}

fn session_state_is_newer(current: &SessionState, next: &SessionState) -> bool {
    current.instance > next.instance
        || (current.instance == next.instance && current.revision > next.revision)
}

fn merge_projection(current: &mut Option<CoreProjectionEvent>, next: Option<CoreProjectionEvent>) {
    if let Some(next) = next
        && current
            .as_ref()
            .is_none_or(|current| next.revision >= current.revision)
    {
        *current = Some(next);
    }
}

fn session_aggregate(state: &SessionState) -> Option<(AggregateRef, u64)> {
    (state.revision != 0).then(|| {
        (
            AggregateRef::new("session", format!("{}:{}", state.id, state.instance))
                .expect("daemon session aggregate is valid"),
            state.revision,
        )
    })
}

fn session_dto(state: &SessionState) -> SessionStateChanged {
    SessionStateChanged {
        session_id: state.id.clone(),
        generation: state.instance,
        revision: state.revision,
        cwd: state.cwd.clone(),
        launch: state.launch.clone(),
        provider: state.provider.clone(),
        conversation_id: state.conversation_id.clone(),
        title: state.title.clone(),
        phase: match state.phase {
            Phase::Connecting => CoreSessionPhase::Connecting,
            Phase::Thinking => CoreSessionPhase::Thinking,
            Phase::ExecutingTool => CoreSessionPhase::ExecutingTool,
            Phase::AwaitingApproval => CoreSessionPhase::AwaitingApproval,
            Phase::WaitingForUser => CoreSessionPhase::WaitingForUser,
            Phase::Succeeded => CoreSessionPhase::Succeeded,
            Phase::Failed => CoreSessionPhase::Failed,
            Phase::Idle => CoreSessionPhase::Idle,
            Phase::Dead => CoreSessionPhase::Dead,
        },
        phase_since_unix_seconds: state.phase_since,
        pending_question: state.pending_question.clone(),
        tokens_used: state.tokens_used,
        branch: state.branch.clone(),
        dirty_files: state.dirty_files.clone(),
        updated_at_unix_seconds: state.updated_at,
        structured_events: state.structured_events,
        turn_events: state.turn_events,
        agent_event_version: state.agent_event_version,
        runtime: state.runtime,
    }
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

pub(crate) fn delivery_to_subscribe_message(
    delivery: Delivery,
) -> smelt_event_protocol::SubscribeMessage {
    match delivery {
        Delivery::Snapshot(snapshot) => smelt_event_protocol::SubscribeMessage::Snapshot {
            kind: snapshot.kind,
            watermark: snapshot.watermark,
            revision: snapshot.revision,
            payload: snapshot.payload,
        },
        Delivery::Event(event) => smelt_event_protocol::SubscribeMessage::Event {
            sequence: (event.sequence != 0).then_some(event.sequence),
            envelope: event.envelope.clone(),
        },
        Delivery::Lag {
            dropped,
            resume_after,
        } => smelt_event_protocol::SubscribeMessage::Lag {
            dropped,
            resume_after,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_projection_is_available_to_snapshot_recovery() {
        let event_hub = DaemonEventHub::new().unwrap();
        let snapshot = AutomationFile {
            revision: 7,
            ..Default::default()
        };

        assert!(event_hub.publish_automations(&snapshot).unwrap());
        assert_eq!(
            event_hub.legacy_snapshot().unwrap()["automations"]["revision"],
            7
        );
        assert!(!event_hub.publish_automations(&snapshot).unwrap());
    }

    #[test]
    fn automation_live_projection_omits_run_bodies() {
        let event_hub = DaemonEventHub::new().unwrap();
        let mut snapshot = AutomationFile {
            revision: 4,
            ..Default::default()
        };
        snapshot.runs.push(smelt_core::automation::AutomationRun {
            id: "run-1".into(),
            automation_id: "auto-1".into(),
            context: smelt_core::automation::AutomationRunContext {
                automation_name: "Report".into(),
                action: smelt_core::automation::AutomationAction::Agent {
                    agent_definition_id: "agent-1".into(),
                    prompt: Some("Go".into()),
                },
                cwd: "/tmp".into(),
                trigger_payload: Some(serde_json::json!({"n": 1})),
                prompt: Some("Go".into()),
                agent_definition_name: Some("Agent".into()),
                engine_kind_id: Some("pi".into()),
                agent_instructions: Some("Be useful".into()),
            },
            source: smelt_core::automation::AutomationRunSource::Manual,
            scheduled_for: None,
            status: smelt_core::automation::AutomationRunStatus::Completed,
            created_at: 1,
            started_at: Some(1),
            delivery_attempt_at: Some(1),
            delivery_attempts: 1,
            finished_at: Some(2),
            session_id: None,
            provider_session_id: None,
            output: Some("blob".into()),
            error: None,
            runtime_released_at: Some(2),
        });
        assert!(event_hub.publish_automations(&snapshot).unwrap());
        let run = &event_hub.legacy_snapshot().unwrap()["automations"]["runs"][0];
        assert!(run.get("output").is_none() || run["output"].is_null());
        assert!(run["context"].get("trigger_payload").is_none());
        assert!(run["context"].get("agent_instructions").is_none());
        assert_eq!(run["context"]["prompt"], "Go");
    }
}
