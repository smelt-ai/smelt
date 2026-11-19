//! 账户额度那一条（[`contract::QUOTA_VIEW`]）：每家一枚额度环，双环外 5 小时、内周；总额未知
//! 时写剩余数字。悬停看各窗口和重置时间，点开看全部，点哪家刷新哪家。
//!
//! 只列智能体自己的账户（Codex、Claude、Copilot）。Pi 底下几家 provider 是独立账户，额度跟在
//! 对话模型菜单的 provider 名字后面，那里才说得清数字属于谁。
//!
//! 状态归 [`quota`]，这里不另存：变了（[`contract::QUOTA_CHANGED`]）就整份换掉手里那份再重画。
//! 摆在哪归布局。

use std::rc::Rc;

use api::model::provider_quota::{ProviderQuotaData, ProviderQuotaWindow};
use api::model::quota_service::{QuotaStatus, QuotaTarget};
use api::ui::popover::Popover;
use api::ui::truncated_line::truncated_line;
use api::ui::{Icon, IconName, ListRow, ui_theme};
use api::{HostEvent, PluginView, ViewHandle, ViewSpec};
use chrono::Local;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use rust_i18n::t;
use smelt_plugin_api as api;

use crate::contract::{self, Quota};
use crate::quota;
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

/// 点开的列表宽度：名字一栏、用量条、百分比放得下。
const POPOVER_WIDTH: f32 = 268.;

/// 用到这个百分比就算快用完：标黄。
const WARN_PERCENT: f64 = 75.0;

pub(crate) fn view_spec() -> ViewSpec {
    ViewSpec {
        name: contract::QUOTA_VIEW
            .split_once('/')
            .map_or(contract::QUOTA_VIEW, |(_, name)| name)
            .into(),
        title: api::text!("quota.title"),
        icon: IconName::Gauge.path(),
        // 一条额度，不要外壳的标题带。
        draws_top_bar: true,
        instances: false,
        settings: None,
        fills: Vec::new(),
        build: Rc::new(|_window, cx| Rc::new(cx.new(QuotaBar::new)) as Rc<dyn ViewHandle>),
    }
}

pub(crate) struct QuotaBar {
    /// [`quota`] 那份的只读缓存，render 只读它。
    quota: Quota,
    _subscriptions: Vec<Subscription>,
}

impl QuotaBar {
    fn new(cx: &mut Context<Self>) -> Self {
        let events = api::events(cx);
        let subscriptions = vec![cx.subscribe(&events, |this, _, event: &HostEvent, cx| {
            if let Some(quota) = contract::QUOTA_CHANGED.parse(event) {
                this.quota = quota;
                cx.notify();
            }
        })];
        Self {
            quota: quota::snapshot(cx),
            _subscriptions: subscriptions,
        }
    }

    /// 这一条列的账户，按展示顺序；还没查到的不列。
    fn chips(&self) -> Vec<Chip> {
        chips_for(&self.quota)
    }

    /// 弹层里的全部开启账户（含暂未查到或失败的），按展示顺序。
    fn popover_rows(&self) -> Vec<PopoverEntry> {
        popover_entries(&self.quota)
    }
}

fn chips_for(quota: &Quota) -> Vec<Chip> {
    quota
        .statuses
        .iter()
        .filter(|(target, _)| target.provider.is_none())
        .filter_map(|(target, status)| chip_for(quota, *target, status))
        .collect()
}

fn popover_entries(quota: &Quota) -> Vec<PopoverEntry> {
    quota
        .targets
        .iter()
        .copied()
        .filter(|target| target.provider.is_none())
        .map(|target| {
            let status = quota
                .statuses
                .iter()
                .find(|(candidate, _)| *candidate == target)
                .map(|(_, status)| status);
            if let Some(chip) = status.and_then(|status| chip_for(quota, target, status)) {
                return PopoverEntry::Ready(chip);
            }
            let error = status.and_then(|status| status.error.clone());
            PopoverEntry::Pending(PendingRow {
                target,
                refreshing: quota.refreshing.contains(&target),
                error,
            })
        })
        .collect()
}

