//! 智能体：每台机器上的智能体定义（指令、引擎与模型、Pi 插件与 MCP、上下文、记忆），
//! 和跟智能体开新对话的入口。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 和宿主打交道，宿主里没有它的类型。视图 id 是
//! [`VIEW_ID`]，打开时的参数都可以不给：
//! `{"machine": …, "agent": "<id>", "tab": "memory", "new_conversation": true}`。
//! `agent` 打开那个智能体的编辑页，`tab` 是停在哪个页签；`new_conversation` 是从「新对话」
//! 过来的，还没选默认智能体（见 [`NEW_CONVERSATION`]）。
//!
//! 命令 [`NEW_CONVERSATION`]：跟默认智能体开一段新对话；没有默认就来这页选，一个智能体都
//! 没有就来这页建，建好了点「开始对话」才开。
//!
//! 每个智能体挂着几条自动化看的是运行时推来的 `automations.changed`，不靠自动化插件，
//! 见 `automations.rs`。挂着自动化不让删由运行时判定。去能力库按视图 id 打开，没装就没有
//! 那个入口。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use api::model::agent_definition::AgentDefinition;
use api::model::agent_kind::ConversationAgentKind;
use api::model::daemon_state::{
    AgentMemoryChanged, CORE_TOPIC_AGENT_MEMORY_CHANGED, CORE_TOPIC_PI_CONFIG_CHANGED,
};
use api::serde_json;
use api::ui::IconAction as _;
use api::ui::menu::{Menu, MenuItem, MenuTrigger};
use api::ui::truncated_line::truncated_line;
use api::ui::{
    Badge, Button, Icon, IconName, ListRow, Segment, Segmented, Switch, TextArea, TextInput,
    ui_theme,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_base::StyledExt as _;
use smelt_plugin_api::ui::field::{InputEvent, InputState, TextareaState};
use smelt_plugin_api::{
    self as api, CommandSpec, Handled, HostEvent, PluginView, RuntimeKey, TopicMessage, ViewHandle,
    ViewSpec,
};

mod agent_hosts;
mod automations;
mod cards;
mod contract;
mod editor;
mod model;
mod quota;
mod quota_view;
mod settings_page;
mod view;

#[cfg(test)]
mod tests;

use cards::*;
use editor::*;
use model::*;

const OWNER: &str = "smelt.agents";
const VIEW_ID: &str = contract::VIEW;
/// 跟默认智能体开一段新对话。
const NEW_CONVERSATION: &str = contract::NEW_CONVERSATION.id();
/// 能力库的视图。没装就没有「管理插件 / MCP」的入口。
// 约定：smelt.capabilities 的 VIEW，权威定义在 crates/plugins/capabilities/src/contract.rs。
const CAPABILITIES_VIEW: &str = "smelt.capabilities/main";

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    api::register_views(
        OWNER,
        vec![
            ViewSpec {
                name: "main".into(),
                title: "智能体".into(),
                icon: IconName::Bot.path(),
                draws_top_bar: false,
                instances: false,
                settings: None,
                build: Rc::new(|_window, cx| {
                    Rc::new(cx.new(AgentsPage::new)) as Rc<dyn ViewHandle>
                }),
            },
            settings_page::view_spec(),
            quota_view::view_spec(),
        ],
        cx,
    )
    .expect("智能体的视图表写错了");
    agent_hosts::install(cx);
    quota::install(cx);
    automations::install(cx);
    register_commands(new_conversation_title(None), cx);
    sync_new_conversation_title(cx);
    cx.observe_global::<agent_hosts::AgentHosts>(sync_new_conversation_title)
        .detach();
    let events = api::events(cx);
    cx.subscribe(&events, |_, event, cx| {
        if matches!(event, HostEvent::MachinesChanged) {
            sync_new_conversation_title(cx);
        }
    })
    .detach();
}

