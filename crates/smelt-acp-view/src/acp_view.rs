//! ACP 会话的消息流视图：第二种会话类型的 GPUI 皮肤。
//!
//! **薄客户端**：agent 子进程由 smeltd 托管（`smelt_core::acp_session` 里的
//! `apply_event` 归约、`ConversationEvent::Permission`/`Elicitation` 的 responder 也
//! 都在那边——responder 绑在连接线程上没法跨进程传，这里没有资格直接持有
//! 它们）。这层只做两件事：把 `smelt_core::acp_client` 收到的 `ConversationSnapshot`
//! 摊平进本地字段渲染出来，把用户操作打包成 `AcpUserAction` 发回去。四档
//! 着色 / Dock 角标 / 应用内待处理通知现在都由 smeltd 的集中状态订阅驱动
//! （跟终端会话共用 subscribe 通道），这层只镜像视图态，不再自己判相位跳变。

use gpui::prelude::FluentBuilder;
use gpui::{
    Anchor, Animation, AnimationExt, App, AppContext, ClipboardItem, Context, Entity, EventEmitter,
    FocusHandle, Focusable, FollowMode, InteractiveElement, IntoElement, ListAlignment, ListState,
    ParentElement, PathBuilder, Render, ScrollHandle, StatefulInteractiveElement, Styled,
    TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, canvas, div,
    list as virtual_list, point, px, size,
};
use gpui_component::button::{Button, ButtonRounded, ButtonVariants};
use gpui_component::clipboard::Clipboard;
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_component::popover::Popover;
use gpui_component::scroll::{Scrollbar, ScrollbarMode};
use gpui_component::text::TextView;
use gpui_component::{ActiveTheme, Icon, IconName, RopeExt, Sizable, StyledExt, h_flex, v_flex};

use agent_client_protocol::schema::v1::SessionId;

use smelt_core::acp_client::{
    ACP_HISTORY_PAGE_LIMIT, ConversationClientHandle, ConversationClientLaunch, load_acp_history,
    spawn_acp_client,
};
use smelt_core::acp_conn::{ModelProviderGroup, ModelState, SessionConfigState};
use smelt_core::acp_session::{
    AcpEndKind, AcpTurnOutcome, AcpUserAction, ApprovalDetailsView, ConversationSnapshot,
    ElicitFieldKindView, PendingElicitation, PendingPermission, PermissionOptionKindView,
    PlanEntryStatusView, PlanView,
};
use smelt_core::agent_kind::{ConversationAgentKind, ConversationLaunchSpec};
use smelt_core::agent_status::{AcpStatusEvidence, AgentStatus};
use smelt_core::daemon_state::DaemonPhase;
use smelt_ui::daemon_states_global::{AttentionGlobal, AttentionKind};
use smelt_ui::motion::{
    ambient_animation, ambient_application_active, ambient_motion_enabled, ambient_spinner,
};
use smelt_ui::ui_theme;

/// 消息流数据模型（AcpEntry/ToolOutputPart/ToolKind/ToolCallStatus）与 diff/
/// markdown 围栏这批纯逻辑现在都活在 `smelt_core::acp_chat`——不依赖 GPUI 也不
/// 依赖 agent_client_protocol，未来 web/mobile 端渲染同一份对话时不用重新实现
/// 一遍「怎么把协议事件变成可展示内容」。这里整段 re-export，文件里大量既有的
/// 裸 `AcpEntry::...` 用法不用逐处改路径。
pub use smelt_core::acp_chat::{
    AcpEntry, AcpImage, DiffLine, DiffLineTag, ToolCallStatus, ToolKind, ToolOutputPart,
    compact_diff_lines, completion_summary_text, diff_line_stats, diff_lines, is_interrupt_marker,
    is_task_completion_tool_title, strip_code_fence,
};

mod render;
#[cfg(test)]
mod tests;
mod trajectory;

const RESTORED_ENTRY_HEIGHT_HINT_PX: f32 = 96.;
const AUTO_RECONNECT_ATTEMPTS: u32 = 6;

fn model_label_with_provider(value: &str, name: &str) -> String {
    value
        .split_once('/')
        .filter(|(provider, model)| !provider.is_empty() && !model.is_empty())
        .map_or_else(
            || name.to_string(),
            |(provider, _)| format!("{provider} · {name}"),
        )
}

/// 输入栏模型按钮要显示当前模型名，不能退化成 agent 简称。
fn composer_model_label(model: Option<&ModelState>, agent_short: &str) -> String {
    let Some(model) = model else {
        return agent_short.to_string();
    };
    let name = model.current_name.trim();
    if !name.is_empty() {
        return model.current_name.clone();
    }
    let value = model.current_value.trim();
    if !value.is_empty() {
        return model.current_value.clone();
    }
    agent_short.to_string()
}

fn usage_percent(used: u64, size: u64) -> u32 {
    if size == 0 {
        0
    } else {
        (((used as f64 / size as f64) * 100.0).round() as u32).min(100)
    }
}

fn compact_token_count(n: u64) -> String {
    fn fmt(value: f64, suffix: &str) -> String {
        let rounded = (value * 10.0).round() / 10.0;
        if (rounded - rounded.round()).abs() < f64::EPSILON {
            format!("{:.0}{suffix}", rounded)
        } else {
            format!("{rounded:.1}{suffix}")
        }
    }
    if n >= 1_000_000 {
        fmt(n as f64 / 1_000_000.0, "M")
    } else if n >= 1_000 {
        fmt(n as f64 / 1_000.0, "k")
    } else {
        n.to_string()
    }
}

#[derive(Debug, Clone, PartialEq)]
struct UsageBreakdownRow {
    color: u32,
    label: &'static str,
    tokens: u64,
    percent: f32,
    free: bool,
}

fn usage_percent_of(part: u64, size: u64) -> f32 {
    if size == 0 {
        0.0
    } else {
        (part as f64 / size as f64 * 100.0) as f32
    }
}

fn composer_usage_breakdown(
    used: u64,
    size: u64,
    cached_read: Option<u64>,
    breakdown: Option<&smelt_core::acp_conn::ContextUsageBreakdown>,
    conversation_color: u32,
) -> Vec<UsageBreakdownRow> {
    let _ = cached_read;
    if let Some(buckets) = breakdown {
        let aligned = buckets.clone().aligned_to_used(used);
        return vec![
            usage_row(
                ui_theme::text_muted(),
                "System prompt",
                aligned.system_prompt,
                size,
            ),
            usage_row(
                ui_theme::purple(),
                "Tool definitions",
                aligned.tools_definition,
                size,
            ),
            usage_row(ui_theme::green(), "Rules", aligned.rules, size),
            usage_row(ui_theme::yellow(), "Skills", aligned.skills, size),
            usage_row(
                ui_theme::accent(),
                "MCP & dynamic tools",
                aligned.mcp_dynamic,
                size,
            ),
            usage_row(
                ui_theme::blue(),
                "Subagent definitions",
                aligned.subagent,
                size,
            ),
            usage_row(
                ui_theme::red(),
                "Summarized conversation",
                aligned.summarized,
                size,
            ),
            usage_row(
                conversation_color,
                "Conversation",
                aligned.conversation,
                size,
            ),
        ];
    }
    vec![usage_row(conversation_color, "Conversation", used, size)]
}

fn usage_row(color: u32, label: &'static str, tokens: u64, size: u64) -> UsageBreakdownRow {
    UsageBreakdownRow {
        color,
        label,
        tokens,
        percent: usage_percent_of(tokens, size),
        free: false,
    }
}

fn render_usage_stacked_bar(rows: &[UsageBreakdownRow], size: u64) -> gpui::AnyElement {
    let mut bar = h_flex()
        .w_full()
        .h(px(6.))
        .rounded_full()
        .overflow_hidden()
        .bg(ui_theme::overlay(0x22));
    if size == 0 {
        return bar.into_any_element();
    }
    for row in rows.iter().filter(|row| !row.free && row.tokens > 0) {
        let frac = (row.tokens as f32 / size as f32).clamp(0., 1.);
        bar = bar.child(
            div()
                .h_full()
                .flex_shrink_0()
                .w(gpui::relative(frac))
                .bg(gpui::rgb(row.color)),
        );
    }
    bar.into_any_element()
}

fn usage_token_header(used: u64, size: u64) -> String {
    if size == 0 {
        compact_token_count(used)
    } else {
        format!(
            "~{} / {}",
            compact_token_count(used),
            compact_token_count(size)
        )
    }
}

fn render_usage_hover_card(used: u64, size: u64, cached_read: Option<u64>) -> gpui::AnyElement {
    let pct = usage_percent(used, size);
    let tokens = if size == 0 {
        format!("{} tokens", compact_token_count(used))
    } else {
        format!(
            "{} / {} tokens",
            compact_token_count(used),
            compact_token_count(size)
        )
    };
    v_flex()
        .gap_1()
        .child(
            div()
                .text_sm()
                .font_medium()
                .text_color(gpui::rgb(ui_theme::text_bright()))
                .child(format!("{pct}% context used")),
        )
        .child(
            div()
                .text_sm()
                .text_color(gpui::rgb(ui_theme::text_muted()))
                .child(tokens),
        )
        .children(cached_read.filter(|tokens| *tokens > 0).map(|tokens| {
            div()
                .text_sm()
                .text_color(gpui::rgb(ui_theme::text_muted()))
                .child(format!("{} cached", compact_token_count(tokens)))
        }))
        .into_any_element()
}

fn usage_ring(pct: u32, color: gpui::Hsla, track: gpui::Hsla) -> gpui::AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let size = f32::from(bounds.size.width.min(bounds.size.height));
            if size < 2.0 {
                return;
            }
            let center_x = f32::from(bounds.origin.x) + f32::from(bounds.size.width) / 2.0;
            let center_y = f32::from(bounds.origin.y) + f32::from(bounds.size.height) / 2.0;
            let stroke = 2.25_f32;
            let radius = (size / 2.0 - stroke / 2.0).max(0.5);
            let radii = point(px(radius), px(radius));
            let top = point(px(center_x), px(center_y - radius));
            let bottom = point(px(center_x), px(center_y + radius));

            let mut track_path = PathBuilder::stroke(px(stroke));
            track_path.move_to(top);
            track_path.arc_to(radii, px(0.), false, true, bottom);
            track_path.arc_to(radii, px(0.), false, true, top);
            if let Ok(path) = track_path.build() {
                window.paint_path(path, track);
            }

            let fraction = (pct.min(100) as f32) / 100.0;
            if fraction <= 0.0 {
                return;
            }
            let mut progress = PathBuilder::stroke(px(stroke));
            progress.move_to(top);
            if fraction >= 0.999 {
                progress.arc_to(radii, px(0.), false, true, bottom);
                progress.arc_to(radii, px(0.), false, true, top);
            } else {
                let theta = fraction * std::f32::consts::TAU;
                progress.arc_to(
                    radii,
                    px(0.),
                    fraction > 0.5,
                    true,
                    point(
                        px(center_x + radius * theta.sin()),
                        px(center_y - radius * theta.cos()),
                    ),
                );
            }
            if let Ok(path) = progress.build() {
                window.paint_path(path, color);
            }
        },
    )
    .size(px(18.))
    .flex_shrink_0()
    .into_any_element()
}

fn format_cost(cost: f64) -> String {
    if cost >= 0.01 {
        format!("${cost:.2}")
    } else {
        format!("${cost:.4}")
    }
}

/// 用量平时不冒泡；75% 黄、90% 红。
fn usage_warn_color(pct: u32) -> Option<u32> {
    if pct >= 90 {
        Some(ui_theme::red())
    } else if pct >= 75 {
        Some(ui_theme::yellow())
    } else {
        None
    }
}

/// 会话面板要先露配置。模型一长，必须收进二级，否则贴底的菜单只看得见模型。
fn composer_should_nest_models(
    extra_config_count: usize,
    provider_count: usize,
    model_count: usize,
) -> bool {
    model_count > 1 && (extra_config_count > 0 || provider_count > 1)
}

/// 模型胶囊弹层的一个分区。`Config` 带的是 `extra_configs` 下标。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ComposerMenuSection {
    Provider,
    Model,
    Config(usize),
}

/// 胶囊上写的是当前模型名，点开就必须先看到 Provider 和模型；权限模式这类会话
/// 配置排在后面。只有一个 Provider/模型时也要如实列出（勾选、不可切），否则单模
/// 型场景弹出来只剩权限模式，看着就像点错了入口。
fn composer_menu_sections(
    provider_count: usize,
    model_count: usize,
    extra_config_count: usize,
) -> Vec<ComposerMenuSection> {
    let mut sections = Vec::with_capacity(2 + extra_config_count);
    if provider_count > 0 {
        sections.push(ComposerMenuSection::Provider);
    }
    if model_count > 0 {
        sections.push(ComposerMenuSection::Model);
    }
    sections.extend((0..extra_config_count).map(ComposerMenuSection::Config));
    sections
}

/// 运行中改配置只是排队到本轮结束；菜单标题要说清楚，避免当成已经生效。
fn composer_config_section_label(name: &str, pending: bool, next_turn_hint: bool) -> String {
    if pending && next_turn_hint {
        format!("{name} · 下轮生效")
    } else {
        name.to_string()
    }
}

fn pending_config_choice_names(
    pending: &[(String, String)],
    configs: &[SessionConfigState],
    model: Option<&ModelState>,
) -> Vec<String> {
    pending
        .iter()
        .filter_map(|(id, value)| {
            if let Some(model) = model
                && (id == &model.config_id || id == "model")
            {
                return model
                    .options
                    .iter()
                    .find(|(option, _)| option == value)
                    .map(|(_, name)| name.clone());
            }
            let config = configs.iter().find(|config| &config.config_id == id)?;
            let choice = config
                .options
                .iter()
                .find(|(option, _)| option == value)
                .map(|(_, name)| name.as_str())?;
            Some(if config.boolean.is_some() {
                format!("{} {choice}", config.name)
            } else {
                choice.to_string()
            })
        })
        .collect()
}

/// 输入栏常驻提示：点完菜单关掉后也能看见，不走 toast。
fn composer_next_turn_notice(names: &[String], turn_active: bool) -> Option<String> {
    if !turn_active {
        return None;
    }
    match names {
        [] => None,
        [name] => Some(format!("{name} · 下轮生效")),
        [first, second] => Some(format!("{first}、{second} · 下轮生效")),
        [first, ..] => Some(format!("{first} 等 {} 项 · 下轮生效", names.len())),
    }
}

/// 运行中输入框的快捷键说明：回车插当前回合，⌥Enter 等本轮结束再发。
fn composer_native_queue_shortcut_hint() -> &'static str {
    "Enter 插入当前回合 · ⌥Enter 回合后发送"
}

fn native_queue_item_kind_label(is_follow_up: bool) -> &'static str {
    if is_follow_up {
        "下一回合"
    } else {
        "当前回合"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeImmediateSendPlan {
    selected: String,
    leftovers: Vec<String>,
}

/// 原生排队条点「立即发送」：选中的那条改成新 prompt，其余还回输入框。
fn plan_native_immediate_send(
    steering: &[String],
    follow_up: &[String],
    index: usize,
) -> Option<NativeImmediateSendPlan> {
    let mut items: Vec<String> = steering.iter().chain(follow_up).cloned().collect();
    if index >= items.len() {
        return None;
    }
    let selected = items.remove(index);
    Some(NativeImmediateSendPlan {
        selected,
        leftovers: items,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComposerRestoreConsume {
    last_revision: u64,
    skip_next: bool,
    restore_texts: Option<Vec<String>>,
}

/// cancel 会把整队还回输入框。立即发送已经把选中条改成新 prompt，这次还原必须丢掉。
fn consume_composer_restore(
    last_revision: u64,
    incoming_revision: u64,
    incoming_texts: Vec<String>,
    skip_next: bool,
) -> ComposerRestoreConsume {
    if incoming_revision <= last_revision {
        return ComposerRestoreConsume {
            last_revision,
            skip_next,
            restore_texts: None,
        };
    }
    if skip_next {
        return ComposerRestoreConsume {
            last_revision: incoming_revision,
            skip_next: false,
            restore_texts: None,
        };
    }
    ComposerRestoreConsume {
        last_revision: incoming_revision,
        skip_next: false,
        restore_texts: (!incoming_texts.is_empty()).then_some(incoming_texts),
    }
}

fn native_queue_from_snapshot(
    skip_pending_immediate: bool,
    steering: Vec<String>,
    follow_up: Vec<String>,
) -> (Vec<String>, Vec<String>) {
    if skip_pending_immediate {
        (Vec::new(), Vec::new())
    } else {
        (steering, follow_up)
    }
}

fn session_config_current_value(config: &SessionConfigState) -> Option<&str> {
    config
        .options
        .iter()
        .find_map(|(value, name)| (name == &config.current_name).then_some(value.as_str()))
}

fn confirmed_config_value<'a>(
    config_id: &str,
    configs: &'a [SessionConfigState],
    model: Option<&'a ModelState>,
) -> Option<&'a str> {
    if let Some(model) = model
        && (config_id == model.config_id || config_id == "model")
        && !model.current_value.is_empty()
    {
        return Some(model.current_value.as_str());
    }
    configs
        .iter()
        .find(|config| config.config_id == config_id)
        .and_then(session_config_current_value)
}

fn upsert_session_config_value(
    values: &mut Vec<(String, String)>,
    config_id: String,
    value_id: String,
) {
    if let Some((_, current)) = values.iter_mut().find(|(id, _)| id == &config_id) {
        *current = value_id;
    } else {
        values.push((config_id, value_id));
    }
}

/// 握手完成前的恢复配置还没发出去。用户这时切模型，必须改这份待发列表，
/// 否则第一份 Idle 快照会把旧值再打回去，胶囊上看着换了、下一轮仍是原模型。
fn overlay_pending_initial_config(
    pending_initial: &mut Vec<(String, String)>,
    config_id: String,
    value_id: String,
) {
    if pending_initial.is_empty() {
        return;
    }
    upsert_session_config_value(pending_initial, config_id, value_id);
}

/// 用户刚点的配置：同一 id 只留最后一次；点回快照当前值等于取消 pending。
fn apply_pending_config_selection(
    pending: &mut Vec<(String, String)>,
    config_id: String,
    value_id: String,
    confirmed_value: Option<&str>,
) {
    if confirmed_value == Some(value_id.as_str()) {
        pending.retain(|(id, _)| id != &config_id);
        return;
    }
    if let Some((_, pending_value)) = pending
        .iter_mut()
        .find(|(pending_id, _)| pending_id == &config_id)
    {
        *pending_value = value_id;
    } else {
        pending.push((config_id, value_id));
    }
}

/// agent 回包对上了才清 pending；带着旧值的快照不能把勾选打回去。
fn reconcile_pending_config_values(
    pending: &mut Vec<(String, String)>,
    configs: &[SessionConfigState],
    model: Option<&ModelState>,
) {
    pending
        .retain(|(id, value)| confirmed_config_value(id, configs, model) != Some(value.as_str()));
}

fn config_update_failure_is_new(prev_status: Option<&str>, next_status: Option<&str>) -> bool {
    const PREFIX: &str = "更新会话配置失败";
    next_status.is_some_and(|status| status.starts_with(PREFIX)) && prev_status != next_status
}

fn config_selection_is_pending(pending: &[(String, String)], config_id: &str) -> bool {
    pending.iter().any(|(id, _)| id == config_id)
}

fn overlay_session_configs(
    configs: &[SessionConfigState],
    pending: &[(String, String)],
) -> Vec<SessionConfigState> {
    configs
        .iter()
        .map(|config| {
            let Some((_, value)) = pending.iter().find(|(id, _)| id == &config.config_id) else {
                return config.clone();
            };
            let Some((_, name)) = config.options.iter().find(|(option, _)| option == value) else {
                return config.clone();
            };
            let mut displayed = config.clone();
            displayed.current_name = name.clone();
            if let Some(flag) = displayed.boolean.as_mut() {
                *flag = value == "true";
            }
            displayed
        })
        .collect()
}

fn overlay_model_state(model: &ModelState, pending: &[(String, String)]) -> ModelState {
    let Some((_, value)) = pending
        .iter()
        .find(|(id, _)| id == &model.config_id || id == "model")
    else {
        return model.clone();
    };
    let Some((_, name)) = model.options.iter().find(|(option, _)| option == value) else {
        return model.clone();
    };
    let mut displayed = model.clone();
    displayed.current_value = value.clone();
    displayed.current_name = name.clone();
    displayed
}

fn selected_provider_group(model: &ModelState) -> Option<&ModelProviderGroup> {
    model
        .provider_groups
        .iter()
        .find(|group| {
            group
                .options
                .iter()
                .any(|(value, _)| value == &model.current_value)
        })
        .or_else(|| {
            model.provider_groups.iter().find(|group| {
                group
                    .options
                    .iter()
                    .any(|(_, name)| name == &model.current_name)
            })
        })
        .or_else(|| model.provider_groups.first())
}

fn provider_switch_value<'a>(
    provider: &'a ModelProviderGroup,
    current_model_name: &str,
) -> Option<&'a str> {
    provider
        .options
        .iter()
        .find(|(_, name)| name == current_model_name)
        .or_else(|| provider.options.first())
        .map(|(value, _)| value.as_str())
}

fn should_seed_restored_height_hints(
    awaiting_initial_snapshot: bool,
    replaying_history: bool,
    following_tail: bool,
    entries_changed: bool,
    old_entry_count: usize,
    new_entry_count: usize,
) -> bool {
    new_entry_count > 0
        && following_tail
        && ((awaiting_initial_snapshot && entries_changed)
            || (replaying_history && new_entry_count > old_entry_count))
}

fn loaded_entries_end(loaded_offset: usize, entries_len: usize) -> usize {
    loaded_offset.saturating_add(entries_len)
}

fn can_load_older_history(history_loading: bool, loaded_offset: usize) -> bool {
    !history_loading && loaded_offset > 0
}

fn can_dispatch_prompt_immediately(
    phase: &DaemonPhase,
    prompt_dispatch_pending: bool,
    queue_is_empty: bool,
    has_handle: bool,
) -> bool {
    matches!(phase, DaemonPhase::Idle) && !prompt_dispatch_pending && queue_is_empty && has_handle
}

fn is_recovered_phase(phase: &DaemonPhase) -> bool {
    matches!(
        phase,
        DaemonPhase::Idle
            | DaemonPhase::Thinking
            | DaemonPhase::AwaitingApproval
            | DaemonPhase::WaitingForUser
    )
}

fn did_recover_from_ended(was_ended: bool, phase: &DaemonPhase) -> bool {
    was_ended && is_recovered_phase(phase)
}

fn is_new_conversation_command(text: &str) -> bool {
    text.trim().eq_ignore_ascii_case("/clear")
}

/// 新建的空白会话可以静默准备：输入框已经可用，用户无需先等 ACP 握手完成。
/// 续接历史、自动交接和已有消息的会话仍展示启动状态，避免隐藏实际的恢复工作。
fn is_fresh_conversation_start(
    phase: &DaemonPhase,
    entries_are_empty: bool,
    has_history_session: bool,
    has_initial_prompt: bool,
) -> bool {
    matches!(phase, DaemonPhase::Connecting)
        && entries_are_empty
        && !has_history_session
        && !has_initial_prompt
}

fn should_show_starting_placeholder(
    phase: &DaemonPhase,
    entries_are_empty: bool,
    has_history_session: bool,
    has_initial_prompt: bool,
) -> bool {
    matches!(phase, DaemonPhase::Connecting)
        && entries_are_empty
        && !is_fresh_conversation_start(
            phase,
            entries_are_empty,
            has_history_session,
            has_initial_prompt,
        )
}

/// 真正的新会话（或刚完成握手、尚未产生消息的新会话）才展示快捷起点。
/// 续接历史、自动交接、已排队的消息与运行中的会话各有自己的状态反馈，不应被
/// 这个引导盖住。输入框里的草稿还没发出去，不算一轮对话，快捷起点继续留着。
fn should_show_empty_conversation_state(
    phase: &DaemonPhase,
    entries_are_empty: bool,
    has_history_session: bool,
    has_initial_prompt: bool,
    queue_is_empty: bool,
    prompt_dispatch_pending: bool,
) -> bool {
    if !entries_are_empty || has_initial_prompt || !queue_is_empty || prompt_dispatch_pending {
        return false;
    }

    matches!(phase, DaemonPhase::Idle)
        || is_fresh_conversation_start(
            phase,
            entries_are_empty,
            has_history_session,
            has_initial_prompt,
        )
}

fn starting_status_copy(
    status_line: Option<&str>,
    is_resuming: bool,
    agent_label: &str,
    waited_seconds: u64,
) -> (String, String, String) {
    let title = if is_resuming {
        "正在恢复上次的会话".to_string()
    } else {
        format!("正在启动 {agent_label}")
    };
    let detail = status_line
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if is_resuming {
                "正在恢复历史消息和工作上下文".to_string()
            } else {
                "正在建立与 agent 的连接".to_string()
            }
        });
    let elapsed = format!("已等待 {waited_seconds} 秒");

    (title, detail, elapsed)
}

