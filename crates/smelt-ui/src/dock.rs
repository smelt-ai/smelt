//! Dock 的 smelt 画法。gpui-base 的 Dock 管摆放、拖动、存档，自己一笔不画；这里是两层 Dock
//! （主窗口的工作台、会话的分屏）共用的几样：分隔条、停靠区边上拖宽度的把手、组里显示着的
//! 面板、拖动时的落点、页签条和拖页签时跟着鼠标的那块。各自的 `DockAreaRenderer` /
//! `TabGroupRenderer` 拿这些拼，标签条上画什么（标题、角标、按钮）归各自。
//!
//! 没有实底（底是窗口那一层），栏与栏之间只靠一根 `hair` 分开；页签是
//! [`crate::tabs`] 的胶囊；落点是一块圆角的选区色。

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, AnyView, App, AppContext as _, Context, Div, Empty, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Point, Render, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, WeakEntity, Window, canvas, div,
    prelude::FluentBuilder as _, px, rgb, size,
};
use gpui_base::dock::{
    DockArea, DockContext, DockEvent, DockPlacement, DragPanel, DropIndicator, TabGroupContext,
};
use gpui_base::motion::spring;
use gpui_base::{HandleEdge, ResizeHandleContext};

use crate::kit::{MOTION, RADIUS};
use crate::tabs::{Tab, TabBar};
use crate::window_bar::with_window_drag;
use crate::{resizable, ui_theme};

/// 分屏里两格之间的分隔条：一根 `hair`，压上去、拖着时亮到 `faint`。
pub fn split_handle(handle: &ResizeHandleContext) -> AnyElement {
    resizable::hairline(handle.axis(), handle.state())
}

/// 停靠区靠里那条边上拖宽度的把手。一块 Dock 一份，由它的皮肤持有。
///
/// 拖动不只发生在把手那几个像素上：一拖指针就离开了把手，所以拖着时整扇窗口的移动都交给
/// Dock（[`DockContext::resize_to`]），松手时收尾（[`DockContext::end_resize`]）。宽度存在
/// 停靠区上、不在布局树里，Dock 自己不为它发 `LayoutChanged`，松手时这里补发，存档才记得住。
pub struct DockEdges {
    area: WeakEntity<DockArea>,
    /// 正在拖的那一个停靠区。
    resizing: Rc<Cell<Option<DockPlacement>>>,
}

/// 拖停靠区边的拖动内容。只用来区分拖动，什么都不画。
#[derive(Clone)]
struct ResizeDock;

impl Render for ResizeDock {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

impl DockEdges {
    pub fn new(area: WeakEntity<DockArea>) -> Self {
        Self {
            area,
            resizing: Rc::default(),
        }
    }

    /// `DockAreaRenderer::render_dock`：内容，加上靠里那条边的把手。
    pub fn render(&self, dock: &DockContext, content: AnyElement) -> AnyElement {
        let placement = dock.placement();
        // 每个停靠区一个 id：几个停靠区画在同一个有状态的祖先下面，同一个字面量会让它们
        // 共用一份元素状态，按左边的把手拖起来的是右边的。
        let id = match placement {
            DockPlacement::Left => "dock-edge-left",
            DockPlacement::Right => "dock-edge-right",
            DockPlacement::Bottom => "dock-edge-bottom",
            DockPlacement::Center => "dock-edge-center",
        };
        // 停靠区按自己的框裁剪，把手只能在里面：左停靠区贴右边，右、下停靠区贴左边、上边。
        let edge = match placement {
            DockPlacement::Left => HandleEdge::Trailing,
            _ => HandleEdge::Leading,
        };
        let handle = gpui_base::resize_handle(id, placement.axis())
            .inside(edge)
            .with_appearance(resizable::handle_appearance())
            .on_drag(ResizeDock, {
                let resizing = self.resizing.clone();
                move |drag, _, _, cx| {
                    cx.stop_propagation();
                    resizing.set(Some(placement));
                    cx.new(|_| drag.clone())
                }
            });
        let tracker = canvas(|_, _, _| {}, {
            let dock = dock.clone();
            let area = self.area.clone();
            let resizing = self.resizing.clone();
            move |_, _, window, _| track_resize(dock, area, resizing, window)
        })
        .absolute()
        .size_0();
        div()
            .flex()
            .size_full()
            .relative()
            .child(content)
            .child(handle)
            .child(tracker)
            .into_any_element()
    }
}

/// 拖着这个停靠区的边时，整扇窗口的指针移动改它的宽度，松手收尾。只能在 paint 里挂。
fn track_resize(
    dock: DockContext,
    area: WeakEntity<DockArea>,
    resizing: Rc<Cell<Option<DockPlacement>>>,
    window: &mut Window,
) {
    let placement = dock.placement();
    window.on_mouse_event({
        let dock = dock.clone();
        let resizing = resizing.clone();
        move |event: &gpui::MouseMoveEvent, phase, window, cx| {
            if phase.bubble() && resizing.get() == Some(placement) {
                dock.resize_to(event.position, window, cx);
            }
        }
    });
    window.on_mouse_event(move |_: &gpui::MouseUpEvent, phase, window, cx| {
        if !phase.bubble() || resizing.get() != Some(placement) {
            return;
        }
        resizing.set(None);
        dock.end_resize(window, cx);
        _ = area.update(cx, |_, cx| cx.emit(DockEvent::LayoutChanged));
    });
}

/// `TabGroupRenderer::render_active_panel`：组里显示着的面板铺满内容区。面板是缓存的视图，
/// 别处重画不连带它。收起的组（只剩页签条）不画内容。
pub fn active_panel(panel: AnyView, group: &TabGroupContext) -> AnyElement {
    if group.is_collapsed() {
        return Empty.into_any_element();
    }
    div()
        .id("dock-active-panel")
        .relative()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .child(panel.cached(StyleRefinement::default().absolute().size_full()))
        .into_any_element()
}

/// `TabGroupRenderer::render_drop_indicator`：拖面板时它会落到的那块，跟着落点滑过去。
pub fn drop_indicator(indicator: DropIndicator, window: &mut Window, cx: &mut App) -> AnyElement {
    let to = indicator.to();
    // 半个像素以内算到了，不为看不出的差别一直要帧。
    let policy = MOTION.control.with_epsilon(0.5);
    let id = "dock-drop-indicator";
    let left = spring((id, "left"), to.origin().x, policy, window, cx);
    let top = spring((id, "top"), to.origin().y, policy, window, cx);
    let width = spring((id, "width"), to.size().width, policy, window, cx);
    let height = spring((id, "height"), to.size().height, policy, window, cx);
    div()
        .absolute()
        .left(left)
        .top(top)
        .w(width)
        .h(height)
        .rounded(px(RADIUS.md))
        .bg(ui_theme::selection())
        .into_any_element()
}

/// 拖页签时跟着鼠标的那块的大小，Dock 按它算落点从哪飞过去。
pub const DRAG_PREVIEW_SIZE: gpui::Size<Pixels> = size(px(96.), px(30.));

/// 拖页签时跟着鼠标的那块：面板名字的一小块浮层。
pub struct DragPreview {
    title: SharedString,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("dock-drag-preview")
            .cursor_grab()
            .py_1()
            .px_3()
            .w(DRAG_PREVIEW_SIZE.width)
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .border_1()
            .border_color(ui_theme::hair())
            .rounded(px(RADIUS.md))
            .text_color(rgb(ui_theme::text()))
            .bg(rgb(ui_theme::surface()))
            .opacity(0.75)
            .child(self.title.clone())
    }
}

