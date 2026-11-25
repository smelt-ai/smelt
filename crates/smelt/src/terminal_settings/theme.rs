//! 把当前生效的终端配色发给本机运行时（OSC 颜色查询与移动端同步）。

use gpui::{App, Global};
use smelt_plugin_api::model::control_api::TerminalThemePublish;
use smelt_plugin_api::model::terminal_theme::TerminalThemeSnapshot;
use smelt_plugin_api::{self as api, HostEvent, RuntimeKey};

pub(crate) fn register(cx: &mut App) {
    cx.observe_global::<smelt_ui::theme_catalog::ActiveTheme>(publish)
        .detach();
    cx.subscribe(&api::events(cx), |_, event: &HostEvent, cx| {
        if *event == HostEvent::Reconnected(RuntimeKey::Local) {
            publish(cx);
        }
    })
    .detach();
    publish(cx);
}

#[derive(Default)]
struct Publisher {
    sending: bool,
    pending: Option<TerminalThemeSnapshot>,
}

impl Global for Publisher {}

fn publish(cx: &mut App) {
    let theme = smelt_terminal::theme_snapshot();
    let publisher = cx.default_global::<Publisher>();
    publisher.pending = Some(theme);
    if !publisher.sending {
        send_next(cx);
    }
}

fn send_next(cx: &mut App) {
    let publisher = cx.default_global::<Publisher>();
    let Some(theme) = publisher.pending.take() else {
        publisher.sending = false;
        return;
    };
    publisher.sending = true;
    let task = api::call::<TerminalThemePublish>(&RuntimeKey::Local, theme, api::CALL_TIMEOUT, cx);
    cx.spawn(async move |cx| {
        let result = task.await;
        if let Err(error) = result {
            eprintln!("[terminal] 发布终端配色失败：{error}");
        }
        cx.update(send_next);
    })
    .detach();
}
