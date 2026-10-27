//! 产品级智能体与自动化工作台。
//!
//! 智能体是持久化产品对象，当前只注册 Pi 执行引擎。它本身不是进程或对话；
//! 自动化通过独立一级入口引用智能体，并在触发时创建 Run。
//!
//! 跟 `git_panel` / `settings` 同一套路：`impl Workspace` 方法从本模块拆出，
//! 字段仍声明在 `main.rs` 的 `Workspace` 里。数据修改在 `workspace.rs`，
//! 智能体页面在 `view.rs`，自动化页面在 `automation_view.rs`。

use chrono::{Local, TimeZone, Utc};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Toggle, ToggleGroup, ToggleVariants as _};
use gpui_component::input::{Input, InputEvent, InputState, NumberInput, Textarea, TextareaState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_component::switch::Switch;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::tag::Tag;
use gpui_component::{
    ActiveTheme as _, Disableable as _, FocusableExt as _, Icon, IconName, Sizable as _,
    StyledExt as _,
};
pub(crate) use smelt_ui::Button;
use smelt_ui::ListRow;

use crate::workspace_sessions::NewAcpSessionRequest;
use crate::{AgentStatus, CapabilitiesTab, Workspace, WorkspaceRoute, acp_view, settings};
use smelt_model::pi_plugin_catalog::PiPlugin;
use smelt_ui::IconAction as _;

/// 一个智能体：在哪台机器上、叫什么。定义存在那台机器的运行时里，不同机器上的 id 互不
/// 相干，所以读、改、开对话都要连机器一起说。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AgentRef {
    pub(crate) runtime: crate::RuntimeKey,
    pub(crate) id: String,
}

impl AgentRef {
    pub(crate) fn new(runtime: crate::RuntimeKey, id: String) -> Self {
        Self { runtime, id }
    }

    /// 它现在的定义；机器没挂、还没读到或已经删了是 `None`。
    pub(crate) fn definition(&self, cx: &App) -> Option<settings::AgentDefinition> {
        settings::agent_definition(&self.runtime, &self.id, cx)
    }
}

/// 智能体面的瞬时 UI。导航页在 [`crate::WorkspaceNav`]；这里只放选中项和编辑器。
pub(crate) struct AgentsSurface {
    /// 落盘：工作台最后选中的本机定义，新建自动化拿它当默认。自动化眼下只在本机，
    /// 选中远程机器上的智能体不记。
    pub(crate) selected_id: Option<String>,
    pub(crate) editor: Option<AgentEditor>,
    pub(crate) error: Option<String>,
    /// 正在等二次确认删除的智能体。
    pub(crate) pending_delete: Option<AgentDeleteTarget>,
    /// 正在等二次确认删除的记忆。
    pub(crate) pending_memory_delete: Option<MemoryDeleteTarget>,
    /// 从侧栏「新对话」进来、还没有可对话的智能体。下一次新建走草稿，不是普通创建。
    pub(crate) arm_conversation_on_create: bool,
    /// 这次「新对话」新建的草稿。点「开始对话」才设默认；返回则删掉。
    pub(crate) conversation_draft: Option<AgentRef>,
    /// 已经请运行时监听记忆目录的智能体（见 `agent.memory.watch`）。监听在运行时进程里，
    /// 运行时断线（换了进程）时清掉那台机器的，下次重取记忆时重新登记。
    pub(crate) watched_memories: std::collections::HashSet<AgentRef>,
}

impl AgentsSurface {
    pub(crate) fn from_persisted(selected_id: Option<String>) -> Self {
        Self {
            selected_id,
            editor: None,
            error: None,
            pending_delete: None,
            pending_memory_delete: None,
            arm_conversation_on_create: false,
            conversation_draft: None,
            watched_memories: std::collections::HashSet::new(),
        }
    }
}

pub(crate) struct AgentDeleteTarget {
    agent: AgentRef,
    name: String,
    conversations: usize,
}

pub(crate) struct MemoryDeleteTarget {
    agent_id: String,
    layer: MemoryLayer,
    file_name: String,
    name: String,
}

pub(crate) struct AutomationDeleteTarget {
    id: String,
    name: String,
    runs: usize,
}

/// 自动化目录根页的分段：任务表 / 全局 Run。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AutomationCatalogTab {
    #[default]
    Tasks,
    Runs,
}

/// 自动化面的瞬时 UI。Run 现场不进项目会话列表。
pub(crate) struct AutomationsSurface {
    pub(crate) editor: Option<AutomationEditor>,
    pub(crate) live_view: Option<Entity<acp_view::AcpView>>,
    pub(crate) live_sub: Option<Subscription>,
    pub(crate) error: Option<String>,
    pub(crate) catalog_tab: AutomationCatalogTab,
    pub(crate) template_filter: Option<automation_templates::AutomationTemplateCategory>,
    /// 编辑器里选中的那一次运行。`None` 表示右侧仍是任务表单。
    pub(crate) selected_run_id: Option<String>,
    /// 正在等二次确认删除的自动化。
    pub(crate) pending_delete: Option<AutomationDeleteTarget>,
    /// 选中运行的落库内容。后台读好再放进来，render 里不读库。
    pub(crate) run_detail: Option<automation_run_detail::AutomationRunDetail>,
    /// 正在后台读取的那份运行版本，防止逐帧重复排读取。
    pub(crate) run_detail_loading: Option<automation_run_detail::RunDetailKey>,
}

impl Default for AutomationsSurface {
    fn default() -> Self {
        Self {
            editor: None,
            live_view: None,
            live_sub: None,
            error: None,
            catalog_tab: AutomationCatalogTab::Tasks,
            template_filter: None,
            selected_run_id: None,
            pending_delete: None,
            run_detail: None,
            run_detail_loading: None,
        }
    }
}

pub(crate) struct AgentEditor {
    agent: AgentRef,
    name: Entity<InputState>,
    /// 名字平时当标题看，点一下才变成输入框，避免一进编辑器就闪光标。
    name_editing: bool,
    instructions: Entity<TextareaState>,
    /// 绑定参考链接用的输入框。目录走原生选择框，链接只能手输。
    context_link: Entity<InputState>,
    /// 智能体那台机器扫出的插件目录，取自 Workspace 的 Pi 配置快照；快照更新时跟着换。
    plugins: Vec<PiPlugin>,
    /// 运行时那台机器上 `~/.pi/agent/mcp.json` 里的服务器，同上。
    mcp_servers: Vec<smelt_model::pi_mcp::PiMcpServer>,
    /// 同理，记忆不在渲染里扫：打开编辑器、运行时报记忆目录有变化、有对话停下来、
    /// 点刷新或删除后才重扫。
    memories: AgentMemories,
    /// 当前页签。每次打开编辑器回到配置。
    tab: AgentEditorTab,
    /// 记忆页签的搜索框和筛选。
    memory_query: Entity<InputState>,
    memory_filter: MemoryFilter,
    /// 记忆页签里展开看全文的那条记忆。只是看，改仍走系统编辑器。
    expanded_memory: Option<(MemoryLayer, String)>,
    _subscriptions: Vec<Subscription>,
}

impl AgentEditor {
    /// 换上 `runtime` 那台机器新给的插件和 MCP 目录。别的机器的目录不是这个智能体能用的，
    /// 不换。
    pub(crate) fn set_pi_catalog(
        &mut self,
        runtime: &crate::RuntimeKey,
        plugins: Vec<PiPlugin>,
        mcp_servers: Vec<smelt_model::pi_mcp::PiMcpServer>,
    ) {
        if self.agent.runtime != *runtime {
            return;
        }
        self.plugins = plugins;
        self.mcp_servers = mcp_servers;
    }
}

/// 记忆卡片的两层数据：智能体自己的，和所有智能体共享的用户全局层。由运行时列出。
pub(crate) use smelt_model::agent_memory::AgentMemories;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryLayer {
    Agent,
    User,
}

impl MemoryLayer {
    pub(crate) fn wire(self) -> smelt_model::control_api::AgentMemoryLayer {
        match self {
            Self::Agent => smelt_model::control_api::AgentMemoryLayer::Agent,
            Self::User => smelt_model::control_api::AgentMemoryLayer::User,
        }
    }
}

/// 会话是否停在两步之间。记忆的读写元数据在工具结果里落库，模型还在想或工具还在跑
/// 时重扫可能赶在写文件之后、记元数据之前。
pub(crate) fn session_phase_settled(phase: smelt_client::daemon_state::DaemonPhase) -> bool {
    use smelt_client::daemon_state::DaemonPhase;
    !matches!(
        phase,
        DaemonPhase::Connecting | DaemonPhase::Thinking | DaemonPhase::ExecutingTool
    )
}

/// 编辑页的两个页签。配置是设一次就不怎么动的东西；记忆是会一直长的数据，单独一页，
/// 不把插件和 MCP 挤到后面去。
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) enum AgentEditorTab {
    #[default]
    Config,
    Memory,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) enum MemoryFilter {
    #[default]
    All,
    Pending,
    Rules,
    Facts,
}

impl MemoryFilter {
    const ALL: [Self; 4] = [Self::All, Self::Pending, Self::Rules, Self::Facts];

    fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Pending => "待确认",
            Self::Rules => "规则",
            Self::Facts => "事实",
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            Self::All => "所有记忆",
            Self::Pending => "没生效、等你采纳的记忆",
            Self::Rules => "你本人的情况和你纠正过的做法，每轮都带上",
            Self::Facts => "项目、资源一类的信息，用到时才召回",
        }
    }

    fn matches(self, entry: &smelt_model::agent_memory::MemoryEntry) -> bool {
        let rule = smelt_model::agent_memory::is_rule_memory_kind(entry.kind.as_deref());
        match self {
            Self::All => true,
            Self::Pending => entry.pending_reason.is_some(),
            Self::Rules => rule,
            Self::Facts => !rule,
        }
    }
}

/// 记忆页签里一层要显示的记忆。搜索不分大小写，名字、描述、文件名、正文任一命中即可。
/// 待确认要用户处理，排最前；其余按最近用过排，没用过的垫底，同档按名字。
pub(crate) fn visible_memories<'a>(
    entries: &'a [smelt_model::agent_memory::MemoryEntry],
    query: &str,
    filter: MemoryFilter,
) -> Vec<&'a smelt_model::agent_memory::MemoryEntry> {
    let query = query.trim().to_lowercase();
    let mut visible: Vec<_> = entries
        .iter()
        .filter(|entry| filter.matches(entry))
        .filter(|entry| {
            query.is_empty()
                || [
                    &entry.name,
                    &entry.description,
                    &entry.file_name,
                    &entry.body,
                ]
                .iter()
                .any(|field| field.to_lowercase().contains(&query))
        })
        .collect();
    visible.sort_by(|a, b| {
        b.pending_reason
            .is_some()
            .cmp(&a.pending_reason.is_some())
            .then_with(|| b.last_used.cmp(&a.last_used))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    visible
}

/// 配置页摘要行的条数。只报数，不列条目，记忆再多也是一行。
pub(crate) fn memory_summary_text(agent: usize, user: usize) -> String {
    if agent + user == 0 {
        "还没有记忆".to_string()
    } else {
        format!("这个智能体 {agent} 条 · 全局 {user} 条")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScheduleKind {
    Daily,
    Weekdays,
    Weekly,
    Interval,
}

impl ScheduleKind {
    fn clock_kinds() -> [Self; 3] {
        [Self::Daily, Self::Weekdays, Self::Weekly]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Daily => "每天",
            Self::Weekdays => "工作日",
            Self::Weekly => "每周",
            Self::Interval => "每隔",
        }
    }

    fn is_interval(self) -> bool {
        matches!(self, Self::Interval)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IntervalUnit {
    Minutes,
    Hours,
}

impl IntervalUnit {
    fn label(self) -> &'static str {
        match self {
            Self::Minutes => "分钟",
            Self::Hours => "小时",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IntervalDayScope {
    Everyday,
    Weekdays,
    Weekly,
}

impl IntervalDayScope {
    fn all() -> [Self; 3] {
        [Self::Everyday, Self::Weekdays, Self::Weekly]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Everyday => "每天",
            Self::Weekdays => "工作日",
            Self::Weekly => "每周",
        }
    }

    fn from_days(days: u8) -> Self {
        let days = days & smelt_model::automation::SCHEDULE_ALL_DAYS_MASK;
        if days == 0 || days == smelt_model::automation::SCHEDULE_ALL_DAYS_MASK {
            Self::Everyday
        } else if days == smelt_model::automation::SCHEDULE_WEEKDAYS_MASK {
            Self::Weekdays
        } else {
            Self::Weekly
        }
    }
}

struct ScheduleRuleEditor {
    kind: ScheduleKind,
    interval_unit: IntervalUnit,
    interval_day_scope: IntervalDayScope,
    weekday_mask: u8,
    time: Entity<InputState>,
    interval: Entity<InputState>,
    window_start: Entity<InputState>,
    window_end: Entity<InputState>,
    /// 空窗口默认收起，避免「每隔 15 分钟」第一眼摊开两个「不限」。
    window_open: bool,
}

struct TriggerEntryEditor {
    trigger_kind: settings::AutomationTriggerKindId,
    schedule: ScheduleRuleEditor,
    webhook_secret: String,
}

pub(crate) struct AutomationEditor {
    instance_id: uuid::Uuid,
    action_kind: settings::AutomationActionKindId,
    agent_id: String,
    automation_id: String,
    is_new: bool,
    name: Entity<InputState>,
    /// 名字平时当标题看，点一下才变成输入框，避免一进编辑器就闪光标。
    name_editing: bool,
    prompt: Entity<TextareaState>,
    command: Entity<TextareaState>,
    retained_events: Vec<settings::EventIngress>,
    entries: Vec<TriggerEntryEditor>,
    notification: settings::AutomationNotificationPreset,
    draft_revision: u64,
    saving_revision: Option<u64>,
    /// 重新生成地址这类后台提交不让顶栏「运行一次」进入 loading 状态。
    quiet_save: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

fn save_on_input_event(event: &InputEvent) -> bool {
    matches!(event, InputEvent::Change | InputEvent::Blur)
}

fn identity_name_should_finish_edit(event: &InputEvent) -> bool {
    matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. })
}

#[cfg(test)]
fn identity_name_idle_text<'a>(value: &'a str, placeholder: &'a str) -> (&'a str, bool) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        (placeholder, true)
    } else {
        (trimmed, false)
    }
}

const IDENTITY_NAME_HEIGHT: f32 = 28.;

fn render_identity_name(input: &Entity<InputState>) -> AnyElement {
    editor_field_frame()
        .w_full()
        .child(
            Input::new(input)
                .appearance(false)
                .focus_bordered(false)
                .focus_ring(false)
                .w_full()
                .h(px(IDENTITY_NAME_HEIGHT))
                .px_2()
                .text_lg()
                .font_semibold(),
        )
        .into_any_element()
}

/// 侧栏「新对话」的文案。有默认时带上名字，没有就只写动作。
fn new_conversation_row_label(agent_name: Option<&str>) -> String {
    match agent_name.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => format!("新对话 · {name}"),
        None => "新对话".to_string(),
    }
}

