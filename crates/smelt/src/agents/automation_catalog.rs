//! 自动化目录：任务表、模板卡、全局 Run 列表。
//!
//! 版式跟 Grok Bot 自动化页：顶栏分段 + 已有任务表 + 模板画廊。

use super::automation_templates::{
    AutomationTemplate, AutomationTemplateCategory, AutomationTemplateGlyph, filtered_templates,
};
use super::*;
use smelt_ui::truncated_line::truncated_line;

const CATALOG_MAX_WIDTH: f32 = 960.;
const TEMPLATE_COLUMNS: usize = 3;

impl Workspace {
    pub(super) fn render_automations_catalog_header(
        &self,
        entity: Entity<Workspace>,
        left_guard: Option<Pixels>,
        cx: &App,
    ) -> AnyElement {
        const TABS: [(AutomationCatalogTab, &str); 2] = [
            (AutomationCatalogTab::Tasks, "自动化"),
            (AutomationCatalogTab::Runs, "运行"),
        ];
        let current = self.automation_surface.catalog_tab;
        let tab_entity = entity.clone();
        let add_entity = entity;
        product_page_chrome(left_guard)
            .justify_between()
            .gap_3()
            .child(choice_tabs(
                "automations-tabs",
                TABS.iter().position(|(tab, _)| *tab == current),
                TABS.map(|(_, label)| Tab::new().label(label)),
                move |ix, cx| {
                    if let Some((tab, _)) = TABS.get(ix).copied() {
                        tab_entity.update(cx, |workspace, cx| {
                            workspace.set_automation_catalog_tab(tab, cx);
                        });
                    }
                },
            ))
            .child(machine_picking_button(
                Button::new("automations-add")
                    .normal()
                    .icon(IconName::Plus)
                    .label("新建自动化"),
                add_entity,
                |workspace, runtime, window, cx| workspace.add_automation(runtime, window, cx),
                cx,
            ))
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
        // 每台机器有自己的自动化库。不止一台时按机器分组，每组标出机器和它的状态。
        let groups = automation_machine_groups(cx);
        let grouped = groups.len() > 1;
        let empty = groups.iter().all(|group| group.automations.is_empty());
        automation_catalog_column()
            .flex()
            .flex_col()
            .gap_6()
            .children(error.map(render_agent_error))
            .children(groups.into_iter().map(|group| {
                let header = grouped.then(|| render_automation_machine_header(&group, cx));
                div().flex().flex_col().gap_2().children(header).when(
                    !group.automations.is_empty(),
                    |column| {
                        column.child(self.render_automation_table(
                            &group.runtime,
                            &group.automations,
                            entity.clone(),
                            cx,
                        ))
                    },
                )
            }))
            .when(empty, |column| {
                column.child(
                    div()
                        .text_sm()
                        .text_color(rgb(crate::ui_theme::text_muted()))
                        .child("还没有自动化，可从下面的模板添加。"),
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
        // 各台机器的运行合在一起按时间排。借引用排序：整份克隆会把每条运行的输出在每帧
        // 重绘里复制一遍。
        let mut runs = config
            .automations
            .iter()
            .flat_map(|(runtime, machine)| machine.runs.iter().map(move |run| (runtime, run)))
            .collect::<Vec<_>>();
        runs.sort_by(|(_, a), (_, b)| (b.created_at, &b.id).cmp(&(a.created_at, &a.id)));
        let show_machine = config.automations.len() > 1;
        automation_catalog_column()
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
                    .children(runs.into_iter().map(|(runtime, run)| {
                        self.render_catalog_run_row(runtime, run, show_machine, entity.clone(), cx)
                    }))
                    .into_any_element()
            })
    }

    fn render_automation_table(
        &self,
        runtime: &crate::RuntimeKey,
        automations: &[settings::Automation],
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        // 引用的智能体按自动化所在的那台机器认。
        let agents = settings::agents_on(runtime, cx);
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
                self.render_automation_table_row(runtime, automation, agent, entity.clone(), cx)
            }))
            .into_any_element()
    }

    fn render_automation_table_row(
        &self,
        runtime: &crate::RuntimeKey,
        automation: settings::Automation,
        agent: Option<settings::AgentDefinition>,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let empty = settings::MachineAutomations::default();
        let machine = cx
            .global::<settings::AgentHostState>()
            .automations_on(runtime)
            .unwrap_or(&empty);
        let target = AutomationRef::new(runtime.clone(), automation.id.clone());
        let enabled = automation.enabled;
        let active_run = machine.active_run(&automation.id).cloned();
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
            machine.store_error.is_some(),
            agent_missing,
            running,
        );
        let now = Local::now().timestamp();
        let next_run = format_next_run_cell(
            machine
                .state_for(&automation.id)
                .and_then(|state| state.next_run_at),
            enabled,
            now,
        );
        let latest_run = machine.latest_run(&automation.id);
        let latest_run_id = latest_run.map(|run| run.id.clone());
        let last_run = format_last_run_cell(
            latest_run.map(|run| (run.status, run.finished_at.unwrap_or(run.created_at))),
            now,
        );
        let last_run_failed = latest_run
            .is_some_and(|run| run.status == smelt_model::automation::AutomationRunStatus::Failed);
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
        let open_id = target.clone();
        let open_entity = entity.clone();
        let menu = render_automation_row_menu(
            &target,
            enabled,
            can_run,
            cancel_run_id,
            latest_run_id,
            entity,
        );

        ListRow::new(format!(
            "automation-row-{}-{}",
            runtime_element_key(runtime),
            automation.id
        ))
        .card()
        .w_full()
        .px_4()
        .py_3()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_3()
                .child(neutral_icon_mark(
                    Icon::new(gpui_kit_assets::IconName::Workflow),
                    28.,
                ))
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
                                .overflow_hidden()
                                .child(truncated_line(display_name)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(crate::ui_theme::text_faint()))
                                .overflow_hidden()
                                .child(truncated_line(action_summary)),
                        ),
                ),
        )
        .child(
            automation_fixed_col(AUTOMATION_COL_SCHEDULE)
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_muted()))
                .overflow_hidden()
                .child(truncated_line(schedule)),
        )
        .child(
            automation_fixed_col(AUTOMATION_COL_LAST)
                .flex()
                .items_center()
                .gap_1p5()
                .when(last_run_failed, |cell| {
                    cell.child(
                        div()
                            .flex_shrink_0()
                            .size(px(6.))
                            .rounded_full()
                            .bg(rgb(crate::ui_theme::red())),
                    )
                })
                .child(
                    div()
                        .min_w_0()
                        .text_xs()
                        .text_color(rgb(crate::ui_theme::text_muted()))
                        .overflow_hidden()
                        .child(truncated_line(last_run)),
                ),
        )
        .child(
            automation_fixed_col(AUTOMATION_COL_NEXT)
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_muted()))
                .overflow_hidden()
                .child(truncated_line(next_run)),
        )
        .child(automation_fixed_col(AUTOMATION_COL_STATUS).child(automation_status_cell(status)))
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
        runtime: &crate::RuntimeKey,
        run: &smelt_model::automation::AutomationRun,
        show_machine: bool,
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
            .filter(|_| run.status == smelt_model::automation::AutomationRunStatus::Failed)
            .map(compact_instructions);
        let open_entity = entity;
        let run_id = run.id.clone();
        let open_runtime = runtime.clone();
        // 多台机器的运行混在一起时，标出是哪台机器上跑的。
        let machine = if show_machine {
            format!("{} · ", crate::runtimes::runtime_name(runtime, cx))
        } else {
            String::new()
        };
        ListRow::new(format!(
            "automation-catalog-run-{}-{run_id}",
            runtime_element_key(runtime)
        ))
        .card()
        .w_full()
        .px_4()
        .py_3()
        .flex()
        .items_center()
        .gap_3()
        .child(automation_run_detail::run_status_dot(run.status))
        .child(
            automation_run_lines(
                name,
                format!("{machine}{source} · {status} · {}", format_unix_local(at)),
                error,
            )
            .flex_1(),
        )
        .on_click(move |_, window, cx| {
            let run_id = run_id.clone();
            let runtime = open_runtime.clone();
            open_entity.update(cx, |workspace, cx| {
                workspace.open_catalog_automation_run(runtime, run_id, window, cx)
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
        let mut filters = vec![(None, "全部")];
        filters.extend(
            AutomationTemplateCategory::ALL
                .into_iter()
                .map(|category| (Some(category), category.label())),
        );
        let selected_ix = filters.iter().position(|(filter, _)| *filter == selected);
        let tabs: Vec<Tab> = filters
            .iter()
            .map(|(_, label)| Tab::new().label(*label))
            .collect();
        choice_tabs(
            "automation-template-filters",
            selected_ix,
            tabs,
            move |ix, cx| {
                if let Some((filter, _)) = filters.get(ix).copied() {
                    entity.update(cx, |workspace, cx| {
                        workspace.set_automation_template_filter(filter, cx);
                    });
                }
            },
        )
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
                            .child(render_template_card(template, entity.clone(), _cx))
                    }))
                    .children((chunk.len()..TEMPLATE_COLUMNS).map(|_| div().flex_1()))
            }))
            .into_any_element()
    }
}

