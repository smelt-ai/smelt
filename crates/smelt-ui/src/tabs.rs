//! 页签条：工作台一组里并排的几块面板。页签是 `md` 圆角的胶囊，
//! 24 高，字 `muted`；选中的铺 `row_selected`、字换 `text`，悬停铺 `row_hover`。
//!
//! 只管样子和读屏角色（tablist / tab、选中态），点击、拖动、放下都由调用方挂在 [`Tab`]
//! 上（它是可交互元素）。页面里在几个视图间切换用分段控件（[`crate::Segmented`]），
//! 不用页签。

use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, ElementId, InteractiveElement, Interactivity,
    IntoElement, ParentElement, RenderOnce, StatefulInteractiveElement, StyleRefinement, Styled,
    Window, prelude::FluentBuilder as _, px, rgb,
};
use gpui_base::StyledExt as _;

use crate::kit::{RADIUS, TYPE};
use crate::ui_theme;

/// 一排页签，`role=tablist`。
#[derive(IntoElement)]
pub struct TabBar {
    base: gpui_base::Tabs,
}

impl TabBar {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: gpui_base::Tabs::new(id),
        }
    }
}

impl ParentElement for TabBar {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

impl Styled for TabBar {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl RenderOnce for TabBar {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        // 调用方的样式和默认样式写在同一份里：先取出调用方的，写完默认再叠回去，调用方的赢。
        let mut base = self.base;
        let custom = std::mem::take(base.style());
        base.flex()
            .flex_row()
            .items_center()
            .gap(px(2.))
            .refine_style(&custom)
    }
}

/// 一个页签，`role=tab`。
#[derive(IntoElement)]
pub struct Tab {
    base: gpui_base::Tab,
    selected: bool,
}

impl Tab {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: gpui_base::Tab::new(id),
            selected: false,
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.base = self.base.on_click(handler);
        self
    }

    /// 读屏用的「第几个，共几个」，从 1 数。
    pub fn set_position(mut self, position: usize, size: usize) -> Self {
        self.base = self.base.set_position(position, size);
        self
    }
}

impl ParentElement for Tab {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

impl Styled for Tab {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for Tab {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl StatefulInteractiveElement for Tab {}

impl RenderOnce for Tab {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let selected = self.selected;
        let mut base = self.base;
        let custom = std::mem::take(base.style());
        base.selected(selected)
            .flex_none()
            .h(ui_theme::height::xs())
            .px(px(8.))
            .gap(px(4.))
            .rounded(px(RADIUS.md))
            .map(|this| TYPE.ui.apply(this))
            .whitespace_nowrap()
            .text_color(rgb(if selected {
                ui_theme::text()
            } else {
                ui_theme::muted()
            }))
            .map(|this| {
                if selected {
                    this.bg(ui_theme::row_selected())
                } else {
                    this.hover(|style| style.bg(ui_theme::row_hover()))
                }
            })
            // 页签条是一个 Tab 停留点，左右键在页签间移（gpui-base 的 `Tabs`）。键盘焦点画在
            // 边内：页签条套在横向滚动区里，画在外面会被裁掉。
            .focus_visible(|style| style.shadow(focus_ring()))
            .refine_style(&custom)
    }
}

fn focus_ring() -> Vec<BoxShadow> {
    vec![
        BoxShadow::new(px(0.), px(0.), ui_theme::focus().into())
            .spread_radius(px(2.))
            .inset(),
    ]
}