/// 登记本插件的全部命令。登记是整套替换，所以「新对话」改标题时也要把别的一起带上。
fn register_commands(new_conversation_title: String, cx: &mut App) {
    let mut commands = vec![CommandSpec {
        name: contract::NEW_CONVERSATION.name().to_string(),
        title: new_conversation_title,
        key: Some("cmd-n".to_string()),
        context: "Workspace".to_string(),
        palette: true,
        requires_args: false,
        handler: Rc::new(|_, _, cx| new_conversation(cx)),
    }];
    commands.extend(agent_hosts::commands());
    commands.extend(quota::commands());
    api::register_commands(OWNER, "智能体", commands, cx).expect("智能体的命令登记不上");
}

/// 默认智能体换了、改名了、配置读到了，「新对话」的标题跟着换。标题没变不重新登记：
/// 登记一变宿主就重装键表和菜单。
fn sync_new_conversation_title(cx: &mut App) {
    let draft = built_page(cx).and_then(|page| page.read(cx).conversation_draft.clone());
    let plan = plan_new_conversation(draft.as_ref(), cx);
    let title = new_conversation_title(plan.default_name.as_deref());
    if api::command(NEW_CONVERSATION, cx).is_some_and(|command| command.title != title) {
        register_commands(title, cx);
    }
}

/// 「新对话」：有默认就开；没有默认去智能体页选；一个都没有去建，建好的先当草稿，点
/// 「开始对话」才开、才设成默认。
fn new_conversation(cx: &mut App) -> Handled {
    let page = built_page(cx);
    let draft = page
        .as_ref()
        .and_then(|page| page.read(cx).conversation_draft.clone());
    let plan = plan_new_conversation(draft.as_ref(), cx);
    for runtime in &plan.stale_defaults {
        clear_default_conversation_agent(runtime, cx);
    }
    let open_args = match plan.intent {
        NewConversationIntent::Start(agent) => {
            if let Some(page) = &page {
                page.update(cx, |page, cx| {
                    page.discard_uncommitted_draft(Some(&agent), cx)
                });
            }
            let started = start_conversation(agent, cx);
            return Handled::Done(
                cx.spawn(async move |_| started.await.map(|()| serde_json::Value::Null)),
            );
        }
        NewConversationIntent::PickAgent => serde_json::json!({ "new_conversation": true }),
        NewConversationIntent::CreateAgent => {
            // 已经有草稿就回到它，不再建一个。
            let no_agents = agent_machines(cx)
                .iter()
                .all(|machine| agents_on(machine, cx).is_empty());
            serde_json::json!({ "new_conversation": draft.is_none() && no_agents })
        }
    };
    let opened = api::OPEN_VIEW.run(
        &api::OpenView {
            view: VIEW_ID.to_string(),
            args: open_args,
        },
        cx,
    );
    Handled::Done(cx.spawn(async move |_| opened.await.map(|_| serde_json::Value::Null)))
}

/// 已经建好的智能体页；还没打开过是 `None`。
fn built_page(cx: &App) -> Option<Entity<AgentsPage>> {
    api::built_view(VIEW_ID, cx)?
        .view()
        .downcast::<AgentsPage>()
        .ok()
}

pub(crate) struct AgentsPage {
    pub(crate) focus_handle: FocusHandle,
    /// 正在编辑的智能体；`None` 是目录。
    pub(crate) editor: Option<AgentEditor>,
    pub(crate) error: Option<String>,
    /// 「新对话」时一个智能体都没有，建出来的那个：点「开始对话」前都算草稿，返回就删掉。
    pub(crate) conversation_draft: Option<AgentRef>,
    /// 从「新对话」过来、一个智能体都没有：下一个建出来的当草稿。
    pub(crate) arm_conversation_on_create: bool,
    /// 从「新对话」过来、还没选默认智能体：选择器里选了就开对话。
    pub(crate) picking_for_conversation: bool,
    /// 已经请运行时监听记忆目录的智能体。重新连上后运行时那边的监听没了，清空重登。
    pub(crate) watched_memories: HashSet<AgentRef>,
    /// 编辑器那台机器上各会话上次是不是停在两步之间，见 [`Self::on_sessions_changed`]。
    session_settled: HashMap<String, bool>,
    _host_events: Subscription,
    _automations: Subscription,
    _agent_hosts: Subscription,
}

