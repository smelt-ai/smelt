//! macOS 实现：WKWebView 使用独立的 AppKit child window。

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::os::raw::c_void;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, Ordering};

use objc::declare::ClassDecl;
use objc::runtime::{self, Class, Imp, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};

/// AppKit `NSRect` 的内存布局。只声明用得上的部分，不引第三方绑定。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct NsRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

use wry::dpi::{LogicalPosition, LogicalSize};
use wry::http::{Request, Response, header::CONTENT_SECURITY_POLICY, header::CONTENT_TYPE};
use wry::{Rect, WebViewBuilder};

use super::{INBOX, PanelMessage, PanelRect, PanelSource, PanelSpec};

/// 把 WKWebView 挂到窗口的 `contentView` 上，而不是 GPUI 的 Metal view 上。
///
/// 面板住在自己的 borderless 子窗口里，这个 handle 指向那个子窗口的
/// contentView，wry 把 WKWebView 建在里面。
///
/// 为什么非要独立窗口：GPUI 的 view 实现了 `performKeyEquivalent:`，而它把这
/// 个方法当成完整的键盘入口（`handle_key_event(.., true)`）。AppKit 对
/// `performKeyEquivalent:` 的派发是 NSWindow → contentView → **递归全部
/// subviews**，且早于 first responder 的 `keyDown:`。所以只要 WebView 和
/// GPUI 的 view 在同一个 NSWindow 里，无论挂成谁的子视图，GPUI 都会先收到
/// 每一个按键——同窗口内根本隔离不了。
///
/// 换成子窗口后，`performKeyEquivalent:` 只在 key window 内派发，两边的
/// input context 也各自独立，输入法归属由 AppKit 的 key window 机制裁决。
struct ContentViewHandle(std::ptr::NonNull<std::ffi::c_void>);

impl raw_window_handle::HasWindowHandle for ContentViewHandle {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        let handle = raw_window_handle::AppKitWindowHandle::new(self.0);
        // SAFETY: 指针来自当前窗口的 contentView，其生命周期长于面板。
        Ok(unsafe {
            raw_window_handle::WindowHandle::borrow_raw(raw_window_handle::RawWindowHandle::AppKit(
                handle,
            ))
        })
    }
}

/// 插件资源协议。页面的 origin 因此是 `smelt-plugin://<panel-id>`，
/// 不同插件天然跨域，`localStorage` 等也各自隔离。
const SCHEME: &str = "smelt-plugin";

/// 一个已挂载的面板：一个 borderless 子窗口 + 填满它的 WKWebView。
///
/// `gpui_view` 是父窗口里 GPUI 的 Metal view，收回键盘焦点时要用它。
struct MountedPanel {
    webview: wry::WebView,
    /// 加载外部网页的内容视图。懒创建——插件不用这个能力就没有第二个 WebView。
    ///
    /// 它和 chrome 视图同住一个子窗口：两个都是 WKWebView，键盘焦点由 AppKit
    /// 在窗口内正常裁决，不会再有 GPUI 那套 `performKeyEquivalent` 的串扰。
    content: Option<wry::WebView>,
    /// 承载 WebView 的子窗口（`NSWindow`）。
    panel_window: *mut Object,
    gpui_view: *mut Object,
}

thread_local! {
    static PANELS: RefCell<HashMap<String, MountedPanel>> = RefCell::new(HashMap::new());
    /// 内容视图的状态变化。wry 的回调在主线程触发，但那时 PANELS 可能正被借用，
    /// 所以先入队，由宿主每帧取走。
    static VIEW_EVENTS: Rc<RefCell<VecDeque<super::PanelViewEvent>>> =
        Rc::new(RefCell::new(VecDeque::new()));
}

fn to_rect(rect: PanelRect) -> Rect {
    Rect {
        position: LogicalPosition::new(f64::from(rect.x), f64::from(rect.y)).into(),
        size: LogicalSize::new(f64::from(rect.width), f64::from(rect.height)).into(),
    }
}

pub(super) fn set_bounds(id: &str, rect: PanelRect) -> Result<(), String> {
    PANELS.with(|panels| {
        let panels = panels.borrow();
        let Some(panel) = panels.get(id) else {
            return Ok(());
        };
        // 移动的是子窗口；WebView 只需要跟着填满它。
        unsafe {
            let frame = screen_frame(panel.gpui_view, rect);
            let _: () = msg_send![panel.panel_window, setFrame: frame display: objc::runtime::YES];
        }
        panel
            .webview
            .set_bounds(wry::Rect {
                position: LogicalPosition::new(0.0, 0.0).into(),
                size: LogicalSize::new(f64::from(rect.width), f64::from(rect.height)).into(),
            })
            .map_err(|error| format!("移动插件面板失败: {error}"))
    })
}

