//! `SmeltDragItem`：能拖着换位置的一项（列表行、标签），拖到同一种类的另一项上松手，那一项的
//! `on_drop(from, before)` 告诉脚本拖的是哪一项、放在它前面还是后面。挪不挪、挪了意味着什么
//! （换顺序、跨过置顶区就置顶）都由脚本定，按新顺序重画。
//!
//! - `new(id, kind)`：`id` 是这一项的身份，交给 `on_drop` 的就是它；只有 `kind` 一样的项之间
//!   能互放，规则（只在同一组里挪、只在同一台机器下挪）编进 `kind` 里。
//! - `content(元素)` 是这一项画出来的样子，拖起来时浮在指针下的牌子也按它画（宽度照这一项）。
//! - 拖着时原处变淡，要放到的那一项上沿或下沿画一条线；落回自己上不算。
//!
//! 样式落在外框上：行的外边距给外框（`mx`），落点线就和行一样宽。放在 `SmeltScrollView`
//! 里，拖到上下边缘会自动滚。

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, ElementId, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Render, RenderOnce, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, WeakEntity, Window, div, px,
    rgb,
};
use gpui_base::{ElementExt as _, StyledExt as _};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentCallback,
    ComponentCallbackArgument, ComponentDescriptor, ComponentElementFactory, ComponentMaterializer,
    ComponentPayload, ComponentRegistry, ConstructorDescriptor, MaterializeRequest, RegistryError,
    anyhow, gpui,
};
use smelt_ui::ui_theme;

use super::{callback_method, latest, ops};

const OWNER: &str = "SmeltDragItem";

/// 拖起来的牌子淡到这么多，透出底下的行和落点线，看得清要放到哪。
const PREVIEW_OPACITY: f32 = 0.55;
/// 原处在拖着时淡到这么多。
const LIFTED_OPACITY: f32 = 0.4;

/// 正拖着的那一项。滚动区域也认它（拖到边缘自动滚）。
#[derive(Clone)]
pub(super) struct ItemDrag {
    kind: SharedString,
    id: SharedString,
    /// 原处那一项的状态：牌子放掉时清掉「拿起来了」。
    item: WeakEntity<ItemState>,
}

#[derive(Clone)]
struct Payload {
    id: String,
    kind: String,
}

#[derive(Clone)]
struct OnDrop(ComponentArgument);

/// 一项跨帧留着的：上一帧画出来多宽（牌子照它画），拿没拿起来，落点线画在哪边。
#[derive(Default)]
struct ItemState {
    width: Pixels,
    lifted: bool,
    /// 拖着的东西停在这一项上：`true` 是放在它前面（线在上沿）。
    hint: Option<bool>,
}

#[derive(IntoElement)]
struct DragItem {
    id: SharedString,
    kind: SharedString,
    content: ComponentElementFactory,
    on_drop: Option<ComponentCallback>,
    style: StyleRefinement,
}

impl RenderOnce for DragItem {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let element_id = ElementId::from(self.id.clone());
        let state = window.use_keyed_state((element_id.clone(), "smelt-drag-item"), cx, |_, _| {
            ItemState::default()
        });
        // 牌子每帧按最近交来的样子画；这一项不画了（被挪走、删了）牌子就空着。
        let face = latest(&element_id, self.content.clone(), window, cx);
        let dragging = cx.has_active_drag();
        let (lifted, hint) = {
            let state = state.read(cx);
            (
                dragging && state.lifted,
                dragging.then_some(state.hint).flatten(),
            )
        };
        let content = build(&self.content, window, cx);
        let drag = ItemDrag {
            kind: self.kind.clone(),
            id: self.id.clone(),
            item: state.downgrade(),
        };
        div()
            .id(element_id)
            .relative()
            .refine_style(&self.style)
            .child(content)
            .when(lifted, |item| item.opacity(LIFTED_OPACITY))
            .when_some(hint, |item, before| item.child(drop_line(before)))
            .on_prepaint({
                let state = state.downgrade();
                move |bounds: Bounds<Pixels>, _, cx| {
                    let _ = state.update(cx, |state, _| state.width = bounds.size.width);
                }
            })
            .on_drag(drag, {
                let state = state.clone();
                move |drag, _, _, cx| {
                    let width = state.update(cx, |state, cx| {
                        state.lifted = true;
                        state.hint = None;
                        cx.notify();
                        state.width
                    });
                    let preview = cx.new(|_| Preview {
                        face: face.downgrade(),
                        width,
                    });
                    // 松手、放下、拖出窗口取消，GPUI 都会扔掉牌子：那时原处亮回来。
                    let item = drag.item.clone();
                    cx.observe_release(&preview, move |_, cx| {
                        let _ = item.update(cx, |state, cx| {
                            state.lifted = false;
                            state.hint = None;
                            cx.notify();
                        });
                    })
                    .detach();
                    preview
                }
            })
            .on_drag_move::<ItemDrag>({
                let (state, kind, id) = (state.clone(), self.kind.clone(), self.id.clone());
                move |event, _, cx| {
                    let dragged = event.drag(cx);
                    let accepts = dragged.kind == kind && dragged.id != id;
                    let hint = (accepts && event.bounds.contains(&event.event.position))
                        .then(|| event.event.position.y < event.bounds.center().y);
                    state.update(cx, |state, cx| {
                        if state.hint != hint {
                            state.hint = hint;
                            cx.notify();
                        }
                    });
                }
            })
            .on_drop::<ItemDrag>({
                let (kind, id, on_drop) = (self.kind, self.id, self.on_drop);
                move |dragged, window, cx| {
                    let before = state.update(cx, |state, cx| {
                        cx.notify();
                        state.hint.take()
                    });
                    let Some(before) = before else {
                        return;
                    };
                    if dragged.kind != kind || dragged.id == id {
                        return;
                    }
                    if let Some(callback) = &on_drop {
                        callback.invoke_and_report_with(
                            "SmeltDragItem.on_drop callback failed",
                            &[
                                ComponentCallbackArgument::String(dragged.id.to_string()),
                                ComponentCallbackArgument::Boolean(before),
                            ],
                            window,
                            cx,
                        );
                    }
                }
            })
    }
}

