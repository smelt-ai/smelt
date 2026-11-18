//! 会话视图（S2）：视图 [`crate::contract::SESSION`]，露选中的那段会话——对话和终端混排的
//! 分屏，切会话整套换。旧宿主的 `stage.rs`、空状态、关闭确认搬到这里；分屏在
//! [`crate::split`]、[`crate::split_layout`]。
//!
//! 建好的会话（[`LiveSession`]）归这个视图：分屏和里面的视图实例都绑着窗口，跟视图一起在
//! 主窗口里建、在主窗口里扔，不放进全局状态。跟着走的都是事件：
//! - 打开（`OPEN_VIEW` 带 `contract::SessionArgs`）：在目录里找到就选中、建好、焦点给它；
//!   要等目录推到的记成 [`SessionView::waiting`]，见 contract 模块文档。
//! - `Sessions` 变了（换了选中）、目录推到、视图登记变了：[`SessionView::sync`] 补建选中的
//!   那段，同步、扔掉目录里没了的，兑现还在等的打开。
//! - 分屏眼下那一格换了（`SplitEvent`）：推一份进 `Sessions::active_pane`，发布活动上下文。

use rust_i18n::t;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyView, App, AppContext as _, Axis, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StyleRefinement, Styled as _,
    Subscription, Window, div, px, rgb,
};
use gpui_base::StyledExt as _;
use smelt_plugin_api::model::agent_status::AgentStatus;
use smelt_plugin_api::model::workspace_directory::{
    WorkspaceConversation, WorkspaceDirectory, WorkspaceTerminal,
};
use smelt_plugin_api::ui::dialog::Confirm;
use smelt_plugin_api::ui::kit::RADIUS;
use smelt_plugin_api::ui::window_bar::window_bar;
use smelt_plugin_api::ui::{Alert, Button, Icon, IconName, ui_theme};
use smelt_plugin_api::{self as api, Command, HostEvent, PluginView, RuntimeKey, ViewHandle};

use crate::contract::{self, SessionArgs, SessionAt};
use crate::lifecycle;
use crate::model::{self, ConversationKey, FailTarget, Located, Sessions};
use crate::split::{Split, SplitEvent};
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

// 约定：smelt.machines 的 ADD_MACHINE，权威定义在 crates/plugins/machines/src/contract.rs。
/// 去添加一台远程机器（打开远程机器插件的「远程」页）。不带参数。那个插件没装就没登记，
/// 空状态不给这个按钮。
const ADD_MACHINE: Command<(), ()> = Command::new("smelt.machines/add-machine");

/// 一段建好的会话：它的分屏，每格（对话、终端）一个视图实例。选中过一次就建，留着（切回来
/// 不重连、终端不丢屏），直到目录里没了它或机器移除。扔掉就放掉里面所有视图实例，守护里
/// 的会话照常跑（关会话是另一回事，发目录操作）。
struct LiveSession {
    split: Entity<Split>,
    _split_events: Subscription,
}

/// 会话里的一格。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pane {
    /// 守护会话 id。
    pub(crate) session_id: String,
    pub(crate) kind: PaneKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneKind {
    Terminal,
    Conversation,
}

/// 视图 [`crate::contract::SESSION`] 的建法。
pub(crate) fn build_view(window: &mut Window, cx: &mut App) -> Rc<dyn ViewHandle> {
    Rc::new(cx.new(|cx: &mut Context<SessionView>| {
        let sessions = Sessions::global(cx);
        let events = api::events(cx);
        SessionView {
            focus: cx.focus_handle(),
            offer_add_machine: offers_add_machine(cx),
            add_machine_error: None,
            live: HashMap::new(),
            waiting: None,
            _subscriptions: vec![
                cx.observe_in(&sessions, window, |view, _, window, cx| {
                    view.sync(false, window, cx);
                    cx.notify();
                }),
                cx.subscribe_in(&events, window, |view, _, event: &HostEvent, window, cx| {
                    view.on_event(event, window, cx)
                }),
            ],
        }
    }))
}

// ---- 分屏命令（S5 登记，作用在选中的会话上） ----

