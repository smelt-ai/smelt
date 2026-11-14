//! 设置页「通知」：哪类提醒发系统通知。系统授权是整页的前提，出问题时挂在页面标题旁。

use std::cell::RefCell;
use std::rc::Rc;

use api::ui::Button;
use api::{HostEvent, NotificationPermission, SettingsGroup, SettingsPageSpec};
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, SharedString, Window, div, px};
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_component::{ActiveTheme as _, IconName, IconNamed as _, h_flex};
use smelt_plugin_api as api;

use crate::store::Prefs;
use crate::{OWNER, Plugin};

pub(crate) fn register(plugin: Rc<RefCell<Plugin>>, cx: &mut App) {
    let spec = SettingsPageSpec {
        name: "main".into(),
        title: "通知".into(),
        icon: IconName::Bell.path(),
        group: SettingsGroup::General,
        keywords: [
            "Agent",
            "提醒",
            "notification",
            "系统通知",
            "等待批准",
            "等待输入",
            "任务完成",
            "任务失败",
            "审批",
            "批准",
            "响铃",
            "bell",
        ]
        .into_iter()
        .map(SharedString::from)
        .collect(),
        build: Rc::new(move |_, cx| {
            let plugin = plugin.clone();
            cx.new(|cx| NotifySettings::new(plugin, cx)).into()
        }),
    };
    if let Err(error) = api::register_settings_pages(OWNER, vec![spec], cx) {
        eprintln!("[notifications] 设置页登记失败：{error}");
    }
}

struct NotifySettings {
    plugin: Rc<RefCell<Plugin>>,
}

impl NotifySettings {
    fn new(plugin: Rc<RefCell<Plugin>>, cx: &mut Context<Self>) -> Self {
        // 授权在系统设置里改，回到应用时宿主重查，变了这里跟着画。
        cx.subscribe(&api::events(cx), |_, _, event: &HostEvent, cx| {
            if matches!(event, HostEvent::NotificationPermissionChanged) {
                cx.notify();
            }
        })
        .detach();
        Self { plugin }
    }
}

impl Render for NotifySettings {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let switch = |label: &'static str,
                      description: Option<&'static str>,
                      get: fn(&Prefs) -> bool,
                      set: fn(&mut Prefs, bool)| {
            let read = self.plugin.clone();
            let write = self.plugin.clone();
            let item = SettingItem::new(
                label,
                SettingField::switch(
                    move |_: &App| get(&read.borrow().prefs),
                    move |value: bool, _: &mut App| {
                        let mut plugin = write.borrow_mut();
                        set(&mut plugin.prefs, value);
                        let prefs = plugin.prefs;
                        plugin.store.save_prefs(prefs);
                    },
                ),
            );
            match description {
                Some(description) => item.description(description),
                None => item,
            }
        };
        let page = SettingPage::new("通知")
            .group(SettingGroup::new().title("Agent 什么时候叫你").items(vec![
                switch("等待批准", None, |p| p.approval, |p, v| p.approval = v),
                switch(
                    "等待输入",
                    Some("提问或等你继续时"),
                    |p| p.input,
                    |p, v| p.input = v,
                ),
                switch("任务完成", None, |p| p.success, |p, v| p.success = v),
                switch("任务失败", None, |p| p.failure, |p, v| p.failure = v),
            ]))
            .group(SettingGroup::new().title("终端").item(switch(
                "响铃",
                Some("终端输出 BEL 时提醒"),
                |p| p.bell,
                |p, v| p.bell = v,
            )))
            .title_suffix(|_, cx| permission_status(cx));
        api::settings_pane("notifications-settings", page)
    }
}

/// 系统授权不正常时，页面标题旁写明是什么情况；被关掉的给个按钮去系统设置里开。已授权、
/// 还在读、这个系统没有通知时不出。
fn permission_status(cx: &mut App) -> AnyElement {
    // 被禁止的能去系统设置里开；读不到的是系统报错，回到应用时宿主会重查。
    let (text, is_error, denied) = match api::host(cx).notification_permission(cx) {
        NotificationPermission::Checking
        | NotificationPermission::Granted
        | NotificationPermission::Unsupported => return div().into_any_element(),
        NotificationPermission::NotRequested => ("尚未请求系统授权".to_string(), false, false),
        NotificationPermission::Denied => ("已被系统禁止".to_string(), true, true),
        NotificationPermission::Unavailable(reason) => (reason, true, false),
    };
    let color = if is_error {
        cx.theme().danger
    } else {
        cx.theme().muted_foreground
    };
    h_flex()
        .items_center()
        .gap_2()
        .child(div().size(px(6.)).rounded_full().bg(color))
        .child(div().text_xs().text_color(color).child(text))
        .children(denied.then(|| {
            Button::new("open-system-notification-settings")
                .compact()
                .label("打开系统设置 ↗")
                .on_click(|_, _, cx| {
                    let task = api::OPEN_NOTIFICATION_SETTINGS.run(&(), cx);
                    cx.spawn(async move |_| {
                        if let Err(error) = task.await {
                            eprintln!("[notifications] 打不开系统设置：{error}");
                        }
                    })
                    .detach();
                })
        }))
        .into_any_element()
}
