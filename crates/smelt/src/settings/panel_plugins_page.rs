//! 设置页：面板插件（NSH JS/TSX 插件，每个是工作台布局里的一个视图）。宿主自己的页，和插件
//! 的设置页一样是登记的视图（见 [`super::view_specs`]）。
//!
//! 列出眼下加载的插件，导入、删除自己装的插件。App 自带的插件跟着 smelt 更新，不能删；和
//! 能力库里 Pi 的技能与扩展是两回事：这些插件跑在 smelt 界面里，只在本机。改磁盘放后台线程；
//! 主壳盯着插件目录，文件一变自己重编热重载，这页不用管挂载。

use super::*;
use crate::plugins::InstalledPlugin;
use rust_i18n::t;
use smelt_plugin_api::HostEvent;
use smelt_ui::kit::{TYPE, TypeStyled as _};
use smelt_ui::{SettingsPage, SettingsRow as Row, SettingsSection as Section};

/// 示例插件仓库，照着它写自己的插件。
const DEMO_URL: &str = "https://github.com/smelt-ai/smelt-plugin-demo";

pub(super) struct PanelPluginsPage {
    /// 眼下加载的插件，建这页时和插件登记变了时重读（只读内存）。
    plugins: Vec<InstalledPlugin>,
    /// 用户插件目录，导入的插件复制进这里。
    user_dir: Option<std::path::PathBuf>,
    /// 正在等二次确认删除的插件 id。
    pending_delete: Option<String>,
    /// 导入、删除、打开目录的最近一次错误。
    error: Option<String>,
    /// 正在复制或删除，按钮先禁用。
    busy: bool,
    _views: Subscription,
}

