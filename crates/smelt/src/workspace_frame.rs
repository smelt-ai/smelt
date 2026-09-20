//! 工作区三栏共用的卡片框架与顶栏样式。

use gpui::*;
use gpui_component::TitleBar;

/// 工作区自绘顶栏与 gpui-component TitleBar 共用同一高度契约。
pub(crate) const TOP_BAR_HEIGHT: Pixels = gpui_component::TITLE_BAR_HEIGHT;

/// 主窗口透明标题栏配置。调用方传入内容外壳的顶部内边距，原生交通灯应跟随
/// 自绘顶栏一起下移；横向位置维持 Smelt 左侧 18px 的视觉基准。
pub(crate) fn titlebar_options(shell_padding: Pixels) -> TitlebarOptions {
    let mut options = TitleBar::title_bar_options();
    if let Some(position) = options.traffic_light_position.as_mut() {
        position.x = px(18.);
        position.y += shell_padding;
    }
    options
}

/// 会话栏、舞台和工具栏共用的外壳。Grok Bot 分栏是贴边矩形，不是浮在壳里的圆角卡。
pub(crate) fn card(surface: Hsla) -> Div {
    div()
        .size_full()
        .flex()
        .relative()
        .overflow_hidden()
        .bg(surface)
}

/// 外壳第一行共用的表面。头栏跟分栏主体同一块实底，不再单独刷色或切圆角。
pub(crate) fn top_bar() -> Div {
    div()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TitlebarMouseAction {
    MoveWindow,
    DoubleClick,
}

fn titlebar_mouse_action(click_count: usize) -> TitlebarMouseAction {
    if click_count == 2 {
        TitlebarMouseAction::DoubleClick
    } else {
        TitlebarMouseAction::MoveWindow
    }
}

/// 卡片头上的拖窗口区域：浮层不再整条抢点击之后，拖拽绑在各栏自己的头上。
pub(crate) fn with_window_drag(el: Div) -> Div {
    el.window_control_area(WindowControlArea::Drag)
        .on_mouse_down(
            MouseButton::Left,
            |event, window, _| match titlebar_mouse_action(event.click_count) {
                TitlebarMouseAction::MoveWindow => window.start_window_move(),
                TitlebarMouseAction::DoubleClick => window.titlebar_double_click(),
            },
        )
}

/// 会话内容区共用的背景图层：始终覆盖容器，裁切方式和终端保持一致。
/// `opacity` 是图片透明度（0–1）——图片做低透明度装饰层，主题底色/材质透出，
/// 避免明亮图片直接铺满压过内容。
pub(crate) fn background_image_layer(path: &str, opacity: f32) -> Div {
    div().absolute().inset_0().child(
        img(std::path::PathBuf::from(path))
            .absolute()
            .inset_0()
            .size_full()
            .object_fit(ObjectFit::Cover)
            .opacity(opacity),
    )
}

#[cfg(test)]
mod tests {
    use super::{TitlebarMouseAction, titlebar_mouse_action, titlebar_options};
    use gpui::px;
    use gpui_component::TitleBar;

    #[test]
    fn plain_titlebar_region_moves_or_double_clicks() {
        assert_eq!(titlebar_mouse_action(1), TitlebarMouseAction::MoveWindow);
        assert_eq!(titlebar_mouse_action(2), TitlebarMouseAction::DoubleClick);
    }

    #[test]
    fn native_traffic_lights_follow_the_shared_titlebar_geometry() {
        let component_position = TitleBar::title_bar_options()
            .traffic_light_position
            .expect("gpui-component 的 macOS TitleBar 应声明交通灯位置");

        let flush_position = titlebar_options(px(0.))
            .traffic_light_position
            .expect("Smelt 透明标题栏应声明交通灯位置");
        assert_eq!(flush_position.x, px(18.));
        assert_eq!(
            flush_position.y, component_position.y,
            "内容贴顶时必须复用与 34px TitleBar 匹配的原生纵向位置"
        );

        let inset = px(10.);
        let inset_position = titlebar_options(inset)
            .traffic_light_position
            .expect("Smelt 透明标题栏应声明交通灯位置");
        assert_eq!(
            inset_position.y,
            component_position.y + inset,
            "外壳顶部内边距变化时，交通灯必须随自绘顶栏一起移动"
        );
    }
}
