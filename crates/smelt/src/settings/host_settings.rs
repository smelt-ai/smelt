//! 宿主设置导航与 8 个宿主设置页的状态控制器与动作分发（渲染在 NSH 主壳 `HostSettings.tsx`）。

use std::path::PathBuf;

use gpui::{
    App, Context, Hsla, KeybindingKeystroke, Keystroke, PathPromptOptions, Rgba, SharedString,
    Window,
};
use rust_i18n::t;
use serde_json::{Value, json};
use smelt_model::appearance_settings::MAX_BG_IMAGE_OPACITY;
use smelt_model::font_config;
use smelt_ui::theme_config::ThemeMode;

use crate::Workspace;
use crate::keymap::{self, KeymapState};

use super::{
    Appearance, MAX_UI_FONT_PX, MIN_UI_FONT_PX, SETTINGS_PAGE, apply_appearance, apply_named_theme,
    attention_pages, chosen_page, resolve_page, select_page, set_bg_opacity, settings_nav,
};

const DEMO_URL: &str = "https://github.com/smelt-ai/smelt-plugin-demo";

pub(crate) struct HostSettingsState {
    /// 系统已装字体列表：首次打开外观页时枚举一次并缓存。
    fonts: Option<Vec<String>>,
    /// 主题目录快照缓存：主题目录变化时失效。
    themes_cache: Option<Vec<Value>>,
    /// 面板插件页的异步状态（目录、错误、忙碌）。
    panel_plugins_dir: Option<PathBuf>,
    panel_plugins_error: Option<String>,
    panel_plugins_busy: bool,
    /// 本机维护：命令行安装与清理残留状态。
    #[cfg(target_os = "macos")]
    cli: crate::maintenance::CliState,
    cleanup: crate::maintenance::CleanupState,
    /// 最近一次在设置页里激活的页面 id：切到需要后台扫描的页时自动触发刷新。
    last_page_id: Option<String>,
}

impl HostSettingsState {
    pub(crate) fn new() -> Self {
        Self {
            fonts: None,
            themes_cache: None,
            panel_plugins_dir: crate::plugins::user_plugin_dir(),
            panel_plugins_error: None,
            panel_plugins_busy: false,
            #[cfg(target_os = "macos")]
            cli: crate::maintenance::CliState::default(),
            cleanup: crate::maintenance::CleanupState::default(),
            last_page_id: None,
        }
    }

    pub(crate) fn invalidate_themes(&mut self) {
        self.themes_cache = None;
    }

    fn fonts(&mut self, cx: &App) -> &[String] {
        self.fonts.get_or_insert_with(|| {
            let mut names = cx.text_system().all_font_names();
            names.sort();
            names.dedup();
            names
        })
    }

    fn themes(&mut self, cx: &App) -> &[Value] {
        self.themes_cache
            .get_or_insert_with(|| build_theme_options(cx))
    }
}

