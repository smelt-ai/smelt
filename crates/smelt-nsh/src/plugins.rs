//! 运行时装插件（A2 架构）：单引擎线程 + 单界面树（`Core`）+ 每插件独立 QuickJS VM。
//!
//! 外壳打成一包（`App.tsx` + `slots.tsx` + `Shell.tsx`），每个插件各自打成标准 ES 模块（[`Compiled`]），
//! 由 [`PluginHost`] 经 `AppView::load_plugin` 载入各自隔离的 QuickJS VM（`harden: true`、独立堆与
//! CPU 熔断、独立 `HostChannel`、独立 `TenantId`）：
//! - 装、改、删一个插件只重建它自己的 VM，外壳里的终端、对话和别的插件不动；
//! - 插件死循环、OOM、超节点配额或越权操作节点只停掉并清理它自己的租户，引擎与外壳不重启；
//! - 只有宿主外壳经内部插槽协议挂载顶层插件视图（`<Slot>` → `smelt.slot.ops` → 引擎内 [`PluginHost`]
//!   路由 → `smelt.slot.sync` → `mountSlot`），热重载时宿主按 `instances` 表原位重放；插件自身不能
//!   使用 `smelt/slots` 嵌套其它插件，复用的重画布一律走原子原生能力组件。
//!
//! 打包和读文件都阻塞，调用方放到后台做；[`PluginHost::apply`] 在主线程。

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use nsh_bundler::BundleOptions;
use nsh_core::{access::Capabilities, element::description::SetDescription};
use nsh_engine::{AppError, AppErrorKind, HostCall, HostChannel, Limits, PluginConfig, Policy};
use nsh_platform_gpui::{App, AppView, Bundle};
use oxc::{
    allocator::Allocator, ast::ast::Statement, parser::Parser, semantic::SemanticBuilder,
    span::SourceType,
};
use serde_json::{Value, json};

const APP: &str = include_str!("plugins/App.tsx");
const PROVIDER: &str = include_str!("plugins/provider.tsx");
const SLOTS: &str = include_str!("plugins/slots.tsx");
const SDK: &str = include_str!("plugins/sdk.tsx");
/// 外壳专用的别名配置：提供外壳内部挂载器 `smelt/slots` 与通用 `smelt/sdk`。
const SHELL_NSH_CONFIG: &str =
    r#"{"resolve":{"alias":{"smelt/slots":"./slots.tsx","smelt/sdk":"./sdk.tsx"}}}"#;
/// Guest 插件的别名配置：仅开放 `smelt/sdk`，不暴露 `smelt/slots`（禁止插件嵌套插件）。
const PLUGIN_NSH_CONFIG: &str = r#"{"resolve":{"alias":{"smelt/sdk":"./sdk.tsx"}}}"#;

/// 外壳上报跨 VM 顶层视图插槽增改删批次。
const SLOT_OPS: &str = "smelt.slot.ops";
/// 宿主推给提供方插件的插槽同步批次（mount / update / unmount）。
const SLOT_SYNC: &str = "smelt.slot.sync";
/// 宿主推给外壳的插件故障状态（显示/清除兜底提示）。
const SLOT_FAILED: &str = "smelt.slot.failed";
/// 提供方插件挂好 `smelt.slot.sync` 监听后上报就绪，触发活跃实例重放。
const PLUGIN_READY: &str = "smelt.plugin.ready";
/// 外壳在宿主桥里的身份标识。
pub(crate) const SHELL_OWNER: &str = "__shell__";

/// 一个插件的入口文件，导出它的视图。
pub(crate) const ENTRY: &str = "main.tsx";

/// 一个插件打好的标准 ES 模块。
#[derive(Clone, Debug)]
pub struct Compiled {
    id: String,
    code: Arc<str>,
}

impl Compiled {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn code(&self) -> &str {
        &self.code
    }
}

/// 一次改动之后，引擎那头要做的。
#[derive(Clone, Debug)]
pub enum Change {
    /// 起一个新引擎：外壳，加上此刻装着的全部插件。
    Start {
        shell: Arc<str>,
        plugins: Vec<Compiled>,
    },
    /// 装上或者原地热重载一个插件。
    Install(Compiled),
    /// 卸掉一个插件。
    Remove(String),
}

