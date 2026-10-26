//! 设置面板：外观 / 模型 / 会话与 Agent / 协作 / 插件 / 维护 / 快捷键，含独立
//! 设置窗口的渲染逻辑。左侧是分类和子菜单，每个子菜单对应独立内容页。
//!
//! 跟 git_panel / file_tree 同一个套路：从 main.rs 拆出来的 `impl Workspace`
//! 方法 + 独立类型/函数，字段仍然声明在 main.rs 的 `Workspace` struct 里。
//!
//! 自动更新（`update_status`/`check_for_update`/`request_daemon_restart` 等）**不在
//! 这里**——那是应用级生命周期状态，不属于任何一个面板，仍留在 main.rs；这里的
//! 「维护」SettingPage 只是读它、展示它、提供按钮触发它。

use std::time::{Duration, Instant};

use gpui::InteractiveElement;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
#[cfg(target_os = "macos")]
use gpui_component::progress::Progress;
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings};
use gpui_component::slider::{Slider, SliderEvent, SliderState, SliderValue};
#[cfg(target_os = "macos")]
use gpui_component::switch::Switch;
use gpui_component::*;
pub(crate) use smelt_ui::Button;

#[cfg(target_os = "macos")]
use crate::updater;
use crate::{Workspace, liquid_glass, terminal, terminal_view};

mod agent_hooks;
mod bundled_helpers;
mod memory;
mod nav;
mod panel_plugins;
mod pi_auth;
mod pi_config;
mod pi_mcp;
mod remote;
mod workspace;
mod workspace_view;

pub(crate) use nav::{
    SettingsCategoryId, SettingsNavCategory, SettingsSection, filter_settings_nav, settings_nav,
};
pub(super) use remote::*;

/// 「反馈问题」入口统一跳转的链接。应用菜单 / 侧栏用户菜单 / 设置页三处共用。
/// 打包时设 `SMELT_FEEDBACK_URL` 换成自己的反馈渠道。
pub const FEEDBACK_URL: &str = build_time_url(
    option_env!("SMELT_FEEDBACK_URL"),
    "https://github.com/smelt-ai/smelt/issues/new/choose",
);

/// 「帮助文档」入口的链接，侧栏用户菜单和设置页共用。打包时设 `SMELT_DOCS_URL` 换掉。
pub const DOCS_URL: &str = build_time_url(option_env!("SMELT_DOCS_URL"), "https://smelt.onoo.io/");

/// 打包时没设或设成空串，都用开源版的默认地址。
const fn build_time_url(value: Option<&'static str>, default: &'static str) -> &'static str {
    match value {
        Some(url) if !url.is_empty() => url,
        _ => default,
    }
}

#[cfg(test)]
pub(super) use remote::qr_png_for_url;

pub(super) use agent_hooks::{
    AgentHooksCache, install_agent_hooks, reconcile_agent_hooks, refresh_agent_hooks_status,
    uninstall_agent_hooks,
};
pub(super) use bundled_helpers::sync_bundled_smelt_notify;
pub(crate) use pi_config::{PiConfigSlot, PiLibrary, pi_model_choice_groups};
pub(crate) use panel_plugins::{PanelPluginsSnapshot, PanelPluginsState};

// ===================== 外观 / 启动 配置类型 =====================

fn default_theme_mode() -> ThemeMode {
    ThemeMode::Dark
}

fn default_theme_name() -> String {
    smelt_model::theme_index::DEFAULT_DARK.to_string()
}

/// 界面字号（基准 rem，px）默认值与取值范围。
pub const DEFAULT_UI_FONT_PX: u32 = 16;
pub const MIN_UI_FONT_PX: u32 = 14;
pub const MAX_UI_FONT_PX: u32 = 26;

fn default_ui_font_px() -> u32 {
    DEFAULT_UI_FONT_PX
}

/// 老版本 appearance.json 没有 font_px 字段时的回退，跟 terminal_view::FONT_PX_ATOM
/// 的出厂默认值保持一致。
fn default_font_px() -> u32 {
    13
}

/// `bg_color` 从未被用户改过时的出厂值——终端背景层要不要跟着主题模式自动换色，
/// 就看当前值是不是还等于这个（见 `Appearance::bg_color_is_default`）。
const DEFAULT_BG_COLOR: u32 = 0x1a1b26;

/// 背景图出厂透明度：25% 是业内常用起点（WezTerm/Windows Terminal 的
/// backgroundImageOpacity 0.15–0.35 区间），既保留壁纸观感又不压过文字。
fn default_bg_image_opacity() -> f32 {
    0.25
}

/// 窗口与终端外观设置（全局单例；存 SQLite KV）。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Appearance {
    /// 终端底色（0xRRGGBB）。
    pub bg_color: u32,
    /// 背景图片绝对路径（None = 无）。
    pub bg_image: Option<String>,
    /// 背景图片透明度 0–1（0=全透，1=全不透明）。默认低透明度做装饰层，
    /// 让主题底色和窗口材质透出，避免明亮图片直接铺满破坏可读性（对齐
    /// WezTerm / Windows Terminal 的 backgroundImageOpacity 惯例）。
    #[serde(default = "default_bg_image_opacity")]
    pub bg_image_opacity: f32,
    /// 不透明度 0.3–1.0；应用到整扇原生窗口，后方应用会参与系统合成。
    pub opacity: f32,
    /// 旧配置兼容字段；支持时使用液态玻璃，旧系统继续使用毛玻璃。
    pub blur: bool,
    /// 液态玻璃材质档位（macOS 26+；旧系统忽略）。老配置文件缺省 = 标准。
    #[serde(default)]
    pub glass_style: liquid_glass::GlassStyle,
    /// 明暗。跟着 `theme_name` 走，CLI 的 `appearance.theme_mode` 会把主题拨到同一族的另一侧。
    #[serde(default = "default_theme_mode")]
    pub theme_mode: ThemeMode,
    /// 主题名，对应注册表里的 `name`，例如 `Default Dark`、`Ayu Light`。
    #[serde(default = "default_theme_name")]
    pub theme_name: String,
    /// 界面字号（基准 rem，px）。控制侧边栏、Agent 会话、设置页等全局 UI 元素的排版缩放。
    #[serde(default = "default_ui_font_px")]
    pub ui_font_px: u32,
    /// 界面字体族。空 = 系统 UI 字体（GPUI `.SystemUIFont`，macOS 上是 SF Pro + PingFang）。
    #[serde(default)]
    pub ui_font_family: String,
    /// 终端字号（px）。控制终端缓冲区与命令行输出文本字号。
    #[serde(default = "default_font_px")]
    pub font_px: u32,
    /// 代码/终端等宽字体族。空 = 出厂默认（terminal_view::DEFAULT_FONT_FAMILY）；
    /// 填了但机器上没装时，渲染/测量会落到内嵌默认再落到 Menlo（见 terminal_view::terminal_font）。
    /// 只作用于终端网格、diff、代码块；侧栏和按钮走 `ui_font_family`。
    #[serde(default)]
    pub font_family: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            bg_color: DEFAULT_BG_COLOR,
            bg_image: None,
            bg_image_opacity: default_bg_image_opacity(),
            // 95% keeps the native Liquid Glass visible without making text and controls
            // look washed out. Existing user-configured values are preserved on load.
            opacity: 0.95,
            blur: true,
            glass_style: liquid_glass::GlassStyle::Regular,
            theme_mode: ThemeMode::Dark,
            theme_name: default_theme_name(),
            ui_font_px: default_ui_font_px(),
            ui_font_family: String::new(),
            font_px: default_font_px(),
            font_family: String::new(),
        }
    }
}

impl Global for Appearance {}

impl Appearance {
    /// 持久化配置可能被手动改坏；所有窗口都使用同一份合法的原生窗口透明度。
    pub fn window_opacity(&self) -> f32 {
        if self.opacity.is_finite() {
            self.opacity.clamp(0.3, 1.0)
        } else {
            1.0
        }
    }

    /// 整窗继续使用系统优化过的传统毛玻璃。Liquid Glass 仅用于顶部的稳定导航区域；
    /// 若把它铺在终端/ACP 这些高频更新内容下，会触发全帧背景重采样并造成掉帧。
    pub fn window_bg(&self) -> WindowBackgroundAppearance {
        WindowBackgroundAppearance::Blurred
    }

