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
//!   字段该怎么读）、加必填字段、改握手。这些改动要先想清楚已发布的运行时怎么办，见下一条。
//! - **读得了就不抬最低版本**：[`MIN_SUPPORTED_EVENT_BUS_PROTOCOL_VERSION`] 不跟着当前版本
//!   走，新客户端读得了的旧运行时照常连，个别功能不对等它升级。读不了（形状变了、没法宽容
//!   读取）才抬，旧运行时判为过旧、请它升级。不为旧运行时写专门的兼容分支。
//! - **核心订阅加话题也要升**：运行时遇到不认识的话题会拒掉整个订阅，新客户端连旧运行时只会
//!   反复重连。不升版本的新话题只能放进单独的一条订阅（见 [`PiConfigChanged`]）：旧运行时拒掉
//!   的只是那一条，客户端不重试，那项功能在这台机器上不可用。
//! - **其余枚举加值也要升**：它们解不出会让整条消息失败，比如相位、自动化状态。相位还决定客户端
//!   怎么判断会话在不在忙，没有安全的兜底含义。
//! - **请求不宽容**：订阅请求和 `ConversationInput` 保持严格。旧运行时要是忽略新字段，用户的
//!   意图会悄悄丢掉（比如发了附件没送到）；严格解码时它会明确拒绝。新字段没用上时不要序列化。
//!   控制接口的参数不在此列，规则见 `docs/control-api.md`。
//! - 删掉的字段名不再拿来表示别的东西：旧客户端可能还按原来的意思读。
//! - 认证（[`EventClientAuth`]）保持严格：客户端不能在里面夹带运行时不认识的声明。

use serde::{Deserialize, Serialize};

use std::{fmt, str::FromStr};

// 运行时自己说的话的文案（note.rs，见 docs/i18n.md）。
rust_i18n::i18n!("locales", fallback = "en");

mod note;
pub use note::{ConversationEndReason, SessionNote, SignInAgent, TurnFailure};

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
/// 去掉 `projection_version`。v6 的客户端严格解码，读不了这一版运行时的载荷，所以运行时要
/// 升版拦住它们。反过来 v7 的客户端读得了 v6 运行时：多出来的 `projection_version` 忽略，
/// 没带的字段读成缺省。
///
/// 带回执的外观修改（[`CORE_TOPIC_APPEARANCE_APPLY_REQUESTED`]）加在 v7 之后，没有升版本：
/// 它只走单独的一条订阅，核心订阅仍是 v7 的 `appearance.change_requested`。
pub const EVENT_BUS_PROTOCOL_VERSION: u16 = 7;
/// 客户端接受的最低运行时版本，见模块文档「读得了就不抬最低版本」。
pub const MIN_SUPPORTED_EVENT_BUS_PROTOCOL_VERSION: u16 = 6;

