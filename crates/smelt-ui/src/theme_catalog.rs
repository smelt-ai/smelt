//! 界面主题目录：内存里的一张「名字 → 主题配置」表，按名字 `apply`。
//!
//! gpui-kit 的 [`ThemeRegistry`] 只能加不能删，所以除了库自带的 Default Light /
//! Default Dark（`gpui_component::init` 装好，名字保留），其它主题都登记在这里的
//! [`ThemeCatalog`] 上，不再往 [`ThemeRegistry`] 里装。内置主题和第三方主题走同一套
//! 接口：按「来源」（owner，通常是插件 id）整组登记 [`register_theme_sets`]，整组换掉
//! 或 [`unregister_themes`] 卸下。登记是原子的：任何一份解析失败、名字为空、同组重名、
//! 和保留名或别的来源重名，整组都不生效，原来的登记原样保留。
//!
//! 只在登记时解析 JSON；[`apply`] / [`swatch`] / [`themes`] 只读内存，可以在 render 里调。

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::rc::Rc;

use gpui::{App, Global, Hsla, SharedString};
use gpui_component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry, ThemeSet};

use crate::ui_theme;

/// 一款主题的代表色：背景、前景、强调（primary）。设置页拿它画色点。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeSwatch {
    pub background: Hsla,
    pub foreground: Hsla,
    pub accent: Hsla,
}

/// 目录里的一款主题，给设置页列选项用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeInfo {
    pub name: SharedString,
    pub mode: ThemeMode,
    pub family: SharedString,
    /// 登记它的来源；库自带的 Default 是 `None`。
    pub owner: Option<SharedString>,
}

/// 登记失败的原因。失败时目录不变。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThemeRegisterError {
    /// 来源名为空。
    EmptyOwner,
    /// 第 `index` 份主题文件不是合法的主题集 JSON。
    Parse { index: usize, message: String },
    /// 主题集没有系列名或没有主题。
    EmptySet { index: usize },
    /// 有主题没写名字（或只有空白）。
    EmptyName,
    /// 这一组里有两份同名主题。
    DuplicateInSet(SharedString),
    /// 和库自带的保留名重名（Default Light / Default Dark）。
    Reserved(SharedString),
    /// 名字已被另一个来源登记。
    Taken {
        name: SharedString,
        owner: SharedString,
    },
}

impl fmt::Display for ThemeRegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyOwner => write!(f, "主题来源名为空"),
            Self::Parse { index, message } => write!(f, "第 {index} 份主题文件解析失败: {message}"),
            Self::EmptySet { index } => write!(f, "第 {index} 份主题文件需要系列名和至少一款主题"),
            Self::EmptyName => write!(f, "主题没有名字"),
            Self::DuplicateInSet(name) => write!(f, "主题「{name}」在同一组里出现了两次"),
            Self::Reserved(name) => write!(f, "主题名「{name}」是保留名"),
            Self::Taken { name, owner } => write!(f, "主题名「{name}」已被 {owner} 登记"),
        }
    }
}

impl std::error::Error for ThemeRegisterError {}

#[derive(Clone)]
struct Registered {
    owner: SharedString,
    family: SharedString,
    config: Rc<ThemeConfig>,
}

/// 主题目录本体。宿主经 [`register_theme_sets`] 等函数用它的全局实例；
/// 单独构造只为测试和不带 gpui 的调用方。
#[derive(Clone, Default)]
pub struct ThemeCatalog {
    // BTreeMap：列出来的顺序稳定，不随哈希变。
    themes: BTreeMap<SharedString, Registered>,
    generation: u64,
}

impl Global for ThemeCatalog {}

