//! smeltd 事件总线的 wire 协议：话题、事件信封、订阅与投递确认。

use serde::{Deserialize, Serialize};

use std::{fmt, str::FromStr};

pub const EVENT_BUS_PROTOCOL_VERSION: u16 = 1;
pub const MIN_SUPPORTED_EVENT_BUS_PROTOCOL_VERSION: u16 = 1;

pub const CORE_PROJECTION_VERSION: u16 = 1;
/// v2 起 SessionStateChanged 带 hook 检测到的 provider，v3 起再带 provider 自己的
/// 对话 id；只演进会话投影，不连带 workspace 等仍为 v1 的核心投影。
///
/// 新增字段都是 `Option` + `serde(default)`，所以新客户端能解码任意更低版本的
/// 载荷；解码端因此接受 `1..=当前版本`，而不是只认「1 或当前」。
pub const CORE_SESSION_PROJECTION_VERSION: u16 = 3;
pub const CORE_SESSION_STATE_SCHEMA_VERSION: u16 = 3;
pub const CORE_TOPIC_SESSION_STATE_CHANGED: &str = "session.state_changed";
pub const CORE_TOPIC_SESSION_REMOVED: &str = "session.removed";
pub const CORE_TOPIC_REMOTE_SESSIONS_CHANGED: &str = "remote_sessions.changed";
pub const CORE_TOPIC_WORKSPACE_MENU_CHANGED: &str = "workspace_menu.changed";
pub const CORE_TOPIC_AUTOMATIONS_CHANGED: &str = "automations.changed";
pub const CORE_TOPIC_AGENT_MESSAGE_DELIVERED: &str = "agent.message_delivered";
/// 有人要看一段 daemon 里还没在跑的对话（典型是 daemon 冷重启后手机点开）。
/// 对话的启动规格归桌面端所有，daemon 只能把请求转给它。
pub const CORE_TOPIC_CONVERSATION_WAKE_REQUESTED: &str = "conversation.wake_requested";
/// 项目目录里的文件变了（已滤掉构建产物、依赖目录）。只在客户端用 `project.watch`
/// 关注过的项目上发；客户端据此重取 git 状态、文件树。
pub const CORE_TOPIC_PROJECT_CHANGED: &str = "project.changed";
/// CLI / Control API 要改外观。外观归客户端，运行时只校验后转给在线的桌面端。
pub const CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED: &str = "appearance.change_requested";
/// 终端里的程序用 OSC 9 / 99 / 777 发了一条通知。运行时在 PTY 字节流上扫一次，
/// 会话已有结构化 hook 事件时不发；没人连着时发生的就丢了，不补。
pub const CORE_TOPIC_TERMINAL_NOTIFICATION: &str = "terminal.notification";
/// 终端里的程序用 OSC 9;4（ConEmu 进度）报的进度变了。运行时变了就发，进度条还在时
/// 每 5 秒重发一次（后连上的客户端据此补齐），15 秒没新报告就清掉。不进会话投影。
pub const CORE_TOPIC_TERMINAL_PROGRESS: &str = "terminal.progress";

pub const CORE_CAPABILITY_SESSION_READ: &str = "session.read";
pub const CORE_CAPABILITY_REMOTE_SESSIONS_READ: &str = "remote_sessions.read";
pub const CORE_CAPABILITY_WORKSPACE_READ: &str = "workspace.read";
pub const CORE_CAPABILITY_AUTOMATION_READ: &str = "automation.read";
pub const CORE_CAPABILITY_AGENT_MESSAGE_READ: &str = "agent.message.read";
/// 接收对话唤醒请求。只有持有对话启动规格的一方（桌面端）该拿这项能力：
/// daemon 靠「有没有人订阅」判断请求能不能被兑现。
pub const CORE_CAPABILITY_CONVERSATION_WAKE: &str = "conversation.wake";
/// 接收外观修改请求。只有自己持有外观的桌面端该拿：运行时据此判断有没有人能接。
pub const CORE_CAPABILITY_APPEARANCE_CONTROL: &str = "appearance.control";

pub const CORE_SNAPSHOT_SESSIONS: &str = "core.sessions.v1";
pub const CORE_SNAPSHOT_REMOTE_SESSIONS: &str = "core.remote_sessions.v1";
pub const CORE_SNAPSHOT_WORKSPACE_MENU: &str = "core.workspace_menu.v1";
pub const CORE_SNAPSHOT_AUTOMATIONS: &str = "core.automations.v1";

