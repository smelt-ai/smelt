//! macOS 26 顶部导航的系统玻璃桥。
//!
//! 这是窗口铬，不是内容区视觉语言（内容区跟 Grok Bot 实色 sand）。
//! GPUI 目前只公开传统 `WindowBackgroundAppearance::Blurred`，其 macOS 实现是
//! `NSVisualEffectView`。这里在 GPUI 的 Metal view 下插入原生
//! `NSGlassEffectView`，只给稳定的顶部导航区域使用系统玻璃。
//! 故意不铺满工作区：终端和 ACP 会持续更新，整窗玻璃会让 AppKit 每帧重采样整块
//! 背景，导致整个应用掉帧。
//!
//! 注意：macOS 26 的 AppKit 把 `NSView` 的 `setTag:` 移除了（`tag` 变成只读），
//! 向 `NSGlassEffectView` 发送 `setTag:` 会抛 unrecognized selector 异常并导致
//! 进程 abort。因此这里不再用 tag 标记玻璃视图，改从 subviews 按类识别。

use gpui::Window;

/// Liquid Glass 材质档位（对应 AppKit 的 `NSGlassEffectViewStyle`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum GlassStyle {
    /// 标准玻璃材质（`NSGlassEffectViewStyleRegular`）。
    #[default]
    Regular,
    /// 清透玻璃材质（`NSGlassEffectViewStyleClear`），折射质感更明显。
    Clear,
}