/// 挂着智能体配置的机器，本机在前，其余按显示名。
fn attached_agent_machines(cx: &App) -> Vec<crate::RuntimeKey> {
    let mut keys: Vec<crate::RuntimeKey> = settings::agent_hosts(cx)
        .map(|hosts| hosts.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort_by_cached_key(|key| {
        (
            *key != crate::RuntimeKey::Local,
            crate::runtimes::runtime_name(key, cx),
        )
    });
    keys
}

/// 其中配置已经读到的。
pub(crate) fn agent_machines(cx: &App) -> Vec<crate::RuntimeKey> {
    attached_agent_machines(cx)
        .into_iter()
        .filter(|key| settings::agent_host_loaded(key, cx))
        .collect()
}

/// 选择器里一个能开对话的智能体。
#[derive(Clone)]
pub(crate) struct ConversationAgentPick {
    agent: AgentRef,
    name: String,
}

/// 侧栏「新对话」这一下该做什么。
pub(crate) enum NewConversationIntent {
    /// 没有可对话的智能体。去创建，不要开会话。
    CreateAgent,
    /// 有可对话的智能体，但没有有效默认。让用户选一次。
    PickAgent,
    /// 有有效默认。直接用它开对话。
    Start(AgentRef),
}

/// 跨机器的「新对话」决议。默认智能体记在它所在那台机器的配置里，换默认时清掉别的机器
/// 上记着的，所以同一时间最多一台记着；按机器顺序取第一个有效的。每台机器的判定规则见
/// [`smelt_model::default_conversation::resolve_new_conversation`]。
pub(crate) struct NewConversationPlan {
    pub(crate) intent: NewConversationIntent,
    /// 有效默认的原文名字。只有 [`NewConversationIntent::Start`] 时有值。
    pub(crate) default_name: Option<String>,
    /// 各台机器上能开对话、且不是这次未提交草稿的智能体，按机器顺序。
    pub(crate) ready: Vec<ConversationAgentPick>,
    /// 记着的默认已经对不上任何可对话智能体的机器。调用方应清掉记录。
    pub(crate) stale_defaults: Vec<crate::RuntimeKey>,
}

pub(crate) fn plan_new_conversation(draft: Option<&AgentRef>, cx: &App) -> NewConversationPlan {
    let mut start = None;
    let mut default_name = None;
    let mut ready = Vec::new();
    let mut stale_defaults = Vec::new();
    for key in agent_machines(cx) {
        let Some(config) = settings::agent_hosts(cx).and_then(|hosts| hosts.settings(&key)) else {
            continue;
        };
        let draft_id = draft
            .filter(|draft| draft.runtime == key)
            .map(|draft| draft.id.as_str());
        let resolution = smelt_model::default_conversation::resolve_new_conversation(
            &config.agents,
            config.default_conversation_agent.as_deref(),
            draft_id,
        );
        if resolution.clear_stored_default {
            stale_defaults.push(key.clone());
        }
        if start.is_none()
            && let smelt_model::default_conversation::NewConversationIntent::Start { agent_id } =
                resolution.intent
        {
            start = Some(AgentRef::new(key.clone(), agent_id));
            default_name = resolution.default_name;
        }
        ready.extend(
            resolution
                .ready
                .into_iter()
                .map(|choice| ConversationAgentPick {
                    agent: AgentRef::new(key.clone(), choice.id),
                    name: choice.name,
                }),
        );
    }
    let intent = match start {
        Some(agent) => NewConversationIntent::Start(agent),
        None if ready.is_empty() => NewConversationIntent::CreateAgent,
        None => NewConversationIntent::PickAgent,
    };
    NewConversationPlan {
        intent,
        default_name,
        ready,
        stale_defaults,
    }
}

/// 那台机器的智能体配置最近一次读取或保存失败的原因。
fn agent_host_error(runtime: &crate::RuntimeKey, cx: &App) -> Option<String> {
    settings::agent_hosts(cx)?.machine(runtime)?.error.clone()
}

/// 引用它的自动化有几条。自动化眼下只在本机，别的机器上的智能体没有。
fn agent_automation_count(agent: &AgentRef, cx: &App) -> usize {
    if agent.runtime != crate::RuntimeKey::Local {
        return 0;
    }
    cx.global::<settings::AgentHostState>()
        .automations_for(&agent.id)
        .len()
}

/// 它是不是侧栏「新对话」记着的默认。
fn is_default_conversation_agent(agent: &AgentRef, cx: &App) -> bool {
    settings::agent_hosts(cx)
        .and_then(|hosts| hosts.settings(&agent.runtime))
        .and_then(|config| config.default_conversation_agent.as_deref())
        == Some(agent.id.as_str())
}

/// 设成侧栏默认：记在它那台机器上，别的机器上记着的清掉。
fn set_default_conversation_agent(agent: &AgentRef, cx: &mut App) {
    for key in agent_machines(cx) {
        let target = (key == agent.runtime).then(|| agent.id.clone());
        let current = settings::agent_hosts(cx)
            .and_then(|hosts| hosts.settings(&key))
            .and_then(|config| config.default_conversation_agent.clone());
        if current != target {
            settings::apply_agent_host(&key, move |c| c.default_conversation_agent = target, cx);
        }
    }
}

fn clear_default_conversation_agent(runtime: &crate::RuntimeKey, cx: &mut App) {
    settings::apply_agent_host(runtime, |c| c.default_conversation_agent = None, cx);
}

/// 对话行图标色。只跟状态走：选中已经由行底色表达。
///
/// 空闲选中如果再走强调色，就会和运行中撞色——选中那条永远看不出在不在跑。
fn conversation_icon_color(status: AgentStatus, phase: Option<f32>) -> gpui::Rgba {
    Workspace::session_icon_color(status, phase)
}

/// 这段对话属于哪个智能体：只看创建时写下的绑定。
///
/// 项目里选智能体开的也算。旧的工作台裸对话没有绑定，不算任何智能体。
/// 自动化 Run 虽然可能带着同一个 id，但那不是对话。不从 cwd 反推——项目
/// 路径说明不了是谁，space 子目录也不是另一个智能体。
fn bound_agent_definition_id<'a>(
    definition_id: Option<&'a str>,
    automation_id: Option<&str>,
) -> Option<&'a str> {
    if automation_id.is_some() {
        return None;
    }
    definition_id.map(str::trim).filter(|id| !id.is_empty())
}

/// 侧栏「对话」分组：按最近活动排，新的在前。
pub(crate) fn recent_conversation_order(updated_at: &[u64]) -> Vec<usize> {
    let mut ixes: Vec<usize> = (0..updated_at.len()).collect();
    ixes.sort_by(|left, right| {
        updated_at[*right]
            .cmp(&updated_at[*left])
            .then(left.cmp(right))
    });
    ixes
}

/// 行首头像：智能体取显示名第一个字；没绑智能体的工作台对话标 Pi。
/// 绑了智能体的会话行首头像。没绑（工作台「新对话」、裸引擎）返回 `None`。
pub(crate) fn bound_agent_mark(
    definition_id: Option<&str>,
    automation_id: Option<&str>,
    agents: &[settings::AgentDefinition],
) -> Option<String> {
    let id = bound_agent_definition_id(definition_id, automation_id)?;
    let name = agents
        .iter()
        .find(|agent| agent.id == id)
        .map(|agent| agent_display_name(&agent.name));
    Some(conversation_agent_mark(name.as_deref().or(Some(id))))
}

fn conversation_agent_mark(agent_name: Option<&str>) -> String {
    match agent_name.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => name.chars().next().unwrap_or('智').to_string(),
        None => "Pi".to_string(),
    }
}

fn is_cjk(ch: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&ch)
}

/// 目录和页头上的缩写。中文取前两字，「日志」比单独一个「日」认得出来；
/// 英文取开头的两个字母，像 Pi。
fn agent_catalog_monogram(name: &str) -> String {
    let name = name.trim();
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return "智".to_string();
    };
    if is_cjk(first) {
        return match chars.next() {
            Some(second) if is_cjk(second) => format!("{first}{second}"),
            _ => first.to_string(),
        };
    }
    let mut word = String::new();
    word.push(first);
    for ch in chars {
        if word.chars().count() >= 2 || ch.is_whitespace() || is_cjk(ch) {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            word.push(ch);
        } else {
            break;
        }
    }
    word
}

/// 对话行只展示用户命名或对话自己的自动标题。
fn conversation_row_title(custom_title: Option<&str>, auto_title: Option<&str>) -> String {
    custom_title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .or_else(|| auto_title.map(str::trim).filter(|title| !title.is_empty()))
        .unwrap_or("新对话")
        .to_string()
}

fn compact_instructions(value: &str) -> String {
    excerpt_text(value, 96)
}

fn excerpt_text(value: &str, limit: usize) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = value.chars();
    let mut compact = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        compact.push('…');
    }
    compact
}

/// 产品智能体的执行引擎注册点在 `smelt_core`：运行时也要照同一份名单给手机产出
/// 新建对话选项，注册表留在 GUI crate 里两边就会各注册各的。
pub(crate) use smelt_model::agent_definition::agent_engine_kinds;

fn clock_input_is_editable(value: &str) -> bool {
    if value.len() > 5
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b':')
    {
        return false;
    }
    let mut parts = value.split(':');
    let hour = parts.next().unwrap_or_default();
    let minute = parts.next();
    if parts.next().is_some() || hour.len() > 2 || hour.is_empty() && minute.is_some() {
        return false;
    }
    if !hour.is_empty() && hour.parse::<u8>().ok().is_none_or(|hour| hour > 23) {
        return false;
    }
    minute.is_none_or(|minute| {
        minute.len() <= 2
            && (minute.is_empty() || minute.parse::<u8>().ok().is_some_and(|minute| minute <= 59))
    })
}

fn parse_clock(value: &str) -> Option<(u8, u8)> {
    let (hour, minute) = value.trim().split_once(':')?;
    if hour.is_empty() || minute.is_empty() || hour.len() > 2 || minute.len() > 2 {
        return None;
    }
    let hour = hour.parse::<u8>().ok()?;
    let minute = minute.parse::<u8>().ok()?;
    (hour <= 23 && minute <= 59).then_some((hour, minute))
}

