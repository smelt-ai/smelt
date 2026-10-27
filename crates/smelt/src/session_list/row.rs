//! 会话行展示所需的图标、状态、外部链接与额度文案。

use chrono::{DateTime, Datelike, Local, TimeZone};

use super::*;
use smelt_client::daemon_state::{TerminalProgress, TerminalProgressState};
use smelt_model::agent_kind::TerminalAgentKind;

/// agent 身份图标统一走本地单色 SVG，颜色由会话状态驱动：图标同时回答
/// 「这是哪家的会话」和「当前处于什么状态」。
pub(super) fn provider_icon(agent: Option<TerminalAgentKind>) -> Icon {
    match agent {
        Some(agent) => Icon::empty().path(agent.icon_asset()),
        None => Icon::new(IconName::SquareTerminal),
    }
}

/// ACP agent 的图标。两张表不是同一个集合，所以直接按稳定存档 id 取资源（`smelt-icons/agent-<id>.svg`）。
pub(super) fn acp_provider_icon(agent: ConversationAgentKind) -> Icon {
    Icon::empty().path(agent.icon_asset())
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

/// 进度条填满的比例：没有数值的出错画满一条，其余按百分比。不确定进度不画条（`None`），
/// 见 [`session_progress_bar`]。
pub(super) fn progress_fraction(progress: TerminalProgress) -> Option<f32> {
    match (progress.state, progress.percent) {
        (TerminalProgressState::Indeterminate, _) => None,
        (TerminalProgressState::Error, None | Some(0)) => Some(1.0),
        (_, percent) => Some(f32::from(percent.unwrap_or(0).min(100)) / 100.0),
    }
}

/// 会话行底部的细进度条：终端程序用 OSC 9;4 报的进度。用组件库的 `Progress`，颜色
/// 跟主题的进度条色走；出错、暂停换成危险、警告色。
///
/// 不确定进度不画：别家终端拿动画表达它，侧栏常驻不开动画（会让整个工作台不停重绘），
/// 静止的一条空轨道又像行间分隔线，让人错认选中的是哪一行。程序在忙已经由会话图标说了。
pub(super) fn session_progress_bar(ui_id: u64, progress: TerminalProgress) -> Option<Div> {
    let fraction = progress_fraction(progress)?;
    let bar = progress::Progress::new(("session-progress", ui_id))
        .with_size(px(2.))
        .value(fraction * 100.);
    let bar = match progress.state {
        TerminalProgressState::Normal | TerminalProgressState::Indeterminate => bar,
        TerminalProgressState::Error => bar.color(rgb(ui_theme::red())),
        TerminalProgressState::Paused => bar.color(rgb(ui_theme::yellow())),
    };
    Some(
        div()
            .absolute()
            .left(px(8.))
            .right(px(8.))
            .bottom_0()
            .child(bar),
    )
}

/// 侧栏行尾的最近活动时间。用不会随分钟流逝而过期的本地日历文案，侧栏不必为它
/// 定时重画：今天只写时分，昨天写「昨天」，今年更早的写月日，再早的写年月日；旧存档
/// 没有时间时写「未知」。行尾一格放不下「今天 14:05」这种长写法。
pub(crate) fn session_updated_at_text(timestamp: u64, now: &DateTime<Local>) -> String {
    if timestamp == 0 {
        return "未知".to_string();
    }
    let Some(updated) = Local
        .timestamp_opt(timestamp.min(i64::MAX as u64) as i64, 0)
        .single()
    else {
        return "未知".to_string();
    };
    match (now.date_naive() - updated.date_naive()).num_days() {
        days if days <= 0 => updated.format("%H:%M").to_string(),
        1 => "昨天".to_string(),
        _ if updated.year() == now.year() => updated.format("%m-%d").to_string(),
        _ => updated.format("%Y-%m-%d").to_string(),
    }
}

/// 键盘焦点环：盖在行上的一圈描边，不占排版，前后的行不会挪。
pub(crate) fn focus_ring() -> Div {
    div()
        .absolute()
        .inset_0()
        .rounded(ui_theme::row_radius())
        .border_2()
        .border_color(rgb(ui_theme::border_focus()))
}

/// 刚新建那一行闪多久。
const NEW_ROW_FLASH_MS: u64 = 600;

/// 刚新建的那一行：铺一层强调色再淡回，让人看见新东西落在了哪。只播一次，播完是
/// 透明的；放在行的第一个子元素，压在图标和文字下面。
pub(crate) fn new_row_flash(id: impl Into<ElementId>) -> impl IntoElement {
    ambient_animation(id, Duration::from_millis(NEW_ROW_FLASH_MS), true, |phase| {
        div()
            .absolute()
            .inset_0()
            .rounded(ui_theme::row_radius())
            .bg(rgb(ui_theme::accent()))
            .opacity(0.28 * (1.0 - phase))
    })
}

/// 行内改名的输入框，占标题那一格。回车、点别处保存（见 `start_rename` 里的订阅），
/// Esc 取消：输入框自己用不上 Esc 时会往上传，在这里接住。按下不往行上传，免得点进
/// 输入框就起拖拽或切会话。
pub(crate) fn inline_rename_input(
    input: &Entity<gpui_component::input::InputState>,
    workspace: Entity<Workspace>,
) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_action(move |_: &gpui_component::input::Escape, _window, cx| {
            workspace.update(cx, |ws, cx| ws.cancel_rename(cx));
        })
        .child(gpui_component::input::Input::new(input).small())
}

