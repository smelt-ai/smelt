//! 连着一台机器的远程桌面：工具行加画面。
//!
//! 画面和鼠标键盘交给网页视图里的 noVNC，经本机转发口（`client::desktop`）和运行时连到那台
//! 机器的 VNC。VNC 只能逐键发，本机输入法拼出来的中文过不去：页面把输入法上屏的整段文字报
//! 上来，这里按顺序交给运行时，注入到那台机器当前聚焦的窗口。
//!
//! 网页视图是盖在 GPUI 上面的原生子视图，同一块区域里的 GPUI 弹层会被它挡住。这一块整块都是
//! 画面，页头和工具条在它上面不受影响；宿主的遮罩层（确认框、命令面板）开着时把它藏起来。

use std::time::Duration;

use api::client::desktop::DesktopProxy;
use api::client::protocol::desktop::{DesktopTypeTextCall, DesktopTypeTextParams};
use api::client::{DEFAULT_REQUEST_TIMEOUT, RuntimeClient};
use api::ui::Button;
use api::ui::truncated_line::truncated_line;
use api::{HostEvent, RuntimeKey};
use gpui::prelude::FluentBuilder;
use gpui::*;
use smelt_plugin_api as api;

use crate::ui_theme;
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

enum Status {
    Connecting,
    Connected,
    /// 断了，正在自动重连。
    Reconnecting,
    /// 人点了断开，或连过、断了、压根没连上，自动重连也用完了；文字给人看。
    Closed(String),
}

/// 自动重连的等待时间，用完就停，之后只等运行时重新上线或人点「重新连接」。
const RETRY_DELAYS: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
    Duration::from_secs(30),
];

fn retry_delay(retries: usize) -> Option<Duration> {
    RETRY_DELAYS.get(retries).copied()
}

/// 页面经 `window.ipc.postMessage` 报上来的事。
#[derive(Debug, PartialEq)]
enum PageEvent {
    Connected,
    Disconnected {
        clean: bool,
    },
    /// 远端复制了文字。
    Clipboard(String),
    /// 本机输入法上屏、或粘贴进来的文字，要整段打到远端。
    Text(String),
}

fn page_event(body: &str) -> Option<PageEvent> {
    let message: api::serde_json::Value = api::serde_json::from_str(body).ok()?;
    match message["event"].as_str()? {
        "connected" => Some(PageEvent::Connected),
        "disconnected" => Some(PageEvent::Disconnected {
            clean: message["clean"].as_bool().unwrap_or(false),
        }),
        "clipboard" => Some(PageEvent::Clipboard(message["text"].as_str()?.to_string())),
        "text" => Some(PageEvent::Text(message["text"].as_str()?.to_string())),
        _ => None,
    }
}

/// 那台机器眼下连着（远程机器在线；本机总连着）。
fn machine_online(machine: &RuntimeKey, cx: &App) -> bool {
    api::host(cx)
        .machines(cx)
        .iter()
        .any(|candidate| &candidate.key == machine && candidate.unavailable.is_none())
}

pub(crate) struct DesktopView {
    machine: RuntimeKey,
    machine_name: String,
    client: RuntimeClient,
    proxy: Option<DesktopProxy>,
    webview: Option<Entity<gpui_webview::WebView>>,
    status: Status,
    controlling: bool,
    /// 要打到远端的文字，按收到的顺序一段一段打，不并发，免得前后颠倒。
    typer: smol::channel::Sender<String>,
    /// 上一段文字没打进去的原因；下一段打成功就清掉。
    type_error: Option<String>,
    /// 这次断线后已经重连了几次，连上就清零。
    retries: usize,
    machine_online: bool,
    /// 宿主的遮罩层开着：网页视图藏起来让位。
    overlay_open: bool,
    _typing: Task<()>,
    _page_events: Option<Task<()>>,
    /// 排着的下一次重连；换掉或丢掉就取消了。
    _retry: Option<Task<()>>,
    _host_events: Subscription,
}

impl DesktopView {
    pub(crate) fn new(
        machine: RuntimeKey,
        client: RuntimeClient,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (typer, queue) = smol::channel::unbounded::<String>();
        let typing = cx.spawn({
            let client = client.clone();
            async move |this, cx| {
                while let Ok(text) = queue.recv().await {
                    let client = client.clone();
                    let typed = cx
                        .background_executor()
                        .spawn(async move {
                            client.call::<DesktopTypeTextCall>(
                                &DesktopTypeTextParams { text },
                                DEFAULT_REQUEST_TIMEOUT,
                            )
                        })
                        .await;
                    let reported = this.update(cx, |view, cx| {
                        view.type_error = typed.err().map(|error| format!("文字没打进去：{error}"));
                        cx.notify();
                    });
                    if reported.is_err() {
                        return;
                    }
                }
            }
        });
        let host_events =
            cx.subscribe(
                &api::events(cx),
                |this, _, event: &HostEvent, cx| match event {
                    HostEvent::MachinesChanged => this.on_machines_changed(cx),
                    HostEvent::OverlayChanged => this.sync_overlay(cx),
                    _ => {}
                },
            );
        let mut view = Self {
            machine_name: crate::machine_name(&machine, cx).unwrap_or_default(),
            machine_online: machine_online(&machine, cx),
            overlay_open: api::host(cx).overlay_open(cx),
            machine,
            client,
            proxy: None,
            webview: None,
            status: Status::Connecting,
            controlling: false,
            typer,
            type_error: None,
            retries: 0,
            _typing: typing,
            _page_events: None,
            _retry: None,
            _host_events: host_events,
        };
        view.connect(window, cx);
        view
    }

