//! 资源页怎么画。数都在 [`PanelSnapshot`] 里拼好了，这里只读。

use std::collections::HashSet;
use std::rc::Rc;

use api::RuntimeKey;
use api::model::control_api::{ProcessEntry, ProcessUsage};
use api::ui::chart::AreaChart;
use api::ui::truncated_line::truncated_line;
use api::ui::ui_theme;
use api::ui::{Button, Icon, IconAction as _, IconName, ListRow, SettingsPage, SettingsSection};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_base::StyledExt as _;
use smelt_plugin_api as api;

use crate::ResourcesPage;
use crate::monitor::{
    MemoryPart, PanelSnapshot, Row, Section, SessionGroup, SortBy, format_bytes, format_cpu,
    process_summary,
};
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

const CPU_COLUMN: f32 = 64.;
const MEMORY_COLUMN: f32 = 80.;

/// 资源页。
pub(crate) fn page(
    page: Entity<ResourcesPage>,
    snapshot: Rc<PanelSnapshot>,
    sort: SortBy,
    expanded: Rc<HashSet<SharedString>>,
) -> SettingsPage {
    // 它是监视器不是设置：总量卡片标题旁写刷新频率，口径说明放到卡片底部。
    SettingsPage::new("resources-settings")
        .child(
            SettingsSection::new()
                .title("本机合计")
                .description("停在这一页时每 2 秒刷新")
                .child(summary_view(&snapshot)),
        )
        .children(snapshot.sections.iter().map(|section| {
            SettingsSection::new()
                .title(section.name.clone())
                .child(section_view(section, sort, &expanded, &page))
        }))
}

/// 本机合计、占比和最近两分钟的内存曲线。
fn summary_view(snapshot: &PanelSnapshot) -> AnyElement {
    let Some(local) = snapshot.local else {
        return div()
            .type_style(TYPE.ui)
            .text_color(rgb(ui_theme::muted()))
            .child("读取中…")
            .into_any_element();
    };
    let share = snapshot.total_memory.map(|total| {
        format!(
            "占本机 {} 内存的 {:.0}%",
            format_bytes(total),
            local.memory_bytes as f64 * 100. / total as f64
        )
    });
    let mut view = div().flex().flex_col().w_full().gap_2().child(
        div()
            .flex()
            .items_center()
            .gap_3()
            .items_baseline()
            .font_features(ui_theme::tabular_figures())
            .child(
                div()
                    .text_2xl()
                    .font_semibold()
                    .text_color(rgb(ui_theme::text()))
                    .child(format_bytes(local.memory_bytes)),
            )
            .child(
                div()
                    .type_style(TYPE.ui)
                    .text_color(rgb(ui_theme::text()))
                    .child(format!("CPU {}", format_cpu(local.cpu_percent))),
            )
            .children(share.map(|share| {
                div()
                    .type_style(TYPE.ui)
                    .text_color(rgb(ui_theme::muted()))
                    .child(share)
            })),
    );
    if snapshot.chart.len() > 1 {
        let peak = snapshot.chart.iter().copied().fold(0., f64::max);
        view = view.child(
            div()
                .h(px(96.))
                .w_full()
                .child(AreaChart::new(snapshot.chart.iter().copied()).max(peak * 1.2)),
        );
    }
    view.child(
        div()
            .type_style(TYPE.text_xsmall)
            .text_color(rgb(ui_theme::muted()))
            .child(
                "含 Smelt 界面、后台服务和本机会话，曲线最多两分钟。会话按整棵进程树计，\
                 包括智能体起的命令；CPU 按单核计，多核可超过 100%。",
            ),
    )
    .into_any_element()
}

