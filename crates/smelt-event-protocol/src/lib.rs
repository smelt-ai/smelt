//! smeltd 事件总线的 wire 协议：话题、事件信封、订阅与投递确认。
//!
//! # 怎么演进
//!
//! 桌面、手机、各台机器上的守护各自发布，版本总会错开（手机要过商店审核，一部手机连好几台
//! 机器）。所以运行时推给客户端的载荷按「宽容读取」写：读的一方忽略认不出的字段，标了「会增长」
//! 的枚举认不出的值落到 `Unknown`。据此：
//!
//! - **不升版本**：响应和推送里加字段（一律可缺省，缺了有明确含义）、给标了「会增长」的枚举
//!   （只有不影响判断的显示类枚举）加值、加订阅消息类型。
//! - **升 [`EVENT_BUS_PROTOCOL_VERSION`]**：删字段、删话题、改字段含义（包括新字段改变了旧
//!   字段该怎么读）、加必填字段、改握手。新旧两边在订阅握手时就拿到版本不兼容。
//! - **加话题也要升**：运行时遇到不认识的话题会拒掉整个订阅，新客户端连旧运行时只会反复重连。
//! - **其余枚举加值也要升**：它们解不出会让整条消息失败，比如相位、自动化状态。相位还决定客户端
//!   怎么判断会话在不在忙，没有安全的兜底含义。
//! - **请求不宽容**：客户端发给运行时的请求保持严格。旧运行时要是忽略新字段，用户的意图会悄悄
//!   丢掉（比如发了附件没送到）；严格解码时它会明确拒绝。新字段没用上时不要序列化。
//! - 删掉的字段名不再拿来表示别的东西：旧客户端可能还按原来的意思读。
//! - 认证（[`EventClientAuth`]）保持严格：客户端不能在里面夹带运行时不认识的声明。

use serde::{Deserialize, Serialize};

use std::{fmt, str::FromStr};

/// v2：跨 Agent 消息下线，持久投递随之删掉（它是唯一的使用者）：订阅不再声明 `delivery`、
/// 不带 `cursor`，没有 ack / nack 和 `cursor` 消息，`SessionStateChanged` 去掉 `agent_mcp`。
/// 载荷都是 `deny_unknown_fields`，新旧两边谁也解不了谁，所以整条事件协议升版，在订阅
/// 握手时就拦住。
///
/// v3：桌面发布的侧栏菜单下线，`workspace_menu.changed` 和它的快照删掉，客户端改订
/// `workspace.changed`。订阅里带着未知话题会被整条拒掉，旧客户端只会反复重连，所以升版，
/// 让它在握手时就拿到「版本不兼容」。
///
/// v4：远程会话目录下线，`remote_sessions.changed`、它的快照和读权限删掉。手机、命令行、
/// 智能体新建的对话由运行时直接写进工作区目录，客户端只看 `workspace.changed`。
///
/// v5：会话状态多了 `custom_title`（终端当前那段对话的名字，守护按身份查好）。会话状态
/// 严格解码，旧客户端认不出这个字段，所以升版。
///
/// v6：会话状态多了 `pending_request`（对话会话正在等的那个审批或选择题的身份）。同样
/// 严格解码，所以升版。
///
/// v7：推送载荷改成宽容读取（见模块文档），以后推送加字段不再升版；投影事件和会话快照
/// 去掉 `projection_version`。v6 及更早的客户端严格解码，读不了加过字段的载荷，所以这一版
/// 本身还要升。
pub const EVENT_BUS_PROTOCOL_VERSION: u16 = 7;
pub const MIN_SUPPORTED_EVENT_BUS_PROTOCOL_VERSION: u16 = 7;

