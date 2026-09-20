//! 会话侧栏的拖拽排序：payload、浮层预览、drop hint 与命中层。

use super::*;
use gpui_component::scroll::AutoScroll;
use smelt_core::agent_kind::TerminalAgentKind;

/// 指针在侧栏视口顶/底边缘时的滚动量。正数往下，负数往上。
/// 横向上离开侧栏太远就停，避免拖到舞台区还在滚列表。
pub(super) fn sidebar_drag_scroll_delta(
    pointer: Point<Pixels>,
    viewport: Bounds<Pixels>,
) -> Option<Pixels> {
    const X_SLACK: f32 = 24.0;
    if pointer.x < viewport.left() - px(X_SLACK) || pointer.x > viewport.right() + px(X_SLACK) {
        return None;
    }
    AutoScroll::compute_delta(pointer.y, viewport)
}

/// `offset.y` 越往下越负。delta 正数把列表往下推。
pub(super) fn apply_sidebar_scroll_delta(offset_y: Pixels, delta: Pixels, max_y: Pixels) -> Pixels {
    (offset_y - delta).clamp(-max_y.max(px(0.)), px(0.))
}

/// 拖拽会话排序的 payload。身份用 `ui_id`；其余字段只为预览复原那一行。
#[derive(Clone)]
pub(super) struct SessionDrag {
    pub(super) id: u64,
    pub(super) title: SharedString,
    pub(super) acp: Option<ConversationAgentKind>,
    pub(super) terminal: Option<TerminalAgentKind>,
    pub(super) status: AgentStatus,
    pub(super) plugin_agent: Option<crate::plugin_ui::PluginAgentPresentation>,
    pub(super) subtitle: Option<SharedString>,
    pub(super) status_label: Option<SharedString>,
    pub(super) grab: Point<Pixels>,
}

impl Render for SessionDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        offset_drag_preview(self.grab, session_drag_preview(self))
    }
}

/// 拖拽项目分组的 payload。身份用 root；其余字段复原项目头那一行。
#[derive(Clone)]
pub(super) struct ProjectDrag {
    pub(super) root: SharedString,
    pub(super) label: SharedString,
    pub(super) collapsed: bool,
    pub(super) empty: bool,
    pub(super) branch: Option<SharedString>,
    pub(super) agg: AgentStatus,
    pub(super) plugin_agent: Option<crate::plugin_ui::PluginAgentPresentation>,
    pub(super) session_count: usize,
    pub(super) grab: Point<Pixels>,
}

impl Render for ProjectDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        offset_drag_preview(self.grab, project_drag_preview(self))
    }
}

fn lifted_preview() -> Div {
    div()
        .shadow_md()
        .opacity(0.88)
        .border_1()
        .border_color(ui_theme::card_stroke())
}

/// 浮层钉在指针右下方：热点和列表里的插入缝不被预览盖住，drop 命中也不被挡。
fn offset_drag_preview(grab: Point<Pixels>, preview: Div) -> Div {
    div().relative().size(px(1.)).child(
        preview
            .absolute()
            .left(grab.x + px(16.))
            .top(grab.y + px(12.)),
    )
}

fn session_drag_preview(drag: &SessionDrag) -> Div {
    lifted_preview()
        .w(px(232.))
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py(px(7.))
        .rounded(px(10.))
        .bg(rgb(ui_theme::bg_selected()))
        .child(
            div()
                .flex_shrink_0()
                .size(px(SESSION_AGENT_ICON_BOX_SIZE))
                .flex()
                .items_center()
                .justify_center()
                .text_color(ui_theme::session_dot_color(drag.status))
                .child(
                    drag.plugin_agent
                        .as_ref()
                        .map(plugin_agent_icon)
                        .unwrap_or_else(|| {
                            drag.acp
                                .map(acp_provider_icon)
                                .unwrap_or_else(|| provider_icon(drag.terminal))
                        })
                        .size(px(SESSION_AGENT_ICON_SIZE)),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(13.))
                .text_color(rgb(ui_theme::text_bright()))
                .truncate()
                .child(drag.title.clone()),
        )
        .children(drag.subtitle.clone().map(|subtitle| {
            div()
                .flex_shrink_0()
                .text_size(px(11.))
                .font_family(crate::terminal_view::font_family())
                .text_color(rgb(ui_theme::text_faint()))
                .child(subtitle)
        }))
        .children(drag.status_label.clone().map(|label| {
            div()
                .flex_shrink_0()
                .text_size(px(11.))
                .text_color(ui_theme::session_dot_color(drag.status))
                .child(label)
        }))
}

