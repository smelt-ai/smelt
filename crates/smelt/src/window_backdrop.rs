//! 主窗口背后的系统材质。
//!
//! 有材质时主窗口透明，整窗拿材质做底，不铺主题底色（见 `workspace_frame::window_surface`）。
//! 不给开关：毛玻璃是 smelt 的样子，不是一项偏好；能选的是哪一种系统材质
//! （[`WindowMaterial`]，存在外观里）。材质整窗一块，不用跟着哪一栏同步位置和大小。着色、
//! 窗口失焦变样、系统里关掉透明效果都由系统处理。模糊程度系统不让调，不做。
//!
//! - macOS：宿主在 GPUI 的 Metal 视图下面垫一块铺满整窗的 `NSVisualEffectView`，透的是
//!   窗口后面的桌面和别的窗口。默认用系统侧栏那种（`Sidebar`）。
//! - Windows 11 22H2 起：系统背景材质 Mica、Acrylic、Mica Alt。GPUI 给了 Mica 和 Mica Alt
//!   （`MicaBackdrop` / `MicaAltBackdrop`），Acrylic 由这里设。Mica 取的是桌面壁纸的颜色，
//!   看不到后面的窗口，这是系统的设计；要透出后面的窗口选 Acrylic。
//! - 别的：没有材质，窗口不透明。Windows 更早的版本只有 GPUI 的 `Blurred`（Acrylic，走
//!   没公开的接口，和 22H2 起系统给的 Acrylic 不是一回事），拖窗口时会卡，不拿它兜底；
//!   Linux 的透明窗口看合成器，靠不住。
//!
//! 不用 GPUI 在 macOS 上的 `Blurred`：它固定用 `UnderWindowBackground`，又特意剥掉了
//! 系统的着色和饱和度，只剩纯模糊，铺在侧栏下面和实色分不出来。也不用 macOS 26 的
//! `NSGlassEffectView`：液态玻璃是给浮在内容上的控件用的，不是侧栏的底。

use gpui::{Window, WindowBackgroundAppearance};
use smelt_ui::ui_theme::MaterialTone;

/// 窗口底下垫哪一种系统材质。各平台能选的不一样，见 [`WindowMaterial::choices`]。
///
/// macOS 列的是 `NSVisualEffectMaterial` 里没废弃的全部 14 种，名字照系统给它定的用途叫；
/// 整窗只垫一块，用途只是说明它原本长什么样。Windows 是系统背景材质
/// （`DWMWA_SYSTEMBACKDROP_TYPE`）的全部三种。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowMaterial {
    Sidebar,
    Titlebar,
    HeaderView,
    Menu,
    Popover,
    Sheet,
    Hud,
    ToolTip,
    FullScreenUi,
    WindowBackground,
    ContentBackground,
    UnderWindow,
    UnderPage,
    Selection,
    /// Windows 11 的 Mica：取桌面壁纸的颜色，看不到后面的窗口。
    Mica,
    /// Windows 11 的 Acrylic：透出窗口后面的桌面和别的窗口，系统给临时窗口用的那种。
    Acrylic,
    /// Windows 11 的 Mica Alt：颜色更重，标签式窗口用的那种。
    MicaAlt,
}

