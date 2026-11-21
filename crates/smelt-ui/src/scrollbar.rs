//! 滚动区域及固定在视口上的滚动条。
//!
//! `Scrollable` 接收调用方持有的句柄；源元素是内容，不是滚动视口。
//! 已有列表等自带视口的组件应将 `Scrollbar` 放在视口的外层兄弟位置，
//! 并使用默认的句柄视口，不能将条放进滚动内容中。

use rust_i18n::t;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use gpui::{
    AnyElement, App, Axis, Bounds, DispatchPhase, Div, Element, ElementId, EntityId, Global,
    GlobalElementId, Hitbox, HitboxBehavior, HitboxId, InspectorElementId, InteractiveElement,
    IntoElement, LayoutId, OngoingScroll, Overflow, ParentElement, Pixels, Point, PointRefinement,
    RenderOnce, ScrollDelta, ScrollEvent, ScrollHandle, SharedString, Stateful,
    StatefulInteractiveElement, StyleRefinement, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_base::{InteractiveElementExt as _, StyledExt as _};

pub use gpui_base::{
    Scrollbar, ScrollbarAxis, ScrollbarEntrance, ScrollbarHandle, ScrollbarMode, ScrollbarMotion,
    ScrollbarStyles, ScrollbarThumbStyle, ScrollbarTrackStyle,
};

/// 将内容包装为滚动区域。ID 和句柄由调用方持有并在重绘间保持稳定。
///
/// 滚轮先给指针下最里层还能动的 `Scrollable`，它到头了再交给外层。里面的多行输入框、代码
/// 编辑器先滚，它们滚了外层就不动。里面要再套滚动区域也用 `Scrollable`：GPUI 原生的
/// `overflow_*_scroll` 滚了也不拦事件，会和外层一起滚。
#[derive(IntoElement)]
pub struct Scrollable<E: InteractiveElement + Styled + ParentElement + Element> {
    id: ElementId,
    element: E,
    axis: ScrollbarAxis,
    scroll_handle: ScrollHandle,
    vertical_label: SharedString,
    horizontal_label: SharedString,
}

impl<E> Scrollable<E>
where
    E: InteractiveElement + Styled + ParentElement + Element,
{
    pub fn new(
        id: impl Into<ElementId>,
        element: E,
        scroll_handle: &ScrollHandle,
        axis: impl Into<ScrollbarAxis>,
    ) -> Self {
        Self {
            id: id.into(),
            element,
            axis: axis.into(),
            scroll_handle: scroll_handle.clone(),
            vertical_label: t!("scrollbar.vertical").into(),
            horizontal_label: t!("scrollbar.horizontal").into(),
        }
    }

    /// 为辅助技术区分同一页面上的多个滚动区域。
    pub fn vertical_label(mut self, label: impl Into<SharedString>) -> Self {
        self.vertical_label = label.into();
        self
    }

    pub fn horizontal_label(mut self, label: impl Into<SharedString>) -> Self {
        self.horizontal_label = label.into();
        self
    }
}

impl<E> Styled for Scrollable<E>
where
    E: InteractiveElement + Styled + ParentElement + Element,
{
    fn style(&mut self) -> &mut StyleRefinement {
        self.element.style()
    }
}

impl<E> ParentElement for Scrollable<E>
where
    E: InteractiveElement + Styled + ParentElement + Element,
{
    fn extend(&mut self, elements: impl IntoIterator<Item = gpui::AnyElement>) {
        self.element.extend(elements)
    }
}

impl<E> InteractiveElement for Scrollable<E>
where
    E: InteractiveElement + Styled + ParentElement + Element,
{
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.element.interactivity()
    }

    fn track_focus(mut self, focus: &gpui::FocusHandle) -> Self {
        self.element = self.element.track_focus(focus);
        self
    }
}

