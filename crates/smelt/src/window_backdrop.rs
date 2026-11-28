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
//! - macOS：在 GPUI 的 Metal 视图下面垫 `NSVisualEffectView`，透的是窗口后面的桌面和别的
//!   窗口。侧栏用系统侧栏材质，窗口底用系统窗口底材质。整页只声明了一种材质时（如出厂布局
//!   只有左栏 `Sidebar`），底下常驻一块铺满内容视图的 `NSVisualEffectView`，由 Metal 层的
//!   实色 / 半透色块在单帧内原子裁出毛玻璃区域——停靠区开合、拖宽、放大都不增删或移动原生
//!   视图，避免异步 `CAMetalLayer` 与 AppKit `CATransaction` 错帧露出窗口后的画面；只有整页
//!   混用多种材质时才按块放置，并在增删/切换方案时用单帧过渡保底。
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
use smelt_plugin_api::DockPlacement;
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
#[derive(Clone, Default, PartialEq)]
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

    pub(crate) fn set(&mut self, placement: DockPlacement, bounds: Option<Bounds<Pixels>>) {
        let bounds = bounds.filter(|b| b.size.width > px(0.) && b.size.height > px(0.));
        let slot = match placement {
            DockPlacement::Left => &mut self.left,
            DockPlacement::Right => &mut self.right,
            DockPlacement::Bottom => &mut self.bottom,
            DockPlacement::Center => &mut self.center,
        };
        *slot = bounds;
    }

    #[cfg(test)]
    pub(crate) fn record(&mut self, placement: DockPlacement, bounds: Bounds<Pixels>) {
        self.set(placement, Some(bounds));
    }
}

/// 外壳和窗口根共用的那份 [`Regions`]：外壳的各页 Dock 往里记，窗口根读。
pub(crate) type SharedRegions = std::rc::Rc<std::cell::RefCell<Regions>>;

/// 透的地方：哪种材质，铺在哪几块矩形里。
type Panes = Vec<(Material, Rect)>;

/// 布局声明的材质种类：全无、单一材质、还是混用多种材质。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MaterialKind {
    None,
    Single(Material),
    Multiple,
}

fn material_kind(materials: LayoutMaterials) -> MaterialKind {
    let mut single = None;
    for material in [
        materials.left,
        materials.right,
        materials.bottom,
        materials.center,
    ]
    .into_iter()
    .flatten()
    {
        match single {
            None => single = Some(material),
            Some(existing) if existing == material => {}
            Some(_) => return MaterialKind::Multiple,
        }
    }
    match single {
        Some(material) => MaterialKind::Single(material),
        None => MaterialKind::None,
    }
}

/// 交给系统垫在 Metal 视图底下的材质方案。
#[derive(Clone, Debug, PartialEq)]
enum SystemBackdrop {
    /// 不垫材质视图。
    None,
    /// 整扇内容视图常驻一块材质视图（`ViewWidthSizable | ViewHeightSizable`），
    /// 由 Metal 层的 `solid_fill` / `glass_fill` 在单帧内原子裁出透出区。
    WholeWindow(Material),
    /// 混用多种材质时按各块可见玻璃区域分别放置。
    PerPane(Panes),
}

impl SystemBackdrop {
    /// 已就位的系统材质视图在几何上是否完全盖住 `panes` 的每一块矩形。
    fn covers_geometry(&self, panes: &[(Material, Rect)]) -> bool {
        if panes.is_empty() {
            return true;
        }
        match self {
            Self::None => false,
            Self::WholeWindow(_) => true,
            Self::PerPane(installed) => panes
                .iter()
                .all(|(_, rect)| installed.iter().any(|(_, cover)| cover.contains(*rect))),
        }
    }
}

