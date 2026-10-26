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

pub use smelt_model::acp_session::*;

use std::collections::{BTreeMap, BTreeSet};

use agent_client_protocol::schema::v1::{ElicitationContentValue, Plan};

use crate::acp_chat::AcpEntry;
use crate::acp_conn::{
    ElicitField, ElicitFieldKind, ElicitOption, ElicitationResponder, ModelState,
    PermissionResponder, SessionConfigState,
};
use crate::daemon_state::DaemonPhase;

mod apply;
pub use apply::{
    ApplyOutcome, acknowledge_composer_restore, apply_event, finalize_dangling_tool_calls,
    interrupt_open_turn, queue_composer_restore, queued_item_images, take_claimed_queue_text,
};
use apply::{pending_action_phase, resume_running};

// ===================== wire 快照类型（无 agent_client_protocol 依赖） =====================

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

// ===================== 服务端活体状态 =====================

/// 待审批卡片：`responder` 只在收到 `AcpUserAction::PermissionSelect` 时消费。
pub struct LivePermission {
    pub question: String,
    pub tool_call_id: String,
    pub options: Vec<PermissionOptionView>,
    pub details: ApprovalDetailsView,
    pub responder: Option<PermissionResponder>,
    /// 这张卡对应请求的原始 JSON-RPC 行（见 `ConversationEvent::Permission` 同名字段）。
    pub raw_request_line: Option<String>,
}

/// 选择题卡片：字段原始形态保留在 `raw_fields`（翻译回
/// `ElicitationContentValue` 要用），`chosen` 是当前选择态。
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

fn last_user_index(entries: &[AcpEntry]) -> Option<usize> {
    entries
        .iter()
        .rposition(|entry| matches!(entry, AcpEntry::User(_) | AcpEntry::UserWithImages { .. }))
}

/// 一次宿主快照合并的结果。调试正文只活到父进程把这一帧写给已经打开的窗口。
pub struct HostedSnapshotMerge {
    pub changed_from: usize,
    pub runtime_debug: Option<RuntimeDebug>,
}

