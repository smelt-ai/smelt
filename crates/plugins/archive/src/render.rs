//! 归档按钮和它的弹出卡片：机器一节（挂着几台机器才分）、项目一组、对话一行。只读视图上
//! 算好的分组。

use gpui::prelude::FluentBuilder;
use gpui::*;
use rust_i18n::t;
use smelt_plugin_api as api;
use smelt_plugin_api::RuntimeKey;
use smelt_plugin_api::ui::popover::{Popover, PopoverState};
use smelt_plugin_api::ui::truncated_line::truncated_line;
use smelt_plugin_api::ui::{Button, Icon, IconAction, IconName, ListRow, Radio, ui_theme};

use crate::Archive;
use crate::model::{ArchivedRow, MachineGroup, ProjectGroup};
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

/// 卡片宽度：侧栏底下往上弹，标题和悬停露出的两个按钮放得下。
const POPOVER_WIDTH: f32 = 320.;
/// 卡片里列表的最高高度，多了在里面滚。
const LIST_MAX_HEIGHT: f32 = 420.;
/// 悬停一行才露出恢复、删除按钮。
const ROW_GROUP: &str = "archive-row";

impl Render for Archive {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bar = div().h_full().flex().items_center();
        if self.machines.is_empty() {
            return bar;
        }
        // 贴着窗口下面的角时给窗口圆角让位。
        let insets = api::safe_area(cx.entity_id(), cx).bar_insets();
        let archive = cx.entity();
        bar.pl(insets.left).pr(insets.right).child(
            Popover::new("archive-popover")
                .anchor(Anchor::BottomLeft)
                .w(px(POPOVER_WIDTH))
                .p_1()
                .trigger(
                    Button::new("archive-button")
                        .quiet()
                        .icon(IconName::Archive)
                        .icon_action(t!("title")),
                )
                .content(move |_, _, cx| {
                    let popover = cx.entity().downgrade();
                    archive.read(cx).card(&archive, popover)
                }),
        )
    }
}

