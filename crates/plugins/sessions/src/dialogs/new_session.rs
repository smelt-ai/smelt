//! 新建会话的菜单：从那台机器的新建目录里挑一项，点一下就在选好的位置建好。
//!
//! 两个入口，同一份内容：
//! - 项目行、工作树行的「+」（[`NewSessionButton`]）：贴着按钮弹出，位置就是那一行，不显示
//!   位置。和终端、编辑器的「新建」下拉一样，就地弹、单列、点一项就建。
//! - 命令（⌘N、项目菜单）：没有按钮可贴，像命令面板那样钉在窗口上方，顶上多一行位置，
//!   能换机器、项目、工作树。
//!
//! 排法：常用在最上面、不加组标题，行尾标类型和数字键（1~9 直接建，位置固定好记，所以不按
//! 最近用过重排）；下面按终端、对话分组，组里不再写类型。星标只在选中的那行出现（鼠标移上去
//! 就选中，键盘和鼠标共用一个高亮）：常用按机器存，设置页只管本机，所以就地改。
//!
//! 新建目录（`new_session.catalog`）由那台机器的运行时按它自己的启动项、装没装、常用项
//! 拼好，打开时和换机器时各读一次；点星标改的是那台机器的常用项，改完重读。对话的启动
//! 规格按那台机器的智能体配置现算（[`conversation_acp`]），选中时才算，配置在菜单开着
//! 期间变了也以最新的为准。
//!
//! 位置的名字在打开、选位置、目录和机器变了时算好存着，render 不读目录。

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, ElementId, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, Keystroke, MouseMoveEvent, ParentElement as _, Render, RenderOnce, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window,
    div, px, rgb, svg,
};
use rust_i18n::t;
use smelt_plugin_api::client::protocol::control::EmptyParams;
use smelt_plugin_api::model::agent_kind::{ConversationAgentKind, TerminalAgentKind};
use smelt_plugin_api::model::control_api::{NewSessionCatalog, NewSessionPinsToggle};
use smelt_plugin_api::model::new_session::{
    NewSessionAction, NewSessionSections, NewSessionTarget, PinParams, section_label,
};
use smelt_plugin_api::model::session_control::profile_id_from_option_id;
use smelt_plugin_api::model::workspace_directory::{
    WorkspaceConversation, WorkspaceTerminal, project_labels, project_name,
};
use smelt_plugin_api::ui::IconName;
use smelt_plugin_api::ui::dialog;
use smelt_plugin_api::ui::keybind::Keybind;
use smelt_plugin_api::ui::kit::{RADIUS, TYPE, TypeStyled as _};
use smelt_plugin_api::ui::menu::{Menu, MenuItem, MenuTrigger};
use smelt_plugin_api::ui::popover::{Popover, PopoverState};
use smelt_plugin_api::ui::{Button, IconAction as _, ui_theme};
use smelt_plugin_api::{self as api, HostEvent, RuntimeKey};

use super::agents::conversation_acp;
use super::repositories::{self, Mode};
use crate::peers::agents::{AGENT_SETTINGS, AGENT_SETTINGS_CHANGED, AgentSettingsState};
use crate::peers::conversation::ConversationStart;
use crate::{contract, lifecycle};

/// 菜单宽度。从命令打开的那张顶上多一行「机器 · 项目 / 工作树」，宽一些。
const WIDTH: f32 = 280.;
const PALETTE_WIDTH: f32 = 360.;
/// 列表最高多高，再多就在里面滚。
const LIST_HEIGHT: f32 = 420.;
/// 从命令打开时卡片顶边离窗口顶，和命令面板一样。
const PALETTE_TOP: f32 = 80.;
/// 行高、面板内边距，和组件库的菜单一致。
const ROW_HEIGHT: f32 = 28.;
const PADDING: f32 = 4.;

/// 从命令打开：钉在窗口上方，带位置行。
pub(super) fn open(
    machine: Option<RuntimeKey>,
    project: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let (machine, root, workset) = initial_location(machine, project, cx);
    let state = cx.new(|cx| NewSession::new(machine, root, workset, Host::Palette, cx));
    let focus = state.read(cx).focus.clone();
    dialog::open(window, cx, move |card, _, _| {
        let confirm = state.clone();
        card.top(px(PALETTE_TOP))
            .w(px(PALETTE_WIDTH))
            .p(px(PADDING))
            .gap_0()
            .child(state.clone())
            // Enter 在对话框层是确认动作，先于按键监听，走这里。
            .on_ok(move |_, cx| confirm.update(cx, |state, cx| state.launch_selected(cx)))
    });
    // 对话框层开好时焦点在卡片上；交给菜单，上下键、数字键才收得到。
    window.defer(cx, move |window, cx| focus.focus(window, cx));
}

