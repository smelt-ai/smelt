//! 设置：主窗口里单独一页（[`SETTINGS_PAGE`]），两栏：左边是导航视图
//! （[`SETTINGS_NAV_VIEW`]，分组、搜索），右边是内容视图（[`SETTINGS_VIEW`]，一页一页的
//! 设置）。一页只画那一页，页内再分节。两个视图都看工作台的 `settings_section`：导航改它，
//! 内容按它画。
//!
//! 宿主自己只有外观、快捷键、面板插件三页；别的页（Agent、远程、终端、关于……）都是插件
//! 登记的设置视图，按它挑的组排进导航。`impl Workspace` 的方法在子模块里，字段仍然声明在
//! main.rs 的 `Workspace` struct 里。

use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_base::StyledExt as _;
use gpui_component::Theme;
use smelt_ui::Button;
use smelt_ui::slider::{Slider, SliderEvent, SliderState, SliderValue};
use smelt_ui::theme_config::ThemeMode;

use crate::Workspace;
use crate::window_backdrop::WindowMaterial;
use smelt_model::font_config;

mod nav;
mod panel_plugins;
mod workspace;
mod workspace_view;

pub(crate) use nav::{SettingsSection, filter_settings_nav, settings_nav};
pub(crate) use panel_plugins::{PanelPluginsSnapshot, PanelPluginsState};

// ===================== 外观 =====================

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

fn default_bg_image_opacity() -> f32 {
    smelt_model::appearance_settings::DEFAULT_BG_IMAGE_OPACITY
}

/// 窗口外观设置（全局单例；存在客户端库里，见 [`load_appearance`]）。
#[derive(Clone)]
pub struct Appearance {
    /// 背景图片绝对路径（None = 无）。
    pub bg_image: Option<String>,
    /// 背景图片透明度 0–1（0=全透，1=全不透明）。默认低透明度做装饰层，
    /// 让主题底色透出，避免明亮图片直接铺满破坏可读性（对齐
    /// WezTerm / Windows Terminal 的 backgroundImageOpacity 惯例）。
    pub bg_image_opacity: f32,
    /// 明暗。跟着 `theme_name` 走，CLI 的 `appearance.theme_mode` 会把主题拨到同一族的另一侧。
    pub theme_mode: ThemeMode,
    /// 主题名，对应注册表里的 `name`，例如 `smelt Dark`、`Ayu Light`。
    pub theme_name: String,
    /// 界面字号（基准 rem，px）。控制侧边栏、Agent 会话、设置页等全局 UI 元素的排版缩放。
    pub ui_font_px: u32,
    /// 界面字体族。空 = 出厂的 Inter（随 app 带），中文落到系统字体。
    pub ui_font_family: String,
    /// 代码/终端等宽字体族。空 = 出厂默认（`font_config::DEFAULT_FONT_FAMILY`）；
    /// 填了但机器上没装时，渲染/测量会落到内嵌默认再落到 Menlo。
    /// 只作用于终端网格、diff、代码块；侧栏和按钮走 `ui_font_family`。
    pub font_family: String,
    /// 选的是哪份布局（布局 id）。`None` 是没选过，用第一个登记的布局。选的布局暂时没登记
    /// （插件还没加载、被卸了）时照样留着，不改写成别的。
    pub layout_id: Option<String>,
    /// 窗口底下垫哪一种系统材质。
    pub material: WindowMaterial,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            bg_image: None,
            bg_image_opacity: default_bg_image_opacity(),
            theme_mode: ThemeMode::Dark,
            theme_name: default_theme_name(),
            ui_font_px: DEFAULT_UI_FONT_PX,
            ui_font_family: String::new(),
            font_family: String::new(),
            layout_id: None,
            material: WindowMaterial::platform_default(),
        }
    }
}

impl Global for Appearance {}

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
    font_config::set_font_family(&appearance.font_family);
    cx.set_global(appearance.clone());
    apply_named_theme(&appearance.theme_name, cx);
    cx.refresh_windows();
}

