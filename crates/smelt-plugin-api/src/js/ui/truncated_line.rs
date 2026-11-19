//! `SmeltTruncatedLine`：单行文字，放不下就截短、省略号收尾（[`smelt_ui::truncated_line`]）。
//! 按整行实际排版定截断点，夹着中文标点也不会截出比框宽的一行。字体、字号、颜色沿用父元素。

use std::sync::Arc;

use gpui::{ParentElement as _, Styled as _, div};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, ConstructorDescriptor,
    MaterializeRequest, RegistryError, anyhow, gpui,
};
use smelt_ui::truncated_line::truncated_line;

const OWNER: &str = "SmeltTruncatedLine";

#[derive(Clone)]
struct Text(String);

struct TruncatedLineMaterializer;

impl ComponentMaterializer for TruncatedLineMaterializer {
    fn materialize(&self, request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let text = request
            .payload()
            .downcast_ref::<Text>()
            .ok_or_else(|| anyhow::anyhow!("{OWNER} received an incompatible payload"))?
            .0
            .clone();
        anyhow::ensure!(
            request.children_len() == 0,
            "{OWNER} takes its text in new(text), not children"
        );
        // 截短的那行本身不收样式；外面包一层占满给它的宽，宽、外边距、字色都落在这层上。
        request.finish(
            div()
                .min_w_0()
                .overflow_hidden()
                .child(truncated_line(text)),
        )
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(TruncatedLineMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                OWNER,
                vec![ArgumentDescriptor::new("text", ArgumentSchema::String)],
                |args| match args {
                    [ComponentArgument::String(text)] => {
                        Ok(ComponentPayload::new(Text(text.clone())))
                    }
                    _ => Err(format!("{OWNER}.new(text) expects one string")),
                },
            )])
            .with_documentation(
                "One line of text that ends in an ellipsis when it does not fit; inherits the parent's font and color.",
            ),
    )?;
    Ok(())
}