fn chip_for(quota_state: &Quota, target: QuotaTarget, status: &QuotaStatus) -> Option<Chip> {
    let quota = status.quota.as_ref()?;
    let row = quota_row(target, status)?;
    let rings = ring_windows(quota);
    let hint = if rings.len() > 1 {
        t!(
            "quota.rings",
            hint = row.hint,
            outer = rings[0].0,
            inner = rings[1].0
        )
        .into_owned()
    } else {
        row.hint
    };
    Some(Chip {
        target,
        text: row.text,
        color: row.color,
        hint,
        used: quota.used_percent(),
        rings,
        refreshing: quota_state.manual.contains(&target),
    })
}

impl PluginView for QuotaBar {
    fn open(&mut self, _: &api::serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}

impl Render for QuotaBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chips = self.chips();
        let rows = self.popover_rows();
        // 自己就是一行：贴着窗口下面的角时两头给窗口圆角让位，不贴时留标准边距。
        let insets = api::safe_area(cx.entity_id(), cx).bar_insets();
        let bar = div()
            .size_full()
            .min_w_0()
            .pl(insets.left)
            .pr(insets.right)
            .flex()
            .items_center()
            .justify_end()
            .type_style(TYPE.text_xsmall)
            .font_family(api::model::font_config::font_family());
        if rows.is_empty() {
            // 所有账户都关掉了：查询中时给个过渡，不然空着。
            return bar.when(!self.quota.refreshing.is_empty(), |bar| {
                bar.text_color(rgb(ui_theme::faint()))
                    .child(t!("quota.loading").into_owned())
            });
        }
        // 能折行的触发器：按钮高度固定折不了行，用列表行（悬停、弹层开着时的底跟别的行一样）。
        let trigger = ListRow::new("quota-bar")
            .min_w_0()
            .px_1p5()
            .py_1()
            .rounded(ui_theme::row_radius())
            .flex()
            .flex_wrap()
            .justify_end()
            .items_center()
            .gap_x_2()
            .gap_y_1();
        let trigger = if chips.is_empty() {
            let loading = !self.quota.refreshing.is_empty();
            let tip = if loading {
                t!("quota.loading").into_owned()
            } else {
                t!("quota.empty_trigger").into_owned()
            };
            trigger
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_color(rgb(ui_theme::muted()))
                        .child(Icon::path(IconName::Gauge.path()).size(px(14.)))
                        .when(loading, |this| {
                            this.text_color(rgb(ui_theme::faint()))
                                .child(t!("quota.loading").into_owned())
                        }),
                )
                .tooltip(move |window, cx| {
                    api::ui::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                })
        } else {
            trigger.children(chips.iter().map(chip))
        };
        bar.child(
            Popover::new("quota-bar-popover")
                .anchor(Anchor::BottomRight)
                .w(px(POPOVER_WIDTH))
                .p_1()
                // 触发容器要能收窄，额度才会在里面折行，不把这一条撑宽。
                .trigger_style(StyleRefinement::default().min_w_0())
                .trigger(trigger)
                .content(move |_, _, _| popover(rows.clone())),
        )
    }
}

/// 一家账户的额度。
#[derive(Clone)]
struct Chip {
    target: QuotaTarget,
    text: String,
    color: u32,
    hint: String,
    used: f64,
    /// 从外到内的窗口和已用百分比；总额未知时为空，不把未知画成满额。
    rings: Vec<(String, f64)>,
    /// 用户手点刷新、还没回来。启动首查和定时刷新不打扰。
    refreshing: bool,
}

/// 弹层里未查到数据或查询失败的一行，点一下同样触发单查。
#[derive(Clone)]
struct PendingRow {
    target: QuotaTarget,
    refreshing: bool,
    error: Option<String>,
}

#[derive(Clone)]
enum PopoverEntry {
    Ready(Chip),
    Pending(PendingRow),
}

fn chip(quota: &Chip) -> impl IntoElement {
    let tip = if quota.refreshing {
        t!("quota.refreshing", hint = quota.hint).into_owned()
    } else {
        quota.hint.clone()
    };
    let icon_size = if quota.rings.len() > 1 { 9. } else { 11. };
    div()
        .flex()
        .items_center()
        .id(SharedString::from(format!(
            "quota-chip-{}",
            quota.target.id()
        )))
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .child(
            div()
                .relative()
                .size(px(26.))
                .flex()
                .items_center()
                .justify_center()
                .children((!quota.rings.is_empty()).then(|| rings(quota.rings.clone())))
                .text_color(rgb(ui_theme::text()))
                .map(|icon| {
                    if quota.refreshing {
                        icon.child(api::ui::motion::ambient_spinner(
                            SharedString::from(format!("quota-chip-refresh-{}", quota.target.id())),
                            rgb(quota.color).into(),
                            true,
                        ))
                    } else {
                        icon.child(Icon::path(quota.target.agent.icon_asset()).size(px(icon_size)))
                    }
                }),
        )
        .children(quota.rings.is_empty().then(|| quota.text.clone()))
        .tooltip(move |window, cx| api::ui::tooltip::Tooltip::new(tip.clone()).build(window, cx))
}

