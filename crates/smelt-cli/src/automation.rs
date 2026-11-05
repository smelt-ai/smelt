//! `smelt automation …`：自动化的增删改查、手动运行和运行记录。

use crate::{control, fail, flag_value, id_arg, json_id, on_off, respond, usage};
use smelt_model::automation::{
    Automation, AutomationAction, AutomationCommand, AutomationRun, AutomationSchedule,
    AutomationSink, AutomationTrigger, EventIngress, SCHEDULE_ALL_DAYS_MASK, SCHEDULE_DAY_FRI,
    SCHEDULE_DAY_MON, SCHEDULE_DAY_SAT, SCHEDULE_DAY_SUN, SCHEDULE_DAY_THU, SCHEDULE_DAY_TUE,
    SCHEDULE_DAY_WED, SCHEDULE_WEEKDAYS_MASK, redact_automation_credentials, validate_event_filter,
};
use smelt_model::control_api::{
    AgentDefinitionIdParams, AutomationGet, AutomationList, AutomationRunDetail,
    AutomationRunDetailParams,
};
use smelt_runtime_client::protocol::control::EmptyParams;

const TRIGGER_FLAGS: &str =
    "--every-minutes、--every-hours、--daily、--weekly、--during、--webhook 或 --event";

pub(crate) fn run(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("list") => respond(
            control::<AutomationList>(EmptyParams::default())
                .map(|result| serde_json::json!({ "automations": result.automations })),
        ),
        Some("get") => match id_arg(rest) {
            Some(id) => respond(load(id)),
            None => usage("smelt automation get <id>"),
        },
        Some("create") => match parse_flags(rest).and_then(create_from) {
            Ok(automation) => respond(upsert(automation)),
            Err(message) => usage(&message),
        },
        Some("update") => {
            let Some(id) = id_arg(rest) else {
                return usage("smelt automation update <id> [--name ...] [--prompt ...]");
            };
            let flags = match parse_flags(&rest[1..]) {
                Ok(flags) => flags,
                Err(message) => return usage(&message),
            };
            let current = match load(id) {
                Ok(automation) => automation,
                Err(error) => return fail(&error),
            };
            match apply_update(current, flags) {
                Ok(automation) => respond(upsert(automation)),
                Err(message) => usage(&message),
            }
        }
        Some(verb @ ("enable" | "disable")) => match id_arg(rest) {
            Some(id) => respond(submit(AutomationCommand::SetEnabled {
                automation_id: id.to_string(),
                enabled: verb == "enable",
            })),
            None => usage("smelt automation enable|disable <id>"),
        },
        Some("run") => match id_arg(rest) {
            Some(id) => respond(submit(AutomationCommand::RunOnce {
                automation_id: id.to_string(),
            })),
            None => usage("smelt automation run <id>"),
        },
        Some("cancel") => match id_arg(rest) {
            Some(run_id) => respond(submit(AutomationCommand::CancelRun {
                run_id: run_id.to_string(),
            })),
            None => usage("smelt automation cancel <run_id>"),
        },
        Some("delete") => match id_arg(rest) {
            Some(id) => respond(
                submit(AutomationCommand::Delete {
                    automation_id: id.to_string(),
                })
                .map(|_| json_id(id)),
            ),
            None => usage("smelt automation delete <id>"),
        },
        Some("runs") => match parse_runs_args(rest) {
            Ok((automation_id, limit)) => respond(list_runs(automation_id.as_deref(), limit)),
            Err(message) => usage(&message),
        },
        Some("run-log") => match id_arg(rest) {
            Some(run_id) => respond(control::<AutomationRunDetail>(AutomationRunDetailParams {
                run_id: run_id.to_string(),
                include_archived_run: true,
            })),
            None => usage("smelt automation run-log <run_id>"),
        },
        Some(command) => usage(&format!("未知子命令: {command}")),
        None => usage(
            "smelt automation list|get|create|update|enable|disable|run|cancel|delete|runs|run-log",
        ),
    }
}

/// 运行时给的是去掉 webhook 令牌的投影；提交回去时运行时按位置换回真令牌。
fn load(id: &str) -> Result<Automation, String> {
    control::<AutomationGet>(AgentDefinitionIdParams { id: id.to_string() })
        .map(|result| result.automation)
}

