//! 智能体目录、编辑器和对话页。
//!
//! 只组 UI。写配置、开对话、改 Workspace 字段的方法在 `workspace.rs`。
use super::*;

impl Workspace {
    pub(super) fn render_agent_product_navigation_row(
        &self,
        id: &'static str,
        label: &'static str,
        icon: IconName,
        count: usize,
        route: WorkspaceRoute,
        entity: Entity<Workspace>,
    ) -> AnyElement {
        let selected = self.active_tab() == &route
            && (route != WorkspaceRoute::Agents || self.nav.agents().conversation_sid().is_none());
        div()
            .id(id)
            .h(px(32.))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded(crate::ui_theme::row_radius())
            .cursor_pointer()
            .map(|row| {
                if selected {
                    row.bg(rgb(crate::ui_theme::bg_selected()))
                } else {
                    row.hover(|row| row.bg(rgb(crate::ui_theme::bg_row_hover())))
                }
            })
            .child(Icon::new(icon).size(px(17.)).text_color(rgb(if selected {
                crate::ui_theme::accent()
            } else {
                crate::ui_theme::text_muted()
            })))
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .font_medium()
                    .text_color(rgb(if selected {
                        crate::ui_theme::text_bright()
                    } else {
                        crate::ui_theme::text_mid()
                    }))
                    .child(label),
            )
            .when(count > 0, |row| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(rgb(crate::ui_theme::text_faint()))
                        .child(count.to_string()),
                )
            })
            .on_click({
                move |_, window, cx| {
                    let route = route.clone();
                    entity.update(cx, |workspace, cx| {
                        workspace.open_agent_product_route(route, window, cx)
                    });
                }
            })
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_product_conversation_nav(
        &self,
        entity: Entity<Workspace>,
        statuses: &[AgentStatus],
        animate_running: bool,
        cx: &App,
    ) -> AnyElement {
        let config = cx.global::<settings::AgentHostState>();
        let agents: Vec<(String, String)> = config
            .agents
            .iter()
            .map(|agent| (agent.id.clone(), agent.name.clone()))
            .collect();
        let conversations: Vec<AgentConversationInput> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| session.is_agent_conversation(cx))
            .map(|(ix, session)| {
                let cwd = session.cwd(cx);
                AgentConversationInput::new(
                    ix,
                    session.agent_definition_id.as_deref(),
                    cwd.as_deref(),
                )
            })
            .collect();
        let groups = group_agent_conversations(&agents, &conversations);
        let mut rows = Vec::new();
        for (group_ix, group) in groups.iter().enumerate() {
            let collapsed =
                !group.agent_id.is_empty() && self.collapsed_agents.contains(&group.agent_id);
            let group_active = group.session_ixes.iter().any(|&ix| {
                self.sessions.get(ix).is_some_and(|session| {
                    session.active_acp().is_some_and(|view| {
                        self.active_tab() == &WorkspaceRoute::Agents
                            && self.nav.agents().conversation_sid()
                                == Some(view.read(cx).session_id())
                    })
                })
            });
            let group_status = agent_conversation_group_status(&group.session_ixes, statuses);
            let toggle_id = group.agent_id.clone();
            let toggle_entity = entity.clone();
            let can_start = !group.agent_id.is_empty();
            let start_id = group.agent_id.clone();
            let start_entity = entity.clone();
            rows.push(
                div()
                    .id(("agent-conv-header", group_ix))
                    .group(AGENT_CONV_HEADER_GROUP)
                    .h(px(32.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(crate::ui_theme::row_radius())
                    .when(can_start, |header| {
                        header
                            .cursor_pointer()
                            .hover(|row| row.bg(rgb(crate::ui_theme::bg_row_hover())))
                    })
                    .child(
                        div()
                            .w(px(14.))
                            .flex_shrink_0()
                            .flex()
                            .justify_center()
                            .children(can_start.then(|| {
                                Icon::new(if collapsed {
                                    IconName::ChevronRight
                                } else {
                                    IconName::ChevronDown
                                })
                                .size(px(13.))
                            })),
                    )
                    .child(Icon::new(IconName::Bot).size(px(15.)).text_color(rgb(
                        if group_active {
                            crate::ui_theme::text_bright()
                        } else {
                            crate::ui_theme::text_muted()
                        },
                    )))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_semibold()
                            .text_color(rgb(if group_active {
                                crate::ui_theme::text_bright()
                            } else {
                                crate::ui_theme::text_mid()
                            }))
                            .truncate()
                            .child(group.agent_name.clone()),
                    )
                    .when(group_status != AgentStatus::Idle, |header| {
                        header.child(
                            div()
                                .id(("agent-conv-status-dot", group_ix))
                                .flex_shrink_0()
                                .size(px(6.))
                                .rounded_full()
                                .bg(crate::ui_theme::session_dot_color(group_status)),
                        )
                    })
                    .child(
                        div()
                            .relative()
                            .flex_shrink_0()
                            .w(px(28.))
                            .h(px(20.))
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(11.))
                                    .text_color(rgb(crate::ui_theme::text_faint()))
                                    .group_hover(AGENT_CONV_HEADER_GROUP, |style| {
                                        style.opacity(if can_start { 0.0 } else { 1.0 })
                                    })
                                    .child(group.session_ixes.len().to_string()),
                            )
                            .when(can_start, |slot| {
                                slot.child(
                                    div()
                                        .absolute()
                                        .inset_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .opacity(0.0)
                                        .group_hover(AGENT_CONV_HEADER_GROUP, |style| {
                                            style.opacity(1.0)
                                        })
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
                                        .child(
                                            Button::new(("agent-conv-new", group_ix))
                                                .ghost()
                                                .xsmall()
                                                .icon(IconName::Plus)
                                                .on_click({
                                                    let start_id = start_id.clone();
                                                    let start_entity = start_entity.clone();
                                                    move |_, window, cx| {
                                                        cx.stop_propagation();
                                                        let id = start_id.clone();
                                                        start_entity.update(cx, |workspace, cx| {
                                                            workspace.start_agent_conversation(
                                                                id, window, cx,
                                                            )
                                                        });
                                                    }
                                                }),
                                        ),
                                )
                            }),
                    )
                    .when(can_start, |header| {
                        header.on_click(move |_, _, cx| {
                            let id = toggle_id.clone();
                            toggle_entity.update(cx, |workspace, cx| {
                                workspace.toggle_agent_conversations_collapsed(&id, cx);
                            });
                        })
                    })
                    .into_any_element(),
            );
            if collapsed {
                continue;
            }
            let mut body = div()
                .relative()
                .flex()
                .flex_col()
                .gap(px(1.))
                .pb_1()
                .child(
                    div()
                        .absolute()
                        .left(px(AGENT_GUIDE_LEFT))
                        .top_0()
                        .bottom(px(4.))
                        .w(px(1.))
                        .bg(crate::ui_theme::hairline()),
                )
                .pl(px(AGENT_SESSION_INDENT));
            for &ix in &group.session_ixes {
                let Some(session) = self.sessions.get(ix) else {
                    continue;
                };
                let Some(view) = session.active_acp() else {
                    continue;
                };
                let sid = view.read(cx).session_id().to_string();
                let selected = self.active_tab() == &WorkspaceRoute::Agents
                    && self.nav.agents().conversation_sid() == Some(sid.as_str());
                let title = nested_agent_conversation_title(
                    session.custom_title.as_deref(),
                    view.read(cx).auto_title().as_deref(),
                );
                let status = statuses.get(ix).copied().unwrap_or(AgentStatus::Idle);
                let open_ix = ix;
                let open_entity = entity.clone();
                let close_entity = entity.clone();
                let rename_entity = entity.clone();
                body = body.child(
                    div()
                        .id(("product-conversation", ix))
                        .group("product-conv-row")
                        .h(px(32.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(crate::ui_theme::row_radius())
                        .cursor_pointer()
                        .map(|row| {
                            if selected {
                                row.bg(rgb(crate::ui_theme::bg_selected()))
                            } else {
                                row.hover(|row| row.bg(rgb(crate::ui_theme::bg_row_hover())))
                            }
                        })
                        .child(conversation_status_icon(
                            ix,
                            status,
                            animate_running,
                            session.acp_kind(cx),
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .font_medium()
                                .text_color(rgb(if selected {
                                    crate::ui_theme::text_bright()
                                } else {
                                    crate::ui_theme::text_mid()
                                }))
                                .truncate()
                                .child(title),
                        )
                        .children(
                            crate::session_list::row::session_row_status_label(status).map(
                                |label| {
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(11.))
                                        .text_color(crate::ui_theme::session_dot_color(status))
                                        .child(label)
                                },
                            ),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .opacity(0.0)
                                .group_hover("product-conv-row", |s| s.opacity(1.0))
                                .child(
                                    div()
                                        .id(("close-product-conversation", ix))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .size(px(20.))
                                        .rounded(px(4.))
                                        .cursor_pointer()
                                        .text_color(rgb(crate::ui_theme::text_muted()))
                                        .hover(|d| d.text_color(rgb(crate::ui_theme::red())))
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
                                        .child(Icon::new(IconName::CircleX).size(px(14.)))
                                        .on_click(move |_, _, cx| {
                                            cx.stop_propagation();
                                            close_entity.update(cx, |workspace, cx| {
                                                workspace.close_session(open_ix, cx)
                                            });
                                        }),
                                ),
                        )
                        .on_click(move |_, window, cx| {
                            open_entity.update(cx, |workspace, cx| {
                                workspace.open_agent_conversation(open_ix, window, cx)
                            });
                        })
                        .context_menu(move |menu, _window, _cx| {
                            let rename_entity = rename_entity.clone();
                            menu.item(PopupMenuItem::new("重命名").on_click(
                                move |_ev, window, cx| {
                                    rename_entity.update(cx, |workspace, cx| {
                                        workspace.start_rename(
                                            crate::RenameTarget::Session(open_ix),
                                            window,
                                            cx,
                                        )
                                    });
                                },
                            ))
                        }),
                );
            }
            rows.push(body.into_any_element());
        }
        div()
            .pt_1()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .px_2()
                    .pb_1()
                    .text_xs()
                    .font_medium()
                    .text_color(rgb(crate::ui_theme::text_faint()))
                    .child("对话"),
            )
            .map(|section| {
                if rows.is_empty() {
                    section.child(
                        div()
                            .px_2()
                            .h(px(28.))
                            .flex()
                            .items_center()
                            .text_xs()
                            .text_color(rgb(crate::ui_theme::text_faint()))
                            .child("还没有对话"),
                    )
                } else {
                    section.children(rows)
                }
            })
            .into_any_element()
    }

    pub(super) fn render_agents_header(
        &self,
        count: usize,
        entity: Entity<Workspace>,
        _cx: &App,
    ) -> AnyElement {
        let add_entity = entity.clone();
        let engines = agent_engine_kinds();
        let add_button = if let [kind] = engines.as_slice() {
            let kind = *kind;
            Button::new("agents-add")
                .primary()
                .small()
                .icon(IconName::Plus)
                .label("新建")
                .on_click(move |_, window, cx| {
                    add_entity.update(cx, |workspace, cx| workspace.create_agent(kind, window, cx));
                })
                .into_any_element()
        } else {
            Button::new("agents-add")
                .primary()
                .small()
                .icon(IconName::Plus)
                .label("新建")
                .dropdown_menu(move |mut menu, _, _| {
                    for kind in agent_engine_kinds() {
                        let entity = add_entity.clone();
                        menu = menu.item(
                            PopupMenuItem::new(kind.label())
                                .icon(IconName::Bot)
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |workspace, cx| {
                                        workspace.create_agent(kind, window, cx)
                                    });
                                }),
                        );
                    }
                    menu
                })
                .into_any_element()
        };
        product_page_header(
            "智能体",
            "定义它是谁、怎么做事。对话和自动化都会引用这里。",
            count,
            "个",
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(render_agent_settings_entry(entity))
                .child(add_button)
                .into_any_element(),
        )
    }

    pub(super) fn render_agent_editor_header(
        &self,
        name: &str,
        entity: Entity<Workspace>,
    ) -> AnyElement {
        let back_entity = entity;
        div()
            .flex_shrink_0()
            .px_5()
            .py_4()
            .flex()
            .items_start()
            .gap_2()
            .child(
                Button::new("agent-editor-back")
                    .ghost()
                    .small()
                    .icon(IconName::ArrowLeft)
                    .tooltip("返回目录")
                    .on_click(move |_, _, cx| {
                        back_entity
                            .update(cx, |workspace, cx| workspace.close_agent_definition(cx));
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
                            .child(agent_display_name(name)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(crate::ui_theme::text_muted()))
                            .child("长期规则会带进对话和自动化，不会当成第一条消息。"),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_agent_catalog(
        &self,
        agents: &[settings::AgentDefinition],
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let error = self.agent_surface.error.clone().or_else(|| {
            cx.global::<settings::AgentHostState>()
                .persistence_error
                .clone()
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
                    .children(agents.iter().enumerate().map(|(index, agent)| {
                        self.render_agent_catalog_row(index, agent, entity.clone(), cx)
                    })),
            )
            .into_any_element()
    }

    pub(super) fn render_agent_catalog_row(
        &self,
        index: usize,
        agent: &settings::AgentDefinition,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let id = agent.id.clone();
        let open_id = id.clone();
        let talk_id = id.clone();
        let open_entity = entity.clone();
        let talk_entity = entity;
        let name = agent_display_name(&agent.name);
        let engine_kind = agent.engine_kind();
        let engine_label = engine_kind
            .map(|kind| kind.short_label().to_string())
            .unwrap_or_else(|| agent.engine_kind_id.clone());
        let automation_count = cx
            .global::<settings::AgentHostState>()
            .automations_for(&agent.id)
            .len();
        let summary = agent_prompt_summary(&agent.prompt);
        div()
            .id(("agent-row", index))
            .w_full()
            .px_4()
            .py_3()
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(crate::ui_theme::border_mid()))
            .bg(rgb(crate::ui_theme::bg_card()))
            .cursor_pointer()
            .hover(|row| {
                row.bg(rgb(crate::ui_theme::bg_hover()))
                    .border_color(rgb(crate::ui_theme::border_loud()))
            })
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(rgb(crate::ui_theme::text_bright()))
                                    .child(name),
                            )
                            .child(render_engine_chip(engine_label, engine_kind))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(crate::ui_theme::text_faint()))
                                    .child(agent_usage_summary(
                                        self.agent_conversation_count(&id, cx),
                                        automation_count,
                                    )),
                            ),
                    )
                    .children(summary.map(|summary| {
                        div()
                            .text_xs()
                            .text_color(rgb(crate::ui_theme::text_muted()))
                            .truncate()
                            .child(summary)
                    })),
            )
            .child(
                Button::new(format!("agent-row-talk-{index}"))
                    .primary()
                    .small()
                    .label("新对话")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        let id = talk_id.clone();
                        talk_entity.update(cx, |workspace, cx| {
                            workspace.start_agent_conversation(id, window, cx)
                        });
                    }),
            )
            .on_click(move |_, window, cx| {
                let id = open_id.clone();
                open_entity.update(cx, |workspace, cx| {
                    workspace.open_agent_definition(id, window, cx)
                });
            })
            .into_any_element()
    }

    pub(super) fn render_agents_empty(&self, entity: Entity<Workspace>, _cx: &App) -> AnyElement {
        let kind = agent_engine_kinds()
            .into_iter()
            .next()
            .unwrap_or(settings::ConversationAgentKind::Pi);
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
                                    .text_color(rgb(crate::ui_theme::text_bright()))
                                    .child("还没有智能体"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(crate::ui_theme::text_muted()))
                                    .text_center()
                                    .child("创建一个可复用的智能体。之后对话和自动化都会引用它。"),
                            ),
                    )
                    .child(
                        Button::new("agents-empty-add")
                            .primary()
                            .small()
                            .icon(IconName::Plus)
                            .label("新建智能体")
                            .on_click(move |_, window, cx| {
                                entity.update(cx, |workspace, cx| {
                                    workspace.create_agent(kind, window, cx)
                                });
                            }),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_agent_config_panel(
        &self,
        agent: settings::AgentDefinition,
        editor: &AgentEditor,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let automation_count = cx
            .global::<settings::AgentHostState>()
            .automations_for(&agent.id)
            .len();
        let error = self.agent_surface.error.clone().or_else(|| {
            cx.global::<settings::AgentHostState>()
                .persistence_error
                .clone()
        });
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
            agent.id.clone(),
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

        let conversation_id = agent.id.clone();
        let talk_entity = entity.clone();
        let talk_button = Button::new("agent-new-conversation")
            .primary()
            .small()
            .label("新对话")
            .tooltip("新开一段对话")
            .on_click(move |_, window, cx| {
                let id = conversation_id.clone();
                talk_entity.update(cx, |workspace, cx| {
                    workspace.start_agent_conversation(id, window, cx)
                });
            });

        let delete_entity = entity.clone();
        let delete_id = agent.id.clone();
        let delete_tooltip = if automation_count == 0 {
            "删除智能体；已有对话会保留".to_string()
        } else {
            format!("请先删除关联的 {automation_count} 条自动化")
        };
        let delete_button = Button::new("agent-delete")
            .ghost()
            .small()
            .icon(IconName::Ellipsis)
            .tooltip(delete_tooltip)
            .dropdown_menu(move |menu, _, _| {
                let entity = delete_entity.clone();
                let id = delete_id.clone();
                let disabled = automation_count != 0;
                menu.item(
                    PopupMenuItem::new("删除智能体")
                        .icon(IconName::Delete)
                        .disabled(disabled)
                        .on_click(move |_, _, cx| {
                            let id = id.clone();
                            entity.update(cx, |workspace, cx| workspace.delete_agent(id, cx));
                        }),
                )
            });

        div()
            .id("agent-config-scroll")
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(rgb(crate::ui_theme::bg_stage()))
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
                                                    .text_color(rgb(crate::ui_theme::text_faint()))
                                                    .child(agent_usage_summary(
                                                        self.agent_conversation_count(&agent.id, cx),
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
                                            .text_color(rgb(crate::ui_theme::text_bright()))
                                            .child("工作方式"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(crate::ui_theme::text_muted()))
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
                                    .text_color(rgb(crate::ui_theme::text_faint()))
                                    .child(instructions_footer),
                            ),
                    )
                    .child(render_agent_context_card(&agent, editor, entity.clone()))
                    .child(render_agent_plugin_card(&agent, editor, entity, cx)),
            )
            .into_any_element()
    }

    pub(super) fn render_product_conversation_view(
        &self,
        view: Entity<acp_view::AcpView>,
        cx: &App,
    ) -> AnyElement {
        let appearance = cx.try_global::<settings::Appearance>();
        let background_image = appearance.and_then(|appearance| appearance.bg_image.clone());
        let bg_image_opacity = appearance
            .map(|appearance| appearance.bg_image_opacity)
            .unwrap_or(0.25);
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .bg(rgb(crate::ui_theme::bg_stage()))
            .children(
                background_image.as_deref().map(|path| {
                    crate::workspace_frame::background_image_layer(path, bg_image_opacity)
                }),
            )
            .child(view)
            .into_any_element()
    }

    pub(super) fn render_agent_conversation_pane(
        &self,
        view: Entity<acp_view::AcpView>,
        cx: &App,
    ) -> AnyElement {
        self.render_product_conversation_view(view, cx)
    }

    pub(crate) fn render_agents_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let agents = cx.global::<settings::AgentHostState>().agents.clone();
        if self
            .nav
            .agents()
            .editor_id()
            .is_some_and(|id| agents.iter().all(|agent| agent.id != id))
        {
            self.nav.agents_mut().pop_to_root();
            self.agent_surface.editor = None;
        }

        let conversation = self.selected_agent_conversation_view(cx);
        let entity = cx.entity();
        if let Some((_, view)) = conversation {
            return self.render_agent_conversation_pane(view, cx);
        }

        let editor_id = self.nav.agents().editor_id().map(str::to_string);
        if editor_id.is_some() {
            self.ensure_agent_editor(window, cx);
        } else {
            self.agent_surface.editor = None;
        }

        let (header, body) = if let Some(editor_id) = editor_id {
            let selected = agents.iter().find(|agent| agent.id == editor_id).cloned();
            match (selected, self.agent_surface.editor.as_ref()) {
                (Some(agent), Some(editor)) => (
                    self.render_agent_editor_header(&agent.name, entity.clone()),
                    self.render_agent_config_panel(agent, editor, entity, cx),
                ),
                _ => (
                    self.render_agents_header(agents.len(), entity.clone(), cx),
                    self.render_agent_catalog(&agents, entity, cx),
                ),
            }
        } else {
            let header = self.render_agents_header(agents.len(), entity.clone(), cx);
            let body = if agents.is_empty() {
                self.render_agents_empty(entity, cx)
            } else {
                self.render_agent_catalog(&agents, entity, cx)
            };
            (header, body)
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
