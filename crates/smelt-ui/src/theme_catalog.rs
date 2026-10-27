//! 把内置主题文件装进 gpui-kit 的 [`ThemeRegistry`]，再按名字 `apply_config`。
//!
//! 库启动时只带 Default Light / Default Dark。Ayu、Catppuccin 等在这里的
//! `themes/` 里，随二进制嵌入，不依赖进程的当前目录。Default 不在这些文件里，
//! `gpui_component::init` 已经装好。

use std::collections::HashMap;
use std::sync::LazyLock;

use gpui::SharedString;
use gpui_component::{Theme, ThemeRegistry};

use crate::theme_files::THEME_FILES;
use crate::ui_theme;

/// 设置页下拉的 (值, 标签)。值是主题文件里的 `name`，例如 `Ayu Light`。
pub fn options() -> Vec<(SharedString, SharedString)> {
    smelt_model::theme_index::entries()
        .iter()
        .map(|entry| {
            (
                SharedString::from(entry.name.clone()),
                SharedString::from(entry.name.clone()),
            )
        })
        .collect()
}

/// 一款主题的四个代表色：背景、前景、强调（primary / accent）、终端底（secondary）。
///
/// 设置页用它们画色点，不用先把主题装进 [`ThemeRegistry`]。色值来自主题文件的
/// `colors`；Default 不在这些文件里，按 gpui-kit 默认主题的同名 token 解析。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeSwatch {
    pub background: u32,
    pub foreground: u32,
    pub accent: u32,
    pub terminal: u32,
}

impl ThemeSwatch {
    pub fn colors(self) -> [u32; 4] {
        [self.background, self.foreground, self.accent, self.terminal]
    }
}

/// 按主题名取四色。名字不在目录里时返回 `None`。
pub fn swatch(name: &str) -> Option<ThemeSwatch> {
    SWATCHES.get(name).copied()
}

static SWATCHES: LazyLock<HashMap<String, ThemeSwatch>> = LazyLock::new(load_swatches);

fn load_swatches() -> HashMap<String, ThemeSwatch> {
    let mut swatches = HashMap::new();
    for file in THEME_FILES {
        let parsed: ThemeFile = serde_json::from_str(file).expect("内置主题文件必须能解析出色点");
        for theme in parsed.themes {
            if let Some(swatch) = swatch_from_colors(&theme.colors) {
                swatches.insert(theme.name, swatch);
            }
        }
    }
    // gpui-kit 自带的 Default 用命名色（neutral-950 / white），不在 themes/*.json 里。
    swatches.insert(
        "Default Dark".to_string(),
        ThemeSwatch {
            background: named_color("neutral-950").unwrap(),
            foreground: named_color("neutral-50").unwrap(),
            accent: named_color("neutral-50").unwrap(),
            terminal: named_color("neutral-800").unwrap(),
        },
    );
    swatches.insert(
        "Default Light".to_string(),
        ThemeSwatch {
            background: named_color("white").unwrap(),
            foreground: named_color("neutral-950").unwrap(),
            accent: named_color("neutral-900").unwrap(),
            terminal: named_color("neutral-200").unwrap(),
        },
    );
    swatches
}

#[derive(serde::Deserialize)]
struct ThemeFile {
    themes: Vec<ThemeBody>,
}

#[derive(serde::Deserialize)]
struct ThemeBody {
    name: String,
    colors: HashMap<String, String>,
}

fn swatch_from_colors(colors: &HashMap<String, String>) -> Option<ThemeSwatch> {
    let background = color_value(colors.get("background")?)?;
    let foreground = color_value(colors.get("foreground")?)?;
    let accent = [
        "primary.background",
        "accent.background",
        "ring",
        "base.blue",
    ]
    .iter()
    .find_map(|key| colors.get(*key).and_then(|value| color_value(value)))?;
    let terminal = ["secondary.background", "popover.background", "background"]
        .iter()
        .find_map(|key| colors.get(*key).and_then(|value| color_value(value)))?;
    Some(ThemeSwatch {
        background,
        foreground,
        accent,
        terminal,
    })
}

/// `#RRGGBB`、`#RRGGBBAA`，或主题文件里的 `linear-gradient(..., #1E293B, ...)`。
/// 没有十六进制时再认 gpui-kit Default 用的命名色。
fn color_value(raw: &str) -> Option<u32> {
    if let Some(hex) = first_hex(raw) {
        return Some(hex);
    }
    let name = raw.split('/').next().unwrap_or(raw).trim();
    named_color(name)
}

