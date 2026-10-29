//! 面板插件：Tool Panel 里的 gpui-shell 插件（见 `crate::plugins`）的导入、删除和重新加载。
//!
//! 和能力库里的「智能体插件」（Pi 的技能与扩展）是两回事：这些插件跑在 smelt 界面里，
//! 只在本机。改磁盘放后台线程；改完在工作台窗口里重新挂载全部插件，不用重启。

use super::*;
use crate::plugins::{InstalledPlugin, PluginSource};
use crate::tool_panel::ToolPanelTab;

#[derive(Default)]
pub(crate) struct PanelPluginsState {
    /// 正在等二次确认删除的插件 id。
    pending_delete: Option<String>,
    /// 导入、删除的最近一次错误。
    error: Option<String>,
    /// 正在复制或删除，按钮先禁用。
    busy: bool,
}

#[derive(Clone)]
pub(crate) struct PanelPluginsSnapshot {
    pub(crate) plugins: Vec<InstalledPlugin>,
    pub(crate) user_dir: Option<std::path::PathBuf>,
    pub(crate) pending_delete: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) busy: bool,
}

impl Workspace {
    /// 只读内存：插件注册表和这页的状态。
    pub(super) fn panel_plugins_snapshot(&self) -> PanelPluginsSnapshot {
        PanelPluginsSnapshot {
            plugins: crate::plugins::installed(),
            user_dir: crate::plugins::user_plugin_dir(),
            pending_delete: self.panel_plugins.pending_delete.clone(),
            error: self.panel_plugins.error.clone(),
            busy: self.panel_plugins.busy,
        }
    }

    /// 选一个插件目录，复制进用户插件目录，然后重新加载。
    pub(super) fn import_panel_plugin(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("导入插件".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(source) = paths.into_iter().next() else {
                return;
            };
            let started = this.update(cx, |this, cx| this.begin_panel_plugin_change(cx));
            if !matches!(started, Ok(true)) {
                return;
            }
            let result = cx
                .background_executor()
                .spawn(async move { crate::plugins::import(&source) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish_panel_plugin_change(result.map(|name| format!("已导入「{name}」")), cx)
            });
        })
        .detach();
    }

    /// 点一下删除只是进入待确认状态；再点一次才删。
    pub(super) fn request_panel_plugin_delete(&mut self, id: String, cx: &mut Context<Self>) {
        self.panel_plugins.error = None;
        self.panel_plugins.pending_delete = Some(id);
        cx.notify();
    }

    pub(super) fn cancel_panel_plugin_delete(&mut self, cx: &mut Context<Self>) {
        self.panel_plugins.pending_delete = None;
        cx.notify();
    }

    /// 把插件挪进回收目录，然后重新加载。
    pub(super) fn confirm_panel_plugin_delete(&mut self, id: String, cx: &mut Context<Self>) {
        self.panel_plugins.pending_delete = None;
        let Some(plugin) = crate::plugins::installed()
            .into_iter()
            .find(|plugin| plugin.id == id && plugin.source == PluginSource::User)
        else {
            self.panel_plugins.error = Some("找不到这个插件，可能已经删掉了".to_string());
            cx.notify();
            return;
        };
        if !self.begin_panel_plugin_change(cx) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let root = plugin.root.clone();
            let result = cx
                .background_executor()
                .spawn(async move { crate::plugins::uninstall(&root) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish_panel_plugin_change(
                    result.map(|()| format!("已删除「{}」，原文件在回收目录里", plugin.name)),
                    cx,
                )
            });
        })
        .detach();
    }

    /// 在系统文件管理器里打开用户插件目录，没有就先建出来。
    pub(super) fn open_panel_plugin_dir(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = crate::plugins::user_plugin_dir() else {
            return;
        };
        if let Err(error) = std::fs::create_dir_all(&dir) {
            self.panel_plugins.error = Some(format!("创建插件目录失败：{error}"));
            cx.notify();
            return;
        }
        cx.open_with_system(&dir);
    }

    /// 重新读盘、重新挂载全部插件。插件作者改完脚本点它，不用重启。
    ///
    /// 挂载要用工作台窗口，而这里多半是从设置窗口的按钮进来的，正占着 `Workspace`：
    /// 推到这一轮更新结束后再进工作台窗口，免得重入。
    pub(crate) fn reload_panel_plugins(&mut self, cx: &mut Context<Self>) {
        let handle = self.window_handle;
        let view = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = cx.update_window(handle, |_root, window, cx| {
                crate::plugins::mount_all(window, cx);
                let _ = view.update(cx, |workspace, cx| {
                    workspace.leave_unmounted_plugin_tab(cx);
                    cx.notify();
                });
            });
        });
    }

    /// 正停在一个已经删掉的插件 tab 上就回到 Files。别的会话、项目里记着的不管：
    /// 切过去时那个 tab 说明插件已删除，下次启动存档自然降级。
    fn leave_unmounted_plugin_tab(&mut self, cx: &mut Context<Self>) {
        if let ToolPanelTab::Plugin(slot) = self.tool_panel_tab
            && !crate::plugins::is_mounted(slot)
        {
            self.tool_panel_tab = ToolPanelTab::Files;
            self.save_state(cx);
        }
    }

    /// 返回能不能开始：同一时刻只做一件。
    fn begin_panel_plugin_change(&mut self, cx: &mut Context<Self>) -> bool {
        if self.panel_plugins.busy {
            return false;
        }
        self.panel_plugins.busy = true;
        self.panel_plugins.error = None;
        cx.notify();
        true
    }

    fn finish_panel_plugin_change(
        &mut self,
        result: Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        self.panel_plugins.busy = false;
        match result {
            Ok(message) => {
                crate::status_item::notify_success(message);
                self.reload_panel_plugins(cx);
            }
            Err(error) => self.panel_plugins.error = Some(error),
        }
        cx.notify();
    }
}
