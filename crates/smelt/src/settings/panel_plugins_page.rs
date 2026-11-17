//! 设置页：面板插件（gpui-shell 插件，每个是工作台布局里的一个视图）。宿主自己的页，和插件
//! 的设置页一样是登记的视图（见 [`super::view_specs`]）。
//!
//! 列出眼下加载的插件，导入、删除自己装的插件，重新加载全部插件。App 自带的插件跟着
//! smelt 更新，不能删；用户装一个同 id 的副本就能盖过它。和能力库里的「智能体插件」（Pi 的
//! 技能与扩展）是两回事：这些插件跑在 smelt 界面里，只在本机。改磁盘放后台线程；改完在
//! 工作台窗口里重新挂载全部插件，不用重启。

use super::*;
use crate::plugins::{InstalledPlugin, PluginSource};
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
            // 重新加载后插件的视图登记跟着变，列表随之重读。
            _views: cx.subscribe(&smelt_plugin_api::events(cx), |this, _, event, cx| {
                if matches!(event, HostEvent::ViewsChanged) {
                    this.refresh(cx);
                }
            }),
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.plugins = crate::plugins::installed();
        cx.notify();
    }

    /// 选一个插件目录，复制进用户插件目录，然后重新加载。
    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("导入插件".into()),
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
            let result = cx
                .background_executor()
                .spawn(async move { crate::plugins::import(&source) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_change(result.map(|name| format!("已导入「{name}」")), window, cx)
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

    /// 把插件挪进回收目录，然后重新加载。
    fn confirm_delete(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_delete = None;
        let Some(plugin) = self
            .plugins
            .iter()
            .find(|plugin| plugin.id == id && plugin.source == PluginSource::User)
            .cloned()
        else {
            self.error = Some("找不到这个插件，可能已经删掉了".to_string());
            cx.notify();
            return;
        };
        if !self.begin_change(cx) {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let root = plugin.root.clone();
            let result = cx
                .background_executor()
                .spawn(async move { crate::plugins::uninstall(&root) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_change(
                    result.map(|()| format!("已删除「{}」，原文件在回收目录里", plugin.name)),
                    window,
                    cx,
                )
            });
        })
        .detach();
    }

    /// 在系统文件管理器里打开用户插件目录，没有就先建出来。
    fn open_dir(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.user_dir.clone() else {
            return;
        };
        if let Err(error) = std::fs::create_dir_all(&dir) {
            self.error = Some(format!("创建插件目录失败：{error}"));
            cx.notify();
            return;
        }
        cx.open_with_system(&dir);
    }

    /// 重新读盘、重新挂载全部插件。插件作者改完脚本点它，不用重启。这页摆在工作台窗口里，
    /// `window` 就是挂载要用的那个；推到这一轮更新之后做，挂载时不占着这页。
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let page = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            // 视图登记变了由工作台自己跟（重装布局、对路由），见 `subscribe_host_events`。
            crate::plugins::mount_all(window, cx);
            // 加载出错不一定改登记，列表在这里再读一次。
            let _ = page.update(cx, |page, cx| page.refresh(cx));
        });
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

    fn finish_change(
        &mut self,
        result: Result<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = false;
        match result {
            Ok(message) => {
                crate::status_item::notify_success(message);
                self.reload(window, cx);
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    /// 页面的主体：已装的插件。导入、打开目录、重新加载是对这张列表的操作，放在它的第一格。
    fn list_section(&self, cx: &Context<Self>) -> Section {
        let title = match self.plugins.len() {
            0 => "已装插件".to_string(),
            count => format!("已装 {count} 个"),
        };
        let section = Section::new()
            .title(title)
            .description(
                "每个插件是一个视图：摆进工作台布局的是面板，没摆进去的打开时显示在主区域。",
            )
            .child(self.toolbar_cell(cx));
        if self.plugins.is_empty() {
            return section.child(
                div()
                    .type_style(TYPE.ui)
                    .text_color(rgb(crate::ui_theme::muted()))
                    .child("还没有插件。"),
            );
        }
        section.children(self.plugins.iter().map(|plugin| {
            let pending = self.pending_delete.as_deref() == Some(plugin.id.as_str());
            self.plugin_row(plugin, pending, cx)
        }))
    }

    /// 列表第一格：左边说明导入会做什么，右边三个操作，导入是唯一的主按钮；下面一句警告。
    fn toolbar_cell(&self, cx: &Context<Self>) -> Div {
        let user_dir = self
            .user_dir
            .as_ref()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        let busy = self.busy;
        let row = Row::new("导入")
            .description(format!(
                "选一个含 gpui-shell.json 的目录，复制进 {user_dir}；改了脚本点「重新加载」，不用重启。"
            ))
            .child(
                Button::new("reload-panel-plugins")
                    .quiet()
                    .small()
                    .label("重新加载")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))),
            )
            .child(
                Button::new("open-panel-plugin-dir")
                    .quiet()
                    .small()
                    .label("打开插件目录")
                    .on_click(cx.listener(|this, _, _, cx| this.open_dir(cx))),
            )
            .child(
                Button::new("import-panel-plugin")
                    .small()
                    .label("导入插件…")
                    .loading(busy)
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.import(window, cx))),
            );
        // 用警告色：导入别人的插件等于给它整台机器的权限。
        let warning = div()
            .type_style(TYPE.text_xsmall)
            .text_color(rgb(crate::ui_theme::need()))
            .child(
                "插件和 Smelt 一样能读写任意文件、运行任意命令，只导入你信任的。\
                 删除的插件挪进 ~/.smelt/trash/plugins，需要时能找回。",
            );
        div().flex().flex_col().gap_2().child(row).child(warning)
    }

    fn plugin_row(&self, plugin: &InstalledPlugin, pending: bool, cx: &Context<Self>) -> Row {
        let id = plugin.id.clone();
        let removable = plugin.source == PluginSource::User;

        // 清单里的 version 可以不写。
        let mut detail = vec![if plugin.version.is_empty() {
            plugin.id.clone()
        } else {
            format!("{} · 版本 {}", plugin.id, plugin.version)
        }];
        detail.push(match plugin.source {
            PluginSource::User => format!("自己装的 · {}", plugin.root.display()),
            PluginSource::Bundled => "smelt 自带，跟着 smelt 更新".to_string(),
            #[cfg(debug_assertions)]
            PluginSource::Repo => format!("开发构建从仓库加载 · {}", plugin.root.display()),
        });
        if let Some(error) = &plugin.error {
            detail.push(format!("加载失败：{error}"));
        }

        let row = Row::new(plugin.name.clone()).description(detail.join("\n"));
        if !removable {
            return row;
        }
        if pending {
            let confirm_id = id.clone();
            row.child(
                Button::new(format!("confirm-delete-panel-plugin-{id}"))
                    .danger()
                    .small()
                    .label("确认删除")
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.confirm_delete(confirm_id.clone(), window, cx)
                    })),
            )
            .child(
                Button::new(format!("cancel-delete-panel-plugin-{id}"))
                    .quiet()
                    .small()
                    .label("取消")
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_delete(cx))),
            )
        } else {
            row.child(
                Button::new(format!("delete-panel-plugin-{id}"))
                    .quiet()
                    .small()
                    .label("删除…")
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
                Section::new().title("上一次操作失败").child(
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
    Section::new().title("自己写插件").child(
        Row::new("示例和教程")
            .description(
                "gpui-shell 脚本：JavaScript 描述界面，GPUI 原生绘制。console.log 和脚本错误写进日志，\
                 带 [plugin] 标记。",
            )
            .child(
                Button::new("open-panel-plugin-log")
                    .quiet()
                    .small()
                    .label("打开日志")
                    .on_click(|_, _, _| {
                        let opened = smelt_sys::app_log::log_path()
                            .ok_or_else(|| "找不到日志文件".to_string())
                            .and_then(|path| crate::system_open::open_in_system_text_editor(&path));
                        if let Err(error) = opened {
                            crate::status_item::notify_error(error);
                        }
                    }),
            )
            .child(
                smelt_ui::Link::new("open-panel-plugin-demo", DEMO_URL)
                    .type_style(TYPE.ui)
                    .child("示例和教程 ↗"),
            ),
    )
}
