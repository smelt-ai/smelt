//! 原生 `wry::WebView` 在 GPUI Fast 中的薄封装：跟随布局与裁剪区域定位、支持显隐、
//! 点击外部失焦，并在 macOS 上接入 GPUI Fast 的窗口合成层以让浮层正常盖在画布之上。

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, ContentMask, Element, ElementId, Entity, FocusHandle, Focusable, GlobalElementId,
    Hitbox, HitboxBehavior, InteractiveElement as _, IntoElement, LayoutId, MouseDownEvent,
    ParentElement as _, Pixels, Render, Size, Style, Styled as _, Window, div,
};
use wry::Rect;

#[cfg(target_os = "macos")]
mod macos_composition {
    use std::cell::Cell;
    use std::rc::Rc;

    use anyhow::{Context as _, Result};
    use gpui::{AnyWindowHandle, AsyncApp, Bounds, Window, WindowCompositionSurface};
    use objc2::rc::Retained;
    use objc2_app_kit::NSView;
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use wry::WebViewExtMacOS as _;

    pub(super) struct NativeWebViewSurface {
        surface: WindowCompositionSurface,
        original_parent: Retained<NSView>,
        scale_factor: Cell<f32>,
        window: AnyWindowHandle,
        cx: AsyncApp,
    }

    impl NativeWebViewSurface {
        pub(super) fn new(webview: &wry::WebView, window: &Window, cx: &gpui::App) -> Result<Self> {
            let composition = window.enable_window_composition()?;
            let surface = composition.create_native_surface()?;
            let attached = (|| {
                let handle = surface.platform_surface()?.platform_handle()?;
                let parent = *handle
                    .downcast::<usize>()
                    .map_err(|_| anyhow::anyhow!("native surface is not an AppKit view"))?;
                anyhow::ensure!(parent != 0, "native surface has no AppKit view");
                let view = webview.webview();
                // SAFETY: Wry 与窗口合成层在同一 UI 线程持有这两个 AppKit 视图。
                unsafe {
                    let original_parent = view.superview().context("WebView has no parent")?;
                    (&*(parent as *const NSView)).addSubview(&view);
                    Ok(original_parent)
                }
            })();
            match attached {
                Ok(original_parent) => Ok(Self {
                    surface,
                    original_parent,
                    scale_factor: Cell::new(window.scale_factor()),
                    window: window.window_handle(),
                    cx: cx.to_async(),
                }),
                Err(error) => {
                    let _ = composition.remove_surface(surface.id());
                    Err(error)
                }
            }
        }

        pub(super) fn set_scale_factor(&self, scale_factor: f32) {
            self.scale_factor.set(scale_factor);
        }

        pub(super) fn set_geometry(
            &self,
            webview: &wry::WebView,
            document: Bounds<gpui::Pixels>,
            visible: Bounds<gpui::Pixels>,
        ) -> Result<()> {
            let scale = self.scale_factor.get();
            self.surface
                .platform_surface()?
                .set_bounds(visible.to_device_pixels(scale))?;
            let offset = document.origin - visible.origin;
            webview.webview().setFrame(document_frame(
                f64::from(offset.x),
                f64::from(visible.size.height - offset.y),
                NSSize::new(
                    f64::from(document.size.width),
                    f64::from(document.size.height),
                ),
            ));
            Ok(())
        }

        pub(super) fn set_visible(&self, visible: bool) -> Result<()> {
            self.surface.platform_surface()?.set_visible(visible)
        }

        pub(super) fn release(self, webview: Rc<wry::WebView>) {
            // 实体析构时 App 可能处于借用期，推迟到前台执行器归还子视图并移除合成层。
            self.cx
                .clone()
                .spawn(async move |cx| {
                    self.original_parent.addSubview(&webview.webview());
                    let result = self.window.update(cx, |_, window, _| {
                        window
                            .enable_window_composition()?
                            .remove_surface(self.surface.id())?;
                        window.refresh();
                        anyhow::Ok(())
                    });
                    if let Ok(Err(error)) = result {
                        log::warn!("cannot release WebView composition surface: {error:#}");
                    }
                })
                .detach();
        }
    }

    pub(super) fn document_frame(left: f64, top: f64, mut size: NSSize) -> NSRect {
        size.width = size.width.ceil();
        size.height = size.height.ceil();
        NSRect::new(NSPoint::new(left, top - size.height), size)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn fractional_document_covers_clip_without_shifting_top_edge() {
            let frame = document_frame(-4.5, 145.5, NSSize::new(200.25, 140.5));
            assert_eq!(frame.size, NSSize::new(201., 141.));
            assert_eq!(frame.origin.x, -4.5);
            assert_eq!(frame.origin.y + frame.size.height, 145.5);
        }
    }
}

/// 嵌入 GPUI 视图树的原生 `wry::WebView`。
pub(crate) struct NativeWebView {
    focus_handle: FocusHandle,
    webview: Rc<wry::WebView>,
    visible: Cell<bool>,
    geometry_failed: Cell<bool>,
    #[cfg(target_os = "macos")]
    composition: Option<macos_composition::NativeWebViewSurface>,
}

