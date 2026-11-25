//! smelt 的主题给 NSH：smelt-ui 色板（[`ui_theme::Palette`]）投影成 NSH 主题的 shadcn 语义位，
//! 同一个角色对同一个位，NSH 画的和 smelt-ui 画的才对得上。只给当前这一档深浅的色，深浅
//! 两套都填它：换主题、换深浅时整份重给（见 `PilotView`）。

use gpui::{BoxShadow, Rgba};
use nsh_platform_gpui::{AdaptiveColor, Appearance, Color, Theme};
use serde_json::{Map, Value, json};
use smelt_ui::button::{ButtonShadow, resting_shadows};
use smelt_ui::kit::{RADIUS, Radius, TYPE, TextStyle, Type, elevation};
use smelt_ui::theme_config::{Elevation, StateColors, ThemeColors};
use smelt_ui::ui_theme::{self, Palette};

/// 不画的投影：主题这一档没写层时给它，类名照样认得。
const NO_SHADOW: &str = "0 0 #00000000";

/// 几层投影写成 CSS `box-shadow`，色带透明度（`#rrggbbaa`）。
fn shadow_css(layers: &[BoxShadow]) -> String {
    let css: Vec<String> = layers
        .iter()
        .map(|layer| {
            let color = Rgba::from(layer.color);
            let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
            format!(
                "{}{}px {}px {}px {}px #{:02x}{:02x}{:02x}{:02x}",
                if layer.inset { "inset " } else { "" },
                f32::from(layer.offset.x),
                f32::from(layer.offset.y),
                f32::from(layer.blur_radius),
                f32::from(layer.spread_radius),
                channel(color.r),
                channel(color.g),
                channel(color.b),
                channel(color.a),
            )
        })
        .collect();
    if css.is_empty() {
        NO_SHADOW.to_owned()
    } else {
        css.join(", ")
    }
}

/// 交给 NSH 的投影令牌（类名就是令牌名，`shadow-floating`）：主题 `elevation` 的每一档都给，类名是
/// `shadow-` 加主题键（下划线换连字符），见 [`elevation_levels`]；另加按钮实际画的那几组
/// （[`resting_shadows`]，已按底色透不透换过）。原始的 `button_neutral` 也在，但半透明底上画外投影
/// 会透出一圈灰，按钮请用 `shadow-button`。
fn shadows() -> Vec<(String, Vec<BoxShadow>)> {
    let mut out: Vec<(String, Vec<BoxShadow>)> = elevation_levels(&elevation::current())
        .into_iter()
        .map(|(key, layers)| (format!("shadow-{}", key.replace('_', "-")), layers))
        .collect();
    out.extend([
        (
            "shadow-button".to_owned(),
            resting_shadows(ButtonShadow::Neutral),
        ),
        (
            "shadow-button-primary".to_owned(),
            resting_shadows(ButtonShadow::Primary),
        ),
        (
            "shadow-button-floating".to_owned(),
            resting_shadows(ButtonShadow::Floating),
        ),
    ]);
    out
}

/// 主题的每一档投影和它在主题文件里的键。整个解构不留 `..`：主题加一档，这里编译不过，插件就
/// 不会漏拿。
fn elevation_levels(e: &Elevation) -> Vec<(&'static str, Vec<BoxShadow>)> {
    let Elevation {
        raised,
        floating,
        overlay,
        button_neutral,
        button_contrast,
        elements,
        switch_off,
        switch_on,
        md,
        knob,
        input,
    } = e;
    vec![
        ("raised", raised.clone()),
        ("floating", floating.clone()),
        ("overlay", overlay.clone()),
        ("button_neutral", button_neutral.clone()),
        ("button_contrast", button_contrast.clone()),
        ("elements", elements.clone()),
        ("switch_off", switch_off.clone()),
        ("switch_on", switch_on.clone()),
        ("md", md.clone()),
        ("knob", knob.clone()),
        ("input", input.clone()),
    ]
}

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
        // 全局语义色令牌（全应用共享，由引擎原生解析 Tailwind class，不经 JS 状态广播）。
        ("faint", solid(p.faint)),
        ("link", solid(p.link)),
        ("need", solid(p.need)),
        ("ok", solid(p.ok)),
        ("bad", solid(p.bad)),
        ("on-bad", solid(p.on_bad)),
        ("soft", solid(p.soft)),
        ("soft-hover", solid(p.soft_hover)),
        ("row-selected", alpha(p.row_selected)),
        ("scrim", alpha(p.scrim)),
    ] {
        theme.set_color(name, color);
    }
    for (name, layers) in shadows() {
        let css = shadow_css(&layers);
        let set = theme.set_shadow(&name, &css, &css);
        debug_assert!(set, "{name}: {css}");
    }
    theme.set_number("radius", RADIUS.md);
    theme.set_font(
        "font-mono",
        smelt_plugin_api::model::font_config::font_family(),
    );
    theme
}

