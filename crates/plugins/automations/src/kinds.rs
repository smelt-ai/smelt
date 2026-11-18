//! 自动化的触发、动作、通知预设目录：编辑器里列出来的那几种，各自的文案和换算。
//!
//! 排程、运行状态归运行时（smeltd 是唯一权威），这里只有编辑器要的目录。

pub(crate) use smelt_plugin_api::model::automation::{
    Automation, AutomationAction, AutomationNotify, AutomationSchedule, AutomationTrigger,
    EventIngress, WebhookIngress,
};

use gpui::SharedString;
use rust_i18n::t;

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
    pub fn label(self) -> SharedString {
        match self {
            Self::Agent => t!("action.agent").into(),
            Self::Shell => "Shell".into(),
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
    pub fn label(self) -> SharedString {
        match self {
            Self::Hourly => t!("trigger_preset.hourly"),
            Self::Daily => t!("trigger_preset.daily"),
            Self::Weekly => t!("trigger_preset.weekly"),
            Self::Interval => t!("trigger_preset.interval"),
            Self::Webhook => "Webhook".into(),
        }
        .into()
    }

    pub fn hint(self) -> SharedString {
        match self {
            Self::Hourly => t!("trigger_preset.hourly_hint"),
            Self::Daily => t!("trigger_preset.daily_hint"),
            Self::Weekly => t!("trigger_preset.weekly_hint"),
            Self::Interval => t!("trigger_preset.interval_hint"),
            Self::Webhook => t!("trigger_preset.webhook_hint"),
        }
        .into()
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

/// 编辑器里通知的三档，从「不通知」到「每次」排。
pub(crate) const NOTIFY_CHOICES: [AutomationNotify; 3] = [
    AutomationNotify::Never,
    AutomationNotify::FailuresOnly,
    AutomationNotify::Always,
];

pub(crate) fn notify_label(notify: AutomationNotify) -> SharedString {
    match notify {
        AutomationNotify::Never => t!("notification.off"),
        AutomationNotify::FailuresOnly => t!("notification.failures"),
        AutomationNotify::Always => t!("notification.always"),
    }
    .into()
}

/// 选中那档下面的一句说明。
pub(crate) fn notify_hint(notify: AutomationNotify) -> SharedString {
    match notify {
        AutomationNotify::Never => t!("notification.off_hint"),
        AutomationNotify::FailuresOnly => t!("notification.failures_hint"),
        AutomationNotify::Always => t!("notification.always_hint"),
    }
    .into()
}
