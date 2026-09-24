//! 状态装饰动画的统一能耗策略。
//!
//! GPUI 的重复动画默认跟随显示器刷新率；即使用 `with_max_fps`，每个动画元素也会
//! 各自创建 timer。Smelt 的根视图较重，多个侧栏呼吸灯或 spinner 错峰唤醒时会让
//! 整窗持续布局、绘制和提交 Metal。状态进入动画只走一次、同窗共用 timer，
//! 到达静态终态后停止唤醒。进行中的 spinner 也走这只时钟，按周期循环，
//! 不跟显示器刷新率。面板开合等交互过渡仍由调用方使用默认动画帧率。

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, EntityId, Global, GlobalElementId, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, Styled as _, Transformation, Window,
    WindowId, percentage,
};
use gpui_component::{Icon, IconName, Sizable as _};
use scheduler::Instant;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    time::Duration,
};

/// 状态提亮、一次性 pulse、spinner 等装饰过渡的最高刷新率。
///
/// 6 FPS 足以表达状态刚刚发生变化，并把短暂过渡的重绘量压低一个数量级。
pub const AMBIENT_ANIMATION_MAX_FPS: f32 = 6.0;

fn ambient_frame_interval() -> Duration {
    Duration::from_secs_f32(1.0 / AMBIENT_ANIMATION_MAX_FPS)
}

/// 状态装饰动画只在用户当前能看到它时运行。
pub const fn ambient_motion_enabled(
    application_active: bool,
    window_active: bool,
    surface_visible: bool,
) -> bool {
    application_active && window_active && surface_visible
}

#[derive(Clone, Copy)]
struct AmbientMotionState {
    application_active: bool,
}

impl Global for AmbientMotionState {}

/// 发布 macOS 应用级激活状态。`Window::is_window_active()` 只表示 key window，在
/// 切换应用或 Space 后可能仍为 true，不能单独决定后台动画是否继续。
pub fn set_ambient_application_active(cx: &mut App, application_active: bool) {
    if cx
        .try_global::<AmbientMotionState>()
        .is_some_and(|state| state.application_active == application_active)
    {
        return;
    }
    cx.set_global(AmbientMotionState { application_active });
}

/// 未接入原生应用状态的独立窗口保持原有行为；Smelt 主应用会在根视图绘制时发布。
pub fn ambient_application_active(cx: &App) -> bool {
    cx.try_global::<AmbientMotionState>()
        .is_none_or(|state| state.application_active)
}

#[derive(Default)]
struct AmbientFrameScheduler {
    pending_targets: RefCell<HashMap<WindowId, HashSet<EntityId>>>,
}

impl Global for AmbientFrameScheduler {}

impl AmbientFrameScheduler {
    /// 同一窗口已有下一帧在途时，后续动画只登记所属视图，不再各自创建 timer。
    /// 返回 true 表示调用方取得 timer 所有权。
    fn claim(&self, window_id: WindowId, target: EntityId) -> bool {
        let mut pending = self.pending_targets.borrow_mut();
        let targets = pending.entry(window_id).or_default();
        let owns_timer = targets.is_empty();
        targets.insert(target);
        owns_timer
    }

    fn take_targets(&self, window_id: WindowId) -> HashSet<EntityId> {
        self.pending_targets
            .borrow_mut()
            .remove(&window_id)
            .unwrap_or_default()
    }
}

fn request_shared_ambient_frame(window: &Window, target: EntityId, cx: &mut App) {
    if !cx.has_global::<AmbientFrameScheduler>() {
        cx.set_global(AmbientFrameScheduler::default());
    }

    let window_id = window.window_handle().window_id();
    if !cx
        .global::<AmbientFrameScheduler>()
        .claim(window_id, target)
    {
        return;
    }

    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(ambient_frame_interval())
            .await;
        cx.update(move |cx| {
            let targets = cx.global::<AmbientFrameScheduler>().take_targets(window_id);
            for target in targets {
                // 目标是动画元素在 prepaint 时所属的响应式视图。普通 entity
                // invalidation 会按布局依赖标脏祖先，但不会像 Window::refresh
                // 那样绕过全部缓存。
                cx.notify(target);
            }
        });
    })
    .detach();
}

