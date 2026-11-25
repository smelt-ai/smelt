//! 终端画布原子能力：连接 smeltd 终端会话、alacritty_terminal 网格状态机、GPUI 画布绘制
//! （含 IME、Cmd+F 终端内搜索、kitty 图形、断线自愈与输入恢复），以及终端主题配色快照与字号原子量。
//!
//! 不认识任何插件 id，不含设置页、SQLite 存储、右键菜单 id 或本地化文案——这些全部由 JS 插件
//! （`plugins/terminal/`）实现。

mod redraw;
mod terminal;
mod terminal_images;
mod terminal_view;

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    App, Context, Entity, FocusHandle, Global, KeyBinding, SharedString, Subscription, Task,
    Window, div,
};
use smelt_plugin_api as api;
use smelt_plugin_api::RuntimeKey;
use smelt_plugin_api::model::terminal_theme::{TERMINAL_THEME_VERSION, TerminalThemeSnapshot};

use crate::terminal::Terminal;
use crate::terminal_view::{TerminalView, search_hit_bg, sel_bg};

pub use crate::terminal_view::{DEFAULT_FONT_PX, MAX_FONT_PX, MIN_FONT_PX, font_px, set_font_px};

/// 绑定终端聚焦时的专属键位（Tab / Shift-Tab / Esc），单进程幂等。
pub fn bind_keys(cx: &mut App) {
    struct KeysBound;
    impl Global for KeysBound {}

    if cx.has_global::<KeysBound>() {
        return;
    }
    cx.set_global(KeysBound);
    // 把 Tab/Shift-Tab 从组件库 Root 的焦点跳转手里、Esc 从工作台手里要回来，终端聚焦时
    // 发给 shell（见 terminal_view 里 `TerminalTab` 的注释）。
    cx.bind_keys([
        KeyBinding::new("tab", terminal_view::TerminalTab, Some("Terminal")),
        KeyBinding::new(
            "shift-tab",
            terminal_view::TerminalBackTab,
            Some("Terminal"),
        ),
        KeyBinding::new("escape", terminal_view::TerminalEscape, Some("Terminal")),
    ]);
}

/// 按当前主题组装的终端配色快照（供运行时应答 OSC 颜色查询与移动端同步）。
pub fn theme_snapshot() -> TerminalThemeSnapshot {
    TerminalThemeSnapshot {
        version: TERMINAL_THEME_VERSION,
        dark: terminal::is_dark(),
        background: terminal::default_bg(),
        foreground: terminal::default_fg(),
        // 光标是实心块反显（见 terminal_view 的 in_block 分支），块本身就是前景色。
        cursor: terminal::default_fg(),
        selection: sel_bg(),
        palette: terminal::ansi_palette().to_vec(),
        search_hit: search_hit_bg(false),
        search_hit_current: search_hit_bg(true),
    }
}

/// 连接一格终端所需的会话标识与启动参数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalSpec {
    pub machine: RuntimeKey,
    pub session_id: String,
    pub cwd: Option<String>,
    pub launch: Option<String>,
}

/// 终端画布内部原生控件（右键复制/粘贴、内置搜索条）使用的本地化文案，由外层 JS 传入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalLabels {
    pub copy: SharedString,
    pub paste: SharedString,
    pub search_placeholder: SharedString,
    pub search_none: SharedString,
}

impl Default for TerminalLabels {
    fn default() -> Self {
        Self {
            copy: "Copy".into(),
            paste: "Paste".into(),
            search_placeholder: "Find in terminal…".into(),
            search_none: "No results".into(),
        }
    }
}

/// 终端画布发往外层的生命周期与状态事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalCanvasEvent {
    StateChanged {
        state: &'static str,
        error: Option<String>,
    },
    Reconnecting(bool),
    WriteError {
        kind: &'static str,
    },
    FocusIn,
}

pub type TerminalEventHandler = Rc<dyn Fn(TerminalCanvasEvent)>;

enum State {
    Idle,
    Connecting,
    Live(Entity<TerminalView>),
    Failed(String),
}

