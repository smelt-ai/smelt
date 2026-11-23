//! Smelt 的 NSH（longbridge/nsh）A2 主窗口外壳与插件运行时。
//!
//! 架构（A2）：单引擎线程 + 单界面树（[`AppView`]）+ 每插件独立加固 QuickJS VM：
//! - 主壳 `shell/MainShell.tsx` 承载各页 `nsh/base <DockArea>`，布局受控并与
//!   `smelt_plugin_api::DockAreaState` 无损互转（[`shell`]、[`shell_layout`]）；
//! - 自动发现、运行时打包并热重载自带与用户插件（[`manifest`]、[`plugins`]），装改删不用重编；
//! - 宿主纯同步 Rust 模块与原生矢量图标 `smelt/host`（[`module`]）、每插件独立 SQLite 存储（[`store`]）、
//!   原生重组件桥 `smelt.view`（[`view`]）、统一宿主状态/事件/动作桥（[`host`]）、
//!   原生小人/标题带/毛玻璃坐标（[`components`]）和主题投影（[`theme`]）。

mod components;
mod desktop;
mod host;
pub mod manifest;
mod module;
mod os_apps;
pub mod plugins;
pub mod shell;
pub mod shell_layout;
mod store;
mod terminal;
mod theme;
mod view;

pub use components::{BackdropHooks, BackdropPlacement};
pub use manifest::{LoadedPluginInfo, MANIFEST_FILE, PluginManifest, PluginSource, SourceDir};
pub use shell::{MainShell, MainShellEvent, OutletTarget, ShellConfig, ShellLabels};
pub use shell_layout::{ComputedShellView, WindowCorners};

use std::{cell::RefCell, path::Path, rc::Rc};

use gpui::{App, Entity};
use nsh_core::element::description::SetDescription;
use nsh_platform_gpui::AppView;

/// 离线检查一个 NSH 插件目录（清单、locales、主题文件、TSX 编译与外部 import 校验）。
pub fn check_plugin(dir: &Path) -> Result<PluginManifest, String> {
    manifest::check_plugin(dir, ui_sets())
}

/// 打包时认的那几套，和交给引擎的一致。
fn ui_sets() -> Vec<SetDescription> {
    vec![
        SetDescription::from(nsh_base::registry()),
        SetDescription::from(module::registry()),
    ]
}

fn register_components(
    app: &AppView,
    backdrop: Rc<RefCell<components::BackdropHooks>>,
    safe_area: Option<view::SafeAreaLookup>,
) {
    app.register_component(
        view::NAME,
        Rc::new(move |cx| view::SmeltView::make(cx, safe_area.clone())),
    );
    app.register_component(components::BUDDY, Rc::new(components::Buddy::make));
    app.register_component(components::WINDOW_BAR, Rc::new(components::WindowBar::make));
    app.register_component(
        components::WINDOW_DRAG,
        Rc::new(components::WindowDrag::make),
    );
    app.register_component(
        components::BACKDROP_REGION,
        Rc::new(move |cx| components::BackdropRegion::make(cx, backdrop.clone())),
    );
    app.register_component(desktop::NAME, Rc::new(desktop::SmeltDesktop::make));
    app.register_component(terminal::NAME, Rc::new(terminal::SmeltTerminal::make));
}

/// 把 smelt 当前的主题和深浅交给 NSH。建好时一次，换主题时再一次。
pub(crate) fn apply_theme(app: &Entity<AppView>, cx: &mut App) {
    let (nsh_theme, appearance) = (theme::nsh_theme(), theme::appearance());
    app.update(cx, |view, cx| {
        view.set_theme(Some(nsh_theme), cx);
        view.set_appearance(appearance, cx);
    });
}

#[doc(hidden)]
pub mod testing {
    use nsh_platform_gpui::{ComponentContext, NativeComponent};

    pub const BUDDY: &str = crate::components::BUDDY;

    pub fn make_buddy(cx: ComponentContext) -> Box<dyn NativeComponent> {
        crate::components::Buddy::make(cx)
    }
}
