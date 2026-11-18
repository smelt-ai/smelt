//! `SmeltSortableList`：拖动改顺序的竖排列表。子元素是一行一行，拖起一行、松在别的位置上，
//! `on_reorder(from, to)` 告诉脚本那一行从第几行挪到了第几行（都从 0 数，`to` 是挪完以后的
//! 位置），顺序由脚本自己改、自己存，再按新顺序重画。
//!
//! 只在同一个列表里挪：拖进别的列表、拖出列表松手都不算。拖的时候被拖的那行变淡，要落到的
//! 地方画一条线。

use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, ElementId, EmptyView, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Pixels, Refineable as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled, Window, div, prelude::FluentBuilder,
    px,
};
use gpui_shell::{
    ComponentArgument, ComponentCallback, ComponentCallbackArgument, ComponentDescriptor,
    ComponentMaterializer, ComponentRegistry, MaterializeRequest, RegistryError, anyhow,
};
use smelt_ui::ui_theme;

use super::{callback_method, control_id, id_constructor, ops};

const OWNER: &str = "SmeltSortableList";

/// 落点线的粗细。
const LINE: Pixels = px(2.);

#[derive(Clone)]
struct Reorder(ComponentArgument);

/// 拖着的那一行：哪个列表的第几行。
struct RowDrag {
    list: ElementId,
    from: usize,
}

/// 这一次拖动：拖的第几行，现在会插到第几行之前（等于行数是插到最后）。指针不在列表上时
/// 没有落点。松手就清掉：松在列表上经 `on_drop`，松在列表外经 `on_mouse_up_out`。
#[derive(Default)]
struct DragState {
    from: Option<usize>,
    to: Option<usize>,
}

struct SortableMaterializer;

impl ComponentMaterializer for SortableMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<AnyElement> {
        let id = control_id(OWNER, &request)?;
        let on_reorder = ops::<Reorder>(&request)
            .last()
            .map(|reorder| request.resolve_callback(&reorder.0))
            .transpose()?;
        Ok(Sortable {
            id: SharedString::from(id).into(),
            on_reorder,
            style: request.take_style(),
            rows: request.take_children()?,
        }
        .into_any_element())
    }
}

#[derive(IntoElement)]
struct Sortable {
    id: ElementId,
    on_reorder: Option<ComponentCallback>,
    style: StyleRefinement,
    rows: Vec<AnyElement>,
}

impl RenderOnce for Sortable {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state((self.id.clone(), "smelt-ui-sortable"), cx, |_, _| {
            DragState::default()
        });
        let count = self.rows.len();
        let (from, to) = if cx.has_active_drag() {
            let state = state.read(cx);
            (state.from, state.to)
        } else {
            (None, None)
        };
        // 落回原处（自己上面、下面）不算挪，不画线。
        let line = to.filter(|to| from.is_some_and(|from| *to != from && *to != from + 1));
        let rows = self.rows.into_iter().enumerate().map(|(index, row)| {
            let list = self.id.clone();
            div()
                .id(index)
                .relative()
                .child(row)
                .when(from == Some(index), |row| row.opacity(0.5))
                .on_drag(RowDrag { list, from: index }, {
                    let state = state.clone();
                    move |_, _, _, cx| {
                        state.update(cx, |state, _| {
                            *state = DragState {
                                from: Some(index),
                                to: None,
                            }
                        });
                        cx.new(|_| EmptyView)
                    }
                })
                .on_drag_move::<RowDrag>({
                    let (state, list) = (state.clone(), self.id.clone());
                    move |event, _, cx| {
                        let bounds = event.bounds;
                        if event.drag(cx).list != list || !bounds.contains(&event.event.position) {
                            return;
                        }
                        let below = event.event.position.y > bounds.center().y;
                        let to = Some(index + usize::from(below));
                        state.update(cx, |state, cx| {
                            if state.to != to {
                                state.to = to;
                                cx.notify();
                            }
                        });
                    }
                })
                .when(line == Some(index), |row| {
                    row.child(drop_line().top(-LINE / 2.))
                })
                .when(index + 1 == count && line == Some(count), |row| {
                    row.child(drop_line().bottom(-LINE / 2.))
                })
        });
        let mut list = div()
            .id(self.id.clone())
            .flex()
            .flex_col()
            .children(rows)
            .on_drag_move::<RowDrag>({
                let state = state.clone();
                move |event, _, cx| {
                    if event.bounds.contains(&event.event.position) {
                        return;
                    }
                    state.update(cx, |state, cx| {
                        if state.to.take().is_some() {
                            cx.notify();
                        }
                    });
                }
            })
            .on_mouse_up_out(MouseButton::Left, {
                let state = state.clone();
                move |_, _, cx| {
                    state.update(cx, |state, cx| {
                        if state.from.is_some() {
                            *state = DragState::default();
                            cx.notify();
                        }
                    });
                }
            })
            .on_drop::<RowDrag>({
                let (list, on_reorder) = (self.id, self.on_reorder);
                move |dragged, window, cx| {
                    if dragged.list != list {
                        return;
                    }
                    let to = state.update(cx, |state, cx| {
                        cx.notify();
                        std::mem::take(state).to
                    });
                    let (Some(to), Some(callback)) = (to, &on_reorder) else {
                        return;
                    };
                    let from = dragged.from;
                    // 插到自己后面的位置时，先拿掉自己，后面的都往前挪了一行。
                    let to = if to > from { to - 1 } else { to };
                    if to == from {
                        return;
                    }
                    callback.invoke_and_report_with(
                        "SmeltSortableList.on_reorder callback failed",
                        &[
                            ComponentCallbackArgument::Number(from as f64),
                            ComponentCallbackArgument::Number(to as f64),
                        ],
                        window,
                        cx,
                    );
                }
            });
        list.style().refine(&self.style);
        list
    }
}

/// 落点那条线，压在两行之间的缝上。
fn drop_line() -> gpui::Div {
    div()
        .absolute()
        .left_0()
        .right_0()
        .h(LINE)
        .rounded_full()
        .bg(ui_theme::focus())
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(SortableMaterializer))
            .with_constructors(vec![id_constructor(OWNER)])
            .with_methods(vec![callback_method(
                OWNER,
                "on_reorder",
                "(from: number, to: number, cx: Context) => void",
                "Runs when the user drags row `from` to position `to` (both 0-based; `to` is the \
                 row's index after the move). Reorder your own data and redraw.",
                Reorder,
            )])
            .with_documentation(
                "A vertical list whose children are rows the user can drag to reorder within \
                 the list. The script owns the order and applies it in on_reorder.",
            ),
    )?;
    Ok(())
}
