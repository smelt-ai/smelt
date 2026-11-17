//! 窗口顶带：主窗口用透明标题栏，窗口顶上那一行由贴着窗口顶的视图自己画。这里是那一行的
//! 尺寸、拖窗口，和视图据此让位的 [`SafeArea`]。
//!
//! 照 Apple 的 safe area 和 macOS 26 的角让位：宿主控件（红黄绿按钮、停靠区开合按钮）占着
//! 窗口的角，视图问宿主自己被占了哪几个角、顶上是不是标题栏，按 [`SafeArea::insets`] 让开。
//! 容器把自己的那份按子视图碰到哪几条边分下去（[`SafeArea::child`]）。

use gpui::{
    Corners, Div, Edges, ElementId, InteractiveElement, MouseButton, MouseDownEvent, Pixels, Point,
    Size, Stateful, Styled as _, Window, WindowControlArea, div, point, px,
};

/// 窗口顶栏高。红黄绿按钮按它竖直居中（[`TRAFFIC_LIGHTS_ORIGIN`]）。
pub const TOP_BAR_HEIGHT: Pixels = px(34.);

/// 红黄绿按钮的位置，内容贴着窗口顶时：横向是 smelt 的 18px，竖向在 [`TOP_BAR_HEIGHT`]
/// 里居中（按钮 16 高）。外壳有顶部内边距时竖向跟着加。
pub const TRAFFIC_LIGHTS_ORIGIN: Point<Pixels> = point(px(18.), px(9.));

/// 红黄绿按钮连同两边留白占的宽度。
pub const TRAFFIC_LIGHTS_WIDTH: Pixels = px(80.);

/// Windows 主窗口用系统标题栏：拖动、贴边分屏、最小化 / 最大化 / 关闭都交给系统，客户区里
/// 没有红黄绿按钮，顶带也不当拖动区。
pub const NATIVE_TITLEBAR: bool = cfg!(windows);

/// 视图被宿主控件占掉的地方。只读内存，render 里问（`smelt_plugin_api::safe_area`）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SafeArea {
    /// 顶上一行是窗口标题栏：留顶栏高，空白处拖窗口，双击缩放。
    pub title_bar: bool,
    /// 四角被宿主控件占的宽高：左上 = 红黄绿按钮（全屏和 Windows 为 0）+ 左开合按钮，右上 =
    /// 右开合按钮。
    pub corners: Corners<Size<Pixels>>,
}

/// 内容怎么让开角上的控件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adapt {
    /// 和角上的控件同一行，横着让：标题栏里的标题、按钮。
    Horizontal,
    /// 从角上的控件下面开始，竖着让：标题栏下面的内容。
    Vertical,
}

impl SafeArea {
    /// 让开角（和标题栏）要留的边距。
    pub fn insets(&self, adapt: Adapt) -> Edges<Pixels> {
        let Corners {
            top_left,
            top_right,
            bottom_right,
            bottom_left,
        } = self.corners;
        match adapt {
            Adapt::Horizontal => Edges {
                left: top_left.width.max(bottom_left.width),
                right: top_right.width.max(bottom_right.width),
                ..Default::default()
            },
            Adapt::Vertical => Edges {
                top: top_left
                    .height
                    .max(top_right.height)
                    .max(if self.title_bar {
                        TOP_BAR_HEIGHT
                    } else {
                        px(0.)
                    }),
                bottom: bottom_left.height.max(bottom_right.height),
                ..Default::default()
            },
        }
    }

