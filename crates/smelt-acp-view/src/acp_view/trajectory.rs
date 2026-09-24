//! 独立 Trajectory 窗口：参考 DSH `ui-trajectory` 的账本 + 时间概览。
//!
//! 只投影这场会话已经发生的事，不编造耗时。工具级起止时间我们没有，
//! Overview 用 sequence 等宽色块（DSH 也有 sequence 模式），账本按 Turn 分组。

use super::*;
use gpui_component::input::{Input, InputEvent, InputState};

#[derive(Clone, Copy, PartialEq, Eq)]
enum InspectorTab {
    Preview,
    Source,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrajectoryCacheKey {
    snapshot_revision: u64,
    entries_len: usize,
    loaded_entries_offset: usize,
    entries_total: usize,
    tool_debug_len: usize,
    runtime_version: u32,
    system_prompt_len: usize,
    runtime_tool_count: usize,
    model_call_count: usize,
    last_model_call_sequence: u64,
    last_response_at_ms: u64,
    compaction_count: usize,
    last_compaction_finished_at_ms: u64,
}

impl TrajectoryCacheKey {
    fn for_session(session: &AcpView) -> Self {
        let runtime_debug = &session.runtime_debug;
        let last_call = runtime_debug
            .model_calls
            .last()
            .or(runtime_debug.model_call.as_ref());
        let last_compaction = runtime_debug.compactions.last();
        Self {
            // While a turn is running, assistant/tool updates can arrive per token. The trace
            // reflects the last structural/capture update and is fully refreshed at turn end.
            snapshot_revision: if session.has_active_turn() {
                0
            } else {
                session.last_snapshot_revision
            },
            entries_len: session.entries.len(),
            loaded_entries_offset: session.loaded_entries_offset,
            entries_total: session.entries_total,
            tool_debug_len: session.tool_debug.len(),
            runtime_version: runtime_debug.version,
            system_prompt_len: runtime_debug.system_prompt.as_ref().map_or(0, String::len),
            runtime_tool_count: runtime_debug.tools.len(),
            model_call_count: runtime_debug.model_calls.len(),
            last_model_call_sequence: last_call.map_or(0, |call| call.sequence),
            last_response_at_ms: last_call
                .and_then(|call| call.response_captured_at_ms)
                .unwrap_or(0),
            compaction_count: runtime_debug.compactions.len(),
            last_compaction_finished_at_ms: last_compaction
                .and_then(|compaction| compaction.finished_at_ms)
                .unwrap_or(0),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct OverviewBin {
    input: bool,
    model: bool,
    tools: bool,
}

#[derive(Clone, Debug, Default)]
struct TrajectoryOverview {
    ordered_event_count: usize,
    bins: Vec<OverviewBin>,
}

impl TrajectoryOverview {
    fn from_events(events: &[TrajectoryEvent]) -> Self {
        const MAX_BINS: usize = 256;
        let ordered_event_count = events.iter().filter(|event| event.turn > 0).count();
        let bin_count = ordered_event_count.min(MAX_BINS);
        if bin_count == 0 {
            return Self::default();
        }

        let mut bins = vec![OverviewBin::default(); bin_count];
        let mut position = 0usize;
        for event in events.iter().filter(|event| event.turn > 0) {
            let bin = position * bin_count / ordered_event_count;
            let cell = &mut bins[bin.min(bin_count - 1)];
            match event.lane {
                TrajectoryLane::User => cell.input = true,
                TrajectoryLane::Assistant | TrajectoryLane::Thinking => cell.model = true,
                TrajectoryLane::Tool => cell.tools = true,
                _ => {}
            }
            position += 1;
        }
        Self {
            ordered_event_count,
            bins,
        }
    }
}

pub(super) struct TrajectoryWindow {
    session: Entity<AcpView>,
    event_list: ListState,
    search: Entity<InputState>,
    selected: Option<usize>,
    cached_events: Vec<TrajectoryEvent>,
    cached_key: Option<TrajectoryCacheKey>,
    cached_turns: usize,
    cached_calls: usize,
    cached_completed_duration_ms: u64,
    cached_overview: TrajectoryOverview,
    visible_sequences: std::rc::Rc<Vec<usize>>,
    visible_key: Option<TrajectoryCacheKey>,
    last_query: String,
    inspector_tab: InspectorTab,
    _observe: gpui::Subscription,
    _search: gpui::Subscription,
}

impl TrajectoryWindow {
    fn new(session: Entity<AcpView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let _search = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let _observe = cx.observe(&session, |_, _, cx| cx.notify());
        Self {
            session,
            event_list: ListState::new(0, ListAlignment::Top, px(600.))
                .with_uniform_item_height(px(34.)),
            search,
            selected: None,
            cached_events: Vec::new(),
            cached_key: None,
            cached_turns: 0,
            cached_calls: 0,
            cached_completed_duration_ms: 0,
            cached_overview: TrajectoryOverview::default(),
            visible_sequences: std::rc::Rc::new(Vec::new()),
            visible_key: None,
            last_query: String::new(),
            inspector_tab: InspectorTab::Preview,
            _observe,
            _search,
        }
    }
}

impl AcpView {
    pub(super) fn open_trajectory_window(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.trajectory_window
            && handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            return;
        }
        self.trajectory_window = None;
        let session = cx.entity();
        cx.defer(move |cx| {
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("会话追踪".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::centered(size(px(1120.), px(760.)), cx)),
                ..Default::default()
            };
            let Ok(handle) = cx.open_window(options, |window, cx| {
                cx.new(|cx| TrajectoryWindow::new(session.clone(), window, cx))
            }) else {
                return;
            };
            session.update(cx, |this, _| {
                this.trajectory_window = Some(handle);
            });
        });
    }
}

pub(super) fn runtime_debug_contexts(debug: &RuntimeDebug) -> Vec<TrajectoryContext> {
    let mut contexts = Vec::new();
    if let Some(system_prompt) = debug
        .system_prompt
        .as_deref()
        .filter(|prompt| !prompt.is_empty())
    {
        let source = serde_json::json!({
            "version": debug.version,
            "source": debug.source,
            "systemPrompt": system_prompt,
            "tools": debug.tools,
        });
        contexts.push(TrajectoryContext {
            lane: TrajectoryLane::Request,
            label: "Pi REQUEST CONFIG".to_string(),
            preview: format!(
                "System prompt: {} characters · {} tools",
                system_prompt.chars().count(),
                debug.tools.len()
            ),
            source: compact_json(&source),
            pi_turn: None,
            captured_at_ms: None,
        });
    }

    let mut captures = Vec::new();
    let calls = if debug.model_calls.is_empty() {
        debug.model_call.iter().collect::<Vec<_>>()
    } else {
        debug.model_calls.iter().collect::<Vec<_>>()
    };
    for call in calls {
        let source = serde_json::to_string(call).unwrap_or_else(|_| "null".into());
        let preview = format!(
            "{} · {} · {} Pi messages → {} provider messages · {} tools{}{}",
            call.model.provider.as_deref().unwrap_or("unknown provider"),
            call.model.id.as_deref().unwrap_or("unknown model"),
            call.pi_context.as_array().map_or(0, Vec::len),
            call.payload
                .get("messages")
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len),
            call.request_config.tools.len(),
            call.response_metadata
                .last()
                .map_or_else(String::new, |metadata| format!(
                    " · HTTP {}",
                    metadata.status
                )),
            call.response.as_ref().map_or_else(String::new, |response| {
                let usage = response.get("usage").map_or(0, |usage| {
                    usage
                        .get("output")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0)
                });
                format!(" · response captured · {usage} output tokens")
            })
        );
        let turn = call
            .turn
            .map(|turn| format!(" · PI TURN {turn}"))
            .unwrap_or_default();
        let compaction = call
            .compaction_sequence
            .map(|sequence| format!(" · COMPACTION #{sequence}"))
            .unwrap_or_default();
        captures.push(TrajectoryContext {
            lane: TrajectoryLane::ModelCall,
            label: format!("MODEL CALL #{}{turn}{compaction}", call.sequence),
            preview,
            source,
            pi_turn: call.turn,
            captured_at_ms: Some(call.captured_at_ms),
        });
    }

