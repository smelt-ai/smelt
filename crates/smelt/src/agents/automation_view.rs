//! 自动化目录和编辑器。
//!
//! 运行记录留在编辑器左侧。选中一次运行后，右侧是详情；有会话时这块详情就是现场。
//! 只组 UI。触发器/Run 的写入在 `workspace.rs`。
use super::*;

impl Workspace {
    pub(crate) fn render_automation_navigation(
        &self,
        entity: Entity<Workspace>,
        statuses: &[AgentStatus],
        animate_running: bool,
        cx: &App,
    ) -> Div {
        let config = cx.global::<settings::AgentHostState>();
        div()
            .flex_shrink_0()
            .px_2()
            .pt_2()
            .pb_1()
            .flex()
            .flex_col()
            .gap_1()
            .child(self.render_workbench_chat_row(entity.clone()))
            .child(self.render_agent_product_navigation_row(
                "agents-route",
                "智能体",
                IconName::Bot,
                config.agents.len(),
                WorkspaceRoute::Agents,
                entity.clone(),
            ))
            .child(self.render_agent_product_navigation_row(
                "automations-route",
                "自动化",
                IconName::Cpu,
                config.automations.len(),
                WorkspaceRoute::Automations,
                entity.clone(),
            ))
            .child(self.render_product_conversation_nav(entity, statuses, animate_running, cx))
    }

    pub(super) fn render_automation_drill_header(
        &self,
        title: impl Into<SharedString>,
        entity: Entity<Workspace>,
        left_guard: Pixels,
    ) -> AnyElement {
        let back_entity = entity;
        product_page_chrome(left_guard)
            .gap_2()
            .child(product_back_button(
                "automation-drill-back",
                move |_, _, cx| {
                    back_entity.update(cx, |workspace, cx| workspace.close_automation_editor(cx));
                },
            ))
            .child(
                div()
                    .min_w_0()
                    .text_lg()
                    .font_semibold()
                    .text_color(rgb(crate::ui_theme::text_bright()))
                    .truncate()
                    .child(title.into()),
            )
            .into_any_element()
    }

