//! smelt 的插件：gpui-shell 脚本应用。
//!
//! 插件目录里放 gpui-shell 的 manifest `gpui-shell.json` 和入口脚本，界面由 GPUI 原生
//! 绘制，脚本跑在 UI 线程的 QuickJS 里。一个插件一份脚本实例：入口的默认导出是插件的根，
//! 它在 `init` 里登记视图、命令等，画在零大小的一层里，只用来浮出不贴着哪个视图的对话框、
//! 弹层（命令打开的）；视图按它交出的类在同一份实例里建，id 是
//! `<插件 id>/<名字>`，由布局摆放（见 `smelt_plugin_api::js`、`smelt_plugin_api::view`、
//! `workspace_layout`）。
//!
//! 插件当作可信代码，权限全部一样，和 smelt 自己一样不设限（见 `unrestricted`）。
//! 每个插件一份自己的 Policy（应用名就是插件 id），里面装 smelt 的宿主模块：
//! `smelt-plugin-api`（见 `smelt_plugin_api::js`，每个插件一份，模块由此知道是谁在调）和
//! `smelt-fs`（见 `host_fs`）。gpui-shell 按
//! 调用所在视图的 Policy 分派宿主函数，所以各插件拿到的是自己那份。挂载时把它设成默认
//! Policy，视图带着它出生（见 `mount_all`）。插件是可信代码，权限不按 manifest 给，
//! 一律见 [`unrestricted`]。
//!
//! 没用上游的 `PluginManager`：它按 manifest 给每个插件新建一份空 Policy，拿不到
//! 宿主注册的模块。这里只用公开的 `PluginManifest::read` 发现插件。
//!
//! 插件从两处来（见 [`Source`]）：
//! 1. App 自带的：仓库根 `plugins/`，打包时放进安装包，只读（见 `builtin`）。
//! 2. `~/.smelt/plugins/`：用户自己装的，也是写插件的地方。设置里的「面板插件」页往这里导入、
//!    从这里删除（见 `manage`）。
//!
//! 两处 id 不能重：同 id 不是替换，用户插件和自带的重了就不加载、报重复。想改自带插件，复制一份
//! 换个 id。目录里文件一变就重挂改动的那个插件（见 `watch`），不用重启，也不用点「重新加载」。
//!
//! 命令行 `smelt plugin check|status`（见 `cli`）：不开 App 加载并渲染一次插件；查 App 里
//! 各插件的加载结果（App 每次挂载后写一份快照，见 `status`）。
//!
//! 脚本从 `"smelt-ui"` 导入 smelt 自己的控件（`smelt_plugin_api::js::ui`），确认框走 smelt-ui
//! 登记在窗口根（`gpui_base::Root`）上的对话框层（`smelt_ui::init`）。`gpui-shell` 自己的
//! `window.open_dialog` 要找 `ShellRoot`（窗口第一层，或 `Root` 里面那一层），
//! 工作台没有包它，这条用不了。

mod assets;
mod builtin;
mod check;
mod cli;
mod host_fs;
mod log;
mod manage;
mod status;
mod themes;
mod watch;

use rust_i18n::t;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::Context as _;
use gpui::*;
use gpui_shell::plugin::{MANIFEST_FILE, PluginManifest};
use gpui_shell::policy::{self, Policy};
use gpui_shell::{Capabilities, ExecuteGrant, ScriptView, ShellRuntime};
use smelt_plugin_api as api;
use smelt_plugin_api::js::JsPlugin;

pub(crate) use assets::Assets;
pub(crate) use cli::maybe_run as maybe_run_cli;
pub(crate) use log::install as install_log;
pub(crate) use manage::{import, uninstall, user_plugin_dir};
pub(crate) use watch::watch;

/// 用户安装插件的目录（`~/.smelt` 下）。
const PLUGIN_DIR: &str = "plugins";

thread_local! {
    // GPUI 实体只活在 UI 线程，注册表也就只放这里。
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

#[derive(Default)]
struct Registry {
    runtime: Option<Rc<ShellRuntime>>,
    /// 已挂载的插件。视图本身在视图注册表里，撤下登记就卸载：视图拥有的任务随视图
    /// 一起取消。
    panels: Vec<PluginPanel>,
    /// 已登记主题文件的插件来源，重载时整批撤下再登记。
    theme_owners: Vec<String>,
}

struct PluginPanel {
    id: String,
    title: String,
    version: String,
    root: PathBuf,
    source: Source,
    /// 和先发现的插件同 id，没加载，也没登记任何东西：撤下时不能按 id 撤，那是别人的。
    duplicate: bool,
    /// 加载失败的原因。
    error: Option<String>,
    /// 挂上的插件根。拿着它插件的任务、事件队列才活着；放掉就是卸下。工作台把它画在一层
    /// 零大小的容器里（见 [`roots`]）：不占地方，它画的对话框、弹层经窗口根的层浮出来。
    /// 主题包、重复的、加载失败的没有。
    script: Option<Entity<ScriptView>>,
}

/// 插件从哪来。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// App 自带的，跟着 App 更新，不能删。
    Builtin,
    /// `~/.smelt/plugins/`，用户自己装的。
    User,
}

