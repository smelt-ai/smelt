//! 差异视图用到的颜色和字体，从 smelt 当前主题取：`cx.theme().border` 这样读，每次 render
//! 现取，换主题不用另外同步。

use std::sync::Arc;

use gpui::{App, Hsla, Pixels, SharedString, px};
use gpui_base::input::{HighlightStyleResolver, SharedHighlightStyleResolver};

use crate::kit::RADIUS;
use crate::{theme_catalog, ui_theme};

pub(super) struct Theme {
    pub background: Hsla,
    pub foreground: Hsla,
    pub border: Hsla,
    /// 折叠行、分段标题、批注的浅底（按需再调透明度）。
    pub muted: Hsla,
    pub muted_foreground: Hsla,
    pub success: Hsla,
    pub info: Hsla,
    pub danger: Hsla,
    pub warning: Hsla,
    pub list_hover: Hsla,
    pub selection: Hsla,
    /// 加批注按钮的底和字。
    pub primary: Hsla,
    pub primary_foreground: Hsla,
    pub radius: Pixels,
    pub mono_font_family: SharedString,
    /// 代码高亮样式：和代码编辑器用同一份。
    pub highlight_theme: SharedHighlightStyleResolver,
}

pub(super) trait DiffTheme {
    fn theme(&self) -> Theme;
}

impl DiffTheme for App {
    fn theme(&self) -> Theme {
        let c = ui_theme::colors();
        Theme {
            background: c.bg_base.into(),
            foreground: c.text_base.into(),
            border: c.border_base.into(),
            muted: c.layer_01.into(),
            muted_foreground: c.text_muted.into(),
            success: c.success.icon.into(),
            info: c.info.icon.into(),
            danger: c.danger.icon.into(),
            warning: c.warning.icon.into(),
            list_hover: c.hover.into(),
            selection: c.selection.into(),
            primary: c.bg_contrast.into(),
            primary_foreground: c.text_contrast.into(),
            radius: px(RADIUS.md),
            mono_font_family: smelt_model::font_config::font_family().into(),
            highlight_theme: theme_catalog::active(self)
                .map(|active| active.syntax().clone())
                .unwrap_or_else(|| Arc::new(Plain)),
        }
    }
}

/// 还没装主题时：代码不上色。
struct Plain;

impl HighlightStyleResolver for Plain {
    fn style(&self, _: &str) -> Option<gpui::HighlightStyle> {
        None
    }
}