fn upsert(automation: Automation) -> Result<serde_json::Value, String> {
    submit(AutomationCommand::Upsert {
        automation: Box::new(automation),
    })
}

fn submit(command: AutomationCommand) -> Result<serde_json::Value, String> {
    let (result, snapshot) = smelt_client::session_ops::submit_automation_command(
        &smelt_runtime_client::local::local_runtime(),
        &command,
    )?;
    let snapshot = redact_automation_credentials(snapshot);
    Ok(serde_json::json!({
        "result": result,
        "automations": snapshot.automations,
    }))
}

fn list_runs(automation_id: Option<&str>, limit: usize) -> Result<serde_json::Value, String> {
    let snapshot = smelt_client::session_ops::automations_snapshot(
        &smelt_runtime_client::local::local_runtime(),
    )?;
    Ok(serde_json::json!({ "runs": recent_runs(snapshot.runs, automation_id, limit) }))
}

/// 新的在前。
fn recent_runs(
    mut runs: Vec<AutomationRun>,
    automation_id: Option<&str>,
    limit: usize,
) -> Vec<AutomationRun> {
    runs.retain(|run| automation_id.is_none_or(|id| run.automation_id == id));
    runs.sort_by_key(|run| std::cmp::Reverse(run.created_at));
    runs.truncate(limit);
    runs
}

fn parse_runs_args(args: &[String]) -> Result<(Option<String>, usize), String> {
    let mut automation_id = None;
    let mut limit = 20;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--limit" => {
                limit = flag_value(args, index)?
                    .parse::<usize>()
                    .ok()
                    .filter(|limit| *limit > 0)
                    .ok_or("--limit 必须是正整数")?;
                index += 2;
            }
            flag if flag.starts_with('-') => return Err(format!("未知参数: {flag}")),
            id if automation_id.is_none() => {
                automation_id = Some(id.to_string());
                index += 1;
            }
            extra => return Err(format!("多余参数: {extra}")),
        }
    }
    Ok((automation_id, limit))
}

#[derive(Default)]
struct Flags {
    /// 写了任何参数。
    touched: bool,
    name: Option<String>,
    agent: Option<String>,
    prompt: Option<String>,
    shell: Option<String>,
    schedules: Vec<AutomationSchedule>,
    events: Vec<EventIngress>,
    webhook: bool,
    no_schedule: bool,
    no_webhook: bool,
    no_event: bool,
    notify: Option<bool>,
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut flags = Flags {
        touched: !args.is_empty(),
        ..Flags::default()
    };
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let switch = match flag {
            "--webhook" => Some(&mut flags.webhook),
            "--no-schedule" => Some(&mut flags.no_schedule),
            "--no-webhook" => Some(&mut flags.no_webhook),
            "--no-event" => Some(&mut flags.no_event),
            _ => None,
        };
        if let Some(switch) = switch {
            *switch = true;
            index += 1;
            continue;
        }
        if !flag.starts_with('-') {
            return Err(format!("多余参数: {flag}"));
        }
        let value = flag_value(args, index)?;
        match flag {
            "--name" => flags.name = Some(value),
            "--agent" => flags.agent = Some(value),
            "--prompt" => flags.prompt = Some(value),
            "--shell" => flags.shell = Some(value),
            "--every-minutes" => flags.schedules.push(AutomationSchedule::EveryMinutes {
                minutes: positive(flag, &value)?,
            }),
            "--every-hours" => flags.schedules.push(AutomationSchedule::EveryHours {
                hours: positive(flag, &value)?,
            }),
            "--daily" => {
                let (hour, minute) = parse_clock(&value)?;
                flags
                    .schedules
                    .push(AutomationSchedule::Daily { hour, minute });
            }
            "--weekly" => flags.schedules.push(parse_weekly(&value)?),
            "--during" => flags.schedules.push(parse_during(&value)?),
            "--event" => {
                let topic = value.trim();
                if topic.is_empty() {
                    return Err("--event 需要主题".into());
                }
                flags.events.push(EventIngress {
                    topic: topic.to_string(),
                    filter: None,
                });
            }
            "--event-filter" => {
                let Some(event) = flags.events.last_mut() else {
                    return Err("--event-filter 要跟在 --event 后面".into());
                };
                event.filter = validate_event_filter(Some(&value))?;
            }
            "--notify" => flags.notify = Some(on_off(flag, &value)?),
            other => return Err(format!("未知参数: {other}")),
        }
        index += 2;
    }
    for schedule in &flags.schedules {
        schedule.validate()?;
    }
    Ok(flags)
}