/// 分组行开头那一格的宽度。项目、「对话」两种分组共用，下面会话的引导线对着它的中线。
pub(crate) const GROUP_ICON_BOX: f32 = 14.;

/// 分组箭头转一次多久。
const DISCLOSURE_TURN_MS: u64 = 120;

/// 分组行开头的图标：平时是分组自己的图标（文件夹、对话气泡），悬停整行时换成展开 /
/// 收起箭头，告诉人点这一行会折起来。分组空着、没东西可折时只有图标。两个图标叠在
/// 同一格里只切透明度，换的时候后面的名字不动。
///
/// 箭头收起时朝右、展开时朝下，开合时转过去。动画的 id 带着开合状态：一换状态就是
/// 一个新动画，从另一头转过来；转完不再要帧。
pub(crate) fn group_disclosure(
    id: impl Into<ElementId>,
    group: &'static str,
    icon: Icon,
    collapsed: bool,
    collapsible: bool,
) -> Stateful<Div> {
    let id = id.into();
    let turn_id = ElementId::from((id.clone(), if collapsed { "collapsed" } else { "expanded" }));
    div()
        .id(id)
        .relative()
        .w(px(GROUP_ICON_BOX))
        .h(px(16.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(ui_theme::text_muted()))
        .group_hover(group, |style| {
            style.text_color(rgb(ui_theme::text_bright()))
        })
        .child(
            div()
                .flex()
                .items_center()
                .when(collapsible, |d| {
                    d.group_hover(group, |style| style.opacity(0.0))
                })
                .child(icon),
        )
        .when(collapsible, |d| {
            d.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .opacity(0.0)
                    .group_hover(group, |style| style.opacity(1.0))
                    .child(ambient_animation(
                        turn_id,
                        Duration::from_millis(DISCLOSURE_TURN_MS),
                        true,
                        move |phase| {
                            let (from, to) = if collapsed { (90.0, 0.0) } else { (0.0, 90.0) };
                            let degrees: f32 =
                                from + (to - from) * smelt_ui::motion::ease_snap(phase);
                            Icon::new(IconName::ChevronRight)
                                .size(px(14.))
                                .transform(Transformation::rotate(radians(degrees.to_radians())))
                        },
                    )),
            )
        })
}