/// smeltd 侧一份 ACP 会话的完整活体状态。协议事件走 `apply_event`，用户动作走
/// `apply_user_action` / `note_*`；连接级结束（所有权冲突、provider 没退出、
/// 传输断开）走 [`force_end`]。smeltd 不得再直接写 `phase`。
///
/// `phase` 是当前活动，直接存 [`DaemonPhase`]。归约器只写 Connecting / Idle /
/// Thinking / AwaitingApproval / WaitingForUser / Dead；Succeeded、Failed、
/// ExecutingTool 是守护广播层的投影。
pub struct AcpSessionState {
    /// 归约器的工作缓冲。重放期间它是从空缓冲重建的草稿，不是会话正文，
    /// 所以不对外公开：外部一律读 [`Self::entries`]（已发布正文）。
    /// 父进程镜像在合并宿主快照后会丢掉已提交前缀，只留当前回合；
    /// 这时 `entries[0]` 的绝对下标是 [`Self::entries_base`]。
    /// 会话宿主的归约从 0 开始。死亡宿主的重启种子可能只是这一截后缀，
    /// 重放提交后 [`Self::commit_replay`] 把下标清回 0。
    entries: Vec<AcpEntry>,
    /// `entries` 之前还有这么多条已提交正文不在这个缓冲里。
    entries_base: usize,
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
    /// 这份历史已经确认读不出来。和 `history_session_id` 同属会话身份。
    /// 有 id 时以 id 为准；没有 id 且为真，才清空库里的恢复身份。
    pub history_abandoned: bool,
    pub supports_image: bool,
    /// 「等自己刚发那条 prompt 的回声」，见旧版字段同名注释——语义原样保留。
    pub awaiting_user_echo: bool,
    /// `session/load` 正在重建历史投影。回放通知和实时通知共用同一套 ACP
    /// update，必须在本地记住这段边界，避免历史 assistant/tool 消息把空闲
    /// 会话误标成 Running。收到 `HistoryReplayFinished`（或失败重置、中途发新 prompt）时收尾结束。
    pub replaying_history: bool,
    /// 重放期间 `entries` 是从空缓冲重建的草稿。已经交给窗口的正文留在这里。
    /// 快照、活帧和历史分页都读它；草稿重建完再一次性换上，中间那帧空缓冲
    /// 不能被当成「会话现在没有消息」。
    replay_baseline: Option<Vec<AcpEntry>>,
    pub available_commands: Vec<(String, String)>,
    pub usage: Option<(u64, u64)>,
    pub usage_cached_read: Option<u64>,
    pub usage_cost: Option<f64>,
    pub usage_breakdown: Option<crate::acp_conn::ContextUsageBreakdown>,
    pub plan: Option<PlanView>,
    pub background_tasks: Vec<BackgroundTaskView>,
    /// Pi 子智能体。会话宿主每次变化推全量。
    pub subagents: Vec<SubagentView>,
    /// 别的 Pi 对话发来、宿主还记得的消息。会话宿主每次变化推全量。
    pub peer_inbox: Vec<crate::acp_session::PeerMessageView>,
    pub model: Option<ModelState>,
    pub config_options: Vec<SessionConfigState>,
    pub supports_compaction: bool,
    pub supports_native_queue: bool,
    pub supports_rewind: bool,
    pub compacting: bool,
    /// Provider 正在自动重试模型请求。
    pub provider_retry: Option<ProviderRetryView>,
    /// 给用户看的一次性提示。启动进度不在这里，在 `status_line`。
    pub notice: Option<AcpNoticeView>,
    /// smeltd 回合闸门后面那条队列的展示投影。队列本体（附图、投递 id）在 smeltd，
    /// 它每次改队列都整份重写这里；归约器不碰。
    pub pending_prompts: Vec<PendingPromptView>,
    pub queued_steering: Vec<String>,
    /// 与 `queued_steering` 对齐的附图。快照只发文本，活体状态用来在真正插入时还原。
    pub queued_steering_images: Vec<Vec<crate::acp_chat::AcpImage>>,
    pub queued_follow_up: Vec<String>,
    pub composer_restore_revision: u64,
    pub composer_restore_texts: Vec<String>,
    pub composer_restore_images: Vec<crate::acp_chat::AcpImage>,
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
            entries_base: 0,
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
            history_abandoned: false,
            supports_image: true,
            awaiting_user_echo: false,
            replaying_history: false,
            replay_baseline: None,
            available_commands: Vec::new(),
            usage: None,
            usage_cached_read: None,
            usage_cost: None,
            usage_breakdown: None,
            plan: None,
            background_tasks: Vec::new(),
            subagents: Vec::new(),
            peer_inbox: Vec::new(),
            model: None,
            config_options: Vec::new(),
            supports_compaction: false,
            supports_native_queue: false,
            supports_rewind: false,
            compacting: false,
            provider_retry: None,
            notice: None,
            pending_prompts: Vec::new(),
            queued_steering: Vec::new(),
            queued_steering_images: Vec::new(),
            queued_follow_up: Vec::new(),
            composer_restore_revision: 0,
            composer_restore_texts: Vec::new(),
            composer_restore_images: Vec::new(),
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
            // `entries` 是从 `entries_offset` 开始的一截。忽略这个下标会把后缀当成全文。
            entries: snap.entries,
            entries_base: snap.entries_offset,
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
            history_abandoned: snap.history_abandoned,
            supports_image: snap.supports_image,
            awaiting_user_echo: false,
            replaying_history: false,
            replay_baseline: None,
            available_commands: snap.available_commands,
            usage: snap.usage,
            usage_cached_read: snap.usage_cached_read,
            usage_cost: snap.usage_cost,
            usage_breakdown: snap.usage_breakdown,
            plan: snap.plan,
            background_tasks: snap.background_tasks,
            subagents: snap.subagents,
            peer_inbox: snap.peer_inbox,
            model: snap.model,
            config_options: snap.config_options,
            supports_compaction: snap.supports_compaction,
            supports_native_queue: snap.supports_native_queue,
            supports_rewind: snap.supports_rewind,
            compacting: snap.compacting,
            provider_retry: snap.provider_retry,
            notice: snap.notice,
            // 队列本体不随进程交接（交接会等队列先清空），新进程从空队列开始。
            pending_prompts: Vec::new(),
            queued_steering: snap.queued_steering,
            queued_steering_images: Vec::new(),
            queued_follow_up: snap.queued_follow_up,
            composer_restore_revision: snap.composer_restore_revision,
            composer_restore_texts: snap.composer_restore_texts,
            composer_restore_images: snap.composer_restore_images,
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
    /// session host 才持有真实 JSON-RPC responder、回合队列和已提交正文。daemon
    /// 这份状态只服务于 list/watch/任务对账，所以只留下当前回合（空闲时留下
    /// 刚结束的那一轮，供自动化取输出）。更早的正文向宿主分页要。
    /// 分页失败也不把整段放回这里：权威正文在宿主和会话文件里。
    ///
    /// 待审批卡片在这里恢复为 `responder=None`，用户动作必须继续转发给 host。
    /// 返回最早变化的绝对 entry 下标，以及本帧完整的调试体（镜像里只留账本）。
    /// 增量尾巴接不上本地末端时，调用方应请求 host 重发完整快照。
    pub fn merge_hosted_snapshot(
        &mut self,
        snap: ConversationSnapshot,
    ) -> Result<HostedSnapshotMerge, String> {
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
            history_abandoned,
            supports_image,
            available_commands,
            usage,
            usage_cached_read,
            usage_cost,
            usage_breakdown,
            plan,
            background_tasks,
            subagents,
            peer_inbox,
            model,
            config_options,
            conversation_state: _,
            supports_compaction,
            supports_native_queue,
            supports_rewind,
            compacting,
            provider_retry,
            notice,
            pending_prompts,
            queued_steering,
            queued_follow_up,
            composer_restore_revision,
            composer_restore_texts,
            composer_restore_images,
            turn_started_at_ms,
            turn_timings,
            completed_unread,
            turn_outcome,
            should_persist: _,
            history_error: _,
            history_omitted: _,
            envelope_omitted,
        } = snap;