fn move_queue_item_to_front<T>(queue: &mut std::collections::VecDeque<T>, index: usize) -> bool {
    let Some(item) = queue.remove(index) else {
        return false;
    };
    queue.push_front(item);
    true
}

fn should_cancel_for_immediate_prompt(phase: &DaemonPhase, prompt_dispatch_pending: bool) -> bool {
    matches!(
        phase,
        DaemonPhase::Thinking
            | DaemonPhase::ExecutingTool
            | DaemonPhase::AwaitingApproval
            | DaemonPhase::WaitingForUser
    ) || (prompt_dispatch_pending && matches!(phase, DaemonPhase::Idle))
}

fn is_dispatch_in_flight(prompt_dispatch_pending: bool, immediate_cancel_pending: bool) -> bool {
    prompt_dispatch_pending || immediate_cancel_pending
}

fn conversation_phase_label(
    phase: &DaemonPhase,
    turn_outcome: Option<AcpTurnOutcome>,
    prompt_dispatch_pending: bool,
    immediate_cancel_pending: bool,
    fresh_start: bool,
    fresh_start_pending: bool,
) -> (&'static str, u32) {
    if is_dispatch_in_flight(prompt_dispatch_pending, immediate_cancel_pending)
        && !matches!(phase, DaemonPhase::Dead | DaemonPhase::Connecting)
    {
        return ("运行中", ui_theme::blue());
    }
    match phase {
        DaemonPhase::Connecting if fresh_start_pending => ("运行中", ui_theme::blue()),
        DaemonPhase::Connecting if fresh_start => ("新对话", ui_theme::text_faint()),
        DaemonPhase::Connecting => ("启动中", ui_theme::blue()),
        DaemonPhase::Idle => match turn_outcome {
            Some(AcpTurnOutcome::Succeeded) => ("已完成", ui_theme::green()),
            Some(AcpTurnOutcome::Cancelled) => ("已停止", ui_theme::text_faint()),
            Some(outcome) if outcome.failure_message().is_some() => ("失败", ui_theme::red()),
            _ => ("空闲", ui_theme::text_faint()),
        },
        DaemonPhase::Thinking | DaemonPhase::ExecutingTool => ("运行中", ui_theme::blue()),
        DaemonPhase::AwaitingApproval => ("等你批准", ui_theme::yellow()),
        DaemonPhase::WaitingForUser => ("等你选择", ui_theme::accent()),
        DaemonPhase::Succeeded => ("已完成", ui_theme::green()),
        DaemonPhase::Failed => ("失败", ui_theme::red()),
        DaemonPhase::Dead => ("已结束", ui_theme::text_faint()),
    }
}

/// 快照到达后本地 prompt 闸门怎么走。立即发送会先 cancel 再等 Idle 派发队首；
/// 若把「任意 Idle」当成取消完成，上一条尚未确认的 dispatch 会把队首永远卡住。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SnapshotPromptGate {
    prompt_dispatch_pending: bool,
    immediate_cancel_pending: bool,
    should_flush_queue: bool,
}

fn next_snapshot_prompt_gate(
    phase: &DaemonPhase,
    turn_started_at_ms: Option<u64>,
    prompt_dispatch_pending: bool,
    immediate_cancel_pending: bool,
    queue_is_empty: bool,
    turn_outcome: Option<AcpTurnOutcome>,
) -> SnapshotPromptGate {
    let ended = matches!(phase, DaemonPhase::Dead);
    let idle = matches!(phase, DaemonPhase::Idle);
    // Running 确认、会话结束，或「立即发送」已经把当前回合取消到 Idle：
    // 上一条 dispatch 不会再等到 Running 回执。
    // 失败/取消的 Idle 也必须放开：否则 Stop 一直亮，回车被当成还在跑。
    // 成功 Idle 不能清——那是防连续提交并发的闸门。
    let failed_or_cancelled = idle
        && turn_outcome.is_some_and(|outcome| {
            outcome.failure_message().is_some() || matches!(outcome, AcpTurnOutcome::Cancelled)
        });
    let prompt_dispatch_pending = if turn_started_at_ms.is_some()
        || ended
        || failed_or_cancelled
        || (immediate_cancel_pending && idle)
    {
        false
    } else {
        prompt_dispatch_pending
    };
    let should_flush_queue = idle && !prompt_dispatch_pending && !queue_is_empty;
    // 取消意图保留到真正能派发（或已经无队）为止。
    let immediate_cancel_pending =
        immediate_cancel_pending && !ended && !(idle && !prompt_dispatch_pending);

    SnapshotPromptGate {
        prompt_dispatch_pending,
        immediate_cancel_pending,
        should_flush_queue,
    }
}

/// smeltd 会为每份实时快照分配单调递增版本。版本为 0 的是旧 daemon 或本地
/// 断线兜底快照，仍需接受；其余过期/重复快照不能把较新的 Running 倒退成 Idle。
fn should_apply_snapshot_revision(last_applied: u64, incoming: u64) -> bool {
    incoming == 0 || incoming > last_applied
}

fn should_replace_session_title(incoming_has_title: bool, entries_changed: bool) -> bool {
    incoming_has_title || entries_changed
}

fn is_stale_blank_history_id(
    entries_are_empty: bool,
    history_session_id: Option<&SessionId>,
    acp_session_id: Option<&SessionId>,
) -> bool {
    entries_are_empty && history_session_id.is_some() && history_session_id == acp_session_id
}

fn should_clear_history_session_id_after_snapshot(
    phase: &DaemonPhase,
    entries_are_empty: bool,
    snapshot_revision: u64,
    has_runtime_session_id: bool,
) -> bool {
    entries_are_empty
        && snapshot_revision != 0
        && has_runtime_session_id
        && matches!(phase, DaemonPhase::Idle)
}

fn merge_snapshot_entries(
    entries: &mut Vec<AcpEntry>,
    loaded_offset: &mut usize,
    entries_total: &mut usize,
    incoming_offset: usize,
    incoming_total: usize,
    incoming: Vec<AcpEntry>,
    initial: bool,
) -> Option<usize> {
    let incoming_end = incoming_offset.saturating_add(incoming.len());
    let incoming_total = incoming_total.max(incoming_end);
    if initial {
        *entries = incoming;
        *loaded_offset = incoming_offset;
        *entries_total = incoming_total;
        return Some(0);
    }

    let loaded_end = loaded_offset.saturating_add(entries.len());
    if incoming_total < *loaded_offset {
        // `session/load` restarted from an empty history while this GUI still held an old tail.
        *entries = incoming;
        *loaded_offset = incoming_offset;
        *entries_total = incoming_total;
        return Some(0);
    }
    *entries_total = incoming_total;
    if incoming_offset == loaded_end && incoming.is_empty() {
        return None;
    }

    if incoming_offset >= *loaded_offset && incoming_offset <= loaded_end {
        let local_offset = incoming_offset - *loaded_offset;
        entries.truncate(local_offset);
        entries.extend(incoming);
        return Some(local_offset);
    }
    if incoming_offset < *loaded_offset && incoming_end >= *loaded_offset {
        let overlap = incoming_end - *loaded_offset;
        let suffix_start = overlap.min(entries.len());
        let mut merged = incoming;
        merged.extend(entries.drain(suffix_start..));
        *entries = merged;
        *loaded_offset = incoming_offset;
        return Some(0);
    }
    if incoming_offset > loaded_end {
        // A missed stream update created a gap. Keep the newest contiguous suffix and let upward
        // pagination fill older entries rather than displaying mismatched local/global indices.
        *entries = incoming;
        *loaded_offset = incoming_offset;
        return Some(0);
    }
    None
}

#[derive(Clone)]
struct CachedDiff {
    lines: std::rc::Rc<Vec<DiffLine>>,
    added: usize,
    removed: usize,
}

/// `@` / `/` 补全弹层的状态。回合态，不落盘。
struct CompletionPopup {
    /// 触发 token 在输入框文本里的字节范围（含 `@`/`/`），接受候选时按它替换。
    start: usize,
    end: usize,
    items: Vec<smelt_ui::acp_completion::Candidate>,
    selected: usize,
}

struct PendingConversationInput {
    input: smelt_core::conversation::ConversationInput,
    text: String,
    images: Vec<std::sync::Arc<gpui::Image>>,
    snapshot_revision: u64,
}

fn restorable_gui_prompt(
    prompt: Option<String>,
    legacy_delivery_id: Option<&str>,
) -> Option<String> {
    if legacy_delivery_id.is_some() {
        return None;
    }
    prompt.filter(|text| !text.trim().is_empty())
}

pub enum AcpViewEvent {
    Changed,
    /// 用户在输入栏配置菜单中显式选择了一项。上层据此按 agent 维度持久化；
    /// 自动恢复、controller 注入等内部配置不会发这个事件，避免污染交互默认值。
    ConfigSelected {
        config_id: String,
        value_id: String,
    },
    PreviewImage(std::sync::Arc<gpui::Image>),
    NewSession(Box<AcpNewSessionRequest>),
    /// Pi 活体最终回答上的分叉：新进程 `--fork` 源 session。
    ForkConversation(Box<AcpHandoffRequest>),
    NavigateToSession(String),
    /// 回合结束且无人在等（无 pending_permissions / pending_elicitation）的上升沿。
    /// GUI 用它刷新会话展示；投递完成归约由 smeltd 直接观察 ACP 快照完成。
    CompletedTurn {
        delivery_id: Option<String>,
    },
    /// ACP 正常连接内的回合失败（限额、拒绝或协议失败），与连接 Ended 分开。
    FailedTurn {
        reason: String,
        delivery_id: Option<String>,
    },
    /// 连接结束（Dead）的上升沿。控制流使用稳定分类，reason 只负责展示。
    Ended {
        kind: AcpEndKind,
        reason: String,
        delivery_id: Option<String>,
    },
    /// 会话从 Ended 恢复（自动重连 / 用户手动重启 / GUI 重开 attach）的上升沿。
    /// 只用于刷新视图；Task 状态由 smeltd 直接观察同一 ACP 快照归约。
    Recovered,
}

fn task_body_from_selection(selection: String) -> Option<String> {
    (!selection.trim().is_empty()).then_some(selection)
}

fn append_prompt_text(current: &str, text: &str) -> (String, usize) {
    let merged = if current.trim().is_empty() {
        format!("{text} ")
    } else if current.ends_with(' ') {
        format!("{current}{text} ")
    } else {
        format!("{current} {text} ")
    };
    let cursor_offset = merged.len();
    (merged, cursor_offset)
}

fn merge_rejected_prompt(current: &str, rejected: &str) -> String {
    if current.trim().is_empty() {
        rejected.to_string()
    } else if rejected.trim().is_empty() {
        current.to_string()
    } else {
        format!("{rejected}\n\n{current}")
    }
}

fn conversation_input_for_submit(
    text: String,
    images: Vec<AcpImage>,
    uncertain: Option<&smelt_core::conversation::ConversationInput>,
) -> smelt_core::conversation::ConversationInput {
    uncertain
        .filter(|previous| previous.text == text && previous.images == images)
        .cloned()
        .unwrap_or_else(|| smelt_core::conversation::ConversationInput::new(text, images))
}

fn selected_text_context_menu(
    menu: PopupMenu,
    _view: Entity<AcpView>,
    _cwd: Option<String>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(body) = task_body_from_selection(gpui_base::TextSelection::selected_text(window, cx))
    else {
        return menu.item(PopupMenuItem::label("请先选中文本"));
    };
    let copied_body = body;

    menu.item(
        PopupMenuItem::new("复制").on_click(move |_event, _window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copied_body.clone()));
        }),
    )
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AcpForkOrigin {
    pub session_id: String,
    pub title: String,
    /// 源会话是哪家 agent（`ConversationAgentKind::id()`）。旧存档没有这个字段 → `None`，
    /// 一律按「同一家」显示，不会因为缺字段就把老会话标成迁移来的。
    #[serde(default)]
    pub agent: Option<String>,
    /// 源会话的 workspace profile 名（默认 workspace 为 `None`）。只用于显示，
    /// profile 可能已被删掉，不拿它反查配置。
    #[serde(default)]
    pub profile_label: Option<String>,
    /// 源是历史会话页上的磁盘存档（不是当前开着的 Smelt 会话）。此时
    /// `session_id` 是源 agent 自己的 session id，Smelt 这边按它找不到会话，
    /// 顶栏不提供「返回原会话」。
    #[serde(default)]
    pub from_history: bool,
}

/// 历史会话页「迁移到」的目标：把这段对话交给谁。
#[derive(Clone)]
pub struct AcpHandoffTarget {
    pub agent: ConversationAgentKind,
    pub launch: ConversationLaunchSpec,
    pub profile_id: Option<String>,
    /// profile 的显示名；基础 agent 槽位为 `None`。
    pub profile_label: Option<String>,
}

#[derive(Clone)]
pub struct AcpHandoffRequest {
    /// 交接来源。`None` = 不是从另一个会话 fork 的（任务开跑等无来源场景）。
    pub source: Option<AcpForkOrigin>,
    pub cwd: Option<String>,
    pub agent: ConversationAgentKind,
    pub launch: ConversationLaunchSpec,
    pub refresh_launch_from_settings: bool,
    pub profile_id: Option<String>,
    /// 首包前应用的 provider 配置。
    pub config_values: Vec<(String, String)>,
    /// 本次 ACP 子进程专用环境变量；不能放进 `launch`，否则会被普通会话存档持久化。
    pub ephemeral_env: std::collections::BTreeMap<String, String>,
    pub prompt: String,
    /// 随首包一起发出去的图片。空 = 纯文本首包。
    pub images: Vec<smelt_core::acp_chat::AcpImage>,
    /// 目标 profile 的显示名，跟 `source.profile_label` 一起决定会话标题怎么写。
    pub profile_label: Option<String>,
    /// 前次 provider 会话 id（controller 续跑上下文）：非空时 ACP 连接用
    /// `session/load` 恢复历史，而不是开全新会话。
    pub resume_session_id: Option<String>,
    /// Pi 原生 `--fork` 的源 session id。有值时新进程复制源 session 文件，
    /// 不要再注入交接摘要 prompt。
    pub fork_session_id: Option<String>,
    /// 与 `fork_session_id` 搭配的分叉切点：副本打开后、重放前切到该用户消息
    /// 之前（不含它），新会话恰好包含到被点击的那条回答为止。`None` = 整份
    /// 拷贝（点的是最后一回合，无需切）。
    pub fork_cut: Option<smelt_core::acp_conn::AcpForkCut>,
    /// 目标会话的交互输入路由。与 agent/profile 正交；Direct 是普通本地对话。
    pub conversation_binding: smelt_core::conversation::ConversationBinding,
    /// 产品级智能体会话身份。它决定插件提供的名称、图标和生命周期 UI，
    /// 与执行这条会话的 ACP provider 相互独立。
    pub agent_session: Option<smelt_plugin_api::AgentSessionBinding>,
}

#[derive(Clone)]
pub struct AcpNewSessionRequest {
    pub agent: ConversationAgentKind,
    pub launch: ConversationLaunchSpec,
    pub profile_id: Option<String>,
    pub cwd: Option<String>,
}

/// 冷启动占位视图的初始状态：agent 身份、启动规格与恢复来源。把 `placeholder`
/// 的参数收成一组，避免 10 个参数逐位传递。
pub struct AcpViewOrigin {
    pub agent: ConversationAgentKind,
    pub launch: ConversationLaunchSpec,
    pub refresh_launch_from_settings: bool,
    pub profile_id: Option<String>,
    pub cwd: Option<String>,
    pub reason: String,
    pub entries: Vec<AcpEntry>,
    pub resume_session_id: Option<SessionId>,
    pub saved_sid: Option<String>,
}

impl EventEmitter<AcpViewEvent> for AcpView {}

pub struct AcpView {
    sid: String,
    cwd: Option<String>,
    entries: Vec<AcpEntry>,
    permissions: Vec<PendingPermission>,
    /// 已发出回执、等待 smeltd 快照确认的队首审批。期间按钮替换成处理中状态，
    /// 防止网络往返时重复点击；队首变化后清空。
    permission_submitting: Option<(String, String)>,
    elicitation: Option<PendingElicitation>,
    /// 自由文本 elicitation 的本地编辑器；协议状态只保存字符串，不持有 GPUI 实体。
    elicitation_inputs: std::collections::HashMap<usize, Entity<InputState>>,
    phase: DaemonPhase,
    /// `phase == Dead` 时的展示文案。
    end_reason: String,
    /// `phase == Dead` 时的机器可读分类；不能从展示文案反推。
    end_kind: AcpEndKind,
    /// daemon 受理/执行/完成的稳定外部投递身份，随快照恢复。
    accepted_delivery_ids: std::collections::BTreeSet<String>,
    active_delivery_id: Option<String>,
    completed_delivery_id: Option<String>,
    /// 启动阶段的进度文案（下载运行时等），Starting 横幅显示。
    status_line: Option<String>,
    /// None = 已结束的占位视图（重开后才建；Ended 态没有输入框）。
    input: Option<Entity<TextareaState>>,
    /// 输入框是否已有文字草稿；空会话引导随草稿隐藏，清空后再出现。
    input_has_draft: bool,
    /// smeltd 连接句柄——`None` 只在真正的冷恢复占位（没连过）出现；只要连过
    /// 一次就一直持有到视图销毁，Drop 时只会断开 socket（见 `ConversationClientHandle`
    /// 文件头注释），不影响 smeltd 那边的会话存活。
    handle: Option<ConversationClientHandle>,
    /// 每次换掉 ACP socket 都递增。旧 socket 的断线兜底快照不能覆盖新连接。
    snapshot_stream_generation: u64,
    /// 当前 socket 已应用的最高 daemon 快照版本，0 表示尚未收到带版本的快照。
    last_snapshot_revision: u64,
    /// 守护侧断连（smeltd 升级/重启）后自动重连的剩余预算。配合指数退避重试
    /// （main.rs 里延迟 500ms 起、翻倍到 8s 上限），每次自动重连递减，收到活跃
    /// 快照（连接成功）回满；耗尽则停止自动重连，等用户手动。
    auto_reconnect_left: u32,
    /// 重启用的启动规格（placeholder / restart 共用）。
    launch: ConversationLaunchSpec,
    /// 只在本进程内保留的启动环境变量；强制重启时也需要复用，但绝不持久化。
    ephemeral_env: std::collections::BTreeMap<String, String>,
    /// true = 普通会话重启时按当前设置刷新命令；false = 保留持久化下来的 launch。
    refresh_launch_from_settings: bool,
    /// workspace profile 的稳定 id；普通 agent 会话为 None。
    profile_id: Option<String>,
    /// 这条会话接的是哪个 agent（Claude / Copilot / Codex）：决定显示名，也决定
    /// 「重新开始」时该去全局配置的哪一条命令上取最新值。
    agent: ConversationAgentKind,
    /// 已粘进来、等着随下一条 prompt 发出去的图片（缩略图条显示，发完清空）。
    /// 只在内存里待到发送为止：图片体积大，不进 workspace.json。
    pending_images: Vec<std::sync::Arc<gpui::Image>>,
    /// macOS 剪贴板会优先把 CleanShot 这类图片暴露为文件路径。文件在
    /// 后台读取时阻止提交，避免用户立即回车只发出了文字。
    pending_external_image_loads: usize,
    /// ACP 同一 session 一次只能运行一个 turn。新建空白会话的首条 prompt 会直接
    /// 交给 smeltd（它在握手完成后发送）；运行中或上一条 prompt 尚未确认时，
    /// 新消息仍先排队，等 Idle 后按顺序发送。「下一条发送」只调整顺序；
    /// 用户明确选择「立即发送」时才会中断当前回合。
    queued_prompts: std::collections::VecDeque<(String, Vec<std::sync::Arc<gpui::Image>>)>,
    /// action 已交给 smeltd，但 Running 快照尚未回来。防止快点两次时本地 phase
    /// 仍是 Idle，第二条 prompt 越过队列直接并发发送。
    prompt_dispatch_pending: bool,
    /// 用户明确要求「立即发送」后，已请求停止当前 turn，等待 Idle 再派发队首。
    immediate_cancel_pending: bool,
    /// 已发送消息的解码图片缓存，避免流式输出或 spinner 重绘时反复解码 base64。
    rendered_images: std::collections::HashMap<(usize, usize), std::sync::Arc<gpui::Image>>,
    /// Edit diff 的算法结果与紧凑预览，避免卡片展开后的每次重绘都重新计算。
    rendered_diffs: std::collections::HashMap<String, Vec<Option<CachedDiff>>>,
    /// 工具输出里图片块的解码缓存，与 `rendered_diffs` 同构（按 tool id，下标对齐
    /// `output`）。流式重绘每帧都会重建元素，不能每帧重跑一次 base64 解码。
    rendered_tool_images:
        std::collections::HashMap<String, Vec<Option<std::sync::Arc<gpui::Image>>>>,
    /// 与 `entries` 同索引的 Markdown 预处理结果。文件链接解析与用户 HTML 转义只在
    /// entry 变化时执行，静态重绘直接复用 `SharedString`。
    rendered_markdown: Vec<Option<gpui::SharedString>>,
    /// 当前本地 `entries[0]` 在 smeltd 完整历史中的全局下标，以及服务端总条数。
    /// GUI 首次 attach 只取尾页，实时增量仍使用服务端全局 offset，因此合并时必须
    /// 显式换算，不能再假设本地 Vec 从全局 0 开始。
    loaded_entries_offset: usize,
    entries_total: usize,
    history_loading: bool,
    /// 分页尾页可能不含首条用户消息，标题由快照单独携带。
    session_title: Option<String>,
    /// 本会话的 agent 是否收图（握手 Ready 带来）。握手前默认 true——那时还没
    /// 粘图的机会，先假设支持，Ready 到了再按实际能力修正（Grok = false）。
    supports_image: bool,
    /// 图片粘贴的一次性状态/错误提示，输入框上方显示一行。
    paste_hint: Option<String>,
    /// `@` / `/` 补全弹层的当前状态；None = 没在补全。
    completion: Option<CompletionPopup>,
    /// 补全候选列表的滚动位置；键盘移动选中项时同步保证其可见。
    completion_scroll: ScrollHandle,
    /// cwd 下的文件清单缓存（`@` 的候选源）。每敲一个字符跑一次 git ls-files
    /// 会明显卡手，所以一次会话只列一次。
    file_cache: Option<std::rc::Rc<Vec<String>>>,
    /// 首次列文件的后台任务是否在途：git ls-files + 目录遍历可能耗时，
    /// 一律后台跑，避免事件回调里同步起子进程卡 UI。
    file_list_loading: bool,
    /// 当前 ACP 连接使用的运行时 session id，仅用于展示连接状态。
    acp_session_id: Option<SessionId>,
    /// agent 历史存储中的 canonical id。持久化和 `session/load` 只使用它；
    /// runtime 连接重新建立后返回的新 id 不能覆盖它。
    history_session_id: Option<SessionId>,
    /// 会话当前可用的斜杠命令 (名字, 说明)；空 = agent 没发过这个更新。
    /// 胶囊点开列出来、点一条填进输入框——只显示数量没有任何用处。
    available_commands: Vec<(String, String)>,
    /// 上下文用量：(已用 token, 窗口大小)。None = agent 没上报过，不显示。
    usage: Option<(u64, u64)>,
    usage_cached_read: Option<u64>,
    usage_cost: Option<f64>,
    usage_breakdown: Option<smelt_core::acp_conn::ContextUsageBreakdown>,
    /// 底栏用量圆环点开的 Context Usage 面板。只属于本地浏览状态。
    usage_popover_open: bool,
    /// 独立 Trajectory 窗口。已打开则前置，不重复开。
    trajectory_window: Option<WindowHandle<trajectory::TrajectoryWindow>>,
    supports_compaction: bool,
    supports_native_queue: bool,
    /// 驱动是否支持回退到历史消息重发（Pi 的 fork）。用户气泡上的
    /// 「回到这里重发」按钮据此显隐。
    supports_rewind: bool,
    compacting: bool,
    queued_steering: Vec<String>,
    queued_follow_up: Vec<String>,
    last_composer_restore_revision: u64,
    pending_composer_restore: Option<Vec<String>>,
    /// 原生排队「立即发送」已把选中条改成新 prompt。随后 cancel 的 ComposerRestore
    /// 不能再把同一条还进输入框，否则会和即将发出的 prompt 重复。
    skip_next_composer_restore: bool,
    /// 非新建会话启动/续接的起点，用来在横幅上报「已等了几秒」。
    /// 实测 `session/new` 里 Claude Code 自身要约 10 秒（跟下载无关，同一适配器
    /// 进程建第二个会话一样慢），没有进度反馈会让人以为卡死了。
    starting_since: Option<std::time::Instant>,
    /// agent 最近一次上报的任务计划（每次全量覆盖）。回合态：不落盘，
    /// TurnEnded 保留最后一份供回看，「重新开始」清空。
    plan: Option<PlanView>,
    /// PLAN 条折叠态（默认展开，跟设计稿一致）。
    plan_collapsed: bool,
    /// 用户手动展开的过程/分析摘要（key = entries 索引）。过程组外的思考、
    /// 以及过程组里对用户说的中间正文共用这份状态；只属于当前视图。
    expanded_thoughts: std::collections::HashSet<usize>,
    /// 手动展开的回合执行过程（key = 该组第一条 entry 的索引）。默认整组收起。
    /// 展开后工具、思考和对用户说的中间正文都留在时间线上。
    expanded_process_groups: std::collections::HashSet<usize>,
    /// 同类紧凑工具合并行是否撑开（key = 该段第一条 entry 的索引）。
    /// 分组本身仍内联展开；单条工具的输出走 popover，不占时间线高度。
    expanded_tool_runs: std::collections::HashSet<usize>,
    /// 模型状态：当前名 + 可切换的候选（协议给什么显示什么）；None = agent
    /// 没上报过，UI 就不显示模型胶囊，不拿适配器包名冒充。
    model: Option<ModelState>,
    /// 除模型以外的 ACP 会话配置。agent 未上报则不显示。
    config_options: Vec<SessionConfigState>,
    /// 用户已点选、agent 快照尚未确认的配置。与 `restart_config_values` 分开：
    /// 那份要跨重启，这份只覆盖当前勾选，旧快照不能把它打回去。
    pending_config_values: Vec<(String, String)>,
    /// 当前回合开始时间，由 smeltd 计时后随快照同步。None = 没有运行中的回合。
    turn_started_at_ms: Option<u64>,
    turn_timings: Vec<smelt_core::acp_session::TurnTiming>,
    /// 运行中每秒重绘「已用 Ns」；回合结束后关掉。
    running_tick: bool,
    /// 最近一轮的真实终结结果；Idle 只表示当前没有运行中的回合。
    turn_outcome: Option<AcpTurnOutcome>,
    /// 上一份快照的 `completed_unread` 与相位是否已处于 Ended：用于检测完成/失败
    /// 上升沿（绑定任务的边沿），避免每个 Idle 快照都重复触发。
    was_completed_unread: bool,
    was_ended: bool,
    /// 手动展开了完整输出的工具调用（key = tool_call_id）。长输出默认折叠成
    /// 前几行 + 「展开」，回合态不落盘。
    expanded_tools: std::collections::HashSet<String>,
    /// 手动展开 / 收起了整个工具卡片（key = tool_call_id）。有可展示输出的卡片
    /// 默认收起；没有输出的卡片静态展示。用户点过后按这两组覆盖默认值。只属于
    /// 本地浏览状态，不落盘。
    expanded_tool_cards: std::collections::HashSet<String>,
    collapsed_tool_cards: std::collections::HashSet<String>,
    /// 可变高度消息虚拟列表：只测量和构建视口附近的 Markdown/工具卡。
    list_state: ListState,
    /// 占位视图尚未收到恢复后的首份非空历史。冷 attach 的完整快照要先给未测量
    /// 条目高度提示，避免滚动范围为零。`session/load` 回放则只信快照里的
    /// `replaying_history`。
    awaiting_initial_history_snapshot: bool,
    /// 用户是否已滚离消息尾部；由 ListScrollEvent 更新。
    viewing_history: bool,
    /// 「强制重启」请求正在路上：用于阻止侧栏菜单重复触发。跟
    /// `DaemonPhase::Connecting` 不是一回事——那是新进程握手阶段，这个是"旧进程
    /// 还没确认死透"的过渡态，两者可能重叠（发出 acp_restart 到收到新一份
    /// Connecting 快照之间有个网络往返）。
    restarting: bool,
    /// 「强制重启」失败时的提示文案（连不上 smeltd、会话已不存在等）；下次
    /// 操作前一直显示，成功后清空。只属于本地展示状态，不落盘。
    restart_error: Option<String>,
    /// 冷恢复占位待自动启动：GUI 重启后第一次切到这个会话时自动 restart，
    /// 有旧 session id 则协议级续接，没有则新建一轮但保留本地历史。只消费一次——
    /// 自动启动失败（Fatal → Ended）后回到手动，错误得让人看见，不能循环重试。
    auto_resume_pending: bool,
    /// ACP 没有 fork 原语。新会话握手进入 Idle 后，自动发送一次精简交接提示。
    pending_initial_prompt: Option<String>,
    /// 来源会话当前选择的模型、权限等 ACP 配置。必须先于交接提示写入新会话。
    pending_initial_config: Vec<(String, String)>,
    /// 需要在当前 view 重启时重新应用的初始配置。无人值守会话的全权限 mode 不是
    /// 启动命令参数（Claude/Codex 由 adapter session config 控制），普通会话的
    /// 用户选择也需要在重启后恢复，因此不能在 `pending_initial_config` 被消费后
    /// 丢掉。
    restart_config_values: Vec<(String, String)>,
    /// “在新会话中继续”的来源，只用于导航和解释会话关系。
    fork_origin: Option<AcpForkOrigin>,
    /// daemon 拥有的通用交互输入路由镜像。插件上下文只存在 route.context 中，
    /// ACP view 不认识任何具体远端平台的字段。
    conversation_binding: smelt_core::conversation::ConversationBinding,
    /// 产品级智能体与其 session controller 的通用身份。侧栏和未来的会话 UI
    /// contribution 只读这份事实，不从输入路由或平台上下文反推。
    agent_session: Option<smelt_plugin_api::AgentSessionBinding>,
    /// 独立 `acp_submit_input` 请求尚未回执。保留原始草稿和图片，拒绝或网络
    /// 结果不明时恢复到 composer，绝不自动换路由重发。
    pending_conversation_input: Option<PendingConversationInput>,
    /// 上次提交越过远端副作用边界后结果未知。用户原样重试时复用 submission id，
    /// 由插件持久化账本阻止重复；内容有任何修改则视为一条新的明确提交。
    uncertain_conversation_input: Option<smelt_core::conversation::ConversationInput>,
    /// daemon 持有的“下一条交互输入”预设镜像。这里只用于 workspace 恢复；
    /// 真正合并和成功后消费都在 daemon，保证桌面与移动端一致。
    pending_agent_preset: Option<String>,
    focus_handle: FocusHandle,
    _input_sub: Option<gpui::Subscription>,
}

