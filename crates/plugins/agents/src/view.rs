//! 智能体页面：目录、编辑页和各种菜单。

use super::*;
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

impl AgentsPage {
    pub(crate) fn render_agents_header(
        &self,
        count: usize,
        entity: Entity<AgentsPage>,
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
                    .icon(IconName::Plus)
                    .label(t!("agent.new"))
                    .on_click(move |_, window, cx| {
                        let runtime = runtime.clone();
                        add_entity
                            .update(cx, |page, cx| page.create_agent(runtime, kind, window, cx));
                    })
                    .into_any_element()
            }
            _ => MenuTrigger::dropdown(
                "agents-add-menu",
                Button::new("agents-add")
                    .icon(IconName::Plus)
                    .label(t!("agent.new")),
                move |_, cx| fill_create_agent_menu(Menu::new(), add_entity.clone(), cx),
            )
            .into_any_element(),
        };
        let count_label = match count {
            1 => t!("page.count.one").into_owned(),
            count => t!("page.count.other", count = count).into_owned(),
        };
        product_page_header(
            count_label,
            div()
                .flex()
                .items_center()
                .gap_2()
                .children(self.render_default_agent_picker(entity, cx))
                .child(add_button)
                .into_any_element(),
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
                t!(
                    "default.current",
                    name = agent_display_name(plan.default_name.as_deref().unwrap_or_default())
                )
                .into_owned(),
                Some(agent),
                false,
            ),
            NewConversationIntent::PickAgent | NewConversationIntent::CreateAgent => (
                t!("default.pick").into_owned(),
                None,
                self.picking_for_conversation,
            ),
        };
        let ready = plan.ready;
        Some(
            MenuTrigger::dropdown(
                "agents-default-picker-menu",
                Button::new("agents-default-picker")
                    .quiet()
                    .label(label)
                    .tooltip(t!("default.tooltip")),
                move |_, cx| {
                    fill_conversation_agent_menu(
                        Menu::new(),
                        ready.clone(),
                        current.clone(),
                        start,
                        entity.clone(),
                        cx,
                    )
                },
            )
            .into_any_element(),
        )
    }

    pub(crate) fn render_agent_editor_header(
        &self,
        name: &str,
        entity: Entity<AgentsPage>,
        cx: &App,
    ) -> AnyElement {
        let draft = self.editing_conversation_draft();
        let back_entity = entity.clone();
        let tabs = self.editor.as_ref().map(|editor| {
            // 设置在前、记忆在后，别的插件填的页签按登记顺序夹在中间。
            let tabs: Rc<Vec<AgentEditorTab>> = Rc::new(
                std::iter::once(AgentEditorTab::Config)
                    .chain(
                        editor
                            .fills
                            .iter()
                            .map(|fill| AgentEditorTab::Fill(fill.id().clone())),
                    )
                    .chain(std::iter::once(AgentEditorTab::Memory))
                    .collect(),
            );
            let entity = entity.clone();
            Segmented::new("agent-editor-tabs")
                .segments(
                    tabs.iter()
                        .map(|tab| agent_editor_tab_label(tab, editor, cx)),
                )
                .selected(tabs.iter().position(|tab| *tab == editor.tab))
                .on_select(move |ix, _, cx| {
                    if let Some(tab) = tabs.get(ix).cloned() {
                        entity.update(cx, |page, cx| page.select_agent_editor_tab(tab, cx));
                    }
                })
        });
        // 草稿是「开始对话」（开了才算建好）；已经建好的随时能开一段新对话。
        let start_entity = entity;
        let start = if draft {
            Button::new("agent-draft-start-conversation")
                .primary()
                .label(t!("editor.start"))
                .on_click(move |_, _, cx| {
                    start_entity.update(cx, |page, cx| page.commit_conversation_draft(cx));
                })
        } else {
            let agent = self.editor.as_ref().map(|editor| editor.agent.clone());
            Button::new("agent-new-conversation")
                .primary()
                .label(t!("new_conversation.title"))
                .tooltip(t!("new_conversation.tooltip"))
                .on_click(move |_, _, cx| {
                    if let Some(agent) = agent.clone() {
                        start_entity
                            .update(cx, |page, cx| page.start_agent_conversation(agent, cx));
                    }
                })
        };
        api::ui::page_toolbar()
            .child(product_back_button(
                "agent-editor-back",
                if draft { t!("editor.cancel") } else { t!("editor.back") },
                move |_, _, cx| {
                    back_entity.update(cx, |page, cx| page.close_agent_settings(cx));
                },
            ))
            .child(agent_monogram(name, 20., false))
            // 名字之外只在草稿时补一句后果，长期规则的说明在正文的指令卡上。
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
                            .type_style(TYPE.ui_medium)
                            .text_color(rgb(ui_theme::text()))
                            .overflow_hidden()
                            .child(truncated_line(agent_display_name(name))),
                    )
                    .when(draft, |row| {
                        row.child(
                            div()
                                .min_w_0()
                                .type_style(TYPE.text_xsmall)
                                .text_color(rgb(ui_theme::muted()))
                                .overflow_hidden()
                                .child(truncated_line(t!("editor.draft_note"))),
                        )
                    }),
            )
            .children(tabs)
            .child(start)
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
        // 哪台机器给不了配置写在它自己的分组标题里；不分组时没有标题，才放到页顶。
        let error = self.error.clone().or_else(|| {
            groups
                .first()
                .filter(|_| !grouped)
                .and_then(|group| agent_hosts::unavailable(&group.runtime, cx))
                .map(str::to_string)
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

    pub(crate) fn render_agent_catalog_row(
        &self,
        runtime: &RuntimeKey,
        agent: &AgentDefinition,
        entity: Entity<AgentsPage>,
        cx: &App,
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
        let automation_count = self.automation_count(&agent_ref, cx);
        let menu_talk_entity = open_entity.clone();
        let menu_talk_agent = agent_ref.clone();
        let menu_delete_entity = open_entity.clone();
        let menu_delete_agent = agent_ref;
        let menu_id = SharedString::from(format!("{row_id}-menu"));
        let row = content_card_style(ListRow::new(row_id))
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
                                    .type_style(TYPE.ui_medium)
                                    .text_color(rgb(ui_theme::text()))
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
                                .type_style(TYPE.text_xsmall)
                                .line_height(px(18.))
                                .text_color(rgb(ui_theme::muted()))
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
            });
        MenuTrigger::context(menu_id, row, move |_, _| {
            let talk_entity = menu_talk_entity.clone();
            let talk_agent = menu_talk_agent.clone();
            let delete_entity = menu_delete_entity.clone();
            let delete_agent = menu_delete_agent.clone();
            Menu::new()
                .item(
                    MenuItem::new(t!("new_conversation.title")).on_click(move |_, _, cx| {
                        let agent = talk_agent.clone();
                        talk_entity.update(cx, |page, cx| page.start_agent_conversation(agent, cx));
                    }),
                )
                .separator()
                .item(
                    MenuItem::new(t!("delete.title"))
                        .icon(api::ui::trash_icon())
                        .danger()
                        .disabled(automation_count != 0)
                        .on_click(move |_, window, cx| {
                            let agent = delete_agent.clone();
                            delete_entity
                                .update(cx, |page, cx| page.start_delete_agent(agent, window, cx));
                        }),
                )
        })
        .w_full()
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
                                    .text_color(rgb(ui_theme::text()))
                                    .child(t!("empty.title").into_owned()),
                            )
                            .child(
                                div()
                                    .type_style(TYPE.ui)
                                    .text_color(rgb(ui_theme::muted()))
                                    .text_center()
                                    .child(SharedString::from(
                                        if self.arm_conversation_on_create {
                                            t!("empty.description_arm")
                                        } else {
                                            t!("empty.description")
                                        },
                                    )),
                            ),
                    )
                    .child({
                        let button = Button::new("agents-empty-add")
                            .small()
                            .icon(IconName::Plus)
                            .label(t!("agent.new"));
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
                            _ => MenuTrigger::dropdown(
                                "agents-empty-add-menu",
                                button,
                                move |_, cx| {
                                    fill_create_agent_menu(Menu::new(), entity.clone(), cx)
                                },
                            )
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
        let automation_count = self.automation_count(&editor.agent, cx);
        let error = self
            .error
            .clone()
            .or_else(|| agent_hosts::unavailable(&editor.agent.runtime, cx).map(str::to_string));
        if let AgentEditorTab::Fill(id) = &editor.tab
            && let Some(fill) = editor.fills.iter().find(|fill| fill.id() == id)
        {
            // 填的视图自己排版、自己滚动。
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .h_full()
                .child(fill.view())
                .into_any_element();
        }
        if editor.tab == AgentEditorTab::Memory {
            // 单独的滚动区：两个页签各记各的滚动位置。
            return div()
                .id("agent-memory-scroll")
                .flex_1()
                .min_w_0()
                .h_full()
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

        let delete_row = render_agent_delete_row(
            editor.agent.clone(),
            self.automation_names(&editor.agent, cx),
            entity.clone(),
        );

        div()
            .id("agent-config-scroll")
            .flex_1()
            .min_w_0()
            .h_full()
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
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .child(render_identity_name(&editor.name)),
                                            ),
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
                                                    .type_style(TYPE.text_xsmall)
                                                    .text_color(rgb(ui_theme::faint()))
                                                    .child(agent_usage_summary(
                                                        agent_conversation_count(&editor.agent, cx),
                                                        automation_count,
                                                    )),
                                            ),
                                    ),
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
                                            .type_style(TYPE.ui_medium)
                                            .text_color(rgb(ui_theme::text()))
                                            .child(t!("instructions.title").into_owned()),
                                    )
                                    .child(
                                        div()
                                            .type_style(TYPE.text_xsmall)
                                            .text_color(rgb(ui_theme::muted()))
                                            .child(t!("instructions.description").into_owned()),
                                    ),
                            )
                            .child(
                                div()
                                    .px_5()
                                    .pb_3()
                                    .child(TextArea::new(&editor.instructions)),
                            )
                            .child(
                                div()
                                    .px_5()
                                    .pt_1()
                                    .pb_3()
                                    .type_style(TYPE.text_xsmall)
                                    .text_color(rgb(ui_theme::faint()))
                                    .child(instructions_footer),
                            ),
                    )
                    .child(render_agent_context_card(&agent, editor, entity.clone()))
                    .child(render_agent_memory_summary(&agent, editor, entity.clone()))
                    .child(render_agent_capability_card(&agent, editor, entity, cx))
                    .child(delete_row),
            )
            .into_any_element()
    }
    pub(crate) fn render_agents_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
                self.render_agent_editor_header(&agent.name, entity.clone(), cx),
                self.render_agent_config_panel(agent, editor, entity, cx),
            ),
            None => {
                let header = self.render_agents_header(count, entity.clone(), cx);
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
            ui_theme::bg()
        } else {
            ui_theme::surface()
        }))
        .text_size(px(if compact { size * 0.34 } else { size * 0.42 }))
        .font_medium()
        .text_color(rgb(ui_theme::text()))
        .child(monogram)
}

