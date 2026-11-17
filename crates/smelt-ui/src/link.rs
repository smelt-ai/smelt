//! 文字链接：外部网页（文档、反馈、源码）。`link` 色（默认就是正文色）加下划线、手型指针，
//! 点了用系统浏览器打开。不用蓝：蓝只表示在跑。应用内的命令用按钮，不用链接。
//!
//! 角色、焦点、键盘激活、禁用用 gpui-base 的 `Link`。

use gpui::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    SharedString, StyleRefinement, Styled, Window, px, rgb,
};
use gpui_base::StyledExt as _;

use crate::kit::RADIUS;
use crate::ui_theme;

#[derive(IntoElement)]
pub struct Link {
    id: ElementId,
    href: SharedString,
    style: StyleRefinement,
    children: Vec<AnyElement>,
}

impl Link {
    pub fn new(id: impl Into<ElementId>, href: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            href: href.into(),
            style: StyleRefinement::default(),
            children: Vec::new(),
        }
    }
}

impl ParentElement for Link {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for Link {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Link {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let focus = crate::button::focus_ring();
        gpui_base::Link::new(self.id)
            .href(self.href)
            .open_with(|href, _, _, cx| cx.open_url(href))
            .text_color(rgb(ui_theme::link()))
            .underline()
            .cursor_pointer()
            .rounded(px(RADIUS.xs))
            .hover(|style| style.opacity(0.75))
            .focus_visible(move |style| style.shadow(focus))
            .children(self.children)
            .refine_style(&self.style)
    }
}