/// 单帧铺底与系统材质同步计划（纯计算，便于单测验证所有状态迁移）。
#[derive(Debug, PartialEq)]
struct BackdropStep {
    /// 本帧 Metal 层允许画成半透 `glass_fill` 的区域（其余画实色 `solid_fill`）。
    paint_panes: Panes,
    /// 需要立刻提交给系统的材质方案；`None` 表示本帧不碰系统视图树。
    sync: Option<SystemBackdrop>,
    /// 本帧处于新增或卸载系统材质视图的过渡态，需再跑一帧完成切换。
    needs_next_frame: bool,
}

/// 已经交给系统的材质状态，没变就不再发；macOS 上同时守住异步 Metal 与 AppKit 的两段同步不变量。
#[derive(Default)]
pub(crate) struct Backdrop {
    /// 当前已提交到系统的材质方案（`None` 表示尚未提交过）。
    applied: Option<SystemBackdrop>,
    /// 当前已提交到系统的深浅标记（`None` 表示下次需重同步，如 `invalidate` 后）。
    applied_dark: Option<bool>,
    /// 上一帧 Metal 实际画成半透的区域：在新的实色帧上屏前，不能拆掉或缩小盖着它们的材质视图。
    prev_glass_panes: Panes,
}

impl Backdrop {
    /// 下一帧不管变没变都重发一次：Windows 上系统换深浅时 GPUI 会把窗口的深浅改回跟系统。
    /// 保留 `applied` 记录的已安装视图覆盖范围，避免换深浅时误把正在显示的毛玻璃闪回一帧实色。
    pub(crate) fn invalidate(&mut self) {
        self.applied_dark = None;
    }

    /// 决定这一帧 Metal 铺哪些玻璃区域、是否同步系统材质视图、以及是否需要下一帧收尾。
    ///
    /// 当 `async_backdrop_views` 为真（macOS 上 `CAMetalLayer.presentsWithTransaction == NO`）时，
    /// 维持两条不变量，彻底杜绝任何半透像素后面没有 `NSVisualEffectView` 的空窗帧：
    /// 1. 仅当新方案在几何上仍盖住上一帧已画出的玻璃区时，才提交系统视图树改动（防止先拆视图
    ///    后换 Metal 帧导致透出窗口后的画面）；
    /// 2. 本帧只对「上一帧已就位的系统视图已经盖住」的区域画半透（防止 Metal 先挖透明洞、
    ///    新加的 `NSVisualEffectView` 晚一帧才生效）。
    fn step(
        &mut self,
        materials: LayoutMaterials,
        desired_panes: &Panes,
        bg_opacity: f32,
        dark: bool,
        backdrop_available: bool,
        async_backdrop_views: bool,
    ) -> BackdropStep {
        let desired_system = if !backdrop_available || bg_opacity >= 1.0 {
            SystemBackdrop::None
        } else {
            match material_kind(materials) {
                MaterialKind::None => SystemBackdrop::None,
                MaterialKind::Single(material) => SystemBackdrop::WholeWindow(material),
                MaterialKind::Multiple if desired_panes.is_empty() => SystemBackdrop::None,
                MaterialKind::Multiple => SystemBackdrop::PerPane(desired_panes.clone()),
            }
        };

        if !async_backdrop_views {
            let needs_sync =
                self.applied.as_ref() != Some(&desired_system) || self.applied_dark != Some(dark);
            let sync = needs_sync.then(|| desired_system.clone());
            if needs_sync {
                self.applied = Some(desired_system);
                self.applied_dark = Some(dark);
            }
            self.prev_glass_panes = desired_panes.clone();
            return BackdropStep {
                paint_panes: desired_panes.clone(),
                sync,
                needs_next_frame: false,
            };
        }

        // 不变量 1：上一帧屏幕上仍有半透像素不在新方案覆盖下，则本帧先保留旧系统视图，
        // 仅把新旧共同覆盖的区域画成半透（其余铺实色），等下一帧 Metal 实色上屏后再拆旧视图。
        if !desired_system.covers_geometry(&self.prev_glass_panes) {
            let paint_panes: Panes = desired_panes
                .iter()
                .copied()
                .filter(|pane| {
                    desired_system.covers_geometry(std::slice::from_ref(pane))
                        && self
                            .applied
                            .as_ref()
                            .is_some_and(|cur| cur.covers_geometry(std::slice::from_ref(pane)))
                })
                .collect();
            self.prev_glass_panes = paint_panes.clone();
            return BackdropStep {
                paint_panes,
                sync: None,
                needs_next_frame: true,
            };
        }

        // 不变量 2：本帧只对同步前已经就位的系统视图覆盖到的区域画半透；新装/扩大的区域先铺一帧实色，
        // 同期把系统视图装好，下一帧再揭开半透。单一材质的 WholeWindow 就位后，开合/拖宽/放大都
        // 满足覆盖条件，0 帧延迟、0 次 AppKit 视图树操作。
        let paint_panes: Panes = desired_panes
            .iter()
            .copied()
            .filter(|pane| {
                self.applied
                    .as_ref()
                    .is_some_and(|cur| cur.covers_geometry(std::slice::from_ref(pane)))
            })
            .collect();

        let needs_sync =
            self.applied.as_ref() != Some(&desired_system) || self.applied_dark != Some(dark);
        let sync = needs_sync.then(|| desired_system.clone());
        if needs_sync {
            self.applied = Some(desired_system);
            self.applied_dark = Some(dark);
        }

        let needs_next_frame = paint_panes.len() < desired_panes.len();
        self.prev_glass_panes = paint_panes.clone();
        BackdropStep {
            paint_panes,
            sync,
            needs_next_frame,
        }
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
    let scale = window.scale_factor();
    let desired_panes = if available() && bg_opacity < 1.0 {
        glass_panes(materials, regions, scale)
    } else {
        Vec::new()
    };
    let dark = !ui_theme::is_light();
    let step = backdrop.step(
        materials,
        &desired_panes,
        bg_opacity,
        dark,
        available(),
        cfg!(target_os = "macos"),
    );
    paint_quads(window_bounds, &step.paint_panes, scale, window);
    if let Some(target) = step.sync {
        sync(window, window_bounds, &target, dark);
    }
    if step.needs_next_frame {
        window.request_animation_frame();
    }
}

/// 铺底：透的地方铺半透的主题底色，别处铺实的。返回透的那几块（单测用）。
#[cfg(test)]
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
    paint_quads(window_bounds, &panes, scale, window);
    panes
}

