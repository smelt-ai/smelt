//! 状态装饰动画的统一能耗策略。
//!
//! GPUI 的重复动画默认跟随显示器刷新率；即使用 `with_max_fps`，每个动画元素也会
//! 各自创建 timer。Smelt 的根视图较重，多个侧栏呼吸灯或 spinner 错峰唤醒时会让
//! 整窗持续布局、绘制和提交 Metal。状态进入动画只走一次、同窗共用 timer，
//! 到达静态终态后停止唤醒。进行中的 spinner 也走这只时钟，按周期循环，
//! 不跟显示器刷新率。面板开合等交互过渡仍由调用方使用默认动画帧率。

use gpui::{
    AnyElement, App, AppContext as _, Bounds, BoxShadow, Context, Element, ElementId, Entity,
    EntityId, Global, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId,
    ParentElement, Pixels, Render, Rgba, Styled as _, Transformation, Window, WindowId, hsla,
    percentage, point, px,
};
use gpui_component::{Icon, IconName, Sizable as _, h_flex};
// 时间一律从 `cx.background_executor().now()` 取（测试里可换假时钟）。它返回的
// `Instant` 在桌面端就是 std 的，直接用 std 的类型名，不为一个类型依赖 scheduler 包。
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    time::{Duration, Instant},
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

/// 一段装饰动画的参数。外层每次重建都会带来新的 `renderer`，原样交给动画视图。
#[derive(Clone)]
struct AmbientAnimationSpec {
    id: ElementId,
    duration: Duration,
    animate: bool,
    repeat: bool,
    renderer: Rc<dyn Fn(f32) -> AnyElement>,
}

/// 装饰动画单独占一个小视图。动画帧只通知这个视图：保留模式渲染下，它外面的大
/// 视图（比如整个工作台）直接沿用上一帧，不会跟着 6 FPS 时钟整个重建。
///
/// 视图实体存在外层的元素状态里，元素从树上消失时随之释放。
pub struct AmbientAnimationElement {
    spec: AmbientAnimationSpec,
}

struct AmbientAnimationView {
    spec: AmbientAnimationSpec,
}

impl Render for AmbientAnimationView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        AmbientAnimationFrame {
            spec: self.spec.clone(),
        }
    }
}

impl IntoElement for AmbientAnimationElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for AmbientAnimationElement {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.spec.id.clone())
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
        let spec = self.spec.clone();
        let view =
            window.with_element_state(id, |view: Option<Entity<AmbientAnimationView>>, _| {
                let view =
                    view.unwrap_or_else(|| cx.new(|_| AmbientAnimationView { spec: spec.clone() }));
                (view.clone(), view)
            });
        // 只写不通知：动画视图跟着外层这次重建一起重画，外层不会因此被标成已变化。
        view.update(cx, |view, _| view.spec = spec);
        let mut element = view.into_any_element();
        let layout_id = element.request_layout(window, cx);
        (layout_id, element)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

/// 动画视图里真正按时间取相位、预约下一帧的元素。起点存在元素状态里。
struct AmbientAnimationFrame {
    spec: AmbientAnimationSpec,
}

impl IntoElement for AmbientAnimationFrame {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for AmbientAnimationFrame {
    type RequestLayoutState = AmbientAnimationLayoutState;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.spec.id.clone())
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
        let duration = self.spec.duration;
        let animate = self.spec.animate;
        let repeat = self.spec.repeat;
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
        let mut element = (self.spec.renderer)(phase);
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
        spec: AmbientAnimationSpec {
            id: id.into(),
            duration,
            animate,
            repeat: false,
            renderer: Rc::new(move |phase| renderer(phase).into_any_element()),
        },
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
    animation.spec.repeat = true;
    animation.into_any_element()
}

/// 按下反馈。交互过渡走调用方的帧率，不进 6 FPS 装饰时钟。
pub const PRESS_MS: u64 = 80;
/// 焦点呼吸、思考展开。
pub const BASE_MS: u64 = 200;
/// 用户气泡入场、附件挤入。
pub const ENTER_MS: u64 = 280;
/// 复制成功后对勾停留再淡回。
pub const COPY_ACK_MS: u64 = 1200;

