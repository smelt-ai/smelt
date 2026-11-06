//! smelt 的插件：gpui-shell 脚本应用。
//!
//! 插件目录里放 gpui-shell 的 manifest `gpui-shell.json` 和入口脚本，界面由 GPUI 原生
//! 绘制，脚本跑在 UI 线程的 QuickJS 里。
//!
//! 入口默认导出的视图是插件的根：宿主挂载并持有它（持有着就是插件活着），但不显示。
//! 插件要显示的东西是面板：在 `smelt.json` 里声明，在根视图的 `init()` 里用
//! `DockArea.register_panel` 注册，布局按 `plugin:<id>/<面板>` 摆（见
//! `workspace_layout`）。没被点名摆出来的面板由布局里的 `plugin:*` 收进它所在的标签组。
//!
//! gpui-shell 不提供「列出某个插件注册了哪些面板」，`gpui-shell.json` 又不收别的字段，
//! 所以面板清单写在 smelt 自己的 `smelt.json` 里（和 VS Code 的 `contributes.views`
//! 加代码注册是一个路数）：宿主不跑脚本就知道有哪些面板，声明了没注册的显示原因。
//!
//! 每个插件挂载前把默认 Policy 换成它自己的一份（见 `plugin_policy`），视图带着它出生：
//! - 应用名是插件 id，面板名因此是 `shell:<插件 id>/<面板>`，两个插件都叫 `board` 也不会撞；
//! - smelt 的 HostModule（`smelt-fs` 见 `host_fs`，`smelt-workspace` 见 `host_workspace`）
//!   按插件各建一份，宿主因此知道是哪个插件在调，命令只发给声明它的插件；
//! - 权限大家一样：插件当作可信代码，和 smelt 自己一样不设限（见 `unrestricted`）。插件
//!   manifest 的 `capabilities` 里只认 `network.hosts`：gpui-shell 的网络授权只能逐个列
//!   主机名，各插件声明的主机合起来授给全部插件。
//!
//! 插件还可以在 `smelt.json` 里声明命令：命令面板里列出来，可以带默认快捷键（`keymap`
//! 装键），插件在根视图里循环 `await next_command()` 取。
//!
//! 没用上游的 `PluginManager`：它按 manifest 给每个插件新建一份空 Policy，装不上
//! smelt 的模块。这里只用公开的 `PluginManifest::read` 发现插件。
//!
//! 插件从哪来（前面的赢得同名 id，用户装的副本可以盖过自带的）：
//! 1. `~/.smelt/plugins/`：用户自己装的。设置里的「面板插件」页往这里导入、从这里删除
//!    （见 `manage`），改完调 `mount_all` 重新加载，不用重启。
//! 2. App 自带的 `Contents/Resources/plugins/`，只读加载。
//! 3. debug 构建直接读仓库里的 `plugins/`，改完脚本重新加载即可。
//!
//! 脚本可以从 `"gpui-component"` 导入带皮肤的控件。这些控件的 dialog、sheet、
//! notification 走组件库的 `Root`，和这扇窗口的根一致。`gpui-shell` 自己的
//! `window.open_dialog` 要找 `ShellRoot`（窗口第一层，或 `Root` 里面那一层），
//! 工作台没有包它，这条暂时用不了。

mod host_fs;
mod host_workspace;
mod log;
mod manage;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::Context as _;
use gpui::*;
use gpui_shell::plugin::{MANIFEST_FILE, PluginManifest};
use gpui_shell::policy::{self, Policy};
use gpui_shell::{Capabilities, ExecuteGrant, ScriptView, ShellRuntime};

use host_workspace::{CommandQueue, HostContext, Topic};
pub(crate) use host_workspace::{
    CurrentProject, MachineEntry, MachineInfo, ProjectInfo, SessionInfo,
};
use smelt_client::layout::{LayoutEntry, PluginPanelRef};

