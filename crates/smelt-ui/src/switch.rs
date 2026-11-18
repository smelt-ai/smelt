//! 开关：一项设置立刻生效的开或关。规则见 `docs/ui-design-language.md` 第 2、4、7 节。
//!
//! 36×16 的轨道和 21×14 的胶囊滑块，滑块离轨道边 1px；悬停时伸长 2、按下时伸长 5。
//! 开关切换用控件档弹簧。交互与无障碍由 gpui-base 管理。

use std::rc::Rc;

use gpui::{
    App, BoxShadow, ElementId, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels,
    RenderOnce, Rgba, SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Window,
    div, linear_color_stop, linear_gradient, point, prelude::FluentBuilder as _, px, rgb,
};
use gpui_base::{GlobalState, StyledExt as _, Switch as BaseSwitch};

use crate::kit::{
    MOTION,
    elevation::{self, Elevation},
};
use crate::{motion, tooltip::Tooltip, ui_theme};

type ChangeHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

const TRACK_W: f32 = 36.;
const TRACK_H: f32 = 16.;
const THUMB_W: f32 = 21.;
const THUMB_H: f32 = 14.;
const INSET: f32 = 1.;
const HOVER_STRETCH: f32 = 2.;
/// 滑块高的 35%，取整。
const PRESS_STRETCH: f32 = 5.;
const GROUP: &str = "smelt-switch";

/// 右端在开启时固定，左端在关闭时固定；过渡中也保持连续。
fn thumb_geometry(on: f32, stretch: f32) -> (f32, f32) {
    let width = THUMB_W + stretch;
    let left = INSET + (TRACK_W - 2. * INSET - width) * on;
    (left, width)
}

fn gradient(top: Rgba, bottom: Rgba) -> gpui::Background {
    linear_gradient(
        180.,
        linear_color_stop(top, 0.),
        linear_color_stop(bottom, 1.),
    )
}

/// 轨道的内阴影：主题里关、开两套逐层插值，过渡中连续。两套层数不同时多出的层不画。
fn track_shadows(elevation: &Elevation, on: f32) -> Vec<BoxShadow> {
    let lerp = |a: Pixels, b: Pixels| a + (b - a) * on;
    elevation
        .switch_off
        .iter()
        .zip(&elevation.switch_on)
        .map(|(off, on_layer)| BoxShadow {
            color: motion::mix_rgba(off.color.into(), on_layer.color.into(), on).into(),
            offset: point(
                lerp(off.offset.x, on_layer.offset.x),
                lerp(off.offset.y, on_layer.offset.y),
            ),
            blur_radius: lerp(off.blur_radius, on_layer.blur_radius),
            spread_radius: lerp(off.spread_radius, on_layer.spread_radius),
            inset: off.inset,
        })
        .collect()
}

/// 轨道底：上沿是底色，下沿压一层 `alpha` 透明度的白光。
fn track_fill(fill: Rgba, white: Rgba, alpha: f32) -> gpui::Background {
    gradient(fill, fill.blend(Rgba { a: alpha, ..white }))
}

