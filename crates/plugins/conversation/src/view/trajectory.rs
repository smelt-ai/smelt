//! 独立 Trajectory 窗口：参考 DSH `ui-trajectory` 的账本 + 时间概览。
//!
//! 只投影这场会话已经发生的事，不编造耗时。工具级起止时间我们没有，
//! Overview 用 sequence 等宽色块（DSH 也有 sequence 模式），账本按 Turn 分组。

use super::*;
use crate::acp_client::load_acp_debug;
use rust_i18n::t;
use smelt_plugin_api::model::acp_session::{ConversationDebug, RuntimeDebug};
use smelt_plugin_api::ui::field::{InputEvent, InputState};
use smelt_plugin_api::ui::kit::TYPE;
use smelt_plugin_api::ui::truncated_line::truncated_line;
use smelt_plugin_api::ui::{Divider, Segmented, TextInput};

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
    /// 每拉回一份调试数据加一。
    debug_revision: u64,
}

impl TrajectoryCacheKey {
    fn for_session(session: &ConversationView, debug_revision: u64) -> Self {
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
            debug_revision,
        }
    }
}

/// 什么时候该重拉调试数据：回合中只看条目增减，回合结束再按修订号补一次，
/// 和上面重建缓存的节奏一致。
fn debug_refresh_key(session: &ConversationView) -> (u64, usize) {
    let revision = if session.has_active_turn() {
        0
    } else {
        session.last_snapshot_revision
    };
    (revision, session.entries_total)
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
        for (position, event) in events.iter().filter(|event| event.turn > 0).enumerate() {
            let bin = position * bin_count / ordered_event_count;
            let cell = &mut bins[bin.min(bin_count - 1)];
            match event.lane {
                TrajectoryLane::User => cell.input = true,
                TrajectoryLane::Assistant | TrajectoryLane::Thinking => cell.model = true,
                TrajectoryLane::Tool => cell.tools = true,
                _ => {}
            }
        }
        Self {
            ordered_event_count,
            bins,
        }
    }
}

pub(super) struct TrajectoryWindow {
    session: Entity<ConversationView>,
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
    /// 工具参数表和 runtime debug。会话链路不推它们，窗口自己用 `acp_debug` 拉。
    debug: ConversationDebug,
    debug_revision: u64,
    debug_error: Option<String>,
    /// 上次发请求时会话的 [`debug_refresh_key`]；变了才重拉。
    debug_key: Option<(u64, usize)>,
    debug_loading: bool,
    /// 请求在途期间会话又变了：回来后再补拉一次。
    debug_stale: bool,
    _observe: gpui::Subscription,
    _search: gpui::Subscription,
}

impl TrajectoryWindow {
    fn new(session: Entity<ConversationView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let _search = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let _observe = cx.observe(&session, |this, session, cx| {
            if this.debug_key != Some(debug_refresh_key(session.read(cx))) {
                this.request_debug(cx);
            }
            cx.notify();
        });
        let mut this = Self {
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
            debug: ConversationDebug::default(),
            debug_revision: 0,
            debug_error: None,
            debug_key: None,
            debug_loading: false,
            debug_stale: false,
            _observe,
            _search,
        };
        this.request_debug(cx);
        this
    }

