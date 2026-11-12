//! 主题目录条目（名字、明暗、族）和在一份目录里查找、换明暗的规则。
//!
//! 外观校验不认死内置目录：桌面把当前注册表（内置 + 插件主题）整理成
//! `&[ThemeEntry]` 传进 [`find_in`] / [`sibling_in`]。

use serde::Deserialize;

/// gpui-kit 自带的默认主题。`gpui_component::init` 装进注册表的就是这一对。
pub const DEFAULT_DARK: &str = "Default Dark";
pub const DEFAULT_LIGHT: &str = "Default Light";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ThemeEntry {
    pub name: String,
    pub mode: String,
    pub family: String,
}

impl ThemeEntry {
    pub fn new(
        name: impl Into<String>,
        mode: impl Into<String>,
        family: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            mode: mode.into(),
            family: family.into(),
        }
    }
}

/// 在给定目录里按名字精确查找。目录里重名时取最后一份（后注册的覆盖先注册的）。
pub fn find_in<'a>(catalog: &'a [ThemeEntry], name: &str) -> Option<&'a ThemeEntry> {
    catalog.iter().rev().find(|entry| entry.name == name)
}

pub fn default_for_mode(mode: &str) -> &'static str {
    if mode == "light" {
        DEFAULT_LIGHT
    } else {
        DEFAULT_DARK
    }
}

/// 在给定目录里把当前主题拨到 `mode`。同一族里有对应明暗就用它，否则回到库的
/// Default。一族里同一明暗有多份时，用目录里的最后一份。当前主题不在目录里时
/// 也回到 Default。
pub fn sibling_in(catalog: &[ThemeEntry], current: &str, mode: &str) -> String {
    let mode = if mode == "light" { "light" } else { "dark" };
    let Some(entry) = find_in(catalog, current) else {
        return default_for_mode(mode).to_string();
    };
    if entry.mode == mode {
        return entry.name.clone();
    }
    catalog
        .iter()
        .rev()
        .find(|other| other.family == entry.family && other.mode == mode)
        .map(|other| other.name.clone())
        .unwrap_or_else(|| default_for_mode(mode).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Vec<ThemeEntry> {
        vec![
            ThemeEntry::new(DEFAULT_DARK, "dark", "Default"),
            ThemeEntry::new(DEFAULT_LIGHT, "light", "Default"),
            ThemeEntry::new("Ayu Dark", "dark", "Ayu"),
            ThemeEntry::new("Ayu Light", "light", "Ayu"),
            ThemeEntry::new("Catppuccin Latte", "light", "Catppuccin"),
            ThemeEntry::new("Catppuccin Mocha", "dark", "Catppuccin"),
            ThemeEntry::new("Sand Dark", "dark", "Sand"),
        ]
    }

    fn sibling(current: &str, mode: &str) -> String {
        sibling_in(&catalog(), current, mode)
    }

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
        let entries = catalog();
        let entry = find_in(&entries, &name).unwrap();
        assert_eq!(entry.family, "Catppuccin");
        assert_eq!(entry.mode, "dark");
    }

    #[test]
    fn dynamic_catalog_resolves_plugin_themes() {
        let catalog = [
            ThemeEntry::new("Plug Night", "dark", "Plug"),
            ThemeEntry::new("Plug Day", "light", "Plug"),
            ThemeEntry::new("Solo", "dark", "Solo"),
        ];
        assert_eq!(find_in(&catalog, "Plug Day").unwrap().mode, "light");
        assert!(find_in(&catalog, "Ayu Dark").is_none());
        assert_eq!(sibling_in(&catalog, "Plug Night", "light"), "Plug Day");
        assert_eq!(sibling_in(&catalog, "Solo", "light"), DEFAULT_LIGHT);
        assert_eq!(sibling_in(&catalog, "Ayu Dark", "light"), DEFAULT_LIGHT);
    }

    #[test]
    fn later_duplicate_name_wins() {
        let catalog = [
            ThemeEntry::new("X", "dark", "A"),
            ThemeEntry::new("X", "light", "B"),
        ];
        assert_eq!(find_in(&catalog, "X").unwrap().family, "B");
    }
}
