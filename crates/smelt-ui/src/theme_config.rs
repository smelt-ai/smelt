//! 主题文件：格式、校验和求值。
//!
//! 主题就是数据。文件是 smelt 自己的 JSON 格式，带版本号（当前是 [`FORMAT_VERSION`]）：
//!
//! ```json
//! {
//!   "version": 1,
//!   "name": "系列名",
//!   "author": "选填",
//!   "url": "选填",
//!   "themes": [{
//!     "name": "主题名",
//!     "mode": "light",
//!     "colors": { "bg_base": "#ffffff", "text_base": "#1a1a18", "bg_accent": "#3a6ee1" },
//!     "elevation": { "floating": [{ "y": 4, "blur": 8, "color": "#0000001f" }] },
//!     "syntax": { "keyword": { "color": "#d73a49", "font_weight": 600 } }
//!   }]
//! }
//! ```
//!
//! 颜色键就是 [`ThemeColors`] 的字段名（四种状态色写成 `success.bg` 这样，分类色写成
//! `chart.1`–`chart.5`）。只有 `bg_base`、`text_base` 两个种子色必填；没写的色在 OKLab 里
//! 从已经求出的色推出来，写了的就用写的，下游的色跟着它推。所以只给种子色、逐个写死、两者
//! 混着写都行。每个色的推导规则
//! 写在 [`ThemeColors`] 的字段注释里，`docs/ui-design-language.md` 第 2 节有整张表。
//!
//! 校验是严格的：版本不是 1、出现不认识的键、颜色不是 `#rgb` / `#rgba` / `#rrggbb` /
//! `#rrggbbaa`、种子色带透明度、类型不对，整份文件都不收（登记报错，见 `theme_catalog`）。
//! 解析和求值都在这里，只在登记时做一次；之后读到的都是求好的 [`ThemeConfig`]。

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use gpui::{BoxShadow, Rgba, SharedString, px};
use serde_json::{Map, Value};

/// 主题文件格式的版本。格式改了就加一；读到别的版本整份不收。
pub const FORMAT_VERSION: u64 = 1;

/// 主题的明暗。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ThemeMode {
    #[default]
    Light,
    Dark,
}

impl ThemeMode {
    pub fn is_dark(self) -> bool {
        self == Self::Dark
    }

    /// 主题文件和外观设置里的写法：`light` / `dark`。
    pub fn key(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// [`Self::key`] 的反向；别的写法不认。
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }
}

/// 一种状态（成功、警告、危险、信息）的四个色。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StateColors {
    /// 浅底：提示条、标记底。← 从「抬 0」那层（`bg_base`，深色明度不低于 0.145）往 `fg` 走
    /// 12%（OKLab）。
    pub bg: Rgba,
    /// 字色。← 中性色相 oklch(深色 0.75 / 浅色 0.5, 0.15, h)，h 成功 150、警告 80、危险 25、信息 256。
    pub fg: Rgba,
    /// 边框。← 同 `bg`，走 35%。
    pub border: Rgba,
    /// 图标、状态点。← `fg`
    pub icon: Rgba,
}