/// 按主题名装上配色。`theme_catalog::apply` 装上自绘色板，组件库的 `Theme` 由
/// `component_theme` 跟着投影，终端等插件看着当前主题自己跟着切。
/// 字色按外观里选的窗口材质配，所以换了材质也要重装一次。字号和字体在装完之后写回 Appearance，主题文件里的字号不覆盖用户设置。
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
    if !applied {
        crate::ui_theme::set_light(!requested_dark);
    }
    sync_theme_fonts(cx);
}

/// 出厂界面字体：随 app 带的 Inter（`smelt_ui::init` 载入），空配置时用它。中文字形
/// 由系统字体回落补上（macOS 上是苹方）。
const DEFAULT_UI_FONT: &str = smelt_ui::kit::UI_FONT_FAMILY;

/// 当前生效的界面字体。空配置 = 出厂的 Inter。
pub fn resolved_ui_font_family(a: &Appearance) -> SharedString {
    let name = a.ui_font_family.trim();
    if name.is_empty() {
        DEFAULT_UI_FONT.into()
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
        .unwrap_or((DEFAULT_UI_FONT_PX, DEFAULT_UI_FONT.into()));
    if !cx.has_global::<Theme>() {
        return;
    }
    // Root 每帧用 Theme.font_size 设 rem。update 结尾的 sync_base 用当前主题
    // 重写分栏手柄和滚动条，这里不再另涂一层。
    Theme::update(cx, |theme| {
        theme.font_size = px(ui_font_px as f32);
        theme.font_family = ui_family;
        theme.mono_font_family = font_config::font_family().into();
    });
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
        theme_mode: appearance.theme_mode.key().to_string(),
        theme_name: if appearance.theme_name.trim().is_empty() {
            default_theme_name()
        } else {
            appearance.theme_name.clone()
        },
        ui_font_px: appearance.ui_font_px,
        ui_font_family: appearance.ui_font_family.clone(),
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
        theme_mode: ThemeMode::from_key(&snapshot.theme_mode).unwrap_or_else(default_theme_mode),
        theme_name: if snapshot.theme_name.trim().is_empty() {
            default_theme_name()
        } else {
            snapshot.theme_name
        },
        ui_font_px: snapshot.ui_font_px,
        ui_font_family: snapshot.ui_font_family,
        font_family: snapshot.font_family,
        ..base
    }
}

/// 改外观全局 + 存盘，不触发 view 重绘（调用方按需自己 notify/refresh）。
/// 供只有 `&mut App`（没有 `Context<Self>`）的场景用，比如设置页控件的回调。
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

// ===================== 设置页 =====================

/// 工作台的通知来源多（插件、窗口状态、各种全局），频率高于设置页所需。设置主体只取
/// 最终状态，因此将连续更新合并到这一间隔；本地输入控件仍由各自的实体即时刷新。
const SETTINGS_CONTENT_REFRESH_MIN_INTERVAL: Duration = Duration::from_millis(250);