    /// `bg_color` 是否还是没被用户碰过的出厂值。是的话终端背景层该跟主题模式自动
    /// 切换（见 terminal_view.rs 的 bg_layer）；用户显式选过颜色后就不再跟随，
    /// 保留其选择（深浅色模式来回切也不丢）。
    pub fn bg_color_is_default(&self) -> bool {
        self.bg_color == DEFAULT_BG_COLOR
    }
}

/// 通过 AppKit 的 `alphaValue` 改整扇原生窗口的合成透明度。和元素 `.opacity()` 不同，
/// 这也会作用于 GPUI 内容下方的原生液态玻璃/毛玻璃，因此能实际看见窗口后的应用。
#[cfg(target_os = "macos")]
pub fn apply_window_opacity(window: &Window, opacity: f32) {
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = HasWindowHandle::window_handle(window)
        .expect("GPUI window must expose a native window handle");
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        unreachable!("macOS GPUI windows must use AppKit window handles");
    };
    let native_view = handle.ns_view.as_ptr().cast::<Object>();

    unsafe {
        let native_window: *mut Object = msg_send![native_view, window];
        if !native_window.is_null() {
            let _: () = msg_send![native_window, setAlphaValue: opacity as f64];
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn apply_window_opacity(_window: &Window, _opacity: f32) {}

/// 在线更新配置（全局单例，存 SQLite KV）。
///
/// 通道只决定更新源给哪一路包（GitHub 上认不认预发布，或打包时指定的哪份清单）；
/// 更新流程本身仍由 updater 统一处理。只在 macOS 上存在（设计文档 D4）。
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UpdateSettings {
    #[serde(default)]
    pub channel: updater::UpdateChannel,
    /// 发现新版本后是否自动下载并装好。关掉只影响"要不要自动下载"这一步，
    /// 检查照常做——否则用户既不会自动更新也看不到有新版，等于彻底静默。
    #[serde(default = "default_auto_install")]
    pub auto_install: bool,
}

#[cfg(target_os = "macos")]
fn default_auto_install() -> bool {
    true
}

#[cfg(target_os = "macos")]
impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            channel: updater::UpdateChannel::default(),
            auto_install: default_auto_install(),
        }
    }
}

#[cfg(target_os = "macos")]
impl Global for UpdateSettings {}

/// 枚举在客户端库里存它的 serde 名（`dev`、`dark`、`session`……），和协议里的写法一致。
pub(crate) fn enum_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// 认不出的名字（库是别的版本写的）返回 `None`，由调用方用出厂值。
pub(crate) fn enum_from_name<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(name.to_string())).ok()
}

/// 更新设置归客户端（每台设备各自管自己的更新），存在客户端库。
#[cfg(target_os = "macos")]
pub fn load_update_settings() -> UpdateSettings {
    let record = match smelt_client_store::client_store().and_then(|store| store.update_settings())
    {
        Ok(record) => record,
        Err(error) => {
            eprintln!("[storage] 读取更新设置失败，使用默认值: {error}");
            None
        }
    };
    let Some(record) = record else {
        return UpdateSettings::default();
    };
    UpdateSettings {
        channel: enum_from_name(&record.channel).unwrap_or_default(),
        auto_install: record.auto_install,
    }
}

#[cfg(target_os = "macos")]
pub fn save_update_settings(settings: &UpdateSettings) {
    let record = smelt_client_store::UpdateSettingsRecord {
        channel: enum_name(&settings.channel),
        auto_install: settings.auto_install,
    };
    if let Err(error) =
        smelt_client_store::client_store().and_then(|store| store.put_update_settings(&record))
    {
        eprintln!("[storage] 保存更新设置失败: {error}");
    }
}

#[cfg(target_os = "macos")]
pub fn apply_update_channel(channel: updater::UpdateChannel, cx: &mut App) {
    let mut settings = *cx.global::<UpdateSettings>();
    settings.channel = channel;
    save_update_settings(&settings);
    cx.set_global(settings);
}

#[cfg(target_os = "macos")]
pub fn apply_auto_install(auto_install: bool, cx: &mut App) {
    let mut settings = *cx.global::<UpdateSettings>();
    settings.auto_install = auto_install;
    save_update_settings(&settings);
    cx.set_global(settings);
}

/// CLI / Control API 经运行时转来的外观改动：按可脚本目录的规则套到当前外观上，
/// 存进客户端库并整份重新安装。规则在运行时那边已经校验过一遍，这里再走同一份
/// 校验，是因为「主题明暗」要按本机当前的主题找同族的另一侧。
pub fn apply_requested_appearance(
    settings: &std::collections::BTreeMap<String, serde_json::Value>,
    cx: &mut App,
) {
    let current = cx.global::<Appearance>().clone();
    let mut snapshot = snapshot_from_appearance(&current);
    if let Err(error) = smelt_model::appearance_settings::apply_patch(&mut snapshot, settings) {
        crate::status_item::notify_error(format!("外观修改未生效：{error}"));
        return;
    }
    let next = appearance_from_snapshot(snapshot);
    save_appearance(&next);
    install_appearance(next, cx);
}

/// 把一份完整外观套到所有层并重绘。外观的真源是客户端库，内存里的全局只是它的投影。
fn install_appearance(appearance: Appearance, cx: &mut App) {
    terminal_view::set_font_px(appearance.font_px);
    terminal_view::set_font_family(&appearance.font_family);
    cx.set_global(appearance.clone());
    apply_named_theme(&appearance.theme_name, cx);
    apply_bg_color(&appearance);
    cx.refresh_windows();
}

/// 按主题名装上配色。组件库走 `apply_config`，自绘色板从这份主题投影，终端跟着明暗切。
/// 字号和字体在装完之后写回 Appearance，主题文件里的字号不覆盖用户设置。
/// 只改全局态不重绘，调用方自己决定什么时候 `cx.refresh_windows()`。
pub fn apply_named_theme(name: &str, cx: &mut App) {
    let applied = smelt_ui::theme_catalog::apply(name, cx);
    let dark = if applied && cx.has_global::<Theme>() {
        cx.global::<Theme>().is_dark()
    } else {
        cx.try_global::<Appearance>()
            .map(|appearance| appearance.theme_mode.is_dark())
            .unwrap_or(true)
    };
    if !applied {
        crate::ui_theme::set_light(!dark);
    }
    terminal::set_dark_mode(dark);
    sync_theme_fonts(cx);
    publish_terminal_theme();
}

/// GPUI 系统界面字体名：组件库 Theme 默认值，空配置时回落到它。
const SYSTEM_UI_FONT: &str = ".SystemUIFont";

/// 当前生效的界面字体。空配置 = 系统 UI 字体。
pub fn resolved_ui_font_family(a: &Appearance) -> SharedString {
    let name = a.ui_font_family.trim();
    if name.is_empty() {
        SYSTEM_UI_FONT.into()
    } else {
        name.to_string().into()
    }
}

/// 把 Appearance 的界面字号/字体、代码字体写进 gpui-component Theme。
/// Root 每帧用 Theme.font_size 设 rem；部件正文用 Theme.font_family；
/// TextView 代码块用 Theme.mono_font_family。
fn sync_theme_fonts(cx: &mut App) {
    let (ui_font_px, ui_family) = cx
        .try_global::<Appearance>()
        .map(|a| (a.ui_font_px, resolved_ui_font_family(a)))
        .unwrap_or((DEFAULT_UI_FONT_PX, SYSTEM_UI_FONT.into()));
    if !cx.has_global::<Theme>() {
        return;
    }
    // Root 每帧用 Theme.font_size 设 rem。update 结尾的 sync_base 用当前主题
    // 重写分栏手柄和滚动条，这里不再另涂一层。
    Theme::update(cx, |theme| {
        theme.font_size = px(ui_font_px as f32);
        theme.font_family = ui_family;
        theme.mono_font_family = terminal_view::font_family().into();
    });
}