pub(crate) use log::install as install_log;
pub(crate) use manage::{import, uninstall, user_plugin_dir};

/// 用户安装插件的目录（`~/.smelt` 下），也是 App 包里、仓库里放插件的目录名。
const PLUGIN_DIR: &str = "plugins";

thread_local! {
    // GPUI 实体只活在 UI 线程，注册表也就只放这里。
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

/// 宿主处理插件发来的工作台请求（开会话、切项目……），由工作台启动时装上。
type WorkspaceHandler = Rc<dyn Fn(WorkspaceRequest, &mut App)>;

#[derive(Default)]
struct Registry {
    runtime: Option<Rc<ShellRuntime>>,
    /// 已挂载的插件，持有它们的根视图。清掉就卸载：视图拥有的任务随视图一起取消。
    plugins: Vec<MountedPlugin>,
    /// 每次 `mount_all` 加一。插件面板记着自己按哪一次建的，对不上就重建。
    generation: u64,
    /// 所有插件共享的宿主上下文，`smelt-workspace.context()` 读它。
    context: Rc<RefCell<HostContext>>,
    /// 宿主模块要用的主目录和文件接口，运行时第一次建好时定下，每个插件的模块都用这一份。
    host: Option<HostParts>,
    /// 插件命令的快捷键装上了哪些、哪些没装上，`keymap` 每次装键后写进来。
    keys: PluginKeys,
    /// `smelt-workspace` 里的操作（跳到会话、关会话、切项目）交给谁做。工作台创建时装上。
    workspace_handler: Option<WorkspaceHandler>,
}

impl Registry {
    fn plugin(&self, id: &str) -> Option<&MountedPlugin> {
        self.plugins.iter().find(|plugin| plugin.id == id)
    }
}

struct HostParts {
    home: PathBuf,
    fs: Rc<host_fs::HostFs>,
}

/// 插件命令的快捷键这一次装成什么样。
#[derive(Clone, Debug, Default)]
pub(crate) struct PluginKeys {
    /// 装上了的：(插件 id, 命令名, 按键)。
    pub(crate) bound: Vec<(String, String, String)>,
    /// 没装上的：(插件 id, 给人看的原因)。
    pub(crate) problems: Vec<(String, String)>,
}

struct MountedPlugin {
    id: String,
    name: String,
    version: String,
    root: PathBuf,
    source: PluginSource,
    /// 根视图。不显示；失败时它的面板都画这个错误：静默消失的插件最难排查。
    view: Result<Entity<ScriptView>, String>,
    /// `smelt.json` 声明的面板和命令。清单坏了插件照样挂载，只是什么也没声明。
    declared: Result<Declared, String>,
    /// 发给这个插件的命令，等它经 `next_command()` 取走。
    commands: Rc<RefCell<CommandQueue>>,
}

#[derive(Debug, PartialEq, Eq)]
struct DeclaredPanel {
    name: String,
    title: String,
}

/// 插件在 `smelt.json` 里声明的一条命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeclaredCommand {
    /// 插件收到的名字：`next_command()` 兑现的就是它。
    pub(crate) name: String,
    /// 命令面板里的名字。
    pub(crate) title: String,
    /// 默认快捷键，原样留着；能不能绑上由 `keymap` 装键时判断。
    pub(crate) key: Option<String>,
}

/// `smelt.json` 里声明的东西。
#[derive(Debug, Default, PartialEq, Eq)]
struct Declared {
    panels: Vec<DeclaredPanel>,
    commands: Vec<DeclaredCommand>,
}