impl WindowMaterial {
    /// 这个平台能选的材质，第一项是默认。没有材质的平台是空的。
    pub(crate) const fn choices() -> &'static [Self] {
        if cfg!(target_os = "macos") {
            &[
                Self::Sidebar,
                Self::Titlebar,
                Self::HeaderView,
                Self::Menu,
                Self::Popover,
                Self::Sheet,
                Self::Hud,
                Self::ToolTip,
                Self::FullScreenUi,
                Self::WindowBackground,
                Self::ContentBackground,
                Self::UnderWindow,
                Self::UnderPage,
                Self::Selection,
            ]
        } else if cfg!(windows) {
            &[Self::Mica, Self::Acrylic, Self::MicaAlt]
        } else {
            &[]
        }
    }

    /// 这个平台的默认材质。没有材质的平台随便给一个，反正用不上。
    pub(crate) fn platform_default() -> Self {
        Self::choices().first().copied().unwrap_or(Self::Sidebar)
    }

    /// 存库的名字。
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::Sidebar => "sidebar",
            Self::Titlebar => "titlebar",
            Self::HeaderView => "header-view",
            Self::Menu => "menu",
            Self::Popover => "popover",
            Self::Sheet => "sheet",
            Self::Hud => "hud",
            Self::ToolTip => "tool-tip",
            Self::FullScreenUi => "full-screen-ui",
            Self::WindowBackground => "window",
            Self::ContentBackground => "content",
            Self::UnderWindow => "under-window",
            Self::UnderPage => "under-page",
            Self::Selection => "selection",
            Self::Mica => "mica",
            Self::Acrylic => "acrylic",
            Self::MicaAlt => "mica-alt",
        }
    }

    /// 从库里的名字读回来。不认识的、这个平台没有的，回到默认。
    pub(crate) fn from_key(key: &str) -> Self {
        Self::choices()
            .iter()
            .copied()
            .find(|material| material.key() == key)
            .unwrap_or_else(Self::platform_default)
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Sidebar => "侧栏",
            Self::Titlebar => "标题栏",
            Self::HeaderView => "表头",
            Self::Menu => "菜单",
            Self::Popover => "弹出框",
            Self::Sheet => "对话框",
            Self::Hud => "浮层",
            Self::ToolTip => "工具提示",
            Self::FullScreenUi => "全屏界面",
            Self::WindowBackground => "窗口底（几乎不透）",
            Self::ContentBackground => "内容区（不透）",
            Self::UnderWindow => "窗口下层",
            Self::UnderPage => "页面背后",
            Self::Selection => "选中高亮（带强调色）",
            Self::Mica => "Mica",
            Self::Acrylic => "Acrylic（透出后面的窗口）",
            Self::MicaAlt => "Mica Alt（颜色更重）",
        }
    }

    /// 字压在这种材质上时按哪个颜色配（见 `ui_theme::MaterialTone`）。材质的颜色跟着桌面
    /// 变，这里是常见桌面上的大致颜色往不利的那头（深色往亮、浅色往暗）留了余量。
    /// TODO: 除侧栏外都是估值，按实机截图校准。
    const fn tone(self) -> MaterialTone {
        let (dark, light) = match self {
            // 常见约 #2D3136 / #E3E3E6。
            Self::Sidebar => (0x363a40, 0xd6d6da),
            Self::Titlebar => (0x34343a, 0xdadade),
            Self::HeaderView => (0x323236, 0xd8d8da),
            Self::Menu => (0x343438, 0xd8d8db),
            Self::Popover => (0x38383c, 0xdcdcdf),
            Self::Sheet => (0x38383a, 0xdedee0),
            Self::Hud => (0x2c2c2f, 0xd0d0d3),
            Self::ToolTip => (0x3e3e40, 0xe4e4e6),
            Self::FullScreenUi => (0x2e2e30, 0xd2d2d4),
            // 系统窗口底色 #323232 / #ECECEC，几乎不透，余量留得少。
            Self::WindowBackground => (0x363636, 0xe2e2e2),
            // 不透，约 #1E1E1E / #FFFFFF。
            Self::ContentBackground => (0x242424, 0xf6f6f6),
            Self::UnderWindow => (0x2e3034, 0xdcdcdf),
            Self::UnderPage => (0x303032, 0xc8c8cb),
            // 带系统强调色，按默认的蓝估。
            Self::Selection => (0x2b4f86, 0xb5cdf0),
            // Mica 常见约 #202020 / #F3F3F3。
            Self::Mica => (0x2c2c2c, 0xe6e6e6),
            Self::Acrylic => (0x343434, 0xdadada),
            Self::MicaAlt => (0x2a2a2a, 0xd8d8d8),
        };
        MaterialTone { dark, light }
    }
}

/// 字压着的那层材质的代表色，交给色板配字色。这台机器上没有材质时是 `None`。
pub(crate) fn text_tone(material: WindowMaterial) -> Option<MaterialTone> {
    available().then(|| material.tone())
}

/// 这台机器上有没有窗口材质可透。render 里会问，Windows 上查一次系统版本后缓存。
#[cfg(windows)]
pub(crate) fn available() -> bool {
    imp::mica_supported()
}

/// 这台机器上有没有窗口材质可透。
#[cfg(not(windows))]
pub(crate) const fn available() -> bool {
    cfg!(target_os = "macos")
}

