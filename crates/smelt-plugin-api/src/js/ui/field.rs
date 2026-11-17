//! `SmeltInputState` 和 `SmeltTextInput`：单行文本框（[`smelt_ui::TextInput`]）。
//!
//! 文字放在状态里：在 `init` 里 `SmeltInputState.new(placeholder)` 建一份留在视图上，
//! `state.on("change", ...)` 订变化、`state.value()` 取字；render 里只
//! `SmeltTextInput.new(state)` 把它画出来。状态的方法和事件同 gpui-shell 自带的
//! `InputState`。

use std::sync::Arc;

use gpui::{AppContext as _, Entity};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, ConstructorDescriptor,
    MaterializeRequest, RegistryError, StateDescriptor, anyhow,
};
use smelt_ui::TextInput;
use smelt_ui::field::InputState;

use super::{bool_method, finish_leaf, flag_method, ops, string_method};

const STATE: &str = "SmeltInputState";
const OWNER: &str = "SmeltTextInput";

#[derive(Clone)]
enum InputOp {
    AriaLabel(String),
    Small,
    Cleanable(bool),
}

struct TextInputMaterializer;

impl ComponentMaterializer for TextInputMaterializer {
    fn materialize(&self, request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let argument = request
            .payload()
            .downcast_ref::<ComponentArgument>()
            .ok_or_else(|| anyhow::anyhow!("{OWNER} received an incompatible payload"))?;
        let state = request.with_state::<Entity<InputState>, _>(argument, Clone::clone)?;
        let mut input = TextInput::new(&state);
        for op in ops::<InputOp>(&request) {
            input = match op {
                InputOp::AriaLabel(label) => input.aria_label(label.clone()),
                InputOp::Small => input.small(),
                InputOp::Cleanable(cleanable) => input.cleanable(*cleanable),
            };
        }
        finish_leaf(OWNER, request, input)
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register_state(
        StateDescriptor::new(
            STATE,
            STATE,
            vec![ArgumentDescriptor::new(
                "placeholder",
                ArgumentSchema::Optional(Box::new(ArgumentSchema::String)),
            )],
            |args, window, cx| {
                let placeholder = match args {
                    [ComponentArgument::Optional(None)] => String::new(),
                    [ComponentArgument::Optional(Some(value))] => match value.as_ref() {
                        ComponentArgument::String(text) => text.clone(),
                        _ => return Err(format!("{STATE} placeholder expects text")),
                    },
                    _ => return Err(format!("{STATE}(placeholder?) expects optional text")),
                };
                Ok(Box::new(
                    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder)),
                ))
            },
        )
        .with_methods(gpui_shell::input_token_state_methods())
        .with_input_events::<gpui_base::input::InputMode>()
        .with_documentation(
            "Retained single-line text, created in init() and drawn by SmeltTextInput; read it with value() and subscribe with on(\"change\", ...).",
        ),
    )?;
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(TextInputMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                OWNER,
                vec![ArgumentDescriptor::new(
                    "state",
                    ArgumentSchema::Entity(STATE),
                )],
                |args| Ok(ComponentPayload::new(args[0].clone())),
            )])
            .with_methods(vec![
                string_method(
                    OWNER,
                    "aria_label",
                    "Sets the accessible name; required when no visible title or placeholder names the field.",
                    InputOp::AriaLabel,
                ),
                flag_method(
                    "small",
                    InputOp::Small,
                    "Uses the compact size, for toolbars and filters.",
                ),
                bool_method(
                    OWNER,
                    "cleanable",
                    "Shows a clear button at the end while the field has text; for filters.",
                    InputOp::Cleanable,
                ),
            ])
            .with_documentation("A Smelt single-line text field over a SmeltInputState."),
    )?;
    Ok(())
}
