//! Stable, low-dependency wire contracts shared by Smelt and plugins.

use serde::{Deserialize, Serialize};

/// 插件 bootstrap/control 通道单条 JSON 行的协议上限。20 MiB 原始附件经 base64
/// 膨胀并叠加文本、上下文后仍能容纳；双方读写必须使用同一常量。
pub const PLUGIN_WIRE_MAX_LINE_BYTES: usize = 32 * 1024 * 1024;
use std::{fmt, str::FromStr};

pub const PLUGIN_API_VERSION: u16 = 1;
pub const EVENT_BUS_PROTOCOL_VERSION: u16 = 1;
pub const MIN_SUPPORTED_PLUGIN_API_VERSION: u16 = 1;
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

pub const CORE_CAPABILITY_SESSION_READ: &str = "session.read";
pub const CORE_CAPABILITY_REMOTE_SESSIONS_READ: &str = "remote_sessions.read";
pub const CORE_CAPABILITY_WORKSPACE_READ: &str = "workspace.read";
pub const CORE_CAPABILITY_AUTOMATION_READ: &str = "automation.read";
pub const CORE_CAPABILITY_PROJECT_READ: &str = "project.read";
pub const CORE_CAPABILITY_WORKSPACE_CREATE: &str = "workspace.create";
pub const CORE_CAPABILITY_WORKSPACE_RELEASE: &str = "workspace.release";
pub const CORE_CAPABILITY_AGENT_MESSAGE_READ: &str = "agent.message.read";
pub const CORE_CAPABILITY_UI_CONTRIBUTE: &str = "ui.contribute";
/// 允许插件注册产品级智能体及其会话控制器。ACP provider 仍只负责执行；这项
/// capability 声明的是会话身份、生命周期和宿主 UI 的扩展所有权。
pub const CORE_CAPABILITY_AGENT_CONTRIBUTE: &str = "agent.contribute";
/// 允许插件声明并接收会话输入路由。它与 UI contribution 正交：没有这项
/// capability 的插件不能成为会话消息的发送目标。
pub const CORE_CAPABILITY_SESSION_INPUT_ROUTE: &str = "session.input.route";
/// 允许插件面板导航到插件包以外的网络地址。没有它，面板的 CSP 锁死在包内资源。
/// 这条 capability 只放宽 WebView 的内容来源，不放宽任何宿主 API。
pub const CORE_CAPABILITY_WEB_BROWSE: &str = "web.browse";
/// 允许面板接收宿主的 UI 上下文推送（当前项目根、会话）。
///
/// 当前项目是 GUI 本地概念，插件进程看不到它。没有这条 capability 的面板收不到
/// `panel.context`，因此也做不了任何“项目级”的事。
pub const CORE_CAPABILITY_UI_CONTEXT_READ: &str = "ui.context.read";
/// 允许面板请求宿主弹出原生选择框。页面自己开不了原生对话框，这是必须由宿主
/// 代办的能力，因此单独授权。
pub const CORE_CAPABILITY_UI_DIALOG: &str = "ui.dialog";
/// 允许面板请求在文件管理器里显示某个路径。它不给读文件的能力。
pub const CORE_CAPABILITY_FS_REVEAL: &str = "fs.reveal";

/// 面板页面发给插件的消息统一走这一个 invocation operation。
/// `tool_panel` 和 `workspace_surface` 都收这个 operation，页面不能改走别的入口。
///
/// 不让页面自选 operation：那等于把 manifest 里声明过的调用面完全绕开，
/// 页面（最容易被内容影响的一层）就能点名插件的任意入口。
pub const CORE_INVOCATION_PANEL_MESSAGE: &str = "panel.message";

/// 宿主代办命令（面板页面 -> 宿主 GUI 进程）。
///
/// 它们和 Action（插件进程 -> daemon）是两回事：这些操作本身就发生在 GUI 本地，
/// 绕一圈 daemon 既没有收益，也会让原生对话框失去父窗口。每个命令都由一条
/// capability 单独门控，宿主不按 `plugin_id` 开分支。
pub const CORE_HOST_COMMAND_CONTEXT: &str = "ui.context";
pub const CORE_HOST_COMMAND_PICK_DIRECTORY: &str = "dialog.pick_directory";
pub const CORE_HOST_COMMAND_REVEAL: &str = "shell.reveal";
pub const CORE_HOST_COMMAND_OPEN_EXTERNAL: &str = "shell.open_external";

pub const CORE_ACTION_PROJECT_LIST: &str = "project.list";
pub const CORE_ACTION_PROJECT_RESOLVE_REPOSITORIES: &str = "project.resolve_repositories";
pub const CORE_ACTION_WORKSPACE_CREATE_ISOLATED: &str = "workspace.create_isolated";
pub const CORE_ACTION_WORKSPACE_RELEASE: &str = "workspace.release";

pub const CORE_SNAPSHOT_SESSIONS: &str = "core.sessions.v1";
pub const CORE_SNAPSHOT_REMOTE_SESSIONS: &str = "core.remote_sessions.v1";
pub const CORE_SNAPSHOT_WORKSPACE_MENU: &str = "core.workspace_menu.v1";
pub const CORE_SNAPSHOT_AUTOMATIONS: &str = "core.automations.v1";

pub const FIRST_PARTY_DESKTOP_PLUGIN_ID: &str = "core.desktop";
pub const FIRST_PARTY_REMOTE_GATEWAY_PLUGIN_ID: &str = "core.remote-gateway";

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
string_id!(ActionId);
string_id!(PluginId);
string_id!(SubscriptionId);
string_id!(ContributionId);
string_id!(InvocationId);
string_id!(InvocationOperation);
string_id!(PluginResourceType);
string_id!(PluginResourceId);

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
    ])
}

