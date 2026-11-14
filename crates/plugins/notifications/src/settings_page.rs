//! 设置页「通知」：哪类提醒发系统通知。系统授权是整页的前提，状态挂在页面标题旁。

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

/// 页面标题旁一直写着系统通知的状态，并给出按钮：没问过的先问，别的都能一键去系统设置里
/// 本应用那一页。有挡着提醒的写明是哪项。还在读、这个系统没有通知时不出。
fn permission_status(cx: &mut App) -> AnyElement {
    let Some(line) = permission_line(&api::host(cx).notification_permission(cx)) else {
        return div().into_any_element();
    };
    let color = match line.severity {
        Severity::Ok => cx.theme().success,
        Severity::Error => cx.theme().danger,
        Severity::Warning => cx.theme().warning,
        Severity::Info => cx.theme().muted_foreground,
    };
    let fix = line.fix.map(|fix| {
        let (id, label, command) = match fix {
            Fix::Request => (
                "request-notification-permission",
                "请求授权",
                api::REQUEST_NOTIFICATION_PERMISSION,
            ),
            Fix::OpenSettings => (
                "open-system-notification-settings",
                "打开系统设置 ↗",
                api::OPEN_NOTIFICATION_SETTINGS,
            ),
        };
        Button::new(id)
            .compact()
            .label(label)
            .on_click(move |_, _, cx| {
                let task = command.run(&(), cx);
                cx.spawn(async move |_| {
                    if let Err(error) = task.await {
                        eprintln!("[notifications] {label}失败：{error}");
                    }
                })
                .detach();
            })
    });
    h_flex()
        .items_center()
        .gap_2()
        .child(div().size(px(6.)).rounded_full().bg(color))
        .child(div().text_xs().text_color(color).child(line.text))
        .children(fix)
        .into_any_element()
}

#[derive(Debug, PartialEq, Eq)]
struct PermissionLine {
    text: String,
    severity: Severity,
    fix: Option<Fix>,
}

#[derive(Debug, PartialEq, Eq)]
enum Severity {
    Ok,
    Error,
    Warning,
    Info,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fix {
    Request,
    OpenSettings,
}

/// 标题旁那一行。声音关了不算问题：那是用户自己选的安静，提醒照样到；横幅和角标关了，
/// 人就看不见提醒。
fn permission_line(permission: &NotificationPermission) -> Option<PermissionLine> {
    let line = |text: &str, severity, fix| PermissionLine {
        text: text.to_string(),
        severity,
        fix,
    };
    match permission {
        NotificationPermission::Checking | NotificationPermission::Unsupported => None,
        NotificationPermission::Granted { banners, badge, .. } => match (banners, badge) {
            (true, true) => Some(line(
                "系统通知已允许",
                Severity::Ok,
                Some(Fix::OpenSettings),
            )),
            (false, true) => Some(line(
                "提醒样式是「无」，只进通知中心，不弹横幅",
                Severity::Warning,
                Some(Fix::OpenSettings),
            )),
            (true, false) => Some(line(
                "Dock 角标被系统关了，看不到未读数",
                Severity::Warning,
                Some(Fix::OpenSettings),
            )),
            (false, false) => Some(line(
                "不弹横幅，Dock 也不显示未读数",
                Severity::Warning,
                Some(Fix::OpenSettings),
            )),
        },
        NotificationPermission::NotRequested => {
            Some(line("尚未请求系统授权", Severity::Info, Some(Fix::Request)))
        }
        NotificationPermission::Denied => Some(line(
            "已被系统禁止",
            Severity::Error,
            Some(Fix::OpenSettings),
        )),
        // 系统报错：回到应用时宿主会重查。
        NotificationPermission::Unavailable(reason) => Some(line(reason, Severity::Error, None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn granted(banners: bool, badge: bool) -> NotificationPermission {
        NotificationPermission::Granted {
            banners,
            sound: false,
            badge,
        }
    }

    #[test]
    fn everything_on_still_shows_the_way_to_settings() {
        // 声音关着也算正常。
        let ok = permission_line(&granted(true, true)).unwrap();
        assert_eq!(ok.severity, Severity::Ok);
        assert_eq!(ok.fix, Some(Fix::OpenSettings));
        // 还在读、没有系统通知：不出。
        assert_eq!(permission_line(&NotificationPermission::Checking), None);
        assert_eq!(permission_line(&NotificationPermission::Unsupported), None);
    }

    #[test]
    fn switched_off_items_point_to_system_settings() {
        for permission in [
            granted(false, true),
            granted(true, false),
            granted(false, false),
        ] {
            let line = permission_line(&permission).unwrap();
            assert_eq!(line.severity, Severity::Warning);
            assert_eq!(line.fix, Some(Fix::OpenSettings));
        }
        assert!(
            permission_line(&granted(true, false))
                .unwrap()
                .text
                .contains("角标")
        );
        assert_eq!(
            permission_line(&NotificationPermission::Denied)
                .unwrap()
                .fix,
            Some(Fix::OpenSettings)
        );
    }

    #[test]
    fn not_asked_yet_offers_to_ask() {
        assert_eq!(
            permission_line(&NotificationPermission::NotRequested)
                .unwrap()
                .fix,
            Some(Fix::Request)
        );
        assert_eq!(
            permission_line(&NotificationPermission::Unavailable("坏了".into()))
                .unwrap()
                .fix,
            None
        );
    }
}
