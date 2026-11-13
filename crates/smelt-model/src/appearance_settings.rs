//! 外观设置的可脚本目录：允许的键、取值范围、怎么套到一份外观上。外观归客户端，
//! 运行时只按这里的规则校验命令行发来的改动再转给桌面。

/// 一份完整外观。字段名和桌面的外观结构一致，取值规则见 [`apply_patch`]。
#[derive(Clone, Debug, PartialEq)]
pub struct AppearanceSnapshot {
    pub bg_color: u32,
    pub bg_image: Option<String>,
    pub bg_image_opacity: f32,
    pub opacity: f32,
    pub blur: bool,
    pub glass_style: String,
    pub theme_mode: String,
    /// 主题注册名，例如 `Default Dark`、`Ayu Light`。
    pub theme_name: String,
    pub ui_font_px: u32,
    pub ui_font_family: String,
    pub font_px: u32,
    pub font_family: String,
}

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub use crate::theme_index::ThemeEntry;

pub const THEME_MODE: &str = "appearance.theme_mode";

pub const THEME_NAME: &str = "appearance.theme";

pub const UI_FONT_PX: &str = "appearance.ui_font_px";

pub const UI_FONT_FAMILY: &str = "appearance.ui_font_family";

pub const FONT_PX: &str = "appearance.font_px";

pub const FONT_FAMILY: &str = "appearance.font_family";

pub const OPACITY: &str = "appearance.opacity";

pub const MIN_UI_FONT_PX: u32 = 14;

pub const MAX_UI_FONT_PX: u32 = 26;

pub const MIN_FONT_PX: u32 = 9;

pub const MAX_FONT_PX: u32 = 22;

pub const MIN_OPACITY: f32 = 0.3;