/// Div 没有 scale。按下用这段透明度代替缩到 0.96，布局不会跟着抖。
pub const PRESS_OPACITY: f32 = 0.72;

const QUIET_DOT_CYCLE_MS: u64 = 1_900;

/// `cubic-bezier(.32, .72, 0, 1)`。输出留在 \[0, 1\]，给宽度和焦点用。
pub fn ease_snap(t: f32) -> f32 {
    unit_bezier(0.32, 0.72, 0.0, 1.0).sample(t).clamp(0.0, 1.0)
}

/// `cubic-bezier(.34, 1.3, .64, 1)`。y 可以超过 1，端点仍是 0 和 1。
///
/// `Easing::CubicBezier` 走不钳 y 的求解，过冲留得住。驱动透明度或颜色的调用方自己钳。
pub fn ease_overshoot(t: f32) -> f32 {
    unit_bezier(0.34, 1.3, 0.64, 1.0).sample(t)
}

fn unit_bezier(x1: f32, y1: f32, x2: f32, y2: f32) -> gpui_base::Easing {
    gpui_base::Easing::cubic_bezier(x1, y1, x2, y2)
        .expect("内置缓动的控制点是常量，x 在 0..=1 且 y 有限")
}

/// 气泡从 6px 落到 0。过冲时偏移为负，大约不到 1px。
pub fn enter_offset_px(eased: f32) -> f32 {
    6.0 * (1.0 - eased)
}

/// 对勾先保持约 800ms，再用最后约 400ms 淡出。
pub fn copy_ack_opacity(elapsed: Duration) -> f32 {
    let total = Duration::from_millis(COPY_ACK_MS);
    let hold = Duration::from_millis(COPY_ACK_MS.saturating_sub(400));
    if elapsed <= hold {
        return 1.0;
    }
    if elapsed >= total {
        return 0.0;
    }
    let fade = (elapsed - hold).as_secs_f32() / (total - hold).as_secs_f32();
    1.0 - ease_snap(fade)
}

/// 三个点的一拍。0–8% 隐去，22–70% 亮着，90% 之后再隐去。
pub fn quiet_dot_opacity(cycle_t: f32) -> f32 {
    let t = cycle_t.rem_euclid(1.0);
    if t <= 0.08 {
        0.0
    } else if t < 0.22 {
        (t - 0.08) / (0.22 - 0.08)
    } else if t <= 0.70 {
        1.0
    } else if t < 0.90 {
        1.0 - (t - 0.70) / (0.90 - 0.70)
    } else {
        0.0
    }
}

/// 纯思考的记号。走共享的 6 FPS 时钟，不给每个点单独开 timer。
///
/// `animate == false` 或减弱动态效果时，相位会被定在 1，而波形在 1 的透明度是 0。
/// 那种情况画三颗静态点，避免记号消失。
pub fn quiet_dots(id: impl Into<ElementId>, color: Hsla, animate: bool) -> AnyElement {
    let mut animation = ambient_animation(
        id,
        Duration::from_millis(QUIET_DOT_CYCLE_MS),
        animate,
        move |phase| {
            let frozen = !animate || phase >= 1.0;
            h_flex().gap(px(3.)).children((0..3).map(|dot| {
                let opacity = if frozen {
                    0.55
                } else {
                    let delay = match dot {
                        0 => 0.0,
                        1 => 0.15 / 1.9,
                        _ => 0.30 / 1.9,
                    };
                    quiet_dot_opacity((phase - delay).rem_euclid(1.0))
                };
                gpui::div()
                    .size(px(3.))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(color.opacity(opacity))
            }))
        },
    );
    animation.spec.repeat = true;
    animation.into_any_element()
}

/// 在 RGBA 上混合。灰边插到蓝色时不能走色相，否则会绕过一整圈色环。
pub fn mix_rgba(from: Rgba, to: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    Rgba {
        r: from.r + (to.r - from.r) * t,
        g: from.g + (to.g - from.g) * t,
        b: from.b + (to.b - from.b) * t,
        a: from.a + (to.a - from.a) * t,
    }
}