        let local_end = self.entries_base + self.entries.len();
        if entries_offset > local_end {
            return Err(format!(
                "host 快照缺少历史前缀：offset={entries_offset}, local={local_end}"
            ));
        }
        let merged_len = entries_offset.saturating_add(entries.len());
        if merged_len != entries_total {
            return Err(format!(
                "host 快照不是连续尾片：offset={entries_offset}, tail={}, total={entries_total}",
                entries.len()
            ));
        }

        let changed_from = entries_offset;
        if entries_offset < self.entries_base {
            // 尾巴盖过了已经丢掉的前缀。先整段接上，下面再按回合裁。
            self.entries_base = entries_offset;
            self.entries = entries;
        } else {
            let local_index = entries_offset - self.entries_base;
            self.entries.truncate(local_index);
            self.entries.extend(entries);
        }
        if let Some(tool_debug) = tool_debug {
            self.tool_debug = tool_debug;
            self.note_tool_debug_changed();
        }
        let runtime_debug = if let Some(runtime_debug) = runtime_debug {
            self.runtime_debug = runtime_debug.ledger();
            Some(runtime_debug)
        } else {
            None
        };
        // 没带标题不是清空。流式尾巴经常省略标题，盖掉已有协议标题后，
        // 父镜像只会用当前这一截的提问当兜底，侧栏就闪成另一串字再跳回来。
        if let Some(title) = crate::session_title::provider_session_title(session_title.as_deref())
        {
            self.protocol_title = Some(title.to_string());
        }
        self.phase = phase;
        self.end_reason = end_reason;
        self.end_kind = end_kind;
        if !envelope_omitted {
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
            self.available_commands = available_commands;
            self.usage = usage;
            self.usage_cached_read = usage_cached_read;
            self.usage_cost = usage_cost;
            self.usage_breakdown = usage_breakdown;
            self.plan = plan;
            self.background_tasks = background_tasks;
            self.subagents = subagents;
            self.peer_inbox = peer_inbox;
            self.model = model;
            self.config_options = config_options;
            self.supports_compaction = supports_compaction;
            self.supports_native_queue = supports_native_queue;
            self.supports_rewind = supports_rewind;
            self.compacting = compacting;
            self.provider_retry = provider_retry;
            self.notice = notice;
            self.pending_prompts = pending_prompts;
            self.queued_steering = queued_steering;
            self.queued_steering_images.clear();
            self.queued_follow_up = queued_follow_up;
            self.composer_restore_revision = composer_restore_revision;
            self.composer_restore_texts = composer_restore_texts;
            self.composer_restore_images = composer_restore_images;
            self.turn_started_at_ms = turn_started_at_ms;
            self.turn_timings = turn_timings;
        }
        self.acp_session_id = acp_session_id;
        self.history_session_id = history_session_id.filter(|id| !id.trim().is_empty());
        self.history_abandoned = history_abandoned;
        self.supports_image = supports_image;
        self.awaiting_user_echo = false;
        self.replaying_history = replaying_history;
        self.completed_unread = completed_unread;
        self.turn_outcome = turn_outcome;
        self.retain_mirror_suffix();
        Ok(HostedSnapshotMerge {
            changed_from,
            runtime_debug,
        })
    }

    /// 父镜像不保留已提交前缀。进行中的回合从最后一条用户消息留到末尾；
    /// 空闲时同样留下刚结束的那一轮，自动化要拿它的输出，恢复判断也才知道这不是空会话。
    fn retain_mirror_suffix(&mut self) {
        let active = self.turn_started_at_ms.is_some()
            || matches!(
                self.phase,
                DaemonPhase::Thinking
                    | DaemonPhase::ExecutingTool
                    | DaemonPhase::AwaitingApproval
                    | DaemonPhase::WaitingForUser
            )
            || crate::acp_chat::has_unfinished_tool_call(&self.entries);
        let keep_from = if active {
            last_user_index(&self.entries).unwrap_or(0)
        } else {
            last_user_index(&self.entries).unwrap_or(self.entries.len())
        };
        if keep_from == 0 {
            return;
        }
        self.entries.drain(0..keep_from);
        self.entries.shrink_to_fit();
        self.entries_base += keep_from;
    }

    /// 当前有没有一张卡（权限/选择题）正等着人处理，有就带上它原始请求那行。
    /// 同一时刻协议上只会有一张卡挂起（agent 等到上一个请求有回应才会发下一个）。
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
    /// 标题已经在协议里时不再扫描正文。
    pub fn resolved_session_title(&self) -> Option<String> {
        if let Some(title) =
            crate::session_title::provider_session_title(self.protocol_title.as_deref())
        {
            return Some(title.to_string());
        }
        let fallback = crate::acp_chat::auto_title(self.entries());
        crate::session_title::session_name(None, fallback.as_deref())
    }

    /// 回合成功结束了，「完成」还压着没报。后台任务、后台子智能体还在跑时压着，
    /// 等它们结束再报；正常结束的会叫醒 Provider 再跑一轮，完成交给那一轮报。
    /// 只用快照里有的字段推出来，宿主镜像和热升级之后都一样。
    pub fn completion_withheld(&self) -> bool {
        matches!(self.phase, DaemonPhase::Idle)
            && self.turn_started_at_ms.is_none()
            && !self.completed_unread
            && self.turn_outcome.is_some_and(AcpTurnOutcome::is_success)
    }

    /// 后台任务、后台子智能体还有在跑的。
    pub fn background_work_running(&self) -> bool {
        self.background_tasks
            .iter()
            .any(|task| task.status == BackgroundTaskStatus::Running)
            || self
                .subagents
                .iter()
                .any(|subagent| subagent.background && subagent.status.is_running())
    }

    /// 回合结束了，但后台还在干活：会话对外算在执行，不算完成。
    pub fn completion_waits_on_background(&self) -> bool {
        self.completion_withheld() && self.background_work_running()
    }

    /// 会话正文：窗口、分页、活帧、回退校验与持久化看到的都是这一份。
    /// 重放草稿还没换上之前，这里是重放前已发布的那一份。
    /// 父镜像裁过前缀时，这只是绝对历史末尾的那一截。
    pub fn entries(&self) -> &[AcpEntry] {
        self.replay_baseline.as_deref().unwrap_or(&self.entries)
    }

    /// 父镜像丢掉的已提交前缀长度。宿主和同进程会话是 0。
    pub fn entries_base(&self) -> usize {
        self.entries_base
    }

    /// 重启宿主前把整段正文放回父镜像，作为新宿主的种子。下一帧合并会再裁掉。
    pub fn install_resident_entries(&mut self, entries: Vec<AcpEntry>) {
        self.entries = entries;
        self.entries_base = 0;
    }

    /// 绝对历史长度。父镜像即使只留着当前回合，这个数仍是宿主那边的总长度。
    pub fn entries_total(&self) -> usize {
        self.entries_base + self.entries().len()
    }

    /// 测试搭场景用：直接改工作缓冲，绕过归约器。正式代码不得启用。
    #[cfg(any(test, feature = "test-support"))]
    pub fn entries_mut(&mut self) -> &mut Vec<AcpEntry> {
        &mut self.entries
    }

    /// 重放草稿是从 0 开始的完整投影。提交时丢掉上一份正文，并把后缀下标清掉。
    /// 没在重放时调用只清标志。后缀种子的绝对长度要留到真正的重放提交。
    pub fn commit_replay(&mut self) {
        let was_replaying = self.replaying_history || self.replay_baseline.is_some();
        let committing_scratch = self.replay_baseline.take().is_some();
        if committing_scratch {
            self.entries_base = 0;
        }
        self.replaying_history = false;
        // 从历史里恢复出来的问题卡片在等用户回答，重启后照样能答，这一轮没有中断。
        let awaiting_recovered_answer = self
            .elicitation
            .as_ref()
            .is_some_and(|card| card.recovered_tool_call_id.is_some());
        // 重放结束时，文件里没写完的那一轮不会再收到 TurnEnded。留着会让
        // 重连后的会话一直转圈。用户取消仍记成 Cancelled。
        if was_replaying && !awaiting_recovered_answer {
            let _ = interrupt_open_turn(self);
        }
    }

    /// 重放没有形成完整投影。草稿不能当会话，回到开始重放前已发布的正文。
    pub fn abandon_replay_scratch(&mut self) {
        if let Some(baseline) = self.replay_baseline.take() {
            self.entries = baseline;
        }
        self.replaying_history = false;
    }

    /// `should_persist` 不是从 `self` 能算出来的——它是"这次变化是怎么发生的"
    /// 这个上下文信息，调用方（smeltd 的事件循环）从 `apply_event` 的返回值
    /// 里拿，这里只负责原样塞进快照，见该字段注释。
    /// 已落进会话文件的历史终点，以及还没写完的这一轮。
    /// 空闲且没有进行中的工具时，这一轮是空的。
    pub fn open_turn_range(&self) -> (usize, usize) {
        let entries = self.entries();
        let total = self.entries_base + entries.len();
        let active = self.turn_started_at_ms.is_some()
            || matches!(
                self.phase,
                DaemonPhase::Thinking
                    | DaemonPhase::ExecutingTool
                    | DaemonPhase::AwaitingApproval
                    | DaemonPhase::WaitingForUser
            )
            || crate::acp_chat::has_unfinished_tool_call(entries);
        if !active {
            return (total, total);
        }
        let start = last_user_index(entries).unwrap_or(0);
        (self.entries_base + start, total)
    }

    pub fn to_snapshot(&self, should_persist: bool) -> ConversationSnapshot {
        self.to_snapshot_since(should_persist, 0)
    }

    /// 旁观或历史首帧。活状态始终来自当前镜像。
    /// `hosted_page` 只用来补 `[offset, entries_base)` 里已经丢掉的正文；
    /// 镜像里还留着的后缀以这里为准，分页等待期间的流式更新不会被旧页盖住。
    /// 页的总长度对不上、或没有盖到后缀起点时，退回镜像自己能证明的那一截。
    pub fn snapshot_with_older_page(
        &self,
        offset: usize,
        hosted_page: Option<&ConversationSnapshot>,
    ) -> ConversationSnapshot {
        let mut snapshot = self.to_snapshot_since(false, offset);
        let Some(page) = hosted_page else {
            return snapshot;
        };
        let base = self.entries_base;
        let total = self.entries_total();
        if offset >= base || page.entries_total != total || page.entries_offset > offset {
            return snapshot;
        }
        let start_in_page = offset - page.entries_offset;
        let base_in_page = base - page.entries_offset;
        if base_in_page > page.entries.len() || start_in_page > base_in_page {
            return snapshot;
        }
        let mut entries = page.entries[start_in_page..base_in_page].to_vec();
        entries.append(&mut snapshot.entries);
        snapshot.entries = entries;
        snapshot.entries_offset = offset;
        snapshot
    }

    pub fn to_snapshot_since(
        &self,
        should_persist: bool,
        entries_offset: usize,
    ) -> ConversationSnapshot {
        // 终点是绝对长度。父镜像裁过前缀后，本地 `entries.len()` 比这个数小。
        self.to_snapshot_range(should_persist, entries_offset, self.entries_total())
    }

    pub fn to_snapshot_range(
        &self,
        should_persist: bool,
        entries_offset: usize,
        entries_end: usize,
    ) -> ConversationSnapshot {
        self.to_snapshot_range_selective(
            should_persist,
            entries_offset,
            entries_end,
            true,
            true,
            true,
        )
    }

    /// 与 [`Self::to_snapshot_range`] 相同，但可以不克隆工具参数表、runtime debug
    /// 和会话信封。流式正文帧只带尾部条目。
    pub fn to_snapshot_range_selective(
        &self,
        should_persist: bool,
        entries_offset: usize,
        entries_end: usize,
        include_tool_debug: bool,
        include_runtime_debug: bool,
        include_envelope: bool,
    ) -> ConversationSnapshot {
        let published = self.entries();
        let base = self.entries_base;
        let entries_total = base + published.len();
        // 调用方传入的是绝对下标。终点落在已丢掉的前缀里时抬到后缀起点，避免下溢。
        let entries_end = entries_end.clamp(base.min(entries_total), entries_total);
        let requested = entries_offset.min(entries_end);
        // 要的起点落在已丢掉的前缀里时，只能从还留着的那一截开始，不能把下标说成 0。
        let entries_offset = requested.max(base).min(entries_end);
        let local_start = entries_offset - base;
        let local_end = entries_end - base;
        let entries = published[local_start..local_end].to_vec();
        ConversationSnapshot {
            entries_offset,
            entries_total,
            snapshot_revision: 0,
            session_title: include_envelope
                .then(|| self.resolved_session_title())
                .flatten(),
            replaying_history: self.replaying_history,
            entries,
            tool_debug: include_tool_debug.then(|| self.tool_debug.clone()),
            runtime_debug: include_runtime_debug.then(|| self.runtime_debug.clone()),
            phase: self.phase,
            end_reason: self.end_reason.clone(),
            end_kind: self.end_kind,
            accepted_delivery_ids: if include_envelope {
                self.accepted_delivery_ids.clone()
            } else {
                Default::default()
            },
            active_delivery_id: include_envelope
                .then(|| self.active_delivery_id.clone())
                .flatten(),
            completed_delivery_id: include_envelope
                .then(|| self.completed_delivery_id.clone())
                .flatten(),
            pending_permissions: if include_envelope {
                self.permissions
                    .iter()
                    .map(|p| PendingPermission {
                        question: p.question.clone(),
                        tool_call_id: p.tool_call_id.clone(),
                        options: p.options.clone(),
                        details: p.details.clone(),
                    })
                    .collect()
            } else {
                Vec::new()
            },
            pending_elicitation: if include_envelope {
                self.elicitation.as_ref().map(|e| PendingElicitation {
                    message: e.message.clone(),
                    fields: e.raw_fields.iter().map(elicit_field_view).collect(),
                    chosen: e.chosen.clone(),
                    text_values: e.text_values.clone(),
                })
            } else {
                None
            },
            status_line: include_envelope.then(|| self.status_line.clone()).flatten(),
            acp_session_id: self.acp_session_id.clone(),
            history_session_id: self.history_session_id.clone(),
            history_abandoned: self.history_abandoned,
            supports_image: self.supports_image,
            available_commands: if include_envelope {
                self.available_commands.clone()
            } else {
                Vec::new()
            },
            usage: self.usage,
            usage_cached_read: self.usage_cached_read,
            usage_cost: self.usage_cost,
            usage_breakdown: include_envelope
                .then(|| self.usage_breakdown.clone())
                .flatten(),
            plan: include_envelope.then(|| self.plan.clone()).flatten(),
            background_tasks: if include_envelope {
                self.background_tasks.clone()
            } else {
                Vec::new()
            },
            subagents: if include_envelope {
                self.subagents.clone()
            } else {
                Vec::new()
            },
            peer_inbox: if include_envelope {
                self.peer_inbox.clone()
            } else {
                Vec::new()
            },
            model: include_envelope.then(|| self.model.clone()).flatten(),
            config_options: if include_envelope {
                self.config_options.clone()
            } else {
                Vec::new()
            },
            conversation_state: None,
            supports_compaction: self.supports_compaction,
            supports_native_queue: self.supports_native_queue,
            supports_rewind: self.supports_rewind,
            compacting: self.compacting,
            provider_retry: self.provider_retry.clone(),
            notice: include_envelope.then(|| self.notice.clone()).flatten(),
            pending_prompts: if include_envelope {
                self.pending_prompts.clone()
            } else {
                Vec::new()
            },
            queued_steering: if include_envelope {
                self.queued_steering.clone()
            } else {
                Vec::new()
            },
            queued_follow_up: if include_envelope {
                self.queued_follow_up.clone()
            } else {
                Vec::new()
            },
            composer_restore_revision: if include_envelope {
                self.composer_restore_revision
            } else {
                0
            },
            composer_restore_texts: if include_envelope {
                self.composer_restore_texts.clone()
            } else {
                Vec::new()
            },
            composer_restore_images: if include_envelope {
                self.composer_restore_images.clone()
            } else {
                Vec::new()
            },
            turn_started_at_ms: include_envelope
                .then_some(self.turn_started_at_ms)
                .flatten(),
            turn_timings: if include_envelope {
                self.turn_timings.clone()
            } else {
                Vec::new()
            },
            completed_unread: self.completed_unread,
            turn_outcome: self.turn_outcome,
            should_persist,
            history_error: None,
            history_omitted: false,
            envelope_omitted: !include_envelope,
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
    state.abandon_replay_scratch();
    state.permissions.clear();
    state.runtime_debug = RuntimeDebug::default();
    state.elicitation = None;
    state.plan = None;
    state.model = None;
    state.usage = None;
    state.usage_breakdown = None;
    state.completed_unread = false;
    state.turn_outcome = None;
    state.provider_retry = None;
    state.notice = None;
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
        if state.history_session_id.is_some() {
            state.history_abandoned = false;
        }
    }
    // 草稿还是空的，说明重放还没交出新正文，新消息接在上一份已发布正文上。
    // 草稿里已经有重放出来的条目时，那份草稿就是新投影，不能退回旧正文。
    if state.entries.is_empty() {
        state.abandon_replay_scratch();
    } else {
        state.commit_replay();
    }
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
    state.provider_retry = None;
    state.notice = None;
    let started_at_ms = unix_time_ms();
    state.turn_started_at_ms = Some(started_at_ms);
    state.turn_timings.push(TurnTiming {
        user_index: state.entries.len().saturating_sub(1),
        started_at_ms,
        ended_at_ms: None,
    });
}

