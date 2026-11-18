//! 宿主自己登记的视图：出口、设置（导航、内容和宿主自己的几页设置）。和插件视图在同一张
//! 注册表里，布局按 id 摆。
//!
//! 出口（[`api::OUTLET_VIEW`]）是路由的机制：布局里摆它的地方显示路由到的视图（见
//! `router`、`workspace_layout`）。设置的导航和内容是摆设置页的机制，不认识具体哪一页；
//! 宿主自己的那几页（外观、快捷键、面板插件）管的是宿主据此行动的东西，见 `settings`。
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
        title: api::text!("view.outlet"),
        icon: smelt_ui::IconName::AppWindow.path(),
        // 只管出口空着的时候：里面放了视图，标题和顶上那条归谁画都跟着那个视图（见
        // `workspace_shell::show_in_outlet`）。空着什么都不画。
        draws_top_bar: true,
        instances: false,
        settings: None,
        fills: Vec::new(),
        build: Rc::new(move |_window, cx| -> Rc<dyn ViewHandle> {
            let workspace = workspace.clone();
            Rc::new(cx.new(|cx| OutletView::new(workspace, cx)))
        }),
    }
}

/// 出口：画开着的这页出口里显示的视图。放什么由路由定，视图由 `Workspace::sync_outlet`
/// 建好（按实例建的由 `Workspace::open_instance`）；这里只取，不建。
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
            let workspace = workspace.read(cx);
            let id = workspace.shell_view.outlet.clone()?;
            // 按实例建的那份归工作台留着，别的在注册表里。
            match workspace.outlet_instance() {
                Some(instance) if **instance.id() == *id => Some(instance.view()),
                _ => api::built_view(&id, cx).map(|view| view.view()),
            }
        });
        // 空着又贴着窗口顶：顶上那条归它，至少留一条能拖窗口的顶带。
        let area = api::safe_area(cx.entity_id(), cx);
        let empty_bar = (content.is_none() && area.title_bar)
            .then(|| smelt_ui::window_bar::window_bar("outlet-empty-bar", &area));
        div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .children(empty_bar)
            .children(content)
    }
}

impl PluginView for OutletView {
    /// 放什么由路由定，参数没有用处。
    fn open(&mut self, _: &serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}
