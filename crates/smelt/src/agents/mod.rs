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
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState, NumberInput, Textarea, TextareaState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::switch::Switch;
use gpui_component::{
    ActiveTheme as _, Disableable as _, FocusableExt as _, Icon, IconName, Selectable as _,
    Sizable as _, StyledExt as _,
};

use crate::workspace_nav::AutomationsView;
use crate::workspace_sessions::NewAcpSessionRequest;
use crate::{AgentStatus, Workspace, WorkspaceRoute, acp_view, settings};
use smelt_core::pi_plugin_catalog::{PiPlugin, discover_plugins};

/// 智能体面的瞬时 UI。导航页在 [`crate::WorkspaceNav`]；这里只放选中项和编辑器。
pub(crate) struct AgentsSurface {
    /// 落盘：工作台最后选中的定义。
    pub(crate) selected_id: Option<String>,
    pub(crate) editor: Option<AgentEditor>,
    pub(crate) error: Option<String>,
}

impl AgentsSurface {
    pub(crate) fn from_persisted(selected_id: Option<String>) -> Self {
        Self {
            selected_id,
            editor: None,
            error: None,
        }
    }
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
    /// 从目录打开 History / Run 时，返回应回到目录而不是编辑器。
    pub(crate) return_to_catalog: bool,
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
            return_to_catalog: false,
        }
    }
}