/// 把「当前生效的终端配色」发给守护：它拿来应答 OSC 颜色查询，以后也由它转给移动端。
///
/// 移动端渲染的是同一份 PTY 字节流，而 TUI 会用 OSC 11 查背景色来决定用哪档灰
/// （应答见 `terminal::EventProxy::resolve_color`）。手机若用自己写死的配色，
/// TUI 以为的底色跟实际底色对不上，就是对比度问题。所以颜色真源只有 PC 这一份，
/// 而且一部手机可以连多台设备，配色必须**按设备**下发而不是客户端内置。
///
/// 主题、用户自选底色变化时都要调（见 `apply_named_theme` / `apply_appearance`）。
///
/// 交给运行时落盘，在后台按顺序发：主题切换时会连着调好几次，只有最后一份要紧。
pub fn publish_terminal_theme() {
    static PUBLISHER: std::sync::OnceLock<
        smol::channel::Sender<smelt_model::terminal_theme::TerminalThemeSnapshot>,
    > = std::sync::OnceLock::new();
    let publisher = PUBLISHER.get_or_init(|| {
        let (tx, rx) =
            smol::channel::unbounded::<smelt_model::terminal_theme::TerminalThemeSnapshot>();
        std::thread::spawn(move || {
            while let Ok(mut theme) = rx.recv_blocking() {
                // 积压的旧配色直接跳过。
                while let Ok(newer) = rx.try_recv() {
                    theme = newer;
                }
                if let Err(error) = crate::runtime_call::call::<
                    smelt_model::control_api::TerminalThemePublish,
                >(
                    theme, crate::runtime_call::RUNTIME_CALL_TIMEOUT
                ) {
                    eprintln!(
                        "[theme] 发布终端配色失败：{}",
                        crate::runtime_call::error_text(error)
                    );
                }
            }
        });
        tx
    });
    let _ = publisher.try_send(current_terminal_theme());
}

/// 按当前主题 + 外观设置组装配色快照。
fn current_terminal_theme() -> smelt_model::terminal_theme::TerminalThemeSnapshot {
    smelt_model::terminal_theme::TerminalThemeSnapshot {
        version: smelt_model::terminal_theme::TERMINAL_THEME_VERSION,
        dark: terminal::is_dark(),
        background: terminal::default_bg(),
        foreground: terminal::default_fg(),
        // 光标是实心块反显（见 terminal_view 的 in_block 分支），块本身就是前景色。
        cursor: terminal::default_fg(),
        selection: terminal_view::sel_bg(),
        palette: terminal::ansi_palette().to_vec(),
        search_hit: terminal_view::search_hit_bg(false),
        search_hit_current: terminal_view::search_hit_bg(true),
    }
}

/// 读取外观设置；缺失/损坏回退默认。外观归客户端，存在客户端库。
pub fn load_appearance() -> Appearance {
    match smelt_client_store::client_store().and_then(|store| store.appearance()) {
        Ok(Some(record)) => {
            appearance_from_snapshot(smelt_model::appearance_settings::AppearanceSnapshot {
                bg_color: record.bg_color,
                bg_image: record.bg_image,
                bg_image_opacity: record.bg_image_opacity,
                opacity: record.opacity,
                blur: record.blur,
                glass_style: record.glass_style,
                theme_mode: record.theme_mode,
                theme_name: record.theme_name,
                ui_font_px: record.ui_font_px,
                ui_font_family: record.ui_font_family,
                font_px: record.font_px,
                font_family: record.font_family,
            })
        }
        Ok(None) => Appearance::default(),
        Err(error) => {
            eprintln!("[storage] 读取外观设置失败，使用默认值: {error}");
            Appearance::default()
        }
    }
}

fn save_appearance(appearance: &Appearance) {
    let snapshot = snapshot_from_appearance(appearance);
    let record = smelt_client_store::AppearanceRecord {
        bg_color: snapshot.bg_color,
        bg_image: snapshot.bg_image,
        bg_image_opacity: snapshot.bg_image_opacity,
        opacity: snapshot.opacity,
        blur: snapshot.blur,
        glass_style: snapshot.glass_style,
        theme_mode: snapshot.theme_mode,
        theme_name: snapshot.theme_name,
        ui_font_px: snapshot.ui_font_px,
        ui_font_family: snapshot.ui_font_family,
        font_px: snapshot.font_px,
        font_family: snapshot.font_family,
    };
    if let Err(error) =
        smelt_client_store::client_store().and_then(|store| store.put_appearance(&record))
    {
        eprintln!("[storage] 保存外观设置失败: {error}");
    }
}

fn snapshot_from_appearance(
    appearance: &Appearance,
) -> smelt_model::appearance_settings::AppearanceSnapshot {
    smelt_model::appearance_settings::AppearanceSnapshot {
        bg_color: appearance.bg_color,
        bg_image: appearance.bg_image.clone(),
        bg_image_opacity: appearance.bg_image_opacity,
        opacity: appearance.opacity,
        blur: appearance.blur,
        glass_style: match appearance.glass_style {
            liquid_glass::GlassStyle::Regular => "regular".into(),
            liquid_glass::GlassStyle::Clear => "clear".into(),
        },
        theme_mode: serde_json::to_value(appearance.theme_mode)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "dark".into()),
        theme_name: if appearance.theme_name.trim().is_empty() {
            default_theme_name()
        } else {
            appearance.theme_name.clone()
        },
        ui_font_px: appearance.ui_font_px,
        ui_font_family: appearance.ui_font_family.clone(),
        font_px: appearance.font_px,
        font_family: appearance.font_family.clone(),
    }
}

fn appearance_from_snapshot(
    snapshot: smelt_model::appearance_settings::AppearanceSnapshot,
) -> Appearance {
    Appearance {
        bg_color: snapshot.bg_color,
        bg_image: snapshot.bg_image,
        bg_image_opacity: snapshot.bg_image_opacity,
        opacity: snapshot.opacity,
        blur: snapshot.blur,
        glass_style: match snapshot.glass_style.as_str() {
            "clear" => liquid_glass::GlassStyle::Clear,
            _ => liquid_glass::GlassStyle::Regular,
        },
        theme_mode: serde_json::from_value(serde_json::Value::String(snapshot.theme_mode.clone()))
            .unwrap_or_else(|_| default_theme_mode()),
        theme_name: if snapshot.theme_name.trim().is_empty() {
            default_theme_name()
        } else {
            snapshot.theme_name
        },
        ui_font_px: snapshot.ui_font_px,
        ui_font_family: snapshot.ui_font_family,
        font_px: snapshot.font_px,
        font_family: snapshot.font_family,
    }
}

/// 项目行「+」下拉菜单里的一条可配置启动项，以及它的出厂默认集合。
/// 定义在 `smelt-core`：桌面新建菜单和移动端读的是同一份启动项目录。
pub use smelt_model::new_session::{LaunchEntry, default_launch_entries};

/// 删除自定义启动项。内置项是固定集合，数据层也拒绝删除，不能只依赖设置页
/// 隐藏按钮。
fn remove_launch_entry_at(config: &mut LaunchConfig, index: usize) -> bool {
    let Some(entry) = config.entries.get(index) else {
        return false;
    };
    if smelt_model::launch_settings::launch_entry_is_builtin(entry) {
        return false;
    }
    config.entries.remove(index);
    true
}

/// 项目行「+」可配置启动项：运行时那份清单的缓存（全局单例）。窗口先起来，后台
/// 加载；没加载好之前显示出厂默认，不接受修改。
#[derive(Clone)]
pub struct LaunchConfig {
    /// 除固定的「新建终端」外，下拉菜单里的启动项。
    pub entries: Vec<LaunchEntry>,
    /// 是否已从运行时加载。
    pub loaded: bool,
}

impl Default for LaunchConfig {
    fn default() -> Self {
        Self {
            entries: default_launch_entries(),
            loaded: false,
        }
    }
}

