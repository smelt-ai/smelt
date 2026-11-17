//! 新建会话的对话框：选位置（机器、项目、工作树），再从那台机器的新建目录里挑一项。
//!
//! 新建目录（`new_session.catalog`）由那台机器的运行时按它自己的启动项、装没装、常用项
//! 拼好，打开时和换机器时各读一次；点星标改的是那台机器的常用项，改完重读。对话的启动
//! 规格按那台机器的智能体配置现算（[`conversation_acp`]），确认时才算，配置在框开着
//! 期间变了也以最新的为准。
//!
//! 位置的名字在打开、选位置、目录和机器变了时算好存着，render 不读目录。

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window, div, px, rgb,
    svg,
};
use gpui_kit_assets::IconName;
use smelt_plugin_api::client::protocol::control::EmptyParams;
use smelt_plugin_api::model::agent_kind::{ConversationAgentKind, TerminalAgentKind};
use smelt_plugin_api::model::control_api::{NewSessionCatalog, NewSessionPinsToggle};
use smelt_plugin_api::model::new_session::{
    NewSessionAction, NewSessionSections, NewSessionTarget, PinParams, SECTION_LABELS,
};
use smelt_plugin_api::model::session_control::profile_id_from_option_id;
use smelt_plugin_api::model::workspace_directory::{
    WorkspaceConversation, WorkspaceTerminal, project_labels, project_name,
};
use smelt_plugin_api::ui::dialog;
use smelt_plugin_api::ui::menu::{Menu, MenuItem, MenuTrigger};
use smelt_plugin_api::ui::{Button, IconAction as _, ListRow, ui_theme};
use smelt_plugin_api::{self as api, HostEvent, RuntimeKey};

use super::agents::conversation_acp;
use super::repositories::{self, Mode};
use crate::peers::agents::{AGENT_SETTINGS, AGENT_SETTINGS_CHANGED, AgentSettingsState};
use crate::peers::conversation::ConversationStart;
use crate::{contract, lifecycle};
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

/// 卡片宽度。
const WIDTH: f32 = 520.;
/// 启动项列表最高多高，再多就在里面滚。
const LIST_HEIGHT: f32 = 360.;

pub(super) fn open(
    machine: Option<RuntimeKey>,
    project: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let (machine, root, workset) = initial_location(machine, project, cx);
    let state = cx.new(|cx| NewSession::new(machine, root, workset, cx));
    let focus = state.read(cx).focus.clone();
    dialog::open(window, cx, move |card, _, cx| {
        let confirm = state.clone();
        card.w(px(WIDTH))
            .child(state.clone())
            .footer(footer(&state, cx))
            .on_ok(move |_, cx| confirm.update(cx, |state, cx| state.confirm(cx)))
    });
    // 对话框层开好时焦点在卡片上；交给列表，上下键才选得动。
    window.defer(cx, move |window, cx| focus.focus(window, cx));
}

/// 预选的位置：机器、项目根、工作树 id。给了位置路径的，是项目根就选那个项目，落在某个
/// 工作树里就选那个工作树；目录里两样都对不上（还没收到）就按项目根用。没给的按活动项目，
/// 再不行取那台机器上第一个打开的项目。
fn initial_location(
    machine: Option<RuntimeKey>,
    path: Option<String>,
    cx: &App,
) -> (RuntimeKey, Option<String>, Option<String>) {
    let host = api::host(cx);
    let active = api::active_context(cx).project;
    let machine = machine
        .or_else(|| active.as_ref().map(|project| project.machine.clone()))
        .unwrap_or(RuntimeKey::Local);
    let directory = host.workspace_directory(&machine, cx);
    if let Some(path) = path {
        let style = host.path_style(&machine, cx);
        let workset = directory.as_ref().and_then(|directory| {
            if directory
                .projects
                .iter()
                .any(|root| style.clean(root) == style.clean(&path))
            {
                return None;
            }
            directory
                .worksets
                .iter()
                .find(|workset| workset.contains_cwd(style, &path))
        });
        return match workset {
            Some(workset) => (
                machine,
                Some(workset.project_root.clone()),
                Some(workset.id.clone()),
            ),
            None => (machine, Some(path), None),
        };
    }
    let root = active
        .filter(|project| project.machine == machine)
        .map(|project| project.root)
        .or_else(|| directory.and_then(|directory| directory.projects.into_iter().next()));
    (machine, root, None)
}