fn ambient_transition_phase(duration: Duration, elapsed: Duration) -> (f32, bool) {
    if duration.is_zero() {
        return (1.0, false);
    }
    if elapsed >= duration {
        (1.0, false)
    } else {
        (elapsed.as_secs_f32() / duration.as_secs_f32(), true)
    }
}

/// 进行中的 spinner 按周期折返。`running` 由调用方决定，这里只给角度。
fn ambient_loop_phase(duration: Duration, elapsed: Duration) -> f32 {
    if duration.is_zero() {
        return 0.0;
    }
    let cycle = duration.as_secs_f32();
    (elapsed.as_secs_f32() % cycle) / cycle
}

struct AmbientAnimationState {
    animate: bool,
    started_at: Instant,
}

#[doc(hidden)]
pub struct AmbientAnimationLayoutState {
    element: AnyElement,
    running: bool,
}

/// 动画起点保存在 GPUI 的元素状态中；元素从树上消失时状态随之释放。
pub struct AmbientAnimationElement {
    id: ElementId,
    duration: Duration,
    animate: bool,
    repeat: bool,
    renderer: Box<dyn Fn(f32) -> AnyElement>,
}

impl IntoElement for AmbientAnimationElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for AmbientAnimationElement {
    type RequestLayoutState = AmbientAnimationLayoutState;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let id = id.expect("状态动画元素必须有稳定 id");
        let duration = self.duration;
        let animate = self.animate;
        let repeat = self.repeat;
        let now = cx.background_executor().now();
        let (phase, running) =
            window.with_element_state(id, |state: Option<AmbientAnimationState>, _window| {
                let mut state = state.unwrap_or(AmbientAnimationState {
                    animate,
                    started_at: now,
                });
                if !state.animate && animate {
                    state.started_at = now;
                }
                state.animate = animate;
                let elapsed = now.saturating_duration_since(state.started_at);
                let transition = if !animate || cx.reduce_motion() {
                    (1.0, false)
                } else if repeat {
                    (ambient_loop_phase(duration, elapsed), true)
                } else {
                    ambient_transition_phase(duration, elapsed)
                };
                (transition, state)
            });
        let mut element = (self.renderer)(phase);
        let layout_id = element.request_layout(window, cx);
        (layout_id, AmbientAnimationLayoutState { element, running })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        if state.running {
            request_shared_ambient_frame(window, window.current_view(), cx);
        }
        state.element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        state.element.paint(window, cx);
    }
}

/// 构造由元素状态保存起点的一次性状态动画。调用方传入可重复执行的元素工厂，而
/// 不是一次性的已构造元素；动画到达 1.0 后不再预约帧，稳态保持静态提示。
pub fn ambient_animation<E: IntoElement + 'static>(
    id: impl Into<ElementId>,
    duration: Duration,
    animate: bool,
    renderer: impl Fn(f32) -> E + 'static,
) -> AmbientAnimationElement {
    AmbientAnimationElement {
        id: id.into(),
        duration,
        animate,
        repeat: false,
        renderer: Box::new(move |phase| renderer(phase).into_any_element()),
    }
}

