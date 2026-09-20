//! 会话行展示所需的图标、状态、外部链接与额度文案。

use chrono::{DateTime, Datelike, Local, TimeZone};

use super::*;
use smelt_core::agent_kind::TerminalAgentKind;

/// agent 身份图标统一走本地单色 SVG，颜色由会话状态驱动：图标同时回答
/// 「这是哪家的会话」和「当前处于什么状态」。
pub(super) fn provider_icon(agent: Option<TerminalAgentKind>) -> Icon {
    match agent {
        Some(agent) => Icon::empty().path(agent.icon_asset()),
        None => Icon::new(IconName::SquareTerminal),
    }
}

/// ACP agent 的图标。两张表不是同一个集合——dsh 只有 ACP 桥，终端表里查不到它，
/// 所以直接按稳定存档 id 取资源（`smelt-icons/agent-<id>.svg`）。
pub(super) fn acp_provider_icon(agent: ConversationAgentKind) -> Icon {
    Icon::empty().path(agent.icon_asset())
}

pub(super) fn plugin_agent_icon(agent: &crate::plugin_ui::PluginAgentPresentation) -> Icon {
    agent
        .icon_asset
        .as_ref()
        .map(|path| Icon::empty().path(path.clone()))
        .unwrap_or_else(|| Icon::new(IconName::Bot))
}

pub(super) fn plugin_session_action_icon(
    icon: Option<smelt_plugin_api::SessionActionIcon>,
) -> Option<IconName> {
    match icon {
        Some(smelt_plugin_api::SessionActionIcon::Check) => Some(IconName::Check),
        Some(smelt_plugin_api::SessionActionIcon::ExternalLink) => Some(IconName::ExternalLink),
        None => None,
    }
}

/// 任务页会保留上次活动会话，方便返回时恢复；但它不是任务页中的选中项。
pub(super) fn session_row_is_selected(
    session_route_active: bool,
    project_context_matches: bool,
    ix: usize,
    active: usize,
) -> bool {
    session_route_active && project_context_matches && ix == active
}

/// 项目只是会话的分组上下文；当前会话已经在这个项目里时，项目标题不应再画一层
/// 同等级选中底色。
pub(super) fn project_header_is_selected(
    session_route_active: bool,
    is_active_project: bool,
    active_session_is_visible: bool,
) -> bool {
    session_route_active && is_active_project && !active_session_is_visible
}

/// 会话行副标题里的状态文案（与菜单栏下拉同一套口径）。
pub(super) fn status_text(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::NeedsYou => "需要你",
        AgentStatus::Running => "运行中",
        AgentStatus::Idle => "空闲",
    }
}

/// 侧栏会话行要写出来的状态：只要人管的才占字。运行中靠图标颜色，不写「运行中」。
pub(crate) fn session_row_status_label(status: AgentStatus) -> Option<&'static str> {
    (status == AgentStatus::NeedsYou).then(|| status_text(status))
}

/// 侧栏第二行的最近活动时间。采用不会随分钟流逝而过期的本地日历文案：今天和
/// 昨天保留时分，更早的会话显示日期；旧存档没有时间时明确写“时间未知”。
pub(super) fn session_updated_at_text(timestamp: u64, now: &DateTime<Local>) -> String {
    if timestamp == 0 {
        return "时间未知".to_string();
    }
    let Some(updated) = Local
        .timestamp_opt(timestamp.min(i64::MAX as u64) as i64, 0)
        .single()
    else {
        return "时间未知".to_string();
    };
    match (now.date_naive() - updated.date_naive()).num_days() {
        days if days <= 0 => format!("今天 {}", updated.format("%H:%M")),
        1 => format!("昨天 {}", updated.format("%H:%M")),
        _ if updated.year() == now.year() => updated.format("%m-%d %H:%M").to_string(),
        _ => updated.format("%Y-%m-%d").to_string(),
    }
}

/// 分屏优先展示自己的守护更新时间；守护镜像尚未到达时，回退到所属会话时间。
pub(super) fn effective_pane_updated_at(pane_updated_at: u64, session_updated_at: u64) -> u64 {
    if pane_updated_at == 0 {
        session_updated_at
    } else {
        pane_updated_at
    }
}