/// 额度环跟对话上下文环一样用路径绘制，长度表示已用量；双环依次是 5 小时、周。
fn rings(rings: Vec<(String, f64)>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let center_x = f32::from(bounds.origin.x + bounds.size.width / 2.0);
            let center_y = f32::from(bounds.origin.y + bounds.size.height / 2.0);
            let stroke = if rings.len() > 1 { 1.75_f32 } else { 2.0_f32 };
            let outer_radius = if rings.len() > 1 {
                12.125_f32
            } else {
                10.0_f32
            };
            for (index, (_, used)) in rings.iter().enumerate() {
                let radius = outer_radius - index as f32 * 3.5;
                let radii = point(px(radius), px(radius));
                let top = point(px(center_x), px(center_y - radius));
                let bottom = point(px(center_x), px(center_y + radius));
                let mut track = PathBuilder::stroke(px(stroke));
                track.move_to(top);
                track.arc_to(radii, px(0.), false, true, bottom);
                track.arc_to(radii, px(0.), false, true, top);
                if let Ok(path) = track.build() {
                    window.paint_path(path, ui_theme::hair());
                }

                let fraction = ring_fraction(*used);
                if fraction <= 0.0 {
                    continue;
                }
                let color = if *used >= 90.0 {
                    ui_theme::bad()
                } else if *used >= 50.0 {
                    ui_theme::need()
                } else {
                    ui_theme::ok()
                };
                let mut progress = PathBuilder::stroke(px(stroke));
                progress.move_to(top);
                let end = if fraction >= 0.999 {
                    progress.arc_to(radii, px(0.), false, true, bottom);
                    progress.arc_to(radii, px(0.), false, true, top);
                    None
                } else {
                    let theta = fraction * std::f32::consts::TAU;
                    let end = point(
                        px(center_x + radius * theta.sin()),
                        px(center_y - radius * theta.cos()),
                    );
                    progress.arc_to(radii, px(0.), fraction > 0.5, true, end);
                    Some(end)
                };
                if let Ok(path) = progress.build() {
                    window.paint_path(path, rgb(color));
                }
                // PathBuilder 的弧线端点是平的，补两个半径为半线宽的圆作圆头。
                if let Some(end) = end {
                    let cap_radius = px(stroke / 2.0);
                    for cap in [top, end] {
                        let mut dot = PathBuilder::fill();
                        dot.move_to(point(cap.x - cap_radius, cap.y));
                        dot.arc_to(
                            point(cap_radius, cap_radius),
                            px(0.),
                            false,
                            true,
                            point(cap.x + cap_radius, cap.y),
                        );
                        dot.arc_to(
                            point(cap_radius, cap_radius),
                            px(0.),
                            false,
                            true,
                            point(cap.x - cap_radius, cap.y),
                        );
                        if let Ok(path) = dot.build() {
                            window.paint_path(path, rgb(color));
                        }
                    }
                }
            }
        },
    )
    .absolute()
    .inset_0()
    .size_full()
}

/// 点开的列表：每家一行，名字、用量条、百分比。悬停看各窗口和重置时间，点哪家刷新哪家；
/// 暂未查到或失败的账户也列在其中，点一下即可主动查询。
fn popover(entries: Vec<PopoverEntry>) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .child(
            div()
                .h(ui_theme::height::sm())
                .px_2()
                .flex()
                .items_center()
                .type_style(TYPE.text_xsmall_medium)
                .text_color(rgb(ui_theme::muted()))
                .child(t!("quota.title").into_owned()),
        )
        .children(entries.into_iter().map(popover_row))
}