/// 装着的插件（编译与暂存层，运行在后台线程）。
pub struct Plugins {
    stage: PathBuf,
    ui_sets: Vec<SetDescription>,
    shell: Arc<str>,
    installed: BTreeMap<String, Compiled>,
}

impl Plugins {
    /// `stage` 是只归它用的目录，打包时摆文件用；`shell` 是导出 `Shell` 的 TSX，插件摆在它画的
    /// `Slot` 里；`ui_sets` 和交给 `set_ui_sets` 的一致。
    pub fn new(stage: &Path, shell: &str, ui_sets: Vec<SetDescription>) -> Result<Self, String> {
        let io = |error: io::Error| format!("{}: {error}", stage.display());
        fs::create_dir_all(stage).map_err(io)?;
        let stage = stage.canonicalize().map_err(io)?;
        let dir = stage.join("shell");
        let files = [
            ("nsh.json", SHELL_NSH_CONFIG),
            ("App.tsx", APP),
            ("provider.tsx", PROVIDER),
            ("slots.tsx", SLOTS),
            ("sdk.tsx", SDK),
            ("Shell.tsx", shell),
        ];
        write_dir(
            &dir,
            files.map(|(path, text)| (Path::new(path), text.as_bytes())),
        )
        .map_err(io)?;
        let bundle = bundle(&dir.join("App.tsx"), &ui_sets)?;
        let served = native_specifiers(&ui_sets);
        validate_imports(SHELL_OWNER, &bundle.code, &served)?;
        Ok(Self {
            shell: bundle.code.into(),
            stage,
            ui_sets,
            installed: BTreeMap::new(),
        })
    }

    /// 起引擎：外壳加上此刻装着的全部插件。
    pub fn start(&self) -> Change {
        Change::Start {
            shell: self.shell.clone(),
            plugins: self.installed.values().cloned().collect(),
        }
    }

    /// 装一个插件，或者它的文件变了。打不过包就什么都不变，它上一份照常跑。
    pub fn install(&mut self, id: &str, dir: &Path) -> Result<Change, String> {
        self.install_entry(id, dir, ENTRY)
    }

    /// 装一个指定入口文件（相对插件目录）的插件。
    pub fn install_entry(
        &mut self,
        id: &str,
        dir: &Path,
        entry_rel: &str,
    ) -> Result<Change, String> {
        let compiled = self.compile(id, dir, entry_rel)?;
        self.installed.insert(id.to_owned(), compiled.clone());
        Ok(Change::Install(compiled))
    }

    /// 装着的插件 id。
    pub fn installed(&self) -> impl Iterator<Item = &str> {
        self.installed.keys().map(String::as_str)
    }

    pub fn remove(&mut self, id: &str) -> Change {
        self.installed.remove(id);
        Change::Remove(id.to_owned())
    }