/// 各 agent 在图标类界面（「+」下拉菜单等）的统一图标：`icon_for_launch_command`
/// （快捷终端）等共用这份映射，别各自维护一遍。
pub fn icon_for_agent_kind(agent: TerminalAgentKind) -> IconName {
    match agent {
        TerminalAgentKind::Claude => IconName::Asterisk,
        TerminalAgentKind::Codex => IconName::Bot,
        TerminalAgentKind::Copilot => IconName::Github,
        TerminalAgentKind::Grok
        | TerminalAgentKind::Antigravity
        | TerminalAgentKind::Cursor
        | TerminalAgentKind::OpenCode
        | TerminalAgentKind::Kiro
        | TerminalAgentKind::Pi
        | TerminalAgentKind::Crush => IconName::Bot,
    }
}

/// 按命令前缀猜侧栏/菜单图标（自定义 agent 走通用终端图标）。判断本体挪进
/// `TerminalAgentKind::from_command_prefix`——跟 `terminal_view::classify_launch`
/// 共用同一份「这行命令是哪家 agent」逻辑。
pub fn icon_for_launch_command(command: &str) -> IconName {
    TerminalAgentKind::from_command_prefix(command)
        .map(icon_for_agent_kind)
        .unwrap_or(IconName::SquareTerminal)
}

/// 过滤出可展示的启动项（名/命令非空）。
pub fn active_launch_entries(cx: &App) -> Vec<LaunchEntry> {
    cx.global::<LaunchConfig>()
        .entries
        .iter()
        .filter(|e| !e.label.trim().is_empty() && !e.command.trim().is_empty())
        .cloned()
        .collect()
}

impl Global for LaunchConfig {}

fn fetch_launch_settings() -> Result<smelt_model::launch_settings::LaunchSettings, String> {
    crate::runtime_call::call::<smelt_model::control_api::LaunchGet>(
        smelt_runtime_client::protocol::control::EmptyParams::default(),
        crate::runtime_call::RUNTIME_CALL_TIMEOUT,
    )
    .map_err(crate::runtime_call::error_text)
}

fn set_launch_entries(entries: Vec<LaunchEntry>, cx: &mut App) {
    cx.set_global(LaunchConfig {
        entries,
        loaded: true,
    });
}

/// 后台从本机运行时加载启动项：等它在线再读，读失败就退避后回去等（见
/// [`crate::runtime_events::wait_online`]），直到拿到为止。
pub fn start_launch_config_load(cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut failures = 0u32;
        loop {
            if !crate::runtime_events::wait_online(&crate::RuntimeKey::Local, failures, cx).await {
                return;
            }
            let fetched = cx
                .background_executor()
                .spawn(async { fetch_launch_settings() })
                .await;
            if let Ok(settings) = fetched {
                cx.update(|cx| set_launch_entries(settings.entries, cx));
                return;
            }
            failures = failures.saturating_add(1);
        }
    })
    .detach();
}

/// 整表替换按提交顺序逐个发给运行时：每次的 `base` 都是上一次提交后的本地状态，
/// 并发发出去就会互相判成冲突。
struct LaunchEditQueue {
    tx: smol::channel::Sender<(Vec<LaunchEntry>, Vec<LaunchEntry>)>,
}

impl Global for LaunchEditQueue {}

fn submit_launch_entries(base: Vec<LaunchEntry>, entries: Vec<LaunchEntry>, cx: &mut App) {
    if !cx.has_global::<LaunchEditQueue>() {
        let (tx, rx) = smol::channel::unbounded::<(Vec<LaunchEntry>, Vec<LaunchEntry>)>();
        cx.set_global(LaunchEditQueue { tx });
        cx.spawn(async move |cx| {
            while let Ok((base, entries)) = rx.recv().await {
                let replaced = cx
                    .background_executor()
                    .spawn(async move {
                        crate::runtime_call::call::<smelt_model::control_api::LaunchReplace>(
                            smelt_model::control_api::LaunchReplaceParams { base, entries },
                            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                        )
                    })
                    .await;
                match replaced {
                    // 后面还有排着的修改时，这一包不是最终状态，先不覆盖本地。
                    Ok(settings) => {
                        if rx.is_empty() {
                            cx.update(|cx| set_launch_entries(settings.entries, cx));
                        }
                    }
                    Err(error) => {
                        // 冲突或失败都以运行时为准：丢掉排着的修改（它们的 base 已经
                        // 不成立），重新拉一份。
                        while rx.try_recv().is_ok() {}
                        crate::status_item::notify_error(crate::runtime_call::error_text(error));
                        let fresh = cx
                            .background_executor()
                            .spawn(async { fetch_launch_settings() })
                            .await;
                        if let Ok(settings) = fresh {
                            cx.update(|cx| set_launch_entries(settings.entries, cx));
                        }
                    }
                }
            }
        })
        .detach();
    }
    let _ = cx.global::<LaunchEditQueue>().tx.try_send((base, entries));
}

/// 改启动项：本地立刻生效，整表按序提交给运行时。还没从运行时加载时不接受修改。
fn apply_launch_config(f: impl FnOnce(&mut LaunchConfig), cx: &mut App) {
    let current = cx.global::<LaunchConfig>().clone();
    if !current.loaded {
        crate::status_item::notify_error("正在读取运行时的启动项，请稍后再改");
        return;
    }
    let mut next = current.clone();
    f(&mut next);
    if next.entries == current.entries {
        return;
    }
    cx.set_global(next.clone());
    submit_launch_entries(current.entries, next.entries, cx);
}

// ===================== Agent 身份与宿主状态 =====================
//
// ConversationAgentKind / AcpProfile 在 smelt-model（本身不需要 GPUI），AgentHostState
// 和按机器的 AgentHosts（需要 `gpui::Global`）在 smelt-ui——都是 acp_view.rs 独立成
// smelt-acp-view crate 之后要跨 crate 共用的数据模型。这里重导出成裸名字。
//
// 智能体配置（启动命令、env、profile、定义）归运行时，每台机器一份：读、改都要说明是
// 哪台机器。会话按它所在的运行时取，设置页眼下只配本机。
pub use smelt_model::agent_host::AgentHostSettings;
use smelt_model::agent_kind::ConversationLaunchSpec;
pub use smelt_model::agent_kind::{
    AcpProfile, ConversationAgentKind, HistorySourceKind, TerminalAgentKind,
};
pub use smelt_ui::agent_host_state::{
    AgentDefinition, AgentHostState, AgentHosts, agent_host_loaded, agent_host_settled,
    agent_hosts, apply_agent_host, apply_notify_prefs, detach_agent_host, initial_agent_host_state,
    reload_agent_hosts, try_apply_agent_host,
};
pub use smelt_ui::automation::{
    Automation, AutomationAction, AutomationActionKindId, AutomationNotificationPreset,
    AutomationSchedule, AutomationSink, AutomationTrigger, AutomationTriggerKindId,
    AutomationTriggerPresetId, BUILTIN_AUTOMATION_ACTIONS, BUILTIN_AUTOMATION_TRIGGER_PRESETS,
    EventIngress, WebhookIngress,
};

/// 挂上一台机器并在后台读它的配置：等那台机器在线再读，读失败就退避后回去等（见
/// [`crate::runtime_events::wait_online`]），直到读到或机器被移除。已经挂着的只换客户端
/// （比如改了中继），不重读。`on_first_load` 在第一次读到后调用一次。
pub fn attach_agent_host(
    key: crate::RuntimeKey,
    client: smelt_runtime_client::RuntimeClient,
    cx: &mut App,
    on_first_load: impl FnOnce(&mut App) + 'static,
) {
    use smelt_ui::agent_host_state::{AgentHostLoad, load_agent_host, register_agent_host};

    if !register_agent_host(&key, client, cx) {
        return;
    }
    cx.spawn(async move |cx| {
        let mut failures = 0u32;
        loop {
            if !crate::runtime_events::wait_online(&key, failures, cx).await {
                return;
            }
            // 刚断线、订阅还没察觉时会读失败，先安静重试几次再报。
            match load_agent_host(&key, failures >= 4, cx).await {
                AgentHostLoad::Loaded => {
                    cx.update(on_first_load);
                    return;
                }
                AgentHostLoad::Detached => return,
                AgentHostLoad::Failed => failures = failures.saturating_add(1),
            }
        }
    })
    .detach();
}