    /// 后台拉一次调试数据。同时只有一个请求在途，期间的变化记一笔，回来后补拉。
    fn request_debug(&mut self, cx: &mut Context<Self>) {
        if self.debug_loading {
            self.debug_stale = true;
            return;
        }
        let session = self.session.read(cx);
        self.debug_key = Some(debug_refresh_key(session));
        self.debug_loading = true;
        let runtime = session.runtime.client().clone();
        let sid = session.sid.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { load_acp_debug(&runtime, &sid) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.debug_loading = false;
                match result {
                    Ok(debug) => {
                        this.debug = debug;
                        this.debug_revision += 1;
                        this.debug_error = None;
                    }
                    Err(error) => this.debug_error = Some(error),
                }
                if std::mem::take(&mut this.debug_stale) {
                    this.request_debug(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl ConversationView {
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
                    title: Some(t!("trace.open").into()),
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

impl Render for TrajectoryWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session_entity = self.session.clone();
        let (cache_key, rebuilt_events, rebuilt_stats, live_elapsed) = {
            let session = session_entity.read(cx);
            let cache_key = TrajectoryCacheKey::for_session(session, self.debug_revision);
            let should_rebuild = self.cached_key != Some(cache_key);
            let rebuilt_events = should_rebuild.then(|| {
                session_trajectory_events(
                    &session.entries,
                    runtime_debug_contexts(&self.debug.runtime_debug),
                    &self.debug.tool_debug,
                )
            });
            let rebuilt_stats = should_rebuild.then(|| {
                (
                    trajectory_counts(&session.entries),
                    session
                        .turn_timings
                        .iter()
                        .filter_map(
                            smelt_plugin_api::model::acp_session::TurnTiming::completed_elapsed_ms,
                        )
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
        let muted: gpui::Hsla = gpui::rgb(ui_theme::muted()).into();
        let list = render_event_ledger(
            visible_sequences,
            self.cached_events.is_empty(),
            self.event_list.clone(),
            cx.entity(),
            muted,
        );

        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .bg(gpui::rgb(ui_theme::bg()))
            .child(render_trajectory_toolbar(
                self.cached_turns,
                self.cached_calls,
                self.cached_completed_duration_ms + live_elapsed,
                visible_count,
                self.cached_events.len(),
                &self.search,
                muted,
            ))
            .children(self.debug_error.as_ref().map(|error| {
                div()
                    .flex_shrink_0()
                    .px_4()
                    .py_1()
                    .type_style(TYPE.text_xsmall)
                    .text_color(muted)
                    .child(SharedString::from(t!(
                        "trace.debug_unavailable",
                        error = error
                    )))
            }))
            .child(render_sequence_overview(&self.cached_overview, muted))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .flex_col()
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
    div()
        .flex()
        .items_center()
        .h(ui_theme::height::xl())
        .flex_shrink_0()
        .w_full()
        .px_4()
        .gap_3()
        .border_b_1()
        .border_color(ui_theme::hair())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    Icon::new(IconName::Inspector)
                        .size(px(14.))
                        .text_color(gpui::rgb(ui_theme::muted())),
                )
                .child(
                    div()
                        .type_style(TYPE.ui_medium)
                        .text_color(gpui::rgb(ui_theme::text()))
                        .child(t!("trace.open").into_owned()),
                ),
        )
        .child(Divider::vertical().h(px(14.)))
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child(stat_text("DURATION", format_duration(duration_ms)))
                .child(stat_text("TURNS", turns.to_string()))
                .child(stat_text("CALLS", calls.to_string()))
                .child(stat_text("EVENTS", event_count)),
        )
        .child(div().flex_1())
        .child(
            div()
                .w(px(240.))
                .child(TextInput::new(search).small().cleanable(true)),
        )
}

fn stat_text(label: &'static str, value: String) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(div().text_color(gpui::rgb(ui_theme::muted())).child(label))
        .child(
            div()
                .font_medium()
                .text_color(gpui::rgb(ui_theme::muted()))
                .child(value),
        )
}

fn render_ledger_header(muted: gpui::Hsla) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .h(ui_theme::height::sm())
        .flex_shrink_0()
        .w_full()
        .px_3()
        .border_b_1()
        .border_color(ui_theme::hair())
        .bg(ui_theme::overlay(0x0a))
        .type_style(TYPE.text_xsmall_medium)
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
        return div()
            .flex()
            .flex_col()
            .w_full()
            .items_center()
            .justify_center()
            .py_12()
            .gap_2()
            .child(Icon::new(IconName::Search).size(px(18.)).text_color(muted))
            .child(
                div()
                    .type_style(TYPE.ui)
                    .text_color(muted)
                    .child(SharedString::from(if all_events_empty {
                        t!("trace.no_events")
                    } else {
                        t!("trace.no_matches")
                    })),
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

    div()
        .flex()
        .items_center()
        .id(("trajectory-event", seq))
        .relative()
        .min_h(px(34.))
        .w_full()
        .px_3()
        .border_b_1()
        .border_color(ui_theme::overlay(0x10))
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
                    .bg(gpui::rgb(ui_theme::ink())),
            )
        })
        .child(
            div()
                .flex()
                .items_center()
                .relative()
                .self_stretch()
                .w(px(64.))
                .flex_shrink_0()
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
                        .type_style(TYPE.text_xsmall_medium)
                        .text_color(muted)
                        .when(begins_group, |label| {
                            label.text_color(gpui::rgb(ui_theme::muted()))
                        })
                        .child(if begins_group {
                            turn_label
                        } else {
                            String::new()
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .w(px(116.))
                .flex_shrink_0()
                .gap_2()
                .child(div().size(px(6.)).rounded_full().bg(gpui::rgb(lane_color)))
                .child(
                    div()
                        .type_style(TYPE.text_xsmall_medium)
                        .text_color(gpui::rgb(lane_color))
                        .child(trajectory_lane_compact_label(event.lane)),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .flex_1()
                .min_w_0()
                .when(event.depth > 0, |content| {
                    content.child(div().flex_shrink_0().w(px(indent))).child(
                        div()
                            .mr_2()
                            .type_style(TYPE.text_xsmall)
                            .text_color(muted)
                            .child("└"),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .type_style(TYPE.ui)
                        .text_color(gpui::rgb(ui_theme::muted()))
                        .child(truncated_line(event.text.clone())),
                ),
        )
        .child(
            div()
                .w(px(48.))
                .flex_shrink_0()
                .text_right()
                .type_style(TYPE.text_xsmall)
                .font_family(smelt_plugin_api::model::font_config::font_family())
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
    let kind_text = match event.lane {
        TrajectoryLane::ModelCall => t!("trace.lane.model_call"),
        TrajectoryLane::Compaction => t!("trace.lane.compaction"),
        TrajectoryLane::Request => t!("trace.lane.request"),
        TrajectoryLane::User => t!("trace.lane.user"),
        TrajectoryLane::Assistant => t!("trace.lane.assistant"),
        TrajectoryLane::Thinking => t!("trace.lane.thinking"),
        TrajectoryLane::Tool => t!("trace.lane.tool"),
    };
    let turn_value = if event.turn == 0 {
        t!("trace.off_timeline").into_owned()
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

    div()
        .flex()
        .flex_col()
        .w(px(440.))
        .h_full()
        .min_h_0()
        .flex_shrink_0()
        .overflow_hidden()
        .border_l_1()
        .border_color(ui_theme::hair())
        .bg(gpui::rgb(ui_theme::bg()))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_shrink_0()
                .px_4()
                .pt_3()
                .pb_2()
                .gap_2()
                .border_b_1()
                .border_color(ui_theme::hair())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .size(px(7.))
                                .rounded_full()
                                .bg(gpui::rgb(trajectory_lane_color(event.lane))),
                        )
                        .child(
                            div()
                                .type_style(TYPE.text_xsmall_medium)
                                .text_color(gpui::rgb(trajectory_lane_color(event.lane)))
                                .child(trajectory_lane_label(event.lane)),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("trajectory-inspector-close")
                                .dismiss()
                                .icon(IconName::Close)
                                .icon_action(t!("action.close"))
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
                        .text_color(gpui::rgb(ui_theme::text()))
                        .child(event.text.clone()),
                )
                .child(
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(muted)
                        .child(SharedString::from(kind_text)),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .h(px(34.))
                .flex_shrink_0()
                .px_4()
                .gap_5()
                .border_b_1()
                .border_color(ui_theme::hair())
                .type_style(TYPE.text_xsmall)
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
            div()
                .flex()
                .items_center()
                .w_full()
                .h(ui_theme::height::lg())
                .flex_shrink_0()
                .px_4()
                .border_b_1()
                .border_color(ui_theme::hair())
                .child(
                    Segmented::new("trajectory-inspector-tabs")
                        .segments([t!("trace.preview").to_string(), t!("trace.raw").to_string()])
                        .selected(Some(if preview { 0 } else { 1 }))
                        .on_select({
                            let view = cx.entity();
                            move |ix, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.inspector_tab = if ix == 0 {
                                        InspectorTab::Preview
                                    } else {
                                        InspectorTab::Source
                                    };
                                    cx.notify();
                                });
                            }
                        }),
                ),
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
                        .type_style(TYPE.ui)
                        .line_height(px(20.))
                        .text_color(gpui::rgb(ui_theme::muted()))
                        .when(!preview, |source| {
                            source
                                .font_family(smelt_plugin_api::model::font_config::font_family())
                                .type_style(TYPE.text_xsmall)
                        })
                        .child(body),
                ),
        )
}

fn inspector_meta(label: &'static str, value: String, muted: gpui::Hsla) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(div().text_color(muted).child(label))
        .child(
            div()
                .font_medium()
                .text_color(gpui::rgb(ui_theme::muted()))
                .child(value),
        )
}

fn render_sequence_overview(overview: &TrajectoryOverview, muted: gpui::Hsla) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .w_full()
        .px_4()
        .py_2()
        .gap_1()
        .border_b_1()
        .border_color(ui_theme::hair())
        .bg(ui_theme::overlay(0x06))
        .child(
            div()
                .flex()
                .items_center()
                .h(px(16.))
                .child(
                    div()
                        .type_style(TYPE.text_xsmall_medium)
                        .text_color(gpui::rgb(ui_theme::muted()))
                        .child("SEQUENCE OVERVIEW"),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(muted)
                        .child(format!("{} ordered events", overview.ordered_event_count)),
                ),
        )
        .child(overview_lane(
            "INPUT",
            ui_theme::chart(0),
            &overview.bins,
            |bin| bin.input,
        ))
        .child(overview_lane(
            "MODEL",
            ui_theme::chart(3),
            &overview.bins,
            |bin| bin.model,
        ))
        .child(overview_lane(
            "TOOLS",
            ui_theme::chart(1),
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
    let mut track = div().flex().items_center().flex_1().h(px(7.)).gap(px(1.));
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
    div()
        .flex()
        .items_center()
        .w_full()
        .h(px(9.))
        .gap_3()
        .child(
            div()
                .w(px(48.))
                .flex_shrink_0()
                .type_style(TYPE.text_xsmall_medium)
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