fn schedule_from_values(
    kind: ScheduleKind,
    time: &str,
    interval: &str,
    interval_unit: IntervalUnit,
    weekday_mask: u8,
    window_start: &str,
    window_end: &str,
) -> Result<settings::AutomationSchedule, String> {
    let schedule = match kind {
        ScheduleKind::Daily => {
            let (hour, minute) =
                parse_clock(time).ok_or_else(|| "请输入 00:00 到 23:59 之间的时间".to_string())?;
            settings::AutomationSchedule::Daily { hour, minute }
        }
        ScheduleKind::Weekdays => {
            let (hour, minute) =
                parse_clock(time).ok_or_else(|| "请输入 00:00 到 23:59 之间的时间".to_string())?;
            settings::AutomationSchedule::Weekly {
                days: smelt_model::automation::SCHEDULE_WEEKDAYS_MASK,
                hour,
                minute,
            }
        }
        ScheduleKind::Weekly => {
            let (hour, minute) =
                parse_clock(time).ok_or_else(|| "请输入 00:00 到 23:59 之间的时间".to_string())?;
            let days = weekday_mask & smelt_model::automation::SCHEDULE_ALL_DAYS_MASK;
            if days == 0 {
                return Err("请至少选择一天".to_string());
            }
            if days == smelt_model::automation::SCHEDULE_ALL_DAYS_MASK {
                settings::AutomationSchedule::Daily { hour, minute }
            } else {
                settings::AutomationSchedule::Weekly { days, hour, minute }
            }
        }
        ScheduleKind::Interval => {
            let start = window_start.trim();
            let end = window_end.trim();
            let window = match (start.is_empty(), end.is_empty()) {
                (true, true) => None,
                (false, false) => {
                    let start = parse_clock(start)
                        .ok_or_else(|| "请输入 00:00 到 23:59 之间的时间".to_string())?;
                    let end = parse_clock(end)
                        .ok_or_else(|| "请输入 00:00 到 23:59 之间的时间".to_string())?;
                    Some((start, end))
                }
                _ => return Err("请同时填写开始和结束时间".to_string()),
            };
            let days = weekday_mask & smelt_model::automation::SCHEDULE_ALL_DAYS_MASK;
            let constrained = window.is_some()
                || (days != 0 && days != smelt_model::automation::SCHEDULE_ALL_DAYS_MASK);
            match interval_unit {
                IntervalUnit::Minutes => {
                    let minutes = interval
                        .trim()
                        .parse::<u32>()
                        .ok()
                        .filter(|minutes| (1..=24 * 60).contains(minutes))
                        .ok_or_else(|| "执行间隔需为 1 到 1440 分钟".to_string())?;
                    if !constrained {
                        settings::AutomationSchedule::EveryMinutes { minutes }
                    } else {
                        let ((start_hour, start_minute), (end_hour, end_minute)) =
                            window.unwrap_or(((0, 0), (23, 59)));
                        settings::AutomationSchedule::During {
                            minutes,
                            days: if days == 0 {
                                smelt_model::automation::SCHEDULE_ALL_DAYS_MASK
                            } else {
                                days
                            },
                            start_hour,
                            start_minute,
                            end_hour,
                            end_minute,
                        }
                    }
                }
                IntervalUnit::Hours => {
                    let hours = interval
                        .trim()
                        .parse::<u32>()
                        .ok()
                        .filter(|hours| (1..=24 * 7).contains(hours))
                        .ok_or_else(|| "执行间隔需为 1 到 168 小时".to_string())?;
                    if !constrained {
                        settings::AutomationSchedule::EveryHours { hours }
                    } else {
                        let ((start_hour, start_minute), (end_hour, end_minute)) =
                            window.unwrap_or(((0, 0), (23, 59)));
                        settings::AutomationSchedule::During {
                            minutes: hours.saturating_mul(60),
                            days: if days == 0 {
                                smelt_model::automation::SCHEDULE_ALL_DAYS_MASK
                            } else {
                                days
                            },
                            start_hour,
                            start_minute,
                            end_hour,
                            end_minute,
                        }
                    }
                }
            }
        }
    };
    schedule.validate()?;
    Ok(schedule)
}

fn trigger_from_editor(
    editor: &AutomationEditor,
    cx: &App,
) -> Result<settings::AutomationTrigger, String> {
    let mut trigger = settings::AutomationTrigger {
        events: editor.retained_events.clone(),
        ..Default::default()
    };
    for entry in &editor.entries {
        match entry.trigger_kind {
            settings::AutomationTriggerKindId::Calendar
            | settings::AutomationTriggerKindId::Interval => {
                trigger
                    .schedules
                    .push(schedule_from_values_for_rule(&entry.schedule, cx)?);
            }
            settings::AutomationTriggerKindId::Webhook => {
                let secret = entry.webhook_secret.trim();
                let secret = if secret.is_empty() {
                    smelt_model::automation::new_webhook_secret()
                } else {
                    secret.to_string()
                };
                trigger.webhooks.push(settings::WebhookIngress {
                    endpoint: secret.clone(),
                    secret: Some(secret),
                });
            }
        }
    }
    trigger.validate()?;
    Ok(trigger)
}

impl AutomationEditor {
    fn action(&self, cx: &App) -> settings::AutomationAction {
        match self.action_kind {
            settings::AutomationActionKindId::Agent => {
                let prompt = self.prompt.read(cx).value().trim().to_string();
                settings::AutomationAction::Agent {
                    agent_definition_id: self.agent_id.clone(),
                    prompt: (!prompt.is_empty()).then_some(prompt),
                }
            }
            settings::AutomationActionKindId::Shell => settings::AutomationAction::shell(
                self.command.read(cx).value().trim().to_string(),
                Vec::new(),
            ),
        }
    }

    fn missing_fields(&self, cx: &App) -> Vec<&'static str> {
        // 自动化眼下只在本机，引用的是本机的智能体。
        let agent_exists = !self.agent_id.is_empty()
            && settings::agent_definition(&crate::RuntimeKey::Local, &self.agent_id, cx).is_some();
        let has_schedule = self
            .entries
            .iter()
            .any(|entry| entry.trigger_kind != settings::AutomationTriggerKindId::Webhook);
        automation_missing_fields(
            &self.name.read(cx).value(),
            trigger_from_editor(self, cx).is_ok(),
            has_schedule,
            &self.action(cx),
            agent_exists,
        )
    }
}

fn schedule_from_values_for_rule(
    rule: &ScheduleRuleEditor,
    cx: &App,
) -> Result<settings::AutomationSchedule, String> {
    let weekday_mask = if rule.kind == ScheduleKind::Interval {
        match rule.interval_day_scope {
            IntervalDayScope::Everyday => smelt_model::automation::SCHEDULE_ALL_DAYS_MASK,
            IntervalDayScope::Weekdays => smelt_model::automation::SCHEDULE_WEEKDAYS_MASK,
            IntervalDayScope::Weekly => {
                let days = rule.weekday_mask & smelt_model::automation::SCHEDULE_ALL_DAYS_MASK;
                if days == 0 {
                    return Err("请至少选择一天".to_string());
                }
                days
            }
        }
    } else {
        rule.weekday_mask
    };
    schedule_from_values(
        rule.kind,
        &rule.time.read(cx).value(),
        &rule.interval.read(cx).value(),
        rule.interval_unit,
        weekday_mask,
        &rule.window_start.read(cx).value(),
        &rule.window_end.read(cx).value(),
    )
}

fn new_schedule_rule(
    window: &mut Window,
    cx: &mut Context<Workspace>,
    schedule: Option<&settings::AutomationSchedule>,
) -> ScheduleRuleEditor {
    let mut hour = 9_u8;
    let mut minute = 0_u8;
    let mut interval = 15_u32;
    let mut kind = ScheduleKind::Daily;
    let mut interval_unit = IntervalUnit::Minutes;
    let mut weekday_mask = smelt_model::automation::SCHEDULE_ALL_DAYS_MASK;
    let mut interval_day_scope = IntervalDayScope::Everyday;
    let mut window_start_value = String::new();
    let mut window_end_value = String::new();
    match schedule {
        Some(settings::AutomationSchedule::Weekly {
            days,
            hour: start_hour,
            minute: start_minute,
        }) => {
            hour = *start_hour;
            minute = *start_minute;
            kind = if *days == smelt_model::automation::SCHEDULE_WEEKDAYS_MASK {
                ScheduleKind::Weekdays
            } else {
                ScheduleKind::Weekly
            };
            weekday_mask = *days;
            interval_day_scope = IntervalDayScope::from_days(*days);
        }
        Some(settings::AutomationSchedule::Daily {
            hour: start_hour,
            minute: start_minute,
        }) => {
            hour = *start_hour;
            minute = *start_minute;
            kind = ScheduleKind::Daily;
        }
        Some(settings::AutomationSchedule::EveryMinutes { minutes }) => {
            interval = (*minutes).max(1);
            kind = ScheduleKind::Interval;
        }
        Some(settings::AutomationSchedule::EveryHours { hours }) => {
            interval = (*hours).max(1);
            kind = ScheduleKind::Interval;
            interval_unit = IntervalUnit::Hours;
        }
        Some(settings::AutomationSchedule::During {
            minutes,
            days,
            start_hour,
            start_minute,
            end_hour,
            end_minute,
        }) => {
            hour = *start_hour;
            minute = *start_minute;
            if *minutes >= 60 && *minutes % 60 == 0 {
                interval = *minutes / 60;
                interval_unit = IntervalUnit::Hours;
            } else {
                interval = (*minutes).max(1);
            }
            kind = ScheduleKind::Interval;
            weekday_mask = if *days == 0 {
                smelt_model::automation::SCHEDULE_ALL_DAYS_MASK
            } else {
                *days
            };
            interval_day_scope = IntervalDayScope::from_days(*days);
            if !(*start_hour == 0 && *start_minute == 0 && *end_hour == 23 && *end_minute == 59) {
                window_start_value = format!("{start_hour:02}:{start_minute:02}");
                window_end_value = format!("{end_hour:02}:{end_minute:02}");
            }
        }
        None => {
            interval = 1;
        }
    }
    let time = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("09:00")
            .validate(|value, _| clock_input_is_editable(value))
            .default_value(format!("{hour:02}:{minute:02}"))
    });
    let interval = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("1")
            .default_value(interval.to_string())
            .step(1.)
            .min(1.)
            .max((24 * 60) as f64)
    });
    let window_open = !window_start_value.is_empty();
    let window_start = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("开始")
            .validate(|value, _| clock_input_is_editable(value))
            .default_value(window_start_value)
    });
    let window_end = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("结束")
            .validate(|value, _| clock_input_is_editable(value))
            .default_value(window_end_value)
    });
    ScheduleRuleEditor {
        kind,
        interval_unit,
        interval_day_scope,
        weekday_mask,
        time,
        interval,
        window_start,
        window_end,
        window_open,
    }
}

fn webhook_ingress_secret(webhook: &settings::WebhookIngress) -> String {
    webhook
        .secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let endpoint = webhook.endpoint.trim();
            (!endpoint.is_empty()).then(|| endpoint.to_string())
        })
        .unwrap_or_else(smelt_model::automation::new_webhook_secret)
}

fn new_trigger_entry(
    window: &mut Window,
    cx: &mut Context<Workspace>,
    trigger_kind: settings::AutomationTriggerKindId,
    schedule: Option<&settings::AutomationSchedule>,
    webhook_secret: Option<String>,
) -> TriggerEntryEditor {
    let interval_default = settings::AutomationSchedule::EveryMinutes { minutes: 15 };
    let mut schedule = match (trigger_kind, schedule) {
        (_, Some(schedule)) => new_schedule_rule(window, cx, Some(schedule)),
        (settings::AutomationTriggerKindId::Interval, None) => {
            new_schedule_rule(window, cx, Some(&interval_default))
        }
        _ => new_schedule_rule(window, cx, None),
    };
    match trigger_kind {
        settings::AutomationTriggerKindId::Interval => schedule.kind = ScheduleKind::Interval,
        settings::AutomationTriggerKindId::Calendar if schedule.kind.is_interval() => {
            schedule.kind = ScheduleKind::Daily;
        }
        _ => {}
    }
    TriggerEntryEditor {
        trigger_kind,
        schedule,
        webhook_secret: webhook_secret
            .filter(|secret| !secret.trim().is_empty())
            .unwrap_or_else(smelt_model::automation::new_webhook_secret),
    }
}

fn subscribe_trigger_entry_inputs(
    subscriptions: &mut Vec<Subscription>,
    entry: &TriggerEntryEditor,
    cx: &mut Context<Workspace>,
) {
    for input in [
        &entry.schedule.time,
        &entry.schedule.interval,
        &entry.schedule.window_start,
        &entry.schedule.window_end,
    ] {
        subscriptions.push(cx.subscribe(input, {
            move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.clear_automation_editor_error(cx);
                }
            }
        }));
    }
}

fn render_trigger_entries(
    editor: &AutomationEditor,
    entity: Entity<Workspace>,
    cx: &App,
) -> AnyElement {
    let add = render_add_trigger_button(entity.clone());
    div()
        .flex()
        .flex_col()
        .gap_3()
        .children(
            editor.entries.iter().enumerate().map(|(index, entry)| {
                render_trigger_entry(index, entry, editor, entity.clone(), cx)
            }),
        )
        .child(add)
        .into_any_element()
}

