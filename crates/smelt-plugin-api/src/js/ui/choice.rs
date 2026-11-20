//! `SmeltCheckbox`、`SmeltRadio`：[`smelt_ui::Checkbox`]、[`smelt_ui::Radio`]。都是受控的：
//! 勾没勾由脚本经 `checked(...)` 给，回调里改了自己的状态再重画。
//!
//! 单选没有「组」：一组就是几个 `SmeltRadio` 排在一起，各自 `checked` 和 `on_select`。

use std::sync::Arc;

use gpui_shell::{
    ComponentArgument, ComponentCallbackArgument, ComponentDescriptor, ComponentMaterializer,
    ComponentRegistry, MaterializeRequest, RegistryError, anyhow, gpui,
};
use smelt_ui::{Checkbox, Radio};

use super::{
    bool_method, callback_method, control_id, disabled_method, finish_leaf, id_constructor, ops,
    string_method,
};

const CHECKBOX: &str = "SmeltCheckbox";
const RADIO: &str = "SmeltRadio";

#[derive(Clone)]
enum ChoiceOp {
    Label(String),
    AccessibilityLabel(String),
    Checked(bool),
    Changed(ComponentArgument),
}

struct CheckboxMaterializer;

impl ComponentMaterializer for CheckboxMaterializer {
    fn materialize(&self, request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let mut checkbox =
            Checkbox::new(control_id(CHECKBOX, &request)?).disabled(request.disabled());
        for op in ops::<ChoiceOp>(&request) {
            checkbox = match op {
                ChoiceOp::Label(label) => checkbox.label(label.clone()),
                ChoiceOp::AccessibilityLabel(label) => checkbox.accessibility_label(label.clone()),
                ChoiceOp::Checked(checked) => checkbox.checked(*checked),
                ChoiceOp::Changed(callback) => {
                    let callback = request.resolve_callback(callback)?;
                    checkbox.on_change(move |checked, window, cx| {
                        callback.invoke_and_report_with(
                            "SmeltCheckbox.on_change callback failed",
                            &[ComponentCallbackArgument::Boolean(checked)],
                            window,
                            cx,
                        )
                    })
                }
            };
        }
        finish_leaf(CHECKBOX, request, checkbox)
    }
}

struct RadioMaterializer;

impl ComponentMaterializer for RadioMaterializer {
    fn materialize(&self, request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let mut radio = Radio::new(control_id(RADIO, &request)?).disabled(request.disabled());
        for op in ops::<ChoiceOp>(&request) {
            radio = match op {
                ChoiceOp::Label(label) => radio.label(label.clone()),
                ChoiceOp::AccessibilityLabel(label) => radio.accessibility_label(label.clone()),
                ChoiceOp::Checked(checked) => radio.checked(*checked),
                ChoiceOp::Changed(callback) => {
                    let callback = request.resolve_callback(callback)?;
                    radio.on_select(move |window, cx| {
                        callback.invoke_and_report_with(
                            "SmeltRadio.on_select callback failed",
                            &[],
                            window,
                            cx,
                        )
                    })
                }
            };
        }
        finish_leaf(RADIO, request, radio)
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    let common = |owner: &'static str| {
        vec![
            string_method(
                owner,
                "label",
                "Sets the visible label, which is also the accessible name; clicking it toggles the control.",
                ChoiceOp::Label,
            ),
            string_method(
                owner,
                "accessibility_label",
                "Names the control for screen readers when it has no visible label, such as a checkbox at the head of a row.",
                ChoiceOp::AccessibilityLabel,
            ),
            bool_method(
                owner,
                "checked",
                "Sets whether the control is checked; the script owns this state.",
                ChoiceOp::Checked,
            ),
            disabled_method(owner),
        ]
    };
    registry.register(
        ComponentDescriptor::new(CHECKBOX, Arc::new(CheckboxMaterializer))
            .with_constructors(vec![id_constructor(CHECKBOX)])
            .with_methods(
                [
                    common(CHECKBOX),
                    vec![callback_method(
                        CHECKBOX,
                        "on_change",
                        "(checked: boolean, cx: Context) => void",
                        "Receives the toggled value; store it and pass it back through checked().",
                        ChoiceOp::Changed,
                    )],
                ]
                .concat(),
            )
            .with_documentation("A Smelt checkbox: one independent on/off choice."),
    )?;
    registry.register(
        ComponentDescriptor::new(RADIO, Arc::new(RadioMaterializer))
            .with_constructors(vec![id_constructor(RADIO)])
            .with_methods(
                [
                    common(RADIO),
                    vec![callback_method(
                        RADIO,
                        "on_select",
                        "(cx: Context) => void",
                        "Runs when the user picks this option; not called for the option already checked.",
                        ChoiceOp::Changed,
                    )],
                ]
                .concat(),
            )
            .with_documentation(
                "A Smelt radio option; a group is several radios side by side, one of them checked.",
            ),
    )?;
    Ok(())
}
