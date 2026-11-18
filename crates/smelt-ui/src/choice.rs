//! 复选框和单选：在一组选项里勾几项、选一项。规则见 `docs/ui-design-language.md` 第 2、4、7 节。
//!
//! 指示块 16 见方：复选框 4 圆角、中性弱底和 10 的勾，标签隔 12、行高 24；单选是圆、
//! 主题主色和 6 的圆点，标签隔 8、行高 20。可以带标签，点标签和点指示块一样。
//!
//! 建在 gpui-base 的无样式复选框和单选上：焦点、空格/回车、无障碍归它们。根元素对外开放
//! 样式和交互，调用方可以把整行画成卡片（ACP 提问卡）；焦点环只画在指示块上，不圈整行。

use std::rc::Rc;

use gpui::{
    AnyElement, App, BoxShadow, Div, ElementId, FocusHandle, InteractiveElement, Interactivity,
    IntoElement, MouseButton, ParentElement, RenderOnce, Rgba, SharedString,
    StatefulInteractiveElement, StyleRefinement, Styled, Window, div, prelude::FluentBuilder as _,
    px, rgb, svg,
};
use gpui_base::{Checkbox as BaseCheckbox, GlobalState, Radio as BaseRadio};

use crate::kit::{RADIUS, TYPE};
use crate::ui_theme;

type ChangeHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;
type SelectHandler = Rc<dyn Fn(&mut Window, &mut App)>;

const INDICATOR: f32 = 16.;
const CHECKBOX_RADIUS: f32 = RADIUS.sm;
const MARK: f32 = 10.;
const DOT: f32 = 6.;
const CHECKBOX_GAP: f32 = 12.;
const RADIO_GAP: f32 = 8.;
const CHECKBOX_LABEL_HEIGHT: f32 = 24.;
const RADIO_LABEL_HEIGHT: f32 = 20.;
const DISABLED_INDICATOR_OPACITY: f32 = 0.5;
/// 悬停整行时指示块跟着变：`group_hover` 找最近的这个名字。
const GROUP: &str = "smelt-choice";

/// 复选框：独立的一项勾或不勾。
#[derive(IntoElement)]
pub struct Checkbox {
    id: ElementId,
    base: BaseCheckbox,
    checked: bool,
    disabled: bool,
    label: Option<SharedString>,
    accessibility_label: Option<SharedString>,
    on_change: Option<ChangeHandler>,
}

