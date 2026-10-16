//! gpui-shell 插件（可行性原型）。
//!
//! 与旧插件体系（smelt-plugin-host、bun 进程、WebView、`plugin.json`）完全独立：
//! 插件目录里放 gpui-shell 自己的 manifest `gpui-shell.json` 和入口脚本，界面由 GPUI
//! 原生绘制，脚本跑在 UI 线程的 QuickJS 里。每个插件挂成 Tool Panel 的一个 tab。
//!
//! 发现、授权、加载、卸载都交给 gpui-shell 的 `PluginManager`：每个插件一份自己的
//! Policy（manifest 声明的授权 + smelt 借出的 HostModule，见 `fs`），卸载时由它取消
//! 该插件的全部任务。
//!
//! 插件从哪来（前面的赢得同名 id，用户装的副本可以盖过自带的）：
//! 1. `~/.smelt/shell-plugins/`：用户自己装的。
//! 2. App 自带的 `Contents/Resources/shell-plugins/`，只读加载。
//! 3. debug 构建直接读仓库里的 `shell-plugins/`，改完脚本重启即可。
//!
//! 原型阶段刻意不处理的已知限制：gpui-shell 的 dialog / sheet / toast / tooltip 都按
//! 「窗口根是 `ShellRoot`」去找浮层宿主，而 smelt 的窗口根是 gpui-component 的
//! `Root`，所以脚本里这些 API 用不了，弹层由脚本自己在面板里画。

mod fs;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::Context as _;
use gpui::*;
use gpui_component::ActiveTheme as _;
use gpui_shell::plugin::{PluginManager, PluginManifest};
use gpui_shell::policy::Policy;
use gpui_shell::{ScriptView, ShellRuntime};

use fs::HostContext;

/// 用户安装 gpui-shell 插件的目录（`~/.smelt` 下），也是 App 包里自带插件的目录名。
const PLUGIN_DIR: &str = "shell-plugins";
/// 插件数据根（`~/.smelt` 下）；`PluginManager` 在它下面按插件 id 分目录。
const PLUGIN_DATA_DIR: &str = "shell-plugin-data";

thread_local! {
    // GPUI 实体只活在 UI 线程，注册表也就只放这里。
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

#[derive(Default)]
struct Registry {
    runtime: Option<Rc<ShellRuntime>>,
    /// 持有已加载的插件。换掉它（Drop）就会卸载全部插件、取消它们的任务。
    manager: Option<PluginManager>,
    panels: Vec<ShellPanel>,
    /// 所有插件共享的宿主上下文；授权和 `smelt-workspace.context()` 都读它。
    context: Rc<RefCell<HostContext>>,
}

struct ShellPanel {
    id: String,
    title: String,
    /// 加载失败也占一个 tab，把错误画出来：静默消失的插件最难排查。
    content: Result<Entity<ScriptView>, String>,
}

/// Tool Panel 里 gpui-shell 插件 tab 的槽位。`ToolPanelTab` 必须是 `Copy`，所以
/// 运行时用下标，存档时换回插件 id（见 `key_for` / `slot_for_key`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShellTabSlot(pub(crate) u16);

/// 发现并挂载全部插件。在工作台创建时调用（不是 render）：这里会读盘、执行脚本。
///
/// 必须先于工作台存档读取：存档里的 tab 以插件 id 记录，注册表为空时会降级成 Files。
/// 工作台窗口同一时刻只有一个；重开窗口时旧的插件全部卸载、重新加载。
pub(crate) fn mount_all(window: &mut Window, cx: &mut App) {
    let runtime = match runtime(cx) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[shell-plugins] 运行时启动失败：{error:#}");
            return;
        }
    };
    let Some(data_home) = smelt_paths::smelt_home().map(|home| home.join(PLUGIN_DATA_DIR)) else {
        eprintln!("[shell-plugins] 无法定位 smelt 数据目录");
        return;
    };
    // 先卸载上一个窗口留下的插件，再建新的管理器。
    let context = REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.manager = None;
        registry.panels.clear();
        registry.context.clone()
    });

    let mut manager = PluginManager::new(plugin_dirs()).with_data_home(data_home);
    for result in manager.discover() {
        if let Err(error) = result {
            eprintln!("[shell-plugins] {error}");
        }
    }
    let available: Vec<(String, String)> = manager
        .available()
        .map(|manifest| (manifest.id().to_string(), manifest.name().to_string()))
        .collect();

    let mut panels = Vec::new();
    for (id, title) in available {
        let loaded = manager.load_with_policy(
            &runtime,
            &id,
            // 原型阶段只加载 smelt 自带和用户手动放进目录的插件，不弹授权确认。
            |_| true,
            |manifest, root, policy| lend_host_modules(manifest, root, policy, context.clone()),
            window,
            cx,
        );
        let content = match loaded {
            Ok(()) => manager
                .plugin(&id)
                .map(|plugin| plugin.view().clone())
                .context("插件已加载却查不到视图"),
            Err(error) => Err(error),
        }
        .map_err(|error| format!("{error:#}"));
        if let Err(error) = &content {
            eprintln!("[shell-plugins] {id} 加载失败：{error}");
        }
        panels.push(ShellPanel { id, title, content });
    }

    REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.manager = Some(manager);
        registry.panels = panels;
    });
}

