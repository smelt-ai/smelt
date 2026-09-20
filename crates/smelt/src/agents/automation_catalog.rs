//! 自动化目录：任务表、模板卡、全局 Run 列表。
//!
//! 版式跟 Grok Bot 自动化页：顶栏分段 + 已有任务表 + 模板画廊。

use super::automation_templates::{
    AutomationTemplate, AutomationTemplateCategory, AutomationTemplateGlyph,
    AutomationTemplateTint, filtered_templates,
};
use super::*;

const CATALOG_MAX_WIDTH: f32 = 960.;
const TEMPLATE_COLUMNS: usize = 3;

impl Workspace {
    pub(super) fn render_automations_catalog_header(
        &self,
        entity: Entity<Workspace>,
    ) -> AnyElement {
        let tab = self.automation_surface.catalog_tab;
        let tasks_entity = entity.clone();
        let runs_entity = entity.clone();
        let add_entity = entity;
        automation_catalog_column()
            .py_4()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .p_0p5()
                    .rounded_full()
                    .bg(rgb(crate::ui_theme::bg_card()))
                    .border_1()
                    .border_color(rgb(crate::ui_theme::border_mid()))
                    .child(catalog_tab_pill(
                        "automations-tab-tasks",
                        "自动化任务",
                        tab == AutomationCatalogTab::Tasks,
                        move |_, _, cx| {
                            tasks_entity.update(cx, |workspace, cx| {
                                workspace
                                    .set_automation_catalog_tab(AutomationCatalogTab::Tasks, cx);
                            });
                        },
                    ))
                    .child(catalog_tab_pill(
                        "automations-tab-runs",
                        "运行",
                        tab == AutomationCatalogTab::Runs,
                        move |_, _, cx| {
                            runs_entity.update(cx, |workspace, cx| {
                                workspace
                                    .set_automation_catalog_tab(AutomationCatalogTab::Runs, cx);
                            });
                        },
                    )),
            )
            .child(
                Button::new("automations-add")
                    .primary()
                    .small()
                    .icon(IconName::Plus)
                    .label("新建自动化")
                    .on_click(move |_, window, cx| {
                        add_entity.update(cx, |workspace, cx| workspace.add_automation(window, cx));
                    }),
            )
            .into_any_element()
    }

    pub(super) fn render_automations_catalog(
        &self,
        error: Option<String>,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let body = match self.automation_surface.catalog_tab {
            AutomationCatalogTab::Tasks => self.render_automation_tasks_catalog(error, entity, cx),
            AutomationCatalogTab::Runs => self.render_automation_runs_catalog(error, entity, cx),
        };
        div()
            .id("automations-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(body)
            .into_any_element()
    }

    fn render_automation_tasks_catalog(
        &self,
        error: Option<String>,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> Div {
        let config = cx.global::<settings::AgentHostState>();
        let automations = config.automations.clone();
        let agents = config.agents.clone();
        automation_catalog_column()
            .pb_8()
            .flex()
            .flex_col()
            .gap_6()
            .children(error.map(render_agent_error))
            .when(!automations.is_empty(), |column| {
                column.child(self.render_automation_table(
                    &automations,
                    &agents,
                    entity.clone(),
                    cx,
                ))
            })
            .when(automations.is_empty(), |column| {
                column.child(
                    div()
                        .text_sm()
                        .text_color(rgb(crate::ui_theme::text_muted()))
                        .child("还没有自动化任务，可从下面的模板添加。"),
                )
            })
            .child(self.render_automation_templates(entity, cx))
    }

    fn render_automation_runs_catalog(
        &self,
        error: Option<String>,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> Div {
        let config = cx.global::<settings::AgentHostState>();
        let mut runs = config.automation_runs.clone();
        runs.sort_by_key(|run| std::cmp::Reverse((run.created_at, run.id.clone())));
        automation_catalog_column()
            .pb_8()
            .flex()
            .flex_col()
            .gap_3()
            .children(error.map(render_agent_error))
            .child(if runs.is_empty() {
                div()
                    .py_16()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(crate::ui_theme::text_muted()))
                            .child("暂无运行记录"),
                    )
                    .into_any_element()
            } else {
                agent_surface_card()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .children(runs.into_iter().enumerate().map(|(index, run)| {
                        self.render_catalog_run_row(index, run, entity.clone(), cx)
                    }))
                    .into_any_element()
            })
    }

    fn render_automation_table(
        &self,
        automations: &[settings::Automation],
        agents: &[settings::AgentDefinition],
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        agent_surface_card()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(automation_table_header())
            .children(automations.iter().cloned().map(|automation| {
                let agent = agents
                    .iter()
                    .find(|agent| automation.agent_definition_id() == Some(agent.id.as_str()))
                    .cloned();
                self.render_automation_table_row(automation, agent, entity.clone(), cx)
            }))
            .into_any_element()
    }

    fn render_automation_table_row(
        &self,
        automation: settings::Automation,
        agent: Option<settings::AgentDefinition>,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let config = cx.global::<settings::AgentHostState>();
        let enabled = automation.enabled;
        let active_run = config.active_automation_run(&automation.id).cloned();
        let action_ready = match &automation.action {
            settings::AutomationAction::Agent { .. } => agent.is_some(),
            settings::AutomationAction::Shell { .. } => true,
        };
        let can_run = automation.is_ready() && action_ready && active_run.is_none();
        let cancel_run_id = active_run
            .as_ref()
            .filter(|run| run.status.is_active())
            .map(|run| run.id.clone());
        let running = cancel_run_id.is_some();
        let agent_missing = matches!(automation.action, settings::AutomationAction::Agent { .. })
            && agent.is_none();
        let status = automation_row_status(
            enabled,
            config.automation_store_error.is_some(),
            agent_missing,
            running,
        );
        let next_run = format_next_run_cell(
            config
                .automation_state_for(&automation.id)
                .and_then(|state| state.next_run_at),
            enabled,
            Local::now().timestamp(),
        );
        let action_summary = match &automation.action {
            settings::AutomationAction::Agent { .. } => agent
                .as_ref()
                .map(|agent| agent.name.clone())
                .unwrap_or_else(|| "智能体已删除".to_string()),
            settings::AutomationAction::Shell { command, .. } => {
                let line = compact_instructions(command);
                if line.is_empty() {
                    "Shell".to_string()
                } else {
                    format!("Shell · {line}")
                }
            }
        };
        let display_name = if automation.name.trim().is_empty() {
            "未命名自动化".to_string()
        } else {
            automation.name.clone()
        };
        let schedule = automation.trigger.summary();
        let tint = automation_accent_color(&automation.id);
        let open_id = automation.id.clone();
        let open_entity = entity.clone();
        let menu =
            render_automation_row_menu(&automation.id, enabled, can_run, cancel_run_id, entity);

        div()
            .id(format!("automation-row-{}", automation.id))
            .w_full()
            .px_4()
            .py_3()
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .hover(|row| row.bg(rgb(crate::ui_theme::bg_hover())))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(automation_glyph_mark(IconName::Cpu, tint, 28.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(rgb(crate::ui_theme::text_bright()))
                                    .truncate()
                                    .child(display_name),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(crate::ui_theme::text_faint()))
                                    .truncate()
                                    .child(action_summary),
                            ),
                    ),
            )
            .child(
                div()
                    .w(px(168.))
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(rgb(crate::ui_theme::text_muted()))
                    .truncate()
                    .child(schedule),
            )
            .child(
                div()
                    .w(px(96.))
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(rgb(crate::ui_theme::text_muted()))
                    .truncate()
                    .child(next_run),
            )
            .child(
                div()
                    .w(px(108.))
                    .flex_shrink_0()
                    .child(automation_status_cell(status)),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(menu),
            )
            .on_click(move |_, window, cx| {
                let id = open_id.clone();
                open_entity.update(cx, |workspace, cx| {
                    workspace.open_automation_editor(id, window, cx)
                });
            })
            .into_any_element()
    }

    fn render_catalog_run_row(
        &self,
        index: usize,
        run: smelt_core::automation::AutomationRun,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let at = run.finished_at.unwrap_or(run.created_at);
        let source = run_source_label(run.source);
        let status = run_status_label(run.status);
        let name = if run.context.automation_name.trim().is_empty() {
            "未命名自动化".to_string()
        } else {
            run.context.automation_name.clone()
        };
        let error = run
            .error
            .as_deref()
            .filter(|_| run.status == smelt_core::automation::AutomationRunStatus::Failed)
            .map(compact_instructions);
        let open_entity = entity.clone();
        let run_id = run.id.clone();
        let view_entity = entity;
        let view_session_id = run.session_id;
        div()
            .id(("automation-catalog-run", index))
            .w_full()
            .px_4()
            .py_3()
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .hover(|row| row.bg(rgb(crate::ui_theme::bg_hover())))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .text_color(cx.theme().foreground)
                            .truncate()
                            .child(name),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(format!("{source} · {status} · {}", format_unix_local(at))),
                    )
                    .children(error.map(|error| {
                        div()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .truncate()
                            .child(error)
                    })),
            )
            .children(view_session_id.map(|session_id| {
                Button::new(format!("automation-catalog-run-view-{run_id}"))
                    .ghost()
                    .small()
                    .icon(IconName::Eye)
                    .tooltip("查看运行")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        let session_id = session_id.clone();
                        view_entity.update(cx, |workspace, cx| {
                            workspace.open_automation_run_session(&session_id, window, cx)
                        });
                    })
            }))
            .on_click(move |_, _, cx| {
                let run_id = run_id.clone();
                open_entity.update(cx, |workspace, cx| {
                    workspace.open_catalog_automation_run(run_id, cx)
                });
            })
            .into_any_element()
    }

    fn render_automation_templates(&self, entity: Entity<Workspace>, cx: &App) -> AnyElement {
        let filter = self.automation_surface.template_filter;
        let templates = filtered_templates(filter).collect::<Vec<_>>();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .text_color(rgb(crate::ui_theme::text_bright()))
                            .child("模板"),
                    )
                    .child(self.render_template_filters(entity.clone())),
            )
            .child(self.render_template_grid(&templates, entity, cx))
            .into_any_element()
    }

    fn render_template_filters(&self, entity: Entity<Workspace>) -> AnyElement {
        let selected = self.automation_surface.template_filter;
        let mut filters = vec![(None, "所有的")];
        filters.extend(
            AutomationTemplateCategory::ALL
                .into_iter()
                .map(|category| (Some(category), category.label())),
        );
        div()
            .flex()
            .items_center()
            .gap_1()
            .children(filters.into_iter().map(|(filter, label)| {
                let entity = entity.clone();
                catalog_tab_pill(
                    format!(
                        "automation-template-filter-{}",
                        filter.map(|category| category.label()).unwrap_or("all")
                    ),
                    label,
                    selected == filter,
                    move |_, _, cx| {
                        entity.update(cx, |workspace, cx| {
                            workspace.set_automation_template_filter(filter, cx);
                        });
                    },
                )
            }))
            .into_any_element()
    }

    fn render_template_grid(
        &self,
        templates: &[&AutomationTemplate],
        entity: Entity<Workspace>,
        _cx: &App,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .children(templates.chunks(TEMPLATE_COLUMNS).map(|chunk| {
                div()
                    .w_full()
                    .flex()
                    .gap_3()
                    .children(chunk.iter().copied().map(|template| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(render_template_card(template, entity.clone()))
                    }))
                    .children((chunk.len()..TEMPLATE_COLUMNS).map(|_| div().flex_1()))
            }))
            .into_any_element()
    }
}

