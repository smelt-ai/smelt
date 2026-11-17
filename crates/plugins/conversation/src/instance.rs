//! 一段对话的实例视图（[`crate::contract::VIEW`]）：摆对话的一方按 [`ConversationArgs`] 建一份，
//! 这里按工作区目录里那条（或调用方给的 `spec`）接上守护里的会话，换成 [`ConversationView`]。
//!
//! 视图要别处做的事（开新对话、分叉、跳到别的对话、去智能体页和能力库、记住会话选项、
//! 记忆卡片的采纳和删除、看大图）都在这里按约定转出去，视图自己不认识别的插件。
//!
//! 工作区里的对话还浮着一颗标题胶囊：写对话的名字，最近一问整条滚出视口后换成那一问，
//! 按下滚回去。自动化运行的现场这类嵌在别处的不画。
//!
//! 放掉实例只是断开这段画面，守护里的会话还在；结束会话是运行时的操作，由摆它的一方发。

use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, Global, MouseButton, SharedString, Stateful,
    Subscription, Task, WeakEntity, Window, div, px, rgb,
};
use smelt_plugin_api::model::agent_kind::ConversationAgentKind;
use smelt_plugin_api::model::control_api::{
    AgentMemoryApprove, AgentMemoryDelete, AgentMemoryFileParams,
};
use smelt_plugin_api::model::session_control::is_background_acp_session_id;
use smelt_plugin_api::model::workspace_directory::WorkspaceAcp;
use smelt_plugin_api::ui::Button;
use smelt_plugin_api::ui::tooltip::Tooltip;
use smelt_plugin_api::ui::truncated_line::truncated_line;
use smelt_plugin_api::ui::ui_theme;
use smelt_plugin_api::ui::window_bar::with_window_drag;
use smelt_plugin_api::{
    self as api, HostEvent, PluginView, Runtime, RuntimeKey, ViewHandle, ViewSpec,
};

use crate::contract::{
    AT_LATEST_CHANGED, AtLatest, ConversationArgs, ConversationSpec, ConversationStart,
    ConversationText, VIEW,
};
use crate::sessions;
use crate::view::{
    ConversationOrigin, ConversationView, ConversationViewEvent, LaunchPrep, MemoryAction,
    MemoryActionKind,
};
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

/// 实例视图的登记项。
pub(crate) fn spec() -> ViewSpec {
    ViewSpec {
        // 来源内的名字，跟 `Command::name` 一样从约定里的 id 取。
        name: VIEW.split_once('/').map_or(VIEW, |(_, name)| name).into(),
        title: "对话".into(),
        icon: crate::ICON.into(),
        // 一段对话只画消息流和输入栏，顶栏归摆它的一方。
        draws_top_bar: false,
        instances: true,
        settings: None,
        build: Rc::new(|_, cx| Rc::new(cx.new(ConversationInstance::new)) as Rc<dyn ViewHandle>),
    }
}

/// 一段对话。还没接上时画等待或失败，接上以后整块交给 [`ConversationView`]。
struct ConversationInstance {
    /// 第一次 `open` 给的参数。实例按参数建，之后再 `open` 不换对话。
    args: Option<ConversationArgs>,
    state: State,
    /// 还没接上时接住焦点，接上以后转给对话。
    focus_handle: FocusHandle,
    /// 标题胶囊上的名字，会话状态、机器变了时重算；`None` 不画胶囊。
    title: Option<String>,
    _events: Option<Subscription>,
    _view: Vec<Subscription>,
}

enum State {
    /// 等 `open` 给参数。
    Idle,
    /// 工作区目录里还没有这一条，等它推到。
    Waiting,
    Live(Entity<ConversationView>),
    /// 接不上：参数不对、机器移除了。给人看的原因。
    Failed(SharedString),
}