/// 新建目录读到哪了。
enum Catalog {
    Loading,
    Ready,
    Failed(String),
}

/// 列表里的一行：所在的段（`SECTION_LABELS` 的下标）和那一项。常用段里的项在原来的段里
/// 也有一份，所以选中记行号，不记 key。
struct Row {
    section: usize,
    action: NewSessionAction,
}

struct NewSession {
    machine: RuntimeKey,
    root: Option<String>,
    workset: Option<String>,
    /// 刚从这里建的工作树（项目根、任务名）：目录推到时选上它。
    awaiting_workset: Option<(String, String)>,
    /// 机器名、位置名、位置是不是工作树：在事件里算好，render 只读。
    machine_name: String,
    location: Option<String>,
    in_workset: bool,
    catalog: Catalog,
    rows: Vec<Row>,
    /// 每行在列表里是第几个子元素（段标题也占位），键盘选中后滚过去用。
    row_children: Vec<usize>,
    selected: Option<usize>,
    scroll: ScrollHandle,
    /// 这台机器的智能体配置。`None` 是还没问到。
    settings: Option<AgentSettingsState>,
    error: Option<String>,
    focus: FocusHandle,
    /// 换一次机器加一；回来的结果对不上就是上一台的，丢掉。
    generation: u64,
    /// 读一次新建目录加一；点星标后连着重读几次，只认最后一次。
    catalog_request: u64,
    _events: Subscription,
}

impl NewSession {
    fn new(
        machine: RuntimeKey,
        root: Option<String>,
        workset: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let events = cx.subscribe(&api::events(cx), |this, _, event: &HostEvent, cx| {
            this.on_host_event(event, cx);
        });
        let mut this = Self {
            machine,
            root,
            workset,
            awaiting_workset: None,
            machine_name: String::new(),
            location: None,
            in_workset: false,
            catalog: Catalog::Loading,
            rows: Vec::new(),
            row_children: Vec::new(),
            selected: None,
            scroll: ScrollHandle::new(),
            settings: None,
            error: None,
            focus: cx.focus_handle(),
            generation: 0,
            catalog_request: 0,
            _events: events,
        };
        this.refresh_location(cx);
        this.load(cx);
        this
    }

    fn on_host_event(&mut self, event: &HostEvent, cx: &mut Context<Self>) {
        if let Some(changed) = AGENT_SETTINGS_CHANGED.parse(event) {
            if changed.machine == self.machine {
                self.settings = Some(changed.state);
                cx.notify();
            }
            return;
        }
        // 连不上时读失败的，重新连上就再读一次。
        if let HostEvent::Reconnected(machine) = event
            && *machine == self.machine
            && matches!(self.catalog, Catalog::Failed(_))
        {
            self.load(cx);
            cx.notify();
            return;
        }
        if matches!(
            event,
            HostEvent::SessionsChanged | HostEvent::MachinesChanged
        ) {
            self.refresh_location(cx);
            cx.notify();
        }
    }