pub(super) fn create(
    window: &gpui::Window,
    spec: &PanelSpec,
    rect: PanelRect,
) -> Result<(), String> {
    let panel_id = spec.id.clone();
    let inbox = INBOX.with(Rc::clone);
    let ipc_panel_id = panel_id.clone();

    // 页面永远从自定义协议加载，`Html` 变体也一样：两条来源共用同一个
    // origin 和同一套 CSP，避免"内置页能跑、插件页不能跑"这类差异。
    let source = Arc::new(spec.source.clone());
    let protocol_panel_id = panel_id.clone();
    let allow_web_browse = spec.allow_web_browse;

    let gpui_view = native_view(window).unwrap_or(std::ptr::null_mut());
    if gpui_view.is_null() {
        return Err("GPUI 窗口没有可用的原生视图".into());
    }
    let panel_window = create_panel_window(gpui_view, rect)?;
    let panel_content: *mut Object = unsafe { msg_send![panel_window, contentView] };
    let Some(content) = std::ptr::NonNull::new(panel_content.cast::<std::ffi::c_void>()) else {
        destroy_panel_window(panel_window, gpui_view);
        return Err("面板子窗口没有 contentView".into());
    };
    let parent = ContentViewHandle(content);

    let webview = match plugin_webview_builder()
        .with_bounds(to_rect(rect))
        .with_devtools(spec.devtools)
        // 面板背景由页面自己画（用注入的 --smelt-* 变量），透明可以让
        // 加载首帧透出 GPUI 的骨架而不是一块白板。
        .with_transparent(true)
        .with_initialization_script(super::bootstrap_script(spec))
        .with_ipc_handler(move |request: Request<String>| {
            inbox.borrow_mut().push_back(PanelMessage {
                panel_id: ipc_panel_id.clone(),
                body: request.into_body(),
            });
        })
        .with_custom_protocol(SCHEME.to_string(), move |_id, request| {
            serve(&source, &protocol_panel_id, allow_web_browse, &request)
        })
        .with_url(format!("{SCHEME}://{panel_id}/index.html"))
        .build_as_child(&parent)
    {
        Ok(webview) => webview,
        Err(error) => {
            destroy_panel_window(panel_window, gpui_view);
            return Err(format!("创建插件面板失败: {error}"));
        }
    };

    // WebView 永远填满子窗口；面板的位置由子窗口自己的 frame 决定。
    let _ = webview.set_bounds(wry::Rect {
        position: LogicalPosition::new(0.0, 0.0).into(),
        size: LogicalSize::new(f64::from(rect.width), f64::from(rect.height)).into(),
    });
    let previous = PANELS.with(|panels| {
        panels.borrow_mut().insert(
            spec.id.clone(),
            MountedPanel {
                webview,
                content: None,
                panel_window,
                gpui_view,
            },
        )
    });
    if let Some(previous) = previous {
        close_mounted_panel(previous);
    }
    ensure_plugin_first_mouse();
    Ok(())
}

const PANEL_WINDOW_CLASS_NAME: &str = "SmeltPluginPanelWindow";

/// 面板子窗口的 ObjC 类。
///
/// 必须自己声明一个 `NSWindow` 子类：AppKit 里 borderless 窗口
/// （`styleMask = 0`）的 `canBecomeKeyWindow` 默认返回 `NO`，用原生
/// `NSWindow` 建出来的面板永远拿不到键盘焦点——表现就是面板里什么都点不动。
///
/// `canBecomeMainWindow` 保持 `NO`：main window 让给父窗口，这样面板拿到键盘
/// 时，主窗口的标题栏不会变成失活的灰色。
///
/// 第一次点击走 `acceptsFirstMouse:`，不覆盖 `sendEvent:`。覆盖 `sendEvent:`
/// 会把 AppKit 事件循环暴露给 objc 0.2 的消息发送（错误 selector 即主线程 abort）。
fn panel_window_class() -> Option<&'static Class> {
    static REGISTERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

    extern "C" fn can_become_key(_: &Object, _: Sel) -> objc::runtime::BOOL {
        objc::runtime::YES
    }
    extern "C" fn can_become_main(_: &Object, _: Sel) -> objc::runtime::BOOL {
        objc::runtime::NO
    }

    REGISTERED.get_or_init(|| {
        let Some(mut decl) = ClassDecl::new(PANEL_WINDOW_CLASS_NAME, class!(NSWindow)) else {
            // 已经注册过（例如同进程内重复初始化），直接用现成的。
            return Class::get(PANEL_WINDOW_CLASS_NAME).is_some();
        };
        unsafe {
            decl.add_method(
                sel!(canBecomeKeyWindow),
                can_become_key as extern "C" fn(&Object, Sel) -> objc::runtime::BOOL,
            );
            decl.add_method(
                sel!(canBecomeMainWindow),
                can_become_main as extern "C" fn(&Object, Sel) -> objc::runtime::BOOL,
            );
        }
        decl.register();
        true
    });
    let class = Class::get(PANEL_WINDOW_CLASS_NAME);
    if class.is_some() {
        ensure_plugin_first_mouse();
    }
    class
}

