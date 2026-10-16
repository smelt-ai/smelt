//! 外观设置的可脚本目录。GUI、CLI 和 Control API 只改这里登记过的 key。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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

pub fn get_settings_on(store: &smelt_store::Store) -> Result<SettingsResult, SettingsError> {
    let mut settings = snapshot_to_map(get_snapshot_on(store)?);
    settings.extend(crate::memory_settings::to_map(
        &crate::memory_settings::load_on(store)?,
        crate::memory_settings::load_rerank_model_on(store)?.as_ref(),
    ));
    Ok(SettingsResult { settings })
}

/// 所有写入都经 `Store::modify_appearance_snapshot` 在一个写事务里读-改-写：
/// GUI、smeltd（Control API）和 CLI 是三个进程，谁都不能拿事务外的旧值整份回写。
pub fn update_settings_on(
    store: &smelt_store::Store,
    patch: SettingsPatch,
) -> Result<SettingsResult, SettingsError> {
    if patch.settings.is_empty() {
        return Err(SettingsError::Invalid("没有要更新的设置".into()));
    }
    // 记忆的向量检索配置不属于外观快照，单独存；其余照旧在一个写事务里读-改-写。
    let (memory, appearance): (Vec<_>, Vec<_>) = patch
        .settings
        .iter()
        .partition(|(key, _)| crate::memory_settings::is_memory_key(key));
    for (key, value) in memory {
        crate::memory_settings::apply_on(store, key, value).map_err(SettingsError::Invalid)?;
    }
    if !appearance.is_empty() {
        store.modify_appearance_snapshot(default_snapshot, |snapshot| {
            for (key, value) in &appearance {
                apply_key(snapshot, key, value)?;
            }
            Ok::<(), SettingsError>(())
        })?;
    }
    get_settings_on(store)
}

/// 完整外观快照（含不在可脚本目录里的底色、玻璃等字段），供 GUI 设置页使用。
pub fn get_snapshot_on(
    store: &smelt_store::Store,
) -> Result<smelt_store::AppearanceSnapshot, SettingsError> {
    Ok(store
        .get_appearance_snapshot()?
        .unwrap_or_else(default_snapshot))
}

/// 提交 GUI 的一次编辑：`before` 是编辑前的副本（可能已落后于库），只把
/// `before → after` 之间真正改过的字段合并到库里当前值上，返回提交后的完整快照。
pub fn commit_edit_on(
    store: &smelt_store::Store,
    before: &smelt_store::AppearanceSnapshot,
    after: &smelt_store::AppearanceSnapshot,
) -> Result<smelt_store::AppearanceSnapshot, SettingsError> {
    store.modify_appearance_snapshot(default_snapshot, |current| {
        take_edited_fields(current, before, after);
        Ok::<(), SettingsError>(())
    })
}

fn take_edited_fields(
    current: &mut smelt_store::AppearanceSnapshot,
    before: &smelt_store::AppearanceSnapshot,
    after: &smelt_store::AppearanceSnapshot,
) {
    // 穷举解构：快照新增字段时这里编译失败，逼着决定它是否参与合并。
    let smelt_store::AppearanceSnapshot {
        bg_color: _,
        bg_image: _,
        bg_image_opacity: _,
        opacity: _,
        blur: _,
        glass_style: _,
        theme_mode: _,
        theme_name: _,
        ui_font_px: _,
        ui_font_family: _,
        font_px: _,
        font_family: _,
    } = after;
    macro_rules! take {
        ($($field:ident),* $(,)?) => {
            $(if before.$field != after.$field {
                current.$field = after.$field.clone();
            })*
        };
    }
    take!(
        bg_color,
        bg_image,
        bg_image_opacity,
        opacity,
        blur,
        glass_style,
        theme_mode,
        theme_name,
        ui_font_px,
        ui_font_family,
        font_px,
        font_family,
    );
}

