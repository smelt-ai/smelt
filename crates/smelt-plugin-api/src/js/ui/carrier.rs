//! 只带数据、不画东西的元素：菜单项这类「描述」交给父控件读，不进界面。父控件物化子元素后
//! 用 [`take`] 取回数据；直接摆进界面的话画成一个空 div。

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Window,
};
use gpui_shell::anyhow;

pub(super) struct Carrier<T: 'static>(Option<T>);

impl<T: 'static> Carrier<T> {
    pub(super) fn new(value: T) -> Self {
        Self(Some(value))
    }
}

impl<T: 'static> IntoElement for Carrier<T> {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl<T: 'static> Element for Carrier<T> {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
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
    ) -> (LayoutId, AnyElement) {
        let mut element = gpui::div().into_any_element();
        let id = element.request_layout(window, cx);
        (id, element)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        element: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        element: &mut AnyElement,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

/// 取回子元素带的数据，只能取一次。`expected` 写父控件要的是什么，脚本放错了子元素时报得
/// 出是哪个。
pub(super) fn take<T: 'static>(element: &mut AnyElement, expected: &str) -> anyhow::Result<T> {
    element
        .downcast_mut::<Carrier<T>>()
        .ok_or_else(|| anyhow::anyhow!("expected {expected}"))?
        .0
        .take()
        .ok_or_else(|| anyhow::anyhow!("{expected} was already consumed"))
}