    /// 拷一份到自己的目录里再打包：插件目录里不落生成的文件，import 也出不了这个目录。
    fn compile(&self, id: &str, source: &Path, entry_rel: &str) -> Result<Compiled, String> {
        if !valid_id(id) {
            return Err(format!("插件 id 不能作目录名：{id}"));
        }
        let rel = Path::new(entry_rel);
        if rel.is_absolute()
            || rel.components().any(|part| {
                matches!(
                    part,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(format!("{id} 的入口路径不合法：{entry_rel}"));
        }
        let normalized_entry: PathBuf = rel
            .components()
            .filter(|part| !matches!(part, Component::CurDir))
            .collect();
        let files = read_dir(source).map_err(|error| format!("{}: {error}", source.display()))?;
        if !files.contains_key(&normalized_entry) {
            return Err(format!("{id} 没有 {entry_rel}"));
        }
        let import_rel = normalized_entry.to_string_lossy().replace('\\', "/");
        let dir = self.stage.join("plugins").join(id);
        let entry = format!(
            "import {{ provideViews }} from \"./provider.tsx\";\n\
             import * as views from \"./plugin/{import_rel}\";\n\
             provideViews({id:?}, views);\n"
        );
        let staged: Vec<(PathBuf, &[u8])> = files
            .iter()
            .map(|(path, bytes)| (Path::new("plugin").join(path), bytes.as_slice()))
            .chain([
                (PathBuf::from("nsh.json"), PLUGIN_NSH_CONFIG.as_bytes()),
                (PathBuf::from("provider.tsx"), PROVIDER.as_bytes()),
                (PathBuf::from("sdk.tsx"), SDK.as_bytes()),
                (PathBuf::from("entry.ts"), entry.as_bytes()),
            ])
            .collect();
        write_dir(
            &dir,
            staged.iter().map(|(path, bytes)| (path.as_path(), *bytes)),
        )
        .map_err(|error| format!("{}: {error}", dir.display()))?;
        let bundle = bundle(&dir.join("entry.ts"), &self.ui_sets)?;
        if let Some(outside) = bundle.watch_files.iter().find(|file| !inside(file, &dir)) {
            return Err(format!(
                "{id} 引了自己目录以外的文件：{}",
                outside.display()
            ));
        }
        if let Some(asset) = bundle.assets.first() {
            return Err(format!("{id} 引了资源文件，还不支持：{}", asset.display()));
        }
        let served = native_specifiers(&self.ui_sets);
        validate_imports(id, &bundle.code, &served)?;
        Ok(Compiled {
            id: id.to_owned(),
            code: bundle.code.into(),
        })
    }
}

/// 引擎内插件运行时与跨 VM 插槽路由器。
///
/// 每个插件独享一条 [`HostChannel`]，回调拿到的 `plugin_id` 由 Rust 闭包绑定，插件无法伪造；
/// 跨 VM 插槽的挂载、参数更新、卸载与热重载重放全部在引擎线程内直转，不回绕 GPUI 主线程。
#[derive(Clone, Default)]
pub struct PluginHost(Arc<Mutex<HostInner>>);

type CallHandler = Arc<dyn Fn(&str, HostCall) + Send + Sync>;

/// 插件 VM 生命周期事件（在引擎线程触发，经 channel 转主线程处理）。
#[derive(Clone, Debug)]
pub enum PluginLifecycle {
    Ready { plugin: String, views: Vec<String> },
    Fatal { plugin: String, message: String },
}

type LifecycleObserver = Arc<dyn Fn(PluginLifecycle) + Send + Sync>;

struct PluginEntry {
    generation: u64,
    channel: HostChannel,
    ready: bool,
}

#[derive(Clone)]
struct SlotInstance {
    target: String,
    plugin: String,
    view: String,
    args: Value,
}

#[derive(Default)]
struct HostInner {
    shell_channel: Option<HostChannel>,
    next_generation: u64,
    plugins: BTreeMap<String, PluginEntry>,
    instances: BTreeMap<String, SlotInstance>,
    failed: BTreeSet<String>,
    handlers: BTreeMap<String, CallHandler>,
    lifecycle: Option<LifecycleObserver>,
    plugin_limits: Option<Limits>,
    /// 默认关闭按墙钟计时的 1s 滚动 CPU 窗（`None`）：macOS 休眠唤醒、电池降频或线程抢占会使单次
    /// 轻量回调的墙钟耗时越过 250ms 而误杀健康插件（如 `smelt.layouts`）。死循环与内存耗尽仍由
    /// `Limits::default()` 的 `call_budget` / `load_budget` / `memory_bytes` / `max_nodes` 熔断保护。
    cpu_window_budget: Option<Duration>,
}

impl PluginHost {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, HostInner> {
        self.0.lock().expect("plugin host lock poisoned")
    }

    /// 覆写 Guest 插件 VM 的资源上限（默认沿用 `PluginConfig::new` 的加固默认值）。
    pub fn set_plugin_limits(&self, limits: Limits) {
        self.lock().plugin_limits = Some(limits);
    }

    /// 覆写 Guest 插件 1 秒滚动 CPU 配额（`None` 关闭滚动窗）。
    pub fn set_cpu_window_budget(&self, budget: Option<Duration>) {
        self.lock().cpu_window_budget = budget;
    }

    /// 登记一条给所有插件调的宿主函数；`handler` 在引擎线程上跑，第一个参数是不可伪造的插件 id。
    pub fn handle(
        &self,
        name: impl Into<String>,
        handler: impl Fn(&str, HostCall) + Send + Sync + 'static,
    ) {
        self.lock().handlers.insert(name.into(), Arc::new(handler));
    }

    /// 把一条宿主事件广播给此刻存活的全部插件 VM。
    pub fn broadcast(&self, event: &str, data: Value) {
        let channels: Vec<HostChannel> = self
            .lock()
            .plugins
            .values()
            .map(|entry| entry.channel.clone())
            .collect();
        for channel in channels {
            if let Err(error) = channel.send(event, data.clone()) {
                eprintln!("[nsh-plugins] 广播 {event} 失败：{error}");
            }
        }
    }

    /// 把一条宿主事件广播给外壳和此刻存活的全部插件 VM。
    pub fn broadcast_all(&self, event: &str, data: Value) {
        let channels: Vec<HostChannel> = {
            let inner = self.lock();
            inner
                .shell_channel
                .iter()
                .cloned()
                .chain(inner.plugins.values().map(|entry| entry.channel.clone()))
                .collect()
        };
        for channel in channels {
            if let Err(error) = channel.send(event, data.clone()) {
                eprintln!("[nsh-plugins] 广播 {event} 失败：{error}");
            }
        }
    }

    /// 登记插件 VM 生命周期观察者（就绪时拿到导出的视图列表，致命故障时清理宿主登记）。
    pub fn set_lifecycle_observer(
        &self,
        observer: impl Fn(PluginLifecycle) + Send + Sync + 'static,
    ) {
        self.lock().lifecycle = Some(Arc::new(observer));
    }

    /// 把一条事件只推给外壳 VM。
    pub fn send_to_shell(&self, event: &str, data: Value) {
        let channel = self.lock().shell_channel.clone();
        if let Some(channel) = channel
            && let Err(error) = channel.send(event, data)
        {
            eprintln!("[nsh-plugins] 推送 {event} 给外壳失败：{error}");
        }
    }

    /// 把一条宿主事件只推给指定的插件 VM。
    pub fn send_to(&self, plugin: &str, event: &str, data: Value) {
        let channel = self
            .lock()
            .plugins
            .get(plugin)
            .map(|entry| entry.channel.clone());
        if let Some(channel) = channel
            && let Err(error) = channel.send(event, data)
        {
            eprintln!("[nsh-plugins] 推送 {event} 给 {plugin} 失败：{error}");
        }
    }

    /// 把一次变更交给引擎：外壳和插件各自运行在 `harden: true` 的独立 QuickJS VM 中。
    pub fn apply(&self, view: &AppView, change: Change) {
        match change {
            Change::Start { shell, plugins } => {
                let shell_channel = view.host();
                let (generation, handlers) = {
                    let mut inner = self.lock();
                    inner.next_generation += 1;
                    inner.shell_channel = Some(shell_channel.clone());
                    inner.plugins.clear();
                    inner.instances.clear();
                    inner.failed.clear();
                    let handlers: Vec<(String, CallHandler)> = inner
                        .handlers
                        .iter()
                        .map(|(name, f)| (name.clone(), f.clone()))
                        .collect();
                    (inner.next_generation, handlers)
                };
                let mut params = serde_json::Map::new();
                params.insert(
                    "__smelt_owner".into(),
                    Value::String(format!("shell:{generation}")),
                );
                shell_channel.set_params(params);
                self.attach_slot_ops(&shell_channel);

                let mut declared_host = vec![SLOT_OPS.to_owned()];
                for (name, handler) in handlers {
                    declared_host.push(name.clone());
                    shell_channel.handle(name, move |call| handler(SHELL_OWNER, call));
                }

                let mut policy = Policy::default();
                let mut declared = Capabilities::all();
                declared.host = declared_host;
                policy.declared = Some(declared);
                view.load(App::Js(
                    Bundle::source(shell.to_string()).with_policy(policy),
                ));
                for plugin in plugins {
                    self.load_one(view, plugin);
                }
            }
            Change::Install(plugin) => self.load_one(view, plugin),
            Change::Remove(id) => {
                let shell_channel = {
                    let mut inner = self.lock();
                    inner.plugins.remove(&id);
                    let was_failed = inner.failed.remove(&id);
                    was_failed.then(|| inner.shell_channel.clone()).flatten()
                };
                if let Some(shell_channel) = shell_channel {
                    send_failed_status(&shell_channel, &id, false);
                }
                view.unload_plugin(id);
            }
        }
    }

    /// 处理引擎报出的 [`AppError`]：若是某个插件的致命错误（顶层抛错、OOM、死循环熔断等），
    /// 仅收尾该插件的路由表并标出故障状态，绝不重启引擎或影响其它插件。
    pub fn on_app_error(&self, error: &AppError) {
        let Some(plugin_id) = error.plugin.as_deref() else {
            return;
        };
        if !error.is_fatal() {
            return;
        }
        let (shell_channel, observer) = {
            let mut inner = self.lock();
            // 若新版本已经加载完成并报就绪，则忽略上一代延迟到达的 Load 错误。
            if error.kind == AppErrorKind::Load
                && inner
                    .plugins
                    .get(plugin_id)
                    .is_some_and(|entry| entry.ready)
            {
                return;
            }
            inner.plugins.remove(plugin_id);
            inner.failed.insert(plugin_id.to_owned());
            (inner.shell_channel.clone(), inner.lifecycle.clone())
        };
        if let Some(shell_channel) = shell_channel {
            send_failed_status(&shell_channel, plugin_id, true);
        }
        if let Some(observer) = observer {
            observer(PluginLifecycle::Fatal {
                plugin: plugin_id.to_owned(),
                message: error.to_string(),
            });
        }
    }

    fn load_one(&self, view: &AppView, plugin: Compiled) {
        let id = plugin.id;
        let channel = HostChannel::new();
        let (generation, handlers, limits, cpu_window, clear_failed_shell) = {
            let mut inner = self.lock();
            inner.next_generation += 1;
            let generation = inner.next_generation;
            let was_failed = inner.failed.remove(&id);
            inner.plugins.insert(
                id.clone(),
                PluginEntry {
                    generation,
                    channel: channel.clone(),
                    ready: false,
                },
            );
            let handlers: Vec<(String, CallHandler)> = inner
                .handlers
                .iter()
                .map(|(name, f)| (name.clone(), f.clone()))
                .collect();
            let clear_failed_shell = was_failed.then(|| inner.shell_channel.clone()).flatten();
            (
                generation,
                handlers,
                inner.plugin_limits,
                inner.cpu_window_budget,
                clear_failed_shell,
            )
        };

        if let Some(shell_channel) = clear_failed_shell {
            send_failed_status(&shell_channel, &id, false);
        }

        let mut params = serde_json::Map::new();
        params.insert("__smelt_plugin_id".into(), Value::String(id.clone()));
        if let Ok(data_dir) = smelt_plugin_api::plugin_data_dir(&id) {
            params.insert(
                "__smelt_data_dir".into(),
                Value::String(data_dir.to_string_lossy().into_owned()),
            );
        }
        channel.set_params(params);

        // 插件初始化完 provideViews 后上报就绪：宿主立即按外壳 instances 表原位重放。
        {
            let host = self.clone();
            let plugin_id = id.clone();
            let ready_channel = channel.clone();
            channel.handle(PLUGIN_READY, move |call| {
                let views: Vec<String> = call
                    .data()
                    .get("views")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let (replay, observer) = {
                    let mut inner = host.lock();
                    let Some(entry) = inner.plugins.get_mut(&plugin_id) else {
                        return;
                    };
                    if entry.generation != generation {
                        return;
                    }
                    entry.ready = true;
                    let replay: Vec<Value> = inner
                        .instances
                        .iter()
                        .filter(|(_, inst)| inst.plugin == plugin_id)
                        .map(|(instance, inst)| {
                            json!({
                                "op": "mount",
                                "instance": instance,
                                "target": inst.target,
                                "view": inst.view,
                                "args": inst.args,
                            })
                        })
                        .collect();
                    (replay, inner.lifecycle.clone())
                };
                if !replay.is_empty()
                    && let Err(error) = ready_channel.send(SLOT_SYNC, Value::Array(replay))
                {
                    eprintln!("[nsh-plugins] 重放 {plugin_id} 插槽失败：{error}");
                }
                if let Some(observer) = observer {
                    observer(PluginLifecycle::Ready {
                        plugin: plugin_id.clone(),
                        views,
                    });
                }
                call.resolve(Value::Null);
            });
        }

        // 绑定所有宿主函数：plugin_id 由 Rust 闭包固定，插件无法篡改。
        // Guest 插件不具备 SLOT_OPS 权限：只有宿主外壳能挂载插件视图。
        let mut declared_host = vec![PLUGIN_READY.to_owned()];
        for (name, handler) in handlers {
            declared_host.push(name.clone());
            let plugin_id = id.clone();
            channel.handle(name, move |call| handler(&plugin_id, call));
        }

        let mut policy = Policy::default();
        let mut declared = Capabilities::all();
        declared.host = declared_host;
        policy.declared = Some(declared);
        let bundle = Bundle::source(plugin.code.to_string()).with_policy(policy);
        let mut config = PluginConfig::new(id, bundle)
            .with_host(channel)
            .with_cpu_window_budget(cpu_window);
        if let Some(limits) = limits {
            config = config.with_limits(limits);
        }
        view.load_plugin(config);
    }

    fn attach_slot_ops(&self, channel: &HostChannel) {
        let host = self.clone();
        channel.handle(SLOT_OPS, move |mut call| {
            let Value::Array(ops) = call.take_data() else {
                return;
            };
            let outgoing = host.lock().apply_slot_ops(ops);
            flush_sync_batches(outgoing);
        });
    }
}

impl HostInner {
    fn apply_slot_ops(&mut self, ops: Vec<Value>) -> Vec<(HostChannel, Vec<Value>)> {
        let mut by_target: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for mut item in ops {
            let Some(op) = item.get("op").and_then(Value::as_str).map(str::to_owned) else {
                continue;
            };
            let Some(instance) = item
                .get("instance")
                .and_then(Value::as_str)
                .map(str::to_owned)
            else {
                continue;
            };
            match op.as_str() {
                "mount" => {
                    let Some(target) = item
                        .get("target")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                    else {
                        continue;
                    };
                    let Some(plugin) = item
                        .get("plugin")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                    else {
                        continue;
                    };
                    let view = item
                        .get("view")
                        .and_then(Value::as_str)
                        .unwrap_or("default")
                        .to_owned();
                    let args = item.get_mut("args").map(Value::take).unwrap_or(Value::Null);
                    if let Some(prev) = self.instances.insert(
                        instance.clone(),
                        SlotInstance {
                            target: target.clone(),
                            plugin: plugin.clone(),
                            view: view.clone(),
                            args: args.clone(),
                        },
                    ) && prev.plugin != plugin
                    {
                        by_target
                            .entry(prev.plugin)
                            .or_default()
                            .push(json!({ "op": "unmount", "instance": instance }));
                    }
                    by_target.entry(plugin).or_default().push(json!({
                        "op": "mount",
                        "instance": instance,
                        "target": target,
                        "view": view,
                        "args": args,
                    }));
                }
                "update" => {
                    let args = item.get_mut("args").map(Value::take).unwrap_or(Value::Null);
                    let Some(inst) = self.instances.get_mut(&instance) else {
                        continue;
                    };
                    inst.args = args.clone();
                    by_target
                        .entry(inst.plugin.clone())
                        .or_default()
                        .push(json!({
                            "op": "update",
                            "instance": instance,
                            "args": args,
                        }));
                }
                "unmount" => {
                    if let Some(inst) = self.instances.remove(&instance) {
                        by_target
                            .entry(inst.plugin)
                            .or_default()
                            .push(json!({ "op": "unmount", "instance": instance }));
                    }
                }
                _ => {}
            }
        }
        self.resolve_ready_batches(by_target)
    }

    fn resolve_ready_batches(
        &self,
        by_target: BTreeMap<String, Vec<Value>>,
    ) -> Vec<(HostChannel, Vec<Value>)> {
        let mut out = Vec::new();
        for (target_plugin, batch) in by_target {
            if batch.is_empty() {
                continue;
            }
            if let Some(entry) = self.plugins.get(&target_plugin)
                && entry.ready
            {
                out.push((entry.channel.clone(), batch));
            }
        }
        out
    }
}

fn flush_sync_batches(batches: Vec<(HostChannel, Vec<Value>)>) {
    for (channel, batch) in batches {
        if let Err(error) = channel.send(SLOT_SYNC, Value::Array(batch)) {
            eprintln!("[nsh-plugins] 下发插槽同步失败：{error}");
        }
    }
}

fn send_failed_status(channel: &HostChannel, plugin: &str, failed: bool) {
    let _ = channel.send(SLOT_FAILED, json!({ "plugin": plugin, "failed": failed }));
}

fn bundle(entry: &Path, ui_sets: &[SetDescription]) -> Result<nsh_bundler::Bundle, String> {
    nsh_bundler::bundle(&BundleOptions {
        entry: entry.to_path_buf(),
        ui_sets: ui_sets.to_vec(),
        ..Default::default()
    })
    .map_err(|error| error.plain)
}

/// 插件能 import 的宿主模块。
fn native_specifiers(ui_sets: &[SetDescription]) -> Vec<String> {
    nsh_core::js::NATIVE_MODULES
        .iter()
        .map(|name| (*name).to_owned())
        .chain(ui_sets.iter().map(|set| set.specifier.clone()))
        .collect()
}

/// QuickJS + NSH 沙箱提供的合法全局标识符；打包产物里除此以外的未解析自由标识符多半是漏 import 或拼写错误。
const ALLOWED_GLOBALS: &[&str] = &[
    // ECMAScript 标准全局
    "AggregateError",
    "Array",
    "ArrayBuffer",
    "Atomics",
    "BigInt",
    "BigInt64Array",
    "BigUint64Array",
    "Boolean",
    "DataView",
    "Date",
    "Error",
    "EvalError",
    "FinalizationRegistry",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "Function",
    "Infinity",
    "Int8Array",
    "Int16Array",
    "Int32Array",
    "Intl",
    "JSON",
    "Map",
    "Math",
    "NaN",
    "Number",
    "Object",
    "Promise",
    "Proxy",
    "RangeError",
    "ReferenceError",
    "Reflect",
    "RegExp",
    "Set",
    "SharedArrayBuffer",
    "String",
    "Symbol",
    "SyntaxError",
    "TypeError",
    "URIError",
    "Uint8Array",
    "Uint8ClampedArray",
    "Uint16Array",
    "Uint32Array",
    "WeakMap",
    "WeakRef",
    "WeakSet",
    "decodeURI",
    "decodeURIComponent",
    "encodeURI",
    "encodeURIComponent",
    "escape",
    "eval",
    "globalThis",
    "isFinite",
    "isNaN",
    "parseFloat",
    "parseInt",
    "undefined",
    "unescape",
    // QuickJS / Web / NSH 运行时全局
    "AbortController",
    "AbortSignal",
    "CustomEvent",
    "Event",
    "EventTarget",
    "Headers",
    "Request",
    "Response",
    "TextDecoder",
    "TextEncoder",
    "URL",
    "URLSearchParams",
    "atob",
    "btoa",
    "clearInterval",
    "clearTimeout",
    "console",
    "crypto",
    "fetch",
    "performance",
    "queueMicrotask",
    "setInterval",
    "setTimeout",
    "structuredClone",
];

/// 只读校验打包后的 ES 模块仅 import 宿主允许的模块，拒绝未知外部依赖（不做任何 AST 改写）。
fn validate_imports(id: &str, code: &str, served: &[String]) -> Result<(), String> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, SourceType::mjs()).parse();
    if let Some(error) = parsed.diagnostics.first() {
        return Err(format!("{id}: {error}"));
    }
    for statement in &parsed.program.body {
        let Statement::ImportDeclaration(import) = statement else {
            continue;
        };
        // 按包里剩下的 import 查，不按 `Bundle::natives`：打包时认不得的模块当外部依赖原样留着，
        // 不在那张表里。
        let source = import.source.value.as_str();
        if !served.iter().any(|name| name == source) {
            return Err(format!("{id} 引了宿主没给的模块：{source}"));
        }
    }
    Ok(())
}