/// 求值后一款主题的全部语义色。字段名就是主题文件 `colors` 里的键。
///
/// 注释里 `←` 后面是没写时的推法。「抬 x」是把 `bg_base` 的 OKLab 明度往前景方向挪 x（深色
/// 主题往亮、浅色往暗；深色从不低于 0.145 的明度起抬，纯黑底也有看得出的层），色度按同样的
/// 比例从 `bg_base` 的往 `text_base` 的走（明度走到正文色那一半，色度也走一半）。「字 x%」是
/// `text_base` 换成 x% 的不透明度。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeColors {
    // ---- 底色 ----
    /// 窗口最外层（侧栏），主内容像一张纸浮在它上面。← `bg_base` 的 OKLab 明度降 0.02（深浅
    /// 都往暗走）
    pub page: Rgba,
    /// 主内容底。种子色，必填，不透明。
    pub bg_base: Rgba,
    /// 比主内容更靠后的一层。← `page`
    pub bg_deep: Rgba,
    /// 一档档抬起来的层。← 抬 深色 0.06 / 浅色 0.015
    pub layer_01: Rgba,
    /// ← 抬 0.10 / 0.04
    pub layer_02: Rgba,
    /// ← 抬 0.15 / 0.05
    pub layer_03: Rgba,
    /// 最高一层：关着的开关槽。← 抬 0.275 / 0.11
    pub layer_04: Rgba,
    /// 反色底。← `text_base`
    pub bg_inverse: Rgba,
    /// 高对比底：主按钮，两种明暗下都是压浅色字的深色块。← 深色 `layer_04` / 浅色 `text_base`
    pub bg_contrast: Rgba,
    /// 中性按钮底。← 深色 字 6% / 浅色 `bg_base`
    pub bg_button_neutral: Rgba,
    /// 高对比图标按钮（发送）底。深色主题里反过来是浅色块。← 深色 OKLab 明度 0.89、色相色度
    /// 同 `text_base` / 浅色 `bg_contrast`
    pub bg_icon_button_contrast: Rgba,
    /// 强调色：开关打开、单选。种子色，选填。← 蓝 oklch(0.55, 0.23, 268)
    pub bg_accent: Rgba,

    // ---- 发送按钮 ----
    /// 发送按钮上端的高光。← 白 深色 60% / 浅色 20%
    pub submit_highlight: Rgba,
    /// ← 深色 黑 8% / 浅色 `bg_base` 12%
    pub submit_hover: Rgba,
    /// ← 深色 `text_inverse` / 浅色 `text_contrast`
    pub submit_fg: Rgba,
    /// ← `submit_fg`
    pub submit_icon: Rgba,

    // ---- 文字 ----
    /// 正文。种子色，必填，不透明。
    pub text_base: Rgba,
    /// 次要字。← 从 `bg_base` 往 `text_base` 走 深色 72% / 浅色 57% 的 OKLab 明度，对比不到
    /// 4.5:1 时再往前走到够（`text_base` 自己都不够就是 `text_base`）。
    pub text_muted: Rgba,
    /// 最弱字。← 同上，走 54% / 36%，保底 3:1。
    pub text_faint: Rgba,
    /// 压在反色底上的字。← `bg_base`
    pub text_inverse: Rgba,
    /// 压在高对比底上的字，两种明暗下都是浅的那头。← 深色 `text_base` / 浅色 `bg_base`
    pub text_contrast: Rgba,
    /// 强调色的字（链接）。← `bg_accent`，压在 `bg_base` 上不到 4.5:1 时保持色相把明度往远离
    /// 底色的方向挪到够
    pub text_accent: Rgba,

    // ---- 图标 ----
    /// ← `text_base`
    pub icon_base: Rgba,
    /// ← `text_muted`
    pub icon_muted: Rgba,
    /// ← `text_faint`
    pub icon_faint: Rgba,
    /// ← `text_inverse`
    pub icon_inverse: Rgba,
    /// ← `text_contrast`
    pub icon_contrast: Rgba,
    /// ← `text_accent`
    pub icon_accent: Rgba,

    // ---- 线 ----
    /// ← 字 8%
    pub border_muted: Rgba,
    /// 分隔线、卡片外沿。← 字 深色 11% / 浅色 10%
    pub border_base: Rgba,
    /// 输入框外沿。← 字 20%
    pub border_strong: Rgba,
    /// 焦点环。← 蓝 oklch(0.70, 0.15, 268)
    pub border_focus: Rgba,

    // ---- 叠层和固定色 ----
    /// 悬停叠层。← 字 深色 6% / 浅色 4%
    pub hover: Rgba,
    /// 按下叠层。← 字 深色 10% / 浅色 8%
    pub pressed: Rgba,
    /// 压在高对比底上的悬停叠层：浅色主题把深色块提亮，深色主题把灰块压暗。← 深色 黑 24% /
    /// 浅色 `bg_base` 12%
    pub contrast_hover: Rgba,
    /// ← 深色 黑 40% / 浅色 `bg_base` 24%
    pub contrast_pressed: Rgba,
    /// 对话框后的遮罩。← 黑 深色 60% / 浅色 40%
    pub scrim: Rgba,
    /// 立体高光的上端。← 白
    pub depth_top: Rgba,
    /// 立体高光的下端。← 白 0%
    pub depth_bottom: Rgba,
    /// ← 白
    pub white: Rgba,
    /// ← 黑
    pub black: Rgba,
    /// 开关、单选的圆钮：两种明暗下都是近白的实体。← OKLab 明度 0.96，色相色度取 `bg_base`
    /// 和 `text_base` 里更鲜艳的那个
    pub knob: Rgba,
    /// 打开时的圆钮，比关着的略暗一点。← 明度 0.95，色相同上
    pub knob_on: Rgba,

    // ---- 状态 ----
    pub success: StateColors,
    pub warning: StateColors,
    pub danger: StateColors,
    pub info: StateColors,

    // ---- 浮起的面 ----
    /// 菜单、弹层、对话框。← 深色 抬 0.015 / 浅色 `bg_base`
    pub surface_raised: Rgba,

    // ---- 选区、分类色 ----
    /// 文字选区。← `text_accent` 30%
    pub selection: Rgba,
    /// 分类色，键是 `chart.1`–`chart.5`。← oklch(深色 0.72 / 浅色 0.58, 0.13, h)，h 依次 255、
    /// 150、75、300、0。
    pub chart: [Rgba; 5],
}

/// 主题文件 `colors` 里认得的键，和 [`ThemeColors`] 的字段一一对应。
#[rustfmt::skip]
const COLOR_KEYS: &[&str] = &[
    "page", "bg_base", "bg_deep", "layer_01", "layer_02", "layer_03", "layer_04",
    "bg_inverse", "bg_contrast", "bg_button_neutral", "bg_icon_button_contrast", "bg_accent",
    "submit_highlight", "submit_hover", "submit_fg", "submit_icon",
    "text_base", "text_muted", "text_faint", "text_inverse", "text_contrast", "text_accent",
    "icon_base", "icon_muted", "icon_faint", "icon_inverse", "icon_contrast", "icon_accent",
    "border_muted", "border_base", "border_strong", "border_focus",
    "hover", "pressed", "contrast_hover", "contrast_pressed", "scrim",
    "depth_top", "depth_bottom", "white", "black", "knob", "knob_on",
    "success.bg", "success.fg", "success.border", "success.icon",
    "warning.bg", "warning.fg", "warning.border", "warning.icon",
    "danger.bg", "danger.fg", "danger.border", "danger.icon",
    "info.bg", "info.fg", "info.border", "info.icon",
    "surface_raised", "selection", "chart.1", "chart.2", "chart.3", "chart.4", "chart.5",
];

/// 一款主题的投影。每档是叠在一起的几层阴影，`[]` 是没有。
///
/// 没写的档按下面的中性几何推：阴影是黑色，外圈线是 0.5px 的「字 浅色 12% / 深色 16%」（半
/// 像素的线画出来比 1px 边框淡，要比 `border_base` 浓才看得出同样的边）。「浮起 y」是一远
/// 一近两层：近的下移 y、模糊 2y，黑 深色 30% / 浅色 8%；远的下移 2y、模糊 4y，黑 深色 30% /
/// 浅色 4%。
#[derive(Clone, Debug, PartialEq)]
pub struct Elevation {
    /// 贴着内容抬起一点的卡片。← 浮起 1 + 外圈线
    pub raised: Vec<BoxShadow>,
    /// 菜单、提示。← 浮起 4 + 外圈线
    pub floating: Vec<BoxShadow>,
    /// 对话框。← 浮起 8 + 外圈线
    pub overlay: Vec<BoxShadow>,
    /// 中性按钮。← 下移 1 模糊 1.5 黑 深色 40% / 浅色 10% + 外圈线
    pub button_neutral: Vec<BoxShadow>,
    /// 高对比按钮。← 下移 1 模糊 1.5 黑 深色 40% / 浅色 20%
    pub button_contrast: Vec<BoxShadow>,
    /// 小部件（开关圆钮）。← 下移 0.5 模糊 0.5 黑 40%
    pub elements: Vec<BoxShadow>,
    /// 关着的开关槽。← 外圈线换成内圈
    pub switch_off: Vec<BoxShadow>,
    /// 开着的开关槽。← 同上
    pub switch_on: Vec<BoxShadow>,
    /// 自带边框的弹层，不再叠外圈线。← 浮起 3
    pub md: Vec<BoxShadow>,
    /// 圆钮。← 下移 1 模糊 3 黑 深色 30% / 浅色 8%
    pub knob: Vec<BoxShadow>,
    /// 输入框。← `button_neutral`
    pub input: Vec<BoxShadow>,
}

