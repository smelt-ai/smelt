//! 设置面板：外观 / 模型 / 会话与 Agent / 协作 / 插件 / 维护 / 快捷键，含独立
//! 设置窗口的渲染逻辑。左侧是分类和子菜单，每个子菜单对应独立内容页。
//!
//! 跟 git_panel / file_tree 同一个套路：从 main.rs 拆出来的 `impl Workspace`
//! 方法 + 独立类型/函数，字段仍然声明在 main.rs 的 `Workspace` struct 里。
//!
//! 自动更新（`update_status`/`check_for_update`/`upgrade_daemon_seamless` 等）**不在
//! 这里**——那是应用级生命周期状态，不属于任何一个面板，仍留在 main.rs；这里的
//! 「维护」SettingPage 只是读它、展示它、提供按钮触发它。

use std::time::{Duration, Instant};

use gpui::InteractiveElement;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::progress::Progress;
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings};
use gpui_component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_component::switch::Switch;
use gpui_component::*;

use crate::{Workspace, liquid_glass, terminal, terminal_view, updater};

mod agent_hooks;
mod nav;
mod pi_auth;
pub(crate) mod plugins;
mod remote;
mod workspace;
mod workspace_view;

pub(crate) use nav::{
    SettingsCategoryId, SettingsNavCategory, SettingsScope, SettingsSection, filter_settings_nav,
    nav_contains,
};
pub(super) use plugins::*;
pub(super) use remote::*;

#[cfg(test)]
pub(super) use remote::qr_png_for_url;

pub(super) use agent_hooks::{
    hooks_installed_status, install_agent_hooks, invalidate_hooks_cache, smelt_notify_path,
    sync_bundled_smelt_agent_mcp, sync_bundled_smelt_notify, uninstall_agent_hooks,
};

#[cfg(test)]
pub(super) use agent_hooks::{
    ANTIGRAVITY_HOOK_EVENTS, ANTIGRAVITY_HOOK_NAME, antigravity_event_installed,
    command_uses_smelt_notify, merge_antigravity_hooks, uninstall_antigravity_hooks_at,
    uninstall_hook_file, write_json_atomic,
};

// ===================== 外观 / 启动 配置类型 =====================

fn default_theme_mode() -> ThemeMode {
    ThemeMode::Dark
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
    /// 明暗主题模式。
    #[serde(default = "default_theme_mode")]
    pub theme_mode: ThemeMode,
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
/// 通道只决定读取哪一个公司 COS manifest；更新流程本身仍由 updater 统一处理。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UpdateSettings {
    #[serde(default)]
    pub channel: updater::UpdateChannel,
    /// 发现新版本后是否自动下载并装好。关掉只影响"要不要自动下载"这一步，
    /// 检查照常做——否则用户既不会自动更新也看不到有新版，等于彻底静默。
    #[serde(default = "default_auto_install")]
    pub auto_install: bool,
}

fn default_auto_install() -> bool {
    true
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            channel: updater::UpdateChannel::default(),
            auto_install: default_auto_install(),
        }
    }
}

impl Global for UpdateSettings {}

pub fn load_update_settings() -> UpdateSettings {
    let Ok(store) = smelt_core::sqlite_state::default_sqlite_store() else {
        return UpdateSettings::default();
    };
    match store.get_update_settings_snapshot() {
        Ok(Some(snapshot)) => settings_from_snapshot(snapshot),
        Ok(None) | Err(_) => UpdateSettings::default(),
    }
}

pub fn save_update_settings(settings: &UpdateSettings) {
    if let Ok(store) = smelt_core::sqlite_state::default_sqlite_store() {
        let _ = store.put_update_settings_snapshot(&snapshot_from_settings(settings));
    }
}

fn snapshot_from_settings(settings: &UpdateSettings) -> smelt_store::UpdateSettingsSnapshot {
    smelt_store::UpdateSettingsSnapshot {
        channel: match settings.channel {
            updater::UpdateChannel::Dev => "dev".into(),
            updater::UpdateChannel::Prod => "prod".into(),
        },
        auto_install: settings.auto_install,
    }
}

fn settings_from_snapshot(snapshot: smelt_store::UpdateSettingsSnapshot) -> UpdateSettings {
    UpdateSettings {
        channel: match snapshot.channel.as_str() {
            "dev" => updater::UpdateChannel::Dev,
            _ => updater::UpdateChannel::Prod,
        },
        auto_install: snapshot.auto_install,
    }
}

pub fn apply_update_channel(channel: updater::UpdateChannel, cx: &mut App) {
    let mut settings = *cx.global::<UpdateSettings>();
    settings.channel = channel;
    save_update_settings(&settings);
    cx.set_global(settings);
}

pub fn apply_auto_install(auto_install: bool, cx: &mut App) {
    let mut settings = *cx.global::<UpdateSettings>();
    settings.auto_install = auto_install;
    save_update_settings(&settings);
    cx.set_global(settings);
}

/// 把主题模式落到所有吃颜色的层：gpui-component 部件、自绘 UI 语义色板、终端调色板。
/// **唯一入口**——三处必须同时切，漏一处就是「面板变浅了但终端还是黑的」这种半吊子。
/// 只改全局态不重绘，调用方自己决定什么时候 `cx.refresh_windows()`
/// （启动时还没有窗口，切换时才需要）。
/// CLI 改过 SQLite 后，打开设置页时把外观重新套上窗口。
pub fn reload_appearance_from_store(cx: &mut App) {
    let appearance = load_appearance();
    terminal_view::set_font_px(appearance.font_px);
    terminal_view::set_font_family(&appearance.font_family);
    cx.set_global(appearance.clone());
    apply_theme_mode(appearance.theme_mode, cx);
    apply_bg_color(&appearance);
    cx.refresh_windows();
}