pub(crate) fn resolve_restart_launch(
    current_launch: &ConversationLaunchSpec,
    profile_id: Option<&str>,
    config: &smelt_ui::agent_host_state::AgentHostState,
    agent: ConversationAgentKind,
    refresh_launch_from_settings: bool,
) -> ConversationLaunchSpec {
    let mut launch = if let Some(profile_id) = profile_id {
        config
            .find_profile(profile_id)
            .and_then(|profile| config.profile_launch_spec(profile).ok())
            .unwrap_or_else(|| current_launch.clone())
    } else if refresh_launch_from_settings {
        config.acp_launch_for(agent)
    } else {
        current_launch.clone()
    };
    // Product-agent instructions are launch identity, not a mutable provider preference. A
    // settings refresh may replace command/env defaults, but it must not turn the session into a
    // plain provider conversation when a fresh runtime is required.
    if let Some(instructions) = current_launch
        .env
        .get(smelt_core::agent_kind::SMELT_AGENT_INSTRUCTIONS_ENV)
    {
        launch.env.insert(
            smelt_core::agent_kind::SMELT_AGENT_INSTRUCTIONS_ENV.to_string(),
            instructions.clone(),
        );
    }
    launch
}

impl AcpView {
    /// 建视图并立即向 smeltd 发起 `acp_open`（非阻塞，握手结果以快照回来）。
    pub fn start(
        window: &mut Window,
        cx: &mut Context<Self>,
        agent: ConversationAgentKind,
        launch: ConversationLaunchSpec,
        profile_id: Option<String>,
        cwd: Option<String>,
        pending_agent_preset: Option<String>,
    ) -> Self {
        let mut this = Self::placeholder(
            cx,
            AcpViewOrigin {
                agent,
                launch,
                refresh_launch_from_settings: profile_id.is_none(),
                profile_id,
                cwd,
                reason: String::new(),
                entries: Vec::new(),
                resume_session_id: None,
                saved_sid: None,
            },
        );
        this.awaiting_initial_history_snapshot = false;
        this.phase = DaemonPhase::Connecting;
        this.end_reason.clear();
        this.starting_since = Some(std::time::Instant::now());
        this.pending_agent_preset = pending_agent_preset;
        this.init_input(window, cx);
        let handle = spawn_acp_client(ConversationClientLaunch {
            id: this.sid.clone(),
            cwd: this.cwd.clone(),
            launch: this.launch.clone(),
            engine_kind: agent,
            ephemeral_env: Default::default(),
            resume_id: None, // 第一次开，没有旧会话可续
            fork_id: None,
            fork_cut: None,
            retained_entries_end: loaded_entries_end(
                this.loaded_entries_offset,
                this.entries.len(),
            ),
            conversation_binding: this.conversation_binding.clone(),
            agent_session: this.agent_session.clone(),
            pending_agent_preset: this.pending_agent_preset.clone(),
        });
        this.attach_handle(handle, cx);
        this
    }

    /// 新建独立 ACP 会话（无指定 sid），并在握手完成后发送来源会话的精简交接上下文。
    pub fn start_with_handoff(
        window: &mut Window,
        cx: &mut Context<Self>,
        request: AcpHandoffRequest,
    ) -> Self {
        Self::start_with_handoff_sid(window, cx, request, None)
    }

    /// 同 `start_with_handoff`，但允许调用方注入固定 sid（任务开跑用 `acp-<uuid>`
    /// 绑定执行记录；任务完成边沿按 sid 回查）。`None` = 生成全新 id。
    pub fn start_with_handoff_sid(
        window: &mut Window,
        cx: &mut Context<Self>,
        request: AcpHandoffRequest,
        saved_sid: Option<String>,
    ) -> Self {
        let mut this = Self::placeholder(
            cx,
            AcpViewOrigin {
                agent: request.agent,
                launch: request.launch,
                refresh_launch_from_settings: request.profile_id.is_none(),
                profile_id: request.profile_id,
                cwd: request.cwd,
                reason: String::new(),
                entries: Vec::new(),
                resume_session_id: None,
                saved_sid,
            },
        );
        this.awaiting_initial_history_snapshot = false;
        this.phase = DaemonPhase::Connecting;
        this.end_reason.clear();
        this.starting_since = Some(std::time::Instant::now());
        this.init_input(window, cx);
        this.refresh_launch_from_settings = request.refresh_launch_from_settings;
        this.pending_initial_prompt = {
            let prompt = request.prompt.trim();
            (!prompt.is_empty() && request.fork_session_id.is_none()).then_some(request.prompt)
        };
        // 首包图片解码成待发图片，Idle 时随首包一起发出去。
        this.pending_images = request.images.iter().filter_map(decode_acp_image).collect();
        this.restart_config_values = request.config_values.clone();
        this.pending_initial_config = request.config_values;
        this.ephemeral_env = request.ephemeral_env;
        this.fork_origin = request.source;
        this.conversation_binding = request.conversation_binding;
        this.agent_session = request.agent_session;
        let handle = spawn_acp_client(ConversationClientLaunch {
            id: this.sid.clone(),
            cwd: this.cwd.clone(),
            launch: this.launch.clone(),
            engine_kind: request.agent,
            ephemeral_env: this.ephemeral_env.clone(),
            // controller 续跑：有前次 provider 会话 id 就交给 smeltd 做 session/load
            // 恢复（接上下文），否则开全新会话。Pi 分叉走 `--fork`，不能 --session。
            resume_id: request.resume_session_id,
            fork_id: request.fork_session_id,
            fork_cut: request.fork_cut,
            retained_entries_end: loaded_entries_end(
                this.loaded_entries_offset,
                this.entries.len(),
            ),
            conversation_binding: this.conversation_binding.clone(),
            agent_session: this.agent_session.clone(),
            pending_agent_preset: None,
        });
        this.attach_handle(handle, cx);
        this
    }

    /// 冷启动恢复用的占位：首次显示时自动启动。`origin.entries` 只用于读取旧版存档的
    /// 迁移兼容；当前版本以 agent 的 `session/load` 重放作为历史唯一来源。
    /// `origin.resume_session_id` 是上次握手成功后 agent 分配的 session id。
    ///
    /// `origin.saved_sid`：**这是让 GUI 重开后能真正"接上还活着的 smeltd 会话"而不是
    /// 每次都当新会话重新 spawn 子进程的关键**——smeltd 用 id 判断"这是不是同
    /// 一个会话"，`Some(id)` 时沿用上次持久化的 id（GUI 冷启动恢复走这条，
    /// `main.rs` 的 `AcpSaved.sid`），id 对上了 smeltd 那边只要还没退出/没被
    /// kill，`restart()` 发起的 `acp_open` 就是一次廉价 attach，不是重新 spawn
    /// 子进程。`None` 生成一个全新 id——「从历史会话页继续」和真正的新会话都
    /// 走这条：前者本质是"起一条新的 smeltd 托管连接，靠 `resume_id` 对 agent
    /// 自己的持久化做 session/load"，不是"接上 smeltd 里已经在跑的那个会话"，
    /// 没有理由假装是同一个 id。
    pub fn placeholder(cx: &mut Context<Self>, origin: AcpViewOrigin) -> Self {
        let AcpViewOrigin {
            agent,
            launch,
            refresh_launch_from_settings,
            profile_id,
            cwd,
            reason,
            entries,
            resume_session_id,
            saved_sid,
        } = origin;
        // 冷恢复会话首次显示就直接进入可用的对话页：有旧 session id 时续接，
        // 没有时启动新一轮。历史仍先留在本地，守护端若能 attach 会用其快照覆盖。
        let auto_resume_pending = true;
        let initial_entry_count = entries.len();
        let rendered_images = decode_entry_images(&entries, 0);
        let rendered_diffs = std::collections::HashMap::new();
        let rendered_markdown = build_markdown_cache(&entries, cwd.as_deref());
        let list_state = ListState::new(initial_entry_count, ListAlignment::Top, px(800.))
            .with_uniform_item_height(px(RESTORED_ENTRY_HEIGHT_HINT_PX));
        list_state.set_follow_mode(FollowMode::Tail);
        let view = cx.entity().downgrade();
        list_state.set_scroll_handler(move |event, _window, cx| {
            let view = view.clone();
            let viewing_history = event.is_scrolled && !event.is_following_tail;
            let near_loaded_top = event.visible_range.start <= 5;
            // list 正在可变借用自己的状态，延后通知外层重新判断 sticky 提问。
            cx.defer(move |cx| {
                let _ = view.update(cx, |this, cx| {
                    if this.viewing_history != viewing_history {
                        this.viewing_history = viewing_history;
                        cx.notify();
                    }
                    if near_loaded_top {
                        this.load_older_history(cx);
                    }
                });
            });
        });
        Self {
            auto_resume_pending,
            sid: saved_sid.unwrap_or_else(|| format!("acp-{}", uuid::Uuid::new_v4())),
            cwd,
            entries,
            permissions: Vec::new(),
            permission_submitting: None,
            elicitation: None,
            elicitation_inputs: Default::default(),
            status_line: None,
            phase: DaemonPhase::Dead,
            end_reason: reason,
            end_kind: AcpEndKind::Unknown,
            accepted_delivery_ids: Default::default(),
            active_delivery_id: None,
            completed_delivery_id: None,
            input: None,
            input_has_draft: false,
            handle: None,
            snapshot_stream_generation: 0,
            last_snapshot_revision: 0,
            auto_reconnect_left: AUTO_RECONNECT_ATTEMPTS,
            launch,
            ephemeral_env: Default::default(),
            refresh_launch_from_settings,
            profile_id,
            agent,
            pending_images: Vec::new(),
            pending_external_image_loads: 0,
            queued_prompts: std::collections::VecDeque::new(),
            prompt_dispatch_pending: false,
            immediate_cancel_pending: false,
            rendered_images,
            rendered_diffs,
            rendered_tool_images: std::collections::HashMap::new(),
            rendered_markdown,
            loaded_entries_offset: 0,
            entries_total: initial_entry_count,
            history_loading: false,
            session_title: None,
            supports_image: true,
            paste_hint: None,
            completion: None,
            completion_scroll: ScrollHandle::new(),
            file_cache: None,
            file_list_loading: false,
            acp_session_id: None,
            history_session_id: resume_session_id,
            available_commands: Vec::new(),
            usage: None,
            usage_cached_read: None,
            usage_cost: None,
            usage_breakdown: None,
            usage_popover_open: false,
            trajectory_window: None,
            supports_compaction: false,
            supports_native_queue: false,
            supports_rewind: false,
            compacting: false,
            queued_steering: Vec::new(),
            queued_follow_up: Vec::new(),
            last_composer_restore_revision: 0,
            pending_composer_restore: None,
            skip_next_composer_restore: false,
            starting_since: None,
            plan: None,
            // 计划是导航摘要，不应该在打开会话时占据整块消息区；需要细节时
            // 由用户主动展开，保持第一眼聚焦在目标和结果上。
            plan_collapsed: true,
            expanded_thoughts: std::collections::HashSet::new(),
            expanded_process_groups: std::collections::HashSet::new(),
            expanded_tool_runs: std::collections::HashSet::new(),
            model: None,
            config_options: Vec::new(),
            pending_config_values: Vec::new(),
            turn_started_at_ms: None,
            turn_timings: Vec::new(),
            running_tick: false,
            turn_outcome: None,
            was_completed_unread: false,
            was_ended: false,
            expanded_tools: std::collections::HashSet::new(),
            expanded_tool_cards: std::collections::HashSet::new(),
            collapsed_tool_cards: std::collections::HashSet::new(),
            list_state,
            awaiting_initial_history_snapshot: true,
            viewing_history: false,
            restarting: false,
            restart_error: None,
            pending_initial_prompt: None,
            pending_initial_config: Vec::new(),
            restart_config_values: Vec::new(),
            fork_origin: None,
            conversation_binding: smelt_core::conversation::ConversationBinding::Direct,
            agent_session: None,
            pending_conversation_input: None,
            uncertain_conversation_input: None,
            pending_agent_preset: None,
            focus_handle: cx.focus_handle(),
            _input_sub: None,
        }
    }