/// `smelt.json`：smelt 要知道、gpui-shell 清单里放不下的东西：面板和命令。
const SMELT_MANIFEST_FILE: &str = "smelt.json";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SmeltManifest {
    #[serde(default)]
    panels: Vec<SmeltPanel>,
    #[serde(default)]
    commands: Vec<SmeltCommand>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SmeltCommand {
    name: String,
    title: String,
    #[serde(default)]
    key: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SmeltPanel {
    /// 和 `DockArea.register_panel(name, …)` 的 `name` 一致，布局里写 `plugin:<id>/<name>`。
    name: String,
    /// 标签上的名字。布局里的 `title` 可以覆盖它。
    title: String,
}

/// 读插件的 `smelt.json`。没有这个文件就是什么也没声明。
fn read_declared(root: &std::path::Path) -> Result<Declared, String> {
    let path = root.join(SMELT_MANIFEST_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Declared::default());
        }
        Err(error) => return Err(format!("读不了 {SMELT_MANIFEST_FILE}：{error}")),
    };
    let manifest: SmeltManifest = serde_json::from_str(&text)
        .map_err(|error| format!("{SMELT_MANIFEST_FILE} 不对：{error}"))?;
    let mut declared = Declared::default();
    for SmeltPanel { name, title } in manifest.panels {
        check_name("面板", &name, &title)?;
        if declared.panels.iter().any(|seen| seen.name == name) {
            return Err(format!("{SMELT_MANIFEST_FILE}：面板 `{name}` 写了两次"));
        }
        declared.panels.push(DeclaredPanel { name, title });
    }
    for SmeltCommand { name, title, key } in manifest.commands {
        check_name("命令", &name, &title)?;
        if declared.commands.iter().any(|seen| seen.name == name) {
            return Err(format!("{SMELT_MANIFEST_FILE}：命令 `{name}` 写了两次"));
        }
        declared.commands.push(DeclaredCommand { name, title, key });
    }
    Ok(declared)
}

/// 面板名、命令名不能为空、不能带 `/`（`/` 前面是插件 id），标题不能为空。
fn check_name(what: &str, name: &str, title: &str) -> Result<(), String> {
    if name.is_empty() || name.contains('/') {
        return Err(format!(
            "{SMELT_MANIFEST_FILE}：{what}名 `{name}` 不能为空、不能带 `/`"
        ));
    }
    if title.trim().is_empty() {
        return Err(format!(
            "{SMELT_MANIFEST_FILE}：{what} `{name}` 的 `title` 不能为空"
        ));
    }
    Ok(())
}

/// 插件从哪个目录加载的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PluginSource {
    /// `~/.smelt/plugins/`，用户自己装的，可以删。
    User,
    /// App 自带的，跟着 smelt 更新。
    Bundled,
    /// 开发构建直接读仓库里的 `plugins/`。release 构建不从仓库加载，没有这一种。
    #[cfg(debug_assertions)]
    Repo,
}

impl PluginSource {
    /// 加载时要不要往插件目录里写编辑器文件（`gpui-kit.d.ts`、`jsconfig.json` 和依赖
    /// 链接），让人改脚本时有补全。App 自带的目录不写：那是已签名的 .app 里面，写进去
    /// 会破坏签名，也没人在那里改脚本。
    fn editor_files(self) -> bool {
        !matches!(self, Self::Bundled)
    }
}

/// 设置页列出的一个已加载插件。
#[derive(Clone, Debug)]
pub(crate) struct InstalledPlugin {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) root: PathBuf,
    pub(crate) source: PluginSource,
    /// 加载失败的原因。
    pub(crate) error: Option<String>,
    /// 加载成功但有要说一声的：比如命令的快捷键没装上。
    pub(crate) notes: Vec<String>,
}

