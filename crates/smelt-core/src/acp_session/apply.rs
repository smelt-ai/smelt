//! ACP 事件归约：`ConversationEvent` → 会话投影。
//!
//! `apply_event` 只做分发；有实质逻辑的臂是 `apply_*`。类型、快照和用户动作
//! 仍在父模块，这里不碰 GPUI、也不持有跨进程句柄。

use std::collections::{BTreeMap, BTreeSet};

use agent_client_protocol::schema::v1::{Plan, SessionId, StopReason, ToolCall, ToolCallUpdate};

use super::{AcpEndKind, AcpSessionState, AcpTurnOutcome, LiveElicitation, LivePermission};
use crate::acp_chat::AcpEntry;
use crate::acp_conn::{
    ConversationEvent, ConversationRestoreFailure, ElicitField, ElicitationResponder, PromptImage,
    ReadyKind,
};
use crate::daemon_state::DaemonPhase;

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
    /// 本次事件替换了大体积 runtime debug sidecar。实时广播只在该位为真时携带
    /// sidecar，避免完整 provider payload 随每个流式 token 重复传输。
    pub runtime_debug_changed: bool,
    /// 相位刚变成需要人处理 → (标题, 正文, is_approval)，调用方决定要不要弹
    /// 通知（GUI 按各类 Agent 通知开关决定；这个决定权不下放到
    /// smeltd，因为那是纯 GUI 展示偏好，smeltd 不该知道）。
    pub notify: Option<(String, String, bool)>,
}