/// 一格终端的画布实体：管理连接、焦点接管与重连，只画活体终端网格；
/// 连接中、失败重试、断线提示等外围界面交由外层 JS 绘制。
pub struct TerminalCanvas {
    spec: Option<TerminalSpec>,
    state: State,
    focus_handle: FocusHandle,
    labels: TerminalLabels,
    context_menu: Option<SharedString>,
    on_event: Option<TerminalEventHandler>,
    connecting: Option<Task<()>>,
    _focus_sub: Subscription,
}

impl TerminalCanvas {
    pub fn new(
        labels: TerminalLabels,
        context_menu: Option<SharedString>,
        on_event: Option<TerminalEventHandler>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let focus_sub = cx.on_focus_in(&focus_handle, window, |this, _, _| {
            this.emit(TerminalCanvasEvent::FocusIn);
        });
        Self {
            spec: None,
            state: State::Idle,
            focus_handle,
            labels,
            context_menu,
            on_event,
            connecting: None,
            _focus_sub: focus_sub,
        }
    }

    /// 绑定新的事件回调（实例复用认领时重绑到新的 `ComponentContext`），并回放当前状态。
    pub fn rebind_event_handler(
        &mut self,
        on_event: Option<TerminalEventHandler>,
        cx: &mut Context<Self>,
    ) {
        self.on_event = on_event.clone();
        if let State::Live(view) = &self.state {
            view.update(cx, |view, _| view.set_event_handler(on_event));
        }
        self.emit_current_state(cx);
    }

    pub fn set_labels(&mut self, labels: TerminalLabels, cx: &mut Context<Self>) {
        if self.labels == labels {
            return;
        }
        self.labels = labels.clone();
        if let State::Live(view) = &self.state {
            view.update(cx, |view, cx| view.set_labels(labels, cx));
        }
    }

    pub fn set_context_menu(&mut self, context_menu: Option<SharedString>, cx: &mut Context<Self>) {
        if self.context_menu == context_menu {
            return;
        }
        self.context_menu = context_menu.clone();
        if let State::Live(view) = &self.state {
            view.update(cx, |view, cx| view.set_context_menu(context_menu, cx));
        }
    }

    /// 按会话规格建立连接；相同的 `(machine, session_id)` 不重建连接。
    pub fn open(&mut self, spec: TerminalSpec, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .spec
            .as_ref()
            .is_some_and(|cur| cur.machine == spec.machine && cur.session_id == spec.session_id)
        {
            return;
        }
        self.spec = Some(spec);
        self.connect(window, cx);
    }