pub(crate) struct AgentEditor {
    id: String,
    name: Entity<InputState>,
    /// 名字平时当标题看，点一下才变成输入框，避免一进编辑器就闪光标。
    name_editing: bool,
    instructions: Entity<TextareaState>,
    /// 绑定参考链接用的输入框。目录走原生选择框，链接只能手输。
    context_link: Entity<InputState>,
    /// 打开编辑器时扫一次磁盘的插件目录；不在渲染里扫，避免每次输入都读文件。
    plugins: Vec<PiPlugin>,
    _subscriptions: Vec<Subscription>,
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
        let days = days & smelt_core::automation::SCHEDULE_ALL_DAYS_MASK;
        if days == 0 || days == smelt_core::automation::SCHEDULE_ALL_DAYS_MASK {
            Self::Everyday
        } else if days == smelt_core::automation::SCHEDULE_WEEKDAYS_MASK {
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
    feishu_chat: Entity<InputState>,
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

fn ensure_agent_conversation_cwd(agent_definition_id: &str) -> Option<String> {
    let dir = smelt_core::agent_definition_store::ensure_agent_space(agent_definition_id)?;
    Some(dir.to_string_lossy().into_owned())
}

fn ensure_workbench_conversation_cwd() -> Option<String> {
    let dir = smelt_core::agent_definition_store::ensure_workbench_conversation_workspace()?;
    Some(dir.to_string_lossy().into_owned())
}

/// 对话行图标色。只跟状态走：选中已经由行底色表达。
///
/// 空闲选中如果再走 accent 蓝，就会和运行中撞色——选中那条永远看不出在不在跑。
fn conversation_icon_color(status: AgentStatus, phase: Option<f32>) -> gpui::Rgba {
    Workspace::session_icon_color(status, phase)
}

/// 对话行的状态图标。语义与项目会话行同一套：三态上色，Running 进入时短暂
/// 提亮一次，随后回落静态蓝——不让一个长任务永久持有动画 timer。
///
/// 选中态不再单独染色：状态色本身就是这一行要传达的信息，被选中压过去就等于
/// 「选中的那条永远看不出在不在跑」。选中与否已经由整行底色表达。
fn conversation_status_icon(
    ix: usize,
    status: AgentStatus,
    animate_running: bool,
    engine: Option<settings::ConversationAgentKind>,
) -> AnyElement {
    let color = move |phase: Option<f32>| conversation_icon_color(status, phase);
    let icon = move || match engine {
        Some(kind) => Icon::empty().path(kind.icon_asset()),
        None => Icon::new(IconName::Inbox),
    };
    if animate_running && status == AgentStatus::Running {
        return smelt_ui::motion::ambient_animation(
            ("product-conv-glow", ix),
            crate::session_list::SESSION_GLOW_PERIOD,
            true,
            move |phase| icon().size(px(17.)).text_color(color(Some(phase))),
        )
        .into_any_element();
    }
    icon()
        .size(px(17.))
        .text_color(color(None))
        .into_any_element()
}

const AGENT_CONV_HEADER_GROUP: &str = "agent-conv-header";
const AGENT_SESSION_INDENT: f32 = 22.;
const AGENT_GUIDE_LEFT: f32 = 18.;

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentConversationInput {
    session_ix: usize,
    definition_id: Option<String>,
    cwd: Option<String>,
}

impl AgentConversationInput {
    fn new(session_ix: usize, definition_id: Option<&str>, cwd: Option<&str>) -> Self {
        Self {
            session_ix,
            definition_id: definition_id.map(str::to_string),
            cwd: cwd.map(str::to_string),
        }
    }

    fn resolved_definition_id(&self) -> Option<String> {
        self.definition_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .or_else(|| {
                self.cwd.as_deref().and_then(|cwd| {
                    smelt_core::agent_definition_store::agent_definition_id_for_space(
                        std::path::Path::new(cwd),
                    )
                })
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentConversationGroup {
    agent_id: String,
    agent_name: String,
    session_ixes: Vec<usize>,
}

/// 侧栏「对话」按智能体分组：定义顺序优先，找不到定义的对话收进「其他」。
fn group_agent_conversations(
    agents: &[(String, String)],
    conversations: &[AgentConversationInput],
) -> Vec<AgentConversationGroup> {
    let mut groups: Vec<AgentConversationGroup> = agents
        .iter()
        .map(|(id, name)| AgentConversationGroup {
            agent_id: id.clone(),
            agent_name: agent_display_name(name),
            session_ixes: Vec::new(),
        })
        .collect();
    let mut orphans = Vec::new();
    for conversation in conversations {
        match conversation.resolved_definition_id() {
            Some(id) => {
                if let Some(group) = groups.iter_mut().find(|group| group.agent_id == id) {
                    group.session_ixes.push(conversation.session_ix);
                } else {
                    orphans.push(conversation.session_ix);
                }
            }
            None => orphans.push(conversation.session_ix),
        }
    }
    groups.retain(|group| !group.session_ixes.is_empty());
    if !orphans.is_empty() {
        groups.push(AgentConversationGroup {
            agent_id: String::new(),
            agent_name: "其他".into(),
            session_ixes: orphans,
        });
    }
    groups
}

fn agent_conversation_group_status(
    session_ixes: &[usize],
    statuses: &[AgentStatus],
) -> AgentStatus {
    AgentStatus::highest(
        session_ixes
            .iter()
            .filter_map(|&session_ix| statuses.get(session_ix).copied()),
    )
}

/// 分组标题已经表达智能体身份；对话行只展示用户命名或对话自己的自动标题。
fn nested_agent_conversation_title(custom_title: Option<&str>, auto_title: Option<&str>) -> String {
    custom_title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .or_else(|| auto_title.map(str::trim).filter(|title| !title.is_empty()))
        .unwrap_or("新对话")
        .to_string()
}

fn compact_instructions(value: &str) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = value.chars();
    let mut compact = chars.by_ref().take(96).collect::<String>();
    if chars.next().is_some() {
        compact.push('…');
    }
    compact
}

/// 产品智能体的执行引擎注册点在 `smelt_core`：移动端网关也要照同一份名单产出
/// 新建对话选项，注册表留在 GUI crate 里两边就会各注册各的。
pub(crate) use smelt_core::agent_definition::agent_engine_kinds;

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
                days: smelt_core::automation::SCHEDULE_WEEKDAYS_MASK,
                hour,
                minute,
            }
        }
        ScheduleKind::Weekly => {
            let (hour, minute) =
                parse_clock(time).ok_or_else(|| "请输入 00:00 到 23:59 之间的时间".to_string())?;
            let days = weekday_mask & smelt_core::automation::SCHEDULE_ALL_DAYS_MASK;
            if days == 0 {
                return Err("请至少选择一天".to_string());
            }
            if days == smelt_core::automation::SCHEDULE_ALL_DAYS_MASK {
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
            let days = weekday_mask & smelt_core::automation::SCHEDULE_ALL_DAYS_MASK;
            let constrained = window.is_some()
                || (days != 0 && days != smelt_core::automation::SCHEDULE_ALL_DAYS_MASK);
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
                                smelt_core::automation::SCHEDULE_ALL_DAYS_MASK
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
                                smelt_core::automation::SCHEDULE_ALL_DAYS_MASK
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
                    smelt_core::automation::new_webhook_secret()
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

fn schedule_from_values_for_rule(
    rule: &ScheduleRuleEditor,
    cx: &App,
) -> Result<settings::AutomationSchedule, String> {
    let weekday_mask = if rule.kind == ScheduleKind::Interval {
        match rule.interval_day_scope {
            IntervalDayScope::Everyday => smelt_core::automation::SCHEDULE_ALL_DAYS_MASK,
            IntervalDayScope::Weekdays => smelt_core::automation::SCHEDULE_WEEKDAYS_MASK,
            IntervalDayScope::Weekly => {
                let days = rule.weekday_mask & smelt_core::automation::SCHEDULE_ALL_DAYS_MASK;
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
    let mut weekday_mask = smelt_core::automation::SCHEDULE_ALL_DAYS_MASK;
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
            kind = if *days == smelt_core::automation::SCHEDULE_WEEKDAYS_MASK {
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
                smelt_core::automation::SCHEDULE_ALL_DAYS_MASK
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
        .unwrap_or_else(smelt_core::automation::new_webhook_secret)
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
            .unwrap_or_else(smelt_core::automation::new_webhook_secret),
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
        .h(px(44.))
        .rounded_full()
        .border_1()
        .border_dashed()
        .border_color(crate::ui_theme::card_stroke())
        .flex()
        .items_center()
        .child(
            Button::new("automation-add-trigger")
                .ghost()
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

fn trigger_preset_icon(preset: settings::AutomationTriggerPresetId) -> IconName {
    match preset {
        settings::AutomationTriggerPresetId::Webhook => IconName::Network,
        settings::AutomationTriggerPresetId::Hourly
        | settings::AutomationTriggerPresetId::Interval => IconName::Cpu,
        settings::AutomationTriggerPresetId::Daily
        | settings::AutomationTriggerPresetId::Weekly => IconName::Calendar,
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
    let icon = match entry.trigger_kind {
        settings::AutomationTriggerKindId::Webhook => IconName::Network,
        settings::AutomationTriggerKindId::Interval => IconName::Cpu,
        settings::AutomationTriggerKindId::Calendar => IconName::Calendar,
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
    automation_form_shell()
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
                    compact_chip(format!("automation-remove-trigger-{index}"))
                        .icon(IconName::Delete)
                        .tooltip("删除触发器")
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

fn automation_trigger_glyph(icon: IconName) -> AnyElement {
    div()
        .size(px(32.))
        .rounded_full()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(crate::ui_theme::tint(crate::ui_theme::blue(), 0x28))
        .child(
            Icon::new(icon)
                .size(px(16.))
                .text_color(rgb(crate::ui_theme::blue())),
        )
        .into_any_element()
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
    match entry.schedule.interval_unit {
        IntervalUnit::Hours => "每小时".to_string(),
        IntervalUnit::Minutes => {
            let value = entry.schedule.interval.read(cx).value();
            if value.trim().is_empty() {
                "间隔".to_string()
            } else {
                format!("每隔 {value} 分钟")
            }
        }
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
        settings::AutomationTriggerKindId::Interval => match entry.schedule.interval_unit {
            IntervalUnit::Hours => "按选定间隔每小时",
            IntervalUnit::Minutes => "按固定间隔执行，可限制星期和时段",
        },
    }
}

fn render_notification_field(
    editor: &AutomationEditor,
    entity: Entity<Workspace>,
    _cx: &App,
) -> AnyElement {
    let selected = editor.notification;
    let picker_entity = entity.clone();
    let picker = automation_form_row().child(
        Button::new("automation-notification")
            .ghost()
            .focus_ring(false)
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
    let mut field = div()
        .flex()
        .flex_col()
        .gap_2()
        .child(automation_section_label("通知"))
        .child(picker);
    if selected.includes_feishu() {
        field = field.child(
            automation_form_row().child(
                Input::new(&editor.feishu_chat)
                    .appearance(false)
                    .focus_bordered(false)
                    .focus_ring(false)
                    .w_full()
                    .aria_label("飞书会话 ID"),
            ),
        );
    }
    field.into_any_element()
}

fn notification_preset_icon(preset: settings::AutomationNotificationPreset) -> IconName {
    match preset {
        settings::AutomationNotificationPreset::Off => IconName::EyeOff,
        settings::AutomationNotificationPreset::App => IconName::Bell,
        settings::AutomationNotificationPreset::Feishu
        | settings::AutomationNotificationPreset::AppAndFeishu => IconName::Inbox,
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
        .unwrap_or_else(|| smelt_core::automation::DEFAULT_WEBHOOK_BASE_URL.to_string());
    let stored = cx
        .global::<settings::AgentHostState>()
        .automations
        .iter()
        .find(|automation| automation.id == editor.automation_id);
    let paused = stored.is_some_and(|automation| !automation.enabled);
    let live_url = (!editor.is_new && !entry.webhook_secret.is_empty())
        .then(|| smelt_core::automation::webhook_url(&base, &entry.webhook_secret));
    let copy_id = format!("automation-copy-webhook-{index}");
    let curl_id = format!("automation-copy-webhook-curl-{index}");
    let regen_id = format!("automation-regen-webhook-{index}");
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
                        compact_chip(copy_id.clone())
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
                        compact_chip(curl_id.clone())
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
                        compact_chip(regen_id.clone())
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
        "支持 {{payload}} 和请求字段变量（如 {{text}}）；留空时直接发送请求内容。长期指令请在智能体设置中配置。"
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
        segmented_track(cx).children(ScheduleKind::clock_kinds().into_iter().enumerate().map(
            |(kind_index, kind)| {
                let selected = kind == rule.kind;
                let frequency_entity = entity.clone();
                compact_chip(format!("agent-trigger-frequency-{index}-{kind_index}"))
                    .label(kind.label())
                    .selected(selected)
                    .toggled(selected)
                    .on_click(move |_, _, cx| {
                        frequency_entity.update(cx, |workspace, cx| {
                            workspace.set_trigger_schedule_kind(index, kind, cx);
                        });
                    })
            },
        ))
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
    let weekday_picker = |id_prefix: &str| {
        segmented_track(cx).children((0..7).map(|day_index| {
            let bit = 1_u8 << day_index;
            let selected = rule.weekday_mask & bit != 0;
            let day_entity = entity.clone();
            compact_chip(format!("{id_prefix}-{index}-{day_index}"))
                .w(px(32.))
                .label(smelt_core::automation::SCHEDULE_DAY_LABELS[day_index])
                .selected(selected)
                .toggled(selected)
                .on_click(move |_, _, cx| {
                    day_entity.update(cx, |workspace, cx| {
                        workspace.toggle_schedule_weekday(index, bit, cx);
                    });
                })
        }))
    };
    let interval_unit = rule.interval_unit;
    let schedule_control = match rule.kind {
        ScheduleKind::Interval => {
            let day_scope =
                segmented_track(cx).children(IntervalDayScope::all().into_iter().enumerate().map(
                    |(scope_index, scope)| {
                        let selected = scope == rule.interval_day_scope;
                        let scope_entity = entity.clone();
                        compact_chip(format!("automation-interval-days-{index}-{scope_index}"))
                            .label(scope.label())
                            .selected(selected)
                            .toggled(selected)
                            .on_click(move |_, _, cx| {
                                scope_entity.update(cx, |workspace, cx| {
                                    workspace.set_interval_day_scope(index, scope, cx);
                                });
                            })
                    },
                ));
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
                        .child(
                            segmented_track(cx).children(
                                [IntervalUnit::Minutes, IntervalUnit::Hours]
                                    .into_iter()
                                    .map(|unit| {
                                        let selected = unit == interval_unit;
                                        let unit_entity = entity.clone();
                                        compact_chip(format!(
                                            "automation-interval-unit-{index}-{}",
                                            unit.label()
                                        ))
                                        .label(unit.label())
                                        .selected(selected)
                                        .toggled(selected)
                                        .on_click(
                                            move |_, _, cx| {
                                                unit_entity.update(cx, |workspace, cx| {
                                                    workspace.set_interval_unit(index, unit, cx);
                                                });
                                            },
                                        )
                                    }),
                            ),
                        ),
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
                            compact_chip(format!("automation-interval-window-clear-{index}"))
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
                    compact_chip(format!("automation-interval-window-open-{index}"))
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
        compact_chip(format!("automation-remove-schedule-{index}"))
            .icon(IconName::Delete)
            .tooltip("删除时间设置")
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

fn run_source_label(source: smelt_core::automation::AutomationRunSource) -> &'static str {
    source.label()
}

fn run_status_label(status: smelt_core::automation::AutomationRunStatus) -> &'static str {
    use smelt_core::automation::AutomationRunStatus;
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

fn agent_prompt_summary(prompt: &str) -> Option<String> {
    let compact = compact_instructions(prompt);
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
        AutomationRowStatus::Active => "已激活",
        AutomationRowStatus::Paused => "已暂停",
    }
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

fn agent_surface_card() -> Div {
    div()
        .rounded(px(16.))
        .border_1()
        .border_color(rgb(crate::ui_theme::border_mid()))
        .bg(rgb(crate::ui_theme::bg_card()))
}

fn render_agent_error(error: String) -> AnyElement {
    div()
        .px_3()
        .py_2()
        .rounded(px(10.))
        .bg(rgb(crate::ui_theme::red()).opacity(0.12))
        .text_sm()
        .text_color(rgb(crate::ui_theme::red()))
        .child(error)
        .into_any_element()
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
            .ghost()
            .small()
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
    agent_id: String,
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
        .ghost()
        .small()
        .label(label)
        .tooltip("这个智能体的新对话和自动化用这套模型。对话里仍可再改。")
        .dropdown_menu(move |mut menu, window, cx| {
            let inherit_entity = entity.clone();
            let inherit_id = agent_id.clone();
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
            let groups = smelt_core::pi_model_settings::list_pi_model_choice_groups();
            if !groups.is_empty() {
                menu = menu.separator();
            }
            for group in groups {
                let item_entity = entity.clone();
                let item_id = agent_id.clone();
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
    div()
        .h(px(22.))
        .px_2()
        .rounded_full()
        .bg(rgb(crate::ui_theme::bg_hover()))
        .flex()
        .items_center()
        .gap_1()
        .child(render_engine_mark(kind, 12.))
        .child(
            div()
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_muted()))
                .child(label),
        )
        .into_any_element()
}

/// 智能体页头的配置入口：直接开那扇只有模型和插件两页的独立窗口。
///
/// 不套一层「模型 / 插件」下拉：窗口左侧本来就列着这两页，多一层菜单只是让用户
/// 多点一次，还得先猜自己要找的东西归在哪一页。
fn render_agent_settings_entry(entity: Entity<Workspace>) -> AnyElement {
    Button::new("agent-settings-entry")
        .ghost()
        .small()
        .icon(IconName::Settings2)
        .label("配置")
        .tooltip("智能体的模型与插件")
        .on_click(move |_, _, cx| {
            entity.update(cx, |workspace, cx| {
                workspace.open_scoped_settings_section(
                    settings::SettingsScope::AgentSetup,
                    settings::SettingsSection::PiModel,
                    cx,
                );
            });
        })
        .into_any_element()
}

fn product_page_header(
    title: &'static str,
    subtitle: &'static str,
    count: usize,
    count_unit: &'static str,
    action: AnyElement,
) -> AnyElement {
    div()
        .flex_shrink_0()
        .px_5()
        .py_4()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap_2()
                        .child(
                            div()
                                .text_lg()
                                .font_semibold()
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
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(crate::ui_theme::text_muted()))
                        .child(subtitle),
                ),
        )
        .child(action)
        .into_any_element()
}

fn automation_run_detail_section(
    title: &'static str,
    body: impl IntoElement,
    cx: &App,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_medium()
                .text_color(cx.theme().muted_foreground)
                .child(title),
        )
        .child(body)
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
        .icon(IconName::Redo)
        .tooltip("重新扫描插件目录")
        .on_click(move |_, _, cx| {
            refresh_entity.update(cx, |workspace, cx| workspace.refresh_agent_plugins(cx));
        });
    let manage_entity = entity.clone();
    let manage_button = compact_chip("agent-plugins-manage")
        .label("管理")
        .tooltip("去设置里导入或删除插件")
        .on_click(move |_, _, cx| {
            manage_entity.update(cx, |workspace, cx| {
                workspace.open_scoped_settings_section(
                    settings::SettingsScope::AgentSetup,
                    settings::SettingsSection::PiPlugins,
                    cx,
                );
            });
        });

    // 已勾选但磁盘上已经没有的条目也要露出来，否则用户没法取消它。
    let mut rows: Vec<AnyElement> = editor
        .plugins
        .iter()
        .map(|plugin| {
            agent_plugin_row(
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
        let (kind_label, name) = match smelt_core::pi_plugin_catalog::split_plugin_id(id) {
            Some((kind, name)) => (kind.label(), name.to_string()),
            None => ("未知", id.clone()),
        };
        rows.push(agent_plugin_row(
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
                                .child("能力插件"),
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
    let space_root = smelt_core::agent_definition_store::agent_space_root(&agent.id)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();

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
                    "工作目录 {space_root}，同一智能体的所有对话共用它。工作目录和绑定目录下的 {} 会自动作为技能加载。",
                    smelt_core::pi_plugin_catalog::WORKSPACE_SKILL_DIRS.join(" / ")
                )),
        )
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
                .xsmall()
                .icon(IconName::Close)
                .tooltip(if is_folder {
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

#[allow(clippy::too_many_arguments)]
fn agent_plugin_row(
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
            Switch::new(format!("agent-plugin-{plugin_id}"))
                .checked(checked)
                .disabled(!usable && !checked)
                .on_click(move |_, _, cx| {
                    let agent_id = toggle_agent.clone();
                    let plugin_id = toggle_plugin.clone();
                    entity.update(cx, |workspace, cx| {
                        workspace.toggle_agent_plugin(agent_id, plugin_id, cx)
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
        .h(px(28.))
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
        .pt_3()
        .min_h(px(120.))
        .child(textarea.appearance(false))
        .into_any_element()
}

fn compact_chip(id: impl Into<ElementId>) -> Button {
    Button::new(id).ghost().small().focus_ring(false)
}

fn automation_form_shell() -> Div {
    div()
        .w_full()
        .rounded(px(16.))
        .border_1()
        .border_color(crate::ui_theme::card_stroke())
        .bg(rgb(crate::ui_theme::bg_card()))
}

fn automation_form_row() -> Div {
    automation_form_shell()
        .h(px(44.))
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

/// 分段开关：描边跟输入框同一档，选中格铺满，看起来就是能点的控件。
fn segmented_track(_cx: &App) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .p_0p5()
        .rounded(px(8.))
        .border_1()
        .border_color(rgb(crate::ui_theme::border_loud()))
        .bg(rgb(crate::ui_theme::bg_column()))
}

mod automation_catalog;
mod automation_templates;
mod automation_view;
mod view;
mod workspace;

#[cfg(test)]
mod tests;