pub fn apply_theme_mode(mode: ThemeMode, cx: &mut App) {
    Theme::change(mode, None, cx);
    crate::ui_theme::set_light(!mode.is_dark());
    terminal::set_dark_mode(mode.is_dark());
    // Theme::change 装的是组件库自带色板，跟 ui_theme 是两套值——同屏里
    // `t.border` 和 `ui_theme::border_mid()` 挨着出现就会差一档。这里按语义位
    // 把组件库主题覆写成 ui_theme 的值，色真源收敛成一个。
    // 覆写必须在 Theme::change 之后：它会整套 apply_config 覆盖回默认。
    crate::ui_theme::apply_to_component_theme(cx);
    // Theme::change 会把 font_size / mono_font_family 重置回组件库默认；
    // 必须写回 Appearance，否则界面字号闪回 16px，markdown 代码块也会掉回 Menlo。
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
    if cx.has_global::<Theme>() {
        let theme = cx.global_mut::<Theme>();
        theme.font_size = px(ui_font_px as f32);
        theme.font_family = ui_family;
        theme.mono_font_family = terminal_view::font_family().into();
    }
}

/// 把「当前生效的终端配色」落盘给守护/网关，转发给移动端。
///
/// 移动端渲染的是同一份 PTY 字节流，而 TUI 会用 OSC 11 查背景色来决定用哪档灰
/// （应答见 `terminal::EventProxy::resolve_color`）。手机若用自己写死的配色，
/// TUI 以为的底色跟实际底色对不上，就是对比度问题。所以颜色真源只有 PC 这一份，
/// 而且一部手机可以连多台设备，配色必须**按设备**下发而不是客户端内置。
///
/// 主题模式、用户自选底色变化时都要调（见 `apply_theme_mode` / `apply_appearance`）。
pub fn publish_terminal_theme() {
    smelt_core::terminal_theme::publish(&current_terminal_theme());
}

/// 按当前主题 + 外观设置组装配色快照。
fn current_terminal_theme() -> smelt_core::terminal_theme::TerminalThemeSnapshot {
    smelt_core::terminal_theme::TerminalThemeSnapshot {
        version: smelt_core::terminal_theme::TERMINAL_THEME_VERSION,
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

/// 读取外观设置；缺失/损坏回退默认。
pub fn load_appearance() -> Appearance {
    let Ok(store) = smelt_core::sqlite_state::default_sqlite_store() else {
        return Appearance::default();
    };
    match store.get_appearance_snapshot() {
        Ok(Some(snapshot)) => appearance_from_snapshot(snapshot),
        Ok(None) => Appearance::default(),
        Err(error) => {
            eprintln!("[storage] 读取外观设置失败，使用默认值: {error}");
            Appearance::default()
        }
    }
}

/// 写回外观设置；失败由统一存储入口记录，界面主流程继续运行。
fn save_appearance(a: &Appearance) {
    persist_appearance(a);
}

fn persist_appearance(appearance: &Appearance) {
    if let Ok(store) = smelt_core::sqlite_state::default_sqlite_store() {
        let _ = store.put_appearance_snapshot(&snapshot_from_appearance(appearance));
    }
}

fn snapshot_from_appearance(appearance: &Appearance) -> smelt_store::AppearanceSnapshot {
    smelt_store::AppearanceSnapshot {
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
        ui_font_px: appearance.ui_font_px,
        ui_font_family: appearance.ui_font_family.clone(),
        font_px: appearance.font_px,
        font_family: appearance.font_family.clone(),
    }
}

fn appearance_from_snapshot(snapshot: smelt_store::AppearanceSnapshot) -> Appearance {
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
        theme_mode: serde_json::from_value(serde_json::Value::String(snapshot.theme_mode))
            .unwrap_or_else(|_| default_theme_mode()),
        ui_font_px: snapshot.ui_font_px,
        ui_font_family: snapshot.ui_font_family,
        font_px: snapshot.font_px,
        font_family: snapshot.font_family,
    }
}

/// 项目行「+」下拉菜单里的一条可配置启动项，以及它的出厂默认集合。
/// 定义在 `smelt-core`：桌面新建菜单和移动端读的是同一份启动项目录。
pub use smelt_core::new_session::{LaunchEntry, default_launch_entries};

/// 内置启动项由 provider 注册信息识别；旧配置缺少 provider 时，只把 Smelt
/// 发布过的逐字默认命令视为内置，避免把用户自定义的 Agent 命令误锁定。
fn launch_entry_matches_builtin(entry: &LaunchEntry, agent: TerminalAgentKind) -> bool {
    entry.provider.as_deref() == Some(agent.id())
        || (entry.provider.is_none()
            && (entry.command.trim() == agent.quick_terminal_cmd()
                || agent
                    .upgrade_released_quick_terminal_command(&entry.command)
                    .is_some()))
}

fn launch_entry_is_builtin(entry: &LaunchEntry) -> bool {
    entry
        .provider
        .as_deref()
        .and_then(TerminalAgentKind::from_id)
        .is_some()
}

/// 固定内置启动项与注册表对齐，并给旧版无 provider 的逐字出厂项补上稳定标识。
/// 返回 true 表示配置发生变化，需要持久化。
fn ensure_builtin_launch_entries(config: &mut LaunchConfig) -> bool {
    let mut changed = false;
    for agent in TerminalAgentKind::ALL {
        if let Some(entry) = config
            .entries
            .iter_mut()
            .find(|entry| launch_entry_matches_builtin(entry, agent))
        {
            if entry.provider.is_none() {
                entry.provider = Some(agent.id().to_string());
                changed = true;
            }
            continue;
        }
        config.entries.push(LaunchEntry {
            label: agent.quick_terminal_label().to_string(),
            command: agent.quick_terminal_cmd().to_string(),
            provider: Some(agent.id().to_string()),
        });
        changed = true;
    }
    changed
}

/// 删除自定义启动项。内置项是固定集合，数据层也拒绝删除，不能只依赖设置页
/// 隐藏按钮。
fn remove_launch_entry_at(config: &mut LaunchConfig, index: usize) -> bool {
    let Some(entry) = config.entries.get(index) else {
        return false;
    };
    if launch_entry_is_builtin(entry) {
        return false;
    }
    config.entries.remove(index);
    true
}

/// 项目行「+」可配置启动项列表（全局单例，存 SQLite KV）。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct LaunchConfig {
    /// 启动项迁移版本。旧出厂命令与标签只在对应版本升级时改写；固定内置项则在
    /// 每次加载时与注册表对齐。
    #[serde(default = "current_launch_config_version")]
    version: u32,
    /// 除固定的「新建终端」「新建 Worktree…」外，下拉菜单里的启动项。
    pub entries: Vec<LaunchEntry>,
}