impl Drop for NativeWebView {
    fn drop(&mut self) {
        self.hide();
        #[cfg(target_os = "macos")]
        if let Some(surface) = self.composition.take() {
            surface.release(self.webview.clone());
        }
    }
}

impl NativeWebView {
    pub(crate) fn new(webview: wry::WebView, window: &mut Window, cx: &mut App) -> Self {
        let _ = webview.set_bounds(Rect::default());
        #[cfg(target_os = "macos")]
        let composition = match macos_composition::NativeWebViewSurface::new(&webview, window, cx) {
            Ok(surface) => Some(surface),
            Err(error) => {
                log::warn!("WebView composition unavailable: {error:#}");
                None
            }
        };
        #[cfg(not(target_os = "macos"))]
        let _ = window;

        Self {
            focus_handle: cx.focus_handle(),
            webview: Rc::new(webview),
            visible: Cell::new(true),
            geometry_failed: Cell::new(false),
            #[cfg(target_os = "macos")]
            composition,
        }
    }

    pub(crate) fn raw(&self) -> &wry::WebView {
        &self.webview
    }

    pub(crate) fn show(&mut self) {
        let _ = self.set_visible(true);
    }

    pub(crate) fn hide(&mut self) {
        let _ = self.set_visible(false);
    }

    fn visible(&self) -> bool {
        self.visible.get()
    }

    fn set_native_visible(&self, visible: bool) -> wry::Result<()> {
        self.webview.set_visible(visible)?;
        #[cfg(target_os = "macos")]
        if let Some(surface) = &self.composition {
            surface
                .set_visible(visible)
                .map_err(|error| wry::Error::Io(std::io::Error::other(error)))?;
        }
        Ok(())
    }

    fn set_visible(&self, visible: bool) -> wry::Result<()> {
        let focus_result = if !visible && self.visible.get() {
            self.webview.focus_parent()
        } else {
            Ok(())
        };
        self.set_native_visible(visible)?;
        self.visible.set(visible);
        focus_result
    }

    fn set_geometry(
        &self,
        document: Bounds<Pixels>,
        clip: Bounds<Pixels>,
        scale_factor: f32,
    ) -> anyhow::Result<()> {
        let visible = document.intersect(&clip);
        if visible.is_empty() || !self.visible.get() {
            self.set_native_visible(false)?;
            #[cfg(target_os = "macos")]
            if let Some(surface) = &self.composition
                && self.visible.get()
                && !document.is_empty()
            {
                surface.set_geometry(&self.webview, document, document)?;
            }
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        if let Some(surface) = &self.composition {
            surface.set_scale_factor(scale_factor);
            surface.set_geometry(&self.webview, document, visible)?;
            surface.set_visible(true)?;
            self.webview.set_visible(true)?;
            return Ok(());
        }
        let rect = bounds_to_wry_rect(document, scale_factor);
        self.webview.set_bounds(rect)?;
        self.set_native_visible(true)?;
        Ok(())
    }
}

fn bounds_to_wry_rect(bounds: Bounds<Pixels>, scale_factor: f32) -> Rect {
    #[cfg(target_os = "linux")]
    {
        let scale = f64::from(scale_factor);
        Rect {
            position: wry::dpi::PhysicalPosition::new(
                (f64::from(bounds.origin.x) * scale).round() as i32,
                (f64::from(bounds.origin.y) * scale).round() as i32,
            )
            .into(),
            size: wry::dpi::PhysicalSize::new(
                (f64::from(bounds.size.width) * scale).round().max(0.) as u32,
                (f64::from(bounds.size.height) * scale).round().max(0.) as u32,
            )
            .into(),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = scale_factor;
        Rect {
            position: wry::dpi::LogicalPosition::new(
                f64::from(bounds.origin.x),
                f64::from(bounds.origin.y),
            )
            .into(),
            size: wry::dpi::LogicalSize::new(
                f64::from(bounds.size.width),
                f64::from(bounds.size.height),
            )
            .into(),
        }
    }
}

impl Focusable for NativeWebView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NativeWebView {
    fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let parent = cx.entity();
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(NativeWebViewElement {
                parent,
                webview: self.webview.clone(),
            })
    }
}

struct NativeWebViewElement {
    parent: Entity<NativeWebView>,
    webview: Rc<wry::WebView>,
}

impl Element for NativeWebViewElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            size: Size::full(),
            flex_shrink: 1.,
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let parent = self.parent.read(cx);
        if !parent.visible() {
            return None;
        }
        let clip = window.content_mask().bounds;
        if let Err(error) = parent.set_geometry(bounds, clip, window.scale_factor()) {
            if !parent.geometry_failed.replace(true) {
                log::warn!("WebView geometry update failed: {error:#}");
            }
            let _ = parent.set_native_visible(false);
            return None;
        }
        parent.geometry_failed.set(false);
        let visible = bounds.intersect(&clip);
        if visible.is_empty() {
            return None;
        }
        Some(window.insert_hitbox(visible, HitboxBehavior::BlockMouse))
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        _: &mut App,
    ) {
        let bounds = hitbox.as_ref().map_or(bounds, |h| h.bounds);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            let webview = self.webview.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, _, _, _| {
                if !bounds.contains(&event.position) {
                    let _ = webview.focus_parent();
                }
            });
        });
    }
}