/// 以下三个投影版本号**冻结**，不再递增，v7 起的客户端也不检查。只为 v6 及更早的客户端：
/// 它们靠 `system.describe` 里的这几个数判断兼容，照旧报出来，旧客户端才能说清「需要升级
/// 客户端」而不是解析失败。没有 v6 客户端之后连同 describe 里的字段一起删；
/// `CORE_SESSION_STATE_SCHEMA_VERSION` 还填着信封的 `schema_version`（没有客户端读），到时
/// 一起处理。
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
/// 某台机器上 Pi 的配置变了（模型、凭据、MCP、插件），载荷 [`PiConfigChanged`]。不进
/// 核心订阅，见模块文档。
pub const CORE_TOPIC_PI_CONFIG_CHANGED: &str = "pi_config.changed";
/// 某台机器上 agent 的历史存档经运行时改过（改名、删除、记下所属智能体），载荷
/// [`HistoryChanged`]。不进核心订阅，同 [`CORE_TOPIC_PI_CONFIG_CHANGED`]。
pub const CORE_TOPIC_HISTORY_CHANGED: &str = "history.changed";
/// 这台运行时的对端节点（`peer.list`）变了：本机配对、改设置、删除，或别的节点来配对、
/// 取消配对。载荷 [`PeersChanged`]。不进核心订阅，同 [`CORE_TOPIC_PI_CONFIG_CHANGED`]。
pub const CORE_TOPIC_PEERS_CHANGED: &str = "peers.changed";
/// v7 核心话题：CLI / Control API 要改外观，载荷 [`AppearanceChangeRequested`]。外观归客户端，
/// 运行时只校验后转给在线的桌面端。
pub const CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED: &str = "appearance.change_requested";
/// 带回执的外观修改，载荷 [`AppearanceApplyRequested`]，桌面用 [`AppearanceApplyReceipt`]
/// 回执。加在 v7 之后、没有升版本，所以不进核心订阅，只放在单独的一条订阅里（同
/// [`PiConfigChanged`]），不认识该话题的旧运行时只会拒掉那一条。
pub const CORE_TOPIC_APPEARANCE_APPLY_REQUESTED: &str = "appearance.apply_requested";
/// 命令行要列出或执行本机 App 登记的命令，载荷 [`AppCommandRequested`]，桌面用
/// [`AppCommandReceipt`] 回执。和带回执的外观修改一样单独订阅、不进核心订阅，不升版本。
pub const CORE_TOPIC_APP_COMMAND_REQUESTED: &str = "app.command_requested";
/// 终端里的程序用 OSC 9 / 99 / 777 发了一条通知。运行时在 PTY 字节流上扫一次，
/// 会话已有结构化 hook 事件时不发；没人连着时发生的就丢了，不补。
pub const CORE_TOPIC_TERMINAL_NOTIFICATION: &str = "terminal.notification";
/// 一段会话的提醒变了（[`AttentionChanged`]），带快照 [`CORE_SNAPSHOT_ATTENTION`]。
///
/// 加在事件协议 v7 之后、没有升版本，所以只放在单独的一条订阅里，v7 的运行时会拒掉那一条，
/// 那台机器上就没有提醒。
pub const CORE_TOPIC_ATTENTION_CHANGED: &str = "attention.changed";
/// 那台机器的智能体配置（自定义 Agent、智能体定义、环境变量、hook 开关）变了，载荷是改完的
/// 整份 `smelt_model::agent_host::AgentHostSettings`（同 `agent.host.get` 的结果），带快照
/// [`CORE_SNAPSHOT_AGENT_HOST`]。配置只有运行时写，谁改的都发；客户端只留这一份，不另读。
///
/// 加在事件协议 v7 之后、没有升版本，同 [`CORE_TOPIC_ATTENTION_CHANGED`] 只放在单独的一条
/// 订阅里；v7 的运行时会拒掉那一条，那台机器的智能体配置要等它升级后才可用。
pub const CORE_TOPIC_AGENT_HOST_CHANGED: &str = "agent_host.changed";
/// 终端里的程序用 OSC 9;4（ConEmu 进度）报的进度变了。运行时变了就发，进度条还在时
/// 每 5 秒重发一次（后连上的客户端据此补齐），15 秒没新报告就清掉。不进会话投影。
pub const CORE_TOPIC_TERMINAL_PROGRESS: &str = "terminal.progress";
/// 一次目录占用统计（`fs.usage.scan`）的进度，载荷 [`FsUsageProgressChanged`]。扫的过程中
/// 隔一会儿发一次，扫完或被 `fs.usage.release` 停下时再发一次。没有快照：错过了就用
/// `fs.usage.children` 问。不进核心订阅，同 [`CORE_TOPIC_ATTENTION_CHANGED`]。
pub const CORE_TOPIC_FS_USAGE_PROGRESS: &str = "fs.usage.progress";
/// 「新建会话」目录（智能体选项、分组动作、Pin 与 CLI 探测）在运行时更新了，载荷为
/// `smelt_model::control_api::NewSessionCatalogResult`，带快照 [`CORE_SNAPSHOT_NEW_SESSION_CATALOG`]。
///
/// 加在事件协议 v7 之后、没有升版本，同 [`CORE_TOPIC_AGENT_HOST_CHANGED`] 只放在单独的一条
/// 订阅里；不认识该话题的旧运行时只会拒掉那一条。
pub const CORE_TOPIC_NEW_SESSION_CATALOG_CHANGED: &str = "new_session.catalog_changed";
/// 群组列表、成员实时状态、暂停态、剩余跳数与盲评/任务进度变更，带快照 [`CORE_SNAPSHOT_GROUPS`]。
/// 加在 v7 之后、不升版本，走独立订阅。
pub const CORE_TOPIC_GROUPS_CHANGED: &str = "group.changed";
/// 群组内追加了新消息、盲评批量揭晓或权限放行卡状态更新。不带快照，走独立订阅。
pub const CORE_TOPIC_GROUP_MESSAGE_APPENDED: &str = "group.message_appended";

