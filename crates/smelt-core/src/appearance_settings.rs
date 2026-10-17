//! 外观设置的可脚本目录。GUI、CLI 和 Control API 只改这里登记过的 key。
//!
//! 外观归客户端，存在客户端自己的库里。CLI / Control API 发来的外观改动由运行时在这
//! 里校验，再转给在线的桌面客户端去改；记忆的向量检索配置归运行时，直接写库。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::sqlite_state;

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

impl From<smelt_store::StoreError> for SettingsError {
    fn from(error: smelt_store::StoreError) -> Self {
        Self::Store(error.to_string())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingsPatch {
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingsResult {
    pub settings: BTreeMap<String, Value>,
}

/// 运行时这边能直接回答的设置：记忆的向量检索配置。外观归客户端（设计文档 D1），
/// 运行时不存，也就不回显。
pub fn get_settings_on(store: &smelt_store::Store) -> Result<SettingsResult, SettingsError> {
    Ok(SettingsResult {
        settings: crate::memory_settings::to_map(
            &crate::memory_settings::load_on(store)?,
            crate::memory_settings::load_rerank_model_on(store)?.as_ref(),
        ),
    })
}

/// 一次 `settings.update` 拆开后的结果：记忆配置已经写进运行时的库；外观要转给
/// 桌面客户端去改，由调用方发出去。
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsUpdate {
    pub result: SettingsResult,
    /// 校验过的外观改动，原样转给客户端。没有外观键时为空。
    pub appearance: BTreeMap<String, Value>,
}

pub fn update_settings_on(
    store: &smelt_store::Store,
    patch: SettingsPatch,
) -> Result<SettingsUpdate, SettingsError> {
    if patch.settings.is_empty() {
        return Err(SettingsError::Invalid("没有要更新的设置".into()));
    }
    let (memory, appearance): (BTreeMap<_, _>, BTreeMap<_, _>) = patch
        .settings
        .into_iter()
        .partition(|(key, _)| crate::memory_settings::is_memory_key(key));
    // 外观先在出厂外观上演算一遍：键名、取值范围、主题名在这里就拒掉，坏请求不转给
    // 客户端；也保证一个补丁要么整体生效，要么什么都没写。
    if !appearance.is_empty() {
        apply_patch(&mut default_snapshot(), &appearance)?;
    }
    for (key, value) in &memory {
        crate::memory_settings::apply_on(store, key, value).map_err(SettingsError::Invalid)?;
    }
    Ok(SettingsUpdate {
        result: get_settings_on(store)?,
        appearance,
    })
}

/// 把一组外观改动按可脚本目录的规则套到快照上。客户端收到转来的改动时也用它，
/// 运行时和客户端走同一份校验。
pub fn apply_patch(
    snapshot: &mut smelt_store::AppearanceSnapshot,
    patch: &BTreeMap<String, Value>,
) -> Result<(), SettingsError> {
    for (key, value) in patch {
        apply_key(snapshot, key, value)?;
    }
    Ok(())
}

pub fn get_settings() -> Result<SettingsResult, SettingsError> {
    get_settings_on(&default_store()?)
}

pub fn update_settings(patch: SettingsPatch) -> Result<SettingsUpdate, SettingsError> {
    update_settings_on(&default_store()?, patch)
}

fn default_store() -> Result<smelt_store::Store, SettingsError> {
    sqlite_state::default_sqlite_store().map_err(SettingsError::Store)
}

/// 库里还没有外观记录时的默认值，GUI 与 CLI / Control API 共用这一份。
pub fn default_snapshot() -> smelt_store::AppearanceSnapshot {
    smelt_store::AppearanceSnapshot {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn open_store() -> (tempfile::TempDir, smelt_store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let store =
            smelt_store::Store::open_or_create(dir.path().join(smelt_store::DATABASE_FILE_NAME))
                .unwrap();
        (dir, store)
    }

    #[test]
    fn unknown_keys_are_rejected_before_anything_is_written() {
        let (_dir, store) = open_store();
        let error = update_settings_on(
            &store,
            SettingsPatch {
                settings: BTreeMap::from([
                    ("appearance.nope".into(), json!("x")),
                    (
                        crate::memory_settings::EMBEDDING_MODEL.into(),
                        json!("bge-m3"),
                    ),
                ]),
            },
        )
        .unwrap_err();
        assert!(matches!(error, SettingsError::Invalid(_)));
        assert_eq!(
            get_settings_on(&store).unwrap().settings[crate::memory_settings::EMBEDDING_MODEL],
            "",
            "补丁里有坏键时，记忆配置也不能写进去"
        );
    }

    /// 外观不进运行时的库：校验过的改动原样交给调用方转给客户端。
    #[test]
    fn appearance_changes_are_returned_for_forwarding_not_stored() {
        let (_dir, store) = open_store();
        let update = update_settings_on(
            &store,
            SettingsPatch {
                settings: BTreeMap::from([
                    (THEME_MODE.into(), json!("light")),
                    (UI_FONT_PX.into(), json!(18)),
                ]),
            },
        )
        .unwrap();
        assert_eq!(update.appearance[THEME_MODE], "light");
        assert_eq!(update.appearance[UI_FONT_PX], 18);
        assert!(!update.result.settings.contains_key(THEME_MODE));
        assert!(store.get_appearance_snapshot().unwrap().is_none());
    }

    #[test]
    fn theme_name_selects_a_catalog_entry_and_sets_mode() {
        let mut snapshot = default_snapshot();
        apply_patch(
            &mut snapshot,
            &BTreeMap::from([(THEME_NAME.into(), json!("Ayu Light"))]),
        )
        .unwrap();
        assert_eq!(snapshot.theme_name, "Ayu Light");
        assert_eq!(snapshot.theme_mode, "light");

        let error = apply_patch(
            &mut snapshot,
            &BTreeMap::from([(THEME_NAME.into(), json!("Not A Theme"))]),
        )
        .unwrap_err();
        assert!(matches!(error, SettingsError::Invalid(_)));
    }

    #[test]
    fn theme_mode_flips_to_the_sibling_of_the_current_theme() {
        let mut snapshot = default_snapshot();
        apply_patch(
            &mut snapshot,
            &BTreeMap::from([(THEME_MODE.into(), json!("light"))]),
        )
        .unwrap();
        assert_eq!(snapshot.theme_mode, "light");
        assert_eq!(snapshot.theme_name, "Default Light");
    }
}
