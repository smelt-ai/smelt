//! 布局方案的前提：布局文件不写尺寸，存下来的就是用户看到的。
//!
//! 用户拖过分隔条以后 `dump` 存档，换一个同样大小的窗口 `load` 回来，
//! 各格画出来的宽度要和存档前一样；存档里记的也要是屏幕上的宽度。

use gpui::{
    AnyView, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, Modifiers, MouseButton, ParentElement as _, Pixels,
    Render, StyleRefinement, Styled as _, TestAppContext, VisualTestContext, Window, div, point,
    px,
};
use gpui_component::dock::{
    BasePanel, DockArea, DockAreaState, DockLayout, Panel, PanelEvent, PanelInfo, panel_handle,
};

const NAMES: [&str; 3] = ["left", "middle", "right"];

struct Probe {
    name: &'static str,
    focus_handle: FocusHandle,
}

impl Probe {
    fn new(name: &'static str, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self {
            name,
            focus_handle: cx.focus_handle(),
        })
    }
}

impl BasePanel for Probe {
    /// 每格一个名字，`load` 才能按名字把它们建回来。
    fn panel_name(&self) -> &'static str {
        self.name
    }
}

impl Panel for Probe {}
impl EventEmitter<PanelEvent> for Probe {}

impl Focusable for Probe {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let name = self.name;
        div().size_full().debug_selector(move || name.into())
    }
}

/// 和工作台一样，把 Dock 当缓存的子视图画。
struct Root {
    area: Entity<DockArea>,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(600.))
            .h(px(200.))
            .child(AnyView::from(self.area.clone()).cached(StyleRefinement::default().size_full()))
    }
}

fn window(cx: &mut TestAppContext) -> (Entity<DockArea>, &mut VisualTestContext) {
    let (root, cx) = cx.add_window_view(|window, cx| Root {
        area: cx.new(|cx| DockArea::new("roundtrip-test", None, window, cx)),
    });
    let area = cx.read(|cx| root.read(cx).area.clone());
    (area, cx)
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    for _ in 0..3 {
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn widths(cx: &mut VisualTestContext) -> Vec<Pixels> {
    NAMES
        .iter()
        .map(|name| {
            cx.debug_bounds(name)
                .unwrap_or_else(|| panic!("{name} 画出来了"))
                .size
                .width
        })
        .collect()
}

fn dump(area: &Entity<DockArea>, cx: &mut VisualTestContext) -> DockAreaState {
    cx.read(|cx| area.read(cx).dump(cx))
}

fn sizes(state: &DockAreaState) -> Vec<Pixels> {
    match &state.center.info {
        PanelInfo::Stack { sizes, .. } => sizes.clone(),
        other => panic!("中间是一个拆分：{other:?}"),
    }
}

/// 差半个像素以内算一样：布局按浮点算，画的时候取整。
fn assert_close(left: &[Pixels], right: &[Pixels], what: &str) {
    let close = left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| (f32::from(*a) - f32::from(*b)).abs() <= 0.5);
    assert!(close, "{what}：{left:?} 和 {right:?}");
}

/// 把左边那条分隔条从原处拖到 `to`。
fn drag_first_handle(to: f32, cx: &mut VisualTestContext) {
    let boundary = cx.debug_bounds("middle").unwrap().left();
    let y = px(100.);
    cx.simulate_mouse_down(
        point(boundary - px(2.), y),
        MouseButton::Left,
        Modifiers::default(),
    );
    // 第一段越过拖动阈值，第二段才真正改尺寸（同 resizable_split_tests）。
    cx.simulate_mouse_move(
        point(boundary + px(10.), y),
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    cx.simulate_mouse_move(
        point(px(to), y),
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    cx.simulate_mouse_up(point(px(to), y), MouseButton::Left, Modifiers::default());
}

#[gpui::test]
fn a_saved_layout_draws_the_same_after_reload(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        for name in NAMES {
            gpui_base::dock::register_panel(cx, name, move |_, _, cx| {
                panel_handle(Probe::new(name, cx))
            });
        }
    });

    // 布局不写尺寸。
    let (area, first) = window(cx);
    first.update(|window, cx| {
        let mut layout = DockLayout::h_split();
        for name in NAMES {
            let tabs = DockLayout::tabs().panel_view(panel_handle(Probe::new(name, cx)), cx);
            layout = layout.child(tabs, None);
        }
        area.update(cx, |area, cx| area.set_center(layout, window, cx));
    });
    draw(first);
    let fresh = widths(first);
    assert_close(&sizes(&dump(&area, first)), &fresh, "没拖过：存档和屏幕");

    drag_first_handle(150., first);
    draw(first);
    let dragged = widths(first);
    assert!(
        (f32::from(dragged[0]) - 150.).abs() <= 1.,
        "拖动生效：{fresh:?} → {dragged:?}"
    );
    let saved = dump(&area, first);
    assert_close(&sizes(&saved), &dragged, "拖过以后：存档和屏幕");

    // 同样大小的新窗口里读回来。
    let (area, second) = window(cx);
    second.update(|window, cx| {
        area.update(cx, |area, cx| area.load(saved.clone(), window, cx))
            .expect("存档读得回来")
    });
    draw(second);
    assert_close(&widths(second), &dragged, "读回来以后：屏幕和存档前一样");
    assert_close(
        &sizes(&dump(&area, second)),
        &sizes(&saved),
        "读回来再存：存档不变",
    );
}
