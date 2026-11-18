//! 设置页「终端」：终端字号。代码字体是终端、diff、代码块共用的，留在外观里。

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{App, Context, SharedString, Window, div, px, rgb};
use rust_i18n::t;
use smelt_plugin_api::ui::{Button, SettingsPage, SettingsRow, SettingsSection, ui_theme};
use smelt_plugin_api::{
    self as api, PluginView, SettingsEntry, SettingsGroup, ViewHandle, ViewSpec,
};

use crate::store::TerminalStore;
use crate::terminal_view::{MAX_FONT_PX, MIN_FONT_PX, font_px, set_font_px};
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

pub(crate) fn spec(store: Rc<TerminalStore>) -> ViewSpec {
    ViewSpec {
        name: "settings".into(),
        title: api::text!("title"),
        icon: crate::ICON.into(),
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
        build: Rc::new(move |_, cx| {
            let store = store.clone();
            Rc::new(cx.new(|_| TerminalSettings { store })) as Rc<dyn ViewHandle>
        }),
    }
}

struct TerminalSettings {
    store: Rc<TerminalStore>,
}

impl PluginView for TerminalSettings {
    fn open(&mut self, _: &api::serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}

/// 改终端字号：开着的终端下一帧按新字号重排，存进本插件的库。
fn change_font_px(store: &TerminalStore, px: u32, cx: &mut App) {
    let px = px.clamp(MIN_FONT_PX, MAX_FONT_PX);
    if px == font_px() as u32 {
        return;
    }
    set_font_px(px);
    store.save_font_px(px);
    cx.refresh_windows();
}

impl Render for TerminalSettings {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let current = font_px() as u32;
        let smaller = self.store.clone();
        let larger = self.store.clone();
        let size = SettingsRow::new(t!("settings.font_size"))
            .description(format!("{MIN_FONT_PX}–{MAX_FONT_PX}px"))
            .child(
                Button::new("terminal-font-smaller")
                    .small()
                    .label("−")
                    .accessibility_label(t!("settings.smaller"))
                    .disabled(current <= MIN_FONT_PX)
                    .on_click(move |_, _, cx| {
                        change_font_px(&smaller, current.saturating_sub(1), cx)
                    }),
            )
            .child(
                div()
                    .min_w(px(40.))
                    .text_center()
                    .type_style(TYPE.ui)
                    .text_color(rgb(ui_theme::text()))
                    .font_features(ui_theme::tabular_figures())
                    .child(format!("{current}px")),
            )
            .child(
                Button::new("terminal-font-larger")
                    .small()
                    .label("+")
                    .accessibility_label(t!("settings.larger"))
                    .disabled(current >= MAX_FONT_PX)
                    .on_click(move |_, _, cx| change_font_px(&larger, current + 1, cx)),
            );
        SettingsPage::new("terminal-settings").child(
            SettingsSection::new()
                .title(t!("settings.display"))
                .child(size),
        )
    }
}
