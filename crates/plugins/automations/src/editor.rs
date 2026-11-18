//! 编辑器的状态和表单：名称、触发器（定时 / 间隔 / webhook）、动作、完成提醒。

use crate::prelude::*;
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScheduleKind {
    Daily,
    Weekdays,
    Weekly,
    Interval,
}

impl ScheduleKind {
    pub(crate) fn clock_kinds() -> [Self; 3] {
        [Self::Daily, Self::Weekdays, Self::Weekly]
    }

    pub(crate) fn label(self) -> SharedString {
        match self {
            Self::Daily => t!("schedule_kind.daily"),
            Self::Weekdays => t!("schedule_kind.weekdays"),
            Self::Weekly => t!("schedule_kind.weekly"),
            Self::Interval => t!("schedule_kind.interval"),
        }
        .into()
    }

    pub(crate) fn is_interval(self) -> bool {
        matches!(self, Self::Interval)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntervalUnit {
    Minutes,
    Hours,
}

impl IntervalUnit {
    pub(crate) fn label(self) -> SharedString {
        match self {
            Self::Minutes => t!("interval_unit.minutes"),
            Self::Hours => t!("interval_unit.hours"),
        }
        .into()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntervalDayScope {
    Everyday,
    Weekdays,
    Weekly,
}

impl IntervalDayScope {
    pub(crate) fn all() -> [Self; 3] {
        [Self::Everyday, Self::Weekdays, Self::Weekly]
    }

    pub(crate) fn label(self) -> SharedString {
        match self {
            Self::Everyday => t!("schedule_kind.daily"),
            Self::Weekdays => t!("schedule_kind.weekdays"),
            Self::Weekly => t!("schedule_kind.weekly"),
        }
        .into()
    }

    pub(crate) fn from_days(days: u8) -> Self {
        let days = days & api::model::automation::SCHEDULE_ALL_DAYS_MASK;
        if days == 0 || days == api::model::automation::SCHEDULE_ALL_DAYS_MASK {
            Self::Everyday
        } else if days == api::model::automation::SCHEDULE_WEEKDAYS_MASK {
            Self::Weekdays
        } else {
            Self::Weekly
        }
    }
}

pub(crate) struct ScheduleRuleEditor {
    pub(crate) kind: ScheduleKind,
    pub(crate) interval_unit: IntervalUnit,
    pub(crate) interval_day_scope: IntervalDayScope,
    pub(crate) weekday_mask: u8,
    pub(crate) time: Entity<InputState>,
    pub(crate) interval: Entity<InputState>,
    pub(crate) window_start: Entity<InputState>,
    pub(crate) window_end: Entity<InputState>,
    /// 空窗口默认收起，避免「每隔 15 分钟」第一眼摊开两个「不限」。
    pub(crate) window_open: bool,
}

impl ScheduleRuleEditor {
    /// 输入框的占位文字。建的时候和换语言时各设一次。
    pub(crate) fn label(&self, window: &mut Window, cx: &mut App) {
        self.window_start.update(cx, |input, cx| {
            input.set_placeholder(t!("schedule.window_start"), window, cx)
        });
        self.window_end.update(cx, |input, cx| {
            input.set_placeholder(t!("schedule.window_end"), window, cx)
        });
    }
}

pub(crate) struct TriggerEntryEditor {
    pub(crate) trigger_kind: AutomationTriggerKindId,
    pub(crate) schedule: ScheduleRuleEditor,
    pub(crate) webhook_secret: String,
}

pub(crate) struct AutomationEditor {
    pub(crate) instance_id: uuid::Uuid,
    pub(crate) action_kind: AutomationActionKindId,
    pub(crate) agent_id: String,
    /// 自动化在哪台机器上；保存、运行、智能体选项都按它。
    pub(crate) automation: AutomationRef,
    pub(crate) is_new: bool,
    pub(crate) name: Entity<InputState>,
    /// 名字平时当标题看，点一下才变成输入框，避免一进编辑器就闪光标。
    pub(crate) name_editing: bool,
    pub(crate) prompt: Entity<TextareaState>,
    pub(crate) command: Entity<TextareaState>,
    pub(crate) retained_events: Vec<EventIngress>,
    pub(crate) entries: Vec<TriggerEntryEditor>,
    pub(crate) notification: AutomationNotify,
    /// 在哪个目录里跑：运行时给的托管目录，或用户选的。保存时原样带回去，运行时据此
    /// 认出是不是托管的（`Automation::custom_workspace`）。
    pub(crate) workspace_dir: Option<String>,
    /// 从模板建的：模板指令里留给用户补的空（[`crate::templates::placeholders`]）。原样还在
    /// 指令里的算没填，填完才能创建。
    pub(crate) placeholders: Vec<String>,
    pub(crate) draft_revision: u64,
    pub(crate) saving_revision: Option<u64>,
    /// 重新生成地址这类后台提交不让顶栏「运行一次」进入 loading 状态。
    pub(crate) quiet_save: bool,
    pub(crate) error: Option<String>,
    /// 正闪着字的按钮：元素 id 和闪的字（「已复制 ✓」），两秒后清掉。
    pub(crate) flash: Option<(String, FlashLabel)>,
    pub(crate) _subscriptions: Vec<Subscription>,
}

/// 按钮点过后闪的字。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlashLabel {
    Copied,
    Regenerated,
}

impl FlashLabel {
    pub(crate) fn text(self) -> SharedString {
        match self {
            Self::Copied => t!("flash.copied"),
            Self::Regenerated => t!("flash.regenerated"),
        }
        .into()
    }
}

impl AutomationEditor {
    /// 输入框的占位文字。建的时候和换语言时各设一次。
    pub(crate) fn label(&self, window: &mut Window, cx: &mut App) {
        self.name.update(cx, |input, cx| {
            input.set_placeholder(t!("editor.name_placeholder"), window, cx)
        });
        self.prompt.update(cx, |input, cx| {
            input.set_placeholder(t!("editor.prompt_placeholder"), window, cx)
        });
        self.command.update(cx, |input, cx| {
            input.set_placeholder(t!("editor.command_placeholder"), window, cx)
        });
        for entry in &self.entries {
            entry.schedule.label(window, cx);
        }
    }

    /// 按钮上的字：刚点过的那颗闪一下，其余照常。
    pub(crate) fn button_label(&self, id: &str, idle: SharedString) -> SharedString {
        match &self.flash {
            Some((flashed, label)) if flashed == id => label.text(),
            _ => idle,
        }
    }
}

pub(crate) fn identity_name_should_finish_edit(event: &InputEvent) -> bool {
    matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. })
}

pub(crate) fn compact_instructions(value: &str) -> String {
    excerpt_text(value, 96)
}

pub(crate) fn excerpt_text(value: &str, limit: usize) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = value.chars();
    let mut compact = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        compact.push('…');
    }
    compact
}