fn render_add_trigger_button(entity: Entity<Workspace>) -> AnyElement {
    div()
        .w_full()
        .h(crate::ui_theme::height::xl())
        .rounded_full()
        .border_1()
        .border_dashed()
        .border_color(crate::ui_theme::card_stroke())
        .flex()
        .items_center()
        .child(
            Button::new("automation-add-trigger")
                .quiet()
                .w_full()
                .icon(IconName::Plus)
                .label("添加触发器")
                .dropdown_menu(move |mut menu, _, _| {
                    let mut saw_advanced = false;
                    for preset in settings::BUILTIN_AUTOMATION_TRIGGER_PRESETS {
                        if preset.is_advanced() && !saw_advanced {
                            menu = menu.separator();
                            saw_advanced = true;
                        }
                        let entity = entity.clone();
                        let preset = *preset;
                        menu = menu.item(
                            PopupMenuItem::new(format!("{} · {}", preset.label(), preset.hint()))
                                .icon(trigger_preset_icon(preset))
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |workspace, cx| {
                                        workspace.add_trigger_preset(preset, window, cx);
                                    });
                                }),
                        );
                    }
                    menu
                }),
        )
        .into_any_element()
}

fn trigger_preset_icon(preset: settings::AutomationTriggerPresetId) -> Icon {
    match preset {
        settings::AutomationTriggerPresetId::Webhook => Icon::new(gpui_kit_assets::IconName::Webhook),
        settings::AutomationTriggerPresetId::Hourly
        | settings::AutomationTriggerPresetId::Interval => Icon::new(gpui_kit_assets::IconName::Clock),
        settings::AutomationTriggerPresetId::Daily
        | settings::AutomationTriggerPresetId::Weekly => IconName::Calendar.into(),
    }
}

fn render_trigger_entry(
    index: usize,
    entry: &TriggerEntryEditor,
    editor: &AutomationEditor,
    entity: Entity<Workspace>,
    cx: &App,
) -> AnyElement {
    let remove_entity = entity.clone();
    let title = trigger_entry_title(entry, cx);
    let hint = trigger_entry_hint(entry);
    let show_hint = title != hint;
    let icon: Icon = match entry.trigger_kind {
        settings::AutomationTriggerKindId::Webhook => Icon::new(gpui_kit_assets::IconName::Webhook),
        settings::AutomationTriggerKindId::Interval => Icon::new(gpui_kit_assets::IconName::Clock),
        settings::AutomationTriggerKindId::Calendar => IconName::Calendar.into(),
    };
    let body = match entry.trigger_kind {
        settings::AutomationTriggerKindId::Webhook => {
            render_webhook_entry(index, entry, editor, entity, cx)
        }
        settings::AutomationTriggerKindId::Calendar
        | settings::AutomationTriggerKindId::Interval => {
            render_schedule_rule(index, &entry.schedule, false, entity, cx)
        }
    };
    let row_key = entry.schedule.time.entity_id();
    automation_form_shell()
        .id(("automation-trigger", row_key))
        .px_4()
        .py_3()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_start()
                .gap_3()
                .child(automation_trigger_glyph(icon))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_medium()
                                .text_color(rgb(crate::ui_theme::text_bright()))
                                .child(title),
                        )
                        .when(show_hint, |col| {
                            col.child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(crate::ui_theme::text_muted()))
                                    .child(hint),
                            )
                        }),
                )
                .child(
                    compact_chip("automation-remove-trigger")
                        .icon(crate::trash_icon())
                        .icon_action("删除触发器")
                        .on_click(move |_, _, cx| {
                            remove_entity.update(cx, |workspace, cx| {
                                workspace.remove_trigger_entry(index, cx);
                            });
                        }),
                ),
        )
        .child(body)
        .into_any_element()
}

/// 目录和编辑器里的小图标。不染色，底就是悬停灰。
fn neutral_icon_mark(icon: impl Into<Icon>, size: f32) -> AnyElement {
    div()
        .size(px(size))
        .rounded(crate::ui_theme::row_radius())
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(crate::ui_theme::bg_hover()))
        .child(
            Icon::new(icon)
                .size(px((size * 0.5).round()))
                .text_color(rgb(crate::ui_theme::text_mid())),
        )
        .into_any_element()
}

fn automation_trigger_glyph(icon: Icon) -> AnyElement {
    neutral_icon_mark(icon, 32.)
}

fn trigger_entry_title(entry: &TriggerEntryEditor, cx: &App) -> String {
    match entry.trigger_kind {
        settings::AutomationTriggerKindId::Webhook => "当此 webhook 收到请求时".to_string(),
        settings::AutomationTriggerKindId::Calendar => match entry.schedule.kind {
            ScheduleKind::Daily => "每天".to_string(),
            ScheduleKind::Weekdays => "工作日".to_string(),
            ScheduleKind::Weekly => "每周".to_string(),
            ScheduleKind::Interval => interval_entry_title(entry, cx),
        },
        settings::AutomationTriggerKindId::Interval => interval_entry_title(entry, cx),
    }
}

fn interval_entry_title(entry: &TriggerEntryEditor, cx: &App) -> String {
    interval_title(
        entry.schedule.interval_unit,
        &entry.schedule.interval.read(cx).value(),
    )
}

fn interval_title(unit: IntervalUnit, value: &str) -> String {
    let value = value.trim();
    match unit {
        _ if value.is_empty() => "间隔".to_string(),
        IntervalUnit::Hours if value == "1" => "每小时".to_string(),
        IntervalUnit::Hours => format!("每隔 {value} 小时"),
        IntervalUnit::Minutes => format!("每隔 {value} 分钟"),
    }
}

fn trigger_entry_hint(entry: &TriggerEntryEditor) -> &'static str {
    match entry.trigger_kind {
        settings::AutomationTriggerKindId::Webhook => "当此 webhook 收到请求时",
        settings::AutomationTriggerKindId::Calendar => match entry.schedule.kind {
            ScheduleKind::Daily => "每天在选定的时间",
            ScheduleKind::Weekdays => "工作日在选定的时间",
            ScheduleKind::Weekly => "每周在选定的一天",
            ScheduleKind::Interval => "按固定间隔执行",
        },
        settings::AutomationTriggerKindId::Interval => "按固定间隔执行，可限制星期和时段",
    }
}

fn render_notification_field(
    editor: &AutomationEditor,
    entity: Entity<Workspace>,
    _cx: &App,
) -> AnyElement {
    let selected = editor.notification;
    let picker_entity = entity;
    let picker = automation_form_row().child(
        Button::new("automation-notification")
            .quiet()
            .w_full()
            .dropdown_caret(true)
            .icon(notification_preset_icon(selected))
            .label(selected.label())
            .dropdown_menu(move |mut menu, _, _| {
                for preset in settings::AutomationNotificationPreset::ALL {
                    let entity = picker_entity.clone();
                    menu = menu.item(
                        PopupMenuItem::new(preset.label())
                            .icon(notification_preset_icon(preset))
                            .checked(preset == selected)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |workspace, cx| {
                                    workspace.set_automation_notification(preset, cx);
                                });
                            }),
                    );
                }
                menu
            }),
    );
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(automation_section_label("通知"))
        .child(picker)
        .into_any_element()
}

/// 权限说明。这里不能切换运行方式，所以不做成和通知一样的设置卡。
fn render_automation_access_note(is_agent: bool) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_medium()
                .text_color(rgb(crate::ui_theme::text_muted()))
                .child("无人值守 · 完全访问"),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_muted()))
                .child(automation_access_note(is_agent)),
        )
        .into_any_element()
}

fn automation_access_note(is_agent: bool) -> &'static str {
    if is_agent {
        "运行时不会停下来等你批准，可以读写本机文件、执行命令。在智能体自己的空间里运行，能读到它在对话里攒下的文件和绑定的目录；你的项目不会自动带上，要处理的仓库请在指令里写明路径。"
    } else {
        "运行时不会停下来等你批准，命令以你的身份执行，可以读写本机文件。每次都在这条自动化自己的目录里执行。"
    }
}

fn notification_preset_icon(preset: settings::AutomationNotificationPreset) -> Icon {
    match preset {
        settings::AutomationNotificationPreset::Off => Icon::new(gpui_kit_assets::IconName::BellOff),
        settings::AutomationNotificationPreset::App => IconName::Bell.into(),
    }
}

fn render_webhook_entry(
    index: usize,
    entry: &TriggerEntryEditor,
    editor: &AutomationEditor,
    entity: Entity<Workspace>,
    cx: &App,
) -> AnyElement {
    let base = cx
        .global::<settings::AgentHostState>()
        .webhook_base_url
        .clone()
        .unwrap_or_else(|| smelt_model::automation::DEFAULT_WEBHOOK_BASE_URL.to_string());
    let stored = cx
        .global::<settings::AgentHostState>()
        .automations
        .iter()
        .find(|automation| automation.id == editor.automation_id);
    let paused = stored.is_some_and(|automation| !automation.enabled);
    let live_url = (!editor.is_new && !entry.webhook_secret.is_empty())
        .then(|| smelt_model::automation::webhook_url(&base, &entry.webhook_secret));
    let row_key = entry.schedule.time.entity_id();
    let copy_id = format!("automation-copy-webhook-{row_key}");
    let curl_id = format!("automation-copy-webhook-curl-{row_key}");
    let regen_id = format!("automation-regen-webhook-{row_key}");
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(if live_url.is_some() {
                    "向此地址发送 POST 请求即可触发运行。"
                } else {
                    "保存此自动化以查看 webhook 配置。"
                }),
        )
        .when(paused, |body| {
            body.child(
                div()
                    .text_xs()
                    .text_color(rgb(crate::ui_theme::text_muted()))
                    .child("自动化已暂停，启用后可继续接收请求。"),
            )
        })
        .children({
            let copy_entity = entity.clone();
            live_url.clone().map(move |url| {
                let copy_url = url.clone();
                let copy_entity = copy_entity.clone();
                let copy_id_click = copy_id.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px_3()
                            .py_2()
                            .rounded(px(8.))
                            .border_1()
                            .border_color(rgb(crate::ui_theme::border_loud()))
                            .bg(rgb(crate::ui_theme::bg_column()))
                            .text_xs()
                            .text_color(rgb(crate::ui_theme::text_bright()))
                            .child(url),
                    )
                    .child(
                        compact_chip("automation-copy-webhook")
                            .label(settings::flash_btn_label(&copy_id, "复制", cx))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()));
                                let copy_id_click = copy_id_click.clone();
                                copy_entity.update(cx, |_, cx| {
                                    settings::flash_button(copy_id_click, "已复制 ✓", cx);
                                    cx.notify();
                                });
                            }),
                    )
            })
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .children({
                    let copy_entity = entity.clone();
                    live_url.map(move |url| {
                        let copy_curl = webhook_curl_example(&url);
                        let copy_entity = copy_entity.clone();
                        let curl_id_click = curl_id.clone();
                        compact_chip("automation-copy-webhook-curl")
                            .label(settings::flash_btn_label(&curl_id, "复制示例", cx))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_curl.clone()));
                                let curl_id_click = curl_id_click.clone();
                                copy_entity.update(cx, |_, cx| {
                                    settings::flash_button(curl_id_click, "已复制 ✓", cx);
                                    cx.notify();
                                });
                            })
                    })
                })
                .when(!editor.is_new, |actions| {
                    let entity = entity.clone();
                    actions.child(
                        compact_chip("automation-regen-webhook")
                            .loading(editor.saving_revision.is_some() && editor.quiet_save)
                            .label(settings::flash_btn_label(&regen_id, "重新生成地址", cx))
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |workspace, cx| {
                                    workspace.regenerate_automation_webhook_secret(index, cx);
                                });
                            }),
                    )
                }),
        )
        .into_any_element()
}

fn webhook_curl_example(url: &str) -> String {
    format!("curl -sS -X POST {url} -H 'Content-Type: application/json' -d '{{\"text\":\"你好\"}}'")
}

fn automation_prompt_hint(is_webhook: bool) -> &'static str {
    if is_webhook {
        "收到的请求内容会附在指令后面；想放到指定位置，用 {{payload}} 或字段变量（如 {{text}}）。留空时直接发送请求内容。长期指令请在智能体设置中配置。"
    } else {
        "每次运行时发送给智能体。长期指令请在智能体设置中配置。"
    }
}