/// 那台机器的智能体配置；还没读到（或机器已移除）时用出厂值。
pub fn agent_host_settings<'a>(
    runtime: &crate::RuntimeKey,
    cx: &'a App,
) -> std::borrow::Cow<'a, AgentHostSettings> {
    match agent_hosts(cx).and_then(|hosts| hosts.settings(runtime)) {
        Some(settings) => std::borrow::Cow::Borrowed(settings),
        None => std::borrow::Cow::Owned(AgentHostSettings::fresh()),
    }
}

/// 那台机器上的全部智能体定义；没读到是空的。
pub fn agents_on<'a>(runtime: &crate::RuntimeKey, cx: &'a App) -> &'a [AgentDefinition] {
    agent_hosts(cx)
        .map(|hosts| hosts.agents(runtime))
        .unwrap_or_default()
}

/// 那台机器上的智能体定义。
pub fn agent_definition(
    runtime: &crate::RuntimeKey,
    id: &str,
    cx: &App,
) -> Option<AgentDefinition> {
    agent_hosts(cx)?.agent(runtime, id).cloned()
}

/// 那台机器上某个 agent 的启动命令。
pub fn acp_cmd_for(runtime: &crate::RuntimeKey, agent: ConversationAgentKind, cx: &App) -> String {
    agent_host_settings(runtime, cx).acp_cmd_for(agent)
}

/// 那台机器上某个 agent 的完整启动规格（命令 + 用户配的环境变量）。
///
/// 新建会话一律走这里。只取 `acp_cmd_for` 再自己 `from_command` 会把环境变量
/// 丢掉，表现是"设置里填了 API key 却还是说没配"——而且只在真发一轮时才暴露。
pub fn acp_launch_for(
    runtime: &crate::RuntimeKey,
    agent: ConversationAgentKind,
    cx: &App,
) -> ConversationLaunchSpec {
    agent_host_settings(runtime, cx).acp_launch_for(agent)
}

/// 普通 ACP 对话的初始配置：Agent 级最近选择打底，会话自身存档逐项覆盖。
/// 插件自动任务不应调用这里，避免交互偏好污染任务运行参数。
pub fn initial_acp_config_for(
    runtime: &crate::RuntimeKey,
    agent: ConversationAgentKind,
    session_values: &[(String, String)],
    cx: &App,
) -> Vec<(String, String)> {
    agent_host_settings(runtime, cx).initial_acp_config(agent, session_values)
}

/// 智能体页新开的产品对话继承模型等交互偏好，但权限固定为该引擎注册的全权限
/// 模式。普通 ACP 对话和已有会话恢复继续走 `initial_acp_config_for`。
pub fn initial_agent_conversation_config_for(
    runtime: &crate::RuntimeKey,
    agent: ConversationAgentKind,
    cx: &App,
) -> Vec<(String, String)> {
    agent_host_settings(runtime, cx).initial_agent_conversation_config(agent)
}

/// 设置页只在用户覆盖内置适配器时显示原始命令；默认 `bunx`、CLI 参数等属于
/// smelt 的实现细节，不应要求用户理解或维护。
fn acp_cmd_setting_value(agent: ConversationAgentKind, command: String) -> SharedString {
    if command == agent.default_cmd() {
        SharedString::default()
    } else {
        command.into()
    }
}

/// 设置页「Agent 集成」里每个 agent 一条自定义启动命令输入框（从枚举派生，加
/// 一家 agent 不用回来抄第四遍）。
fn acp_cmd_setting_item(agent: ConversationAgentKind) -> SettingItem {
    SettingItem::new(
        format!("{} 自定义启动命令", agent.label()),
        SettingField::input(
            move |cx: &App| {
                acp_cmd_setting_value(agent, acp_cmd_for(&crate::RuntimeKey::Local, agent, cx))
            },
            move |v: SharedString, cx: &mut App| {
                let v = v.trim().to_string();
                // 留空 = 使用内置适配器（不是清成空串跑不起来）。
                let cmd = if v.is_empty() { agent.default_cmd() } else { v };
                apply_agent_host(
                    &crate::RuntimeKey::Local,
                    move |c| c.set_acp_cmd_for(agent, cmd),
                    cx,
                );
            },
        ),
    )
    .description(format!(
        "留空使用内置适配器；仅在需要替换适配器或追加参数时填写。\
         改动只影响之后新建的「{}」对话会话。",
        agent.label()
    ))
    .keywords(["acp", "对话", "agent", agent.id()])
}

/// 按钮的短暂成功文案（设置页复制、自动化保存等读它改按钮字）。
#[derive(Clone, Default)]
struct CopyFlash {
    id: String,
    label: String,
    until: Option<Instant>,
}

impl Global for CopyFlash {}

/// 「存储」设置页的扫描结果 + 上一次清理的提示文案（点按钮时同步刷新，扫描很快
/// 不值得像更新检查那样搞异步状态机）。
#[derive(Clone, Default)]
struct CleanupState {
    scan: Option<crate::storage_cleanup::CleanupScan>,
    message: Option<SharedString>,
}

impl Global for CleanupState {}

/// Agent CLI 的只读探测结果。实际命令探测在后台执行；设置页只读取这个快照，
/// 避免打开设置时被多个 `--version` 子进程卡住。
#[derive(Clone, Default)]
pub struct AcpRuntimeState {
    pub diagnostics: Option<smelt_model::acp_conn::AcpRuntimeDiagnostics>,
    pub refreshing: bool,
    pub checked_at: Option<Instant>,
    pub installing: Option<ConversationAgentKind>,
    pub install_message: Option<SharedString>,
    pub install_failed: bool,
}

impl Global for AcpRuntimeState {}

fn runtime_checked_at_text(checked_at: Option<Instant>) -> String {
    let Some(checked_at) = checked_at else {
        return "尚未检测".to_string();
    };
    let elapsed = checked_at.elapsed().as_secs();
    match elapsed {
        0..=4 => "刚刚检测".to_string(),
        5..=59 => format!("{elapsed} 秒前检测"),
        60..=3599 => format!("{} 分钟前检测", elapsed / 60),
        _ => format!("{} 小时前检测", elapsed / 3600),
    }
}

pub(crate) fn copy_btn_label(id: &str, idle: &str, cx: &App) -> String {
    flash_btn_label(id, idle, cx)
}

pub(crate) fn flash_btn_label(id: &str, idle: &str, cx: &App) -> String {
    if let Some(f) = cx.try_global::<CopyFlash>()
        && f.id == id
        && let Some(until) = f.until
        && Instant::now() < until
    {
        if f.label.is_empty() {
            return "已复制 ✓".into();
        }
        return f.label.clone();
    }
    idle.into()
}

/// 只改按钮文案。
pub(crate) fn flash_button(id: impl Into<String>, label: &'static str, cx: &mut App) {
    let id = id.into();
    cx.set_global(CopyFlash {
        id: id.clone(),
        label: label.into(),
        until: Some(Instant::now() + Duration::from_millis(2000)),
    });
    let clear_id = id;
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(2000))
            .await;
        cx.update(|cx| {
            let same = cx
                .try_global::<CopyFlash>()
                .map(|f| f.id == clear_id)
                .unwrap_or(false);
            if same {
                cx.set_global(CopyFlash::default());
            }
        });
    })
    .detach();
}

/// 写入剪贴板，按钮文案闪「已复制 ✓」约 2 秒。
pub(crate) fn copy_with_feedback(text: String, btn_id: &'static str, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
    flash_button(btn_id, "已复制 ✓", cx);
    cx.refresh_windows();
}

/// 改外观全局 + 存盘，不触发 view 重绘（调用方按需自己 notify/refresh）。
/// 供只有 `&mut App`（没有 `Context<Self>`）的场景用，比如设置页 SettingField 的 get/set 闭包。
fn apply_appearance(f: impl FnOnce(&mut Appearance), cx: &mut App) {
    let mut edited = cx.global::<Appearance>().clone();
    f(&mut edited);
    save_appearance(&edited);
    // 用户自选底色要镜像给 PTY 线程（OSC 11 应答）和移动端配色快照，三处同源。
    apply_bg_color(&edited);
    cx.set_global(edited);
    sync_theme_fonts(cx);
}

