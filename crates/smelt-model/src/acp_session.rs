//! ACP 会话推给客户端的快照，以及客户端发回的用户操作。快照里只有展示要用的投影
//! （权限请求、表单、计划、后台任务、调试信息），没有 agent_client_protocol 的连接对象。
//! 把 ACP 事件归约成状态的部分在运行时（smelt-core）。

use std::collections::{BTreeMap, BTreeSet};

use agent_client_protocol::schema::v1::{PermissionOptionKind, PlanEntryStatus, StopReason};

use crate::acp_chat::{AcpEntry, AcpImage as PromptImage};
use crate::acp_conn::{ModelState, SessionConfigState};
use crate::daemon_state::DaemonPhase;

/// ACP 连接结束的稳定分类。展示文案留在 `end_reason`，控制流只能依据
/// 这里的结构化原因，避免改文案或本地化后悄悄改变重连/失败语义。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpEndKind {
    /// 兼容旧快照；未知原因按不可重试处理，避免把真实 provider 故障循环重连。
    #[default]
    Unknown,
    /// GUI 与 smeltd 的控制连接断开，或 daemon 内部 ACP 字节流意外 EOF。
    TransportDisconnected,
    /// agent/provider 启动或运行失败。
    ProviderFailed,
    /// `session/load` 明确失败。
    RestoreFailed,
    /// provider session 已被另一条 Smelt 会话持有。
    SessionOwnershipConflict,
    /// 用户（本机或远端客户端）明确终结了这条会话：smeltd 已摘表、已杀
    /// provider。跟 `TransportDisconnected` 必须区分开——后者只是传输断了，
    /// 客户端应该重连；这个语义下重连等于把刚被删掉的会话原地复活。
    SessionTerminated,
}

impl AcpEndKind {
    pub fn is_transient(self) -> bool {
        matches!(self, Self::TransportDisconnected)
    }

    /// 会话本体已经不存在：客户端只能拆掉视图，不能重连、也不必再发 kill。
    pub fn is_terminated(self) -> bool {
        matches!(self, Self::SessionTerminated)
    }
}

/// Provider 正在自动重试这一轮的模型请求（限流、过载这类临时错误）。重试结束
/// （成功或放弃）、回合结束、开新一轮时清掉。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProviderRetryView {
    /// 第几次重试，从 1 数起。
    pub attempt: u32,
    pub max_attempts: u32,
    /// 这次重试前要等多久。
    pub delay_ms: u64,
    /// 触发重试的错误。
    pub error: String,
}

/// 守护进程闸门后面排队、还没交给 agent 的一条消息。回合在跑时不支持中途插话的
/// agent 收到的新消息都在这里，等当前回合结束再按序发出。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingPromptView {
    /// 会话内稳定 id。撤回按它找，不按文本或下标。
    pub id: u64,
    pub text: String,
    #[serde(default)]
    pub image_count: usize,
    /// 用户自己在输入框里发的才能撤回。任务、自动化、其它智能体的投递归守护进程。
    #[serde(default)]
    pub withdrawable: bool,
}

/// 给用户看的一次性提示（插话、压缩、停后台任务失败这类）。不是启动进度：
/// 启动进度走 `status_line`。新回合开始、会话重启时清掉。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AcpNoticeView {
    /// 单调递增，客户端按它记「这条已经关掉」，新提示不会被旧的关闭动作盖住。
    pub id: u64,
    pub text: String,
}

/// ACP 回合终结原因的稳定投影。`DaemonPhase::Idle` 只描述当前没有运行中的回合，
/// 不能同时承担“上一轮成功/取消/被拒绝”的结果语义；否则 daemon 只能把所有
/// `TurnEnded` 猜成成功。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpTurnOutcome {
    Succeeded,
    Cancelled,
    MaxTokens,
    MaxTurnRequests,
    Refused,
    Failed,
}

impl AcpTurnOutcome {
    pub fn from_stop_reason(reason: StopReason, cancel_requested: bool) -> Self {
        if cancel_requested {
            return Self::Cancelled;
        }
        match reason {
            StopReason::EndTurn => Self::Succeeded,
            StopReason::Cancelled => Self::Cancelled,
            StopReason::MaxTokens => Self::MaxTokens,
            StopReason::MaxTurnRequests => Self::MaxTurnRequests,
            StopReason::Refusal => Self::Refused,
            _ => Self::Failed,
        }
    }

    /// 只有协议明确给出的成功才是成功。调用方对 `None` 不能再补成成功。
    pub fn is_success(self) -> bool {
        matches!(self, Self::Succeeded)
    }