impl ConversationInstance {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            args: None,
            state: State::Idle,
            focus_handle: cx.focus_handle(),
            title: None,
            _events: None,
            _view: Vec::new(),
        }
    }

    /// 接上对话：按 `spec` 或工作区目录里那条建视图。目录里还没有就等下一次会话变化。
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(args) = self.args.clone() else {
            return;
        };
        let Some(client) = api::host(cx).client(&args.machine, cx) else {
            self.state = State::Failed(api::MACHINE_REMOVED.into());
            cx.notify();
            return;
        };
        let runtime = Runtime::new(args.machine.clone(), client);
        let view = match &args.spec {
            Some(spec) => Some(embedded_view(runtime, &args, spec, window, cx)),
            None => directory_acp(&args, cx)
                .map(|acp| directory_view(runtime, &args, acp, args.start.clone(), window, cx)),
        };
        let Some(view) = view else {
            self.state = State::Waiting;
            cx.notify();
            return;
        };
        self._view = vec![
            cx.subscribe_in(&view, window, Self::on_view_event),
            // 标题胶囊读对话的吸顶提问和模型。
            cx.observe(&view, |_, _, cx| cx.notify()),
        ];
        if self.focus_handle.is_focused(window) {
            window.focus(&view.read(cx).focus_handle(cx), cx);
        }
        track(&args.machine, &args.session_id, &view, cx);
        self.state = State::Live(view);
        self.refresh_title(cx);
        cx.notify();
    }

    fn on_host_event(&mut self, event: &HostEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            HostEvent::SessionsChanged | HostEvent::MachinesChanged => {
                if matches!(self.state, State::Waiting) {
                    self.connect(window, cx);
                } else {
                    self.refresh_title(cx);
                }
            }
            _ => {}
        }
    }

    /// 重算标题胶囊上的名字：会话状态里运行时算好的展示名，远程机器后面带上机器名。只给
    /// 工作区里的普通对话画，自动化、后台对话这类嵌在别处的不画。
    fn refresh_title(&mut self, cx: &mut Context<Self>) {
        let title = self.args.as_ref().and_then(|args| {
            if args.spec.is_some() || is_background_acp_session_id(&args.session_id) {
                return None;
            }
            let acp = directory_acp(args, cx)?;
            if acp.automation_id.is_some() {
                return None;
            }
            let host = api::host(cx);
            let name = host
                .session_states(&args.machine, cx)
                .into_iter()
                .find(|state| state.id == args.session_id)
                .and_then(|state| state.conversation_name)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| "对话".to_string());
            Some(match &args.machine {
                RuntimeKey::Local => name,
                RuntimeKey::Remote(id) => {
                    let machine_name = host
                        .machines(cx)
                        .into_iter()
                        .find(|known| known.key == args.machine)
                        .map_or_else(|| id.clone(), |known| known.name);
                    format!("{name} · {machine_name}")
                }
            })
        });
        if self.title != title {
            self.title = title;
            cx.notify();
        }
    }

    fn on_view_event(
        &mut self,
        view: &Entity<ConversationView>,
        event: &ConversationViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(args) = self.args.clone() else {
            return;
        };
        let machine = args.machine;
        match event {
            ConversationViewEvent::ConfigSelected {
                config_id,
                value_id,
            } => {
                // 按 agent 记住这次选择，下一段新对话沿用。
                let agent = view.read(cx).agent_kind();
                let (config_id, value_id) = (config_id.clone(), value_id.clone());
                let edit = crate::agents::edit_settings(
                    &machine,
                    move |settings| settings.remember_acp_config_value(agent, config_id, value_id),
                    cx,
                );
                log_failure(edit, "记住会话选项失败", cx);
            }
            ConversationViewEvent::PreviewImage(image) => preview_image(image.clone(), window, cx),
            ConversationViewEvent::NewSession => new_session(machine, view, cx),
            ConversationViewEvent::ForkConversation {
                provider_session_id,
                cut,
                prompt,
            } => {
                let fork = sessions::FORK.run(
                    &sessions::Fork {
                        machine,
                        session_id: args.session_id,
                        provider_session_id: provider_session_id.clone(),
                        cut: cut.clone(),
                        prompt: prompt.clone(),
                    },
                    cx,
                );
                log_failure(fork, "分叉失败", cx);
            }
            ConversationViewEvent::ViewingHistoryChanged => {
                let at_latest = !view.read(cx).is_viewing_history();
                AT_LATEST_CHANGED.emit(
                    &AtLatest {
                        machine,
                        session_id: args.session_id,
                        at_latest,
                    },
                    cx,
                );
            }
            ConversationViewEvent::OpenConversation { sid } => {
                let activate = sessions::ACTIVATE.run(
                    &sessions::Activate {
                        machine,
                        session_id: sid.clone(),
                    },
                    cx,
                );
                log_failure(activate, "切到那段对话失败", cx);
            }
            ConversationViewEvent::ManageCapabilities { by_agent: false } => open_view(
                sessions::CAPABILITIES_VIEW,
                api::serde_json::json!({
                    "tab": "plugins",
                    "machine": api::machine_to_json(&machine),
                }),
                cx,
            ),
            ConversationViewEvent::ManageCapabilities { by_agent: true } => {
                let mut args =
                    api::serde_json::json!({ "machine": api::machine_to_json(&machine) });
                if let Some(agent) = view.read(cx).agent_definition_id() {
                    args["agent"] = agent.into();
                }
                open_view(crate::agents::VIEW, args, cx);
            }
            ConversationViewEvent::OpenAgentMemory { agent_id } => open_view(
                crate::agents::VIEW,
                api::serde_json::json!({
                    "machine": api::machine_to_json(&machine),
                    "agent": agent_id,
                    "tab": "memory",
                }),
                cx,
            ),
            ConversationViewEvent::MemoryAction(action) => {
                run_memory_action(&machine, view.downgrade(), action.clone(), cx)
            }
        }
    }
}

