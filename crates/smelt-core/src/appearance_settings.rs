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

/// 外观和记忆归不同的库，不能在同一个补丁里原子提交。
fn reject_mixed_patch(patch: &SettingsPatch) -> Result<(), SettingsError> {
    let has_memory = patch
        .settings
        .keys()
        .any(|key| crate::memory_settings::is_memory_key(key));
    let has_other = patch
        .settings
        .keys()
        .any(|key| !crate::memory_settings::is_memory_key(key));
    if has_memory && has_other {
        return Err(SettingsError::Invalid("外观和记忆设置请分两次提交".into()));
    }
    Ok(())
}

pub fn update_settings_on(
    store: &smelt_store::Store,
    patch: SettingsPatch,
) -> Result<SettingsUpdate, SettingsError> {
    if patch.settings.is_empty() {
        return Err(SettingsError::Invalid("没有要更新的设置".into()));
    }
    reject_mixed_patch(&patch)?;
    let (memory, appearance): (BTreeMap<_, _>, BTreeMap<_, _>) = patch
        .settings
        .into_iter()
        .partition(|(key, _)| crate::memory_settings::is_memory_key(key));
    // 外观只校验形状：主题目录（含插件主题）只有桌面知道，主题名是否存在、明暗
    // 换到哪个同族主题由桌面用 `apply_patch` 按它的目录判定。
    validate_patch(&appearance)?;
    if !memory.is_empty() {
        crate::memory_settings::apply_patch_on(store, &memory).map_err(SettingsError::Invalid)?;
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
    reject_mixed_patch(&patch)?;
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
    fn unknown_keys_are_rejected() {
        let (_dir, store) = open_store();
        let error = update_settings_on(
            &store,
            SettingsPatch {
                settings: BTreeMap::from([("appearance.nope".into(), json!("x"))]),
            },
        )
        .unwrap_err();
        assert!(matches!(error, SettingsError::Invalid(_)));
    }

    #[test]
    fn mixed_memory_and_appearance_are_rejected_without_writing() {
        let (_dir, store) = open_store();
        let error = update_settings_on(
            &store,
            SettingsPatch {
                settings: BTreeMap::from([
                    (THEME_NAME.into(), json!("Some Plugin Theme")),
                    (
                        crate::memory_settings::EMBEDDING_MODEL.into(),
                        json!("bge-m3"),
                    ),
                ]),
            },
        )
        .unwrap_err();
        assert!(matches!(error, SettingsError::Invalid(ref message) if message.contains("分两次")));
        assert_eq!(
            get_settings_on(&store).unwrap().settings[crate::memory_settings::EMBEDDING_MODEL],
            ""
        );
    }

    /// 默认库入口也要拒绝混合补丁；此测试只验证错误，不验证打开库的时机。
    #[test]
    fn default_entry_rejects_mixed_patch() {
        let error = update_settings(SettingsPatch {
            settings: BTreeMap::from([
                (THEME_NAME.into(), json!("Some Plugin Theme")),
                (
                    crate::memory_settings::EMBEDDING_MODEL.into(),
                    json!("bge-m3"),
                ),
            ]),
        })
        .unwrap_err();
        assert!(matches!(error, SettingsError::Invalid(ref message) if message.contains("分两次")));
    }

    #[test]
    fn memory_patch_is_atomic_when_a_later_field_is_invalid() {
        let (_dir, store) = open_store();
        let error = update_settings_on(
            &store,
            SettingsPatch {
                settings: BTreeMap::from([
                    (
                        crate::memory_settings::EMBEDDING_API_KEY.into(),
                        json!("secret"),
                    ),
                    (
                        crate::memory_settings::EMBEDDING_BASE_URL.into(),
                        json!("invalid"),
                    ),
                ]),
            },
        )
        .unwrap_err();
        assert!(matches!(error, SettingsError::Invalid(_)));
        assert_eq!(crate::memory_settings::load_on(&store).unwrap().api_key, "");
    }

    #[test]
    fn memory_patch_updates_multiple_fields() {
        let (_dir, store) = open_store();
        update_settings_on(
            &store,
            SettingsPatch {
                settings: BTreeMap::from([
                    (
                        crate::memory_settings::EMBEDDING_BASE_URL.into(),
                        json!("https://example.com/v1"),
                    ),
                    (
                        crate::memory_settings::EMBEDDING_MODEL.into(),
                        json!("bge-m3"),
                    ),
                ]),
            },
        )
        .unwrap();
        let saved = crate::memory_settings::load_on(&store).unwrap();
        assert_eq!(saved.base_url, "https://example.com/v1");
        assert_eq!(saved.model, "bge-m3");
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
    }

    /// 运行时不认主题目录：插件主题名照样转发，存在与否由桌面判定。
    #[test]
    fn theme_name_is_shape_checked_and_forwarded_without_catalog_lookup() {
        let (_dir, store) = open_store();
        let update = update_settings_on(
            &store,
            SettingsPatch {
                settings: BTreeMap::from([(THEME_NAME.into(), json!("Some Plugin Theme"))]),
            },
        )
        .unwrap();
        assert_eq!(update.appearance[THEME_NAME], "Some Plugin Theme");

        let error = update_settings_on(
            &store,
            SettingsPatch {
                settings: BTreeMap::from([(THEME_NAME.into(), json!(""))]),
            },
        )
        .unwrap_err();
        assert!(matches!(error, SettingsError::Invalid(_)));
    }

    #[test]
    fn bad_theme_mode_is_rejected() {
        let (_dir, store) = open_store();
        let error = update_settings_on(
            &store,
            SettingsPatch {
                settings: BTreeMap::from([(THEME_MODE.into(), json!("sepia"))]),
            },
        )
        .unwrap_err();
        assert!(matches!(error, SettingsError::Invalid(_)));
    }
}
