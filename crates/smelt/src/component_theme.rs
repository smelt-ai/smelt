//! 让组件库（gpui-component）的颜色跟着 smelt 的主题走。
//!
//! smelt 的主题存在 `smelt_ui::theme_catalog::ActiveTheme`，以它为准。应用里还在用组件库的
//! 只剩 git diff 视图。它不认 smelt 的主题，只读组件库自己的 `Theme` 全局。所以每次换主题，
//! 这里就把 smelt 当前的颜色翻译成一份组件库 `Theme` 整份换上。这份只是翻译出来的副本，别处
//! 不改它。（文件页的代码编辑器和 Markdown 代码块已经用 smelt-ui 自己的高亮，直接读
//! `ActiveTheme`，不经过这里。）
//!
//! 组件库 `Theme` 的字段很多，只填 diff 视图会读的：底、字、线、选区、焦点环、列表悬停、
//! 状态色、滚动条，加上代码高亮（主题文件的 `syntax` 段、行号和当前行、诊断色）。组件库别的
//! 控件已经不用，它们的字段不填，留组件库的默认值。

use std::rc::Rc;

use gpui::{App, Rgba, px};
use gpui_component::Theme;
use serde_json::{Map, Value, json};
use smelt_ui::theme_catalog::{ActiveTheme, syntax_styles};
use smelt_ui::theme_config::{StateColors, SyntaxFontStyle, ThemeColors, ThemeConfig};

/// 装上同步。在 `gpui_component::init` 之后、第一次装主题之前调一次。
pub(crate) fn install(cx: &mut App) {
    cx.observe_global::<ActiveTheme>(sync).detach();
}

fn sync(cx: &mut App) {
    if !cx.has_global::<Theme>() {
        return;
    }
    let Some(active) = smelt_ui::theme_catalog::active(cx) else {
        return;
    };
    let source = projection(active.config(), active.colors());
    let config: gpui_component::ThemeConfig = match serde_json::from_value(source) {
        Ok(config) => config,
        Err(error) => {
            // 投影全是这里拼的，组件库读不了是这里写错了；保留上一份，不让界面崩。
            eprintln!(
                "[theme] 组件库读不了主题「{}」的投影：{error}",
                active.config().name()
            );
            return;
        }
    };
    let config = Rc::new(config);
    Theme::update(cx, |theme| {
        // 整份换上：`apply_config` 先回到组件库的默认色，再盖上投影里的位；投影不带字体，
        // 字号字体归外观设置（`settings::sync_theme_fonts`），这里不动。
        theme.radius = px(smelt_ui::kit::RADIUS.md);
        theme.radius_lg = px(smelt_ui::kit::RADIUS.lg);
        theme.shadow = true;
        theme.apply_config(&config);
    });
    cx.refresh_windows();
}

/// 当前主题投影成一份组件库主题。`colors` 是按窗口材质配过字色的那份语义色。
fn projection(config: &ThemeConfig, c: &ThemeColors) -> Value {
    let colors = json!({
        "background": hex(c.bg_base),
        "foreground": hex(c.text_base),
        "border": hex(c.border_base),
        "input.border": hex(c.border_strong),
        "muted.background": hex(c.layer_01),
        "muted.foreground": hex(c.text_muted),
        "accent.background": hex(c.hover),
        "accent.foreground": hex(c.text_base),
        "secondary.background": hex(c.bg_button_neutral),
        "secondary.foreground": hex(c.text_base),
        "popover.background": hex(c.surface_raised),
        "popover.foreground": hex(c.text_base),
        "primary.background": hex(c.bg_contrast),
        "primary.foreground": hex(c.text_contrast),
        "danger.background": hex(c.danger.icon),
        "danger.foreground": hex(c.text_contrast),
        "success.background": hex(c.success.icon),
        "warning.background": hex(c.warning.icon),
        "info.background": hex(c.info.icon),
        "base.red": hex(c.danger.icon),
        "base.green": hex(c.success.icon),
        "base.yellow": hex(c.warning.icon),
        "base.blue": hex(c.info.icon),
        "base.cyan": hex(c.info.icon),
        "ring": hex(c.border_focus),
        "selection.background": hex(c.selection),
        "caret": hex(c.text_base),
        "link": hex(c.text_accent),
        "list.hover.background": hex(c.hover),
        "list.active.background": hex(c.pressed),
        "table.head.background": hex(c.layer_01),
        "table.head.foreground": hex(c.text_base),
        "scrollbar.background": "#00000000",
        "scrollbar.thumb.background": hex(c.border_strong),
        "scrollbar.thumb.hover.background": hex(c.text_faint),
        "drag.border": hex(c.border_focus),
        "overlay": hex(c.scrim),
    });
    let mut highlight = Map::new();
    for (key, color) in [
        ("editor.background", c.bg_base),
        ("editor.foreground", c.text_base),
        ("editor.active_line.background", c.hover),
        ("editor.line_number", c.text_faint),
        ("editor.active_line_number", c.text_base),
        ("editor.invisible", c.text_faint),
        ("editor.gutter.background", c.bg_base),
        ("created", c.success.icon),
        ("modified", c.warning.icon),
        ("deleted", c.danger.icon),
    ] {
        highlight.insert(key.into(), hex(color).into());
    }
    // 诊断（错误、警告……）在编辑器里画成字色加浅底加边框，用同一状态的三个色。
    for (key, state) in [
        ("error", c.danger),
        ("warning", c.warning),
        ("info", c.info),
        ("success", c.success),
        ("hint", c.info),
    ] {
        insert_state(&mut highlight, key, state);
    }
    highlight.insert("syntax".into(), syntax(config));
    json!({
        "name": config.name().to_string(),
        "mode": config.mode().key(),
        "colors": colors,
        "highlight": highlight,
    })
}

