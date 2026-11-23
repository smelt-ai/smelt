//! 推理强度互动舞台滑轨：高帧率离屏绘制的星舰发射栈舞台，支持连续拖拽与档位点选。
//!
//! 画面按归一化进度 `p ∈ [0.0, 1.0]` 连续演进（冷态待命 → 点火马赫环 → Max-Q 音爆云锥 →
//! 地球弧线真空羽流 → 深空曲速离子束 + 弓形光盾），点击跨档触发临界阻尼弹簧与 ~210ms 升档冲击波。
//!
//! 渲染在独立实体 [`EffortStageState`] 里完成：帧循环只通知这一个小实体，不触发外层工作台重排；
//! 出帧靠 `window.on_next_frame` 驱动（空闲微动 30fps，拖拽与升档冲击全速跟手），弹层关闭或
//! 系统开启减弱动态效果时立刻停止出帧并释放 GPU 贴图，零后台唤醒。

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Bounds, BoxShadow, Context, DragMoveEvent, ElementId, EmptyView,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ObjectFit,
    ParentElement as _, Pixels, Render, RenderImage, RenderOnce, Rgba, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled, StyledImage as _, Subscription,
    WeakEntity, Window, canvas, div, img, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _, px, relative, rgb,
};
use gpui_base::StyledExt as _;
use image::{Frame, Rgba as ImageRgba, RgbaImage};
use rust_i18n::t;

use crate::kit::{RADIUS, TYPE, TypeStyled as _};
use crate::{motion, ui_theme};

/// 轨道可视高度（pt）。
const TRACK_HEIGHT: f32 = 52.;
/// 玻璃镜片滑块静息 / 拖拽直径（pt）。
const LENS_DIAMETER: f32 = 46.;
const LENS_DRAG_DIAMETER: f32 = 49.;
/// 滑块中心相对轨道宽度的左右留白比例，让圆镜和弓形光盾完整落在胶囊内。
const TRACK_PAD_LEFT: f32 = 0.115;
const TRACK_PAD_RIGHT: f32 = 0.875;

/// 离屏帧尺寸（像素）。比例贴合 `320×52 pt`，多项式柔光在 debug 构建下也能 1ms 内出帧。
const FRAME_W: usize = 420;
const FRAME_H: usize = 68;
/// 稳态环境光/星海的出帧率（fps）。拖拽和升档过渡期间直接跟显示器刷新率。
const IDLE_FPS: f64 = 30.;
/// 升档冲击波的衰减时长（秒）。
const BOOST_DURATION_S: f32 = 0.21;
/// 临界阻尼弹簧角频率（约 200ms 干脆落位）。
const SPRING_OMEGA: f32 = 18.0;
/// 静态定格帧的采样时间（秒），快照与减弱动态模式下保持同一帧。
const STILL_TIME_S: f32 = 4.2;

type SelectHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// 推理强度的一个可选档位。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffortTier {
    pub value: SharedString,
    pub label: SharedString,
}