/// 按已用百分比取行内颜色：>=90% 红、>=75% 黄、否则灰（额度充足，默认不抢眼）。
pub(super) fn quota_color(used_percent: f64) -> u32 {
    if used_percent >= 90.0 {
        ui_theme::red()
    } else if used_percent >= 75.0 {
        ui_theme::yellow()
    } else {
        ui_theme::text_muted()
    }
}

/// 把相对秒数格式化成「X天X小时」这类可读文案；0 或负数（已重置/不足一分钟）
/// 返回 None，调用方决定不附加相对时间。
pub(super) fn quota_until_text(seconds: i64) -> Option<String> {
    let seconds = seconds.max(0);
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        if hours > 0 {
            Some(format!("{days}天{hours}小时"))
        } else {
            Some(format!("{days}天"))
        }
    } else if hours > 0 {
        if minutes > 0 {
            Some(format!("{hours}小时{minutes}分"))
        } else {
            Some(format!("{hours}小时"))
        }
    } else if minutes > 0 {
        Some(format!("{minutes}分"))
    } else {
        None
    }
}

/// 窗口重置时间文本：绝对时间带相对剩余（如「重置 08-09 14:00（还有 2天5小时）」），
/// 没有绝对时间时退回相对秒数。
pub(super) fn quota_reset_text(window: &smelt_core::provider_quota::ProviderQuotaWindow) -> String {
    if let Some(millis) = window.resets_at_ms
        && let Some(date) = Local.timestamp_millis_opt(millis as i64).single()
    {
        let until = date.signed_duration_since(Local::now()).num_seconds();
        return match quota_until_text(until) {
            Some(text) => format!("重置 {}（还有 {text}）", date.format("%m-%d %H:%M")),
            None => format!("重置 {}", date.format("%m-%d %H:%M")),
        };
    }
    if let Some(seconds) = window.reset_after_seconds {
        return match quota_until_text(seconds as i64) {
            Some(text) => format!("约 {text} 后重置"),
            None => "即将重置".to_string(),
        };
    }
    "重置时间未知".to_string()
}

pub(super) struct ProviderQuotaRow {
    pub(super) text: String,
    pub(super) color: u32,
    pub(super) hint: String,
}

