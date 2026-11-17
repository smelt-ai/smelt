//! `SmeltMenuButton`：按钮带出一个下拉菜单（[`smelt_ui::menu`]），一条 `item` 一条命令。
//! 按钮的变体、尺寸和状态同 `SmeltButton`。

use std::sync::Arc;

use gpui::{
    App, ElementId, IntoElement, RenderOnce, SharedString, StyleRefinement, Styled, Window,
};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentCallback, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, MaterializeRequest,
    MethodDescriptor, RegistryError, anyhow,
};
use smelt_ui::button::Button;
use smelt_ui::menu::{Menu, MenuItem, MenuTrigger};

use super::{button, control_id, id_constructor, latest, ops};

const OWNER: &str = "SmeltMenuButton";

#[derive(Clone)]
struct ItemOp {
    label: String,
    callback: ComponentArgument,
}

type Items = Vec<(SharedString, ComponentCallback)>;

#[derive(IntoElement)]
struct MenuButton {
    id: ElementId,
    button: Button,
    items: Items,
    disabled: bool,
    style: StyleRefinement,
}

impl RenderOnce for MenuButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let items = latest(&self.id, self.items, window, cx);
        // 菜单打开时才搭，搭好就留在弹层里；条目点下去时按名字回槽里找这次绘制的回调，
        // 菜单开着时脚本重绘过也不会调到作废的回调。
        let mut trigger = MenuTrigger::dropdown(self.id, self.button, move |_, cx| {
            items.read(cx).iter().fold(Menu::new(), |menu, (label, _)| {
                let items = items.clone();
                let label = label.clone();
                menu.item(MenuItem::new(label.clone()).on_click(move |_, window, cx| {
                    let callback = items
                        .read(cx)
                        .iter()
                        .find(|(current, _)| *current == label)
                        .map(|(_, callback)| callback.clone());
                    if let Some(callback) = callback {
                        callback.invoke_and_report_with(
                            "SmeltMenuButton.item callback failed",
                            &[],
                            window,
                            cx,
                        );
                    }
                }))
            })
        })
        .disabled(self.disabled);
        *trigger.style() = self.style;
        trigger
    }
}

struct MenuButtonMaterializer;

impl ComponentMaterializer for MenuButtonMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        anyhow::ensure!(
            request.children_len() == 0,
            "{OWNER} takes item(label, callback) entries, not children"
        );
        let id = control_id(OWNER, &request)?;
        let button = button::build(OWNER, &request)?;
        let items = ops::<ItemOp>(&request)
            .map(|item| {
                Ok((
                    SharedString::from(item.label.clone()),
                    request.resolve_callback(&item.callback)?,
                ))
            })
            .collect::<anyhow::Result<Items>>()?;
        // 条目点下去时按名字找回调，同名的会调错。
        for (index, (label, _)) in items.iter().enumerate() {
            anyhow::ensure!(
                !items[..index].iter().any(|(earlier, _)| earlier == label),
                "{OWNER} has two items labelled {label:?}"
            );
        }
        Ok(MenuButton {
            id: id.into(),
            button,
            items,
            disabled: request.disabled(),
            style: request.take_style(),
        }
        .into_any_element())
    }
}

fn item_method() -> MethodDescriptor {
    MethodDescriptor::new(
        "item",
        vec![
            ArgumentDescriptor::new("label", ArgumentSchema::String),
            ArgumentDescriptor::new(
                "callback",
                ArgumentSchema::Callback("(cx: Context) => void"),
            ),
        ],
        |args| match args {
            [
                ComponentArgument::String(label),
                callback @ ComponentArgument::Callback(_),
            ] if !label.trim().is_empty() => Ok(ComponentPayload::new(ItemOp {
                label: label.clone(),
                callback: callback.clone(),
            })),
            [ComponentArgument::String(_), ComponentArgument::Callback(_)] => {
                Err(format!("{OWNER}.item label must not be empty"))
            }
            _ => Err(format!(
                "{OWNER}.item(label, callback) expects a string and a callback"
            )),
        },
    )
    .with_documentation("Appends a command in script order; labels must be unique within the menu.")
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(MenuButtonMaterializer))
            .with_constructors(vec![id_constructor(OWNER)])
            .with_methods([button::methods(OWNER), vec![item_method()]].concat())
            .with_documentation(
                "A Smelt button that opens a dropdown menu of commands; styles apply to the trigger.",
            ),
    )?;
    Ok(())
}
