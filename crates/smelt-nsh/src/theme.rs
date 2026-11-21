//! smelt 的主题给 NSH：smelt-ui 色板（[`ui_theme::Palette`]）投影成 NSH 主题的 shadcn 语义位，
//! 同一个角色对同一个位，NSH 画的和 smelt-ui 画的才对得上。只给当前这一档深浅的色，深浅
//! 两套都填它：换主题、换深浅时整份重给（见 `PilotView`）。

use gpui::Rgba;
use nsh_platform_gpui::{AdaptiveColor, Appearance, Color, Theme};
use smelt_ui::kit::RADIUS;
use smelt_ui::ui_theme;

/// `0xRRGGBB`。
fn solid(rgb: u32) -> AdaptiveColor {
    Color((rgb << 8) | 0xff).into()
}

/// 带透明度的。
fn alpha(color: Rgba) -> AdaptiveColor {
    let channel = |value: f32| u32::from((value.clamp(0., 1.) * 255.).round() as u8);
    Color(
        (channel(color.r) << 24)
            | (channel(color.g) << 16)
            | (channel(color.b) << 8)
            | channel(color.a),
    )
    .into()
}

pub(crate) fn appearance() -> Appearance {
    if ui_theme::is_light() {
        Appearance::Light
    } else {
        Appearance::Dark
    }
}

pub(crate) fn nsh_theme() -> Theme {
    let p = ui_theme::palette();
    let mut theme = Theme::default();
    for (name, color) in [
        ("background", solid(p.bg)),
        ("foreground", solid(p.text)),
        ("card", solid(p.surface)),
        ("card-foreground", solid(p.text)),
        ("popover", solid(p.surface)),
        ("popover-foreground", alpha(p.popover_text)),
        ("primary", solid(p.ink)),
        ("primary-foreground", solid(p.on_ink)),
        ("secondary", solid(p.soft)),
        ("secondary-foreground", solid(p.text)),
        ("muted", solid(p.soft)),
        ("muted-foreground", solid(p.muted)),
        ("accent", alpha(p.row_hover)),
        ("accent-foreground", solid(p.text)),
        ("destructive", solid(p.bad)),
        ("border", alpha(p.hair)),
        ("input", alpha(p.hair_strong)),
        ("ring", alpha(p.focus)),
        ("chart-1", solid(p.chart[0])),
        ("chart-2", solid(p.chart[1])),
        ("chart-3", solid(p.chart[2])),
        ("chart-4", solid(p.chart[3])),
        ("chart-5", solid(p.chart[4])),
        ("sidebar", solid(p.side)),
        ("sidebar-foreground", solid(p.text)),
        ("sidebar-accent", alpha(p.row_selected)),
        ("sidebar-accent-foreground", solid(p.text)),
        ("sidebar-border", alpha(p.hair)),
        ("sidebar-ring", alpha(p.focus)),
    ] {
        theme.set_color(name, color);
    }
    theme.set_number("radius", RADIUS.md);
    theme
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_keep_their_channels() {
        assert_eq!(Color::from(solid(0x336699).light), Color(0x336699ff));
        let half = Rgba {
            r: 1.,
            g: 0.,
            b: 0.,
            a: 0.5,
        };
        assert_eq!(Color::from(alpha(half).dark), Color(0xff000080));
    }
}