/// 设置页列出的一个已加载插件。
#[derive(Clone, Debug)]
pub(crate) struct InstalledPlugin {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) root: PathBuf,
    /// 加载失败的原因。
    pub(crate) error: Option<String>,
}

/// 发现并挂载全部插件，每个登记成一个视图。读盘、执行脚本，只在事件里调用，不在 render 里。
///
/// 工作台创建时调一次，先于装布局：布局按视图 id 摆，没登记的摆不上。之后设置页导入、
/// 删除、重新加载插件时再调：同 id 的登记换成新加载的，没了的插件撤下登记。视图登记
/// 一变，工作台自己重装布局（见 `Workspace::subscribe_host_events`）。工作台窗口同一时刻
/// 只有一个，`window` 就是它。
pub(crate) fn mount_all(window: &mut Window, cx: &mut App) {
    remount(None, window, cx);
}

/// 插件目录里有文件变了（见 `watch`）：只重挂 `changed` 这几个插件目录，加上新出现的、
/// 已经没了的。别的插件不动，视图里的状态留着。
pub(crate) fn reload_changed(changed: &BTreeSet<PathBuf>, window: &mut Window, cx: &mut App) {
    remount(Some(changed), window, cx);
}

/// `changed` 是 `None` 时全部重挂。
fn remount(changed: Option<&BTreeSet<PathBuf>>, window: &mut Window, cx: &mut App) {
    let found = discover();
    let mut theme_errors = register_theme_contributions(&found, cx);
    let previous = REGISTRY.with(|registry| std::mem::take(&mut registry.borrow_mut().panels));
    // 没变的插件原样留着：根目录和 id 都对得上、目录里没有文件变过，是不是重复也没变（别处
    // 新出现一个同 id 的，原来加载着的这份就成了重复，要撤下）。
    let mut kept = std::collections::HashMap::new();
    for panel in previous {
        let unchanged = changed.is_some_and(|changed| !changed.contains(&panel.root))
            && found.iter().any(|plugin| {
                plugin.root == panel.root
                    && plugin.manifest.id() == panel.id
                    && plugin.duplicate_of.is_some() == panel.duplicate
            });
        if unchanged {
            kept.insert(panel.root.clone(), panel);
        } else {
            // 脚本挂载时自己再登记布局、命令、主题；重载后不再登记的、卸掉的插件的不能留着。
            // 事件在这一轮更新结束才发，工作台看到的是挂完的结果，不会先换走再换回来。
            if !panel.duplicate {
                unmount(&panel.id, cx);
            }
        }
    }

    let mut runtime_slot = None;
    let panels: Vec<PluginPanel> = found
        .into_iter()
        .map(|plugin| {
            if let Some(panel) = kept.remove(&plugin.root) {
                return panel;
            }
            let theme_error = theme_errors.remove(plugin.manifest.id());
            mount_one(plugin, theme_error, &mut runtime_slot, window, cx)
        })
        .collect();

    status::publish(&panels);
    REGISTRY.with(|registry| registry.borrow_mut().panels = panels);
    // 插件根换了：工作台重画，画的是新的这批根。
    window.refresh();
    // 首次挂载时订阅可能还没装；后续重载也只在批次末通知一次。
    api::emit(api::HostEvent::ThemesChanged, cx);
    if let Some(appearance) = cx.try_global::<crate::settings::Appearance>() {
        let selected = appearance.theme_name.clone();
        crate::settings::apply_named_theme(&selected, cx);
        cx.refresh_windows();
    }
}

