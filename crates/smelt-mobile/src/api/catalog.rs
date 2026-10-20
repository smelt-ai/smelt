//! 会话目录：会话摘要与 attention 的事件流、工作区、自动化、历史、新建与删除。
//!
//! 会话列表和 attention 由 Rust 侧订阅每台机器的事件后在手机本地投影（attention 的已读
//! 状态只属于这台手机）；其余都是对某台机器的一问一答，第一个参数 `runtime_id` 指定是哪台。
//! 会阻塞的调用都是普通 `pub fn`：FRB 放到工作线程池执行，Dart 拿到 Future。

use flutter_rust_bridge::frb;
use serde::{Deserialize, Serialize};

use crate::api::error::SmeltError;
use crate::frb_generated::StreamSink;

// ---------------------------------------------------------------------------
// 会话列表与 attention
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionKind {
    Acp,
    Terminal,
}

/// 会话当前所处的阶段。没有回合级结构化来源时，活动 / 结果阶段一律降为 `Idle`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionPhase {
    /// ACP 握手 / 启动进程。
    Connecting,
    Thinking,
    ExecutingTool,
    AwaitingApproval,
    WaitingForUser,
    Succeeded,
    Failed,
    Idle,
    /// 运行时已结束。
    Dead,
}

/// 侧栏、菜单栏、手机共用的三态。刚完成、断连、无活动都算空闲。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    /// 要你：等批准、等输入、失败。
    NeedsYou,
    Running,
    Idle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttentionKind {
    Approval,
    Input,
    Success,
    Failure,
    Bell,
    Notice,
}

/// 一条需要告诉用户的事。`Approval` / `Input` / `Failure` 是要处理的事。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionItem {
    pub session_id: String,
    pub title: String,
    pub message: String,
    pub kind: AttentionKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutomationRunStatus {
    Starting,
    Queued,
    Dispatching,
    Running,
    AwaitingApproval,
    WaitingForUser,
    Completed,
    Failed,
    Cancelled,
    Skipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutomationRunSource {
    Manual,
    Scheduled,
    Webhook,
    Event,
}

/// 这条会话是某条自动化的一次运行现场。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationSource {
    pub automation_name: String,
    pub run_status: AutomationRunStatus,
    pub run_source: AutomationRunSource,
    /// 这次运行固化的输入，不是自动化当前的定义值。
    pub prompt: Option<String>,
    /// unix 秒。
    pub started_at: Option<i64>,
}

/// 列表上的一条会话。列表本身已按桌面菜单的顺序排好。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub kind: SessionKind,
    pub title: String,
    pub phase: SessionPhase,
    pub status: SessionStatus,
    /// 画图标用的 agent 标识（`claude`、`pi`……），认不出是 `other`。
    pub agent: String,
    pub cwd: Option<String>,
    /// 自动化运行的会话没有项目。
    pub project_root: Option<String>,
    pub project_title: Option<String>,
    /// 项目在菜单里的次序；没有项目时是 `u32::MAX`。
    pub project_order: u32,
    /// unix 秒，最后一次活动；从没动过的会话是 0。
    pub updated_at: i64,
    /// 工具名或审批问句。
    pub detail: Option<String>,
    /// 这台手机上还没读的提醒；有值就是未读。
    pub attention: Option<AttentionItem>,
    /// 只有自动化运行的会话才有。
    pub automation: Option<AutomationSource>,
    /// 这是一条产品智能体的对话时，智能体定义的 id。名字到自动化目录的 `agents` 里配。
    pub agent_definition_id: Option<String>,
}

/// 当前已知的会话列表。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionList {
    pub sessions: Vec<SessionSummary>,
    /// 不是运行时的实时数据：还没收到这台机器的快照（没连上、正在连）。
    /// 此时 `saved_at_ms` 有值表示列表来自离线缓存，没有值表示列表为空占位。
    pub cached: bool,
    /// 缓存保存时间（unix 毫秒）。
    pub saved_at_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogEventKind {
    /// `sessions` 有值：整份会话列表（短时间内的连续变化合并成一次）。
    Sessions,
    /// `attention` 有值：新的提醒。同一会话、类型和正文 60 秒内只来一次。
    Attention,
    /// `session_id` 有值：这条会话要处理的事（审批、输入、失败）已经过去了。
    AttentionResolved,
    /// `message` 有值：会话目录的订阅出错（会自动重试），只给人看。
    Error,
}