/// 输入条抬起阴影。静息约 0.06，聚焦约 0.14。
pub fn composer_lift_alpha(t: f32) -> f32 {
    0.06 + 0.08 * t.clamp(0.0, 1.0)
}

/// 聚焦时的强调色外环。静息为 0，结构仍保留这一层。
pub fn composer_ring_alpha(t: f32) -> f32 {
    0.28 * t.clamp(0.0, 1.0)
}

/// 抬起阴影加一圈强调色。两层始终都在，避免聚焦时阴影数组换形状。
pub fn composer_shadows(t: f32, accent: Rgba) -> Vec<BoxShadow> {
    let t = t.clamp(0.0, 1.0);
    let accent = Hsla::from(accent).alpha(composer_ring_alpha(t));
    vec![
        BoxShadow::new(px(0.), px(1.), hsla(0., 0., 0., composer_lift_alpha(t)))
            .blur_radius(px(3.)),
        BoxShadow::new(px(0.), px(0.), accent).spread_radius(px(3.)),
    ]
}

/// 临界阻尼弹簧：最快回到目标，且不过冲。
///
/// 只用在会被打断、或接着手势的过渡上。中途换目标时它带着当前速度继续，先减速再掉头；
/// 贝塞尔只能从当前位置按新曲线重新起步，速度会瞬间突变。一次走完、不会被打断的过渡
/// 仍用上面的曲线，那种情况两者看不出区别。
///
/// 用解析解，不逐帧积分：给定起始位移和速度，任意时刻的状态直接算出来，掉帧也不会漂。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CriticalSpring {
    /// 固有角频率 ω（rad/s），越大越快。从静止出发，`5 / ω` 秒走完约 97%。
    pub omega: f32,
}

impl CriticalSpring {
    /// 面板开合、列表重排归位。约 125ms 走完 97%，200ms 出头收尾，
    /// 帧数和原来 180ms 的五次方缓出相当。
    pub const SNAP: Self = Self { omega: 40.0 };

    /// 从相对目标的位移 `x0`、速度 `v0` 出发，经过 `t` 秒后的（位移, 速度）。
    pub fn sample(self, x0: f32, v0: f32, t: f32) -> (f32, f32) {
        let w = self.omega;
        let t = t.max(0.0);
        let c = v0 + w * x0;
        let decay = (-w * t).exp();
        ((x0 + c * t) * decay, (v0 - w * c * t) * decay)
    }

    /// 离目标不到 `rest`、速度也小到下一帧走不出 `rest` 时，就算停稳。
    /// `rest` 与位移同单位：进度用 0.002 左右，像素用半个像素。
    pub fn is_settled(self, x: f32, v: f32, rest: f32) -> bool {
        x.abs() < rest && v.abs() < rest * self.omega
    }
}

#[derive(Clone, Copy)]
struct FlipMotion {
    displacement: f32,
    velocity: f32,
    started_at: Instant,
}

impl FlipMotion {
    fn sample(self, now: Instant) -> (f32, f32) {
        let t = now.saturating_duration_since(self.started_at).as_secs_f32();
        CriticalSpring::SNAP.sample(self.displacement, self.velocity, t)
    }
}

#[derive(Default)]
struct FlipState {
    seen_generation: Option<u64>,
    /// 上一帧实际画在哪（布局位置加当时的偏移）。
    painted_top: Option<Pixels>,
    motion: Option<FlipMotion>,
}

/// 半个像素以内的位移不值得动。
const FLIP_REST_PX: f32 = 0.5;

