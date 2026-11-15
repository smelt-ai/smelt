//! 远程桌面：运行时那台机器的整块屏幕，打开就能操作；切到「只看」时鼠标键盘不发（设计文档 D9）。
//! 宿主登记的视图（[`DESKTOP_VIEW`]），打开时放进出口。
//!
//! 画面和鼠标键盘交给网页视图里的 noVNC，经本机转发口（`smelt_runtime_client::desktop`）
//! 和运行时连到那台机器的 VNC。VNC 只能逐键发，本机输入法拼出来的中文过不去：页面把输入法
//! 上屏的整段文字报上来，这里按顺序交给运行时，注入到那台机器当前聚焦的窗口。
//!
//! 网页视图是盖在 GPUI 上面的原生子视图，同一块区域里的 GPUI 弹层会被它挡住；这一块
//! 整块都是画面，页头和工具条在它上面，不受影响。

use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{StyledExt as _, h_flex};
use smelt_ui::truncated_line::truncated_line;
use smelt_runtime_client::desktop::DesktopProxy;
use smelt_runtime_client::protocol::desktop::{DesktopTypeTextCall, DesktopTypeTextParams};
pub(crate) use smelt_ui::Button;

use crate::runtimes::Runtime;
use crate::ui_theme;

enum Status {
    Connecting,
    Connected,
    /// 断了，正在自动重连。
    Reconnecting,
    /// 连过、断了，或者压根没连上，自动重连也用完了；文字给人看。
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
    webview: Option<Entity<gpui_webview::WebView>>,
    status: Status,
    controlling: bool,
    /// 要打到远端的文字，按收到的顺序一段一段打，不并发，免得前后颠倒。
    typer: smol::channel::Sender<String>,
    /// 上一段文字没打进去的原因；下一段打成功就清掉。
    type_error: Option<String>,
    /// 这次断线后已经重连了几次，连上就清零。
    retries: usize,
    runtime_online: bool,
    _typing: Task<()>,
    _page_events: Option<Task<()>>,
    /// 排着的下一次重连；换掉或丢掉就取消了。
    _retry: Option<Task<()>>,
    _runtime_status: Subscription,
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
        let runtime_online = crate::runtimes::runtime_online(runtime.key(), cx);
        let runtime_status = cx.observe_global::<crate::runtimes::RuntimeHub>(|this, cx| {
            let online = crate::runtimes::runtime_online(this.runtime.key(), cx);
            let came_back = online && !this.runtime_online;
            this.runtime_online = online;
            if came_back {
                this.on_runtime_back(cx);
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
            retries: 0,
            runtime_online,
            _typing: typing,
            _page_events: None,
            _retry: None,
            _runtime_status: runtime_status,
        };
        view.connect(window, cx);
        view
    }

    /// 开转发口、建网页视图。重连也走这里：旧的整套丢掉重来。
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.webview = None;
        self.proxy = None;
        self.retries = 0;
        self._retry = None;
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
        self.webview = Some(cx.new(|cx| gpui_webview::WebView::new(native, window, cx)));
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

    /// 运行时重新上线：不用等退避，次数用完了也再来一轮。
    fn on_runtime_back(&mut self, cx: &mut Context<Self>) {
        // 网页视图都没建起来时重连不了，留着原来的错误和「重新连接」按钮。
        if self.webview.is_none()
            || !matches!(self.status, Status::Reconnecting | Status::Closed(_))
        {
            return;
        }
        self.retries = 0;
        self.status = Status::Reconnecting;
        self.reconnect_page(cx);
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
        h_flex()
            .w_full()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(ui_theme::hair())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_xs()
                    .text_color(rgb(color))
                    .child(truncated_line(label)),
            )
            .when(closed, |d| {
                d.child(
                    Button::new("desktop-reconnect")
                        .small()
                        .label("重新连接")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.connect(window, cx);
                            cx.notify();
                        })),
                )
            })
            .child({
                let button = Button::new("desktop-control")
                    .small()
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
                    .bg(rgb(ui_theme::bg()))
                    .children(self.webview.clone()),
            )
    }
}

/// 远程桌面视图的 id。哪页都没摆它，打开时放进出口。
pub(crate) const DESKTOP_VIEW: &str = "smelt/desktop";