    for compaction in &debug.compactions {
        let source = serde_json::to_string(compaction).unwrap_or_else(|_| "null".into());
        let preview = format!(
            "{} · {} · {} messages summarized · {} tokens before",
            compaction.reason,
            compaction.status,
            compaction.summarized_message_count.unwrap_or(0),
            compaction.tokens_before.unwrap_or(0),
        );
        let turn = compaction
            .turn
            .map(|turn| format!(" · PI TURN {turn}"))
            .unwrap_or_default();
        captures.push(TrajectoryContext {
            lane: TrajectoryLane::Compaction,
            label: format!(
                "COMPACTION #{} · {} · {}{turn}",
                compaction.sequence,
                compaction.reason.to_uppercase(),
                compaction.status.to_uppercase(),
            ),
            preview,
            source,
            pi_turn: compaction.turn,
            captured_at_ms: Some(
                compaction
                    .finished_at_ms
                    .unwrap_or(compaction.started_at_ms),
            ),
        });
    }
    captures.sort_by_key(|capture| capture.captured_at_ms);
    contexts.extend(captures);
    contexts
}

fn trajectory_contexts(session: &AcpView) -> Vec<TrajectoryContext> {
    runtime_debug_contexts(&session.runtime_debug)
}

impl Render for TrajectoryWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session_entity = self.session.clone();
        let (cache_key, rebuilt_events, rebuilt_stats, live_elapsed) = {
            let session = session_entity.read(cx);
            let cache_key = TrajectoryCacheKey::for_session(&session);
            let should_rebuild = self.cached_key != Some(cache_key);
            let rebuilt_events = should_rebuild.then(|| {
                session_trajectory_events(
                    &session.entries,
                    trajectory_contexts(&session),
                    &session.tool_debug,
                )
            });
            let rebuilt_stats = should_rebuild.then(|| {
                (
                    trajectory_counts(&session.entries),
                    session
                        .turn_timings
                        .iter()
                        .filter_map(smelt_core::acp_session::TurnTiming::completed_elapsed_ms)
                        .sum::<u64>(),
                )
            });
            let live_elapsed = if session.has_active_turn() {
                live_elapsed_ms(session.turn_started_at_ms).unwrap_or(0)
            } else {
                0
            };
            (cache_key, rebuilt_events, rebuilt_stats, live_elapsed)
        };
        if let (Some(events), Some(((turns, calls), completed_duration_ms))) =
            (rebuilt_events, rebuilt_stats)
        {
            self.cached_overview = TrajectoryOverview::from_events(&events);
            self.cached_events = events;
            self.cached_turns = turns;
            self.cached_calls = calls;
            self.cached_completed_duration_ms = completed_duration_ms;
            self.cached_key = Some(cache_key);
        }

        let query = self.search.read(cx).value().to_string();
        if self.visible_key != Some(cache_key) || self.last_query != query {
            self.visible_sequences = std::rc::Rc::new(filter_trajectory_event_sequences(
                &self.cached_events,
                &query,
            ));
            self.visible_key = Some(cache_key);
            self.last_query.clone_from(&query);
        }
        let visible_sequences = self.visible_sequences.clone();
        let visible_count = visible_sequences.len();
        let item_count = self.event_list.item_count();
        if item_count != visible_count {
            if visible_count > item_count {
                self.event_list
                    .splice(item_count..item_count, visible_count - item_count);
            } else {
                self.event_list.splice(visible_count..item_count, 0);
            }
        }
        let muted = cx.theme().muted_foreground;
        let list = render_event_ledger(
            visible_sequences,
            self.cached_events.is_empty(),
            self.event_list.clone(),
            cx.entity(),
            muted,
        );

        v_flex()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .bg(gpui::rgb(ui_theme::bg_stage()))
            .child(render_trajectory_toolbar(
                self.cached_turns,
                self.cached_calls,
                self.cached_completed_duration_ms + live_elapsed,
                visible_count,
                self.cached_events.len(),
                &self.search,
                muted,
            ))
            .child(render_sequence_overview(&self.cached_overview, muted))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .overflow_hidden()
                    .child(
                        v_flex()
                            .flex_1()
                            .h_full()
                            .min_h_0()
                            .min_w_0()
                            .overflow_hidden()
                            .child(render_ledger_header(muted))
                            .child(list)
                            .children((!self.cached_events.is_empty()).then(|| {
                                Scrollbar::vertical(&self.event_list)
                                    .id("trajectory-scrollbar")
                                    .mode(ScrollbarMode::Always)
                            })),
                    )
                    .children(
                        self.selected
                            .and_then(|seq| self.cached_events.get(seq))
                            .map(|event| render_inspector(event, self.inspector_tab, muted, cx)),
                    ),
            )
    }
}