/// 主题文件 `elevation` 里认得的键，和 [`Elevation`] 的字段一一对应。
const ELEVATION_KEYS: &[&str] = &[
    "raised",
    "floating",
    "overlay",
    "button_neutral",
    "button_contrast",
    "elements",
    "switch_off",
    "switch_on",
    "md",
    "knob",
    "input",
];

/// 代码高亮的字形。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxFontStyle {
    Normal,
    Italic,
    Underline,
}

/// 一类代码记号的样式。没写的项由高亮器用默认的。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SyntaxStyle {
    pub color: Option<Rgba>,
    pub font_style: Option<SyntaxFontStyle>,
    /// 100–900，整百。
    pub font_weight: Option<u16>,
}

/// 主题文件 `syntax` 里认得的记号类别（tree-sitter 高亮名）。
#[rustfmt::skip]
pub const SYNTAX_KEYS: &[&str] = &[
    "attribute", "boolean", "comment", "comment.doc", "constant", "constructor", "embedded",
    "emphasis", "emphasis.strong", "enum", "function", "hint", "keyword", "label",
    "link_text", "link_uri", "number", "operator", "predictive", "preproc", "primary",
    "property", "punctuation", "punctuation.bracket", "punctuation.delimiter",
    "punctuation.list_marker", "punctuation.special", "string", "string.escape",
    "string.regex", "string.special", "string.special.symbol", "tag", "tag.doctype",
    "text.code.span", "text.literal", "title", "type", "variable", "variable.special", "variant",
];

/// 一款求好的主题。登记时解析、求值一次，之后只读。
#[derive(Clone, Debug)]
pub struct ThemeConfig {
    name: SharedString,
    mode: ThemeMode,
    colors: ThemeColors,
    elevation: Arc<Elevation>,
    syntax: Vec<(&'static str, SyntaxStyle)>,
}

impl ThemeConfig {
    pub fn name(&self) -> &SharedString {
        &self.name
    }

    pub fn mode(&self) -> ThemeMode {
        self.mode
    }

    /// 求值后的语义色，没按窗口材质配过字色（装上后配过的见 `ui_theme::colors`）。
    pub fn colors(&self) -> &ThemeColors {
        &self.colors
    }

    pub fn elevation(&self) -> &Arc<Elevation> {
        &self.elevation
    }

    /// 文件里写了的代码高亮样式，键是 [`SYNTAX_KEYS`] 里的类别。
    pub fn syntax(&self) -> &[(&'static str, SyntaxStyle)] {
        &self.syntax
    }
}

/// 一份主题集文件。
pub(crate) struct ThemeSet {
    pub name: SharedString,
    pub themes: Vec<ThemeConfig>,
}

/// 主题文件不合格式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeParseError(String);

impl fmt::Display for ThemeParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn invalid(message: impl Into<String>) -> ThemeParseError {
    ThemeParseError(message.into())
}

/// 解析一份主题集文件并求值其中每款主题。任何一处不合格式整份报错。
pub(crate) fn parse_theme_set(source: &str) -> Result<ThemeSet, ThemeParseError> {
    let value: Value = serde_json::from_str(source).map_err(|err| invalid(err.to_string()))?;
    let at = "主题文件";
    let set = object(&value, at)?;
    known_keys(set, &["version", "name", "author", "url", "themes"], at)?;
    match set.get("version") {
        Some(version) if version.as_u64() == Some(FORMAT_VERSION) => {}
        Some(other) => {
            return Err(invalid(format!(
                "不认识的主题格式版本 {other}，这个版本的 smelt 只认 {FORMAT_VERSION}"
            )));
        }
        None => {
            return Err(invalid(format!(
                "{at}缺少 version（当前是 {FORMAT_VERSION}）"
            )));
        }
    }
    let name = required_str(set, "name", at)?;
    optional_str(set, "author", at)?;
    optional_str(set, "url", at)?;
    let themes = match set.get("themes") {
        Some(Value::Array(themes)) => themes
            .iter()
            .enumerate()
            .map(|(index, theme)| parse_theme(theme, &format!("themes[{index}]")))
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(invalid("themes 必须是数组")),
        None => return Err(invalid(format!("{at}缺少 themes"))),
    };
    Ok(ThemeSet { name, themes })
}

fn object<'a>(value: &'a Value, at: &str) -> Result<&'a Map<String, Value>, ThemeParseError> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{at} 必须是 JSON 对象")))
}

fn known_keys(map: &Map<String, Value>, allowed: &[&str], at: &str) -> Result<(), ThemeParseError> {
    match map.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(invalid(format!("{at} 里有不认识的键 {key}"))),
        None => Ok(()),
    }
}

fn required_str(
    map: &Map<String, Value>,
    key: &str,
    at: &str,
) -> Result<SharedString, ThemeParseError> {
    match map.get(key) {
        Some(Value::String(text)) => Ok(text.clone().into()),
        Some(_) => Err(invalid(format!("{at} 的 {key} 必须是字符串"))),
        None => Err(invalid(format!("{at} 缺少 {key}"))),
    }
}

fn optional_str(map: &Map<String, Value>, key: &str, at: &str) -> Result<(), ThemeParseError> {
    match map.get(key) {
        None | Some(Value::String(_)) => Ok(()),
        Some(_) => Err(invalid(format!("{at} 的 {key} 必须是字符串"))),
    }
}