/// AppKit 问的是 **命中视图** 的 `acceptsFirstMouse:`，不是 WKWebView。
/// wry 把该方法加在 `WryWebView` 上，真正吃到点击的是内部 `WKContentView`，
/// 所以第一次点击仍会被当成激活窗口。这里只改插件子窗口里的命中行为。
static NSVIEW_ACCEPTS_FIRST_MOUSE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static WKCONTENT_ACCEPTS_FIRST_MOUSE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

fn ensure_plugin_first_mouse() {
    swizzle_accepts_first_mouse(class!(NSView), &NSVIEW_ACCEPTS_FIRST_MOUSE);
    if let Some(wk) = Class::get("WKContentView") {
        swizzle_accepts_first_mouse(wk, &WKCONTENT_ACCEPTS_FIRST_MOUSE);
    }
}

fn swizzle_accepts_first_mouse(cls: &Class, slot: &AtomicPtr<c_void>) {
    if !slot.load(Ordering::Acquire).is_null() {
        return;
    }
    unsafe {
        if !class_directly_implements(cls, sel!(acceptsFirstMouse:)) {
            return;
        }
        let method = runtime::class_getInstanceMethod(cls, sel!(acceptsFirstMouse:));
        if method.is_null() {
            return;
        }
        let imp: Imp = std::mem::transmute(
            plugin_accepts_first_mouse
                as extern "C" fn(&Object, Sel, *mut Object) -> objc::runtime::BOOL,
        );
        let original = runtime::method_setImplementation(method.cast_mut(), imp);
        slot.store(original as *mut c_void, Ordering::Release);
    }
}

extern "C" fn plugin_accepts_first_mouse(
    this: &Object,
    sel: Sel,
    event: *mut Object,
) -> objc::runtime::BOOL {
    unsafe {
        if view_is_in_plugin_panel(this) {
            return objc::runtime::YES;
        }
        let Some(original) = original_accepts_first_mouse(this) else {
            return objc::runtime::NO;
        };
        let original: extern "C" fn(&Object, Sel, *mut Object) -> objc::runtime::BOOL =
            std::mem::transmute(original);
        original(this, sel, event)
    }
}

unsafe fn view_is_in_plugin_panel(view: &Object) -> bool {
    unsafe {
        let window: *mut Object = msg_send![view, window];
        if window.is_null() {
            return false;
        }
        let Some(panel_class) = Class::get(PANEL_WINDOW_CLASS_NAME) else {
            return false;
        };
        let is_panel: objc::runtime::BOOL = msg_send![window, isKindOfClass: panel_class];
        is_panel == objc::runtime::YES
    }
}

fn original_accepts_first_mouse(this: &Object) -> Option<Imp> {
    unsafe {
        if let Some(wk) = Class::get("WKContentView") {
            let is_wk: objc::runtime::BOOL = msg_send![this, isKindOfClass: wk];
            if is_wk == objc::runtime::YES {
                let ptr = WKCONTENT_ACCEPTS_FIRST_MOUSE.load(Ordering::Acquire);
                if !ptr.is_null() {
                    return Some(std::mem::transmute(ptr));
                }
            }
        }
        let ptr = NSVIEW_ACCEPTS_FIRST_MOUSE.load(Ordering::Acquire);
        if ptr.is_null() {
            return None;
        }
        Some(std::mem::transmute(ptr))
    }
}

unsafe fn class_directly_implements(cls: &Class, sel: Sel) -> bool {
    unsafe {
        let method = runtime::class_getInstanceMethod(cls, sel);
        if method.is_null() {
            return false;
        }
        let superclass = runtime::class_getSuperclass(cls);
        if superclass.is_null() {
            return true;
        }
        runtime::class_getInstanceMethod(&*superclass, sel) != method
    }
}

