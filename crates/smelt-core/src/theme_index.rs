//! 内置主题目录的名字、明暗和族。
//!
//! 完整配色在 `smelt-ui` 的主题文件里。这里只留索引，让不引 GPUI 的设置写入
//! （CLI、Control API）也能把 `appearance.theme_mode` 拨到同一族的另一侧。

use std::sync::LazyLock;

use serde::Deserialize;

/// gpui-kit 自带的默认主题。`gpui_component::init` 装进注册表的就是这一对。
pub const DEFAULT_DARK: &str = "Default Dark";
pub const DEFAULT_LIGHT: &str = "Default Light";

#[derive(Clone, Debug, Deserialize)]
pub struct ThemeEntry {
    pub name: String,
    pub mode: String,
    pub family: String,
}

static INDEX: LazyLock<Vec<ThemeEntry>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../smelt-ui/themes/index.json"))
        .expect("themes/index.json")
});

pub fn entries() -> &'static [ThemeEntry] {
    INDEX.as_slice()
}

pub fn find(name: &str) -> Option<&'static ThemeEntry> {
    entries().iter().find(|entry| entry.name == name)
}

pub fn default_for_mode(mode: &str) -> &'static str {
    if mode == "light" {
        DEFAULT_LIGHT
    } else {
        DEFAULT_DARK
    }
}

/// 把当前主题拨到 `mode`。同一族里有对应明暗就用它，否则回到库的 Default。
/// 一族里同一明暗有多份时，用目录里的最后一份。
pub fn sibling(current: &str, mode: &str) -> String {
    let mode = if mode == "light" { "light" } else { "dark" };
    let Some(entry) = find(current) else {
        return default_for_mode(mode).to_string();
    };
    if entry.mode == mode {
        return entry.name.clone();
    }
    entries()
        .iter()
        .rev()
        .find(|other| other.family == entry.family && other.mode == mode)
        .map(|other| other.name.clone())
        .unwrap_or_else(|| default_for_mode(mode).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_the_builtin_pair() {
        assert_eq!(sibling("Default Dark", "light"), DEFAULT_LIGHT);
        assert_eq!(sibling("Default Light", "dark"), DEFAULT_DARK);
        assert_eq!(sibling("Default Dark", "dark"), DEFAULT_DARK);
        assert_eq!(sibling("missing", "light"), DEFAULT_LIGHT);
        assert_eq!(sibling("Sand Dark", "light"), DEFAULT_LIGHT);
    }

    #[test]
    fn ayu_switches_within_its_family() {
        assert_eq!(sibling("Ayu Dark", "light"), "Ayu Light");
        assert_eq!(sibling("Ayu Light", "dark"), "Ayu Dark");
    }

    #[test]
    fn catppuccin_light_falls_back_inside_the_family() {
        let name = sibling("Catppuccin Latte", "dark");
        let entry = find(&name).unwrap();
        assert_eq!(entry.family, "Catppuccin");
        assert_eq!(entry.mode, "dark");
    }
}
