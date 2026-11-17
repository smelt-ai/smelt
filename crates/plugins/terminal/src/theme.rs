//! 把当前生效的终端配色发给本机运行时：它拿来应答 OSC 颜色查询，也转给移动端。
//!
//! 手机渲染的是同一份 PTY 字节流，TUI 会用 OSC 11 查底色来挑灰度。手机若用自己写死的配色，
//! TUI 以为的底色跟实际底色对不上，就是对比度问题。所以颜色只有桌面这一份，跟着主题变。
//!
//! 什么时候发：登记时一次、换了主题（宿主装上新的当前主题）、本机运行时重新连上。
//! 连着换几次时只发最后一份，同一时刻只有一次调用在路上。

use gpui::{App, Global};
use smelt_plugin_api::model::control_api::TerminalThemePublish;
use smelt_plugin_api::model::terminal_theme::{TERMINAL_THEME_VERSION, TerminalThemeSnapshot};
use smelt_plugin_api::{self as api, HostEvent, RuntimeKey};

use crate::terminal;
use crate::terminal_view::{search_hit_bg, sel_bg};

pub(crate) fn register(cx: &mut App) {
    cx.observe_global::<api::ui::theme_catalog::ActiveTheme>(publish)
        .detach();
    cx.subscribe(&api::events(cx), |_, event: &HostEvent, cx| {
        if *event == HostEvent::Reconnected(RuntimeKey::Local) {
            publish(cx);
        }
    })
    .detach();
    publish(cx);
}

/// 按当前主题组装的配色快照。
pub(crate) fn current() -> TerminalThemeSnapshot {
    TerminalThemeSnapshot {
        version: TERMINAL_THEME_VERSION,
        dark: terminal::is_dark(),
        background: terminal::default_bg(),
        foreground: terminal::default_fg(),
        // 光标是实心块反显（见 terminal_view 的 in_block 分支），块本身就是前景色。
        cursor: terminal::default_fg(),
        selection: sel_bg(),
        palette: terminal::ansi_palette().to_vec(),
        search_hit: search_hit_bg(false),
        search_hit_current: search_hit_bg(true),
    }
}

/// 发送队列：在路上的那次回来之前，后来的只留最新一份。
#[derive(Default)]
struct Publisher {
    sending: bool,
    pending: Option<TerminalThemeSnapshot>,
}

impl Global for Publisher {}

fn publish(cx: &mut App) {
    let theme = current();
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
        // 运行时还没起来或正在重启：重新连上时会再发。
        if let Err(error) = result {
            eprintln!("[terminal] 发布终端配色失败：{error}");
        }
        cx.update(send_next);
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::current;
    use crate::terminal::lock_theme_globals;
    use smelt_plugin_api::model::terminal_theme::TerminalThemeSnapshot;
    use smelt_plugin_api::ui::ui_theme;

    /// 守护先于桌面起来、还没收到快照时用默认值：它要跟深色主题实际发的一致。
    #[test]
    fn dark_snapshot_matches_runtime_fallback() {
        let _guard = lock_theme_globals();
        ui_theme::set_light(false);
        assert_eq!(current(), TerminalThemeSnapshot::default());
    }

    #[test]
    fn snapshot_follows_theme_mode() {
        let _guard = lock_theme_globals();
        ui_theme::set_light(true);
        let light = current();
        assert!(!light.dark);
        assert_eq!(light.background, ui_theme::bg());
        assert_ne!(
            light.palette,
            TerminalThemeSnapshot::default().palette,
            "浅色主题要发浅色板，不能原样发深色板"
        );
        ui_theme::set_light(false);
    }
}