/// 项目行、工作树行的「+」：点开贴着它弹出新建菜单，位置就是 `path`（项目根或工作树路径）。
///
/// 给了悬停组（[`Self::revealed_on_hover`]）的平时隐着、悬停那一行才出现；菜单开着时一直
/// 显示，鼠标移进菜单、离开那一行也不消失。
#[derive(IntoElement)]
pub(crate) struct NewSessionButton {
    id: SharedString,
    tooltip: SharedString,
    hover_group: Option<&'static str>,
    machine: RuntimeKey,
    path: String,
}

impl NewSessionButton {
    pub(crate) fn new(
        id: impl Into<SharedString>,
        tooltip: impl Into<SharedString>,
        machine: RuntimeKey,
        path: String,
    ) -> Self {
        Self {
            id: id.into(),
            tooltip: tooltip.into(),
            hover_group: None,
            machine,
            path,
        }
    }

    pub(crate) fn revealed_on_hover(mut self, group: &'static str) -> Self {
        self.hover_group = Some(group);
        self
    }
}

impl RenderOnce for NewSessionButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            tooltip,
            hover_group,
            machine,
            path,
        } = self;
        let open =
            window.use_keyed_state(ElementId::Name(format!("{id}:open").into()), cx, |_, _| {
                false
            });
        let is_open = *open.read(cx);
        let popover = Popover::new(ElementId::Name(id.clone()))
            .w(px(WIDTH))
            .p(px(PADDING))
            .on_open_change(move |opened, _, cx| {
                open.update(cx, |open, cx| {
                    *open = *opened;
                    cx.notify();
                });
            })
            .trigger(
                Button::new(ElementId::Name(format!("{id}:trigger").into()))
                    .quiet()
                    .small()
                    .icon(IconName::Plus)
                    .icon_action(tooltip),
            )
            .content(move |_, window, cx| {
                let popover = cx.entity().downgrade();
                // 只在开着时搭，关掉的那一帧起就不再取，状态随之丢掉：每次打开都重读目录。
                let key = ElementId::Name(format!("{id}:menu").into());
                window.use_keyed_state(key, cx, move |window, cx| {
                    let (machine, root, workset) = initial_location(Some(machine), Some(path), cx);
                    let menu = NewSession::new(machine, root, workset, Host::Popover(popover), cx);
                    let focus = menu.focus.clone();
                    window.defer(cx, move |window, cx| focus.focus(window, cx));
                    menu
                })
            });
        div()
            .flex_shrink_0()
            .when_some(hover_group.filter(|_| !is_open), |slot, group| {
                slot.opacity(0.)
                    .group_hover(group, |style| style.opacity(1.))
            })
            .child(popover)
    }
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

/// 菜单开在哪：关菜单时按这个关。
#[derive(Clone)]
enum Host {
    /// 贴着「+」的弹出层。
    Popover(WeakEntity<PopoverState>),
    /// 命令打开的、钉在窗口上方的那张，带位置行。
    Palette,
}

impl Host {
    /// 关掉菜单，焦点回到打开前的地方。
    fn close(&self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Popover(popover) => {
                let _ = popover.update(cx, |popover, cx| popover.dismiss(window, cx));
            }
            Self::Palette => dialog::close(window, cx),
        }
    }
}

/// 列表里的一行：所在的段（`section_label` 的下标，0 是常用）和那一项。
struct Row {
    section: usize,
    action: NewSessionAction,
}

/// 列表里一格放什么。常用不加组标题，和下面的分组之间隔一条线；终端、对话各有组标题。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Separator,
    Header(usize),
    Row(usize),
}

/// 选中项建好要做的事：先关菜单（焦点回原处），再建，新会话拿到的焦点不会被关菜单抢回去。
struct Launch {
    machine: RuntimeKey,
    conversation: WorkspaceConversation,
    start: Option<ConversationStart>,
}

impl Launch {
    fn run(self, cx: &mut App) {
        let created =
            lifecycle::create_conversation(self.machine, self.conversation, self.start, cx);
        crate::report_failure(t!("action.new_session"), created, cx);
    }
}

