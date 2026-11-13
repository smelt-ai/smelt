//! 宿主这一侧的插件接口：实现 [`smelt_plugin_api::Host`]，登记内置插件，决定插件视图摆在
//! 哪：布局里摆了它就在它的面板里（见 `workspace_shell`），没摆的是舞台上的一页（工作台
//! 路由 `Page(id)`），侧栏给它一个入口。设计见 `docs/app-plugins.md`。

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;

use gpui::*;
use smelt_plugin_api::{self as api, HostEvent, Machine, PathRequest, Project, RuntimeKey};
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
                smelt_home: runtimes::smelt_home(&key, cx),
                key,
            })
            .collect()
    }

    fn client(&self, machine: &RuntimeKey, cx: &App) -> Option<RuntimeClient> {
        runtimes::client(machine, cx)
    }

    fn notify(&self, subtitle: &str, body: &str) {
        crate::status_item::deliver_app_notification(subtitle, body);
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

    fn active_project(&self, cx: &App) -> Option<Project> {
        let project = self.workspace()?.read(cx).active_project_ref(cx)?;
        Some(Project {
            machine: project.runtime,
            root: project.root,
        })
    }

    fn agent_settings<'a>(
        &self,
        machine: &RuntimeKey,
        cx: &'a App,
    ) -> Option<&'a api::model::agent_host::AgentHostSettings> {
        crate::settings::agent_hosts(cx)?.settings(machine)
    }

    fn view_shown(&self, id: &str, cx: &App) -> bool {
        self.workspace()
            .is_some_and(|workspace| workspace.read(cx).plugin_shown_views.contains(id))
    }

    /// 页面画在舞台上、舞台又顶着窗口上沿时，页头让出顶栏、给红黄绿按钮让位、能拖窗口；
    /// 摆在自己面板里的视图上面有面板的标题带，页头只是一条普通的头。
    fn page_header(&self, window: &Window, cx: &App) -> Div {
        let left_guard = self.workspace().and_then(|workspace| {
            let workspace = workspace.read(cx);
            let WorkspaceRoute::Page(id) = workspace.active_tab() else {
                return None;
            };
            let on_stage = !workspace.shell_view.laid_out.contains(id.as_str())
                && api::built_view(id, cx)
                    .is_some_and(|view| view.view().entity_id() == window.current_view());
            on_stage
                .then(|| workspace.chrome_left_guard(window))
                .flatten()
        });
        crate::agents::product_page_chrome(left_guard)
    }

    fn window_active(&self, cx: &App) -> bool {
        self.workspace()
            .is_some_and(|workspace| workspace.read(cx).workspace_window_active)
    }

    fn main_window(&self, _cx: &App) -> Option<AnyWindowHandle> {
        self.current
            .borrow()
            .as_ref()
            .map(|main| main.window.into())
    }

    fn open_view(
        &self,
        id: &str,
        args: serde_json::Value,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        self.workspace().is_some_and(|workspace| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_page(id, args, window, cx)
            })
        })
    }

    fn send_to_session(&self, text: &str, window: &mut Window, cx: &mut App) -> Result<(), String> {
        let workspace = self
            .workspace()
            .ok_or_else(|| "主窗口没有打开".to_string())?;
        workspace.update(cx, |workspace, cx| {
            workspace.send_to_session(text, window, cx)
        })
    }

    fn path_style(&self, machine: &RuntimeKey, cx: &App) -> api::model::runtime_path::PathStyle {
        runtimes::path_style(machine, cx)
    }
}

/// 启动时、开主窗口之前调一次：装好宿主服务，登记内置插件。
pub(crate) fn install(current: Rc<RefCell<Option<MainWindow>>>, cx: &mut App) {
    api::install(SmeltHost { current }, cx);
    // 机器加了、移除了、改名、连上断开都记在 RuntimeHub 上。
    cx.observe_global::<runtimes::RuntimeHub>(|cx| api::emit(HostEvent::MachinesChanged, cx))
        .detach();
    // 各台机器的智能体配置读到、改了、重读了都写在这个全局上。
    cx.observe_global::<crate::settings::AgentHosts>(|cx| {
        api::emit(HostEvent::AgentSettingsChanged, cx)
    })
    .detach();
    smelt_capabilities::register(cx);
    smelt_git_ui::register(cx);
    smelt_history::register(cx);
    smelt_layouts::register(cx);
}

impl Workspace {
    /// 打开一个视图：布局里摆了它就让它的面板露出来，没摆就在舞台上开成一页。没登记过
    /// 这个 id 就什么都不做，返回 `false`。
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
        if !self.show_view(id, window, cx) {
            self.activate_workspace_tab(WorkspaceRoute::Page(id.to_string()), window, cx);
        }
        view.open(&args, window, cx);
        true
    }

    /// 让停着的插件页对上注册表：启动时恢复上次停的那页，视图登记变了（插件加载、
    /// 卸载、替换）时重新取。视图已经没有了、或者已经摆进布局了就回会话；新建的视图打开
    /// 一次，让它读数据。
    pub(crate) fn sync_page_route(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let WorkspaceRoute::Page(id) = self.active_tab().clone() else {
            return;
        };
        if self.shell_view.laid_out.contains(id.as_str()) {
            self.nav.set_active(WorkspaceRoute::Session);
            self.show_view(&id, window, cx);
            return;
        }
        match api::ensure_view(&id, window, cx) {
            Some((view, true)) => view.open(&serde_json::Value::Null, window, cx),
            Some((_, false)) => {}
            None => self.nav.set_active(WorkspaceRoute::Session),
        }
    }

    /// 宿主事件里工作台自己要管的：视图登记变了要重装布局（新登记的补上、撤下的变
    /// 占位）、对路由，侧栏入口要重画。
    pub(crate) fn subscribe_host_events(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            &api::events(cx),
            window,
            |this, _, event: &HostEvent, window, cx| match event {
                HostEvent::ViewsChanged => {
                    this.reload_layout(window, cx);
                    this.sync_page_route(window, cx);
                    cx.notify();
                }
                HostEvent::LayoutsChanged => this.sync_layout_choice(window, cx),
                HostEvent::ThemesChanged => {
                    if let Some(appearance) = cx.try_global::<crate::settings::Appearance>() {
                        let selected = appearance.theme_name.clone();
                        crate::settings::apply_named_theme(&selected, cx);
                        cx.refresh_windows();
                    }
                    cx.notify();
                }
                _ => {}
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
        let project = self.active_project_ref(cx);
        if self.plugin_project != project {
            self.plugin_project = project;
            api::emit(HostEvent::ActiveProjectChanged, cx);
        }
    }

    /// 看得见的视图变了就告诉插件：布局里所在组显示着的，加上舞台上开成当前页的那个。
    /// Dock 变了（`sync_shell_view`）和路由变了（可见刷新）时调。
    pub(crate) fn note_shown_views(&mut self, cx: &mut Context<Self>) {
        let mut shown: HashSet<String> = self.shell_view.shown.clone();
        if let WorkspaceRoute::Page(id) = self.active_tab()
            && self
                .shell_view
                .shown
                .contains(crate::builtin_views::Builtin::Stage.id())
        {
            shown.insert(id.clone());
        }
        if self.plugin_shown_views != shown {
            self.plugin_shown_views = shown;
            api::emit(HostEvent::ShownViewsChanged, cx);
        }
    }
}
