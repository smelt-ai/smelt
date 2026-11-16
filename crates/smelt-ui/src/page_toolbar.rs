//! 页面的工具行：插件页、远程桌面内容最上面的一行。左边放切换、返回和名字，右边放新建
//! 这类操作。
//!
//! 窗口顶上那条标题栏（34 高，和红黄绿按钮同一行）归外壳画，只写页名、拖窗口；按钮和分段
//! 控件比它矮不了多少，塞进去上下就贴边了，所以都放在这一行里，用默认大小。

use gpui::{Div, Styled as _, div, px};

use crate::ui_theme;

/// 工具行至少多高：默认大小的控件（32）上下各留 8。只放文字的时候也这么高，同一页切来
/// 切去（列表、详情）这一行不跳。
const MIN_HEIGHT: f32 = 48.;

/// 一页最上面的工具行，底边一条发丝和内容分开。左右内边距 20，和页面内容对齐。
pub fn page_toolbar() -> Div {
    div()
        .w_full()
        .flex_shrink_0()
        .min_h(px(MIN_HEIGHT))
        .px(px(20.))
        .py(px(8.))
        .flex()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(ui_theme::hair())
}