/// 面板 contentView：空白处同样接受第一次点击，避免点到 WebView 周围的垫层
/// 时又把事件消耗成窗口激活。
fn panel_content_view_class() -> Option<&'static Class> {
    const NAME: &str = "SmeltPluginPanelContentView";
    static REGISTERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

    extern "C" fn accepts_first_mouse(_: &Object, _: Sel, _: *mut Object) -> objc::runtime::BOOL {
        objc::runtime::YES
    }

    REGISTERED.get_or_init(|| {
        let Some(mut decl) = ClassDecl::new(NAME, class!(NSView)) else {
            return Class::get(NAME).is_some();
        };
        unsafe {
            decl.add_method(
                sel!(acceptsFirstMouse:),
                accepts_first_mouse
                    as extern "C" fn(&Object, Sel, *mut Object) -> objc::runtime::BOOL,
            );
        }
        decl.register();
        true
    });
    Class::get(NAME)
}

fn plugin_webview_builder() -> WebViewBuilder<'static> {
    // wry 默认 false：非 key 窗口上的第一次点击只激活窗口，不落到页面。
    WebViewBuilder::new().with_accept_first_mouse(true)
}

/// 建一个 borderless、透明、无阴影的子窗口来装这个面板。
///
/// `addChildWindow:ordered:` 让它自动跟随父窗口移动、并始终排在父窗口之上；
/// 父窗口因此保持 main window 外观，标题栏不会因为面板拿到键盘焦点而变灰。
fn create_panel_window(gpui_view: *mut Object, rect: PanelRect) -> Result<*mut Object, String> {
    unsafe {
        let parent: *mut Object = msg_send![gpui_view, window];
        if parent.is_null() {
            return Err("GPUI 视图还没有附着到窗口".into());
        }
        let frame = screen_frame(gpui_view, rect);
        let class = panel_window_class().ok_or("注册面板窗口类失败")?;
        let window: *mut Object = msg_send![class, alloc];
        // styleMask=Borderless(0)，backing=Buffered(2)，defer=NO
        let window: *mut Object = msg_send![
            window,
            initWithContentRect: frame
            styleMask: 0u64
            backing: 2u64
            defer: objc::runtime::NO
        ];
        if window.is_null() {
            return Err("创建面板子窗口失败".into());
        }
        let clear: *mut Object = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![window, setBackgroundColor: clear];
        let _: () = msg_send![window, setOpaque: objc::runtime::NO];
        let _: () = msg_send![window, setHasShadow: objc::runtime::NO];
        // 面板不进 Cmd+` 的窗口轮转，也不该出现在 Exposé 里。
        let _: () = msg_send![window, setExcludedFromWindowsMenu: objc::runtime::YES];
        if let Some(content_class) = panel_content_view_class() {
            let content: *mut Object = msg_send![content_class, new];
            if !content.is_null() {
                let _: () = msg_send![window, setContentView: content];
            }
        }
        // NSWindowAbove = 1
        let _: () = msg_send![parent, addChildWindow: window ordered: 1i64];
        Ok(window)
    }
}

fn destroy_panel_window(panel_window: *mut Object, gpui_view: *mut Object) {
    if panel_window.is_null() {
        return;
    }
    unsafe {
        let parent: *mut Object = msg_send![gpui_view, window];
        if !parent.is_null() {
            let _: () = msg_send![parent, removeChildWindow: panel_window];
        }
        let _: () = msg_send![panel_window, close];
    }
}

fn close_mounted_panel(panel: MountedPanel) {
    let was_focused = unsafe {
        let is_key: objc::runtime::BOOL = msg_send![panel.panel_window, isKeyWindow];
        is_key == objc::runtime::YES
    };
    release_panel_focus(panel.panel_window, panel.gpui_view, was_focused);
    destroy_panel_window(panel.panel_window, panel.gpui_view);
    drop(panel);
}

/// 把 GPUI 元素的 bounds 换算成屏幕坐标，供子窗口的 frame 使用。
///
/// 三步：GPUI 的左上原点 → GPUI view 的 AppKit 坐标 → 窗口坐标 → 屏幕坐标。
/// 后两步交给 AppKit 自己转，比手算可靠。
fn appkit_local_frame(parent_bounds: NsRect, rect: PanelRect, parent_is_flipped: bool) -> NsRect {
    NsRect {
        x: f64::from(rect.x),
        y: if parent_is_flipped {
            f64::from(rect.y)
        } else {
            parent_bounds.height - f64::from(rect.y) - f64::from(rect.height)
        },
        width: f64::from(rect.width),
        height: f64::from(rect.height),
    }
}

