//! 设置「通用 → 语言」：选界面语言。

use std::rc::Rc;

use gpui::{
    App, AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString, Window,
};
use rust_i18n::t;
use smelt_plugin_api::ui::{Select, SettingsPage, SettingsRow, SettingsSection};
use smelt_plugin_api::{
    self as api, PluginView, SettingsEntry, SettingsGroup, ViewHandle, ViewSpec, text,
};

use crate::Choice;

pub(crate) fn register(choice: Rc<Choice>, cx: &mut App) {
    // 搜索词两种语言都写：用哪种语言搜都找得到。
    let keywords = ["language", "语言", "locale", "界面语言", "English", "中文"];
    let spec = ViewSpec {
        name: "settings".into(),
        title: text!("page.title"),
        icon: api::ui::IconName::Languages.path(),
        draws_top_bar: false,
        instances: false,
        settings: Some(SettingsEntry {
            group: SettingsGroup::General,
            keywords: keywords.into_iter().map(SharedString::from).collect(),
        }),
        fills: Vec::new(),
        build: Rc::new(move |_, cx| {
            let choice = choice.clone();
            Rc::new(cx.new(|_| LanguagePage { choice })) as Rc<dyn ViewHandle>
        }),
    };
    if let Err(error) = api::register_views(crate::OWNER, vec![spec], cx) {
        eprintln!("[language] 设置页登记失败：{error}");
    }
}

struct LanguagePage {
    choice: Rc<Choice>,
}

impl PluginView for LanguagePage {
    fn open(&mut self, _: &api::serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}

/// 语言代码在这种语言里的叫法。
fn native_name(code: &str) -> &'static str {
    api::LOCALES
        .iter()
        .find(|(locale, _)| *locale == code)
        .map_or("", |(_, name)| name)
}

impl Render for LanguagePage {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let system = native_name(api::system_locale());
        // 选项：跟随系统在最前，后面是每种语言自己的叫法（不翻译，换到看不懂的语言也找得回来）。
        let options = std::iter::once((
            None,
            SharedString::from(t!("option.system", language = system)),
        ))
        .chain(
            api::LOCALES
                .iter()
                .map(|(code, name)| (Some(*code), SharedString::new_static(name))),
        );
        let choice = self.choice.clone();
        let select = Select::new("language-select")
            .options(options)
            .selected(Some(self.choice.get()))
            .on_change(move |picked: &Option<&'static str>, _, cx| choice.set(*picked, cx));
        SettingsPage::new("language-settings").child(
            SettingsSection::new().title(t!("section.interface")).child(
                SettingsRow::new(t!("row.language"))
                    .description(t!("row.language_description"))
                    .child(select),
            ),
        )
    }
}