pub(crate) fn clock_input_is_editable(value: &str) -> bool {
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

pub(crate) fn parse_clock(value: &str) -> Option<(u8, u8)> {
    let (hour, minute) = value.trim().split_once(':')?;
    if hour.is_empty() || minute.is_empty() || hour.len() > 2 || minute.len() > 2 {
        return None;
    }
    let hour = hour.parse::<u8>().ok()?;
    let minute = minute.parse::<u8>().ok()?;
    (hour <= 23 && minute <= 59).then_some((hour, minute))
}

pub(crate) fn schedule_from_values(
    kind: ScheduleKind,
    time: &str,
    interval: &str,
    interval_unit: IntervalUnit,
    weekday_mask: u8,
    window_start: &str,
    window_end: &str,
) -> Result<AutomationSchedule, String> {
    let schedule = match kind {
        ScheduleKind::Daily => {
            let (hour, minute) =
                parse_clock(time).ok_or_else(|| t!("error.invalid_time").into_owned())?;
            AutomationSchedule::Daily { hour, minute }
        }
        ScheduleKind::Weekdays => {
            let (hour, minute) =
                parse_clock(time).ok_or_else(|| t!("error.invalid_time").into_owned())?;
            AutomationSchedule::Weekly {
                days: api::model::automation::SCHEDULE_WEEKDAYS_MASK,
                hour,
                minute,
            }
        }
        ScheduleKind::Weekly => {
            let (hour, minute) =
                parse_clock(time).ok_or_else(|| t!("error.invalid_time").into_owned())?;
            let days = weekday_mask & api::model::automation::SCHEDULE_ALL_DAYS_MASK;
            if days == 0 {
                return Err(t!("error.no_days").into_owned());
            }
            if days == api::model::automation::SCHEDULE_ALL_DAYS_MASK {
                AutomationSchedule::Daily { hour, minute }
            } else {
                AutomationSchedule::Weekly { days, hour, minute }
            }
        }
        ScheduleKind::Interval => {
            let start = window_start.trim();
            let end = window_end.trim();
            let window = match (start.is_empty(), end.is_empty()) {
                (true, true) => None,
                (false, false) => {
                    let start =
                        parse_clock(start).ok_or_else(|| t!("error.invalid_time").into_owned())?;
                    let end =
                        parse_clock(end).ok_or_else(|| t!("error.invalid_time").into_owned())?;
                    Some((start, end))
                }
                _ => return Err(t!("error.window_incomplete").into_owned()),
            };
            let days = weekday_mask & api::model::automation::SCHEDULE_ALL_DAYS_MASK;
            let constrained = window.is_some()
                || (days != 0 && days != api::model::automation::SCHEDULE_ALL_DAYS_MASK);
            match interval_unit {
                IntervalUnit::Minutes => {
                    let minutes = interval
                        .trim()
                        .parse::<u32>()
                        .ok()
                        .filter(|minutes| (1..=24 * 60).contains(minutes))
                        .ok_or_else(|| t!("error.interval_minutes").into_owned())?;
                    if !constrained {
                        AutomationSchedule::EveryMinutes { minutes }
                    } else {
                        let ((start_hour, start_minute), (end_hour, end_minute)) =
                            window.unwrap_or(((0, 0), (23, 59)));
                        AutomationSchedule::During {
                            minutes,
                            days: if days == 0 {
                                api::model::automation::SCHEDULE_ALL_DAYS_MASK
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
                        .ok_or_else(|| t!("error.interval_hours").into_owned())?;
                    if !constrained {
                        AutomationSchedule::EveryHours { hours }
                    } else {
                        let ((start_hour, start_minute), (end_hour, end_minute)) =
                            window.unwrap_or(((0, 0), (23, 59)));
                        AutomationSchedule::During {
                            minutes: hours.saturating_mul(60),
                            days: if days == 0 {
                                api::model::automation::SCHEDULE_ALL_DAYS_MASK
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
    schedule.validate().map_err(|problem| problem.to_string())?;
    Ok(schedule)
}

pub(crate) fn trigger_from_editor(
    editor: &AutomationEditor,
    cx: &App,
) -> Result<AutomationTrigger, String> {
    let mut trigger = AutomationTrigger {
        events: editor.retained_events.clone(),
        ..Default::default()
    };
    for entry in &editor.entries {
        match entry.trigger_kind {
            AutomationTriggerKindId::Calendar | AutomationTriggerKindId::Interval => {
                trigger
                    .schedules
                    .push(schedule_from_values_for_rule(&entry.schedule, cx)?);
            }
            AutomationTriggerKindId::Webhook => {
                let secret = entry.webhook_secret.trim();
                let secret = if secret.is_empty() {
                    api::model::automation::new_webhook_secret()
                } else {
                    secret.to_string()
                };
                trigger.webhooks.push(WebhookIngress {
                    endpoint: secret.clone(),
                    secret: Some(secret),
                });
            }
        }
    }
    trigger.validate().map_err(|problem| problem.to_string())?;
    Ok(trigger)
}

impl AutomationEditor {
    pub(crate) fn action(&self, cx: &App) -> AutomationAction {
        match self.action_kind {
            AutomationActionKindId::Agent => {
                let prompt = self.prompt.read(cx).value().trim().to_string();
                AutomationAction::Agent {
                    agent_definition_id: self.agent_id.clone(),
                    prompt: (!prompt.is_empty()).then_some(prompt),
                }
            }
            AutomationActionKindId::Shell => AutomationAction::shell(
                self.command.read(cx).value().trim().to_string(),
                Vec::new(),
            ),
        }
    }

    pub(crate) fn missing_fields(&self, cx: &App) -> Vec<MissingField> {
        // 引用的是自动化所在那台机器上的智能体。
        let agent_exists = !self.agent_id.is_empty()
            && agent_definition(&self.automation.runtime, &self.agent_id, cx).is_some();
        let has_schedule = self
            .entries
            .iter()
            .any(|entry| entry.trigger_kind != AutomationTriggerKindId::Webhook);
        let action = self.action(cx);
        let unfilled = match &action {
            AutomationAction::Agent {
                prompt: Some(prompt),
                ..
            } => self
                .placeholders
                .iter()
                .filter(|slot| prompt.contains(slot.as_str()))
                .count(),
            _ => 0,
        };
        automation_missing_fields(
            &self.name.read(cx).value(),
            trigger_from_editor(self, cx).is_ok(),
            has_schedule,
            &action,
            agent_exists,
            unfilled,
        )
    }
}

pub(crate) fn schedule_from_values_for_rule(
    rule: &ScheduleRuleEditor,
    cx: &App,
) -> Result<AutomationSchedule, String> {
    let weekday_mask = if rule.kind == ScheduleKind::Interval {
        match rule.interval_day_scope {
            IntervalDayScope::Everyday => api::model::automation::SCHEDULE_ALL_DAYS_MASK,
            IntervalDayScope::Weekdays => api::model::automation::SCHEDULE_WEEKDAYS_MASK,
            IntervalDayScope::Weekly => {
                let days = rule.weekday_mask & api::model::automation::SCHEDULE_ALL_DAYS_MASK;
                if days == 0 {
                    return Err(t!("error.no_days").into_owned());
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

pub(crate) fn new_schedule_rule(
    window: &mut Window,
    cx: &mut Context<AutomationsPage>,
    schedule: Option<&AutomationSchedule>,
) -> ScheduleRuleEditor {
    let mut hour = 9_u8;
    let mut minute = 0_u8;
    let mut interval = 15_u32;
    let mut kind = ScheduleKind::Daily;
    let mut interval_unit = IntervalUnit::Minutes;
    let mut weekday_mask = api::model::automation::SCHEDULE_ALL_DAYS_MASK;
    let mut interval_day_scope = IntervalDayScope::Everyday;
    let mut window_start_value = String::new();
    let mut window_end_value = String::new();
    match schedule {
        Some(AutomationSchedule::Weekly {
            days,
            hour: start_hour,
            minute: start_minute,
        }) => {
            hour = *start_hour;
            minute = *start_minute;
            kind = if *days == api::model::automation::SCHEDULE_WEEKDAYS_MASK {
                ScheduleKind::Weekdays
            } else {
                ScheduleKind::Weekly
            };
            weekday_mask = *days;
            interval_day_scope = IntervalDayScope::from_days(*days);
        }
        Some(AutomationSchedule::Daily {
            hour: start_hour,
            minute: start_minute,
        }) => {
            hour = *start_hour;
            minute = *start_minute;
            kind = ScheduleKind::Daily;
        }
        Some(AutomationSchedule::EveryMinutes { minutes }) => {
            interval = (*minutes).max(1);
            kind = ScheduleKind::Interval;
        }
        Some(AutomationSchedule::EveryHours { hours }) => {
            interval = (*hours).max(1);
            kind = ScheduleKind::Interval;
            interval_unit = IntervalUnit::Hours;
        }
        Some(AutomationSchedule::During {
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
                api::model::automation::SCHEDULE_ALL_DAYS_MASK
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
            .validate(|value, _| clock_input_is_editable(value))
            .default_value(window_start_value)
    });
    let window_end = cx.new(|cx| {
        InputState::new(window, cx)
            .validate(|value, _| clock_input_is_editable(value))
            .default_value(window_end_value)
    });
    let editor = ScheduleRuleEditor {
        kind,
        interval_unit,
        interval_day_scope,
        weekday_mask,
        time,
        interval,
        window_start,
        window_end,
        window_open,
    };
    editor.label(window, cx);
    editor
}

pub(crate) fn webhook_ingress_secret(webhook: &WebhookIngress) -> String {
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
        .unwrap_or_else(api::model::automation::new_webhook_secret)
}

pub(crate) fn new_trigger_entry(
    window: &mut Window,
    cx: &mut Context<AutomationsPage>,
    trigger_kind: AutomationTriggerKindId,
    schedule: Option<&AutomationSchedule>,
    webhook_secret: Option<String>,
) -> TriggerEntryEditor {
    let interval_default = AutomationSchedule::EveryMinutes { minutes: 15 };
    let mut schedule = match (trigger_kind, schedule) {
        (_, Some(schedule)) => new_schedule_rule(window, cx, Some(schedule)),
        (AutomationTriggerKindId::Interval, None) => {
            new_schedule_rule(window, cx, Some(&interval_default))
        }
        _ => new_schedule_rule(window, cx, None),
    };
    match trigger_kind {
        AutomationTriggerKindId::Interval => schedule.kind = ScheduleKind::Interval,
        AutomationTriggerKindId::Calendar if schedule.kind.is_interval() => {
            schedule.kind = ScheduleKind::Daily;
        }
        _ => {}
    }
    TriggerEntryEditor {
        trigger_kind,
        schedule,
        webhook_secret: webhook_secret
            .filter(|secret| !secret.trim().is_empty())
            .unwrap_or_else(api::model::automation::new_webhook_secret),
    }
}

pub(crate) fn subscribe_trigger_entry_inputs(
    subscriptions: &mut Vec<Subscription>,
    entry: &TriggerEntryEditor,
    cx: &mut Context<AutomationsPage>,
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

pub(crate) fn render_trigger_entries(
    editor: &AutomationEditor,
    entity: Entity<AutomationsPage>,
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

pub(crate) fn render_add_trigger_button(entity: Entity<AutomationsPage>) -> AnyElement {
    div()
        .w_full()
        .h(ui_theme::height::xl())
        .rounded_full()
        .border_1()
        .border_dashed()
        .border_color(ui_theme::hair())
        .flex()
        .items_center()
        .child(
            MenuTrigger::dropdown(
                "automation-add-trigger-menu",
                Button::new("automation-add-trigger")
                    .quiet()
                    .w_full()
                    .icon(IconName::Plus)
                    .label(t!("trigger.add")),
                move |_, _| {
                    let mut menu = Menu::new();
                    let mut saw_advanced = false;
                    for preset in BUILTIN_AUTOMATION_TRIGGER_PRESETS {
                        if preset.is_advanced() && !saw_advanced {
                            menu = menu.separator();
                            saw_advanced = true;
                        }
                        let entity = entity.clone();
                        let preset = *preset;
                        menu = menu.item(
                            MenuItem::new(format!("{} · {}", preset.label(), preset.hint()))
                                .icon(trigger_preset_icon(preset))
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |page, cx| {
                                        page.add_trigger_preset(preset, window, cx);
                                    });
                                }),
                        );
                    }
                    menu
                },
            )
            .w_full(),
        )
        .into_any_element()
}

pub(crate) fn trigger_preset_icon(preset: AutomationTriggerPresetId) -> IconName {
    match preset {
        AutomationTriggerPresetId::Webhook => IconName::Webhook,
        AutomationTriggerPresetId::Hourly | AutomationTriggerPresetId::Interval => IconName::Clock,
        AutomationTriggerPresetId::Daily | AutomationTriggerPresetId::Weekly => IconName::Calendar,
    }
}

pub(crate) fn render_trigger_entry(
    index: usize,
    entry: &TriggerEntryEditor,
    editor: &AutomationEditor,
    entity: Entity<AutomationsPage>,
    cx: &App,
) -> AnyElement {
    let remove_entity = entity.clone();
    let title = trigger_entry_title(entry, cx);
    let hint = trigger_entry_hint(entry);
    let show_hint = title.as_str() != hint.as_ref();
    let icon: Icon = match entry.trigger_kind {
        AutomationTriggerKindId::Webhook => Icon::new(IconName::Webhook),
        AutomationTriggerKindId::Interval => Icon::new(IconName::Clock),
        AutomationTriggerKindId::Calendar => IconName::Calendar.into(),
    };
    let body = match entry.trigger_kind {
        AutomationTriggerKindId::Webhook => render_webhook_entry(index, entry, editor, entity, cx),
        AutomationTriggerKindId::Calendar | AutomationTriggerKindId::Interval => {
            render_schedule_rule(index, &entry.schedule, false, entity)
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
                                .type_style(TYPE.ui_medium)
                                .text_color(rgb(ui_theme::text()))
                                .child(title),
                        )
                        .when(show_hint, |col| {
                            col.child(
                                div()
                                    .type_style(TYPE.text_xsmall)
                                    .text_color(rgb(ui_theme::muted()))
                                    .child(hint),
                            )
                        }),
                )
                .child(
                    compact_chip("automation-remove-trigger")
                        .icon(IconName::Trash)
                        .icon_action(t!("trigger.delete"))
                        .on_click(move |_, _, cx| {
                            remove_entity.update(cx, |page, cx| {
                                page.remove_trigger_entry(index, cx);
                            });
                        }),
                ),
        )
        .child(body)
        .into_any_element()
}

/// 目录和编辑器里的小图标。不染色，底就是悬停灰。
pub(crate) fn neutral_icon_mark(icon: impl Into<Icon>, size: f32) -> AnyElement {
    let icon: Icon = icon.into();
    div()
        .size(px(size))
        .rounded(ui_theme::row_radius())
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(ui_theme::soft()))
        .child(
            icon.size(px((size * 0.5).round()))
                .text_color(rgb(ui_theme::muted())),
        )
        .into_any_element()
}

pub(crate) fn automation_trigger_glyph(icon: Icon) -> AnyElement {
    neutral_icon_mark(icon, 32.)
}

pub(crate) fn trigger_entry_title(entry: &TriggerEntryEditor, cx: &App) -> String {
    match entry.trigger_kind {
        AutomationTriggerKindId::Webhook => t!("trigger_preset.webhook_hint").into_owned(),
        AutomationTriggerKindId::Calendar => match entry.schedule.kind {
            ScheduleKind::Daily => t!("schedule_kind.daily").into_owned(),
            ScheduleKind::Weekdays => t!("schedule_kind.weekdays").into_owned(),
            ScheduleKind::Weekly => t!("schedule_kind.weekly").into_owned(),
            ScheduleKind::Interval => interval_entry_title(entry, cx),
        },
        AutomationTriggerKindId::Interval => interval_entry_title(entry, cx),
    }
}

pub(crate) fn interval_entry_title(entry: &TriggerEntryEditor, cx: &App) -> String {
    interval_title(
        entry.schedule.interval_unit,
        &entry.schedule.interval.read(cx).value(),
    )
}

pub(crate) fn interval_title(unit: IntervalUnit, value: &str) -> String {
    let value = value.trim();
    match unit {
        _ if value.is_empty() => t!("trigger_preset.interval").into_owned(),
        IntervalUnit::Hours if value == "1" => t!("trigger_preset.hourly").into_owned(),
        IntervalUnit::Hours => t!("schedule.every_hours", count = value).into_owned(),
        IntervalUnit::Minutes => t!("schedule.every_minutes", count = value).into_owned(),
    }
}

pub(crate) fn trigger_entry_hint(entry: &TriggerEntryEditor) -> SharedString {
    match entry.trigger_kind {
        AutomationTriggerKindId::Webhook => t!("trigger_preset.webhook_hint"),
        AutomationTriggerKindId::Calendar => match entry.schedule.kind {
            ScheduleKind::Daily => t!("trigger_preset.daily_hint"),
            ScheduleKind::Weekdays => t!("trigger_preset.weekdays_hint"),
            ScheduleKind::Weekly => t!("trigger_preset.weekly_hint"),
            ScheduleKind::Interval => t!("trigger_preset.fixed_interval_hint"),
        },
        AutomationTriggerKindId::Interval => t!("trigger_preset.interval_hint"),
    }
    .into()
}

pub(crate) fn render_notification_field(
    editor: &AutomationEditor,
    entity: Entity<AutomationsPage>,
    _cx: &App,
) -> AnyElement {
    let selected = editor.notification;
    let picker = Segmented::new("automation-notification")
        .segments(NOTIFY_CHOICES.iter().map(|notify| notify_label(*notify)))
        .selected(NOTIFY_CHOICES.iter().position(|notify| *notify == selected))
        .on_select(move |ix, _, cx| {
            if let Some(notify) = NOTIFY_CHOICES.get(ix).copied() {
                entity.update(cx, |page, cx| page.set_automation_notification(notify, cx));
            }
        });
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(automation_section_label(t!("notification.title")))
        .child(div().flex().child(picker))
        .child(crate::view::automation_field_hint(notify_hint(selected)))
        .into_any_element()
}

/// 在哪做：默认目录说清是哪儿；选了文件夹就显示路径，能改回默认。
pub(crate) fn render_workspace_field(
    editor: &AutomationEditor,
    is_agent: bool,
    entity: Entity<AutomationsPage>,
) -> AnyElement {
    let custom = editor.workspace_dir.as_deref().filter(|dir| {
        !dir.trim().is_empty()
            && !api::model::automation::is_managed_workspace(dir, &editor.automation.id)
    });
    let current = div()
        .flex_1()
        .min_w_0()
        .truncate()
        .type_style(TYPE.text_small)
        .text_color(rgb(if custom.is_some() {
            ui_theme::text()
        } else {
            ui_theme::muted()
        }))
        .child(SharedString::from(custom.map_or_else(
            || t!("workspace.default").into_owned(),
            str::to_owned,
        )));
    let choose = Button::new("automation-workspace-choose")
        .small()
        .label(t!("workspace.choose"))
        .on_click({
            let entity = entity.clone();
            move |_, _, cx| entity.update(cx, |page, cx| page.pick_automation_workspace(cx))
        });
    let reset = custom.is_some().then(|| {
        Button::new("automation-workspace-reset")
            .small()
            .quiet()
            .label(t!("workspace.reset"))
            .on_click(move |_, _, cx| {
                entity.update(cx, |page, cx| page.set_automation_workspace(None, cx))
            })
    });
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(current)
                .child(choose)
                .children(reset),
        )
        .when(custom.is_none(), |field| {
            field.child(crate::view::automation_field_hint(if is_agent {
                t!("workspace.default_agent").into()
            } else {
                t!("workspace.default_shell").into()
            }))
        })
        .into_any_element()
}

/// 权限说明。这里不能切换运行方式，所以不做成和通知一样的设置卡。
pub(crate) fn render_automation_access_note(is_agent: bool) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .type_style(TYPE.text_xsmall_medium)
                .text_color(rgb(ui_theme::muted()))
                .child(t!("access.title").into_owned()),
        )
        .child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(ui_theme::muted()))
                .child(automation_access_note(is_agent)),
        )
        .into_any_element()
}

pub(crate) fn automation_access_note(is_agent: bool) -> SharedString {
    if is_agent {
        t!("access.agent_note")
    } else {
        t!("access.shell_note")
    }
    .into()
}

pub(crate) fn render_webhook_entry(
    index: usize,
    entry: &TriggerEntryEditor,
    editor: &AutomationEditor,
    entity: Entity<AutomationsPage>,
    cx: &App,
) -> AnyElement {
    // webhook 打到自动化所在的那台机器，地址前缀用它自己推来的。
    let machine = automations(cx).on(&editor.automation.runtime);
    let base = machine
        .and_then(|machine| machine.webhook_base_url.clone())
        .unwrap_or_else(|| api::model::automation::DEFAULT_WEBHOOK_BASE_URL.to_string());
    let stored = machine.and_then(|machine| machine.automation(&editor.automation.id));
    let paused = stored.is_some_and(|automation| !automation.enabled);
    let live_url = (!editor.is_new && !entry.webhook_secret.is_empty())
        .then(|| api::model::automation::webhook_url(&base, &entry.webhook_secret));
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
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(ui_theme::muted()))
                .child(SharedString::from(if live_url.is_some() {
                    t!("webhook.post_hint")
                } else {
                    t!("webhook.save_hint")
                })),
        )
        .when(paused, |body| {
            body.child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(rgb(ui_theme::muted()))
                    .child(t!("webhook.paused").into_owned()),
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
                            .border_color(ui_theme::hair_strong())
                            .bg(rgb(ui_theme::side()))
                            .type_style(TYPE.text_xsmall)
                            .text_color(rgb(ui_theme::text()))
                            .child(url),
                    )
                    .child(
                        compact_chip("automation-copy-webhook")
                            .label(editor.button_label(&copy_id, t!("webhook.copy").into()))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()));
                                let copy_id_click = copy_id_click.clone();
                                copy_entity.update(cx, |page, cx| {
                                    page.flash_editor_button(copy_id_click, FlashLabel::Copied, cx);
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
                            .label(editor.button_label(&curl_id, t!("webhook.copy_example").into()))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_curl.clone()));
                                let curl_id_click = curl_id_click.clone();
                                copy_entity.update(cx, |page, cx| {
                                    page.flash_editor_button(curl_id_click, FlashLabel::Copied, cx);
                                });
                            })
                    })
                })
                .when(!editor.is_new, |actions| {
                    let entity = entity.clone();
                    actions.child(
                        compact_chip("automation-regen-webhook")
                            .loading(editor.saving_revision.is_some() && editor.quiet_save)
                            .label(editor.button_label(&regen_id, t!("webhook.regenerate").into()))
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |page, cx| {
                                    page.regenerate_automation_webhook_secret(index, cx);
                                });
                            }),
                    )
                }),
        )
        .into_any_element()
}