fn build(factory: &ComponentElementFactory, window: &mut Window, cx: &mut App) -> AnyElement {
    match factory.build(window, cx) {
        Ok(element) => element,
        Err(error) => div()
            .child(format!("Failed to render {OWNER} content: {error:#}"))
            .into_any_element(),
    }
}

/// 落点：这一项上沿或下沿一条线，压在两项之间的缝上。
fn drop_line(before: bool) -> impl IntoElement {
    div()
        .absolute()
        .left_0()
        .right_0()
        .h(px(2.))
        .rounded_full()
        .bg(ui_theme::focus())
        .map(|line| {
            if before {
                line.top(px(-1.))
            } else {
                line.bottom(px(-1.))
            }
        })
}

/// 拖着走的牌子：那一项浮起来。宽度要给死：GPUI 按最小内容宽度排牌子，能截短的单行字最窄
/// 是 0，不给宽度标题就只剩一个省略号。底先铺实色，免得行底的半透明叠出两层字。
struct Preview {
    face: WeakEntity<ComponentElementFactory>,
    width: Pixels,
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let face = self.face.upgrade().map(|face| face.read(cx).clone());
        div()
            .w(self.width)
            .rounded(ui_theme::row_radius())
            .bg(rgb(ui_theme::surface()))
            .shadow_lg()
            .opacity(PREVIEW_OPACITY)
            .text_color(rgb(ui_theme::text()))
            .children(face.map(|face| build(&face, window, cx)))
    }
}

struct DragItemMaterializer;

impl ComponentMaterializer for DragItemMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<AnyElement> {
        anyhow::ensure!(
            request.children_len() == 0,
            "{OWNER} takes content(element), not children: the drag preview is drawn from it"
        );
        let Payload { id, kind } = request
            .payload()
            .downcast_ref::<Payload>()
            .ok_or_else(|| anyhow::anyhow!("{OWNER} received an incompatible payload"))?
            .clone();
        let content = request
            .take_slot_factory("content")
            .ok_or_else(|| anyhow::anyhow!("{OWNER} requires content(element)"))?;
        let on_drop = ops::<OnDrop>(&request)
            .last()
            .cloned()
            .map(|on_drop| request.resolve_callback(&on_drop.0))
            .transpose()?;
        Ok(DragItem {
            id: id.into(),
            kind: kind.into(),
            content,
            on_drop,
            style: request.take_style(),
        }
        .into_any_element())
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(DragItemMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                OWNER,
                vec![
                    ArgumentDescriptor::new("id", ArgumentSchema::String),
                    ArgumentDescriptor::new("kind", ArgumentSchema::String),
                ],
                |args| match args {
                    [ComponentArgument::String(id), ComponentArgument::String(kind)]
                        if !id.is_empty() && !kind.is_empty() =>
                    {
                        Ok(ComponentPayload::new(Payload {
                            id: id.clone(),
                            kind: kind.clone(),
                        }))
                    }
                    _ => Err(format!("{OWNER}.new(id, kind) expects two non-empty strings")),
                },
            )])
            .with_methods(vec![callback_method(
                OWNER,
                "on_drop",
                "(from: string, before: boolean, cx: Context) => void",
                "Runs on this item when an item of the same kind is dropped on it: `from` is the dragged item's id, `before` whether it goes above this one. Reorder your own data and redraw.",
                OnDrop,
            )])
            .with_documentation(
                "An item the user can drag onto other items of the same kind; content(element) draws it and its drag preview. Styles apply to the frame.",
            ),
    )?;
    Ok(())
}
