//! `smelt.terminal`：终端原生画布组件（桥接 `smelt_terminal::TerminalCanvas` 到 NSH JS）。
//!
//! Props：
//! - `spec`: `{ machine: null | string, session_id: string, cwd?: string | null, launch?: string | null, reuseKey?: string | null }`
//! - `contextMenu`: `string | null`（可选右键菜单扩展点 id）
//! - `labels`: `{ copy?, paste?, search_placeholder?, search_none? }`
//! - `focusSeq`: `number`（递增时把焦点交给终端）
//! - `retrySeq`: `number`（递增时重试连接）
//! - `sendText`: `{ seq: number, text: string } | null`（递增 `seq` 时往终端写入文本）
//!
//! Events：
//! - `stateChange`: `{ state: "idle" | "connecting" | "live" | "failed", error: string | null }`
//! - `reconnecting`: `{ active: boolean }`
//! - `writeError`: `{ kind: "overflow" | "session_ended" | "reopened" }`
//! - `focusIn`: `{}`

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Entity, FocusHandle, IntoElement as _, Window,
};
use nsh_platform_gpui::{ComponentContext, NativeComponent};
use serde_json::{Value, json};
use smelt_plugin_api::{self as api, RuntimeKey};
use smelt_terminal::{
    TerminalCanvas, TerminalCanvasEvent, TerminalEventHandler, TerminalLabels, TerminalSpec,
};

pub(crate) const NAME: &str = "smelt.terminal";

static REUSE_SEQ: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static REUSE_POOL: RefCell<HashMap<String, (u64, Entity<TerminalCanvas>)>> =
        RefCell::new(HashMap::new());
}

fn park_canvas(key: String, canvas: Entity<TerminalCanvas>, app: Option<&AsyncApp>) {
    let token = REUSE_SEQ.fetch_add(1, Ordering::Relaxed);
    REUSE_POOL.with(|pool| {
        pool.borrow_mut().insert(key.clone(), (token, canvas));
    });
    if let Some(app) = app {
        app.spawn(async move |cx| {
            cx.update(|cx| {
                cx.defer(move |_| {
                    REUSE_POOL.with(|pool| {
                        let mut pool = pool.borrow_mut();
                        if pool.get(&key).is_some_and(|(t, _)| *t == token) {
                            pool.remove(&key);
                        }
                    });
                });
            });
        })
        .detach();
    } else {
        REUSE_POOL.with(|pool| {
            pool.borrow_mut().remove(&key);
        });
    }
}

fn claim_canvas(key: &str) -> Option<Entity<TerminalCanvas>> {
    REUSE_POOL.with(|pool| pool.borrow_mut().remove(key).map(|(_, canvas)| canvas))
}

fn event_handler(cx: ComponentContext) -> TerminalEventHandler {
    Rc::new(move |event| match event {
        TerminalCanvasEvent::StateChanged { state, error } => {
            cx.emit("stateChange", json!({ "state": state, "error": error }));
        }
        TerminalCanvasEvent::Reconnecting(active) => {
            cx.emit("reconnecting", json!({ "active": active }));
        }
        TerminalCanvasEvent::WriteError { kind } => {
            cx.emit("writeError", json!({ "kind": kind }));
        }
        TerminalCanvasEvent::FocusIn => {
            cx.emit("focusIn", json!({}));
        }
    })
}

fn parse_labels(value: Option<&Value>) -> TerminalLabels {
    let mut labels = TerminalLabels::default();
    let Some(obj) = value.and_then(Value::as_object) else {
        return labels;
    };
    if let Some(v) = obj.get("copy").and_then(Value::as_str) {
        labels.copy = v.to_owned().into();
    }
    if let Some(v) = obj.get("paste").and_then(Value::as_str) {
        labels.paste = v.to_owned().into();
    }
    if let Some(v) = obj.get("search_placeholder").and_then(Value::as_str) {
        labels.search_placeholder = v.to_owned().into();
    }
    if let Some(v) = obj.get("search_none").and_then(Value::as_str) {
        labels.search_none = v.to_owned().into();
    }
    labels
}

fn parse_spec(value: &Value) -> Option<(TerminalSpec, Option<String>)> {
    let obj = value.as_object()?;
    let session_id = obj
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_owned();
    let machine = match obj.get("machine") {
        None | Some(Value::Null) => RuntimeKey::Local,
        Some(v) => api::machine_from_json(v)?,
    };
    let cwd = obj
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let launch = obj
        .get("launch")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let reuse_key = obj
        .get("reuseKey")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    Some((
        TerminalSpec {
            machine,
            session_id,
            cwd,
            launch,
        },
        reuse_key,
    ))
}

pub(crate) struct SmeltTerminal {
    cx: ComponentContext,
    async_app: Option<AsyncApp>,
    spec: Option<TerminalSpec>,
    reuse_key: Option<String>,
    labels: TerminalLabels,
    context_menu: Option<gpui::SharedString>,
    canvas: Option<Entity<TerminalCanvas>>,
    focus: Option<FocusHandle>,
    focus_seq: u64,
    pending_focus: bool,
    retry_seq: u64,
    send_text_seq: u64,
}