/// 在眼下那一格右边（`Axis::Horizontal`）或下边分出一格新终端：先写进目录，格子随
/// 目录推到建出来，焦点交给它。没有选中的会话是失败。
pub(crate) fn split_pane(axis: Axis, _window: &mut Window, cx: &mut App) -> Result<(), String> {
    let (key, split) = selected(cx)?;
    let mut conversation = directory_entry(&key, cx)?;
    let beside = split.read(cx).active_pane();
    // 新终端开在眼下那一格的目录：那一格的会话报了工作目录就用它，否则按会话的工作目录。
    let states = api::host(cx).session_states(&key.machine, cx);
    let cwd = beside
        .as_ref()
        .and_then(|pane| states.iter().find(|state| state.id == pane.session_id))
        .and_then(|state| state.cwd.clone())
        .or_else(|| model::cwd_of(&conversation).map(str::to_string));
    let session_id = uuid::Uuid::new_v4().to_string();
    conversation.terminals.push(WorkspaceTerminal {
        id: Some(session_id.clone()),
        cwd,
        ..Default::default()
    });
    split.update(cx, |split, _| {
        split.expect_pane(session_id.clone(), beside.map(|pane| pane.session_id), axis)
    });
    let target = FailTarget::Conversation(key.clone());
    crate::clear_row_failure(target.clone(), cx);
    let updated = lifecycle::update_conversation(key.machine.clone(), conversation, cx);
    cx.spawn(async move |cx| {
        if let Err(error) = updated.await {
            cx.update(|cx| {
                split.update(cx, |split, _| split.forget_pane(&session_id));
                crate::fail_row(target, t!("action.split"), error, cx);
            });
        }
    })
    .detach();
    Ok(())
}

/// 焦点移到下一格（`forward`）或上一格，到头绕回。
pub(crate) fn cycle_pane(forward: bool, window: &mut Window, cx: &mut App) -> Result<(), String> {
    let (_, split) = selected(cx)?;
    split.update(cx, |split, cx| split.cycle(forward, window, cx));
    Ok(())
}

/// 关掉眼下那一格。分屏里的一格终端只关它（从目录删掉，守护改目录前先收掉进程）；最后一格、
/// 或对话那一格，是关整段会话（对话是会话的主体，没有只剩终端的对话会话）。正在跑的
/// 先确认。
pub(crate) fn close_pane(window: &mut Window, cx: &mut App) -> Result<(), String> {
    let (key, split) = selected(cx)?;
    let conversation = directory_entry(&key, cx)?;
    let states = api::host(cx).session_states(&key.machine, cx);
    let (pane, many) = {
        let split = split.read(cx);
        (split.active_pane(), split.len() > 1)
    };
    let single = pane.filter(|pane| many && pane.kind == PaneKind::Terminal);
    let Some(pane) = single else {
        let running = conversation.status(&states) == AgentStatus::Running;
        let title = model::title(&conversation, &states);
        confirm_if(
            running,
            t!("close.session_running"),
            t!("close.session"),
            title,
            window,
            cx,
            move |cx| {
                let target = FailTarget::Conversation(key.clone());
                let closed = lifecycle::close_conversation(key, cx);
                crate::report_row_failure(target, t!("action.close_session"), closed, cx)
            },
        );
        return Ok(());
    };
    let running = states
        .iter()
        .find(|state| state.id == pane.session_id)
        .and_then(AgentStatus::from_daemon_state)
        == Some(AgentStatus::Running);
    let title = split.read(cx).pane_title(&pane.session_id, cx).to_string();
    confirm_if(
        running,
        t!("close.pane_running"),
        t!("close.pane"),
        title,
        window,
        cx,
        move |cx| close_terminal(key, pane.session_id, cx),
    );
    Ok(())
}

/// 从目录里删掉一格终端。守护改目录前先收掉它的进程（收不掉就报错、目录不动），格子
/// 随目录推回来拿掉。按眼下的目录删，确认框开着的时候目录可能又变了。
fn close_terminal(key: ConversationKey, session_id: String, cx: &mut App) {
    let target = FailTarget::Conversation(key.clone());
    let mut conversation = match directory_entry(&key, cx) {
        Ok(conversation) => conversation,
        Err(error) => return crate::fail_row(target, t!("action.close_pane"), error, cx),
    };
    let before = conversation.terminals.len();
    conversation
        .terminals
        .retain(|terminal| terminal.id.as_deref() != Some(session_id.as_str()));
    if conversation.terminals.len() == before {
        return;
    }
    let updated = lifecycle::update_conversation(key.machine, conversation, cx);
    crate::report_row_failure(target, t!("action.close_pane"), updated, cx);
}

