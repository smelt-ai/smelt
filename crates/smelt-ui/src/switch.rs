//! 开关：一项设置立刻生效的开或关。规则见 `docs/ui-design-language.md` 第 2、4、7 节。
//!
//! 22×36 的胶囊，开是 `ink`，关是 `soft_press`；滑块 18 圆，开时 `on_ink`，关时 `surface`
//! 抬起。建在 gpui-base 的无样式开关上：焦点、空格/回车、无障碍归它，样子在这里画。

use std::rc::Rc;

use gpui::{
    App, ElementId, InteractiveElement, IntoElement, MouseButton, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Window, div,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_base::{GlobalState, Switch as BaseSwitch};
use gpui_component::StyledExt as _;
use gpui_component::tooltip::Tooltip;

use crate::button::{DISABLED_OPACITY, focus_ring};
use crate::{motion, ui_theme};

type ChangeHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

const TRACK_W: f32 = 36.;
const TRACK_H: f32 = 22.;
const THUMB: f32 = 18.;
const INSET: f32 = (TRACK_H - THUMB) / 2.;

/// 滑块离左边多远：关贴左，开贴右。
fn thumb_left(checked: bool) -> f32 {
    if checked {
        TRACK_W - INSET - THUMB
    } else {
        INSET
    }
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

        // 0 是关，1 是开。滑块位置、底色、滑块颜色和阴影都跟着它走，切换时一起过渡；
        // 页面刚打开时直接到位，连点时从半路掉头。键名和 gpui-base 存焦点的键分开。
        let on = motion::unit_transition(
            ElementId::NamedChild(id.clone().into(), "thumb".into()),
            if checked { 1. } else { 0. },
            window,
            cx,
        );
        let off = 1. - on;
        let thumb_shadow = ui_theme::lift_shadow()
            .into_iter()
            .map(|mut shadow| {
                shadow.color.a *= off;
                shadow
            })
            .collect::<Vec<_>>();
        let thumb = div()
            .absolute()
            .top(px(INSET))
            .left(px(
                thumb_left(false) + (thumb_left(true) - thumb_left(false)) * on
            ))
            .size(px(THUMB))
            .rounded_full()
            .bg(motion::mix_rgba(
                rgb(ui_theme::surface()),
                rgb(ui_theme::on_ink()),
                on,
            ))
            .shadow(thumb_shadow);
        let track = motion::mix_rgba(rgb(ui_theme::soft_press()), rgb(ui_theme::ink()), on);

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
            .bg(track)
            // 不做悬停变色：关着的底已经是最深的 `soft_press`，再变只能变浅，像松开了。
            .when(interactive, |this| {
                this.on_mouse_down(MouseButton::Left, |_, window, cx| {
                    // 和按钮一样：点了不抢焦点，也不开始选字。
                    window.prevent_default();
                    GlobalState::suppress_text_selection(cx);
                })
            })
            .when(disabled, |this| this.opacity(DISABLED_OPACITY))
            .focus_visible(|style| style.shadow(focus_ring()))
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

    #[test]
    fn thumb_sits_inside_the_track_at_both_ends() {
        assert_eq!(thumb_left(false), INSET);
        assert_eq!(thumb_left(true) + THUMB + INSET, TRACK_W);
    }
}