pub fn core_topic_schema_version(topic: &str) -> u16 {
    if topic == CORE_TOPIC_SESSION_STATE_CHANGED {
        CORE_SESSION_STATE_SCHEMA_VERSION
    } else {
        1
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
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Contribution {
    Command {
        id: ContributionId,
        title: String,
        operation: InvocationOperation,
    },
    /// 工具面板里的一个 tab。宿主只画 tab 本身（标题、选中态），tab 里的内容
    /// 整块交给插件包内的网页，由宿主的 WebView 承载。
    ///
    /// `entry` 是包内相对路径（如 `web/index.html`）。宿主按这个路径的父目录
    /// 建立资源根，页面只能读到该目录以内的文件。
    ToolPanel {
        id: ContributionId,
        title: String,
        entry: String,
    },
    /// 左侧会话列表上方的工作台页面。点击后在中间舞台打开包内网页，
    /// 不占用右侧 Tool Panel。标题可由用户改名，宿主持久化覆盖值。
    WorkspaceSurface {
        id: ContributionId,
        title: String,
        entry: String,
    },
    /// 宿主原生渲染的受限设置页。`snapshot` 必须引用同插件声明的 Command，
    /// 插件只返回稳定 view model，不能注入 GPUI 组件或任意组件树。
    SettingsSection {
        id: ContributionId,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        snapshot: ContributionId,
    },
    /// 将某个 SettingsSection 内的 Account item 投影到侧栏账户位。宿主只显示
    /// 当前已启用插件的投影，并通过 `settings_section` 回到对应插件设置页。
    SidebarAccount {
        id: ContributionId,
        settings_section: ContributionId,
        account_item: ContributionId,
    },
    /// 宿主原生渲染的固定资源创建表单。插件只能提供有界目标列表，并把读取与
    /// 提交分别绑定到同插件已声明的 Command，不能注入组件或动态 operation。
    ResourceCreator {
        id: ContributionId,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        target_label: String,
        target_placeholder: String,
        submit_label: String,
        snapshot: ContributionId,
        submit: ContributionId,
    },
    /// 为插件资源提供受限原生装饰。资源类型和包内 icon 由静态 manifest 固定，
    /// 动态快照只能按 resource id 返回 Badge、tooltip 与 http(s) 外链。
    EntityDecoration {
        id: ContributionId,
        resource_type: PluginResourceType,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        snapshot: ContributionId,
    },
    /// 会话输入的非 UI 路由。宿主按会话绑定选择该 contribution，并把用户输入
    /// 作为 invocation 交给插件；成功只代表远端已接受，不代表 ACP 已开始执行。
    InputRoute {
        id: ContributionId,
        operation: InvocationOperation,
    },
    /// 产品级智能体。它描述“这是什么会话”，不替代 ACP 执行 provider。
    /// 会话行为由同一插件声明的 controller 提供。
    Agent {
        id: ContributionId,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        controller: ContributionId,
    },
    /// 智能体会话的产品控制面。交互输入接到既有 InputRoute，产品动作由引用
    /// controller 的 SessionAction 扩展。宿主始终拥有本地 ACP runtime 的创建、
    /// 停止、重连和回收；插件通过 durable 会话与 agent 事件收敛远端实例生命周期。
    SessionController {
        id: ContributionId,
        input_route: ContributionId,
    },
    /// 由 session controller 提供的通用会话菜单动作。宿主负责标准位置、图标、
    /// 调用与结果处理；插件只声明文案/operation，并根据实例与上下文执行。
    SessionAction {
        id: ContributionId,
        title: String,
        operation: InvocationOperation,
        controller: ContributionId,
        locations: Vec<SessionActionLocation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<SessionActionIcon>,
        #[serde(default)]
        result: SessionActionResult,
    },
}

/// 插件会话附带的后续输入路由。插件身份来自已认证的 action 调用方，
/// 因而这里只保存 contribution 和无凭据上下文，不允许插件自行填写 plugin id。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInputRouteBinding {
    pub contribution_id: ContributionId,
    #[serde(default)]
    pub context: serde_json::Value,
}

/// 守护调用 `InputRoute` contribution 时使用的稳定 payload。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationInputImage {
    pub mime: String,
    pub data_base64: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputRouteInvocationPayload {
    #[serde(default)]
    pub submission_id: String,
    pub context: serde_json::Value,
    /// 仅首轮提供的智能体预设。它与用户正文分层，controller 可将其映射为
    /// 系统提示、策略模板或远端会话配置，不能由宿主拼进用户评论。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_preset: Option<String>,
    pub text: String,
    #[serde(default)]
    pub images: Vec<ConversationInputImage>,
}

/// 可选 UI sidecar 的固定文件名。基础 `plugin.json` 是旧安装器也会读取的升级
/// 契约，新增 contribution 类型必须放在 sidecar，不能再扩展基础 manifest 的枚举值。
pub const PLUGIN_UI_MANIFEST_FILE: &str = "plugin-ui.json";

/// 可选输入路由 sidecar。旧安装器的 `Contribution` 枚举没有 `input_route`，
/// 候选 App 会被正在运行的旧二进制扫描，所以这条声明不能写进基础 `plugin.json`。
pub const PLUGIN_INPUT_MANIFEST_FILE: &str = "plugin-input.json";

/// 可选智能体 sidecar。与 UI/input sidecar 一样，它不会被只认识旧 Contribution
/// 枚举的安装器读取。
pub const PLUGIN_AGENT_MANIFEST_FILE: &str = "plugin-agent.json";

/// 插件 UI 声明与基础 manifest 分开演进。顶层保留宽松解析，新增元数据不会让
/// 整份 sidecar 失效；未知 contribution 也会被跳过。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginUiManifest {
    #[serde(default)]
    pub contributions: Vec<UiContribution>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiContribution {
    ToolPanel {
        id: ContributionId,
        title: String,
        entry: String,
    },
    WorkspaceSurface {
        id: ContributionId,
        title: String,
        entry: String,
    },
    SettingsSection {
        id: ContributionId,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        snapshot: ContributionId,
    },
    SidebarAccount {
        id: ContributionId,
        settings_section: ContributionId,
        account_item: ContributionId,
    },
    ResourceCreator {
        id: ContributionId,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        target_label: String,
        target_placeholder: String,
        submit_label: String,
        snapshot: ContributionId,
        submit: ContributionId,
    },
    EntityDecoration {
        id: ContributionId,
        resource_type: PluginResourceType,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        snapshot: ContributionId,
    },
    /// 新版 sidecar 可以加入宿主尚不认识的 UI 类型。旧宿主忽略这一条，仍加载
    /// 插件主体和同文件里的其他已知 contribution。
    #[serde(other)]
    Unknown,
}

impl PluginUiManifest {
    pub fn known_contributions(&self) -> impl Iterator<Item = Contribution> + '_ {
        self.contributions
            .iter()
            .filter_map(|contribution| match contribution {
                UiContribution::ToolPanel { id, title, entry } => Some(Contribution::ToolPanel {
                    id: id.clone(),
                    title: title.clone(),
                    entry: entry.clone(),
                }),
                UiContribution::WorkspaceSurface { id, title, entry } => {
                    Some(Contribution::WorkspaceSurface {
                        id: id.clone(),
                        title: title.clone(),
                        entry: entry.clone(),
                    })
                }
                UiContribution::SettingsSection {
                    id,
                    title,
                    description,
                    snapshot,
                } => Some(Contribution::SettingsSection {
                    id: id.clone(),
                    title: title.clone(),
                    description: description.clone(),
                    snapshot: snapshot.clone(),
                }),
                UiContribution::SidebarAccount {
                    id,
                    settings_section,
                    account_item,
                } => Some(Contribution::SidebarAccount {
                    id: id.clone(),
                    settings_section: settings_section.clone(),
                    account_item: account_item.clone(),
                }),
                UiContribution::ResourceCreator {
                    id,
                    title,
                    description,
                    target_label,
                    target_placeholder,
                    submit_label,
                    snapshot,
                    submit,
                } => Some(Contribution::ResourceCreator {
                    id: id.clone(),
                    title: title.clone(),
                    description: description.clone(),
                    target_label: target_label.clone(),
                    target_placeholder: target_placeholder.clone(),
                    submit_label: submit_label.clone(),
                    snapshot: snapshot.clone(),
                    submit: submit.clone(),
                }),
                UiContribution::EntityDecoration {
                    id,
                    resource_type,
                    icon,
                    snapshot,
                } => Some(Contribution::EntityDecoration {
                    id: id.clone(),
                    resource_type: resource_type.clone(),
                    icon: icon.clone(),
                    snapshot: snapshot.clone(),
                }),
                UiContribution::Unknown => None,
            })
    }
}

const MAX_SETTINGS_ITEMS: usize = 64;
const MAX_SETTINGS_ACTIONS: usize = 64;
const MAX_SETTINGS_ACTION_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_RESOURCE_CREATOR_TARGETS: usize = 128;
const MAX_RESOURCE_DESCRIPTION_BYTES: usize = 16 * 1024;
const MAX_ENTITY_DECORATIONS: usize = 512;

fn empty_json_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// 设置项触发的同插件 Command。宿主从已认证 manifest 解析 operation，插件返回的
/// view model 无权自选 operation 或目标 plugin。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsActionView {
    pub id: ContributionId,
    #[serde(default)]
    pub label: String,
    pub command: ContributionId,
    #[serde(default = "empty_json_object")]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub style: SettingsActionStyle,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsActionStyle {
    #[default]
    Default,
    Primary,
    Danger,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsTone {
    #[default]
    Neutral,
    Positive,
    Warning,
    Negative,
}

/// 头像字节通过单独的 Command 按 revision 缓存，避免设置快照每两秒重复携带
/// 数 MiB base64。Command 结果使用 [`SettingsAvatarData`]。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsAvatarRef {
    pub command: ContributionId,
    pub revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsAvatarData {
    pub mime: String,
    pub data_base64: String,
}

/// 第一版设置 renderer 只覆盖现有真实需求，不接受任意嵌套组件树。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SettingsItemView {
    Header {
        id: ContributionId,
        title: String,
    },
    Account {
        id: ContributionId,
        title: String,
        #[serde(default)]
        signed_in: bool,
        #[serde(default)]
        display_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default)]
        tone: SettingsTone,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        avatar: Option<SettingsAvatarRef>,
        #[serde(default)]
        actions: Vec<SettingsActionView>,
    },
    Status {
        id: ContributionId,
        title: String,
        text: String,
        #[serde(default)]
        tone: SettingsTone,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default)]
        actions: Vec<SettingsActionView>,
    },
    Text {
        id: ContributionId,
        title: String,
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default)]
        copyable: bool,
    },
    /// 宿主点击后把新布尔值写入 action payload 的 `value` 字段；若静态 payload
    /// 已含同名字段，以宿主生成的值为准。
    Toggle {
        id: ContributionId,
        title: String,
        value: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        action: SettingsActionView,
        #[serde(default)]
        disabled: bool,
    },
    Actions {
        id: ContributionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        actions: Vec<SettingsActionView>,
    },
}

impl SettingsItemView {
    pub fn id(&self) -> &ContributionId {
        match self {
            Self::Header { id, .. }
            | Self::Account { id, .. }
            | Self::Status { id, .. }
            | Self::Text { id, .. }
            | Self::Toggle { id, .. }
            | Self::Actions { id, .. } => id,
        }
    }