impl PluginView for ConversationInstance {
    fn open(&mut self, args: &api::serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        if self.args.is_some() {
            return;
        }
        match api::serde_json::from_value::<ConversationArgs>(args.clone()) {
            Ok(args) => {
                self.args = Some(args);
                crate::agents::ensure_loaded(cx);
                self._events = Some(cx.subscribe_in(
                    &api::events(cx),
                    window,
                    |this, _, event, window, cx| this.on_host_event(event, window, cx),
                ));
                self.connect(window, cx);
            }
            Err(error) => {
                self.state = State::Failed(format!("对话的参数不对：{error}").into());
                cx.notify();
            }
        }
    }

    fn focus_target(&self, cx: &App) -> Option<FocusHandle> {
        Some(match &self.state {
            State::Live(view) => view.read(cx).focus_handle(cx),
            _ => self.focus_handle.clone(),
        })
    }
}

impl Render for ConversationInstance {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shell = div().size_full().track_focus(&self.focus_handle);
        match &self.state {
            State::Live(view) => {
                // 顶上是窗口标题栏时，按住名字拖窗口。
                let drag = smelt_plugin_api::safe_area(cx.entity_id(), cx).title_bar;
                let island = self
                    .title
                    .clone()
                    .map(|title| title_island(title, view, drag, cx));
                div()
                    .relative()
                    .size_full()
                    .child(view.clone())
                    .children(island)
            }
            State::Idle | State::Waiting => {
                let text = self
                    .args
                    .as_ref()
                    .and_then(|args| args.placeholder.clone())
                    .unwrap_or_else(|| "正在打开对话…".to_string());
                shell
                    .flex()
                    .items_center()
                    .justify_center()
                    .type_style(TYPE.ui)
                    .text_color(rgb(ui_theme::muted()))
                    .child(text)
            }
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
                        Button::new("conversation-retry")
                            .small()
                            .label("重试")
                            .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
                    )
                }),
        }
    }
}

/// 工作区目录里这段对话的那条。目录还没收到、里面没有它是 `None`。
fn directory_acp(args: &ConversationArgs, cx: &App) -> Option<WorkspaceAcp> {
    api::host(cx)
        .workspace_directory(&args.machine, cx)?
        .conversations
        .into_iter()
        .filter_map(|conversation| conversation.acp)
        .find(|acp| acp.sid == args.session_id)
}

/// 按工作区目录里那条接上：守护里还活着就是廉价的 attach，不在了按目录续上。`start` 是
/// 调用方刚写进目录、要新拉起的。
fn directory_view(
    runtime: Runtime,
    args: &ConversationArgs,
    acp: WorkspaceAcp,
    start: Option<ConversationStart>,
    window: &mut Window,
    cx: &mut Context<ConversationInstance>,
) -> Entity<ConversationView> {
    let agent = ConversationAgentKind::from_id(&acp.agent)
        .or_else(|| ConversationAgentKind::from_command_loose(&acp.launch.command))
        .unwrap_or(ConversationAgentKind::Claude);
    // 会话自己的选项盖在这个 agent 记住的选择上；智能体配置还没收到时只用会话自己的。
    let config_values = match crate::agents::settings(&args.machine, cx) {
        Some(settings) => settings.initial_acp_config(agent, &acp.config_values),
        None => acp.config_values.clone(),
    };
    let reason = args
        .placeholder
        .clone()
        .unwrap_or_else(|| "正在恢复上次的对话…".to_string());
    let view = cx.new(|cx| {
        ConversationView::placeholder(
            cx,
            ConversationOrigin {
                runtime,
                agent,
                launch: acp.launch,
                // 第一次连上前由运行时补齐：新拉起的按当前定义组装；恢复的完好快照原样沿用，
                // 只补记忆目录。
                launch_prep: Some(LaunchPrep {
                    agent_definition_id: acp.agent_definition_id,
                    restore: start.is_none(),
                    restart: None,
                }),
                profile_id: acp.profile_id,
                cwd: acp.cwd,
                reason,
                entries: Vec::new(),
                resume_session_id: acp.history_session_id,
                saved_sid: Some(acp.sid),
            },
        )
    });
    view.update(cx, |view, cx| {
        view.restore_config_values(config_values);
        view.restore_pending_prompt(acp.pending_prompt);
        view.restore_pending_agent_preset(acp.pending_agent_preset);
        match start {
            Some(start) => view.launch_new(
                start.fork.map(|fork| (fork.provider_session_id, fork.cut)),
                start.prefill,
                window,
                cx,
            ),
            None => view.maybe_auto_resume(window, cx),
        }
    });
    view
}

