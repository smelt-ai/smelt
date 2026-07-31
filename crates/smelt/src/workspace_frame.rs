//! 工作区三栏共用的卡片框架与顶栏样式。

use gpui::*;

use crate::ui_theme;

const CARD_RADIUS: f32 = 8.;

/// 左侧栏、舞台和 Inspector 共用的外壳。
pub(crate) fn card(surface: Hsla) -> Div {
    div()
        .size_full()
        .flex()
        .relative()
        .overflow_hidden()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(gpui::transparent_black())
        .bg(surface)
        .shadow_sm()
}

/// 外壳第一行共用的表面。GPUI 不会按父级圆角裁切子元素背景，因此这里显式
/// 设置相同的上圆角。
pub(crate) fn top_bar() -> Div {
    div()
        .rounded_t(px(CARD_RADIUS))
        .bg(rgb(ui_theme::bg_bar()))
        .border_b_1()
        .border_color(rgb(ui_theme::border_dim()))
}