pub const FIRST_PARTY_DESKTOP_PLUGIN_ID: &str = "core.desktop";

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
                let value = value.into();
                validate_identifier(stringify!($name), &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = ValidationError;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

string_id!(EventId);
string_id!(CorrelationId);
string_id!(CommandId);
string_id!(PluginId);
string_id!(SubscriptionId);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    message: String,
}

impl ValidationError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ValidationError {}

fn validate_identifier(label: &str, value: &str) -> Result<(), ValidationError> {
    if value.is_empty() || value.len() > 255 {
        return Err(ValidationError::new(format!(
            "{label} must contain 1..=255 bytes"
        )));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(ValidationError::new(format!(
            "{label} contains unsupported characters"
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Topic(String);

impl Topic {
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        validate_topic(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_plugin_topic_for(&self, plugin_id: &PluginId) -> bool {
        self.0.strip_prefix("plugin.").is_some_and(|rest| {
            rest.starts_with(plugin_id.as_str())
                && rest.as_bytes().get(plugin_id.as_str().len()) == Some(&b'.')
        })
    }

    pub fn is_plugin_topic(&self) -> bool {
        self.0.starts_with("plugin.")
    }
}

impl<'de> Deserialize<'de> for Topic {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Topic {
    type Err = ValidationError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

fn validate_topic(value: &str) -> Result<(), ValidationError> {
    if value.len() > 255 {
        return Err(ValidationError::new("topic exceeds 255 bytes"));
    }
    let segments = value.split('.').collect::<Vec<_>>();
    if segments.len() < 2
        || segments.iter().any(|segment| {
            segment.is_empty()
                || !segment.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                })
        })
    {
        return Err(ValidationError::new(
            "topic must be dot-separated lowercase namespace segments",
        ));
    }
    if segments[0] == "plugin" && segments.len() < 4 {
        return Err(ValidationError::new(
            "plugin topic must be plugin.<plugin_id>.<name>",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Capability(String);

impl Capability {
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        let segments = value.split('.').collect::<Vec<_>>();
        if segments.len() < 2
            || segments.iter().any(|segment| {
                segment.is_empty()
                    || !segment.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            })
        {
            return Err(ValidationError::new(
                "capability must be dot-separated lowercase segments",
            ));
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Capability {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregateRef {
    pub kind: String,
    pub id: String,
}

impl AggregateRef {
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Result<Self, ValidationError> {
        let kind = kind.into();
        let id = id.into();
        validate_identifier("aggregate kind", &kind)?;
        validate_identifier("aggregate id", &id)?;
        Ok(Self { kind, id })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventSource {
    Core,
    Plugin { plugin_id: PluginId },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PublisherIdentity {
    Core,
    Plugin(PluginId),
}

impl PublisherIdentity {
    pub fn event_source(&self) -> EventSource {
        match self {
            Self::Core => EventSource::Core,
            Self::Plugin(plugin_id) => EventSource::Plugin {
                plugin_id: plugin_id.clone(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelope<P> {
    pub event_id: EventId,
    pub topic: Topic,
    pub schema_version: u16,
    pub occurred_at_ms: u64,
    pub aggregate: Option<AggregateRef>,
    pub aggregate_revision: Option<u64>,
    pub source: EventSource,
    pub correlation_id: CorrelationId,
    pub causation_id: Option<EventId>,
    pub hop_count: u16,
    pub payload: P,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryClass {
    Durable,
    Ephemeral,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataSensitivity {
    Public,
    Internal,
    Sensitive,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SnapshotKind(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopicDescriptor {
    pub topic: Topic,
    pub schema_version: u16,
    pub delivery: DeliveryClass,
    pub required_subscribe_capability: Capability,
    pub required_publish_capability: Option<Capability>,
    pub sensitivity: DataSensitivity,
    pub max_payload_bytes: usize,
    pub snapshot_kind: Option<SnapshotKind>,
}

/// Stable session phase vocabulary. Daemon-internal reducers may evolve independently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreSessionPhase {
    Connecting,
    Thinking,
    ExecutingTool,
    AwaitingApproval,
    WaitingForUser,
    Succeeded,
    Failed,
    #[default]
    Idle,
    Dead,
}

/// Public session projection. Runtime locks, blocker identities, and capability tokens are
/// intentionally absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStateChanged {
    #[serde(rename = "id")]
    pub session_id: String,
    pub generation: u64,
    pub revision: u64,
    pub cwd: Option<String>,
    pub launch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// provider 自己的对话 id（hook 上报）。终端里跑的 agent 换一段对话就会变；
    /// 客户端据此把会话对上 provider 的历史存档（改名、显示名跟随对话）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    pub agent_mcp: bool,
    pub title: Option<String>,
    pub phase: CoreSessionPhase,
    #[serde(rename = "phase_since")]
    pub phase_since_unix_seconds: u64,
    pub pending_question: Option<String>,
    pub tokens_used: Option<u64>,
    pub branch: Option<String>,
    pub dirty_files: Vec<String>,
    #[serde(rename = "updated_at")]
    pub updated_at_unix_seconds: u64,
    pub structured_events: bool,
    pub turn_events: bool,
    pub agent_event_version: Option<u32>,
    pub runtime: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStateRemoved {
    pub session_id: String,
    pub generation: u64,
    pub last_revision: u64,
    pub removed_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStatesSnapshot {
    pub projection_version: u16,
    pub revision: u64,
    pub sessions: Vec<SessionStateChanged>,
}

/// Versioned escape hatch for projections whose complete domain DTO is not yet stabilized.
/// `data` is the projection snapshot, never a provider hook or command envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreProjectionEvent {
    pub projection_version: u16,
    pub revision: u64,
    pub data: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationWakeRequested {
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectChanged {
    /// 关注时传入的项目根目录（运行时那台机器上的路径）。
    pub root: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalNotification {
    pub session_id: String,
    /// 通知正文，已从 OSC 载荷里取出（777 取最后一段，99 已解 base64）。
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalProgressState {
    Normal,
    Error,
    Indeterminate,
    Paused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalProgress {
    pub state: TerminalProgressState,
    /// 0..=100。不确定进度（indeterminate）时没有。
    pub percent: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalProgressChanged {
    pub session_id: String,
    /// `None`：进度条清掉了。
    pub progress: Option<TerminalProgress>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppearanceChangeRequested {
    /// 已由运行时按可脚本目录校验过的外观键值。
    pub settings: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessageDelivered {
    pub message_id: String,
    pub source_session_id: String,
    pub target_session_id: String,
    pub delivered_at_ms: u64,
}

pub fn core_topic_descriptors() -> Result<Vec<TopicDescriptor>, ValidationError> {
    fn descriptor(
        topic: &str,
        delivery: DeliveryClass,
        capability: &str,
        snapshot: Option<&str>,
        max_payload_bytes: usize,
    ) -> Result<TopicDescriptor, ValidationError> {
        Ok(TopicDescriptor {
            topic: Topic::new(topic)?,
            schema_version: core_topic_schema_version(topic),
            delivery,
            required_subscribe_capability: Capability::new(capability)?,
            required_publish_capability: None,
            sensitivity: DataSensitivity::Internal,
            max_payload_bytes,
            snapshot_kind: snapshot.map(|kind| SnapshotKind(kind.to_string())),
        })
    }

    Ok(vec![
        descriptor(
            CORE_TOPIC_SESSION_STATE_CHANGED,
            DeliveryClass::Ephemeral,
            CORE_CAPABILITY_SESSION_READ,
            Some(CORE_SNAPSHOT_SESSIONS),
            64 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_SESSION_REMOVED,
            DeliveryClass::Ephemeral,
            CORE_CAPABILITY_SESSION_READ,
            Some(CORE_SNAPSHOT_SESSIONS),
            16 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_REMOTE_SESSIONS_CHANGED,
            DeliveryClass::Ephemeral,
            CORE_CAPABILITY_REMOTE_SESSIONS_READ,
            Some(CORE_SNAPSHOT_REMOTE_SESSIONS),
            512 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_WORKSPACE_MENU_CHANGED,
            DeliveryClass::Ephemeral,
            CORE_CAPABILITY_WORKSPACE_READ,
            Some(CORE_SNAPSHOT_WORKSPACE_MENU),
            1024 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_AUTOMATIONS_CHANGED,
            DeliveryClass::Ephemeral,
            CORE_CAPABILITY_AUTOMATION_READ,
            Some(CORE_SNAPSHOT_AUTOMATIONS),
            1024 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_AGENT_MESSAGE_DELIVERED,
            DeliveryClass::Durable,
            CORE_CAPABILITY_AGENT_MESSAGE_READ,
            None,
            16 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_CONVERSATION_WAKE_REQUESTED,
            DeliveryClass::Ephemeral,
            CORE_CAPABILITY_CONVERSATION_WAKE,
            None,
            4 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED,
            DeliveryClass::Ephemeral,
            CORE_CAPABILITY_APPEARANCE_CONTROL,
            None,
            4 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_PROJECT_CHANGED,
            DeliveryClass::Ephemeral,
            CORE_CAPABILITY_WORKSPACE_READ,
            None,
            4 * 1024,
        )?,
        // 正文最多 4096 字节（扫描器上限），JSON 转义后可能翻倍。
        descriptor(
            CORE_TOPIC_TERMINAL_NOTIFICATION,
            DeliveryClass::Ephemeral,
            CORE_CAPABILITY_SESSION_READ,
            None,
            16 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_TERMINAL_PROGRESS,
            DeliveryClass::Ephemeral,
            CORE_CAPABILITY_SESSION_READ,
            None,
            4 * 1024,
        )?,
    ])
}

pub fn core_topic_schema_version(topic: &str) -> u16 {
    if topic == CORE_TOPIC_SESSION_STATE_CHANGED {
        CORE_SESSION_STATE_SCHEMA_VERSION
    } else {
        1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstPartyClientKind {
    Desktop,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventClientAuth {
    FirstParty { kind: FirstPartyClientKind },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolRange {
    pub min: u16,
    pub max: u16,
}

impl ProtocolRange {
    pub fn supports(&self, version: u16) -> bool {
        self.min <= self.max && self.min <= version && self.max >= version
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionDeclaration {
    pub id: SubscriptionId,
    pub topics: Vec<Topic>,
    pub delivery: DeliveryClass,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscribeRequest {
    pub protocol: ProtocolRange,
    pub subscription: SubscriptionDeclaration,
    #[serde(default)]
    pub cursor: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventSubscribeRequest {
    pub auth: EventClientAuth,
    pub request: SubscribeRequest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscribeErrorCode {
    Rejected,
    Protocol,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubscribeMessage<P = serde_json::Value> {
    Snapshot {
        kind: SnapshotKind,
        watermark: u64,
        revision: u64,
        payload: P,
    },
    Event {
        sequence: Option<u64>,
        envelope: EventEnvelope<P>,
    },
    Lag {
        dropped: u64,
        resume_after: Option<u64>,
    },
    Cursor {
        last_acked_sequence: u64,
    },
    Error {
        code: SubscribeErrorCode,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ack {
    pub subscription_id: SubscriptionId,
    pub sequence: u64,
    pub event_id: EventId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NackDisposition {
    Retryable,
    Permanent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Nack {
    pub subscription_id: SubscriptionId,
    pub sequence: u64,
    pub event_id: EventId,
    pub disposition: NackDisposition,
    pub error_code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubscribeControlMessage {
    Ack {
        #[serde(flatten)]
        ack: Ack,
    },
    Nack {
        #[serde(flatten)]
        nack: Nack,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_core_and_plugin_topics() {
        assert!(Topic::new("session.state_changed").is_ok());
        assert!(Topic::new("plugin.com.example.review_requested").is_ok());
        for invalid in ["task", "Task.created", "task..created", "plugin.foo.event"] {
            assert!(Topic::new(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn plugin_namespace_is_exact() {
        let plugin = PluginId::new("com.example").unwrap();
        assert!(
            Topic::new("plugin.com.example.changed")
                .unwrap()
                .is_plugin_topic_for(&plugin)
        );
        assert!(
            !Topic::new("plugin.com.example2.changed")
                .unwrap()
                .is_plugin_topic_for(&plugin)
        );
    }

    #[test]
    fn stable_structs_reject_unknown_fields() {
        let error = serde_json::from_value::<AggregateRef>(json!({
            "kind": "session", "id": "1", "future": true
        }))
        .unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn envelope_round_trips() {
        let envelope = EventEnvelope {
            event_id: EventId::new("event-1").unwrap(),
            topic: Topic::new("session.state_changed").unwrap(),
            schema_version: 1,
            occurred_at_ms: 7,
            aggregate: Some(AggregateRef::new("session", "session-1").unwrap()),
            aggregate_revision: Some(1),
            source: EventSource::Core,
            correlation_id: CorrelationId::new("correlation-1").unwrap(),
            causation_id: None,
            hop_count: 0,
            payload: json!({"title": "test"}),
        };
        let encoded = serde_json::to_vec(&envelope).unwrap();
        assert_eq!(
            serde_json::from_slice::<EventEnvelope<serde_json::Value>>(&encoded).unwrap(),
            envelope
        );
    }

    #[test]
    fn wire_ids_and_capabilities_are_validated_on_decode() {
        assert!(serde_json::from_str::<PluginId>(r#""bad id""#).is_err());
        assert!(serde_json::from_str::<Capability>(r#""Task.Read""#).is_err());
    }

    #[test]
    fn core_descriptors_are_stable_and_durable_is_explicit() {
        let descriptors = core_topic_descriptors().unwrap();
        let expected = [
            (
                CORE_TOPIC_SESSION_STATE_CHANGED,
                DeliveryClass::Ephemeral,
                Some(CORE_SNAPSHOT_SESSIONS),
                CORE_SESSION_STATE_SCHEMA_VERSION,
            ),
            (
                CORE_TOPIC_SESSION_REMOVED,
                DeliveryClass::Ephemeral,
                Some(CORE_SNAPSHOT_SESSIONS),
                1,
            ),
            (
                CORE_TOPIC_REMOTE_SESSIONS_CHANGED,
                DeliveryClass::Ephemeral,
                Some(CORE_SNAPSHOT_REMOTE_SESSIONS),
                1,
            ),
            (
                CORE_TOPIC_WORKSPACE_MENU_CHANGED,
                DeliveryClass::Ephemeral,
                Some(CORE_SNAPSHOT_WORKSPACE_MENU),
                1,
            ),
            (
                CORE_TOPIC_AUTOMATIONS_CHANGED,
                DeliveryClass::Ephemeral,
                Some(CORE_SNAPSHOT_AUTOMATIONS),
                1,
            ),
            (
                CORE_TOPIC_AGENT_MESSAGE_DELIVERED,
                DeliveryClass::Durable,
                None,
                1,
            ),
            (
                CORE_TOPIC_CONVERSATION_WAKE_REQUESTED,
                DeliveryClass::Ephemeral,
                None,
                1,
            ),
            (
                CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED,
                DeliveryClass::Ephemeral,
                None,
                1,
            ),
            (
                CORE_TOPIC_PROJECT_CHANGED,
                DeliveryClass::Ephemeral,
                None,
                1,
            ),
            (
                CORE_TOPIC_TERMINAL_NOTIFICATION,
                DeliveryClass::Ephemeral,
                None,
                1,
            ),
            (
                CORE_TOPIC_TERMINAL_PROGRESS,
                DeliveryClass::Ephemeral,
                None,
                1,
            ),
        ];
        assert_eq!(descriptors.len(), expected.len());
        for (topic, delivery, snapshot, schema_version) in expected {
            let descriptor = descriptors
                .iter()
                .find(|descriptor| descriptor.topic.as_str() == topic)
                .unwrap_or_else(|| panic!("missing core topic descriptor {topic}"));
            assert_eq!(descriptor.delivery, delivery, "{topic}");
            assert_eq!(descriptor.schema_version, schema_version, "{topic}");
            assert_eq!(
                descriptor
                    .snapshot_kind
                    .as_ref()
                    .map(|kind| kind.0.as_str()),
                snapshot,
                "{topic}"
            );
        }
    }

    #[test]
    fn subscribe_control_messages_are_versioned_wire_values() {
        let value = json!({
            "type": "ack",
            "subscription_id": "sessions",
            "sequence": 7,
            "event_id": "event-7"
        });
        let message: SubscribeControlMessage = serde_json::from_value(value).unwrap();
        assert!(matches!(
            message,
            SubscribeControlMessage::Ack { ack } if ack.sequence == 7
        ));
    }

    #[test]
    fn event_client_auth_is_a_strict_stable_wire_contract() {
        let auth: EventClientAuth = serde_json::from_value(json!({
            "type": "first_party",
            "kind": "desktop"
        }))
        .unwrap();
        assert_eq!(
            auth,
            EventClientAuth::FirstParty {
                kind: FirstPartyClientKind::Desktop
            }
        );
        assert!(
            serde_json::from_value::<EventClientAuth>(json!({
                "type": "first_party",
                "kind": "desktop",
                "capabilities": ["session.read"]
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<EventClientAuth>(json!({
                "type": "plugin",
                "plugin_id": "self-reported",
                "credential": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }))
            .is_err()
        );
    }
}
