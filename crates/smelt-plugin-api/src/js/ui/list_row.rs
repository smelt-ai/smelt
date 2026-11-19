//! `SmeltListRow`：可点的列表行（[`smelt_ui::ListRow`]）。悬停底、选中底、禁用和光标都归它，
//! 脚本只写行里的布局和内容，不自己铺悬停底。

use std::sync::Arc;

use gpui_shell::{
    ComponentDescriptor, ComponentMaterializer, ComponentRegistry, MaterializeRequest,
    RegistryError, anyhow, gpui,
};
use smelt_ui::ListRow;

use super::{callback_method, control_id, disabled_method, id_constructor, selected_method};

const OWNER: &str = "SmeltListRow";

struct ListRowMaterializer;

impl ComponentMaterializer for ListRowMaterializer {
    fn materialize(&self, request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let mut row = ListRow::new(control_id(OWNER, &request)?)
            .selected(request.selected())
            .disabled(request.disabled());
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
