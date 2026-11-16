//! 设置：主窗口里单独一页（[`SETTINGS_PAGE`]），两栏：左边是导航视图
//! （[`SETTINGS_NAV_VIEW`]，分组、搜索），右边是内容视图（[`SETTINGS_VIEW`]，一页一页的
//! 设置）。一页只画那一页，页内再分节。两个视图都看工作台的 `settings_section`：导航改它，
//! 内容按它画。
//!
//! 跟 agents 同一个套路：从 main.rs 拆出来的 `impl Workspace`
//! 方法 + 独立类型/函数，字段仍然声明在 main.rs 的 `Workspace` struct 里。
//!
//! 「系统」组的页都在内置插件里：「关于」在「更新与帮助」（`smelt-updater-ui`），终端里的
//! smelt 命令在「维护」（`smelt-maintenance`）。

use std::time::{Duration, Instant};

use gpui::InteractiveElement;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_component::*;
use smelt_ui::IconAction as _;
pub(crate) use smelt_ui::{Button, Switch};

use crate::window_backdrop::WindowMaterial;
use crate::{Workspace, terminal, terminal_view};

mod agent_hooks;
mod bundled_helpers;
mod nav;
mod panel_plugins;
mod quota;
mod remote;
mod workspace;
mod workspace_view;

pub(crate) use nav::{SettingsSection, filter_settings_nav, settings_nav};
pub(crate) use quota::refresh_quota_settings;
pub(super) use quota::{QuotaSettingsState, set_quota_enabled};
pub(super) use remote::*;

#[cfg(test)]
pub(super) use remote::qr_png_for_url;

pub(super) use agent_hooks::{
    AgentHooksCache, install_agent_hooks, reconcile_agent_hooks, refresh_agent_hooks_status,
    uninstall_agent_hooks,
};
pub(super) use bundled_helpers::sync_bundled_smelt_notify;
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

fn default_bg_image_opacity() -> f32 {
    smelt_model::appearance_settings::DEFAULT_BG_IMAGE_OPACITY
}

/// 窗口与终端外观设置（全局单例；存 SQLite KV）。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Appearance {
    /// 背景图片绝对路径（None = 无）。
    pub bg_image: Option<String>,
    /// 背景图片透明度 0–1（0=全透，1=全不透明）。默认低透明度做装饰层，
    /// 让主题底色透出，避免明亮图片直接铺满破坏可读性（对齐
    /// WezTerm / Windows Terminal 的 backgroundImageOpacity 惯例）。
    #[serde(default = "default_bg_image_opacity")]
    pub bg_image_opacity: f32,
    /// 明暗。跟着 `theme_name` 走，CLI 的 `appearance.theme_mode` 会把主题拨到同一族的另一侧。
    #[serde(default = "default_theme_mode")]
    pub theme_mode: ThemeMode,
    /// 主题名，对应注册表里的 `name`，例如 `smelt Dark`、`Ayu Light`。
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
    /// 选的是哪份布局（布局 id）。`None` 是没选过，用第一个登记的布局。选的布局暂时没登记
    /// （插件还没加载、被卸了）时照样留着，不改写成别的。
    #[serde(default)]
    pub layout_id: Option<String>,
    /// 窗口底下垫哪一种系统材质。只存在客户端库里，不进旧的 JSON 形式。
    #[serde(skip, default = "WindowMaterial::platform_default")]
    pub material: WindowMaterial,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            bg_image: None,
            bg_image_opacity: default_bg_image_opacity(),
            theme_mode: ThemeMode::Dark,
            theme_name: default_theme_name(),
            ui_font_px: default_ui_font_px(),
            ui_font_family: String::new(),
            font_px: default_font_px(),
            font_family: String::new(),
            layout_id: None,
            material: WindowMaterial::platform_default(),
        }
    }
}

impl Global for Appearance {}

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

