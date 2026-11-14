//! 编辑器的状态和表单：名称、触发器（定时 / 间隔 / webhook）、动作、完成提醒。

use crate::prelude::*;

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

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Daily => "每天",
            Self::Weekdays => "工作日",
            Self::Weekly => "每周",
            Self::Interval => "每隔",
        }
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
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Minutes => "分钟",
            Self::Hours => "小时",
        }
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

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Everyday => "每天",
            Self::Weekdays => "工作日",
            Self::Weekly => "每周",
        }
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
    pub(crate) notification: AutomationNotificationPreset,
    pub(crate) draft_revision: u64,
    pub(crate) saving_revision: Option<u64>,
    /// 重新生成地址这类后台提交不让顶栏「运行一次」进入 loading 状态。
    pub(crate) quiet_save: bool,
    pub(crate) error: Option<String>,
    /// 正闪着字的按钮：元素 id 和闪的字（「已复制 ✓」），两秒后清掉。
    pub(crate) flash: Option<(String, &'static str)>,
    pub(crate) _subscriptions: Vec<Subscription>,
}

impl AutomationEditor {
    /// 按钮上的字：刚点过的那颗闪一下，其余照常。
    pub(crate) fn button_label(&self, id: &str, idle: &'static str) -> &'static str {
        match &self.flash {
            Some((flashed, label)) if flashed == id => label,
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
                parse_clock(time).ok_or_else(|| "请输入 00:00 到 23:59 之间的时间".to_string())?;
            AutomationSchedule::Daily { hour, minute }
        }
        ScheduleKind::Weekdays => {
            let (hour, minute) =
                parse_clock(time).ok_or_else(|| "请输入 00:00 到 23:59 之间的时间".to_string())?;
            AutomationSchedule::Weekly {
                days: api::model::automation::SCHEDULE_WEEKDAYS_MASK,
                hour,
                minute,
            }
        }
        ScheduleKind::Weekly => {
            let (hour, minute) =
                parse_clock(time).ok_or_else(|| "请输入 00:00 到 23:59 之间的时间".to_string())?;
            let days = weekday_mask & api::model::automation::SCHEDULE_ALL_DAYS_MASK;
            if days == 0 {
                return Err("请至少选择一天".to_string());
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
                    let start = parse_clock(start)
                        .ok_or_else(|| "请输入 00:00 到 23:59 之间的时间".to_string())?;
                    let end = parse_clock(end)
                        .ok_or_else(|| "请输入 00:00 到 23:59 之间的时间".to_string())?;
                    Some((start, end))
                }
                _ => return Err("请同时填写开始和结束时间".to_string()),
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
                        .ok_or_else(|| "执行间隔需为 1 到 1440 分钟".to_string())?;
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
                        .ok_or_else(|| "执行间隔需为 1 到 168 小时".to_string())?;
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
    schedule.validate()?;
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
    trigger.validate()?;
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

    pub(crate) fn missing_fields(&self, cx: &App) -> Vec<&'static str> {
        // 引用的是自动化所在那台机器上的智能体。
        let agent_exists = !self.agent_id.is_empty()
            && agent_definition(&self.automation.runtime, &self.agent_id, cx).is_some();
        let has_schedule = self
            .entries
            .iter()
            .any(|entry| entry.trigger_kind != AutomationTriggerKindId::Webhook);
        automation_missing_fields(
            &self.name.read(cx).value(),
            trigger_from_editor(self, cx).is_ok(),
            has_schedule,
            &self.action(cx),
            agent_exists,
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
        .border_color(ui_theme::card_stroke())
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
                    for preset in BUILTIN_AUTOMATION_TRIGGER_PRESETS {
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
                                    entity.update(cx, |page, cx| {
                                        page.add_trigger_preset(preset, window, cx);
                                    });
                                }),
                        );
                    }
                    menu
                }),
        )
        .into_any_element()
}

