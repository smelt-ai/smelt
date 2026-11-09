//! 把内置主题文件装进 gpui-kit 的 [`ThemeRegistry`]，再按名字 `apply_config`。
//!
//! 库启动时只带 Default Light / Default Dark。Ayu、Catppuccin 等在这里的
//! `themes/` 里，随二进制嵌入，不依赖进程的当前目录。Default 不在这些文件里，
//! `gpui_component::init` 已经装好。
//!
//! 插件带的主题不进注册表：注册表同名先到先得、装进去就删不掉，插件改了配色重新加载、
//! 或者删掉插件，注册表里还是旧的。它们放在 [`PluginThemes`] 里，每次挂载插件整份换掉；
//! 按名字找时先找内置的，插件的主题不能顶替内置的。

use std::rc::Rc;

use gpui::{App, Global, Hsla, SharedString};
use gpui_component::{Theme, ThemeConfig, ThemeRegistry};

use crate::theme_files::THEME_FILES;
use crate::ui_theme;

/// 一款主题的代表色：背景、前景、强调（primary）。设置页拿它画色点。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeSwatch {
    pub background: Hsla,
    pub foreground: Hsla,
    pub accent: Hsla,
}

/// 插件带的一款主题。
#[derive(Clone, Debug)]
pub struct PluginTheme {
    pub config: Rc<ThemeConfig>,
    /// 设置页下拉里的系列名：带它的插件的名字。
    pub family: SharedString,
}

/// 眼下挂着的插件带的主题，按插件加载顺序。
#[derive(Default)]
struct PluginThemes(Vec<PluginTheme>);

impl Global for PluginThemes {}

/// 换上这一批插件的主题。只写内存；正用着的主题要不要重新套，调用方定。
pub fn set_plugin_themes(themes: Vec<PluginTheme>, cx: &mut App) {
    cx.set_global(PluginThemes(themes));
}

/// 插件带的主题，按插件加载顺序。
pub fn plugin_themes(cx: &App) -> &[PluginTheme] {
    cx.try_global::<PluginThemes>()
        .map_or(&[], |themes| themes.0.as_slice())
}

/// 按名字找主题配置：先内置的，再插件的。
fn config(name: &str, cx: &App) -> Option<Rc<ThemeConfig>> {
    let key = SharedString::from(name);
    ThemeRegistry::global(cx)
        .themes()
        .get(&key)
        .cloned()
        .or_else(|| {
            plugin_themes(cx)
                .iter()
                .find(|theme| theme.config.name == key)
                .map(|theme| theme.config.clone())
        })
}

/// 按名字取一款主题的代表色。颜色由 gpui-kit 自己解析主题配置得出，换上这款主题后
/// 界面就是这几个色；不另写一套解析。名字内置的和插件的都找不到时是 `None`。
///
/// 注册表在启动装主题时已经装满（见 [`apply`]），这里只读不装。
pub fn swatch(name: &str, cx: &App) -> Option<ThemeSwatch> {
    let config = config(name, cx)?;
    let mut theme = Theme::global(cx).clone();
    theme.apply_config(&config);
    Some(ThemeSwatch {
        background: theme.background,
        foreground: theme.foreground,
        accent: theme.primary,
    })
}

/// 按名字装上主题，产品色跟着换成这份主题。内置的和插件的都找不到时返回 false。
pub fn apply(name: &str, cx: &mut gpui::App) -> bool {
    if !cx.has_global::<ThemeRegistry>() || !cx.has_global::<Theme>() {
        return false;
    }
    ensure_loaded(cx);
    let Some(config) = config(name, cx) else {
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
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn every_indexed_theme_has_a_swatch(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            // 装主题顺带把整份目录装进注册表。
            assert!(apply("Default Dark", cx));
            for entry in smelt_model::theme_index::entries() {
                let swatch = swatch(&entry.name, cx)
                    .unwrap_or_else(|| panic!("{} 不在注册表里", entry.name));
                assert_ne!(
                    swatch.background, swatch.foreground,
                    "{} 的背景和前景不应是同一个色",
                    entry.name
                );
            }
            assert_eq!(swatch("没有这款主题", cx), None);
        });
    }

    fn plugin_theme(json: &str, family: &str) -> PluginTheme {
        PluginTheme {
            config: Rc::new(serde_json::from_str(json).unwrap()),
            family: family.into(),
        }
    }

    #[gpui::test]
    fn plugin_themes_apply_by_name_and_go_away_with_the_plugin(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let traveler = plugin_theme(
                r##"{"name": "Traveler", "mode": "light", "radius": 2, "colors": {"background": "#f4e4c1"}}"##,
                "旅人",
            );
            set_plugin_themes(vec![traveler], cx);
            assert!(apply("Traveler", cx));
            assert!(!cx.global::<Theme>().is_dark());
            assert_eq!(cx.global::<Theme>().radius, gpui::px(2.));
            assert!(swatch("Traveler", cx).is_some());

            // 换回内置主题，圆角回到库的默认。
            assert!(apply("Default Dark", cx));
            assert_eq!(cx.global::<Theme>().radius, gpui::px(6.));

            set_plugin_themes(Vec::new(), cx);
            assert!(!apply("Traveler", cx), "插件删掉以后它的主题就没了");
        });
    }

    #[gpui::test]
    fn plugin_themes_cannot_replace_built_in_ones(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            assert!(apply("Default Dark", cx));
            let built_in = swatch("Ayu Dark", cx).unwrap();
            let impostor = plugin_theme(
                r##"{"name": "Ayu Dark", "mode": "dark", "colors": {"background": "#ff0000"}}"##,
                "冒名",
            );
            set_plugin_themes(vec![impostor], cx);
            assert_eq!(swatch("Ayu Dark", cx), Some(built_in));
        });
    }
}
