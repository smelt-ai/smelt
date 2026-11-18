//! 设置「通用 → 会话列表」：会话列表的偏好。眼下只有行首头像用哪个。
//!
//! 行首头像是插槽 [`SESSION_AVATAR`]，只放一个。好几个插件都填时用哪个只有用户知道，这边
//! 不按登记顺序、来源去猜，在这里让人选；没选过用第一个。

use std::rc::Rc;

use gpui::{
    App, AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString, Window,
};
use rust_i18n::t;
use smelt_plugin_api::ui::{Select, SettingsPage, SettingsRow, SettingsSection};
use smelt_plugin_api::{
    self as api, HostEvent, PluginView, SettingsEntry, SettingsGroup, ViewHandle, ViewSpec, text,
};

use crate::contract::SESSION_AVATAR;
use crate::model::{AvatarChoice, Sessions};

pub(crate) fn spec() -> ViewSpec {
    // 搜索词两种语言都写：用哪种语言搜都找得到。
    let keywords = [
        "头像", "小人", "行首", "侧栏", "avatar", "buddy", "sidebar", "sessions",
    ];
    ViewSpec {
        name: "settings".into(),
        title: text!("settings.title"),
        icon: api::ui::IconName::MessagesSquare.path(),
        draws_top_bar: false,
        instances: false,
        settings: Some(SettingsEntry {
            group: SettingsGroup::General,
            keywords: keywords.into_iter().map(SharedString::from).collect(),
        }),
        fills: Vec::new(),
        build: Rc::new(|_, cx| Rc::new(cx.new(SessionsSettings::new)) as Rc<dyn ViewHandle>),
    }
}

struct SessionsSettings;

impl SessionsSettings {
    fn new(cx: &mut Context<Self>) -> Self {
        // 选择改了在 `Sessions` 上通知；填插槽的插件来了、走了发 `ViewsChanged`。
        cx.observe(&Sessions::global(cx), |_, _, cx| cx.notify())
            .detach();
        cx.subscribe(&api::events(cx), |_, _, event: &HostEvent, cx| {
            if matches!(event, HostEvent::ViewsChanged) {
                cx.notify();
            }
        })
        .detach();
        Self
    }
}

impl PluginView for SessionsSettings {
    fn open(&mut self, _: &api::serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}

impl Render for SessionsSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let filled = api::slot_views(SESSION_AVATAR, cx);
        let ids: Vec<SharedString> = filled.iter().map(|view| view.id.clone()).collect();
        // 下拉里显示的是眼下实际用的那个：没选过、选的卸了时是第一个。
        let current = match Sessions::global(cx).read(cx).avatar.pick(&ids) {
            Some(id) => AvatarChoice::View(id.to_string()),
            None => AvatarChoice::Icon,
        };
        let options = filled
            .into_iter()
            .map(|view| (AvatarChoice::View(view.id.to_string()), view.title.get()))
            .chain([(AvatarChoice::Icon, t!("settings.avatar_icon").into())]);
        let select = Select::new("session-avatar")
            .options(options)
            .selected(Some(current))
            .on_change(|choice: &AvatarChoice, _, cx: &mut App| {
                let choice = choice.clone();
                Sessions::global(cx).update(cx, |sessions, cx| sessions.set_avatar(choice, cx));
            });
        SettingsPage::new("sessions-settings").child(
            SettingsSection::new()
                .title(t!("settings.section_list"))
                .child(
                    SettingsRow::new(t!("settings.avatar"))
                        .description(t!("settings.avatar_description"))
                        .child(select),
                ),
        )
    }
}
