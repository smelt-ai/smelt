//! 对话：订阅一个会话的快照流、翻更早的页、发送与各类动作、取图片。
//!
//! 实现在 `crate::conversation`，这里只放 Dart 用的视图模型和入口。会阻塞的调用都是普通
//! `pub fn`（FRB 放到工作线程池，Dart 拿 Future），不用 `async fn`：iroh 连接器内部
//! `block_on`，不能跑在 tokio 线程上。
//!
//! 项目没有引入 freezed，FRB 只能把「不带数据的枚举」翻成 Dart enum。所以带数据的
//! 变体一律写成「`kind` 枚举 + 各形态的可选字段」的普通结构体：按 `kind` 读对应字段，
//! 其余字段为空。

use flutter_rust_bridge::frb;

use crate::api::error::SmeltError;
use crate::frb_generated::StreamSink;

// ---------------------------------------------------------------------------
// 快照流
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationEventKind {
    /// 一份新的对话视图（`view` 有值）。
    Snapshot,
    /// 运行时接不上这段对话（没在跑、电脑端也没把它拉起来）。`message` 是给人看的原因。
    /// 订阅随之结束；「重试」就再调一次 [`watch_conversation`]。
    Unavailable,
    /// 订阅出错（没连运行时、订阅连接建不起来）。`message` 是原因。还连着运行时的话
    /// 订阅会在后台退避重试，成功后照常推 `Snapshot`。
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConversationEvent {
    pub kind: ConversationEventKind,
    /// `Snapshot` 时有。
    pub view: Option<ConversationView>,
    /// 这份视图来自离线缓存，还没收到运行时的实时数据。
    pub cached: bool,
    /// `cached` 时是缓存的保存时间（Unix 毫秒）。
    pub saved_at_ms: Option<i64>,
    /// `Unavailable` 的原因，或 `Error` 的文案。
    pub message: Option<String>,
}

/// 与守护进程 `DaemonPhase` 一一对应。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationPhase {
    Connecting,
    Thinking,
    ExecutingTool,
    AwaitingApproval,
    WaitingForUser,
    Succeeded,
    Failed,
    Idle,
    Dead,
}

/// 对话页要画的全部内容。条目是「已经在手里的连续窗口」：实时尾部加上翻出来的更早页。
#[derive(Clone, Debug, PartialEq)]
pub struct ConversationView {
    pub session_id: String,
    pub phase: ConversationPhase,
    pub status_line: Option<String>,
    /// `Dead` / `Failed` 时的原因，活会话为空串。
    pub end_reason: String,
    pub session_title: Option<String>,
    /// 当前回合开始的 Unix 毫秒；没有运行中的回合时为空。
    pub turn_started_at_ms: Option<i64>,
    /// 最近一个已结束回合的耗时，画在最终回答下面。
    pub last_turn_duration_ms: Option<i64>,
    /// 计划步骤。空表示没有计划。
    pub plan: Vec<PlanStep>,
    pub pending_permissions: Vec<PermissionRequest>,
    pub pending_elicitation: Option<Elicitation>,
    /// 按时间正序。
    pub entries: Vec<ConversationEntry>,
    /// `entries[0]` 在整段对话里的下标。
    pub entries_offset: i64,
    /// 前面还有更早的条目可翻（[`load_older`]）。
    pub has_more_before: bool,
    /// 回合已结束时，「最终回答」在 `entries` 里的下标（最后一条非空正文或完成信号）。
    pub final_answer_index: Option<u32>,
    pub supports_image: bool,
    pub available_commands: Vec<SlashCommand>,
    pub usage: Option<ContextUsage>,
    pub model: Option<ModelSelector>,
    /// 除模型以外的会话配置项。
    pub config_options: Vec<ConfigOption>,
}

// ---------------------------------------------------------------------------
// 条目
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// 用户消息：`text` + `images`。
    User,
    /// agent 回显的「用户中断」标记，画成分割线。`text` 是原文。
    Interrupted,
    /// 助手正文或思考块：`text` + `thought`。
    Assistant,
    /// 工具调用：`tool_call`。
    ToolCall,
    /// `task_complete` 完成信号：`text` 是给人看的总结（已去代码围栏，可能为空），
    /// `tool_call` 里有状态和原始输出。
    Completion,
    /// 「重新开始」等分割线：`text` 是标签。
    Divider,
    /// 后台任务 / 子智能体结束：`task_note`，`text` 是一行说明。
    TaskNote,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConversationEntry {
    /// 在整段对话里的下标，可当列表项的 key。
    pub index: i64,
    pub kind: EntryKind,
    pub text: String,
    pub thought: bool,
    pub images: Vec<ConversationImage>,
    pub tool_call: Option<ToolCallView>,
    pub task_note: Option<TaskNoteView>,
}

