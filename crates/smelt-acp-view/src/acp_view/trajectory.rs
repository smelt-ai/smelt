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
                    title: Some("Trajectory".into()),
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

impl Render for TrajectoryWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let context_note = (session.agent == ConversationAgentKind::Pi).then(|| {
            let skills = smelt_core::pi_plugin_catalog::loaded_skills_for_launch(
                &session.launch,
                session.cwd.as_deref().map(std::path::Path::new),
            );
            if skills.is_empty() {
                "Available skills: none".to_string()
            } else {
                format!(
                    "Available skills: {}",
                    skills
                        .iter()
                        .map(|skill| skill.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        });
        let events = session_trajectory_events(&session.entries, context_note);
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
            for event in visible.iter() {
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
        TrajectoryLane::Context => "上下文",
        TrajectoryLane::User => "消息",
        TrajectoryLane::Assistant => "回复",
        TrajectoryLane::Tool => "工具",
    };
    let subtitle = if event.turn == 0 {
        kind_zh.to_string()
    } else {
        format!("第 {} 轮 · {kind_zh}", event.turn)
    };
    let source = format!(
        "kind: {}\nturn: {}\ntext: {}",
        trajectory_lane_label(event.lane),
        event.turn,
        event.text
    );
    let body = if preview { event.text.clone() } else { source };
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
    v_flex()
        .w_full()
        .gap_1()
        .child(overview_lane(
            "Input",
            ui_theme::text_muted(),
            events,
            |lane| matches!(lane, TrajectoryLane::User | TrajectoryLane::Context),
        ))
        .child(overview_lane("Model", ui_theme::purple(), events, |lane| {
            lane == TrajectoryLane::Assistant
        }))
        .child(overview_lane("Tools", ui_theme::green(), events, |lane| {
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