pub const CORE_CAPABILITY_SESSION_READ: &str = "session.read";
pub const CORE_CAPABILITY_WORKSPACE_READ: &str = "workspace.read";
pub const CORE_CAPABILITY_AUTOMATION_READ: &str = "automation.read";
/// 接收对话唤醒请求。只有持有对话启动规格的一方（桌面端）该拿这项能力：
/// daemon 靠「有没有人订阅」判断请求能不能被兑现。
pub const CORE_CAPABILITY_CONVERSATION_WAKE: &str = "conversation.wake";
/// 接收外观修改请求。只有自己持有外观的桌面端该拿：运行时据此判断有没有人能接。
pub const CORE_CAPABILITY_APPEARANCE_CONTROL: &str = "appearance.control";
/// 接收命令行转来的 App 命令（列出、执行）。只有登记着命令的本机桌面该拿。
pub const CORE_CAPABILITY_APP_COMMAND: &str = "app.command";

pub const CORE_SNAPSHOT_SESSIONS: &str = "core.sessions.v1";
pub const CORE_SNAPSHOT_WORKSPACE: &str = "core.workspace.v1";
pub const CORE_SNAPSHOT_AUTOMATIONS: &str = "core.automations.v1";
pub const CORE_SNAPSHOT_ATTENTION: &str = "core.attention.v1";
pub const CORE_SNAPSHOT_AGENT_HOST: &str = "core.agent_host.v1";
pub const CORE_SNAPSHOT_NEW_SESSION_CATALOG: &str = "core.new_session_catalog.v1";
pub const CORE_SNAPSHOT_GROUPS: &str = "core.groups.v1";

pub const FIRST_PARTY_DESKTOP_PLUGIN_ID: &str = "core.desktop";

macro_rules! string_id {
    ($name:ident) => {
        #[derive(
            Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, schemars::JsonSchema,
        )]
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

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, schemars::JsonSchema)]
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

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, schemars::JsonSchema)]
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

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DataSensitivity {
    Public,
    Internal,
    Sensitive,
}