fn first_hex(raw: &str) -> Option<u32> {
    let rest = raw.split_once('#')?.1;
    let hex: String = rest
        .chars()
        .take_while(|ch| ch.is_ascii_hexdigit())
        .collect();
    let rgb = match hex.len() {
        3 => {
            let chars = hex.chars().collect::<Vec<_>>();
            format!("{0}{0}{1}{1}{2}{2}", chars[0], chars[1], chars[2])
        }
        6 | 8 => hex[..6].to_string(),
        _ => return None,
    };
    u32::from_str_radix(&rgb, 16).ok()
}

fn named_color(name: &str) -> Option<u32> {
    Some(match name {
        "white" => 0xffffff,
        "black" => 0x000000,
        "neutral-50" => 0xfafafa,
        "neutral-100" => 0xf5f5f5,
        "neutral-200" => 0xe5e5e5,
        "neutral-300" => 0xd4d4d4,
        "neutral-400" => 0xa3a3a3,
        "neutral-500" => 0x737373,
        "neutral-600" => 0x525252,
        "neutral-700" => 0x404040,
        "neutral-800" => 0x262626,
        "neutral-900" => 0x171717,
        "neutral-950" => 0x0a0a0a,
        _ => return None,
    })
}

/// 按名字装上主题，产品色跟着换成这份主题。名字不在目录里时返回 false。
pub fn apply(name: &str, cx: &mut gpui::App) -> bool {
    if !cx.has_global::<ThemeRegistry>() || !cx.has_global::<Theme>() {
        return false;
    }
    ensure_loaded(cx);
    let key = SharedString::from(name);
    let Some(config) = ThemeRegistry::global(cx).themes().get(&key).cloned() else {
        return false;
    };
    Theme::update(cx, |theme| {
        // 换主题是整份换上，不是在上一份上面改漏掉的字段。先回到库的默认
        // 圆角和阴影，主题文件写了的再盖上去。字号字体属于 Appearance。
        let font_family = theme.font_family.clone();
        let font_size = theme.font_size;
        let mono_family = theme.mono_font_family.clone();
        let mono_size = theme.mono_font_size;
        let scrollbar_mode = theme.scrollbar_mode;
        theme.radius = gpui::px(6.);
        theme.radius_lg = gpui::px(8.);
        theme.shadow = true;
        theme.apply_config(&config);
        theme.font_family = font_family;
        theme.font_size = font_size;
        theme.mono_font_family = mono_family;
        theme.mono_font_size = mono_size;
        theme.scrollbar_mode = scrollbar_mode;
        // 通知固定在右下角，避开左侧会话列表。主题文件不带这个位置。
        theme.notification.placement = gpui::Anchor::BottomRight;
    });
    ui_theme::adopt_active_theme(cx);
    true
}

fn ensure_loaded(cx: &mut gpui::App) {
    // Default Dark 启动时就在注册表里，不能拿它当「目录已装完」的标记。
    let loaded = SharedString::from("Ayu Dark");
    if ThemeRegistry::global(cx).themes().contains_key(&loaded) {
        return;
    }
    // 必须和 `gpui_component::init` 同一次 App::update 里装完。注册表观察者
    // 要等这次 update 末尾才激活。拖到后面再装，它会 `Theme::change`，
    // 用主题文件盖掉刚刚写上的字号。
    for file in THEME_FILES {
        if let Err(err) = ThemeRegistry::global_mut(cx).load_themes_from_str(file) {
            eprintln!("[theme] 加载主题失败: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_indexed_theme_has_four_swatch_colors() {
        let entries = smelt_model::theme_index::entries();
        assert_eq!(entries.len(), 38);
        for entry in entries {
            let swatch = swatch(&entry.name).unwrap_or_else(|| {
                panic!("{} 缺少四色预览", entry.name);
            });
            assert_ne!(
                swatch.background, swatch.foreground,
                "{} 的背景和前景不应是同一个色",
                entry.name
            );
        }
    }

    #[test]
    fn packaged_themes_use_background_foreground_primary_and_secondary() {
        assert_eq!(
            swatch("Ayu Dark").unwrap().colors(),
            [0x0d1016, 0xb3b1ad, 0x5ac1fe, 0x1f2127]
        );
        assert_eq!(
            swatch("Catppuccin Frappe").unwrap().colors(),
            [0x232634, 0xc6d0f5, 0x8caaee, 0x414559]
        );
        // Aurora 的 primary 是渐变，取第一段实色。
        assert_eq!(swatch("Aurora Light").unwrap().accent, 0x1e293b);
    }

    #[test]
    fn default_themes_resolve_named_palette_tokens() {
        assert_eq!(
            swatch("Default Dark").unwrap().colors(),
            [0x0a0a0a, 0xfafafa, 0xfafafa, 0x262626]
        );
        assert_eq!(
            swatch("Default Light").unwrap().colors(),
            [0xffffff, 0x0a0a0a, 0x171717, 0xe5e5e5]
        );
    }
}