pub const MAX_OPACITY: f32 = 1.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsError {
    Invalid(String),
    Store(String),
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::Store(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SettingsError {}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingsPatch {
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingsResult {
    pub settings: BTreeMap<String, Value>,
}

/// 一次 `settings.update` 拆开后的结果：记忆配置已经写进运行时的库；外观要转给
/// 桌面客户端去改，由调用方发出去。
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsUpdate {
    pub result: SettingsResult,
    /// 校验过的外观改动，原样转给客户端。没有外观键时为空。
    pub appearance: BTreeMap<String, Value>,
}

/// 只校验外观改动的形状：键名是否登记过、取值类型和范围。不认主题目录，
/// 所以 `appearance.theme` 只查是不是一个像样的名字（非空、不超过 80 字节），
/// 主题是否存在留给持有目录的桌面在 [`apply_patch`] 里判定。运行时用它挡掉坏请求。
pub fn validate_patch(patch: &BTreeMap<String, Value>) -> Result<(), SettingsError> {
    for (key, value) in patch {
        parse_change(key, value)?;
    }
    Ok(())
}

/// 把一组外观改动套到快照上，主题名和同族明暗按 `catalog`（桌面当前注册的主题，
/// 含插件主题）解析。原子：任何一个键不合法（含目录里没有的主题、目录条目明暗
/// 不是 dark / light）都返回错误，`snapshot` 保持原样。
///
/// 键按名字顺序套用，`appearance.theme` 在 `appearance.theme_mode` 之前：两个
/// 同时给时，先选主题，再拨到该主题同族的指定明暗。
pub fn apply_patch(
    snapshot: &mut AppearanceSnapshot,
    patch: &BTreeMap<String, Value>,
    catalog: &[ThemeEntry],
) -> Result<(), SettingsError> {
    let mut next = snapshot.clone();
    for (key, value) in patch {
        apply_change(&mut next, parse_change(key, value)?, catalog)?;
    }
    *snapshot = next;
    Ok(())
}

/// 库里还没有外观记录时的默认值，GUI 与 CLI / Control API 共用这一份。
pub fn default_snapshot() -> AppearanceSnapshot {
    AppearanceSnapshot {
        bg_color: 0x1a1b26,
        bg_image: None,
        bg_image_opacity: 0.25,
        opacity: 0.95,
        blur: true,
        glass_style: "regular".into(),
        theme_mode: "dark".into(),
        theme_name: crate::theme_index::DEFAULT_DARK.into(),
        ui_font_px: 16,
        ui_font_family: String::new(),
        font_px: 13,
        font_family: String::new(),
    }
}

/// 一个键校验过形状后的改动。
enum Change {
    ThemeMode(&'static str),
    ThemeName(String),
    UiFontPx(u32),
    UiFontFamily(String),
    FontPx(u32),
    FontFamily(String),
    Opacity(f32),
}

fn parse_change(key: &str, value: &Value) -> Result<Change, SettingsError> {
    Ok(match key {
        THEME_MODE => match value.as_str().map(str::trim) {
            Some("dark") => Change::ThemeMode("dark"),
            Some("light") => Change::ThemeMode("light"),
            _ => {
                return Err(SettingsError::Invalid(
                    "appearance.theme_mode 只能是 dark 或 light".into(),
                ));
            }
        },
        THEME_NAME => {
            let name = value
                .as_str()
                .map(str::trim)
                .filter(|name| !name.is_empty() && name.len() <= 80)
                .ok_or_else(|| SettingsError::Invalid("appearance.theme 必须是主题名".into()))?;
            Change::ThemeName(name.to_string())
        }
        UI_FONT_PX => Change::UiFontPx(u32_in(value, UI_FONT_PX, MIN_UI_FONT_PX, MAX_UI_FONT_PX)?),
        UI_FONT_FAMILY => Change::UiFontFamily(string_value(value, UI_FONT_FAMILY)?),
        FONT_PX => Change::FontPx(u32_in(value, FONT_PX, MIN_FONT_PX, MAX_FONT_PX)?),
        FONT_FAMILY => Change::FontFamily(string_value(value, FONT_FAMILY)?),
        OPACITY => {
            let opacity = value
                .as_f64()
                .ok_or_else(|| SettingsError::Invalid("appearance.opacity 必须是数字".into()))?
                as f32;
            if !opacity.is_finite() || !(MIN_OPACITY..=MAX_OPACITY).contains(&opacity) {
                return Err(SettingsError::Invalid(format!(
                    "appearance.opacity 必须在 {MIN_OPACITY} 和 {MAX_OPACITY} 之间"
                )));
            }
            Change::Opacity(opacity)
        }
        other => {
            return Err(SettingsError::Invalid(format!("未知设置: {other}")));
        }
    })
}

fn apply_change(
    snapshot: &mut AppearanceSnapshot,
    change: Change,
    catalog: &[ThemeEntry],
) -> Result<(), SettingsError> {
    match change {
        Change::ThemeMode(mode) => {
            snapshot.theme_mode = mode.to_string();
            snapshot.theme_name =
                crate::theme_index::sibling_in(catalog, &snapshot.theme_name, mode);
        }
        Change::ThemeName(name) => {
            let entry = crate::theme_index::find_in(catalog, &name)
                .ok_or_else(|| SettingsError::Invalid(format!("未知主题: {name}")))?;
            if entry.mode != "dark" && entry.mode != "light" {
                return Err(SettingsError::Invalid(format!(
                    "主题 {name} 的明暗不是 dark 或 light: {}",
                    entry.mode
                )));
            }
            snapshot.theme_name = entry.name.clone();
            snapshot.theme_mode = entry.mode.clone();
        }
        Change::UiFontPx(px) => snapshot.ui_font_px = px,
        Change::UiFontFamily(family) => snapshot.ui_font_family = family,
        Change::FontPx(px) => snapshot.font_px = px,
        Change::FontFamily(family) => snapshot.font_family = family,
        Change::Opacity(opacity) => snapshot.opacity = opacity,
    }
    Ok(())
}

fn u32_in(value: &Value, key: &str, min: u32, max: u32) -> Result<u32, SettingsError> {
    let number = value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
        .ok_or_else(|| SettingsError::Invalid(format!("{key} 必须是整数")))?;
    let number =
        u32::try_from(number).map_err(|_| SettingsError::Invalid(format!("{key} 超出范围")))?;
    if number < min || number > max {
        return Err(SettingsError::Invalid(format!(
            "{key} 必须在 {min} 和 {max} 之间"
        )));
    }
    Ok(number)
}

fn string_value(value: &Value, key: &str) -> Result<String, SettingsError> {
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| SettingsError::Invalid(format!("{key} 必须是字符串")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog() -> Vec<ThemeEntry> {
        vec![
            ThemeEntry::new("Default Dark", "dark", "Default"),
            ThemeEntry::new("Default Light", "light", "Default"),
            ThemeEntry::new("Plug Night", "dark", "Plug"),
            ThemeEntry::new("Plug Day", "light", "Plug"),
            ThemeEntry::new("Weird", "sepia", "Weird"),
        ]
    }

    fn patch(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn validate_checks_shape_but_not_theme_existence() {
        validate_patch(&patch(&[(THEME_NAME, json!("Any Plugin Theme"))])).unwrap();
        validate_patch(&patch(&[(THEME_MODE, json!(" light "))])).unwrap();
        for bad in [
            patch(&[(THEME_NAME, json!(""))]),
            patch(&[(THEME_NAME, json!("x".repeat(81)))]),
            patch(&[(THEME_MODE, json!("sepia"))]),
            patch(&[(UI_FONT_PX, json!(MAX_UI_FONT_PX + 1))]),
            patch(&[(FONT_PX, json!(-1))]),
            patch(&[(OPACITY, json!(0.1))]),
            patch(&[(FONT_FAMILY, json!(1))]),
            patch(&[("appearance.nope", json!(1))]),
        ] {
            assert!(
                matches!(validate_patch(&bad), Err(SettingsError::Invalid(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn plugin_theme_resolves_from_supplied_catalog() {
        let mut snapshot = default_snapshot();
        apply_patch(
            &mut snapshot,
            &patch(&[(THEME_NAME, json!("Plug Day"))]),
            &catalog(),
        )
        .unwrap();
        assert_eq!(snapshot.theme_name, "Plug Day");
        assert_eq!(snapshot.theme_mode, "light");

        apply_patch(
            &mut snapshot,
            &patch(&[(THEME_MODE, json!("dark"))]),
            &catalog(),
        )
        .unwrap();
        assert_eq!(snapshot.theme_name, "Plug Night");
        assert_eq!(snapshot.theme_mode, "dark");
    }

    #[test]
    fn theme_then_mode_in_one_patch_picks_the_family_sibling() {
        let mut snapshot = default_snapshot();
        apply_patch(
            &mut snapshot,
            &patch(&[
                (THEME_NAME, json!("Plug Night")),
                (THEME_MODE, json!("light")),
            ]),
            &catalog(),
        )
        .unwrap();
        assert_eq!(snapshot.theme_name, "Plug Day");
        assert_eq!(snapshot.theme_mode, "light");
    }

    #[test]
    fn unknown_theme_or_bad_catalog_mode_is_rejected_atomically() {
        for bad in ["Ayu Dark", "Weird"] {
            let mut snapshot = default_snapshot();
            let error = apply_patch(
                &mut snapshot,
                &patch(&[(THEME_NAME, json!(bad)), (UI_FONT_PX, json!(20))]),
                &catalog(),
            )
            .unwrap_err();
            assert!(matches!(error, SettingsError::Invalid(_)));
            assert_eq!(snapshot, default_snapshot(), "失败的补丁不能改动快照");
        }
    }

    #[test]
    fn mode_flip_from_theme_missing_in_catalog_falls_back_to_default() {
        let mut snapshot = default_snapshot();
        snapshot.theme_name = "Uninstalled Plugin Theme".into();
        apply_patch(
            &mut snapshot,
            &patch(&[(THEME_MODE, json!("light"))]),
            &catalog(),
        )
        .unwrap();
        assert_eq!(snapshot.theme_name, crate::theme_index::DEFAULT_LIGHT);
    }
}