/// 一台机器的表：列头（点 CPU、内存按它排）、固定行、会话、侧栏之外的会话、合计。
fn section_view(
    section: &Section,
    sort_by: SortBy,
    open: &HashSet<SharedString>,
    page: &Entity<ResourcesPage>,
) -> AnyElement {
    let colors = RowColors {
        fg: rgb(ui_theme::text()).into(),
        muted: rgb(ui_theme::muted()).into(),
    };
    let sort_header = |label: &'static str, by: SortBy, width: f32| {
        let page = page.clone();
        div()
            .id(SharedString::from(format!("{}-sort-{label}", section.id_prefix)))
            .w(px(width))
            .text_right()
            .text_color(if sort_by == by { colors.fg } else { colors.muted })
            .when(sort_by == by, |d| d.font_weight(TYPE.ui_medium.weight))
            .on_click(move |_ev, _window, cx| {
                page.update(cx, |page, cx| page.set_sort(by, cx));
            })
            // 当前按哪列排，在列头标出来：只靠颜色深浅分不出来。
            .child(if sort_by == by {
                format!("{label} ↓")
            } else {
                label.to_string()
            })
    };
    let mut table = div().flex().flex_col().w_full().child(
        div()
            .flex()
            .items_center()
            .px_2()
            .pb_1()
            .gap_3()
            .type_style(TYPE.text_xsmall)
            .child(div().w(px(DISCLOSURE_COLUMN)))
            .child(div().flex_1().text_color(colors.muted).child("名称"))
            .child(sort_header("CPU", SortBy::Cpu, CPU_COLUMN))
            .child(sort_header("内存", SortBy::Memory, MEMORY_COLUMN)),
    );
    if let Some(note) = &section.note {
        table = table.child(
            div()
                .flex()
                .items_center()
                .px_2()
                .py_1()
                .gap_3()
                .child(div().flex_none().w(px(DISCLOSURE_COLUMN)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .type_style(TYPE.ui)
                        .text_color(colors.muted)
                        .child(note.clone()),
                ),
        );
    }
    for (ix, row) in section.fixed.iter().enumerate() {
        let id = SharedString::from(format!("{}-fixed-{ix}", section.id_prefix));
        let is_open = open.contains(&id);
        table = table.child(fixed_row(id, row, is_open, page, colors));
    }
    let mut sessions: Vec<&SessionGroup> = section.sessions.iter().collect();
    sort_by.sort(&mut sessions);
    for (ix, group) in sessions.into_iter().chain(&section.others).enumerate() {
        let id = SharedString::from(match &group.jump {
            Some(jump) => format!("{}-{jump}", section.id_prefix),
            None => format!("{}-others-{ix}", section.id_prefix),
        });
        let is_open = open.contains(&id);
        table = table.child(session_row(
            id,
            &section.machine,
            group,
            is_open,
            page,
            colors,
        ));
    }
    if let Some(total) = section.total {
        table = table.child(
            div()
                .flex()
                .items_center()
                .mt_1()
                .px_2()
                .pt_1()
                .gap_3()
                .border_t_1()
                .border_color(ui_theme::hair())
                .child(div().w(px(DISCLOSURE_COLUMN)))
                .child(
                    div()
                        .flex_1()
                        .type_style(TYPE.ui_medium)
                        .text_color(colors.fg)
                        .child("合计"),
                )
                .child(usage_columns(total, colors.fg)),
        );
    }
    table.into_any_element()
}

/// 展开箭头那一列的宽度。没有明细的行也空出这一列，名字才对得齐。
const DISCLOSURE_COLUMN: f32 = 20.;

#[derive(Clone, Copy)]
struct RowColors {
    fg: Hsla,
    muted: Hsla,
}

/// 一行右边的两列数字。
fn usage_columns(usage: ProcessUsage, color: Hsla) -> Div {
    div()
        .flex()
        .items_center()
        .flex_none()
        .font_features(ui_theme::tabular_figures())
        .type_style(TYPE.ui)
        .text_color(color)
        .child(
            div()
                .w(px(CPU_COLUMN))
                .text_right()
                .child(format_cpu(usage.cpu_percent)),
        )
        .child(
            div()
                .w(px(MEMORY_COLUMN))
                .text_right()
                .child(format_bytes(usage.memory_bytes)),
        )
}

fn row_label(label: String, detail: Option<String>, colors: RowColors) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .child(
            div()
                .overflow_hidden()
                .type_style(TYPE.ui)
                .text_color(colors.fg)
                .child(truncated_line(label)),
        )
        .children(detail.map(|detail| {
            div()
                .overflow_hidden()
                .type_style(TYPE.text_xsmall)
                .text_color(colors.muted)
                .child(truncated_line(detail))
        }))
}

/// 桌面进程、守护进程：单个进程。有内存拆分的点整行展开或收起。
fn fixed_row(
    id: SharedString,
    row: &Row,
    is_open: bool,
    page: &Entity<ResourcesPage>,
    colors: RowColors,
) -> AnyElement {
    let label = row_label(row.label.clone(), row.detail.clone(), colors);
    if row.parts.is_empty() {
        return div()
            .flex()
            .items_center()
            .px_2()
            .py_1()
            .gap_3()
            .child(div().w(px(DISCLOSURE_COLUMN)))
            .child(label)
            .child(usage_columns(row.usage, colors.fg))
            .into_any_element();
    }
    let head = ListRow::new(ElementId::from(id.clone()))
        .px_2()
        .py_1()
        .gap_3()
        .flex()
        .items_center()
        .rounded(ui_theme::row_radius())
        .on_click(toggle_row(page, id))
        .child(disclosure(is_open, colors))
        .child(label)
        .child(usage_columns(row.usage, colors.fg));
    if !is_open {
        return head.into_any_element();
    }
    let body = div().flex().flex_col()
        // 和名字对齐：行内边距之后是箭头列，再隔一个列间距。
        .pl(px(DISCLOSURE_COLUMN + 12.))
        .pb_1()
        .children(row.parts.iter().map(|part| memory_part_row(part, colors)));
    div()
        .flex()
        .flex_col()
        .child(head)
        .child(body)
        .into_any_element()
}