// ---- 插件的 `theme()` ----

/// 局部变量按名字装进对象：键就是变量名（也就是结构的字段名、主题文件的键）。
macro_rules! fields {
    ($($name:ident),* $(,)?) => {{
        let mut map = Map::new();
        $(map.insert(stringify!($name).to_owned(), Css::css($name));)*
        Value::Object(map)
    }};
}

/// 一个主题值写成插件拿到的样子：不透明色 `#rrggbb`，带透明度的 `rgba(…)`，成组的给数组或对象。
trait Css {
    fn css(&self) -> Value;
}

impl Css for u32 {
    fn css(&self) -> Value {
        smelt_plugin_api::hex_color(*self).into()
    }
}

impl Css for Rgba {
    fn css(&self) -> Value {
        let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
        format!(
            "rgba({}, {}, {}, {:.3})",
            channel(self.r),
            channel(self.g),
            channel(self.b),
            self.a.clamp(0., 1.)
        )
        .into()
    }
}

impl<T: Css, const N: usize> Css for [T; N] {
    fn css(&self) -> Value {
        Value::Array(self.iter().map(Css::css).collect())
    }
}

impl Css for StateColors {
    fn css(&self) -> Value {
        let StateColors {
            bg,
            fg,
            border,
            icon,
        } = self;
        fields!(bg, fg, border, icon)
    }
}

/// 一档投影写成 NSH 样式的 `boxShadow` 数组，插件可以直接放进 `style`。
impl Css for Vec<BoxShadow> {
    fn css(&self) -> Value {
        Value::Array(
            self.iter()
                .map(|layer| {
                    json!({
                        "x": f32::from(layer.offset.x),
                        "y": f32::from(layer.offset.y),
                        "blur": f32::from(layer.blur_radius),
                        "spread": f32::from(layer.spread_radius),
                        "color": Rgba::from(layer.color).css(),
                        "inset": layer.inset,
                    })
                })
                .collect(),
        )
    }
}

/// 插件 `theme()` 拿到的整份主题，换主题、换深浅、改界面字号时整份重给：
///
/// - `palette`：语义色板（[`Palette`]），画界面先用它；
/// - `colors`：主题文件里的每个颜色键（[`ThemeColors`]），色板没有的位从这里取；
/// - `elevation`：每一档投影，`boxShadow` 数组；同一档也有类名 `shadow-<键>`；
/// - `type`：每一档字，字号、行高按这扇窗口的 rem 换成点；`radius`：每一档圆角；`fonts`。
///
/// 每个结构都整个解构、不留 `..`：smelt-ui 的主题加一个位，这里编译不过，插件就不会漏拿，也
/// 不用等哪个插件要了再来补。
pub(crate) fn plugin_theme(rem: f32) -> Value {
    json!({
        "rem": rem,
        "palette": palette_json(&ui_theme::palette()),
        "colors": colors_json(&ui_theme::colors()),
        "elevation": Value::Object(
            elevation_levels(&elevation::current())
                .into_iter()
                .map(|(key, layers)| (key.to_owned(), layers.css()))
                .collect(),
        ),
        "type": type_json(&TYPE, rem),
        "radius": radius_json(&RADIUS),
        "fonts": {
            "mono": smelt_plugin_api::model::font_config::font_family(),
        },
    })
}

fn palette_json(p: &Palette) -> Value {
    let Palette {
        bg,
        side,
        surface,
        soft,
        soft_hover,
        soft_press,
        row_hover,
        row_selected,
        hair,
        hair_strong,
        text,
        muted,
        faint,
        muted_selected,
        faint_selected,
        ink,
        ink_hover,
        ink_press,
        on_ink,
        button_primary,
        button_primary_foreground,
        button_submit,
        button_submit_foreground,
        button_secondary,
        button_secondary_foreground,
        focus,
        selection,
        link,
        accent,
        on_accent,
        track,
        knob,
        knob_on,
        need,
        ok,
        bad,
        on_bad,
        popover_text,
        scrim,
        chart,
    } = p;
    fields!(
        bg,
        side,
        surface,
        soft,
        soft_hover,
        soft_press,
        row_hover,
        row_selected,
        hair,
        hair_strong,
        text,
        muted,
        faint,
        muted_selected,
        faint_selected,
        ink,
        ink_hover,
        ink_press,
        on_ink,
        button_primary,
        button_primary_foreground,
        button_submit,
        button_submit_foreground,
        button_secondary,
        button_secondary_foreground,
        focus,
        selection,
        link,
        accent,
        on_accent,
        track,
        knob,
        knob_on,
        need,
        ok,
        bad,
        on_bad,
        popover_text,
        scrim,
        chart,
    )
}