struct NewSession {
    host: Host,
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
    /// 按 `rows` 排好的各格，和每行是第几格（键盘选中后滚过去用）。
    slots: Vec<Slot>,
    row_slots: Vec<usize>,
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
        host: Host,
        cx: &mut Context<Self>,
    ) -> Self {
        let events = cx.subscribe(&api::events(cx), |this, _, event: &HostEvent, cx| {
            this.on_host_event(event, cx);
        });
        let mut this = Self {
            host,
            machine,
            root,
            workset,
            awaiting_workset: None,
            machine_name: String::new(),
            location: None,
            in_workset: false,
            catalog: Catalog::Loading,
            rows: Vec::new(),
            slots: Vec::new(),
            row_slots: Vec::new(),
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
                        reason: t!("error.read_agent_settings", error = error).into_owned(),
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
        self.slots = slots(&self.rows);
        self.row_slots = row_slots(&self.slots);
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
        // 本机的名字是主机名（常是一串序列号），不如「本机」好认。
        self.machine_name = match self.machine {
            RuntimeKey::Local => t!("machine.local").into(),
            _ => host
                .machines(cx)
                .into_iter()
                .find(|machine| machine.key == self.machine)
                .map(|machine| machine.name)
                .unwrap_or_else(|| t!("machine.this").into()),
        };
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
                Some(None) => format!("{project} / {}", t!("new.workset_gone")),
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
        if let Some(&slot) = self.row_slots.get(next as usize) {
            self.scroll.scroll_to_item(slot);
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
                        this.error = Some(
                            t!(
                                "error.failed",
                                what = t!("action.save_favorites"),
                                error = error
                            )
                            .into_owned(),
                        );
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn fail(&mut self, message: impl Into<String>, cx: &mut Context<Self>) -> Option<Launch> {
        self.error = Some(message.into());
        cx.notify();
        None
    }

    /// 点了第 `ix` 行（或按了它的数字键）：建得成就关掉菜单再建，建不成留着菜单写原因。
    fn launch(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(ix);
        let Some(launch) = self.prepare(cx) else {
            return;
        };
        self.host.close(window, cx);
        cx.defer(move |cx| launch.run(cx));
    }

    /// 对话框层的 Enter：返回能不能关，关是对话框层自己做的，建推到关完之后。
    fn launch_selected(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(launch) = self.prepare(cx) else {
            return false;
        };
        cx.defer(move |cx| launch.run(cx));
        true
    }

    /// 按选中的那项和选好的位置拼出要建的会话。拼不成把原因写在菜单里，返回 `None`。
    fn prepare(&mut self, cx: &mut Context<Self>) -> Option<Launch> {
        let action = self
            .selected
            .and_then(|ix| self.rows.get(ix))
            .map(|row| row.action.clone())?;
        let host = api::host(cx);
        if let Some(reason) = host
            .machines(cx)
            .into_iter()
            .find(|machine| machine.key == self.machine)
            .and_then(|machine| machine.unavailable)
        {
            let message = t!(
                "new.machine_unavailable",
                machine = self.machine_name,
                reason = reason
            );
            return self.fail(message, cx);
        }
        let Some(root) = self.root.clone() else {
            return self.fail(t!("new.choose_project"), cx);
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
                    None => return self.fail(t!("new.workset_gone_choose"), cx),
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
                        let message = t!("error.agent_settings_unavailable", reason = reason);
                        return self.fail(message, cx);
                    }
                    Some(AgentSettingsState::Absent) | None => {
                        return self.fail(t!("error.agent_settings_loading"), cx);
                    }
                };
                let sid = format!("acp-{}", uuid::Uuid::new_v4());
                let Some(acp) = conversation_acp(settings, agent_option_id, sid, cwd) else {
                    return self.fail(t!("new.settings_changed"), cx);
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
        Some(Launch {
            machine: self.machine.clone(),
            conversation,
            start,
        })
    }
}

/// 按行排出各格：常用在前、不加标题；常用和下面隔一条线；终端、对话各带组标题。
fn slots(rows: &[Row]) -> Vec<Slot> {
    let mut slots = Vec::with_capacity(rows.len() + 3);
    let mut section = None;
    for (ix, row) in rows.iter().enumerate() {
        if section != Some(row.section) {
            if section.is_some() && row.section > 0 && rows[ix - 1].section == 0 {
                slots.push(Slot::Separator);
            }
            if row.section > 0 {
                slots.push(Slot::Header(row.section));
            }
            section = Some(row.section);
        }
        slots.push(Slot::Row(ix));
    }
    slots
}

/// 每行是第几格。
fn row_slots(slots: &[Slot]) -> Vec<usize> {
    slots
        .iter()
        .enumerate()
        .filter_map(|(at, slot)| matches!(slot, Slot::Row(_)).then_some(at))
        .collect()
}

/// 数字键 1~9 对应的常用项：常用排在最前，第 n 项就是第 n 行。
fn pinned_by_digit(rows: &[Row], key: &str) -> Option<usize> {
    let n: usize = key.parse().ok().filter(|n| (1..=9).contains(n))?;
    rows.get(n - 1)
        .filter(|row| row.section == 0)
        .map(|_| n - 1)
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
            .unwrap_or_else(|| IconName::Terminal.path()),
        NewSessionTarget::BlankTerminal => IconName::Terminal.path(),
    }
}

impl Render for NewSession {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            // 焦点挂在整块上（列表还没读到时也在），Esc 照样冒到弹出层、对话框。
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                if keystroke.modifiers.modified() {
                    return;
                }
                match keystroke.key.as_str() {
                    "up" => this.step(-1, cx),
                    "down" => this.step(1, cx),
                    // 弹出层里 Enter 不是动作，落到这里；对话框里它先被对话框层接走（`on_ok`）。
                    "enter" => match this.selected {
                        Some(ix) => this.launch(ix, window, cx),
                        None => return,
                    },
                    key => match pinned_by_digit(&this.rows, key) {
                        Some(ix) => this.launch(ix, window, cx),
                        None => return,
                    },
                }
                cx.stop_propagation();
            }))
            .when(matches!(self.host, Host::Palette), |menu| {
                menu.child(self.render_location(cx)).child(separator())
            })
            .child(self.render_body(cx))
            .when_some(self.error.clone(), |menu, error| {
                menu.child(
                    div()
                        .px(px(12.))
                        .py_1()
                        .type_style(TYPE.ui)
                        .text_color(rgb(ui_theme::bad()))
                        .child(error),
                )
            })
    }
}