/// 还挂着没人处理的卡片时对应的相位。审批优先于选择题，跟 `Ready` 分支里
/// 那段判断同序。
pub(super) fn pending_action_phase(state: &AcpSessionState) -> Option<DaemonPhase> {
    if !state.permissions.is_empty() {
        Some(DaemonPhase::AwaitingApproval)
    } else if state.elicitation.is_some() {
        Some(DaemonPhase::WaitingForUser)
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
pub(super) fn resume_running(state: &mut AcpSessionState) {
    state.phase = pending_action_phase(state).unwrap_or(DaemonPhase::Thinking);
}

/// 事件归约：entries 合并 + phase 机。跟旧版 `AcpView::apply_event` 逐行对应，
/// 唯一的行为差异是旁路效果收进返回值而不是直接执行。
///
/// `match` 只做分发；每条有实质逻辑的臂是 `apply_*`。流式增量等一行赋值仍留在
/// 分发里，避免为赋值再套一层函数。
pub fn apply_event(state: &mut AcpSessionState, ev: ConversationEvent) -> ApplyOutcome {
    let mut outcome = ApplyOutcome::default();
    let mut skip_persist = is_streaming_event(&ev);
    if clears_user_echo(&ev) {
        state.awaiting_user_echo = false;
    }

    match ev {
        ConversationEvent::AvailableCommands(list) => state.available_commands = list,
        ConversationEvent::Usage {
            used,
            size,
            cached_read,
            cost,
            breakdown,
        } => apply_usage(state, used, size, cached_read, cost, breakdown),
        ConversationEvent::SessionControls {
            compaction,
            native_queue,
            rewind,
        } => {
            state.supports_compaction = compaction;
            state.supports_native_queue = native_queue;
            state.supports_rewind = rewind;
        }
        ConversationEvent::Compaction {
            running,
            detail,
            used,
            size,
        } => {
            state.compacting = running;
            if !detail.is_empty() {
                state.status_line = Some(detail);
            }
            if let (Some(used), Some(size)) = (used, size)
                && size > 0
            {
                state.usage = Some((used, size));
            }
        }
        ConversationEvent::PromptQueue {
            steering,
            follow_up,
        } => {
            if apply_prompt_queue(state, &mut outcome, steering, follow_up) {
                skip_persist = false;
            }
        }
        ConversationEvent::ComposerRestore { revision, texts } => {
            state.queued_steering.clear();
            state.queued_steering_images.clear();
            state.queued_follow_up.clear();
            if revision > state.composer_restore_revision {
                state.composer_restore_revision = revision;
                state.composer_restore_texts = texts;
            }
        }
        ConversationEvent::Status(msg) => state.status_line = Some(msg),
        ConversationEvent::SessionTitle(title) => state.protocol_title = title,
        ConversationEvent::HistoryReplayStarted => {
            apply_history_replay_started(state, &mut outcome)
        }
        ConversationEvent::HistoryReplayFinished => state.replaying_history = false,
        ConversationEvent::RestoreFailed(failure) => apply_restore_failed(state, failure),
        ConversationEvent::UserChunk(text) => apply_user_chunk(state, &mut outcome, text),
        ConversationEvent::UserImage(image) => apply_user_image(state, &mut outcome, image),
        ConversationEvent::Ready {
            session_id,
            kind,
            supports_image,
        } => apply_ready(state, &mut outcome, session_id, kind, supports_image),
        ConversationEvent::AgentChunk {
            thought,
            text,
            parent_id,
        } => apply_agent_chunk(state, &mut outcome, thought, text, parent_id),
        ConversationEvent::TerminalOutput {
            terminal_id,
            output,
            truncated,
            exit_code,
            signal,
        } => apply_terminal_output(state, terminal_id, output, truncated, exit_code, signal),
        ConversationEvent::ToolCall(tc) => apply_tool_call(state, &mut outcome, tc),
        ConversationEvent::ToolCallUpdate(update) => {
            apply_tool_call_update(state, &mut outcome, update)
        }
        ConversationEvent::ToolDebug {
            id,
            name,
            raw_input,
        } => apply_tool_debug(state, id, name, raw_input),
        ConversationEvent::RuntimeDebug(debug) => {
            state.runtime_debug = debug;
            outcome.runtime_debug_changed = true;
        }
        ConversationEvent::ToolStarted { id, title, kind } => {
            apply_tool_started(state, &mut outcome, id, title, kind);
        }
        ConversationEvent::ToolOutputDelta { id, delta } => {
            // 取消后的迟到增量必须在设置 should_persist 之前返回，保持「不落盘」。
            if is_late_tool_after_cancel(state, &id) {
                return outcome;
            }
            apply_tool_output_delta(state, &mut outcome, id, delta);
        }
        ConversationEvent::ToolFinished { id, status, output } => {
            apply_tool_finished(state, &mut outcome, id, status, output)
        }
        ConversationEvent::ToolChildren {
            id,
            children,
            debug,
        } => apply_tool_children(state, &mut outcome, id, children, debug),
        ConversationEvent::Model(model) => state.model = Some(model),
        ConversationEvent::ConfigOptions(options) => state.config_options = options,
        ConversationEvent::Plan(plan) => apply_plan(state, plan),
        ConversationEvent::Permission {
            question,
            tool_call_id,
            pub_options,
            responder,
            details,
            raw_request_line,
        } => apply_permission(
            state,
            &mut outcome,
            LivePermission {
                question,
                tool_call_id: tool_call_id.to_string(),
                options: pub_options,
                details,
                responder: Some(responder),
                raw_request_line,
            },
        ),
        ConversationEvent::Elicitation {
            message,
            fields,
            responder,
            raw_request_line,
        } => apply_elicitation(
            state,
            &mut outcome,
            message,
            fields,
            responder,
            raw_request_line,
        ),
        ConversationEvent::TurnEnded(reason) => outcome.entries_offset = finish_turn(state, reason),
        ConversationEvent::TurnFailed(msg) => apply_turn_failed(state, &mut outcome, msg),
        ConversationEvent::Rewound { truncate_from } => {
            apply_rewound(state, &mut outcome, truncate_from)
        }
        ConversationEvent::ProviderSessionIdChanged(session_id) => {
            apply_provider_session_id_changed(state, session_id)
        }
        ConversationEvent::Fatal(msg) => super::force_end(state, AcpEndKind::ProviderFailed, msg),
    }

    outcome.should_persist = !skip_persist;
    outcome
}

/// Pi 的 steering 队列是 FIFO。新列表若是旧列表的后缀，前缀就是已经插入当前
/// 回合的消息，这时才写入会话；还在队列里的不得出现用户气泡。
fn apply_prompt_queue(
    state: &mut AcpSessionState,
    outcome: &mut ApplyOutcome,
    steering: Vec<String>,
    follow_up: Vec<String>,
) -> bool {
    let old = std::mem::take(&mut state.queued_steering);
    let old_images = std::mem::take(&mut state.queued_steering_images);
    let consumed = consumed_steering_count(&old, &steering);
    let first_new = state.entries.len();
    for i in 0..consumed {
        super::note_mid_turn_input(
            state,
            old[i].clone(),
            old_images.get(i).cloned().unwrap_or_default(),
        );
    }
    let mut remain_images: Vec<_> = old_images.into_iter().skip(consumed).collect();
    remain_images.resize(steering.len(), Vec::new());
    state.queued_steering = steering;
    state.queued_steering_images = remain_images;
    state.queued_follow_up = follow_up;
    if consumed == 0 {
        return false;
    }
    outcome.entries_offset = Some(first_new);
    true
}

fn consumed_steering_count(old: &[String], new: &[String]) -> usize {
    if new.len() <= old.len() && old.ends_with(new) {
        old.len() - new.len()
    } else {
        0
    }
}

fn is_streaming_event(ev: &ConversationEvent) -> bool {
    matches!(
        ev,
        ConversationEvent::AgentChunk { .. }
            | ConversationEvent::TerminalOutput { .. }
            | ConversationEvent::Plan(_)
            | ConversationEvent::Model(_)
            | ConversationEvent::ConfigOptions(_)
            | ConversationEvent::Usage { .. }
            | ConversationEvent::RuntimeDebug(_)
            | ConversationEvent::SessionControls { .. }
            | ConversationEvent::Compaction { .. }
            | ConversationEvent::PromptQueue { .. }
            | ConversationEvent::ComposerRestore { .. }
    )
}

fn clears_user_echo(ev: &ConversationEvent) -> bool {
    !matches!(
        ev,
        ConversationEvent::UserChunk(_)
            | ConversationEvent::UserImage(_)
            | ConversationEvent::Status(_)
            | ConversationEvent::AvailableCommands(_)
            | ConversationEvent::Usage { .. }
            | ConversationEvent::RuntimeDebug(_)
            | ConversationEvent::SessionControls { .. }
            | ConversationEvent::Compaction { .. }
            | ConversationEvent::PromptQueue { .. }
            | ConversationEvent::ComposerRestore { .. }
            | ConversationEvent::Plan(_)
            | ConversationEvent::Model(_)
            | ConversationEvent::ConfigOptions(_)
            | ConversationEvent::SessionTitle(_)
            | ConversationEvent::Ready { .. }
    )
}

fn apply_history_replay_started(state: &mut AcpSessionState, outcome: &mut ApplyOutcome) {
    outcome.entries_offset = Some(0);
    state.entries.clear();
    state.tool_debug.clear();
    state.replaying_history = true;
    state.cancelled_tool_call_ids.clear();
    state.cancelled_turn_seq = None;
    state.tool_turn_seq.clear();
}

fn apply_restore_failed(state: &mut AcpSessionState, failure: ConversationRestoreFailure) {
    super::force_end(state, AcpEndKind::RestoreFailed, failure.to_string());
    state.replaying_history = false;
}

fn apply_user_chunk(state: &mut AcpSessionState, outcome: &mut ApplyOutcome, text: String) {
    clear_recovered_elicitation_after_replayed_user_message(state);
    if state.awaiting_user_echo {
        // 自己刚发那条的回声——本地已经在 apply_user_action(Prompt) 时显示过了。
        return;
    }
    outcome.entries_offset = Some(state.entries.len().saturating_sub(1));
    match state.entries.last_mut() {
        Some(AcpEntry::User(existing)) => existing.push_str(&text),
        Some(AcpEntry::UserWithImages { text: existing, .. }) => existing.push_str(&text),
        _ => {
            outcome.entries_offset = Some(state.entries.len());
            state.entries.push(AcpEntry::User(text));
        }
    }
}

fn apply_user_image(state: &mut AcpSessionState, outcome: &mut ApplyOutcome, image: PromptImage) {
    if state.awaiting_user_echo {
        return;
    }
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

fn apply_ready(
    state: &mut AcpSessionState,
    outcome: &mut ApplyOutcome,
    session_id: SessionId,
    kind: ReadyKind,
    supports_image: bool,
) {
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
        DaemonPhase::AwaitingApproval
    } else if state.elicitation.is_some() {
        DaemonPhase::WaitingForUser
    } else if has_active_turn {
        DaemonPhase::Thinking
    } else {
        DaemonPhase::Idle
    };
    state.status_line = None;
}

fn apply_agent_chunk(
    state: &mut AcpSessionState,
    outcome: &mut ApplyOutcome,
    thought: bool,
    text: String,
    parent_id: Option<String>,
) {
    if is_cancelled_generation(state) {
        // 仍停在已取消世代上：迟到正文不能长进消息流，也不能污染下一轮。
        return;
    }
    outcome.entries_offset = push_agent_text_inner(state, text, thought, parent_id);
    if !state.replaying_history && state.turn_started_at_ms.is_some() {
        resume_running(state);
    }
}

fn apply_terminal_output(
    state: &mut AcpSessionState,
    terminal_id: String,
    output: String,
    truncated: bool,
    exit_code: Option<i32>,
    signal: Option<String>,
) {
    let snapshot = crate::acp_terminal::TerminalSnapshot {
        output,
        truncated,
        exit_code,
        signal,
    };
    apply_terminal_snapshot(state, &terminal_id, &snapshot);
    state.terminal_buffers.insert(terminal_id, snapshot);
}

fn apply_tool_call(state: &mut AcpSessionState, outcome: &mut ApplyOutcome, tc: ToolCall) {
    let tool_call_id = tc.tool_call_id.to_string();
    apply_tool_debug(state, tool_call_id.clone(), None, tc.raw_input.clone());
    let incoming_status = crate::acp_conn::tool_status_from_acp(tc.status);
    let tool_kind = crate::acp_conn::tool_kind_from_acp_call(&tc);
    let parent_id = crate::acp_conn::parent_tool_id_from_meta(tc.meta.as_ref());
    remember_tool_turn_seq(state, &tool_call_id);
    let late_after_cancel = is_late_tool_after_cancel(state, &tool_call_id);
    if late_after_cancel {
        state.cancelled_tool_call_ids.insert(tool_call_id.clone());
    }
    let tool_status = if late_after_cancel {
        crate::acp_chat::ToolCallStatus::Failed
    } else {
        incoming_status
    };
    let replayed_elicitation = matches!(
        tool_status,
        crate::acp_chat::ToolCallStatus::Pending | crate::acp_chat::ToolCallStatus::InProgress
    )
    .then(|| recovered_elicitation(&tc.title, tc.raw_input.as_ref(), &tool_call_id))
    .flatten();
    let mut output = if late_after_cancel {
        cancelled_tool_output(crate::acp_conn::tool_content_parts(&tc.content))
    } else {
        crate::acp_conn::tool_content_parts(&tc.content)
    };
    hydrate_terminal_output(&state.terminal_buffers, &mut output);
    let title = if tool_kind == crate::acp_chat::ToolKind::Collaborate {
        crate::acp_chat::subagent_display_title(&tc.title, tc.raw_input.as_ref())
    } else {
        tc.title
    };
    let entry = AcpEntry::tool_call(tool_call_id.clone(), title, tool_kind, tool_status, output);
    outcome.entries_offset = push_tool_entry(state, entry, parent_id.as_deref(), &tool_call_id);
    if let Some(elicitation) = replayed_elicitation {
        state.elicitation = Some(elicitation);
        state.phase = DaemonPhase::WaitingForUser;
    } else if !state.replaying_history && state.turn_started_at_ms.is_some() {
        resume_running(state);
    }
}

fn apply_tool_call_update(
    state: &mut AcpSessionState,
    outcome: &mut ApplyOutcome,
    update: ToolCallUpdate,
) {
    let update_id = update.tool_call_id.to_string();
    apply_tool_debug(
        state,
        update_id.clone(),
        None,
        update.fields.raw_input.clone(),
    );
    remember_tool_turn_seq(state, &update_id);
    let update_status = update
        .fields
        .status
        .map(crate::acp_conn::tool_status_from_acp);
    let update_kind = crate::acp_conn::tool_kind_from_acp_fields(
        update.fields.kind,
        update.fields.title.as_deref(),
        update.fields.raw_input.as_ref(),
        update.meta.as_ref(),
    );
    let accepts_update = !is_late_tool_after_cancel(state, &update_id);
    let remains_pending = update_status.is_none_or(|status| {
        matches!(
            status,
            crate::acp_chat::ToolCallStatus::Pending | crate::acp_chat::ToolCallStatus::InProgress
        )
    });
    let replayed_elicitation = (accepts_update && remains_pending)
        .then_some(update.fields.raw_input.as_ref())
        .flatten()
        .and_then(|raw_input| {
            let title = update.fields.title.clone().or_else(|| {
                crate::acp_chat::find_tool_call_mut(&mut state.entries, &update_id).and_then(
                    |entry| match entry {
                        AcpEntry::ToolCall { title, .. } => Some(title.clone()),
                        _ => None,
                    },
                )
            })?;
            recovered_elicitation(&title, Some(raw_input), &update_id)
        });
    let parent_id = crate::acp_conn::parent_tool_id_from_meta(update.meta.as_ref());
    outcome.entries_offset = crate::acp_chat::find_tool_top_level_index(&state.entries, &update_id);
    if let Some(entry) = crate::acp_chat::find_tool_call_mut(&mut state.entries, &update_id)
        && let AcpEntry::ToolCall {
            title,
            kind,
            status,
            output,
            ..
        } = entry
    {
        if let Some(k) = update_kind {
            *kind = k;
        }
        if *kind == crate::acp_chat::ToolKind::Collaborate {
            if let Some(t) = update.fields.title {
                *title =
                    crate::acp_chat::subagent_display_title(&t, update.fields.raw_input.as_ref());
            } else if let Some(args) = update.fields.raw_input.as_ref() {
                let next = crate::acp_chat::subagent_display_title(title.as_str(), Some(args));
                *title = next;
            }
        } else if let Some(t) = update.fields.title {
            *title = t;
        }
        if accepts_update {
            if let Some(s) = update_status {
                *status = s;
            }
            if let Some(c) = update.fields.content {
                *output = crate::acp_conn::tool_content_parts(&c);
                hydrate_terminal_output(&state.terminal_buffers, output);
            }
        }
    } else if accepts_update {
        let mut output = update
            .fields
            .content
            .as_ref()
            .map(|c| crate::acp_conn::tool_content_parts(c))
            .unwrap_or_default();
        hydrate_terminal_output(&state.terminal_buffers, &mut output);
        let kind = update_kind.unwrap_or(crate::acp_chat::ToolKind::Other);
        let raw_title = update.fields.title.clone().unwrap_or_default();
        let title = if kind == crate::acp_chat::ToolKind::Collaborate {
            crate::acp_chat::subagent_display_title(&raw_title, update.fields.raw_input.as_ref())
        } else {
            raw_title
        };
        let entry = AcpEntry::tool_call(
            update_id.clone(),
            title,
            kind,
            update_status.unwrap_or(crate::acp_chat::ToolCallStatus::InProgress),
            output,
        );
        outcome.entries_offset = push_tool_entry(state, entry, parent_id.as_deref(), &update_id);
    }
    if let Some(elicitation) = replayed_elicitation {
        state.elicitation = Some(elicitation);
        state.phase = DaemonPhase::WaitingForUser;
    } else if update_status.is_some_and(|status| {
        matches!(
            status,
            crate::acp_chat::ToolCallStatus::Completed | crate::acp_chat::ToolCallStatus::Failed
        )
    }) {
        clear_recovered_elicitation(state, &update_id);
    }
}

fn apply_usage(
    state: &mut AcpSessionState,
    used: u64,
    size: u64,
    cached_read: Option<u64>,
    cost: Option<f64>,
    breakdown: Option<crate::acp_conn::ContextUsageBreakdown>,
) {
    if size > 0 {
        if used > 0 {
            state.usage = Some((used, size));
        } else if let Some((_, window)) = &mut state.usage {
            // used == 0 means this event only carries billing metadata or an
            // explicitly unknown Pi context size. Preserve the last authority.
            *window = size;
        }
    }
    if cached_read.is_some() {
        state.usage_cached_read = cached_read;
    }
    if cost.is_some() {
        state.usage_cost = cost;
    }
    if breakdown.is_some() {
        state.usage_breakdown = breakdown;
    }
}

fn apply_tool_debug(
    state: &mut AcpSessionState,
    id: String,
    name: Option<String>,
    raw_input: Option<serde_json::Value>,
) {
    if name.is_none() && raw_input.is_none() {
        return;
    }
    let debug = state.tool_debug.entry(id).or_default();
    if name.is_some() {
        debug.name = name;
    }
    if raw_input.is_some() {
        debug.raw_input = raw_input;
    }
}

fn apply_tool_started(
    state: &mut AcpSessionState,
    outcome: &mut ApplyOutcome,
    id: String,
    title: String,
    kind: crate::acp_chat::ToolKind,
) {
    remember_tool_turn_seq(state, &id);
    let late_after_cancel = is_late_tool_after_cancel(state, &id);
    if late_after_cancel {
        state.cancelled_tool_call_ids.insert(id.clone());
    }
    if crate::acp_chat::contains_tool_call(&state.entries, &id) {
        outcome.entries_offset = crate::acp_chat::find_tool_top_level_index(&state.entries, &id);
        if !late_after_cancel
            && let Some(crate::acp_chat::AcpEntry::ToolCall {
                title: current_title,
                kind: current_kind,
                ..
            }) = crate::acp_chat::find_tool_call_mut(&mut state.entries, &id)
        {
            if !title.is_empty() {
                *current_title = title;
            }
            *current_kind = kind;
        }
        return;
    }
    outcome.entries_offset = Some(state.entries.len());
    state.entries.push(AcpEntry::tool_call(
        id,
        title,
        kind,
        if late_after_cancel {
            crate::acp_chat::ToolCallStatus::Failed
        } else {
            crate::acp_chat::ToolCallStatus::InProgress
        },
        if late_after_cancel {
            cancelled_tool_output(Vec::new())
        } else {
            Vec::new()
        },
    ));
    if !state.replaying_history && state.turn_started_at_ms.is_some() {
        resume_running(state);
    }
}

fn apply_tool_output_delta(
    state: &mut AcpSessionState,
    outcome: &mut ApplyOutcome,
    id: String,
    delta: String,
) {
    outcome.entries_offset = crate::acp_chat::find_tool_top_level_index(&state.entries, &id);
    if let Some(AcpEntry::ToolCall { output, .. }) =
        crate::acp_chat::find_tool_call_mut(&mut state.entries, &id)
    {
        match output.last_mut() {
            Some(crate::acp_chat::ToolOutputPart::Text(text)) => text.push_str(&delta),
            _ => output.push(crate::acp_chat::ToolOutputPart::Text(delta)),
        }
    }
}

fn apply_tool_children(
    state: &mut AcpSessionState,
    outcome: &mut ApplyOutcome,
    id: String,
    children: Vec<AcpEntry>,
    debug: std::collections::BTreeMap<String, super::ToolCallDebug>,
) {
    outcome.entries_offset = crate::acp_chat::find_tool_top_level_index(&state.entries, &id);
    let mut replaced_ids = std::collections::BTreeSet::new();
    let mut next_ids = std::collections::BTreeSet::new();
    collect_tool_ids(&children, &mut next_ids);
    let mut replaced = false;
    if let Some(AcpEntry::ToolCall {
        children: current, ..
    }) = crate::acp_chat::find_tool_call_mut(&mut state.entries, &id)
    {
        collect_tool_ids(current, &mut replaced_ids);
        *current = children;
        replaced = true;
    }
    if !replaced {
        return;
    }
    state
        .tool_debug
        .retain(|tool_id, _| !replaced_ids.contains(tool_id));
    state
        .tool_debug
        .extend(debug.into_iter().filter(|(id, _)| next_ids.contains(id)));
}

fn apply_tool_finished(
    state: &mut AcpSessionState,
    outcome: &mut ApplyOutcome,
    id: String,
    status: crate::acp_chat::ToolCallStatus,
    output: Vec<crate::acp_chat::ToolOutputPart>,
) {
    let cancelled = is_late_tool_after_cancel(state, &id);
    outcome.entries_offset = crate::acp_chat::find_tool_top_level_index(&state.entries, &id);
    if let Some(AcpEntry::ToolCall {
        status: current,
        output: current_output,
        ..
    }) = crate::acp_chat::find_tool_call_mut(&mut state.entries, &id)
        && !cancelled
    {
        *current = status;
        *current_output = output;
    }
}

fn apply_plan(state: &mut AcpSessionState, plan: Plan) {
    if is_cancelled_generation(state) {
        return;
    }
    state.plan = if plan.entries.is_empty() {
        None
    } else {
        Some(super::plan_view_from_acp(&plan))
    };
    if !state.replaying_history && state.turn_started_at_ms.is_some() {
        resume_running(state);
    }
}

fn apply_permission(
    state: &mut AcpSessionState,
    outcome: &mut ApplyOutcome,
    permission: LivePermission,
) {
    if should_ignore_late_interactive_request(state, Some(&permission.tool_call_id)) {
        // responder 在此 drop → Cancelled，避免旧审批挂到下一回合。
        return;
    }
    let question = permission.question.clone();
    state.permissions.push(permission);
    state.phase = DaemonPhase::AwaitingApproval;
    outcome.notify = Some(("等你批准".to_string(), question, true));
}

fn apply_elicitation(
    state: &mut AcpSessionState,
    outcome: &mut ApplyOutcome,
    message: String,
    fields: Vec<ElicitField>,
    responder: ElicitationResponder,
    raw_request_line: Option<String>,
) {
    if should_ignore_late_interactive_request(state, None) {
        // 同上：取消窗口内的迟到选择题直接回 Cancel。
        return;
    }
    state.elicitation = Some(LiveElicitation {
        message: message.clone(),
        raw_fields: fields,
        chosen: Default::default(),
        text_values: Default::default(),
        responder: Some(responder),
        recovered_tool_call_id: None,
        raw_request_line,
    });
    state.phase = DaemonPhase::WaitingForUser;
    outcome.notify = Some(("等你选择".to_string(), message, false));
}

fn apply_turn_failed(state: &mut AcpSessionState, outcome: &mut ApplyOutcome, msg: String) {
    // 错误文本当成 assistant 正文落进消息流：它本来就是 agent 说的话，
    // 复用同一条渲染路径就不用给每个前端加一种新 entry。
    let offset = push_agent_text(state, format!("⚠️ 回合失败：{msg}"));
    let tool_offset = finish_turn_with(state, AcpTurnOutcome::Failed);
    outcome.entries_offset = match (offset, tool_offset) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
}

/// 回合已经结束、但仍有工具调用停在 Pending/InProgress 时的收尾。
///
/// 正常情况下这些工具的终态只是晚到几毫秒；GUI 保留真实 Idle/结果，只用独立
/// settling gate 阻止新 prompt 抢跑。可某些 adapter 会彻底不补发终态——比如
/// shell 输出读取被中断。这时挂在上面的 controller 任务仍无法安全收尾；宽限期过后
/// 强制把工具收敛成终态，让完成边沿一定能出现。
///
/// 返回被改写的最小 entry 下标（用于增量广播）；没有可收尾的工具时返回 `None`。
pub fn finalize_dangling_tool_calls(state: &mut AcpSessionState) -> Option<usize> {
    // 还有人在等（待批权限 / 待答选择题）说明回合并没有真的结束，此时的
    // 未结束工具是合理状态，不能收尾。
    if !state.permissions.is_empty() || state.elicitation.is_some() {
        return None;
    }
    if !matches!(state.phase, DaemonPhase::Idle) || state.replaying_history {
        return None;
    }
    let mut first_changed = None;
    for (index, entry) in state.entries.iter_mut().enumerate() {
        if finalize_dangling_entry(entry, true) {
            first_changed.get_or_insert(index);
        }
    }
    first_changed
}

/// 后台协作代理及其嵌套工具在 idle 后继续跑，不能收成 Failed。
fn finalize_dangling_entry(entry: &mut AcpEntry, skip_active_agents: bool) -> bool {
    let AcpEntry::ToolCall {
        kind,
        status,
        children,
        ..
    } = entry
    else {
        return false;
    };
    if skip_active_agents && crate::acp_chat::is_active_agent_tool(*kind, *status, children) {
        return false;
    }
    if skip_active_agents && *kind == crate::acp_chat::ToolKind::Collaborate {
        return children
            .iter_mut()
            .any(|child| finalize_dangling_entry(child, true));
    }
    let mut changed = false;
    if is_unfinished_tool_status(*status) {
        *status = crate::acp_chat::ToolCallStatus::Failed;
        changed = true;
    }
    for child in children {
        changed |= finalize_dangling_entry(child, skip_active_agents);
    }
    changed
}

/// Agent 按 ACP 语义确认取消后，不会保证再为每个已启动工具发送终态更新。
///
/// 取消必须让会话回到可派发：历史里未收尾的工具已经不是活跃回合，继续
/// `InProgress` 会让 daemon 握着 prompt 闸门，而 GUI 分页尾页又看不见它们，
/// 「立即发送」后的下一条就会进 `pending_prompts` 且等不到 Running 回执。
fn finish_cancelled_turn_tools(state: &mut AcpSessionState) -> Option<usize> {
    let mut first_changed = None;
    for (index, entry) in state.entries.iter_mut().enumerate() {
        if finish_cancelled_entry(entry, &mut state.cancelled_tool_call_ids) {
            first_changed.get_or_insert(index);
        }
    }
    first_changed
}

fn finish_cancelled_entry(entry: &mut AcpEntry, cancelled_ids: &mut BTreeSet<String>) -> bool {
    let AcpEntry::ToolCall {
        id,
        status,
        output,
        children,
        ..
    } = entry
    else {
        return false;
    };
    let mut changed = false;
    if is_unfinished_tool_status(*status) {
        *status = crate::acp_chat::ToolCallStatus::Failed;
        output.extend(cancelled_tool_output(Vec::new()));
        cancelled_ids.insert(id.clone());
        changed = true;
    }
    for child in children {
        changed |= finish_cancelled_entry(child, cancelled_ids);
    }
    changed
}

fn is_unfinished_tool_status(status: crate::acp_chat::ToolCallStatus) -> bool {
    matches!(
        status,
        crate::acp_chat::ToolCallStatus::Pending | crate::acp_chat::ToolCallStatus::InProgress
    )
}

fn cancelled_tool_output(
    mut output: Vec<crate::acp_chat::ToolOutputPart>,
) -> Vec<crate::acp_chat::ToolOutputPart> {
    output.push(crate::acp_chat::ToolOutputPart::Text(
        "已因用户停止而取消".to_string(),
    ));
    output
}

fn finish_turn(state: &mut AcpSessionState, reason: StopReason) -> Option<usize> {
    let outcome = AcpTurnOutcome::from_stop_reason(reason, state.cancel_requested);
    finish_turn_with(state, outcome)
}

/// 回退落地：把目标用户消息及其之后的所有条目从投影里丢掉。发起方（daemon
/// 的用户动作臂）已确认过相位是 Idle，这里不再重复闸门检查，但游标可能因
/// 极端时序超出本地投影范围，超出时按“无变化”处理，别把会话清空。
fn apply_rewound(state: &mut AcpSessionState, outcome: &mut ApplyOutcome, truncate_from: usize) {
    if truncate_from >= state.entries.len() {
        return;
    }
    state.entries.truncate(truncate_from);
    let mut remaining_tool_ids = std::collections::BTreeSet::new();
    collect_tool_ids(&state.entries, &mut remaining_tool_ids);
    state
        .tool_debug
        .retain(|id, _| remaining_tool_ids.contains(id));
    // fork 已把 provider 侧队列一并作废（teardown 会 abort），本地镜像同步清空。
    state.queued_steering.clear();
    state.queued_steering_images.clear();
    state.queued_follow_up.clear();
    // 被截掉的回合没有“未读完成”可言；计时也只保留仍存在的那部分。
    state.completed_unread = false;
    state.completed_delivery_id = None;
    state.turn_outcome = None;
    state
        .turn_timings
        .retain(|timing| timing.user_index < truncate_from);
    // 全量快照重发：截断是“变短”，增量协议只描述“变长/改写”，
    // offset=0 是唯一能让所有消费者对齐的传播方式。
    outcome.entries_offset = Some(0);
}

/// Pi fork 之后 provider 侧身份换了新 session 文件。恢复 / 续接必须认新 id，
/// 否则下次重连会把被丢弃的旧分支重新放出来。
fn collect_tool_ids(entries: &[AcpEntry], ids: &mut std::collections::BTreeSet<String>) {
    for entry in entries {
        if let AcpEntry::ToolCall { id, children, .. } = entry {
            ids.insert(id.clone());
            collect_tool_ids(children, ids);
        }
    }
}

fn apply_provider_session_id_changed(state: &mut AcpSessionState, session_id: SessionId) {
    let session_id = session_id.to_string();
    if session_id.is_empty() {
        return;
    }
    state.acp_session_id = Some(session_id.clone());
    state.history_session_id = Some(session_id);
}

/// 把一段 agent 正文/思考文本落进消息流：跟上一条同类 entry 合并，否则新起
/// 一条。返回受影响的最小 entry 下标（增量广播用）。
fn push_agent_text_inner(
    state: &mut AcpSessionState,
    text: String,
    thought: bool,
    parent_id: Option<String>,
) -> Option<usize> {
    if let Some(parent_id) = parent_id.as_deref() {
        let offset = crate::acp_chat::find_tool_top_level_index(&state.entries, parent_id);
        if let Some(AcpEntry::ToolCall { children, .. }) =
            crate::acp_chat::find_tool_call_mut(&mut state.entries, parent_id)
        {
            append_agent_text(children, text, thought);
            return offset;
        }
    }
    append_agent_text(&mut state.entries, text, thought);
    Some(state.entries.len().saturating_sub(1))
}

fn append_agent_text(entries: &mut Vec<AcpEntry>, text: String, thought: bool) {
    match entries.last_mut() {
        Some(AcpEntry::Assistant {
            text: t,
            thought: th,
        }) if *th == thought => t.push_str(&text),
        _ => entries.push(AcpEntry::Assistant { text, thought }),
    }
}

fn push_agent_text(state: &mut AcpSessionState, text: String) -> Option<usize> {
    push_agent_text_inner(state, text, false, None)
}

fn push_tool_entry(
    state: &mut AcpSessionState,
    entry: AcpEntry,
    parent_id: Option<&str>,
    tool_id: &str,
) -> Option<usize> {
    if let Some(parent_id) = parent_id {
        let offset = crate::acp_chat::find_tool_top_level_index(&state.entries, parent_id);
        if let Some(AcpEntry::ToolCall { children, .. }) =
            crate::acp_chat::find_tool_call_mut(&mut state.entries, parent_id)
        {
            children.push(entry);
            return offset;
        }
    }
    if crate::acp_chat::contains_tool_call(&state.entries, tool_id) {
        return crate::acp_chat::find_tool_top_level_index(&state.entries, tool_id);
    }
    let at = state.entries.len();
    state.entries.push(entry);
    Some(at)
}

fn hydrate_terminal_output(
    buffers: &BTreeMap<String, crate::acp_terminal::TerminalSnapshot>,
    output: &mut Vec<crate::acp_chat::ToolOutputPart>,
) {
    for part in output {
        if let crate::acp_chat::ToolOutputPart::Terminal {
            id,
            output,
            truncated,
            exit_code,
            signal,
        } = part
            && let Some(snapshot) = buffers.get(id)
        {
            *output = snapshot.output.clone();
            *truncated = snapshot.truncated;
            *exit_code = snapshot.exit_code;
            *signal = snapshot.signal.clone();
        }
    }
}

fn apply_terminal_snapshot(
    state: &mut AcpSessionState,
    terminal_id: &str,
    snapshot: &crate::acp_terminal::TerminalSnapshot,
) {
    fn visit(
        entries: &mut [AcpEntry],
        terminal_id: &str,
        snapshot: &crate::acp_terminal::TerminalSnapshot,
    ) {
        for entry in entries {
            if let AcpEntry::ToolCall {
                output, children, ..
            } = entry
            {
                for part in output {
                    if let crate::acp_chat::ToolOutputPart::Terminal {
                        id,
                        output,
                        truncated,
                        exit_code,
                        signal,
                    } = part
                        && id == terminal_id
                    {
                        *output = snapshot.output.clone();
                        *truncated = snapshot.truncated;
                        *exit_code = snapshot.exit_code;
                        *signal = snapshot.signal.clone();
                    }
                }
                visit(children, terminal_id, snapshot);
            }
        }
    }
    visit(&mut state.entries, terminal_id, snapshot);
}

/// `finish_turn` 的下层：回合终态已经定好时直接收尾。回合以 agent 明确回的
/// 错误收场时没有 `StopReason` 可映射，走这条。
fn finish_turn_with(state: &mut AcpSessionState, outcome: AcpTurnOutcome) -> Option<usize> {
    let outcome = if state.cancel_requested {
        AcpTurnOutcome::Cancelled
    } else {
        outcome
    };
    let cancelled = outcome == AcpTurnOutcome::Cancelled;
    state.cancel_requested = false;
    if cancelled {
        state.cancelled_turn_seq = Some(state.turn_seq);
    }
    let tool_offset = cancelled
        .then(|| finish_cancelled_turn_tools(state))
        .flatten();
    state.permissions.clear();
    state.elicitation = None;
    state.phase = DaemonPhase::Idle;
    state.end_reason.clear();
    state.completed_delivery_id = state.active_delivery_id.take();
    state.completed_unread = true;
    state.turn_outcome = Some(outcome);
    let ended_at_ms = super::unix_time_ms();
    if let Some(timing) = state.turn_timings.last_mut()
        && timing.ended_at_ms.is_none()
    {
        timing.ended_at_ms = Some(ended_at_ms);
    }
    state.turn_started_at_ms = None;
    tool_offset
}

/// Some agents replay an unfinished AskUserQuestion as a pending tool call but do not recreate
/// the ACP elicitation responder. The raw tool input still contains the questions and choices, so
/// keep them actionable through a prompt-backed elicitation (`responder: None`).
pub(super) fn recovered_elicitation(
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
            // 恢复的也是题目卡：答案同样允许在选项之外自己写。
            allow_custom_input: true,
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
        DaemonPhase::AwaitingApproval
    } else if !state.replaying_history && state.turn_started_at_ms.is_some() {
        DaemonPhase::Thinking
    } else {
        DaemonPhase::Idle
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

fn is_cancelled_generation(state: &AcpSessionState) -> bool {
    state
        .cancelled_turn_seq
        .is_some_and(|seq| seq == state.turn_seq)
}

fn remember_tool_turn_seq(state: &mut AcpSessionState, tool_id: &str) {
    state
        .tool_turn_seq
        .entry(tool_id.to_string())
        .or_insert(state.turn_seq);
}

fn is_late_tool_after_cancel(state: &AcpSessionState, tool_id: &str) -> bool {
    if state.cancelled_tool_call_ids.contains(tool_id) || is_cancelled_generation(state) {
        return true;
    }
    state
        .cancelled_turn_seq
        .is_some_and(|cancelled| state.tool_turn_seq.get(tool_id) == Some(&cancelled))
}

fn should_ignore_late_interactive_request(
    state: &AcpSessionState,
    tool_call_id: Option<&str>,
) -> bool {
    if state.cancel_requested || is_cancelled_generation(state) {
        return true;
    }
    let Some(tool_id) = tool_call_id else {
        return false;
    };
    if is_late_tool_after_cancel(state, tool_id) {
        return true;
    }
    // 已经进入比取消世代更新的回合，却来一张从未见过的工具审批：
    // 协议里审批跟在 tool_call 后面，这更像旧世代迟到请求。
    // 首轮、或从未取消过时，审批可以是某工具的第一次出现。
    state.cancelled_turn_seq.is_some()
        && !is_cancelled_generation(state)
        && state.turn_started_at_ms.is_some()
        && !state.tool_turn_seq.contains_key(tool_id)
        && !state
            .entries
            .iter()
            .any(|entry| matches!(entry, AcpEntry::ToolCall { id, .. } if id == tool_id))
}