impl AgentsPage {
    fn new(cx: &mut Context<Self>) -> Self {
        let events = api::events(cx);
        let host_events = cx.subscribe(&events, |this, _, event: &HostEvent, cx| {
            this.on_host_event(event, cx);
        });
        Self {
            focus_handle: cx.focus_handle(),
            editor: None,
            error: None,
            conversation_draft: None,
            arm_conversation_on_create: false,
            picking_for_conversation: false,
            watched_memories: HashSet::new(),
            session_settled: HashMap::new(),
            _host_events: host_events,
            _automations: cx.observe_global::<automations::AgentAutomations>(|_, cx| cx.notify()),
            _agent_hosts: cx.observe_global::<agent_hosts::AgentHosts>(Self::on_agents_changed),
        }
    }

    fn on_host_event(&mut self, event: &HostEvent, cx: &mut Context<Self>) {
        match event {
            HostEvent::RuntimeTopic {
                machine,
                topic,
                message,
            } => self.on_runtime_topic(machine, topic, message, cx),
            HostEvent::Reconnected(machine) => {
                // 运行时那边替我们建的记忆监听没了；断开期间也可能错过了推送。
                self.watched_memories
                    .retain(|agent| agent.runtime != *machine);
                if self.editor_machine() == Some(machine) {
                    self.refresh_agent_memories(cx);
                    self.refresh_agent_pi_config(cx);
                }
            }
            HostEvent::SessionsChanged => self.on_sessions_changed(cx),
            HostEvent::MachinesChanged => self.on_agents_changed(cx),
            HostEvent::ShownViewsChanged => {
                // 离开这页就是不要草稿了。
                if !api::host(cx).view_shown(VIEW_ID, cx) {
                    self.discard_uncommitted_draft(None, cx);
                    self.picking_for_conversation = false;
                }
            }
            HostEvent::ProjectChanged { .. }
            | HostEvent::WindowActivated
            | HostEvent::OverlayChanged
            | HostEvent::ActiveContextChanged
            | HostEvent::NotificationPermissionChanged
            | HostEvent::LaidOutViewsChanged
            | HostEvent::ViewsChanged
            | HostEvent::ViewBadgesChanged
            | HostEvent::SafeAreaChanged
            | HostEvent::ThemesChanged
            | HostEvent::LayoutsChanged
            | HostEvent::PagesChanged
            | HostEvent::CommandsChanged
            | HostEvent::Plugin { .. }
            | HostEvent::OpenPaths { .. } => {}
        }
    }

    fn editor_machine(&self) -> Option<&RuntimeKey> {
        self.editor.as_ref().map(|editor| &editor.agent.runtime)
    }

    fn on_runtime_topic(
        &mut self,
        machine: &RuntimeKey,
        topic: &str,
        message: &TopicMessage,
        cx: &mut Context<Self>,
    ) {
        match topic {
            CORE_TOPIC_PI_CONFIG_CHANGED => {
                if self.editor_machine() == Some(machine) {
                    self.refresh_agent_pi_config(cx);
                }
            }
            CORE_TOPIC_AGENT_MEMORY_CHANGED => {
                let affected = match message {
                    TopicMessage::Event(payload) => {
                        match serde_json::from_value::<AgentMemoryChanged>(payload.clone()) {
                            Ok(changed) => self.editor.as_ref().is_some_and(|editor| {
                                editor.agent.runtime == *machine
                                    && changed.agent_id.is_none_or(|id| id == editor.agent.id)
                            }),
                            Err(error) => {
                                eprintln!("[agents] 记忆变化的推送认不出：{error}");
                                self.editor_machine() == Some(machine)
                            }
                        }
                    }
                    // 可能错过了推送。
                    TopicMessage::Snapshot(_) | TopicMessage::Lagged => {
                        self.editor_machine() == Some(machine)
                    }
                    // 这一轮不会再推，编辑器里的记忆照旧，手动刷新还能重扫。
                    TopicMessage::Unavailable(_) => false,
                };
                if affected {
                    self.refresh_agent_memories(cx);
                }
            }
            _ => {}
        }
    }