fn render_trajectory_toolbar(
    turns: usize,
    calls: usize,
    duration_ms: u64,
    visible_events: usize,
    total_events: usize,
    search: &Entity<InputState>,
    muted: gpui::Hsla,
) -> gpui::Div {
    let event_count = if visible_events == total_events {
        format!("{total_events} events")
    } else {
        format!("{visible_events} / {total_events} events")
    };
    h_flex()
        .h(px(44.))
        .flex_shrink_0()
        .w_full()
        .px_4()
        .gap_3()
        .items_center()
        .border_b_1()
        .border_color(ui_theme::card_stroke())
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Icon::new(IconName::Inspector)
                        .size(px(14.))
                        .text_color(gpui::rgb(ui_theme::text_mid())),
                )
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(gpui::rgb(ui_theme::text_bright()))
                        .child("会话追踪"),
                ),
        )
        .child(div().w(px(1.)).h(px(14.)).bg(ui_theme::card_stroke()))
        .child(
            h_flex()
                .gap_3()
                .text_xs()
                .text_color(muted)
                .child(stat_text("DURATION", format_duration(duration_ms)))
                .child(stat_text("TURNS", turns.to_string()))
                .child(stat_text("CALLS", calls.to_string()))
                .child(stat_text("EVENTS", event_count)),
        )
        .child(div().flex_1())
        .child(div().w(px(240.)).child(Input::new(search).cleanable(true)))
}