    /// 「重新开始」：带着上次的 session id（如果有）尝试真续接——smeltd 那边
    /// 如果这个会话还活着就是普通 attach，已经 Ended 才会真的重新 spawn
    /// 子进程（见 smeltd `acp_open` 的 attach-vs-relaunch 判断，这里不用关心
    /// 是哪一种，反正结果都会以快照回来：`ReadyKind::ResumedWithReplay` 时
    /// 服务端已经清空 entries 让 replay 重建，`Fresh` 且本地有历史时服务端
    /// 已经插好分割线——这层拿到的快照就是最终结果，不用再猜）。
    fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.restart_with_launch(window, cx, false);
    }

    /// 重启时是否保留调用方刚刚注入的启动规格。controller 复用已有 profile 会话
    /// 时仍要保留 profile 身份用于展示，但不能让 profile 的全局配置覆盖这次
    /// 任务专用的权限参数。
    fn restart_with_launch(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        preserve_launch: bool,
    ) {
        // 空白会话把 runtime id 错存成 history id，是旧版留下的脏状态；
        // 这类会话没有可 load 的内容，直接开新会话。真正的历史恢复占位
        // 只有 history id、没有同值 runtime id，因此仍保留 session/load。
        let stale_blank_history_id = is_stale_blank_history_id(
            self.entries.is_empty(),
            self.history_session_id.as_ref(),
            self.acp_session_id.as_ref(),
        );
        let resume_id = (!stale_blank_history_id)
            .then(|| self.history_session_id.as_ref().map(ToString::to_string))
            .flatten();
        if stale_blank_history_id {
            self.history_session_id = None;
        }
        if !preserve_launch
            && let Some(cfg) = cx.try_global::<smelt_ui::agent_host_state::AgentHostState>()
        {
            self.launch = resolve_restart_launch(
                &self.launch,
                self.profile_id.as_deref(),
                cfg,
                self.agent,
                self.refresh_launch_from_settings,
            );
        }
        self.permissions.clear();
        self.permission_submitting = None;
        self.elicitation = None;
        self.elicitation_inputs.clear();
        self.plan = None; // 计划是回合态，新会话不该带着上一段的进度条
        self.model = None; // 模型等新会话握手后重新上报
        self.config_options.clear();
        self.pending_config_values.clear();
        // 不要让重启后的会话悄悄退回 provider 默认配置；这组值同时覆盖普通
        // 会话的用户选择和 controller 任务注入的权限/推理配置。
        self.pending_initial_config = self.restart_config_values.clone();
        self.usage = None; // 上下文用量属于旧会话，别带到新的上
        self.usage_breakdown = None;
        self.usage_popover_open = false;
        self.prompt_dispatch_pending = false;
        self.immediate_cancel_pending = false;
        self.turn_outcome = None;
        self.phase = DaemonPhase::Connecting;
        self.end_reason.clear();
        self.starting_since = Some(std::time::Instant::now());
        self.init_input(window, cx);
        let handle = spawn_acp_client(ConversationClientLaunch {
            id: self.sid.clone(),
            cwd: self.cwd.clone(),
            launch: self.launch.clone(),
            engine_kind: self.agent,
            ephemeral_env: self.ephemeral_env.clone(),
            resume_id,
            fork_id: None,
            fork_cut: None,
            retained_entries_end: loaded_entries_end(
                self.loaded_entries_offset,
                self.entries.len(),
            ),
            conversation_binding: self.conversation_binding.clone(),
            agent_session: self.agent_session.clone(),
            pending_agent_preset: self.pending_agent_preset.clone(),
        });
        self.attach_handle(handle, cx);
        cx.notify();
    }

    /// 历史页再次点“继续”时主动重新连接 smeltd。若 daemon 会话仍在，这只是
    /// attach 并立即返回完整快照；若 GUI 之前断线留下了旧 View，也能由此修复。
    pub fn reattach_to_daemon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.restart(window, cx);
    }

    /// 守护侧断连（smeltd 升级/重启/未就绪）后自动重连：预算内直接 restart。
    /// 由 `workspace_sessions.rs` 在 `AcpViewEvent::Ended`（断连原因）时按指数退避循环调用。
    /// 返回 true = 本次发起了重连；false = 预算耗尽 / 连接已活跃 / 视图态不对
    /// （调用方据此结束退避循环）。
    pub fn maybe_auto_reconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // 连接已重建（用户手动重启 / 上轮自动重连已生效）就不重复。
        if !matches!(self.phase, DaemonPhase::Dead) || self.handle.is_some() {
            return false;
        }
        if self.auto_reconnect_left == 0 {
            return false;
        }
        self.auto_reconnect_left -= 1;
        self.restart(window, cx);
        true
    }

    /// 舞台头状态胶囊用的相位：ACP 当前活动，与守护相位同一枚举。
    pub fn phase(&self) -> DaemonPhase {
        self.phase
    }

    /// 当前上下文 token 用量 `(used, window)`；这不是本轮 input/output，
    /// 不能直接作为外部 controller 的账单 usage 回传。
    pub fn usage(&self) -> Option<(u64, u64)> {
        self.usage
    }

    /// 最近一条状态行（agent 在干什么，如「正在运行 test」）；None = 无。
    /// controller 进度上报（report_progress summary）用。
    pub fn status_line(&self) -> Option<String> {
        self.status_line.clone()
    }

    /// 当前完整消息流。controller 任务执行日志上报用（增量按 entries 长度水位
    /// 取新条目，映射成官方 TaskMessage 批量上报）。
    pub fn entries(&self) -> &[AcpEntry] {
        &self.entries
    }

    pub fn phase_label(&self) -> (&'static str, u32) {
        conversation_phase_label(
            &self.phase,
            self.turn_outcome,
            self.prompt_dispatch_pending,
            self.immediate_cancel_pending,
            self.is_fresh_conversation_start(),
            self.has_pending_fresh_start_prompt(),
        )
    }

    fn is_fresh_conversation_start(&self) -> bool {
        is_fresh_conversation_start(
            &self.phase,
            self.entries.is_empty(),
            self.history_session_id.is_some(),
            self.pending_initial_prompt.is_some(),
        )
    }

    fn has_pending_fresh_start_prompt(&self) -> bool {
        self.is_fresh_conversation_start() && self.prompt_dispatch_pending
    }

    fn has_active_turn(&self) -> bool {
        matches!(
            self.phase,
            DaemonPhase::Thinking
                | DaemonPhase::ExecutingTool
                | DaemonPhase::AwaitingApproval
                | DaemonPhase::WaitingForUser
        ) || self.has_pending_fresh_start_prompt()
            || is_dispatch_in_flight(self.prompt_dispatch_pending, self.immediate_cancel_pending)
    }

    fn status_evidence(&self) -> AcpStatusEvidence {
        AcpStatusEvidence {
            phase: self.phase,
            turn_outcome: self.turn_outcome,
            prompt_dispatch_pending: self.prompt_dispatch_pending
                || self.pending_conversation_input.is_some(),
            immediate_cancel_pending: self.immediate_cancel_pending,
            fresh_start_pending: self.has_pending_fresh_start_prompt(),
            has_unfinished_tool: smelt_core::acp_chat::has_unfinished_tool_call(&self.entries),
        }
    }

    /// 对话里用户能看见的「正在跑」：思考/工具或派发窗口。
    /// 回合结束后仍未收尾的工具卡不能把侧栏钉在运行蓝。
    pub fn is_visibly_running(&self) -> bool {
        self.status_evidence().is_visibly_running()
    }

    /// ACP 协议视图自己的三态提示。最终状态还会在 Workspace 与 smeltd 镜像
    /// 统一聚合，避免发送/失败快照先到任一侧时短暂显示成空闲。
    pub fn agent_status(&self) -> AgentStatus {
        self.status_evidence().status()
    }

    /// 切到本会话时自动启动：冷恢复占位（Ended）第一次被激活就 restart，
    /// 像终端一样「点开就是活的」。只触发一次，见字段注释。
    pub fn maybe_auto_resume(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.auto_resume_pending {
            return;
        }
        self.auto_resume_pending = false;
        if matches!(self.phase, DaemonPhase::Dead) && self.handle.is_none() {
            self.restart(window, cx);
        }
    }

    fn init_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.input.is_some() {
            return;
        }
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("想让它做什么？")
                .submit_on_enter(true)
                .auto_grow(3, 10)
        });
        self._input_sub = Some(cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, input, ev: &InputEvent, window, cx| {
                match ev {
                    InputEvent::PressEnter {
                        shift, secondary, ..
                    } => {
                        if !shift {
                            this.submit_input(window, *secondary, cx);
                        }
                    }
                    // 每次文本变化重算补全 token（打 `@`/`/` 就弹，打空格就收）。
                    InputEvent::Change => {
                        let has_draft = !input.read(cx).value().trim().is_empty();
                        if this.input_has_draft != has_draft {
                            this.input_has_draft = has_draft;
                            cx.notify();
                        }
                        this.refresh_completion(cx);
                    }
                    InputEvent::Blur => this.completion = None,
                    _ => {}
                }
            },
        ));
        self.input = Some(input);
    }

    /// GPUI 输入实体只能在拥有 `Window`/`Context` 的渲染边界创建。把这一步封装
    /// 在 AcpView 内部，避免调用方必须记住额外的准备顺序；后续元素树投影保持只读。
    fn ensure_elicitation_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(card) = &self.elicitation {
            // 自由文本字段，以及题目卡上「自己写答案」的输入行，共用同一套
            // 输入实体（按字段下标存取，回显快照里的 text_values）。
            let text_fields: Vec<(usize, bool, String, String)> = card
                .fields
                .iter()
                .enumerate()
                .filter_map(|(ix, field)| {
                    let (secret, placeholder) = match &field.kind {
                        ElicitFieldKindView::Text { secret } => (*secret, field.title.clone()),
                        ElicitFieldKindView::Select(_) | ElicitFieldKindView::MultiSelect(_)
                            if field.allow_custom_input =>
                        {
                            (false, "输入你自己的答案".to_string())
                        }
                        _ => return None,
                    };
                    Some((
                        ix,
                        secret,
                        placeholder,
                        card.text_values.get(&ix).cloned().unwrap_or_default(),
                    ))
                })
                .collect();
            self.elicitation_inputs
                .retain(|ix, _| text_fields.iter().any(|(field_ix, ..)| field_ix == ix));
            for (ix, secret, title, value) in text_fields {
                self.elicitation_inputs.entry(ix).or_insert_with(|| {
                    cx.new(|cx| {
                        let mut state = InputState::new(window, cx)
                            .placeholder(&title)
                            .default_value(value);
                        if secret {
                            state = state.masked(true);
                        }
                        state
                    })
                });
            }
        }
    }

    /// 回合进行中每秒重绘一次，让「已用 6s」跟 Grok 一样往前走。
    fn ensure_running_tick(&mut self, cx: &mut Context<Self>) {
        if self.running_tick {
            return;
        }
        self.running_tick = true;
        cx.spawn(async move |this, cx| {
            loop {
                smol::Timer::after(std::time::Duration::from_secs(1)).await;
                let keep = this
                    .update(cx, |view, cx| {
                        let running = view.has_active_turn();
                        if running {
                            cx.notify();
                        } else {
                            view.running_tick = false;
                        }
                        running
                    })
                    .unwrap_or(false);
                if !keep {
                    return;
                }
            }
        })
        .detach();
    }

    /// 非静默启动期每秒重绘一次，让横幅上的「已 N 秒」真的在走。
    /// 相位离开 Starting 就自然停（不占常驻定时器）。
    fn tick_starting(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                smol::Timer::after(std::time::Duration::from_secs(1)).await;
                let keep = this
                    .update(cx, |v, cx| {
                        let starting = matches!(v.phase, DaemonPhase::Connecting);
                        if starting {
                            cx.notify();
                        }
                        starting
                    })
                    .unwrap_or(false);
                if !keep {
                    return;
                }
            }
        })
        .detach();
    }

    /// 挂上连接句柄并起快照 drain（start / restart 共用）。
    fn attach_handle(&mut self, handle: ConversationClientHandle, cx: &mut Context<Self>) {
        let snapshot_rx = handle.snapshot_rx.clone();
        self.snapshot_stream_generation = self.snapshot_stream_generation.wrapping_add(1);
        let stream_generation = self.snapshot_stream_generation;
        self.last_snapshot_revision = 0;
        self.handle = Some(handle);
        if !self.is_fresh_conversation_start() {
            self.tick_starting(cx);
        }
        cx.spawn(async move |this, cx| {
            while let Ok(snap) = snapshot_rx.recv().await {
                if this
                    .update(cx, |view, cx| {
                        if view.snapshot_stream_generation == stream_generation {
                            view.apply_snapshot(snap, cx);
                        }
                    })
                    .is_err()
                {
                    return; // 视图已销毁
                }
            }
        })
        .detach();
    }

    fn load_older_history(&mut self, cx: &mut Context<Self>) {
        if !can_load_older_history(self.history_loading, self.loaded_entries_offset) {
            return;
        }
        self.history_loading = true;
        let sid = self.sid.clone();
        let before = self.loaded_entries_offset;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { load_acp_history(&sid, before, ACP_HISTORY_PAGE_LIMIT) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.history_loading = false;
                match result {
                    Ok(snapshot) => view.prepend_history_page(snapshot, cx),
                    Err(error) => eprintln!("[workspace] ACP 历史分页失败：{error}"),
                }
            });
        })
        .detach();
    }

    fn prepend_history_page(&mut self, snapshot: ConversationSnapshot, cx: &mut Context<Self>) {
        let page_len = snapshot.entries.len();
        if page_len == 0
            || snapshot.entries_offset.saturating_add(page_len) != self.loaded_entries_offset
        {
            return;
        }

        let mut page = snapshot.entries;
        page.append(&mut self.entries);
        self.entries = page;
        self.loaded_entries_offset = snapshot.entries_offset;
        self.entries_total = self.entries_total.max(snapshot.entries_total);
        if snapshot.session_title.is_some() {
            self.session_title = snapshot.session_title;
        }

        self.rendered_images = self
            .rendered_images
            .drain()
            .map(|((entry_ix, image_ix), image)| ((entry_ix + page_len, image_ix), image))
            .collect();
        self.rendered_images
            .extend(decode_entry_images(&self.entries[..page_len], 0));

        let mut markdown = build_markdown_cache(&self.entries[..page_len], self.cwd.as_deref());
        markdown.append(&mut self.rendered_markdown);
        self.rendered_markdown = markdown;
        self.expanded_thoughts = self
            .expanded_thoughts
            .drain()
            .map(|entry_ix| entry_ix + page_len)
            .collect();
        self.expanded_process_groups = self
            .expanded_process_groups
            .drain()
            .map(|entry_ix| entry_ix + page_len)
            .collect();
        self.expanded_tool_runs = self
            .expanded_tool_runs
            .drain()
            .map(|entry_ix| entry_ix + page_len)
            .collect();

        // GPUI shifts the logical scroll anchor by `page_len` for a pure prepend, so the
        // same message stays under the cursor instead of jumping to the newly loaded page.
        self.list_state.splice(0..0, page_len);
        cx.notify();
    }

    /// smeltd 托管用的会话 id，也是持久化存档（`AcpSaved.sid`）的 key——
    /// GUI 重开后拿它原样传回 `placeholder` 的 `saved_sid`，才能接上 smeltd
    /// 里还活着的同一个会话，而不是每次都当新会话处理。
    pub fn session_id(&self) -> &str {
        &self.sid
    }

    /// 通用 ACP `session_info_update` 的 Agent 标题优先；Agent 尚未上报时由
    /// smeltd 从首条用户消息生成稳定兜底。侧栏和完成通知共用这个结果。
    pub fn auto_title(&self) -> Option<String> {
        self.session_title
            .clone()
            .or_else(|| smelt_core::acp_chat::auto_title(&self.entries))
    }

    /// 存档快照：写进 AcpSaved.history_session_id，GUI 重开后「重新开始」
    /// 才有旧 session id 可用来尝试真续接。
    pub fn history_session_id_for_save(&self) -> Option<SessionId> {
        self.history_session_id.clone()
    }

    /// 首包尚未真正发给 agent 时的可恢复文本。
    ///
    /// `start_with_handoff_sid` 先把交接 prompt 放在内存里，等 ACP 进入 Idle
    /// 后再发送。应用若在这个窗口重启，原来的占位视图不会再拥有这段文本，
    /// 最终就会变成“有会话、无消息、也无错误”的空白页。只保存文本即可恢复
    /// 发送意图；已经从 pending 取走并发出的 prompt 不会出现在这里。
    pub fn pending_prompt_for_save(&self) -> Option<String> {
        self.pending_initial_prompt
            .clone()
            .or_else(|| self.queued_prompts.front().map(|(text, _)| text.clone()))
            .filter(|text| !text.trim().is_empty())
    }

    pub fn pending_delivery_id_for_save(&self) -> Option<String> {
        // 兼容旧 workspace schema；Task delivery 已由 smeltd 独占，GUI 不再
        // 保存可重放的 run id。
        None
    }

    pub fn pending_agent_preset_for_save(&self) -> Option<String> {
        self.pending_agent_preset.clone()
    }

    pub fn conversation_binding_for_save(&self) -> smelt_core::conversation::ConversationBinding {
        self.conversation_binding.clone()
    }

    /// 当前产品级智能体会话身份。ACP provider 只描述执行器，不能替代它。
    pub fn agent_session(&self) -> Option<smelt_plugin_api::AgentSessionBinding> {
        self.agent_session.clone()
    }

    pub fn agent_session_for_save(&self) -> Option<smelt_plugin_api::AgentSessionBinding> {
        self.agent_session.clone()
    }

    /// 标准 SessionAction 的调用目标。实例身份来自 daemon 已认证的 binding；
    /// 只有同一插件的输入路由上下文才会透传，避免把别家路由数据交叉泄露。
    pub fn session_action_payload(
        &self,
    ) -> Option<smelt_plugin_api::SessionActionInvocationPayload> {
        let agent_session = self.agent_session.clone()?;
        let context = match &self.conversation_binding {
            smelt_core::conversation::ConversationBinding::Plugin { plugin_id, route }
                if plugin_id == &agent_session.controller.plugin_id =>
            {
                route.context.clone()
            }
            _ => serde_json::Value::Null,
        };
        Some(smelt_plugin_api::SessionActionInvocationPayload {
            agent_session,
            context,
        })
    }

    /// daemon 不存在时由 workspace 重建首轮预设；热 attach 时 daemon 会保留自己
    /// 的消费事实，不会被这里的旧镜像重新写入。
    pub fn restore_pending_agent_preset(&mut self, prompt: Option<String>) {
        self.pending_agent_preset = prompt.filter(|prompt| !prompt.trim().is_empty());
    }

    /// 冷启动恢复出来的视图没有消息，`auto_title` 的推导兜底也就无从谈起。
    /// 存档里记着上次生效的标题，先摆上去；agent 之后上报新标题会覆盖它。
    pub fn restore_session_title(&mut self, title: Option<String>) {
        if let Some(title) = title.filter(|title| !title.trim().is_empty()) {
            self.session_title = Some(title);
        }
    }

    /// 用户在侧栏改名后同步给 agent。本地标题立即生效，不等 agent 回执；
    /// agent 不支持改名时也只是没有回流，侧栏名字照样是新的。
    pub fn rename_session(&mut self, title: Option<String>) {
        let title = title
            .map(|title| title.trim().to_string())
            .filter(|title| !title.is_empty());
        self.session_title = title.clone();
        let (Some(handle), Some(title)) = (&self.handle, title) else {
            return;
        };
        let _ = handle
            .action_tx
            .try_send(AcpUserAction::SetSessionTitle { title });
    }

    /// 存档快照：把当前生效的标题固化下来，重启后侧栏名字保持不变。
    pub fn session_title_for_save(&self) -> Option<String> {
        self.auto_title()
            .map(|title| title.trim().to_string())
            .filter(|title| !title.is_empty())
    }

    /// daemon 不存在时用 workspace 的最后镜像创建会话；热 attach 后首份快照会以
    /// daemon 当前 binding 覆盖它。
    pub fn restore_conversation_binding(
        &mut self,
        binding: smelt_core::conversation::ConversationBinding,
    ) {
        self.conversation_binding = binding;
    }

    /// daemon 不存在时用 workspace 的最后镜像恢复产品身份；热 attach 后快照会
    /// 以 daemon 当前绑定覆盖它。
    pub fn restore_agent_session(
        &mut self,
        agent_session: Option<smelt_plugin_api::AgentSessionBinding>,
    ) {
        self.agent_session = agent_session;
    }

    /// 当前会话最后一次生效的 ACP 配置，写进 `AcpSaved` 供 GUI 重启后恢复。
    ///
    /// 普通会话在用户从配置胶囊切换选项时更新这份缓存；尚未发生过显式切换时
    /// 从握手快照读取当前默认值，避免新会话存档后丢失 provider 配置。Task 的
    /// runtime 配置由 smeltd 持有，不进入 GUI workspace。
    pub fn config_values_for_save(&self) -> Vec<(String, String)> {
        if !self.restart_config_values.is_empty() {
            return self.restart_config_values.clone();
        }
        self.current_config_values()
    }

    /// 从 workspace 存档恢复会话配置。调用方应在视图第一次自动重启前调用，
    /// 这样 `restart()` 会把这组值排到握手后的首包之前。
    pub fn restore_config_values(&mut self, values: Vec<(String, String)>) {
        self.restart_config_values = values.clone();
        self.pending_initial_config = values;
    }

    /// 从 workspace 恢复尚未发送的首包。只在视图自身没有待发消息时写入，
    /// 防止恢复流程重复覆盖用户已经排队的新输入。
    pub fn restore_pending_prompt(&mut self, prompt: Option<String>, delivery_id: Option<String>) {
        // 旧版本可能把未派发的外部投递首包存进 GUI workspace。daemon 现在是
        // 唯一执行者，带 delivery id 的旧意图必须丢弃，避免打开多个窗口时重放。
        if self.pending_initial_prompt.is_some() || !self.queued_prompts.is_empty() {
            return;
        }
        self.pending_initial_prompt = restorable_gui_prompt(prompt, delivery_id.as_deref());
    }

    /// 停止当前 turn（session/cancel）。agent 会以 Cancelled 收尾，相位随 TurnEnded 回 Idle。
    fn cancel_turn(&mut self) {
        if let Some(h) = &self.handle {
            let _ = h.action_tx.try_send(AcpUserAction::Cancel);
        }
    }

    /// 「停止」打不断（agent 卡在工具调用里对 cancel 不理不睬）时的兜底：
    /// 让 smeltd 直接杀掉整个 agent 进程组、换一个新的接着跑，带
    /// `history_session_id` 走 `session/load` 接回同一份历史——标签、这条视图
    /// 的 entries、GUI 这边的 acp_open 连接全部原地不动，只是底下的进程换了
    /// 一个。跟 `restart()`（给已 Ended 的占位视图用）不是一回事：那个要重新
    /// 建 GUI 自己的 socket 连接；这个不需要，smeltd 杀完重连内部子进程后会
    /// 照常沿着现有连接推新快照过来（`attach_handle` 起的 drain 循环还在跑）。
    pub fn force_restart(&mut self, cx: &mut Context<Self>) {
        if self.restarting {
            return;
        }
        self.restarting = true;
        self.restart_error = None;
        cx.notify();
        let sid = self.sid.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { smelt_core::acp_client::restart_acp_session(&sid) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.restarting = false;
                if let Err(err) = result {
                    view.restart_error = Some(err);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn cwd(&self) -> Option<String> {
        self.cwd.clone()
    }

    /// 启动规格（存档用：重开 GUI 后按它「重新开始」）。
    pub fn launch_spec(&self) -> ConversationLaunchSpec {
        self.launch.clone()
    }

    pub fn refresh_launch_from_settings(&self) -> bool {
        self.refresh_launch_from_settings
    }

    pub fn profile_id(&self) -> Option<&str> {
        self.profile_id.as_deref()
    }

    /// 这条会话接的 agent 种类（存档 / 标题 / 舞台头胶囊用）。
    pub fn agent_kind(&self) -> ConversationAgentKind {
        self.agent
    }

    pub fn fork_origin(&self) -> Option<AcpForkOrigin> {
        self.fork_origin.clone()
    }

    pub fn set_fork_origin(&mut self, origin: Option<AcpForkOrigin>) {
        self.fork_origin = origin;
    }

    /// 当前生效的会话配置（含模型），`(config_id, value)` 形式——只在同 agent
    /// 续接时透传。
    fn current_config_values(&self) -> Vec<(String, String)> {
        let mut values: Vec<(String, String)> = self
            .config_options
            .iter()
            .filter_map(|config| {
                config.options.iter().find_map(|(value, name)| {
                    (name == &config.current_name)
                        .then(|| (config.config_id.clone(), value.clone()))
                })
            })
            .collect();
        if let Some(model) = &self.model
            && !model.current_value.is_empty()
        {
            values.push((model.config_id.clone(), model.current_value.clone()));
        }
        values
    }

    /// cwd 下的文件清单（首次调用后台列，之后走缓存）。
    fn file_list(&mut self, cx: &mut Context<Self>) -> std::rc::Rc<Vec<String>> {
        if let Some(cached) = &self.file_cache {
            return cached.clone();
        }
        if self.file_list_loading {
            return std::rc::Rc::new(Vec::new());
        }
        let Some(cwd) = self.cwd.clone() else {
            return std::rc::Rc::new(Vec::new());
        };
        // 首次：git ls-files / 目录遍历挪后台（大仓库可能跑几百 ms），
        // 完成后重算当前补全；期间 At 补全先给空，等任务回来自动弹出。
        self.file_list_loading = true;
        cx.spawn(async move |this, cx| {
            // list_files 返回 Rc（非 Send），后台只产出 Vec，回主线程再包 Rc。
            let list = cx
                .background_executor()
                .spawn(async move { smelt_ui::acp_completion::list_files(&cwd).as_ref().clone() })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.file_list_loading = false;
                this.file_cache = Some(std::rc::Rc::new(list));
                this.refresh_completion(cx);
            });
        })
        .detach();
        std::rc::Rc::new(Vec::new())
    }

    /// 按输入框当前内容重算补全候选。
    fn refresh_completion(&mut self, cx: &mut Context<Self>) {
        // 普通粘贴提示可在用户继续输入后撤掉；图片文件仍在后台读取时
        // 必须保留进度提示，否则看起来又像粘贴没生效。
        if self.pending_external_image_loads == 0 {
            self.paste_hint = None;
        }
        let Some(input) = self.input.clone() else {
            self.completion = None;
            return;
        };
        let (text, cursor) = {
            let s = input.read(cx);
            (s.value().to_string(), s.cursor())
        };
        // cursor 是字节偏移，可能落在多字节字符中间（中文输入过程中），
        // 切之前先确认是字符边界，否则 panic。
        let cursor = cursor.min(text.len());
        if !text.is_char_boundary(cursor) {
            return;
        }
        let Some(trigger) = smelt_ui::acp_completion::detect_trigger(&text[..cursor]) else {
            if self.completion.is_some() {
                self.completion = None;
                cx.notify();
            }
            return;
        };
        let files = match trigger.kind {
            smelt_ui::acp_completion::Kind::At => self.file_list(cx),
            smelt_ui::acp_completion::Kind::Slash => std::rc::Rc::new(Vec::new()),
        };
        let items =
            smelt_ui::acp_completion::candidates(&trigger, &files, &self.available_commands);
        self.completion = (!items.is_empty()).then_some(CompletionPopup {
            start: trigger.start,
            end: cursor,
            items,
            selected: 0,
        });
        self.completion_scroll.scroll_to_item(0);
        cx.notify();
    }

    /// 上下移动补全选中项（返回 false = 当前没在补全，按键该交回输入框）。
    fn move_completion(&mut self, delta: i32, cx: &mut Context<Self>) -> bool {
        let Some(popup) = &mut self.completion else {
            return false;
        };
        let n = popup.items.len() as i32;
        popup.selected = (popup.selected as i32 + delta).rem_euclid(n) as usize;
        self.completion_scroll.scroll_to_item(popup.selected);
        cx.notify();
        true
    }

    /// 把选中的候选替换进输入框（返回 false = 没在补全）。
    fn accept_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(popup) = self.completion.take() else {
            return false;
        };
        let Some(input) = self.input.clone() else {
            return false;
        };
        let Some(item) = popup.items.get(popup.selected) else {
            return false;
        };
        let insert = item.insert.clone();
        input.update(cx, |s, cx| {
            let text = s.value().to_string();
            // 只换掉触发 token 那一段，光标后面的内容原样留着。
            if popup.start <= popup.end
                && popup.end <= text.len()
                && text.is_char_boundary(popup.start)
                && text.is_char_boundary(popup.end)
            {
                let merged = format!("{}{}{}", &text[..popup.start], insert, &text[popup.end..]);
                let cursor_after_insert = popup.start + insert.len();
                s.set_value(merged, window, cx);
                let position = s.text().offset_to_position(cursor_after_insert);
                s.set_cursor_position(position, window, cx);
            }
            s.focus(window, cx);
        });
        cx.notify();
        true
    }

    /// 末几条消息的纯文本（总览卡片迷你预览，对齐终端的 last_lines）。
    pub fn last_lines(&self, n: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in self.entries.iter().rev() {
            if out.len() >= n {
                break;
            }
            let line = match e {
                AcpEntry::User(t) => format!("> {}", t.lines().next().unwrap_or_default()),
                AcpEntry::UserWithImages { text, images } => {
                    let first = text.lines().next().unwrap_or_default();
                    if first.is_empty() {
                        format!("> {} 张图片", images.len())
                    } else {
                        format!("> {first}")
                    }
                }
                AcpEntry::Assistant {
                    text,
                    thought: false,
                } => text.lines().last().unwrap_or_default().to_string(),
                AcpEntry::Assistant { thought: true, .. } => continue,
                AcpEntry::ToolCall { title, output, .. }
                    if is_task_completion_tool_title(title) =>
                {
                    let summary = completion_summary_text(output);
                    if summary.trim().is_empty() {
                        "完成".to_string()
                    } else {
                        summary.lines().last().unwrap_or("完成").to_string()
                    }
                }
                AcpEntry::ToolCall { title, .. } => format!("🔧 {title}"),
                AcpEntry::Divider(_) => continue,
            };
            if !line.trim().is_empty() {
                out.push(line);
            }
        }
        out.reverse();
        out
    }

    pub fn completed_unread(&self, cx: &App) -> bool {
        cx.try_global::<AttentionGlobal>().is_some_and(|store| {
            store
                .0
                .lock()
                .unwrap()
                .unread(&self.sid)
                .is_some_and(|item| item.kind == AttentionKind::Success)
        })
    }

    /// 会话被激活查看后清「有结果可看」。
    pub fn mark_read(&mut self, cx: &mut Context<Self>) {
        if cx.try_global::<AttentionGlobal>().is_some() {
            AttentionGlobal::mark_read(&self.sid, cx);
        }
    }

    pub fn is_awaiting_approval(&self) -> bool {
        matches!(self.phase, DaemonPhase::AwaitingApproval)
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self.phase,
            DaemonPhase::Thinking | DaemonPhase::ExecutingTool
        )
    }

    /// 出了选择题等用户点（四档色里归「需要处理」橙档）。
    pub fn is_awaiting_choice(&self) -> bool {
        matches!(self.phase, DaemonPhase::WaitingForUser)
    }

    pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
        if let Some(input) = &self.input {
            input.update(cx, |s, cx| s.focus(window, cx));
        }
    }

    /// 把一段文本塞进输入框并聚焦（SKILLS 面板点一条 skill 用）。
    /// 不自动发送——skill 后面常还要补一句话，发不发由人定。
    pub fn insert_prompt_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.input.clone() else {
            return;
        };
        input.update(cx, |s, cx| {
            let cur = s.value().to_string();
            let (merged, cursor_offset) = append_prompt_text(&cur, text);
            s.set_value(merged, window, cx);
            let position = s.text().offset_to_position(cursor_offset);
            s.set_cursor_position(position, window, cx);
            s.focus(window, cx);
        });
        self.input_has_draft = true;
        cx.notify();
    }

    /// Composer 的交互输入统一走 daemon-owned 路由。后台 Task/Peer delivery 仍
    /// 直接使用 `AcpUserAction::Prompt`，不会经过这里再次送回远端系统。
    pub fn send_prompt(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if (text.trim().is_empty() && self.pending_images.is_empty())
            || self.pending_conversation_input.is_some()
        {
            return;
        }
        let images = std::mem::take(&mut self.pending_images);
        let input = conversation_input_for_submit(
            text.clone(),
            encode_prompt_images(&images),
            self.uncertain_conversation_input.as_ref(),
        );
        self.pending_conversation_input = Some(PendingConversationInput {
            input: input.clone(),
            text,
            images,
            snapshot_revision: self.last_snapshot_revision,
        });
        self.paste_hint = None;
        cx.notify();

        let sid = self.sid.clone();
        let config_values = self.pending_config_values.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    smelt_core::session_control::submit_conversation_input(
                        &sid,
                        &input,
                        &config_values,
                    )
                })
                .await;
            let _ = this.update_in(cx, |view, window, cx| {
                view.finish_conversation_input(result, window, cx)
            });
        })
        .detach();
    }

    fn finish_conversation_input(
        &mut self,
        result: Result<
            smelt_core::conversation::ConversationInputRoute,
            smelt_core::conversation::ConversationSubmitError,
        >,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_conversation_input.take() else {
            return;
        };
        match result {
            Ok(smelt_core::conversation::ConversationInputRoute::Direct) => {
                self.uncertain_conversation_input = None;
                // 只有提交后尚未见过任何新快照时才补本地派发态。Running 快照
                // 可能先于短请求回执到达，不能在它之后重新把 pending 置回 true。
                if self.last_snapshot_revision == pending.snapshot_revision {
                    self.prompt_dispatch_pending = true;
                }
            }
            Ok(smelt_core::conversation::ConversationInputRoute::Plugin) => {
                self.uncertain_conversation_input = None;
            }
            Err(error) => {
                if let Some(input) = self.input.clone() {
                    let current = input.read(cx).value().to_string();
                    let restored = merge_rejected_prompt(&current, &pending.text);
                    input.update(cx, |input, cx| {
                        input.set_value(restored, window, cx);
                        input.focus(window, cx);
                    });
                }
                let mut restored_images = pending.images;
                restored_images.append(&mut self.pending_images);
                self.pending_images = restored_images;
                self.input_has_draft = true;
                self.uncertain_conversation_input = match error.kind {
                    smelt_core::conversation::ConversationSubmitErrorKind::Rejected => None,
                    smelt_core::conversation::ConversationSubmitErrorKind::Unknown => {
                        Some(pending.input)
                    }
                };
                self.paste_hint = Some(match error.kind {
                    smelt_core::conversation::ConversationSubmitErrorKind::Rejected => {
                        format!("未发送：{}", error.message)
                    }
                    smelt_core::conversation::ConversationSubmitErrorKind::Unknown => {
                        format!("发送结果未知：{}；再次发送可能重复。", error.message)
                    }
                });
            }
        }
        cx.notify();
    }

    fn encode_prompt_for_agent(
        &mut self,
        text: &str,
        images: &[std::sync::Arc<gpui::Image>],
    ) -> (String, Vec<AcpImage>) {
        if images.is_empty() || self.supports_image {
            return (text.to_string(), encode_prompt_images(images));
        }
        let note = format!(
            "[该任务附带 {} 张图片，但 {} 不支持图片输入，图片未转发。]",
            images.len(),
            self.agent.short_label()
        );
        self.paste_hint = Some(note.clone());
        let text = if text.trim().is_empty() {
            note
        } else {
            format!("{text}\n\n{note}")
        };
        (text, Vec::new())
    }

    fn emit_new_session(&self, cx: &mut Context<Self>) {
        cx.emit(AcpViewEvent::NewSession(Box::new(AcpNewSessionRequest {
            agent: self.agent,
            launch: self.launch.clone(),
            profile_id: self.profile_id.clone(),
            cwd: self.cwd.clone(),
        })));
    }

    /// 从这条最终回答分叉：新 Pi 进程 `--fork` 源 session，重放真实历史；
    /// 点击的回答之后若还有用户消息，则把副本切到那条消息之前——新会话
    /// 恰好包含到被点击的回答为止。之后没有用户消息（点的是最后一回合）时
    /// 整份拷贝本身就是所需历史，不设切点。
    fn fork_from_answer(&self, through_index: usize, cx: &mut Context<Self>) {
        if let Some(request) = self.build_pi_fork_request(through_index) {
            cx.emit(AcpViewEvent::ForkConversation(Box::new(request)));
        }
    }

    fn build_pi_fork_request(&self, through_index: usize) -> Option<AcpHandoffRequest> {
        if !smelt_core::session_handoff::live_fork_is_available(self.agent) {
            return None;
        }
        let fork_session_id = self.provider_session_id()?.to_string();
        if fork_session_id.trim().is_empty() {
            return None;
        }
        let title = self
            .auto_title()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "对话".to_string());
        Some(AcpHandoffRequest {
            source: Some(AcpForkOrigin {
                session_id: self.sid.clone(),
                title,
                agent: Some(self.agent.id().to_string()),
                profile_label: None,
                from_history: false,
            }),
            cwd: self.cwd.clone(),
            agent: self.agent,
            launch: self.launch.clone(),
            refresh_launch_from_settings: self.refresh_launch_from_settings,
            profile_id: self.profile_id.clone(),
            config_values: self.current_config_values(),
            ephemeral_env: self.ephemeral_env.clone(),
            prompt: String::new(),
            images: Vec::new(),
            profile_label: None,
            resume_session_id: None,
            fork_session_id: Some(fork_session_id),
            fork_cut: smelt_core::acp_chat::fork_cut_after(&self.entries, through_index)
                .map(|(text, occurrence)| smelt_core::acp_conn::AcpForkCut { text, occurrence }),
            conversation_binding: self.conversation_binding.clone(),
            agent_session: None,
        })
    }

    /// 最后一条 agent 正文（非思考）文本；没有返回 None。
    /// controller 任务回写用它作为 complete 的 output（结构化回复，精准）。
    pub fn last_assistant_text(&self) -> Option<String> {
        self.entries.iter().rev().find_map(|e| match e {
            AcpEntry::Assistant {
                text,
                thought: false,
            } if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        })
    }

    /// provider 侧会话 id（ACP session/new 返回，可用于下次 session/load 恢复）。
    /// controller 回写时作为 complete 的 session_id 回传，供下个任务续跑上下文。
    pub fn provider_session_id(&self) -> Option<SessionId> {
        self.acp_session_id.clone()
    }

    /// 任务等外部调用者需要把 prompt 和一次执行记录严格对应，不能把消息排到
    /// 正在运行的 turn 后面。仅在当前会话可立即发送时返回 true。
    pub fn can_send_prompt_immediately(&self) -> bool {
        can_dispatch_prompt_immediately(
            &self.phase,
            self.prompt_dispatch_pending,
            self.queued_prompts.is_empty(),
            self.handle.is_some(),
        )
    }

    /// 立即发送 prompt（可带图），绝不排队。返回 false 表示会话不再空闲或连接不可用。
    pub fn try_send_prompt_immediately(
        &mut self,
        text: String,
        images: Vec<std::sync::Arc<gpui::Image>>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.try_send_prompt_immediately_with_config(text, images, Vec::new(), cx)
    }

    /// 在已打开的普通 ACP 会话里立即发送 prompt；可选地先切换模型。
    pub fn try_send_prompt_immediately_with_model(
        &mut self,
        text: String,
        images: Vec<std::sync::Arc<gpui::Image>>,
        model_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let config_values = model_id
            .filter(|model| !model.trim().is_empty())
            .map(|model| vec![("model".to_string(), model)])
            .unwrap_or_default();
        self.try_send_prompt_immediately_with_config(text, images, config_values, cx)
    }

    /// 在已打开的普通 ACP 会话里立即发送 prompt，并先应用配置；配置动作和
    /// prompt 走同一条 smeltd action 队列，顺序不会倒置。
    pub fn try_send_prompt_immediately_with_config(
        &mut self,
        text: String,
        images: Vec<std::sync::Arc<gpui::Image>>,
        config_values: Vec<(String, String)>,
        cx: &mut Context<Self>,
    ) -> bool {
        if text.trim().is_empty() || !self.can_send_prompt_immediately() {
            return false;
        }
        self.awaiting_initial_history_snapshot = false;
        // 记住非空配置，让随后强制重启的 adapter 仍按最近一次用户选择恢复。
        if !config_values.is_empty() {
            self.restart_config_values = config_values.clone();
            // 不能只等回合结束才把新配置写进 workspace 存档。
            cx.emit(AcpViewEvent::Changed);
        }
        self.queue_config_values(config_values);
        self.send_prompt_now(&text, &images, cx)
    }

    /// 真正把一条 prompt 打给 smeltd——不碰 `self.pending_images`，图片由调用方
    /// 传入，保证文本和图片 prompt 共用同一套编码/发送逻辑。返回 false 表示连接
    /// 尚未可用，调用方应保留消息而不是静默丢弃。
    fn send_prompt_now(
        &mut self,
        text: &str,
        images: &[std::sync::Arc<gpui::Image>],
        cx: &mut Context<Self>,
    ) -> bool {
        let (text, encoded) = self.encode_prompt_for_agent(text, images);
        let Some(h) = &self.handle else {
            return false;
        };
        if h.action_tx
            .try_send(AcpUserAction::Prompt {
                text,
                images: encoded,
                delivery_id: None,
            })
            .is_err()
        {
            return false;
        }
        self.prompt_dispatch_pending = true;
        // 发送动作已经进入 smeltd 队列后，立刻持久化“首包不再 pending”的状态。
        // 如果此时进程退出，workspace 不能把旧的待发 prompt 恢复出来再次发送。
        cx.emit(AcpViewEvent::Changed);
        cx.notify();
        true
    }

    /// 相位回 Idle 时按顺序取一条排队消息发出去。一次只发一条，避免把整个队列
    /// 一口气打光后又回到协议不支持的裸并发。
    fn flush_queued_prompt(&mut self, cx: &mut Context<Self>) {
        if self.prompt_dispatch_pending || !matches!(self.phase, DaemonPhase::Idle) {
            return;
        }
        let Some((text, images)) = self.queued_prompts.pop_front() else {
            return;
        };
        if !self.send_prompt_now(&text, &images, cx) {
            self.queued_prompts.push_front((text, images));
        }
    }

    /// 把一条排队消息移到队首。这里只改变下一条顺序，**绝不**取消正在进行的
    /// turn；需要中断时用户必须明确点击底部「停止」。
    fn move_queued_prompt_next(&mut self, ix: usize, cx: &mut Context<Self>) {
        if !move_queue_item_to_front(&mut self.queued_prompts, ix) {
            return;
        }

        if matches!(self.phase, DaemonPhase::Idle) {
            self.flush_queued_prompt(cx);
        }
        cx.notify();
    }

    /// 立即发送必须先停止当前 turn；ACP 同一 session 不支持并发 prompt。这个
    /// 动作只由队列项里明确标注的「立即发送（停止当前回答）」触发。
    fn send_queued_prompt_immediately(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.immediate_cancel_pending || !move_queue_item_to_front(&mut self.queued_prompts, ix)
        {
            return;
        }

        if should_cancel_for_immediate_prompt(&self.phase, self.prompt_dispatch_pending) {
            self.immediate_cancel_pending = true;
            self.cancel_turn();
        } else if matches!(self.phase, DaemonPhase::Idle) {
            self.flush_queued_prompt(cx);
        }
        cx.notify();
    }

    /// 原生排队项改成新 prompt：先停当前回合，再按本地队列立即发送。
    /// cancel 会把 Pi 整队还回输入框，所以选中条必须跳过这次还原。
    fn send_native_queue_item_immediately(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.immediate_cancel_pending {
            return;
        }
        let Some(plan) =
            plan_native_immediate_send(&self.queued_steering, &self.queued_follow_up, ix)
        else {
            return;
        };
        self.queued_steering.clear();
        self.queued_follow_up.clear();
        self.skip_next_composer_restore = true;
        if !plan.leftovers.is_empty() {
            match &mut self.pending_composer_restore {
                Some(existing) => existing.extend(plan.leftovers),
                None => self.pending_composer_restore = Some(plan.leftovers),
            }
        }
        self.queued_prompts.push_front((plan.selected, Vec::new()));
        self.send_queued_prompt_immediately(0, cx);
    }

    /// 剪贴板里是图就收进待发列表（返回 true 表示这次粘贴被图片消费掉了，
    /// 调用方据此拦下事件，别再让输入框按文本粘一遍）。macOS 上 CleanShot
    /// 等应用给的是 `ExternalPaths + String`，这类图片文件需要后台读入。
    fn take_clipboard_image(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else {
            return false;
        };

        let has_direct_image = item
            .entries()
            .iter()
            .any(|entry| matches!(entry, gpui::ClipboardEntry::Image(_)));
        // 有原生 Image 时它是同一剪贴板内容的最直接表示，不再读路径，
        // 避免某些平台同时提供两种格式时附加两遍。
        let external_paths = if has_direct_image {
            Vec::new()
        } else {
            external_clipboard_image_paths(item.entries())
        };
        if !has_direct_image && external_paths.is_empty() {
            return false;
        }

        // 能力门：agent 不收图就别收进来（Grok = false）。返回 true 照样吞掉这次
        // 粘贴；ExternalPaths 后面那条路径文本也不能放行，否则会被 agent 当成命令。
        if !self.supports_image {
            self.paste_hint = Some(format!(
                "{} 不支持图片，已忽略粘贴",
                self.agent.short_label()
            ));
            cx.notify();
            return true;
        }

        if has_direct_image {
            self.pending_images
                .extend(item.into_entries().filter_map(|entry| match entry {
                    gpui::ClipboardEntry::Image(image) => Some(std::sync::Arc::new(image)),
                    _ => None,
                }));
            self.paste_hint = None;
            cx.notify();
            return true;
        }

        self.enqueue_external_images(external_paths, cx);
        true
    }

    /// 输入栏「+」：弹系统选文件框。图片进待发缩略图，其它文件/目录按 `@` 提及
    /// 插进输入框——agent 自己有读文件工具，给路径比伪造 ResourceLink 稳。
    ///
    /// 不能在点击回调里 `NSOpenPanel runModal`：那会嵌进 AppKit 模态循环，
    /// 和 GPUI 的事件/Metal 叠在一起直接闪退。走 GPUI 的异步 `prompt_for_paths`。
    fn pick_composer_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some("选择要发送的文件".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            if paths.is_empty() {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.attach_picked_paths(paths, window, cx);
            });
        })
        .detach();
    }

    fn attach_picked_paths(
        &mut self,
        paths: Vec<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (image_paths, file_paths) = classify_attached_paths(&paths, self.supports_image);
        if !file_paths.is_empty() {
            self.insert_prompt_text(
                &format_attached_paths(&file_paths, self.cwd.as_deref()),
                window,
                cx,
            );
        } else {
            self.focus_input(window, cx);
        }
        self.enqueue_external_images(image_paths, cx);
    }

    fn enqueue_external_images(
        &mut self,
        image_paths: Vec<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if image_paths.is_empty() {
            return;
        }
        let requested = image_paths.len();
        self.pending_external_image_loads += requested;
        self.paste_hint = Some(format!("正在读取 {requested} 张图片…"));
        cx.notify();

        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    image_paths
                        .iter()
                        .filter_map(|path| load_external_clipboard_image(path))
                        .collect::<Vec<_>>()
                })
                .await;
            let loaded_count = loaded.len();
            let _ = this.update(cx, |this, cx| {
                this.pending_external_image_loads =
                    this.pending_external_image_loads.saturating_sub(requested);
                this.pending_images.extend(loaded);
                this.paste_hint = if this.pending_external_image_loads > 0 {
                    Some(format!(
                        "正在读取 {} 张图片…",
                        this.pending_external_image_loads
                    ))
                } else if loaded_count == requested {
                    None
                } else if loaded_count == 0 {
                    Some("图片读取失败，请重试".to_string())
                } else {
                    Some(format!(
                        "已附加 {loaded_count} 张图片，{} 张读取失败",
                        requested - loaded_count
                    ))
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// 关闭标签：只摘掉本地连接（`ConversationClientHandle` Drop 会断开 socket），
    /// **不**终止 smeltd 里的会话——跟关一个终端标签不会杀掉底下的 shell 是
    /// 唯一调用方是 `main.rs::close_session`（用户点 × 主动关标签）——那条
    /// 路径本来就跟终端会话共用同一个"用户主动关 = 让守护杀掉底层进程"的
    /// 语气（挨着的 `terminal::kill_remote` 调用是同一个意图），不是"切标签/
    /// 退出 App 这种先不看了"。唯一例外是 daemon-owned 自动化 Run：关闭它的
    /// 临时查看页只能断开 GUI，停止必须走带 Run id 的自动化命令。
    pub fn shutdown(&mut self, _cx: &mut App) {
        // 这是用户明确关闭会话的路径，必须在视图被移除、甚至 App 退出前完成一次
        // 有界的 kill 往返。`kill_acp_session` 自带 5s 读写超时；把它丢进 detached
        // 任务会让进程在任务送达前退出，daemon 里的 ACP 会话就会继续存活。
        if !matches!(
            &self.conversation_binding,
            smelt_core::conversation::ConversationBinding::Automation { .. }
        ) {
            smelt_core::acp_client::kill_acp_session(&self.sid);
        }
        self.handle = None;
    }

    fn submit_input(&mut self, window: &mut Window, follow_up: bool, cx: &mut Context<Self>) {
        let Some(input) = self.input.clone() else {
            return;
        };
        if self.pending_external_image_loads > 0 {
            self.paste_hint = Some(format!(
                "正在读取 {} 张图片，请稍候…",
                self.pending_external_image_loads
            ));
            cx.notify();
            return;
        }
        if self.pending_conversation_input.is_some() {
            return;
        }
        let text = input.read(cx).value().trim().to_string();
        // 只贴了图没打字也要能发。
        if text.is_empty() && self.pending_images.is_empty() {
            return;
        }
        input.update(cx, |s, cx| s.set_value("", window, cx));
        self.input_has_draft = false;
        if self.pending_images.is_empty() && is_new_conversation_command(&text) {
            self.emit_new_session(cx);
            return;
        }
        if follow_up && self.supports_native_queue && self.is_visibly_running() {
            self.send_follow_up(text, window, cx);
            return;
        }
        self.send_prompt(text, window, cx);
    }

    fn send_follow_up(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if text.trim().is_empty() && self.pending_images.is_empty() {
            return;
        }
        let images = std::mem::take(&mut self.pending_images);
        let (text, encoded) = self.encode_prompt_for_agent(&text, &images);
        if self.handle.is_none() {
            if let Some(input) = self.input.clone() {
                input.update(cx, |input, cx| {
                    input.set_value(&text, window, cx);
                    input.focus(window, cx);
                });
            }
            self.pending_images = images;
            return;
        }
        // ⌥↩ 不打断当前回合；先把已选模型写进会话，这条后续消息结束时用新模型。
        for (config_id, value_id) in self.pending_config_values.clone() {
            let _ = self.set_config_option(config_id, value_id);
        }
        let Some(h) = &self.handle else {
            self.pending_images = images;
            return;
        };
        if h.action_tx
            .try_send(AcpUserAction::FollowUp {
                text,
                images: encoded,
                delivery_id: None,
            })
            .is_err()
        {
            self.pending_images = images;
            self.paste_hint = Some("后续消息未能排队".to_string());
            cx.notify();
            return;
        }
        self.paste_hint = None;
        cx.notify();
    }

    fn compact_context(&mut self, cx: &mut Context<Self>) {
        if !self.supports_compaction || self.compacting {
            return;
        }
        let Some(h) = &self.handle else {
            return;
        };
        if h.action_tx.try_send(AcpUserAction::Compact).is_err() {
            self.paste_hint = Some("无法压缩上下文".to_string());
        }
        cx.notify();
    }

    fn clear_native_queue(&mut self, cx: &mut Context<Self>) {
        if !self.supports_native_queue {
            return;
        }
        let Some(h) = &self.handle else {
            return;
        };
        if h.action_tx.try_send(AcpUserAction::ClearQueue).is_err() {
            self.paste_hint = Some("无法清空排队消息".to_string());
        }
        cx.notify();
    }

    /// 回退到某条历史用户消息（仅 Pi）：agent 切到该消息之前，消息原文回输入框。
    /// `entry_index` 是本地列表下标；daemon 的投影才是事实源，这里只传绝对
    /// 下标，验证与同文本序号都由 daemon 侧做。失败只提示——会话状态不变。
    fn rewind_to_message(&mut self, entry_index: usize, cx: &mut Context<Self>) {
        if !self.supports_rewind || self.has_active_turn() {
            return;
        }
        let Some(h) = &self.handle else {
            return;
        };
        if h.action_tx
            .try_send(AcpUserAction::RewindToMessage {
                entry_index: self.loaded_entries_offset + entry_index,
            })
            .is_err()
        {
            self.paste_hint = Some("无法回退到这条消息".to_string());
            cx.notify();
        }
    }

    fn apply_pending_composer_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(texts) = self.pending_composer_restore.take() else {
            return;
        };
        if texts.is_empty() {
            return;
        }
        let Some(input) = self.input.clone() else {
            self.pending_composer_restore = Some(texts);
            return;
        };
        let restored = texts.join("\n\n");
        let current = input.read(cx).value().to_string();
        let merged = if current.trim().is_empty() {
            restored
        } else {
            format!("{restored}\n\n{}", current.trim())
        };
        input.update(cx, |input, cx| {
            input.set_value(&merged, window, cx);
            input.focus(window, cx);
        });
        self.input_has_draft = !merged.trim().is_empty();
    }

    /// 快照应用：整份状态从 smeltd 镜像过来。归约（entries 合并/phase 机/
    /// 回声去重）已经在服务端做完了，这里只做两件事：
    /// 1. 摊平快照字段进本地同名字段，渲染代码不用碰；
    /// 2. 持久化 / 重绘时机跟着快照走。四色状态、Dock 角标、待处理通知都由
    ///    外面的集中状态订阅维护，这里不再自己判相位跳变。
    fn apply_snapshot(&mut self, mut snap: ConversationSnapshot, cx: &mut Context<Self>) {
        if !should_apply_snapshot_revision(self.last_snapshot_revision, snap.snapshot_revision) {
            return;
        }
        if snap.snapshot_revision != 0 {
            self.last_snapshot_revision = snap.snapshot_revision;
        }

        let mut should_persist = snap.should_persist;
        if let Some(conversation_state) = snap.conversation_state.take() {
            let pending_changed =
                self.pending_agent_preset != conversation_state.pending_agent_preset;
            self.pending_agent_preset = conversation_state.pending_agent_preset;
            let binding_changed = conversation_state
                .binding
                .as_ref()
                .is_some_and(|binding| &self.conversation_binding != binding);
            if let Some(binding) = conversation_state.binding {
                self.conversation_binding = binding;
            }
            let agent_session_changed = self.agent_session != conversation_state.agent_session;
            self.agent_session = conversation_state.agent_session;
            should_persist |= pending_changed || binding_changed || agent_session_changed;
        }
        let previous_history_session_id = self.history_session_id.clone();
        let old_entries_len = self.entries.len();
        let replaying_history = snap.replaying_history;
        let initial_snapshot = self.awaiting_initial_history_snapshot;
        let changed_from = merge_snapshot_entries(
            &mut self.entries,
            &mut self.loaded_entries_offset,
            &mut self.entries_total,
            snap.entries_offset,
            snap.entries_total,
            std::mem::take(&mut snap.entries),
            initial_snapshot,
        );
        let snapshot_entries_changed = changed_from.is_some();
        let entries_offset = changed_from.unwrap_or(self.entries.len());
        let previous_permission = self
            .permissions
            .first()
            .map(|card| (card.tool_call_id.clone(), card.question.clone()));
        if snapshot_entries_changed {
            refresh_markdown_cache(
                &self.entries,
                entries_offset,
                self.cwd.as_deref(),
                &mut self.rendered_markdown,
            );
            self.rendered_images
                .retain(|(entry_ix, _), _| *entry_ix < entries_offset);
            self.rendered_images.extend(decode_entry_images(
                &self.entries[entries_offset..],
                entries_offset,
            ));
            for id in self.entries[entries_offset..]
                .iter()
                .filter_map(|entry| match entry {
                    AcpEntry::ToolCall { id, .. } => Some(id),
                    _ => None,
                })
            {
                self.rendered_diffs.remove(id);
                self.rendered_tool_images.remove(id);
            }
        }
        let new_entries_len = self.entries.len();
        let seed_restored_height_hints = should_seed_restored_height_hints(
            self.awaiting_initial_history_snapshot,
            replaying_history,
            self.list_state.is_following_tail(),
            snapshot_entries_changed,
            old_entries_len,
            new_entries_len,
        );
        if seed_restored_height_hints {
            self.list_state
                .reset_with_uniform_height(new_entries_len, px(RESTORED_ENTRY_HEIGHT_HINT_PX));
        } else if let Some(entries_offset) = changed_from {
            // splice 会把落在被替换 item 内部的滚动锚点重置到该 item 顶部。
            // 流式正文持续替换最后一项时，用户若正在这项里向上浏览，就会每个
            // chunk 被拉回一次，形成明显抖动。离开尾随态后保留逻辑锚点；正文
            // 只在锚点下方增长，原 offset 仍然代表同一块可见内容。
            let scroll_anchor = (!self.list_state.is_following_tail())
                .then(|| self.list_state.logical_scroll_top());
            self.list_state.splice(
                entries_offset..old_entries_len,
                new_entries_len.saturating_sub(entries_offset),
            );
            if let Some(anchor) = scroll_anchor {
                self.list_state.scroll_to(anchor);
            }
        }
        if new_entries_len > 0 {
            self.awaiting_initial_history_snapshot = false;
        }
        let snapshot_phase = snap.phase;
        self.phase = snap.phase;
        self.end_reason = snap.end_reason;
        self.end_kind = snap.end_kind;
        // 只有 daemon 已回到可用相位才补满预算。Starting 仍是同一次重连尝试，
        // 若在这里补满会让启动失败形成无限重试。
        if is_recovered_phase(&self.phase) {
            self.auto_reconnect_left = AUTO_RECONNECT_ATTEMPTS;
        }
        self.permissions = snap.pending_permissions;
        let current_permission = self
            .permissions
            .first()
            .map(|card| (card.tool_call_id.clone(), card.question.clone()));
        if previous_permission != current_permission {
            self.permission_submitting = None;
        }
        self.elicitation = snap.pending_elicitation;
        if self.elicitation.is_none() {
            self.elicitation_inputs.clear();
        }
        let previous_status_line = self.status_line.clone();
        self.status_line = snap.status_line;
        if config_update_failure_is_new(
            previous_status_line.as_deref(),
            self.status_line.as_deref(),
        ) {
            self.pending_config_values.clear();
        }
        if should_replace_session_title(snap.session_title.is_some(), snapshot_entries_changed) {
            self.session_title = snap.session_title;
        }
        let has_runtime_session_id = snap.acp_session_id.is_some();
        let runtime_session_id = snap.acp_session_id.map(SessionId::new);
        self.acp_session_id = runtime_session_id.clone();
        if let Some(history_session_id) = snap.history_session_id {
            self.history_session_id = Some(SessionId::new(history_session_id));
        } else if should_clear_history_session_id_after_snapshot(
            &snapshot_phase,
            new_entries_len == 0,
            snap.snapshot_revision,
            has_runtime_session_id,
        ) {
            // 只有 Ready/Fresh 后的 Idle 空快照才证明新会话已经真正建立；
            // Starting/Ended 仍可能只是恢复失败或连接超时，必须保留 canonical
            // history id，让下一次打开还能重试原会话。
            self.history_session_id = None;
        } else if self.history_session_id.is_none() && new_entries_len > 0 {
            // 兼容尚未携带 history_session_id 的旧 daemon 快照，但不要把
            // 空白会话的运行时 id 当成可恢复历史身份。
            self.history_session_id = runtime_session_id;
        }
        self.supports_image = snap.supports_image;
        self.available_commands = snap.available_commands;
        self.usage = snap.usage;
        self.usage_cached_read = snap.usage_cached_read;
        self.usage_cost = snap.usage_cost;
        self.usage_breakdown = snap.usage_breakdown.clone();
        self.supports_compaction = snap.supports_compaction;
        self.supports_native_queue = snap.supports_native_queue;
        self.supports_rewind = snap.supports_rewind;
        self.compacting = snap.compacting;
        let skip_restore = self.skip_next_composer_restore;
        (self.queued_steering, self.queued_follow_up) =
            native_queue_from_snapshot(skip_restore, snap.queued_steering, snap.queued_follow_up);
        let restore = consume_composer_restore(
            self.last_composer_restore_revision,
            snap.composer_restore_revision,
            snap.composer_restore_texts,
            skip_restore,
        );
        self.last_composer_restore_revision = restore.last_revision;
        self.skip_next_composer_restore = restore.skip_next;
        if let Some(texts) = restore.restore_texts {
            self.pending_composer_restore = Some(texts);
        }
        self.plan = snap.plan;
        self.model = snap.model;
        self.config_options = snap.config_options;
        reconcile_pending_config_values(
            &mut self.pending_config_values,
            &self.config_options,
            self.model.as_ref(),
        );
        self.turn_started_at_ms = snap.turn_started_at_ms;
        self.turn_timings = snap.turn_timings;
        self.turn_outcome = snap.turn_outcome;
        self.accepted_delivery_ids = snap.accepted_delivery_ids;
        self.active_delivery_id = snap.active_delivery_id;
        self.completed_delivery_id = snap.completed_delivery_id;
        // note_prompt_sent 会先把 Running 快照推给 GUI，再开始真正的 ACP RPC。
        // 只有收到这份确认后才允许下一条排队消息等待 Idle；不能用旧的 Idle 快照
        // 清掉 pending 标记，否则快速连续提交仍会并发打进同一个 session。
        // 「立即发送」取消成功后的 Idle 是例外：那条 dispatch 不会再有 Running 回执。
        let gate = next_snapshot_prompt_gate(
            &self.phase,
            self.turn_started_at_ms,
            self.prompt_dispatch_pending,
            self.immediate_cancel_pending,
            self.queued_prompts.is_empty(),
            self.turn_outcome,
        );
        self.prompt_dispatch_pending = gate.prompt_dispatch_pending;
        self.immediate_cancel_pending = gate.immediate_cancel_pending;
        // 完成边沿以 TurnEnded 为准：回合明确成功、无人等待即可。未完成工具
        // 只影响展示，迟到终态按 tool id 回写，不再挡住 CompletedTurn。
        let waiting_on_user = !self.permissions.is_empty() || self.elicitation.is_some();
        let succeeded = matches!(self.turn_outcome, Some(AcpTurnOutcome::Succeeded) | None);
        let completed = snap.completed_unread
            && !self.was_completed_unread
            && !waiting_on_user
            && succeeded
            && matches!(self.phase, DaemonPhase::Idle);
        let failed_turn = snap.completed_unread
            && !self.was_completed_unread
            && !waiting_on_user
            && matches!(self.phase, DaemonPhase::Idle)
            && self
                .turn_outcome
                .is_some_and(|outcome| outcome.failure_message().is_some());
        let failure_message = failed_turn
            .then(|| self.turn_outcome.and_then(AcpTurnOutcome::failure_message))
            .flatten()
            .map(String::from);
        // 失败/取消不触发 CompletedTurn，但仍消费一次，避免后续明细快照重复发边沿。
        self.was_completed_unread =
            snap.completed_unread && !waiting_on_user && matches!(self.phase, DaemonPhase::Idle);
        let ended_msg = (self.phase == DaemonPhase::Dead).then(|| self.end_reason.clone());
        let became_ended = ended_msg.is_some() && !self.was_ended;
        // 从 Ended 恢复（自动重连 / 手动重启 / GUI 重开 attach）：绑定任务从
        // 「重连中」回执行中，不能继续按失败收尾。
        let became_recovered = did_recover_from_ended(self.was_ended, &self.phase);
        // Ended -> Starting 只是重连尝试已经发起，还不是恢复成功。保留 ended
        // 标记，直到收到可用相位；若启动再次失败，原重连循环可继续消费预算。
        self.was_ended = ended_msg.is_some()
            || (self.was_ended && matches!(self.phase, DaemonPhase::Connecting));
        self.prune_tool_ui_state();

        if matches!(self.phase, DaemonPhase::Dead) {
            self.handle = None;
        }

        if completed {
            cx.emit(AcpViewEvent::CompletedTurn {
                delivery_id: self.completed_delivery_id.clone(),
            });
        }
        if let Some(reason) = failure_message {
            cx.emit(AcpViewEvent::FailedTurn {
                reason,
                delivery_id: self.completed_delivery_id.clone(),
            });
        }
        if became_ended {
            cx.emit(AcpViewEvent::Ended {
                kind: self.end_kind,
                reason: ended_msg.unwrap(),
                delivery_id: self.active_delivery_id.clone(),
            });
        }
        if became_recovered {
            cx.emit(AcpViewEvent::Recovered);
        }

        if matches!(self.phase, DaemonPhase::Idle) {
            let initial_config = std::mem::take(&mut self.pending_initial_config);
            self.queue_config_values(initial_config);
            if let Some(prompt) = self.pending_initial_prompt.take() {
                self.awaiting_initial_history_snapshot = false;
                let images = std::mem::take(&mut self.pending_images);
                if self.prompt_dispatch_pending || !self.send_prompt_now(&prompt, &images, cx) {
                    self.pending_initial_prompt = Some(prompt);
                    self.pending_images = images;
                }
            } else if gate.should_flush_queue {
                // 交接提示和排队消息不会同时出现（前者只在全新 fork 会话里用），
                // 分支互斥即可：这轮 Idle 只发队首一条，剩下的等下一次 Idle。
                self.flush_queued_prompt(cx);
            }
        }

        if should_persist || self.history_session_id != previous_history_session_id {
            cx.emit(AcpViewEvent::Changed);
        }
        if self.has_active_turn() {
            self.ensure_running_tick(cx);
        }
        cx.notify();
    }

    /// 快照是全量覆盖，历史重放 / 新会话可能清空旧 entries；把只属于本地 UI
    /// 的工具展开状态同步裁剪掉，避免长会话来回续接后集合无限长。
    fn prune_tool_ui_state(&mut self) {
        let mut live_ids = std::collections::HashSet::new();
        smelt_core::acp_chat::for_each_tool_id(&self.entries, |id| {
            live_ids.insert(id.to_string());
        });
        self.rendered_diffs.retain(|id, _| live_ids.contains(id));
        self.expanded_tools.retain(|id| live_ids.contains(id));
        self.expanded_tool_cards.retain(|id| live_ids.contains(id));
        self.collapsed_tool_cards.retain(|id| live_ids.contains(id));
        self.expanded_thoughts.retain(|ix| *ix < self.entries.len());
        self.expanded_process_groups
            .retain(|ix| *ix < self.entries.len());
    }

    fn ensure_diff_cache_for_entry(&mut self, entry_ix: usize) {
        let Some(AcpEntry::ToolCall { id, output, .. }) = self.entries.get(entry_ix) else {
            return;
        };
        let id = id.clone();
        if self
            .rendered_diffs
            .get(&id)
            .is_some_and(|parts| diff_cache_matches_output(parts, output))
        {
            return;
        }
        let parts = build_diff_parts(output);
        self.rendered_diffs.insert(id, parts);
    }

    /// 工具输出图片的解码缓存。图片块是最终结果（不会就地变更），形状变了就整条重建。
    fn ensure_tool_image_cache_for_entry(&mut self, entry_ix: usize) {
        let Some(AcpEntry::ToolCall { id, output, .. }) = self.entries.get(entry_ix) else {
            return;
        };
        if !output
            .iter()
            .any(|part| matches!(part, ToolOutputPart::Image(_)))
        {
            return;
        }
        let id = id.clone();
        if self
            .rendered_tool_images
            .get(&id)
            .is_some_and(|parts| tool_image_cache_matches_output(parts, output))
        {
            return;
        }
        let parts = build_tool_image_parts(output);
        self.rendered_tool_images.insert(id, parts);
    }

    fn tool_card_is_expanded(
        &self,
        id: &str,
        has_expandable_content: bool,
        default_expanded: bool,
    ) -> bool {
        if !has_expandable_content {
            return false;
        }
        if self.expanded_tool_cards.contains(id) {
            return true;
        }
        if self.collapsed_tool_cards.contains(id) {
            return false;
        }
        default_expanded || tool_card_default_expanded()
    }

    fn toggle_tool_card(
        &mut self,
        entry_ix: usize,
        id: String,
        has_expandable_content: bool,
        default_expanded: bool,
        cx: &mut Context<Self>,
    ) {
        if !has_expandable_content {
            return;
        }
        if self.tool_card_is_expanded(&id, has_expandable_content, default_expanded) {
            self.expanded_tool_cards.remove(&id);
            self.collapsed_tool_cards.insert(id);
        } else {
            self.collapsed_tool_cards.remove(&id);
            self.expanded_tool_cards.insert(id);
        }
        self.list_state
            .remeasure_items(entry_ix..entry_ix.saturating_add(1));
        cx.notify();
    }

    fn tool_run_is_open(&self, start: usize) -> bool {
        self.expanded_tool_runs.contains(&start)
    }

    fn toggle_tool_run(&mut self, start: usize, cx: &mut Context<Self>) {
        if !self.expanded_tool_runs.remove(&start) {
            self.expanded_tool_runs.insert(start);
        }
        self.list_state
            .remeasure_items(start..start.saturating_add(1));
        cx.notify();
    }

    /// 选择题点选：动作发给 smeltd（真正的选中/提交状态机在服务端跑，见
    /// `smelt_core::acp_session::choose_elicitation`），下一份快照回来就带着
    /// 更新后的 `chosen`——本地 socket 往返够快，不用做乐观本地更新，免得
    /// 跟服务端真相分叉。单字段单选点了自动追发一条 Submit，跟服务端那边
    /// 「整卡只有一个单选字段，点了就是答案」的判断保持一致。
    fn pick_elicit_option(&mut self, field_ix: usize, opt_ix: usize, _cx: &mut Context<Self>) {
        let Some(h) = &self.handle else { return };
        let _ = h
            .action_tx
            .try_send(AcpUserAction::ElicitationChoose { field_ix, opt_ix });
        let single_select = self.elicitation.as_ref().is_some_and(|card| {
            card.fields.len() == 1 && matches!(card.fields[0].kind, ElicitFieldKindView::Select(_))
        });
        if single_select {
            let _ = h.action_tx.try_send(AcpUserAction::ElicitationSubmit);
        }
    }

    /// 所有必填字段都有值后才可提交（可选字段留空不会阻塞）。
    fn elicit_ready(&self, cx: &App) -> bool {
        self.elicitation.as_ref().is_some_and(|card| {
            card.fields.iter().enumerate().all(|(ix, field)| {
                !field.required
                    || match &field.kind {
                        ElicitFieldKindView::Text { .. } => self
                            .elicitation_inputs
                            .get(&ix)
                            .is_some_and(|input| !input.read(cx).value().trim().is_empty()),
                        ElicitFieldKindView::ExternalUrl(_) => true,
                        // 题目卡：点了选项或自己写了答案都算就绪。
                        ElicitFieldKindView::Select(_) | ElicitFieldKindView::MultiSelect(_)
                            if field.allow_custom_input =>
                        {
                            card.chosen.get(&ix).is_some_and(|sel| !sel.is_empty())
                                || self
                                    .elicitation_inputs
                                    .get(&ix)
                                    .is_some_and(|input| !input.read(cx).value().trim().is_empty())
                        }
                        _ => card.chosen.get(&ix).is_some_and(|sel| !sel.is_empty()),
                    }
            })
        })
    }

    fn submit_elicitation(&mut self, cx: &mut Context<Self>) {
        if let Some(h) = &self.handle {
            for (&field_ix, input) in &self.elicitation_inputs {
                let _ = h.action_tx.try_send(AcpUserAction::ElicitationText {
                    field_ix,
                    value: input.read(cx).value().to_string(),
                });
            }
            let _ = h.action_tx.try_send(AcpUserAction::ElicitationSubmit);
        }
    }

    /// 「跳过」：丢弃卡片（服务端那边的 responder Drop 自动回 Cancel），继续
    /// 文本对话。
    fn dismiss_elicitation(&mut self, _cx: &mut Context<Self>) {
        if let Some(h) = &self.handle {
            let _ = h.action_tx.try_send(AcpUserAction::ElicitationDismiss);
        }
    }

    /// 当前模型的人类可读名（舞台头显示用）；None = agent 没上报过。
    pub fn model_name(&self) -> Option<String> {
        self.model.as_ref().map(|model| {
            let displayed = overlay_model_state(model, &self.pending_config_values);
            model_label_with_provider(&displayed.current_value, &displayed.current_name)
        })
    }

    /// 当前上下文已用 token 数（舞台头显示用）；None = agent 没上报过用量。
    /// 跟输入栏「上下文 %」胶囊同一个数据源，只是这里要精确数字而非百分比。
    pub fn context_tokens_used(&self) -> Option<u64> {
        self.usage.map(|(used, _)| used)
    }

    /// 写回 agent 上报的会话配置。四个 agent 共用 ACP 的标准接口。
    fn set_config_option(&mut self, config_id: String, value_id: String) -> bool {
        let Some(h) = &self.handle else {
            return false;
        };
        let boolean = self
            .config_options
            .iter()
            .find(|config| config.config_id == config_id)
            .and_then(|config| config.boolean.map(|_| value_id == "true"));
        if h.action_tx
            .try_send(AcpUserAction::SetConfigOption {
                config_id: config_id.clone(),
                value_id: value_id.clone(),
                boolean,
            })
            .is_err()
        {
            return false;
        }
        self.remember_config_value(config_id, value_id);
        true
    }

    /// 输入栏菜单的用户选择。模型统一用语义 id `model` 记忆，下一次握手后再映射
    /// 到该 adapter 实际上报的 config id；其它配置保留 provider 原始 id。
    fn select_config_option(
        &mut self,
        config_id: String,
        value_id: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let memory_config_id = if config_id == "model"
            || self
                .model
                .as_ref()
                .is_some_and(|model| model.config_id == config_id)
        {
            "model".to_string()
        } else {
            config_id.clone()
        };
        overlay_pending_initial_config(
            &mut self.pending_initial_config,
            memory_config_id.clone(),
            value_id.clone(),
        );
        let sent = self.set_config_option(config_id.clone(), value_id.clone());
        let confirmed =
            confirmed_config_value(&config_id, &self.config_options, self.model.as_ref());
        apply_pending_config_selection(
            &mut self.pending_config_values,
            config_id,
            value_id.clone(),
            confirmed,
        );
        if sent {
            cx.emit(AcpViewEvent::ConfigSelected {
                config_id: memory_config_id,
                value_id,
            });
        }
        cx.notify();
        sent
    }

    /// 记住一项已发给 adapter 的配置。模型在不同 adapter 上可能使用不同的
    /// config id（网页任务会先用语义名 `model`），因此新增模型值时同时清掉
    /// 语义名和当前 adapter 的真实 id，避免恢复时重复发送两份模型配置。
    fn remember_config_value(&mut self, config_id: String, value_id: String) {
        let model_config_id = self.model.as_ref().map(|model| model.config_id.clone());
        let is_model = config_id == "model" || model_config_id.as_deref() == Some(&config_id);
        if is_model {
            self.restart_config_values
                .retain(|(id, _)| id != "model" && model_config_id.as_deref() != Some(id.as_str()));
            self.restart_config_values
                .insert(0, ("model".to_string(), value_id));
            return;
        }

        if let Some(existing) = self
            .restart_config_values
            .iter_mut()
            .find(|(id, _)| id == &config_id)
        {
            existing.1 = value_id;
        } else {
            self.restart_config_values.push((config_id, value_id));
        }
    }

    /// 把语义化的初始配置变成 agent 实际暴露的 config id。`model` 是唯一跨
    /// adapter 的语义占位名；其余配置（如 `mode` / `reasoning_effort`）必须在
    /// 当前握手广告的选项里出现才发送，避免把过期网页配置打成 ACP 错误。
    fn queue_config_values(&mut self, values: impl IntoIterator<Item = (String, String)>) {
        for (config_id, value_id) in values {
            let actual_config_id = if config_id == "model" {
                if let Some(model) = &self.model {
                    if !model.options.is_empty()
                        && !model.options.iter().any(|(value, _)| value == &value_id)
                    {
                        eprintln!("[acp] 忽略未被 agent 接受的初始模型 {value_id}");
                        continue;
                    }
                    model.config_id.clone()
                } else {
                    config_id.clone()
                }
            } else {
                config_id.clone()
            };
            let Some(config) = self
                .config_options
                .iter()
                .find(|config| config.config_id == actual_config_id)
            else {
                // model 由独立的 `ModelState` 描述，未必也放在 config_options。
                // 其余项目则必须由本次握手明确支持，不能把别家 adapter 的私有
                // id 盲发过去。
                if config_id != "model" || self.model.is_none() {
                    eprintln!("[acp] 当前 agent 不支持初始配置 {actual_config_id}");
                    continue;
                }
                self.set_config_option(actual_config_id, value_id);
                continue;
            };
            if !config.options.is_empty()
                && !config.options.iter().any(|(value, _)| value == &value_id)
            {
                eprintln!("[acp] 忽略未被 agent 接受的初始配置 {actual_config_id}");
                continue;
            }
            self.set_config_option(actual_config_id, value_id);
        }
    }

    /// PLAN 条：agent 上报的任务计划 → 消息流上方的可折叠进度条。
    /// 折叠 = 一行摘要 + 进度条；展开 = 三态步骤清单（对齐设计稿）。
    /// 只借 `&Context`（listener 不需要可变借用），render 里跟 theme 引用共存。
    fn render_plan_bar(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let plan = self.plan.as_ref()?;
        let total = plan.entries.len();
        if total == 0 {
            return None;
        }
        let done = plan
            .entries
            .iter()
            .filter(|e| matches!(e.status, PlanEntryStatusView::Completed))
            .count();
        let in_progress = plan
            .entries
            .iter()
            .filter(|e| matches!(e.status, PlanEntryStatusView::InProgress))
            .count();
        // 正在跑的算当前步；全完成就是 n / n。
        let current = (done + in_progress).min(total);
        let (summary, summary_color) = if done == total {
            (
                format!("{total} / {total} · 完成"),
                gpui::rgb(ui_theme::green()),
            )
        } else if in_progress > 0 {
            (
                format!("{current} / {total}"),
                gpui::rgb(ui_theme::accent()),
            )
        } else {
            (
                format!("{done} / {total}"),
                gpui::rgb(ui_theme::text_muted()),
            )
        };
        let current_step = plan_current_step(plan).unwrap_or("等待下一步").to_string();
        let progress = (done as f32 + in_progress as f32 * 0.5) / total as f32;

        let mut bar = gpui_component::v_flex()
            .border_b_1()
            .border_color(gpui::rgb(ui_theme::border_dim()))
            .bg(gpui::rgb(ui_theme::bg_status()))
            .child(
                h_flex()
                    .id("acp-plan-toggle")
                    .px_4()
                    .py_2()
                    .gap_2p5()
                    .items_center()
                    .cursor_pointer()
                    .hover(|d| d.bg(ui_theme::overlay(0x14)))
                    .active(|d| d.opacity(0.88))
                    .on_click(cx.listener(|this, _ev, _window, cx| {
                        this.plan_collapsed = !this.plan_collapsed;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(px(10.))
                            .text_xs()
                            .text_color(gpui::rgb(ui_theme::text_muted()))
                            .child(if self.plan_collapsed { "▸" } else { "▾" }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(gpui::rgb(ui_theme::text_mid()))
                            .child("任务进度"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(gpui::rgb(ui_theme::text_bright()))
                            .child(current_step),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .font_family(smelt_core::font_config::font_family())
                            .text_color(summary_color)
                            .child(summary),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(px(120.))
                            .h(px(5.))
                            .rounded_full()
                            .bg(gpui::rgb(ui_theme::border_dim()))
                            .overflow_hidden()
                            .child(
                                div()
                                    .w(gpui::relative(progress.clamp(0., 1.)))
                                    .h_full()
                                    .bg(gpui::rgb(ui_theme::accent())),
                            ),
                    ),
            );
        if !self.plan_collapsed {
            let mut steps = gpui_component::v_flex().px_4().pb_3().gap_0p5();
            for entry in &plan.entries {
                let row = h_flex().gap_2p5().items_center().py_0p5();
                let row = match entry.status {
                    PlanEntryStatusView::Completed => row
                        .child(
                            div()
                                .flex_shrink_0()
                                .size(px(15.))
                                .rounded_sm()
                                .bg(gpui::rgb(ui_theme::green()))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_xs()
                                .text_color(gpui::rgb(ui_theme::on_accent()))
                                .child("✓"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(gpui::rgb(ui_theme::text_faint()))
                                .line_through()
                                .child(entry.content.clone()),
                        ),
                    PlanEntryStatusView::InProgress => row
                        .child(
                            div()
                                .flex_shrink_0()
                                .size(px(15.))
                                .rounded_sm()
                                .border_1()
                                .border_color(gpui::rgb(ui_theme::accent()))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .size(px(7.))
                                        .rounded_xs()
                                        .bg(gpui::rgb(ui_theme::accent())),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_medium()
                                        .text_color(gpui::rgb(ui_theme::text_bright()))
                                        .child(entry.content.clone()),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(gpui::rgb(ui_theme::accent()))
                                        .child("· 进行中"),
                                ),
                        ),
                    // Pending 与协议未来的新状态都按「待做」渲染。
                    _ => row
                        .child(
                            div()
                                .flex_shrink_0()
                                .size(px(15.))
                                .rounded_sm()
                                .border_1()
                                .border_color(gpui::rgb(ui_theme::border_focus())),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(gpui::rgb(ui_theme::text_mid()))
                                .child(entry.content.clone()),
                        ),
                };
                steps = steps.child(row);
            }
            bar = bar.child(steps);
        }
        Some(bar.into_any_element())
    }

    /// ⌘⏎ 快捷批准：选第一个 allow 类选项（跟绿色主按钮同一目标）。
    fn pick_permission_primary(&mut self, cx: &mut Context<Self>) {
        let Some(card) = self.permissions.first() else {
            return;
        };
        let Some(pix) = card.options.iter().position(|o| {
            matches!(
                o.kind,
                PermissionOptionKindView::AllowOnce | PermissionOptionKindView::AllowAlways
            )
        }) else {
            return;
        };
        let option_id = card.options[pix].option_id.clone();
        let tool_call_id = card.tool_call_id.clone();
        self.pick_permission(&tool_call_id, &option_id, cx);
    }

    /// 审批按钮：把选中项发给 smeltd（真正消费 responder 回 RPC 是服务端的
    /// 事），卡片收起、相位回 Running 等下一份快照即可，不用本地抢跑。
    fn pick_permission(&mut self, tool_call_id: &str, option_id: &str, cx: &mut Context<Self>) {
        // ACP agent 可能一次发来多条请求，但实际执行仍按队列推进。只允许回应
        // 队首，避免上一帧残留的按钮或其它入口越过当前审批。
        if self.permission_submitting.is_some()
            || !is_active_permission_selection(&self.permissions, tool_call_id, option_id)
        {
            return;
        }
        let tool_call_id = tool_call_id.to_string();
        let option_id = option_id.to_string();
        if let Some(h) = &self.handle
            && h.action_tx
                .try_send(AcpUserAction::PermissionSelect {
                    tool_call_id: tool_call_id.clone(),
                    option_id: option_id.clone(),
                })
                .is_ok()
        {
            self.permission_submitting = Some((tool_call_id, option_id));
            cx.notify();
        }
    }
}

/// 折叠计划也要告诉用户“现在具体在做什么”，不能只剩一个 2/4。没有显式
/// InProgress 时回退到下一条 Pending；全完成则显示最后一项。
fn plan_current_step(plan: &PlanView) -> Option<&str> {
    plan.entries
        .iter()
        .find(|entry| matches!(entry.status, PlanEntryStatusView::InProgress))
        .or_else(|| {
            plan.entries
                .iter()
                .find(|entry| matches!(entry.status, PlanEntryStatusView::Pending))
        })
        .or_else(|| plan.entries.last())
        .map(|entry| entry.content.as_str())
        .filter(|content| !content.trim().is_empty())
}

impl Focusable for AcpView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// macOS 剪贴板会在图片来自文件时先返回 `ExternalPaths`，后跟一份
/// 路径文本。只识别 GPUI/ACP 已支持编码的图片扩展名，其他文件继续走普通
/// 文本粘贴。
fn classify_attached_paths(
    paths: &[std::path::PathBuf],
    attach_images: bool,
) -> (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) {
    let mut images = Vec::new();
    let mut files = Vec::new();
    for path in paths {
        if attach_images && external_image_format(path).is_some() {
            images.push(path.clone());
        } else {
            files.push(path.clone());
        }
    }
    (images, files)
}

fn format_attached_path(path: &std::path::Path, cwd: Option<&str>) -> String {
    let rendered = cwd
        .map(std::path::Path::new)
        .and_then(|cwd| path.strip_prefix(cwd).ok())
        .map(|rel| {
            let rel = rel.to_string_lossy();
            if rel.is_empty() {
                path.to_string_lossy().into_owned()
            } else {
                format!("@{rel}")
            }
        })
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    if rendered.chars().any(char::is_whitespace) {
        format!("\"{rendered}\"")
    } else {
        rendered
    }
}

fn format_attached_paths(paths: &[std::path::PathBuf], cwd: Option<&str>) -> String {
    paths
        .iter()
        .map(|path| format_attached_path(path, cwd))
        .collect::<Vec<_>>()
        .join(" ")
}

fn external_clipboard_image_paths(entries: &[gpui::ClipboardEntry]) -> Vec<std::path::PathBuf> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            gpui::ClipboardEntry::ExternalPaths(paths) => Some(paths.paths()),
            _ => None,
        })
        .flatten()
        .filter(|path| external_image_format(path).is_some())
        .cloned()
        .collect()
}

fn external_image_format(path: &std::path::Path) -> Option<gpui::ImageFormat> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some(gpui::ImageFormat::Png),
        "jpg" | "jpeg" => Some(gpui::ImageFormat::Jpeg),
        "webp" => Some(gpui::ImageFormat::Webp),
        "gif" => Some(gpui::ImageFormat::Gif),
        "svg" => Some(gpui::ImageFormat::Svg),
        "bmp" => Some(gpui::ImageFormat::Bmp),
        "tif" | "tiff" => Some(gpui::ImageFormat::Tiff),
        _ => None,
    }
}