/// `ask` 为真先弹确认（正在跑的这一轮会被终止），否则直接 `run`。
fn confirm_if(
    ask: bool,
    question: Cow<'static, str>,
    action: Cow<'static, str>,
    title: String,
    window: &mut Window,
    cx: &mut App,
    run: impl FnOnce(&mut App) + 'static,
) {
    if !ask {
        run(cx);
        return;
    }
    let run = std::cell::RefCell::new(Some(run));
    Confirm::new(question, action, move |_, cx| {
        if let Some(run) = run.borrow_mut().take() {
            run(cx);
        }
        true
    })
    .danger()
    .description(t!("close.running_description", title = title))
    .open(window, cx);
}

/// 选中的会话和它的分屏。没选中、会话视图还没建、会话还没建是失败。分屏命令在事件里调，
/// 那时会话视图没在更新，可以读。
fn selected(cx: &App) -> Result<(ConversationKey, Entity<Split>), String> {
    let key = Sessions::global(cx)
        .read(cx)
        .selection
        .session()
        .cloned()
        .ok_or_else(|| t!("error.no_selection").into_owned())?;
    let split = api::built_view(contract::SESSION, cx)
        .and_then(|view| view.view().downcast::<SessionView>().ok())
        .and_then(|view| Some(view.read(cx).live.get(&key)?.split.clone()))
        .ok_or_else(|| t!("error.not_open").into_owned())?;
    Ok((key, split))
}

/// 目录里眼下的这一条。改目录一律在它上面改，不用分屏里缓存的那份。
fn directory_entry(key: &ConversationKey, cx: &App) -> Result<WorkspaceConversation, String> {
    api::host(cx)
        .workspace_directory(&key.machine, cx)
        .and_then(|directory| {
            directory
                .conversations
                .into_iter()
                .find(|conversation| conversation.id == key.id)
        })
        .ok_or_else(|| t!("error.not_in_directory").into_owned())
}

// ---- 视图 ----

/// 会话视图：画选中的那段会话的分屏，没有就是空状态。
pub(crate) struct SessionView {
    focus: FocusHandle,
    /// 空状态给「添加远程机器」，见 [`offers_add_machine`]。命令登记、机器表变了重算，
    /// render 只读它。
    offer_add_machine: bool,
    /// 上次点「添加远程机器」没打开的原因，画在按钮下面；再点一次先清掉。
    add_machine_error: Option<SharedString>,
    /// 建好的会话，见 [`LiveSession`]。
    live: HashMap<ConversationKey, LiveSession>,
    /// 还在等目录推到的那次打开（带 `after` 的）。同一时刻只有一个，后来的打开盖掉它；
    /// 推到了兑现或作废，窗口关了跟着视图一起没。
    waiting: Option<SessionArgs>,
    _subscriptions: Vec<Subscription>,
}

