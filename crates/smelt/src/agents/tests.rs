use super::{
    agent_catalog_monogram, bound_agent_definition_id, conversation_agent_mark,
    conversation_icon_color, conversation_row_title, new_conversation_row_label,
    recent_conversation_order,
};
use crate::AgentStatus;
use gpui::rgb;

#[test]
fn conversation_belongs_to_the_bound_agent_not_the_cwd() {
    assert_eq!(bound_agent_definition_id(Some("a"), None), Some("a"));
    assert_eq!(bound_agent_definition_id(Some("  a  "), None), Some("a"));
    assert_eq!(bound_agent_definition_id(Some(""), None), None);
    assert_eq!(bound_agent_definition_id(None, None), None);
    assert_eq!(
        bound_agent_definition_id(Some("a"), Some("auto-1")),
        None,
        "自动化 Run 不是对话"
    );
}

#[test]
fn recent_conversations_put_the_newest_first() {
    assert_eq!(recent_conversation_order(&[10, 30, 20]), vec![1, 2, 0]);
    assert_eq!(
        recent_conversation_order(&[5, 5]),
        vec![0, 1],
        "同一秒保持原来的顺序"
    );
}

#[test]
fn conversation_mark_uses_the_first_character_or_pi() {
    assert_eq!(conversation_agent_mark(Some("写作助手")), "写");
    assert_eq!(conversation_agent_mark(Some("  研究助理")), "研");
    assert_eq!(conversation_agent_mark(None), "Pi");
    assert_eq!(conversation_agent_mark(Some("  ")), "Pi");
}

#[test]
fn catalog_monogram_keeps_two_characters_when_they_identify_the_agent() {
    assert_eq!(agent_catalog_monogram("编码助手"), "编码");
    assert_eq!(agent_catalog_monogram("日志与磁盘清理智能体"), "日志");
    assert_eq!(agent_catalog_monogram("Pi 智能体 3"), "Pi");
    assert_eq!(agent_catalog_monogram("A"), "A");
    assert_eq!(agent_catalog_monogram("   "), "智");
}

#[test]
fn new_conversation_row_names_the_default_and_stays_plain_without_one() {
    assert_eq!(new_conversation_row_label(None), "新对话");
    assert_eq!(new_conversation_row_label(Some("  ")), "新对话");
    assert_eq!(
        new_conversation_row_label(Some("写作助手")),
        "新对话 · 写作助手"
    );
}

#[test]
fn conversation_row_title_uses_title_source_not_agent_name_equality() {
    assert_eq!(
        conversation_row_title(Some("量化交易助手"), Some("自动标题")),
        "量化交易助手",
        "用户把会话命名成智能体名时不能被自动标题覆盖"
    );
    assert_eq!(
        conversation_row_title(None, Some("季度复盘")),
        "季度复盘",
        "智能体改名不能影响已经生成的对话标题"
    );
    assert_eq!(conversation_row_title(None, None), "新对话");
}

#[test]
fn selected_idle_conversation_icon_does_not_look_like_running() {
    // 选中由行底色表达。图标如果因为选中而走 accent 蓝，就会和运行中撞色。
    let idle = conversation_icon_color(AgentStatus::Idle, None);
    let running = conversation_icon_color(AgentStatus::Running, None);
    assert_eq!(
        idle,
        crate::Workspace::session_icon_color(AgentStatus::Idle, None),
        "空闲图标必须仍是空闲色，不能因为选中改成蓝"
    );
    assert_ne!(idle, running, "空闲不能和运行中撞色");
    assert_ne!(
        idle,
        rgb(crate::ui_theme::accent()),
        "空闲不能走 accent 蓝，否则选中行看起来像运行中"
    );
}

