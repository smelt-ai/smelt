//! 按钮投影令牌：NSH 插件按钮经主题令牌取与 Smelt 视觉语言一致的静止投影
//! （含「任一状态透底就只留内细圈」规则）。

use gpui::{BoxShadow, Hsla, Rgba, px, rgb};

use crate::kit::elevation::{self, Elevation};
use crate::ui_theme;

/// 按钮静止投影的角色分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonShadow {
    /// 中性、危险按钮。
    Neutral,
    /// 主操作。
    Primary,
    /// 浮在内容上的按钮（`floating`）。
    Floating,
}

/// 一颗可点的按钮静止时的投影（含「任一状态透底就只留内细圈」）。
/// NSH 插件的按钮经主题令牌拿它，不自己推。
pub fn resting_shadows(kind: ButtonShadow) -> Vec<BoxShadow> {
    let palette = ui_theme::palette();
    let (rest_bg, hover_bg, press_bg, floating) = match kind {
        ButtonShadow::Neutral => {
            let s = palette.button_secondary;
            (s[0], s[1], s[2], false)
        }
        ButtonShadow::Primary => {
            let p = palette.button_primary;
            (p[0], p[1], p[2], false)
        }
        ButtonShadow::Floating => {
            let s = palette.button_secondary;
            (rgb(ui_theme::surface()), s[1], s[2], true)
        }
    };
    let safe_drop_shadows = rest_bg.a >= 1. && hover_bg.a >= 1. && press_bg.a >= 1.;
    let theme_elevation = elevation::current();
    if !safe_drop_shadows {
        translucent_button_shadows(kind, &theme_elevation, floating, ui_theme::hair())
    } else if floating {
        theme_elevation.raised.clone()
    } else {
        match kind {
            ButtonShadow::Neutral => theme_elevation.button_neutral.clone(),
            ButtonShadow::Primary => theme_elevation.button_contrast.clone(),
            ButtonShadow::Floating => theme_elevation.raised.clone(),
        }
    }
}

fn inset_ring(color: impl Into<Hsla>, width: f32) -> BoxShadow {
    BoxShadow::new(px(0.), px(0.), color.into())
        .spread_radius(px(width))
        .inset()
}

fn translucent_button_shadows(
    kind: ButtonShadow,
    elevation: &Elevation,
    floating: bool,
    hair: Rgba,
) -> Vec<BoxShadow> {
    let ring = |layers: &[BoxShadow]| {
        layers
            .iter()
            .find(|layer| {
                !layer.inset
                    && layer.offset.y == px(0.)
                    && layer.blur_radius == px(0.)
                    && layer.spread_radius > px(0.)
            })
            .map(|layer| layer.color)
    };
    let color = if floating {
        Some(hair.into())
    } else {
        match kind {
            ButtonShadow::Neutral => ring(&elevation.button_neutral),
            ButtonShadow::Primary => ring(&elevation.button_contrast),
            ButtonShadow::Floating => Some(hair.into()),
        }
    };
    color
        .map(|color| vec![inset_ring(color, 1.)])
        .unwrap_or_default()
}