/// 主窗口的背景外观：有材质时窗口透明、材质垫在下面，没有就不透明。Windows 上材质就是
/// 背景外观本身；GPUI 没有 Acrylic，先给 Mica，`imp::sync` 再把系统背景材质换成 Acrylic。
pub(crate) fn window_background(material: WindowMaterial) -> WindowBackgroundAppearance {
    if !available() {
        return WindowBackgroundAppearance::Opaque;
    }
    match material {
        WindowMaterial::Mica | WindowMaterial::Acrylic => WindowBackgroundAppearance::MicaBackdrop,
        WindowMaterial::MicaAlt => WindowBackgroundAppearance::MicaAltBackdrop,
        _ => WindowBackgroundAppearance::Transparent,
    }
}

/// 把材质种类和深浅设成外观里的样子（macOS 上第一次调用时顺手垫上材质）。
///
/// 材质默认跟系统的深浅，用户在 smelt 里选的主题可以和系统相反，所以每次换主题都要
/// 把明暗告诉它。Windows 上系统换深浅时 GPUI 会把窗口的深浅改回跟系统，调用方要在
/// 系统深浅变了的时候也再调一次。
pub(crate) fn sync(window: &Window, material: WindowMaterial, dark: bool) {
    if !available() {
        return;
    }

    #[cfg(any(target_os = "macos", windows))]
    imp::sync(window, material, dark);

    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = (window, material, dark);
}

