//! 选中块会滑动的分段控件。能力库顶部那种胶囊分段、工具面板那种下划线页签共用。
//!
//! 轨道是一个自己排版、自己画的元素：先画轨道底色和描边，再画选中块，最后画各项。
//! 选中块要夹在轨道和各项之间，普通 Div 做不到这个层次。
//!
//! 选中块用临界阻尼弹簧追着选中项走：连点时带着当前速度换方向，不会每次从头起步。
//! 位置按轨道内的相对坐标记，轨道跟着滚动、拖窗口整体移动时选中块不会自己动。

use gpui::prelude::*;
use gpui::{
    AlignItems, AnyElement, App, BorderStyle, Bounds, Corners, Display, Edges, Element, ElementId,
    FlexDirection, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, Pixels, Style,
    Window, fill, point, px, quad, size,
};
use gpui_component::ActiveTheme as _;
use std::time::Instant;

use crate::motion::CriticalSpring;
use crate::ui_theme;

/// 选中块离目标不到半个像素就算停稳。
const REST_PX: f32 = 0.5;
/// 下划线粗细，和原来的下划线页签一致。
const UNDERLINE_PX: f32 = 2.;
/// 胶囊轨道的圆角；选中块比它小一圈内边距，两条圆弧才是同心的。
const PILL_TRACK_RADIUS: f32 = 8.;
const PILL_TRACK_PADDING: f32 = 2.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentedVariant {
    /// 描边圆角轨道里一块实心选中块。几个互斥选项并排时用。
    Pill,
    /// 透明轨道，选中项下面一道横线。一排页签用。
    Underline,
}

pub struct SegmentedTrack {
    id: ElementId,
    variant: SegmentedVariant,
    selected: Option<usize>,
    items: Vec<AnyElement>,
}

/// `selected` 是 `None` 时不画选中块，比如工具面板收起时。
pub fn segmented_track(
    id: impl Into<ElementId>,
    variant: SegmentedVariant,
    selected: Option<usize>,
) -> SegmentedTrack {
    SegmentedTrack {
        id: id.into(),
        variant,
        selected,
        items: Vec::new(),
    }
}

impl SegmentedTrack {
    pub fn child(mut self, item: impl IntoElement) -> Self {
        self.items.push(item.into_any_element());
        self
    }

    pub fn children(mut self, items: impl IntoIterator<Item = impl IntoElement>) -> Self {
        self.items
            .extend(items.into_iter().map(IntoElement::into_any_element));
        self
    }
}

/// 一个方向上的弹簧：目标、出发时相对目标的位移和速度、出发时刻。
#[derive(Clone, Copy)]
struct Glide {
    target: f32,
    displacement: f32,
    velocity: f32,
    started_at: Instant,
}

impl Glide {
    fn at(value: f32, now: Instant) -> Self {
        Self {
            target: value,
            displacement: 0.,
            velocity: 0.,
            started_at: now,
        }
    }

    fn sample(&self, now: Instant) -> (f32, f32) {
        let t = now.saturating_duration_since(self.started_at).as_secs_f32();
        let (offset, velocity) = CriticalSpring::SNAP.sample(self.displacement, self.velocity, t);
        (self.target + offset, velocity)
    }

    /// 换目标时从当前位置、带着当前速度出发。
    fn retarget(&mut self, target: f32, now: Instant) {
        if (target - self.target).abs() < REST_PX {
            return;
        }
        let (value, velocity) = self.sample(now);
        *self = Self {
            target,
            displacement: value - target,
            velocity,
            started_at: now,
        };
    }

    fn settled(&self, now: Instant) -> bool {
        let (value, velocity) = self.sample(now);
        CriticalSpring::SNAP.is_settled(value - self.target, velocity, REST_PX)
    }
}

/// 选中块的左沿（相对轨道）和宽度。
struct ThumbState {
    left: Glide,
    width: Glide,
}

pub struct ThumbPaint {
    bounds: Option<Bounds<Pixels>>,
}