    /// 换了机器（或刚打开）：清掉上一台的，重读新建目录和智能体配置。
    fn load(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.catalog = Catalog::Loading;
        self.rows.clear();
        self.selected = None;
        self.settings = None;
        self.fetch_catalog(cx);

        let generation = self.generation;
        let settings = AGENT_SETTINGS.run(&(), cx);
        cx.spawn(async move |this, cx| {
            let result = settings.await;
            let _ = this.update(cx, |this, cx| {
                // 事件先到的那份更新，不拿这份盖掉。
                if this.generation != generation || this.settings.is_some() {
                    return;
                }
                this.settings = match result {
                    // 列表里没有这台：还没收到推送，等事件。
                    Ok(all) => all
                        .into_iter()
                        .find(|settings| settings.machine == this.machine)
                        .map(|settings| settings.state),
                    Err(error) => Some(AgentSettingsState::Unavailable {
                        reason: format!("读不到智能体配置：{error}"),
                    }),
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// 读一次新建目录。读的时候列表照旧显示，回来了整份换掉，选中的尽量留在原处。
    fn fetch_catalog(&mut self, cx: &mut Context<Self>) {
        self.catalog_request += 1;
        let (generation, request) = (self.generation, self.catalog_request);
        let catalog = api::call::<NewSessionCatalog>(
            &self.machine,
            EmptyParams::default(),
            api::CALL_TIMEOUT,
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = catalog.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation || this.catalog_request != request {
                    return;
                }
                match result {
                    Ok(catalog) => {
                        this.catalog = Catalog::Ready;
                        this.apply_sections(catalog.sections);
                    }
                    Err(error) => {
                        this.catalog = Catalog::Failed(error);
                        this.rows.clear();
                        this.selected = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_sections(&mut self, sections: NewSessionSections) {
        let previous = self
            .selected
            .and_then(|ix| self.rows.get(ix))
            .map(|row| (row.section, row.action.key.clone()));
        self.rows = sections
            .into_array()
            .into_iter()
            .enumerate()
            .flat_map(|(section, actions)| {
                actions
                    .into_iter()
                    .map(move |action| Row { section, action })
            })
            .collect();
        self.row_children = child_indexes(&self.rows);
        self.selected = previous
            .and_then(|(section, key)| {
                self.rows
                    .iter()
                    .position(|row| row.section == section && row.action.key == key)
                    .or_else(|| self.rows.iter().position(|row| row.action.key == key))
            })
            .or((!self.rows.is_empty()).then_some(0));
    }

    fn set_location(
        &mut self,
        machine: RuntimeKey,
        root: String,
        workset: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let switched = machine != self.machine;
        self.machine = machine;
        self.root = Some(root);
        self.workset = workset;
        self.awaiting_workset = None;
        self.error = None;
        self.refresh_location(cx);
        if switched {
            self.load(cx);
        }
        cx.notify();
    }

    /// 仓库对话框刚在 `root` 下建好了工作树 `task`：目录里有了就选上它。
    fn await_workset(&mut self, root: String, task: String, cx: &mut Context<Self>) {
        self.awaiting_workset = Some((root, task));
        self.refresh_location(cx);
        cx.notify();
    }

    /// 按眼下的目录和机器表重算位置的名字；等着的工作树到了就选上。
    fn refresh_location(&mut self, cx: &App) {
        let host = api::host(cx);
        self.machine_name = host
            .machines(cx)
            .into_iter()
            .find(|machine| machine.key == self.machine)
            .map(|machine| machine.name)
            .unwrap_or_else(|| "这台机器".into());
        let style = host.path_style(&self.machine, cx);
        let directory = host.workspace_directory(&self.machine, cx);
        let worksets = directory
            .as_ref()
            .map(|directory| directory.worksets.as_slice())
            .unwrap_or_default();
        if let Some((root, task)) = &self.awaiting_workset
            && let Some(workset) = worksets
                .iter()
                .find(|workset| &workset.project_root == root && &workset.task == task)
        {
            self.root = Some(root.clone());
            self.workset = Some(workset.id.clone());
            self.awaiting_workset = None;
        }
        let workset = self
            .workset
            .as_ref()
            .map(|id| worksets.iter().find(|workset| &workset.id == id));
        self.in_workset = self.workset.is_some();
        self.location = self.root.as_ref().map(|root| {
            let project = project_name(style, root);
            match workset {
                None => project,
                Some(Some(workset)) => format!("{project} / {}", workset.task),
                Some(None) => format!("{project} / 工作树已不存在"),
            }
        });
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.rows.len() as isize;
        if len == 0 {
            return;
        }
        let next = match self.selected {
            Some(ix) => (ix as isize + delta).rem_euclid(len),
            None if delta > 0 => 0,
            None => len - 1,
        };
        self.selected = Some(next as usize);
        if let Some(&child) = self.row_children.get(next as usize) {
            self.scroll.scroll_to_item(child);
        }
        cx.notify();
    }

    fn toggle_pin(&mut self, key: String, cx: &mut Context<Self>) {
        let generation = self.generation;
        let toggled = api::call::<NewSessionPinsToggle>(
            &self.machine,
            PinParams { key },
            api::CALL_TIMEOUT,
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = toggled.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(_) => this.fetch_catalog(cx),
                    Err(error) => {
                        this.error = Some(format!("保存常用项失败：{error}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn fail(&mut self, message: impl Into<String>, cx: &mut Context<Self>) -> bool {
        self.error = Some(message.into());
        cx.notify();
        false
    }

    /// 按选中的那项在选好的位置新建，交给生命周期写目录、跳过去。返回框能不能关：
    /// 没建成就留着框，把原因写在里面。
    fn confirm(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(action) = self
            .selected
            .and_then(|ix| self.rows.get(ix))
            .map(|row| row.action.clone())
        else {
            return false;
        };
        let host = api::host(cx);
        if let Some(reason) = host
            .machines(cx)
            .into_iter()
            .find(|machine| machine.key == self.machine)
            .and_then(|machine| machine.unavailable)
        {
            let message = format!("「{}」{reason}，暂时无法新建会话", self.machine_name);
            return self.fail(message, cx);
        }
        let Some(root) = self.root.clone() else {
            return self.fail("先选一个项目", cx);
        };
        let cwd = match &self.workset {
            None => root,
            Some(id) => {
                let style = host.path_style(&self.machine, cx);
                let root = host
                    .workspace_directory(&self.machine, cx)
                    .and_then(|directory| {
                        directory
                            .worksets
                            .iter()
                            .find(|workset| &workset.id == id)
                            .map(|workset| workset.task_root(style))
                    });
                match root {
                    Some(root) => root,
                    None => return self.fail("工作树已不存在，请重新选择位置", cx),
                }
            }
        };

        let mut conversation = WorkspaceConversation {
            id: uuid::Uuid::new_v4().to_string(),
            workset_id: self.workset.clone(),
            ..WorkspaceConversation::default()
        };
        let start = match &action.target {
            NewSessionTarget::Conversation {
                agent_option_id, ..
            } => {
                let settings = match &self.settings {
                    Some(AgentSettingsState::Ready { settings }) => settings,
                    Some(AgentSettingsState::Unavailable { reason }) => {
                        let message = format!("那台机器的智能体配置不可用：{reason}");
                        return self.fail(message, cx);
                    }
                    Some(AgentSettingsState::Absent) | None => {
                        return self.fail("正在读取那台机器的智能体配置，请稍后再试", cx);
                    }
                };
                let sid = format!("acp-{}", uuid::Uuid::new_v4());
                let Some(acp) = conversation_acp(settings, agent_option_id, sid, cwd) else {
                    return self.fail("智能体配置已变更，请重新选择启动项", cx);
                };
                conversation.acp = Some(acp);
                Some(ConversationStart::default())
            }
            NewSessionTarget::Terminal { command, .. } => {
                conversation.terminals = vec![new_terminal(
                    cwd,
                    Some(action.label.clone()),
                    Some(command.clone()),
                )];
                None
            }
            NewSessionTarget::BlankTerminal => {
                conversation.terminals = vec![new_terminal(cwd, None, None)];
                None
            }
        };
        let created = lifecycle::create_conversation(self.machine.clone(), conversation, start, cx);
        crate::report_failure("新建会话", created, cx);
        true
    }
}

/// 每行对应列表的第几个子元素：每段第一行前面有一个段标题。和 `render_body` 的摆法一致。
fn child_indexes(rows: &[Row]) -> Vec<usize> {
    let mut section = None;
    let mut child = 0;
    rows.iter()
        .map(|row| {
            if section != Some(row.section) {
                section = Some(row.section);
                child += 1;
            }
            child += 1;
            child - 1
        })
        .collect()
}

fn new_terminal(cwd: String, label: Option<String>, command: Option<String>) -> WorkspaceTerminal {
    WorkspaceTerminal {
        id: Some(uuid::Uuid::new_v4().to_string()),
        cwd: Some(cwd),
        launch_label: label,
        launch_cmd: command,
        ..WorkspaceTerminal::default()
    }
}

/// 行首的图标：引擎或终端 agent 自己的图标；自定义 Agent 一律画通用的，和侧栏一致。
fn action_icon(target: &NewSessionTarget) -> SharedString {
    match target {
        NewSessionTarget::Conversation {
            agent_option_id,
            agent_kind,
        } => profile_id_from_option_id(agent_option_id)
            .map(|_| ConversationAgentKind::Custom)
            .or_else(|| ConversationAgentKind::from_id(agent_kind))
            .map(|kind| kind.icon_asset().into())
            .unwrap_or_else(|| IconName::Bot.path()),
        NewSessionTarget::Terminal { command, provider } => provider
            .as_deref()
            .and_then(TerminalAgentKind::from_id)
            .or_else(|| TerminalAgentKind::from_command_prefix(command))
            .map(|kind| kind.icon_asset().into())
            .unwrap_or_else(|| IconName::SquareTerminal.path()),
        NewSessionTarget::BlankTerminal => IconName::SquareTerminal.path(),
    }
}

impl Render for NewSession {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = rgb(ui_theme::muted());
        let weak = cx.entity().downgrade();
        let location = Button::new("new-session-location")
            .label(
                self.location
                    .clone()
                    .unwrap_or_else(|| "选择项目".to_string()),
            )
            .icon(if self.in_workset {
                IconName::GitBranch
            } else {
                IconName::Folder
            })
            .dropdown_caret(true);
        let location = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .type_style(TYPE.ui)
                    .text_color(muted)
                    .child(self.machine_name.clone()),
            )
            .child(MenuTrigger::dropdown(
                "new-session-location-menu",
                location,
                move |_, cx| location_menu(&weak, cx),
            ));

        // 焦点挂在整块正文上（列表还没读到时也在），Esc、Enter 照样冒到对话框。
        dialog::content("新建会话", None)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let keystroke = &event.keystroke;
                if keystroke.modifiers.modified() {
                    return;
                }
                match keystroke.key.as_str() {
                    "up" => this.step(-1, cx),
                    "down" => this.step(1, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(location)
            .child(self.render_body(cx))
            .when_some(self.error.clone(), |content, error| {
                content.child(
                    div()
                        .type_style(TYPE.ui)
                        .text_color(rgb(ui_theme::bad()))
                        .child(error),
                )
            })
    }
}

impl NewSession {
    fn render_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let notice = |text: String| {
            div()
                .py_4()
                .type_style(TYPE.ui)
                .text_color(rgb(ui_theme::muted()))
                .child(text)
                .into_any_element()
        };
        if self.root.is_none() {
            return div()
                .flex()
                .flex_col()
                .items_start()
                .gap_2()
                .child(notice(format!(
                    "「{}」上还没有打开的项目。",
                    self.machine_name
                )))
                .child(
                    Button::new("new-session-open-project")
                        .label("打开项目…")
                        .on_click(|_, window, cx| {
                            dialog::close(window, cx);
                            let opened = contract::OPEN_PROJECT.run(&(), cx);
                            crate::report_failure("打开项目", opened, cx);
                        }),
                )
                .into_any_element();
        }
        match &self.catalog {
            Catalog::Loading => {
                return notice(format!("正在读取「{}」上装了哪些工具…", self.machine_name));
            }
            Catalog::Failed(error) => {
                return notice(format!("连不上「{}」：{error}", self.machine_name));
            }
            Catalog::Ready if self.rows.is_empty() => {
                return notice("没有可用的启动项".to_string());
            }
            Catalog::Ready => {}
        }

        let mut list = div()
            .id("new-session-actions")
            .track_scroll(&self.scroll)
            .overflow_y_scroll()
            .max_h(px(LIST_HEIGHT))
            .flex()
            .flex_col()
            .gap_0p5();
        let mut section = None;
        for (ix, row) in self.rows.iter().enumerate() {
            if section != Some(row.section) {
                section = Some(row.section);
                list = list.child(
                    div()
                        .px_2()
                        .pt_2()
                        .pb_1()
                        .type_style(TYPE.text_xsmall)
                        .text_color(rgb(ui_theme::muted()))
                        .child(SECTION_LABELS[row.section]),
                );
            }
            list = list.child(self.render_row(ix, row, cx));
        }
        list.into_any_element()
    }

    fn render_row(&self, ix: usize, row: &Row, cx: &mut Context<Self>) -> ListRow {
        let selected = self.selected == Some(ix);
        let action = &row.action;
        let key = action.key.clone();
        let pinned = action.pinned;
        ListRow::new(("new-session-action", ix))
            .selected(selected)
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.selected = Some(ix);
                if this.confirm(cx) {
                    dialog::close(window, cx);
                }
            }))
            .child(
                svg()
                    .path(action_icon(&action.target))
                    .flex_none()
                    .size(px(16.))
                    .text_color(rgb(ui_theme::row_muted(selected))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .type_style(TYPE.ui)
                    .child(format!("{} · {}", action.label, action.kind.label())),
            )
            .child(
                Button::new(("new-session-pin", ix))
                    .quiet()
                    .small()
                    .icon(if pinned {
                        IconName::StarFill
                    } else {
                        IconName::Star
                    })
                    .icon_action(if pinned {
                        "取消常用"
                    } else {
                        "设为常用"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // 星标在行里，别让这一下也算点了行。
                        cx.stop_propagation();
                        this.toggle_pin(key.clone(), cx);
                    })),
            )
    }
}

fn footer(state: &Entity<NewSession>, cx: &App) -> impl IntoElement {
    let this = state.read(cx);
    let place = this.root.clone().map(|root| (this.machine.clone(), root));
    let can_confirm = place.is_some() && this.selected.is_some();
    let confirm = state.clone();
    dialog::footer()
        .child(
            Button::new("new-session-repositories")
                .quiet()
                .small()
                .label("管理项目仓库…")
                .disabled(place.is_none())
                .on_click(move |_, window, cx| {
                    if let Some((machine, root)) = place.clone() {
                        repositories::open(machine, root, Mode::Manage, None, window, cx);
                    }
                }),
        )
        .child(div().flex_1())
        .child(
            Button::new("new-session-cancel")
                .quiet()
                .small()
                .label("取消")
                .on_click(|_, window, cx| dialog::close(window, cx)),
        )
        .child(
            Button::new("new-session-confirm")
                .primary()
                .small()
                .label("新建")
                .disabled(!can_confirm)
                .on_click(move |_, window, cx| {
                    if confirm.update(cx, |state, cx| state.confirm(cx)) {
                        dialog::close(window, cx);
                    }
                }),
        )
}

/// 位置下拉：每台机器上打开的项目和它们的工作树，最后是在选中的项目里新建工作树。
fn location_menu(state: &WeakEntity<NewSession>, cx: &mut App) -> Menu {
    let Some(current) = state.upgrade() else {
        return Menu::new();
    };
    let (current_machine, current_root, current_workset) = {
        let current = current.read(cx);
        (
            current.machine.clone(),
            current.root.clone(),
            current.workset.clone(),
        )
    };
    let host = api::host(cx);
    let machines = host.machines(cx);
    let labelled = machines.len() > 1;
    let mut menu = Menu::new();
    for machine in machines {
        if labelled {
            menu = menu.label(machine.name.clone());
        }
        let Some(directory) = host.workspace_directory(&machine.key, cx) else {
            menu = menu.item(MenuItem::new("还没连上").disabled(true));
            continue;
        };
        if directory.projects.is_empty() {
            menu = menu.item(MenuItem::new("没有打开的项目").disabled(true));
            continue;
        }
        let style = host.path_style(&machine.key, cx);
        let roots: Vec<&str> = directory.projects.iter().map(String::as_str).collect();
        let labels = project_labels(style, &roots);
        let here = machine.key == current_machine;
        for (root, label) in directory.projects.iter().zip(labels) {
            let checked =
                here && current_root.as_deref() == Some(root.as_str()) && current_workset.is_none();
            menu = menu.item(
                MenuItem::new(label.clone())
                    .icon(IconName::Folder)
                    .checked(checked)
                    .on_click(pick(state, &machine.key, root, None)),
            );
            for workset in directory
                .worksets
                .iter()
                .filter(|workset| &workset.project_root == root)
            {
                let checked = here && current_workset.as_deref() == Some(workset.id.as_str());
                menu = menu.item(
                    MenuItem::new(format!("{label} / {}", workset.task))
                        .icon(IconName::GitBranch)
                        .checked(checked)
                        .on_click(pick(state, &machine.key, root, Some(&workset.id))),
                );
            }
        }
    }
    let create = state.clone();
    menu.separator().item(
        MenuItem::new("新建工作树…")
            .icon(IconName::GitBranch)
            .disabled(current_root.is_none())
            .on_click(move |_, window, cx| new_worktree(&create, window, cx)),
    )
}

fn pick(
    state: &WeakEntity<NewSession>,
    machine: &RuntimeKey,
    root: &str,
    workset: Option<&str>,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let state = state.clone();
    let machine = machine.clone();
    let root = root.to_string();
    let workset = workset.map(str::to_string);
    move |_, _, cx| {
        let _ = state.update(cx, |state, cx| {
            state.set_location(machine.clone(), root.clone(), workset.clone(), cx);
        });
    }
}

/// 在选中的项目里建工作树：仓库对话框叠在上面，建好了回到这里并选上它。
fn new_worktree(state: &WeakEntity<NewSession>, window: &mut Window, cx: &mut App) {
    let Some(current) = state.upgrade() else {
        return;
    };
    let (machine, root) = {
        let current = current.read(cx);
        (current.machine.clone(), current.root.clone())
    };
    let Some(root) = root else {
        return;
    };
    let state = state.clone();
    let project = root.clone();
    let created: repositories::Created = std::rc::Rc::new(move |task: String, cx: &mut App| {
        let _ = state.update(cx, |state, cx| {
            state.await_workset(project.clone(), task, cx);
        });
    });
    repositories::open(machine, root, Mode::Create, Some(created), window, cx);
}

#[cfg(test)]
mod tests {
    use smelt_plugin_api::model::new_session::NewSessionActionKind;

    use super::*;

    fn row(section: usize, key: &str) -> Row {
        Row {
            section,
            action: NewSessionAction {
                key: key.into(),
                label: key.into(),
                kind: NewSessionActionKind::Terminal,
                pinned: false,
                target: NewSessionTarget::BlankTerminal,
            },
        }
    }

    #[test]
    fn section_headers_shift_the_scroll_targets() {
        let rows = [row(0, "a"), row(1, "a"), row(1, "b"), row(2, "c")];
        // 标题0 a 标题1 a b 标题2 c
        assert_eq!(child_indexes(&rows), vec![1, 3, 4, 6]);
    }

    #[test]
    fn custom_agents_draw_the_generic_conversation_icon() {
        let profile = NewSessionTarget::Conversation {
            agent_option_id: "profile:mine".into(),
            agent_kind: ConversationAgentKind::ALL[0].id().into(),
        };
        assert_eq!(
            action_icon(&profile),
            SharedString::from(ConversationAgentKind::Custom.icon_asset())
        );
    }

    #[test]
    fn unknown_terminal_commands_draw_a_terminal() {
        let terminal = NewSessionTarget::Terminal {
            command: "make dev".into(),
            provider: None,
        };
        assert_eq!(action_icon(&terminal), IconName::SquareTerminal.path());
        assert_eq!(
            action_icon(&NewSessionTarget::BlankTerminal),
            IconName::SquareTerminal.path()
        );
    }
}