/// 能开对话的智能体。挂了不止一台机器时按机器分段，段头是机器名。
pub(crate) fn fill_conversation_agent_menu(
    mut menu: Menu,
    ready: Vec<ConversationAgentPick>,
    current: Option<AgentRef>,
    start: bool,
    entity: Entity<AgentsPage>,
    cx: &App,
) -> Menu {
    let grouped = ready
        .iter()
        .any(|pick| pick.agent.runtime != RuntimeKey::Local);
    let mut section = None;
    for pick in ready {
        if grouped && section.as_ref() != Some(&pick.agent.runtime) {
            if section.is_some() {
                menu = menu.separator();
            }
            menu = menu.label(machine_name(&pick.agent.runtime, cx));
            section = Some(pick.agent.runtime.clone());
        }
        let entity = entity.clone();
        let checked = !start && current.as_ref() == Some(&pick.agent);
        let agent = pick.agent;
        menu = menu.item(
            MenuItem::new(agent_display_name(&pick.name))
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
pub(crate) fn fill_create_agent_menu(mut menu: Menu, entity: Entity<AgentsPage>, cx: &App) -> Menu {
    let machines = agent_creation_machines(cx);
    let grouped = machines.len() > 1;
    for (ix, runtime) in machines.into_iter().enumerate() {
        if grouped {
            if ix > 0 {
                menu = menu.separator();
            }
            menu = menu.label(machine_name(&runtime, cx));
        }
        for kind in agent_engine_kinds() {
            let entity = entity.clone();
            let runtime = runtime.clone();
            menu = menu.item(MenuItem::new(kind.label()).icon(IconName::Bot).on_click(
                move |_, window, cx| {
                    let runtime = runtime.clone();
                    entity.update(cx, |page, cx| page.create_agent(runtime, kind, window, cx));
                },
            ));
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
            let loaded = agent_hosts::settings(&runtime, cx).is_some();
            let status = machine_unavailable(&runtime, cx)
                .or_else(|| agent_hosts::unavailable(&runtime, cx).map(str::to_string))
                .or_else(|| (!loaded).then(|| t!("common.loading").into_owned()));
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
                .type_style(TYPE.text_xsmall_medium)
                .text_color(rgb(ui_theme::muted()))
                .child(machine_name(&group.runtime, cx)),
        )
        .children(group.status.clone().map(|status| {
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(ui_theme::faint()))
                .child(status)
        }))
        .when(group.status.is_none() && group.agents.is_empty(), |row| {
            row.child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(rgb(ui_theme::faint()))
                    .child(t!("empty.title").into_owned()),
            )
        })
        .into_any_element()
}