impl ThemeCatalog {
    /// 把 `owner` 的主题整组换成 `sources` 里的主题集（每份是 gpui-kit 主题集 JSON）。
    /// `is_reserved` 判断名字是否保留。成功返回这组的主题名（按文件里的顺序）；
    /// 失败时目录不变，`owner` 原来的那组还在。`sources` 为空等于卸下。
    pub fn register(
        &mut self,
        owner: &str,
        sources: &[&str],
        is_reserved: impl Fn(&str) -> bool,
    ) -> Result<Vec<SharedString>, ThemeRegisterError> {
        if owner.trim().is_empty() {
            return Err(ThemeRegisterError::EmptyOwner);
        }
        let owner = SharedString::from(owner.to_string());
        let mut parsed: Vec<(SharedString, ThemeConfig)> = Vec::new();
        for (index, source) in sources.iter().enumerate() {
            let set = serde_json::from_str::<ThemeSet>(source).map_err(|err| {
                ThemeRegisterError::Parse {
                    index,
                    message: err.to_string(),
                }
            })?;
            if set.name.trim().is_empty() || set.themes.is_empty() {
                return Err(ThemeRegisterError::EmptySet { index });
            }
            let family = set.name;
            parsed.extend(
                set.themes
                    .into_iter()
                    .map(|config| (family.clone(), config)),
            );
        }
        let mut seen = HashSet::new();
        for (_, config) in &parsed {
            let name = &config.name;
            if name.trim().is_empty() {
                return Err(ThemeRegisterError::EmptyName);
            }
            if !seen.insert(name.clone()) {
                return Err(ThemeRegisterError::DuplicateInSet(name.clone()));
            }
            if is_reserved(name) {
                return Err(ThemeRegisterError::Reserved(name.clone()));
            }
            if let Some(existing) = self.themes.get(name)
                && existing.owner != owner
            {
                return Err(ThemeRegisterError::Taken {
                    name: name.clone(),
                    owner: existing.owner.clone(),
                });
            }
        }
        // 全部检查通过才动目录。
        self.themes.retain(|_, entry| entry.owner != owner);
        let names = parsed
            .iter()
            .map(|(_, config)| config.name.clone())
            .collect();
        for (family, config) in parsed {
            self.themes.insert(
                config.name.clone(),
                Registered {
                    owner: owner.clone(),
                    family,
                    config: Rc::new(config),
                },
            );
        }
        self.generation = self.generation.wrapping_add(1);
        Ok(names)
    }

    /// 卸下 `owner` 登记的全部主题，返回卸下的名字。
    pub fn unregister(&mut self, owner: &str) -> Vec<SharedString> {
        let mut removed = Vec::new();
        self.themes.retain(|name, entry| {
            let keep = entry.owner != owner;
            if !keep {
                removed.push(name.clone());
            }
            keep
        });
        if !removed.is_empty() {
            self.generation = self.generation.wrapping_add(1);
        }
        removed
    }

    pub fn get(&self, name: &str) -> Option<&Rc<ThemeConfig>> {
        self.themes.get(name).map(|entry| &entry.config)
    }

    pub fn owner_of(&self, name: &str) -> Option<&SharedString> {
        self.themes.get(name).map(|entry| &entry.owner)
    }

    pub fn themes(&self) -> impl Iterator<Item = ThemeInfo> + '_ {
        self.themes.values().map(|entry| ThemeInfo {
            name: entry.config.name.clone(),
            mode: entry.config.mode,
            family: entry.family.clone(),
            owner: Some(entry.owner.clone()),
        })
    }
}

/// 库自带的 Default 主题（只读，不登记在目录里）。
fn builtin_default(name: &str, cx: &App) -> Option<Rc<ThemeConfig>> {
    if !cx.has_global::<ThemeRegistry>() {
        return None;
    }
    ThemeRegistry::global(cx)
        .default_themes()
        .values()
        .find(|config| config.name == name)
        .cloned()
}

fn lookup(name: &str, cx: &App) -> Option<Rc<ThemeConfig>> {
    if let Some(config) = builtin_default(name, cx) {
        return Some(config);
    }
    cx.try_global::<ThemeCatalog>()?.get(name).cloned()
}

/// 按来源整组登记主题，语义见 [`ThemeCatalog::register`]。保留名是 gpui-kit 自带的
/// Default 主题。不会改当前在用的主题：换掉的组里有当前主题时，界面保持原样，
/// 要不要换由调用方再 [`apply`]。
pub fn register_theme_sets(
    owner: &str,
    sources: &[&str],
    cx: &mut App,
) -> Result<Vec<SharedString>, ThemeRegisterError> {
    let defaults: Vec<SharedString> = if cx.has_global::<ThemeRegistry>() {
        ThemeRegistry::global(cx)
            .default_themes()
            .values()
            .map(|config| config.name.clone())
            .collect()
    } else {
        Vec::new()
    };
    let is_reserved = |name: &str| {
        name == smelt_model::theme_index::DEFAULT_DARK
            || name == smelt_model::theme_index::DEFAULT_LIGHT
            || defaults.iter().any(|default| *default == name)
    };
    cx.default_global::<ThemeCatalog>()
        .register(owner, sources, is_reserved)
}

/// 卸下来源 `owner` 的全部主题，返回卸下的名字。同 [`register_theme_sets`]，不动当前主题。
pub fn unregister_themes(owner: &str, cx: &mut App) -> Vec<SharedString> {
    if !cx.has_global::<ThemeCatalog>() {
        return Vec::new();
    }
    cx.global_mut::<ThemeCatalog>().unregister(owner)
}