/// 不在工作区目录里的对话（自动化运行的现场）：接上守护里正在跑的那段，不按规格重新起。
fn embedded_view(
    runtime: Runtime,
    args: &ConversationArgs,
    spec: &ConversationSpec,
    window: &mut Window,
    cx: &mut Context<ConversationInstance>,
) -> Entity<ConversationView> {
    let agent =
        ConversationAgentKind::from_id(&spec.agent).unwrap_or(ConversationAgentKind::Claude);
    let view = cx.new(|cx| {
        ConversationView::placeholder(
            cx,
            ConversationOrigin {
                runtime,
                agent,
                launch: agent.default_launch(),
                launch_prep: None,
                profile_id: None,
                cwd: Some(spec.cwd.clone()),
                reason: args.placeholder.clone().unwrap_or_default(),
                entries: Vec::new(),
                resume_session_id: None,
                saved_sid: Some(args.session_id.clone()),
            },
        )
    });
    view.update(cx, |view, cx| {
        view.restore_conversation_binding(spec.binding.clone());
        view.maybe_auto_resume(window, cx);
    });
    view
}

/// 输入框里的 `/clear`：照这段对话另开一段新的。跟智能体开的按智能体开（模型、启动命令按
/// 它现算）；裸引擎的按同一个 agent 和自定义 Agent 开。
fn new_session(machine: RuntimeKey, view: &Entity<ConversationView>, cx: &mut App) {
    let view = view.read(cx);
    let Some(cwd) = view.cwd() else {
        return;
    };
    let with = match view.agent_definition_id() {
        Some(agent_definition_id) => sessions::StartWith::Agent {
            agent_definition_id: agent_definition_id.to_string(),
        },
        None => sessions::StartWith::Engine {
            agent: view.agent_kind().id().to_string(),
            profile_id: view.profile_id().map(str::to_string),
        },
    };
    let task =
        sessions::START_CONVERSATION.run(&sessions::StartConversation { machine, cwd, with }, cx);
    log_failure(task, "开新对话失败", cx);
}

/// 记忆卡片上的采纳、删除。成功后卡片随守护推来的快照更新，这里只把结果交回视图。
fn run_memory_action(
    machine: &RuntimeKey,
    view: WeakEntity<ConversationView>,
    action: MemoryAction,
    cx: &mut App,
) {
    let params = AgentMemoryFileParams {
        agent_id: action.agent_id,
        layer: action.layer,
        file_name: action.file_name,
    };
    // 两个调用都只回一个空结果，任务同型。
    let (task, failure) = match action.kind {
        MemoryActionKind::Approve => (
            api::call::<AgentMemoryApprove>(machine, params, api::CALL_TIMEOUT, cx),
            "采纳失败",
        ),
        MemoryActionKind::Delete => (
            api::call::<AgentMemoryDelete>(machine, params, api::CALL_TIMEOUT, cx),
            "删除失败",
        ),
    };
    cx.spawn(async move |cx| {
        let error = task.await.err().map(|error| format!("{failure}：{error}"));
        let _ = view.update(cx, |view, cx| view.finish_memory_action(error, cx));
    })
    .detach();
}

/// 大图预览：尽量占满窗口的对话框，图片按比例放进去。
fn preview_image(image: Arc<gpui::Image>, window: &mut Window, cx: &mut App) {
    // 解码进按图片 id 索引的缓存，完成时刷新窗口。
    api::ui::image::ensure_preview(&image, cx);
    api::ui::dialog::open(window, cx, move |card, window, _| {
        // 对话框会把宽高夹在窗口减去边距以内，这里只表达「尽量大」。
        let viewport = window.viewport_size();
        card.w(viewport.width)
            .h(viewport.height)
            .child(api::ui::image::image_surface(&image))
    });
}

fn open_view(view: &str, args: api::serde_json::Value, cx: &mut App) {
    let open = api::OPEN_VIEW.run(
        &api::OpenView {
            view: view.to_string(),
            args,
        },
        cx,
    );
    log_failure(open, "打开页面失败", cx);
}

