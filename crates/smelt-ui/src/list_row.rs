//! 可点击的列表行和折叠头。
//!
//! 悬停底、选中底、字色、禁用和箭头光标都在这里。调用点只写布局和内容。
//! 不要再要手型光标，也不要自己铺悬停底——禁用时这里会把点击和悬停一起拿掉。
//!
//! 行里的名字（项目、会话、视图入口）用正文色，不管选没选中、悬停没有：名字是要读的
//! 东西，平时压成灰字等悬停或选中才提亮，整栏就只剩一行看得清（压在毛玻璃上更糊）。
//! 选中和悬停只换底；选中再由调用点给名字加中等字重，跟悬停分开。选中底比悬停深一档，
//! 压在窗口底上保底到一眼认得出（`ui_theme::row_selected`）。时间、分支、数量这类附属信息
//! 由调用点标 `ui_theme::row_muted(selected)` / `row_faint(selected)`：选中行里的次要字压在
//! 选中底上，要换成按选中底配过的那档，不然选中的那行反而最难读。名字不要再按选中、悬停
//! 换字色。
//!
//! 不套 gpui-component 的 `ListItem`：它自带内边距、字号和勾选槽，悬停/选中直接铺主题
//! 的 `list_hover` / `list_active` 实色，压在毛玻璃上不透，也不按材质保底。

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
        // 半透明叠色：底透出窗口材质时行底跟着透；深浅按字底那层窗口底保过底。
        let hover = ui_theme::row_hover();
        let selected_fill = ui_theme::row_selected();
        // 名字的字色先铺正文色，再吃调用点的布局：整行都是附属信息的（比如「已隐藏 N 个」）
        // 调用点在行上写的字色盖得过它。最后盖箭头，调用点写了别的光标画出来也是箭头。
        let row = base
            .text_color(gpui::rgb(ui_theme::text()))
            .refine_style(&style)
            .cursor_default();
        // 选中只换底。字重留给调用点加在名字上，免得时间这类附属信息也跟着加粗。
        let row = if selected { row.bg(selected_fill) } else { row };
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