fn project_drag_preview(drag: &ProjectDrag) -> Div {
    lifted_preview()
        .w(px(248.))
        .rounded(ui_theme::card_radius())
        .bg(ui_theme::glass_card())
        .py_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .px_3()
                .py(px(4.))
                .child(
                    div()
                        .w(px(14.))
                        .flex_shrink_0()
                        .flex()
                        .justify_center()
                        .text_color(rgb(ui_theme::text_muted()))
                        .child(
                            Icon::new(project_disclosure_icon(drag.collapsed || drag.empty))
                                .size(px(PROJECT_DISCLOSURE_ICON_SIZE)),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .overflow_hidden()
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(12.))
                                .font_semibold()
                                .text_color(rgb(if drag.empty {
                                    ui_theme::text_faint()
                                } else {
                                    ui_theme::text_bright()
                                }))
                                .child(drag.label.clone()),
                        )
                        .when(drag.agg != AgentStatus::Idle, |row| {
                            row.child(
                                div()
                                    .flex_shrink_0()
                                    .size(px(6.))
                                    .rounded_full()
                                    .bg(ui_theme::session_dot_color(drag.agg)),
                            )
                        })
                        .children(drag.plugin_agent.as_ref().map(|agent| {
                            div()
                                .flex_shrink_0()
                                .size(px(15.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(rgb(ui_theme::text_muted()))
                                .child(plugin_agent_icon(agent).size(px(14.)))
                        }))
                        .children(drag.branch.clone().map(|branch| {
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(11.))
                                .font_family(crate::terminal_view::font_family())
                                .text_color(rgb(ui_theme::text_faint()))
                                .child(branch)
                        })),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(11.))
                        .text_color(rgb(ui_theme::text_faint()))
                        .child(drag.session_count.to_string()),
                ),
        )
}

/// 叠在行缝上的插入记号：绝对定位，不撑开布局，下面的内容才不会跟着跳。
fn sidebar_drop_indicator(at_top: bool) -> Div {
    let color = rgb(ui_theme::accent());
    div()
        .absolute()
        .left(px(8.))
        .right(px(8.))
        .h(px(8.))
        .flex()
        .items_center()
        .gap_1()
        .map(|d| {
            if at_top {
                d.top(px(-4.))
            } else {
                d.bottom(px(-4.))
            }
        })
        .child(div().flex_shrink_0().size(px(8.)).rounded_full().bg(color))
        .child(div().flex_1().h(px(2.)).rounded_full().bg(color))
}

fn update_sess_drop_hint(
    workspace: Entity<Workspace>,
    ui_id: u64,
    before: bool,
) -> impl Fn(&DragMoveEvent<SessionDrag>, &mut Window, &mut App) + 'static {
    move |ev, _window, cx| {
        let inside = ev.bounds.contains(&ev.event.position);
        workspace.update(cx, |ws, cx| {
            let hint = Some((ui_id, before));
            if inside && ws.sess_drop_hint != hint {
                ws.sess_drop_hint = hint;
                cx.notify();
            } else if !inside && ws.sess_drop_hint == hint {
                ws.sess_drop_hint = None;
                cx.notify();
            }
        });
    }
}

fn update_proj_drop_hint(
    workspace: Entity<Workspace>,
    root: SharedString,
    before: bool,
) -> impl Fn(&DragMoveEvent<ProjectDrag>, &mut Window, &mut App) + 'static {
    move |ev, _window, cx| {
        let inside = ev.bounds.contains(&ev.event.position);
        workspace.update(cx, |ws, cx| {
            let hint = Some((root.to_string(), before));
            if inside && ws.proj_drop_hint != hint {
                ws.proj_drop_hint = hint;
                cx.notify();
            } else if !inside && ws.proj_drop_hint == hint {
                ws.proj_drop_hint = None;
                cx.notify();
            }
        });
    }
}

pub(super) fn with_session_drag(
    row: Stateful<Div>,
    payload: SessionDrag,
    workspace: Entity<Workspace>,
) -> Stateful<Div> {
    let ui_id = payload.id;
    row.on_drag(payload, move |drag, grab, _, cx| {
        workspace.update(cx, |ws, _| {
            ws.sess_drop_hint = None;
            ws.proj_drop_hint = None;
            ws.sidebar_drag = Some(crate::SidebarDrag::Session(ui_id));
        });
        let mut drag = drag.clone();
        drag.grab = grab;
        cx.new(|_| drag)
    })
}

pub(super) fn with_project_drag(
    row: Stateful<Div>,
    payload: ProjectDrag,
    workspace: Entity<Workspace>,
) -> Stateful<Div> {
    row.on_drag(payload, move |drag, grab, _, cx| {
        let dragged_root = drag.root.to_string();
        workspace.update(cx, |ws, _| {
            ws.sess_drop_hint = None;
            ws.proj_drop_hint = None;
            ws.sidebar_drag = Some(crate::SidebarDrag::Project(dragged_root));
        });
        let mut drag = drag.clone();
        drag.grab = grab;
        cx.new(|_| drag)
    })
}

