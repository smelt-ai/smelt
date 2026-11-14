//! 自动化的触发、动作、通知预设目录：编辑器里列出来的那几种，各自的文案和换算。
//!
//! 排程、运行状态归运行时（smeltd 是唯一权威），这里只有编辑器要的目录。

pub(crate) use smelt_plugin_api::model::automation::{
    Automation, AutomationAction, AutomationSchedule, AutomationSink, AutomationTrigger,
    EventIngress, WebhookIngress, automation_notifies_app,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutomationTriggerKindId {
    Calendar,
    Interval,
    Webhook,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutomationActionKindId {
    Agent,
    Shell,
}

pub(crate) const BUILTIN_AUTOMATION_ACTIONS: &[AutomationActionKindId] =
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
pub(crate) enum AutomationTriggerPresetId {
    Hourly,
    Daily,
    Weekly,
    Interval,
    Webhook,
}

pub(crate) const BUILTIN_AUTOMATION_TRIGGER_PRESETS: &[AutomationTriggerPresetId] = &[
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
                days: smelt_plugin_api::model::automation::SCHEDULE_DAY_MON,
                hour: 9,
                minute: 0,
            }),
            Self::Interval => Some(AutomationSchedule::EveryMinutes { minutes: 15 }),
            Self::Webhook => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutomationNotificationPreset {
    App,
    Off,
}

impl AutomationNotificationPreset {
    pub const ALL: [Self; 2] = [Self::App, Self::Off];

    pub fn from_sinks(sinks: &[AutomationSink]) -> Self {
        if automation_notifies_app(sinks) {
            Self::App
        } else {
            Self::Off
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::App => "应用内通知",
            Self::Off => "不通知",
        }
    }

    pub fn to_sinks(self) -> Vec<AutomationSink> {
        match self {
            Self::App => vec![AutomationSink::Local],
            Self::Off => vec![AutomationSink::Off],
        }
    }
}