/// 目录事件。FRB 把带数据的枚举生成成 freezed 类，本项目没开 freezed，所以用
/// `kind` + 对应的可选负载。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogEvent {
    /// 哪台机器的事件。
    pub runtime_id: String,
    pub kind: CatalogEventKind,
    pub sessions: Option<SessionList>,
    pub attention: Option<AttentionItem>,
    pub session_id: Option<String>,
    pub message: Option<String>,
}

/// 所有机器的会话目录事件流，事件带 `runtime_id`。订阅时每台机器先推一次当前已知的列表；
/// 之后配对的机器同样会推。忘记一台机器时推一次它的空列表（`cached = true`、没有
/// `saved_at_ms`）。可以有多个订阅者。
pub fn catalog_events(sink: StreamSink<CatalogEvent>) {
    crate::fleet::fleet().subscribe_catalog(Box::new(move |event| {
        // Dart 侧取消订阅后端口关闭，发送失败，监听随之移除。
        sink.add(event.clone()).is_ok()
    }));
}

/// 标记这条会话的提醒已读。只影响本机。没有这台机器时什么也不做。
#[frb(sync)]
pub fn mark_read(runtime_id: String, session_id: String) {
    if let Ok(node) = crate::fleet::fleet().node(&runtime_id) {
        node.catalog.mark_read(&session_id);
    }
}

// ---------------------------------------------------------------------------
// 工作区与新建
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceProject {
    pub root: String,
    pub title: String,
    pub order: u32,
}

/// 可开对话的 agent 选项。产品智能体在前，裸引擎与 profile 在后。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcpAgentOption {
    pub id: String,
    /// 引擎 id（`claude`、`pi`……）。
    pub kind: String,
    pub label: String,
    /// 产品智能体选项才有。这种对话可以不选项目，开在智能体自己的 space。
    pub agent_definition_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchTarget {
    Conversation,
    Terminal,
    BlankTerminal,
}

/// 新建选择器里的一行。只回传 `key`，命令永远在运行时那台机器上解析。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchAction {
    pub key: String,
    /// 不含类型后缀的名称。
    pub label: String,
    pub target: LaunchTarget,
    /// 画图标用的 agent 标识；认不出是空串。
    pub agent: String,
}

/// 新建选择器的三个分组，已按桌面同样的顺序和 Pin 排好。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchSections {
    pub common: Vec<LaunchAction>,
    pub terminal: Vec<LaunchAction>,
    pub conversation: Vec<LaunchAction>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceCatalog {
    /// 按菜单次序排好。
    pub projects: Vec<WorkspaceProject>,
    pub agents: Vec<AcpAgentOption>,
    pub sections: LaunchSections,
}

/// 项目列表（来自桌面菜单与远程会话）和新建目录。刚连上还没收到会话目录时最多等几秒。
pub fn workspace_catalog(runtime_id: String) -> Result<WorkspaceCatalog, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .catalog
        .workspace_catalog()
}

/// 新建会话，返回会话 id。
///
/// - `launch_key`：新建目录里那一行的 key，给了就由运行时决定开对话还是哪种终端。
/// - 否则 `kind` 为空或 `Acp` 时开对话（要 `agent_option_id`），`Terminal` 开空白终端。
/// - `project_root` 为空：只有产品智能体的对话可以，开在智能体自己的 space。
/// - `request_id`：一次新建意图的 UUID，重试时复用，落到同一个会话。
pub fn create_session(
    runtime_id: String,
    project_root: String,
    agent_option_id: Option<String>,
    launch_key: Option<String>,
    resume_id: Option<String>,
    kind: Option<SessionKind>,
    request_id: Option<String>,
) -> Result<String, SmeltError> {
    let node = crate::fleet::fleet().node(&runtime_id)?;
    node.catalog.create_session(crate::catalog::CreateSession {
        project_root,
        agent_option_id,
        launch_key,
        resume_id,
        kind,
        request_id,
    })
}

/// 删除会话（终端或对话由运行时判断；自动化运行的会话会被拒绝）。成功后立刻从列表划掉。
pub fn delete_session(runtime_id: String, session_id: String) -> Result<(), SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .catalog
        .delete_session(&session_id)
}