fn automation_catalog_column() -> Div {
    div().w_full().max_w(px(CATALOG_MAX_WIDTH)).mx_auto().px_6()
}

fn catalog_tab_pill(
    id: impl Into<ElementId>,
    label: &'static str,
    selected: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .h(px(28.))
        .px_3()
        .rounded_full()
        .flex()
        .items_center()
        .cursor_pointer()
        .when(selected, |pill| {
            pill.bg(rgb(crate::ui_theme::bg_hover()))
                .text_color(rgb(crate::ui_theme::text_bright()))
        })
        .when(!selected, |pill| {
            pill.text_color(rgb(crate::ui_theme::text_muted()))
                .hover(|pill| {
                    pill.bg(rgb(crate::ui_theme::bg_row_hover()))
                        .text_color(rgb(crate::ui_theme::text()))
                })
        })
        .on_click(on_click)
        .child(div().text_xs().font_medium().child(label))
        .into_any_element()
}

fn automation_table_header() -> AnyElement {
    div()
        .w_full()
        .px_4()
        .pt_3()
        .pb_2()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child("自动化任务"),
        )
        .child(
            div()
                .w(px(168.))
                .flex_shrink_0()
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child("日程"),
        )
        .child(
            div()
                .w(px(96.))
                .flex_shrink_0()
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child("下次运行"),
        )
        .child(
            div()
                .w(px(108.))
                .flex_shrink_0()
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child("状态"),
        )
        .child(div().w(px(32.)).flex_shrink_0())
        .into_any_element()
}

