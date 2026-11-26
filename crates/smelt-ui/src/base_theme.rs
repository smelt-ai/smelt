//! gpui-base 自己的主题。它直接画的几样东西读这份：输入框的光标和选区、滚动条。颜色在装
//! smelt 主题时（[`crate::theme_catalog::apply`]）从 smelt 的语义色写进去，代码字体在换字体
//! 时（[`set_code_font`]）写进去，别处不改它。

use std::time::Duration;

use gpui::{App, Hsla, Rgba, px, rgb};
use gpui_base::{
    ScrollbarEntrance, ScrollbarMotion, ScrollbarStyles, ScrollbarTheme, Theme, ThemeAppearance,
};

use crate::kit::RADIUS;
use crate::theme_config::ThemeColors;
use crate::ui_theme::{self, flatten};

/// 滚动停下后，滚动条还留多久才淡出。
const SCROLLBAR_IDLE: Duration = Duration::from_secs(2);
/// 滚动条淡入、淡出各用多久。
const SCROLLBAR_ENTER: Duration = Duration::from_millis(300);
const SCROLLBAR_EXIT: Duration = Duration::from_millis(500);
/// 指针移上去时滚动条变宽用多久。
const SCROLLBAR_EXPAND: Duration = Duration::from_millis(300);

/// 换代码字体，空串是出厂默认。终端和差异视图每次画都读 `font_config`，gpui-base 的富文本
/// （Markdown 代码块和行内代码）读它自己主题里的那份，这里两处一起换。
pub fn set_code_font(family: &str, cx: &mut App) {
    smelt_model::font_config::set_font_family(family);
    Theme::global_mut(cx).tokens.typography.mono = smelt_model::font_config::font_family().into();
}

pub(crate) fn install(colors: &ThemeColors, dark: bool, cx: &mut App) {
    let c = colors;
    // 语义色除选区外先压到窗口实际的底上（[`ui_theme::backdrop`]：主题底色叠在材质上）再
    // 写进去。悬停、描边这些主题色带透明度，gpui-shell 交给 JS 插件时只写 `#rrggbb`、丢掉
    // 透明度：深色主题白 6% 的悬停到了 JS 手里就成了纯白。压在主题底色上也不对，毛玻璃时
    // 底色只铺了一部分，压出来的颜色和周围接不上。滚动条、分栏手柄盖在内容上面，也不交给
    // 插件，照主题原色。
    let behind = ui_theme::backdrop();
    let solid = |color: Rgba| -> Hsla { rgb(flatten(color, behind)).into() };
    let hsla = |color: Rgba| -> Hsla { color.into() };
    let theme = Theme::global_mut(cx);
    theme.appearance = if dark {
        ThemeAppearance::Dark
    } else {
        ThemeAppearance::Light
    };

    let tokens = &mut theme.tokens.colors;
    tokens.background = solid(c.bg_base);
    tokens.foreground = solid(c.text_base);
    tokens.surface = solid(c.surface_raised);
    tokens.surface_foreground = solid(c.text_base);
    tokens.primary = solid(c.bg_contrast);
    tokens.primary_foreground = solid(c.text_contrast);
    tokens.secondary = solid(c.bg_button_neutral);
    tokens.secondary_foreground = solid(c.text_base);
    tokens.muted = solid(c.layer_01);
    tokens.muted_foreground = solid(c.text_muted);
    tokens.accent = solid(c.hover);
    tokens.accent_foreground = solid(c.text_base);
    tokens.destructive = solid(c.danger.icon);
    tokens.destructive_foreground = solid(c.text_contrast);
    tokens.border = solid(c.border_base);
    tokens.input = solid(c.border_strong);
    tokens.ring = solid(c.border_focus);
    // 选区要留透明度：gpui-base 把它盖在图片这类行内对象上面，压实了会把底下整块遮住。
    tokens.selection = hsla(c.selection);

    let radius = &mut theme.tokens.radius;
    radius.sm = px(RADIUS.sm);
    radius.md = px(RADIUS.md);
    radius.lg = px(RADIUS.lg);
    radius.xl = px(RADIUS.xl);

    let thumb = hsla(c.border_strong);
    let thumb_hover = hsla(c.text_faint);
    let track_border = hsla(c.border_base);
    let thumb_radius = px(RADIUS.md);
    theme.scrollbar = ScrollbarTheme::new()
        .with_motion(
            ScrollbarMotion::default()
                .with_idle(SCROLLBAR_IDLE)
                .with_enter(SCROLLBAR_ENTER)
                .with_exit(SCROLLBAR_EXIT)
                .with_expand(SCROLLBAR_EXPAND)
                .with_entrance(ScrollbarEntrance::Fade)
                .with_thumb_hover_entrance(ScrollbarEntrance::Fade),
        )
        .with_styles(
            ScrollbarStyles::default()
                .track_active(|style| style.border_color(track_border))
                .thumb(|style| style.bg(thumb).radius(thumb_radius))
                .thumb_hover(|style| style.bg(thumb_hover).radius(thumb_radius))
                .thumb_active(|style| style.bg(thumb_hover).radius(thumb_radius)),
        );
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;
    use crate::ui_theme::pack;

    /// JS 插件拿到的颜色没有透明度，交出去的语义色就得是实色；只有选区留着透明度。
    #[gpui::test]
    fn semantic_colors_are_solid_except_selection(cx: &mut TestAppContext) {
        cx.update(|cx| {
            for name in [
                smelt_model::theme_index::DEFAULT_DARK,
                smelt_model::theme_index::DEFAULT_LIGHT,
            ] {
                assert!(crate::theme_catalog::apply(name, cx));
                let c = crate::theme_catalog::active(cx).unwrap().colors();
                assert!(c.hover.a < 1.0, "{name}: 悬停色不带透明度，测不到压平");
                let tokens = Theme::global(cx).tokens.colors;
                let accent: Rgba = tokens.accent.into();
                assert_eq!(
                    pack(accent),
                    flatten(c.hover, ui_theme::backdrop()),
                    "{name}"
                );
                for (token, color) in [
                    ("background", tokens.background),
                    ("muted", tokens.muted),
                    ("accent", tokens.accent),
                    ("border", tokens.border),
                    ("input", tokens.input),
                ] {
                    assert_eq!(color.a, 1.0, "{name}: {token}");
                }
                assert!(tokens.selection.a < 1.0, "{name}: selection");
            }
        });
    }
}
