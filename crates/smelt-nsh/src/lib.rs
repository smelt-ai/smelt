//! 试点：把 NSH（longbridge/nsh）的界面嵌进 smelt，看插件层换成 NSH 行不行得通。
//!
//! `pilot/` 是一块 TSX 写的面板，构建时打包成字节码（`build.rs`）。这里把它登记成视图
//! `nsh-pilot/pilot`，入口列表里点开放进出口。一块视图一个 [`AppView`]，自带一条引擎线程；
//! 它画在 GPUI 里，主题跟着 smelt 的（[`theme`]），背景不画、透出窗口材质。
//!
//! 面板按方案 2 摆：整块是 NSH，左边对话列表，右边嵌真的对话和终端，量重组件进 NSH 的代价。
//! 和宿主打交道的几条路：
//! - 宿主的 Rust 模块 `smelt/pilot`（[`module`]）：推本机的对话列表、分配会话 id。模块函数
//!   跑在引擎线程，宿主的东西经主线程推过去。
//! - 宿主登记的视图作原生组件 `smelt.view`（[`view`]）：终端、对话这些重组件。
//! - smelt-ui 的按钮登记成 `smelt.button`（[`button`]）。
//! - 主题：smelt-ui 的色板投影成 NSH 的 shadcn 语义位。

mod button;
mod module;
mod theme;
mod view;

use std::{path::PathBuf, rc::Rc};

use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable as _, IntoElement,
    ParentElement as _, Render, Styled as _, Subscription, Window, div,
};
use nsh_platform_gpui::{App as NshApp, AppOptions, AppView, AssetBase, Bundle};
use smelt_plugin_api::{self as api, PluginView, ViewHandle, ViewSpec};

const OWNER: &str = "nsh-pilot";

/// 登记试点视图，接上宿主模块。App 启动、主窗口建好后调一次。
pub fn register(cx: &mut App) {
    // 引擎能按名字接的：NSH 自带的 `nsh/base`，和宿主的 `smelt/pilot`。进程级，每个引擎都给。
    nsh_platform_gpui::set_ui_sets(vec![nsh_base::registry(), module::registry()]);
    module::serve(cx);
    let spec = ViewSpec {
        name: "pilot".into(),
        title: "NSH 试点".into(),
        icon: smelt_ui::IconName::LayoutDashboard.path(),
        draws_top_bar: false,
        settings: None,
        instances: false,
        fills: Vec::new(),
        build: Rc::new(|window, cx| -> Rc<dyn ViewHandle> {
            Rc::new(cx.new(|cx| PilotView::new(window, cx)))
        }),
    };
    if let Err(error) = api::register_views(OWNER, vec![spec], cx) {
        eprintln!("[nsh-pilot] 视图登记失败：{error}");
    }
}

/// 构建时打好的包。
fn bundle_dir() -> PathBuf {
    PathBuf::from(env!("OUT_DIR")).join("pilot")
}

/// 一块试点面板：外面一层是 smelt 的视图，里面是 NSH 的 [`AppView`]。
struct PilotView {
    app: Entity<AppView>,
    _theme: Subscription,
}

impl PilotView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dir = bundle_dir();
        let options = AppOptions {
            asset_base: Some(AssetBase::Directory(dir.join("assets"))),
            // 不画底，透出窗口材质（毛玻璃）。
            transparent: true,
            ..AppOptions::default()
        };
        let app = cx.new(|cx| AppView::new(options, window, cx));
        app.read(cx)
            .register_component(button::NAME, Rc::new(button::SmeltButton::make));
        app.read(cx)
            .register_component(view::NAME, Rc::new(view::SmeltView::make));
        apply_theme(&app, cx);
        // SAFETY: 包是本 crate 构建时打的（`build.rs`），和 App 自己的资源一样可信。
        match unsafe { Bundle::read_trusted(&dir.join("bundle.nshc")) } {
            Ok(bundle) => app.update(cx, |view, _| {
                view.on_error(|message| eprintln!("[nsh-pilot] {message}"));
                view.load(NshApp::Js(bundle));
            }),
            Err(error) => eprintln!("[nsh-pilot] 读不了包：{error}"),
        }
        let _theme = cx.observe_global::<smelt_ui::theme_catalog::ActiveTheme>(|this, cx| {
            apply_theme(&this.app, cx);
        });
        Self { app, _theme }
    }
}

/// 把 smelt 当前的主题和深浅交给 NSH。建好时一次，换主题时再一次。
fn apply_theme(app: &Entity<AppView>, cx: &mut App) {
    let (nsh_theme, appearance) = (theme::nsh_theme(), theme::appearance());
    app.update(cx, |view, cx| {
        view.set_theme(Some(nsh_theme), cx);
        view.set_appearance(appearance, cx);
    });
}

impl Render for PilotView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.app.clone())
    }
}

impl PluginView for PilotView {
    fn open(&mut self, _args: &serde_json::Value, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn focus_target(&self, cx: &App) -> Option<FocusHandle> {
        Some(self.app.focus_handle(cx))
    }
}