fn render_schedule_rule(
    index: usize,
    rule: &ScheduleRuleEditor,
    can_remove: bool,
    entity: Entity<Workspace>,
    cx: &App,
) -> AnyElement {
    let frequency = (!rule.kind.is_interval()).then(|| {
        let kinds = ScheduleKind::clock_kinds();
        let frequency_entity = entity.clone();
        choice_tabs(
            ("agent-trigger-frequency", index),
            kinds.iter().position(|kind| *kind == rule.kind),
            kinds.iter().map(|kind| Tab::new().label(kind.label())),
            move |ix, cx| {
                if let Some(kind) = kinds.get(ix).copied() {
                    frequency_entity.update(cx, |workspace, cx| {
                        workspace.set_trigger_schedule_kind(index, kind, cx);
                    });
                }
            },
        )
    });
    let clock = |input: &Entity<InputState>, aria: &'static str| {
        framed_text_input(input, aria)
            .w(px(108.))
            .into_any_element()
    };
    let timezone = || {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(local_timezone_label())
    };
    // 星期可以多选，用开关组而不是单选的分段页签。
    let weekday_picker = |id_prefix: &'static str| {
        let mask = rule.weekday_mask;
        let day_entity = entity.clone();
        ToggleGroup::new((id_prefix, index))
            .outline()
            .segmented()
            .flex_none()
            .children((0..7).map(|day_index| {
                Toggle::new(day_index)
                    .w(px(32.))
                    .label(smelt_model::automation::SCHEDULE_DAY_LABELS[day_index])
                    .checked(mask & (1_u8 << day_index) != 0)
            }))
            .on_click(move |checks, _, cx| {
                // 一次只点一格：和当前不一样的那一格就是被点的。
                let Some(day_index) = checks
                    .iter()
                    .enumerate()
                    .position(|(day, on)| *on != (mask & (1_u8 << day) != 0))
                else {
                    return;
                };
                day_entity.update(cx, |workspace, cx| {
                    workspace.toggle_schedule_weekday(index, 1_u8 << day_index, cx);
                });
            })
    };
    let interval_unit = rule.interval_unit;
    let schedule_control = match rule.kind {
        ScheduleKind::Interval => {
            let scopes = IntervalDayScope::all();
            let scope_entity = entity.clone();
            let day_scope = choice_tabs(
                ("automation-interval-days", index),
                scopes
                    .iter()
                    .position(|scope| *scope == rule.interval_day_scope),
                scopes.iter().map(|scope| Tab::new().label(scope.label())),
                move |ix, cx| {
                    if let Some(scope) = scopes.get(ix).copied() {
                        scope_entity.update(cx, |workspace, cx| {
                            workspace.set_interval_day_scope(index, scope, cx);
                        });
                    }
                },
            );
            let mut interval_body = div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            framed_number_input(&rule.interval)
                                .w(px(120.))
                                .into_any_element(),
                        )
                        .child({
                            const UNITS: [IntervalUnit; 2] =
                                [IntervalUnit::Minutes, IntervalUnit::Hours];
                            let unit_entity = entity.clone();
                            choice_tabs(
                                ("automation-interval-unit", index),
                                UNITS.iter().position(|unit| *unit == interval_unit),
                                UNITS.iter().map(|unit| Tab::new().label(unit.label())),
                                move |ix, cx| {
                                    if let Some(unit) = UNITS.get(ix).copied() {
                                        unit_entity.update(cx, |workspace, cx| {
                                            workspace.set_interval_unit(index, unit, cx);
                                        });
                                    }
                                },
                            )
                        }),
                )
                .child(day_scope);
            if rule.interval_day_scope == IntervalDayScope::Weekly {
                interval_body = interval_body.child(weekday_picker("automation-interval-weekday"));
            }
            if rule.window_open {
                let close_entity = entity.clone();
                interval_body = interval_body.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("从"),
                        )
                        .child(clock(&rule.window_start, "开始时间"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("到"),
                        )
                        .child(clock(&rule.window_end, "结束时间"))
                        .child(timezone())
                        .child(
                            compact_chip("automation-interval-window-clear")
                                .label("全天")
                                .on_click(move |_, window, cx| {
                                    close_entity.update(cx, |workspace, cx| {
                                        workspace
                                            .set_interval_window_open(index, false, window, cx);
                                    });
                                }),
                        ),
                );
            } else {
                let open_entity = entity.clone();
                interval_body = interval_body.child(
                    compact_chip("automation-interval-window-open")
                        .label("限制时段")
                        .on_click(move |_, window, cx| {
                            open_entity.update(cx, |workspace, cx| {
                                workspace.set_interval_window_open(index, true, window, cx);
                            });
                        }),
                );
            }
            interval_body.into_any_element()
        }
        ScheduleKind::Daily | ScheduleKind::Weekdays => div()
            .flex()
            .items_center()
            .gap_2()
            .child(clock(
                &rule.time,
                if rule.kind == ScheduleKind::Daily {
                    "每日执行时间"
                } else {
                    "工作日执行时间"
                },
            ))
            .child(timezone())
            .into_any_element(),
        ScheduleKind::Weekly => div()
            .flex()
            .flex_col()
            .gap_2()
            .child(weekday_picker("automation-weekday"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(clock(&rule.time, "执行时间"))
                    .child(timezone()),
            )
            .into_any_element(),
    };
    let remove = can_remove.then(|| {
        let remove_entity = entity.clone();
        compact_chip("automation-remove-schedule")
            .icon(crate::trash_icon())
            .icon_action("删除时间设置")
            .on_click(move |_, _, cx| {
                remove_entity.update(cx, |workspace, cx| {
                    workspace.remove_trigger_entry(index, cx);
                });
            })
    });
    let body = div()
        .flex()
        .flex_col()
        .gap_2()
        .when(frequency.is_some() || can_remove, |body| {
            body.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .children(frequency)
                    .children(remove),
            )
        })
        .child(schedule_control);
    if can_remove {
        body.p_3()
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(crate::ui_theme::border()))
            .into_any_element()
    } else {
        body.into_any_element()
    }
}

fn local_timezone_label() -> String {
    let seconds = Local::now().offset().local_minus_utc();
    let sign = if seconds >= 0 { '+' } else { '-' };
    let total_minutes = seconds.abs() / 60;
    format!(
        "本机时区 · UTC{sign}{:02}:{:02}",
        total_minutes / 60,
        total_minutes % 60
    )
}

fn format_unix_local(ts: i64) -> String {
    Utc.timestamp_opt(ts, 0)
        .single()
        .map(|utc| utc.with_timezone(&Local).format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|| ts.to_string())
}

fn format_relative_until(ts: i64, now: i64) -> String {
    let delta = ts - now;
    if delta <= 0 {
        return "即将运行".to_string();
    }
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    if delta < HOUR {
        let minutes = (delta / MINUTE).max(1);
        format!("{minutes} 分钟后")
    } else if delta < DAY {
        format!("{} 小时后", delta / HOUR)
    } else {
        format!("{} 天后", delta / DAY)
    }
}

fn format_next_run_cell(next_run_at: Option<i64>, enabled: bool, now: i64) -> String {
    if !enabled {
        return "—".to_string();
    }
    match next_run_at {
        Some(ts) => format_relative_until(ts, now),
        None => "—".to_string(),
    }
}

fn format_relative_since(ts: i64, now: i64) -> String {
    let delta = now - ts;
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    if delta < MINUTE {
        "刚刚".to_string()
    } else if delta < HOUR {
        format!("{} 分钟前", delta / MINUTE)
    } else if delta < DAY {
        format!("{} 小时前", delta / HOUR)
    } else {
        format!("{} 天前", delta / DAY)
    }
}

fn format_last_run_cell(
    last: Option<(smelt_model::automation::AutomationRunStatus, i64)>,
    now: i64,
) -> String {
    match last {
        Some((status, at)) => format!(
            "{} · {}",
            run_status_label(status),
            format_relative_since(at, now)
        ),
        None => "—".to_string(),
    }
}

/// 表单还差哪些才能保存。定时触发不带外部内容，所以只有它要求智能体指令；
/// webhook / 事件会把收到的内容当指令。
fn automation_missing_fields(
    name: &str,
    trigger_ready: bool,
    has_schedule: bool,
    action: &settings::AutomationAction,
    agent_exists: bool,
) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if name.trim().is_empty() {
        missing.push("名称");
    }
    if !trigger_ready {
        missing.push("触发器");
    }
    match action {
        settings::AutomationAction::Agent { prompt, .. } => {
            if !agent_exists {
                missing.push("智能体");
            }
            if has_schedule
                && prompt
                    .as_deref()
                    .is_none_or(|prompt| prompt.trim().is_empty())
            {
                missing.push("指令");
            }
        }
        settings::AutomationAction::Shell { command, .. } => {
            if command.trim().is_empty() {
                missing.push("命令");
            }
        }
    }
    missing
}

/// 页头上的保存状态。新建还没落盘，只说还差什么；已有的会在补齐后自动保存。
fn automation_save_hint(missing: &[&str], is_new: bool) -> Option<String> {
    if missing.is_empty() {
        return None;
    }
    let list = missing.join("、");
    Some(if is_new {
        format!("还差：{list}")
    } else {
        format!("还差：{list}，补齐后自动保存")
    })
}

fn run_source_label(source: smelt_model::automation::AutomationRunSource) -> &'static str {
    source.label()
}

fn run_status_label(status: smelt_model::automation::AutomationRunStatus) -> &'static str {
    use smelt_model::automation::AutomationRunStatus;
    match status {
        AutomationRunStatus::Starting => "启动中",
        AutomationRunStatus::Queued => "等待投递",
        AutomationRunStatus::Dispatching => "正在投递",
        AutomationRunStatus::Running => "运行中",
        AutomationRunStatus::AwaitingApproval => "等待审批",
        AutomationRunStatus::WaitingForUser => "等待输入",
        AutomationRunStatus::Completed => "已完成",
        AutomationRunStatus::Failed => "失败",
        AutomationRunStatus::Cancelled => "已取消",
        AutomationRunStatus::Skipped => "已跳过",
    }
}

fn agent_display_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        "未命名".to_string()
    } else {
        name.to_string()
    }
}

/// 卡片正文大约两行，窄窗口也不会把省略号裁掉。
fn agent_card_prompt(prompt: &str) -> Option<String> {
    let compact = excerpt_text(prompt, 64);
    if compact.is_empty() {
        None
    } else {
        Some(compact)
    }
}

fn agent_instructions_footer(prompt_chars: usize) -> String {
    if prompt_chars == 0 {
        "会作为系统提示词带进每次对话和自动化。".to_string()
    } else {
        format!("{prompt_chars} 字 · 会作为系统提示词带进每次对话和自动化")
    }
}

fn agent_conversation_start_error(agent: &settings::AgentDefinition) -> Option<&'static str> {
    if agent.engine_kind().is_none() {
        Some("请选择一个可用的执行引擎")
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutomationRowStatus {
    Unavailable,
    AgentMissing,
    Running,
    Active,
    Paused,
}

fn automation_row_status(
    enabled: bool,
    store_error: bool,
    agent_missing: bool,
    running: bool,
) -> AutomationRowStatus {
    if store_error {
        AutomationRowStatus::Unavailable
    } else if agent_missing {
        AutomationRowStatus::AgentMissing
    } else if running {
        AutomationRowStatus::Running
    } else if enabled {
        AutomationRowStatus::Active
    } else {
        AutomationRowStatus::Paused
    }
}

fn automation_row_status_label(status: AutomationRowStatus) -> &'static str {
    match status {
        AutomationRowStatus::Unavailable => "不可用",
        AutomationRowStatus::AgentMissing => "智能体已删除",
        AutomationRowStatus::Running => "运行中",
        AutomationRowStatus::Active => "已启用",
        AutomationRowStatus::Paused => "已暂停",
    }
}

/// 「启用」不用 Play：同一个菜单里「运行一次」已经是 Play。
fn automation_enabled_toggle_item(enabled: bool) -> PopupMenuItem {
    if enabled {
        PopupMenuItem::new("暂停").icon(IconName::Pause)
    } else {
        PopupMenuItem::new("启用").icon(IconName::CircleCheck)
    }
}

fn automation_nav_needs_attention(config: &settings::AgentHostState) -> bool {
    config.automations.iter().any(|automation| {
        automation.enabled
            && config
                .latest_automation_run(&automation.id)
                .is_some_and(|run| {
                    run.status == smelt_model::automation::AutomationRunStatus::Failed
                })
    })
}

fn agent_usage_summary(conversations: usize, automations: usize) -> String {
    match (conversations, automations) {
        (0, 0) => "还没有对话".to_string(),
        (count, 0) => format!("{count} 段对话"),
        (0, count) => format!("{count} 条自动化"),
        (conversations, automations) => format!("{conversations} 段对话 · {automations} 条自动化"),
    }
}

fn agent_definition_column() -> Div {
    div().w_full().max_w(px(720.)).mx_auto().px_8()
}

/// 内容卡：12px 圆角加发丝描边。智能体空状态、自动化表和表单共用。
fn content_card_style<E: Styled>(element: E) -> E {
    element
        .rounded(crate::ui_theme::card_radius())
        .border_1()
        .border_color(crate::ui_theme::card_stroke())
        .bg(rgb(crate::ui_theme::bg_card()))
}

fn content_card() -> Div {
    content_card_style(div())
}

fn agent_surface_card() -> Div {
    content_card()
}

fn render_agent_error(error: String) -> AnyElement {
    gpui_component::alert::Alert::error("agent-error", error).into_any_element()
}

fn render_engine_mark(kind: Option<settings::ConversationAgentKind>, size: f32) -> Icon {
    match kind {
        Some(kind) => Icon::empty()
            .path(kind.icon_asset())
            .size(px(size))
            .text_color(rgb(crate::ui_theme::text_mid())),
        None => Icon::new(IconName::Bot)
            .size(px(size))
            .text_color(rgb(crate::ui_theme::text_mid())),
    }
}