/// 列表重排时，让条目从旧位置滑到新位置（FLIP）。
///
/// 每帧记下这一项画在哪。调用方重排后把 `generation` 加一；之后的第一次 prepaint 拿上一帧
/// 的位置减去新布局位置，用元素偏移先画回旧处，再用弹簧把偏移收到 0。纠正在同一帧的
/// prepaint 里完成，不会先在新位置闪一帧。点击区域跟着偏移走，动画中途也点得准。
///
/// 只在 `generation` 变化那一帧比较位置，所以滚动、窗口缩放这类位移不会触发动画。
/// 子项随祖先一起移动时，自己量到的位移是 0，不会叠加两次。
///
/// `id` 必须跟着条目身份走（会话 id、项目路径），不能用下标，否则重排后状态会串到别的行。
pub struct FlipSlot {
    id: ElementId,
    generation: u64,
    origin_top: Option<Pixels>,
    child: AnyElement,
}

pub fn flip_slot(id: impl Into<ElementId>, generation: u64, child: impl IntoElement) -> FlipSlot {
    FlipSlot {
        id: id.into(),
        generation,
        origin_top: None,
        child: child.into_any_element(),
    }
}

impl FlipSlot {
    /// 这一次重排从哪个高度出发（窗口坐标）。拖拽松手时传拖拽浮层所在的高度，
    /// 那一项就从手放下的地方落到新位置，而不是先跳回原位再滑过去。
    /// 只在重排那一帧生效；`None` 时用上一帧画的位置。
    pub fn from_top(mut self, top: Option<Pixels>) -> Self {
        self.origin_top = top;
        self
    }
}

