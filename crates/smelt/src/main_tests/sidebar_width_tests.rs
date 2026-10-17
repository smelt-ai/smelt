use gpui::px;

use crate::sidebar_width_for_viewport;

#[test]
fn narrow_viewport_only_clamps_the_rendered_sidebar_width() {
    assert_eq!(sidebar_width_for_viewport(360., px(1000.)), 360.);
    assert_eq!(sidebar_width_for_viewport(360., px(700.)), 300.);
    assert_eq!(sidebar_width_for_viewport(360., px(500.)), 240.);
}
