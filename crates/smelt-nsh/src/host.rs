//! 试点给插件的宿主这头：一份只读的状态（宿主变了推整份）和两个调用（`state`、`run`）。
//! 只给通用的事实，不认识哪个插件。
//!
//! 插件的 `host.call` 在引擎线程上到：`state` 当场拿缓存回，`run` 转到主线程执行、完了再回。

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
};

use gpui::{App, EntityId};
use nsh_engine::HostCall;
use nsh_platform_gpui::AppView;
use serde_json::{Value, json};
use smelt_plugin_api::{self as api, HostEvent, RuntimeKey, model::agent_status::AgentStatus};
use smelt_ui::{
    kit::{TYPE, TextStyle},
    ui_theme,
};
use tokio::sync::mpsc;

use crate::plugins;

/// 推给插件的事件：整份状态，`revision` 只增不减。
const STATE: &str = "smelt.state";

/// 插件要主线程跑的一条命令。
pub(crate) struct Run {
    pub command: String,
    pub args: Value,
    pub call: HostCall,
}

#[derive(Default)]
struct Shared {
    state: Value,
    revision: u64,
    /// 插件 id → 它 `locales/` 里的文案（语言 → 键 → 字）。
    texts: BTreeMap<String, Value>,
    /// 报了开始装、还没报装好（或出了错但引擎没事）的插件：引擎停掉时就是它。
    loading: Option<String>,
}

#[derive(Clone, Default)]
pub(crate) struct Bridge(Arc<Mutex<Shared>>);

impl Bridge {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 接上插件的调用和装载进度。在装载外壳之前调一次。
    pub fn serve(&self, view: &AppView) -> mpsc::UnboundedReceiver<Run> {
        let (runs, received) = mpsc::unbounded_channel();
        let bridge = self.clone();
        view.host().handle(plugins::CALL, move |mut call| {
            let mut request = call.take_data();
            let plugin = request["plugin"].as_str().unwrap_or_default().to_owned();
            match request["name"].as_str().unwrap_or_default() {
                "state" => {
                    let shared = bridge.lock();
                    let mut state = shared.state.clone();
                    if let Some(fields) = state.as_object_mut() {
                        let texts = shared.texts.get(&plugin).cloned();
                        fields.insert("texts".into(), texts.unwrap_or_else(|| json!({})));
                    }
                    drop(shared);
                    call.resolve(state);
                }
                "run" => {
                    let mut data = request["data"].take();
                    let Some(command) = data["command"].as_str().map(str::to_owned) else {
                        return call.reject("run：缺 command");
                    };
                    let args = data["args"].take();
                    if let Err(unsent) = runs.send(Run {
                        command,
                        args,
                        call,
                    }) {
                        unsent.0.call.reject("面板已经关了");
                    }
                }
                other => call.reject(format!("宿主没有 {other}")),
            }
        });
        let bridge = self.clone();
        view.host().handle(plugins::STATUS, move |call| {
            let status = call.data();
            let plugin = status["plugin"].as_str().unwrap_or_default();
            match status["state"].as_str() {
                Some("running") => bridge.lock().loading = Some(plugin.to_owned()),
                // 还可能是它吃光了内存、引擎马上要停：先不清。
                Some("failed") => {
                    eprintln!("[nsh-pilot] {plugin} 装不上：{}", status["message"]);
                }
                _ => {
                    let mut shared = bridge.lock();
                    if shared.loading.as_deref() == Some(plugin) {
                        shared.loading = None;
                    }
                }
            }
        });
        received
    }

    pub fn take_loading(&self) -> Option<String> {
        self.lock().loading.take()
    }

    pub fn set_texts(&self, plugin: &str, texts: Value) {
        self.lock().texts.insert(plugin.to_owned(), texts);
    }

    /// 按宿主眼下的样子换一份状态，变了才推。在主线程的事件里调，不在 render 里。
    pub fn publish(&self, app: &AppView, pilot: EntityId, rem: f32, cx: &App) {
        self.push(app, snapshot(pilot, rem, cx));
    }

    pub(crate) fn push(&self, app: &AppView, mut next: Value) {
        let mut shared = self.lock();
        next["revision"] = shared.revision.into();
        if next == shared.state {
            return;
        }
        shared.revision += 1;
        next["revision"] = shared.revision.into();
        shared.state = next.clone();
        drop(shared);
        if let Err(error) = app.host().send(STATE, next) {
            eprintln!("[nsh-pilot] 状态送不进去：{error}");
        }
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
            let facts: Vec<Value> = directory
                .iter()
                .flat_map(|directory| &directory.conversations)
                .map(|conversation| {
                    let status = match conversation.status(&states) {
                        AgentStatus::NeedsYou => "needs_you",
                        AgentStatus::Running => "running",
                        AgentStatus::Idle => "idle",
                    };
                    json!({ "id": conversation.id, "status": status })
                })
                .collect();
            json!({
                "machine": machine_value(&machine.key),
                "directory": directory,
                "facts": facts,
            })
        })
        .collect();
    let session = api::active_context(cx)
        .session
        .map(|session| json!({ "machine": machine_value(&session.machine), "id": session.id }));
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
        "active_context": { "session": session },
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
            "text": api::hex_color(p.text),
            "muted": api::hex_color(p.muted),
            "muted_selected": api::hex_color(p.muted_selected),
            "bad": api::hex_color(p.bad),
        },
        "type": { "ui": text(TYPE.ui), "ui_medium": text(TYPE.ui_medium) },
        "radius": { "row": f32::from(ui_theme::row_radius()) },
    })
}