/// 把 `Appearance.bg_color` 落到终端默认底色：没被用户改过就跟主题走。
pub fn apply_bg_color(a: &Appearance) {
    terminal::set_bg_override((!a.bg_color_is_default()).then_some(a.bg_color));
    publish_terminal_theme();
}

/// Hsla → 0xRRGGBB（取色器回调把颜色写回 config 用）。
fn hsla_to_rgb(c: Hsla) -> u32 {
    let rgba = Rgba::from(c);
    let q = |f: f32| ((f.clamp(0.0, 1.0) * 255.0).round() as u32) & 0xff;
    (q(rgba.r) << 16) | (q(rgba.g) << 8) | q(rgba.b)
}

// ===================== 设置页专属类型 =====================

/// 启动项列表编辑器：每项一对 label/command 输入框。
pub struct LaunchInputs {
    rows: Vec<(
        Entity<gpui_component::input::InputState>,
        Entity<gpui_component::input::InputState>,
        bool,
    )>,
    _subs: Vec<Subscription>,
}

/// 手动添加 workspace 列表编辑器：每项一对 label/workspace_dir 输入框；agent
/// 种类走下拉选择（离散值，不需要输入框），选完直接存盘不用另外的 InputState。
pub struct ProfileInputs {
    rows: Vec<(
        Entity<gpui_component::input::InputState>,
        Entity<gpui_component::input::InputState>,
    )>,
    _subs: Vec<Subscription>,
}

#[derive(Clone)]
pub struct PiModelEditor {
    pub provider: Entity<gpui_component::input::InputState>,
    pub model: Entity<gpui_component::input::InputState>,
    pub base_url: Entity<gpui_component::input::InputState>,
    pub api_key: Entity<gpui_component::input::InputState>,
    pub thinking_level: String,
    pub credential_configured: bool,
}

#[derive(Clone)]
pub struct PiCustomModelEditor {
    pub id: Entity<gpui_component::input::InputState>,
    pub name: Entity<gpui_component::input::InputState>,
    pub context_window: Entity<gpui_component::input::InputState>,
    pub max_tokens: Entity<gpui_component::input::InputState>,
}

#[derive(Clone)]
pub struct PiCustomProviderEditor {
    pub previous_id: Option<String>,
    pub id: Entity<gpui_component::input::InputState>,
    pub display_name: Entity<gpui_component::input::InputState>,
    pub api_key: Entity<gpui_component::input::InputState>,
    pub base_url: Entity<gpui_component::input::InputState>,
    pub api: String,
    pub models: Vec<PiCustomModelEditor>,
    /// 「获取模型列表」的进度与结果；`None` 表示还没点过。
    pub discovery: Option<ModelDiscoveryState>,
    pub credential_configured: bool,
}

/// 模型发现的进度：一次直连 HTTP。
#[derive(Clone)]
pub enum ModelDiscoveryState {
    Loading,
    /// 端点答了。空列表也是答案——它说"我一个都不提供"。
    Ready(Vec<smelt_model::provider_api::DiscoveredModel>),
    /// 问不到。协议不支持列举、端点拒绝、凭据不对都落在这里，理由必须原样带出来。
    Failed(String),
}

/// Pi 内置 provider 的凭据面板：列表状态。
///
/// 列一次要起一个 bun 子进程（首次还可能连带装运行时），所以是异步填的，且
/// `None` 表示还没问过——面板不展开就不问，别让每次打开设置都多起一个进程。
#[derive(Clone)]
pub enum PiAuthProvidersState {
    Loading,
    Ready(Vec<smelt_model::pi_auth::PiAuthProvider>),
    Failed(String),
}

/// 「换个默认模型」的选择器：某个已配置 provider 现在能用的模型。
///
/// 和 [`PiLoginView::models`] 分开，是因为两者的生命周期不同：那份跟着一次登录
/// 会话走，登录面板一关就没了；这份是随时可以对任意一个已登录 provider 打开的。
#[derive(Clone)]
pub struct PiAuthModelPicker {
    pub provider_id: String,
    pub provider_name: String,
    /// `None` 表示还在问。
    pub models: Option<Result<Vec<smelt_model::provider_api::DiscoveredModel>, String>>,
}

/// 登录流程当前在等用户回答的那个提问。
#[derive(Clone)]
pub struct PiLoginPrompt {
    pub id: String,
    pub kind: smelt_model::pi_auth::PiPromptKind,
    pub message: String,
    pub placeholder: Option<String>,
    pub options: Vec<smelt_model::pi_auth::PiPromptOption>,
}

/// 一次进行中的（或刚结束的）登录。登录本身跑在运行时那边，这里只拿着它的 id。
#[derive(Clone)]
pub struct PiLoginView {
    pub provider_id: String,
    pub provider_name: String,
    /// 运行时给的登录 id：取事件、回答、取消都带着它。
    pub login_id: String,
    /// progress / info 的流水，最新的在最后。
    pub messages: Vec<String>,
    pub auth_url: Option<String>,
    pub instructions: Option<String>,
    /// device code 流：用户码 + 验证地址。
    pub device_code: Option<(String, String)>,
    pub prompt: Option<PiLoginPrompt>,
    /// 两个输入框在登录开始时就建好：提问是从子进程的事件流里来的，那里没有
    /// `Window`，建不了控件，也改不了掩码。按提问类型选用哪一个即可。
    pub input: Entity<gpui_component::input::InputState>,
    pub secret_input: Entity<gpui_component::input::InputState>,
    /// 输入框上的回车订阅。快照会 clone 整个 view，而订阅一旦被丢弃回车就失灵，
    /// 所以放进 `Rc` 跟着 view 一起活。订阅不是 `Send`，不能用 `Arc`。
    #[allow(dead_code, reason = "只为让订阅活到面板关闭，没有读取方")]
    pub input_subscriptions: std::rc::Rc<Vec<gpui::Subscription>>,
    /// `None` 表示还在跑。
    pub outcome: Option<Result<(), String>>,
    /// 登录成功后可选的模型目录，用来一键设默认。
    pub models: Option<Result<Vec<smelt_model::provider_api::DiscoveredModel>, String>>,
}

impl PiLoginView {
    /// 当前提问该用哪个输入框。
    pub fn active_input(&self) -> Option<&Entity<gpui_component::input::InputState>> {
        match self.prompt.as_ref()?.kind {
            smelt_model::pi_auth::PiPromptKind::Select => None,
            smelt_model::pi_auth::PiPromptKind::Secret => Some(&self.secret_input),
            _ => Some(&self.input),
        }
    }
}

/// 画自定义 provider 的模型发现结果。
#[allow(clippy::too_many_arguments)]
fn render_model_discovery_section(
    id_prefix: &'static str,
    state: Option<ModelDiscoveryState>,
    adopted: Vec<String>,
    manual_rows: usize,
    workspace: Entity<Workspace>,
    adopt: fn(
        &mut Workspace,
        smelt_model::provider_api::DiscoveredModel,
        &mut Window,
        &mut Context<Workspace>,
    ),
    muted: gpui::Hsla,
    danger_fg: gpui::Hsla,
) -> gpui::AnyElement {
    let note = |color: gpui::Hsla, text: String| div().text_xs().text_color(color).child(text);

    match state {
        None if manual_rows == 0 => note(
            muted,
            "未列出任何模型：保存时将从 API 地址自动获取，并在每次启动时刷新。\
             想自己固定一份目录，就在上面添加模型。"
                .to_string(),
        )
        .into_any_element(),
        None => note(
            muted,
            "可以点「获取模型列表」从 API 地址读取该服务提供的模型，不必逐个手填。".to_string(),
        )
        .into_any_element(),
        Some(ModelDiscoveryState::Loading) => {
            note(muted, "正在向该端点询问可用模型…".to_string()).into_any_element()
        }
        Some(ModelDiscoveryState::Failed(error)) => note(
            danger_fg,
            format!("未能读取模型列表：{error}（仍可在下方手动填写）"),
        )
        .into_any_element(),
        Some(ModelDiscoveryState::Ready(models)) if models.is_empty() => note(
            muted,
            "该端点没有报告任何模型，请在下方手动填写。".to_string(),
        )
        .into_any_element(),
        Some(ModelDiscoveryState::Ready(models)) => {
            let total = models.len();
            let buttons = models.into_iter().map(move |model| {
                let entity = workspace.clone();
                let taken = adopted.iter().any(|id| id == &model.id);
                let label = model.label().to_string();
                Button::new(format!("{id_prefix}-discovered-{}", model.id))
                    .choice()
                    .compact()
                    .selected(taken)
                    .label(label)
                    .disabled(taken)
                    .on_click(move |_, window, cx| {
                        let model = model.clone();
                        entity.update(cx, |workspace, cx| {
                            adopt(workspace, model, window, cx);
                        });
                    })
            });
            v_flex()
                .gap_1()
                .child(note(
                    muted,
                    format!("该端点提供 {total} 个模型，点击添加到下方列表："),
                ))
                .child(h_flex().gap_2().flex_wrap().children(buttons))
                .into_any_element()
        }
    }
}

