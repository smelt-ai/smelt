//! 图标：一枚 svg，默认跟着所在处的字号和字色，样式照普通元素写（`size_3p5()`、
//! `text_color(..)`）。名字用 gpui-kit 自带的 Lucide 集（[`IconName`]），应用资源里的
//! svg 用 [`Icon::path`]。只是摆一枚图标、不改大小颜色时，直接 `.child(IconName::X)`。

use gpui::{
    App, IntoElement, Radians, RenderOnce, SharedString, StyleRefinement, Styled, Transformation,
    Window, prelude::FluentBuilder as _, svg,
};
use gpui_base::StyledExt as _;

pub use gpui_kit_assets::{IconName, IconNamed};

#[derive(IntoElement)]
pub struct Icon {
    path: SharedString,
    style: StyleRefinement,
    transformation: Option<Transformation>,
}

impl Icon {
    pub fn new(icon: impl IconNamed) -> Self {
        Self::path(icon.path())
    }

    /// 应用资源里的 svg，如 `smelt-icons/trash.svg`、插件登记的图标路径。
    pub fn path(path: impl Into<SharedString>) -> Self {
        Self {
            path: path.into(),
            style: StyleRefinement::default(),
            transformation: None,
        }
    }

    pub fn transform(mut self, transformation: Transformation) -> Self {
        self.transformation = Some(transformation);
        self
    }

    pub fn rotate(self, radians: impl Into<Radians>) -> Self {
        self.transform(Transformation::rotate(radians))
    }
}

impl<T: IconNamed> From<T> for Icon {
    fn from(icon: T) -> Self {
        Self::new(icon)
    }
}

impl Styled for Icon {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        // 默认跟字：字号定大小、字色定颜色，调用方写了的覆盖默认。
        let text = window.text_style();
        svg()
            .path(self.path)
            .flex_shrink_0()
            .size(text.font_size.to_pixels(window.rem_size()))
            .text_color(text.color)
            .refine_style(&self.style)
            .when_some(self.transformation, |this, transformation| {
                this.with_transformation(transformation)
            })
    }
}
