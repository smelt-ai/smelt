//! `SmeltDialog`：模态对话框（[`smelt_ui::dialog`]），压在窗口的对话框层上，焦点圈、遮罩、
//! 打开时的淡入都归它。
//!
//! 开关由脚本控制：把 `SmeltDialog` 放在自己的界面里（放哪都行，它本身不占地方），
//! `open(true)` 时压上一张，`open(false)` 或不再画它就关掉。用户按 Esc、点遮罩经
//! `on_dismiss` 交回，按 Enter 经 `on_confirm` 交回，框都不自己关：脚本把状态改成关再重画。
//!
//! 卡片里是 `title` / `description`，下面 `content(元素)` 是正文（太长时滚动），`footer(元素)`
//! 是钉在底下的按钮行。脚本每次绘制把这几样交进一个槽，对话框层重画时按槽里最近那份搭。
//! 样式落在卡片上（宽）。`pin_top(像素)` 把卡片顶边钉住，不再上下居中。

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, ElementId, IntoElement, ParentElement as _, RenderOnce, SharedString, StyleRefinement,
    Styled, Window, div,
};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentCallback, ComponentDescriptor,
    ComponentElementFactory, ComponentMaterializer, ComponentPayload, ComponentRegistry,
    MaterializeRequest, MethodDescriptor, RegistryError, anyhow, gpui,
};
use smelt_ui::dialog::{self, DialogId};

use super::{bool_method, callback_method, control_id, id_constructor, latest, ops, string_method};

const OWNER: &str = "SmeltDialog";

#[derive(Clone)]
enum Op {
    Open(bool),
    Title(String),
    Description(String),
    CloseOnBackdrop(bool),
    Top(f32),
    OnConfirm(ComponentArgument),
    OnDismiss(ComponentArgument),
}

/// 最近一次脚本绘制交来的卡片，对话框层重绘时按它搭。
struct Latest {
    title: Option<SharedString>,
    description: Option<SharedString>,
    content: Option<ComponentElementFactory>,
    footer: Option<ComponentElementFactory>,
    style: StyleRefinement,
    close_on_backdrop: bool,
    top: Option<f32>,
    on_confirm: Option<ComponentCallback>,
    on_dismiss: Option<ComponentCallback>,
}

/// 这个控件压上去的那张。开是推到这一帧之后做的，中间再画一次不能又开一张，所以有「正在开」。
#[derive(Clone, Copy, Default, PartialEq)]
enum Opened {
    #[default]
    Closed,
    Opening,
    Open(DialogId),
}

#[derive(IntoElement)]
struct ScriptDialog {
    id: ElementId,
    open: bool,
    latest: Latest,
}

impl RenderOnce for ScriptDialog {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let slot = latest(&self.id, self.latest, window, cx);
        let opened = window.use_keyed_state((self.id.clone(), "smelt-dialog"), cx, |_, _| {
            Opened::default()
        });
        let current = *opened.read(cx);
        match (self.open, current) {
            (true, Opened::Closed) => {
                opened.update(cx, |opened, _| *opened = Opened::Opening);
                let weak_slot = slot.downgrade();
                let weak_opened = opened.downgrade();
                window.defer(cx, move |window, cx| {
                    // 推迟期间脚本改成了关，或控件不画了：不开。
                    let Some(opened) = weak_opened.upgrade() else {
                        return;
                    };
                    if *opened.read(cx) != Opened::Opening {
                        return;
                    }
                    let own = Rc::new(Cell::new(None::<DialogId>));
                    let id = dialog::open(window, cx, {
                        let own = own.clone();
                        let weak_opened = opened.downgrade();
                        move |card, window, cx| {
                            // 控件不画了，状态被回收：这张也该关了。正在画对话框层，推到之后关。
                            let Some(slot) = weak_slot.upgrade() else {
                                if let Some(id) = own.take() {
                                    window.defer(cx, move |window, cx| id.close(window, cx));
                                }
                                return card;
                            };
                            let (own, weak_opened) = (own.get(), weak_opened.clone());
                            let dismiss = slot.downgrade();
                            build(card, &slot, window, cx).on_close(move |window, cx| {
                                // 脚本自己关的先记成了关；还记着这张，就是别处关的（全局的
                                // `dialog::close`）：记成关，经 on_dismiss 告诉脚本。
                                let Some(opened) = weak_opened.upgrade() else {
                                    return;
                                };
                                if own.is_none_or(|own| *opened.read(cx) != Opened::Open(own)) {
                                    return;
                                }
                                opened.update(cx, |opened, _| *opened = Opened::Closed);
                                dismissed(&dismiss, window, cx);
                            })
                        }
                    });
                    own.set(id);
                    opened.update(cx, |opened, _| {
                        *opened = id.map_or(Opened::Closed, Opened::Open);
                    });
                });
            }
            (false, Opened::Open(id)) => {
                opened.update(cx, |opened, _| *opened = Opened::Closed);
                window.defer(cx, move |window, cx| id.close(window, cx));
            }
            (false, Opened::Opening) => {
                opened.update(cx, |opened, _| *opened = Opened::Closed);
            }
            // 开着的那张由对话框层在这一帧随后重画（窗口根上它排在主体之后），读到的就是
            // 刚交进槽里的这份，不用另外叫它。
            (true, _) | (false, Opened::Closed) => {}
        }
        div()
    }
}

