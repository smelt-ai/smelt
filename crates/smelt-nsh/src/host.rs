//! 试点给插件的宿主这头：一份只读的通用状态（宿主变了广播整份）、离散宿主事件转发
//! （`smelt.event`），以及通用宿主调用（`state`、`run`、`call`、`context_menu_items`、
//! `clipboard.write`、`fs.path_kind`、`register_topics`）。只给通用事实与能力，不认识任何具体插件。
//!
//! 插件的 `host.call` 经各自的 `HostChannel` 在引擎线程上直达，携带不可伪造的 `plugin_id`：
//! `state` 当场拿缓存并拼上该插件自己的文案回；`fs.path_kind` 在后台线程做不碰主线程；
//! 需要 GPUI `&App` 的调用封装为 [`Action`] 转主线程执行。

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use gpui::{App, EntityId};
use nsh_engine::HostCall;
use serde_json::{Value, json};
use smelt_plugin_api::{
    self as api, ContextMenuAction, ContextMenuItem, HostEvent, Placement, RuntimeKey,
    TopicMessage,
    client::protocol::control::ControlMethod,
    model::{agent_status::AgentStatus, runtime_path::PathStyle},
};
use smelt_ui::{
    kit::{RADIUS, TYPE, TextStyle},
    ui_theme,
};
use tokio::sync::mpsc;

use crate::plugins::PluginHost;

/// 推给外壳和插件的事件：整份状态，`revision` 只增不减。
const STATE: &str = "smelt.state";
/// 推给插件的离散宿主事件（runtime_topic、plugin、window_activated、open_paths 等）。
const EVENT: &str = "smelt.event";

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
    RegisterTopics {
        plugin: String,
        topics: Vec<String>,
        call: HostCall,
    },
}

#[derive(Default)]
struct Shared {
    state: Value,
    revision: u64,
    /// 插件 id → 它 `locales/` 里的文案（语言 → 键 → 字）。
    texts: BTreeMap<String, Value>,
    /// 插件 id → 它登记订阅的运行时话题集合。
    topics: BTreeMap<String, BTreeSet<String>>,
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
                let mut state = shared.state.clone();
                if let Some(fields) = state.as_object_mut() {
                    let texts = shared.texts.get(plugin).cloned();
                    fields.insert("texts".into(), texts.unwrap_or_else(|| json!({})));
                }
                state
            };
            call.resolve(state);
        });

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
                send_action(
                    &actions,
                    Action::Call {
                        machine,
                        method,
                        params,
                        timeout,
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

        {
            let bridge = self.clone();
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

        received
    }

    pub fn set_texts(&self, plugin: &str, texts: Value) {
        self.lock().texts.insert(plugin.to_owned(), texts);
    }

    pub fn forget_plugin(&self, plugin: &str) {
        let mut shared = self.lock();
        shared.texts.remove(plugin);
        shared.topics.remove(plugin);
    }

    /// 按宿主眼下的样子换一份状态，变了才广播。在主线程的事件里调，不在 render 里。
    pub fn publish(&self, plugins: &PluginHost, pilot: EntityId, rem: f32, cx: &App) {
        self.push(plugins, snapshot(pilot, rem, cx));
    }

    pub(crate) fn push(&self, plugins: &PluginHost, mut next: Value) {
        let mut shared = self.lock();
        next["revision"] = shared.revision.into();
        if next == shared.state {
            return;
        }
        shared.revision += 1;
        next["revision"] = shared.revision.into();
        shared.state = next.clone();
        drop(shared);
        plugins.broadcast_all(STATE, next);
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
            _ => {}
        }
    }

    #[cfg(test)]
    pub(crate) fn emit_event(&self, plugins: &PluginHost, event: Value) {
        plugins.broadcast(EVENT, event);
    }
}

fn send_action(sender: &mpsc::UnboundedSender<Action>, action: Action) {
    if let Err(unsent) = sender.send(action) {
        let call = match unsent.0 {
            Action::Run { call, .. }
            | Action::Call { call, .. }
            | Action::ContextMenuItems { call, .. }
            | Action::ClipboardWrite { call, .. }
            | Action::RegisterTopics { call, .. } => call,
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
        HostEvent::SessionsChanged
            | HostEvent::MachinesChanged
            | HostEvent::ActiveContextChanged
            | HostEvent::ViewsChanged
            | HostEvent::LaidOutViewsChanged
            | HostEvent::ShownViewsChanged
            | HostEvent::ViewBadgesChanged
            | HostEvent::PagesChanged
            | HostEvent::CommandsChanged
            | HostEvent::LocaleChanged
            | HostEvent::SafeAreaChanged
            | HostEvent::ThemesChanged
    )
}

fn snapshot(pilot: EntityId, rem: f32, cx: &App) -> Value {
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
        .map(|page| json!({ "id": page.id.to_string(), "index": page.index.as_deref() }))
        .collect();
    let views: Vec<Value> = api::views(cx)
        .iter()
        .map(|view| {
            json!({
                "id": view.id.to_string(),
                "title": view.title.get().to_string(),
                "icon": view.icon.to_string(),
                "settings": view.settings.as_ref().map(|_| true),
                "instances": view.instances,
                "fills": view.fills.iter().map(|slot| slot.as_ref()).collect::<Vec<_>>(),
                "badge": api::view_badge(&view.id, cx).map(|badge| badge.to_string()),
                "laid_out": host.view_laid_out(&view.id, cx),
                "shown": host.view_shown(&view.id, cx),
            })
        })
        .collect();
    let commands: Vec<Value> = api::commands(cx)
        .iter()
        .map(|command| json!({ "id": command.id.to_string(), "title": command.title.get().to_string() }))
        .collect();
    json!({
        "locale": api::locale().to_string(),
        "machines": machines,
        "connections": connections,
        "active_context": { "project": project, "session": session },
        "pages": pages,
        "views": views,
        "commands": commands,
        "bar_insets": bar_insets(pilot, cx),
        "theme": theme(rem),
    })
}

fn machine_value(machine: &RuntimeKey) -> Value {
    api::machine_to_json(machine)
}

/// 贴着这块面板下面某个角的一行，两头让多少。
fn bar_insets(pilot: EntityId, cx: &App) -> Value {
    let area = api::safe_area(pilot, cx);
    let insets = |bottom: bool, left: bool, right: bool| {
        let insets = area
            .child(gpui::Edges {
                top: false,
                bottom,
                left,
                right,
            })
            .bar_insets();
        json!({ "left": f32::from(insets.left), "right": f32::from(insets.right) })
    };
    json!({
        "none": insets(false, false, false),
        "bottom-left": insets(true, true, false),
        "bottom-right": insets(true, false, true),
    })
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
            "row": f32::from(ui_theme::row_radius()),
        },
    })
}
