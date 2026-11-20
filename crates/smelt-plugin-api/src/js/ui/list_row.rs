//! `SmeltListRow`：可点的列表行（[`smelt_ui::ListRow`]）。悬停底、选中底、禁用和光标都归它，
//! 脚本只写行里的布局和内容，不自己铺悬停底。

use std::sync::Arc;

use gpui_shell::{
    ComponentDescriptor, ComponentMaterializer, ComponentRegistry, MaterializeRequest,
    RegistryError, anyhow,
    gpui::{self, InteractiveElement as _, SharedString, StatefulInteractiveElement as _},
};
use smelt_ui::{ListRow, tooltip::Tooltip};

use super::{
    callback_method, control_id, disabled_method, id_constructor, ops, selected_method,
    string_method,
};

const OWNER: &str = "SmeltListRow";

/// `group(name)`：把这一行起成悬停组，行里元素的 `group_hover(name, ...)` 跟着整行悬停变。
struct Group(String);

/// `tooltip(text)`：悬停整行时的说明，如窄侧栏里被截断的完整标题。
struct TooltipText(String);

struct ListRowMaterializer;

impl ComponentMaterializer for ListRowMaterializer {
    fn materialize(&self, request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let mut row = ListRow::new(control_id(OWNER, &request)?)
            .selected(request.selected())
            .disabled(request.disabled());
        if let Some(Group(name)) = ops::<Group>(&request).last() {
            row = row.group(name.clone());
        }
        if let Some(TooltipText(text)) = ops::<TooltipText>(&request).last() {
            let text = SharedString::from(text.clone());
            row = row.tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx));
        }
        if let Some(callback) = request.on_click() {
            row = row.on_click(move |event, window, cx| callback.invoke(event, window, cx));
        }
        request.finish(row)
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(ListRowMaterializer))
            .with_constructors(vec![id_constructor(OWNER)])
            .with_methods(vec![
                selected_method(OWNER),
                disabled_method(OWNER),
                string_method(
                    OWNER,
                    "group",
                    "Names the row as a hover group, so group_hover(name, declare) on elements inside it follows the whole row.",
                    Group,
                ),
                string_method(
                    OWNER,
                    "tooltip",
                    "Shows text while the pointer rests on the row, such as its full title when truncated.",
                    TooltipText,
                ),
                // `on_click` 由 Shell 自己读（`MaterializeRequest::on_click`）。
                callback_method(
                    OWNER,
                    "on_click",
                    "(event: ClickEvent, cx: Context) => void",
                    "Invokes the callback when the row is clicked.",
                    |_| (),
                ),
            ])
            .with_documentation(
                "A Smelt list row with hover, selected and disabled backgrounds; children are its content.",
            ),
    )?;
    Ok(())
}
