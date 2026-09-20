//! Automation UI registration points and shared domain-model exports.
//!
//! Scheduling and Run state belong to `smelt-core` so the daemon can be their sole owner. This
//! module only keeps the trigger / action catalogs used by the product UI.

pub use smelt_core::automation::{
    Automation, AutomationAction, AutomationRun, AutomationRunSource, AutomationRunStatus,
    AutomationSchedule, AutomationSink, AutomationState, AutomationTrigger, EventIngress,
    WebhookIngress, automation_notifies_app,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomationTriggerKindId {
    Calendar,
    Interval,
    Webhook,
}

pub const BUILTIN_AUTOMATION_TRIGGERS: &[AutomationTriggerKindId] = &[
    AutomationTriggerKindId::Calendar,
    AutomationTriggerKindId::Interval,
    AutomationTriggerKindId::Webhook,
];

impl AutomationTriggerKindId {
    pub fn label(self) -> &'static str {
        match self {
            Self::Calendar => "定时",
            Self::Interval => "间隔",
            Self::Webhook => "外部触发",
        }
    }

    pub fn event_title(self) -> &'static str {
        match self {
            Self::Calendar => "定时执行",
            Self::Interval => "间隔执行",
            Self::Webhook => "外部触发",
        }
    }

    pub fn event_hint(self) -> &'static str {
        match self {
            Self::Calendar => "按每日、工作日或每周的指定时间执行",
            Self::Interval => "按固定间隔执行，可限制星期和时段",
            Self::Webhook => "接收本机程序的 HTTP 请求后执行",
        }
    }

    pub fn enabled_on(self, trigger: &AutomationTrigger) -> bool {
        match self {
            Self::Calendar => trigger
                .schedule_values()
                .iter()
                .any(AutomationSchedule::is_clock),
            Self::Interval => trigger
                .schedule_values()
                .iter()
                .any(AutomationSchedule::is_interval),
            Self::Webhook => trigger.webhook_token().is_some(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomationActionKindId {
    Agent,
    Shell,
}

pub const BUILTIN_AUTOMATION_ACTIONS: &[AutomationActionKindId] =
    &[AutomationActionKindId::Agent, AutomationActionKindId::Shell];

impl AutomationActionKindId {
    pub fn label(self) -> &'static str {
        match self {
            Self::Agent => "智能体",
            Self::Shell => "Shell",
        }
    }

    pub fn from_action(action: &AutomationAction) -> Self {
        match action {
            AutomationAction::Agent { .. } => Self::Agent,
            AutomationAction::Shell { .. } => Self::Shell,
        }
    }
}

/// 添加触发器时的产品预填。内部仍落成 Calendar / Interval / Webhook。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomationTriggerPresetId {
    Hourly,
    Daily,
    Weekly,
    Interval,
    Webhook,
}

pub const BUILTIN_AUTOMATION_TRIGGER_PRESETS: &[AutomationTriggerPresetId] = &[
    AutomationTriggerPresetId::Hourly,
    AutomationTriggerPresetId::Daily,
    AutomationTriggerPresetId::Weekly,
    AutomationTriggerPresetId::Interval,
    AutomationTriggerPresetId::Webhook,
];

impl AutomationTriggerPresetId {
    pub fn label(self) -> &'static str {
        match self {
            Self::Hourly => "每小时",
            Self::Daily => "每天",
            Self::Weekly => "每周",
            Self::Interval => "间隔",
            Self::Webhook => "Webhook",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Hourly => "按选定间隔每小时",
            Self::Daily => "每天在选定的时间",
            Self::Weekly => "每周在选定的一天",
            Self::Interval => "按固定间隔执行，可限制星期和时段",
            Self::Webhook => "当此 webhook 收到请求时",
        }
    }

    pub fn is_advanced(self) -> bool {
        matches!(self, Self::Webhook)
    }

    pub fn trigger_kind(self) -> AutomationTriggerKindId {
        match self {
            Self::Hourly | Self::Interval => AutomationTriggerKindId::Interval,
            Self::Daily | Self::Weekly => AutomationTriggerKindId::Calendar,
            Self::Webhook => AutomationTriggerKindId::Webhook,
        }
    }

    pub fn schedule(self) -> Option<AutomationSchedule> {
        match self {
            Self::Hourly => Some(AutomationSchedule::EveryHours { hours: 1 }),
            Self::Daily => Some(AutomationSchedule::Daily { hour: 9, minute: 0 }),
            Self::Weekly => Some(AutomationSchedule::Weekly {
                days: smelt_core::automation::SCHEDULE_DAY_MON,
                hour: 9,
                minute: 0,
            }),
            Self::Interval => Some(AutomationSchedule::EveryMinutes { minutes: 15 }),
            Self::Webhook => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomationNotificationPreset {
    Off,
    App,
    Feishu,
    AppAndFeishu,
}

impl AutomationNotificationPreset {
    pub const ALL: [Self; 4] = [Self::Off, Self::App, Self::Feishu, Self::AppAndFeishu];

    pub fn from_sinks(sinks: &[AutomationSink]) -> Self {
        let off = sinks.iter().any(AutomationSink::is_off);
        let app = sinks.iter().any(AutomationSink::is_local);
        let feishu = sinks
            .iter()
            .any(|sink| matches!(sink, AutomationSink::Feishu { .. }));
        if off {
            Self::Off
        } else if app && feishu {
            Self::AppAndFeishu
        } else if feishu {
            Self::Feishu
        } else {
            Self::App
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "关闭",
            Self::App => "应用",
            Self::Feishu => "飞书",
            Self::AppAndFeishu => "应用 + 飞书",
        }
    }

    pub fn includes_feishu(self) -> bool {
        matches!(self, Self::Feishu | Self::AppAndFeishu)
    }

    pub fn to_sinks(self, feishu_chat_id: &str) -> Vec<AutomationSink> {
        let chat_id = feishu_chat_id.trim().to_string();
        match self {
            Self::Off => vec![AutomationSink::Off],
            Self::App => vec![AutomationSink::Local],
            Self::Feishu => vec![AutomationSink::Feishu { chat_id }],
            Self::AppAndFeishu => vec![AutomationSink::Local, AutomationSink::Feishu { chat_id }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, TimeZone};

    fn at(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
    ) -> chrono::DateTime<FixedOffset> {
        FixedOffset::east_opt(8 * 3600)
            .expect("CST")
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("valid local time")
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
    fn builtin_trigger_catalog_splits_clock_and_interval() {
        assert_eq!(
            BUILTIN_AUTOMATION_TRIGGERS,
            &[
                AutomationTriggerKindId::Calendar,
                AutomationTriggerKindId::Interval,
                AutomationTriggerKindId::Webhook
            ]
        );
        assert_eq!(AutomationTriggerKindId::Calendar.label(), "定时");
        assert_eq!(AutomationTriggerKindId::Interval.label(), "间隔");
        assert_eq!(AutomationTriggerKindId::Webhook.label(), "外部触发");
        assert_eq!(AutomationTriggerKindId::Calendar.event_title(), "定时执行");
        assert_eq!(AutomationTriggerKindId::Interval.event_title(), "间隔执行");
        assert_eq!(AutomationTriggerKindId::Webhook.event_title(), "外部触发");
        let webhook = AutomationTrigger::webhook_with_secret("tok");
        assert!(AutomationTriggerKindId::Webhook.enabled_on(&webhook));
        assert!(!AutomationTriggerKindId::Calendar.enabled_on(&webhook));
        assert!(!AutomationTriggerKindId::Interval.enabled_on(&webhook));
        let clock = AutomationTrigger::schedule(AutomationSchedule::daily_morning());
        assert!(AutomationTriggerKindId::Calendar.enabled_on(&clock));
        assert!(!AutomationTriggerKindId::Interval.enabled_on(&clock));
        let interval = AutomationTrigger::schedule(AutomationSchedule::EveryHours { hours: 1 });
        assert!(!AutomationTriggerKindId::Calendar.enabled_on(&interval));
        assert!(AutomationTriggerKindId::Interval.enabled_on(&interval));
        let both = clock.with_webhook_secret("tok");
        assert!(AutomationTriggerKindId::Calendar.enabled_on(&both));
        assert!(AutomationTriggerKindId::Webhook.enabled_on(&both));
        assert_eq!(
            AutomationTrigger::webhook_with_secret("tok_should_not_leak").summary(),
            "外部触发"
        );
        assert_eq!(both.summary(), "每天 09:00；外部触发");
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
        assert!(AutomationTriggerKindId::Webhook.enabled_on(&two_hooks));
        assert_eq!(two_hooks.summary(), "外部触发；外部触发");
    }

    #[test]
    fn simple_schedule_labels_remain_stable() {
        assert_eq!(
            AutomationSchedule::Weekly {
                days: smelt_core::automation::SCHEDULE_WEEKDAYS_MASK,
                hour: 9,
                minute: 35,
            }
            .summary(),
            "每周一、二、三、四、五 · 09:35"
        );
        assert_eq!(
            AutomationSchedule::Daily { hour: 9, minute: 0 }.summary(),
            "每天 09:00"
        );
        assert_eq!(
            AutomationSchedule::EveryMinutes { minutes: 15 }.summary(),
            "每隔 15 分钟"
        );
        assert_eq!(
            AutomationSchedule::EveryHours { hours: 1 }.summary(),
            "每隔 1 小时"
        );
        assert_eq!(
            AutomationSchedule::During {
                minutes: 15,
                days: smelt_core::automation::SCHEDULE_WEEKDAYS_MASK,
                start_hour: 9,
                start_minute: 30,
                end_hour: 15,
                end_minute: 0,
            }
            .summary(),
            "每周一、二、三、四、五 09:30–15:00 · 每隔 15 分钟"
        );
        assert_eq!(
            AutomationSchedule::Weekly {
                days: smelt_core::automation::SCHEDULE_DAY_MON
                    | smelt_core::automation::SCHEDULE_DAY_WED
                    | smelt_core::automation::SCHEDULE_DAY_FRI,
                hour: 9,
                minute: 35,
            }
            .summary(),
            "每周一、三、五 · 09:35"
        );
    }

    #[test]
    fn legacy_weekdays_json_loads_as_weekly_days() {
        let loaded: AutomationSchedule = serde_json::from_value(serde_json::json!({
            "type": "weekdays",
            "hour": 9,
            "minute": 35
        }))
        .unwrap();
        assert_eq!(
            loaded,
            AutomationSchedule::Weekly {
                days: smelt_core::automation::SCHEDULE_WEEKDAYS_MASK,
                hour: 9,
                minute: 35,
            }
        );
        let stored = serde_json::to_value(loaded).unwrap();
        assert_eq!(stored["type"], "weekly");
        assert!(stored.get("days").is_some());
    }

    #[test]
    fn weekday_schedule_skips_the_weekend() {
        let schedule = AutomationSchedule::Weekly {
            days: smelt_core::automation::SCHEDULE_WEEKDAYS_MASK,
            hour: 9,
            minute: 35,
        };
        assert_eq!(
            schedule.next_after(at(2026, 9, 4, 18, 0)),
            at(2026, 9, 7, 9, 35)
        );
    }

    #[test]
    fn weekly_schedule_honors_selected_days() {
        let schedule = AutomationSchedule::Weekly {
            days: smelt_core::automation::SCHEDULE_DAY_MON
                | smelt_core::automation::SCHEDULE_DAY_WED,
            hour: 9,
            minute: 35,
        };
        assert_eq!(
            schedule.next_after(at(2026, 9, 4, 18, 0)),
            at(2026, 9, 7, 9, 35)
        );
        assert_eq!(
            schedule.next_after(at(2026, 9, 7, 10, 0)),
            at(2026, 9, 9, 9, 35)
        );
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
    fn notification_preset_round_trips_app_feishu_and_off() {
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
        assert_eq!(
            AutomationNotificationPreset::from_sinks(&[AutomationSink::Feishu {
                chat_id: "oc_1".into(),
            }]),
            AutomationNotificationPreset::Feishu
        );
        assert_eq!(
            AutomationNotificationPreset::from_sinks(&[
                AutomationSink::Local,
                AutomationSink::Feishu {
                    chat_id: String::new(),
                },
            ]),
            AutomationNotificationPreset::AppAndFeishu
        );
        assert_eq!(
            AutomationNotificationPreset::Off.to_sinks("oc_1"),
            vec![AutomationSink::Off]
        );
        assert_eq!(
            AutomationNotificationPreset::Feishu.to_sinks(" oc_9 "),
            vec![AutomationSink::Feishu {
                chat_id: "oc_9".into(),
            }]
        );
        assert_eq!(AutomationNotificationPreset::App.label(), "应用");
        assert_eq!(AutomationNotificationPreset::Feishu.label(), "飞书");
        assert_eq!(
            AutomationNotificationPreset::AppAndFeishu.label(),
            "应用 + 飞书"
        );
        assert!(automation_notifies_app(&[]));
        assert!(automation_notifies_app(&[AutomationSink::Local]));
        assert!(!automation_notifies_app(&[AutomationSink::Off]));
        assert!(!automation_notifies_app(&[AutomationSink::Feishu {
            chat_id: "oc_1".into(),
        }]));
        assert!(automation_notifies_app(&[
            AutomationSink::Local,
            AutomationSink::Feishu {
                chat_id: "oc_1".into(),
            }
        ]));
    }
}