impl IntoElement for FlipSlot {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for FlipSlot {
    type RequestLayoutState = ();
    type PrepaintState = ();

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
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let id = id.expect("FlipSlot 必须有稳定 id");
        let now = cx.background_executor().now();
        let generation = self.generation;
        let origin_top = self.origin_top;
        let animate = !cx.reduce_motion();
        let (offset, running) = window.with_element_state(id, |state: Option<FlipState>, _| {
            let mut state = state.unwrap_or_default();
            let layout_top = bounds.top();
            let rearranged = state.seen_generation.is_some_and(|seen| seen != generation);
            if rearranged
                && animate
                && let Some(from_top) = origin_top.or(state.painted_top)
            {
                let displacement = f32::from(from_top - layout_top);
                if displacement.abs() >= FLIP_REST_PX {
                    let velocity = state.motion.map_or(0.0, |motion| motion.sample(now).1);
                    state.motion = Some(FlipMotion {
                        displacement,
                        velocity,
                        started_at: now,
                    });
                }
            }
            state.seen_generation = Some(generation);
            let (offset, velocity) = state.motion.map_or((0.0, 0.0), |motion| motion.sample(now));
            let running = state.motion.is_some()
                && !CriticalSpring::SNAP.is_settled(offset, velocity, FLIP_REST_PX);
            let offset = if running { offset } else { 0.0 };
            if !running {
                state.motion = None;
            }
            state.painted_top = Some(layout_top + px(offset));
            ((offset, running), state)
        });
        if running {
            window.request_animation_frame();
        }
        window.with_element_offset(point(px(0.), px(offset)), |window| {
            self.child.prepaint(window, cx);
        });
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

/// 0 到 1 的焦点类过渡。第一帧直接采用当前值，之后才插值。
pub fn unit_transition(
    id: impl Into<gpui_base::motion::TransitionId>,
    target: f32,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    gpui_base::motion::transition(
        id,
        target,
        gpui_base::motion::Transition::new(Duration::from_millis(BASE_MS)).ease(ease_snap),
        window,
        cx,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Render, TestAppContext, div, px, size};
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
    fn critical_spring_starts_where_it_is_told() {
        let spring = CriticalSpring::SNAP;
        let (x, v) = spring.sample(-1.0, 3.0, 0.0);
        assert!((x + 1.0).abs() < 1e-6 && (v - 3.0).abs() < 1e-6);
    }

    #[test]
    fn critical_spring_from_rest_never_overshoots_and_settles_quickly() {
        let spring = CriticalSpring::SNAP;
        let mut settled_at = None;
        for ms in 0..=400 {
            let (x, v) = spring.sample(-1.0, 0.0, ms as f32 / 1000.0);
            assert!(x <= 0.0, "{ms}ms 时冲过了目标：{x}");
            if settled_at.is_none() && spring.is_settled(x, v, 0.002) {
                settled_at = Some(ms);
            }
        }
        let settled_at = settled_at.expect("400ms 内必须停稳");
        assert!(
            (150..=260).contains(&settled_at),
            "收尾时间应和原来 180ms 的面板缓出同一量级，实际 {settled_at}ms"
        );
    }

    #[test]
    fn critical_spring_velocity_is_the_derivative_of_position() {
        let spring = CriticalSpring::SNAP;
        let (x0, v0, t, dt) = (-0.6, 4.0, 0.05, 1e-3);
        let (_, v) = spring.sample(x0, v0, t);
        let numeric =
            (spring.sample(x0, v0, t + dt).0 - spring.sample(x0, v0, t - dt).0) / (2.0 * dt);
        assert!(
            (v - numeric).abs() < 0.05,
            "解析速度 {v}，数值导数 {numeric}"
        );
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
        // gpui-pre 通知子视图时会把祖先标脏，父视图的 render 会再跑。
        // 「沿用上一帧」只在保留模式渲染器上成立，这里不把它当门。

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

    #[test]
    fn ease_snap_stays_inside_the_unit_interval() {
        assert_eq!(ease_snap(0.0), 0.0);
        assert_eq!(ease_snap(1.0), 1.0);
        let mut previous = 0.0;
        let mut t = 0.0;
        while t <= 1.0 {
            let value = ease_snap(t);
            assert!((0.0..=1.0).contains(&value), "{t} -> {value}");
            assert!(value + 1e-4 >= previous, "{t} 往回走了");
            previous = value;
            t += 0.05;
        }
    }

    #[test]
    fn ease_overshoot_leaves_the_unit_interval_and_returns() {
        assert_eq!(ease_overshoot(0.0), 0.0);
        assert_eq!(ease_overshoot(1.0), 1.0);
        let mut peak: f32 = 0.0;
        let mut t = 0.0;
        while t <= 1.0 {
            peak = peak.max(ease_overshoot(t));
            t += 0.05;
        }
        assert!(peak > 1.0, "过冲曲线中段必须超过 1");
        assert!(enter_offset_px(peak) < 0.0);
    }

    #[test]
    fn copy_ack_holds_then_fades_out() {
        assert_eq!(copy_ack_opacity(Duration::ZERO), 1.0);
        assert_eq!(copy_ack_opacity(Duration::from_millis(800)), 1.0);
        let mid = copy_ack_opacity(Duration::from_millis(1_000));
        assert!(mid < 1.0 && mid > 0.0, "淡出中段还在");
        assert_eq!(copy_ack_opacity(Duration::from_millis(COPY_ACK_MS)), 0.0);
    }

    #[test]
    fn quiet_dot_wave_is_dark_at_the_ends_and_lit_while_holding() {
        assert_eq!(quiet_dot_opacity(0.0), 0.0);
        assert_eq!(quiet_dot_opacity(1.0), 0.0);
        assert_eq!(quiet_dot_opacity(0.22), 1.0);
        assert_eq!(quiet_dot_opacity(0.50), 1.0);
        assert_eq!(quiet_dot_opacity(0.70), 1.0);
    }

    #[test]
    fn mix_rgba_matches_both_ends() {
        let from = gpui::rgba(0x11223344);
        let to = gpui::rgba(0xaabbccdd);
        assert_eq!(mix_rgba(from, to, 0.0), from);
        assert_eq!(mix_rgba(from, to, 1.0), to);
    }

    #[test]
    fn composer_focus_raises_the_shadow_without_changing_its_shape() {
        let accent = gpui::rgb(0x1084fe);
        assert_eq!(composer_shadows(0.0, accent).len(), 2);
        assert_eq!(composer_shadows(1.0, accent).len(), 2);
        assert!(composer_lift_alpha(1.0) > composer_lift_alpha(0.0));
        assert_eq!(composer_ring_alpha(0.0), 0.0);
        assert!((composer_ring_alpha(1.0) - 0.28).abs() < 1e-5);
    }
}