    pub fn actions(&self) -> &[SettingsActionView] {
        match self {
            Self::Account { actions, .. }
            | Self::Status { actions, .. }
            | Self::Actions { actions, .. } => actions,
            Self::Toggle { action, .. } => std::slice::from_ref(action),
            Self::Header { .. } | Self::Text { .. } => &[],
        }
    }
}

/// 插件返回给宿主和 UI 的脱敏展示投影。这里只能包含可直接展示给当前用户的值；
/// token、cookie、私有文件路径等秘密不得放入 item、description 或 action payload。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsSectionView {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub items: Vec<SettingsItemView>,
}

impl SettingsSectionView {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.items.len() > MAX_SETTINGS_ITEMS {
            return Err(ValidationError::new(
                "settings view contains too many items",
            ));
        }
        let mut item_ids = std::collections::BTreeSet::new();
        let mut action_ids = std::collections::BTreeSet::new();
        let mut action_count = 0usize;
        for item in &self.items {
            if !item_ids.insert(item.id().clone()) {
                return Err(ValidationError::new(
                    "settings view contains duplicate item ids",
                ));
            }
            validate_settings_item(item)?;
            for action in item.actions() {
                action_count += 1;
                if action_count > MAX_SETTINGS_ACTIONS {
                    return Err(ValidationError::new(
                        "settings view contains too many actions",
                    ));
                }
                if !action_ids.insert(action.id.clone()) {
                    return Err(ValidationError::new(
                        "settings view contains duplicate action ids",
                    ));
                }
                validate_settings_action(action, !matches!(item, SettingsItemView::Toggle { .. }))?;
            }
        }
        Ok(())
    }
}

fn validate_optional_settings_text(
    label: &str,
    value: Option<&str>,
    max_len: usize,
) -> Result<(), ValidationError> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.len() > max_len || value.chars().any(char::is_control) {
        return Err(ValidationError::new(format!("{label} is invalid")));
    }
    Ok(())
}

fn validate_settings_item(item: &SettingsItemView) -> Result<(), ValidationError> {
    let title = match item {
        SettingsItemView::Header { title, .. }
        | SettingsItemView::Account { title, .. }
        | SettingsItemView::Status { title, .. }
        | SettingsItemView::Text { title, .. }
        | SettingsItemView::Toggle { title, .. } => Some(title.as_str()),
        SettingsItemView::Actions { title, .. } => title.as_deref(),
    };
    if let Some(title) = title {
        validate_display_text("settings item title", title, 128)?;
    }
    match item {
        SettingsItemView::Account {
            signed_in,
            display_name,
            detail,
            message,
            avatar,
            ..
        } => {
            if *signed_in && display_name.is_empty() {
                return Err(ValidationError::new(
                    "signed-in settings account requires a display name",
                ));
            }
            validate_optional_settings_text(
                "settings account display name",
                (!display_name.is_empty()).then_some(display_name.as_str()),
                256,
            )?;
            validate_optional_settings_text("settings account detail", detail.as_deref(), 512)?;
            validate_optional_settings_text("settings account message", message.as_deref(), 1024)?;
            if let Some(avatar) = avatar
                && (avatar.revision.is_empty()
                    || avatar.revision.len() > 128
                    || avatar.revision.chars().any(char::is_control))
            {
                return Err(ValidationError::new("settings avatar revision is invalid"));
            }
        }
        SettingsItemView::Status {
            text, description, ..
        } => {
            validate_optional_settings_text("settings status", Some(text), 1024)?;
            validate_optional_settings_text(
                "settings item description",
                description.as_deref(),
                2048,
            )?;
        }
        SettingsItemView::Text {
            value, description, ..
        } => {
            validate_optional_settings_text("settings text value", Some(value), 4096)?;
            validate_optional_settings_text(
                "settings item description",
                description.as_deref(),
                2048,
            )?;
        }
        SettingsItemView::Toggle { description, .. }
        | SettingsItemView::Actions { description, .. } => validate_optional_settings_text(
            "settings item description",
            description.as_deref(),
            2048,
        )?,
        SettingsItemView::Header { .. } => {}
    }
    Ok(())
}