/// 挂一个插件：它的根在 `init` 里自己登记视图、命令等。加载失败的记下原因（设置的插件页、
/// `smelt plugin status` 列出来），登记了一半的撤掉。`runtime` 第一次有脚本插件要挂时才建。
fn mount_one(
    Found {
        root,
        manifest,
        source,
        duplicate_of,
    }: Found,
    theme_error: Option<String>,
    runtime: &mut Option<Result<Rc<ShellRuntime>, String>>,
    window: &mut Window,
    cx: &mut App,
) -> PluginPanel {
    let id = manifest.id().to_string();
    if let Some(other) = duplicate_of {
        let error = t!(
            "plugins.duplicate_skipped",
            id = id,
            other = other.display().to_string()
        );
        eprintln!("[plugins] {} 没加载：{error}", root.display());
        return PluginPanel {
            title: manifest.name().to_string(),
            version: manifest.version().to_string(),
            id,
            root,
            source,
            duplicate: true,
            error: Some(error.into_owned()),
            script: None,
        };
    }
    if is_theme_only(&manifest) {
        return PluginPanel {
            title: manifest.name().to_string(),
            version: manifest.version().to_string(),
            id,
            root,
            source,
            duplicate: false,
            error: theme_error,
            script: None,
        };
    }
    let runtime = runtime
        .get_or_insert_with(|| {
            self::runtime(cx).map_err(|error| {
                t!("plugins.runtime_failed", error = format!("{error:#}")).into_owned()
            })
        })
        .clone();
    // 先于加载：`init` 和第一次 `render` 就可能要画插件目录里的图。
    let assets = assets::mount(&id, root.clone());
    let plugin = JsPlugin::for_plugin(&id, manifest.name()).with_assets(assets, &root);
    let content = runtime.and_then(|runtime| {
        mount_root(&runtime, &id, &root, manifest.entry(), &plugin, window, cx)
            .map_err(|error| format!("{error:#}"))
    });
    if let Err(error) = &content {
        // 脚本可能登记了一半才出错，它给的主题、布局、命令、视图都不留。
        unmount(&id, cx);
        eprintln!("[plugins] {id} 加载失败：{error}");
    }
    PluginPanel {
        title: manifest.name().to_string(),
        version: manifest.version().to_string(),
        id,
        root,
        source,
        duplicate: false,
        error: theme_error.or_else(|| content.as_ref().err().cloned()),
        script: content.ok(),
    }
}

/// 加载、挂上插件的根（入口的默认导出）：它的 `init` 就是插件启动，登记视图、命令等。根画在
/// 零大小的一层里（见 [`roots`]），插件的视图按它登记的类在同一份脚本里建（见
/// `smelt_plugin_api::js`）。
fn mount_root(
    runtime: &Rc<ShellRuntime>,
    id: &str,
    root: &std::path::Path,
    entry: &str,
    plugin: &JsPlugin,
    window: &mut Window,
    cx: &mut App,
) -> anyhow::Result<Entity<ScriptView>> {
    let view = plugin_policy(id, plugin).and_then(|policy| {
        // 加载时解析导入、挂载时视图拿走的都是默认 Policy。
        policy::set_default(policy);
        runtime.set_editor_files(builtin::editor_files(root));
        let application = runtime.load_application(root, entry)?;
        runtime.mount_application(&application, window, cx)
    });
    // 默认 Policy 不留这个插件的：之后再有脚本在视图外跑，什么都不给。
    policy::set_default(Policy::new());
    let view = view?;
    plugin.attach(&view, cx);
    Ok(view)
}

/// 清单的入口是 JSON 的插件只带主题，不跑脚本。
fn is_theme_only(manifest: &PluginManifest) -> bool {
    std::path::Path::new(manifest.entry())
        .extension()
        .is_some_and(|extension| extension == "json")
}

/// 撤下插件挂载时登记的东西：布局、页、命令、文件打开者、菜单、话题、视图（主入口和脚本登记的）、
/// 脚本登记的主题。主题文件里的另算（见 `register_theme_contributions`）。
fn unmount(id: &str, cx: &mut App) {
    smelt_ui::theme_catalog::unregister_themes(&format!("script:{id}"), cx);
    api::unregister_layouts(id, cx);
    api::unregister_pages(id, cx);
    api::unregister_commands(id, cx);
    api::unregister_file_opener(id, cx);
    api::unregister_menu_items(id, cx);
    api::set_status_menu(id, None, cx);
    api::unregister_context_menu_items(id, cx);
    api::unregister_quit_guard(id, cx);
    api::unregister_topics(id, cx);
    api::unregister_views(id, cx);
    assets::unmount(id);
}

/// 一个插件自己的 Policy：权限大家一样，宿主模块里的 `smelt-plugin-api` 是它自己的。
fn plugin_policy(id: &str, plugin: &JsPlugin) -> anyhow::Result<Policy> {
    let host_error = |error: gpui_shell::HostError| anyhow::anyhow!(error.message().to_string());
    Policy::new()
        .with_application(id)
        .with_capabilities(unrestricted())
        .with_host_module(plugin.module())
        .and_then(|policy| policy.with_host_module(host_fs::fs_module(Rc::new(host_fs::HostFs))))
        .map_err(host_error)
}

