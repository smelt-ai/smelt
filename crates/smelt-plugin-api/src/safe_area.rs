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
    let areas = &mut cx.default_global::<SafeAreas>().0;
    // 视图放掉了，它那份跟着清掉。
    areas.retain(|_, (view, _)| view.upgrade().is_some());
    let id = view.entity_id();
    let before = areas.get(&id).map(|(_, area)| *area).unwrap_or_default();
    if before == area {
        return;
    }
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