/// 不同插件可以使用相同的族名，明暗切换只能在同一个来源内寻找。
fn scoped_theme_family(owner: Option<&str>, family: &str) -> String {
    match owner {
        Some(owner) => format!("plugin:{}:{owner}{family}", owner.len()),
        None => format!("builtin:{family}"),
    }
}

/// 当前桌面的主题目录。运行时没有桌面插件，主题名和同族明暗都在这里解析。
fn registered_theme_entries(cx: &App) -> Vec<smelt_model::theme_index::ThemeEntry> {
    smelt_ui::theme_catalog::themes(cx)
        .into_iter()
        .map(|theme| smelt_model::theme_index::ThemeEntry {
            name: theme.name.to_string(),
            mode: if theme.mode.is_dark() {
                "dark"
            } else {
                "light"
            }
            .into(),
            family: scoped_theme_family(theme.owner.as_ref().map(|owner| owner.as_ref()), &theme.family),
        })
        .collect()
}

/// CLI / Control API 经运行时转来的外观改动：由当前桌面的主题目录校验，
/// 存进客户端库并整份重新安装。
pub fn apply_requested_appearance(
    settings: &std::collections::BTreeMap<String, serde_json::Value>,
    cx: &mut App,
) -> Result<(), smelt_model::appearance_settings::SettingsError> {
    let store = smelt_client_store::client_store()
        .map_err(|error| smelt_model::appearance_settings::SettingsError::Store(format!("打开客户端库失败：{error}")))?;
    apply_requested_appearance_on(settings, store, cx)
}

fn apply_requested_appearance_on(
    settings: &std::collections::BTreeMap<String, serde_json::Value>,
    store: &smelt_client_store::ClientStore,
    cx: &mut App,
) -> Result<(), smelt_model::appearance_settings::SettingsError> {
    let current = cx.global::<Appearance>().clone();
    let mut snapshot = snapshot_from_appearance(&current);
    let catalog = registered_theme_entries(cx);
    smelt_model::appearance_settings::apply_patch(&mut snapshot, settings, &catalog)?;
    let next = appearance_from_snapshot(snapshot, current);
    save_appearance_on(&next, store)
        .map_err(|error| smelt_model::appearance_settings::SettingsError::Store(format!("保存外观失败：{error}")))?;
    install_appearance(next, cx);
    Ok(())
}

/// 把一份完整外观套到所有层并重绘。外观的真源是客户端库，内存里的全局只是它的投影。
fn install_appearance(appearance: Appearance, cx: &mut App) {
    terminal_view::set_font_px(appearance.font_px);
    terminal_view::set_font_family(&appearance.font_family);
    cx.set_global(appearance.clone());
    apply_named_theme(&appearance.theme_name, cx);
    cx.refresh_windows();
}

/// 按主题名装上配色。组件库走 `apply_config`，自绘色板从这份主题投影，终端跟着明暗切。
/// 字色按外观里选的窗口材质配（见 `ui_theme::fit_text_to_backdrop`），所以换了材质也要
/// 重装一次。字号和字体在装完之后写回 Appearance，主题文件里的字号不覆盖用户设置。
/// 只改全局态不重绘，调用方自己决定什么时候 `cx.refresh_windows()`。
pub fn apply_named_theme(name: &str, cx: &mut App) {
    let appearance = cx.try_global::<Appearance>();
    let requested_dark = appearance
        .map(|appearance| appearance.theme_mode.is_dark())
        .unwrap_or(true);
    let material = appearance
        .map(|appearance| appearance.material)
        .unwrap_or_else(WindowMaterial::platform_default);
    crate::ui_theme::set_window_material(crate::window_backdrop::text_tone(material));
    let applied = smelt_ui::theme_catalog::apply(name, cx)
        || smelt_ui::theme_catalog::apply(
            smelt_model::theme_index::default_for_mode(if requested_dark {
                "dark"
            } else {
                "light"
            }),
            cx,
        );
    let dark = if applied && cx.has_global::<Theme>() {
        cx.global::<Theme>().is_dark()
    } else {
        crate::ui_theme::set_light(!requested_dark);
        requested_dark
    };
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
/// 主题变化时要调（见 `apply_named_theme`）。
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
        Ok(Some(record)) => appearance_from_snapshot(
            smelt_model::appearance_settings::AppearanceSnapshot {
                bg_image: record.bg_image,
                bg_image_opacity: record.bg_image_opacity,
                theme_mode: record.theme_mode,
                theme_name: record.theme_name,
                ui_font_px: record.ui_font_px,
                ui_font_family: record.ui_font_family,
                font_px: record.font_px,
                font_family: record.font_family,
            },
            Appearance {
                layout_id: record.layout_id,
                material: WindowMaterial::from_key(&record.material),
                ..Appearance::default()
            },
        ),
        Ok(None) => Appearance::default(),
        Err(error) => {
            eprintln!("[storage] 读取外观设置失败，使用默认值: {error}");
            Appearance::default()
        }
    }
}

