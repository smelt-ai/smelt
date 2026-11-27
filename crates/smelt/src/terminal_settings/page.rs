//! 设置页「终端」：终端字号（`MIN_FONT_PX`–`MAX_FONT_PX`），渲染由 NSH 主壳承担。

use gpui::{App, SharedString};
use rust_i18n::t;
use serde_json::{Value, json};
use smelt_plugin_api::{SettingsEntry, SettingsGroup, ViewSpec, text};
use smelt_terminal::{MAX_FONT_PX, MIN_FONT_PX, font_px, set_font_px};
use smelt_ui::IconName;

use super::TerminalStoreGlobal;

pub(crate) fn spec() -> ViewSpec {
    ViewSpec {
        name: "settings".into(),
        title: text!("terminal.title"),
        icon: IconName::Terminal.path(),
        draws_top_bar: false,
        instances: false,
        settings: Some(SettingsEntry {
            group: SettingsGroup::General,
            keywords: ["terminal", "终端", "字号", "字体大小", "font size"]
                .into_iter()
                .map(SharedString::from)
                .collect(),
        }),
        fills: Vec::new(),
        build: crate::host_views::nsh_managed_view_builder(),
    }
}

pub(crate) fn snapshot() -> Value {
    let current = font_px() as u32;
    json!({
        "kind": "terminal",
        "section_title": t!("terminal.settings.display").into_owned(),
        "row_title": t!("terminal.settings.font_size").into_owned(),
        "row_description": format!("{MIN_FONT_PX}–{MAX_FONT_PX}px"),
        "current": current,
        "min": MIN_FONT_PX,
        "max": MAX_FONT_PX,
        "smaller_label": t!("terminal.settings.smaller").into_owned(),
        "larger_label": t!("terminal.settings.larger").into_owned(),
    })
}

pub(crate) fn handle_action(action: &Value, cx: &mut App) {
    if action["kind"].as_str() != Some("set_font_px") {
        return;
    }
    let Some(px) = action["px"].as_u64() else {
        return;
    };
    let px = (px as u32).clamp(MIN_FONT_PX, MAX_FONT_PX);
    if px == font_px() as u32 {
        return;
    }
    set_font_px(px);
    if let Some(store) = cx
        .try_global::<TerminalStoreGlobal>()
        .map(|global| global.0.clone())
    {
        store.save_font_px(px);
    }
    cx.refresh_windows();
}
