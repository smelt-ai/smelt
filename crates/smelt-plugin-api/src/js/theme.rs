//! `theme()`：smelt 当前主题给 JS 插件的样子。色板是 [`smelt_ui::ui_theme::Palette`] 的语义位，
//! 名字同字段；字阶是 [`smelt_ui::kit::TYPE`]，按 rem 给，跟着界面字号缩放。gpui-shell 自己的
//! 主题色（`theme().colors`）是给它的通用组件用的，没有 smelt 的状态色、选中行上的次要字这些。
//!
//! 圆角给 smelt-ui 的整套档位（[`smelt_ui::kit::RADIUS`]）加列表行、卡片的；投影是主题的浮起层级
//! （[`smelt_ui::kit::elevation`]），每档几层，形状同元素的 `shadow([...])`。
//!
//! 主题一换，宿主同步的 gpui-base 主题跟着变，脚本视图就重画，render 里现读就是新的。

use gpui::{BoxShadow, Rgba};
use gpui_shell::{HostObject, HostValue};
use smelt_ui::kit::{RADIUS, TYPE, TextStyle, elevation};
use smelt_ui::ui_theme;

/// `0xRRGGBB` 写成 `#rrggbb`，同组件颜色参数收的写法（[`crate::hex_color`]）。
fn solid(color: u32) -> HostValue {
    crate::hex_color(color).into()
}

/// 带透明度的写成 `#rrggbbaa`。
fn alpha(color: Rgba) -> HostValue {
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        channel(color.r),
        channel(color.g),
        channel(color.b),
        channel(color.a)
    )
    .into()
}

fn palette() -> HostValue {
    let p = ui_theme::palette();
    let states = |states: [Rgba; 3]| HostValue::Array(states.into_iter().map(alpha).collect());
    HostObject::new()
        .field("bg", solid(p.bg))
        .field("side", solid(p.side))
        .field("surface", solid(p.surface))
        .field("soft", solid(p.soft))
        .field("soft_hover", solid(p.soft_hover))
        .field("soft_press", solid(p.soft_press))
        .field("row_hover", alpha(p.row_hover))
        .field("row_selected", alpha(p.row_selected))
        .field("hair", alpha(p.hair))
        .field("hair_strong", alpha(p.hair_strong))
        .field("text", solid(p.text))
        .field("muted", solid(p.muted))
        .field("faint", solid(p.faint))
        .field("muted_selected", solid(p.muted_selected))
        .field("faint_selected", solid(p.faint_selected))
        .field("ink", solid(p.ink))
        .field("ink_hover", solid(p.ink_hover))
        .field("ink_press", solid(p.ink_press))
        .field("on_ink", solid(p.on_ink))
        .field("button_primary", states(p.button_primary))
        .field(
            "button_primary_foreground",
            solid(p.button_primary_foreground),
        )
        .field("button_submit", states(p.button_submit))
        .field(
            "button_submit_foreground",
            solid(p.button_submit_foreground),
        )
        .field("button_secondary", states(p.button_secondary))
        .field(
            "button_secondary_foreground",
            solid(p.button_secondary_foreground),
        )
        .field("focus", alpha(p.focus))
        .field("selection", alpha(p.selection))
        .field("link", solid(p.link))
        .field("run", solid(p.run))
        .field("need", solid(p.need))
        .field("ok", solid(p.ok))
        .field("bad", solid(p.bad))
        .field("on_bad", solid(p.on_bad))
        .field("popover_text", alpha(p.popover_text))
        .field("scrim", alpha(p.scrim))
        .field(
            "chart",
            HostValue::Array(p.chart.into_iter().map(solid).collect()),
        )
        .into()
}

/// 一档字：`text_size`、`line_height` 是 rem 字符串，直接交给元素的同名方法。
fn text_style(style: TextStyle) -> HostValue {
    HostObject::new()
        .field("text_size", format!("{}rem", style.size / 16.))
        .field("line_height", format!("{}rem", style.line_height / 16.))
        .field("font_weight", f64::from(style.weight.0))
        .into()
}

fn type_scale() -> HostValue {
    HostObject::new()
        .field("ui", text_style(TYPE.ui))
        .field("ui_medium", text_style(TYPE.ui_medium))
        .field("ui_compact", text_style(TYPE.ui_compact))
        .field("small", text_style(TYPE.small))
        .field("small_medium", text_style(TYPE.small_medium))
        .field("body", text_style(TYPE.body))
        .field("body_medium", text_style(TYPE.body_medium))
        .field("text_small", text_style(TYPE.text_small))
        .field("text_small_medium", text_style(TYPE.text_small_medium))
        .field("text_xsmall", text_style(TYPE.text_xsmall))
        .field("text_xsmall_medium", text_style(TYPE.text_xsmall_medium))
        .field("mono", text_style(TYPE.mono))
        .into()
}