/// 按给定玻璃区域把窗口底分成实色与半透色块铺到 Metal 场景里。
fn paint_quads(window_bounds: Bounds<Pixels>, panes: &Panes, scale: f32, window: &mut Window) {
    let solid = ui_theme::solid_fill();
    let glass = ui_theme::glass_fill();
    let holes: Vec<Rect> = panes.iter().map(|(_, rect)| *rect).collect();
    for rect in subtract_all(Rect::snapped(window_bounds, scale), &holes) {
        window.paint_quad(fill(rect.bounds(), solid));
    }
    for (_, rect) in panes {
        window.paint_quad(fill(rect.bounds(), glass));
    }
}

/// 本帧布局与不透明度实际需要外壳回报坐标的停靠方位：
/// - 平台不支持毛玻璃或 `bg_opacity >= 1.0`（纯实色）时不需要任何区域；
/// - 其余情况只回报声明了材质的方位；若 `center` 声明了材质，还需回报 `bottom` 供 `glass_panes` 扣除。
pub(crate) fn needed_placements(
    materials: LayoutMaterials,
    bg_opacity: f32,
    backdrop_available: bool,
) -> Vec<smelt_nsh::BackdropPlacement> {
    if !backdrop_available || bg_opacity >= 1.0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(4);
    if materials.left.is_some() {
        out.push(smelt_nsh::BackdropPlacement::Left);
    }
    if materials.right.is_some() {
        out.push(smelt_nsh::BackdropPlacement::Right);
    }
    if materials.center.is_some() {
        out.push(smelt_nsh::BackdropPlacement::Center);
    }
    if materials.bottom.is_some() || materials.center.is_some() {
        out.push(smelt_nsh::BackdropPlacement::Bottom);
    }
    out
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

    fn contains(self, inner: Rect) -> bool {
        self.left <= inner.left
            && self.top <= inner.top
            && self.right >= inner.right
            && self.bottom >= inner.bottom
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

/// 把材质交给系统：macOS 上按方案摆好材质视图，Windows 上只有整窗一种，设深浅。
fn sync(window: &Window, window_bounds: Bounds<Pixels>, backdrop: &SystemBackdrop, dark: bool) {
    #[cfg(target_os = "macos")]
    imp::sync(window, window_bounds, backdrop, dark);

    #[cfg(windows)]
    {
        let _ = (window_bounds, backdrop);
        imp::set_dark(window, dark);
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = (window, window_bounds, backdrop, dark);
}

#[cfg(target_os = "macos")]
mod imp {
    use gpui::{Bounds, Pixels, Window};
    use objc::runtime::{BOOL, NO, Object, YES};
    use objc::{class, msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use smelt_plugin_api::Material;

    use super::SystemBackdrop;

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
    /// `NSViewNotSizable`。
    const AUTORESIZE_NONE: usize = 0;
    /// `NSViewWidthSizable (2) | NSViewHeightSizable (16)`：整窗材质视图随 `contentView` 自动缩放。
    const AUTORESIZE_FULL: usize = 2 | 16;

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

    unsafe fn install(
        native_view: *mut Object,
        container: *mut Object,
        frame: NSRect,
        autoresizing_mask: usize,
        material: Material,
        appearance: *mut Object,
    ) -> *mut Object {
        unsafe {
            let view: *mut Object = msg_send![class!(NSVisualEffectView), alloc];
            let view: *mut Object = msg_send![view, initWithFrame: frame];
            assert!(
                !view.is_null(),
                "NSVisualEffectView initialization unexpectedly returned nil"
            );
            let _: () = msg_send![view, setAutoresizingMask: autoresizing_mask];
            let _: () = msg_send![view, setBlendingMode: BLENDING_BEHIND_WINDOW];
            let _: () = msg_send![view, setState: STATE_FOLLOWS_WINDOW];
            let _: () = msg_send![view, setMaterial: ns_material(material)];
            let _: () = msg_send![view, setAppearance: appearance];
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

    /// 按方案把材质视图摆到内容视图底下，多的撤掉。位置换成内容视图的坐标：GPUI 的原点在左上，
    /// AppKit 的不翻转视图原点在左下。
    pub(super) fn sync(
        window: &Window,
        window_bounds: Bounds<Pixels>,
        backdrop: &SystemBackdrop,
        dark: bool,
    ) {
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
            let same_rect = |a: NSRect, b: NSRect| {
                (a.origin.x - b.origin.x).abs() < 0.5
                    && (a.origin.y - b.origin.y).abs() < 0.5
                    && (a.size.width - b.size.width).abs() < 0.5
                    && (a.size.height - b.size.height).abs() < 0.5
            };

            let mut existing = our_views(container);
            match backdrop {
                SystemBackdrop::None => {
                    for extra in existing {
                        let _: () = msg_send![extra, removeFromSuperview];
                    }
                }
                SystemBackdrop::WholeWindow(material) => {
                    if let Some(view) = existing.pop() {
                        let cur: NSRect = msg_send![view, frame];
                        if !same_rect(cur, container_bounds) {
                            let _: () = msg_send![view, setFrame: container_bounds];
                        }
                        let _: () = msg_send![view, setAutoresizingMask: AUTORESIZE_FULL];
                        let _: () = msg_send![view, setMaterial: ns_material(*material)];
                        let _: () = msg_send![view, setAppearance: appearance];
                    } else {
                        install(
                            native_view,
                            container,
                            container_bounds,
                            AUTORESIZE_FULL,
                            *material,
                            appearance,
                        );
                    }
                    for extra in existing {
                        let _: () = msg_send![extra, removeFromSuperview];
                    }
                }
                SystemBackdrop::PerPane(panes) => {
                    // 先按坐标复用已经在目标位置的 NSVisualEffectView，避免跨整窗挪动造成闪烁。
                    let mut targets: Vec<(Material, NSRect)> = Vec::with_capacity(panes.len());
                    for (material, rect) in panes {
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
                        targets.push((*material, frame));
                    }
                    let mut assigned: Vec<Option<*mut Object>> = vec![None; targets.len()];
                    for (slot, (_, frame)) in assigned.iter_mut().zip(&targets) {
                        if let Some(pos) = existing.iter().position(|&view| {
                            let cur: NSRect = msg_send![view, frame];
                            same_rect(cur, *frame)
                        }) {
                            *slot = Some(existing.swap_remove(pos));
                        }
                    }
                    let mut rest = existing.into_iter();
                    for (slot, (material, frame)) in assigned.into_iter().zip(&targets) {
                        match slot {
                            Some(view) => {
                                let _: () = msg_send![view, setAutoresizingMask: AUTORESIZE_NONE];
                                let _: () = msg_send![view, setMaterial: ns_material(*material)];
                                let _: () = msg_send![view, setAppearance: appearance];
                            }
                            None => {
                                if let Some(view) = rest.next() {
                                    let _: () =
                                        msg_send![view, setAutoresizingMask: AUTORESIZE_NONE];
                                    let _: () = msg_send![view, setFrame: *frame];
                                    let _: () =
                                        msg_send![view, setMaterial: ns_material(*material)];
                                    let _: () = msg_send![view, setAppearance: appearance];
                                } else {
                                    install(
                                        native_view,
                                        container,
                                        *frame,
                                        AUTORESIZE_NONE,
                                        *material,
                                        appearance,
                                    );
                                }
                            }
                        }
                    }
                    for extra in rest {
                        let _: () = msg_send![extra, removeFromSuperview];
                    }
                }
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
            let record = |placement| {
                let regions = self.regions.clone();
                gpui::canvas(
                    move |bounds, _, _| regions.borrow_mut().record(placement, bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0()
            };
            let regions = self.regions.clone();
            gpui::div()
                .relative()
                .size_full()
                .flex()
                .flex_row()
                .child(
                    gpui::canvas(move |_, _, _| regions.borrow_mut().begin(), |_, _, _, _| {})
                        .absolute()
                        .size_0(),
                )
                .children(self.left.then(|| {
                    gpui::div()
                        .relative()
                        .w(px(30.))
                        .h_full()
                        .child(record(DockPlacement::Left))
                }))
                .child(
                    gpui::div()
                        .relative()
                        .flex_1()
                        .h_full()
                        .child(record(DockPlacement::Center)),
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

    /// 单一材质布局（如出厂标准布局只有左栏 Sidebar）：整窗常驻一块材质视图，之后开合左侧抽屉、
    /// 拖拽改宽都在 Metal 单帧内原子切换，不增删/移动 AppKit 视图树，杜绝露出 app 后面的画面。
    #[test]
    fn single_material_dock_toggle_never_mutates_system_backdrop() {
        let materials = LayoutMaterials {
            left: Some(Material::Sidebar),
            ..LayoutMaterials::default()
        };
        let left = vec![(Material::Sidebar, rect(0., 0., 312., 800.))];
        let mut backdrop = Backdrop::default();

        // 首帧冷启：先装整窗材质视图，本帧 Metal 铺实色防错帧透底，请求下一帧揭开玻璃。
        let first = backdrop.step(materials, &left, 0.0, true, true, true);
        assert_eq!(
            first,
            BackdropStep {
                paint_panes: Vec::new(),
                sync: Some(SystemBackdrop::WholeWindow(Material::Sidebar)),
                needs_next_frame: true,
            }
        );

        // 第二帧：系统视图已就位，画玻璃，不碰系统视图树。
        let second = backdrop.step(materials, &left, 0.0, true, true, true);
        assert_eq!(
            second,
            BackdropStep {
                paint_panes: left.clone(),
                sync: None,
                needs_next_frame: false,
            }
        );

        // 收起左抽屉：立刻全实色，0 次 AppKit 视图操作，0 额外帧。
        let collapsed = backdrop.step(materials, &Vec::new(), 0.0, true, true, true);
        assert_eq!(
            collapsed,
            BackdropStep {
                paint_panes: Vec::new(),
                sync: None,
                needs_next_frame: false,
            }
        );

        // 重新展开左抽屉：立刻揭开玻璃，0 次 AppKit 视图操作，0 额外帧。
        let expanded = backdrop.step(materials, &left, 0.0, true, true, true);
        assert_eq!(
            expanded,
            BackdropStep {
                paint_panes: left,
                sync: None,
                needs_next_frame: false,
            }
        );

        // 拖拽拉宽左抽屉：立刻按新宽度画玻璃，0 次 AppKit 视图操作。
        let wider = vec![(Material::Sidebar, rect(0., 0., 420., 800.))];
        let resized = backdrop.step(materials, &wider, 0.0, true, true, true);
        assert_eq!(
            resized,
            BackdropStep {
                paint_panes: wider,
                sync: None,
                needs_next_frame: false,
            }
        );
    }

    /// 从带毛玻璃的首页切到纯实色页（如设置页）再切回：卸载延迟到实色帧上屏之后，重新装载时
    /// 先垫实色一帧，双向都不会闪出窗口后的画面。
    #[test]
    fn leaving_glass_page_defers_teardown_until_solid_frame_presents() {
        let workbench = LayoutMaterials {
            left: Some(Material::Sidebar),
            ..LayoutMaterials::default()
        };
        let settings = LayoutMaterials::default();
        let left = vec![(Material::Sidebar, rect(0., 0., 312., 800.))];
        let mut backdrop = Backdrop::default();

        // 稳态首页（左栏透出）。
        let _ = backdrop.step(workbench, &left, 0.0, true, true, true);
        let steady = backdrop.step(workbench, &left, 0.0, true, true, true);
        assert_eq!(steady.paint_panes, left);

        // 切到设置页第 1 帧：Metal 铺全实色，但上一帧屏幕上仍有左栏透明洞，不能立刻拆材质视图。
        let leave_1 = backdrop.step(settings, &Vec::new(), 0.0, true, true, true);
        assert_eq!(
            leave_1,
            BackdropStep {
                paint_panes: Vec::new(),
                sync: None,
                needs_next_frame: true,
            }
        );

        // 切到设置页第 2 帧：实色已上屏，此时再拆掉材质视图。
        let leave_2 = backdrop.step(settings, &Vec::new(), 0.0, true, true, true);
        assert_eq!(
            leave_2,
            BackdropStep {
                paint_panes: Vec::new(),
                sync: Some(SystemBackdrop::None),
                needs_next_frame: false,
            }
        );

        // 回到首页：第 1 帧先装回材质视图并铺实色，第 2 帧揭开毛玻璃。
        let back_1 = backdrop.step(workbench, &left, 0.0, true, true, true);
        assert_eq!(
            back_1,
            BackdropStep {
                paint_panes: Vec::new(),
                sync: Some(SystemBackdrop::WholeWindow(Material::Sidebar)),
                needs_next_frame: true,
            }
        );
        let back_2 = backdrop.step(workbench, &left, 0.0, true, true, true);
        assert_eq!(
            back_2,
            BackdropStep {
                paint_panes: left,
                sync: None,
                needs_next_frame: false,
            }
        );
    }

    /// `invalidate()` 只重同步深浅外观，不丢掉已安装覆盖范围，不会让正在显示的毛玻璃闪一帧实色。
    #[test]
    fn invalidate_resyncs_appearance_without_hiding_glass() {
        let materials = LayoutMaterials {
            left: Some(Material::Sidebar),
            ..LayoutMaterials::default()
        };
        let left = vec![(Material::Sidebar, rect(0., 0., 260., 800.))];
        let mut backdrop = Backdrop::default();

        let _ = backdrop.step(materials, &left, 0.0, true, true, true);
        let _ = backdrop.step(materials, &left, 0.0, true, true, true);

        backdrop.invalidate();
        let after = backdrop.step(materials, &left, 0.0, false, true, true);
        assert_eq!(
            after,
            BackdropStep {
                paint_panes: left,
                sync: Some(SystemBackdrop::WholeWindow(Material::Sidebar)),
                needs_next_frame: false,
            }
        );
    }

    /// 混用多种材质（如左栏 Sidebar + 中央 Window）按区域放置时，增删区域也会守住两段同步不变量。
    #[test]
    fn multi_material_layout_hands_off_safely_without_uncovered_glass() {
        let materials = LayoutMaterials {
            left: Some(Material::Sidebar),
            center: Some(Material::Window),
            ..LayoutMaterials::default()
        };
        let center_only = vec![(Material::Window, rect(30., 0., 80., 60.))];
        let both = vec![
            (Material::Window, rect(30., 0., 80., 60.)),
            (Material::Sidebar, rect(0., 0., 30., 80.)),
        ];
        let mut backdrop = Backdrop::default();

        // 冷启装两块 -> 第二帧全亮。
        let init_1 = backdrop.step(materials, &both, 0.0, true, true, true);
        assert_eq!(init_1.paint_panes, Vec::new());
        assert_eq!(init_1.sync, Some(SystemBackdrop::PerPane(both.clone())));
        assert!(init_1.needs_next_frame);

        let init_2 = backdrop.step(materials, &both, 0.0, true, true, true);
        assert_eq!(init_2.paint_panes, both);
        assert_eq!(init_2.sync, None);
        assert!(!init_2.needs_next_frame);

        // 撤掉左栏（中央框不变）：第 1 帧先保住两块材质视图，仅给中央画玻璃、左栏铺实色。
        let drop_1 = backdrop.step(materials, &center_only, 0.0, true, true, true);
        assert_eq!(drop_1.paint_panes, center_only);
        assert_eq!(drop_1.sync, None);
        assert!(drop_1.needs_next_frame);

        // 第 2 帧：左栏已实色上屏，安全拆掉左栏材质视图。
        let drop_2 = backdrop.step(materials, &center_only, 0.0, true, true, true);
        assert_eq!(drop_2.paint_panes, center_only);
        assert_eq!(
            drop_2.sync,
            Some(SystemBackdrop::PerPane(center_only.clone()))
        );
        assert!(!drop_2.needs_next_frame);

        // 再把左栏加回来：第 1 帧装好两块视图，中央继续透出、新左栏先实色一帧；第 2 帧两块都透。
        let add_1 = backdrop.step(materials, &both, 0.0, true, true, true);
        assert_eq!(add_1.paint_panes, center_only);
        assert_eq!(add_1.sync, Some(SystemBackdrop::PerPane(both.clone())));
        assert!(add_1.needs_next_frame);

        let add_2 = backdrop.step(materials, &both, 0.0, true, true, true);
        assert_eq!(add_2.paint_panes, both);
        assert_eq!(add_2.sync, None);
        assert!(!add_2.needs_next_frame);
    }

    #[test]
    fn needed_placements_only_includes_active_materials() {
        use smelt_nsh::BackdropPlacement as P;

        let sidebar_only = LayoutMaterials {
            left: Some(Material::Sidebar),
            ..LayoutMaterials::default()
        };
        assert_eq!(needed_placements(sidebar_only, 0.85, true), vec![P::Left]);
        assert!(needed_placements(sidebar_only, 1.0, true).is_empty());
        assert!(needed_placements(sidebar_only, 0.85, false).is_empty());
        assert!(needed_placements(LayoutMaterials::default(), 0.5, true).is_empty());

        let center_glass = LayoutMaterials {
            center: Some(Material::Window),
            ..LayoutMaterials::default()
        };
        // center 含材质时需同时回报 bottom 以供 glass_panes 扣除。
        assert_eq!(
            needed_placements(center_glass, 0.7, true),
            vec![P::Center, P::Bottom]
        );
    }
}