    /// 开转发口、建网页视图。重连也走这里：旧的整套丢掉重来。
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.drop_connection();
        // 打开就是接管：开这一页多半就是要动手。只看反而要人去切。
        self.controlling = true;
        self.status = Status::Connecting;
        let proxy = match DesktopProxy::start(self.client.clone()) {
            Ok(proxy) => proxy,
            Err(error) => {
                self.status = Status::Closed(format!("开不了本机转发口：{error}"));
                return;
            }
        };
        let (events, inbox) = smol::channel::unbounded::<String>();
        let origin = proxy.page_url(false);
        let origin = origin[..origin.find("/desktop.html").unwrap_or(origin.len())].to_string();
        let native = wry::WebViewBuilder::new()
            .with_url(proxy.page_url(true))
            .with_background_color((17, 17, 17, 255))
            // 只停在转发口自己的页面上，页面里的链接不会把视图带走。
            .with_navigation_handler(move |url| url.starts_with(&origin))
            .with_ipc_handler(move |request| {
                let _ = events.try_send(request.body().clone());
            })
            .build_as_child(&*window);
        let native = match native {
            Ok(native) => native,
            Err(error) => {
                self.status = Status::Closed(format!("网页视图起不来：{error}"));
                return;
            }
        };
        let overlay_open = self.overlay_open;
        self.webview = Some(cx.new(|cx| {
            let mut webview = gpui_webview::WebView::new(native, window, cx);
            if overlay_open {
                webview.hide();
            }
            webview
        }));
        self.proxy = Some(proxy);
        self._page_events = Some(cx.spawn_in(window, async move |this, cx| {
            while let Ok(body) = inbox.recv().await {
                let Some(event) = page_event(&body) else {
                    continue;
                };
                if this
                    .update(cx, |view, cx| view.on_page_event(event, cx))
                    .is_err()
                {
                    return;
                }
            }
        }));
    }

    /// 网页视图、转发口、排着的重连都放掉。
    fn drop_connection(&mut self) {
        self.webview = None;
        self.proxy = None;
        self._page_events = None;
        self._retry = None;
        self.retries = 0;
    }

    /// 人点了断开：连接放掉，留在这一页，等人点「重新连接」。换到别处再回来也会重新连上。
    fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.drop_connection();
        self.status = Status::Closed("已断开".to_string());
        cx.notify();
    }

    fn on_page_event(&mut self, event: PageEvent, cx: &mut Context<Self>) {
        match event {
            PageEvent::Connected => {
                self.status = Status::Connected;
                self.retries = 0;
                self._retry = None;
            }
            // 接管还是只看不动：重连回来还是人选的那个状态。
            PageEvent::Disconnected { clean } => self.schedule_retry(clean, cx),
            // 远端复制的文字放进本机剪贴板，跟常见远程桌面一样。
            PageEvent::Clipboard(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            PageEvent::Text(text)
                if self.controlling && matches!(self.status, Status::Connected) =>
            {
                let _ = self.typer.try_send(text);
            }
            PageEvent::Text(_) => {}
        }
        cx.notify();
    }

    /// 按退避等一会儿再重连；次数用完就停下，等运行时重新上线或人点按钮。
    fn schedule_retry(&mut self, clean: bool, cx: &mut Context<Self>) {
        let Some(delay) = retry_delay(self.retries) else {
            self._retry = None;
            self.status = Status::Closed(if clean {
                "远程桌面已断开".to_string()
            } else {
                "和远程桌面的连接断了".to_string()
            });
            return;
        };
        self.retries += 1;
        self.status = Status::Reconnecting;
        self._retry = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |view, cx| view.reconnect_page(cx));
        }));
    }

    /// 让页面重新连一次，转发口和网页视图都接着用。
    fn reconnect_page(&mut self, cx: &mut Context<Self>) {
        self._retry = None;
        let Some(webview) = &self.webview else {
            return;
        };
        let _ = webview
            .read(cx)
            .raw()
            .evaluate_script("window.smelt && window.smelt.reconnect()");
    }

    /// 机器变了：改名跟着换；运行时重新上线就不等退避，次数用完了也再来一轮。
    fn on_machines_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(name) = crate::machine_name(&self.machine, cx)
            && name != self.machine_name
        {
            self.machine_name = name;
            cx.notify();
        }
        let online = machine_online(&self.machine, cx);
        let came_back = online && !self.machine_online;
        self.machine_online = online;
        // 网页视图都没建起来、人自己断开的，重连不了，留着原来的话和「重新连接」按钮。
        if !came_back
            || self.webview.is_none()
            || !matches!(self.status, Status::Reconnecting | Status::Closed(_))
        {
            return;
        }
        self.retries = 0;
        self.status = Status::Reconnecting;
        self.reconnect_page(cx);
        cx.notify();
    }

    /// 宿主的遮罩层开了就藏起网页视图，关了再露出来。
    fn sync_overlay(&mut self, cx: &mut Context<Self>) {
        let open = api::host(cx).overlay_open(cx);
        if open == self.overlay_open {
            return;
        }
        self.overlay_open = open;
        if let Some(webview) = &self.webview {
            webview.update(cx, |webview, _| {
                if open {
                    webview.hide();
                } else {
                    webview.show();
                }
            });
        }
        cx.notify();
    }

    /// 接管或交还。noVNC 的只看开关在页面里，交还后鼠标键盘都不发。
    fn set_control(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(webview) = &self.webview else {
            return;
        };
        let raw = webview.read(cx).raw();
        let _ = raw.evaluate_script(&format!("window.smelt && window.smelt.setControl({on})"));
        if on {
            let _ = raw.focus();
        }
        self.controlling = on;
        cx.notify();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let connected = matches!(self.status, Status::Connected);
        let closed = matches!(self.status, Status::Closed(_));
        let (label, color) = match &self.status {
            Status::Connected if self.type_error.is_some() => {
                (self.type_error.clone().unwrap_or_default(), ui_theme::bad())
            }
            Status::Connecting => ("正在连接…".to_string(), ui_theme::muted()),
            Status::Reconnecting => ("连接断了，正在重连…".to_string(), ui_theme::muted()),
            Status::Closed(reason) => (reason.clone(), ui_theme::muted()),
            Status::Connected if self.controlling => ("接管中".to_string(), ui_theme::need()),
            Status::Connected => ("只看".to_string(), ui_theme::muted()),
        };
        let controlling = self.controlling;
        // 页名「远程桌面」在外壳画的标题带上；这一行写连的哪台、连得怎么样，和操作。
        page_toolbar(self.machine_name.clone())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .type_style(TYPE.text_xsmall)
                    .text_color(rgb(color))
                    .child(truncated_line(label)),
            )
            .when(closed, |d| {
                d.child(
                    Button::new("desktop-reconnect")
                        .label("重新连接")
                        .primary()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.connect(window, cx);
                            cx.notify();
                        })),
                )
            })
            .when(!closed, |d| {
                d.child({
                    let button = Button::new("desktop-control")
                        .disabled(!connected)
                        .label(if controlling { "只看" } else { "接管" })
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.set_control(!controlling, cx)),
                        );
                    if controlling {
                        button
                    } else {
                        button.primary()
                    }
                })
                // 远程桌面是点开的一页，换到别处就放掉，不需要「关闭」。这里留的是断开：
                // 连接放掉，留在这一页。
                .child(
                    Button::new("desktop-disconnect")
                        .quiet()
                        .label("断开")
                        .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
                )
            })
    }
}

