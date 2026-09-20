use chrono::{TimeZone, Timelike, Utc};
use chrono_tz::America::New_York;
use smelt_core::automation::{
    Automation, AutomationAction, AutomationCommand, AutomationFile, AutomationInboundEvent,
    AutomationRun, AutomationRunContext, AutomationRunSource, AutomationRunStatus,
    AutomationSchedule, AutomationTrigger, apply_automation_command, claim_due_automations,
    event_run_id, publish_automation_event,
};

fn automation(id: &str) -> Automation {
    Automation {
        id: id.to_string(),
        name: "Morning report".to_string(),
        enabled: true,
        workspace_dir: Some("/tmp".to_string()),
        trigger: AutomationTrigger::schedule(AutomationSchedule::EveryMinutes { minutes: 30 }),
        action: smelt_core::automation::AutomationAction::Agent {
            agent_definition_id: "agent-1".to_string(),
            prompt: Some("Summarize the project".to_string()),
        },
        sinks: Vec::new(),
    }
}

fn event_automation(id: &str, topic: &str, filter: Option<&str>) -> Automation {
    Automation {
        id: id.to_string(),
        name: id.to_string(),
        enabled: true,
        workspace_dir: Some("/tmp".to_string()),
        trigger: AutomationTrigger::event(topic, filter.map(str::to_string)),
        action: smelt_core::automation::AutomationAction::Agent {
            agent_definition_id: "agent-1".to_string(),
            prompt: None,
        },
        sinks: Vec::new(),
    }
}

fn inbound(topic: &str, event_id: &str, payload: serde_json::Value) -> AutomationInboundEvent {
    AutomationInboundEvent {
        topic: topic.to_string(),
        event_id: event_id.to_string(),
        payload: Some(payload),
    }
}

#[test]
fn shell_automation_does_not_require_an_agent() {
    let automation = Automation {
        id: "shell-1".into(),
        name: "Health check".into(),
        enabled: true,
        workspace_dir: Some("/tmp".into()),
        trigger: AutomationTrigger::schedule(AutomationSchedule::EveryMinutes { minutes: 5 }),
        action: smelt_core::automation::AutomationAction::shell("true", Vec::new()),
        sinks: Vec::new(),
    };

    assert!(automation.validate().is_ok());
    assert!(automation.agent_definition_id().is_none());
    assert_eq!(
        automation.action.shell_invocation(),
        Some(("bash".into(), vec!["-lc".into(), "true".into()]))
    );
    assert_eq!(
        smelt_core::automation::AutomationAction::shell("echo", vec!["hi".into()])
            .shell_invocation(),
        Some(("echo".into(), vec!["hi".into()]))
    );
}

#[test]
fn agent_action_serializes_an_unambiguous_definition_id() {
    let value = serde_json::to_value(AutomationAction::Agent {
        agent_definition_id: "agent-1".into(),
        prompt: None,
    })
    .unwrap();

    assert_eq!(value["agent_definition_id"], "agent-1");
    assert!(value.get("agent_id").is_none());

    let migrated: AutomationAction = serde_json::from_value(serde_json::json!({
        "type": "agent",
        "agent_id": "legacy-agent"
    }))
    .unwrap();
    assert_eq!(migrated.agent_definition_id(), Some("legacy-agent"));
}

#[test]
fn run_context_rejects_conflicting_agent_definition_ids() {
    let error = serde_json::from_value::<AutomationRunContext>(serde_json::json!({
        "automation_name": "Morning report",
        "action": {
            "type": "agent",
            "agent_id": "action-agent"
        },
        "cwd": "/tmp",
        "agent_id": "context-agent"
    }))
    .unwrap_err();

    assert!(error.to_string().contains("智能体定义 ID 不一致"));
}

#[test]
fn legacy_cwd_field_loads_as_the_managed_workspace_slot() {
    let mut value = serde_json::to_value(automation("automation-1")).unwrap();
    let object = value.as_object_mut().unwrap();
    let workspace = object.remove("workspace_dir").unwrap();
    object.insert("cwd".to_string(), workspace);

    let loaded: Automation = serde_json::from_value(value).unwrap();

    assert_eq!(loaded.workspace_dir.as_deref(), Some("/tmp"));
    let stored = serde_json::to_value(loaded).unwrap();
    assert_eq!(stored["workspace_dir"], "/tmp");
    assert!(stored.get("cwd").is_none());
}

