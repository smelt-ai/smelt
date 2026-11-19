//! `SmeltKeybind`：键帽（[`smelt_ui::Keybind`]），快捷键画成并排的小方块，键名按平台。
//! `new("cmd-k cmd-s")`：空格分开的是先后几步，每步的写法同快捷键设置（`cmd-shift-k`）。

use std::sync::Arc;

use gpui::{App, IntoElement, OperatingSystem, ParentElement as _, RenderOnce, Window, div};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, ConstructorDescriptor,
    MaterializeRequest, MethodDescriptor, RegistryError, anyhow, gpui,
};
use smelt_ui::Keybind;

use super::{ops, parse_keys};

const OWNER: &str = "SmeltKeybind";

#[derive(Clone)]
struct Keys(String);

#[derive(Clone, Copy)]
struct Backdrop(u32);

/// 画的时候才拿得到平台，按它解 `secondary-`。
#[derive(IntoElement)]
struct Caps {
    keys: String,
    backdrop: Option<u32>,
}

impl RenderOnce for Caps {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        // 构造时已经查过写法，这里不会失败。
        let keys = parse_keys(&self.keys, cx.platform_info().os).unwrap_or_default();
        let keybind = Keybind::new(keys);
        match self.backdrop {
            Some(color) => keybind.backdrop(color),
            None => keybind,
        }
    }
}

struct KeybindMaterializer;

impl ComponentMaterializer for KeybindMaterializer {
    fn materialize(&self, request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let keys = request
            .payload()
            .downcast_ref::<Keys>()
            .ok_or_else(|| anyhow::anyhow!("{OWNER} received an incompatible payload"))?
            .0
            .clone();
        anyhow::ensure!(
            request.children_len() == 0,
            "{OWNER} does not accept children"
        );
        let backdrop = ops::<Backdrop>(&request)
            .last()
            .map(|Backdrop(color)| *color);
        // 键帽自己不收样式，外边距这类落在外面这层。
        request.finish(div().child(Caps { keys, backdrop }))
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(KeybindMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                OWNER,
                vec![ArgumentDescriptor::new("keys", ArgumentSchema::String)],
                |args| match args {
                    [ComponentArgument::String(keys)] => parse_keys(keys, OperatingSystem::Mac)
                        .map(|_| ComponentPayload::new(Keys(keys.clone())))
                        .map_err(|error| format!("{OWNER}.new({keys:?}): {error}")),
                    _ => Err(format!("{OWNER}.new(keys) expects one string")),
                },
            )])
            .with_methods(vec![
                MethodDescriptor::new(
                    "backdrop",
                    vec![ArgumentDescriptor::new("color", ArgumentSchema::Number)],
                    |args| match args {
                        [ComponentArgument::Number(color)]
                            if color.fract() == 0. && (0. ..=f64::from(0xFF_FF_FF)).contains(color) =>
                        {
                            Ok(ComponentPayload::new(Backdrop(*color as u32)))
                        }
                        _ => Err(format!("{OWNER}.backdrop(color) expects 0xRRGGBB")),
                    },
                )
                .with_documentation(
                    "The color under the keycaps (0xRRGGBB) when they sit on a popover or card instead of the page background.",
                ),
            ])
            .with_documentation(
                "Keycaps for a shortcut such as \"cmd-k cmd-s\": space-separated steps, platform key names.",
            ),
    )?;
    Ok(())
}
