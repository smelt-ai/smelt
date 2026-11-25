use super::*;

#[cfg(test)]
mod appearance_tests {
    use super::{Appearance, ThemeMode};

    #[test]
    fn empty_ui_font_family_means_the_bundled_inter() {
        let ap = Appearance::default();
        assert_eq!(ap.ui_font_px, super::DEFAULT_UI_FONT_PX);
        assert_eq!(super::resolved_ui_font_family(&ap).as_ref(), "Inter");
    }

    /// 外观存进客户端库再读回来，枚举字段按名字往返不丢。
    #[test]
    fn appearance_survives_the_snapshot_round_trip() {
        let appearance = Appearance {
            theme_mode: ThemeMode::Light,
            theme_name: "Ayu Light".to_string(),
            ui_font_px: 18,
            bg_image: Some("/tmp/bg.png".to_string()),
            ..Appearance::default()
        };
        let snapshot = super::snapshot_from_appearance(&appearance);
        assert_eq!(snapshot.theme_mode, "light");
        let back = super::appearance_from_snapshot(snapshot, Appearance::default());
        assert_eq!(back.theme_mode, ThemeMode::Light);
        assert_eq!(back.theme_name, "Ayu Light");
        assert_eq!(back.ui_font_px, 18);
        assert_eq!(back.bg_image.as_deref(), Some("/tmp/bg.png"));
    }

    /// 背景图不透明度封顶：旧设置里存着更高的值，读出来按上限截。
    #[test]
    fn bg_image_opacity_is_capped() {
        let appearance = Appearance {
            bg_image_opacity: 0.8,
            ..Appearance::default()
        };
        let snapshot = super::snapshot_from_appearance(&appearance);
        let back = super::appearance_from_snapshot(snapshot, Appearance::default());
        assert_eq!(
            back.bg_image_opacity,
            smelt_model::appearance_settings::MAX_BG_IMAGE_OPACITY
        );
    }

    #[test]
    fn sibling_themes_are_scoped_by_plugin_owner() {
        use smelt_model::theme_index::{ThemeEntry, sibling_in};
        let family = |owner| super::scoped_theme_family(owner, "Shared");
        let entries = [
            ThemeEntry::new("First Dark", "dark", family(Some("first"))),
            ThemeEntry::new("First Light", "light", family(Some("first"))),
            ThemeEntry::new("Second Light", "light", family(Some("second"))),
        ];
        assert_eq!(sibling_in(&entries, "First Dark", "light"), "First Light");
        let entries = [
            ThemeEntry::new("First Dark", "dark", family(Some("first"))),
            ThemeEntry::new("Second Light", "light", family(Some("second"))),
        ];
        assert_eq!(
            sibling_in(&entries, "First Dark", "light"),
            smelt_model::theme_index::DEFAULT_LIGHT
        );
        assert_ne!(family(None), family(Some("")));
    }