impl<E> RenderOnce for Scrollable<E>
where
    E: InteractiveElement + Styled + ParentElement + Element + 'static,
{
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let scroll_handle = self.scroll_handle.clone();
        let root_style = root_style_from(&mut self.element, self.axis);
        let root_id = self.id.clone();
        let area_id = (self.id.clone(), "area");
        let content_id = (self.id.clone(), "content");
        let scrollbar_id = (self.id.clone(), "scrollbar");

        let mut content = self
            .element
            .id(content_id)
            .flex_none()
            .map(|this| match self.axis {
                ScrollbarAxis::Vertical => this.h_auto().min_h_full(),
                ScrollbarAxis::Horizontal => this.w_auto().min_w_full(),
                ScrollbarAxis::Both => this.size_auto().min_size_full(),
            });
        // 最大尺寸属于外层视口；留在内容上会阻止滚动轴产生溢出。
        let max_size = &mut content.style().max_size;
        match self.axis {
            ScrollbarAxis::Vertical => max_size.height = None,
            ScrollbarAxis::Horizontal => max_size.width = None,
            ScrollbarAxis::Both => *max_size = Default::default(),
        }

        let scroll_area = div()
            .id(area_id)
            .size_full()
            .flex()
            .track_scroll(&scroll_handle)
            .map(|this| match self.axis {
                ScrollbarAxis::Vertical => this.flex_col().overflow_y_scroll(),
                ScrollbarAxis::Horizontal => this.flex_row().overflow_x_scroll(),
                // 横排时交叉轴默认拉伸，内容会被压成视口高，纵向就量不出溢出。按内容自己的
                // 高度排，`min_size_full` 仍保证至少铺满视口。
                ScrollbarAxis::Both => this.items_start().overflow_scroll(),
            })
            .lock_scroll_axis()
            .child(content);

        // 保持滚动区域在普通布局流中，使自动尺寸祖先仍能按内容增长。
        div()
            .id(root_id)
            .size_full()
            .refine_style(&root_style)
            .relative()
            .flex()
            .map(|this| match self.axis {
                ScrollbarAxis::Horizontal => this.flex_row(),
                ScrollbarAxis::Vertical | ScrollbarAxis::Both => this.flex_col(),
            })
            .child(WheelArea::new(
                (self.id.clone(), "wheel"),
                &scroll_handle,
                self.axis,
                scroll_area,
            ))
            .child(render_scrollbar(
                scrollbar_id,
                &scroll_handle,
                self.axis,
                window,
                cx,
            ))
            .when(self.axis.has_vertical(), |this| {
                this.child(AxisControl::new(
                    (self.id.clone(), "vertical-control"),
                    &scroll_handle,
                    Axis::Vertical,
                    self.vertical_label.clone(),
                ))
            })
            .when(self.axis.has_horizontal(), |this| {
                this.child(AxisControl::new(
                    (self.id.clone(), "horizontal-control"),
                    &scroll_handle,
                    Axis::Horizontal,
                    self.horizontal_label.clone(),
                ))
            })
    }
}

fn root_style_from<E: Styled>(element: &mut E, axis: ScrollbarAxis) -> StyleRefinement {
    let style = element.style();
    StyleRefinement {
        size: style.size.clone(),
        min_size: style.min_size.clone(),
        max_size: style.max_size.clone(),
        flex_grow: style.flex_grow,
        flex_shrink: style.flex_shrink,
        flex_basis: style.flex_basis,
        align_self: style.align_self,
        // 允许 flex 项收缩，又不覆盖调用方显式指定的最小尺寸。
        overflow: PointRefinement {
            x: axis.has_horizontal().then_some(Overflow::Hidden),
            y: axis.has_vertical().then_some(Overflow::Hidden),
        },
        ..Default::default()
    }
}

fn render_scrollbar<H: ScrollbarHandle + Clone>(
    id: impl Into<ElementId>,
    scroll_handle: &H,
    axis: ScrollbarAxis,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    if window.is_inspector_picking(cx) {
        return div();
    }
    div()
        .absolute()
        .inset_0()
        .debug_selector(|| "scrollbar-overlay".to_string())
        .child(Scrollbar::new(scroll_handle).id(id).axis(axis))
}

// 谁拿这次滚轮：捕获阶段从外往里走，每个这次能动的滚动区域都把自己登记上，最后留下的
// 是最里层那个；到了边的不登记，滚轮就交给外层。真正去滚放在冒泡阶段最后，里面别的
// 滚动区域（gpui-base 的输入框等）先滚，滚了就拦下事件，外层不再动。外层自己那块原生
// 滚动区域在这之前会顺手挪一下，这里写的是捕获时算好的位置，会盖掉它。
#[derive(Default)]
struct WheelOwner(Option<HitboxId>);
impl Global for WheelOwner {}