fn popover_row(entry: PopoverEntry) -> impl IntoElement {
    let (target, color, text, used, refreshing, tip, name_color) = match entry {
        PopoverEntry::Ready(quota) => {
            let tip = if quota.refreshing {
                t!("quota.refreshing", hint = quota.hint).into_owned()
            } else {
                t!("quota.click_to_refresh", hint = quota.hint).into_owned()
            };
            (
                quota.target,
                quota.color,
                quota.text,
                quota.used,
                quota.refreshing,
                tip,
                ui_theme::text(),
            )
        }
        PopoverEntry::Pending(pending) => {
            let tip = if pending.refreshing {
                t!("quota.refreshing", hint = pending.target.label()).into_owned()
            } else if let Some(error) = pending.error.as_deref() {
                t!("quota.click_to_retry", error = error).into_owned()
            } else {
                t!("quota.click_to_check").into_owned()
            };
            let color = if pending.error.is_some() {
                ui_theme::need()
            } else {
                ui_theme::faint()
            };
            (
                pending.target,
                color,
                "—".to_string(),
                0.0,
                pending.refreshing,
                tip,
                ui_theme::muted(),
            )
        }
    };
    ListRow::new(SharedString::from(format!("quota-{}", target.id())))
        .h(px(30.))
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(6.))
        .type_style(TYPE.text_xsmall)
        .child(
            Icon::path(target.agent.icon_asset())
                .size(px(14.))
                .text_color(rgb(color)),
        )
        .child(
            div()
                .w(px(64.))
                .flex_shrink_0()
                .overflow_hidden()
                .text_color(rgb(name_color))
                .child(truncated_line(target.label())),
        )
        .child(
            div()
                .flex_1()
                .h(px(4.))
                .rounded_full()
                .overflow_hidden()
                .bg(ui_theme::hair())
                .when(used > 0.0, |track| {
                    track.child(
                        div()
                            .h_full()
                            .w(relative((used / 100.0).clamp(0.0, 1.0) as f32))
                            .rounded_full()
                            .bg(rgb(color)),
                    )
                }),
        )
        // 点一下发起查询、还没回来时，右侧换成转圈：真在等，等完就停。
        .child(
            div()
                .w(px(36.))
                .flex_shrink_0()
                .flex()
                .justify_end()
                .font_family(api::model::font_config::font_family())
                .text_color(rgb(color))
                .map(|cell| {
                    if refreshing {
                        cell.child(api::ui::motion::ambient_spinner(
                            SharedString::from(format!("quota-refresh-{}", target.id())),
                            rgb(color).into(),
                            true,
                        ))
                    } else {
                        cell.child(text)
                    }
                }),
        )
        .tooltip(move |window, cx| api::ui::tooltip::Tooltip::new(tip.clone()).build(window, cx))
        // 刷新开始、结束都会发 `QUOTA_CHANGED`，这一条跟着重画。
        .on_click(move |_, _, cx| quota::request_refresh(target, cx))
}

/// 按已用百分比取字色：>=90% 红、>=75% 黄、否则灰（额度充足，不抢眼）。
fn quota_color(used_percent: f64) -> u32 {
    if used_percent >= 90.0 {
        ui_theme::bad()
    } else if used_percent >= WARN_PERCENT {
        ui_theme::need()
    } else {
        ui_theme::muted()
    }
}

/// 把相对秒数写成「X天X小时」这类；0 或负数（已重置、不足一分钟）是 `None`，调用方不写
/// 相对时间。
fn until_text(seconds: i64) -> Option<String> {
    let seconds = seconds.max(0);
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        if hours > 0 {
            Some(t!("duration.days_hours", days = days, hours = hours).into_owned())
        } else {
            Some(t!("duration.days", days = days).into_owned())
        }
    } else if hours > 0 {
        if minutes > 0 {
            Some(t!("duration.hours_minutes", hours = hours, minutes = minutes).into_owned())
        } else {
            Some(t!("duration.hours", hours = hours).into_owned())
        }
    } else if minutes > 0 {
        Some(t!("duration.minutes", minutes = minutes).into_owned())
    } else {
        None
    }
}

/// 窗口还有多久重置，只写剩余时间（「2天5小时后重置」）：悬停提示是一行，绝对时间占地方又
/// 不如剩余时间好用。不知道什么时候重置就不写。
fn reset_text(window: &ProviderQuotaWindow) -> Option<String> {
    let seconds = match (window.resets_at_ms, window.reset_after_seconds) {
        (Some(millis), _) => (millis as i64 - Local::now().timestamp_millis()) / 1000,
        (None, Some(seconds)) => seconds as i64,
        (None, None) => return None,
    };
    Some(match until_text(seconds) {
        Some(text) => t!("quota.resets_in", time = text).into_owned(),
        None => t!("quota.resets_soon").into_owned(),
    })
}

