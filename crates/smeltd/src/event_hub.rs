//! smeltd domain projections backed by the single process-wide `smelt_event_bus::EventHub`.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use smelt_event_bus::{
    Delivery, EventBusError, EventHub, MemoryEventStore, Snapshot, SnapshotProvider, StoredEvent,
    TopicRegistry,
};
use smelt_plugin_api::{
    AgentMessageDelivered, AggregateRef, CORE_PROJECTION_VERSION, CORE_SESSION_PROJECTION_VERSION,
    CORE_SNAPSHOT_AUTOMATIONS, CORE_SNAPSHOT_REMOTE_SESSIONS, CORE_SNAPSHOT_SESSIONS,
    CORE_SNAPSHOT_WORKSPACE_MENU, CORE_TOPIC_AGENT_MESSAGE_DELIVERED,
    CORE_TOPIC_AUTOMATIONS_CHANGED, CORE_TOPIC_REMOTE_SESSIONS_CHANGED, CORE_TOPIC_SESSION_REMOVED,
    CORE_TOPIC_SESSION_STATE_CHANGED, CORE_TOPIC_WORKSPACE_MENU_CHANGED, Capability,
    CoreProjectionEvent, CoreSessionPhase, CorrelationId, DeliveryClass, EventEnvelope, EventId,
    EventSource, PluginId, PublisherIdentity, SessionStateChanged, SessionStateRemoved,
    SessionStatesSnapshot, SnapshotKind, SubscriptionId, Topic,
};
use smelt_store::{PeerMessageCommandResult, PeerMessageCommitOutcome};

use super::{
    AutomationFile, DaemonProjectionSeed, Phase, RemoteSessionSnapshot, SessionState,
    WorkspaceMenuSnapshot,
};

const MAILBOX_CAPACITY: usize = 256;
const OUTBOX_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) type EventHubHandle = Arc<DaemonEventHub>;

#[derive(Default)]
struct ProjectionState {
    sessions: BTreeMap<String, SessionState>,
    sessions_revision: u64,
    retired_instances: BTreeMap<String, u64>,
    remote_sessions: Option<CoreProjectionEvent>,
    workspace_menu: Option<CoreProjectionEvent>,
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

struct DurableDispatcher {
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
}

#[derive(Clone)]
struct PeerMessageLedgerEntry {
    result: PeerMessageCommandResult,
    event: EventEnvelope<serde_json::Value>,
    persisted: bool,
}

pub(crate) struct DaemonEventHub {
    hub: Arc<EventHub>,
    projection: Arc<Mutex<ProjectionState>>,
    store: Option<Arc<smelt_store::Store>>,
    dispatcher: Option<DurableDispatcher>,
    peer_messages:
        Arc<Mutex<BTreeMap<(String, smelt_plugin_api::CommandId), PeerMessageLedgerEntry>>>,
    allow_in_memory_peer_messages: bool,
}

impl DaemonEventHub {
    pub(crate) fn open_default() -> Result<EventHubHandle, String> {
        let store = match smelt_paths::smelt_home() {
            Some(root) => match std::fs::create_dir_all(&root) {
                Ok(()) => {
                    let database = root.join(smelt_store::DATABASE_FILE_NAME);
                    match smelt_store::Store::open_or_create(&database) {
                        Ok(store) => Some(Arc::new(store)),
                        Err(error) => {
                            log_durable_unavailable(format!(
                                "EventHub Store 打开失败（{}）：{error}",
                                database.display()
                            ));
                            None
                        }
                    }
                }
                Err(error) => {
                    log_durable_unavailable(format!(
                        "EventHub Store 目录创建失败（{}）：{error}",
                        root.display()
                    ));
                    None
                }
            },
            None => {
                log_durable_unavailable(
                    "无法定位 home 目录，不能打开 ~/.smelt/smelt.sqlite3".to_string(),
                );
                None
            }
        };
        Self::build(store)
    }