#[derive(Default)]
struct WheelState {
    gesture: OngoingScroll,
    target: Option<Point<Pixels>>,
}

struct WheelArea {
    id: ElementId,
    handle: ScrollHandle,
    axis: ScrollbarAxis,
    child: AnyElement,
}

impl WheelArea {
    fn new(
        id: impl Into<ElementId>,
        handle: &ScrollHandle,
        axis: ScrollbarAxis,
        child: impl IntoElement,
    ) -> Self {
        Self {
            id: id.into(),
            handle: handle.clone(),
            axis,
            child: child.into_any_element(),
        }
    }
}

impl Element for WheelArea {
    type RequestLayoutState = ();
    type PrepaintState = (Hitbox, Rc<RefCell<WheelState>>);
    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }
    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let state = window.with_element_state(
            id.expect("wheel area has an id"),
            |state: Option<Rc<RefCell<WheelState>>>, _| {
                let state = state.unwrap_or_default();
                (state.clone(), state)
            },
        );
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        self.child.prepaint(window, cx);
        (hitbox, state)
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (hitbox, state) = prepaint;
        let hitbox_id = hitbox.id;
        let state_for_claim = state.clone();
        let handle = self.handle.clone();
        let axis = self.axis;
        window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture {
                return;
            }
            let mut state = state_for_claim.borrow_mut();
            state.target = None;
            if !hitbox_id.should_handle_scroll(window) {
                return;
            }
            let mut delta = event.delta.pixel_delta(window.line_height());
            if !delta.x.as_f32().is_finite() || !delta.y.as_f32().is_finite() {
                state.target = None;
                return;
            }
            if event.delta.precise() {
                state.gesture.filter(&mut delta, event.touch_phase);
            }
            if axis == ScrollbarAxis::Horizontal
                && let ScrollDelta::Lines(lines) = event.delta
                && lines.x == 0.
            {
                delta.x = delta.y;
                delta.y = px(0.);
            }
            if delta.x.abs() > delta.y.abs() {
                delta.y = px(0.);
            } else {
                delta.x = px(0.);
            }
            let before = handle.offset();
            let max = handle.max_offset();
            let mut target = before;
            if axis.has_horizontal() {
                target.x = (before.x + delta.x).clamp(-max.x, px(0.));
            }
            if axis.has_vertical() {
                target.y = (before.y + delta.y).clamp(-max.y, px(0.));
            }
            state.target = (target != before).then_some(target);
            if state.target.is_some() {
                cx.default_global::<WheelOwner>().0 = Some(hitbox_id);
            }
        });
        // 冒泡阶段按登记的倒序走，这个在子元素之前登记，所以等里面的都走完才轮到它。
        // 里面的输入框这类区域滚了会拦下事件，到不了这里，页面就不跟着动。
        let handle = self.handle.clone();
        let state = state.clone();
        let view = window.current_view();
        window.on_mouse_event(move |_: &ScrollEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || !hitbox_id.should_handle_scroll(window)
                || cx.try_global::<WheelOwner>().and_then(|owner| owner.0) != Some(hitbox_id)
            {
                return;
            }
            if let Some(target) = state.borrow_mut().target.take() {
                handle.set_offset(target);
                cx.notify(view);
                cx.stop_propagation();
            }
        });
        self.child.paint(window, cx);
    }
}

// The div handles focus and events; the wrapper writes values during prepaint,
// after the preceding scroll area has updated its viewport and range.
struct AxisControl {
    inner: Stateful<Div>,
    handle: ScrollHandle,
    axis: Axis,
    label: SharedString,
    /// 画这根条的视图。滚动后要让它重画，可视图只有在画的时候才问得到，所以 prepaint 时
    /// 记下来，按键和无障碍动作里用。
    view: Rc<Cell<Option<EntityId>>>,
}

