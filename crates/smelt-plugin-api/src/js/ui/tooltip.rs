//! `SmeltTooltip`：给子元素挂悬停提示（[`smelt_ui::tooltip::Tooltip`]），smelt 自己的紧凑样式，
//! 能带快捷键键帽。gpui-shell 元素自带的 `tooltip(text)` 是 Shell 的样式，和 App 里别处不一样。

use std::sync::Arc;

use gpui::{InteractiveElement as _, OperatingSystem, StatefulInteractiveElement as _, div};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, ConstructorDescriptor,
    MaterializeRequest, MethodDescriptor, RegistryError, anyhow, gpui,
};
use smelt_ui::tooltip::Tooltip;

use super::{ops, parse_keys};

const OWNER: &str = "SmeltTooltip";

#[derive(Clone)]
struct Spec {
    id: String,
    text: String,
}

#[derive(Clone)]
struct Keys(String);

struct TooltipMaterializer;

impl ComponentMaterializer for TooltipMaterializer {
    fn materialize(&self, request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let Spec { id, text } = request
            .payload()
            .downcast_ref::<Spec>()
            .ok_or_else(|| anyhow::anyhow!("{OWNER} received an incompatible payload"))?
            .clone();
        let keys = ops::<Keys>(&request).last().map(|keys| keys.0.clone());
        let anchor = div().id(id).tooltip(move |window, cx| {
            let keys = keys
                .as_deref()
                .and_then(|keys| parse_keys(keys, cx.platform_info().os).ok());
            Tooltip::new(text.clone())
                .key_binding(keys)
                .build(window, cx)
        });
        request.finish(anchor)
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(TooltipMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                OWNER,
                vec![
                    ArgumentDescriptor::new("id", ArgumentSchema::String),
                    ArgumentDescriptor::new("text", ArgumentSchema::String),
                ],
                |args| match args {
                    [
                        ComponentArgument::String(id),
                        ComponentArgument::String(text),
                    ] if !id.is_empty() => Ok(ComponentPayload::new(Spec {
                        id: id.clone(),
                        text: text.clone(),
                    })),
                    _ => Err(format!(
                        "{OWNER}.new(id, text) expects a non-empty id and a string"
                    )),
                },
            )])
            .with_methods(vec![
                MethodDescriptor::new(
                    "keys",
                    vec![ArgumentDescriptor::new("keys", ArgumentSchema::String)],
                    |args| match args {
                        [ComponentArgument::String(keys)] => parse_keys(keys, OperatingSystem::Mac)
                            .map(|_| ComponentPayload::new(Keys(keys.clone())))
                            .map_err(|error| format!("{OWNER}.keys({keys:?}): {error}")),
                        _ => Err(format!("{OWNER}.keys(keys) expects one string")),
                    },
                )
                .with_documentation(
                    "Shows a shortcut such as \"cmd-k\" after the text; space-separated steps.",
                ),
            ])
            .with_documentation(
                "Wraps its children and shows a Smelt tooltip when the pointer rests on them.",
            ),
    )?;
    Ok(())
}
