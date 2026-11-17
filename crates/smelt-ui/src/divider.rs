//! 分隔线：1px `hair`。默认横向铺满宽，竖向铺满高，长度用普通
//! 样式改（`.h(px(14.))`）。

use gpui::{App, Div, IntoElement, RenderOnce, StyleRefinement, Styled, Window, div, px};
use gpui_base::StyledExt as _;

use crate::ui_theme;

#[derive(IntoElement)]
pub struct Divider {
    vertical: bool,
    style: StyleRefinement,
}

impl Divider {
    pub fn horizontal() -> Self {
        Self {
            vertical: false,
            style: StyleRefinement::default(),
        }
    }

    pub fn vertical() -> Self {
        Self {
            vertical: true,
            style: StyleRefinement::default(),
        }
    }
}

impl Styled for Divider {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Divider {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let line: Div = div().flex_none().bg(ui_theme::hair());
        if self.vertical {
            line.w(px(1.)).h_full()
        } else {
            line.h(px(1.)).w_full()
        }
        .refine_style(&self.style)
    }
}
