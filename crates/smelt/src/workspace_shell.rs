//! 工作台外壳分栏与伸缩动画。
//!
//! 左栏（会话列表与机器管理）、中间舞台与右栏（工具面板）采用直接分栏排布：
//! - 侧栏与工具栏的开合由平滑动画驱动，在 0px 与目标宽度之间丝滑过渡，没有底层硬截断；
//! - 展开状态下在栏间提供拖拽分隔条调节宽度，松手时落盘；拖过收起线时平滑闭合；
//! - 终端分屏是舞台里的专用 Dock（`term_dock`），不与顶层外壳耦合。

use std::time::Duration;
use gpui_base::{Easing, Transition};
use gpui::{
    AnyElement, Context, ExternalPaths, Hsla, MouseButton, MouseDownEvent, Pixels, Window,
    div, prelude::*, px, rgb,
};
use gpui_component::{ActiveTheme, Icon};
use gpui_component::empty::{
    Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyTitle,
};
pub(crate) use smelt_ui::Button;

use crate::settings::Appearance;
use crate::{
    AgentStatus, MIN_SIDEBAR_WIDTH, MIN_TOOL_PANEL_WIDTH, MIN_WORKSPACE_CONTENT_WIDTH, SessionKind,
    Workspace, WorkspaceRoute, terminal, ui_theme, workspace_frame,
};

/// 允许拖到的侧栏上限。
pub(crate) fn sidebar_cap(viewport: Pixels) -> f32 {
    let shell = f32::from(ui_theme::shell_padding()) * 2.0;
    (f32::from(viewport) - shell - MIN_WORKSPACE_CONTENT_WIDTH).max(MIN_SIDEBAR_WIDTH)
}

/// 允许拖到的工具栏上限。
pub(crate) fn tool_cap(viewport: Pixels, sidebar_on_screen: f32) -> f32 {
    let shell = f32::from(ui_theme::shell_padding()) * 2.0;
    (f32::from(viewport) - shell - sidebar_on_screen - MIN_WORKSPACE_CONTENT_WIDTH)
        .max(MIN_TOOL_PANEL_WIDTH)
}

pub(crate) fn tool_display(preference: f32, viewport: Pixels, sidebar_on_screen: f32) -> f32 {
    preference.clamp(MIN_TOOL_PANEL_WIDTH, tool_cap(viewport, sidebar_on_screen))
}

/// 松手时窄于收起线，就用按下前的宽度，并把它抬到这一栏的最小宽度以上。
/// 否则这次松手不收起。
pub(crate) fn width_after_collapse(gesture_width: f32, current_width: f32, min_width: f32) -> Option<f32> {
    if current_width < crate::DOCK_COLLAPSE_WIDTH {
        Some(gesture_width.max(min_width))
    } else {
        None
    }
}

pub(crate) fn sidebar_width_after_collapse(gesture_width: f32, current_width: f32) -> Option<f32> {
    width_after_collapse(gesture_width, current_width, crate::MIN_SIDEBAR_WIDTH)
}

pub(crate) fn tool_panel_width_after_collapse(gesture_width: f32, current_width: f32) -> Option<f32> {
    width_after_collapse(gesture_width, current_width, crate::MIN_TOOL_PANEL_WIDTH)
}

/// 分隔条拖动和窗口变宽时，像素与存档宽度怎么对齐。保留纯计算逻辑供单元测试。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ShellSizeStep {
    pub(crate) preference: f32,
    pub(crate) applied: f32,
    pub(crate) dock: f32,
    pub(crate) display: f32,
    pub(crate) min: f32,
    pub(crate) max: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ShellSizeAction {
    Keep,
    Adopt {
        preference: f32,
        push: Option<f32>,
    },
    Push(f32),
}