/// 开始拖一块面板：记下按在哪、预览多大，返回跟着鼠标的那块。挂在页签、标题条的
/// `on_drag` 里。
pub fn drag_preview(
    drag: &DragPanel,
    offset: Point<Pixels>,
    title: SharedString,
    cx: &mut App,
) -> gpui::Entity<DragPreview> {
    cx.stop_propagation();
    drag.set_drag_offset(offset);
    drag.set_preview_size(DRAG_PREVIEW_SIZE);
    cx.new(|_| DragPreview { title })
}

/// 页签条上一块面板的样子，由画标签条的那一方按面板给。
pub struct TabLabel {
    /// 纯文本：拖动预览用。
    pub text: SharedString,
    pub label: AnyElement,
    /// 名字后面的角标。
    pub badge: Option<AnyElement>,
}

/// 组里的一排页签：点了切过去，拖着重排、拖进别的组；页签放不下时横着滚。后面的空白
/// 收拖过来的页签（放到最后），`drag_window` 时它也拖窗口（这排在窗口标题栏上）。
///
/// `tabs` 是看得见的那几块，带它在组里的序号。高度和两头的让位归调用方。
pub fn tab_strip(
    group: &TabGroupContext,
    tabs: Vec<(usize, TabLabel)>,
    drag_window: bool,
    cx: &App,
) -> Div {
    let selected = tabs.iter().position(|(ix, _)| *ix == group.active_ix());
    let draggable = group.is_draggable();
    let droppable = group.is_droppable();
    let count = tabs.len();
    let tabs: Vec<Tab> = tabs
        .into_iter()
        .enumerate()
        .map(|(shown, (ix, label))| {
            let drag = draggable.then(|| group.drag_panel(ix, cx)).flatten();
            let select = group.clone();
            let title = label.text;
            Tab::new(("dock-tab", ix))
                .selected(selected == Some(shown))
                .set_position(shown + 1, count)
                .on_click(move |_, window, cx| select.select_tab(ix, window, cx))
                .child(label.label)
                .children(label.badge)
                .when_some(drag, |tab, drag| {
                    tab.on_drag(drag, move |drag, offset, _, cx| {
                        drag_preview(drag, offset, title.clone(), cx)
                    })
                })
                .when(droppable, |tab| {
                    let group = group.clone();
                    tab.drag_over::<DragPanel>(|this, _, _, _| {
                        this.border_l_2().border_color(ui_theme::focus())
                    })
                    .on_drop(move |drag: &DragPanel, window, cx| {
                        group.drop_panel(drag.clone(), Some(ix), true, window, cx);
                    })
                })
        })
        .collect();
    let last = group.panels().len().saturating_sub(1);
    let node = group.node();
    let tail_group = group.clone();
    let tail = div()
        .id("dock-tab-strip-tail")
        .flex_1()
        .min_w_0()
        .h_full()
        .when(droppable, |this| {
            this.drag_over::<DragPanel>(|this, _, _, _| this.bg(ui_theme::row_selected()))
                .on_drop(move |drag: &DragPanel, window, cx| {
                    // 自己组里的拖到最后一格；别处来的追加在后面，不抢显示。
                    let ix = (drag.source() == node).then_some(last);
                    tail_group.drop_panel(drag.clone(), ix, false, window, cx);
                })
        });
    div()
        .flex()
        .items_center()
        .flex_1()
        .min_w_0()
        .h_full()
        .child(
            div()
                .id("dock-tab-strip-scroll")
                .min_w_0()
                .overflow_x_scroll()
                .child(TabBar::new("dock-tab-strip").children(tabs)),
        )
        .child(if drag_window {
            with_window_drag(tail)
        } else {
            tail
        })
}
