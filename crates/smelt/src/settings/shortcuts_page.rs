//! 设置页：键盘快捷键。登记过的命令按来源分组列出来，点键帽录新键；视图里面改不了的键
//! 只列出来。改键的规则见 `crate::keymap`。宿主自己的页，和插件的设置页一样是登记的视图
//! （见 [`super::view_specs`]）。

use std::borrow::Cow;

use super::*;
use rust_i18n::t;
use smelt_ui::kit::{TYPE, TypeStyled as _};
use smelt_ui::{Keybind, SettingsPage, SettingsSection as Section};

use crate::keymap::{self, Bound, KeymapState};
use crate::ui_theme;

/// 快捷键页：画的全是 [`KeymapState`] 里的，它一变就重画。
pub(super) struct ShortcutsPage {
    _keymap: Subscription,
}

impl ShortcutsPage {
    pub(super) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            _keymap: cx.observe_global::<KeymapState>(|_, cx| cx.notify()),
        }
    }
}

impl Render for ShortcutsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        shortcuts_page(cx)
    }
}

impl smelt_plugin_api::PluginView for ShortcutsPage {
    fn open(&mut self, _: &serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}

fn shortcuts_page(cx: &App) -> SettingsPage {
    let state = cx.global::<KeymapState>();
    let os = cx.platform_info().os;
    let fixed = |label: Cow<'static, str>, keys: &[&'static str]| {
        row(label, keys.iter().copied().map(fixed_key).collect())
    };

    // 按命令登记的先后分组，组名是来源给的。
    let mut groups: Vec<(smelt_plugin_api::Text, Vec<gpui::AnyElement>)> = Vec::new();
    for command in state.bound() {
        let row = editable_row(command, state, os);
        match groups
            .iter_mut()
            .find(|(group, _)| *group == command.info.group)
        {
            Some((_, rows)) => rows.push(row),
            None => groups.push((command.info.group.clone(), vec![row])),
        }
    }
    let view_keys = vec![
        fixed(t!("shortcuts.terminal_search"), &["cmd-f"]),
        fixed(t!("shortcuts.close_or_back"), &["escape"]),
        fixed(t!("shortcuts.terminal_complete"), &["tab", "shift-tab"]),
    ];

    SettingsPage::new("settings-page:shortcuts")
        .child(
            div()
                .px_4()
                .type_style(TYPE.ui)
                .text_color(rgb(ui_theme::muted()))
                .child(t!("shortcuts.hint")),
        )
        .children(
            groups
                .into_iter()
                .map(|(title, rows)| Section::new().title(title.get()).children(rows)),
        )
        .child(
            Section::new()
                .title(t!("shortcuts.view_keys"))
                .children(view_keys),
        )
}

