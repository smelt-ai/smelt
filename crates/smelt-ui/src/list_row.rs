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

/// 悬停底压在哪一层上。侧栏和卡片的底色不同，同一层 `list_hover` 叠上去不是同一个色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Surface {
    /// 侧栏、列表。悬停是 [`ui_theme::bg_row_hover`]。
    #[default]
    Sidebar,
    /// 卡片、弹出内容。悬停是 [`ui_theme::bg_hover`]。
    Card,
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// 桌面列表行。光标是默认箭头。
#[derive(IntoElement)]
pub struct ListRow {
    base: Stateful<Div>,
    style: StyleRefinement,
    surface: Surface,
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
            surface: Surface::Sidebar,
            selected: false,
            disabled: false,
            on_click: None,
            children: Vec::new(),
        }
    }

    /// 行画在卡片上。悬停改用压在卡片底上的那一档。
    pub fn card(mut self) -> Self {
        self.surface = Surface::Card;
        self
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
            surface,
            selected,
            disabled,
            on_click,
            children,
        } = self;
        let hover = gpui::rgb(match surface {
            Surface::Sidebar => ui_theme::bg_row_hover(),
            Surface::Card => ui_theme::bg_hover(),
        });
        let selected_fill = gpui::rgb(ui_theme::bg_selected());
        // 先吃调用点的布局，再盖箭头。这样调用点就算写了别的光标，画出来仍是箭头。
        let row = base.refine_style(&style).cursor_default();
        // 选中照组件库侧栏：底色之外，字也换成主题给选中项的字色。子元素自己没定字色的就
        // 跟着变；字重留给调用点，免得时间这类次要字也跟着加粗。
        let row = if selected {
            row.bg(selected_fill)
                .text_color(gpui::rgb(ui_theme::text_selected()))
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
            .card()
            .on_click(|_, _, _| {});
        assert!(row.is_selected());
        assert!(row.disabled);
        assert_eq!(row.surface, Surface::Card);
        assert!(row.on_click.is_some());
    }
}
