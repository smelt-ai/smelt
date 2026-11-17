//! 进度条：一条 `soft` 轨道，`muted` 填充，两头全圆，默认 4 高。不带标题行：
//! 要说明文字由调用方摆在旁边。宽高用普通样式改（`.w(px(120.))`、`.h(px(5.))`）。
//!
//! 不知道总量时用 [`Progress::indeterminate`]：四分之一长的一段在轨道里来回走；系统开了
//! 减弱动态效果时停在左端。读屏角色和数值用 gpui-base 的 `Progress`。

use std::f32::consts::TAU;
use std::time::Duration;

use gpui::{
    App, ElementId, IntoElement, ParentElement, RenderOnce, StyleRefinement, Styled, Window, div,
    relative, rgb,
};
use gpui_base::StyledExt as _;

use crate::{motion, ui_theme};

const SWEEP: Duration = Duration::from_millis(1600);
const SWEEP_WIDTH: f32 = 0.25;

#[derive(IntoElement)]
pub struct Progress {
    id: ElementId,
    value: f32,
    indeterminate: bool,
    style: StyleRefinement,
}

impl Progress {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            value: 0.,
            indeterminate: false,
            style: StyleRefinement::default(),
        }
    }

    /// 0–100，超出的截到两头。
    pub fn value(mut self, value: f32) -> Self {
        self.value = value.clamp(0., 100.);
        self
    }

    pub fn indeterminate(mut self, indeterminate: bool) -> Self {
        self.indeterminate = indeterminate;
        self
    }
}

impl Styled for Progress {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

/// 来回走的那段左端位置（占轨道长的比例）。相位 0 和 1 都在最左。
fn sweep_left(phase: f32) -> f32 {
    (1. - (TAU * phase).cos()) / 2. * (1. - SWEEP_WIDTH)
}

impl RenderOnce for Progress {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let fill = rgb(ui_theme::muted());
        let indicator = if self.indeterminate {
            // 挂在进度条（有 id）底下，元素状态按父 id 分开，固定 id 不会撞。
            motion::ambient_loop("progress-sweep", SWEEP, true, move |phase| {
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(sweep_left(phase)))
                    .w(relative(SWEEP_WIDTH))
                    .rounded_full()
                    .bg(fill)
            })
            .into_any_element()
        } else {
            div()
                .h_full()
                .w(relative(self.value / 100.))
                .rounded_full()
                .bg(fill)
                .into_any_element()
        };
        gpui_base::Progress::new(self.id)
            .value(self.value)
            .indeterminate(self.indeterminate)
            .relative()
            .w_full()
            .h(gpui::px(4.))
            .overflow_hidden()
            .rounded_full()
            .bg(rgb(ui_theme::soft()))
            .child(indicator)
            .refine_style(&self.style)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_stays_inside_the_track_and_rests_at_the_start() {
        for step in 0..=100 {
            let left = sweep_left(step as f32 / 100.);
            assert!(
                (0. ..=1. - SWEEP_WIDTH + f32::EPSILON).contains(&left),
                "{left}"
            );
        }
        assert!(sweep_left(1.).abs() < 1e-6);
    }
}