impl GlassStyle {
    /// 对应 AppKit 枚举的原始值（Regular=0，Clear=1）。
    pub fn ns_value(self) -> i64 {
        match self {
            GlassStyle::Regular => 0,
            GlassStyle::Clear => 1,
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use gpui::Window;
    use objc::runtime::{Class, Object};
    use objc::{class, msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    // `NSViewWidthSizable | NSViewMinYMargin`：固定 56pt 高、贴住窗口顶部，
    // 宽度随窗口变化。全窗口玻璃会和流式终端/ACP 内容形成昂贵的重采样环路。
    const TOP_BAR_RESIZE_WITH_PARENT: u64 = (1 << 1) | (1 << 3);
    // Glass content 自身则需要跟其父 glass view 一起改变宽高。
    const CONTENT_RESIZE_WITH_PARENT: u64 = (1 << 1) | (1 << 4);
    const TOP_BAR_HEIGHT: f64 = 56.0;
    const WINDOW_BELOW: i64 = -1;
    const GLASS_CLASS_NAME: &str = "NSGlassEffectView";
    const AVAILABILITY_CACHE_TTL: Duration = Duration::from_secs(3);

    struct AvailabilityCache {
        checked_at: Instant,
        available: bool,
    }

    static LIQUID_GLASS_AVAILABLE: OnceLock<Mutex<Option<AvailabilityCache>>> = OnceLock::new();

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSSize {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSRect {
        origin: NSPoint,
        size: NSSize,
    }

    fn top_bar_frame(bounds: NSRect) -> NSRect {
        let height = bounds.size.height.min(TOP_BAR_HEIGHT);
        NSRect {
            origin: NSPoint {
                x: bounds.origin.x,
                y: bounds.origin.y + bounds.size.height - height,
            },
            size: NSSize {
                width: bounds.size.width,
                height,
            },
        }
    }

    fn local_frame(bounds: NSRect) -> NSRect {
        NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: bounds.size,
        }
    }

    fn native_views(window: &Window) -> (*mut Object, *mut Object) {
        let handle = HasWindowHandle::window_handle(window)
            .expect("GPUI window must expose a native window handle");
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            unreachable!("macOS GPUI windows must use AppKit window handles");
        };
        let native_view = handle.ns_view.as_ptr().cast::<Object>();
        let container: *mut Object = unsafe { msg_send![native_view, superview] };
        assert!(
            !container.is_null(),
            "GPUI native view must be attached to an AppKit content view"
        );
        (native_view, container)
    }

    /// `view` 的 ObjC 类名是否等于 `name`（拿不到 `Class` 指针时兜底用）。
    unsafe fn class_name_is(view: *mut Object, name: &str) -> bool {
        let class_name: *mut Object = msg_send![view, className];
        if class_name.is_null() {
            return false;
        }
        let utf8: *const std::os::raw::c_char = msg_send![class_name, UTF8String];
        if utf8.is_null() {
            return false;
        }
        unsafe { std::ffi::CStr::from_ptr(utf8) }.to_bytes() == name.as_bytes()
    }

    /// 在 `container` 的 subviews 里找已安装的 glass 视图。`glass_class` 传
    /// `None` 时退化为按类名字符串匹配（例如运行在无 Liquid Glass 的系统上）。
    fn find_glass_view(
        container: *mut Object,
        glass_class: Option<&'static Class>,
    ) -> Option<*mut Object> {
        unsafe {
            let subviews: *mut Object = msg_send![container, subviews];
            if subviews.is_null() {
                return None;
            }
            let count: usize = msg_send![subviews, count];
            for i in 0..count {
                let view: *mut Object = msg_send![subviews, objectAtIndex: i];
                if view.is_null() {
                    continue;
                }
                let is_glass = match glass_class {
                    Some(cls) => {
                        let b: objc::runtime::BOOL = msg_send![view, isKindOfClass: cls];
                        b != objc::runtime::NO
                    }
                    None => class_name_is(view, GLASS_CLASS_NAME),
                };
                if is_glass {
                    return Some(view);
                }
            }
            None
        }
    }

    fn remove_existing(container: *mut Object) {
        unsafe {
            if let Some(glass) = find_glass_view(container, Class::get(GLASS_CLASS_NAME)) {
                let _: () = msg_send![glass, removeFromSuperview];
            }
        }
    }

    pub(super) fn is_available() -> bool {
        // Runtime lookup is deliberate: builds made with the macOS 26 SDK must still run
        // unchanged on macOS versions that do not provide the Liquid Glass class.
        // AppKit 的辅助功能查询跨越系统对象边界，不能在 Workspace 每帧重复执行；
        // 短 TTL 保留了用户切换设置后无需重启即可生效的行为。
        let cache = LIQUID_GLASS_AVAILABLE.get_or_init(|| Mutex::new(None));
        let now = Instant::now();
        if let Ok(guard) = cache.lock()
            && let Some(entry) = guard.as_ref()
            && now.duration_since(entry.checked_at) < AVAILABILITY_CACHE_TTL
        {
            return entry.available;
        }
        let available = !reduce_transparency() && Class::get(GLASS_CLASS_NAME).is_some();
        if let Ok(mut guard) = cache.lock() {
            *guard = Some(AvailabilityCache {
                checked_at: now,
                available,
            });
        }
        available
    }

    /// `style` 是后续 SDK 才加上的；早期 `NSGlassEffectView` 头文件里没有。
    /// 和 `setTag:` 一样，缺方法就是 unrecognized selector → 主线程 abort。
    unsafe fn set_style_if_available(glass: *mut Object, style: i64) {
        unsafe {
            if glass.is_null() {
                return;
            }
            let sel = sel!(setStyle:);
            let responds: objc::runtime::BOOL = msg_send![glass, respondsToSelector: sel];
            if responds != objc::runtime::NO {
                let _: () = msg_send![glass, setStyle: style];
            }
        }
    }

    /// 系统「减少透明度」辅助功能（系统设置 → 辅助功能 → 显示）开启时返回 true。
    /// 此时跳过玻璃安装，退回 GPUI 的 `Blurred` 毛玻璃，避免违背用户的辅助功能偏好。
    fn reduce_transparency() -> bool {
        unsafe {
            let shared: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
            if shared.is_null() {
                return false;
            }
            let reduced: objc::runtime::BOOL =
                msg_send![shared, accessibilityDisplayShouldReduceTransparency];
            reduced != objc::runtime::NO
        }
    }

    pub(super) fn sync(window: &Window, style: i64) {
        let (native_view, container) = native_views(window);
        if !is_available() {
            remove_existing(container);
            return;
        }
        let Some(glass_class) = Class::get(GLASS_CLASS_NAME) else {
            remove_existing(container);
            return;
        };

        unsafe {
            if let Some(existing) = find_glass_view(container, Some(glass_class)) {
                let frame = top_bar_frame(msg_send![container, bounds]);
                let _: () = msg_send![existing, setFrame: frame];
                let _: () = msg_send![existing, setAutoresizingMask: TOP_BAR_RESIZE_WITH_PARENT];
                set_style_if_available(existing, style);
                let content: *mut Object = msg_send![existing, contentView];
                if !content.is_null() {
                    let _: () = msg_send![content, setFrame: local_frame(frame)];
                    let _: () = msg_send![content, setAutoresizingMask: CONTENT_RESIZE_WITH_PARENT];
                }
                return;
            }

            let bounds: NSRect = msg_send![container, bounds];
            let frame = top_bar_frame(bounds);
            let glass: *mut Object = msg_send![glass_class, alloc];
            let glass: *mut Object = msg_send![glass, initWithFrame: frame];
            assert!(
                !glass.is_null(),
                "NSGlassEffectView initialization unexpectedly returned nil"
            );

            set_style_if_available(glass, style);
            let _: () = msg_send![glass, setAutoresizingMask: TOP_BAR_RESIZE_WITH_PARENT];
            // AppKit guarantees the glass compositing contract for `contentView`, not arbitrary
            // subviews. A transparent NSView makes this narrow toolbar material a valid glass
            // surface while GPUI's Metal view remains its sibling above it.
            let content: *mut Object = msg_send![class!(NSView), alloc];
            let content: *mut Object = msg_send![content, initWithFrame: local_frame(frame)];
            assert!(
                !content.is_null(),
                "Liquid Glass content view initialization unexpectedly returned nil"
            );
            let _: () = msg_send![content, setAutoresizingMask: CONTENT_RESIZE_WITH_PARENT];
            let _: () = msg_send![glass, setContentView: content];
            let _: () = msg_send![content, release];
            // Place it behind GPUI's Metal view so native glass remains a backdrop, not an
            // input-intercepting overlay above the application.
            let _: () = msg_send![
                container,
                addSubview: glass
                positioned: WINDOW_BELOW
                relativeTo: native_view
            ];
            // The superview retained the newly allocated view.
            let _: () = msg_send![glass, release];
        }
    }
}

/// 安装或保持顶部导航那一条系统玻璃。内容区分栏不走这条路径。
pub(crate) fn sync(window: &Window, style: GlassStyle) {
    #[cfg(target_os = "macos")]
    imp::sync(window, style.ns_value());

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, style);
    }
}

#[cfg(test)]
mod tests {
    use super::GlassStyle;

    #[test]
    fn glass_style_serde_roundtrip() {
        assert_eq!(
            serde_json::from_str::<GlassStyle>("\"clear\"").unwrap(),
            GlassStyle::Clear
        );
        assert_eq!(
            serde_json::from_str::<GlassStyle>("\"regular\"").unwrap(),
            GlassStyle::Regular
        );
        assert_eq!(
            serde_json::to_string(&GlassStyle::Clear).unwrap(),
            "\"clear\""
        );
        assert_eq!(
            serde_json::to_string(&GlassStyle::Regular).unwrap(),
            "\"regular\""
        );
    }
}