#[test]
fn legacy_flat_automation_loads_as_trigger_and_action() {
    let loaded: Automation = serde_json::from_value(serde_json::json!({
        "id": "1713b9f0-c7a1-497e-9975-1ba15be02ae4",
        "agent_id": "968bf075-e968-4799-812d-e42bbc67b750",
        "name": "sss",
        "enabled": true,
        "prompt": "dddd",
        "workspace_dir": "/tmp",
        "permission_mode": "full_access",
        "kind": "schedule",
        "schedule": {"type": "every_minutes", "minutes": 15}
    }))
    .unwrap();

    assert_eq!(loaded.name, "sss");
    assert_eq!(
        loaded.trigger,
        AutomationTrigger::schedule(AutomationSchedule::EveryMinutes { minutes: 15 })
    );
    assert_eq!(
        loaded.agent_definition_id(),
        Some("968bf075-e968-4799-812d-e42bbc67b750")
    );
    assert_eq!(loaded.prompt(), Some("dddd"));

    let stored = serde_json::to_value(&loaded).unwrap();
    assert!(stored.get("kind").is_none());
    assert!(stored.get("agent_id").is_none());
    assert!(stored["trigger"].get("kind").is_none());
    assert_eq!(stored["trigger"]["schedules"].as_array().unwrap().len(), 1);
    assert_eq!(stored["action"]["type"], "agent");
    assert!(stored.get("permission_mode").is_none());
    assert!(stored["action"].get("permission_mode").is_none());
}

#[test]
fn legacy_run_context_without_action_is_reconstructed() {
    let file: AutomationFile = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "store_id": "store-1",
        "revision": 3,
        "automations": [{
            "id": "automation-1",
            "agent_id": "agent-1",
            "name": "sss",
            "enabled": true,
            "prompt": "dddd",
            "cwd": "/tmp",
            "permission_mode": "full_access",
            "kind": "schedule",
            "schedule": {"type": "every_minutes", "minutes": 15}
        }],
        "runs": [{
            "id": "run-1",
            "automation_id": "automation-1",
            "context": {
                "automation_name": "sss",
                "cwd": "/tmp",
                "permission_mode": "full_access",
                "prompt": "dddd",
                "agent_id": "agent-1",
                "agent_name": "量化交易",
                "agent_kind": "pi",
                "agent_instructions": "交易"
            },
            "source": "scheduled",
            "status": "completed",
            "created_at": 1,
            "finished_at": 2,
            "runtime_released_at": 2
        }]
    }))
    .unwrap();

    file.validate().unwrap();
    assert_eq!(
        file.runs[0].context.action,
        smelt_core::automation::AutomationAction::Agent {
            agent_definition_id: "agent-1".into(),
            prompt: Some("dddd".into()),
        }
    );
    let stored = serde_json::to_value(file).unwrap();
    assert!(stored["runs"][0]["context"].get("agent_id").is_none());
    assert_eq!(
        stored["runs"][0]["context"]["agent_definition_name"],
        "量化交易"
    );
    assert_eq!(stored["runs"][0]["context"]["engine_kind_id"], "pi");
    assert!(
        stored["runs"][0]["context"]
            .get("permission_mode")
            .is_none()
    );
    assert!(
        stored["runs"][0]["context"]["action"]
            .get("permission_mode")
            .is_none()
    );
}

#[test]
fn manual_run_preserves_the_scheduled_cursor_and_obeys_overlap() {
    let now = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
    let mut file = AutomationFile::default();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(automation("automation-1")),
        },
        now,
    )
    .unwrap();
    let next_before = file
        .state_for("automation-1")
        .and_then(|state| state.next_run_at)
        .unwrap();

    let run = apply_automation_command(
        &mut file,
        AutomationCommand::RunOnce {
            automation_id: "automation-1".to_string(),
        },
        now + chrono::Duration::minutes(5),
    )
    .unwrap()
    .run()
    .unwrap();

    assert_eq!(run.source, AutomationRunSource::Manual);
    assert_eq!(run.status, AutomationRunStatus::Starting);
    assert_eq!(
        file.state_for("automation-1")
            .and_then(|state| state.next_run_at),
        Some(next_before)
    );

    file.runs[0].status = AutomationRunStatus::AwaitingApproval;
    let overlap = apply_automation_command(
        &mut file,
        AutomationCommand::RunOnce {
            automation_id: "automation-1".to_string(),
        },
        now + chrono::Duration::minutes(6),
    );
    assert!(overlap.is_err());
}