    /// 编辑器左侧的运行列表。任务行回到表单，运行行在右侧打开详情。
    fn render_automation_run_column(
        &self,
        runs: &[smelt_core::automation::AutomationRun],
        show_task: bool,
        entity: Entity<Workspace>,
        _cx: &App,
    ) -> AnyElement {
        let selected_run_id = self.automation_surface.selected_run_id.clone();
        let task_selected = selected_run_id.is_none();
        let task_entity = entity.clone();
        let rows = runs
            .iter()
            .map(|run| {
                let at = run.finished_at.unwrap_or(run.created_at);
                let source = run_source_label(run.source);
                let status = run_status_label(run.status);
                let error = run
                    .error
                    .as_deref()
                    .filter(|_| run.status == smelt_core::automation::AutomationRunStatus::Failed)
                    .map(compact_instructions);
                let open_entity = entity.clone();
                let run_id = run.id.clone();
                let selected = selected_run_id.as_deref() == Some(run.id.as_str());
                div()
                    .id(format!("automation-editor-run-{run_id}"))
                    .w_full()
                    .px_2()
                    .py_2()
                    .rounded(crate::ui_theme::row_radius())
                    .cursor_pointer()
                    .when(selected, |row| row.bg(rgb(crate::ui_theme::bg_selected())))
                    .when(!selected, |row| {
                        row.hover(|row| row.bg(rgb(crate::ui_theme::bg_row_hover())))
                    })
                    .child(
                        automation_run_lines(
                            format!("{source} · {status}"),
                            format_unix_local(at),
                            error,
                        )
                        .w_full(),
                    )
                    .on_click(move |_, window, cx| {
                        let run_id = run_id.clone();
                        open_entity.update(cx, |workspace, cx| {
                            workspace.select_automation_run(run_id, window, cx)
                        });
                    })
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        div()
            .id("automation-run-column")
            .w(px(280.))
            .flex_shrink_0()
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(crate::ui_theme::hairline())
            .overflow_y_scroll()
            .p_2()
            .gap_1()
            .when(show_task, |column| {
                column.child(
                    div()
                        .id("automation-run-column-task")
                        .w_full()
                        .px_2()
                        .py_2()
                        .rounded(crate::ui_theme::row_radius())
                        .cursor_pointer()
                        .when(task_selected, |row| {
                            row.bg(rgb(crate::ui_theme::bg_selected()))
                        })
                        .when(!task_selected, |row| {
                            row.hover(|row| row.bg(rgb(crate::ui_theme::bg_row_hover())))
                        })
                        .child(
                            div()
                                .text_sm()
                                .font_medium()
                                .text_color(rgb(crate::ui_theme::text_bright()))
                                .child("任务"),
                        )
                        .on_click(move |_, _, cx| {
                            task_entity
                                .update(cx, |workspace, cx| workspace.show_automation_task(cx));
                        }),
                )
            })
            .child(
                div()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .text_xs()
                    .text_color(rgb(crate::ui_theme::text_faint()))
                    .child("运行记录"),
            )
            .children(rows)
            .into_any_element()
    }

    pub(super) fn render_automation_run_detail(
        &self,
        run: smelt_core::automation::AutomationRun,
        cx: &App,
    ) -> AnyElement {
        let run_id = run.id.clone();
        let source = run_source_label(run.source);
        let status = run_status_label(run.status);
        let agent_name = run
            .context
            .agent_definition_name
            .as_deref()
            .unwrap_or("未知智能体");
        let engine_kind_id = run.context.engine_kind_id.as_deref().unwrap_or("未知引擎");
        let scheduled_for = run
            .scheduled_for
            .map(format_unix_local)
            .unwrap_or_else(|| "不适用".to_string());
        let mut timeline = vec![("已创建".to_string(), format_unix_local(run.created_at))];
        if let Some(delivery_attempt_at) = run.delivery_attempt_at {
            timeline.push((
                "最近一次投递".to_string(),
                format!(
                    "{} · 共 {} 次",
                    format_unix_local(delivery_attempt_at),
                    run.delivery_attempts
                ),
            ));
        }
        if let Some(started_at) = run.started_at {
            timeline.push(("开始运行".to_string(), format_unix_local(started_at)));
        }
        if let Some(finished_at) = run.finished_at {
            timeline.push((status.to_string(), format_unix_local(finished_at)));
        } else {
            timeline.push((status.to_string(), "当前状态".to_string()));
        }
        let metadata = [
            ("来源", source.to_string()),
            ("状态", status.to_string()),
            ("智能体", format!("{agent_name} · {engine_kind_id}")),
            ("计划时间", scheduled_for),
            ("Smelt 工作区", run.context.cwd.clone()),
            ("Automation ID", run.automation_id.clone()),
            (
                "智能体定义 ID",
                run.context
                    .action
                    .agent_definition_id()
                    .map(str::to_string)
                    .unwrap_or_else(|| "（无）".to_string()),
            ),
            ("Run ID", run.id.clone()),
            (
                "ACP 会话",
                run.session_id
                    .clone()
                    .unwrap_or_else(|| "未创建".to_string()),
            ),
            (
                "Provider 会话",
                run.provider_session_id
                    .clone()
                    .unwrap_or_else(|| "未创建".to_string()),
            ),
        ];
        let prompt_id = format!("automation-run-prompt-{run_id}");
        let output_id = format!("automation-run-output-{run_id}");
        let transcript_id = format!("automation-run-transcript-{run_id}");
        let error_id = format!("automation-run-error-{run_id}");
        let instructions_id = format!("automation-run-instructions-{run_id}");
        let archived = smelt_core::automation_store::load_run(&run.id);
        let output = run
            .output
            .clone()
            .or_else(|| archived.as_ref().and_then(|run| run.output.clone()));
        let instructions = run.context.agent_instructions.clone().or_else(|| {
            archived
                .as_ref()
                .and_then(|run| run.context.agent_instructions.clone())
        });
        let transcript = smelt_core::automation_transcript::load_run_transcript_markdown(&run.id);
        let has_transcript = transcript.is_some();

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .border_t_1()
                    .border_color(rgb(crate::ui_theme::border()))
                    .children(metadata.into_iter().map(|(label, value)| {
                        div()
                            .py_2()
                            .flex()
                            .gap_4()
                            .border_b_1()
                            .border_color(rgb(crate::ui_theme::border()))
                            .child(
                                div()
                                    .w(px(88.))
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .child(value),
                            )
                    })),
            )
            .child(automation_run_detail_section(
                "时间线",
                div()
                    .flex()
                    .flex_col()
                    .children(timeline.into_iter().map(|(event, at)| {
                        div()
                            .py_1()
                            .flex()
                            .gap_4()
                            .child(
                                div()
                                    .w(px(88.))
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .child(event),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(at),
                            )
                    })),
                cx,
            ))
            .child(automation_run_detail_section(
                "本次输入",
                crate::markdown_mermaid::markdown_view(
                    prompt_id,
                    run.context
                        .prompt
                        .clone()
                        .unwrap_or_else(|| "（无）".to_string()),
                ),
                cx,
            ))
            .children(instructions.map(|instructions| {
                automation_run_detail_section(
                    "智能体指令快照",
                    crate::markdown_mermaid::markdown_view(instructions_id, instructions),
                    cx,
                )
            }))
            .children(output.clone().map(|output| {
                automation_run_detail_section(
                    "运行结果",
                    crate::markdown_mermaid::markdown_view(output_id, output),
                    cx,
                )
            }))
            .children(transcript.map(|transcript| {
                automation_run_detail_section(
                    "完整对话",
                    crate::markdown_mermaid::markdown_view(transcript_id, transcript),
                    cx,
                )
            }))
            .children(run.error.clone().map(|error| {
                automation_run_detail_section(
                    "错误",
                    crate::markdown_mermaid::markdown_view(error_id, error),
                    cx,
                )
            }))
            .when(
                output.is_none() && run.error.is_none() && !has_transcript,
                |detail| {
                    detail.child(automation_run_detail_section(
                        "运行结果",
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("暂无结果"),
                        cx,
                    ))
                },
            )
            .into_any_element()
    }

    pub(super) fn render_automation_editor(
        &self,
        editor: &AutomationEditor,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let ready_agents = cx
            .global::<settings::AgentHostState>()
            .agents
            .iter()
            .filter(|agent| agent.is_ready())
            .cloned()
            .collect::<Vec<_>>();
        let current_agent = ready_agents
            .iter()
            .find(|agent| agent.id == editor.agent_id)
            .cloned();
        let current_agent_name = current_agent
            .as_ref()
            .map(|agent| agent.name.clone())
            .unwrap_or_else(|| "选择智能体".to_string());
        let no_ready_agents = ready_agents.is_empty();
        let agent_entity = entity.clone();
        let agent_selector = Button::new("automation-agent")
            .ghost()
            .small()
            .focus_ring(false)
            .icon(IconName::Bot)
            .label(current_agent_name)
            .disabled(no_ready_agents)
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for agent in &ready_agents {
                    let entity = agent_entity.clone();
                    let id = agent.id.clone();
                    menu = menu.item(
                        PopupMenuItem::new(agent.name.clone())
                            .icon(IconName::Bot)
                            .on_click(move |_, _, cx| {
                                let id = id.clone();
                                entity.update(cx, |workspace, cx| {
                                    workspace.set_automation_agent(id, cx)
                                });
                            }),
                    );
                }
                menu
            });
        let action_kind = div().flex().items_center().gap_1().children(
            settings::BUILTIN_AUTOMATION_ACTIONS
                .iter()
                .enumerate()
                .map(|(index, kind)| {
                    let selected = *kind == editor.action_kind;
                    let action_entity = entity.clone();
                    let kind = *kind;
                    compact_chip(format!("automation-action-{index}"))
                        .label(kind.label())
                        .selected(selected)
                        .toggled(selected)
                        .on_click(move |_, _, cx| {
                            action_entity.update(cx, |workspace, cx| {
                                workspace.set_automation_action_kind(kind, cx);
                            });
                        })
                }),
        );
        let trigger_field = render_trigger_entries(editor, entity.clone(), cx);
        let test_entity = entity.clone();
        let delete_entity = entity.clone();
        let toggle_entity = entity.clone();
        let saving = editor.saving_revision.is_some();
        let header_saving = saving && !editor.quiet_save;
        let config = cx.global::<settings::AgentHostState>();
        let stored = config
            .automations
            .iter()
            .find(|automation| automation.id == editor.automation_id);
        let enabled = stored.map(|automation| automation.enabled).unwrap_or(true);
        let delete_id = editor.automation_id.clone();
        let toggle_id = editor.automation_id.clone();
        let is_agent = editor.action_kind == settings::AutomationActionKindId::Agent;
        let is_webhook = editor
            .entries
            .iter()
            .any(|entry| entry.trigger_kind == settings::AutomationTriggerKindId::Webhook);
        let instruction = if is_agent {
            framed_automation_textarea(Textarea::new(&editor.prompt))
        } else {
            framed_automation_textarea(Textarea::new(&editor.command))
        };

