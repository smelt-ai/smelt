//! 界面主题目录：内存里的一张「名字 → 主题配置」表，按名字 `apply`。
//!
//! smelt-ui 自带 smelt Light / smelt Dark（`themes/smelt.json`，名字保留），删掉主题插件
//! 也还在。内置主题和第三方主题走同一套接口：按「来源」（owner，通常是插件 id）整组登记
//! [`register_theme_sets`]，整组换掉或 [`unregister_themes`] 卸下。登记是原子的：任何一份
//! 解析失败、名字为空、同组重名、和保留名或别的来源重名，整组都不生效，原来的登记原样保留。
//!
//! 主题文件的格式和求值见 [`crate::theme_config`]，登记时解析、求值一次。装上的主题是
//! [`ActiveTheme`] 这个全局，它是当前主题的唯一出处：自绘色板（`ui_theme`）在 [`apply`] 里
//! 从它投影；应用里还用组件库画的 diff 和代码编辑器由应用观察这个全局，把同一份主题投影给
//! 组件库（见 `smelt` 的 `component_theme`）。
//!
//! 只在登记时解析 JSON；[`apply`] / [`swatch`] / [`themes`] 只读内存，可以在 render 里调。

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::sync::{Arc, LazyLock};

use gpui::{App, Global, Hsla, SharedString};

use crate::theme_config::{self, ThemeColors};
pub use crate::theme_config::{ThemeConfig, ThemeMode};
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
    /// 登记它的来源；库自带的 smelt 主题是 `None`。
    pub owner: Option<SharedString>,
}

