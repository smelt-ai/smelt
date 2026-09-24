//! ACP 会话的无 GPUI 状态机：`ConversationEvent` → 可展示状态的归约逻辑，原来长在
//! GPUI 视图（原 `crates/smelt/src/acp_view.rs`，现 `crates/smelt-acp-view`），现在挪到这——smeltd 要托管 ACP
//! 会话（GUI 退出不中断），谁持有连接谁就得跑这份归约（见 `ConversationEvent::Permission`/
//! `Elicitation` 带的 responder：绑在连接线程上的一次性回执，没法跨进程传，
//! 所以「谁接手连接」这件事没有选择余地，只能是 smeltd）。
//!
//! 分两层类型：
//! - `AcpSessionState`：服务端（smeltd）持有的完整活体状态，permission/
//!   elicitation 待办卡片里揣着真正的 responder，只能在本进程内消费。
//! - `ConversationSnapshot`：`AcpSessionState` 去掉 responder 之后能序列化的镜像，是
//!   smeltd → GUI（以后是 → web/mobile）那条 wire 的唯一内容。GUI 侧只认这份
//!   快照，再也不碰 `agent_client_protocol` 的 schema 类型。
//!
//! 回中的动作走反方向：GUI 发 `AcpUserAction`（纯数据，无 responder），smeltd
//! 收到后要么转发进连接线程的 `ConversationCommand`（Prompt/Cancel/SetModel/Shutdown），
//! 要么直接消费自己攥着的 responder（PermissionSelect/Elicitation*）。

use std::collections::{BTreeMap, BTreeSet};

use agent_client_protocol::schema::v1::{
    ElicitationContentValue, PermissionOptionKind, Plan, PlanEntryStatus, StopReason,
};

use crate::acp_chat::AcpEntry;
use crate::acp_conn::{
    ElicitField, ElicitFieldKind, ElicitOption, ElicitationResponder, ModelState,
    PermissionResponder, PromptImage, SessionConfigState,
};
use crate::daemon_state::DaemonPhase;

mod apply;
pub use apply::{
    ApplyOutcome, acknowledge_composer_restore, apply_event, finalize_dangling_tool_calls,
};
use apply::{pending_action_phase, resume_running};

// ===================== wire 快照类型（无 agent_client_protocol 依赖） =====================

/// ACP 快照线上仍用这组历史变体名（`Starting` / `Running` / `Ended`…），
/// 内存里立刻转成 [`DaemonPhase`]。混升期间 GUI 与 smeltd 必须能互读。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
enum AcpPhaseWire {
    Starting,
    Idle,
    Running,
    AwaitingApproval,
    AwaitingChoice,
    Ended(String),
}

fn daemon_phase_from_wire(phase: AcpPhaseWire) -> (DaemonPhase, String) {
    match phase {
        AcpPhaseWire::Starting => (DaemonPhase::Connecting, String::new()),
        AcpPhaseWire::Idle => (DaemonPhase::Idle, String::new()),
        AcpPhaseWire::Running => (DaemonPhase::Thinking, String::new()),
        AcpPhaseWire::AwaitingApproval => (DaemonPhase::AwaitingApproval, String::new()),
        AcpPhaseWire::AwaitingChoice => (DaemonPhase::WaitingForUser, String::new()),
        AcpPhaseWire::Ended(reason) => (DaemonPhase::Dead, reason),
    }
}