/// 静态检查打包后的 ES 模块不含未声明的自由标识符（供 `check_plugin` 与内置插件校验用）。
pub(crate) fn validate_unresolved_references(id: &str, code: &str) -> Result<(), String> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, SourceType::mjs()).parse();
    if let Some(error) = parsed.diagnostics.first() {
        return Err(format!("{id}: {error}"));
    }
    let semantic = SemanticBuilder::new().build(&parsed.program).semantic;
    let mut unknown: Vec<String> = semantic
        .scoping()
        .root_unresolved_references()
        .keys()
        .map(|name| name.as_str())
        .filter(|name| !ALLOWED_GLOBALS.contains(name))
        .map(str::to_owned)
        .collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(format!("{id} 引用了未定义的标识符：{}", unknown.join(", ")));
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    !id.starts_with('.')
        && !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// `file` 在 `dir` 里。两边都是规范过的绝对路径，不含 `..`。
fn inside(file: &Path, dir: &Path) -> bool {
    file.starts_with(dir) && !file.components().any(|part| part == Component::ParentDir)
}

/// 目录里的文件，不跟符号链接，不要点开头的和 `node_modules`。
fn read_dir(root: &Path) -> io::Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut dirs = vec![PathBuf::new()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(root.join(&dir))? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') || name == "node_modules" {
                continue;
            }
            let path = dir.join(&name);
            let kind = entry.file_type()?;
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file() {
                files.insert(path, fs::read(entry.path())?);
            }
        }
    }
    Ok(files)
}