/// 按槽里最近的一份搭卡片。确认、取消都不让框自己关：交给脚本，脚本改状态再画。
fn build(
    card: dialog::Card,
    slot: &gpui::Entity<Latest>,
    window: &mut Window,
    cx: &mut App,
) -> dialog::Card {
    let latest = slot.read(cx);
    let heading = latest
        .title
        .clone()
        .map(|title| (title, latest.description.clone()));
    let content = latest.content.clone();
    let footer = latest.footer.clone();
    let close_on_backdrop = latest.close_on_backdrop;
    let top = latest.top;
    let style = latest.style.clone();
    let mut body = match heading {
        Some((title, description)) => dialog::content(title, description),
        None => div().flex().flex_col(),
    };
    if let Some(content) = &content {
        body = body.child(materialize(content, window, cx));
    }
    let footer = footer.map(|footer| materialize(&footer, window, cx));
    let mut card = card.close_on_backdrop(close_on_backdrop).child(body);
    *card.style() = style;
    if let Some(top) = top {
        card = card.top(gpui::px(top));
    }
    if let Some(footer) = footer {
        card = card.footer(footer);
    }
    let confirm = slot.downgrade();
    let dismiss = slot.downgrade();
    card.on_ok(move |window, cx| {
        let callback = confirm
            .upgrade()
            .and_then(|slot| slot.read(cx).on_confirm.clone());
        if let Some(callback) = callback {
            callback.invoke_and_report_with(
                "SmeltDialog.on_confirm callback failed",
                &[],
                window,
                cx,
            );
        }
        false
    })
    .on_cancel(move |window, cx| {
        dismissed(&dismiss, window, cx);
        false
    })
}

fn dismissed(slot: &gpui::WeakEntity<Latest>, window: &mut Window, cx: &mut App) {
    let callback = slot
        .upgrade()
        .and_then(|slot| slot.read(cx).on_dismiss.clone());
    if let Some(callback) = callback {
        callback.invoke_and_report_with("SmeltDialog.on_dismiss callback failed", &[], window, cx);
    }
}

fn materialize(
    factory: &ComponentElementFactory,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyElement {
    match factory.build(window, cx) {
        Ok(element) => element,
        Err(error) => div()
            .child(format!("Failed to render {OWNER} content: {error:#}"))
            .into_any_element(),
    }
}

struct DialogMaterializer;

impl ComponentMaterializer for DialogMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        anyhow::ensure!(
            request.children_len() == 0,
            "{OWNER} takes content(element) and footer(element), not children"
        );
        let id = control_id(OWNER, &request)?;
        let mut open = false;
        let mut latest = Latest {
            title: None,
            description: None,
            content: request.take_slot_factory("content"),
            footer: request.take_slot_factory("footer"),
            style: StyleRefinement::default(),
            close_on_backdrop: true,
            top: None,
            on_confirm: None,
            on_dismiss: None,
        };
        for op in ops::<Op>(&request).cloned().collect::<Vec<_>>() {
            match op {
                Op::Open(value) => open = value,
                Op::Title(text) => latest.title = Some(text.into()),
                Op::Description(text) => latest.description = Some(text.into()),
                Op::CloseOnBackdrop(value) => latest.close_on_backdrop = value,
                Op::Top(offset) => latest.top = Some(offset),
                Op::OnConfirm(callback) => {
                    latest.on_confirm = Some(request.resolve_callback(&callback)?)
                }
                Op::OnDismiss(callback) => {
                    latest.on_dismiss = Some(request.resolve_callback(&callback)?)
                }
            }
        }
        anyhow::ensure!(
            latest.on_dismiss.is_some(),
            "{OWNER} needs on_dismiss: the dialog never closes itself, Esc has to reach the script"
        );
        latest.style = request.take_style();
        Ok(ScriptDialog {
            id: id.into(),
            open,
            latest,
        }
        .into_any_element())
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(DialogMaterializer))
            .with_constructors(vec![id_constructor(OWNER)])
            .with_methods(vec![
                bool_method(
                    OWNER,
                    "open",
                    "Whether the dialog is up; the script owns this state.",
                    Op::Open,
                ),
                string_method(OWNER, "title", "The heading at the top of the card.", Op::Title),
                string_method(
                    OWNER,
                    "description",
                    "A sentence under the heading.",
                    Op::Description,
                ),
                bool_method(
                    OWNER,
                    "close_on_backdrop",
                    "Whether clicking the backdrop reports on_dismiss; default true. Turn it off for forms.",
                    Op::CloseOnBackdrop,
                ),
                MethodDescriptor::new(
                    "pin_top",
                    vec![ArgumentDescriptor::new("offset", ArgumentSchema::Number)],
                    |args| match args {
                        [ComponentArgument::Number(offset)] if offset.is_finite() && *offset >= 0. => {
                            Ok(ComponentPayload::new(Op::Top(*offset as f32)))
                        }
                        _ => Err(format!("{OWNER}.pin_top(offset) expects pixels >= 0")),
                    },
                )
                .with_documentation(
                    "Pins the card's top edge this many pixels below the window top instead of centring it; for dialogs whose height follows their content (a filtered list), so the top does not jump.",
                ),
                callback_method(
                    OWNER,
                    "on_confirm",
                    "(cx: Context) => void",
                    "Runs on Enter; close the dialog by setting open(false) if the action is done.",
                    Op::OnConfirm,
                ),
                callback_method(
                    OWNER,
                    "on_dismiss",
                    "(cx: Context) => void",
                    "Runs on Esc or a backdrop click; required. Set open(false) to close.",
                    Op::OnDismiss,
                ),
            ])
            .with_documentation(
                "A Smelt modal dialog controlled by open(bool): title, description, content(element) body and footer(element) buttons; styles apply to the card.",
            ),
    )?;
    Ok(())
}