fn screen_frame(gpui_view: *mut Object, rect: PanelRect) -> NsRect {
    unsafe {
        let bounds: NsRect = msg_send![gpui_view, bounds];
        let is_flipped: objc::runtime::BOOL = msg_send![gpui_view, isFlipped];
        let local = appkit_local_frame(bounds, rect, is_flipped == objc::runtime::YES);
        let in_window: NsRect =
            msg_send![gpui_view, convertRect: local toView: std::ptr::null_mut::<Object>()];
        let window: *mut Object = msg_send![gpui_view, window];
        if window.is_null() {
            return in_window;
        }
        msg_send![window, convertRectToScreen: in_window]
    }
}

/// 把键盘焦点交还给 GPUI。
///
/// WKWebView 一旦成为 first responder，窗口的输入法上下文就归它了。只调
/// `setHidden:` 不交还焦点，输入法会继续认着这个看不见的 WebView——smelt
/// 自己的对话和终端于是收得到英文按键，却收不到中文上屏。
/// 把焦点交还给这个窗口的 GPUI view。供宿主在"点击落到面板之外"时调用。
pub(super) fn release_focus_in(window: &gpui::Window) {
    let Some(view) = native_view(window) else {
        return;
    };
    release_focus(view);
}

/// Resign every mounted WebView child before restoring the parent GPUI view.
///
/// The WebView is deliberately not a subview of the GPUI window. Looking only at
/// `gpui_view.window.firstResponder` therefore misses the case where the panel
/// child window is key; that case is exactly what otherwise leaves the IME attached
/// to a hidden plugin after a modal opens.
fn release_focus(gpui_view: *mut Object) {
    if gpui_view.is_null() {
        return;
    }
    unsafe {
        let parent_window: *mut Object = msg_send![gpui_view, window];
        if parent_window.is_null() {
            return;
        }

        let panel_needs_release = PANELS.with(|panels| {
            let panels = panels.borrow();
            let mut needs_release = false;
            for panel in panels.values() {
                if panel.panel_window.is_null() || panel.gpui_view.is_null() {
                    continue;
                }
                let panel_parent: *mut Object = msg_send![panel.gpui_view, window];
                if panel_parent != parent_window {
                    continue;
                }
                let is_key: objc::runtime::BOOL = msg_send![panel.panel_window, isKeyWindow];
                if is_key == objc::runtime::YES {
                    needs_release = true;
                    let _: objc::runtime::BOOL = msg_send![
                        panel.panel_window,
                        makeFirstResponder: std::ptr::null_mut::<Object>()
                    ];
                }
            }
            needs_release
        });

        // If no WebView owned focus, the caller has nothing to release.
        if !panel_needs_release {
            return;
        }

        restore_gpui_focus(parent_window, gpui_view);
    }
}

/// Release one specific panel while leaving a different visible plugin panel alone.
fn release_panel_focus(panel_window: *mut Object, gpui_view: *mut Object, focus_hint: bool) {
    if panel_window.is_null() || gpui_view.is_null() || !focus_hint {
        return;
    }
    unsafe {
        let _: objc::runtime::BOOL =
            msg_send![panel_window, makeFirstResponder: std::ptr::null_mut::<Object>()];
        let parent_window: *mut Object = msg_send![gpui_view, window];
        if parent_window.is_null() {
            return;
        }
        let another_panel_is_active = PANELS.with(|panels| {
            panels.borrow().values().any(|panel| {
                if panel.panel_window.is_null()
                    || panel.gpui_view.is_null()
                    || panel.panel_window == panel_window
                {
                    return false;
                }
                let panel_parent: *mut Object = msg_send![panel.gpui_view, window];
                if panel_parent != parent_window {
                    return false;
                }
                let is_key: objc::runtime::BOOL = msg_send![panel.panel_window, isKeyWindow];
                is_key == objc::runtime::YES
            })
        });
        if !another_panel_is_active {
            restore_gpui_focus(parent_window, gpui_view);
        }
    }
}

unsafe fn restore_gpui_focus(parent_window: *mut Object, gpui_view: *mut Object) {
    let _: () = msg_send![parent_window, makeKeyWindow];
    let _: objc::runtime::BOOL = msg_send![parent_window, makeFirstResponder: gpui_view];
    let context: *mut Object = msg_send![gpui_view, inputContext];
    if !context.is_null() {
        let _: () = msg_send![context, activate];
    }
}

/// 刚显示的插件面板立刻成为 key，并把 first responder 交给 WKWebView。
///
/// 侧栏 / tab 上的点击发生在 GPUI，父窗口仍是 key。这里不抢的话，用户点进
/// 页面的第一次点击会被 AppKit 当成激活子窗口，而不是命中按钮或输入框。
pub(super) fn activate_panel(id: &str) {
    PANELS.with(|panels| {
        let panels = panels.borrow();
        let Some(panel) = panels.get(id) else {
            return;
        };
        make_panel_key(panel.panel_window);
    });
}