fn stat_text(label: &'static str, value: String) -> gpui::Div {
    h_flex()
        .gap_1()
        .child(
            div()
                .text_color(gpui::rgb(ui_theme::text_muted()))
                .child(label),
        )
        .child(
            div()
                .font_medium()
                .text_color(gpui::rgb(ui_theme::text_mid()))
                .child(value),
        )
}

fn render_ledger_header(muted: gpui::Hsla) -> gpui::Div {
    h_flex()
        .h(px(28.))
        .flex_shrink_0()
        .w_full()
        .px_3()
        .border_b_1()
        .border_color(ui_theme::card_stroke())
        .bg(ui_theme::overlay(0x0a))
        .text_xs()
        .font_medium()
        .text_color(muted)
        .child(div().w(px(64.)).flex_shrink_0().child("TURN"))
        .child(div().w(px(116.)).flex_shrink_0().child("EVENT"))
        .child(div().flex_1().min_w_0().child("SUMMARY"))
        .child(div().w(px(48.)).flex_shrink_0().text_right().child("SEQ"))
}

fn render_event_ledger(
    visible_sequences: std::rc::Rc<Vec<usize>>,
    all_events_empty: bool,
    list_state: ListState,
    view: Entity<TrajectoryWindow>,
    muted: gpui::Hsla,
) -> gpui::AnyElement {
    if visible_sequences.is_empty() {
        return v_flex()
            .w_full()
            .items_center()
            .justify_center()
            .py_12()
            .gap_2()
            .child(Icon::new(IconName::Search).size(px(18.)).text_color(muted))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(if all_events_empty {
                        "还没有会话事件"
                    } else {
                        "没有匹配的事件"
                    }),
            )
            .into_any_element();
    }

    virtual_list(list_state, move |index, _window, app| {
        let visible_sequences = visible_sequences.clone();
        let view = view.clone();
        view.update(app, move |this, cx| {
            let Some(seq) = visible_sequences.get(index).copied() else {
                return div().into_any_element();
            };
            let events = &this.cached_events;
            let Some(event) = events.get(seq) else {
                return div().into_any_element();
            };
            let begins_group = index == 0
                || visible_sequences
                    .get(index - 1)
                    .and_then(|previous| events.get(*previous))
                    .is_none_or(|previous| previous.turn != event.turn);
            let ends_group = visible_sequences
                .get(index + 1)
                .and_then(|next| events.get(*next))
                .is_none_or(|next| next.turn != event.turn);
            render_ledger_row(
                event,
                begins_group,
                ends_group,
                this.selected == Some(event.seq),
                muted,
                cx,
            )
            .into_any_element()
        })
    })
    .w_full()
    .flex_1()
    .min_h_0()
    .into_any_element()
}