const LAUNCH_CONFIG_VERSION: u32 = 14;

fn current_launch_config_version() -> u32 {
    LAUNCH_CONFIG_VERSION
}

impl Default for LaunchConfig {
    fn default() -> Self {
        Self {
            version: LAUNCH_CONFIG_VERSION,
            entries: default_launch_entries(),
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

fn launch_config_path() -> Option<std::path::PathBuf> {
    smelt_paths::smelt_home().map(|h| h.join(smelt_store::DATABASE_FILE_NAME))
}

/// 持久化文档的原始形状：兼容旧版「全权限」三开关，也兼容新版 `entries` 列表。
/// `entries: None` 表示文档里没写这个键（旧格式）→ 迁到出厂默认并回写；
/// `Some([])` 表示用户清空了列表，照用。
/// 读取启动配置；缺失则回退出厂默认。
pub fn load_launch_config() -> LaunchConfig {
    let Some(path) = launch_config_path() else {
        return LaunchConfig::default();
    };
    load_launch_config_from_path(&path)
}

fn persist_launch_config_at(path: &std::path::Path, config: &LaunchConfig) {
    if let Ok(store) = smelt_core::sqlite_state::open_sqlite_store(path) {
        let snapshot = smelt_store::LaunchSnapshot {
            version: config.version,
            entries: config
                .entries
                .iter()
                .map(|entry| smelt_store::LaunchEntryRecord {
                    label: entry.label.clone(),
                    command: entry.command.clone(),
                    provider: entry.provider.clone(),
                })
                .collect(),
        };
        let _ = store.put_launch_snapshot(&snapshot);
    }
}

fn launch_from_snapshot(snapshot: smelt_store::LaunchSnapshot) -> LaunchConfig {
    LaunchConfig {
        version: snapshot.version,
        entries: snapshot
            .entries
            .into_iter()
            .map(|entry| LaunchEntry {
                label: entry.label,
                command: entry.command,
                provider: entry.provider,
            })
            .collect(),
    }
}

fn load_launch_config_from_path(path: &std::path::Path) -> LaunchConfig {
    if let Ok(store) = smelt_core::sqlite_state::open_sqlite_store(path)
        && let Ok(Some(snapshot)) = store.get_launch_snapshot()
    {
        let mut config = launch_from_snapshot(snapshot);
        let mut changed = config.version < LAUNCH_CONFIG_VERSION;
        if changed {
            for entry in &mut config.entries {
                if let Some(command) = TerminalAgentKind::ALL
                    .into_iter()
                    .find_map(|agent| agent.upgrade_released_quick_terminal_command(&entry.command))
                {
                    entry.command = command.to_string();
                }
                if entry.label.trim() == "Copilot"
                    && (entry.provider.as_deref() == Some(TerminalAgentKind::Copilot.id())
                        || TerminalAgentKind::from_command_prefix(&entry.command)
                            == Some(TerminalAgentKind::Copilot))
                {
                    entry.label = TerminalAgentKind::Copilot
                        .quick_terminal_label()
                        .to_string();
                }
            }
        }
        changed |= ensure_builtin_launch_entries(&mut config);
        if changed {
            config.version = LAUNCH_CONFIG_VERSION;
            persist_launch_config_at(path, &config);
        }
        return config;
    }
    LaunchConfig::default()
}

/// 写回启动配置；失败由统一存储入口记录，界面主流程继续运行。
fn save_launch_config(c: &LaunchConfig) {
    if let Some(path) = launch_config_path() {
        persist_launch_config_at(&path, c);
    }
}

/// 改启动配置全局 + 存盘，不触发 view 重绘，用法同 [`apply_appearance`]。
fn apply_launch_config(f: impl FnOnce(&mut LaunchConfig), cx: &mut App) {
    let mut c = cx.global::<LaunchConfig>().clone();
    f(&mut c);
    save_launch_config(&c);
    cx.set_global(c);
}

// ===================== Agent UI / Claude hooks（B 路线） =====================
//
// ConversationAgentKind / AcpProfile 搬进 smelt-core（本身不需要 GPUI），AgentHostState
// （需要 `gpui::Global`）搬进 smelt-ui——都是 acp_view.rs 独立成 smelt-acp-view
// crate 之后要跨 crate 共用的数据模型。这里重导出成原来的裸名字，本文件剩下
// 的 UI 渲染代码（acp_cmd_setting_item、手动添加 workspace 的编辑器等）不用
// 逐处改路径。
use smelt_core::agent_kind::ConversationLaunchSpec;
pub use smelt_core::agent_kind::{
    AcpProfile, ConversationAgentKind, HistorySourceKind, TerminalAgentKind,
};
pub use smelt_ui::agent_host_state::{
    AgentDefinition, AgentHostState, apply_agent_host, load_agent_host_state,
    reload_agent_definitions, try_apply_agent_host,
};
pub use smelt_ui::automation::{
    Automation, AutomationAction, AutomationActionKindId, AutomationNotificationPreset,
    AutomationSchedule, AutomationSink, AutomationTrigger, AutomationTriggerKindId,
    AutomationTriggerPresetId, BUILTIN_AUTOMATION_ACTIONS, BUILTIN_AUTOMATION_TRIGGER_PRESETS,
    EventIngress, WebhookIngress,
};

/// 全局配置里某个 agent 的启动命令；配置还没装载就退回出厂值。
pub fn acp_cmd_for(agent: ConversationAgentKind, cx: &App) -> String {
    cx.try_global::<AgentHostState>()
        .map(|c| c.acp_cmd_for(agent))
        .unwrap_or_else(|| agent.default_cmd())
}

/// 全局配置里某个 agent 的完整启动规格（命令 + 用户配的环境变量）。
///
/// 新建会话一律走这里。只取 `acp_cmd_for` 再自己 `from_command` 会把环境变量
/// 丢掉，表现是"设置里填了 API key 却还是说没配"——而且只在真发一轮时才暴露。
pub fn acp_launch_for(agent: ConversationAgentKind, cx: &App) -> ConversationLaunchSpec {
    cx.try_global::<AgentHostState>()
        .map(|c| c.acp_launch_for(agent))
        .unwrap_or_else(|| agent.default_launch())
}

/// 普通 ACP 对话的初始配置：Agent 级最近选择打底，会话自身存档逐项覆盖。
/// 插件自动任务不应调用这里，避免交互偏好污染任务运行参数。
pub fn initial_acp_config_for(
    agent: ConversationAgentKind,
    session_values: &[(String, String)],
    cx: &App,
) -> Vec<(String, String)> {
    cx.try_global::<AgentHostState>()
        .map(|config| config.initial_acp_config(agent, session_values))
        .unwrap_or_else(|| session_values.to_vec())
}

/// 智能体页新开的产品对话继承模型等交互偏好，但权限固定为该引擎注册的全权限
/// 模式。普通 ACP 对话和已有会话恢复继续走 `initial_acp_config_for`。
pub fn initial_agent_conversation_config_for(
    agent: ConversationAgentKind,
    cx: &App,
) -> Vec<(String, String)> {
    cx.try_global::<AgentHostState>()
        .map(|config| config.initial_agent_conversation_config(agent))
        .unwrap_or_else(|| {
            agent
                .task_params()
                .full_access_mode
                .map(|mode| vec![("mode".to_string(), mode.to_string())])
                .unwrap_or_default()
        })
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
            move |cx: &App| acp_cmd_setting_value(agent, acp_cmd_for(agent, cx)),
            move |v: SharedString, cx: &mut App| {
                let v = v.trim().to_string();
                // 留空 = 使用内置适配器（不是清成空串跑不起来）。
                let cmd = if v.is_empty() { agent.default_cmd() } else { v };
                apply_agent_host(move |c| c.set_acp_cmd_for(agent, cmd), cx);
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
    pub diagnostics: Option<smelt_core::acp_conn::AcpRuntimeDiagnostics>,
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
    let mut a = cx.global::<Appearance>().clone();
    f(&mut a);
    save_appearance(&a);
    // 用户自选底色要镜像给 PTY 线程（OSC 11 应答）和移动端配色快照，三处同源。
    apply_bg_color(&a);
    cx.set_global(a);
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

/// 插件管理面板的状态。
///
/// 面板一次只服务一个 profile：管理动作会真实改写该 profile 的依赖树，同时开多个
/// 只会让「这条报错是谁的」变得说不清。切 profile 直接换掉整个面板。
/// 插件面板能发起的三件事。
///
/// 安装与更新是**不同的 pnpm 命令**，不能合成一个布尔：`add` 见现装版本已满足
/// manifest 里的范围就直接不动，用它做更新等于什么都没做。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DshPluginAction {
    Install,
    Update,
    Remove,
}

#[derive(Clone)]
pub struct DshPluginManager {
    pub profile: String,
    /// 包名输入框，供安装/升级使用。
    pub spec: Entity<gpui_component::input::InputState>,
    /// 已加载的清单；None 表示还没读过。
    pub plugins: Option<Result<Vec<smelt_core::agent_kind::DshPlugin>, String>>,
    /// 正在执行的动作描述，非 None 时禁用所有按钮。
    pub busy: Option<String>,
    /// 上一次动作的结果。
    pub message: Option<(String, bool)>,
}

#[derive(Clone)]
pub struct DshModelEditor {
    model: Entity<gpui_component::input::InputState>,
    base_url: Entity<gpui_component::input::InputState>,
    api_key: Entity<gpui_component::input::InputState>,
    reasoning_effort: String,
    configured_api_key_env: String,
    credential_configured: bool,
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

#[derive(Clone)]
pub struct DshCustomModelEditor {
    id: Entity<gpui_component::input::InputState>,
    name: Entity<gpui_component::input::InputState>,
    context_window: Entity<gpui_component::input::InputState>,
    max_tokens: Entity<gpui_component::input::InputState>,
}

#[derive(Clone)]
pub struct DshCustomProviderEditor {
    previous_id: Option<String>,
    id: Entity<gpui_component::input::InputState>,
    display_name: Entity<gpui_component::input::InputState>,
    api_key: Entity<gpui_component::input::InputState>,
    base_url: Entity<gpui_component::input::InputState>,
    api: String,
    models: Vec<DshCustomModelEditor>,
    /// 「获取模型列表」的进度与结果；`None` 表示还没点过。
    discovery: Option<ModelDiscoveryState>,
    configured_api_key_env: String,
    credential_configured: bool,
}

/// 模型发现的进度。dsh 侧要起一个完整 dsh 进程再加一次网络往返（约 3-4 秒），
/// Pi 侧是一次直连 HTTP。
#[derive(Clone)]
pub enum ModelDiscoveryState {
    Loading,
    /// 端点答了。空列表也是答案——它说"我一个都不提供"。
    Ready(Vec<smelt_core::provider_api::DiscoveredModel>),
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
    Ready(Vec<smelt_core::pi_auth::PiAuthProvider>),
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
    pub models: Option<Result<Vec<smelt_core::provider_api::DiscoveredModel>, String>>,
}

/// 登录流程当前在等用户回答的那个提问。
#[derive(Clone)]
pub struct PiLoginPrompt {
    pub id: String,
    pub kind: smelt_core::pi_auth::PiPromptKind,
    pub message: String,
    pub placeholder: Option<String>,
    pub options: Vec<smelt_core::pi_auth::PiPromptOption>,
}

/// 一次进行中的（或刚结束的）登录。
///
/// `session` 用 `Arc` 是因为快照会把它复制给渲染层，而它的 `Drop` 会杀掉登录
/// 子进程：面板还开着、渲染却持有最后一个引用被丢掉，用户的授权就断在半路。
#[derive(Clone)]
pub struct PiLoginView {
    pub provider_id: String,
    pub provider_name: String,
    pub session: std::sync::Arc<smelt_core::pi_auth::PiLoginSession>,
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
    pub models: Option<Result<Vec<smelt_core::provider_api::DiscoveredModel>, String>>,
}

impl PiLoginView {
    /// 当前提问该用哪个输入框。
    pub fn active_input(&self) -> Option<&Entity<gpui_component::input::InputState>> {
        match self.prompt.as_ref()?.kind {
            smelt_core::pi_auth::PiPromptKind::Select => None,
            smelt_core::pi_auth::PiPromptKind::Secret => Some(&self.secret_input),
            _ => Some(&self.input),
        }
    }
}

/// 保存 Pi 自定义 provider 时，从各个输入框读出来的纯文本草稿。
///
/// 单独拎出来的理由和 dsh 那份一样：「不填模型」那条路要先去端点问一遍模型，
/// 问完才能落盘，而 `Entity<InputState>` 不能跨 await 带走。
#[derive(Clone)]
pub struct PiCustomProviderDraft {
    pub previous_id: Option<String>,
    pub id: String,
    pub display_name: String,
    pub api: String,
    pub base_url: String,
    /// `None` 表示这次没改凭据，沿用已保存的那份。
    pub api_key: Option<String>,
    pub credential_configured: bool,
    pub set_default: bool,
}

/// 保存自定义 provider 时，从各个输入框读出来的纯文本草稿。
///
/// 单独拎出来是因为「不填模型」那条路要先去端点问一遍模型，问完才能落盘，而
/// `Entity<InputState>` 不能跨 await 带走，也不该在几秒之后再去读。
#[derive(Clone)]
pub struct NativeDshCustomProviderDraft {
    pub previous_id: Option<String>,
    pub id: String,
    pub display_name: String,
    pub api: String,
    pub base_url: String,
    pub api_key_env: String,
    pub api_key: String,
    pub set_default: bool,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeDshModelSettings {
    default_model: NativeDshDefaultModel,
    deepseek: NativeDshDeepSeekSettings,
    credential_configured: bool,
    #[serde(default)]
    custom_providers: Vec<NativeDshCustomProvider>,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeDshDefaultModel {
    provider: String,
    model: String,
    reasoning_effort: String,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeDshDeepSeekSettings {
    // dsh 自己的 settings.yaml 用的就是 `baseURL`，camelCase 规则会算出
    // `baseUrl`，对不上。这里显式钉住助手的实际键名。
    #[serde(rename = "baseURL")]
    base_url: String,
    api_key_env: String,
    reasoning_effort: String,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeDshCustomProvider {
    id: String,
    display_name: String,
    api: String,
    #[serde(rename = "baseURL")]
    base_url: String,
    api_key_env: String,
    credential_configured: bool,
    #[serde(default)]
    models: Vec<NativeDshCustomModel>,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeDshCustomModel {
    id: String,
    name: String,
    context_window: Option<u64>,
    max_tokens: Option<u64>,
}

#[derive(serde::Deserialize)]
struct NativeDshModelSaveResponse {
    ok: bool,
}

/// 画"推理强度"那一栏。
///
/// 独立成函数有两个理由，都不是风格问题：一是设置页那条渲染链已经长到让 rustc
/// 在解析期爆栈，就地再嵌一层 match 直接编译不过；二是这一栏的四种状态是这次
/// 改动的全部要点，摊开在几千行链式调用中间没人看得见。
fn render_dsh_effort_section(
    view: DshEffortView,
    current: String,
    workspace: Entity<Workspace>,
    fg: gpui::Hsla,
    muted: gpui::Hsla,
    danger_fg: gpui::Hsla,
) -> gpui::AnyElement {
    let heading = div()
        .text_xs()
        .font_semibold()
        .text_color(fg)
        .child("推理强度");
    let note = |color: gpui::Hsla, text: String| div().text_xs().text_color(color).child(text);

    let body = match view {
        // 查询要起一个完整 dsh 运行时，有几秒空窗。说一声，别让这一栏凭空出现。
        DshEffortView::Loading => {
            note(muted, "正在读取该模型支持的档位…".to_string()).into_any_element()
        }
        // 说明"没有"，而不是什么都不画：一栏静默消失，用户只会当成界面坏了。
        DshEffortView::Unavailable => note(
            muted,
            "当前模型没有可调的推理强度，将使用助手默认值。".to_string(),
        )
        .into_any_element(),
        DshEffortView::Unknown(error) => note(
            muted,
            format!("未能读取该模型支持的档位（{error}）；保持现有设置不变。"),
        )
        .into_any_element(),
        DshEffortView::Choices {
            efforts,
            default_effort,
        } => {
            // 存着的值不在广告列表里，多半是换过 baseURL 或中转：那条设置照样会
            // 被发出去，所以必须说出来，而不是让按钮全部显示未选中了事。
            let stale = !current.is_empty() && !efforts.iter().any(|effort| effort.id == current);
            let buttons = efforts.into_iter().map(|effort| {
                let entity = workspace.clone();
                let value = effort.id.clone();
                let selected = current == effort.id;
                let label = if default_effort.as_deref() == Some(effort.id.as_str()) {
                    format!("{}（默认）", effort.name)
                } else {
                    effort.name.clone()
                };
                Button::new(format!("dsh-reasoning-{}", effort.id))
                    .secondary()
                    .small()
                    .label(if selected {
                        format!("✓ {label}")
                    } else {
                        label
                    })
                    .on_click(move |_, _, cx| {
                        let value = value.clone();
                        entity.update(cx, |workspace, cx| {
                            workspace.set_native_dsh_reasoning_effort(value, cx);
                        });
                    })
            });
            v_flex()
                .gap_1()
                .child(h_flex().gap_2().children(buttons))
                .children(stale.then(|| {
                    note(
                        danger_fg,
                        format!("已保存的「{current}」不在该模型支持的档位内，请重新选择。"),
                    )
                }))
                .into_any_element()
        }
    };

    v_flex()
        .gap_1()
        .child(heading)
        .child(body)
        .into_any_element()
}

/// 交互式表单目前只覆盖这一条内置路由；自定义 provider 走另一个编辑器。
const NATIVE_DSH_EDITABLE_PROVIDER: &str = "deepseek-official";

/// 能力查询的进度。查询要起一个完整 dsh 进程（约 3-4 秒），不能同步做。
#[derive(Clone)]
pub enum NativeDshCapabilityState {
    Loading,
    Ready(smelt_core::agent_kind::DshModelCapabilities),
    Failed(String),
}

/// 推理强度那一栏该画什么。
///
/// 四种情况分开，是因为对用户它们是四件不同的事：等一下、这条路由没这个旋钮、
/// 没问出来、这些是可选项。以前只有"画四个按钮"一种，于是后三种全被伪装成了
/// 第四种。
#[derive(Clone)]
enum DshEffortView {
    Loading,
    /// 助手明确没有广告推理强度——不是没问到，是这条路由没有这个能力。
    Unavailable,
    /// 问失败了。和 `Unavailable` 分开：一个是答案，一个是没有答案。
    Unknown(String),
    Choices {
        efforts: Vec<smelt_core::agent_kind::DshEffort>,
        default_effort: Option<String>,
    },
}

/// 模型那一栏的候选来源。
///
/// 和 `DshEffortView` 同一套契约：拿不到就说拿不到，绝不编一个候选列表出来。
/// 输入框始终保留——助手广告的是它此刻在提供的路由，用户完全可能想填一个尚未
/// 出现在目录里的模型 ID。
#[derive(Clone)]
enum DshModelChoicesView {
    Loading,
    /// 问到了，但这条 provider 一个模型都没有。
    Unavailable,
    Unknown(String),
    Choices(Vec<smelt_core::agent_kind::DshModelCapability>),
}

/// 画「可选模型」那一排。
///
/// 和 effort 一样抽成自由函数：这条渲染链已经长到让 rustc 在解析期爆栈，就地
/// 再嵌一层 match 编译不过。
fn render_dsh_model_choices_section(
    view: DshModelChoicesView,
    current: String,
    editor_model: Entity<gpui_component::input::InputState>,
    fg: gpui::Hsla,
    muted: gpui::Hsla,
) -> gpui::AnyElement {
    let note = |color: gpui::Hsla, text: String| div().text_xs().text_color(color).child(text);

    let heading = div()
        .text_xs()
        .font_semibold()
        .text_color(fg)
        .child("可选模型");

    let body = match view {
        DshModelChoicesView::Loading => {
            note(muted, "正在读取助手正在提供的模型…".to_string()).into_any_element()
        }
        DshModelChoicesView::Unavailable => note(
            muted,
            "助手当前没有提供可选模型，请直接填写模型 ID。".to_string(),
        )
        .into_any_element(),
        DshModelChoicesView::Unknown(error) => note(
            muted,
            format!("未能读取可选模型（{error}）；请直接填写模型 ID。"),
        )
        .into_any_element(),
        DshModelChoicesView::Choices(models) => {
            let buttons = models.into_iter().map(move |model| {
                let selected = current == model.id;
                let value = model.id.clone();
                let state = editor_model.clone();
                Button::new(format!("dsh-model-choice-{}", model.id))
                    .secondary()
                    .small()
                    .label(if selected {
                        format!("✓ {}", model.label())
                    } else {
                        model.label().to_string()
                    })
                    .on_click(move |_, window, cx| {
                        let value = value.clone();
                        state.update(cx, |state, cx| state.set_value(value, window, cx));
                    })
            });
            v_flex()
                .gap_1()
                .child(h_flex().gap_2().flex_wrap().children(buttons))
                .child(note(
                    muted,
                    "点击填入上方「默认模型」，仍需按「保存」生效。".to_string(),
                ))
                .into_any_element()
        }
    };

    v_flex()
        .gap_1()
        .child(heading)
        .child(body)
        .into_any_element()
}

/// 画自定义 provider 的模型发现结果。
/// 发现结果区块。dsh 和 Pi 共用：两边问的是同一件事，措辞和交互没有理由不同。
///
/// `adopt` 用函数指针而不是在函数里判断是哪一侧——加第三种 harness 时这里不该
/// 再长出一个 match。`id_prefix` 只为让两侧的按钮 id 不撞。
#[allow(clippy::too_many_arguments)]
fn render_model_discovery_section(
    id_prefix: &'static str,
    state: Option<ModelDiscoveryState>,
    adopted: Vec<String>,
    manual_rows: usize,
    workspace: Entity<Workspace>,
    adopt: fn(
        &mut Workspace,
        smelt_core::provider_api::DiscoveredModel,
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
                    .secondary()
                    .small()
                    .label(if taken { format!("✓ {label}") } else { label })
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
fn daemon_info_line(info: &terminal::DaemonInfo) -> String {
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
    dsh_plugin_manager: Option<DshPluginManager>,
    dsh_model_editor: Option<DshModelEditor>,
    dsh_custom_provider_editor: Option<DshCustomProviderEditor>,
    dsh_model_editor_error: Option<String>,
    /// 推理强度栏该画什么；`None` 表示表单没打开。
    dsh_effort_view: Option<DshEffortView>,
    /// 可选模型栏该画什么；`None` 表示表单没打开。
    dsh_model_choices_view: Option<DshModelChoicesView>,
    native_dsh_model_settings: Option<Result<NativeDshModelSettings, String>>,
    dsh_settings: Result<smelt_core::agent_kind::DshSettingsSummary, String>,
    pi_model_editor: Option<PiModelEditor>,
    pi_custom_provider_editor: Option<PiCustomProviderEditor>,
    /// Pi 内置 provider 的登录状态；`None` 表示这台机器上还没问过。
    pi_auth_providers: Option<PiAuthProvidersState>,
    pi_login: Option<PiLoginView>,
    pi_auth_model_picker: Option<PiAuthModelPicker>,
    /// 注销、刷新这类一次性操作的最近一次错误。
    pi_auth_error: Option<String>,
    pi_auth_show_all: bool,
    pi_model_editor_error: Option<String>,
    pi_model_settings: Result<smelt_core::pi_model_settings::PiModelSettings, String>,
    pi_settings: Result<smelt_core::pi_model_settings::PiSettingsSummary, String>,
    pi_plugins: Vec<smelt_core::pi_plugin_catalog::PiPlugin>,
    pi_plugin_pending_delete: Option<String>,
    pi_plugin_error: Option<String>,
    /// 插件 id -> 勾选了它的智能体名字，用来在删除前提示占用方。
    agent_names_using_plugin: std::collections::HashMap<String, Vec<String>>,
    update_status: updater::UpdateStatus,
    daemon_outdated: Option<bool>,
    daemon_upgrading: bool,
    daemon_upgrade_msg: Option<String>,
    daemon_info: Option<terminal::DaemonInfo>,
    settings_section: SettingsSection,
    settings_page_nonce: usize,
    show_daemon_restart_confirm: bool,
    session_manager_open: bool,
}

/// 设置窗口的内容子视图。将它与外层的键盘/性能 HUD 分开，HUD 的逐帧采样不会重建
/// 全部 Settings 元素，也不会重新布局二维码和设置侧栏。
pub struct SettingsContentView {
    workspace: Entity<Workspace>,
    /// 本窗口显示哪一档内容。同一个 Workspace 可以同时被多扇作用域不同的窗口观察。
    scope: SettingsScope,
    snapshot: SettingsRenderSnapshot,
    _observe_workspace: Subscription,
    _observe_settings_globals: Vec<Subscription>,
    last_refresh: Option<Instant>,
    refresh_pending: bool,
    refresh_generation: u64,
}

impl SettingsContentView {
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
        // 子菜单切换必须立刻生效，不能被设置页 250ms 刷新节流挡住。
        let current_section = self.workspace.read(cx).settings_section.clone();
        if current_section != self.snapshot.settings_section {
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 设置内容 +（可选）守护管理弹层：弹层必须画在本窗，不能只改 Workspace 上的
        // flag 却在主窗口 render——用户点的是设置里的按钮，弹窗却跑到主界面。
        let snapshot = self.snapshot.clone();
        let workspace = self.workspace.clone();
        let show_daemon_restart_confirm = snapshot.show_daemon_restart_confirm;
        let session_manager_open = snapshot.session_manager_open;

        div()
            .relative()
            .size_full()
            .child(Workspace::render_settings_content(
                workspace.clone(),
                self.scope,
                &snapshot,
                cx,
            ))
            // 弹层开启期间才需要读它的动态会话数据；正常浏览设置页不会因此订阅整个
            // Workspace 的高频状态通知。
            .children(
                show_daemon_restart_confirm
                    .then(|| workspace.update(cx, |ws, cx| ws.render_daemon_restart_confirm(cx))),
            )
            .children(
                session_manager_open
                    .then(|| workspace.update(cx, |ws, cx| ws.render_session_manager(cx))),
            )
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
    /// 设置窗口独立采样，不能复用主工作区的 FPS：两个窗口的布局负载不同。
    debug_hud: bool,
    last_frame: Option<Instant>,
    fps_ema: f32,
    debug_mem_rss: Option<u64>,
    debug_mem_sampled_at: Option<Instant>,
}

impl SettingsWindow {
    fn toggle_debug_hud(&mut self, cx: &mut Context<Self>) {
        self.debug_hud = !self.debug_hud;
        self.fps_ema = 0.0;
        self.last_frame = None;
        self.debug_mem_rss = None;
        self.debug_mem_sampled_at = None;
        cx.notify();
    }

    fn update_debug_hud(&mut self, window: &mut Window) {
        if !self.debug_hud {
            self.last_frame = None;
            self.debug_mem_rss = None;
            self.debug_mem_sampled_at = None;
            return;
        }

        let now = Instant::now();
        if let Some(previous) = self.last_frame {
            let delta = now.saturating_duration_since(previous).as_secs_f32();
            if delta > 0.0 {
                let instantaneous_fps = 1.0 / delta;
                self.fps_ema = if self.fps_ema <= 0.0 {
                    instantaneous_fps
                } else {
                    self.fps_ema * 0.9 + instantaneous_fps * 0.1
                };
            }
        }
        self.last_frame = Some(now);

        if self.debug_mem_sampled_at.is_none_or(|sampled_at| {
            now.saturating_duration_since(sampled_at) >= Duration::from_secs(1)
        }) {
            self.debug_mem_rss = crate::mem_usage::current_rss_bytes();
            self.debug_mem_sampled_at = Some(now);
        }
        window.request_animation_frame();
    }

    fn render_debug_hud(&self) -> Div {
        let fps = self.fps_ema;
        let frame_ms = if fps > 0.0 { 1000.0 / fps } else { 0.0 };
        let memory = self
            .debug_mem_rss
            .map(crate::mem_usage::format_rss)
            .unwrap_or_else(|| "-".into());
        let color = if fps >= 55.0 {
            rgb(crate::ui_theme::green())
        } else if fps >= 30.0 {
            rgb(crate::ui_theme::yellow())
        } else {
            rgb(crate::ui_theme::red())
        };

        div()
            .absolute()
            .top(px(40.))
            .right(px(12.))
            .px_2()
            .py_1()
            .rounded_md()
            .bg(crate::ui_theme::tint(crate::ui_theme::bg_card(), 0xcc))
            .border_1()
            .border_color(crate::ui_theme::overlay(0x22))
            .font_family(terminal_view::font_family())
            .text_xs()
            .text_color(color)
            .child(format!("{fps:.0} FPS · {frame_ms:.1} ms · RSS {memory}"))
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.did_focus {
            self.did_focus = true;
            window.focus(&self.focus_handle, cx);
        }
        self.update_debug_hud(window);
        let debug_hud = self.debug_hud.then(|| self.render_debug_hud());

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

/// 各作用域设置窗口的单例句柄：已经开着就聚焦复用，避免重复开出好几扇一样的窗口。
#[derive(Default)]
pub struct SettingsWindowHandles(pub std::collections::HashMap<SettingsScope, WindowHandle<Root>>);
impl Global for SettingsWindowHandles {}

// ===================== Workspace 方法 =====================

#[cfg(test)]
mod tests;