#[test]
fn paused_automation_remains_manually_runnable() {
    let now = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
    let mut file = AutomationFile::default();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(automation("automation-1")),
        },
        now,
    )
    .unwrap();
    apply_automation_command(
        &mut file,
        AutomationCommand::SetEnabled {
            automation_id: "automation-1".into(),
            enabled: false,
        },
        now,
    )
    .unwrap();

    let run = apply_automation_command(
        &mut file,
        AutomationCommand::RunOnce {
            automation_id: "automation-1".into(),
        },
        now,
    )
    .unwrap()
    .run()
    .unwrap();

    assert_eq!(run.status, AutomationRunStatus::Starting);
    assert_eq!(run.source, AutomationRunSource::Manual);
    assert_eq!(file.state_for("automation-1").unwrap().next_run_at, None);
}

#[test]
fn claimed_run_context_is_immutable_when_the_automation_changes() {
    let now = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
    let mut file = AutomationFile::default();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(automation("automation-1")),
        },
        now,
    )
    .unwrap();
    let run_id = apply_automation_command(
        &mut file,
        AutomationCommand::RunOnce {
            automation_id: "automation-1".into(),
        },
        now,
    )
    .unwrap()
    .run()
    .unwrap()
    .id;

    let mut edited = automation("automation-1");
    edited.name = "Changed automation".into();
    edited.action = smelt_core::automation::AutomationAction::Agent {
        agent_definition_id: "agent-1".into(),
        prompt: Some("Do something different".into()),
    };
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(edited),
        },
        now + chrono::Duration::minutes(1),
    )
    .unwrap();

    let claimed = file.runs.iter().find(|run| run.id == run_id).unwrap();
    assert_eq!(claimed.context.automation_name, "Morning report");
    assert_eq!(
        claimed.context.prompt.as_deref(),
        Some("Summarize the project")
    );
}

#[test]
fn cancellation_targets_the_claimed_run_without_moving_the_schedule() {
    let now = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
    let mut file = AutomationFile::default();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(automation("automation-1")),
        },
        now,
    )
    .unwrap();
    let next_run_at = file.state_for("automation-1").unwrap().next_run_at;
    let run = apply_automation_command(
        &mut file,
        AutomationCommand::RunOnce {
            automation_id: "automation-1".into(),
        },
        now,
    )
    .unwrap()
    .run()
    .unwrap();

    apply_automation_command(
        &mut file,
        AutomationCommand::CancelRun { run_id: run.id },
        now + chrono::Duration::seconds(1),
    )
    .unwrap();

    assert_eq!(file.runs[0].status, AutomationRunStatus::Cancelled);
    assert!(file.runs[0].runtime_released_at.is_some());
    assert_eq!(
        file.state_for("automation-1").unwrap().next_run_at,
        next_run_at
    );
}

#[test]
fn cancelling_a_shell_run_keeps_runtime_until_released() {
    let now = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
    let mut file = AutomationFile::default();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(Automation {
                id: "shell-1".into(),
                name: "Health check".into(),
                enabled: true,
                workspace_dir: Some("/tmp".into()),
                trigger: AutomationTrigger::schedule(AutomationSchedule::EveryMinutes {
                    minutes: 5,
                }),
                action: AutomationAction::shell("sleep 1", Vec::new()),
                sinks: Vec::new(),
            }),
        },
        now,
    )
    .unwrap();
    let run = apply_automation_command(
        &mut file,
        AutomationCommand::RunOnce {
            automation_id: "shell-1".into(),
        },
        now,
    )
    .unwrap()
    .run()
    .unwrap();

    apply_automation_command(
        &mut file,
        AutomationCommand::CancelRun { run_id: run.id },
        now + chrono::Duration::seconds(1),
    )
    .unwrap();

    assert_eq!(file.runs[0].status, AutomationRunStatus::Cancelled);
    assert!(file.runs[0].runtime_released_at.is_none());
    assert!(file.active_run_for("shell-1").is_some());
    assert!(
        apply_automation_command(
            &mut file,
            AutomationCommand::RunOnce {
                automation_id: "shell-1".into(),
            },
            now + chrono::Duration::seconds(2),
        )
        .is_err()
    );
}

