//! 设置页：通知。按来源分 Agent 和终端；系统授权是整页的前提，出问题时挂在页面标题旁。

use super::*;

pub(super) fn notify_page() -> SettingPage {
    let page = SettingPage::new("通知")
        .group(SettingGroup::new().title("Agent 什么时候叫你").items(vec![
            notify_switch("等待批准", None, |c| c.notify_approval, |c, v| {
                c.notify_approval = v
            })
            .keywords(["通知", "notification", "审批"]),
            notify_switch(
                "等待输入",
                Some("提问或等你继续时"),
                |c| c.notify_input,
                |c, v| c.notify_input = v,
            ),
            notify_switch("任务完成", None, |c| c.notify_success, |c, v| {
                c.notify_success = v
            }),
            notify_switch("任务失败", None, |c| c.notify_failure, |c, v| {
                c.notify_failure = v
            }),
        ]))
        .group(
            SettingGroup::new().title("终端").item(
                notify_switch(
                    "响铃",
                    Some("终端输出 BEL 时提醒"),
                    |c| c.notify_terminal_bell,
                    |c, v| c.notify_terminal_bell = v,
                )
                .keywords(["通知", "notification", "响铃", "bell"]),
            ),
        );
    // 系统通知只有 macOS 有。别的平台状态固定是「不可用」，挂出来只会一直报错。
    #[cfg(target_os = "macos")]
    let page = page.title_suffix(|_, cx| permission_status(cx));
    page
}

/// 一个提醒开关。没读到设置时按开着算，跟 AgentHostState 的默认值一致。
fn notify_switch(
    label: &'static str,
    description: Option<&'static str>,
    get: fn(&AgentHostState) -> bool,
    set: fn(&mut AgentHostState, bool),
) -> SettingItem {
    let item = SettingItem::new(
        label,
        SettingField::switch(
            move |cx: &App| cx.try_global::<AgentHostState>().map(get).unwrap_or(true),
            move |v: bool, cx: &mut App| apply_notify_prefs(|c| set(c, v), cx),
        ),
    );
    match description {
        Some(description) => item.description(description),
        None => item,
    }
}

/// 系统授权不正常时，页面标题旁写明是什么情况，并给出能修的按钮。已授权或还在读时不出。
#[cfg(target_os = "macos")]
fn permission_status(cx: &App) -> AnyElement {
    use crate::status_item::SystemNotificationAuthorization as Authorization;
    let status = crate::status_item::system_notification_status();
    if matches!(
        status.authorization,
        Authorization::Authorized | Authorization::Unknown
    ) {
        return div().into_any_element();
    }
    let presentation = crate::status_item::system_notification_status_presentation(&status);
    let color = if presentation.is_error {
        cx.theme().danger
    } else {
        cx.theme().muted_foreground
    };
    h_flex()
        .items_center()
        .gap_2()
        .child(div().size(px(6.)).rounded_full().bg(color))
        .child(div().text_xs().text_color(color).child(presentation.text))
        .children(presentation.is_error.then(|| {
            Button::new("refresh-system-notification-status")
                .compact()
                .quiet()
                .label("重新检查")
                .on_click(|_, _, _| crate::status_item::refresh_system_notification_authorization())
        }))
        .children(presentation.can_open_settings.then(|| {
            Button::new("open-system-notification-settings")
                .compact()
                .label("打开系统设置 ↗")
                .on_click(|_, _, cx| {
                    cx.open_url(crate::status_item::SYSTEM_NOTIFICATION_SETTINGS_URL)
                })
        }))
        .into_any_element()
}
