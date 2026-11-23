//! 给外壳和所有独立插件 VM 的通用宿主桥：一份只读通用状态（宿主变了广播整份）、离散宿主事件转发
//! （`smelt.event`）、双向命令派发（`smelt.command` / `command.reply`），以及通用宿主调用
//! （`state`、`run`、`call`、`request`、`context_menu_items`、`clipboard.write`、`fs.path_kind`、
//! `fs.reveal_path`、`sys.process_usage`、`os.discover_apps`、`os.launch_path`、`set_locale`、`set_active_context`、`open_file`、`open_url`、`register_views`、
//! `register_commands`、`register_topics`、`register_layouts`、`register_pages`、
//! `register_context_menu_items`、`register_menu_items`、`register_file_opener`）。
//!
//! 只给通用事实与能力，不认识任何具体插件。
//! 插件的 `host.call` 经各自的 `HostChannel` 在引擎线程上直达，携带不可伪造的 `plugin_id`：
//! `state` 与 `command.reply` 当场在引擎线程处理；`fs.path_kind` 在后台线程做不碰主线程；
//! 需要 GPUI `&App` 的调用封装为 [`Action`] 转主线程执行。

use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use gpui::{
    App, AppContext as _, ClipboardItem, Context, IntoElement, Render, SharedString, Window, div,
};
use nsh_engine::HostCall;
use serde_json::{Value, json};
use smelt_plugin_api::{
    self as api, CommandHandler, CommandSpec, ContextMenuAction, ContextMenuItem, Handled,
    HostEvent, LayoutMaterials, LayoutSource, Material, MenuEntry, MenuImage, MenuSlot, PageSource,
    Placement, PluginView, RuntimeKey, SettingsEntry, SettingsGroup, Text, TopicMessage,
    ViewHandle, ViewSpec,
    client::protocol::control::ControlMethod,
    model::{agent_status::AgentStatus, font_config, runtime_path::PathStyle},
};
use smelt_ui::{
    kit::{RADIUS, TYPE, TextStyle},
    ui_theme,
};
use tokio::sync::{mpsc, oneshot};

use crate::plugins::PluginHost;

/// 推给外壳和插件的事件：整份状态，`revision` 只增不减。
const STATE: &str = "smelt.state";
/// 推给插件的离散宿主事件（runtime_topic、plugin、window_activated、open_paths 等）。
const EVENT: &str = "smelt.event";
/// 推给插件的命令调用请求，插件跑完后用 `command.reply` 交回结果。
const COMMAND: &str = "smelt.command";

/// 插件发往主线程执行的通用宿主动作。
pub(crate) enum Action {
    Run {
        command: String,
        args: Value,
        call: HostCall,
    },
    Call {
        machine: RuntimeKey,
        method: ControlMethod,
        params: Value,
        timeout: Duration,
        detailed: bool,
        call: HostCall,
    },
    Request {
        machine: RuntimeKey,
        payload: Value,
        timeout: Duration,
        call: HostCall,
    },
    ContextMenuItems {
        menu: String,
        machine: RuntimeKey,
        context: Value,
        call: HostCall,
    },
    ClipboardWrite {
        text: String,
        call: HostCall,
    },
    SetLocale {
        locale: Option<String>,
        call: HostCall,
    },
    SetActiveContext {
        context: api::ActiveContext,
        call: HostCall,
    },
    OpenFile {
        location: api::FileLocation,
        call: HostCall,
    },
    OpenUrl {
        url: String,
        call: HostCall,
    },
    EmitEvent {
        name: String,
        payload: Value,
        call: HostCall,
    },
    RegisterTopics {
        plugin: String,
        topics: Vec<String>,
        call: HostCall,
    },
    RegisterViews {
        plugin: String,
        views: Vec<Value>,
        call: HostCall,
    },
    RegisterCommands {
        plugin: String,
        group_title: Option<Value>,
        commands: Vec<Value>,
        call: HostCall,
    },
    RegisterLayouts {
        plugin: String,
        layouts: Vec<Value>,
        call: HostCall,
    },
    RegisterPages {
        plugin: String,
        pages: Vec<Value>,
        call: HostCall,
    },
    RegisterContextMenuItems {
        plugin: String,
        menu: String,
        items: Vec<Value>,
        call: HostCall,
    },
    RegisterMenuItems {
        plugin: String,
        items: Vec<Value>,
        call: HostCall,
    },
    RegisterFileOpener {
        plugin: String,
        name: String,
        call: HostCall,
    },
    SetStatusMenu {
        plugin: String,
        menu: Option<api::StatusMenu>,
        call: HostCall,
    },
    SetTerminalFontPx {
        px: u32,
        call: HostCall,
    },
}

struct PendingCommand {
    plugin: String,
    reply: oneshot::Sender<Result<Value, String>>,
}

#[derive(Default)]
struct Shared {
    /// 推给 Guest 插件的状态快照（不含仅主壳消费的 `shell` 布局树）。
    plugin_state: Value,
    plugin_revision: u64,
    /// 推给主壳 VM（`__shell__`）的完整状态快照（附加 `shell` 布局树）。
    shell_state: Value,
    shell_revision: u64,
    /// 插件 id → 它 `locales/` 里的文案（语言 → 键 → 字）。
    texts: BTreeMap<String, Value>,
    /// 插件 id → 它登记订阅的运行时话题集合。
    topics: BTreeMap<String, BTreeSet<String>>,
    /// JS 插件登记的视图全称 `<plugin>/<name>` → `(plugin_id, export_name)`，供主壳 `<HostView>` 区分 JS/Rust。
    js_views: BTreeMap<String, (String, String)>,
    /// 发往插件 VM 等待 `command.reply` 的在途命令。
    next_command_id: u64,
    pending_commands: BTreeMap<u64, PendingCommand>,
}

#[derive(Clone, Default)]
pub(crate) struct Bridge(Arc<Mutex<Shared>>);