    /// 记忆的使用次数和待确认状态存在主库，是智能体在一轮里写完文件之后才记的，目录监听
    /// 看不到。编辑器那台机器上有对话刚停下来时这些已经落库，重扫一次。
    fn on_sessions_changed(&mut self, cx: &mut Context<Self>) {
        let Some(machine) = self.editor_machine().cloned() else {
            self.session_settled.clear();
            return;
        };
        let settled: HashMap<String, bool> = api::host(cx)
            .session_states(&machine, cx)
            .into_iter()
            .map(|state| {
                let settled = session_phase_settled(state.effective_phase());
                (state.id, settled)
            })
            .collect();
        let just_settled = settled.iter().any(|(id, settled)| {
            *settled && self.session_settled.get(id).is_some_and(|before| !before)
        });
        self.session_settled = settled;
        if just_settled {
            self.refresh_agent_memories(cx);
        }
    }

    /// 智能体配置或机器变了：编辑器里的智能体没了就回目录。
    fn on_agents_changed(&mut self, cx: &mut Context<Self>) {
        let machines: HashSet<RuntimeKey> = api::host(cx)
            .machines(cx)
            .into_iter()
            .map(|machine| machine.key)
            .collect();
        self.watched_memories
            .retain(|agent| machines.contains(&agent.runtime));
        // 配置还没读到（机器刚断开重连）时先留着，读到了再判断。
        let gone = |agent: &AgentRef, cx: &App| {
            !machines.contains(&agent.runtime)
                || agent_hosts::settings(&agent.runtime, cx)
                    .is_some_and(|config| !config.agents.iter().any(|it| it.id == agent.id))
        };
        if self
            .conversation_draft
            .as_ref()
            .is_some_and(|draft| gone(draft, cx))
        {
            self.conversation_draft = None;
        }
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| gone(&editor.agent, cx))
        {
            self.close_editor(cx);
        }
        cx.notify();
    }
}

impl PluginView for AgentsPage {
    fn open(&mut self, args: &serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        let new_conversation = args
            .get("new_conversation")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        self.picking_for_conversation = new_conversation;
        if new_conversation {
            // 一个智能体都没有（命令判断过）：下一个建出来的当草稿。
            self.arm_conversation_on_create = agent_machines(cx)
                .iter()
                .all(|machine| agents_on(machine, cx).is_empty());
            if self.arm_conversation_on_create {
                self.close_editor(cx);
            }
        }
        let agent = args.get("agent").and_then(serde_json::Value::as_str);
        if let Some(id) = agent {
            let machine = args
                .get("machine")
                .and_then(api::machine_from_json)
                .unwrap_or(RuntimeKey::Local);
            let tab = match args.get("tab").and_then(serde_json::Value::as_str) {
                Some("memory") => AgentEditorTab::Memory,
                _ => AgentEditorTab::Config,
            };
            let agent = AgentRef::new(machine, id.to_string());
            if agent.definition(cx).is_some() {
                self.discard_uncommitted_draft(Some(&agent), cx);
                self.open_agent(agent, tab, window, cx);
            } else {
                self.error = Some("这个智能体已经不在了".to_string());
            }
        } else if let Some(draft) = self.conversation_draft.clone() {
            // 回到还没提交的草稿。
            self.open_agent(draft, AgentEditorTab::Config, window, cx);
        }
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }
}

impl Render for AgentsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_agents_page(cx)
    }
}
