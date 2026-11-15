//! 可点击的列表行和折叠头。
//!
//! 悬停底、选中底、禁用和箭头光标都在这里。调用点只写布局和内容。
//! 不要再要手型光标，也不要自己铺悬停底——禁用时这里会把点击和悬停一起拿掉。
//!
//! 不套 gpui-component 的 `ListItem`：它自带内边距、字号和勾选槽，悬停/选中用的是
//! 库的 `list_hover` / `list_active`，套到现有行上会改掉几何和颜色。

use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, Stateful, StatefulInteractiveElement, StyleRefinement, Styled, Window, div,
};
use gpui_component::{Disableable, Selectable, StyledExt as _};

use crate::ui_theme;

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// 桌面列表行。光标是默认箭头。
#[derive(IntoElement)]
pub struct ListRow {
    base: Stateful<Div>,
    style: StyleRefinement,
    selected: bool,
    disabled: bool,
    on_click: Option<ClickHandler>,
    children: Vec<AnyElement>,
}

impl ListRow {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: div().id(id),
            style: StyleRefinement::default(),
            selected: false,
            disabled: false,
            on_click: None,
            children: Vec::new(),
        }
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    pub fn selected(self, selected: bool) -> Self {
        Selectable::selected(self, selected)
    }

    pub fn disabled(self, disabled: bool) -> Self {
        Disableable::disabled(self, disabled)
    }
}

impl Disableable for ListRow {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl Selectable for ListRow {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl Styled for ListRow {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for ListRow {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl InteractiveElement for ListRow {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.base.interactivity()
    }
}

impl StatefulInteractiveElement for ListRow {}

impl RenderOnce for ListRow {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let ListRow {
            base,
            style,
            selected,
            disabled,
            on_click,
            children,
        } = self;
        // `soft` 是实色，侧栏和卡片上悬停是同一档，不分底层。
        let hover = gpui::rgb(ui_theme::soft());
        let selected_fill = gpui::rgb(ui_theme::soft_hover());
        // 先吃调用点的布局，再盖箭头。这样调用点就算写了别的光标，画出来仍是箭头。
        let row = base.refine_style(&style).cursor_default();
        // 选中照组件库侧栏：底色之外，字也换成主题给选中项的字色。子元素自己没定字色的就
        // 跟着变；字重留给调用点，免得时间这类次要字也跟着加粗。
        let row = if selected {
            row.bg(selected_fill)
                .text_color(gpui::rgb(ui_theme::text()))
        } else {
            row
        };
        let row = if disabled || selected {
            row
        } else {
            row.hover(move |row| row.bg(hover))
        };
        let row = if disabled {
            row
        } else if let Some(on_click) = on_click {
            row.on_click(on_click)
        } else {
            row
        };
        row.children(children)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_and_disabled_are_recorded_for_render() {
        let row = ListRow::new("row")
            .selected(true)
            .disabled(true)
            .on_click(|_, _, _| {});
        assert!(row.is_selected());
        assert!(row.disabled);
        assert!(row.on_click.is_some());
    }
}