        div()
            .id("automation-editor-scroll")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .child(
                div()
                    .w_full()
                    .max_w(px(560.))
                    .mx_auto()
                    .px_6()
                    .py_5()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .children(editor.error.clone().map(render_agent_error))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(neutral_icon_mark(IconName::Cpu, 32.))
                            .child(
                                automation_form_row().flex_1().min_w_0().child(
                                    Input::new(&editor.name)
                                        .appearance(false)
                                        .focus_bordered(false)
                                        .focus_ring(false)
                                        .w_full()
                                        .text_sm(),
                                ),
                            )
                            .when(!editor.is_new, |row| {
                                row.child(
                                    Button::new("automation-editor-more")
                                        .ghost()
                                        .small()
                                        .icon(IconName::Ellipsis)
                                        .dropdown_menu(move |mut menu, _, _| {
                                            let run_entity = test_entity.clone();
                                            menu = menu.item(
                                                PopupMenuItem::new("运行一次")
                                                    .icon(IconName::Play)
                                                    .disabled(header_saving)
                                                    .on_click(move |_, _, cx| {
                                                        run_entity.update(cx, |workspace, cx| {
                                                            workspace.test_automation_editor(cx);
                                                        });
                                                    }),
                                            );
                                            let toggle_entity = toggle_entity.clone();
                                            let toggle_id = toggle_id.clone();
                                            menu = menu.item(
                                                PopupMenuItem::new(if enabled {
                                                    "暂停"
                                                } else {
                                                    "启用"
                                                })
                                                .on_click(move |_, _, cx| {
                                                    let id = toggle_id.clone();
                                                    toggle_entity.update(cx, |workspace, cx| {
                                                        workspace.set_automation_enabled(
                                                            id, !enabled, cx,
                                                        );
                                                    });
                                                }),
                                            );
                                            let delete_entity = delete_entity.clone();
                                            let delete_id = delete_id.clone();
                                            menu.item(
                                                PopupMenuItem::new("删除自动化")
                                                    .icon(IconName::Delete)
                                                    .on_click(move |_, _, cx| {
                                                        let id = delete_id.clone();
                                                        delete_entity.update(
                                                            cx,
                                                            |workspace, cx| {
                                                                workspace.delete_automation(id, cx)
                                                            },
                                                        );
                                                    }),
                                            )
                                        }),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(automation_section_label("触发器"))
                            .child(trigger_field),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(automation_section_label("指令"))
                            .child(
                                automation_form_shell()
                                    .flex()
                                    .flex_col()
                                    .child(instruction)
                                    .child(
                                        div()
                                            .px_3()
                                            .pb_3()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(action_kind)
                                            .when(is_agent, |footer| {
                                                if no_ready_agents {
                                                    let agents_entity = entity.clone();
                                                    footer.child(
                                                        compact_chip("automation-open-agents")
                                                            .icon(IconName::Bot)
                                                            .label("创建智能体")
                                                            .on_click(move |_, window, cx| {
                                                                agents_entity.update(
                                                                    cx,
                                                                    |workspace, cx| {
                                                                        workspace.open_agents_route(
                                                                            window, cx,
                                                                        )
                                                                    },
                                                                );
                                                            }),
                                                    )
                                                } else {
                                                    footer.child(agent_selector)
                                                }
                                            }),
                                    ),
                            )
                            .when(is_agent, |section| {
                                section.child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(crate::ui_theme::text_faint()))
                                        .child(automation_prompt_hint(is_webhook)),
                                )
                            }),
                    )
                    .child(render_notification_field(editor, entity, cx)),
            )
            .into_any_element()
    }

    /// 选中运行的右侧详情。有现场时这里就是对话，否则是这次运行的记录。
    fn render_automation_run_pane(
        &self,
        run: smelt_core::automation::AutomationRun,
        live_view: Option<Entity<acp_view::AcpView>>,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let can_cancel = run.status.is_active();
        let source = run_source_label(run.source);
        let status = run_status_label(run.status);
        let run_id = run.id.clone();
        let cancel_entity = entity;
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(rgb(crate::ui_theme::border()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_medium()
                            .text_color(cx.theme().foreground)
                            .truncate()
                            .child(format!("{source} · {status}")),
                    )
                    .when(can_cancel, |bar| {
                        let run_id = run_id.clone();
                        bar.child(
                            Button::new("automation-run-pane-cancel")
                                .ghost()
                                .small()
                                .icon(IconName::CircleX)
                                .label("停止")
                                .on_click(move |_, _, cx| {
                                    let run_id = run_id.clone();
                                    cancel_entity.update(cx, |workspace, cx| {
                                        workspace.cancel_automation_run(run_id, cx)
                                    });
                                }),
                        )
                    }),
            )
            .child(match live_view {
                Some(view) => self.render_product_conversation_view(view, cx),
                None => div()
                    .id("automation-run-detail-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(720.))
                            .px_5()
                            .py_5()
                            .child(self.render_automation_run_detail(run, cx)),
                    )
                    .into_any_element(),
            })
            .into_any_element()
    }

    pub(crate) fn render_automations_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let config = cx.global::<settings::AgentHostState>().clone();
        let error = self
            .automation_surface
            .error
            .clone()
            .or_else(|| config.persistence_error.clone())
            .or_else(|| config.automation_store_error.clone());
        let store_ready = !config.automation_store_id.is_empty();
        if let Some(automation_id) = self.nav.automations().editor_id().map(str::to_string) {
            let automation_exists = config
                .automations
                .iter()
                .any(|automation| automation.id == automation_id);
            let runs_exist = config
                .automation_runs
                .iter()
                .any(|run| run.automation_id == automation_id);
            let new_draft = self
                .automation_surface
                .editor
                .as_ref()
                .is_some_and(|editor| editor.is_new && editor.automation_id == automation_id);
            if store_ready && !automation_exists && !runs_exist && !new_draft {
                self.nav.automations_mut().pop_to_root();
                self.automation_surface.editor = None;
                self.automation_surface.selected_run_id = None;
                self.automation_surface.live_view = None;
                self.automation_surface.live_sub = None;
            } else if store_ready
                && self
                    .automation_surface
                    .selected_run_id
                    .as_ref()
                    .is_some_and(|run_id| {
                        !config
                            .automation_runs
                            .iter()
                            .any(|run| run.id == *run_id && run.automation_id == automation_id)
                    })
            {
                self.automation_surface.selected_run_id = None;
                self.automation_surface.live_view = None;
                self.automation_surface.live_sub = None;
            }
            if self.nav.automations().editor_id() == Some(automation_id.as_str())
                && automation_exists
                && self
                    .automation_surface
                    .editor
                    .as_ref()
                    .is_none_or(|editor| editor.automation_id != automation_id)
            {
                self.open_automation_editor(automation_id, window, cx);
            }
        }

        let entity = cx.entity();
        let left_guard = self.chrome_left_guard(window);
        let Some(automation_id) = self.nav.automations().editor_id().map(str::to_string) else {
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .bg(rgb(crate::ui_theme::bg_stage()))
                .child(self.render_automations_catalog_header(entity.clone(), left_guard))
                .child(self.render_automations_catalog(error, entity, cx))
                .into_any_element();
        };

        let runs = config
            .automation_runs_for(&automation_id)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let selected_run = self
            .automation_surface
            .selected_run_id
            .as_ref()
            .and_then(|run_id| runs.iter().find(|run| run.id == *run_id))
            .cloned();
        let live_view = selected_run.as_ref().and_then(|run| {
            let session_id = run.session_id.as_deref()?;
            self.automation_surface
                .live_view
                .as_ref()
                .and_then(|view| (view.read(cx).session_id() == session_id).then(|| view.clone()))
        });
        let show_task = self
            .automation_surface
            .editor
            .as_ref()
            .is_some_and(|editor| editor.automation_id == automation_id);
        let title = self
            .automation_surface
            .editor
            .as_ref()
            .filter(|editor| editor.automation_id == automation_id)
            .map(|editor| {
                if editor.is_new {
                    "新建自动化".to_string()
                } else {
                    let trimmed = editor.name.read(cx).value();
                    let trimmed = trimmed.trim();
                    if trimmed.is_empty() {
                        "未命名自动化".to_string()
                    } else {
                        trimmed.to_string()
                    }
                }
            })
            .or_else(|| {
                config
                    .automations
                    .iter()
                    .find(|automation| automation.id == automation_id)
                    .map(|automation| automation.name.clone())
                    .filter(|name| !name.trim().is_empty())
            })
            .or_else(|| {
                runs.first()
                    .map(|run| run.context.automation_name.clone())
                    .filter(|name| !name.trim().is_empty())
            })
            .unwrap_or_else(|| "未命名自动化".to_string());
        let header = self.render_automation_drill_header(title, entity.clone(), left_guard);
        let body = if runs.is_empty() {
            if let Some(editor) = self
                .automation_surface
                .editor
                .as_ref()
                .filter(|editor| editor.automation_id == automation_id)
            {
                self.render_automation_editor(editor, entity, cx)
            } else {
                div()
                    .flex_1()
                    .p_5()
                    .text_sm()
                    .text_color(rgb(crate::ui_theme::text_muted()))
                    .child("这条自动化已经不在了。")
                    .into_any_element()
            }
        } else {
            let column = self.render_automation_run_column(&runs, show_task, entity.clone(), cx);
            let detail = if let Some(run) = selected_run {
                self.render_automation_run_pane(run, live_view, entity, cx)
            } else if let Some(editor) = self
                .automation_surface
                .editor
                .as_ref()
                .filter(|editor| editor.automation_id == automation_id)
            {
                self.render_automation_editor(editor, entity, cx)
            } else {
                div()
                    .flex_1()
                    .p_5()
                    .text_sm()
                    .text_color(rgb(crate::ui_theme::text_muted()))
                    .child("选择一次运行。")
                    .into_any_element()
            };
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(column)
                .child(detail)
                .into_any_element()
        };

        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(crate::ui_theme::bg_stage()))
            .child(
                div()
                    .bg(rgb(crate::ui_theme::bg_stage()))
                    .border_b_1()
                    .border_color(rgb(crate::ui_theme::border()))
                    .child(header),
            )
            .child(body)
            .into_any_element()
    }
}