/// 在认得的键里找到这个键的 `'static` 版本。
fn known(keys: &[&'static str], key: &str) -> Option<&'static str> {
    keys.iter().copied().find(|known| *known == key)
}

fn parse_theme(value: &Value, at: &str) -> Result<ThemeConfig, ThemeParseError> {
    let theme = object(value, at)?;
    known_keys(
        theme,
        &["name", "mode", "colors", "elevation", "syntax"],
        at,
    )?;
    let name = required_str(theme, "name", at)?;
    let mode = match theme.get("mode") {
        Some(Value::String(key)) => ThemeMode::from_key(key)
            .ok_or_else(|| invalid(format!("{at}.mode 只能是 light 或 dark，读到 \"{key}\"")))?,
        Some(other) => {
            return Err(invalid(format!(
                "{at}.mode 只能是 light 或 dark，读到 {other}"
            )));
        }
        None => return Err(invalid(format!("{at} 缺少 mode"))),
    };
    let colors = match theme.get("colors") {
        Some(colors) => parse_colors(colors, &format!("{at}.colors"))?,
        None => {
            return Err(invalid(format!(
                "{at} 缺少 colors（至少要写 bg_base 和 text_base）"
            )));
        }
    };
    let shadows = match theme.get("elevation") {
        Some(elevation) => parse_elevation(elevation, &format!("{at}.elevation"))?,
        None => HashMap::new(),
    };
    let syntax = match theme.get("syntax") {
        Some(syntax) => parse_syntax(syntax, &format!("{at}.syntax"))?,
        None => Vec::new(),
    };
    let dark = mode.is_dark();
    let colors = resolve_colors(dark, &colors);
    let ring = alpha(colors.text_base, if dark { 0.16 } else { 0.12 });
    let elevation = resolve_elevation(dark, ring, shadows);
    Ok(ThemeConfig {
        name,
        mode,
        colors,
        elevation: Arc::new(elevation),
        syntax,
    })
}

fn parse_colors(value: &Value, at: &str) -> Result<HashMap<&'static str, Rgba>, ThemeParseError> {
    let mut colors = HashMap::new();
    for (key, value) in object(value, at)? {
        let Some(key) = known(COLOR_KEYS, key) else {
            return Err(invalid(format!("{at} 里有不认识的颜色 {key}")));
        };
        colors.insert(key, color_value(value, &format!("{at}.{key}"))?);
    }
    for seed in ["bg_base", "text_base"] {
        match colors.get(seed) {
            None => return Err(invalid(format!("{at} 缺少种子色 {seed}"))),
            Some(color) if color.a < 1.0 => {
                return Err(invalid(format!("{at}.{seed} 是种子色，必须不透明")));
            }
            Some(_) => {}
        }
    }
    Ok(colors)
}

fn color_value(value: &Value, at: &str) -> Result<Rgba, ThemeParseError> {
    let Value::String(text) = value else {
        return Err(invalid(format!("{at} 必须是颜色字符串")));
    };
    parse_hex(text).ok_or_else(|| {
        invalid(format!(
            "{at} 的 {text} 不是 #rgb、#rgba、#rrggbb 或 #rrggbbaa"
        ))
    })
}

/// 解析 `#rgb` / `#rgba` / `#rrggbb` / `#rrggbbaa`。别的写法（色名、渐变、前后空白）都不认。
pub(crate) fn parse_hex(text: &str) -> Option<Rgba> {
    let digits = text.strip_prefix('#')?;
    if !matches!(digits.len(), 3 | 4 | 6 | 8) || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Rgba::try_from(text).ok()
}

fn parse_elevation(
    value: &Value,
    at: &str,
) -> Result<HashMap<&'static str, Vec<BoxShadow>>, ThemeParseError> {
    let mut slots = HashMap::new();
    for (key, value) in object(value, at)? {
        let Some(key) = known(ELEVATION_KEYS, key) else {
            return Err(invalid(format!("{at} 里有不认识的投影 {key}")));
        };
        let at = format!("{at}.{key}");
        let Value::Array(layers) = value else {
            return Err(invalid(format!("{at} 必须是数组（[] 表示没有投影）")));
        };
        let shadows = layers
            .iter()
            .enumerate()
            .map(|(index, layer)| parse_shadow(layer, &format!("{at}[{index}]")))
            .collect::<Result<_, _>>()?;
        slots.insert(key, shadows);
    }
    Ok(slots)
}

/// 一层阴影：`{"x", "y", "blur", "spread", "color", "inset"}`，单位是逻辑像素。只有 `color` 必填，数值缺省是 0，`inset` 缺省是 false。
fn parse_shadow(value: &Value, at: &str) -> Result<BoxShadow, ThemeParseError> {
    let layer = object(value, at)?;
    known_keys(layer, &["x", "y", "blur", "spread", "color", "inset"], at)?;
    let number = |key: &str| match layer.get(key) {
        None => Ok(0.0),
        Some(value) => value
            .as_f64()
            .filter(|n| n.is_finite())
            .map(|n| n as f32)
            .ok_or_else(|| invalid(format!("{at}.{key} 必须是数字"))),
    };
    let blur = number("blur")?;
    if blur < 0.0 {
        return Err(invalid(format!("{at}.blur 不能是负数")));
    }
    let color = match layer.get("color") {
        Some(color) => color_value(color, &format!("{at}.color"))?,
        None => return Err(invalid(format!("{at} 缺少 color"))),
    };
    let inset = match layer.get("inset") {
        None => false,
        Some(Value::Bool(inset)) => *inset,
        Some(_) => return Err(invalid(format!("{at}.inset 必须是 true 或 false"))),
    };
    let shadow = BoxShadow::new(px(number("x")?), px(number("y")?), color.into())
        .blur_radius(px(blur))
        .spread_radius(px(number("spread")?));
    Ok(if inset { shadow.inset() } else { shadow })
}

fn parse_syntax(
    value: &Value,
    at: &str,
) -> Result<Vec<(&'static str, SyntaxStyle)>, ThemeParseError> {
    let mut styles = Vec::new();
    for (key, value) in object(value, at)? {
        let Some(key) = known(SYNTAX_KEYS, key) else {
            return Err(invalid(format!("{at} 里有不认识的记号类别 {key}")));
        };
        let at = format!("{at}.{key}");
        let style = object(value, &at)?;
        known_keys(style, &["color", "font_style", "font_weight"], &at)?;
        let color = match style.get("color") {
            Some(color) => Some(color_value(color, &format!("{at}.color"))?),
            None => None,
        };
        let font_style = match style.get("font_style").map(Value::as_str) {
            None => None,
            Some(Some("normal")) => Some(SyntaxFontStyle::Normal),
            Some(Some("italic")) => Some(SyntaxFontStyle::Italic),
            Some(Some("underline")) => Some(SyntaxFontStyle::Underline),
            Some(_) => {
                return Err(invalid(format!(
                    "{at}.font_style 只能是 normal、italic 或 underline"
                )));
            }
        };
        let font_weight = match style.get("font_weight") {
            None => None,
            Some(weight) => match weight.as_u64() {
                Some(weight @ 100..=900) if weight % 100 == 0 => Some(weight as u16),
                _ => {
                    return Err(invalid(format!("{at}.font_weight 只能是 100–900 的整百数")));
                }
            },
        };
        styles.push((
            key,
            SyntaxStyle {
                color,
                font_style,
                font_weight,
            },
        ));
    }
    Ok(styles)
}

