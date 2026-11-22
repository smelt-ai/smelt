//! 试点：把 NSH（longbridge/nsh）的界面嵌进 smelt，看插件层换成 NSH 行不行得通。
//!
//! `pilot/Shell.tsx` 是面板的外壳，`pilot-plugins/` 下每个目录一个 TSX 插件：都在运行时打包
//! （[`plugins`]），插件装、改、删不用重编（[`live`]）。这里把面板登记成视图 `nsh-pilot/pilot`，
//! 入口列表里点开放进出口。一块视图一个 [`AppView`]，自带一条引擎线程；它画在 GPUI 里，主题
//! 跟着 smelt 的（[`theme`]），背景不画、透出窗口材质。
//!
//! 面板按方案 2 摆：整块是 NSH，左边对话列表，右边嵌真的对话和终端，量重组件进 NSH 的代价。
//! 和宿主打交道的几条路：
//! - 宿主的 Rust 模块 `smelt/pilot`（[`module`]）：推本机的对话列表、分配会话 id。模块函数
//!   跑在引擎线程，宿主的东西经主线程推过去。
//! - 宿主登记的视图作原生组件 `smelt.view`（[`view`]）：终端、对话这些重组件。
//! - 插件要的宿主状态和命令（[`host`]）。
//! - smelt-ui 的按钮登记成 `smelt.button`（[`button`]），插件拼不出的几样在 [`components`]。
//! - 主题：smelt-ui 的色板投影成 NSH 的 shadcn 语义位。

mod button;
mod components;
mod host;
mod live;
mod module;
pub mod plugins;
#[cfg(test)]
mod tests;
mod theme;
mod view;

use std::{path::PathBuf, rc::Rc};

use gpui::{
    App, AppContext as _, Context, Entity, EntityId, FocusHandle, Focusable as _, IntoElement,
    ParentElement as _, Render, Styled as _, Subscription, Task, Window, div,
};
use nsh_core::element::description::SetDescription;
use nsh_platform_gpui::{AppErrorKind, AppOptions, AppView};
use smelt_plugin_api::{self as api, HostEvent, PluginView, ViewHandle, ViewSpec};
use tokio::sync::mpsc;

const OWNER: &str = "nsh-pilot";
const SHELL: &str = include_str!("../pilot/Shell.tsx");

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

/// 打包时认的那几套，和交给引擎的一致。
fn ui_sets() -> Vec<SetDescription> {
    vec![
        SetDescription::from(nsh_base::registry()),
        SetDescription::from(module::registry()),
    ]
}

/// 试点的插件目录：直接读仓库，改了即换。试点只在编它的这台机器上用，不随安装包带。
fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("pilot-plugins")
}

fn register_components(app: &AppView, pilot: EntityId) {
    app.register_component(button::NAME, Rc::new(button::SmeltButton::make));
    app.register_component(view::NAME, Rc::new(view::SmeltView::make));
    app.register_component(components::BUDDY, Rc::new(components::Buddy::make));
    app.register_component(components::ICON, Rc::new(components::Icon::make));
    app.register_component(
        components::WINDOW_BAR,
        Rc::new(move |cx| components::WindowBar::make(cx, pilot)),
    );
}

/// 起引擎：外壳加上 `sources` 里的插件，之后那里变了什么就换什么。
fn boot(
    app: &Entity<AppView>,
    bridge: &host::Bridge,
    sources: PathBuf,
    stage: PathBuf,
    cx: &mut App,
) -> Task<()> {
    let (sender, mut changes) = mpsc::unbounded_channel();
    let live = live::Live::start(sources, stage, SHELL, ui_sets(), bridge.clone(), sender);
    let bridge = bridge.clone();
    app.update(cx, |view, _| {
        view.on_app_error(move |error| {
            eprintln!("[nsh-pilot] {error}");
            if matches!(error.kind, AppErrorKind::Stopped) {
                live.stopped(bridge.take_loading());
            }
        });
    });
    let app = app.downgrade();
    cx.spawn(async move |cx| {
        while let Some(change) = changes.recv().await {
            if app
                .update(cx, |view, _| plugins::apply(view, change))
                .is_err()
            {
                break;
            }
        }
    })
}

/// 一块试点面板：外面一层是 smelt 的视图，里面是 NSH 的 [`AppView`]。
struct PilotView {
    app: Entity<AppView>,
    bridge: host::Bridge,
    /// 插件算字和间距用的 rem，变了重推一次状态。
    rem: f32,
    _tasks: [Task<()>; 2],
    _subscriptions: [Subscription; 2],
}

impl PilotView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let options = AppOptions {
            // 不画底，透出窗口材质（毛玻璃）。
            transparent: true,
            ..AppOptions::default()
        };
        let app = cx.new(|cx| AppView::new(options, window, cx));
        let pilot = cx.entity_id();
        register_components(app.read(cx), pilot);
        apply_theme(&app, cx);

        let bridge = host::Bridge::default();
        let mut runs = bridge.serve(app.read(cx));
        let rem = f32::from(window.rem_size());
        bridge.publish(app.read(cx), pilot, rem, cx);

        let installing = boot(
            &app,
            &bridge,
            plugin_dir(),
            std::env::temp_dir().join(format!("smelt-nsh-{}", uuid::Uuid::new_v4())),
            cx,
        );
        let running = cx.spawn(async move |_, cx| {
            while let Some(host::Run {
                command,
                args,
                call,
            }) = runs.recv().await
            {
                cx.update(|cx| {
                    let result = api::run(&command, args, cx);
                    // 各跑各的：一条慢命令不挡着后面的。面板关了也照样跑完、照样回。
                    cx.spawn(async move |_| call.reply(result.await)).detach();
                });
            }
        });

        let themed = cx.observe_global::<smelt_ui::theme_catalog::ActiveTheme>(|this, cx| {
            apply_theme(&this.app, cx);
            this.publish(cx);
        });
        let events = cx.subscribe(&api::events(cx), |this, _, event: &HostEvent, cx| {
            if host::affects_state(event) {
                this.publish(cx);
            }
        });
        Self {
            app,
            bridge,
            rem,
            _tasks: [installing, running],
            _subscriptions: [themed, events],
        }
    }

    fn publish(&self, cx: &Context<Self>) {
        self.bridge
            .publish(self.app.read(cx), cx.entity_id(), self.rem, cx);
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = f32::from(window.rem_size());
        if self.rem != rem {
            self.rem = rem;
            // 改界面字号没有事件，只有重画：推状态挪到这一轮画完之后。
            let this = cx.weak_entity();
            cx.defer(move |cx| {
                let _ = this.update(cx, |this, cx| this.publish(cx));
            });
        }
        div().size_full().child(self.app.clone())
    }
}

impl PluginView for PilotView {
    fn open(&mut self, _args: &serde_json::Value, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn focus_target(&self, cx: &App) -> Option<FocusHandle> {
        Some(self.app.focus_handle(cx))
    }
}
