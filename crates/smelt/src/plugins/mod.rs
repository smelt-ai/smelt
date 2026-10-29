//! smelt 的插件：gpui-shell 脚本应用。
//!
//! 插件目录里放 gpui-shell 的 manifest `gpui-shell.json` 和入口脚本，界面由 GPUI 原生
//! 绘制，脚本跑在 UI 线程的 QuickJS 里。每个插件挂成 Tool Panel 的一个 tab。
//!
//! 插件当作可信代码，全部共用一份宿主定的权限，和 smelt 自己一样不设限（见
//! `unrestricted`），smelt 的通用 HostModule（`smelt-fs` 见 `host_fs`，
//! `smelt-workspace` 见 `host_workspace`）用 `export_module` 注册一次，所有插件都能导入。这就是官方给嵌入宿主的
//! 写法：宿主先装好默认 Policy 和模块，再 `load_application` / `mount_application`，
//! 视图带着默认 Policy 出生。插件 manifest 的 `capabilities` 里只认 `network.hosts`：
//! gpui-shell 的网络授权只能逐个列主机名，各插件声明的主机合起来授给全部插件。
//!
//! 没用上游的 `PluginManager`：它按 manifest 给每个插件新建一份空 Policy，拿不到
//! 宿主注册的模块。这里只用公开的 `PluginManifest::read` 发现插件。
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

use crate::ui_theme;
use anyhow::Context as _;
use gpui::*;
use gpui_component::ActiveTheme as _;
use gpui_shell::plugin::{MANIFEST_FILE, PluginManifest};
use gpui_shell::policy::{self, Policy};
use gpui_shell::{Capabilities, ExecuteGrant, ScriptView, ShellRuntime};

use host_workspace::HostContext;
pub(crate) use host_workspace::SessionInfo;

pub(crate) use log::install as install_log;
pub(crate) use manage::{import, uninstall, user_plugin_dir};

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
    /// 这个进程里见过的插件 id，下标就是 `PluginTabSlot`。只增不删：重新加载、增删
    /// 插件之后，同一个 id 还是同一个槽位，各会话、各项目记着的 tab 不用跟着改。
    ids: Vec<String>,
    /// 所有插件共享的宿主上下文，`smelt-workspace.context()` 读它。
    context: Rc<RefCell<HostContext>>,
    /// `smelt-workspace.open_session` 怎么跳到会话。工作台创建时装上。
    session_opener: Option<Rc<dyn Fn(String, &mut App)>>,
}

impl Registry {
    fn slot_of(&mut self, id: &str) -> Option<PluginTabSlot> {
        let index = match self.ids.iter().position(|seen| seen == id) {
            Some(index) => index,
            None => {
                self.ids.push(id.to_string());
                self.ids.len() - 1
            }
        };
        u16::try_from(index).ok().map(PluginTabSlot)
    }

    fn panel(&self, slot: PluginTabSlot) -> Option<&PluginPanel> {
        self.panels.iter().find(|panel| panel.slot == slot)
    }
}

struct PluginPanel {
    slot: PluginTabSlot,
    id: String,
    title: String,
    version: String,
    root: PathBuf,
    source: PluginSource,
    /// 加载失败也占一个 tab，把错误画出来：静默消失的插件最难排查。
    content: Result<Entity<ScriptView>, String>,
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
}

/// Tool Panel 里插件 tab 的槽位。`ToolPanelTab` 必须是 `Copy`，所以运行时用下标，
/// 存档时换回插件 id（见 `key_for` / `slot_for_key`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PluginTabSlot(pub(crate) u16);

/// 发现并挂载全部插件。读盘、执行脚本，只在事件里调用，不在 render 里。
///
/// 工作台创建时调一次，必须先于工作台存档读取：存档里的 tab 以插件 id 记录，注册表
/// 为空时会降级成 Files。之后设置页导入、删除、重新加载插件时再调：旧的插件全部卸载、
/// 重新加载。工作台窗口同一时刻只有一个，`window` 就是它。
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
    // 视图出生时定下权限，所以每次挂载前按这一批插件的声明重新装一遍。
    let hosts = found
        .iter()
        .flat_map(|plugin| declared_hosts(&plugin.root))
        .collect::<Vec<_>>();
    gpui_shell::set_capabilities(unrestricted().network_hosts(hosts));

    let panels: Vec<PluginPanel> = found
        .into_iter()
        .filter_map(
            |Found {
                 root,
                 manifest,
                 source,
             }| {
                let id = manifest.id().to_string();
                let Some(slot) = REGISTRY.with(|registry| registry.borrow_mut().slot_of(&id))
                else {
                    eprintln!("[plugins] 插件太多，{id} 没有加载");
                    return None;
                };
                // 开关挂在运行时上、加载时读，所以每个插件加载前按它的来源设一次。
                runtime.set_editor_files(source.editor_files());
                let content = runtime
                    .load_application(&root, manifest.entry())
                    .and_then(|application| runtime.mount_application(&application, window, cx))
                    .map_err(|error| format!("{error:#}"));
                if let Err(error) = &content {
                    eprintln!("[plugins] {id} 加载失败：{error}");
                }
                Some(PluginPanel {
                    slot,
                    title: manifest.name().to_string(),
                    version: manifest.version().to_string(),
                    id,
                    root,
                    source,
                    content,
                })
            },
        )
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
    let (runtime, opener) = REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        (registry.runtime.take(), registry.session_opener.take())
    });
    drop((runtime, opener));
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

    let home = dirs::home_dir().context("无法定位用户主目录")?;
    let home = std::fs::canonicalize(&home).unwrap_or(home);
    // 权限在 `mount_all` 里按插件声明装，这里只立一份空的默认 Policy。必须先于
    // `export_module`：模块挂在默认 Policy 上，后装的 `set_default` 会把它整个换掉。
    policy::set_default(Policy::new());
    let context = REGISTRY.with(|registry| registry.borrow().context.clone());
    let host_error = |error: gpui_shell::HostError| anyhow::anyhow!(error.message().to_string());
    let fs = Rc::new(host_fs::HostFs);
    gpui_shell::export_module(host_fs::fs_module(fs)).map_err(host_error)?;
    gpui_shell::export_module(host_workspace::workspace_module(home, context))
        .map_err(host_error)?;

    REGISTRY.with(|registry| registry.borrow_mut().runtime = Some(runtime.clone()));
    Ok(runtime)
}