/// 以下三个投影版本号**冻结**，不再递增，v7 起的客户端也不检查。只为 v6 及更早的客户端：
/// 它们靠 `system.describe` 里的这几个数判断兼容，照旧报出来，旧客户端才能说清「需要升级
/// 客户端」而不是解析失败。没有 v6 客户端之后连同 describe 里的字段一起删。
pub const CORE_PROJECTION_VERSION: u16 = 1;
pub const CORE_SESSION_PROJECTION_VERSION: u16 = 3;
pub const CORE_SESSION_STATE_SCHEMA_VERSION: u16 = 3;
pub const CORE_TOPIC_SESSION_STATE_CHANGED: &str = "session.state_changed";
pub const CORE_TOPIC_SESSION_REMOVED: &str = "session.removed";
/// 运行时的工作区目录（项目、对话）变了，载荷是改完的整份目录。
pub const CORE_TOPIC_WORKSPACE_CHANGED: &str = "workspace.changed";
pub const CORE_TOPIC_AUTOMATIONS_CHANGED: &str = "automations.changed";
/// 有人要看一段 daemon 里还没在跑的对话（典型是 daemon 冷重启后手机点开）。
/// 对话的启动规格归桌面端所有，daemon 只能把请求转给它。
pub const CORE_TOPIC_CONVERSATION_WAKE_REQUESTED: &str = "conversation.wake_requested";
/// 项目目录里的文件变了（已滤掉构建产物、依赖目录）。只在客户端用 `project.watch`
/// 关注过的项目上发；客户端据此重取 git 状态、文件树。
pub const CORE_TOPIC_PROJECT_CHANGED: &str = "project.changed";
/// 智能体的记忆目录里有文件变了。只在客户端用 `agent.memory.watch` 关注过的智能体上发；
/// 客户端据此重取记忆列表。
pub const CORE_TOPIC_AGENT_MEMORY_CHANGED: &str = "agent_memory.changed";
/// CLI / Control API 要改外观。外观归客户端，运行时只校验后转给在线的桌面端。
pub const CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED: &str = "appearance.change_requested";
/// 终端里的程序用 OSC 9 / 99 / 777 发了一条通知。运行时在 PTY 字节流上扫一次，
/// 会话已有结构化 hook 事件时不发；没人连着时发生的就丢了，不补。
pub const CORE_TOPIC_TERMINAL_NOTIFICATION: &str = "terminal.notification";
/// 终端里的程序用 OSC 9;4（ConEmu 进度）报的进度变了。运行时变了就发，进度条还在时
/// 每 5 秒重发一次（后连上的客户端据此补齐），15 秒没新报告就清掉。不进会话投影。
pub const CORE_TOPIC_TERMINAL_PROGRESS: &str = "terminal.progress";

pub const CORE_CAPABILITY_SESSION_READ: &str = "session.read";
pub const CORE_CAPABILITY_WORKSPACE_READ: &str = "workspace.read";
pub const CORE_CAPABILITY_AUTOMATION_READ: &str = "automation.read";
/// 接收对话唤醒请求。只有持有对话启动规格的一方（桌面端）该拿这项能力：
/// daemon 靠「有没有人订阅」判断请求能不能被兑现。
pub const CORE_CAPABILITY_CONVERSATION_WAKE: &str = "conversation.wake";
/// 接收外观修改请求。只有自己持有外观的桌面端该拿：运行时据此判断有没有人能接。
pub const CORE_CAPABILITY_APPEARANCE_CONTROL: &str = "appearance.control";