/// 发现并挂载全部插件。读盘、执行脚本，只在事件里调用，不在 render 里。
///
/// 工作台创建时调一次，必须先于布局搭建：布局里的插件面板要等插件注册过才建得出来，
/// `plugin:*` 也要知道有哪些面板。之后设置页
/// 导入、删除、重新加载插件时再调：旧的插件全部卸载、重新加载，调用方随后让布局重建
/// 插件面板（`Workspace::rebuild_plugin_panels`）。工作台窗口同一时刻只有一个，`window` 就是它。
pub(crate) fn mount_all(window: &mut Window, cx: &mut App) {
    let runtime = match runtime(cx) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[plugins] 运行时启动失败：{error:#}");
            return;
        }
    };
    // 先卸载已经挂着的插件：上一个窗口留下的，或者这次要重新加载的。
    unmount_all();

    let found = discover();
    // 网络只能逐个列主机，各插件声明的合起来授给全部插件。
    let hosts = found
        .iter()
        .flat_map(|plugin| declared_hosts(&plugin.root))
        .collect::<Vec<_>>();
    let capabilities = unrestricted().network_hosts(hosts);

    let plugins: Vec<MountedPlugin> = found
        .into_iter()
        .map(
            |Found {
                 root,
                 manifest,
                 source,
             }| {
                let id = manifest.id().to_string();
                let declared = read_declared(&root);
                if let Err(error) = &declared {
                    eprintln!("[plugins] {id}：{error}");
                }
                let commands = Rc::new(RefCell::new(CommandQueue::default()));
                // 开关挂在运行时上、加载时读，所以每个插件加载前按它的来源设一次。
                runtime.set_editor_files(source.editor_files());
                // 视图带着默认 Policy 出生：每个插件挂载前换成它自己的一份。
                let view = plugin_policy(&id, &capabilities, &commands).and_then(|policy| {
                    policy::set_default(policy);
                    runtime
                        .load_application(&root, manifest.entry())
                        .and_then(|application| runtime.mount_application(&application, window, cx))
                        .map_err(|error| format!("{error:#}"))
                });
                if let Err(error) = &view {
                    eprintln!("[plugins] {id} 加载失败：{error}");
                }
                MountedPlugin {
                    name: manifest.name().to_string(),
                    version: manifest.version().to_string(),
                    id,
                    root,
                    source,
                    view,
                    declared,
                    commands,
                }
            },
        )
        .collect();
    // 插件以外再建的视图不冒用最后一个插件的身份和模块。
    policy::set_default(Policy::new());

    REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.plugins = plugins;
        registry.generation += 1;
    });
    // 插件声明的命令快捷键跟着这一批插件重装。
    crate::keymap::install(cx);
}

/// 一个插件自己的 Policy：权限大家一样，应用名和宿主模块各是各的。
///
/// 模块按插件各建一份，`smelt-workspace` 才知道是谁在调：`next_command()` 只取发给
/// 这个插件的命令。gpui-shell 的 Policy 本来就是一个应用一份；面板类注册时记下注册它的
/// Policy，之后再建的面板照样是这个插件的。
fn plugin_policy(
    id: &str,
    capabilities: &Capabilities,
    commands: &Rc<RefCell<CommandQueue>>,
) -> Result<Policy, String> {
    let (home, fs, context) = REGISTRY.with(|registry| {
        let registry = registry.borrow();
        let host = registry
            .host
            .as_ref()
            .expect("运行时建好时就备好了宿主模块的材料");
        (host.home.clone(), host.fs.clone(), registry.context.clone())
    });
    Policy::new()
        .with_application(id)
        .with_capabilities(capabilities.clone())
        .with_host_module(host_fs::fs_module(fs))
        .and_then(|policy| {
            policy.with_host_module(host_workspace::workspace_module(
                home,
                context,
                commands.clone(),
            ))
        })
        .map_err(|error| error.message().to_string())
}

/// 卸载全部插件。视图拥有的任务随视图一起取消。
fn unmount_all() {
    let plugins = REGISTRY.with(|registry| std::mem::take(&mut registry.borrow_mut().plugins));
    drop(plugins);
}

/// 应用退出时释放插件视图、宿主模块和运行时。
///
/// 它们都挂在线程局部的注册表上。留到线程局部变量析构时再释放，gpui-shell 的
/// 任务表（也是线程局部，且建得比注册表晚、析构得比它早）已经没了，释放时
/// 取消任务会 panic，析构里的 panic 直接中止进程。
pub(crate) fn shutdown() {
    unmount_all();
    let (runtime, handler) = REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        (registry.runtime.take(), registry.workspace_handler.take())
    });
    drop((runtime, handler));
}

