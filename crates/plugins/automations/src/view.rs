//! 自动化目录和编辑器。
//!
//! 运行记录留在编辑器左侧。选中一次运行后，右侧是详情；有会话时这块详情就是现场。
//! 只组 UI，不做 IO。写入在 `actions.rs`。
use api::ViewInstance;

use crate::peers::agents::VIEW as AGENTS_VIEW;
use crate::prelude::*;

impl AutomationsPage {
    /// 编辑器的工具行：返回、标题（和保存提示）、右侧操作。
    pub(crate) fn render_automation_drill_header(
        &self,
        title: impl Into<SharedString>,
        subtitle: Option<(SharedString, u32)>,
        trailing: Option<AnyElement>,
        back_tooltip: &'static str,
        entity: Entity<AutomationsPage>,
    ) -> AnyElement {
        let back_entity = entity;
        api::ui::page_toolbar()
            .gap_3()
            .child(
                Button::new("automation-drill-back")
                    .quiet()
                    .icon(IconName::ChevronLeft)
                    .icon_action(back_tooltip)
                    .on_click(move |_, _, cx| {
                        back_entity.update(cx, |page, cx| page.close_automation_editor(cx));
                    }),
            )
            // 名字后面跟保存状态，不另起一行副标题：工具行里的控件是一行高。
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .text_sm()
                            .font_medium()
                            .text_color(rgb(ui_theme::text()))
                            .overflow_hidden()
                            .child(truncated_line(title)),
                    )
                    .when_some(subtitle, |row, (text, color)| {
                        row.child(
                            div()
                                .min_w_0()
                                .text_xs()
                                .text_color(rgb(color))
                                .overflow_hidden()
                                .child(truncated_line(text)),
                        )
                    }),
            )
            .when_some(trailing, |row, trailing| row.child(trailing))
            .into_any_element()
    }

    /// 页头右侧。新建是「创建」；已保存的是运行、启用和删除。不跟名称输入框挤在一行。
    pub(crate) fn render_automation_editor_actions(
        &self,
        editor: &AutomationEditor,
        entity: Entity<AutomationsPage>,
        cx: &App,
    ) -> AnyElement {
        if editor.is_new {
            let missing = editor.missing_fields(cx);
            let create_entity = entity;
            let saving = editor.saving_revision.is_some();
            return div()
                .flex_shrink_0()
                .child(
                    Button::new("automation-create")
                        .primary()
                        .label("创建")
                        .loading(saving)
                        .disabled(!missing.is_empty())
                        .on_click(move |_, _, cx| {
                            create_entity.update(cx, |page, cx| {
                                page.create_automation_from_editor(cx);
                            });
                        }),
                )
                .into_any_element();
        }

        let test_entity = entity.clone();
        let delete_entity = entity.clone();
        let toggle_entity = entity;
        let saving = editor.saving_revision.is_some();
        let header_saving = saving && !editor.quiet_save;
        let enabled = automations(cx)
            .on(&editor.automation.runtime)
            .and_then(|machine| machine.automation(&editor.automation.id))
            .map(|automation| automation.enabled)
            .unwrap_or(true);
        let delete_id = editor.automation.clone();
        let toggle_id = editor.automation.clone();
        div()
            .flex_shrink_0()
            .child(MenuTrigger::dropdown(
                "automation-editor-more-menu",
                Button::new("automation-editor-more")
                    .quiet()
                    .icon(IconName::Ellipsis)
                    .accessibility_label("更多操作"),
                move |_, _| {
                    let run_entity = test_entity.clone();
                    let mut menu = Menu::new().item(
                        MenuItem::new("运行一次")
                            .icon(IconName::Play)
                            .disabled(header_saving)
                            .on_click(move |_, _, cx| {
                                run_entity.update(cx, |page, cx| {
                                    page.test_automation_editor(cx);
                                });
                            }),
                    );
                    let toggle_entity = toggle_entity.clone();
                    let toggle_id = toggle_id.clone();
                    menu = menu.item(automation_enabled_toggle_item(enabled).on_click(
                        move |_, _, cx| {
                            let id = toggle_id.clone();
                            toggle_entity.update(cx, |page, cx| {
                                page.set_automation_enabled(id, !enabled, cx);
                            });
                        },
                    ));
                    let delete_entity = delete_entity.clone();
                    let delete_id = delete_id.clone();
                    menu.separator().item(
                        MenuItem::new("删除自动化")
                            .icon(api::ui::trash_icon())
                            .danger()
                            .on_click(move |_, window, cx| {
                                let id = delete_id.clone();
                                delete_entity.update(cx, |page, cx| {
                                    page.start_delete_automation(id, window, cx)
                                });
                            }),
                    )
                },
            ))
            .into_any_element()
    }

    /// 编辑器左侧的运行列表。任务行回到表单，运行行在右侧打开详情。
    pub(crate) fn render_automation_run_column(
        &self,
        runs: &[api::model::automation::AutomationRun],
        show_task: bool,
        entity: Entity<AutomationsPage>,
        _cx: &App,
    ) -> AnyElement {
        let selected_run_id = self.selected_run_id.clone();
        let task_selected = selected_run_id.is_none();
        let task_entity = entity.clone();
        let runtime = self.route.as_ref().map(|auto| auto.runtime.clone());
        let rows = runs
            .iter()
            .map(|run| {
                let at = run.finished_at.unwrap_or(run.created_at);
                let source = run_source_label(run.source);
                let status = run_status_label(run.status);
                let error = run
                    .error
                    .as_deref()
                    .filter(|_| run.status == api::model::automation::AutomationRunStatus::Failed)
                    .map(compact_instructions);
                let open_entity = entity.clone();
                let run_id = run.id.clone();
                let run_runtime = runtime.clone();
                let selected = selected_run_id.as_deref() == Some(run.id.as_str());
                ListRow::new(format!("automation-editor-run-{run_id}"))
                    .selected(selected)
                    .w_full()
                    .px_2()
                    .py_2()
                    .rounded(ui_theme::row_radius())
                    .flex()
                    .gap_2()
                    .child(crate::run_detail::run_status_dot(run.status).mt(px(6.)))
                    .child(
                        automation_run_lines(
                            format!("{source} · {status}"),
                            format_unix_local(at),
                            error,
                        )
                        .flex_1(),
                    )
                    .on_click(move |_, window, cx| {
                        let run_id = run_id.clone();
                        let Some(runtime) = run_runtime.clone() else {
                            return;
                        };
                        open_entity.update(cx, |page, cx| {
                            page.select_automation_run(&runtime, run_id, window, cx)
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
            .border_color(ui_theme::hair())
            .overflow_y_scroll()
            .p_2()
            .gap_1()
            .when(show_task, |column| {
                column.child(
                    ListRow::new("automation-run-column-task")
                        .selected(task_selected)
                        .w_full()
                        .px_2()
                        .py_2()
                        .rounded(ui_theme::row_radius())
                        // 字色跟着行走，选中时换成选中字色（见 `ListRow`）。
                        .child(div().text_sm().font_medium().child("设置"))
                        .on_click(move |_, _, cx| {
                            task_entity
                                .update(cx, |page, cx| page.show_automation_task(cx));
                        }),
                )
            })
            .child(
                div()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .text_xs()
                    .text_color(rgb(ui_theme::faint()))
                    .child("运行记录"),
            )
            .children(rows)
            .into_any_element()
    }

    pub(crate) fn render_automation_editor(
        &self,
        editor: &AutomationEditor,
        entity: Entity<AutomationsPage>,
        cx: &App,
    ) -> AnyElement {
        // 只能选自动化所在那台机器上的智能体。
        let ready_agents = agents_on(&editor.automation.runtime, cx)
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
        let agent_button = Button::new("automation-agent")
            .quiet()
            .small()
            .icon(IconName::Bot)
            .label(current_agent_name)
            .disabled(no_ready_agents)
            .dropdown_caret(true);
        let agent_selector =
            MenuTrigger::dropdown("automation-agent-menu", agent_button, move |_, _| {
                let mut menu = Menu::new();
                for agent in &ready_agents {
                    let entity = agent_entity.clone();
                    let id = agent.id.clone();
                    menu = menu.item(
                        MenuItem::new(agent.name.clone())
                            .icon(IconName::Bot)
                            .on_click(move |_, _, cx| {
                                let id = id.clone();
                                entity.update(cx, |page, cx| page.set_automation_agent(id, cx));
                            }),
                    );
                }
                menu
            })
            .disabled(no_ready_agents);
        let action_entity = entity.clone();
        let action_kind = Segmented::new("automation-action-kind")
            .segments(BUILTIN_AUTOMATION_ACTIONS.iter().map(|kind| kind.label()))
            .selected(
                BUILTIN_AUTOMATION_ACTIONS
                    .iter()
                    .position(|kind| *kind == editor.action_kind),
            )
            .on_select(move |ix, _, cx| {
                if let Some(kind) = BUILTIN_AUTOMATION_ACTIONS.get(ix).copied() {
                    action_entity.update(cx, |page, cx| {
                        page.set_automation_action_kind(kind, cx);
                    });
                }
            });
        let trigger_field = render_trigger_entries(editor, entity.clone(), cx);
        let is_agent = editor.action_kind == AutomationActionKindId::Agent;
        let is_webhook = editor
            .entries
            .iter()
            .any(|entry| entry.trigger_kind == AutomationTriggerKindId::Webhook);
        let instruction = if is_agent {
            framed_automation_textarea(TextArea::new(&editor.prompt).aria_label("指令"))
        } else {
            framed_automation_textarea(TextArea::new(&editor.command).aria_label("命令"))
        };
        let instruction_label = if is_agent { "指令" } else { "命令" };
        let name_field = TextInput::new(&editor.name).aria_label("名称").w_full();

        div()
            .id("automation-editor-scroll")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .child(
                div()
                    .w_full()
                    .max_w(px(640.))
                    .mx_auto()
                    .px_6()
                    .pt_6()
                    .pb_10()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .children(editor.error.clone().map(render_agent_error))
                    .child(automation_editor_section("名称", None, name_field))
                    .child(automation_editor_section(
                        "触发器",
                        Some("到点、按间隔，或收到 webhook 时运行。"),
                        trigger_field,
                    ))
                    .child(automation_editor_section(
                        instruction_label,
                        None,
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                automation_form_shell()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .px_3()
                                            .py_2()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .gap_3()
                                            .border_b_1()
                                            .border_color(ui_theme::hair())
                                            .child(action_kind)
                                            .when(is_agent, |toolbar| {
                                                if no_ready_agents {
                                                    // 智能体页由「智能体」插件提供，没装就不给入口。
                                                    let machine = editor.automation.runtime.clone();
                                                    toolbar.when(
                                                        api::is_registered(AGENTS_VIEW, cx),
                                                        |toolbar| {
                                                            toolbar.child(
                                                                compact_chip(
                                                                    "automation-open-agents",
                                                                )
                                                                .icon(IconName::Bot)
                                                                .label("创建智能体")
                                                                .on_click(move |_, _, cx| {
                                                                    open_agents_view(&machine, cx);
                                                                }),
                                                            )
                                                        },
                                                    )
                                                } else {
                                                    toolbar.child(agent_selector)
                                                }
                                            }),
                                    )
                                    .child(instruction),
                            )
                            .when(is_agent, |section| {
                                section.child(automation_field_hint(automation_prompt_hint(
                                    is_webhook,
                                )))
                            })
                            .child(render_automation_access_note(is_agent)),
                    ))
                    .child(render_notification_field(editor, entity, cx)),
            )
            .into_any_element()
    }

    /// 选中运行的右侧详情。有现场时这里就是对话，否则是这次运行的记录。
    pub(crate) fn render_automation_run_pane(
        &self,
        run: api::model::automation::AutomationRun,
        live_view: Option<AnyView>,
        entity: Entity<AutomationsPage>,
    ) -> AnyElement {
        let can_cancel = run.status.is_active();
        let source = run_source_label(run.source);
        let status = run_status_label(run.status);
        let run_id = run.id.clone();
        let cancel_entity = entity.clone();
        let cancel_runtime = self.route.as_ref().map(|auto| auto.runtime.clone());
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
                    .border_color(ui_theme::hair())
                    .child(crate::run_detail::run_status_dot(run.status))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_medium()
                            .text_color(rgb(ui_theme::text()))
                            .overflow_hidden()
                            .child(truncated_line(format!("{source} · {status}"))),
                    )
                    .when(can_cancel, |bar| {
                        let run_id = run_id.clone();
                        bar.child(
                            Button::new("automation-run-pane-cancel")
                                .danger_quiet()
                                .small()
                                .icon(IconName::Close)
                                .label("停止")
                                .on_click(move |_, _, cx| {
                                    let run_id = run_id.clone();
                                    let Some(runtime) = cancel_runtime.clone() else {
                                        return;
                                    };
                                    cancel_entity.update(cx, |page, cx| {
                                        page.cancel_automation_run(&runtime, run_id, cx)
                                    });
                                }),
                        )
                    }),
            )
            .when(self.lacks_conversation_view(&run), |pane| {
                pane.child(
                    div()
                        .flex_shrink_0()
                        .px_4()
                        .py_2()
                        .border_b_1()
                        .border_color(ui_theme::hair())
                        .text_xs()
                        .text_color(rgb(ui_theme::muted()))
                        .child("对话插件未安装，看不到运行现场；下面是落库的记录。"),
                )
            })
            .child(match live_view {
                Some(view) => div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .child(view.cached(StyleRefinement::default().size_full()))
                    .into_any_element(),
                None => div()
                    .id("automation-run-detail-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(760.))
                            .mx_auto()
                            .px_6()
                            .py_5()
                            .child(self.render_automation_run_detail(&run, entity)),
                    )
                    .into_any_element(),
            })
            .into_any_element()
    }

    pub(crate) fn render_automations_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        // 只借用插件里那份投影、按需取值。整份克隆会连带所有运行的输出，每帧重绘都复制一遍。
        let config = automations(cx);
        // 列表页的错误：本机的自动化库读不了。远程机器的在它那一组的标题上说。
        let error = self.error.clone().or_else(|| {
            config
                .on(&RuntimeKey::Local)
                .and_then(|machine| machine.store_error.clone())
        });
        let entity = cx.entity();
        let Some(automation) = self.route.as_ref().cloned() else {
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .child(self.render_automations_catalog_header(entity.clone(), cx))
                .child(self.render_automations_catalog(error, entity, cx))
                .into_any_element();
        };

        let runs = automations(cx)
            .on(&automation.runtime)
            .map(|machine| {
                machine
                    .runs_for(&automation.id)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let selected_run = self
            .selected_run_id
            .as_ref()
            .and_then(|run_id| runs.iter().find(|run| run.id == *run_id))
            .cloned();
        let live_view = selected_run
            .as_ref()
            .and_then(|run| self.live_view_for(run))
            .map(ViewInstance::view);
        let show_task = self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.automation == automation);
        let title = self
            .editor
            .as_ref()
            .filter(|editor| editor.automation == automation)
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
                automations(cx)
                    .on(&automation.runtime)
                    .and_then(|machine| machine.automation(&automation.id))
                    .map(|automation| automation.name.clone())
                    .filter(|name| !name.trim().is_empty())
            })
            .or_else(|| {
                runs.first()
                    .map(|run| run.context.automation_name.clone())
                    .filter(|name| !name.trim().is_empty())
            })
            .unwrap_or_else(|| "未命名自动化".to_string());
        let discarding_draft = self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.is_new && editor.automation == automation);
        let subtitle = self
            .editor
            .as_ref()
            .filter(|editor| editor.automation == automation)
            .and_then(|editor| {
                let text = automation_save_hint(&editor.missing_fields(cx), editor.is_new)?;
                let color = if editor.is_new {
                    ui_theme::faint()
                } else {
                    ui_theme::bad()
                };
                Some((SharedString::from(text), color))
            });
        let header_actions = self
            .editor
            .as_ref()
            .filter(|editor| editor.automation == automation)
            .map(|editor| self.render_automation_editor_actions(editor, entity.clone(), cx));
        let header = self.render_automation_drill_header(
            title,
            subtitle,
            header_actions,
            if discarding_draft { "取消" } else { "返回" },
            entity.clone(),
        );
        let body = if runs.is_empty() {
            if let Some(editor) = self
                .editor
                .as_ref()
                .filter(|editor| editor.automation == automation)
            {
                self.render_automation_editor(editor, entity, cx)
            } else {
                div()
                    .flex_1()
                    .p_5()
                    .text_sm()
                    .text_color(rgb(ui_theme::muted()))
                    .child("这条自动化已经不在了。")
                    .into_any_element()
            }
        } else {
            let column = self.render_automation_run_column(&runs, show_task, entity.clone(), cx);
            let detail = if let Some(run) = selected_run {
                self.render_automation_run_pane(run, live_view, entity)
            } else if let Some(editor) = self
                .editor
                .as_ref()
                .filter(|editor| editor.automation == automation)
            {
                self.render_automation_editor(editor, entity, cx)
            } else {
                div()
                    .flex_1()
                    .p_5()
                    .text_sm()
                    .text_color(rgb(ui_theme::muted()))
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
            .child(header)
            .child(body)
            .into_any_element()
    }
}

pub(crate) fn automation_field_hint(text: &'static str) -> AnyElement {
    div()
        .text_xs()
        .text_color(rgb(ui_theme::faint()))
        .child(text)
        .into_any_element()
}

pub(crate) fn automation_editor_section(
    label: &'static str,
    hint: Option<&'static str>,
    body: impl IntoElement,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(automation_section_label(label))
                .when_some(hint, |heading, hint| {
                    heading.child(automation_field_hint(hint))
                }),
        )
        .child(body)
        .into_any_element()
}