#[test]
fn terminal_run_blocks_overlap_until_its_runtime_is_released() {
    let now = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
    let mut file = AutomationFile::default();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(automation("automation-1")),
        },
        now,
    )
    .unwrap();
    apply_automation_command(
        &mut file,
        AutomationCommand::RunOnce {
            automation_id: "automation-1".into(),
        },
        now,
    )
    .unwrap();
    file.runs[0].status = AutomationRunStatus::Completed;
    file.runs[0].finished_at = Some(now.timestamp());
    file.runs[0].session_id = Some("session-1".into());

    assert!(
        apply_automation_command(
            &mut file,
            AutomationCommand::RunOnce {
                automation_id: "automation-1".into(),
            },
            now,
        )
        .is_err()
    );

    file.runs[0].runtime_released_at = Some(now.timestamp());
    assert!(
        apply_automation_command(
            &mut file,
            AutomationCommand::RunOnce {
                automation_id: "automation-1".into(),
            },
            now,
        )
        .is_ok()
    );

    let history = file.runs_for("automation-1");
    assert_eq!(
        history.len(),
        2,
        "完成的 Run 必须留在历史上，不能只保留最近一次"
    );
    assert_ne!(history[0].id, history[1].id);
    assert_eq!(
        history[0].id,
        file.latest_run_for("automation-1").unwrap().id
    );
}

#[test]
fn unreleased_runs_cannot_share_one_runtime_session() {
    let now = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
    let mut file = AutomationFile::default();
    for id in ["automation-1", "automation-2"] {
        apply_automation_command(
            &mut file,
            AutomationCommand::Upsert {
                automation: Box::new(automation(id)),
            },
            now,
        )
        .unwrap();
        apply_automation_command(
            &mut file,
            AutomationCommand::RunOnce {
                automation_id: id.into(),
            },
            now,
        )
        .unwrap();
    }
    for run in &mut file.runs {
        run.context.agent_definition_name = Some("Reporter".into());
        run.context.engine_kind_id = Some("pi".into());
        run.context.agent_instructions = Some("Report carefully".into());
        run.session_id = Some("shared-session".into());
    }

    assert!(
        file.validate()
            .unwrap_err()
            .contains("多个未释放 AutomationRun 共用 ACP 会话")
    );
}

#[test]
fn scheduler_claims_at_most_one_catch_up_occurrence() {
    let now = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
    let mut file = AutomationFile::default();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(automation("automation-1")),
        },
        now,
    )
    .unwrap();
    file.state_for_mut("automation-1").unwrap().next_run_at = Some(now.timestamp() - 3_600);

    let claimed = claim_due_automations(&mut file, now);
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].source, AutomationRunSource::Scheduled);
    assert!(
        file.state_for("automation-1")
            .and_then(|state| state.next_run_at)
            .unwrap()
            > now.timestamp()
    );
    assert!(claim_due_automations(&mut file, now).is_empty());
}

#[test]
fn multiple_schedules_use_the_earliest_next_slot() {
    let now = Utc.with_ymd_and_hms(2026, 9, 4, 8, 0, 0).unwrap();
    let trigger = AutomationTrigger::schedules(vec![
        AutomationSchedule::Daily { hour: 9, minute: 0 },
        AutomationSchedule::EveryHours { hours: 2 },
    ]);
    assert_eq!(trigger.summary(), "每天 09:00；每隔 2 小时");
    assert_eq!(
        trigger.next_after(now).unwrap(),
        Utc.with_ymd_and_hms(2026, 9, 4, 9, 0, 0).unwrap()
    );
    let later = Utc.with_ymd_and_hms(2026, 9, 4, 9, 30, 0).unwrap();
    assert_eq!(
        trigger.next_after(later).unwrap(),
        Utc.with_ymd_and_hms(2026, 9, 4, 11, 30, 0).unwrap()
    );
}

#[test]
fn legacy_single_schedule_field_loads_as_a_one_rule_list() {
    let loaded: AutomationTrigger = serde_json::from_value(serde_json::json!({
        "kind": "schedule",
        "schedule": {"type": "daily", "hour": 9, "minute": 0}
    }))
    .unwrap();
    assert_eq!(
        loaded.schedule_values(),
        &[AutomationSchedule::Daily { hour: 9, minute: 0 }]
    );
    let stored = serde_json::to_value(&loaded).unwrap();
    assert!(stored.get("kind").is_none());
    assert!(stored.get("schedule").is_none());
    assert_eq!(stored["schedules"].as_array().unwrap().len(), 1);
}