fn render_automation_row_menu(
    automation_id: &str,
    enabled: bool,
    can_run: bool,
    cancel_run_id: Option<String>,
    entity: Entity<Workspace>,
) -> AnyElement {
    let run_id = automation_id.to_string();
    let toggle_id = automation_id.to_string();
    let history_id = automation_id.to_string();
    let delete_id = automation_id.to_string();
    let run_entity = entity.clone();
    let toggle_entity = entity.clone();
    let history_entity = entity.clone();
    let delete_entity = entity.clone();
    let cancel_entity = entity;
    Button::new(format!("automation-row-more-{automation_id}"))
        .ghost()
        .small()
        .icon(IconName::Ellipsis)
        .dropdown_menu(move |mut menu, _, _| {
            if let Some(active_run_id) = cancel_run_id.clone() {
                let entity = cancel_entity.clone();
                menu = menu.item(PopupMenuItem::new("停止").icon(IconName::CircleX).on_click(
                    move |_, _, cx| {
                        let run_id = active_run_id.clone();
                        entity.update(cx, |workspace, cx| {
                            workspace.cancel_automation_run(run_id, cx);
                        });
                    },
                ));
            } else {
                let entity = run_entity.clone();
                let id = run_id.clone();
                menu = menu.item(
                    PopupMenuItem::new("运行一次")
                        .icon(IconName::Play)
                        .disabled(!can_run)
                        .on_click(move |_, _, cx| {
                            let id = id.clone();
                            entity.update(cx, |workspace, cx| {
                                workspace.run_automation_once(id, cx);
                            });
                        }),
                );
            }
            let history_entity = history_entity.clone();
            let history_id = history_id.clone();
            menu = menu.item(PopupMenuItem::new("查看运行").icon(IconName::Eye).on_click(
                move |_, _, cx| {
                    let id = history_id.clone();
                    history_entity.update(cx, |workspace, cx| {
                        workspace.open_catalog_automation_history(id, cx);
                    });
                },
            ));
            let toggle_entity = toggle_entity.clone();
            let toggle_id = toggle_id.clone();
            menu = menu.item(
                PopupMenuItem::new(if enabled { "暂停" } else { "启用" }).on_click(
                    move |_, _, cx| {
                        let id = toggle_id.clone();
                        toggle_entity.update(cx, |workspace, cx| {
                            workspace.set_automation_enabled(id, !enabled, cx);
                        });
                    },
                ),
            );
            let delete_entity = delete_entity.clone();
            let delete_id = delete_id.clone();
            menu.item(
                PopupMenuItem::new("删除自动化")
                    .icon(IconName::Delete)
                    .on_click(move |_, _, cx| {
                        let id = delete_id.clone();
                        delete_entity.update(cx, |workspace, cx| {
                            workspace.delete_automation(id, cx);
                        });
                    }),
            )
        })
        .into_any_element()
}