impl AxisControl {
    fn new(
        id: impl Into<ElementId>,
        handle: &ScrollHandle,
        axis: Axis,
        label: SharedString,
    ) -> Self {
        use gpui::accesskit::{Action, ActionData};
        let view: Rc<Cell<Option<EntityId>>> = Rc::default();
        let notify = {
            let view = view.clone();
            move |cx: &mut App| {
                if let Some(view) = view.get() {
                    cx.notify(view);
                }
            }
        };
        let keyboard_notify = notify.clone();
        let increment_notify = notify.clone();
        let decrement_notify = notify.clone();
        let set_value_notify = notify;
        let keyboard_handle = handle.clone();
        let increment = handle.clone();
        let decrement = handle.clone();
        let set_value = handle.clone();
        let inner = div()
            .id(id)
            .absolute()
            .map(|this| match axis {
                Axis::Vertical => this.right_0().top_0().h_full().w(px(12.)),
                Axis::Horizontal => this.left_0().bottom_0().w_full().h(px(12.)),
            })
            .focusable()
            .tab_stop(true)
            .focus_visible(|style| style.shadow(crate::button::focus_ring()))
            .on_key_down(move |event, _, cx| {
                if event.keystroke.modifiers != gpui::Modifiers::default() {
                    return;
                }
                let key = event.keystroke.key.as_str();
                let page = axis_viewport(&keyboard_handle, axis);
                let target = match (axis, key) {
                    (Axis::Vertical, "up") | (Axis::Horizontal, "left") => {
                        Some(ScrollCommand::By(-px(40.)))
                    }
                    (Axis::Vertical, "down") | (Axis::Horizontal, "right") => {
                        Some(ScrollCommand::By(px(40.)))
                    }
                    (_, "pageup") => Some(ScrollCommand::By(-page)),
                    (_, "pagedown") => Some(ScrollCommand::By(page)),
                    (_, "home") => Some(ScrollCommand::To(px(0.))),
                    (_, "end") => Some(ScrollCommand::To(axis_max(&keyboard_handle, axis))),
                    _ => None,
                };
                if let Some(command) = target {
                    scroll_axis(&keyboard_handle, axis, command);
                    keyboard_notify(cx);
                    cx.stop_propagation();
                }
            })
            .on_a11y_action(Action::Increment, move |_, _, cx| {
                scroll_axis(&increment, axis, ScrollCommand::By(px(40.)));
                increment_notify(cx);
            })
            .on_a11y_action(Action::Decrement, move |_, _, cx| {
                scroll_axis(&decrement, axis, ScrollCommand::By(-px(40.)));
                decrement_notify(cx);
            })
            .on_a11y_action(Action::SetValue, move |data, _, cx| {
                let value = match data {
                    Some(ActionData::NumericValue(value)) => Some(*value),
                    Some(ActionData::Value(value)) => value.parse::<f64>().ok(),
                    _ => None,
                };
                if let Some(value) = value.filter(|value| value.is_finite()) {
                    let max = f64::from(axis_max(&set_value, axis));
                    scroll_axis(
                        &set_value,
                        axis,
                        ScrollCommand::To(px(value.clamp(0., max) as f32)),
                    );
                    set_value_notify(cx);
                }
            });
        Self {
            inner,
            handle: handle.clone(),
            axis,
            label,
            view,
        }
    }
}

enum ScrollCommand {
    By(Pixels),
    To(Pixels),
}

fn axis_viewport(handle: &ScrollHandle, axis: Axis) -> Pixels {
    match axis {
        Axis::Vertical => handle.bounds().size.height,
        Axis::Horizontal => handle.bounds().size.width,
    }
}

fn axis_max(handle: &ScrollHandle, axis: Axis) -> Pixels {
    match axis {
        Axis::Vertical => handle.max_offset().y,
        Axis::Horizontal => handle.max_offset().x,
    }
    .max(px(0.))
}

fn scroll_axis(handle: &ScrollHandle, axis: Axis, command: ScrollCommand) {
    let mut offset = handle.offset();
    let position = match axis {
        Axis::Vertical => &mut offset.y,
        Axis::Horizontal => &mut offset.x,
    };
    let current = -*position;
    let target = match command {
        ScrollCommand::By(delta) => current + delta,
        ScrollCommand::To(value) => value,
    };
    *position = -target.clamp(px(0.), axis_max(handle, axis));
    handle.set_offset(offset);
}

impl Element for AxisControl {
    type RequestLayoutState = <Stateful<Div> as Element>::RequestLayoutState;
    type PrepaintState = <Stateful<Div> as Element>::PrepaintState;

