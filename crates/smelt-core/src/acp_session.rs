//! ACP 会话的无 GPUI 状态机：`AcpEvent` → 可展示状态的归约逻辑，原来长在
//! `crates/smelt/src/acp_view.rs`（GPUI 绑定），现在挪到这——smeltd 要托管 ACP
//! 会话（GUI 退出不中断），谁持有连接谁就得跑这份归约（见 `AcpEvent::Permission`/
//! `Elicitation` 带的 responder：绑在连接线程上的一次性回执，没法跨进程传，
//! 所以「谁接手连接」这件事没有选择余地，只能是 smeltd）。
//!
//! 分两层类型：
//! - `AcpSessionState`：服务端（smeltd）持有的完整活体状态，permission/
//!   elicitation 待办卡片里揣着真正的 responder，只能在本进程内消费。
//! - `AcpSnapshot`：`AcpSessionState` 去掉 responder 之后能序列化的镜像，是
//!   smeltd → GUI（以后是 → web/mobile）那条 wire 的唯一内容。GUI 侧只认这份
//!   快照，再也不碰 `agent_client_protocol` 的 schema 类型。
//!
//! 回中的动作走反方向：GUI 发 `AcpUserAction`（纯数据，无 responder），smeltd
//! 收到后要么转发进连接线程的 `AcpCommand`（Prompt/Cancel/SetModel/Shutdown），
//! 要么直接消费自己攥着的 responder（PermissionSelect/Elicitation*）。

use std::collections::BTreeMap;

use agent_client_protocol::schema::v1::{
    ElicitationContentValue, PermissionOptionKind, Plan, PlanEntryStatus,
};

use crate::acp_chat::AcpEntry;
use crate::acp_conn::{
    AcpEvent, ElicitField, ElicitFieldKind, ElicitationResponder, ModelState, PermissionResponder,
    PromptImage, ReadyKind, SessionConfigState,
};

// ===================== wire 快照类型（无 agent_client_protocol 依赖） =====================

/// 会话相位。GUI 舞台头胶囊 / 四色状态点都从这个派生。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AcpPhase {
    Starting,
    Idle,
    Running,
    AwaitingApproval,
    AwaitingChoice,
    /// 连接不可恢复地结束（Fatal / 占位恢复），带原因文本。
    Ended(String),
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

