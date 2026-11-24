//! 给外壳和所有独立插件 VM 的通用宿主桥：一份只读通用状态（宿主变了广播整份）、离散宿主事件转发
//! （`smelt.event`）、双向命令派发（`smelt.command` / `command.reply`），以及通用宿主调用
//! （`state`、`run`、`call`、`request`、`context_menu_items`、`clipboard.write`、`fs.path_kind`、
//! `fs.reveal_path`、`sys.process_usage`、`os.discover_apps`、`os.launch_path`、`set_active_context`、`open_file`、`open_url`、`register_views`、
//! `register_commands`、`register_topics`、`register_layouts`、`register_pages`、
//! `register_context_menu_items`、`register_menu_items`、`register_file_opener`）。
//!
//! 只给通用事实与能力，不认识任何具体插件。
//! 插件的 `host.call` 经各自的 `HostChannel` 在引擎线程上直达，携带不可伪造的 `plugin_id`：
//! `state` 与 `command.reply` 当场在引擎线程处理；`fs.path_kind` 在后台线程做不碰主线程；
//! 需要 GPUI `&App` 的调用封装为 [`Action`] 转主线程执行。

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::{BitOr, BitOrAssign},
    rc::Rc,
    sync::{Arc, Mutex, MutexGuard, RwLock},
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
/// 定向推给订阅方插件的 ACP 对话增量事件（不全局广播）。
const ACP_EVENT: &str = "smelt.acp.event";

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
    AcpSubscribe {
        plugin: String,
        sub_id: String,
        machine: RuntimeKey,
        reuse_key: Option<String>,
        launch: smelt_acp::ConversationClientLaunch,
        call: HostCall,
    },
    AcpHistory {
        machine: RuntimeKey,
        session_id: String,
        before: usize,
        limit: usize,
        call: HostCall,
    },
    AcpRestart {
        machine: RuntimeKey,
        session_id: String,
        call: HostCall,
    },
    AcpClipboardImages {
        call: HostCall,
    },
}

struct PendingCommand {
    plugin: String,
    reply: oneshot::Sender<Result<Value, String>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SliceMask(u32);

impl SliceMask {
    pub(crate) const EMPTY: Self = Self(0);
    pub(crate) const LOCALE: Self = Self(1 << 0);
    pub(crate) const SYSTEM_LOCALE: Self = Self(1 << 1);
    pub(crate) const LOCALES: Self = Self(1 << 2);
    pub(crate) const WINDOW_ACTIVE: Self = Self(1 << 3);
    pub(crate) const NOTIFICATION_PERMISSION: Self = Self(1 << 4);
    pub(crate) const MACHINES_META: Self = Self(1 << 5);
    pub(crate) const SESSIONS_DATA: Self = Self(1 << 6);
    pub(crate) const CONNECTIONS: Self = Self(1 << 7);
    pub(crate) const ACTIVE_CONTEXT: Self = Self(1 << 8);
    pub(crate) const PAGES: Self = Self(1 << 9);
    pub(crate) const VIEWS: Self = Self(1 << 10);
    pub(crate) const COMMANDS: Self = Self(1 << 11);
    pub(crate) const JS_VIEWS: Self = Self(1 << 12);
    pub(crate) const SAFE_AREAS: Self = Self(1 << 13);
    pub(crate) const THEME: Self = Self(1 << 14);
    pub(crate) const SHELL: Self = Self(1 << 15);
    pub(crate) const ALL: Self = Self((1 << 16) - 1);

    pub(crate) const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub(crate) const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub(crate) const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }
}

impl BitOr for SliceMask {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for SliceMask {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

const SHELL_KEYS: &[&str] = &[
    "locale",
    "system_locale",
    "locales",
    "pages",
    "views",
    "js_views",
    "safe_areas",
    "bar_insets",
    "theme",
    "shell",
];

#[derive(Default)]
struct StateSub {
    all: bool,
    slices: BTreeSet<String>,
}

#[derive(Default)]
struct EventSub {
    all: bool,
    types: BTreeSet<String>,
}

#[derive(Default)]
struct Shared {
    /// 推给 Guest 插件的状态快照（不含仅主壳消费的 `shell` 布局树）。
    plugin_state: BTreeMap<String, Arc<Value>>,
    plugin_revision: u64,
    /// 推给主壳 VM（`__shell__`）的状态快照（只包含主壳消费的布局与视觉切片）。
    shell_state: BTreeMap<String, Arc<Value>>,
    shell_revision: u64,
    /// 插件 id → 它 `locales/` 里的文案（语言 → 键 → 字）。
    texts: BTreeMap<String, Arc<Value>>,
    /// 插件 id → 它登记订阅的运行时话题集合。
    topics: BTreeMap<String, BTreeSet<String>>,
    /// 插件 id → 按需订阅的状态顶层切片集合。
    state_subs: BTreeMap<String, StateSub>,
    /// 插件 id → 按需订阅的离散宿主事件类型集合。
    event_subs: BTreeMap<String, EventSub>,
    /// JS 插件登记的视图全称 `<plugin>/<name>` → `(plugin_id, export_name)`，供主壳 `<HostView>` 区分 JS/Rust。
    js_views: BTreeMap<String, (String, String)>,
    /// 发往插件 VM 等待 `command.reply` 的在途命令。
    next_command_id: u64,
    pending_commands: BTreeMap<u64, PendingCommand>,
    /// ACP 活体订阅中心与图片字节缓存（在 `serve` 中绑定定向推送 `PluginHost`）。
    acp_hub: Option<smelt_acp::AcpHub>,
}

struct CachedMachineSessions {
    revisions: (u64, u64),
    path_style: PathStyle,
    entry: Arc<Value>,
}

#[derive(Default)]
struct SessionsDataCache {
    initialized: bool,
    machines: BTreeMap<RuntimeKey, CachedMachineSessions>,
    last_keys: Vec<(RuntimeKey, (u64, u64), PathStyle)>,
}

#[derive(Default)]
struct BridgeInner {
    shared: Mutex<Shared>,
    clients: RwLock<BTreeMap<RuntimeKey, api::client::RuntimeClient>>,
    sessions_cache: Mutex<SessionsDataCache>,
}

#[derive(Clone, Default)]
pub(crate) struct Bridge(Arc<BridgeInner>);

impl Bridge {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.0.shared.lock().expect("host bridge lock poisoned")
    }

    pub fn sync_clients(&self, cx: &App) {
        let host = api::host(cx);
        let mut next = BTreeMap::new();
        for machine in host.machines(cx) {
            if let Some(client) = host.client(&machine.key, cx) {
                next.insert(machine.key.clone(), client);
            }
        }
        if !next.contains_key(&RuntimeKey::Local)
            && let Some(local) = host.client(&RuntimeKey::Local, cx)
        {
            next.insert(RuntimeKey::Local, local);
        }
        *self
            .0
            .clients
            .write()
            .expect("host bridge clients lock poisoned") = next;
    }

    fn cached_client(&self, machine: &RuntimeKey) -> Option<api::client::RuntimeClient> {
        self.0
            .clients
            .read()
            .expect("host bridge clients lock poisoned")
            .get(machine)
            .cloned()
    }

