//! `SmeltTooltip`：给子元素挂悬停提示（[`smelt_ui::tooltip::Tooltip`]），smelt 自己的紧凑样式，
//! 能带快捷键键帽。gpui-shell 元素自带的 `tooltip(text)` 在 App 里不弹：它要窗口里的 `ShellRoot`
//! 提示层，工作台窗口没有包它，找不到就静默不画。普通元素要提示一律用这个；smelt-ui 的按钮、
//! 列表行自带的 `tooltip` 是 smelt 自己的，照用。
//!
//! 键帽两种给法：`keys` 写死一串按键；`command` 给一条命令，弹出时按眼下的键位表查它的快捷键
//! （用户在设置里改过就是改后的），键位表只有宿主看得见。

use std::sync::Arc;

use gpui::{InteractiveElement as _, OperatingSystem, StatefulInteractiveElement as _, div};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, ConstructorDescriptor,
    MaterializeRequest, MethodDescriptor, RegistryError, anyhow, gpui,
};
use smelt_ui::tooltip::Tooltip;

use super::{ops, parse_keys};
use crate::RunCommand;

const OWNER: &str = "SmeltTooltip";

#[derive(Clone)]
struct Spec {
    id: String,
    text: String,
}

/// 键帽：`keys` 和 `command` 后给的算数。
#[derive(Clone)]
enum Shortcut {
    Keys(String),
    Command(String),
}

struct TooltipMaterializer;

impl ComponentMaterializer for TooltipMaterializer {
    fn materialize(&self, request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let Spec { id, text } = request
            .payload()
            .downcast_ref::<Spec>()
            .ok_or_else(|| anyhow::anyhow!("{OWNER} received an incompatible payload"))?
            .clone();
        let shortcut = ops::<Shortcut>(&request).last().cloned();
        let anchor = div().id(id).tooltip(move |window, cx| {
            let tooltip = Tooltip::new(text.clone());
            match &shortcut {
                Some(Shortcut::Keys(keys)) => {
                    tooltip.key_binding(parse_keys(keys, cx.platform_info().os).ok())
                }
                Some(Shortcut::Command(command)) => {
                    tooltip.action(&RunCommand::new(command.clone()), None)
                }
                None => tooltip,
            }
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
                            .map(|_| ComponentPayload::new(Shortcut::Keys(keys.clone())))
                            .map_err(|error| format!("{OWNER}.keys({keys:?}): {error}")),
                        _ => Err(format!("{OWNER}.keys(keys) expects one string")),
                    },
                )
                .with_documentation(
                    "Shows a shortcut such as \"cmd-k\" after the text; space-separated steps.",
                ),
                MethodDescriptor::new(
                    "command",
                    vec![ArgumentDescriptor::new("command", ArgumentSchema::String)],
                    |args| match args {
                        [ComponentArgument::String(command)] if !command.is_empty() => Ok(
                            ComponentPayload::new(Shortcut::Command(command.clone())),
                        ),
                        _ => Err(format!(
                            "{OWNER}.command(command) expects a non-empty command id"
                        )),
                    },
                )
                .with_documentation(
                    "Shows the shortcut the user has bound to this command (\"<plugin id>/<name>\", run without arguments) after the text, looked up when the tooltip opens; nothing when it has none. The later of keys and command wins.",
                ),
            ])
            .with_documentation(
                "Wraps its children and shows a Smelt tooltip when the pointer rests on them. Use this instead of an element's own `tooltip(text)`, which never shows in Smelt's windows; the `tooltip` of Smelt's own components (buttons, list rows) works.",
            ),
    )?;
    Ok(())
}
