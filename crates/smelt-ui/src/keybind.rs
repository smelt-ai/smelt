//! 键帽：一组快捷键画成并排的小方块，修饰键各占一格：
//! 14 高、最窄 14、左右 4、`xs` 圆角，`soft` 底配 `muted` 字。多步的快捷键（`⌘K ⌘S`）
//! 步与步之间空 6，同一步的键之间空 2。
//!
//! 键名按平台：macOS 用 ⌃⌥⌘⇧，别的平台写 Ctrl / Alt / Shift，Super 键是 ⊞ 或 ❖。
//! 只要一行文字（拼在提示句子里）时用 [`label`]。

use std::sync::{Arc, LazyLock};

use gpui::{
    App, FontFeatures, IntoElement, KeybindingKeystroke, Keystroke, OperatingSystem, ParentElement,
    RenderOnce, Styled, Window, div, prelude::FluentBuilder as _, px, rgb,
};

use crate::kit::{RADIUS, TYPE};
use crate::ui_theme;

enum Steps {
    Keys(Vec<KeybindingKeystroke>),
    /// 已经按平台拆好的键名，提示里查键位表时用。
    Caps(Vec<Vec<String>>),
}

#[derive(IntoElement)]
pub struct Keybind {
    steps: Steps,
    backdrop: Option<u32>,
}

impl Keybind {
    /// 一步或多步的快捷键，按顺序画。
    pub fn new(keystrokes: impl IntoIterator<Item = Keystroke>) -> Self {
        Self {
            steps: Steps::Keys(
                keystrokes
                    .into_iter()
                    .map(KeybindingKeystroke::from_keystroke)
                    .collect(),
            ),
            backdrop: None,
        }
    }

    pub(crate) fn caps(steps: Vec<Vec<String>>) -> Self {
        Self {
            steps: Steps::Caps(steps),
            backdrop: None,
        }
    }

    /// 键帽下面的底色，默认 `bg`。压在浮层、卡片上时传那一层的底，键帽和字按它保对比。
    pub fn backdrop(mut self, color: u32) -> Self {
        self.backdrop = Some(color);
        self
    }
}

impl RenderOnce for Keybind {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let steps = match self.steps {
            Steps::Keys(keys) => {
                let os = cx.platform_info().os;
                keys.iter().map(|key| keycaps(key, os)).collect()
            }
            Steps::Caps(caps) => caps,
        };
        let (fill, fg) = colors(
            self.backdrop.unwrap_or_else(ui_theme::bg),
            ui_theme::soft(),
            ui_theme::muted(),
        );
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(6.))
            .map(|this| TYPE.small_medium.lh(11.).apply(this))
            .children(steps.into_iter().map(|keys| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .children(keys.into_iter().map(|key| {
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .min_w(px(14.))
                            .h(px(14.))
                            .px(px(4.))
                            .rounded(px(RADIUS.xs))
                            .bg(rgb(fill))
                            .text_color(rgb(fg))
                            .font_features(keycap_features())
                            .child(key)
                    }))
            }))
    }
}

/// 键帽底和字：底对 `backdrop` 至少 1.2:1 才看得出一格，字对底至少 4.5:1。
pub(crate) fn colors(backdrop: u32, fill: u32, fg: u32) -> (u32, u32) {
    let fill = ui_theme::legible(fill, backdrop, 1.2);
    (fill, ui_theme::legible(fg, fill, 4.5))
}

fn keycap_features() -> FontFeatures {
    static FEATURES: LazyLock<FontFeatures> =
        LazyLock::new(|| FontFeatures(Arc::new(vec![("tnum".into(), 1), ("lnum".into(), 1)])));
    FEATURES.clone()
}

/// 一步快捷键的一行文字：macOS 上键名连写（`⌘⇧S`），别的平台用 `+` 连（`Ctrl+Shift+S`）。
pub fn label(key: &Keystroke, os: OperatingSystem) -> String {
    let separator = if os == OperatingSystem::Mac { "" } else { "+" };
    keycaps(&KeybindingKeystroke::from_keystroke(key.clone()), os).join(separator)
}

