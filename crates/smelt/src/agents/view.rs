//! 智能体目录、编辑器和对话页。
//!
//! 只组 UI。写配置、开对话、改 Workspace 字段的方法在 `workspace.rs`。
use super::*;

impl Workspace {
    pub(crate) fn render_delete_agent_confirm(&self, cx: &mut Context<Self>) -> Div {
        let Some(target) = self.agent_surface.pending_delete.as_ref() else {
            return div();
        };
        let (fg, muted) = {
            let theme = cx.theme();
            (theme.foreground, theme.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);
        let kept = if target.conversations == 0 {
            String::new()
        } else {
            format!("已有的 {} 段对话会保留。", target.conversations)
        };
        let content = div()
            .flex()
            .flex_col()
            .child(Self::modal_title(fg, "确定删除这个智能体吗？"))
            .child(div().text_sm().text_color(muted).child(format!(
                "将删除智能体「{}」和它的指令、插件设置。{kept}此操作不可撤销。",
                target.name
            )))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-delete-agent",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_delete_agent(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-delete-agent",
                        "确定删除",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_delete_agent(cx),
                        cx,
                    )),
            );
        Self::modal_shell(420., true, content, cx)
    }

    pub(crate) fn render_delete_agent_memory_confirm(&self, cx: &mut Context<Self>) -> Div {
        let Some(target) = self.agent_surface.pending_memory_delete.as_ref() else {
            return div();
        };
        let (fg, muted) = {
            let theme = cx.theme();
            (theme.foreground, theme.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);
        let scope = match target.layer {
            MemoryLayer::Agent => "这个智能体之后不会再用到它。",
            MemoryLayer::User => "这是全局记忆，所有智能体之后都不会再用到它。",
        };
        let content = div()
            .flex()
            .flex_col()
            .child(Self::modal_title(fg, "确定删除这条记忆吗？"))
            .child(div().text_sm().text_color(muted).child(format!(
                "将删除记忆「{}」和它的使用记录。{scope}此操作不可撤销。",
                target.name
            )))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-delete-agent-memory",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_delete_agent_memory(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-delete-agent-memory",
                        "确定删除",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_delete_agent_memory(cx),
                        cx,
                    )),
            );
        Self::modal_shell(420., true, content, cx)
    }

    pub(super) fn render_agent_product_navigation_row(
        &self,
        id: &'static str,
        label: &'static str,
        icon: IconName,
        attention: bool,
        route: WorkspaceRoute,
        entity: Entity<Workspace>,
    ) -> AnyElement {
        let selected = self.active_tab() == &route
            && (route != WorkspaceRoute::Agents || self.nav.agents().conversation_sid().is_none());
        div()
            .id(id)
            .h(crate::ui_theme::height::md())
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
            .child(Icon::new(icon).size(px(16.)).text_color(rgb(if selected {
                crate::ui_theme::text_mid()
            } else {
                crate::ui_theme::text_muted()
            })))
            .child(
                div()
                    .flex_1()
                    .text_size(px(crate::session_list::SIDEBAR_FONT_SIZE))
                    .font_medium()
                    .text_color(rgb(if selected {
                        crate::ui_theme::text_bright()
                    } else {
                        crate::ui_theme::text_mid()
                    }))
                    .child(label),
            )
            .when(attention, |row| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .size(px(6.))
                        .rounded_full()
                        .bg(rgb(crate::ui_theme::red())),
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

    /// 侧栏最顶「新对话」：跟某个智能体开一场，不是裸引擎。
    ///
    /// 有默认时单击直接开，右侧箭头只换默认。没有默认时弹出可对话的智能体。
    /// 一个都没有就去空状态，不偷偷开 Pi。
    pub(super) fn render_workbench_chat_row(
        &self,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let stored = stored_default_conversation_agent();
        let resolution = smelt_core::default_conversation::resolve_new_conversation(
            &cx.global::<settings::AgentHostState>().agents,
            stored.as_deref(),
            self.agent_surface.conversation_draft_id.as_deref(),
        );
        let default_name = resolution.default_name.as_deref().map(agent_display_name);
        let label = new_conversation_row_label(default_name.as_deref());
        let mark = default_name
            .as_deref()
            .map(|name| conversation_agent_mark(Some(name)));
        match resolution.intent {
            smelt_core::default_conversation::NewConversationIntent::PickAgent => self
                .render_new_conversation_picker(
                    entity,
                    label,
                    resolution.ready,
                    resolution.clear_stored_default,
                    cx,
                ),
            smelt_core::default_conversation::NewConversationIntent::Start { agent_id } => self
                .render_new_conversation_action(
                    entity,
                    label,
                    mark,
                    Some((agent_id, resolution.ready)),
                ),
            smelt_core::default_conversation::NewConversationIntent::CreateAgent => {
                self.render_new_conversation_action(entity, label, mark, None)
            }
        }
    }

    fn render_new_conversation_action(
        &self,
        entity: Entity<Workspace>,
        label: String,
        mark: Option<String>,
        switch: Option<(
            String,
            Vec<smelt_core::default_conversation::ConversationAgentChoice>,
        )>,
    ) -> AnyElement {
        let click_entity = entity.clone();
        div()
            .id("workbench-chat")
            .h(crate::ui_theme::height::md())
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded(crate::ui_theme::row_radius())
            .cursor_pointer()
            .hover(|row| row.bg(rgb(crate::ui_theme::bg_row_hover())))
            .child(new_conversation_row_body(label, mark).flex_1())
            .when_some(switch, |row, (current_id, ready)| {
                let menu_entity = entity.clone();
                row.child(
                    div()
                        .flex_shrink_0()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            Button::new("workbench-chat-switch")
                                .ghost()
                                .size(px(22.))
                                .icon(IconName::ChevronDown)
                                .tooltip("更换默认智能体")
                                // 这一行在侧栏顶上。Bottom* 会往上翻进红绿灯，
                                // 菜单要向下展开，并贴着箭头右缘，避免冲出侧栏。
                                .dropdown_menu_with_anchor(
                                    Anchor::TopRight,
                                    move |menu, _, _| {
                                        fill_conversation_agent_menu(
                                            menu,
                                            ready.clone(),
                                            Some(current_id.clone()),
                                            false,
                                            menu_entity.clone(),
                                        )
                                    },
                                ),
                        ),
                )
            })
            .on_click(move |_, window, cx| {
                click_entity.update(cx, |workspace, cx| {
                    workspace.begin_new_conversation(window, cx)
                });
            })
            .into_any_element()
    }

    fn render_new_conversation_picker(
        &self,
        entity: Entity<Workspace>,
        label: String,
        ready: Vec<smelt_core::default_conversation::ConversationAgentChoice>,
        clear_stored: bool,
        cx: &App,
    ) -> AnyElement {
        div()
            .id("workbench-chat")
            .h(crate::ui_theme::height::md())
            .rounded(crate::ui_theme::row_radius())
            .cursor_pointer()
            .hover(|row| row.bg(rgb(crate::ui_theme::bg_row_hover())))
            .child(
                Button::new("workbench-chat-pick")
                    .custom(ButtonCustomVariant::new(cx))
                    .h(crate::ui_theme::height::md())
                    .w_full()
                    .px_2()
                    .cursor_pointer()
                    .child(new_conversation_row_body(label, None).w_full())
                    // 侧栏顶行，菜单向下展开。BottomLeft 会盖住红绿灯。
                    .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                        if clear_stored {
                            clear_default_conversation_agent();
                        }
                        fill_conversation_agent_menu(
                            menu,
                            ready.clone(),
                            None,
                            true,
                            entity.clone(),
                        )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn render_product_conversation_nav(
        &self,
        entity: Entity<Workspace>,
        statuses: &[AgentStatus],
        _animate_running: bool,
        cx: &App,
    ) -> AnyElement {
        let config = cx.global::<settings::AgentHostState>();
        let product: Vec<usize> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| session.is_agent_conversation(cx))
            .map(|(ix, _)| ix)
            .collect();
        let updated: Vec<u64> = product
            .iter()
            .map(|&ix| self.sessions[ix].effective_updated_at(cx))
            .collect();
        let ixes: Vec<usize> = recent_conversation_order(&updated)
            .into_iter()
            .map(|order| product[order])
            .collect();
        let mut rows = Vec::new();
        for &ix in &ixes {
            let Some(session) = self.sessions.get(ix) else {
                continue;
            };
            let Some(view) = session.active_acp() else {
                continue;
            };
            let sid = view.read(cx).session_id().to_string();
            let selected = self.active_tab() == &WorkspaceRoute::Agents
                && self.nav.agents().conversation_sid() == Some(sid.as_str());
            let title = conversation_row_title(
                session.custom_title.as_deref(),
                view.read(cx).auto_title().as_deref(),
            );
            let status = statuses.get(ix).copied().unwrap_or(AgentStatus::Idle);
            let bound = bound_agent_definition_id(
                session.agent_definition_id.as_deref(),
                session.automation_id.as_deref(),
            );
            let agent_name = bound.and_then(|id| {
                config
                    .agents
                    .iter()
                    .find(|agent| agent.id == id)
                    .map(|agent| agent_display_name(&agent.name))
            });
            let mark = conversation_agent_mark(agent_name.as_deref());
            let open_ix = ix;
            let open_entity = entity.clone();
            let close_entity = entity.clone();
            let rename_entity = entity.clone();
            rows.push(
                div()
                    .id(format!("product-conversation-{sid}"))
                    .group("product-conv-row")
                    .h(crate::ui_theme::height::md())
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
                    .child(
                        div()
                            .flex_shrink_0()
                            .size(px(18.))
                            .rounded_full()
                            .bg(rgb(crate::ui_theme::bg_selected()))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(10.))
                            .font_medium()
                            .text_color(conversation_icon_color(status, None))
                            .child(mark),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::session_list::SIDEBAR_FONT_SIZE))
                            .text_color(rgb(if selected {
                                crate::ui_theme::text_bright()
                            } else {
                                crate::ui_theme::text()
                            }))
                            .truncate()
                            .child(title),
                    )
                    .children(
                        crate::session_list::row::session_row_status_label(status).map(|label| {
                            div()
                                .flex_shrink_0()
                                .text_size(px(11.))
                                .text_color(crate::ui_theme::session_dot_color(status))
                                .child(label)
                        }),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .opacity(0.0)
                            .group_hover("product-conv-row", |s| s.opacity(1.0))
                            .child(
                                div()
                                    .id(format!("close-product-conversation-{sid}"))
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
                                    .child(Icon::new(IconName::Close).size(px(14.)))
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
                        menu.item(
                            PopupMenuItem::new("重命名")
                                .icon(Icon::empty().path("smelt-icons/square-pen.svg"))
                                .on_click(move |_ev, window, cx| {
                                    rename_entity.update(cx, |workspace, cx| {
                                        workspace.start_rename(
                                            crate::RenameTarget::Session(open_ix),
                                            window,
                                            cx,
                                        )
                                    });
                                }),
                        )
                    })
                    .into_any_element(),
            );
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
                    .text_size(px(crate::session_list::SIDEBAR_FONT_SIZE))
                    .font_medium()
                    .text_color(rgb(crate::ui_theme::text_faint()))
                    .child("最近对话"),
            )
            .map(|section| {
                if rows.is_empty() {
                    section.child(
                        div()
                            .px_2()
                            .h(crate::ui_theme::height::sm())
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
        left_guard: Pixels,
    ) -> AnyElement {
        let add_entity = entity.clone();
        let engines = agent_engine_kinds();
        let add_button = if let [kind] = engines.as_slice() {
            let kind = *kind;
            Button::new("agents-add")
                .primary()
                .icon(IconName::Plus)
                .label("新建智能体")
                .on_click(move |_, window, cx| {
                    add_entity.update(cx, |workspace, cx| workspace.create_agent(kind, window, cx));
                })
                .into_any_element()
        } else {
            Button::new("agents-add")
                .primary()
                .icon(IconName::Plus)
                .label("新建智能体")
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
            count,
            "个",
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(add_button)
                .into_any_element(),
            left_guard,
        )
    }

    pub(super) fn render_agent_editor_header(
        &self,
        name: &str,
        entity: Entity<Workspace>,
        left_guard: Pixels,
        cx: &App,
    ) -> AnyElement {
        let draft = self.editing_conversation_draft();
        let back_entity = entity.clone();
        let tabs = self.agent_surface.editor.as_ref().map(|editor| {
            segmented_track(cx).children([AgentEditorTab::Config, AgentEditorTab::Memory].map(
                |tab| {
                    let selected = tab == editor.tab;
                    let entity = entity.clone();
                    compact_chip(match tab {
                        AgentEditorTab::Config => "agent-editor-tab-config",
                        AgentEditorTab::Memory => "agent-editor-tab-memory",
                    })
                    .label(agent_editor_tab_label(tab, editor))
                    .selected(selected)
                    .toggled(selected)
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |workspace, cx| {
                            workspace.select_agent_editor_tab(tab, cx)
                        });
                    })
                },
            ))
        });
        let start_entity = entity;
        product_page_chrome(left_guard)
            .gap_2()
            .child(product_back_button(
                "agent-editor-back",
                if draft { "取消" } else { "返回" },
                move |_, window, cx| {
                    back_entity.update(cx, |workspace, cx| {
                        workspace.close_agent_settings(window, cx)
                    });
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
                            .text_color(rgb(crate::ui_theme::text_bright()))
                            .truncate()
                            .child(agent_display_name(name)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(crate::ui_theme::text_muted()))
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
                        .primary()
                        .small()
                        .label("开始对话")
                        .on_click(move |_, window, cx| {
                            start_entity.update(cx, |workspace, cx| {
                                workspace.commit_conversation_draft(window, cx)
                            });
                        }),
                )
            })
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
                    .children(
                        agents
                            .iter()
                            .map(|agent| self.render_agent_catalog_row(agent, entity.clone(), cx)),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_agent_catalog_row(
        &self,
        agent: &settings::AgentDefinition,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let id = agent.id.clone();
        let open_id = id.clone();
        let open_entity = entity;
        let name = agent_display_name(&agent.name);
        let engine_kind = agent.engine_kind();
        let engine_label = engine_kind
            .map(|kind| kind.short_label().to_string())
            .unwrap_or_else(|| agent.engine_kind_id.clone());
        let summary = agent_card_prompt(&agent.prompt);
        let show_engine = agent_engine_kinds().len() > 1;
        let automation_count = cx
            .global::<settings::AgentHostState>()
            .automations_for(&agent.id)
            .len();
        let menu_talk_entity = open_entity.clone();
        let menu_talk_id = id.clone();
        let menu_delete_entity = open_entity.clone();
        let menu_delete_id = id.clone();
        agent_surface_card()
            .id(format!("agent-row-{id}"))
            .w_full()
            .px_4()
            .py_3()
            .cursor_pointer()
            .flex()
            .items_center()
            .gap_3()
            .hover(|card| card.bg(rgb(crate::ui_theme::bg_hover())))
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
                                    .text_color(rgb(crate::ui_theme::text_bright()))
                                    .truncate()
                                    .child(name.clone()),
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
                                .text_color(rgb(crate::ui_theme::text_muted()))
                                .child(summary),
                        )
                    }),
            )
            // 行上不放「新对话」：点行进智能体详情，那里有新对话入口；右键菜单也有。
            .on_click(move |_, window, cx| {
                let id = open_id.clone();
                open_entity.update(cx, |workspace, cx| {
                    workspace.open_agent_definition(id, window, cx)
                });
            })
            .context_menu(move |menu, _, _| {
                let talk_entity = menu_talk_entity.clone();
                let talk_id = menu_talk_id.clone();
                let delete_entity = menu_delete_entity.clone();
                let delete_id = menu_delete_id.clone();
                menu.item(PopupMenuItem::new("新对话").on_click(move |_, window, cx| {
                    let id = talk_id.clone();
                    talk_entity.update(cx, |workspace, cx| {
                        workspace.start_agent_conversation(id, window, cx)
                    });
                }))
                .separator()
                .item(
                    PopupMenuItem::new("删除智能体")
                        .icon(crate::trash_icon())
                        .disabled(automation_count != 0)
                        .on_click(move |_, _, cx| {
                            let id = delete_id.clone();
                            delete_entity
                                .update(cx, |workspace, cx| workspace.start_delete_agent(id, cx));
                        }),
                )
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
                                    .child(if self.agent_surface.arm_conversation_on_create {
                                        "创建一个可复用的智能体。开始对话后会用它开场，并设为默认。返回则不保留。"
                                    } else {
                                        "写下一个可复用的智能体。对话会带着它的工作方式开始。"
                                    }),
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
        if editor.tab == AgentEditorTab::Memory {
            // 单独的滚动区：两个页签各记各的滚动位置。
            return div()
                .id("agent-memory-scroll")
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
            "删除智能体和它的记忆；已有对话会保留".to_string()
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
                        .icon(crate::trash_icon())
                        .disabled(disabled)
                        .on_click(move |_, _, cx| {
                            let id = id.clone();
                            entity.update(cx, |workspace, cx| workspace.start_delete_agent(id, cx));
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
                                                        self.agent_conversation_count(&agent.id),
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
                    .child(render_agent_memory_summary(&agent, editor, entity.clone()))
                    .child(render_agent_plugin_card(&agent, editor, entity.clone(), cx))
                    .child(render_agent_mcp_card(&agent, editor, entity, cx)),
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
            .children(background_image.as_deref().map(|path| {
                crate::workspace_frame::background_image_layer(path, bg_image_opacity, cx)
            }))
            .child(view.cached(gpui::StyleRefinement::default().size_full()))
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
        let open_id = self.nav.agents().editor_id().map(str::to_string);
        if open_id
            .as_deref()
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

        let left_guard = self.chrome_left_guard(window);
        let (header, body) = if let Some(editor_id) = editor_id {
            let selected = agents.iter().find(|agent| agent.id == editor_id).cloned();
            match (selected, self.agent_surface.editor.as_ref()) {
                (Some(agent), Some(editor)) => (
                    self.render_agent_editor_header(&agent.name, entity.clone(), left_guard, cx),
                    self.render_agent_config_panel(agent, editor, entity, cx),
                ),
                _ => (
                    self.render_agents_header(agents.len(), entity.clone(), cx, left_guard),
                    self.render_agent_catalog(&agents, entity, cx),
                ),
            }
        } else {
            let header = self.render_agents_header(agents.len(), entity.clone(), cx, left_guard);
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
            .child(header)
            .child(body)
            .into_any_element()
    }

    /// 能力库：页头和智能体、自动化同一条，左边页名接三个页签。正文是独立的设置内容视图，
    /// 自己节流刷新快照，终端输出这类高频通知不会让它每帧重读 Pi 配置。
    pub(crate) fn render_capabilities_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        let current = self.nav.capabilities_tab();
        // 选中块从上一个页签滑到这一个，而不是两格之间瞬间换底色。
        let tabs = smelt_ui::segmented::segmented_track(
            "capabilities-tabs",
            smelt_ui::segmented::SegmentedVariant::Pill,
            CapabilitiesTab::ALL.iter().position(|tab| *tab == current),
        )
        .children(CapabilitiesTab::ALL.into_iter().map(|tab| {
            let entity = entity.clone();
            smelt_ui::segmented::pill_segment(tab.element_id(), tab.label(), tab == current)
                .on_click(move |_, _, cx| {
                    entity.update(cx, |workspace, cx| {
                        workspace.select_capabilities_tab(tab, cx)
                    });
                })
        }));
        let header = product_page_chrome(self.chrome_left_guard(window))
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .font_medium()
                    .text_color(rgb(crate::ui_theme::text_bright()))
                    .child("能力库"),
            )
            .child(tabs);

        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(crate::ui_theme::bg_stage()))
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .children(self.capabilities_view.clone()),
            )
            .into_any_element()
    }
}