/// 行尾一格：平时写时间或要人管的状态，悬停这一行时同一格换成关闭键。两样叠在一起，
/// 换的时候这一格不变宽，标题不跟着伸缩。`group` 是整行的 hover group 名。
pub(crate) fn row_trailing_slot(
    group: &'static str,
    meta: Option<(SharedString, Hsla)>,
    close: Button,
) -> Div {
    div()
        .relative()
        .flex_shrink_0()
        .min_w(px(20.))
        .h(px(20.))
        .flex()
        .items_center()
        .justify_end()
        .children(meta.map(|(text, color)| {
            div()
                .text_xs()
                .text_color(color)
                .group_hover(group, |style| style.opacity(0.0))
                .child(text)
        }))
        .child(
            div()
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .flex()
                .items_center()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    close
                        .opacity(0.0)
                        .group_hover(group, |style| style.opacity(1.0)),
                ),
        )
}

/// 分屏优先展示自己的守护更新时间；守护镜像尚未到达时，回退到所属会话时间。
pub(super) fn effective_pane_updated_at(pane_updated_at: u64, session_updated_at: u64) -> u64 {
    if pane_updated_at == 0 {
        session_updated_at
    } else {
        pane_updated_at
    }
}

/// 用到这个百分比就算快用完：标黄，并且在底栏常显。
pub(super) const QUOTA_WARN_PERCENT: f64 = 75.0;