fn create_from(flags: Flags) -> Result<Automation, String> {
    if flags.no_schedule || flags.no_webhook || flags.no_event {
        return Err("--no-schedule / --no-webhook / --no-event 只用于 update".into());
    }
    let name = flags
        .name
        .filter(|name| !name.trim().is_empty())
        .ok_or("需要 --name")?;
    let action = automation_action(flags.agent, flags.shell, flags.prompt)?;
    let mut trigger = AutomationTrigger {
        schedules: flags.schedules,
        webhooks: Vec::new(),
        events: flags.events,
    };
    if flags.webhook {
        trigger = trigger.with_webhook_secret(new_webhook_secret());
    }
    if is_untriggered(&trigger) {
        return Err(format!("需要至少一种触发条件：{TRIGGER_FLAGS}"));
    }
    Ok(Automation {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        enabled: true,
        workspace_dir: None,
        trigger,
        action,
        sinks: notify_sinks(Vec::new(), flags.notify),
    })
}

/// 只改写了的那几项。触发条件按类别合并：给了定时就整组换掉定时，webhook 和事件不动；
/// `--webhook` 已经有了就保留原来的地址（换令牌等于让对方的调用全部失效）。
fn apply_update(mut automation: Automation, flags: Flags) -> Result<Automation, String> {
    if !flags.touched {
        return Err("smelt automation update <id> 需要至少一个字段".into());
    }
    if let Some(name) = flags.name.filter(|name| !name.trim().is_empty()) {
        automation.name = name;
    }
    if flags.agent.is_some() || flags.shell.is_some() {
        let current_agent = automation.agent_definition_id().map(str::to_string);
        let current_prompt = automation.prompt().map(str::to_string);
        // 换成 Shell 时不沿用原来的智能体。
        let agent = flags
            .agent
            .or(current_agent.filter(|_| flags.shell.is_none()));
        automation.action = automation_action(agent, flags.shell, flags.prompt.or(current_prompt))?;
    } else if let Some(prompt) = flags.prompt {
        match &mut automation.action {
            AutomationAction::Agent { prompt: slot, .. } => *slot = Some(prompt),
            AutomationAction::Shell { .. } => {
                return Err("这条自动化是 Shell 动作，不能改 --prompt".into());
            }
        }
    }

    if flags.no_schedule && !flags.schedules.is_empty() {
        return Err("不能同时给定时和 --no-schedule".into());
    }
    if flags.no_webhook && flags.webhook {
        return Err("不能同时给 --webhook 和 --no-webhook".into());
    }
    if flags.no_event && !flags.events.is_empty() {
        return Err("不能同时给 --event 和 --no-event".into());
    }
    let trigger = &mut automation.trigger;
    if flags.no_schedule || !flags.schedules.is_empty() {
        trigger.schedules = flags.schedules;
    }
    if flags.no_webhook {
        trigger.webhooks.clear();
    } else if flags.webhook && trigger.webhooks.is_empty() {
        *trigger = std::mem::take(trigger).with_webhook_secret(new_webhook_secret());
    }
    if flags.no_event || !flags.events.is_empty() {
        trigger.events = flags.events;
    }
    if is_untriggered(trigger) {
        return Err(format!("改完就没有触发条件了，至少留一种：{TRIGGER_FLAGS}"));
    }

    automation.sinks = notify_sinks(std::mem::take(&mut automation.sinks), flags.notify);
    Ok(automation)
}

fn is_untriggered(trigger: &AutomationTrigger) -> bool {
    trigger.schedules.is_empty() && trigger.webhooks.is_empty() && trigger.events.is_empty()
}

fn new_webhook_secret() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// 空列表沿用应用通知（见 `automation_notifies_app`）。
fn notify_sinks(mut sinks: Vec<AutomationSink>, notify: Option<bool>) -> Vec<AutomationSink> {
    match notify {
        None => sinks,
        Some(false) => vec![AutomationSink::Off],
        Some(true) => {
            sinks.retain(|sink| !sink.is_off());
            if !sinks.iter().any(AutomationSink::is_local) {
                sinks.push(AutomationSink::Local);
            }
            sinks
        }
    }
}