/// 彩色弧的长度表示已用比例。
fn ring_fraction(used_percent: f64) -> f32 {
    (used_percent.clamp(0.0, 100.0) / 100.0) as f32
}

/// 5 小时在外、周额度在内；没有标准窗口时只画最紧张的窗口。Copilot 不给总额时算不出比例，
/// 写剩余数字，不画假的额度环。
fn ring_windows(quota: &ProviderQuotaData) -> Vec<(String, f64)> {
    match quota {
        ProviderQuotaData::Windows(quota) => {
            let standard: Vec<_> = ["5h", "7d"]
                .into_iter()
                .filter_map(|label| quota.windows.iter().find(|window| window.label == label))
                .map(|window| (window.display_label(), window.used_percent))
                .collect();
            if !standard.is_empty() {
                standard
            } else {
                quota
                    .windows
                    .iter()
                    .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
                    .map(|window| vec![(window.display_label(), window.used_percent)])
                    .unwrap_or_default()
            }
        }
        ProviderQuotaData::Copilot(copilot) if copilot.limit.is_some() => {
            vec![(t!("quota.period").into_owned(), quota.used_percent())]
        }
        ProviderQuotaData::Copilot(_) => Vec::new(),
    }
}

/// 一家账户在这一条上的样子。
struct QuotaRow {
    text: String,
    color: u32,
    hint: String,
}

/// 两种额度模型归一成一行。行内数字和模型菜单里的括号走同一个口径
/// （`ProviderQuotaData::short_text`），两处不会对同一个账户给出不同的百分比。
fn quota_row(target: QuotaTarget, status: &QuotaStatus) -> Option<QuotaRow> {
    let label = target.label();
    let quota = status.quota.as_ref()?;
    let text = quota.short_text();
    let color = quota_color(quota.used_percent());
    // 悬停提示一行写完，像状态栏：名字写一次，窗口之间用「·」隔开，
    // 如「Claude 5h 76%（2小时后重置） · 7d 30%（4天后重置）」。
    let hint = match quota {
        ProviderQuotaData::Windows(quota) => {
            let windows = quota
                .windows
                .iter()
                .map(|window| {
                    let used = window.used_percent.round() as u32;
                    match reset_text(window) {
                        Some(reset) => t!(
                            "quota.window_reset",
                            window = window.display_label(),
                            used = used,
                            reset = reset
                        )
                        .into_owned(),
                        None => format!("{} {used}%", window.display_label()),
                    }
                })
                .collect::<Vec<_>>()
                .join(" · ");
            format!("{label} {windows}")
        }
        ProviderQuotaData::Copilot(copilot) => match copilot.limit {
            Some(limit) => {
                let used = copilot.used.unwrap_or((limit - copilot.remaining).max(0.0));
                t!(
                    "quota.copilot_used",
                    label = label,
                    used = api::model::copilot_quota::format_amount(used),
                    limit = api::model::copilot_quota::format_amount(limit),
                    unit = copilot.unit.label(),
                    percent = quota.used_percent().round() as u32
                )
                .into_owned()
            }
            None => t!(
                "quota.copilot_remaining",
                label = label,
                remaining = api::model::copilot_quota::format_amount(copilot.remaining),
                unit = copilot.unit.label()
            )
            .into_owned(),
        },
    };
    Some(QuotaRow { text, color, hint })
}

#[cfg(test)]
mod tests {
    use api::model::agent_kind::ConversationAgentKind;
    use api::model::copilot_quota::{CopilotQuota, CopilotQuotaUnit};
    use api::model::provider_quota::ProviderQuota;

    use super::*;
    // `super::*` 带进了 gpui 的 glob，其中的 `test` 宏会顶掉标准库的 `#[test]`，展开时无限递归。
    use std::prelude::rust_2024::test;

    fn window(label: &str, used_percent: f64) -> ProviderQuotaWindow {
        ProviderQuotaWindow {
            label: label.into(),
            used_percent,
            resets_at_ms: None,
            reset_after_seconds: None,
        }
    }

    #[test]
    fn ring_fraction_represents_used_quota() {
        assert_eq!(ring_fraction(0.0), 0.0);
        assert_eq!(ring_fraction(20.0), 0.2);
        assert_eq!(ring_fraction(80.0), 0.8);
        assert_eq!(ring_fraction(100.0), 1.0);
        assert_eq!(ring_fraction(150.0), 1.0);
    }