    /// 重试当前会话连接。
    pub fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.spec.is_some() {
            self.connect(window, cx);
        }
    }

    /// 往已连接的终端写入文字（等价于粘贴）。未连接时返回 `false`。
    pub fn send_text(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let State::Live(view) = &self.state else {
            return false;
        };
        view.update(cx, |view, cx| view.send_text(text, cx));
        true
    }

    pub fn focus_target(&self, cx: &App) -> FocusHandle {
        match &self.state {
            State::Live(view) => view.read(cx).focus_handle(),
            _ => self.focus_handle.clone(),
        }
    }

    pub fn focus_now(&self, window: &mut Window, cx: &mut App) {
        let target = self.focus_target(cx);
        window.focus(&target, cx);
    }

    fn emit(&self, event: TerminalCanvasEvent) {
        if let Some(on_event) = &self.on_event {
            on_event(event);
        }
    }

    fn emit_current_state(&self, cx: &App) {
        match &self.state {
            State::Idle => self.emit(TerminalCanvasEvent::StateChanged {
                state: "idle",
                error: None,
            }),
            State::Connecting => self.emit(TerminalCanvasEvent::StateChanged {
                state: "connecting",
                error: None,
            }),
            State::Live(view) => {
                self.emit(TerminalCanvasEvent::StateChanged {
                    state: "live",
                    error: None,
                });
                self.emit(TerminalCanvasEvent::Reconnecting(
                    view.read(cx).is_reconnecting(),
                ));
            }
            State::Failed(error) => self.emit(TerminalCanvasEvent::StateChanged {
                state: "failed",
                error: Some(error.clone()),
            }),
        }
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(spec) = self.spec.clone() else {
            return;
        };
        self.state = State::Connecting;
        self.emit(TerminalCanvasEvent::Reconnecting(false));
        self.emit(TerminalCanvasEvent::StateChanged {
            state: "connecting",
            error: None,
        });
        cx.notify();
        self.connecting = Some(cx.spawn_in(window, async move |this, cx| {
            if !api::wait_online(&spec.machine, 0, cx).await {
                let _ = this.update(cx, |this, cx| {
                    let message = api::machine_removed();
                    this.state = State::Failed(message.clone());
                    this.emit(TerminalCanvasEvent::StateChanged {
                        state: "failed",
                        error: Some(message),
                    });
                    cx.notify();
                });
                return;
            }
            let Ok(connector) = this.update(cx, |_, cx| api::host(cx).connector(&spec.machine, cx))
            else {
                return;
            };
            let Some(connector) = connector else {
                let _ = this.update(cx, |this, cx| {
                    let message = api::machine_removed();
                    this.state = State::Failed(message.clone());
                    this.emit(TerminalCanvasEvent::StateChanged {
                        state: "failed",
                        error: Some(message),
                    });
                    cx.notify();
                });
                return;
            };
            let open = spec.clone();
            // 行列先按 24×80 开，首帧布局后终端按实际大小重发。
            let terminal = api::blocking::run(move || {
                Terminal::spawn(
                    connector.as_ref(),
                    24,
                    80,
                    open.cwd.as_deref(),
                    &open.session_id,
                    open.launch.as_deref(),
                )
            })
            .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match terminal {
                    Ok(terminal) => {
                        let labels = this.labels.clone();
                        let context_menu = this.context_menu.clone();
                        let on_event = this.on_event.clone();
                        let view = cx.new(|cx| {
                            TerminalView::from_terminal(
                                cx,
                                spec.clone(),
                                terminal,
                                labels,
                                context_menu,
                                on_event,
                            )
                        });
                        if this.focus_handle.is_focused(window) {
                            window.focus(&view.read(cx).focus_handle(), cx);
                        }
                        this.state = State::Live(view);
                        this.emit(TerminalCanvasEvent::StateChanged {
                            state: "live",
                            error: None,
                        });
                    }
                    Err(error) => {
                        eprintln!("[terminal] 连不上终端会话 {}：{error:#}", spec.session_id);
                        let message = error.to_string();
                        this.state = State::Failed(message.clone());
                        this.emit(TerminalCanvasEvent::StateChanged {
                            state: "failed",
                            error: Some(message),
                        });
                    }
                }
                cx.notify();
            });
        }));
    }
}

impl Render for TerminalCanvas {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().size_full().track_focus(&self.focus_handle);
        match &self.state {
            State::Live(view) => root.child(view.clone()),
            State::Idle | State::Connecting | State::Failed(_) => root.on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus_handle, cx);
                }),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::theme_snapshot;
    use crate::terminal::lock_theme_globals;
    use smelt_plugin_api::model::terminal_theme::TerminalThemeSnapshot;
    use smelt_plugin_api::ui::ui_theme;

    /// 守护先于桌面起来、还没收到快照时用默认值：它要跟深色主题实际发的一致。
    #[test]
    fn dark_snapshot_matches_runtime_fallback() {
        let _guard = lock_theme_globals();
        ui_theme::set_light(false);
        assert_eq!(theme_snapshot(), TerminalThemeSnapshot::default());
    }

    #[test]
    fn snapshot_follows_theme_mode() {
        let _guard = lock_theme_globals();
        ui_theme::set_light(true);
        let light = theme_snapshot();
        assert!(!light.dark);
        assert_eq!(light.background, ui_theme::bg());
        assert_ne!(
            light.palette,
            TerminalThemeSnapshot::default().palette,
            "浅色主题要发浅色板，不能原样发深色板"
        );
        ui_theme::set_light(false);
    }
}