fn agent_monogram(name: &str, size: f32, on_card: bool) -> Div {
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
            crate::ui_theme::bg_stage()
        } else {
            crate::ui_theme::bg_card()
        }))
        .text_size(px(if compact { size * 0.34 } else { size * 0.42 }))
        .font_medium()
        .text_color(rgb(crate::ui_theme::text_bright()))
        .child(monogram)
}

fn new_conversation_mark(mark: String) -> Div {
    div()
        .flex_shrink_0()
        .size(px(18.))
        .rounded_full()
        .bg(rgb(crate::ui_theme::bg_selected()))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(10.))
        .font_medium()
        .text_color(rgb(crate::ui_theme::text_muted()))
        .child(mark)
}

fn new_conversation_row_body(label: String, mark: Option<String>) -> Div {
    div()
        .min_w_0()
        .flex()
        .items_center()
        .gap_2()
        .child(match mark {
            Some(mark) => new_conversation_mark(mark),
            None => div().flex_shrink_0().child(
                Icon::empty()
                    .path("smelt-icons/square-pen.svg")
                    .size(px(16.))
                    .text_color(rgb(crate::ui_theme::text_muted())),
            ),
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(crate::session_list::SIDEBAR_FONT_SIZE))
                .font_medium()
                .text_color(rgb(crate::ui_theme::text_mid()))
                .child(label),
        )
}

fn fill_conversation_agent_menu(
    mut menu: PopupMenu,
    ready: Vec<smelt_core::default_conversation::ConversationAgentChoice>,
    current_id: Option<String>,
    start: bool,
    entity: Entity<Workspace>,
) -> PopupMenu {
    for choice in ready {
        let entity = entity.clone();
        let id = choice.id;
        let checked = !start && current_id.as_deref() == Some(id.as_str());
        menu = menu.item(
            PopupMenuItem::new(agent_display_name(&choice.name))
                .checked(checked)
                .on_click(move |_, window, cx| {
                    let id = id.clone();
                    entity.update(cx, |workspace, cx| {
                        workspace.choose_default_conversation_agent(id, start, window, cx)
                    });
                }),
        );
    }
    menu
}