#[cfg(test)]
mod schedule_editor_tests {
    use super::super::{
        IntervalUnit, ScheduleKind, automation_missing_fields, automation_prompt_hint,
        automation_save_hint, clock_input_is_editable, interval_title, schedule_from_values,
        webhook_curl_example,
    };
    use crate::settings;
    use smelt_core::automation::{SCHEDULE_DAY_FRI, SCHEDULE_DAY_MON, SCHEDULE_WEEKDAYS_MASK};

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
            settings::AutomationTriggerKindId::Calendar.event_title(),
            "定时执行"
        );
        assert_eq!(
            settings::AutomationTriggerKindId::Interval.event_title(),
            "间隔执行"
        );
        assert_eq!(
            settings::AutomationTriggerKindId::Webhook.event_title(),
            "外部触发"
        );
        assert_eq!(
            automation_prompt_hint(false),
            "每次运行时发送给智能体。长期指令请在智能体设置中配置。"
        );
        assert_eq!(
            automation_prompt_hint(true),
            "收到的请求内容会附在指令后面；想放到指定位置，用 {{payload}} 或字段变量（如 {{text}}）。留空时直接发送请求内容。长期指令请在智能体设置中配置。"
        );
        assert_eq!(settings::AutomationTriggerPresetId::Daily.label(), "每天");
        assert_eq!(
            settings::AutomationTriggerPresetId::Webhook.label(),
            "Webhook"
        );
        assert_eq!(
            settings::AutomationNotificationPreset::App.label(),
            "应用内通知"
        );
        assert_eq!(
            settings::AutomationNotificationPreset::Off.label(),
            "不通知"
        );
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
        let agent = |prompt: Option<&str>| settings::AutomationAction::Agent {
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
                &settings::AutomationAction::shell("", Vec::new()),
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
    ) -> Result<settings::AutomationSchedule, String> {
        schedule_from_values(kind, time, interval, unit, days, "", "")
    }

    #[test]
    fn event_only_and_blank_triggers_do_not_invent_a_clock() {
        let event_only = settings::AutomationTrigger::event("task.completed", None);
        assert!(event_only.schedules.is_empty());
        assert!(settings::AutomationTrigger::default().schedules.is_empty());
    }

    #[test]
    fn schedule_editor_rejects_invalid_clock_and_interval_instead_of_defaulting() {
        assert_eq!(
            parse_schedule(ScheduleKind::Daily, "23:59", "", IntervalUnit::Minutes, 0),
            Ok(settings::AutomationSchedule::Daily {
                hour: 23,
                minute: 59,
            })
        );
        assert!(
            parse_schedule(ScheduleKind::Daily, "24:00", "", IntervalUnit::Minutes, 0).is_err()
        );
        assert_eq!(
            parse_schedule(ScheduleKind::Interval, "", "15", IntervalUnit::Minutes, 0),
            Ok(settings::AutomationSchedule::EveryMinutes { minutes: 15 })
        );
        assert_eq!(
            parse_schedule(ScheduleKind::Interval, "", "2", IntervalUnit::Hours, 0),
            Ok(settings::AutomationSchedule::EveryHours { hours: 2 })
        );
        assert!(parse_schedule(ScheduleKind::Interval, "", "0", IntervalUnit::Minutes, 0).is_err());
        assert_eq!(
            parse_schedule(
                ScheduleKind::Weekdays,
                "09:35",
                "",
                IntervalUnit::Minutes,
                0
            ),
            Ok(settings::AutomationSchedule::Weekly {
                days: SCHEDULE_WEEKDAYS_MASK,
                hour: 9,
                minute: 35,
            })
        );
        assert_eq!(
            parse_schedule(
                ScheduleKind::Weekly,
                "09:35",
                "",
                IntervalUnit::Minutes,
                SCHEDULE_WEEKDAYS_MASK
            ),
            Ok(settings::AutomationSchedule::Weekly {
                days: SCHEDULE_WEEKDAYS_MASK,
                hour: 9,
                minute: 35,
            })
        );
        assert_eq!(
            parse_schedule(
                ScheduleKind::Weekly,
                "09:35",
                "",
                IntervalUnit::Minutes,
                SCHEDULE_DAY_MON | SCHEDULE_DAY_FRI
            ),
            Ok(settings::AutomationSchedule::Weekly {
                days: SCHEDULE_DAY_MON | SCHEDULE_DAY_FRI,
                hour: 9,
                minute: 35,
            })
        );
        assert!(
            parse_schedule(ScheduleKind::Weekly, "09:35", "", IntervalUnit::Minutes, 0).is_err()
        );
    }

    #[test]
    fn interval_with_weekday_window_maps_to_during() {
        assert_eq!(
            schedule_from_values(
                ScheduleKind::Interval,
                "",
                "15",
                IntervalUnit::Minutes,
                SCHEDULE_WEEKDAYS_MASK,
                "09:30",
                "11:30",
            ),
            Ok(settings::AutomationSchedule::During {
                minutes: 15,
                days: SCHEDULE_WEEKDAYS_MASK,
                start_hour: 9,
                start_minute: 30,
                end_hour: 11,
                end_minute: 30,
            })
        );
        assert_eq!(
            schedule_from_values(
                ScheduleKind::Interval,
                "",
                "15",
                IntervalUnit::Minutes,
                SCHEDULE_WEEKDAYS_MASK,
                "13:00",
                "15:00",
            ),
            Ok(settings::AutomationSchedule::During {
                minutes: 15,
                days: SCHEDULE_WEEKDAYS_MASK,
                start_hour: 13,
                start_minute: 0,
                end_hour: 15,
                end_minute: 0,
            })
        );
        assert!(
            schedule_from_values(
                ScheduleKind::Interval,
                "",
                "15",
                IntervalUnit::Minutes,
                SCHEDULE_WEEKDAYS_MASK,
                "09:30",
                "",
            )
            .is_err()
        );
        assert_eq!(
            schedule_from_values(
                ScheduleKind::Interval,
                "",
                "15",
                IntervalUnit::Minutes,
                SCHEDULE_WEEKDAYS_MASK,
                "11:30",
                "09:30",
            )
            .unwrap_err(),
            "结束时间需晚于开始时间"
        );
    }
}

