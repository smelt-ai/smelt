//! smelt 的插件：gpui-shell 脚本应用。
//!
//! 插件目录里放 gpui-shell 的 manifest `gpui-shell.json` 和入口脚本，界面由 GPUI 原生
//! 绘制，脚本跑在 UI 线程的 QuickJS 里。每个插件挂成 Tool Panel 的一个 tab。
//!
//! 插件当作可信代码，全部共用一份宿主定的权限：文件访问限在用户主目录内（官方
//! `fs/promises` 的授权根），smelt 的通用 HostModule（`smelt-fs` 见 `host_fs`，
//! `smelt-workspace` 见 `host_workspace`）用 `export_module` 注册一次，所有插件都能导入。这就是官方给嵌入宿主的
//! 写法：宿主先装好默认 Policy 和模块，再 `load_application` / `mount_application`，
//! 视图带着默认 Policy 出生。插件 manifest 里的 `capabilities` 不生效。
//!
//! 没用上游的 `PluginManager`：它按 manifest 给每个插件新建一份空 Policy，拿不到
//! 宿主注册的模块。这里只用公开的 `PluginManifest::read` 发现插件。
//!
//! 插件从哪来（前面的赢得同名 id，用户装的副本可以盖过自带的）：
//! 1. `~/.smelt/plugins/`：用户自己装的。
//! 2. App 自带的 `Contents/Resources/plugins/`，只读加载。
//! 3. debug 构建直接读仓库里的 `plugins/`，改完脚本重启即可。
//!
//! 脚本可以从 `"gpui-component"` 导入带皮肤的控件。这些控件的 dialog、sheet、
//! notification 走组件库的 `Root`，和这扇窗口的根一致。`gpui-shell` 自己的
//! `window.open_dialog` 要找 `ShellRoot`（窗口第一层，或 `Root` 里面那一层），
//! 工作台没有包它，这条暂时用不了。

mod host_fs;
mod host_workspace;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::Context as _;
use gpui::*;
use gpui_component::ActiveTheme as _;
use crate::ui_theme;
use gpui_shell::plugin::{MANIFEST_FILE, PluginManifest};
use gpui_shell::policy::{self, Policy};
use gpui_shell::{Capabilities, ScriptView, ShellRuntime};

use host_workspace::HostContext;

/// 用户安装插件的目录（`~/.smelt` 下），也是 App 包里、仓库里放插件的目录名。
const PLUGIN_DIR: &str = "plugins";

thread_local! {
    // GPUI 实体只活在 UI 线程，注册表也就只放这里。
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

#[derive(Default)]
struct Registry {
    runtime: Option<Rc<ShellRuntime>>,
    /// 持有已挂载的插件视图。清掉就卸载：视图拥有的任务随视图一起取消。
    panels: Vec<PluginPanel>,
    /// 所有插件共享的宿主上下文，`smelt-workspace.context()` 读它。
    context: Rc<RefCell<HostContext>>,
}

struct PluginPanel {
    id: String,
    title: String,
    /// 加载失败也占一个 tab，把错误画出来：静默消失的插件最难排查。
    content: Result<Entity<ScriptView>, String>,
}

/// Tool Panel 里插件 tab 的槽位。`ToolPanelTab` 必须是 `Copy`，所以运行时用下标，
/// 存档时换回插件 id（见 `key_for` / `slot_for_key`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PluginTabSlot(pub(crate) u16);

/// 发现并挂载全部插件。在工作台创建时调用（不是 render）：这里会读盘、执行脚本。
///
/// 必须先于工作台存档读取：存档里的 tab 以插件 id 记录，注册表为空时会降级成 Files。
/// 工作台窗口同一时刻只有一个；重开窗口时旧的插件全部卸载、重新加载。
pub(crate) fn mount_all(window: &mut Window, cx: &mut App) {
    let runtime = match runtime(cx) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[plugins] 运行时启动失败：{error:#}");
            return;
        }
    };
    // 先卸载上一个窗口留下的插件。
    unmount_all();

    let panels = discover()
        .into_iter()
        .map(|(root, manifest)| {
            let id = manifest.id().to_string();
            let title = manifest.name().to_string();
            let content = runtime
                .load_application(&root, manifest.entry())
                .and_then(|application| runtime.mount_application(&application, window, cx))
                .map_err(|error| format!("{error:#}"));
            if let Err(error) = &content {
                eprintln!("[plugins] {id} 加载失败：{error}");
            }
            PluginPanel { id, title, content }
        })
        .collect();

    REGISTRY.with(|registry| registry.borrow_mut().panels = panels);
}

