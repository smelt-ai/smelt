//! 编辑器和目录上的文案、解析。从宿主原来的 `agents/tests.rs` 搬来。
//!
//! 不用 `crate::prelude::*`：那里带进 `gpui::*` 里的 `test` 属性宏，会把这里的 `#[test]`
//! 换成 gpui 的版本。

use smelt_plugin_api::model::automation::{
    AutomationAction, AutomationNotify, AutomationRunStatus, AutomationSchedule, AutomationTrigger,
    SCHEDULE_DAY_FRI, SCHEDULE_DAY_MON, SCHEDULE_DAY_WED, SCHEDULE_WEEKDAYS_MASK, WebhookIngress,
};
use smelt_plugin_api::ui::field::InputEvent;

use crate::kinds::{
    AutomationActionKindId, AutomationTriggerKindId, AutomationTriggerPresetId,
    BUILTIN_AUTOMATION_ACTIONS, BUILTIN_AUTOMATION_TRIGGER_PRESETS, NOTIFY_CHOICES, notify_hint,
    notify_label,
};

use crate::editor::{
    IntervalUnit, ScheduleKind, automation_prompt_hint, clock_input_is_editable,
    identity_name_should_finish_edit, interval_title, schedule_from_values, webhook_curl_example,
};
use crate::format::{
    AutomationRowStatus, MissingField, agent_display_name, automation_missing_fields,
    automation_row_status, automation_row_status_label, automation_save_hint, format_last_run_cell,
    format_next_run_cell, format_relative_since, format_relative_until, schedule_summary,
    trigger_summary,
};

#[test]
fn clock_editor_accepts_partial_valid_values_without_accepting_impossible_times() {
    for value in ["", "0", "09", "09:", "09:3", "09:35", "9:05"] {
        assert!(clock_input_is_editable(value), "{value}");
    }
    for value in [":", "24", "09:60", "009:00", "09::30", "noon"] {
        assert!(!clock_input_is_editable(value), "{value}");
    }
}

