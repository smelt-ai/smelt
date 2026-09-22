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
                window_bounds: Some(WindowBounds::centered(size(px(960.), px(720.)), cx)),
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
        let t = cx.theme();
        let muted = t.muted_foreground;
        let overview = render_sequence_overview(&events);
        let mut list = v_flex().w_full();
        if visible.is_empty() {
            list = list.child(div().px_5().py_8().text_sm().text_color(muted).child(
                if events.is_empty() {
                    "还没有轨迹"
                } else {
                    "没有匹配的记录"
                },
            ));
        } else {
            let mut last_turn = usize::MAX;
            let mut showed_debug_header = false;
            for event in visible.iter() {
                if event.turn == 0 && !showed_debug_header {
                    showed_debug_header = true;
                    list = list.child(
                        h_flex().w_full().px_5().pt_3().pb_1().child(
                            div()
                                .text_xs()
                                .font_medium()
                                .text_color(muted)
                                .child("Runtime captures · 非时间线"),
                        ),
                    );
                }
                if event.turn != last_turn && event.turn > 0 {
                    last_turn = event.turn;
                    list = list.child(
                        h_flex()
                            .id(("trajectory-turn", event.turn))
                            .w_full()
                            .px_5()
                            .pt_3()
                            .pb_1()
                            .child(
                                div()
                                    .px_2()
                                    .py(px(1.))
                                    .rounded(px(4.))
                                    .bg(ui_theme::overlay(0x18))
                                    .text_xs()
                                    .font_medium()
                                    .text_color(muted)
                                    .child(format!("Turn {}", event.turn)),
                            ),
                    );
                }
                let label = trajectory_lane_label(event.lane);
                let badge_bg = trajectory_lane_color(event.lane);
                let seq = event.seq;
                let selected = self.selected == Some(seq);
                let indent = event.depth as f32 * 16.;
                list = list.child(
                    h_flex()
                        .id(("trajectory-event", seq))
                        .w_full()
                        .px_5()
                        .py_2()
                        .gap_3()
                        .items_start()
                        .cursor_pointer()
                        .when(selected, |row| row.bg(ui_theme::overlay(0x18)))
                        .hover(|row| row.bg(ui_theme::overlay(0x10)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.selected = Some(seq);
                            cx.notify();
                        }))
                        .when(event.depth > 0, |row| {
                            row.child(div().flex_shrink_0().w(px(indent)))
                        })
                        .child(
                            div()
                                .flex_shrink_0()
                                .w(px(88.))
                                .px_2()
                                .py(px(2.))
                                .rounded(px(4.))
                                .bg(ui_theme::tint(badge_bg, 0x44))
                                .text_xs()
                                .font_semibold()
                                .text_color(gpui::rgb(ui_theme::text_bright()))
                                .child(label),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .text_color(gpui::rgb(ui_theme::text_mid()))
                                .child(event.text.clone()),
                        ),
                );
            }
        }
        v_flex()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .bg(gpui::rgb(ui_theme::bg_stage()))
            .child(
                v_flex()
                    .flex_shrink_0()
                    .w_full()
                    .px_5()
                    .pt_3()
                    .pb_2()
                    .gap_3()
                    .bg(gpui::rgb(ui_theme::bg_stage()))
                    .border_b_1()
                    .border_color(ui_theme::card_stroke())
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap_4()
                            .child(
                                h_flex()
                                    .gap_4()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(
                                        h_flex()
                                            .gap_1()
                                            .items_center()
                                            .child(Icon::new(IconName::LoaderCircle).size(px(12.)))
                                            .child(format!(
                                                "Duration {}",
                                                format_duration(duration_ms)
                                            )),
                                    )
                                    .child(format!("Turns {turns}"))
                                    .child(format!("Calls {calls}")),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .w(px(220.))
                                    .child(Input::new(&self.search).cleanable(true)),
                            ),
                    )
                    .child(overview),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .overflow_hidden()
                    .child(
                        div()
                            .id("trajectory-scroll")
                            .relative()
                            .flex_1()
                            .h_full()
                            .min_h_0()
                            .min_w_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .child(list),
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

fn render_inspector(
    event: TrajectoryEvent,
    tab: InspectorTab,
    muted: gpui::Hsla,
    cx: &mut Context<TrajectoryWindow>,
) -> gpui::Div {
    let preview = tab == InspectorTab::Preview;
    let kind_zh = match event.lane {
        TrajectoryLane::ModelCall => "模型调用（provider payload）",
        TrajectoryLane::Request => "Pi 请求组装配置",
        TrajectoryLane::User => "消息",
        TrajectoryLane::Assistant => "模型消息",
        TrajectoryLane::Thinking => "模型思考",
        TrajectoryLane::Tool => "工具",
    };
    let subtitle = if event.turn == 0 {
        kind_zh.to_string()
    } else {
        format!("第 {} 轮 · {kind_zh}", event.turn)
    };
    let body = if preview {
        event.preview.clone()
    } else {
        event.source.clone()
    };
    v_flex()
        .w(px(380.))
        .h_full()
        .min_h_0()
        .flex_shrink_0()
        .overflow_hidden()
        .border_l_1()
        .border_color(ui_theme::card_stroke())
        .bg(gpui::rgb(ui_theme::bg_stage()))
        .child(
            h_flex()
                .flex_shrink_0()
                .px_3()
                .py_2()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(ui_theme::card_stroke())
                .child(
                    div()
                        .px_2()
                        .py(px(2.))
                        .rounded(px(4.))
                        .bg(ui_theme::tint(trajectory_lane_color(event.lane), 0x44))
                        .text_xs()
                        .font_semibold()
                        .text_color(gpui::rgb(ui_theme::text_bright()))
                        .child(trajectory_lane_label(event.lane)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(muted)
                        .child(subtitle),
                )
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
            h_flex()
                .flex_shrink_0()
                .px_3()
                .gap_4()
                .border_b_1()
                .border_color(ui_theme::card_stroke())
                .child(inspector_tab_label(
                    "预览",
                    preview,
                    InspectorTab::Preview,
                    cx,
                ))
                .child(inspector_tab_label(
                    "来源",
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
                .p_3()
                .overflow_y_scroll()
                .text_sm()
                .text_color(gpui::rgb(ui_theme::text_mid()))
                .child(body),
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
        .py_2()
        .cursor_pointer()
        .text_sm()
        .when(active, |tab_el| {
            tab_el
                .font_medium()
                .text_color(gpui::rgb(ui_theme::accent()))
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

fn render_sequence_overview(events: &[TrajectoryEvent]) -> gpui::AnyElement {
    // 请求配置和本地诊断目前都只是最新快照，没有可靠的 turn 关联；即使内容真实，
    // 也不能在时间概览里占一个“执行步骤”。
    let events = events
        .iter()
        .filter(|event| event.turn > 0)
        .cloned()
        .collect::<Vec<_>>();
    v_flex()
        .w_full()
        .gap_1()
        .child(overview_lane(
            "Input",
            ui_theme::text_muted(),
            &events,
            |lane| lane == TrajectoryLane::User,
        ))
        .child(overview_lane(
            "Model",
            ui_theme::purple(),
            &events,
            |lane| matches!(lane, TrajectoryLane::Assistant | TrajectoryLane::Thinking),
        ))
        .child(overview_lane("Tools", ui_theme::green(), &events, |lane| {
            lane == TrajectoryLane::Tool
        }))
        .into_any_element()
}

fn overview_lane(
    label: &'static str,
    color: u32,
    events: &[TrajectoryEvent],
    belongs: impl Fn(TrajectoryLane) -> bool,
) -> gpui::Div {
    let mut track = h_flex().flex_1().h(px(10.)).gap(px(1.)).items_center();
    if events.is_empty() {
        track = track.child(
            div()
                .flex_1()
                .h(px(8.))
                .rounded(px(2.))
                .bg(ui_theme::overlay(0x14)),
        );
    } else {
        for event in events {
            let cell = div().flex_1().h(px(8.)).min_w(px(4.)).rounded(px(2.));
            track = track.child(if belongs(event.lane) {
                cell.bg(gpui::rgb(trajectory_lane_color(event.lane)))
            } else {
                cell
            });
        }
    }
    h_flex()
        .w_full()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(44.))
                .flex_shrink_0()
                .text_xs()
                .text_color(gpui::rgb(color))
                .child(label),
        )
        .child(track)
}