/// 按已用百分比取行内颜色：>=90% 红、>=75% 黄、否则灰（额度充足，默认不抢眼）。
pub(super) fn quota_color(used_percent: f64) -> u32 {
    if used_percent >= 90.0 {
        ui_theme::red()
    } else if used_percent >= QUOTA_WARN_PERCENT {
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

/// 窗口还有多久重置，只写剩余时间（「2天5小时后重置」）：悬停提示是一行，绝对时间
/// 占地方又不如剩余时间好用。不知道什么时候重置就不写。
pub(super) fn quota_reset_text(
    window: &smelt_model::provider_quota::ProviderQuotaWindow,
) -> Option<String> {
    let seconds = match (window.resets_at_ms, window.reset_after_seconds) {
        (Some(millis), _) => (millis as i64 - Local::now().timestamp_millis()) / 1000,
        (None, Some(seconds)) => seconds as i64,
        (None, None) => return None,
    };
    Some(match quota_until_text(seconds) {
        Some(text) => format!("{text}后重置"),
        None => "即将重置".to_string(),
    })
}

/// 底栏上一家 provider 的额度。
#[derive(Clone)]
pub(super) struct FooterQuota {
    pub(super) target: crate::provider_quota::QuotaTarget,
    pub(super) text: String,
    pub(super) color: u32,
    pub(super) hint: String,
    pub(super) used: f64,
    /// 用户手点刷新、还没回来。只有这种在百分比的位置转圈；启动首查和定时刷新不打扰。
    pub(super) refreshing: bool,
}

pub(super) struct ProviderQuotaRow {
    pub(super) text: String,
    pub(super) color: u32,
    pub(super) hint: String,
}

/// 把两种 provider 额度模型归一成侧栏的一行。注册表负责“查哪家”，这里仅负责
/// “怎么显示”，Workspace 渲染无需按 Agent 身份重复展开状态。
///
/// 行内数字和模型菜单里的括号走同一个口径（`ProviderQuotaData::short_text`），
/// 两处不会对同一个账户给出不同的百分比。
pub(super) fn provider_quota_row(
    target: crate::provider_quota::QuotaTarget,
    status: &crate::provider_quota::QuotaStatus,
) -> Option<ProviderQuotaRow> {
    let label = target.label();
    let quota = status.quota.as_ref()?;
    let text = quota.short_text();
    let color = quota_color(quota.used_percent());
    // 悬停提示一行写完，像状态栏：名字写一次，窗口之间用「·」隔开，
    // 如「Claude 5h 76%（2小时后重置） · 7d 30%（4天后重置）」。
    let hint = match quota {
        crate::provider_quota::ProviderQuotaData::Windows(quota) => {
            let windows = quota
                .windows
                .iter()
                .map(|window| {
                    let used = window.used_percent.round() as u32;
                    match quota_reset_text(window) {
                        Some(reset) => format!("{} {used}%（{reset}）", window.label),
                        None => format!("{} {used}%", window.label),
                    }
                })
                .collect::<Vec<_>>()
                .join(" · ");
            format!("{label} {windows}")
        }
        crate::provider_quota::ProviderQuotaData::Copilot(copilot) => match copilot.limit {
            Some(limit) => {
                let used = copilot.used.unwrap_or((limit - copilot.remaining).max(0.0));
                format!(
                    "{label} 本周期 {} / {} {}（{}%）",
                    smelt_model::copilot_quota::format_amount(used),
                    smelt_model::copilot_quota::format_amount(limit),
                    copilot.unit.label(),
                    quota.used_percent().round() as u32
                )
            }
            None => format!(
                "{label} 本周期剩余 {} {}",
                smelt_model::copilot_quota::format_amount(copilot.remaining),
                copilot.unit.label()
            ),
        },
    };
    Some(ProviderQuotaRow { text, color, hint })
}

#[cfg(test)]
mod tests {
    use super::{
        effective_pane_updated_at, progress_fraction, provider_quota_row, session_updated_at_text,
    };
    use crate::provider_quota::{ProviderQuotaData, QuotaStatus, QuotaTarget};
    use crate::settings::ConversationAgentKind;
    use crate::ui_theme;
    use chrono::{Local, TimeZone};
    use smelt_client::daemon_state::{TerminalProgress, TerminalProgressState};
    use smelt_model::copilot_quota::{CopilotQuota, CopilotQuotaUnit};
    use smelt_model::provider_quota::{ProviderQuota, ProviderQuotaWindow};

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
            error: None,
        };

        let row =
            provider_quota_row(QuotaTarget::agent(ConversationAgentKind::Claude), &status).unwrap();

        assert_eq!(row.text, "91%");
        assert_eq!(row.color, ui_theme::red());
        assert_eq!(row.hint, "Claude 5h 76%（1小时后重置） · 7d 91%");
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

        let row = provider_quota_row(QuotaTarget::agent(ConversationAgentKind::Copilot), &status)
            .unwrap();

        assert_eq!(row.text, "25%");
        assert_eq!(row.color, ui_theme::text_muted());
        assert_eq!(row.hint, "Copilot 本周期 25 / 100 premium requests（25%）");
    }

    #[test]
    fn progress_bar_fills_by_percent_and_skips_unknown() {
        let bar = |state, percent| progress_fraction(TerminalProgress { state, percent });
        let close = |a: Option<f32>, b: f32| a.is_some_and(|a| (a - b).abs() < 1e-6);
        assert!(close(bar(TerminalProgressState::Normal, Some(42)), 0.42));
        assert!(close(bar(TerminalProgressState::Paused, Some(100)), 1.0));
        assert_eq!(
            bar(TerminalProgressState::Indeterminate, Some(60)),
            None,
            "不确定进度不画条，空轨道会被看成分隔线"
        );
        assert!(
            close(bar(TerminalProgressState::Error, None), 1.0),
            "出错没数值也要看得见"
        );
        assert!(close(bar(TerminalProgressState::Error, Some(30)), 0.3));
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
            "14:05"
        );
        assert_eq!(
            session_updated_at_text(yesterday.timestamp() as u64, &now),
            "昨天"
        );
        assert_eq!(
            session_updated_at_text(earlier.timestamp() as u64, &now),
            "08-01"
        );
        assert_eq!(
            session_updated_at_text(previous_year.timestamp() as u64, &now),
            "2025-12-31"
        );
        assert_eq!(session_updated_at_text(0, &now), "未知");
    }

    #[test]
    fn pane_time_prefers_its_own_activity_and_falls_back_to_the_session() {
        assert_eq!(effective_pane_updated_at(120, 240), 120);
        assert_eq!(effective_pane_updated_at(0, 240), 240);
    }
}
