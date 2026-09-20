use gpui::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
    TestAppContext, Window, div, px,
};

use crate::{fixed_sidebar_columns, sidebar_width_for_viewport};

struct SidebarLayoutHarness {
    container_width: gpui::Pixels,
    sidebar_width: gpui::Pixels,
}

impl Render for SidebarLayoutHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let sidebar = div()
            .size_full()
            .debug_selector(|| "sidebar-panel".into())
            .into_any_element();
        let content = div().size_full().into_any_element();

        div()
            .w(self.container_width)
            .h(px(100.))
            .child(fixed_sidebar_columns(
                self.sidebar_width,
                px(240.),
                sidebar,
                content,
            ))
    }
}

#[gpui::test]
fn window_resize_does_not_change_the_fixed_sidebar_width(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(move |_, _| SidebarLayoutHarness {
        container_width: px(800.),
        sidebar_width: px(320.),
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(
        cx.debug_bounds("sidebar-panel").unwrap().size.width,
        px(320.)
    );

    view.update(cx, |view, cx| {
        view.container_width = px(1000.);
        cx.notify();
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));

    assert_eq!(
        cx.debug_bounds("sidebar-panel").unwrap().size.width,
        px(320.),
        "侧栏是持久化的固定像素宽度，窗口变化不能让运行时宽度与 SQLite 偏好分叉"
    );
}

#[test]
fn narrow_viewport_only_clamps_the_rendered_sidebar_width() {
    assert_eq!(sidebar_width_for_viewport(360., px(1000.)), 360.);
    assert_eq!(sidebar_width_for_viewport(360., px(700.)), 300.);
    assert_eq!(sidebar_width_for_viewport(360., px(500.)), 240.);
}