fn validate_settings_action(
    action: &SettingsActionView,
    require_label: bool,
) -> Result<(), ValidationError> {
    if require_label {
        validate_display_text("settings action label", &action.label, 128)?;
    } else {
        validate_optional_settings_text(
            "settings action label",
            (!action.label.is_empty()).then_some(action.label.as_str()),
            128,
        )?;
    }
    if !action.payload.is_object() {
        return Err(ValidationError::new(
            "settings action payload must be an object",
        ));
    }
    let payload_len = serde_json::to_vec(&action.payload)
        .map_err(|_| ValidationError::new("settings action payload cannot be encoded"))?
        .len();
    if payload_len > MAX_SETTINGS_ACTION_PAYLOAD_BYTES {
        return Err(ValidationError::new(
            "settings action payload exceeds the size limit",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceCreatorTargetView {
    pub id: String,
    pub label: String,
}

/// 插件返回的脱敏目标投影。目标 ID 可以是远端 UUID，但不得包含凭据、私有路径
/// 或其他宿主无需显示和提交的数据。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceCreatorView {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_message: Option<String>,
    #[serde(default)]
    pub targets: Vec<ResourceCreatorTargetView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_target_id: Option<String>,
}

impl ResourceCreatorView {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.targets.len() > MAX_RESOURCE_CREATOR_TARGETS {
            return Err(ValidationError::new(
                "resource creator contains too many targets",
            ));
        }
        if self.available && self.targets.is_empty() {
            return Err(ValidationError::new(
                "available resource creator requires at least one target",
            ));
        }
        if !self.available {
            if !self.targets.is_empty() || self.default_target_id.is_some() {
                return Err(ValidationError::new(
                    "unavailable resource creator cannot expose targets",
                ));
            }
            validate_display_text(
                "resource creator unavailable message",
                self.unavailable_message.as_deref().unwrap_or_default(),
                1024,
            )?;
        } else {
            validate_optional_settings_text(
                "resource creator unavailable message",
                self.unavailable_message.as_deref(),
                1024,
            )?;
        }
        let mut ids = std::collections::BTreeSet::new();
        for target in &self.targets {
            validate_resource_id("resource creator target id", &target.id)?;
            validate_display_text("resource creator target label", &target.label, 256)?;
            if !ids.insert(target.id.as_str()) {
                return Err(ValidationError::new(
                    "resource creator contains duplicate target ids",
                ));
            }
        }
        if let Some(default_target_id) = &self.default_target_id
            && !ids.contains(default_target_id.as_str())
        {
            return Err(ValidationError::new(
                "resource creator default target is not declared",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceCreateParams {
    /// 宿主为一次表单提交生成并在重试间保持不变的幂等标识。
    pub submission_id: String,
    pub target_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl ResourceCreateParams {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_resource_id("resource create submission id", &self.submission_id)?;
        validate_resource_id("resource create target id", &self.target_id)?;
        validate_display_text("resource create title", &self.title, 512)?;
        if let Some(description) = &self.description {
            validate_multiline_text(
                "resource create description",
                description,
                MAX_RESOURCE_DESCRIPTION_BYTES,
            )?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceCreateResult {
    pub resource_id: String,
    pub message: String,
}

impl ResourceCreateResult {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_resource_id("created resource id", &self.resource_id)?;
        validate_display_text("resource create result message", &self.message, 1024)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityDecorationTone {
    #[default]
    Neutral,
    Accent,
    Positive,
    Warning,
    Negative,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityDecorationItemView {
    pub resource_id: PluginResourceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    #[serde(default)]
    pub tone: EntityDecorationTone,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tooltip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_url: Option<String>,
}

/// 插件资源的有界脱敏展示快照。宿主将它与核心保存的 [`PluginResourceRef`]
/// 关联；插件不能在快照中指定另一个插件或资源类型。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityDecorationView {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub items: Vec<EntityDecorationItemView>,
}

impl EntityDecorationView {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.items.len() > MAX_ENTITY_DECORATIONS {
            return Err(ValidationError::new(
                "entity decoration view contains too many items",
            ));
        }
        let mut resource_ids = std::collections::BTreeSet::new();
        for item in &self.items {
            if !resource_ids.insert(item.resource_id.clone()) {
                return Err(ValidationError::new(
                    "entity decoration view contains duplicate resource ids",
                ));
            }
            if let Some(badge) = &item.badge {
                validate_display_text("entity decoration badge", badge, 32)?;
            }
            if let Some(tooltip) = &item.tooltip {
                validate_display_text("entity decoration tooltip", tooltip, 1024)?;
            }
            if let Some(url) = &item.external_url
                && (url.len() > 2048
                    || url.chars().any(char::is_control)
                    || !(url.starts_with("https://") || url.starts_with("http://")))
            {
                return Err(ValidationError::new(
                    "entity decoration external URL is invalid",
                ));
            }
        }
        Ok(())
    }
}

fn validate_resource_id(label: &str, value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty()
        || value.trim() != value
        || value.len() > 256
        || value.chars().any(char::is_control)
    {
        return Err(ValidationError::new(format!("{label} is invalid")));
    }
    Ok(())
}

fn validate_multiline_text(
    label: &str,
    value: &str,
    max_bytes: usize,
) -> Result<(), ValidationError> {
    if value.len() > max_bytes
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(ValidationError::new(format!("{label} is invalid")));
    }
    Ok(())
}

/// 输入路由与基础 manifest 分开演进。未知类型按条目跳过，避免 sidecar 里的
/// 新路由声明让旧宿主拒绝整个插件包。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInputManifest {
    #[serde(default)]
    pub contributions: Vec<InputContribution>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputContribution {
    InputRoute {
        id: ContributionId,
        operation: InvocationOperation,
    },
    #[serde(other)]
    Unknown,
}

impl PluginInputManifest {
    pub fn known_contributions(&self) -> impl Iterator<Item = Contribution> + '_ {
        self.contributions
            .iter()
            .filter_map(|contribution| match contribution {
                InputContribution::InputRoute { id, operation } => Some(Contribution::InputRoute {
                    id: id.clone(),
                    operation: operation.clone(),
                }),
                InputContribution::Unknown => None,
            })
    }
}

/// 产品级智能体及其会话控制器声明。未知类型按条目跳过，以便后续增加新的
/// controller UI contribution 时旧宿主仍可加载插件主体。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginAgentManifest {
    #[serde(default)]
    pub contributions: Vec<AgentContribution>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionActionLocation {
    SessionMenu,
    ProjectMenu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionActionIcon {
    Check,
    ExternalLink,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionActionResult {
    #[default]
    Ignore,
    /// Invocation 成功结果必须是 `{ "url": "https://..." }`；宿主校验协议后打开。
    OpenExternal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentContribution {
    Agent {
        id: ContributionId,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        controller: ContributionId,
    },
    SessionController {
        id: ContributionId,
        input_route: ContributionId,
    },
    SessionAction {
        id: ContributionId,
        title: String,
        operation: InvocationOperation,
        controller: ContributionId,
        locations: Vec<SessionActionLocation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<SessionActionIcon>,
        #[serde(default)]
        result: SessionActionResult,
    },
    #[serde(other)]
    Unknown,
}

impl PluginAgentManifest {
    pub fn known_contributions(&self) -> impl Iterator<Item = Contribution> + '_ {
        self.contributions
            .iter()
            .filter_map(|contribution| match contribution {
                AgentContribution::Agent {
                    id,
                    name,
                    icon,
                    controller,
                } => Some(Contribution::Agent {
                    id: id.clone(),
                    name: name.clone(),
                    icon: icon.clone(),
                    controller: controller.clone(),
                }),
                AgentContribution::SessionController { id, input_route } => {
                    Some(Contribution::SessionController {
                        id: id.clone(),
                        input_route: input_route.clone(),
                    })
                }
                AgentContribution::SessionAction {
                    id,
                    title,
                    operation,
                    controller,
                    locations,
                    icon,
                    result,
                } => Some(Contribution::SessionAction {
                    id: id.clone(),
                    title: title.clone(),
                    operation: operation.clone(),
                    controller: controller.clone(),
                    locations: locations.clone(),
                    icon: *icon,
                    result: *result,
                }),
                AgentContribution::Unknown => None,
            })
    }
}

impl Contribution {
    pub fn id(&self) -> &ContributionId {
        match self {
            Self::Command { id, .. }
            | Self::ToolPanel { id, .. }
            | Self::WorkspaceSurface { id, .. }
            | Self::SettingsSection { id, .. }
            | Self::SidebarAccount { id, .. }
            | Self::ResourceCreator { id, .. }
            | Self::EntityDecoration { id, .. }
            | Self::InputRoute { id, .. }
            | Self::Agent { id, .. }
            | Self::SessionController { id, .. }
            | Self::SessionAction { id, .. } => id,
        }
    }

    fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Command { title, .. } => validate_display_text("command title", title, 128),
            Self::ToolPanel { title, entry, .. } => {
                validate_display_text("tool panel title", title, 64)?;
                validate_package_relative_path("tool panel entry", entry)
            }
            Self::WorkspaceSurface { title, entry, .. } => {
                validate_display_text("workspace surface title", title, 64)?;
                validate_package_relative_path("workspace surface entry", entry)
            }
            Self::SettingsSection {
                title, description, ..
            } => {
                validate_display_text("settings section title", title, 128)?;
                validate_optional_settings_text(
                    "settings section description",
                    description.as_deref(),
                    2048,
                )
            }
            Self::SidebarAccount { .. } | Self::InputRoute { .. } => Ok(()),
            Self::ResourceCreator {
                title,
                description,
                target_label,
                target_placeholder,
                submit_label,
                ..
            } => {
                validate_display_text("resource creator title", title, 128)?;
                validate_optional_settings_text(
                    "resource creator description",
                    description.as_deref(),
                    2048,
                )?;
                validate_display_text("resource creator target label", target_label, 128)?;
                validate_display_text(
                    "resource creator target placeholder",
                    target_placeholder,
                    256,
                )?;
                validate_display_text("resource creator submit label", submit_label, 128)
            }
            Self::EntityDecoration { icon, .. } => {
                if let Some(icon) = icon {
                    validate_package_relative_path("entity decoration icon", icon)?;
                }
                Ok(())
            }
            Self::Agent { name, icon, .. } => {
                validate_display_text("agent name", name, 128)?;
                if let Some(icon) = icon {
                    validate_package_relative_path("agent icon", icon)?;
                }
                Ok(())
            }
            Self::SessionController { .. } => Ok(()),
            Self::SessionAction {
                title, locations, ..
            } => {
                validate_display_text("session action title", title, 128)?;
                if locations.is_empty()
                    || locations
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        != locations.len()
                {
                    return Err(ValidationError::new(
                        "session action locations must be non-empty and unique",
                    ));
                }
                Ok(())
            }
        }
    }
}

/// 校验一个"包内相对路径"。与 `validate_plugin_entrypoint` 同样的规则，但不
/// 强制 `bin/` 前缀：Web 资源住在插件自己选的目录下。
///
/// 这里只挡明显越界的写法；真正的边界由宿主在读文件时用 canonicalize 兜住，
/// 因为 manifest 是插件提供的数据，不能作为安全依据。
fn validate_package_relative_path(label: &str, value: &str) -> Result<(), ValidationError> {
    let invalid = value.is_empty()
        || value.len() > 1024
        || value.starts_with('/')
        || value.contains('\\')
        || value.split('/').any(|segment| {
            segment.is_empty()
                || matches!(segment, "." | "..")
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        });
    if invalid {
        return Err(ValidationError::new(format!("{label} is invalid")));
    }
    Ok(())
}

fn validate_display_text(
    label: &str,
    value: &str,
    max_bytes: usize,
) -> Result<(), ValidationError> {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(ValidationError::new(format!("{label} is invalid")));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct IgnoredManifestField;

impl<'de> Deserialize<'de> for IgnoredManifestField {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(Self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub id: PluginId,
    pub name: String,
    pub version: String,
    pub api_version: u16,
    pub entrypoint: String,
    /// 已废弃。历史上用来区分 bun/wasm。解码时丢掉，不参与行为，也不写回。
    #[serde(default, skip_serializing)]
    runtime: IgnoredManifestField,
    #[serde(default)]
    pub subscriptions: Vec<SubscriptionDeclaration>,
    #[serde(default)]
    pub publishes: Vec<Topic>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub contributions: Vec<Contribution>,
    /// 是否随宿主一起分发。测试用插件设 `false`，打包和开发期 staging 都会
    /// 跳过它——否则一个只为跑测试而存在的插件会被装进用户的产物里。
    #[serde(default = "default_bundled")]
    pub bundled: bool,
}

fn default_bundled() -> bool {
    true
}

impl PluginManifest {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.api_version != PLUGIN_API_VERSION {
            return Err(ValidationError::new("unsupported plugin api_version"));
        }
        if self.name.trim().is_empty()
            || self.name.len() > 128
            || self.name.chars().any(char::is_control)
        {
            return Err(ValidationError::new("plugin name is invalid"));
        }
        if self.version.trim().is_empty()
            || self.version.len() > 64
            || self.version.chars().any(char::is_control)
        {
            return Err(ValidationError::new("plugin version cannot be empty"));
        }
        if !self.subscriptions.is_empty() || !self.publishes.is_empty() {
            return Err(ValidationError::new(
                "shared plugins do not yet support subscriptions or published topics",
            ));
        }
        validate_plugin_entrypoint(&self.entrypoint)?;
        let declared_capabilities = self
            .capabilities
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        if declared_capabilities.len() != self.capabilities.len() {
            return Err(ValidationError::new("duplicate capability"));
        }
        if self.contributions.iter().any(|contribution| {
            matches!(
                contribution,
                Contribution::Command { .. }
                    | Contribution::ToolPanel { .. }
                    | Contribution::WorkspaceSurface { .. }
                    | Contribution::SettingsSection { .. }
                    | Contribution::SidebarAccount { .. }
                    | Contribution::ResourceCreator { .. }
                    | Contribution::EntityDecoration { .. }
                    | Contribution::SessionAction { .. }
            )
        }) && !declared_capabilities.contains(
            &Capability::new(CORE_CAPABILITY_UI_CONTRIBUTE)
                .expect("core capability constant is valid"),
        ) {
            return Err(ValidationError::new(
                "UI contributions require ui.contribute capability",
            ));
        }
        if self
            .contributions
            .iter()
            .any(|contribution| matches!(contribution, Contribution::InputRoute { .. }))
            && !declared_capabilities.contains(
                &Capability::new(CORE_CAPABILITY_SESSION_INPUT_ROUTE)
                    .expect("core capability constant is valid"),
            )
        {
            return Err(ValidationError::new(
                "input route contributions require session.input.route capability",
            ));
        }
        if self.contributions.iter().any(|contribution| {
            matches!(
                contribution,
                Contribution::Agent { .. }
                    | Contribution::SessionController { .. }
                    | Contribution::SessionAction { .. }
            )
        }) && !declared_capabilities.contains(
            &Capability::new(CORE_CAPABILITY_AGENT_CONTRIBUTE)
                .expect("core capability constant is valid"),
        ) {
            return Err(ValidationError::new(
                "agent contributions require agent.contribute capability",
            ));
        }
        let mut contribution_ids = std::collections::BTreeSet::new();
        for contribution in &self.contributions {
            contribution.validate()?;
            if !contribution_ids.insert(contribution.id().clone()) {
                return Err(ValidationError::new("duplicate contribution id"));
            }
        }
        for contribution in &self.contributions {
            match contribution {
                Contribution::Agent { controller, .. } => {
                    if !self.contributions.iter().any(|candidate| {
                        matches!(
                            candidate,
                            Contribution::SessionController { id, .. } if id == controller
                        )
                    }) {
                        return Err(ValidationError::new(
                            "agent references an undeclared session controller",
                        ));
                    }
                }
                Contribution::SessionController { input_route, .. } => {
                    if !self.contributions.iter().any(|candidate| {
                        matches!(candidate, Contribution::InputRoute { id, .. } if id == input_route)
                    }) {
                        return Err(ValidationError::new(
                            "session controller references an undeclared input route",
                        ));
                    }
                }
                Contribution::SessionAction { controller, .. } => {
                    if !self.contributions.iter().any(|candidate| {
                        matches!(
                            candidate,
                            Contribution::SessionController { id, .. } if id == controller
                        )
                    }) {
                        return Err(ValidationError::new(
                            "session action references an undeclared session controller",
                        ));
                    }
                }
                Contribution::SettingsSection { snapshot, .. } => {
                    if !self.contributions.iter().any(|candidate| {
                        matches!(candidate, Contribution::Command { id, .. } if id == snapshot)
                    }) {
                        return Err(ValidationError::new(
                            "settings section references an undeclared snapshot command",
                        ));
                    }
                }
                Contribution::SidebarAccount {
                    settings_section, ..
                } if !self.contributions.iter().any(|candidate| {
                    matches!(
                        candidate,
                        Contribution::SettingsSection { id, .. } if id == settings_section
                    )
                }) => {
                    return Err(ValidationError::new(
                        "sidebar account references an undeclared settings section",
                    ));
                }
                Contribution::ResourceCreator {
                    snapshot, submit, ..
                } => {
                    if !self.contributions.iter().any(|candidate| {
                        matches!(candidate, Contribution::Command { id, .. } if id == snapshot)
                    }) {
                        return Err(ValidationError::new(
                            "resource creator references an undeclared snapshot command",
                        ));
                    }
                    if !self.contributions.iter().any(|candidate| {
                        matches!(candidate, Contribution::Command { id, .. } if id == submit)
                    }) {
                        return Err(ValidationError::new(
                            "resource creator references an undeclared submit command",
                        ));
                    }
                }
                Contribution::EntityDecoration { snapshot, .. }
                    if !self.contributions.iter().any(|candidate| {
                        matches!(candidate, Contribution::Command { id, .. } if id == snapshot)
                    }) =>
                {
                    return Err(ValidationError::new(
                        "entity decoration references an undeclared snapshot command",
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn validate_plugin_entrypoint(entrypoint: &str) -> Result<(), ValidationError> {
    let directory = "bin/";
    let Some(relative) = entrypoint.strip_prefix(directory) else {
        return Err(ValidationError::new(format!(
            "plugin entrypoint must be inside the package {} directory",
            directory.trim_end_matches('/')
        )));
    };
    if entrypoint.len() > 1024
        || relative.is_empty()
        || entrypoint.contains('\\')
        || relative.split('/').any(|segment| {
            segment.is_empty()
                || matches!(segment, "." | "..")
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
    {
        return Err(ValidationError::new("plugin entrypoint is invalid"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolRange {
    pub min: u16,
    pub max: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationRequest<P = serde_json::Value> {
    pub invocation_id: InvocationId,
    pub contribution_id: ContributionId,
    pub operation: InvocationOperation,
    pub payload: P,
    pub deadline_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationErrorCode {
    InvalidRequest,
    Rejected,
    Conflict,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum InvocationResponse<R = serde_json::Value, D = serde_json::Value> {
    Success {
        invocation_id: InvocationId,
        result: R,
    },
    Error {
        invocation_id: InvocationId,
        code: InvocationErrorCode,
        message: String,
        #[serde(default)]
        retryable: bool,
        #[serde(default)]
        details: Option<D>,
    },
}

impl<R, D> InvocationResponse<R, D> {
    pub fn invocation_id(&self) -> &InvocationId {
        match self {
            Self::Success { invocation_id, .. } | Self::Error { invocation_id, .. } => {
                invocation_id
            }
        }
    }
}

impl ProtocolRange {
    pub fn supports(&self, version: u16) -> bool {
        self.min <= self.max && self.min <= version && self.max >= version
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginContributionSet {
    pub plugin_id: PluginId,
    pub name: String,
    pub version: String,
    pub contributions: Vec<Contribution>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstPartyClientKind {
    Desktop,
    RemoteGateway,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventClientAuth {
    FirstParty { kind: FirstPartyClientKind },
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRequest<P = serde_json::Value> {
    pub command_id: CommandId,
    pub action: ActionId,
    pub api_version: u16,
    pub correlation_id: CorrelationId,
    pub causation_id: Option<EventId>,
    pub params: P,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionResponse<R = serde_json::Value> {
    pub command_id: CommandId,
    pub result: R,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionErrorCode {
    Unauthorized,
    UnsupportedVersion,
    UnknownAction,
    InvalidParams,
    NotFound,
    Conflict,
    RateLimited,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionError<D = serde_json::Value> {
    pub command_id: CommandId,
    pub code: ActionErrorCode,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default)]
    pub details: Option<D>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionReply {
    Success {
        response: ActionResponse,
    },
    Error {
        error: ActionError,
    },
    ProtocolError {
        code: ActionErrorCode,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginResourceRef {
    pub plugin_id: PluginId,
    pub resource_type: PluginResourceType,
    pub resource_id: PluginResourceId,
}

/// 全局唯一的 contribution 引用。声明文件内部可以使用局部 id；一旦进入
/// 会话投影就必须带上插件命名空间，避免核心根据某个具体插件反推所有者。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginContributionRef {
    pub plugin_id: PluginId,
    pub contribution_id: ContributionId,
}

/// 已由宿主解析并认证的智能体会话绑定。`agent` 是产品身份，`controller` 是
/// 生命周期所有者，`instance` 是该控制器管理的远端/本地实例。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSessionBinding {
    pub agent: PluginContributionRef,
    pub controller: PluginContributionRef,
    pub instance: PluginResourceRef,
}

/// 宿主调用 SessionAction 时提供的稳定上下文。`agent_session` 证明动作作用于哪个
/// controller/实例；`context` 是同插件输入路由的无凭据上下文，不暴露核心类型。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionActionInvocationPayload {
    pub agent_session: AgentSessionBinding,
    #[serde(default)]
    pub context: serde_json::Value,
}

/// 插件提交的局部智能体绑定。插件 id 由认证身份补齐；输入上下文由
/// controller 声明的 InputRoute 承载，调用方不能自行指定另一个 controller。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginAgentSessionSpec {
    pub agent_id: ContributionId,
    pub instance: PluginResourceRef,
    #[serde(default)]
    pub context: serde_json::Value,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectListParams {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRepository {
    pub path: String,
    pub remote_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectDescriptor {
    pub root: String,
    pub title: String,
    pub repositories: Vec<ProjectRepository>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectListResult {
    pub projects: Vec<ProjectDescriptor>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectResolveRepositoriesParams {
    #[serde(default)]
    pub repository_urls: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectResolveRepositoriesResult {
    pub project_root: String,
    pub repositories: Vec<ProjectRepository>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCreateIsolatedParams {
    pub owner: PluginResourceRef,
    #[serde(default)]
    pub repository_urls: Vec<String>,
    pub branch_label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsolatedWorkspaceRepository {
    pub remote_key: String,
    pub worktree_dir: String,
    pub branch_name: String,
    pub base_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsolatedWorkspaceDescriptor {
    pub id: String,
    pub owner: PluginResourceRef,
    pub branch_label: String,
    pub workspace_dir: String,
    pub repositories: Vec<IsolatedWorkspaceRepository>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCreateIsolatedResult {
    pub workspace: IsolatedWorkspaceDescriptor,
    pub created: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceReleaseParams {
    pub owner: PluginResourceRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceReleaseResult {
    pub released: bool,
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
    fn plugin_manifest_accepts_current_required_fields() {
        let manifest: PluginManifest = serde_json::from_value(json!({
            "id": "com.example",
            "name": "Example",
            "version": "1.0.0",
            "api_version": 1,
            "entrypoint": "bin/example.ts"
        }))
        .unwrap();
        manifest.validate().unwrap();
        assert!(manifest.capabilities.is_empty());
        assert!(manifest.subscriptions.is_empty());
        assert!(manifest.publishes.is_empty());
    }

    #[test]
    fn manifest_ignores_runtime_field() {
        let manifest: PluginManifest = serde_json::from_value(json!({
            "id": "com.example",
            "name": "Example",
            "version": "1.0.0",
            "api_version": 1,
            "runtime": "wasm",
            "entrypoint": "bin/example.ts"
        }))
        .unwrap();
        manifest.validate().unwrap();
        assert!(
            !serde_json::to_value(&manifest)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("runtime")
        );

        let error = serde_json::from_value::<PluginManifest>(json!({
            "id": "com.example",
            "name": "Example",
            "version": "1.0.0",
            "api_version": 1,
            "runtime": "wasm",
            "entrypoint": "module/example.wasm"
        }))
        .and_then(|manifest| manifest.validate().map_err(serde::de::Error::custom))
        .unwrap_err();
        assert!(error.to_string().contains("bin"));
    }

    #[test]
    fn manifest_decoding_rejects_obsolete_execution_modes() {
        for execution in ["shared", "dedicated"] {
            let error = serde_json::from_value::<PluginManifest>(json!({
                "id": "com.example.obsolete-execution",
                "name": "Bun",
                "version": "1.0.0",
                "api_version": 1,
                "execution": execution,
                "entrypoint": "bin/example.ts"
            }))
            .unwrap_err();
            assert!(error.to_string().contains("unknown field `execution`"));
        }
    }

    #[test]
    fn wire_ids_and_capabilities_are_validated_on_decode() {
        assert!(serde_json::from_str::<PluginId>(r#""bad id""#).is_err());
        assert!(serde_json::from_str::<Capability>(r#""Task.Read""#).is_err());
    }

    #[test]
    fn shared_manifests_reject_published_topics() {
        let manifest: PluginManifest = serde_json::from_value(json!({
            "id": "com.example",
            "name": "Example",
            "version": "1.0.0",
            "api_version": 1,
            "entrypoint": "bin/example",
            "publishes": ["plugin.com.other.changed"]
        }))
        .unwrap();
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn manifest_requires_install_metadata() {
        assert!(
            serde_json::from_value::<PluginManifest>(json!({
                "id": "com.example",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": "bin/example"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<PluginManifest>(json!({
                "id": "com.example",
                "name": "Example",
                "version": "1.0.0",
                "api_version": 1
            }))
            .is_err()
        );
    }

    #[test]
    fn manifest_rejects_unsafe_entrypoints_and_duplicate_declarations() {
        for entrypoint in ["/tmp/plugin", "../plugin", "bin/../plugin", "plugin"] {
            let manifest: PluginManifest = serde_json::from_value(json!({
                "id": "com.example",
                "name": "Example",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": entrypoint
            }))
            .unwrap();
            assert!(manifest.validate().is_err(), "accepted {entrypoint}");
        }

        let duplicate_capability: PluginManifest = serde_json::from_value(json!({
            "id": "com.example",
            "name": "Example",
            "version": "1.0.0",
            "api_version": 1,
            "entrypoint": "bin/example",
            "capabilities": ["session.read", "session.read"]
        }))
        .unwrap();
        assert!(duplicate_capability.validate().is_err());
    }

    #[test]
    fn shared_manifests_reject_subscriptions() {
        let manifest = |delivery: &str, capabilities: serde_json::Value| {
            serde_json::from_value::<PluginManifest>(json!({
                "id": "com.example",
                "name": "Example",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": "bin/example",
                "subscriptions": [{
                    "id": "sessions",
                    "topics": [CORE_TOPIC_SESSION_STATE_CHANGED],
                    "delivery": delivery
                }],
                "capabilities": capabilities
            }))
            .unwrap()
        };
        assert!(manifest("ephemeral", json!([])).validate().is_err());
        assert!(
            manifest("durable", json!([CORE_CAPABILITY_SESSION_READ]))
                .validate()
                .is_err()
        );
        assert!(
            manifest("ephemeral", json!([CORE_CAPABILITY_SESSION_READ]))
                .validate()
                .is_err()
        );
    }

    #[test]
    fn manifest_contributions_are_unique_and_require_ui_capability() {
        let manifest = |capabilities: serde_json::Value, contributions: serde_json::Value| {
            serde_json::from_value::<PluginManifest>(json!({
                "id": "com.example",
                "name": "Example",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": "bin/example",
                "capabilities": capabilities,
                "contributions": contributions
            }))
            .unwrap()
        };
        let command = json!({
            "type": "command",
            "id": "open-dashboard",
            "title": "Open dashboard",
            "operation": "open"
        });
        assert!(manifest(json!([]), json!([command])).validate().is_err());
        assert!(
            manifest(
                json!([CORE_CAPABILITY_UI_CONTRIBUTE]),
                json!([command.clone(), command])
            )
            .validate()
            .is_err()
        );
        assert!(
            manifest(
                json!([CORE_CAPABILITY_UI_CONTRIBUTE]),
                json!([{
                    "type": "command",
                    "id": "open-dashboard",
                    "title": "Open dashboard",
                    "operation": "open"
                }])
            )
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn input_route_contribution_uses_its_own_non_ui_capability() {
        let manifest = |capabilities: serde_json::Value| {
            serde_json::from_value::<PluginManifest>(json!({
                "id": "com.example",
                "name": "Example",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": "bin/example",
                "capabilities": capabilities,
                "contributions": [{
                    "type": "input_route",
                    "id": "conversation-input",
                    "operation": "submit_input"
                }]
            }))
            .unwrap()
        };

        assert!(manifest(json!([])).validate().is_err());
        assert!(
            manifest(json!([CORE_CAPABILITY_UI_CONTRIBUTE]))
                .validate()
                .is_err(),
            "ui.contribute 不能冒充会话输入路由权限"
        );
        assert!(
            manifest(json!([CORE_CAPABILITY_SESSION_INPUT_ROUTE]))
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn tool_panel_entry_must_stay_inside_the_package() {
        let manifest = |entry: &str| {
            serde_json::from_value::<PluginManifest>(json!({
                "id": "com.example",
                "name": "Example",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": "bin/example",
                "capabilities": [CORE_CAPABILITY_UI_CONTRIBUTE],
                "contributions": [{
                    "type": "tool_panel",
                    "id": "browser",
                    "title": "浏览器",
                    "entry": entry
                }]
            }))
            .unwrap()
            .validate()
        };
        assert!(manifest("web/index.html").is_ok());
        let surface = serde_json::from_value::<PluginUiManifest>(json!({
            "contributions": [{
                "type": "workspace_surface",
                "id": "board",
                "title": "Board",
                "entry": "web/index.html"
            }]
        }))
        .unwrap();
        assert!(matches!(
            surface.known_contributions().next(),
            Some(Contribution::WorkspaceSurface { .. })
        ));
        assert!(manifest("../../etc/passwd").is_err());
        assert!(manifest("/etc/passwd").is_err());
        assert!(manifest("web/../../escape.html").is_err());
        assert!(manifest("").is_err());
    }

    #[test]
    fn ui_manifest_keeps_known_entries_when_a_future_type_is_present() {
        let manifest: PluginUiManifest = serde_json::from_value(json!({
            "contributions": [
                {
                    "type": "future_surface",
                    "id": "ignored",
                    "metadata": { "anything": true }
                },
                {
                    "type": "tool_panel",
                    "id": "browser",
                    "title": "浏览器",
                    "entry": "web/index.html"
                }
            ],
            "future_top_level_field": true
        }))
        .unwrap();

        assert_eq!(manifest.contributions.len(), 2);
        assert!(matches!(manifest.contributions[0], UiContribution::Unknown));
        assert!(matches!(
            manifest.known_contributions().next(),
            Some(Contribution::ToolPanel { .. })
        ));
    }

    #[test]
    fn settings_and_sidebar_account_are_bounded_ui_contributions() {
        let sidecar: PluginUiManifest = serde_json::from_value(json!({
            "contributions": [
                {
                    "type": "sidebar_account",
                    "id": "account-slot",
                    "settings_section": "settings",
                    "account_item": "account"
                },
                {
                    "type": "settings_section",
                    "id": "settings",
                    "title": "Example",
                    "description": "Example account and execution settings.",
                    "snapshot": "settings-snapshot"
                }
            ]
        }))
        .unwrap();

        let mut manifest: PluginManifest = serde_json::from_value(json!({
            "id": "com.example",
            "name": "Example",
            "version": "1.0.0",
            "api_version": 1,
            "entrypoint": "bin/example",
            "capabilities": [CORE_CAPABILITY_UI_CONTRIBUTE],
            "contributions": [{
                "type": "command",
                "id": "settings-snapshot",
                "title": "Read settings",
                "operation": "get_settings_view"
            }]
        }))
        .unwrap();
        manifest.contributions.extend(sidecar.known_contributions());
        manifest.validate().unwrap();

        let mut missing_section = manifest.clone();
        missing_section
            .contributions
            .retain(|contribution| contribution.id().as_str() != "settings");
        assert!(
            missing_section.validate().is_err(),
            "sidebar accounts must reference a declared settings section"
        );

        manifest
            .contributions
            .retain(|contribution| contribution.id().as_str() != "settings-snapshot");
        assert!(
            manifest.validate().is_err(),
            "settings snapshots must reference a declared command"
        );
    }

    #[test]
    fn ui_sidecar_keeps_entity_decoration_as_a_known_surface() {
        let sidecar: PluginUiManifest = serde_json::from_value(json!({
            "contributions": [{
                "type": "entity_decoration",
                "id": "issue-decoration",
                "resource_type": "issue",
                "icon": "assets/icon.svg",
                "snapshot": "decoration-snapshot"
            }]
        }))
        .unwrap();
        let mut manifest: PluginManifest = serde_json::from_value(json!({
            "id": "com.example",
            "name": "Example",
            "version": "1.0.0",
            "api_version": 1,
            "entrypoint": "bin/example",
            "capabilities": [CORE_CAPABILITY_UI_CONTRIBUTE],
            "contributions": [{
                "type": "command",
                "id": "decoration-snapshot",
                "title": "Read decorations",
                "operation": "get_decorations"
            }]
        }))
        .unwrap();
        manifest.contributions.extend(sidecar.known_contributions());
        manifest.validate().unwrap();

        manifest
            .contributions
            .retain(|contribution| contribution.id().as_str() != "decoration-snapshot");
        assert!(
            manifest.validate().is_err(),
            "entity decorations must bind a declared snapshot command"
        );

        let valid: EntityDecorationView = serde_json::from_value(json!({
            "revision": 3,
            "items": [{
                "resource_id": "issue-1",
                "badge": "Issue",
                "tone": "accent",
                "tooltip": "Open issue",
                "external_url": "https://example.com/issues/1"
            }]
        }))
        .unwrap();
        valid.validate().unwrap();

        let duplicate: EntityDecorationView = serde_json::from_value(json!({
            "items": [
                { "resource_id": "issue-1" },
                { "resource_id": "issue-1" }
            ]
        }))
        .unwrap();
        assert!(duplicate.validate().is_err());

        let unsafe_url: EntityDecorationView = serde_json::from_value(json!({
            "items": [{
                "resource_id": "issue-1",
                "external_url": "file:///private/token"
            }]
        }))
        .unwrap();
        assert!(unsafe_url.validate().is_err());
    }

    #[test]
    fn resource_creator_binds_declared_commands_and_bounds_its_projection() {
        let sidecar: PluginUiManifest = serde_json::from_value(json!({
            "contributions": [{
                "type": "resource_creator",
                "id": "issue-creator",
                "title": "New issue",
                "description": "Create an issue in the selected workspace.",
                "target_label": "Workspace",
                "target_placeholder": "Choose a workspace",
                "submit_label": "Create issue",
                "snapshot": "creator-snapshot",
                "submit": "create-issue"
            }]
        }))
        .unwrap();
        let mut manifest: PluginManifest = serde_json::from_value(json!({
            "id": "com.example",
            "name": "Example",
            "version": "1.0.0",
            "api_version": 1,
            "entrypoint": "bin/example",
            "capabilities": [CORE_CAPABILITY_UI_CONTRIBUTE],
            "contributions": [
                {
                    "type": "command",
                    "id": "creator-snapshot",
                    "title": "Read creator",
                    "operation": "get_creator"
                },
                {
                    "type": "command",
                    "id": "create-issue",
                    "title": "Create issue",
                    "operation": "create_issue"
                }
            ]
        }))
        .unwrap();
        manifest.contributions.extend(sidecar.known_contributions());
        manifest.validate().unwrap();

        let mut missing_snapshot = manifest.clone();
        missing_snapshot
            .contributions
            .retain(|contribution| contribution.id().as_str() != "creator-snapshot");
        assert!(
            missing_snapshot.validate().is_err(),
            "resource creators must bind a declared snapshot command"
        );
        manifest
            .contributions
            .retain(|contribution| contribution.id().as_str() != "create-issue");
        assert!(
            manifest.validate().is_err(),
            "resource creators must bind a declared submit command"
        );

        let valid: ResourceCreatorView = serde_json::from_value(json!({
            "revision": 1,
            "available": true,
            "targets": [{ "id": "workspace-1", "label": "Workspace" }],
            "default_target_id": "workspace-1"
        }))
        .unwrap();
        valid.validate().unwrap();
        ResourceCreateParams {
            submission_id: "resource-1".into(),
            target_id: "workspace-1".into(),
            title: "New issue".into(),
            description: Some("Line one\nLine two".into()),
        }
        .validate()
        .unwrap();
        assert!(
            ResourceCreateParams {
                submission_id: String::new(),
                target_id: "workspace-1".into(),
                title: "New issue".into(),
                description: None,
            }
            .validate()
            .is_err()
        );
        assert!(
            ResourceCreateParams {
                submission_id: " resource-1".into(),
                target_id: "workspace-1".into(),
                title: "New issue".into(),
                description: None,
            }
            .validate()
            .is_err(),
            "stable submission IDs must not contain surrounding whitespace"
        );

        let duplicate: ResourceCreatorView = serde_json::from_value(json!({
            "available": true,
            "targets": [
                { "id": "same", "label": "One" },
                { "id": "same", "label": "Two" }
            ]
        }))
        .unwrap();
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn settings_view_rejects_duplicate_items_and_unbounded_action_payloads() {
        let duplicate: SettingsSectionView = serde_json::from_value(json!({
            "revision": 1,
            "items": [
                { "type": "text", "id": "same", "title": "One", "value": "1" },
                { "type": "text", "id": "same", "title": "Two", "value": "2" }
            ]
        }))
        .unwrap();
        assert!(duplicate.validate().is_err());

        let anonymous_signed_in: SettingsSectionView = serde_json::from_value(json!({
            "items": [{
                "type": "account",
                "id": "account",
                "title": "Account",
                "signed_in": true
            }]
        }))
        .unwrap();
        assert!(anonymous_signed_in.validate().is_err());

        let oversized: SettingsSectionView = serde_json::from_value(json!({
            "items": [{
                "type": "actions",
                "id": "actions",
                "actions": [{
                    "id": "run",
                    "label": "Run",
                    "command": "run-command",
                    "payload": { "text": "x".repeat(70 * 1024) }
                }]
            }]
        }))
        .unwrap();
        assert!(oversized.validate().is_err());
    }

    #[test]
    fn input_manifest_keeps_known_entries_when_a_future_type_is_present() {
        let manifest: PluginInputManifest = serde_json::from_value(json!({
            "contributions": [
                {
                    "type": "future_route",
                    "id": "ignored"
                },
                {
                    "type": "input_route",
                    "id": "conversation-input",
                    "operation": "submit_input"
                }
            ]
        }))
        .unwrap();

        assert_eq!(manifest.contributions.len(), 2);
        assert!(matches!(
            manifest.contributions[0],
            InputContribution::Unknown
        ));
        assert!(matches!(
            manifest.known_contributions().next(),
            Some(Contribution::InputRoute { .. })
        ));
    }

    #[test]
    fn agent_manifest_keeps_known_entries_and_validates_the_reference_graph() {
        let sidecar: PluginAgentManifest = serde_json::from_value(json!({
            "contributions": [
                { "type": "future_agent_surface", "id": "ignored" },
                {
                    "type": "agent",
                    "id": "researcher",
                    "name": "Researcher",
                    "icon": "assets/researcher.svg",
                    "controller": "remote-session"
                },
                {
                    "type": "session_controller",
                    "id": "remote-session",
                    "input_route": "conversation-input"
                }
            ]
        }))
        .unwrap();
        assert!(matches!(
            sidecar.contributions[0],
            AgentContribution::Unknown
        ));

        let mut manifest: PluginManifest = serde_json::from_value(json!({
            "id": "com.example",
            "name": "Example",
            "version": "1.0.0",
            "api_version": 1,
            "entrypoint": "bin/example",
            "capabilities": [
                CORE_CAPABILITY_AGENT_CONTRIBUTE,
                CORE_CAPABILITY_SESSION_INPUT_ROUTE
            ],
            "contributions": [{
                "type": "input_route",
                "id": "conversation-input",
                "operation": "submit_input"
            }]
        }))
        .unwrap();
        manifest.contributions.extend(sidecar.known_contributions());
        manifest.validate().unwrap();

        manifest
            .contributions
            .retain(|contribution| !matches!(contribution, Contribution::SessionController { .. }));
        assert!(
            manifest.validate().is_err(),
            "agent cannot reference an undeclared controller"
        );
    }

    #[test]
    fn session_actions_are_controller_scoped_ui_contributions() {
        let sidecar: PluginAgentManifest = serde_json::from_value(json!({
            "contributions": [
                {
                    "type": "session_controller",
                    "id": "quant-session",
                    "input_route": "quant-input"
                },
                {
                    "type": "agent",
                    "id": "quant-agent",
                    "name": "Quant",
                    "controller": "quant-session"
                },
                {
                    "type": "session_action",
                    "id": "open-strategy",
                    "title": "打开策略详情",
                    "operation": "open_strategy",
                    "controller": "quant-session",
                    "locations": ["session_menu", "project_menu"],
                    "icon": "external_link",
                    "result": "open_external"
                }
            ]
        }))
        .unwrap();
        assert!(matches!(
            sidecar.contributions[2],
            AgentContribution::SessionAction {
                result: SessionActionResult::OpenExternal,
                ..
            }
        ));

        let mut manifest: PluginManifest = serde_json::from_value(json!({
            "id": "com.example.quant",
            "name": "Quant",
            "version": "1.0.0",
            "api_version": 1,
            "entrypoint": "bin/example",
            "capabilities": [
                CORE_CAPABILITY_UI_CONTRIBUTE,
                CORE_CAPABILITY_AGENT_CONTRIBUTE,
                CORE_CAPABILITY_SESSION_INPUT_ROUTE
            ],
            "contributions": [{
                "type": "input_route",
                "id": "quant-input",
                "operation": "submit_input"
            }]
        }))
        .unwrap();
        manifest.contributions.extend(sidecar.known_contributions());
        manifest.validate().unwrap();

        manifest
            .contributions
            .retain(|contribution| !matches!(contribution, Contribution::SessionController { .. }));
        assert!(
            manifest.validate().is_err(),
            "session action cannot outlive its controller"
        );
    }

    #[test]
    fn action_and_invocation_messages_are_strict_versioned_contracts() {
        let action: ActionRequest = serde_json::from_value(json!({
            "command_id": "command-1",
            "action": CORE_ACTION_PROJECT_LIST,
            "api_version": 1,
            "correlation_id": "correlation-1",
            "causation_id": null,
            "params": {}
        }))
        .unwrap();
        assert_eq!(action.action.as_str(), CORE_ACTION_PROJECT_LIST);

        let invocation: InvocationRequest = serde_json::from_value(json!({
            "invocation_id": "invocation-1",
            "contribution_id": "open-dashboard",
            "operation": "open",
            "payload": {},
            "deadline_ms": 1000
        }))
        .unwrap();
        assert_eq!(invocation.operation.as_str(), "open");
        assert!(
            serde_json::from_value::<InvocationRequest>(json!({
                "invocation_id": "invocation-1",
                "contribution_id": "open-dashboard",
                "operation": "open",
                "payload": {},
                "deadline_ms": 1000,
                "future": true
            }))
            .is_err()
        );
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
    fn project_action_contracts_are_typed_and_strict() {
        let params: ProjectResolveRepositoriesParams = serde_json::from_value(json!({
            "repository_urls": ["git@example.com:org/repo.git"]
        }))
        .unwrap();
        assert_eq!(params.repository_urls.len(), 1);
        assert!(
            serde_json::from_value::<ProjectResolveRepositoriesParams>(json!({
                "repository_urls": [],
                "project_root": "/forged"
            }))
            .is_err()
        );

        let result = ProjectListResult {
            projects: vec![ProjectDescriptor {
                root: "/repo".to_string(),
                title: "Repo".to_string(),
                repositories: vec![ProjectRepository {
                    path: "/repo".to_string(),
                    remote_key: "example.com/org/repo".to_string(),
                }],
            }],
        };
        assert_eq!(
            serde_json::to_value(result).unwrap()["projects"][0]["repositories"][0]["remote_key"],
            "example.com/org/repo"
        );
    }

    #[test]
    fn isolated_workspace_action_contracts_do_not_accept_local_paths() {
        let owner = PluginResourceRef {
            plugin_id: PluginId::new("com.example.automation").unwrap(),
            resource_type: PluginResourceType::new("job").unwrap(),
            resource_id: PluginResourceId::new("job-1").unwrap(),
        };
        let params: WorkspaceCreateIsolatedParams = serde_json::from_value(json!({
            "owner": owner,
            "repository_urls": ["git@example.com:org/repo.git"],
            "branch_label": "AUTO-7"
        }))
        .unwrap();
        assert_eq!(params.owner.resource_id.as_str(), "job-1");
        assert!(
            serde_json::from_value::<WorkspaceCreateIsolatedParams>(json!({
                "owner": params.owner,
                "repository_urls": [],
                "branch_label": "AUTO-7",
                "repo_root": "/forged/source",
                "worktree_dir": "/forged/worktree"
            }))
            .is_err()
        );
    }

    #[test]
    fn event_client_auth_is_a_strict_stable_wire_contract() {
        let auth: EventClientAuth = serde_json::from_value(json!({
            "type": "first_party",
            "kind": "remote_gateway"
        }))
        .unwrap();
        assert_eq!(
            auth,
            EventClientAuth::FirstParty {
                kind: FirstPartyClientKind::RemoteGateway
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
