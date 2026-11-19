use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, ScrollDelta,
    ScrollEvent, Styled as _, TestAppContext, VisualTestContext, Window, div, point, px,
};

use super::{Diff, DiffState, document::fixture};

/// Every line changed, so the unified patch is two rows per line.
const LINES: usize = 300;

struct Host {
    state: Entity<DiffState>,
    /// Sized to its rows inside a box that only caps the height, the way a
    /// dialog card holds it, instead of filling a fixed box.
    fit: bool,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        if self.fit {
            return div()
                .w(px(400.))
                .max_h(px(200.))
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(Diff::new(&self.state).fit_height(true));
        }
        div()
            .w(px(400.))
            .h(px(200.))
            .child(Diff::new(&self.state).size_full())
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
}

fn long_patch(cx: &mut TestAppContext) -> (Entity<DiffState>, &mut VisualTestContext) {
    long_patch_in(cx, false)
}

fn long_patch_in(
    cx: &mut TestAppContext,
    fit: bool,
) -> (Entity<DiffState>, &mut VisualTestContext) {
    cx.update(super::test_init);
    let original = (0..LINES)
        .map(|ix| format!("old {ix}\n"))
        .collect::<String>();
    let modified = (0..LINES)
        .map(|ix| format!("new {ix}\n"))
        .collect::<String>();
    let state =
        cx.new(|cx| DiffState::new([fixture::modified("long.txt", &original, &modified)], cx));
    let (_, cx) = cx.add_window_view({
        let state = state.clone();
        move |_, _| Host { state, fit }
    });
    // The body records its size on the first paint and lays out again.
    for _ in 0..3 {
        draw(cx);
    }
    (state, cx)
}

#[gpui::test]
fn the_whole_patch_keeps_its_height_after_the_first_layout(cx: &mut TestAppContext) {
    let (state, cx) = long_patch(cx);
    let (max, viewport) = cx.update(|_, cx| {
        let list = state.read(cx).list();
        (
            list.max_offset_for_scrollbar().y,
            list.viewport_bounds().size.height,
        )
    });
    // Rows past the viewport have not been rendered; without their hinted
    // height the patch would read as barely taller than one screen.
    assert!(
        max > viewport * 10.,
        "max offset {max:?} for a {viewport:?} viewport"
    );
}

#[gpui::test]
fn the_wheel_scrolls_past_the_rows_rendered_so_far(cx: &mut TestAppContext) {
    let (state, cx) = long_patch(cx);
    for _ in 0..50 {
        cx.simulate_event(ScrollEvent {
            position: point(px(100.), px(120.)),
            delta: ScrollDelta::Pixels(point(px(0.), px(-40.))),
            ..Default::default()
        });
        draw(cx);
    }
    let top = cx.update(|_, cx| state.read(cx).list().logical_scroll_top().item_ix);
    // 2000 px of wheel travel crosses far more than the first screen of rows.
    assert!(top > 40, "scrolled to row {top}");
}

/// Wheel travel over the patch, then the row at the top.
fn scroll_down(state: &Entity<DiffState>, cx: &mut VisualTestContext) -> usize {
    for _ in 0..50 {
        cx.simulate_event(ScrollEvent {
            position: point(px(100.), px(120.)),
            delta: ScrollDelta::Pixels(point(px(0.), px(-40.))),
            ..Default::default()
        });
        draw(cx);
    }
    cx.update(|_, cx| state.read(cx).list().logical_scroll_top().item_ix)
}

#[gpui::test]
fn a_fitted_patch_past_its_cap_scrolls_inside_it(cx: &mut TestAppContext) {
    let (state, cx) = long_patch_in(cx, true);
    let viewport = cx.update(|_, cx| state.read(cx).list().viewport_bounds().size.height);
    // The cap squeezes the list itself; a list as tall as its rows would only
    // be clipped by the box and never scroll.
    assert!(viewport <= px(200.), "viewport {viewport:?} past the cap");
    let top = scroll_down(&state, cx);
    assert!(top > 40, "scrolled to row {top}");
}