impl Workspace {
    /// 将设置导航与当前宿主设置页状态推给 `MainShell`。不在设置页时发 `Value::Null`，零额外序列化开销。
    pub(crate) fn sync_host_settings(&mut self, cx: &mut Context<Self>) {
        if self.router.page() != Some(SETTINGS_PAGE) {
            self.host_settings.last_page_id = None;
            self.nsh_shell
                .update(cx, |shell, cx| shell.set_host_settings(Value::Null, cx));
            return;
        }

        let nav_tree = settings_nav(&smelt_plugin_api::views(cx));
        let selected_leaf = resolve_page(&nav_tree, chosen_page(cx));
        let selected_id = selected_leaf.map(|leaf| leaf.id.to_string());
        let attention = attention_pages(cx);

        // 切入需要后台扫描的维护页时，同旧 `PluginView::open` 一样在后台刷新一次。
        if self.host_settings.last_page_id != selected_id {
            self.host_settings.last_page_id = selected_id.clone();
            match selected_id.as_deref() {
                #[cfg(target_os = "macos")]
                Some("smelt.maintenance/cli") => {
                    crate::maintenance::CliState::check(|ws| &mut ws.host_settings.cli, cx);
                }
                Some("smelt.maintenance/cleanup") => {
                    self.host_settings
                        .cleanup
                        .rescan(|ws| &mut ws.host_settings.cleanup, cx);
                }
                _ => {}
            }
        }

        let categories: Vec<Value> = nav_tree
            .iter()
            .map(|cat| {
                let children: Vec<Value> = cat
                    .children
                    .iter()
                    .map(|child| {
                        json!({
                            "id": child.id.as_ref(),
                            "title": child.title,
                            "icon": child.icon.as_ref(),
                            "keywords": child.keywords,
                            "attention": attention.contains(&child.id),
                        })
                    })
                    .collect();
                json!({
                    "title": cat.title.as_ref(),
                    "keywords": cat.keywords,
                    "children": children,
                })
            })
            .collect();

        let page_json = match selected_id.as_deref() {
            Some("smelt.language/settings") => Some(crate::language::snapshot(cx)),
            Some("smelt.terminal/settings") => Some(crate::terminal_settings::snapshot()),
            #[cfg(target_os = "macos")]
            Some("smelt.maintenance/cli") => Some(self.host_settings.cli.snapshot()),
            Some("smelt.maintenance/cleanup") => Some(self.host_settings.cleanup.snapshot()),
            Some("smelt.updater/about") => Some(crate::updater::snapshot(cx)),
            Some("smelt/panel-plugins") => Some(self.panel_plugins_snapshot()),
            Some("smelt/appearance") => Some(self.appearance_snapshot(cx)),
            Some("smelt/shortcuts") => Some(shortcuts_snapshot(cx)),
            _ => None,
        };

        let payload = json!({
            "nav": {
                "selected": selected_id,
                "categories": categories,
                "labels": {
                    "back": t!("settings.back").into_owned(),
                    "search": t!("settings.search").into_owned(),
                    "results_none": t!("settings.results.none").into_owned(),
                    "results_one": t!("settings.results.one").into_owned(),
                    "results_other": t!("settings.results.other", count = "%{count}").into_owned(),
                },
            },
            "page": page_json,
        });
        self.nsh_shell
            .update(cx, |shell, cx| shell.set_host_settings(payload, cx));
    }

    pub(crate) fn handle_host_settings_action(
        &mut self,
        action: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(page) = action["page"].as_str() else {
            return;
        };
        match page {
            "common" => {
                if action["kind"].as_str() == Some("open_url")
                    && let Some(url) = action["url"].as_str()
                {
                    cx.open_url(url);
                }
            }
            "nav" => match action["kind"].as_str() {
                Some("back") => self.go_back(window, cx),
                Some("select") => {
                    if let Some(id) = action["id"].as_str() {
                        select_page(SharedString::from(id.to_owned()), cx);
                    }
                }
                _ => {}
            },
            "language" => {
                crate::language::handle_action(action, cx);
                self.sync_host_settings(cx);
            }
            "terminal" => {
                crate::terminal_settings::handle_action(action, cx);
                self.sync_host_settings(cx);
            }
            #[cfg(target_os = "macos")]
            "cli" => {
                if action["kind"].as_str() == Some("install") {
                    self.host_settings
                        .cli
                        .install(|ws| &mut ws.host_settings.cli, cx);
                    self.sync_host_settings(cx);
                }
            }
            "cleanup" => {
                if action["kind"].as_str() == Some("clean") {
                    self.host_settings
                        .cleanup
                        .clean(|ws| &mut ws.host_settings.cleanup, cx);
                    self.sync_host_settings(cx);
                }
            }
            "about" => {
                crate::updater::handle_action(action, window, cx);
                self.sync_host_settings(cx);
            }
            "panel_plugins" => self.handle_panel_plugins_action(action, window, cx),
            "appearance" => self.handle_appearance_action(action, cx),
            "shortcuts" => handle_shortcuts_action(action, window, cx),
            _ => {}
        }
    }

    // ===================== 面板插件页 =====================