#[test]
fn during_window_aligns_to_session_hours_on_weekdays() {
    use smelt_core::automation::SCHEDULE_WEEKDAYS_MASK;
    let cst = chrono::FixedOffset::east_opt(8 * 3600).expect("CST");
    let schedule = AutomationSchedule::During {
        minutes: 15,
        days: SCHEDULE_WEEKDAYS_MASK,
        start_hour: 9,
        start_minute: 30,
        end_hour: 15,
        end_minute: 0,
    };
    let friday_before = cst.with_ymd_and_hms(2026, 9, 4, 9, 20, 0).single().unwrap();
    assert_eq!(
        schedule
            .next_after(friday_before)
            .format("%H:%M")
            .to_string(),
        "09:30"
    );
    let friday_inside = cst.with_ymd_and_hms(2026, 9, 4, 9, 31, 0).single().unwrap();
    assert_eq!(
        schedule
            .next_after(friday_inside)
            .format("%H:%M")
            .to_string(),
        "09:45"
    );
    let friday_close = cst
        .with_ymd_and_hms(2026, 9, 4, 14, 59, 0)
        .single()
        .unwrap();
    assert_eq!(
        schedule
            .next_after(friday_close)
            .format("%F %H:%M")
            .to_string(),
        "2026-09-04 15:00"
    );
    let friday_after = cst.with_ymd_and_hms(2026, 9, 4, 15, 0, 1).single().unwrap();
    assert_eq!(
        schedule
            .next_after(friday_after)
            .format("%F %H:%M")
            .to_string(),
        "2026-09-07 09:30"
    );
    let saturday = cst.with_ymd_and_hms(2026, 9, 5, 10, 0, 0).single().unwrap();
    assert_eq!(
        schedule.next_after(saturday).format("%F %H:%M").to_string(),
        "2026-09-07 09:30"
    );
    assert_eq!(
        schedule.summary(),
        "每周一、二、三、四、五 09:30–15:00 · 每隔 15 分钟"
    );
}

#[test]
fn split_session_windows_skip_the_midday_break() {
    use smelt_core::automation::SCHEDULE_WEEKDAYS_MASK;
    let cst = chrono::FixedOffset::east_opt(8 * 3600).expect("CST");
    let trigger = AutomationTrigger::schedules(vec![
        AutomationSchedule::During {
            minutes: 15,
            days: SCHEDULE_WEEKDAYS_MASK,
            start_hour: 9,
            start_minute: 30,
            end_hour: 11,
            end_minute: 30,
        },
        AutomationSchedule::During {
            minutes: 15,
            days: SCHEDULE_WEEKDAYS_MASK,
            start_hour: 13,
            start_minute: 0,
            end_hour: 15,
            end_minute: 0,
        },
    ]);
    let lunch = cst
        .with_ymd_and_hms(2026, 9, 4, 11, 31, 0)
        .single()
        .unwrap();
    assert_eq!(
        trigger
            .next_after(lunch)
            .unwrap()
            .format("%F %H:%M")
            .to_string(),
        "2026-09-04 13:00"
    );
    let after_close = cst.with_ymd_and_hms(2026, 9, 4, 15, 0, 1).single().unwrap();
    assert_eq!(
        trigger
            .next_after(after_close)
            .unwrap()
            .format("%F %H:%M")
            .to_string(),
        "2026-09-07 09:30"
    );
}

#[test]
fn weekly_and_hourly_schedules_compute_the_next_slot() {
    use smelt_core::automation::{SCHEDULE_DAY_FRI, SCHEDULE_DAY_MON};
    let now = Utc.with_ymd_and_hms(2026, 9, 4, 10, 0, 0).unwrap();
    let weekly = AutomationSchedule::Weekly {
        days: SCHEDULE_DAY_MON | SCHEDULE_DAY_FRI,
        hour: 9,
        minute: 0,
    };
    assert_eq!(
        weekly.next_after(now).date_naive().to_string(),
        "2026-09-07"
    );
    assert_eq!(
        AutomationSchedule::EveryHours { hours: 3 }
            .next_after(now)
            .hour(),
        13
    );
}

#[test]
fn daily_wall_clock_time_survives_a_dst_offset_change() {
    let before_transition = New_York.with_ymd_and_hms(2026, 3, 7, 10, 0, 0).unwrap();
    let schedule = AutomationSchedule::Daily {
        hour: 9,
        minute: 35,
    };

    let next = schedule.next_after(before_transition);

    assert_eq!(next.date_naive().to_string(), "2026-03-08");
    assert_eq!(next.time().format("%H:%M").to_string(), "09:35");
    assert_eq!(next.offset().to_string(), "EDT");
}