/// 插件是可信代码，和 smelt 自己一样不设限：任意文件、任意命令、剪贴板、全部环境变量。
///
/// 网络另算：gpui-shell 只认逐个列出的主机名，没有「任意主机」，由调用方补上各插件
/// 声明的主机（见 `declared_hosts`）。`exit` 不给：
/// 那是请宿主退出，插件不该能关掉 smelt。`localStorage` 不给：所有插件共用这一份
/// Policy，存储也就是同一个文件，键会互相覆盖；插件要存东西就自己写文件。
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

/// 已挂载插件的槽位，按加载顺序。
pub(crate) fn slots() -> Vec<PluginTabSlot> {
    with_registry(|registry| registry.panels.iter().map(|panel| panel.slot).collect())
}

/// 槽位上的插件眼下挂着。插件被删掉以后，记着它的 tab 还在，只是这里为 false。
pub(crate) fn is_mounted(slot: PluginTabSlot) -> bool {
    with_registry(|registry| registry.panel(slot).is_some())
}

pub(crate) fn title(slot: PluginTabSlot) -> String {
    with_registry(|registry| {
        registry
            .panel(slot)
            .map(|panel| panel.title.clone())
            .unwrap_or_default()
    })
}

/// 存档里的稳定身份：插件 id。插件已经删掉也照样给出 id：下次启动解析不到，
/// 自然降级成 Files；要是又装回来，还能回到它。
pub(crate) fn key_for(slot: PluginTabSlot) -> Option<String> {
    with_registry(|registry| registry.ids.get(usize::from(slot.0)).cloned())
}

/// 只认眼下挂着的插件。
pub(crate) fn slot_for_key(key: &str) -> Option<PluginTabSlot> {
    with_registry(|registry| {
        registry
            .panels
            .iter()
            .find(|panel| panel.id == key)
            .map(|panel| panel.slot)
    })
}

/// 已挂载的插件，按加载顺序。只读内存。
pub(crate) fn installed() -> Vec<InstalledPlugin> {
    with_registry(|registry| {
        registry
            .panels
            .iter()
            .map(|panel| InstalledPlugin {
                id: panel.id.clone(),
                name: panel.title.clone(),
                version: panel.version.clone(),
                root: panel.root.clone(),
                source: panel.source,
                error: panel.content.as_ref().err().cloned(),
            })
            .collect()
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

/// 把工作台侧栏的会话同步给插件。只比较、写内存；内容没变时 generation 不动。
pub(crate) fn set_sessions(sessions: Vec<SessionInfo>) {
    REGISTRY.with(|registry| {
        let registry = registry.borrow();
        let mut context = registry.context.borrow_mut();
        if context.sessions != sessions {
            context.sessions = sessions;
            context.sessions_generation += 1;
        }
    });
}

/// 插件调 `open_session` 时怎么跳到会话。跳转要用工作台窗口和 `Workspace`，
/// 插件模块够不着，由工作台创建时装上。
pub(crate) fn set_session_opener(opener: impl Fn(String, &mut App) + 'static) {
    REGISTRY.with(|registry| registry.borrow_mut().session_opener = Some(Rc::new(opener)));
}

fn open_session(id: String, cx: &mut App) {
    // 先取出来再调：跳转可能重入注册表。
    let opener = REGISTRY.with(|registry| registry.borrow().session_opener.clone());
    if let Some(opener) = opener {
        opener(id, cx);
    }
}

/// 只取注册表里已挂载的视图，不做 IO。
///
/// 底色由宿主铺，和文件树等内置面板一样是实底的舞台色：主题原色可能带透明度，
/// 插件自己铺会透出窗口后面的东西。插件只画内容，不画页面底。
pub(crate) fn render(slot: PluginTabSlot, cx: &App) -> AnyElement {
    let content = with_registry(|registry| registry.panel(slot).map(|panel| panel.content.clone()));
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
        // 别的会话、项目里还记着已删插件的 tab，切回去时到这里。
        None => div()
            .flex_1()
            .min_h_0()
            .p_4()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("这个插件已经删除。")
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::{MANIFEST_FILE, PluginManifest, declared_hosts, repo_plugin_dir};

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
            assert!(
                ids.insert(manifest.id().to_string()),
                "插件 id 重复：{}",
                manifest.id()
            );
        }
        assert!(!ids.is_empty(), "{} 下没有插件", dir.display());
    }
}
