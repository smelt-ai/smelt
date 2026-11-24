//! 设置页「通知」：哪类提醒发系统通知。系统授权是整页的前提，状态挂在页面标题旁。

use std::cell::RefCell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, SharedString, Window, div, px, rgb};
use rust_i18n::t;
use smelt_plugin_api::{
    self as api, HostEvent, NotificationPermission, PluginView, SettingsEntry, SettingsGroup,
    ViewHandle, ViewSpec, text,
};
use smelt_ui::kit::{TYPE, TypeStyled as _};
use smelt_ui::{Button, IconName, SettingsPage, SettingsRow, SettingsSection, Switch, ui_theme};

use super::store::Prefs;
use super::{OWNER, Plugin};

pub(super) fn register(plugin: Rc<RefCell<Plugin>>, cx: &mut App) {
    let spec = ViewSpec {
        name: "main".into(),
        title: text!("notifications.page.title"),
        icon: IconName::Bell.path(),
        draws_top_bar: false,
        instances: false,
        settings: Some(SettingsEntry {
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
                "approval",
                "alert",
            ]
            .into_iter()
            .map(SharedString::from)
            .collect(),
        }),
        fills: Vec::new(),
        build: Rc::new(move |_, cx| {
            let plugin = plugin.clone();
            Rc::new(cx.new(|cx| NotifySettings::new(plugin, cx))) as Rc<dyn ViewHandle>
        }),
    };
    if let Err(error) = api::register_views(OWNER, vec![spec], cx) {
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

impl PluginView for NotifySettings {
    fn open(&mut self, _: &api::serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}

impl Render for NotifySettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prefs = self.plugin.borrow().prefs;
        let switch = |id: &'static str,
                      label: SharedString,
                      description: Option<SharedString>,
                      get: fn(&Prefs) -> bool,
                      set: fn(&mut Prefs, bool)| {
            let write = self.plugin.clone();
            let row = SettingsRow::new(label.clone()).child(
                Switch::new(id)
                    .checked(get(&prefs))
                    .accessibility_label(label)
                    .on_change(move |value, window, _| {
                        let mut plugin = write.borrow_mut();
                        set(&mut plugin.prefs, value);
                        let prefs = plugin.prefs;
                        plugin.store.save_prefs(prefs);
                        window.refresh();
                    }),
            );
            match description {
                Some(description) => row.description(description),
                None => row,
            }
        };
        // 系统授权放在最前面：没授权的话下面怎么开都弹不出来。
        SettingsPage::new("notifications-settings")
            .child(
                SettingsSection::new()
                    .title(t!("notifications.section.system"))
                    .child(permission_status(cx)),
            )
            .child(
                SettingsSection::new()
                    .title(t!("notifications.section.agent"))
                    .child(switch(
                        "approval",
                        t!("notifications.prefs.approval").into(),
                        None,
                        |p| p.approval,
                        |p, v| p.approval = v,
                    ))
                    .child(switch(
                        "input",
                        t!("notifications.prefs.input").into(),
                        Some(t!("notifications.prefs.input_description").into()),
                        |p| p.input,
                        |p, v| p.input = v,
                    ))
                    .child(switch(
                        "success",
                        t!("notifications.prefs.success").into(),
                        None,
                        |p| p.success,
                        |p, v| p.success = v,
                    ))
                    .child(switch(
                        "failure",
                        t!("notifications.prefs.failure").into(),
                        None,
                        |p| p.failure,
                        |p, v| p.failure = v,
                    )),
            )
            .child(
                SettingsSection::new()
                    .title(t!("notifications.section.terminal"))
                    .child(switch(
                        "bell",
                        t!("notifications.prefs.bell").into(),
                        Some(t!("notifications.prefs.bell_description").into()),
                        |p| p.bell,
                        |p, v| p.bell = v,
                    )),
            )
    }
}

/// 系统通知的状态，并给出按钮：没问过的先问，别的都能一键去系统设置里本应用那一页。有挡着
/// 提醒的写明是哪项。还在读、这个系统没有通知时只写一句。
fn permission_status(cx: &mut App) -> AnyElement {
    let permission = api::host(cx).notification_permission(cx);
    let Some(line) = permission_line(&permission) else {
        let text = match permission {
            NotificationPermission::Unsupported => t!("notifications.permission.unsupported"),
            _ => t!("notifications.permission.checking"),
        };
        return div()
            .type_style(TYPE.ui)
            .text_color(rgb(ui_theme::muted()))
            .child(text.into_owned())
            .into_any_element();
    };
    let color = rgb(match line.severity {
        Severity::Ok => ui_theme::ok(),
        Severity::Error => ui_theme::bad(),
        Severity::Warning => ui_theme::need(),
        Severity::Info => ui_theme::muted(),
    });
    let fix = line.fix.map(|fix| {
        let (id, label, command) = match fix {
            Fix::Request => (
                "request-notification-permission",
                t!("notifications.permission.request"),
                api::REQUEST_NOTIFICATION_PERMISSION,
            ),
            Fix::OpenSettings => (
                "open-system-notification-settings",
                t!("notifications.permission.open_settings"),
                api::OPEN_NOTIFICATION_SETTINGS,
            ),
        };
        Button::new(id)
            .small()
            .label(label)
            .on_click(move |_, _, cx| {
                let task = command.run(&(), cx);
                cx.spawn(async move |_| {
                    if let Err(error) = task.await {
                        eprintln!("[notifications] {id} 失败：{error}");
                    }
                })
                .detach();
            })
    });
    // 卡片里的一行：状态在左，处理它的按钮在右，跟别的设置项的「名字左、控件右」一致。
    div()
        .flex()
        .w_full()
        .min_h(px(28.))
        .items_center()
        .gap_2()
        .child(div().flex_none().size(px(6.)).rounded_full().bg(color))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .type_style(TYPE.ui)
                .text_color(color)
                .child(line.text),
        )
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

/// 「系统通知」那一行。声音关了不算问题：那是用户自己选的安静，提醒照样到；横幅和角标关了，
/// 人就看不见提醒。
fn permission_line(permission: &NotificationPermission) -> Option<PermissionLine> {
    let line = |text: std::borrow::Cow<'_, str>, severity, fix| PermissionLine {
        text: text.into_owned(),
        severity,
        fix,
    };
    match permission {
        NotificationPermission::Checking | NotificationPermission::Unsupported => None,
        NotificationPermission::Granted { banners, badge, .. } => match (banners, badge) {
            (true, true) => Some(line(
                t!("notifications.permission.granted"),
                Severity::Ok,
                Some(Fix::OpenSettings),
            )),
            (false, true) => Some(line(
                t!("notifications.permission.no_banners"),
                Severity::Warning,
                Some(Fix::OpenSettings),
            )),
            (true, false) => Some(line(
                t!("notifications.permission.no_badge"),
                Severity::Warning,
                Some(Fix::OpenSettings),
            )),
            (false, false) => Some(line(
                t!("notifications.permission.no_banners_or_badge"),
                Severity::Warning,
                Some(Fix::OpenSettings),
            )),
        },
        NotificationPermission::NotRequested => Some(line(
            t!("notifications.permission.not_requested"),
            Severity::Info,
            Some(Fix::Request),
        )),
        NotificationPermission::Denied => Some(line(
            t!("notifications.permission.denied"),
            Severity::Error,
            Some(Fix::OpenSettings),
        )),
        // 系统报错：回到应用时宿主会重查。
        NotificationPermission::Unavailable(reason) => {
            Some(line(reason.into(), Severity::Error, None))
        }
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
                .contains("badge")
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