fn render_agent_engine_control(
    agent_id: String,
    label: String,
    kind: Option<settings::ConversationAgentKind>,
    entity: Entity<Workspace>,
) -> AnyElement {
    let icon = render_engine_mark(kind, 12.);
    if agent_engine_kinds().len() > 1 {
        Button::new("agent-engine")
            .quiet()
            .compact()
            .icon(icon)
            .label(label)
            .tooltip("执行这个智能体的运行时。后续 skill 和插件会挂在这个引擎上。")
            .dropdown_menu(move |mut menu, _, _| {
                for kind in agent_engine_kinds() {
                    let entity = entity.clone();
                    let id = agent_id.clone();
                    menu = menu.item(PopupMenuItem::new(kind.label()).on_click(move |_, _, cx| {
                        let id = id.clone();
                        entity.update(cx, |workspace, cx| workspace.set_agent_engine(id, kind, cx));
                    }));
                }
                menu
            })
            .into_any_element()
    } else {
        render_engine_chip(label, kind)
    }
}

fn render_agent_model_control(
    agent: AgentRef,
    model_provider: String,
    model_id: String,
    entity: Entity<Workspace>,
) -> AnyElement {
    let inherit = model_provider.trim().is_empty() || model_id.trim().is_empty();
    let label = if inherit {
        "跟随 Pi 默认".to_string()
    } else {
        format!("{model_provider} / {model_id}")
    };
    Button::new("agent-model")
        .quiet()
        .compact()
        .label(label)
        .tooltip("这个智能体的新对话和自动化用这套模型。对话里仍可再改。")
        .dropdown_menu(move |mut menu, window, cx| {
            let inherit_entity = entity.clone();
            let inherit_id = agent.id.clone();
            menu = menu.item(
                PopupMenuItem::new("跟随 Pi 默认")
                    .checked(inherit)
                    .on_click(move |_, _, cx| {
                        let id = inherit_id.clone();
                        inherit_entity.update(cx, |workspace, cx| {
                            workspace.set_agent_model(id, String::new(), String::new(), cx);
                        });
                    }),
            );
            // 模型在智能体那台机器上跑，可选的也是那台机器的。
            let groups = crate::settings::pi_model_choice_groups(&agent.runtime, cx);
            if !groups.is_empty() {
                menu = menu.separator();
            }
            for group in groups {
                let item_entity = entity.clone();
                let item_id = agent.id.clone();
                let current_provider = model_provider.clone();
                let current_model = model_id.clone();
                let models = group.models;
                menu = menu.submenu(
                    group.provider_name.clone(),
                    window,
                    cx,
                    move |mut sub, _, _| {
                        for choice in &models {
                            let checked =
                                choice.provider == current_provider && choice.id == current_model;
                            let item_entity = item_entity.clone();
                            let item_id = item_id.clone();
                            let provider = choice.provider.clone();
                            let model = choice.id.clone();
                            sub = sub.item(
                                PopupMenuItem::new(choice.short_label().to_string())
                                    .checked(checked)
                                    .on_click(move |_, _, cx| {
                                        let id = item_id.clone();
                                        let provider = provider.clone();
                                        let model = model.clone();
                                        item_entity.update(cx, |workspace, cx| {
                                            workspace.set_agent_model(id, provider, model, cx);
                                        });
                                    }),
                            );
                        }
                        // 这一层没有再嵌套。模型一多就超出窗口，在二级里滚。
                        sub.scrollable(true)
                    },
                );
            }
            menu
        })
        .into_any_element()
}

fn render_engine_chip(label: String, kind: Option<settings::ConversationAgentKind>) -> AnyElement {
    Tag::secondary()
        .rounded_full()
        .flex_none()
        .gap_1()
        .child(render_engine_mark(kind, 12.))
        .child(label)
        .into_any_element()
}

/// 页头前缘的返回。可见的只有符号，悬停提示是控件名。它只退一层，侧栏标题负责回根页。
fn product_back_button(
    id: &'static str,
    tooltip: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    Button::new(id)
        .quiet()
        .icon(IconName::ChevronLeft)
        .icon_action(tooltip)
        .on_click(on_click)
}

/// 目录和编辑器的页头。这些面不画标题胶囊。
///
/// 最上 `TOP_BAR_HEIGHT` 是空的拖窗口条，和侧栏列表、交通灯对齐。分段、标题和按钮放在这条下面，
/// 不再塞进 34px 里贴着窗口顶。左侧让开交通灯，右侧让开窗口开关，底边是发丝。按钮自己拦住点击。
pub(crate) fn product_page_chrome(left_guard: Pixels) -> Div {
    crate::workspace_frame::with_window_drag(
        div()
            .w_full()
            .flex_shrink_0()
            .pt(crate::workspace_frame::TOP_BAR_HEIGHT)
            .pb(px(8.))
            .pl(left_guard + px(20.))
            .pr(px(crate::stage::CHROME_TOGGLE_RESERVE))
            .border_b_1()
            .border_color(crate::ui_theme::hairline())
            .flex()
            .items_center(),
    )
}

/// 目录页头：和自动化顶栏同一条高度。左边页名加个数，不写副标题；右边是同尺寸的新建按钮。
fn product_page_header(
    title: &'static str,
    count: usize,
    count_unit: &'static str,
    action: AnyElement,
    left_guard: Pixels,
) -> AnyElement {
    product_page_chrome(left_guard)
        .justify_between()
        .gap_3()
        .child(
            div()
                .h(crate::ui_theme::height::sm())
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_medium()
                        .text_color(rgb(crate::ui_theme::text_bright()))
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(crate::ui_theme::text_muted()))
                        .child(format!("{count} {count_unit}")),
                ),
        )
        .child(action)
        .into_any_element()
}

/// 智能体的插件勾选区。只列全局插件（`~/.pi/agent/skills`、`~/.agents/skills`、
/// `~/.pi/agent/extensions`），因为智能体对话的工作目录是一次性空目录，项目级插件永远扫不到。
fn render_agent_plugin_card(
    agent: &settings::AgentDefinition,
    editor: &AgentEditor,
    entity: Entity<Workspace>,
    cx: &App,
) -> AnyElement {
    let selected = agent.plugins.clone();
    let selected_count = selected.len();

    let refresh_entity = entity.clone();
    let refresh_button = compact_chip("agent-plugins-refresh")
        .icon(gpui_component::Icon::new(gpui_kit_assets::IconName::RefreshCw))
        .icon_action("重新扫描插件目录")
        .on_click(move |_, _, cx| {
            refresh_entity.update(cx, |workspace, cx| workspace.refresh_agent_plugins(cx));
        });
    let manage_entity = entity.clone();
    let manage_button = compact_chip("agent-plugins-manage")
        .label("管理")
        .tooltip("去能力库导入或删除插件")
        .on_click(move |_, window, cx| {
            manage_entity.update(cx, |workspace, cx| {
                workspace.open_capabilities(CapabilitiesTab::Plugins, window, cx);
            });
        });

    // 已勾选但磁盘上已经没有的条目也要露出来，否则用户没法取消它。
    let mut rows: Vec<AnyElement> = editor
        .plugins
        .iter()
        .map(|plugin| {
            agent_plugin_row(
                AgentPick::Plugin,
                &agent.id,
                &plugin.id,
                &plugin.name,
                plugin.kind.label(),
                &plugin.description,
                &plugin.origin,
                plugin.broken.clone(),
                selected.contains(&plugin.id),
                entity.clone(),
                cx,
            )
        })
        .collect();
    for id in &selected {
        if editor.plugins.iter().any(|plugin| plugin.id == *id) {
            continue;
        }
        let (kind_label, name) = match smelt_model::pi_plugin_catalog::split_plugin_id(id) {
            Some((kind, name)) => (kind.label(), name.to_string()),
            None => ("未知", id.clone()),
        };
        rows.push(agent_plugin_row(
            AgentPick::Plugin,
            &agent.id,
            id,
            &name,
            kind_label,
            "",
            "",
            Some("磁盘上已经找不到了".to_string()),
            true,
            entity.clone(),
            cx,
        ));
    }

    let body = if rows.is_empty() {
        div()
            .px_5()
            .py_4()
            .text_xs()
            .text_color(rgb(crate::ui_theme::text_faint()))
            .child("还没有装任何插件。去「设置 → 智能体插件」导入技能或扩展。")
            .into_any_element()
    } else {
        div()
            .px_2()
            .pb_2()
            .flex()
            .flex_col()
            .children(rows)
            .into_any_element()
    };

    agent_surface_card()
        .flex()
        .flex_col()
        .child(
            div()
                .px_5()
                .pt_4()
                .pb_2()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_medium()
                                .text_color(rgb(crate::ui_theme::text_bright()))
                                .child("技能"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(crate::ui_theme::text_muted()))
                                .child(format!(
                                    "已勾选 {selected_count} 个。对话只加载勾选的技能和扩展，没勾的一律不加载。改动对新开的对话生效。"
                                )),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(refresh_button)
                        .child(manage_button),
                ),
        )
        .child(body)
        .into_any_element()
}

/// 「绑定的上下文」卡片。Pi 只接受单个 cwd，会话的工作目录是智能体自己的
/// space，所以业务素材只能靠这里挂进来——绑定的目录与链接会以绝对路径写进
/// system prompt，让 Pi 用 read/bash 自己去取。
fn render_agent_context_card(
    agent: &settings::AgentDefinition,
    editor: &AgentEditor,
    entity: Entity<Workspace>,
) -> AnyElement {
    // 工作目录按那台机器的主目录算，本机算不出远程机器上的路径。
    let space_root = match editor.agent.runtime {
        crate::RuntimeKey::Local => smelt_model::agent_paths::agent_space_root(&agent.id)
            .map(|path| format!("工作目录 {}", path.to_string_lossy()))
            .unwrap_or_else(|| "工作目录".to_string()),
        crate::RuntimeKey::Remote(_) => "工作目录在那台机器上".to_string(),
    };

    let pick_agent = agent.id.clone();
    let pick_entity = entity.clone();
    let add_folder = compact_chip("agent-context-add-folder")
        .icon(IconName::Plus)
        .label("目录")
        .tooltip("绑定一个本地目录")
        .on_click(move |_, _, cx| {
            let agent_id = pick_agent.clone();
            pick_entity.update(cx, |workspace, cx| {
                workspace.pick_agent_context_folders(agent_id, cx)
            });
        });

    let link_agent = agent.id.clone();
    let link_entity = entity.clone();
    let add_link = compact_chip("agent-context-add-link")
        .icon(IconName::Plus)
        .label("链接")
        .on_click(move |_, window, cx| {
            let agent_id = link_agent.clone();
            link_entity.update(cx, |workspace, cx| {
                workspace.add_agent_context_link(agent_id, window, cx)
            });
        });

    let mut rows: Vec<AnyElement> = Vec::new();
    for folder in &agent.context_folders {
        rows.push(agent_context_row(
            &agent.id,
            folder,
            "目录",
            true,
            entity.clone(),
        ));
    }
    for link in &agent.context_links {
        rows.push(agent_context_row(
            &agent.id,
            link,
            "链接",
            false,
            entity.clone(),
        ));
    }

    let body = if rows.is_empty() {
        div()
            .px_5()
            .py_4()
            .text_xs()
            .text_color(rgb(crate::ui_theme::text_faint()))
            .child("还没绑定任何上下文。绑定后智能体就知道去哪儿找素材。")
            .into_any_element()
    } else {
        div()
            .px_2()
            .pb_1()
            .flex()
            .flex_col()
            .children(rows)
            .into_any_element()
    };

    agent_surface_card()
        .flex()
        .flex_col()
        .child(
            div()
                .px_5()
                .pt_4()
                .pb_2()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_medium()
                                .text_color(rgb(crate::ui_theme::text_bright()))
                                .child("绑定的上下文"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(crate::ui_theme::text_muted()))
                                .child("目录和链接会写进 system prompt。改动对新开的对话生效。"),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(add_folder)
                        .child(add_link),
                ),
        )
        .child(body)
        .child(
            div()
                .px_3()
                .pb_2()
                .child(framed_text_input(&editor.context_link, "上下文链接").w_full()),
        )
        .child(
            div()
                .px_5()
                .pb_3()
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child(format!(
                    "{space_root}，同一智能体的所有对话共用它。工作目录和绑定目录下的 {} 会自动作为技能加载。",
                    smelt_model::pi_plugin_catalog::WORKSPACE_SKILL_DIRS.join(" / ")
                )),
        )
        .into_any_element()
}

fn agent_memory_switch(
    id: &'static str,
    agent: &settings::AgentDefinition,
    entity: Entity<Workspace>,
) -> Switch {
    let memory_enabled = agent.memory_enabled;
    let agent_id = agent.id.clone();
    Switch::new(id)
        .checked(memory_enabled)
        .accessibility_label("记忆")
        .tooltip(if memory_enabled {
            "关闭后不再注入和写入记忆，已有记忆保留"
        } else {
            "打开后在对话和自动化里使用记忆"
        })
        .on_click(move |checked, _, cx| {
            let agent_id = agent_id.clone();
            let enabled = *checked;
            entity.update(cx, |workspace, cx| {
                workspace.set_agent_memory_enabled(agent_id, enabled, cx)
            });
        })
}

