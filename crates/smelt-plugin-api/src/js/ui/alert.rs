//! `SmeltAlert`：页面里一段要人看到的说明（[`smelt_ui::Alert`]）。默认是出错，`need()`
//! 换成「做之前要你知道的后果」。「完成」这类结果不用它，一行弱色的字就够。

use std::sync::Arc;

use gpui::{IntoElement as _, ParentElement as _, Refineable as _, Styled as _};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, ConstructorDescriptor,
    MaterializeRequest, RegistryError, anyhow,
};
use smelt_ui::Alert;

use super::{flag_method, ops};

const OWNER: &str = "SmeltAlert";

#[derive(Clone)]
struct AlertPayload {
    id: String,
    message: String,
}

#[derive(Clone)]
struct Need;

struct AlertMaterializer;

impl ComponentMaterializer for AlertMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        anyhow::ensure!(
            request.children_len() == 0,
            "{OWNER} shows its message only and does not accept children"
        );
        let AlertPayload { id, message } = request
            .payload()
            .downcast_ref::<AlertPayload>()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{OWNER} received an incompatible payload"))?;
        let alert = if ops::<Need>(&request).next().is_some() {
            Alert::need(id, message)
        } else {
            Alert::error(id, message)
        };
        // 提示块本身不开放样式，套一层放脚本给的布局样式。
        let mut wrapper = gpui::div().child(alert);
        wrapper.style().refine(&request.take_style());
        Ok(wrapper.into_any_element())
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(AlertMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                OWNER,
                vec![
                    ArgumentDescriptor::new("id", ArgumentSchema::String),
                    ArgumentDescriptor::new("message", ArgumentSchema::String),
                ],
                |args| match args {
                    [ComponentArgument::String(id), ComponentArgument::String(message)]
                        if !id.is_empty() =>
                    {
                        Ok(ComponentPayload::new(AlertPayload {
                            id: id.clone(),
                            message: message.clone(),
                        }))
                    }
                    [ComponentArgument::String(_), ComponentArgument::String(_)] => {
                        Err(format!("{OWNER} id must not be empty"))
                    }
                    _ => Err(format!("{OWNER}(id, message) expects two strings")),
                },
            )])
            .with_methods(vec![flag_method(
                "need",
                Need,
                "Marks a consequence to know before acting instead of an error.",
            )])
            .with_documentation(
                "A Smelt inline alert announced to screen readers; an error unless need() is called.",
            ),
    )?;
    Ok(())
}
