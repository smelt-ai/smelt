//! 试点：把 NSH（longbridge/nsh）的界面嵌进 smelt，看插件层换成 NSH 行不行得通。
//!
//! `pilot/Shell.tsx` 是面板的外壳，`pilot-plugins/` 下每个目录一个 TSX 插件：都在运行时打包
//! （[`plugins`]），插件装、改、删不用重编（[`live`]）。这里把面板登记成视图 `nsh-pilot/pilot`，
//! 入口列表里点开放进出口。一块视图一个 [`AppView`]，自带一条引擎线程；它画在 GPUI 里，主题
//! 跟着 smelt 的（[`theme`]），背景不画、透出窗口材质。
//!
//! 面板按方案 2 摆：整块是 NSH，左边由独立插件 VM 组合而成，右边嵌真的对话和终端，量重组件进
//! NSH 的代价。和宿主打交道的几条通用能力路：
//! - 宿主的纯同步 Rust 模块 `smelt/pilot`（[`module`]）：路径规范、项目名、UUID、系统标识。
//! - 每插件独立的 SQLite 存储能力（[`store`]）：封装 `smelt_plugin_api::PluginStore`，后台串行落盘。
//! - 宿主登记的视图作原生组件 `smelt.view`（[`view`]）：终端、对话这些重组件。
//! - 插件要的宿主状态、离散事件与通用动作（[`host`]）。
//! - smelt-ui 的按钮登记成 `smelt.button`（[`button`]），插件拼不出的几样在 [`components`]。
//! - 主题：smelt-ui 的色板投影成 NSH 的 shadcn 语义位。

mod button;
mod components;
mod host;
mod live;
mod module;
pub mod plugins;
mod store;
#[cfg(test)]
mod tests;
mod theme;
mod view;

use std::{path::PathBuf, rc::Rc};

use gpui::{
    App, AppContext as _, ClipboardItem, Context, Entity, EntityId, FocusHandle, Focusable as _,
    IntoElement, ParentElement as _, Render, Styled as _, Subscription, Task, Window, div,
};
use nsh_core::element::description::SetDescription;
use nsh_platform_gpui::{AppOptions, AppView};
use serde_json::Value;
use smelt_plugin_api::{self as api, HostEvent, PluginView, ViewHandle, ViewSpec};
use tokio::sync::mpsc;

use crate::plugins::{Change, PluginHost};

const OWNER: &str = "nsh-pilot";
const SHELL: &str = include_str!("../pilot/Shell.tsx");

/// 登记试点视图，接上宿主模块。App 启动、主窗口建好后调一次。
pub fn register(cx: &mut App) {
    // 引擎能按名字接的：NSH 自带的 `nsh/base`，和宿主的 `smelt/pilot`。进程级，每个引擎都给。
    nsh_platform_gpui::set_ui_sets(vec![nsh_base::registry(), module::registry()]);
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
    plugin_host: &PluginHost,
    bridge: &host::Bridge,
    sources: PathBuf,
    stage: PathBuf,
    cx: &mut App,
) -> Task<()> {
    let (sender, mut changes) = mpsc::unbounded_channel();
    let _live = live::Live::start(sources, stage, SHELL, ui_sets(), bridge.clone(), sender);
    let err_host = plugin_host.clone();
    app.update(cx, |view, _| {
        view.on_app_error(move |error| {
            eprintln!("[nsh-pilot] {error}");
            err_host.on_app_error(error);
        });
    });
    let apply_host = plugin_host.clone();
    let cleanup_bridge = bridge.clone();
    let app = app.downgrade();
    cx.spawn(async move |cx| {
        let _keep_live = _live;
        while let Some(change) = changes.recv().await {
            let removed = match &change {
                Change::Remove(id) => Some(id.clone()),
                _ => None,
            };
            if app
                .update(cx, |view, cx| {
                    if let Some(id) = removed {
                        cleanup_bridge.forget_plugin(&id);
                        api::unregister_topics(&id, cx);
                    }
                    apply_host.apply(view, change);
                })
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
    plugins: PluginHost,
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

        let plugins = PluginHost::new();
        let store_hub = store::StoreHub::global();
        store_hub.serve(&plugins);
        let bridge = host::Bridge::default();
        let mut actions = bridge.serve(&plugins);
        let rem = f32::from(window.rem_size());
        bridge.publish(&plugins, pilot, rem, cx);

        let installing = boot(
            &app,
            &plugins,
            &bridge,
            plugin_dir(),
            std::env::temp_dir().join(format!("smelt-nsh-{}", uuid::Uuid::new_v4())),
            cx,
        );
        let serving = cx.spawn(async move |_, cx| {
            while let Some(action) = actions.recv().await {
                cx.update(|cx| handle_action(action, cx));
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
            this.bridge.forward_event(&this.plugins, event);
        });
        Self {
            app,
            plugins,
            bridge,
            rem,
            _tasks: [installing, serving],
            _subscriptions: [themed, events],
        }
    }

    fn publish(&self, cx: &Context<Self>) {
        self.bridge
            .publish(&self.plugins, cx.entity_id(), self.rem, cx);
    }
}

fn handle_action(action: host::Action, cx: &mut App) {
    match action {
        host::Action::Run {
            command,
            args,
            call,
        } => {
            let result = api::run(&command, args, cx);
            // 各跑各的：一条慢命令不挡着后面的。面板关了也照样跑完、照样回。
            cx.spawn(async move |_| call.reply(result.await)).detach();
        }
        host::Action::Call {
            machine,
            method,
            params,
            timeout,
            call,
        } => {
            let client = api::host(cx).client(&machine, cx);
            cx.background_executor()
                .spawn(async move {
                    let result = match client {
                        Some(client) => client
                            .call_method(method, params, timeout)
                            .map_err(api::call_error_text),
                        None => Err(api::machine_removed()),
                    };
                    call.reply(result);
                })
                .detach();
        }
        host::Action::ContextMenuItems {
            menu,
            machine,
            context,
            call,
        } => {
            let items: Vec<Value> = api::context_menu_items(&menu, &machine, cx)
                .iter()
                .map(|item| host::context_menu_json(item, &context, cx))
                .collect();
            call.resolve(Value::Array(items));
        }
        host::Action::ClipboardWrite { text, call } => {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            call.resolve(Value::Null);
        }
        host::Action::RegisterTopics {
            plugin,
            topics,
            call,
        } => match api::register_topics(&plugin, topics, cx) {
            Ok(()) => call.resolve(Value::Null),
            Err(error) => call.reject(format!("register_topics：{error}")),
        },
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