/// 卸载全部插件。视图拥有的任务随视图一起取消。
fn unmount_all() {
    let panels = REGISTRY.with(|registry| std::mem::take(&mut registry.borrow_mut().panels));
    drop(panels);
}

/// 应用退出时释放插件视图、宿主模块和运行时。
///
/// 它们都挂在线程局部的注册表上。留到线程局部变量析构时再释放，gpui-shell 的
/// 任务表（也是线程局部，且建得比注册表晚、析构得比它早）已经没了，释放时
/// 取消任务会 panic，析构里的 panic 直接中止进程。
pub(crate) fn shutdown() {
    unmount_all();
    gpui_shell::clear_exported_modules();
    let runtime = REGISTRY.with(|registry| registry.borrow_mut().runtime.take());
    drop(runtime);
}

/// 按目录顺序找插件，同名 id 先到先得。坏的 manifest 记一笔就跳过，不执行任何脚本。
fn discover() -> Vec<(PathBuf, PluginManifest)> {
    let mut found: Vec<(PathBuf, PluginManifest)> = Vec::new();
    for directory in plugin_dirs() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut roots: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.join(MANIFEST_FILE).is_file())
            .collect();
        // 目录遍历顺序随文件系统而变；不排序的话 tab 顺序每次启动都可能不同。
        roots.sort();
        for root in roots {
            match PluginManifest::read(&root) {
                Ok(manifest) => {
                    if !found.iter().any(|(_, seen)| seen.id() == manifest.id()) {
                        found.push((root, manifest));
                    }
                }
                Err(error) => eprintln!("[plugins] {error}"),
            }
        }
    }
    found
}

/// 一个进程一个运行时；重开窗口时复用它。第一次创建时装好全部插件共用的
/// 默认 Policy 和宿主模块，顺序是先 Policy 后模块：`export_module` 往默认 Policy
/// 上加，先装的会被后面的 `set_default` 整个换掉。
fn runtime(cx: &mut App) -> anyhow::Result<Rc<ShellRuntime>> {
    if let Some(runtime) = REGISTRY.with(|registry| registry.borrow().runtime.clone()) {
        return Ok(runtime);
    }
    // 不调 `gpui_shell::init`，也不调 `init_with_components`。后者会再跑一遍目录里
    // 登记的 `gpui_component::init`，而启动时已经调用过；`gpui_base::init` 重复做会
    // 重复绑键。样式表首次用到时自会建好。
    //
    // 只要组件目录，不用它的开窗口函数：那是给 gpui-shell 自己当应用入口用的。
    let components =
        gpui_shell::components::components().context("构建 gpui-component 脚本组件目录")?;
    let runtime = ShellRuntime::new_with_components(cx, components)?;
    // 编辑器文件（gpui-kit.d.ts 等）只在开发构建里写：发布版从 .app 里只读加载，
    // 写进已签名的包会破坏签名。
    runtime.set_editor_files(cfg!(debug_assertions));

    let home = dirs::home_dir().context("无法定位用户主目录")?;
    let home = std::fs::canonicalize(&home).unwrap_or(home);
    policy::set_default(
        Policy::new().with_capabilities(
            Capabilities::new()
                .read_roots([home.clone()])
                .write_roots([home.clone()]),
        ),
    );
    let context = REGISTRY.with(|registry| registry.borrow().context.clone());
    let host_error = |error: gpui_shell::HostError| anyhow::anyhow!(error.message().to_string());
    let fs = Rc::new(host_fs::HostFs::new(home.clone()));
    gpui_shell::export_module(host_fs::fs_module(fs)).map_err(host_error)?;
    gpui_shell::export_module(host_workspace::workspace_module(home, context))
        .map_err(host_error)?;

    REGISTRY.with(|registry| registry.borrow_mut().runtime = Some(runtime.clone()));
    Ok(runtime)
}

fn plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = smelt_paths::smelt_home() {
        dirs.push(home.join(PLUGIN_DIR));
    }
    if let Some(bundled) = bundled_plugin_dir() {
        dirs.push(bundled);
    }
    // 开发期直接加载仓库里的插件，改完脚本重启即可，不必先装。
    #[cfg(debug_assertions)]
    dirs.push(repo_plugin_dir());
    dirs
}