    fn panel_plugins_snapshot(&self) -> Value {
        let plugins = crate::plugins::installed();
        let list_title = match plugins.len() {
            0 => t!("plugins.installed").into_owned(),
            count => t!("plugins.installed_count", count = count).into_owned(),
        };
        let user_dir = self
            .host_settings
            .panel_plugins_dir
            .as_ref()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        let rows: Vec<Value> = plugins
            .iter()
            .map(|plugin| {
                let mut detail = vec![if plugin.version.is_empty() {
                    plugin.id.clone()
                } else {
                    t!(
                        "plugins.id_version",
                        id = plugin.id,
                        version = plugin.version
                    )
                    .into_owned()
                }];
                detail.push(plugin.root.display().to_string());
                if plugin.overrides_builtin && !plugin.fallback_to_builtin {
                    detail.push(t!("plugins.override_builtin").into_owned());
                }
                let error = plugin.error.as_ref().map(|error| {
                    if plugin.fallback_to_builtin {
                        t!("plugins.override_fallback", error = error).into_owned()
                    } else {
                        t!("plugins.load_failed", error = error).into_owned()
                    }
                });
                let action_label = if plugin.overrides_builtin {
                    t!("plugins.restore_default").into_owned()
                } else {
                    t!("plugins.delete").into_owned()
                };
                let confirm_label = if plugin.overrides_builtin {
                    t!("plugins.confirm_restore").into_owned()
                } else {
                    t!("plugins.confirm_delete").into_owned()
                };
                json!({
                    "id": plugin.id,
                    "name": plugin.name,
                    "detail": detail.join("\n"),
                    "error": error,
                    "overrides_builtin": plugin.overrides_builtin,
                    "action_label": action_label,
                    "confirm_label": confirm_label,
                })
            })
            .collect();

        json!({
            "kind": "panel_plugins",
            "error": self.host_settings.panel_plugins_error,
            "last_failed_title": t!("plugins.last_failed").into_owned(),
            "list_title": list_title,
            "list_hint": t!("plugins.list_hint").into_owned(),
            "empty_label": t!("plugins.empty").into_owned(),
            "import_title": t!("plugins.import").into_owned(),
            "import_hint": t!("plugins.import_hint", dir = user_dir).into_owned(),
            "open_dir_label": t!("plugins.open_dir").into_owned(),
            "import_button_label": t!("plugins.import_button").into_owned(),
            "trust_warning": t!("plugins.trust_warning").into_owned(),
            "busy": self.host_settings.panel_plugins_busy,
            "plugins": rows,
            "cancel_label": t!("action.cancel").into_owned(),
            "authoring": {
                "section_title": t!("plugins.write").into_owned(),
                "row_title": t!("plugins.examples").into_owned(),
                "description": t!("plugins.write_hint").into_owned(),
                "open_log_label": t!("plugins.open_log").into_owned(),
                "examples_label": t!("plugins.examples_link").into_owned(),
                "examples_url": DEMO_URL,
            },
        })
    }