#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    style: StyleRefinement,
    checked: bool,
    disabled: bool,
    accessibility_label: Option<SharedString>,
    tooltip: Option<SharedString>,
    on_change: Option<ChangeHandler>,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            checked: false,
            disabled: false,
            accessibility_label: None,
            tooltip: None,
            on_change: None,
        }
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// 开关旁边通常有可见的标题；读屏用这个名字。
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    /// 拿到切换后的值；调用方存下来再经 [`Switch::checked`] 画回来。
    pub fn on_change(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl Styled for Switch {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Switch {
            id,
            style,
            checked,
            disabled,
            accessibility_label,
            tooltip,
            on_change,
        } = self;
        let interactive = !disabled && on_change.is_some();

        // 控件档弹簧（200ms、临界阻尼）。首帧直接到位，中途改目标保留速度；减弱动态效果时
        // 直接到位。
        let on = gpui_base::motion::spring(
            ElementId::NamedChild(id.clone().into(), "thumb".into()),
            if checked { 1. } else { 0. },
            MOTION.control,
            window,
            cx,
        );
        let (left, width) = thumb_geometry(on, 0.);
        let (hover_left, hover_width) = thumb_geometry(on, HOVER_STRETCH);
        let (press_left, press_width) = thumb_geometry(on, PRESS_STRETCH);
        // 关：最高一层底、底边白光 20%；开：强调色、白光 10%。悬停在关上叠 `hover`，在开上叠
        // `contrast_hover`。滑块用主题的开关钮色，上亮下实、描一道亮边。
        let c = ui_theme::colors();
        let mix = |off: Rgba, on_color: Rgba| motion::mix_rgba(off, on_color, on);
        let sheen = 0.2 + (0.1 - 0.2) * on;
        let track = track_fill(mix(c.layer_04, c.bg_accent), c.white, sheen);
        let hover_track = track_fill(
            mix(
                c.layer_04.blend(c.hover),
                c.bg_accent.blend(c.contrast_hover),
            ),
            c.white,
            sheen,
        );
        let knob = mix(c.knob, c.knob_on);
        let thumb = div()
            .id("smelt-switch-thumb")
            .debug_selector(|| "SWITCH_THUMB".into())
            .absolute()
            .top(px(INSET))
            .left(px(left))
            .w(px(width))
            .h(px(THUMB_H))
            .rounded_full()
            .bg(gradient(knob.blend(c.depth_top), knob.blend(c.depth_bottom)))
            .border(px(0.5))
            .border_color(knob.blend(c.depth_top))
            .shadow(elevation::elements())
            // 悬停伸长 2、按下伸长 5，朝没固定的那头长；伸长本身不做过渡。
            .when(interactive, |this| {
                this.group_hover(GROUP, move |style| {
                    style.left(px(hover_left)).w(px(hover_width))
                })
                .group_active(GROUP, move |style| {
                    style.left(px(press_left)).w(px(press_width))
                })
            });
        let shadows = track_shadows(&elevation::current(), on);
        let mut focused_shadows = shadows.clone();
        // 设计是 1px 透明间隙、外接 2px 焦点环。GPUI 的外阴影不能挖空，
        // 间隙用主题背景近似；放在非背景色的容器上会看见这一圈。
        focused_shadows
            .push(BoxShadow::new(px(0.), px(0.), ui_theme::focus().into()).spread_radius(px(3.)));
        focused_shadows
            .push(BoxShadow::new(px(0.), px(0.), rgb(ui_theme::bg()).into()).spread_radius(px(1.)));

        BaseSwitch::new(id)
            .checked(checked)
            .disabled(disabled)
            .when_some(accessibility_label, |this, label| {
                this.accessibility_label(label)
            })
            .when_some(on_change, |this, on_change| {
                this.on_change(move |next, _, window, cx| on_change(next, window, cx))
            })
            .relative()
            .flex_none()
            .w(px(TRACK_W))
            .h(px(TRACK_H))
            .rounded_full()
            .cursor_default()
            .group(GROUP)
            .bg(track)
            .shadow(shadows)
            .when(interactive, |this| {
                this.hover(move |style| style.bg(hover_track))
            })
            .when(interactive, |this| {
                this.on_mouse_down(MouseButton::Left, |_, window, cx| {
                    // 和按钮一样：点了不抢焦点，也不开始选字。
                    window.prevent_default();
                    GlobalState::suppress_text_selection(cx);
                })
            })
            .when(disabled, |this| this.opacity(0.5))
            .focus_visible(move |style| style.shadow(focused_shadows))
            .when_some(tooltip, |this, text| {
                this.tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
            })
            .child(thumb)
            .refine_style(&style)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(debug_assertions)]
    use gpui::{
        Context, Entity, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, Render, TestAppContext,
        VisualTestContext, point,
    };
    #[cfg(debug_assertions)]
    use std::cell::Cell;

    #[cfg(debug_assertions)]
    struct SwitchHarness {
        checked: bool,
        disabled: bool,
        changes: Rc<Cell<usize>>,
    }

    #[cfg(debug_assertions)]
    impl Render for SwitchHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let changes = self.changes.clone();
            let view = cx.weak_entity();
            div().tab_group().size(px(100.)).child(
                Switch::new("switch")
                    .checked(self.checked)
                    .disabled(self.disabled)
                    .on_change(move |next, _, cx| {
                        changes.set(changes.get() + 1);
                        let _ = view.update(cx, |view, cx| {
                            view.checked = next;
                            cx.notify();
                        });
                    }),
            )
        }
    }

    #[cfg(debug_assertions)]
    fn harness(
        cx: &mut TestAppContext,
        checked: bool,
    ) -> (
        Entity<SwitchHarness>,
        &mut VisualTestContext,
        Rc<Cell<usize>>,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_reduce_motion(true);
        });
        let changes = Rc::new(Cell::new(0));
        let (view, cx) = cx.add_window_view({
            let changes = changes.clone();
            move |_, _| SwitchHarness {
                checked,
                disabled: false,
                changes,
            }
        });
        cx.update(|window, cx| {
            window.activate_window();
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();
        (view, cx, changes)
    }

    #[cfg(debug_assertions)]
    fn thumb_bounds(cx: &mut VisualTestContext) -> gpui::Bounds<gpui::Pixels> {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.debug_bounds("SWITCH_THUMB").expect("switch thumb")
    }

    // gpui-pre 的 debug_selector 在 release 依赖中是 no-op。
    #[cfg(debug_assertions)]
    #[gpui::test]
    fn click_toggles_and_disabled_blocks_activation(cx: &mut TestAppContext) {
        let (view, cx, changes) = harness(cx, false);
        let inside = point(px(10.), px(8.));
        cx.simulate_click(inside, Modifiers::default());
        assert_eq!(changes.get(), 1);
        assert!(view.read_with(cx, |view, _| view.checked));
        thumb_bounds(cx);
        cx.simulate_click(inside, Modifiers::default());
        assert_eq!(changes.get(), 2);
        assert!(!view.read_with(cx, |view, _| view.checked));
        view.update(cx, |view, cx| {
            view.disabled = true;
            cx.notify();
        });
        thumb_bounds(cx);
        cx.simulate_click(inside, Modifiers::default());
        assert_eq!(changes.get(), 2);
        assert!(!view.read_with(cx, |view, _| view.checked));
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn keyboard_activation_and_dynamic_disable(cx: &mut TestAppContext) {
        let (view, cx, changes) = harness(cx, false);
        cx.update(|window, cx| {
            window.focus_next(cx);
            assert!(window.focused(cx).is_some());
            window.draw(cx).clear(cx);
        });
        let activate = |cx: &mut VisualTestContext| {
            let keystroke = cx
                .update(|_, cx| Keystroke::parse_with_os("space", cx.platform_info().os).unwrap());
            cx.simulate_event(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            cx.simulate_event(KeyUpEvent { keystroke });
        };
        activate(cx);
        assert_eq!(changes.get(), 1);
        assert!(view.read_with(cx, |view, _| view.checked));
        thumb_bounds(cx);
        activate(cx);
        assert_eq!(changes.get(), 2);
        assert!(!view.read_with(cx, |view, _| view.checked));
        view.update(cx, |view, cx| {
            view.disabled = true;
            cx.notify();
        });
        thumb_bounds(cx);
        activate(cx);
        assert_eq!(changes.get(), 2);
        assert!(!view.read_with(cx, |view, _| view.checked));
    }

    #[test]
    fn mini_geometry_anchors_both_ends_during_hover() {
        assert_eq!(
            (TRACK_W, TRACK_H, THUMB_W, THUMB_H, INSET),
            (36., 16., 21., 14., 1.)
        );
        for stretch in [0., HOVER_STRETCH, PRESS_STRETCH] {
            let (off_left, off_width) = thumb_geometry(0., stretch);
            let (on_left, on_width) = thumb_geometry(1., stretch);
            assert_eq!(off_left, INSET);
            assert_eq!(on_left + on_width + INSET, TRACK_W);
            assert_eq!(off_width, THUMB_W + stretch);
            assert_eq!(on_width, off_width);
        }
        assert_eq!(thumb_geometry(0.5, 2.), (6.5, 23.));
    }

    #[test]
    fn track_shadows_interpolate_between_off_and_on() {
        use crate::theme_catalog::builtin_for_mode;
        use crate::theme_config::ThemeMode;
        let light = builtin_for_mode(ThemeMode::Light);
        let dark = builtin_for_mode(ThemeMode::Dark);
        let off = track_shadows(light.elevation(), 0.);
        let half = track_shadows(light.elevation(), 0.5);
        let on = track_shadows(light.elevation(), 1.);
        let dark = track_shadows(dark.elevation(), 0.);
        assert_eq!(off.len(), 3);
        assert_eq!(on.len(), 3);
        assert_eq!(dark.len(), 2);
        assert!(off.iter().chain(&on).chain(&dark).all(|s| s.inset));
        assert_eq!(off[0].offset.y, px(1.));
        assert_eq!(half[0].offset.y, px(1.5));
        assert_eq!(on[0].offset.y, px(2.));
        assert_eq!(dark[1].spread_radius, px(0.5));
    }
}