struct Found {
    root: PathBuf,
    manifest: PluginManifest,
    source: PluginSource,
}

/// 按目录顺序找插件，同名 id 先到先得。坏的 manifest 记一笔就跳过，不执行任何脚本。
fn discover() -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    for (directory, source) in plugin_dirs() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
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
                    if !found.iter().any(|seen| seen.manifest.id() == manifest.id()) {
                        found.push(Found {
                            root,
                            manifest,
                            source,
                        });
                    }
                }
                Err(error) => eprintln!("[plugins] {error}"),
            }
        }
    }
    found
}

/// 一个进程一个运行时；重开窗口时复用它。第一次创建时备好宿主模块要用的主目录和文件接口，
/// 模块本身在挂载每个插件时按插件建（见 `plugin_policy`）。
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

    let home = dirs::home_dir().context("无法定位用户主目录")?;
    let home = std::fs::canonicalize(&home).unwrap_or(home);
    REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.host = Some(HostParts {
            home,
            fs: Rc::new(host_fs::HostFs),
        });
        registry.runtime = Some(runtime.clone());
    });
    Ok(runtime)
}

/// 插件是可信代码，和 smelt 自己一样不设限：任意文件、任意命令、剪贴板、全部环境变量。
///
/// 网络另算：gpui-shell 只认逐个列出的主机名，没有「任意主机」，由调用方补上各插件
/// 声明的主机（见 `declared_hosts`）。`exit` 不给：
/// 那是请宿主退出，插件不该能关掉 smelt。`localStorage` 不给：要给得按插件定存储文件
/// 放在哪，还没做；插件要存东西就自己写文件。
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

