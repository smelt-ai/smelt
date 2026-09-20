use std::collections::HashSet;

use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone,
};
use serde::de::{Deserializer, Error as DeError};
use serde::{Deserialize, Serialize};

pub const AUTOMATION_FILE_SCHEMA_VERSION: u32 = 1;
pub const MAX_RUNS_PER_AUTOMATION: usize = 50;
pub const MAX_RUN_OUTPUT_BYTES: usize = 16 * 1024;
pub const MAX_RUN_ERROR_BYTES: usize = 4 * 1024;
pub const MAX_EVENT_TOPIC_BYTES: usize = 255;
pub const MAX_EVENT_ID_BYTES: usize = 128;
pub const DEFAULT_WEBHOOK_PORT: u16 = 17827;
pub const DEFAULT_WEBHOOK_BASE_URL: &str = "http://127.0.0.1:17827";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AutomationAction {
    Agent {
        #[serde(alias = "agent_id")]
        agent_definition_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    Shell {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
}

impl AutomationAction {
    pub fn agent(agent_definition_id: impl Into<String>, prompt: Option<String>) -> Self {
        Self::Agent {
            agent_definition_id: agent_definition_id.into(),
            prompt,
        }
    }

    pub fn shell(command: impl Into<String>, args: Vec<String>) -> Self {
        Self::Shell {
            command: command.into(),
            args,
        }
    }

    /// 单行命令按 bash 脚本执行；带 args 时按 execve 直接启动。
    pub fn shell_invocation(&self) -> Option<(String, Vec<String>)> {
        match self {
            Self::Shell { command, args } if args.is_empty() => {
                Some(("bash".to_string(), vec!["-lc".to_string(), command.clone()]))
            }
            Self::Shell { command, args } => Some((command.clone(), args.clone())),
            Self::Agent { .. } => None,
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Agent { .. } => "智能体",
            Self::Shell { .. } => "Shell 脚本",
        }
    }

    pub fn agent_definition_id(&self) -> Option<&str> {
        match self {
            Self::Agent {
                agent_definition_id,
                ..
            } => Some(agent_definition_id),
            Self::Shell { .. } => None,
        }
    }

    pub fn prompt(&self) -> Option<&str> {
        match self {
            Self::Agent { prompt, .. } => prompt.as_deref(),
            Self::Shell { .. } => None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Agent {
                agent_definition_id,
                ..
            } => {
                if agent_definition_id.trim().is_empty() {
                    Err("请选择已配置的智能体".to_string())
                } else {
                    Ok(())
                }
            }
            Self::Shell { command, .. } => {
                if command.trim().is_empty() {
                    Err("请填写要执行的 Shell 命令".to_string())
                } else {
                    Ok(())
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AutomationSink {
    Local,
    Webhook {
        url: String,
    },
    Feishu {
        chat_id: String,
    },
    /// 明确关闭通知。空 `sinks` 仍表示沿用应用通知（旧数据）。
    Off,
}

impl AutomationSink {
    pub fn is_off(&self) -> bool {
        matches!(self, Self::Off)
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    pub fn feishu_chat_id(&self) -> Option<&str> {
        match self {
            Self::Feishu { chat_id } => {
                let chat_id = chat_id.trim();
                (!chat_id.is_empty()).then_some(chat_id)
            }
            Self::Local | Self::Webhook { .. } | Self::Off => None,
        }
    }
}

/// 空列表沿用应用通知；`Off` 明确关闭。
pub fn automation_notifies_app(sinks: &[AutomationSink]) -> bool {
    if sinks.iter().any(AutomationSink::is_off) {
        false
    } else {
        sinks.is_empty() || sinks.iter().any(AutomationSink::is_local)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookIngress {
    #[serde(default)]
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventIngress {
    pub topic: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

/// 一条自动化是一份时机列表：到点、间隔、HTTP、内部事件可混排，共享同一个忙时槽位。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AutomationTrigger {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schedules: Vec<AutomationSchedule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub webhooks: Vec<WebhookIngress>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventIngress>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AutomationTriggerLegacyWire {
    Schedule {
        #[serde(default)]
        schedules: Vec<AutomationSchedule>,
        #[serde(default)]
        schedule: Option<AutomationSchedule>,
    },
    Webhook {
        #[serde(default)]
        endpoint: String,
        #[serde(default)]
        secret: Option<String>,
    },
    Event {
        topic: String,
        #[serde(default)]
        filter: Option<String>,
    },
}

#[derive(Deserialize)]
struct AutomationTriggerFieldsWire {
    #[serde(default)]
    schedules: Vec<AutomationSchedule>,
    #[serde(default)]
    webhook: Option<WebhookIngress>,
    #[serde(default)]
    webhooks: Vec<WebhookIngress>,
    #[serde(default)]
    event: Option<EventIngress>,
    #[serde(default)]
    events: Vec<EventIngress>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AutomationTriggerWire {
    Legacy(AutomationTriggerLegacyWire),
    Fields(AutomationTriggerFieldsWire),
}

impl From<AutomationTriggerWire> for AutomationTrigger {
    fn from(value: AutomationTriggerWire) -> Self {
        match value {
            AutomationTriggerWire::Legacy(legacy) => match legacy {
                AutomationTriggerLegacyWire::Schedule {
                    schedules,
                    schedule,
                } => Self {
                    schedules: if schedules.is_empty() {
                        schedule.into_iter().collect()
                    } else {
                        schedules
                    },
                    ..Self::default()
                },
                AutomationTriggerLegacyWire::Webhook { endpoint, secret } => {
                    Self::webhook(endpoint, secret)
                }
                AutomationTriggerLegacyWire::Event { topic, filter } => Self::event(topic, filter),
            },
            AutomationTriggerWire::Fields(fields) => {
                let mut webhooks = fields.webhooks;
                if let Some(webhook) = fields.webhook {
                    webhooks.insert(0, webhook);
                }
                let mut events = fields.events;
                if let Some(event) = fields.event {
                    events.insert(0, event);
                }
                Self {
                    schedules: fields.schedules,
                    webhooks,
                    events,
                }
            }
        }
    }
}

impl<'de> Deserialize<'de> for AutomationTrigger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(AutomationTriggerWire::deserialize(deserializer)?.into())
    }
}

impl AutomationTrigger {
    pub fn schedule(schedule: AutomationSchedule) -> Self {
        Self::schedules(vec![schedule])
    }

    pub fn schedules(schedules: Vec<AutomationSchedule>) -> Self {
        Self {
            schedules,
            ..Self::default()
        }
    }

    pub fn webhook(endpoint: impl Into<String>, secret: Option<String>) -> Self {
        Self {
            webhooks: vec![WebhookIngress {
                endpoint: endpoint.into(),
                secret,
            }],
            ..Self::default()
        }
    }

    pub fn webhook_with_secret(secret: impl Into<String>) -> Self {
        let secret = secret.into();
        Self::webhook(secret.clone(), Some(secret))
    }

    pub fn with_webhook_secret(mut self, secret: impl Into<String>) -> Self {
        let secret = secret.into();
        self.webhooks = vec![WebhookIngress {
            endpoint: secret.clone(),
            secret: Some(secret),
        }];
        self
    }

    pub fn webhook_token(&self) -> Option<&str> {
        self.webhooks.iter().find_map(webhook_ingress_token)
    }

    pub fn matches_webhook_token(&self, token: &str) -> bool {
        self.webhooks
            .iter()
            .any(|webhook| webhook_ingress_token(webhook) == Some(token))
    }

    pub fn event(topic: impl Into<String>, filter: Option<String>) -> Self {
        Self {
            events: vec![EventIngress {
                topic: topic.into(),
                filter,
            }],
            ..Self::default()
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match (
            !self.schedules.is_empty(),
            !self.webhooks.is_empty(),
            !self.events.is_empty(),
        ) {
            (true, false, false) => "定时",
            (false, true, false) => "外部触发",
            (false, false, true) => "事件",
            (false, false, false) => "未配置触发条件",
            _ => "多种触发条件",
        }
    }

    pub fn summary(&self) -> String {
        let mut parts = self
            .schedules
            .iter()
            .map(AutomationSchedule::summary)
            .collect::<Vec<_>>();
        for webhook in &self.webhooks {
            parts.push(if webhook_ingress_token(webhook).is_none() {
                "未配置外部触发".to_string()
            } else {
                "外部触发".to_string()
            });
        }
        for event in &self.events {
            parts.push(format!("事件 · {}", event.topic));
        }
        if parts.is_empty() {
            "未配置触发条件".to_string()
        } else {
            parts.join("；")
        }
    }

    pub fn schedule_values(&self) -> &[AutomationSchedule] {
        &self.schedules
    }

    pub fn next_after<Tz>(&self, now: DateTime<Tz>) -> Option<DateTime<Tz>>
    where
        Tz: TimeZone,
    {
        self.schedule_values()
            .iter()
            .map(|schedule| schedule.next_after(now.clone()))
            .min()
    }

    pub fn has_calendar_schedule(&self) -> bool {
        self.schedule_values()
            .iter()
            .any(AutomationSchedule::is_calendar)
    }

    pub fn matches_inbound_event(&self, topic: &str, payload: Option<&serde_json::Value>) -> bool {
        self.events.iter().any(|event| {
            normalize_event_topic(&event.topic) == normalize_event_topic(topic)
                && event_filter_matches(event.filter.as_deref(), payload)
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        let mut any = false;
        if !self.schedules.is_empty() {
            any = true;
            for schedule in &self.schedules {
                schedule.validate()?;
            }
        }
        if !self.webhooks.is_empty() {
            any = true;
            if self
                .webhooks
                .iter()
                .any(|webhook| webhook_ingress_token(webhook).is_none())
            {
                return Err("外部触发缺少接收令牌".to_string());
            }
        }
        if !self.events.is_empty() {
            any = true;
            for event in &self.events {
                validate_event_topic(&event.topic)?;
                validate_event_filter(event.filter.as_deref())?;
            }
        }
        if any {
            Ok(())
        } else {
            Err("请至少添加一条触发条件".to_string())
        }
    }
}

/// 列表/CLI 投影用：去掉 webhook 令牌，只保留「有外部触发」这个事实。
pub fn redact_automation_credentials(mut file: AutomationFile) -> AutomationFile {
    for automation in &mut file.automations {
        for webhook in &mut automation.trigger.webhooks {
            if webhook_ingress_token(webhook).is_some() {
                webhook.endpoint = "<redacted>".into();
                webhook.secret = None;
            }
        }
    }
    file
}

fn webhook_ingress_token(webhook: &WebhookIngress) -> Option<&str> {
    webhook
        .secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let endpoint = webhook.endpoint.trim();
            (!endpoint.is_empty()).then_some(endpoint)
        })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Automation {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// Daemon-managed workspace assigned from the stable automation id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_dir: Option<String>,
    pub trigger: AutomationTrigger,
    pub action: AutomationAction,
    pub sinks: Vec<AutomationSink>,
}

#[derive(Deserialize)]
struct AutomationWire {
    id: String,
    name: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default, alias = "cwd")]
    workspace_dir: Option<String>,
    #[serde(default)]
    trigger: Option<AutomationTrigger>,
    #[serde(default)]
    action: Option<AutomationAction>,
    #[serde(default)]
    sinks: Vec<AutomationSink>,
    #[serde(default, rename = "agent_definition_id", alias = "agent_id")]
    agent_definition_id: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    schedule: Option<AutomationSchedule>,
    #[serde(default)]
    endpoint: String,
    #[serde(default)]
    secret: Option<String>,
    #[serde(default)]
    topic: String,
    #[serde(default)]
    filter: Option<String>,
}

impl AutomationWire {
    fn into_automation(self) -> Result<Automation, String> {
        let trigger = match self.trigger {
            Some(trigger) => trigger,
            None => match (self.kind.as_deref(), self.schedule) {
                (Some("schedule"), Some(schedule)) | (None, Some(schedule)) => {
                    AutomationTrigger::schedule(schedule)
                }
                (Some("webhook"), _) => AutomationTrigger::webhook(self.endpoint, self.secret),
                (Some("event"), _) => AutomationTrigger::event(self.topic, self.filter),
                _ => return Err("自动化缺少 trigger".to_string()),
            },
        };
        let action = match self.action {
            Some(action) => action,
            None => {
                let agent_definition_id = self.agent_definition_id.unwrap_or_default();
                if agent_definition_id.trim().is_empty() {
                    return Err("自动化缺少 action".to_string());
                }
                AutomationAction::Agent {
                    agent_definition_id,
                    prompt: self.prompt,
                }
            }
        };
        Ok(Automation {
            id: self.id,
            name: self.name,
            enabled: self.enabled,
            workspace_dir: self.workspace_dir,
            trigger,
            action,
            sinks: self.sinks,
        })
    }
}

impl<'de> Deserialize<'de> for Automation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        AutomationWire::deserialize(deserializer)?
            .into_automation()
            .map_err(DeError::custom)
    }
}

impl Automation {
    pub fn notifies_app(&self) -> bool {
        automation_notifies_app(&self.sinks)
    }

    pub fn feishu_chat_id(&self) -> Option<&str> {
        self.sinks.iter().find_map(AutomationSink::feishu_chat_id)
    }

    pub fn is_ready(&self) -> bool {
        self.validate().is_ok()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("自动化缺少 id".to_string());
        }
        if self.name.trim().is_empty() {
            return Err("请填写自动化名称".to_string());
        }
        self.action.validate()?;
        self.trigger.validate()
    }

    pub fn validate_execution_context(&self) -> Result<(), String> {
        self.validate()?;
        let workspace_dir = self
            .workspace_dir
            .as_deref()
            .map(str::trim)
            .unwrap_or_default();
        if workspace_dir.is_empty() {
            return Err("自动化的 Smelt 工作区尚未就绪".to_string());
        }
        if !std::path::Path::new(workspace_dir).is_dir() {
            return Err(format!(
                "自动化的 Smelt 工作区不存在或不是目录: {workspace_dir}"
            ));
        }
        Ok(())
    }

    pub fn agent_definition_id(&self) -> Option<&str> {
        self.action.agent_definition_id()
    }

    pub fn prompt(&self) -> Option<&str> {
        self.action.prompt()
    }
}

/// 周一到周日，bit0 = 周一。
pub const SCHEDULE_DAY_MON: u8 = 1 << 0;
pub const SCHEDULE_DAY_TUE: u8 = 1 << 1;
pub const SCHEDULE_DAY_WED: u8 = 1 << 2;
pub const SCHEDULE_DAY_THU: u8 = 1 << 3;
pub const SCHEDULE_DAY_FRI: u8 = 1 << 4;
pub const SCHEDULE_DAY_SAT: u8 = 1 << 5;
pub const SCHEDULE_DAY_SUN: u8 = 1 << 6;
pub const SCHEDULE_WEEKDAYS_MASK: u8 =
    SCHEDULE_DAY_MON | SCHEDULE_DAY_TUE | SCHEDULE_DAY_WED | SCHEDULE_DAY_THU | SCHEDULE_DAY_FRI;
pub const SCHEDULE_ALL_DAYS_MASK: u8 = SCHEDULE_WEEKDAYS_MASK | SCHEDULE_DAY_SAT | SCHEDULE_DAY_SUN;
pub const SCHEDULE_DAY_LABELS: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AutomationSchedule {
    Daily {
        hour: u8,
        minute: u8,
    },
    EveryMinutes {
        minutes: u32,
    },
    EveryHours {
        hours: u32,
    },
    Weekly {
        days: u8,
        hour: u8,
        minute: u8,
    },
    /// 在指定星期的墙钟时段内，按固定间隔对齐触发。
    During {
        minutes: u32,
        days: u8,
        start_hour: u8,
        start_minute: u8,
        end_hour: u8,
        end_minute: u8,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AutomationScheduleWire {
    Weekdays {
        hour: u8,
        minute: u8,
    },
    Daily {
        hour: u8,
        minute: u8,
    },
    EveryMinutes {
        minutes: u32,
    },
    EveryHours {
        hours: u32,
    },
    Weekly {
        days: u8,
        hour: u8,
        minute: u8,
    },
    During {
        minutes: u32,
        #[serde(default)]
        days: u8,
        start_hour: u8,
        start_minute: u8,
        end_hour: u8,
        end_minute: u8,
    },
}

impl From<AutomationScheduleWire> for AutomationSchedule {
    fn from(value: AutomationScheduleWire) -> Self {
        match value {
            AutomationScheduleWire::Weekdays { hour, minute } => Self::Weekly {
                days: SCHEDULE_WEEKDAYS_MASK,
                hour,
                minute,
            },
            AutomationScheduleWire::Daily { hour, minute } => Self::Daily { hour, minute },
            AutomationScheduleWire::EveryMinutes { minutes } => Self::EveryMinutes { minutes },
            AutomationScheduleWire::EveryHours { hours } => Self::EveryHours { hours },
            AutomationScheduleWire::Weekly { days, hour, minute } => {
                Self::Weekly { days, hour, minute }
            }
            AutomationScheduleWire::During {
                minutes,
                days,
                start_hour,
                start_minute,
                end_hour,
                end_minute,
            } => Self::During {
                minutes,
                days,
                start_hour,
                start_minute,
                end_hour,
                end_minute,
            },
        }
    }
}

impl<'de> Deserialize<'de> for AutomationSchedule {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(AutomationScheduleWire::deserialize(deserializer)?.into())
    }
}

impl AutomationSchedule {
    pub const fn daily_morning() -> Self {
        Self::Daily { hour: 9, minute: 0 }
    }

    pub fn is_calendar(&self) -> bool {
        matches!(
            self,
            Self::Daily { .. } | Self::Weekly { .. } | Self::During { .. }
        )
    }

    /// 墙钟时刻：每天 / 每周某天的某一分。
    pub fn is_clock(&self) -> bool {
        matches!(self, Self::Daily { .. } | Self::Weekly { .. })
    }

    /// 间隔重复：每隔 N 分/小时，或在时段内按间隔对齐。
    pub fn is_interval(&self) -> bool {
        matches!(
            self,
            Self::EveryMinutes { .. } | Self::EveryHours { .. } | Self::During { .. }
        )
    }

    pub fn summary(&self) -> String {
        match *self {
            Self::EveryMinutes { minutes } => format!("每隔 {minutes} 分钟"),
            Self::EveryHours { hours } => format!("每隔 {hours} 小时"),
            Self::Daily { hour, minute } => format!("每天 {hour:02}:{minute:02}"),
            Self::Weekly { days, hour, minute } => {
                format!("{} · {hour:02}:{minute:02}", weekly_days_summary(days))
            }
            Self::During {
                minutes,
                days,
                start_hour,
                start_minute,
                end_hour,
                end_minute,
            } => {
                let days = weekly_days_summary(if days == 0 {
                    SCHEDULE_ALL_DAYS_MASK
                } else {
                    days
                });
                format!(
                    "{days} {start_hour:02}:{start_minute:02}–{end_hour:02}:{end_minute:02} · 每隔 {minutes} 分钟"
                )
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match *self {
            Self::Daily { hour, minute } | Self::Weekly { hour, minute, .. }
                if hour < 24 && minute < 60 =>
            {
                if let Self::Weekly { days, .. } = *self
                    && days & SCHEDULE_ALL_DAYS_MASK == 0
                {
                    return Err("请至少选择一天".to_string());
                }
                Ok(())
            }
            Self::Daily { .. } | Self::Weekly { .. } => {
                Err("请输入 00:00 到 23:59 之间的时间".to_string())
            }
            Self::EveryMinutes { minutes } if (1..=24 * 60).contains(&minutes) => Ok(()),
            Self::EveryMinutes { .. } => Err("执行间隔需为 1 到 1440 分钟".to_string()),
            Self::EveryHours { hours } if (1..=24 * 7).contains(&hours) => Ok(()),
            Self::EveryHours { .. } => Err("执行间隔需为 1 到 168 小时".to_string()),
            Self::During {
                minutes,
                days,
                start_hour,
                start_minute,
                end_hour,
                end_minute,
            } => {
                if !(1..=24 * 60).contains(&minutes) {
                    return Err("执行间隔需为 1 到 1440 分钟".to_string());
                }
                if start_hour > 23 || end_hour > 23 || start_minute > 59 || end_minute > 59 {
                    return Err("请输入 00:00 到 23:59 之间的时间".to_string());
                }
                if clock_minutes(start_hour, start_minute) >= clock_minutes(end_hour, end_minute) {
                    return Err("结束时间需晚于开始时间".to_string());
                }
                if days != 0 && days & SCHEDULE_ALL_DAYS_MASK == 0 {
                    return Err("请至少选择一天".to_string());
                }
                Ok(())
            }
        }
    }

    pub fn next_after<Tz>(&self, now: DateTime<Tz>) -> DateTime<Tz>
    where
        Tz: TimeZone,
    {
        match *self {
            Self::EveryMinutes { minutes } => now + Duration::minutes(i64::from(minutes.max(1))),
            Self::EveryHours { hours } => now + Duration::hours(i64::from(hours.max(1))),
            Self::Daily { hour, minute } => {
                next_calendar_occurrence(now, hour, minute, SCHEDULE_ALL_DAYS_MASK)
            }
            Self::Weekly { days, hour, minute } => {
                next_calendar_occurrence(now, hour, minute, days & SCHEDULE_ALL_DAYS_MASK)
            }
            Self::During {
                minutes,
                days,
                start_hour,
                start_minute,
                end_hour,
                end_minute,
            } => next_windowed_interval(
                now,
                minutes,
                days,
                start_hour,
                start_minute,
                end_hour,
                end_minute,
            ),
        }
    }
}

fn clock_minutes(hour: u8, minute: u8) -> u32 {
    u32::from(hour) * 60 + u32::from(minute)
}

fn next_windowed_interval<Tz>(
    now: DateTime<Tz>,
    interval_minutes: u32,
    days: u8,
    start_hour: u8,
    start_minute: u8,
    end_hour: u8,
    end_minute: u8,
) -> DateTime<Tz>
where
    Tz: TimeZone,
{
    let timezone = now.timezone();
    let interval = interval_minutes.max(1);
    let start = clock_minutes(start_hour, start_minute);
    let end = clock_minutes(end_hour, end_minute);
    let days_mask = if days & SCHEDULE_ALL_DAYS_MASK == 0 {
        SCHEDULE_ALL_DAYS_MASK
    } else {
        days & SCHEDULE_ALL_DAYS_MASK
    };
    let mut date = now.date_naive();
    for _ in 0..14 {
        let day_bit = 1 << (date.weekday().number_from_monday() - 1);
        if days_mask & day_bit != 0 {
            let mut slot = start;
            while slot <= end {
                let hour = slot / 60;
                let minute = slot % 60;
                if let Some(time) = NaiveTime::from_hms_opt(hour, minute, 0)
                    && let Some(candidate) = resolve_wall_clock(&timezone, date, time)
                    && candidate > now
                {
                    return candidate;
                }
                slot = slot.saturating_add(interval);
            }
        }
        date = date
            .succ_opt()
            .expect("automation calendar date remains representable");
    }
    now + Duration::minutes(i64::from(interval))
}

fn weekly_days_summary(days: u8) -> String {
    let days = days & SCHEDULE_ALL_DAYS_MASK;
    if days == SCHEDULE_ALL_DAYS_MASK {
        return "每天".to_string();
    }
    let labels = (0..7)
        .filter(|index| days & (1 << index) != 0)
        .map(|index| SCHEDULE_DAY_LABELS[index])
        .collect::<Vec<_>>();
    if labels.is_empty() {
        "未选择星期".to_string()
    } else {
        format!("每周{}", labels.join("、"))
    }
}

fn next_calendar_occurrence<Tz>(
    now: DateTime<Tz>,
    hour: u8,
    minute: u8,
    days_mask: u8,
) -> DateTime<Tz>
where
    Tz: TimeZone,
{
    let timezone = now.timezone();
    let mut date = now.date_naive();
    let time = NaiveTime::from_hms_opt(u32::from(hour), u32::from(minute), 0)
        .expect("validated automation calendar schedule");
    let days_mask = if days_mask & SCHEDULE_ALL_DAYS_MASK == 0 {
        SCHEDULE_ALL_DAYS_MASK
    } else {
        days_mask & SCHEDULE_ALL_DAYS_MASK
    };
    loop {
        let day_bit = 1 << (date.weekday().number_from_monday() - 1);
        if days_mask & day_bit != 0
            && let Some(candidate) = resolve_wall_clock(&timezone, date, time)
            && candidate > now
        {
            return candidate;
        }
        date = date
            .succ_opt()
            .expect("automation calendar date remains representable");
    }
}

fn resolve_wall_clock<Tz>(timezone: &Tz, date: NaiveDate, time: NaiveTime) -> Option<DateTime<Tz>>
where
    Tz: TimeZone,
{
    let local = NaiveDateTime::new(date, time);
    match timezone.from_local_datetime(&local) {
        LocalResult::Single(value) => Some(value),
        // Use the first wall-clock occurrence during a fall-back overlap so one schedule slot
        // cannot execute twice.
        LocalResult::Ambiguous(first, _) => Some(first),
        // A nonexistent spring-forward wall-clock time is skipped for that calendar day.
        LocalResult::None => None,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationState {
    pub automation_id: String,
    #[serde(default)]
    pub next_run_at: Option<i64>,
    #[serde(default)]
    pub last_run_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunSource {
    Manual,
    Scheduled,
    Webhook,
    Event,
}

impl AutomationRunSource {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Manual => "手动运行",
            Self::Scheduled => "定时触发",
            Self::Webhook => "外部触发",
            Self::Event => "事件触发",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunStatus {
    Starting,
    Queued,
    Dispatching,
    Running,
    AwaitingApproval,
    WaitingForUser,
    Completed,
    Failed,
    Cancelled,
    Skipped,
}

impl AutomationRunStatus {
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Starting
                | Self::Queued
                | Self::Dispatching
                | Self::Running
                | Self::AwaitingApproval
                | Self::WaitingForUser
        )
    }

    pub const fn is_terminal(self) -> bool {
        !self.is_active()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AutomationRunContext {
    pub automation_name: String,
    pub action: AutomationAction,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_definition_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_kind_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_instructions: Option<String>,
}

#[derive(Deserialize)]
struct AutomationRunContextWire {
    automation_name: String,
    #[serde(default)]
    action: Option<AutomationAction>,
    cwd: String,
    #[serde(default)]
    trigger_payload: Option<serde_json::Value>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default, rename = "agent_definition_id", alias = "agent_id")]
    agent_definition_id: Option<String>,
    #[serde(default, rename = "agent_definition_name", alias = "agent_name")]
    agent_definition_name: Option<String>,
    #[serde(default, rename = "engine_kind_id", alias = "agent_kind")]
    engine_kind_id: Option<String>,
    #[serde(default)]
    agent_instructions: Option<String>,
}

impl AutomationRunContextWire {
    fn into_context(self) -> Result<AutomationRunContext, String> {
        let action = match self.action {
            Some(action) => {
                match (
                    action.agent_definition_id(),
                    self.agent_definition_id.as_deref(),
                ) {
                    (Some(action_id), Some(context_id)) if action_id != context_id => {
                        return Err(format!(
                            "自动化运行的智能体定义 ID 不一致: action={action_id}, context={context_id}"
                        ));
                    }
                    (None, Some(context_id)) if !context_id.trim().is_empty() => {
                        return Err("Shell 自动化运行不能携带智能体定义 ID".to_string());
                    }
                    _ => {}
                }
                action
            }
            None => {
                let agent_definition_id = self.agent_definition_id.clone().unwrap_or_default();
                if agent_definition_id.trim().is_empty() {
                    return Err("AutomationRun 缺少 action".to_string());
                }
                AutomationAction::Agent {
                    agent_definition_id,
                    prompt: self.prompt.clone(),
                }
            }
        };
        Ok(AutomationRunContext {
            automation_name: self.automation_name,
            action,
            cwd: self.cwd,
            trigger_payload: self.trigger_payload,
            prompt: self.prompt,
            agent_definition_name: self.agent_definition_name,
            engine_kind_id: self.engine_kind_id,
            agent_instructions: self.agent_instructions,
        })
    }
}

impl<'de> Deserialize<'de> for AutomationRunContext {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        AutomationRunContextWire::deserialize(deserializer)?
            .into_context()
            .map_err(DeError::custom)
    }
}

impl AutomationRunContext {
    pub fn from_automation(automation: &Automation) -> Self {
        Self::from_automation_with_payload(automation, None)
    }

    pub fn from_automation_with_payload(
        automation: &Automation,
        payload: Option<serde_json::Value>,
    ) -> Self {
        let prompt = match &automation.action {
            AutomationAction::Agent { prompt, .. } => {
                resolve_prompt_with_payload(prompt.as_deref(), payload.as_ref())
            }
            AutomationAction::Shell { .. } => None,
        };
        Self {
            automation_name: automation.name.clone(),
            action: automation.action.clone(),
            cwd: automation.workspace_dir.clone().unwrap_or_default(),
            trigger_payload: payload,
            prompt,
            agent_definition_name: None,
            engine_kind_id: None,
            agent_instructions: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationRun {
    pub id: String,
    pub automation_id: String,
    pub context: AutomationRunContext,
    pub source: AutomationRunSource,
    #[serde(default)]
    pub scheduled_for: Option<i64>,
    pub status: AutomationRunStatus,
    pub created_at: i64,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub delivery_attempt_at: Option<i64>,
    #[serde(default)]
    pub delivery_attempts: u32,
    #[serde(default)]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub provider_session_id: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub runtime_released_at: Option<i64>,
}

impl AutomationRun {
    pub fn blocks_overlap(&self) -> bool {
        if self.status.is_active() {
            return true;
        }
        if self.runtime_released_at.is_some() {
            return false;
        }
        self.session_id.is_some()
            || (matches!(self.status, AutomationRunStatus::Cancelled)
                && self.context.action.shell_invocation().is_some())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationFile {
    pub schema_version: u32,
    #[serde(default)]
    pub store_id: String,
    pub revision: u64,
    #[serde(default)]
    pub timezone_fingerprint: String,
    #[serde(default)]
    pub automations: Vec<Automation>,
    #[serde(default)]
    pub states: Vec<AutomationState>,
    #[serde(default)]
    pub runs: Vec<AutomationRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_error: Option<String>,
    /// 本机 Webhook 根地址，由 daemon 运行时填入投影，不作为存档真源。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_base_url: Option<String>,
}

impl Default for AutomationFile {
    fn default() -> Self {
        Self {
            schema_version: AUTOMATION_FILE_SCHEMA_VERSION,
            store_id: new_store_id(),
            revision: 0,
            timezone_fingerprint: String::new(),
            automations: Vec::new(),
            states: Vec::new(),
            runs: Vec::new(),
            store_error: None,
            webhook_base_url: None,
        }
    }
}

pub fn new_webhook_secret() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub fn webhook_url(base: &str, token: &str) -> String {
    format!("{}/hooks/{token}", base.trim_end_matches('/'))
}

impl AutomationFile {
    pub fn state_for(&self, automation_id: &str) -> Option<&AutomationState> {
        self.states
            .iter()
            .find(|state| state.automation_id == automation_id)
    }

    pub fn state_for_mut(&mut self, automation_id: &str) -> Option<&mut AutomationState> {
        self.states
            .iter_mut()
            .find(|state| state.automation_id == automation_id)
    }

    pub fn webhook_automation_for(&self, token: &str) -> Option<&Automation> {
        let token = token.trim();
        if token.is_empty() {
            return None;
        }
        self.automations
            .iter()
            .find(|automation| automation.trigger.matches_webhook_token(token))
    }

    /// EventHub 直播投影：定义、状态、Run 元数据。
    /// Run 正文（output / 触发载荷 / 智能体指令快照）留在 SQLite，点开详情再读。
    pub fn live_projection(&self) -> Self {
        let mut file = self.clone();
        for run in &mut file.runs {
            run.output = None;
            run.context.trigger_payload = None;
            run.context.agent_instructions = None;
        }
        file
    }

    pub fn active_run_for(&self, automation_id: &str) -> Option<&AutomationRun> {
        self.runs
            .iter()
            .find(|run| run.automation_id == automation_id && run.blocks_overlap())
    }

    pub fn latest_run_for(&self, automation_id: &str) -> Option<&AutomationRun> {
        self.runs
            .iter()
            .filter(|run| run.automation_id == automation_id)
            .max_by_key(|run| (run.created_at, run.id.as_str()))
    }

    pub fn runs_for(&self, automation_id: &str) -> Vec<&AutomationRun> {
        let mut runs = self
            .runs
            .iter()
            .filter(|run| run.automation_id == automation_id)
            .collect::<Vec<_>>();
        runs.sort_by_key(|run| std::cmp::Reverse((run.created_at, run.id.as_str())));
        runs
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.store_id.trim().is_empty() {
            return Err("自动化存储缺少 store_id".to_string());
        }
        if self.schema_version > AUTOMATION_FILE_SCHEMA_VERSION {
            return Err(format!(
                "自动化存储版本 {} 高于当前支持版本 {}",
                self.schema_version, AUTOMATION_FILE_SCHEMA_VERSION
            ));
        }
        let mut ids = HashSet::new();
        for automation in &self.automations {
            automation.validate()?;
            if !ids.insert(automation.id.as_str()) {
                return Err(format!("自动化 id 重复: {}", automation.id));
            }
        }
        let automation_ids = self
            .automations
            .iter()
            .map(|automation| automation.id.as_str())
            .collect::<HashSet<_>>();
        let mut state_ids = HashSet::new();
        for state in &self.states {
            if !automation_ids.contains(state.automation_id.as_str()) {
                return Err(format!(
                    "自动化运行态引用了不存在的定义: {}",
                    state.automation_id
                ));
            }
            if !state_ids.insert(state.automation_id.as_str()) {
                return Err(format!("自动化运行态重复: {}", state.automation_id));
            }
            if let Some(last_run_id) = state.last_run_id.as_deref()
                && !self
                    .runs
                    .iter()
                    .any(|run| run.id == last_run_id && run.automation_id == state.automation_id)
            {
                return Err(format!("自动化运行态引用了不存在的 Run: {last_run_id}"));
            }
        }
        let mut run_ids = HashSet::new();
        let mut unreleased_session_ids = HashSet::new();
        for run in &self.runs {
            if !run_ids.insert(run.id.as_str()) {
                return Err(format!("AutomationRun id 重复: {}", run.id));
            }
            if !automation_ids.contains(run.automation_id.as_str()) {
                return Err(format!("AutomationRun 引用了不存在的自动化: {}", run.id));
            }
            if let Some(session_id) = run
                .session_id
                .as_deref()
                .filter(|_| run.runtime_released_at.is_none())
                && !unreleased_session_ids.insert(session_id)
            {
                return Err(format!(
                    "多个未释放 AutomationRun 共用 ACP 会话: {session_id}"
                ));
            }
            if run.context.cwd.trim().is_empty() {
                return Err(format!("AutomationRun 执行上下文不完整: {}", run.id));
            }
            if let AutomationAction::Agent {
                agent_definition_id,
                ..
            } = &run.context.action
            {
                if agent_definition_id.trim().is_empty() {
                    return Err(format!("AutomationRun 缺少智能体标识: {}", run.id));
                }
            } else if let AutomationAction::Shell { command, .. } = &run.context.action
                && command.trim().is_empty()
            {
                return Err(format!("AutomationRun 缺少 Shell 命令: {}", run.id));
            }
            if run
                .output
                .as_ref()
                .is_some_and(|value| value.len() > MAX_RUN_OUTPUT_BYTES)
                || run
                    .error
                    .as_ref()
                    .is_some_and(|value| value.len() > MAX_RUN_ERROR_BYTES)
            {
                return Err(format!("AutomationRun 结果文本超过存储上限: {}", run.id));
            }
            if run.blocks_overlap()
                && let AutomationAction::Agent { .. } = &run.context.action
                && (run
                    .context
                    .agent_definition_name
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .is_empty()
                    || run
                        .context
                        .engine_kind_id
                        .as_deref()
                        .map(str::trim)
                        .unwrap_or_default()
                        .is_empty()
                    || run.context.agent_instructions.is_none())
            {
                return Err(format!("活跃 AutomationRun 缺少智能体快照: {}", run.id));
            }
        }
        for automation_id in automation_ids {
            if self
                .runs
                .iter()
                .filter(|run| run.automation_id == automation_id && run.blocks_overlap())
                .count()
                > 1
            {
                return Err(format!("自动化 {automation_id} 存在多个活跃 Run"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum AutomationCommand {
    Upsert {
        automation: Box<Automation>,
    },
    Delete {
        automation_id: String,
    },
    SetEnabled {
        automation_id: String,
        enabled: bool,
    },
    CancelRun {
        run_id: String,
    },
    RunOnce {
        automation_id: String,
    },
    Trigger {
        automation_id: String,
        source: AutomationRunSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
pub enum AutomationCommandResult {
    Applied(bool),
    Run(Box<AutomationRun>),
}

impl AutomationCommandResult {
    pub fn run(self) -> Option<AutomationRun> {
        match self {
            Self::Run(run) => Some(*run),
            Self::Applied(_) => None,
        }
    }
}

pub fn apply_automation_command<Tz>(
    file: &mut AutomationFile,
    command: AutomationCommand,
    now: DateTime<Tz>,
) -> Result<AutomationCommandResult, String>
where
    Tz: TimeZone,
{
    match command {
        AutomationCommand::Upsert { automation } => {
            automation.validate_execution_context()?;
            let automation = *automation;
            let existing = file
                .automations
                .iter()
                .position(|candidate| candidate.id == automation.id);
            let changed = existing
                .map(|index| file.automations[index] != automation)
                .unwrap_or(true);
            if !changed {
                return Ok(AutomationCommandResult::Applied(false));
            }
            let schedule_changed = existing
                .map(|index| {
                    file.automations[index].trigger != automation.trigger
                        || file.automations[index].enabled != automation.enabled
                })
                .unwrap_or(true);
            let automation_id = automation.id.clone();
            if let Some(index) = existing {
                file.automations[index] = automation;
            } else {
                file.automations.push(automation);
            }
            ensure_state(file, &automation_id);
            if schedule_changed {
                refresh_schedule(file, &automation_id, now);
            }
            Ok(AutomationCommandResult::Applied(true))
        }
        AutomationCommand::Delete { automation_id } => {
            if let Some(run) = file.active_run_for(&automation_id) {
                return Err(if run.status.is_active() {
                    "自动化正在运行，暂时不能删除".to_string()
                } else {
                    "自动化运行现场正在清理，请稍后再删除".to_string()
                });
            }
            let before = file.automations.len();
            file.automations
                .retain(|automation| automation.id != automation_id);
            file.states
                .retain(|state| state.automation_id != automation_id);
            file.runs.retain(|run| run.automation_id != automation_id);
            Ok(AutomationCommandResult::Applied(
                file.automations.len() != before,
            ))
        }
        AutomationCommand::SetEnabled {
            automation_id,
            enabled,
        } => {
            let Some(index) = file
                .automations
                .iter()
                .position(|automation| automation.id == automation_id)
            else {
                return Err("自动化不存在".to_string());
            };
            if file.automations[index].enabled == enabled {
                return Ok(AutomationCommandResult::Applied(false));
            }
            if enabled {
                file.automations[index].validate_execution_context()?;
            }
            file.automations[index].enabled = enabled;
            ensure_state(file, &automation_id);
            refresh_schedule(file, &automation_id, now);
            Ok(AutomationCommandResult::Applied(true))
        }
        AutomationCommand::CancelRun { run_id } => {
            let run = file
                .runs
                .iter_mut()
                .find(|run| run.id == run_id)
                .ok_or_else(|| "AutomationRun 不存在".to_string())?;
            if run.status.is_terminal() {
                return Ok(AutomationCommandResult::Applied(false));
            }
            run.status = AutomationRunStatus::Cancelled;
            run.finished_at = Some(now.timestamp());
            // Shell 进程和 ACP 会话都由 runtime 回收后再释放；尚未绑定会话的
            // 智能体 Run 没有可回收资源，这里直接放开重叠锁。
            if run.session_id.is_none() && run.context.action.shell_invocation().is_none() {
                run.runtime_released_at = Some(now.timestamp());
            }
            Ok(AutomationCommandResult::Applied(true))
        }
        AutomationCommand::RunOnce { automation_id } => {
            execute_trigger(file, automation_id, AutomationRunSource::Manual, None, now)
        }
        AutomationCommand::Trigger {
            automation_id,
            source,
            payload,
        } => execute_trigger(file, automation_id, source, payload, now),
    }
}

fn execute_trigger<Tz>(
    file: &mut AutomationFile,
    automation_id: String,
    source: AutomationRunSource,
    payload: Option<serde_json::Value>,
    now: DateTime<Tz>,
) -> Result<AutomationCommandResult, String>
where
    Tz: TimeZone,
{
    let Some(automation) = file
        .automations
        .iter()
        .find(|automation| automation.id == automation_id)
    else {
        return Err("自动化不存在".to_string());
    };
    automation.validate_execution_context()?;
    if let Some(run) = file.active_run_for(&automation_id) {
        return Err(format!("自动化已有运行中的 Run: {}", run.id));
    }
    let run = AutomationRun {
        id: uuid::Uuid::new_v4().to_string(),
        automation_id: automation_id.clone(),
        context: AutomationRunContext::from_automation_with_payload(automation, payload),
        source,
        scheduled_for: None,
        status: AutomationRunStatus::Starting,
        created_at: now.timestamp(),
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
    ensure_state(file, &automation_id);
    file.state_for_mut(&automation_id).unwrap().last_run_id = Some(run.id.clone());
    file.runs.push(run.clone());
    prune_runs(file);
    Ok(AutomationCommandResult::Run(Box::new(run)))
}

/// 外部程序投递的通用事件。自动化按 topic / filter 订阅，不点名自动化 id。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationInboundEvent {
    pub topic: String,
    pub event_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

impl AutomationInboundEvent {
    pub fn validate(&self) -> Result<(), String> {
        validate_event_topic(&self.topic)?;
        validate_event_id(&self.event_id)?;
        Ok(())
    }

    pub fn normalized_topic(&self) -> String {
        normalize_event_topic(&self.topic)
    }

    pub fn normalized_event_id(&self) -> String {
        self.event_id.trim().to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomationEventPublished {
    pub event_id: String,
    pub topic: String,
    pub runs: Vec<AutomationRun>,
    pub already_recorded: usize,
}

pub fn event_run_id(automation_id: &str, event_id: &str) -> String {
    format!("event:{automation_id}:{event_id}")
}

pub fn normalize_event_topic(topic: &str) -> String {
    topic.trim().to_ascii_lowercase()
}

pub fn validate_event_topic(topic: &str) -> Result<String, String> {
    let topic = normalize_event_topic(topic);
    if topic.is_empty() {
        return Err("请填写事件主题".to_string());
    }
    if topic.len() > MAX_EVENT_TOPIC_BYTES {
        return Err("事件主题过长".to_string());
    }
    let segments = topic.split('.').collect::<Vec<_>>();
    if segments.len() < 2
        || segments.iter().any(|segment| {
            segment.is_empty()
                || !segment.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                })
        })
    {
        return Err("事件主题须为小写点分名，例如 lark.message.received".to_string());
    }
    Ok(topic)
}

pub fn validate_event_id(event_id: &str) -> Result<String, String> {
    let event_id = event_id.trim();
    if event_id.is_empty() {
        return Err("请填写 event_id".to_string());
    }
    if event_id.len() > MAX_EVENT_ID_BYTES {
        return Err("event_id 过长".to_string());
    }
    if event_id
        .chars()
        .any(|ch| ch.is_control() || matches!(ch, '/' | '\\'))
    {
        return Err("event_id 含非法字符".to_string());
    }
    Ok(event_id.to_string())
}

pub fn validate_event_filter(filter: Option<&str>) -> Result<Option<String>, String> {
    let Some(raw) = filter.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Object(_)) => Ok(Some(raw.to_string())),
        _ => Err("事件过滤须为 JSON 对象，例如 {\"chat_id\":\"oc_123\"}".to_string()),
    }
}

pub fn event_filter_matches(filter: Option<&str>, payload: Option<&serde_json::Value>) -> bool {
    let Some(filter) = filter.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    let Ok(serde_json::Value::Object(required)) = serde_json::from_str::<serde_json::Value>(filter)
    else {
        return false;
    };
    if required.is_empty() {
        return true;
    }
    let Some(serde_json::Value::Object(payload)) = payload else {
        return false;
    };
    required
        .iter()
        .all(|(key, value)| payload.get(key) == Some(value))
}

pub fn publish_automation_event<Tz>(
    file: &mut AutomationFile,
    event: &AutomationInboundEvent,
    now: DateTime<Tz>,
) -> Result<AutomationEventPublished, String>
where
    Tz: TimeZone,
{
    event.validate()?;
    let topic = event.normalized_topic();
    let event_id = event.normalized_event_id();
    let payload = event.payload.clone();
    let matched = file
        .automations
        .iter()
        .filter(|automation| {
            automation.enabled
                && automation
                    .trigger
                    .matches_inbound_event(&topic, payload.as_ref())
        })
        .cloned()
        .collect::<Vec<_>>();

    let mut runs = Vec::new();
    let mut already_recorded = 0;
    for automation in matched {
        let run_id = event_run_id(&automation.id, &event_id);
        if file.runs.iter().any(|run| run.id == run_id) {
            already_recorded += 1;
            continue;
        }
        let overlapping = file.active_run_for(&automation.id).is_some();
        let context_error = automation.validate_execution_context().err();
        let (status, error, finished_at, runtime_released_at) = if let Some(error) = context_error {
            (
                AutomationRunStatus::Failed,
                Some(error),
                Some(now.timestamp()),
                Some(now.timestamp()),
            )
        } else if overlapping {
            (
                AutomationRunStatus::Skipped,
                Some("上一条 Run 仍在执行，本次事件已跳过".to_string()),
                Some(now.timestamp()),
                Some(now.timestamp()),
            )
        } else {
            (AutomationRunStatus::Starting, None, None, None)
        };
        let run = AutomationRun {
            id: run_id,
            automation_id: automation.id.clone(),
            context: AutomationRunContext::from_automation_with_payload(
                &automation,
                payload.clone(),
            ),
            source: AutomationRunSource::Event,
            scheduled_for: None,
            status,
            created_at: now.timestamp(),
            started_at: None,
            delivery_attempt_at: None,
            delivery_attempts: 0,
            finished_at,
            session_id: None,
            provider_session_id: None,
            output: None,
            error,
            runtime_released_at,
        };
        ensure_state(file, &automation.id);
        file.state_for_mut(&automation.id).unwrap().last_run_id = Some(run.id.clone());
        file.runs.push(run.clone());
        runs.push(run);
    }
    prune_runs(file);
    Ok(AutomationEventPublished {
        event_id,
        topic,
        runs,
        already_recorded,
    })
}

pub fn claim_due_automations<Tz>(file: &mut AutomationFile, now: DateTime<Tz>) -> Vec<AutomationRun>
where
    Tz: TimeZone,
{
    let due = file
        .automations
        .iter()
        .filter(|automation| automation.enabled)
        .filter_map(|automation| {
            let scheduled_for = file.state_for(&automation.id)?.next_run_at?;
            (scheduled_for <= now.timestamp()).then(|| (automation.clone(), scheduled_for))
        })
        .collect::<Vec<_>>();
    let mut claimed = Vec::new();
    for (automation, scheduled_for) in due {
        let next_run_at = automation
            .trigger
            .next_after(now.clone())
            .map(|when| when.timestamp());
        ensure_state(file, &automation.id);
        file.state_for_mut(&automation.id).unwrap().next_run_at = next_run_at;

        let run_id = format!("schedule:{}:{scheduled_for}", automation.id);
        if file.runs.iter().any(|run| run.id == run_id) {
            continue;
        }
        let overlapping = file.active_run_for(&automation.id).is_some();
        let run = AutomationRun {
            id: run_id,
            automation_id: automation.id.clone(),
            context: AutomationRunContext::from_automation(&automation),
            source: AutomationRunSource::Scheduled,
            scheduled_for: Some(scheduled_for),
            status: if overlapping {
                AutomationRunStatus::Skipped
            } else {
                AutomationRunStatus::Starting
            },
            created_at: now.timestamp(),
            started_at: None,
            delivery_attempt_at: None,
            delivery_attempts: 0,
            finished_at: overlapping.then_some(now.timestamp()),
            session_id: None,
            provider_session_id: None,
            output: None,
            error: overlapping.then(|| "上一条 Run 仍在执行，本次计划已跳过".to_string()),
            runtime_released_at: overlapping.then_some(now.timestamp()),
        };
        file.state_for_mut(&automation.id).unwrap().last_run_id = Some(run.id.clone());
        file.runs.push(run.clone());
        if !overlapping {
            claimed.push(run);
        }
    }
    prune_runs(file);
    claimed
}

fn ensure_state(file: &mut AutomationFile, automation_id: &str) {
    if file.state_for(automation_id).is_none() {
        file.states.push(AutomationState {
            automation_id: automation_id.to_string(),
            next_run_at: None,
            last_run_id: None,
        });
    }
}

fn refresh_schedule<Tz>(file: &mut AutomationFile, automation_id: &str, now: DateTime<Tz>)
where
    Tz: TimeZone,
{
    let next_run_at = file
        .automations
        .iter()
        .find(|automation| automation.id == automation_id)
        .filter(|automation| automation.enabled)
        .and_then(|automation| {
            automation
                .trigger
                .next_after(now)
                .map(|when| when.timestamp())
        });
    file.state_for_mut(automation_id).unwrap().next_run_at = next_run_at;
}

pub fn resolve_prompt_with_payload(
    template: Option<&str>,
    payload: Option<&serde_json::Value>,
) -> Option<String> {
    match (template, payload) {
        (Some(tmpl), Some(payload)) if tmpl.contains("{{") => Some(render_template(tmpl, payload)),
        (Some(tmpl), _) if !tmpl.trim().is_empty() => Some(tmpl.to_string()),
        (None | Some(_), Some(payload)) => Some(format_payload_prompt(payload)),
        (Some(tmpl), None) => Some(tmpl.to_string()),
        (None, None) => None,
    }
}

pub fn format_payload_prompt(payload: &serde_json::Value) -> String {
    if let Some(s) = payload.as_str() {
        s.to_string()
    } else if let Some(text) = payload.get("text").and_then(|v| v.as_str()) {
        text.to_string()
    } else if let Some(message) = payload.get("message").and_then(|v| v.as_str()) {
        message.to_string()
    } else if let Some(content) = payload.get("content").and_then(|v| v.as_str()) {
        content.to_string()
    } else {
        serde_json::to_string_pretty(payload).unwrap_or_else(|_| payload.to_string())
    }
}

pub fn render_template(template: &str, payload: &serde_json::Value) -> String {
    let mut result = template.to_string();
    if let Some(obj) = payload.as_object() {
        for (k, v) in obj {
            let val_str = if let Some(s) = v.as_str() {
                s.to_string()
            } else {
                v.to_string()
            };
            result = result.replace(&format!("{{{{{k}}}}}"), &val_str);
        }
    }
    result = result.replace("{{payload}}", &format_payload_prompt(payload));
    result
}

pub fn prune_runs(file: &mut AutomationFile) {
    let automation_ids = file
        .automations
        .iter()
        .map(|automation| automation.id.clone())
        .collect::<Vec<_>>();
    for automation_id in automation_ids {
        let last_run_id = file
            .state_for(&automation_id)
            .and_then(|state| state.last_run_id.as_deref());
        let mut terminal = file
            .runs
            .iter()
            .enumerate()
            .filter(|(_, run)| run.automation_id == automation_id && run.status.is_terminal())
            .map(|(index, run)| (index, run.created_at, run.id.clone()))
            .collect::<Vec<_>>();
        terminal.sort_by_key(|(_, created_at, id)| std::cmp::Reverse((*created_at, id.clone())));
        let excess = terminal.len().saturating_sub(MAX_RUNS_PER_AUTOMATION);
        let remove = terminal
            .into_iter()
            .rev()
            .filter(|(index, _, id)| {
                let run = &file.runs[*index];
                Some(id.as_str()) != last_run_id
                    && (run.session_id.is_none() || run.runtime_released_at.is_some())
            })
            .take(excess)
            .map(|(index, _, _)| index)
            .collect::<HashSet<_>>();
        if !remove.is_empty() {
            file.runs = file
                .runs
                .drain(..)
                .enumerate()
                .filter_map(|(index, run)| (!remove.contains(&index)).then_some(run))
                .collect();
        }
    }
}

fn new_store_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

const fn default_true() -> bool {
    true
}