    fn handle_panel_plugins_action(
        &mut self,
        action: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action["kind"].as_str() {
            Some("open_dir") => {
                let Some(dir) = self.host_settings.panel_plugins_dir.clone() else {
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let dir_for_create = dir.clone();
                    let created =
                        smelt_sys::blocking::run(move || std::fs::create_dir_all(&dir_for_create))
                            .await;
                    let _ = this.update(cx, |this, cx| match created {
                        Ok(()) => cx.open_with_system(&dir),
                        Err(error) => {
                            this.host_settings.panel_plugins_error =
                                Some(t!("plugins.create_dir_failed", error = error).into_owned());
                            this.sync_host_settings(cx);
                        }
                    });
                })
                .detach();
            }
            Some("import") => {
                let paths = cx.prompt_for_paths(PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some(t!("plugins.import_prompt").into_owned().into()),
                });
                cx.spawn_in(window, async move |this, cx| {
                    let Ok(Ok(Some(paths))) = paths.await else {
                        return;
                    };
                    let Some(source) = paths.into_iter().next() else {
                        return;
                    };
                    let started = this
                        .update(cx, |this, cx| this.begin_panel_plugin_change(cx))
                        .unwrap_or(false);
                    if !started {
                        return;
                    }
                    let result =
                        smelt_sys::blocking::run(move || crate::plugins::import(&source)).await;
                    let _ = this.update(cx, |this, cx| {
                        this.finish_panel_plugin_change(
                            result.map(|name| t!("plugins.imported", name = name).into_owned()),
                            cx,
                        );
                    });
                })
                .detach();
            }
            Some("delete") => {
                let Some(id) = action["id"].as_str() else {
                    return;
                };
                let installed = crate::plugins::installed();
                let Some(plugin) = installed.into_iter().find(|p| p.id == id) else {
                    self.host_settings.panel_plugins_error =
                        Some(t!("plugins.not_found").into_owned());
                    self.sync_host_settings(cx);
                    return;
                };
                if !self.begin_panel_plugin_change(cx) {
                    return;
                }
                cx.spawn_in(window, async move |this, cx| {
                    let root = plugin.root.clone();
                    let result =
                        smelt_sys::blocking::run(move || crate::plugins::uninstall(&root)).await;
                    let _ = this.update(cx, |this, cx| {
                        let message = if plugin.overrides_builtin {
                            t!("plugins.restored", name = plugin.name).into_owned()
                        } else {
                            t!("plugins.deleted", name = plugin.name).into_owned()
                        };
                        this.finish_panel_plugin_change(result.map(|()| message), cx);
                    });
                })
                .detach();
            }
            Some("open_log") => {
                let opened = smelt_sys::app_log::log_path()
                    .ok_or_else(|| t!("plugins.no_log").into_owned())
                    .and_then(|path| crate::system_open::open_in_system_text_editor(&path));
                if let Err(error) = opened {
                    crate::status_item::notify_error(error);
                }
            }
            _ => {}
        }
    }

    fn begin_panel_plugin_change(&mut self, cx: &mut Context<Self>) -> bool {
        if self.host_settings.panel_plugins_busy {
            return false;
        }
        self.host_settings.panel_plugins_busy = true;
        self.host_settings.panel_plugins_error = None;
        self.sync_host_settings(cx);
        true
    }

    fn finish_panel_plugin_change(
        &mut self,
        result: Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        self.host_settings.panel_plugins_busy = false;
        match result {
            Ok(message) => crate::status_item::notify_success(message),
            Err(error) => self.host_settings.panel_plugins_error = Some(error),
        }
        self.sync_host_settings(cx);
    }

    // ===================== 外观页 =====================

    fn appearance_snapshot(&mut self, cx: &App) -> Value {
        let appearance = cx.global::<Appearance>().clone();
        let fonts = self.host_settings.fonts(cx).to_vec();
        let themes = self.host_settings.themes(cx).to_vec();
        let layouts: Vec<Value> = crate::workspace_layout::home_layouts(cx)
            .into_iter()
            .map(|layout| {
                json!({
                    "value": layout.id.as_ref(),
                    "title": layout.title.get().as_ref(),
                })
            })
            .collect();
        let selected_layout =
            crate::workspace_layout::effective_home_layout(cx).unwrap_or_default();
        let image_name = appearance
            .bg_image
            .as_deref()
            .and_then(|path| path.rsplit(['/', '\\']).next())
            .map(str::to_string);
        let has_image = image_name.is_some();

        let mut map = serde_json::Map::new();
        let mut insert = |key: &str, value: Value| {
            map.insert(key.to_owned(), value);
        };
        insert("kind", json!("appearance"));
        insert(
            "interface_title",
            json!(t!("appearance.interface").into_owned()),
        );
        insert("theme_label", json!(t!("appearance.theme").into_owned()));
        insert(
            "theme_hint",
            json!(t!("appearance.theme_hint").into_owned()),
        );
        insert("theme_selected", json!(displayed_theme_name(cx)));
        insert(
            "theme_empty",
            json!(t!("settings.results.none").into_owned()),
        );
        insert("themes", json!(themes));
        insert(
            "ui_font_size_label",
            json!(t!("appearance.ui_font_size").into_owned()),
        );
        insert(
            "ui_font_size_hint",
            json!(t!("appearance.ui_font_size_hint").into_owned()),
        );
        insert("ui_font_px", json!(appearance.ui_font_px));
        insert("min_ui_font_px", json!(MIN_UI_FONT_PX));
        insert("max_ui_font_px", json!(MAX_UI_FONT_PX));
        insert(
            "ui_font_label",
            json!(t!("appearance.ui_font").into_owned()),
        );
        insert("ui_font_family", json!(appearance.ui_font_family));
        insert(
            "ui_default_font_label",
            json!(
                t!(
                    "appearance.default_font",
                    name = smelt_ui::kit::UI_FONT_FAMILY
                )
                .into_owned()
            ),
        );
        insert("code_title", json!(t!("appearance.code").into_owned()));
        insert(
            "code_font_label",
            json!(t!("appearance.code_font").into_owned()),
        );
        insert(
            "code_font_hint",
            json!(t!("appearance.code_font_hint").into_owned()),
        );
        insert("code_font_family", json!(appearance.font_family));
        insert(
            "code_default_font_label",
            json!(
                t!(
                    "appearance.default_font",
                    name = font_config::DEFAULT_FONT_FAMILY
                )
                .into_owned()
            ),
        );
        insert("fonts", json!(fonts));
        insert("layout_title", json!(t!("appearance.layout").into_owned()));
        insert("layout_label", json!(t!("appearance.layout").into_owned()));
        insert(
            "layout_hint",
            json!(t!("appearance.layout_hint").into_owned()),
        );
        insert(
            "layout_none_hint",
            json!(t!("appearance.layout_none_hint").into_owned()),
        );
        insert(
            "no_layouts_label",
            json!(t!("appearance.no_layouts").into_owned()),
        );
        insert("layouts", json!(layouts));
        insert("selected_layout", json!(selected_layout));
        insert(
            "reset_layout_label",
            json!(t!("appearance.reset_layout").into_owned()),
        );
        insert(
            "reset_layout_hint",
            json!(t!("appearance.reset_layout_hint").into_owned()),
        );
        insert("reset_action_label", json!(t!("action.reset").into_owned()));
        insert(
            "background_title",
            json!(t!("appearance.background").into_owned()),
        );
        insert(
            "backdrop_available",
            json!(crate::window_backdrop::available()),
        );
        insert("glass_label", json!(t!("appearance.glass").into_owned()));
        insert(
            "glass_hint",
            json!(t!("appearance.glass_hint").into_owned()),
        );
        insert(
            "bg_opacity_pct",
            json!((appearance.bg_opacity * 100.0).round() as u32),
        );
        insert("image_label", json!(t!("appearance.image").into_owned()));
        insert(
            "image_hint",
            json!(t!("appearance.image_hint").into_owned()),
        );
        insert("image_name", json!(image_name));
        insert(
            "image_button_label",
            json!(if has_image {
                t!("appearance.change_image").into_owned()
            } else {
                t!("appearance.choose_image").into_owned()
            }),
        );
        insert("clear_label", json!(t!("action.clear").into_owned()));
        insert(
            "image_opacity_label",
            json!(t!("appearance.image_opacity").into_owned()),
        );
        insert(
            "image_opacity_hint",
            json!(t!("appearance.image_opacity_hint").into_owned()),
        );
        insert(
            "bg_image_opacity_pct",
            json!((appearance.bg_image_opacity * 100.0).round() as u32),
        );
        insert(
            "max_bg_image_opacity_pct",
            json!((MAX_BG_IMAGE_OPACITY * 100.0).round() as u32),
        );
        Value::Object(map)
    }

    fn handle_appearance_action(&mut self, action: &Value, cx: &mut Context<Self>) {
        match action["kind"].as_str() {
            Some("set_theme") => {
                if let Some(name) = action["value"].as_str() {
                    select_theme(name.to_owned(), cx);
                }
            }
            Some("set_ui_font_px") => {
                if let Some(px) = action["px"].as_u64() {
                    let size =
                        px.clamp(u64::from(MIN_UI_FONT_PX), u64::from(MAX_UI_FONT_PX)) as u32;
                    apply_appearance(move |a| a.ui_font_px = size, cx);
                    cx.refresh_windows();
                }
            }
            Some("set_ui_font_family") => {
                if let Some(name) = action["value"].as_str() {
                    let name = name.trim().to_string();
                    apply_appearance(move |a| a.ui_font_family = name, cx);
                    cx.refresh_windows();
                }
            }
            Some("set_code_font_family") => {
                if let Some(name) = action["value"].as_str() {
                    let name = name.trim().to_string();
                    smelt_model::font_config::set_font_family(&name);
                    apply_appearance(move |a| a.font_family = name, cx);
                    cx.refresh_windows();
                }
            }
            Some("set_layout") => {
                if let Some(id) = action["value"].as_str() {
                    let id = id.to_owned();
                    apply_appearance(move |a| a.layout_id = Some(id), cx);
                }
            }
            Some("reset_layout") => {
                self.restore_layout(cx);
            }
            Some("set_bg_opacity") => {
                if let Some(pct) = action["pct"].as_f64() {
                    set_bg_opacity((pct as f32) / 100.0, cx);
                }
            }
            Some("pick_bg_image") => {
                pick_bg_image(cx);
            }
            Some("clear_bg_image") => {
                set_bg_image(None, cx);
            }
            Some("set_bg_image_opacity") => {
                if let Some(pct) = action["pct"].as_f64() {
                    let opacity = ((pct as f32) / 100.0).clamp(0.0, MAX_BG_IMAGE_OPACITY);
                    apply_appearance(move |a| a.bg_image_opacity = opacity, cx);
                    cx.refresh_windows();
                }
            }
            _ => {}
        }
    }
}

