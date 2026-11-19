//! `SmeltSelect`：下拉选择（[`smelt_ui::Select`]），写着当前值的按钮，点开一张菜单，当前那项
//! 打勾。值由脚本持有：`value(当前值)` 传进来，选了哪项由 `on_change(值)` 交回去，脚本存好
//! 再重画。
//!
//! 菜单比画出它的那次绘制活得久，`on_change` 回调按控件 id 存进 [`latest`]，点下去时取最近
//! 一次绘制的。

use std::sync::Arc;

use gpui::{
    App, ElementId, IntoElement, RenderOnce, SharedString, StyleRefinement, Styled, Window,
};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentCallback,
    ComponentCallbackArgument, ComponentDescriptor, ComponentMaterializer, ComponentPayload,
    ComponentRegistry, MaterializeRequest, MethodDescriptor, RegistryError, anyhow,
};
use smelt_ui::Select;

use super::{
    callback_method, control_id, disabled_method, id_constructor, latest, ops, string_method,
};

const OWNER: &str = "SmeltSelect";

#[derive(Clone)]
struct OptionOp {
    value: String,
    label: String,
}

#[derive(Clone)]
struct Value(String);

#[derive(Clone)]
struct Placeholder(String);

#[derive(Clone)]
struct OnChange(ComponentArgument);

#[derive(IntoElement)]
struct ScriptSelect {
    id: ElementId,
    options: Vec<(String, SharedString)>,
    value: Option<String>,
    placeholder: Option<String>,
    disabled: bool,
    on_change: Option<ComponentCallback>,
    style: StyleRefinement,
}

impl RenderOnce for ScriptSelect {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let on_change = latest(&self.id, self.on_change, window, cx);
        let mut select = Select::new(self.id)
            .options(self.options)
            .selected(self.value)
            .disabled(self.disabled)
            .on_change(move |value: &String, window, cx| {
                if let Some(callback) = on_change.read(cx).clone() {
                    callback.invoke_and_report_with(
                        "SmeltSelect.on_change callback failed",
                        &[ComponentCallbackArgument::String(value.clone())],
                        window,
                        cx,
                    );
                }
            });
        if let Some(placeholder) = self.placeholder {
            select = select.placeholder(placeholder);
        }
        *select.style() = self.style;
        select
    }
}

struct SelectMaterializer;

impl ComponentMaterializer for SelectMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        anyhow::ensure!(
            request.children_len() == 0,
            "{OWNER} takes option(value, label) entries, not children"
        );
        let options = ops::<OptionOp>(&request)
            .map(|option| {
                (
                    option.value.clone(),
                    SharedString::from(option.label.clone()),
                )
            })
            .collect::<Vec<_>>();
        for (index, (value, _)) in options.iter().enumerate() {
            anyhow::ensure!(
                !options[..index].iter().any(|(earlier, _)| earlier == value),
                "{OWNER} has two options with the value {value:?}"
            );
        }
        let on_change = ops::<OnChange>(&request)
            .last()
            .cloned()
            .map(|OnChange(callback)| request.resolve_callback(&callback))
            .transpose()?;
        Ok(ScriptSelect {
            id: control_id(OWNER, &request)?.into(),
            options,
            value: ops::<Value>(&request).last().map(|value| value.0.clone()),
            placeholder: ops::<Placeholder>(&request)
                .last()
                .map(|text| text.0.clone()),
            disabled: request.disabled(),
            on_change,
            style: request.take_style(),
        }
        .into_any_element())
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    let option = MethodDescriptor::new(
        "option",
        vec![
            ArgumentDescriptor::new("value", ArgumentSchema::String),
            ArgumentDescriptor::new("label", ArgumentSchema::String),
        ],
        |args| match args {
            [
                ComponentArgument::String(value),
                ComponentArgument::String(label),
            ] => Ok(ComponentPayload::new(OptionOp {
                value: value.clone(),
                label: label.clone(),
            })),
            _ => Err(format!("{OWNER}.option(value, label) expects two strings")),
        },
    )
    .with_documentation("Appends an option in script order; values must be unique.");
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(SelectMaterializer))
            .with_constructors(vec![id_constructor(OWNER)])
            .with_methods(vec![
                option,
                string_method(
                    OWNER,
                    "value",
                    "The current value; the button shows its label, or the placeholder when no option has it.",
                    Value,
                ),
                string_method(
                    OWNER,
                    "placeholder",
                    "What the button says when the value matches no option.",
                    Placeholder,
                ),
                disabled_method(OWNER),
                callback_method(
                    OWNER,
                    "on_change",
                    "(value: string, cx: Context) => void",
                    "Reports the value the user picked, including the current one.",
                    OnChange,
                ),
            ])
            .with_documentation(
                "A Smelt dropdown select: a button showing the current value that opens a checked menu of options.",
            ),
    )?;
    Ok(())
}