    /// 把通用宿主调用登记到 [`PluginHost`] 上。在装载插件之前调一次。
    pub fn serve(&self, plugins: &PluginHost) -> mpsc::UnboundedReceiver<Action> {
        let (actions, received) = mpsc::unbounded_channel();

        let acp_hub = {
            let sink_plugins = plugins.clone();
            let sink: smelt_acp::AcpEventSink = Arc::new(move |owner, payload| {
                sink_plugins.send_to(owner, ACP_EVENT, payload);
            });
            let hub = smelt_acp::AcpHub::new(smelt_acp::ImageStore::new(), sink);
            self.lock().acp_hub = Some(hub.clone());
            hub
        };

        {
            let bridge = self.clone();
            let hub = acp_hub.clone();
            let actions = actions.clone();
            plugins.handle("acp.subscribe", move |plugin, mut call| {
                let mut data = call.take_data();
                let Some(sub_id) = data["sub_id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                else {
                    return call.reject("acp.subscribe：缺 sub_id 字符串");
                };
                let Some(machine) = parse_machine(data.get("machine")) else {
                    return call.reject("acp.subscribe：machine 要是 null 或字符串");
                };
                let reuse_key = data
                    .get("reuse_key")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                let launch: smelt_acp::ConversationClientLaunch =
                    match serde_json::from_value(data["launch"].take()) {
                        Ok(launch) => launch,
                        Err(error) => {
                            return call.reject(format!("acp.subscribe：launch 参数不对：{error}"));
                        }
                    };
                // 缓存命中时直接在引擎线程挂载（内部仅 spawn IO 线程，不阻塞），
                // 免去切 GPUI 主线程一跳并保持与 acp.unsubscribe 严格 FIFO（分屏复用不丢 reuse_key）。
                if let Some(client) = bridge.cached_client(&machine) {
                    hub.subscribe(
                        client,
                        smelt_acp::SubscribeRequest {
                            sub_id,
                            owner: plugin.to_owned(),
                            machine,
                            reuse_key,
                            launch,
                        },
                    );
                    call.resolve(Value::Null);
                } else {
                    send_action(
                        &actions,
                        Action::AcpSubscribe {
                            plugin: plugin.to_owned(),
                            sub_id,
                            machine,
                            reuse_key,
                            launch,
                            call,
                        },
                    );
                }
            });
        }

        {
            let hub = acp_hub.clone();
            plugins.handle("acp.unsubscribe", move |plugin, call| {
                let data = call.data();
                let Some(sub_id) = data["sub_id"].as_str() else {
                    return call.reject("acp.unsubscribe：缺 sub_id 字符串");
                };
                let reuse_key = data
                    .get("reuse_key")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                hub.unsubscribe(plugin, sub_id, reuse_key);
                call.resolve(Value::Null);
            });
        }

        {
            let hub = acp_hub.clone();
            plugins.handle("acp.action", move |plugin, mut call| {
                let mut data = call.take_data();
                let Some(sub_id) = data["sub_id"].as_str().map(str::to_owned) else {
                    return call.reject("acp.action：缺 sub_id 字符串");
                };
                let action_val = data["action"].take();
                let image_refs: Vec<String> = data
                    .get("image_refs")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                if image_refs.is_empty() {
                    match hub.send_action(plugin, &sub_id, action_val, &image_refs) {
                        Ok(()) => call.resolve(Value::Null),
                        Err(error) => call.reject(error),
                    }
                } else {
                    let hub = hub.clone();
                    let owner = plugin.to_owned();
                    api::blocking::spawn_detached(move || {
                        match hub.send_action(&owner, &sub_id, action_val, &image_refs) {
                            Ok(()) => call.resolve(Value::Null),
                            Err(error) => call.reject(error),
                        }
                    });
                }
            });
        }

        {
            let bridge = self.clone();
            let images = acp_hub.images().clone();
            let actions = actions.clone();
            plugins.handle("acp.history", move |_plugin, call| {
                let data = call.data();
                let Some(machine) = parse_machine(data.get("machine")) else {
                    return call.reject("acp.history：machine 要是 null 或字符串");
                };
                let Some(session_id) = data["session_id"].as_str().map(str::to_owned) else {
                    return call.reject("acp.history：缺 session_id 字符串");
                };
                let Some(before) = data["before"].as_u64() else {
                    return call.reject("acp.history：缺 before 非负整数");
                };
                let limit = data
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(smelt_acp::ACP_HISTORY_PAGE_LIMIT as u64)
                    as usize;
                if let Some(client) = bridge.cached_client(&machine) {
                    let images = images.clone();
                    api::blocking::spawn_detached(move || {
                        execute_acp_history(
                            Some(client),
                            Some(images),
                            &session_id,
                            before as usize,
                            limit,
                            call,
                        );
                    });
                } else {
                    send_action(
                        &actions,
                        Action::AcpHistory {
                            machine,
                            session_id,
                            before: before as usize,
                            limit,
                            call,
                        },
                    );
                }
            });
        }

        {
            let bridge = self.clone();
            let actions = actions.clone();
            plugins.handle("acp.restart", move |_plugin, call| {
                let data = call.data();
                let Some(machine) = parse_machine(data.get("machine")) else {
                    return call.reject("acp.restart：machine 要是 null 或字符串");
                };
                let Some(session_id) = data["session_id"].as_str().map(str::to_owned) else {
                    return call.reject("acp.restart：缺 session_id 字符串");
                };
                if let Some(client) = bridge.cached_client(&machine) {
                    api::blocking::spawn_detached(move || {
                        execute_acp_restart(Some(client), &session_id, call);
                    });
                } else {
                    send_action(
                        &actions,
                        Action::AcpRestart {
                            machine,
                            session_id,
                            call,
                        },
                    );
                }
            });
        }

        {
            let images = acp_hub.images().clone();
            plugins.handle("acp.paste_image", move |_plugin, call| {
                let data = call.data();
                let data_b64 = data
                    .get("data_base64")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let mime = data.get("mime").and_then(Value::as_str).map(str::to_owned);
                let path = data.get("path").and_then(Value::as_str).map(str::to_owned);
                let images = images.clone();
                api::blocking::spawn_detached(move || {
                    let result = if let Some(b64) = data_b64 {
                        images.store_base64(mime.as_deref().unwrap_or("image/png"), &b64)
                    } else if let Some(raw_path) = path {
                        let fs_path = raw_path
                            .strip_prefix("file://")
                            .map(std::path::PathBuf::from)
                            .unwrap_or_else(|| std::path::PathBuf::from(raw_path));
                        images.store_file(&fs_path)
                    } else {
                        Err("acp.paste_image：需要 data_base64 或 path".into())
                    };
                    match result
                        .and_then(|meta| serde_json::to_value(meta).map_err(|e| e.to_string()))
                    {
                        Ok(value) => call.resolve(value),
                        Err(error) => call.reject(error),
                    }
                });
            });
        }

        {
            let actions = actions.clone();
            plugins.handle("acp.clipboard_images", move |_plugin, call| {
                send_action(&actions, Action::AcpClipboardImages { call });
            });
        }

        {
            let images = acp_hub.images().clone();
            plugins.handle("acp.image_uri", move |_plugin, call| {
                let Some(ref_id) = call.data()["ref_id"].as_str().map(str::to_owned) else {
                    return call.reject("acp.image_uri：缺 ref_id 字符串");
                };
                let images = images.clone();
                api::blocking::spawn_detached(move || match images.get_data_uri(&ref_id) {
                    Some(uri) => call.resolve(Value::String(uri)),
                    None => call.resolve(Value::Null),
                });
            });
        }

        plugins.handle("mermaid.render", move |_plugin, call| {
            let Some(source) = call.data()["source"].as_str().map(str::to_owned) else {
                return call.reject("mermaid.render：缺 source 字符串");
            };
            api::blocking::spawn_detached(move || {
                match smelt_ui::markdown_mermaid::render_mermaid_svg_data_uri(&source) {
                    Ok(rendered) => call.resolve(json!({
                        "ok": true,
                        "data_uri": rendered.data_uri,
                        "width": rendered.width,
                        "height": rendered.height,
                    })),
                    Err(error) => call.resolve(json!({
                        "ok": false,
                        "error": error,
                    })),
                }
            });
        });

        let bridge = self.clone();
        let state_plugins = plugins.clone();
        plugins.handle("state", move |plugin, call| {
            let requested: Option<Vec<String>> = call
                .data()
                .get("slices")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                });
            let is_shell = plugin == crate::plugins::SHELL_OWNER;
            let is_builtins = plugin == crate::plugins::BUILTINS_VM_ID;
            let members = if is_builtins {
                state_plugins.members_of(plugin)
            } else {
                Vec::new()
            };
            let (rev, entries, single_text, member_texts) = {
                let mut shared = bridge.lock();
                if !is_shell && let Some(keys) = &requested {
                    let sub = shared.state_subs.entry(plugin.to_owned()).or_default();
                    for key in keys {
                        sub.slices.insert(key.clone());
                    }
                }
                let source = if is_shell {
                    &shared.shell_state
                } else {
                    &shared.plugin_state
                };
                let entries: Vec<(String, Arc<Value>)> = match &requested {
                    Some(keys) => keys
                        .iter()
                        .filter_map(|key| source.get(key).map(|val| (key.clone(), Arc::clone(val))))
                        .collect(),
                    None => source
                        .iter()
                        .filter(|(key, _)| key.as_str() != "sessions_data")
                        .map(|(key, val)| (key.clone(), Arc::clone(val)))
                        .collect(),
                };
                let (single_text, member_texts) = if is_builtins {
                    let member_texts: Vec<(String, Option<Arc<Value>>)> = members
                        .into_iter()
                        .map(|member| {
                            let table = shared.texts.get(&member).map(Arc::clone);
                            (member, table)
                        })
                        .collect();
                    (None, Some(member_texts))
                } else {
                    (shared.texts.get(plugin).map(Arc::clone), None)
                };
                let rev = if is_shell {
                    shared.shell_revision
                } else {
                    shared.plugin_revision
                };
                (rev, entries, single_text, member_texts)
            };
            let mut out = serde_json::Map::with_capacity(entries.len() + 3);
            for (key, val) in entries {
                out.insert(key, val.as_ref().clone());
            }
            let texts = single_text
                .map(|table| table.as_ref().clone())
                .unwrap_or_else(|| json!({}));
            out.insert("revision".into(), Value::from(rev));
            out.insert("texts".into(), texts);
            if let Some(member_texts) = member_texts {
                let mut by_plugin = serde_json::Map::with_capacity(member_texts.len());
                for (member, table) in member_texts {
                    by_plugin.insert(
                        member,
                        table
                            .map(|t| t.as_ref().clone())
                            .unwrap_or_else(|| json!({})),
                    );
                }
                out.insert("texts_by_plugin".into(), Value::Object(by_plugin));
            }
            call.resolve(Value::Object(out));
        });