fn radius() -> HostValue {
    HostObject::new()
        .field("xs", f64::from(RADIUS.xs))
        .field("sm", f64::from(RADIUS.sm))
        .field("md", f64::from(RADIUS.md))
        .field("lg", f64::from(RADIUS.lg))
        .field("xl", f64::from(RADIUS.xl))
        .field("row", f64::from(f32::from(ui_theme::row_radius())))
        .field("card", f64::from(f32::from(ui_theme::card_radius())))
        .into()
}

/// 一档投影的几层，每层同元素 `shadow([...])` 收的 `{ x, y, blur, spread, color, inset }`。
fn shadows(layers: &[BoxShadow]) -> HostValue {
    HostValue::Array(
        layers
            .iter()
            .map(|layer| {
                HostObject::new()
                    .field("x", f64::from(f32::from(layer.offset.x)))
                    .field("y", f64::from(f32::from(layer.offset.y)))
                    .field("blur", f64::from(f32::from(layer.blur_radius)))
                    .field("spread", f64::from(f32::from(layer.spread_radius)))
                    .field("color", alpha(Rgba::from(layer.color)))
                    .field("inset", layer.inset)
                    .into()
            })
            .collect(),
    )
}

fn elevations() -> HostValue {
    let e = elevation::current();
    HostObject::new()
        .field("raised", shadows(&e.raised))
        .field("floating", shadows(&e.floating))
        .field("overlay", shadows(&e.overlay))
        .field("md", shadows(&e.md))
        .field("input", shadows(&e.input))
        .into()
}

/// 当前主题：深浅、色板、字阶、圆角、投影，和用户选的等宽字体。
pub(super) fn theme() -> HostValue {
    HostObject::new()
        .field("light", ui_theme::is_light())
        .field("palette", palette())
        .field("type", type_scale())
        .field(
            "fonts",
            HostObject::new().field("mono", crate::model::font_config::font_family()),
        )
        .field("radius", radius())
        .field("elevation", elevations())
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field<'a>(value: &'a HostValue, name: &str) -> &'a HostValue {
        match value {
            HostValue::Object(fields) => {
                &fields
                    .iter()
                    .find(|(key, _)| key == name)
                    .unwrap_or_else(|| panic!("没有 {name}"))
                    .1
            }
            other => panic!("不是对象：{other:?}"),
        }
    }

    #[test]
    fn colors_are_hex_and_type_is_in_rems() {
        let theme = theme();
        let palette = field(&theme, "palette");
        let HostValue::Str(run) = field(palette, "run") else {
            panic!("run 不是字符串");
        };
        assert!(run.starts_with('#') && run.len() == 7, "{run}");
        let HostValue::Str(hover) = field(palette, "row_hover") else {
            panic!("row_hover 不是字符串");
        };
        assert_eq!(hover.len(), 9, "带透明度的是 #rrggbbaa：{hover}");
        let ui = field(field(&theme, "type"), "ui");
        assert_eq!(field(ui, "text_size"), &HostValue::from("0.8125rem"));
        assert_eq!(field(ui, "line_height"), &HostValue::from("1.25rem"));
        assert_eq!(field(ui, "font_weight"), &HostValue::Number(440.));
    }

    /// 投影每层是 `shadow([...])` 收的形状，颜色带透明度。
    #[test]
    fn elevation_layers_match_the_shadow_spec() {
        let theme = theme();
        let HostValue::Array(input) = field(field(&theme, "elevation"), "input") else {
            panic!("input 不是数组");
        };
        let layer = input.first().expect("输入框有投影");
        for name in ["x", "y", "blur", "spread"] {
            assert!(matches!(field(layer, name), HostValue::Number(_)), "{name}");
        }
        let HostValue::Str(color) = field(layer, "color") else {
            panic!("color 不是字符串");
        };
        assert_eq!(color.len(), 9, "{color}");
        assert_eq!(field(field(&theme, "radius"), "md"), &HostValue::Number(6.));
    }

    #[test]
    fn alpha_rounds_each_channel() {
        let color = Rgba {
            r: 1.,
            g: 0.,
            b: 0.5,
            a: 0.1,
        };
        assert_eq!(alpha(color), HostValue::from("#ff00801a"));
    }
}
