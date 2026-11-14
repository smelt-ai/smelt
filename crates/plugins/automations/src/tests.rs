//! 编辑器和目录上的文案、解析。从宿主原来的 `agents/tests.rs` 搬来。
//!
//! 不用 `crate::prelude::*`：那里带进 `gpui::*` 里的 `test` 属性宏，会把这里的 `#[test]`
//! 换成 gpui 的版本。

use gpui_component::input::InputEvent;
use smelt_plugin_api::model::automation::{
    AutomationAction, AutomationRunStatus, AutomationSchedule, AutomationSink, AutomationTrigger,
    SCHEDULE_DAY_FRI, SCHEDULE_DAY_MON, SCHEDULE_WEEKDAYS_MASK,
};

use crate::kinds::{
    AutomationActionKindId, AutomationNotificationPreset, AutomationTriggerKindId,
    AutomationTriggerPresetId, BUILTIN_AUTOMATION_ACTIONS, BUILTIN_AUTOMATION_TRIGGER_PRESETS,
};

use crate::editor::{
    IntervalUnit, ScheduleKind, automation_prompt_hint, clock_input_is_editable,
    identity_name_should_finish_edit, interval_title, schedule_from_values, webhook_curl_example,
};
use crate::format::{
    AutomationRowStatus, agent_display_name, automation_missing_fields, automation_row_status,
    automation_row_status_label, automation_save_hint, format_last_run_cell, format_next_run_cell,
    format_relative_since, format_relative_until,
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
    assert!(curl.contains(r#"{"text":"你好"}"#));
    assert!(!curl.contains('\n'));
}

#[test]
fn automation_copy_uses_precise_product_terms() {
    assert_eq!(
        automation_prompt_hint(false),
        "每次运行时发送给智能体。长期指令请在智能体设置中配置。"
    );
    assert_eq!(
        automation_prompt_hint(true),
        "收到的请求内容会附在指令后面；想放到指定位置，用 {{payload}} 或字段变量（如 {{text}}）。留空时直接发送请求内容。长期指令请在智能体设置中配置。"
    );
    assert_eq!(AutomationTriggerPresetId::Daily.label(), "每天");
    assert_eq!(AutomationTriggerPresetId::Webhook.label(), "Webhook");
    assert_eq!(AutomationNotificationPreset::App.label(), "应用内通知");
    assert_eq!(AutomationNotificationPreset::Off.label(), "不通知");
}

#[test]
fn interval_title_names_the_actual_interval() {
    assert_eq!(interval_title(IntervalUnit::Hours, "1"), "每小时");
    assert_eq!(interval_title(IntervalUnit::Hours, " 6 "), "每隔 6 小时");
    assert_eq!(interval_title(IntervalUnit::Minutes, "15"), "每隔 15 分钟");
    assert_eq!(interval_title(IntervalUnit::Hours, ""), "间隔");
    assert_eq!(interval_title(IntervalUnit::Minutes, "  "), "间隔");
}

#[test]
fn automation_form_lists_every_missing_field() {
    let agent = |prompt: Option<&str>| AutomationAction::Agent {
        agent_definition_id: "agent-1".into(),
        prompt: prompt.map(str::to_string),
    };
    assert_eq!(
        automation_missing_fields("", false, false, &agent(None), true),
        vec!["名称", "触发器"]
    );
    assert_eq!(
        automation_missing_fields("晨报", true, true, &agent(Some("  ")), true),
        vec!["指令"]
    );
    assert!(
        automation_missing_fields("Hook", true, false, &agent(None), true).is_empty(),
        "webhook / 事件把收到的内容当指令，可以不写"
    );
    assert_eq!(
        automation_missing_fields("晨报", true, true, &agent(Some("汇总")), false),
        vec!["智能体"]
    );
    assert_eq!(
        automation_missing_fields(
            "巡检",
            true,
            true,
            &AutomationAction::shell("", Vec::new()),
            false
        ),
        vec!["命令"]
    );
}

#[test]
fn save_hint_names_what_is_missing_and_how_saving_works() {
    assert_eq!(automation_save_hint(&[], true), None);
    assert_eq!(
        automation_save_hint(&["名称", "触发器"], true).as_deref(),
        Some("还差：名称、触发器")
    );
    assert_eq!(
        automation_save_hint(&["指令"], false).as_deref(),
        Some("还差：指令，补齐后自动保存")
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
        "结束时间需晚于开始时间"
    );
}

#[test]
fn unnamed_agent_falls_back_to_placeholder() {
    assert_eq!(agent_display_name(""), "未命名");
    assert_eq!(agent_display_name("   "), "未命名");
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
        "已启用"
    );
    assert_eq!(
        automation_row_status_label(AutomationRowStatus::Paused),
        "已暂停"
    );
}

#[test]
fn next_run_cell_uses_relative_time_and_dash_when_paused() {
    assert_eq!(format_relative_until(100, 100), "即将运行");
    assert_eq!(format_relative_until(100, 160), "即将运行");
    assert_eq!(format_relative_until(100 + 3 * 60, 100), "3 分钟后");
    assert_eq!(format_relative_until(100 + 15 * 3600, 100), "15 小时后");
    assert_eq!(format_relative_until(100 + 2 * 86400, 100), "2 天后");
    assert_eq!(
        format_next_run_cell(Some(100 + 3600), true, 100),
        "1 小时后"
    );
    assert_eq!(format_next_run_cell(Some(100 + 3600), false, 100), "—");
    assert_eq!(format_next_run_cell(None, true, 100), "—");
}

#[test]
fn last_run_cell_reads_status_then_relative_time() {
    assert_eq!(format_relative_since(100, 100), "刚刚");
    assert_eq!(format_relative_since(100, 130), "刚刚");
    assert_eq!(format_relative_since(100, 100 + 3 * 60), "3 分钟前");
    assert_eq!(format_relative_since(100, 100 + 5 * 3600), "5 小时前");
    assert_eq!(format_relative_since(100, 100 + 2 * 86400), "2 天前");
    assert_eq!(format_relative_since(100, 100 + 40 * 86400), "1 个月前");
    assert_eq!(format_relative_since(100, 100 + 400 * 86400), "1 年前");
    // 远程机器的时钟可能比本机快，不写成负数。
    assert_eq!(format_relative_since(100 + 3600, 100), "刚刚");
    assert_eq!(format_last_run_cell(None, 100), "—");
    assert_eq!(
        format_last_run_cell(Some((AutomationRunStatus::Failed, 100)), 100 + 3600),
        "失败 · 1 小时前"
    );
}

#[test]
fn builtin_action_catalog_covers_agent_and_shell() {
    assert_eq!(
        BUILTIN_AUTOMATION_ACTIONS,
        &[AutomationActionKindId::Agent, AutomationActionKindId::Shell,]
    );
    assert_eq!(AutomationActionKindId::Agent.label(), "智能体");
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
    assert_eq!(AutomationTriggerPresetId::Daily.label(), "每天");
    assert_eq!(
        AutomationTriggerPresetId::Webhook.hint(),
        "当此 webhook 收到请求时"
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
fn notification_preset_only_offers_targets_that_deliver() {
    assert_eq!(
        AutomationNotificationPreset::ALL,
        [
            AutomationNotificationPreset::App,
            AutomationNotificationPreset::Off
        ]
    );
    assert_eq!(
        AutomationNotificationPreset::from_sinks(&[]),
        AutomationNotificationPreset::App
    );
    assert_eq!(
        AutomationNotificationPreset::from_sinks(&[AutomationSink::Local]),
        AutomationNotificationPreset::App
    );
    assert_eq!(
        AutomationNotificationPreset::from_sinks(&[AutomationSink::Off]),
        AutomationNotificationPreset::Off
    );
    for preset in AutomationNotificationPreset::ALL {
        assert_eq!(
            AutomationNotificationPreset::from_sinks(&preset.to_sinks()),
            preset
        );
    }
    assert_eq!(AutomationNotificationPreset::App.label(), "应用内通知");
    assert_eq!(AutomationNotificationPreset::Off.label(), "不通知");
}