#[test]
fn inbound_event_fans_out_by_topic_not_automation_id() {
    let now = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
    let mut file = AutomationFile::default();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(event_automation(
                "lark-mentions",
                "lark.message.received",
                Some(r#"{"mention":true}"#),
            )),
        },
        now,
    )
    .unwrap();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(event_automation("lark-all", "lark.message.received", None)),
        },
        now,
    )
    .unwrap();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(event_automation("git-push", "git.push.received", None)),
        },
        now,
    )
    .unwrap();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(automation("scheduled")),
        },
        now,
    )
    .unwrap();

    let published = publish_automation_event(
        &mut file,
        &inbound(
            "lark.message.received",
            "om_1",
            serde_json::json!({
                "text": "帮我看这段报错",
                "mention": true,
                "chat_id": "oc_123"
            }),
        ),
        now,
    )
    .unwrap();

    assert_eq!(published.topic, "lark.message.received");
    assert_eq!(published.event_id, "om_1");
    assert_eq!(published.already_recorded, 0);
    let mut ids = published
        .runs
        .iter()
        .map(|run| run.automation_id.as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    assert_eq!(ids, vec!["lark-all", "lark-mentions"]);
    assert!(published.runs.iter().all(|run| {
        run.source == AutomationRunSource::Event
            && run.status == AutomationRunStatus::Starting
            && run.context.prompt.as_deref() == Some("帮我看这段报错")
    }));
    assert!(!file.runs.iter().any(|run| run.automation_id == "git-push"
        || run.automation_id == "scheduled"
        || run.source == AutomationRunSource::Scheduled));
}

#[test]
fn inbound_event_filter_and_idempotency_are_generic() {
    let now = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
    let mut file = AutomationFile::default();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(event_automation(
                "mentions",
                "lark.message.received",
                Some(r#"{"mention":true}"#),
            )),
        },
        now,
    )
    .unwrap();

    let missed = publish_automation_event(
        &mut file,
        &inbound(
            "lark.message.received",
            "om_plain",
            serde_json::json!({ "text": "hi", "mention": false }),
        ),
        now,
    )
    .unwrap();
    assert!(missed.runs.is_empty());

    let first = publish_automation_event(
        &mut file,
        &inbound(
            "lark.message.received",
            "om_mention",
            serde_json::json!({ "text": "@bot 看看", "mention": true }),
        ),
        now,
    )
    .unwrap();
    assert_eq!(first.runs.len(), 1);
    assert_eq!(first.runs[0].id, event_run_id("mentions", "om_mention"));

    let again = publish_automation_event(
        &mut file,
        &inbound(
            "lark.message.received",
            "om_mention",
            serde_json::json!({ "text": "@bot 看看", "mention": true }),
        ),
        now,
    )
    .unwrap();
    assert!(again.runs.is_empty());
    assert_eq!(again.already_recorded, 1);
    assert_eq!(
        file.runs
            .iter()
            .filter(|run| run.automation_id == "mentions")
            .count(),
        1
    );
}

#[test]
fn inbound_event_skips_busy_automation_without_blocking_others() {
    let now = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
    let mut file = AutomationFile::default();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(event_automation("busy", "src.changed", None)),
        },
        now,
    )
    .unwrap();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(event_automation("idle", "src.changed", None)),
        },
        now,
    )
    .unwrap();
    let first = publish_automation_event(
        &mut file,
        &inbound("src.changed", "evt-0", serde_json::json!({"path": "a.rs"})),
        now,
    )
    .unwrap();
    assert_eq!(first.runs.len(), 2);
    for run in &mut file.runs {
        if run.automation_id == "busy" {
            run.status = AutomationRunStatus::Running;
        } else {
            run.status = AutomationRunStatus::Completed;
            run.finished_at = Some(now.timestamp());
            run.runtime_released_at = Some(now.timestamp());
        }
    }

    let published = publish_automation_event(
        &mut file,
        &inbound("src.changed", "evt-1", serde_json::json!({"path": "a.rs"})),
        now,
    )
    .unwrap();
    let busy = published
        .runs
        .iter()
        .find(|run| run.automation_id == "busy")
        .unwrap();
    let idle = published
        .runs
        .iter()
        .find(|run| run.automation_id == "idle")
        .unwrap();
    assert_eq!(busy.status, AutomationRunStatus::Skipped);
    assert_eq!(idle.status, AutomationRunStatus::Starting);
}

