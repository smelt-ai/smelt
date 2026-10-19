//! 远程桌面页：运行时那台机器的整块屏幕，打开就能操作；切到「只看」时鼠标键盘不发（设计文档 D9）。
//!
//! 画面和鼠标键盘交给网页视图里的 noVNC，经本机转发口（`smelt_runtime_client::desktop`）
//! 和运行时连到那台机器的 VNC。VNC 只能逐键发，本机输入法拼出来的中文过不去：页面把输入法
//! 上屏的整段文字报上来，这里按顺序交给运行时，注入到那台机器当前聚焦的窗口。
//!
//! 网页视图是盖在 GPUI 上面的原生子视图，同一块区域里的 GPUI 弹层会被它挡住；这一页
//! 整块都是画面，工具条在它上面，不受影响。

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{StyledExt as _, h_flex};
use smelt_runtime_client::desktop::DesktopProxy;
use smelt_runtime_client::protocol::desktop::{DesktopTypeTextCall, DesktopTypeTextParams};
pub(crate) use smelt_ui::Button;

use crate::runtimes::Runtime;
use crate::ui_theme;

enum Status {
    Connecting,
    Connected,
    /// 连过、断了，或者压根没连上；文字给人看。
    Closed(String),
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
    let message: serde_json::Value = serde_json::from_str(body).ok()?;
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

pub(crate) struct DesktopView {
    runtime: Runtime,
    proxy: Option<DesktopProxy>,
    webview: Option<Entity<gpui_wry::WebView>>,
    status: Status,
    controlling: bool,
    /// 要打到远端的文字，按收到的顺序一段一段打，不并发，免得前后颠倒。
    typer: smol::channel::Sender<String>,
    /// 上一段文字没打进去的原因；下一段打成功就清掉。
    type_error: Option<String>,
    _typing: Task<()>,
    _page_events: Option<Task<()>>,
}

impl DesktopView {
    pub(crate) fn new(runtime: Runtime, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (typer, queue) = smol::channel::unbounded::<String>();
        let client = runtime.client().clone();
        let typing = cx.spawn(async move |this, cx| {
            while let Ok(text) = queue.recv().await {
                let client = client.clone();
                let typed = cx
                    .background_executor()
                    .spawn(async move {
                        client.call::<DesktopTypeTextCall>(
                            &DesktopTypeTextParams { text },
                            smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT,
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
        });
        let mut view = Self {
            runtime,
            proxy: None,
            webview: None,
            status: Status::Connecting,
            controlling: false,
            typer,
            type_error: None,
            _typing: typing,
            _page_events: None,
        };
        view.connect(window, cx);
        view
    }

    pub(crate) fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// 开转发口、建网页视图。重连也走这里：旧的整套丢掉重来。
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.webview = None;
        self.proxy = None;
        // 打开就是接管：开这一页多半就是要动手。只看反而要人去切。
        self.controlling = true;
        self.status = Status::Connecting;
        let proxy = match DesktopProxy::start(self.runtime.client().clone()) {
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
        self.webview = Some(cx.new(|cx| gpui_wry::WebView::new(native, window, cx)));
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

    fn on_page_event(&mut self, event: PageEvent, cx: &mut Context<Self>) {
        match event {
            PageEvent::Connected => self.status = Status::Connected,
            PageEvent::Disconnected { clean } => {
                self.controlling = false;
                self.status = Status::Closed(if clean {
                    "远程桌面已断开".to_string()
                } else {
                    "和远程桌面的连接断了".to_string()
                });
            }
            // 远端复制的文字放进本机剪贴板，跟常见远程桌面一样。
            PageEvent::Clipboard(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            PageEvent::Text(text) if self.controlling => {
                let _ = self.typer.try_send(text);
            }
            PageEvent::Text(_) => {}
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
                (self.type_error.clone().unwrap_or_default(), ui_theme::red())
            }
            Status::Connecting => ("正在连接…".to_string(), ui_theme::text_muted()),
            Status::Closed(reason) => (reason.clone(), ui_theme::text_muted()),
            Status::Connected if self.controlling => ("接管中".to_string(), ui_theme::yellow()),
            Status::Connected => ("只看".to_string(), ui_theme::text_muted()),
        };
        let controlling = self.controlling;
        h_flex()
            .w_full()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(ui_theme::hairline())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(rgb(color))
                    .child(label),
            )
            .when(closed, |d| {
                d.child(
                    Button::new("desktop-reconnect")
                        .compact()
                        .outline()
                        .label("重新连接")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.connect(window, cx);
                            cx.notify();
                        })),
                )
            })
            .child({
                let button = Button::new("desktop-control")
                    .compact()
                    .disabled(!connected)
                    .label(if controlling { "只看" } else { "接管" })
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.set_control(!controlling, cx)),
                    );
                if controlling {
                    button.outline()
                } else {
                    button.commit()
                }
            })
    }
}

impl Render for DesktopView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .bg(rgb(ui_theme::bg_stage()))
                    .children(self.webview.clone()),
            )
    }
}

impl crate::Workspace {
    /// 打开某台运行时的远程桌面。同一台已经开着就接着用，换一台就重连。
    pub(crate) fn open_desktop(
        &mut self,
        runtime: Runtime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let reuse = self
            .desktop
            .as_ref()
            .is_some_and(|view| view.read(cx).runtime().key() == runtime.key());
        if !reuse {
            self.desktop = Some(cx.new(|cx| DesktopView::new(runtime, window, cx)));
        }
        self.activate_workspace_tab(crate::WorkspaceRoute::Desktop, window, cx);
    }

    pub(crate) fn render_desktop_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(view) = self.desktop.clone() else {
            return div().into_any_element();
        };
        let name = crate::runtimes::runtime_name(view.read(cx).runtime().key(), cx);
        let header = crate::agents::product_page_chrome(self.chrome_left_guard(window))
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .font_medium()
                    .text_color(rgb(ui_theme::text_bright()))
                    .child(format!("远程桌面 · {name}")),
            )
            .child(div().flex_1())
            // 回到会话页；离开这一页时视图和连接一起关掉。
            .child(
                Button::new("desktop-close")
                    .compact()
                    .quiet()
                    .label("关闭")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.activate_workspace_tab(crate::WorkspaceRoute::Session, window, cx)
                    })),
            );
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(ui_theme::bg_stage()))
            .child(header)
            .child(div().flex_1().min_h_0().child(view))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{PageEvent, page_event};

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