fn make_panel_key(panel_window: *mut Object) {
    if panel_window.is_null() {
        return;
    }
    unsafe {
        let _: () = msg_send![panel_window, makeKeyWindow];
        let content: *mut Object = msg_send![panel_window, contentView];
        if content.is_null() {
            return;
        }
        let responder = preferred_first_responder(content);
        let _: objc::runtime::BOOL = msg_send![panel_window, makeFirstResponder: responder];
    }
}

fn preferred_first_responder(content: *mut Object) -> *mut Object {
    unsafe {
        let subviews: *mut Object = msg_send![content, subviews];
        if subviews.is_null() {
            return content;
        }
        let count: usize = msg_send![subviews, count];
        if count == 0 {
            return content;
        }
        // 内容视图叠在 chrome 之上，焦点给最上面的 WKWebView。
        msg_send![subviews, objectAtIndex: count.saturating_sub(1)]
    }
}

pub(super) fn navigate_view(id: &str, url: &str, rect: PanelRect) -> Result<(), String> {
    // 只放行 http(s)。这个 WebView 不受面板 CSP 约束，是真正的浏览器，
    // 所以协议白名单必须在这一层守住。
    let lowered = url.to_ascii_lowercase();
    if !lowered.starts_with("http://") && !lowered.starts_with("https://") {
        return Err("只支持 http 与 https".into());
    }
    ensure_content_view(id, rect)?;
    PANELS.with(|panels| {
        let panels = panels.borrow();
        let Some(content) = panels.get(id).and_then(|panel| panel.content.as_ref()) else {
            return Err(format!("面板 {id} 没有内容视图"));
        };
        let _ = content.set_bounds(to_rect(rect));
        let _ = content.set_visible(true);
        content
            .load_url(url)
            .map_err(|error| format!("导航失败: {error}"))
    })
}

pub(super) fn set_view_bounds(id: &str, rect: PanelRect) -> Result<(), String> {
    PANELS.with(|panels| {
        let panels = panels.borrow();
        let Some(content) = panels.get(id).and_then(|panel| panel.content.as_ref()) else {
            return Ok(());
        };
        content
            .set_bounds(to_rect(rect))
            .map_err(|error| format!("移动内容视图失败: {error}"))
    })
}

pub(super) fn view_command(id: &str, command: super::PanelViewCommand) -> Result<(), String> {
    use super::PanelViewCommand as Command;
    PANELS.with(|panels| {
        let panels = panels.borrow();
        let Some(content) = panels.get(id).and_then(|panel| panel.content.as_ref()) else {
            return Ok(());
        };
        let result = match command {
            Command::Back => content.go_back(),
            Command::Forward => content.go_forward(),
            Command::Reload => content.reload(),
            Command::Hide => content.set_visible(false),
        };
        result.map_err(|error| format!("内容视图操作失败: {error}"))
    })
}

pub(super) fn drain_view_events() -> Vec<super::PanelViewEvent> {
    VIEW_EVENTS.with(|events| events.borrow_mut().drain(..).collect())
}

/// 首次导航时才建内容视图，建在面板子窗口里、chrome 视图之上。
fn ensure_content_view(id: &str, rect: PanelRect) -> Result<(), String> {
    let existing =
        PANELS.with(|panels| panels.borrow().get(id).map(|panel| panel.content.is_some()));
    match existing {
        None => return Err(format!("面板 {id} 未挂载")),
        Some(true) => return Ok(()),
        Some(false) => {}
    }

    let parent_content: *mut Object = PANELS.with(|panels| {
        panels
            .borrow()
            .get(id)
            .map(|panel| unsafe { msg_send![panel.panel_window, contentView] })
            .unwrap_or(std::ptr::null_mut())
    });
    let Some(parent) = std::ptr::NonNull::new(parent_content.cast::<std::ffi::c_void>()) else {
        return Err("面板子窗口没有 contentView".into());
    };

    let events = VIEW_EVENTS.with(Rc::clone);
    let panel_id = id.to_string();
    let title_panel_id = panel_id.clone();
    let title_events = Rc::clone(&events);
    let load_panel_id = panel_id;

    let content = plugin_webview_builder()
        .with_bounds(to_rect(rect))
        .with_devtools(cfg!(debug_assertions))
        .with_document_title_changed_handler(move |title| {
            title_events.borrow_mut().push_back(super::PanelViewEvent {
                panel_id: title_panel_id.clone(),
                state: super::PanelViewState {
                    title,
                    ..Default::default()
                },
            });
        })
        .with_on_page_load_handler(move |event, url| {
            events.borrow_mut().push_back(super::PanelViewEvent {
                panel_id: load_panel_id.clone(),
                state: super::PanelViewState {
                    url,
                    loading: matches!(event, wry::PageLoadEvent::Started),
                    ..Default::default()
                },
            });
        })
        .build_as_child(&ContentViewHandle(parent))
        .map_err(|error| format!("创建内容视图失败: {error}"))?;

    PANELS.with(|panels| {
        if let Some(panel) = panels.borrow_mut().get_mut(id) {
            panel.content = Some(content);
        }
    });
    ensure_plugin_first_mouse();
    Ok(())
}