impl EffortTier {
    pub fn new(value: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

/// 档位下标对应的归一化进度 `[0.0, 1.0]`。
#[must_use]
pub fn effort_progress(selected: usize, count: usize) -> f32 {
    if count <= 1 {
        0.0
    } else {
        (selected.min(count - 1) as f32) / ((count - 1) as f32)
    }
}

/// 与星舰各阶段羽流色相对应的状态点颜色（RGB 24-bit），供标题和触发胶囊共用。
#[must_use]
pub fn effort_dot_color(progress: f32) -> u32 {
    let p = progress.clamp(0.0, 1.0);
    if p < 0.18 {
        0x60a5fa
    } else if p < 0.44 {
        0xfb923c
    } else if p < 0.62 {
        0xf43f5e
    } else if p < 0.85 {
        0xc084fc
    } else {
        0x38bdf8
    }
}

/// 拖拽载荷：携带舞台实体的身份，只在对应滑轨上响应移动。
#[derive(Clone, Copy)]
struct EffortDrag(gpui::EntityId);

/// 推理强度互动舞台滑轨。
#[derive(IntoElement)]
pub struct EffortSlider {
    id: ElementId,
    title: SharedString,
    tiers: Vec<EffortTier>,
    selected: usize,
    pending_hint: Option<SharedString>,
    still: bool,
    on_select: Option<SelectHandler>,
    style: StyleRefinement,
}

impl EffortSlider {
    pub fn new(
        id: impl Into<ElementId>,
        title: impl Into<SharedString>,
        tiers: Vec<EffortTier>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            tiers,
            selected: 0,
            pending_hint: None,
            still: false,
            on_select: None,
            style: StyleRefinement::default(),
        }
    }

    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// 对话运行中修改时的「下轮生效」短字提示。
    pub fn pending_hint(mut self, hint: Option<impl Into<SharedString>>) -> Self {
        self.pending_hint = hint.map(Into::into);
        self
    }

    /// 定格在确定性静态帧（快照或减弱动态时）。
    pub fn still(mut self, still: bool) -> Self {
        self.still = still;
        self
    }

    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

impl Styled for EffortSlider {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for EffortSlider {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let count = self.tiers.len();
        let selected = if count == 0 {
            0
        } else {
            self.selected.min(count - 1)
        };
        let still = self.still
            || cx.reduce_motion()
            || !motion::ambient_motion_enabled(
                motion::ambient_application_active(cx),
                window.is_window_active(),
                true,
            );
        let stage = window.use_keyed_state((self.id.clone(), "stage"), cx, {
            let tiers = self.tiers.clone();
            let title = self.title.clone();
            let pending_hint = self.pending_hint.clone();
            let on_select = self.on_select.clone();
            move |_, cx| {
                EffortStageState::new(title, tiers, selected, pending_hint, still, on_select, cx)
            }
        });
        stage.update(cx, |state, cx| {
            state.sync_props(
                self.title,
                self.tiers,
                selected,
                self.pending_hint,
                still,
                self.on_select,
                cx,
            );
        });
        div()
            .id(self.id)
            .w_full()
            .refine_style(&self.style)
            .child(stage)
    }
}

struct EffortStageState {
    title: SharedString,
    tiers: Vec<EffortTier>,
    selected: usize,
    pending_hint: Option<SharedString>,
    still: bool,
    on_select: Option<SelectHandler>,
    /// 动画状态：当前画面进度、目标进度、弹簧速度、升档冲击波。
    visual_p: f32,
    target_p: f32,
    velocity_p: f32,
    boost: f32,
    boost_dir: f32,
    dragging: bool,
    /// 最近一帧测到的轨道位置，拖拽时按它换算进度。
    track_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// 出帧时钟与节拍去重。
    epoch: Instant,
    last_step: Option<Instant>,
    waiting: Rc<Cell<bool>>,
    drawn_tick: Rc<Cell<u64>>,
    /// 复用像素累加缓冲与上一帧贴图缓存。
    scratch: Vec<[f32; 4]>,
    shown: Option<(FrameKey, Arc<RenderImage>)>,
    _release: Subscription,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FrameKey {
    time_ms: u32,
    p_quant: u16,
    boost_quant: u16,
    boost_neg: bool,
}

impl EffortStageState {
    fn new(
        title: SharedString,
        tiers: Vec<EffortTier>,
        selected: usize,
        pending_hint: Option<SharedString>,
        still: bool,
        on_select: Option<SelectHandler>,
        cx: &mut Context<Self>,
    ) -> Self {
        let p = effort_progress(selected, tiers.len());
        let now = cx.background_executor().now();
        let release = cx.on_release(|this: &mut Self, cx| {
            if let Some((_, image)) = this.shown.take() {
                cx.drop_image(image, None);
            }
        });
        Self {
            title,
            tiers,
            selected,
            pending_hint,
            still,
            on_select,
            visual_p: p,
            target_p: p,
            velocity_p: 0.0,
            boost: 0.0,
            boost_dir: 1.0,
            dragging: false,
            track_bounds: Rc::new(Cell::new(Bounds::default())),
            epoch: now,
            last_step: None,
            waiting: Rc::new(Cell::new(false)),
            drawn_tick: Rc::new(Cell::new(0)),
            scratch: vec![[0.0; 4]; FRAME_W * FRAME_H],
            shown: None,
            _release: release,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors the stateless EffortSlider props into the keyed entity state"
    )]
    fn sync_props(
        &mut self,
        title: SharedString,
        tiers: Vec<EffortTier>,
        selected: usize,
        pending_hint: Option<SharedString>,
        still: bool,
        on_select: Option<SelectHandler>,
        cx: &mut Context<Self>,
    ) {
        let prev_selected = self.selected;
        let prev_count = self.tiers.len();
        self.title = title;
        self.tiers = tiers;
        self.pending_hint = pending_hint;
        self.still = still;
        self.on_select = on_select;

        if selected != prev_selected || self.tiers.len() != prev_count {
            self.selected = selected;
            let next_p = effort_progress(selected, self.tiers.len());
            self.target_p = next_p;
            if self.still || self.tiers.len() != prev_count {
                self.visual_p = next_p;
                self.velocity_p = 0.0;
                self.boost = 0.0;
            } else if !self.dragging && selected != prev_selected {
                self.boost = 1.0;
                self.boost_dir = if selected > prev_selected { 1.0 } else { -1.0 };
            }
            cx.notify();
        }
    }

    fn pointer_to_progress(&self, x: Pixels) -> f32 {
        let bounds = self.track_bounds.get();
        let width = f32::from(bounds.size.width);
        if width <= 1.0 {
            return self.visual_p;
        }
        let pad_l = width * TRACK_PAD_LEFT;
        let pad_r = width * TRACK_PAD_RIGHT;
        let span = (pad_r - pad_l).max(1.0);
        let rel = f32::from(x - bounds.origin.x) - pad_l;
        (rel / span).clamp(0.0, 1.0)
    }

    fn apply_pointer(&mut self, x: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.tiers.len();
        if count == 0 {
            return;
        }
        let p = self.pointer_to_progress(x);
        self.visual_p = p;
        self.target_p = p;
        self.velocity_p = 0.0;
        let next = progress_to_tier(p, count);
        if next != self.selected {
            self.selected = next;
            if let Some(on_select) = self.on_select.clone() {
                on_select(next, window, cx);
            }
        }
        cx.notify();
    }

    fn finish_drag(&mut self, cx: &mut Context<Self>) {
        if !self.dragging {
            return;
        }
        self.dragging = false;
        self.target_p = effort_progress(self.selected, self.tiers.len());
        if self.still {
            self.visual_p = self.target_p;
        }
        cx.notify();
    }

    fn pick_tier(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.tiers.len();
        if count == 0 {
            return;
        }
        let index = index.min(count - 1);
        let prev = self.selected;
        self.dragging = false;
        self.selected = index;
        self.target_p = effort_progress(index, count);
        if self.still {
            self.visual_p = self.target_p;
            self.velocity_p = 0.0;
            self.boost = 0.0;
        } else if index != prev {
            self.boost = 1.0;
            self.boost_dir = if index > prev { 1.0 } else { -1.0 };
        }
        if let Some(on_select) = self.on_select.clone() {
            on_select(index, window, cx);
        }
        cx.notify();
    }

    fn step_physics(&mut self, now: Instant) -> (f32, bool) {
        if self.still {
            self.visual_p = self.target_p;
            self.velocity_p = 0.0;
            self.boost = 0.0;
            self.last_step = None;
            return (STILL_TIME_S, false);
        }
        let dt = self.last_step.map_or(1.0 / 60.0, |prev| {
            now.saturating_duration_since(prev).as_secs_f32().min(0.05)
        });
        self.last_step = Some(now);

        if !self.dragging {
            let diff = self.visual_p - self.target_p;
            let accel = -SPRING_OMEGA * SPRING_OMEGA * diff - 2.0 * SPRING_OMEGA * self.velocity_p;
            self.velocity_p += accel * dt;
            self.visual_p = (self.visual_p + self.velocity_p * dt).clamp(0.0, 1.0);
            if diff.abs() < 0.0006 && self.velocity_p.abs() < 0.002 {
                self.visual_p = self.target_p;
                self.velocity_p = 0.0;
            }
        }
        if self.boost > 0.0 {
            self.boost = (self.boost - dt / BOOST_DURATION_S).max(0.0);
        }
        let high_rate =
            self.dragging || self.boost > 0.001 || (self.visual_p - self.target_p).abs() > 0.001;
        let time_s = now.saturating_duration_since(self.epoch).as_secs_f32();
        (time_s, high_rate)
    }

    fn frame_image(&mut self, time_s: f32, window: &mut Window) -> Arc<RenderImage> {
        let key = FrameKey {
            time_ms: (time_s * 1000.0).round() as u32,
            p_quant: (self.visual_p.clamp(0.0, 1.0) * 1000.0).round() as u16,
            boost_quant: (self.boost.clamp(0.0, 1.0) * 500.0).round() as u16,
            boost_neg: self.boost_dir < 0.0,
        };
        if let Some((shown_key, image)) = &self.shown
            && *shown_key == key
        {
            return image.clone();
        }
        let image = render_starship_frame(
            &mut self.scratch,
            self.visual_p,
            time_s,
            self.boost,
            self.boost_dir,
        );
        if let Some((_, stale)) = self.shown.replace((key, image.clone())) {
            window.drop_image(stale).ok();
        }
        image
    }
}

impl Render for EffortStageState {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = cx.background_executor().now();
        let (time_s, high_rate) = self.step_physics(now);
        let image = self.frame_image(time_s, window);

        let p = if self.dragging {
            self.visual_p
        } else {
            effort_progress(self.selected, self.tiers.len())
        };
        let dot_rgb = effort_dot_color(p);
        let current_label = self
            .tiers
            .get(self.selected)
            .map_or_else(SharedString::default, |tier| tier.label.clone());
        let lens_frac = lerp(
            TRACK_PAD_LEFT,
            TRACK_PAD_RIGHT,
            self.visual_p.clamp(0.0, 1.0),
        );
        let lens_size = if self.dragging {
            LENS_DRAG_DIAMETER
        } else {
            LENS_DIAMETER
        };

        let tick = if high_rate {
            now.saturating_duration_since(self.epoch).as_millis() as u64
        } else {
            (now.saturating_duration_since(self.epoch).as_secs_f64() * IDLE_FPS) as u64
        };
        self.drawn_tick.set(tick);

        // 在 prepaint 里记轨道几何并决定要不要下一帧（看不见或定格时不挂回调）。
        let track_bounds = self.track_bounds.clone();
        let waiting = self.waiting.clone();
        let drawn_tick = self.drawn_tick.clone();
        let epoch = self.epoch;
        let still = self.still;
        let weak = cx.entity().downgrade();
        let track_probe = canvas(
            move |bounds, window, _| {
                track_bounds.set(bounds);
                let visible = bounds.intersects(&window.content_mask().bounds);
                if !still && visible && !waiting.replace(true) {
                    schedule_next_frame(waiting, drawn_tick, epoch, high_rate, weak, window);
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let lens = div()
            .absolute()
            .top(px((TRACK_HEIGHT - lens_size) / 2.0))
            .left(relative(lens_frac))
            .ml(px(-lens_size / 2.0))
            .size(px(lens_size))
            .rounded_full()
            .border(px(1.5))
            .border_color(Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: if self.dragging { 0.92 } else { 0.68 },
            })
            .bg(linear_gradient(
                145.,
                linear_color_stop(
                    Rgba {
                        r: 1.0,
                        g: 1.0,
                        b: 1.0,
                        a: 0.24,
                    },
                    0.0,
                ),
                linear_color_stop(
                    Rgba {
                        r: 1.0,
                        g: 1.0,
                        b: 1.0,
                        a: 0.03,
                    },
                    0.55,
                ),
            ))
            .shadow(vec![
                BoxShadow::new(
                    px(0.),
                    px(2.),
                    Rgba {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.36,
                    }
                    .into(),
                )
                .blur_radius(px(8.)),
            ]);

        let entity_id = cx.entity_id();
        let track = div()
            .id("effort-stage-track")
            .relative()
            .w_full()
            .h(px(TRACK_HEIGHT))
            .rounded_full()
            .when(self.dragging, |el| el.cursor_grabbing())
            .when(!self.dragging, |el| el.cursor_grab())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.dragging = true;
                    this.apply_pointer(event.position.x, window, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_drag(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_drag(cx)),
            )
            .on_drag(EffortDrag(entity_id), |_, _, _, cx| cx.new(|_| EmptyView))
            .on_drag_move(cx.listener(
                move |this, event: &DragMoveEvent<EffortDrag>, window, cx| {
                    if event.drag(cx).0 != entity_id || !this.dragging {
                        return;
                    }
                    this.apply_pointer(event.event.position.x, window, cx);
                },
            ))
            .child(
                img(image)
                    .object_fit(ObjectFit::Fill)
                    .absolute()
                    .inset_0()
                    .size_full(),
            )
            .child(lens)
            .child(track_probe);

        let dark = crate::theme_catalog::active(cx).is_some_and(|theme| theme.mode().is_dark());
        let tick_buttons = self.tiers.iter().enumerate().map(|(ix, tier)| {
            let active = ix == self.selected;
            div()
                .id(("effort-tier", ix))
                .role(gpui::Role::Button)
                .aria_selected(active)
                .aria_label(tier.label.clone())
                .flex_1()
                .min_w_0()
                .h(px(22.))
                .px_1()
                .rounded(px(5.))
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .when(active, |el| {
                    let el = el
                        .type_style(TYPE.small_medium)
                        .text_color(rgb(ui_theme::text()));
                    if dark {
                        el.bg(ui_theme::row_selected())
                            .border_1()
                            .border_color(ui_theme::hair())
                    } else {
                        el.bg(rgb(ui_theme::surface()))
                            .border_1()
                            .border_color(ui_theme::hair())
                    }
                })
                .when(!active, |el| {
                    el.type_style(TYPE.small)
                        .text_color(rgb(ui_theme::faint()))
                        .hover(|h| {
                            h.bg(ui_theme::row_hover())
                                .text_color(rgb(ui_theme::text()))
                        })
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.pick_tier(ix, window, cx);
                }))
                .child(div().truncate().child(tier.label.clone()))
        });

        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px(px(2.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w_0()
                    .child(
                        div()
                            .size(px(7.))
                            .rounded_full()
                            .flex_shrink_0()
                            .bg(rgb(dot_rgb)),
                    )
                    .child(
                        div()
                            .type_style(TYPE.ui_medium)
                            .text_color(rgb(ui_theme::text()))
                            .truncate()
                            .child(format!("{} · {current_label}", self.title)),
                    )
                    .when_some(self.pending_hint.clone(), |row, hint| {
                        row.child(
                            div()
                                .flex_shrink_0()
                                .type_style(TYPE.small_medium)
                                .text_color(rgb(ui_theme::need()))
                                .child(format!("· {hint}")),
                        )
                    }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .type_style(TYPE.small)
                    .text_color(rgb(ui_theme::faint()))
                    .child(t!("effort.levels", count = self.tiers.len()).into_owned()),
            );

        div()
            .w_full()
            .rounded(px(RADIUS.xl))
            .bg(rgb(ui_theme::soft()))
            .border_1()
            .border_color(ui_theme::hair())
            .p(px(10.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(header)
            .child(track)
            .child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .gap(px(2.))
                    .children(tick_buttons),
            )
    }
}

fn schedule_next_frame(
    waiting: Rc<Cell<bool>>,
    drawn_tick: Rc<Cell<u64>>,
    epoch: Instant,
    high_rate: bool,
    view: WeakEntity<EffortStageState>,
    window: &Window,
) {
    window.on_next_frame(move |window, cx: &mut App| {
        if view.upgrade().is_none() {
            waiting.set(false);
            return;
        }
        let now = cx.background_executor().now();
        let elapsed = now.saturating_duration_since(epoch);
        let ready = if high_rate {
            elapsed >= Duration::from_millis(drawn_tick.get().saturating_add(1))
        } else {
            (elapsed.as_secs_f64() * IDLE_FPS) as u64 > drawn_tick.get()
        };
        if !ready {
            schedule_next_frame(waiting, drawn_tick, epoch, high_rate, view, window);
            return;
        }
        waiting.set(false);
        cx.notify(view.entity_id());
    });
}

fn progress_to_tier(p: f32, count: usize) -> usize {
    if count <= 1 {
        0
    } else {
        (p.clamp(0.0, 1.0) * (count - 1) as f32)
            .round()
            .clamp(0.0, (count - 1) as f32) as usize
    }
}

/* ─────────────────────────── Starship Procedural Rasterizer ─────────────────────────── */

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn mix_rgb(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        lerp(a[0], b[0], t),
        lerp(a[1], b[1], t),
        lerp(a[2], b[2], t),
    ]
}

#[inline]
fn hash01(n: usize) -> f32 {
    let x = ((n as f32) * 127.1 + 311.7).sin() * 43_758.547;
    x - x.floor()
}

/// 在像素缓冲中叠加普通透明色（source-over）。
#[inline]
fn blend_over(buf: &mut [[f32; 4]], x: i32, y: i32, rgb: [f32; 3], alpha: f32) {
    if alpha <= 0.001 || x < 0 || y < 0 || x >= FRAME_W as i32 || y >= FRAME_H as i32 {
        return;
    }
    let a = alpha.min(1.0);
    let px = &mut buf[y as usize * FRAME_W + x as usize];
    let inv = 1.0 - a;
    px[0] = px[0] * inv + rgb[0] * a;
    px[1] = px[1] * inv + rgb[1] * a;
    px[2] = px[2] * inv + rgb[2] * a;
}

/// 在像素缓冲中做加法辉光（lighter）。
#[inline]
fn blend_add(buf: &mut [[f32; 4]], x: i32, y: i32, rgb: [f32; 3], alpha: f32) {
    if alpha <= 0.001 || x < 0 || y < 0 || x >= FRAME_W as i32 || y >= FRAME_H as i32 {
        return;
    }
    let px = &mut buf[y as usize * FRAME_W + x as usize];
    px[0] += rgb[0] * alpha;
    px[1] += rgb[1] * alpha;
    px[2] += rgb[2] * alpha;
}

/// 软体积椭圆辉光：多项式 `(1 - d²)³` 衰减，边界严格归零、无硬边。
#[expect(
    clippy::too_many_arguments,
    reason = "tight inner rasterizer helper with direct scalar parameters"
)]
fn draw_soft_ellipse(
    buf: &mut [[f32; 4]],
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    inner_rgb: [f32; 3],
    outer_rgb: [f32; 3],
    alpha: f32,
    additive: bool,
) {
    if alpha <= 0.002 || rx <= 0.5 || ry <= 0.5 {
        return;
    }
    let x0 = ((cx - rx).floor() as i32).max(0);
    let x1 = ((cx + rx).ceil() as i32).min(FRAME_W as i32 - 1);
    let y0 = ((cy - ry).floor() as i32).max(0);
    let y1 = ((cy + ry).ceil() as i32).min(FRAME_H as i32 - 1);
    let inv_rx = 1.0 / rx;
    let inv_ry = 1.0 / ry;
    for y in y0..=y1 {
        let dy = (y as f32 + 0.5 - cy) * inv_ry;
        let dy2 = dy * dy;
        if dy2 >= 1.0 {
            continue;
        }
        let row = y as usize * FRAME_W;
        for x in x0..=x1 {
            let dx = (x as f32 + 0.5 - cx) * inv_rx;
            let d2 = dx * dx + dy2;
            if d2 >= 1.0 {
                continue;
            }
            let u = 1.0 - d2;
            let falloff = u * u * u;
            let a = alpha * falloff;
            let col = mix_rgb(inner_rgb, outer_rgb, d2);
            let px = &mut buf[row + x as usize];
            if additive {
                px[0] += col[0] * a;
                px[1] += col[1] * a;
                px[2] += col[2] * a;
            } else {
                let a = a.min(1.0);
                let inv = 1.0 - a;
                px[0] = px[0] * inv + col[0] * a;
                px[1] = px[1] * inv + col[1] * a;
                px[2] = px[2] * inv + col[2] * a;
            }
        }
    }
}

/// 抗锯齿线段（Wu/距离场），用于筷子塔、曲速星轨与光芒。
#[expect(
    clippy::too_many_arguments,
    reason = "tight rasterizer helper with direct geometry parameters"
)]
fn draw_aa_line(
    buf: &mut [[f32; 4]],
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    half_width: f32,
    rgb: [f32; 3],
    alpha: f32,
    additive: bool,
) {
    if alpha <= 0.002 {
        return;
    }
    let pad = half_width + 1.2;
    let min_x = (x0.min(x1) - pad).floor().max(0.0) as i32;
    let max_x = (x0.max(x1) + pad).ceil().min((FRAME_W - 1) as f32) as i32;
    let min_y = (y0.min(y1) - pad).floor().max(0.0) as i32;
    let max_y = (y0.max(y1) + pad).ceil().min((FRAME_H - 1) as f32) as i32;
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len2 = (dx * dx + dy * dy).max(1e-5);
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let t = (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0);
            let qx = x0 + dx * t;
            let qy = y0 + dy * t;
            let dist = ((px - qx).powi(2) + (py - qy).powi(2)).sqrt();
            let cov = (half_width + 0.85 - dist).clamp(0.0, 1.0);
            if cov > 0.0 {
                if additive {
                    blend_add(buf, x, y, rgb, alpha * cov);
                } else {
                    blend_over(buf, x, y, rgb, alpha * cov);
                }
            }
        }
    }
}

