//! 外观设置的可脚本目录。GUI、CLI 和 Control API 只改这里登记过的 key。
//!
//! 外观归客户端，存在客户端自己的库里。CLI / Control API 发来的外观改动由运行时在这
//! 里校验，再转给在线的桌面客户端去改；记忆的向量检索配置和本机标题模型归运行时，直接写库。

pub use smelt_model::appearance_settings::*;
pub use smelt_model::local_title_settings::is_local_title_key;

use std::collections::BTreeMap;

use serde_json::Value;
use smelt_model::local_title_settings::{self as local_title, LocalTitleModel};

use crate::sqlite_state;

/// 运行时这边能直接回答的设置：记忆的向量检索配置、本机标题模型。外观归客户端
/// （设计文档 D1），运行时不存，也就不回显。
pub fn get_settings_on(store: &smelt_store::Store) -> Result<SettingsResult, SettingsError> {
    let store_error = |error: smelt_store::StoreError| SettingsError::Store(error.to_string());
    let mut settings = crate::memory_settings::to_map(
        &crate::memory_settings::load_on(store).map_err(store_error)?,
        crate::memory_settings::load_rerank_model_on(store)
            .map_err(store_error)?
            .as_ref(),
    );
    settings.insert(
        local_title::MODEL.to_string(),
        Value::from(
            store.local_title_model().map_err(store_error)?
                .unwrap_or_else(|| LocalTitleModel::default().id().to_string()),
        ),
    );
    Ok(SettingsResult { settings })
}

/// 只认字符串形式的已知 id；不裁空白、不接受空值清除，认不出就拒。
fn parse_local_title_model(value: &Value) -> Result<LocalTitleModel, SettingsError> {
    value
        .as_str()
        .and_then(LocalTitleModel::from_id)
        .ok_or_else(|| {
            let allowed: Vec<_> = LocalTitleModel::ALL.iter().map(|model| model.id()).collect();
            SettingsError::Invalid(format!(
                "{} 只能是 {}，收到 {value}",
                local_title::MODEL,
                allowed.join(" / ")
            ))
        })
}

pub fn update_settings_on(
    store: &smelt_store::Store,
    patch: SettingsPatch,
) -> Result<SettingsUpdate, SettingsError> {
    if patch.settings.is_empty() {
        return Err(SettingsError::Invalid("没有要更新的设置".into()));
    }
    let (runtime, appearance): (BTreeMap<_, _>, BTreeMap<_, _>) =
        patch.settings.into_iter().partition(|(key, _)| {
            crate::memory_settings::is_memory_key(key) || local_title::is_local_title_key(key)
        });
    let (local_title_patch, memory): (BTreeMap<_, _>, BTreeMap<_, _>) = runtime
        .into_iter()
        .partition(|(key, _)| local_title::is_local_title_key(key));
    // 外观先在出厂外观上演算一遍：键名、取值范围、主题名在这里就拒掉，坏请求不转给
    // 客户端；也保证一个补丁要么整体生效，要么什么都没写。
    if !appearance.is_empty() {
        apply_patch(&mut default_snapshot(), &appearance)?;
    }
    // 本机标题模型同样先校验，坏值不让同一补丁里的记忆配置写进去。
    let local_title_model = local_title_patch
        .get(local_title::MODEL)
        .map(parse_local_title_model)
        .transpose()?;
    for (key, value) in &memory {
        crate::memory_settings::apply_on(store, key, value).map_err(SettingsError::Invalid)?;
    }
    if let Some(model) = local_title_model {
        store
            .put_local_title_model(model.id())
            .map_err(|error| SettingsError::Store(error.to_string()))?;
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
    }

    #[test]
    fn local_title_model_defaults_round_trips_and_is_not_forwarded() {
        let (_dir, store) = open_store();
        assert_eq!(
            get_settings_on(&store).unwrap().settings[local_title::MODEL],
            "qwen3.5-0.8b"
        );
        assert_eq!(store.local_title_model().unwrap(), None, "读默认值不落库");
        let update = update_settings_on(
            &store,
            SettingsPatch {
                settings: BTreeMap::from([(local_title::MODEL.into(), json!("qwen3.5-0.8b"))]),
            },
        )
        .unwrap();
        assert!(update.appearance.is_empty(), "运行时设置不转给客户端");
        assert_eq!(update.result.settings[local_title::MODEL], "qwen3.5-0.8b");
        assert_eq!(store.local_title_model().unwrap().as_deref(), Some("qwen3.5-0.8b"));
    }

    #[test]
    fn unknown_local_title_model_is_rejected_before_anything_is_written() {
        let (_dir, store) = open_store();
        for bad in [
            json!("qwen3-4b"),
            json!(""),
            json!(null),
            json!(1),
            json!(" qwen3.5-0.8b"),
            // 已下线的模型和其它未知值一样拒绝。
            json!("qwen3-0.6b"),
        ] {
            let error = update_settings_on(
                &store,
                SettingsPatch {
                    settings: BTreeMap::from([
                        (local_title::MODEL.into(), bad.clone()),
                        (
                            crate::memory_settings::EMBEDDING_MODEL.into(),
                            json!("bge-m3"),
                        ),
                    ]),
                },
            )
            .unwrap_err();
            assert!(matches!(error, SettingsError::Invalid(_)), "{bad}");
        }
        assert_eq!(store.local_title_model().unwrap(), None);
        assert_eq!(
            get_settings_on(&store).unwrap().settings[crate::memory_settings::EMBEDDING_MODEL],
            "",
            "补丁里有坏值时，记忆配置也不能写进去"
        );
    }

    /// 降级等情况下不能把不认识的模型冒充默认模型，让界面可以提示并重新选择。
    #[test]
    fn unrecognised_stored_local_title_model_is_returned_verbatim() {
        let (_dir, store) = open_store();
        store.put_local_title_model("qwen9-huge").unwrap();
        assert_eq!(get_settings_on(&store).unwrap().settings[local_title::MODEL], "qwen9-huge");
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
