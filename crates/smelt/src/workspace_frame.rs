//! 工作区三栏共用的卡片框架与顶栏样式。

use gpui::*;
use gpui_component::TitleBar;

/// 工作区自绘顶栏与 gpui-component TitleBar 共用同一高度契约。
pub(crate) const TOP_BAR_HEIGHT: Pixels = gpui_component::TITLE_BAR_HEIGHT;

/// Windows 主窗口用系统标题栏：拖动、贴边分屏、最小化 / 最大化 / 关闭都交给系统。
/// 透明标题栏下这些都得应用自己画，而 GPUI 在 Windows 上没有 `start_window_move`。
/// 自绘顶栏照常留在系统标题栏下面，只是不再当拖动区，客户区里也没有交通灯。
pub(crate) const NATIVE_TITLEBAR: bool = cfg!(windows);

/// 客户区里有没有交通灯要让位。macOS 全屏时隐藏，用系统标题栏时根本不在客户区。
pub(crate) fn traffic_lights_visible(window: &Window) -> bool {
    !NATIVE_TITLEBAR && !window.is_fullscreen()
}

/// 主窗口透明标题栏配置。调用方传入内容外壳的顶部内边距，原生交通灯应跟随
/// 自绘顶栏一起下移；横向位置维持 Smelt 左侧 18px 的视觉基准。
/// 用系统标题栏的平台只给窗口标题。
pub(crate) fn titlebar_options(shell_padding: Pixels) -> TitlebarOptions {
    if NATIVE_TITLEBAR {
        return TitlebarOptions {
            title: Some("Smelt".into()),
            appears_transparent: false,
            traffic_light_position: None,
        };
    }
    let mut options = TitleBar::title_bar_options();
    if let Some(position) = options.traffic_light_position.as_mut() {
        position.x = px(18.);
        position.y += shell_padding;
    }
    options
}

/// 主工作台的窗口选项。顶栏是自绘的：空白处拖动、双击缩放都由
/// [`with_window_drag`] 交给 `start_window_move` / `titlebar_double_click`。
///
/// `app_owns_titlebar_drag` 必须一起打开。否则 AppKit 仍把这 34px 当成系统
/// 标题栏，macOS 27 会把单击扣住等双击判定，页签和顶栏按钮经常点不下去。
/// 设置、会话追踪用系统标题栏，继续让 AppKit 拖，不要套这份选项。
pub(crate) fn workspace_window_options(
    shell_padding: Pixels,
    window_background: WindowBackgroundAppearance,
) -> WindowOptions {
    WindowOptions {
        titlebar: Some(titlebar_options(shell_padding)),
        window_background,
        app_owns_titlebar_drag: !NATIVE_TITLEBAR,
        ..Default::default()
    }
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

fn titlebar_mouse_down(event: &MouseDownEvent, window: &mut Window, cx: &mut App) {
    // 拖窗口的那一下停在标题区。胶囊和侧栏安全条都浮在内容上，不能再落到终端或消息。
    cx.stop_propagation();
    match titlebar_mouse_action(event.click_count) {
        TitlebarMouseAction::MoveWindow => window.start_window_move(),
        TitlebarMouseAction::DoubleClick => window.titlebar_double_click(),
    }
}

/// 卡片头和标题胶囊共用的拖窗口。单击移动，双击缩放，并拦住这一下。
///
/// 用系统标题栏时原样返回：拖动归系统标题栏。Windows 会把客户区里的 `Drag` 区域
/// 当成标题栏，叠在上面的按钮反而点不到。
pub(crate) fn with_window_drag<E: InteractiveElement>(el: E) -> E {
    if NATIVE_TITLEBAR {
        return el;
    }
    el.window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, titlebar_mouse_down)
}

/// 会话内容区共用的背景图层：始终覆盖容器，裁切方式和终端保持一致。
/// `opacity` 是图片透明度（0–1）——图片做低透明度装饰层，主题底色透出。
/// 透明度为 0 时不读文件。栅格图只画降采样预览，不把原图交给 `img()`。
pub(crate) fn background_image_layer(path: &str, opacity: f32, cx: &App) -> Div {
    let layer = div().absolute().inset_0();
    if opacity <= 0.0 {
        return layer;
    }
    let path = std::path::Path::new(path);
    smelt_ui::image::ensure_file_preview(path, cx);
    layer.child(
        div()
            .size_full()
            .opacity(opacity)
            .child(smelt_ui::image::file_surface(
                path,
                smelt_ui::image::FileFit::Cover,
            )),
    )
}

#[cfg(test)]
mod tests {
    use super::{TitlebarMouseAction, titlebar_mouse_action, workspace_window_options};
    use gpui::{WindowBackgroundAppearance, px};

    #[test]
    fn plain_titlebar_region_moves_or_double_clicks() {
        assert_eq!(titlebar_mouse_action(1), TitlebarMouseAction::MoveWindow);
        assert_eq!(titlebar_mouse_action(2), TitlebarMouseAction::DoubleClick);
    }

    #[cfg(not(windows))]
    #[test]
    fn native_traffic_lights_follow_the_shared_titlebar_geometry() {
        use super::titlebar_options;
        use gpui_component::TitleBar;

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

    #[cfg(windows)]
    #[test]
    fn windows_uses_the_system_titlebar() {
        let options = workspace_window_options(px(0.), WindowBackgroundAppearance::Opaque);
        assert!(!options.app_owns_titlebar_drag, "拖动归系统标题栏");
        let titlebar = options.titlebar.expect("主窗口要给系统标题栏一个标题");
        assert!(
            !titlebar.appears_transparent,
            "透明标题栏下 Windows 没有最小化 / 最大化 / 关闭，也拖不动"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn self_drawn_titlebar_owns_its_drag() {
        let options = workspace_window_options(px(0.), WindowBackgroundAppearance::Opaque);
        assert!(
            options.app_owns_titlebar_drag,
            "自绘顶栏必须声明应用拥有拖动，否则 AppKit 扣住标题栏里的单击"
        );
        let titlebar = options.titlebar.expect("主窗口使用透明标题栏");
        assert!(titlebar.appears_transparent);
    }
}
