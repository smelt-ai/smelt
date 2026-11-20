//! `SmeltInputState` 和 `SmeltTextInput`：单行文本框（[`smelt_ui::TextInput`]）。
//!
//! 文字放在状态里：在 `init` 里 `SmeltInputState.new(placeholder)` 建一份留在视图上，
//! `state.on("change", ...)` 订变化、`state.value()` 取字；render 里只
//! `SmeltTextInput.new(state)` 把它画出来。状态的方法和事件同 gpui-shell 自带的
//! `InputState`，另加 `focus()`、`select_all()`：行内改名、展开的搜索框要一出来就能打字，
//! gpui-shell 的那份给不了。`SmeltTextInput.on_escape(cb)` 接框里用不上的 Esc（取消改名、
//! 收起搜索）：脚本的 `on_action` 只认脚本自己登记的动作，接不住输入框往上传的 Esc。

use std::sync::Arc;

use gpui::{AppContext as _, Entity};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentDataValue, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, ConstructorDescriptor,
    MaterializeRequest, RegistryError, StateDescriptor, StateMethodDescriptor, anyhow,
};
use smelt_ui::TextInput;
use smelt_ui::field::InputState;

use super::{
    bool_method, callback_method, disabled_method, finish_leaf, flag_method, ops, string_method,
};

const STATE: &str = "SmeltInputState";
const OWNER: &str = "SmeltTextInput";

#[derive(Clone)]
enum InputOp {
    AriaLabel(String),
    Small,
    Cleanable(bool),
}

#[derive(Clone)]
struct OnEscape(ComponentArgument);

struct TextInputMaterializer;

impl ComponentMaterializer for TextInputMaterializer {
    fn materialize(&self, request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let argument = request
            .payload()
            .downcast_ref::<ComponentArgument>()
            .ok_or_else(|| anyhow::anyhow!("{OWNER} received an incompatible payload"))?;
        let state = request.with_state::<Entity<InputState>, _>(argument, Clone::clone)?;
        let mut input = TextInput::new(&state).disabled(request.disabled());
        if let Some(on_escape) = ops::<OnEscape>(&request).last() {
            let callback = request.resolve_callback(&on_escape.0)?;
            input = input.on_escape(move |window, cx| {
                callback.invoke_and_report_with(
                    "SmeltTextInput.on_escape callback failed",
                    &[],
                    window,
                    cx,
                )
            });
        }
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

/// gpui-shell 给的文字方法，加上拿焦点、全选。
fn state_methods() -> Vec<StateMethodDescriptor> {
    let mut methods = gpui_shell::input_token_state_methods();
    methods.push(StateMethodDescriptor::new::<Entity<InputState>>(
        "focus",
        "(): void",
        |state, _, window, cx| {
            state.update(cx, |state, cx| state.focus(window, cx));
            Ok(ComponentDataValue::Null)
        },
    ));
    methods.push(StateMethodDescriptor::new::<Entity<InputState>>(
        "select_all",
        "(): void",
        |state, _, window, cx| {
            state.update(cx, |state, cx| state.select_all(window, cx));
            Ok(ComponentDataValue::Null)
        },
    ));
    methods
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
        .with_methods(state_methods())
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
                disabled_method(OWNER),
                callback_method(
                    OWNER,
                    "on_escape",
                    "(cx: Context) => void",
                    "Runs on Esc when the field has nothing of its own to dismiss (a selection, composition): cancel a rename, close a search.",
                    OnEscape,
                ),
            ])
            .with_documentation("A Smelt single-line text field over a SmeltInputState."),
    )?;
    Ok(())
}