fn memory_pending_count(memories: &AgentMemories) -> usize {
    memories
        .agent
        .entries
        .iter()
        .chain(&memories.user.entries)
        .filter(|entry| entry.pending_reason.is_some())
        .count()
}

/// 编辑页页签名。待确认要用户处理，挂在页签上，在配置页也看得到。
pub(super) fn agent_editor_tab_label(tab: AgentEditorTab, editor: &AgentEditor) -> String {
    match tab {
        AgentEditorTab::Config => "配置".to_string(),
        AgentEditorTab::Memory => match memory_pending_count(&editor.memories) {
            0 => "记忆".to_string(),
            pending => format!("记忆 · {pending} 待确认"),
        },
    }
}

/// 配置页上的记忆只占一行：开关、条数、待确认、进记忆页签的入口。条目一律不在这里列，
/// 记忆再多也不会把插件和 MCP 挤到后面。
fn render_agent_memory_summary(
    agent: &settings::AgentDefinition,
    editor: &AgentEditor,
    entity: Entity<Workspace>,
) -> AnyElement {
    let pending = memory_pending_count(&editor.memories);
    let manage_entity = entity.clone();
    let manage = compact_chip("agent-memory-manage")
        .label("管理")
        .icon(IconName::ChevronRight)
        .tooltip("查看、搜索和删除记忆")
        .on_click(move |_, _, cx| {
            manage_entity.update(cx, |workspace, cx| {
                workspace.select_agent_editor_tab(AgentEditorTab::Memory, cx)
            });
        });
    agent_surface_card()
        .px_5()
        .py_3()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_shrink_0()
                .text_sm()
                .font_medium()
                .text_color(rgb(crate::ui_theme::text_bright()))
                .child("记忆"),
        )
        .child(agent_memory_switch(
            "agent-memory-enabled-summary",
            agent,
            entity,
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(crate::ui_theme::text_muted()))
                        .child(if agent.memory_enabled {
                            memory_summary_text(
                                editor.memories.agent.entries.len(),
                                editor.memories.user.entries.len(),
                            )
                        } else {
                            "已关闭：不注入、不写入，已有记忆保留".to_string()
                        }),
                )
                .when(pending > 0, |row| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .text_color(rgb(crate::ui_theme::red()))
                            .child(format!("{pending} 条待确认")),
                    )
                }),
        )
        .child(manage)
        .into_any_element()
}

/// 记忆页签：智能体在对话和自动化里自己记下的长期信息。正文是 Markdown 文件，
/// 这里搜索、筛选、展开看正文、打开和删除；写入规则见 `docs/agent-memory.md`。
fn render_agent_memory_card(
    agent: &settings::AgentDefinition,
    editor: &AgentEditor,
    entity: Entity<Workspace>,
    cx: &App,
) -> AnyElement {
    let memory_enabled = agent.memory_enabled;
    let refresh_entity = entity.clone();
    let refresh_button = compact_chip("agent-memory-refresh")
        .icon(gpui_component::Icon::new(gpui_kit_assets::IconName::RefreshCw))
        .icon_action("重新读取记忆")
        .on_click(move |_, _, cx| {
            refresh_entity.update(cx, |workspace, cx| workspace.refresh_agent_memories(cx));
        });
    let query = editor.memory_query.read(cx).value().to_string();
    let filter = editor.memory_filter;
    let narrowed = !query.trim().is_empty() || filter != MemoryFilter::All;
    let filter_entity = entity.clone();
    let filters = choice_tabs(
        "agent-memory-filters",
        MemoryFilter::ALL
            .iter()
            .position(|option| *option == filter),
        MemoryFilter::ALL.into_iter().map(|option| {
            Tab::new().label(option.label()).tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(option.tooltip()).build(window, cx)
            })
        }),
        move |ix, cx| {
            if let Some(option) = MemoryFilter::ALL.get(ix).copied() {
                filter_entity.update(cx, |workspace, cx| {
                    workspace.set_agent_memory_filter(option, cx)
                });
            }
        },
    );
    agent_surface_card()
        .flex()
        .flex_col()
        .child(
            div()
                .px_5()
                .pt_4()
                .pb_2()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_medium()
                                .text_color(rgb(crate::ui_theme::text_bright()))
                                .child("记忆"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(crate::ui_theme::text_muted()))
                                .child(if memory_enabled {
                                    "智能体在对话和自动化里记下的长期信息。改动从下一轮对话起生效。"
                                } else {
                                    "记忆已关闭：不注入、不写入。下面的记忆保留着，重新打开后照常使用。"
                                }),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(refresh_button)
                        .child(agent_memory_switch("agent-memory-enabled", agent, entity.clone())),
                ),
        )
        .child(
            div()
                .px_5()
                .pb_2()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(200.))
                        .child(Input::new(&editor.memory_query).small().cleanable(true)),
                )
                .child(filters),
        )
        .child(agent_memory_section(
            &agent.id,
            AgentMemorySection {
                layer: MemoryLayer::Agent,
                title: "这个智能体",
                empty_hint: "还没有记忆。用户纠正它或让它记住什么时，它会自己写下来。",
                listing: &editor.memories.agent,
                query: &query,
                filter,
                narrowed,
            },
            editor.expanded_memory.as_ref(),
            entity.clone(),
        ))
        .child(agent_memory_section(
            &agent.id,
            AgentMemorySection {
                layer: MemoryLayer::User,
                title: "全局（所有智能体共享）",
                empty_hint: "还没有全局记忆。这里只放你本人的情况和你纠正过的做法。",
                listing: &editor.memories.user,
                query: &query,
                filter,
                narrowed,
            },
            editor.expanded_memory.as_ref(),
            entity,
        ))
        .into_any_element()
}

struct AgentMemorySection<'a> {
    layer: MemoryLayer,
    title: &'static str,
    empty_hint: &'static str,
    listing: &'a smelt_model::agent_memory::MemoryListing,
    query: &'a str,
    filter: MemoryFilter,
    /// 有搜索词或筛选时，空列表要说「没有匹配」，不能说「还没有记忆」。
    narrowed: bool,
}

fn agent_memory_section(
    agent_id: &str,
    section: AgentMemorySection<'_>,
    expanded: Option<&(MemoryLayer, String)>,
    entity: Entity<Workspace>,
) -> AnyElement {
    let AgentMemorySection {
        layer,
        title,
        empty_hint,
        listing,
        query,
        filter,
        narrowed,
    } = section;
    let layer_key = match layer {
        MemoryLayer::Agent => "agent",
        MemoryLayer::User => "user",
    };
    let folder_agent = agent_id.to_string();
    let folder_entity = entity.clone();
    let open_folder = compact_chip(format!("agent-memory-folder-{layer_key}"))
        .icon_size()
        .icon(IconName::FolderOpen)
        .icon_action("打开记忆文件夹")
        .on_click(move |_, _, cx| {
            let agent_id = folder_agent.clone();
            folder_entity.update(cx, |workspace, cx| {
                workspace.open_agent_memory_folder(agent_id, layer, cx)
            });
        });

    let rows: Vec<AnyElement> = visible_memories(&listing.entries, query, filter)
        .into_iter()
        .map(|entry| {
            let open = expanded.is_some_and(|(expanded_layer, file_name)| {
                *expanded_layer == layer && *file_name == entry.file_name
            });
            agent_memory_row(agent_id, layer, layer_key, entry, open, entity.clone())
        })
        .collect();
    let shown = rows.len();
    let body = if rows.is_empty() {
        div()
            .px_3()
            .py_1p5()
            .text_xs()
            .text_color(rgb(crate::ui_theme::text_faint()))
            .child(if narrowed && !listing.entries.is_empty() {
                "没有匹配的记忆"
            } else {
                empty_hint
            })
            .into_any_element()
    } else {
        div().flex().flex_col().children(rows).into_any_element()
    };

    div()
        .px_2()
        .pb_3()
        .flex()
        .flex_col()
        .child(
            div()
                .px_3()
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .font_medium()
                        .text_color(rgb(crate::ui_theme::text_muted()))
                        .child(memory_section_title(title, listing, narrowed.then_some(shown))),
                )
                .child(open_folder),
        )
        .when(listing.core_over_budget, |section| {
            section.child(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(crate::ui_theme::red()))
                    .child(format!(
                        "规则类记忆正文超过 {}KB 常驻上限，启动时只会注入一部分。请合并或删掉过时的规则。",
                        smelt_model::agent_memory::CORE_MEMORY_MAX_BYTES / 1000,
                    )),
            )
        })
        .child(body)
        .into_any_element()
}

fn memory_section_title(
    title: &str,
    listing: &smelt_model::agent_memory::MemoryListing,
    shown: Option<usize>,
) -> String {
    let total = listing.entries.len();
    let count = match shown {
        Some(shown) => format!("{shown} / {total} 条"),
        None => format!("{total} 条"),
    };
    let pending = listing
        .entries
        .iter()
        .filter(|entry| entry.pending_reason.is_some())
        .count();
    if pending == 0 {
        format!("{title} · {count}")
    } else {
        format!("{title} · {count}，{pending} 条待确认")
    }
}

fn memory_kind_label(kind: Option<&str>) -> &'static str {
    match kind {
        Some("user") => "关于你",
        Some("feedback") => "做法",
        Some("project") => "事实",
        Some("reference") => "资源",
        _ => "格式有误",
    }
}

/// 行尾的状态：待确认和长期没用的标红，提示用户处理；用过的显示次数。
fn memory_usage_label(entry: &smelt_model::agent_memory::MemoryEntry) -> Option<(String, bool)> {
    if entry.pending_reason.is_some() {
        return Some(("待确认".to_string(), true));
    }
    if entry.stale {
        return Some(("长期未用".to_string(), true));
    }
    (entry.read_count > 0).then(|| (format!("用过 {} 次", entry.read_count), false))
}

fn agent_memory_row(
    agent_id: &str,
    layer: MemoryLayer,
    layer_key: &str,
    entry: &smelt_model::agent_memory::MemoryEntry,
    expanded: bool,
    entity: Entity<Workspace>,
) -> AnyElement {
    let open_path = entry.path.clone();
    let toggle_file = entry.file_name.clone();
    let toggle_entity = entity.clone();
    let open_entity = entity.clone();
    let remove_agent = agent_id.to_string();
    let remove_file = entry.file_name.clone();
    let remove_name = entry.name.clone();
    let subtitle = match &entry.pending_reason {
        // 待确认时把原因放在描述前面，用户采纳前能看到它为什么没生效。
        Some(reason) => format!("{reason}：{}", entry.description),
        None if entry.description.is_empty() => entry.file_name.clone(),
        None => entry.description.clone(),
    };
    let approve_agent = agent_id.to_string();
    let approve_file = entry.file_name.clone();
    let approve_entity = entity.clone();
    let body_id = format!("agent-memory-body-{layer_key}-{}", entry.file_name);
    let header = div()
        .px_3()
        .py_1p5()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_shrink_0()
                .w(px(44.))
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child(memory_kind_label(entry.kind.as_deref())),
        )
        .child(
            div()
                .id(SharedString::from(format!(
                    "agent-memory-expand-{layer_key}-{}",
                    entry.file_name
                )))
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .on_click(move |_, _, cx| {
                    let file_name = toggle_file.clone();
                    toggle_entity.update(cx, |workspace, cx| {
                        workspace.toggle_agent_memory_expanded(layer, file_name, cx)
                    });
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            Icon::new(if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .size(px(12.))
                            .flex_shrink_0()
                            .text_color(rgb(crate::ui_theme::text_faint())),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .text_xs()
                                .text_color(rgb(crate::ui_theme::text_bright()))
                                .truncate()
                                .child(entry.name.clone()),
                        ),
                )
                .child(
                    // 待确认时前面挂着原因，一行放不下就把描述挤没了；给两行再省略。
                    div()
                        .text_xs()
                        .text_color(rgb(crate::ui_theme::text_faint()))
                        .text_ellipsis()
                        .line_clamp(2)
                        .child(subtitle),
                ),
        )
        .when_some(memory_usage_label(entry), |row, (label, stale)| {
            row.child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(rgb(if stale {
                        crate::ui_theme::red()
                    } else {
                        crate::ui_theme::text_faint()
                    }))
                    .child(label),
            )
        })
        .child(
            compact_chip(format!("agent-memory-open-{layer_key}-{}", entry.file_name))
                .icon_size()
                .icon(IconName::ExternalLink)
                .icon_action("用文本编辑器打开")
                .on_click(move |_, _, cx| {
                    let path = open_path.clone();
                    open_entity.update(cx, |workspace, cx| {
                        workspace.open_agent_memory_file(path, cx)
                    });
                }),
        )
        .when(entry.pending_reason.is_some(), |row| {
            row.child(
                compact_chip(format!(
                    "agent-memory-approve-{layer_key}-{}",
                    entry.file_name
                ))
                .icon_size()
                .icon(IconName::Check)
                .accessibility_label("采纳")
                .tooltip("采纳：之后的对话会用到这条记忆")
                .on_click(move |_, _, cx| {
                    let agent_id = approve_agent.clone();
                    let file_name = approve_file.clone();
                    approve_entity.update(cx, |workspace, cx| {
                        workspace.approve_agent_memory(agent_id, layer, file_name, cx)
                    });
                }),
            )
        })
        .child(
            compact_chip(format!(
                "agent-memory-remove-{layer_key}-{}",
                entry.file_name
            ))
            .icon_size()
            .icon(IconName::Close)
            .icon_action("删除这条记忆")
            .on_click(move |_, _, cx| {
                let target = MemoryDeleteTarget {
                    agent_id: remove_agent.clone(),
                    layer,
                    file_name: remove_file.clone(),
                    name: remove_name.clone(),
                };
                entity.update(cx, |workspace, cx| {
                    workspace.request_delete_agent_memory(target, cx)
                });
            }),
        );
    div()
        .flex()
        .flex_col()
        .child(header)
        .when(expanded, |row| {
            row.child(agent_memory_body(body_id, &entry.body))
        })
        .into_any_element()
}