#[test]
fn webhook_token_identifies_the_automation() {
    let now = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
    let mut file = AutomationFile::default();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(Automation {
                id: "hook-1".into(),
                name: "Hook".into(),
                enabled: true,
                workspace_dir: Some("/tmp".into()),
                trigger: AutomationTrigger::webhook_with_secret("tok_abc"),
                action: smelt_core::automation::AutomationAction::Agent {
                    agent_definition_id: "agent-1".into(),
                    prompt: None,
                },
                sinks: Vec::new(),
            }),
        },
        now,
    )
    .unwrap();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(automation("scheduled")),
        },
        now,
    )
    .unwrap();

    assert_eq!(
        file.webhook_automation_for("tok_abc")
            .map(|automation| automation.id.as_str()),
        Some("hook-1")
    );
    assert!(file.webhook_automation_for("missing").is_none());

    let run = apply_automation_command(
        &mut file,
        AutomationCommand::Trigger {
            automation_id: "hook-1".into(),
            source: AutomationRunSource::Webhook,
            payload: Some(serde_json::json!({"text": "你好"})),
        },
        now,
    )
    .unwrap()
    .run()
    .unwrap();
    assert_eq!(run.source, AutomationRunSource::Webhook);
    assert_eq!(run.context.prompt.as_deref(), Some("你好"));
}

#[test]
fn paused_webhook_is_found_but_not_runnable() {
    let now = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
    let mut file = AutomationFile::default();
    let mut hook = Automation {
        id: "hook-1".into(),
        name: "Hook".into(),
        enabled: false,
        workspace_dir: Some("/tmp".into()),
        trigger: AutomationTrigger::webhook_with_secret("tok_abc"),
        action: smelt_core::automation::AutomationAction::shell("true", Vec::new()),
        sinks: Vec::new(),
    };
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(hook.clone()),
        },
        now,
    )
    .unwrap();
    assert_eq!(
        file.webhook_automation_for("tok_abc")
            .map(|automation| automation.enabled),
        Some(false)
    );

    hook.enabled = true;
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(hook),
        },
        now,
    )
    .unwrap();
    assert_eq!(
        file.webhook_automation_for("tok_abc")
            .map(|automation| automation.enabled),
        Some(true)
    );
}

#[test]
fn event_topic_rejects_unqualified_names() {
    assert!(AutomationTrigger::event("lark", None).validate().is_err());
    assert!(
        AutomationTrigger::event("Lark.Message", None)
            .validate()
            .is_ok()
    );
    assert!(
        AutomationTrigger::event("lark.message.received", Some("[1]".into()))
            .validate()
            .is_err()
    );
}

#[test]
fn webhook_validation_uses_product_copy() {
    let trigger = AutomationTrigger::webhook("", None);
    assert_eq!(trigger.validate().unwrap_err(), "外部触发缺少接收令牌");
    assert!(!trigger.validate().unwrap_err().contains("Webhook"));
}

#[test]
fn empty_trigger_uses_trigger_condition_copy() {
    let trigger = AutomationTrigger::default();
    assert_eq!(trigger.kind_label(), "未配置触发条件");
    assert_eq!(trigger.summary(), "未配置触发条件");
    assert_eq!(trigger.validate().unwrap_err(), "请至少添加一条触发条件");

    let mixed = AutomationTrigger::schedule(AutomationSchedule::daily_morning())
        .with_webhook_secret("tok_mixed");
    assert_eq!(mixed.kind_label(), "多种触发条件");
}

#[test]
fn schedule_and_webhook_can_share_one_automation() {
    let now = Utc.with_ymd_and_hms(2026, 9, 4, 8, 0, 0).unwrap();
    let trigger = AutomationTrigger::schedule(AutomationSchedule::Daily {
        hour: 9,
        minute: 30,
    })
    .with_webhook_secret("tok_both");
    assert!(trigger.validate().is_ok());
    assert_eq!(trigger.summary(), "每天 09:30；外部触发");
    assert_eq!(
        trigger.next_after(now).unwrap().format("%H:%M").to_string(),
        "09:30"
    );

    let mut file = AutomationFile::default();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(Automation {
                id: "both".into(),
                name: "Both".into(),
                enabled: true,
                workspace_dir: Some("/tmp".into()),
                trigger: trigger.clone(),
                action: smelt_core::automation::AutomationAction::shell("true", Vec::new()),
                sinks: Vec::new(),
            }),
        },
        now,
    )
    .unwrap();
    assert_eq!(
        file.webhook_automation_for("tok_both")
            .map(|automation| automation.id.as_str()),
        Some("both")
    );

    let stored = serde_json::to_value(&trigger).unwrap();
    assert!(stored.get("kind").is_none());
    assert!(stored.get("webhook").is_none());
    assert_eq!(stored["schedules"].as_array().unwrap().len(), 1);
    assert_eq!(stored["webhooks"][0]["secret"], "tok_both");
    let loaded: AutomationTrigger = serde_json::from_value(stored).unwrap();
    assert_eq!(loaded, trigger);

    let legacy: AutomationTrigger = serde_json::from_value(serde_json::json!({
        "kind": "webhook",
        "secret": "legacy_tok"
    }))
    .unwrap();
    assert_eq!(legacy.webhook_token(), Some("legacy_tok"));
    assert!(legacy.schedule_values().is_empty());
}

