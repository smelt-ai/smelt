//! 设置页：键盘快捷键。登记过的命令按来源分组列出来，点键帽录新键；视图里面改不了的键
//! 只列出来。改键的规则见 `crate::keymap`。

use super::*;
use crate::keymap::{self, Bound, KeymapState};

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
            SettingItem::render(move |_, _window, cx| {
                let state = cx.global::<KeymapState>();
                let os = cx.platform_info().os;
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

                // 按命令登记的先后分组，组名是来源给的。
                let mut groups: Vec<(SharedString, Vec<gpui::AnyElement>)> = Vec::new();
                for command in state.bound() {
                    let row = editable_row(command, state, os, colors);
                    match groups
                        .iter_mut()
                        .find(|(group, _)| *group == command.info.group)
                    {
                        Some((_, rows)) => rows.push(row),
                        None => groups.push((command.info.group.clone(), vec![row])),
                    }
                }
                let view_keys = vec![
                    fixed("终端内搜索", &["cmd-f"]),
                    fixed("关闭预览或返回", &["escape"]),
                    fixed("终端补全 / 反向补全", &["tab", "shift-tab"]),
                ];

                v_flex()
                    .w_full()
                    .gap_6()
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.muted)
                            .child("点键帽录新的快捷键：Esc 取消，⌫ 清掉不用。要带 ⌘，或者用 F 键。"),
                    )
                    .children(
                        groups
                            .into_iter()
                            .map(|(title, rows)| section(title, rows, colors)),
                    )
                    .child(section("视图里的按键".into(), view_keys, colors))
                    .into_any_element()
            })
            .keywords(["快捷键", "键盘", "改键", "shortcut", "keyboard", "hotkey", "keybinding"]),
        ),
    )
}

fn section(title: SharedString, rows: Vec<gpui::AnyElement>, colors: Palette) -> gpui::AnyElement {
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
fn editable_row(
    command: &Bound,
    state: &KeymapState,
    os: gpui::OperatingSystem,
    colors: Palette,
) -> gpui::AnyElement {
    let id = command.info.id.clone();
    let title = command.info.title.clone();
    let recording = state.recording.as_ref() == Some(&id);
    let overridden = command.overridden;
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
        let key = gpui::Keystroke::parse_with_os(&reassign.keystroke, os)
            .map(|stroke| keymap::display(&stroke))
            .unwrap_or_else(|_| reassign.keystroke.clone());
        let other = state
            .find(&reassign.other)
            .map_or(reassign.other.clone(), |other| other.info.title.clone());
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
        match command
            .key
            .as_deref()
            .and_then(|spec| gpui::Keystroke::parse_with_os(spec, os).ok())
        {
            Some(stroke) => kbd_cap(gpui_component::kbd::Kbd::new(stroke), colors.border, colors),
            None => keycap_text("未设置", colors.border, colors),
        }
    };
    let key = div()
        .id(format!("shortcut-key-{id}"))
        .role(gpui::Role::Button)
        .aria_label(format!("修改「{title}」的快捷键"))
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
                .child(div().text_sm().text_color(colors.fg).child(title))
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
                            .on_click(move |_, _, cx| keymap::reset(id.clone(), cx)),
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