/// 深色主题的层从不低于这个 OKLab 明度（约 #0a0a0a）起抬：从纯黑抬 0.04 的明度只是 #010101，
/// 看不出来。
const DARK_FLOOR: f32 = 0.145;
/// 四档层各抬多少明度。深色每档拉开得多：同样的明度差在暗处看着更小，最后一档要撑得起开关
/// 关着的槽。
const DARK_LAYERS: [f32; 4] = [0.06, 0.10, 0.15, 0.275];
const LIGHT_LAYERS: [f32; 4] = [0.015, 0.04, 0.05, 0.11];
/// 窗口最外层比主内容暗多少明度（深浅都往暗走，主内容像一张浮在上面的纸）。
const PAGE_DROP: f32 = 0.02;

const WHITE: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};
const BLACK: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

/// 求值：文件写了的用写的，没写的按 [`ThemeColors`] 字段注释里的规则推，推的时候读的是
/// 已经求出的上游色。`set` 里必有两个种子色（[`parse_colors`] 查过）。
fn resolve_colors(dark: bool, set: &HashMap<&'static str, Rgba>) -> ThemeColors {
    let get = |key: &str, derived: Rgba| set.get(key).copied().unwrap_or(derived);
    let bg = set["bg_base"];
    let fg = set["text_base"];
    let pick = |light: f32, dark_value: f32| if dark { dark_value } else { light };

    let (lab_bg, lab_fg) = (oklab(bg), oklab(fg));
    let chroma = |lab: [f32; 3]| lab[1].hypot(lab[2]);
    let tinted = if chroma(lab_bg) >= chroma(lab_fg) {
        lab_bg
    } else {
        lab_fg
    };
    let tint = [tinted[1], tinted[2]];
    let base = if dark {
        lab_bg[0].max(DARK_FLOOR)
    } else {
        lab_bg[0]
    };
    let toward_fg = if dark { 1.0 } else { -1.0 };
    // 层沿底色到正文色那条线走：明度抬几成，色度就从底色的往正文色的走几成。暗处直接套
    // 正文色的色度会饱和得发脏（米黄字的深色主题，卡片成了深褐）。
    let span = (lab_fg[0] - base).abs().max(f32::EPSILON);
    let lift = |step: f32| {
        let t = (step / span).clamp(0.0, 1.0);
        let toward = |i: usize| lab_bg[i] + (lab_fg[i] - lab_bg[i]) * t;
        from_oklab(
            [
                (base + toward_fg * step).clamp(0.0, 1.0),
                toward(1),
                toward(2),
            ],
            1.0,
        )
    };
    let steps = if dark { DARK_LAYERS } else { LIGHT_LAYERS };
    // 状态浅底从这一层起往状态色混。
    let ground = lift(0.0);
    let ink = |a: f32| alpha(fg, a);

    let page = get(
        "page",
        from_oklab(
            [(lab_bg[0] - PAGE_DROP).max(0.0), lab_bg[1], lab_bg[2]],
            1.0,
        ),
    );
    let bg_deep = get("bg_deep", page);
    let layer_01 = get("layer_01", lift(steps[0]));
    let layer_02 = get("layer_02", lift(steps[1]));
    let layer_03 = get("layer_03", lift(steps[2]));
    let layer_04 = get("layer_04", lift(steps[3]));
    let bg_inverse = get("bg_inverse", fg);
    // 高对比底在两种明暗下都是压白字的深色块：浅色主题用正文色，深色主题用最高一层。
    let bg_contrast = get("bg_contrast", if dark { layer_04 } else { fg });
    let bg_button_neutral = get("bg_button_neutral", if dark { ink(0.06) } else { bg });
    // 深色的图标主按钮是一块亮实色，明度定在 0.89、色相跟正文色：正文色偏暗的主题照着
    // 正文色混，底太灰，按下再压一层黑就压不住深色图标。
    let bg_icon_button_contrast = get(
        "bg_icon_button_contrast",
        if dark {
            from_oklab([0.89, lab_fg[1], lab_fg[2]], 1.0)
        } else {
            bg_contrast
        },
    );
    let bg_accent = get("bg_accent", oklch(0.55, 0.23, 268.0));

    let text_muted = get("text_muted", secondary(bg, fg, tint, pick(0.57, 0.72), 4.5));
    let text_faint = get("text_faint", secondary(bg, fg, tint, pick(0.36, 0.54), 3.0));
    let text_inverse = get("text_inverse", bg);
    let text_contrast = get("text_contrast", if dark { fg } else { bg });
    let text_accent = get("text_accent", readable(bg_accent, bg, 4.5));
    let icon_base = get("icon_base", fg);
    let icon_muted = get("icon_muted", text_muted);
    let icon_faint = get("icon_faint", text_faint);
    let icon_inverse = get("icon_inverse", text_inverse);
    let icon_contrast = get("icon_contrast", text_contrast);
    let icon_accent = get("icon_accent", text_accent);

    let submit_fg = get("submit_fg", if dark { text_inverse } else { text_contrast });
    let border_base = get("border_base", ink(pick(0.10, 0.11)));
    let border_focus = get("border_focus", oklch(0.70, 0.15, 268.0));
    let hover = get("hover", ink(pick(0.04, 0.06)));
    // 圆钮在两种明暗下都是近白的实体，压在强调色和最高一层上都分得出。
    let knob = get("knob", from_oklab([0.96, tint[0], tint[1]], 1.0));
    let surface_raised = get("surface_raised", if dark { lift(0.015) } else { bg });

    let state = |name: &str, hue: f32| {
        let fg = get(&format!("{name}.fg"), oklch(pick(0.5, 0.75), 0.15, hue));
        StateColors {
            bg: get(&format!("{name}.bg"), mix_oklab(ground, fg, 0.12)),
            fg,
            border: get(&format!("{name}.border"), mix_oklab(ground, fg, 0.35)),
            icon: get(&format!("{name}.icon"), fg),
        }
    };
    let chart = [255.0, 150.0, 75.0, 300.0, 0.0];
    let mut index = 0;
    let chart = chart.map(|hue| {
        index += 1;
        get(
            &format!("chart.{index}"),
            oklch(pick(0.58, 0.72), 0.13, hue),
        )
    });

    ThemeColors {
        page,
        bg_base: bg,
        bg_deep,
        layer_01,
        layer_02,
        layer_03,
        layer_04,
        bg_inverse,
        bg_contrast,
        bg_button_neutral,
        bg_icon_button_contrast,
        bg_accent,
        submit_highlight: get("submit_highlight", alpha(WHITE, pick(0.20, 0.60))),
        submit_hover: get(
            "submit_hover",
            if dark {
                alpha(BLACK, 0.08)
            } else {
                alpha(bg, 0.12)
            },
        ),
        submit_fg,
        submit_icon: get("submit_icon", submit_fg),
        text_base: fg,
        text_muted,
        text_faint,
        text_inverse,
        text_contrast,
        text_accent,
        icon_base,
        icon_muted,
        icon_faint,
        icon_inverse,
        icon_contrast,
        icon_accent,
        border_muted: get("border_muted", ink(0.08)),
        border_base,
        border_strong: get("border_strong", ink(0.20)),
        border_focus,
        hover,
        pressed: get("pressed", ink(pick(0.08, 0.10))),
        // 浅色的高对比底是深色块，悬停往亮提；深色的是灰块，往暗压。
        contrast_hover: get(
            "contrast_hover",
            if dark {
                alpha(BLACK, 0.24)
            } else {
                alpha(bg, 0.12)
            },
        ),
        contrast_pressed: get(
            "contrast_pressed",
            if dark {
                alpha(BLACK, 0.40)
            } else {
                alpha(bg, 0.24)
            },
        ),
        scrim: get("scrim", alpha(BLACK, pick(0.40, 0.60))),
        depth_top: get("depth_top", WHITE),
        depth_bottom: get("depth_bottom", alpha(WHITE, 0.0)),
        white: get("white", WHITE),
        black: get("black", BLACK),
        knob,
        knob_on: get("knob_on", from_oklab([0.95, tint[0], tint[1]], 1.0)),
        success: state("success", 150.0),
        warning: state("warning", 80.0),
        danger: state("danger", 25.0),
        info: state("info", 256.0),
        surface_raised,
        selection: get("selection", alpha(text_accent, 0.30)),
        chart,
    }
}

