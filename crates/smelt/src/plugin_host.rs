//! 宿主这一侧的插件接口：实现 [`smelt_plugin_api::Host`]，登记内置插件，决定插件视图摆在
//! 哪：眼下每个视图都是舞台上的一页（工作台路由 `Page(id)`），侧栏给它一个入口。设计见 `docs/app-plugins.md`。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::*;
use smelt_plugin_api::{self as api, HostEvent, Machine, PathRequest, RuntimeKey};
use smelt_runtime_client::RuntimeClient;

use crate::dir_picker::DirPickerPurpose;
use crate::{MainWindow, Workspace, WorkspaceRoute, current_workspace, runtimes};

struct SmeltHost {
    current: Rc<RefCell<Option<MainWindow>>>,
}

impl SmeltHost {
    fn workspace(&self) -> Option<Entity<Workspace>> {
        current_workspace(&self.current)?.upgrade()
    }
}

impl api::Host for SmeltHost {
    fn machines(&self, cx: &App) -> Vec<Machine> {
        let mut keys = vec![RuntimeKey::Local];
        if let Some(hub) = cx.try_global::<runtimes::RuntimeHub>() {
            keys.extend(
                hub.remotes
                    .iter()
                    .map(|remote| remote.runtime.key().clone()),
            );
        }
        keys.into_iter()
            .map(|key| Machine {
                name: runtimes::runtime_name(&key, cx),
                unavailable: runtimes::runtime_unavailable_label(&key, cx).map(str::to_string),
                has_desktop: runtimes::runtime_has_desktop(&key, cx),
                key,
            })
            .collect()
    }

    fn client(&self, machine: &RuntimeKey, cx: &App) -> Option<RuntimeClient> {
        runtimes::client(machine, cx)
    }

    fn notify_success(&self, message: &str) {
        crate::status_item::notify_success(message);
    }

    fn notify_error(&self, message: &str) {
        crate::status_item::notify_error(message);
    }

    fn open_in_text_editor(&self, path: &Path) -> Result<(), String> {
        crate::ide::open_in_system_text_editor(path)
    }

    fn pick_paths(
        &self,
        machine: &RuntimeKey,
        request: PathRequest,
        done: api::PathsPicked,
        cx: &mut App,
    ) {
        let Some(runtime) = runtimes::runtime(machine, cx) else {
            crate::status_item::notify_error(api::MACHINE_REMOVED);
            return;
        };
        let Some(workspace) = self.workspace() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            workspace.open_dir_picker(runtime, DirPickerPurpose::Plugin { request, done }, cx);
        });
    }

    fn active_project(&self, cx: &App) -> Option<PathBuf> {
        self.workspace()?
            .read(cx)
            .active_local_project_root(cx)
            .map(PathBuf::from)
    }

    fn page_header(&self, window: &Window, cx: &App) -> Div {
        let left_guard = self
            .workspace()
            .map(|workspace| workspace.read(cx).chrome_left_guard(window))
            .unwrap_or_default();
        crate::agents::product_page_chrome(left_guard)
    }
}

/// 启动时、开主窗口之前调一次：装好宿主服务，登记内置插件。
pub(crate) fn install(current: Rc<RefCell<Option<MainWindow>>>, cx: &mut App) {
    api::install(SmeltHost { current }, cx);
    // 机器加了、移除了、改名、连上断开都记在 RuntimeHub 上。
    cx.observe_global::<runtimes::RuntimeHub>(|cx| api::emit(HostEvent::MachinesChanged, cx))
        .detach();
    smelt_capabilities::register(cx);
}

impl Workspace {
    /// 在舞台上打开一个插件视图。没登记过这个 id 就什么都不做，返回 `false`。
    pub(crate) fn open_page(
        &mut self,
        id: &str,
        args: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((view, _)) = api::ensure_view(id, window, cx) else {
            return false;
        };
        self.activate_workspace_tab(WorkspaceRoute::Page(id.to_string()), window, cx);
        view.open(&args, window, cx);
        true
    }

    /// 让停着的插件页对上注册表：启动时恢复上次停的那页，视图登记变了（插件加载、
    /// 卸载、替换）时重新取。视图已经没有了就回会话；新建的视图打开一次，让它读数据。
    pub(crate) fn sync_page_route(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let WorkspaceRoute::Page(id) = self.active_tab().clone() else {
            return;
        };
        match api::ensure_view(&id, window, cx) {
            Some((view, true)) => view.open(&serde_json::Value::Null, window, cx),
            Some((_, false)) => {}
            None => self.nav.set_active(WorkspaceRoute::Session),
        }
    }

    /// 宿主事件里工作台自己要管的：视图登记变了要对路由，侧栏入口要重画。
    pub(crate) fn subscribe_host_events(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            &api::events(cx),
            window,
            |this, _, event: &HostEvent, window, cx| {
                if *event == HostEvent::ViewsChanged {
                    this.sync_page_route(window, cx);
                    cx.notify();
                }
            },
        )
    }

    /// 只取已经建好的视图，不建。
    pub(crate) fn render_page(&self, id: &str, cx: &App) -> AnyElement {
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .children(api::built_view(id, cx).map(|view| view.view()))
            .into_any_element()
    }

    /// 当前项目变了就告诉插件。在可见刷新里调：当前项目跟着会话、侧栏选中、项目分组
    /// 变，这些变更都会排一次可见刷新。
    pub(crate) fn note_active_project(&mut self, cx: &mut Context<Self>) {
        let root = self.active_local_project_root(cx);
        if self.plugin_project_root != root {
            self.plugin_project_root = root;
            api::emit(HostEvent::ActiveProjectChanged, cx);
        }
    }
}
