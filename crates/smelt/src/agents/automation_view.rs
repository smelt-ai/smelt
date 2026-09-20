//! 自动化目录、编辑器、Run 历史和详情。
//!
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
        subtitle: impl Into<SharedString>,
        entity: Entity<Workspace>,
    ) -> AnyElement {
        let back_entity = entity;
        let subtitle = subtitle.into();
        div()
            .flex_shrink_0()
            .px_5()
            .py_4()
            .flex()
            .items_start()
            .gap_2()
            .child(
                Button::new("automation-drill-back")
                    .ghost()
                    .small()
                    .icon(IconName::ArrowLeft)
                    .tooltip("返回")
                    .on_click(move |_, _, cx| {
                        back_entity.update(cx, |workspace, cx| {
                            if workspace.nav.automations().is_history()
                                || workspace.nav.automations().run_id().is_some()
                            {
                                workspace.close_automation_run_history(cx);
                            } else {
                                workspace.close_automation_editor(cx);
                            }
                        });
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_semibold()
                            .text_color(rgb(crate::ui_theme::text_bright()))
                            .child(title.into()),
                    )
                    .when(!subtitle.as_str().is_empty(), |col| {
                        col.child(
                            div()
                                .text_xs()
                                .text_color(rgb(crate::ui_theme::text_muted()))
                                .child(subtitle),
                        )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn render_automation_run_history(
        &self,
        _name: String,
        runs: Vec<smelt_core::automation::AutomationRun>,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let count = runs.len();
        let rows = runs
            .into_iter()
            .enumerate()
            .map(|(index, run)| {
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
                let view_entity = entity.clone();
                let view_session_id = run.session_id;
                div()
                    .id(("automation-history-run", index))
                    .w_full()
                    .px_3()
                    .py_3()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(rgb(crate::ui_theme::border()))
                    .bg(rgb(crate::ui_theme::bg_card()))
                    .flex()
                    .items_center()
                    .gap_3()
                    .cursor_pointer()
                    .hover(|row| row.bg(rgb(crate::ui_theme::bg_row_hover())))
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
                                    .child(format!("{source} · {status}")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format_unix_local(at)),
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
                        Button::new(format!("automation-history-view-{run_id}"))
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
                            workspace.open_automation_run_detail(run_id, cx)
                        });
                    })
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        div()
            .id("automation-run-history-scroll")
            .flex_1()
            .w_full()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                div()
                    .w_full()
                    .max_w(px(780.))
                    .mx_auto()
                    .px_5()
                    .py_6()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children((count == 0).then(|| {
                        div()
                            .text_sm()
                            .text_color(rgb(crate::ui_theme::text_muted()))
                            .child("暂无运行记录")
                    }))
                    .children(rows),
            )
            .into_any_element()
    }

    pub(super) fn render_automation_run_detail(
        &self,
        run: smelt_core::automation::AutomationRun,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let back_entity = entity.clone();
        let cancel_entity = entity.clone();
        let view_entity = entity;
        let run_id = run.id.clone();
        let can_cancel = run.status.is_active();
        let view_session_id = run.session_id.clone();
        let automation_name = run.context.automation_name;
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
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .flex_1()
                            .child(
                                Button::new("automation-run-detail-back")
                                    .ghost()
                                    .small()
                                    .icon(IconName::ArrowLeft)
                                    .tooltip("返回运行历史")
                                    .on_click(move |_, _, cx| {
                                        back_entity.update(cx, |workspace, cx| {
                                            workspace.close_automation_run_detail(cx)
                                        });
                                    }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .w_full()
                                            .text_base()
                                            .font_semibold()
                                            .text_color(cx.theme().foreground)
                                            .truncate()
                                            .child(automation_name),
                                    )
                                    .child(
                                        div()
                                            .w_full()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .truncate()
                                            .child(format!("{source} · {status}")),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .children(view_session_id.map(|session_id| {
                                Button::new("automation-run-detail-session")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Eye)
                                    .label("查看运行")
                                    .on_click(move |_, window, cx| {
                                        let session_id = session_id.clone();
                                        view_entity.update(cx, |workspace, cx| {
                                            workspace.open_automation_run_session(
                                                &session_id,
                                                window,
                                                cx,
                                            )
                                        });
                                    })
                            }))
                            .when(can_cancel, |actions| {
                                let run_id = run_id.clone();
                                actions.child(
                                    Button::new("automation-run-detail-cancel")
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
                    ),
            )
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
        let history_entity = entity.clone();
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
        let run_count = config.automation_runs_for(&editor.automation_id).len();
        let latest_run = config.latest_automation_run(&editor.automation_id);
        let history_id = editor.automation_id.clone();
        let delete_id = editor.automation_id.clone();
        let toggle_id = editor.automation_id.clone();
        let history_summary = latest_run.map(|run| {
            format!(
                "共 {run_count} 次 · 最近一次 {} {}",
                format_unix_local(run.finished_at.unwrap_or(run.created_at)),
                run_status_label(run.status)
            )
        });
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
                            .child(
                                div()
                                    .size(px(44.))
                                    .rounded(px(12.))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .border_1()
                                    .border_color(crate::ui_theme::card_stroke())
                                    .bg(rgb(crate::ui_theme::bg_card()))
                                    .child(
                                        Icon::new(IconName::Cpu)
                                            .size(px(18.))
                                            .text_color(rgb(crate::ui_theme::text_muted())),
                                    ),
                            )
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
                    .child(render_notification_field(editor, entity.clone(), cx))
                    .when(!editor.is_new && run_count > 0, |column| {
                        column.child(
                            automation_form_row()
                                .id("automation-editor-history")
                                .cursor_pointer()
                                .hover(|row| row.bg(rgb(crate::ui_theme::bg_hover())))
                                .child(
                                    div()
                                        .flex_1()
                                        .text_sm()
                                        .text_color(rgb(crate::ui_theme::text_bright()))
                                        .child("运行记录"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(crate::ui_theme::text_faint()))
                                        .child(
                                            history_summary
                                                .unwrap_or_else(|| format!("共 {run_count} 次")),
                                        ),
                                )
                                .on_click(move |_, _, cx| {
                                    let id = history_id.clone();
                                    history_entity.update(cx, |workspace, cx| {
                                        workspace.open_automation_run_history(id, cx)
                                    });
                                }),
                        )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn render_automation_conversation_pane(
        &self,
        run: smelt_core::automation::AutomationRun,
        view: Entity<acp_view::AcpView>,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let back_entity = entity.clone();
        let cancel_entity = entity;
        let can_cancel = run.status.is_active();
        let source = run_source_label(run.source);
        let status = run_status_label(run.status);
        let run_id = run.id.clone();
        let automation_name = run.context.automation_name;
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
                        Button::new("automation-conversation-back")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .tooltip("返回运行历史")
                            .on_click(move |_, _, cx| {
                                back_entity.update(cx, |workspace, cx| {
                                    workspace.close_automation_run_detail(cx)
                                });
                            }),
                    )
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
                                    .child(automation_name),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{source} · {status}")),
                            ),
                    )
                    .when(can_cancel, |actions| {
                        actions.child(
                            Button::new("automation-conversation-cancel")
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
            .child(self.render_product_conversation_view(view, cx))
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
        let run_still_exists =
            |run_id: &str| config.automation_runs.iter().any(|run| run.id == run_id);
        let history_still_exists = |automation_id: &str| {
            config
                .automations
                .iter()
                .any(|automation| automation.id == automation_id)
                || config
                    .automation_runs
                    .iter()
                    .any(|run| run.automation_id == automation_id)
        };
        match self.nav.automations().clone() {
            AutomationsView::Run { run_id, .. } | AutomationsView::Live { run_id, .. }
                if !config.automation_store_id.is_empty() && !run_still_exists(&run_id) =>
            {
                self.nav.automations_mut().back();
            }
            AutomationsView::History { automation_id } if !history_still_exists(&automation_id) => {
                self.nav.automations_mut().back();
            }
            AutomationsView::Editor { automation_id }
                if self
                    .automation_surface
                    .editor
                    .as_ref()
                    .is_none_or(|editor| {
                        !editor.is_new || editor.automation_id != automation_id
                    })
                    && config
                        .automations
                        .iter()
                        .all(|automation| automation.id != automation_id) =>
            {
                self.nav.automations_mut().pop_to_root();
                self.automation_surface.editor = None;
            }
            _ => {}
        }
        if let Some(editor_id) = self.nav.automations().editor_id().map(str::to_string)
            && self
                .automation_surface
                .editor
                .as_ref()
                .is_none_or(|editor| editor.automation_id != editor_id)
        {
            self.open_automation_editor(editor_id, window, cx);
        }
        let selected_run = self.nav.automations().run_id().and_then(|run_id| {
            config
                .automation_runs
                .iter()
                .find(|run| run.id == run_id)
                .cloned()
        });
        let automations = config.automations.clone();
        let entity = cx.entity();
        let live_view = selected_run.as_ref().and_then(|run| {
            if !self.nav.automations().is_live() {
                return None;
            }
            let session_id = run.session_id.as_deref()?;
            self.automation_surface
                .live_view
                .as_ref()
                .and_then(|view| (view.read(cx).session_id() == session_id).then(|| view.clone()))
        });

        if let (Some(run), Some(view)) = (selected_run.clone(), live_view) {
            return self.render_automation_conversation_pane(run, view, entity, cx);
        }
        if let Some(run) = selected_run {
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .bg(rgb(crate::ui_theme::bg_stage()))
                .child(
                    div()
                        .id("automation-run-detail-scroll")
                        .flex_1()
                        .w_full()
                        .min_w_0()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(
                            div()
                                .w_full()
                                .max_w(px(780.))
                                .mx_auto()
                                .px_5()
                                .py_6()
                                .child(self.render_automation_run_detail(run, entity, cx)),
                        ),
                )
                .into_any_element();
        }

        let (header, body) = if self.nav.automations().is_history() {
            let id = self.nav.automations().automation_id().unwrap_or_default();
            let name = automations
                .iter()
                .find(|automation| automation.id == id)
                .map(|automation| automation.name.clone())
                .filter(|name| !name.trim().is_empty())
                .or_else(|| {
                    config
                        .automation_runs
                        .iter()
                        .find(|run| run.automation_id == id)
                        .map(|run| run.context.automation_name.clone())
                })
                .unwrap_or_else(|| "未命名自动化".to_string());
            let runs = config
                .automation_runs_for(id)
                .into_iter()
                .cloned()
                .collect::<Vec<_>>();
            let subtitle = if runs.is_empty() {
                "暂无运行记录".to_string()
            } else {
                format!("显示最近 {} 次运行。选择记录可查看输入和结果。", runs.len())
            };
            (
                self.render_automation_drill_header(name.clone(), subtitle, entity.clone()),
                self.render_automation_run_history(name, runs, entity, cx),
            )
        } else if let Some(editor) = self.automation_surface.editor.as_ref().filter(|editor| {
            self.nav.automations().editor_id() == Some(editor.automation_id.as_str())
        }) {
            let title = if editor.is_new {
                "新建自动化".to_string()
            } else {
                let value = editor.name.read(cx).value();
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    "未命名自动化".to_string()
                } else {
                    trimmed.to_string()
                }
            };
            (
                self.render_automation_drill_header(title, "", entity.clone()),
                self.render_automation_editor(editor, entity, cx),
            )
        } else {
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .bg(rgb(crate::ui_theme::bg_stage()))
                .child(self.render_automations_catalog_header(entity.clone()))
                .child(self.render_automations_catalog(error, entity, cx))
                .into_any_element();
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
