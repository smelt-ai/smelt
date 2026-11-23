//! `smelt.desktop`：远程桌面原生画布组件（桥接 `smelt_desktop::DesktopCanvas` 到 NSH JS）。
//!
//! Props：
//! - `machine`: `null | string`（目标运行时键）
//! - `active`: `bool`（`true` 时挂载本地转发口与原生 WebView，`false` 时立刻释放零后台流量）
//! - `control`: `bool`（接管/只看）
//! - `reconnectSeq`: `number`（递增时触发重连；若画布未启动则重新启动）
//!
//! Events：`connected`、`disconnected`（`{ clean }`）、`clipboard`（`{ text }`）、`text`（`{ text }`）、
//! `error`（`{ kind, message }`）。

use std::rc::Rc;

use gpui::{AnyElement, App, AppContext as _, Entity, IntoElement as _, Window};
use nsh_platform_gpui::{ComponentContext, NativeComponent};
use serde_json::{Value, json};
use smelt_desktop::{DesktopCanvas, DesktopCanvasEvent};
use smelt_plugin_api::{self as api, RuntimeKey};

pub(crate) const NAME: &str = "smelt.desktop";

pub(crate) struct SmeltDesktop {
    cx: ComponentContext,
    machine: Option<RuntimeKey>,
    active: bool,
    control: bool,
    reconnect_seq: u64,
    canvas: Option<Entity<DesktopCanvas>>,
}

impl SmeltDesktop {
    pub(crate) fn make(cx: ComponentContext) -> Box<dyn NativeComponent> {
        Box::new(Self {
            cx,
            machine: None,
            active: false,
            control: true,
            reconnect_seq: 0,
            canvas: None,
        })
    }

    fn mount(&mut self, window: &mut Window, cx: &mut App) {
        self.canvas = None;
        if !self.active {
            self.cx.redraw();
            return;
        }
        let Some(machine) = self.machine.clone() else {
            self.cx.redraw();
            return;
        };
        let Some(client) = api::host(cx).client(&machine, cx) else {
            self.cx.emit(
                "error",
                json!({ "kind": "proxy", "message": api::machine_removed() }),
            );
            self.cx.redraw();
            return;
        };
        let ctx = self.cx.clone();
        let on_event = Rc::new(move |event: DesktopCanvasEvent| match event {
            DesktopCanvasEvent::Connected => ctx.emit("connected", json!({})),
            DesktopCanvasEvent::Disconnected { clean } => {
                ctx.emit("disconnected", json!({ "clean": clean }))
            }
            DesktopCanvasEvent::Clipboard(text) => ctx.emit("clipboard", json!({ "text": text })),
            DesktopCanvasEvent::Text(text) => ctx.emit("text", json!({ "text": text })),
            DesktopCanvasEvent::Error { kind, message } => {
                ctx.emit("error", json!({ "kind": kind, "message": message }))
            }
        });
        let control = self.control;
        self.canvas = Some(cx.new(|cx| DesktopCanvas::new(client, control, on_event, window, cx)));
        self.cx.redraw();
    }
}

impl NativeComponent for SmeltDesktop {
    fn render(&mut self, _: &mut Window, _: &mut App) -> Option<AnyElement> {
        self.canvas
            .as_ref()
            .map(|canvas| canvas.clone().into_any_element())
    }

    fn set_prop(&mut self, name: &str, value: Option<&Value>, window: &mut Window, cx: &mut App) {
        match name {
            "machine" => {
                let next = value.and_then(|v| {
                    if v.is_null() {
                        Some(RuntimeKey::Local)
                    } else {
                        api::machine_from_json(v)
                    }
                });
                if self.machine != next {
                    self.machine = next;
                    if self.active {
                        self.mount(window, cx);
                    }
                }
            }
            "active" => {
                let next = value.and_then(Value::as_bool).unwrap_or(false);
                if self.active != next {
                    self.active = next;
                    self.mount(window, cx);
                }
            }
            "control" => {
                let next = value.and_then(Value::as_bool).unwrap_or(true);
                if self.control != next {
                    self.control = next;
                    if let Some(canvas) = &self.canvas {
                        canvas.update(cx, |canvas, cx| canvas.set_control(next, cx));
                    }
                }
            }
            "reconnectSeq" => {
                let seq = value.and_then(Value::as_u64).unwrap_or(0);
                if seq > self.reconnect_seq {
                    self.reconnect_seq = seq;
                    if self.active {
                        if let Some(canvas) = &self.canvas {
                            canvas.update(cx, |canvas, cx| canvas.reconnect(cx));
                        } else {
                            self.mount(window, cx);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