#[test]
fn one_automation_can_have_two_webhook_addresses() {
    let trigger = AutomationTrigger {
        webhooks: vec![
            smelt_core::automation::WebhookIngress {
                endpoint: "tok_a".into(),
                secret: Some("tok_a".into()),
            },
            smelt_core::automation::WebhookIngress {
                endpoint: "tok_b".into(),
                secret: Some("tok_b".into()),
            },
        ],
        ..AutomationTrigger::default()
    };
    assert!(trigger.validate().is_ok());
    assert_eq!(trigger.summary(), "外部触发；外部触发");
    assert!(trigger.matches_webhook_token("tok_a"));
    assert!(trigger.matches_webhook_token("tok_b"));
    assert!(!trigger.matches_webhook_token("tok_c"));

    let now = Utc.with_ymd_and_hms(2026, 9, 4, 8, 0, 0).unwrap();
    let mut file = AutomationFile::default();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(Automation {
                id: "hooks".into(),
                name: "Hooks".into(),
                enabled: true,
                workspace_dir: Some("/tmp".into()),
                trigger,
                action: smelt_core::automation::AutomationAction::shell("true", Vec::new()),
                sinks: Vec::new(),
            }),
        },
        now,
    )
    .unwrap();
    assert_eq!(
        file.webhook_automation_for("tok_b")
            .map(|automation| automation.id.as_str()),
        Some("hooks")
    );
}

#[test]
fn legacy_permission_mode_is_ignored_and_not_serialized() {
    let action: smelt_core::automation::AutomationAction =
        serde_json::from_value(serde_json::json!({
            "type": "agent",
            "agent_id": "agent-1",
            "permission_mode": "ask"
        }))
        .unwrap();

    assert_eq!(action.agent_definition_id(), Some("agent-1"));
    let stored = serde_json::to_value(action).unwrap();
    assert!(
        stored.get("permission_mode").is_none(),
        "automation permission is fixed full-access and must not remain a configurable wire field"
    );
}

#[test]
fn live_projection_keeps_catalog_fields_and_drops_run_bodies() {
    let mut file = AutomationFile {
        schema_version: 1,
        store_id: "store".into(),
        revision: 9,
        ..Default::default()
    };
    file.runs.push(AutomationRun {
        id: "run-1".into(),
        automation_id: "auto-1".into(),
        context: AutomationRunContext {
            automation_name: "Report".into(),
            action: AutomationAction::Agent {
                agent_definition_id: "agent-1".into(),
                prompt: Some("Summarize".into()),
            },
            cwd: "/tmp".into(),
            trigger_payload: Some(serde_json::json!({"body": "x".repeat(2048)})),
            prompt: Some("Summarize".into()),
            agent_definition_name: Some("Agent".into()),
            engine_kind_id: Some("pi".into()),
            agent_instructions: Some("Be useful".into()),
        },
        source: AutomationRunSource::Manual,
        scheduled_for: None,
        status: AutomationRunStatus::Completed,
        created_at: 1,
        started_at: Some(1),
        delivery_attempt_at: Some(1),
        delivery_attempts: 1,
        finished_at: Some(2),
        session_id: Some("acp-1".into()),
        provider_session_id: None,
        output: Some("x".repeat(16 * 1024)),
        error: Some("failed".into()),
        runtime_released_at: Some(2),
    });

    let live = file.live_projection();
    assert_eq!(live.revision, 9);
    assert_eq!(live.runs.len(), 1);
    assert_eq!(live.runs[0].status, AutomationRunStatus::Completed);
    assert_eq!(live.runs[0].context.prompt.as_deref(), Some("Summarize"));
    assert_eq!(live.runs[0].error.as_deref(), Some("failed"));
    assert!(live.runs[0].output.is_none());
    assert!(live.runs[0].context.trigger_payload.is_none());
    assert!(live.runs[0].context.agent_instructions.is_none());
    assert_eq!(
        file.runs[0].output.as_ref().map(String::len),
        Some(16 * 1024)
    );
}