pub(crate) fn shell_size_action(step: ShellSizeStep) -> ShellSizeAction {
    let cap = step.max.max(step.min);
    if (step.dock - step.applied).abs() > 0.5 {
        let preference = step.dock.clamp(step.min, cap);
        let push = ((preference - step.dock).abs() > 0.5).then_some(preference);
        ShellSizeAction::Adopt { preference, push }
    } else if (step.dock - step.display).abs() > 0.5 {
        ShellSizeAction::Push(step.display.clamp(step.min, cap))
    } else {
        ShellSizeAction::Keep
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ShellSizeDecision {
    pub(crate) preference: f32,
    pub(crate) applied: f32,
    pub(crate) push: Option<f32>,
    pub(crate) dragged: bool,
}

pub(crate) fn decide_shell_size(step: ShellSizeStep, pointer_down: bool) -> ShellSizeDecision {
    match shell_size_action(step) {
        ShellSizeAction::Keep => ShellSizeDecision {
            preference: step.preference,
            applied: step.applied,
            push: None,
            dragged: false,
        },
        ShellSizeAction::Adopt { preference, push } => ShellSizeDecision {
            preference,
            applied: if pointer_down {
                step.dock
            } else {
                push.unwrap_or(preference)
            },
            push: if pointer_down { None } else { push },
            dragged: true,
        },
        ShellSizeAction::Push(next) => {
            if pointer_down {
                ShellSizeDecision {
                    preference: step.preference,
                    applied: step.applied,
                    push: None,
                    dragged: false,
                }
            } else {
                ShellSizeDecision {
                    preference: step.preference,
                    applied: next,
                    push: Some(next),
                    dragged: false,
                }
            }
        }
    }
}

impl Workspace {
    /// 计算侧栏与工具栏在当前动画帧中的像素跨度与动画进度。
    pub(crate) fn shell_animated_spans(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (f32, f32, f32, f32) {
        let viewport = window.viewport_size().width;
        let sidebar_open = self.sidebar_open;
        let tool_open = self.tool_panel_open && !self.tool_panel_promoted();

        let sidebar_display = crate::sidebar_width_for_viewport(self.sidebar_w, viewport);
        let sidebar_policy = Transition::new(Duration::from_millis(200)).easing(Easing::EaseOut);
        let sidebar_progress: f32 = gpui_base::transition(
            gpui::ElementId::from("workspace-shell-sidebar-collapse"),
            if sidebar_open { 1.0f32 } else { 0.0f32 },
            sidebar_policy,
            window,
            cx,
        );

        let tool_policy = Transition::new(Duration::from_millis(200)).easing(Easing::EaseOut);
        let tool_progress: f32 = gpui_base::transition(
            gpui::ElementId::from("workspace-shell-tool-collapse"),
            if tool_open { 1.0f32 } else { 0.0f32 },
            tool_policy,
            window,
            cx,
        );

        let animated_sidebar_w = (sidebar_display * sidebar_progress).max(0.0);
        let tool_shown = tool_display(self.tool_panel_w, viewport, animated_sidebar_w);
        let animated_tool_w = (tool_shown * tool_progress).max(0.0);

        (
            animated_sidebar_w,
            animated_tool_w,
            sidebar_progress,
            tool_progress,
        )
    }

    /// 渲染外壳主体三栏及栏间分隔条。
    pub(crate) fn render_shell_columns(
        &mut self,
        animated_sidebar_w: f32,
        animated_tool_w: f32,
        sidebar_progress: f32,
        tool_progress: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sidebar_open = self.sidebar_open;
        let tool_open = self.tool_panel_open && !self.tool_panel_promoted();

        div()
            .size_full()
            .flex()
            .flex_row()
            .when(animated_sidebar_w > 0.001, |row| {
                row.child(
                    div()
                        .w(px(animated_sidebar_w))
                        .h_full()
                        .overflow_hidden()
                        .flex_none()
                        .relative()
                        .child(self.render_sidebar_dock(window, cx))
                        .when(sidebar_open && sidebar_progress >= 0.999, |col| {
                            col.child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .right_0()
                                    .w(ui_theme::chrome_gap())
                                    .child(self.render_sidebar_resize_handle(cx)),
                            )
                        }),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_stage_dock(window, cx)),
            )
            .when(animated_tool_w > 0.001, |row| {
                row.child(
                    div()
                        .w(px(animated_tool_w))
                        .h_full()
                        .overflow_hidden()
                        .flex_none()
                        .relative()
                        .child(self.render_tool_dock(window, cx))
                        .when(tool_open && tool_progress >= 0.999, |col| {
                            col.child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left_0()
                                    .w(ui_theme::chrome_gap())
                                    .child(self.render_tool_resize_handle(cx)),
                            )
                        }),
                )
            })
            .into_any_element()
    }

    /// 侧栏与舞台之间的拖拽调节发丝。
    pub(crate) fn render_sidebar_resize_handle(&self, cx: &mut Context<Self>) -> AnyElement {
        let is_dragging = self.shell_drag_sidebar_start.is_some();
        div()
            .id("sidebar-shell-resize-handle")
            .size_full()
            .cursor_col_resize()
            .group("sidebar-handle")
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(2.))
                    .w(px(1.))
                    .bg(rgb(if is_dragging {
                        ui_theme::text_faint()
                    } else {
                        ui_theme::border_dim()
                    }))
                    .group_hover("sidebar-handle", |line| line.bg(rgb(ui_theme::text_faint()))),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    this.shell_drag_sidebar_start = Some((
                        f32::from(event.position.x),
                        this.sidebar_w,
                    ));
                    this.shell_gesture_sidebar_w = Some(this.sidebar_w);
                    this.shell_pointer_down = true;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    /// 舞台与工具栏之间的拖拽调节发丝。
    pub(crate) fn render_tool_resize_handle(&self, cx: &mut Context<Self>) -> AnyElement {
        let is_dragging = self.shell_drag_tool_start.is_some();
        div()
            .id("tool-shell-resize-handle")
            .size_full()
            .cursor_col_resize()
            .group("tool-handle")
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(3.))
                    .w(px(1.))
                    .bg(rgb(if is_dragging {
                        ui_theme::text_faint()
                    } else {
                        ui_theme::border_dim()
                    }))
                    .group_hover("tool-handle", |line| line.bg(rgb(ui_theme::text_faint()))),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    this.shell_drag_tool_start = Some((
                        f32::from(event.position.x),
                        this.tool_panel_w,
                    ));
                    this.shell_gesture_tool_w = Some(this.tool_panel_w);
                    this.shell_pointer_down = true;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    /// 松开鼠标指针时提交拖拽结果并判定是否收起。
    pub(crate) fn commit_shell_pointer(
        &mut self,
        window: &mut Window,
        _viewport: Pixels,
        cx: &mut Context<Self>,
    ) {
        self.shell_pointer_down = false;
        let mut changed = false;
        if let Some((_, _)) = self.shell_drag_sidebar_start.take() {
            let gesture_w = self
                .shell_gesture_sidebar_w
                .take()
                .unwrap_or(self.sidebar_w);
            if self.sidebar_w < crate::DOCK_COLLAPSE_WIDTH {
                self.sidebar_w = gesture_w.max(crate::MIN_SIDEBAR_WIDTH);
                self.sidebar_open = false;
                self.focus_active_stage(window, cx);
            } else {
                self.sidebar_w = self.sidebar_w.max(crate::MIN_SIDEBAR_WIDTH);
            }
            changed = true;
        }
        if let Some((_, _)) = self.shell_drag_tool_start.take() {
            let gesture_w = self
                .shell_gesture_tool_w
                .take()
                .unwrap_or(self.tool_panel_w);
            if self.tool_panel_w < crate::DOCK_COLLAPSE_WIDTH {
                self.tool_panel_w = gesture_w.max(crate::MIN_TOOL_PANEL_WIDTH);
                self.set_tool_panel_open(false, cx);
                self.focus_active_stage(window, cx);
            } else {
                self.tool_panel_w = self.tool_panel_w.max(crate::MIN_TOOL_PANEL_WIDTH);
            }
            changed = true;
        }
        if changed || self.shell_needs_save {
            self.shell_needs_save = false;
            self.save_state(cx);
            cx.notify();
        }
    }

    pub(crate) fn collapse_sidebar(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_w = width.max(crate::MIN_SIDEBAR_WIDTH);
        self.sidebar_open = false;
        self.focus_active_stage(window, cx);
        self.save_state(cx);
        cx.notify();
    }

    pub(crate) fn collapse_tool_panel(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.tool_panel_w = width.max(crate::MIN_TOOL_PANEL_WIDTH);
        self.set_tool_panel_open(false, cx);
        self.focus_active_stage(window, cx);
        self.save_state(cx);
        cx.notify();
    }

    pub(crate) fn render_sidebar_dock(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let surface: Hsla = rgb(ui_theme::bg_column()).into();
        let animate = self.workspace_window_active && self.sidebar_open;
        let statuses: Vec<AgentStatus> = self
            .sessions
            .iter()
            .map(|session| session.status(cx))
            .collect();
        let focused_key = self.sidebar_focused_key(window);
        let list = self.render_session_list(cx.entity(), window, cx, &statuses, animate, focused_key);
        let viewport = window.viewport_size().width;
        let sidebar_display = crate::sidebar_width_for_viewport(self.sidebar_w, viewport);
        div()
            .size_full()
            .pr(ui_theme::chrome_gap())
            .child(
                workspace_frame::card(surface)
                    .pt(workspace_frame::TOP_BAR_HEIGHT)
                    // 从访达拖文件夹进侧栏 = 打开项目。只认侧栏，不挂整个窗口：对话输入框要自己收文件。
                    .drag_over::<ExternalPaths>(|style, _paths, _window, cx| {
                        style.bg(cx.theme().drop_target)
                    })
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                        this.open_paths(paths.paths(), cx);
                    }))
                    .child(
                        div()
                            .w(px(sidebar_display))
                            .h_full()
                            .flex_none()
                            .child(list),
                    )
                    .child(workspace_frame::with_window_drag(
                        div()
                            .absolute()
                            .top_0()
                            .left(px(80.))
                            .right_0()
                            .h(workspace_frame::TOP_BAR_HEIGHT),
                    )),
            )
            .into_any_element()
    }

    pub(crate) fn render_tool_dock(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let surface: Hsla = rgb(ui_theme::bg_column()).into();
        let tool_w = self.tool_panel_w;
        div()
            .size_full()
            .pl(ui_theme::chrome_gap())
            .child(
                workspace_frame::card(surface)
                    .justify_end()
                    .child(
                        div()
                            .w(px(tool_w))
                            .h_full()
                            .flex_none()
                            .child(self.render_tool_panel(window, cx)),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn render_stage_dock(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stage_surface: Hsla = rgb(ui_theme::bg_stage()).into();
        let left_guard = self.chrome_left_guard(window);
        let stage_content: AnyElement = match self.active_tab() {
            WorkspaceRoute::Agents => self.render_agents_page(window, cx),
            WorkspaceRoute::Automations => self.render_automations_page(window, cx),
            WorkspaceRoute::Page(id) => self.render_page(id, cx),
            WorkspaceRoute::Desktop => self.render_desktop_page(window, cx),
            WorkspaceRoute::Session => {
                if self.tool_panel_promoted() {
                    self.render_tool_panel_stage(left_guard, window, cx)
                } else {
                    let background_image = cx.global::<Appearance>().bg_image.clone();
                    let content = if self
                        .sessions
                        .get(self.active_session)
                        .is_some_and(|session| !session.is_product_conversation(cx))
                    {
                        div().flex_1().min_w_0().min_h_0().flex().flex_col().child(
                            div().flex_1().min_w_0().min_h_0().flex().child(
                                match &self.sessions[self.active_session].kind {
                                    SessionKind::Term { dock, .. } => div()
                                        .flex_1()
                                        .min_w_0()
                                        .min_h_0()
                                        .flex()
                                        .bg(rgb(terminal::default_bg()))
                                        .pt(px(ui_theme::CHROME_ISLAND_BAND_PX))
                                        .child(dock.area().clone())
                                        .into_any_element(),
                                    SessionKind::Conversation(view) => {
                                        let bg_image_opacity =
                                            cx.global::<Appearance>().bg_image_opacity;
                                        div()
                                            .relative()
                                            .flex_1()
                                            .min_w_0()
                                            .min_h_0()
                                            .flex()
                                            .bg(rgb(ui_theme::bg_stage()))
                                            .children(background_image.as_deref().map(|path| {
                                                workspace_frame::background_image_layer(
                                                    path,
                                                    bg_image_opacity,
                                                    cx,
                                                )
                                            }))
                                            .child(view.clone().cached(
                                                gpui::StyleRefinement::default().size_full(),
                                            ))
                                            .into_any_element()
                                    }
                                },
                            ),
                        )
                    } else if self.spaces.is_empty() {
                        stage_empty_state(
                            gpui_kit_assets::IconName::Server,
                            "添加远程机器",
                            "配对一台 Linux 或 macOS 机器后，就可以在这里打开项目、对话和终端。",
                            Some(
                                Button::new("empty-add-runtime")
                                    .commit()
                                    .label("添加远程机器")
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.open_settings_section(
                                            crate::settings::SettingsSection::Remote,
                                            cx,
                                        );
                                    })),
                            ),
                        )
                    } else {
                        stage_empty_state(
                            gpui_kit_assets::IconName::Coffee,
                            "休息一下",
                            "没有打开的会话",
                            None,
                        )
                    };
                    content.into_any_element()
                }
            }
        };
        let stage = div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(stage_content);
        div()
            .size_full()
            .child(workspace_frame::card(stage_surface).child(stage))
            .into_any_element()
    }
}

