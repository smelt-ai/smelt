//! 工作区三栏共用的卡片框架与顶栏样式。

use gpui::*;

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

/// 外壳第一行共用的表面。曾经用来把顶栏刷成跟卡片主体不同的 bg_bar，制造
/// 「工具栏 vs 内容区」的层次——三张卡片（侧栏/舞台/Inspector）的主体色板
/// 本来就不一样（bg_elev / bg_panel），叠上同一个 bg_bar 后每张卡片顶边看着
/// 都是「一块不一样的颜色」，不沉浸。现在改成完全透明，头栏跟卡片主体融成
/// 一整块——圆角留着（虽然透明背景下看不出效果，万一以后哪天想恢复底色，
/// 圆角裁切已经配好，不用重新算)。
pub(crate) fn top_bar() -> Div {
    div().rounded_t(px(CARD_RADIUS))
}