    pub(crate) fn in_memory() -> EventHubHandle {
        let mut registry = TopicRegistry::default();
        for descriptor in smelt_plugin_api::core_topic_descriptors().unwrap() {
            registry.register(descriptor).unwrap();
        }
        let projection = Arc::new(Mutex::new(ProjectionState::default()));
        let store = Arc::new(MemoryEventStore::default());
        let hub = Arc::new(EventHub::new(registry, Some(store)));
        register_snapshot_providers(&hub, &projection).unwrap();
        Arc::new(Self {
            hub,
            projection,
            store: None,
            dispatcher: None,
            peer_messages: Arc::new(Mutex::new(BTreeMap::new())),
            allow_in_memory_peer_messages: true,
        })
    }

    fn build(store: Option<Arc<smelt_store::Store>>) -> Result<EventHubHandle, String> {
        let mut registry = TopicRegistry::default();
        for descriptor in smelt_plugin_api::core_topic_descriptors()
            .map_err(|error| format!("invalid core topic descriptor: {error}"))?
        {
            registry
                .register(descriptor)
                .map_err(|error| format!("register core topic: {error}"))?;
        }
        let event_store = store
            .as_ref()
            .map(|store| Arc::clone(store) as Arc<dyn smelt_event_bus::EventStore>);
        let hub = Arc::new(EventHub::new(registry, event_store));
        let projection = Arc::new(Mutex::new(ProjectionState::default()));
        let peer_messages = Arc::new(Mutex::new(BTreeMap::new()));
        register_snapshot_providers(&hub, &projection)
            .map_err(|error| format!("register core snapshot provider: {error}"))?;

        let dispatcher = store.as_ref().map(|store| {
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = Arc::clone(&stop);
            let worker_store = Arc::clone(store);
            let worker_hub = Arc::clone(&hub);
            let worker_peer_messages = Arc::clone(&peer_messages);
            let worker = thread::spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    if let Err(error) = persist_pending_peer_messages(
                        Some(&worker_store),
                        &worker_hub,
                        &worker_peer_messages,
                        false,
                    ) {
                        eprintln!("[event-hub] peer message fact persistence failed: {error}");
                    }
                    if let Err(error) = dispatch_outbox_once(&worker_store, &worker_hub) {
                        eprintln!("[event-hub] durable outbox dispatch failed: {error}");
                    }
                    if let Err(error) = worker_hub.pump_durable_subscribers() {
                        eprintln!("[event-hub] durable subscriber pump failed: {error}");
                    }
                    if let Err(error) = worker_hub.flush_cursors() {
                        eprintln!("[event-hub] durable cursor flush failed: {error}");
                    }
                    thread::sleep(OUTBOX_POLL_INTERVAL);
                }
            });
            DurableDispatcher {
                stop,
                thread: Mutex::new(Some(worker)),
            }
        });
        Ok(Arc::new(Self {
            hub,
            projection,
            store,
            dispatcher,
            peer_messages,
            allow_in_memory_peer_messages: false,
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
            ProjectionTarget::Automations => projection.automations = Some(event),
        }
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) fn publish_agent_message_delivered(
        &self,
        message_id: &str,
        source_session_id: &str,
        target_session_id: &str,
    ) -> Result<Arc<StoredEvent>, EventBusError> {
        let delivered_at_ms = now_ms();
        let payload = AgentMessageDelivered {
            message_id: message_id.to_string(),
            source_session_id: source_session_id.to_string(),
            target_session_id: target_session_id.to_string(),
            delivered_at_ms,
        };
        self.publish_core(
            CORE_TOPIC_AGENT_MESSAGE_DELIVERED,
            serde_json::to_value(payload).map_err(|error| {
                EventBusError::new(format!("serialize agent delivery event: {error}"))
            })?,
            None,
        )
    }

    pub(crate) fn supports_peer_message_persistence(&self) -> bool {
        self.store.is_some() || self.allow_in_memory_peer_messages
    }

    pub(crate) fn peer_message_result(
        &self,
        source_session_id: &str,
        command_id: &smelt_plugin_api::CommandId,
    ) -> Result<Option<PeerMessageCommandResult>, EventBusError> {
        let key = (source_session_id.to_string(), command_id.clone());
        if self.peer_messages.lock().unwrap().contains_key(&key) {
            persist_peer_message(
                self.store.as_deref(),
                &self.hub,
                &self.peer_messages,
                &key,
                self.allow_in_memory_peer_messages,
            )
            .map_err(EventBusError::new)?;
            let result = self
                .peer_messages
                .lock()
                .unwrap()
                .get(&key)
                .map(|entry| entry.result.clone());
            if self.store.is_some() {
                self.peer_messages.lock().unwrap().remove(&key);
            }
            return Ok(result);
        }
        let Some(store) = &self.store else {
            return Ok(None);
        };
        store
            .get_peer_message_command_result(source_session_id, command_id)
            .map_err(EventBusError::new)
    }

    pub(crate) fn record_peer_message_delivery(
        &self,
        result: PeerMessageCommandResult,
    ) -> Result<PeerMessageCommandResult, EventBusError> {
        let key = (result.source_session_id.clone(), result.command_id.clone());
        let event = agent_message_delivered_envelope(&result)?;
        self.peer_messages
            .lock()
            .unwrap()
            .entry(key.clone())
            .or_insert(PeerMessageLedgerEntry {
                result,
                event,
                persisted: false,
            });
        persist_peer_message(
            self.store.as_deref(),
            &self.hub,
            &self.peer_messages,
            &key,
            self.allow_in_memory_peer_messages,
        )
        .map_err(EventBusError::new)?;
        let result = self
            .peer_messages
            .lock()
            .unwrap()
            .get(&key)
            .expect("peer message ledger entry must remain present")
            .result
            .clone();
        if self.store.is_some() {
            self.peer_messages.lock().unwrap().remove(&key);
        }
        Ok(result)
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
        delivery: DeliveryClass,
        capabilities: &BTreeSet<Capability>,
    ) -> Result<smelt_event_bus::Subscription, EventBusError> {
        self.hub.subscribe(
            plugin_id,
            subscription_id,
            topics,
            delivery,
            capabilities,
            MAILBOX_CAPACITY,
        )
    }

    #[cfg(test)]
    pub(crate) fn legacy_snapshot(&self) -> Result<serde_json::Value, EventBusError> {
        let sessions = self.hub.snapshot(
            &SnapshotKind(CORE_SNAPSHOT_SESSIONS.to_string()),
            DeliveryClass::Ephemeral,
        )?;
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
            ("automations", CORE_SNAPSHOT_AUTOMATIONS),
        ] {
            let snapshot = self
                .hub
                .snapshot(&SnapshotKind(kind.to_string()), DeliveryClass::Ephemeral)?;
            let projection: CoreProjectionEvent = serde_json::from_value(snapshot.payload)
                .map_err(|error| EventBusError::new(format!("decode {field} snapshot: {error}")))?;
            if !projection.data.is_null() {
                root.insert(field.to_string(), projection.data);
            }
        }
        Ok(serde_json::Value::Object(root))
    }

    pub(crate) fn flush_for_shutdown(&self) -> Result<(), EventBusError> {
        persist_pending_peer_messages(
            self.store.as_deref(),
            &self.hub,
            &self.peer_messages,
            self.allow_in_memory_peer_messages,
        )
        .map_err(EventBusError::new)?;
        self.hub.flush_durable()?;
        if let Some(store) = &self.store {
            loop {
                let dispatched =
                    dispatch_outbox_once(store, &self.hub).map_err(EventBusError::new)?;
                if dispatched == 0 {
                    break;
                }
            }
        }
        self.hub.flush_cursors()?;
        Ok(())
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
        schema_version: smelt_plugin_api::core_topic_schema_version(topic),
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

fn agent_message_delivered_envelope(
    result: &PeerMessageCommandResult,
) -> Result<EventEnvelope<serde_json::Value>, EventBusError> {
    let payload = AgentMessageDelivered {
        message_id: result.message_id.clone(),
        source_session_id: result.source_session_id.clone(),
        target_session_id: result.target_session_id.clone(),
        delivered_at_ms: result.completed_at_ms,
    };
    Ok(EventEnvelope {
        event_id: EventId::new(format!("event-{}", uuid::Uuid::new_v4().simple()))
            .expect("generated event id is valid"),
        topic: Topic::new(CORE_TOPIC_AGENT_MESSAGE_DELIVERED)
            .expect("registered core topic is valid"),
        schema_version: 1,
        occurred_at_ms: result.completed_at_ms,
        aggregate: None,
        aggregate_revision: None,
        source: EventSource::Core,
        correlation_id: CorrelationId::new(result.command_id.as_str().to_string())
            .map_err(|error| EventBusError::new(error.to_string()))?,
        causation_id: None,
        hop_count: 0,
        payload: serde_json::to_value(payload)
            .map_err(|error| EventBusError::new(error.to_string()))?,
    })
}

fn persist_peer_message(
    store: Option<&smelt_store::Store>,
    hub: &EventHub,
    ledger: &Mutex<BTreeMap<(String, smelt_plugin_api::CommandId), PeerMessageLedgerEntry>>,
    key: &(String, smelt_plugin_api::CommandId),
    allow_in_memory: bool,
) -> Result<(), String> {
    let mut ledger = ledger
        .lock()
        .map_err(|_| "peer message ledger lock poisoned".to_string())?;
    let Some(entry) = ledger.get_mut(key) else {
        return Ok(());
    };
    if entry.persisted {
        return Ok(());
    }
    match store {
        Some(store) => match store.commit_peer_message_delivery(&entry.result, &entry.event)? {
            PeerMessageCommitOutcome::Inserted => {}
            PeerMessageCommitOutcome::Existing(existing)
            | PeerMessageCommitOutcome::Conflict(existing) => entry.result = existing,
        },
        None if allow_in_memory => {
            hub.publish(
                &PublisherIdentity::Core,
                entry.event.clone(),
                &BTreeSet::new(),
            )
            .map_err(|error| error.to_string())?;
        }
        None => return Err("durable peer message store is unavailable".to_string()),
    }
    entry.persisted = true;
    Ok(())
}

fn persist_pending_peer_messages(
    store: Option<&smelt_store::Store>,
    hub: &EventHub,
    ledger: &Mutex<BTreeMap<(String, smelt_plugin_api::CommandId), PeerMessageLedgerEntry>>,
    allow_in_memory: bool,
) -> Result<(), String> {
    let keys = ledger
        .lock()
        .map_err(|_| "peer message ledger lock poisoned".to_string())?
        .iter()
        .filter(|(_, entry)| !entry.persisted)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    let mut first_error = None;
    for key in keys {
        if let Err(error) = persist_peer_message(store, hub, ledger, &key, allow_in_memory)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn log_durable_unavailable(reason: String) {
    let message = format!("{reason}；durable 事件能力已关闭");
    eprintln!("[event-hub] {message}");
    smelt_core::app_log::error("smeltd", &message);
}

impl Drop for DaemonEventHub {
    fn drop(&mut self) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.stop.store(true, Ordering::Release);
            if let Ok(mut worker) = dispatcher.thread.lock()
                && let Some(worker) = worker.take()
            {
                let _ = worker.join();
            }
        }
        if let Err(error) = self.flush_for_shutdown() {
            eprintln!("[event-hub] final durable flush failed: {error}");
        }
    }
}

enum ProjectionTarget {
    RemoteSessions,
    WorkspaceMenu,
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
        CORE_SNAPSHOT_AUTOMATIONS,
    ] {
        hub.register_snapshot_provider(
            SnapshotKind(kind.to_string()),
            Arc::clone(&projection_provider),
        )?;
    }
    Ok(())
}