fn render_ledger_row(
    event: &TrajectoryEvent,
    begins_group: bool,
    ends_group: bool,
    selected: bool,
    muted: gpui::Hsla,
    cx: &mut Context<TrajectoryWindow>,
) -> impl IntoElement {
    let seq = event.seq;
    let lane_color = trajectory_lane_color(event.lane);
    let turn_label = if event.turn == 0 {
        "CAP".to_string()
    } else {
        format!("T{:02}", event.turn)
    };
    let indent = event.depth as f32 * 14.;

    h_flex()
        .id(("trajectory-event", seq))
        .relative()
        .min_h(px(34.))
        .w_full()
        .px_3()
        .items_center()
        .border_b_1()
        .border_color(ui_theme::overlay(0x10))
        .cursor_pointer()
        .when(selected, |row| row.bg(ui_theme::overlay(0x1c)))
        .hover(|row| row.bg(ui_theme::overlay(0x10)))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.selected = Some(seq);
            cx.notify();
        }))
        .when(selected, |row| {
            row.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(2.))
                    .bg(gpui::rgb(ui_theme::accent())),
            )
        })
        .child(
            h_flex()
                .relative()
                .self_stretch()
                .w(px(64.))
                .flex_shrink_0()
                .items_center()
                .child(
                    div()
                        .absolute()
                        .left(px(4.))
                        .when(!begins_group, |line| line.top_0())
                        .when(begins_group, |line| line.top(px(17.)))
                        .when(!ends_group, |line| line.bottom_0())
                        .when(ends_group, |line| line.bottom(px(17.)))
                        .w(px(1.))
                        .bg(ui_theme::overlay(0x34)),
                )
                .child(
                    div()
                        .ml(px(13.))
                        .text_xs()
                        .font_medium()
                        .text_color(muted)
                        .when(begins_group, |label| {
                            label.text_color(gpui::rgb(ui_theme::text_mid()))
                        })
                        .child(if begins_group {
                            turn_label
                        } else {
                            String::new()
                        }),
                ),
        )
        .child(
            h_flex()
                .w(px(116.))
                .flex_shrink_0()
                .gap_2()
                .items_center()
                .child(div().size(px(6.)).rounded_full().bg(gpui::rgb(lane_color)))
                .child(
                    div()
                        .text_xs()
                        .font_medium()
                        .text_color(gpui::rgb(lane_color))
                        .child(trajectory_lane_compact_label(event.lane)),
                ),
        )
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .when(event.depth > 0, |content| {
                    content
                        .child(div().flex_shrink_0().w(px(indent)))
                        .child(div().mr_2().text_xs().text_color(muted).child("└"))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(gpui::rgb(ui_theme::text_mid()))
                        .child(event.text.clone()),
                ),
        )
        .child(
            div()
                .w(px(48.))
                .flex_shrink_0()
                .text_right()
                .text_xs()
                .font_family(smelt_core::font_config::font_family())
                .text_color(muted)
                .child(format!("{:03}", event.seq + 1)),
        )
}

fn trajectory_lane_compact_label(lane: TrajectoryLane) -> &'static str {
    match lane {
        TrajectoryLane::ModelCall => "MODEL",
        TrajectoryLane::Compaction => "COMPACT",
        TrajectoryLane::Request => "CONFIG",
        other => trajectory_lane_label(other),
    }
}