pub(crate) fn trigger_preset_icon(preset: AutomationTriggerPresetId) -> Icon {
    match preset {
        AutomationTriggerPresetId::Webhook => Icon::new(gpui_kit_assets::IconName::Webhook),
        AutomationTriggerPresetId::Hourly | AutomationTriggerPresetId::Interval => {
            Icon::new(gpui_kit_assets::IconName::Clock)
        }
        AutomationTriggerPresetId::Daily | AutomationTriggerPresetId::Weekly => {
            IconName::Calendar.into()
        }
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
    let show_hint = title != hint;
    let icon: Icon = match entry.trigger_kind {
        AutomationTriggerKindId::Webhook => Icon::new(gpui_kit_assets::IconName::Webhook),
        AutomationTriggerKindId::Interval => Icon::new(gpui_kit_assets::IconName::Clock),
        AutomationTriggerKindId::Calendar => IconName::Calendar.into(),
    };
    let body = match entry.trigger_kind {
        AutomationTriggerKindId::Webhook => render_webhook_entry(index, entry, editor, entity, cx),
        AutomationTriggerKindId::Calendar | AutomationTriggerKindId::Interval => {
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
                                .text_color(rgb(ui_theme::text_bright()))
                                .child(title),
                        )
                        .when(show_hint, |col| {
                            col.child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(ui_theme::text_muted()))
                                    .child(hint),
                            )
                        }),
                )
                .child(
                    compact_chip("automation-remove-trigger")
                        .icon(api::ui::trash_icon())
                        .icon_action("删除触发器")
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
    div()
        .size(px(size))
        .rounded(ui_theme::row_radius())
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(ui_theme::bg_hover()))
        .child(
            Icon::new(icon)
                .size(px((size * 0.5).round()))
                .text_color(rgb(ui_theme::text_mid())),
        )
        .into_any_element()
}

pub(crate) fn automation_trigger_glyph(icon: Icon) -> AnyElement {
    neutral_icon_mark(icon, 32.)
}

pub(crate) fn trigger_entry_title(entry: &TriggerEntryEditor, cx: &App) -> String {
    match entry.trigger_kind {
        AutomationTriggerKindId::Webhook => "当此 webhook 收到请求时".to_string(),
        AutomationTriggerKindId::Calendar => match entry.schedule.kind {
            ScheduleKind::Daily => "每天".to_string(),
            ScheduleKind::Weekdays => "工作日".to_string(),
            ScheduleKind::Weekly => "每周".to_string(),
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
        _ if value.is_empty() => "间隔".to_string(),
        IntervalUnit::Hours if value == "1" => "每小时".to_string(),
        IntervalUnit::Hours => format!("每隔 {value} 小时"),
        IntervalUnit::Minutes => format!("每隔 {value} 分钟"),
    }
}

pub(crate) fn trigger_entry_hint(entry: &TriggerEntryEditor) -> &'static str {
    match entry.trigger_kind {
        AutomationTriggerKindId::Webhook => "当此 webhook 收到请求时",
        AutomationTriggerKindId::Calendar => match entry.schedule.kind {
            ScheduleKind::Daily => "每天在选定的时间",
            ScheduleKind::Weekdays => "工作日在选定的时间",
            ScheduleKind::Weekly => "每周在选定的一天",
            ScheduleKind::Interval => "按固定间隔执行",
        },
        AutomationTriggerKindId::Interval => "按固定间隔执行，可限制星期和时段",
    }
}

