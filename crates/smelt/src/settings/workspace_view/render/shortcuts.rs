//! 设置页：键盘快捷键。能改的点键帽录新键，改不了的只列出来。改键的规则见 `crate::keymap`。

use super::*;
use crate::keymap::{self, KeymapState, SHORTCUTS, Shortcut, ShortcutGroup};

#[derive(Clone, Copy)]
struct Palette {
    fg: Hsla,
    muted: Hsla,
    border: Hsla,
    popover: Hsla,
    primary: Hsla,
    danger: Hsla,
}

pub(super) fn shortcuts_page(
    _entity: Entity<Workspace>,
    _snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    let colors = {
        let t = cx.theme();
        Palette {
            fg: t.foreground,
            muted: t.muted_foreground,
            border: t.border,
            popover: t.popover,
            primary: t.primary,
            danger: t.danger,
        }
    };

    SettingPage::new("快捷键").resettable(false).group(
        SettingGroup::new().item(
            SettingItem::render(move |_, window, cx| {
                let state = cx.global::<KeymapState>();
                let editable = |group: ShortcutGroup| -> Vec<gpui::AnyElement> {
                    SHORTCUTS
                        .iter()
                        .filter(|shortcut| shortcut.group == group)
                        .map(|shortcut| editable_row(shortcut, state, colors))
                        .collect()
                };
                let fixed = |label: &'static str, keys: &[&'static str]| {
                    row(
                        label,
                        keys.iter()
                            .copied()
                            .map(|key| fixed_key(key, colors))
                            .collect(),
                        colors,
                    )
                };

                let mut global = editable(ShortcutGroup::Global);
                global.push(row(
                    "退出 Smelt",
                    vec![bound_key(window, &crate::Quit, None, "⌘Q", colors)],
                    colors,
                ));
                let mut session = editable(ShortcutGroup::Session);
                session.push(row(
                    "切换到第 1–9 个会话",
                    (0..9)
                        .map(|index| {
                            bound_key(
                                window,
                                &crate::ActivateSession { index },
                                Some("Workspace"),
                                SESSION_KEY_FALLBACK[index],
                                colors,
                            )
                        })
                        .collect(),
                    colors,
                ));
                let mut edit = editable(ShortcutGroup::Edit);
                edit.extend([
                    fixed("终端内搜索", &["cmd-f"]),
                    fixed("关闭预览或返回", &["escape"]),
                    fixed("终端补全 / 反向补全", &["tab", "shift-tab"]),
                ]);

                v_flex()
                    .w_full()
                    .gap_6()
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.muted)
                            .child("点键帽录新的快捷键：Esc 取消，⌫ 清掉不用。要带 ⌘，或者用 F 键。"),
                    )
                    .child(section("全局", global, colors))
                    .child(section("会话与面板", session, colors))
                    .child(section("编辑与导航", edit, colors))
                    .into_any_element()
            })
            .keywords(["快捷键", "键盘", "改键", "shortcut", "keyboard", "hotkey", "keybinding"]),
        ),
    )
}

const SESSION_KEY_FALLBACK: [&str; 9] = ["⌘1", "⌘2", "⌘3", "⌘4", "⌘5", "⌘6", "⌘7", "⌘8", "⌘9"];

fn section(title: &'static str, rows: Vec<gpui::AnyElement>, colors: Palette) -> gpui::AnyElement {
    v_flex()
        .w_full()
        .gap_1()
        .child(
            div()
                .pb_1()
                .text_xs()
                .font_semibold()
                .text_color(colors.muted)
                .child(title),
        )
        .children(rows)
        .into_any_element()
}

fn row(label: &'static str, keys: Vec<gpui::AnyElement>, colors: Palette) -> gpui::AnyElement {
    h_flex()
        .w_full()
        .min_h(px(38.))
        .justify_between()
        .items_center()
        .gap_4()
        .border_b_1()
        .border_color(colors.border)
        .child(div().text_sm().text_color(colors.fg).child(label))
        .child(h_flex().flex_none().gap_1().children(keys))
        .into_any_element()
}

