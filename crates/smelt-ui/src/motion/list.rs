//! 列表的动画零件：一组的开合（[`disclosure_open`] + [`fold_body`]）、新的一行撑开落进来
//! （[`arrival_open`]）、铺一层底色闪一下让眼睛找到它（[`wash_alpha`]）、图标亮一下
//! （[`GLOW`]，颜色见 `ui_theme::glow_color`）。重排滑到新位置见
//! [`super::flip_slot`]。
//!
//! 进度都在 render 里按时间取（弹簧、[`replay_elapsed`] 的起点挂在元素状态上），没播完就要下一
//! 帧；画的一方只管按进度画。

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ContentMask, Display, Element, ElementId,
    FlexDirection, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Style,
    Window, px, relative, size,
};
use gpui_base::spring;

use super::ease_snap;
use crate::kit::MOTION;

/// 一组开到哪了（1 全开、0 全合），按 [`MOTION`] 的 `disclosure` 弹簧走。`id` 在一个视图里
/// 跟着那一组的身份走；同一帧里用同一个 `id` 再取（标题行的箭头）得到同一个值。
pub fn disclosure_open(
    id: impl Into<ElementId>,
    collapsed: bool,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    let target = if collapsed { 0. } else { 1. };
    spring(id.into(), target, MOTION.disclosure, window, cx)
}

/// 合上并且停稳了：子行不用建。
pub fn disclosure_settled_closed(collapsed: bool, open: f32) -> bool {
    const SETTLED: f32 = 0.001;
    collapsed && open <= SETTLED
}

/// 按进度 `open`（0 到 1）露出 `child` 的高度：全开时自然布局并记下高度，动画时按记下的高度
/// 裁、按进度缩。一组的子行、新落进来的那一行都用它。
///
/// 全开时不裁：收起第一帧裁剪层刚出现、还没量过高，用的是全开时记下的那个，后面的行不会先
/// 被挤上去；展开着时子行变多也不会被裁掉。
pub struct FoldBody {
    id: ElementId,
    open: f32,
    child: AnyElement,
}

/// `id` 在视图里唯一，量到的高度挂在它上面。
pub fn fold_body(id: impl Into<ElementId>, open: f32, child: impl IntoElement) -> FoldBody {
    FoldBody {
        id: id.into(),
        open: open.clamp(0., 1.),
        child: child.into_any_element(),
    }
}

impl IntoElement for FoldBody {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for FoldBody {
    type RequestLayoutState = Option<LayoutId>;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        if self.open >= 1. - 0.001 {
            style.display = Display::Flex;
            style.flex_direction = FlexDirection::Column;
            let child = self.child.request_layout(window, cx);
            (window.request_layout(style, [child], cx), Some(child))
        } else {
            let height = window.with_element_state(
                id.expect("FoldBody 必须有稳定 id"),
                |height: Option<Pixels>, _| (height, height.unwrap_or(px(0.))),
            );
            style.size.height = (height.unwrap_or(px(0.)) * self.open).into();
            (window.request_layout(style, None, cx), None)
        }
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        child_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let height = if let Some(layout) = child_layout {
            window.layout_bounds(*layout).size.height
        } else {
            self.child
                .layout_as_root(
                    size(
                        AvailableSpace::Definite(bounds.size.width),
                        AvailableSpace::MinContent,
                    ),
                    window,
                    cx,
                )
                .height
        };
        let changed = window.with_element_state(
            id.expect("FoldBody 必须有稳定 id"),
            |previous: Option<Pixels>, _| (previous != Some(height), height),
        );
        if changed && child_layout.is_none() {
            window.request_animation_frame();
        }
        if child_layout.is_some() {
            self.child.prepaint(window, cx);
        } else {
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                self.child.prepaint_at(bounds.origin, window, cx);
            });
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        child_layout: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mask = child_layout.is_none().then_some(ContentMask { bounds });
        window.with_content_mask(mask, |window| self.child.paint(window, cx));
    }
}

/// 新的一行撑开到全高要多久，和浮层入场一样长。
pub const ARRIVAL_OPEN: Duration = Duration::from_millis(240);
/// 铺底：淡入、停一下、再慢慢退掉，到 [`WASH_END`] 退干净。
pub const WASH_RISE: Duration = Duration::from_millis(120);
pub const WASH_HOLD: Duration = Duration::from_millis(360);
pub const WASH_END: Duration = Duration::from_millis(1100);
/// 铺底最浓时的不透明度（铺正文色，深浅主题都看得见，又压不住字）。
pub const WASH_PEAK: f32 = 0.1;
/// 图标亮一下要多久：亮起、再回到原来的颜色。
pub const GLOW: Duration = Duration::from_secs(2);

