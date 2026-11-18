//! 设置：主窗口里单独一页（[`SETTINGS_PAGE`]），两栏：左边是导航视图
//! （`smelt/settings-nav`，分组、搜索），右边是内容视图（[`SETTINGS_VIEW`]）。一次只画
//! 一页，页内再分节。停在哪页记在 [`SettingsSelection`]：导航改它，内容按它摆。
//!
//! 每一页都是带 `settings` 的视图，按它挑的组排进导航。宿主自己的外观、快捷键、面板插件
//! 三页管的是宿主据此行动的东西（窗口外观、键表、插件加载），由宿主登记（[`view_specs`]）；
//! 别的页（Agent、远程、终端、关于……）都是插件登记的。设置这一页只是摆设置视图的机制，
//! 不认识具体哪一页。

use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use smelt_ui::Button;
use smelt_ui::slider::{Slider, SliderEvent, SliderState, SliderValue};
use smelt_ui::theme_config::ThemeMode;

use crate::Workspace;
use smelt_model::appearance_settings::MAX_BG_IMAGE_OPACITY;

mod appearance_page;
mod content_view;
mod nav;
mod nav_view;
mod panel_plugins_page;
mod shortcuts_page;

use nav::{SettingsNavCategory, filter_settings_nav, resolve_page, settings_nav};
use smelt_plugin_api::{SettingsEntry, SettingsGroup, ViewHandle, ViewSpec};

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

/// 底色不透明度出厂值：主题底色占八成，看得出主题，也透得出毛玻璃。客户端库迁移时给的
/// 也是这个数（`smelt_client_store` 的 `MIGRATED_BG_OPACITY`）。
pub const DEFAULT_BG_OPACITY: f32 = 0.8;

/// 窗口外观设置（全局单例；存在客户端库里，见 [`load_appearance`]）。
#[derive(Clone)]
pub struct Appearance {
    /// 背景图片绝对路径（None = 无）。
    pub bg_image: Option<String>,
    /// 背景图片的不透明度，0 到 [`MAX_BG_IMAGE_OPACITY`]。图只做装饰，叠在底色上面、所有
    /// 内容下面；字色不按图配，所以封顶，再亮再暗的图也压不坏字。
    pub bg_image_opacity: f32,
    /// 有窗口材质时主题底色铺在材质上的不透明度，0–1：1 是主题本色、不透，0 是只剩毛玻璃。
    /// 没有材质的机器窗口不透明，不看它。
    pub bg_opacity: f32,
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
    /// 库里 `appearance.material` 列原样读回、原样写回，没人用：材质改由布局按地方声明
    /// （`smelt_plugin_api::LayoutMaterials`）。TODO: 表结构确认后连列一起删。
    pub material: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            bg_image: None,
            bg_image_opacity: default_bg_image_opacity(),
            bg_opacity: DEFAULT_BG_OPACITY,
            theme_mode: ThemeMode::Dark,
            theme_name: default_theme_name(),
            ui_font_px: DEFAULT_UI_FONT_PX,
            ui_font_family: String::new(),
            font_family: String::new(),
            layout_id: None,
            material: String::new(),
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
    let store = smelt_client_store::client_store().map_err(|error| {
        smelt_model::appearance_settings::SettingsError::Store(format!("打开客户端库失败：{error}"))
    })?;
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
    save_appearance_on(&next, store).map_err(|error| {
        smelt_model::appearance_settings::SettingsError::Store(format!("保存外观失败：{error}"))
    })?;
    install_appearance(next, cx);
    Ok(())
}

/// 把一份完整外观套到所有层并重绘。外观的真源是客户端库，内存里的全局只是它的投影。
fn install_appearance(appearance: Appearance, cx: &mut App) {
    smelt_ui::set_code_font(&appearance.font_family, cx);
    cx.set_global(appearance.clone());
    apply_named_theme(&appearance.theme_name, cx);
    cx.refresh_windows();
}