impl SmeltTerminal {
    pub(crate) fn make(cx: ComponentContext) -> Box<dyn NativeComponent> {
        Box::new(Self {
            cx,
            async_app: None,
            spec: None,
            reuse_key: None,
            labels: TerminalLabels::default(),
            context_menu: None,
            canvas: None,
            focus: None,
            focus_seq: 0,
            pending_focus: false,
            retry_seq: 0,
            send_text_seq: 0,
        })
    }

    fn apply_spec(&mut self, value: Option<&Value>, window: &mut Window, cx: &mut App) {
        let parsed = value.and_then(parse_spec);
        let Some((spec, reuse_key)) = parsed else {
            self.spec = None;
            self.reuse_key = None;
            self.canvas = None;
            self.focus = None;
            self.cx.redraw();
            return;
        };
        if self
            .spec
            .as_ref()
            .is_some_and(|cur| cur.machine == spec.machine && cur.session_id == spec.session_id)
            && self.canvas.is_some()
        {
            self.reuse_key = reuse_key;
            return;
        }
        self.spec = Some(spec.clone());
        self.reuse_key = reuse_key.clone();

        if let Some(key) = reuse_key.as_deref()
            && let Some(reclaimed) = claim_canvas(key)
        {
            let handler = event_handler(self.cx.clone());
            let labels = self.labels.clone();
            let menu = self.context_menu.clone();
            reclaimed.update(cx, |canvas, cx| {
                canvas.set_labels(labels, cx);
                canvas.set_context_menu(menu, cx);
                canvas.rebind_event_handler(Some(handler), cx);
            });
            let target = reclaimed.read(cx).focus_target(cx);
            self.focus = Some(target.clone());
            if self.pending_focus {
                self.pending_focus = false;
                window.focus(&target, cx);
            }
            self.canvas = Some(reclaimed);
            self.cx.redraw();
            return;
        }

        let handler = event_handler(self.cx.clone());
        let labels = self.labels.clone();
        let menu = self.context_menu.clone();
        let canvas = cx.new(|cx| TerminalCanvas::new(labels, menu, Some(handler), window, cx));
        canvas.update(cx, |canvas, cx| canvas.open(spec, window, cx));
        let target = canvas.read(cx).focus_target(cx);
        self.focus = Some(target.clone());
        if self.pending_focus {
            self.pending_focus = false;
            window.focus(&target, cx);
        }
        self.canvas = Some(canvas);
        self.cx.redraw();
    }
}

impl NativeComponent for SmeltTerminal {
    fn render(&mut self, _: &mut Window, cx: &mut App) -> Option<AnyElement> {
        let canvas = self.canvas.as_ref()?;
        self.focus = Some(canvas.read(cx).focus_target(cx));
        Some(canvas.clone().into_any_element())
    }

    fn focus_handle(&self) -> Option<FocusHandle> {
        self.focus.clone()
    }

    fn set_prop(&mut self, name: &str, value: Option<&Value>, window: &mut Window, cx: &mut App) {
        if self.async_app.is_none() {
            self.async_app = Some(cx.to_async());
        }
        smelt_terminal::bind_keys(cx);
        match name {
            "spec" => self.apply_spec(value, window, cx),
            "labels" => {
                let labels = parse_labels(value);
                self.labels = labels.clone();
                if let Some(canvas) = &self.canvas {
                    canvas.update(cx, |canvas, cx| canvas.set_labels(labels, cx));
                }
            }
            "contextMenu" => {
                let menu = value
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(|s| gpui::SharedString::from(s.to_owned()));
                self.context_menu = menu.clone();
                if let Some(canvas) = &self.canvas {
                    canvas.update(cx, |canvas, cx| canvas.set_context_menu(menu, cx));
                }
            }
            "focusSeq" => {
                let seq = value.and_then(Value::as_u64).unwrap_or(0);
                if seq > self.focus_seq {
                    self.focus_seq = seq;
                    if let Some(canvas) = &self.canvas {
                        let target = canvas.read(cx).focus_target(cx);
                        window.focus(&target, cx);
                    } else {
                        self.pending_focus = true;
                    }
                }
            }
            "retrySeq" => {
                let seq = value.and_then(Value::as_u64).unwrap_or(0);
                if seq > self.retry_seq {
                    self.retry_seq = seq;
                    if let Some(canvas) = &self.canvas {
                        canvas.update(cx, |canvas, cx| canvas.retry(window, cx));
                    }
                }
            }
            "sendText" => {
                if let Some(obj) = value.and_then(Value::as_object)
                    && let Some(seq) = obj.get("seq").and_then(Value::as_u64)
                    && seq > self.send_text_seq
                    && let Some(text) = obj.get("text").and_then(Value::as_str)
                {
                    self.send_text_seq = seq;
                    if let Some(canvas) = &self.canvas {
                        canvas.update(cx, |canvas, cx| {
                            canvas.send_text(text, cx);
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

impl Drop for SmeltTerminal {
    fn drop(&mut self) {
        if let (Some(key), Some(canvas)) = (self.reuse_key.take(), self.canvas.take()) {
            park_canvas(key, canvas, self.async_app.as_ref());
        }
    }
}