/// 登记失败的原因。失败时目录不变。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThemeRegisterError {
    /// 来源名为空。
    EmptyOwner,
    /// 第 `index` 份主题文件不合法（格式见 `theme_config`）。
    Parse { index: usize, message: String },
    /// 主题集没有系列名或没有主题。
    EmptySet { index: usize },
    /// 有主题没写名字（或只有空白）。
    EmptyName,
    /// 这一组里有两份同名主题。
    DuplicateInSet(SharedString),
    /// 和库自带的保留名重名（smelt Light / smelt Dark）。
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
    config: Arc<ThemeConfig>,
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
    /// 把 `owner` 的主题整组换成 `sources` 里的主题集（每份是一个 smelt 主题文件，格式见
    /// [`crate::theme_config`]）。
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
            let set =
                theme_config::parse_theme_set(source).map_err(|err| ThemeRegisterError::Parse {
                    index,
                    message: err.to_string(),
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
            let name = config.name();
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
            .map(|(_, config)| config.name().clone())
            .collect();
        for (family, config) in parsed {
            self.themes.insert(
                config.name().clone(),
                Registered {
                    owner: owner.clone(),
                    family,
                    config: Arc::new(config),
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

    pub fn get(&self, name: &str) -> Option<&Arc<ThemeConfig>> {
        self.themes.get(name).map(|entry| &entry.config)
    }

    pub fn owner_of(&self, name: &str) -> Option<&SharedString> {
        self.themes.get(name).map(|entry| &entry.owner)
    }

    pub fn themes(&self) -> impl Iterator<Item = ThemeInfo> + '_ {
        self.themes.values().map(|entry| ThemeInfo {
            name: entry.config.name().clone(),
            mode: entry.config.mode(),
            family: entry.family.clone(),
            owner: Some(entry.owner.clone()),
        })
    }
}

/// 库自带的 smelt Light / smelt Dark。只在第一次用到时解析一次。
const BUILTIN_SOURCE: &str = include_str!("../themes/smelt.json");

static BUILTIN: LazyLock<(SharedString, Vec<Arc<ThemeConfig>>)> = LazyLock::new(|| {
    let set = theme_config::parse_theme_set(BUILTIN_SOURCE)
        .expect("themes/smelt.json 必须是合法的主题文件");
    (set.name, set.themes.into_iter().map(Arc::new).collect())
});

fn builtin(name: &str) -> Option<Arc<ThemeConfig>> {
    BUILTIN
        .1
        .iter()
        .find(|config| config.name() == name)
        .cloned()
}

/// 同明暗的 smelt 主题：还没装主题、或用户选的主题没了时用它。
pub(crate) fn builtin_for_mode(mode: ThemeMode) -> Arc<ThemeConfig> {
    BUILTIN
        .1
        .iter()
        .find(|config| config.mode() == mode)
        .cloned()
        .expect("themes/smelt.json 深浅两款都要有")
}

/// 当前装上的主题。由 [`apply`] 整份换上；换主题时观察它的全局观察者都会被叫到。
pub struct ActiveTheme {
    config: Arc<ThemeConfig>,
    colors: ThemeColors,
}

impl Global for ActiveTheme {}

impl ActiveTheme {
    /// 装上的那款主题。
    pub fn config(&self) -> &ThemeConfig {
        &self.config
    }

    pub fn mode(&self) -> ThemeMode {
        self.config.mode()
    }

    /// 装上的语义色。窗口底下垫了材质时字色已按材质配够对比，可能和主题文件求出的不同。
    pub fn colors(&self) -> &ThemeColors {
        &self.colors
    }
}

/// 当前装上的主题；还没 [`apply`] 过时是 `None`。
pub fn active(cx: &App) -> Option<&ActiveTheme> {
    cx.try_global::<ActiveTheme>()
}

fn lookup(name: &str, cx: &App) -> Option<Arc<ThemeConfig>> {
    if let Some(config) = builtin(name) {
        return Some(config);
    }
    cx.try_global::<ThemeCatalog>()?.get(name).cloned()
}

/// 按来源整组登记主题，语义见 [`ThemeCatalog::register`]。保留名是库自带的 smelt 主题。
/// 不会改当前在用的主题：换掉的组里有当前主题时，界面保持原样，要不要换由调用方再
/// [`apply`]。
pub fn register_theme_sets(
    owner: &str,
    sources: &[&str],
    cx: &mut App,
) -> Result<Vec<SharedString>, ThemeRegisterError> {
    cx.default_global::<ThemeCatalog>()
        .register(owner, sources, |name| builtin(name).is_some())
}

/// 卸下来源 `owner` 的全部主题，返回卸下的名字。同 [`register_theme_sets`]，不动当前主题。
pub fn unregister_themes(owner: &str, cx: &mut App) -> Vec<SharedString> {
    if !cx.has_global::<ThemeCatalog>() {
        return Vec::new();
    }
    cx.global_mut::<ThemeCatalog>().unregister(owner)
}

/// 目录里能选的全部主题：先是库自带的 smelt（浅色在前），再是登记的主题（按名字）。
pub fn themes(cx: &App) -> Vec<ThemeInfo> {
    let (family, builtin) = &*BUILTIN;
    let mut list: Vec<ThemeInfo> = builtin
        .iter()
        .map(|config| ThemeInfo {
            name: config.name().clone(),
            mode: config.mode(),
            family: family.clone(),
            owner: None,
        })
        .collect();
    list.sort_by_key(|info| info.mode.is_dark());
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

/// 名字现在能不能选（库自带的 smelt 主题或已登记）。
pub fn contains(name: &str, cx: &App) -> bool {
    lookup(name, cx).is_some()
}

/// 按名字取一款主题的代表色：装上这款主题后界面就是这几个色。名字不在目录里时是
/// `None`。只读内存。
pub fn swatch(name: &str, cx: &App) -> Option<ThemeSwatch> {
    let config = lookup(name, cx)?;
    let colors = config.colors();
    Some(ThemeSwatch {
        background: colors.bg_base.into(),
        foreground: colors.text_base.into(),
        accent: colors.bg_accent.into(),
    })
}

/// 按名字装上主题，产品色跟着换成这份主题。名字不在目录里时返回 false，当前主题不变。
pub fn apply(name: &str, cx: &mut App) -> bool {
    let Some(config) = lookup(name, cx) else {
        return false;
    };
    let dark = config.mode().is_dark();
    let mut colors = *config.colors();
    // 毛玻璃时字压在材质上，不是压在主题底色上，字色按材质配够对比。
    ui_theme::fit_text_to_backdrop(&mut colors, dark);
    ui_theme::install_theme(&colors, config.elevation().clone(), dark);
    cx.set_global(ActiveTheme { config, colors });
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
                    "colors": { "bg_base": bg, "text_base": "#fafafa", "bg_accent": "#3366ff" }
                })
            })
            .collect();
        serde_json::json!({ "version": 1, "name": "test", "themes": themes }).to_string()
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
    fn apply_follows_registration(cx: &mut TestAppContext) {
        cx.update(|cx| {
            // 库自带的 smelt 主题不用登记就能选，也不能被抢名；旧版本存的 Default 认不出。
            assert!(apply(smelt_model::theme_index::DEFAULT_LIGHT, cx));
            assert!(!active(cx).unwrap().mode().is_dark());
            assert!(apply(smelt_model::theme_index::DEFAULT_DARK, cx));
            assert!(active(cx).unwrap().mode().is_dark());
            assert!(!apply("Default Dark", cx));
            assert!(themes(cx).iter().all(|info| info.family != "Default"));
            let clash = set(&[(smelt_model::theme_index::DEFAULT_DARK, "#000000")]);
            assert!(matches!(
                register_theme_sets("third", &[&clash], cx),
                Err(ThemeRegisterError::Reserved(_))
            ));

            assert!(!apply("Plugin Dark", cx));
            let source = set(&[("Plugin Dark", "#101820")]);
            register_theme_sets("third", &[&source], cx).unwrap();
            assert!(contains("Plugin Dark", cx));
            assert!(apply("Plugin Dark", cx));
            assert_eq!(active(cx).unwrap().config().name().as_ref(), "Plugin Dark");
            assert_eq!(ui_theme::bg(), 0x101820, "自绘色板跟着换");
            let plugin_swatch = swatch("Plugin Dark", cx).unwrap();
            assert_eq!(plugin_swatch.background, gpui::rgb(0x101820).into());
            assert!(themes(cx).iter().any(|info| info.name == "Plugin Dark"
                && info.owner.as_ref().is_some_and(|o| *o == "third")));

            assert_eq!(unregister_themes("third", cx), ["Plugin Dark"]);
            assert!(!apply("Plugin Dark", cx));
            assert_eq!(swatch("Plugin Dark", cx), None);
        });
    }
}