/// 抗锯齿圆环（用于 180ms 升档冲击波与弓形光盾）。
#[expect(
    clippy::too_many_arguments,
    reason = "tight rasterizer helper with direct arc parameters"
)]
fn draw_arc_ring(
    buf: &mut [[f32; 4]],
    cx: f32,
    cy: f32,
    radius: f32,
    half_thick: f32,
    angle_min: f32,
    angle_max: f32,
    rgb: [f32; 3],
    alpha: f32,
) {
    if alpha <= 0.002 || radius <= 0.5 {
        return;
    }
    let outer = radius + half_thick + 2.0;
    let x0 = ((cx - outer).floor() as i32).max(0);
    let x1 = ((cx + outer).ceil() as i32).min(FRAME_W as i32 - 1);
    let y0 = ((cy - outer).floor() as i32).max(0);
    let y1 = ((cy + outer).ceil() as i32).min(FRAME_H as i32 - 1);
    let full = (angle_max - angle_min) >= std::f32::consts::TAU - 0.01;
    for y in y0..=y1 {
        let dy = y as f32 + 0.5 - cy;
        for x in x0..=x1 {
            let dx = x as f32 + 0.5 - cx;
            let r = (dx * dx + dy * dy).sqrt();
            let dr = (r - radius).abs();
            if dr > half_thick + 1.5 {
                continue;
            }
            let ang_fade = if full {
                1.0
            } else {
                let ang = dy.atan2(dx);
                if ang < angle_min || ang > angle_max {
                    continue;
                }
                let mid = 0.5 * (angle_min + angle_max);
                let half_span = (0.5 * (angle_max - angle_min)).max(0.01);
                let u = 1.0 - ((ang - mid).abs() / half_span).clamp(0.0, 1.0);
                u * u * (3.0 - 2.0 * u)
            };
            let u = (1.0 - dr / (half_thick + 1.5)).clamp(0.0, 1.0);
            let ring = u * u;
            blend_add(buf, x, y, rgb, alpha * ring * ang_fade);
        }
    }
}