        {
            let bridge = self.clone();
            plugins.handle("subscribe", move |plugin, call| {
                let res = bridge.apply_subscribe(plugin, call.data());
                call.resolve(res);
            });
        }

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
            let bridge = self.clone();
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
                if let Some(client) = bridge.cached_client(&machine) {
                    api::blocking::spawn_detached(move || {
                        execute_call_rpc(Some(client), method, params, timeout, detailed, call);
                    });
                } else {
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
                }
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

        plugins.handle("fs.path_kind", move |_plugin, call| {
            let Some(path) = call.data()["path"].as_str().map(str::to_owned) else {
                return call.reject("fs.path_kind：缺 path 字符串");
            };
            api::blocking::spawn_detached(move || {
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
            api::blocking::spawn_detached(move || {
                reveal_local_path(&path);
                call.resolve(Value::Null);
            });
        });

        {
            let sampler = Arc::new(Mutex::new(None::<api::process_usage::ProcessSampler>));
            plugins.handle("sys.process_usage", move |_plugin, call| {
                let sampler = sampler.clone();
                api::blocking::spawn_detached(move || {
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
            api::blocking::spawn_detached(move || {
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
            api::blocking::spawn_detached(move || {
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
            let bridge = self.clone();
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
                if let Some(client) = bridge.cached_client(&machine) {
                    api::blocking::spawn_detached(move || {
                        execute_request_rpc(Some(client), payload, timeout, call);
                    });
                } else {
                    send_action(
                        &actions,
                        Action::Request {
                            machine,
                            payload,
                            timeout,
                            call,
                        },
                    );
                }
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
        self.lock().texts.insert(plugin.to_owned(), Arc::new(texts));
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

    pub fn forget_vm(&self, vm_id: &str) {
        let acp_hub = {
            let mut shared = self.lock();
            shared.state_subs.remove(vm_id);
            shared.event_subs.remove(vm_id);
            shared.acp_hub.clone()
        };
        if let Some(hub) = acp_hub {
            hub.remove_owner(vm_id);
        }
    }

    pub fn forget_plugin(&self, plugin: &str) {
        let (acp_hub, orphaned): (
            Option<smelt_acp::AcpHub>,
            Vec<oneshot::Sender<Result<Value, String>>>,
        ) = {
            let mut shared = self.lock();
            shared.texts.remove(plugin);
            shared.topics.remove(plugin);
            shared.state_subs.remove(plugin);
            shared.event_subs.remove(plugin);
            shared.js_views.retain(|_, (owner, _)| owner != plugin);
            let ids: Vec<u64> = shared
                .pending_commands
                .iter()
                .filter(|(_, item)| item.plugin == plugin)
                .map(|(id, _)| *id)
                .collect();
            let orphaned = ids
                .into_iter()
                .filter_map(|id| shared.pending_commands.remove(&id).map(|item| item.reply))
                .collect();
            (shared.acp_hub.clone(), orphaned)
        };
        if let Some(hub) = acp_hub {
            hub.remove_owner(plugin);
        }
        for reply in orphaned {
            let _ = reply.send(Err(format!("插件 {plugin} 已卸载")));
        }
    }

    pub(crate) fn acp_hub(&self) -> Option<smelt_acp::AcpHub> {
        self.lock().acp_hub.clone()
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
        // JS 插件无法登记退出守卫；`"smelt.updater"` 的退出守卫由宿主侧注册，热重载/覆写时保留。
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
            json!({ "id": id, "plugin": plugin, "name": name, "args": args }),
        );
        rx
    }

    /// 登记插件按需订阅的状态切片与事件类型，并返回新增切片的补齐快照（锁外拼 JSON）。
    fn apply_subscribe(&self, plugin: &str, data: &Value) -> Value {
        let (rev, patch_entries) = {
            let mut shared = self.lock();
            if plugin == crate::plugins::SHELL_OWNER {
                return json!({
                    "revision": shared.shell_revision,
                    "patch": {},
                });
            }

            let mut added_keys = BTreeSet::new();
            let all_slices = data
                .get("all_slices")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            {
                let Shared {
                    state_subs,
                    plugin_state,
                    ..
                } = &mut *shared;
                let sub = state_subs.entry(plugin.to_owned()).or_default();
                if all_slices && !sub.all {
                    for key in plugin_state.keys() {
                        if key != "revision" && !sub.slices.contains(key) {
                            added_keys.insert(key.clone());
                        }
                    }
                    sub.all = true;
                }
                if let Some(items) = data.get("add_slices").and_then(Value::as_array) {
                    for key in items.iter().filter_map(Value::as_str) {
                        if !sub.all && sub.slices.insert(key.to_owned()) {
                            added_keys.insert(key.to_owned());
                        }
                    }
                }
            }

            let has_ev_all = data.get("events_all").is_some();
            let has_ev_types = data.get("event_types").is_some();
            if has_ev_all || has_ev_types {
                let ev = shared.event_subs.entry(plugin.to_owned()).or_default();
                if let Some(all) = data.get("events_all").and_then(Value::as_bool) {
                    ev.all = all;
                }
                if let Some(types) = data.get("event_types").and_then(Value::as_array) {
                    ev.types = types
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect();
                }
            }

            let patch_entries: Vec<(String, Arc<Value>)> = added_keys
                .into_iter()
                .filter_map(|key| {
                    shared
                        .plugin_state
                        .get(&key)
                        .map(|val| (key, Arc::clone(val)))
                })
                .collect();
            (shared.plugin_revision, patch_entries)
        };

        let mut patch = serde_json::Map::with_capacity(patch_entries.len());
        for (key, val) in patch_entries {
            patch.insert(key, val.as_ref().clone());
        }
        json!({
            "revision": rev,
            "patch": Value::Object(patch),
        })
    }

    fn js_views_json(&self) -> Value {
        let entries: Vec<(String, (String, String))> = self
            .lock()
            .js_views
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut map = serde_json::Map::with_capacity(entries.len());
        for (id, (plugin, view)) in entries {
            map.insert(id, json!({ "plugin": plugin, "view": view }));
        }
        Value::Object(map)
    }

    pub(crate) fn views_of(&self, plugin: &str) -> Vec<String> {
        self.lock()
            .js_views
            .iter()
            .filter(|(_, (owner, _))| owner == plugin)
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn snapshot_sessions_data_cached(&self, cx: &App) -> Option<Value> {
        let host = api::host(cx);
        let machines = host.machines(cx);
        let mut meta = Vec::with_capacity(machines.len());
        for machine in &machines {
            let revs = host.sessions_revision(&machine.key, cx);
            let style = host.path_style(&machine.key, cx);
            meta.push((&machine.key, revs, style));
        }
        if meta.iter().all(|(_, revs, _)| revs.is_some()) {
            let keys: Vec<(RuntimeKey, (u64, u64), PathStyle)> = meta
                .iter()
                .map(|(k, r, s)| ((*k).clone(), r.expect("checked"), *s))
                .collect();
            let mut cache = self
                .0
                .sessions_cache
                .lock()
                .expect("sessions cache lock poisoned");
            if cache.initialized && cache.last_keys == keys {
                return None;
            }
            let mut entries = Vec::with_capacity(keys.len());
            let mut next_machines = BTreeMap::new();
            for (key, revs, style) in &keys {
                let entry = match cache.machines.get(key) {
                    Some(cached) if cached.revisions == *revs && cached.path_style == *style => {
                        Arc::clone(&cached.entry)
                    }
                    _ => Arc::new(build_machine_sessions_json(&*host, key, *style, cx)),
                };
                entries.push(entry.as_ref().clone());
                next_machines.insert(
                    key.clone(),
                    CachedMachineSessions {
                        revisions: *revs,
                        path_style: *style,
                        entry,
                    },
                );
            }
            cache.initialized = true;
            cache.last_keys = keys;
            cache.machines = next_machines;
            return Some(Value::Array(entries));
        }

        let entries = meta
            .into_iter()
            .map(|(key, _, style)| build_machine_sessions_json(&*host, key, style, cx))
            .collect();
        Some(Value::Array(entries))
    }

    /// 按 `dirty` 掩码增量计算发生变化的宿主状态切片，只把真正变了的切片推给订阅它的插件。
    pub fn publish_dirty(
        &self,
        plugins: &PluginHost,
        dirty: SliceMask,
        rem: f32,
        safe_areas: Option<Value>,
        shell: Option<Value>,
        cx: &App,
    ) {
        if dirty.is_empty() && shell.is_none() && safe_areas.is_none() {
            return;
        }
        let slow = smelt_ui::frame_stats::slow_frame_threshold();
        let started = slow.map(|_| std::time::Instant::now());
        let js_views = dirty
            .intersects(SliceMask::JS_VIEWS)
            .then(|| self.js_views_json());
        let sessions_data = dirty
            .contains(SliceMask::SESSIONS_DATA)
            .then(|| self.snapshot_sessions_data_cached(cx))
            .flatten();
        let updates = snapshot_slices(dirty, rem, js_views, safe_areas, sessions_data, cx);
        let (shell_send, deliveries) = self.apply_delta_split(updates, shell, false);
        if let Some(next) = shell_send {
            plugins.send_to_shell(STATE, next);
        }
        for (plugin, delta) in deliveries {
            plugins.send_to(&plugin, STATE, delta);
        }
        if let (Some(threshold), Some(started)) = (slow, started) {
            let elapsed = started.elapsed();
            if elapsed >= threshold {
                eprintln!(
                    "[smelt-nsh slow publish] dirty={dirty:?} total={:.2}ms",
                    elapsed.as_secs_f64() * 1000.0
                );
            }
        }
    }

    fn apply_delta_split(
        &self,
        mut updates: serde_json::Map<String, Value>,
        shell: Option<Value>,
        replace_plugin_map: bool,
    ) -> (Option<Value>, Vec<(String, Value)>) {
        updates.remove("revision");

        // 1. 短锁只拷贝现有切片的 Arc 指针，比较与包装放到锁外做，不卡 NSH 引擎线程。
        let (prev_plugin, prev_shell_entries, prev_shell_val) = {
            let shared = self.lock();
            let prev_plugin: BTreeMap<String, Arc<Value>> = updates
                .keys()
                .filter_map(|k| {
                    shared
                        .plugin_state
                        .get(k)
                        .map(|v| (k.clone(), Arc::clone(v)))
                })
                .collect();
            let prev_shell_entries: BTreeMap<&'static str, Arc<Value>> = SHELL_KEYS
                .iter()
                .filter(|&&k| k != "shell" && updates.contains_key(k))
                .filter_map(|&k| shared.shell_state.get(k).map(|v| (k, Arc::clone(v))))
                .collect();
            let prev_shell_val = shell
                .is_some()
                .then(|| shared.shell_state.get("shell").map(Arc::clone))
                .flatten();
            (prev_plugin, prev_shell_entries, prev_shell_val)
        };

        // 2. 锁外比对差异并把变化了的切片包装成 Arc<Value>。
        let update_keys: BTreeSet<String> = updates.keys().cloned().collect();
        let mut changed_plugin_keys = BTreeSet::new();
        let mut changed_shell_keys: BTreeSet<&'static str> = BTreeSet::new();
        for (key, next_val) in &updates {
            if prev_plugin
                .get(key)
                .is_none_or(|prev| prev.as_ref() != next_val)
            {
                changed_plugin_keys.insert(key.clone());
            }
        }
        for &key in SHELL_KEYS {
            if key == "shell" {
                continue;
            }
            if let Some(next_val) = updates.get(key)
                && prev_shell_entries
                    .get(key)
                    .is_none_or(|prev| prev.as_ref() != next_val)
            {
                changed_shell_keys.insert(key);
            }
        }
        let shell_arc = shell.and_then(|shell_val| {
            prev_shell_val
                .as_deref()
                .is_none_or(|prev| prev != &shell_val)
                .then(|| Arc::new(shell_val))
        });

        let mut wrapped_updates: BTreeMap<String, Arc<Value>> = BTreeMap::new();
        for (key, val) in updates {
            if changed_plugin_keys.contains(&key) || changed_shell_keys.contains(key.as_str()) {
                wrapped_updates.insert(key, Arc::new(val));
            }
        }

        // 3. 短锁仅写入变化了的 Arc 指针、递增版本号并抽取投递清单。
        let (delivery_recipes, shell_recipe) = {
            let mut shared = self.lock();
            if replace_plugin_map {
                let removed: Vec<String> = shared
                    .plugin_state
                    .keys()
                    .filter(|key| !update_keys.contains(key.as_str()))
                    .cloned()
                    .collect();
                for key in removed {
                    shared.plugin_state.remove(&key);
                    changed_plugin_keys.insert(key);
                }
            }
            for key in &changed_plugin_keys {
                if let Some(arc_val) = wrapped_updates.get(key) {
                    shared.plugin_state.insert(key.clone(), Arc::clone(arc_val));
                }
            }

            let delivery_recipes: Vec<(String, u64, Vec<(String, Option<Arc<Value>>)>)> =
                if changed_plugin_keys.is_empty() {
                    Vec::new()
                } else {
                    shared.plugin_revision += 1;
                    let rev = shared.plugin_revision;
                    shared
                        .state_subs
                        .iter()
                        .filter_map(|(plugin, sub)| {
                            let mut entries = Vec::new();
                            for key in &changed_plugin_keys {
                                if sub.all || sub.slices.contains(key) {
                                    entries
                                        .push((key.clone(), shared.plugin_state.get(key).cloned()));
                                }
                            }
                            (!entries.is_empty()).then_some((plugin.clone(), rev, entries))
                        })
                        .collect()
                };

            if let Some(arc_shell) = shell_arc {
                shared.shell_state.insert("shell".to_owned(), arc_shell);
                changed_shell_keys.insert("shell");
            }
            for &key in &changed_shell_keys {
                if key != "shell"
                    && let Some(arc_val) = wrapped_updates.get(key)
                {
                    shared
                        .shell_state
                        .insert(key.to_owned(), Arc::clone(arc_val));
                }
            }

            let shell_recipe = if changed_shell_keys.is_empty() {
                None
            } else {
                shared.shell_revision += 1;
                let srev = shared.shell_revision;
                let entries: Vec<(&'static str, Arc<Value>)> = changed_shell_keys
                    .iter()
                    .filter_map(|&key| shared.shell_state.get(key).map(|v| (key, Arc::clone(v))))
                    .collect();
                Some((srev, entries))
            };
            (delivery_recipes, shell_recipe)
        };

        // 4. 锁外组装每个订阅方的增量 JSON 对象。
        let deliveries = delivery_recipes
            .into_iter()
            .map(|(plugin, rev, entries)| {
                let mut patch = serde_json::Map::with_capacity(entries.len() + 1);
                patch.insert("revision".into(), Value::from(rev));
                for (key, val) in entries {
                    patch.insert(key, val.map(|v| v.as_ref().clone()).unwrap_or(Value::Null));
                }
                (plugin, Value::Object(patch))
            })
            .collect();

        let shell_send = shell_recipe.map(|(srev, entries)| {
            let mut patch = serde_json::Map::with_capacity(entries.len() + 1);
            patch.insert("revision".into(), Value::from(srev));
            for (key, val) in entries {
                patch.insert(key.to_owned(), val.as_ref().clone());
            }
            Value::Object(patch)
        });

        (shell_send, deliveries)
    }

    #[cfg(test)]
    fn compute_push_split(
        &self,
        plugin_next: Value,
        shell: Value,
    ) -> (Option<Value>, Vec<(String, Value)>) {
        let updates = match plugin_next {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        self.apply_delta_split(updates, Some(shell), true)
    }

    #[cfg(test)]
    fn push_split(&self, plugins: &PluginHost, plugin_next: Value, shell: Value) {
        let (shell_send, deliveries) = self.compute_push_split(plugin_next, shell);
        if let Some(next) = shell_send {
            plugins.send_to_shell(STATE, next);
        }
        for (plugin, delta) in deliveries {
            plugins.send_to(&plugin, STATE, delta);
        }
    }

    fn event_recipients(&self, event_type: &str) -> Vec<String> {
        self.lock()
            .event_subs
            .iter()
            .filter(|(_, sub)| sub.all || sub.types.contains(event_type))
            .map(|(plugin, _)| plugin.clone())
            .collect()
    }

    fn plugin_event_recipients(&self, name: &str) -> Vec<String> {
        let exact = format!("plugin:{name}");
        self.lock()
            .event_subs
            .iter()
            .filter(|(_, sub)| {
                sub.all || sub.types.contains("plugin") || sub.types.contains(&exact)
            })
            .map(|(plugin, _)| plugin.clone())
            .collect()
    }

    /// 转发离散的宿主事件给插件：`RuntimeTopic` 按 `topics` 单播，`plugin` 按精确事件名与类型过滤下发。
    pub fn forward_event(&self, plugins: &PluginHost, event: &HostEvent) {
        let (event_type, payload) = match event {
            HostEvent::RuntimeTopic {
                machine,
                topic,
                message,
            } => {
                let recipients: Vec<String> = {
                    let shared = self.lock();
                    let mut vms = BTreeSet::new();
                    for (plugin, wanted) in &shared.topics {
                        if !wanted.contains(topic) {
                            continue;
                        }
                        let vm = plugins.vm_of(plugin);
                        if shared
                            .event_subs
                            .get(&vm)
                            .is_some_and(|sub| sub.all || sub.types.contains("runtime_topic"))
                        {
                            vms.insert(vm);
                        }
                    }
                    vms.into_iter().collect()
                };
                if recipients.is_empty() {
                    return;
                }
                let payload = json!({
                    "type": "runtime_topic",
                    "machine": machine_value(machine),
                    "topic": topic,
                    "message": topic_message_json(message),
                });
                for vm in recipients {
                    plugins.send_to(&vm, EVENT, payload.clone());
                }
                return;
            }
            HostEvent::Plugin { name, payload } => {
                let recipients = self.plugin_event_recipients(name.as_ref());
                if recipients.is_empty() {
                    return;
                }
                let body = json!({
                    "type": "plugin",
                    "name": name.as_ref(),
                    "payload": payload,
                });
                for plugin in recipients {
                    plugins.send_to(&plugin, EVENT, body.clone());
                }
                return;
            }
            HostEvent::WindowActivated => {
                ("window_activated", json!({ "type": "window_activated" }))
            }
            HostEvent::ContextMenuItemsChanged => (
                "context_menu_items_changed",
                json!({ "type": "context_menu_items_changed" }),
            ),
            HostEvent::Reconnected(machine) => (
                "reconnected",
                json!({ "type": "reconnected", "machine": machine_value(machine) }),
            ),
            HostEvent::OpenPaths { machine, paths } => (
                "open_paths",
                json!({
                    "type": "open_paths",
                    "machine": machine_value(machine),
                    "paths": paths,
                }),
            ),
            HostEvent::ProjectChanged { machine, root } => (
                "project_changed",
                json!({
                    "type": "project_changed",
                    "machine": machine_value(machine),
                    "root": root,
                }),
            ),
            HostEvent::SessionsChanged => {
                ("sessions_changed", json!({ "type": "sessions_changed" }))
            }
            HostEvent::MachinesChanged => {
                ("machines_changed", json!({ "type": "machines_changed" }))
            }
            HostEvent::ActiveContextChanged => (
                "active_context_changed",
                json!({ "type": "active_context_changed" }),
            ),
            HostEvent::ViewsChanged => ("views_changed", json!({ "type": "views_changed" })),
            HostEvent::ViewBadgesChanged => (
                "view_badges_changed",
                json!({ "type": "view_badges_changed" }),
            ),
            HostEvent::ShownViewsChanged => (
                "shown_views_changed",
                json!({ "type": "shown_views_changed" }),
            ),
            HostEvent::LaidOutViewsChanged => (
                "laid_out_views_changed",
                json!({ "type": "laid_out_views_changed" }),
            ),
            HostEvent::PagesChanged => ("pages_changed", json!({ "type": "pages_changed" })),
            HostEvent::LayoutsChanged => ("layouts_changed", json!({ "type": "layouts_changed" })),
            HostEvent::CommandsChanged => {
                ("commands_changed", json!({ "type": "commands_changed" }))
            }
            HostEvent::LocaleChanged => ("locale_changed", json!({ "type": "locale_changed" })),
            HostEvent::SafeAreaChanged => {
                ("safe_area_changed", json!({ "type": "safe_area_changed" }))
            }
            HostEvent::ThemesChanged => ("themes_changed", json!({ "type": "themes_changed" })),
            HostEvent::OverlayChanged => ("overlay_changed", json!({ "type": "overlay_changed" })),
            HostEvent::NotificationPermissionChanged => (
                "notification_permission_changed",
                json!({ "type": "notification_permission_changed" }),
            ),
            _ => return,
        };
        for plugin in self.event_recipients(event_type) {
            plugins.send_to(&plugin, EVENT, payload.clone());
        }
    }
}

fn execute_call_rpc(
    client: Option<api::client::RuntimeClient>,
    method: ControlMethod,
    params: Value,
    timeout: Duration,
    detailed: bool,
    call: HostCall,
) {
    if detailed {
        let response = match client {
            Some(client) => match client.call_method(method, params, timeout) {
                Ok(value) => json!({ "ok": true, "value": value }),
                Err(error) => {
                    let code = match &error {
                        api::client::protocol::control::ControlClientError::Protocol(protocol) => {
                            serde_json::to_value(protocol.code)
                                .ok()
                                .unwrap_or(Value::Null)
                        }
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
}

fn execute_request_rpc(
    client: Option<api::client::RuntimeClient>,
    payload: Value,
    timeout: Duration,
    call: HostCall,
) {
    let result = match client {
        Some(client) => client.request_ok(&payload, timeout),
        None => Err(api::machine_removed()),
    };
    call.reply(result);
}

fn execute_acp_history(
    client: Option<api::client::RuntimeClient>,
    images: Option<smelt_acp::ImageStore>,
    session_id: &str,
    before: usize,
    limit: usize,
    call: HostCall,
) {
    let Some(client) = client else {
        call.reject(api::machine_removed());
        return;
    };
    match smelt_acp::load_acp_history(&client, session_id, before, limit) {
        Ok(snapshot) => match serde_json::to_value(&snapshot) {
            Ok(mut value) => {
                if let Some(images) = images {
                    images.compact_snapshot_json(&mut value);
                }
                call.resolve(value);
            }
            Err(error) => call.reject(error.to_string()),
        },
        Err(error) => call.reject(format!("{}: {error}", error.code())),
    }
}

fn execute_acp_restart(
    client: Option<api::client::RuntimeClient>,
    session_id: &str,
    call: HostCall,
) {
    let Some(client) = client else {
        call.reject(api::machine_removed());
        return;
    };
    match smelt_acp::restart_acp_session(&client, session_id) {
        Ok(()) => call.resolve(Value::Null),
        Err(error) => call.reject(format!("{}: {error}", error.code())),
    }
}

/// 在 GPUI 主线程执行无法在引擎线程直接完成的宿主动作（需要 `&App` 或冷缓存兜底）。
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
            api::blocking::spawn_detached(move || {
                execute_call_rpc(client, method, params, timeout, detailed, call);
            });
        }
        Action::Request {
            machine,
            payload,
            timeout,
            call,
        } => {
            let client = api::host(cx).client(&machine, cx);
            api::blocking::spawn_detached(move || {
                execute_request_rpc(client, payload, timeout, call);
            });
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
        Action::SetActiveContext { context, call } => {
            api::set_active_context(context, cx);
            call.resolve(Value::Null);
        }
        Action::SetStatusMenu { plugin, menu, call } => {
            api::set_status_menu(&plugin, menu, cx);
            call.resolve(Value::Null);
        }
        Action::AcpSubscribe {
            plugin,
            sub_id,
            machine,
            reuse_key,
            launch,
            call,
        } => {
            let Some(hub) = bridge.acp_hub() else {
                call.reject("acp.subscribe：ACP hub 未初始化");
                return;
            };
            let Some(client) = api::host(cx).client(&machine, cx) else {
                call.reject(api::machine_removed());
                return;
            };
            hub.subscribe(
                client,
                smelt_acp::SubscribeRequest {
                    sub_id,
                    owner: plugin,
                    machine,
                    reuse_key,
                    launch,
                },
            );
            call.resolve(Value::Null);
        }
        Action::AcpHistory {
            machine,
            session_id,
            before,
            limit,
            call,
        } => {
            let client = api::host(cx).client(&machine, cx);
            let images = bridge.acp_hub().map(|h| h.images().clone());
            api::blocking::spawn_detached(move || {
                execute_acp_history(client, images, &session_id, before, limit, call);
            });
        }
        Action::AcpRestart {
            machine,
            session_id,
            call,
        } => {
            let client = api::host(cx).client(&machine, cx);
            api::blocking::spawn_detached(move || {
                execute_acp_restart(client, &session_id, call);
            });
        }
        Action::AcpClipboardImages { call } => {
            let Some(images_store) = bridge.acp_hub().map(|h| h.images().clone()) else {
                call.reject("acp.clipboard_images：ACP hub 未初始化");
                return;
            };
            let mut raw_images: Vec<Vec<u8>> = Vec::new();
            let mut paths: Vec<std::path::PathBuf> = Vec::new();
            if let Some(item) = cx.read_from_clipboard() {
                for entry in item.into_entries() {
                    match entry {
                        gpui::ClipboardEntry::Image(img) => raw_images.push(img.bytes),
                        gpui::ClipboardEntry::ExternalPaths(ext) => {
                            paths.extend(ext.paths().iter().cloned());
                        }
                        _ => {}
                    }
                }
            }
            api::blocking::spawn_detached(move || {
                let mut stored = Vec::new();
                let mut non_image_paths = Vec::new();
                for bytes in raw_images {
                    if let Ok(meta) = images_store.store_bytes(bytes, None) {
                        stored.push(meta);
                    }
                }
                for path in paths {
                    let is_img = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                        matches!(
                            e.to_ascii_lowercase().as_str(),
                            "png" | "jpg" | "jpeg" | "gif" | "webp"
                        )
                    });
                    if is_img {
                        if let Ok(meta) = images_store.store_file(&path) {
                            stored.push(meta);
                        }
                    } else {
                        non_image_paths.push(path.to_string_lossy().into_owned());
                    }
                }
                call.resolve(json!({
                    "images": stored,
                    "paths": non_image_paths,
                }));
            });
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
                "__target_plugin": self.plugin,
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
            | Action::AcpSubscribe { call, .. }
            | Action::AcpHistory { call, .. }
            | Action::AcpRestart { call, .. }
            | Action::AcpClipboardImages { call, .. } => call,
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

/// 按离散宿主事件精确映射受影响的顶层状态切片，避免每次变化全量重算所有切片。
pub(crate) fn dirty_slices_for_event(event: &HostEvent) -> SliceMask {
    match event {
        HostEvent::SessionsChanged => SliceMask::SESSIONS_DATA,
        HostEvent::MachinesChanged => {
            SliceMask::MACHINES_META | SliceMask::SESSIONS_DATA | SliceMask::CONNECTIONS
        }
        HostEvent::ActiveContextChanged => SliceMask::ACTIVE_CONTEXT,
        HostEvent::WindowActivated => SliceMask::WINDOW_ACTIVE,
        HostEvent::NotificationPermissionChanged => SliceMask::NOTIFICATION_PERMISSION,
        HostEvent::ViewsChanged
        | HostEvent::LaidOutViewsChanged
        | HostEvent::ShownViewsChanged
        | HostEvent::ViewBadgesChanged => {
            SliceMask::VIEWS | SliceMask::SHELL | SliceMask::SAFE_AREAS
        }
        HostEvent::PagesChanged => SliceMask::PAGES | SliceMask::SHELL,
        HostEvent::LayoutsChanged => SliceMask::SHELL | SliceMask::SAFE_AREAS,
        HostEvent::SafeAreaChanged => SliceMask::SAFE_AREAS,
        HostEvent::CommandsChanged => SliceMask::COMMANDS,
        HostEvent::LocaleChanged => {
            SliceMask::LOCALE
                | SliceMask::SYSTEM_LOCALE
                | SliceMask::PAGES
                | SliceMask::VIEWS
                | SliceMask::COMMANDS
                | SliceMask::SHELL
        }
        HostEvent::ThemesChanged => SliceMask::THEME,
        _ => SliceMask::EMPTY,
    }
}

fn snapshot_machines_meta(cx: &App) -> Value {
    let host = api::host(cx);
    let machines: Vec<Value> = host
        .machines(cx)
        .iter()
        .map(|machine| {
            let path_style = match host.path_style(&machine.key, cx) {
                PathStyle::Posix => "posix",
                PathStyle::Windows => "windows",
            };
            json!({
                "machine": machine_value(&machine.key),
                "name": machine.name,
                "online": host.machine_online(&machine.key, cx),
                "unavailable": machine.unavailable,
                "path_style": path_style,
            })
        })
        .collect();
    Value::Array(machines)
}

fn build_machine_sessions_json(
    host: &dyn api::Host,
    machine: &RuntimeKey,
    style: PathStyle,
    cx: &App,
) -> Value {
    let directory = host.workspace_directory(machine, cx);
    let states = host.session_states(machine, cx);
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
        "machine": machine_value(machine),
        "directory": directory,
        "states": states,
        "facts": facts,
    })
}

fn snapshot_active_context(cx: &App) -> Value {
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
    json!({ "project": project, "session": session })
}

fn snapshot_pages(cx: &App) -> Value {
    Value::Array(
        api::pages(cx)
            .iter()
            .map(|page| {
                json!({
                    "id": page.id.to_string(),
                    "title": page.title.get().to_string(),
                    "home": page.home,
                    "index": page.index.as_deref(),
                })
            })
            .collect(),
    )
}

fn snapshot_views(cx: &App) -> Value {
    let host = api::host(cx);
    Value::Array(
        api::views(cx)
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
            .collect(),
    )
}

fn snapshot_commands(cx: &App) -> Value {
    Value::Array(
        api::commands(cx)
            .iter()
            .map(|command| {
                json!({
                    "id": command.id.to_string(),
                    "title": command.title.get().to_string(),
                    "key": command.key.as_deref(),
                })
            })
            .collect(),
    )
}

fn snapshot_locales() -> Value {
    Value::Array(
        api::LOCALES
            .iter()
            .map(|(code, name)| json!({ "code": code, "name": name }))
            .collect(),
    )
}

fn snapshot_notification_permission(cx: &App) -> Value {
    match api::host(cx).notification_permission(cx) {
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
    }
}

fn snapshot_slices(
    mask: SliceMask,
    rem: f32,
    js_views: Option<Value>,
    safe_areas: Option<Value>,
    sessions_data: Option<Value>,
    cx: &App,
) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    if mask.contains(SliceMask::LOCALE) {
        out.insert("locale".into(), Value::String(api::locale().to_string()));
    }
    if mask.contains(SliceMask::SYSTEM_LOCALE) {
        out.insert(
            "system_locale".into(),
            Value::String(api::system_locale().to_owned()),
        );
    }
    if mask.contains(SliceMask::LOCALES) {
        out.insert("locales".into(), snapshot_locales());
    }
    if mask.contains(SliceMask::WINDOW_ACTIVE) {
        out.insert(
            "window_active".into(),
            Value::Bool(api::host(cx).window_active(cx)),
        );
    }
    if mask.contains(SliceMask::NOTIFICATION_PERMISSION) {
        out.insert(
            "notification_permission".into(),
            snapshot_notification_permission(cx),
        );
    }
    if mask.contains(SliceMask::MACHINES_META) {
        out.insert("machines_meta".into(), snapshot_machines_meta(cx));
    }
    if let Some(sessions_data) = sessions_data {
        out.insert("sessions_data".into(), sessions_data);
    }
    if mask.contains(SliceMask::CONNECTIONS) {
        out.insert(
            "connections".into(),
            serde_json::to_value(api::host(cx).connections(cx)).unwrap_or(Value::Null),
        );
    }
    if mask.contains(SliceMask::ACTIVE_CONTEXT) {
        out.insert("active_context".into(), snapshot_active_context(cx));
    }
    if mask.contains(SliceMask::PAGES) {
        out.insert("pages".into(), snapshot_pages(cx));
    }
    if mask.contains(SliceMask::VIEWS) {
        out.insert("views".into(), snapshot_views(cx));
    }
    if mask.contains(SliceMask::COMMANDS) {
        out.insert("commands".into(), snapshot_commands(cx));
    }
    if mask.contains(SliceMask::JS_VIEWS)
        && let Some(js_views) = js_views
    {
        out.insert("js_views".into(), js_views);
    }
    if mask.contains(SliceMask::SAFE_AREAS) {
        if let Some(safe_areas) = safe_areas {
            out.insert("safe_areas".into(), safe_areas);
        }
        out.insert("bar_insets".into(), json!({}));
    }
    if mask.contains(SliceMask::THEME) {
        out.insert("theme".into(), theme(rem));
    }
    out
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
            json!({ "locale": "zh-CN", "machines_meta": [], "sessions_data": [] }),
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
            assert!(
                shared.shell_state.get("sessions_data").is_none(),
                "主壳快照不应携带重量级 sessions_data"
            );
            assert_eq!(shared.shell_state["shell"]["pages"][0]["layout_rev"], 1);
        }

        // 仅主壳布局/出口变化时，只递增 shell_revision，不递增也不广播 Guest 状态。
        bridge.push_split(
            &plugins,
            json!({ "locale": "zh-CN", "machines_meta": [], "sessions_data": [] }),
            json!({ "pages": [{ "key": "home", "layout_rev": 2 }] }),
        );
        {
            let shared = bridge.lock();
            assert_eq!(shared.plugin_revision, 1);
            assert_eq!(shared.shell_revision, 2);
            assert_eq!(shared.shell_state["shell"]["pages"][0]["layout_rev"], 2);
        }

        // 仅 sessions_data 变化时，只递增 plugin_revision，不打扰主壳 shell_revision。
        bridge.push_split(
            &plugins,
            json!({
                "locale": "zh-CN",
                "machines_meta": [],
                "sessions_data": [{ "machine": null, "states": [{ "id": "s1" }] }],
            }),
            json!({ "pages": [{ "key": "home", "layout_rev": 2 }] }),
        );
        {
            let shared = bridge.lock();
            assert_eq!(shared.plugin_revision, 2);
            assert_eq!(shared.shell_revision, 2);
        }

        // 共享字段变化时，双方都更新各自快照。
        bridge.push_split(
            &plugins,
            json!({
                "locale": "en",
                "machines_meta": [],
                "sessions_data": [{ "machine": null, "states": [{ "id": "s1" }] }],
            }),
            json!({ "pages": [{ "key": "home", "layout_rev": 2 }] }),
        );
        {
            let shared = bridge.lock();
            assert_eq!(shared.plugin_revision, 3);
            assert_eq!(shared.shell_revision, 3);
            assert_eq!(shared.plugin_state["locale"].as_str(), Some("en"));
            assert_eq!(shared.shell_state["locale"].as_str(), Some("en"));
        }
    }

    #[test]
    fn subscribe_delivers_minimal_deltas_catchup_patches_and_filtered_events() {
        let bridge = Bridge::default();

        // 1. 先有初始状态快照（revision = 1），未订阅前不产生增量投递。
        let (_, d0) = bridge.compute_push_split(
            json!({
                "locale": "en",
                "machines_meta": [{ "machine": "m1", "online": true }],
                "sessions_data": [{ "machine": "m1", "states": [{ "id": "s1" }] }],
                "theme": { "rem": 16 },
            }),
            json!({}),
        );
        assert!(d0.is_empty());

        // 2. 插件 A 仅订阅 locale；插件 B 订阅 machines_meta 并精准过滤事件名与 plugin:<name>。
        let sub_a = bridge.apply_subscribe("plug.a", &json!({ "add_slices": ["locale"] }));
        assert_eq!(sub_a["revision"], 1);
        assert_eq!(sub_a["patch"], json!({ "locale": "en" }));

        let sub_b = bridge.apply_subscribe(
            "plug.b",
            &json!({
                "add_slices": ["machines_meta"],
                "events_all": false,
                "event_types": ["themes_changed", "reconnected", "plugin:foo/bar"],
            }),
        );
        assert_eq!(
            sub_b["patch"],
            json!({ "machines_meta": [{ "machine": "m1", "online": true }] })
        );

        // 3. 只有 sessions_data 变化：既没订 sessions_data 的 plug.a / plug.b 都不被唤醒。
        let (_, d1) = bridge.compute_push_split(
            json!({
                "locale": "en",
                "machines_meta": [{ "machine": "m1", "online": true }],
                "sessions_data": [{ "machine": "m1", "states": [{ "id": "s2" }] }],
                "theme": { "rem": 16 },
            }),
            json!({}),
        );
        assert!(d1.is_empty());

        // 4. machines_meta 变化：只给 plug.b 推仅含 machines_meta + revision 的最小 delta。
        let (_, d2) = bridge.compute_push_split(
            json!({
                "locale": "en",
                "machines_meta": [
                    { "machine": "m1", "online": true },
                    { "machine": "m2", "online": false }
                ],
                "sessions_data": [{ "machine": "m1", "states": [{ "id": "s2" }] }],
                "theme": { "rem": 16 },
            }),
            json!({}),
        );
        assert_eq!(d2.len(), 1);
        assert_eq!(d2[0].0, "plug.b");
        assert_eq!(
            d2[0].1,
            json!({
                "revision": 3,
                "machines_meta": [
                    { "machine": "m1", "online": true },
                    { "machine": "m2", "online": false }
                ],
            })
        );

        // 5. plug.a 后续懒订阅 theme：立即返回当前值补齐，不影响已订阅的 locale。
        let catchup_a = bridge.apply_subscribe("plug.a", &json!({ "add_slices": ["theme"] }));
        assert_eq!(catchup_a["revision"], 3);
        assert_eq!(catchup_a["patch"], json!({ "theme": { "rem": 16 } }));

        // 6. 事件精确过滤：未订阅的 plugin 事件名不命中 plug.b。
        assert!(bridge.event_recipients("window_activated").is_empty());
        assert_eq!(
            bridge.event_recipients("themes_changed"),
            vec!["plug.b".to_owned()]
        );
        assert_eq!(
            bridge.plugin_event_recipients("foo/bar"),
            vec!["plug.b".to_owned()]
        );
        assert!(bridge.plugin_event_recipients("other/evt").is_empty());

        // 7. dirty_slices_for_event 只标脏实际受影响的切片。
        assert_eq!(
            dirty_slices_for_event(&HostEvent::SessionsChanged),
            SliceMask::SESSIONS_DATA
        );
        assert_eq!(
            dirty_slices_for_event(&HostEvent::ThemesChanged),
            SliceMask::THEME
        );

        // 8. forget_plugin 清掉订阅记录。
        bridge.forget_plugin("plug.b");
        assert!(bridge.event_recipients("themes_changed").is_empty());
        assert!(bridge.plugin_event_recipients("foo/bar").is_empty());
        let (_, d3) = bridge.compute_push_split(
            json!({
                "locale": "zh-CN",
                "machines_meta": [],
                "sessions_data": [],
                "theme": { "rem": 16 },
            }),
            json!({}),
        );
        assert_eq!(d3.len(), 1);
        assert_eq!(d3[0].0, "plug.a");
        assert_eq!(d3[0].1, json!({ "revision": 4, "locale": "zh-CN" }));
    }

    #[gpui::test]
    fn sessions_data_cache_reuses_unchanged_machines_and_skips_matching_revisions(
        cx: &mut gpui::TestAppContext,
    ) {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use api::{
            Connections, Host, Machine, NotificationPermission,
            client::{Connector, RuntimeClient},
            model::{
                daemon_state::{DaemonSessionState, RuntimePresence},
                workspace_directory::WorkspaceDirectory,
            },
        };

        struct StubHost {
            local_rev: Arc<Mutex<(u64, u64)>>,
            remote_rev: Arc<Mutex<(u64, u64)>>,
            remote_project: Arc<Mutex<String>>,
            local_reads: Arc<AtomicUsize>,
            remote_reads: Arc<AtomicUsize>,
        }

        impl StubHost {
            fn count(&self, machine: &RuntimeKey) {
                match machine {
                    RuntimeKey::Local => {
                        self.local_reads.fetch_add(1, Ordering::Relaxed);
                    }
                    RuntimeKey::Remote(_) => {
                        self.remote_reads.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }

        impl Host for StubHost {
            fn machines(&self, _cx: &App) -> Vec<Machine> {
                vec![
                    Machine {
                        key: RuntimeKey::Local,
                        name: "This Mac".into(),
                        unavailable: None,
                    },
                    Machine {
                        key: RuntimeKey::Remote("r1".into()),
                        name: "Remote 1".into(),
                        unavailable: None,
                    },
                ]
            }

            fn client(&self, _machine: &RuntimeKey, _cx: &App) -> Option<RuntimeClient> {
                None
            }

            fn connector(&self, _machine: &RuntimeKey, _cx: &App) -> Option<Arc<dyn Connector>> {
                None
            }

            fn machine_online(&self, _machine: &RuntimeKey, _cx: &App) -> bool {
                true
            }

            fn connections(&self, _cx: &App) -> Connections {
                Connections::default()
            }

            fn view_shown(&self, _id: &str, _cx: &App) -> bool {
                false
            }

            fn view_laid_out(&self, _id: &str, _cx: &App) -> bool {
                false
            }

            fn main_window(&self, _cx: &App) -> Option<gpui::AnyWindowHandle> {
                None
            }

            fn window_active(&self, _cx: &App) -> bool {
                true
            }

            fn overlay_open(&self, _cx: &App) -> bool {
                false
            }

            fn path_style(&self, _machine: &RuntimeKey, _cx: &App) -> PathStyle {
                PathStyle::Posix
            }

            fn session_states(&self, machine: &RuntimeKey, _cx: &App) -> Vec<DaemonSessionState> {
                self.count(machine);
                Vec::new()
            }

            fn session_presence(
                &self,
                _machine: &RuntimeKey,
                _session_id: &str,
                _cx: &App,
            ) -> Option<RuntimePresence> {
                None
            }

            fn workspace_directory(
                &self,
                machine: &RuntimeKey,
                _cx: &App,
            ) -> Option<WorkspaceDirectory> {
                self.count(machine);
                let project = match machine {
                    RuntimeKey::Local => "/work/local".to_owned(),
                    RuntimeKey::Remote(_) => self.remote_project.lock().unwrap().clone(),
                };
                Some(WorkspaceDirectory {
                    epoch: "e1".into(),
                    revision: 1,
                    projects: vec![project],
                    ..Default::default()
                })
            }

            fn sessions_revision(&self, machine: &RuntimeKey, _cx: &App) -> Option<(u64, u64)> {
                let lock = match machine {
                    RuntimeKey::Local => &self.local_rev,
                    RuntimeKey::Remote(_) => &self.remote_rev,
                };
                Some(*lock.lock().unwrap())
            }

            fn notification_permission(&self, _cx: &App) -> NotificationPermission {
                NotificationPermission::Unsupported
            }
        }

        let local_rev = Arc::new(Mutex::new((1, 1)));
        let remote_rev = Arc::new(Mutex::new((1, 1)));
        let remote_project = Arc::new(Mutex::new("/work/r1-v1".to_owned()));
        let local_reads = Arc::new(AtomicUsize::new(0));
        let remote_reads = Arc::new(AtomicUsize::new(0));
        let stub = StubHost {
            local_rev,
            remote_rev: Arc::clone(&remote_rev),
            remote_project: Arc::clone(&remote_project),
            local_reads: Arc::clone(&local_reads),
            remote_reads: Arc::clone(&remote_reads),
        };
        let bridge = Bridge::default();

        cx.update(|cx| {
            api::install(stub, cx);

            let first = bridge
                .snapshot_sessions_data_cached(cx)
                .expect("initial snapshot should build");
            let items = first.as_array().expect("sessions_data should be an array");
            assert_eq!(items.len(), 2);
            assert_eq!(items[0]["directory"]["projects"], json!(["/work/local"]));
            assert_eq!(items[1]["directory"]["projects"], json!(["/work/r1-v1"]));
            assert_eq!(local_reads.load(Ordering::Relaxed), 2);
            assert_eq!(remote_reads.load(Ordering::Relaxed), 2);

            assert!(
                bridge.snapshot_sessions_data_cached(cx).is_none(),
                "unchanged revisions must skip rebuilding sessions_data"
            );
            assert_eq!(local_reads.load(Ordering::Relaxed), 2);
            assert_eq!(remote_reads.load(Ordering::Relaxed), 2);

            *remote_project.lock().unwrap() = "/work/r1-v2".to_owned();
            *remote_rev.lock().unwrap() = (2, 1);

            let third = bridge
                .snapshot_sessions_data_cached(cx)
                .expect("bumped remote revision must rebuild sessions_data");
            let items3 = third.as_array().expect("sessions_data should be an array");
            assert_eq!(items3.len(), 2);
            assert_eq!(items3[0]["directory"]["projects"], json!(["/work/local"]));
            assert_eq!(items3[1]["directory"]["projects"], json!(["/work/r1-v2"]));
            assert_eq!(
                local_reads.load(Ordering::Relaxed),
                2,
                "unchanged Local machine must reuse cached JSON without re-reading host state"
            );
            assert_eq!(remote_reads.load(Ordering::Relaxed), 4);
        });
    }
}