/// 应用退出时释放插件视图和运行时。
///
/// 留到线程局部变量、全局状态析构时再释放，gpui-shell 的任务表（也是线程局部，且建得
/// 比注册表晚、析构得比它早）已经没了，释放时取消任务会 panic，析构里的 panic 直接
/// 中止进程。视图在视图注册表里，撤下登记就放掉；布局里摆着的那份随工作台一起放。
pub(crate) fn shutdown(cx: &mut App) {
    let panels = REGISTRY.with(|registry| std::mem::take(&mut registry.borrow_mut().panels));
    for panel in panels.iter().filter(|panel| !panel.duplicate) {
        // 视图和设置页的建页函数都要在运行时之前放掉，理由同上。
        api::unregister_views(&panel.id, cx);
    }
    // 插件的根也是，它拿着插件的任务。
    drop(panels);
    let runtime = REGISTRY.with(|registry| registry.borrow_mut().runtime.take());
    drop(runtime);
    status::retract();
}

struct Found {
    root: PathBuf,
    manifest: PluginManifest,
    source: Source,
    /// 同 id 的插件已经先发现了：它的目录。这一份不加载。
    duplicate_of: Option<PathBuf>,
}

/// 先于窗口和外观恢复，登记已安装插件的主题；纯主题包不执行脚本。
pub(crate) fn register_initial_themes(cx: &mut App) {
    let found = discover();
    for (id, error) in register_theme_contributions(&found, cx) {
        eprintln!("[plugins] {id} 主题加载失败：{error}");
    }
}

/// 按发现顺序登记各插件主题文件里的主题；重载从文件重建，避免已删或出错的主题残留。
fn register_theme_contributions(
    found: &[Found],
    cx: &mut App,
) -> std::collections::HashMap<String, String> {
    let previous =
        REGISTRY.with(|registry| std::mem::take(&mut registry.borrow_mut().theme_owners));
    for owner in previous {
        smelt_ui::theme_catalog::unregister_themes(&owner, cx);
    }
    let mut owners = Vec::new();
    let mut errors = std::collections::HashMap::new();
    for plugin in found.iter().filter(|plugin| plugin.duplicate_of.is_none()) {
        let id = plugin.manifest.id();
        // 脚本登记的主题（`script:<id>`）随插件挂载、卸载走（见 `unmount`），这里只管文件里的。
        let owner = format!("files:{id}");
        let result = themes::read(&plugin.root, plugin.manifest.entry()).and_then(|sources| {
            if sources.is_empty() {
                return Ok(false);
            }
            let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
            smelt_ui::theme_catalog::register_theme_sets(&owner, &sources, cx)
                .map(|_| true)
                .map_err(|error| error.to_string())
        });
        match result {
            Ok(true) => owners.push(owner),
            Ok(false) => {}
            Err(error) => {
                eprintln!("[plugins] {id} 主题加载失败：{error}");
                errors.insert(id.to_string(), error);
            }
        }
    }
    REGISTRY.with(|registry| registry.borrow_mut().theme_owners = owners);
    errors
}

/// 找插件：先自带的，再 `~/.smelt/plugins/` 下的。同 id 先到先得，后到的记成重复、不加载。
/// 坏的 manifest 记一笔就跳过，不执行任何脚本。
fn discover() -> Vec<Found> {
    let sources = [
        builtin::dir().map(|dir| (dir.to_path_buf(), Source::Builtin)),
        user_plugin_dir().map(|dir| (dir, Source::User)),
    ];
    let mut found: Vec<Found> = Vec::new();
    for (dir, source) in sources.into_iter().flatten() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut roots: Vec<PathBuf> = entries
            .flatten()
            // 点开头的是导入到一半的暂存目录（见 `manage`），不是插件。
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .map(|entry| entry.path())
            .filter(|path| path.join(MANIFEST_FILE).is_file())
            .collect();
        // 目录遍历顺序随文件系统而变；不排序的话 tab 顺序每次启动都可能不同。
        roots.sort();
        for root in roots {
            match PluginManifest::read(&root) {
                Ok(manifest) => {
                    let duplicate_of = found
                        .iter()
                        .find(|seen| {
                            seen.duplicate_of.is_none() && seen.manifest.id() == manifest.id()
                        })
                        .map(|seen| seen.root.clone());
                    found.push(Found {
                        root,
                        manifest,
                        source,
                        duplicate_of,
                    });
                }
                Err(error) => eprintln!("[plugins] {error}"),
            }
        }
    }
    found
}