#[test]
fn webhook_curl_example_is_a_single_copyable_command() {
    let curl = webhook_curl_example("http://127.0.0.1:17827/hooks/tok");
    assert!(curl.starts_with("curl -sS -X POST http://127.0.0.1:17827/hooks/tok "));
    assert!(curl.contains(r#"{"text":"Hello"}"#));
    assert!(!curl.contains('\n'));
}

#[test]
fn automation_copy_uses_precise_product_terms() {
    assert_eq!(
        automation_prompt_hint(false),
        "Sent to the agent on every run. Set standing instructions in the agent's settings."
    );
    assert_eq!(
        automation_prompt_hint(true),
        "The request body is appended to the instructions; to place it elsewhere, use {{payload}} or a field variable (such as {{text}}). Leave empty to send the request body as is. Set standing instructions in the agent's settings."
    );
    assert_eq!(AutomationTriggerPresetId::Daily.label(), "Daily");
    assert_eq!(AutomationTriggerPresetId::Webhook.label(), "Webhook");
    assert_eq!(notify_label(AutomationNotify::Always), "Every Run");
    assert_eq!(notify_label(AutomationNotify::Never), "No Notification");
}

#[test]
fn schedule_summaries_read_naturally() {
    assert_eq!(
        schedule_summary(&AutomationSchedule::Weekly {
            days: SCHEDULE_WEEKDAYS_MASK,
            hour: 9,
            minute: 35,
        }),
        "Every Mon, Tue, Wed, Thu, Fri · 09:35"
    );
    assert_eq!(
        schedule_summary(&AutomationSchedule::Daily { hour: 9, minute: 0 }),
        "Daily 09:00"
    );
    assert_eq!(
        schedule_summary(&AutomationSchedule::EveryMinutes { minutes: 15 }),
        "Every 15 min"
    );
    assert_eq!(
        schedule_summary(&AutomationSchedule::EveryHours { hours: 1 }),
        "Every 1 h"
    );
    assert_eq!(
        schedule_summary(&AutomationSchedule::During {
            minutes: 15,
            days: SCHEDULE_WEEKDAYS_MASK,
            start_hour: 9,
            start_minute: 30,
            end_hour: 15,
            end_minute: 0,
        }),
        "Every Mon, Tue, Wed, Thu, Fri 09:30–15:00 · every 15 min"
    );
    assert_eq!(
        schedule_summary(&AutomationSchedule::Weekly {
            days: SCHEDULE_DAY_MON | SCHEDULE_DAY_WED | SCHEDULE_DAY_FRI,
            hour: 9,
            minute: 35,
        }),
        "Every Mon, Wed, Fri · 09:35"
    );
}

/// 触发的摘要逐条列出，外部触发不带令牌。
#[test]
fn trigger_summary_lists_every_trigger_without_the_token() {
    assert_eq!(
        trigger_summary(&AutomationTrigger::webhook_with_secret(
            "tok_should_not_leak"
        )),
        "External Trigger"
    );
    let both =
        AutomationTrigger::schedule(AutomationSchedule::daily_morning()).with_webhook_secret("tok");
    assert_eq!(trigger_summary(&both), "Daily 09:00; External Trigger");
    let two_hooks = AutomationTrigger {
        webhooks: vec![
            WebhookIngress {
                endpoint: "a".into(),
                secret: Some("a".into()),
            },
            WebhookIngress {
                endpoint: "b".into(),
                secret: Some("b".into()),
            },
        ],
        ..AutomationTrigger::default()
    };
    assert_eq!(
        trigger_summary(&two_hooks),
        "External Trigger; External Trigger"
    );
    assert_eq!(
        trigger_summary(&AutomationTrigger::default()),
        "No trigger set up"
    );
}

#[test]
fn interval_title_names_the_actual_interval() {
    assert_eq!(interval_title(IntervalUnit::Hours, "1"), "Hourly");
    assert_eq!(interval_title(IntervalUnit::Hours, " 6 "), "Every 6 h");
    assert_eq!(interval_title(IntervalUnit::Minutes, "15"), "Every 15 min");
    assert_eq!(interval_title(IntervalUnit::Hours, ""), "Interval");
    assert_eq!(interval_title(IntervalUnit::Minutes, "  "), "Interval");
}

#[test]
fn automation_form_lists_every_missing_field() {
    let agent = |prompt: Option<&str>| AutomationAction::Agent {
        agent_definition_id: "agent-1".into(),
        prompt: prompt.map(str::to_string),
    };
    assert_eq!(
        automation_missing_fields("", false, false, &agent(None), true),
        vec![MissingField::Name, MissingField::Trigger]
    );
    assert_eq!(
        automation_missing_fields("晨报", true, true, &agent(Some("  ")), true),
        vec![MissingField::Prompt]
    );
    assert!(
        automation_missing_fields("Hook", true, false, &agent(None), true).is_empty(),
        "webhook / 事件把收到的内容当指令，可以不写"
    );
    assert_eq!(
        automation_missing_fields("晨报", true, true, &agent(Some("汇总")), false),
        vec![MissingField::Agent]
    );
    assert_eq!(
        automation_missing_fields(
            "巡检",
            true,
            true,
            &AutomationAction::shell("", Vec::new()),
            false
        ),
        vec![MissingField::Command]
    );
}

#[test]
fn save_hint_names_what_is_missing_and_how_saving_works() {
    assert_eq!(automation_save_hint(&[], true), None);
    assert_eq!(
        automation_save_hint(&[MissingField::Name, MissingField::Trigger], true).as_deref(),
        Some("Still needed: Name, Trigger")
    );
    assert_eq!(
        automation_save_hint(&[MissingField::Prompt], false).as_deref(),
        Some("Still needed: Instructions. Saves automatically once complete")
    );
}

fn parse_schedule(
    kind: ScheduleKind,
    time: &str,
    interval: &str,
    unit: IntervalUnit,
    days: u8,
) -> Result<AutomationSchedule, String> {
    schedule_from_values(kind, time, interval, unit, days, "", "")
}

#[test]
fn event_only_and_blank_triggers_do_not_invent_a_clock() {
    let event_only = AutomationTrigger::event("task.completed", None);
    assert!(event_only.schedules.is_empty());
    assert!(AutomationTrigger::default().schedules.is_empty());
}

#[test]
fn schedule_editor_rejects_invalid_clock_and_interval_instead_of_defaulting() {
    let minutes = IntervalUnit::Minutes;
    assert_eq!(
        parse_schedule(ScheduleKind::Daily, "23:59", "", minutes, 0),
        Ok(AutomationSchedule::Daily {
            hour: 23,
            minute: 59,
        })
    );
    assert!(parse_schedule(ScheduleKind::Daily, "24:00", "", minutes, 0).is_err());
    assert_eq!(
        parse_schedule(ScheduleKind::Interval, "", "15", minutes, 0),
        Ok(AutomationSchedule::EveryMinutes { minutes: 15 })
    );
    assert_eq!(
        parse_schedule(ScheduleKind::Interval, "", "2", IntervalUnit::Hours, 0),
        Ok(AutomationSchedule::EveryHours { hours: 2 })
    );
    assert!(parse_schedule(ScheduleKind::Interval, "", "0", minutes, 0).is_err());
    let weekdays_at_935 = Ok(AutomationSchedule::Weekly {
        days: SCHEDULE_WEEKDAYS_MASK,
        hour: 9,
        minute: 35,
    });
    assert_eq!(
        parse_schedule(ScheduleKind::Weekdays, "09:35", "", minutes, 0),
        weekdays_at_935
    );
    assert_eq!(
        parse_schedule(
            ScheduleKind::Weekly,
            "09:35",
            "",
            minutes,
            SCHEDULE_WEEKDAYS_MASK
        ),
        weekdays_at_935
    );
    assert_eq!(
        parse_schedule(
            ScheduleKind::Weekly,
            "09:35",
            "",
            minutes,
            SCHEDULE_DAY_MON | SCHEDULE_DAY_FRI
        ),
        Ok(AutomationSchedule::Weekly {
            days: SCHEDULE_DAY_MON | SCHEDULE_DAY_FRI,
            hour: 9,
            minute: 35,
        })
    );
    assert!(parse_schedule(ScheduleKind::Weekly, "09:35", "", minutes, 0).is_err());
}

#[test]
fn interval_with_weekday_window_maps_to_during() {
    let window = |start: &str, end: &str| {
        schedule_from_values(
            ScheduleKind::Interval,
            "",
            "15",
            IntervalUnit::Minutes,
            SCHEDULE_WEEKDAYS_MASK,
            start,
            end,
        )
    };
    assert_eq!(
        window("09:30", "11:30"),
        Ok(AutomationSchedule::During {
            minutes: 15,
            days: SCHEDULE_WEEKDAYS_MASK,
            start_hour: 9,
            start_minute: 30,
            end_hour: 11,
            end_minute: 30,
        })
    );
    assert_eq!(
        window("13:00", "15:00"),
        Ok(AutomationSchedule::During {
            minutes: 15,
            days: SCHEDULE_WEEKDAYS_MASK,
            start_hour: 13,
            start_minute: 0,
            end_hour: 15,
            end_minute: 0,
        })
    );
    assert!(window("09:30", "").is_err());
    assert_eq!(
        window("11:30", "09:30").unwrap_err(),
        "The end time must be later than the start time"
    );
}

#[test]
fn unnamed_agent_falls_back_to_placeholder() {
    assert_eq!(agent_display_name(""), "Untitled");
    assert_eq!(agent_display_name("   "), "Untitled");
    assert_eq!(agent_display_name(" 量化交易 "), "量化交易");
}

#[test]
fn automation_name_finishes_editing_on_blur_or_enter() {
    assert!(identity_name_should_finish_edit(&InputEvent::Blur));
    assert!(identity_name_should_finish_edit(&InputEvent::PressEnter {
        secondary: false,
        shift: false,
    }));
    assert!(!identity_name_should_finish_edit(&InputEvent::Change));
    assert!(!identity_name_should_finish_edit(&InputEvent::Focus));
}

#[test]
fn automation_row_status_prefers_faults_then_running_then_enabled() {
    assert_eq!(
        automation_row_status(true, true, true, true),
        AutomationRowStatus::Unavailable
    );
    assert_eq!(
        automation_row_status(true, false, true, true),
        AutomationRowStatus::AgentMissing
    );
    assert_eq!(
        automation_row_status(true, false, false, true),
        AutomationRowStatus::Running
    );
    assert_eq!(
        automation_row_status(true, false, false, false),
        AutomationRowStatus::Active
    );
    assert_eq!(
        automation_row_status(false, false, false, false),
        AutomationRowStatus::Paused
    );
    assert_eq!(
        automation_row_status_label(AutomationRowStatus::Active),
        "Enabled"
    );
    assert_eq!(
        automation_row_status_label(AutomationRowStatus::Paused),
        "Paused"
    );
}

#[test]
fn next_run_cell_uses_relative_time_and_dash_when_paused() {
    assert_eq!(format_relative_until(100, 100), "Running soon");
    assert_eq!(format_relative_until(100, 160), "Running soon");
    assert_eq!(format_relative_until(100 + 3 * 60, 100), "in 3 min");
    assert_eq!(format_relative_until(100 + 15 * 3600, 100), "in 15 h");
    assert_eq!(format_relative_until(100 + 2 * 86400, 100), "in 2 d");
    assert_eq!(format_next_run_cell(Some(100 + 3600), true, 100), "in 1 h");
    assert_eq!(format_next_run_cell(Some(100 + 3600), false, 100), "—");
    assert_eq!(format_next_run_cell(None, true, 100), "—");
}

#[test]
fn last_run_cell_reads_status_then_relative_time() {
    assert_eq!(format_relative_since(100, 100), "Just now");
    assert_eq!(format_relative_since(100, 130), "Just now");
    assert_eq!(format_relative_since(100, 100 + 3 * 60), "3 min ago");
    assert_eq!(format_relative_since(100, 100 + 5 * 3600), "5 h ago");
    assert_eq!(format_relative_since(100, 100 + 2 * 86400), "2 d ago");
    assert_eq!(format_relative_since(100, 100 + 40 * 86400), "1 mo ago");
    assert_eq!(format_relative_since(100, 100 + 400 * 86400), "1 yr ago");
    // 远程机器的时钟可能比本机快，不写成负数。
    assert_eq!(format_relative_since(100 + 3600, 100), "Just now");
    assert_eq!(format_last_run_cell(None, 100), "—");
    assert_eq!(
        format_last_run_cell(Some((AutomationRunStatus::Failed, 100)), 100 + 3600),
        "Failed · 1 h ago"
    );
}

#[test]
fn builtin_action_catalog_covers_agent_and_shell() {
    assert_eq!(
        BUILTIN_AUTOMATION_ACTIONS,
        &[AutomationActionKindId::Agent, AutomationActionKindId::Shell,]
    );
    assert_eq!(AutomationActionKindId::Agent.label(), "Agent");
    assert_eq!(AutomationActionKindId::Shell.label(), "Shell");
}

#[test]
fn trigger_presets_cover_hourly_daily_weekly_interval_and_webhook() {
    assert_eq!(
        BUILTIN_AUTOMATION_TRIGGER_PRESETS,
        &[
            AutomationTriggerPresetId::Hourly,
            AutomationTriggerPresetId::Daily,
            AutomationTriggerPresetId::Weekly,
            AutomationTriggerPresetId::Interval,
            AutomationTriggerPresetId::Webhook,
        ]
    );
    assert_eq!(AutomationTriggerPresetId::Daily.label(), "Daily");
    assert_eq!(
        AutomationTriggerPresetId::Webhook.hint(),
        "When this webhook receives a request"
    );
    assert!(AutomationTriggerPresetId::Webhook.is_advanced());
    assert!(!AutomationTriggerPresetId::Daily.is_advanced());
    assert_eq!(
        AutomationTriggerPresetId::Hourly.schedule(),
        Some(AutomationSchedule::EveryHours { hours: 1 })
    );
    assert_eq!(
        AutomationTriggerPresetId::Webhook.trigger_kind(),
        AutomationTriggerKindId::Webhook
    );
}

#[test]
fn notification_choices_run_from_quiet_to_every_run() {
    assert_eq!(
        NOTIFY_CHOICES,
        [
            AutomationNotify::Never,
            AutomationNotify::FailuresOnly,
            AutomationNotify::Always
        ]
    );
    for notify in NOTIFY_CHOICES {
        assert_eq!(AutomationNotify::from_sinks(&notify.to_sinks()), notify);
        assert!(!notify_label(notify).is_empty());
        assert!(!notify_hint(notify).is_empty());
    }
    assert_eq!(
        notify_label(AutomationNotify::FailuresOnly),
        "Failures Only"
    );
}