/// 插件清单里 `capabilities.network.hosts` 声明的主机。只读这一项，其余权限一律放开，
/// 不看清单。清单坏了发现阶段就已跳过，这里读不出来就当没声明。
fn declared_hosts(root: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(root.join(MANIFEST_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|manifest| {
            manifest
                .pointer("/capabilities/network/hosts")?
                .as_array()
                .map(|hosts| {
                    hosts
                        .iter()
                        .filter_map(|host| host.as_str().map(str::to_owned))
                        .collect()
                })
        })
        .unwrap_or_default()
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

fn plugin_dirs() -> Vec<(PathBuf, PluginSource)> {
    let mut dirs = Vec::new();
    if let Some(user) = user_plugin_dir() {
        dirs.push((user, PluginSource::User));
    }
    if let Some(bundled) = bundled_plugin_dir() {
        dirs.push((bundled, PluginSource::Bundled));
    }
    // 开发期直接加载仓库里的插件，改完脚本重新加载即可，不必先装。
    #[cfg(debug_assertions)]
    dirs.push((repo_plugin_dir(), PluginSource::Repo));
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

fn with_registry<R>(read: impl FnOnce(&Registry) -> R) -> R {
    REGISTRY.with(|registry| read(&registry.borrow()))
}

/// 第几次 `mount_all`。插件面板据此判断要不要重建。
pub(crate) fn generation() -> u64 {
    with_registry(|registry| registry.generation)
}

/// 已挂载插件声明的面板，按插件加载顺序、再按 `smelt.json` 里的先后。
/// 布局里 `plugin:*` 收的就是其中没被点名摆出来的那些。
pub(crate) fn declared() -> Vec<PluginPanelRef> {
    with_registry(|registry| {
        registry
            .plugins
            .iter()
            .filter_map(|plugin| {
                let declared = plugin.declared.as_ref().ok()?;
                Some(
                    declared
                        .panels
                        .iter()
                        .map(|panel| PluginPanelRef::new(plugin.id.clone(), panel.name.clone())),
                )
            })
            .flatten()
            .collect()
    })
}

/// 已挂载插件带的布局（插件目录下的 `layouts/`），按插件加载顺序。读盘，只在事件里调。
pub(crate) fn layouts() -> Vec<LayoutEntry> {
    let roots: Vec<(String, PathBuf)> = with_registry(|registry| {
        registry
            .plugins
            .iter()
            .map(|plugin| (plugin.id.clone(), plugin.root.clone()))
            .collect()
    });
    roots
        .iter()
        .flat_map(|(id, root)| smelt_client::layout::discover_plugin(id, root))
        .collect()
}

/// 一个插件面板眼下能不能建，能建的话注册名和标题是什么。看的是挂着哪些插件、它们声明了
/// 什么，不看 Dock 的注册表：删掉的插件注册过的类还留在运行时里，照样建得出来。
pub(crate) struct PanelTarget {
    /// gpui-shell 注册时用的名字：`shell:<插件 id>/<面板>`。
    pub(crate) registered_name: &'static str,
    pub(crate) title: String,
}

pub(crate) fn panel_target(plugin: &str, panel: &str) -> Result<PanelTarget, String> {
    with_registry(|registry| {
        let Some(mounted) = registry.plugin(plugin) else {
            return Err(format!(
                "没有装插件 `{plugin}`。去「设置 → 插件」装上，或者换一套布局。"
            ));
        };
        if let Err(error) = &mounted.view {
            return Err(format!("插件「{}」加载失败：{error}", mounted.name));
        }
        let declared = match &mounted.declared {
            Ok(declared) => declared
                .panels
                .iter()
                .find(|declared| declared.name == panel),
            Err(error) => return Err(format!("插件「{}」的 {error}", mounted.name)),
        };
        let Some(declared) = declared else {
            return Err(format!(
                "插件「{}」没有声明面板 `{panel}`（见它的 {SMELT_MANIFEST_FILE}）。",
                mounted.name
            ));
        };
        Ok(PanelTarget {
            registered_name: gpui_shell::dock::panel_name(plugin, panel),
            title: declared.title.clone(),
        })
    })
}

/// 已挂载的插件，按加载顺序。只读内存。
pub(crate) fn installed() -> Vec<InstalledPlugin> {
    with_registry(|registry| {
        registry
            .plugins
            .iter()
            .map(|plugin| InstalledPlugin {
                id: plugin.id.clone(),
                name: plugin.name.clone(),
                version: plugin.version.clone(),
                root: plugin.root.clone(),
                source: plugin.source,
                error: plugin
                    .view
                    .as_ref()
                    .err()
                    .or(plugin.declared.as_ref().err())
                    .cloned(),
                notes: registry
                    .keys
                    .problems
                    .iter()
                    .filter(|(id, _)| *id == plugin.id)
                    .map(|(_, problem)| problem.clone())
                    .collect(),
            })
            .collect()
    })
}

/// 一条插件命令，连同它属于谁。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginCommand {
    pub(crate) plugin: String,
    pub(crate) plugin_name: String,
    pub(crate) command: DeclaredCommand,
}

/// 加载成功的插件声明的命令，按插件加载顺序、再按 `smelt.json` 里的先后。只读内存。
/// 加载失败的插件不列：没人会来取它的命令。
pub(crate) fn commands() -> Vec<PluginCommand> {
    with_registry(|registry| {
        registry
            .plugins
            .iter()
            .filter(|plugin| plugin.view.is_ok())
            .filter_map(|plugin| Some((plugin, plugin.declared.as_ref().ok()?)))
            .flat_map(|(plugin, declared)| {
                declared.commands.iter().map(|command| PluginCommand {
                    plugin: plugin.id.clone(),
                    plugin_name: plugin.name.clone(),
                    command: command.clone(),
                })
            })
            .collect()
    })
}

/// 把命令交给插件：排进它的队列，正在 `next_command()` 里等的立刻醒。插件已经不在了、
/// 没声明这条命令，返回 false。
pub(crate) fn invoke(plugin: &str, command: &str) -> bool {
    let queue = with_registry(|registry| {
        let mounted = registry.plugin(plugin)?;
        mounted.view.as_ref().ok()?;
        let declared = mounted.declared.as_ref().ok()?;
        declared
            .commands
            .iter()
            .any(|declared| declared.name == command)
            .then(|| mounted.commands.clone())
    });
    match queue {
        Some(queue) => {
            queue.borrow_mut().push(command.to_owned());
            true
        }
        None => false,
    }
}

/// `keymap` 每次重装键（改键、插件重新加载）以后告诉这里结果：设置页列出没装上的，
/// `commands()` 给出装上的键。smelt 自己的键也可能刚改过，所以每次都叫醒等 `commands` 的插件。
pub(crate) fn set_keys(keys: PluginKeys) {
    REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.keys = keys;
        let mut context = registry.context.borrow_mut();
        context.commands_generation += 1;
        context.wake(Topic::Commands);
    });
}