#[cfg(test)]
mod agent_definition_ui_tests {
    use super::super::{
        AutomationRowStatus, agent_card_prompt, agent_conversation_start_error, agent_display_name,
        agent_instructions_footer, agent_usage_summary, automation_row_status,
        automation_row_status_label, format_last_run_cell, format_next_run_cell,
        format_relative_since, format_relative_until, identity_name_idle_text,
        identity_name_should_finish_edit,
    };
    use crate::settings::{AgentDefinition, ConversationAgentKind};
    use gpui_component::input::InputEvent;
    use smelt_core::automation::AutomationRunStatus;

    fn definition(name: &str, prompt: &str, engine_kind_id: &str) -> AgentDefinition {
        AgentDefinition {
            id: "quant".into(),
            name: name.into(),
            description: String::new(),
            engine_kind_id: engine_kind_id.into(),
            prompt: prompt.into(),
            plugins: Vec::new(),
            context_folders: Vec::new(),
            context_links: Vec::new(),
            ..Default::default()
        }
    }

    #[test]
    fn unnamed_agent_falls_back_to_placeholder() {
        assert_eq!(agent_display_name(""), "未命名");
        assert_eq!(agent_display_name("   "), "未命名");
        assert_eq!(agent_display_name(" 量化交易 "), "量化交易");
    }

    #[test]
    fn identity_name_stays_a_title_until_clicked() {
        assert_eq!(
            identity_name_idle_text("", "给它起个名字"),
            ("给它起个名字", true)
        );
        assert_eq!(
            identity_name_idle_text("   ", "给它起个名字"),
            ("给它起个名字", true)
        );
        assert_eq!(
            identity_name_idle_text("Pi 智能体", "给它起个名字"),
            ("Pi 智能体", false)
        );
        assert!(identity_name_should_finish_edit(&InputEvent::Blur));
        assert!(identity_name_should_finish_edit(&InputEvent::PressEnter {
            secondary: false,
            shift: false,
        }));
        assert!(!identity_name_should_finish_edit(&InputEvent::Change));
        assert!(!identity_name_should_finish_edit(&InputEvent::Focus));
    }

    #[test]
    fn conversation_start_does_not_require_name_or_instructions() {
        assert!(definition("Pi 智能体", "", ConversationAgentKind::Pi.id()).is_ready());
        assert_eq!(
            agent_conversation_start_error(&definition("", "", ConversationAgentKind::Pi.id())),
            None
        );
        assert_eq!(
            agent_conversation_start_error(&definition("量化交易", "规则", "unknown")),
            Some("请选择一个可用的执行引擎")
        );
        assert_eq!(
            agent_conversation_start_error(&definition(
                "量化交易",
                "规则",
                ConversationAgentKind::Pi.id()
            )),
            None
        );
    }

    #[test]
    fn card_prompt_stays_within_two_lines_and_skips_blank() {
        assert_eq!(agent_card_prompt("   \n  "), None);
        assert_eq!(
            agent_card_prompt("先验证行情时间，再给出结论。"),
            Some("先验证行情时间，再给出结论。".into())
        );
        let long = "你是一个专业的本地系统与磁盘清理智能体。职责：检查并安全清理本地超过 7 天的历史日志、AI 助手会话轨迹与临时缓存，然后再做别的。";
        let card = agent_card_prompt(long).unwrap();
        assert!(card.ends_with('…'));
        assert!(card.chars().count() <= 65);
    }

    #[test]
    fn instructions_footer_includes_count_only_when_written() {
        assert_eq!(
            agent_instructions_footer(0),
            "会作为系统提示词带进每次对话和自动化。"
        );
        assert_eq!(
            agent_instructions_footer(48),
            "48 字 · 会作为系统提示词带进每次对话和自动化"
        );
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
        assert_eq!(format_last_run_cell(None, 100), "—");
        assert_eq!(
            format_last_run_cell(Some((AutomationRunStatus::Failed, 100)), 100 + 3600),
            "失败 · 1 小时前"
        );
    }

    #[test]
    fn usage_summary_omits_zero_counts_and_joins_the_rest() {
        assert_eq!(agent_usage_summary(0, 0), "还没有对话");
        assert_eq!(agent_usage_summary(2, 0), "2 段对话");
        assert_eq!(agent_usage_summary(0, 1), "1 条自动化");
        assert_eq!(agent_usage_summary(2, 1), "2 段对话 · 1 条自动化");
    }
}