fn render_inspector(
    event: &TrajectoryEvent,
    tab: InspectorTab,
    muted: gpui::Hsla,
    cx: &mut Context<TrajectoryWindow>,
) -> gpui::Div {
    let preview = tab == InspectorTab::Preview;
    let kind_zh = match event.lane {
        TrajectoryLane::ModelCall => "模型调用 · 请求 / 响应",
        TrajectoryLane::Compaction => "Pi 上报的上下文压缩过程",
        TrajectoryLane::Request => "Pi 请求组装配置",
        TrajectoryLane::User => "用户消息",
        TrajectoryLane::Assistant => "模型输出",
        TrajectoryLane::Thinking => "模型思考",
        TrajectoryLane::Tool => "工具调用",
    };
    let turn_value = if event.turn == 0 {
        "非时间线".to_string()
    } else {
        format!("Turn {}", event.turn)
    };
    let body = if preview {
        event.preview.clone()
    } else {
        serde_json::from_str::<serde_json::Value>(&event.source)
            .map(|source| pretty_json(&source))
            .unwrap_or_else(|_| event.source.clone())
    };

    v_flex()
        .w(px(440.))
        .h_full()
        .min_h_0()
        .flex_shrink_0()
        .overflow_hidden()
        .border_l_1()
        .border_color(ui_theme::card_stroke())
        .bg(gpui::rgb(ui_theme::bg_stage()))
        .child(
            v_flex()
                .flex_shrink_0()
                .px_4()
                .pt_3()
                .pb_2()
                .gap_2()
                .border_b_1()
                .border_color(ui_theme::card_stroke())
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .size(px(7.))
                                .rounded_full()
                                .bg(gpui::rgb(trajectory_lane_color(event.lane))),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_semibold()
                                .text_color(gpui::rgb(trajectory_lane_color(event.lane)))
                                .child(trajectory_lane_label(event.lane)),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .id("trajectory-inspector-close")
                                .size(px(22.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.))
                                .cursor_pointer()
                                .hover(|d| d.bg(ui_theme::overlay(0x20)))
                                .child(Icon::new(IconName::Close).size(px(12.)))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.selected = None;
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    div()
                        .text_base()
                        .font_semibold()
                        .text_color(gpui::rgb(ui_theme::text_bright()))
                        .child(event.text.clone()),
                )
                .child(div().text_xs().text_color(muted).child(kind_zh)),
        )
        .child(
            h_flex()
                .h(px(34.))
                .flex_shrink_0()
                .px_4()
                .gap_5()
                .items_center()
                .border_b_1()
                .border_color(ui_theme::card_stroke())
                .text_xs()
                .child(inspector_meta(
                    "SEQ",
                    format!("{:03}", event.seq + 1),
                    muted,
                ))
                .child(inspector_meta("SCOPE", turn_value, muted))
                .when(event.pi_turn.is_some(), |row| {
                    row.child(inspector_meta(
                        "PI TURN",
                        event.pi_turn.unwrap_or_default().to_string(),
                        muted,
                    ))
                })
                .when(event.depth > 0, |row| {
                    row.child(inspector_meta("DEPTH", event.depth.to_string(), muted))
                }),
        )
        .child(
            h_flex()
                .h(px(36.))
                .flex_shrink_0()
                .px_4()
                .gap_5()
                .items_end()
                .border_b_1()
                .border_color(ui_theme::card_stroke())
                .child(inspector_tab_label(
                    "预览",
                    preview,
                    InspectorTab::Preview,
                    cx,
                ))
                .child(inspector_tab_label(
                    "原始数据",
                    !preview,
                    InspectorTab::Source,
                    cx,
                )),
        )
        .child(
            div()
                .id("trajectory-inspector-body")
                .flex_1()
                .min_h_0()
                .min_w_0()
                .overflow_y_scroll()
                .when(!preview, |body| body.overflow_x_scroll())
                .child(
                    div()
                        .p_4()
                        .text_sm()
                        .line_height(px(20.))
                        .text_color(gpui::rgb(ui_theme::text_mid()))
                        .when(!preview, |source| {
                            source
                                .font_family(smelt_core::font_config::font_family())
                                .text_xs()
                        })
                        .child(body),
                ),
        )
}

