//! 装饰动画（状态提亮、spinner、思考点）和交互过渡的共用件。
//!
//! 装饰动画跟 gpui 的 `with_animation` 一样按显示器刷新率出帧，不限帧率。成本靠视图缓存压：
//! gpui-pre 通知一个视图会把它的祖先一并标脏，祖先自己那层 render 每帧重跑；不在这条链上的
//! `.cached()` 子树直接复用上一帧。所以重的子树（对话列表、工作台各栏）要做成 cached 实体，
//! 链上各层的 render 只读内存。应用不在前台、窗口失焦或系统开了减弱动态效果时，停在静态终态。

mod list;

pub use list::{
    ARRIVAL_OPEN, FoldBody, GLOW, WASH_END, WASH_HOLD, WASH_PEAK, WASH_RISE, arrival_open,
    disclosure_open, disclosure_settled_closed, fold_body, replay_elapsed, wash_alpha,
};

use gpui::{
    Anchor, AnyElement, App, AsyncApp, Bounds, BoxShadow, Element, ElementId, GlobalElementId,
    Hsla, InspectorElementId, IntoElement, LayoutId, ParentElement, Pixels, Rgba, Styled,
    Transformation, Window, hsla, percentage, point, px, svg,
};
use gpui_base::h_flex;
// 时间一律从 `cx.background_executor().now()` 取（测试里可换假时钟）。它返回的
// `Instant` 在桌面端就是 std 的，直接用 std 的类型名，不为一个类型依赖 scheduler 包。
use std::time::{Duration, Instant};

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

impl gpui::Global for AmbientMotionState {}

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

/// 等应用回到前台（[`ambient_application_active`]）。已经在前台就立刻返回。只在用户看得见
/// 时才有意义的定时重绘，在后台时挂在这里不醒。
pub async fn ambient_application_activated(cx: &AsyncApp) {
    crate::global_wait::wait_for_global::<AmbientMotionState, ()>(cx, |cx| {
        ambient_application_active(cx).then_some(())
    })
    .await;
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

/// 按时间取相位、重建子元素、没到终态就要下一帧的元素。起点存在元素状态里。
///
/// 不直接用 `with_animation`：它取的是真实时钟，测试换不了；它要先有一个元素再改，
/// 这里是按相位整个重建；`animate` 从关到开时还要从头播。
pub struct AmbientAnimationElement {
    id: ElementId,
    duration: Duration,
    animate: bool,
    repeat: bool,
    renderer: Box<dyn Fn(f32) -> AnyElement>,
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
            window.request_animation_frame();
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
/// 不是一次性的已构造元素；动画到达 1.0 后不再要帧，稳态保持静态提示。
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

/// 按周期一直循环的装饰动画，`animate == false` 时停在相位 1。
pub(crate) fn ambient_loop<E: IntoElement + 'static>(
    id: impl Into<ElementId>,
    cycle: Duration,
    animate: bool,
    renderer: impl Fn(f32) -> E + 'static,
) -> AmbientAnimationElement {
    AmbientAnimationElement {
        repeat: true,
        ..ambient_animation(id, cycle, animate, renderer)
    }
}

/// 小号 loading spinner。
///
/// 进行中一直转。`animate == false` 或系统开启减弱动态效果时停在静态图标。
pub fn ambient_spinner(id: impl Into<ElementId>, color: Hsla, animate: bool) -> AnyElement {
    ambient_loop(id, Duration::from_millis(800), animate, move |delta| {
        svg()
            .path(crate::IconName::Loader.path())
            .size_3()
            .flex_shrink_0()
            .text_color(color)
            .with_transformation(Transformation::rotate(percentage(delta)))
    })
    .into_any_element()
}

/// 按下反馈。
pub const PRESS_MS: u64 = 80;
/// 焦点呼吸、思考展开。
pub const BASE_MS: u64 = 200;
/// 用户气泡入场、附件挤入。
pub const ENTER_MS: u64 = 280;
/// 复制成功后对勾停留再淡回。
pub const COPY_ACK_MS: u64 = 1200;

const QUIET_DOT_CYCLE_MS: u64 = 1_900;

/// `cubic-bezier(.32, .72, 0, 1)`。输出留在 \[0, 1\]，给宽度和焦点用。
pub fn ease_snap(t: f32) -> f32 {
    unit_bezier(0.32, 0.72, 0.0, 1.0).sample(t).clamp(0.0, 1.0)
}

fn unit_bezier(x1: f32, y1: f32, x2: f32, y2: f32) -> gpui_base::Easing {
    gpui_base::Easing::cubic_bezier(x1, y1, x2, y2)
        .expect("内置缓动的控制点是常量，x 在 0..=1 且 y 有限")
}

/// 气泡从 6px 落到 0。
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

/// 纯思考的记号。
///
/// `animate == false` 或减弱动态效果时，相位会被定在 1，而波形在 1 的透明度是 0。
/// 那种情况画三颗静态点，避免记号消失。
pub fn quiet_dots(id: impl Into<ElementId>, color: Hsla, animate: bool) -> AnyElement {
    ambient_loop(
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
    )
    .into_any_element()
}

/// 光带往返一次的时长，单程 2.2 秒。它一跑就是好几分钟、一直在余光里，
/// 比几秒钟的进度条走得慢。
const SWEEP_CYCLE_MS: u64 = 4_400;
/// 光带占轨道宽度的比例。
const SWEEP_WIDTH: f32 = 0.3;
/// 光晕：挂在光带中段的胶囊投影，往上偏，光主要渗到框外，不压输入的字。
const SWEEP_HALO_ALPHA: f32 = 0.35;
const SWEEP_HALO_BLUR_PX: f32 = 8.0;
/// 不动时整条顶边染色的浓度，相对亮芯。
const SWEEP_STILL_ALPHA: f32 = 0.4;

/// 光带的三种样子。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepMode {
    /// 在干活：来回扫。
    Sweep,
    /// 轮到用户（等批准、等回答）：停在正中，颜色由调用方换。
    Hold,
    /// 窗口失焦或系统减弱动态效果：整条顶边淡淡染色，不动。
    Still,
}

