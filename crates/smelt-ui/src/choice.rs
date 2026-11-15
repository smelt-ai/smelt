//! 复选框和单选：在一组选项里勾几项、选一项。规则见 `docs/ui-design-language.md` 第 2、4、7 节。
//!
//! 指示块 18 见方：复选框圆角 6，单选是圆。没勾是 `hair_strong` 细边，勾上是 `ink` 实底，
//! 里面的勾或圆点用 `on_ink`。可以带一段标签，点标签和点指示块一样。
//!
//! 建在 gpui-base 的无样式复选框和单选上：焦点、空格/回车、无障碍归它们。根元素对外开放
//! 样式和交互，调用方可以把整行画成卡片（ACP 提问卡）；焦点环只画在指示块上，不圈整行。

use std::rc::Rc;

use gpui::{
    AnyElement, App, Div, ElementId, FocusHandle, InteractiveElement, Interactivity, IntoElement,
    MouseButton, ParentElement, RenderOnce, SharedString, StatefulInteractiveElement,
    StyleRefinement, Styled, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_base::{Checkbox as BaseCheckbox, GlobalState, Radio as BaseRadio};
use gpui_component::{Icon, IconName};

use crate::button::{DISABLED_OPACITY, focus_ring};
use crate::ui_theme;

type ChangeHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;
type SelectHandler = Rc<dyn Fn(&mut Window, &mut App)>;

const INDICATOR: f32 = 18.;
const CHECKBOX_RADIUS: f32 = 6.;
const MARK: f32 = 12.;
const DOT: f32 = 6.;
const LABEL_GAP: f32 = 8.;
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

#[derive(Clone, Copy)]
enum Shape {
    Square,
    Round,
}

/// 指示块：形状、勾没勾、能不能点、键盘焦点在不在它身上。
fn indicator(shape: Shape, checked: bool, interactive: bool, focus_visible: bool) -> Div {
    let mark: Option<AnyElement> = checked.then(|| match shape {
        Shape::Square => Icon::new(IconName::Check)
            .size(px(MARK))
            .text_color(rgb(ui_theme::on_ink()))
            .into_any_element(),
        Shape::Round => div()
            .size(px(DOT))
            .rounded_full()
            .bg(rgb(ui_theme::on_ink()))
            .into_any_element(),
    });
    div()
        .flex_none()
        .size(px(INDICATOR))
        .flex()
        .items_center()
        .justify_center()
        .map(|this| match shape {
            Shape::Square => this.rounded(px(CHECKBOX_RADIUS)),
            Shape::Round => this.rounded_full(),
        })
        .map(|this| {
            if checked {
                this.bg(rgb(ui_theme::ink()))
            } else {
                this.border_1().border_color(ui_theme::hair_strong())
            }
        })
        .when(interactive, |this| {
            let hover = if checked {
                ui_theme::ink_hover()
            } else {
                ui_theme::soft_hover()
            };
            this.group_hover(GROUP, move |style| style.bg(rgb(hover)))
        })
        .when(focus_visible, |this| this.shadow(focus_ring()))
        .children(mark)
}

/// 根元素：指示块加可选标签，点哪里都一样。
fn paint_root<E>(root: E, indicator: Div, label: Option<SharedString>, disabled: bool) -> E
where
    E: Styled + ParentElement + InteractiveElement + IntoElement,
{
    root.group(GROUP)
        .flex()
        .items_center()
        .gap(px(LABEL_GAP))
        .cursor_default()
        .when(disabled, |this| this.opacity(DISABLED_OPACITY))
        .when(!disabled, |this| {
            this.on_mouse_down(MouseButton::Left, |_, window, cx| {
                // 和按钮一样：点了不抢焦点，也不开始选字。
                window.prevent_default();
                GlobalState::suppress_text_selection(cx);
            })
        })
        .child(indicator)
        .when_some(label, |this, label| {
            this.child(
                div()
                    .min_w_0()
                    .text_color(rgb(ui_theme::text()))
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
        paint_root(base, indicator, label, disabled)
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
        paint_root(base, indicator, label, disabled)
    }
}