    /// 需要作为失败提醒展示的稳定文案。用户主动取消是正常控制动作，不产生
    /// “需要处理”角标；其余非成功终点都应明确告诉用户为什么没有正常完成。
    pub fn failure_message(self) -> Option<&'static str> {
        match self {
            Self::Succeeded | Self::Cancelled => None,
            Self::MaxTokens => Some("已达到本轮最大令牌数"),
            Self::MaxTurnRequests => Some("已达到本轮最大请求数"),
            Self::Refused => Some("Agent 拒绝继续处理"),
            Self::Failed => Some("回合未正常完成"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PermissionOptionKindView {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

impl PermissionOptionKindView {
    pub fn from_acp(k: PermissionOptionKind) -> Self {
        match k {
            PermissionOptionKind::AllowOnce => Self::AllowOnce,
            PermissionOptionKind::AllowAlways => Self::AllowAlways,
            PermissionOptionKind::RejectOnce => Self::RejectOnce,
            PermissionOptionKind::RejectAlways => Self::RejectAlways,
            // #[non_exhaustive]：协议以后加新分类先当「拒绝一次」——比默认允许安全。
            _ => Self::RejectOnce,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PermissionOptionView {
    pub option_id: String,
    pub name: String,
    pub kind: PermissionOptionKindView,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PendingPermission {
    pub question: String,
    pub tool_call_id: String,
    pub options: Vec<PermissionOptionView>,
    #[serde(default)]
    pub details: ApprovalDetailsView,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalDetailsView {
    Command {
        command: String,
        cwd: Option<String>,
        reason: Option<String>,
    },
    FileChange {
        reason: Option<String>,
        grant_root: Option<String>,
    },
    Permissions {
        summary: String,
    },
    #[default]
    Generic,
}

/// 选择题字段的展示形态——**不带** `ElicitationContentValue`：客户端只按
/// `(字段下标, 选项下标)` 回选，真正的协议值只在 smeltd 自己持有的
/// `AcpSessionState`（非快照那份）里，翻译成 `ElicitationContentValue` 是
/// `submit_elicitation` 收到 `AcpUserAction::ElicitationSubmit` 时才做的事。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ElicitOptionView {
    pub label: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum ElicitFieldKindView {
    Select(Vec<ElicitOptionView>),
    MultiSelect(Vec<ElicitOptionView>),
    Text { secret: bool },
    ExternalUrl(String),
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ElicitFieldView {
    pub key: String,
    pub title: String,
    // Snapshots before optional elicitation fields existed omitted this key.
    // Those fields were all required, so preserve that behavior on restore.
    #[serde(default = "elicitation_field_required_by_default")]
    pub required: bool,
    /// 允许在选项之外自己输入答案。旧快照没有这个键，反序列化得 false（只是
    /// 不显示自由输入行），不会改变已有行为。
    #[serde(default)]
    pub allow_custom_input: bool,
    pub kind: ElicitFieldKindView,
}

fn elicitation_field_required_by_default() -> bool {
    true
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PendingElicitation {
    pub message: String,
    pub fields: Vec<ElicitFieldView>,
    /// 已选中的 (字段下标 → 选项下标列表)，跟旧版 `ElicitCard.chosen` 同一份
    /// 语义——GUI 要能画出「已经点了哪些」，不能重连一次就清空选择态。
    pub chosen: BTreeMap<usize, Vec<usize>>,
    #[serde(default)]
    pub text_values: BTreeMap<usize, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlanEntryStatusView {
    Pending,
    InProgress,
    Completed,
}

impl PlanEntryStatusView {
    pub fn from_acp(s: PlanEntryStatus) -> Self {
        match s {
            PlanEntryStatus::Completed => Self::Completed,
            PlanEntryStatus::InProgress => Self::InProgress,
            _ => Self::Pending,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PlanEntryView {
    pub content: String,
    pub status: PlanEntryStatusView,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PlanView {
    pub entries: Vec<PlanEntryView>,
}

/// Pi 后台任务的展示态。每次 widget 推送都是全量覆盖，输出只保留尾部。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTaskView {
    pub id: String,
    pub title: String,
    pub command: String,
    pub status: BackgroundTaskStatus,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub output: String,
    pub started_at_ms: u64,
    #[serde(default)]
    pub finished_at_ms: Option<u64>,
    /// 挂接任务（后台子智能体）是哪次工具调用派出去的。界面据此把那张派发卡片
    /// 画成还在后台跑；宿主自己起的命令没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackgroundTaskStatus {
    Running,
    Completed,
    Failed,
    Stopped,
    #[serde(rename = "timed_out")]
    TimedOut,
}

impl BackgroundTaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "进行中",
            Self::Completed => "完成",
            Self::Failed => "失败",
            Self::Stopped => "已停止",
            Self::TimedOut => "超时",
        }
    }
}

const BACKGROUND_TASK_OUTPUT_CAP: usize = 8_000;

const BACKGROUND_TASK_LIMIT: usize = 12;

/// 解析 `smelt-background-tasks` widget 的一行 JSON。
/// `tasks` 缺失时返回 None，调用方保留上一份；空数组表示清空。
pub fn parse_background_task_widget(line: &str) -> Option<Vec<BackgroundTaskView>> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let tasks = value.get("tasks")?.as_array()?;
    let mut parsed = Vec::new();
    for task in tasks {
        let Ok(mut view) = serde_json::from_value::<BackgroundTaskView>(task.clone()) else {
            continue;
        };
        if view.id.is_empty() || view.command.is_empty() {
            continue;
        }
        view.output = cap_task_output(view.output);
        parsed.push(view);
    }
    if parsed.len() > BACKGROUND_TASK_LIMIT {
        parsed.drain(0..parsed.len() - BACKGROUND_TASK_LIMIT);
    }
    Some(parsed)
}

fn cap_task_output(text: String) -> String {
    if text.len() <= BACKGROUND_TASK_OUTPUT_CAP {
        return text;
    }
    let mut start = text.len() - BACKGROUND_TASK_OUTPUT_CAP;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

/// 一次用户 prompt 对应的回合计时。旧快照没有这个字段。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TurnTiming {
    pub user_index: usize,
    pub started_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
}

impl TurnTiming {
    pub fn completed_elapsed_ms(&self) -> Option<u64> {
        self.ended_at_ms?.checked_sub(self.started_at_ms)
    }
}

/// Provider 实际暴露的工具调试信息。它与聊天条目分离，避免普通消息模型承载
/// 仅供审计的原始参数；按 tool call id 关联，旧历史缺失时不做反推。
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCallDebug {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<serde_json::Value>,
}

/// Pi 在实际组装 system prompt 时一并报告的工具定义。只包含会送给模型的
/// name/description/parameters 和来源，不包含 provider 请求头、凭据或环境变量。
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDebugTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Pi 在 provider 请求 hook 中暴露的模型身份。只记录 provider/model/api/effort，
/// 不记录 base URL、认证配置或环境变量；headers 走独立脱敏 sidecar。
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDebugModel {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDebugHeaderCapture {
    pub captured_at_ms: u64,
    pub headers: serde_json::Value,
    #[serde(default)]
    pub redacted_paths: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDebugResponseMetadata {
    pub captured_at_ms: u64,
    pub status: u16,
    pub headers: serde_json::Value,
    #[serde(default)]
    pub redacted_paths: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDebugRequestConfig {
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub tools: Vec<RuntimeDebugTool>,
}

/// 一次模型调用的完整 Pi Context、请求头、provider payload 和规范化响应。Pi 和 Rust
/// 两层都会递归脱敏；Pi API 不暴露原始响应流。sequence 是本地顺序，不冒充 provider request id。
/// turn 是 Pi 当前分支中从用户消息计数得到的序号；captured_at_ms 是扩展观察时间，
/// 不代表 provider 端的时间戳。
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDebugModelCall {
    #[serde(default)]
    pub sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_sequence: Option<u64>,
    #[serde(default)]
    pub captured_at_ms: u64,
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_source: Option<String>,
    #[serde(default)]
    pub model: RuntimeDebugModel,
    #[serde(default)]
    pub request_config: RuntimeDebugRequestConfig,
    #[serde(default)]
    pub pi_context: serde_json::Value,
    #[serde(default)]
    pub pi_context_redacted_paths: Vec<String>,
    #[serde(default)]
    pub request_headers: Vec<RuntimeDebugHeaderCapture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_captured_at_ms: Option<u64>,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_captured_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_redacted_paths: Option<Vec<String>>,
    #[serde(default)]
    pub response_metadata: Vec<RuntimeDebugResponseMetadata>,
    #[serde(default)]
    pub redacted_paths: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDebugCompactionMessage {
    pub segment: String,
    pub role: String,
    pub preview: String,
    pub truncated: bool,
}

/// Pi compaction 生命周期中实际可观察到的边界和摘要。来源消息保留完整文本；
/// 图片二进制仍不进入轨迹。
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDebugCompaction {
    pub sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    pub status: String,
    pub reason: String,
    pub will_retry: bool,
    pub started_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_kept_entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_before: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_split_turn: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summarized_message_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_prefix_message_count: Option<usize>,
    #[serde(default)]
    pub source_messages: Vec<RuntimeDebugCompactionMessage>,
    #[serde(default)]
    pub request_headers: Vec<RuntimeDebugHeaderCapture>,
    #[serde(default)]
    pub response_metadata: Vec<RuntimeDebugResponseMetadata>,
    /// 旧版有界预览的计数；当前完整采集路径恒为 0。
    #[serde(default)]
    pub source_messages_omitted: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_extension: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aborted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Pi 实际上报的模型调试数据。v1/v2 为旧版单次快照；v3 保留逐次请求/响应，
/// 并记录明确上报的 compaction 生命周期。通用 ACP 没有等价能力时保持空值，不能从
/// 聊天内容或当前会话配置反推。由于完整 prompt/payload 可能敏感，这些记录是运行时
/// 调试 sidecar，不作为普通会话 transcript 持久化。
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDebug {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub tools: Vec<RuntimeDebugTool>,
    #[serde(default)]
    pub model_calls: Vec<RuntimeDebugModelCall>,
    #[serde(default)]
    pub model_calls_omitted: u64,
    #[serde(default)]
    pub compactions: Vec<RuntimeDebugCompaction>,
    #[serde(default)]
    pub compactions_omitted: u64,
    /// 只接收旧版 v2 widget 的单数 `modelCall`，规范化后不再向 GUI 序列化。
    #[serde(default, skip_serializing)]
    pub model_call: Option<RuntimeDebugModelCall>,
}

impl RuntimeDebugModelCall {
    /// 父进程镜像留的账本：序号、模型和时间还在，请求体与响应体不留。
    fn ledger(&self) -> Self {
        Self {
            sequence: self.sequence,
            turn: self.turn,
            compaction_sequence: self.compaction_sequence,
            captured_at_ms: self.captured_at_ms,
            source: self.source.clone(),
            payload_source: self.payload_source.clone(),
            model: self.model.clone(),
            request_captured_at_ms: self.request_captured_at_ms,
            response_captured_at_ms: self.response_captured_at_ms,
            response_metadata: self.response_metadata.clone(),
            redacted_paths: self.redacted_paths.clone(),
            pi_context_redacted_paths: self.pi_context_redacted_paths.clone(),
            request_headers: self.request_headers.clone(),
            response_redacted_paths: self.response_redacted_paths.clone(),
            ..Self::default()
        }
    }
}

impl RuntimeDebugCompaction {
    fn ledger(&self) -> Self {
        let source_messages_omitted = self
            .source_messages_omitted
            .saturating_add(self.source_messages.len());
        Self {
            sequence: self.sequence,
            turn: self.turn,
            status: self.status.clone(),
            reason: self.reason.clone(),
            will_retry: self.will_retry,
            started_at_ms: self.started_at_ms,
            finished_at_ms: self.finished_at_ms,
            first_kept_entry_id: self.first_kept_entry_id.clone(),
            tokens_before: self.tokens_before,
            is_split_turn: self.is_split_turn,
            previous_summary: self.previous_summary.clone(),
            summarized_message_count: self.summarized_message_count,
            turn_prefix_message_count: self.turn_prefix_message_count,
            source_messages_omitted,
            summary: self.summary.clone(),
            usage: self.usage.clone(),
            from_extension: self.from_extension,
            aborted: self.aborted,
            error_message: self.error_message.clone(),
            request_headers: self.request_headers.clone(),
            response_metadata: self.response_metadata.clone(),
            source_messages: Vec::new(),
        }
    }
}

impl RuntimeDebug {
    /// 去掉会把进程撑大的正文。轨迹窗口要的完整请求只在变更帧上走一遭，不进父镜像。
    pub fn ledger(&self) -> Self {
        Self {
            version: self.version,
            source: self.source.clone(),
            system_prompt: None,
            tools: Vec::new(),
            model_calls: self
                .model_calls
                .iter()
                .map(RuntimeDebugModelCall::ledger)
                .collect(),
            model_calls_omitted: self.model_calls_omitted,
            compactions: self
                .compactions
                .iter()
                .map(RuntimeDebugCompaction::ledger)
                .collect(),
            compactions_omitted: self.compactions_omitted,
            model_call: None,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// smeltd → GUI 的完整快照：`acp_watch`/`acp_open` 接上时发一份，之后每次
/// `apply_event` 有实质变化再发一份。entries 使用尾片增量；大体积 runtime debug
/// sidecar 只在变更帧、显式全量恢复和初始 attach 时携带，避免完整模型历史随每个
/// 流式 token 重复传输。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(from = "ConversationSnapshotDe", into = "ConversationSnapshotDe")]
pub struct ConversationSnapshot {
    /// `entries` 应替换本地历史的起始下标。0 表示完整快照；大于 0 表示增量尾片。
    pub entries_offset: usize,
    /// 生成快照时服务端持有的完整历史长度。旧快照缺少该字段时由客户端根据
    /// `entries_offset + entries.len()` 推导。
    pub entries_total: usize,
    /// Monotonic version assigned by smeltd whenever it publishes this session.
    pub snapshot_revision: u64,
    /// Agent 通过通用 ACP 上报的标题优先，否则从完整历史首条用户消息生成稳定
    /// 兜底。分页快照可能不包含首条消息，客户端不能只靠当前页反推标题。
    pub session_title: Option<String>,
    /// `session/load` 正在用协议通知重建历史。客户端可据此为批量恢复的未测量
    /// 条目提供高度提示，而不必同步布局整段历史。
    pub replaying_history: bool,
    pub entries: Vec<AcpEntry>,
    /// 工具原始名称/参数 sidecar。`Some` 是整表替换，用来覆盖新增和删除。
    /// `None` 表示这帧没变，或旧 daemon 没提供；消费者必须保留已知值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_debug: Option<BTreeMap<String, ToolCallDebug>>,
    /// 运行时 system prompt、工具定义和最近一次模型调用 sidecar。Some 表示本帧
    /// 给出的权威替换值；None 表示旧 daemon 不支持，或本增量帧刻意省略未变化的
    /// 大字段。两种情况消费者都必须保留已知值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_debug: Option<RuntimeDebug>,
    /// ACP 当前活动，与守护相位同一套枚举。
    pub phase: DaemonPhase,
    /// `phase == Dead` 时的展示文案；活会话为空。
    pub end_reason: String,
    /// `phase == Dead` 时的机器可读原因；旧快照缺失时为 Unknown。
    pub end_kind: AcpEndKind,
    /// daemon 已经受理过的外部投递 id。重连方可据此重放同一个请求而不重复执行。
    pub accepted_delivery_ids: BTreeSet<String>,
    /// 当前 provider 回合对应的外部投递；普通用户 prompt 为 None。
    pub active_delivery_id: Option<String>,
    /// 最近一次 TurnEnded 对应的外部投递，用于把完成事实精确关联到投递方。
    pub completed_delivery_id: Option<String>,
    pub pending_permissions: Vec<PendingPermission>,
    pub pending_elicitation: Option<PendingElicitation>,
    pub status_line: Option<String>,
    /// 当前 ACP 连接使用的运行时 session id。
    pub acp_session_id: Option<String>,
    /// 跨进程恢复时传给 `session/load` 的 canonical history id。
    pub history_session_id: Option<String>,
    /// 这份历史已经确认读不出来。和 `history_session_id` 同属会话身份：
    /// 有 id 时以 id 为准；没有 id 且为真，才清空库里的恢复身份。
    /// 旧快照没有这个字段，按未放弃读。
    pub history_abandoned: bool,
    pub supports_image: bool,
    pub available_commands: Vec<(String, String)>,
    pub usage: Option<(u64, u64)>,
    /// 本会话累计 cache read tokens。旧快照缺省为 None。
    #[serde(default)]
    pub usage_cached_read: Option<u64>,
    /// 本会话累计费用（美元）。旧快照缺省为 None。
    #[serde(default)]
    pub usage_cost: Option<f64>,
    /// 当前窗口各桶估数。旧快照缺省为 None。
    #[serde(default)]
    pub usage_breakdown: Option<crate::acp_conn::ContextUsageBreakdown>,
    pub plan: Option<PlanView>,
    /// Pi 后台任务。旧快照缺省为空；空列表表示当前没有任务。
    #[serde(default)]
    pub background_tasks: Vec<BackgroundTaskView>,
    pub model: Option<ModelState>,
    pub config_options: Vec<SessionConfigState>,
    /// 宿主级会话输入状态。旧 daemon / 独立 session host 不提供时为 None，客户端
    /// 必须保留自己的已知值；主 daemon 对外发布快照时总会填 Some。
    pub conversation_state: Option<crate::conversation::ConversationStateSnapshot>,
    /// 驱动是否支持手动压缩上下文。旧快照缺省为 false。
    #[serde(default)]
    pub supports_compaction: bool,
    /// 驱动是否支持 follow-up / clear_queue 原生队列。旧快照缺省为 false。
    #[serde(default)]
    pub supports_native_queue: bool,
    /// 驱动是否支持编辑并重发最后一条用户消息（Pi 的 fork）。旧快照缺省为 false。
    #[serde(default)]
    pub supports_rewind: bool,
    /// 正在压缩上下文。
    #[serde(default)]
    pub compacting: bool,
    /// Provider 正在自动重试模型请求。旧快照缺省为 None。
    #[serde(default)]
    pub provider_retry: Option<ProviderRetryView>,
    /// 用户该看到的一次性提示。旧快照缺省为 None。
    #[serde(default)]
    pub notice: Option<AcpNoticeView>,
    /// 守护进程闸门后面排队的消息。旧快照缺省为空。
    #[serde(default)]
    pub pending_prompts: Vec<PendingPromptView>,
    /// Provider 侧 steering 队列。
    #[serde(default)]
    pub queued_steering: Vec<String>,
    /// Provider 侧 follow-up 队列。
    #[serde(default)]
    pub queued_follow_up: Vec<String>,
    /// `clear_queue` 还回输入框的单调版本。0 表示从未还原。
    #[serde(default)]
    pub composer_restore_revision: u64,
    /// 与 `composer_restore_revision` 配套、尚未被客户端确认写入输入框的原文。
    /// 确认后清掉文本和图片，保留 revision 水位，避免迟到确认误伤后续恢复。
    #[serde(default)]
    pub composer_restore_texts: Vec<String>,
    /// 同一版还原里的图片。只有图没有字时也在。
    #[serde(default)]
    pub composer_restore_images: Vec<crate::acp_chat::AcpImage>,
    /// 当前回合开始的 Unix 毫秒时间戳；None = 当前没有运行中的回合。
    pub turn_started_at_ms: Option<u64>,
    /// 每个已发出 prompt 的回合耗时。`user_index` 是该回合用户消息在完整
    /// `entries` 里的下标。
    #[serde(default)]
    pub turn_timings: Vec<TurnTiming>,
    /// 回合结束且没人看过 → 「有结果可看」绿点，跟旧版 `completed_unread` 同一
    /// 边沿，只是现在从服务端算，客户端不用自己维护。具体结果必须结合
    /// `turn_outcome`，不能把取消/拒绝一律解释成成功。
    pub completed_unread: bool,
    /// 最近一次 `TurnEnded` 的结果。`None` 表示还没有回合结果，不是成功。
    pub turn_outcome: Option<AcpTurnOutcome>,
    /// 这份快照值不值得触发一次落盘。**不是**"数据有没有变"（每次推送数据
    /// 都变了），是旧版 `apply_event` 里 `skip_persist` 那条线的服务端版本：
    /// 流式增量（AgentChunk/Plan/Model/Usage）推快照是为了实时画面，但不该
    /// 把每次落盘都变成写盘风暴——完整内容在 TurnEnded 时已经在 entries 里
    /// 了，那时候存一次就够。客户端拿这个字段决定要不要 `cx.emit(Changed)`，
    /// 不用自己在两次快照之间做增量判断。
    pub should_persist: bool,
    /// 活状态已经生效，但历史正文读失败。空表示这帧的历史可用。
    /// 客户端不能因为这个字段把相位退回启动中。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_error: Option<String>,
    /// 交接文件故意没带已提交正文。恢复后向 agent `session/load` 重放。
    /// 旧交接文件没有这个字段，仍按其中的 `entries` 恢复。
    #[serde(default)]
    pub history_omitted: bool,
    /// 这帧只带正文尾部和相位。命令、配置、计划、队列、计时等信封没变，
    /// 接收方必须保留上一份，不能把空列表当成清空。
    #[serde(default, skip_serializing_if = "is_false")]
    pub envelope_omitted: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ConversationSnapshotDe {
    #[serde(default)]
    pub entries_offset: usize,
    #[serde(default)]
    pub entries_total: usize,
    #[serde(default)]
    pub snapshot_revision: u64,
    #[serde(default)]
    pub session_title: Option<String>,
    #[serde(default)]
    pub replaying_history: bool,
    pub entries: Vec<AcpEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_debug: Option<BTreeMap<String, ToolCallDebug>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_debug: Option<RuntimeDebug>,
    pub phase: SnapshotPhase,
    #[serde(default)]
    pub end_reason: String,
    #[serde(default)]
    pub end_kind: AcpEndKind,
    #[serde(default)]
    pub accepted_delivery_ids: BTreeSet<String>,
    #[serde(default)]
    pub active_delivery_id: Option<String>,
    #[serde(default)]
    pub completed_delivery_id: Option<String>,
    #[serde(default)]
    pub pending_permissions: Vec<PendingPermission>,
    pub pending_elicitation: Option<PendingElicitation>,
    pub status_line: Option<String>,
    pub acp_session_id: Option<String>,
    #[serde(default)]
    pub history_session_id: Option<String>,
    #[serde(default)]
    pub history_abandoned: bool,
    pub supports_image: bool,
    pub available_commands: Vec<(String, String)>,
    pub usage: Option<(u64, u64)>,
    #[serde(default)]
    pub usage_cached_read: Option<u64>,
    #[serde(default)]
    pub usage_cost: Option<f64>,
    #[serde(default)]
    pub usage_breakdown: Option<crate::acp_conn::ContextUsageBreakdown>,
    pub plan: Option<PlanView>,
    #[serde(default)]
    pub background_tasks: Vec<BackgroundTaskView>,
    pub model: Option<ModelState>,
    pub config_options: Vec<SessionConfigState>,
    #[serde(default)]
    pub conversation_state: Option<crate::conversation::ConversationStateSnapshot>,
    #[serde(default)]
    pub supports_compaction: bool,
    #[serde(default)]
    pub supports_native_queue: bool,
    #[serde(default)]
    pub supports_rewind: bool,
    #[serde(default)]
    pub compacting: bool,
    #[serde(default)]
    pub provider_retry: Option<ProviderRetryView>,
    #[serde(default)]
    pub notice: Option<AcpNoticeView>,
    #[serde(default)]
    pub pending_prompts: Vec<PendingPromptView>,
    #[serde(default)]
    pub queued_steering: Vec<String>,
    #[serde(default)]
    pub queued_follow_up: Vec<String>,
    #[serde(default)]
    pub composer_restore_revision: u64,
    #[serde(default)]
    pub composer_restore_texts: Vec<String>,
    #[serde(default)]
    pub composer_restore_images: Vec<crate::acp_chat::AcpImage>,
    #[serde(default)]
    pub turn_started_at_ms: Option<u64>,
    #[serde(default)]
    pub turn_timings: Vec<TurnTiming>,
    pub completed_unread: bool,
    #[serde(default)]
    pub turn_outcome: Option<AcpTurnOutcome>,
    pub should_persist: bool,
    #[serde(default)]
    pub history_error: Option<String>,
    #[serde(default)]
    pub history_omitted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub envelope_omitted: bool,
}

impl From<ConversationSnapshotDe> for ConversationSnapshot {
    fn from(de: ConversationSnapshotDe) -> Self {
        Self {
            entries_offset: de.entries_offset,
            entries_total: de.entries_total,
            snapshot_revision: de.snapshot_revision,
            session_title: de.session_title,
            replaying_history: de.replaying_history,
            entries: de.entries,
            tool_debug: de.tool_debug,
            runtime_debug: de.runtime_debug,
            phase: de.phase.phase,
            end_reason: if de.end_reason.is_empty() {
                de.phase.ended_reason
            } else {
                de.end_reason
            },
            end_kind: de.end_kind,
            accepted_delivery_ids: de.accepted_delivery_ids,
            active_delivery_id: de.active_delivery_id,
            completed_delivery_id: de.completed_delivery_id,
            pending_permissions: de.pending_permissions,
            pending_elicitation: de.pending_elicitation,
            status_line: de.status_line,
            acp_session_id: de.acp_session_id,
            history_session_id: de.history_session_id,
            history_abandoned: de.history_abandoned,
            supports_image: de.supports_image,
            available_commands: de.available_commands,
            usage: de.usage,
            usage_cached_read: de.usage_cached_read,
            usage_cost: de.usage_cost,
            usage_breakdown: de.usage_breakdown,
            plan: de.plan,
            background_tasks: de.background_tasks,
            model: de.model,
            config_options: de.config_options,
            conversation_state: de.conversation_state,
            supports_compaction: de.supports_compaction,
            supports_native_queue: de.supports_native_queue,
            supports_rewind: de.supports_rewind,
            compacting: de.compacting,
            provider_retry: de.provider_retry,
            notice: de.notice,
            pending_prompts: de.pending_prompts,
            queued_steering: de.queued_steering,
            queued_follow_up: de.queued_follow_up,
            composer_restore_revision: de.composer_restore_revision,
            composer_restore_texts: de.composer_restore_texts,
            composer_restore_images: de.composer_restore_images,
            turn_started_at_ms: de.turn_started_at_ms,
            turn_timings: de.turn_timings,
            completed_unread: de.completed_unread,
            turn_outcome: de.turn_outcome,
            should_persist: de.should_persist,
            history_error: de.history_error,
            history_omitted: de.history_omitted,
            envelope_omitted: de.envelope_omitted,
        }
    }
}

impl From<ConversationSnapshot> for ConversationSnapshotDe {
    fn from(snap: ConversationSnapshot) -> Self {
        Self {
            entries_offset: snap.entries_offset,
            entries_total: snap.entries_total,
            snapshot_revision: snap.snapshot_revision,
            session_title: snap.session_title,
            replaying_history: snap.replaying_history,
            entries: snap.entries,
            tool_debug: snap.tool_debug,
            runtime_debug: snap.runtime_debug,
            phase: SnapshotPhase {
                phase: snap.phase,
                ended_reason: String::new(),
            },
            end_reason: snap.end_reason,
            end_kind: snap.end_kind,
            accepted_delivery_ids: snap.accepted_delivery_ids,
            active_delivery_id: snap.active_delivery_id,
            completed_delivery_id: snap.completed_delivery_id,
            pending_permissions: snap.pending_permissions,
            pending_elicitation: snap.pending_elicitation,
            status_line: snap.status_line,
            acp_session_id: snap.acp_session_id,
            history_session_id: snap.history_session_id,
            history_abandoned: snap.history_abandoned,
            supports_image: snap.supports_image,
            available_commands: snap.available_commands,
            usage: snap.usage,
            usage_cached_read: snap.usage_cached_read,
            usage_cost: snap.usage_cost,
            usage_breakdown: snap.usage_breakdown,
            plan: snap.plan,
            background_tasks: snap.background_tasks,
            model: snap.model,
            config_options: snap.config_options,
            conversation_state: snap.conversation_state,
            supports_compaction: snap.supports_compaction,
            supports_native_queue: snap.supports_native_queue,
            supports_rewind: snap.supports_rewind,
            compacting: snap.compacting,
            provider_retry: snap.provider_retry,
            notice: snap.notice,
            pending_prompts: snap.pending_prompts,
            queued_steering: snap.queued_steering,
            queued_follow_up: snap.queued_follow_up,
            composer_restore_revision: snap.composer_restore_revision,
            composer_restore_texts: snap.composer_restore_texts,
            composer_restore_images: snap.composer_restore_images,
            turn_started_at_ms: snap.turn_started_at_ms,
            turn_timings: snap.turn_timings,
            completed_unread: snap.completed_unread,
            turn_outcome: snap.turn_outcome,
            should_persist: snap.should_persist,
            history_error: snap.history_error,
            history_omitted: snap.history_omitted,
            envelope_omitted: snap.envelope_omitted,
        }
    }
}

/// 快照相位只认 [`DaemonPhase`] 的 snake_case。旧名字（`Idle`、`Starting`、
/// `Running`、`Ended`）不再读入。
pub struct SnapshotPhase {
    pub phase: DaemonPhase,
    pub ended_reason: String,
}

pub fn snapshot_phase_from_value(value: &serde_json::Value) -> Result<SnapshotPhase, String> {
    match value {
        serde_json::Value::String(name) => {
            snapshot_phase_from_name(name).map(|phase| SnapshotPhase {
                phase,
                ended_reason: String::new(),
            })
        }
        _ => Err(format!("无法识别的相位：{value}")),
    }
}

fn snapshot_phase_from_name(name: &str) -> Result<DaemonPhase, String> {
    match name {
        "connecting" => Ok(DaemonPhase::Connecting),
        "thinking" => Ok(DaemonPhase::Thinking),
        "executing_tool" => Ok(DaemonPhase::ExecutingTool),
        "awaiting_approval" => Ok(DaemonPhase::AwaitingApproval),
        "waiting_for_user" => Ok(DaemonPhase::WaitingForUser),
        "succeeded" => Ok(DaemonPhase::Succeeded),
        "failed" => Ok(DaemonPhase::Failed),
        "idle" => Ok(DaemonPhase::Idle),
        "dead" => Ok(DaemonPhase::Dead),
        _ => Err(format!("无法识别的相位：{name}")),
    }
}

impl serde::Serialize for SnapshotPhase {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.phase.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for SnapshotPhase {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        snapshot_phase_from_value(&serde_json::Value::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

/// GUI → smeltd 的用户动作，走 `acp_open` 连接的 JSON 行。prompt/取消/切模型
/// 三种转发进连接线程原有的 `ConversationCommand`；权限/选择题四种直接消费
/// `AcpSessionState` 自己攥着的 responder，不经过连接线程（那几种压根不是发给
/// agent 的 JSON-RPC 请求，是在回上一条来自 agent 的请求）。`PromptImage`
/// 复用连接层已有的 wire 形状，没有另造一份。
///
/// 没有 `Shutdown`：关闭子进程是会话生命周期层面的事，走独立的 `acp_kill` op
/// （同终端会话的 `kill`），不是"在一条打开的连接里发的一个动作"——GUI 断开
/// `acp_open` 连接（切标签/关标签/退出 App）只是摘掉这条连接，会话照样在
/// smeltd 里活着，这正是这一整层要解决的问题。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AcpUserAction {
    /// 独立 session host 重连后请求 offset=0 的完整权威快照。普通 GUI 不需要
    /// 主动发送；即使发送也只是一次无副作用的状态刷新。
    Refresh,
    /// 独立 session host 内部重启 provider。让宿主使用自己仍保有的完整 launch
    /// 与临时凭据，避免主 daemon exec 后从无密钥镜像重建时丢失运行环境。
    RestartRuntime,
    Prompt {
        text: String,
        images: Vec<PromptImage>,
        /// 外部投递的稳定 id；普通用户输入缺省为 None。
        #[serde(default)]
        delivery_id: Option<String>,
    },
    Cancel,
    SetConfigOption {
        config_id: String,
        value_id: String,
        /// 旧动作没有这个字段，按 select 值 id 发送。
        #[serde(default)]
        boolean: Option<bool>,
    },
    /// 侧栏重命名后把新名字同步给 agent。旧客户端不会发这个动作。
    SetSessionTitle {
        title: String,
    },
    /// 本机 provider 凭据或模型配置变了，请 agent 重拉一次模型目录。
    /// 旧客户端不会发；不支持的驱动静默忽略。
    RefreshModels,
    /// 等当前回合完全结束后再送。不支持原生队列的驱动按普通 Prompt 排队。
    FollowUp {
        text: String,
        images: Vec<PromptImage>,
        #[serde(default)]
        delivery_id: Option<String>,
    },
    /// 手动压缩上下文。
    Compact,
    /// GUI 已将指定 revision 的恢复文本写入输入框。daemon 只在它仍是当前
    /// revision 时清空文本；动作幂等，迟到确认不会误清后续恢复。
    AcknowledgeComposerRestore {
        revision: u64,
    },
    /// 清空 provider 侧队列并把原文还回输入框，不中止当前回合。
    ClearQueue,
    /// 停掉会话的一个后台任务（后台命令或后台子智能体）。和模型调停止是同一件事：
    /// 结果照常交回会话，但不叫醒模型。旧 daemon 不认识，会回 unknown variant。
    StopBackgroundTask {
        id: String,
    },
    /// 把原生队列里的这一条从 provider 拿走，停掉当前回答，等 provider 真正空闲后只发一次。
    /// 再发同一个文本是同一个动作，不会再发一遍。旧 daemon 不认识，会回 unknown variant。
    DeliverQueuedNow {
        text: String,
    },
    /// 撤回守护进程队列里还没发出的一条（`PendingPromptView::id`），正文和附图回输入框。
    /// 已经发出、或不是用户自己发的，不动。旧 daemon 不认识，会回 unknown variant。
    WithdrawPendingPrompt {
        id: u64,
    },
    /// 编辑并重发最后一条用户消息：agent 切到该消息之前（Pi 的 fork），消息原文
    /// 回输入框供修改。`entry_index` 是投影里的绝对 entry 下标。仅空闲
    /// 会话可回退；旧 daemon 不认识这个动作，会回 unknown variant 错误。
    RewindToMessage {
        entry_index: usize,
    },
    PermissionSelect {
        tool_call_id: String,
        option_id: String,
    },
    ElicitationChoose {
        field_ix: usize,
        opt_ix: usize,
    },
    ElicitationText {
        field_ix: usize,
        value: String,
    },
    ElicitationSubmit,
    ElicitationDismiss,
    /// 父进程镜像不再常驻已提交正文。分页向 session host 要绝对下标 `before` 之前的一页。
    /// 只在主 daemon 与宿主之间使用；GUI 不发这个动作。
    HistoryPage {
        request_id: u64,
        before: usize,
        limit: usize,
    },
}

/// 一个本端解析不了的用户动作。
///
/// 区分两种情况是有意义的：对端比本端新（发来了这一版还不认识的动作）通常只需要
/// 升级守护进程，而畸形 JSON 是真的坏消息。两者都只让这一次动作失败，不拆连接。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownUserAction {
    /// 动作名。能认出名字说明消息形状是对的，只是本端不认识这个动作。
    pub name: Option<String>,
    /// 本端比对端旧（收到了未知的动作名），而不是消息本身坏掉。
    pub newer_peer: bool,
    pub detail: String,
}

impl UnknownUserAction {
    /// 回给对端、也写进日志的一句话。
    pub fn message(&self) -> String {
        match (&self.name, self.newer_peer) {
            (Some(name), true) => {
                format!("unsupported ACP action `{name}`: this build is older than the client")
            }
            (Some(name), false) => format!("invalid ACP action `{name}`: {}", self.detail),
            (None, _) => format!("invalid ACP action: {}", self.detail),
        }
    }
}

/// 解析一个用户动作，把「本端不认识」和「根本不是个动作」分开。
///
/// `AcpUserAction` 是 externally tagged enum，serde 的 `#[serde(other)]` 只支持
/// internally / adjacently tagged 形状，对它用不了；而换标记方式会改变 wire 格式，
/// 把所有旧端一起打死。所以兜底只能放在解析这一处。
pub fn parse_user_action(
    value: serde_json::Value,
) -> Result<AcpUserAction, Box<UnknownUserAction>> {
    let error = match serde_json::from_value::<AcpUserAction>(value.clone()) {
        Ok(action) => return Ok(action),
        Err(error) => error,
    };
    let detail = error.to_string();
    Err(Box::new(UnknownUserAction {
        name: user_action_name(&value),
        // serde 对未知变体固定报 `unknown variant \`X\`, expected one of ...`。
        // 认错了只影响这句提示文案，不影响「这次动作失败」这个行为本身。
        newer_peer: detail.starts_with("unknown variant"),
        detail,
    }))
}

/// 从 wire 形状里取动作名：unit variant 是裸字符串，带数据的是单键对象。
fn user_action_name(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(name) => Some(name.clone()),
        serde_json::Value::Object(map) if map.len() == 1 => map.keys().next().cloned(),
        _ => None,
    }
}

/// 会话的活状态。不含历史正文。窗口靠它离开「正在启动」；
/// 历史读失败不能把相位退回去。
#[derive(Clone, Debug)]
pub struct SessionLive {
    pub phase: DaemonPhase,
    pub end_reason: String,
    pub end_kind: AcpEndKind,
    pub acp_session_id: Option<String>,
    pub history_session_id: Option<String>,
    pub session_title: Option<String>,
    pub status_line: Option<String>,
    /// 已提交历史的长度。`None` 表示这帧仍把历史放在快照 `entries` 里（旧守护）。
    pub history_len: Option<usize>,
    /// 旧帧在快照解析失败时用来保住当前回合。新守护不再填写：正文只在快照 `entries`。
    pub open_turn: Vec<crate::acp_chat::AcpEntry>,
}

impl SessionLive {
    pub fn from_snapshot(snap: &ConversationSnapshot) -> Self {
        Self {
            phase: snap.phase,
            end_reason: snap.end_reason.clone(),
            end_kind: snap.end_kind,
            acp_session_id: snap.acp_session_id.clone(),
            history_session_id: snap.history_session_id.clone(),
            session_title: snap.session_title.clone(),
            status_line: snap.status_line.clone(),
            history_len: None,
            open_turn: Vec::new(),
        }
    }

    /// 从 `live` 对象或快照对象里取出活状态。相位只认当前 snake_case。
    pub fn from_json(value: &serde_json::Value) -> Result<Self, String> {
        let phase_value = value.get("phase").ok_or_else(|| "缺少相位".to_string())?;
        let decoded = snapshot_phase_from_value(phase_value)?;
        let stored_reason = value
            .get("end_reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let end_reason = if stored_reason.is_empty() {
            decoded.ended_reason
        } else {
            stored_reason.to_string()
        };
        let end_kind = value
            .get("end_kind")
            .and_then(|kind| serde_json::from_value(kind.clone()).ok())
            .unwrap_or_default();
        Ok(Self {
            phase: decoded.phase,
            end_reason,
            end_kind,
            acp_session_id: json_string(value, "acp_session_id"),
            history_session_id: json_string(value, "history_session_id"),
            session_title: json_string(value, "session_title"),
            status_line: json_string(value, "status_line"),
            history_len: value
                .get("history_len")
                .and_then(serde_json::Value::as_u64)
                .map(|len| len as usize),
            open_turn: value
                .get("open_turn")
                .and_then(|entries| serde_json::from_value(entries.clone()).ok())
                .unwrap_or_default(),
        })
    }
}

#[derive(serde::Deserialize)]
struct SessionLiveDe {
    phase: DaemonPhase,
    #[serde(default)]
    end_reason: String,
    #[serde(default)]
    end_kind: AcpEndKind,
    #[serde(default)]
    acp_session_id: Option<String>,
    #[serde(default)]
    history_session_id: Option<String>,
    #[serde(default)]
    session_title: Option<String>,
    #[serde(default)]
    status_line: Option<String>,
    #[serde(default)]
    history_len: Option<usize>,
    #[serde(default)]
    open_turn: Vec<crate::acp_chat::AcpEntry>,
}

impl From<SessionLiveDe> for SessionLive {
    fn from(value: SessionLiveDe) -> Self {
        Self {
            phase: value.phase,
            end_reason: value.end_reason,
            end_kind: value.end_kind,
            acp_session_id: value.acp_session_id,
            history_session_id: value.history_session_id,
            session_title: value.session_title,
            status_line: value.status_line,
            history_len: value.history_len,
            open_turn: value.open_turn,
        }
    }
}

impl<'de> serde::Deserialize<'de> for SessionLive {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        SessionLiveDe::deserialize(deserializer).map(Self::from)
    }
}

/// 守护推给窗口的一帧。正文只在 `snapshot`；`live` 是相位和历史边界。
/// 恢复身份的放弃标志在 `snapshot.history_abandoned` 上，和会话状态同一份。
///
/// 这是整段帧。长连接上之后的帧是相对它的增量 `{"delta": …}`，接收方用
/// [`crate::acp_wire::FrameMirror`] 还原成这个形状再解析。
#[derive(serde::Serialize)]
pub struct AcpSnapshotFrame<'a> {
    pub live: &'a SessionLive,
    pub snapshot: &'a ConversationSnapshot,
    pub provider_pid: Option<i32>,
}

impl AcpSnapshotFrame<'_> {
    pub fn to_line_bytes(&self) -> Vec<u8> {
        let mut bytes =
            serde_json::to_vec(self).expect("ACP snapshot frame contains only serializable data");
        bytes.push(b'\n');
        bytes
    }
}

impl serde::Serialize for SessionLive {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("SessionLive", 9)?;
        state.serialize_field("phase", &self.phase)?;
        state.serialize_field("end_reason", &self.end_reason)?;
        state.serialize_field("end_kind", &self.end_kind)?;
        state.serialize_field("acp_session_id", &self.acp_session_id)?;
        state.serialize_field("history_session_id", &self.history_session_id)?;
        state.serialize_field("session_title", &self.session_title)?;
        state.serialize_field("status_line", &self.status_line)?;
        state.serialize_field("history_len", &self.history_len)?;
        state.serialize_field("open_turn", &self.open_turn)?;
        state.end()
    }
}

fn json_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}