fn save_appearance(appearance: &Appearance) {
    if let Err(error) = smelt_client_store::client_store()
        .and_then(|store| save_appearance_on(appearance, store))
    {
        eprintln!("[storage] 保存外观设置失败: {error}");
    }
}

fn save_appearance_on(
    appearance: &Appearance,
    store: &smelt_client_store::ClientStore,
) -> Result<(), smelt_client_store::ClientStoreError> {
    let snapshot = snapshot_from_appearance(appearance);
    let record = smelt_client_store::AppearanceRecord {
        bg_image: snapshot.bg_image,
        bg_image_opacity: snapshot.bg_image_opacity,
        theme_mode: snapshot.theme_mode,
        theme_name: snapshot.theme_name,
        ui_font_px: snapshot.ui_font_px,
        ui_font_family: snapshot.ui_font_family,
        font_px: snapshot.font_px,
        font_family: snapshot.font_family,
        layout_id: appearance.layout_id.clone(),
        material: appearance.material.key().to_string(),
    };
    store.put_appearance(&record)
}

fn snapshot_from_appearance(
    appearance: &Appearance,
) -> smelt_model::appearance_settings::AppearanceSnapshot {
    smelt_model::appearance_settings::AppearanceSnapshot {
        bg_image: appearance.bg_image.clone(),
        bg_image_opacity: appearance.bg_image_opacity,
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

/// 快照只有能脚本改的那些键；别的（选的布局、材质）照抄 `base`。
fn appearance_from_snapshot(
    snapshot: smelt_model::appearance_settings::AppearanceSnapshot,
    base: Appearance,
) -> Appearance {
    Appearance {
        bg_image: snapshot.bg_image,
        bg_image_opacity: snapshot.bg_image_opacity,
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
        ..base
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
        | TerminalAgentKind::OhMyPi
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
// ConversationAgentKind / AcpProfile 在 smelt-model（本身不需要 GPUI），按机器的
// AgentHosts（需要 `gpui::Global`）在 smelt-ui——都是 acp_view.rs 独立成
// smelt-acp-view crate 之后要跨 crate 共用的数据模型。这里重导出成裸名字。
//
// 智能体配置（启动命令、env、profile、定义）归运行时，每台机器一份：读、改都要说明是
// 哪台机器。会话按它所在的运行时取，设置页眼下只配本机。
pub use smelt_app_state::agent_host_state::{
    AgentDefinition, AgentHosts, agent_host_loaded, agent_host_settled, agent_hosts,
    forget_agent_host,
};
pub use smelt_model::agent_host::AgentHostSettings;
pub use smelt_model::agent_kind::{
    AcpProfile, ConversationAgentKind, HistorySourceKind, TerminalAgentKind,
};
use smelt_model::agent_kind::{ConversationLaunchSpec, format_env_lines, parse_env_lines};

/// 那台机器推来的智能体配置（话题 `agent_host.changed`）：整份换上缓存。积压丢了事件不用管，
/// 后面跟着一份快照；订阅被拒（运行时太旧）就记成给不了。
pub fn receive_agent_host_topic(
    runtime: &crate::RuntimeKey,
    message: smelt_client::daemon_state::TopicMessage,
    cx: &mut App,
) {
    use smelt_app_state::agent_host_state::{agent_host_unavailable, receive_agent_host};
    use smelt_client::daemon_state::TopicMessage;

    match message {
        TopicMessage::Event { payload, .. } | TopicMessage::Snapshot { payload, .. } => {
            match serde_json::from_value::<AgentHostSettings>(payload) {
                Ok(settings) => receive_agent_host(runtime, settings, cx),
                Err(error) => {
                    eprintln!("[agent-host] {runtime:?} 推来的智能体配置解不开：{error}");
                    agent_host_unavailable(runtime, format!("智能体配置解不开：{error}"), cx);
                }
            }
        }
        TopicMessage::Lagged => {}
        TopicMessage::Unavailable(reason) => agent_host_unavailable(runtime, reason, cx),
    }
}

/// 改那台机器的智能体配置（宿主自己的设置页用）：发给它的运行时，改完由它推回来；没改上
/// 就报给人看。
pub fn edit_agent_host(
    runtime: &crate::RuntimeKey,
    f: impl FnOnce(&mut AgentHostSettings) + 'static,
    cx: &mut App,
) {
    let edit = smelt_plugin_api::edit_agent_settings(runtime, f, cx);
    cx.spawn(async move |_| {
        if let Err(error) = edit.await {
            crate::status_item::notify_error(format!("保存智能体配置失败：{error}"));
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

/// 那台机器上某个 agent 的完整启动规格（命令 + 用户配的环境变量）。
///
/// 新建会话一律走这里。只取出厂命令再自己 `from_command` 会把环境变量
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

/// 按钮的短暂成功文案（设置页复制、自动化保存等读它改按钮字）。
#[derive(Clone, Default)]
struct CopyFlash {
    id: String,
    label: String,
    until: Option<Instant>,
}

impl Global for CopyFlash {}

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
    cx.set_global(edited);
    sync_theme_fonts(cx);
}

/// 换窗口材质：存进外观，再按新材质重装主题（字色按材质配）。窗口上的材质由工作台在
/// 外观变化时换（`Workspace::sync_window_backdrop`）。
pub(crate) fn set_window_material(material: WindowMaterial, cx: &mut App) {
    apply_appearance(move |a| a.material = material, cx);
    let theme_name = cx.global::<Appearance>().theme_name.clone();
    apply_named_theme(&theme_name, cx);
    cx.refresh_windows();
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

/// 自定义 Agent 列表编辑器：每项一组名称 / 启动命令 / 环境变量 / 数据目录输入框；
/// 类型走下拉选择（离散值，不需要输入框），选完直接存盘。
pub struct ProfileInputs {
    rows: Vec<ProfileRow>,
    _subs: Vec<Subscription>,
}

/// 设置页上一个自定义 Agent 的输入框。环境变量写不对时不存盘，错误挂在这里给页面显示。
#[derive(Clone)]
pub struct ProfileRow {
    pub(crate) id: String,
    /// 建这一行时的类型：输入框的占位字跟着类型走，换了类型要重建。
    pub(crate) kind_id: String,
    pub(crate) label: Entity<gpui_component::input::InputState>,
    pub(crate) command: Entity<gpui_component::input::InputState>,
    pub(crate) env: Entity<gpui_component::input::TextareaState>,
    pub(crate) dir: Entity<gpui_component::input::InputState>,
    pub(crate) env_error: Option<String>,
}

/// Workspace 同时承载终端、任务和动画，通知频率会远高于设置页所需。设置主体只取
/// 最终状态，因此将连续更新合并到这一间隔；本地输入控件仍由各自的实体即时刷新。
const SETTINGS_CONTENT_REFRESH_MIN_INTERVAL: Duration = Duration::from_millis(250);

/// 设置页在渲染前从 Workspace 取出的快照。不能在 `Render::render` 中直接访问
/// Workspace：终端流式输出等高频通知会让 GPUI 认为整张设置页都依赖它，从而每帧
/// 失效缓存。可交互控件仍保留实体句柄，点击回调再回到 Workspace 执行。
#[derive(Clone)]
struct SettingsRenderSnapshot {
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
    profile_rows: Vec<ProfileRow>,
    panel_plugins: PanelPluginsSnapshot,
    settings_section: SettingsSection,
    settings_page_nonce: usize,
}

/// 设置内容视图的 id，打开设置就是打开它。宿主的视图，和插件的视图在同一张表里；只摆在
/// 设置那一页（[`SETTINGS_PAGE`]），打开它就是去那一页。
pub(crate) const SETTINGS_VIEW: &str = "smelt/settings";

/// 设置导航视图的 id，摆在设置那一页的左栏。
const SETTINGS_NAV_VIEW: &str = "smelt/settings-nav";

/// 设置那一页的 id。
const SETTINGS_PAGE: &str = "smelt/settings";

/// 设置那一页的布局：左右两栏，左边导航、右边内容，中间的分隔条能拖。
const SETTINGS_LAYOUT: &str = r#"{
  "version": 1,
  "center": {
    "panel_name": "StackPanel",
    "info": { "stack": { "sizes": [220, 1000], "axis": 0 } },
    "children": [
      {
        "panel_name": "TabPanel",
        "info": { "tabs": { "active_index": 0 } },
        "children": [{ "panel_name": "smelt/settings-nav", "children": [], "info": { "panel": null } }]
      },
      {
        "panel_name": "TabPanel",
        "info": { "tabs": { "active_index": 0 } },
        "children": [{ "panel_name": "smelt/settings", "children": [], "info": { "panel": null } }]
      }
    ]
  }
}"#;

/// 设置那一页和它的布局登记进页、布局注册表；设置的两个视图由 [`view_specs`] 交给
/// `host_views` 和宿主别的视图一起登记。没变就不动。
pub(crate) fn register_page(cx: &mut App) {
    let page = smelt_plugin_api::PageSource {
        name: "settings",
        title: "设置",
        home: false,
        index: None,
    };
    if let Err(error) = smelt_plugin_api::register_pages(crate::host_views::HOST_OWNER, &[page], cx)
    {
        eprintln!("[plugins] 设置页登记失败：{error}");
    }
    let layout = smelt_plugin_api::LayoutSource {
        page: SETTINGS_PAGE,
        name: "settings",
        title: "设置",
        state: SETTINGS_LAYOUT,
    };
    if let Err(error) =
        smelt_plugin_api::register_layouts(crate::host_views::HOST_OWNER, &[layout], cx)
    {
        eprintln!("[plugins] 设置页的布局登记失败：{error}");
    }
}

/// 设置的两个视图：导航和内容。
pub(crate) fn view_specs(workspace: WeakEntity<Workspace>) -> [smelt_plugin_api::ViewSpec; 2] {
    let nav = workspace.clone();
    [
        smelt_plugin_api::ViewSpec {
            name: "settings-nav".into(),
            title: "设置导航".into(),
            icon: gpui_kit_assets::IconName::Settings.path(),
            per_machine: false,
            // 贴着窗口顶，顶上那条（拖窗口、给红黄绿按钮让位、标题）由页头画。
            draws_top_bar: true,
            settings: None,
            build: std::rc::Rc::new(
                move |window, cx| -> std::rc::Rc<dyn smelt_plugin_api::ViewHandle> {
                    let workspace = nav.clone();
                    std::rc::Rc::new(cx.new(|cx| SettingsNavView::new(workspace, window, cx)))
                },
            ),
        },
        smelt_plugin_api::ViewSpec {
            name: "settings".into(),
            title: "设置".into(),
            icon: gpui_kit_assets::IconName::Settings.path(),
            per_machine: false,
            // 贴着窗口顶，顶上那条（拖窗口、关闭按钮）由页头画。
            draws_top_bar: true,
            settings: None,
            build: std::rc::Rc::new(
                move |window, cx| -> std::rc::Rc<dyn smelt_plugin_api::ViewHandle> {
                    let workspace = workspace.clone();
                    std::rc::Rc::new(cx.new(|cx| SettingsContentView::new(workspace, window, cx)))
                },
            ),
        },
    ]
}

/// 设置导航：搜索框和按组列出的各页，点一页就停到那一页（改工作台的 `settings_section`）。
/// 选中哪页、哪页有要处理的事随工作台的通知和机器状态的变化比一下，变了才重画：工作台
/// 通知很密（终端、spinner），不跟着每次都重画。
struct SettingsNavView {
    workspace: WeakEntity<Workspace>,
    search: Entity<gpui_component::input::InputState>,
    selected: SettingsSection,
    attention: Option<SettingsSection>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsNavView {
    fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx).placeholder("搜索设置、功能或命令")
        });
        let mut subscriptions = vec![
            cx.subscribe(&search, |_, _, event, cx| {
                if matches!(event, gpui_component::input::InputEvent::Change) {
                    cx.notify();
                }
            }),
            // 插件的设置页登记、撤下了，导航跟着变。
            cx.subscribe(
                &smelt_plugin_api::events(cx),
                |_, _, event: &smelt_plugin_api::HostEvent, cx| {
                    if matches!(event, smelt_plugin_api::HostEvent::ViewsChanged) {
                        cx.notify();
                    }
                },
            ),
        ];
        if let Some(host) = workspace.upgrade() {
            subscriptions.push(cx.observe(&host, |this: &mut Self, _, cx| this.sync(cx)));
        }
        // 机器断了、慢了不一定通知工作台，远程那页的红点直接跟它。
        subscriptions.push(
            cx.observe_global::<crate::runtimes::RuntimeHub>(|this: &mut Self, cx| this.sync(cx)),
        );
        // 建它的地方（打开设置、装布局）正更新着工作台，这时读不了，推后一拍再读。
        cx.defer_in(window, |this, _, cx| this.sync(cx));
        Self {
            workspace,
            search,
            selected: SettingsSection::default(),
            attention: None,
            _subscriptions: subscriptions,
        }
    }

    /// 重读选中的那页、有要处理的事的那页，变了才重画。只读内存。
    fn sync(&mut self, cx: &mut Context<Self>) {
        let Some(host) = self.workspace.upgrade() else {
            return;
        };
        let workspace = host.read(cx);
        let selected = workspace.settings_section.clone();
        let attention = workspace.settings_attention(cx);
        if selected != self.selected || attention != self.attention {
            self.selected = selected;
            self.attention = attention;
            cx.notify();
        }
    }
}

impl Render for SettingsNavView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 顶上只是标题栏那一行：红黄绿按钮、拖窗口。侧栏从上到下是一整块，不画分隔线；
        // 页名在右边内容的标题行里。
        let header = self
            .workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).page_chrome_of(SETTINGS_NAV_VIEW, window))
            .map(|header| header.border_b_0());
        let nav = self.workspace.upgrade().map(|workspace| {
            Workspace::render_settings_nav(
                workspace,
                &self.selected,
                self.attention.as_ref(),
                &self.search,
                cx,
            )
        });
        v_flex()
            .size_full()
            .min_h_0()
            .children(header)
            .children(nav)
    }
}