#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(transparent)]
pub struct SnapshotKind(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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
    /// 各端显示的这段对话的名字（侧栏的对话行、菜单栏）：运行时按工作区目录、用户改的名、
    /// 会话名、智能体定义现算好，同一段对话里每一格都是同一个值。客户端直接显示，不再自己
    /// 拼。不在工作区目录里的会话、旧运行时没有，客户端显示通用名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_name: Option<String>,
    /// 这一格自己的名字（分屏的子行、手机上的一格）。只有一格时和 `conversation_name` 相同。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub phase: CoreSessionPhase,
    #[serde(rename = "phase_since")]
    pub phase_since_unix_seconds: u64,
    /// 这一轮从什么时候开始忙（unix 秒），会话列表上「本轮已运行」按它算；一轮里换阶段
    /// 不变，不在一轮里是 0。旧运行时不报，读成 0：客户端就不显示本轮时长。
    #[serde(rename = "turn_started_at", default)]
    pub turn_started_at_unix_seconds: u64,
    pub pending_question: Option<String>,
    /// `pending_question` 是运行时自己说的话时，它的种类和参数，客户端按自己的语言说
    /// （[`SessionNote::text`]）；此时 `pending_question` 是运行时按英文说的同一句，给旧客户端
    /// 兜底。智能体的原话没有。旧运行时不报。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_note: Option<SessionNote>,
    /// 对话会话正在等的那个请求的身份：`permission:<请求 id>` 或
    /// `elicitation:<卡片 id>`。变了就是换了一个请求；终端会话没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_request: Option<String>,
    /// 最近结束的那轮对应的外部投递（`conversation.prompt` 的 `delivery_id`）；用户自己说的
    /// 那轮是 None。投递方看到自己的 id 就去 `conversation.turn_result` 取结果。旧运行时不报。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_delivery_id: Option<String>,
    pub tokens_used: Option<u64>,
    /// 会话目录所在仓库的当前分支，运行时监听 HEAD 维护，切了分支会再推一次。不是 git
    /// 仓库、仓库还没有提交是 None，分离头指针是 `HEAD`。旧运行时不报。
    pub branch: Option<String>,
    pub dirty_files: Vec<String>,
    #[serde(rename = "updated_at")]
    pub updated_at_unix_seconds: u64,
    pub structured_events: bool,
    pub turn_events: bool,
    pub agent_event_version: Option<u32>,
    pub runtime: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionStateRemoved {
    pub session_id: String,
    pub generation: u64,
    pub last_revision: u64,
    pub removed_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionStatesSnapshot {
    pub revision: u64,
    pub sessions: Vec<SessionStateChanged>,
}

/// Escape hatch for projections whose complete domain DTO is not yet stabilized.
/// `data` is the projection snapshot, never a provider hook or command envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CoreProjectionEvent {
    pub revision: u64,
    pub data: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConversationWakeRequested {
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectChanged {
    /// 关注时传入的项目根目录（运行时那台机器上的路径）。
    pub root: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentMemoryChanged {
    /// 哪个智能体自己的记忆变了。`None` 是用户全局记忆，所有智能体都读它。
    pub agent_id: Option<String>,
}

/// 经运行时改了 Pi 的配置：谁改的都发（桌面、手机、命令行），读的一方据此重取
/// `pi.config.get`，不靠改的那一方去通知别人。绕过运行时的修改（手改配置文件、直接用 `pi`
/// 命令行）不在内。没有字段：读的一方重取自己关心的那部分。删插件、MCP 服务器时运行时还会清掉
/// 各智能体对它的勾选，所以缓存了智能体配置的一方也要重取 `agent.host.get`。
///
/// 这个话题加在事件协议 v7 之后、没有升版本，所以只放在单独的一条订阅里，v7 的运行时会拒掉
/// 那一条。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PiConfigChanged {}

/// 对端节点变了。没有字段：读的一方重取 `peer.list`。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeersChanged {}

/// 经运行时改了历史存档：谁改的都发，读的一方据此重取 `history.list`。agent 自己往存档里
/// 写新会话不在内（运行时不盯存档目录）。没有字段：读的一方重取自己在看的那几份列表。
///
/// 加在事件协议 v7 之后、没有升版本，所以单独占一条订阅，旧运行时只会拒掉那一条。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryChanged {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TerminalNotification {
    pub session_id: String,
    /// 智能体主动发的通知（`notify_user` 工具）带的标题。终端程序的 OSC 通知没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 通知正文，已从 OSC 载荷里取出（777 取最后一段，99 已解 base64），或智能体写的消息。
    pub message: String,
}

/// 要告诉用户的一件事是什么。会增长：认不出的值读成 [`Self::Unknown`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    /// 等你批准。
    Approval,
    /// 等你回答或继续。
    Input,
    /// 这一轮做完了。
    Success,
    /// 这一轮失败了。
    Failure,
    /// 终端响铃。
    Bell,
    /// 终端程序的 OSC 通知，或智能体主动发的消息。
    Notice,
    /// 比这个客户端新的运行时报的类型。按 [`Self::Notice`] 对待。
    #[serde(other)]
    Unknown,
}

impl AttentionKind {
    /// 按当前语言说的种类名；终端通知、响铃这些用来源给的标题，没有自己的名字。
    pub fn label(&self) -> Option<String> {
        let label = match self {
            Self::Approval => rust_i18n::t!("attention.approval"),
            Self::Input => rust_i18n::t!("attention.input"),
            Self::Success => rust_i18n::t!("attention.success"),
            Self::Failure => rust_i18n::t!("attention.failure"),
            Self::Bell | Self::Notice | Self::Unknown => return None,
        };
        Some(label.into_owned())
    }
}

/// 没有名字的会话在提醒里怎么称呼：按当前语言说「会话」加 id 的开头。
pub fn unnamed_session(id: &str) -> String {
    let short = id.get(..8).unwrap_or(id);
    rust_i18n::t!("attention.unnamed_session", id = short).into_owned()
}

impl AttentionKind {
    /// 等用户处理的事：等批准、等输入、失败。看过也不算完，会话往下走了才算。
    pub fn requires_action(self) -> bool {
        matches!(self, Self::Approval | Self::Input | Self::Failure)
    }
}

/// 一条提醒。给人看用 [`AttentionItem::title_text`]、[`AttentionItem::message_text`]。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AttentionItem {
    pub kind: AttentionKind,
    /// 短标题：等批准、等输入、完成、失败这几种是运行时按英文说的种类名（客户端按种类自己说），
    /// 其余是来源给的标题（终端通知的标题、智能体的名字）。
    pub title: String,
    /// 一行内容：等批准的命令、等回答的问题、失败原因、通知正文。有 `note` 时是它按英文说的
    /// 同一句，给旧客户端兜底。
    pub message: String,
    /// `message` 是运行时自己说的话时，它的种类和参数。旧运行时不报。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<SessionNote>,
}

impl AttentionItem {
    /// 按当前语言说的标题：种类有自己的名字就用它，否则是来源给的标题。
    pub fn title_text(&self) -> String {
        self.kind.label().unwrap_or_else(|| self.title.clone())
    }

    /// 按当前语言说的内容：运行时自己说的话按种类说，其余原样。
    pub fn message_text(&self) -> String {
        self.note
            .as_ref()
            .and_then(SessionNote::text)
            .unwrap_or_else(|| self.message.clone())
    }
}

/// 一段会话此刻的提醒。两样都没有的会话不在快照里。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionAttention {
    pub session_id: String,
    /// 等你处理的事（等批准、等输入、失败）。看过也留着，会话往下走了才清掉。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<AttentionItem>,
    /// 最近一条还没看过的提醒，可能就是上面那件事。哪台设备上看过、在终端里打字、在对话里
    /// 回复，都算看过。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread: Option<AttentionItem>,
}