impl SessionView {
    /// 空页上的「添加远程机器」：打开远程机器插件那一页，没打开的原因画在按钮下面。
    fn add_machine(&mut self, cx: &mut Context<Self>) {
        self.add_machine_error = None;
        cx.notify();
        let added = ADD_MACHINE.run(&(), cx);
        cx.spawn(async move |view, cx| {
            if let Err(error) = added.await {
                let message = t!(
                    "error.failed",
                    what = t!("action.add_machine"),
                    error = error
                );
                view.update(cx, |view, cx| {
                    view.add_machine_error = Some(message.into_owned().into());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn split(&self, cx: &App) -> Option<Entity<Split>> {
        let key = Sessions::global(cx).read(cx).selection.session()?;
        Some(self.live.get(key)?.split.clone())
    }

    fn on_event(&mut self, event: &HostEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            HostEvent::SessionsChanged => self.sync(false, window, cx),
            HostEvent::ViewsChanged => self.sync(true, window, cx),
            HostEvent::MachinesChanged => {
                self.sync(false, window, cx);
                self.refresh_offer(cx);
            }
            HostEvent::CommandsChanged => self.refresh_offer(cx),
            HostEvent::SafeAreaChanged => self.push_safe_area(cx),
            _ => {}
        }
    }

    /// 分屏铺满整个视图：视图的 safe area 原样给每段会话的分屏（没变的不会再发）。
    fn push_safe_area(&self, cx: &mut Context<Self>) {
        let area = api::safe_area(cx.entity_id(), cx);
        for live in self.live.values() {
            api::set_safe_area(&AnyView::from(live.split.clone()), area, cx);
        }
    }

    fn refresh_offer(&mut self, cx: &mut Context<Self>) {
        let offer = offers_add_machine(cx);
        if offer != self.offer_add_machine {
            self.offer_add_machine = offer;
            cx.notify();
        }
    }

    /// 打开一段会话（参数见 `contract::SessionArgs`）：目录里找到了就选中、建好、焦点给它
    /// （按守护会话 id 找的给那一格）；要等目录推到的记下；找不到报给用户。
    fn reveal(&mut self, args: SessionArgs, window: &mut Window, cx: &mut Context<Self>) {
        let host = api::host(cx);
        let directory = host.workspace_directory(&args.machine, cx);
        let style = host.path_style(&args.machine, cx);
        match model::locate(
            &args.machine,
            &args.at,
            args.after.as_ref(),
            style,
            directory.as_ref(),
        ) {
            Located::Found(key) => {
                self.waiting = None;
                lifecycle::select(key.clone(), cx);
                self.sync(false, window, cx);
                let pane = match &args.at {
                    SessionAt::Session { session_id } => Some(session_id.as_str()),
                    SessionAt::Conversation { .. } => None,
                };
                let focused = self.live.get(&key).is_some_and(|live| {
                    live.split.update(cx, |split, cx| {
                        pane.is_some_and(|id| split.focus_session(id, window, cx))
                            || split.focus_active(window, cx)
                    })
                });
                if !focused {
                    window.focus(&self.focus, cx);
                }
                cx.notify();
            }
            Located::Wait => self.waiting = Some(args),
            Located::Missing => {
                self.waiting = None;
                crate::notify_error(&t!("error.session_gone"), cx);
            }
        }
    }

    /// 跟上选中和目录：扔掉目录里没了（或不在侧栏了）、机器移除了的会话，目录里那条变了
    /// 的同步分屏（`views_changed` 时每段都同步，补建刚登记上的视图实例），补建选中的那段
    /// （这时焦点没在别处就给它），兑现还在等的打开，再把选中那段眼下那一格推进
    /// `Sessions::active_pane`。都是幂等的，多调无妨；会话状态一变就会走到这里，所以分屏
    /// 只在它那条真变了时才同步。
    fn sync(&mut self, views_changed: bool, window: &mut Window, cx: &mut Context<Self>) {
        let host = api::host(cx);
        let machines: HashSet<RuntimeKey> = host
            .machines(cx)
            .into_iter()
            .map(|machine| machine.key)
            .collect();
        let mut directories: HashMap<RuntimeKey, Option<WorkspaceDirectory>> = HashMap::new();
        let mut shown =
            |key: &ConversationKey, cx: &App| -> Option<Option<WorkspaceConversation>> {
                let directory = directories
                    .entry(key.machine.clone())
                    .or_insert_with(|| host.workspace_directory(&key.machine, cx))
                    .as_ref()?;
                let style = host.path_style(&key.machine, cx);
                Some(model::shown_conversation(style, directory, &key.id).cloned())
            };

        // 目录还没收到（刚连上、离线）的先留着；机器移除了、目录里没了的扔掉。
        let keys: Vec<ConversationKey> = self.live.keys().cloned().collect();
        for key in keys {
            match shown(&key, cx) {
                Some(Some(conversation)) => {
                    if let Some(live) = self.live.get(&key)
                        && (views_changed || live.split.read(cx).conversation() != &conversation)
                    {
                        live.split
                            .update(cx, |split, cx| split.sync(&conversation, window, cx));
                    }
                }
                Some(None) => {
                    self.live.remove(&key);
                }
                None if !machines.contains(&key.machine) => {
                    self.live.remove(&key);
                }
                None => {}
            }
        }

        if let Some(args) = self.waiting.take() {
            self.reveal(args, window, cx);
        }

        let selected = Sessions::global(cx).read(cx).selection.session().cloned();
        if let Some(key) = selected
            && !self.live.contains_key(&key)
            && let Some(Some(conversation)) = shown(&key, cx)
        {
            let (saved, start) = Sessions::global(cx).update(cx, |sessions, _| {
                (sessions.store.layout(&key), sessions.starts.remove(&key))
            });
            let split = Split::new(&key, &conversation, saved, start, window, cx);
            let split_events =
                cx.subscribe(&split, |view, _, event: &SplitEvent, cx| match event {
                    SplitEvent::ActivePaneChanged => view.push_active_pane(cx),
                });
            self.live.insert(
                key,
                LiveSession {
                    split: split.clone(),
                    _split_events: split_events,
                },
            );
            self.push_safe_area(cx);
            // 启动时恢复上次的选中走这里：焦点没在别处才给，不抢。
            if window.focused(cx).is_none() {
                split.update(cx, |split, cx| split.focus_active(window, cx));
            }
            cx.notify();
        }
        self.push_active_pane(cx);
    }

    /// 选中那段眼下那一格推进 `Sessions::active_pane`（分屏是权威，那边是只读缓存），变了
    /// 就重新发布活动上下文。
    fn push_active_pane(&self, cx: &mut Context<Self>) {
        let sessions = Sessions::global(cx);
        let active = sessions.read(cx).selection.session().and_then(|key| {
            let pane = self.live.get(key)?.split.read(cx).active_pane()?;
            Some((key.clone(), pane))
        });
        let changed = sessions.update(cx, |sessions, _| {
            if sessions.active_pane == active {
                return false;
            }
            sessions.active_pane = active;
            true
        });
        if changed {
            lifecycle::publish_context(cx);
        }
    }
}

impl Render for SessionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let split = self.split(cx);
        div()
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .track_focus(&self.focus)
            .map(|this| match split {
                // 拖窗口的顶带由分屏画：它知道顶上有没有格子的标题条。
                Some(split) => {
                    this.child(AnyView::from(split).cached(StyleRefinement::default().size_full()))
                }
                None => {
                    let area = api::safe_area(cx.entity_id(), cx);
                    this.flex_col()
                        .when(area.title_bar, |this| {
                            this.child(window_bar("sessions-empty-bar", &area))
                        })
                        .child(empty_state(
                            self.offer_add_machine,
                            self.add_machine_error.clone(),
                            cx,
                        ))
                }
            })
    }
}

/// 空状态给不给「添加远程机器」：远程机器插件登记了这条命令，而且还一台远程机器都没配。
/// 配过了就不再催，要加去设置「远程」页或命令面板。
fn offers_add_machine(cx: &App) -> bool {
    should_offer_add_machine(
        api::command(ADD_MACHINE.id(), cx).is_some(),
        api::host(cx).connections(cx).remotes.len(),
    )
}

fn should_offer_add_machine(registered: bool, remotes: usize) -> bool {
    registered && remotes == 0
}

/// 没有选中的会话。顶上是窗口标题栏时由调用方先画那一条。
fn empty_state(
    add_machine: bool,
    error: Option<SharedString>,
    cx: &mut Context<SessionView>,
) -> impl IntoElement {
    let empty = div()
        .m_auto()
        .p_6()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .text_center()
        .type_style(TYPE.ui)
        .child(
            div()
                .size_8()
                .mb_2()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(RADIUS.md))
                .bg(rgb(ui_theme::soft()))
                .child(
                    Icon::new(IconName::Coffee)
                        .size_4()
                        .text_color(rgb(ui_theme::text())),
                ),
        )
        .child(
            div()
                .font_medium()
                .child(SharedString::from(t!("empty.title"))),
        )
        .child(
            div()
                .text_color(rgb(ui_theme::muted()))
                .child(SharedString::from(t!("empty.no_session"))),
        )
        .when(add_machine, |empty| {
            empty.child(
                Button::new("empty-add-machine")
                    .primary()
                    .label(t!("empty.add_machine"))
                    .mt_2()
                    .on_click(cx.listener(|view, _, _, cx| view.add_machine(cx))),
            )
        })
        .when_some(error, |empty, error| {
            empty.child(
                div()
                    .mt_2()
                    .child(Alert::error("empty-add-machine-error", error)),
            )
        });
    div().flex_1().min_h_0().flex().child(empty)
}

#[cfg(test)]
mod tests {
    use super::should_offer_add_machine;

    #[test]
    fn add_machine_is_offered_until_a_remote_is_paired() {
        assert!(should_offer_add_machine(true, 0));
        assert!(!should_offer_add_machine(true, 1));
        // 远程机器插件没装：没有命令可执行，不给按钮。
        assert!(!should_offer_add_machine(false, 0));
    }
}

impl PluginView for SessionView {
    /// 带 `contract::SessionArgs` 是要露那段会话；不带参数（宿主建好视图时、只是露出来）
    /// 只跟上眼下的选中。
    fn open(&mut self, args: &serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        if args.is_null() {
            self.sync(false, window, cx);
            return;
        }
        match serde_json::from_value::<SessionArgs>(args.clone()) {
            Ok(args) => self.reveal(args, window, cx),
            Err(error) => crate::notify_error(&t!("error.bad_open_args", error = error), cx),
        }
    }

    /// 键盘焦点落在选中会话眼下那一格；没有会话落在视图自己身上。
    fn focus_target(&self, cx: &App) -> Option<FocusHandle> {
        self.split(cx)
            .and_then(|split| split.read(cx).focus_target(cx))
            .or_else(|| Some(self.focus.clone()))
    }
}
