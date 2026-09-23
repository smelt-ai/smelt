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

pub(super) struct TrajectoryWindow {
    session: Entity<AcpView>,
    scroll: ScrollHandle,
    search: Entity<InputState>,
    selected: Option<usize>,
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
            scroll: ScrollHandle::new(),
            search,
            selected: None,
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
    if let Some(call) = &debug.model_call {
        let source = serde_json::to_value(call).unwrap_or(serde_json::Value::Null);
        contexts.push(TrajectoryContext {
            lane: TrajectoryLane::ModelCall,
            label: format!("MODEL CALL #{}", call.sequence),
            preview: pretty_json(&source),
            source,
        });
    }

    if let Some(system_prompt) = debug.system_prompt.as_deref() {
        let source = serde_json::json!({
            "version": debug.version,
            "source": debug.source,
            "systemPrompt": system_prompt,
            "tools": debug.tools,
        });
        contexts.push(TrajectoryContext {
            lane: TrajectoryLane::Request,
            label: "Pi REQUEST CONFIG".to_string(),
            preview: pretty_json(&source),
            source,
        });
    }
    contexts
}

fn trajectory_contexts(session: &AcpView) -> Vec<TrajectoryContext> {
    runtime_debug_contexts(&session.runtime_debug)
}

impl Render for TrajectoryWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let contexts = trajectory_contexts(session);
        let events = session_trajectory_events(&session.entries, contexts, &session.tool_debug);
        let query = self.search.read(cx).value().to_string();
        let visible = filter_trajectory_events(&events, &query);
        let (turns, calls) = trajectory_counts(&session.entries);
        let duration_ms = session
            .turn_timings
            .iter()
            .filter_map(smelt_core::acp_session::TurnTiming::completed_elapsed_ms)
            .sum::<u64>()
            + if session.has_active_turn() {
                live_elapsed_ms(session.turn_started_at_ms).unwrap_or(0)
            } else {
                0
            };
        let muted = cx.theme().muted_foreground;
        let list = render_event_ledger(&visible, &events, self.selected, muted, cx);

        v_flex()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .bg(gpui::rgb(ui_theme::bg_stage()))
            .child(render_trajectory_toolbar(
                turns,
                calls,
                duration_ms,
                visible.len(),
                events.len(),
                &self.search,
                muted,
            ))
            .child(render_sequence_overview(&events, muted))
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
                            .child(
                                div()
                                    .id("trajectory-scroll")
                                    .relative()
                                    .flex_1()
                                    .min_h_0()
                                    .min_w_0()
                                    .overflow_y_scroll()
                                    .track_scroll(&self.scroll)
                                    .child(list),
                            ),
                    )
                    .children(self.selected.and_then(|seq| {
                        events
                            .iter()
                            .find(|event| event.seq == seq)
                            .cloned()
                            .map(|event| render_inspector(event, self.inspector_tab, muted, cx))
                    })),
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
    visible: &[&TrajectoryEvent],
    all_events: &[TrajectoryEvent],
    selected: Option<usize>,
    muted: gpui::Hsla,
    cx: &mut Context<TrajectoryWindow>,
) -> gpui::Div {
    if visible.is_empty() {
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
                    .child(if all_events.is_empty() {
                        "还没有会话事件"
                    } else {
                        "没有匹配的事件"
                    }),
            );
    }

    let mut ledger = v_flex().w_full();
    let mut previous_turn = usize::MAX;
    for (index, event) in visible.iter().enumerate() {
        let begins_group = event.turn != previous_turn;
        previous_turn = event.turn;
        let ends_group = visible
            .get(index + 1)
            .is_none_or(|next| next.turn != event.turn);
        ledger = ledger.child(render_ledger_row(
            event,
            begins_group,
            ends_group,
            selected == Some(event.seq),
            muted,
            cx,
        ));
    }
    ledger
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
        TrajectoryLane::Request => "CONFIG",
        other => trajectory_lane_label(other),
    }
}

fn render_inspector(
    event: TrajectoryEvent,
    tab: InspectorTab,
    muted: gpui::Hsla,
    cx: &mut Context<TrajectoryWindow>,
) -> gpui::Div {
    let preview = tab == InspectorTab::Preview;
    let kind_zh = match event.lane {
        TrajectoryLane::ModelCall => "模型调用 · provider payload",
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
        event.source.clone()
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

fn render_sequence_overview(events: &[TrajectoryEvent], muted: gpui::Hsla) -> gpui::Div {
    // Runtime captures 没有可靠 turn 外键，不在 sequence navigator 里冒充执行步骤。
    let timeline_events = events
        .iter()
        .filter(|event| event.turn > 0)
        .collect::<Vec<_>>();
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
                        .child(format!("{} ordered events", timeline_events.len())),
                ),
        )
        .child(overview_lane(
            "INPUT",
            ui_theme::blue(),
            &timeline_events,
            |lane| lane == TrajectoryLane::User,
        ))
        .child(overview_lane(
            "MODEL",
            ui_theme::purple(),
            &timeline_events,
            |lane| matches!(lane, TrajectoryLane::Assistant | TrajectoryLane::Thinking),
        ))
        .child(overview_lane(
            "TOOLS",
            ui_theme::green(),
            &timeline_events,
            |lane| lane == TrajectoryLane::Tool,
        ))
}

fn overview_lane(
    label: &'static str,
    color: u32,
    events: &[&TrajectoryEvent],
    belongs: impl Fn(TrajectoryLane) -> bool,
) -> gpui::Div {
    let mut track = h_flex().flex_1().h(px(7.)).gap(px(1.)).items_center();
    if events.is_empty() {
        track = track.child(div().flex_1().h(px(3.)).bg(ui_theme::overlay(0x14)));
    } else {
        for event in events {
            let cell = div().flex_1().h(px(5.)).min_w(px(3.));
            track = track.child(if belongs(event.lane) {
                cell.bg(gpui::rgb(trajectory_lane_color(event.lane)))
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
