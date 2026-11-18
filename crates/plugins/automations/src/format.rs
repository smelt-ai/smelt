//! 时间、状态、来源这些给人看的短语。

use crate::prelude::*;

pub(crate) fn local_timezone_label() -> String {
    let seconds = Local::now().offset().local_minus_utc();
    let sign = if seconds >= 0 { '+' } else { '-' };
    let total_minutes = seconds.abs() / 60;
    let offset = format!(
        "UTC{sign}{:02}:{:02}",
        total_minutes / 60,
        total_minutes % 60
    );
    t!("timezone.local", offset = offset).into_owned()
}

pub(crate) fn format_unix_local(ts: i64) -> String {
    Utc.timestamp_opt(ts, 0)
        .single()
        .map(|utc| utc.with_timezone(&Local).format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|| ts.to_string())
}

pub(crate) fn format_relative_until(ts: i64, now: i64) -> String {
    let delta = ts - now;
    if delta <= 0 {
        return t!("time.due_now").into_owned();
    }
    pub(crate) const MINUTE: i64 = 60;
    pub(crate) const HOUR: i64 = 60 * MINUTE;
    pub(crate) const DAY: i64 = 24 * HOUR;
    if delta < HOUR {
        let minutes = (delta / MINUTE).max(1);
        t!("time.in_minutes", count = minutes).into_owned()
    } else if delta < DAY {
        t!("time.in_hours", count = delta / HOUR).into_owned()
    } else {
        t!("time.in_days", count = delta / DAY).into_owned()
    }
}

pub(crate) fn format_next_run_cell(next_run_at: Option<i64>, enabled: bool, now: i64) -> String {
    if !enabled {
        return "—".to_string();
    }
    match next_run_at {
        Some(ts) => format_relative_until(ts, now),
        None => "—".to_string(),
    }
}

pub(crate) fn format_relative_since(ts: i64, now: i64) -> String {
    let delta = now - ts;
    pub(crate) const MINUTE: i64 = 60;
    pub(crate) const HOUR: i64 = 60 * MINUTE;
    pub(crate) const DAY: i64 = 24 * HOUR;
    if delta < MINUTE {
        t!("time.just_now").into_owned()
    } else if delta < HOUR {
        t!("time.minutes_ago", count = delta / MINUTE).into_owned()
    } else if delta < DAY {
        t!("time.hours_ago", count = delta / HOUR).into_owned()
    } else if delta < 30 * DAY {
        t!("time.days_ago", count = delta / DAY).into_owned()
    } else if delta < 365 * DAY {
        t!("time.months_ago", count = delta / (30 * DAY)).into_owned()
    } else {
        t!("time.years_ago", count = delta / (365 * DAY)).into_owned()
    }
}

pub(crate) fn format_last_run_cell(
    last: Option<(api::model::automation::AutomationRunStatus, i64)>,
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

/// 保存前必须填的一项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MissingField {
    Name,
    Trigger,
    Agent,
    Prompt,
    Command,
}

impl MissingField {
    pub(crate) fn label(self) -> SharedString {
        match self {
            Self::Name => t!("missing.name"),
            Self::Trigger => t!("missing.trigger"),
            Self::Agent => t!("missing.agent"),
            Self::Prompt => t!("missing.prompt"),
            Self::Command => t!("missing.command"),
        }
        .into()
    }
}

/// 表单还差哪些才能保存。定时触发不带外部内容，所以只有它要求智能体指令；
/// webhook / 事件会把收到的内容当指令。
pub(crate) fn automation_missing_fields(
    name: &str,
    trigger_ready: bool,
    has_schedule: bool,
    action: &AutomationAction,
    agent_exists: bool,
) -> Vec<MissingField> {
    let mut missing = Vec::new();
    if name.trim().is_empty() {
        missing.push(MissingField::Name);
    }
    if !trigger_ready {
        missing.push(MissingField::Trigger);
    }
    match action {
        AutomationAction::Agent { prompt, .. } => {
            if !agent_exists {
                missing.push(MissingField::Agent);
            }
            if has_schedule
                && prompt
                    .as_deref()
                    .is_none_or(|prompt| prompt.trim().is_empty())
            {
                missing.push(MissingField::Prompt);
            }
        }
        AutomationAction::Shell { command, .. } => {
            if command.trim().is_empty() {
                missing.push(MissingField::Command);
            }
        }
    }
    missing
}

/// 页头上的保存状态。新建还没落盘，只说还差什么；已有的会在补齐后自动保存。
pub(crate) fn automation_save_hint(missing: &[MissingField], is_new: bool) -> Option<String> {
    if missing.is_empty() {
        return None;
    }
    let list = missing
        .iter()
        .map(|field| field.label().to_string())
        .collect::<Vec<_>>()
        .join(&*t!("list.separator"));
    Some(if is_new {
        t!("missing.hint_new", fields = list).into_owned()
    } else {
        t!("missing.hint", fields = list).into_owned()
    })
}

pub(crate) fn run_source_label(
    source: api::model::automation::AutomationRunSource,
) -> SharedString {
    use api::model::automation::AutomationRunSource;
    match source {
        AutomationRunSource::Manual => t!("run_source.manual"),
        AutomationRunSource::Scheduled => t!("run_source.scheduled"),
        AutomationRunSource::Webhook => t!("run_source.webhook"),
        AutomationRunSource::Event => t!("run_source.event"),
    }
    .into()
}