/// 运行时长的人话格式：秒 → 「3 小时 12 分」。只保留两级单位，设置页那行不需要秒级精度。
fn fmt_uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{secs} 秒"),
        (0, 0, m) => format!("{m} 分钟"),
        (0, h, m) => format!("{h} 小时 {m} 分"),
        (d, h, _) => format!("{d} 天 {h} 小时"),
    }
}

/// 守护运行信息拼成一行：`v0.5.4 · PID 64954 · 启动于 07-16 20:38（已运行 3 小时 12 分）· 5 个会话`。
/// 老守护回不出的字段直接不显示——宁可少一段，也不摆「未知」占位。
fn daemon_info_line(info: &crate::local_daemon::DaemonInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(v) = &info.version {
        parts.push(format!("v{v}"));
    }
    if let Some(pid) = info.pid {
        parts.push(format!("PID {pid}"));
    }
    if let Some(started) = info.started_at {
        // 本地时区显示；秒数换算成人话时长跟在后面。
        let started_txt = chrono::DateTime::from_timestamp(started as i64, 0)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| "?".into());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // saturating：守护跟 GUI 之间时钟若有漂移，别算出个天文数字。
        parts.push(format!(
            "启动于 {started_txt}（已运行 {}）",
            fmt_uptime(now.saturating_sub(started))
        ));
    }
    if let Some(n) = info.session_count {
        parts.push(format!("{n} 个会话"));
    }
    parts.join(" · ")
}

/// Workspace 同时承载终端、任务和动画，通知频率会远高于设置页所需。设置主体只取
/// 最终状态，因此将连续更新合并到这一间隔；本地输入控件仍由各自的实体即时刷新。
const SETTINGS_CONTENT_REFRESH_MIN_INTERVAL: Duration = Duration::from_millis(250);

/// 设置是调整透明度时也必须能稳定阅读的工作面；透明度只预览在工作台窗口，
/// 设置窗口本身始终保持完全不透明，避免后方终端文字穿过控件和导航。
const SETTINGS_WINDOW_OPACITY: f32 = 1.0;

/// 设置页在渲染前从 Workspace 取出的快照。不能在 `Render::render` 中直接访问
/// Workspace：终端流式输出等高频通知会让 GPUI 认为整张设置页都依赖它，从而每帧
/// 失效缓存。可交互控件仍保留实体句柄，点击回调再回到 Workspace 执行。
#[derive(Clone)]
struct SettingsRenderSnapshot {
    bg_color_picker: Option<Entity<ColorPickerState>>,
    opacity_slider: Option<Entity<SliderState>>,
    ui_font_size_slider: Option<Entity<SliderState>>,
    font_size_slider: Option<Entity<SliderState>>,
    bg_image_opacity_slider: Option<Entity<SliderState>>,
    font_options: std::sync::Arc<Vec<(SharedString, SharedString)>>,
    ui_font_options: std::sync::Arc<Vec<(SharedString, SharedString)>>,
    launch_rows: Vec<(
        Entity<gpui_component::input::InputState>,
        Entity<gpui_component::input::InputState>,
        bool,
    )>,
    profile_rows: Vec<(
        Entity<gpui_component::input::InputState>,
        Entity<gpui_component::input::InputState>,
    )>,
    pi_model_editor: Option<PiModelEditor>,
    pi_custom_provider_editor: Option<PiCustomProviderEditor>,
    /// Pi 内置 provider 的登录状态；`None` 表示这台机器上还没问过。
    pi_auth_providers: Option<PiAuthProvidersState>,
    pi_login: Option<PiLoginView>,
    pi_auth_model_picker: Option<PiAuthModelPicker>,
    /// 注销、刷新这类一次性操作的最近一次错误。
    pi_auth_error: Option<String>,
    pi_auth_show_all: bool,
    /// `~/.pi/agent/mcp.json` 里的服务器。文件很小，每次刷新快照时重读，外部改动也能跟上。
    pi_mcp_servers: Result<Vec<smelt_model::pi_mcp::PiMcpServer>, String>,
    /// 能力库那台机器装了远程桌面：浏览器类预设在那里接远程桌面里的 Chrome。
    pi_mcp_remote_desktop: bool,
    /// 能力库在配哪台机器：模型下拉按它取可选模型。
    pi_library_runtime: crate::RuntimeKey,
    pi_mcp_login: Option<String>,
    pi_mcp_pending_remove: Option<String>,
    pi_mcp_error: Option<String>,
    pi_model_editor_error: Option<String>,
    pi_model_settings: Result<smelt_model::pi_model_settings::PiModelSettings, String>,
    pi_settings: Result<smelt_model::pi_model_settings::PiSettingsSummary, String>,
    pi_plugins: Vec<smelt_model::pi_plugin_catalog::PiPlugin>,
    pi_paths: smelt_model::pi_service::PiConfigPaths,
    pi_plugin_pending_delete: Option<String>,
    pi_plugin_error: Option<String>,
    /// 插件 id -> 勾选了它的智能体名字，用来在删除前提示占用方。
    agent_names_using_plugin: std::collections::HashMap<String, Vec<String>>,
    panel_plugins: PanelPluginsSnapshot,
    #[cfg(target_os = "macos")]
    update_status: updater::UpdateStatus,
    /// 正在把 App 移到「应用程序」，见 `Workspace::move_app_to_applications`。
    #[cfg(target_os = "macos")]
    moving_app: bool,
    #[cfg(target_os = "macos")]
    move_app_error: Option<SharedString>,
    daemon_outdated: Option<bool>,
    daemon_upgrading: bool,
    daemon_upgrade_msg: Option<String>,
    daemon_info: Option<crate::local_daemon::DaemonInfo>,
    settings_section: SettingsSection,
    settings_page_nonce: usize,
    /// 能力库当前页签。和 `settings_section` 分开记：设置窗口和能力库可以同时开着，
    /// 各看各的页。
    capabilities_tab: crate::CapabilitiesTab,
    show_daemon_restart_confirm: bool,
    show_update_install_confirm: bool,
    session_manager_open: bool,
    /// 资源页的数。只在停在资源页时取，别的页用不着拼它。
    resources: Option<std::rc::Rc<crate::resource_monitor::PanelSnapshot>>,
}

/// 设置内容画在哪。两处共用同一套快照和节流刷新，只是外壳不同。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsSurface {
    /// 独立设置窗口：左侧两级导航，右侧当前页。
    Window,
    /// 主窗口「能力库」页的正文。页头和页签由 Workspace 画，这里只画当前页签的内容。
    Capabilities,
}

/// 设置内容子视图。独立设置窗口里，它和外层的键盘/性能 HUD 分开，HUD 的逐帧采样不会
/// 重建全部 Settings 元素；主窗口里，它把设置快照和终端等高频通知隔开。
pub struct SettingsContentView {
    workspace: Entity<Workspace>,
    surface: SettingsSurface,
    snapshot: SettingsRenderSnapshot,
    _observe_workspace: Subscription,
    _observe_settings_globals: Vec<Subscription>,
    last_refresh: Option<Instant>,
    refresh_pending: bool,
    refresh_generation: u64,
}