fn daemon_phase_to_wire(phase: DaemonPhase, end_reason: &str) -> AcpPhaseWire {
    match phase {
        DaemonPhase::Connecting => AcpPhaseWire::Starting,
        DaemonPhase::Idle | DaemonPhase::Succeeded => AcpPhaseWire::Idle,
        DaemonPhase::Thinking | DaemonPhase::ExecutingTool => AcpPhaseWire::Running,
        DaemonPhase::AwaitingApproval => AcpPhaseWire::AwaitingApproval,
        DaemonPhase::WaitingForUser => AcpPhaseWire::AwaitingChoice,
        DaemonPhase::Dead | DaemonPhase::Failed => AcpPhaseWire::Ended(end_reason.to_string()),
    }
}

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
    fn from_stop_reason(reason: StopReason, cancel_requested: bool) -> Self {
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
    pub(crate) fn from_acp(k: PermissionOptionKind) -> Self {
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
    /// 语义——GUI 要能画出「已经点了哪些」，不能重连一次就清空選択态。
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
    fn from_acp(s: PlanEntryStatus) -> Self {
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

pub(super) fn plan_view_from_acp(p: &Plan) -> PlanView {
    PlanView {
        entries: p
            .entries
            .iter()
            .map(|e| PlanEntryView {
                content: e.content.clone(),
                status: PlanEntryStatusView::from_acp(e.status.clone()),
            })
            .collect(),
    }
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
    /// 确认后只清文本而保留 revision 水位，避免迟到确认误伤后续恢复。
    #[serde(default)]
    pub composer_restore_texts: Vec<String>,
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
    /// 最近一次 `TurnEnded` 的真实结果。旧快照缺失时为 None，兼容解释为成功。
    pub turn_outcome: Option<AcpTurnOutcome>,
    /// 这份快照值不值得触发一次落盘。**不是**"数据有没有变"（每次推送数据
    /// 都变了），是旧版 `apply_event` 里 `skip_persist` 那条线的服务端版本：
    /// 流式增量（AgentChunk/Plan/Model/Usage）推快照是为了实时画面，但不该
    /// 把每次落盘都变成写盘风暴——完整内容在 TurnEnded 时已经在 entries 里
    /// 了，那时候存一次就够。客户端拿这个字段决定要不要 `cx.emit(Changed)`，
    /// 不用自己在两次快照之间做增量判断。
    pub should_persist: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ConversationSnapshotDe {
    #[serde(default)]
    entries_offset: usize,
    #[serde(default)]
    entries_total: usize,
    #[serde(default)]
    snapshot_revision: u64,
    #[serde(default)]
    session_title: Option<String>,
    #[serde(default)]
    replaying_history: bool,
    entries: Vec<AcpEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_debug: Option<BTreeMap<String, ToolCallDebug>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime_debug: Option<RuntimeDebug>,
    phase: AcpPhaseWire,
    #[serde(default)]
    end_reason: String,
    #[serde(default)]
    end_kind: AcpEndKind,
    #[serde(default)]
    accepted_delivery_ids: BTreeSet<String>,
    #[serde(default)]
    active_delivery_id: Option<String>,
    #[serde(default)]
    completed_delivery_id: Option<String>,
    #[serde(default)]
    pending_permissions: Vec<PendingPermission>,
    pending_elicitation: Option<PendingElicitation>,
    status_line: Option<String>,
    acp_session_id: Option<String>,
    #[serde(default)]
    history_session_id: Option<String>,
    supports_image: bool,
    available_commands: Vec<(String, String)>,
    usage: Option<(u64, u64)>,
    #[serde(default)]
    usage_cached_read: Option<u64>,
    #[serde(default)]
    usage_cost: Option<f64>,
    #[serde(default)]
    usage_breakdown: Option<crate::acp_conn::ContextUsageBreakdown>,
    plan: Option<PlanView>,
    model: Option<ModelState>,
    config_options: Vec<SessionConfigState>,
    #[serde(default)]
    conversation_state: Option<crate::conversation::ConversationStateSnapshot>,
    #[serde(default)]
    supports_compaction: bool,
    #[serde(default)]
    supports_native_queue: bool,
    #[serde(default)]
    supports_rewind: bool,
    #[serde(default)]
    compacting: bool,
    #[serde(default)]
    queued_steering: Vec<String>,
    #[serde(default)]
    queued_follow_up: Vec<String>,
    #[serde(default)]
    composer_restore_revision: u64,
    #[serde(default)]
    composer_restore_texts: Vec<String>,
    #[serde(default)]
    turn_started_at_ms: Option<u64>,
    #[serde(default)]
    turn_timings: Vec<TurnTiming>,
    completed_unread: bool,
    #[serde(default)]
    turn_outcome: Option<AcpTurnOutcome>,
    should_persist: bool,
}

impl From<ConversationSnapshotDe> for ConversationSnapshot {
    fn from(de: ConversationSnapshotDe) -> Self {
        let (phase, ended_reason) = daemon_phase_from_wire(de.phase);
        let end_reason = if ended_reason.is_empty() {
            de.end_reason
        } else {
            ended_reason
        };
        Self {
            entries_offset: de.entries_offset,
            entries_total: de.entries_total,
            snapshot_revision: de.snapshot_revision,
            session_title: de.session_title,
            replaying_history: de.replaying_history,
            entries: de.entries,
            tool_debug: de.tool_debug,
            runtime_debug: de.runtime_debug,
            phase,
            end_reason,
            end_kind: de.end_kind,
            accepted_delivery_ids: de.accepted_delivery_ids,
            active_delivery_id: de.active_delivery_id,
            completed_delivery_id: de.completed_delivery_id,
            pending_permissions: de.pending_permissions,
            pending_elicitation: de.pending_elicitation,
            status_line: de.status_line,
            acp_session_id: de.acp_session_id,
            history_session_id: de.history_session_id,
            supports_image: de.supports_image,
            available_commands: de.available_commands,
            usage: de.usage,
            usage_cached_read: de.usage_cached_read,
            usage_cost: de.usage_cost,
            usage_breakdown: de.usage_breakdown,
            plan: de.plan,
            model: de.model,
            config_options: de.config_options,
            conversation_state: de.conversation_state,
            supports_compaction: de.supports_compaction,
            supports_native_queue: de.supports_native_queue,
            supports_rewind: de.supports_rewind,
            compacting: de.compacting,
            queued_steering: de.queued_steering,
            queued_follow_up: de.queued_follow_up,
            composer_restore_revision: de.composer_restore_revision,
            composer_restore_texts: de.composer_restore_texts,
            turn_started_at_ms: de.turn_started_at_ms,
            turn_timings: de.turn_timings,
            completed_unread: de.completed_unread,
            turn_outcome: de.turn_outcome,
            should_persist: de.should_persist,
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
            phase: daemon_phase_to_wire(snap.phase, &snap.end_reason),
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
            supports_image: snap.supports_image,
            available_commands: snap.available_commands,
            usage: snap.usage,
            usage_cached_read: snap.usage_cached_read,
            usage_cost: snap.usage_cost,
            usage_breakdown: snap.usage_breakdown,
            plan: snap.plan,
            model: snap.model,
            config_options: snap.config_options,
            conversation_state: snap.conversation_state,
            supports_compaction: snap.supports_compaction,
            supports_native_queue: snap.supports_native_queue,
            supports_rewind: snap.supports_rewind,
            compacting: snap.compacting,
            queued_steering: snap.queued_steering,
            queued_follow_up: snap.queued_follow_up,
            composer_restore_revision: snap.composer_restore_revision,
            composer_restore_texts: snap.composer_restore_texts,
            turn_started_at_ms: snap.turn_started_at_ms,
            turn_timings: snap.turn_timings,
            completed_unread: snap.completed_unread,
            turn_outcome: snap.turn_outcome,
            should_persist: snap.should_persist,
        }
    }
}

// ===================== 服务端活体状态 =====================

/// 待审批卡片：`responder` 只在收到 `AcpUserAction::PermissionSelect` 时消费。
pub struct LivePermission {
    pub question: String,
    pub tool_call_id: String,
    pub options: Vec<PermissionOptionView>,
    pub details: ApprovalDetailsView,
    pub responder: Option<PermissionResponder>,
    /// 这张卡对应请求的原始 JSON-RPC 行，smeltd 无缝升级时用来重放（见
    /// `ConversationEvent::Permission` 同名字段）。
    pub raw_request_line: Option<String>,
}

/// 选择题卡片：字段原始形态保留在 `raw_fields`（翻译回
/// `ElicitationContentValue` 要用），`chosen` 是当前選択态。
pub struct LiveElicitation {
    pub message: String,
    pub raw_fields: Vec<ElicitField>,
    pub chosen: BTreeMap<usize, Vec<usize>>,
    pub text_values: BTreeMap<usize, String>,
    pub responder: Option<ElicitationResponder>,
    /// `session/load` may reconstruct an unanswered AskUserQuestion from a tool call instead of
    /// replaying its elicitation request. Track that tool so a later terminal status can retire
    /// the synthetic card. Live protocol elicitations leave this as `None`.
    pub recovered_tool_call_id: Option<String>,
    /// 同 `LivePermission::raw_request_line`。
    pub raw_request_line: Option<String>,
}

/// smeltd 侧一份 ACP 会话的完整活体状态。协议事件走 `apply_event`，用户动作走
/// `apply_user_action` / `note_*`；连接级结束（所有权冲突、provider 没退出、
/// 传输断开）走 [`force_end`]。smeltd 不得再直接写 `phase`。
///
/// `phase` 是当前活动，直接存 [`DaemonPhase`]。归约器只写 Connecting / Idle /
/// Thinking / AwaitingApproval / WaitingForUser / Dead；Succeeded、Failed、
/// ExecutingTool 是守护广播层的投影。
pub struct AcpSessionState {
    pub entries: Vec<AcpEntry>,
    pub tool_debug: BTreeMap<String, ToolCallDebug>,
    /// 只在内存里递增。快照不带它。连接用它判断这帧要不要再带整张工具参数表。
    pub tool_debug_generation: u64,
    pub runtime_debug: RuntimeDebug,
    /// Agent 通过通用 ACP `session_info_update` 上报的标题。没有时由首条用户消息
    /// 生成稳定兜底；这不是某个 provider 的专属能力。
    pub protocol_title: Option<String>,
    pub phase: DaemonPhase,
    /// `phase == Dead` 时给对话页看的原因；活会话为空。
    pub end_reason: String,
    pub end_kind: AcpEndKind,
    pub accepted_delivery_ids: BTreeSet<String>,
    pub active_delivery_id: Option<String>,
    pub completed_delivery_id: Option<String>,
    pub permissions: Vec<LivePermission>,
    pub elicitation: Option<LiveElicitation>,
    pub completed_unread: bool,
    pub turn_outcome: Option<AcpTurnOutcome>,
    pub status_line: Option<String>,
    /// 当前 ACP 连接使用的运行时 session id。
    pub acp_session_id: Option<String>,
    /// 已确认的历史会话 id。恢复成功建立新 runtime 连接时也不能覆盖它。
    pub history_session_id: Option<String>,
    pub supports_image: bool,
    /// 「等自己刚发那条 prompt 的回声」，见旧版字段同名注释——语义原样保留。
    pub awaiting_user_echo: bool,
    /// `session/load` 正在重建历史投影。回放通知和实时通知共用同一套 ACP
    /// update，必须在本地记住这段边界，避免历史 assistant/tool 消息把空闲
    /// 会话误标成 Running。下一次用户 prompt 发出时结束回放态。
    pub replaying_history: bool,
    pub available_commands: Vec<(String, String)>,
    pub usage: Option<(u64, u64)>,
    pub usage_cached_read: Option<u64>,
    pub usage_cost: Option<f64>,
    pub usage_breakdown: Option<crate::acp_conn::ContextUsageBreakdown>,
    pub plan: Option<PlanView>,
    pub model: Option<ModelState>,
    pub config_options: Vec<SessionConfigState>,
    pub supports_compaction: bool,
    pub supports_native_queue: bool,
    pub supports_rewind: bool,
    pub compacting: bool,
    pub queued_steering: Vec<String>,
    /// 与 `queued_steering` 对齐的附图。快照只发文本，活体状态用来在真正插入时还原。
    pub queued_steering_images: Vec<Vec<crate::acp_chat::AcpImage>>,
    pub queued_follow_up: Vec<String>,
    pub composer_restore_revision: u64,
    pub composer_restore_texts: Vec<String>,
    pub turn_started_at_ms: Option<u64>,
    pub turn_timings: Vec<TurnTiming>,
    /// 已被取消的工具调用 id。部分 adapter 会在 `TurnEnded(Cancelled)` 之后
    /// 迟到发送工具更新；只冻结这些具体工具，不能用会话级标记，否则下一轮
    /// 的正常工具也会被误判成已取消。
    cancelled_tool_call_ids: BTreeSet<String>,
    /// 当前回合世代。每次真正发出 prompt 加一；流式事件没有协议回合号，
    /// 用它区分「已取消的那一轮」和「下一轮」。
    turn_seq: u64,
    /// 最近一次被取消的回合世代。该世代上的无归属事件和工具一律丢弃/标失败。
    cancelled_turn_seq: Option<u64>,
    /// 每个 tool_call_id 第一次出现时所在的回合世代。
    tool_turn_seq: BTreeMap<String, u64>,
    /// 用户已经请求停止当前 turn。少数 adapter 会在接收 session/cancel 后以
    /// EndTurn 而非 Cancelled 回应；保留该事实，仍按取消收尾。
    cancel_requested: bool,
    /// `terminal/create` 的输出可能早于引用它的 tool_call 到达。
    terminal_buffers: BTreeMap<String, crate::acp_terminal::TerminalSnapshot>,
}

impl Default for AcpSessionState {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            tool_debug: BTreeMap::new(),
            tool_debug_generation: 0,
            runtime_debug: RuntimeDebug::default(),
            protocol_title: None,
            phase: DaemonPhase::Connecting,
            end_reason: String::new(),
            end_kind: AcpEndKind::Unknown,
            accepted_delivery_ids: BTreeSet::new(),
            active_delivery_id: None,
            completed_delivery_id: None,
            permissions: Vec::new(),
            elicitation: None,
            completed_unread: false,
            turn_outcome: None,
            status_line: None,
            acp_session_id: None,
            history_session_id: None,
            supports_image: true,
            awaiting_user_echo: false,
            replaying_history: false,
            available_commands: Vec::new(),
            usage: None,
            usage_cached_read: None,
            usage_cost: None,
            usage_breakdown: None,
            plan: None,
            model: None,
            config_options: Vec::new(),
            supports_compaction: false,
            supports_native_queue: false,
            supports_rewind: false,
            compacting: false,
            queued_steering: Vec::new(),
            queued_steering_images: Vec::new(),
            queued_follow_up: Vec::new(),
            composer_restore_revision: 0,
            composer_restore_texts: Vec::new(),
            turn_started_at_ms: None,
            turn_timings: Vec::new(),
            cancelled_tool_call_ids: BTreeSet::new(),
            turn_seq: 0,
            cancelled_turn_seq: None,
            tool_turn_seq: BTreeMap::new(),
            cancel_requested: false,
            terminal_buffers: BTreeMap::new(),
        }
    }
}

impl AcpSessionState {
    /// 冷恢复占位：只有落盘的历史消息 + 上次的 agent session id，还没有
    /// 连接。跟旧版 `AcpView::placeholder` 的字段初始化一一对应。
    pub fn placeholder(
        entries: Vec<AcpEntry>,
        resume_session_id: Option<String>,
        reason: String,
    ) -> Self {
        Self {
            entries,
            phase: DaemonPhase::Dead,
            end_reason: reason,
            end_kind: AcpEndKind::Unknown,
            acp_session_id: None,
            history_session_id: resume_session_id,
            ..Self::default()
        }
    }

    /// smeltd 无缝升级续接：从升级前落进交接文件的快照重建活体状态。
    /// `permission`/`elicitation` 故意留空——不是丢了，是这份快照本来就没有
    /// 真正的 responder 可用（那是连接线程内部状态，没法序列化），如果确实
    /// 有一张卡正卡着，`pending_raw_request_line` 抓出来的那行原文会在
    /// `resume_acp_from_fds` 重新接上连接后被回放，SDK 重新解析出等价请求，
    /// 走一遍正常的 `apply_event(Permission/Elicitation)`，到时候会自然把
    /// `permission`/`elicitation` 填回去——不需要（也没法）在这里预置。
    /// 工具参数表变了。没变的快照不要再拷这张表。
    pub fn note_tool_debug_changed(&mut self) {
        self.tool_debug_generation = self.tool_debug_generation.wrapping_add(1);
    }

    pub fn from_snapshot(snap: ConversationSnapshot) -> Self {
        // Running 必须有对应的活跃回合。旧版可能先收到 TurnEnded、再收到 SDK
        // 迟交付的工具通知，把 phase 重新写成 Running，却已经清掉了开始时间。
        // 无缝升级时在边界上修复这种非法组合，避免把僵尸回合继续带进新进程。
        let phase = if snap.phase == DaemonPhase::Thinking && snap.turn_started_at_ms.is_none() {
            DaemonPhase::Idle
        } else {
            snap.phase
        };
        let mut restored = Self {
            entries: snap.entries,
            tool_debug: snap.tool_debug.unwrap_or_default(),
            tool_debug_generation: 0,
            runtime_debug: snap.runtime_debug.unwrap_or_default(),
            protocol_title: snap.session_title,
            phase,
            end_reason: snap.end_reason,
            end_kind: snap.end_kind,
            accepted_delivery_ids: snap.accepted_delivery_ids,
            active_delivery_id: snap.active_delivery_id,
            completed_delivery_id: snap.completed_delivery_id,
            permissions: Vec::new(),
            elicitation: None,
            completed_unread: snap.completed_unread,
            turn_outcome: snap.turn_outcome,
            status_line: snap.status_line,
            acp_session_id: snap.acp_session_id,
            history_session_id: snap.history_session_id,
            supports_image: snap.supports_image,
            awaiting_user_echo: false,
            replaying_history: false,
            available_commands: snap.available_commands,
            usage: snap.usage,
            usage_cached_read: snap.usage_cached_read,
            usage_cost: snap.usage_cost,
            usage_breakdown: snap.usage_breakdown,
            plan: snap.plan,
            model: snap.model,
            config_options: snap.config_options,
            supports_compaction: snap.supports_compaction,
            supports_native_queue: snap.supports_native_queue,
            supports_rewind: snap.supports_rewind,
            compacting: snap.compacting,
            queued_steering: snap.queued_steering,
            queued_steering_images: Vec::new(),
            queued_follow_up: snap.queued_follow_up,
            composer_restore_revision: snap.composer_restore_revision,
            composer_restore_texts: snap.composer_restore_texts,
            turn_started_at_ms: snap.turn_started_at_ms,
            turn_timings: snap.turn_timings,
            cancelled_tool_call_ids: BTreeSet::new(),
            turn_seq: 0,
            cancelled_turn_seq: None,
            tool_turn_seq: BTreeMap::new(),
            cancel_requested: false,
            terminal_buffers: BTreeMap::new(),
        };
        // 上一段连接已经消失，它欠的工具终态不会再来了。回合已结束却留着
        // 未结束工具时，工具卡会一直停在「执行中」，所以在进程边界上一并收尾。
        // 回合仍在进行（Thinking / ExecutingTool）的会话不受影响。
        finalize_dangling_tool_calls(&mut restored);
        restored
    }

    /// 把独立 ACP session host 发来的 wire 快照合并进 daemon 的只读镜像。
    ///
    /// session host 才持有真实 JSON-RPC responder 与回合队列；daemon 这份状态只
    /// 服务于 list/watch/任务对账。因此待审批卡片在这里恢复为 `responder=None`，
    /// 用户动作必须继续转发给 host，不能在这份镜像上调用 `select_permission` /
    /// `submit_elicitation`。
    ///
    /// 返回最早变化的 entry 下标。增量快照的前缀若不在本地，说明 daemon 在
    /// exec/重连窗口漏过了基线；调用方应请求 host 重发 offset=0 的完整快照。
    pub fn merge_hosted_snapshot(&mut self, snap: ConversationSnapshot) -> Result<usize, String> {
        let ConversationSnapshot {
            entries_offset,
            entries_total,
            snapshot_revision: _,
            session_title,
            replaying_history,
            entries,
            tool_debug,
            runtime_debug,
            phase,
            end_reason,
            end_kind,
            accepted_delivery_ids,
            active_delivery_id,
            completed_delivery_id,
            pending_permissions,
            pending_elicitation,
            status_line,
            acp_session_id,
            history_session_id,
            supports_image,
            available_commands,
            usage,
            usage_cached_read,
            usage_cost,
            usage_breakdown,
            plan,
            model,
            config_options,
            conversation_state: _,
            supports_compaction,
            supports_native_queue,
            supports_rewind,
            compacting,
            queued_steering,
            queued_follow_up,
            composer_restore_revision,
            composer_restore_texts,
            turn_started_at_ms,
            turn_timings,
            completed_unread,
            turn_outcome,
            should_persist: _,
        } = snap;

        if entries_offset > self.entries.len() {
            return Err(format!(
                "host 快照缺少历史前缀：offset={entries_offset}, local={}",
                self.entries.len()
            ));
        }
        let merged_len = entries_offset.saturating_add(entries.len());
        if merged_len != entries_total {
            return Err(format!(
                "host 快照不是连续尾片：offset={entries_offset}, tail={}, total={entries_total}",
                entries.len()
            ));
        }

        self.entries.truncate(entries_offset);
        self.entries.extend(entries);
        if let Some(tool_debug) = tool_debug {
            self.tool_debug = tool_debug;
            self.note_tool_debug_changed();
        }
        if let Some(runtime_debug) = runtime_debug {
            self.runtime_debug = runtime_debug;
        }
        self.protocol_title = session_title;
        self.phase = phase;
        self.end_reason = end_reason;
        self.end_kind = end_kind;
        self.accepted_delivery_ids = accepted_delivery_ids;
        self.active_delivery_id = active_delivery_id;
        self.completed_delivery_id = completed_delivery_id;
        self.permissions = pending_permissions
            .into_iter()
            .map(|permission| LivePermission {
                question: permission.question,
                tool_call_id: permission.tool_call_id,
                options: permission.options,
                details: permission.details,
                responder: None,
                raw_request_line: None,
            })
            .collect();
        self.elicitation = pending_elicitation.map(|elicitation| LiveElicitation {
            message: elicitation.message,
            raw_fields: elicitation
                .fields
                .into_iter()
                .map(mirror_elicitation_field)
                .collect(),
            chosen: elicitation.chosen,
            text_values: elicitation.text_values,
            responder: None,
            recovered_tool_call_id: None,
            raw_request_line: None,
        });
        self.status_line = status_line;
        self.acp_session_id = acp_session_id;
        self.history_session_id = history_session_id;
        self.supports_image = supports_image;
        self.awaiting_user_echo = false;
        self.replaying_history = replaying_history;
        self.available_commands = available_commands;
        self.usage = usage;
        self.usage_cached_read = usage_cached_read;
        self.usage_cost = usage_cost;
        self.usage_breakdown = usage_breakdown;
        self.plan = plan;
        self.model = model;
        self.config_options = config_options;
        self.supports_compaction = supports_compaction;
        self.supports_native_queue = supports_native_queue;
        self.supports_rewind = supports_rewind;
        self.compacting = compacting;
        self.queued_steering = queued_steering;
        self.queued_steering_images.clear();
        self.queued_follow_up = queued_follow_up;
        self.composer_restore_revision = composer_restore_revision;
        self.composer_restore_texts = composer_restore_texts;
        self.turn_started_at_ms = turn_started_at_ms;
        self.turn_timings = turn_timings;
        self.completed_unread = completed_unread;
        self.turn_outcome = turn_outcome;
        Ok(entries_offset)
    }

    /// 当前有没有一张卡（权限/选择题）正等着人处理，有就带上它原始请求那行
    /// ——smeltd 无缝升级时用来判断"这条会话要不要在交接文件里多带一行"
    /// 以及 resume 时重放这行，见 `resume_acp_from_fds`。同一时刻协议上只会
    /// 有一张卡挂起（agent 等到上一个请求有回应才会发下一个），不用管两者
    /// 都有值的情况。
    pub fn pending_raw_request_line(&self) -> Option<&str> {
        self.permissions
            .iter()
            .find_map(|p| p.raw_request_line.as_deref())
            .or_else(|| {
                self.elicitation
                    .as_ref()
                    .and_then(|e| e.raw_request_line.as_deref())
            })
    }

    /// 协议标题优先；Agent 尚未上报或明确清空时，用首条用户消息兜底。
    pub fn resolved_session_title(&self) -> Option<String> {
        self.protocol_title
            .clone()
            .or_else(|| crate::acp_chat::auto_title(&self.entries))
    }

    /// `should_persist` 不是从 `self` 能算出来的——它是"这次变化是怎么发生的"
    /// 这个上下文信息，调用方（smeltd 的事件循环）从 `apply_event` 的返回值
    /// 里拿，这里只负责原样塞进快照，见该字段注释。
    pub fn to_snapshot(&self, should_persist: bool) -> ConversationSnapshot {
        self.to_snapshot_since(should_persist, 0)
    }

    pub fn to_snapshot_since(
        &self,
        should_persist: bool,
        entries_offset: usize,
    ) -> ConversationSnapshot {
        self.to_snapshot_range(should_persist, entries_offset, self.entries.len())
    }

    pub fn to_snapshot_range(
        &self,
        should_persist: bool,
        entries_offset: usize,
        entries_end: usize,
    ) -> ConversationSnapshot {
        let entries_total = self.entries.len();
        let entries_end = entries_end.min(entries_total);
        let entries_offset = entries_offset.min(entries_end);
        ConversationSnapshot {
            entries_offset,
            entries_total,
            snapshot_revision: 0,
            session_title: self.resolved_session_title(),
            replaying_history: self.replaying_history,
            entries: self.entries[entries_offset..entries_end].to_vec(),
            tool_debug: Some(self.tool_debug.clone()),
            runtime_debug: Some(self.runtime_debug.clone()),
            phase: self.phase,
            end_reason: self.end_reason.clone(),
            end_kind: self.end_kind,
            accepted_delivery_ids: self.accepted_delivery_ids.clone(),
            active_delivery_id: self.active_delivery_id.clone(),
            completed_delivery_id: self.completed_delivery_id.clone(),
            pending_permissions: self
                .permissions
                .iter()
                .map(|p| PendingPermission {
                    question: p.question.clone(),
                    tool_call_id: p.tool_call_id.clone(),
                    options: p.options.clone(),
                    details: p.details.clone(),
                })
                .collect(),
            pending_elicitation: self.elicitation.as_ref().map(|e| PendingElicitation {
                message: e.message.clone(),
                fields: e.raw_fields.iter().map(elicit_field_view).collect(),
                chosen: e.chosen.clone(),
                text_values: e.text_values.clone(),
            }),
            status_line: self.status_line.clone(),
            acp_session_id: self.acp_session_id.clone(),
            history_session_id: self.history_session_id.clone(),
            supports_image: self.supports_image,
            available_commands: self.available_commands.clone(),
            usage: self.usage,
            usage_cached_read: self.usage_cached_read,
            usage_cost: self.usage_cost,
            usage_breakdown: self.usage_breakdown.clone(),
            plan: self.plan.clone(),
            model: self.model.clone(),
            config_options: self.config_options.clone(),
            conversation_state: None,
            supports_compaction: self.supports_compaction,
            supports_native_queue: self.supports_native_queue,
            supports_rewind: self.supports_rewind,
            compacting: self.compacting,
            queued_steering: self.queued_steering.clone(),
            queued_follow_up: self.queued_follow_up.clone(),
            composer_restore_revision: self.composer_restore_revision,
            composer_restore_texts: self.composer_restore_texts.clone(),
            turn_started_at_ms: self.turn_started_at_ms,
            turn_timings: self.turn_timings.clone(),
            completed_unread: self.completed_unread,
            turn_outcome: self.turn_outcome,
            should_persist,
        }
    }
}

/// daemon 镜像只需把 host 快照重新投影给 GUI，不会在本地把字段翻译回 ACP
/// response。选项值使用 label 占位，避免为只读镜像再引入一套平行字段类型。
fn mirror_elicitation_field(field: ElicitFieldView) -> ElicitField {
    let kind = match field.kind {
        ElicitFieldKindView::Select(options) => ElicitFieldKind::Select(
            options
                .into_iter()
                .map(|option| ElicitOption {
                    value: ElicitationContentValue::String(option.label.clone()),
                    label: option.label,
                })
                .collect(),
        ),
        ElicitFieldKindView::MultiSelect(options) => ElicitFieldKind::MultiSelect(
            options
                .into_iter()
                .map(|option| ElicitOption {
                    value: ElicitationContentValue::String(option.label.clone()),
                    label: option.label,
                })
                .collect(),
        ),
        ElicitFieldKindView::Text { secret } => ElicitFieldKind::Text { secret },
        ElicitFieldKindView::ExternalUrl(url) => ElicitFieldKind::ExternalUrl(url),
    };
    ElicitField {
        key: field.key,
        title: field.title,
        required: field.required,
        allow_custom_input: field.allow_custom_input,
        kind,
    }
}

fn elicit_field_view(f: &ElicitField) -> ElicitFieldView {
    ElicitFieldView {
        key: f.key.clone(),
        title: f.title.clone(),
        required: f.required,
        allow_custom_input: f.allow_custom_input,
        kind: match &f.kind {
            ElicitFieldKind::Select(opts) => ElicitFieldKindView::Select(
                opts.iter()
                    .map(|o| ElicitOptionView {
                        label: o.label.clone(),
                    })
                    .collect(),
            ),
            ElicitFieldKind::MultiSelect(opts) => ElicitFieldKindView::MultiSelect(
                opts.iter()
                    .map(|o| ElicitOptionView {
                        label: o.label.clone(),
                    })
                    .collect(),
            ),
            ElicitFieldKind::Text { secret } => ElicitFieldKindView::Text { secret: *secret },
            ElicitFieldKind::ExternalUrl(url) => ElicitFieldKindView::ExternalUrl(url.clone()),
        },
    }
}

/// Build the plain-text reply used by a recovered elicitation whose original responder no longer
/// exists. Live elicitations return `None` and continue through the protocol responder.
pub fn recovered_elicitation_answer(state: &AcpSessionState) -> Option<String> {
    let card = state.elicitation.as_ref()?;
    if card.responder.is_some() {
        return None;
    }
    let mut answers = Vec::new();
    for (ix, field) in card.raw_fields.iter().enumerate() {
        let selected = card.chosen.get(&ix).cloned().unwrap_or_default();
        let mut labels: Vec<String> = match &field.kind {
            ElicitFieldKind::Select(options) | ElicitFieldKind::MultiSelect(options) => selected
                .iter()
                .filter_map(|&option_ix| options.get(option_ix))
                .map(|option| option.label.clone())
                .collect(),
            _ => return None,
        };
        if field.allow_custom_input
            && let Some(text) = card
                .text_values
                .get(&ix)
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
        {
            labels.push(text.to_string());
        }
        if labels.is_empty() {
            return None;
        }
        answers.push(if card.raw_fields.len() == 1 {
            labels.join("、")
        } else {
            format!("{}：{}", field.title, labels.join("、"))
        });
    }
    Some(answers.join("\n"))
}

/// 连接级结束：清掉待办卡片并带上稳定的 [`AcpEndKind`]。
///
/// 不是 ACP 协议事件（那些走 [`apply_event`]），所以 smeltd 的所有权冲突 /
/// provider 未退出 / 传输断开必须走这里，不能直接写 `phase`。
pub fn force_end(state: &mut AcpSessionState, kind: AcpEndKind, msg: impl Into<String>) {
    state.permissions.clear();
    state.elicitation = None;
    state.phase = DaemonPhase::Dead;
    state.end_reason = msg.into();
    state.end_kind = kind;
}

/// 「重新开始」/新建时的相位重置：跟旧版 `AcpView::restart` 里那几行对应（cmd/
/// spawn 那部分是 smeltd 的事，不在这个纯状态函数里）。
pub fn reset_for_restart(state: &mut AcpSessionState) {
    state.permissions.clear();
    state.runtime_debug = RuntimeDebug::default();
    state.elicitation = None;
    state.plan = None;
    state.model = None;
    state.usage = None;
    state.usage_breakdown = None;
    state.completed_unread = false;
    state.turn_outcome = None;
    state.replaying_history = false;
    state.awaiting_user_echo = false;
    state.cancelled_tool_call_ids.clear();
    state.cancelled_turn_seq = None;
    state.tool_turn_seq.clear();
    state.turn_seq = 0;
    state.cancel_requested = false;
    state.acp_session_id = None;
    state.turn_started_at_ms = None;
    state.phase = DaemonPhase::Connecting;
    state.end_reason.clear();
    state.end_kind = AcpEndKind::Unknown;
}

/// 用户发的一条 prompt（本地立即回显 + 打开等回声窗口），跟旧版 `send_prompt`
/// 里非 I/O 的那部分对应（`h.cmd_tx.try_send` 由调用方在成功后自己做，因为
/// 这个函数不持有 `ConversationHandle`）。
pub fn note_prompt_sent(
    state: &mut AcpSessionState,
    text: String,
    images: Vec<crate::acp_chat::AcpImage>,
) {
    note_prompt_sent_with_delivery(state, text, images, None);
}

/// 回合中途插入：先进入 steering 队列，不写消息流。
///
/// 会话气泡只在 Pi 真正从队列里吃掉之后才出现，避免「会话里有了、队列里还在」
/// 的双重完成态。
pub fn queue_mid_turn_input(
    state: &mut AcpSessionState,
    text: String,
    images: Vec<crate::acp_chat::AcpImage>,
) {
    state.queued_steering.push(text);
    state.queued_steering_images.push(images);
}

/// 把一条**已经插入当前回合**的用户消息记进消息流。只在 steering 队列把这条
/// 交出去之后调用。不开新回合：不 bump `turn_seq`、不收尾未完成工具、不重置
/// 计时与 delivery。这些字段属于正在跑的那一轮，改了会让回合归约和时长统计错乱。
pub fn note_mid_turn_input(
    state: &mut AcpSessionState,
    text: String,
    images: Vec<crate::acp_chat::AcpImage>,
) {
    if images.is_empty() {
        state.entries.push(AcpEntry::User(text));
    } else {
        state
            .entries
            .push(AcpEntry::UserWithImages { text, images });
    }
}

/// 与 [`note_prompt_sent`] 相同，但把外部任务的稳定投递 id 绑定到本回合。
pub fn note_prompt_sent_with_delivery(
    state: &mut AcpSessionState,
    text: String,
    images: Vec<crate::acp_chat::AcpImage>,
    delivery_id: Option<String>,
) {
    // 空白会话在 Ready 时不会登记 history_session_id；首条 prompt 真正发出
    // 后，运行时 id 才升级为可跨进程恢复的历史身份。若 prompt 早于 Ready
    // 排队，则由 Ready 分支在看到本地回显后补上。
    if state.history_session_id.is_none() {
        state.history_session_id = state.acp_session_id.clone();
    }
    state.replaying_history = false;
    state.cancel_requested = false;
    // 上一回合若还有未终态工具，用「下一条已发出」作为收尾信号，而不是睡一轮。
    // 之后到达的 ToolFinished 仍可按 tool id 改写（未取消的条目）。
    let _ = finalize_dangling_tool_calls(state);
    state.turn_seq = state.turn_seq.saturating_add(1);
    if images.is_empty() {
        state.entries.push(AcpEntry::User(text));
    } else {
        state
            .entries
            .push(AcpEntry::UserWithImages { text, images });
    }
    state.awaiting_user_echo = true;
    state.phase = DaemonPhase::Thinking;
    state.end_reason.clear();
    state.active_delivery_id = delivery_id;
    state.completed_delivery_id = None;
    state.completed_unread = false;
    state.turn_outcome = None;
    let started_at_ms = unix_time_ms();
    state.turn_started_at_ms = Some(started_at_ms);
    state.turn_timings.push(TurnTiming {
        user_index: state.entries.len().saturating_sub(1),
        started_at_ms,
        ended_at_ms: None,
    });
}

/// 记录用户已向当前 ACP turn 发出了 session/cancel。实际的终态仍由
/// `TurnEnded` 统一归约，避免在 adapter 尚未确认前提前释放 prompt 闸门。
///
/// 待批卡片立刻丢掉：responder Drop 会回 Cancelled，agent 不会继续卡在审批上。
/// 「立即发送」尤其依赖这一点——旧审批若留到下一回合，用户一点就把新对话带偏。
pub fn note_cancel_requested(state: &mut AcpSessionState) {
    state.cancel_requested = true;
    state.permissions.clear();
    state.elicitation = None;
    if matches!(
        state.phase,
        DaemonPhase::AwaitingApproval | DaemonPhase::WaitingForUser
    ) && state.turn_started_at_ms.is_some()
    {
        state.phase = DaemonPhase::Thinking;
    }
}

pub(super) fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

/// 权限审批：按工具调用与 option id 精确定位。批准一项只移除对应卡片，其余
/// 请求继续保持待审批状态；旧按钮也不能误命中另一张卡的同名 option。
pub fn select_permission(state: &mut AcpSessionState, tool_call_id: &str, option_id: &str) {
    let Some(ix) = state.permissions.iter().position(|card| {
        card.tool_call_id == tool_call_id && card.options.iter().any(|o| o.option_id == option_id)
    }) else {
        return;
    };
    let mut card = state.permissions.remove(ix);
    if let Some(responder) = card.responder.take() {
        responder.select(option_id.to_string());
    }
    resume_running(state);
}

/// 选择题点选：单选替换，多选 toggle，跟旧版 `pick_elicit_option` 一致。
/// 返回 true 表示这是「整卡单字段单选」的快捷路径，调用方应该紧接着调用
/// `submit_elicitation`（旧版点了就直接提交，不用等再按一次「确定」）。
pub fn choose_elicitation(state: &mut AcpSessionState, field_ix: usize, opt_ix: usize) -> bool {
    let Some(card) = &mut state.elicitation else {
        return false;
    };
    let Some(field) = card.raw_fields.get(field_ix) else {
        return false;
    };
    match &field.kind {
        ElicitFieldKind::Select(_) => {
            card.chosen.insert(field_ix, vec![opt_ix]);
        }
        ElicitFieldKind::MultiSelect(_) => {
            let sel = card.chosen.entry(field_ix).or_default();
            if let Some(pos) = sel.iter().position(|&i| i == opt_ix) {
                sel.remove(pos);
            } else {
                sel.push(opt_ix);
            }
        }
        ElicitFieldKind::Text { .. } => return false,
        ElicitFieldKind::ExternalUrl(_) => return false,
    }
    card.raw_fields.len() == 1 && matches!(card.raw_fields[0].kind, ElicitFieldKind::Select(_))
}

pub fn set_elicitation_text(state: &mut AcpSessionState, field_ix: usize, value: String) {
    let Some(card) = &mut state.elicitation else {
        return;
    };
    if card.raw_fields.get(field_ix).is_some_and(|field| {
        matches!(field.kind, ElicitFieldKind::Text { .. })
            || (field.allow_custom_input
                && matches!(
                    field.kind,
                    ElicitFieldKind::Select(_) | ElicitFieldKind::MultiSelect(_)
                ))
    }) {
        card.text_values.insert(field_ix, value);
    }
}

/// 提交选择题：把 `chosen` 翻译回 `ElicitationContentValue` 传给 responder。
/// 跟旧版 `submit_elicitation` 一致；字段没有選択就跳过（agent 那边按 schema
/// 自己决定必填与否，这里不做客户端校验）。
pub fn submit_elicitation(state: &mut AcpSessionState) {
    let Some(mut card) = state.elicitation.take() else {
        return;
    };
    let Some(responder) = card.responder.take() else {
        return;
    };
    let mut content = BTreeMap::new();
    for (ix, field) in card.raw_fields.iter().enumerate() {
        // 选项之外自己写的答案：非空即作为该字段的值；多选时追加到末尾。
        let custom = card
            .text_values
            .get(&ix)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty());
        match &field.kind {
            ElicitFieldKind::Select(options) => {
                if let Some(text) = custom {
                    content.insert(
                        field.key.clone(),
                        ElicitationContentValue::String(text.to_string()),
                    );
                } else if let Some(opt) = card
                    .chosen
                    .get(&ix)
                    .and_then(|sel| sel.first())
                    .and_then(|&i| options.get(i))
                {
                    content.insert(field.key.clone(), opt.value.clone());
                }
            }
            ElicitFieldKind::MultiSelect(options) => {
                let mut values: Vec<String> = card
                    .chosen
                    .get(&ix)
                    .map(|sel| {
                        sel.iter()
                            .filter_map(|&i| options.get(i))
                            .filter_map(|o| match &o.value {
                                ElicitationContentValue::String(s) => Some(s.clone()),
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(text) = custom {
                    values.push(text.to_string());
                }
                if !values.is_empty() {
                    content.insert(
                        field.key.clone(),
                        ElicitationContentValue::StringArray(values),
                    );
                }
            }
            ElicitFieldKind::Text { .. } => {
                if let Some(value) = card.text_values.get(&ix).filter(|value| !value.is_empty()) {
                    content.insert(
                        field.key.clone(),
                        ElicitationContentValue::String(value.clone()),
                    );
                }
            }
            ElicitFieldKind::ExternalUrl(_) => {}
        }
    }
    responder.accept(content);
    resume_running(state);
}

/// 「跳过」：丢卡片，responder Drop 自动回 Cancel（见 `ElicitationResponder`
/// 的 Drop 实现）。
pub fn dismiss_elicitation(state: &mut AcpSessionState) {
    let recovered = state
        .elicitation
        .as_ref()
        .is_some_and(|card| card.responder.is_none());
    state.elicitation = None;
    if recovered {
        state.phase = pending_action_phase(state).unwrap_or(DaemonPhase::Idle);
    } else {
        resume_running(state);
    }
}

/// 一份 turn 结束/连接终止后要不要自动续接（冷恢复占位第一次被访问时）：
/// 有旧 session id 才值得——没有 id 只能开全新会话，交给用户手动决定。
pub fn should_auto_resume(state: &AcpSessionState) -> bool {
    matches!(state.phase, DaemonPhase::Dead) && state.history_session_id.is_some()
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
}

#[cfg(test)]
mod tests;