/// 撤回 [`note_prompt_sent_with_delivery`] 刚写下的那条用户消息。
///
/// 只用在这条 prompt 最终没能进模型、正文要还给输入框的场景（Pi 预检失败 /
/// 用户取消）——正文回了输入框，消息流里就不能再留着它，否则同一句话会在
/// 两处同时存在。
///
/// 只撤可逆的那部分：`history_session_id` 的升级和悬空工具收尾都是单调的，
/// 回滚它们只会把别的东西弄坏。最后一条不是刚写下的用户消息时一律不动
/// 状态返回 `None`：那说明已经有新正文长进来，此时删条目只会错位。
pub fn undo_prompt_sent(
    state: &mut AcpSessionState,
) -> Option<(String, Vec<crate::acp_chat::AcpImage>)> {
    let last_index = state.entries.len().checked_sub(1)?;
    if !matches!(
        state.entries.last(),
        Some(AcpEntry::User(_) | AcpEntry::UserWithImages { .. })
    ) {
        return None;
    }
    // 这一轮必须是「刚开头」：计时条目指向最后这条用户消息且还没收尾。
    match state.turn_timings.last() {
        Some(timing) if timing.user_index == last_index && timing.ended_at_ms.is_none() => {}
        _ => return None,
    }
    let taken = match state.entries.pop()? {
        AcpEntry::User(text) => (text, Vec::new()),
        AcpEntry::UserWithImages { text, images } => (text, images),
        // 上面已经匹配过类型，这里不可能走到。
        other => {
            state.entries.push(other);
            return None;
        }
    };
    state.turn_timings.pop();
    state.turn_seq = state.turn_seq.saturating_sub(1);
    state.awaiting_user_echo = false;
    state.turn_started_at_ms = None;
    state.active_delivery_id = None;
    state.phase = DaemonPhase::Idle;
    Some(taken)
}