/// 撑开的进度：前段快、收尾慢，跟浮层入场同一条曲线。
pub fn arrival_open(elapsed: Duration) -> f32 {
    ease_snap(elapsed.as_secs_f32() / ARRIVAL_OPEN.as_secs_f32())
}

/// 铺底的不透明度：一下子铺满会像闪了一下，所以先用一小段淡入，停住让眼睛找到它，再慢慢
/// 退干净。
pub fn wash_alpha(elapsed: Duration) -> f32 {
    let t = elapsed.as_secs_f32();
    let rise = smoothstep(t / WASH_RISE.as_secs_f32());
    let hold = WASH_HOLD.as_secs_f32();
    let fall = 1. - smoothstep((t - hold) / (WASH_END.as_secs_f32() - hold));
    WASH_PEAK * rise.min(fall)
}

fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0., 1.);
    x * x * (3. - 2. * x)
}

/// 一次性动画的起点：第一次见到、或 `serial` 变了，就从现在起。
struct Replay {
    serial: u64,
    started_at: Instant,
    /// 这一次被叫停了（比如应用转到后台，装饰动画不播），同一个 `serial` 不再接着播。
    stopped: bool,
}

/// 一次性动画从起点到现在多久；`stop` 为真时叫停这一次，返回 `None`，之后同一个 `serial`
/// 一直是 `None`。起点挂在 `id` 的元素状态上，在 render 里取。
pub fn replay_elapsed(
    id: impl Into<ElementId>,
    serial: u64,
    stop: bool,
    window: &mut Window,
    cx: &mut App,
) -> Option<Duration> {
    let now = cx.background_executor().now();
    let state = window.use_keyed_state(id.into(), cx, |_, _| Replay {
        serial,
        started_at: now,
        stopped: false,
    });
    state.update(cx, |state, _| {
        if state.serial != serial {
            *state = Replay {
                serial,
                started_at: now,
                stopped: false,
            };
        }
        state.stopped |= stop;
        (!state.stopped).then(|| now.saturating_duration_since(state.started_at))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(debug_assertions)]
    use gpui::{InteractiveElement as _, ParentElement as _, Styled as _, div};

    #[cfg(debug_assertions)]
    struct FoldProbe {
        collapsed: bool,
        body_height: Pixels,
    }

    #[cfg(debug_assertions)]
    impl gpui::Render for FoldProbe {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let open = disclosure_open("probe", self.collapsed, window, cx);
            let body = (!disclosure_settled_closed(self.collapsed, open)).then(|| {
                fold_body(
                    "probe-body",
                    open,
                    div().flex().flex_col().w_full().opacity(open).child(
                        div()
                            .debug_selector(|| "fold-row".into())
                            .flex()
                            .mx(px(8.))
                            .h(self.body_height)
                            .child(div().w(px(40.)).flex_shrink_0()),
                    ),
                )
            });
            div()
                .flex()
                .flex_col()
                .w(px(200.))
                .children(body)
                .child(div().debug_selector(|| "after-fold".into()).h(px(20.)))
        }
    }

    #[cfg(debug_assertions)]
    fn draw(window: &mut gpui::VisualTestContext) {
        window.update(|window, cx| window.draw(cx).clear(cx));
    }

    #[cfg(debug_assertions)]
    fn after(window: &mut gpui::VisualTestContext) -> Pixels {
        window.debug_bounds("after-fold").unwrap().top()
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn closing_an_open_group_keeps_following_rows_in_place_on_the_first_frame(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| cx.set_reduce_motion(false));
        let (view, window) = cx.add_window_view(|_, _| FoldProbe {
            collapsed: false,
            body_height: px(120.),
        });
        draw(window);
        let before = after(window);

        // 展开着的内容变化时，要当帧自然撑高，不能沿用上次量到的高度。
        view.update(window, |view, cx| {
            view.body_height = px(160.);
            cx.notify();
        });
        draw(window);
        let grown = after(window);
        assert_eq!(grown - before, px(40.));

        view.update(window, |view, cx| {
            view.collapsed = true;
            cx.notify();
        });
        draw(window);
        assert_eq!(after(window), grown, "收起第一帧不能先跳到最终位置");

        window.executor().advance_clock(Duration::from_millis(40));
        draw(window);
        let middle = after(window);
        assert!(middle < grown && middle > grown - px(160.));
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn opening_a_closed_group_moves_following_rows_smoothly(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(false));
        let (view, window) = cx.add_window_view(|_, _| FoldProbe {
            collapsed: true,
            body_height: px(120.),
        });
        draw(window);
        let closed = after(window);
        view.update(window, |view, cx| {
            view.collapsed = false;
            cx.notify();
        });
        draw(window);
        assert_eq!(after(window), closed);
        window.executor().advance_clock(Duration::from_millis(40));
        draw(window);
        let middle = after(window);
        assert!(middle > closed && middle < closed + px(120.));
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn folding_keeps_rows_constrained_to_the_list_width(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(false));
        let (view, window) = cx.add_window_view(|_, _| FoldProbe {
            collapsed: false,
            body_height: px(120.),
        });
        draw(window);
        let row = window.debug_bounds("fold-row").unwrap();
        assert_eq!(row.size.width, px(184.));

        view.update(window, |view, cx| {
            view.collapsed = true;
            cx.notify();
        });
        draw(window);
        window.executor().advance_clock(Duration::from_millis(40));
        draw(window);
        let closing = window.debug_bounds("fold-row").unwrap();
        assert_eq!(closing.size.width, row.size.width);
        assert_eq!(closing.left(), row.left());

        window.executor().advance_clock(Duration::from_secs(2));
        draw(window);
        view.update(window, |view, cx| {
            view.collapsed = false;
            cx.notify();
        });
        draw(window);
        window.executor().advance_clock(Duration::from_millis(40));
        draw(window);
        let opening = window.debug_bounds("fold-row").unwrap();
        assert_eq!(opening.size.width, row.size.width);
        assert_eq!(opening.left(), row.left());
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn reversing_a_fold_does_not_jump(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(false));
        let (view, window) = cx.add_window_view(|_, _| FoldProbe {
            collapsed: false,
            body_height: px(120.),
        });
        draw(window);
        let open = after(window);
        view.update(window, |view, cx| {
            view.collapsed = true;
            cx.notify();
        });
        draw(window);
        window.executor().advance_clock(Duration::from_millis(40));
        draw(window);
        let middle = after(window);
        assert!(middle < open && middle > open - px(120.));

        view.update(window, |view, cx| {
            view.collapsed = false;
            cx.notify();
        });
        draw(window);
        assert_eq!(after(window), middle);
        window.executor().advance_clock(Duration::from_secs(2));
        draw(window);
        assert_eq!(after(window), open);
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn reduced_motion_changes_fold_immediately(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, window) = cx.add_window_view(|_, _| FoldProbe {
            collapsed: false,
            body_height: px(120.),
        });
        draw(window);
        let open = after(window);
        view.update(window, |view, cx| {
            view.collapsed = true;
            cx.notify();
        });
        draw(window);
        assert_eq!(after(window), open - px(120.));
        view.update(window, |view, cx| {
            view.collapsed = false;
            cx.notify();
        });
        draw(window);
        assert_eq!(after(window), open);
    }

    #[test]
    fn wash_fades_in_holds_and_leaves_nothing_behind() {
        let at = |ms: u64| wash_alpha(Duration::from_millis(ms));
        assert_eq!(at(0), 0.);
        assert!(at(60) > 0. && at(60) < WASH_PEAK);
        assert_eq!(at(200), WASH_PEAK);
        assert_eq!(at(360), WASH_PEAK);
        assert!(at(700) < WASH_PEAK);
        assert!(at(1100) < 1e-4);
        assert_eq!(at(1500), 0.);
    }

    #[test]
    fn the_row_opens_fully_within_the_overlay_time() {
        assert!(arrival_open(Duration::ZERO) < 1e-4);
        assert!(arrival_open(ARRIVAL_OPEN) > 1. - 1e-4);
        assert_eq!(arrival_open(ARRIVAL_OPEN * 2), 1.);
    }

    #[cfg(debug_assertions)]
    struct ReplayProbe {
        serial: u64,
        stop: bool,
        seen: Option<Duration>,
    }

    #[cfg(debug_assertions)]
    impl gpui::Render for ReplayProbe {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            self.seen = replay_elapsed("replay", self.serial, self.stop, window, cx);
            div()
        }
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn a_replay_restarts_when_the_serial_changes_and_stays_stopped(cx: &mut gpui::TestAppContext) {
        let (view, window) = cx.add_window_view(|_, _| ReplayProbe {
            serial: 1,
            stop: false,
            seen: None,
        });
        let seen = |window: &mut gpui::VisualTestContext| {
            draw(window);
            view.read_with(window, |view, _| view.seen)
        };
        assert_eq!(seen(window), Some(Duration::ZERO));
        window.executor().advance_clock(Duration::from_millis(300));
        assert_eq!(seen(window), Some(Duration::from_millis(300)));

        view.update(window, |view, cx| {
            view.serial = 2;
            cx.notify();
        });
        assert_eq!(seen(window), Some(Duration::ZERO), "换了一次就从头播");

        view.update(window, |view, cx| {
            view.stop = true;
            cx.notify();
        });
        assert_eq!(seen(window), None);
        view.update(window, |view, cx| {
            view.stop = false;
            cx.notify();
        });
        assert_eq!(seen(window), None, "叫停的这一次不再接着播");
        view.update(window, |view, cx| {
            view.serial = 3;
            cx.notify();
        });
        assert_eq!(seen(window), Some(Duration::ZERO));
    }
}
