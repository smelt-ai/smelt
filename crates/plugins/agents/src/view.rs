//! 智能体页面：目录、编辑页和各种菜单。

use super::*;

impl AgentsPage {
    pub(crate) fn render_agents_header(
        &self,
        count: usize,
        entity: Entity<AgentsPage>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let add_entity = entity.clone();
        let engines = agent_engine_kinds();
        let machines = agent_creation_machines(cx);
        // 只有一台机器、一种引擎时直接建；不然先选建在哪台机器上、用哪个引擎。
        let add_button = match (machines.as_slice(), engines.as_slice()) {
            ([runtime], [kind]) => {
                let (runtime, kind) = (runtime.clone(), *kind);
                Button::new("agents-add")
                    .normal()
                    .icon(IconName::Plus)
                    .label("新建智能体")
                    .on_click(move |_, window, cx| {
                        let runtime = runtime.clone();
                        add_entity
                            .update(cx, |page, cx| page.create_agent(runtime, kind, window, cx));
                    })
                    .into_any_element()
            }
            _ => Button::new("agents-add")
                .normal()
                .icon(IconName::Plus)
                .label("新建智能体")
                .dropdown_menu(move |menu, _, cx| {
                    fill_create_agent_menu(menu, add_entity.clone(), cx)
                })
                .into_any_element(),
        };
        product_page_header(
            "智能体",
            count,
            "个",
            div()
                .flex()
                .items_center()
                .gap_2()
                .children(self.render_default_agent_picker(entity, cx))
                .child(add_button)
                .into_any_element(),
            window,
            cx,
        )
    }

    /// 页头的默认智能体选择器：「新对话」跟谁开。从「新对话」过来、还没默认时，选了就开。
    /// 一个能开对话的智能体都没有时不显示。
    fn render_default_agent_picker(
        &self,
        entity: Entity<AgentsPage>,
        cx: &App,
    ) -> Option<AnyElement> {
        let plan = plan_new_conversation(self.conversation_draft.as_ref(), cx);
        if plan.ready.is_empty() {
            return None;
        }
        let (label, current, start) = match plan.intent {
            NewConversationIntent::Start(agent) => (
                format!(
                    "默认：{}",
                    agent_display_name(plan.default_name.as_deref().unwrap_or_default())
                ),
                Some(agent),
                false,
            ),
            NewConversationIntent::PickAgent | NewConversationIntent::CreateAgent => (
                "选默认智能体".to_string(),
                None,
                self.picking_for_conversation,
            ),
        };
        let ready = plan.ready;
        Some(
            Button::new("agents-default-picker")
                .quiet()
                .label(label)
                .tooltip("「新对话」用这个智能体开")
                .dropdown_menu(move |menu, _, cx| {
                    fill_conversation_agent_menu(
                        menu,
                        ready.clone(),
                        current.clone(),
                        start,
                        entity.clone(),
                        cx,
                    )
                })
                .into_any_element(),
        )
    }