/// 舞台空态。
fn stage_empty_state(
    icon: impl Into<Icon>,
    title: &'static str,
    description: &'static str,
    action: Option<Button>,
) -> gpui::Div {
    let empty = Empty::new()
        .header(
            EmptyHeader::new()
                .media(
                    EmptyMedia::new()
                        .with_variant(EmptyMediaVariant::Icon)
                        .child(Icon::new(icon)),
                )
                .title(EmptyTitle::new().child(title))
                .description(EmptyDescription::new().max_w(px(420.)).child(description)),
        )
        .when_some(action, |empty, action| {
            empty.content(EmptyContent::new().child(action))
        });
    div().flex_1().flex().child(empty)
}

#[cfg(test)]
mod tests {
    use super::{
        ShellSizeAction, ShellSizeDecision, ShellSizeStep, decide_shell_size, shell_size_action,
        sidebar_width_after_collapse, tool_panel_width_after_collapse,
    };

    fn step(preference: f32, applied: f32, dock: f32, display: f32) -> ShellSizeStep {
        ShellSizeStep {
            preference,
            applied,
            dock,
            display,
            min: 240.,
            max: 10_000.,
        }
    }

    fn capped(dock: f32, max: f32) -> ShellSizeStep {
        ShellSizeStep {
            preference: 320.,
            applied: 320.,
            dock,
            display: 320.,
            min: 240.,
            max,
        }
    }