    fn id(&self) -> Option<ElementId> {
        Element::id(&self.inner)
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn a11y_role(&self) -> Option<gpui::accesskit::Role> {
        Some(gpui::accesskit::Role::ScrollBar)
    }
    fn write_a11y_info(&self, node: &mut gpui::accesskit::Node) {
        use gpui::accesskit::{Action, Orientation};
        let position = match self.axis {
            Axis::Vertical => -self.handle.offset().y,
            Axis::Horizontal => -self.handle.offset().x,
        };
        let max = axis_max(&self.handle, self.axis);
        node.set_label(self.label.to_string());
        node.set_orientation(match self.axis {
            Axis::Vertical => Orientation::Vertical,
            Axis::Horizontal => Orientation::Horizontal,
        });
        node.set_min_numeric_value(0.);
        node.set_max_numeric_value(f64::from(max));
        node.set_numeric_value(f64::from(position.clamp(px(0.), max)));
        node.set_numeric_value_step(40.);
        node.add_action(Action::Increment);
        node.add_action(Action::Decrement);
        node.add_action(Action::SetValue);
    }
    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.inner.request_layout(id, inspector, window, cx)
    }
    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.view.set(Some(window.current_view()));
        self.inner
            .prepaint(id, inspector, bounds, layout, window, cx)
    }
    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.inner
            .paint(id, inspector, bounds, layout, prepaint, window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        Context, IntoElement, ListAlignment, ListState, Modifiers, MouseButton, Render,
        ScrollDelta, ScrollEvent, ScrollHandle, TestAppContext, VisualTestContext, Window, div,
        list, point, px,
    };

    struct Area {
        scroll: ScrollHandle,
        mode: ScrollbarMode,
        content: f32,
    }

    impl Render for Area {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .relative()
                .size(px(100.))
                .child(
                    div()
                        .id("area")
                        .size_full()
                        .overflow_scroll()
                        .track_scroll(&self.scroll)
                        .child(div().size(px(self.content)).flex_none()),
                )
                .child(Scrollbar::new(&self.scroll).id("bar").mode(self.mode))
        }
    }

    fn area(
        cx: &mut TestAppContext,
        mode: ScrollbarMode,
        content: f32,
    ) -> (&mut VisualTestContext, ScrollHandle) {
        let scroll = ScrollHandle::default();
        let (_, cx) = cx.add_window_view({
            let scroll = scroll.clone();
            move |_, _| Area {
                scroll,
                mode,
                content,
            }
        });
        draw(cx);
        (cx, scroll)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    #[gpui::test]
    fn both_axes_drive_the_tracked_area(cx: &mut TestAppContext) {
        let (cx, scroll) = area(cx, ScrollbarMode::Always, 500.);
        assert!(scroll.max_offset().x > px(0.));
        assert!(scroll.max_offset().y > px(0.));

        cx.simulate_click(point(px(80.), px(95.)), Modifiers::default());
        assert!(scroll.offset().x < px(0.));
        assert_eq!(scroll.offset().y, px(0.));
        draw(cx);
        cx.simulate_click(point(px(95.), px(80.)), Modifiers::default());
        assert!(scroll.offset().y < px(0.));
    }

    #[gpui::test]
    fn dragging_the_thumb_moves_the_tracked_area(cx: &mut TestAppContext) {
        let (cx, scroll) = area(cx, ScrollbarMode::Always, 500.);
        cx.simulate_mouse_down(
            point(px(95.), px(20.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        draw(cx);
        cx.simulate_mouse_move(
            point(px(95.), px(80.)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            point(px(95.), px(80.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        assert!(scroll.offset().y < px(0.));
        assert_eq!(scroll.offset().x, px(0.));
    }

    #[gpui::test]
    fn wheel_scrolls_the_area_and_reveals_scrolling_mode(cx: &mut TestAppContext) {
        let (cx, scroll) = area(cx, ScrollbarMode::Scrolling, 500.);
        cx.simulate_event(ScrollEvent {
            position: point(px(40.), px(40.)),
            delta: ScrollDelta::Pixels(point(px(0.), px(-50.))),
            ..Default::default()
        });
        draw(cx);
        let before = scroll.offset().y;
        assert!(before < px(0.));
        cx.simulate_click(point(px(95.), px(80.)), Modifiers::default());
        assert!(scroll.offset().y < before);
    }

    #[gpui::test]
    fn hover_mode_reveals_the_hidden_track(cx: &mut TestAppContext) {
        let (cx, scroll) = area(cx, ScrollbarMode::Hover, 500.);
        cx.simulate_click(point(px(95.), px(80.)), Modifiers::default());
        assert_eq!(scroll.offset(), point(px(0.), px(0.)));
        cx.simulate_mouse_move(point(px(95.), px(50.)), None, Modifiers::default());
        draw(cx);
        cx.simulate_click(point(px(95.), px(80.)), Modifiers::default());
        assert!(scroll.offset().y < px(0.));
    }

    #[gpui::test]
    fn no_overflow_has_no_scroll_range(cx: &mut TestAppContext) {
        let (cx, scroll) = area(cx, ScrollbarMode::Always, 80.);
        cx.simulate_click(point(px(95.), px(80.)), Modifiers::default());
        assert_eq!(scroll.offset(), point(px(0.), px(0.)));
        assert_eq!(scroll.max_offset(), point(px(0.), px(0.)));
    }

    fn wheel(cx: &mut VisualTestContext, x: f32, y: f32, delta: ScrollDelta) {
        cx.simulate_event(ScrollEvent {
            position: point(px(x), px(y)),
            delta,
            ..Default::default()
        });
        draw(cx);
    }

    struct Wrapped {
        scroll: ScrollHandle,
        axis: ScrollbarAxis,
        content: f32,
    }

    impl Render for Wrapped {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .flex()
                .flex_col()
                .size(px(140.))
                .child(div().h(px(20.)).flex_none())
                .child(Scrollable::new(
                    "wrapped",
                    div().flex().flex_col().size(px(100.)).child(
                        div()
                            .size(px(self.content))
                            .flex_none()
                            .debug_selector(|| "wrapped-content".to_string()),
                    ),
                    &self.scroll,
                    self.axis,
                ))
        }
    }

    fn wrapped(
        cx: &mut TestAppContext,
        axis: ScrollbarAxis,
        content: f32,
    ) -> (&mut VisualTestContext, ScrollHandle) {
        cx.update(|cx| {
            gpui_base::Theme::global_mut(cx).scrollbar =
                gpui_base::ScrollbarTheme::new().with_mode(ScrollbarMode::Always);
        });
        let scroll = ScrollHandle::default();
        let (_, cx) = cx.add_window_view({
            let scroll = scroll.clone();
            move |_, _| Wrapped {
                scroll,
                axis,
                content,
            }
        });
        draw(cx);
        (cx, scroll)
    }

    #[gpui::test]
    fn wrapper_wheel_then_drag_keeps_bar_at_viewport(cx: &mut TestAppContext) {
        let (cx, scroll) = wrapped(cx, ScrollbarAxis::Vertical, 500.);
        assert_eq!(scroll.bounds().top(), px(20.));
        assert_eq!(scroll.bounds().size.height, px(100.));
        wheel(cx, 40., 50., ScrollDelta::Pixels(point(px(0.), px(-100.))));
        assert_eq!(scroll.offset().y, px(-100.));
        // At offset 100 the thumb remains at the viewport, not at the
        // translated content's origin. Drag it to the bottom of the track.
        cx.simulate_mouse_down(
            point(px(95.), px(40.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        draw(cx);
        cx.simulate_mouse_move(
            point(px(95.), px(115.)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            point(px(95.), px(115.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        draw(cx);
        assert!(scroll.offset().y < px(-100.));
        assert!(scroll.offset().y >= -scroll.max_offset().y);
    }

    #[gpui::test]
    fn wrapper_clamps_at_both_edges_and_preserves_other_axis(cx: &mut TestAppContext) {
        let (cx, scroll) = wrapped(cx, ScrollbarAxis::Both, 500.);
        wheel(cx, 40., 50., ScrollDelta::Pixels(point(px(-80.), px(0.))));
        let x = scroll.offset().x;
        assert!(x < px(0.));
        wheel(cx, 40., 50., ScrollDelta::Pixels(point(px(0.), px(-2000.))));
        assert_eq!(scroll.offset().y, -scroll.max_offset().y);
        assert_eq!(scroll.offset().x, x);
        wheel(cx, 40., 50., ScrollDelta::Pixels(point(px(0.), px(2000.))));
        assert_eq!(scroll.offset().y, px(0.));
    }

    #[gpui::test]
    fn horizontal_wrapper_accepts_trackpad_and_mouse_wheel(cx: &mut TestAppContext) {
        let (cx, scroll) = wrapped(cx, ScrollbarAxis::Horizontal, 500.);
        wheel(cx, 40., 50., ScrollDelta::Pixels(point(px(-30.), px(0.))));
        let first = scroll.offset().x;
        assert!(first < px(0.));
        wheel(cx, 40., 50., ScrollDelta::Lines(point(0., -1.)));
        assert!(scroll.offset().x < first);
        assert_eq!(scroll.offset().y, px(0.));
    }

    #[gpui::test]
    fn keyboard_operates_each_axis_and_clamps(cx: &mut TestAppContext) {
        let (cx, scroll) = wrapped(cx, ScrollbarAxis::Both, 500.);
        cx.update(|window, cx| window.focus_next(cx));
        cx.simulate_keystrokes("down");
        assert_eq!(scroll.offset().y, px(-40.));
        cx.simulate_keystrokes("end");
        assert_eq!(scroll.offset().y, -scroll.max_offset().y);
        cx.simulate_keystrokes("home");
        assert_eq!(scroll.offset().y, px(0.));
        cx.update(|window, cx| window.focus_next(cx));
        cx.simulate_keystrokes("right");
        assert_eq!(scroll.offset().x, px(-40.));
        cx.simulate_keystrokes("pagedown");
        assert_eq!(scroll.offset().x, px(-140.));
        assert_eq!(scroll.offset().y, px(0.));
    }

    #[gpui::test]
    fn content_shrink_clamps_the_callers_handle(cx: &mut TestAppContext) {
        let scroll = ScrollHandle::default();
        let (view, cx) = cx.add_window_view({
            let scroll = scroll.clone();
            move |_, _| Wrapped {
                scroll,
                axis: ScrollbarAxis::Vertical,
                content: 500.,
            }
        });
        draw(cx);
        wheel(cx, 40., 50., ScrollDelta::Pixels(point(px(0.), px(-2000.))));
        assert!(scroll.offset().y < px(0.));
        view.update(cx, |view, cx| {
            view.content = 50.;
            cx.notify();
        });
        draw(cx);
        assert_eq!(scroll.max_offset().y, px(0.));
        assert_eq!(scroll.offset().y, px(0.));
    }

    struct Nested {
        outer: ScrollHandle,
        inner: ScrollHandle,
    }

    impl Render for Nested {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            Scrollable::new(
                "outer",
                div()
                    .flex()
                    .flex_col()
                    .size(px(100.))
                    .child(Scrollable::new(
                        "inner",
                        div().size(px(80.)).child(div().h(px(300.)).flex_none()),
                        &self.inner,
                        ScrollbarAxis::Vertical,
                    ))
                    .child(div().h(px(300.)).flex_none()),
                &self.outer,
                ScrollbarAxis::Vertical,
            )
        }
    }

    #[gpui::test]
    fn nested_wheel_moves_innermost_then_parent_at_boundary(cx: &mut TestAppContext) {
        let outer = ScrollHandle::default();
        let inner = ScrollHandle::default();
        let (_, cx) = cx.add_window_view({
            let outer = outer.clone();
            let inner = inner.clone();
            move |_, _| Nested { outer, inner }
        });
        draw(cx);
        wheel(cx, 30., 30., ScrollDelta::Pixels(point(px(0.), px(-40.))));
        assert!(inner.offset().y < px(0.));
        assert_eq!(outer.offset().y, px(0.));
        inner.set_offset(point(px(0.), -inner.max_offset().y));
        draw(cx);
        wheel(cx, 30., 30., ScrollDelta::Pixels(point(px(0.), px(-40.))));
        assert!(outer.offset().y < px(0.));
    }

    struct TextAreaInPage {
        outer: ScrollHandle,
        text: gpui::Entity<crate::field::TextareaState>,
    }

    impl Render for TextAreaInPage {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            Scrollable::new(
                "page",
                div()
                    .flex()
                    .flex_col()
                    .w(px(200.))
                    .child(crate::field::TextArea::new(&self.text))
                    .child(div().h(px(600.)).flex_none()),
                &self.outer,
                ScrollbarAxis::Vertical,
            )
            .h(px(200.))
        }
    }

    /// 设置页里内容装不下的多行输入框：滚轮滚它自己，页面不动。
    #[gpui::test]
    fn text_area_inside_takes_the_wheel_before_the_page(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            crate::init(cx);
        });
        let outer = ScrollHandle::default();
        let (view, cx) = cx.add_window_view({
            let outer = outer.clone();
            move |window, cx| TextAreaInPage {
                outer,
                text: gpui::AppContext::new(cx, |cx| {
                    crate::field::TextareaState::new(window, cx)
                        .auto_grow(1, 3)
                        .default_value((1..=40).map(|n| format!("line {n}\n")).collect::<String>())
                }),
            }
        });
        draw(cx);
        draw(cx);
        wheel(cx, 40., 20., ScrollDelta::Pixels(point(px(0.), px(-30.))));
        let text = cx.update(|_, cx| view.read(cx).text.read(cx).scroll_offset());
        assert!(text.y < px(0.), "the text area must scroll, got {text:?}");
        assert_eq!(outer.offset().y, px(0.), "the page must stay");
    }

    #[gpui::test]
    fn axis_accessibility_reports_current_range_and_value(cx: &mut TestAppContext) {
        let (cx, scroll) = wrapped(cx, ScrollbarAxis::Vertical, 500.);
        wheel(cx, 40., 50., ScrollDelta::Pixels(point(px(0.), px(-40.))));
        let control = AxisControl::new("test-control", &scroll, Axis::Vertical, "消息".into());
        let mut node = gpui::accesskit::Node::new(gpui::accesskit::Role::ScrollBar);
        control.write_a11y_info(&mut node);
        assert_eq!(node.numeric_value(), Some(40.));
        assert_eq!(
            node.max_numeric_value(),
            Some(f64::from(scroll.max_offset().y))
        );
        assert_eq!(
            node.orientation(),
            Some(gpui::accesskit::Orientation::Vertical)
        );
        assert!(node.supports_action(gpui::accesskit::Action::SetValue));
    }

    #[gpui::test]
    fn no_overflow_keyboard_and_wheel_stay_at_origin(cx: &mut TestAppContext) {
        let (cx, scroll) = wrapped(cx, ScrollbarAxis::Both, 50.);
        cx.update(|window, cx| window.focus_next(cx));
        cx.simulate_keystrokes("end down");
        wheel(cx, 40., 50., ScrollDelta::Pixels(point(px(0.), px(-100.))));
        assert_eq!(scroll.max_offset(), point(px(0.), px(0.)));
        assert_eq!(scroll.offset(), point(px(0.), px(0.)));
    }

    struct Ledger {
        list: ListState,
    }

    impl Render for Ledger {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .relative()
                .flex()
                .flex_col()
                .w(px(100.))
                .h(px(120.))
                .child(div().h(px(20.)))
                .child(
                    list(self.list.clone(), |_, _, _| {
                        div().h(px(20.)).w_full().into_any_element()
                    })
                    .w_full()
                    .h(px(100.)),
                )
                .child(
                    Scrollbar::vertical(&self.list)
                        .id("ledger-bar")
                        .mode(ScrollbarMode::Always),
                )
        }
    }

    #[gpui::test]
    fn list_scrollbar_uses_list_viewport_below_fixed_header(cx: &mut TestAppContext) {
        let state =
            ListState::new(100, ListAlignment::Top, px(200.)).with_uniform_item_height(px(20.));
        let (_, cx) = cx.add_window_view({
            let state = state.clone();
            move |_, _| Ledger { list: state }
        });
        draw(cx);
        assert_eq!(state.viewport_bounds().top(), px(20.));
        cx.simulate_click(point(px(95.), px(10.)), Modifiers::default());
        assert_eq!(state.scroll_px_offset_for_scrollbar().y, px(0.));
        cx.simulate_click(point(px(95.), px(100.)), Modifiers::default());
        assert!(state.scroll_px_offset_for_scrollbar().y < px(0.));
    }
}