pub(super) fn attach_session_drop_layers<E: ParentElement + FluentBuilder>(
    row: E,
    key: usize,
    ui_id: u64,
    hint_before: bool,
    hint_after: bool,
    workspace: Entity<Workspace>,
) -> E {
    let e_before = workspace.clone();
    let e_after = workspace.clone();
    row.child(
        div()
            .absolute()
            .inset_0()
            .child(
                div()
                    .id(("sess-drop-before", key))
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h_1_2()
                    .on_drag_move(update_sess_drop_hint(workspace.clone(), ui_id, true))
                    .on_drop(move |drag: &SessionDrag, _window, cx| {
                        let dragged = drag.id;
                        e_before.update(cx, |ws, cx| {
                            ws.move_session_near(dragged, ui_id, true, cx);
                        });
                    }),
            )
            .child(
                div()
                    .id(("sess-drop-after", key))
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .h_1_2()
                    .on_drag_move(update_sess_drop_hint(workspace, ui_id, false))
                    .on_drop(move |drag: &SessionDrag, _window, cx| {
                        let dragged = drag.id;
                        e_after.update(cx, |ws, cx| {
                            ws.move_session_near(dragged, ui_id, false, cx);
                        });
                    }),
            ),
    )
    .when(hint_before, |row| row.child(sidebar_drop_indicator(true)))
    .when(hint_after, |row| row.child(sidebar_drop_indicator(false)))
}

pub(super) fn attach_project_drop_layers<E: ParentElement + FluentBuilder>(
    row: E,
    pix: usize,
    root: SharedString,
    hint_before: bool,
    hint_after: bool,
    workspace: Entity<Workspace>,
) -> E {
    let e_before = workspace.clone();
    let e_after = workspace.clone();
    let before_root = root.clone();
    let after_root = root.clone();
    row.child(
        div()
            .absolute()
            .inset_0()
            .child(
                div()
                    .id(("proj-drop-before", pix))
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h_1_2()
                    .on_drag_move(update_proj_drop_hint(workspace.clone(), root.clone(), true))
                    .on_drop(move |drag: &ProjectDrag, _window, cx| {
                        let from = drag.root.to_string();
                        let to = before_root.to_string();
                        e_before.update(cx, |ws, cx| {
                            ws.move_project_near(&from, &to, true, cx);
                        });
                    }),
            )
            .child(
                div()
                    .id(("proj-drop-after", pix))
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .h_1_2()
                    .on_drag_move(update_proj_drop_hint(workspace, root, false))
                    .on_drop(move |drag: &ProjectDrag, _window, cx| {
                        let from = drag.root.to_string();
                        let to = after_root.to_string();
                        e_after.update(cx, |ws, cx| {
                            ws.move_project_near(&from, &to, false, cx);
                        });
                    }),
            ),
    )
    .when(hint_before, |row| row.child(sidebar_drop_indicator(true)))
    .when(hint_after, |row| row.child(sidebar_drop_indicator(false)))
}

impl Workspace {
    /// 拖着会话/项目在侧栏顶底边缘时，按指针位置自动滚列表。
    pub(crate) fn update_sidebar_drag_scroll(
        &mut self,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_drag.is_none() {
            self.sidebar_auto_scroll.stop();
            return;
        }
        let viewport = self.sidebar_scroll.bounds();
        if viewport.size.height <= px(0.) {
            self.sidebar_auto_scroll.stop();
            return;
        }
        let delta = sidebar_drag_scroll_delta(position, viewport);
        self.sidebar_auto_scroll.set(delta, cx, |delta, ws, cx| {
            let offset = ws.sidebar_scroll.offset();
            let next_y =
                apply_sidebar_scroll_delta(offset.y, delta, ws.sidebar_scroll.max_offset().y);
            if next_y != offset.y {
                ws.sidebar_scroll.set_offset(point(offset.x, next_y));
                cx.notify();
            }
        });
    }

    pub(crate) fn stop_sidebar_drag_scroll(&mut self) {
        self.sidebar_auto_scroll.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::{apply_sidebar_scroll_delta, sidebar_drag_scroll_delta};
    use gpui::{Bounds, point, px, size};

    #[test]
    fn drag_scroll_triggers_at_the_top_edge_and_stops_in_the_middle() {
        let viewport = Bounds::new(point(px(0.), px(0.)), size(px(280.), px(400.)));
        let top = sidebar_drag_scroll_delta(point(px(140.), px(4.)), viewport).unwrap();
        let bottom = sidebar_drag_scroll_delta(point(px(140.), px(396.)), viewport).unwrap();
        assert!(top < px(0.));
        assert!(bottom > px(0.));
        assert_eq!(
            sidebar_drag_scroll_delta(point(px(140.), px(200.)), viewport),
            None
        );
    }

    #[test]
    fn drag_scroll_ignores_pointer_far_from_the_sidebar() {
        let viewport = Bounds::new(point(px(0.), px(0.)), size(px(280.), px(400.)));
        assert_eq!(
            sidebar_drag_scroll_delta(point(px(400.), px(4.)), viewport),
            None
        );
    }

    #[test]
    fn scroll_delta_clamps_to_the_content_range() {
        assert_eq!(
            apply_sidebar_scroll_delta(px(0.), px(-12.), px(200.)),
            px(0.)
        );
        assert_eq!(
            apply_sidebar_scroll_delta(px(0.), px(12.), px(200.)),
            px(-12.)
        );
        assert_eq!(
            apply_sidebar_scroll_delta(px(-200.), px(12.), px(200.)),
            px(-200.)
        );
    }
}