fn colors_json(c: &ThemeColors) -> Value {
    let ThemeColors {
        page,
        bg_base,
        bg_deep,
        layer_01,
        layer_02,
        layer_03,
        layer_04,
        bg_inverse,
        bg_contrast,
        bg_button_neutral,
        bg_icon_button_contrast,
        bg_accent,
        submit_highlight,
        submit_hover,
        submit_fg,
        submit_icon,
        text_base,
        text_muted,
        text_faint,
        text_inverse,
        text_contrast,
        text_accent,
        icon_base,
        icon_muted,
        icon_faint,
        icon_inverse,
        icon_contrast,
        icon_accent,
        border_muted,
        border_base,
        border_strong,
        border_focus,
        hover,
        pressed,
        contrast_hover,
        contrast_pressed,
        scrim,
        depth_top,
        depth_bottom,
        white,
        black,
        knob,
        knob_on,
        success,
        warning,
        danger,
        info,
        surface_raised,
        selection,
        chart,
    } = c;
    fields!(
        page,
        bg_base,
        bg_deep,
        layer_01,
        layer_02,
        layer_03,
        layer_04,
        bg_inverse,
        bg_contrast,
        bg_button_neutral,
        bg_icon_button_contrast,
        bg_accent,
        submit_highlight,
        submit_hover,
        submit_fg,
        submit_icon,
        text_base,
        text_muted,
        text_faint,
        text_inverse,
        text_contrast,
        text_accent,
        icon_base,
        icon_muted,
        icon_faint,
        icon_inverse,
        icon_contrast,
        icon_accent,
        border_muted,
        border_base,
        border_strong,
        border_focus,
        hover,
        pressed,
        contrast_hover,
        contrast_pressed,
        scrim,
        depth_top,
        depth_bottom,
        white,
        black,
        knob,
        knob_on,
        success,
        warning,
        danger,
        info,
        surface_raised,
        selection,
        chart,
    )
}

fn type_json(t: &Type, rem: f32) -> Value {
    let Type {
        ui,
        ui_medium,
        ui_compact,
        small,
        small_medium,
        body,
        body_medium,
        text_small,
        text_small_medium,
        text_xsmall,
        text_xsmall_medium,
        mono,
    } = t;
    let text = |style: &TextStyle| {
        json!({
            "size": style.size / 16. * rem,
            "line_height": style.line_height / 16. * rem,
            "weight": style.weight.0,
        })
    };
    json!({
        "ui": text(ui),
        "ui_medium": text(ui_medium),
        "ui_compact": text(ui_compact),
        "small": text(small),
        "small_medium": text(small_medium),
        "body": text(body),
        "body_medium": text(body_medium),
        "text_small": text(text_small),
        "text_small_medium": text(text_small_medium),
        "text_xsmall": text(text_xsmall),
        "text_xsmall_medium": text(text_xsmall_medium),
        "mono": text(mono),
    })
}

fn radius_json(r: &Radius) -> Value {
    let Radius { xs, sm, md, lg, xl } = *r;
    json!({
        "xs": xs,
        "sm": sm,
        "md": md,
        "lg": lg,
        "xl": xl,
        // 列表行的圆角（`ui_theme::row_radius`），不在 `RADIUS` 那几档里。
        "row": f32::from(ui_theme::row_radius()),
    })
}

