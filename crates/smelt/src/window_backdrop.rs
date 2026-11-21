//! 主窗口背后的系统材质（毛玻璃），按地方垫。
//!
//! 哪几块地方透、透哪种材质由布局说（`smelt_plugin_api::LayoutMaterials`，按用途：侧栏、
//! 窗口底），宿主不认识哪块是什么。窗口本身透明，每帧画内容之前先铺底（[`paint`]）：声明了
//! 材质的地方按外观里的底色不透明度铺一层主题底色、透出底下的材质，别处铺实的主题底色。
//! 没声明材质的布局、放大了一块面板（停靠区都不画）时整扇窗口是实色。
//!
//! 各块地方的位置要等排版完才知道：外壳的 Dock 在 prepaint 里把各停靠区和中间的框记进
//! [`Regions`]，窗口根上最先画的那块在 paint 里读（GPUI 先给整棵树 prepaint，再整棵 paint，
//! 所以读到的是这一帧的位置），铺底、对齐材质，一帧之内完成。
//!
//! GPUI Fast 里没通知的视图照上一帧拷，prepaint、paint 都不再跑，所以两头都不能假设每帧都
//! 会走到：位置是 Dock 那一次重画记下的一整套（[`Regions::begin`] 开头作废上一套），Dock
//! 被拷的那帧原样留着、照样有效；铺底在窗口根（Workspace）上，Dock 一变 Workspace 就跟着
//! 重画（`Workspace::watch_page_dock`），新位置当帧铺上。字的配色按最透的那种材质算（[`text_tone`]），
//! 实色的地方对比只会更足。着色、窗口失焦变样、系统里关掉透明效果都由系统处理；模糊程度
//! 系统不让调，不做。
//!
//! - macOS：在 GPUI 的 Metal 视图下面，每块透的地方垫一块 `NSVisualEffectView`，位置大小
//!   跟着那块地方，透的是窗口后面的桌面和别的窗口。侧栏用系统侧栏材质，窗口底用系统窗口底
//!   材质。
//! - Windows 11 22H2 起：系统背景材质只能整扇窗口一种，用 Mica（GPUI 的 `MicaBackdrop`），
//!   声明了的地方都透它，用途分不出来。Mica 取的是桌面壁纸的颜色，看不到后面的窗口，这是
//!   系统给主窗口的设计。
//! - 别的：没有材质，窗口不透明，整扇实色。Windows 更早的版本只有 GPUI 的 `Blurred`（走
//!   没公开的接口），拖窗口时会卡，不拿它兜底；Linux 的透明窗口看合成器，靠不住。
//!
//! 不用 GPUI 在 macOS 上的 `Blurred`：它固定用 `UnderWindowBackground`，又特意剥掉了
//! 系统的着色和饱和度，只剩纯模糊，铺在侧栏下面和实色分不出来。也不用 macOS 26 的
//! `NSGlassEffectView`：液态玻璃是给浮在内容上的控件用的，不是侧栏的底。

use gpui::{Bounds, Pixels, Window, WindowBackgroundAppearance, fill, point, px, size};
use gpui_base::dock::DockPlacement;
use smelt_plugin_api::{LayoutMaterials, Material};
use smelt_ui::ui_theme::{self, MaterialTone};

/// 一种材质在这个平台上的代表色：字压在上面时按它配（见 `ui_theme::MaterialTone`）。材质
/// 的颜色跟着桌面变，这里是常见桌面上的大致颜色往不利的那头（深色往亮、浅色往暗）留了余量。
/// TODO: 窗口底是估值，按实机截图校准。
fn tone(material: Material) -> MaterialTone {
    let (dark, light) = if cfg!(windows) {
        // Mica 常见约 #202020 / #F3F3F3。
        (0x2c2c2c, 0xe6e6e6)
    } else {
        match material {
            // 常见约 #2D3136 / #E3E3E6。
            Material::Sidebar => (0x363a40, 0xd6d6da),
            // 系统窗口底色 #323232 / #ECECEC，几乎不透，余量留得少。
            Material::Window => (0x363636, 0xe2e2e2),
        }
    };
    MaterialTone { dark, light }
}