fn render_template_card(template: &AutomationTemplate, entity: Entity<Workspace>) -> AnyElement {
    let id = template.id;
    let add_entity = entity;
    let tint = template_tint_color(template.tint);
    agent_surface_card()
        .id(format!("automation-template-{id}"))
        .h_full()
        .px_4()
        .py_4()
        .flex()
        .flex_col()
        .gap_3()
        .min_h(px(168.))
        .child(
            div()
                .flex()
                .items_start()
                .justify_between()
                .gap_2()
                .child(automation_glyph_mark(
                    template_icon(template.glyph),
                    tint,
                    32.,
                ))
                .child(
                    Button::new(format!("automation-template-add-{id}"))
                        .ghost()
                        .small()
                        .label("添加")
                        .on_click(move |_, window, cx| {
                            add_entity.update(cx, |workspace, cx| {
                                workspace.add_automation_from_template(id, window, cx);
                            });
                        }),
                ),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(rgb(crate::ui_theme::text_bright()))
                        .child(template.name),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(crate::ui_theme::text_muted()))
                        .child(template.description),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    Icon::new(IconName::Calendar)
                        .size(px(12.))
                        .text_color(rgb(crate::ui_theme::text_faint())),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(crate::ui_theme::text_faint()))
                        .child(template.schedule.summary()),
                ),
        )
        .into_any_element()
}