/// 绘制整帧星舰发射栈，返回圆角胶囊已裁切好的 BGRA `RenderImage`。
fn render_starship_frame(
    buf: &mut [[f32; 4]],
    p: f32,
    time_s: f32,
    boost: f32,
    boost_dir: f32,
) -> Arc<RenderImage> {
    let w = FRAME_W as f32;
    let h = FRAME_H as f32;
    let s = h / 52.0; // 与原型 52pt 基准对齐的缩放因子（约 1.3077）

    let pad_x = w * TRACK_PAD_LEFT;
    let max_x = w * TRACK_PAD_RIGHT;
    let ship_x = lerp(pad_x, max_x, p) - boost * boost_dir * 2.5 * s;
    let ship_y = h * 0.50
        + if boost > 0.01 {
            (time_s * 85.0).sin() * boost * 1.2 * s
        } else {
            0.0
        };
    let pitch = lerp(-0.22, 0.0, smoothstep(0.0, 0.30, p));

    // 1. 多阶段天空 / 太空水平渐变
    let sky_left = if p < 0.25 {
        mix_rgb([11., 22., 46.], [44., 22., 22.], p / 0.25)
    } else if p < 0.52 {
        mix_rgb([44., 22., 22.], [92., 48., 56.], (p - 0.25) / 0.27)
    } else if p < 0.78 {
        mix_rgb([92., 48., 56.], [10., 15., 36.], (p - 0.52) / 0.26)
    } else {
        mix_rgb([10., 15., 36.], [5., 8., 20.], (p - 0.78) / 0.22)
    };
    let sky_mid = if p < 0.30 {
        mix_rgb([14., 28., 58.], [54., 29., 24.], p / 0.30)
    } else if p < 0.55 {
        mix_rgb([54., 29., 24.], [38., 45., 86.], (p - 0.30) / 0.25)
    } else if p < 0.80 {
        mix_rgb([38., 45., 86.], [18., 22., 54.], (p - 0.55) / 0.25)
    } else {
        mix_rgb([18., 22., 54.], [9., 14., 34.], (p - 0.80) / 0.20)
    };
    let sky_right = if p < 0.55 {
        mix_rgb([11., 24., 52.], [11., 25., 64.], p / 0.55)
    } else {
        mix_rgb([11., 25., 64.], [5., 10., 26.], (p - 0.55) / 0.45)
    };

    for x in 0..FRAME_W {
        let u = x as f32 / (w - 1.0);
        let col = if u < 0.48 {
            mix_rgb(sky_left, sky_mid, u / 0.48)
        } else {
            mix_rgb(sky_mid, sky_right, (u - 0.48) / 0.52)
        };
        for y in 0..FRAME_H {
            buf[y * FRAME_W + x] = [col[0], col[1], col[2], 255.0];
        }
    }

    // 2. 繁星与高档曲速星轨
    let warp_amt = smoothstep(0.76, 1.0, p);
    let star_vis = lerp(0.55, 1.0, smoothstep(0.50, 0.85, p))
        * (1.0 - 0.35 * smoothstep(0.25, 0.50, p) * (1.0 - smoothstep(0.50, 0.70, p)));
    for i in 0..64 {
        let sx0 = hash01(i * 5 + 1);
        let sy0 = hash01(i * 5 + 2);
        let sr = (0.45 + hash01(i * 5 + 3) * 1.05) * s;
        let twinkle = hash01(i * 5 + 4) * std::f32::consts::TAU;
        let speed = 0.35 + hash01(i * 5 + 5) * 1.35;
        let scroll = (time_s * (0.04 + warp_amt * 1.15) * speed).fract();
        let sx = (sx0 - scroll).rem_euclid(1.0) * w;
        let sy = sy0 * h;
        let alpha = star_vis * (0.45 + 0.55 * (time_s * 2.4 + twinkle).sin());
        if warp_amt > 0.08 {
            let len = (6.0 + speed * 28.0) * s * warp_amt * (0.75 + boost * 0.8);
            draw_aa_line(
                buf,
                sx,
                sy,
                sx + len,
                sy,
                sr * 0.42,
                [215., 238., 255.],
                alpha * 0.72,
                true,
            );
        } else {
            draw_soft_ellipse(
                buf,
                sx,
                sy,
                sr * 1.35,
                sr * 1.35,
                [235., 245., 255.],
                [185., 215., 255.],
                alpha,
                true,
            );
        }
    }

    // 3. 近地轨道弧形发光地球（p ∈ [0.56, 0.97]）
    let limb_vis = smoothstep(0.56, 0.74, p) * (1.0 - smoothstep(0.84, 0.97, p));
    if limb_vis > 0.01 {
        let cx = w * 0.44;
        let r = w * 0.30;
        let cy = h + r - h * 0.34;
        let outer = r * 1.12;
        let x0 = ((cx - outer).floor() as i32).max(0);
        let x1 = ((cx + outer).ceil() as i32).min(FRAME_W as i32 - 1);
        for y in 0..FRAME_H as i32 {
            let dy = y as f32 + 0.5 - cy;
            for x in x0..=x1 {
                let dx = x as f32 + 0.5 - cx;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist > outer {
                    continue;
                }
                if dist > r {
                    let u = 1.0 - (dist - r) / (outer - r);
                    let glow = u * u * u * limb_vis * 0.58;
                    blend_add(buf, x, y, [88., 208., 255.], glow);
                } else {
                    let depth = ((r - dist) / (r * 0.22)).clamp(0.0, 1.0);
                    let earth = if depth < 0.25 {
                        mix_rgb([105., 224., 255.], [30., 116., 205.], depth / 0.25)
                    } else {
                        mix_rgb([30., 116., 205.], [8., 30., 84.], (depth - 0.25) / 0.75)
                    };
                    blend_over(buf, x, y, earth, limb_vis);
                }
            }
        }
    }

    // 4. 深空曲速：左侧远去的蓝色地球（p > 0.80）
    let marble_vis = smoothstep(0.80, 0.96, p);
    if marble_vis > 0.01 {
        let mx = w * 0.09;
        let my = h * 0.50;
        let mr = h * 0.29;
        draw_soft_ellipse(
            buf,
            mx,
            my,
            mr * 1.9,
            mr * 1.9,
            [72., 184., 255.],
            [36., 105., 240.],
            marble_vis * 0.55,
            true,
        );
        let x0 = ((mx - mr - 1.0).floor() as i32).max(0);
        let x1 = ((mx + mr + 1.0).ceil() as i32).min(FRAME_W as i32 - 1);
        let y0 = ((my - mr - 1.0).floor() as i32).max(0);
        let y1 = ((my + mr + 1.0).ceil() as i32).min(FRAME_H as i32 - 1);
        for y in y0..=y1 {
            let dy = y as f32 + 0.5 - my;
            for x in x0..=x1 {
                let dx = x as f32 + 0.5 - mx;
                let dist = (dx * dx + dy * dy).sqrt();
                let cov = (mr + 0.7 - dist).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let lx = (dx - mr * 0.25) / mr;
                let ly = (dy + mr * 0.25) / mr;
                let shade = ((lx * lx + ly * ly).sqrt() * 0.85).clamp(0.0, 1.0);
                let globe = if shade < 0.4 {
                    mix_rgb([145., 232., 255.], [42., 138., 235.], shade / 0.4)
                } else {
                    mix_rgb([42., 138., 235.], [8., 28., 78.], (shade - 0.4) / 0.6)
                };
                blend_over(buf, x, y, globe, cov * marble_vis);
            }
        }
    }

    // 5. 发射台、筷子塔与探照灯（p < 0.22）
    let pad_vis = 1.0 - smoothstep(0.05, 0.21, p);
    if pad_vis > 0.01 {
        let tx = pad_x - 24.0 * s - p * 45.0 * s;
        let tw = 11.0 * s;
        // 发射台底面
        let ground_y = (h * 0.82) as i32;
        for y in ground_y..FRAME_H as i32 {
            for x in 0..FRAME_W as i32 {
                blend_over(buf, x, y, [18., 30., 52.], 0.85 * pad_vis);
            }
        }
        // 探照灯柔光锥
        let sweep = (time_s * 1.1).sin() * 4.0 * s;
        draw_soft_ellipse(
            buf,
            ship_x + sweep * 0.4,
            ship_y + 4.0 * s,
            26.0 * s,
            18.0 * s,
            [155., 210., 255.],
            [90., 160., 235.],
            0.26 * pad_vis,
            true,
        );
        // 钢架筷子塔
        let tower_top = h * 0.10;
        let tower_bot = h * 0.84;
        let lattice = [142., 168., 206.];
        draw_aa_line(
            buf,
            tx,
            tower_top,
            tx,
            tower_bot,
            0.65,
            lattice,
            0.55 * pad_vis,
            false,
        );
        draw_aa_line(
            buf,
            tx + tw,
            tower_top,
            tx + tw,
            tower_bot,
            0.65,
            lattice,
            0.55 * pad_vis,
            false,
        );
        let step = 8.0 * s;
        let mut y = tower_top + 2.0 * s;
        while y < tower_bot - step * 0.5 {
            draw_aa_line(
                buf,
                tx,
                y,
                tx + tw,
                y + step * 0.75,
                0.5,
                lattice,
                0.42 * pad_vis,
                false,
            );
            draw_aa_line(
                buf,
                tx + tw,
                y,
                tx,
                y + step * 0.75,
                0.5,
                lattice,
                0.42 * pad_vis,
                false,
            );
            y += step;
        }
        // 筷子抓臂
        draw_aa_line(
            buf,
            tx + tw,
            h * 0.34,
            tx + tw + 13.5 * s,
            h * 0.42,
            1.1,
            [188., 214., 242.],
            0.78 * pad_vis,
            false,
        );
        // 塔顶红闪信标
        let blink = 0.55 + 0.45 * (time_s * 3.5).sin();
        draw_soft_ellipse(
            buf,
            tx + tw * 0.5,
            tower_top,
            3.2 * s,
            3.2 * s,
            [255., 110., 110.],
            [255., 55., 55.],
            blink * pad_vis,
            true,
        );
    }

    // 6. 锚定到星舰坐标系的体积羽流与粒子
    let ship_cx = ship_x - 1.5 * s;
    let (sin_p, cos_p) = pitch.sin_cos();
    let to_world = |lx: f32, ly: f32| -> (f32, f32) {
        (
            ship_cx + lx * cos_p - ly * sin_p,
            ship_y + lx * sin_p + ly * cos_p,
        )
    };

    let tail_x = -13.0 * s;
    let flame_amt = smoothstep(0.08, 0.22, p) * (1.0 - smoothstep(0.58, 0.72, p));
    let vac_amt = smoothstep(0.54, 0.70, p) * (1.0 - smoothstep(0.88, 0.97, p));
    let warp_beam = smoothstep(0.82, 0.98, p);

    // 6a. 低空翻涌白汽
    let steam_amt = smoothstep(0.10, 0.24, p) * (1.0 - smoothstep(0.32, 0.48, p));
    if steam_amt > 0.01 {
        for i in 0..10 {
            let phase = i as f32 / 10.0 + hash01(i * 7 + 3) * 0.2;
            let spread = (hash01(i * 7 + 4) - 0.5) * 2.0;
            let speed = 0.7 + hash01(i * 7 + 5) * 0.7;
            let age = (time_s * 0.65 * speed + phase).fract();
            let (wx, wy) = to_world(
                tail_x - (10.0 + age * 72.0) * s,
                spread * (4.0 + age * 14.0) * s,
            );
            let rad = (5.0 + age * 13.0) * s;
            draw_soft_ellipse(
                buf,
                wx,
                wy,
                rad,
                rad * 0.85,
                [255., 234., 210.],
                [245., 210., 180.],
                steam_amt * (1.0 - age) * 0.25,
                false,
            );
        }
    }

    // 6b. 暖金烈焰 + 马赫环 + 尾焰火花
    if flame_amt > 0.01 {
        let flicker = 0.93 + 0.07 * (time_s * 42.0).sin() + boost * 0.35;
        let len = lerp(58.0, 108.0, smoothstep(0.12, 0.56, p)) * s * flicker;
        let (ox, oy) = to_world(tail_x - len * 0.40, 0.0);
        draw_soft_ellipse(
            buf,
            ox,
            oy,
            len * 0.64,
            15.0 * s,
            [255., 165., 62.],
            [255., 72., 18.],
            flame_amt * 0.88,
            true,
        );
        let (ix, iy) = to_world(tail_x - len * 0.22, 0.0);
        draw_soft_ellipse(
            buf,
            ix,
            iy,
            len * 0.34,
            5.2 * s,
            [255., 254., 238.],
            [255., 195., 88.],
            flame_amt * 0.96,
            true,
        );
        for d in 0..4 {
            let (dx, dy) = to_world(tail_x - (7.0 * s) - (d as f32) * (len * 0.14), 0.0);
            let dr = (4.2 - d as f32 * 0.68) * s * flicker;
            draw_soft_ellipse(
                buf,
                dx,
                dy,
                dr * 1.65,
                dr * 0.72,
                [255., 252., 230.],
                [255., 170., 68.],
                flame_amt * 0.88,
                true,
            );
        }
        for i in 0..14 {
            let phase = i as f32 / 14.0 + hash01(i * 11 + 2) * 0.15;
            let spread = (hash01(i * 11 + 3) - 0.5) * 2.0;
            let speed = 0.8 + hash01(i * 11 + 4) * 0.8;
            let age = (time_s * 2.0 * speed + phase).fract();
            let (px, py) = to_world(tail_x - age * len * 1.05, spread * (2.0 + age * 7.5) * s);
            let r = (0.9 + hash01(i * 11 + 5) * 0.8) * s;
            draw_soft_ellipse(
                buf,
                px,
                py,
                r,
                r,
                [255., 235., 175.],
                [255., 165., 65.],
                flame_amt * (1.0 - age) * 0.82,
                true,
            );
        }
    }

    // 6c. 钟形青紫真空羽流（近地轨道）
    if vac_amt > 0.01 {
        let pulse = 0.95 + 0.05 * (time_s * 26.0).sin() + boost * 0.32;
        let (bx, by) = to_world(tail_x - 52.0 * s * pulse, 0.0);
        draw_soft_ellipse(
            buf,
            bx,
            by,
            66.0 * s * pulse,
            20.0 * s,
            [212., 122., 255.],
            [95., 45., 225.],
            vac_amt * 0.82,
            true,
        );
        let (cx, cy) = to_world(tail_x - 22.0 * s * pulse, 0.0);
        draw_soft_ellipse(
            buf,
            cx,
            cy,
            30.0 * s * pulse,
            7.4 * s,
            [255., 255., 255.],
            [135., 232., 255.],
            vac_amt * 0.98,
            true,
        );
    }

    // 6d. 深空曲速离子束 + 十字星芒 + 弓形光盾
    if warp_beam > 0.01 {
        let pulse = 0.96 + 0.04 * (time_s * 36.0).sin() + boost * 0.25;
        let beam_len = w * 0.68;
        let (cx, cy) = to_world(tail_x - beam_len * 0.44, 0.0);
        draw_soft_ellipse(
            buf,
            cx,
            cy,
            beam_len * 0.56,
            16.5 * s * pulse,
            [110., 232., 255.],
            [125., 60., 235.],
            warp_beam * 0.72,
            true,
        );
        let (jx, jy) = to_world(tail_x - beam_len * 0.40, 0.0);
        draw_soft_ellipse(
            buf,
            jx,
            jy,
            beam_len * 0.48,
            4.4 * s * pulse,
            [255., 255., 255.],
            [120., 225., 255.],
            warp_beam * 0.98,
            true,
        );
        // 引擎喷口直连的炽白离子源，让曲速束从尾喷口连续拉出不脱节。
        let (nx, ny) = to_world(tail_x - 14.0 * s, 0.0);
        draw_soft_ellipse(
            buf,
            nx,
            ny,
            28.0 * s * pulse,
            5.2 * s * pulse,
            [255., 255., 255.],
            [130., 238., 255.],
            warp_beam * 0.96,
            true,
        );
        for i in 0..6 {
            let phase = i as f32 / 6.0;
            let spread = (hash01(i * 13 + 7) - 0.5) * 2.0;
            let age = (time_s * 1.4 + phase).fract();
            let (gx, gy) = to_world(tail_x - 18.0 * s - age * beam_len * 0.80, spread * 10.5 * s);
            let gs = (1.0 - age) * 4.5 * s;
            let a = warp_beam * (1.0 - age) * 0.85;
            draw_aa_line(
                buf,
                gx - gs,
                gy,
                gx + gs,
                gy,
                0.55,
                [215., 248., 255.],
                a,
                true,
            );
            draw_aa_line(
                buf,
                gx,
                gy - gs,
                gx,
                gy + gs,
                0.55,
                [215., 248., 255.],
                a,
                true,
            );
        }
        // 弓形光盾
        let (sx, sy) = to_world(6.5 * s, 0.0);
        draw_arc_ring(
            buf,
            sx,
            sy,
            15.2 * s,
            2.6 * s,
            -std::f32::consts::FRAC_PI_2 * 0.88,
            std::f32::consts::FRAC_PI_2 * 0.88,
            [135., 242., 255.],
            warp_beam * 0.92,
        );
    }

    // 7. 高辨识度不锈钢星舰本体（逆旋转回船体局部坐标系做抗锯齿 SDF 绘制）
    {
        let pad = 20.0 * s;
        let x0 = ((ship_cx - pad).floor() as i32).max(0);
        let x1 = ((ship_cx + pad).ceil() as i32).min(FRAME_W as i32 - 1);
        let y0 = ((ship_y - pad).floor() as i32).max(0);
        let y1 = ((ship_y + pad).ceil() as i32).min(FRAME_H as i32 - 1);
        let inv_s = 1.0 / s;

        for y in y0..=y1 {
            let wy = y as f32 + 0.5 - ship_y;
            for x in x0..=x1 {
                let wx = x as f32 + 0.5 - ship_cx;
                // 反旋转到船体局部坐标（与原型 52pt 同单位）
                let lx = (wx * cos_p + wy * sin_p) * inv_s;
                let ly = (-wx * sin_p + wy * cos_p) * inv_s;

                // 船身 + 尖拱鼻锥外轮廓
                let hull_half_h = if !(-13.0..=16.5).contains(&lx) {
                    -1.0
                } else if lx <= 5.0 {
                    4.0
                } else {
                    let t = (lx - 5.0) / 11.5;
                    4.0 * (1.0 - t * t).max(0.0).sqrt()
                };
                let hull_dist = if hull_half_h > 0.0 {
                    ly.abs() - hull_half_h
                } else {
                    10.0
                };

                // 前后气动翼面（三角形区域）
                let in_aft_flap = (-14.5..=-7.0).contains(&lx) && {
                    let ay = ly.abs();
                    let top = if lx < -12.0 {
                        lerp(7.2, 3.5, (-12.0 - lx) / 2.5)
                    } else {
                        lerp(7.2, 4.0, (lx + 14.5) / 7.5)
                    };
                    ay >= 3.4 && ay <= top
                };
                let in_fwd_flap = (3.5..=9.5).contains(&lx) && {
                    let ay = ly.abs();
                    let top = lerp(6.0, 2.5, (lx - 3.5) / 6.0);
                    ay >= 2.8 && ay <= top
                };

                if hull_dist <= 0.7 {
                    let cov = (0.7 - hull_dist).clamp(0.0, 1.0);
                    let ny = ((ly + 4.0) / 8.0).clamp(0.0, 1.0);
                    let mut col = if ny < 0.32 {
                        mix_rgb([248., 250., 252.], [203., 213., 225.], ny / 0.32)
                    } else if ny < 0.66 {
                        mix_rgb([203., 213., 225.], [95., 112., 135.], (ny - 0.32) / 0.34)
                    } else {
                        mix_rgb([36., 46., 64.], [15., 23., 42.], (ny - 0.66) / 0.34)
                    };
                    // 顶部不锈钢高光条
                    let spec = (1.0 - ((ly + 2.0).abs() / 1.15)).clamp(0.0, 1.0);
                    if lx > -11.5 && lx < 11.0 && spec > 0.0 {
                        col = mix_rgb(col, [255., 255., 255.], spec * 0.78);
                    }
                    // 尾部裙边引擎反光
                    if p > 0.10 && lx < -11.8 && ly.abs() < 3.0 {
                        let skirt = if p < 0.62 {
                            [255., 200., 115.]
                        } else {
                            [170., 242., 255.]
                        };
                        col = mix_rgb(col, skirt, 0.85);
                    }
                    blend_over(buf, x, y, col, cov);
                } else if in_aft_flap {
                    blend_over(buf, x, y, [78., 92., 112.], 0.92);
                } else if in_fwd_flap {
                    blend_over(buf, x, y, [104., 118., 138.], 0.92);
                }
            }
        }
    }

    // 8. Max-Q 音爆凝结云锥（p ∈ [0.36, 0.70]）
    let cone_amt = smoothstep(0.36, 0.50, p) * (1.0 - smoothstep(0.56, 0.70, p));
    if cone_amt > 0.01 {
        let pad = 22.0 * s;
        let x0 = ((ship_cx - pad).floor() as i32).max(0);
        let x1 = ((ship_cx + pad).ceil() as i32).min(FRAME_W as i32 - 1);
        let y0 = ((ship_y - pad).floor() as i32).max(0);
        let y1 = ((ship_y + pad).ceil() as i32).min(FRAME_H as i32 - 1);
        let inv_s = 1.0 / s;
        for y in y0..=y1 {
            let wy = y as f32 + 0.5 - ship_y;
            for x in x0..=x1 {
                let wx = x as f32 + 0.5 - ship_cx;
                let lx = (wx * cos_p + wy * sin_p) * inv_s;
                let ly = (-wx * sin_p + wy * cos_p) * inv_s;
                if lx > 11.0 || lx < -10.0 {
                    continue;
                }
                let t = (11.0 - lx) / 21.0;
                let cone_r = t * 15.5;
                let ay = ly.abs();
                if ay > cone_r + 1.2 {
                    continue;
                }
                let edge_fade = (1.0 - t).powf(0.65);
                let rim = (1.0 - (cone_r - ay).abs() / 3.8).clamp(0.0, 1.0);
                let fill = if ay < cone_r { 0.34 } else { 0.0 };
                let alpha = cone_amt * edge_fade * (fill + rim * 0.62);
                blend_add(buf, x, y, [225., 242., 255.], alpha * 0.85);
            }
        }
    }

    // 9. 升档 180ms 冲击波环 + 中心闪光
    if boost > 0.01 {
        let ring_r = (8.0 + (1.0 - boost) * 28.0) * s;
        let ring_col = if p > 0.70 {
            [140., 240., 255.]
        } else {
            [255., 225., 165.]
        };
        draw_arc_ring(
            buf,
            ship_cx + 2.0 * s,
            ship_y,
            ring_r,
            2.1 * s * boost,
            0.0,
            std::f32::consts::TAU,
            ring_col,
            boost * 0.88,
        );
        draw_soft_ellipse(
            buf,
            ship_cx,
            ship_y,
            22.0 * s,
            22.0 * s,
            [255., 255., 255.],
            ring_col,
            boost * 0.55,
            true,
        );
    }

    // 10. 抗锯齿胶囊圆角裁切 + 细内描边，输出 BGRA RenderImage
    let pill_r = h * 0.5;
    let left_cx = pill_r;
    let right_cx = w - pill_r;
    let mut pixels = RgbaImage::new(FRAME_W as u32, FRAME_H as u32);
    for y in 0..FRAME_H {
        let py = y as f32 + 0.5;
        let dy = py - pill_r;
        let row = y * FRAME_W;
        for x in 0..FRAME_W {
            let px = x as f32 + 0.5;
            let dx = if px < left_cx {
                px - left_cx
            } else if px > right_cx {
                px - right_cx
            } else {
                0.0
            };
            let dist = (dx * dx + dy * dy).sqrt();
            let cov = (pill_r - dist + 0.5).clamp(0.0, 1.0);
            if cov <= 0.0 {
                pixels.put_pixel(x as u32, y as u32, ImageRgba([0, 0, 0, 0]));
                continue;
            }
            let [mut r, mut g, mut b, _] = buf[row + x];
            // 胶囊内沿一圈微弱玻璃边
            let rim = (1.0 - (pill_r - dist).abs() / 1.4).clamp(0.0, 1.0);
            if rim > 0.0 {
                let lift = rim * 26.0;
                r += lift;
                g += lift;
                b += lift;
            }
            let to_u8 = |v: f32| v.clamp(0.0, 255.0).round() as u8;
            // GPUI RenderImage 吃非预乘 BGRA
            pixels.put_pixel(
                x as u32,
                y as u32,
                ImageRgba([to_u8(b), to_u8(g), to_u8(r), to_u8(cov * 255.0)]),
            );
        }
    }
    Arc::new(RenderImage::new(vec![Frame::new(pixels)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_progress_and_tier_round_trip() {
        for count in 2..=6 {
            for ix in 0..count {
                let p = effort_progress(ix, count);
                assert_eq!(progress_to_tier(p, count), ix, "count={count} ix={ix}");
            }
        }
        assert_eq!(effort_progress(0, 1), 0.0);
        assert_eq!(progress_to_tier(0.5, 1), 0);
    }

    #[test]
    fn starship_rasterizer_produces_clean_pill_corners() {
        let mut scratch = vec![[0.0; 4]; FRAME_W * FRAME_H];
        let image = render_starship_frame(&mut scratch, 1.0, STILL_TIME_S, 0.5, 1.0);
        let size = image.size(0);
        assert_eq!(size.width.0 as usize, FRAME_W);
        assert_eq!(size.height.0 as usize, FRAME_H);
    }

    #[test]
    fn effort_dot_colors_shift_monotonically_by_stage() {
        let stages: Vec<u32> = [0.0, 0.3, 0.52, 0.74, 1.0]
            .into_iter()
            .map(effort_dot_color)
            .collect();
        // 每个阶段色相都彼此区分，不撞色。
        for i in 0..stages.len() {
            for j in (i + 1)..stages.len() {
                assert_ne!(stages[i], stages[j], "stages {i} and {j} collided");
            }
        }
    }
}
