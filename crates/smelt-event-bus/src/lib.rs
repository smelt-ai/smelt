//! 进程内事件路由：话题登记、按能力订阅、快照先于增量、有界邮箱和滞后报告。
//!
//! 事件只在内存里按发布顺序编号、投递，不落盘、不确认、不重放；订阅方滞后或重连时用
//! 快照补齐。

use smelt_event_protocol::{
    Capability, EventEnvelope, EventId, PluginId, PublisherIdentity, SnapshotKind, SubscriptionId,
    Topic, TopicDescriptor,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
};

pub const DEFAULT_MAX_HOPS: u16 = 16;
pub const DEFAULT_CAUSAL_CACHE_CAPACITY: usize = 1024;

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

/// 编好号的事件，连同序列化一次、各订阅方共用的线上字节。
#[derive(Clone, Debug, PartialEq)]
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
    topics: BTreeSet<Topic>,
    sender: SyncSender<Delivery>,
    lagged: Arc<AtomicU64>,
    last_sequence: Arc<AtomicU64>,
    initializing: bool,
    pending: Vec<Arc<StoredEvent>>,
}

#[derive(Clone)]
struct CausalMetadata {
    correlation_id: smelt_event_protocol::CorrelationId,
    hop_count: u16,
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

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

type RevisionReservation = ((String, String), Option<u64>, u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventHubMetrics {
    pub published: u64,
    pub published_wire_bytes: u64,
    pub lagged: u64,
}

#[derive(Default)]
struct Metrics {
    published: AtomicU64,
    published_wire_bytes: AtomicU64,
    lagged: AtomicU64,
}

pub struct EventHub {
    registry: TopicRegistry,
    subscribers: Mutex<BTreeMap<(PluginId, SubscriptionId), SubscriberState>>,
    aggregate_revisions: Mutex<HashMap<(String, String), u64>>,
    causal: Mutex<CausalCache>,
    snapshots: Mutex<BTreeMap<SnapshotKind, Arc<dyn SnapshotProvider>>>,
    max_hops: u16,
    /// 编号和投递在这把锁里一起做：同一聚合的修订号按编号顺序送到每个订阅方。
    publish_order: Mutex<()>,
    sequence: AtomicU64,
    metrics: Metrics,
}

impl EventHub {
    pub fn new(registry: TopicRegistry) -> Self {
        Self {
            registry,
            subscribers: Mutex::new(BTreeMap::new()),
            aggregate_revisions: Mutex::new(HashMap::new()),
            causal: Mutex::new(CausalCache::new()),
            snapshots: Mutex::new(BTreeMap::new()),
            max_hops: DEFAULT_MAX_HOPS,
            publish_order: Mutex::new(()),
            sequence: AtomicU64::new(0),
            metrics: Metrics::default(),
        }
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

    pub fn snapshot(&self, kind: &SnapshotKind) -> Result<Snapshot, EventBusError> {
        let watermark = self.sequence.load(Ordering::Acquire);
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
        let kinds = {
            let subscribers = self
                .subscribers
                .lock()
                .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?;
            let subscriber = subscribers
                .get(&(plugin_id.clone(), subscription_id.clone()))
                .ok_or_else(|| EventBusError::new("unknown subscription"))?;
            subscriber
                .topics
                .iter()
                .filter_map(|topic| self.registry.get(topic))
                .filter_map(|descriptor| descriptor.snapshot_kind.clone())
                .collect::<BTreeSet<_>>()
        };
        kinds.into_iter().map(|kind| self.snapshot(&kind)).collect()
    }

    pub fn subscribe(
        &self,
        plugin_id: PluginId,
        id: SubscriptionId,
        topics: BTreeSet<Topic>,
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
        let state = SubscriberState {
            topics,
            sender: sender.clone(),
            lagged: lagged.clone(),
            last_sequence: last_sequence.clone(),
            initializing: true,
            pending: Vec::new(),
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
        let watermark = self.sequence.load(Ordering::Acquire);
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
                Err(TrySendError::Full(_)) => {
                    state.lagged.fetch_add(1, Ordering::AcqRel);
                    self.metrics.lagged.fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
        drop(subscribers);
        Ok(Subscription {
            plugin_id,
            id,
            receiver,
            lagged,
            last_sequence,
        })
    }

    pub fn publish(
        &self,
        publisher: &PublisherIdentity,
        envelope: EventEnvelope<serde_json::Value>,
        publisher_capabilities: &BTreeSet<Capability>,
    ) -> Result<Arc<StoredEvent>, EventBusError> {
        let descriptor = self
            .registry
            .get(&envelope.topic)
            .ok_or_else(|| EventBusError::new(format!("unknown topic: {}", envelope.topic)))?;
        let _publish_order = self
            .publish_order
            .lock()
            .map_err(|_| EventBusError::new("publish lock poisoned"))?;
        self.validate_publish(publisher, &envelope, descriptor, publisher_capabilities)?;
        let mut event = StoredEvent::new(0, envelope)?;
        if event.wire_bytes.len() > descriptor.max_payload_bytes {
            return Err(EventBusError::new(format!(
                "event exceeds payload limit for {}",
                event.envelope.topic
            )));
        }
        let revision_reservation = self.reserve_revision(&event.envelope)?;
        if let Err(error) = self.reserve_causation(&event.envelope) {
            self.rollback_revision(revision_reservation);
            return Err(error);
        }
        event.sequence = self.sequence.fetch_add(1, Ordering::AcqRel) + 1;
        let event = Arc::new(event);
        self.metrics.published.fetch_add(1, Ordering::Relaxed);
        self.metrics
            .published_wire_bytes
            .fetch_add(event.wire_bytes.len() as u64, Ordering::Relaxed);
        self.deliver(&event)?;
        Ok(event)
    }

    pub fn metrics(&self) -> EventHubMetrics {
        EventHubMetrics {
            published: self.metrics.published.load(Ordering::Relaxed),
            published_wire_bytes: self.metrics.published_wire_bytes.load(Ordering::Relaxed),
            lagged: self.metrics.lagged.load(Ordering::Relaxed),
        }
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

    /// 当前是否有订阅在听这个 topic。用于「请求只有订阅方能兑现」的场景：
    /// 没人听就立刻回绝，而不是让请求方空等一个注定不会来的结果。
    pub fn has_subscriber(&self, topic: &Topic) -> Result<bool, EventBusError> {
        Ok(self
            .subscribers
            .lock()
            .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?
            .values()
            .any(|subscriber| subscriber.topics.contains(topic)))
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
    ) -> Result<(), EventBusError> {
        let mut causal = self
            .causal
            .lock()
            .map_err(|_| EventBusError::new("causal index lock poisoned"))?;
        if causal.get(&envelope.event_id).is_some() {
            return Err(EventBusError::new("duplicate event_id"));
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
            },
        );
        Ok(())
    }

    fn deliver(&self, event: &Arc<StoredEvent>) -> Result<(), EventBusError> {
        let mut subscribers = self
            .subscribers
            .lock()
            .map_err(|_| EventBusError::new("subscriber registry lock poisoned"))?;
        for subscriber in subscribers.values_mut() {
            if !subscriber.topics.contains(&event.envelope.topic) {
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
                    self.metrics.lagged.fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Disconnected(_)) => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use smelt_event_protocol::{AggregateRef, CorrelationId, DataSensitivity, EventSource};
    use std::sync::Barrier;
    use std::thread;

    fn capability(value: &str) -> Capability {
        Capability::new(value).unwrap()
    }

    fn topic(value: &str) -> Topic {
        Topic::new(value).unwrap()
    }

    fn registry(snapshot: Option<&str>) -> TopicRegistry {
        let mut registry = TopicRegistry::default();
        registry
            .register(TopicDescriptor {
                topic: topic("example.changed"),
                schema_version: 1,
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
        let mut registry = registry(None);
        let descriptor = registry.get(&topic("example.changed")).unwrap().clone();
        assert!(registry.register(descriptor).is_err());
    }

    #[test]
    fn filters_capabilities_and_topics() {
        let hub = EventHub::new(registry(None));
        assert!(
            hub.subscribe(
                PluginId::new("com.example").unwrap(),
                SubscriptionId::new("sessions").unwrap(),
                BTreeSet::from([topic("example.changed")]),
                &BTreeSet::new(),
                1,
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_revision_regression() {
        let hub = EventHub::new(registry(None));
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

        let hub = Arc::new(EventHub::new(registry(None)));
        let subscription = hub
            .subscribe(
                PluginId::new("com.example").unwrap(),
                SubscriptionId::new("sessions").unwrap(),
                BTreeSet::from([topic("example.changed")]),
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
        let hub = Arc::new(EventHub::new(registry(None)));
        let poisoner = hub.clone();
        assert!(
            thread::spawn(move || {
                let _guard = poisoner.publish_order.lock().unwrap();
                panic!("poison ephemeral publish lock");
            })
            .join()
            .is_err()
        );

        assert!(
            hub.publish(&PublisherIdentity::Core, envelope(1), &BTreeSet::new())
                .unwrap_err()
                .to_string()
                .contains("publish lock poisoned")
        );
    }

    #[test]
    fn ephemeral_mailbox_reports_lag() {
        let hub = EventHub::new(registry(None));
        let subscription = hub
            .subscribe(
                PluginId::new("com.example").unwrap(),
                SubscriptionId::new("sessions").unwrap(),
                BTreeSet::from([topic("example.changed")]),
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
    fn plugin_cannot_publish_core_topic() {
        let hub = EventHub::new(registry(None));
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
        let hub = EventHub::new(registry(None));
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
                required_subscribe_capability: capability("session.read"),
                required_publish_capability: Some(publish_capability.clone()),
                sensitivity: DataSensitivity::Internal,
                max_payload_bytes: 4096,
                snapshot_kind: None,
            })
            .unwrap();
        let hub = EventHub::new(registry);
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
        let hub = EventHub::new(registry(None));
        for plugin in ["com.first", "com.second"] {
            hub.subscribe(
                PluginId::new(plugin).unwrap(),
                SubscriptionId::new("sessions").unwrap(),
                BTreeSet::from([topic("example.changed")]),
                &BTreeSet::from([capability("session.read")]),
                1,
            )
            .unwrap();
        }
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
        let hub = EventHub::new(registry(Some("tasks")));
        hub.register_snapshot_provider(SnapshotKind("tasks".into()), Arc::new(TestSnapshot))
            .unwrap();
        let subscription = hub
            .subscribe(
                PluginId::new("com.example").unwrap(),
                SubscriptionId::new("sessions").unwrap(),
                BTreeSet::from([topic("example.changed")]),
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
    fn has_subscriber_tracks_live_subscriptions_per_topic() {
        let hub = EventHub::new(registry(None));
        let plugin = PluginId::new("com.example").unwrap();
        let id = SubscriptionId::new("wake").unwrap();
        assert!(!hub.has_subscriber(&topic("example.changed")).unwrap());
        let _subscription = hub
            .subscribe(
                plugin.clone(),
                id.clone(),
                BTreeSet::from([topic("example.changed")]),
                &BTreeSet::from([capability("session.read")]),
                1,
            )
            .unwrap();
        assert!(hub.has_subscriber(&topic("example.changed")).unwrap());
        assert!(!hub.has_subscriber(&topic("other.changed")).unwrap());
        hub.unsubscribe(&plugin, &id).unwrap();
        assert!(!hub.has_subscriber(&topic("example.changed")).unwrap());
    }

    #[test]
    fn causal_metadata_cache_stays_bounded_during_long_running_publish() {
        let hub = EventHub::new(registry(None));
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
        let hub = EventHub::new(registry(None));
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

    #[test]
    fn snapshot_watermark_is_the_publish_sequence() {
        let hub = EventHub::new(registry(Some("tasks")));
        hub.register_snapshot_provider(SnapshotKind("tasks".into()), Arc::new(TestSnapshot))
            .unwrap();
        hub.publish(
            &PublisherIdentity::Core,
            unversioned_envelope(1),
            &BTreeSet::new(),
        )
        .unwrap();
        let subscription = hub
            .subscribe(
                PluginId::new("com.example").unwrap(),
                SubscriptionId::new("sessions").unwrap(),
                BTreeSet::from([topic("example.changed")]),
                &BTreeSet::from([capability("session.read")]),
                1,
            )
            .unwrap();
        let Delivery::Snapshot(snapshot) = subscription.try_recv().unwrap() else {
            panic!("expected snapshot");
        };
        assert_eq!(snapshot.watermark, 1);
    }
}