    #[gpui::test]
    fn requested_appearance_reports_unknown_theme(cx: &mut gpui::TestAppContext) {
        let dir =
            std::env::temp_dir().join(format!("smelt-appearance-test-{}", uuid::Uuid::new_v4()));
        let store = smelt_client_store::ClientStore::open(dir.join("client.sqlite3")).unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            smelt_ui::init(cx);
        });
        cx.update(|cx| {
            cx.set_global(Appearance::default());
            let result = super::apply_requested_appearance_on(
                &std::collections::BTreeMap::from([(
                    "appearance.theme".into(),
                    serde_json::json!("not a registered theme"),
                )]),
                &store,
                cx,
            );
            assert!(matches!(
                result,
                Err(smelt_model::appearance_settings::SettingsError::Invalid(_))
            ));
            assert!(store.appearance().unwrap().is_none());
        });
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 命令行改外观只动它给的键；选的布局不在可改的键里，改完照样留着，库里也是。
    #[gpui::test]
    fn requested_appearance_keeps_the_chosen_layout(cx: &mut gpui::TestAppContext) {
        let dir =
            std::env::temp_dir().join(format!("smelt-appearance-test-{}", uuid::Uuid::new_v4()));
        let store = smelt_client_store::ClientStore::open(dir.join("client.sqlite3")).unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            smelt_ui::init(cx);
        });
        cx.update(|cx| {
            cx.set_global(Appearance {
                layout_id: Some("trader/two-columns".to_string()),
                ..Appearance::default()
            });
            super::apply_requested_appearance_on(
                &std::collections::BTreeMap::from([(
                    "appearance.ui_font_family".into(),
                    serde_json::json!("Menlo"),
                )]),
                &store,
                cx,
            )
            .unwrap();
            assert_eq!(
                cx.global::<Appearance>().layout_id.as_deref(),
                Some("trader/two-columns")
            );
            let saved = store.appearance().unwrap().unwrap();
            assert_eq!(saved.ui_font_family, "Menlo");
            assert_eq!(saved.layout_id.as_deref(), Some("trader/two-columns"));
        });
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[gpui::test]
    fn requested_appearance_does_not_install_when_saving_fails(cx: &mut gpui::TestAppContext) {
        let dir =
            std::env::temp_dir().join(format!("smelt-appearance-test-{}", uuid::Uuid::new_v4()));
        let store = smelt_client_store::ClientStore::open(dir.join("client.sqlite3")).unwrap();
        let connection = rusqlite::Connection::open(store.path()).unwrap();
        connection.execute_batch("CREATE TRIGGER fail_appearance BEFORE INSERT ON appearance BEGIN SELECT RAISE(FAIL, 'save blocked'); END;").unwrap();
        drop(connection);
        cx.update(|cx| {
            gpui_base::init(cx);
            smelt_ui::init(cx);
        });
        cx.update(|cx| {
            cx.set_global(Appearance::default());
            let original = cx.global::<Appearance>().ui_font_family.clone();
            let result = super::apply_requested_appearance_on(
                &std::collections::BTreeMap::from([(
                    "appearance.ui_font_family".into(),
                    serde_json::json!("Menlo"),
                )]),
                &store,
                cx,
            );
            assert!(matches!(
                result,
                Err(smelt_model::appearance_settings::SettingsError::Store(_))
            ));
            assert_eq!(cx.global::<Appearance>().ui_font_family, original);
            assert!(store.appearance().unwrap().is_none());
        });
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

mod page_tests {
    use super::super::{SETTINGS_PAGE, SETTINGS_VIEW, register_page};

    /// 设置那一页的布局按官方格式解析得了：中间横排两栏，左边导航、右边内容，各自单独一组。
    #[gpui::test]
    fn the_settings_page_lays_out_nav_and_content_side_by_side(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_page(cx);
            let layouts = smelt_plugin_api::layouts(cx);
            assert_eq!(layouts.len(), 1);
            assert_eq!(layouts[0].page.as_ref(), SETTINGS_PAGE);
            let state = smelt_plugin_api::layout(&layouts[0].id, cx).expect("登记上了");
            let center = serde_json::to_value(&state).unwrap()["center"].clone();
            assert_eq!(center["panel_name"], "StackPanel");
            assert_eq!(center["info"]["stack"]["axis"], 0, "0 是横排");
            let columns: Vec<Vec<&str>> = center["children"]
                .as_array()
                .unwrap()
                .iter()
                .map(|tabs| {
                    assert_eq!(tabs["panel_name"], "TabPanel");
                    tabs["children"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|panel| panel["panel_name"].as_str().unwrap())
                        .collect()
                })
                .collect();
            assert_eq!(columns, [["smelt/settings-nav"], [SETTINGS_VIEW]]);
        });
    }

    /// 多个设置页同时挂角标时，导航红点集合必须同时保留全部有角标的设置页（非设置页不混入），
    /// 摘掉其中一页的角标只清那一页。
    #[gpui::test]
    fn attention_pages_tracks_all_badged_settings_views(cx: &mut gpui::TestAppContext) {
        use gpui::{AppContext as _, Context, IntoElement, Render, Window, div};
        use smelt_plugin_api::{SettingsEntry, SettingsGroup, ViewHandle, ViewSpec};
        use std::rc::Rc;

        struct DummyView;
        impl Render for DummyView {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
            }
        }
        impl smelt_plugin_api::PluginView for DummyView {
            fn open(&mut self, _: &serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
        }
        let spec = |name: &str, group: Option<SettingsGroup>| ViewSpec {
            name: name.to_owned().into(),
            title: name.to_owned().into(),
            icon: "icons/settings.svg".into(),
            draws_top_bar: false,
            settings: group.map(|group| SettingsEntry {
                group,
                keywords: Vec::new(),
            }),
            instances: false,
            fills: Vec::new(),
            build: Rc::new(|_, cx| -> Rc<dyn ViewHandle> { Rc::new(cx.new(|_| DummyView)) }),
        };

        cx.update(|cx| {
            smelt_plugin_api::register_views(
                "a",
                vec![spec("one", Some(SettingsGroup::Agents))],
                cx,
            )
            .unwrap();
            smelt_plugin_api::register_views(
                "b",
                vec![spec("two", Some(SettingsGroup::System))],
                cx,
            )
            .unwrap();
            smelt_plugin_api::register_views("c", vec![spec("other", None)], cx).unwrap();

            smelt_plugin_api::set_view_badge("a/one", Some("!".into()), cx);
            smelt_plugin_api::set_view_badge("b/two", Some("new".into()), cx);
            smelt_plugin_api::set_view_badge("c/other", Some("9".into()), cx);

            let pages = super::super::attention_pages(cx);
            assert_eq!(
                pages,
                std::collections::BTreeSet::from(["a/one".into(), "b/two".into()])
            );

            smelt_plugin_api::set_view_badge("a/one", None, cx);
            let pages = super::super::attention_pages(cx);
            assert_eq!(pages, std::collections::BTreeSet::from(["b/two".into()]));
        });
    }
}