pub fn get_settings() -> Result<SettingsResult, SettingsError> {
    get_settings_on(&default_store()?)
}

pub fn update_settings(patch: SettingsPatch) -> Result<SettingsResult, SettingsError> {
    update_settings_on(&default_store()?, patch)
}

pub fn get_snapshot() -> Result<smelt_store::AppearanceSnapshot, SettingsError> {
    get_snapshot_on(&default_store()?)
}

pub fn commit_edit(
    before: &smelt_store::AppearanceSnapshot,
    after: &smelt_store::AppearanceSnapshot,
) -> Result<smelt_store::AppearanceSnapshot, SettingsError> {
    commit_edit_on(&default_store()?, before, after)
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

fn snapshot_to_map(snapshot: smelt_store::AppearanceSnapshot) -> BTreeMap<String, Value> {
    let mut settings = BTreeMap::new();
    settings.insert(THEME_MODE.into(), json!(snapshot.theme_mode));
    settings.insert(THEME_NAME.into(), json!(snapshot.theme_name));
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
            snapshot.theme_name = crate::theme_index::sibling(&snapshot.theme_name, mode);
        }
        THEME_NAME => {
            let name = value
                .as_str()
                .map(str::trim)
                .filter(|name| !name.is_empty() && name.len() <= 80)
                .ok_or_else(|| SettingsError::Invalid("appearance.theme 必须是主题名".into()))?;
            let entry = crate::theme_index::find(name).ok_or_else(|| {
                SettingsError::Invalid(format!("未知主题: {name}"))
            })?;
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

    fn open_store() -> (tempfile::TempDir, smelt_store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let store =
            smelt_store::Store::open_or_create(dir.path().join(smelt_store::DATABASE_FILE_NAME))
                .unwrap();
        (dir, store)
    }

    /// GUI 设置页拿着打开时的副本改了不透明度；在此期间 `smelt settings set` 改了字号。
    /// GUI 落盘只能写它自己改过的字段，不能把 CLI 的修改用旧副本覆盖回去。
    #[test]
    fn gui_edit_merges_onto_concurrent_cli_update() {
        let (_dir, store) = open_store();
        let gui_before = get_snapshot_on(&store).unwrap();

        update_settings_on(
            &store,
            SettingsPatch {
                settings: BTreeMap::from([(FONT_PX.into(), json!(18))]),
            },
        )
        .unwrap();

        let mut gui_after = gui_before.clone();
        gui_after.opacity = 0.6;
        let committed = commit_edit_on(&store, &gui_before, &gui_after).unwrap();

        assert_eq!(committed.font_px, 18, "CLI 改的字号被 GUI 旧副本覆盖了");
        assert_eq!(committed.opacity, 0.6);
        assert_eq!(get_snapshot_on(&store).unwrap(), committed);
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
        assert_eq!(updated.settings[THEME_NAME], "Default Light");
        assert_eq!(updated.settings[UI_FONT_PX], 18);
        assert_eq!(
            get_settings_on(&store).unwrap().settings[THEME_MODE],
            "light"
        );
    }

    #[test]
    fn theme_name_selects_a_catalog_entry_and_sets_mode() {
        let (_dir, store) = open_store();
        let updated = update_settings_on(
            &store,
            SettingsPatch {
                settings: BTreeMap::from([(THEME_NAME.into(), json!("Ayu Light"))]),
            },
        )
        .unwrap();
        assert_eq!(updated.settings[THEME_NAME], "Ayu Light");
        assert_eq!(updated.settings[THEME_MODE], "light");

        let error = update_settings_on(
            &store,
            SettingsPatch {
                settings: BTreeMap::from([(THEME_NAME.into(), json!("Not A Theme"))]),
            },
        )
        .unwrap_err();
        assert!(matches!(error, SettingsError::Invalid(_)));
        assert_eq!(
            get_settings_on(&store).unwrap().settings[THEME_NAME],
            "Ayu Light"
        );
    }
}