/// 一个进程一个运行时；重开窗口时复用它。Policy 和宿主模块每个插件一份，挂载时装
/// （见 `mount_all`），不挂在运行时上。
fn runtime(cx: &mut App) -> anyhow::Result<Rc<ShellRuntime>> {
    if let Some(runtime) = REGISTRY.with(|registry| registry.borrow().runtime.clone()) {
        return Ok(runtime);
    }
    // 不调 `gpui_shell::init`：它会再跑一遍 `gpui_base::init`，而启动时已经调用过，重复做会
    // 重复绑键。样式表首次用到时自会建好；控件要的全局件由启动时的 `smelt_ui::init` 装好。
    let components = smelt_plugin_api::js::ui::components().context("构建脚本组件目录 smelt-ui")?;
    let runtime = ShellRuntime::new_with_components(cx, components)?;
    REGISTRY.with(|registry| registry.borrow_mut().runtime = Some(runtime.clone()));
    Ok(runtime)
}

/// 插件是可信代码，和 smelt 自己一样不设限：任意文件、任意命令、剪贴板、全部环境变量。
/// 这不是安全边界，装别人的插件前要读过它的代码。
///
/// 网络授权不给：gpui-shell 只认逐个列出的主机名，没有「任意主机」，按清单声明逐个授
/// 又名不副实（插件能跑 `curl`）。所以脚本里的 `fetch` 用不了，要联网经命令执行。`exit` 不给：
/// 那是请宿主退出，插件不该能关掉 smelt。`localStorage` 不给：那是 JSON 文件，业务数据
/// 要进 SQLite；插件要存东西就自己写文件。
fn unrestricted() -> Capabilities {
    let roots = filesystem_roots();
    Capabilities::new()
        .read_roots(roots.clone())
        .write_roots(roots)
        .execute(ExecuteGrant::Unrestricted)
        .clipboard_read(true)
        .clipboard_write(true)
        // 只能按名字授权，就把启动时有的全部列上。
        .env_variables(std::env::vars_os().filter_map(|(name, _)| name.into_string().ok()))
}

/// 文件系统的根：Unix 是 `/`，Windows 是每个存在的盘符。
fn filesystem_roots() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        ('A'..='Z')
            .map(|drive| PathBuf::from(format!("{drive}:\\")))
            .filter(|root| root.is_dir())
            .collect()
    }
    #[cfg(not(windows))]
    {
        vec![PathBuf::from("/")]
    }
}

/// 挂上的各插件根，工作台每帧画进一层零大小的容器（插件的对话框、弹层要有个画它的地方，
/// 命令打开的、不贴着哪个视图的也是）。只读内存。
pub(crate) fn roots() -> Vec<Entity<ScriptView>> {
    with_registry(|registry| {
        registry
            .panels
            .iter()
            .filter_map(|panel| panel.script.clone())
            .collect()
    })
}

fn with_registry<R>(read: impl FnOnce(&Registry) -> R) -> R {
    REGISTRY.with(|registry| read(&registry.borrow()))
}