impl NewSession {
    /// 命令打开时顶上那行：机器 · 位置，点开换。
    fn render_location(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let location: SharedString = match self.location.as_deref() {
            Some(location) => location.to_string().into(),
            None => t!("new.choose_location").into(),
        };
        let button = Button::new("new-session-location")
            .quiet()
            .small()
            .label(format!("{} · {location}", self.machine_name))
            .icon(if self.in_workset {
                IconName::GitBranch
            } else {
                IconName::Folder
            })
            .dropdown_caret(true);
        div()
            .flex()
            .items_center()
            .px(px(6.))
            .py(px(4.))
            .child(MenuTrigger::dropdown(
                "new-session-location-menu",
                button,
                move |_, cx| location_menu(&weak, cx),
            ))
    }

    fn render_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let notice = |text: String| {
            div()
                .px(px(12.))
                .py_2()
                .type_style(TYPE.ui)
                .text_color(rgb(ui_theme::muted()))
                .child(text)
                .into_any_element()
        };
        if self.root.is_none() {
            let host = self.host.clone();
            return div()
                .flex()
                .flex_col()
                .items_start()
                .child(notice(
                    t!("new.no_projects", machine = self.machine_name).into_owned(),
                ))
                .child(
                    div().px(px(6.)).pb(px(4.)).child(
                        Button::new("new-session-open-project")
                            .small()
                            .label(t!("command.open_project"))
                            .on_click(move |_, window, cx| {
                                host.close(window, cx);
                                let opened = contract::OPEN_PROJECT.run(&(), cx);
                                crate::report_failure(t!("action.open_project"), opened, cx);
                            }),
                    ),
                )
                .into_any_element();
        }
        match &self.catalog {
            Catalog::Loading => {
                return notice(t!("new.loading_tools", machine = self.machine_name).into_owned());
            }
            Catalog::Failed(error) => {
                return notice(
                    t!(
                        "new.unreachable",
                        machine = self.machine_name,
                        error = error
                    )
                    .into_owned(),
                );
            }
            Catalog::Ready if self.rows.is_empty() => {
                return notice(t!("new.no_options").into_owned());
            }
            Catalog::Ready => {}
        }