fn load_external_clipboard_image(path: &std::path::Path) -> Option<std::sync::Arc<gpui::Image>> {
    let format = external_image_format(path)?;
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some(std::sync::Arc::new(gpui::Image::from_bytes(format, bytes)))
}

/// GPUI 剪贴板图片格式 → 协议要的 MIME。
fn image_mime(format: gpui::ImageFormat) -> &'static str {
    match format {
        gpui::ImageFormat::Png => "image/png",
        gpui::ImageFormat::Jpeg => "image/jpeg",
        gpui::ImageFormat::Webp => "image/webp",
        gpui::ImageFormat::Gif => "image/gif",
        gpui::ImageFormat::Svg => "image/svg+xml",
        gpui::ImageFormat::Bmp => "image/bmp",
        gpui::ImageFormat::Tiff => "image/tiff",
        // 协议字段是必填的字符串，认不出的格式给个通用值让 agent 自己嗅探，
        // 总好过不发（ImageFormat 是 #[non_exhaustive]，会长新枝）。
        _ => "application/octet-stream",
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// 待发 GPUI 图片 → ACP/插件输入路由共用的可传输表示。
///
/// 普通 ACP 直发与插件输入路由必须用同一套编码，避免前者带图、后者只留下
/// 缩略图却在后台丢字节。
fn encode_prompt_images(images: &[std::sync::Arc<gpui::Image>]) -> Vec<AcpImage> {
    images
        .iter()
        .map(|im| AcpImage {
            mime: image_mime(im.format).to_string(),
            data_b64: base64_encode(&im.bytes),
        })
        .collect()
}

fn decode_acp_image(image: &AcpImage) -> Option<std::sync::Arc<gpui::Image>> {
    use base64::Engine as _;

    let format = match image.mime.as_str() {
        "image/png" => gpui::ImageFormat::Png,
        "image/jpeg" => gpui::ImageFormat::Jpeg,
        "image/webp" => gpui::ImageFormat::Webp,
        "image/gif" => gpui::ImageFormat::Gif,
        "image/svg+xml" => gpui::ImageFormat::Svg,
        "image/bmp" => gpui::ImageFormat::Bmp,
        "image/tiff" => gpui::ImageFormat::Tiff,
        _ => return None,
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&image.data_b64)
        .ok()?;
    Some(std::sync::Arc::new(gpui::Image::from_bytes(format, bytes)))
}

fn decode_entry_images(
    entries: &[AcpEntry],
    offset: usize,
) -> std::collections::HashMap<(usize, usize), std::sync::Arc<gpui::Image>> {
    entries
        .iter()
        .enumerate()
        .flat_map(|(entry_ix, entry)| match entry {
            AcpEntry::UserWithImages { images, .. } => images
                .iter()
                .enumerate()
                .filter_map(move |(image_ix, image)| {
                    decode_acp_image(image).map(|decoded| ((offset + entry_ix, image_ix), decoded))
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect()
}

fn build_diff_parts(output: &[ToolOutputPart]) -> Vec<Option<CachedDiff>> {
    output
        .iter()
        .map(|part| match part {
            ToolOutputPart::Diff {
                old_text, new_text, ..
            } => {
                let full = diff_lines(old_text.as_deref().unwrap_or(""), new_text);
                let added = full
                    .iter()
                    .filter(|line| line.tag == DiffLineTag::Added)
                    .count();
                let removed = full
                    .iter()
                    .filter(|line| line.tag == DiffLineTag::Removed)
                    .count();
                Some(CachedDiff {
                    lines: std::rc::Rc::new(compact_diff_lines(&full, 3)),
                    added,
                    removed,
                })
            }
            ToolOutputPart::Text(_) => None,
            ToolOutputPart::Image(_) => None,
            ToolOutputPart::Terminal { .. } => None,
        })
        .collect()
}

fn diff_cache_matches_output(cached: &[Option<CachedDiff>], output: &[ToolOutputPart]) -> bool {
    cached.len() == output.len()
        && cached.iter().zip(output).all(|(cached, part)| {
            matches!(
                (part, cached),
                (ToolOutputPart::Diff { .. }, Some(_))
                    | (ToolOutputPart::Text(_), None)
                    | (ToolOutputPart::Image(_), None)
                    | (ToolOutputPart::Terminal { .. }, None)
            )
        })
}

/// 工具输出 → 与 `output` 下标对齐的解码图片（非图片块为 None）。
fn build_tool_image_parts(output: &[ToolOutputPart]) -> Vec<Option<std::sync::Arc<gpui::Image>>> {
    output
        .iter()
        .map(|part| match part {
            ToolOutputPart::Image(image) => decode_acp_image(image),
            _ => None,
        })
        .collect()
}

fn tool_image_cache_matches_output(
    cached: &[Option<std::sync::Arc<gpui::Image>>],
    output: &[ToolOutputPart],
) -> bool {
    cached.len() == output.len()
        && cached.iter().zip(output).all(|(cached, part)| {
            // 解码失败的图片块缓存为 None，所以只校验「非图片块一定是 None」。
            matches!(part, ToolOutputPart::Image(_)) || cached.is_none()
        })
}

fn cached_diff_stats(parts: Option<&[Option<CachedDiff>]>) -> Option<(usize, usize)> {
    let mut added = 0;
    let mut removed = 0;
    let mut has_diff = false;
    if let Some(parts) = parts {
        for diff in parts.iter().flatten() {
            added += diff.added;
            removed += diff.removed;
            has_diff = true;
        }
    }
    has_diff.then_some((added, removed))
}

fn is_user_entry(entry: &AcpEntry) -> bool {
    matches!(entry, AcpEntry::User(_) | AcpEntry::UserWithImages { .. })
}

fn is_completion_entry(entry: &AcpEntry) -> bool {
    matches!(
        entry,
        AcpEntry::ToolCall { title, .. } if is_task_completion_tool_title(title)
    )
}

fn is_collaboration_entry(entry: &AcpEntry) -> bool {
    matches!(
        entry,
        AcpEntry::ToolCall {
            kind: ToolKind::Collaborate,
            ..
        }
    )
}

/// 模型的思考摘要标题经常整行套 `**像这样**`/`__这样__`；折叠预览是纯文本
/// `div`，不走 markdown 渲染，裸露的星号看着像漏渲染的格式错误。只在整行
/// 前后都包着同一种标记时才剥掉，避免误伤正文里本来就有的单个星号。
fn strip_thought_heading_markers(line: &str) -> &str {
    for wrapper in ["**", "__"] {
        if let Some(inner) = line
            .strip_prefix(wrapper)
            .and_then(|s| s.strip_suffix(wrapper))
            && !inner.is_empty()
        {
            return inner;
        }
    }
    line
}

/// 对话过程里的思考与中间说明都先归约成一条可扫描的进展摘要。默认只取首个
/// 非空行，避免把模型的长篇过程说明重新铺满消息流；完整内容仍可按需展开。
fn progress_summary(text: &str) -> String {
    const MAX_CHARS: usize = 72;
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .map(strip_thought_heading_markers)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = line.chars();
    let preview: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

/// 单行短进展已经完整展示，不再提供一个点开后内容完全相同的空操作。多段内容
/// 或超过摘要长度的长句才显示展开入口。
fn progress_has_details(text: &str) -> bool {
    const MAX_CHARS: usize = 72;
    let mut meaningful = text.lines().filter(|line| !line.trim().is_empty());
    let Some(first) = meaningful.next() else {
        return false;
    };
    meaningful.next().is_some()
        || strip_thought_heading_markers(first.trim())
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .count()
            > MAX_CHARS
}

/// 顶栏那条来源横幅的文案。同一家 agent 续接说「继续」，换了 agent 或换了 workspace
/// 说「迁移」并点名来源——这条会话的上下文是二手的，用户得一眼看见。
fn fork_banner_text(origin: &AcpForkOrigin, current_agent: ConversationAgentKind) -> String {
    let source_name = origin.profile_label.clone().or_else(|| {
        origin
            .agent
            .as_deref()
            .and_then(ConversationAgentKind::from_id)
            .filter(|kind| *kind != current_agent || origin.profile_label.is_some())
            .map(|kind| kind.label().to_string())
    });
    let scope = if origin.from_history {
        "历史会话"
    } else {
        "会话"
    };
    match source_name {
        Some(name) => format!("从 {name} 的{scope}「{}」迁移而来", origin.title),
        // 源 agent 未知（旧存档）或就是同一家：保持原来的说法。
        None => format!("从「{}」继续", origin.title),
    }
}

/// 工具输出默认只展开这么多行，其余折叠到「展开全部 N 行」后面。
const TOOL_OUTPUT_PREVIEW_LINES: usize = 8;

/// 工具调用的输出不自动抢占对话空间；需要细节时由用户展开卡片。
fn tool_card_default_expanded() -> bool {
    false
}

/// 展开“执行过程”时，已完成和失败、且无需用户授权的工具都走紧凑轨迹行。
/// 失败默认不铺开输出，只在行尾标红「失败」；执行中和待授权仍保留完整卡片。
fn tool_uses_compact_process_row(status: ToolCallStatus, has_pending_permission: bool) -> bool {
    !has_pending_permission && matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed)
}

fn tool_has_live_children(children: &[AcpEntry]) -> bool {
    children.iter().any(|child| {
        matches!(
            child,
            AcpEntry::ToolCall {
                status: ToolCallStatus::Pending | ToolCallStatus::InProgress,
                ..
            }
        ) || smelt_core::acp_chat::has_unfinished_tool_call(std::slice::from_ref(child))
    })
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn live_elapsed_ms(turn_started_at_ms: Option<u64>) -> Option<u64> {
    turn_started_at_ms.and_then(|started| now_unix_ms().checked_sub(started))
}

/// 本轮用户消息之后是否已经有思考/工具/正文。没有时在消息流里画「已用 Ns」。
fn current_turn_has_agent_output(entries: &[AcpEntry]) -> bool {
    entries
        .iter()
        .rev()
        .take_while(|entry| !is_user_entry(entry))
        .any(|entry| !matches!(entry, AcpEntry::Divider(_)))
}

fn previous_visible_process_index(index: usize, first: usize) -> Option<usize> {
    (index > first).then(|| index - 1)
}

fn next_visible_process_index(index: usize, end: usize) -> Option<usize> {
    let next = index + 1;
    (next < end).then_some(next)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompactToolRun {
    kind: ToolKind,
    start: usize,
    indices: Vec<usize>,
}

fn groupable_process_tool_kind(
    entry: &AcpEntry,
    pending_tool_id: Option<&str>,
) -> Option<ToolKind> {
    match entry {
        AcpEntry::ToolCall {
            id,
            kind,
            status,
            title,
            children,
            ..
        } if *kind != ToolKind::Collaborate
            && !is_task_completion_tool_title(title)
            && children.is_empty() =>
        {
            let pending = pending_tool_id == Some(id.as_str());
            // 只合并成功的同类工具。失败单独占一行，避免被收进「读取了 N 个文件」里看不见。
            (!pending && matches!(status, ToolCallStatus::Completed)).then_some(*kind)
        }
        _ => None,
    }
}

/// 展开的过程组里，连续同一种已完成工具收成 Grok 式「搜索了 3 次」。
/// 思考和对用户说的中间正文都会打断合并，避免把思考夹进合成的工具段里。
/// 单次调用仍走原来的紧凑行，不额外套一层。
fn consecutive_compact_tool_run(
    entries: &[AcpEntry],
    index: usize,
    first: usize,
    end: usize,
    pending_tool_id: Option<&str>,
) -> Option<CompactToolRun> {
    if index < first || index >= end || index >= entries.len() {
        return None;
    }
    let kind = groupable_process_tool_kind(&entries[index], pending_tool_id)?;
    let mut start = index;
    while let Some(prev) = previous_visible_process_index(start, first) {
        if groupable_process_tool_kind(&entries[prev], pending_tool_id) != Some(kind) {
            break;
        }
        start = prev;
    }
    let mut indices = vec![start];
    let mut cursor = start;
    while let Some(next) = next_visible_process_index(cursor, end) {
        if groupable_process_tool_kind(&entries[next], pending_tool_id) != Some(kind) {
            break;
        }
        indices.push(next);
        cursor = next;
    }
    (indices.len() >= 2).then_some(CompactToolRun {
        kind,
        start,
        indices,
    })
}

fn compact_tool_run_label(kind: ToolKind, count: usize) -> String {
    match kind {
        ToolKind::Search => format!("搜索了 {count} 次"),
        ToolKind::Read => format!("读取了 {count} 个文件"),
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move => {
            format!("修改了 {count} 个文件")
        }
        ToolKind::Execute => format!("运行了 {count} 条命令"),
        ToolKind::Fetch => format!("打开了 {count} 个页面"),
        ToolKind::Image => format!("处理了 {count} 张图"),
        _ => format!("{} {count} 次", tool_kind_label(&kind)),
    }
}

/// 时间线上单条工具的动词句，对齐 Grok「Opened page / Searched web for」。
fn compact_tool_headline(kind: ToolKind, title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        return tool_kind_label(&kind).to_string();
    }
    match kind {
        ToolKind::Fetch => format!("打开了 {}", compact_fetch_target(title)),
        ToolKind::Search => format!("搜索 {title}"),
        ToolKind::Read => format!("读取了 {}", compact_path_leaf(title)),
        ToolKind::Edit => format!("修改了 {}", compact_path_leaf(title)),
        ToolKind::Delete => format!("删除了 {}", compact_path_leaf(title)),
        ToolKind::Move => format!("移动了 {}", compact_path_leaf(title)),
        ToolKind::Execute => format!("运行了 {}", compact_command_leaf(title)),
        ToolKind::Image => format!("处理了 {}", compact_path_leaf(title)),
        _ => title.to_string(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrajectoryLane {
    User,
    Assistant,
    Tool,
    Context,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TrajectoryEvent {
    seq: usize,
    lane: TrajectoryLane,
    /// 1-based 回合；0 表示第一轮用户消息之前（CONTEXT）。
    turn: usize,
    text: String,
}

/// 整场会话的轨迹事件，给轨迹窗口用。参考 DSH Trajectory 的 USER / CONTEXT / ASSISTANT / TOOL。
fn session_trajectory_events(
    entries: &[AcpEntry],
    context_note: Option<String>,
) -> Vec<TrajectoryEvent> {
    let mut events = Vec::new();
    let mut turn = 0usize;
    if let Some(note) = context_note.filter(|note| !note.is_empty()) {
        events.push(TrajectoryEvent {
            seq: events.len(),
            lane: TrajectoryLane::Context,
            turn: 0,
            text: note,
        });
    }
    for entry in entries {
        match entry {
            AcpEntry::User(text) | AcpEntry::UserWithImages { text, .. } => {
                turn += 1;
                events.push(TrajectoryEvent {
                    seq: events.len(),
                    lane: TrajectoryLane::User,
                    turn,
                    text: text.clone(),
                });
            }
            AcpEntry::Assistant { text, thought } if !thought && !text.trim().is_empty() => {
                events.push(TrajectoryEvent {
                    seq: events.len(),
                    lane: TrajectoryLane::Assistant,
                    turn: turn.max(1),
                    text: text.clone(),
                });
            }
            AcpEntry::ToolCall { kind, title, .. } if !is_task_completion_tool_title(title) => {
                events.push(TrajectoryEvent {
                    seq: events.len(),
                    lane: TrajectoryLane::Tool,
                    turn: turn.max(1),
                    text: compact_tool_headline(*kind, title),
                });
            }
            _ => {}
        }
    }
    events
}

fn filter_trajectory_events<'a>(
    events: &'a [TrajectoryEvent],
    query: &str,
) -> Vec<&'a TrajectoryEvent> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|term| term.to_lowercase())
        .filter(|term| !term.is_empty())
        .collect();
    if terms.is_empty() {
        return events.iter().collect();
    }
    events
        .iter()
        .filter(|event| {
            let haystack = format!(
                "{} {} {}",
                trajectory_lane_label(event.lane),
                event.text,
                if event.turn == 0 {
                    String::new()
                } else {
                    format!("turn {}", event.turn)
                }
            )
            .to_lowercase();
            terms.iter().all(|term| haystack.contains(term))
        })
        .collect()
}

fn trajectory_lane_label(lane: TrajectoryLane) -> &'static str {
    match lane {
        TrajectoryLane::User => "USER",
        TrajectoryLane::Assistant => "ASSISTANT",
        TrajectoryLane::Tool => "TOOL",
        TrajectoryLane::Context => "CONTEXT",
    }
}

fn trajectory_lane_color(lane: TrajectoryLane) -> u32 {
    match lane {
        TrajectoryLane::User => ui_theme::blue(),
        TrajectoryLane::Assistant => ui_theme::purple(),
        TrajectoryLane::Tool => ui_theme::green(),
        TrajectoryLane::Context => ui_theme::text_muted(),
    }
}

fn trajectory_counts(entries: &[AcpEntry]) -> (usize, usize) {
    let turns = entries.iter().filter(|entry| is_user_entry(entry)).count();
    let calls = entries
        .iter()
        .filter(|entry| matches!(entry, AcpEntry::ToolCall { title, .. } if !is_task_completion_tool_title(title)))
        .count();
    (turns, calls)
}

fn compact_fetch_target(title: &str) -> String {
    let stripped = title
        .strip_prefix("https://")
        .or_else(|| title.strip_prefix("http://"))
        .unwrap_or(title);
    stripped
        .strip_prefix("www.")
        .unwrap_or(stripped)
        .to_string()
}

fn compact_path_leaf(title: &str) -> String {
    title
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(title)
        .to_string()
}

fn compact_command_leaf(title: &str) -> String {
    title
        .lines()
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(title)
        .to_string()
}

fn tool_output_has_diff(output: &[ToolOutputPart]) -> bool {
    output
        .iter()
        .any(|part| matches!(part, ToolOutputPart::Diff { .. }))
}

/// 没有可展示输出的工具只有标题，不提供点击展开，避免展开后得到空卡片。
fn tool_output_has_content(output: &[ToolOutputPart]) -> bool {
    output.iter().any(|part| match part {
        ToolOutputPart::Text(text) => !strip_code_fence(text).trim().is_empty(),
        ToolOutputPart::Diff { .. } => true,
        ToolOutputPart::Image(_) => true,
        ToolOutputPart::Terminal { .. } => true,
    })
}

/// 工具头部/紧凑行的稳定结果摘要。只在工具完成后显示，避免把流式中的半截
/// stdout 误当最终结果；原始内容继续留在展开卡片里。
fn tool_result_summary(
    kind: ToolKind,
    status: ToolCallStatus,
    output: &[ToolOutputPart],
) -> Option<String> {
    if !matches!(status, ToolCallStatus::Completed) {
        return None;
    }
    if matches!(kind, ToolKind::Search) {
        return search_summary_text(output);
    }
    let lines = output
        .iter()
        .filter_map(|part| match part {
            ToolOutputPart::Text(text) => Some(strip_code_fence(text)),
            ToolOutputPart::Diff { .. } => None,
            ToolOutputPart::Image(_) => None,
            ToolOutputPart::Terminal { output, .. } => Some(output.as_str()),
        })
        .flat_map(str::lines)
        .filter(|line| !line.trim().is_empty())
        .count();
    if lines == 0 {
        return None;
    }
    match kind {
        ToolKind::Read | ToolKind::Fetch => Some(format!("{lines} 行")),
        ToolKind::Execute => Some(format!("{lines} 行输出")),
        ToolKind::Review => Some(format!("{lines} 行结果")),
        _ => None,
    }
}

/// search 工具的头部摘要：直接复用 agent 输出里的匹配数汇总行原文
/// （如「found 3 matches」「3 matches」「(3 matches)」），与展开后的内容
/// 一字不差，不做二次计算。没有汇总行则返回 None（头部不显示摘要）。
fn search_summary_text(output: &[ToolOutputPart]) -> Option<String> {
    for part in output {
        let ToolOutputPart::Text(text) = part else {
            continue;
        };
        let body = smelt_core::acp_chat::strip_code_fence(text);
        for line in body.lines() {
            let t = line.trim();
            if is_match_count_line(t) {
                // 返回原始行文本（保留括号等原样），与展开内容一字不差。
                return Some(t.to_string());
            }
        }
    }
    None
}

/// 汇总行判断：`found N matches` / `N matches` / `(N matches)` / `N results`。
/// 只判形态不取数字；普通匹配行（如 `path:12: found a match`）不会误判。
fn is_match_count_line(line: &str) -> bool {
    let t = line.trim().trim_matches(['(', ')']).trim();
    let lower = t.to_ascii_lowercase();
    let rest = lower.strip_prefix("found ").unwrap_or(&lower);
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return false;
    }
    let tail = rest[digits.len()..].trim_start();
    tail.starts_with("match") || tail.starts_with("result")
}

fn render_compact_diff_stats(added: usize, removed: usize) -> gpui::AnyElement {
    h_flex()
        .flex_shrink_0()
        .gap_1p5()
        .items_center()
        .child(
            div()
                .text_xs()
                .font_family(smelt_core::font_config::font_family())
                .text_color(gpui::rgb(ui_theme::green()))
                .child(format!("+{added}")),
        )
        .child(
            div()
                .text_xs()
                .font_family(smelt_core::font_config::font_family())
                .text_color(gpui::rgb(ui_theme::red()))
                .child(format!("-{removed}")),
        )
        .into_any_element()
}

#[derive(Clone, Copy, Default)]
struct ProcessGroupInfo {
    first: usize,
    end: usize,
    /// 过程组所属回合仍在运行；不能借用整个视图的当前回合状态，否则旧组会被误标。
    active: bool,
    /// 最近一项未结束的普通工具类别，用于“正在读取/搜索/修改”等语义状态。
    active_tool_kind: Option<ToolKind>,
    /// 被 ACP 服务端投影为 tool_call 的子 agent / 委派操作数量。
    agents: usize,
    /// 尚未结束的 agent 操作。折叠执行过程时也必须让协作状态可见。
    active_agents: usize,
    /// 该回合从发出 prompt 到 TurnEnded 的耗时。旧会话没有计时。
    elapsed_ms: Option<u64>,
}

#[derive(Clone, Copy, Default)]
struct EntryPresentation {
    final_answer: bool,
    process_group: Option<ProcessGroupInfo>,
}

fn process_group_label(group: ProcessGroupInfo) -> String {
    if group.active && group.active_agents > 0 {
        return if group.active_agents == group.agents {
            format!("{} 个 Agent 工作中", group.active_agents)
        } else {
            format!("{}/{} 个 Agent 工作中", group.active_agents, group.agents)
        };
    }
    if group.active {
        return if group.agents > 0 && group.active_tool_kind.is_none() {
            format!("正在协作 · {} 个 Agent", group.agents)
        } else if let Some(kind) = group.active_tool_kind {
            tool_running_label(kind).to_string()
        } else {
            "正在分析".to_string()
        };
    }
    "执行过程".to_string()
}

/// 过程组标题：进行中写当前动作；完成后只留 Grok 式「工作了 9s」，点开再看工具。
fn process_group_header_label(entries: &[AcpEntry], group: ProcessGroupInfo) -> Option<String> {
    if group.active {
        return Some(process_group_label(group));
    }
    if let Some(elapsed) = group.elapsed_ms {
        return Some(format!("工作了 {}", format_duration(elapsed)));
    }
    let summary = process_group_tool_summary(entries, group.first, group.end);
    (!summary.is_empty()).then_some(summary)
}

fn process_group_tool_summary(entries: &[AcpEntry], first: usize, end: usize) -> String {
    let mut counts: Vec<(ToolKind, usize)> = Vec::new();
    for entry in entries.get(first..end).unwrap_or(&[]) {
        let AcpEntry::ToolCall { kind, title, .. } = entry else {
            continue;
        };
        if is_task_completion_tool_title(title) {
            continue;
        }
        if let Some((_, count)) = counts.iter_mut().find(|(item, _)| *item == *kind) {
            *count += 1;
        } else {
            counts.push((*kind, 1));
        }
    }
    const MAX_KINDS: usize = 3;
    let extra = counts.len().saturating_sub(MAX_KINDS);
    let mut parts = counts
        .iter()
        .take(MAX_KINDS)
        .map(|(kind, count)| tool_kind_summary(*kind, *count))
        .collect::<Vec<_>>();
    if extra > 0 {
        parts.push("等".to_string());
    }
    parts.join(" · ")
}

fn tool_kind_summary(kind: ToolKind, count: usize) -> String {
    match kind {
        ToolKind::Search => format!("搜索了 {count} 次"),
        ToolKind::Read => format!("读取了 {count} 次"),
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move => {
            format!("修改了 {count} 次")
        }
        ToolKind::Execute => format!("运行了 {count} 次"),
        ToolKind::Fetch => format!("获取了 {count} 次"),
        ToolKind::Think => format!("分析了 {count} 次"),
        ToolKind::Collaborate => format!("{count} 个 Agent"),
        ToolKind::Review => format!("审阅了 {count} 次"),
        ToolKind::Image => format!("处理了 {count} 张图片"),
        ToolKind::Compact => format!("整理了 {count} 次上下文"),
        ToolKind::Wait => format!("等待了 {count} 次"),
        ToolKind::SwitchMode => format!("切换了 {count} 次模式"),
        ToolKind::Other => format!("处理了 {count} 次"),
    }
}

fn tool_running_label(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "正在读取",
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move => "正在修改",
        ToolKind::Search => "正在搜索",
        ToolKind::Execute => "正在运行",
        ToolKind::Fetch => "正在获取",
        ToolKind::Think => "正在分析",
        ToolKind::SwitchMode => "正在切换模式",
        ToolKind::Collaborate => "正在协作",
        ToolKind::Review => "正在审阅",
        ToolKind::Image => "正在处理图片",
        ToolKind::Compact => "正在整理上下文",
        ToolKind::Wait => "正在等待",
        ToolKind::Other => "正在处理",
    }
}

/// 把协议 entries 一次归约成渲染布局。已结束回合的最后一段正式正文或完成摘要
/// 是最终回答；活跃回合没有最终回答，避免流式过程中最新正文反复在“过程/结论”
/// 之间跳动。最终回答之后迟到的普通工具通知仍属于同一过程组，不能散落成独立卡片。
#[cfg(test)]
fn build_conversation_layout(
    entries: &[AcpEntry],
    current_turn_active: bool,
) -> Vec<EntryPresentation> {
    build_conversation_layout_with_timings(entries, current_turn_active, 0, &[], None)
}

fn build_conversation_layout_with_timings(
    entries: &[AcpEntry],
    current_turn_active: bool,
    entries_offset: usize,
    turn_timings: &[smelt_core::acp_session::TurnTiming],
    live_elapsed_ms: Option<u64>,
) -> Vec<EntryPresentation> {
    let mut layout = vec![EntryPresentation::default(); entries.len()];
    let mut start = 0;
    while start < entries.len() {
        if is_user_entry(&entries[start]) || matches!(entries[start], AcpEntry::Divider(_)) {
            start += 1;
            continue;
        }
        let end = entries[start..]
            .iter()
            .position(|entry| is_user_entry(entry) || matches!(entry, AcpEntry::Divider(_)))
            .map_or(entries.len(), |offset| start + offset);
        let closed = end < entries.len() || !current_turn_active;
        let final_ix = closed
            .then(|| {
                (start..end).rev().find(|ix| {
                    matches!(entries[*ix], AcpEntry::Assistant { thought: false, .. })
                        || is_completion_entry(&entries[*ix])
                })
            })
            .flatten();
        if let Some(final_ix) = final_ix {
            layout[final_ix].final_answer = true;
        }
        let process_indices: Vec<usize> = (start..end)
            .filter(|ix| {
                Some(*ix) != final_ix
                    && !is_completion_entry(&entries[*ix])
                    && !is_collaboration_entry(&entries[*ix])
            })
            .collect();
        if let Some(&first) = process_indices.first() {
            let tools = process_indices
                .iter()
                .filter(|ix| matches!(entries[**ix], AcpEntry::ToolCall { .. }))
                .count();
            let active_tool_kind = process_indices
                .iter()
                .rev()
                .find_map(|ix| match entries[*ix] {
                    AcpEntry::ToolCall {
                        kind,
                        status: ToolCallStatus::Pending | ToolCallStatus::InProgress,
                        ..
                    } if kind != ToolKind::Collaborate => Some(kind),
                    _ => None,
                });
            let agents = process_indices
                .iter()
                .filter(|ix| {
                    matches!(
                        entries[**ix],
                        AcpEntry::ToolCall {
                            kind: ToolKind::Collaborate,
                            ..
                        }
                    )
                })
                .count();
            let active_agents = process_indices
                .iter()
                .filter(|ix| match &entries[**ix] {
                    AcpEntry::ToolCall {
                        kind,
                        status,
                        children,
                        ..
                    } => smelt_core::acp_chat::is_active_agent_tool(*kind, *status, children),
                    _ => false,
                })
                .count();
            let group_end = process_indices
                .last()
                .copied()
                .map_or(first.saturating_add(1), |ix| ix.saturating_add(1));
            // 有工具就收成过程组。进行中默认展开成 Grok 那种紧凑时间线；
            // 没有工具就不要包组，寒暄的思考走进展行，把原文摘要亮出来。
            if tools > 0 {
                let elapsed_ms = if closed {
                    (0..start)
                        .rev()
                        .find(|ix| is_user_entry(&entries[*ix]))
                        .and_then(|user_index| {
                            let global_index = entries_offset.saturating_add(user_index);
                            turn_timings
                                .iter()
                                .find(|timing| timing.user_index == global_index)
                                .and_then(smelt_core::acp_session::TurnTiming::completed_elapsed_ms)
                        })
                } else {
                    live_elapsed_ms
                };
                let group = ProcessGroupInfo {
                    first,
                    end: group_end,
                    active: !closed,
                    active_tool_kind,
                    agents,
                    active_agents,
                    elapsed_ms,
                };
                for ix in process_indices {
                    layout[ix].process_group = Some(group);
                }
            }
        }
        start = end;
    }
    layout
}

fn format_duration(milliseconds: u64) -> String {
    let total_seconds = milliseconds / 1_000;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

/// gpui-component 会把 Markdown 链接目标原样交给 `open_url`。相对文件路径在
/// macOS 上会被 LaunchServices 误当作应用标识并报 -50，因此在进入 Markdown
/// 渲染前把本地路径解析成 Smelt 内部使用的 file URL。
fn markdown_text_for_cwd(text: &str, cwd: Option<&str>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("](") {
        let target_start = open + 2;
        let Some(close_offset) = rest[target_start..].find(')') else {
            break;
        };
        let target_end = target_start + close_offset;
        let target = &rest[target_start..target_end];
        out.push_str(&rest[..target_start]);
        if let Some(resolved) = resolve_relative_file_link(target, cwd) {
            out.push_str(&resolved);
        } else {
            out.push_str(target);
        }
        out.push(')');
        rest = &rest[target_end + 1..];
    }
    out.push_str(rest);
    out
}

fn markdown_user_text_for_cwd(text: &str, cwd: Option<&str>) -> String {
    markdown_text_for_cwd(&escape_html_tags_for_markdown(text), cwd)
}

fn markdown_for_entry(entry: &AcpEntry, cwd: Option<&str>) -> Option<gpui::SharedString> {
    let rendered = match entry {
        AcpEntry::User(text) if !is_interrupt_marker(text) => markdown_user_text_for_cwd(text, cwd),
        AcpEntry::UserWithImages { text, .. } => markdown_user_text_for_cwd(text, cwd),
        AcpEntry::Assistant { text, .. } => markdown_text_for_cwd(text, cwd),
        AcpEntry::ToolCall { title, output, .. } if is_task_completion_tool_title(title) => {
            let text = completion_summary_text(output);
            if text.trim().is_empty() {
                return None;
            }
            markdown_text_for_cwd(&text, cwd)
        }
        _ => return None,
    };
    Some(rendered.into())
}

fn build_markdown_cache(
    entries: &[AcpEntry],
    cwd: Option<&str>,
) -> Vec<Option<gpui::SharedString>> {
    entries
        .iter()
        .map(|entry| markdown_for_entry(entry, cwd))
        .collect()
}

fn refresh_markdown_cache(
    entries: &[AcpEntry],
    entries_offset: usize,
    cwd: Option<&str>,
    cache: &mut Vec<Option<gpui::SharedString>>,
) {
    if cache.len() < entries_offset {
        *cache = build_markdown_cache(entries, cwd);
        return;
    }
    cache.truncate(entries_offset);
    cache.extend(
        entries[entries_offset..]
            .iter()
            .map(|entry| markdown_for_entry(entry, cwd)),
    );
}

fn cached_entry_markdown(
    cache: &[Option<gpui::SharedString>],
    entry_ix: usize,
    entry: &AcpEntry,
    cwd: Option<&str>,
) -> gpui::SharedString {
    cache
        .get(entry_ix)
        .and_then(Clone::clone)
        .or_else(|| markdown_for_entry(entry, cwd))
        .unwrap_or_default()
}

fn diff_stats_for_output(output: &[ToolOutputPart]) -> Option<(usize, usize)> {
    let mut added = 0;
    let mut removed = 0;
    let mut has_diff = false;
    for part in output {
        let ToolOutputPart::Diff {
            old_text, new_text, ..
        } = part
        else {
            continue;
        };
        let (part_added, part_removed) =
            diff_line_stats(old_text.as_deref().unwrap_or(""), new_text);
        added += part_added;
        removed += part_removed;
        has_diff = true;
    }
    has_diff.then_some((added, removed))
}

/// Raw HTML is parsed as markup by the Markdown renderer. Unsupported tags can
/// therefore make the whole fragment disappear; user messages should show the
/// literal tag instead. Keep code spans and fenced code untouched.
fn escape_html_tags_for_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut inline_code_ticks = None;
    let mut fenced_code_ticks = None;

    while i < text.len() {
        if let Some(fence_len) = fenced_code_ticks {
            if is_line_start(text, i)
                && let Some((run_len, end)) = backtick_run(text, i)
                && run_len >= fence_len
                && line_after_backticks_is_blank(text, end)
            {
                out.push_str(&text[i..end]);
                i = end;
                fenced_code_ticks = None;
                continue;
            }
            let ch = text[i..].chars().next().expect("valid UTF-8 offset");
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }

        if is_line_start(text, i) {
            let indent = text[i..]
                .chars()
                .take(3)
                .take_while(|ch| *ch == ' ')
                .count();
            if let Some((run_len, end)) = backtick_run(text, i + indent)
                && run_len >= 3
            {
                out.push_str(&text[i..end]);
                i = end;
                fenced_code_ticks = Some(run_len);
                inline_code_ticks = None;
                continue;
            }
        }

        if text.as_bytes()[i] == b'`'
            && let Some((run_len, end)) = backtick_run(text, i)
        {
            out.push_str(&text[i..end]);
            i = end;
            inline_code_ticks = match inline_code_ticks {
                Some(active) if active == run_len => None,
                None => Some(run_len),
                active => active,
            };
            continue;
        }

        if inline_code_ticks.is_none()
            && text.as_bytes()[i] == b'<'
            && looks_like_html_tag(text, i)
            && html_tag_end(text, i).is_some()
            && (i == 0 || text.as_bytes()[i - 1] != b'\\')
        {
            out.push('\\');
            out.push('<');
            i += 1;
            continue;
        }

        let ch = text[i..].chars().next().expect("valid UTF-8 offset");
        out.push(ch);
        i += ch.len_utf8();
    }

    out
}

/// `TextView::markdown` deliberately interprets Markdown. Tool output and diff text must remain
/// literal, so render escaped HTML inside `<pre>` while still opting into native text selection.
fn selectable_plain_text(id: impl Into<gpui::ElementId>, text: &str) -> TextView {
    TextView::html(id, preformatted_html(text)).selectable(true)
}

fn preformatted_html(text: &str) -> String {
    let mut html = String::with_capacity(text.len() + 11);
    html.push_str("<pre>");
    for ch in text.chars() {
        match ch {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            '"' => html.push_str("&quot;"),
            '\'' => html.push_str("&#39;"),
            _ => html.push(ch),
        }
    }
    html.push_str("</pre>");
    html
}

fn is_line_start(text: &str, offset: usize) -> bool {
    offset == 0 || text.as_bytes().get(offset - 1) == Some(&b'\n')
}

fn backtick_run(text: &str, offset: usize) -> Option<(usize, usize)> {
    if text.as_bytes().get(offset) != Some(&b'`') {
        return None;
    }
    let mut end = offset;
    while text.as_bytes().get(end) == Some(&b'`') {
        end += 1;
    }
    Some((end - offset, end))
}

fn line_after_backticks_is_blank(text: &str, offset: usize) -> bool {
    text[offset..]
        .split_once('\n')
        .is_none_or(|(line, _)| line.trim().is_empty())
}

fn looks_like_html_tag(text: &str, start: usize) -> bool {
    let rest = &text[start + 1..];
    if rest.starts_with("http://") || rest.starts_with("https://") || rest.starts_with("mailto:") {
        return false;
    }
    if rest.starts_with('/')
        || rest.starts_with('!')
        || rest.starts_with('?')
        || rest.starts_with("![CDATA[")
    {
        return true;
    }

    let first = rest.chars().next();
    if !first.is_some_and(|ch| ch.is_ascii_alphabetic()) {
        return false;
    }
    let first_len = first.map_or(0, char::len_utf8);
    let mut name_end = first_len;
    for (offset, ch) in rest[first_len..].char_indices() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':') {
            name_end = first_len + offset + ch.len_utf8();
        } else {
            break;
        }
    }
    rest[name_end..]
        .chars()
        .next()
        .is_some_and(|ch| ch == '>' || ch == '/' || ch.is_whitespace())
}

fn html_tag_end(text: &str, start: usize) -> Option<usize> {
    let mut quote = None;
    for (offset, ch) in text[start + 1..].char_indices() {
        match quote {
            Some(active) if ch == active => quote = None,
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '>' => return Some(start + 1 + offset),
            _ => {}
        }
    }
    None
}

fn resolve_relative_file_link(target: &str, cwd: Option<&str>) -> Option<String> {
    let target = target.trim();
    if target.is_empty()
        || target.starts_with('#')
        || target.starts_with('~')
        || target.contains("://")
        || target.starts_with("mailto:")
        || target.starts_with("data:")
    {
        return None;
    }
    let (path, fragment) = target.split_once('#').unwrap_or((target, ""));
    // Agent 引用文件常写成 grep/编译器诊断那种 `path:行号` 或 `path:行号:列号`
    // 格式（不是 `#L行号` 片段）。这种没有 `#` 片段时，才尝试从冒号后缀里抠
    // 行号出来——不然会把 `:2765` 当成文件名的一部分拼进路径，读不到文件时
    // 还误报“可能是二进制文件”。
    let (path, fragment) = if fragment.is_empty() {
        match extract_trailing_line_number(path) {
            Some((base, line)) => (base, format!("L{line}")),
            None => (path, String::new()),
        }
    } else {
        (path, fragment.to_string())
    };
    let path = std::path::Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::path::Path::new(cwd?).join(path)
    };
    let mut url = url::Url::from_file_path(absolute).ok()?;
    if !fragment.is_empty() {
        url.set_fragment(Some(&fragment));
    }
    // `Url::set_scheme` 会把自定义 scheme 序列化成 `smelt-file:/path`
    // （单斜杠），而 macOS URL scheme 与主进程解析器都按 authority 形式接收。
    // 从标准 file URL 替换前缀可稳定保留 `smelt-file:///absolute/path`。
    Some(url.to_string().replacen("file://", "smelt-file://", 1))
}

/// 从 `path:行号` / `path:行号:列号` 里拆出末尾的行号：从右往左数，只要是纯
/// 数字的 segment 就一直往前吞（列号可选，最多吞两段），吞到的最后一个数字
/// segment 就是行号；一段数字都没吃到就说明不是这种格式，返回 None 原样处理
/// （比如 Windows 盘符 `C:\...`，虽然本 app 只跑 macOS，多判一下无妨）。
fn extract_trailing_line_number(path: &str) -> Option<(&str, u32)> {
    let mut rest = path;
    let mut line = None;
    for _ in 0..2 {
        let Some((head, tail)) = rest.rsplit_once(':') else {
            break;
        };
        if head.is_empty() {
            break;
        }
        let Ok(n) = tail.parse::<u32>() else {
            break;
        };
        line = Some(n);
        rest = head;
    }
    line.map(|n| (rest, n))
}

/// 审批请求按收到顺序串行展示和处理，不能越过队首回应后续 responder。
fn is_active_permission_selection(
    permissions: &[PendingPermission],
    tool_call_id: &str,
    option_id: &str,
) -> bool {
    permissions.first().is_some_and(|card| {
        card.tool_call_id == tool_call_id
            && card
                .options
                .iter()
                .any(|option| option.option_id == option_id)
    })
}

/// ToolKind → 简短英文标签（跟工具本身在协议里的调用名对齐，比长句子扫得快）。
fn tool_kind_label(kind: &ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "Read",
        ToolKind::Edit => "Edit",
        ToolKind::Delete => "Delete",
        ToolKind::Move => "Move",
        ToolKind::Search => "Search",
        ToolKind::Execute => "Bash",
        ToolKind::Fetch => "Fetch",
        ToolKind::Think => "Think",
        ToolKind::SwitchMode => "Mode",
        ToolKind::Collaborate => "Agent",
        ToolKind::Review => "Review",
        ToolKind::Image => "Image",
        ToolKind::Compact => "Compact",
        ToolKind::Wait => "Wait",
        _ => "Tool",
    }
}

fn tool_kind_icon(kind: &ToolKind) -> IconName {
    match kind {
        ToolKind::Execute => IconName::SquareTerminal,
        ToolKind::Read | ToolKind::Edit | ToolKind::Delete | ToolKind::Move | ToolKind::Image => {
            IconName::File
        }
        ToolKind::Search | ToolKind::Fetch => IconName::Search,
        ToolKind::Collaborate => IconName::Bot,
        ToolKind::Review | ToolKind::Compact | ToolKind::Think | ToolKind::SwitchMode => {
            IconName::Asterisk
        }
        ToolKind::Wait | ToolKind::Other => IconName::Asterisk,
    }
}

/// 时间线用更接近 Grok 的图标：打开页面是地球，搜索是放大镜。
fn process_timeline_icon(kind: &ToolKind) -> IconName {
    match kind {
        ToolKind::Fetch => IconName::Globe,
        ToolKind::Search => IconName::Search,
        _ => tool_kind_icon(kind),
    }
}

/// 渲染一份 diff：逐行红（删）/绿（增）/灰（不变），等宽字体，滚动限高——大改动
/// 不能把整个消息流撑爆，超出部分滚动查看。`key` 保证同一条消息里多个 diff
/// 块各自有唯一 element id。行数据来自 `smelt_core::acp_chat::diff_lines`——
/// 跟头部「+N -M」摘要（`diff_line_stats`）共用同一次计算结果，数字不会对不上。
fn render_diff_lines(
    lines: &[DiffLine],
    key: (usize, usize),
    border_color: gpui::Hsla,
    muted_color: gpui::Hsla,
) -> gpui::AnyElement {
    let mut rows = v_flex()
        .id(("acp-diff", key.0 * 10_000 + key.1))
        .max_h(px(320.))
        .overflow_y_scroll()
        .rounded_md()
        .border_1()
        .border_color(border_color)
        .font_family(smelt_core::font_config::font_family())
        .text_xs();
    for (line_ix, line) in lines.iter().enumerate() {
        let (bg, prefix, fg): (Option<gpui::Hsla>, &str, gpui::Hsla) = match line.tag {
            DiffLineTag::Removed => (
                Some(smelt_ui::ui_theme::tint(smelt_ui::ui_theme::red(), 0x22).into()),
                "-",
                gpui::rgb(smelt_ui::ui_theme::red()).into(),
            ),
            DiffLineTag::Added => (
                Some(smelt_ui::ui_theme::tint(smelt_ui::ui_theme::green(), 0x22).into()),
                "+",
                gpui::rgb(smelt_ui::ui_theme::diff_add_text()).into(),
            ),
            DiffLineTag::Context => (None, " ", muted_color),
        };
        let mut row = h_flex().px_2().gap_2();
        if let Some(bg) = bg {
            row = row.bg(bg);
        }
        let text = format!("{prefix}{}", line.text);
        rows = rows.child(
            row.child(
                selectable_plain_text(
                    format!("acp-diff-line-{}-{}-{line_ix}", key.0, key.1),
                    &text,
                )
                .flex_1()
                .min_w_0()
                .text_xs()
                .font_family(smelt_core::font_config::font_family())
                .text_color(fg),
            ),
        );
    }
    rows.into_any_element()
}

// strip_code_fence / is_interrupt_marker 的单测随实现一起搬进了
// smelt_core::acp_chat（见该模块的 #[cfg(test)]），这里不再重复。
