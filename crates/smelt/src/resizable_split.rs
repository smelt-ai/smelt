//! Smelt 的可拖拽分屏门面。
//!
//! `gpui-component` 的 resize handle 已经有 8px 命中区和拖拽状态，但当前 hover
//! refinement 仍使用普通色，鼠标移入时视觉不会变化。这里不复制拖拽逻辑，只在每个
//! 后续 panel 的边界上延迟绘制一根 hover 发丝；视觉层不注册 hitbox，底下的原生
//! handle 继续独占拖拽行为。

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Axis, Bounds, DispatchPhase, ElementId, Entity, IntoElement, MouseMoveEvent,
    ParentElement as _, Pixels, RenderOnce, Rgba, Styled as _, Window, canvas, deferred, div, fill,
    point, px, rgb, size,
};
use gpui_component::resizable::{ResizablePanel, ResizablePanelGroup, ResizableState};

use crate::ui_theme;

pub(crate) const RESIZE_HANDLE_HITBOX_SIZE: Pixels = px(8.);
const RESIZE_HANDLE_HALF_HITBOX: Pixels = px(4.);
const RESIZE_HANDLE_LINE_SIZE: Pixels = px(1.);

pub(crate) fn resize_handle_hover_color() -> Rgba {
    rgb(ui_theme::text_faint())
}

pub(crate) fn resize_handle_line_bounds(axis: Axis, hitbox: Bounds<Pixels>) -> Bounds<Pixels> {
    match axis {
        Axis::Horizontal => Bounds::new(
            point(hitbox.left() + RESIZE_HANDLE_HALF_HITBOX, hitbox.top()),
            size(RESIZE_HANDLE_LINE_SIZE, hitbox.size.height),
        ),
        Axis::Vertical => Bounds::new(
            point(hitbox.left(), hitbox.top() + RESIZE_HANDLE_HALF_HITBOX),
            size(hitbox.size.width, RESIZE_HANDLE_LINE_SIZE),
        ),
    }
}

/// 画在原生 handle 之上的纯视觉层。这里故意不注册 hitbox：GPUI 的重叠 sibling
/// 即使不 `occlude()`，也会把 drag 的事件路径带到最前面的 sibling，导致底下原生
/// handle 收不到拖拽。visual 只按 bounds + 鼠标坐标判断 hover，事件行为完全留给原生
/// handle。
pub(crate) fn resize_hover_handle(axis: Axis) -> AnyElement {
    let paint_axis = axis;
    deferred(
        div()
            .absolute()
            .when(axis == Axis::Horizontal, |handle| {
                handle
                    .top_0()
                    .left(-RESIZE_HANDLE_HALF_HITBOX)
                    .w(RESIZE_HANDLE_HITBOX_SIZE)
                    .h_full()
            })
            .when(axis == Axis::Vertical, |handle| {
                handle
                    .top(-RESIZE_HANDLE_HALF_HITBOX)
                    .left_0()
                    .w_full()
                    .h(RESIZE_HANDLE_HITBOX_SIZE)
            })
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let was_hovered = !window.last_input_was_keyboard()
                            && bounds.contains(&window.mouse_position());
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, _| {
                            if phase == DispatchPhase::Capture
                                && bounds.contains(&event.position) != was_hovered
                            {
                                window.refresh();
                            }
                        });

                        if was_hovered {
                            window.paint_quad(fill(
                                resize_handle_line_bounds(paint_axis, bounds),
                                resize_handle_hover_color(),
                            ));
                        }
                    },
                )
                .size_full(),
            ),
    )
    .into_any_element()
}

/// 只扩展分隔条视觉，panel 尺寸、约束、持久化与拖拽仍全部由组件库负责。
#[derive(IntoElement)]
pub(crate) struct ResizableSplit {
    group: ResizablePanelGroup,
    axis: Axis,
    panel_count: usize,
}

impl ResizableSplit {
    pub(crate) fn with_state(mut self, state: &Entity<ResizableState>) -> Self {
        self.group = self.group.with_state(state);
        self
    }

    pub(crate) fn child(mut self, panel: impl Into<ResizablePanel>) -> Self {
        let mut panel = panel.into();
        if self.panel_count > 0 {
            panel = panel.child(resize_hover_handle(self.axis));
        }
        self.group = self.group.child(panel);
        self.panel_count += 1;
        self
    }
}

impl RenderOnce for ResizableSplit {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.group
    }
}

pub(crate) fn h_resizable(id: impl Into<ElementId>) -> ResizableSplit {
    ResizableSplit {
        group: gpui_component::resizable::h_resizable(id),
        axis: Axis::Horizontal,
        panel_count: 0,
    }
}

pub(crate) fn v_resizable(id: impl Into<ElementId>) -> ResizableSplit {
    ResizableSplit {
        group: gpui_component::resizable::v_resizable(id),
        axis: Axis::Vertical,
        panel_count: 0,
    }
}