/// 按主题名装上配色。`theme_catalog::apply` 装上自绘色板和 gpui-base 的颜色，终端等插件
/// 看着当前主题自己跟着切。配色按窗口底（材质、底色不透明度）算，所以这两样变了也要重装一次。
/// 只改全局态不重绘，调用方自己决定什么时候 `cx.refresh_windows()`。
pub fn apply_named_theme(name: &str, cx: &mut App) {
    let appearance = cx.try_global::<Appearance>();
    let requested_dark = appearance
        .map(|appearance| appearance.theme_mode.is_dark())
        .unwrap_or(true);
    let bg_opacity = appearance.map_or(DEFAULT_BG_OPACITY, |appearance| appearance.bg_opacity);
    crate::ui_theme::set_window_backdrop(crate::window_backdrop::text_tone(), bg_opacity);
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
                material: record.material,
                bg_opacity: record.bg_opacity.clamp(0.0, 1.0),
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
        material: appearance.material.clone(),
        bg_opacity: appearance.bg_opacity,
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

/// 快照只有能脚本改的那些键；别的（选的布局、材质、底色不透明度）照抄 `base`。
fn appearance_from_snapshot(
    snapshot: smelt_model::appearance_settings::AppearanceSnapshot,
    base: Appearance,
) -> Appearance {
    Appearance {
        bg_image: snapshot.bg_image,
        bg_image_opacity: snapshot.bg_image_opacity.clamp(0.0, MAX_BG_IMAGE_OPACITY),
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
}

/// 换透的地方的底色不透明度：存进外观，再按新的窗口底重装主题。窗口根下一帧按它铺底。
pub(crate) fn set_bg_opacity(opacity: f32, cx: &mut App) {
    let opacity = opacity.clamp(0.0, 1.0);
    set_window_backdrop(move |a| a.bg_opacity = opacity, cx);
}

fn set_window_backdrop(edit: impl FnOnce(&mut Appearance), cx: &mut App) {
    apply_appearance(edit, cx);
    let theme_name = cx.global::<Appearance>().theme_name.clone();
    apply_named_theme(&theme_name, cx);
    cx.refresh_windows();
}

// ===================== 设置页 =====================

/// 设置内容视图的 id，打开设置就是打开它。宿主的视图，和插件的视图在同一张表里；只摆在
/// 设置那一页（[`SETTINGS_PAGE`]），打开它就是去那一页。
pub(crate) const SETTINGS_VIEW: &str = "smelt/settings";

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

/// 设置那一页和它的布局登记进页、布局注册表；设置的视图由 [`view_specs`] 交给
/// `host_views` 和宿主别的视图一起登记。没变就不动。
pub(crate) fn register_page(cx: &mut App) {
    let page = smelt_plugin_api::PageSource {
        name: "settings",
        title: smelt_plugin_api::text!("settings.title"),
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
        title: smelt_plugin_api::text!("settings.title"),
        state: SETTINGS_LAYOUT,
        materials: smelt_plugin_api::LayoutMaterials::default(),
    };
    if let Err(error) =
        smelt_plugin_api::register_layouts(crate::host_views::HOST_OWNER, &[layout], cx)
    {
        eprintln!("[plugins] 设置页的布局登记失败：{error}");
    }
}

/// 宿主自己的一页设置：登记成视图时的名字、标题、图标和搜索用的词。
struct HostPage {
    name: &'static str,
    /// 跟着语言变的标题。常量里放不下 `Text`，放个取它的函数。
    title: fn() -> smelt_plugin_api::Text,
    icon: smelt_ui::IconName,
    keywords: &'static [&'static str],
    build: fn(&WeakEntity<Workspace>, &mut App) -> Rc<dyn ViewHandle>,
}

impl HostPage {
    fn entry(&self) -> SettingsEntry {
        SettingsEntry {
            group: SettingsGroup::General,
            keywords: self
                .keywords
                .iter()
                .copied()
                .map(SharedString::new_static)
                .collect(),
        }
    }
}

/// 宿主自己的设置页，按这个次序排在「通用」组的最前面。
const HOST_PAGES: [HostPage; 3] = [
    HostPage {
        name: "appearance",
        title: || smelt_plugin_api::text!("settings.page.appearance"),
        icon: smelt_ui::IconName::Palette,
        keywords: &[
            "外观",
            "appearance",
            "theme",
            "dark",
            "light",
            "font",
            "layout",
            "background",
            "wallpaper",
            "主题",
            "颜色模式",
            "深色",
            "暗色",
            "夜间",
            "浅色",
            "亮色",
            "配色",
            "Default",
            "Ayu",
            "Catppuccin",
            "界面字号",
            "UI 字号",
            "界面字体",
            "字体大小",
            "调整字号",
            "缩放",
            "terminal",
            "终端",
            "代码字体",
            "等宽字体",
            "字体",
            "布局",
            "恢复布局",
            "背景",
            "背景图",
            "背景图片",
            "壁纸",
            "透明",
            "透明度",
            "背景透明",
            "材质",
            "毛玻璃",
            "模糊",
        ],
        build: build_appearance,
    },
    HostPage {
        name: "shortcuts",
        title: || smelt_plugin_api::text!("settings.page.shortcuts"),
        icon: smelt_ui::IconName::Keyboard,
        keywords: &[
            "快捷键",
            "键盘快捷键",
            "键盘",
            "按键",
            "组合键",
            "改键",
            "shortcut",
            "keymap",
            "keybinding",
            "keyboard",
            "hotkey",
            "cmd",
            "command",
        ],
        build: build_shortcuts,
    },
    HostPage {
        name: "panel-plugins",
        title: || smelt_plugin_api::text!("settings.page.panel_plugins"),
        icon: smelt_ui::IconName::Puzzle,
        keywords: &[
            "面板插件",
            "插件",
            "plugin",
            "panel",
            "import",
            "install",
            "reload",
            "扩展",
            "面板",
            "导入插件",
            "安装插件",
            "删除插件",
            "卸载插件",
            "重新加载",
            "gpui-shell",
        ],
        build: build_panel_plugins,
    },
];

fn build_appearance(workspace: &WeakEntity<Workspace>, cx: &mut App) -> Rc<dyn ViewHandle> {
    let workspace = workspace.clone();
    Rc::new(cx.new(|cx| appearance_page::AppearancePage::new(workspace, cx)))
}

fn build_shortcuts(_: &WeakEntity<Workspace>, cx: &mut App) -> Rc<dyn ViewHandle> {
    Rc::new(cx.new(shortcuts_page::ShortcutsPage::new))
}

fn build_panel_plugins(_: &WeakEntity<Workspace>, cx: &mut App) -> Rc<dyn ViewHandle> {
    Rc::new(cx.new(panel_plugins_page::PanelPluginsPage::new))
}

/// 是宿主自己登记的视图。导航里宿主的页排在同组插件的页前面。
fn is_host_view(id: &str) -> bool {
    id.strip_prefix(crate::host_views::HOST_OWNER)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// 设置的视图：导航、内容，和宿主自己的几页设置。
pub(crate) fn view_specs(workspace: WeakEntity<Workspace>) -> Vec<ViewSpec> {
    let nav = workspace.clone();
    let mut specs = vec![
        ViewSpec {
            name: "settings-nav".into(),
            title: smelt_plugin_api::text!("settings.nav"),
            icon: smelt_ui::IconName::Settings.path(),
            // 贴着窗口顶，顶上那条（拖窗口、给红黄绿按钮让位、标题）由页头画。
            draws_top_bar: true,
            instances: false,
            settings: None,
            fills: Vec::new(),
            build: Rc::new(move |window, cx| -> Rc<dyn ViewHandle> {
                let workspace = nav.clone();
                Rc::new(cx.new(|cx| nav_view::SettingsNavView::new(workspace, window, cx)))
            }),
        },
        ViewSpec {
            name: "settings".into(),
            title: smelt_plugin_api::text!("settings.title"),
            icon: smelt_ui::IconName::Settings.path(),
            // 贴着窗口顶，顶上那条（拖窗口、关闭按钮）由页头画。
            draws_top_bar: true,
            instances: false,
            settings: None,
            fills: Vec::new(),
            build: Rc::new(|window, cx| -> Rc<dyn ViewHandle> {
                Rc::new(cx.new(|cx| content_view::SettingsContentView::new(window, cx)))
            }),
        },
    ];
    specs.extend(HOST_PAGES.iter().map(|page| {
        let workspace = workspace.clone();
        let build = page.build;
        ViewSpec {
            name: page.name.into(),
            title: (page.title)(),
            icon: page.icon.path(),
            draws_top_bar: false,
            instances: false,
            settings: Some(page.entry()),
            fills: Vec::new(),
            build: Rc::new(move |_, cx| build(&workspace, cx)),
        }
    }));
    specs
}

/// 设置停在哪一页（设置页视图的 id）。`None` 是还没选过；选过的那页撤下了也一样，都停在
/// 导航里的第一页（见 [`nav::resolve_page`]）。导航改它，内容按它摆。
#[derive(Default)]
struct SettingsSelection(Option<SharedString>);

impl Global for SettingsSelection {}

/// 选过的那页，没选过是 `None`。只读内存。
fn chosen_page(cx: &App) -> Option<&str> {
    cx.try_global::<SettingsSelection>()?.0.as_deref()
}

/// 停到这一页。没变不动，免得导航和内容白白重取。
fn select_page(id: SharedString, cx: &mut App) {
    if chosen_page(cx) != Some(id.as_ref()) {
        cx.set_global(SettingsSelection(Some(id)));
    }
}

/// 设置里有没有要你处理的事，有的话落在哪一页：设置页挂着角标（比如「关于」页有新版本）→
/// 那一页。导航里那一页的红点看它。只读内存。
fn attention(cx: &App) -> Option<SharedString> {
    smelt_plugin_api::views(cx)
        .into_iter()
        .find(|view| {
            view.settings.is_some() && smelt_plugin_api::view_badge(&view.id, cx).is_some()
        })
        .map(|view| view.id)
}

impl Workspace {
    /// 打开设置：⌘,、菜单栏「设置…」、命令都走这里。设置已经露着就不换页；没露着时有要
    /// 处理的事就停在那一页，没有就停在上次那页。
    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !crate::plugin_host::view_shown(SETTINGS_VIEW, cx)
            && let Some(page) = attention(cx)
        {
            return self.open_settings_page(page, window, cx);
        }
        self.open_view(SETTINGS_VIEW, serde_json::Value::Null, window, cx);
    }

    /// 打开设置并停在这一页（设置页视图的 id）。
    pub(crate) fn open_settings_page(
        &mut self,
        page: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        select_page(page, cx);
        self.open_view(SETTINGS_VIEW, serde_json::Value::Null, window, cx);
    }
}
#[cfg(test)]
mod tests;
