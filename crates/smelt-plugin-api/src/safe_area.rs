//! 视图的 safe area（[`SafeArea`]）：视图顶上是不是窗口标题栏、哪几个角被占着（上面两个是
//! 宿主控件，下面两个是窗口圆角）。
//!
//! 谁摆视图谁给它设：宿主给页里直接摆的视图设，自己摆子视图的（分屏、容器）按子视图碰到
//! 哪几条边把自己的那份分下去（[`SafeArea::child`]）再设给子视图。视图在 render 里问自己的
//! （`safe_area(cx.entity_id(), cx)`），按 [`SafeArea::insets`] 让开，顶上是标题栏时画
//! `ui::window_bar::window_bar`；自己就是一行控件的（贴底的一条）两头按
//! [`SafeArea::bar_insets`] 排。

use std::collections::HashMap;

use gpui::{AnyView, AnyWeakView, App, EntityId, Global};

use crate::HostEvent;

pub use smelt_ui::window_bar::{Adapt, SafeArea};

#[derive(Default)]
struct SafeAreas(HashMap<EntityId, (AnyWeakView, SafeArea)>);

impl Global for SafeAreas {}

/// 给摆着的视图设 safe area。变了重画那个视图，发 [`HostEvent::SafeAreaChanged`]（摆着子视图
/// 的据此重分）。在事件里调，不在 render 里。
pub fn set_safe_area(view: &AnyView, area: SafeArea, cx: &mut App) {
    let id = view.entity_id();
    // 先只读比较：可写地取全局（`default_global`）就算它变了，读过它的视图都要重画、观察者都
    // 收到通知，没变不能碰。
    if safe_area(id, cx) == area {
        return;
    }
    let areas = &mut cx.default_global::<SafeAreas>().0;
    // 视图放掉了，它那份跟着清掉。
    areas.retain(|_, (view, _)| view.upgrade().is_some());
    if area == SafeArea::default() {
        areas.remove(&id);
    } else {
        areas.insert(id, (view.downgrade(), area));
    }
    cx.notify(id);
    crate::emit(HostEvent::SafeAreaChanged, cx);
}

/// 视图的 safe area；没人设过是全零：顶上不是标题栏，没有角被占。只读内存，render 里可以调。
pub fn safe_area(view: EntityId, cx: &App) -> SafeArea {
    cx.try_global::<SafeAreas>()
        .and_then(|areas| areas.0.get(&view))
        .map(|(_, area)| *area)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use gpui::{Context, IntoElement, Render, TestAppContext, Window, div};

    use super::*;

    struct Blank;

    impl Render for Blank {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    /// 没变不碰全局：工作台的布局一通知就把各视图的 safe area 重设一遍，可写地取一下就算
    /// 它变了，所有画顶栏的视图都要跟着重画。
    #[gpui::test]
    fn setting_the_same_area_leaves_the_global_untouched(cx: &mut TestAppContext) {
        let (view, _) = cx.add_window_view(|_, _| Blank);
        let view = AnyView::from(view);
        let writes = std::rc::Rc::new(std::cell::Cell::new(0));
        cx.update(|cx| {
            let writes = writes.clone();
            cx.observe_global::<SafeAreas>(move |_| writes.set(writes.get() + 1))
                .detach();
        });
        cx.update(|cx| set_safe_area(&view, SafeArea::default(), cx));
        assert_eq!(writes.get(), 0, "没设过就是全零，设全零不算变");
        let area = SafeArea {
            title_bar: true,
            ..SafeArea::default()
        };
        cx.update(|cx| set_safe_area(&view, area, cx));
        assert_eq!(writes.get(), 1);
        cx.update(|cx| set_safe_area(&view, area, cx));
        assert_eq!(writes.get(), 1, "和上次一样");
        assert_eq!(cx.update(|cx| safe_area(view.entity_id(), cx)), area);
    }
}