/// 光带左缘在轨道上的位置（占轨道宽度的比例）。前半圈从左到右，后半圈折回，两头放慢。
/// 光带始终整段留在轨道里，两头渐隐到透明，走到端点也不会被裁出硬边。
fn sweep_left(cycle_t: f32) -> f32 {
    gpui::bounce(gpui::ease_in_out)(cycle_t.rem_euclid(1.0)) * (1.0 - SWEEP_WIDTH)
}

/// 亮芯的峰值透明度。同样的蓝在浅色底上更扎眼，压低一些。
fn sweep_core_alpha() -> f32 {
    if crate::ui_theme::is_light() {
        0.7
    } else {
        0.9
    }
}

/// 一段两头渐隐的亮条，`left`、`width` 都是占轨道宽度的比例。
/// 渐变只有两个色标，所以拆成左右两半各渐一边；渐到同色的透明，不渐到透明黑，免得两头发灰。
fn sweep_band(left: f32, width: f32, color: Hsla, halo: bool) -> gpui::Div {
    let clear = color.opacity(0.0);
    let half = |from: Hsla, to: Hsla| {
        gpui::div().flex_1().h_full().bg(gpui::linear_gradient(
            90.,
            gpui::linear_color_stop(from, 0.),
            gpui::linear_color_stop(to, 1.),
        ))
    };
    let band = h_flex()
        .absolute()
        .top_0()
        .h_full()
        .left(gpui::relative(left))
        .w(gpui::relative(width))
        .child(half(clear, color))
        .child(half(color, clear));
    if !halo {
        return band;
    }
    // 光晕只挂中段一半：两头本来就渐隐，模糊边落在暗处，看不出投影的方角。
    band.child(
        gpui::div()
            .absolute()
            .top(px(-0.5))
            .left(gpui::relative(0.25))
            .w(gpui::relative(0.5))
            .h(px(2.))
            .rounded_full()
            .shadow(vec![BoxShadow {
                color: color.opacity(SWEEP_HALO_ALPHA),
                offset: point(px(0.), px(-2.)),
                blur_radius: px(SWEEP_HALO_BLUR_PX),
                spread_radius: px(0.),
                inset: false,
            }]),
    )
}