impl IntoElement for SegmentedTrack {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SegmentedTrack {
    type RequestLayoutState = Vec<LayoutId>;
    type PrepaintState = ThumbPaint;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let children: Vec<LayoutId> = self
            .items
            .iter_mut()
            .map(|item| item.request_layout(window, cx))
            .collect();
        let mut style = Style::default();
        style.display = Display::Flex;
        style.flex_direction = FlexDirection::Row;
        style.align_items = Some(AlignItems::Center);
        style.flex_shrink = 0.;
        match self.variant {
            SegmentedVariant::Pill => {
                style.gap = size(px(4.).into(), px(0.).into());
                style.padding = Edges::all(px(PILL_TRACK_PADDING).into());
                style.border_widths = Edges::all(px(1.).into());
            }
            SegmentedVariant::Underline => {
                // 和 gpui-component 的 XSmall 下划线页签同高同间距，替换后这一行不跳。
                style.gap = size(px(10.).into(), px(0.).into());
                style.size.height = px(26.).into();
            }
        }
        (
            window.request_layout(style, children.iter().copied(), cx),
            children,
        )
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        children: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        for item in &mut self.items {
            item.prepaint(window, cx);
        }
        let selected = self
            .selected
            .and_then(|ix| children.get(ix))
            .map(|layout_id| window.layout_bounds(*layout_id));
        let now = cx.background_executor().now();
        let animate = !cx.reduce_motion();
        let id = id.expect("分段轨道必须有稳定 id");
        // 元素状态存 `Option<ThumbState>`：没有选中项时清空，下次选中从那里直接出现。
        let (thumb, running) =
            window.with_element_state(id, |state: Option<Option<ThumbState>>, _| {
                let state = state.flatten();
                let Some(item) = selected else {
                    // 没有选中项就不画；下次选中时从那里直接出现，不从上一次的位置飞过来。
                    return ((None, false), None);
                };
                let left = f32::from(item.left() - bounds.left());
                let width = f32::from(item.size.width);
                let state = match state {
                    Some(mut state) if animate => {
                        state.left.retarget(left, now);
                        state.width.retarget(width, now);
                        state
                    }
                    _ => ThumbState {
                        left: Glide::at(left, now),
                        width: Glide::at(width, now),
                    },
                };
                let running = !state.left.settled(now) || !state.width.settled(now);
                let (left, width) = if running {
                    (state.left.sample(now).0, state.width.sample(now).0.max(0.))
                } else {
                    (state.left.target, state.width.target)
                };
                let thumb = match self.variant {
                    SegmentedVariant::Pill => Bounds::new(
                        point(bounds.left() + px(left), item.top()),
                        size(px(width), item.size.height),
                    ),
                    SegmentedVariant::Underline => Bounds::new(
                        point(bounds.left() + px(left), bounds.bottom() - px(UNDERLINE_PX)),
                        size(px(width), px(UNDERLINE_PX)),
                    ),
                };
                ((Some(thumb), running), Some(state))
            });
        if running {
            window.request_animation_frame();
        }
        ThumbPaint { bounds: thumb }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _children: &mut Self::RequestLayoutState,
        thumb: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        match self.variant {
            SegmentedVariant::Pill => {
                window.paint_quad(quad(
                    bounds,
                    Corners::all(px(PILL_TRACK_RADIUS)),
                    Hsla::from(gpui::rgb(ui_theme::bg_column())),
                    Edges::all(px(1.)),
                    Hsla::from(gpui::rgb(ui_theme::border_loud())),
                    BorderStyle::default(),
                ));
                if let Some(thumb) = thumb.bounds {
                    window.paint_quad(
                        fill(thumb, Hsla::from(gpui::rgb(ui_theme::bg_selected())))
                            .corner_radii(Corners::all(px(PILL_TRACK_RADIUS - PILL_TRACK_PADDING))),
                    );
                }
            }
            SegmentedVariant::Underline => {
                if let Some(thumb) = thumb.bounds {
                    window.paint_quad(
                        fill(thumb, cx.theme().primary)
                            .corner_radii(Corners::all(px(UNDERLINE_PX / 2.))),
                    );
                }
            }
        }
        for item in &mut self.items {
            item.paint(window, cx);
        }
    }
}

/// 胶囊轨道里的一格。选中只换字色，底色由轨道的选中块负责；自己再铺底色会和滑块叠成两层。
pub fn pill_segment(
    id: impl Into<ElementId>,
    label: impl Into<gpui::SharedString>,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    gpui::div()
        .id(id)
        .h(ui_theme::height::xs())
        .px_2p5()
        .flex()
        .items_center()
        .rounded(px(PILL_TRACK_RADIUS - PILL_TRACK_PADDING))
        .cursor_pointer()
        .text_sm()
        .text_color(gpui::rgb(if selected {
            ui_theme::text_bright()
        } else {
            ui_theme::text_muted()
        }))
        .when(!selected, |segment| {
            segment.hover(|segment| segment.bg(ui_theme::overlay(0x14)))
        })
        .child(label.into())
}

/// 下划线页签里的一格。字号、字色和 gpui-component 的 XSmall 下划线页签一致。
pub fn underline_segment(
    id: impl Into<ElementId>,
    label: impl Into<gpui::SharedString>,
    selected: bool,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme();
    let active = theme.tab_active_foreground;
    gpui::div()
        .id(id)
        .h(px(20.))
        .flex()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .text_xs()
        .text_color(if selected {
            active
        } else {
            theme.tab_foreground
        })
        .hover(move |segment| segment.text_color(active))
        .child(label.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn thumb_glides_to_the_new_segment_and_stops_there() {
        let start = Instant::now();
        let mut left = Glide::at(0., start);
        left.retarget(80., start);
        let (midway, _) = left.sample(start + Duration::from_millis(40));
        assert!(midway > 0. && midway < 80., "40ms 时应在半路：{midway}");
        let end = start + Duration::from_millis(400);
        assert!(left.settled(end));
        assert!((left.sample(end).0 - 80.).abs() < REST_PX);
    }

    #[test]
    fn clicking_another_segment_mid_glide_keeps_the_motion_continuous() {
        let start = Instant::now();
        let mut left = Glide::at(0., start);
        left.retarget(160., start);
        let turn = start + Duration::from_millis(30);
        let (position, velocity) = left.sample(turn);
        assert!(velocity > 0.);

        left.retarget(40., turn);
        let (after_position, after_velocity) = left.sample(turn);
        assert!(
            (after_position - position).abs() < 0.01,
            "换目标那一刻不能跳"
        );
        assert!(
            (after_velocity - velocity).abs() < 0.01,
            "换目标那一刻速度不能突变：{velocity} → {after_velocity}"
        );
    }

    #[test]
    fn retargeting_to_the_same_place_does_not_restart() {
        let start = Instant::now();
        let mut left = Glide::at(0., start);
        left.retarget(50., start);
        let later = start + Duration::from_millis(60);
        let before = left.sample(later);
        left.retarget(50.2, later);
        assert_eq!(left.sample(later), before, "半像素以内的变化不算换目标");
    }
}
