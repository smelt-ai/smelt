//! 键帽格式化：按平台拆成键帽上的字或拼成一行快捷键文本。
//! 键名按平台：macOS 用 ⌃⌥⌘⇧，别的平台写 Ctrl / Alt / Shift，Super 键是 ⊞ 或 ❖。

use gpui::{KeybindingKeystroke, Keystroke, OperatingSystem};

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
    fn label_joins_caps_per_platform() {
        let key = Keystroke::parse_with_os("cmd-shift-s", OperatingSystem::Mac).unwrap();
        assert_eq!(label(&key, OperatingSystem::Mac), "⌘⇧S");
        let key = Keystroke::parse_with_os("ctrl-shift-s", OperatingSystem::Linux).unwrap();
        assert_eq!(label(&key, OperatingSystem::Linux), "Ctrl+Shift+S");
    }
}