impl SessionAttention {
    pub fn is_empty(&self) -> bool {
        self.action.is_none() && self.unread.is_none()
    }
}

/// [`CORE_SNAPSHOT_ATTENTION`] 的载荷：所有有提醒的会话。只在内存里：运行时重启后从空开始，
/// 重启前的提醒不补。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AttentionSnapshot {
    pub revision: u64,
    pub sessions: Vec<SessionAttention>,
}

/// [`CORE_TOPIC_ATTENTION_CHANGED`] 的载荷。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AttentionChanged {
    pub revision: u64,
    /// 这段会话现在的提醒，整份替换。空的就是没有了（看过、处理掉、会话删了）。
    pub session: SessionAttention,
    /// 刚来、该响一下的那条（系统通知、推送）。快照里没有：后连上的客户端不补响。同一段会话
    /// 同样的提醒 60 秒内只响一次。自动化运行的会话、命令行发的消息不进会话提醒，只有这一项。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alert: Option<AttentionItem>,
}

/// 会增长：认不出的值读成 [`Self::Unknown`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TerminalProgress {
    pub state: TerminalProgressState,
    /// 0..=100。不确定进度（indeterminate）时没有。
    pub percent: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FsUsageProgressChanged {
    /// `fs.usage.scan` 给的扫描 id。
    pub scan: String,
    /// 到目前为止数过的文件数和字节数。
    pub files: u64,
    pub bytes: u64,
    /// 扫完了，或者被停下了。
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TerminalProgressChanged {
    pub session_id: String,
    /// `None`：进度条清掉了。
    pub progress: Option<TerminalProgress>,
}

/// v7 话题 [`CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED`] 的载荷，形状不能动。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AppearanceChangeRequested {
    /// 已由运行时按可脚本目录校验过的外观键值。
    pub settings: std::collections::BTreeMap<String, serde_json::Value>,
}