/// 只列出来、改不了的一行。
fn row(label: Cow<'static, str>, keys: Vec<gpui::AnyElement>) -> gpui::AnyElement {
    div()
        .flex()
        .w_full()
        .min_h(px(28.))
        .justify_between()
        .items_center()
        .gap_4()
        .child(
            div()
                .type_style(TYPE.ui)
                .text_color(rgb(ui_theme::text()))
                .child(SharedString::from(label)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .flex_none()
                .gap_1()
                .children(keys),
        )
        .into_any_element()
}

/// 能改的一行：键帽点了开始录，再点一次不录了；改过的旁边有「恢复默认」。录键时、没改成时、
/// 撞上别的快捷键等确认改绑时，说明写在名字下面。
fn editable_row(
    command: &Bound,
    state: &KeymapState,
    os: gpui::OperatingSystem,
) -> gpui::AnyElement {
    let id = command.info.id.clone();
    let title = command.info.title.get();
    let recording = state.recording.as_ref() == Some(&id);
    let overridden = command.overridden;
    let error = state
        .error
        .as_ref()
        .filter(|(failed, _)| *failed == id)
        .map(|(_, reason)| reason.clone());
    let reassign = state.reassign.as_ref().filter(|reassign| reassign.id == id);
    let note: Option<(String, u32)> = if recording {
        Some((
            t!("shortcuts.recording_note").into_owned(),
            ui_theme::muted(),
        ))
    } else if let Some(reassign) = reassign {
        let key = gpui::Keystroke::parse_with_os(&reassign.keystroke, os)
            .map(|stroke| smelt_ui::keybind::label(&stroke, os))
            .unwrap_or_else(|_| reassign.keystroke.clone());
        let other = state
            .find(&reassign.other)
            .map_or(reassign.other.clone(), |other| other.info.title.get());
        Some((
            t!("shortcuts.taken", key = key, other = other).into_owned(),
            ui_theme::bad(),
        ))
    } else {
        error.map(|reason| (reason, ui_theme::bad()))
    };
    let reassigning = reassign.is_some();

    let key: gpui::AnyElement = if recording {
        keycap_text(t!("shortcuts.press"), ui_theme::focus())
    } else {
        match command
            .key
            .as_deref()
            .and_then(|spec| gpui::Keystroke::parse_with_os(spec, os).ok())
        {
            Some(stroke) => Keybind::new([stroke]).into_any_element(),
            None => keycap_text(t!("shortcuts.unset"), ui_theme::hair()),
        }
    };
    let key = div()
        .id(format!("shortcut-key-{id}"))
        .role(gpui::Role::Button)
        .aria_label(t!("shortcuts.change", title = title).into_owned())
        .rounded_md()
        .hover(|style| style.opacity(0.8))
        .child(key)
        .on_click({
            let id = id.clone();
            move |_, window, cx| {
                if recording {
                    keymap::stop_recording(cx);
                } else {
                    keymap::start_recording(id.clone(), window, cx);
                }
            }
        });

    div()
        .flex()
        .w_full()
        .min_h(px(28.))
        .justify_between()
        .items_center()
        .gap_4()
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .type_style(TYPE.ui)
                        .text_color(rgb(ui_theme::text()))
                        .child(title),
                )
                .children(note.map(|(text, color)| {
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(rgb(color))
                        .child(text)
                })),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .gap_2()
                .items_center()
                .when(reassigning, |keys| {
                    keys.child(
                        Button::new(format!("shortcut-reassign-cancel-{id}"))
                            .quiet()
                            .small()
                            .label(t!("action.never_mind"))
                            .on_click(|_, _, cx| keymap::cancel_reassign(cx)),
                    )
                    .child(
                        Button::new(format!("shortcut-reassign-{id}"))
                            .small()
                            .label(t!("shortcuts.reassign"))
                            .on_click(|_, _, cx| keymap::confirm_reassign(cx)),
                    )
                })
                .when(overridden && !recording && !reassigning, |keys| {
                    keys.child(
                        Button::new(format!("shortcut-reset-{id}"))
                            .quiet()
                            .small()
                            .label(t!("shortcuts.reset"))
                            .on_click(move |_, _, cx| keymap::reset(id.clone(), cx)),
                    )
                })
                .child(key),
        )
        .into_any_element()
}

/// 没有键可画时（正在录、未设置、固定键位写错）和键帽一样大的文字框。
fn keycap_text(text: impl Into<SharedString>, border: Rgba) -> gpui::AnyElement {
    div()
        .min_w(px(34.))
        .h(ui_theme::height::xs())
        .px_2()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(rgb(ui_theme::surface()))
        .type_style(TYPE.text_xsmall)
        .text_color(rgb(ui_theme::muted()))
        .child(text.into())
        .into_any_element()
}

/// 固定键位按 gpui 键位写法（`cmd-f`）解析，按平台显示键名。
/// 写错解析不了时原样写出来，不留空键帽。
fn fixed_key(spec: &'static str) -> gpui::AnyElement {
    match gpui::Keystroke::parse(spec) {
        Ok(stroke) => Keybind::new([stroke]).into_any_element(),
        Err(_) => keycap_text(spec, ui_theme::hair()),
    }
}
