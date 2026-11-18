//! 图表。比例换算、形状和元素都是 gpui-base 的 plot，颜色读 [`ui_theme::chart`] 的分类色。
//!
//! 只做用到的图形：资源页的内存曲线是一条不带坐标轴、网格和悬停的面积图。

use gpui::{App, Bounds, IntoElement, Pixels, Window, rgb};
use gpui_base::plot::{Curve, Plot, PlotElement, shape::Area};

use crate::ui_theme;

/// 填色的面积在曲线下面压一层这么浓的同色（0.4）。
const FILL_ALPHA: u8 = 0x66;

/// 一组数从左到右铺满宽度、从底边往上填色的面积图，折线连点。没有坐标轴、网格和悬停：
/// 它是给一个大数字配的走势，不是让人读数的图。宽高跟随父元素，少于两个点不画。
pub struct AreaChart {
    values: Vec<f64>,
    max: Option<f64>,
}

impl AreaChart {
    pub fn new(values: impl IntoIterator<Item = f64>) -> Self {
        Self {
            values: values.into_iter().collect(),
            max: None,
        }
    }

    /// 上沿对应的值，底边是 0。默认是数据里的最大值，最高点贴着上沿；想留出余量就给大一些。
    pub fn max(mut self, max: f64) -> Self {
        self.max = Some(max);
        self
    }
}

impl Plot for AreaChart {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, _: &mut App) {
        if self.values.len() < 2 {
            return;
        }
        let max = self
            .max
            .unwrap_or_else(|| self.values.iter().copied().fold(0., f64::max));
        if !max.is_finite() || max <= 0. {
            return;
        }
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        let step = width / (self.values.len() - 1) as f32;
        let color = ui_theme::chart(0);
        Area::new()
            .data(self.values.iter().copied().enumerate())
            .x(move |(ix, _)| Some(*ix as f32 * step))
            .y0(height)
            // 不是数的点留空，不当 0 画。
            .y1(move |(_, value)| {
                value
                    .is_finite()
                    .then(|| height * (1. - (value / max).clamp(0., 1.) as f32))
            })
            .fill(ui_theme::tint(color, FILL_ALPHA))
            .stroke(rgb(color))
            .curve(Curve::Linear)
            .paint(&bounds, window);
    }
}

impl IntoElement for AreaChart {
    type Element = PlotElement<Self>;

    fn into_element(self) -> Self::Element {
        PlotElement::new(self)
    }
}