/// 一步快捷键拆成键帽上的字，修饰键各一格。
///
/// 保留 GPUI 映射后的平台键名（包括 Windows 键盘布局），只把修饰键拆成独立键帽。
pub fn keycaps(key: &KeybindingKeystroke, os: OperatingSystem) -> Vec<String> {
    let modifiers = key.modifiers();
    let mut result = Vec::new();
    for (enabled, name) in [
        (modifiers.control, "control"),
        (modifiers.alt, "alt"),
        (modifiers.platform, "platform"),
        (modifiers.shift, "shift"),
        (modifiers.function, "fn"),
    ] {
        if enabled {
            let label = match (os, name) {
                (OperatingSystem::Mac, "control") => "⌃",
                (OperatingSystem::Mac, "alt") => "⌥",
                (OperatingSystem::Mac, "platform") => "⌘",
                (OperatingSystem::Mac, "shift") => "⇧",
                (OperatingSystem::Windows, "platform") => "⊞",
                (OperatingSystem::Linux | OperatingSystem::FreeBsd, "platform") => "❖",
                (_, "platform") => "Cmd",
                (_, "control") => "Ctrl",
                (_, "alt") => "Alt",
                (_, "shift") => "Shift",
                (_, _) => "Fn",
            };
            result.push(label.to_owned());
        }
    }
    // GPUI 将单独按下的修饰键记为 control/platform/function，而不是 ctrl/cmd/fn。
    // 除这些键外仍使用 GPUI 的 display，包括 Windows 映射后的键名。
    let label = match key.key() {
        "control" => Some(match os {
            OperatingSystem::Mac => "⌃",
            _ => "Ctrl",
        }),
        "platform" => Some(match os {
            OperatingSystem::Mac => "⌘",
            OperatingSystem::Windows => "⊞",
            OperatingSystem::Linux | OperatingSystem::FreeBsd => "❖",
            _ => "Cmd",
        }),
        "function" => Some("Fn"),
        "alt" if os != OperatingSystem::Mac => Some("Alt"),
        "shift" if os != OperatingSystem::Mac => Some("Shift"),
        _ => None,
    };
    result.push(label.map(str::to_owned).unwrap_or_else(|| {
        Keystroke {
            key: key.key().to_owned(),
            ..Default::default()
        }
        .display(os)
        .to_string()
    }));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::KeyBinding;

    gpui::actions!(keybind_tests, [Save, Sequence]);

    #[test]
    fn keycaps_keep_named_keys_and_modifiers_separate_on_each_platform() {
        let binding = KeyBinding::new("ctrl-alt-shift-enter", Save, None);
        let key = &binding.keystrokes()[0];
        assert_eq!(keycaps(key, OperatingSystem::Mac), ["⌃", "⌥", "⇧", "enter"]);
        assert_eq!(
            keycaps(key, OperatingSystem::Linux),
            ["Ctrl", "Alt", "Shift", "enter"]
        );
        let binding = KeyBinding::new("fn-f1", Save, None);
        assert_eq!(
            keycaps(&binding.keystrokes()[0], OperatingSystem::Mac),
            ["Fn", "f1"]
        );
    }

    #[test]
    fn standalone_modifiers_match_their_combination_labels_on_all_platforms() {
        for os in [
            OperatingSystem::Mac,
            OperatingSystem::Windows,
            OperatingSystem::Linux,
            OperatingSystem::FreeBsd,
            OperatingSystem::Unknown,
        ] {
            for modifier in ["ctrl", "alt", "shift", "cmd", "fn"] {
                let single = KeybindingKeystroke::from_keystroke(
                    Keystroke::parse_with_os(modifier, os).unwrap(),
                );
                let combined = KeybindingKeystroke::from_keystroke(
                    Keystroke::parse_with_os(&format!("{modifier}-k"), os).unwrap(),
                );
                assert_eq!(keycaps(&single, os), keycaps(&combined, os)[..1]);
            }
            let binding = KeyBinding::new("cmd-k ctrl-shift-s fn-f1", Sequence, None);
            let steps: Vec<_> = binding
                .keystrokes()
                .iter()
                .map(|key| keycaps(key, os))
                .collect();
            assert_eq!(steps.len(), 3);
            assert_eq!(steps[0][1], "K");
            assert_eq!(steps[1].last().unwrap(), "S");
            assert_eq!(steps[2], ["Fn", "f1"]);
        }
    }

    #[test]
    fn colors_keep_cap_visible_and_text_legible() {
        assert_eq!(colors(0xffffff, 0xdddddd, 0x333333), (0xdddddd, 0x333333));
        for (backdrop, muted) in [
            (0x101010, 0x101010),
            (0xfafafa, 0xfafafa),
            (0x777777, 0x777777),
        ] {
            let (fill, fg) = colors(backdrop, backdrop, muted);
            assert!(ui_theme::contrast_ratio(fill, backdrop) >= 1.2);
            assert!(ui_theme::contrast_ratio(fg, fill) >= 4.5);
        }
    }

    #[test]
    fn label_joins_caps_per_platform() {
        let key = Keystroke::parse_with_os("cmd-shift-s", OperatingSystem::Mac).unwrap();
        assert_eq!(label(&key, OperatingSystem::Mac), "⌘⇧S");
        let key = Keystroke::parse_with_os("ctrl-shift-s", OperatingSystem::Linux).unwrap();
        assert_eq!(label(&key, OperatingSystem::Linux), "Ctrl+Shift+S");
    }
}