/// 目录里能选的全部主题：先是库自带的 Default（浅色在前），再是登记的主题（按名字）。
pub fn themes(cx: &App) -> Vec<ThemeInfo> {
    let mut list: Vec<ThemeInfo> = Vec::new();
    if cx.has_global::<ThemeRegistry>() {
        let mut defaults: Vec<_> = ThemeRegistry::global(cx)
            .default_themes()
            .values()
            .map(|config| ThemeInfo {
                name: config.name.clone(),
                mode: config.mode,
                family: "Default".into(),
                owner: None,
            })
            .collect();
        defaults.sort_by_key(|info| info.mode.is_dark());
        list.extend(defaults);
    }
    if let Some(catalog) = cx.try_global::<ThemeCatalog>() {
        list.extend(catalog.themes());
    }
    list
}

/// 目录变化代数，用于在主题登记或撤下时重建界面缓存。
pub fn generation(cx: &App) -> u64 {
    cx.try_global::<ThemeCatalog>()
        .map_or(0, |catalog| catalog.generation)
}

/// 名字现在能不能选（库自带的 Default 或已登记）。
pub fn contains(name: &str, cx: &App) -> bool {
    lookup(name, cx).is_some()
}

/// 按名字取一款主题的代表色。颜色由 gpui-kit 自己解析主题配置得出，换上这款主题后
/// 界面就是这几个色；不另写一套解析。名字不在目录里时是 `None`。只读内存。
pub fn swatch(name: &str, cx: &App) -> Option<ThemeSwatch> {
    let config = lookup(name, cx)?;
    let mut theme = Theme::global(cx).clone();
    theme.apply_config(&config);
    Some(ThemeSwatch {
        background: theme.background,
        foreground: theme.foreground,
        accent: theme.primary,
    })
}