fn insert_state(highlight: &mut Map<String, Value>, key: &str, state: StateColors) {
    highlight.insert(key.into(), hex(state.fg).into());
    highlight.insert(format!("{key}.background"), hex(state.bg).into());
    highlight.insert(format!("{key}.border"), hex(state.border).into());
}

/// 主题的代码高亮样式交给 diff 视图的高亮器，和代码编辑器用同一份
/// （`theme_catalog::syntax_styles`：没写 `syntax` 的主题用同明暗 smelt 主题的）。
fn syntax(config: &ThemeConfig) -> Value {
    let mut syntax = Map::new();
    for (key, style) in syntax_styles(config) {
        let mut entry = Map::new();
        if let Some(color) = style.color {
            entry.insert("color".into(), hex(color).into());
        }
        if let Some(font_style) = style.font_style {
            let font_style = match font_style {
                SyntaxFontStyle::Normal => "normal",
                SyntaxFontStyle::Italic => "italic",
                SyntaxFontStyle::Underline => "underline",
            };
            entry.insert("font_style".into(), font_style.into());
        }
        if let Some(weight) = style.font_weight {
            entry.insert("font_weight".into(), weight.into());
        }
        // 高亮器里文档注释这一类的键名写作 `comment_doc`，查的时候按 `comment.doc` 找。
        let key = if *key == "comment.doc" {
            "comment_doc"
        } else {
            *key
        };
        syntax.insert(key.into(), entry.into());
    }
    syntax.into()
}

fn hex(color: Rgba) -> String {
    let q = |channel: f32| (channel.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        q(color.r),
        q(color.g),
        q(color.b),
        q(color.a)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 投影组件库读得了：颜色、高亮、文档注释的键名都对上。
    #[test]
    fn projection_parses_as_a_component_theme() {
        let source = serde_json::json!({
            "version": 1,
            "name": "test",
            "themes": [{
                "name": "T",
                "mode": "dark",
                "colors": { "bg_base": "#101820", "text_base": "#fafafa" },
                "syntax": {
                    "keyword": { "color": "#ff0000", "font_weight": 700 },
                    "comment.doc": { "color": "#00ff00", "font_style": "italic" }
                }
            }]
        });
        let mut catalog = smelt_ui::theme_catalog::ThemeCatalog::default();
        catalog
            .register("test", &[&source.to_string()], |_| false)
            .unwrap();
        let config = catalog.get("T").unwrap();
        let value = projection(config, config.colors());
        assert_eq!(value["colors"]["background"], "#101820ff");
        assert_eq!(
            value["highlight"]["syntax"]["comment_doc"]["font_style"],
            "italic"
        );
        let parsed: gpui_component::ThemeConfig = serde_json::from_value(value).unwrap();
        let style = parsed.highlight.unwrap();
        assert!(style.syntax.style("keyword").is_some());
        assert!(style.syntax.style("comment.doc").is_some());
    }

    /// 没写 `syntax` 的主题照样有代码高亮（同明暗 smelt 主题的）。
    #[test]
    fn missing_syntax_falls_back_to_the_builtin_palette() {
        let mut catalog = smelt_ui::theme_catalog::ThemeCatalog::default();
        let source = r##"{"version":1,"name":"t","themes":[{"name":"L","mode":"light","colors":{"bg_base":"#ffffff","text_base":"#1a1a18"}}]}"##;
        catalog.register("test", &[source], |_| false).unwrap();
        let config = catalog.get("L").unwrap();
        let value = projection(config, config.colors());
        assert!(
            value["highlight"]["syntax"]
                .as_object()
                .is_some_and(|syntax| !syntax.is_empty())
        );
    }
}
