//! 远程桌面画布能力：本地 noVNC 转发口（[`DesktopProxy`]）与原生子窗口 [`wry::WebView`] 封装。
//!
//! 只负责画布、输入法/剪贴板 IPC 桥接与遮罩层显隐，不含工具栏、重连策略、机器探测或插件登记——
//! 这些全部由 JS 插件（`plugins/desktop/`）实现。

mod webview;

use std::rc::Rc;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Task, Window, div, rgb,
};
use smelt_plugin_api as api;
use smelt_plugin_api::HostEvent;
use smelt_plugin_api::client::RuntimeClient;
use smelt_plugin_api::client::desktop::DesktopProxy;
use webview::NativeWebView;

/// 画布发往 JS 层的原生事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesktopCanvasEvent {
    Connected,
    Disconnected { clean: bool },
    Clipboard(String),
    Text(String),
    Error { kind: &'static str, message: String },
}

pub type DesktopEventHandler = Rc<dyn Fn(DesktopCanvasEvent)>;

/// 页面经 `window.ipc.postMessage` 报上来的消息。
fn parse_page_event(body: &str) -> Option<DesktopCanvasEvent> {
    let message: serde_json::Value = serde_json::from_str(body).ok()?;
    match message["event"].as_str()? {
        "connected" => Some(DesktopCanvasEvent::Connected),
        "disconnected" => Some(DesktopCanvasEvent::Disconnected {
            clean: message["clean"].as_bool().unwrap_or(false),
        }),
        "clipboard" => Some(DesktopCanvasEvent::Clipboard(
            message["text"].as_str()?.to_owned(),
        )),
        "text" => Some(DesktopCanvasEvent::Text(
            message["text"].as_str()?.to_owned(),
        )),
        _ => None,
    }
}

/// 单台机器的远程桌面原生画布：持有本地转发口与原生 `wry::WebView`，释放时自动断连。
pub struct DesktopCanvas {
    proxy: Option<DesktopProxy>,
    webview: Option<Entity<NativeWebView>>,
    overlay_open: bool,
    _page_events: Option<Task<()>>,
    _overlay_sub: Subscription,
}

impl DesktopCanvas {
    pub fn new(
        client: RuntimeClient,
        initial_control: bool,
        on_event: DesktopEventHandler,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let overlay_open = api::host(cx).overlay_open(cx);
        let overlay_sub = cx.subscribe(&api::events(cx), |this, _, event: &HostEvent, cx| {
            if matches!(event, HostEvent::OverlayChanged) {
                this.sync_overlay(cx);
            }
        });
        let mut canvas = Self {
            proxy: None,
            webview: None,
            overlay_open,
            _page_events: None,
            _overlay_sub: overlay_sub,
        };
        canvas.start(client, initial_control, on_event, window, cx);
        canvas
    }

    fn start(
        &mut self,
        client: RuntimeClient,
        initial_control: bool,
        on_event: DesktopEventHandler,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let proxy = match DesktopProxy::start(client) {
            Ok(proxy) => proxy,
            Err(error) => {
                on_event(DesktopCanvasEvent::Error {
                    kind: "proxy",
                    message: error.to_string(),
                });
                return;
            }
        };
        let (events, inbox) = smol::channel::unbounded::<String>();
        let origin = proxy.page_url(false);
        let origin = origin[..origin.find("/desktop.html").unwrap_or(origin.len())].to_string();
        let native = wry::WebViewBuilder::new()
            .with_url(proxy.page_url(initial_control))
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
                on_event(DesktopCanvasEvent::Error {
                    kind: "webview",
                    message: error.to_string(),
                });
                return;
            }
        };
        let overlay_open = self.overlay_open;
        self.webview = Some(cx.new(|cx| {
            let mut webview = NativeWebView::new(native, window, cx);
            if overlay_open {
                webview.hide();
            }
            webview
        }));
        self.proxy = Some(proxy);
        self._page_events = Some(cx.spawn_in(window, async move |_this, _cx| {
            while let Ok(body) = inbox.recv().await {
                if let Some(event) = parse_page_event(&body) {
                    on_event(event);
                }
            }
        }));
    }

    /// 切换接管 / 只看。
    pub fn set_control(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(webview) = &self.webview else {
            return;
        };
        let raw = webview.read(cx).raw();
        let _ = raw.evaluate_script(&format!("window.smelt && window.smelt.setControl({on})"));
        if on {
            let _ = raw.focus();
        }
    }

    /// 让页面内 noVNC 重新连一次（复用已有转发口与 WebView）。
    pub fn reconnect(&mut self, cx: &mut Context<Self>) {
        let Some(webview) = &self.webview else {
            return;
        };
        let _ = webview
            .read(cx)
            .raw()
            .evaluate_script("window.smelt && window.smelt.reconnect()");
    }

    /// 宿主的遮罩层开了就藏起原生网页视图，关了再露出来。
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
}

impl Render for DesktopCanvas {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0x111111))
            .children(self.webview.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_page_ipc_messages() {
        assert_eq!(
            parse_page_event(r#"{"event":"connected"}"#),
            Some(DesktopCanvasEvent::Connected)
        );
        assert_eq!(
            parse_page_event(r#"{"event":"disconnected","clean":true}"#),
            Some(DesktopCanvasEvent::Disconnected { clean: true })
        );
        assert_eq!(
            parse_page_event(r#"{"event":"disconnected"}"#),
            Some(DesktopCanvasEvent::Disconnected { clean: false })
        );
        assert_eq!(
            parse_page_event(r#"{"event":"clipboard","text":"hello"}"#),
            Some(DesktopCanvasEvent::Clipboard("hello".into()))
        );
        assert_eq!(
            parse_page_event(r#"{"event":"text","text":"中文"}"#),
            Some(DesktopCanvasEvent::Text("中文".into()))
        );
        assert_eq!(parse_page_event(r#"{"event":"unknown"}"#), None);
        assert_eq!(parse_page_event("not json"), None);
    }
}
