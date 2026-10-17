//! 外观设置的可脚本目录。GUI、CLI 和 Control API 只改这里登记过的 key。
//!
//! 外观归客户端，存在客户端自己的库里。CLI / Control API 发来的外观改动由运行时在这
//! 里校验，再转给在线的桌面客户端去改；记忆的向量检索配置归运行时，直接写库。

pub use smelt_model::appearance_settings::*;

use std::collections::BTreeMap;


use crate::sqlite_state;

/// 运行时这边能直接回答的设置：记忆的向量检索配置。外观归客户端（设计文档 D1），
/// 运行时不存，也就不回显。
pub fn get_settings_on(store: &smelt_store::Store) -> Result<SettingsResult, SettingsError> {
    let store_error = |error: smelt_store::StoreError| SettingsError::Store(error.to_string());
    Ok(SettingsResult {
        settings: crate::memory_settings::to_map(
            &crate::memory_settings::load_on(store).map_err(store_error)?,
            crate::memory_settings::load_rerank_model_on(store)
                .map_err(store_error)?
                .as_ref(),
        ),
    })
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

pub fn get_settings() -> Result<SettingsResult, SettingsError> {
    get_settings_on(&default_store()?)
}

pub fn update_settings(patch: SettingsPatch) -> Result<SettingsUpdate, SettingsError> {
    update_settings_on(&default_store()?, patch)
}

fn default_store() -> Result<smelt_store::Store, SettingsError> {
    sqlite_state::default_sqlite_store().map_err(SettingsError::Store)
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
