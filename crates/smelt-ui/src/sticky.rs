//! 吸顶区段：标题加内容，整段跟着滚；标题滚过可视区顶边时停在顶上，区段快滚完时被下一段
//! 顶走。和 CSS 的 `position: sticky; top: 0` 一样，按最近的滚动区域（当前的裁剪范围）吸。
//!
//! 只是一个元素，不要滚动句柄、不记跨帧状态：每一帧 prepaint 时按自己和标题眼下的位置算
//! 标题挪多少，滚动时不晚一帧。停住时内容从标题下沿往下裁，滚上去的行不从透明的标题底下
//! 透出来，也点不到标题底下的行。
//!
//! 只管一层：区段里再放吸顶区段，里面那层会吸到可视区顶边，和外层标题叠在一起。
//!
//! 外面有 [`ScrollAnchors`](crate::scroll_anchors::ScrollAnchors) 时把标题占的高度报给它：
//! 压在标题底下的项算看不见，滚到某一项时也不落到标题底下。

use gpui::{
    AnyElement, App, Bounds, ContentMask, Display, Element, ElementId, FlexDirection,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Style, Window, point, px,
};

/// 标题 `header` 在上、内容 `body` 在下竖着排的一段。
pub struct StickySection {
    header: AnyElement,
    body: AnyElement,
}

impl StickySection {
    pub fn new(header: impl IntoElement, body: impl IntoElement) -> Self {
        Self {
            header: header.into_any_element(),
            body: body.into_any_element(),
        }
    }
}

impl IntoElement for StickySection {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for StickySection {
    /// 标题的布局，prepaint 时取它的位置。
    type RequestLayoutState = LayoutId;
    /// 标题停住时内容的裁剪范围；没停住是 `None`。
    type PrepaintState = Option<ContentMask<Pixels>>;

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
    ) -> (LayoutId, LayoutId) {
        let header = self.header.request_layout(window, cx);
        let body = self.body.request_layout(window, cx);
        let style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            ..Style::default()
        };
        (window.request_layout(style, [header, body], cx), header)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        header: &mut LayoutId,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let header = window.layout_bounds(*header);
        let viewport = window.content_mask().bounds;
        let shift = stuck_shift(
            f32::from(header.top()),
            f32::from(header.bottom()),
            f32::from(bounds.bottom()),
            f32::from(viewport.top()),
        );
        let stuck = if shift > 0. {
            (header.bottom() + px(shift) - viewport.top()).max(px(0.))
        } else {
            px(0.)
        };
        crate::scroll_anchors::report_sticky_header(viewport.top(), header.size.height, stuck, cx);
        let mask = (shift > 0.).then(|| {
            let top = (header.bottom() + px(shift)).min(viewport.bottom());
            ContentMask {
                bounds: Bounds::from_corners(point(viewport.left(), top), viewport.bottom_right()),
            }
        });
        // 内容先、标题后：标题的点击区在上面，画也画在上面。
        window.with_content_mask(mask.clone(), |window| self.body.prepaint(window, cx));
        window.with_element_offset(point(px(0.), px(shift)), |window| {
            self.header.prepaint(window, cx);
        });
        mask
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut LayoutId,
        mask: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_content_mask(mask.clone(), |window| self.body.paint(window, cx));
        self.header.paint(window, cx);
    }
}

/// 标题要往下挪多少：上沿滚过可视区顶边（`viewport_top`）就跟到顶边上，但下沿不出区段底
/// （`section_bottom`），再往下滚就被下一段顶走。没滚过是 0。
fn stuck_shift(header_top: f32, header_bottom: f32, section_bottom: f32, viewport_top: f32) -> f32 {
    (viewport_top - header_top)
        .min(section_bottom - header_bottom)
        .max(0.)
}

#[cfg(test)]
mod tests {
    use super::stuck_shift;

    /// 区段 0..300，标题 0..20。
    #[test]
    fn header_sticks_until_the_section_runs_out() {
        let shift = |viewport_top| stuck_shift(0., 20., 300., viewport_top);
        assert_eq!(shift(-50.), 0., "还没滚到：原位");
        assert_eq!(shift(0.), 0.);
        assert_eq!(shift(120.), 120., "滚过了：停在顶边");
        assert_eq!(shift(280.), 280., "下沿正好碰到区段底");
        assert_eq!(shift(295.), 280., "再往下被顶走");
        assert_eq!(shift(400.), 280.);
    }

    #[test]
    fn a_section_with_nothing_under_the_header_never_sticks() {
        assert_eq!(stuck_shift(0., 20., 20., 100.), 0.);
    }
}