/// 运行中的光带：亮芯加光晕，铺满父元素给出的轨道。
///
/// `presence` 是 0 到 1 的显隐，回合开始、结束时由调用方过渡，光带淡入淡出而不是一下冒出来。
pub fn running_sweep(
    id: impl Into<ElementId>,
    color: Hsla,
    mode: SweepMode,
    presence: f32,
) -> AnyElement {
    let core = color.opacity(sweep_core_alpha());
    let layer = match mode {
        SweepMode::Sweep => ambient_loop(
            id,
            Duration::from_millis(SWEEP_CYCLE_MS),
            true,
            move |phase| sweep_band(sweep_left(phase), SWEEP_WIDTH, core, true),
        )
        .into_any_element(),
        SweepMode::Hold => {
            sweep_band((1.0 - SWEEP_WIDTH) / 2.0, SWEEP_WIDTH, core, true).into_any_element()
        }
        SweepMode::Still => {
            sweep_band(0.0, 1.0, core.opacity(SWEEP_STILL_ALPHA), false).into_any_element()
        }
    };
    gpui::div()
        .size_full()
        .opacity(presence.clamp(0.0, 1.0))
        .child(layer)
        .into_any_element()
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

/// 聚焦时的焦点色外环。静息为 0，结构仍保留这一层。
pub fn composer_ring_alpha(t: f32) -> f32 {
    0.28 * t.clamp(0.0, 1.0)
}

/// 抬起阴影加一圈焦点色。两层始终都在，避免聚焦时阴影数组换形状。
pub fn composer_shadows(t: f32, ring: Rgba) -> Vec<BoxShadow> {
    let t = t.clamp(0.0, 1.0);
    let ring = Hsla::from(ring).alpha(composer_ring_alpha(t));
    vec![
        BoxShadow::new(px(0.), px(1.), hsla(0., 0., 0., composer_lift_alpha(t)))
            .blur_radius(px(3.)),
        BoxShadow::new(px(0.), px(0.), ring).spread_radius(px(3.)),
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

/// 控件状态切换的过渡：开关滑块、分段选中块、焦点。`BASE_MS` 加 `ease_snap`。
///
/// 交给 `gpui_base::motion::transition` 用：第一帧直接采用目标值（页面刚打开不会滑），
/// 中途换目标从当前采样值掉头，减弱动效时直接到位。
pub fn snap_transition() -> gpui_base::motion::Transition {
    gpui_base::motion::Transition::new(Duration::from_millis(BASE_MS)).ease(ease_snap)
}

/// 0 到 1 的焦点类过渡。第一帧直接采用当前值，之后才插值。
pub fn unit_transition(
    id: impl Into<gpui_base::motion::TransitionId>,
    target: f32,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    gpui_base::motion::transition(id, target, snap_transition(), window, cx)
}

/// 浮层入场时离终点的距离：从触发点那一侧挪过来这么多。
const OVERLAY_ENTER_SHIFT: Pixels = px(4.);

/// 浮层从哪一侧进场。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnterFrom {
    /// 开在触发点下方：从上面（触发点那侧）落下来。
    Above,
    /// 开在触发点上方，或窗口里的对话框：从下面升上来。
    Below,
    /// 只淡入、不挪：位置会翻边的（右键菜单）、开在侧边的、贴着指针的提示。
    InPlace,
}

impl EnterFrom {
    /// 按卡片哪个角对着触发点推出从哪边进。gpui-base 的 `Popup` 放不下时只往窗口里夹、
    /// 不翻边，所以按锚点推出的方向就是实际方向。
    pub fn anchored(anchor: Anchor) -> Self {
        match anchor {
            Anchor::TopLeft | Anchor::TopCenter | Anchor::TopRight => Self::Above,
            Anchor::BottomLeft | Anchor::BottomCenter | Anchor::BottomRight => Self::Below,
            Anchor::LeftCenter | Anchor::RightCenter => Self::InPlace,
        }
    }
}

/// 浮层入场的一帧：整块的透明度（连投影一起）和竖直方向离终点的偏移。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayEnter {
    pub opacity: f32,
    /// 正数往下。用作相对定位的 `top` 偏移，不改排版，也不挪别的元素。
    pub dy: Pixels,
}

impl OverlayEnter {
    /// 到位：不透明、不偏。
    pub const SETTLED: Self = Self {
        opacity: 1.,
        dy: Pixels::ZERO,
    };

    /// `progress` 是已经缓动过的 0 到 1。
    pub fn at(progress: f32, from: EnterFrom) -> Self {
        let progress = progress.clamp(0., 1.);
        let remaining = OVERLAY_ENTER_SHIFT * (1. - progress);
        Self {
            opacity: progress,
            dy: match from {
                EnterFrom::Above => -remaining,
                EnterFrom::Below => remaining,
                EnterFrom::InPlace => Pixels::ZERO,
            },
        }
    }

    /// 把这一帧落到浮层卡片上。卡片自己已经用 `top` 留间距的，自己合上 `dy`，别用这个。
    pub fn apply<E: Styled>(self, element: E) -> E {
        element.opacity(self.opacity).top(self.dy)
    }
}

/// 浮层入场：菜单、弹出卡片、悬停卡片、提示、对话框打开时淡入，并从触发点那侧挪 4px 到位，
/// 交代它从哪来。时长和曲线是 [`crate::kit::MOTION`] 的 `overlay_enter()`。
///
/// 浮层每次打开都是新挂上的元素。进度挂在元素状态上，只在连续几帧都画它时留着：关掉那一帧
/// 没人取，状态就回收了，下次打开从 0 再播；开着时一直取，到位后不再要帧。系统减弱动态效果时
/// 直接到位。只在浮层开着时调用；`id` 带上浮层自己的 id，同一视图里的几个浮层才不串。
///
/// 只做入场。gpui-base 关掉浮层就卸下内容，出场淡出做不了。
pub fn overlay_enter(
    id: impl Into<gpui_base::motion::TransitionId>,
    from: EnterFrom,
    window: &mut Window,
    cx: &mut App,
) -> OverlayEnter {
    let progress = gpui_base::motion::Presence::new(id, true)
        .transition(crate::kit::MOTION.overlay_enter())
        .sample(window, cx)
        .progress;
    OverlayEnter::at(progress, from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Render, TestAppContext, WindowHandle, div, px, size};
    use std::{cell::RefCell, rc::Rc, time::Duration};

    struct AmbientTestView {
        repeat: bool,
        phases: Rc<RefCell<Vec<f32>>>,
    }

    impl Render for AmbientTestView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let phases = self.phases.clone();
            let renderer = move |phase| {
                phases.borrow_mut().push(phase);
                div()
            };
            let id = "ambient-frame-test";
            let cycle = Duration::from_secs(1);
            div().child(if self.repeat {
                ambient_loop(id, cycle, true, renderer)
            } else {
                ambient_animation(id, cycle, true, renderer)
            })
        }
    }

    fn open_ambient_test_view(
        repeat: bool,
        cx: &mut TestAppContext,
    ) -> (Rc<RefCell<Vec<f32>>>, WindowHandle<AmbientTestView>) {
        let phases = Rc::new(RefCell::new(Vec::new()));
        let window = cx.open_window(size(px(100.), px(100.)), {
            let phases = phases.clone();
            move |_, _| AmbientTestView { repeat, phases }
        });
        cx.run_until_parked();
        (phases, window)
    }

    /// 送出一帧，返回这一帧前有几个元素要过帧。
    fn simulate_next_frame(
        window: &WindowHandle<AmbientTestView>,
        cx: &mut TestAppContext,
    ) -> usize {
        let callbacks = window
            .update(cx, |_, window, cx| window.simulate_next_frame(cx))
            .unwrap();
        cx.run_until_parked();
        callbacks
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
    fn ambient_animation_is_one_shot() {
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

    #[gpui::test]
    fn ambient_animation_follows_frames_then_stops_at_the_end(cx: &mut TestAppContext) {
        let (phases, window) = open_ambient_test_view(false, cx);
        assert_eq!(*phases.borrow(), vec![0.0]);

        cx.executor().advance_clock(Duration::from_millis(250));
        assert!(simulate_next_frame(&window, cx) > 0, "动画中途必须要下一帧");
        let mid = *phases.borrow().last().unwrap();
        assert!((mid - 0.25).abs() < 0.01, "相位跟着时钟走，实际 {mid}");

        cx.executor().advance_clock(Duration::from_secs(2));
        assert!(simulate_next_frame(&window, cx) > 0);
        assert_eq!(*phases.borrow().last().unwrap(), 1.0);

        assert_eq!(
            simulate_next_frame(&window, cx),
            0,
            "状态动画到达终态后不能再要帧"
        );
    }

    #[gpui::test]
    fn ambient_loop_keeps_requesting_frames_past_one_cycle(cx: &mut TestAppContext) {
        let (phases, window) = open_ambient_test_view(true, cx);

        cx.executor().advance_clock(Duration::from_millis(2_500));
        assert!(simulate_next_frame(&window, cx) > 0);
        let phase = *phases.borrow().last().unwrap();
        assert!((phase - 0.5).abs() < 0.01, "两圈半应回到半圈，实际 {phase}");
        assert!(
            simulate_next_frame(&window, cx) > 0,
            "循环动画过了一圈也要继续要帧"
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
    fn sweep_goes_left_to_right_then_back_and_stays_on_the_track() {
        assert_eq!(sweep_left(0.0), 0.0);
        assert!(
            (sweep_left(0.5) - (1.0 - SWEEP_WIDTH)).abs() < 1e-5,
            "半圈走到右端"
        );
        assert!(sweep_left(1.0).abs() < 1e-5, "一圈回到左端");
        assert!(sweep_left(0.25) < sweep_left(0.4), "前半圈往右");
        assert!(sweep_left(0.6) > sweep_left(0.75), "后半圈往左");
        let mut t = 0.0;
        while t <= 1.0 {
            let left = sweep_left(t);
            assert!(
                (0.0..=1.0 - SWEEP_WIDTH + 1e-5).contains(&left),
                "{t} 时光带出了轨道：{left}"
            );
            t += 0.01;
        }
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
        let ring = gpui::rgb(0x1084fe);
        assert_eq!(composer_shadows(0.0, ring).len(), 2);
        assert_eq!(composer_shadows(1.0, ring).len(), 2);
        assert!(composer_lift_alpha(1.0) > composer_lift_alpha(0.0));
        assert_eq!(composer_ring_alpha(0.0), 0.0);
        assert!((composer_ring_alpha(1.0) - 0.28).abs() < 1e-5);
    }

    #[test]
    fn overlay_enter_comes_from_the_trigger_side_and_settles_in_place() {
        let below_trigger = OverlayEnter::at(0.0, EnterFrom::anchored(gpui::Anchor::TopLeft));
        assert_eq!(below_trigger.opacity, 0.0);
        assert!(below_trigger.dy < px(0.), "开在下方的从上面落下来");

        let above_trigger = OverlayEnter::at(0.0, EnterFrom::anchored(gpui::Anchor::BottomRight));
        assert!(above_trigger.dy > px(0.), "开在上方的从下面升上来");

        let side = OverlayEnter::at(0.0, EnterFrom::anchored(gpui::Anchor::RightCenter));
        assert_eq!(side.dy, px(0.), "开在侧边的只淡入");

        for from in [EnterFrom::Above, EnterFrom::Below, EnterFrom::InPlace] {
            assert_eq!(OverlayEnter::at(1.0, from), OverlayEnter::SETTLED);
        }
    }

    struct OverlayTestView {
        open: bool,
        samples: Rc<RefCell<Vec<OverlayEnter>>>,
    }

    impl Render for OverlayTestView {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            if self.open {
                let enter = overlay_enter("overlay-enter-test", EnterFrom::Above, window, cx);
                self.samples.borrow_mut().push(enter);
            }
            div()
        }
    }

    #[gpui::test]
    fn overlay_enter_plays_on_every_open_and_stops_once_settled(cx: &mut TestAppContext) {
        let samples = Rc::new(RefCell::new(Vec::new()));
        let window = cx.open_window(size(px(100.), px(100.)), {
            let samples = samples.clone();
            move |_, _| OverlayTestView {
                open: true,
                samples,
            }
        });
        cx.run_until_parked();
        let last = |samples: &Rc<RefCell<Vec<OverlayEnter>>>| *samples.borrow().last().unwrap();
        assert_eq!(last(&samples).opacity, 0.0, "刚打开的第一帧从透明起");

        cx.executor().advance_clock(Duration::from_millis(60));
        let next_frame = |cx: &mut TestAppContext| {
            let callbacks = window
                .update(cx, |_, window, cx| window.simulate_next_frame(cx))
                .unwrap();
            cx.run_until_parked();
            callbacks
        };
        assert!(next_frame(cx) > 0, "入场中途要下一帧");
        let mid = last(&samples);
        assert!(
            mid.opacity > 0.0 && mid.opacity < 1.0,
            "中途半透明：{mid:?}"
        );

        cx.executor().advance_clock(Duration::from_millis(400));
        next_frame(cx);
        assert_eq!(last(&samples), OverlayEnter::SETTLED);
        assert_eq!(next_frame(cx), 0, "到位后不能再要帧");

        // 关上画一帧，状态随之回收；再打开要从头播，而不是直接到位。
        window
            .update(cx, |view, _, cx| {
                view.open = false;
                cx.notify();
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |view, _, cx| {
                view.open = true;
                cx.notify();
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(last(&samples).opacity, 0.0, "重新打开要再播一次入场");
    }
}