impl smelt_plugin_api::PluginView for SettingsNavView {
    /// 停在哪一页由工作台的 `settings_section` 定，参数没有用处。
    fn open(&mut self, _: &serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}

/// 设置内容：导航选中的那一页。设置快照和终端等高频通知隔开。
pub struct SettingsContentView {
    /// 弱引用：视图记在全局的视图注册表里，不能替关掉的主窗口留着工作台。
    workspace: WeakEntity<Workspace>,
    /// 第一次打开时在事件里取（见 [`Self::prepare`]）；建视图时工作台可能正被更新着，
    /// 取不了。
    snapshot: Option<SettingsRenderSnapshot>,
    _observe_workspace: Option<Subscription>,
    _views: Subscription,
    /// 视图放掉（登记被替换）时，露着的插件设置页随之清空。
    _clear_shown_page: Subscription,
    _observe_settings_globals: Vec<Subscription>,
    last_refresh: Option<Instant>,
    refresh_pending: bool,
    refresh_generation: u64,
}

impl SettingsContentView {
    fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            _clear_shown_page: cx
                .on_release(|_, cx| crate::plugin_host::set_settings_shown(None, cx)),
            _views: cx.subscribe_in(
                &smelt_plugin_api::events(cx),
                window,
                |this: &mut SettingsContentView, _, event, window, cx| {
                    if matches!(event, smelt_plugin_api::HostEvent::ViewsChanged) {
                        this.sync_plugin_pages(window, cx);
                        cx.notify();
                    }
                },
            ),
            _observe_workspace: workspace.upgrade().map(|workspace| {
                cx.observe_in(
                    &workspace,
                    window,
                    |this: &mut SettingsContentView, _, window, cx| {
                        this.request_refresh(window, cx);
                    },
                )
            }),
            workspace,
            snapshot: None,
            _observe_settings_globals: vec![
                Self::observe_settings_global::<Appearance>(window, cx),
                Self::observe_settings_global::<LaunchConfig>(window, cx),
                Self::observe_settings_global::<AgentHosts>(window, cx),
                Self::observe_settings_global::<AcpRuntimeState>(window, cx),
                Self::observe_settings_global::<CopyFlash>(window, cx),
                Self::observe_settings_global::<RemoteConfig>(window, cx),
                Self::observe_settings_global::<RemoteSettingsState>(window, cx),
                Self::observe_settings_global::<QuotaSettingsState>(window, cx),
                Self::observe_settings_global::<crate::runtimes::RuntimeHub>(window, cx),
                Self::observe_settings_global::<crate::keymap::KeymapState>(window, cx),
            ],
            last_refresh: None,
            refresh_pending: false,
            refresh_generation: 0,
        }
    }

    /// 每次打开设置都重读一遍只在设置里显示、平时不盯着的状态（hooks 接入、远程访问、
    /// 额度、远程机器的连通），再取快照。
    fn prepare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        refresh_agent_hooks_status(cx);
        refresh_remote(cx);
        refresh_quota_settings(cx);
        crate::runtime_events::probe_remotes(cx);
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        if workspace.read(cx).settings_section == SettingsSection::Remote {
            // 延迟平时只在连接换路径时读；停在远程页时重读一次，看到的是现在的数。
            crate::runtimes::sample_links(cx);
        }
        if cx
            .try_global::<AcpRuntimeState>()
            .is_some_and(|state| state.diagnostics.is_none() && !state.refreshing)
        {
            workspace.update(cx, |workspace, cx| workspace.refresh_acp_runtime(cx));
        }
        self.refresh_snapshot(window, cx);
    }

    /// 告诉插件设置停在哪个插件页上。快照换了页时调：快照停在哪页，画的就是哪页。设置
    /// 视图本身露没露着由工作台报，两样并起来才是这一页看不看得见（见 `ShownViews`）。
    fn publish_shown_page(&self, cx: &mut App) {
        let page = match self
            .snapshot
            .as_ref()
            .map(|snapshot| &snapshot.settings_section)
        {
            Some(SettingsSection::Plugin(id)) => Some(id.clone()),
            _ => None,
        };
        crate::plugin_host::set_settings_shown(page, cx);
    }

    /// 当前停在插件的设置页上而它还没建就建，建好打开一次。建好的那份记在视图注册表里，
    /// 登记换了或撤下跟着放掉。快照换了、视图登记变了时调。
    fn sync_plugin_pages(&mut self, window: &mut Window, cx: &mut App) {
        let Some(SettingsSection::Plugin(id)) = self
            .snapshot
            .as_ref()
            .map(|snapshot| &snapshot.settings_section)
        else {
            return;
        };
        if !smelt_plugin_api::view(id, cx).is_some_and(|view| view.settings.is_some()) {
            return;
        }
        if let Some((view, true)) = smelt_plugin_api::ensure_view(id, window, cx) {
            view.open(&serde_json::Value::Null, window, cx);
        }
    }

    /// 停着的那一页的名字，和导航里写的一样。只读内存。
    fn page_title(&self, cx: &App) -> Option<SharedString> {
        let selected = &self.snapshot.as_ref()?.settings_section;
        settings_nav(&smelt_plugin_api::views(cx))
            .into_iter()
            .flat_map(|category| category.children)
            .find(|leaf| &leaf.section == selected)
            .map(|leaf| leaf.title.into())
    }

    /// 当前停着的插件设置页，建好了才有。只读内存。
    fn plugin_page(&self, cx: &App) -> Option<AnyView> {
        let SettingsSection::Plugin(id) = &self.snapshot.as_ref()?.settings_section else {
            return None;
        };
        smelt_plugin_api::view(id, cx)?.settings?;
        Some(smelt_plugin_api::built_view(id, cx)?.view())
    }

    fn observe_settings_global<G: Global + 'static>(
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.observe_global_in::<G>(window, |this, window, cx| this.request_refresh(window, cx))
    }

    fn refresh_snapshot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(snapshot) = self.workspace.upgrade().map(|workspace| {
            workspace.update(cx, |workspace, cx| {
                workspace.ensure_appearance_controls(cx);
                workspace.ensure_launch_inputs(window, cx);
                workspace.ensure_profile_inputs(window, cx);
                workspace.settings_render_snapshot(cx)
            })
        }) else {
            return;
        };
        self.snapshot = Some(snapshot);
        self.sync_plugin_pages(window, cx);
        self.publish_shown_page(cx);
        self.last_refresh = Some(Instant::now());
        self.refresh_pending = false;
        self.refresh_generation = self.refresh_generation.wrapping_add(1);
        cx.notify();
    }

    fn request_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 还没打开过：打开时才取第一份快照。
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        // 子菜单切换必须立刻生效，不能被设置页 250ms 刷新节流挡住。
        if self.workspace.upgrade().is_some_and(|workspace| {
            workspace.read(cx).settings_section != snapshot.settings_section
        }) {
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
        // 工具栏就是标题栏那一行，只写这一页的名字（返回在左边导航的最上面）。Agent 页改的
        // 都是本机运行时上的东西（CLI、hooks、启动命令），挂了远程机器时在名字旁注明。
        let local_only = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.settings_section == SettingsSection::Agents)
            && crate::runtimes::has_remote_runtimes(cx);
        let header = self
            .workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).page_chrome_of(SETTINGS_VIEW, window))
            .unwrap_or_else(div)
            .gap_2()
            .children(self.page_title(cx).map(|title| {
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_medium()
                    .text_color(rgb(crate::ui_theme::text()))
                    .child(title)
            }))
            .when(local_only, |header| {
                header.child(crate::runtimes::local_runtime_scope_chip())
            });
        let body = match (self.workspace.upgrade(), &self.snapshot) {
            (Some(workspace), Some(snapshot)) => {
                Workspace::render_settings_content(workspace, snapshot, self.plugin_page(cx), cx)
                    .into_any_element()
            }
            _ => div().into_any_element(),
        };
        v_flex()
            .size_full()
            .min_h_0()
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}

impl smelt_plugin_api::PluginView for SettingsContentView {
    /// 停在哪一页由工作台的 `settings_section` 定，参数没有用处。在事件里推后一拍做：
    /// 打开它的地方往往正更新着工作台，这里要回头取工作台的快照。
    fn open(&mut self, _: &serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        cx.defer_in(window, |this, window, cx| this.prepare(window, cx));
    }
}

// ===================== Workspace 方法 =====================

#[cfg(test)]
mod tests;
