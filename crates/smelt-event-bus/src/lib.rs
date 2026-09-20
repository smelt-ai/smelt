//! Domain-independent event routing and persistence contracts.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use smelt_plugin_api::{
    Capability, CommandId, DeliveryClass, EventEnvelope, EventId, PluginId, PublisherIdentity,
    SnapshotKind, SubscriptionId, Topic, TopicDescriptor,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const DEFAULT_MAX_HOPS: u16 = 16;
pub const DEFAULT_DURABLE_LOAD_BATCH: usize = 128;
pub const DEFAULT_WRITER_BATCH_SIZE: usize = 64;
pub const DEFAULT_WRITER_MAX_WAIT_MS: u64 = 10;
pub const DEFAULT_CURSOR_BATCH_SIZE: usize = 50;
pub const DEFAULT_CURSOR_MAX_WAIT_MS: u64 = 100;
pub const DEFAULT_CAUSAL_CACHE_CAPACITY: usize = 1024;
const DURABLE_RETRY_BASE_DELAY_MS: u64 = 250;
const MAX_DURABLE_DELIVERY_ATTEMPTS: u32 = 5;

fn durable_retry_delay_ms(attempts: u32) -> u64 {
    DURABLE_RETRY_BASE_DELAY_MS.saturating_mul(1_u64 << attempts.saturating_sub(1).min(4))
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventBusError(String);

impl EventBusError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for EventBusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EventBusError {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredEvent {
    pub sequence: u64,
    pub envelope: EventEnvelope<serde_json::Value>,
    /// Canonical serialized envelope, produced once and shared across subscribers.
    pub wire_bytes: Vec<u8>,
}

impl StoredEvent {
    pub fn new(
        sequence: u64,
        envelope: EventEnvelope<serde_json::Value>,
    ) -> Result<Self, EventBusError> {
        let wire_bytes = serde_json::to_vec(&envelope)
            .map_err(|error| EventBusError::new(format!("serialize event: {error}")))?;
        Ok(Self {
            sequence,
            envelope,
            wire_bytes,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeadLetter {
    pub plugin_id: PluginId,
    pub subscription_id: SubscriptionId,
    pub sequence: u64,
    pub event_id: EventId,
    pub attempts: u32,
    pub reason: String,
    pub failed_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryRetry {
    pub plugin_id: PluginId,
    pub subscription_id: SubscriptionId,
    pub sequence: u64,
    pub event_id: EventId,
    pub attempts: u32,
    pub next_attempt_at_ms: u64,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandResult {
    pub plugin_id: PluginId,
    pub command_id: CommandId,
    pub completed_at_ms: u64,
    pub result: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorDeclaration {
    pub last_acked_sequence: u64,
    pub declaration_fingerprint: String,
}

pub trait EventStore: Send + Sync {
    fn append_batch(&self, events: &[StoredEvent]) -> Result<Vec<StoredEvent>, EventBusError>;
    fn append_batch_idempotent(
        &self,
        events: &[StoredEvent],
    ) -> Result<Vec<StoredEvent>, EventBusError> {
        self.append_batch(events)
    }
    fn load_after_topics(
        &self,
        sequence: u64,
        topics: &BTreeSet<Topic>,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, EventBusError>;
    fn high_watermark(&self) -> Result<u64, EventBusError>;
    fn bind_cursor_declaration(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
        declaration_fingerprint: &str,
    ) -> Result<CursorDeclaration, EventBusError>;
    fn load_cursor_declaration(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
    ) -> Result<Option<CursorDeclaration>, EventBusError>;
    fn load_cursor(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
    ) -> Result<u64, EventBusError> {
        Ok(self
            .load_cursor_declaration(plugin_id, subscription_id)?
            .map(|declaration| declaration.last_acked_sequence)
            .unwrap_or(0))
    }
    fn store_cursors(
        &self,
        cursors: &[(PluginId, SubscriptionId, u64)],
    ) -> Result<(), EventBusError>;
    fn put_dead_letter(&self, dead_letter: &DeadLetter) -> Result<(), EventBusError>;
    fn load_dead_letters(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
        limit: usize,
    ) -> Result<Vec<DeadLetter>, EventBusError>;
    fn load_retry(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
    ) -> Result<Option<DeliveryRetry>, EventBusError>;
    fn put_retry(&self, retry: &DeliveryRetry) -> Result<(), EventBusError>;
    fn delete_retry(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
    ) -> Result<(), EventBusError>;
    fn get_command_result(
        &self,
        plugin_id: &PluginId,
        command_id: &CommandId,
    ) -> Result<Option<CommandResult>, EventBusError>;
    fn put_command_result(&self, result: &CommandResult) -> Result<bool, EventBusError>;
}

fn subscription_declaration_fingerprint(
    delivery: DeliveryClass,
    topics: &BTreeSet<Topic>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"smelt.subscription.declaration.v1\0");
    digest.update(match delivery {
        DeliveryClass::Ephemeral => b"ephemeral".as_slice(),
        DeliveryClass::Durable => b"durable".as_slice(),
    });
    for topic in topics {
        let bytes = topic.as_str().as_bytes();
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    format!("sha256:{:x}", digest.finalize())
}

#[derive(Default)]
pub struct TopicRegistry {
    descriptors: BTreeMap<Topic, TopicDescriptor>,
}

impl TopicRegistry {
    pub fn register(&mut self, descriptor: TopicDescriptor) -> Result<(), EventBusError> {
        if descriptor.schema_version == 0 {
            return Err(EventBusError::new("topic schema version must be non-zero"));
        }
        if descriptor.max_payload_bytes == 0 {
            return Err(EventBusError::new("topic payload limit must be non-zero"));
        }
        if descriptor.topic.is_plugin_topic() && descriptor.required_publish_capability.is_none() {
            return Err(EventBusError::new(
                "plugin-owned topics require a publish capability",
            ));
        }
        if self.descriptors.contains_key(&descriptor.topic) {
            return Err(EventBusError::new(format!(
                "duplicate topic registration: {}",
                descriptor.topic
            )));
        }
        self.descriptors
            .insert(descriptor.topic.clone(), descriptor);
        Ok(())
    }

    pub fn get(&self, topic: &Topic) -> Option<&TopicDescriptor> {
        self.descriptors.get(topic)
    }

    pub fn iter(&self) -> impl Iterator<Item = &TopicDescriptor> {
        self.descriptors.values()
    }
}

pub trait SnapshotProvider: Send + Sync {
    fn snapshot(&self, kind: &SnapshotKind, watermark: u64) -> Result<Snapshot, EventBusError>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub kind: SnapshotKind,
    pub watermark: u64,
    pub revision: u64,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug)]
pub enum Delivery {
    Snapshot(Snapshot),
    Event(Arc<StoredEvent>),
    Lag {
        dropped: u64,
        resume_after: Option<u64>,
    },
}

pub struct Subscription {
    plugin_id: PluginId,
    id: SubscriptionId,
    receiver: Receiver<Delivery>,
    lagged: Arc<AtomicU64>,
    last_sequence: Arc<AtomicU64>,
}

impl Subscription {
    pub fn plugin_id(&self) -> &PluginId {
        &self.plugin_id
    }

    pub fn id(&self) -> &SubscriptionId {
        &self.id
    }

    pub fn try_recv(&self) -> Result<Delivery, TryRecvError> {
        let dropped = self.lagged.swap(0, Ordering::AcqRel);
        if dropped > 0 {
            return Ok(Delivery::Lag {
                dropped,
                resume_after: match self.last_sequence.load(Ordering::Acquire) {
                    0 => None,
                    value => Some(value),
                },
            });
        }
        self.receiver.try_recv()
    }

    pub fn recv(&self) -> Result<Delivery, mpsc::RecvError> {
        let dropped = self.lagged.swap(0, Ordering::AcqRel);
        if dropped > 0 {
            return Ok(Delivery::Lag {
                dropped,
                resume_after: match self.last_sequence.load(Ordering::Acquire) {
                    0 => None,
                    value => Some(value),
                },
            });
        }
        self.receiver.recv()
    }

    /// Drops buffered incrementals before a lag recovery snapshot is taken.
    pub fn discard_pending(&self) {
        while self.receiver.try_recv().is_ok() {}
    }
}

struct SubscriberState {
    plugin_id: PluginId,
    topics: BTreeSet<Topic>,
    delivery: DeliveryClass,
    sender: SyncSender<Delivery>,
    lagged: Arc<AtomicU64>,
    last_sequence: Arc<AtomicU64>,
    initializing: bool,
    pending: Vec<Arc<StoredEvent>>,
    ack_watermark: u64,
    scan_watermark: u64,
    inflight_limit: usize,
    inflight: BTreeMap<u64, InflightEvent>,
    retry: Option<DeliveryRetry>,
    dispatching: bool,
    dispatch_pending: bool,
}

struct InflightEvent {
    event: Arc<StoredEvent>,
    acked: bool,
}

#[derive(Clone)]
struct CausalMetadata {
    correlation_id: smelt_plugin_api::CorrelationId,
    hop_count: u16,
    envelope_fingerprint: [u8; 32],
}

struct CausalCache {
    entries: HashMap<EventId, CausalMetadata>,
    insertion_order: VecDeque<EventId>,
    capacity: usize,
}

impl CausalCache {
    fn new() -> Self {
        Self::with_capacity(DEFAULT_CAUSAL_CACHE_CAPACITY)
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity),
            insertion_order: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    #[cfg(test)]
    fn contains_key(&self, event_id: &EventId) -> bool {
        self.entries.contains_key(event_id)
    }

    fn get(&self, event_id: &EventId) -> Option<&CausalMetadata> {
        self.entries.get(event_id)
    }

    fn insert(&mut self, event_id: EventId, metadata: CausalMetadata) {
        if self.capacity == 0 {
            return;
        }
        if let Some(entry) = self.entries.get_mut(&event_id) {
            *entry = metadata;
            return;
        }
        while self.entries.len() >= self.capacity {
            let Some(oldest) = self.insertion_order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
        self.insertion_order.push_back(event_id.clone());
        self.entries.insert(event_id, metadata);
    }

    fn remove(&mut self, event_id: &EventId) {
        self.entries.remove(event_id);
        self.insertion_order.retain(|known| known != event_id);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

type RevisionReservation = ((String, String), Option<u64>, u64);

enum WriterCommand {
    Publish {
        event: Box<StoredEvent>,
        idempotent: bool,
        reply: mpsc::Sender<Result<StoredEvent, EventBusError>>,
    },
    Flush(mpsc::Sender<Result<(), EventBusError>>),
    Shutdown,
}

struct DurableWriter {
    sender: mpsc::Sender<WriterCommand>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl DurableWriter {
    fn new(store: Arc<dyn EventStore>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let writer_thread = thread::spawn(move || durable_writer_loop(store, receiver));
        Self {
            sender,
            thread: Mutex::new(Some(writer_thread)),
        }
    }

    fn publish(&self, event: StoredEvent, idempotent: bool) -> Result<StoredEvent, EventBusError> {
        let (reply, result) = mpsc::channel();
        self.sender
            .send(WriterCommand::Publish {
                event: Box::new(event),
                idempotent,
                reply,
            })
            .map_err(|_| EventBusError::new("durable writer stopped"))?;
        result
            .recv()
            .map_err(|_| EventBusError::new("durable writer stopped"))?
    }

    fn flush(&self) -> Result<(), EventBusError> {
        let (reply, result) = mpsc::channel();
        self.sender
            .send(WriterCommand::Flush(reply))
            .map_err(|_| EventBusError::new("durable writer stopped"))?;
        result
            .recv()
            .map_err(|_| EventBusError::new("durable writer stopped"))?
    }
}

impl Drop for DurableWriter {
    fn drop(&mut self) {
        let _ = self.sender.send(WriterCommand::Shutdown);
        if let Ok(thread) = self.thread.get_mut()
            && let Some(thread) = thread.take()
        {
            let _ = thread.join();
        }
    }
}

fn durable_writer_loop(store: Arc<dyn EventStore>, receiver: Receiver<WriterCommand>) {
    let mut deferred = None;
    loop {
        let command = match deferred.take().or_else(|| receiver.recv().ok()) {
            Some(command) => command,
            None => return,
        };
        match command {
            WriterCommand::Publish {
                event,
                idempotent,
                reply,
            } => {
                let started = Instant::now();
                let mut events = vec![*event];
                let mut replies = vec![reply];
                while events.len() < DEFAULT_WRITER_BATCH_SIZE {
                    let remaining = Duration::from_millis(DEFAULT_WRITER_MAX_WAIT_MS)
                        .saturating_sub(started.elapsed());
                    if remaining.is_zero() {
                        break;
                    }
                    match receiver.recv_timeout(remaining) {
                        Ok(WriterCommand::Publish {
                            event,
                            idempotent: next_idempotent,
                            reply,
                        }) if next_idempotent == idempotent => {
                            events.push(*event);
                            replies.push(reply);
                        }
                        Ok(command) => {
                            deferred = Some(command);
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                let appended = if idempotent {
                    store.append_batch_idempotent(&events)
                } else {
                    store.append_batch(&events)
                };
                match appended {
                    Ok(appended) if appended.len() == replies.len() => {
                        for (reply, event) in replies.into_iter().zip(appended) {
                            let _ = reply.send(Ok(event));
                        }
                    }
                    Ok(_) => {
                        let error =
                            EventBusError::new("EventStore returned a mismatched durable batch");
                        for reply in replies {
                            let _ = reply.send(Err(error.clone()));
                        }
                    }
                    Err(error) => {
                        for reply in replies {
                            let _ = reply.send(Err(error.clone()));
                        }
                    }
                }
            }
            WriterCommand::Flush(reply) => {
                let _ = reply.send(Ok(()));
            }
            WriterCommand::Shutdown => return,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventHubMetrics {
    pub durable_published: u64,
    pub ephemeral_published: u64,
    pub published_wire_bytes: u64,
    pub ephemeral_lagged: u64,
}

#[derive(Default)]
struct Metrics {
    durable_published: AtomicU64,
    ephemeral_published: AtomicU64,
    published_wire_bytes: AtomicU64,
    ephemeral_lagged: AtomicU64,
}

pub struct EventHub {
    registry: TopicRegistry,
    store: Option<Arc<dyn EventStore>>,
    subscribers: Mutex<BTreeMap<(PluginId, SubscriptionId), SubscriberState>>,
    aggregate_revisions: Mutex<HashMap<(String, String), u64>>,
    causal: Mutex<CausalCache>,
    snapshots: Mutex<BTreeMap<SnapshotKind, Arc<dyn SnapshotProvider>>>,
    pending_cursors: Mutex<BTreeMap<(PluginId, SubscriptionId), u64>>,
    max_hops: u16,
    ephemeral_publish: Mutex<()>,
    ephemeral_sequence: AtomicU64,
    metrics: Metrics,
    writer: Option<DurableWriter>,
}

impl EventHub {
    pub fn new(registry: TopicRegistry, store: Option<Arc<dyn EventStore>>) -> Self {
        let writer = store
            .as_ref()
            .map(|store| DurableWriter::new(store.clone()));
        Self {
            registry,
            store,
            subscribers: Mutex::new(BTreeMap::new()),
            aggregate_revisions: Mutex::new(HashMap::new()),
            causal: Mutex::new(CausalCache::new()),
            snapshots: Mutex::new(BTreeMap::new()),
            pending_cursors: Mutex::new(BTreeMap::new()),
            max_hops: DEFAULT_MAX_HOPS,
            ephemeral_publish: Mutex::new(()),
            ephemeral_sequence: AtomicU64::new(0),
            metrics: Metrics::default(),
            writer,
        }
    }

    pub fn with_max_hops(mut self, max_hops: u16) -> Self {
        self.max_hops = max_hops;
        self
    }

    pub fn register_snapshot_provider(
        &self,
        kind: SnapshotKind,
        provider: Arc<dyn SnapshotProvider>,
    ) -> Result<(), EventBusError> {
        let mut snapshots = self
            .snapshots
            .lock()
            .map_err(|_| EventBusError::new("snapshot registry lock poisoned"))?;
        if snapshots.insert(kind.clone(), provider).is_some() {
            return Err(EventBusError::new(format!(
                "duplicate snapshot provider: {}",
                kind.0
            )));
        }
        Ok(())
    }

    pub fn snapshot(
        &self,
        kind: &SnapshotKind,
        delivery: DeliveryClass,
    ) -> Result<Snapshot, EventBusError> {
        let watermark = match delivery {
            DeliveryClass::Durable => self.high_watermark()?,
            DeliveryClass::Ephemeral => self.ephemeral_sequence.load(Ordering::Acquire),
        };
        let provider = self
            .snapshots
            .lock()
            .map_err(|_| EventBusError::new("snapshot registry lock poisoned"))?
            .get(kind)
            .cloned()
            .ok_or_else(|| EventBusError::new(format!("missing snapshot provider: {}", kind.0)))?;
        provider.snapshot(kind, watermark)
    }

    pub fn recover_snapshots(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
    ) -> Result<Vec<Snapshot>, EventBusError> {
        let (delivery, kinds) = {
            let subscribers = self
                .subscribers
                .lock()
                .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?;
            let subscriber = subscribers
                .get(&(plugin_id.clone(), subscription_id.clone()))
                .ok_or_else(|| EventBusError::new("unknown subscription"))?;
            let kinds = subscriber
                .topics
                .iter()
                .filter_map(|topic| self.registry.get(topic))
                .filter_map(|descriptor| descriptor.snapshot_kind.clone())
                .collect::<BTreeSet<_>>();
            (subscriber.delivery, kinds)
        };
        kinds
            .into_iter()
            .map(|kind| self.snapshot(&kind, delivery))
            .collect()
    }

    pub fn subscribe(
        &self,
        plugin_id: PluginId,
        id: SubscriptionId,
        topics: BTreeSet<Topic>,
        delivery: DeliveryClass,
        capabilities: &BTreeSet<Capability>,
        mailbox_capacity: usize,
    ) -> Result<Subscription, EventBusError> {
        if mailbox_capacity == 0 {
            return Err(EventBusError::new("mailbox capacity must be non-zero"));
        }
        if topics.is_empty() {
            return Err(EventBusError::new("subscription must include a topic"));
        }
        let mut snapshot_kinds = BTreeSet::new();
        for topic in &topics {
            let descriptor = self
                .registry
                .get(topic)
                .ok_or_else(|| EventBusError::new(format!("unknown topic: {topic}")))?;
            if descriptor.delivery != delivery {
                return Err(EventBusError::new(format!(
                    "delivery mismatch for topic {topic}"
                )));
            }
            if !capabilities.contains(&descriptor.required_subscribe_capability) {
                return Err(EventBusError::new(format!(
                    "missing subscribe capability {}",
                    descriptor.required_subscribe_capability
                )));
            }
            if let Some(kind) = &descriptor.snapshot_kind {
                snapshot_kinds.insert(kind.clone());
            }
        }
        let (sender, receiver) = mpsc::sync_channel(mailbox_capacity);
        let lagged = Arc::new(AtomicU64::new(0));
        let last_sequence = Arc::new(AtomicU64::new(0));
        let key = (plugin_id.clone(), id.clone());
        let (ack_watermark, retry) = if delivery == DeliveryClass::Durable {
            let fingerprint = subscription_declaration_fingerprint(delivery, &topics);
            let store = self
                .store
                .as_ref()
                .ok_or_else(|| EventBusError::new("durable topic requires an EventStore"))?;
            let declaration = store.bind_cursor_declaration(&plugin_id, &id, &fingerprint)?;
            if declaration.declaration_fingerprint != fingerprint {
                return Err(EventBusError::new(
                    "EventStore returned a mismatched cursor declaration",
                ));
            }
            let retry = store.load_retry(&plugin_id, &id)?;
            let retry = match retry {
                Some(retry) if retry.sequence <= declaration.last_acked_sequence => {
                    store.delete_retry(&plugin_id, &id)?;
                    None
                }
                retry => retry,
            };
            (declaration.last_acked_sequence, retry)
        } else {
            (0, None)
        };
        let scan_watermark = retry
            .as_ref()
            .map(|retry| retry.sequence.saturating_sub(1))
            .unwrap_or(ack_watermark);
        let state = SubscriberState {
            plugin_id: plugin_id.clone(),
            topics,
            delivery,
            sender: sender.clone(),
            lagged: lagged.clone(),
            last_sequence: last_sequence.clone(),
            initializing: true,
            pending: Vec::new(),
            ack_watermark,
            scan_watermark,
            inflight_limit: if delivery == DeliveryClass::Durable {
                1
            } else {
                mailbox_capacity
            },
            inflight: BTreeMap::new(),
            retry,
            dispatching: false,
            dispatch_pending: false,
        };
        {
            let mut subscribers = self
                .subscribers
                .lock()
                .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?;
            if subscribers.contains_key(&key) {
                return Err(EventBusError::new(format!("duplicate subscription: {id}")));
            }
            subscribers.insert(key.clone(), state);
        }
        let watermark = match delivery {
            DeliveryClass::Durable => match self.high_watermark() {
                Ok(watermark) => watermark,
                Err(error) => {
                    self.subscribers
                        .lock()
                        .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?
                        .remove(&key);
                    return Err(error);
                }
            },
            DeliveryClass::Ephemeral => self.ephemeral_sequence.load(Ordering::Acquire),
        };
        last_sequence.store(watermark, Ordering::Release);
        let initialization = (|| {
            for kind in snapshot_kinds {
                let provider = self
                    .snapshots
                    .lock()
                    .map_err(|_| EventBusError::new("snapshot registry lock poisoned"))?
                    .get(&kind)
                    .cloned()
                    .ok_or_else(|| {
                        EventBusError::new(format!("missing snapshot provider: {}", kind.0))
                    })?;
                let snapshot = provider.snapshot(&kind, watermark)?;
                sender
                    .try_send(Delivery::Snapshot(snapshot))
                    .map_err(|_| EventBusError::new("mailbox too small for initial snapshots"))?;
            }
            Ok(())
        })();
        let mut subscribers = self
            .subscribers
            .lock()
            .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?;
        if let Err(error) = initialization {
            subscribers.remove(&key);
            return Err(error);
        }
        let state = subscribers
            .get_mut(&key)
            .ok_or_else(|| EventBusError::new("subscription disappeared during initialization"))?;
        state.initializing = false;
        for event in std::mem::take(&mut state.pending)
            .into_iter()
            .filter(|event| event.sequence > watermark)
        {
            match state.sender.try_send(Delivery::Event(event.clone())) {
                Ok(()) => {
                    state.last_sequence.store(event.sequence, Ordering::Release);
                }
                Err(TrySendError::Full(_)) if delivery == DeliveryClass::Ephemeral => {
                    state.lagged.fetch_add(1, Ordering::AcqRel);
                    self.metrics
                        .ephemeral_lagged
                        .fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Disconnected(_)) => break,
                Err(TrySendError::Full(_)) => {
                    return Err(EventBusError::new(
                        "durable events must be dispatched from the EventStore",
                    ));
                }
            }
        }
        let dispatch_pending = delivery == DeliveryClass::Durable && state.dispatch_pending;
        drop(subscribers);
        let subscription = Subscription {
            plugin_id,
            id,
            receiver,
            lagged,
            last_sequence,
        };
        if dispatch_pending
            && let Err(error) =
                self.dispatch_durable(&subscription.plugin_id, &subscription.id, mailbox_capacity)
        {
            let _ = self.unsubscribe(&subscription.plugin_id, &subscription.id);
            return Err(error);
        }
        Ok(subscription)
    }

    pub fn publish(
        &self,
        publisher: &PublisherIdentity,
        envelope: EventEnvelope<serde_json::Value>,
        publisher_capabilities: &BTreeSet<Capability>,
    ) -> Result<Arc<StoredEvent>, EventBusError> {
        self.publish_inner(publisher, envelope, publisher_capabilities, false)
    }

    /// Publishes a durable fact with first-write-wins semantics for its event id.
    /// Replaying the exact envelope returns the original stored event; reusing the id
    /// with different content remains an error.
    pub fn publish_idempotent(
        &self,
        publisher: &PublisherIdentity,
        envelope: EventEnvelope<serde_json::Value>,
        publisher_capabilities: &BTreeSet<Capability>,
    ) -> Result<Arc<StoredEvent>, EventBusError> {
        self.publish_inner(publisher, envelope, publisher_capabilities, true)
    }

    fn publish_inner(
        &self,
        publisher: &PublisherIdentity,
        envelope: EventEnvelope<serde_json::Value>,
        publisher_capabilities: &BTreeSet<Capability>,
        idempotent: bool,
    ) -> Result<Arc<StoredEvent>, EventBusError> {
        let descriptor = self
            .registry
            .get(&envelope.topic)
            .ok_or_else(|| EventBusError::new(format!("unknown topic: {}", envelope.topic)))?;
        if idempotent && descriptor.delivery != DeliveryClass::Durable {
            return Err(EventBusError::new(
                "idempotent publish requires a durable topic",
            ));
        }
        let _ephemeral_publish = if descriptor.delivery == DeliveryClass::Ephemeral {
            Some(
                self.ephemeral_publish
                    .lock()
                    .map_err(|_| EventBusError::new("ephemeral publish lock poisoned"))?,
            )
        } else {
            None
        };
        self.validate_publish(publisher, &envelope, descriptor, publisher_capabilities)?;
        let provisional = StoredEvent::new(0, envelope)?;
        if provisional.wire_bytes.len() > descriptor.max_payload_bytes {
            return Err(EventBusError::new(format!(
                "event exceeds payload limit for {}",
                provisional.envelope.topic
            )));
        }
        let revision_reservation = if descriptor.delivery == DeliveryClass::Ephemeral {
            self.reserve_revision(&provisional.envelope)?
        } else {
            None
        };
        let envelope_fingerprint: [u8; 32] = Sha256::digest(&provisional.wire_bytes).into();
        let causation_inserted =
            match self.reserve_causation(&provisional.envelope, envelope_fingerprint, idempotent) {
                Ok(inserted) => inserted,
                Err(error) => {
                    self.rollback_revision(revision_reservation);
                    return Err(error);
                }
            };
        let stored = match descriptor.delivery {
            DeliveryClass::Durable => {
                let writer = self
                    .writer
                    .as_ref()
                    .ok_or_else(|| EventBusError::new("durable topic requires an EventStore"))?;
                match writer.publish(provisional.clone(), idempotent) {
                    Ok(appended) => appended,
                    Err(error) => {
                        self.rollback_revision(revision_reservation);
                        self.rollback_causation(&provisional.envelope, causation_inserted);
                        return Err(error);
                    }
                }
            }
            DeliveryClass::Ephemeral => {
                let mut event = provisional;
                event.sequence = self.ephemeral_sequence.fetch_add(1, Ordering::AcqRel) + 1;
                event
            }
        };
        let stored = Arc::new(stored);
        match descriptor.delivery {
            DeliveryClass::Durable => {
                self.metrics
                    .durable_published
                    .fetch_add(1, Ordering::Relaxed);
            }
            DeliveryClass::Ephemeral => {
                self.metrics
                    .ephemeral_published
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        self.metrics
            .published_wire_bytes
            .fetch_add(stored.wire_bytes.len() as u64, Ordering::Relaxed);
        match descriptor.delivery {
            DeliveryClass::Durable => self.notify_durable_subscribers()?,
            DeliveryClass::Ephemeral => self.deliver_ephemeral(&stored)?,
        }
        Ok(stored)
    }

    /// Delivers an event already made durable by the Store outbox transaction.
    pub fn dispatch_stored(&self, event: StoredEvent) -> Result<(), EventBusError> {
        if event.sequence == 0 {
            return Err(EventBusError::new(
                "stored durable event must have a sequence",
            ));
        }
        let descriptor = self.registry.get(&event.envelope.topic).ok_or_else(|| {
            EventBusError::new(format!("unknown topic: {}", event.envelope.topic))
        })?;
        if descriptor.delivery != DeliveryClass::Durable {
            return Err(EventBusError::new(format!(
                "outbox event is not durable: {}",
                event.envelope.topic
            )));
        }
        if event.envelope.schema_version != descriptor.schema_version {
            return Err(EventBusError::new(format!(
                "unsupported schema {} for {}",
                event.envelope.schema_version, event.envelope.topic
            )));
        }
        let event = Arc::new(event);
        self.metrics
            .durable_published
            .fetch_add(1, Ordering::Relaxed);
        self.metrics
            .published_wire_bytes
            .fetch_add(event.wire_bytes.len() as u64, Ordering::Relaxed);
        self.notify_durable_subscribers()
    }

    pub fn flush_durable(&self) -> Result<(), EventBusError> {
        match &self.writer {
            Some(writer) => writer.flush(),
            None => Ok(()),
        }
    }

    pub fn dispatch_durable(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
        limit: usize,
    ) -> Result<usize, EventBusError> {
        self.dispatch_durable_at(plugin_id, subscription_id, limit, now_unix_ms())
    }

    fn dispatch_durable_at(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
        limit: usize,
        now_ms: u64,
    ) -> Result<usize, EventBusError> {
        if limit == 0 {
            return Ok(0);
        }
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| EventBusError::new("no EventStore configured"))?;
        let key = (plugin_id.clone(), subscription_id.clone());
        {
            let mut subscribers = self
                .subscribers
                .lock()
                .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?;
            let Some(subscriber) = subscribers.get_mut(&key) else {
                // A publisher may race a connection teardown after notify captured the key.
                // The subscription is already clean, so this is not a dispatch failure.
                return Ok(0);
            };
            if subscriber.delivery != DeliveryClass::Durable {
                return Err(EventBusError::new("subscription is not durable"));
            }
            subscriber.dispatch_pending = true;
            if subscriber.initializing || subscriber.dispatching {
                return Ok(0);
            }
            subscriber.dispatching = true;
        }

        let mut disconnected = false;
        let result = (|| {
            let mut delivered = 0;
            loop {
                let scan = {
                    let mut subscribers = self
                        .subscribers
                        .lock()
                        .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?;
                    let Some(subscriber) = subscribers.get_mut(&key) else {
                        return Ok(delivered);
                    };
                    subscriber.dispatch_pending = false;
                    if subscriber.inflight.is_empty()
                        && let Some(retry) = &subscriber.retry
                    {
                        if now_ms < retry.next_attempt_at_ms {
                            return Ok(delivered);
                        }
                        subscriber.scan_watermark = retry.sequence.saturating_sub(1);
                    }

                    let outstanding = subscriber.inflight.len();
                    let available = subscriber.inflight_limit.saturating_sub(outstanding);
                    let batch_limit = available.min(limit.saturating_sub(delivered)).min(
                        if subscriber.retry.is_some() {
                            1
                        } else {
                            usize::MAX
                        },
                    );
                    if batch_limit == 0 {
                        return Ok(delivered);
                    }
                    (
                        subscriber.topics.clone(),
                        subscriber.scan_watermark,
                        batch_limit,
                    )
                };
                let (topics, scan_watermark, batch_limit) = scan;
                let query_watermark = store.high_watermark()?;
                let events = store.load_after_topics(scan_watermark, &topics, batch_limit)?;
                let event_count = events.len();
                let mut subscribers = self
                    .subscribers
                    .lock()
                    .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?;
                let Some(subscriber) = subscribers.get_mut(&key) else {
                    return Ok(delivered);
                };

                if let Some(retry) = &subscriber.retry
                    && events.first().is_none_or(|event| {
                        event.sequence != retry.sequence
                            || event.envelope.event_id != retry.event_id
                    })
                {
                    return Err(EventBusError::new(
                        "persisted retry does not match the durable event log",
                    ));
                }
                let mut consumed_all = true;
                for event in events {
                    let outstanding = subscriber.inflight.len();
                    if outstanding >= subscriber.inflight_limit || delivered >= limit {
                        consumed_all = false;
                        break;
                    }
                    let sequence = event.sequence;
                    if sequence <= subscriber.scan_watermark
                        || subscriber.inflight.contains_key(&sequence)
                    {
                        continue;
                    }
                    let event = Arc::new(event);
                    match subscriber.sender.try_send(Delivery::Event(event.clone())) {
                        Ok(()) => {
                            subscriber.inflight.insert(
                                sequence,
                                InflightEvent {
                                    event,
                                    acked: false,
                                },
                            );
                            subscriber.scan_watermark = sequence;
                            subscriber.last_sequence.store(sequence, Ordering::Release);
                            delivered += 1;
                        }
                        Err(TrySendError::Full(_)) => {
                            consumed_all = false;
                            break;
                        }
                        Err(TrySendError::Disconnected(_)) => {
                            disconnected = true;
                            consumed_all = false;
                            break;
                        }
                    }
                }
                if consumed_all && event_count < batch_limit {
                    subscriber.scan_watermark = subscriber.scan_watermark.max(query_watermark);
                }
                let outstanding = subscriber.inflight.len();
                if disconnected {
                    return Ok(delivered);
                }
                if !subscriber.dispatch_pending
                    || delivered >= limit
                    || outstanding >= subscriber.inflight_limit
                {
                    return Ok(delivered);
                }
            }
        })();

        let redispatch = if let Ok(mut subscribers) = self.subscribers.lock() {
            if disconnected {
                subscribers.remove(&key);
                false
            } else if let Some(subscriber) = subscribers.get_mut(&key) {
                subscriber.dispatching = false;
                let outstanding = subscriber.inflight.len();
                result.is_ok()
                    && subscriber.dispatch_pending
                    && !subscriber.initializing
                    && outstanding < subscriber.inflight_limit
                    && result.as_ref().is_ok_and(|delivered| *delivered < limit)
            } else {
                false
            }
        } else {
            false
        };
        if redispatch {
            let delivered = result?;
            return self
                .dispatch_durable_at(
                    plugin_id,
                    subscription_id,
                    limit.saturating_sub(delivered),
                    now_ms,
                )
                .map(|additional| delivered.saturating_add(additional));
        }
        result
    }

    fn notify_durable_subscribers(&self) -> Result<(), EventBusError> {
        let subscriptions = self
            .subscribers
            .lock()
            .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?
            .iter()
            .filter(|(_, subscriber)| subscriber.delivery == DeliveryClass::Durable)
            .map(|((plugin_id, subscription_id), _)| (plugin_id.clone(), subscription_id.clone()))
            .collect::<Vec<_>>();
        for (plugin_id, subscription_id) in subscriptions {
            self.dispatch_durable(&plugin_id, &subscription_id, DEFAULT_DURABLE_LOAD_BATCH)?;
        }
        Ok(())
    }

    /// Retries durable delivery independently of new publishes or client acknowledgements.
    pub fn pump_durable_subscribers(&self) -> Result<(), EventBusError> {
        self.notify_durable_subscribers()
    }

    pub fn high_watermark(&self) -> Result<u64, EventBusError> {
        match &self.store {
            Some(store) => store.high_watermark(),
            None => Ok(self.ephemeral_sequence.load(Ordering::Acquire)),
        }
    }

    pub fn supports_durable(&self) -> bool {
        self.store.is_some() && self.writer.is_some()
    }

    pub fn command_result(
        &self,
        plugin_id: &PluginId,
        command_id: &CommandId,
    ) -> Result<Option<CommandResult>, EventBusError> {
        self.store
            .as_ref()
            .ok_or_else(|| EventBusError::new("command dedupe requires an EventStore"))?
            .get_command_result(plugin_id, command_id)
    }

    pub fn record_command_result(&self, result: &CommandResult) -> Result<bool, EventBusError> {
        self.store
            .as_ref()
            .ok_or_else(|| EventBusError::new("command dedupe requires an EventStore"))?
            .put_command_result(result)
    }

    pub fn metrics(&self) -> EventHubMetrics {
        EventHubMetrics {
            durable_published: self.metrics.durable_published.load(Ordering::Relaxed),
            ephemeral_published: self.metrics.ephemeral_published.load(Ordering::Relaxed),
            published_wire_bytes: self.metrics.published_wire_bytes.load(Ordering::Relaxed),
            ephemeral_lagged: self.metrics.ephemeral_lagged.load(Ordering::Relaxed),
        }
    }

    /// Advances only the in-memory contiguous watermark. The runtime decides when to batch-flush.
    pub fn ack(
        &self,
        plugin_id: &PluginId,
        ack: &smelt_plugin_api::Ack,
    ) -> Result<(), EventBusError> {
        let key = (plugin_id.clone(), ack.subscription_id.clone());
        let contiguous = {
            let mut subscribers = self
                .subscribers
                .lock()
                .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?;
            let subscriber = subscribers
                .get_mut(&key)
                .ok_or_else(|| EventBusError::new("unknown subscription"))?;
            if subscriber.delivery != DeliveryClass::Durable {
                return Err(EventBusError::new("cannot ack an ephemeral subscription"));
            }
            let inflight = subscriber
                .inflight
                .get(&ack.sequence)
                .ok_or_else(|| EventBusError::new("ack does not match an inflight event"))?;
            if inflight.event.envelope.event_id != ack.event_id {
                return Err(EventBusError::new("ack event_id mismatch"));
            }
            if subscriber.retry.as_ref().is_some_and(|retry| {
                retry.sequence == ack.sequence && retry.event_id == ack.event_id
            }) {
                self.store
                    .as_ref()
                    .ok_or_else(|| EventBusError::new("no EventStore configured"))?
                    .delete_retry(plugin_id, &ack.subscription_id)?;
                subscriber.retry = None;
            }
            subscriber
                .inflight
                .get_mut(&ack.sequence)
                .expect("validated inflight event")
                .acked = true;
            while subscriber
                .inflight
                .first_key_value()
                .is_some_and(|(_, event)| event.acked)
            {
                let (sequence, _) = subscriber
                    .inflight
                    .pop_first()
                    .expect("first inflight entry exists");
                subscriber.ack_watermark = sequence;
            }
            subscriber.ack_watermark
        };
        let mut cursors = self
            .pending_cursors
            .lock()
            .map_err(|_| EventBusError::new("cursor accumulator lock poisoned"))?;
        let cursor = cursors
            .entry((plugin_id.clone(), ack.subscription_id.clone()))
            .or_default();
        *cursor = (*cursor).max(contiguous);
        Ok(())
    }

    pub fn nack(
        &self,
        plugin_id: &PluginId,
        nack: &smelt_plugin_api::Nack,
        failed_at_ms: u64,
    ) -> Result<(), EventBusError> {
        let key = (plugin_id.clone(), nack.subscription_id.clone());
        let dead_letter = {
            let mut subscribers = self
                .subscribers
                .lock()
                .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?;
            let subscriber = subscribers
                .get_mut(&key)
                .ok_or_else(|| EventBusError::new("unknown subscription"))?;
            if subscriber.delivery != DeliveryClass::Durable {
                return Err(EventBusError::new("cannot nack an ephemeral subscription"));
            }
            let inflight = subscriber
                .inflight
                .get(&nack.sequence)
                .ok_or_else(|| EventBusError::new("nack does not match an inflight event"))?;
            if inflight.event.envelope.event_id != nack.event_id {
                return Err(EventBusError::new("nack event_id mismatch"));
            }
            let attempts = subscriber
                .retry
                .as_ref()
                .map(|retry| {
                    if retry.sequence != nack.sequence || retry.event_id != nack.event_id {
                        return Err(EventBusError::new(
                            "persisted retry does not match the inflight event",
                        ));
                    }
                    Ok(retry.attempts)
                })
                .transpose()?
                .unwrap_or(0)
                .saturating_add(1);
            let reason = format!("{}: {}", nack.error_code, nack.message);
            if nack.disposition == smelt_plugin_api::NackDisposition::Retryable
                && attempts < MAX_DURABLE_DELIVERY_ATTEMPTS
            {
                let retry = DeliveryRetry {
                    plugin_id: plugin_id.clone(),
                    subscription_id: nack.subscription_id.clone(),
                    sequence: nack.sequence,
                    event_id: nack.event_id.clone(),
                    attempts,
                    next_attempt_at_ms: failed_at_ms
                        .saturating_add(durable_retry_delay_ms(attempts)),
                    reason,
                };
                self.store
                    .as_ref()
                    .ok_or_else(|| EventBusError::new("no EventStore configured"))?
                    .put_retry(&retry)?;
                subscriber
                    .inflight
                    .remove(&nack.sequence)
                    .expect("validated inflight event");
                subscriber.scan_watermark = subscriber.ack_watermark;
                subscriber.retry = Some(retry);
                return Ok(());
            }
            DeadLetter {
                plugin_id: plugin_id.clone(),
                subscription_id: nack.subscription_id.clone(),
                sequence: nack.sequence,
                event_id: nack.event_id.clone(),
                attempts,
                reason,
                failed_at_ms,
            }
        };

        self.permanent_nack(dead_letter)?;
        self.ack(
            plugin_id,
            &smelt_plugin_api::Ack {
                subscription_id: nack.subscription_id.clone(),
                sequence: nack.sequence,
                event_id: nack.event_id.clone(),
            },
        )
    }

    pub fn unsubscribe(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
    ) -> Result<bool, EventBusError> {
        let removed = self
            .subscribers
            .lock()
            .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?
            .remove(&(plugin_id.clone(), subscription_id.clone()))
            .is_some();
        Ok(removed)
    }

    pub fn subscription_cursor(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
    ) -> Result<u64, EventBusError> {
        self.subscribers
            .lock()
            .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?
            .get(&(plugin_id.clone(), subscription_id.clone()))
            .map(|subscriber| subscriber.ack_watermark)
            .ok_or_else(|| EventBusError::new("unknown subscription"))
    }

    pub fn flush_cursors(&self) -> Result<usize, EventBusError> {
        let Some(store) = &self.store else {
            return Ok(0);
        };
        let batch = {
            let mut pending = self
                .pending_cursors
                .lock()
                .map_err(|_| EventBusError::new("cursor accumulator lock poisoned"))?;
            std::mem::take(&mut *pending)
                .into_iter()
                .map(|((plugin, subscription), sequence)| (plugin, subscription, sequence))
                .collect::<Vec<_>>()
        };
        if let Err(error) = store.store_cursors(&batch) {
            let mut pending = self
                .pending_cursors
                .lock()
                .map_err(|_| EventBusError::new("cursor accumulator lock poisoned"))?;
            for (plugin, subscription, sequence) in batch {
                let cursor = pending.entry((plugin, subscription)).or_default();
                *cursor = (*cursor).max(sequence);
            }
            return Err(error);
        }
        Ok(batch.len())
    }

    pub fn permanent_nack(&self, dead_letter: DeadLetter) -> Result<(), EventBusError> {
        self.validate_subscription_owner(&dead_letter.plugin_id, &dead_letter.subscription_id)?;
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| EventBusError::new("dead-letter requires an EventStore"))?;
        store.put_dead_letter(&dead_letter)
    }

    fn validate_subscription_owner(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
    ) -> Result<(), EventBusError> {
        let subscribers = self
            .subscribers
            .lock()
            .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?;
        let subscriber = subscribers
            .get(&(plugin_id.clone(), subscription_id.clone()))
            .ok_or_else(|| EventBusError::new("unknown subscription"))?;
        if &subscriber.plugin_id != plugin_id {
            return Err(EventBusError::new("subscription owner mismatch"));
        }
        Ok(())
    }

    fn validate_publish(
        &self,
        publisher: &PublisherIdentity,
        envelope: &EventEnvelope<serde_json::Value>,
        descriptor: &TopicDescriptor,
        capabilities: &BTreeSet<Capability>,
    ) -> Result<(), EventBusError> {
        if envelope.schema_version != descriptor.schema_version {
            return Err(EventBusError::new(format!(
                "unsupported schema {} for {}",
                envelope.schema_version, envelope.topic
            )));
        }
        if envelope.source != publisher.event_source() {
            return Err(EventBusError::new(
                "event source does not match authenticated publisher",
            ));
        }
        match publisher {
            PublisherIdentity::Core if envelope.topic.is_plugin_topic() => {
                return Err(EventBusError::new(
                    "core cannot publish plugin-owned topics",
                ));
            }
            PublisherIdentity::Plugin(plugin_id)
                if !envelope.topic.is_plugin_topic_for(plugin_id) =>
            {
                return Err(EventBusError::new(
                    "plugin cannot publish outside its namespace",
                ));
            }
            _ => {}
        }
        if let Some(required) = &descriptor.required_publish_capability
            && !capabilities.contains(required)
        {
            return Err(EventBusError::new(format!(
                "missing publish capability {required}"
            )));
        }
        if envelope.aggregate.is_some() != envelope.aggregate_revision.is_some() {
            return Err(EventBusError::new(
                "aggregate and aggregate_revision must appear together",
            ));
        }
        if envelope.hop_count > self.max_hops {
            return Err(EventBusError::new("event hop limit exceeded"));
        }
        Ok(())
    }

    fn reserve_revision(
        &self,
        envelope: &EventEnvelope<serde_json::Value>,
    ) -> Result<Option<RevisionReservation>, EventBusError> {
        let (Some(aggregate), Some(revision)) = (&envelope.aggregate, envelope.aggregate_revision)
        else {
            return Ok(None);
        };
        let mut revisions = self
            .aggregate_revisions
            .lock()
            .map_err(|_| EventBusError::new("aggregate revision lock poisoned"))?;
        let key = (aggregate.kind.clone(), aggregate.id.clone());
        let previous = revisions.get(&key).copied();
        if previous.is_some_and(|current| revision <= current) {
            return Err(EventBusError::new(format!(
                "aggregate revision moved backwards: {}:{}",
                aggregate.kind, aggregate.id
            )));
        }
        revisions.insert(key, revision);
        Ok(Some((
            (aggregate.kind.clone(), aggregate.id.clone()),
            previous,
            revision,
        )))
    }

    fn rollback_revision(&self, reservation: Option<RevisionReservation>) {
        let Some((key, previous, revision)) = reservation else {
            return;
        };
        if let Ok(mut revisions) = self.aggregate_revisions.lock()
            && revisions.get(&key) == Some(&revision)
        {
            match previous {
                Some(previous) => {
                    revisions.insert(key, previous);
                }
                None => {
                    revisions.remove(&key);
                }
            }
        }
    }

    fn reserve_causation(
        &self,
        envelope: &EventEnvelope<serde_json::Value>,
        envelope_fingerprint: [u8; 32],
        idempotent: bool,
    ) -> Result<bool, EventBusError> {
        let mut causal = self
            .causal
            .lock()
            .map_err(|_| EventBusError::new("causal index lock poisoned"))?;
        if let Some(existing) = causal.get(&envelope.event_id) {
            if !idempotent {
                return Err(EventBusError::new("duplicate event_id"));
            }
            if existing.envelope_fingerprint != envelope_fingerprint {
                return Err(EventBusError::new(
                    "event_id already exists with a different envelope",
                ));
            }
            return Ok(false);
        }
        if let Some(parent_id) = &envelope.causation_id
            && let Some(parent) = causal.get(parent_id)
        {
            if parent.correlation_id != envelope.correlation_id {
                return Err(EventBusError::new("causal chain changed correlation_id"));
            }
            if envelope.hop_count != parent.hop_count.saturating_add(1) {
                return Err(EventBusError::new("causal hop_count is not parent + 1"));
            }
        }
        causal.insert(
            envelope.event_id.clone(),
            CausalMetadata {
                correlation_id: envelope.correlation_id.clone(),
                hop_count: envelope.hop_count,
                envelope_fingerprint,
            },
        );
        Ok(true)
    }

    fn rollback_causation(&self, envelope: &EventEnvelope<serde_json::Value>, inserted: bool) {
        if inserted && let Ok(mut causal) = self.causal.lock() {
            causal.remove(&envelope.event_id);
        }
    }

    fn deliver_ephemeral(&self, event: &Arc<StoredEvent>) -> Result<(), EventBusError> {
        let mut subscribers = self
            .subscribers
            .lock()
            .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?;
        for subscriber in subscribers.values_mut() {
            if subscriber.delivery != DeliveryClass::Ephemeral
                || !subscriber.topics.contains(&event.envelope.topic)
            {
                continue;
            }
            if subscriber.initializing {
                subscriber.pending.push(event.clone());
                continue;
            }
            match subscriber.sender.try_send(Delivery::Event(event.clone())) {
                Ok(()) => {
                    subscriber
                        .last_sequence
                        .store(event.sequence, Ordering::Release);
                }
                Err(TrySendError::Full(_)) => {
                    subscriber.lagged.fetch_add(1, Ordering::AcqRel);
                    self.metrics
                        .ephemeral_lagged
                        .fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Disconnected(_)) => {}
            }
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct MemoryEventStore {
    state: Mutex<MemoryState>,
}

#[derive(Default)]
struct MemoryState {
    events: Vec<StoredEvent>,
    cursors: BTreeMap<(PluginId, SubscriptionId), CursorDeclaration>,
    retries: BTreeMap<(PluginId, SubscriptionId), DeliveryRetry>,
    dead_letters: Vec<DeadLetter>,
    commands: BTreeMap<(PluginId, CommandId), CommandResult>,
}

impl MemoryEventStore {
    fn append_batch_mode(
        &self,
        events: &[StoredEvent],
        idempotent: bool,
    ) -> Result<Vec<StoredEvent>, EventBusError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| EventBusError::new("memory store lock poisoned"))?;
        let mut revisions = state
            .events
            .iter()
            .filter_map(|event| {
                Some((
                    {
                        let aggregate = event.envelope.aggregate.as_ref()?;
                        (aggregate.kind.clone(), aggregate.id.clone())
                    },
                    event.envelope.aggregate_revision?,
                ))
            })
            .collect::<HashMap<_, _>>();
        let mut known_events = state
            .events
            .iter()
            .map(|event| (event.envelope.event_id.clone(), event.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut new_events = Vec::new();
        let mut appended = Vec::with_capacity(events.len());
        for event in events {
            if let Some(existing) = known_events.get(&event.envelope.event_id) {
                if !idempotent {
                    return Err(EventBusError::new("duplicate event_id"));
                }
                if existing.envelope != event.envelope {
                    return Err(EventBusError::new(
                        "event_id already exists with a different envelope",
                    ));
                }
                appended.push(existing.clone());
                continue;
            }
            if let (Some(aggregate), Some(revision)) = (
                event.envelope.aggregate.as_ref(),
                event.envelope.aggregate_revision,
            ) {
                let key = (aggregate.kind.clone(), aggregate.id.clone());
                if revisions
                    .get(&key)
                    .is_some_and(|current| revision <= *current)
                {
                    return Err(EventBusError::new(format!(
                        "aggregate revision moved backwards: {}:{}",
                        aggregate.kind, aggregate.id
                    )));
                }
                revisions.insert(key, revision);
            }
            let mut stored = event.clone();
            stored.sequence = state.events.len() as u64 + new_events.len() as u64 + 1;
            known_events.insert(stored.envelope.event_id.clone(), stored.clone());
            new_events.push(stored.clone());
            appended.push(stored);
        }
        state.events.extend(new_events);
        Ok(appended)
    }
}

impl EventStore for MemoryEventStore {
    fn append_batch(&self, events: &[StoredEvent]) -> Result<Vec<StoredEvent>, EventBusError> {
        self.append_batch_mode(events, false)
    }

    fn append_batch_idempotent(
        &self,
        events: &[StoredEvent],
    ) -> Result<Vec<StoredEvent>, EventBusError> {
        self.append_batch_mode(events, true)
    }

    fn load_after_topics(
        &self,
        sequence: u64,
        topics: &BTreeSet<Topic>,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, EventBusError> {
        let state = self
            .state
            .lock()
            .map_err(|_| EventBusError::new("memory store lock poisoned"))?;
        Ok(state
            .events
            .iter()
            .filter(|event| event.sequence > sequence && topics.contains(&event.envelope.topic))
            .take(limit)
            .cloned()
            .collect())
    }

    fn high_watermark(&self) -> Result<u64, EventBusError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| EventBusError::new("memory store lock poisoned"))?
            .events
            .len() as u64)
    }

    fn bind_cursor_declaration(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
        declaration_fingerprint: &str,
    ) -> Result<CursorDeclaration, EventBusError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| EventBusError::new("memory store lock poisoned"))?;
        let declaration = state
            .cursors
            .entry((plugin_id.clone(), subscription_id.clone()))
            .or_insert_with(|| CursorDeclaration {
                last_acked_sequence: 0,
                declaration_fingerprint: declaration_fingerprint.to_string(),
            });
        if declaration.declaration_fingerprint != declaration_fingerprint {
            return Err(EventBusError::new(
                "durable subscription declaration conflicts with its stored cursor",
            ));
        }
        Ok(declaration.clone())
    }

    fn load_cursor_declaration(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
    ) -> Result<Option<CursorDeclaration>, EventBusError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| EventBusError::new("memory store lock poisoned"))?
            .cursors
            .get(&(plugin_id.clone(), subscription_id.clone()))
            .cloned())
    }

    fn store_cursors(
        &self,
        cursors: &[(PluginId, SubscriptionId, u64)],
    ) -> Result<(), EventBusError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| EventBusError::new("memory store lock poisoned"))?;
        for (plugin, subscription, sequence) in cursors {
            let cursor = state
                .cursors
                .get_mut(&(plugin.clone(), subscription.clone()))
                .ok_or_else(|| {
                    EventBusError::new("cursor declaration must be bound before it is stored")
                })?;
            cursor.last_acked_sequence = cursor.last_acked_sequence.max(*sequence);
        }
        Ok(())
    }

    fn put_dead_letter(&self, dead_letter: &DeadLetter) -> Result<(), EventBusError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| EventBusError::new("memory store lock poisoned"))?;
        let key = (
            dead_letter.plugin_id.clone(),
            dead_letter.subscription_id.clone(),
        );
        let cursor = state.cursors.get_mut(&key).ok_or_else(|| {
            EventBusError::new("cursor declaration must be bound before dead-lettering")
        })?;
        cursor.last_acked_sequence = cursor.last_acked_sequence.max(dead_letter.sequence);
        state.retries.remove(&key);
        if let Some(existing) = state.dead_letters.iter_mut().find(|existing| {
            existing.plugin_id == dead_letter.plugin_id
                && existing.subscription_id == dead_letter.subscription_id
                && existing.sequence == dead_letter.sequence
        }) {
            *existing = dead_letter.clone();
        } else {
            state.dead_letters.push(dead_letter.clone());
        }
        Ok(())
    }

    fn load_dead_letters(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
        limit: usize,
    ) -> Result<Vec<DeadLetter>, EventBusError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| EventBusError::new("memory store lock poisoned"))?
            .dead_letters
            .iter()
            .filter(|dead| &dead.plugin_id == plugin_id && &dead.subscription_id == subscription_id)
            .take(limit)
            .cloned()
            .collect())
    }

    fn load_retry(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
    ) -> Result<Option<DeliveryRetry>, EventBusError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| EventBusError::new("memory store lock poisoned"))?
            .retries
            .get(&(plugin_id.clone(), subscription_id.clone()))
            .cloned())
    }

    fn put_retry(&self, retry: &DeliveryRetry) -> Result<(), EventBusError> {
        self.state
            .lock()
            .map_err(|_| EventBusError::new("memory store lock poisoned"))?
            .retries
            .insert(
                (retry.plugin_id.clone(), retry.subscription_id.clone()),
                retry.clone(),
            );
        Ok(())
    }

    fn delete_retry(
        &self,
        plugin_id: &PluginId,
        subscription_id: &SubscriptionId,
    ) -> Result<(), EventBusError> {
        self.state
            .lock()
            .map_err(|_| EventBusError::new("memory store lock poisoned"))?
            .retries
            .remove(&(plugin_id.clone(), subscription_id.clone()));
        Ok(())
    }

    fn get_command_result(
        &self,
        plugin_id: &PluginId,
        command_id: &CommandId,
    ) -> Result<Option<CommandResult>, EventBusError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| EventBusError::new("memory store lock poisoned"))?
            .commands
            .get(&(plugin_id.clone(), command_id.clone()))
            .cloned())
    }

    fn put_command_result(&self, result: &CommandResult) -> Result<bool, EventBusError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| EventBusError::new("memory store lock poisoned"))?;
        let key = (result.plugin_id.clone(), result.command_id.clone());
        if state.commands.contains_key(&key) {
            return Ok(false);
        }
        state.commands.insert(key, result.clone());
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use smelt_plugin_api::{AggregateRef, CorrelationId, DataSensitivity, EventSource};
    use std::sync::Barrier;

    fn capability(value: &str) -> Capability {
        Capability::new(value).unwrap()
    }

    fn topic(value: &str) -> Topic {
        Topic::new(value).unwrap()
    }

    fn registry(delivery: DeliveryClass, snapshot: Option<&str>) -> TopicRegistry {
        let mut registry = TopicRegistry::default();
        registry
            .register(TopicDescriptor {
                topic: topic("example.changed"),
                schema_version: 1,
                delivery,
                required_subscribe_capability: capability("session.read"),
                required_publish_capability: None,
                sensitivity: DataSensitivity::Internal,
                max_payload_bytes: 4096,
                snapshot_kind: snapshot.map(|value| SnapshotKind(value.to_string())),
            })
            .unwrap();
        registry
    }

    fn envelope(revision: u64) -> EventEnvelope<serde_json::Value> {
        EventEnvelope {
            event_id: EventId::new(format!("event-{revision}")).unwrap(),
            topic: topic("example.changed"),
            schema_version: 1,
            occurred_at_ms: revision,
            aggregate: Some(AggregateRef::new("example", "item-1").unwrap()),
            aggregate_revision: Some(revision),
            source: EventSource::Core,
            correlation_id: CorrelationId::new("chain-1").unwrap(),
            causation_id: None,
            hop_count: 0,
            payload: json!({"revision": revision}),
        }
    }

    fn unversioned_envelope(id: usize) -> EventEnvelope<serde_json::Value> {
        let mut event = envelope(id as u64);
        event.event_id = EventId::new(format!("unversioned-{id}")).unwrap();
        event.aggregate = None;
        event.aggregate_revision = None;
        event
    }

    fn aggregate_envelope(id: usize, revision: u64) -> EventEnvelope<serde_json::Value> {
        let mut event = envelope(revision);
        event.event_id = EventId::new(format!("event-{id}-{revision}")).unwrap();
        event.aggregate = Some(AggregateRef::new("example", format!("item-{id}")).unwrap());
        event.payload = json!({"revision": revision});
        event
    }

    #[test]
    fn registry_rejects_duplicates() {
        let mut registry = registry(DeliveryClass::Ephemeral, None);
        let descriptor = registry.get(&topic("example.changed")).unwrap().clone();
        assert!(registry.register(descriptor).is_err());
    }

    #[test]
    fn filters_capabilities_and_topics() {
        let hub = EventHub::new(registry(DeliveryClass::Ephemeral, None), None);
        assert!(
            hub.subscribe(
                PluginId::new("com.example").unwrap(),
                SubscriptionId::new("sessions").unwrap(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Ephemeral,
                &BTreeSet::new(),
                1,
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_revision_regression() {
        let hub = EventHub::new(registry(DeliveryClass::Ephemeral, None), None);
        hub.publish(&PublisherIdentity::Core, envelope(2), &BTreeSet::new())
            .unwrap();
        assert!(
            hub.publish(&PublisherIdentity::Core, envelope(1), &BTreeSet::new())
                .is_err()
        );
    }

    #[test]
    fn concurrent_ephemeral_revisions_never_publish_or_deliver_in_reverse_order() {
        const ITERATIONS: usize = 128;

        let hub = Arc::new(EventHub::new(
            registry(DeliveryClass::Ephemeral, None),
            None,
        ));
        let subscription = hub
            .subscribe(
                PluginId::new("com.example").unwrap(),
                SubscriptionId::new("sessions").unwrap(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Ephemeral,
                &BTreeSet::from([capability("session.read")]),
                ITERATIONS * 2,
            )
            .unwrap();
        let mut delivered_count = 0;
        let mut paired_publishes = 0;

        for id in 0..ITERATIONS {
            let barrier = Arc::new(Barrier::new(3));
            let threads = [1, 2].map(|revision| {
                let hub = hub.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    hub.publish(
                        &PublisherIdentity::Core,
                        aggregate_envelope(id, revision),
                        &BTreeSet::new(),
                    )
                })
            });
            barrier.wait();
            let [first, second] = threads.map(|thread| thread.join().unwrap());
            delivered_count += usize::from(first.is_ok()) + usize::from(second.is_ok());
            if let (Ok(first), Ok(second)) = (first, second) {
                paired_publishes += 1;
                assert!(
                    first.sequence < second.sequence,
                    "revision 2 was sequenced before revision 1 for aggregate {id}"
                );
            }
        }
        assert!(
            paired_publishes > 0,
            "the concurrency regression never published both revisions"
        );

        let mut revisions_by_aggregate = BTreeMap::<String, Vec<u64>>::new();
        for _ in 0..delivered_count {
            let Delivery::Event(event) = subscription.recv().unwrap() else {
                panic!("expected event");
            };
            revisions_by_aggregate
                .entry(event.envelope.aggregate.as_ref().unwrap().id.clone())
                .or_default()
                .push(event.envelope.aggregate_revision.unwrap());
        }
        for revisions in revisions_by_aggregate.values() {
            assert!(
                revisions.windows(2).all(|pair| pair[0] < pair[1]),
                "ephemeral mailbox delivered aggregate revisions out of order: {revisions:?}"
            );
        }
    }

    #[test]
    fn poisoned_ephemeral_publish_lock_is_reported_as_an_event_bus_error() {
        let hub = Arc::new(EventHub::new(
            registry(DeliveryClass::Ephemeral, None),
            None,
        ));
        let poisoner = hub.clone();
        assert!(
            thread::spawn(move || {
                let _guard = poisoner.ephemeral_publish.lock().unwrap();
                panic!("poison ephemeral publish lock");
            })
            .join()
            .is_err()
        );

        assert!(
            hub.publish(&PublisherIdentity::Core, envelope(1), &BTreeSet::new())
                .unwrap_err()
                .to_string()
                .contains("ephemeral publish lock poisoned")
        );
    }

    #[test]
    fn ephemeral_mailbox_reports_lag() {
        let hub = EventHub::new(registry(DeliveryClass::Ephemeral, None), None);
        let subscription = hub
            .subscribe(
                PluginId::new("com.example").unwrap(),
                SubscriptionId::new("sessions").unwrap(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Ephemeral,
                &BTreeSet::from([capability("session.read")]),
                1,
            )
            .unwrap();
        hub.publish(&PublisherIdentity::Core, envelope(1), &BTreeSet::new())
            .unwrap();
        hub.publish(&PublisherIdentity::Core, envelope(2), &BTreeSet::new())
            .unwrap();
        assert!(matches!(
            subscription.try_recv().unwrap(),
            Delivery::Lag { dropped: 1, .. }
        ));
        assert!(matches!(
            subscription.try_recv().unwrap(),
            Delivery::Event(_)
        ));
    }

    #[test]
    fn durable_events_are_recoverable_from_cursor() {
        let store = Arc::new(MemoryEventStore::default());
        let hub = EventHub::new(registry(DeliveryClass::Durable, None), Some(store.clone()));
        let subscription_id = SubscriptionId::new("sessions").unwrap();
        let subscription = hub
            .subscribe(
                PluginId::new("com.example").unwrap(),
                subscription_id.clone(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Durable,
                &BTreeSet::from([capability("session.read")]),
                1,
            )
            .unwrap();
        hub.publish(&PublisherIdentity::Core, envelope(1), &BTreeSet::new())
            .unwrap();
        let delivered = subscription.try_recv().unwrap();
        let Delivery::Event(event) = delivered else {
            panic!("expected event");
        };

        assert_eq!(event.sequence, 1);
        let plugin = PluginId::new("com.example").unwrap();
        hub.ack(
            &plugin,
            &smelt_plugin_api::Ack {
                subscription_id: subscription_id.clone(),
                sequence: 1,
                event_id: event.envelope.event_id.clone(),
            },
        )
        .unwrap();
        assert_eq!(store.load_cursor(&plugin, &subscription_id).unwrap(), 0);
        assert_eq!(hub.flush_cursors().unwrap(), 1);
        assert_eq!(store.load_cursor(&plugin, &subscription_id).unwrap(), 1);
        hub.permanent_nack(DeadLetter {
            plugin_id: plugin.clone(),
            subscription_id: subscription_id.clone(),
            sequence: 1,
            event_id: event.envelope.event_id.clone(),
            attempts: 5,
            reason: "invalid payload".into(),
            failed_at_ms: 10,
        })
        .unwrap();
        assert_eq!(
            store
                .load_dead_letters(&plugin, &subscription_id, 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn durable_subscription_id_rejects_reconnect_with_changed_topics() {
        let store = Arc::new(MemoryEventStore::default());
        let mut topics = registry(DeliveryClass::Durable, None);
        topics
            .register(TopicDescriptor {
                topic: topic("example.archived"),
                schema_version: 1,
                delivery: DeliveryClass::Durable,
                required_subscribe_capability: capability("session.read"),
                required_publish_capability: None,
                sensitivity: smelt_plugin_api::DataSensitivity::Internal,
                max_payload_bytes: 4096,
                snapshot_kind: None,
            })
            .unwrap();
        let hub = EventHub::new(topics, Some(store));
        let plugin = PluginId::new("com.example").unwrap();
        let subscription_id = SubscriptionId::new("sessions").unwrap();
        hub.subscribe(
            plugin.clone(),
            subscription_id.clone(),
            BTreeSet::from([topic("example.changed")]),
            DeliveryClass::Durable,
            &BTreeSet::from([capability("session.read")]),
            4,
        )
        .unwrap();
        hub.unsubscribe(&plugin, &subscription_id).unwrap();

        let error = match hub.subscribe(
            plugin,
            subscription_id,
            BTreeSet::from([topic("example.archived")]),
            DeliveryClass::Durable,
            &BTreeSet::from([capability("session.read")]),
            4,
        ) {
            Ok(_) => panic!("changed durable declaration must be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("declaration conflicts"));
    }

    #[test]
    fn durable_live_publish_waits_behind_historical_backlog() {
        let store = Arc::new(MemoryEventStore::default());
        store
            .append_batch(&[
                StoredEvent::new(0, unversioned_envelope(1)).unwrap(),
                StoredEvent::new(0, unversioned_envelope(2)).unwrap(),
            ])
            .unwrap();
        let hub = EventHub::new(registry(DeliveryClass::Durable, None), Some(store));
        let plugin = PluginId::new("com.example").unwrap();
        let subscription_id = SubscriptionId::new("sessions").unwrap();
        let subscription = hub
            .subscribe(
                plugin.clone(),
                subscription_id.clone(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Durable,
                &BTreeSet::from([capability("session.read")]),
                1,
            )
            .unwrap();

        hub.publish(
            &PublisherIdentity::Core,
            unversioned_envelope(3),
            &BTreeSet::new(),
        )
        .unwrap();

        let first = match subscription.recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected event"),
        };
        assert_eq!(first.sequence, 1);
        hub.ack(
            &plugin,
            &smelt_plugin_api::Ack {
                subscription_id: subscription_id.clone(),
                sequence: first.sequence,
                event_id: first.envelope.event_id.clone(),
            },
        )
        .unwrap();
        assert_eq!(
            hub.dispatch_durable(&plugin, &subscription_id, 10).unwrap(),
            1
        );
        let second = match subscription.recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected event"),
        };
        assert_eq!(second.sequence, 2);
        hub.ack(
            &plugin,
            &smelt_plugin_api::Ack {
                subscription_id: subscription_id.clone(),
                sequence: second.sequence,
                event_id: second.envelope.event_id.clone(),
            },
        )
        .unwrap();
        assert_eq!(
            hub.dispatch_durable(&plugin, &subscription_id, 10).unwrap(),
            1
        );
        let live = match subscription.recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected event"),
        };
        assert_eq!(live.sequence, 3);
    }

    #[test]
    fn durable_topic_order_does_not_skip_an_unloaded_match() {
        let store = Arc::new(MemoryEventStore::default());
        let mut unrelated = unversioned_envelope(2);
        unrelated.topic = topic("other.changed");
        store
            .append_batch(&[
                StoredEvent::new(0, unversioned_envelope(1)).unwrap(),
                StoredEvent::new(0, unrelated).unwrap(),
                StoredEvent::new(0, unversioned_envelope(3)).unwrap(),
            ])
            .unwrap();
        let hub = EventHub::new(registry(DeliveryClass::Durable, None), Some(store.clone()));
        let plugin = PluginId::new("com.example").unwrap();
        let subscription_id = SubscriptionId::new("sessions").unwrap();
        let subscription = hub
            .subscribe(
                plugin.clone(),
                subscription_id.clone(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Durable,
                &BTreeSet::from([capability("session.read")]),
                1,
            )
            .unwrap();
        hub.publish(
            &PublisherIdentity::Core,
            unversioned_envelope(4),
            &BTreeSet::new(),
        )
        .unwrap();

        let first = match subscription.recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected event"),
        };
        assert_eq!(first.sequence, 1);
        hub.ack(
            &plugin,
            &smelt_plugin_api::Ack {
                subscription_id: subscription_id.clone(),
                sequence: first.sequence,
                event_id: first.envelope.event_id.clone(),
            },
        )
        .unwrap();
        hub.dispatch_durable(&plugin, &subscription_id, 10).unwrap();
        let second = match subscription.recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected event"),
        };
        assert_eq!(second.sequence, 3);
        hub.ack(
            &plugin,
            &smelt_plugin_api::Ack {
                subscription_id: subscription_id.clone(),
                sequence: second.sequence,
                event_id: second.envelope.event_id.clone(),
            },
        )
        .unwrap();
        hub.flush_cursors().unwrap();
        assert_eq!(store.load_cursor(&plugin, &subscription_id).unwrap(), 3);
        assert_eq!(
            hub.dispatch_durable(&plugin, &subscription_id, 10).unwrap(),
            1
        );
        let live = match subscription.recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected event"),
        };
        assert_eq!(live.sequence, 4);
    }

    #[test]
    fn durable_capability_fails_closed_without_store() {
        let hub = EventHub::new(registry(DeliveryClass::Durable, None), None);
        assert!(!hub.supports_durable());
        assert!(
            hub.subscribe(
                PluginId::new("com.example").unwrap(),
                SubscriptionId::new("sessions").unwrap(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Durable,
                &BTreeSet::from([capability("session.read")]),
                1,
            )
            .is_err()
        );
        assert!(
            hub.publish(&PublisherIdentity::Core, envelope(1), &BTreeSet::new())
                .is_err()
        );
    }

    #[test]
    fn plugin_cannot_publish_core_topic() {
        let hub = EventHub::new(registry(DeliveryClass::Ephemeral, None), None);
        let mut event = envelope(1);
        event.source = EventSource::Plugin {
            plugin_id: PluginId::new("com.example").unwrap(),
        };
        assert!(
            hub.publish(
                &PublisherIdentity::Plugin(PluginId::new("com.example").unwrap()),
                event,
                &BTreeSet::new()
            )
            .is_err()
        );
    }

    #[test]
    fn authenticated_publisher_cannot_spoof_envelope_source() {
        let hub = EventHub::new(registry(DeliveryClass::Ephemeral, None), None);
        let mut event = envelope(1);
        event.source = EventSource::Plugin {
            plugin_id: PluginId::new("com.example").unwrap(),
        };
        assert!(
            hub.publish(&PublisherIdentity::Core, event, &BTreeSet::new())
                .is_err()
        );
    }

    #[test]
    fn plugin_publish_requires_namespace_and_capability() {
        let plugin = PluginId::new("com.example").unwrap();
        let plugin_topic = topic("plugin.com.example.changed");
        let publish_capability = capability("example.write");
        let mut registry = TopicRegistry::default();
        registry
            .register(TopicDescriptor {
                topic: plugin_topic.clone(),
                schema_version: 1,
                delivery: DeliveryClass::Ephemeral,
                required_subscribe_capability: capability("session.read"),
                required_publish_capability: Some(publish_capability.clone()),
                sensitivity: DataSensitivity::Internal,
                max_payload_bytes: 4096,
                snapshot_kind: None,
            })
            .unwrap();
        let hub = EventHub::new(registry, None);
        let mut event = unversioned_envelope(1);
        event.topic = plugin_topic;
        event.source = EventSource::Plugin {
            plugin_id: plugin.clone(),
        };
        assert!(
            hub.publish(
                &PublisherIdentity::Plugin(plugin.clone()),
                event.clone(),
                &BTreeSet::new(),
            )
            .is_err()
        );
        hub.publish(
            &PublisherIdentity::Plugin(plugin),
            event,
            &BTreeSet::from([publish_capability]),
        )
        .unwrap();
    }

    #[test]
    fn subscription_ids_are_scoped_by_plugin() {
        let hub = EventHub::new(registry(DeliveryClass::Ephemeral, None), None);
        for plugin in ["com.first", "com.second"] {
            hub.subscribe(
                PluginId::new(plugin).unwrap(),
                SubscriptionId::new("sessions").unwrap(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Ephemeral,
                &BTreeSet::from([capability("session.read")]),
                1,
            )
            .unwrap();
        }
    }

    #[test]
    fn ack_validates_identity_and_advances_serial_durable_delivery() {
        let store = Arc::new(MemoryEventStore::default());
        let hub = EventHub::new(registry(DeliveryClass::Durable, None), Some(store.clone()));
        let plugin = PluginId::new("com.example").unwrap();
        let subscription_id = SubscriptionId::new("sessions").unwrap();
        let subscription = hub
            .subscribe(
                plugin.clone(),
                subscription_id.clone(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Durable,
                &BTreeSet::from([capability("session.read")]),
                2,
            )
            .unwrap();
        let first_published = hub
            .publish(&PublisherIdentity::Core, envelope(1), &BTreeSet::new())
            .unwrap();
        let second_published = hub
            .publish(&PublisherIdentity::Core, envelope(2), &BTreeSet::new())
            .unwrap();
        let first = match subscription.recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected event"),
        };
        assert_eq!(first.sequence, first_published.sequence);
        assert!(matches!(subscription.try_recv(), Err(TryRecvError::Empty)));
        assert!(
            hub.ack(
                &plugin,
                &smelt_plugin_api::Ack {
                    subscription_id: subscription_id.clone(),
                    sequence: first.sequence,
                    event_id: second_published.envelope.event_id.clone(),
                },
            )
            .is_err()
        );
        hub.ack(
            &plugin,
            &smelt_plugin_api::Ack {
                subscription_id: subscription_id.clone(),
                sequence: first.sequence,
                event_id: first.envelope.event_id.clone(),
            },
        )
        .unwrap();
        hub.flush_cursors().unwrap();
        assert_eq!(
            store.load_cursor(&plugin, &subscription_id).unwrap(),
            first.sequence
        );
        assert_eq!(
            hub.dispatch_durable(&plugin, &subscription_id, 10).unwrap(),
            1
        );
        let second = match subscription.recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected event"),
        };
        assert_eq!(second.sequence, second_published.sequence);
        hub.ack(
            &plugin,
            &smelt_plugin_api::Ack {
                subscription_id: subscription_id.clone(),
                sequence: second.sequence,
                event_id: second.envelope.event_id.clone(),
            },
        )
        .unwrap();
        hub.flush_cursors().unwrap();
        assert_eq!(
            store.load_cursor(&plugin, &subscription_id).unwrap(),
            second.sequence
        );
    }

    #[test]
    fn durable_inflight_is_one_even_after_mailbox_is_drained() {
        let store = Arc::new(MemoryEventStore::default());
        store
            .append_batch(
                &(0..5)
                    .map(|id| StoredEvent::new(0, unversioned_envelope(id)).unwrap())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let hub = EventHub::new(registry(DeliveryClass::Durable, None), Some(store));
        let plugin = PluginId::new("com.example").unwrap();
        let subscription_id = SubscriptionId::new("sessions").unwrap();
        let subscription = hub
            .subscribe(
                plugin.clone(),
                subscription_id.clone(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Durable,
                &BTreeSet::from([capability("session.read")]),
                2,
            )
            .unwrap();

        assert_eq!(
            hub.dispatch_durable(&plugin, &subscription_id, 10).unwrap(),
            1
        );
        let first = match subscription.recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected event"),
        };
        assert_eq!(
            hub.dispatch_durable(&plugin, &subscription_id, 10).unwrap(),
            0
        );

        hub.ack(
            &plugin,
            &smelt_plugin_api::Ack {
                subscription_id: subscription_id.clone(),
                sequence: first.sequence,
                event_id: first.envelope.event_id.clone(),
            },
        )
        .unwrap();
        assert_eq!(
            hub.dispatch_durable(&plugin, &subscription_id, 10).unwrap(),
            1
        );
        let third = match subscription.recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected event"),
        };
        assert_eq!(third.sequence, 2);
    }

    #[test]
    fn durable_revision_is_rejected_by_store_after_hub_restart() {
        let store = Arc::new(MemoryEventStore::default());
        EventHub::new(registry(DeliveryClass::Durable, None), Some(store.clone()))
            .publish(&PublisherIdentity::Core, envelope(2), &BTreeSet::new())
            .unwrap();
        assert!(
            EventHub::new(registry(DeliveryClass::Durable, None), Some(store))
                .publish(&PublisherIdentity::Core, envelope(1), &BTreeSet::new())
                .is_err()
        );
    }

    #[test]
    fn only_idempotent_publish_can_reuse_an_identical_durable_event() {
        let store = Arc::new(MemoryEventStore::default());
        let original = unversioned_envelope(1);
        EventHub::new(registry(DeliveryClass::Durable, None), Some(store.clone()))
            .publish(&PublisherIdentity::Core, original.clone(), &BTreeSet::new())
            .unwrap();

        let replacement =
            EventHub::new(registry(DeliveryClass::Durable, None), Some(store.clone()));
        assert!(
            replacement
                .publish(&PublisherIdentity::Core, original.clone(), &BTreeSet::new(),)
                .is_err(),
            "strict publish must reject an event id already present in the store"
        );
        let replayed = replacement
            .publish_idempotent(&PublisherIdentity::Core, original.clone(), &BTreeSet::new())
            .unwrap();
        assert_eq!(replayed.sequence, 1);
        assert_eq!(store.high_watermark().unwrap(), 1);

        let mut changed = original;
        changed.payload = json!({"changed": true});
        assert!(
            replacement
                .publish_idempotent(&PublisherIdentity::Core, changed, &BTreeSet::new())
                .is_err(),
            "an event id cannot be rebound to different content"
        );
    }

    #[test]
    fn topic_filtered_load_is_not_blocked_by_unrelated_events() {
        let store = MemoryEventStore::default();
        let mut events = (0..DEFAULT_DURABLE_LOAD_BATCH + 1)
            .map(|id| {
                let mut event = unversioned_envelope(id);
                event.topic = topic("other.changed");
                StoredEvent::new(0, event).unwrap()
            })
            .collect::<Vec<_>>();
        events.push(StoredEvent::new(0, unversioned_envelope(999)).unwrap());
        store.append_batch(&events).unwrap();
        let loaded = store
            .load_after_topics(0, &BTreeSet::from([topic("example.changed")]), 1)
            .unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].envelope.event_id.as_str(), "unversioned-999");
    }

    struct CountingStore {
        inner: MemoryEventStore,
        append_calls: AtomicU64,
    }

    impl CountingStore {
        fn new() -> Self {
            Self {
                inner: MemoryEventStore::default(),
                append_calls: AtomicU64::new(0),
            }
        }
    }

    impl EventStore for CountingStore {
        fn append_batch(&self, events: &[StoredEvent]) -> Result<Vec<StoredEvent>, EventBusError> {
            self.append_calls.fetch_add(1, Ordering::Relaxed);
            self.inner.append_batch(events)
        }

        fn load_after_topics(
            &self,
            sequence: u64,
            topics: &BTreeSet<Topic>,
            limit: usize,
        ) -> Result<Vec<StoredEvent>, EventBusError> {
            self.inner.load_after_topics(sequence, topics, limit)
        }

        fn high_watermark(&self) -> Result<u64, EventBusError> {
            self.inner.high_watermark()
        }

        fn bind_cursor_declaration(
            &self,
            plugin_id: &PluginId,
            subscription_id: &SubscriptionId,
            declaration_fingerprint: &str,
        ) -> Result<CursorDeclaration, EventBusError> {
            self.inner
                .bind_cursor_declaration(plugin_id, subscription_id, declaration_fingerprint)
        }

        fn load_cursor_declaration(
            &self,
            plugin_id: &PluginId,
            subscription_id: &SubscriptionId,
        ) -> Result<Option<CursorDeclaration>, EventBusError> {
            self.inner
                .load_cursor_declaration(plugin_id, subscription_id)
        }

        fn store_cursors(
            &self,
            cursors: &[(PluginId, SubscriptionId, u64)],
        ) -> Result<(), EventBusError> {
            self.inner.store_cursors(cursors)
        }

        fn put_dead_letter(&self, dead_letter: &DeadLetter) -> Result<(), EventBusError> {
            self.inner.put_dead_letter(dead_letter)
        }

        fn load_dead_letters(
            &self,
            plugin_id: &PluginId,
            subscription_id: &SubscriptionId,
            limit: usize,
        ) -> Result<Vec<DeadLetter>, EventBusError> {
            self.inner
                .load_dead_letters(plugin_id, subscription_id, limit)
        }

        fn load_retry(
            &self,
            plugin_id: &PluginId,
            subscription_id: &SubscriptionId,
        ) -> Result<Option<DeliveryRetry>, EventBusError> {
            self.inner.load_retry(plugin_id, subscription_id)
        }

        fn put_retry(&self, retry: &DeliveryRetry) -> Result<(), EventBusError> {
            self.inner.put_retry(retry)
        }

        fn delete_retry(
            &self,
            plugin_id: &PluginId,
            subscription_id: &SubscriptionId,
        ) -> Result<(), EventBusError> {
            self.inner.delete_retry(plugin_id, subscription_id)
        }

        fn get_command_result(
            &self,
            plugin_id: &PluginId,
            command_id: &CommandId,
        ) -> Result<Option<CommandResult>, EventBusError> {
            self.inner.get_command_result(plugin_id, command_id)
        }

        fn put_command_result(&self, result: &CommandResult) -> Result<bool, EventBusError> {
            self.inner.put_command_result(result)
        }
    }

    #[test]
    fn durable_writer_microbatches_concurrent_publishers() {
        let store = Arc::new(CountingStore::new());
        let hub = Arc::new(EventHub::new(
            registry(DeliveryClass::Durable, None),
            Some(store.clone()),
        ));
        let barrier = Arc::new(Barrier::new(9));
        let threads = (0..8)
            .map(|id| {
                let hub = hub.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    hub.publish(
                        &PublisherIdentity::Core,
                        unversioned_envelope(id),
                        &BTreeSet::new(),
                    )
                    .unwrap();
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        hub.flush_durable().unwrap();
        assert_eq!(store.append_calls.load(Ordering::Relaxed), 1);
        assert_eq!(store.high_watermark().unwrap(), 8);
    }

    struct TestSnapshot;
    impl SnapshotProvider for TestSnapshot {
        fn snapshot(&self, kind: &SnapshotKind, watermark: u64) -> Result<Snapshot, EventBusError> {
            Ok(Snapshot {
                kind: kind.clone(),
                watermark,
                revision: 3,
                payload: json!({"tasks": []}),
            })
        }
    }

    #[test]
    fn snapshot_precedes_incremental_delivery() {
        let hub = EventHub::new(registry(DeliveryClass::Ephemeral, Some("tasks")), None);
        hub.register_snapshot_provider(SnapshotKind("tasks".into()), Arc::new(TestSnapshot))
            .unwrap();
        let subscription = hub
            .subscribe(
                PluginId::new("com.example").unwrap(),
                SubscriptionId::new("sessions").unwrap(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Ephemeral,
                &BTreeSet::from([capability("session.read")]),
                2,
            )
            .unwrap();
        assert!(matches!(
            subscription.try_recv().unwrap(),
            Delivery::Snapshot(_)
        ));
    }

    #[test]
    fn ephemeral_snapshot_uses_ephemeral_not_durable_watermark() {
        let store = Arc::new(MemoryEventStore::default());
        store
            .append_batch(&[StoredEvent::new(0, unversioned_envelope(1)).unwrap()])
            .unwrap();
        let hub = EventHub::new(
            registry(DeliveryClass::Ephemeral, Some("tasks")),
            Some(store),
        );
        hub.register_snapshot_provider(SnapshotKind("tasks".into()), Arc::new(TestSnapshot))
            .unwrap();
        let subscription = hub
            .subscribe(
                PluginId::new("com.example").unwrap(),
                SubscriptionId::new("sessions").unwrap(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Ephemeral,
                &BTreeSet::from([capability("session.read")]),
                1,
            )
            .unwrap();
        let Delivery::Snapshot(snapshot) = subscription.try_recv().unwrap() else {
            panic!("expected snapshot");
        };
        assert_eq!(snapshot.watermark, 0);
    }

    #[test]
    fn durable_pump_continues_backlog_without_a_new_publish() {
        let store = Arc::new(MemoryEventStore::default());
        let hub = EventHub::new(registry(DeliveryClass::Durable, None), Some(store));
        hub.publish(
            &PublisherIdentity::Core,
            unversioned_envelope(1),
            &BTreeSet::new(),
        )
        .unwrap();
        hub.publish(
            &PublisherIdentity::Core,
            unversioned_envelope(2),
            &BTreeSet::new(),
        )
        .unwrap();
        let plugin = PluginId::new("com.example").unwrap();
        let subscription_id = SubscriptionId::new("sessions").unwrap();
        let subscription = hub
            .subscribe(
                plugin.clone(),
                subscription_id.clone(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Durable,
                &BTreeSet::from([capability("session.read")]),
                1,
            )
            .unwrap();
        assert_eq!(
            hub.dispatch_durable(&plugin, &subscription_id, 2).unwrap(),
            1
        );
        let first = match subscription.recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected event"),
        };
        assert_eq!(first.sequence, 1);
        hub.ack(
            &plugin,
            &smelt_plugin_api::Ack {
                subscription_id,
                sequence: first.sequence,
                event_id: first.envelope.event_id.clone(),
            },
        )
        .unwrap();
        hub.pump_durable_subscribers().unwrap();
        let second = match subscription.recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected event"),
        };
        assert_eq!(second.sequence, 2);
    }

    #[test]
    fn repeated_durable_store_signal_does_not_enqueue_the_same_sequence_twice() {
        let store = Arc::new(MemoryEventStore::default());
        let stored = store
            .append_batch(&[StoredEvent::new(0, unversioned_envelope(1)).unwrap()])
            .unwrap()
            .pop()
            .unwrap();
        let hub = EventHub::new(registry(DeliveryClass::Durable, None), Some(store));
        let subscription = hub
            .subscribe(
                PluginId::new("com.example").unwrap(),
                SubscriptionId::new("sessions").unwrap(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Durable,
                &BTreeSet::from([capability("session.read")]),
                2,
            )
            .unwrap();

        hub.dispatch_stored(stored.clone()).unwrap();
        hub.dispatch_stored(stored).unwrap();

        let event = match subscription.try_recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected event"),
        };
        assert_eq!(event.sequence, 1);
        assert!(matches!(subscription.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn retryable_nack_redelivers_before_any_successor() {
        let store = Arc::new(MemoryEventStore::default());
        store
            .append_batch(
                &(1..=3)
                    .map(|id| StoredEvent::new(0, unversioned_envelope(id)).unwrap())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let hub = EventHub::new(registry(DeliveryClass::Durable, None), Some(store.clone()));
        let plugin = PluginId::new("com.example").unwrap();
        let subscription_id = SubscriptionId::new("sessions").unwrap();
        let subscription = hub
            .subscribe(
                plugin.clone(),
                subscription_id.clone(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Durable,
                &BTreeSet::from([capability("session.read")]),
                2,
            )
            .unwrap();

        assert_eq!(
            hub.dispatch_durable(&plugin, &subscription_id, 10).unwrap(),
            1
        );
        let first = match subscription.recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected event"),
        };
        assert!(matches!(subscription.try_recv(), Err(TryRecvError::Empty)));
        hub.nack(
            &plugin,
            &smelt_plugin_api::Nack {
                subscription_id: subscription_id.clone(),
                sequence: first.sequence,
                event_id: first.envelope.event_id.clone(),
                disposition: smelt_plugin_api::NackDisposition::Retryable,
                error_code: "temporary".into(),
                message: "retry".into(),
            },
            1,
        )
        .unwrap();
        hub.flush_cursors().unwrap();
        assert_eq!(store.load_cursor(&plugin, &subscription_id).unwrap(), 0);

        assert_eq!(
            hub.dispatch_durable_at(&plugin, &subscription_id, 10, 1)
                .unwrap(),
            0,
            "retryable nack must not be redelivered before its backoff expires"
        );
        assert_eq!(
            hub.dispatch_durable_at(&plugin, &subscription_id, 10, 1 + durable_retry_delay_ms(1),)
                .unwrap(),
            1
        );
        let retry = match subscription.recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected retry"),
        };
        assert_eq!(retry.sequence, first.sequence);
        assert_eq!(
            hub.dispatch_durable(&plugin, &subscription_id, 10).unwrap(),
            0,
            "the retry must remain the subscription's only unacknowledged event"
        );
        hub.ack(
            &plugin,
            &smelt_plugin_api::Ack {
                subscription_id: subscription_id.clone(),
                sequence: retry.sequence,
                event_id: retry.envelope.event_id.clone(),
            },
        )
        .unwrap();
        hub.flush_cursors().unwrap();
        assert_eq!(
            store.load_cursor(&plugin, &subscription_id).unwrap(),
            first.sequence
        );
        assert_eq!(
            hub.dispatch_durable(&plugin, &subscription_id, 10).unwrap(),
            1
        );
        let third = match subscription.recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected event"),
        };
        assert_eq!(third.sequence, 2);
        hub.ack(
            &plugin,
            &smelt_plugin_api::Ack {
                subscription_id: subscription_id.clone(),
                sequence: third.sequence,
                event_id: third.envelope.event_id.clone(),
            },
        )
        .unwrap();
        assert_eq!(
            hub.dispatch_durable(&plugin, &subscription_id, 10).unwrap(),
            1
        );
        let last = match subscription.recv().unwrap() {
            Delivery::Event(event) => event,
            _ => panic!("expected event"),
        };
        assert_eq!(last.sequence, 3);
    }

    #[test]
    fn retryable_nack_survives_hub_recreation() {
        let store = Arc::new(MemoryEventStore::default());
        store
            .append_batch(&[StoredEvent::new(0, unversioned_envelope(1)).unwrap()])
            .unwrap();
        let plugin = PluginId::new("com.example").unwrap();
        let subscription_id = SubscriptionId::new("sessions").unwrap();
        let topics = BTreeSet::from([topic("example.changed")]);
        let capabilities = BTreeSet::from([capability("session.read")]);

        let hub = EventHub::new(registry(DeliveryClass::Durable, None), Some(store.clone()));
        let subscription = hub
            .subscribe(
                plugin.clone(),
                subscription_id.clone(),
                topics.clone(),
                DeliveryClass::Durable,
                &capabilities,
                1,
            )
            .unwrap();
        hub.dispatch_durable_at(&plugin, &subscription_id, 1, 1)
            .unwrap();
        let Delivery::Event(first) = subscription.recv().unwrap() else {
            panic!("expected event");
        };
        hub.nack(
            &plugin,
            &smelt_plugin_api::Nack {
                subscription_id: subscription_id.clone(),
                sequence: first.sequence,
                event_id: first.envelope.event_id.clone(),
                disposition: smelt_plugin_api::NackDisposition::Retryable,
                error_code: "temporary".into(),
                message: "retry".into(),
            },
            1,
        )
        .unwrap();
        drop(subscription);
        drop(hub);

        let hub = EventHub::new(registry(DeliveryClass::Durable, None), Some(store));
        let subscription = hub
            .subscribe(
                plugin.clone(),
                subscription_id.clone(),
                topics,
                DeliveryClass::Durable,
                &capabilities,
                1,
            )
            .unwrap();
        assert_eq!(
            hub.dispatch_durable_at(&plugin, &subscription_id, 1, 1)
                .unwrap(),
            0
        );
        assert_eq!(
            hub.dispatch_durable_at(&plugin, &subscription_id, 1, 1 + durable_retry_delay_ms(1),)
                .unwrap(),
            1
        );
        let Delivery::Event(retry) = subscription.recv().unwrap() else {
            panic!("expected retry");
        };
        assert_eq!(retry.sequence, first.sequence);
    }

    #[test]
    fn retryable_nack_moves_to_dead_letter_after_the_attempt_limit() {
        let store = Arc::new(MemoryEventStore::default());
        store
            .append_batch(&[StoredEvent::new(0, unversioned_envelope(1)).unwrap()])
            .unwrap();
        let hub = EventHub::new(registry(DeliveryClass::Durable, None), Some(store.clone()));
        let plugin = PluginId::new("com.example").unwrap();
        let subscription_id = SubscriptionId::new("sessions").unwrap();
        let subscription = hub
            .subscribe(
                plugin.clone(),
                subscription_id.clone(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Durable,
                &BTreeSet::from([capability("session.read")]),
                1,
            )
            .unwrap();
        let mut now_ms = 1;
        hub.dispatch_durable_at(&plugin, &subscription_id, 1, now_ms)
            .unwrap();

        for attempt in 1..=MAX_DURABLE_DELIVERY_ATTEMPTS {
            let Delivery::Event(event) = subscription.recv().unwrap() else {
                panic!("expected event attempt {attempt}");
            };
            hub.nack(
                &plugin,
                &smelt_plugin_api::Nack {
                    subscription_id: subscription_id.clone(),
                    sequence: event.sequence,
                    event_id: event.envelope.event_id.clone(),
                    disposition: smelt_plugin_api::NackDisposition::Retryable,
                    error_code: "temporary".into(),
                    message: "retry".into(),
                },
                now_ms,
            )
            .unwrap();
            if attempt < MAX_DURABLE_DELIVERY_ATTEMPTS {
                now_ms = now_ms.saturating_add(durable_retry_delay_ms(attempt));
                assert_eq!(
                    hub.dispatch_durable_at(&plugin, &subscription_id, 1, now_ms)
                        .unwrap(),
                    1
                );
            }
        }

        assert_eq!(store.load_cursor(&plugin, &subscription_id).unwrap(), 1);
        let dead_letters = store
            .load_dead_letters(&plugin, &subscription_id, 10)
            .unwrap();
        assert_eq!(dead_letters.len(), 1);
        assert_eq!(dead_letters[0].attempts, MAX_DURABLE_DELIVERY_ATTEMPTS);
        assert_eq!(store.load_retry(&plugin, &subscription_id).unwrap(), None);
    }

    #[test]
    fn disconnected_durable_subscription_is_cleaned_without_blocking_others() {
        let store = Arc::new(MemoryEventStore::default());
        let hub = EventHub::new(registry(DeliveryClass::Durable, None), Some(store));
        let plugin = PluginId::new("com.example").unwrap();
        let disconnected_id = SubscriptionId::new("disconnected").unwrap();
        let disconnected = hub
            .subscribe(
                plugin.clone(),
                disconnected_id.clone(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Durable,
                &BTreeSet::from([capability("session.read")]),
                1,
            )
            .unwrap();
        let connected_id = SubscriptionId::new("connected").unwrap();
        let connected = hub
            .subscribe(
                plugin.clone(),
                connected_id.clone(),
                BTreeSet::from([topic("example.changed")]),
                DeliveryClass::Durable,
                &BTreeSet::from([capability("session.read")]),
                1,
            )
            .unwrap();
        drop(disconnected);

        hub.publish(
            &PublisherIdentity::Core,
            unversioned_envelope(1),
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(
            !hub.subscribers
                .lock()
                .unwrap()
                .contains_key(&(plugin.clone(), disconnected_id))
        );
        let Delivery::Event(event) = connected.recv().unwrap() else {
            panic!("connected durable subscriber must still receive the event");
        };
        assert_eq!(event.sequence, 1);
        assert!(
            hub.subscribers
                .lock()
                .unwrap()
                .contains_key(&(plugin, connected_id))
        );
    }

    #[test]
    fn causal_metadata_cache_stays_bounded_during_long_running_publish() {
        let hub = EventHub::new(registry(DeliveryClass::Ephemeral, None), None);
        for id in 0..8192 {
            hub.publish(
                &PublisherIdentity::Core,
                unversioned_envelope(id),
                &BTreeSet::new(),
            )
            .unwrap();
        }
        assert!(
            hub.causal.lock().unwrap().len() <= 1024,
            "causal metadata must have a fixed upper bound"
        );
    }

    #[test]
    fn causal_cache_evicts_oldest_entry_at_exact_capacity() {
        let mut cache = CausalCache::with_capacity(2);
        for id in 1..=3 {
            cache.insert(
                EventId::new(format!("event-{id}")).unwrap(),
                CausalMetadata {
                    correlation_id: CorrelationId::new("chain").unwrap(),
                    hop_count: 0,
                    envelope_fingerprint: [id as u8; 32],
                },
            );
        }
        assert_eq!(cache.len(), 2);
        assert!(!cache.contains_key(&EventId::new("event-1").unwrap()));
        assert!(cache.contains_key(&EventId::new("event-2").unwrap()));
        assert!(cache.contains_key(&EventId::new("event-3").unwrap()));
    }

    #[test]
    fn causal_metadata_rejects_recent_duplicates_and_validates_known_parent() {
        let hub = EventHub::new(registry(DeliveryClass::Ephemeral, None), None);
        let parent = unversioned_envelope(1);
        hub.publish(&PublisherIdentity::Core, parent.clone(), &BTreeSet::new())
            .unwrap();

        let mut child = unversioned_envelope(2);
        child.causation_id = Some(parent.event_id.clone());
        child.hop_count = 1;
        child.correlation_id = CorrelationId::new("wrong-chain").unwrap();
        assert!(
            hub.publish(&PublisherIdentity::Core, child.clone(), &BTreeSet::new())
                .is_err()
        );

        child.correlation_id = parent.correlation_id;
        hub.publish(&PublisherIdentity::Core, child.clone(), &BTreeSet::new())
            .unwrap();
        assert!(
            hub.publish(&PublisherIdentity::Core, child, &BTreeSet::new())
                .is_err()
        );
    }
}