pub(crate) fn run_status_label(
    status: api::model::automation::AutomationRunStatus,
) -> SharedString {
    use api::model::automation::AutomationRunStatus;
    match status {
        AutomationRunStatus::Starting => t!("run_status.starting"),
        AutomationRunStatus::Queued => t!("run_status.queued"),
        AutomationRunStatus::Dispatching => t!("run_status.dispatching"),
        AutomationRunStatus::Running => t!("run_status.running"),
        AutomationRunStatus::AwaitingApproval => t!("run_status.awaiting_approval"),
        AutomationRunStatus::WaitingForUser => t!("run_status.waiting_for_user"),
        AutomationRunStatus::Completed => t!("run_status.completed"),
        AutomationRunStatus::Failed => t!("run_status.failed"),
        AutomationRunStatus::Cancelled => t!("run_status.cancelled"),
        AutomationRunStatus::Skipped => t!("run_status.skipped"),
    }
    .into()
}

/// 一条排程给人看的写法：「每天 09:00」「每周一、三 · 09:35」。
pub(crate) fn schedule_summary(schedule: &AutomationSchedule) -> String {
    match *schedule {
        AutomationSchedule::EveryMinutes { minutes } => {
            t!("schedule.every_minutes", count = minutes).into_owned()
        }
        AutomationSchedule::EveryHours { hours } => {
            t!("schedule.every_hours", count = hours).into_owned()
        }
        AutomationSchedule::Daily { hour, minute } => {
            t!("schedule.daily", time = clock_text(hour, minute)).into_owned()
        }
        AutomationSchedule::Weekly { days, hour, minute } => t!(
            "schedule.weekly",
            days = weekly_days_summary(days),
            time = clock_text(hour, minute)
        )
        .into_owned(),
        AutomationSchedule::During {
            minutes,
            days,
            start_hour,
            start_minute,
            end_hour,
            end_minute,
        } => {
            let days = weekly_days_summary(if days == 0 {
                api::model::automation::SCHEDULE_ALL_DAYS_MASK
            } else {
                days
            });
            t!(
                "schedule.during",
                days = days,
                start = clock_text(start_hour, start_minute),
                end = clock_text(end_hour, end_minute),
                count = minutes
            )
            .into_owned()
        }
    }
}

/// 整个触发器的摘要：排程、外部触发、事件逐条列出。外部触发不写令牌。
pub(crate) fn trigger_summary(trigger: &AutomationTrigger) -> String {
    let mut parts = trigger
        .schedules
        .iter()
        .map(schedule_summary)
        .collect::<Vec<_>>();
    for webhook in &trigger.webhooks {
        parts.push(if webhook.token().is_none() {
            t!("trigger.webhook_unset").into_owned()
        } else {
            t!("trigger.webhook").into_owned()
        });
    }
    for event in &trigger.events {
        parts.push(t!("trigger.event", topic = event.topic).into_owned());
    }
    if parts.is_empty() {
        t!("trigger.none").into_owned()
    } else {
        parts.join(&*t!("trigger.separator"))
    }
}

fn clock_text(hour: u8, minute: u8) -> String {
    format!("{hour:02}:{minute:02}")
}

/// 星期几的短名，0 是周一。
pub(crate) fn weekday_label(index: usize) -> SharedString {
    match index {
        0 => t!("weekday.mon"),
        1 => t!("weekday.tue"),
        2 => t!("weekday.wed"),
        3 => t!("weekday.thu"),
        4 => t!("weekday.fri"),
        5 => t!("weekday.sat"),
        _ => t!("weekday.sun"),
    }
    .into()
}

fn weekly_days_summary(days: u8) -> String {
    let days = days & api::model::automation::SCHEDULE_ALL_DAYS_MASK;
    if days == api::model::automation::SCHEDULE_ALL_DAYS_MASK {
        return t!("schedule.every_day").into_owned();
    }
    let labels = (0..7)
        .filter(|index| days & (1 << index) != 0)
        .map(|index| weekday_label(index).to_string())
        .collect::<Vec<_>>();
    if labels.is_empty() {
        t!("schedule.no_days").into_owned()
    } else {
        t!(
            "schedule.weekly_days",
            days = labels.join(&*t!("list.separator"))
        )
        .into_owned()
    }
}

pub(crate) fn agent_display_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        t!("agent.unnamed").into_owned()
    } else {
        name.to_string()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutomationRowStatus {
    Unavailable,
    AgentMissing,
    Running,
    Active,
    Paused,
}

pub(crate) fn automation_row_status(
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

pub(crate) fn automation_row_status_label(status: AutomationRowStatus) -> SharedString {
    match status {
        AutomationRowStatus::Unavailable => t!("row_status.unavailable"),
        AutomationRowStatus::AgentMissing => t!("row_status.agent_missing"),
        AutomationRowStatus::Running => t!("run_status.running"),
        AutomationRowStatus::Active => t!("row_status.active"),
        AutomationRowStatus::Paused => t!("row_status.paused"),
    }
    .into()
}

/// 「启用」不用 Play：同一个菜单里「运行一次」已经是 Play。
pub(crate) fn automation_enabled_toggle_item(enabled: bool) -> MenuItem {
    if enabled {
        MenuItem::new(t!("menu.pause")).icon(IconName::Pause)
    } else {
        MenuItem::new(t!("menu.enable")).icon(IconName::CircleCheck)
    }
}