// ===================== 辅助函数 =====================

fn hsla_hex(color: Hsla) -> String {
    let rgba: Rgba = color.into();
    let r = (rgba.r.clamp(0.0, 1.0) * 255.0).round() as u8;
    let g = (rgba.g.clamp(0.0, 1.0) * 255.0).round() as u8;
    let b = (rgba.b.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn build_theme_options(cx: &App) -> Vec<Value> {
    smelt_ui::theme_catalog::themes(cx)
        .into_iter()
        .map(|theme| {
            let mode = if theme.mode.is_dark() {
                ["深色", "dark"]
            } else {
                ["浅色", "light"]
            };
            let swatch = smelt_ui::theme_catalog::swatch(&theme.name, cx).map(|s| {
                [
                    hsla_hex(s.background),
                    hsla_hex(s.foreground),
                    hsla_hex(s.accent),
                ]
            });
            json!({
                "value": theme.name.as_ref(),
                "title": theme.name.as_ref(),
                "group": theme.family.as_ref(),
                "keywords": [theme.family.as_ref(), mode[0], mode[1]],
                "swatch": swatch,
            })
        })
        .collect()
}

fn displayed_theme_name(cx: &App) -> String {
    let appearance = cx.global::<Appearance>();
    if smelt_ui::theme_catalog::contains(&appearance.theme_name, cx) {
        appearance.theme_name.clone()
    } else {
        smelt_model::theme_index::default_for_mode(if appearance.theme_mode.is_dark() {
            "dark"
        } else {
            "light"
        })
        .to_string()
    }
}

fn select_theme(name: String, cx: &mut App) {
    let Some(entry) = smelt_ui::theme_catalog::themes(cx)
        .into_iter()
        .find(|entry| entry.name.as_ref() == name)
    else {
        return;
    };
    let mode = if !entry.mode.is_dark() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    apply_appearance(
        move |appearance| {
            appearance.theme_name = name;
            appearance.theme_mode = mode;
        },
        cx,
    );
    let selected = cx.global::<Appearance>().theme_name.clone();
    apply_named_theme(&selected, cx);
    cx.refresh_windows();
}

fn set_bg_image(path: Option<String>, cx: &mut App) {
    if let Some(p) = path.as_deref() {
        smelt_ui::image::invalidate_file_preview(std::path::Path::new(p));
    }
    apply_appearance(|a| a.bg_image = path, cx);
    cx.refresh_windows();
}

fn pick_bg_image(cx: &mut App) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(t!("appearance.choose_image_prompt").into()),
    });
    cx.spawn(async move |cx| {
        if let Ok(Ok(Some(paths))) = paths.await
            && let Some(path) = paths
                .into_iter()
                .next()
                .and_then(|path| path.to_str().map(String::from))
        {
            cx.update(|cx| set_bg_image(Some(path), cx));
        }
    })
    .detach();
}