/// 把 `dir` 整个换成这些文件。
fn write_dir<'a>(
    dir: &Path,
    files: impl IntoIterator<Item = (&'a Path, &'a [u8])>,
) -> io::Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    for (path, bytes) in files {
        let path = dir.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_plugin_host_disables_wall_clock_cpu_window() {
        let host = PluginHost::new();
        assert_eq!(host.lock().cpu_window_budget, None);
    }

    #[test]
    fn validate_unresolved_references_rejects_undeclared_identifier() {
        let err = validate_unresolved_references(
            "demo",
            "export function f() { return missingHelper(1); }",
        )
        .expect_err("未定义标识符应当被拦截");
        assert!(
            err.contains("missingHelper"),
            "错误信息应点出未定义名：{err}"
        );
    }

    #[test]
    fn all_builtin_plugins_have_no_undeclared_identifiers() {
        let stage =
            std::env::temp_dir().join(format!("smelt-nsh-shell-check-{}", uuid::Uuid::new_v4()));
        let shell_plugins = Plugins::new(
            &stage,
            include_str!("../shell/MainShell.tsx"),
            crate::ui_sets(),
        );
        let _ = fs::remove_dir_all(&stage);
        let shell_plugins = shell_plugins.expect("MainShell.tsx 打包失败");
        validate_unresolved_references(SHELL_OWNER, &shell_plugins.shell)
            .expect("MainShell.tsx 存在未定义标识符");

        let repo_plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        let mut errors = Vec::new();
        for entry in fs::read_dir(&repo_plugins).expect("读取 plugins/") {
            let path = entry.expect("dir entry").path();
            if path.join(crate::MANIFEST_FILE).is_file()
                && let Err(err) = crate::check_plugin(&path)
            {
                errors.push(format!("{}: {err}", path.display()));
            }
        }
        assert!(
            errors.is_empty(),
            "内置插件静态校验失败：\n{}",
            errors.join("\n")
        );
    }
}
