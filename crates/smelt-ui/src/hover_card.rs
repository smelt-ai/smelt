//! 悬停卡片：指针在触发点上停一会儿浮出的详情，移开一会儿收起。只是快捷方式，同样的
//! 内容要另有点开或展开的路，键盘也够得着。
//!
//! 行为（开关延时、指针移进卡片不收）用 gpui-base 的 `HoverCard`。外观：
//! 和 [`crate::popover::Popover`] 同一块浮层底，8 圆角、4 内边距，离触发点 4px。
//! `Styled` 改的是浮层卡片。

use gpui::{
    Anchor, AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ParentElement,
    RenderOnce, StyleRefinement, Styled, Window, prelude::FluentBuilder as _, px,
};
use gpui_base::StyledExt as _;

use crate::kit::RADIUS;
use crate::popover::{GAP, surface};

/// 卡片内边距。
const PADDING: f32 = 4.;

/// 悬停浮出的卡片，子元素是卡片内容。默认开在触发点下方居中（[`Anchor::TopCenter`]）。
#[derive(IntoElement)]
pub struct HoverCard {
    base: gpui_base::HoverCard,
    anchor: Anchor,
    style: StyleRefinement,
    children: Vec<AnyElement>,
}

impl HoverCard {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: gpui_base::HoverCard::new(id),
            anchor: Anchor::TopCenter,
            style: StyleRefinement::default(),
            children: Vec::new(),
        }
    }

    /// 卡片的哪个角对着触发点，默认 [`Anchor::TopCenter`]。
    pub fn anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = anchor;
        self
    }

    pub fn trigger(mut self, trigger: impl IntoElement) -> Self {
        self.base = self.base.trigger(trigger);
        self
    }

    /// 停多久才浮出，默认 0.6 秒。
    pub fn open_delay(mut self, delay: std::time::Duration) -> Self {
        self.base = self.base.open_delay(delay);
        self
    }
}

impl Styled for HoverCard {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for HoverCard {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for HoverCard {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let anchor = self.anchor;
        let style = self.style;
        let children = self.children;
        // gpui-base 的悬停卡片不留间距，卡片朝外推开 4px。
        let below = matches!(
            anchor,
            Anchor::TopLeft | Anchor::TopCenter | Anchor::TopRight
        );
        self.base.anchor(anchor).content(move |_, _, _| {
            surface()
                .id("smelt-hover-card")
                .occlude()
                .rounded(px(RADIUS.lg))
                .p(px(PADDING))
                .map(|card| {
                    if below {
                        card.top(px(GAP))
                    } else {
                        card.bottom(px(GAP))
                    }
                })
                .refine_style(&style)
                .children(children)
        })
    }
}