    #[test]
    fn rings_show_five_hours_outside_and_week_inside() {
        let quota = ProviderQuotaData::Windows(ProviderQuota {
            windows: vec![window("5h", 20.0), window("7d", 90.0)],
        });
        assert_eq!(
            ring_windows(&quota),
            vec![("5h".into(), 20.0), ("7d".into(), 90.0)]
        );
    }

    #[test]
    fn ring_uses_most_constrained_window_when_periods_are_not_standard() {
        let quota = ProviderQuotaData::Windows(ProviderQuota {
            windows: vec![window("month", 35.0), window("secondary", 75.0)],
        });
        assert_eq!(ring_windows(&quota), vec![("Secondary".into(), 75.0)]);
    }

    #[test]
    fn ring_does_not_invent_a_copilot_total() {
        let quota = ProviderQuotaData::Copilot(CopilotQuota {
            unit: CopilotQuotaUnit::PremiumRequests,
            remaining: 75.0,
            limit: None,
            used: None,
        });
        assert!(ring_windows(&quota).is_empty());
    }

    #[test]
    fn window_quota_row_keeps_the_worst_window_summary() {
        let status = QuotaStatus {
            quota: Some(ProviderQuotaData::Windows(ProviderQuota {
                windows: vec![
                    ProviderQuotaWindow {
                        reset_after_seconds: Some(3_600),
                        ..window("5h", 76.0)
                    },
                    window("7d", 91.0),
                ],
            })),
            checked_at_ms: 1,
            error: None,
        };

        let row = quota_row(QuotaTarget::agent(ConversationAgentKind::Claude), &status).unwrap();

        assert_eq!(row.text, "91%");
        assert_eq!(row.color, ui_theme::bad());
        assert_eq!(row.hint, "Claude 5h 76% (resets in 1h) · 7d 91%");
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

        let row = quota_row(QuotaTarget::agent(ConversationAgentKind::Copilot), &status).unwrap();

        assert_eq!(row.text, "25%");
        assert_eq!(row.color, ui_theme::muted());
        assert_eq!(
            row.hint,
            "Copilot this period 25 / 100 premium requests (25%)"
        );
    }

    #[test]
    fn until_text_reads_largest_units_first() {
        assert_eq!(until_text(0), None);
        assert_eq!(until_text(-5), None);
        assert_eq!(until_text(90).as_deref(), Some("1m"));
        assert_eq!(until_text(3_600).as_deref(), Some("1h"));
        assert_eq!(until_text(3_660).as_deref(), Some("1h 1m"));
        assert_eq!(until_text(86_400 + 7_200).as_deref(), Some("1d 2h"));
    }

    #[test]
    fn popover_entries_include_unfetched_and_failed_targets_while_bar_chips_stay_ready_only() {
        let codex = QuotaTarget::agent(ConversationAgentKind::Codex);
        let agy = QuotaTarget::agent(ConversationAgentKind::Antigravity);
        let grok = QuotaTarget::agent(ConversationAgentKind::Grok);
        let pi = QuotaTarget::pi("openrouter");
        let quota = Quota {
            targets: vec![codex, agy, grok, pi],
            statuses: vec![
                (
                    codex,
                    QuotaStatus {
                        quota: Some(ProviderQuotaData::Windows(ProviderQuota {
                            windows: vec![window("5h", 40.0)],
                        })),
                        checked_at_ms: 1,
                        error: None,
                    },
                ),
                (
                    agy,
                    QuotaStatus {
                        quota: None,
                        checked_at_ms: 2,
                        error: Some("HTTP 401".into()),
                    },
                ),
            ],
            refreshing: vec![agy],
            manual: vec![agy],
        };

        let chips = chips_for(&quota);
        assert_eq!(chips.len(), 1);
        assert_eq!(chips[0].target, codex);

        let entries = popover_entries(&quota);
        assert_eq!(entries.len(), 3, "Pi provider 不进左下角弹层");
        assert!(matches!(&entries[0], PopoverEntry::Ready(c) if c.target == codex));
        assert!(matches!(
            &entries[1],
            PopoverEntry::Pending(p)
                if p.target == agy && p.refreshing && p.error.as_deref() == Some("HTTP 401")
        ));
        assert!(matches!(
            &entries[2],
            PopoverEntry::Pending(p)
                if p.target == grok && !p.refreshing && p.error.is_none()
        ));
    }
}