/// 远程桌面视图，交给 `host_views` 和宿主别的视图一起登记。不按机器给入口（`per_machine`）：
/// 只有装了远程桌面、连着的机器才有入口，在侧栏机器那一行。
pub(crate) fn view_spec(workspace: WeakEntity<crate::Workspace>) -> smelt_plugin_api::ViewSpec {
    smelt_plugin_api::ViewSpec {
        name: "desktop".into(),
        title: "远程桌面".into(),
        icon: "smelt-icons/monitor.svg".into(),
        per_machine: false,
        // 页头自己画顶上那条。
        draws_top_bar: true,
        settings: None,
        build: std::rc::Rc::new(
            move |window, cx| -> std::rc::Rc<dyn smelt_plugin_api::ViewHandle> {
                let workspace = workspace.clone();
                std::rc::Rc::new(cx.new(|cx| DesktopPage::new(workspace, window, cx)))
            },
        ),
    }
}

/// 远程桌面视图：页头加上那台机器的画面。打开时参数带机器（`{"machine": …}`）。看不见时
/// 连接整套放掉，不在后台推画面；再露出来连回上次那台。
struct DesktopPage {
    workspace: WeakEntity<crate::Workspace>,
    machine: Option<crate::RuntimeKey>,
    view: Option<Entity<DesktopView>>,
    _shown: Subscription,
}

impl DesktopPage {
    fn new(
        workspace: WeakEntity<crate::Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let _shown = cx.subscribe_in(
            &smelt_plugin_api::events(cx),
            window,
            |this: &mut Self, _, event: &smelt_plugin_api::HostEvent, window, cx| {
                if matches!(event, smelt_plugin_api::HostEvent::ShownViewsChanged) {
                    this.sync_shown(window, cx);
                }
            },
        );
        Self {
            workspace,
            machine: None,
            view: None,
            _shown,
        }
    }

    /// 看得见就连着，看不见就断开。
    fn sync_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !crate::plugin_host::view_shown(DESKTOP_VIEW, cx) {
            if self.view.take().is_some() {
                cx.notify();
            }
            return;
        }
        if self.view.is_none()
            && let Some(runtime) = self
                .machine
                .as_ref()
                .and_then(|machine| crate::runtimes::runtime(machine, cx))
        {
            self.view = Some(cx.new(|cx| DesktopView::new(runtime, window, cx)));
            cx.notify();
        }
    }
}

impl smelt_plugin_api::PluginView for DesktopPage {
    /// 换一台机器就重连；参数里没有 `machine`（不是本机的 `null`，是没这个字段）时接着看
    /// 上次那台。
    fn open(&mut self, args: &serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(machine) = args
            .get("machine")
            .and_then(smelt_plugin_api::machine_from_json)
            && self.machine.as_ref() != Some(&machine)
        {
            self.machine = Some(machine);
            self.view = None;
        }
        self.sync_shown(window, cx);
    }
}

impl Render for DesktopPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match &self.machine {
            Some(machine) => format!("远程桌面 · {}", crate::runtimes::runtime_name(machine, cx)),
            None => "远程桌面".to_string(),
        };
        let workspace = self.workspace.clone();
        let header = smelt_plugin_api::host(cx)
            .page_header(window, cx)
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .font_medium()
                    .text_color(rgb(ui_theme::text()))
                    .child(title),
            )
            .child(div().flex_1())
            // 出口放回默认内容（会话）；看不见了连接随之放掉。
            .child(
                Button::new("desktop-close")
                    .small()
                    .quiet()
                    .label("关闭")
                    .on_click(move |_, window, cx| {
                        let _ = workspace.update(cx, |workspace, cx| {
                            workspace.reset_outlet(window, cx);
                        });
                    }),
            );
        let body = match &self.view {
            Some(view) => view.clone().into_any_element(),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(rgb(ui_theme::muted()))
                .child("这台机器现在连不上")
                .into_any_element(),
        };
        div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(ui_theme::bg()))
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}

impl crate::Workspace {
    /// 打开某台机器的远程桌面：放进出口。同一台开着就接着用，换一台就重连。
    pub(crate) fn open_desktop(
        &mut self,
        machine: &crate::RuntimeKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let args = serde_json::json!({ "machine": smelt_plugin_api::machine_to_json(machine) });
        self.open_view(DESKTOP_VIEW, args, window, cx);
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