fn automation_catalog_column() -> Div {
    div()
        .w_full()
        .max_w(px(CATALOG_MAX_WIDTH))
        .mx_auto()
        .px_6()
        .pt_5()
        .pb_8()
}

const AUTOMATION_COL_SCHEDULE: f32 = 168.;
const AUTOMATION_COL_LAST: f32 = 120.;
const AUTOMATION_COL_NEXT: f32 = 96.;
const AUTOMATION_COL_STATUS: f32 = 108.;
const AUTOMATION_COL_MENU: f32 = 32.;

fn automation_fixed_col(width: f32) -> Div {
    div().w(px(width)).flex_shrink_0()
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
                .child("自动化"),
        )
        .child(
            automation_fixed_col(AUTOMATION_COL_SCHEDULE)
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child("日程"),
        )
        .child(
            automation_fixed_col(AUTOMATION_COL_LAST)
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child("上次运行"),
        )
        .child(
            automation_fixed_col(AUTOMATION_COL_NEXT)
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child("下次运行"),
        )
        .child(
            automation_fixed_col(AUTOMATION_COL_STATUS)
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child("状态"),
        )
        .child(automation_fixed_col(AUTOMATION_COL_MENU))
        .into_any_element()
}

fn render_automation_row_menu(
    target: &AutomationRef,
    enabled: bool,
    can_run: bool,
    cancel_run_id: Option<String>,
    latest_run_id: Option<String>,
    entity: Entity<Workspace>,
) -> AnyElement {
    let run_id = target.clone();
    let toggle_id = target.clone();
    let delete_id = target.clone();
    let runtime = target.runtime.clone();
    let run_entity = entity.clone();
    let toggle_entity = entity.clone();
    let view_entity = entity.clone();
    let delete_entity = entity.clone();
    let cancel_entity = entity;
    Button::new(format!(
        "automation-row-more-{}-{}",
        runtime_element_key(&target.runtime),
        target.id
    ))
    .quiet()
    .compact()
    .icon(IconName::Ellipsis)
    .accessibility_label("更多操作")
    .dropdown_menu(move |mut menu, _, _| {
        if let Some(active_run_id) = cancel_run_id.clone() {
            let entity = cancel_entity.clone();
            let runtime = runtime.clone();
            menu = menu.item(PopupMenuItem::new("停止").icon(IconName::Close).on_click(
                move |_, _, cx| {
                    let run_id = active_run_id.clone();
                    entity.update(cx, |workspace, cx| {
                        workspace.cancel_automation_run(&runtime, run_id, cx);
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
        if let Some(latest_run_id) = latest_run_id.clone() {
            let entity = view_entity.clone();
            let runtime = runtime.clone();
            menu = menu.item(PopupMenuItem::new("查看运行").icon(IconName::Eye).on_click(
                move |_, window, cx| {
                    let run_id = latest_run_id.clone();
                    let runtime = runtime.clone();
                    entity.update(cx, |workspace, cx| {
                        workspace.open_catalog_automation_run(runtime, run_id, window, cx);
                    });
                },
            ));
        }
        let toggle_entity = toggle_entity.clone();
        let toggle_id = toggle_id.clone();
        menu = menu.item(
            automation_enabled_toggle_item(enabled).on_click(move |_, _, cx| {
                let id = toggle_id.clone();
                toggle_entity.update(cx, |workspace, cx| {
                    workspace.set_automation_enabled(id, !enabled, cx);
                });
            }),
        );
        let delete_entity = delete_entity.clone();
        let delete_id = delete_id.clone();
        menu.item(
            PopupMenuItem::new("删除自动化")
                .icon(smelt_ui::trash_icon())
                .on_click(move |_, _, cx| {
                    let id = delete_id.clone();
                    delete_entity.update(cx, |workspace, cx| {
                        workspace.start_delete_automation(id, cx);
                    });
                }),
        )
    })
    .into_any_element()
}

fn render_template_card(
    template: &AutomationTemplate,
    entity: Entity<Workspace>,
    cx: &App,
) -> AnyElement {
    let id = template.id;
    let add_entity = entity;
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
                .child(neutral_icon_mark(template_icon(template.glyph), 32.))
                .child(machine_picking_button(
                    Button::new(format!("automation-template-add-{id}"))
                        .quiet()
                        .compact()
                        .label("添加"),
                    add_entity,
                    move |workspace, runtime, window, cx| {
                        workspace.add_automation_from_template(runtime, id, window, cx)
                    },
                    cx,
                )),
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

/// 元素 id 里区分机器的那一段。
fn runtime_element_key(runtime: &crate::RuntimeKey) -> &str {
    match runtime {
        crate::RuntimeKey::Local => "local",
        crate::RuntimeKey::Remote(id) => id.as_str(),
    }
}

/// 能建自动化的机器：挂着的机器里自动化库读到了、可用的，本机在前。一台都没有时给本机。
fn automation_creation_machines(cx: &App) -> Vec<crate::RuntimeKey> {
    let config = cx.global::<settings::AgentHostState>();
    let machines = attached_agent_machines(cx)
        .into_iter()
        .filter(|runtime| {
            config.automations_on(runtime).is_some_and(|machine| {
                !machine.store_id.is_empty() && machine.store_error.is_none()
            })
        })
        .collect::<Vec<_>>();
    if machines.is_empty() {
        vec![crate::RuntimeKey::Local]
    } else {
        machines
    }
}

/// 「在某台机器上新建」的按钮：只有一台机器时直接建，多台时点开先选机器。
fn machine_picking_button(
    button: Button,
    entity: Entity<Workspace>,
    action: impl Fn(&mut Workspace, crate::RuntimeKey, &mut Window, &mut Context<Workspace>)
    + Clone
    + 'static,
    cx: &App,
) -> AnyElement {
    let machines = automation_creation_machines(cx);
    if let [only] = machines.as_slice() {
        let only = only.clone();
        return button
            .on_click(move |_, window, cx| {
                let runtime = only.clone();
                let action = action.clone();
                entity.update(cx, |workspace, cx| action(workspace, runtime, window, cx));
            })
            .into_any_element();
    }
    let names = machines
        .iter()
        .map(|runtime| (runtime.clone(), crate::runtimes::runtime_name(runtime, cx)))
        .collect::<Vec<_>>();
    button
        .dropdown_caret(true)
        .dropdown_menu(move |mut menu, _, _| {
            menu = menu.item(PopupMenuItem::label("建在哪台机器上"));
            for (runtime, name) in &names {
                let entity = entity.clone();
                let runtime = runtime.clone();
                let action = action.clone();
                menu = menu.item(PopupMenuItem::new(name.clone()).on_click(
                    move |_, window, cx| {
                        let runtime = runtime.clone();
                        let action = action.clone();
                        entity.update(cx, |workspace, cx| action(workspace, runtime, window, cx));
                    },
                ));
            }
            menu
        })
        .into_any_element()
}

/// 自动化列表里的一台机器。
struct AutomationMachineGroup {
    runtime: crate::RuntimeKey,
    automations: Vec<settings::Automation>,
    /// 机器连不上、库还没读到或读失败时给人看的话。
    status: Option<String>,
}

/// 挂着的每台机器一组，本机在前。还没收到快照的机器也列出来，说明在读。
fn automation_machine_groups(cx: &App) -> Vec<AutomationMachineGroup> {
    let config = cx.global::<settings::AgentHostState>();
    let mut keys = attached_agent_machines(cx);
    if keys.is_empty() {
        keys.push(crate::RuntimeKey::Local);
    }
    keys.into_iter()
        .map(|runtime| {
            let machine = config.automations_on(&runtime);
            let status = crate::runtimes::runtime_unavailable_label(&runtime, cx)
                .map(str::to_string)
                .or_else(|| {
                    machine
                        .and_then(|machine| machine.store_error.clone())
                        .map(|error| format!("自动化存储不可用：{error}"))
                })
                .or_else(|| {
                    machine
                        .is_none_or(|machine| machine.store_id.is_empty())
                        .then(|| "正在读取…".to_string())
                });
            AutomationMachineGroup {
                automations: machine
                    .map(|machine| machine.automations.clone())
                    .unwrap_or_default(),
                runtime,
                status,
            }
        })
        .collect()
}

fn render_automation_machine_header(group: &AutomationMachineGroup, cx: &App) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_medium()
                .text_color(rgb(crate::ui_theme::text_muted()))
                .child(crate::runtimes::runtime_name(&group.runtime, cx)),
        )
        .children(group.status.clone().map(|status| {
            div()
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child(status)
        }))
        .when(
            group.status.is_none() && group.automations.is_empty(),
            |row| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(rgb(crate::ui_theme::text_faint()))
                        .child("还没有自动化"),
                )
            },
        )
        .into_any_element()
}

fn automation_status_cell(status: AutomationRowStatus) -> AnyElement {
    let dot = match status {
        AutomationRowStatus::Running => Some(crate::ui_theme::blue()),
        AutomationRowStatus::AgentMissing | AutomationRowStatus::Unavailable => {
            Some(crate::ui_theme::red())
        }
        AutomationRowStatus::Paused => Some(crate::ui_theme::text_faint()),
        AutomationRowStatus::Active => None,
    };
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .children(dot.map(|color| div().size(px(6.)).rounded_full().bg(rgb(color))))
        .child(
            div()
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_muted()))
                .child(automation_row_status_label(status)),
        )
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
