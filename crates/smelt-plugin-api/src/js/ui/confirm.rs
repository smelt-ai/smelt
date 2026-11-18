//! `SmeltConfirmButton`：点了先弹确认框（[`smelt_ui::dialog::Confirm`]），确认才执行。
//! 按钮的变体、尺寸和状态同 `SmeltButton`；框走 smelt 的窗口对话框层。
//!
//! 只做可撤销的确认（挪进回收目录、删链接）：Enter 等于确认。真删不可恢复的东西要
//! `Confirm::danger` 那种按钮才算数的确认，用到时再加。

use std::sync::Arc;

use gpui::{
    App, ElementId, IntoElement, Refineable as _, RenderOnce, SharedString, Styled as _, Window,
};
use gpui_shell::{
    ComponentArgument, ComponentCallback, ComponentDescriptor, ComponentMaterializer,
    ComponentRegistry, MaterializeRequest, RegistryError, anyhow,
};
use smelt_ui::button::Button;
use smelt_ui::dialog::Confirm;

use super::{button, callback_method, control_id, id_constructor, latest, ops, string_method};

const OWNER: &str = "SmeltConfirmButton";

#[derive(Clone)]
enum ConfirmOp {
    Title(String),
    Description(String),
    Action(String),
    OnConfirm(ComponentArgument),
}

#[derive(IntoElement)]
struct ConfirmButton {
    id: ElementId,
    button: Button,
    title: SharedString,
    description: Option<SharedString>,
    action: SharedString,
    on_confirm: ComponentCallback,
}

impl RenderOnce for ConfirmButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // 框开着时脚本重绘过，确认时也要调到这次绘制的回调。
        let on_confirm = latest(&self.id, self.on_confirm, window, cx);
        let (title, description, action) = (self.title, self.description, self.action);
        self.button.on_click(move |_, window, cx| {
            let on_confirm = on_confirm.clone();
            let confirm = Confirm::new(title.clone(), action.clone(), move |window, cx| {
                let callback = on_confirm.read(cx).clone();
                callback.invoke_and_report_with(
                    "SmeltConfirmButton.on_confirm callback failed",
                    &[],
                    window,
                    cx,
                );
                true
            });
            let confirm = match &description {
                Some(description) => confirm.description(description.clone()),
                None => confirm,
            };
            confirm.open(window, cx);
        })
    }
}

struct ConfirmButtonMaterializer;

impl ComponentMaterializer for ConfirmButtonMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        anyhow::ensure!(
            request.children_len() == 0,
            "{OWNER} does not accept children"
        );
        let id = control_id(OWNER, &request)?;
        let mut button = button::build(OWNER, &request)?;
        button.style().refine(&request.take_style());
        let (mut title, mut description, mut action, mut on_confirm) = (None, None, None, None);
        for op in ops::<ConfirmOp>(&request) {
            match op {
                ConfirmOp::Title(text) => title = Some(SharedString::from(text.clone())),
                ConfirmOp::Description(text) => {
                    description = Some(SharedString::from(text.clone()))
                }
                ConfirmOp::Action(text) => action = Some(SharedString::from(text.clone())),
                ConfirmOp::OnConfirm(callback) => on_confirm = Some(callback),
            }
        }
        let missing = |name: &str| anyhow::anyhow!("{OWNER} needs {name}(...)");
        Ok(ConfirmButton {
            id: id.into(),
            button,
            title: title.ok_or_else(|| missing("title"))?,
            description,
            action: action.ok_or_else(|| missing("action"))?,
            on_confirm: request
                .resolve_callback(on_confirm.ok_or_else(|| missing("on_confirm"))?)?,
        }
        .into_any_element())
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    let confirm_methods = vec![
        string_method(
            OWNER,
            "title",
            "Sets the confirmation question; required.",
            ConfirmOp::Title,
        ),
        string_method(
            OWNER,
            "description",
            "Explains what happens and how to undo it, under the title.",
            ConfirmOp::Description,
        ),
        string_method(
            OWNER,
            "action",
            "Sets the confirm button's verb, such as \"Delete\", never \"OK\"; required.",
            ConfirmOp::Action,
        ),
        callback_method(
            OWNER,
            "on_confirm",
            "(cx: Context) => void",
            "Runs after the user confirms; required. Cancel and Escape do nothing.",
            ConfirmOp::OnConfirm,
        ),
    ];
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(ConfirmButtonMaterializer))
            .with_constructors(vec![id_constructor(OWNER)])
            .with_methods([button::methods(OWNER), confirm_methods].concat())
            .with_documentation(
                "A Smelt button that asks for confirmation in a window dialog before running on_confirm.",
            ),
    )?;
    Ok(())
}