/// 没写的投影按中性几何推，见 [`Elevation`]。`ring` 是外圈线的颜色。
fn resolve_elevation(
    dark: bool,
    ring: Rgba,
    mut set: HashMap<&'static str, Vec<BoxShadow>>,
) -> Elevation {
    let pick = |light: f32, dark_value: f32| if dark { dark_value } else { light };
    let cast = |y: f32, blur: f32, a: f32| {
        BoxShadow::new(px(0.), px(y), alpha(BLACK, a).into()).blur_radius(px(blur))
    };
    // 浮起是一远一近两层：远的下移、模糊都是近的两倍，更淡。深色底吃阴影，压得更重。
    let lift = |y: f32| {
        vec![
            cast(y * 2.0, y * 4.0, pick(0.04, 0.30)),
            cast(y, y * 2.0, pick(0.08, 0.30)),
        ]
    };
    let outline = BoxShadow::new(px(0.), px(0.), ring.into()).spread_radius(px(0.5));
    let ringed = |mut layers: Vec<BoxShadow>| {
        layers.push(outline.clone());
        layers
    };
    let groove = vec![outline.clone().inset()];
    let mut take = |key: &str, derived: Vec<BoxShadow>| set.remove(key).unwrap_or(derived);
    let button_neutral = take(
        "button_neutral",
        ringed(vec![cast(1.0, 1.5, pick(0.10, 0.40))]),
    );
    Elevation {
        raised: take("raised", ringed(lift(1.0))),
        floating: take("floating", ringed(lift(4.0))),
        overlay: take("overlay", ringed(lift(8.0))),
        button_contrast: take("button_contrast", vec![cast(1.0, 1.5, pick(0.20, 0.40))]),
        elements: take("elements", vec![cast(0.5, 0.5, 0.40)]),
        switch_off: take("switch_off", groove.clone()),
        switch_on: take("switch_on", groove),
        md: take("md", lift(3.0)),
        knob: take("knob", vec![cast(1.0, 3.0, pick(0.08, 0.30))]),
        input: take("input", button_neutral.clone()),
        button_neutral,
    }
}

// ---- 颜色运算：sRGB ↔ OKLab（Björn Ottosson 的矩阵），对比度按 WCAG 2 ----

fn alpha(color: Rgba, a: f32) -> Rgba {
    Rgba { a, ..color }
}