/// 字压着的材质的代表色，交给色板配字色：哪种材质都可能被布局用上，取最不利的那种（深色
/// 取最亮的、浅色取最暗的）。实色的地方比材质离字更远，对比只会更足。这台机器上没有材质
/// 时是 `None`。
pub(crate) fn text_tone() -> Option<MaterialTone> {
    if !available() {
        return None;
    }
    let luminance = |color: u32| (color >> 16 & 0xff) + (color >> 8 & 0xff) + (color & 0xff);
    let tones = Material::ALL.map(tone);
    Some(MaterialTone {
        dark: tones
            .iter()
            .map(|tone| tone.dark)
            .max_by_key(|&c| luminance(c))?,
        light: tones
            .iter()
            .map(|tone| tone.light)
            .min_by_key(|&c| luminance(c))?,
    })
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

/// 主窗口的背景外观：有材质的机器窗口透明（Windows 上是 Mica），底由 [`paint`] 按地方铺；
/// 没有就不透明。
pub(crate) fn window_background() -> WindowBackgroundAppearance {
    if !available() {
        WindowBackgroundAppearance::Opaque
    } else if cfg!(windows) {
        WindowBackgroundAppearance::MicaBackdrop
    } else {
        WindowBackgroundAppearance::Transparent
    }
}

/// 外壳的 Dock 上一次重画时记下的各块地方的位置（窗口坐标），窗口根在 paint 里读。没画的
/// （停靠区关着、放大了一块面板）就没有。
#[derive(Default)]
pub(crate) struct Regions {
    left: Option<Bounds<Pixels>>,
    right: Option<Bounds<Pixels>>,
    bottom: Option<Bounds<Pixels>>,
    /// 中间那一列，下停靠区也在里面。
    center: Option<Bounds<Pixels>>,
}

impl Regions {
    /// Dock 开始 prepaint：上一次记的作废，这次画出来的各块随后记上。
    pub(crate) fn begin(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn record(&mut self, placement: DockPlacement, bounds: Bounds<Pixels>) {
        let slot = match placement {
            DockPlacement::Left => &mut self.left,
            DockPlacement::Right => &mut self.right,
            DockPlacement::Bottom => &mut self.bottom,
            DockPlacement::Center => &mut self.center,
        };
        *slot = Some(bounds);
    }
}

/// 外壳和窗口根共用的那份 [`Regions`]：外壳的各页 Dock 往里记，窗口根读。
pub(crate) type SharedRegions = std::rc::Rc<std::cell::RefCell<Regions>>;

/// 透的地方：哪种材质，铺在哪几块矩形里。
type Panes = Vec<(Material, Rect)>;

/// 已经交给系统的材质，没变就不再发。
#[derive(Default)]
pub(crate) struct Backdrop {
    applied: Option<(Panes, bool)>,
}

impl Backdrop {
    /// 下一帧不管变没变都重发一次：Windows 上系统换深浅时 GPUI 会把窗口的深浅改回跟系统。
    pub(crate) fn invalidate(&mut self) {
        self.applied = None;
    }
}

/// 铺这一帧的窗口底，把材质对齐到透的那几块地方。窗口根上最先画的那块在 paint 里调。
///
/// `window_bounds` 是整扇窗口；`materials` 是开着的那页的布局说的；`regions` 是外壳的 Dock
/// 记下的位置；`bg_opacity` 是透的地方主题底色铺多实，1 就是不透、不垫材质。
pub(crate) fn paint(
    window_bounds: Bounds<Pixels>,
    materials: LayoutMaterials,
    regions: &Regions,
    bg_opacity: f32,
    backdrop: &mut Backdrop,
    window: &mut Window,
) {
    let panes = paint_floor(window_bounds, materials, regions, bg_opacity, window);
    let dark = !ui_theme::is_light();
    if backdrop.applied.as_ref() == Some(&(panes.clone(), dark)) {
        return;
    }
    sync(window, window_bounds, &panes, dark);
    backdrop.applied = Some((panes, dark));
}

/// 铺底：透的地方铺半透的主题底色，别处铺实的。返回透的那几块，交给系统垫材质。
fn paint_floor(
    window_bounds: Bounds<Pixels>,
    materials: LayoutMaterials,
    regions: &Regions,
    bg_opacity: f32,
    window: &mut Window,
) -> Panes {
    let scale = window.scale_factor();
    let panes = if available() && bg_opacity < 1.0 {
        glass_panes(materials, regions, scale)
    } else {
        Vec::new()
    };
    let solid = ui_theme::solid_fill();
    let glass = ui_theme::glass_fill();
    let holes: Vec<Rect> = panes.iter().map(|(_, rect)| *rect).collect();
    for rect in subtract_all(Rect::snapped(window_bounds, scale), &holes) {
        window.paint_quad(fill(rect.bounds(), solid));
    }
    for (_, rect) in &panes {
        window.paint_quad(fill(rect.bounds(), glass));
    }
    panes
}

/// 布局声明了材质、这一帧又画出来了的地方。中间那一列要扣掉压在它下面的下停靠区。
fn glass_panes(materials: LayoutMaterials, regions: &Regions, scale: f32) -> Panes {
    let mut panes = Vec::new();
    let mut add = |placement: DockPlacement, bounds: Option<Bounds<Pixels>>, holes: &[Rect]| {
        if let (Some(material), Some(bounds)) = (materials.get(placement), bounds) {
            for rect in subtract_all(Rect::snapped(bounds, scale), holes) {
                panes.push((material, rect));
            }
        }
    };
    let bottom: Vec<Rect> = regions
        .bottom
        .map(|bounds| Rect::snapped(bounds, scale))
        .into_iter()
        .collect();
    add(DockPlacement::Center, regions.center, &bottom);
    add(DockPlacement::Bottom, regions.bottom, &[]);
    add(DockPlacement::Left, regions.left, &[]);
    add(DockPlacement::Right, regions.right, &[]);
    panes
}

/// 一块矩形，按设备像素对齐过的逻辑坐标：相邻两块（实色和透的）严丝合缝，接缝处不漏一条
/// 半透的线。
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

impl Rect {
    fn snapped(bounds: Bounds<Pixels>, scale: f32) -> Self {
        let snap = |value: Pixels| (f32::from(value) * scale).round() / scale;
        Self {
            left: snap(bounds.origin.x),
            top: snap(bounds.origin.y),
            right: snap(bounds.origin.x + bounds.size.width),
            bottom: snap(bounds.origin.y + bounds.size.height),
        }
    }

    fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }

    fn bounds(&self) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(self.left), px(self.top)),
            size: size(px(self.right - self.left), px(self.bottom - self.top)),
        }
    }

    /// 扣掉 `hole` 剩下的部分：上、下两条整宽，中间左、右两块，空的不要。
    fn subtract(self, hole: Rect) -> Vec<Rect> {
        let hole = Rect {
            left: hole.left.max(self.left),
            top: hole.top.max(self.top),
            right: hole.right.min(self.right),
            bottom: hole.bottom.min(self.bottom),
        };
        if hole.is_empty() {
            return vec![self];
        }
        [
            Rect {
                bottom: hole.top,
                ..self
            },
            Rect {
                top: hole.bottom,
                ..self
            },
            Rect {
                top: hole.top,
                bottom: hole.bottom,
                right: hole.left,
                ..self
            },
            Rect {
                top: hole.top,
                bottom: hole.bottom,
                left: hole.right,
                ..self
            },
        ]
        .into_iter()
        .filter(|rect| !rect.is_empty())
        .collect()
    }
}