fn dispatch_outbox_once(store: &smelt_store::Store, hub: &EventHub) -> Result<usize, String> {
    let events = store.dispatch_pending_outbox(smelt_event_bus::DEFAULT_DURABLE_LOAD_BATCH)?;
    let count = events.len();
    for event in events {
        hub.dispatch_stored(event)
            .map_err(|error| error.to_string())?;
    }
    Ok(count)
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
        agent_mcp: state.agent_mcp,
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
) -> smelt_plugin_api::SubscribeMessage {
    match delivery {
        Delivery::Snapshot(snapshot) => smelt_plugin_api::SubscribeMessage::Snapshot {
            kind: snapshot.kind,
            watermark: snapshot.watermark,
            revision: snapshot.revision,
            payload: snapshot.payload,
        },
        Delivery::Event(event) => smelt_plugin_api::SubscribeMessage::Event {
            sequence: (event.sequence != 0).then_some(event.sequence),
            envelope: event.envelope.clone(),
        },
        Delivery::Lag {
            dropped,
            resume_after,
        } => smelt_plugin_api::SubscribeMessage::Lag {
            dropped,
            resume_after,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn durable_envelope(id: &str) -> EventEnvelope<serde_json::Value> {
        EventEnvelope {
            event_id: EventId::new(id).unwrap(),
            topic: Topic::new(CORE_TOPIC_AGENT_MESSAGE_DELIVERED).unwrap(),
            schema_version: 1,
            occurred_at_ms: 1,
            aggregate: None,
            aggregate_revision: None,
            source: EventSource::Core,
            correlation_id: CorrelationId::new(format!("correlation-{id}")).unwrap(),
            causation_id: None,
            hop_count: 0,
            payload: serde_json::json!({
                "message_id": id,
                "source_session_id": "source",
                "target_session_id": "target",
                "delivered_at_ms": 1,
            }),
        }
    }

    fn wait_for(timeout: Duration, mut condition: impl FnMut() -> bool) {
        let started = std::time::Instant::now();
        while !condition() {
            assert!(started.elapsed() < timeout, "condition timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn automation_projection_is_available_to_snapshot_recovery() {
        let event_hub = DaemonEventHub::in_memory();
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
        let event_hub = DaemonEventHub::in_memory();
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

    #[test]
    fn dispatcher_moves_startup_and_runtime_outbox_events() {
        let parent = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/smeltd-event-hub-tests");
        let root = parent.join(uuid::Uuid::new_v4().simple().to_string());
        std::fs::create_dir_all(&root).unwrap();
        let store = Arc::new(
            smelt_store::Store::open_or_create(root.join(smelt_store::DATABASE_FILE_NAME)).unwrap(),
        );
        store
            .append_outbox_batch(&[durable_envelope("startup-event")])
            .unwrap();
        let event_hub = DaemonEventHub::build(Some(Arc::clone(&store))).unwrap();
        wait_for(Duration::from_secs(2), || {
            event_hub.runtime().high_watermark().unwrap() == 1
                && store.load_pending_outbox(10).unwrap().is_empty()
        });

        let plugin_id = PluginId::new("test.outbox-dispatcher").unwrap();
        let subscription_id = SubscriptionId::new("delivered").unwrap();
        let subscription = event_hub
            .subscribe(
                plugin_id.clone(),
                subscription_id.clone(),
                BTreeSet::from([Topic::new(CORE_TOPIC_AGENT_MESSAGE_DELIVERED).unwrap()]),
                DeliveryClass::Durable,
                &BTreeSet::from([Capability::new(
                    smelt_plugin_api::CORE_CAPABILITY_AGENT_MESSAGE_READ,
                )
                .unwrap()]),
            )
            .unwrap();
        assert_eq!(
            event_hub
                .runtime()
                .dispatch_durable(&plugin_id, &subscription_id, 10)
                .unwrap(),
            1
        );
        let Delivery::Event(startup) = subscription.recv().unwrap() else {
            panic!("expected startup outbox event");
        };
        assert_eq!(startup.envelope.event_id.as_str(), "startup-event");
        event_hub
            .runtime()
            .ack(
                &plugin_id,
                &smelt_plugin_api::Ack {
                    subscription_id,
                    sequence: startup.sequence,
                    event_id: startup.envelope.event_id.clone(),
                },
            )
            .unwrap();

        store
            .append_outbox_batch(&[durable_envelope("runtime-event")])
            .unwrap();
        wait_for(Duration::from_secs(2), || {
            matches!(
                subscription.try_recv(),
                Ok(Delivery::Event(ref event))
                    if event.envelope.event_id.as_str() == "runtime-event"
            )
        });
        event_hub.flush_for_shutdown().unwrap();
        drop(subscription);
        drop(event_hub);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
        let _ = std::fs::remove_dir(parent);
    }
}