fn decode(channel: f32) -> f32 {
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn encode(channel: f32) -> f32 {
    if channel <= 0.003_130_8 {
        channel * 12.92
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    }
}

fn oklab(color: Rgba) -> [f32; 3] {
    let [r, g, b] = [color.r, color.g, color.b].map(decode);
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// OKLab 换回 sRGB，出色域的通道夹到 0–1。
fn from_oklab([lightness, a, b]: [f32; 3], opacity: f32) -> Rgba {
    let l = (lightness + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m = (lightness - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s = (lightness - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    let channel = |linear: f32| encode(linear.clamp(0.0, 1.0));
    Rgba {
        r: channel(4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s),
        g: channel(-1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s),
        b: channel(-0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s),
        a: opacity.clamp(0.0, 1.0),
    }
}

fn oklch(lightness: f32, chroma: f32, hue: f32) -> Rgba {
    let (sin, cos) = hue.to_radians().sin_cos();
    from_oklab([lightness, chroma * cos, chroma * sin], 1.0)
}

/// 在 OKLab 里从 `from` 往 `to` 走 `t`，透明度同样插值。
fn mix_oklab(from: Rgba, to: Rgba, t: f32) -> Rgba {
    let (a, b) = (oklab(from), oklab(to));
    from_oklab(
        std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t),
        from.a + (to.a - from.a) * t,
    )
}

fn luminance(color: Rgba) -> f32 {
    0.2126 * decode(color.r) + 0.7152 * decode(color.g) + 0.0722 * decode(color.b)
}

fn contrast(a: Rgba, b: Rgba) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// 次要字：在 OKLab 明度上从 `bg` 往 `fg` 走 `amount`（色相取 `tint`），对比不到 `minimum`
/// 时二分往前走到刚好够；`fg` 自己都不够就是 `fg`。
/// `color` 压在 `bg` 上对比不到 `minimum` 时，保持色相和彩度，把 OKLab 明度往远离 `bg` 的
/// 一头挪到刚好够（挪到头也不够就停在头上）。
fn readable(color: Rgba, bg: Rgba, minimum: f32) -> Rgba {
    if contrast(bg, color) >= minimum {
        return color;
    }
    let [lightness, a, b] = oklab(color);
    let end = if luminance(bg) > 0.18 { 0.0 } else { 1.0 };
    let at = |t: f32| from_oklab([lightness + (end - lightness) * t, a, b], color.a);
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..24 {
        let mid = (low + high) * 0.5;
        if contrast(bg, at(mid)) >= minimum {
            high = mid;
        } else {
            low = mid;
        }
    }
    at(high)
}

fn secondary(bg: Rgba, fg: Rgba, tint: [f32; 2], amount: f32, minimum: f32) -> Rgba {
    let (lb, lf) = (oklab(bg)[0], oklab(fg)[0]);
    let mix = |t: f32| from_oklab([lb + (lf - lb) * t, tint[0], tint[1]], 1.0);
    let candidate = mix(amount);
    if contrast(bg, candidate) >= minimum {
        return candidate;
    }
    if contrast(bg, fg) < minimum {
        return fg;
    }
    let (mut low, mut high) = (amount, 1.0);
    for _ in 0..24 {
        let mid = (low + high) * 0.5;
        if contrast(bg, mix(mid)) >= minimum {
            high = mid;
        } else {
            low = mid;
        }
    }
    mix(high)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(color: Rgba) -> u32 {
        let q = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u32;
        (q(color.r) << 24) | (q(color.g) << 16) | (q(color.b) << 8) | q(color.a)
    }

    /// 一份只有一款主题的文件，`theme` 是这款主题除名字外的字段。
    fn file(theme: serde_json::Value) -> String {
        let mut theme = theme;
        theme["name"] = "T".into();
        serde_json::json!({ "version": 1, "name": "t", "themes": [theme] }).to_string()
    }

    fn seeds(mode: &str) -> serde_json::Value {
        serde_json::json!({
            "mode": mode,
            "colors": { "bg_base": "#1e1e1d", "text_base": "#ecece8" }
        })
    }

    fn parse_one(theme: serde_json::Value) -> ThemeConfig {
        parse_theme_set(&file(theme))
            .unwrap()
            .themes
            .into_iter()
            .next()
            .unwrap()
    }

    #[test]
    fn parses_only_strict_hex() {
        assert_eq!(parse_hex("#336699").map(hex), Some(0x336699ff));
        assert_eq!(parse_hex("#33669980").map(hex), Some(0x33669980));
        assert_eq!(parse_hex("#fff").map(hex), Some(0xffffffff));
        for bad in [
            "blue-600",
            "white",
            " #336699",
            "#33669",
            "#gggggg",
            "linear-gradient(#000, #fff)",
        ] {
            assert_eq!(parse_hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn rejects_files_that_break_the_format() {
        let ok = file(seeds("dark"));
        assert!(parse_theme_set(&ok).is_ok());
        let without_version = serde_json::json!({ "name": "t", "themes": [] }).to_string();
        let future = ok.replace("\"version\":1", "\"version\":2");
        assert_ne!(future, ok);
        let mut cases = vec![
            "{".to_string(),
            without_version,
            future,
            ok.replace("\"name\":\"t\"", "\"name\":\"t\",\"extra\":1"),
        ];
        let mut bad_theme = |patch: &dyn Fn(&mut serde_json::Value)| {
            let mut theme = seeds("dark");
            patch(&mut theme);
            cases.push(file(theme));
        };
        bad_theme(&|t| t["mode"] = "dim".into());
        bad_theme(&|t| {
            t.as_object_mut().unwrap().remove("mode");
        });
        bad_theme(&|t| t["radius"] = 4.into());
        bad_theme(&|t| t["colors"]["background"] = "#000000".into());
        bad_theme(&|t| t["colors"]["layer_01"] = "blue-600".into());
        bad_theme(&|t| t["colors"]["layer_01"] = 1.into());
        bad_theme(&|t| {
            t["colors"].as_object_mut().unwrap().remove("bg_base");
        });
        bad_theme(&|t| t["colors"]["text_base"] = "#ecece880".into());
        bad_theme(&|t| t["elevation"] = serde_json::json!({ "shadow": [] }));
        bad_theme(&|t| t["elevation"] = serde_json::json!({ "md": [{ "y": 1 }] }));
        bad_theme(&|t| t["elevation"] = serde_json::json!({ "md": [{ "color": "#000", "z": 1 }] }));
        bad_theme(&|t| t["elevation"] = serde_json::json!({ "md": "none" }));
        bad_theme(&|t| t["syntax"] = serde_json::json!({ "keywords": {} }));
        bad_theme(&|t| t["syntax"] = serde_json::json!({ "keyword": { "font_weight": 450 } }));
        bad_theme(&|t| t["syntax"] = serde_json::json!({ "keyword": { "font_style": "bold" } }));
        for case in cases {
            assert!(parse_theme_set(&case).is_err(), "应当不收：{case}");
        }
    }

    /// 只给种子色：每个色都按字段注释里的规则推出来。
    #[test]
    fn seeds_alone_derive_every_token() {
        for mode in ["dark", "light"] {
            let mut theme = seeds(mode);
            if mode == "light" {
                theme["colors"] =
                    serde_json::json!({ "bg_base": "#ffffff", "text_base": "#1a1a18" });
            }
            let config = parse_one(theme);
            let c = config.colors();
            let dark = mode == "dark";
            // 窗口最外层深浅都比主内容暗。
            assert!(luminance(c.page) < luminance(c.bg_base), "{mode}");
            assert_eq!(c.bg_deep, c.page);
            // 高对比底两种明暗下都压浅色字。
            if dark {
                assert_eq!(c.bg_contrast, c.layer_04);
                assert_eq!(c.text_contrast, c.text_base);
                assert_eq!(c.bg_button_neutral, alpha(c.text_base, 0.06));
                assert!(luminance(c.surface_raised) > luminance(c.bg_base));
            } else {
                assert_eq!(c.bg_contrast, c.text_base);
                assert_eq!(c.text_contrast, c.bg_base);
                assert_eq!(c.bg_button_neutral, c.bg_base);
                assert_eq!(c.surface_raised, c.bg_base);
            }
            assert!(contrast(c.text_contrast, c.bg_contrast) >= 4.5, "{mode}");
            assert_eq!(c.bg_accent, oklch(0.55, 0.23, 268.0));
            assert!(contrast(c.text_accent, c.bg_base) >= 4.5, "{mode}");
            assert_eq!(c.selection, alpha(c.text_accent, 0.30));
            // 圆钮近白，压在强调色和最高一层上都分得出。
            for track in [c.bg_accent, c.layer_04] {
                assert!(luminance(c.knob) > luminance(track), "{mode}");
            }
            assert!(luminance(c.knob_on) > luminance(c.bg_accent), "{mode}");
            assert_eq!(
                c.border_base,
                alpha(c.text_base, if dark { 0.11 } else { 0.10 })
            );
            assert_eq!(c.hover, alpha(c.text_base, if dark { 0.06 } else { 0.04 }));
            assert_eq!(c.success.icon, c.success.fg);
            // 层一档档往前景走。
            let layers = [c.bg_base, c.layer_01, c.layer_02, c.layer_03, c.layer_04].map(luminance);
            assert!(
                layers.windows(2).all(|pair| (pair[1] > pair[0]) == dark),
                "{mode}: {layers:?}"
            );
            assert!(contrast(c.text_muted, c.bg_base) >= 4.5, "{mode}");
            assert!(contrast(c.text_faint, c.bg_base) >= 3.0, "{mode}");
            assert!(contrast(c.text_muted, c.bg_base) > contrast(c.text_faint, c.bg_base));
            let floating = &config.elevation().floating;
            assert_eq!(floating.len(), 3);
            assert_eq!(floating[2].spread_radius, px(0.5));
            assert_eq!(
                hex(floating[2].color.into()),
                hex(alpha(c.text_base, if dark { 0.16 } else { 0.12 }))
            );
            assert_eq!(config.elevation().input, config.elevation().button_neutral);
        }
    }

    /// 写了的色照写的用，下游没写的色跟着它推。
    #[test]
    fn explicit_tokens_win_and_flow_downstream() {
        let mut theme = seeds("dark");
        theme["colors"]["layer_04"] = "#505050".into();
        theme["colors"]["text_muted"] = "#999999".into();
        theme["colors"]["success.fg"] = "#00ff00".into();
        theme["colors"]["text_accent"] = "#3366ff".into();
        theme["colors"]["chart.2"] = "#123456".into();
        let config = parse_one(theme);
        let c = config.colors();
        for color in [c.layer_04, c.bg_contrast] {
            assert_eq!(hex(color), 0x505050ff);
        }
        for color in [c.text_muted, c.icon_muted] {
            assert_eq!(hex(color), 0x999999ff);
        }
        assert_eq!(hex(c.success.icon), 0x00ff00ff);
        assert_eq!(hex(c.icon_accent), 0x3366ffff);
        assert_eq!(c.selection, alpha(c.text_accent, 0.30));
        assert_eq!(hex(c.chart[1]), 0x123456ff);
    }

    /// 认得的每个颜色键都有地方读：写了它，求出来的色就变。
    #[test]
    fn every_color_key_is_read() {
        let baseline = *parse_one(seeds("dark")).colors();
        for key in COLOR_KEYS {
            let mut theme = seeds("dark");
            theme["colors"][*key] = "#010203".into();
            assert_ne!(*parse_one(theme).colors(), baseline, "{key} 没被读到");
        }
    }

    #[test]
    fn elevation_slots_parse_and_fall_back() {
        let mut theme = seeds("light");
        theme["elevation"] = serde_json::json!({
            "floating": [],
            "raised": [{ "x": 1, "y": 2, "blur": 3, "spread": 4, "color": "#ff000080", "inset": true }]
        });
        let config = parse_one(theme);
        let elevation = config.elevation();
        assert!(elevation.floating.is_empty());
        let raised = &elevation.raised[0];
        assert_eq!(raised.offset, gpui::point(px(1.), px(2.)));
        assert_eq!((raised.blur_radius, raised.spread_radius), (px(3.), px(4.)));
        assert!(raised.inset);
        assert_eq!(hex(raised.color.into()), 0xff000080);
        assert_eq!(elevation.md.len(), 2, "没写的档按中性几何推");
    }

    #[test]
    fn syntax_styles_parse() {
        let mut theme = seeds("dark");
        theme["syntax"] = serde_json::json!({
            "keyword": { "color": "#d73a49", "font_weight": 600 },
            "comment": { "font_style": "italic" }
        });
        let config = parse_one(theme);
        let style = |key: &str| config.syntax().iter().find(|(k, _)| *k == key).unwrap().1;
        assert_eq!(style("keyword").color.map(hex), Some(0xd73a49ff));
        assert_eq!(style("keyword").font_weight, Some(600));
        assert_eq!(style("comment").font_style, Some(SyntaxFontStyle::Italic));
    }

    #[test]
    fn oklab_round_trips() {
        for color in [
            0x1e1e1dff_u32,
            0x3a6ee1ff,
            0xffffffff,
            0x000000ff,
            0xc8372dff,
        ] {
            let rgba = gpui::rgba(color);
            assert_eq!(hex(from_oklab(oklab(rgba), 1.0)), color);
        }
    }

    #[test]
    fn bundled_smelt_theme_parses() {
        let set = parse_theme_set(include_str!("../themes/smelt.json")).unwrap();
        assert_eq!(set.themes.len(), 2);
    }
}