/// 设置页在渲染前从 Workspace 取出的快照。不能在 `Render::render` 中直接访问
/// Workspace：那样 GPUI 会认为整张设置页都依赖它，工作台每次通知都失效缓存。可交互
/// 控件仍保留实体句柄，点击回调再回到 Workspace 执行。
#[derive(Clone)]
struct SettingsRenderSnapshot {
    ui_font_size_slider: Option<Entity<SliderState>>,
    bg_image_opacity_slider: Option<Entity<SliderState>>,
    font_options: std::sync::Arc<Vec<(SharedString, SharedString)>>,
    ui_font_options: std::sync::Arc<Vec<(SharedString, SharedString)>>,
    theme_options: std::rc::Rc<Vec<smelt_ui::ComboboxGroup>>,
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
            // 贴着窗口顶，顶上那条（拖窗口、给红黄绿按钮让位、标题）由页头画。
            draws_top_bar: true,
            instances: false,
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
            // 贴着窗口顶，顶上那条（拖窗口、关闭按钮）由页头画。
            draws_top_bar: true,
            instances: false,
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
/// 选中哪页、哪页有要处理的事随工作台的通知和视图角标的变化比一下，变了才重画：工作台
/// 通知很密，不跟着每次都重画。
struct SettingsNavView {
    workspace: WeakEntity<Workspace>,
    search: Entity<smelt_ui::field::InputState>,
    /// 分组列表的滚动位置。返回和搜索框钉在顶上，不跟着滚。
    scroll: ScrollHandle,
    selected: SettingsSection,
    attention: Option<SettingsSection>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsNavView {
    fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            smelt_ui::field::InputState::new(window, cx).placeholder("搜索设置、功能或命令")
        });
        let mut subscriptions = vec![
            cx.subscribe(&search, |_, _, event, cx| {
                if matches!(event, smelt_ui::field::InputEvent::Change) {
                    cx.notify();
                }
            }),
            // 插件的设置页登记、撤下了，导航跟着变；插件页挂上、摘掉角标，红点跟着变。
            cx.subscribe(
                &smelt_plugin_api::events(cx),
                |this: &mut Self, _, event: &smelt_plugin_api::HostEvent, cx| match event {
                    smelt_plugin_api::HostEvent::ViewsChanged => cx.notify(),
                    smelt_plugin_api::HostEvent::ViewBadgesChanged => this.sync(cx),
                    _ => {}
                },
            ),
        ];
        if let Some(host) = workspace.upgrade() {
            subscriptions.push(cx.observe(&host, |this: &mut Self, _, cx| this.sync(cx)));
        }
        // 建它的地方（打开设置、装布局）正更新着工作台，这时读不了，推后一拍再读。
        cx.defer_in(window, |this, _, cx| this.sync(cx));
        Self {
            workspace,
            search,
            scroll: ScrollHandle::new(),
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 顶上只是标题栏那一行：红黄绿按钮、拖窗口。侧栏从上到下是一整块，不画分隔线；
        // 页名在右边内容的标题行里。
        let header = self
            .workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).page_chrome_of(SETTINGS_NAV_VIEW))
            .map(|header| header.border_b_0());
        let nav = self.workspace.upgrade().map(|workspace| {
            Workspace::render_settings_nav(
                workspace,
                &self.selected,
                self.attention.as_ref(),
                &self.search,
                &self.scroll,
                cx,
            )
        });
        div()
            .flex()
            .flex_col()
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
                Self::observe_settings_global::<crate::keymap::KeymapState>(window, cx),
                // 插件登记、撤下主题时，主题选择的选项跟着变。
                Self::observe_settings_global::<smelt_ui::theme_catalog::ThemeCatalog>(window, cx),
            ],
            last_refresh: None,
            refresh_pending: false,
            refresh_generation: 0,
        }
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _timing = smelt_ui::frame_stats::RenderTiming::start("SettingsContent");
        // 工具栏就是标题栏那一行，只写这一页的名字（返回在左边导航的最上面）。
        let header = self
            .workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).page_chrome_of(SETTINGS_VIEW))
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
            }));
        let body = match (self.workspace.upgrade(), &self.snapshot) {
            (Some(workspace), Some(snapshot)) => {
                Workspace::render_settings_content(workspace, snapshot, self.plugin_page(cx), cx)
                    .into_any_element()
            }
            _ => div().into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}

impl smelt_plugin_api::PluginView for SettingsContentView {
    /// 停在哪一页由工作台的 `settings_section` 定，参数没有用处。每次打开都重取一份快照，
    /// 在事件里推后一拍做：打开它的地方往往正更新着工作台，这里要回头取工作台的快照。
    fn open(&mut self, _: &serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        cx.defer_in(window, |this, window, cx| this.refresh_snapshot(window, cx));
    }
}

// ===================== Workspace 方法 =====================

#[cfg(test)]
mod tests;