    pub(crate) fn render_agent_editor_header(
        &self,
        name: &str,
        entity: Entity<AgentsPage>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let draft = self.editing_conversation_draft();
        let back_entity = entity.clone();
        let tabs = self.editor.as_ref().map(|editor| {
            const TABS: [AgentEditorTab; 2] = [AgentEditorTab::Config, AgentEditorTab::Memory];
            let entity = entity.clone();
            choice_tabs(
                "agent-editor-tabs",
                TABS.iter().position(|tab| *tab == editor.tab),
                TABS.map(|tab| Tab::new().label(agent_editor_tab_label(tab, editor))),
                move |ix, cx| {
                    if let Some(tab) = TABS.get(ix).copied() {
                        entity.update(cx, |page, cx| page.select_agent_editor_tab(tab, cx));
                    }
                },
            )
        });
        let start_entity = entity;
        api::host(cx)
            .page_header(window, cx)
            .gap_2()
            .child(product_back_button(
                "agent-editor-back",
                if draft { "取消" } else { "返回" },
                move |_, _, cx| {
                    back_entity.update(cx, |page, cx| page.close_agent_settings(cx));
                },
            ))
            .child(agent_monogram(name, 28., false))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_semibold()
                            .text_color(rgb(ui_theme::text_bright()))
                            .overflow_hidden()
                            .child(truncated_line(agent_display_name(name))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(ui_theme::text_muted()))
                            .child(if draft {
                                "开始对话后会设为默认。返回会取消，不保留这个智能体。"
                            } else {
                                "长期规则会带进对话和自动化，不会当成第一条消息。"
                            }),
                    ),
            )
            .children(tabs)
            .when(draft, |row| {
                row.child(
                    Button::new("agent-draft-start-conversation")
                        .commit()
                        .compact()
                        .label("开始对话")
                        .on_click(move |_, _, cx| {
                            start_entity.update(cx, |page, cx| page.commit_conversation_draft(cx));
                        }),
                )
            })
            .into_any_element()
    }

    pub(crate) fn render_agent_catalog(
        &self,
        groups: &[AgentMachineGroup],
        entity: Entity<AgentsPage>,
        cx: &App,
    ) -> AnyElement {
        // 只有一台机器时不分组，跟没挂远程机器时一样。
        let grouped = groups.len() > 1;
        let error = self.error.clone().or_else(|| {
            groups
                .iter()
                .find_map(|group| api::host(cx).agent_settings_error(&group.runtime, cx))
        });
        div()
            .id("agent-catalog-scroll")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .child(
                agent_definition_column()
                    .py_5()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(error.map(render_agent_error))
                    .children(groups.iter().map(|group| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .when(grouped, |column| {
                                column.child(render_agent_machine_header(group, cx))
                            })
                            .children(group.agents.iter().map(|agent| {
                                self.render_agent_catalog_row(&group.runtime, agent, entity.clone())
                            }))
                    })),
            )
            .into_any_element()
    }

    pub(crate) fn render_agent_catalog_row(
        &self,
        runtime: &RuntimeKey,
        agent: &AgentDefinition,
        entity: Entity<AgentsPage>,
    ) -> AnyElement {
        let agent_ref = AgentRef::new(runtime.clone(), agent.id.clone());
        let row_id = match runtime {
            RuntimeKey::Local => format!("agent-row-{}", agent.id),
            RuntimeKey::Remote(machine) => format!("agent-row-{machine}-{}", agent.id),
        };
        let open_agent = agent_ref.clone();
        let open_entity = entity;
        let name = agent_display_name(&agent.name);
        let engine_kind = agent.engine_kind();
        let engine_label = engine_kind
            .map(|kind| kind.short_label().to_string())
            .unwrap_or_else(|| agent.engine_kind_id.clone());
        let summary = agent_card_prompt(&agent.prompt);
        let show_engine = agent_engine_kinds().len() > 1;
        let automation_count = self.automation_count(&agent_ref);
        let menu_talk_entity = open_entity.clone();
        let menu_talk_agent = agent_ref.clone();
        let menu_delete_entity = open_entity.clone();
        let menu_delete_agent = agent_ref;
        content_card_style(ListRow::new(row_id).card())
            .w_full()
            .px_4()
            .py_3()
            .flex()
            .items_center()
            .gap_3()
            .child(agent_monogram(&name, 36., true))
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
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(rgb(ui_theme::text_bright()))
                                    .overflow_hidden()
                                    .child(truncated_line(name.clone())),
                            )
                            .when(show_engine, |row| {
                                row.child(
                                    div()
                                        .flex_shrink_0()
                                        .child(render_engine_chip(engine_label, engine_kind)),
                                )
                            }),
                    )
                    .when_some(summary, |column, summary| {
                        column.child(
                            div()
                                .text_xs()
                                .line_height(px(18.))
                                .text_color(rgb(ui_theme::text_muted()))
                                .child(summary),
                        )
                    }),
            )
            // 行上不放「新对话」：点行进智能体详情，那里有新对话入口；右键菜单也有。
            .on_click(move |_, window, cx| {
                let agent = open_agent.clone();
                open_entity.update(cx, |page, cx| {
                    page.open_agent(agent, AgentEditorTab::default(), window, cx)
                });
            })
            .context_menu(move |menu, _, _| {
                let talk_entity = menu_talk_entity.clone();
                let talk_agent = menu_talk_agent.clone();
                let delete_entity = menu_delete_entity.clone();
                let delete_agent = menu_delete_agent.clone();
                menu.item(PopupMenuItem::new("新对话").on_click(move |_, _, cx| {
                    let agent = talk_agent.clone();
                    talk_entity.update(cx, |page, cx| {
                        page.start_agent_conversation(agent, cx)
                    });
                }))
                .separator()
                .item(
                    PopupMenuItem::new("删除智能体")
                        .icon(api::ui::trash_icon())
                        .disabled(automation_count != 0)
                        .on_click(move |_, window, cx| {
                            let agent = delete_agent.clone();
                            delete_entity.update(cx, |page, cx| {
                                page.start_delete_agent(agent, window, cx)
                            });
                        }),
                )
            })
            .into_any_element()
    }

    pub(crate) fn render_agents_empty(&self, entity: Entity<AgentsPage>, cx: &App) -> AnyElement {
        let kind = agent_engine_kinds()
            .into_iter()
            .next()
            .unwrap_or(ConversationAgentKind::Pi);
        let machines = agent_creation_machines(cx);
        div()
            .id("agent-config-scroll")
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .px_8()
            .child(
                agent_surface_card()
                    .w_full()
                    .max_w(px(420.))
                    .px_8()
                    .py_10()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_4()
                    .child(render_engine_mark(Some(kind), 28.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_lg()
                                    .font_semibold()
                                    .text_color(rgb(ui_theme::text_bright()))
                                    .child("还没有智能体"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(ui_theme::text_muted()))
                                    .text_center()
                                    .child(if self.arm_conversation_on_create {
                                        "创建一个可复用的智能体。开始对话后会用它开场，并设为默认。返回则不保留。"
                                    } else {
                                        "写下一个可复用的智能体。对话会带着它的工作方式开始。"
                                    }),
                            ),
                    )
                    .child({
                        let button = Button::new("agents-empty-add")
                            .normal()
                            .compact()
                            .icon(IconName::Plus)
                            .label("新建智能体");
                        // 挂了不止一台机器时先选建在哪台机器上。
                        match machines.as_slice() {
                            [runtime] => {
                                let runtime = runtime.clone();
                                button
                                    .on_click(move |_, window, cx| {
                                        let runtime = runtime.clone();
                                        entity.update(cx, |page, cx| {
                                            page.create_agent(runtime, kind, window, cx)
                                        });
                                    })
                                    .into_any_element()
                            }
                            _ => button
                                .dropdown_menu(move |menu, _, cx| {
                                    fill_create_agent_menu(menu, entity.clone(), cx)
                                })
                                .into_any_element(),
                        }
                    }),
            )
            .into_any_element()
    }

    pub(crate) fn render_agent_config_panel(
        &self,
        agent: AgentDefinition,
        editor: &AgentEditor,
        entity: Entity<AgentsPage>,
        cx: &App,
    ) -> AnyElement {
        let automation_count = self.automation_count(&editor.agent);
        let error = self
            .error
            .clone()
            .or_else(|| api::host(cx).agent_settings_error(&editor.agent.runtime, cx));
        if editor.tab == AgentEditorTab::Memory {
            // 单独的滚动区：两个页签各记各的滚动位置。
            return div()
                .id("agent-memory-scroll")
                .flex_1()
                .min_w_0()
                .h_full()
                .bg(rgb(ui_theme::bg_stage()))
                .overflow_y_scroll()
                .child(
                    agent_definition_column()
                        .py_5()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .children(error.map(render_agent_error))
                        .child(render_agent_memory_card(&agent, editor, entity, cx)),
                )
                .into_any_element();
        }
        let engine_kind = agent.engine_kind();
        let engine_label = engine_kind
            .map(|kind| kind.short_label().to_string())
            .unwrap_or_else(|| agent.engine_kind_id.clone());
        let engine_control = render_agent_engine_control(
            agent.id.clone(),
            engine_label,
            engine_kind,
            entity.clone(),
        );
        let model_control = render_agent_model_control(
            editor.agent.clone(),
            agent.model_provider.clone(),
            agent.model_id.clone(),
            entity.clone(),
        );
        let prompt_chars = agent
            .prompt
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .count();
        let instructions_footer = agent_instructions_footer(prompt_chars);

        let conversation_agent = editor.agent.clone();
        let talk_entity = entity.clone();
        let talk_button = Button::new("agent-new-conversation")
            .commit()
            .compact()
            .label("新对话")
            .tooltip("新开一段对话")
            .on_click(move |_, _, cx| {
                let agent = conversation_agent.clone();
                talk_entity.update(cx, |page, cx| page.start_agent_conversation(agent, cx));
            });

        let delete_entity = entity.clone();
        let delete_agent = editor.agent.clone();
        let delete_tooltip = if automation_count == 0 {
            "删除智能体和它的记忆；已有对话会保留".to_string()
        } else {
            format!("请先删除关联的 {automation_count} 条自动化")
        };
        let delete_button = Button::new("agent-delete")
            .quiet()
            .compact()
            .icon(IconName::Ellipsis)
            .accessibility_label("更多操作")
            .tooltip(delete_tooltip)
            .dropdown_menu(move |menu, _, _| {
                let entity = delete_entity.clone();
                let agent = delete_agent.clone();
                let disabled = automation_count != 0;
                menu.item(
                    PopupMenuItem::new("删除智能体")
                        .icon(api::ui::trash_icon())
                        .disabled(disabled)
                        .on_click(move |_, window, cx| {
                            let agent = agent.clone();
                            entity
                                .update(cx, |page, cx| page.start_delete_agent(agent, window, cx));
                        }),
                )
            });

        div()
            .id("agent-config-scroll")
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(rgb(ui_theme::bg_stage()))
            .overflow_y_scroll()
            .child(
                agent_definition_column()
                    .py_5()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .children(error.map(render_agent_error))
                    .child(
                        agent_surface_card()
                            .px_5()
                            .py_4()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_4()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(render_engine_mark(engine_kind, 18.))
                                            .child(div().flex_1().min_w_0().child(
                                                render_identity_name(&editor.name),
                                            )),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_wrap()
                                            .items_center()
                                            .gap_2()
                                            .child(engine_control)
                                            .child(model_control)
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(rgb(ui_theme::text_faint()))
                                                    .child(agent_usage_summary(
                                                        agent_conversation_count(&editor.agent, cx),
                                                        automation_count,
                                                    )),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .ml_auto()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(talk_button)
                                    .child(delete_button),
                            ),
                    )
                    .child(
                        agent_surface_card()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .px_5()
                                    .pt_4()
                                    .pb_2()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_medium()
                                            .text_color(rgb(ui_theme::text_bright()))
                                            .child("工作方式"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(ui_theme::text_muted()))
                                            .child(
                                                "长期规则。对话和自动化都会带上，不会当成第一条消息。",
                                            ),
                                    ),
                            )
                            .child(
                                div().px_5().pb_3().child(framed_textarea(Textarea::new(
                                    &editor.instructions,
                                ))),
                            )
                            .child(
                                div()
                                    .px_5()
                                    .pt_1()
                                    .pb_3()
                                    .text_xs()
                                    .text_color(rgb(ui_theme::text_faint()))
                                    .child(instructions_footer),
                            ),
                    )
                    .child(render_agent_context_card(&agent, editor, entity.clone()))
                    .child(render_agent_memory_summary(&agent, editor, entity.clone()))
                    .child(render_agent_plugin_card(&agent, editor, entity.clone(), cx))
                    .child(render_agent_mcp_card(&agent, editor, entity, cx)),
            )
            .into_any_element()
    }
    pub(crate) fn render_agents_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let groups = agent_machine_groups(cx);
        let count: usize = groups.iter().map(|group| group.agents.len()).sum();
        let entity = cx.entity();
        // 编辑器里的智能体被删了，事件里会关掉编辑器；这一帧先按目录画。
        let editing = self
            .editor
            .as_ref()
            .and_then(|editor| editor.agent.definition(cx).map(|agent| (agent, editor)));
        let (header, body) = match editing {
            Some((agent, editor)) => (
                self.render_agent_editor_header(&agent.name, entity.clone(), window, cx),
                self.render_agent_config_panel(agent, editor, entity, cx),
            ),
            None => {
                let header = self.render_agents_header(count, entity.clone(), window, cx);
                let body = if count == 0 && groups.len() <= 1 {
                    self.render_agents_empty(entity, cx)
                } else {
                    self.render_agent_catalog(&groups, entity, cx)
                };
                (header, body)
            }
        };

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(ui_theme::bg_stage()))
            .child(header)
            .child(body)
            .into_any_element()
    }
}