/// 把两种 provider 额度模型归一成侧栏的一行。注册表负责“查哪家”，这里仅负责
/// “怎么显示”，Workspace 渲染无需按 Agent 身份重复展开状态。
pub(super) fn provider_quota_row(
    provider: ConversationAgentKind,
    status: &crate::provider_quota::QuotaStatus,
) -> Option<ProviderQuotaRow> {
    let label = provider.short_label();
    match status.quota.as_ref()? {
        crate::provider_quota::ProviderQuotaData::Windows(quota) => {
            let worst_used = quota
                .windows
                .iter()
                .map(|window| window.used_percent)
                .fold(0.0_f64, f64::max);
            let mut hint = quota
                .windows
                .iter()
                .map(|window| {
                    format!(
                        "{label} {} 窗口已用 {}%，{}。",
                        window.label,
                        window.used_percent.round() as u32,
                        quota_reset_text(window)
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            if let Some(error) = &status.error {
                hint.push_str(&format!(
                    " {label} 额度刷新失败，仍显示上次成功结果：{error}。"
                ));
            }
            Some(ProviderQuotaRow {
                text: format!("{}%", worst_used.round() as u32),
                color: quota_color(worst_used),
                hint,
            })
        }
        crate::provider_quota::ProviderQuotaData::Copilot(quota) => {
            let (text, mut hint, used_percent) = if let Some(limit) = quota.limit {
                let used = quota.used.unwrap_or((limit - quota.remaining).max(0.0));
                let used_percent = (used / limit.max(1.0) * 100.0).clamp(0.0, 100.0);
                let used_pct = used_percent.round() as u32;
                (
                    format!("{used_pct}%"),
                    format!(
                        "{label} 本周期已用 {} / {} {}（{used_pct}%）。",
                        smelt_core::copilot_quota::format_amount(used),
                        smelt_core::copilot_quota::format_amount(limit),
                        quota.unit.label()
                    ),
                    used_percent,
                )
            } else {
                (
                    smelt_core::copilot_quota::format_amount(quota.remaining),
                    format!(
                        "{label} 本周期剩余 {} {}。",
                        smelt_core::copilot_quota::format_amount(quota.remaining),
                        quota.unit.label()
                    ),
                    0.0,
                )
            };
            if let Some(error) = &status.error {
                hint.push_str(&format!(
                    " {label} 额度刷新失败，仍显示上次成功结果：{error}。"
                ));
            }
            Some(ProviderQuotaRow {
                text,
                color: quota_color(used_percent),
                hint,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{effective_pane_updated_at, provider_quota_row, session_updated_at_text};
    use crate::provider_quota::{ProviderQuotaData, QuotaStatus};
    use crate::settings::ConversationAgentKind;
    use crate::ui_theme;
    use chrono::{Local, TimeZone};
    use smelt_core::copilot_quota::{CopilotQuota, CopilotQuotaUnit};
    use smelt_core::provider_quota::{ProviderQuota, ProviderQuotaWindow};

    #[test]
    fn window_quota_row_keeps_the_worst_window_summary() {
        let status = QuotaStatus {
            quota: Some(ProviderQuotaData::Windows(ProviderQuota {
                windows: vec![
                    ProviderQuotaWindow {
                        label: "5h".to_string(),
                        used_percent: 76.0,
                        resets_at_ms: None,
                        reset_after_seconds: Some(3_600),
                    },
                    ProviderQuotaWindow {
                        label: "7d".to_string(),
                        used_percent: 91.0,
                        resets_at_ms: None,
                        reset_after_seconds: None,
                    },
                ],
            })),
            checked_at_ms: 1,
            error: Some("offline".to_string()),
        };

        let row = provider_quota_row(ConversationAgentKind::Claude, &status).unwrap();

        assert_eq!(row.text, "91%");
        assert_eq!(row.color, ui_theme::red());
        assert!(row.hint.contains("Claude 5h 窗口已用 76%"));
        assert!(row.hint.contains("仍显示上次成功结果：offline"));
    }

    #[test]
    fn copilot_quota_row_keeps_its_amount_semantics() {
        let status = QuotaStatus {
            quota: Some(ProviderQuotaData::Copilot(CopilotQuota {
                unit: CopilotQuotaUnit::PremiumRequests,
                remaining: 75.0,
                limit: Some(100.0),
                used: Some(25.0),
            })),
            checked_at_ms: 1,
            error: None,
        };

        let row = provider_quota_row(ConversationAgentKind::Copilot, &status).unwrap();

        assert_eq!(row.text, "25%");
        assert_eq!(row.color, ui_theme::text_muted());
        assert!(row.hint.contains("25 / 100 premium requests（25%）"));
    }

    #[test]
    fn session_time_uses_compact_local_calendar_labels() {
        let now = Local
            .with_ymd_and_hms(2026, 8, 24, 15, 30, 0)
            .single()
            .unwrap();
        let today = Local
            .with_ymd_and_hms(2026, 8, 24, 14, 5, 0)
            .single()
            .unwrap();
        let yesterday = Local
            .with_ymd_and_hms(2026, 8, 23, 23, 15, 0)
            .single()
            .unwrap();
        let earlier = Local
            .with_ymd_and_hms(2026, 8, 1, 9, 30, 0)
            .single()
            .unwrap();
        let previous_year = Local
            .with_ymd_and_hms(2025, 12, 31, 9, 30, 0)
            .single()
            .unwrap();

        assert_eq!(
            session_updated_at_text(today.timestamp() as u64, &now),
            "今天 14:05"
        );
        assert_eq!(
            session_updated_at_text(yesterday.timestamp() as u64, &now),
            "昨天 23:15"
        );
        assert_eq!(
            session_updated_at_text(earlier.timestamp() as u64, &now),
            "08-01 09:30"
        );
        assert_eq!(
            session_updated_at_text(previous_year.timestamp() as u64, &now),
            "2025-12-31"
        );
        assert_eq!(session_updated_at_text(0, &now), "时间未知");
    }

    #[test]
    fn pane_time_prefers_its_own_activity_and_falls_back_to_the_session() {
        assert_eq!(effective_pane_updated_at(120, 240), 120);
        assert_eq!(effective_pane_updated_at(0, 240), 240);
    }
}