/// 单播给本机桌面的外观修改（独立订阅）；请求 id 用来关联回执。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AppearanceApplyRequested {
    pub request_id: String,
    pub settings: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AppearanceApplyOutcome {
    Applied,
    Invalid { message: String },
    Failed { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AppearanceApplyReceipt {
    pub request_id: String,
    pub outcome: AppearanceApplyOutcome,
}

/// 单播给本机桌面的 App 命令请求（独立订阅）；请求 id 用来关联回执。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AppCommandRequested {
    pub request_id: String,
    pub request: AppCommandRequest,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppCommandRequest {
    /// 列出开给命令行的命令。
    List,
    /// 执行一条开给命令行的命令。
    Run {
        command: String,
        #[serde(default)]
        args: serde_json::Value,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AppCommandOutcome {
    /// 做完了。列出时是命令表，执行时是命令交回的值（没有就是 `null`）。
    Done { value: serde_json::Value },
    /// 没做成，`message` 是给人看的原因（桌面按自己的语言说）。
    Failed { message: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AppCommandReceipt {
    pub request_id: String,
    pub outcome: AppCommandOutcome,
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
            CORE_TOPIC_APPEARANCE_APPLY_REQUESTED,
            CORE_CAPABILITY_APPEARANCE_CONTROL,
            None,
            4 * 1024,
        )?,
        descriptor(
            CORE_TOPIC_APP_COMMAND_REQUESTED,
            CORE_CAPABILITY_APP_COMMAND,
            None,
            64 * 1024,
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
        descriptor(
            CORE_TOPIC_PI_CONFIG_CHANGED,
            CORE_CAPABILITY_WORKSPACE_READ,
            None,
            1024,
        )?,
        descriptor(
            CORE_TOPIC_HISTORY_CHANGED,
            CORE_CAPABILITY_WORKSPACE_READ,
            None,
            1024,
        )?,
        descriptor(
            CORE_TOPIC_PEERS_CHANGED,
            CORE_CAPABILITY_WORKSPACE_READ,
            None,
            1024,
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
        descriptor(
            CORE_TOPIC_FS_USAGE_PROGRESS,
            CORE_CAPABILITY_WORKSPACE_READ,
            None,
            1024,
        )?,
        // 两条提醒，各自的正文最多 4096 字节（扫描器上限），JSON 转义后可能翻倍。
        descriptor(
            CORE_TOPIC_ATTENTION_CHANGED,
            CORE_CAPABILITY_SESSION_READ,
            Some(CORE_SNAPSHOT_ATTENTION),
            48 * 1024,
        )?,
        // 智能体定义带着系统提示词、技能和环境变量，和自动化定义同一个量级。
        descriptor(
            CORE_TOPIC_AGENT_HOST_CHANGED,
            CORE_CAPABILITY_WORKSPACE_READ,
            Some(CORE_SNAPSHOT_AGENT_HOST),
            1024 * 1024,
        )?,
        // 新建会话目录包含智能体选项与分组启动项。
        descriptor(
            CORE_TOPIC_NEW_SESSION_CATALOG_CHANGED,
            CORE_CAPABILITY_WORKSPACE_READ,
            Some(CORE_SNAPSHOT_NEW_SESSION_CATALOG),
            512 * 1024,
        )?,
        // 群组列表与成员运行态快照。
        descriptor(
            CORE_TOPIC_GROUPS_CHANGED,
            CORE_CAPABILITY_SESSION_READ,
            Some(CORE_SNAPSHOT_GROUPS),
            512 * 1024,
        )?,
        // 群组增量消息、盲评揭晓与放行卡事件。
        descriptor(
            CORE_TOPIC_GROUP_MESSAGE_APPENDED,
            CORE_CAPABILITY_SESSION_READ,
            None,
            256 * 1024,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FirstPartyClientKind {
    Desktop,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventClientAuth {
    FirstParty { kind: FirstPartyClientKind },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionDeclaration {
    pub id: SubscriptionId,
    pub topics: Vec<Topic>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscribeRequest {
    pub protocol: ProtocolRange,
    pub subscription: SubscriptionDeclaration,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventSubscribeRequest {
    pub auth: EventClientAuth,
    pub request: SubscribeRequest,
}

/// 会增长：认不出的值读成 [`Self::Unknown`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubscribeErrorCode {
    Rejected,
    Protocol,
    Internal,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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
        // 这一版之前的运行时不报本轮开始时间：读成 0，客户端不显示本轮时长。
        assert_eq!(state.turn_started_at_unix_seconds, 0);

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
        let error = serde_json::from_value::<SubscribeRequest>(request).unwrap_err();
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    /// 契约：已发布的 v6 运行时还带着 `projection_version`，这一版读得了。
    #[test]
    fn a_v6_runtime_projection_payload_is_readable() {
        let snapshot: SessionStatesSnapshot = serde_json::from_value(json!({
            "projection_version": 3,
            "revision": 4,
            "sessions": [],
        }))
        .unwrap();
        assert_eq!(snapshot.revision, 4);
        let projection: CoreProjectionEvent = serde_json::from_value(json!({
            "projection_version": 1,
            "revision": 5,
            "data": null,
        }))
        .unwrap();
        assert_eq!(projection.revision, 5);
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
        assert_eq!(state.completed_delivery_id, None);
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
            (
                CORE_TOPIC_WORKSPACE_CHANGED,
                Some(CORE_SNAPSHOT_WORKSPACE),
                1,
            ),
            (
                CORE_TOPIC_AUTOMATIONS_CHANGED,
                Some(CORE_SNAPSHOT_AUTOMATIONS),
                1,
            ),
            (CORE_TOPIC_CONVERSATION_WAKE_REQUESTED, None, 1),
            (CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED, None, 1),
            (CORE_TOPIC_APPEARANCE_APPLY_REQUESTED, None, 1),
            (CORE_TOPIC_APP_COMMAND_REQUESTED, None, 1),
            (CORE_TOPIC_PROJECT_CHANGED, None, 1),
            (CORE_TOPIC_AGENT_MEMORY_CHANGED, None, 1),
            (CORE_TOPIC_PI_CONFIG_CHANGED, None, 1),
            (CORE_TOPIC_HISTORY_CHANGED, None, 1),
            (CORE_TOPIC_PEERS_CHANGED, None, 1),
            (CORE_TOPIC_TERMINAL_NOTIFICATION, None, 1),
            (CORE_TOPIC_TERMINAL_PROGRESS, None, 1),
            (CORE_TOPIC_FS_USAGE_PROGRESS, None, 1),
            (
                CORE_TOPIC_ATTENTION_CHANGED,
                Some(CORE_SNAPSHOT_ATTENTION),
                1,
            ),
            (
                CORE_TOPIC_AGENT_HOST_CHANGED,
                Some(CORE_SNAPSHOT_AGENT_HOST),
                1,
            ),
            (
                CORE_TOPIC_NEW_SESSION_CATALOG_CHANGED,
                Some(CORE_SNAPSHOT_NEW_SESSION_CATALOG),
                1,
            ),
            (CORE_TOPIC_GROUPS_CHANGED, Some(CORE_SNAPSHOT_GROUPS), 1),
            (CORE_TOPIC_GROUP_MESSAGE_APPENDED, None, 1),
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

    /// v7 外观载荷的线上形状：只有 `settings`，不带 `request_id`。
    #[test]
    fn legacy_appearance_change_payload_keeps_v7_shape() {
        assert_eq!(EVENT_BUS_PROTOCOL_VERSION, 7);
        assert_eq!(
            CORE_TOPIC_APPEARANCE_CHANGE_REQUESTED,
            "appearance.change_requested"
        );
        let payload = json!({ "settings": { "theme": "dark" } });
        let request: AppearanceChangeRequested = serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), payload);
    }

    #[test]
    fn appearance_receipts_round_trip_success_and_failures() {
        for outcome in [
            AppearanceApplyOutcome::Applied,
            AppearanceApplyOutcome::Invalid {
                message: "未知主题".into(),
            },
            AppearanceApplyOutcome::Failed {
                message: "保存失败".into(),
            },
        ] {
            let receipt = AppearanceApplyReceipt {
                request_id: "request-1".into(),
                outcome,
            };
            let encoded = serde_json::to_value(&receipt).unwrap();
            assert_eq!(
                serde_json::from_value::<AppearanceApplyReceipt>(encoded).unwrap(),
                receipt
            );
        }
    }

    #[test]
    fn app_command_requests_and_receipts_round_trip() {
        let request = AppCommandRequested {
            request_id: "request-1".into(),
            request: AppCommandRequest::Run {
                command: "smelt.app/open-view".into(),
                args: json!({ "view": "hello/main" }),
            },
        };
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(
            encoded,
            json!({
                "request_id": "request-1",
                "request": {
                    "kind": "run",
                    "command": "smelt.app/open-view",
                    "args": { "view": "hello/main" },
                },
            })
        );
        assert_eq!(
            serde_json::from_value::<AppCommandRequested>(encoded).unwrap(),
            request
        );
        // 不带参数的执行读成 `null`。
        assert_eq!(
            serde_json::from_value::<AppCommandRequest>(json!({ "kind": "run", "command": "a/b" }))
                .unwrap(),
            AppCommandRequest::Run {
                command: "a/b".into(),
                args: serde_json::Value::Null,
            }
        );
        for outcome in [
            AppCommandOutcome::Done { value: json!(true) },
            AppCommandOutcome::Failed {
                message: "没有这条命令".into(),
            },
        ] {
            let receipt = AppCommandReceipt {
                request_id: "request-1".into(),
                outcome,
            };
            let encoded = serde_json::to_value(&receipt).unwrap();
            assert_eq!(
                serde_json::from_value::<AppCommandReceipt>(encoded).unwrap(),
                receipt
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