fn automation_action(
    agent: Option<String>,
    shell: Option<String>,
    prompt: Option<String>,
) -> Result<AutomationAction, String> {
    match (agent, shell) {
        (Some(agent_id), None) => Ok(AutomationAction::agent(agent_id, prompt)),
        (None, Some(command)) => Ok(AutomationAction::shell(command, Vec::new())),
        _ => Err("需要 --agent <id> 或 --shell <命令>，不能同时有".into()),
    }
}

fn positive(flag: &str, value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{flag} 必须是正整数"))
}

fn parse_clock(value: &str) -> Result<(u8, u8), String> {
    let invalid = || format!("时间格式为 HH:MM，收到 {value}");
    let (hour, minute) = value.trim().split_once(':').ok_or_else(invalid)?;
    let hour = hour.parse::<u8>().map_err(|_| invalid())?;
    let minute = minute.parse::<u8>().map_err(|_| invalid())?;
    if hour > 23 || minute > 59 {
        return Err("时间必须在 00:00 到 23:59".into());
    }
    Ok((hour, minute))
}

/// `mon,wed,fri@09:30`
fn parse_weekly(value: &str) -> Result<AutomationSchedule, String> {
    let (days, clock) = value
        .split_once('@')
        .ok_or("--weekly 格式为 <星期>@HH:MM，例如 mon,wed,fri@09:30")?;
    let (hour, minute) = parse_clock(clock)?;
    Ok(AutomationSchedule::Weekly {
        days: parse_days(days)?,
        hour,
        minute,
    })
}

/// `30@09:00-18:00` 或 `30@09:00-18:00@weekdays`：时段内每 30 分钟一次，不写星期就是每天。
fn parse_during(value: &str) -> Result<AutomationSchedule, String> {
    const FORMAT: &str =
        "--during 格式为 <分钟>@HH:MM-HH:MM[@<星期>]，例如 30@09:00-18:00@weekdays";
    let mut parts = value.splitn(3, '@');
    let minutes = parts.next().unwrap_or_default();
    let window = parts.next().ok_or(FORMAT)?;
    let days = match parts.next() {
        Some(days) => parse_days(days)?,
        None => SCHEDULE_ALL_DAYS_MASK,
    };
    let (start, end) = window.split_once('-').ok_or(FORMAT)?;
    let (start_hour, start_minute) = parse_clock(start)?;
    let (end_hour, end_minute) = parse_clock(end)?;
    Ok(AutomationSchedule::During {
        minutes: positive("--during 的分钟", minutes)?,
        days,
        start_hour,
        start_minute,
        end_hour,
        end_minute,
    })
}

