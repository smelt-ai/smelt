//! 设置页「语言」：界面显示语言（`跟随系统（...）` 还是固定到列表里的一项），渲染由 NSH 主壳承担。

use gpui::{App, SharedString};
use rust_i18n::t;
use serde_json::{Value, json};
use smelt_plugin_api::{self as api, SettingsEntry, SettingsGroup, ViewSpec, text};
use smelt_ui::IconName;

use super::LanguageChoice;

pub(crate) fn spec() -> ViewSpec {
    let keywords = ["language", "语言", "locale", "界面语言", "English", "中文"];
    ViewSpec {
        name: "settings".into(),
        title: text!("language.page.title"),
        icon: IconName::Languages.path(),
        draws_top_bar: false,
        instances: false,
        settings: Some(SettingsEntry {
            group: SettingsGroup::General,
            keywords: keywords.into_iter().map(SharedString::from).collect(),
        }),
        fills: Vec::new(),
        build: crate::host_views::nsh_managed_view_builder(),
    }
}

fn system_language_name() -> &'static str {
    let code = api::system_locale();
    api::LOCALES
        .iter()
        .find(|(id, _)| *id == code)
        .map_or(code, |(_, name)| *name)
}

pub(crate) fn snapshot(cx: &App) -> Value {
    let system = system_language_name();
    let mut options = Vec::with_capacity(api::LOCALES.len() + 1);
    options.push(json!({
        "value": "",
        "title": t!("language.option.system", language = system).into_owned(),
    }));
    for (code, name) in api::LOCALES {
        options.push(json!({
            "value": *code,
            "title": *name,
        }));
    }
    let selected = cx
        .try_global::<LanguageChoice>()
        .and_then(|choice| choice.get(cx))
        .unwrap_or("");
    json!({
        "kind": "language",
        "section_title": t!("language.section.interface").into_owned(),
        "row_title": t!("language.row.language").into_owned(),
        "row_description": t!("language.row.language_description").into_owned(),
        "selected": selected,
        "options": options,
    })
}

pub(crate) fn handle_action(action: &Value, cx: &mut App) {
    if action["kind"].as_str() != Some("set") {
        return;
    }
    let Some(value) = action["value"].as_str() else {
        return;
    };
    let picked = if value.is_empty() {
        None
    } else {
        api::LOCALES
            .iter()
            .find(|(code, _)| *code == value)
            .map(|(code, _)| *code)
    };
    if let Some(choice) = cx.try_global::<LanguageChoice>().cloned() {
        choice.set(picked, cx);
    }
}
