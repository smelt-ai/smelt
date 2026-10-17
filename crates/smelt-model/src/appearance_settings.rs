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

use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// 把一组外观改动按可脚本目录的规则套到快照上。客户端收到转来的改动时也用它，
/// 运行时和客户端走同一份校验。
pub fn apply_patch(
    snapshot: &mut AppearanceSnapshot,
    patch: &BTreeMap<String, Value>,
) -> Result<(), SettingsError> {
    for (key, value) in patch {
        apply_key(snapshot, key, value)?;
    }
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

fn apply_key(
    snapshot: &mut AppearanceSnapshot,
    key: &str,
    value: &Value,
) -> Result<(), SettingsError> {
    match key {
        THEME_MODE => {
            let mode = value
                .as_str()
                .map(str::trim)
                .filter(|mode| *mode == "dark" || *mode == "light")
                .ok_or_else(|| {
                    SettingsError::Invalid("appearance.theme_mode 只能是 dark 或 light".into())
                })?;
            snapshot.theme_mode = mode.to_string();
            snapshot.theme_name = crate::theme_index::sibling(&snapshot.theme_name, mode);
        }
        THEME_NAME => {
            let name = value
                .as_str()
                .map(str::trim)
                .filter(|name| !name.is_empty() && name.len() <= 80)
                .ok_or_else(|| SettingsError::Invalid("appearance.theme 必须是主题名".into()))?;
            let entry = crate::theme_index::find(name)
                .ok_or_else(|| SettingsError::Invalid(format!("未知主题: {name}")))?;
            snapshot.theme_name = entry.name.clone();
            snapshot.theme_mode = entry.mode.clone();
        }
        UI_FONT_PX => {
            snapshot.ui_font_px = u32_in(value, UI_FONT_PX, MIN_UI_FONT_PX, MAX_UI_FONT_PX)?;
        }
        UI_FONT_FAMILY => {
            snapshot.ui_font_family = string_value(value, UI_FONT_FAMILY)?;
        }
        FONT_PX => {
            snapshot.font_px = u32_in(value, FONT_PX, MIN_FONT_PX, MAX_FONT_PX)?;
        }
        FONT_FAMILY => {
            snapshot.font_family = string_value(value, FONT_FAMILY)?;
        }
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
            snapshot.opacity = opacity;
        }
        other => {
            return Err(SettingsError::Invalid(format!("未知设置: {other}")));
        }
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