// ---------------------------------------------------------------------------
// 自动化
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleKind {
    Daily,
    EveryMinutes,
    EveryHours,
    Weekly,
    /// 在指定星期的时段内按间隔触发。
    During,
}

/// 一条调度规则。没用到的字段为 0。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutomationSchedule {
    pub kind: ScheduleKind,
    /// `Daily` / `Weekly` 的时刻；`During` 的时段开始。
    pub hour: u8,
    pub minute: u8,
    /// `EveryMinutes` / `During` 的间隔。
    pub minutes: u32,
    /// `EveryHours` 的间隔。
    pub hours: u32,
    /// `Weekly` / `During` 的星期位掩码，bit0 = 周一。
    pub days: u8,
    /// `During` 的时段结束。
    pub end_hour: u8,
    pub end_minute: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomationActionKind {
    Agent,
    Shell,
}

/// 目录行上的「上次结果」。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomationRunSummary {
    pub status: AutomationRunStatus,
    pub source: AutomationRunSource,
    /// unix 秒。
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    /// 那次运行的会话，有值就能直接跳进执行现场。
    pub session_id: Option<String>,
    pub error: Option<String>,
}

/// 一条自动化。webhook 的地址和密钥是凭据，不在这里。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomationSummary {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub schedules: Vec<AutomationSchedule>,
    pub event_topics: Vec<String>,
    /// 有没有外部（HTTP）触发入口。
    pub webhook: bool,
    pub action: AutomationActionKind,
    /// 执行的智能体名；定义已被删掉时为空。
    pub agent_name: Option<String>,
    /// unix 秒。
    pub next_run_at: Option<i64>,
    pub last_run: Option<AutomationRunSummary>,
}

/// 智能体定义的只读投影。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentDefinitionSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    /// 执行引擎 id（`pi` 等）。
    pub engine_kind_id: String,
    /// 长期工作方式。
    pub prompt: String,
    pub plugins: Vec<String>,
    pub context_folders: Vec<String>,
    pub context_links: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomationCatalog {
    /// 与桌面列表同序。
    pub automations: Vec<AutomationSummary>,
    pub agents: Vec<AgentDefinitionSummary>,
}

/// 自动化与智能体定义。
pub fn automation_catalog(runtime_id: String) -> Result<AutomationCatalog, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .catalog
        .automation_catalog()
}

/// 开关一条自动化，返回最新目录。
pub fn set_automation_enabled(
    runtime_id: String,
    automation_id: String,
    enabled: bool,
) -> Result<AutomationCatalog, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .catalog
        .set_automation_enabled(&automation_id, enabled)
}

/// 立刻运行一次，返回最新目录（这次运行已在 `last_run` 里）。
pub fn run_automation_once(
    runtime_id: String,
    automation_id: String,
) -> Result<AutomationCatalog, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .catalog
        .run_automation_once(&automation_id)
}

// ---------------------------------------------------------------------------
// 历史会话
// ---------------------------------------------------------------------------

/// 一段可续接的历史会话。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistorySessionSummary {
    /// 续接时传给 `create_session`。
    pub resume_id: String,
    /// agent 自己生成的标题，永远是原始值。
    pub title: String,
    /// 用户在 Smelt 里改的名字。展示时优先用它。
    pub custom_title: Option<String>,
    /// unix 毫秒。
    pub last_active_at_ms: Option<i64>,
    pub message_count: u32,
}

/// 改名结果。恢复默认名时 `custom_title` 为空，`title` 是 agent 原始标题。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRenameResult {
    /// 现在该显示的标题。
    pub title: String,
    pub custom_title: Option<String>,
}

/// 某个 agent 选项在某个项目下的历史会话，最近活跃的在前。
pub fn list_history(
    runtime_id: String,
    project_root: String,
    agent_option_id: String,
) -> Result<Vec<HistorySessionSummary>, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .catalog
        .list_history(&project_root, &agent_option_id)
}

/// 给历史会话改名；`title` 为空表示恢复 agent 原始标题。
pub fn rename_history(
    runtime_id: String,
    project_root: String,
    agent_option_id: String,
    resume_id: String,
    title: Option<String>,
) -> Result<HistoryRenameResult, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .catalog
        .rename_history(
            &project_root,
            &agent_option_id,
            &resume_id,
            title.as_deref(),
        )
}