pub(super) fn set_visible(id: &str, visible: bool) {
    let focus_hint = PANELS.with(|panels| {
        let panels = panels.borrow();
        let panel = panels.get(id)?;
        let was_focused = if visible {
            false
        } else {
            unsafe {
                let is_key: objc::runtime::BOOL = msg_send![panel.panel_window, isKeyWindow];
                is_key == objc::runtime::YES
            }
        };
        let _ = panel.webview.set_visible(visible);
        if let Some(content) = panel.content.as_ref() {
            let _ = content.set_visible(visible);
        }
        unsafe {
            if visible {
                let parent: *mut Object = msg_send![panel.gpui_view, window];
                // 重新挂回父窗口：orderOut 会解除子窗口关系。
                if !parent.is_null() {
                    let _: () = msg_send![parent, addChildWindow: panel.panel_window ordered: 1i64];
                }
            } else {
                let _: () = msg_send![panel.panel_window, orderOut: std::ptr::null_mut::<Object>()];
            }
        }
        (!visible).then_some((panel.panel_window, panel.gpui_view, was_focused))
    });
    // Do this after releasing the PANELS borrow: release_panel_focus may inspect
    // the registry to ensure a different visible panel keeps ownership of focus.
    if let Some((panel_window, gpui_view, was_focused)) = focus_hint {
        release_panel_focus(panel_window, gpui_view, was_focused);
    }
}

pub(super) fn close(id: &str) {
    let panel = PANELS.with(|panels| panels.borrow_mut().remove(id));
    if let Some(panel) = panel {
        close_mounted_panel(panel);
    }
}

/// 取出这个 GPUI 窗口的原生视图（`GPUIView`）。面板子窗口挂在它的窗口上，
/// 收回键盘焦点时也要用它。
fn native_view(window: &gpui::Window) -> Option<*mut Object> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    Some(handle.ns_view.as_ptr().cast::<Object>())
}

pub(super) fn drain_messages() -> Vec<PanelMessage> {
    INBOX.with(|inbox| inbox.borrow_mut().drain(..).collect())
}

pub(super) fn post(id: &str, payload: &str) -> Result<(), String> {
    PANELS.with(|panels| {
        let panels = panels.borrow();
        let Some(panel) = panels.get(id) else {
            return Err(format!("面板 {id} 未挂载"));
        };
        let webview = &panel.webview;
        // payload 是 JSON 文本，用 JSON.parse 而不是字面量拼接，
        // 避免页面侧被注入。
        let script = format!(
            "window.smelt&&window.smelt._emit(JSON.parse({}));",
            super::json_string(payload)
        );
        webview
            .evaluate_script(&script)
            .map_err(|error| format!("向面板 {id} 投递消息失败: {error}"))
    })
}

/// 自定义协议处理：只从插件包目录内取文件，任何越界一律 404 而不是穿越。
fn serve(
    source: &PanelSource,
    panel_id: &str,
    allow_web_browse: bool,
    request: &Request<Vec<u8>>,
) -> Response<Cow<'static, [u8]>> {
    let uri = request.uri();
    // host 段必须是这个面板自己：`smelt-plugin://other-plugin/...` 不给读。
    if uri.host().is_some_and(|host| host != panel_id) {
        return not_found();
    }
    let path = uri.path();

    match source {
        PanelSource::Html(html) => {
            if path == "/index.html" || path == "/" {
                ok("text/html", html.clone().into_bytes(), allow_web_browse)
            } else {
                not_found()
            }
        }
        PanelSource::PackageRoot(root) => match resolve(root, path) {
            Some(file) => match std::fs::read(&file) {
                Ok(bytes) => ok(mime_for(&file), bytes, allow_web_browse),
                Err(_) => not_found(),
            },
            None => not_found(),
        },
    }
}