pub const CORE_SNAPSHOT_SESSIONS: &str = "core.sessions.v1";
pub const CORE_SNAPSHOT_WORKSPACE: &str = "core.workspace.v1";
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
#[serde(tag = "kind", rename_all = "snake_case")]
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
pub enum DataSensitivity {
    Public,
    Internal,
    Sensitive,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SnapshotKind(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicDescriptor {
    pub topic: Topic,
    pub schema_version: u16,
    pub required_subscribe_capability: Capability,
    pub required_publish_capability: Option<Capability>,
    pub sensitivity: DataSensitivity,
    pub max_payload_bytes: usize,
    pub snapshot_kind: Option<SnapshotKind>,
}

/// Stable session phase vocabulary. Daemon-internal reducers may evolve independently.
/// 不设兜底值：相位决定客户端怎么判断会话在不在忙，所以加值要升协议版本。
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
    /// 用户给 `conversation_id` 那段对话起的名字（`history_title` 的投影），守护在换对话、
    /// 改名时查好。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_title: Option<String>,
    pub title: Option<String>,
    pub phase: CoreSessionPhase,
    #[serde(rename = "phase_since")]
    pub phase_since_unix_seconds: u64,
    pub pending_question: Option<String>,
    /// 对话会话正在等的那个请求的身份：`permission:<请求 id>` 或
    /// `elicitation:<卡片 id>`。变了就是换了一个请求；终端会话没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_request: Option<String>,
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
pub struct SessionStateRemoved {
    pub session_id: String,
    pub generation: u64,
    pub last_revision: u64,
    pub removed_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStatesSnapshot {
    pub revision: u64,
    pub sessions: Vec<SessionStateChanged>,
}

/// Escape hatch for projections whose complete domain DTO is not yet stabilized.
/// `data` is the projection snapshot, never a provider hook or command envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoreProjectionEvent {
    pub revision: u64,
    pub data: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationWakeRequested {
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectChanged {
    /// 关注时传入的项目根目录（运行时那台机器上的路径）。
    pub root: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMemoryChanged {
    /// 哪个智能体自己的记忆变了。`None` 是用户全局记忆，所有智能体都读它。
    pub agent_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalNotification {
    pub session_id: String,
    /// 通知正文，已从 OSC 载荷里取出（777 取最后一段，99 已解 base64）。
    pub message: String,
}

/// 会增长：认不出的值读成 [`Self::Unknown`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalProgressState {
    Normal,
    Error,
    Indeterminate,
    Paused,
    /// 比这个客户端新的运行时报的状态。按不确定进度画。
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalProgress {
    pub state: TerminalProgressState,
    /// 0..=100。不确定进度（indeterminate）时没有。
    pub percent: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalProgressChanged {
    pub session_id: String,
    /// `None`：进度条清掉了。
    pub progress: Option<TerminalProgress>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppearanceChangeRequested {
    /// 已由运行时按可脚本目录校验过的外观键值。
    pub settings: std::collections::BTreeMap<String, serde_json::Value>,
}

pub fn core_topic_descriptors() -> Result<Vec<TopicDescriptor>, ValidationError> {
    fn descriptor(
        topic: &str,
        capability: &str,
        snapshot: Option<&str>,
        max_payload_bytes: usize,
    ) -> Result<TopicDescriptor, ValidationError> {
        Ok(TopicDescriptor {
            topic: Topic::new(topic)?,
            schema_version: core_topic_schema_version(topic),
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
            CORE_CAPABILITY_SESSION_READ,
            Some(CORE_SNAPSHOT_SESSIONS),
            64 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_SESSION_REMOVED,
            CORE_CAPABILITY_SESSION_READ,
            Some(CORE_SNAPSHOT_SESSIONS),
            16 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_WORKSPACE_CHANGED,
            CORE_CAPABILITY_WORKSPACE_READ,
            Some(CORE_SNAPSHOT_WORKSPACE),
            4 * 1024 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_AUTOMATIONS_CHANGED,
            CORE_CAPABILITY_AUTOMATION_READ,
            Some(CORE_SNAPSHOT_AUTOMATIONS),
            1024 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_CONVERSATION_WAKE_REQUESTED,
            CORE_CAPABILITY_CONVERSATION_WAKE,
            None,
            4 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED,
            CORE_CAPABILITY_APPEARANCE_CONTROL,
            None,
            4 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_PROJECT_CHANGED,
            CORE_CAPABILITY_WORKSPACE_READ,
            None,
            4 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_AGENT_MEMORY_CHANGED,
            CORE_CAPABILITY_WORKSPACE_READ,
            None,
            4 * 1024,
        )?,
        // 正文最多 4096 字节（扫描器上限），JSON 转义后可能翻倍。
        descriptor(
            CORE_TOPIC_TERMINAL_NOTIFICATION,
            CORE_CAPABILITY_SESSION_READ,
            None,
            16 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_TERMINAL_PROGRESS,
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscribeRequest {
    pub protocol: ProtocolRange,
    pub subscription: SubscriptionDeclaration,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventSubscribeRequest {
    pub auth: EventClientAuth,
    pub request: SubscribeRequest,
}

/// 会增长：认不出的值读成 [`Self::Unknown`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscribeErrorCode {
    Rejected,
    Protocol,
    Internal,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
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
    Error {
        code: SubscribeErrorCode,
        message: String,
    },
    /// 比这个客户端新的运行时发的消息类型，读的一方跳过。
    #[serde(other)]
    Unknown,
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

    /// 契约：新运行时加的字段、给「会增长」的枚举加的值、加的消息类型，这一版读得了。
    #[test]
    fn a_newer_runtime_payload_is_readable() {
        let state: SessionStateChanged = serde_json::from_value(json!({
            "id": "s1",
            "generation": 1,
            "revision": 2,
            "cwd": null,
            "launch": null,
            "title": null,
            "phase": "idle",
            "phase_since": 0,
            "pending_question": null,
            "tokens_used": null,
            "branch": null,
            "dirty_files": [],
            "updated_at": 0,
            "structured_events": true,
            "turn_events": true,
            "agent_event_version": null,
            "runtime": true,
            "some_future_field": {"nested": [1, 2]},
        }))
        .unwrap();
        assert_eq!(state.phase, CoreSessionPhase::Idle);

        let progress: TerminalProgress =
            serde_json::from_value(json!({"state": "some_future_state", "percent": 3})).unwrap();
        assert_eq!(progress.state, TerminalProgressState::Unknown);

        let message: SubscribeMessage =
            serde_json::from_value(json!({"type": "some_future_message", "anything": 1})).unwrap();
        assert_eq!(message, SubscribeMessage::Unknown);

        let error: SubscribeMessage = serde_json::from_value(
            json!({"type": "error", "code": "some_future_code", "message": "x"}),
        )
        .unwrap();
        assert!(matches!(
            error,
            SubscribeMessage::Error {
                code: SubscribeErrorCode::Unknown,
                ..
            }
        ));
    }

    /// 契约：相位加值要升协议版本，不能被读成某个已有相位。
    #[test]
    fn an_unknown_phase_is_rejected() {
        assert!(serde_json::from_value::<CoreSessionPhase>(json!("some_future_phase")).is_err());
    }

    /// 契约：订阅请求不宽容，旧运行时不能悄悄忽略新客户端的声明。
    #[test]
    fn subscribe_requests_reject_unknown_fields() {
        let request = json!({
            "protocol": {"min": 7, "max": 7},
            "subscription": {"id": "s", "topics": []},
            "some_future_field": true,
        });
        assert!(serde_json::from_value::<SubscribeRequest>(request).is_err());
    }

    /// 契约：旧运行时没带的可缺省字段，这一版读成缺省值。
    #[test]
    fn an_older_runtime_payload_reads_defaults() {
        let state: SessionStateChanged = serde_json::from_value(json!({
            "id": "s1",
            "generation": 1,
            "revision": 2,
            "cwd": null,
            "launch": null,
            "title": null,
            "phase": "idle",
            "phase_since": 0,
            "pending_question": null,
            "tokens_used": null,
            "branch": null,
            "dirty_files": [],
            "updated_at": 0,
            "structured_events": false,
            "turn_events": false,
            "agent_event_version": null,
            "runtime": true,
        }))
        .unwrap();
        assert_eq!(state.provider, None);
        assert_eq!(state.conversation_id, None);
        assert_eq!(state.custom_title, None);
        assert_eq!(state.pending_request, None);
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
    fn core_descriptors_are_stable() {
        let descriptors = core_topic_descriptors().unwrap();
        let expected = [
            (
                CORE_TOPIC_SESSION_STATE_CHANGED,
                Some(CORE_SNAPSHOT_SESSIONS),
                CORE_SESSION_STATE_SCHEMA_VERSION,
            ),
            (CORE_TOPIC_SESSION_REMOVED, Some(CORE_SNAPSHOT_SESSIONS), 1),
            (CORE_TOPIC_WORKSPACE_CHANGED, Some(CORE_SNAPSHOT_WORKSPACE), 1),
            (
                CORE_TOPIC_AUTOMATIONS_CHANGED,
                Some(CORE_SNAPSHOT_AUTOMATIONS),
                1,
            ),
            (CORE_TOPIC_CONVERSATION_WAKE_REQUESTED, None, 1),
            (CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED, None, 1),
            (CORE_TOPIC_PROJECT_CHANGED, None, 1),
            (CORE_TOPIC_AGENT_MEMORY_CHANGED, None, 1),
            (CORE_TOPIC_TERMINAL_NOTIFICATION, None, 1),
            (CORE_TOPIC_TERMINAL_PROGRESS, None, 1),
        ];
        assert_eq!(descriptors.len(), expected.len());
        for (topic, snapshot, schema_version) in expected {
            let descriptor = descriptors
                .iter()
                .find(|descriptor| descriptor.topic.as_str() == topic)
                .unwrap_or_else(|| panic!("missing core topic descriptor {topic}"));
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
