//! 一格终端的实例视图（[`crate::contract::VIEW`]）：摆终端的一方按 [`TerminalArgs`] 建一份，
//! 这里等机器上线、连上守护里的那段会话（没有就新起），连上以后换成 [`TerminalView`]。
//!
//! 放掉实例只是断开这格画面，守护里的会话还在；结束会话是运行时的操作，由摆它的一方发。

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    App, Context, Entity, FocusHandle, Global, SharedString, Task, WeakEntity, Window, div, rgb,
};
use smelt_plugin_api::ui::Button;
use smelt_plugin_api::ui::ui_theme;
use smelt_plugin_api::{self as api, PluginView, RuntimeKey, ViewHandle, ViewSpec};

use crate::contract::{TerminalArgs, TerminalText, VIEW};
use crate::terminal::Terminal;
use crate::terminal_view::TerminalView;
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

/// 实例视图的登记项。
pub(crate) fn spec() -> ViewSpec {
    ViewSpec {
        // 来源内的名字，跟 `Command::name` 一样从约定里的 id 取。
        name: VIEW.split_once('/').map_or(VIEW, |(_, name)| name).into(),
        title: "终端".into(),
        icon: crate::ICON.into(),
        // 一格终端只画网格，标题、状态由摆它的一方画。
        draws_top_bar: false,
        instances: true,
        settings: None,
        build: Rc::new(|_, cx| Rc::new(cx.new(TerminalInstance::new)) as Rc<dyn ViewHandle>),
    }
}

/// 一格终端。还没连上时画等待或失败，连上以后整块交给 [`TerminalView`]。
struct TerminalInstance {
    /// 第一次 `open` 给的参数。实例按参数建，之后再 `open` 不换会话。
    args: Option<TerminalArgs>,
    state: State,
    /// 还没连上时接住焦点，连上以后转给终端。
    focus_handle: FocusHandle,
    /// 在等上线、连守护的那次。实例放掉就跟着取消，不再替没人看的格子连。
    connecting: Option<Task<()>>,
}

enum State {
    /// 等 `open` 给参数。
    Idle,
    /// 在等机器上线、连守护。
    Connecting,
    Live(Entity<TerminalView>),
    /// 连不上：参数不对、守护拒了。给人看的原因，可以重试。
    Failed(SharedString),
}

impl TerminalInstance {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            args: None,
            state: State::Idle,
            focus_handle: cx.focus_handle(),
            connecting: None,
        }
    }

    /// 等机器上线后连守护里的会话，没有就按 `cwd`、`launch` 新起。机器被移除了就停在失败。
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(args) = self.args.clone() else {
            return;
        };
        self.state = State::Connecting;
        cx.notify();
        self.connecting = Some(cx.spawn_in(window, async move |this, cx| {
            if !api::wait_online(&args.machine, 0, cx).await {
                let _ = this.update(cx, |this, cx| {
                    this.state = State::Failed(api::MACHINE_REMOVED.into());
                    cx.notify();
                });
                return;
            }
            let Ok(connector) = this.update(cx, |_, cx| api::host(cx).connector(&args.machine, cx))
            else {
                return; // 视图已放掉
            };
            let Some(connector) = connector else {
                let _ = this.update(cx, |this, cx| {
                    this.state = State::Failed(api::MACHINE_REMOVED.into());
                    cx.notify();
                });
                return;
            };
            let open = args.clone();
            // 行列先按 24×80 开，首帧布局后终端按实际大小重发。
            let terminal = cx
                .background_executor()
                .spawn(async move {
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
                        let view = cx.new(|cx| {
                            TerminalView::from_terminal(
                                cx,
                                args.machine.clone(),
                                terminal,
                                args.cwd.clone(),
                                args.session_id.clone(),
                            )
                        });
                        // 等着的时候焦点在这格上，连上了交给终端。
                        if this.focus_handle.is_focused(window) {
                            window.focus(&view.read(cx).focus_handle(), cx);
                        }
                        track(&args.machine, &args.session_id, &view, cx);
                        this.state = State::Live(view);
                    }
                    Err(error) => {
                        eprintln!("[terminal] 连不上终端会话 {}：{error:#}", args.session_id);
                        this.state = State::Failed(format!("连不上终端：{error}").into());
                    }
                }
                cx.notify();
            });
        }));
    }
}

impl PluginView for TerminalInstance {
    fn open(&mut self, args: &api::serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        if self.args.is_some() {
            return;
        }
        match api::serde_json::from_value::<TerminalArgs>(args.clone()) {
            Ok(args) => {
                self.args = Some(args);
                self.connect(window, cx);
            }
            Err(error) => {
                self.state = State::Failed(format!("终端的参数不对：{error}").into());
                cx.notify();
            }
        }
    }

    fn focus_target(&self, cx: &App) -> Option<FocusHandle> {
        Some(match &self.state {
            State::Live(view) => view.read(cx).focus_handle(),
            _ => self.focus_handle.clone(),
        })
    }
}

impl Render for TerminalInstance {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shell = div().size_full().track_focus(&self.focus_handle);
        match &self.state {
            State::Live(view) => div().size_full().child(view.clone()),
            State::Idle | State::Connecting => shell
                .flex()
                .items_center()
                .justify_center()
                .type_style(TYPE.ui)
                .text_color(rgb(ui_theme::muted()))
                .child("正在连接终端…"),
            State::Failed(message) => shell
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .type_style(TYPE.ui)
                        .text_color(rgb(ui_theme::muted()))
                        .child(message.clone()),
                )
                .when(self.args.is_some(), |this| {
                    this.child(
                        Button::new("terminal-retry")
                            .small()
                            .label("重试")
                            .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
                    )
                }),
        }
    }
}

/// 开着的终端，按（机器, 会话 id）找，[`crate::contract::SEND_TEXT`] 用。只记弱引用，实例放掉
/// 了自然失效，找的时候顺手清掉。
#[derive(Default)]
struct Open(Vec<(RuntimeKey, String, WeakEntity<TerminalView>)>);

impl Global for Open {}

fn track(machine: &RuntimeKey, session_id: &str, view: &Entity<TerminalView>, cx: &mut App) {
    let open = &mut cx.default_global::<Open>().0;
    open.retain(|(_, _, view)| view.upgrade().is_some());
    open.push((machine.clone(), session_id.to_string(), view.downgrade()));
}

/// 把文字打进那格终端，见 [`crate::contract::SEND_TEXT`]。
pub(crate) fn send_text(text: TerminalText, cx: &mut App) -> Result<(), String> {
    let view = cx.try_global::<Open>().and_then(|open| {
        open.0
            .iter()
            .filter(|(machine, session_id, _)| {
                *machine == text.machine && *session_id == text.session_id
            })
            .find_map(|(_, _, view)| view.upgrade())
    });
    let Some(view) = view else {
        return Err(format!("终端「{}」没有开着", text.session_id));
    };
    view.update(cx, |view, cx| view.send_text(&text.text, cx));
    Ok(())
}