/// App 自带插件在 .app 里的位置；不是从 .app 启动（cargo run）时没有。
fn bundled_plugin_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    if macos.file_name()? != "MacOS" || contents.file_name()? != "Contents" {
        return None;
    }
    let dir = contents.join("Resources").join(PLUGIN_DIR);
    dir.is_dir().then_some(dir)
}

/// 仓库里的第一方插件。debug 构建直接从这里加载；测试也从这里校验清单。
#[cfg(any(debug_assertions, test))]
fn repo_plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(PLUGIN_DIR)
}

fn with_panels<R>(read: impl FnOnce(&[PluginPanel]) -> R) -> R {
    REGISTRY.with(|registry| read(&registry.borrow().panels))
}

pub(crate) fn slots() -> Vec<PluginTabSlot> {
    with_panels(|panels| {
        (0..panels.len())
            .filter_map(|index| u16::try_from(index).ok().map(PluginTabSlot))
            .collect()
    })
}

pub(crate) fn title(slot: PluginTabSlot) -> String {
    with_panels(|panels| {
        panels
            .get(usize::from(slot.0))
            .map(|panel| panel.title.clone())
            .unwrap_or_default()
    })
}

/// 存档里的稳定身份：插件 id。
pub(crate) fn key_for(slot: PluginTabSlot) -> Option<String> {
    with_panels(|panels| {
        panels
            .get(usize::from(slot.0))
            .map(|panel| panel.id.clone())
    })
}

pub(crate) fn slot_for_key(key: &str) -> Option<PluginTabSlot> {
    with_panels(|panels| {
        panels
            .iter()
            .position(|panel| panel.id == key)
            .and_then(|index| u16::try_from(index).ok())
            .map(PluginTabSlot)
    })
}

/// 把工作台的当前项目同步给插件。只比较、写内存，可以在 render 里调。
pub(crate) fn set_project_root(project_root: Option<String>) {
    let project_root = project_root.map(PathBuf::from);
    REGISTRY.with(|registry| {
        let registry = registry.borrow();
        let mut context = registry.context.borrow_mut();
        if context.project_root != project_root {
            context.project_root = project_root;
            context.generation += 1;
        }
    });
}

/// 只取注册表里已挂载的视图，不做 IO。
///
/// 底色由宿主铺，和文件树等内置面板一样是实底的舞台色：主题原色可能带透明度，
/// 插件自己铺会透出窗口后面的东西。插件只画内容，不画页面底。
pub(crate) fn render(slot: PluginTabSlot, cx: &App) -> AnyElement {
    let content = with_panels(|panels| {
        panels
            .get(usize::from(slot.0))
            .map(|panel| panel.content.clone())
    });
    match content {
        Some(Ok(view)) => div()
            .flex_1()
            .min_h_0()
            .size_full()
            .bg(rgb(ui_theme::bg_stage()))
            .child(view)
            .into_any_element(),
        Some(Err(error)) => div()
            .flex_1()
            .min_h_0()
            .p_4()
            .text_sm()
            .text_color(cx.theme().danger)
            .child(format!("插件加载失败：{error}"))
            .into_any_element(),
        None => div().flex_1().min_h_0().into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::{MANIFEST_FILE, PluginManifest, repo_plugin_dir};

    /// 仓库里每个插件的清单都要能解析、入口存在、id 不重复。本地 clone 进来的
    /// 外部仓库（没有 `gpui-shell.json`）不是插件，跳过。
    #[test]
    fn repo_plugins_have_valid_manifests() {
        let dir = repo_plugin_dir();
        let mut ids = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let root = entry.path();
            if !root.join(MANIFEST_FILE).is_file() {
                continue;
            }
            let manifest = PluginManifest::read(&root)
                .unwrap_or_else(|error| panic!("{}：{error}", root.display()));
            assert!(
                root.join(manifest.entry()).is_file(),
                "{} 缺少入口 {}",
                root.display(),
                manifest.entry()
            );
            assert!(
                ids.insert(manifest.id().to_string()),
                "插件 id 重复：{}",
                manifest.id()
            );
        }
        assert!(!ids.is_empty(), "{} 下没有插件", dir.display());
    }
}
