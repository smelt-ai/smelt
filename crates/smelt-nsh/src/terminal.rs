//! `smelt.terminal`：终端原生画布组件（桥接 `smelt_terminal::TerminalCanvas` 到 NSH JS）。
//!
//! Props：
//! - `spec`: `{ machine: null | string, session_id: string, cwd?: string | null, launch?: string | null, reuseKey?: string | null }`
//! - `focusSeq`: `number`（递增时把焦点交给终端）
//! - `retrySeq`: `number`（递增时重试连接）
//! - `sendText`: `{ seq: number, text: string } | null`（递增 `seq` 时往终端写入文本）
//! - `searchQuery`: `{ open: boolean, query: string } | null`（终端内搜索开关与关键词）
//! - `searchStep`: `{ seq: number, query: string, backward: boolean } | null`（递增 `seq` 时跳转上/下一处匹配）
//!
//! Events：
//! - `stateChange`: `{ state: "idle" | "connecting" | "live" | "failed", error: string | null }`
//! - `reconnecting`: `{ active: boolean }`
//! - `writeError`: `{ kind: "overflow" | "session_ended" | "reopened" }`
//! - `focusIn`: `{}`
//! - `searchOpen`: `{}`
//! - `searchStatus`: `{ current: number, total: number }`
//! - `selectionChange`: `{ text: string | null }`

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
use smelt_terminal::{TerminalCanvas, TerminalCanvasEvent, TerminalEventHandler, TerminalSpec};

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
        TerminalCanvasEvent::SearchOpen => {
            cx.emit("searchOpen", json!({}));
        }
        TerminalCanvasEvent::SearchStatus { current, total } => {
            cx.emit(
                "searchStatus",
                json!({ "current": current, "total": total }),
            );
        }
        TerminalCanvasEvent::SelectionChanged(text) => {
            cx.emit("selectionChange", json!({ "text": text }));
        }
    })
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
    canvas: Option<Entity<TerminalCanvas>>,
    focus: Option<FocusHandle>,
    focus_seq: u64,
    pending_focus: bool,
    retry_seq: u64,
    send_text_seq: u64,
    search_step_seq: u64,
    last_search: (bool, String),
}

impl SmeltTerminal {
    pub(crate) fn make(cx: ComponentContext) -> Box<dyn NativeComponent> {
        Box::new(Self {
            cx,
            async_app: None,
            spec: None,
            reuse_key: None,
            canvas: None,
            focus: None,
            focus_seq: 0,
            pending_focus: false,
            retry_seq: 0,
            send_text_seq: 0,
            search_step_seq: 0,
            last_search: (false, String::new()),
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
            reclaimed.update(cx, |canvas, cx| {
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
        let canvas = cx.new(|cx| TerminalCanvas::new(Some(handler), window, cx));
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
            "searchQuery" => {
                let next = match value.and_then(Value::as_object) {
                    Some(obj) => (
                        obj.get("open").and_then(Value::as_bool).unwrap_or(false),
                        obj.get("query")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                    ),
                    None => (false, String::new()),
                };
                if next != self.last_search {
                    self.last_search = next.clone();
                    if let Some(canvas) = &self.canvas {
                        canvas.update(cx, |canvas, cx| {
                            canvas.set_search(next.0, &next.1, cx);
                        });
                    }
                }
            }
            "searchStep" => {
                if let Some(obj) = value.and_then(Value::as_object)
                    && let Some(seq) = obj.get("seq").and_then(Value::as_u64)
                    && seq > self.search_step_seq
                {
                    self.search_step_seq = seq;
                    let query = obj.get("query").and_then(Value::as_str).unwrap_or("");
                    let backward = obj
                        .get("backward")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if let Some(canvas) = &self.canvas {
                        canvas.update(cx, |canvas, cx| {
                            canvas.find_next(query, backward, cx);
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
