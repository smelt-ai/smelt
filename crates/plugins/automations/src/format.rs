//! 时间、状态、来源这些给人看的短语。

use crate::prelude::*;

pub(crate) fn local_timezone_label() -> String {
    let seconds = Local::now().offset().local_minus_utc();
    let sign = if seconds >= 0 { '+' } else { '-' };
    let total_minutes = seconds.abs() / 60;
    format!(
        "本机时区 · UTC{sign}{:02}:{:02}",
        total_minutes / 60,
        total_minutes % 60
    )
}

pub(crate) fn format_unix_local(ts: i64) -> String {
    Utc.timestamp_opt(ts, 0)
        .single()
        .map(|utc| utc.with_timezone(&Local).format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|| ts.to_string())
}

pub(crate) fn format_relative_until(ts: i64, now: i64) -> String {
    let delta = ts - now;
    if delta <= 0 {
        return "即将运行".to_string();
    }
    pub(crate) const MINUTE: i64 = 60;
    pub(crate) const HOUR: i64 = 60 * MINUTE;
    pub(crate) const DAY: i64 = 24 * HOUR;
    if delta < HOUR {
        let minutes = (delta / MINUTE).max(1);
        format!("{minutes} 分钟后")
    } else if delta < DAY {
        format!("{} 小时后", delta / HOUR)
    } else {
        format!("{} 天后", delta / DAY)
    }
}

pub(crate) fn format_next_run_cell(next_run_at: Option<i64>, enabled: bool, now: i64) -> String {
    if !enabled {
        return "—".to_string();
    }
    match next_run_at {
        Some(ts) => format_relative_until(ts, now),
        None => "—".to_string(),
    }
}

pub(crate) fn format_relative_since(ts: i64, now: i64) -> String {
    let delta = now - ts;
    pub(crate) const MINUTE: i64 = 60;
    pub(crate) const HOUR: i64 = 60 * MINUTE;
    pub(crate) const DAY: i64 = 24 * HOUR;
    if delta < MINUTE {
        "刚刚".to_string()
    } else if delta < HOUR {
        format!("{} 分钟前", delta / MINUTE)
    } else if delta < DAY {
        format!("{} 小时前", delta / HOUR)
    } else if delta < 30 * DAY {
        format!("{} 天前", delta / DAY)
    } else if delta < 365 * DAY {
        format!("{} 个月前", delta / (30 * DAY))
    } else {
        format!("{} 年前", delta / (365 * DAY))
    }
}

pub(crate) fn format_last_run_cell(
    last: Option<(api::model::automation::AutomationRunStatus, i64)>,
    now: i64,
) -> String {
    match last {
        Some((status, at)) => format!(
            "{} · {}",
            run_status_label(status),
            format_relative_since(at, now)
        ),
        None => "—".to_string(),
    }
}

/// 表单还差哪些才能保存。定时触发不带外部内容，所以只有它要求智能体指令；
/// webhook / 事件会把收到的内容当指令。
pub(crate) fn automation_missing_fields(
    name: &str,
    trigger_ready: bool,
    has_schedule: bool,
    action: &AutomationAction,
    agent_exists: bool,
) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if name.trim().is_empty() {
        missing.push("名称");
    }
    if !trigger_ready {
        missing.push("触发器");
    }
    match action {
        AutomationAction::Agent { prompt, .. } => {
            if !agent_exists {
                missing.push("智能体");
            }
            if has_schedule
                && prompt
                    .as_deref()
                    .is_none_or(|prompt| prompt.trim().is_empty())
            {
                missing.push("指令");
            }
        }
        AutomationAction::Shell { command, .. } => {
            if command.trim().is_empty() {
                missing.push("命令");
            }
        }
    }
    missing
}

/// 页头上的保存状态。新建还没落盘，只说还差什么；已有的会在补齐后自动保存。
pub(crate) fn automation_save_hint(missing: &[&str], is_new: bool) -> Option<String> {
    if missing.is_empty() {
        return None;
    }
    let list = missing.join("、");
    Some(if is_new {
        format!("还差：{list}")
    } else {
        format!("还差：{list}，补齐后自动保存")
    })
}

pub(crate) fn run_source_label(
    source: api::model::automation::AutomationRunSource,
) -> &'static str {
    source.label()
}

pub(crate) fn run_status_label(
    status: api::model::automation::AutomationRunStatus,
) -> &'static str {
    use api::model::automation::AutomationRunStatus;
    match status {
        AutomationRunStatus::Starting => "启动中",
        AutomationRunStatus::Queued => "等待投递",
        AutomationRunStatus::Dispatching => "正在投递",
        AutomationRunStatus::Running => "运行中",
        AutomationRunStatus::AwaitingApproval => "等待审批",
        AutomationRunStatus::WaitingForUser => "等待输入",
        AutomationRunStatus::Completed => "已完成",
        AutomationRunStatus::Failed => "失败",
        AutomationRunStatus::Cancelled => "已取消",
        AutomationRunStatus::Skipped => "已跳过",
    }
}

pub(crate) fn agent_display_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        "未命名".to_string()
    } else {
        name.to_string()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutomationRowStatus {
    Unavailable,
    AgentMissing,
    Running,
    Active,
    Paused,
}

pub(crate) fn automation_row_status(
    enabled: bool,
    store_error: bool,
    agent_missing: bool,
    running: bool,
) -> AutomationRowStatus {
    if store_error {
        AutomationRowStatus::Unavailable
    } else if agent_missing {
        AutomationRowStatus::AgentMissing
    } else if running {
        AutomationRowStatus::Running
    } else if enabled {
        AutomationRowStatus::Active
    } else {
        AutomationRowStatus::Paused
    }
}

pub(crate) fn automation_row_status_label(status: AutomationRowStatus) -> &'static str {
    match status {
        AutomationRowStatus::Unavailable => "不可用",
        AutomationRowStatus::AgentMissing => "智能体已删除",
        AutomationRowStatus::Running => "运行中",
        AutomationRowStatus::Active => "已启用",
        AutomationRowStatus::Paused => "已暂停",
    }
}

/// 「启用」不用 Play：同一个菜单里「运行一次」已经是 Play。
pub(crate) fn automation_enabled_toggle_item(enabled: bool) -> PopupMenuItem {
    if enabled {
        PopupMenuItem::new("暂停").icon(IconName::Pause)
    } else {
        PopupMenuItem::new("启用").icon(IconName::CircleCheck)
    }
}