/// 不等结果的命令：失败只记日志（对方插件没装、主窗口关着）。
fn log_failure<R: 'static>(task: Task<Result<R, String>>, what: &'static str, cx: &mut App) {
    cx.spawn(async move |_| {
        if let Err(error) = task.await {
            eprintln!("[conversation] {what}：{error}");
        }
    })
    .detach();
}

// ---- 标题胶囊 ----

/// 静止宽度。胶囊浮在内容第一行正中，只能留一个短名字。
const TITLE_ISLAND_MAX_WIDTH: f32 = 200.;

/// 吸顶提问要认得出是哪一问，比名字宽。
const PROMPT_ISLAND_MAX_WIDTH: f32 = 480.;

/// 浮在对话顶上正中的胶囊：平时是名字（`drag` 时按住拖窗口），最近一问滚出视口后换成
/// 那一问（按下滚回去）。悬停说完整身份：名字 · 模型。不占布局高度。
fn title_island(title: String, view: &Entity<ConversationView>, drag: bool, cx: &App) -> gpui::Div {
    let conversation = view.read(cx);
    let tip = match conversation.model_name() {
        Some(model) if !model.is_empty() => format!("{title} · {model}"),
        _ => title.clone(),
    };
    let island = match conversation.pinned_prompt() {
        Some(prompt) => {
            let view = view.clone();
            island_shell("conversation-pinned-prompt", PROMPT_ISLAND_MAX_WIDTH, tip)
                .gap_1p5()
                .child(
                    div()
                        .flex_shrink_0()
                        .type_style(TYPE.ui)
                        .text_color(rgb(ui_theme::muted()))
                        .child("↑"),
                )
                .child(
                    div()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .type_style(TYPE.ui)
                        .text_color(rgb(ui_theme::text()))
                        .child(truncated_line(prompt)),
                )
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    cx.stop_propagation();
                    view.update(cx, |view, cx| view.reveal_pinned_prompt(cx));
                })
        }
        None => {
            let island = island_shell("conversation-title", TITLE_ISLAND_MAX_WIDTH, tip).child(
                div()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .type_style(TYPE.ui_medium)
                    .text_color(rgb(ui_theme::text()))
                    .child(truncated_line(title)),
            );
            if drag {
                with_window_drag(island)
            } else {
                island
            }
        }
    };
    div()
        .absolute()
        .top(px(ui_theme::CHROME_ISLAND_TOP_PX))
        .left_0()
        .right_0()
        .h(px(ui_theme::CHROME_ISLAND_HEIGHT_PX))
        .flex()
        .items_center()
        .justify_center()
        .child(island)
}

fn island_shell(id: &'static str, max_width: f32, tip: String) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(ui_theme::CHROME_ISLAND_HEIGHT_PX))
        .max_w(px(max_width))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        // 浮层实色：平时一档卡片底，指上去再抬到悬停色。
        .bg(rgb(ui_theme::surface()))
        .border_1()
        .border_color(ui_theme::hair())
        .hover(|island| island.bg(rgb(ui_theme::soft())))
        .shadow_lg()
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
}

// ---- 开着的对话 ----

/// 开着的对话，按（机器, 会话 id）找，[`crate::contract::INSERT_TEXT`] 用。只记弱引用，实例
/// 放掉了自然失效，记新的时顺手清掉。
#[derive(Default)]
struct Open(Vec<(RuntimeKey, String, WeakEntity<ConversationView>)>);

impl Global for Open {}

fn track(machine: &RuntimeKey, session_id: &str, view: &Entity<ConversationView>, cx: &mut App) {
    let open = &mut cx.default_global::<Open>().0;
    open.retain(|(_, _, view)| view.upgrade().is_some());
    open.push((machine.clone(), session_id.to_string(), view.downgrade()));
}

/// 把文字填进那段对话的输入框，见 [`crate::contract::INSERT_TEXT`]。
pub(crate) fn insert_text(
    text: ConversationText,
    window: Option<&mut Window>,
    cx: &mut App,
) -> Result<(), String> {
    let view = cx.try_global::<Open>().and_then(|open| {
        open.0
            .iter()
            .filter(|(machine, session_id, _)| {
                *machine == text.machine && *session_id == text.session_id
            })
            .find_map(|(_, _, view)| view.upgrade())
    });
    let Some(view) = view else {
        return Err(format!("对话「{}」没有开着", text.session_id));
    };
    let Some(window) = window else {
        return Err("主窗口没有打开".to_string());
    };
    view.update(cx, |view, cx| {
        view.insert_prompt_text(&text.text, window, cx);
        view.focus_input(window, cx);
    });
    Ok(())
}
