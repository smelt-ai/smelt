//! 可拖拽分栏。命中带、光标、拖拽和尺寸状态都是 gpui-base 的，这里只画把手。
//!
//! 把手平时是一根 1px `hair`，和窗口分栏、卡片外沿同一根线；指针压上去、按下、拖拽时
//! 整根亮到 `faint`。不画中间一小段药丸：拖一两像素指针就离开命中带，按 GPUI 的悬停
//! 判断会熄灭，颜色跟 [`ResizeHandleState`] 走，拖拽全程保持亮着。

use std::rc::Rc;

use gpui::{AnyElement, Axis, ElementId, IntoElement, Styled as _, div, px, rgb};

pub use gpui_base::{ResizablePanel, ResizablePanelGroup, ResizableState, resizable_panel};
use gpui_base::{ResizeHandleRenderer, ResizeHandleState};

use crate::ui_theme;

/// 左右排的分栏组，把手是发丝线。
pub fn h_resizable(id: impl Into<ElementId>) -> ResizablePanelGroup {
    gpui_base::h_resizable(id).with_handle_appearance(handle_appearance())
}

/// 上下排的分栏组，把手是发丝线。
pub fn v_resizable(id: impl Into<ElementId>) -> ResizablePanelGroup {
    gpui_base::v_resizable(id).with_handle_appearance(handle_appearance())
}

/// 把手的样子，给不经分栏组、自己拿 `gpui_base::resize_handle` 做的边缘把手用。
pub fn handle_appearance() -> ResizeHandleRenderer {
    Rc::new(|handle, _, _| Some(hairline(handle.axis(), handle.state())))
}

/// `axis` 是拖动的方向：左右拖的把手是一根竖线。
pub(crate) fn hairline(axis: Axis, state: ResizeHandleState) -> AnyElement {
    let line = div().flex_none().bg(if state == ResizeHandleState::Idle {
        ui_theme::hair()
    } else {
        rgb(ui_theme::faint())
    });
    match axis {
        Axis::Horizontal => line.w(px(1.)).h_full(),
        Axis::Vertical => line.h(px(1.)).w_full(),
    }
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui::{
        AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, Modifiers,
        MouseButton, ParentElement as _, Render, TestAppContext, Window, div, point, px,
    };

    use super::*;

    struct Split {
        state: Entity<ResizableState>,
    }

    impl Render for Split {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let panel = |width: f32, selector: &'static str| {
                resizable_panel()
                    .size(px(width))
                    .child(div().size_full().debug_selector(move || selector.into()))
            };
            div().w(px(400.)).h(px(100.)).child(
                h_resizable("split")
                    .with_state(&self.state)
                    .child(panel(150., "first"))
                    .child(panel(250., "second")),
            )
        }
    }

    /// 自己画把手以后，命中和拖拽仍归 gpui-base：拖分界线照样改两栏宽度。
    #[gpui::test]
    fn dragging_the_hairline_resizes_the_panels(cx: &mut TestAppContext) {
        let state = cx.update(|cx| cx.new(|_| ResizableState::default()));
        let (_, cx) = cx.add_window_view({
            let state = state.clone();
            move |_, _| Split { state }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let boundary = cx.debug_bounds("second").unwrap().left();
        let y = px(50.);
        cx.simulate_mouse_down(
            point(boundary - px(2.), y),
            MouseButton::Left,
            Modifiers::default(),
        );
        // 第一段越过 GPUI 的拖拽阈值、建立拖拽；下一帧的移动才按把手序号改尺寸。
        cx.simulate_mouse_move(
            point(boundary + px(10.), y),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_move(
            point(px(220.), y),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_up(point(px(220.), y), MouseButton::Left, Modifiers::default());

        state.read_with(cx, |state, _| {
            assert_eq!(state.sizes(), &vec![px(220.), px(180.)]);
        });
    }
}