impl Bridge {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.0.lock().expect("host bridge lock poisoned")
    }

    /// 把通用宿主调用登记到 [`PluginHost`] 上。在装载插件之前调一次。
    pub fn serve(&self, plugins: &PluginHost) -> mpsc::UnboundedReceiver<Action> {
        let (actions, received) = mpsc::unbounded_channel();

        let bridge = self.clone();
        plugins.handle("state", move |plugin, call| {
            let state = {
                let shared = bridge.lock();
                let mut state = if plugin == crate::plugins::SHELL_OWNER {
                    shared.shell_state.clone()
                } else {
                    shared.plugin_state.clone()
                };
                if let Some(fields) = state.as_object_mut() {
                    let texts = shared.texts.get(plugin).cloned();
                    fields.insert("texts".into(), texts.unwrap_or_else(|| json!({})));
                }
                state
            };
            call.resolve(state);
        });

        {
            let bridge = self.clone();
            plugins.handle("command.reply", move |plugin, mut call| {
                let mut data = call.take_data();
                let Some(id) = data["id"].as_u64() else {
                    return call.reject("command.reply：缺 id");
                };
                let ok = data["ok"].as_bool().unwrap_or(false);
                let result = if ok {
                    Ok(data["value"].take())
                } else {
                    Err(data["error"]
                        .as_str()
                        .unwrap_or("插件命令执行失败")
                        .to_owned())
                };
                let pending = {
                    let mut shared = bridge.lock();
                    let matches = shared
                        .pending_commands
                        .get(&id)
                        .is_some_and(|item| item.plugin == plugin);
                    matches
                        .then(|| shared.pending_commands.remove(&id))
                        .flatten()
                };
                if let Some(pending) = pending {
                    let _ = pending.reply.send(result);
                }
                call.resolve(Value::Null);
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("run", move |_plugin, mut call| {
                let mut data = call.take_data();
                let Some(command) = data["command"].as_str().map(str::to_owned) else {
                    return call.reject("run：缺 command");
                };
                let args = data["args"].take();
                send_action(
                    &actions,
                    Action::Run {
                        command,
                        args,
                        call,
                    },
                );
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("call", move |_plugin, mut call| {
                let mut data = call.take_data();
                let Some(machine) = parse_machine(data.get("machine")) else {
                    return call.reject("call：machine 要是 null 或字符串");
                };
                let Some(name) = data["method"].as_str() else {
                    return call.reject("call：缺 method");
                };
                let Some(method) = ControlMethod::from_wire_name(name) else {
                    return call.reject(format!("call：运行时没有「{name}」这个方法"));
                };
                let params = match data.get_mut("params").map(Value::take) {
                    None | Some(Value::Null) => Value::Object(Default::default()),
                    Some(params) => params,
                };
                let timeout = match data.get("timeout_ms") {
                    None | Some(Value::Null) => api::CALL_TIMEOUT,
                    Some(value) => match value.as_f64() {
                        Some(ms) if ms > 0.0 && ms.is_finite() => Duration::from_millis(ms as u64),
                        _ => return call.reject("call：timeout_ms 要是正数"),
                    },
                };
                let detailed = data
                    .get("detailed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                send_action(
                    &actions,
                    Action::Call {
                        machine,
                        method,
                        params,
                        timeout,
                        detailed,
                        call,
                    },
                );
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("context_menu_items", move |_plugin, mut call| {
                let mut data = call.take_data();
                let Some(menu) = data["menu"].as_str().map(str::to_owned) else {
                    return call.reject("context_menu_items：缺 menu");
                };
                let Some(machine) = parse_machine(data.get("machine")) else {
                    return call.reject("context_menu_items：machine 要是 null 或字符串");
                };
                let context = data["context"].take();
                send_action(
                    &actions,
                    Action::ContextMenuItems {
                        menu,
                        machine,
                        context,
                        call,
                    },
                );
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("clipboard.write", move |_plugin, call| {
                let Some(text) = call.data()["text"].as_str().map(str::to_owned) else {
                    return call.reject("clipboard.write：缺 text 字符串");
                };
                send_action(&actions, Action::ClipboardWrite { text, call });
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("set_status_menu", move |plugin, mut call| {
                let mut data = call.take_data();
                let raw = data.get_mut("menu").map(Value::take).unwrap_or(Value::Null);
                let menu = if raw.is_null() {
                    None
                } else {
                    match serde_json::from_value::<api::StatusMenu>(raw) {
                        Ok(menu) => Some(menu),
                        Err(error) => {
                            return call.reject(format!("set_status_menu：菜单格式不对：{error}"));
                        }
                    }
                };
                send_action(
                    &actions,
                    Action::SetStatusMenu {
                        plugin: plugin.to_owned(),
                        menu,
                        call,
                    },
                );
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("set_terminal_font_px", move |_plugin, call| {
                let Some(px) = call.data().get("px").and_then(Value::as_u64) else {
                    return call.reject("set_terminal_font_px：缺 px 整数");
                };
                send_action(
                    &actions,
                    Action::SetTerminalFontPx {
                        px: px as u32,
                        call,
                    },
                );
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("set_locale", move |_plugin, call| {
                let locale = match call.data().get("locale") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(code)) => Some(code.clone()),
                    Some(_) => return call.reject("set_locale：locale 要是字符串或 null"),
                };
                send_action(&actions, Action::SetLocale { locale, call });
            });
        }

        plugins.handle("fs.path_kind", move |_plugin, call| {
            let Some(path) = call.data()["path"].as_str().map(str::to_owned) else {
                return call.reject("fs.path_kind：缺 path 字符串");
            };
            std::thread::spawn(move || {
                let kind = match std::fs::metadata(&path) {
                    Ok(meta) if meta.is_dir() => Value::String("dir".to_owned()),
                    Ok(meta) if meta.is_file() => Value::String("file".to_owned()),
                    _ => Value::Null,
                };
                call.resolve(kind);
            });
        });

        plugins.handle("fs.reveal_path", move |_plugin, call| {
            let Some(path) = call.data()["path"].as_str().map(str::to_owned) else {
                return call.reject("fs.reveal_path：缺 path 字符串");
            };
            std::thread::spawn(move || {
                reveal_local_path(&path);
                call.resolve(Value::Null);
            });
        });

        {
            let sampler = Arc::new(Mutex::new(None::<api::process_usage::ProcessSampler>));
            plugins.handle("sys.process_usage", move |_plugin, call| {
                let sampler = sampler.clone();
                std::thread::spawn(move || {
                    let sampled = {
                        let mut guard = sampler.lock().expect("process sampler lock poisoned");
                        let sampler =
                            guard.get_or_insert_with(api::process_usage::ProcessSampler::new);
                        sampler.sample_process(std::process::id()).map(|usage| {
                            let memory = api::process_usage::own_memory();
                            let breakdown = memory.map(|m| {
                                json!({
                                    "total": m.total,
                                    "graphics": m.graphics,
                                    "heap": m.heap,
                                    "other": m.other(),
                                })
                            });
                            json!({
                                "cpu_percent": usage.cpu_percent,
                                "memory_bytes": memory.map_or(usage.memory_bytes, |m| m.total),
                                "process_count": usage.processes,
                                "memory_breakdown": breakdown,
                            })
                        })
                    };
                    call.resolve(sampled.unwrap_or(Value::Null));
                });
            });
        }

        plugins.handle("os.discover_apps", move |_plugin, mut call| {
            let queries: Vec<crate::os_apps::DiscoverAppQuery> =
                match serde_json::from_value(call.take_data()["apps"].take()) {
                    Ok(queries) => queries,
                    Err(error) => {
                        return call.reject(format!("os.discover_apps：apps 参数不对：{error}"));
                    }
                };
            std::thread::spawn(move || {
                let found = crate::os_apps::discover_apps(&queries);
                match serde_json::to_value(found) {
                    Ok(value) => call.resolve(value),
                    Err(error) => call.reject(error.to_string()),
                }
            });
        });

        plugins.handle("os.launch_path", move |_plugin, call| {
            let data = call.data();
            let Some(kind) = data["kind"].as_str().map(str::to_owned) else {
                return call.reject("os.launch_path：缺 kind 字符串");
            };
            let Some(target) = data["target"].as_str().map(str::to_owned) else {
                return call.reject("os.launch_path：缺 target 字符串");
            };
            let Some(path) = data["path"].as_str().map(str::to_owned) else {
                return call.reject("os.launch_path：缺 path 字符串");
            };
            let require_dir = data
                .get("require_dir")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            std::thread::spawn(move || {
                match crate::os_apps::launch_path(&kind, &target, &path, require_dir) {
                    Ok(()) => call.resolve(Value::Null),
                    Err(error) => call.reject(error),
                }
            });
        });

        {
            let bridge = self.clone();
            let actions = actions.clone();
            plugins.handle("register_topics", move |plugin, call| {
                let Some(items) = call.data()["topics"].as_array() else {
                    return call.reject("register_topics：缺 topics 字符串数组");
                };
                let mut topics = Vec::with_capacity(items.len());
                for item in items {
                    let Some(topic) = item.as_str() else {
                        return call.reject("register_topics：话题名要是字符串");
                    };
                    topics.push(topic.to_owned());
                }
                {
                    let mut shared = bridge.lock();
                    if topics.is_empty() {
                        shared.topics.remove(plugin);
                    } else {
                        shared
                            .topics
                            .insert(plugin.to_owned(), topics.iter().cloned().collect());
                    }
                }
                send_action(
                    &actions,
                    Action::RegisterTopics {
                        plugin: plugin.to_owned(),
                        topics,
                        call,
                    },
                );
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("register_views", move |plugin, mut call| {
                let mut data = call.take_data();
                let Some(views) = data.get_mut("views").and_then(Value::as_array_mut) else {
                    return call.reject("register_views：缺 views 数组");
                };
                let views = std::mem::take(views);
                send_action(
                    &actions,
                    Action::RegisterViews {
                        plugin: plugin.to_owned(),
                        views,
                        call,
                    },
                );
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("register_commands", move |plugin, mut call| {
                let mut data = call.take_data();
                let Some(commands) = data.get_mut("commands").and_then(Value::as_array_mut) else {
                    return call.reject("register_commands：缺 commands 数组");
                };
                let commands = std::mem::take(commands);
                let group_title = data
                    .get_mut("group_title")
                    .map(Value::take)
                    .filter(|v| !v.is_null());
                send_action(
                    &actions,
                    Action::RegisterCommands {
                        plugin: plugin.to_owned(),
                        group_title,
                        commands,
                        call,
                    },
                );
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("register_layouts", move |plugin, mut call| {
                let mut data = call.take_data();
                let Some(layouts) = data.get_mut("layouts").and_then(Value::as_array_mut) else {
                    return call.reject("register_layouts：缺 layouts 数组");
                };
                let layouts = std::mem::take(layouts);
                send_action(
                    &actions,
                    Action::RegisterLayouts {
                        plugin: plugin.to_owned(),
                        layouts,
                        call,
                    },
                );
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("register_pages", move |plugin, mut call| {
                let mut data = call.take_data();
                let Some(pages) = data.get_mut("pages").and_then(Value::as_array_mut) else {
                    return call.reject("register_pages：缺 pages 数组");
                };
                let pages = std::mem::take(pages);
                send_action(
                    &actions,
                    Action::RegisterPages {
                        plugin: plugin.to_owned(),
                        pages,
                        call,
                    },
                );
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("register_context_menu_items", move |plugin, mut call| {
                let mut data = call.take_data();
                let Some(menu) = data["menu"].as_str().map(str::to_owned) else {
                    return call.reject("register_context_menu_items：缺 menu");
                };
                let Some(items) = data.get_mut("items").and_then(Value::as_array_mut) else {
                    return call.reject("register_context_menu_items：缺 items 数组");
                };
                let items = std::mem::take(items);
                send_action(
                    &actions,
                    Action::RegisterContextMenuItems {
                        plugin: plugin.to_owned(),
                        menu,
                        items,
                        call,
                    },
                );
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("request", move |_plugin, mut call| {
                let mut data = call.take_data();
                let Some(machine) = parse_machine(data.get("machine")) else {
                    return call.reject("request：machine 要是 null 或字符串");
                };
                let payload = data["payload"].take();
                if !payload.is_object() {
                    return call.reject("request：payload 要是 JSON 对象");
                }
                let timeout = match data.get("timeout_ms") {
                    None | Some(Value::Null) => api::CALL_TIMEOUT,
                    Some(value) => match value.as_f64() {
                        Some(ms) if ms > 0.0 && ms.is_finite() => Duration::from_millis(ms as u64),
                        _ => return call.reject("request：timeout_ms 要是正数"),
                    },
                };
                send_action(
                    &actions,
                    Action::Request {
                        machine,
                        payload,
                        timeout,
                        call,
                    },
                );
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("set_active_context", move |_plugin, call| {
                let data = call.data();
                let project = match data.get("project") {
                    None | Some(Value::Null) => None,
                    Some(obj) => {
                        let Some(machine) = parse_machine(obj.get("machine")) else {
                            return call.reject("set_active_context：project.machine 不对");
                        };
                        let Some(root) = obj.get("root").and_then(Value::as_str) else {
                            return call.reject("set_active_context：project 缺 root");
                        };
                        Some(api::Project {
                            machine,
                            root: root.to_owned(),
                        })
                    }
                };
                let session = match data.get("session") {
                    None | Some(Value::Null) => None,
                    Some(obj) => {
                        let Some(machine) = parse_machine(obj.get("machine")) else {
                            return call.reject("set_active_context：session.machine 不对");
                        };
                        let Some(id) = obj.get("id").and_then(Value::as_str) else {
                            return call.reject("set_active_context：session 缺 id");
                        };
                        let at_latest = obj
                            .get("at_latest")
                            .and_then(Value::as_bool)
                            .unwrap_or(true);
                        Some(api::ViewedSession {
                            machine,
                            id: id.to_owned(),
                            at_latest,
                        })
                    }
                };
                send_action(
                    &actions,
                    Action::SetActiveContext {
                        context: api::ActiveContext { project, session },
                        call,
                    },
                );
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("open_file", move |_plugin, call| {
                let Some(location) = api::FileLocation::from_args(call.data()) else {
                    return call.reject("open_file：需要 { machine, path, line? }");
                };
                send_action(&actions, Action::OpenFile { location, call });
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("open_url", move |_plugin, call| {
                let Some(url) = call.data()["url"].as_str().map(str::to_owned) else {
                    return call.reject("open_url：缺 url 字符串");
                };
                send_action(&actions, Action::OpenUrl { url, call });
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("emit_event", move |plugin, mut call| {
                let mut data = call.take_data();
                let Some(name) = data["name"].as_str().map(str::to_owned) else {
                    return call.reject("emit_event：缺 name 字符串");
                };
                let prefix = format!("{plugin}/");
                if !name.starts_with(&prefix) {
                    return call.reject(format!("emit_event：事件名必须以 {prefix} 开头"));
                }
                let payload = data
                    .get_mut("payload")
                    .map(Value::take)
                    .unwrap_or(Value::Null);
                send_action(
                    &actions,
                    Action::EmitEvent {
                        name,
                        payload,
                        call,
                    },
                );
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("register_menu_items", move |plugin, mut call| {
                let mut data = call.take_data();
                let Some(items) = data.get_mut("items").and_then(Value::as_array_mut) else {
                    return call.reject("register_menu_items：缺 items 数组");
                };
                let items = std::mem::take(items);
                send_action(
                    &actions,
                    Action::RegisterMenuItems {
                        plugin: plugin.to_owned(),
                        items,
                        call,
                    },
                );
            });
        }

        {
            plugins.handle("register_file_opener", move |plugin, call| {
                let Some(name) = call.data()["name"].as_str().map(str::to_owned) else {
                    return call.reject("register_file_opener：缺 name 字符串");
                };
                send_action(
                    &actions,
                    Action::RegisterFileOpener {
                        plugin: plugin.to_owned(),
                        name,
                        call,
                    },
                );
            });
        }

        received
    }

    pub fn set_texts(&self, plugin: &str, texts: Value) {
        self.lock().texts.insert(plugin.to_owned(), texts);
    }

    /// 记录插件的一批 JS 视图映射（先清除该插件旧映射再写入）。
    pub fn set_js_views(&self, plugin: &str, views: impl IntoIterator<Item = (String, String)>) {
        let mut shared = self.lock();
        shared.js_views.retain(|_, (owner, _)| owner != plugin);
        for (full_id, export_name) in views {
            shared
                .js_views
                .insert(full_id, (plugin.to_owned(), export_name));
        }
    }

    pub fn is_js_view(&self, id: &str) -> bool {
        self.lock().js_views.contains_key(id)
    }

    pub fn forget_plugin(&self, plugin: &str) {
        let orphaned: Vec<oneshot::Sender<Result<Value, String>>> = {
            let mut shared = self.lock();
            shared.texts.remove(plugin);
            shared.topics.remove(plugin);
            shared.js_views.retain(|_, (owner, _)| owner != plugin);
            let ids: Vec<u64> = shared
                .pending_commands
                .iter()
                .filter(|(_, item)| item.plugin == plugin)
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| shared.pending_commands.remove(&id).map(|item| item.reply))
                .collect()
        };
        for reply in orphaned {
            let _ = reply.send(Err(format!("插件 {plugin} 已卸载")));
        }
    }

    /// 撤下插件在宿主 `smelt_plugin_api` 里登记的全部内容并清理本桥上的记录。
    pub fn unregister_plugin(&self, plugin: &str, cx: &mut App) {
        self.forget_plugin(plugin);
        api::unregister_layouts(plugin, cx);
        api::unregister_pages(plugin, cx);
        api::unregister_commands(plugin, cx);
        api::unregister_file_opener(plugin, cx);
        api::unregister_menu_items(plugin, cx);
        api::set_status_menu(plugin, None, cx);
        api::unregister_context_menu_items(plugin, cx);
        api::unregister_quit_guard(plugin, cx);
        api::unregister_topics(plugin, cx);
        api::unregister_views(plugin, cx);
    }

    /// 向插件 VM 派发一条已登记命令，返回等待结果的 receiver。
    pub(crate) fn invoke_plugin_command(
        &self,
        plugins: &PluginHost,
        plugin: &str,
        name: &str,
        args: &Value,
    ) -> oneshot::Receiver<Result<Value, String>> {
        let (tx, rx) = oneshot::channel();
        let id = {
            let mut shared = self.lock();
            shared.next_command_id += 1;
            let id = shared.next_command_id;
            shared.pending_commands.insert(
                id,
                PendingCommand {
                    plugin: plugin.to_owned(),
                    reply: tx,
                },
            );
            id
        };
        plugins.send_to(
            plugin,
            COMMAND,
            json!({ "id": id, "name": name, "args": args }),
        );
        rx
    }

    /// 主壳模式的状态发布：
    /// - Guest 插件只收到不含 `shell` 的共享状态，且仅在共享字段真正变化时广播；
    /// - 主壳 VM（`__shell__`）收到附加 `shell` 路由/布局快照的完整状态。
    pub fn publish_shell(
        &self,
        plugins: &PluginHost,
        rem: f32,
        safe_areas: Value,
        shell: Value,
        cx: &App,
    ) {
        let js_views = self.js_views_json();
        self.push_split(plugins, snapshot(rem, js_views, safe_areas, cx), shell);
    }

    fn js_views_json(&self) -> Value {
        let shared = self.lock();
        let mut map = serde_json::Map::with_capacity(shared.js_views.len());
        for (id, (plugin, view)) in &shared.js_views {
            map.insert(id.clone(), json!({ "plugin": plugin, "view": view }));
        }
        Value::Object(map)
    }

    fn push_split(&self, plugins: &PluginHost, mut plugin_next: Value, shell: Value) {
        let mut shell_next = plugin_next.clone();
        if let Some(fields) = shell_next.as_object_mut() {
            fields.insert("shell".into(), shell);
        }
        let (plugin_broadcast, shell_send) = {
            let mut shared = self.lock();
            plugin_next["revision"] = shared.plugin_revision.into();
            let plugin_broadcast = if plugin_next != shared.plugin_state {
                shared.plugin_revision += 1;
                plugin_next["revision"] = shared.plugin_revision.into();
                shared.plugin_state = plugin_next.clone();
                Some(plugin_next)
            } else {
                None
            };

            shell_next["revision"] = shared.shell_revision.into();
            let shell_send = if shell_next != shared.shell_state {
                shared.shell_revision += 1;
                shell_next["revision"] = shared.shell_revision.into();
                shared.shell_state = shell_next.clone();
                Some(shell_next)
            } else {
                None
            };
            (plugin_broadcast, shell_send)
        };
        if let Some(next) = shell_send {
            plugins.send_to_shell(STATE, next);
        }
        if let Some(next) = plugin_broadcast {
            plugins.broadcast(STATE, next);
        }
    }

    /// 转发离散的宿主事件给插件；`RuntimeTopic` 只推给登记过该话题的插件。
    pub fn forward_event(&self, plugins: &PluginHost, event: &HostEvent) {
        match event {
            HostEvent::RuntimeTopic {
                machine,
                topic,
                message,
            } => {
                let recipients: Vec<String> = self
                    .lock()
                    .topics
                    .iter()
                    .filter(|(_, wanted)| wanted.contains(topic))
                    .map(|(plugin, _)| plugin.clone())
                    .collect();
                if recipients.is_empty() {
                    return;
                }
                let payload = json!({
                    "type": "runtime_topic",
                    "machine": machine_value(machine),
                    "topic": topic,
                    "message": topic_message_json(message),
                });
                for plugin in recipients {
                    plugins.send_to(&plugin, EVENT, payload.clone());
                }
            }
            HostEvent::Plugin { name, payload } => {
                plugins.broadcast(
                    EVENT,
                    json!({
                        "type": "plugin",
                        "name": name.as_ref(),
                        "payload": payload,
                    }),
                );
            }
            HostEvent::WindowActivated => {
                plugins.broadcast(EVENT, json!({ "type": "window_activated" }));
            }
            HostEvent::ContextMenuItemsChanged => {
                plugins.broadcast(EVENT, json!({ "type": "context_menu_items_changed" }));
            }
            HostEvent::Reconnected(machine) => {
                plugins.broadcast(
                    EVENT,
                    json!({ "type": "reconnected", "machine": machine_value(machine) }),
                );
            }
            HostEvent::OpenPaths { machine, paths } => {
                plugins.broadcast(
                    EVENT,
                    json!({
                        "type": "open_paths",
                        "machine": machine_value(machine),
                        "paths": paths,
                    }),
                );
            }
            HostEvent::ProjectChanged { machine, root } => {
                plugins.broadcast(
                    EVENT,
                    json!({
                        "type": "project_changed",
                        "machine": machine_value(machine),
                        "root": root,
                    }),
                );
            }
            HostEvent::SessionsChanged => {
                plugins.broadcast(EVENT, json!({ "type": "sessions_changed" }));
            }
            HostEvent::MachinesChanged => {
                plugins.broadcast(EVENT, json!({ "type": "machines_changed" }));
            }
            HostEvent::ActiveContextChanged => {
                plugins.broadcast(EVENT, json!({ "type": "active_context_changed" }));
            }
            HostEvent::ViewsChanged => {
                plugins.broadcast(EVENT, json!({ "type": "views_changed" }));
            }
            HostEvent::ViewBadgesChanged => {
                plugins.broadcast(EVENT, json!({ "type": "view_badges_changed" }));
            }
            HostEvent::ShownViewsChanged => {
                plugins.broadcast(EVENT, json!({ "type": "shown_views_changed" }));
            }
            HostEvent::LaidOutViewsChanged => {
                plugins.broadcast(EVENT, json!({ "type": "laid_out_views_changed" }));
            }
            HostEvent::PagesChanged => {
                plugins.broadcast(EVENT, json!({ "type": "pages_changed" }));
            }
            HostEvent::LayoutsChanged => {
                plugins.broadcast(EVENT, json!({ "type": "layouts_changed" }));
            }
            HostEvent::CommandsChanged => {
                plugins.broadcast(EVENT, json!({ "type": "commands_changed" }));
            }
            HostEvent::LocaleChanged => {
                plugins.broadcast(EVENT, json!({ "type": "locale_changed" }));
            }
            HostEvent::SafeAreaChanged => {
                plugins.broadcast(EVENT, json!({ "type": "safe_area_changed" }));
            }
            HostEvent::ThemesChanged => {
                plugins.broadcast(EVENT, json!({ "type": "themes_changed" }));
            }
            HostEvent::OverlayChanged => {
                plugins.broadcast(EVENT, json!({ "type": "overlay_changed" }));
            }
            HostEvent::NotificationPermissionChanged => {
                plugins.broadcast(EVENT, json!({ "type": "notification_permission_changed" }));
            }
            _ => {}
        }
    }
}

/// 在 GPUI 主线程执行所有通用插件宿主动作。
pub(crate) fn handle_action(action: Action, bridge: &Bridge, plugins: &PluginHost, cx: &mut App) {
    match action {
        Action::Run {
            command,
            args,
            call,
        } => {
            let result = api::run(&command, args, cx);
            cx.spawn(async move |_| call.reply(result.await)).detach();
        }
        Action::Call {
            machine,
            method,
            params,
            timeout,
            detailed,
            call,
        } => {
            let client = api::host(cx).client(&machine, cx);
            cx.background_executor()
                .spawn(async move {
                    if detailed {
                        let response = match client {
                            Some(client) => match client.call_method(method, params, timeout) {
                                Ok(value) => json!({ "ok": true, "value": value }),
                                Err(error) => {
                                    let code = match &error {
                                        api::client::protocol::control::ControlClientError::Protocol(
                                            protocol,
                                        ) => serde_json::to_value(protocol.code)
                                            .ok()
                                            .unwrap_or(Value::Null),
                                        _ => Value::Null,
                                    };
                                    json!({
                                        "ok": false,
                                        "code": code,
                                        "message": api::call_error_text(error),
                                    })
                                }
                            },
                            None => json!({
                                "ok": false,
                                "code": Value::Null,
                                "message": api::machine_removed(),
                            }),
                        };
                        call.resolve(response);
                    } else {
                        let result = match client {
                            Some(client) => client
                                .call_method(method, params, timeout)
                                .map_err(api::call_error_text),
                            None => Err(api::machine_removed()),
                        };
                        call.reply(result);
                    }
                })
                .detach();
        }
        Action::Request {
            machine,
            payload,
            timeout,
            call,
        } => {
            let client = api::host(cx).client(&machine, cx);
            cx.background_executor()
                .spawn(async move {
                    let result = match client {
                        Some(client) => client.request_ok(&payload, timeout),
                        None => Err(api::machine_removed()),
                    };
                    call.reply(result);
                })
                .detach();
        }
        Action::ContextMenuItems {
            menu,
            machine,
            context,
            call,
        } => {
            let items: Vec<Value> = api::context_menu_items(&menu, &machine, cx)
                .iter()
                .map(|item| context_menu_json(item, &context, cx))
                .collect();
            call.resolve(Value::Array(items));
        }
        Action::ClipboardWrite { text, call } => {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            call.resolve(Value::Null);
        }
        Action::SetLocale { locale, call } => {
            let target: &str = match locale.as_deref() {
                Some(code) => code,
                None => api::system_locale(),
            };
            api::set_locale(target, cx);
            call.resolve(Value::Null);
        }
        Action::SetActiveContext { context, call } => {
            api::set_active_context(context, cx);
            call.resolve(Value::Null);
        }
        Action::SetStatusMenu { plugin, menu, call } => {
            api::set_status_menu(&plugin, menu, cx);
            call.resolve(Value::Null);
        }
        Action::SetTerminalFontPx { px, call } => {
            smelt_terminal::set_font_px(px);
            cx.refresh_windows();
            call.resolve(Value::Null);
        }
        Action::OpenFile { location, call } => {
            let task = api::open_file(location, cx);
            cx.spawn(async move |_| call.reply(task.await.map(|()| Value::Null)))
                .detach();
        }
        Action::OpenUrl { url, call } => {
            cx.open_url(&url);
            call.resolve(Value::Null);
        }
        Action::EmitEvent {
            name,
            payload,
            call,
        } => {
            api::emit(
                HostEvent::Plugin {
                    name: name.into(),
                    payload,
                },
                cx,
            );
            call.resolve(Value::Null);
        }
        Action::RegisterTopics {
            plugin,
            topics,
            call,
        } => match api::register_topics(&plugin, topics, cx) {
            Ok(()) => call.resolve(Value::Null),
            Err(error) => call.reject(format!("register_topics：{error}")),
        },
        Action::RegisterViews {
            plugin,
            views,
            call,
        } => match register_js_views(&plugin, &views, bridge, plugins, cx) {
            Ok(()) => call.resolve(Value::Null),
            Err(error) => call.reject(format!("register_views：{error}")),
        },
        Action::RegisterCommands {
            plugin,
            group_title,
            commands,
            call,
        } => match register_js_commands(
            &plugin,
            group_title.as_ref(),
            &commands,
            bridge,
            plugins,
            cx,
        ) {
            Ok(()) => call.resolve(Value::Null),
            Err(error) => call.reject(format!("register_commands：{error}")),
        },
        Action::RegisterLayouts {
            plugin,
            layouts,
            call,
        } => match register_js_layouts(&plugin, &layouts, cx) {
            Ok(()) => call.resolve(Value::Null),
            Err(error) => call.reject(format!("register_layouts：{error}")),
        },
        Action::RegisterPages {
            plugin,
            pages,
            call,
        } => match register_js_pages(&plugin, &pages, cx) {
            Ok(()) => call.resolve(Value::Null),
            Err(error) => call.reject(format!("register_pages：{error}")),
        },
        Action::RegisterContextMenuItems {
            plugin,
            menu,
            items,
            call,
        } => match register_js_context_menu_items(&plugin, &menu, &items, cx) {
            Ok(()) => call.resolve(Value::Null),
            Err(error) => call.reject(format!("register_context_menu_items：{error}")),
        },
        Action::RegisterMenuItems {
            plugin,
            items,
            call,
        } => match register_js_menu_items(&plugin, &items, cx) {
            Ok(()) => call.resolve(Value::Null),
            Err(error) => call.reject(format!("register_menu_items：{error}")),
        },
        Action::RegisterFileOpener { plugin, name, call } => {
            api::register_file_opener(&plugin, &name, cx);
            call.resolve(Value::Null);
        }
    }
}

/// 挂在 `smelt_plugin_api` 视图表里的轻量代理：真画面由主壳在同一 NSH 树里经 `<Slot>` 绘制，
/// 这里负责把宿主调的 `open(args)` 经 `smelt.event` 转给对应插件 VM。
struct NshViewProxy {
    plugin: String,
    view_id: String,
    plugins: PluginHost,
}

impl Render for NshViewProxy {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl PluginView for NshViewProxy {
    fn open(&mut self, args: &Value, _: &mut Window, _: &mut Context<Self>) {
        self.plugins.send_to(
            &self.plugin,
            EVENT,
            json!({
                "type": "view_opened",
                "view": self.view_id,
                "args": args,
            }),
        );
    }
}

fn parse_text(value: Option<&Value>, fallback: &str) -> Text {
    match value {
        Some(Value::String(s)) => Text::from(s.clone()),
        Some(Value::Object(map)) => {
            let entries: Vec<(SharedString, SharedString)> = map
                .iter()
                .filter_map(|(k, v)| {
                    Some((
                        SharedString::from(k.clone()),
                        SharedString::from(v.as_str()?.to_owned()),
                    ))
                })
                .collect();
            if entries.is_empty() {
                Text::from(fallback.to_owned())
            } else {
                Text::translations(entries)
            }
        }
        _ => Text::from(fallback.to_owned()),
    }
}

fn parse_settings_entry(value: Option<&Value>) -> Option<SettingsEntry> {
    let obj = value?.as_object()?;
    let group = match obj.get("group").and_then(Value::as_str) {
        Some("general") => SettingsGroup::General,
        Some("agents") => SettingsGroup::Agents,
        Some("system") => SettingsGroup::System,
        _ => SettingsGroup::Plugins,
    };
    let keywords = obj
        .get("keywords")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|s| SharedString::from(s.to_owned()))
                .collect()
        })
        .unwrap_or_default();
    Some(SettingsEntry { group, keywords })
}

fn register_js_views(
    plugin: &str,
    items: &[Value],
    bridge: &Bridge,
    plugins: &PluginHost,
    cx: &mut App,
) -> Result<(), String> {
    let mut specs = Vec::with_capacity(items.len());
    let mut js_map = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            return Err("视图缺 name".into());
        };
        let export_name = item
            .get("export")
            .and_then(Value::as_str)
            .unwrap_or(name)
            .to_owned();
        let full_id = format!("{plugin}/{name}");
        let title = parse_text(item.get("title"), name);
        let icon = item
            .get("icon")
            .and_then(Value::as_str)
            .unwrap_or("icons/puzzle.svg")
            .to_owned();
        let draws_top_bar = item
            .get("draws_top_bar")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let instances = item
            .get("instances")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let settings = parse_settings_entry(item.get("settings"));
        let plugin_owned = plugin.to_owned();
        let full_owned = full_id.clone();
        let host_clone = plugins.clone();
        specs.push(ViewSpec {
            name: name.to_owned().into(),
            title,
            icon: icon.into(),
            draws_top_bar,
            settings,
            instances,
            fills: Vec::new(),
            build: Rc::new(move |_window, cx| -> Rc<dyn ViewHandle> {
                let plugin = plugin_owned.clone();
                let view_id = full_owned.clone();
                let plugins = host_clone.clone();
                Rc::new(cx.new(|_| NshViewProxy {
                    plugin,
                    view_id,
                    plugins,
                }))
            }),
        });
        js_map.push((full_id, export_name));
    }
    bridge.set_js_views(plugin, js_map);
    api::register_views(plugin, specs, cx)
}

fn register_js_commands(
    plugin: &str,
    group_title: Option<&Value>,
    items: &[Value],
    bridge: &Bridge,
    plugins: &PluginHost,
    cx: &mut App,
) -> Result<(), String> {
    let group = parse_text(group_title, plugin);
    let mut specs = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            return Err("命令缺 name".into());
        };
        let title = parse_text(item.get("title"), name);
        let key = item.get("key").and_then(Value::as_str).map(str::to_owned);
        let context = item
            .get("context")
            .and_then(Value::as_str)
            .unwrap_or("Workspace")
            .to_owned();
        let requires_args = item
            .get("requires_args")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let palette = item
            .get("palette")
            .and_then(Value::as_bool)
            .unwrap_or(!requires_args);
        let cli = item.get("cli").and_then(Value::as_bool).unwrap_or(false);
        let args = item.get("args").and_then(Value::as_array).map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        });
        let when_view_shown = item
            .get("when_view_shown")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let when_active_session = item
            .get("when_active_session")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let bridge_clone = bridge.clone();
        let plugins_clone = plugins.clone();
        let plugin_owned = plugin.to_owned();
        let name_owned = name.to_owned();
        let handler: CommandHandler = Rc::new(move |call_args, _window, cx| {
            if let Some(view) = when_view_shown.as_deref()
                && !api::host(cx).view_shown(view, cx)
            {
                return Handled::Pass;
            }
            if when_active_session && api::active_context(cx).session.is_none() {
                return Handled::Pass;
            }
            let rx = bridge_clone.invoke_plugin_command(
                &plugins_clone,
                &plugin_owned,
                &name_owned,
                call_args,
            );
            Handled::Done(cx.spawn(async move |_| match rx.await {
                Ok(result) => result,
                Err(_) => Err("插件命令通道已关闭".into()),
            }))
        });
        specs.push(CommandSpec {
            name: name.to_owned(),
            title,
            key,
            context,
            palette,
            requires_args,
            args,
            cli,
            handler,
        });
    }
    api::register_commands(plugin, group, specs, cx)
}

fn parse_material(value: Option<&Value>) -> Option<Material> {
    match value.and_then(Value::as_str) {
        Some("sidebar") => Some(Material::Sidebar),
        Some("window") => Some(Material::Window),
        _ => None,
    }
}

fn register_js_layouts(plugin: &str, items: &[Value], cx: &mut App) -> Result<(), String> {
    struct OwnedLayout {
        page: String,
        name: String,
        title: Text,
        state: String,
        materials: LayoutMaterials,
    }
    let mut owned = Vec::with_capacity(items.len());
    for item in items {
        let Some(page) = item.get("page").and_then(Value::as_str) else {
            return Err("布局缺 page".into());
        };
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            return Err("布局缺 name".into());
        };
        let state = match item.get("state") {
            Some(Value::String(s)) => s.clone(),
            Some(other) if !other.is_null() => other.to_string(),
            _ => return Err("布局缺 state".into()),
        };
        let materials = item
            .get("materials")
            .and_then(Value::as_object)
            .map(|m| LayoutMaterials {
                left: parse_material(m.get("left")),
                right: parse_material(m.get("right")),
                bottom: parse_material(m.get("bottom")),
                center: parse_material(m.get("center")),
            })
            .unwrap_or_default();
        owned.push(OwnedLayout {
            page: page.to_owned(),
            name: name.to_owned(),
            title: parse_text(item.get("title"), name),
            state,
            materials,
        });
    }
    let sources: Vec<LayoutSource<'_>> = owned
        .iter()
        .map(|l| LayoutSource {
            page: &l.page,
            name: &l.name,
            title: l.title.clone(),
            state: &l.state,
            materials: l.materials,
        })
        .collect();
    api::register_layouts(plugin, &sources, cx)
}

fn register_js_pages(plugin: &str, items: &[Value], cx: &mut App) -> Result<(), String> {
    struct OwnedPage {
        name: String,
        title: Text,
        home: bool,
        index: Option<String>,
    }
    let mut owned = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            return Err("页面缺 name".into());
        };
        owned.push(OwnedPage {
            name: name.to_owned(),
            title: parse_text(item.get("title"), name),
            home: item.get("home").and_then(Value::as_bool).unwrap_or(false),
            index: item.get("index").and_then(Value::as_str).map(str::to_owned),
        });
    }
    let sources: Vec<PageSource<'_>> = owned
        .iter()
        .map(|p| PageSource {
            name: &p.name,
            title: p.title.clone(),
            home: p.home,
            index: p.index.as_deref(),
        })
        .collect();
    api::register_pages(plugin, &sources, cx)
}

fn parse_context_menu_item(item: &Value) -> Result<ContextMenuItem, String> {
    let label = parse_text(item.get("label"), "");
    let icon = item
        .get("icon")
        .and_then(Value::as_str)
        .map(|s| SharedString::from(s.to_owned()));
    let image = item
        .get("image")
        .and_then(Value::as_str)
        .map(|s| MenuImage::Asset(SharedString::from(s.to_owned())));
    let machine = match item.get("machine") {
        None => None,
        Some(value) => api::machine_from_json(value),
    };
    let action = if let Some(sub) = item.get("submenu").and_then(Value::as_array) {
        let children = sub
            .iter()
            .map(parse_context_menu_item)
            .collect::<Result<Vec<_>, _>>()?;
        ContextMenuAction::Submenu(children)
    } else if let Some(command) = item.get("command").and_then(Value::as_str) {
        ContextMenuAction::Run {
            command: command.to_owned(),
            args: item.get("args").cloned().unwrap_or(Value::Null),
        }
    } else {
        return Err("右键菜单项缺少 command 或 submenu".into());
    };
    Ok(ContextMenuItem {
        label,
        image,
        icon,
        machine,
        action,
    })
}

fn register_js_context_menu_items(
    plugin: &str,
    menu: &str,
    items: &[Value],
    cx: &mut App,
) -> Result<(), String> {
    let parsed = items
        .iter()
        .map(parse_context_menu_item)
        .collect::<Result<Vec<_>, _>>()?;
    api::register_context_menu_items(plugin, menu, parsed, cx)
}

fn register_js_menu_items(plugin: &str, items: &[Value], cx: &mut App) -> Result<(), String> {
    let mut entries = Vec::with_capacity(items.len());
    for item in items {
        let slot = match item.get("slot").and_then(Value::as_str) {
            Some("app") => MenuSlot::App,
            Some("help") => MenuSlot::Help,
            other => return Err(format!("菜单项 slot 不对：{other:?}")),
        };
        let Some(command) = item.get("command").and_then(Value::as_str) else {
            return Err("菜单项缺 command".into());
        };
        entries.push(MenuEntry {
            slot,
            title: parse_text(item.get("title"), command),
            command: SharedString::from(command.to_owned()),
        });
    }
    api::register_menu_items(plugin, entries, cx);
    Ok(())
}

fn send_action(sender: &mpsc::UnboundedSender<Action>, action: Action) {
    if let Err(unsent) = sender.send(action) {
        let call = match unsent.0 {
            Action::Run { call, .. }
            | Action::Call { call, .. }
            | Action::Request { call, .. }
            | Action::ContextMenuItems { call, .. }
            | Action::ClipboardWrite { call, .. }
            | Action::SetLocale { call, .. }
            | Action::SetActiveContext { call, .. }
            | Action::OpenFile { call, .. }
            | Action::OpenUrl { call, .. }
            | Action::EmitEvent { call, .. }
            | Action::RegisterTopics { call, .. }
            | Action::RegisterViews { call, .. }
            | Action::RegisterCommands { call, .. }
            | Action::RegisterLayouts { call, .. }
            | Action::RegisterPages { call, .. }
            | Action::RegisterContextMenuItems { call, .. }
            | Action::RegisterMenuItems { call, .. }
            | Action::RegisterFileOpener { call, .. }
            | Action::SetStatusMenu { call, .. }
            | Action::SetTerminalFontPx { call, .. } => call,
        };
        call.reject("面板已经关了");
    }
}

fn parse_machine(value: Option<&Value>) -> Option<RuntimeKey> {
    match value {
        None | Some(Value::Null) => Some(RuntimeKey::Local),
        Some(value) => api::machine_from_json(value),
    }
}

fn topic_message_json(message: &TopicMessage) -> Value {
    match message {
        TopicMessage::Event(payload) => json!({ "kind": "event", "payload": payload }),
        TopicMessage::Snapshot(payload) => json!({ "kind": "snapshot", "payload": payload }),
        TopicMessage::Lagged => json!({ "kind": "lagged" }),
        TopicMessage::Unavailable(reason) => json!({ "kind": "unavailable", "reason": reason }),
    }
}

/// 把注册的右键菜单项按 `smelt-plugin-api` 同构形状转成 JSON，点了直接 `run`。
pub(crate) fn context_menu_json(item: &ContextMenuItem, context: &Value, cx: &App) -> Value {
    let image = item.image.as_ref().map(|image| image.asset_path());
    match &item.action {
        ContextMenuAction::Run { command, args } => json!({
            "label": item.label.get().as_ref(),
            "icon": item.icon.as_deref(),
            "image": image,
            "command": command,
            "args": api::context_menu_args(command, args, context, cx),
        }),
        ContextMenuAction::Submenu(items) => json!({
            "label": item.label.get().as_ref(),
            "icon": item.icon.as_deref(),
            "image": image,
            "submenu": items
                .iter()
                .map(|sub| context_menu_json(sub, context, cx))
                .collect::<Vec<_>>(),
        }),
    }
}

/// 状态会跟着这些事件变。
pub(crate) fn affects_state(event: &HostEvent) -> bool {
    matches!(
        event,
        HostEvent::WindowActivated
            | HostEvent::SessionsChanged
            | HostEvent::MachinesChanged
            | HostEvent::ActiveContextChanged
            | HostEvent::ViewsChanged
            | HostEvent::LaidOutViewsChanged
            | HostEvent::ShownViewsChanged
            | HostEvent::ViewBadgesChanged
            | HostEvent::PagesChanged
            | HostEvent::LayoutsChanged
            | HostEvent::CommandsChanged
            | HostEvent::LocaleChanged
            | HostEvent::SafeAreaChanged
            | HostEvent::ThemesChanged
            | HostEvent::NotificationPermissionChanged
    )
}

fn snapshot(rem: f32, js_views: Value, safe_areas: Value, cx: &App) -> Value {
    let host = api::host(cx);
    let machines: Vec<Value> = host
        .machines(cx)
        .iter()
        .map(|machine| {
            let directory = host.workspace_directory(&machine.key, cx);
            let states = host.session_states(&machine.key, cx);
            let style = host.path_style(&machine.key, cx);
            let path_style = match style {
                PathStyle::Posix => "posix",
                PathStyle::Windows => "windows",
            };
            let facts: Vec<Value> = directory
                .iter()
                .flat_map(|directory| {
                    directory.conversations.iter().map(|conversation| {
                        let status = match conversation.status(&states) {
                            AgentStatus::NeedsYou => "needs_you",
                            AgentStatus::Running => "running",
                            AgentStatus::Idle => "idle",
                        };
                        let agent = api::conversation_agent(conversation, &states).map(|agent| {
                            json!({
                                "id": agent.id(),
                                "label": agent.label(),
                                "icon": agent.icon_asset(),
                            })
                        });
                        let placement = match api::placement(style, directory, conversation) {
                            Placement::Project { root, workset } => json!({
                                "kind": "project",
                                "root": root,
                                "workset": workset,
                            }),
                            Placement::Conversations => json!({ "kind": "conversations" }),
                            Placement::Hidden => json!({ "kind": "hidden" }),
                        };
                        json!({
                            "id": conversation.id,
                            "name": api::conversation_name(conversation, &states),
                            "status": status,
                            "agent": agent,
                            "automation": api::is_automation_run(conversation),
                            "cwd": api::conversation_cwd(conversation),
                            "placement": placement,
                        })
                    })
                })
                .collect();
            json!({
                "machine": machine_value(&machine.key),
                "name": machine.name,
                "online": host.machine_online(&machine.key, cx),
                "unavailable": machine.unavailable,
                "path_style": path_style,
                "directory": directory,
                "states": states,
                "facts": facts,
            })
        })
        .collect();
    let connections = host.connections(cx);
    let active = api::active_context(cx);
    let project = active
        .project
        .map(|project| json!({ "machine": machine_value(&project.machine), "root": project.root }));
    let session = active.session.map(|session| {
        json!({
            "machine": machine_value(&session.machine),
            "id": session.id,
            "at_latest": session.at_latest,
        })
    });
    let pages: Vec<Value> = api::pages(cx)
        .iter()
        .map(|page| {
            json!({
                "id": page.id.to_string(),
                "title": page.title.get().to_string(),
                "home": page.home,
                "index": page.index.as_deref(),
            })
        })
        .collect();
    let views: Vec<Value> = api::views(cx)
        .iter()
        .map(|view| {
            let settings = view.settings.as_ref().map(|entry| {
                let group = match entry.group {
                    SettingsGroup::General => "general",
                    SettingsGroup::Agents => "agents",
                    SettingsGroup::System => "system",
                    SettingsGroup::Plugins => "plugins",
                };
                json!({
                    "group": group,
                    "keywords": entry.keywords.iter().map(|k| k.as_ref()).collect::<Vec<_>>(),
                })
            });
            json!({
                "id": view.id.to_string(),
                "title": view.title.get().to_string(),
                "icon": view.icon.to_string(),
                "draws_top_bar": view.draws_top_bar,
                "settings": settings,
                "instances": view.instances,
                "badge": api::view_badge(&view.id, cx).map(|badge| badge.to_string()),
                "laid_out": host.view_laid_out(&view.id, cx),
                "shown": host.view_shown(&view.id, cx),
            })
        })
        .collect();
    let commands: Vec<Value> = api::commands(cx)
        .iter()
        .map(|command| {
            json!({
                "id": command.id.to_string(),
                "title": command.title.get().to_string(),
                "key": command.key.as_deref(),
            })
        })
        .collect();
    let locales: Vec<Value> = api::LOCALES
        .iter()
        .map(|(code, name)| json!({ "code": code, "name": name }))
        .collect();
    let notification_permission = match host.notification_permission(cx) {
        api::NotificationPermission::Checking => json!({ "kind": "checking" }),
        api::NotificationPermission::NotRequested => json!({ "kind": "not_requested" }),
        api::NotificationPermission::Granted {
            banners,
            sound,
            badge,
        } => json!({
            "kind": "granted",
            "banners": banners,
            "sound": sound,
            "badge": badge,
        }),
        api::NotificationPermission::Denied => json!({ "kind": "denied" }),
        api::NotificationPermission::Unavailable(reason) => json!({
            "kind": "unavailable",
            "reason": reason,
        }),
        api::NotificationPermission::Unsupported => json!({ "kind": "unsupported" }),
    };
    json!({
        "locale": api::locale().to_string(),
        "system_locale": api::system_locale(),
        "locales": locales,
        "window_active": host.window_active(cx),
        "notification_permission": notification_permission,
        "machines": machines,
        "connections": connections,
        "active_context": { "project": project, "session": session },
        "pages": pages,
        "views": views,
        "commands": commands,
        "js_views": js_views,
        "safe_areas": safe_areas,
        "bar_insets": {},
        "theme": theme(rem),
    })
}

fn machine_value(machine: &RuntimeKey) -> Value {
    api::machine_to_json(machine)
}

fn rgba_css(color: gpui::Rgba) -> String {
    let r = (color.r.clamp(0., 1.) * 255.).round() as u8;
    let g = (color.g.clamp(0., 1.) * 255.).round() as u8;
    let b = (color.b.clamp(0., 1.) * 255.).round() as u8;
    format!("rgba({r}, {g}, {b}, {:.3})", color.a.clamp(0., 1.))
}

/// 插件自己画时要的几档色、字、圆角；字按这扇窗口的 rem 换成点。
fn theme(rem: f32) -> Value {
    let p = ui_theme::palette();
    let text = |style: TextStyle| {
        json!({
            "size": style.size / 16. * rem,
            "line_height": style.line_height / 16. * rem,
            "weight": style.weight.0,
        })
    };
    json!({
        "rem": rem,
        "palette": {
            "bg": api::hex_color(p.bg),
            "side": api::hex_color(p.side),
            "surface": api::hex_color(p.surface),
            "soft": api::hex_color(p.soft),
            "soft_hover": api::hex_color(p.soft_hover),
            "text": api::hex_color(p.text),
            "muted": api::hex_color(p.muted),
            "muted_selected": api::hex_color(p.muted_selected),
            "faint": api::hex_color(p.faint),
            "faint_selected": api::hex_color(p.faint_selected),
            "ink": api::hex_color(p.ink),
            "on_ink": api::hex_color(p.on_ink),
            "link": api::hex_color(p.link),
            "run": api::hex_color(p.run),
            "need": api::hex_color(p.need),
            "ok": api::hex_color(p.ok),
            "bad": api::hex_color(p.bad),
            "on_bad": api::hex_color(p.on_bad),
            "hair": rgba_css(p.hair),
            "hair_strong": rgba_css(p.hair_strong),
            "row_hover": rgba_css(p.row_hover),
            "row_selected": rgba_css(p.row_selected),
            "focus": rgba_css(p.focus),
            "button_primary": [
                rgba_css(p.button_primary[0]),
                rgba_css(p.button_primary[1]),
                rgba_css(p.button_primary[2]),
            ],
            "button_primary_fg": api::hex_color(p.button_primary_foreground),
            "button_secondary": [
                rgba_css(p.button_secondary[0]),
                rgba_css(p.button_secondary[1]),
                rgba_css(p.button_secondary[2]),
            ],
            "button_secondary_fg": api::hex_color(p.button_secondary_foreground),
            "button_submit": [
                rgba_css(p.button_submit[0]),
                rgba_css(p.button_submit[1]),
                rgba_css(p.button_submit[2]),
            ],
            "button_submit_fg": api::hex_color(p.button_submit_foreground),
        },
        "fonts": {
            "mono": font_config::font_family(),
        },
        "type": {
            "ui": text(TYPE.ui),
            "ui_medium": text(TYPE.ui_medium),
            "small": text(TYPE.small),
            "small_medium": text(TYPE.small_medium),
            "text_xsmall": text(TYPE.text_xsmall),
            "text_xsmall_medium": text(TYPE.text_xsmall_medium),
        },
        "radius": {
            "sm": RADIUS.sm,
            "md": RADIUS.md,
            "lg": RADIUS.lg,
            "row": f32::from(ui_theme::row_radius()),
        },
    })
}

/// 在本机系统文件管理器里定位一个文件或目录（macOS 选中该条目，其它平台打开所在目录）。
fn reveal_local_path(path: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn();
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(parent) = PathStyle::LOCAL.parent(path) {
            let _ = std::process::Command::new(if cfg!(windows) {
                "explorer"
            } else {
                "xdg-open"
            })
            .arg(parent)
            .spawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_split_isolates_shell_payload_from_guest_plugins() {
        let bridge = Bridge::default();
        let plugins = PluginHost::new();

        bridge.push_split(
            &plugins,
            json!({ "locale": "zh-CN", "machines": [] }),
            json!({ "pages": [{ "key": "home", "layout_rev": 1 }] }),
        );
        {
            let shared = bridge.lock();
            assert_eq!(shared.plugin_revision, 1);
            assert_eq!(shared.shell_revision, 1);
            assert!(
                shared.plugin_state.get("shell").is_none(),
                "Guest 插件状态不应包含仅主壳使用的 shell 布局树"
            );
            assert_eq!(shared.shell_state["shell"]["pages"][0]["layout_rev"], 1);
        }

        // 仅主壳布局/出口变化时，只递增 shell_revision，不递增也不广播 Guest 状态。
        bridge.push_split(
            &plugins,
            json!({ "locale": "zh-CN", "machines": [] }),
            json!({ "pages": [{ "key": "home", "layout_rev": 2 }] }),
        );
        {
            let shared = bridge.lock();
            assert_eq!(shared.plugin_revision, 1);
            assert_eq!(shared.shell_revision, 2);
            assert_eq!(shared.shell_state["shell"]["pages"][0]["layout_rev"], 2);
        }

        // 共享字段变化时，双方都更新各自快照。
        bridge.push_split(
            &plugins,
            json!({ "locale": "en", "machines": [] }),
            json!({ "pages": [{ "key": "home", "layout_rev": 2 }] }),
        );
        {
            let shared = bridge.lock();
            assert_eq!(shared.plugin_revision, 2);
            assert_eq!(shared.shell_revision, 3);
            assert_eq!(shared.plugin_state["locale"], "en");
            assert_eq!(shared.shell_state["locale"], "en");
        }
    }
}