impl Checkbox {
    pub fn new(id: impl Into<ElementId>) -> Self {
        let id = id.into();
        Self {
            base: BaseCheckbox::new(id.clone()),
            id,
            checked: false,
            disabled: false,
            label: None,
            accessibility_label: None,
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

    /// 可见标签，也是读屏念的名字。
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// 给读屏的名字，优先于可见标签；没有可见标签时一定要给。
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    /// 拿到切换后的值；调用方存下来再经 [`Checkbox::checked`] 画回来。
    pub fn on_change(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

/// 单选：一组里选一项。点已经选中的那项不回调。
#[derive(IntoElement)]
pub struct Radio {
    id: ElementId,
    base: BaseRadio,
    checked: bool,
    disabled: bool,
    label: Option<SharedString>,
    accessibility_label: Option<SharedString>,
    on_select: Option<SelectHandler>,
}

impl Radio {
    pub fn new(id: impl Into<ElementId>) -> Self {
        let id = id.into();
        Self {
            base: BaseRadio::new(id.clone()),
            id,
            checked: false,
            disabled: false,
            label: None,
            accessibility_label: None,
            on_select: None,
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

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    pub fn on_select(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

macro_rules! open_root {
    ($ty:ty) => {
        impl Styled for $ty {
            fn style(&mut self) -> &mut StyleRefinement {
                self.base.style()
            }
        }

        impl InteractiveElement for $ty {
            fn interactivity(&mut self) -> &mut Interactivity {
                self.base.interactivity()
            }
        }

        impl StatefulInteractiveElement for $ty {}
    };
}

open_root!(Checkbox);
open_root!(Radio);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Square,
    Round,
}

impl Shape {
    fn gap(self) -> f32 {
        match self {
            Self::Square => CHECKBOX_GAP,
            Self::Round => RADIO_GAP,
        }
    }

    fn label_height(self) -> f32 {
        match self {
            Self::Square => CHECKBOX_LABEL_HEIGHT,
            Self::Round => RADIO_LABEL_HEIGHT,
        }
    }
}

/// 设计上的复选框、单选专用色位在当前主题中没有一一对应项；只按已有语义取近似色。
#[derive(Debug, PartialEq)]
struct IndicatorColors {
    background: Option<Rgba>,
    border: Rgba,
    mark: Rgba,
    hover: Option<Rgba>,
}

fn indicator_colors(
    shape: Shape,
    checked: bool,
    disabled: bool,
    interactive: bool,
    focus_visible: bool,
    palette: ui_theme::Palette,
) -> IndicatorColors {
    match shape {
        Shape::Square => IndicatorColors {
            // 禁用时不铺底（设计里的禁用底色为透明）。
            background: (checked && !disabled).then(|| rgb(palette.soft)),
            border: if disabled {
                let mut border = palette.hair_strong;
                border.a *= DISABLED_INDICATOR_OPACITY;
                border
            } else if focus_visible {
                palette.focus
            } else {
                palette.hair_strong
            },
            mark: rgb(palette.text),
            hover: (interactive && !disabled).then(|| rgb(palette.soft_hover)),
        },
        Shape::Round => IndicatorColors {
            background: Some(rgb(if checked { palette.ink } else { palette.bg })),
            border: palette.hair_strong,
            mark: rgb(palette.on_ink),
            // 已选中的单选悬停不换底。
            hover: (interactive && !disabled && !checked).then(|| rgb(palette.soft_hover)),
        },
    }
}

/// 指示块：形状、勾没勾、能不能点、键盘焦点在不在它身上。
fn indicator(
    shape: Shape,
    checked: bool,
    disabled: bool,
    interactive: bool,
    focus_visible: bool,
) -> Div {
    let colors = indicator_colors(
        shape,
        checked,
        disabled,
        interactive,
        focus_visible,
        ui_theme::palette(),
    );
    let mark: Option<AnyElement> = checked.then(|| match shape {
        Shape::Square => svg()
            .path(crate::IconName::Check.path())
            .flex_shrink_0()
            .size(px(MARK))
            .text_color(colors.mark)
            .into_any_element(),
        Shape::Round => div()
            .size(px(DOT))
            .rounded_full()
            .bg(colors.mark)
            .into_any_element(),
    });
    // Radio 的焦点环留出透明间隙，不用窗口底色填充，以免覆盖调用方的卡片背景。
    let focus = focus_visible.then(|| match shape {
        Shape::Square => div()
            .absolute()
            .top(px(-1.))
            .left(px(-1.))
            .size(px(INDICATOR + 2.))
            .rounded(px(CHECKBOX_RADIUS + 1.))
            .border_1()
            .border_color(ui_theme::focus()),
        Shape::Round => div()
            .absolute()
            .top(px(-3.))
            .left(px(-3.))
            .size(px(INDICATOR + 6.))
            .rounded_full()
            .border_2()
            .border_color(ui_theme::focus()),
    });
    div()
        .relative()
        .flex_none()
        .size(px(INDICATOR))
        .flex()
        .items_center()
        .justify_center()
        .map(|this| match shape {
            Shape::Square => this
                .rounded(px(CHECKBOX_RADIUS))
                .border_1()
                .border_color(colors.border),
            // 用 0.5px 扩散的阴影画圆形边线。
            Shape::Round => this.rounded_full().shadow(vec![
                BoxShadow::new(px(0.), px(0.), colors.border.into())
                    .spread_radius(px(0.5))
                    .inset(),
            ]),
        })
        .when_some(colors.background, |this, color| this.bg(color))
        .when_some(colors.hover, |this, color| {
            this.group_hover(GROUP, move |style| style.bg(color))
        })
        .when(disabled && shape == Shape::Round, |this| {
            this.opacity(DISABLED_INDICATOR_OPACITY)
        })
        .children(mark)
        .children(focus)
}

/// 根元素：指示块加可选标签，点哪里都一样。
fn paint_root<E>(
    root: E,
    indicator: Div,
    label: Option<SharedString>,
    disabled: bool,
    shape: Shape,
) -> E
where
    E: Styled + ParentElement + InteractiveElement + IntoElement,
{
    root.group(GROUP)
        .flex()
        .items_center()
        .gap(px(shape.gap()))
        .cursor_default()
        .when(!disabled, |this| {
            this.on_mouse_down(MouseButton::Left, |_, window, cx| {
                // 和按钮一样：点了不抢焦点，也不开始选字。
                window.prevent_default();
                GlobalState::suppress_text_selection(cx);
            })
        })
        .child(indicator)
        .when_some(label, |this, label| {
            // 复选框标签是正文小号（13/400），单选是界面字（13/440），行高各自给。
            let text = match shape {
                Shape::Round => TYPE.ui,
                Shape::Square => TYPE.text_small,
            };
            this.child(
                text.lh(shape.label_height())
                    .apply(div())
                    .min_w_0()
                    .min_h(px(shape.label_height()))
                    .flex()
                    .items_center()
                    .text_color(rgb(if disabled && shape == Shape::Round {
                        ui_theme::muted()
                    } else {
                        ui_theme::text()
                    }))
                    .child(label),
            )
        })
}

/// 控件自己持有焦点句柄，才知道该不该在指示块上画焦点环。
fn focus_handle(id: &ElementId, window: &mut Window, cx: &mut App) -> FocusHandle {
    window
        .use_keyed_state(
            ElementId::NamedChild(id.clone().into(), "focus".into()),
            cx,
            |_, cx| cx.focus_handle(),
        )
        .read(cx)
        .clone()
}

fn keyboard_focused(focus: &FocusHandle, window: &Window) -> bool {
    focus.is_focused(window) && window.last_input_was_keyboard()
}

impl RenderOnce for Checkbox {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Checkbox {
            id,
            base,
            checked,
            disabled,
            label,
            accessibility_label,
            on_change,
        } = self;
        let focus = focus_handle(&id, window, cx);
        let interactive = !disabled && on_change.is_some();
        let indicator = indicator(
            Shape::Square,
            checked,
            disabled,
            interactive,
            keyboard_focused(&focus, window),
        );
        let base = base
            .checked(checked)
            .disabled(disabled)
            .track_focus(&focus)
            .when_some(
                accessibility_label.or_else(|| label.clone()),
                |this, label| this.accessibility_label(label),
            )
            .when_some(on_change, |this, on_change| {
                this.on_change(move |next, _, window, cx| {
                    on_change(next == gpui_base::CheckboxState::Checked, window, cx)
                })
            });
        paint_root(base, indicator, label, disabled, Shape::Square)
    }
}

impl RenderOnce for Radio {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Radio {
            id,
            base,
            checked,
            disabled,
            label,
            accessibility_label,
            on_select,
        } = self;
        let focus = focus_handle(&id, window, cx);
        let interactive = !disabled && !checked && on_select.is_some();
        let indicator = indicator(
            Shape::Round,
            checked,
            disabled,
            interactive,
            keyboard_focused(&focus, window),
        );
        let base = base
            .checked(checked)
            .disabled(disabled)
            .track_focus(&focus)
            .when_some(
                accessibility_label.or_else(|| label.clone()),
                |this, label| this.accessibility_label(label),
            )
            .when_some(on_select, |this, on_select| {
                this.on_change(move |_, _, window, cx| on_select(window, cx))
            });
        paint_root(base, indicator, label, disabled, Shape::Round)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indicator_dimensions_and_label_spacing() {
        assert_eq!(INDICATOR, 16.);
        assert_eq!(CHECKBOX_RADIUS, 4.);
        assert_eq!(MARK, 10.);
        assert_eq!(DOT, 6.);
        assert_eq!(Shape::Square.gap(), 12.);
        assert_eq!(Shape::Round.gap(), 8.);
        assert_eq!(Shape::Square.label_height(), 24.);
        assert_eq!(Shape::Round.label_height(), 20.);
    }

    #[test]
    fn checkbox_states_use_neutral_colors_and_disable_hover() {
        for light in [true, false] {
            let p = ui_theme::builtin_fallback(light);
            let off = indicator_colors(Shape::Square, false, false, true, false, p);
            assert_eq!(off.background, None);
            assert_eq!(off.border, p.hair_strong);
            assert_eq!(off.hover, Some(rgb(p.soft_hover)));
            let on = indicator_colors(Shape::Square, true, false, true, false, p);
            assert_eq!(on.background, Some(rgb(p.soft)));
            assert_eq!(on.mark, rgb(p.text));
            assert_eq!(on.hover, Some(rgb(p.soft_hover)));
            let focused = indicator_colors(Shape::Square, true, false, true, true, p);
            assert_eq!(focused.border, p.focus);
            let disabled = indicator_colors(Shape::Square, true, true, true, true, p);
            assert_eq!(disabled.background, None);
            let mut disabled_border = p.hair_strong;
            disabled_border.a *= DISABLED_INDICATOR_OPACITY;
            assert_eq!(disabled.border, disabled_border);
            assert_eq!(disabled.mark, rgb(p.text));
            assert_eq!(disabled.hover, None);
            let readonly = indicator_colors(Shape::Square, false, false, false, false, p);
            assert_eq!(readonly.hover, None);
        }
    }

    #[test]
    fn colors_follow_the_supplied_theme() {
        let mut p = ui_theme::builtin_fallback(true);
        let other = ui_theme::builtin_fallback(false);
        p.soft = other.soft;
        p.soft_hover = other.soft_hover;
        p.text = other.text;
        p.ink = other.ink;
        p.on_ink = other.on_ink;
        p.focus = other.focus;
        let check = indicator_colors(Shape::Square, true, false, true, true, p);
        assert_eq!(check.background, Some(rgb(other.soft)));
        assert_eq!(check.mark, rgb(other.text));
        assert_eq!(check.border, other.focus);
        assert_eq!(check.hover, Some(rgb(other.soft_hover)));
        let radio = indicator_colors(Shape::Round, true, false, true, false, p);
        assert_eq!(radio.background, Some(rgb(other.ink)));
        assert_eq!(radio.mark, rgb(other.on_ink));
    }

    #[test]
    fn radio_states_use_primary_and_only_unselected_hover() {
        for light in [true, false] {
            let p = ui_theme::builtin_fallback(light);
            let off = indicator_colors(Shape::Round, false, false, true, false, p);
            assert_eq!(off.background, Some(rgb(p.bg)));
            assert_eq!(off.border, p.hair_strong);
            assert_eq!(off.hover, Some(rgb(p.soft_hover)));
            let on = indicator_colors(Shape::Round, true, false, true, true, p);
            assert_eq!(on.background, Some(rgb(p.ink)));
            assert_eq!(on.mark, rgb(p.on_ink));
            assert_eq!(on.hover, None);
            for checked in [false, true] {
                let disabled = indicator_colors(Shape::Round, checked, true, true, false, p);
                assert_eq!(disabled.hover, None);
                let readonly = indicator_colors(Shape::Round, checked, false, false, false, p);
                assert_eq!(readonly.hover, None);
            }
        }
        assert_eq!(DISABLED_INDICATOR_OPACITY, 0.5);
    }
}