/// 低能耗的小号 loading spinner。
///
/// 进行中一直转。帧率和同窗其它装饰动画共用一只时钟，不按显示器刷新率重绘。
/// `animate == false` 或系统开启减弱动态效果时停在静态图标。
pub fn ambient_spinner(id: impl Into<ElementId>, color: Hsla, animate: bool) -> AnyElement {
    let mut animation = ambient_animation(id, Duration::from_millis(800), animate, move |delta| {
        Icon::new(IconName::Loader)
            .xsmall()
            .text_color(color)
            .transform(Transformation::rotate(percentage(delta)))
    });
    animation.repeat = true;
    animation.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, ParentElement as _, Render, TestAppContext, div, px, size};
    use std::{cell::Cell, rc::Rc, time::Duration};

    struct AmbientTransitionTestView {
        parent_renders: Rc<Cell<usize>>,
        animation_renders: Rc<Cell<usize>>,
    }

    impl Render for AmbientTransitionTestView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            self.parent_renders.set(self.parent_renders.get() + 1);
            let animation_renders = self.animation_renders.clone();
            div().child(ambient_animation(
                "ambient-render-boundary-test",
                Duration::from_secs(1),
                true,
                move |_| {
                    animation_renders.set(animation_renders.get() + 1);
                    div()
                },
            ))
        }
    }

    #[test]
    fn ambient_animation_is_one_shot_and_frame_limited() {
        let duration = Duration::from_secs(2);
        assert_eq!(
            ambient_transition_phase(duration, Duration::from_millis(500)),
            (0.25, true)
        );
        assert_eq!(
            ambient_transition_phase(duration, Duration::from_millis(2_500)),
            (1.0, false),
            "状态动画结束后必须停在静态终态，不能继续循环"
        );
        assert_eq!(
            ambient_frame_interval(),
            Duration::from_secs_f32(1.0 / AMBIENT_ANIMATION_MAX_FPS),
            "装饰动画不能退回显示器刷新率"
        );
    }

    #[test]
    fn ambient_spinner_phase_keeps_cycling_for_as_long_as_work_is_running() {
        let cycle = Duration::from_millis(800);
        assert_eq!(ambient_loop_phase(cycle, Duration::ZERO), 0.0);
        assert!((ambient_loop_phase(cycle, Duration::from_millis(400)) - 0.5).abs() < 0.01);
        assert!(ambient_loop_phase(cycle, cycle).abs() < 0.01);
        assert!(
            (ambient_loop_phase(cycle, Duration::from_millis(1_200)) - 0.5).abs() < 0.01,
            "超过一圈后必须继续转，不能停在终态"
        );
    }

    #[test]
    fn ambient_motion_stops_for_inactive_windows_and_hidden_surfaces() {
        assert!(ambient_motion_enabled(true, true, true));
        assert!(!ambient_motion_enabled(false, true, true));
        assert!(!ambient_motion_enabled(true, false, true));
        assert!(!ambient_motion_enabled(true, true, false));
        assert!(!ambient_motion_enabled(false, false, false));
    }

    #[test]
    fn ambient_frame_scheduler_coalesces_requests_per_window() {
        let first_window = gpui::WindowId::from(1);
        let second_window = gpui::WindowId::from(2);
        let first_target = gpui::EntityId::from(1);
        let second_target = gpui::EntityId::from(2);
        let scheduler = AmbientFrameScheduler::default();

        assert!(scheduler.claim(first_window, first_target));
        assert!(!scheduler.claim(first_window, first_target));
        assert!(!scheduler.claim(first_window, second_target));
        assert!(scheduler.claim(second_window, second_target));

        assert_eq!(
            scheduler.take_targets(first_window),
            HashSet::from([first_target, second_target])
        );
        assert!(scheduler.claim(first_window, first_target));
    }

    #[gpui::test]
    fn ambient_frame_stops_scheduling_after_the_transition(cx: &mut TestAppContext) {
        let parent_renders = Rc::new(Cell::new(0));
        let animation_renders = Rc::new(Cell::new(0));
        cx.open_window(size(px(100.), px(100.)), {
            let parent_renders = parent_renders.clone();
            let animation_renders = animation_renders.clone();
            move |_, _| AmbientTransitionTestView {
                parent_renders,
                animation_renders,
            }
        });
        cx.run_until_parked();

        assert_eq!(parent_renders.get(), 1);
        assert_eq!(animation_renders.get(), 1);

        cx.executor()
            .advance_clock(ambient_frame_interval() + Duration::from_millis(5));
        cx.run_until_parked();

        assert!(
            animation_renders.get() > 1,
            "动画子树应在共享时钟到点后更新"
        );
        assert!(parent_renders.get() > 1);

        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        let settled_parent_renders = parent_renders.get();
        let settled_animation_renders = animation_renders.get();

        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        assert_eq!(
            parent_renders.get(),
            settled_parent_renders,
            "状态动画到达终态后不能继续唤醒父视图"
        );
        assert_eq!(
            animation_renders.get(),
            settled_animation_renders,
            "状态动画到达终态后不能继续创建 timer"
        );
    }
}