pub(crate) fn render_notification_field(
    editor: &AutomationEditor,
    entity: Entity<AutomationsPage>,
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
                for preset in AutomationNotificationPreset::ALL {
                    let entity = picker_entity.clone();
                    menu = menu.item(
                        PopupMenuItem::new(preset.label())
                            .icon(notification_preset_icon(preset))
                            .checked(preset == selected)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |page, cx| {
                                    page.set_automation_notification(preset, cx);
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
pub(crate) fn render_automation_access_note(is_agent: bool) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_medium()
                .text_color(rgb(ui_theme::text_muted()))
                .child("无人值守 · 完全访问"),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(ui_theme::text_muted()))
                .child(automation_access_note(is_agent)),
        )
        .into_any_element()
}

pub(crate) fn automation_access_note(is_agent: bool) -> &'static str {
    if is_agent {
        "运行时不会停下来等你批准，可以读写本机文件、执行命令。在智能体自己的空间里运行，能读到它在对话里攒下的文件和绑定的目录；你的项目不会自动带上，要处理的仓库请在指令里写明路径。"
    } else {
        "运行时不会停下来等你批准，命令以你的身份执行，可以读写本机文件。每次都在这条自动化自己的目录里执行。"
    }
}

pub(crate) fn notification_preset_icon(preset: AutomationNotificationPreset) -> Icon {
    match preset {
        AutomationNotificationPreset::Off => Icon::new(gpui_kit_assets::IconName::BellOff),
        AutomationNotificationPreset::App => IconName::Bell.into(),
    }
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
                    .text_color(rgb(ui_theme::text_muted()))
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
                            .border_color(rgb(ui_theme::border_loud()))
                            .bg(rgb(ui_theme::bg_column()))
                            .text_xs()
                            .text_color(rgb(ui_theme::text_bright()))
                            .child(url),
                    )
                    .child(
                        compact_chip("automation-copy-webhook")
                            .label(editor.button_label(&copy_id, "复制"))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()));
                                let copy_id_click = copy_id_click.clone();
                                copy_entity.update(cx, |page, cx| {
                                    page.flash_editor_button(copy_id_click, "已复制 ✓", cx);
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
                            .label(editor.button_label(&curl_id, "复制示例"))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_curl.clone()));
                                let curl_id_click = curl_id_click.clone();
                                copy_entity.update(cx, |page, cx| {
                                    page.flash_editor_button(curl_id_click, "已复制 ✓", cx);
                                });
                            })
                    })
                })
                .when(!editor.is_new, |actions| {
                    let entity = entity.clone();
                    actions.child(
                        compact_chip("automation-regen-webhook")
                            .loading(editor.saving_revision.is_some() && editor.quiet_save)
                            .label(editor.button_label(&regen_id, "重新生成地址"))
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
    format!("curl -sS -X POST {url} -H 'Content-Type: application/json' -d '{{\"text\":\"你好\"}}'")
}

pub(crate) fn automation_prompt_hint(is_webhook: bool) -> &'static str {
    if is_webhook {
        "收到的请求内容会附在指令后面；想放到指定位置，用 {{payload}} 或字段变量（如 {{text}}）。留空时直接发送请求内容。长期指令请在智能体设置中配置。"
    } else {
        "每次运行时发送给智能体。长期指令请在智能体设置中配置。"
    }
}

pub(crate) fn render_schedule_rule(
    index: usize,
    rule: &ScheduleRuleEditor,
    can_remove: bool,
    entity: Entity<AutomationsPage>,
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
                    frequency_entity.update(cx, |page, cx| {
                        page.set_trigger_schedule_kind(index, kind, cx);
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
                    .label(api::model::automation::SCHEDULE_DAY_LABELS[day_index])
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
            let day_scope = choice_tabs(
                ("automation-interval-days", index),
                scopes
                    .iter()
                    .position(|scope| *scope == rule.interval_day_scope),
                scopes.iter().map(|scope| Tab::new().label(scope.label())),
                move |ix, cx| {
                    if let Some(scope) = scopes.get(ix).copied() {
                        scope_entity.update(cx, |page, cx| {
                            page.set_interval_day_scope(index, scope, cx);
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
                                        unit_entity.update(cx, |page, cx| {
                                            page.set_interval_unit(index, unit, cx);
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
                        .label("限制时段")
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
            .icon(api::ui::trash_icon())
            .icon_action("删除时间设置")
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
            .border_color(rgb(ui_theme::border()))
            .into_any_element()
    } else {
        body.into_any_element()
    }
}