/// 把 URL 路径解析成包内真实文件。拒绝 `..`、绝对路径和符号链接逃逸。
fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let relative = path.trim_start_matches('/');
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };
    // 先按段拒绝，再用 canonicalize 兜住符号链接——只做后者会漏掉
    // 根目录本身不存在的情况，只做前者挡不住包内指向外部的链接。
    if Path::new(relative)
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let candidate = root.join(relative).canonicalize().ok()?;
    let root = root.canonicalize().ok()?;
    candidate.starts_with(&root).then_some(candidate)
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
    {
        "html" => "text/html",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "woff2" => "font/woff2",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn ok(mime: &str, body: Vec<u8>, allow_web_browse: bool) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(200)
        .header(CONTENT_TYPE, mime)
        .header(CONTENT_SECURITY_POLICY, csp(allow_web_browse))
        .body(Cow::Owned(body))
        .expect("static response is well-formed")
}

/// 面板页面的 CSP。默认锁死在包内：脚本、样式、图片、字体都必须来自插件包，
/// 网络请求一律经宿主的 IPC 转发，这样 capability 才管得住。
///
/// `web.browse` 只放开 `frame-src`——插件页面自己仍然不能外联（`connect-src`
/// 保持 `none`）。要说清楚的是：这只控制"能不能嵌"，嵌进来的站点是独立的
/// browsing context，有它自己的 CSP，父页面管不到它做什么。
pub(super) fn csp(allow_web_browse: bool) -> &'static str {
    if allow_web_browse {
        "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
connect-src 'none'; frame-src https: http:"
    } else {
        "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
connect-src 'none'; frame-src 'none'"
    }
}

fn not_found() -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(404)
        .body(Cow::Borrowed(&b""[..]))
        .expect("static response is well-formed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_is_refused() {
        let root = std::env::temp_dir();
        assert!(resolve(&root, "/../etc/passwd").is_none());
        assert!(resolve(&root, "/a/../../b").is_none());
        assert!(resolve(&root, "//etc/passwd").is_none());
    }

    #[test]
    fn files_inside_the_package_resolve() {
        let root = std::env::temp_dir().join("smelt-webview-test-root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index.html"), b"<!doctype html>").unwrap();
        assert!(resolve(&root, "/index.html").is_some());
        // 空路径落到入口文件。
        assert!(resolve(&root, "/").is_some());
        assert!(resolve(&root, "/missing.html").is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn framing_external_sites_requires_the_browse_capability() {
        assert!(csp(false).contains("frame-src 'none'"));
        assert!(csp(true).contains("frame-src https: http:"));
        // 页面自己永远不能外联，有没有 web.browse 都一样。
        assert!(csp(true).contains("connect-src 'none'"));
        assert!(csp(false).contains("connect-src 'none'"));
    }

    #[test]
    fn panel_rect_uses_parent_content_coordinates() {
        let parent_bounds = NsRect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 800.0,
        };
        assert_eq!(
            appkit_local_frame(
                parent_bounds,
                PanelRect {
                    x: 120.0,
                    y: 50.0,
                    width: 300.0,
                    height: 240.0,
                },
                false,
            ),
            NsRect {
                x: 120.0,
                y: 510.0,
                width: 300.0,
                height: 240.0,
            }
        );
        assert_eq!(
            appkit_local_frame(
                parent_bounds,
                PanelRect {
                    x: 120.0,
                    y: 50.0,
                    width: 300.0,
                    height: 240.0,
                },
                true,
            ),
            NsRect {
                x: 120.0,
                y: 50.0,
                width: 300.0,
                height: 240.0,
            }
        );
    }

    #[test]
    fn mime_covers_the_web_asset_kinds() {
        assert_eq!(mime_for(Path::new("a/b.js")), "text/javascript");
        assert_eq!(mime_for(Path::new("a/b.woff2")), "font/woff2");
        assert_eq!(
            mime_for(Path::new("a/b.unknown")),
            "application/octet-stream"
        );
    }

    #[test]
    fn plugin_panel_window_does_not_override_send_event() {
        let class = panel_window_class().expect("注册面板窗口类");
        unsafe {
            assert!(
                !class_directly_implements(class, sel!(sendEvent:)),
                "覆盖 sendEvent: 会把 AppKit 事件循环暴露给 objc 消息错误"
            );
            assert!(class_directly_implements(class, sel!(canBecomeKeyWindow)));
            assert!(class_directly_implements(class, sel!(canBecomeMainWindow)));
        }
    }

    #[test]
    fn plugin_panel_content_view_accepts_first_mouse() {
        let class = panel_content_view_class().expect("注册面板 contentView 类");
        unsafe {
            assert!(class_directly_implements(class, sel!(acceptsFirstMouse:)));
        }
    }
}