        div()
            .id("new-session-actions")
            .track_scroll(&self.scroll)
            .overflow_y_scroll()
            .max_h(px(LIST_HEIGHT))
            .flex()
            .flex_col()
            .children(self.slots.iter().map(|slot| {
                match *slot {
                    Slot::Separator => separator().into_any_element(),
                    Slot::Header(section) => div()
                        .h(px(ROW_HEIGHT))
                        .flex()
                        .items_center()
                        .px(px(12.))
                        .type_style(TYPE.small_medium)
                        .text_color(rgb(ui_theme::faint()))
                        .child(section_label(section))
                        .into_any_element(),
                    Slot::Row(ix) => self.render_row(ix, cx).into_any_element(),
                }
            }))
            .into_any_element()
    }

    /// 一行：图标、名字；常用行再标类型和数字键；选中的那行最右是星标。
    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let row = &self.rows[ix];
        let selected = self.selected == Some(ix);
        let action = &row.action;
        let in_common = row.section == 0;
        let key = action.key.clone();
        let pinned = action.pinned;
        let backdrop = if selected {
            ui_theme::soft_hover()
        } else {
            ui_theme::surface()
        };
        div()
            .id(("new-session-action", ix))
            .h(px(ROW_HEIGHT))
            .flex_none()
            .pl(px(12.))
            .pr(px(4.))
            .rounded(px(RADIUS.sm))
            .flex()
            .items_center()
            .gap(px(8.))
            .when(selected, |row| row.bg(rgb(ui_theme::soft_hover())))
            // 鼠标和键盘共用一个高亮：移上去就选中，星标跟着出现。
            .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _, cx| {
                if this.selected != Some(ix) {
                    this.selected = Some(ix);
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| this.launch(ix, window, cx)))
            .child(
                svg()
                    .path(action_icon(&action.target))
                    .flex_none()
                    .size(px(16.))
                    .text_color(rgb(ui_theme::muted())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .type_style(TYPE.ui)
                    .child(action.label.clone()),
            )
            .when(in_common, |row| {
                row.child(
                    div()
                        .flex_none()
                        .type_style(TYPE.text_xsmall)
                        .text_color(rgb(ui_theme::muted()))
                        .child(action.kind.label()),
                )
            })
            .when_some(digit_key(in_common, ix), |row, digit| {
                row.child(Keybind::new([digit]).backdrop(backdrop))
            })
            .child(div().flex_none().w(px(20.)).when(selected, |slot| {
                slot.child(
                    Button::new(("new-session-pin", ix))
                        .quiet()
                        .small()
                        .icon(if pinned {
                            IconName::StarFill
                        } else {
                            IconName::Star
                        })
                        .icon_action(if pinned {
                            t!("new.unfavorite")
                        } else {
                            t!("new.favorite")
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // 星标在行里，别让这一下也算点了行。
                            cx.stop_propagation();
                            this.toggle_pin(key.clone(), cx);
                        })),
                )
            }))
    }
}

/// 常用项第 `ix` 行的数字键（只有前 9 项有）。
fn digit_key(in_common: bool, ix: usize) -> Option<Keystroke> {
    const DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];
    let digit = DIGITS.get(ix).filter(|_| in_common)?;
    Keystroke::parse(digit).ok()
}

/// 分隔线：上下各 2，横向顶到面板边，和组件库的菜单一致。
fn separator() -> gpui::Div {
    div()
        .flex_none()
        .h(px(1.))
        .my(px(2.))
        .mx(px(-PADDING))
        .bg(ui_theme::hair())
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
            menu = menu.item(MenuItem::new(t!("new.not_connected")).disabled(true));
            continue;
        };
        if directory.projects.is_empty() {
            menu = menu.item(MenuItem::new(t!("list.no_projects")).disabled(true));
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
        MenuItem::new(t!("menu.new_workset"))
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
    fn common_comes_first_without_a_header_then_grouped_sections() {
        let rows = [row(0, "a"), row(0, "b"), row(1, "c"), row(2, "d")];
        let slots = slots(&rows);
        assert_eq!(
            slots,
            vec![
                Slot::Row(0),
                Slot::Row(1),
                Slot::Separator,
                Slot::Header(1),
                Slot::Row(2),
                Slot::Header(2),
                Slot::Row(3),
            ]
        );
        assert_eq!(row_slots(&slots), vec![0, 1, 4, 6]);
    }

    #[test]
    fn no_separator_without_common_items() {
        let rows = [row(1, "a"), row(2, "b")];
        assert_eq!(
            slots(&rows),
            vec![Slot::Header(1), Slot::Row(0), Slot::Header(2), Slot::Row(1)]
        );
        assert_eq!(slots(&[row(0, "a")]), vec![Slot::Row(0)]);
    }

    #[test]
    fn digits_pick_only_common_items() {
        let rows = [row(0, "a"), row(0, "b"), row(1, "c")];
        assert_eq!(pinned_by_digit(&rows, "1"), Some(0));
        assert_eq!(pinned_by_digit(&rows, "2"), Some(1));
        assert_eq!(pinned_by_digit(&rows, "3"), None);
        assert_eq!(pinned_by_digit(&rows, "0"), None);
        assert_eq!(pinned_by_digit(&rows, "a"), None);
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
        assert_eq!(action_icon(&terminal), IconName::Terminal.path());
        assert_eq!(
            action_icon(&NewSessionTarget::BlankTerminal),
            IconName::Terminal.path()
        );
    }
}