/// 打包配置（`nsh.json`）里的 `"theme"`：只为让打包器认得 smelt 的令牌类名（`bg-scrim`、
/// `shadow-floating`），免得构建时报未知类名。值不会被用到——宿主给每个视图 `set_theme`
/// 整份换上 [`nsh_theme`]——所以色填透明、投影填不画，构建产物和预编译指纹也就不随用户主题变。
pub(crate) fn build_theme_config() -> serde_json::Value {
    let tokens = nsh_theme().into_tokens();
    let mut names: Vec<(&str, &str)> = tokens
        .colors()
        .map(|(name, _)| (name, "#00000000"))
        .chain(tokens.shadows().map(|(name, _)| (name, NO_SHADOW)))
        .collect();
    names.sort_unstable();
    let colors: serde_json::Map<String, serde_json::Value> = names
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.into()))
        .collect();
    serde_json::json!({ "cssVars": { "light": colors.clone(), "dark": colors } })
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

    /// 插件拿到的每一档投影都有同名类名，`style` 里写和 `class` 里写是同一个东西。
    #[test]
    fn every_elevation_level_is_both_json_and_class() {
        let theme = plugin_theme(16.);
        let levels = theme["elevation"].as_object().unwrap();
        assert_eq!(levels.len(), elevation_levels(&elevation::current()).len());
        let classes: Vec<String> = shadows().into_iter().map(|(name, _)| name).collect();
        for key in levels.keys() {
            let class = format!("shadow-{}", key.replace('_', "-"));
            assert!(classes.contains(&class), "{class}");
        }
        assert!(theme["colors"]["success"]["bg"].is_string());
        assert_eq!(theme["colors"]["chart"].as_array().unwrap().len(), 5);
        assert!(theme["palette"]["button_primary"][2].is_string());
        assert!(theme["type"]["mono"]["size"].is_number());
    }

    #[test]
    fn build_theme_config_names_every_token_and_ignores_the_palette() {
        let config = build_theme_config();
        let light = config["cssVars"]["light"].as_object().unwrap();
        for name in ["scrim", "faint", "row-selected", "popover-foreground"] {
            assert_eq!(light[name], "#00000000", "{name}");
        }
        assert_eq!(light["shadow-floating"], NO_SHADOW);
        let parsed = nsh_platform_gpui::Theme::from_json(&config.to_string()).unwrap();
        assert!(parsed.color("scrim").is_some());
        assert!(parsed.color("run").is_none(), "在跑不配色");
        assert!(parsed.tokens().shadow("shadow-overlay").is_some());
    }

    #[test]
    fn shadows_are_css_box_shadows_and_every_one_is_a_token() {
        let layer = BoxShadow {
            color: Rgba {
                r: 0.,
                g: 0.,
                b: 0.,
                a: 0.25,
            }
            .into(),
            offset: gpui::point(gpui::px(0.), gpui::px(4.)),
            blur_radius: gpui::px(12.),
            spread_radius: gpui::px(-1.5),
            inset: false,
        };
        let ring = BoxShadow {
            inset: true,
            ..layer.clone()
        };
        assert_eq!(
            shadow_css(&[layer, ring]),
            "0px 4px 12px -1.5px #00000040, inset 0px 4px 12px -1.5px #00000040"
        );
        assert_eq!(shadow_css(&[]), NO_SHADOW);
        let theme = nsh_theme();
        for (name, _) in shadows() {
            assert!(theme.tokens().shadow(&name).is_some(), "{name}");
        }
    }

    /// 投影只从主题来（设计语言第 2 节）：内置插件、SDK、主壳里不写 Tailwind 自带的几档
    /// （`shadow-md` 已由主题令牌接管，不算）。
    #[test]
    fn builtin_sources_take_shadows_from_the_theme() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n != "node_modules") {
                        walk(&path, out);
                    }
                } else if path.extension().is_some_and(|e| {
                    ["tsx", "ts", "jsx", "js", "mjs"].contains(&&*e.to_string_lossy())
                }) {
                    out.push(path);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        for dir in ["src/plugins", "shell", "../../plugins"] {
            walk(&root.join(dir), &mut files);
        }
        let mut found = Vec::new();
        for file in &files {
            let text = std::fs::read_to_string(file).unwrap();
            for (index, line) in text.lines().enumerate() {
                for size in ["2xs", "xs", "sm", "lg", "xl", "2xl"] {
                    let class = format!("shadow-{size}");
                    let hit = line.match_indices(&class).any(|(at, _)| {
                        let before = line[..at].chars().next_back();
                        let after = line[at + class.len()..].chars().next();
                        !before.is_some_and(|c| c.is_alphanumeric() || c == '-')
                            && !after.is_some_and(|c| c.is_alphanumeric() || c == '-')
                    });
                    if hit {
                        found.push(format!("{}:{}: {class}", file.display(), index + 1));
                    }
                }
            }
        }
        assert!(files.len() > 50, "只扫到 {} 个文件", files.len());
        assert!(found.is_empty(), "用主题的投影令牌：\n{}", found.join("\n"));
    }

    #[test]
    fn nsh_theme_exposes_smelt_semantic_tokens() {
        let theme = nsh_theme();
        for name in [
            "faint",
            "link",
            "need",
            "ok",
            "bad",
            "on-bad",
            "soft",
            "soft-hover",
            "row-selected",
            "scrim",
            "chart-1",
            "chart-5",
        ] {
            assert!(
                theme.color(name).is_some(),
                "missing NSH theme token {name}"
            );
        }
    }
}
