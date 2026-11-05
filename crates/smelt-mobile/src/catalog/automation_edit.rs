//! 自动化编辑表单和运行时的 `Automation` 之间互转。纯函数，不碰连接。

use smelt_model::agent_definition::AgentDefinition;
use smelt_model::automation::{
    Automation, AutomationAction, AutomationSchedule as ModelSchedule, AutomationSink,
    AutomationTrigger, WebhookIngress, new_webhook_secret,
};

use crate::api::automations::{AutomationAgentChoice, AutomationDraft, AutomationEditor};
use crate::api::catalog::{AutomationActionKind, AutomationSchedule, ScheduleKind};

/// 新建时的草稿：每天 09:00，用第一个智能体。
pub(crate) fn new_draft(agents: &[AgentDefinition]) -> AutomationDraft {
    AutomationDraft {
        name: String::new(),
        action: AutomationActionKind::Agent,
        agent_definition_id: agents
            .first()
            .map(|agent| agent.id.clone())
            .unwrap_or_default(),
        prompt: String::new(),
        command: String::new(),
        schedules: vec![view_schedule(&ModelSchedule::Daily { hour: 9, minute: 0 })],
        webhook: false,
        notify: true,
    }
}

pub(crate) fn editor(
    automation: Option<&Automation>,
    agents: &[AgentDefinition],
) -> AutomationEditor {
    AutomationEditor {
        draft: automation.map_or_else(|| new_draft(agents), draft_of),
        agents: agents
            .iter()
            .map(|agent| AutomationAgentChoice {
                id: agent.id.clone(),
                name: agent.name.clone(),
            })
            .collect(),
        event_topics: automation
            .map(|automation| {
                automation
                    .trigger
                    .events
                    .iter()
                    .map(|event| event.topic.clone())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

pub(crate) fn draft_of(automation: &Automation) -> AutomationDraft {
    let (action, agent_definition_id, prompt, command) = match &automation.action {
        AutomationAction::Agent {
            agent_definition_id,
            prompt,
        } => (
            AutomationActionKind::Agent,
            agent_definition_id.clone(),
            prompt.clone().unwrap_or_default(),
            String::new(),
        ),
        // 和桌面一样把命令和参数拼成一行编辑。
        AutomationAction::Shell { command, args } => (
            AutomationActionKind::Shell,
            String::new(),
            String::new(),
            std::iter::once(command.as_str())
                .chain(args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" "),
        ),
    };
    AutomationDraft {
        name: automation.name.clone(),
        action,
        agent_definition_id,
        prompt,
        command,
        schedules: automation
            .trigger
            .schedules
            .iter()
            .map(view_schedule)
            .collect(),
        webhook: !automation.trigger.webhooks.is_empty(),
        notify: automation.notifies_app(),
    }
}

/// 把表单换到 `base` 上：新建时 `base` 为空，用 `new_id`。手机不编辑的字段（启用状态、
/// 工作目录、事件触发、已有的 webhook）从 `base` 原样带回。
pub(crate) fn apply_draft(
    base: Option<Automation>,
    new_id: impl FnOnce() -> String,
    draft: &AutomationDraft,
) -> Automation {
    let mut automation = base.unwrap_or_else(|| Automation {
        id: new_id(),
        name: String::new(),
        enabled: true,
        workspace_dir: None,
        trigger: AutomationTrigger::default(),
        action: AutomationAction::shell(String::new(), Vec::new()),
        sinks: Vec::new(),
    });
    automation.name = draft.name.trim().to_string();
    automation.action = match draft.action {
        AutomationActionKind::Agent => AutomationAction::Agent {
            agent_definition_id: draft.agent_definition_id.trim().to_string(),
            prompt: Some(draft.prompt.trim().to_string()).filter(|prompt| !prompt.is_empty()),
        },
        AutomationActionKind::Shell => {
            AutomationAction::shell(draft.command.trim().to_string(), Vec::new())
        }
    };
    automation.trigger.schedules = draft.schedules.iter().map(model_schedule).collect();
    // 已有的 webhook 带着被隐去的令牌，运行时按位置换回真令牌，不能重建。
    if !draft.webhook {
        automation.trigger.webhooks.clear();
    } else if automation.trigger.webhooks.is_empty() {
        let secret = new_webhook_secret();
        automation.trigger.webhooks.push(WebhookIngress {
            endpoint: secret.clone(),
            secret: Some(secret),
        });
    }
    automation.sinks = vec![if draft.notify {
        AutomationSink::Local
    } else {
        AutomationSink::Off
    }];
    automation
}

pub(crate) fn view_schedule(schedule: &ModelSchedule) -> AutomationSchedule {
    super::projection::schedule(schedule)
}

pub(crate) fn model_schedule(schedule: &AutomationSchedule) -> ModelSchedule {
    match schedule.kind {
        ScheduleKind::Daily => ModelSchedule::Daily {
            hour: schedule.hour,
            minute: schedule.minute,
        },
        ScheduleKind::EveryMinutes => ModelSchedule::EveryMinutes {
            minutes: schedule.minutes,
        },
        ScheduleKind::EveryHours => ModelSchedule::EveryHours {
            hours: schedule.hours,
        },
        ScheduleKind::Weekly => ModelSchedule::Weekly {
            days: schedule.days,
            hour: schedule.hour,
            minute: schedule.minute,
        },
        ScheduleKind::During => ModelSchedule::During {
            minutes: schedule.minutes,
            days: schedule.days,
            start_hour: schedule.hour,
            start_minute: schedule.minute,
            end_hour: schedule.end_hour,
            end_minute: schedule.end_minute,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> AutomationDraft {
        AutomationDraft {
            name: " 清理 ".into(),
            action: AutomationActionKind::Shell,
            agent_definition_id: String::new(),
            prompt: String::new(),
            command: " rm -rf tmp ".into(),
            schedules: vec![AutomationSchedule {
                kind: ScheduleKind::During,
                hour: 9,
                minute: 0,
                minutes: 30,
                hours: 0,
                days: 0b0011111,
                end_hour: 18,
                end_minute: 0,
            }],
            webhook: true,
            notify: false,
        }
    }

    #[test]
    fn schedules_round_trip_through_the_form() {
        for schedule in [
            ModelSchedule::Daily { hour: 7, minute: 5 },
            ModelSchedule::EveryMinutes { minutes: 15 },
            ModelSchedule::EveryHours { hours: 6 },
            ModelSchedule::Weekly {
                days: 0b1000001,
                hour: 22,
                minute: 30,
            },
            ModelSchedule::During {
                minutes: 20,
                days: 0b0011111,
                start_hour: 9,
                start_minute: 0,
                end_hour: 18,
                end_minute: 30,
            },
        ] {
            assert_eq!(model_schedule(&view_schedule(&schedule)), schedule);
        }
    }

    #[test]
    fn a_new_shell_automation_gets_a_webhook_and_quiet_sinks() {
        let automation = apply_draft(None, || "new-id".into(), &draft());
        assert_eq!(automation.id, "new-id");
        assert_eq!(automation.name, "清理");
        assert!(automation.enabled);
        assert_eq!(
            automation.action,
            AutomationAction::shell("rm -rf tmp".to_string(), Vec::new())
        );
        let webhook = &automation.trigger.webhooks[0];
        assert_eq!(webhook.secret.as_deref(), Some(webhook.endpoint.as_str()));
        assert!(!webhook.endpoint.is_empty());
        assert_eq!(automation.sinks, vec![AutomationSink::Off]);
        automation.validate().unwrap();
        // 再转回表单，看到的是同一份。
        let mut back = draft_of(&automation);
        back.name = " 清理 ".into();
        back.command = " rm -rf tmp ".into();
        assert_eq!(back, draft());
    }

    #[test]
    fn turning_the_webhook_off_drops_it_and_an_empty_prompt_is_none() {
        let mut base = apply_draft(None, || "a".into(), &draft());
        base.enabled = false;
        let mut edited = draft();
        edited.webhook = false;
        edited.action = AutomationActionKind::Agent;
        edited.agent_definition_id = "d1".into();
        edited.prompt = "  ".into();
        let automation = apply_draft(Some(base), || unreachable!("编辑不换 id"), &edited);
        assert!(automation.trigger.webhooks.is_empty());
        assert!(!automation.enabled, "启用状态不归表单管");
        assert_eq!(
            automation.action,
            AutomationAction::Agent {
                agent_definition_id: "d1".into(),
                prompt: None,
            }
        );
    }
}
