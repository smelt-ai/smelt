//! 设计令牌：组件的字体、尺寸、字、阴影、动效、图标只从这里取，颜色只从 [`crate::ui_theme`] 取。
//!
//! 分几块：调色板（[`crate::ui_theme::Palette`]，随主题整份换）、
//! 浮起层级 [`elevation`]、字 [`TYPE`]、圆角 [`RADIUS`]、动效 [`MOTION`]、控件尺寸
//! [`ControlSize`]、图标 [`GLYPHS`]。

use std::time::Duration;

use gpui::{FontWeight, Styled, rems};
use gpui_base::motion::Spring;

use crate::IconName;

/// 界面字体族。随 app 带的静态字面见 [`UI_FONTS`]，由 [`crate::init`] 载入。
pub const UI_FONT_FAMILY: &str = "Inter";

/// Inter 4.001 的六个静态字重：400、440、500、530、600、700。
///
/// 不带可变字体：GPUI 按 CSS 字重在同族字面里挑最近的，不设可变轴，可变字体只画得出 400。
/// 字面由 `scripts/inter-static.py` 从可变字体切出，opsz 定在 14；许可证
/// `assets/fonts/Inter-OFL.txt`（OFL 1.1）。
pub(crate) const UI_FONTS: [&[u8]; 6] = [
    include_bytes!("../../../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../../../assets/fonts/Inter-W440.ttf"),
    include_bytes!("../../../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../../../assets/fonts/Inter-W530.ttf"),
    include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/Inter-Bold.ttf"),
];

/// 控件尺寸档，默认 `Regular`。各控件按档给自己的高度、内边距和圆角。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ControlSize {
    Mini,
    Small,
    #[default]
    Regular,
    Large,
    ExtraLarge,
}

/// 圆角阶梯（px）：2 / 4 / 6 / 8 / 10。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Radius {
    /// 键帽、徽标。
    pub xs: f32,
    /// 提示、复选框、菜单项、`Mini` / `Small` 按钮。
    pub sm: f32,
    /// `Regular` 按钮、输入框、分段控件、列表行、菜单外框、弹出卡片、对话框。
    pub md: f32,
    /// `Large` 按钮、悬停卡片。
    pub lg: f32,
    /// `ExtraLarge` 按钮。
    pub xl: f32,
}

pub const RADIUS: Radius = Radius {
    xs: 2.,
    sm: 4.,
    md: 6.,
    lg: 8.,
    xl: 10.,
};

/// 一档字：字号、行高（px，界面字号 16 时）、字重。
///
/// 设计上 `ui`、`small` 还带字距（-0.04 / +0.05），GPUI 没有字距，画不出来。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub size: f32,
    pub line_height: f32,
    pub weight: FontWeight,
}

impl TextStyle {
    const fn new(size: f32, line_height: f32, weight: f32) -> Self {
        Self {
            size,
            line_height,
            weight: FontWeight(weight),
        }
    }

    /// 换行高：单行控件按自己的高度收行高。
    pub const fn lh(self, line_height: f32) -> Self {
        Self {
            line_height,
            ..self
        }
    }

    /// 套到元素上。字号和行高按 rem 给（16px = 1rem），跟着设置里的界面字号一起缩放。
    pub fn apply<E: Styled>(self, element: E) -> E {
        element
            .text_size(rems(self.size / 16.))
            .line_height(rems(self.line_height / 16.))
            .font_weight(self.weight)
    }
}

/// 给元素套一档字：`.type_style(TYPE.ui)`。界面里的字号都从 [`TYPE`] 取，不手写
/// `text_sm()` / `text_xs()`，字重也跟着档走（要加重就用 `*_medium` 那档）。
pub trait TypeStyled: Styled + Sized {
    fn type_style(self, style: TextStyle) -> Self {
        style.apply(self)
    }
}

impl<E: Styled> TypeStyled for E {}

/// 字阶：十二档，控件、正文、说明、等宽各几档。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Type {
    /// 控件正文：按钮、输入框、菜单项、分段控件。
    pub ui: TextStyle,
    /// 控件里要压一档的字：按钮、标签名、对话框标题。
    pub ui_medium: TextStyle,
    /// 单行控件里的字：输入框、菜单行、下拉框。
    pub ui_compact: TextStyle,
    /// 徽标、提示、分组名。
    pub small: TextStyle,
    pub small_medium: TextStyle,
    /// 正文段落。
    pub body: TextStyle,
    pub body_medium: TextStyle,
    /// 说明文字。
    pub text_small: TextStyle,
    pub text_small_medium: TextStyle,
    pub text_xsmall: TextStyle,
    pub text_xsmall_medium: TextStyle,
    /// 等宽：字号和行高；字体族是用户设的等宽字体，不在这里定。
    pub mono: TextStyle,
}

impl Type {
    /// 小字配宽松行高：控件下面的说明、提示。
    pub const fn caption(&self) -> TextStyle {
        self.small.lh(16.)
    }
}

