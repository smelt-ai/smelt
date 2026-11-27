//! Smelt 的 NSH（longbridge/nsh）A2 主窗口外壳与插件运行时。
//!
//! 架构（A2）：单引擎线程 + 单界面树（[`AppView`]）+ 每插件独立加固 QuickJS VM：
//! - 主壳 `shell/MainShell.tsx` 承载各页 `nsh/base <DockArea>`，布局受控并与
//!   `smelt_plugin_api::DockAreaState` 无损互转（[`shell`]、[`shell_layout`]）；
//! - 自动发现、运行时打包并热重载自带与用户插件（[`manifest`]、[`plugins`]），装改删不用重编；
//! - 宿主纯同步 Rust 模块与原生矢量图标 `smelt/host`（[`module`]）、每插件独立 SQLite 存储（[`store`]）、
//!   原生重组件桥 `smelt.view`（[`view`]）、统一宿主状态/事件/动作桥（[`host`]）、
//!   原生标题带/毛玻璃坐标（[`components`]）和主题投影（[`theme`]）。

mod bytecode_cache;
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
mod types;
mod view;

pub use components::{BackdropHooks, BackdropPlacement};
pub use manifest::{
    DeclarativeLayoutSpec, DeclarativePageSpec, LoadedPluginInfo, MANIFEST_FILE, PluginCheckReport,
    PluginManifest, PluginSource, SourceDir, StageReport, discover_module_packages,
    stage_builtin_plugins,
};
pub use plugins::{ModulePackage, source_fingerprint};
pub use shell::{
    ConfirmDialogState, DirPickerDialogState, DirPickerEntryState, HostDialogAction,
    HostDialogsState, MainShell, MainShellEvent, OutletTarget, QuitDialogState, ShellConfig,
    ShellLabels,
};
pub use shell_layout::{ComputedShellView, WindowCorners};
pub use types::{
    PLUGIN_TSCONFIG, PLUGIN_TSCONFIG_BASE, TypesWriteReport, plugin_type_definitions,
    write_plugin_types,
};

use std::{cell::RefCell, path::Path, rc::Rc};

use gpui::{App, Entity};
use nsh_core::element::description::SetDescription;
use nsh_platform_gpui::AppView;

/// 插件 id 是否合法（仅允许小写/大小写字母、数字与 `._-`，不能以 `.` 开头）。
pub fn is_valid_plugin_id(id: &str) -> bool {
    plugins::valid_id(id)
}

/// 离线检查一个 NSH 插件目录（清单、locales、主题文件、TSX 打包、import 与 QuickJS 字节码编译），
/// 默认同时扫描同级目录里声明了 `module` 的共享模块插件（主要给内置校验与测试用）。
pub fn check_plugin(dir: &Path) -> Result<PluginCheckReport, String> {
    let sources: Vec<SourceDir> = dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|parent| SourceDir {
            path: parent.to_path_buf(),
            source: PluginSource::Builtin,
            watch: false,
        })
        .into_iter()
        .collect();
    manifest::check_plugin_with_sources(dir, &sources, ui_sets())
}

/// 按调用方传入的多目录优先级（如自带 + 用户覆写）离线检查插件或共享模块。
pub fn check_plugin_with_sources(
    dir: &Path,
    sources: &[SourceDir],
) -> Result<PluginCheckReport, String> {
    manifest::check_plugin_with_sources(dir, sources, ui_sets())
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
    use nsh_core::prelude::Registry;

    pub fn host_registry() -> Registry {
        crate::module::registry()
    }

    /// 宿主交给插件的主题切片（`state().theme`），测试里伪造宿主状态时用同一份。
    pub fn theme_state(rem: f32) -> serde_json::Value {
        crate::theme::plugin_theme(rem)
    }
}