/// 远程桌面的工具行：最前面是连的哪台机器。
pub(crate) fn page_toolbar(machine: String) -> Div {
    api::ui::page_toolbar().gap_3().child(
        div()
            .flex_shrink_0()
            .type_style(TYPE.ui)
            .text_color(rgb(ui_theme::text()))
            .child(machine),
    )
}

impl Render for DesktopView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().children(self.webview.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::{PageEvent, RETRY_DELAYS, page_event, retry_delay};

    #[test]
    fn retry_backs_off_and_stops_at_the_cap() {
        let delays: Vec<_> = (0..).map_while(retry_delay).collect();
        assert_eq!(delays, RETRY_DELAYS);
        assert!(delays.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(retry_delay(RETRY_DELAYS.len()), None);
    }

    #[test]
    fn page_events_are_parsed_and_junk_is_ignored() {
        assert_eq!(
            page_event(r#"{"event":"connected"}"#),
            Some(PageEvent::Connected)
        );
        assert_eq!(
            page_event(r#"{"event":"disconnected","clean":true}"#),
            Some(PageEvent::Disconnected { clean: true })
        );
        assert_eq!(
            page_event(r#"{"event":"clipboard","text":"北京"}"#),
            Some(PageEvent::Clipboard("北京".into()))
        );
        assert_eq!(
            page_event(r#"{"event":"text","text":"北京 天气"}"#),
            Some(PageEvent::Text("北京 天气".into()))
        );
        assert_eq!(page_event("not json"), None);
        assert_eq!(page_event(r#"{"event":"other"}"#), None);
    }
}