// ===================== 快捷键页 =====================

fn shortcuts_snapshot(cx: &App) -> Value {
    let state = cx.global::<KeymapState>();
    let os = cx.platform_info().os;

    let mut groups: Vec<(smelt_plugin_api::Text, Vec<Value>)> = Vec::new();
    for command in state.bound() {
        let id = command.info.id.clone();
        let title = command.info.title.get();
        let recording = state.recording.as_ref() == Some(&id);
        let error = state
            .error
            .as_ref()
            .filter(|(failed, _)| *failed == id)
            .map(|(_, reason)| reason.clone());
        let reassign = state.reassign.as_ref().filter(|r| r.id == id);
        let note = if recording {
            Some(json!({
                "text": t!("shortcuts.recording_note").into_owned(),
                "tone": "muted",
            }))
        } else if let Some(reassign) = reassign {
            let key = Keystroke::parse_with_os(&reassign.keystroke, os)
                .map(|stroke| smelt_ui::keybind::label(&stroke, os))
                .unwrap_or_else(|_| reassign.keystroke.clone());
            let other = state
                .find(&reassign.other)
                .map_or(reassign.other.clone(), |other| other.info.title.get());
            Some(json!({
                "text": t!("shortcuts.taken", key = key, other = other).into_owned(),
                "tone": "bad",
            }))
        } else {
            error.map(|reason| {
                json!({
                    "text": reason,
                    "tone": "bad",
                })
            })
        };
        let caps = command
            .key
            .as_deref()
            .and_then(|spec| Keystroke::parse_with_os(spec, os).ok())
            .map(|stroke| {
                smelt_ui::keybind::keycaps(&KeybindingKeystroke::from_keystroke(stroke), os)
            });
        let row = json!({
            "id": id.as_ref(),
            "title": title.as_ref(),
            "caps": caps,
            "recording": recording,
            "overridden": command.overridden,
            "reassigning": reassign.is_some(),
            "note": note,
            "aria_label": t!("shortcuts.change", title = title).into_owned(),
        });
        match groups
            .iter_mut()
            .find(|(group, _)| *group == command.info.group)
        {
            Some((_, rows)) => rows.push(row),
            None => groups.push((command.info.group.clone(), vec![row])),
        }
    }

    let groups_json: Vec<Value> = groups
        .into_iter()
        .map(|(title, rows)| {
            json!({
                "title": title.get().as_ref(),
                "rows": rows,
            })
        })
        .collect();

    let fixed_steps = |specs: &[&'static str]| -> Vec<Vec<String>> {
        specs
            .iter()
            .map(|spec| match Keystroke::parse_with_os(spec, os) {
                Ok(stroke) => {
                    smelt_ui::keybind::keycaps(&KeybindingKeystroke::from_keystroke(stroke), os)
                }
                Err(_) => vec![(*spec).to_owned()],
            })
            .collect()
    };
    let view_keys = vec![
        json!({
            "label": t!("shortcuts.terminal_search").into_owned(),
            "steps": fixed_steps(&["cmd-f"]),
        }),
        json!({
            "label": t!("shortcuts.close_or_back").into_owned(),
            "steps": fixed_steps(&["escape"]),
        }),
        json!({
            "label": t!("shortcuts.terminal_complete").into_owned(),
            "steps": fixed_steps(&["tab", "shift-tab"]),
        }),
    ];

    json!({
        "kind": "shortcuts",
        "hint": t!("shortcuts.hint").into_owned(),
        "groups": groups_json,
        "view_keys_title": t!("shortcuts.view_keys").into_owned(),
        "view_keys": view_keys,
        "labels": {
            "press": t!("shortcuts.press").into_owned(),
            "unset": t!("shortcuts.unset").into_owned(),
            "reset": t!("shortcuts.reset").into_owned(),
            "reassign": t!("shortcuts.reassign").into_owned(),
            "never_mind": t!("action.never_mind").into_owned(),
        },
    })
}

fn handle_shortcuts_action(action: &Value, window: &mut Window, cx: &mut App) {
    match action["kind"].as_str() {
        Some("toggle_record") => {
            let Some(id) = action["id"].as_str() else {
                return;
            };
            let recording = cx.global::<KeymapState>().recording.as_deref() == Some(id);
            if recording {
                keymap::stop_recording(cx);
            } else {
                keymap::start_recording(SharedString::from(id.to_owned()), window, cx);
            }
        }
        Some("reset") => {
            if let Some(id) = action["id"].as_str() {
                keymap::reset(SharedString::from(id.to_owned()), cx);
            }
        }
        Some("confirm_reassign") => keymap::confirm_reassign(cx),
        Some("cancel_reassign") => keymap::cancel_reassign(cx),
        _ => {}
    }
}