/// 明细里的一块内存：名字、它装的是什么，只有内存一列。
fn memory_part_row(part: &MemoryPart, colors: RowColors) -> impl IntoElement + use<> {
    div()
        .flex()
        .items_center()
        .px_2()
        .py(px(3.))
        .gap_3()
        .child(row_label(
            part.label.to_string(),
            Some(part.detail.to_string()),
            colors,
        ))
        .child(
            div()
                .flex()
                .items_center()
                .flex_none()
                .font_features(ui_theme::tabular_figures())
                .type_style(TYPE.ui)
                .text_color(colors.muted)
                .child(div().w(px(CPU_COLUMN)))
                .child(
                    div()
                        .w(px(MEMORY_COLUMN))
                        .text_right()
                        .child(format_bytes(part.bytes)),
                ),
        )
}

/// 点一下展开、再点收起。
fn toggle_row(
    page: &Entity<ResourcesPage>,
    id: SharedString,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let page = page.clone();
    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
        page.update(cx, |page, cx| page.toggle_row(&id, cx));
    }
}

/// 展开箭头。
fn disclosure(is_open: bool, colors: RowColors) -> impl IntoElement + use<> {
    div()
        .w(px(DISCLOSURE_COLUMN))
        .flex_none()
        .text_color(colors.muted)
        .child(Icon::new(if is_open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        }))
}

/// 一行会话。点整行展开或收起它的进程；右边的按钮跳回主窗口的这个会话。
fn session_row(
    id: SharedString,
    machine: &RuntimeKey,
    group: &SessionGroup,
    is_open: bool,
    page: &Entity<ResourcesPage>,
    colors: RowColors,
) -> AnyElement {
    let summary = process_summary(group.usage.process_count, &group.processes);
    let detail = match group.jump {
        Some(_) => summary,
        None => format!("自动化、子智能体或别的客户端开的 · {summary}"),
    };
    let jump = group.jump.clone().map(|session_id| {
        let machine = machine.clone();
        // 按下就截住：不然这一下也会落到外面那一行上，顺手把它展开了。
        div()
            .flex_none()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new(SharedString::from(format!("{id}-jump")))
                    .quiet()
                    .small()
                    .icon(IconName::ExternalLink)
                    .icon_action("跳到会话")
                    .on_click(move |_, _, cx| {
                        crate::jump_to_session(machine.clone(), session_id.clone(), cx)
                    }),
            )
    });
    let head = ListRow::new(ElementId::from(id.clone()))
        .px_2()
        .py_1()
        .gap_3()
        .flex()
        .items_center()
        .rounded(ui_theme::row_radius())
        .on_click(toggle_row(page, id.clone()))
        .child(disclosure(is_open, colors))
        .child(row_label(group.title.clone(), Some(detail), colors))
        .children(jump)
        .child(usage_columns(group.usage, colors.fg));
    if !is_open {
        return head.into_any_element();
    }
    let mut body = div().flex().flex_col()
        // 和会话名对齐：行内边距之后是箭头列，再隔一个列间距。
        .pl(px(DISCLOSURE_COLUMN + 12.))
        .pb_1()
        .children(group.processes.iter().map(|process| process_row(process, colors)));
    let unlisted = (group.usage.process_count as usize).saturating_sub(group.processes.len());
    if unlisted > 0 {
        body = body.child(
            div()
                .px_2()
                .py_1()
                .type_style(TYPE.text_xsmall)
                .text_color(colors.muted)
                .child(format!("另有 {unlisted} 个占用更小的进程没列出")),
        );
    }
    div()
        .flex()
        .flex_col()
        .child(head)
        .child(body)
        .into_any_element()
}

/// 明细里的一个进程：名字和 pid，下面一行是命令行。
fn process_row(process: &ProcessEntry, colors: RowColors) -> impl IntoElement + use<> {
    let command = (!process.command.is_empty()).then(|| process.command.clone());
    div()
        .flex()
        .items_center()
        .px_2()
        .py(px(3.))
        .gap_3()
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .items_baseline()
                        .child(
                            div()
                                .overflow_hidden()
                                .type_style(TYPE.ui)
                                .text_color(colors.fg)
                                .child(truncated_line(process.name.clone())),
                        )
                        .child(
                            div()
                                .flex_none()
                                .type_style(TYPE.text_xsmall)
                                .font_features(ui_theme::tabular_figures())
                                .text_color(colors.muted)
                                .child(format!("pid {}", process.pid)),
                        ),
                )
                .children(command.map(|command| {
                    div()
                        .overflow_hidden()
                        .type_style(TYPE.text_xsmall)
                        .font_family(api::model::font_config::font_family())
                        .text_color(colors.muted)
                        .child(truncated_line(command))
                })),
        )
        .child(usage_columns(process.usage, colors.muted))
}