    #[test]
    fn drag_adopts_the_dock_width() {
        assert_eq!(
            shell_size_action(step(240., 240., 320., 240.)),
            ShellSizeAction::Adopt {
                preference: 320.,
                push: None,
            }
        );
    }

    #[test]
    fn drag_below_the_minimum_is_pushed_back() {
        assert_eq!(
            shell_size_action(step(240., 240., 100., 240.)),
            ShellSizeAction::Adopt {
                preference: 240.,
                push: Some(240.),
            }
        );
    }

    #[test]
    fn a_narrow_window_pushes_the_display_and_keeps_the_preference() {
        assert_eq!(
            shell_size_action(step(360., 360., 360., 280.)),
            ShellSizeAction::Push(280.)
        );
    }

    #[test]
    fn growing_the_window_restores_the_preference() {
        assert_eq!(
            shell_size_action(step(360., 280., 280., 360.)),
            ShellSizeAction::Push(360.)
        );
    }

    #[test]
    fn an_aligned_dock_is_left_alone() {
        assert_eq!(
            shell_size_action(step(320., 320., 320., 320.)),
            ShellSizeAction::Keep
        );
    }

    #[test]
    fn a_drag_past_the_cap_is_clamped() {
        assert_eq!(
            shell_size_action(capped(700., 536.)),
            ShellSizeAction::Adopt {
                preference: 536.,
                push: Some(536.),
            }
        );
    }