fn plan_view_from_acp(p: &Plan) -> PlanView {
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

/// smeltd → GUI 的完整快照：`acp_watch`/`acp_open` 接上时发一份，之后每次
/// `apply_event` 有实质变化再发一份（懒得做增量 diff——快照本身就不大，
/// 消息流几十条 + 几个标量字段，序列化成本远低于维护增量协议的复杂度）。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AcpSnapshot {
    /// `entries` 应替换本地历史的起始下标。0 表示完整快照；大于 0 表示增量尾片。
    #[serde(default)]
    pub entries_offset: usize,
    /// 生成快照时服务端持有的完整历史长度。旧快照缺少该字段时由客户端根据
    /// `entries_offset + entries.len()` 推导。
    #[serde(default)]
    pub entries_total: usize,
    /// Monotonic version assigned by smeltd whenever it publishes this session.
    #[serde(default)]
    pub snapshot_revision: u64,
    /// 从完整历史首条用户消息生成的稳定标题。分页快照可能不包含首条消息，
    /// 客户端不能再只靠当前页反推标题。
    #[serde(default)]
    pub session_title: Option<String>,
    /// `session/load` 正在用协议通知重建历史。客户端可据此为批量恢复的未测量
    /// 条目提供高度提示，而不必同步布局整段历史。
    #[serde(default)]
    pub replaying_history: bool,
    pub entries: Vec<AcpEntry>,
    pub phase: AcpPhase,
    #[serde(default)]
    pub pending_permissions: Vec<PendingPermission>,
    pub pending_elicitation: Option<PendingElicitation>,
    pub status_line: Option<String>,
    /// 当前 ACP 连接使用的运行时 session id。
    pub acp_session_id: Option<String>,
    /// 跨进程恢复时传给 `session/load` 的 canonical history id。
    #[serde(default)]
    pub history_session_id: Option<String>,
    pub supports_image: bool,
    pub available_commands: Vec<(String, String)>,
    pub usage: Option<(u64, u64)>,
    pub plan: Option<PlanView>,
    pub model: Option<ModelState>,
    pub config_options: Vec<SessionConfigState>,
    /// 当前回合开始的 Unix 毫秒时间戳；None = 当前没有运行中的回合。
    #[serde(default)]
    pub turn_started_at_ms: Option<u64>,
    /// 最近完成回合的耗时。开始下一轮时清空。
    #[serde(default)]
    pub last_turn_duration_ms: Option<u64>,
    /// 回合结束且没人看过 → 「有结果可看」绿点，跟旧版 `completed_unread` 同一
    /// 语义，只是现在从服务端算，客户端不用自己维护。
    pub completed_unread: bool,
    /// 这份快照值不值得触发一次落盘。**不是**"数据有没有变"（每次推送数据
    /// 都变了），是旧版 `apply_event` 里 `skip_persist` 那条线的服务端版本：
    /// 流式增量（AgentChunk/Plan/Model/Usage）推快照是为了实时画面，但不该
    /// 把每次落盘都变成写盘风暴——完整内容在 TurnEnded 时已经在 entries 里
    /// 了，那时候存一次就够。客户端拿这个字段决定要不要 `cx.emit(Changed)`，
    /// 不用自己在两次快照之间做增量判断。
    pub should_persist: bool,
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
    /// `AcpEvent::Permission` 同名字段）。
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

/// smeltd 侧一份 ACP 会话的完整活体状态。`apply_event`/`apply_user_action` 是
/// 仅有的两个 mutator，跟旧版 `AcpView::apply_event`/`pick_permission` 等方法
/// 一一对应，去掉的只是 GPUI 相关的那半（`cx.notify()`/`sync_daemon_state`
/// 这些，见 `ApplyOutcome`）。
pub struct AcpSessionState {
    pub entries: Vec<AcpEntry>,
    pub phase: AcpPhase,
    pub permissions: Vec<LivePermission>,
    pub elicitation: Option<LiveElicitation>,
    pub completed_unread: bool,
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
    pub plan: Option<PlanView>,
    pub model: Option<ModelState>,
    pub config_options: Vec<SessionConfigState>,
    pub turn_started_at_ms: Option<u64>,
    pub last_turn_duration_ms: Option<u64>,
}

impl Default for AcpSessionState {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            phase: AcpPhase::Starting,
            permissions: Vec::new(),
            elicitation: None,
            completed_unread: false,
            status_line: None,
            acp_session_id: None,
            history_session_id: None,
            supports_image: true,
            awaiting_user_echo: false,
            replaying_history: false,
            available_commands: Vec::new(),
            usage: None,
            plan: None,
            model: None,
            config_options: Vec::new(),
            turn_started_at_ms: None,
            last_turn_duration_ms: None,
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
            phase: AcpPhase::Ended(reason),
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
    pub fn from_snapshot(snap: AcpSnapshot) -> Self {
        // Running 必须有对应的活跃回合。旧版可能先收到 TurnEnded、再收到 SDK
        // 迟交付的工具通知，把 phase 重新写成 Running，却已经清掉了开始时间。
        // 无缝升级时在边界上修复这种非法组合，避免把僵尸回合继续带进新进程。
        let phase = if matches!(snap.phase, AcpPhase::Running) && snap.turn_started_at_ms.is_none()
        {
            AcpPhase::Idle
        } else {
            snap.phase.clone()
        };
        Self {
            entries: snap.entries,
            phase,
            permissions: Vec::new(),
            elicitation: None,
            completed_unread: snap.completed_unread,
            status_line: snap.status_line,
            acp_session_id: snap.acp_session_id,
            history_session_id: snap.history_session_id,
            supports_image: snap.supports_image,
            awaiting_user_echo: false,
            replaying_history: false,
            available_commands: snap.available_commands,
            usage: snap.usage,
            plan: snap.plan,
            model: snap.model,
            config_options: snap.config_options,
            turn_started_at_ms: snap.turn_started_at_ms,
            last_turn_duration_ms: snap.last_turn_duration_ms,
        }
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

    /// `should_persist` 不是从 `self` 能算出来的——它是"这次变化是怎么发生的"
    /// 这个上下文信息，调用方（smeltd 的事件循环）从 `apply_event` 的返回值
    /// 里拿，这里只负责原样塞进快照，见该字段注释。
    pub fn to_snapshot(&self, should_persist: bool) -> AcpSnapshot {
        self.to_snapshot_since(should_persist, 0)
    }

    pub fn to_snapshot_since(&self, should_persist: bool, entries_offset: usize) -> AcpSnapshot {
        self.to_snapshot_range(should_persist, entries_offset, self.entries.len())
    }

    pub fn to_snapshot_range(
        &self,
        should_persist: bool,
        entries_offset: usize,
        entries_end: usize,
    ) -> AcpSnapshot {
        let entries_total = self.entries.len();
        let entries_end = entries_end.min(entries_total);
        let entries_offset = entries_offset.min(entries_end);
        AcpSnapshot {
            entries_offset,
            entries_total,
            snapshot_revision: 0,
            session_title: crate::acp_chat::auto_title(&self.entries),
            replaying_history: self.replaying_history,
            entries: self.entries[entries_offset..entries_end].to_vec(),
            phase: self.phase.clone(),
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
            plan: self.plan.clone(),
            model: self.model.clone(),
            config_options: self.config_options.clone(),
            turn_started_at_ms: self.turn_started_at_ms,
            last_turn_duration_ms: self.last_turn_duration_ms,
            completed_unread: self.completed_unread,
            should_persist,
        }
    }
}

fn elicit_field_view(f: &ElicitField) -> ElicitFieldView {
    ElicitFieldView {
        key: f.key.clone(),
        title: f.title.clone(),
        required: f.required,
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

/// `apply_event` 的旁路效果——旧版直接在 GPUI `Context` 上做（`cx.notify()`/
/// `cx.emit(Changed)`/推 `PendingAgentNotifs`），归约函数本身不该管这些，
/// 交给调用方（smeltd）根据这份结果自己决定广播/落盘/要不要弹通知。
#[derive(Default)]
pub struct ApplyOutcome {
    /// 值得持久化（entries 有实质变化，排除逐块流式增量）。
    pub should_persist: bool,
    /// 本次事件修改 entries 的最早位置。daemon 从这里发送尾部快照；`None`
    /// 表示 entries 未变化，只需同步 phase / permission 等旁路状态。
    pub entries_offset: Option<usize>,
    /// 相位刚变成需要人处理 → (标题, 正文, is_approval)，调用方决定要不要弹
    /// 通知（GUI 按各类 Agent 通知开关决定；这个决定权不下放到
    /// smeltd，因为那是纯 GUI 展示偏好，smeltd 不该知道）。
    pub notify: Option<(String, String, bool)>,
}

/// 还挂着没人处理的卡片时对应的相位。审批优先于选择题，跟 `Ready` 分支里
/// 那段判断同序。
fn pending_action_phase(state: &AcpSessionState) -> Option<AcpPhase> {
    if !state.permissions.is_empty() {
        Some(AcpPhase::AwaitingApproval)
    } else if state.elicitation.is_some() {
        Some(AcpPhase::AwaitingChoice)
    } else {
        None
    }
}

/// 流式事件（AgentChunk/ToolCall/Plan…）把相位推回 `Running` 时用这个，
/// 而不是直接赋值：卡片没被回答之前，会话对外必须一直是「等你操作」。
///
/// 否则 agent 在发出审批请求后继续推任何一条 update（并行的另一个工具、
/// 一段 thought、一次 plan 刷新都算），相位就会从 `AwaitingApproval` 掉回
/// `Running`，smeltd 的四色相位跟着变成 Thinking/ExecutingTool，
/// `attention::apply_daemon_transition` 便把这条会话的行动项判成"已解决"，
/// 移动端的提醒闪一下就消失——可请求其实还挂着，重新进入会话又能看到。
fn resume_running(state: &mut AcpSessionState) {
    state.phase = pending_action_phase(state).unwrap_or(AcpPhase::Running);
}

/// 事件归约：entries 合并 + phase 机。跟旧版 `AcpView::apply_event` 逐行对应，
/// 唯一的行为差异是旁路效果收进返回值而不是直接执行。
pub fn apply_event(state: &mut AcpSessionState, ev: AcpEvent) -> ApplyOutcome {
    let mut outcome = ApplyOutcome::default();

    let skip_persist = matches!(
        ev,
        AcpEvent::AgentChunk { .. }
            | AcpEvent::Plan(_)
            | AcpEvent::Model(_)
            | AcpEvent::ConfigOptions(_)
            | AcpEvent::Usage { .. }
    );
    if !matches!(
        ev,
        AcpEvent::UserChunk(_)
            | AcpEvent::UserImage(_)
            | AcpEvent::Status(_)
            | AcpEvent::AvailableCommands(_)
            | AcpEvent::Usage { .. }
            | AcpEvent::Plan(_)
            | AcpEvent::Model(_)
            | AcpEvent::ConfigOptions(_)
            | AcpEvent::Ready { .. }
    ) {
        state.awaiting_user_echo = false;
    }

    match ev {
        AcpEvent::AvailableCommands(list) => {
            state.available_commands = list;
        }
        AcpEvent::Usage { used, size, .. } => {
            state.usage = (size > 0).then_some((used, size));
        }
        AcpEvent::Status(msg) => {
            state.status_line = Some(msg);
        }
        AcpEvent::HistoryReplayStarted => {
            outcome.entries_offset = Some(0);
            state.entries.clear();
            state.replaying_history = true;
        }
        AcpEvent::UserChunk(text) => {
            clear_recovered_elicitation_after_replayed_user_message(state);
            if state.awaiting_user_echo {
                // 自己刚发那条的回声——本地已经在 apply_user_action(Prompt) 时显示过了。
            } else {
                outcome.entries_offset = Some(state.entries.len().saturating_sub(1));
                match state.entries.last_mut() {
                    Some(AcpEntry::User(t)) => t.push_str(&text),
                    Some(AcpEntry::UserWithImages { text: t, .. }) => t.push_str(&text),
                    _ => {
                        outcome.entries_offset = Some(state.entries.len());
                        state.entries.push(AcpEntry::User(text));
                    }
                }
            }
        }
        AcpEvent::UserImage(image) => {
            if !state.awaiting_user_echo {
                outcome.entries_offset = Some(state.entries.len().saturating_sub(1));
                match state.entries.last_mut() {
                    Some(AcpEntry::UserWithImages { images, .. }) => images.push(image),
                    Some(AcpEntry::User(_)) => {
                        let Some(AcpEntry::User(text)) = state.entries.pop() else {
                            unreachable!();
                        };
                        state.entries.push(AcpEntry::UserWithImages {
                            text,
                            images: vec![image],
                        });
                    }
                    _ => {
                        outcome.entries_offset = Some(state.entries.len());
                        state.entries.push(AcpEntry::UserWithImages {
                            text: String::new(),
                            images: vec![image],
                        });
                    }
                }
            }
        }
        AcpEvent::Ready {
            session_id,
            kind,
            supports_image,
        } => {
            // 首条 prompt 可以在 `session/new` 完成前先入命令通道。Ready 到达时
            // 它已经本地回显并开始计时，不能把这轮回合重置成 Idle，也不能为
            // 它插入“新会话”分隔线。
            let has_active_turn = state.turn_started_at_ms.is_some();
            state.acp_session_id = Some(session_id.to_string());
            // Fresh session/new 只有在已经有消息（或首条 prompt 已经排队）时
            // 才具备可恢复的历史身份。空白会话的运行时 id 不能拿去走
            // session/load，否则冷恢复会把一个从未产生历史的会话当成旧会话。
            if state.history_session_id.is_none()
                && (!matches!(kind, ReadyKind::Fresh) || !state.entries.is_empty())
            {
                state.history_session_id = Some(session_id.to_string());
            }
            state.supports_image = supports_image;
            if !matches!(kind, ReadyKind::ResumedWithReplay) {
                state.replaying_history = false;
            }
            match kind {
                ReadyKind::ResumedWithReplay => {}
                ReadyKind::ResumedKeepHistory => {}
                ReadyKind::Fresh if !state.entries.is_empty() && !has_active_turn => {
                    outcome.entries_offset = Some(state.entries.len());
                    state.entries.push(AcpEntry::Divider(format!(
                        "新会话 · agent 不记得以上内容 · {}",
                        chrono::Local::now().format("%m-%d %H:%M")
                    )));
                }
                ReadyKind::Fresh => {}
            }
            state.phase = if !state.permissions.is_empty() {
                AcpPhase::AwaitingApproval
            } else if state.elicitation.is_some() {
                AcpPhase::AwaitingChoice
            } else if has_active_turn {
                AcpPhase::Running
            } else {
                AcpPhase::Idle
            };
            state.status_line = None;
        }
        AcpEvent::AgentChunk { thought, text } => {
            outcome.entries_offset = Some(state.entries.len().saturating_sub(1));
            match state.entries.last_mut() {
                Some(AcpEntry::Assistant {
                    text: t,
                    thought: th,
                }) if *th == thought => {
                    t.push_str(&text);
                }
                _ => {
                    outcome.entries_offset = Some(state.entries.len());
                    state.entries.push(AcpEntry::Assistant { text, thought });
                }
            }
            if !state.replaying_history && state.turn_started_at_ms.is_some() {
                resume_running(state);
            }
        }
        AcpEvent::ToolCall(tc) => {
            let tool_call_id = tc.tool_call_id.to_string();
            let tool_status = crate::acp_conn::tool_status_from_acp(tc.status);
            let replayed_elicitation = matches!(
                tool_status,
                crate::acp_chat::ToolCallStatus::Pending
                    | crate::acp_chat::ToolCallStatus::InProgress
            )
            .then(|| recovered_elicitation(&tc.title, tc.raw_input.as_ref(), &tool_call_id))
            .flatten();
            outcome.entries_offset = Some(state.entries.len());
            state.entries.push(AcpEntry::ToolCall {
                id: tool_call_id,
                title: tc.title,
                kind: crate::acp_conn::tool_kind_from_acp(tc.kind),
                status: tool_status,
                output: crate::acp_conn::tool_content_parts(&tc.content),
            });
            if let Some(elicitation) = replayed_elicitation {
                state.elicitation = Some(elicitation);
                state.phase = AcpPhase::AwaitingChoice;
            } else {
                if !state.replaying_history && state.turn_started_at_ms.is_some() {
                    resume_running(state);
                }
            }
        }
        AcpEvent::ToolCallUpdate(u) => {
            let update_id = u.tool_call_id.to_string();
            let update_status = u.fields.status.map(crate::acp_conn::tool_status_from_acp);
            let remains_pending = update_status.is_none_or(|status| {
                matches!(
                    status,
                    crate::acp_chat::ToolCallStatus::Pending
                        | crate::acp_chat::ToolCallStatus::InProgress
                )
            });
            let replayed_elicitation = remains_pending
                .then_some(u.fields.raw_input.as_ref())
                .flatten()
                .and_then(|raw_input| {
                    let title = u.fields.title.as_deref().or_else(|| {
                        state.entries.iter().rev().find_map(|entry| match entry {
                            AcpEntry::ToolCall { id, title, .. } if id == &update_id => {
                                Some(title.as_str())
                            }
                            _ => None,
                        })
                    })?;
                    recovered_elicitation(title, Some(raw_input), &update_id)
                });
            let entry_index = state.entries.iter().rposition(
                |entry| matches!(entry, AcpEntry::ToolCall { id, .. } if id == &update_id),
            );
            if let Some(index) = entry_index
                && let AcpEntry::ToolCall {
                    title,
                    kind,
                    status,
                    output,
                    ..
                } = &mut state.entries[index]
            {
                outcome.entries_offset = Some(index);
                if let Some(t) = u.fields.title {
                    *title = t;
                }
                if let Some(k) = u.fields.kind {
                    *kind = crate::acp_conn::tool_kind_from_acp(k);
                }
                if let Some(s) = update_status {
                    *status = s;
                }
                if let Some(c) = u.fields.content {
                    *output = crate::acp_conn::tool_content_parts(&c);
                }
            }
            if let Some(elicitation) = replayed_elicitation {
                state.elicitation = Some(elicitation);
                state.phase = AcpPhase::AwaitingChoice;
            } else if update_status.is_some_and(|status| {
                matches!(
                    status,
                    crate::acp_chat::ToolCallStatus::Completed
                        | crate::acp_chat::ToolCallStatus::Failed
                )
            }) {
                clear_recovered_elicitation(state, &update_id);
            }
        }
        AcpEvent::ToolStarted { id, title, kind } => {
            outcome.entries_offset = Some(state.entries.len());
            state.entries.push(AcpEntry::ToolCall {
                id,
                title,
                kind,
                status: crate::acp_chat::ToolCallStatus::InProgress,
                output: Vec::new(),
            });
            if !state.replaying_history && state.turn_started_at_ms.is_some() {
                resume_running(state);
            }
        }
        AcpEvent::ToolOutputDelta { id, delta } => {
            let entry_index = state.entries.iter().rposition(
                |entry| matches!(entry, AcpEntry::ToolCall { id: entry_id, .. } if entry_id == &id),
            );
            if let Some(index) = entry_index
                && let AcpEntry::ToolCall { output, .. } = &mut state.entries[index]
            {
                outcome.entries_offset = Some(index);
                match output.last_mut() {
                    Some(crate::acp_chat::ToolOutputPart::Text(text)) => text.push_str(&delta),
                    _ => output.push(crate::acp_chat::ToolOutputPart::Text(delta)),
                }
            }
        }
        AcpEvent::ToolFinished { id, status, output } => {
            let entry_index = state.entries.iter().rposition(
                |entry| matches!(entry, AcpEntry::ToolCall { id: entry_id, .. } if entry_id == &id),
            );
            if let Some(index) = entry_index
                && let AcpEntry::ToolCall {
                    status: current,
                    output: current_output,
                    ..
                } = &mut state.entries[index]
            {
                outcome.entries_offset = Some(index);
                *current = status;
                *current_output = output;
            }
        }
        AcpEvent::Model(m) => {
            state.model = Some(m);
        }
        AcpEvent::ConfigOptions(options) => {
            state.config_options = options;
        }
        AcpEvent::Plan(p) => {
            state.plan = Some(plan_view_from_acp(&p));
            if !state.replaying_history && state.turn_started_at_ms.is_some() {
                resume_running(state);
            }
        }
        AcpEvent::Permission {
            question,
            tool_call_id,
            pub_options,
            responder,
            details,
            raw_request_line,
        } => {
            state.permissions.push(LivePermission {
                question: question.clone(),
                tool_call_id: tool_call_id.to_string(),
                options: pub_options,
                details,
                responder: Some(responder),
                raw_request_line,
            });
            state.phase = AcpPhase::AwaitingApproval;
            outcome.notify = Some(("等你批准".to_string(), question, true));
        }
        AcpEvent::Elicitation {
            message,
            fields,
            responder,
            raw_request_line,
        } => {
            state.elicitation = Some(LiveElicitation {
                message: message.clone(),
                raw_fields: fields,
                chosen: Default::default(),
                text_values: Default::default(),
                responder: Some(responder),
                recovered_tool_call_id: None,
                raw_request_line,
            });
            state.phase = AcpPhase::AwaitingChoice;
            outcome.notify = Some(("等你选择".to_string(), message, false));
        }
        AcpEvent::TurnEnded(reason) => {
            finish_turn(state);
            let _ = reason;
        }
        AcpEvent::Fatal(msg) => {
            state.permissions.clear();
            state.elicitation = None;
            state.phase = AcpPhase::Ended(msg);
        }
    }

    outcome.should_persist = !skip_persist;
    outcome
}

fn finish_turn(state: &mut AcpSessionState) {
    state.permissions.clear();
    state.elicitation = None;
    state.phase = AcpPhase::Idle;
    state.completed_unread = true;
    if let Some(started) = state.turn_started_at_ms.take() {
        state.last_turn_duration_ms = Some(unix_time_ms().saturating_sub(started));
    }
}

/// Some agents replay an unfinished AskUserQuestion as a pending tool call but do not recreate
/// the ACP elicitation responder. The raw tool input still contains the questions and choices, so
/// keep them actionable through a prompt-backed elicitation (`responder: None`).
fn recovered_elicitation(
    title: &str,
    raw_input: Option<&serde_json::Value>,
    tool_call_id: &str,
) -> Option<LiveElicitation> {
    let questions = raw_input?.get("questions")?.as_array()?;
    let mut fields = Vec::new();
    for (ix, question) in questions.iter().enumerate() {
        let prompt = question.get("question")?.as_str()?.trim();
        let options = question.get("options")?.as_array()?;
        let options: Vec<crate::acp_conn::ElicitOption> = options
            .iter()
            .filter_map(|option| option.get("label")?.as_str())
            .map(|label| crate::acp_conn::ElicitOption {
                value: agent_client_protocol::schema::v1::ElicitationContentValue::String(
                    label.to_string(),
                ),
                label: label.to_string(),
            })
            .collect();
        if options.is_empty() {
            return None;
        }
        let kind = if question
            .get("multiSelect")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            crate::acp_conn::ElicitFieldKind::MultiSelect(options)
        } else {
            crate::acp_conn::ElicitFieldKind::Select(options)
        };
        fields.push(crate::acp_conn::ElicitField {
            key: format!("question_{ix}"),
            title: prompt.to_string(),
            required: true,
            kind,
        });
    }
    (!fields.is_empty()).then(|| LiveElicitation {
        message: title.to_string(),
        raw_fields: fields,
        chosen: Default::default(),
        text_values: Default::default(),
        responder: None,
        recovered_tool_call_id: Some(tool_call_id.to_string()),
        raw_request_line: None,
    })
}

fn clear_recovered_elicitation(state: &mut AcpSessionState, tool_call_id: &str) {
    let matches_completed_tool = state
        .elicitation
        .as_ref()
        .is_some_and(|card| card.recovered_tool_call_id.as_deref() == Some(tool_call_id));
    if !matches_completed_tool {
        return;
    }
    state.elicitation = None;
    state.phase = if !state.permissions.is_empty() {
        AcpPhase::AwaitingApproval
    } else if !state.replaying_history && state.turn_started_at_ms.is_some() {
        AcpPhase::Running
    } else {
        AcpPhase::Idle
    };
}

fn clear_recovered_elicitation_after_replayed_user_message(state: &mut AcpSessionState) {
    if !state.replaying_history {
        return;
    }
    let recovered_tool_call_id = state
        .elicitation
        .as_ref()
        .and_then(|card| card.recovered_tool_call_id.clone());
    if let Some(tool_call_id) = recovered_tool_call_id {
        clear_recovered_elicitation(state, &tool_call_id);
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
        let selected = card.chosen.get(&ix)?;
        let labels: Vec<&str> = match &field.kind {
            ElicitFieldKind::Select(options) | ElicitFieldKind::MultiSelect(options) => selected
                .iter()
                .filter_map(|&option_ix| options.get(option_ix))
                .map(|option| option.label.as_str())
                .collect(),
            _ => return None,
        };
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

/// 「重新开始」/新建时的相位重置：跟旧版 `AcpView::restart` 里那几行对应（cmd/
/// spawn 那部分是 smeltd 的事，不在这个纯状态函数里）。
pub fn reset_for_restart(state: &mut AcpSessionState) {
    state.permissions.clear();
    state.elicitation = None;
    state.plan = None;
    state.model = None;
    state.usage = None;
    state.completed_unread = false;
    state.replaying_history = false;
    state.awaiting_user_echo = false;
    state.acp_session_id = None;
    state.turn_started_at_ms = None;
    state.last_turn_duration_ms = None;
    state.phase = AcpPhase::Starting;
}

/// 用户发的一条 prompt（本地立即回显 + 打开等回声窗口），跟旧版 `send_prompt`
/// 里非 I/O 的那部分对应（`h.cmd_tx.try_send` 由调用方在成功后自己做，因为
/// 这个函数不持有 `AcpHandle`）。
pub fn note_prompt_sent(
    state: &mut AcpSessionState,
    text: String,
    images: Vec<crate::acp_chat::AcpImage>,
) {
    // 空白会话在 Ready 时不会登记 history_session_id；首条 prompt 真正发出
    // 后，运行时 id 才升级为可跨进程恢复的历史身份。若 prompt 早于 Ready
    // 排队，则由 Ready 分支在看到本地回显后补上。
    if state.history_session_id.is_none() {
        state.history_session_id = state.acp_session_id.clone();
    }
    state.replaying_history = false;
    if images.is_empty() {
        state.entries.push(AcpEntry::User(text));
    } else {
        state
            .entries
            .push(AcpEntry::UserWithImages { text, images });
    }
    state.awaiting_user_echo = true;
    state.phase = AcpPhase::Running;
    state.completed_unread = false;
    state.turn_started_at_ms = Some(unix_time_ms());
    state.last_turn_duration_ms = None;
}

fn unix_time_ms() -> u64 {
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
    if card
        .raw_fields
        .get(field_ix)
        .is_some_and(|field| matches!(field.kind, ElicitFieldKind::Text { .. }))
    {
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
        match &field.kind {
            ElicitFieldKind::Select(options) => {
                if let Some(opt) = card
                    .chosen
                    .get(&ix)
                    .and_then(|sel| sel.first())
                    .and_then(|&i| options.get(i))
                {
                    content.insert(field.key.clone(), opt.value.clone());
                }
            }
            ElicitFieldKind::MultiSelect(options) => {
                let Some(sel) = card.chosen.get(&ix) else {
                    continue;
                };
                let values: Vec<String> = sel
                    .iter()
                    .filter_map(|&i| options.get(i))
                    .filter_map(|o| match &o.value {
                        ElicitationContentValue::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect();
                content.insert(
                    field.key.clone(),
                    ElicitationContentValue::StringArray(values),
                );
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
        state.phase = pending_action_phase(state).unwrap_or(AcpPhase::Idle);
    } else {
        resume_running(state);
    }
}

/// 一份 turn 结束/连接终止后要不要自动续接（冷恢复占位第一次被访问时）：
/// 有旧 session id 才值得——没有 id 只能开全新会话，交给用户手动决定。
pub fn should_auto_resume(state: &AcpSessionState) -> bool {
    matches!(state.phase, AcpPhase::Ended(_)) && state.history_session_id.is_some()
}

/// GUI → smeltd 的用户动作，走 `acp_open` 连接的 JSON 行。prompt/取消/切模型
/// 三种转发进连接线程原有的 `AcpCommand`；权限/选择题四种直接消费
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
    Prompt {
        text: String,
        images: Vec<PromptImage>,
    },
    Cancel,
    SetConfigOption {
        config_id: String,
        value_id: String,
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
mod tests {
    use super::*;
    use crate::acp_chat::{ToolCallStatus, ToolKind, ToolOutputPart};
    use agent_client_protocol::schema::v1::StopReason;

    fn fresh_state() -> AcpSessionState {
        AcpSessionState::default()
    }

    #[test]
    fn agent_chunk_appends_and_merges_consecutive_same_kind() {
        let mut s = fresh_state();
        note_prompt_sent(&mut s, "hi".into(), Vec::new());
        apply_event(
            &mut s,
            AcpEvent::AgentChunk {
                thought: false,
                text: "he".into(),
            },
        );
        apply_event(
            &mut s,
            AcpEvent::AgentChunk {
                thought: false,
                text: "llo".into(),
            },
        );
        assert_eq!(s.entries.len(), 2);
        assert!(
            matches!(&s.entries[1], AcpEntry::Assistant { text, thought: false } if text == "hello")
        );
        assert!(matches!(s.phase, AcpPhase::Running));
    }

    #[test]
    fn user_echo_suppressed_once_after_prompt_sent() {
        let mut s = fresh_state();
        note_prompt_sent(&mut s, "hi".into(), Vec::new());
        assert!(s.awaiting_user_echo);
        // 回声窗口内收到 UserChunk：吞掉，不重复追加。
        apply_event(&mut s, AcpEvent::UserChunk("hi".into()));
        assert_eq!(s.entries.len(), 1);
        // 任何非 UserChunk/Status/AvailableCommands/Usage 事件都清掉等回声窗口。
        apply_event(
            &mut s,
            AcpEvent::AgentChunk {
                thought: false,
                text: "ok".into(),
            },
        );
        assert!(!s.awaiting_user_echo);
        // 窗口关闭后再来的 UserChunk 是重放历史，正常追加。
        apply_event(&mut s, AcpEvent::UserChunk("old question".into()));
        assert_eq!(s.entries.len(), 3);
        assert!(matches!(&s.entries[2], AcpEntry::User(t) if t == "old question"));
    }

    #[test]
    fn replayed_user_image_is_kept_with_its_text() {
        let mut s = fresh_state();
        apply_event(&mut s, AcpEvent::UserChunk("看这里".into()));
        apply_event(
            &mut s,
            AcpEvent::UserImage(crate::acp_chat::AcpImage {
                mime: "image/png".into(),
                data_b64: "QUJD".into(),
            }),
        );

        assert!(matches!(
            &s.entries[..],
            [AcpEntry::UserWithImages { text, images }]
                if text == "看这里" && images.len() == 1
        ));
    }

    #[test]
    fn ready_does_not_erase_replay_started_before_buffered_updates() {
        let mut s = fresh_state();
        s.entries.push(AcpEntry::User("old".into()));
        s.history_session_id = Some("canonical-history".into());
        apply_event(&mut s, AcpEvent::HistoryReplayStarted);
        assert!(s.entries.is_empty());
        apply_event(
            &mut s,
            AcpEvent::Ready {
                session_id: agent_client_protocol::schema::v1::SessionId::new("runtime-session"),
                kind: ReadyKind::ResumedWithReplay,
                supports_image: true,
            },
        );
        apply_event(&mut s, AcpEvent::UserChunk("replayed question".into()));
        assert!(matches!(
            &s.entries[..],
            [AcpEntry::User(text)] if text == "replayed question"
        ));
        assert_eq!(s.acp_session_id.as_deref(), Some("runtime-session"));
        assert_eq!(s.history_session_id.as_deref(), Some("canonical-history"));
    }

    #[test]
    fn replayed_agent_updates_do_not_leave_session_running() {
        let mut s = fresh_state();
        apply_event(&mut s, AcpEvent::HistoryReplayStarted);
        apply_event(
            &mut s,
            AcpEvent::Ready {
                session_id: agent_client_protocol::schema::v1::SessionId::new("sid-1"),
                kind: ReadyKind::ResumedWithReplay,
                supports_image: true,
            },
        );
        apply_event(
            &mut s,
            AcpEvent::AgentChunk {
                text: "old answer".into(),
                thought: false,
            },
        );

        assert!(matches!(s.phase, AcpPhase::Idle));

        note_prompt_sent(&mut s, "continue".into(), Vec::new());
        assert!(matches!(s.phase, AcpPhase::Running));
        assert!(!s.replaying_history);
    }

    #[test]
    fn load_replay_rebuilds_legacy_projection_without_duplicates() {
        let mut s = fresh_state();
        s.entries.push(AcpEntry::User("old question".into()));
        s.entries.push(AcpEntry::Assistant {
            text: "old answer".into(),
            thought: false,
        });

        apply_event(&mut s, AcpEvent::HistoryReplayStarted);
        apply_event(&mut s, AcpEvent::UserChunk("old question".into()));
        apply_event(
            &mut s,
            AcpEvent::AgentChunk {
                text: "old answer".into(),
                thought: false,
            },
        );
        apply_event(
            &mut s,
            AcpEvent::Ready {
                session_id: agent_client_protocol::schema::v1::SessionId::new("sid-1"),
                kind: ReadyKind::ResumedWithReplay,
                supports_image: true,
            },
        );

        assert_eq!(s.entries.len(), 2);
        assert!(matches!(&s.entries[0], AcpEntry::User(text) if text == "old question"));
        assert!(matches!(
            &s.entries[1],
            AcpEntry::Assistant { text, thought: false } if text == "old answer"
        ));
    }

    #[test]
    fn ready_resumed_keep_history_preserves_local_entries() {
        let mut s = fresh_state();
        s.entries.push(AcpEntry::User("old".into()));
        apply_event(
            &mut s,
            AcpEvent::Ready {
                session_id: agent_client_protocol::schema::v1::SessionId::new("sid-1"),
                kind: ReadyKind::ResumedKeepHistory,
                supports_image: true,
            },
        );
        assert_eq!(s.entries.len(), 1);
    }

    #[test]
    fn ready_fresh_replaces_id_and_preserves_legacy_history_with_divider() {
        let mut s = fresh_state();
        s.entries.push(AcpEntry::User("old".into()));
        s.acp_session_id = Some("old-sid".into());
        apply_event(&mut s, AcpEvent::Status("正在恢复".into()));
        assert_eq!(s.acp_session_id.as_deref(), Some("old-sid"));

        apply_event(
            &mut s,
            AcpEvent::Ready {
                session_id: agent_client_protocol::schema::v1::SessionId::new("new-sid"),
                kind: ReadyKind::Fresh,
                supports_image: true,
            },
        );
        assert_eq!(s.acp_session_id.as_deref(), Some("new-sid"));
        assert_eq!(s.history_session_id.as_deref(), Some("new-sid"));
        assert!(s.status_line.is_none());
        assert_eq!(s.entries.len(), 2);
        assert!(matches!(&s.entries[0], AcpEntry::User(text) if text == "old"));
        assert!(matches!(s.entries[1], AcpEntry::Divider(_)));
    }

    #[test]
    fn ready_fresh_blank_session_does_not_create_history_id() {
        let mut s = fresh_state();

        apply_event(
            &mut s,
            AcpEvent::Ready {
                session_id: agent_client_protocol::schema::v1::SessionId::new("new-sid"),
                kind: ReadyKind::Fresh,
                supports_image: true,
            },
        );

        assert_eq!(s.acp_session_id.as_deref(), Some("new-sid"));
        assert!(s.history_session_id.is_none());
    }

    #[test]
    fn first_prompt_after_ready_makes_blank_session_resumable() {
        let mut s = fresh_state();
        apply_event(
            &mut s,
            AcpEvent::Ready {
                session_id: agent_client_protocol::schema::v1::SessionId::new("new-sid"),
                kind: ReadyKind::Fresh,
                supports_image: true,
            },
        );

        note_prompt_sent(&mut s, "hello".into(), Vec::new());

        assert_eq!(s.history_session_id.as_deref(), Some("new-sid"));
    }

    #[test]
    fn ready_keeps_a_pre_ready_prompt_running_without_a_divider() {
        let mut s = fresh_state();
        note_prompt_sent(&mut s, "你好".into(), Vec::new());
        let started_at = s.turn_started_at_ms;

        apply_event(
            &mut s,
            AcpEvent::Ready {
                session_id: agent_client_protocol::schema::v1::SessionId::new("new-sid"),
                kind: ReadyKind::Fresh,
                supports_image: true,
            },
        );

        assert!(matches!(s.phase, AcpPhase::Running));
        assert_eq!(s.turn_started_at_ms, started_at);
        assert!(s.awaiting_user_echo);
        assert!(matches!(&s.entries[..], [AcpEntry::User(text)] if text == "你好"));

        // session/new 完成后才出现的用户回声也必须吞掉，不能把首条消息重复一遍。
        apply_event(&mut s, AcpEvent::UserChunk("你好".into()));
        assert!(matches!(&s.entries[..], [AcpEntry::User(text)] if text == "你好"));
    }

    #[test]
    fn ready_fresh_does_not_replace_preseeded_history_id() {
        let mut s = fresh_state();
        s.history_session_id = Some("canonical-history".into());

        apply_event(
            &mut s,
            AcpEvent::Ready {
                session_id: agent_client_protocol::schema::v1::SessionId::new("new-runtime"),
                kind: ReadyKind::Fresh,
                supports_image: true,
            },
        );

        assert_eq!(s.acp_session_id.as_deref(), Some("new-runtime"));
        assert_eq!(s.history_session_id.as_deref(), Some("canonical-history"));
    }

    #[test]
    fn turn_ended_clears_pending_cards_and_marks_unread() {
        let mut s = fresh_state();
        s.phase = AcpPhase::Running;
        apply_event(&mut s, AcpEvent::TurnEnded(StopReason::EndTurn));
        assert!(matches!(s.phase, AcpPhase::Idle));
        assert!(s.completed_unread);
    }

    #[test]
    fn late_task_complete_after_turn_ended_does_not_reopen_turn() {
        let mut s = fresh_state();
        note_prompt_sent(&mut s, "do it".into(), Vec::new());
        apply_event(&mut s, AcpEvent::TurnEnded(StopReason::EndTurn));
        assert!(matches!(s.phase, AcpPhase::Idle));

        apply_event(
            &mut s,
            AcpEvent::ToolCall(
                agent_client_protocol::schema::v1::ToolCall::new("done", "task_complete")
                    .kind(agent_client_protocol::schema::v1::ToolKind::Other)
                    .status(agent_client_protocol::schema::v1::ToolCallStatus::Completed),
            ),
        );

        assert!(matches!(s.phase, AcpPhase::Idle));
        assert!(s.completed_unread);
    }

    #[test]
    fn ordinary_tool_during_active_turn_keeps_turn_running() {
        let mut s = fresh_state();
        note_prompt_sent(&mut s, "read it".into(), Vec::new());
        apply_event(
            &mut s,
            AcpEvent::ToolStarted {
                id: "read".into(),
                title: "read_file".into(),
                kind: ToolKind::Read,
            },
        );

        apply_event(
            &mut s,
            AcpEvent::ToolFinished {
                id: "read".into(),
                status: ToolCallStatus::Completed,
                output: Vec::new(),
            },
        );

        assert!(matches!(s.phase, AcpPhase::Running));
        assert!(!s.completed_unread);
    }

    #[test]
    fn snapshot_restore_repairs_running_without_an_active_turn() {
        let mut s = fresh_state();
        s.phase = AcpPhase::Running;
        s.turn_started_at_ms = None;
        s.completed_unread = true;

        let restored = AcpSessionState::from_snapshot(s.to_snapshot(false));

        assert!(matches!(restored.phase, AcpPhase::Idle));
        assert!(restored.completed_unread);
    }

    #[test]
    fn selecting_one_permission_keeps_other_requests_visible() {
        let mut s = fresh_state();
        s.permissions = vec![
            LivePermission {
                question: "first".into(),
                tool_call_id: "tool-1".into(),
                options: vec![PermissionOptionView {
                    option_id: "allow-first".into(),
                    name: "Allow".into(),
                    kind: PermissionOptionKindView::AllowOnce,
                }],
                details: ApprovalDetailsView::Generic,
                responder: None,
                raw_request_line: None,
            },
            LivePermission {
                question: "second".into(),
                tool_call_id: "tool-2".into(),
                options: vec![PermissionOptionView {
                    option_id: "allow-second".into(),
                    name: "Allow".into(),
                    kind: PermissionOptionKindView::AllowOnce,
                }],
                details: ApprovalDetailsView::Generic,
                responder: None,
                raw_request_line: None,
            },
        ];
        s.phase = AcpPhase::AwaitingApproval;

        select_permission(&mut s, "tool-1", "allow-first");

        assert_eq!(s.permissions.len(), 1);
        assert_eq!(s.permissions[0].tool_call_id, "tool-2");
        assert!(matches!(s.phase, AcpPhase::AwaitingApproval));
    }

    #[test]
    fn permission_selection_needs_the_matching_tool_call() {
        let mut s = fresh_state();
        for tool_call_id in ["tool-1", "tool-2"] {
            s.permissions.push(LivePermission {
                question: tool_call_id.into(),
                tool_call_id: tool_call_id.into(),
                options: vec![PermissionOptionView {
                    option_id: "allow".into(),
                    name: "Allow".into(),
                    kind: PermissionOptionKindView::AllowOnce,
                }],
                details: ApprovalDetailsView::Generic,
                responder: None,
                raw_request_line: None,
            });
        }

        select_permission(&mut s, "tool-2", "allow");

        assert_eq!(s.permissions.len(), 1);
        assert_eq!(s.permissions[0].tool_call_id, "tool-1");
    }

    #[test]
    fn streaming_updates_keep_awaiting_approval_while_a_card_is_pending() {
        // 回归：agent 在发出审批请求后继续推流（并行工具、思考片段、plan 刷新
        // 都会），相位一旦掉回 Running，smeltd 的四色相位就变成
        // Thinking/ExecutingTool，移动端会把这条会话的行动项判成已解决，
        // 提醒闪一下就没了——但请求其实还挂着。
        let mut s = fresh_state();
        note_prompt_sent(&mut s, "hi".into(), Vec::new());
        s.permissions.push(LivePermission {
            question: "允许写文件？".into(),
            tool_call_id: "tool-1".into(),
            options: vec![PermissionOptionView {
                option_id: "allow".into(),
                name: "Allow".into(),
                kind: PermissionOptionKindView::AllowOnce,
            }],
            details: ApprovalDetailsView::Generic,
            responder: None,
            raw_request_line: None,
        });
        s.phase = AcpPhase::AwaitingApproval;

        apply_event(
            &mut s,
            AcpEvent::AgentChunk {
                thought: false,
                text: "还在想".into(),
            },
        );
        assert!(matches!(s.phase, AcpPhase::AwaitingApproval));

        apply_event(
            &mut s,
            AcpEvent::ToolStarted {
                id: "tool-2".into(),
                title: "并行工具".into(),
                kind: crate::acp_chat::ToolKind::Other,
            },
        );
        assert!(matches!(s.phase, AcpPhase::AwaitingApproval));

        select_permission(&mut s, "tool-1", "allow");
        assert!(matches!(s.phase, AcpPhase::Running));
    }

    #[test]
    fn fatal_ends_session_and_keeps_reason() {
        let mut s = fresh_state();
        s.acp_session_id = Some("old-sid".into());
        s.entries.push(AcpEntry::User("old".into()));
        let message = "恢复失败，可重试：temporary failure";

        apply_event(&mut s, AcpEvent::Fatal(message.into()));

        assert_eq!(s.acp_session_id.as_deref(), Some("old-sid"));
        assert_eq!(s.entries.len(), 1);
        assert!(matches!(&s.entries[0], AcpEntry::User(text) if text == "old"));
        assert!(matches!(&s.phase, AcpPhase::Ended(reason) if reason == message));
    }

    #[test]
    fn should_persist_excludes_streaming_and_ephemeral_events() {
        let mut s = fresh_state();
        let o = apply_event(
            &mut s,
            AcpEvent::AgentChunk {
                thought: false,
                text: "x".into(),
            },
        );
        assert!(!o.should_persist);
        let o = apply_event(&mut s, AcpEvent::TurnEnded(StopReason::EndTurn));
        assert!(o.should_persist);
    }

    #[test]
    fn parallel_tool_completion_reports_the_matching_entry_offset() {
        let mut s = fresh_state();
        for id in ["tool-a", "tool-b"] {
            apply_event(
                &mut s,
                AcpEvent::ToolStarted {
                    id: id.into(),
                    title: id.into(),
                    kind: ToolKind::Execute,
                },
            );
        }

        let outcome = apply_event(
            &mut s,
            AcpEvent::ToolFinished {
                id: "tool-a".into(),
                status: ToolCallStatus::Completed,
                output: vec![ToolOutputPart::Text("done".into())],
            },
        );

        assert_eq!(outcome.entries_offset, Some(0));
        assert!(matches!(
            &s.entries[0],
            AcpEntry::ToolCall {
                status: ToolCallStatus::Completed,
                ..
            }
        ));
        assert!(matches!(
            &s.entries[1],
            AcpEntry::ToolCall {
                status: ToolCallStatus::InProgress,
                ..
            }
        ));
    }

    #[test]
    fn incremental_snapshot_contains_only_requested_tail() {
        let mut state = fresh_state();
        state.entries = vec![
            AcpEntry::User("one".into()),
            AcpEntry::User("two".into()),
            AcpEntry::User("three".into()),
        ];
        let snapshot = state.to_snapshot_since(false, 2);
        assert_eq!(snapshot.entries_offset, 2);
        assert_eq!(snapshot.entries.len(), 1);
        assert!(matches!(&snapshot.entries[0], AcpEntry::User(text) if text == "three"));
    }

    #[test]
    fn snapshot_replay_flag_is_preserved() {
        let mut state = fresh_state();
        state.replaying_history = true;
        let snapshot = state.to_snapshot(false);
        assert!(snapshot.replaying_history);
    }

    #[test]
    fn legacy_elicitation_field_without_required_stays_required() {
        let legacy: ElicitFieldView = serde_json::from_value(serde_json::json!({
            "key": "scope",
            "title": "Scope",
            "kind": { "Text": { "secret": false } }
        }))
        .expect("legacy elicitation field should deserialize");
        assert!(legacy.required);

        let optional: ElicitFieldView = serde_json::from_value(serde_json::json!({
            "key": "notes",
            "title": "Notes",
            "required": false,
            "kind": { "Text": { "secret": false } }
        }))
        .expect("new optional elicitation field should deserialize");
        assert!(!optional.required);
    }

    #[test]
    fn choose_elicitation_single_select_signals_auto_submit() {
        use agent_client_protocol::schema::v1::ElicitationContentValue as V;
        let mut s = fresh_state();
        s.elicitation = Some(LiveElicitation {
            message: "pick one".into(),
            raw_fields: vec![ElicitField {
                key: "k".into(),
                title: "t".into(),
                required: true,
                kind: ElicitFieldKind::Select(vec![
                    crate::acp_conn::ElicitOption {
                        value: V::String("a".into()),
                        label: "A".into(),
                    },
                    crate::acp_conn::ElicitOption {
                        value: V::String("b".into()),
                        label: "B".into(),
                    },
                ]),
            }],
            chosen: Default::default(),
            text_values: Default::default(),
            responder: None,
            recovered_tool_call_id: None,
            raw_request_line: None,
        });
        let auto_submit = choose_elicitation(&mut s, 0, 1);
        assert!(auto_submit);
        assert_eq!(
            s.elicitation.as_ref().unwrap().chosen.get(&0),
            Some(&vec![1])
        );
    }

    #[test]
    fn choose_elicitation_multi_select_toggles() {
        use agent_client_protocol::schema::v1::ElicitationContentValue as V;
        let mut s = fresh_state();
        s.elicitation = Some(LiveElicitation {
            message: "pick many".into(),
            raw_fields: vec![ElicitField {
                key: "k".into(),
                title: "t".into(),
                required: true,
                kind: ElicitFieldKind::MultiSelect(vec![
                    crate::acp_conn::ElicitOption {
                        value: V::String("a".into()),
                        label: "A".into(),
                    },
                    crate::acp_conn::ElicitOption {
                        value: V::String("b".into()),
                        label: "B".into(),
                    },
                ]),
            }],
            chosen: Default::default(),
            text_values: Default::default(),
            responder: None,
            recovered_tool_call_id: None,
            raw_request_line: None,
        });
        let auto_submit = choose_elicitation(&mut s, 0, 0);
        assert!(!auto_submit); // multi-select 从不自动提交
        assert_eq!(
            s.elicitation.as_ref().unwrap().chosen.get(&0),
            Some(&vec![0])
        );
        choose_elicitation(&mut s, 0, 0); // 再点一次 = 取消
        assert_eq!(
            s.elicitation.as_ref().unwrap().chosen.get(&0),
            Some(&vec![])
        );
    }

    #[test]
    fn unfinished_ask_user_question_rebuilds_a_prompt_backed_elicitation() {
        let raw_input = serde_json::json!({
            "questions": [{
                "question": "提醒走哪个渠道？",
                "multiSelect": false,
                "options": [
                    {"label": "Gitea Issue", "description": "仓库待办"},
                    {"label": "Bark", "description": "手机推送"}
                ]
            }]
        });

        let card = recovered_elicitation("提醒走哪个渠道？", Some(&raw_input), "ask-1")
            .expect("replayed AskUserQuestion should remain actionable");
        assert!(card.responder.is_none());
        assert_eq!(card.recovered_tool_call_id.as_deref(), Some("ask-1"));
        assert_eq!(card.raw_fields.len(), 1);
        let ElicitFieldKind::Select(options) = &card.raw_fields[0].kind else {
            panic!("single-select question should remain a select");
        };
        assert_eq!(options[1].label, "Bark");

        let mut state = fresh_state();
        state.elicitation = Some(card);
        assert!(choose_elicitation(&mut state, 0, 1));
        assert_eq!(
            recovered_elicitation_answer(&state).as_deref(),
            Some("Bark")
        );

        dismiss_elicitation(&mut state);
        assert!(matches!(state.phase, AcpPhase::Idle));
    }

    #[test]
    fn completed_replayed_question_does_not_rebuild_an_elicitation() {
        let raw_input = serde_json::json!({
            "questions": [{
                "question": "Already answered?",
                "options": [{"label": "Yes"}, {"label": "No"}]
            }]
        });
        let mut state = fresh_state();
        apply_event(&mut state, AcpEvent::HistoryReplayStarted);

        apply_event(
            &mut state,
            AcpEvent::ToolCall(
                agent_client_protocol::schema::v1::ToolCall::new("ask-1", "Already answered?")
                    .status(agent_client_protocol::schema::v1::ToolCallStatus::Completed)
                    .raw_input(raw_input),
            ),
        );

        assert!(state.elicitation.is_none());
        assert!(matches!(state.phase, AcpPhase::Starting));
    }

    #[test]
    fn terminal_tool_update_clears_recovered_elicitation() {
        let raw_input = serde_json::json!({
            "questions": [{
                "question": "Already answered?",
                "options": [{"label": "Yes"}, {"label": "No"}]
            }]
        });
        let mut state = fresh_state();
        apply_event(&mut state, AcpEvent::HistoryReplayStarted);
        apply_event(
            &mut state,
            AcpEvent::ToolCall(
                agent_client_protocol::schema::v1::ToolCall::new("ask-1", "Already answered?")
                    .status(agent_client_protocol::schema::v1::ToolCallStatus::Pending)
                    .raw_input(raw_input),
            ),
        );
        assert!(matches!(state.phase, AcpPhase::AwaitingChoice));

        apply_event(
            &mut state,
            AcpEvent::ToolCallUpdate(agent_client_protocol::schema::v1::ToolCallUpdate::new(
                "ask-1",
                agent_client_protocol::schema::v1::ToolCallUpdateFields::new()
                    .status(agent_client_protocol::schema::v1::ToolCallStatus::Completed),
            )),
        );

        assert!(state.elicitation.is_none());
        assert!(matches!(state.phase, AcpPhase::Idle));
        assert!(matches!(
            state.entries.last(),
            Some(AcpEntry::ToolCall {
                status: ToolCallStatus::Completed,
                ..
            })
        ));
    }

    #[test]
    fn replayed_user_answer_clears_question_without_a_terminal_tool_update() {
        let raw_input = serde_json::json!({
            "questions": [{
                "question": "Fix it now?",
                "options": [{"label": "Fix"}, {"label": "Later"}]
            }]
        });
        let mut state = fresh_state();
        apply_event(&mut state, AcpEvent::HistoryReplayStarted);
        apply_event(
            &mut state,
            AcpEvent::ToolCall(
                agent_client_protocol::schema::v1::ToolCall::new("ask-1", "Fix it now?")
                    .status(agent_client_protocol::schema::v1::ToolCallStatus::Pending)
                    .raw_input(raw_input),
            ),
        );
        assert!(matches!(state.phase, AcpPhase::AwaitingChoice));

        apply_event(&mut state, AcpEvent::UserChunk("Fix".into()));

        assert!(state.elicitation.is_none());
        assert!(matches!(state.phase, AcpPhase::Idle));
        assert!(matches!(
            state.entries.last(),
            Some(AcpEntry::User(answer)) if answer == "Fix"
        ));
    }

    #[test]
    fn ready_does_not_overwrite_a_recovered_choice_phase() {
        let raw_input = serde_json::json!({
            "questions": [{
                "question": "Choose",
                "options": [{"label": "A"}]
            }]
        });
        let mut state = fresh_state();
        state.elicitation = recovered_elicitation("Choose", Some(&raw_input), "ask-1");

        apply_event(
            &mut state,
            AcpEvent::Ready {
                session_id: agent_client_protocol::schema::v1::SessionId::new("session"),
                kind: ReadyKind::ResumedKeepHistory,
                supports_image: true,
            },
        );

        assert!(matches!(state.phase, AcpPhase::AwaitingChoice));
        assert!(state.elicitation.is_some());
    }

    #[test]
    fn should_auto_resume_requires_ended_and_known_session_id() {
        let mut s = fresh_state();
        s.phase = AcpPhase::Ended("gone".into());
        assert!(!should_auto_resume(&s)); // 没有旧 session id
        s.history_session_id = Some("sid-1".into());
        assert!(should_auto_resume(&s));
        s.phase = AcpPhase::Idle;
        assert!(!should_auto_resume(&s)); // 还活着，用不上「自动续接」
    }

    #[test]
    fn restart_clears_old_runtime_id_but_keeps_history_identity() {
        let mut s = fresh_state();
        s.acp_session_id = Some("runtime".into());
        s.history_session_id = Some("history".into());

        reset_for_restart(&mut s);

        assert!(s.acp_session_id.is_none());
        assert_eq!(s.history_session_id.as_deref(), Some("history"));
    }
}