/// 按名字装上主题，产品色跟着换成这份主题。名字不在目录里时返回 false，当前主题不变。
pub fn apply(name: &str, cx: &mut App) -> bool {
    if !cx.has_global::<ThemeRegistry>() || !cx.has_global::<Theme>() {
        return false;
    }
    let Some(config) = lookup(name, cx) else {
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
        // gpui-kit 在没有 highlight 时保留旧值；先还原默认，避免串到上一款主题。
        theme.highlight_theme = if config.mode.is_dark() {
            gpui_component::highlighter::HighlightTheme::default_dark()
        } else {
            gpui_component::highlighter::HighlightTheme::default_light()
        };
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

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    fn set(names: &[(&str, &str)]) -> String {
        let themes: Vec<_> = names
            .iter()
            .map(|(name, bg)| {
                serde_json::json!({
                    "name": name,
                    "mode": "dark",
                    "colors": { "background": bg, "foreground": "#fafafa", "primary.background": "#3366ff" }
                })
            })
            .collect();
        serde_json::json!({ "name": "test", "themes": themes }).to_string()
    }

    fn no_reserved(_: &str) -> bool {
        false
    }

    fn names(catalog: &ThemeCatalog) -> Vec<String> {
        catalog.themes().map(|info| info.name.to_string()).collect()
    }

    #[test]
    fn register_replace_and_unregister_by_owner() {
        let mut catalog = ThemeCatalog::default();
        let a = set(&[("A1", "#000000"), ("A2", "#111111")]);
        let b = set(&[("B1", "#222222")]);
        assert_eq!(catalog.register("a", &[&a], no_reserved).unwrap().len(), 2);
        catalog.register("b", &[&b], no_reserved).unwrap();
        assert_eq!(names(&catalog), ["A1", "A2", "B1"]);
        assert_eq!(
            catalog.owner_of("B1").map(ToString::to_string).as_deref(),
            Some("b")
        );

        // 同一来源再登记是整组替换：A2 不在新组里，就没了。
        let a2 = set(&[("A1", "#333333"), ("A3", "#444444")]);
        catalog.register("a", &[&a2], no_reserved).unwrap();
        assert_eq!(names(&catalog), ["A1", "A3", "B1"]);

        let mut removed: Vec<String> = catalog
            .unregister("a")
            .into_iter()
            .map(String::from)
            .collect();
        removed.sort();
        assert_eq!(removed, ["A1", "A3"]);
        assert_eq!(names(&catalog), ["B1"]);
        assert!(catalog.unregister("a").is_empty());
    }

    #[test]
    fn catalog_retains_family_and_changes_generation_only_on_success() {
        let mut catalog = ThemeCatalog::default();
        let initial = catalog.generation;
        let source = set(&[("Day", "#ffffff")]);
        catalog.register("plugin", &[&source], no_reserved).unwrap();
        let registered = catalog.generation;
        assert_ne!(registered, initial);
        assert_eq!(catalog.themes().next().unwrap().family.as_ref(), "test");
        assert!(catalog.register("plugin", &["{"], no_reserved).is_err());
        assert_eq!(catalog.generation, registered);
        catalog.unregister("plugin");
        assert_ne!(catalog.generation, registered);
    }

    #[test]
    fn failed_registration_leaves_catalog_untouched() {
        let mut catalog = ThemeCatalog::default();
        let a = set(&[("A1", "#000000")]);
        let b = set(&[("B1", "#111111")]);
        catalog.register("a", &[&a], no_reserved).unwrap();
        catalog.register("b", &[&b], no_reserved).unwrap();
        let before = names(&catalog);

        // 第二份坏掉，第一份也不能生效；a 原来的组还在。
        let good = set(&[("A9", "#555555")]);
        assert!(matches!(
            catalog.register("a", &[&good, "{ not json"], no_reserved),
            Err(ThemeRegisterError::Parse { index: 1, .. })
        ));
        // 和别的来源重名。
        let clash = set(&[("A9", "#555555"), ("B1", "#666666")]);
        assert_eq!(
            catalog.register("a", &[&clash], no_reserved),
            Err(ThemeRegisterError::Taken {
                name: "B1".into(),
                owner: "b".into()
            })
        );
        // 同组重名（跨文件也算）。
        let dup = set(&[("A9", "#555555")]);
        assert_eq!(
            catalog.register("a", &[&dup, &dup], no_reserved),
            Err(ThemeRegisterError::DuplicateInSet("A9".into()))
        );
        // 保留名、空名、空来源。
        let reserved = set(&[("Default Dark", "#000000")]);
        assert_eq!(
            catalog.register("a", &[&reserved], |name| name == "Default Dark"),
            Err(ThemeRegisterError::Reserved("Default Dark".into()))
        );
        let empty = set(&[(" ", "#000000")]);
        assert_eq!(
            catalog.register("a", &[&empty], no_reserved),
            Err(ThemeRegisterError::EmptyName)
        );
        assert_eq!(
            catalog.register("", &[&good], no_reserved),
            Err(ThemeRegisterError::EmptyOwner)
        );

        assert_eq!(names(&catalog), before);
        assert_eq!(
            catalog.owner_of("A1").map(ToString::to_string).as_deref(),
            Some("a")
        );
    }

    #[gpui::test]
    fn theme_without_highlight_does_not_keep_the_previous_theme_highlight(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let source = r#"{"name":"Test","themes":[
                {"name":"Highlighted","mode":"dark","highlight":{"syntax":{}}},
                {"name":"No highlight","mode":"dark"}
            ]}"#;
            register_theme_sets("test", &[source], cx).unwrap();
            assert!(apply("Highlighted", cx));
            let default = gpui_component::highlighter::HighlightTheme::default_dark();
            assert!(!std::sync::Arc::ptr_eq(
                &Theme::global(cx).highlight_theme,
                &default
            ));
            assert!(apply("No highlight", cx));
            assert!(std::sync::Arc::ptr_eq(
                &Theme::global(cx).highlight_theme,
                &default
            ));
        });
    }

    #[gpui::test]
    fn apply_follows_registration(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            // 库自带的 Default 不用登记就能选，也不能被抢名。
            assert!(apply("Default Light", cx));
            assert!(apply("Default Dark", cx));
            let clash = set(&[("Default Dark", "#000000")]);
            assert!(matches!(
                register_theme_sets("third", &[&clash], cx),
                Err(ThemeRegisterError::Reserved(_))
            ));

            assert!(!apply("Plugin Dark", cx));
            let source = set(&[("Plugin Dark", "#101820")]);
            register_theme_sets("third", &[&source], cx).unwrap();
            assert!(contains("Plugin Dark", cx));
            let font = Theme::global(cx).font_size;
            assert!(apply("Plugin Dark", cx));
            assert_eq!(Theme::global(cx).font_size, font, "换主题不动字号");
            assert!(swatch("Plugin Dark", cx).is_some());
            assert!(themes(cx).iter().any(|info| info.name == "Plugin Dark"
                && info.owner.as_ref().is_some_and(|o| *o == "third")));

            assert_eq!(unregister_themes("third", cx), ["Plugin Dark"]);
            assert!(!apply("Plugin Dark", cx));
            assert_eq!(swatch("Plugin Dark", cx), None);
            // gpui-kit 的注册表没被写进去过。
            assert!(
                !ThemeRegistry::global(cx)
                    .themes()
                    .contains_key(&SharedString::from("Plugin Dark"))
            );
        });
    }
}
