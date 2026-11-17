//! 滑块：横向、单值。4 高的 `layer_04` 轨道（同开关关着的轨道），已选的一段 `ink`，14 的
//! 圆形 `knob` 滑钮带一圈 `hair_strong`。拖动、点轨道跳值、读屏的加减由 gpui-base 的 slider
//! 负责；值、范围、步长和事件在 [`SliderState`] 上。宽度用普通样式改，默认铺满。

use gpui::{
    App, Entity, IntoElement, ParentElement, RenderOnce, StyleRefinement, Styled, Window, div,
    linear_color_stop, linear_gradient, prelude::FluentBuilder as _, px, relative, rgb,
};
use gpui_base::StyledExt as _;
use gpui_base::slider::{Slider as BaseSlider, SliderIndicator, SliderThumb, SliderTrack};

pub use gpui_base::slider::{SliderEvent, SliderState, SliderValue};

use crate::kit::elevation;
use crate::ui_theme;

const THUMB: f32 = 14.;

#[derive(IntoElement)]
pub struct Slider {
    state: Entity<SliderState>,
    disabled: bool,
    style: StyleRefinement,
}

impl Slider {
    pub fn new(state: &Entity<SliderState>) -> Self {
        Self {
            state: state.clone(),
            disabled: false,
            style: StyleRefinement::default(),
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl Styled for Slider {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Slider {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let end = self.state.read(cx).percentage().end;
        let c = ui_theme::colors();
        let thumb = SliderThumb::new(&self.state)
            .disabled(self.disabled)
            .absolute()
            .top(px((4. - THUMB) / 2.))
            .left(relative(end))
            .ml(px(-THUMB / 2.))
            .size(px(THUMB))
            .rounded_full()
            // 同开关的滑钮：上沿压 `depth_top`、下沿压 `depth_bottom`。
            .bg(linear_gradient(
                180.,
                linear_color_stop(c.knob.blend(c.depth_top), 0.),
                linear_color_stop(c.knob.blend(c.depth_bottom), 1.),
            ))
            .border_1()
            .border_color(ui_theme::hair_strong())
            // 跟开关的滑钮一样只压一道贴身的影子，不用浮层那种散开的投影。
            .shadow(elevation::elements())
            .when(!self.disabled, |this| this.cursor_grab());
        BaseSlider::new(&self.state)
            .disabled(self.disabled)
            .w_full()
            .flex()
            .items_center()
            .when(self.disabled, |this| this.opacity(0.4))
            .refine_style(&self.style)
            .child(
                SliderTrack::new(&self.state)
                    .disabled(self.disabled)
                    .flex()
                    .items_center()
                    .w_full()
                    .h(px(20.))
                    .child(
                        SliderIndicator::new(&self.state)
                            .relative()
                            .w_full()
                            .h(px(4.))
                            .rounded_full()
                            .bg(c.layer_04)
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left_0()
                                    .w(relative(end))
                                    .rounded_full()
                                    .bg(rgb(ui_theme::ink())),
                            )
                            .child(thumb),
                    ),
            )
    }
}