pub(crate) fn webhook_curl_example(url: &str) -> String {
    format!(
        "curl -sS -X POST {url} -H 'Content-Type: application/json' -d '{{\"text\":\"{}\"}}'",
        t!("webhook.example_text")
    )
}

pub(crate) fn automation_prompt_hint(is_webhook: bool) -> SharedString {
    if is_webhook {
        t!("prompt.webhook_hint")
    } else {
        t!("prompt.hint")
    }
    .into()
}

pub(crate) fn render_schedule_rule(
    index: usize,
    rule: &ScheduleRuleEditor,
    can_remove: bool,
    entity: Entity<AutomationsPage>,
) -> AnyElement {
    let frequency = (!rule.kind.is_interval()).then(|| {
        let kinds = ScheduleKind::clock_kinds();
        let frequency_entity = entity.clone();
        Segmented::new(("agent-trigger-frequency", index))
            .segments(kinds.map(|kind| kind.label()))
            .selected(kinds.iter().position(|kind| *kind == rule.kind))
            .on_select(move |ix, _, cx| {
                if let Some(kind) = kinds.get(ix).copied() {
                    frequency_entity.update(cx, |page, cx| {
                        page.set_trigger_schedule_kind(index, kind, cx);
                    });
                }
            })
    });
    let clock = |input: &Entity<InputState>, aria: std::borrow::Cow<'static, str>| {
        TextInput::new(input)
            .aria_label(aria)
            .w(px(108.))
            .into_any_element()
    };
    let timezone = || {
        div()
            .type_style(TYPE.text_xsmall)
            .text_color(rgb(ui_theme::muted()))
            .child(local_timezone_label())
    };
    // 星期可以多选：每天各自开关。
    let weekday_picker = |id_prefix: &'static str| {
        let mask = rule.weekday_mask;
        let day_entity = entity.clone();
        Segmented::new((id_prefix, index))
            .segments((0..7).map(crate::format::weekday_label))
            .multiple((0..7).map(|day_index| mask & (1_u8 << day_index) != 0))
            .on_select(move |day_index, _, cx| {
                day_entity.update(cx, |page, cx| {
                    page.toggle_schedule_weekday(index, 1_u8 << day_index, cx);
                });
            })
    };
    let interval_unit = rule.interval_unit;
    let schedule_control = match rule.kind {
        ScheduleKind::Interval => {
            let scopes = IntervalDayScope::all();
            let scope_entity = entity.clone();
            let day_scope = Segmented::new(("automation-interval-days", index))
                .segments(scopes.map(|scope| scope.label()))
                .selected(
                    scopes
                        .iter()
                        .position(|scope| *scope == rule.interval_day_scope),
                )
                .on_select(move |ix, _, cx| {
                    if let Some(scope) = scopes.get(ix).copied() {
                        scope_entity.update(cx, |page, cx| {
                            page.set_interval_day_scope(index, scope, cx);
                        });
                    }
                });
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
                            TextInput::new(&rule.interval)
                                .aria_label(t!("trigger_preset.interval"))
                                .w(px(120.))
                                .into_any_element(),
                        )
                        .child({
                            const UNITS: [IntervalUnit; 2] =
                                [IntervalUnit::Minutes, IntervalUnit::Hours];
                            let unit_entity = entity.clone();
                            Segmented::new(("automation-interval-unit", index))
                                .segments(UNITS.map(|unit| unit.label()))
                                .selected(UNITS.iter().position(|unit| *unit == interval_unit))
                                .on_select(move |ix, _, cx| {
                                    if let Some(unit) = UNITS.get(ix).copied() {
                                        unit_entity.update(cx, |page, cx| {
                                            page.set_interval_unit(index, unit, cx);
                                        });
                                    }
                                })
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
                                .type_style(TYPE.text_xsmall)
                                .text_color(rgb(ui_theme::muted()))
                                .child(t!("schedule.from").into_owned()),
                        )
                        .child(clock(&rule.window_start, t!("schedule.start_time")))
                        .child(
                            div()
                                .type_style(TYPE.text_xsmall)
                                .text_color(rgb(ui_theme::muted()))
                                .child(t!("schedule.to").into_owned()),
                        )
                        .child(clock(&rule.window_end, t!("schedule.end_time")))
                        .child(timezone())
                        .child(
                            compact_chip("automation-interval-window-clear")
                                .label(t!("schedule.all_day"))
                                .on_click(move |_, window, cx| {
                                    close_entity.update(cx, |page, cx| {
                                        page.set_interval_window_open(index, false, window, cx);
                                    });
                                }),
                        ),
                );
            } else {
                let open_entity = entity.clone();
                interval_body = interval_body.child(
                    compact_chip("automation-interval-window-open")
                        .label(t!("schedule.limit_hours"))
                        .on_click(move |_, window, cx| {
                            open_entity.update(cx, |page, cx| {
                                page.set_interval_window_open(index, true, window, cx);
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
                    t!("schedule.daily_time")
                } else {
                    t!("schedule.weekdays_time")
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
                    .child(clock(&rule.time, t!("schedule.run_time")))
                    .child(timezone()),
            )
            .into_any_element(),
    };
    let remove = can_remove.then(|| {
        let remove_entity = entity.clone();
        compact_chip("automation-remove-schedule")
            .icon(IconName::Trash)
            .icon_action(t!("schedule.delete"))
            .on_click(move |_, _, cx| {
                remove_entity.update(cx, |page, cx| {
                    page.remove_trigger_entry(index, cx);
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
            .border_color(ui_theme::hair())
            .into_any_element()
    } else {
        body.into_any_element()
    }
}