/// 用户装的插件，按加载顺序。自带的是 App 的一部分，不在这里列。只读内存。
///
/// 加载失败之外，根上一次画失败也算出错：根画在看不见的一层里（见 [`roots`]），画坏了界面上
/// 看不出来。
pub(crate) fn installed(cx: &App) -> Vec<InstalledPlugin> {
    with_registry(|registry| {
        registry
            .panels
            .iter()
            .filter(|panel| panel.source == Source::User)
            .map(|panel| InstalledPlugin {
                id: panel.id.clone(),
                name: panel.title.clone(),
                version: panel.version.clone(),
                root: panel.root.clone(),
                error: panel.error.clone().or_else(|| {
                    let script = panel.script.as_ref()?.read(cx);
                    script.build_error().map(str::to_string)
                }),
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::{MANIFEST_FILE, PluginManifest};

    #[gpui::test]
    fn production_runtime_mounts_smelt_ui_buttons(cx: &mut gpui::TestAppContext) {
        use gpui::{Modifiers, VisualTestContext, point, px, size};
        use std::ops::Deref;

        let dir = std::env::temp_dir().join(format!("smelt-ui-buttons-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.js"), r#"
import { View, div } from "gpui-kit";
import { SmeltBadge, SmeltButton } from "smelt-ui";
function button(id, label, onClick, variant = "quiet") {
  return SmeltButton.new(id).label(label).small()[variant]().on_click((_event, cx) => onClick(cx));
}
export default class App extends View {
  init() { this.picking = true; this.hits = 0; }
  render() {
    const hit = cx => { this.hits++; cx.notify(); };
    return div().size_full()
      .child(button("ghost", "Ghost", hit).absolute().left(30).top(40).w(180))
      .child(button("secondary", "Secondary", hit, "secondary").absolute().left(30).top(80).w(180))
      .child(button("primary", "Primary", hit, "primary").loading(this.picking).absolute().left(30).top(120).w(180))
      .child(button("danger", "Danger", hit, "danger").absolute().left(30).top(160).w(180))
      .child(SmeltButton.new("toggle").label("Toggle").absolute().left(30).top(200)
        .on_click((_event, cx) => { this.picking = false; cx.notify(); }))
      .child(SmeltBadge.new().child("smelt badge"))
      .child(`hits: ${this.hits}`);
  }
}
"#).unwrap();
        // 与 app 启动一样先初始化一次；runtime 不再初始化。
        cx.update(|cx| {
            gpui_shell::init(cx);
            smelt_ui::init(cx);
        });
        let runtime = cx.update(super::runtime).unwrap();
        runtime.set_editor_files(false);
        // 和正式加载一样带上这个插件的 Policy：`smelt-plugin-api` 等宿主模块挂在它上面。
        let plugin = smelt_plugin_api::js::JsPlugin::for_plugin("smelt.buttons-test", "Buttons");
        super::policy::set_default(super::plugin_policy("smelt.buttons-test", &plugin).unwrap());
        let app = runtime.load_application(&dir, "main.js").unwrap();
        let mounted = std::rc::Rc::new(std::cell::RefCell::new(None));
        let slot = mounted.clone();
        let window = cx.open_window(size(px(500.), px(300.)), move |window, cx| {
            let view = runtime.mount_application(&app, window, cx).unwrap();
            *slot.borrow_mut() = Some(view.clone());
            gpui_base::Root::new(view, window, cx)
        });
        let mut context = VisualTestContext::from_window(*window.deref(), cx);
        let view = mounted.borrow().clone().unwrap();
        let draw = |context: &mut VisualTestContext| {
            context.run_until_parked();
            context.update(|window, cx| window.draw(cx).clear(cx));
        };
        draw(&mut context);
        context.update(|window, _| {
            let scale = window.scale_factor();
            let quads = window.painted_quads();
            let secondary: gpui::Background =
                smelt_ui::ui_theme::palette().button_secondary[0].into();
            // 主按钮开着 `loading`，加载时底换成 `soft_hover`。
            let loading: gpui::Background = gpui::rgb(smelt_ui::ui_theme::soft_hover()).into();
            for (top, height, background) in [
                (40., 24., None),
                (80., 24., Some(secondary)),
                (120., 24., Some(loading)),
                (160., 24., Some(secondary)),
            ] {
                let matching = quads.iter().filter(|quad| {
                    (quad.bounds.origin.x.0 - 30. * scale).abs() < 1.
                        && (quad.bounds.origin.y.0 - top * scale).abs() < 1.
                        && (quad.bounds.size.width.0 - 180. * scale).abs() < 1.
                        && (quad.bounds.size.height.0 - height * scale).abs() < 1.
                });
                if let Some(background) = background {
                    assert!(
                        matching
                            .into_iter()
                            .any(|quad| quad.background == background),
                        "top={top}, quads={quads:?}"
                    );
                } else {
                    assert!(
                        !matching.into_iter().any(|quad| quad
                            .background
                            .as_solid()
                            .is_some_and(|color| color.a > 0.)),
                        "top={top}, quads={quads:?}"
                    );
                }
            }
        });
        context.simulate_click(point(px(40.), px(132.)), Modifiers::default());
        draw(&mut context);
        let tree = context.update(|_, cx| view.read(cx).snapshot().unwrap().debug_tree());
        assert!(
            tree.contains("hits: 0") && tree.contains("smelt badge"),
            "{tree}"
        );
        for method in [
            "quiet",
            "secondary",
            "primary",
            "danger",
            "small",
            "loading",
        ] {
            assert!(tree.contains(&format!(":{method}(registered)")), "{tree}");
        }
        context.simulate_click(point(px(40.), px(214.)), Modifiers::default());
        draw(&mut context);
        context.simulate_click(point(px(40.), px(132.)), Modifiers::default());
        draw(&mut context);
        let tree = context.update(|_, cx| view.read(cx).snapshot().unwrap().debug_tree());
        assert!(tree.contains("hits: 1"), "{tree}");
        cx.update(super::shutdown);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 插件的视图都在插件那一份脚本里建：模块里的状态共用，参数各份各拿，再次打开由插件的
    /// 事件流按实例号交回。
    #[gpui::test]
    fn views_are_built_from_classes_in_the_plugins_one_script(cx: &mut gpui::TestAppContext) {
        use gpui::{VisualTestContext, px, size};
        use serde_json::json;
        use smelt_plugin_api::HostEvent;
        use std::ops::Deref;

        let dir = std::env::temp_dir().join(format!("smelt-plugin-views-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("main.js"),
            r#"
import { View, div } from "gpui-kit";
import { DockArea } from "gpui-base";
import { register_views, opened_view, next_event, emit_event } from "smelt-plugin-api";
let built = 0;
class Chat extends View {
  init() {
    const { instance, args } = opened_view();
    built += 1;
    emit_event("t/built", { instance, args, built });
  }
  render() { return div(); }
}
export default class Root extends View {
  init(_props, cx) {
    DockArea.register_panel("chat", Chat);
    register_views([{ name: "chat", title: "Chat", instances: true }]);
    cx.spawn(async () => {
      for (;;) {
        const event = await next_event();
        if (event.type === "view_opened") emit_event("t/reopened", event);
      }
    });
  }
  render() { return div(); }
}
"#,
        )
        .unwrap();
        cx.update(|cx| {
            gpui_shell::init(cx);
            gpui_base::init(cx);
            smelt_ui::init(cx);
            smelt_plugin_api::install(super::check::CheckHost, cx);
        });
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        cx.update(|cx| {
            let seen = seen.clone();
            cx.subscribe(&smelt_plugin_api::events(cx), move |_, event, _| {
                if let HostEvent::Plugin { name, payload } = event {
                    seen.borrow_mut().push((name.to_string(), payload.clone()));
                }
            })
            .detach();
        });
        let runtime = cx.update(super::runtime).unwrap();
        runtime.set_editor_files(false);
        let window = cx.open_window(size(px(400.), px(300.)), |_, _| gpui::Empty);
        let mut context = VisualTestContext::from_window(*window.deref(), cx);
        let plugin = smelt_plugin_api::js::JsPlugin::for_plugin("t", "T");
        let script = context.update(|window, cx| {
            super::mount_root(&runtime, "t", &dir, "main.js", &plugin, window, cx).unwrap()
        });
        let chat = context.update(|_, cx| {
            smelt_plugin_api::views(cx)
                .into_iter()
                .find(|view| view.id == "t/chat")
                .expect("根在 init 里登记了视图")
        });
        let (first, second) = context.update(|window, cx| {
            let first = (chat.build)(window, cx);
            first.open(&json!({ "a": 1 }), window, cx);
            let second = (chat.build)(window, cx);
            second.open(&json!({ "a": 2 }), window, cx);
            (first, second)
        });
        context.run_until_parked();
        context.update(|window, cx| first.open(&json!({ "a": 3 }), window, cx));
        context.run_until_parked();
        assert_eq!(
            *seen.borrow(),
            [
                (
                    "t/built".to_string(),
                    json!({ "instance": 1, "args": { "a": 1 }, "built": 1 })
                ),
                (
                    "t/built".to_string(),
                    json!({ "instance": 2, "args": { "a": 2 }, "built": 2 })
                ),
                (
                    "t/reopened".to_string(),
                    json!({ "type": "view_opened", "view": "chat", "instance": 1, "args": { "a": 3 } })
                ),
            ]
        );
        drop((first, second, script));
        cx.update(super::shutdown);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 根改了共用的状态，`notify_view()` 重画插件的视图。gpui-shell 的 `cx.notify()` 只重画
    /// 正在跑的那份脚本视图：根里的任务调它重画的是根，视图在自己任务里等根叫醒也一样。
    #[gpui::test]
    fn the_root_redraws_the_plugins_views(cx: &mut gpui::TestAppContext) {
        use gpui::{VisualTestContext, px, size};
        use serde_json::json;
        use smelt_plugin_api::HostEvent;
        use std::ops::Deref;

        let dir = std::env::temp_dir().join(format!("smelt-plugin-wake-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("main.js"),
            r#"
import { View, div } from "gpui-kit";
import { DockArea } from "gpui-base";
import { register_views, next_event, emit_event, notify_view } from "smelt-plugin-api";
let ticks = 0;
let rendered = -1;
class Counter extends View {
  render() { rendered = ticks; return div().child(`${ticks}`); }
}
export default class Root extends View {
  init(_props, cx) {
    DockArea.register_panel("counter", Counter);
    register_views([{ name: "counter", title: "Counter" }]);
    cx.spawn(async () => {
      for (;;) {
        const event = await next_event();
        if (event.type !== "plugin") continue;
        if (event.name === "t/tick") {
          ticks += 1;
          notify_view();
        } else if (event.name === "t/report") {
          emit_event("t/rendered", { rendered });
        }
      }
    });
  }
  render() { return div(); }
}
"#,
        )
        .unwrap();
        cx.update(|cx| {
            gpui_shell::init(cx);
            gpui_base::init(cx);
            smelt_ui::init(cx);
            smelt_plugin_api::install(super::check::CheckHost, cx);
        });
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        cx.update(|cx| {
            let seen = seen.clone();
            cx.subscribe(&smelt_plugin_api::events(cx), move |_, event, _| {
                if let HostEvent::Plugin { name, payload } = event
                    && name == "t/rendered"
                {
                    seen.borrow_mut().push(payload.clone());
                }
            })
            .detach();
        });
        let runtime = cx.update(super::runtime).unwrap();
        runtime.set_editor_files(false);
        let window = cx.open_window(size(px(400.), px(300.)), |_, _| gpui::Empty);
        let mut context = VisualTestContext::from_window(*window.deref(), cx);
        let plugin = smelt_plugin_api::js::JsPlugin::for_plugin("t", "T");
        let script = context.update(|window, cx| {
            super::mount_root(&runtime, "t", &dir, "main.js", &plugin, window, cx).unwrap()
        });
        let counter = context.update(|window, cx| {
            let info = smelt_plugin_api::views(cx)
                .into_iter()
                .find(|view| view.id == "t/counter")
                .unwrap();
            let view = (info.build)(window, cx).view();
            window.replace_root(cx, {
                let view = view.clone();
                move |_, _| super::check::Probe(view)
            });
            view
        });
        let draw = |context: &mut VisualTestContext| {
            context.run_until_parked();
            context.update(|window, cx| window.draw(cx).clear(cx));
        };
        let send = |name: &str, context: &mut VisualTestContext| {
            let event = HostEvent::Plugin {
                name: name.to_string().into(),
                payload: json!(null),
            };
            context.update(|_, cx| smelt_plugin_api::emit(event, cx));
        };
        draw(&mut context);
        send("t/tick", &mut context);
        draw(&mut context);
        send("t/report", &mut context);
        context.run_until_parked();
        assert_eq!(*seen.borrow(), [json!({ "rendered": 1 })]);
        context.update(|window, cx| window.replace_root(cx, |_, _| gpui::Empty));
        drop((counter, script, runtime));
        context.run_until_parked();
        cx.update(super::shutdown);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[gpui::test]
    fn theme_files_are_registered_reloaded_and_removed(cx: &mut gpui::TestAppContext) {
        let dir =
            std::env::temp_dir().join(format!("smelt-plugin-themes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("themes")).unwrap();
        std::fs::write(
            dir.join(MANIFEST_FILE),
            r#"{"id":"com.example.theme-test","name":"Theme Test","entry":"themes/colors.json"}"#,
        )
        .unwrap();
        let path = dir.join("themes/colors.json");
        std::fs::write(&path, r##"{"version":1,"name":"Sample","themes":[{"name":"Sample Dark","mode":"dark","colors":{"bg_base":"#123456","text_base":"#fafafa"}}]}"##).unwrap();
        let found = super::Found {
            root: dir.clone(),
            manifest: PluginManifest::read(&dir).unwrap(),
            source: super::Source::User,
            duplicate_of: None,
        };
        cx.update(|cx| {
            gpui_base::init(cx);
            smelt_ui::init(cx);
        });
        cx.update(|cx| {
            assert!(
                super::register_theme_contributions(std::slice::from_ref(&found), cx).is_empty()
            );
            assert!(smelt_ui::theme_catalog::contains("Sample Dark", cx));
            std::fs::write(
                &path,
                r##"{"version":1,"name":"Sample","themes":[{"name":"Sample Light","mode":"light","colors":{"bg_base":"#ffffff","text_base":"#1a1a1a"}}]}"##,
            )
            .unwrap();
            assert!(
                super::register_theme_contributions(std::slice::from_ref(&found), cx).is_empty()
            );
            assert!(!smelt_ui::theme_catalog::contains("Sample Dark", cx));
            assert!(smelt_ui::theme_catalog::contains("Sample Light", cx));
            std::fs::write(&path, "invalid json").unwrap();
            assert!(
                super::register_theme_contributions(&[found], cx)
                    .contains_key("com.example.theme-test")
            );
            assert!(!smelt_ui::theme_catalog::contains("Sample Light", cx));
            super::register_theme_contributions(&[], cx);
        });
        std::fs::remove_dir_all(dir).unwrap();
    }
}