#[cfg(target_os = "macos")]
mod imp {
    use gpui::Window;
    use objc::runtime::{BOOL, NO, Object};
    use objc::{class, msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::WindowMaterial;

    /// `NSVisualEffectMaterial` 的取值。
    fn ns_material(material: WindowMaterial) -> i64 {
        match material {
            WindowMaterial::Titlebar => 3,
            WindowMaterial::Selection => 4,
            WindowMaterial::Menu => 5,
            WindowMaterial::Popover => 6,
            WindowMaterial::Sidebar => 7,
            WindowMaterial::HeaderView => 10,
            WindowMaterial::Sheet => 11,
            WindowMaterial::WindowBackground => 12,
            WindowMaterial::Hud => 13,
            WindowMaterial::FullScreenUi => 15,
            WindowMaterial::ToolTip => 17,
            WindowMaterial::ContentBackground => 18,
            WindowMaterial::UnderWindow => 21,
            WindowMaterial::UnderPage => 22,
            // 不是 macOS 的材质不会选到这里，给默认的侧栏。
            WindowMaterial::Mica | WindowMaterial::Acrylic | WindowMaterial::MicaAlt => 7,
        }
    }
    /// `NSVisualEffectBlendingModeBehindWindow`：透的是窗口后面的桌面和别的窗口。
    const BLENDING_BEHIND_WINDOW: i64 = 0;
    /// `NSVisualEffectStateFollowsWindowActiveState`：窗口失焦时和系统侧栏一样变灰。
    const STATE_FOLLOWS_WINDOW: i64 = 0;
    /// `NSViewWidthSizable | NSViewHeightSizable`：跟着内容视图一起变大小。
    const FILL_PARENT: u64 = (1 << 1) | (1 << 4);
    /// `NSWindowBelow`。
    const BELOW: i64 = -1;

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

    /// GPUI 的 Metal 视图和装着它的内容视图。
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

    /// 已经垫上的材质。只认正好是 `NSVisualEffectView` 的：GPUI 的 `Blurred` 用的是它的
    /// 子类，不能把那块当成自己的。
    unsafe fn find_backdrop(container: *mut Object) -> Option<*mut Object> {
        unsafe {
            let subviews: *mut Object = msg_send![container, subviews];
            if subviews.is_null() {
                return None;
            }
            let count: usize = msg_send![subviews, count];
            (0..count).find_map(|i| {
                let view: *mut Object = msg_send![subviews, objectAtIndex: i];
                let exact: BOOL = msg_send![view, isMemberOfClass: class!(NSVisualEffectView)];
                (!view.is_null() && exact != NO).then_some(view)
            })
        }
    }

    unsafe fn install(native_view: *mut Object, container: *mut Object) -> *mut Object {
        unsafe {
            let bounds: NSRect = msg_send![container, bounds];
            let view: *mut Object = msg_send![class!(NSVisualEffectView), alloc];
            let view: *mut Object = msg_send![view, initWithFrame: bounds];
            assert!(
                !view.is_null(),
                "NSVisualEffectView initialization unexpectedly returned nil"
            );
            let _: () = msg_send![view, setBlendingMode: BLENDING_BEHIND_WINDOW];
            let _: () = msg_send![view, setState: STATE_FOLLOWS_WINDOW];
            let _: () = msg_send![view, setAutoresizingMask: FILL_PARENT];
            // 放在 Metal 视图下面：材质只是底，点击照常落到 GPUI。
            let _: () = msg_send![
                container,
                addSubview: view
                positioned: BELOW
                relativeTo: native_view
            ];
            // 父视图已经持有它。
            let _: () = msg_send![view, release];
            view
        }
    }

    pub(super) fn sync(window: &Window, material: WindowMaterial, dark: bool) {
        let (native_view, container) = native_views(window);
        unsafe {
            let backdrop = match find_backdrop(container) {
                Some(view) => view,
                None => install(native_view, container),
            };
            let _: () = msg_send![backdrop, setMaterial: ns_material(material)];
            let name = if dark {
                c"NSAppearanceNameDarkAqua"
            } else {
                c"NSAppearanceNameAqua"
            };
            let name: *mut Object =
                msg_send![class!(NSString), stringWithUTF8String: name.as_ptr()];
            let appearance: *mut Object = msg_send![class!(NSAppearance), appearanceNamed: name];
            let _: () = msg_send![backdrop, setAppearance: appearance];
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::OnceLock;

    use gpui::Window;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Dwm::{
        DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
        DwmSetWindowAttribute,
    };
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
    use windows_sys::core::BOOL;

    /// `DWMWA_SYSTEMBACKDROP_TYPE` 从这个版本（Windows 11 22H2）起才有。GPUI 的
    /// `MicaBackdrop` 在更早的系统上什么也不做，窗口却照样画成透明，所以要先挡在这里。
    const FIRST_MICA_BUILD: u32 = 22621;

    pub(super) fn mica_supported() -> bool {
        static SUPPORTED: OnceLock<bool> = OnceLock::new();
        *SUPPORTED.get_or_init(|| {
            let mut info = OSVERSIONINFOW {
                dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
                ..Default::default()
            };
            // `GetVersionEx` 会被兼容层改写版本号，`RtlGetVersion` 给的是真的。
            let status = unsafe { RtlGetVersion(&mut info) };
            status >= 0 && info.dwBuildNumber >= FIRST_MICA_BUILD
        })
    }

    /// 先换材质，再设深浅：材质的深浅跟着窗口的深色模式标记走，这个标记也决定系统
    /// 标题栏的深浅。
    pub(super) fn sync(window: &Window, material: super::WindowMaterial, dark: bool) {
        window.set_background_appearance(super::window_background(material));
        let handle = HasWindowHandle::window_handle(window)
            .expect("GPUI window must expose a native window handle");
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            unreachable!("Windows GPUI windows must use Win32 window handles");
        };
        let hwnd = handle.hwnd.get() as HWND;
        // GPUI 只会设 Mica 和 Mica Alt：上面已经按 Mica 把窗口画成透明，这里把系统背景
        // 材质换成 Acrylic。
        if material == super::WindowMaterial::Acrylic {
            let backdrop = DWMSBT_TRANSIENTWINDOW;
            set_attribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE as u32,
                &backdrop,
                "背景材质",
            );
        }
        let dark = BOOL::from(dark);
        set_attribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            &dark,
            "窗口深浅",
        );
    }

    fn set_attribute<T>(hwnd: HWND, attribute: u32, value: &T, what: &str) {
        let result = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                attribute,
                (value as *const T).cast(),
                size_of::<T>() as u32,
            )
        };
        if result < 0 {
            eprintln!("[window] 设置{what}失败：HRESULT {result:#x}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WindowMaterial;

    /// 这个平台能选的材质按名字存进库再读回来不变；旧库里的 `regular`、空串、别的平台的
    /// 材质都回到默认。
    #[test]
    fn material_keys_round_trip_and_unknown_ones_fall_back() {
        for &material in WindowMaterial::choices() {
            assert_eq!(WindowMaterial::from_key(material.key()), material);
        }
        let foreign = if cfg!(windows) { "sidebar" } else { "mica" };
        for key in ["regular", "", foreign] {
            assert_eq!(
                WindowMaterial::from_key(key),
                WindowMaterial::platform_default(),
                "{key:?}"
            );
        }
    }
}