fn parse_days(value: &str) -> Result<u8, String> {
    let mut days = 0;
    for day in value
        .split(',')
        .map(str::trim)
        .filter(|day| !day.is_empty())
    {
        days |= match day.to_ascii_lowercase().as_str() {
            "mon" => SCHEDULE_DAY_MON,
            "tue" => SCHEDULE_DAY_TUE,
            "wed" => SCHEDULE_DAY_WED,
            "thu" => SCHEDULE_DAY_THU,
            "fri" => SCHEDULE_DAY_FRI,
            "sat" => SCHEDULE_DAY_SAT,
            "sun" => SCHEDULE_DAY_SUN,
            "weekdays" => SCHEDULE_WEEKDAYS_MASK,
            "weekend" => SCHEDULE_DAY_SAT | SCHEDULE_DAY_SUN,
            "everyday" => SCHEDULE_ALL_DAYS_MASK,
            other => {
                return Err(format!(
                    "不认识的星期：{other}（用 mon…sun、weekdays、weekend、everyday，逗号分隔）"
                ));
            }
        };
    }
    if days == 0 {
        return Err("至少写一天".into());
    }
    Ok(days)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args;
    use smelt_model::automation::WebhookIngress;

    fn create(list: &[&str]) -> Result<Automation, String> {
        parse_flags(&args(list)).and_then(create_from)
    }

    fn update(automation: &Automation, list: &[&str]) -> Result<Automation, String> {
        apply_update(automation.clone(), parse_flags(&args(list))?)
    }

    /// 运行时给 CLI 的投影：令牌位是占位，提交回去时按位置换回。
    fn daily_with_webhook() -> Automation {
        let mut automation = create(&[
            "--name",
            "日报",
            "--agent",
            "writer",
            "--prompt",
            "写日报",
            "--daily",
            "09:30",
            "--event",
            "feishu.message",
        ])
        .unwrap();
        automation.trigger.webhooks = vec![WebhookIngress {
            endpoint: "<redacted>".into(),
            secret: None,
        }];
        automation
    }

    #[test]
    fn create_builds_a_hourly_agent_job() {
        let automation = create(&[
            "--name",
            "盯盘",
            "--agent",
            "writer",
            "--every-hours",
            "1",
            "--prompt",
            "看一下持仓",
        ])
        .unwrap();
        assert_eq!(automation.name, "盯盘");
        assert_eq!(automation.agent_definition_id(), Some("writer"));
        assert_eq!(automation.prompt(), Some("看一下持仓"));
        assert_eq!(
            automation.trigger.schedule_values(),
            &[AutomationSchedule::EveryHours { hours: 1 }]
        );
        assert!(automation.sinks.is_empty(), "不写 --notify 就沿用应用通知");
    }

    #[test]
    fn create_accepts_webhook_without_a_schedule() {
        let automation = create(&["--name", "外部", "--agent", "writer", "--webhook"]).unwrap();
        assert!(automation.trigger.webhook_token().is_some());
        assert!(automation.trigger.schedule_values().is_empty());
    }

    #[test]
    fn create_combines_triggers_and_requires_one() {
        let automation = create(&[
            "--name",
            "巡检",
            "--shell",
            "make check",
            "--daily",
            "09:00",
            "--weekly",
            "mon,fri@18:00",
            "--event",
            "git.push",
            "--event-filter",
            r#"{"branch":"main"}"#,
            "--webhook",
        ])
        .unwrap();
        assert_eq!(
            automation.trigger.schedules,
            [
                AutomationSchedule::Daily { hour: 9, minute: 0 },
                AutomationSchedule::Weekly {
                    days: SCHEDULE_DAY_MON | SCHEDULE_DAY_FRI,
                    hour: 18,
                    minute: 0
                },
            ]
        );
        assert_eq!(
            automation.trigger.events[0].filter.as_deref(),
            Some(r#"{"branch":"main"}"#)
        );
        assert_eq!(automation.trigger.webhooks.len(), 1);

        assert!(create(&["--name", "空", "--agent", "writer"]).is_err());
        assert!(
            create(&[
                "--name",
                "x",
                "--agent",
                "w",
                "--no-webhook",
                "--daily",
                "09:00"
            ])
            .is_err()
        );
        assert!(create(&["--name", "x", "--agent", "w", "--event-filter", "{}"]).is_err());
        assert!(
            create(&[
                "--name",
                "x",
                "--agent",
                "w",
                "--event",
                "a",
                "--event-filter",
                "[1]"
            ])
            .is_err()
        );
    }

    #[test]
    fn update_with_a_schedule_keeps_webhook_and_events() {
        let current = daily_with_webhook();
        let updated = update(&current, &["--daily", "10:00"]).unwrap();
        assert_eq!(
            updated.trigger.schedules,
            [AutomationSchedule::Daily {
                hour: 10,
                minute: 0
            }]
        );
        assert_eq!(
            updated.trigger.webhooks, current.trigger.webhooks,
            "webhook 原样交回，运行时按位置换回真令牌"
        );
        assert_eq!(updated.trigger.events, current.trigger.events);
    }

    #[test]
    fn update_webhook_keeps_the_existing_address() {
        let current = daily_with_webhook();
        let updated = update(&current, &["--webhook"]).unwrap();
        assert_eq!(updated.trigger, current.trigger, "已有 webhook 时不换令牌");

        let mut without = current.clone();
        without.trigger.webhooks.clear();
        let added = update(&without, &["--webhook"]).unwrap();
        assert!(added.trigger.webhook_token().is_some());
        assert_eq!(added.trigger.schedules, current.trigger.schedules);
    }

    #[test]
    fn update_removes_one_kind_of_trigger() {
        let current = daily_with_webhook();
        let updated = update(&current, &["--no-schedule"]).unwrap();
        assert!(updated.trigger.schedules.is_empty());
        assert_eq!(updated.trigger.webhooks, current.trigger.webhooks);

        let switched = update(
            &current,
            &["--no-event", "--no-webhook", "--every-hours", "2"],
        )
        .unwrap();
        assert_eq!(
            switched.trigger,
            AutomationTrigger::schedule(AutomationSchedule::EveryHours { hours: 2 })
        );

        assert!(
            update(&current, &["--no-schedule", "--no-webhook", "--no-event"]).is_err(),
            "不能把触发条件删光"
        );
        assert!(update(&current, &["--no-webhook", "--webhook"]).is_err());
        assert!(update(&current, &[]).is_err());
    }

    #[test]
    fn update_switching_to_shell_drops_the_agent() {
        let current = daily_with_webhook();
        let updated = update(&current, &["--shell", "echo hi"]).unwrap();
        assert_eq!(
            updated.action,
            AutomationAction::shell("echo hi", Vec::new())
        );
        assert!(update(&updated, &["--prompt", "x"]).is_err());
    }

    #[test]
    fn notify_flag_turns_app_notifications_on_or_off() {
        let current = daily_with_webhook();
        let off = update(&current, &["--notify", "off"]).unwrap();
        assert_eq!(off.sinks, [AutomationSink::Off]);
        let on = update(&off, &["--notify", "on"]).unwrap();
        assert_eq!(on.sinks, [AutomationSink::Local]);
        assert_eq!(update(&on, &["--name", "改名"]).unwrap().sinks, on.sinks);
    }

    #[test]
    fn calendar_schedules_parse_days_and_windows() {
        assert_eq!(
            parse_weekly("weekdays@08:05").unwrap(),
            AutomationSchedule::Weekly {
                days: SCHEDULE_WEEKDAYS_MASK,
                hour: 8,
                minute: 5
            }
        );
        assert_eq!(
            parse_during("30@09:00-18:00").unwrap(),
            AutomationSchedule::During {
                minutes: 30,
                days: SCHEDULE_ALL_DAYS_MASK,
                start_hour: 9,
                start_minute: 0,
                end_hour: 18,
                end_minute: 0
            }
        );
        assert_eq!(
            parse_during("15@09:00-12:00@sat,sun").unwrap(),
            AutomationSchedule::During {
                minutes: 15,
                days: SCHEDULE_DAY_SAT | SCHEDULE_DAY_SUN,
                start_hour: 9,
                start_minute: 0,
                end_hour: 12,
                end_minute: 0
            }
        );
        assert!(parse_weekly("09:30").is_err());
        assert!(parse_weekly("someday@09:30").is_err());
        assert!(parse_during("30@09:00").is_err());
        assert!(
            parse_flags(&args(&["--during", "30@18:00-09:00"])).is_err(),
            "结束要晚于开始"
        );
        assert!(parse_flags(&args(&["--every-minutes", "0"])).is_err());
        assert!(parse_flags(&args(&["--daily", "24:00"])).is_err());
    }

    #[test]
    fn runs_are_newest_first_and_filtered() {
        let run = |id: &str, automation_id: &str, created_at: i64| AutomationRun {
            id: id.into(),
            automation_id: automation_id.into(),
            context: smelt_model::automation::AutomationRunContext {
                automation_name: "a".into(),
                action: AutomationAction::shell("true", Vec::new()),
                cwd: "/tmp".into(),
                trigger_payload: None,
                prompt: None,
                agent_definition_name: None,
                engine_kind_id: None,
                agent_instructions: None,
            },
            source: smelt_model::automation::AutomationRunSource::Manual,
            scheduled_for: None,
            status: smelt_model::automation::AutomationRunStatus::Queued,
            created_at,
            started_at: None,
            delivery_attempt_at: None,
            delivery_attempts: 0,
            finished_at: None,
            session_id: None,
            provider_session_id: None,
            output: None,
            error: None,
            runtime_released_at: None,
        };
        let runs = vec![run("1", "a", 1), run("2", "b", 2), run("3", "a", 3)];
        let ids = |runs: Vec<AutomationRun>| runs.into_iter().map(|run| run.id).collect::<Vec<_>>();
        assert_eq!(ids(recent_runs(runs.clone(), Some("a"), 10)), ["3", "1"]);
        assert_eq!(ids(recent_runs(runs, None, 2)), ["3", "2"]);

        assert_eq!(
            parse_runs_args(&args(&["a", "--limit", "5"])).unwrap(),
            (Some("a".into()), 5)
        );
        assert!(parse_runs_args(&args(&["--limit", "0"])).is_err());
        assert!(parse_runs_args(&args(&["a", "b"])).is_err());
    }
}