/// 插件命令眼下绑的键。
pub(crate) fn bound_key(plugin: &str, command: &str) -> Option<String> {
    with_registry(|registry| {
        registry
            .keys
            .bound
            .iter()
            .find(|(id, name, _)| id == plugin && name == command)
            .map(|(_, _, key)| key.clone())
    })
}

/// 把工作台当前项目同步给插件。只比较、写内存；变了才加 generation、叫醒等着的插件。
pub(crate) fn set_project(project: Option<CurrentProject>) {
    REGISTRY.with(|registry| {
        let registry = registry.borrow();
        let mut context = registry.context.borrow_mut();
        if context.project != project {
            context.project = project;
            context.generation += 1;
            context.wake(Topic::Context);
        }
    });
}

/// 把工作台的机器和项目同步给插件。只比较、写内存；内容没变时 generation 不动。
pub(crate) fn set_machines(machines: Vec<MachineEntry>) {
    REGISTRY.with(|registry| {
        let registry = registry.borrow();
        let mut context = registry.context.borrow_mut();
        if context.machines != machines {
            context.machines = machines;
            context.machines_generation += 1;
            context.wake(Topic::Machines);
        }
    });
}

/// 把工作台侧栏的会话同步给插件。只比较、写内存；内容没变时 generation 不动。
pub(crate) fn set_sessions(sessions: Vec<SessionInfo>) {
    REGISTRY.with(|registry| {
        let registry = registry.borrow();
        let mut context = registry.context.borrow_mut();
        if context.sessions != sessions {
            context.sessions = sessions;
            context.sessions_generation += 1;
            context.wake(Topic::Sessions);
        }
    });
}