impl PanelPluginsPage {
    pub(super) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            plugins: crate::plugins::installed(),
            user_dir: crate::plugins::user_plugin_dir(),
            pending_delete: None,
            error: None,
            busy: false,
            // 插件重挂后重读列表。每次挂载完都发主题变了（只有主题的插件没有视图，不发视图
            // 变了），加载失败也发，所以两个都听。
            _views: cx.subscribe(&smelt_plugin_api::events(cx), |this, _, event, cx| {
                if matches!(event, HostEvent::ViewsChanged | HostEvent::ThemesChanged) {
                    this.refresh(cx);
                }
            }),
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.plugins = crate::plugins::installed();
        cx.notify();
    }

    /// 选一个插件目录，复制进用户插件目录；挂载由工作台盯着目录自己做。
    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            if !matches!(this.update(cx, |this, cx| this.begin_change(cx)), Ok(true)) {
                return;
            }
            let result = smelt_sys::blocking::run(move || crate::plugins::import(&source)).await;
            let _ = this.update(cx, |this, cx| {
                this.finish_change(
                    result.map(|name| t!("plugins.imported", name = name).into_owned()),
                    cx,
                )
            });
        })
        .detach();
    }

    /// 点一下删除只是进入待确认状态；再点一次才删。
    fn request_delete(&mut self, id: String, cx: &mut Context<Self>) {
        self.error = None;
        self.pending_delete = Some(id);
        cx.notify();
    }

    fn cancel_delete(&mut self, cx: &mut Context<Self>) {
        self.pending_delete = None;
        cx.notify();
    }

    /// 把插件挪进回收目录；撤下由工作台盯着目录自己做。
    fn confirm_delete(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_delete = None;
        let Some(plugin) = self.plugins.iter().find(|plugin| plugin.id == id).cloned() else {
            self.error = Some(t!("plugins.not_found").into_owned());
            cx.notify();
            return;
        };
        if !self.begin_change(cx) {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let root = plugin.root.clone();
            let result = smelt_sys::blocking::run(move || crate::plugins::uninstall(&root)).await;
            let _ = this.update(cx, |this, cx| {
                let message = if plugin.overrides_builtin {
                    t!("plugins.restored", name = plugin.name).into_owned()
                } else {
                    t!("plugins.deleted", name = plugin.name).into_owned()
                };
                this.finish_change(result.map(|()| message), cx)
            });
        })
        .detach();
    }

    /// 在系统文件管理器里打开用户插件目录，没有就先建出来。
    fn open_dir(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.user_dir.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let dir_for_create = dir.clone();
            let created =
                smelt_sys::blocking::run(move || std::fs::create_dir_all(&dir_for_create)).await;
            let _ = this.update(cx, |this, cx| match created {
                Ok(()) => cx.open_with_system(&dir),
                Err(error) => {
                    this.error = Some(t!("plugins.create_dir_failed", error = error).into_owned());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 返回能不能开始：同一时刻只做一件。
    fn begin_change(&mut self, cx: &mut Context<Self>) -> bool {
        if self.busy {
            return false;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        true
    }

    fn finish_change(&mut self, result: Result<String, String>, cx: &mut Context<Self>) {
        self.busy = false;
        match result {
            Ok(message) => crate::status_item::notify_success(message),
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    /// 页面的主体：已装的插件。导入、打开目录是对这张列表的操作，放在它的第一格。
    fn list_section(&self, cx: &Context<Self>) -> Section {
        let title = match self.plugins.len() {
            0 => t!("plugins.installed").into_owned(),
            count => t!("plugins.installed_count", count = count).into_owned(),
        };
        let section = Section::new()
            .title(title)
            .description(t!("plugins.list_hint"))
            .child(self.toolbar_cell(cx));
        if self.plugins.is_empty() {
            return section.child(
                div()
                    .type_style(TYPE.ui)
                    .text_color(rgb(crate::ui_theme::muted()))
                    .child(t!("plugins.empty")),
            );
        }
        section.children(self.plugins.iter().map(|plugin| {
            let pending = self.pending_delete.as_deref() == Some(plugin.id.as_str());
            self.plugin_row(plugin, pending, cx)
        }))
    }

    /// 列表第一格：左边说明导入会做什么，右边两个操作，导入是主按钮；下面一句警告。
    fn toolbar_cell(&self, cx: &Context<Self>) -> Div {
        let user_dir = self
            .user_dir
            .as_ref()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        let busy = self.busy;
        let row = Row::new(t!("plugins.import"))
            .description(t!("plugins.import_hint", dir = user_dir))
            .child(
                Button::new("open-panel-plugin-dir")
                    .quiet()
                    .small()
                    .label(t!("plugins.open_dir"))
                    .on_click(cx.listener(|this, _, _, cx| this.open_dir(cx))),
            )
            .child(
                Button::new("import-panel-plugin")
                    .small()
                    .label(t!("plugins.import_button"))
                    .loading(busy)
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.import(window, cx))),
            );
        // 用警告色：导入别人的插件等于给它整台机器的权限。
        let warning = div()
            .type_style(TYPE.text_xsmall)
            .text_color(rgb(crate::ui_theme::need()))
            .child(SharedString::from(t!("plugins.trust_warning")));
        div().flex().flex_col().gap_2().child(row).child(warning)
    }

    fn plugin_row(&self, plugin: &InstalledPlugin, pending: bool, cx: &Context<Self>) -> Row {
        let id = plugin.id.clone();
        // 清单里的 version 可以不写。
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

        let mut row = Row::new(plugin.name.clone()).description(detail.join("\n"));
        if let Some(error) = &plugin.error {
            row = row.error(if plugin.fallback_to_builtin {
                t!("plugins.override_fallback", error = error)
            } else {
                t!("plugins.load_failed", error = error)
            });
        }
        if pending {
            let confirm_id = id.clone();
            let confirm_label = if plugin.overrides_builtin {
                t!("plugins.confirm_restore")
            } else {
                t!("plugins.confirm_delete")
            };
            row.child(
                Button::new(format!("confirm-delete-panel-plugin-{id}"))
                    .danger()
                    .small()
                    .label(confirm_label)
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.confirm_delete(confirm_id.clone(), window, cx)
                    })),
            )
            .child(
                Button::new(format!("cancel-delete-panel-plugin-{id}"))
                    .quiet()
                    .small()
                    .label(t!("action.cancel"))
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_delete(cx))),
            )
        } else {
            let action_label = if plugin.overrides_builtin {
                t!("plugins.restore_default")
            } else {
                t!("plugins.delete")
            };
            row.child(
                Button::new(format!("delete-panel-plugin-{id}"))
                    .quiet()
                    .small()
                    .label(action_label)
                    .disabled(self.busy)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.request_delete(id.clone(), cx)),
                    ),
            )
        }
    }
}

impl Render for PanelPluginsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        SettingsPage::new("settings-page:panel-plugins")
            .children(self.error.clone().map(|error| {
                Section::new().title(t!("plugins.last_failed")).child(
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(rgb(crate::ui_theme::bad()))
                        .child(error),
                )
            }))
            .child(self.list_section(cx))
            .child(authoring_section())
    }
}

impl smelt_plugin_api::PluginView for PanelPluginsPage {
    fn open(&mut self, _: &serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}

/// 给插件作者看的：示例和日志。放在最后，不跟装插件的人抢位置。
fn authoring_section() -> Section {
    Section::new().title(t!("plugins.write")).child(
        Row::new(t!("plugins.examples"))
            .description(t!("plugins.write_hint"))
            .child(
                Button::new("open-panel-plugin-log")
                    .quiet()
                    .small()
                    .label(t!("plugins.open_log"))
                    .on_click(|_, _, _| {
                        let opened = smelt_sys::app_log::log_path()
                            .ok_or_else(|| t!("plugins.no_log").into_owned())
                            .and_then(|path| crate::system_open::open_in_system_text_editor(&path));
                        if let Err(error) = opened {
                            crate::status_item::notify_error(error);
                        }
                    }),
            )
            .child(
                smelt_ui::Link::new("open-panel-plugin-demo", DEMO_URL)
                    .type_style(TYPE.ui)
                    .child(t!("plugins.examples_link")),
            ),
    )
}