impl Archive {
    /// 卡片内容：标题，下面是能滚的列表。
    fn card(&self, archive: &Entity<Self>, popover: WeakEntity<PopoverState>) -> AnyElement {
        // 只有一台机器有归档时不画机器那一层。
        let by_machine = self.machines.len() > 1;
        let count: usize = self
            .machines
            .iter()
            .flat_map(|machine| &machine.projects)
            .map(|project| project.rows.len())
            .sum();
        div()
            .flex()
            .flex_col()
            .w_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .pt_1()
                    .pb_1p5()
                    .child(
                        div()
                            .type_style(TYPE.ui_medium)
                            .text_color(rgb(ui_theme::text()))
                            .child(t!("title").into_owned()),
                    )
                    .child(
                        div()
                            .type_style(TYPE.text_xsmall)
                            .text_color(rgb(ui_theme::muted()))
                            .child(count.to_string()),
                    ),
            )
            .child(
                div()
                    .id("archive-scroll")
                    .max_h(px(LIST_MAX_HEIGHT))
                    .overflow_y_scroll()
                    .child(div().flex().flex_col().w_full().gap_2().children(
                        self.machines.iter().map(|machine| {
                            self.machine_section(machine, by_machine, archive, &popover)
                        }),
                    )),
            )
            .into_any_element()
    }

    fn machine_section(
        &self,
        machine: &MachineGroup,
        by_machine: bool,
        archive: &Entity<Self>,
        popover: &WeakEntity<PopoverState>,
    ) -> AnyElement {
        let header = by_machine.then(|| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .child(
                    div()
                        .type_style(TYPE.text_xsmall_medium)
                        .text_color(rgb(ui_theme::text()))
                        .child(machine.name.clone()),
                )
                .when_some(machine.unavailable.clone(), |row, reason| {
                    row.child(
                        div()
                            .type_style(TYPE.text_xsmall)
                            .text_color(rgb(ui_theme::faint()))
                            .child(reason),
                    )
                })
        });
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap_2()
            .children(header)
            .children(
                machine
                    .projects
                    .iter()
                    .map(|project| self.project_group(&machine.machine, project, archive, popover)),
            )
            .into_any_element()
    }

    fn project_group(
        &self,
        machine: &RuntimeKey,
        project: &ProjectGroup,
        archive: &Entity<Self>,
        popover: &WeakEntity<PopoverState>,
    ) -> AnyElement {
        let header = div()
            .id(SharedString::from(format!(
                "archive-project-{machine:?}-{}",
                project.root
            )))
            .px_2()
            .type_style(TYPE.text_xsmall)
            .text_color(rgb(ui_theme::muted()))
            .overflow_hidden()
            .child(truncated_line(project.label.clone()))
            .tooltip({
                let root: SharedString = project.root.clone().into();
                move |window, cx| {
                    smelt_plugin_api::ui::tooltip::Tooltip::new(root.clone()).build(window, cx)
                }
            });
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(1.))
            .child(header)
            .children(
                project
                    .rows
                    .iter()
                    .map(|row| self.row(machine, row, archive, popover)),
            )
            .into_any_element()
    }

    fn row(
        &self,
        machine: &RuntimeKey,
        row: &ArchivedRow,
        archive: &Entity<Self>,
        popover: &WeakEntity<PopoverState>,
    ) -> AnyElement {
        let key = format!("archive-{machine:?}-{}", row.id);
        let busy = self.is_busy(machine, &row.id);
        let icon = if row.is_conversation {
            Icon::new(IconName::MessageSquare)
        } else {
            Icon::new(IconName::Terminal)
        };
        // 恢复、删除前先收起卡片：恢复了要跳到那段会话，要确认的对话框也不该压在卡片上。
        let restore = {
            let (archive, popover, machine, row) = (
                archive.clone(),
                popover.clone(),
                machine.clone(),
                row.clone(),
            );
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                cx.stop_propagation();
                let _ = popover.update(cx, |state, cx| state.dismiss(window, cx));
                archive.update(cx, |archive, cx| {
                    archive.request_restore(&machine, &row, window, cx)
                });
            }
        };
        let delete = {
            let (archive, popover, machine, row) = (
                archive.clone(),
                popover.clone(),
                machine.clone(),
                row.clone(),
            );
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                cx.stop_propagation();
                let _ = popover.update(cx, |state, cx| state.dismiss(window, cx));
                archive.update(cx, |archive, cx| {
                    archive.confirm_delete(&machine, &row, window, cx)
                });
            }
        };
        ListRow::new(SharedString::from(key.clone()))
            .group(ROW_GROUP)
            .w_full()
            .px_2()
            .py_1()
            .rounded(ui_theme::row_radius())
            .flex()
            .items_center()
            .gap_2()
            .disabled(busy)
            .child(
                div()
                    .flex_shrink_0()
                    .child(icon.size(px(15.)).text_color(rgb(ui_theme::muted()))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .type_style(TYPE.ui)
                            .child(truncated_line(row.title.clone())),
                    )
                    .when(!row.when.is_empty(), |column| {
                        column.child(
                            div()
                                .type_style(TYPE.text_xsmall)
                                .text_color(rgb(ui_theme::muted()))
                                .child(truncated_line(row.when.clone())),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_shrink_0()
                    .gap_1()
                    .opacity(0.0)
                    .group_hover(ROW_GROUP, |style| style.opacity(1.0))
                    .child(
                        Button::new(SharedString::from(format!("{key}-restore")))
                            .quiet()
                            .small()
                            .icon(IconName::Restore)
                            .icon_action(t!("action.restore"))
                            .disabled(busy)
                            .on_click(restore),
                    )
                    .child(
                        Button::new(SharedString::from(format!("{key}-delete")))
                            .dismiss()
                            .icon(IconName::Trash)
                            .icon_action(t!("action.delete"))
                            .disabled(busy)
                            .on_click(delete),
                    ),
            )
            .into_any_element()
    }
}

/// 选工作树对话框的正文：同一项目下还在的工作树，单选。没有就说清楚去哪建。
pub(crate) fn workset_choices(
    archive: &Entity<Archive>,
    candidates: &[(String, String)],
    cx: &App,
) -> AnyElement {
    if candidates.is_empty() {
        return div()
            .type_style(TYPE.ui)
            .text_color(rgb(ui_theme::muted()))
            .child(t!("restore.no_worksets").to_string())
            .into_any_element();
    }
    let picked = archive.read(cx).picked_workset.clone();
    div()
        .flex()
        .flex_col()
        .id("archive-workset-choices")
        .gap_2()
        .max_h(px(240.))
        .overflow_y_scroll()
        .children(candidates.iter().map(|(id, label)| {
            let archive = archive.clone();
            let workset = id.clone();
            Radio::new(SharedString::from(format!("archive-workset-{id}")))
                .label(label.clone())
                .checked(picked.as_deref() == Some(id.as_str()))
                .on_select(move |window, cx| {
                    archive.update(cx, |archive, _| {
                        archive.picked_workset = Some(workset.clone())
                    });
                    window.refresh();
                })
        }))
        .into_any_element()
}