fn automation_glyph_mark(icon: IconName, tint: u32, size: f32) -> AnyElement {
    div()
        .size(px(size))
        .rounded(px(8.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(crate::ui_theme::tint(tint, 0x28))
        .child(
            Icon::new(icon)
                .size(px((size * 0.5).round()))
                .text_color(rgb(tint)),
        )
        .into_any_element()
}

fn automation_status_cell(status: AutomationRowStatus) -> AnyElement {
    let color = match status {
        AutomationRowStatus::Active => crate::ui_theme::green(),
        AutomationRowStatus::Running => crate::ui_theme::blue(),
        AutomationRowStatus::Paused => crate::ui_theme::text_faint(),
        AutomationRowStatus::AgentMissing | AutomationRowStatus::Unavailable => {
            crate::ui_theme::red()
        }
    };
    let label = automation_row_status_label(status);
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(div().size(px(6.)).rounded_full().bg(rgb(color)))
        .child(div().text_xs().text_color(rgb(color)).child(label))
        .into_any_element()
}

fn template_icon(glyph: AutomationTemplateGlyph) -> IconName {
    match glyph {
        AutomationTemplateGlyph::Sun => IconName::Sun,
        AutomationTemplateGlyph::Calendar => IconName::Calendar,
        AutomationTemplateGlyph::Inbox => IconName::Inbox,
        AutomationTemplateGlyph::File => IconName::File,
        AutomationTemplateGlyph::Cpu => IconName::Cpu,
        AutomationTemplateGlyph::Globe => IconName::Globe,
        AutomationTemplateGlyph::BookOpen => IconName::BookOpen,
        AutomationTemplateGlyph::LayoutDashboard => IconName::LayoutDashboard,
        AutomationTemplateGlyph::TriangleAlert => IconName::TriangleAlert,
    }
}

fn template_tint_color(tint: AutomationTemplateTint) -> u32 {
    match tint {
        AutomationTemplateTint::Blue => crate::ui_theme::blue(),
        AutomationTemplateTint::Purple => crate::ui_theme::purple(),
        AutomationTemplateTint::Green => crate::ui_theme::green(),
        AutomationTemplateTint::Yellow => crate::ui_theme::yellow(),
        AutomationTemplateTint::Accent => crate::ui_theme::accent(),
    }
}

fn automation_accent_color(id: &str) -> u32 {
    match id.bytes().fold(0u8, |acc, byte| acc.wrapping_add(byte)) % 5 {
        0 => crate::ui_theme::blue(),
        1 => crate::ui_theme::purple(),
        2 => crate::ui_theme::green(),
        3 => crate::ui_theme::yellow(),
        _ => crate::ui_theme::accent(),
    }
}
