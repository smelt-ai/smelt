//! 外观设置的可脚本目录。GUI、CLI 和 Control API 只改这里登记过的 key。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::sqlite_state;

pub const THEME_MODE: &str = "appearance.theme_mode";
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

pub fn get_settings_on(store: &smelt_store::Store) -> Result<SettingsResult, SettingsError> {
    Ok(SettingsResult {
        settings: snapshot_to_map(load_snapshot(store)?),
    })
}

pub fn update_settings_on(
    store: &smelt_store::Store,
    patch: SettingsPatch,
) -> Result<SettingsResult, SettingsError> {
    let mut snapshot = load_snapshot(store)?;
    if patch.settings.is_empty() {
        return Err(SettingsError::Invalid("没有要更新的设置".into()));
    }
    for (key, value) in &patch.settings {
        apply_key(&mut snapshot, key, value)?;
    }
    store
        .put_appearance_snapshot(&snapshot)
        .map_err(|error| SettingsError::Store(error.to_string()))?;
    Ok(SettingsResult {
        settings: snapshot_to_map(snapshot),
    })
}

pub fn get_settings() -> Result<SettingsResult, SettingsError> {
    get_settings_on(&default_store()?)
}

pub fn update_settings(patch: SettingsPatch) -> Result<SettingsResult, SettingsError> {
    update_settings_on(&default_store()?, patch)
}

fn default_store() -> Result<smelt_store::Store, SettingsError> {
    sqlite_state::default_sqlite_store().map_err(SettingsError::Store)
}

fn load_snapshot(
    store: &smelt_store::Store,
) -> Result<smelt_store::AppearanceSnapshot, SettingsError> {
    match store.get_appearance_snapshot() {
        Ok(Some(snapshot)) => Ok(snapshot),
        Ok(None) => Ok(default_snapshot()),
        Err(error) => Err(SettingsError::Store(error.to_string())),
    }
}

fn default_snapshot() -> smelt_store::AppearanceSnapshot {
    smelt_store::AppearanceSnapshot {
        bg_color: 0x1a1b26,
        bg_image: None,
        bg_image_opacity: 0.25,
        opacity: 0.95,
        blur: true,
        glass_style: "regular".into(),
        theme_mode: "dark".into(),
        ui_font_px: 16,
        ui_font_family: String::new(),
        font_px: 13,
        font_family: String::new(),
    }
}

fn snapshot_to_map(snapshot: smelt_store::AppearanceSnapshot) -> BTreeMap<String, Value> {
    let mut settings = BTreeMap::new();
    settings.insert(THEME_MODE.into(), json!(snapshot.theme_mode));
    settings.insert(UI_FONT_PX.into(), json!(snapshot.ui_font_px));
    settings.insert(UI_FONT_FAMILY.into(), json!(snapshot.ui_font_family));
    settings.insert(FONT_PX.into(), json!(snapshot.font_px));
    settings.insert(FONT_FAMILY.into(), json!(snapshot.font_family));
    settings.insert(OPACITY.into(), json!(snapshot.opacity));
    settings
}

fn apply_key(
    snapshot: &mut smelt_store::AppearanceSnapshot,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn open_store() -> (tempfile::TempDir, smelt_store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let store =
            smelt_store::Store::open_or_create(dir.path().join(smelt_store::DATABASE_FILE_NAME))
                .unwrap();
        (dir, store)
    }

    #[test]
    fn update_rejects_unknown_keys_and_keeps_known_values() {
        let (_dir, store) = open_store();
        let error = update_settings_on(
            &store,
            SettingsPatch {
                settings: BTreeMap::from([("appearance.nope".into(), json!("x"))]),
            },
        )
        .unwrap_err();
        assert!(matches!(error, SettingsError::Invalid(_)));

        let updated = update_settings_on(
            &store,
            SettingsPatch {
                settings: BTreeMap::from([
                    (THEME_MODE.into(), json!("light")),
                    (UI_FONT_PX.into(), json!(18)),
                ]),
            },
        )
        .unwrap();
        assert_eq!(updated.settings[THEME_MODE], "light");
        assert_eq!(updated.settings[UI_FONT_PX], 18);
        assert_eq!(
            get_settings_on(&store).unwrap().settings[THEME_MODE],
            "light"
        );
    }
}