fn inspector_meta(label: &'static str, value: String, muted: gpui::Hsla) -> gpui::Div {
    h_flex()
        .gap_1()
        .child(div().text_color(muted).child(label))
        .child(
            div()
                .font_medium()
                .text_color(gpui::rgb(ui_theme::text_mid()))
                .child(value),
        )
}

fn inspector_tab_label(
    label: &'static str,
    active: bool,
    tab: InspectorTab,
    cx: &mut Context<TrajectoryWindow>,
) -> impl IntoElement {
    div()
        .id(if matches!(tab, InspectorTab::Preview) {
            "trajectory-tab-preview"
        } else {
            "trajectory-tab-source"
        })
        .h(px(36.))
        .flex()
        .items_center()
        .cursor_pointer()
        .text_sm()
        .when(active, |tab_el| {
            tab_el
                .font_medium()
                .text_color(gpui::rgb(ui_theme::text_bright()))
                .border_b_2()
                .border_color(gpui::rgb(ui_theme::accent()))
        })
        .when(!active, |tab_el| {
            tab_el.text_color(gpui::rgb(ui_theme::text_muted()))
        })
        .child(label)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.inspector_tab = tab;
            cx.notify();
        }))
}

fn render_sequence_overview(overview: &TrajectoryOverview, muted: gpui::Hsla) -> gpui::Div {
    v_flex()
        .flex_shrink_0()
        .w_full()
        .px_4()
        .py_2()
        .gap_1()
        .border_b_1()
        .border_color(ui_theme::card_stroke())
        .bg(ui_theme::overlay(0x06))
        .child(
            h_flex()
                .h(px(16.))
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(gpui::rgb(ui_theme::text_mid()))
                        .child("SEQUENCE OVERVIEW"),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("{} ordered events", overview.ordered_event_count)),
                ),
        )
        .child(overview_lane(
            "INPUT",
            ui_theme::blue(),
            &overview.bins,
            |bin| bin.input,
        ))
        .child(overview_lane(
            "MODEL",
            ui_theme::purple(),
            &overview.bins,
            |bin| bin.model,
        ))
        .child(overview_lane(
            "TOOLS",
            ui_theme::green(),
            &overview.bins,
            |bin| bin.tools,
        ))
}

fn overview_lane(
    label: &'static str,
    color: u32,
    bins: &[OverviewBin],
    belongs: impl Fn(OverviewBin) -> bool,
) -> gpui::Div {
    let mut track = h_flex().flex_1().h(px(7.)).gap(px(1.)).items_center();
    if bins.is_empty() {
        track = track.child(div().flex_1().h(px(3.)).bg(ui_theme::overlay(0x14)));
    } else {
        for bin in bins {
            let cell = div().flex_1().h(px(5.)).min_w(px(3.));
            track = track.child(if belongs(*bin) {
                cell.bg(gpui::rgb(color))
            } else {
                cell.bg(ui_theme::overlay(0x0c))
            });
        }
    }
    h_flex()
        .w_full()
        .h(px(9.))
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(48.))
                .flex_shrink_0()
                .text_xs()
                .font_medium()
                .text_color(gpui::rgb(color))
                .child(label),
        )
        .child(track)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_overview_bounds_rendered_bins_without_dropping_event_count() {
        let events = (0..4_000)
            .map(|index| TrajectoryEvent {
                seq: index,
                lane: match index % 3 {
                    0 => TrajectoryLane::User,
                    1 => TrajectoryLane::Assistant,
                    _ => TrajectoryLane::Tool,
                },
                turn: 1,
                depth: 0,
                parent_tool_id: None,
                text: String::new(),
                preview: String::new(),
                source: String::new(),
                pi_turn: None,
                captured_at_ms: None,
            })
            .collect::<Vec<_>>();
        let overview = TrajectoryOverview::from_events(&events);

        assert_eq!(overview.ordered_event_count, 4_000);
        assert_eq!(overview.bins.len(), 256);
        assert!(overview.bins.iter().any(|bin| bin.input));
        assert!(overview.bins.iter().any(|bin| bin.model));
        assert!(overview.bins.iter().any(|bin| bin.tools));
    }
}
