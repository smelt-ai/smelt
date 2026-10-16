use gpui::{
    AppContext as _, Axis, Context, InteractiveElement, IntoElement, Modifiers, MouseButton,
    ParentElement as _, Render, Styled as _, TestAppContext, Window, div, point, px,
};
use gpui_component::resizable::{ResizableState, resizable_panel};

use crate::resizable_split::{h_resizable, v_resizable};

struct SplitHarness {
    axis: Axis,
    state: gpui::Entity<ResizableState>,
}

impl Render for SplitHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let group = match self.axis {
            Axis::Horizontal => h_resizable("hover-handle-test"),
            Axis::Vertical => v_resizable("hover-handle-test"),
        }
        .with_state(&self.state)
        .child(
            resizable_panel().size(px(150.)).child(
                div()
                    .size_full()
                    .debug_selector(|| "first-split-panel".into()),
            ),
        )
        .child(
            resizable_panel().size(px(250.)).child(
                div()
                    .size_full()
                    .debug_selector(|| "second-split-panel".into()),
            ),
        );

        match self.axis {
            Axis::Horizontal => div().w(px(400.)).h(px(100.)).child(group),
            Axis::Vertical => div().w(px(100.)).h(px(400.)).child(group),
        }
    }
}

#[gpui::test]
fn horizontal_split_drag_still_resizes_the_panels(cx: &mut TestAppContext) {
    // 发丝颜色走 ui_theme。组件库仍先 init：和主窗口同一套全局，
    // 手柄的内置回退线也会读 Theme。
    cx.update(gpui_component::init);
    let state = cx.update(|cx| cx.new(|_| ResizableState::default()));
    let (_, cx) = cx.add_window_view({
        let state = state.clone();
        move |_, _| SplitHarness {
            axis: Axis::Horizontal,
            state,
        }
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));

    let boundary = cx.debug_bounds("second-split-panel").unwrap().left();
    cx.simulate_mouse_down(
        point(boundary - px(2.), px(50.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    // 第一段 move 越过 GPUI 的 drag threshold 并建立 active drag；下一帧的 move
    // 才由 ResizableState 按新的 handle index 改尺寸。
    cx.simulate_mouse_move(
        point(boundary + px(10.), px(50.)),
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    cx.simulate_mouse_move(
        point(px(220.), px(50.)),
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    cx.simulate_mouse_up(
        point(px(220.), px(50.)),
        MouseButton::Left,
        Modifiers::default(),
    );

    state.read_with(cx, |state, _| {
        assert_eq!(state.sizes(), &vec![px(220.), px(180.)]);
    });
}

#[gpui::test]
fn vertical_split_uses_the_same_facade_without_changing_layout(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let state = cx.update(|cx| cx.new(|_| ResizableState::default()));
    let (_, cx) = cx.add_window_view({
        move |_, _| SplitHarness {
            axis: Axis::Vertical,
            state,
        }
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));

    let first = cx.debug_bounds("first-split-panel").unwrap();
    let second = cx.debug_bounds("second-split-panel").unwrap();
    assert_eq!(first.size.height, px(150.));
    assert_eq!(second.top(), first.bottom());
}
