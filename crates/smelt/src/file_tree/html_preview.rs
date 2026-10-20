//! 文件面板里的 HTML 预览：编辑器当前内容交给原生网页视图显示。
//!
//! smelt 跑在 gpui-pre 上，网页视图是直接贴在窗口上的原生子视图：它盖住同一块区域里的
//! GPUI 弹层，也不会被外层容器裁切；所在区域某一帧没画到（面板收起、切页签、换页面），
//! 它照样留在原位。这里把「藏起原生视图」挂在一个随网页视图一起画的元素的元素状态上：
//! 哪一帧没画到它，GPUI 在这一帧结束时丢掉旧状态，原生视图跟着藏起来；再画出来时，
//! 网页视图自己的 prepaint 会把它重新显示。有对话框开着时不画网页视图，免得把对话框挡住。
//!
//! 内容用 `load_html` 整页载入，没有基准地址：相对路径引用的外部 css、脚本、图片加载不到，
//! 单文件页面不受影响。页面里的链接不在预览里跳转，交给系统浏览器打开。

use std::rc::{Rc, Weak};

use gpui::*;
use gpui_component::{ActiveTheme as _, WindowExt as _};
use gpui_webview::{WebView, WebViewHandle};

pub(crate) struct HtmlPreview {
    webview: Option<Entity<WebView>>,
    /// 藏起原生视图的动作，给 [`HideWhenNotPainted`] 拿弱引用。元素状态里不放网页视图的
    /// 强引用，网页视图的生命周期只跟着这个预览走，不会被窗口里残留的帧状态拖长。
    hide_native: Option<Rc<dyn Fn()>>,
    error: Option<SharedString>,
    loaded: Option<SharedString>,
    _open_links: Option<Task<()>>,
}

impl HtmlPreview {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (links, inbox) = smol::channel::unbounded::<String>();
        let builder = wry::WebViewBuilder::new().with_navigation_handler(move |url| {
            // `load_html` 的整页载入和页内锚点落在 about:（WebView2 上是 data:），
            // 其余都是点了链接。
            if url.starts_with("about:") || url.starts_with("data:") {
                return true;
            }
            let _ = links.try_send(url);
            false
        });
        let webview = match WebView::build(builder, window, cx) {
            Ok(webview) => webview,
            Err(error) => {
                return Self {
                    webview: None,
                    hide_native: None,
                    error: Some(format!("网页视图起不来：{error}").into()),
                    loaded: None,
                    _open_links: None,
                };
            }
        };
        let handle: WebViewHandle = webview.handle();
        let hide_native: Rc<dyn Fn()> = Rc::new(move || {
            let _ = handle.raw().set_visible(false);
        });
        let open_links = cx.spawn(async move |_, cx| {
            while let Ok(url) = inbox.recv().await {
                cx.update(|cx| cx.open_url(&url));
            }
        });
        Self {
            webview: Some(cx.new(|_| webview)),
            hide_native: Some(hide_native),
            error: None,
            loaded: None,
            _open_links: Some(open_links),
        }
    }

    /// 内容变了才整页重载：重载会丢掉页面的滚动位置和脚本状态。
    pub(crate) fn set_html(&mut self, html: &SharedString, cx: &mut Context<Self>) {
        if self.loaded.as_ref() == Some(html) {
            return;
        }
        let Some(webview) = &self.webview else {
            return;
        };
        if let Err(error) = webview.read(cx).raw().load_html(html) {
            self.error = Some(format!("页面载入失败：{error}").into());
        } else {
            self.error = None;
            self.loaded = Some(html.clone());
        }
        cx.notify();
    }
}

impl Render for HtmlPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let surface = div().size_full().relative();
        if let Some(error) = &self.error {
            return surface
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(error.clone());
        }
        let (Some(webview), Some(hide_native)) = (&self.webview, &self.hide_native) else {
            return surface;
        };
        if window.has_active_dialog(cx) {
            return surface;
        }
        surface.child(webview.clone()).child(HideWhenNotPainted {
            hide: Rc::downgrade(hide_native),
        })
    }
}

/// 和网页视图画在一起的空元素，见模块注释。
struct HideWhenNotPainted {
    hide: Weak<dyn Fn()>,
}

/// 只活在元素状态里；没被画到的那一帧结束时被丢掉，丢掉就藏起原生视图。
struct HideOnDrop(Weak<dyn Fn()>);

impl Drop for HideOnDrop {
    fn drop(&mut self) {
        if let Some(hide) = self.0.upgrade() {
            hide();
        }
    }
}

impl IntoElement for HideWhenNotPainted {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for HideWhenNotPainted {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some("html-preview-visibility".into())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            position: Position::Absolute,
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let id = id.expect("HideWhenNotPainted 总有 id");
        // 必须惰性构造：多造一个再丢掉，丢掉那一下就会把原生视图藏起来。
        let hide = self.hide.clone();
        window.with_element_state(id, |state: Option<HideOnDrop>, _| {
            ((), state.unwrap_or_else(|| HideOnDrop(hide)))
        });
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::prelude::FluentBuilder as _;
    use gpui::{Context, IntoElement, Render, Styled as _, Window, div};

    use super::HideWhenNotPainted;

    struct Host {
        painted: bool,
        hide: Rc<dyn Fn()>,
    }

    impl Render for Host {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().when(self.painted, |el| {
                el.child(HideWhenNotPainted {
                    hide: Rc::downgrade(&self.hide),
                })
            })
        }
    }

    /// 连着几帧都画到时一次也不藏；某一帧没画到，这一帧结束就藏一次。
    #[gpui::test]
    fn native_view_hides_only_after_a_frame_without_it(cx: &mut gpui::TestAppContext) {
        let hides = Rc::new(Cell::new(0));
        let counter = hides.clone();
        let hide: Rc<dyn Fn()> = Rc::new(move || counter.set(counter.get() + 1));
        let (host, cx) = cx.add_window_view(|_, _| Host {
            painted: true,
            hide,
        });
        for _ in 0..3 {
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
        }
        assert_eq!(hides.get(), 0, "一直画着的网页视图不应被藏起");

        cx.update(|window, cx| {
            host.update(cx, |host, cx| {
                host.painted = false;
                cx.notify();
            });
            let _ = window.draw(cx);
        });
        assert_eq!(hides.get(), 1, "没画到的那一帧结束时应藏起一次");
    }
}