    #[test]
    fn a_held_pointer_follows_the_dock_and_does_not_push() {
        assert_eq!(
            decide_shell_size(capped(700., 536.), true),
            ShellSizeDecision {
                preference: 536.,
                applied: 700.,
                push: None,
                dragged: true,
            }
        );
    }

    #[test]
    fn releasing_the_pointer_pushes_a_capped_drag_back() {
        assert_eq!(
            decide_shell_size(capped(700., 536.), false),
            ShellSizeDecision {
                preference: 536.,
                applied: 536.,
                push: Some(536.),
                dragged: true,
            }
        );
    }

    #[test]
    fn releasing_below_the_collapse_line_keeps_the_width_from_before_the_drag() {
        assert_eq!(sidebar_width_after_collapse(360., 100.), Some(360.));
        assert_eq!(sidebar_width_after_collapse(360., 159.9), Some(360.));
    }

    #[test]
    fn releasing_on_the_collapse_line_stays_open() {
        assert_eq!(
            sidebar_width_after_collapse(360., crate::DOCK_COLLAPSE_WIDTH),
            None
        );
        assert_eq!(sidebar_width_after_collapse(360., 240.), None);
    }

    #[test]
    fn a_collapse_repairs_a_gesture_width_below_the_minimum() {
        assert_eq!(sidebar_width_after_collapse(100., 100.), Some(240.));
    }

    #[test]
    fn releasing_a_narrow_tool_panel_keeps_the_width_from_before_the_drag() {
        assert_eq!(tool_panel_width_after_collapse(480., 100.), Some(480.));
        assert_eq!(tool_panel_width_after_collapse(480., 159.9), Some(480.));
        assert_eq!(
            tool_panel_width_after_collapse(480., crate::DOCK_COLLAPSE_WIDTH),
            None
        );
        assert_eq!(tool_panel_width_after_collapse(200., 100.), Some(320.));
    }

    #[test]
    fn a_held_pointer_does_not_push_a_narrow_window() {
        assert_eq!(
            decide_shell_size(step(360., 360., 360., 280.), true),
            ShellSizeDecision {
                preference: 360.,
                applied: 360.,
                push: None,
                dragged: false,
            }
        );
    }
}