/// 换上一条新的用户提示。id 取毫秒时间且严格大于上一条：进程重启后也不会撞上
/// 客户端已经关掉的那条。
pub fn raise_notice(state: &mut AcpSessionState, text: String) {
    let floor = state.notice.as_ref().map_or(0, |notice| notice.id + 1);
    state.notice = Some(AcpNoticeView {
        id: unix_time_ms().max(floor),
        text,
    });
}

const PROVIDER_STILL_RUNNING_STATUS: &str = "已排队，这一轮结束后再发";

/// Pi 还在跑，这条消息要等它结束。界面若已经空闲，把转圈拉回来，好接住后续输出。
/// 用户刚按过停止时不拉回来：停止就是要界面立刻停。
pub fn note_provider_still_running(state: &mut AcpSessionState) {
    state.status_line = Some(PROVIDER_STILL_RUNNING_STATUS.to_string());
    if state.cancel_requested
        || state
            .cancelled_turn_seq
            .is_some_and(|seq| seq == state.turn_seq)
    {
        return;
    }
    if matches!(state.phase, DaemonPhase::Idle) && state.turn_started_at_ms.is_none() {
        state.phase = DaemonPhase::Thinking;
        state.turn_started_at_ms = Some(unix_time_ms());
    }
}

pub fn clear_provider_queue_status(state: &mut AcpSessionState) {
    if state.status_line.as_deref() == Some(PROVIDER_STILL_RUNNING_STATUS) {
        state.status_line = None;
    }
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

/// 中途插入只对还在跑的回合有意义。
///
/// 取消会先把界面收成 Idle，Pi 的闸门却要等 provider 真正空闲才放开。
/// 这段窗口里再 steer，消息会回到「当前回合」队列；用户再点一次立即发送，
/// 下一条 prompt 会把队列里那份和自己一起送进对话。
pub fn is_cancel_requested(state: &AcpSessionState) -> bool {
    state.cancel_requested
}

pub fn accepts_mid_turn_steer(state: &AcpSessionState) -> bool {
    if state.cancel_requested {
        return false;
    }
    matches!(
        state.phase,
        DaemonPhase::Thinking
            | DaemonPhase::ExecutingTool
            | DaemonPhase::AwaitingApproval
            | DaemonPhase::WaitingForUser
    )
}

pub(crate) fn unix_time_ms() -> u64 {
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
/// 跟旧版 `submit_elicitation` 一致；字段没有选择就跳过（agent 那边按 schema
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

/// 活状态头。`history_len == None`，正文仍由快照 `entries` 携带。
/// 不克隆 entries、工具参数表或 runtime debug。
pub fn live_header_from_state(state: &AcpSessionState) -> SessionLive {
    SessionLive {
        phase: state.phase,
        end_reason: state.end_reason.clone(),
        end_kind: state.end_kind,
        acp_session_id: state.acp_session_id.clone(),
        history_session_id: state.history_session_id.clone(),
        session_title: state.resolved_session_title(),
        status_line: state.status_line.clone(),
        history_len: None,
        open_turn: Vec::new(),
    }
}

/// 窗口投影。`history_len` 标出已提交历史的终点；当前回合的正文不在这里复制。
pub fn live_from_state(state: &AcpSessionState) -> SessionLive {
    let (history_len, _) = state.open_turn_range();
    let mut live = live_header_from_state(state);
    live.history_len = Some(history_len);
    live
}

#[cfg(test)]
mod tests;
