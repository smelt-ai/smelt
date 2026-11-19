//! `SmeltPopover`：点开的浮层卡片（[`smelt_ui::popover::Popover`]），放一组就地的设置、一段
//! 详情、一个小表单。`trigger(元素)` 是触发点，`content(元素)` 是卡片里的内容，开着时才搭。
//! 样式落在卡片上（宽、内边距）。
//!
//! 开关可以交给它自己，也可以由脚本控制：`open(开没开)` 给了就按它画，用户点开、点外面、
//! Esc 经 `on_open_change` 交回，脚本存好再画；脚本在卡片里做完事要关掉它，就把状态改成
//! 关再重画。

use std::sync::Arc;

use gpui::{Anchor, AnyElement, App, IntoElement, ParentElement as _, RenderOnce, Window, div};
use gpui_base::Selectable;
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentCallbackArgument,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, MaterializeRequest,
    MethodDescriptor, RegistryError, anyhow, gpui,
};
use smelt_ui::popover::Popover;

use super::{bool_method, callback_method, control_id, id_constructor, ops};

const OWNER: &str = "SmeltPopover";

#[derive(Clone)]
struct Open(bool);

#[derive(Clone)]
struct OnOpenChange(ComponentArgument);

#[derive(Clone, Copy)]
struct Side(Anchor);

/// 脚本给的触发点：它开没开脚本自己知道（受控时）或不关心，这里不改它的样子。
#[derive(IntoElement)]
struct Trigger(AnyElement);

impl Selectable for Trigger {
    fn selected(self, _: bool) -> Self {
        self
    }

    fn is_selected(&self) -> bool {
        false
    }
}

impl RenderOnce for Trigger {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.0
    }
}

struct PopoverMaterializer;

impl ComponentMaterializer for PopoverMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        anyhow::ensure!(
            request.children_len() == 0,
            "{OWNER} takes trigger(element) and content(element), not children"
        );
        let id = control_id(OWNER, &request)?;
        let trigger = request
            .take_slot("trigger")?
            .ok_or_else(|| anyhow::anyhow!("{OWNER} requires trigger(element)"))?;
        let content = request
            .take_slot_factory("content")
            .ok_or_else(|| anyhow::anyhow!("{OWNER} requires content(element)"))?;
        let mut popover = Popover::new(id).trigger(Trigger(trigger));
        if let Some(Side(anchor)) = ops::<Side>(&request).last() {
            popover = popover.anchor(*anchor);
        }
        if let Some(Open(open)) = ops::<Open>(&request).last() {
            popover = popover.open(*open);
        }
        if let Some(OnOpenChange(callback)) = ops::<OnOpenChange>(&request).last().cloned() {
            let callback = request.resolve_callback(&callback)?;
            popover = popover.on_open_change(move |open, window, cx| {
                callback.invoke_and_report_with(
                    "SmeltPopover.on_open_change callback failed",
                    &[ComponentCallbackArgument::Boolean(*open)],
                    window,
                    cx,
                );
            });
        }
        let popover = popover.content(move |_, window, cx| match content.build(window, cx) {
            Ok(element) => element,
            Err(error) => div()
                .child(format!("Failed to render {OWNER} content: {error:#}"))
                .into_any_element(),
        });
        let style = request.take_style();
        let mut popover = popover;
        *gpui::Styled::style(&mut popover) = style;
        Ok(popover.into_any_element())
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    let side = MethodDescriptor::new(
        "side",
        vec![ArgumentDescriptor::new(
            "corner",
            ArgumentSchema::Enum(&["top_left", "top_right", "bottom_left", "bottom_right"]),
        )],
        |args| {
            let anchor = match args {
                [ComponentArgument::Enum(corner)] => match corner.as_str() {
                    "top_left" => Anchor::TopLeft,
                    "top_right" => Anchor::TopRight,
                    "bottom_left" => Anchor::BottomLeft,
                    "bottom_right" => Anchor::BottomRight,
                    _ => return Err(format!("{OWNER}.side: unknown corner {corner:?}")),
                },
                _ => return Err(format!("{OWNER}.side(corner) expects a corner")),
            };
            Ok(ComponentPayload::new(Side(anchor)))
        },
    )
    .with_documentation(
        "Which corner of the card is pinned to the trigger; default top_left (opens below, left-aligned). Use bottom_* to open above.",
    );
    registry.register(
        gpui_shell::ComponentDescriptor::new(OWNER, Arc::new(PopoverMaterializer))
            .with_constructors(vec![id_constructor(OWNER)])
            .with_methods(vec![
                side,
                bool_method(
                    OWNER,
                    "open",
                    "Controls whether the card is open; user changes come back through on_open_change.",
                    Open,
                ),
                callback_method(
                    OWNER,
                    "on_open_change",
                    "(open: boolean, cx: Context) => void",
                    "Runs when the user opens or closes the card (trigger, Esc, clicking outside).",
                    OnOpenChange,
                ),
            ])
            .with_documentation(
                "A Smelt popover card: trigger(element) opens it, content(element) is built only while open; styles apply to the card.",
            ),
    )?;
    Ok(())
}
