//! 智能体目录、编辑器和对话页。
//!
//! 只组 UI。写配置、开对话、改 Workspace 字段的方法在 `workspace.rs`。
use super::*;
use gpui::StatefulInteractiveElement;

impl Workspace {
    pub(crate) fn render_delete_agent_confirm(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(target) = self.agent_surface.pending_delete.as_ref() else {
            return div().into_any_element();
        };
        let (fg, muted) = {
            let theme = cx.theme();
            (theme.foreground, theme.muted_foreground)
        };
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
                    .child(Self::modal_normal_button(
                        "cancel-delete-agent",
                        "取消",
                        |this, _, _, cx| this.cancel_delete_agent(cx),
                        cx,
                    ))
                    .child(Self::modal_danger_button(
                        "confirm-delete-agent",
                        "确定删除",
                        |this, _, _, cx| this.confirm_delete_agent(cx),
                        cx,
                    )),
            );
        self.modal_shell(
            420.,
            true,
            content,
            crate::overlay::ModalLayer::DeleteAgent,
            cx,
        )
    }

    pub(crate) fn render_delete_agent_memory_confirm(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(target) = self.agent_surface.pending_memory_delete.as_ref() else {
            return div().into_any_element();
        };
        let (fg, muted) = {
            let theme = cx.theme();
            (theme.foreground, theme.muted_foreground)
        };
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
                    .child(Self::modal_normal_button(
                        "cancel-delete-agent-memory",
                        "取消",
                        |this, _, _, cx| this.cancel_delete_agent_memory(cx),
                        cx,
                    ))
                    .child(Self::modal_danger_button(
                        "confirm-delete-agent-memory",
                        "确定删除",
                        |this, _, _, cx| this.confirm_delete_agent_memory(cx),
                        cx,
                    )),
            );
        self.modal_shell(
            420.,
            true,
            content,
            crate::overlay::ModalLayer::DeleteAgentMemory,
            cx,
        )
    }

    pub(super) fn render_agent_product_navigation_row(
        &self,
        id: &'static str,
        label: &'static str,
        icon: impl Into<Icon>,
        attention: bool,
        route: WorkspaceRoute,
        entity: Entity<Workspace>,
    ) -> AnyElement {
        let selected = self.active_tab() == &route
            && (route != WorkspaceRoute::Agents || self.nav.agents().conversation_sid().is_none());
        ListRow::new(id)
            .selected(selected)
            .h(crate::ui_theme::height::md())
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded(crate::ui_theme::row_radius())
            .child(Icon::new(icon).size(px(16.)).text_color(rgb(if selected {
                crate::ui_theme::text_mid()
            } else {
                crate::ui_theme::text_muted()
            })))
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .font_medium()
                    .text_color(rgb(if selected {
                        crate::ui_theme::text_selected()
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
    /// 「新对话」是一个真按钮。默认智能体是它的参数，名字放在右边另一个按钮上，点那个
    /// 只换默认。没有默认时点「新对话」弹出可对话的智能体。一个都没有就去空状态，不偷偷
    /// 开 Pi。
    pub(super) fn render_workbench_chat_row(
        &self,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let plan = plan_new_conversation(self.agent_surface.conversation_draft.as_ref(), cx);
        let default_name = plan.default_name.as_deref().map(agent_display_name);
        let label = new_conversation_row_label(default_name.as_deref());
        match plan.intent {
            NewConversationIntent::PickAgent => self.render_new_conversation_picker(
                entity,
                label,
                plan.ready,
                plan.stale_defaults,
                cx,
            ),
            NewConversationIntent::Start(agent) => self.render_new_conversation_action(
                entity,
                label,
                default_name.map(|name| (name, agent, plan.ready)),
            ),
            NewConversationIntent::CreateAgent => {
                self.render_new_conversation_action(entity, label, None)
            }
        }
    }

    fn render_new_conversation_action(
        &self,
        entity: Entity<Workspace>,
        label: String,
        switch: Option<(String, AgentRef, Vec<ConversationAgentPick>)>,
    ) -> AnyElement {
        let click_entity = entity.clone();
        div()
            .flex()
            .items_center()
            .gap_1p5()
            .child(new_conversation_button("workbench-chat", label).on_click(
                move |_, window, cx| {
                    click_entity.update(cx, |workspace, cx| {
                        workspace.begin_new_conversation(window, cx)
                    });
                },
            ))
            .when_some(switch, |row, (name, current, ready)| {
                let menu_entity = entity.clone();
                row.child(
                    Button::new("workbench-chat-switch")
                        .quiet()
                        .flex_shrink_1()
                        .min_w_0()
                        .max_w(px(NEW_CONVERSATION_SWITCH_MAX_WIDTH))
                        .accessibility_label("更换默认智能体")
                        .tooltip("更换默认智能体")
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(new_conversation_mark(conversation_agent_mark(Some(
                                    &name,
                                ))))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(rgb(crate::ui_theme::text_mid()))
                                        .child(name),
                                )
                                .child(
                                    Icon::new(IconName::ChevronDown)
                                        .size(px(14.))
                                        .text_color(rgb(crate::ui_theme::text_muted())),
                                ),
                        )
                        // 这一行在侧栏顶上。Bottom* 会往上翻进红绿灯，
                        // 菜单要向下展开，并贴着按钮右缘，避免冲出侧栏。
                        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, cx| {
                            fill_conversation_agent_menu(
                                menu,
                                ready.clone(),
                                Some(current.clone()),
                                false,
                                menu_entity.clone(),
                                cx,
                            )
                        }),
                )
            })
            .into_any_element()
    }

    fn render_new_conversation_picker(
        &self,
        entity: Entity<Workspace>,
        label: String,
        ready: Vec<ConversationAgentPick>,
        stale_defaults: Vec<crate::RuntimeKey>,
        _cx: &App,
    ) -> AnyElement {
        new_conversation_button("workbench-chat-pick", label)
            // 侧栏顶行，菜单向下展开。BottomLeft 会盖住红绿灯。
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, cx| {
                for runtime in &stale_defaults {
                    clear_default_conversation_agent(runtime, cx);
                }
                fill_conversation_agent_menu(menu, ready.clone(), None, true, entity.clone(), cx)
            })
            .into_any_element()
    }

    /// 侧栏「对话」分组。跟项目分组同一套：分组行点一下收起或展开，展开列出全部对话，
    /// 按最近活动排；收起时分组行写数量和最要紧的状态。一个对话都没有就整组不出现。
    /// 跟项目在同一个滚动区里，对话多了只是把项目往下推。
    pub(crate) fn render_product_conversation_nav(
        &self,
        entity: Entity<Workspace>,
        statuses: &[AgentStatus],
        // 键盘焦点在侧栏里时，焦点环落在哪一行（`SidebarItem::key`）。
        focused_key: Option<u64>,
        cx: &App,
    ) -> AnyElement {
        // 跟键盘导航同一份顺序。
        let product = self.sidebar_conversation_order(cx);
        if product.is_empty() {
            return div().into_any_element();
        }
        let ordered: Vec<(usize, u64)> = product
            .iter()
            .map(|&ix| (ix, self.sessions[ix].effective_updated_at(cx)))
            .collect();
        let selected_sid = if self.active_tab() == &WorkspaceRoute::Agents {
            self.nav.agents().conversation_sid()
        } else {
            None
        };
        let collapsed = self.sidebar_conversations_collapsed;
        let agg = product
            .iter()
            .filter_map(|&ix| statuses.get(ix).copied())
            .min_by_key(|status| status.rank())
            .unwrap_or(AgentStatus::Idle);
        let selected_inside = selected_sid.is_some_and(|sid| {
            product.iter().any(|&ix| {
                self.sessions[ix]
                    .active_acp()
                    .is_some_and(|view| view.read(cx).session_id().to_string() == sid)
            })
        });
        let toggle_entity = entity.clone();
        let group_key = crate::session_list::SidebarItem::ConversationGroup.key();
        let reveal_target = self.sidebar_reveal.target();
        // 分组行跟项目行同一个样子：图标、名字，收起时右边写数量和状态点。选中的对话被
        // 收在里面时，分组行代它亮起来。
        let header = ListRow::new("conversation-group")
            .group(CONVERSATION_GROUP_HOVER)
            .relative()
            .when(focused_key == Some(group_key), |row| {
                row.child(crate::session_list::row::focus_ring())
            })
            .when(reveal_target == Some(group_key), |row| {
                row.child(crate::session_list::reveal_anchor(
                    self.sidebar_reveal.clone(),
                    group_key,
                    self.sidebar_scroll.clone(),
                    entity.clone(),
                ))
            })
            .selected(collapsed && selected_inside)
            .h(crate::ui_theme::height::md())
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded(crate::ui_theme::row_radius())
            .child(crate::session_list::row::group_disclosure(
                "conversation-group-disclosure",
                CONVERSATION_GROUP_HOVER,
                Icon::new(gpui_kit_assets::IconName::MessageSquare).size(px(15.)),
                collapsed,
                true,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(rgb(crate::ui_theme::text_mid()))
                    .child("对话"),
            )
            .when(collapsed && agg != AgentStatus::Idle, |row| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .size(px(6.))
                        .rounded_full()
                        .bg(crate::ui_theme::session_dot_color(agg)),
                )
            })
            .when(collapsed, |row| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(rgb(crate::ui_theme::text_muted()))
                        .child(product.len().to_string()),
                )
            })
            .on_click(move |_, _, cx| {
                toggle_entity.update(cx, |workspace, cx| {
                    workspace.sidebar_conversations_collapsed =
                        !workspace.sidebar_conversations_collapsed;
                    cx.notify();
                });
            });
        let section = div().mx_2().mt_3().flex().flex_col().child(header);
        if collapsed {
            return section.into_any_element();
        }

        let now = chrono::Local::now();
        // 正在就地改名的那一行：标题那一格换成输入框。
        let renaming = match (&self.rename_target, &self.rename_input) {
            (Some(crate::RenameTarget::Session(ix)), Some(input)) => Some((*ix, input.clone())),
            _ => None,
        };
        let mut rows = Vec::new();
        for (ix, updated_at) in ordered {
            let Some(session) = self.sessions.get(ix) else {
                continue;
            };
            let Some(view) = session.active_acp() else {
                continue;
            };
            let sid = view.read(cx).session_id().to_string();
            let selected = selected_sid == Some(sid.as_str());
            let runtime = view.read(cx).runtime().key().clone();
            let title = conversation_row_title(
                session.custom_title.as_deref(),
                view.read(cx).auto_title().as_deref(),
            );
            let status = statuses.get(ix).copied().unwrap_or(AgentStatus::Idle);
            // 行尾一格：要人管时写状态；对话所在的机器用不了时写它怎么了；否则写时间。
            let meta: (SharedString, Hsla) =
                match crate::session_list::row::session_row_status_label(status) {
                    Some(label) => (
                        label.into(),
                        crate::ui_theme::session_dot_color(status).into(),
                    ),
                    None => (
                        conversation_row_meta(
                            &runtime,
                            crate::session_list::row::session_updated_at_text(updated_at, &now),
                            cx,
                        )
                        .into(),
                        rgb(crate::ui_theme::text_muted()).into(),
                    ),
                };
            let tooltip = conversation_row_tooltip(&title, &runtime, cx);
            let bound = bound_agent_definition_id(
                session.agent_definition_id.as_deref(),
                session.automation_id.as_deref(),
            );
            // 对话绑定的智能体在对话所在的那台机器上。
            let agent_name = bound.and_then(|id| {
                settings::agent_hosts(cx)?
                    .agent(&runtime, id)
                    .map(|agent| agent_display_name(&agent.name))
            });
            let mark = conversation_agent_mark(agent_name.as_deref());
            let open_ix = ix;
            let ui_id = session.ui_id;
            let open_entity = entity.clone();
            let close_entity = entity.clone();
            let rename_entity = entity.clone();
            rows.push(
                ListRow::new(format!("product-conversation-{sid}"))
                    .selected(selected)
                    .group(CONVERSATION_ROW_HOVER)
                    .relative()
                    .h(crate::ui_theme::height::md())
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(crate::ui_theme::row_radius())
                    .when(self.sidebar_flash == Some(ui_id), |row| {
                        row.child(crate::session_list::row::new_row_flash((
                            "sidebar-new-row",
                            ui_id,
                        )))
                    })
                    .when(self.sidebar_reveal.target() == Some(ui_id), |row| {
                        row.child(crate::session_list::reveal_anchor(
                            self.sidebar_reveal.clone(),
                            ui_id,
                            self.sidebar_scroll.clone(),
                            entity.clone(),
                        ))
                    })
                    .when(status == AgentStatus::NeedsYou, |row| {
                        row.child(crate::session_list::attention_anchor(
                            self.sidebar_reveal.clone(),
                            ui_id,
                        ))
                    })
                    .when(focused_key == Some(ui_id), |row| {
                        row.child(crate::session_list::row::focus_ring())
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
                            .text_xs()
                            .font_medium()
                            .text_color(conversation_icon_color(status, None))
                            .child(mark),
                    )
                    // 跟项目下的会话行一样一行：标题占满，行尾一格写时间或状态。
                    .child(match renaming.as_ref().filter(|(renaming, _)| *renaming == ix) {
                        Some((_, input)) => {
                            crate::session_list::row::inline_rename_input(input, entity.clone())
                                .into_any_element()
                        }
                        // 选中跟项目下的会话行同口径：选中字色加中等字重。
                        None => div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .when(selected, |title| title.font_medium())
                            .text_color(rgb(if selected {
                                crate::ui_theme::text_selected()
                            } else {
                                crate::ui_theme::text()
                            }))
                            .truncate()
                            .child(title)
                            .into_any_element(),
                    })
                    .child(crate::session_list::row::row_trailing_slot(
                        CONVERSATION_ROW_HOVER,
                        Some(meta),
                        Button::new(format!("close-product-conversation-{sid}"))
                            .dismiss()
                            .icon(IconName::Close)
                            .icon_action("关闭")
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                close_entity.update(cx, |workspace, cx| {
                                    workspace.request_close_session(open_ix, window, cx)
                                });
                            }),
                    ))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
                    })
                    // 双击改名：第一下已经打开了，第二下就地改名。
                    .on_click(move |ev, window, cx| {
                        open_entity.update(cx, |workspace, cx| {
                            if ev.click_count() >= 2 {
                                workspace.start_rename(
                                    crate::RenameTarget::Session(open_ix),
                                    window,
                                    cx,
                                );
                            } else {
                                workspace.open_agent_conversation(open_ix, window, cx);
                            }
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
        // 对话缩进一档挂在分组下面，左边一根细线，跟项目下的会话同一套尺寸。
        section
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .pb_1()
                    .pl(px(crate::session_list::PROJECT_SESSION_INDENT))
                    .child(
                        div()
                            .absolute()
                            .left(px(crate::session_list::PROJECT_GUIDE_LEFT))
                            .top_0()
                            .bottom(px(4.))
                            .w(px(1.))
                            .bg(crate::ui_theme::hairline()),
                    )
                    .children(rows),
            )
            .into_any_element()
    }

    pub(super) fn render_agents_header(
        &self,
        count: usize,
        entity: Entity<Workspace>,
        cx: &App,
        left_guard: Pixels,
    ) -> AnyElement {
        let add_entity = entity;
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
                        add_entity.update(cx, |workspace, cx| {
                            workspace.create_agent(runtime, kind, window, cx)
                        });
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
    ) -> AnyElement {
        let draft = self.editing_conversation_draft();
        let back_entity = entity.clone();
        let tabs = self.agent_surface.editor.as_ref().map(|editor| {
            const TABS: [AgentEditorTab; 2] = [AgentEditorTab::Config, AgentEditorTab::Memory];
            let entity = entity.clone();
            choice_tabs(
                "agent-editor-tabs",
                TABS.iter().position(|tab| *tab == editor.tab),
                TABS.map(|tab| Tab::new().label(agent_editor_tab_label(tab, editor))),
                move |ix, cx| {
                    if let Some(tab) = TABS.get(ix).copied() {
                        entity.update(cx, |workspace, cx| {
                            workspace.select_agent_editor_tab(tab, cx)
                        });
                    }
                },
            )
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
                        .commit()
                        .compact()
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
        groups: &[AgentMachineGroup],
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        // 只有一台机器时不分组，跟没挂远程机器时一样。
        let grouped = groups.len() > 1;
        let error = self.agent_surface.error.clone().or_else(|| {
            groups
                .iter()
                .find_map(|group| agent_host_error(&group.runtime, cx))
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
                                self.render_agent_catalog_row(
                                    &group.runtime,
                                    agent,
                                    entity.clone(),
                                    cx,
                                )
                            }))
                    })),
            )
            .into_any_element()
    }

    pub(super) fn render_agent_catalog_row(
        &self,
        runtime: &crate::RuntimeKey,
        agent: &settings::AgentDefinition,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let agent_ref = AgentRef::new(runtime.clone(), agent.id.clone());
        let row_id = match runtime {
            crate::RuntimeKey::Local => format!("agent-row-{}", agent.id),
            crate::RuntimeKey::Remote(machine) => format!("agent-row-{machine}-{}", agent.id),
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
        let automation_count = agent_automation_count(&agent_ref, cx);
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
                let agent = open_agent.clone();
                open_entity.update(cx, |workspace, cx| {
                    workspace.open_agent_definition(agent, window, cx)
                });
            })
            .context_menu(move |menu, _, _| {
                let talk_entity = menu_talk_entity.clone();
                let talk_agent = menu_talk_agent.clone();
                let delete_entity = menu_delete_entity.clone();
                let delete_agent = menu_delete_agent.clone();
                menu.item(PopupMenuItem::new("新对话").on_click(move |_, window, cx| {
                    let agent = talk_agent.clone();
                    talk_entity.update(cx, |workspace, cx| {
                        workspace.start_agent_conversation(agent, window, cx)
                    });
                }))
                .separator()
                .item(
                    PopupMenuItem::new("删除智能体")
                        .icon(crate::trash_icon())
                        .disabled(automation_count != 0)
                        .on_click(move |_, _, cx| {
                            let agent = delete_agent.clone();
                            delete_entity.update(cx, |workspace, cx| {
                                workspace.start_delete_agent(agent, cx)
                            });
                        }),
                )
            })
            .into_any_element()
    }

    pub(super) fn render_agents_empty(&self, entity: Entity<Workspace>, cx: &App) -> AnyElement {
        let kind = agent_engine_kinds()
            .into_iter()
            .next()
            .unwrap_or(settings::ConversationAgentKind::Pi);
        let machines = agent_creation_machines(cx);
        let needs_remote = machines.is_empty();
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
                                    .child(if needs_remote {
                                        "先配对一台远程机器。智能体配置保存在那台机器上。"
                                    } else if self.agent_surface.arm_conversation_on_create {
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
                            [] => Button::new("agents-empty-pair")
                                .normal()
                                .compact()
                                .icon(IconName::Plus)
                                .label("添加远程机器")
                                .on_click(move |_, _, cx| {
                                    entity.update(cx, |workspace, cx| {
                                        workspace.open_settings_section(
                                            crate::settings::SettingsSection::Remote,
                                            cx,
                                        );
                                    });
                                })
                                .into_any_element(),
                            [runtime] => {
                                let runtime = runtime.clone();
                                button
                                    .on_click(move |_, window, cx| {
                                        let runtime = runtime.clone();
                                        entity.update(cx, |workspace, cx| {
                                            workspace.create_agent(runtime, kind, window, cx)
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

    pub(super) fn render_agent_config_panel(
        &self,
        agent: settings::AgentDefinition,
        editor: &AgentEditor,
        entity: Entity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let automation_count = agent_automation_count(&editor.agent, cx);
        let error = self
            .agent_surface
            .error
            .clone()
            .or_else(|| agent_host_error(&editor.agent.runtime, cx));
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
            .on_click(move |_, window, cx| {
                let agent = conversation_agent.clone();
                talk_entity.update(cx, |workspace, cx| {
                    workspace.start_agent_conversation(agent, window, cx)
                });
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
                        .icon(crate::trash_icon())
                        .disabled(disabled)
                        .on_click(move |_, _, cx| {
                            let agent = agent.clone();
                            entity.update(cx, |workspace, cx| {
                                workspace.start_delete_agent(agent, cx)
                            });
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
                                                        self.agent_conversation_count(
                                                            &editor.agent,
                                                            cx,
                                                        ),
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
        let groups = agent_machine_groups(cx);
        let count: usize = groups.iter().map(|group| group.agents.len()).sum();
        let open = self.nav.agents().editor_agent().cloned();
        if open.as_ref().is_some_and(|agent| {
            !groups.iter().any(|group| {
                group.runtime == agent.runtime
                    && group
                        .agents
                        .iter()
                        .any(|definition| definition.id == agent.id)
            })
        }) {
            self.nav.agents_mut().pop_to_root();
            self.agent_surface.editor = None;
        }

        let conversation = self.selected_agent_conversation_view(cx);
        let entity = cx.entity();
        if let Some((_, view)) = conversation {
            return self.render_agent_conversation_pane(view, cx);
        }

        let open = self.nav.agents().editor_agent().cloned();
        if open.is_some() {
            self.ensure_agent_editor(window, cx);
        } else {
            self.agent_surface.editor = None;
        }

        let left_guard = self.chrome_left_guard(window);
        let (header, body) = if let Some(open) = open {
            let selected = groups
                .iter()
                .find(|group| group.runtime == open.runtime)
                .and_then(|group| group.agents.iter().find(|agent| agent.id == open.id))
                .cloned();
            match (selected, self.agent_surface.editor.as_ref()) {
                (Some(agent), Some(editor)) => (
                    self.render_agent_editor_header(&agent.name, entity.clone(), left_guard),
                    self.render_agent_config_panel(agent, editor, entity, cx),
                ),
                _ => (
                    self.render_agents_header(count, entity.clone(), cx, left_guard),
                    self.render_agent_catalog(&groups, entity, cx),
                ),
            }
        } else {
            let header = self.render_agents_header(count, entity.clone(), cx, left_guard);
            let body = if count == 0 && groups.len() <= 1 {
                self.render_agents_empty(entity, cx)
            } else {
                self.render_agent_catalog(&groups, entity, cx)
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

    /// 能力库在配哪台机器：一个按钮，点开选本机或别的机器。离线的照样能选，选了会说
    /// 读不到。
    fn render_pi_library_picker(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let current = self.pi_library_runtime().clone();
        let entity = cx.entity();
        Button::new("capabilities-runtime")
            .quiet()
            .compact()
            .label(format!(
                "机器：{}",
                crate::runtimes::runtime_name(&current, cx)
            ))
            .icon(IconName::ChevronDown)
            .dropdown_menu(move |menu, _window, cx| {
                let mut runtimes = vec![crate::RuntimeKey::Local];
                if let Some(hub) = cx.try_global::<crate::runtimes::RuntimeHub>() {
                    runtimes.extend(
                        hub.remotes
                            .iter()
                            .map(|remote| remote.runtime.key().clone()),
                    );
                }
                runtimes.into_iter().fold(menu, |menu, key| {
                    let mut label = crate::runtimes::runtime_name(&key, cx);
                    if let Some(unavailable) = crate::runtimes::runtime_unavailable_label(&key, cx)
                    {
                        label = format!("{label}（{unavailable}）");
                    }
                    let entity = entity.clone();
                    let checked = key == current;
                    menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                        move |_, _window, cx| {
                            let key = key.clone();
                            entity.update(cx, |workspace, cx| {
                                workspace.select_pi_library_runtime(key, cx)
                            });
                        },
                    ))
                })
            })
    }

    fn render_capabilities_needs_runtime(&self, entity: Entity<Workspace>, cx: &App) -> AnyElement {
        let (fg, muted) = {
            let theme = cx.theme();
            (theme.foreground, theme.muted_foreground)
        };
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .px_8()
            .bg(rgb(crate::ui_theme::bg_stage()))
            .child(
                div()
                    .text_size(px(28.))
                    .font_semibold()
                    .text_color(fg)
                    .child("添加远程机器"),
            )
            .child(
                div()
                    .max_w(px(420.))
                    .text_sm()
                    .text_center()
                    .text_color(muted)
                    .child("模型、技能和插件跟运行时所在的机器走。配对一台远程机器后就能配置。"),
            )
            .child(
                Button::new("capabilities-add-runtime")
                    .commit()
                    .capsule()
                    .label("添加远程机器")
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |workspace, cx| {
                            workspace.open_settings_section(
                                crate::settings::SettingsSection::Remote,
                                cx,
                            );
                        });
                    }),
            )
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
        if crate::runtimes::client(self.pi_library_runtime(), cx).is_none() {
            return self.render_capabilities_needs_runtime(entity, cx);
        }
        let current = self.nav.capabilities_tab();
        let tabs = choice_tabs(
            "capabilities-tabs",
            CapabilitiesTab::ALL.iter().position(|tab| *tab == current),
            CapabilitiesTab::ALL.map(|tab| Tab::new().label(tab.label())),
            move |ix, cx| {
                if let Some(tab) = CapabilitiesTab::ALL.get(ix).copied() {
                    entity.update(cx, |workspace, cx| {
                        workspace.select_capabilities_tab(tab, cx)
                    });
                }
            },
        );
        let header = product_page_chrome(self.chrome_left_guard(window))
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .font_medium()
                    .text_color(rgb(crate::ui_theme::text_bright()))
                    .child("能力库"),
            )
            .child(tabs)
            // 挂了别的机器，才需要选在配哪台。
            .when(crate::runtimes::has_remote_runtimes(cx), |header| {
                header
                    .child(div().flex_1())
                    .child(self.render_pi_library_picker(cx))
            });

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
        .text_xs()
        .font_medium()
        .text_color(rgb(crate::ui_theme::text_muted()))
        .child(mark)
}

/// 默认智能体那个按钮最宽多少：名字再长就截断，「新对话」始终占大头。
const NEW_CONVERSATION_SWITCH_MAX_WIDTH: f32 = 128.;

/// 侧栏最顶的「新对话」按钮。`label` 带着默认智能体的名字，给读屏用。悬停提示写上
/// ⌘N。
fn new_conversation_button(id: &'static str, label: String) -> Button {
    Button::new(id)
        .normal()
        .flex_1()
        .min_w_0()
        .icon(IconName::Plus)
        .label("新对话")
        .accessibility_label(label)
        .tooltip_with_action("新对话", &crate::NewConversation, None)
}

/// 能开对话的智能体。挂了不止一台机器时按机器分段，段头是机器名。
fn fill_conversation_agent_menu(
    mut menu: PopupMenu,
    ready: Vec<ConversationAgentPick>,
    current: Option<AgentRef>,
    start: bool,
    entity: Entity<Workspace>,
    cx: &App,
) -> PopupMenu {
    let grouped = ready
        .iter()
        .any(|pick| pick.agent.runtime != crate::RuntimeKey::Local);
    let mut section = None;
    for pick in ready {
        if grouped && section.as_ref() != Some(&pick.agent.runtime) {
            if section.is_some() {
                menu = menu.separator();
            }
            menu = menu.item(PopupMenuItem::label(crate::runtimes::runtime_name(
                &pick.agent.runtime,
                cx,
            )));
            section = Some(pick.agent.runtime.clone());
        }
        let entity = entity.clone();
        let checked = !start && current.as_ref() == Some(&pick.agent);
        let agent = pick.agent;
        menu = menu.item(
            PopupMenuItem::new(agent_display_name(&pick.name))
                .checked(checked)
                .on_click(move |_, window, cx| {
                    let agent = agent.clone();
                    entity.update(cx, |workspace, cx| {
                        workspace.choose_default_conversation_agent(agent, start, window, cx)
                    });
                }),
        );
    }
    menu
}

/// 能新建智能体的机器：配置读到了的。一台都没读到时给本机，建的时候会说还没读到。
fn agent_creation_machines(cx: &App) -> Vec<crate::RuntimeKey> {
    let machines = agent_machines(cx);
    if !machines.is_empty() {
        return machines;
    }
    vec![crate::RuntimeKey::Local]
}

/// 「新建智能体」菜单：不止一台机器时按机器分段，每段列可选的引擎。
fn fill_create_agent_menu(mut menu: PopupMenu, entity: Entity<Workspace>, cx: &App) -> PopupMenu {
    let machines = agent_creation_machines(cx);
    let grouped = machines.len() > 1;
    for (ix, runtime) in machines.into_iter().enumerate() {
        if grouped {
            if ix > 0 {
                menu = menu.separator();
            }
            menu = menu.item(PopupMenuItem::label(crate::runtimes::runtime_name(
                &runtime, cx,
            )));
        }
        for kind in agent_engine_kinds() {
            let entity = entity.clone();
            let runtime = runtime.clone();
            menu = menu.item(
                PopupMenuItem::new(kind.label())
                    .icon(IconName::Bot)
                    .on_click(move |_, window, cx| {
                        let runtime = runtime.clone();
                        entity.update(cx, |workspace, cx| {
                            workspace.create_agent(runtime, kind, window, cx)
                        });
                    }),
            );
        }
    }
    menu
}

/// 智能体列表里的一台机器。
pub(super) struct AgentMachineGroup {
    runtime: crate::RuntimeKey,
    agents: Vec<settings::AgentDefinition>,
    /// 配置还没读到、或机器连不上时给人看的话。
    status: Option<String>,
}

/// 智能体列表按机器分组：挂着的每台机器一组，本机在前。配置还没读到的机器也列出来，
/// 说明在读，不让它像是没有智能体。
fn agent_machine_groups(cx: &App) -> Vec<AgentMachineGroup> {
    let mut keys = attached_agent_machines(cx);
    if keys.is_empty() {
        keys.push(crate::RuntimeKey::Local);
    }
    keys.into_iter()
        .map(|runtime| {
            let loaded = settings::agent_host_loaded(&runtime, cx);
            let status = crate::runtimes::runtime_unavailable_label(&runtime, cx)
                .map(str::to_string)
                .or_else(|| (!loaded).then(|| "正在读取…".to_string()));
            AgentMachineGroup {
                agents: settings::agents_on(&runtime, cx).to_vec(),
                runtime,
                status,
            }
        })
        .collect()
}

fn render_agent_machine_header(group: &AgentMachineGroup, cx: &App) -> AnyElement {
    div()
        .pt_2()
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
        .when(group.status.is_none() && group.agents.is_empty(), |row| {
            row.child(
                div()
                    .text_xs()
                    .text_color(rgb(crate::ui_theme::text_faint()))
                    .child("还没有智能体"),
            )
        })
        .into_any_element()
}

/// 「最近对话」行的第二行。本机的只写时间；别的机器上的在前面标机器名，连不上时写状态：
/// 智能体对话不按机器分组，只能在行上说清楚它在哪台机器上。
/// 对话行行尾那一格：对话所在的机器用不了时写它怎么了（离线、版本不兼容……），
/// 否则写最近活动时间。机器名放不进这一格，写在悬停提示里。
fn conversation_row_meta(runtime: &crate::RuntimeKey, updated: String, cx: &App) -> String {
    crate::runtimes::runtime_unavailable_label(runtime, cx)
        .map(str::to_string)
        .unwrap_or(updated)
}

/// 对话行的悬停提示：完整标题，在远程机器上的再带上机器名。
fn conversation_row_tooltip(title: &str, runtime: &crate::RuntimeKey, cx: &App) -> SharedString {
    match runtime {
        crate::RuntimeKey::Local => title.to_string().into(),
        crate::RuntimeKey::Remote(_) => {
            format!("{title} · {}", crate::runtimes::runtime_name(runtime, cx)).into()
        }
    }
}

/// 「对话」分组行、对话行的 hover group 名。
const CONVERSATION_GROUP_HOVER: &str = "conversation-group-hover";
const CONVERSATION_ROW_HOVER: &str = "product-conv-row";