/// 插件经 `smelt-workspace` 请工作台做的事。id、项目都是插件从快照里拿到的，可能已经
/// 不在了，由工作台判断。
#[derive(Debug)]
pub(crate) enum WorkspaceRequest {
    OpenSession(String),
    CloseSession(String),
    SelectProject {
        machine: String,
        root: String,
    },
    /// key 是 `new_session_options` 给出的选项；root 为空时不带目录。
    NewSession {
        machine: String,
        root: String,
        key: String,
    },
    /// 打开设置窗口。None 和 ⌘, 一样：开着就只提到前面，新开时停在要处理的那一页。
    OpenSettings(Option<crate::settings::SettingsSection>),
    /// 执行 smelt 自己的一条命令（`keymap::SHORTCUTS` 的 id），和按它的快捷键一样。
    Run(&'static str),
}

/// 插件的操作怎么落到工作台。要用工作台窗口和 `Workspace`，插件模块够不着，由工作台
/// 创建时装上。
pub(crate) fn set_workspace_handler(handler: impl Fn(WorkspaceRequest, &mut App) + 'static) {
    REGISTRY.with(|registry| registry.borrow_mut().workspace_handler = Some(Rc::new(handler)));
}

fn request(request: WorkspaceRequest, cx: &mut App) {
    // 先取出来再调：处理时可能重入注册表。
    let handler = REGISTRY.with(|registry| registry.borrow().workspace_handler.clone());
    if let Some(handler) = handler {
        handler(request, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DeclaredCommand, DeclaredPanel, MANIFEST_FILE, PluginManifest, declared_hosts,
        read_declared, repo_plugin_dir,
    };

    #[test]
    fn declared_hosts_come_from_the_manifest() {
        let dir = std::env::temp_dir().join(format!("smelt-plugin-hosts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(MANIFEST_FILE),
            r#"{"id":"com.example.x","name":"X","entry":"main.js",
                "capabilities":{"network":{"hosts":["api.github.com","example.com"]}}}"#,
        )
        .unwrap();
        assert_eq!(declared_hosts(&dir), ["api.github.com", "example.com"]);

        std::fs::write(
            dir.join(MANIFEST_FILE),
            r#"{"id":"com.example.x","name":"X","entry":"main.js"}"#,
        )
        .unwrap();
        assert!(declared_hosts(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

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
            read_declared(&root).unwrap_or_else(|error| panic!("{}：{error}", root.display()));
            assert!(
                ids.insert(manifest.id().to_string()),
                "插件 id 重复：{}",
                manifest.id()
            );
        }
        assert!(!ids.is_empty(), "{} 下没有插件", dir.display());
    }

    #[test]
    fn panels_and_commands_come_from_smelt_json() {
        let dir = std::env::temp_dir().join(format!("smelt-plugin-panels-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(super::SMELT_MANIFEST_FILE);
        assert_eq!(
            read_declared(&dir),
            Ok(Default::default()),
            "没有 smelt.json 就是什么也没声明"
        );

        std::fs::write(
            &file,
            r#"{"panels":[{"name":"board","title":"看板"},{"name":"list","title":"列表"}],
                "commands":[{"name":"jump","title":"跳过去","key":"cmd-shift-j"},
                            {"name":"refresh","title":"刷新"}]}"#,
        )
        .unwrap();
        let declared = read_declared(&dir).unwrap();
        assert_eq!(
            declared.panels,
            [
                DeclaredPanel {
                    name: "board".to_string(),
                    title: "看板".to_string()
                },
                DeclaredPanel {
                    name: "list".to_string(),
                    title: "列表".to_string()
                }
            ]
        );
        assert_eq!(
            declared.commands,
            [
                DeclaredCommand {
                    name: "jump".to_string(),
                    title: "跳过去".to_string(),
                    key: Some("cmd-shift-j".to_string())
                },
                DeclaredCommand {
                    name: "refresh".to_string(),
                    title: "刷新".to_string(),
                    key: None
                }
            ]
        );

        for (text, needle) in [
            (r#"{"panels":[{"name":"a/b","title":"x"}]}"#, "不能带 `/`"),
            (
                r#"{"panels":[{"name":"a","title":" "}]}"#,
                "`title` 不能为空",
            ),
            (
                r#"{"panels":[{"name":"a","title":"x"},{"name":"a","title":"y"}]}"#,
                "写了两次",
            ),
            (r#"{"panels":[{"name":"a","title":"x","size":3}]}"#, "size"),
            (r#"{"views":[]}"#, "views"),
            (
                r#"{"commands":[{"name":"","title":"x"}]}"#,
                "命令名 `` 不能为空",
            ),
            (
                r#"{"commands":[{"name":"a","title":"x"},{"name":"a","title":"y"}]}"#,
                "命令 `a` 写了两次",
            ),
            (
                r#"{"commands":[{"name":"a","title":"x","when":"y"}]}"#,
                "when",
            ),
        ] {
            std::fs::write(&file, text).unwrap();
            let error = read_declared(&dir).unwrap_err();
            assert!(error.contains(needle), "「{error}」里没有「{needle}」");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