    /// 子视图的那份：只留它碰到的边上的角，碰到顶边才有标题栏。碰到哪几条边由父视图按
    /// 结构给（排在第一个、横排里每一个都碰顶），不按像素量。
    pub fn child(&self, touches: Edges<bool>) -> SafeArea {
        let keep = |corner: Size<Pixels>, a: bool, b: bool| {
            if a && b { corner } else { Size::default() }
        };
        SafeArea {
            title_bar: self.title_bar && touches.top,
            corners: Corners {
                top_left: keep(self.corners.top_left, touches.top, touches.left),
                top_right: keep(self.corners.top_right, touches.top, touches.right),
                bottom_right: keep(self.corners.bottom_right, touches.bottom, touches.right),
                bottom_left: keep(self.corners.bottom_left, touches.bottom, touches.left),
            },
        }
    }
}

/// 窗口顶带：[`TOP_BAR_HEIGHT`] 高的一行，两头按 `area` 给角让位，空白处拖窗口、双击缩放。
/// `area.title_bar` 为真时才画——不贴着窗口顶的视图顶上不是标题栏。
///
/// 里面的控件按下要 `prevent_default`（本库的按钮都会），不然按下就成了拖窗口。
pub fn window_bar(id: impl Into<ElementId>, area: &SafeArea) -> Stateful<Div> {
    let insets = area.insets(Adapt::Horizontal);
    with_window_drag(
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .w_full()
            .h(TOP_BAR_HEIGHT)
            .pl(insets.left)
            .pr(insets.right),
    )
}

/// 按住拖窗口，双击缩放，并拦住这一下（拖窗口的条常浮在内容上，不能再落到下面）。
///
/// 用系统标题栏时原样返回：Windows 会把客户区里的 `Drag` 区域当成标题栏，叠在上面的
/// 东西反而点不到。
pub fn with_window_drag<E: InteractiveElement>(el: E) -> E {
    if NATIVE_TITLEBAR {
        return el;
    }
    el.window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, titlebar_mouse_down)
}

fn titlebar_mouse_down(event: &MouseDownEvent, window: &mut Window, cx: &mut gpui::App) {
    cx.stop_propagation();
    // 按在控件上：按钮按下、可聚焦的元素拿焦点时都会 `prevent_default`，子元素先收到这一下。
    // 这时不拖窗口：一进系统的拖动循环，松手就到不了控件，点不下去。
    if window.default_prevented() {
        return;
    }
    if event.click_count == 2 {
        window.titlebar_double_click();
    } else {
        window.start_window_move();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    fn lights() -> SafeArea {
        SafeArea {
            title_bar: true,
            corners: Corners {
                top_left: size(px(112.), TOP_BAR_HEIGHT),
                top_right: size(px(32.), TOP_BAR_HEIGHT),
                ..Default::default()
            },
        }
    }

    #[test]
    fn insets_go_sideways_in_the_bar_and_down_below_it() {
        let area = lights();
        let row = area.insets(Adapt::Horizontal);
        assert_eq!((row.left, row.right, row.top), (px(112.), px(32.), px(0.)));
        let below = area.insets(Adapt::Vertical);
        assert_eq!((below.top, below.left), (TOP_BAR_HEIGHT, px(0.)));
        // 没有角，只有标题栏：竖着也要让出顶栏。
        let bare = SafeArea {
            title_bar: true,
            ..Default::default()
        };
        assert_eq!(bare.insets(Adapt::Vertical).top, TOP_BAR_HEIGHT);
        assert_eq!(SafeArea::default().insets(Adapt::Vertical).top, px(0.));
    }

    #[test]
    fn children_keep_only_the_corners_they_touch() {
        let area = lights();
        let all = Edges {
            top: true,
            right: true,
            bottom: true,
            left: true,
        };
        assert_eq!(area.child(all), area);
        // 横排左边那个：碰顶、碰左。
        let left = area.child(Edges {
            right: false,
            ..all
        });
        assert!(left.title_bar);
        assert_eq!(left.corners.top_left, area.corners.top_left);
        assert_eq!(left.corners.top_right, Size::default());
        // 竖排下面那个：不碰顶，没有标题栏也没有上面的角。
        let below = area.child(Edges { top: false, ..all });
        assert_eq!(below, SafeArea::default());
    }
}
