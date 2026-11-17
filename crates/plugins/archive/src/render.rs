//! 归档页的画法：机器一节、项目一组、对话一行。只读视图上算好的分组。

use gpui::prelude::FluentBuilder;
use gpui::*;
use smelt_plugin_api::RuntimeKey;
use smelt_plugin_api::ui::menu::{Menu, MenuItem, MenuTrigger};
use smelt_plugin_api::ui::truncated_line::truncated_line;
use smelt_plugin_api::ui::{
    Button, Icon, IconAction, IconName, ListRow, Radio, trash_icon, ui_theme,
};

use crate::ArchivePage;
use crate::model::{ArchivedRow, MachineGroup, ProjectGroup};
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

/// 悬停一行才露出恢复、删除按钮。
const ROW_GROUP: &str = "archive-row";

impl Render for ArchivePage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.machines.is_empty() {
            return div()
                .flex()
                .flex_col()
                .size_full()
                .child(placeholder("没有归档的对话"))
                .into_any_element();
        }
        let page = cx.entity();
        let sections = self
            .machines
            .iter()
            .map(|machine| self.machine_section(machine, &page))
            .collect::<Vec<_>>();
        div()
            .id("archive-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .px(px(20.))
                    .py_3()
                    .gap_4()
                    .children(sections),
            )
            .into_any_element()
    }
}

impl ArchivePage {
    fn machine_section(&self, machine: &MachineGroup, page: &Entity<Self>) -> AnyElement {
        let count: usize = machine.projects.iter().map(|p| p.rows.len()).sum();
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .type_style(TYPE.ui_medium)
                    .text_color(rgb(ui_theme::text()))
                    .child(machine.name.clone()),
            )
            .child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(rgb(ui_theme::muted()))
                    .child(count.to_string()),
            )
            .when_some(machine.unavailable.clone(), |row, reason| {
                row.child(
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(rgb(ui_theme::faint()))
                        .child(reason),
                )
            });
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap_2()
            .child(header)
            .children(
                machine
                    .projects
                    .iter()
                    .map(|project| self.project_group(&machine.machine, project, page)),
            )
            .into_any_element()
    }

    fn project_group(
        &self,
        machine: &RuntimeKey,
        project: &ProjectGroup,
        page: &Entity<Self>,
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
            .children(project.rows.iter().map(|row| self.row(machine, row, page)))
            .into_any_element()
    }

    fn row(&self, machine: &RuntimeKey, row: &ArchivedRow, page: &Entity<Self>) -> AnyElement {
        let key = format!("archive-{machine:?}-{}", row.id);
        let busy = self.is_busy(machine, &row.id);
        let icon = if row.is_conversation {
            Icon::new(IconName::MessageSquare)
        } else {
            Icon::new(IconName::SquareTerminal)
        };
        let restore = {
            let (page, machine, row) = (page.clone(), machine.clone(), row.clone());
            move |window: &mut Window, cx: &mut App| {
                page.update(cx, |page, cx| {
                    page.request_restore(&machine, &row, window, cx)
                });
            }
        };
        let delete = {
            let (page, machine, row) = (page.clone(), machine.clone(), row.clone());
            move |window: &mut Window, cx: &mut App| {
                page.update(cx, |page, cx| {
                    page.confirm_delete(&machine, &row, window, cx)
                });
            }
        };
        let (menu_restore, menu_delete) = (restore.clone(), delete.clone());
        let row = ListRow::new(SharedString::from(key.clone()))
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
                            .icon(IconName::RotateCw)
                            .icon_action("恢复")
                            .disabled(busy)
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                restore(window, cx);
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!("{key}-delete")))
                            .dismiss()
                            .icon_path("smelt-icons/trash.svg")
                            .icon_action("彻底删除")
                            .disabled(busy)
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                delete(window, cx);
                            }),
                    ),
            );
        MenuTrigger::context(
            SharedString::from(format!("{key}-menu")),
            row,
            move |_, _| {
                let (restore, delete) = (menu_restore.clone(), menu_delete.clone());
                Menu::new()
                    .item(
                        MenuItem::new("恢复")
                            .disabled(busy)
                            .on_click(move |_, window, cx| restore(window, cx)),
                    )
                    .separator()
                    .item(
                        MenuItem::new("彻底删除")
                            .icon(trash_icon())
                            .danger()
                            .disabled(busy)
                            .on_click(move |_, window, cx| delete(window, cx)),
                    )
            },
        )
        .w_full()
        .into_any_element()
    }
}

/// 选工作树对话框的正文：同一项目下还在的工作树，单选。没有就说清楚去哪建。
pub(crate) fn workset_choices(
    page: &Entity<ArchivePage>,
    candidates: &[(String, String)],
    cx: &App,
) -> AnyElement {
    if candidates.is_empty() {
        return div()
            .type_style(TYPE.ui)
            .text_color(rgb(ui_theme::muted()))
            .child("这个项目下没有可用的工作树。先在会话列表里给项目新建一个工作树，再回来恢复。")
            .into_any_element();
    }
    let picked = page.read(cx).picked_workset.clone();
    div()
        .flex()
        .flex_col()
        .id("archive-workset-choices")
        .gap_2()
        .max_h(px(240.))
        .overflow_y_scroll()
        .children(candidates.iter().map(|(id, label)| {
            let page = page.clone();
            let workset = id.clone();
            Radio::new(SharedString::from(format!("archive-workset-{id}")))
                .label(label.clone())
                .checked(picked.as_deref() == Some(id.as_str()))
                .on_select(move |window, cx| {
                    page.update(cx, |page, _| page.picked_workset = Some(workset.clone()));
                    window.refresh();
                })
        }))
        .into_any_element()
}

fn placeholder(text: &'static str) -> Div {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .px_4()
        .child(
            div()
                .type_style(TYPE.ui)
                .text_color(rgb(ui_theme::muted()))
                .child(text),
        )
}
