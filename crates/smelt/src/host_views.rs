//! 宿主自己登记的视图：出口、设置（导航和内容）。和插件视图在同一张注册表里，布局按 id 摆。
//!
//! 出口（[`api::OUTLET_VIEW`]）是路由的机制：布局里摆它的地方显示路由到的视图（见
//! `router`、`workspace_layout`）。设置是还没拆成插件的功能，先由宿主登记。
//! 同一个来源的视图整组登记，所以都在这里一起交。

use std::rc::Rc;

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _, Subscription,
    WeakEntity, Window, div,
};
use smelt_plugin_api::{self as api, PluginView, ViewHandle, ViewSpec};

use crate::Workspace;

/// 宿主自己登记视图、页和布局用的来源。
pub(crate) const HOST_OWNER: &str = "smelt";

/// 登记宿主的视图，和设置那一页。工作台建好时调；窗口重开时新工作台再登记一遍，替换掉
/// 指着旧工作台的那份视图，页和布局没变就不动。
pub(crate) fn register(workspace: WeakEntity<Workspace>, cx: &mut gpui::App) {
    crate::settings::register_page(cx);
    let mut specs = vec![outlet_spec(workspace.clone())];
    specs.extend(crate::settings::view_specs(workspace));
    if let Err(error) = api::register_views(HOST_OWNER, specs, cx) {
        eprintln!("[plugins] 宿主视图登记失败：{error}");
    }
}

fn outlet_spec(workspace: WeakEntity<Workspace>) -> ViewSpec {
    ViewSpec {
        name: "outlet".into(),
        title: "内容".into(),
        icon: gpui_kit_assets::IconName::AppWindow.path(),
        per_machine: false,
        // 只管出口空着的时候：里面放了视图，标题和顶上那条归谁画都跟着那个视图（见
        // `workspace_shell::show_in_outlet`）。空着什么都不画。
        draws_top_bar: true,
        settings: None,
        build: Rc::new(move |_window, cx| -> Rc<dyn ViewHandle> {
            let workspace = workspace.clone();
            Rc::new(cx.new(|cx| OutletView::new(workspace, cx)))
        }),
    }
}

/// 出口：画开着的这页出口里显示的视图。放什么由路由定，视图由 `Workspace::sync_outlet`
/// 建好；这里只取，不建。
struct OutletView {
    workspace: WeakEntity<Workspace>,
    /// 出口里换内容时工作台会通知。
    _refresh: Subscription,
}

impl OutletView {
    fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        let _refresh = match workspace.upgrade() {
            Some(host) => cx.observe(&host, |_, _, cx| cx.notify()),
            None => Subscription::new(|| {}),
        };
        Self {
            workspace,
            _refresh,
        }
    }
}

impl Render for OutletView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = self.workspace.upgrade().and_then(|workspace| {
            let id = workspace.read(cx).shell_view.outlet.clone()?;
            api::built_view(&id, cx).map(|view| view.view())
        });
        div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .children(content)
    }
}

impl PluginView for OutletView {
    /// 放什么由路由定，参数没有用处。
    fn open(&mut self, _: &serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}