/// 展开的记忆正文：按原样分行显示，不渲染 Markdown，用户看到的就是模型读到的。
fn agent_memory_body(id: String, body: &str) -> AnyElement {
    let lines: Vec<AnyElement> = if body.is_empty() {
        vec![
            div()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child("（正文为空）")
                .into_any_element(),
        ]
    } else {
        body.lines()
            .map(|line| {
                // 空行也要占一行高度，不然段落会挤在一起。
                div()
                    .min_h(px(16.))
                    .child(line.to_string())
                    .into_any_element()
            })
            .collect()
    };
    div()
        .id(SharedString::from(id))
        .mx_3()
        .mb_2()
        .px_3()
        .py_2()
        .max_h(px(240.))
        .overflow_y_scroll()
        .rounded(px(6.))
        .bg(rgb(crate::ui_theme::bg_column()))
        .text_xs()
        .text_color(rgb(crate::ui_theme::text_muted()))
        .children(lines)
        .into_any_element()
}

fn agent_context_row(
    agent_id: &str,
    value: &str,
    kind_label: &str,
    is_folder: bool,
    entity: Entity<Workspace>,
) -> AnyElement {
    let remove_agent = agent_id.to_string();
    let remove_value = value.to_string();
    div()
        .px_3()
        .py_1p5()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_shrink_0()
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child(kind_label.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_bright()))
                .truncate()
                .child(value.to_string()),
        )
        .child(
            compact_chip(format!("agent-context-remove-{kind_label}-{value}"))
                .icon_size()
                .icon(crate::trash_icon())
                .icon_action(if is_folder {
                    "解除这个目录的绑定"
                } else {
                    "移除这个链接"
                })
                .on_click(move |_, _, cx| {
                    let agent_id = remove_agent.clone();
                    let value = remove_value.clone();
                    entity.update(cx, |workspace, cx| {
                        if is_folder {
                            workspace.remove_agent_context_folder(agent_id, value, cx);
                        } else {
                            workspace.remove_agent_context_link(agent_id, value, cx);
                        }
                    });
                }),
        )
        .into_any_element()
}

/// 智能体的 MCP 勾选。服务器在能力库里配置，这里只决定这个智能体连哪几台。
fn render_agent_mcp_card(
    agent: &settings::AgentDefinition,
    editor: &AgentEditor,
    entity: Entity<Workspace>,
    cx: &App,
) -> AnyElement {
    let selected = agent.mcp_servers.clone();
    let selected_count = selected.len();

    let refresh_entity = entity.clone();
    let refresh_button = compact_chip("agent-mcp-refresh")
        .icon(gpui_component::Icon::new(gpui_kit_assets::IconName::RefreshCw))
        .icon_action("重新读取 mcp.json")
        .on_click(move |_, _, cx| {
            refresh_entity.update(cx, |workspace, cx| workspace.refresh_agent_plugins(cx));
        });
    let manage_entity = entity.clone();
    let manage_button = compact_chip("agent-mcp-manage")
        .label("管理")
        .tooltip("去能力库添加、登录或停用 MCP 服务器")
        .on_click(move |_, window, cx| {
            manage_entity.update(cx, |workspace, cx| {
                workspace.open_capabilities(CapabilitiesTab::Mcp, window, cx);
            });
        });

    let mut rows: Vec<AnyElement> = editor
        .mcp_servers
        .iter()
        .map(|server| {
            let (kind_label, detail) = match &server.transport {
                smelt_model::pi_mcp::PiMcpTransport::Http { url } => ("远程", url.clone()),
                smelt_model::pi_mcp::PiMcpTransport::Stdio { command, args } => {
                    ("本地", format!("{command} {}", args.join(" ")))
                }
                smelt_model::pi_mcp::PiMcpTransport::Unknown => ("未知", String::new()),
            };
            // 能力库里停用的服务器勾了也不会连：Pi 读到 `enabled: false` 就跳过。
            let broken = (!server.enabled).then(|| "已在能力库停用，勾选也不会连接".to_string());
            agent_plugin_row(
                AgentPick::McpServer,
                &agent.id,
                &server.name,
                &server.name,
                kind_label,
                server.description.as_deref().unwrap_or(""),
                &detail,
                broken,
                selected.contains(&server.name),
                entity.clone(),
                cx,
            )
        })
        .collect();
    // 已勾选但 mcp.json 里已经删掉的也要露出来，否则用户没法取消它。
    for name in &selected {
        if editor.mcp_servers.iter().any(|server| server.name == *name) {
            continue;
        }
        rows.push(agent_plugin_row(
            AgentPick::McpServer,
            &agent.id,
            name,
            name,
            "未知",
            "",
            "",
            Some("mcp.json 里已经没有这台服务器".to_string()),
            true,
            entity.clone(),
            cx,
        ));
    }

    let body = if rows.is_empty() {
        div()
            .px_5()
            .py_4()
            .text_xs()
            .text_color(rgb(crate::ui_theme::text_faint()))
            .child("还没有配置 MCP 服务器。点「管理」去能力库添加。")
            .into_any_element()
    } else {
        div()
            .px_2()
            .pb_2()
            .flex()
            .flex_col()
            .children(rows)
            .into_any_element()
    };

    agent_surface_card()
        .flex()
        .flex_col()
        .child(
            div()
                .px_5()
                .pt_4()
                .pb_2()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_medium()
                                .text_color(rgb(crate::ui_theme::text_bright()))
                                .child("MCP"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(crate::ui_theme::text_muted()))
                                .child(format!(
                                    "已勾选 {selected_count} 个。对话只连勾选的服务器，没勾的不启动。改动对新开的对话生效。"
                                )),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(refresh_button)
                        .child(manage_button),
                ),
        )
        .child(body)
        .into_any_element()
}

/// 编辑器里可勾选的两类能力：插件（技能 / 扩展）和 MCP 服务器。行的样子一样，
/// 只是写回定义的字段不同。
#[derive(Clone, Copy)]
enum AgentPick {
    Plugin,
    McpServer,
}

#[allow(clippy::too_many_arguments)]
fn agent_plugin_row(
    pick: AgentPick,
    agent_id: &str,
    plugin_id: &str,
    name: &str,
    kind_label: &str,
    description: &str,
    origin: &str,
    broken: Option<String>,
    checked: bool,
    entity: Entity<Workspace>,
    cx: &App,
) -> AnyElement {
    let toggle_agent = agent_id.to_string();
    let toggle_plugin = plugin_id.to_string();
    let usable = broken.is_none();
    let subtitle = match &broken {
        Some(reason) => reason.clone(),
        None if !description.is_empty() => description.to_string(),
        None => origin.to_string(),
    };
    let subtitle_color = if usable {
        rgb(crate::ui_theme::text_faint())
    } else {
        cx.theme().danger.into()
    };

    div()
        .px_3()
        .py_2()
        .flex()
        .items_center()
        .gap_3()
        .child(
            Switch::new(match pick {
                AgentPick::Plugin => format!("agent-plugin-{plugin_id}"),
                AgentPick::McpServer => format!("agent-mcp-{plugin_id}"),
            })
            .checked(checked)
            .disabled(!usable && !checked)
            .accessibility_label(name.to_string())
            .on_click(move |_, _, cx| {
                let agent_id = toggle_agent.clone();
                let plugin_id = toggle_plugin.clone();
                entity.update(cx, |workspace, cx| match pick {
                    AgentPick::Plugin => workspace.toggle_agent_plugin(agent_id, plugin_id, cx),
                    AgentPick::McpServer => {
                        workspace.toggle_agent_mcp_server(agent_id, plugin_id, cx)
                    }
                });
            }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(crate::ui_theme::text_bright()))
                                .child(name.to_string()),
                        )
                        .child(
                            div()
                                .px_1p5()
                                .rounded(px(4.))
                                .bg(rgb(crate::ui_theme::bg_stage()))
                                .text_xs()
                                .text_color(rgb(crate::ui_theme::text_faint()))
                                .child(kind_label.to_string()),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .truncate()
                        .text_color(subtitle_color)
                        .child(subtitle),
                ),
        )
        .into_any_element()
}

/// 组件默认描边叠在卡片底上几乎看不见；聚焦时还会在外侧加 3px ring，把整块撑开。
/// 短输入共用这一层：固定 1px 边 + 锁死高度 + 裁掉 ring。智能体和自动化都走这里。
fn editor_field_frame() -> Div {
    div()
        .flex()
        .items_center()
        .flex_none()
        .h(crate::ui_theme::height::sm())
        .overflow_hidden()
        .border_1()
        .border_color(rgb(crate::ui_theme::border_loud()))
        .rounded(px(8.))
        .bg(rgb(crate::ui_theme::bg_column()))
}

fn framed_text_input(input: &Entity<InputState>, aria: &'static str) -> Div {
    editor_field_frame().child(
        Input::new(input)
            .small()
            .appearance(false)
            .focus_bordered(false)
            .focus_ring(false)
            .w_full()
            .aria_label(aria),
    )
}

fn framed_number_input(input: &Entity<InputState>) -> Div {
    editor_field_frame().child(
        NumberInput::new(input)
            .small()
            .appearance(false)
            .focus_ring(false)
            .w_full(),
    )
}

fn framed_textarea(textarea: Textarea) -> AnyElement {
    div()
        .border_1()
        .border_color(rgb(crate::ui_theme::border_loud()))
        .rounded(px(8.))
        .bg(rgb(crate::ui_theme::bg_column()))
        .child(textarea.appearance(false))
        .into_any_element()
}

fn framed_automation_textarea(textarea: Textarea) -> AnyElement {
    div()
        .w_full()
        .px_3()
        .py_3()
        .min_h(px(120.))
        .child(textarea.appearance(false))
        .into_any_element()
}

fn compact_chip(id: impl Into<ElementId>) -> Button {
    Button::new(id).quiet().compact()
}

fn automation_form_shell() -> Div {
    content_card().w_full()
}

/// 一次运行的三行字：标题、时间或来源，失败时再加一行红字。目录和编辑器共用。
/// 标题不自己定字色，跟着行走：行被选中时换成选中字色（见 `ListRow`）。
fn automation_run_lines(title: String, detail: String, error: Option<String>) -> Div {
    div()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_sm().font_medium().truncate().child(title))
        .child(
            div()
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_muted()))
                .truncate()
                .child(detail),
        )
        .when_some(error, |column, error| {
            column.child(
                div()
                    .text_xs()
                    .text_color(rgb(crate::ui_theme::red()))
                    .truncate()
                    .child(error),
            )
        })
}

fn automation_form_row() -> Div {
    automation_form_shell()
        .h(crate::ui_theme::height::xl())
        .px_3()
        .flex()
        .items_center()
}

fn automation_section_label(label: &'static str) -> AnyElement {
    div()
        .text_sm()
        .font_medium()
        .text_color(rgb(crate::ui_theme::text_bright()))
        .child(label)
        .into_any_element()
}

/// 几个互斥选项的分段开关，选中块在选项之间滑动。`on_select` 拿到被点的下标。
fn choice_tabs(
    id: impl Into<ElementId>,
    selected: Option<usize>,
    tabs: impl IntoIterator<Item = Tab>,
    on_select: impl Fn(usize, &mut App) + 'static,
) -> TabBar {
    TabBar::new(id)
        .segmented()
        .flex_none()
        .when_some(selected, |bar, ix| bar.selected_index(ix))
        .on_click(move |ix, _, cx| on_select(*ix, cx))
        .children(tabs)
}

mod automation_catalog;
mod automation_run_detail;
mod automation_templates;
mod automation_view;
mod view;
mod workspace;

#[cfg(test)]
mod tests;