impl SettingsContentView {
    /// `snapshot` 由调用方在创建前同步取好；构造期间不能再回头 update Workspace。
    fn new(
        workspace: Entity<Workspace>,
        surface: SettingsSurface,
        snapshot: SettingsRenderSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            _observe_workspace: cx.observe_in(
                &workspace,
                window,
                |this: &mut SettingsContentView, _, window, cx| {
                    this.request_refresh(window, cx);
                },
            ),
            workspace,
            surface,
            snapshot,
            _observe_settings_globals: vec![
                Self::observe_settings_global::<Appearance>(window, cx),
                Self::observe_settings_global::<LaunchConfig>(window, cx),
                Self::observe_settings_global::<AgentHostState>(window, cx),
                Self::observe_settings_global::<AgentHosts>(window, cx),
                Self::observe_settings_global::<AcpRuntimeState>(window, cx),
                #[cfg(target_os = "macos")]
                Self::observe_settings_global::<UpdateSettings>(window, cx),
                Self::observe_settings_global::<CleanupState>(window, cx),
                Self::observe_settings_global::<CopyFlash>(window, cx),
                Self::observe_settings_global::<RemoteConfig>(window, cx),
                Self::observe_settings_global::<RemoteSettingsState>(window, cx),
                Self::observe_settings_global::<crate::runtimes::RuntimeHub>(window, cx),
            ],
            // 初始快照已经同步取好；避免刚打开就被第一条后台流式通知立即打穿缓存。
            last_refresh: Some(Instant::now()),
            refresh_pending: false,
            refresh_generation: 0,
        }
    }

    fn observe_settings_global<G: Global + 'static>(
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.observe_global_in::<G>(window, |this, window, cx| this.request_refresh(window, cx))
    }

    fn refresh_snapshot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot = self.workspace.update(cx, |workspace, cx| {
            workspace.ensure_launch_inputs(window, cx);
            workspace.ensure_profile_inputs(window, cx);
            workspace.settings_render_snapshot(cx)
        });
        self.last_refresh = Some(Instant::now());
        self.refresh_pending = false;
        self.refresh_generation = self.refresh_generation.wrapping_add(1);
        cx.notify();
    }

    fn request_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 子菜单和页签切换必须立刻生效，不能被设置页 250ms 刷新节流挡住。
        let (current_section, current_tab) = {
            let workspace = self.workspace.read(cx);
            (
                workspace.settings_section.clone(),
                workspace.nav.capabilities_tab(),
            )
        };
        if current_section != self.snapshot.settings_section
            || current_tab != self.snapshot.capabilities_tab
        {
            self.refresh_snapshot(window, cx);
            return;
        }

        let now = Instant::now();
        let Some(last_refresh) = self.last_refresh else {
            self.refresh_snapshot(window, cx);
            return;
        };

        // 已经排队的合并刷新会带上此前全部状态；等它执行即可，避免 executor 繁忙时
        // 又穿透节流发一次立即刷新。
        if self.refresh_pending {
            return;
        }

        let elapsed = now.saturating_duration_since(last_refresh);
        if elapsed >= SETTINGS_CONTENT_REFRESH_MIN_INTERVAL {
            self.refresh_snapshot(window, cx);
            return;
        }

        self.refresh_pending = true;
        let generation = self.refresh_generation;
        let delay = SETTINGS_CONTENT_REFRESH_MIN_INTERVAL.saturating_sub(elapsed);
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = cx.update(|window, cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.refresh_pending && this.refresh_generation == generation {
                        this.refresh_snapshot(window, cx);
                    }
                });
            });
        })
        .detach();
    }
}

impl Render for SettingsContentView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _timing = smelt_ui::frame_stats::RenderTiming::start("SettingsContent");
        let snapshot = self.snapshot.clone();
        let workspace = self.workspace.clone();
        if self.surface == SettingsSurface::Capabilities {
            // 能力库只有模型、插件、记忆三页，没有维护页那些弹层；弹层若也画在这里，
            // 设置窗口开着时会在主窗口再弹一份。
            return div()
                .size_full()
                .child(Workspace::render_capabilities_content(
                    workspace, &snapshot, cx,
                ))
                .into_any_element();
        }

        // 设置内容 +（可选）守护管理弹层：弹层必须画在本窗，不能只改 Workspace 上的
        // flag 却在主窗口 render——用户点的是设置里的按钮，弹窗却跑到主界面。
        // 焦点也在这扇窗里记，不能放到主窗口的 prepare。
        workspace.update(cx, |ws, cx| ws.sync_settings_modal_focus(window, cx));
        let show_daemon_restart_confirm = snapshot.show_daemon_restart_confirm;
        let show_update_install_confirm = snapshot.show_update_install_confirm;
        let session_manager_open = snapshot.session_manager_open;

        div()
            .relative()
            .size_full()
            .child(Workspace::render_settings_content(
                workspace.clone(),
                &snapshot,
                cx,
            ))
            // 弹层开启期间才需要读它的动态会话数据；正常浏览设置页不会因此订阅整个
            // Workspace 的高频状态通知。
            .children(
                show_daemon_restart_confirm
                    .then(|| workspace.update(cx, |ws, cx| ws.render_daemon_restart_confirm(cx))),
            )
            .children(show_update_install_confirm.then(|| {
                workspace.update(cx, |ws, cx| ws.render_update_install_confirm(cx))
            }))
            .children(
                session_manager_open
                    .then(|| workspace.update(cx, |ws, cx| ws.render_session_manager(cx))),
            )
            .into_any_element()
    }
}

/// 独立设置窗口的根 view。它只负责焦点和性能 HUD，设置主体由可缓存子视图提供。
pub struct SettingsWindow {
    content: Entity<SettingsContentView>,
    focus_handle: FocusHandle,
    did_focus: bool,
    _observe_appearance: Subscription,
    applied_window_bg: Option<WindowBackgroundAppearance>,
    applied_window_opacity: Option<f32>,
    applied_glass_style: Option<liquid_glass::GlassStyle>,
    applied_ui_font_px: Option<u32>,
    /// 调试 HUD 开关。监视器按窗口各一份：两个窗口的布局负载不同。
    debug_hud: bool,
}

impl SettingsWindow {
    fn toggle_debug_hud(&mut self, cx: &mut Context<Self>) {
        self.debug_hud = !self.debug_hud;
        cx.notify();
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.did_focus {
            self.did_focus = true;
            window.focus(&self.focus_handle, cx);
        }
        let debug_hud = self
            .debug_hud
            .then(|| crate::overlay::debug_hud(window, cx));

        let ui_font_px = cx.global::<Appearance>().ui_font_px;
        if self.applied_ui_font_px != Some(ui_font_px) {
            window.set_rem_size(px(ui_font_px as f32));
            self.applied_ui_font_px = Some(ui_font_px);
        }
        let window_bg = cx.global::<Appearance>().window_bg();
        let glass_style = cx.global::<Appearance>().glass_style;
        if self.applied_window_bg != Some(window_bg)
            || self.applied_glass_style != Some(glass_style)
        {
            window.set_background_appearance(window_bg);
            crate::liquid_glass::sync(window, glass_style);
            self.applied_window_bg = Some(window_bg);
            self.applied_glass_style = Some(glass_style);
        }
        let window_opacity = SETTINGS_WINDOW_OPACITY;
        if self.applied_window_opacity != Some(window_opacity) {
            apply_window_opacity(window, window_opacity);
            self.applied_window_opacity = Some(window_opacity);
        }

        let content =
            AnyView::from(self.content.clone()).cached(StyleRefinement::default().size_full());

        div()
            .relative()
            .size_full()
            .font_family(resolved_ui_font_family(cx.global::<Appearance>()))
            .track_focus(&self.focus_handle)
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                let keystroke = &event.keystroke;
                if keystroke.modifiers.platform && keystroke.modifiers.shift && keystroke.key == "f"
                {
                    this.toggle_debug_hud(cx);
                    cx.stop_propagation();
                }
            }))
            .child(content)
            .children(debug_hud)
    }
}

/// 设置窗口的单例句柄：已经开着就聚焦复用，避免重复开出好几扇一样的窗口。
pub struct SettingsWindowHandle(pub WindowHandle<Root>);
impl Global for SettingsWindowHandle {}

// ===================== Workspace 方法 =====================

#[cfg(test)]
mod tests;
