//! gpui-base 自己的主题。它直接画的几样东西读这份：输入框的光标和选区、富文本的选区和
//! 代码字体、滚动条、分栏手柄。颜色在装 smelt 主题时（[`crate::theme_catalog::apply`]）
//! 从 smelt 的语义色写进去，代码字体在换字体时（[`set_code_font`]）写进去，别处不改它。

use std::time::Duration;

use gpui::{App, Hsla, Rgba, px};
use gpui_base::{
    ScrollbarEntrance, ScrollbarMotion, ScrollbarStyles, ScrollbarTheme, Theme, ThemeAppearance,
};

use crate::kit::RADIUS;
use crate::theme_config::ThemeColors;

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
    let hsla = |color: Rgba| -> Hsla { color.into() };
    let theme = Theme::global_mut(cx);
    theme.appearance = if dark {
        ThemeAppearance::Dark
    } else {
        ThemeAppearance::Light
    };

    let tokens = &mut theme.tokens.colors;
    tokens.background = hsla(c.bg_base);
    tokens.foreground = hsla(c.text_base);
    tokens.surface = hsla(c.surface_raised);
    tokens.surface_foreground = hsla(c.text_base);
    tokens.primary = hsla(c.bg_contrast);
    tokens.primary_foreground = hsla(c.text_contrast);
    tokens.secondary = hsla(c.bg_button_neutral);
    tokens.secondary_foreground = hsla(c.text_base);
    tokens.muted = hsla(c.layer_01);
    tokens.muted_foreground = hsla(c.text_muted);
    tokens.accent = hsla(c.hover);
    tokens.accent_foreground = hsla(c.text_base);
    tokens.destructive = hsla(c.danger.icon);
    tokens.destructive_foreground = hsla(c.text_contrast);
    tokens.border = hsla(c.border_base);
    tokens.input = hsla(c.border_strong);
    tokens.ring = hsla(c.border_focus);
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

    theme.resizable.handle = Some(hsla(c.border_base));
    theme.resizable.active_handle = Some(hsla(c.border_focus));
}
