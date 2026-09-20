use std::{cell::Cell, rc::Rc};

use gpui::{
    AppContext as _, Axis, Context, InteractiveElement as _, IntoElement, Modifiers, MouseButton,
    ParentElement as _, Render, Styled as _, TestAppContext, Window, div, point, px, size,
};
use gpui_component::resizable::{ResizableState, resizable_panel};

use crate::resizable_split::{
    RESIZE_HANDLE_HITBOX_SIZE, h_resizable, resize_handle_hover_color, resize_handle_line_bounds,
    resize_hover_handle, v_resizable,
};

struct SplitHarness {
    axis: Axis,
    state: gpui::Entity<ResizableState>,
}

struct HoverRefreshHarness {
    render_count: Rc<Cell<usize>>,
}

impl Render for HoverRefreshHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.render_count.set(self.render_count.get() + 1);
        div().w(px(100.)).h(px(100.)).relative().child(
            div()
                .absolute()
                .left(px(50.))
                .top_0()
                .w(px(50.))
                .h_full()
                .relative()
                .child(resize_hover_handle(Axis::Horizontal)),
        )
    }
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

#[test]
fn hover_line_is_centered_in_the_shared_hitbox_and_uses_the_emphasized_color() {
    let hitbox = gpui::Bounds::new(point(px(10.), px(20.)), size(px(8.), px(100.)));
    let line = resize_handle_line_bounds(Axis::Horizontal, hitbox);

    assert_eq!(RESIZE_HANDLE_HITBOX_SIZE, px(8.));
    assert_eq!(line.left(), px(14.));
    assert_eq!(line.size.width, px(1.));
    assert_eq!(line.size.height, px(100.));
    assert_eq!(
        resize_handle_hover_color(),
        gpui::rgb(crate::ui_theme::text_faint())
    );
}

#[gpui::test]
fn visual_only_layer_repaints_on_hover_transitions(cx: &mut TestAppContext) {
    let render_count = Rc::new(Cell::new(0));
    let (_, cx) = cx.add_window_view({
        let render_count = render_count.clone();
        move |_, _| HoverRefreshHarness { render_count }
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));

    let initial = render_count.get();
    cx.simulate_mouse_move(point(px(50.), px(50.)), None, Modifiers::default());
    assert_eq!(render_count.get(), initial + 1, "进入 8px 命中区要重画发丝");

    cx.simulate_mouse_move(point(px(51.), px(50.)), None, Modifiers::default());
    assert_eq!(render_count.get(), initial + 1, "命中区内移动不应逐帧重画");

    cx.simulate_mouse_move(point(px(70.), px(50.)), None, Modifiers::default());
    assert_eq!(render_count.get(), initial + 2, "离开命中区要擦掉发丝");
}

#[gpui::test]
fn horizontal_hover_layer_keeps_the_underlying_drag_working(cx: &mut TestAppContext) {
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
