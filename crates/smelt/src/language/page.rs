//! 设置页「语言」：界面显示语言（`跟随系统（...）` 还是固定到列表里的一项）。

use std::rc::Rc;

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString, Window,
};
use rust_i18n::t;
use smelt_plugin_api::{
    self as api, PluginView, SettingsEntry, SettingsGroup, ViewHandle, ViewSpec, text,
};
use smelt_ui::{IconName, Select, SettingsPage, SettingsRow, SettingsSection};

use super::LanguageChoice;

pub(crate) fn spec(choice: LanguageChoice) -> ViewSpec {
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
        build: Rc::new(move |_, cx| {
            let choice = choice.clone();
            Rc::new(cx.new(|_| LanguageSettings { choice })) as Rc<dyn ViewHandle>
        }),
    }
}

struct LanguageSettings {
    choice: LanguageChoice,
}

impl PluginView for LanguageSettings {
    fn open(&mut self, _: &api::serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}

fn system_language_name() -> &'static str {
    let code = api::system_locale();
    api::LOCALES
        .iter()
        .find(|(id, _)| *id == code)
        .map_or(code, |(_, name)| *name)
}

impl Render for LanguageSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let system = system_language_name();
        let options = std::iter::once((
            None,
            SharedString::from(t!("language.option.system", language = system)),
        ))
        .chain(
            api::LOCALES
                .iter()
                .map(|(code, name)| (Some(*code), SharedString::new_static(name))),
        );
        let choice = self.choice.clone();
        let select = Select::new("language-select")
            .options(options)
            .selected(Some(self.choice.get(cx)))
            .on_change(move |picked: &Option<&'static str>, _, cx| choice.set(*picked, cx));
        SettingsPage::new("language-settings").child(
            SettingsSection::new()
                .title(t!("language.section.interface"))
                .child(
                    SettingsRow::new(t!("language.row.language"))
                        .description(t!("language.row.language_description"))
                        .child(select),
                ),
        )
    }
}
