//! 主窗口的框架：标题栏、窗口底、拖窗口区和宿主页的标题行。

use gpui::*;
use smelt_ui::window_bar::{
    Adapt, NATIVE_TITLEBAR, SafeArea, TOP_BAR_HEIGHT, TRAFFIC_LIGHTS_ORIGIN, TRAFFIC_LIGHTS_WIDTH,
    with_window_drag,
};

/// 客户区里红黄绿按钮要让的宽度。macOS 全屏时隐藏，用系统标题栏时根本不在客户区，都是 0。
pub(crate) fn traffic_lights_width(window: &Window) -> Pixels {
    if NATIVE_TITLEBAR || window.is_fullscreen() {
        px(0.)
    } else {
        TRAFFIC_LIGHTS_WIDTH
    }
}

/// 主窗口透明标题栏配置。调用方传入内容外壳的顶部内边距，原生交通灯跟着自绘顶栏一起下移。
/// 用系统标题栏的平台只给窗口标题。
pub(crate) fn titlebar_options(shell_padding: Pixels) -> TitlebarOptions {
    if NATIVE_TITLEBAR {
        return TitlebarOptions {
            title: Some("Smelt".into()),
            appears_transparent: false,
            traffic_light_position: None,
        };
    }
    TitlebarOptions {
        title: None,
        appears_transparent: true,
        traffic_light_position: Some(point(
            TRAFFIC_LIGHTS_ORIGIN.x,
            TRAFFIC_LIGHTS_ORIGIN.y + shell_padding,
        )),
    }
}

/// 主工作台的窗口选项。顶栏是自绘的：空白处拖动、双击缩放都由
/// `smelt_ui::window_bar::with_window_drag` 交给 `start_window_move` / `titlebar_double_click`。
///
/// `app_owns_titlebar_drag` 必须一起打开。否则 AppKit 仍把这 34px 当成系统
/// 标题栏，macOS 27 会把单击扣住等双击判定，页签和顶栏按钮经常点不下去。
/// 设置、会话追踪用系统标题栏，继续让 AppKit 拖，不要套这份选项。
pub(crate) fn workspace_window_options(
    shell_padding: Pixels,
    material: crate::window_backdrop::WindowMaterial,
) -> WindowOptions {
    WindowOptions {
        titlebar: Some(titlebar_options(shell_padding)),
        window_background: crate::window_backdrop::window_background(material),
        app_owns_titlebar_drag: !NATIVE_TITLEBAR,
        ..Default::default()
    }
}

/// 整扇窗口的底：有窗口材质（`window_backdrop`）时什么都不铺，毛玻璃就是底；没有时铺一层
/// `bg`。全窗口只有这一处管底，页签栏和各个视图（自带的、插件的）都不铺页面底，铺了就把
/// 材质盖住。
pub(crate) fn window_surface() -> Hsla {
    if crate::window_backdrop::available() {
        gpui::transparent_black()
    } else {
        rgb(crate::ui_theme::bg()).into()
    }
}

/// 整扇窗口的背景图层（挂在窗口根上，见 `Workspace::render`）：铺满容器，按比例裁切。
/// `opacity` 是图片透明度（0–1）——图片做低透明度装饰层，底下的材质（没材质时是 `bg`）透出。
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

/// 自己画顶上那条的宿主页（设置）的标题行。只放标题文字这类不比字高的东西：贴着窗口顶时
/// 它就是 [`TOP_BAR_HEIGHT`] 高的标题栏，放不下按钮和分段控件，那些放进页面自己的工具行
/// （`smelt_ui::page_toolbar`）。
///
/// `area` 是这个视图的 safe area（`smelt_plugin_api::safe_area`）。顶上是标题栏时两头给角上的
/// 控件让位，空白处拖窗口；不是（在标签条下面）时顶上那条归标签条，这里只是一条常规内边距的
/// 头。底边是发丝。
pub(crate) fn page_chrome(area: &SafeArea) -> Div {
    let insets = area.insets(Adapt::Horizontal);
    let header = div()
        .w_full()
        .flex_shrink_0()
        .pl(insets.left + px(20.))
        .pr(insets.right + px(20.))
        .border_b_1()
        .border_color(crate::ui_theme::hair())
        .flex()
        .items_center();
    if area.title_bar {
        with_window_drag(header.min_h(TOP_BAR_HEIGHT))
    } else {
        header.py(px(8.))
    }
}

#[cfg(test)]
mod tests {
    use super::workspace_window_options;
    use gpui::px;

    #[cfg(not(windows))]
    #[test]
    fn native_traffic_lights_follow_the_top_bar() {
        use super::{TRAFFIC_LIGHTS_ORIGIN, titlebar_options};

        let flush = titlebar_options(px(0.))
            .traffic_light_position
            .expect("Smelt 透明标题栏应声明交通灯位置");
        assert_eq!(flush, TRAFFIC_LIGHTS_ORIGIN, "内容贴顶时交通灯在顶栏里居中");

        let inset = px(10.);
        let moved = titlebar_options(inset)
            .traffic_light_position
            .expect("Smelt 透明标题栏应声明交通灯位置");
        assert_eq!(moved.x, TRAFFIC_LIGHTS_ORIGIN.x);
        assert_eq!(
            moved.y,
            TRAFFIC_LIGHTS_ORIGIN.y + inset,
            "外壳顶部内边距变化时，交通灯必须随自绘顶栏一起移动"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_uses_the_system_titlebar() {
        let options = workspace_window_options(
            px(0.),
            crate::window_backdrop::WindowMaterial::platform_default(),
        );
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
        let options = workspace_window_options(
            px(0.),
            crate::window_backdrop::WindowMaterial::platform_default(),
        );
        assert!(
            options.app_owns_titlebar_drag,
            "自绘顶栏必须声明应用拥有拖动，否则 AppKit 扣住标题栏里的单击"
        );
        let titlebar = options.titlebar.expect("主窗口使用透明标题栏");
        assert!(titlebar.appears_transparent);
    }
}