fn subtract_all(rect: Rect, holes: &[Rect]) -> Vec<Rect> {
    holes.iter().fold(vec![rect], |rects, hole| {
        rects
            .into_iter()
            .flat_map(|rect| rect.subtract(*hole))
            .collect()
    })
}

/// 把材质交给系统：macOS 上按透的地方摆好各块材质视图，Windows 上只有整窗一种，设深浅。
fn sync(window: &Window, window_bounds: Bounds<Pixels>, panes: &Panes, dark: bool) {
    #[cfg(target_os = "macos")]
    imp::sync(window, window_bounds, panes, dark);

    #[cfg(windows)]
    {
        let _ = (window_bounds, panes);
        imp::set_dark(window, dark);
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = (window, window_bounds, panes, dark);
}

#[cfg(target_os = "macos")]
mod imp {
    use gpui::{Bounds, Pixels, Window};
    use objc::runtime::{BOOL, NO, Object, YES};
    use objc::{class, msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use smelt_plugin_api::Material;

    use super::Panes;

    /// `NSVisualEffectMaterial` 的取值。
    fn ns_material(material: Material) -> i64 {
        match material {
            Material::Sidebar => 7,
            Material::Window => 12,
        }
    }
    /// `NSVisualEffectBlendingModeBehindWindow`：透的是窗口后面的桌面和别的窗口。
    const BLENDING_BEHIND_WINDOW: i64 = 0;
    /// `NSVisualEffectStateFollowsWindowActiveState`：窗口失焦时和系统侧栏一样变灰。
    const STATE_FOLLOWS_WINDOW: i64 = 0;
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

    /// 已经垫上的材质视图，按先后。只认正好是 `NSVisualEffectView` 的：GPUI 的 `Blurred`
    /// 用的是它的子类，不能当成自己的。
    unsafe fn our_views(container: *mut Object) -> Vec<*mut Object> {
        unsafe {
            let subviews: *mut Object = msg_send![container, subviews];
            if subviews.is_null() {
                return Vec::new();
            }
            let count: usize = msg_send![subviews, count];
            (0..count)
                .map(|i| -> *mut Object { msg_send![subviews, objectAtIndex: i] })
                .filter(|&view| {
                    let exact: BOOL = msg_send![view, isMemberOfClass: class!(NSVisualEffectView)];
                    !view.is_null() && exact != NO
                })
                .collect()
        }
    }

    unsafe fn install(native_view: *mut Object, container: *mut Object) -> *mut Object {
        unsafe {
            let view: *mut Object = msg_send![class!(NSVisualEffectView), alloc];
            let view: *mut Object = msg_send![view, init];
            assert!(
                !view.is_null(),
                "NSVisualEffectView initialization unexpectedly returned nil"
            );
            let _: () = msg_send![view, setBlendingMode: BLENDING_BEHIND_WINDOW];
            let _: () = msg_send![view, setState: STATE_FOLLOWS_WINDOW];
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

    /// 每块透的地方一块材质视图，多的撤掉。位置换成内容视图的坐标：GPUI 的原点在左上，
    /// AppKit 的不翻转视图原点在左下。
    pub(super) fn sync(window: &Window, window_bounds: Bounds<Pixels>, panes: &Panes, dark: bool) {
        let (native_view, container) = native_views(window);
        unsafe {
            let flipped: BOOL = msg_send![container, isFlipped];
            let container_bounds: NSRect = msg_send![container, bounds];
            let name = if dark {
                c"NSAppearanceNameDarkAqua"
            } else {
                c"NSAppearanceNameAqua"
            };
            let name: *mut Object =
                msg_send![class!(NSString), stringWithUTF8String: name.as_ptr()];
            let appearance: *mut Object = msg_send![class!(NSAppearance), appearanceNamed: name];

            let mut views = our_views(container).into_iter();
            for (material, rect) in panes {
                let view = views
                    .next()
                    .unwrap_or_else(|| install(native_view, container));
                let top = f64::from(rect.top - f32::from(window_bounds.origin.y));
                let height = f64::from(rect.bottom - rect.top);
                let y = if flipped == YES {
                    top
                } else {
                    container_bounds.size.height - top - height
                };
                let frame = NSRect {
                    origin: NSPoint {
                        x: f64::from(rect.left - f32::from(window_bounds.origin.x)),
                        y,
                    },
                    size: NSSize {
                        width: f64::from(rect.right - rect.left),
                        height,
                    },
                };
                let _: () = msg_send![view, setFrame: frame];
                let _: () = msg_send![view, setMaterial: ns_material(*material)];
                let _: () = msg_send![view, setAppearance: appearance];
            }
            for extra in views {
                let _: () = msg_send![extra, removeFromSuperview];
            }
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
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
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

    /// Mica 的深浅跟着窗口的深色模式标记走，这个标记也决定系统标题栏的深浅。
    pub(super) fn set_dark(window: &Window, dark: bool) {
        let handle = HasWindowHandle::window_handle(window)
            .expect("GPUI window must expose a native window handle");
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            unreachable!("Windows GPUI windows must use Win32 window handles");
        };
        let hwnd = handle.hwnd.get() as HWND;
        let dark = BOOL::from(dark);
        let result = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                (&dark as *const BOOL).cast(),
                size_of::<BOOL>() as u32,
            )
        };
        if result < 0 {
            eprintln!("[window] 设置窗口深浅失败：HRESULT {result:#x}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: f32, top: f32, right: f32, bottom: f32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    fn bounds(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(x), px(y)),
            size: size(px(w), px(h)),
        }
    }

    fn area(rects: &[Rect]) -> f32 {
        rects
            .iter()
            .map(|r| (r.right - r.left) * (r.bottom - r.top))
            .sum()
    }

    /// 扣掉一块，剩下的面积对、不和扣掉的那块重叠；扣在外面的不动。
    #[test]
    fn subtracting_a_hole_leaves_the_rest() {
        let window = rect(0., 0., 100., 80.);
        let left = rect(0., 0., 30., 80.);
        let rest = subtract_all(window, &[left]);
        assert_eq!(rest, [rect(30., 0., 100., 80.)]);
        let middle = rect(40., 20., 60., 50.);
        let rest = subtract_all(window, &[middle]);
        assert_eq!(rest.len(), 4);
        assert_eq!(area(&rest), 100. * 80. - 20. * 30.);
        assert_eq!(subtract_all(window, &[rect(200., 0., 300., 80.)]), [window]);
        assert!(subtract_all(window, &[window]).is_empty());
    }

    /// 只有布局声明了的地方透；中间那一列扣掉下停靠区，下停靠区自己没声明就是实色。
    #[test]
    fn only_declared_regions_become_glass() {
        let materials = LayoutMaterials {
            left: Some(Material::Sidebar),
            center: Some(Material::Window),
            ..LayoutMaterials::default()
        };
        let mut regions = Regions::default();
        regions.record(DockPlacement::Left, bounds(0., 0., 30., 80.));
        regions.record(DockPlacement::Center, bounds(30., 0., 50., 80.));
        regions.record(DockPlacement::Bottom, bounds(30., 60., 50., 20.));
        regions.record(DockPlacement::Right, bounds(80., 0., 20., 80.));
        let panes = glass_panes(materials, &regions, 2.);
        assert_eq!(
            panes,
            [
                (Material::Window, rect(30., 0., 80., 60.)),
                (Material::Sidebar, rect(0., 0., 30., 80.)),
            ]
        );
        assert!(glass_panes(LayoutMaterials::default(), &regions, 2.).is_empty());
    }

    /// 外壳那层：最先铺底，读停靠区记下的位置；里面套着停靠区。
    struct Shell {
        area: gpui::Entity<Area>,
        regions: SharedRegions,
        floors: std::rc::Rc<std::cell::RefCell<Vec<Panes>>>,
        _area_changed: gpui::Subscription,
    }

    /// 停靠区那层：左边一栏、右边中间那一列，排好后记下位置。
    struct Area {
        regions: SharedRegions,
        left: bool,
    }

    impl gpui::Render for Shell {
        fn render(
            &mut self,
            _: &mut Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            use gpui::{ParentElement as _, Styled as _};
            let regions = self.regions.clone();
            let floors = self.floors.clone();
            let materials = LayoutMaterials {
                left: Some(Material::Sidebar),
                ..LayoutMaterials::default()
            };
            gpui::div()
                .relative()
                .size_full()
                .child(
                    gpui::canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _| {
                            let panes =
                                paint_floor(bounds, materials, &regions.borrow(), 0.5, window);
                            floors.borrow_mut().push(panes);
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
                .child(self.area.clone())
        }
    }

    impl gpui::Render for Area {
        fn render(
            &mut self,
            _: &mut Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            use gpui::{ParentElement as _, Styled as _};
            use gpui_base::ElementExt as _;
            let record = |placement| {
                let regions = self.regions.clone();
                move |bounds, _: &mut Window, _: &mut gpui::App| {
                    regions.borrow_mut().record(placement, bounds)
                }
            };
            let regions = self.regions.clone();
            gpui::div()
                .size_full()
                .flex()
                .flex_row()
                .on_prepaint(move |_, _, _| regions.borrow_mut().begin())
                .children(self.left.then(|| {
                    gpui::div()
                        .w(px(30.))
                        .h_full()
                        .on_prepaint(record(DockPlacement::Left))
                }))
                .child(
                    gpui::div()
                        .flex_1()
                        .h_full()
                        .on_prepaint(record(DockPlacement::Center)),
                )
        }
    }

    /// 按真的出帧画一帧：没通知的视图照上一帧拷（`VisualTestContext::draw` 整窗重画，看不出来）。
    fn frame(cx: &mut gpui::VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn shows_left(panes: &Panes) -> bool {
        panes
            .iter()
            .any(|(material, _)| *material == Material::Sidebar)
    }

    /// 只有外壳重画、停靠区照上一帧拷的那帧，左栏照样透：位置不是每帧都会再记一遍。
    #[gpui::test]
    fn the_floor_keeps_a_region_whose_view_is_copied(cx: &mut gpui::TestAppContext) {
        let regions = SharedRegions::default();
        let floors = std::rc::Rc::<std::cell::RefCell<Vec<Panes>>>::default();
        let (shell, cx) = shell(cx, &regions, &floors);
        frame(cx);
        frame(cx);
        assert!(shows_left(floors.borrow().last().unwrap()));

        shell.update(cx, |_, cx| cx.notify());
        frame(cx);
        assert!(
            shows_left(floors.borrow().last().unwrap()),
            "外壳单独重画时左栏变成了实色"
        );
    }

    /// 只有停靠区变了（关掉左栏）的那帧，铺底当帧跟上，不照上一帧拷。
    #[gpui::test]
    fn the_floor_follows_the_area_in_the_same_frame(cx: &mut gpui::TestAppContext) {
        let regions = SharedRegions::default();
        let floors = std::rc::Rc::<std::cell::RefCell<Vec<Panes>>>::default();
        let (shell, cx) = shell(cx, &regions, &floors);
        frame(cx);
        frame(cx);
        assert!(shows_left(floors.borrow().last().unwrap()));

        let area = shell.read_with(cx, |shell, _| shell.area.clone());
        area.update(cx, |area, cx| {
            area.left = false;
            cx.notify();
        });
        frame(cx);
        assert!(
            !shows_left(floors.borrow().last().unwrap()),
            "关掉左栏那帧铺底还是旧的"
        );
    }

    /// 外壳套着停靠区，像 Workspace 那样订阅停靠区的变化。
    fn shell<'a>(
        cx: &'a mut gpui::TestAppContext,
        regions: &SharedRegions,
        floors: &std::rc::Rc<std::cell::RefCell<Vec<Panes>>>,
    ) -> (gpui::Entity<Shell>, &'a mut gpui::VisualTestContext) {
        cx.add_window_view(|_, cx| {
            let area = gpui::AppContext::new(cx, |_| Area {
                regions: regions.clone(),
                left: true,
            });
            Shell {
                _area_changed: cx.observe(&area, |_, _, cx| cx.notify()),
                area,
                regions: regions.clone(),
                floors: floors.clone(),
            }
        })
    }

    /// 按设备像素对齐：两倍屏上对到半点，实色和透的地方之间不留缝。
    #[test]
    fn rects_snap_to_device_pixels() {
        assert_eq!(
            Rect::snapped(bounds(0.3, 0., 29.9, 10.), 2.),
            rect(0.5, 0., 30., 10.)
        );
    }

    /// 字按最透的那种材质配：深色取最亮的，浅色取最暗的。
    #[cfg(target_os = "macos")]
    #[test]
    fn text_tone_takes_the_least_favourable_material() {
        assert_eq!(
            text_tone(),
            Some(MaterialTone {
                dark: 0x363a40,
                light: 0xd6d6da,
            })
        );
    }
}
