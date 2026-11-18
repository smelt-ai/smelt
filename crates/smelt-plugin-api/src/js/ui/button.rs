//! `SmeltButton`：[`smelt_ui::Button`]。按钮的变体、尺寸和状态也给菜单按钮
//! （[`super::menu`]）、确认按钮（[`super::confirm`]）用。

use std::sync::Arc;

use gpui_shell::{
    ComponentDescriptor, ComponentMaterializer, ComponentRegistry, MaterializeRequest,
    MethodDescriptor, RegistryError, anyhow, gpui,
};
use smelt_ui::button::Button;

use super::{
    bool_method, control_id, disabled_method, flag_method, id_constructor, ops, string_method,
};

const OWNER: &str = "SmeltButton";

#[derive(Clone)]
pub(super) enum ButtonOp {
    Label(String),
    AccessibilityLabel(String),
    Tooltip(String),
    Loading(bool),
    Primary,
    Secondary,
    Quiet,
    Danger,
    DangerQuiet,
    Small,
    Large,
}

/// 按钮的外观和状态方法，不含点击：点了做什么各控件自己定。
pub(super) fn methods(owner: &'static str) -> Vec<MethodDescriptor> {
    vec![
        string_method(
            owner,
            "label",
            "Sets the visible button label.",
            ButtonOp::Label,
        ),
        string_method(
            owner,
            "accessibility_label",
            "Sets the accessible name independently of the visible button label; required for icon buttons.",
            ButtonOp::AccessibilityLabel,
        ),
        string_method(
            owner,
            "tooltip",
            "Sets hover help and an accessible description, not the accessible name.",
            ButtonOp::Tooltip,
        ),
        flag_method("primary", ButtonOp::Primary, "Uses the primary role."),
        flag_method("secondary", ButtonOp::Secondary, "Uses the secondary role."),
        flag_method("quiet", ButtonOp::Quiet, "Uses the quiet role."),
        flag_method("danger", ButtonOp::Danger, "Uses the danger role."),
        flag_method(
            "danger_quiet",
            ButtonOp::DangerQuiet,
            "Uses the quiet danger role.",
        ),
        flag_method("small", ButtonOp::Small, "Uses the small size."),
        flag_method("large", ButtonOp::Large, "Uses the large size."),
        bool_method(
            owner,
            "loading",
            "Shows a spinner and prevents interaction while loading.",
            ButtonOp::Loading,
        ),
        disabled_method(owner),
    ]
}

/// 按脚本记下的操作搭出按钮，样式、子元素和点击留给调用方。
pub(super) fn build(owner: &str, request: &MaterializeRequest<'_>) -> anyhow::Result<Button> {
    let mut button = Button::new(control_id(owner, request)?).disabled(request.disabled());
    for op in ops::<ButtonOp>(request) {
        button = match op {
            ButtonOp::Label(label) => button.label(label.clone()),
            ButtonOp::AccessibilityLabel(label) => button.accessibility_label(label.clone()),
            ButtonOp::Tooltip(tooltip) => button.tooltip(tooltip.clone()),
            ButtonOp::Loading(loading) => button.loading(*loading),
            ButtonOp::Primary => button.primary(),
            ButtonOp::Secondary => button.secondary(),
            ButtonOp::Quiet => button.quiet(),
            ButtonOp::Danger => button.danger(),
            ButtonOp::DangerQuiet => button.danger_quiet(),
            ButtonOp::Small => button.small(),
            ButtonOp::Large => button.large(),
        };
    }
    Ok(button)
}

pub(super) struct ButtonMaterializer;

impl ComponentMaterializer for ButtonMaterializer {
    fn materialize(&self, request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let mut button = build(OWNER, &request)?;
        if let Some(callback) = request.on_click() {
            button = button.on_click(move |event, window, cx| callback.invoke(event, window, cx));
        }
        request.finish(button)
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    // `on_click` 和 `disabled` 一样由 Shell 自己读（`MaterializeRequest::on_click`），
    // 描述符只负责让脚本能调。
    let on_click = super::callback_method(
        OWNER,
        "on_click",
        "(event: ClickEvent, cx: Context) => void",
        "Invokes the callback when the button is activated.",
        |_| (),
    );
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(ButtonMaterializer))
            .with_constructors(vec![id_constructor(OWNER)])
            .with_methods([methods(OWNER), vec![on_click]].concat())
            .with_documentation(
                "A Smelt action button with an explicit id; takes children as its content.",
            ),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
