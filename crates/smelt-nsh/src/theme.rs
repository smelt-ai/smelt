//! smelt 的主题给 NSH：smelt-ui 色板（[`ui_theme::Palette`]）投影成 NSH 主题的 shadcn 语义位，
//! 同一个角色对同一个位，NSH 画的和 smelt-ui 画的才对得上。只给当前这一档深浅的色，深浅
//! 两套都填它：换主题、换深浅时整份重给（见 `PilotView`）。

use gpui::{BoxShadow, Rgba};
use nsh_platform_gpui::{AdaptiveColor, Appearance, Color, Theme};
use smelt_ui::button::{ButtonShadow, resting_shadows};
use smelt_ui::kit::{RADIUS, elevation};
use smelt_ui::ui_theme;

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

/// 交给 NSH 的投影令牌（类名就是令牌名，`shadow-floating`）：主题 `elevation` 里组件直接取的几档，
/// 和按钮实际画的那几组（[`resting_shadows`]，已按底色透不透换过）。按钮不给原始的
/// `button_neutral`：半透明底上画外投影会透出一圈灰，要不要换内细圈归宿主定。开关的槽
/// （关、开两档内细圈）和圆钮也给，SDK 的开关照它画。
fn shadows() -> [(&'static str, Vec<BoxShadow>); 10] {
    let e = elevation::current();
    [
        ("shadow-raised", e.raised.clone()),
        ("shadow-floating", e.floating.clone()),
        ("shadow-md", e.md.clone()),
        ("shadow-overlay", e.overlay.clone()),
        ("shadow-button", resting_shadows(ButtonShadow::Neutral)),
        (
            "shadow-button-primary",
            resting_shadows(ButtonShadow::Primary),
        ),
        (
            "shadow-button-floating",
            resting_shadows(ButtonShadow::Floating),
        ),
        ("shadow-switch-off", e.switch_off.clone()),
        ("shadow-switch-on", e.switch_on.clone()),
        ("shadow-knob", e.elements.clone()),
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
        let set = theme.set_shadow(name, &css, &css);
        debug_assert!(set, "{name}: {css}");
    }
    theme.set_number("radius", RADIUS.md);
    theme.set_font(
        "font-mono",
        smelt_plugin_api::model::font_config::font_family(),
    );
    theme
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
            assert!(theme.tokens().shadow(name).is_some(), "{name}");
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