/// 一张图。小图直接给 `bytes`；大图运行时只给引用 `original`，用 [`load_image`] 取。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationImage {
    pub mime: String,
    pub bytes: Option<Vec<u8>>,
    pub original: Option<ImageRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRef {
    /// 图片所在顶层条目的全局下标。
    pub entry: i64,
    pub digest: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    Collaborate,
    Review,
    Image,
    Compact,
    Wait,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCallView {
    pub id: String,
    pub title: String,
    pub kind: ToolKind,
    pub status: ToolStatus,
    pub output: Vec<ToolOutput>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolOutputKind {
    /// `text`。
    Text,
    /// `diff`。
    Diff,
    /// `image`。
    Image,
    /// 嵌入终端的输出：`text`、`exit_code`、`truncated`。
    Terminal,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutput {
    pub kind: ToolOutputKind,
    pub text: String,
    pub diff: Option<DiffView>,
    pub image: Option<ConversationImage>,
    pub exit_code: Option<i32>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffView {
    pub path: String,
    /// 新文件没有旧内容。
    pub old_text: Option<String>,
    pub new_text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskNoteKind {
    BackgroundTask,
    Subagent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskNoteView {
    pub kind: TaskNoteKind,
    pub id: String,
    pub title: String,
    /// `completed` / `failed` / `timed_out` / `stopped` / `running`，或运行时的新状态名。
    pub status: String,
    pub exit_code: Option<i32>,
    pub output_tail: String,
}

// ---------------------------------------------------------------------------
// 审批与询问
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionRequest {
    pub tool_call_id: String,
    pub question: String,
    pub options: Vec<PermissionOption>,
    pub details: ApprovalDetails,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: PermissionOptionKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalKind {
    /// 跑命令：`command`、`cwd`、`reason`。
    Command,
    /// 改文件：`reason`、`grant_root`。
    FileChange,
    /// 申请权限：`summary`。
    Permissions,
    Generic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalDetails {
    pub kind: ApprovalKind,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub reason: Option<String>,
    pub grant_root: Option<String>,
    pub summary: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Elicitation {
    pub message: String,
    pub fields: Vec<ElicitationField>,
    /// 按运行时记下的选择和文字，必填项都已填好。界面上还没提交的本地输入要自己再算一遍
    /// （必填文字框：本地值或 `text_value` 非空白）。
    pub is_ready: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElicitationFieldKind {
    /// 单选：`options`。
    Select,
    /// 多选：`options`。
    MultiSelect,
    /// 文字：`secret`。
    Text,
    /// 外部步骤：`url`。
    ExternalUrl,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElicitationField {
    pub key: String,
    pub title: String,
    pub required: bool,
    /// 选择题也允许自己输入答案。
    pub allow_custom_input: bool,
    pub kind: ElicitationFieldKind,
    /// 选项标签，回选时用下标。
    pub options: Vec<String>,
    pub secret: bool,
    pub url: Option<String>,
    /// 已选中的选项下标。
    pub chosen: Vec<u32>,
    /// 运行时记下的文字。
    pub text_value: Option<String>,
}

/// 指挥台不进会话就能审批：一个会话当前在等的审批和询问。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingActions {
    pub permissions: Vec<PermissionRequest>,
    pub elicitation: Option<Elicitation>,
}

// ---------------------------------------------------------------------------
// 状态条、模型与配置
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanStep {
    pub content: String,
    pub status: PlanStepStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextUsage {
    pub used_tokens: i64,
    pub context_window: i64,
}

/// 下拉里的一个候选。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChoiceOption {
    /// 写回时用的值（[`set_config_option`] 的 `value_id`）。
    pub value: String,
    pub name: String,
    /// 是当前值。
    pub selected: bool,
}

/// 模型入口。协议按 provider 分组时先选 provider 再选模型（和桌面输入栏同一套）；
/// 平铺协议 `providers` 为空，只有一个模型入口。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSelector {
    /// 写回时的配置项 id。
    pub config_id: String,
    pub current_value: String,
    pub current_name: String,
    pub providers: Vec<ModelProvider>,
    /// 当前 provider 在 `providers` 里的下标。
    pub current_provider: Option<u32>,
    /// 模型菜单要列的候选：有分组时是当前 provider 下的，否则是平铺列表。
    pub options: Vec<ChoiceOption>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelProvider {
    pub id: String,
    /// 显示名，协议没给名字时用 id。
    pub name: String,
    pub options: Vec<ChoiceOption>,
    /// 切到这个 provider 要写回的模型值：同名模型优先，否则组里第一个；空组为空，
    /// 菜单里该项不可点。协议只认模型值，没有「只换 provider」的写法。
    pub switch_value: Option<String>,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigOption {
    pub config_id: String,
    pub name: String,
    pub description: Option<String>,
    pub current_name: String,
    pub options: Vec<ChoiceOption>,
    /// 有值表示这是开关（当前值），写回时用 [`set_config_option`] 的 `boolean`；
    /// 为空表示下拉选择。
    pub boolean: Option<bool>,
}

// ---------------------------------------------------------------------------
// 发送、技能、图片
// ---------------------------------------------------------------------------

/// 要随消息发出的图片：原始字节，Rust 侧编码。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageInput {
    pub mime: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendFailureKind {
    /// 运行时明确没收下（或请求根本没发出去），草稿原样留着，可以放心重发。
    Rejected,
    /// 请求可能已经送达但没等到确认（超时、断线）。直接重发可能发两遍，要让用户确认。
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendMessageError {
    pub kind: SendFailureKind,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElicitationTextInput {
    pub field_index: u32,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSkill {
    pub name: String,
    pub description: String,
}

/// `supported == false`：这场对话的引擎没有「技能」这个概念（目前只有 Pi 有），或会话
/// 没在运行。界面不画入口。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSkills {
    pub supported: bool,
    pub skills: Vec<SessionSkill>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageData {
    pub mime: String,
    pub bytes: Vec<u8>,
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// 订阅一个会话。同一时间只有一个活动订阅：再调一次（同一个或别的会话）会顶掉前一个。
///
/// 有离线缓存就先推一份（`cached = true`），再连运行时，收到实时数据后推 `cached = false`
/// 的视图。断线且运行时还连着时后台退避重订；运行时被切换或断开（换了一代连接）就停，
/// 回到前台或重新连上后要再调一次。Dart 取消订阅也会停。
pub fn watch_conversation(session_id: String, sink: StreamSink<ConversationEvent>) {
    crate::conversation::global().watch(session_id, Box::new(move |event| sink.add(event).is_ok()));
}

/// 停掉当前订阅（离开对话页时调）。
pub fn unwatch_conversation() {
    crate::conversation::global().unwatch();
}

/// 往前翻一页（100 条）。结果合并进当前订阅的窗口，从订阅流推出。
///
/// 返回 false 表示没翻：不是当前订阅的会话、前面没有了，或已经在翻。
pub fn load_older(session_id: String) -> Result<bool, SmeltError> {
    crate::conversation::global().load_older(&session_id)
}

/// 不订阅、只取一个会话当前在等的审批和询问（指挥台用）。
pub fn pending_actions(session_id: String) -> Result<PendingActions, SmeltError> {
    crate::conversation::global().pending_actions(&session_id)
}

/// 新的发送请求 id。草稿里存着它，没等到确认就用同一个 id 重发。
#[frb(sync)]
pub fn new_request_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// 发一条消息。`request_id` 用 [`new_request_id`] 生成，作为这次提交的 `submission_id`；
/// 重发同一份草稿时要用同一个 id。最长等 35 秒。
pub fn send_message(
    session_id: String,
    request_id: String,
    text: String,
    images: Vec<ImageInput>,
) -> Result<(), SendMessageError> {
    crate::conversation::global().send_message(&session_id, &request_id, text, images)
}

/// 停掉当前回合。
pub fn cancel_turn(session_id: String) -> Result<(), SmeltError> {
    crate::conversation::global().cancel_turn(&session_id)
}

/// 切换模型或会话配置。开关项传 `boolean`，下拉项传 `value_id`（开关项的 `value_id`
/// 运行时不看，传空串即可）。
pub fn set_config_option(
    session_id: String,
    config_id: String,
    value_id: String,
    boolean: Option<bool>,
) -> Result<(), SmeltError> {
    crate::conversation::global().set_config_option(&session_id, config_id, value_id, boolean)
}

/// 回答一个审批。
pub fn respond_approval(
    session_id: String,
    tool_call_id: String,
    option_id: String,
) -> Result<(), SmeltError> {
    crate::conversation::global().respond_approval(&session_id, tool_call_id, option_id)
}

/// 点选询问里的一个选项（多选题再点一次是取消）。
pub fn choose_elicitation(
    session_id: String,
    field_index: u32,
    option_index: u32,
) -> Result<(), SmeltError> {
    crate::conversation::global().choose_elicitation(&session_id, field_index, option_index)
}

/// 记下询问里一个文字框的内容。
pub fn update_elicitation_text(
    session_id: String,
    field_index: u32,
    value: String,
) -> Result<(), SmeltError> {
    crate::conversation::global().update_elicitation_text(&session_id, field_index, value)
}

/// 提交询问。`text_values` 是界面上还没同步给运行时的文字框，按顺序先写再提交。
pub fn submit_elicitation(
    session_id: String,
    text_values: Vec<ElicitationTextInput>,
) -> Result<(), SmeltError> {
    crate::conversation::global().submit_elicitation(&session_id, text_values)
}

/// 关掉询问（不回答）。
pub fn dismiss_elicitation(session_id: String) -> Result<(), SmeltError> {
    crate::conversation::global().dismiss_elicitation(&session_id)
}

/// 这场对话加载了哪些技能。
pub fn list_skills(session_id: String) -> Result<SessionSkills, SmeltError> {
    crate::conversation::global().list_skills(&session_id)
}

/// 取一张大图。给 `max_edge` 出缩略图（长边不超过它），不给出可看的原图。
pub fn load_image(
    session_id: String,
    entry: i64,
    digest: String,
    max_edge: Option<u32>,
) -> Result<ImageData, SmeltError> {
    crate::conversation::global().load_image(&session_id, entry, digest, max_edge)
}