/// 一个进程一个运行时；重开窗口时复用它。
fn runtime(cx: &mut App) -> anyhow::Result<Rc<ShellRuntime>> {
    if let Some(runtime) = REGISTRY.with(|registry| registry.borrow().runtime.clone()) {
        return Ok(runtime);
    }
    // 不调 `gpui_shell::init`：它只是 `gpui_base::init` 加样式表预热，前者
    // gpui-component 启动时已经做过（重复做会重复绑键），后者首次用到时自会建好。
    //
    // 编辑器文件（gpui-kit.d.ts 等）只在开发构建里写：发布版从 .app 里只读加载，
    // 写进已签名的包会破坏签名。
    gpui_shell::set_editor_files(cfg!(debug_assertions));
    let runtime = ShellRuntime::new(cx)?;
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
    dirs.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../shell-plugins"));
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

/// 把 smelt 的 HostModule 借给这一个插件：文件门控按它自己的 `smelt-grant.json`。
fn lend_host_modules(
    _manifest: &PluginManifest,
    root: &Path,
    policy: Policy,
    context: Rc<RefCell<HostContext>>,
) -> anyhow::Result<Policy> {
    let home = dirs::home_dir().context("无法定位用户主目录")?;
    let grant = fs::FsGrant::read(root).map_err(anyhow::Error::msg)?;
    let host = Rc::new(fs::HostFs::new(grant, home, context));
    let host_error = |error: gpui_shell::HostError| anyhow::anyhow!(error.message().to_string());
    policy
        .with_host_module(fs::fs_module(host.clone()))
        .map_err(host_error)?
        .with_host_module(fs::workspace_module(host))
        .map_err(host_error)
}

fn with_panels<R>(read: impl FnOnce(&[ShellPanel]) -> R) -> R {
    REGISTRY.with(|registry| read(&registry.borrow().panels))
}

pub(crate) fn slots() -> Vec<ShellTabSlot> {
    with_panels(|panels| {
        (0..panels.len())
            .filter_map(|index| u16::try_from(index).ok().map(ShellTabSlot))
            .collect()
    })
}

pub(crate) fn title(slot: ShellTabSlot) -> String {
    with_panels(|panels| {
        panels
            .get(usize::from(slot.0))
            .map(|panel| panel.title.clone())
            .unwrap_or_default()
    })
}

/// 存档里的稳定身份：插件 id。
pub(crate) fn key_for(slot: ShellTabSlot) -> Option<String> {
    with_panels(|panels| {
        panels
            .get(usize::from(slot.0))
            .map(|panel| panel.id.clone())
    })
}

pub(crate) fn slot_for_key(key: &str) -> Option<ShellTabSlot> {
    with_panels(|panels| {
        panels
            .iter()
            .position(|panel| panel.id == key)
            .and_then(|index| u16::try_from(index).ok())
            .map(ShellTabSlot)
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
pub(crate) fn render(slot: ShellTabSlot, cx: &App) -> AnyElement {
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