pub(crate) fn agent_monogram(name: &str, size: f32, on_card: bool) -> Div {
    let monogram = agent_catalog_monogram(name);
    let compact = monogram.chars().count() > 1;
    div()
        .size(px(size))
        .rounded(px(10.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(if on_card {
            ui_theme::bg_stage()
        } else {
            ui_theme::bg_card()
        }))
        .text_size(px(if compact { size * 0.34 } else { size * 0.42 }))
        .font_medium()
        .text_color(rgb(ui_theme::text_bright()))
        .child(monogram)
}

/// 能开对话的智能体。挂了不止一台机器时按机器分段，段头是机器名。
pub(crate) fn fill_conversation_agent_menu(
    mut menu: PopupMenu,
    ready: Vec<ConversationAgentPick>,
    current: Option<AgentRef>,
    start: bool,
    entity: Entity<AgentsPage>,
    cx: &App,
) -> PopupMenu {
    let grouped = ready
        .iter()
        .any(|pick| pick.agent.runtime != RuntimeKey::Local);
    let mut section = None;
    for pick in ready {
        if grouped && section.as_ref() != Some(&pick.agent.runtime) {
            if section.is_some() {
                menu = menu.separator();
            }
            menu = menu.item(PopupMenuItem::label(machine_name(&pick.agent.runtime, cx)));
            section = Some(pick.agent.runtime.clone());
        }
        let entity = entity.clone();
        let checked = !start && current.as_ref() == Some(&pick.agent);
        let agent = pick.agent;
        menu = menu.item(
            PopupMenuItem::new(agent_display_name(&pick.name))
                .checked(checked)
                .on_click(move |_, _, cx| {
                    let agent = agent.clone();
                    entity.update(cx, |page, cx| {
                        page.choose_default_conversation_agent(agent, start, cx)
                    });
                }),
        );
    }
    menu
}

/// 能新建智能体的机器：配置读到了的。一台都没读到时给本机，建的时候会说还没读到。
pub(crate) fn agent_creation_machines(cx: &App) -> Vec<RuntimeKey> {
    let machines = agent_machines(cx);
    if !machines.is_empty() {
        return machines;
    }
    vec![RuntimeKey::Local]
}

/// 「新建智能体」菜单：不止一台机器时按机器分段，每段列可选的引擎。
pub(crate) fn fill_create_agent_menu(
    mut menu: PopupMenu,
    entity: Entity<AgentsPage>,
    cx: &App,
) -> PopupMenu {
    let machines = agent_creation_machines(cx);
    let grouped = machines.len() > 1;
    for (ix, runtime) in machines.into_iter().enumerate() {
        if grouped {
            if ix > 0 {
                menu = menu.separator();
            }
            menu = menu.item(PopupMenuItem::label(machine_name(&runtime, cx)));
        }
        for kind in agent_engine_kinds() {
            let entity = entity.clone();
            let runtime = runtime.clone();
            menu = menu.item(
                PopupMenuItem::new(kind.label())
                    .icon(IconName::Bot)
                    .on_click(move |_, window, cx| {
                        let runtime = runtime.clone();
                        entity.update(cx, |page, cx| page.create_agent(runtime, kind, window, cx));
                    }),
            );
        }
    }
    menu
}

/// 智能体列表里的一台机器。
pub(crate) struct AgentMachineGroup {
    runtime: RuntimeKey,
    agents: Vec<AgentDefinition>,
    /// 配置还没读到、或机器连不上时给人看的话。
    status: Option<String>,
}

/// 智能体列表按机器分组：挂着的每台机器一组，本机在前。配置还没读到的机器也列出来，
/// 说明在读，不让它像是没有智能体。
pub(crate) fn agent_machine_groups(cx: &App) -> Vec<AgentMachineGroup> {
    let mut keys: Vec<RuntimeKey> = attached_agent_machines(cx)
        .into_iter()
        .map(|machine| machine.key)
        .collect();
    if keys.is_empty() {
        keys.push(RuntimeKey::Local);
    }
    keys.into_iter()
        .map(|runtime| {
            let loaded = api::host(cx).agent_settings(&runtime, cx).is_some();
            let status = machine_unavailable(&runtime, cx)
                .or_else(|| (!loaded).then(|| "正在读取…".to_string()));
            AgentMachineGroup {
                agents: agents_on(&runtime, cx),
                runtime,
                status,
            }
        })
        .collect()
}

pub(crate) fn render_agent_machine_header(group: &AgentMachineGroup, cx: &App) -> AnyElement {
    div()
        .pt_2()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_medium()
                .text_color(rgb(ui_theme::text_muted()))
                .child(machine_name(&group.runtime, cx)),
        )
        .children(group.status.clone().map(|status| {
            div()
                .text_xs()
                .text_color(rgb(ui_theme::text_faint()))
                .child(status)
        }))
        .when(group.status.is_none() && group.agents.is_empty(), |row| {
            row.child(
                div()
                    .text_xs()
                    .text_color(rgb(ui_theme::text_faint()))
                    .child("还没有智能体"),
            )
        })
        .into_any_element()
}
