//! 宿主这一侧的插件接口：实现 [`smelt_plugin_api::Host`]，登记内置插件，把插件页面挂上
//! 工作台的路由。设计见 `docs/app-plugins.md`。

use std::cell::RefCell;
use std::path::Path;
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
    /// 打开一个插件页面。没登记过这个 id 就什么都不做，返回 `false`。
    pub(crate) fn open_page(
        &mut self,
        id: &str,
        args: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(page) = self.page_handle(id, window, cx) else {
            return false;
        };
        self.activate_workspace_tab(WorkspaceRoute::Page(id.to_string()), window, cx);
        page.open(&args, window, cx);
        true
    }

    /// 启动时恢复到上次停的插件页面。那一页已经没有了（插件下线）就回会话。
    pub(crate) fn restore_page_route(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let WorkspaceRoute::Page(id) = self.active_tab().clone() else {
            return;
        };
        match self.page_handle(&id, window, cx) {
            Some(page) => page.open(&serde_json::Value::Null, window, cx),
            None => self.nav.set_active(WorkspaceRoute::Session),
        }
    }

    /// 页面第一次打开时建，之后一直用这一份。
    fn page_handle(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Rc<dyn api::PageHandle>> {
        if let Some(page) = self.pages.get(id) {
            return Some(page.clone());
        }
        let spec = api::page(id, cx)?;
        let page = (spec.build)(window, cx);
        self.pages.insert(spec.id, page.clone());
        Some(page)
    }

    pub(crate) fn render_page(&self, id: &str) -> AnyElement {
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .children(self.pages.get(id).map(|page| page.view()))
            .into_any_element()
    }
}