/// 能改的一行：键帽点了开始录，再点一次不录了；改过的旁边有「恢复默认」。录键时、没改成时、
/// 撞上别的快捷键等确认改绑时，说明写在名字下面。
fn editable_row(shortcut: &'static Shortcut, state: &KeymapState, colors: Palette) -> gpui::AnyElement {
    let id = shortcut.id;
    let recording = state.recording == Some(id);
    let overridden = state.overrides().contains_key(id);
    let error = state
        .error
        .as_ref()
        .filter(|(failed, _)| *failed == id)
        .map(|(_, reason)| reason.clone());
    let reassign = state.reassign.as_ref().filter(|reassign| reassign.id == id);
    let note: Option<(String, Hsla)> = if recording {
        Some((
            "按了没反应的话，这个键可能被系统或别的应用占用了，比如飞书的全局快捷键".to_string(),
            colors.muted,
        ))
    } else if let Some(reassign) = reassign {
        let key = gpui::Keystroke::parse(&reassign.keystroke)
            .map(|stroke| keymap::display(&stroke))
            .unwrap_or_else(|_| reassign.keystroke.clone());
        let other = keymap::shortcut(reassign.other).map_or(reassign.other, |other| other.label);
        Some((
            format!("{key} 已经是「{other}」的快捷键。改绑后「{other}」就没有快捷键了"),
            colors.danger,
        ))
    } else {
        error.map(|reason| (reason, colors.danger))
    };
    let reassigning = reassign.is_some();

    let key: gpui::AnyElement = if recording {
        keycap_text("按下新快捷键…", colors.primary, colors)
    } else {
        match shortcut
            .keystroke(state.overrides())
            .and_then(|spec| gpui::Keystroke::parse(spec).ok())
        {
            Some(stroke) => kbd_cap(gpui_component::kbd::Kbd::new(stroke), colors.border, colors),
            None => keycap_text("未设置", colors.border, colors),
        }
    };
    let key = div()
        .id(format!("shortcut-key-{id}"))
        .role(gpui::Role::Button)
        .aria_label(format!("修改「{}」的快捷键", shortcut.label))
        .rounded_md()
        .hover(|style| style.opacity(0.8))
        .child(key)
        .on_click(move |_, window, cx| {
            if recording {
                keymap::stop_recording(cx);
            } else {
                keymap::start_recording(id, window, cx);
            }
        });

    h_flex()
        .w_full()
        .min_h(px(38.))
        .py_1()
        .justify_between()
        .items_center()
        .gap_4()
        .border_b_1()
        .border_color(colors.border)
        .child(
            v_flex()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().text_color(colors.fg).child(shortcut.label))
                .children(note.map(|(text, color)| div().text_xs().text_color(color).child(text))),
        )
        .child(
            h_flex()
                .flex_none()
                .gap_2()
                .items_center()
                .when(reassigning, |keys| {
                    keys.child(
                        Button::new(format!("shortcut-reassign-cancel-{id}"))
                            .quiet()
                            .compact()
                            .label("算了")
                            .on_click(|_, _, cx| keymap::cancel_reassign(cx)),
                    )
                    .child(
                        Button::new(format!("shortcut-reassign-{id}"))
                            .compact()
                            .label("改绑")
                            .on_click(|_, _, cx| keymap::confirm_reassign(cx)),
                    )
                })
                .when(overridden && !recording && !reassigning, |keys| {
                    keys.child(
                        Button::new(format!("shortcut-reset-{id}"))
                            .quiet()
                            .compact()
                            .label("恢复默认")
                            .on_click(move |_, _, cx| keymap::reset(id, cx)),
                    )
                })
                .child(key),
        )
        .into_any_element()
}

fn keycap_text(text: &'static str, border: Hsla, colors: Palette) -> gpui::AnyElement {
    div()
        .min_w(px(34.))
        .h(crate::ui_theme::height::xs())
        .px_2()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(colors.popover)
        .text_xs()
        .text_color(colors.muted)
        .child(text)
        .into_any_element()
}

fn keycap(key: &'static str, colors: Palette) -> gpui::AnyElement {
    div()
        .min_w(px(34.))
        .h(crate::ui_theme::height::xs())
        .px_2()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(colors.border)
        .bg(colors.popover)
        .font_family(crate::terminal_view::font_family())
        .text_xs()
        .text_color(colors.fg)
        .child(key)
        .into_any_element()
}

/// 固定键位按 gpui 键位写法（`cmd-f`）交给 Kbd，按平台显示键名。
/// 写错解析不了时原样画出来，不留空键帽。
fn fixed_key(spec: &'static str, colors: Palette) -> gpui::AnyElement {
    match gpui::Keystroke::parse(spec) {
        Ok(stroke) => kbd_cap(gpui_component::kbd::Kbd::new(stroke), colors.border, colors),
        Err(_) => keycap(spec, colors),
    }
}

/// 读当前键位。`context` 用 Key Context 声明，不用 predicate。
/// 没有绑定时退回原来的字，避免键帽是空的。
fn bound_key(
    window: &Window,
    action: &dyn gpui::Action,
    context: Option<&str>,
    fallback: &'static str,
    colors: Palette,
) -> gpui::AnyElement {
    match gpui_component::kbd::Kbd::binding_for_action(action, context, window) {
        Some(kbd) => kbd_cap(kbd, colors.border, colors),
        None => keycap(fallback, colors),
    }
}

/// Kbd 套上和兜底键帽一样的外框，两种来源的键帽并排时看不出差别。
fn kbd_cap(kbd: gpui_component::kbd::Kbd, border: Hsla, colors: Palette) -> gpui::AnyElement {
    kbd.min_w(px(34.))
        .h(crate::ui_theme::height::xs())
        .px_2()
        .py_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(colors.popover)
        .font_family(crate::terminal_view::font_family())
        .text_xs()
        .text_color(colors.fg)
        .into_any_element()
}