pub const TYPE: Type = Type {
    ui: TextStyle::new(13., 20., 440.),
    ui_medium: TextStyle::new(13., 20., 530.),
    ui_compact: TextStyle::new(13., 16., 440.),
    small: TextStyle::new(11., 12., 440.),
    small_medium: TextStyle::new(11., 12., 530.),
    body: TextStyle::new(14., 21., 400.),
    body_medium: TextStyle::new(14., 21., 500.),
    text_small: TextStyle::new(13., 19.5, 400.),
    text_small_medium: TextStyle::new(13., 19.5, 500.),
    text_xsmall: TextStyle::new(12., 18., 400.),
    text_xsmall_medium: TextStyle::new(12., 18., 500.),
    mono: TextStyle::new(12., 18., 400.),
};

/// 动效：临界阻尼弹簧（不过冲），值是响应周期。替代网页上 85–170ms
/// 的 ease-out。系统开了减弱动态效果时 gpui-base 的 `spring` 直接到位。
#[derive(Clone, Copy, Debug)]
pub struct Motion {
    /// 控件自身的状态变化：开关滑块、分段指示、按下。
    pub control: Spring,
    /// 浮层出现：菜单、弹出卡片、对话框、提示。
    pub overlay: Spring,
    /// 展开收起。
    pub disclosure: Spring,
}

impl Motion {
    /// 浮层入场用的定时过渡：浮层每次打开都是新挂上的元素，弹簧首帧就到位，入场只能从 0
    /// 播一段时间。时长取 overlay 的响应周期。组件经 [`crate::motion::overlay_enter`] 取用。
    pub fn overlay_enter(&self) -> gpui_base::motion::Transition {
        gpui_base::motion::Transition::new(OVERLAY_RESPONSE).ease(crate::motion::ease_snap)
    }
}

const OVERLAY_RESPONSE: Duration = Duration::from_millis(240);

pub const MOTION: Motion = Motion {
    control: Spring::new(Duration::from_millis(200)),
    overlay: Spring::new(OVERLAY_RESPONSE),
    disclosure: Spring::new(Duration::from_millis(300)),
};

/// 组件用到的图标。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyphs {
    pub check: IconName,
    pub copy: IconName,
    /// 关闭、清空。
    pub close: IconName,
    pub chevron_down: IconName,
    /// 子菜单。
    pub chevron_right: IconName,
    pub loader: IconName,
    /// 删除、移除。
    pub trash: IconName,
    pub eye: IconName,
    pub eye_off: IconName,
    /// 可搜索列表的搜索框。
    pub search: IconName,
}

pub const GLYPHS: Glyphs = Glyphs {
    check: IconName::Check,
    copy: IconName::Copy,
    close: IconName::Close,
    chevron_down: IconName::ChevronDown,
    chevron_right: IconName::ChevronRight,
    loader: IconName::Loader,
    trash: IconName::Trash,
    eye: IconName::Eye,
    eye_off: IconName::EyeOff,
    search: IconName::Search,
};

/// 浮起层级：每档是几层投影叠起来，取自当前主题（主题文件的 `elevation` 一节，没写的档按
/// [`crate::theme_config::Elevation`] 的规则推）。这里只是按档取的简写，表在主题里，不在代码里。
///
/// GPUI 的外投影也画进矩形内部：用外投影的元素底色要不透明，否则透出一圈灰。
pub mod elevation {
    use std::sync::Arc;

    use gpui::BoxShadow;

    pub use crate::theme_config::Elevation;
    use crate::ui_theme;

    /// 当前主题的整套投影。
    pub fn current() -> Arc<Elevation> {
        ui_theme::elevation()
    }

    /// 菜单、提示。
    pub fn floating() -> Vec<BoxShadow> {
        current().floating.clone()
    }

    /// 对话框。
    pub fn overlay() -> Vec<BoxShadow> {
        current().overlay.clone()
    }

    /// 浮动按钮。
    pub fn raised() -> Vec<BoxShadow> {
        current().raised.clone()
    }

    /// 自带边框的弹层。
    pub fn md() -> Vec<BoxShadow> {
        current().md.clone()
    }

    /// 输入框、下拉框。
    pub fn input() -> Vec<BoxShadow> {
        current().input.clone()
    }

    /// 开关圆钮这类贴在控件上的小块。
    pub fn elements() -> Vec<BoxShadow> {
        current().elements.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_scale_levels() {
        assert_eq!(TYPE.ui, TextStyle::new(13., 20., 440.));
        assert_eq!(TYPE.ui_compact.line_height, 16.);
        assert_eq!(TYPE.small_medium.weight, FontWeight(530.));
        assert_eq!(TYPE.caption(), TextStyle::new(11., 16., 440.));
    }

    #[test]
    fn every_font_weight_in_use_has_a_face() {
        // 静态字面的字重和 Type 里用到的字重一一对上，GPUI 才不会退到最近的别的字重。
        let faces = [400., 440., 500., 530., 600., 700.];
        assert_eq!(UI_FONTS.len(), faces.len());
        for style in [
            TYPE.ui,
            TYPE.ui_medium,
            TYPE.ui_compact,
            TYPE.small,
            TYPE.small_medium,
            TYPE.body,
            TYPE.body_medium,
            TYPE.text_small,
            TYPE.text_small_medium,
            TYPE.text_xsmall,
            TYPE.text_xsmall_medium,
            TYPE.mono,
        ] {
            assert!(faces.contains(&style.weight.0), "{style:?}");
        }
    }
}
