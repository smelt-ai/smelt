//! 页面上的卡片、表单件和页头。

use super::*;
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

pub(crate) fn render_identity_name(input: &Entity<InputState>) -> AnyElement {
    TextInput::new(input)
        .small()
        .aria_label(t!("agent.name_label"))
        .w_full()
        .font_semibold()
        .into_any_element()
}

pub(crate) fn agent_definition_column() -> Div {
    div().w_full().max_w(px(720.)).mx_auto().px_8()
}

/// 内容卡：12px 圆角加发丝描边。智能体空状态、自动化表和表单共用。
pub(crate) fn content_card_style<E: Styled>(element: E) -> E {
    element
        .rounded(ui_theme::card_radius())
        .border_1()
        .border_color(ui_theme::hair())
        .bg(rgb(ui_theme::surface()))
}

pub(crate) fn content_card() -> Div {
    content_card_style(div())
}

pub(crate) fn agent_surface_card() -> Div {
    content_card()
}

pub(crate) fn render_agent_error(error: String) -> AnyElement {
    api::ui::Alert::error("agent-error", error).into_any_element()
}

pub(crate) fn render_engine_mark(kind: Option<ConversationAgentKind>, size: f32) -> Icon {
    match kind {
        Some(kind) => Icon::path(kind.icon_asset())
            .size(px(size))
            .text_color(rgb(ui_theme::muted())),
        None => Icon::new(IconName::Bot)
            .size(px(size))
            .text_color(rgb(ui_theme::muted())),
    }
}

pub(crate) fn render_agent_engine_control(
    agent_id: String,
    label: String,
    kind: Option<ConversationAgentKind>,
    entity: Entity<AgentsPage>,
) -> AnyElement {
    let icon = render_engine_mark(kind, 12.);
    if agent_engine_kinds().len() > 1 {
        let trigger = Button::new("agent-engine")
            .quiet()
            .small()
            .accessibility_label(label.clone())
            .child(icon)
            .child(div().min_w_0().text_ellipsis().child(label))
            .tooltip(t!("engine.tooltip"));
        MenuTrigger::dropdown("agent-engine-menu", trigger, move |_, _| {
            let mut menu = Menu::new();
            for kind in agent_engine_kinds() {
                let entity = entity.clone();
                let id = agent_id.clone();
                menu = menu.item(MenuItem::new(kind.label()).on_click(move |_, _, cx| {
                    let id = id.clone();
                    entity.update(cx, |page, cx| page.set_agent_engine(id, kind, cx));
                }));
            }
            menu
        })
        .into_any_element()
    } else {
        render_engine_chip(label, kind)
    }
}

pub(crate) fn render_agent_model_control(
    agent: AgentRef,
    model_provider: String,
    model_id: String,
    entity: Entity<AgentsPage>,
) -> AnyElement {
    let inherit = model_provider.trim().is_empty() || model_id.trim().is_empty();
    let label = if inherit {
        t!("model.inherit").into_owned()
    } else {
        format!("{model_provider} / {model_id}")
    };
    let trigger = Button::new("agent-model")
        .quiet()
        .small()
        .label(label)
        .tooltip(t!("model.tooltip"));
    MenuTrigger::dropdown("agent-model-menu", trigger, move |_, cx| {
        let inherit_entity = entity.clone();
        let inherit_id = agent.id.clone();
        let mut menu = Menu::new().item(
            MenuItem::new(t!("model.inherit"))
                .checked(inherit)
                .on_click(move |_, _, cx| {
                    let id = inherit_id.clone();
                    inherit_entity.update(cx, |page, cx| {
                        page.set_agent_model(id, String::new(), String::new(), cx);
                    });
                }),
        );
        // 模型在智能体那台机器上跑，可选的也是那台机器的：取编辑器读到的那份。
        let groups = entity
            .read(cx)
            .editor
            .as_ref()
            .filter(|editor| editor.agent == agent)
            .map(|editor| editor.pi_model_choice_groups().to_vec())
            .unwrap_or_default();
        if !groups.is_empty() {
            menu = menu.separator();
        }
        for group in groups {
            // 这一层没有再嵌套。模型一多就超出窗口，在二级里滚。
            let mut sub = Menu::new();
            for choice in &group.models {
                let checked = choice.provider == model_provider && choice.id == model_id;
                let item_entity = entity.clone();
                let item_id = agent.id.clone();
                let provider = choice.provider.clone();
                let model = choice.id.clone();
                sub = sub.item(
                    MenuItem::new(choice.short_label().to_string())
                        .checked(checked)
                        .on_click(move |_, _, cx| {
                            let id = item_id.clone();
                            let provider = provider.clone();
                            let model = model.clone();
                            item_entity.update(cx, |page, cx| {
                                page.set_agent_model(id, provider, model, cx);
                            });
                        }),
                );
            }
            menu = menu.item(MenuItem::new(group.provider_name.clone()).submenu(sub));
        }
        menu
    })
    .into_any_element()
}

pub(crate) fn render_engine_chip(label: String, kind: Option<ConversationAgentKind>) -> AnyElement {
    Badge::new()
        .child(render_engine_mark(kind, 12.))
        .child(label)
        .into_any_element()
}

/// 页头前缘的返回。可见的只有符号，悬停提示是控件名。它只退一层，侧栏标题负责回根页。
pub(crate) fn product_back_button(
    id: &'static str,
    tooltip: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    Button::new(id)
        .quiet()
        .icon(IconName::ChevronLeft)
        .icon_action(tooltip)
        .on_click(on_click)
}

/// 目录的工具行：和自动化的同一条高度。左边是有几个（页名在外壳画的标题带或标签上，
/// 这里不再写一遍），右边是新建按钮。
pub(crate) fn product_page_header(count_label: String, action: AnyElement) -> AnyElement {
    api::ui::page_toolbar()
        .justify_between()
        .gap_3()
        .child(
            div()
                .type_style(TYPE.ui)
                .text_color(rgb(ui_theme::muted()))
                .child(count_label),
        )
        .child(action)
        .into_any_element()
}

/// 智能体的插件勾选区。只列全局插件（`~/.pi/agent/skills`、`~/.agents/skills`、
/// `~/.pi/agent/extensions`），因为智能体对话的工作目录是一次性空目录，项目级插件永远扫不到。
pub(crate) fn render_agent_plugin_card(
    agent: &AgentDefinition,
    editor: &AgentEditor,
    entity: Entity<AgentsPage>,
    cx: &App,
) -> AnyElement {
    let selected = agent.plugins.clone();
    let selected_count = selected.len();

    let refresh_entity = entity.clone();
    let refresh_button = compact_chip("agent-plugins-refresh")
        .icon(IconName::Refresh)
        .icon_action(t!("plugins.rescan"))
        .on_click(move |_, _, cx| {
            refresh_entity.update(cx, |page, cx| page.refresh_agent_plugins(cx));
        });
    let manage_entity = entity.clone();
    let manage_button = compact_chip("agent-plugins-manage")
        .label(t!("common.manage"))
        .tooltip(t!("plugins.manage_tooltip"))
        .on_click(move |_, _, cx| {
            manage_entity.update(cx, |page, cx| {
                page.open_capabilities("skills", cx);
            });
        });

    // 已勾选但磁盘上已经没有的条目也要露出来，否则用户没法取消它。
    let mut rows: Vec<AnyElement> = editor
        .pi_plugins()
        .iter()
        .map(|plugin| {
            agent_plugin_row(
                AgentPick::Plugin,
                &agent.id,
                &plugin.id,
                &plugin.name,
                &plugin_kind_label(plugin.kind),
                &plugin.description,
                &plugin.origin,
                plugin.broken.clone(),
                selected.contains(&plugin.id),
                entity.clone(),
            )
        })
        .collect();
    // 目录还没读到时不知道哪些已经没了，先不标。
    let pending = editor.pi_pending_text();
    for id in selected.iter().filter(|_| pending.is_none()) {
        if editor.pi_plugins().iter().any(|plugin| plugin.id == *id) {
            continue;
        }
        let (kind_label, name) = match api::model::pi_plugin_catalog::split_plugin_id(id) {
            Some((kind, name)) => (plugin_kind_label(kind), name.to_string()),
            None => (t!("pick.unknown"), id.clone()),
        };
        rows.push(agent_plugin_row(
            AgentPick::Plugin,
            &agent.id,
            id,
            &name,
            &kind_label,
            "",
            "",
            Some(t!("plugins.missing").into_owned()),
            true,
            entity.clone(),
        ));
    }

    let body = if rows.is_empty() {
        div()
            .px_5()
            .py_4()
            .type_style(TYPE.text_xsmall)
            .text_color(rgb(ui_theme::faint()))
            .child(pending.unwrap_or_else(|| t!("plugins.empty").into_owned()))
            .into_any_element()
    } else {
        div()
            .px_2()
            .pb_2()
            .flex()
            .flex_col()
            .children(rows)
            .into_any_element()
    };

    agent_surface_card()
        .flex()
        .flex_col()
        .child(
            div()
                .px_5()
                .pt_4()
                .pb_2()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .type_style(TYPE.ui_medium)
                                .text_color(rgb(ui_theme::text()))
                                .child(t!("plugins.title").into_owned()),
                        )
                        .child(
                            div()
                                .type_style(TYPE.text_xsmall)
                                .text_color(rgb(ui_theme::muted()))
                                .child(t!("plugins.selected", count = selected_count).into_owned()),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(refresh_button)
                        .when(api::is_registered(CAPABILITIES_VIEW, cx), |row| {
                            row.child(manage_button)
                        }),
                ),
        )
        .child(body)
        .into_any_element()
}

/// 「绑定的上下文」卡片。Pi 只接受单个 cwd，会话的工作目录是智能体自己的
/// space，所以业务素材只能靠这里挂进来——绑定的目录与链接会以绝对路径写进
/// system prompt，让 Pi 用 read/bash 自己去取。
pub(crate) fn render_agent_context_card(
    agent: &AgentDefinition,
    editor: &AgentEditor,
    entity: Entity<AgentsPage>,
) -> AnyElement {
    // 工作目录按那台机器的主目录算，本机算不出远程机器上的路径。
    let space_root = match editor.agent.runtime {
        RuntimeKey::Local => api::model::agent_paths::agent_space_root(&agent.id)
            .map(|path| t!("context.workspace_path", path = path.to_string_lossy()).into_owned())
            .unwrap_or_else(|| t!("context.workspace").into_owned()),
        RuntimeKey::Remote(_) => t!("context.workspace_remote").into_owned(),
    };

    let pick_agent = agent.id.clone();
    let pick_entity = entity.clone();
    let add_folder = compact_chip("agent-context-add-folder")
        .icon(IconName::Plus)
        .label(t!("context.folder"))
        .tooltip(t!("context.folder_tooltip"))
        .on_click(move |_, _, cx| {
            let agent_id = pick_agent.clone();
            pick_entity.update(cx, |page, cx| page.pick_agent_context_folders(agent_id, cx));
        });

    let link_agent = agent.id.clone();
    let link_entity = entity.clone();
    let add_link = compact_chip("agent-context-add-link")
        .icon(IconName::Plus)
        .label(t!("context.link"))
        .on_click(move |_, window, cx| {
            let agent_id = link_agent.clone();
            link_entity.update(cx, |page, cx| {
                page.add_agent_context_link(agent_id, window, cx)
            });
        });

    let mut rows: Vec<AnyElement> = Vec::new();
    for folder in &agent.context_folders {
        rows.push(agent_context_row(
            &agent.id,
            folder,
            &t!("context.folder"),
            true,
            entity.clone(),
        ));
    }
    for link in &agent.context_links {
        rows.push(agent_context_row(
            &agent.id,
            link,
            &t!("context.link"),
            false,
            entity.clone(),
        ));
    }

    let body = if rows.is_empty() {
        div()
            .px_5()
            .py_4()
            .type_style(TYPE.text_xsmall)
            .text_color(rgb(ui_theme::faint()))
            .child(t!("context.empty").into_owned())
            .into_any_element()
    } else {
        div()
            .px_2()
            .pb_1()
            .flex()
            .flex_col()
            .children(rows)
            .into_any_element()
    };

    agent_surface_card()
        .flex()
        .flex_col()
        .child(
            div()
                .px_5()
                .pt_4()
                .pb_2()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .type_style(TYPE.ui_medium)
                                .text_color(rgb(ui_theme::text()))
                                .child(t!("context.title").into_owned()),
                        )
                        .child(
                            div()
                                .type_style(TYPE.text_xsmall)
                                .text_color(rgb(ui_theme::muted()))
                                .child(t!("context.description").into_owned()),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(add_folder)
                        .child(add_link),
                ),
        )
        .child(body)
        .child(
            div().px_3().pb_2().child(
                TextInput::new(&editor.context_link)
                    .small()
                    .aria_label(t!("context.link_label"))
                    .w_full(),
            ),
        )
        .child(
            div()
                .px_5()
                .pb_3()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(ui_theme::faint()))
                .child(
                    t!(
                        "context.workspace_note",
                        workspace = space_root,
                        dirs = api::model::pi_plugin_catalog::WORKSPACE_SKILL_DIRS.join(" / ")
                    )
                    .into_owned(),
                ),
        )
        .into_any_element()
}

pub(crate) fn agent_memory_switch(
    id: &'static str,
    agent: &AgentDefinition,
    entity: Entity<AgentsPage>,
) -> Switch {
    let memory_enabled = agent.memory_enabled;
    let agent_id = agent.id.clone();
    Switch::new(id)
        .checked(memory_enabled)
        .accessibility_label(t!("memory.title").into_owned())
        .tooltip(if memory_enabled {
            t!("memory.switch_off_tooltip")
        } else {
            t!("memory.switch_on_tooltip")
        })
        .on_change(move |enabled, _, cx| {
            let agent_id = agent_id.clone();
            entity.update(cx, |page, cx| {
                page.set_agent_memory_enabled(agent_id, enabled, cx)
            });
        })
}

pub(crate) fn memory_pending_count(memories: &AgentMemories) -> usize {
    memories
        .agent
        .entries
        .iter()
        .chain(&memories.user.entries)
        .filter(|entry| entry.pending_reason.is_some())
        .count()
}

/// 编辑页页签名。待确认要用户处理，挂在页签上，在配置页也看得到。
pub(crate) fn agent_editor_tab_label(tab: AgentEditorTab, editor: &AgentEditor) -> String {
    match tab {
        AgentEditorTab::Config => t!("tab.config").into_owned(),
        AgentEditorTab::Memory => match memory_pending_count(&editor.memories) {
            0 => t!("memory.title").into_owned(),
            pending => t!("tab.memory_pending", count = pending).into_owned(),
        },
    }
}

/// 配置页上的记忆只占一行：开关、条数、待确认、进记忆页签的入口。条目一律不在这里列，
/// 记忆再多也不会把插件和 MCP 挤到后面。
pub(crate) fn render_agent_memory_summary(
    agent: &AgentDefinition,
    editor: &AgentEditor,
    entity: Entity<AgentsPage>,
) -> AnyElement {
    let pending = memory_pending_count(&editor.memories);
    let manage_entity = entity.clone();
    let manage = compact_chip("agent-memory-manage")
        .label(t!("common.manage"))
        .icon(IconName::ChevronRight)
        .tooltip(t!("memory.manage_tooltip"))
        .on_click(move |_, _, cx| {
            manage_entity.update(cx, |page, cx| {
                page.select_agent_editor_tab(AgentEditorTab::Memory, cx)
            });
        });
    agent_surface_card()
        .px_5()
        .py_3()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_shrink_0()
                .type_style(TYPE.ui_medium)
                .text_color(rgb(ui_theme::text()))
                .child(t!("memory.title").into_owned()),
        )
        .child(agent_memory_switch(
            "agent-memory-enabled-summary",
            agent,
            entity,
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .type_style(TYPE.text_xsmall)
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_color(rgb(ui_theme::muted()))
                        .child(truncated_line(if agent.memory_enabled {
                            memory_summary_text(
                                editor.memories.agent.entries.len(),
                                editor.memories.user.entries.len(),
                            )
                        } else {
                            t!("memory.off_summary").into_owned()
                        })),
                )
                .when(pending > 0, |row| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .text_color(rgb(ui_theme::bad()))
                            .child(t!("memory.pending_count", count = pending).into_owned()),
                    )
                }),
        )
        .child(manage)
        .into_any_element()
}

/// 记忆页签：智能体在对话和自动化里自己记下的长期信息。正文是 Markdown 文件，
/// 这里搜索、筛选、展开看正文、打开和删除；写入规则见 `docs/agent-memory.md`。
pub(crate) fn render_agent_memory_card(
    agent: &AgentDefinition,
    editor: &AgentEditor,
    entity: Entity<AgentsPage>,
    cx: &App,
) -> AnyElement {
    let memory_enabled = agent.memory_enabled;
    let refresh_entity = entity.clone();
    let refresh_button = compact_chip("agent-memory-refresh")
        .icon(IconName::Refresh)
        .icon_action(t!("memory.reload"))
        .on_click(move |_, _, cx| {
            refresh_entity.update(cx, |page, cx| page.refresh_agent_memories(cx));
        });
    let query = editor.memory_query.read(cx).value().to_string();
    let filter = editor.memory_filter;
    let narrowed = !query.trim().is_empty() || filter != MemoryFilter::All;
    let filter_entity = entity.clone();
    let filters = Segmented::new("agent-memory-filters")
        .segments(
            MemoryFilter::ALL.map(|option| Segment::new(option.label()).tooltip(option.tooltip())),
        )
        .selected(
            MemoryFilter::ALL
                .iter()
                .position(|option| *option == filter),
        )
        .on_select(move |ix, _, cx| {
            if let Some(option) = MemoryFilter::ALL.get(ix).copied() {
                filter_entity.update(cx, |page, cx| page.set_agent_memory_filter(option, cx));
            }
        });
    agent_surface_card()
        .flex()
        .flex_col()
        .child(
            div()
                .px_5()
                .pt_4()
                .pb_2()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .type_style(TYPE.ui_medium)
                                .text_color(rgb(ui_theme::text()))
                                .child(t!("memory.title").into_owned()),
                        )
                        .child(
                            div()
                                .type_style(TYPE.text_xsmall)
                                .text_color(rgb(ui_theme::muted()))
                                .child(SharedString::from(if memory_enabled {
                                    t!("memory.description")
                                } else {
                                    t!("memory.description_off")
                                })),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(refresh_button)
                        .child(agent_memory_switch(
                            "agent-memory-enabled",
                            agent,
                            entity.clone(),
                        )),
                ),
        )
        .child(
            div()
                .px_5()
                .pb_2()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(200.))
                        .child(TextInput::new(&editor.memory_query).small().cleanable(true)),
                )
                .child(filters),
        )
        .child(agent_memory_section(
            &agent.id,
            AgentMemorySection {
                layer: MemoryLayer::Agent,
                title: t!("memory.agent_title"),
                empty_hint: t!("memory.agent_empty"),
                listing: &editor.memories.agent,
                query: &query,
                filter,
                narrowed,
            },
            editor.expanded_memory.as_ref(),
            entity.clone(),
        ))
        .child(agent_memory_section(
            &agent.id,
            AgentMemorySection {
                layer: MemoryLayer::User,
                title: t!("memory.user_title"),
                empty_hint: t!("memory.user_empty"),
                listing: &editor.memories.user,
                query: &query,
                filter,
                narrowed,
            },
            editor.expanded_memory.as_ref(),
            entity,
        ))
        .into_any_element()
}

pub(crate) struct AgentMemorySection<'a> {
    layer: MemoryLayer,
    title: std::borrow::Cow<'static, str>,
    empty_hint: std::borrow::Cow<'static, str>,
    listing: &'a api::model::agent_memory::MemoryListing,
    query: &'a str,
    filter: MemoryFilter,
    /// 有搜索词或筛选时，空列表要说「没有匹配」，不能说「还没有记忆」。
    narrowed: bool,
}

pub(crate) fn agent_memory_section(
    agent_id: &str,
    section: AgentMemorySection<'_>,
    expanded: Option<&(MemoryLayer, String)>,
    entity: Entity<AgentsPage>,
) -> AnyElement {
    let AgentMemorySection {
        layer,
        title,
        empty_hint,
        listing,
        query,
        filter,
        narrowed,
    } = section;
    let layer_key = match layer {
        MemoryLayer::Agent => "agent",
        MemoryLayer::User => "user",
    };
    let folder_agent = agent_id.to_string();
    let folder_entity = entity.clone();
    let open_folder = compact_chip(format!("agent-memory-folder-{layer_key}"))
        .icon(IconName::FolderOpen)
        .icon_action(t!("memory.open_folder"))
        .on_click(move |_, _, cx| {
            let agent_id = folder_agent.clone();
            folder_entity.update(cx, |page, cx| {
                page.open_agent_memory_folder(agent_id, layer, cx)
            });
        });

    let rows: Vec<AnyElement> = visible_memories(&listing.entries, query, filter)
        .into_iter()
        .map(|entry| {
            let open = expanded.is_some_and(|(expanded_layer, file_name)| {
                *expanded_layer == layer && *file_name == entry.file_name
            });
            agent_memory_row(agent_id, layer, layer_key, entry, open, entity.clone())
        })
        .collect();
    let shown = rows.len();
    let body = if rows.is_empty() {
        div()
            .px_3()
            .py_1p5()
            .type_style(TYPE.text_xsmall)
            .text_color(rgb(ui_theme::faint()))
            .child(SharedString::from(
                if narrowed && !listing.entries.is_empty() {
                    t!("memory.no_match")
                } else {
                    empty_hint
                },
            ))
            .into_any_element()
    } else {
        div().flex().flex_col().children(rows).into_any_element()
    };

    div()
        .px_2()
        .pb_3()
        .flex()
        .flex_col()
        .child(
            div()
                .px_3()
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .type_style(TYPE.text_xsmall_medium)
                        .text_color(rgb(ui_theme::muted()))
                        .child(memory_section_title(
                            &title,
                            listing,
                            narrowed.then_some(shown),
                        )),
                )
                .child(open_folder),
        )
        .when(listing.core_over_budget, |section| {
            section.child(
                div()
                    .px_3()
                    .py_1()
                    .type_style(TYPE.text_xsmall)
                    .text_color(rgb(ui_theme::bad()))
                    .child(
                        t!(
                            "memory.over_budget",
                            kb = api::model::agent_memory::CORE_MEMORY_MAX_BYTES / 1000
                        )
                        .into_owned(),
                    ),
            )
        })
        .child(body)
        .into_any_element()
}

pub(crate) fn memory_section_title(
    title: &str,
    listing: &api::model::agent_memory::MemoryListing,
    shown: Option<usize>,
) -> String {
    let total = listing.entries.len();
    let count = match shown {
        Some(shown) => t!("memory.count_shown", shown = shown, total = total),
        None => t!("memory.count", total = total),
    };
    let pending = listing
        .entries
        .iter()
        .filter(|entry| entry.pending_reason.is_some())
        .count();
    if pending == 0 {
        format!("{title} · {count}")
    } else {
        t!(
            "memory.section_pending",
            title = title,
            count = count,
            pending = pending
        )
        .into_owned()
    }
}

pub(crate) fn memory_kind_label(kind: Option<&str>) -> SharedString {
    match kind {
        Some("user") => t!("memory.kind.user"),
        Some("feedback") => t!("memory.kind.feedback"),
        Some("project") => t!("memory.kind.project"),
        Some("reference") => t!("memory.kind.reference"),
        _ => t!("memory.kind.invalid"),
    }
    .into()
}

/// 技能还是扩展，给人看的名字。
pub(crate) fn plugin_kind_label(
    kind: api::model::pi_plugin_catalog::PiPluginKind,
) -> std::borrow::Cow<'static, str> {
    match kind {
        api::model::pi_plugin_catalog::PiPluginKind::Skill => t!("plugins.kind.skill"),
        api::model::pi_plugin_catalog::PiPluginKind::Extension => t!("plugins.kind.extension"),
    }
}

/// 行尾的状态：待确认和长期没用的标红，提示用户处理；用过的显示次数。
pub(crate) fn memory_usage_label(
    entry: &api::model::agent_memory::MemoryEntry,
) -> Option<(String, bool)> {
    if entry.pending_reason.is_some() {
        return Some((t!("memory.filter.pending").into_owned(), true));
    }
    if entry.stale {
        return Some((t!("memory.stale").into_owned(), true));
    }
    (entry.read_count > 0).then(|| {
        let used = match entry.read_count {
            1 => t!("memory.used.one").into_owned(),
            count => t!("memory.used.other", count = count).into_owned(),
        };
        (used, false)
    })
}

pub(crate) fn agent_memory_row(
    agent_id: &str,
    layer: MemoryLayer,
    layer_key: &str,
    entry: &api::model::agent_memory::MemoryEntry,
    expanded: bool,
    entity: Entity<AgentsPage>,
) -> AnyElement {
    let open_path = entry.path.clone();
    let toggle_file = entry.file_name.clone();
    let toggle_entity = entity.clone();
    let open_entity = entity.clone();
    let remove_agent = agent_id.to_string();
    let remove_file = entry.file_name.clone();
    let remove_name = entry.name.clone();
    let subtitle = match entry.pending_reason_text() {
        // 待确认时把原因放在描述前面，用户采纳前能看到它为什么没生效。
        Some(reason) => t!(
            "memory.pending_subtitle",
            reason = reason,
            description = entry.description
        )
        .into_owned(),
        None if entry.description.is_empty() => entry.file_name.clone(),
        None => entry.description.clone(),
    };
    let approve_agent = agent_id.to_string();
    let approve_file = entry.file_name.clone();
    let approve_entity = entity.clone();
    let body_id = format!("agent-memory-body-{layer_key}-{}", entry.file_name);
    let header = div()
        .px_3()
        .py_1p5()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_shrink_0()
                .w(px(44.))
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(ui_theme::faint()))
                .child(memory_kind_label(entry.kind.as_deref())),
        )
        .child(
            div()
                .id(SharedString::from(format!(
                    "agent-memory-expand-{layer_key}-{}",
                    entry.file_name
                )))
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .on_click(move |_, _, cx| {
                    let file_name = toggle_file.clone();
                    toggle_entity.update(cx, |page, cx| {
                        page.toggle_agent_memory_expanded(layer, file_name, cx)
                    });
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            Icon::new(if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .size(px(12.))
                            .flex_shrink_0()
                            .text_color(rgb(ui_theme::faint())),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .type_style(TYPE.text_xsmall)
                                .text_color(rgb(ui_theme::text()))
                                .overflow_hidden()
                                .child(truncated_line(entry.name.clone())),
                        ),
                )
                .child(
                    // 待确认时前面挂着原因，一行放不下就把描述挤没了；给两行再省略。
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(rgb(ui_theme::faint()))
                        .text_ellipsis()
                        .line_clamp(2)
                        .child(subtitle),
                ),
        )
        .when_some(memory_usage_label(entry), |row, (label, stale)| {
            row.child(
                div()
                    .flex_shrink_0()
                    .type_style(TYPE.text_xsmall)
                    .text_color(rgb(if stale {
                        ui_theme::bad()
                    } else {
                        ui_theme::faint()
                    }))
                    .child(label),
            )
        })
        .child(
            compact_chip(format!("agent-memory-open-{layer_key}-{}", entry.file_name))
                .icon(IconName::ExternalLink)
                .icon_action(t!("memory.open_in_editor"))
                .on_click(move |_, _, cx| {
                    let path = open_path.clone();
                    open_entity.update(cx, |page, cx| page.open_agent_memory_file(path, cx));
                }),
        )
        .when(entry.pending_reason.is_some(), |row| {
            row.child(
                compact_chip(format!(
                    "agent-memory-approve-{layer_key}-{}",
                    entry.file_name
                ))
                .icon(IconName::Check)
                .accessibility_label(t!("memory.accept"))
                .tooltip(t!("memory.accept_tooltip"))
                .on_click(move |_, _, cx| {
                    let agent_id = approve_agent.clone();
                    let file_name = approve_file.clone();
                    approve_entity.update(cx, |page, cx| {
                        page.approve_agent_memory(agent_id, layer, file_name, cx)
                    });
                }),
            )
        })
        .child(
            compact_chip(format!(
                "agent-memory-remove-{layer_key}-{}",
                entry.file_name
            ))
            .icon(IconName::Close)
            .icon_action(t!("memory.delete_tooltip"))
            .on_click(move |_, window, cx| {
                let target = MemoryDeleteTarget {
                    agent_id: remove_agent.clone(),
                    layer,
                    file_name: remove_file.clone(),
                    name: remove_name.clone(),
                };
                entity.update(cx, |page, cx| {
                    page.request_delete_agent_memory(target, window, cx)
                });
            }),
        );
    div()
        .flex()
        .flex_col()
        .child(header)
        .when(expanded, |row| {
            row.child(agent_memory_body(body_id, &entry.body))
        })
        .into_any_element()
}

/// 展开的记忆正文：按原样分行显示，不渲染 Markdown，用户看到的就是模型读到的。
pub(crate) fn agent_memory_body(id: String, body: &str) -> AnyElement {
    let lines: Vec<AnyElement> = if body.is_empty() {
        vec![
            div()
                .text_color(rgb(ui_theme::faint()))
                .child(t!("memory.empty_body").into_owned())
                .into_any_element(),
        ]
    } else {
        body.lines()
            .map(|line| {
                // 空行也要占一行高度，不然段落会挤在一起。
                div()
                    .min_h(px(16.))
                    .child(line.to_string())
                    .into_any_element()
            })
            .collect()
    };
    div()
        .id(SharedString::from(id))
        .mx_3()
        .mb_2()
        .px_3()
        .py_2()
        .max_h(px(240.))
        .overflow_y_scroll()
        .rounded(px(6.))
        .bg(rgb(ui_theme::side()))
        .type_style(TYPE.text_xsmall)
        .text_color(rgb(ui_theme::muted()))
        .children(lines)
        .into_any_element()
}

pub(crate) fn agent_context_row(
    agent_id: &str,
    value: &str,
    kind_label: &str,
    is_folder: bool,
    entity: Entity<AgentsPage>,
) -> AnyElement {
    let remove_agent = agent_id.to_string();
    let remove_value = value.to_string();
    div()
        .px_3()
        .py_1p5()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_shrink_0()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(ui_theme::faint()))
                .child(kind_label.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(ui_theme::text()))
                .overflow_hidden()
                .child(truncated_line(value.to_string())),
        )
        .child(
            compact_chip(format!("agent-context-remove-{kind_label}-{value}"))
                .icon(IconName::Trash)
                .icon_action(if is_folder {
                    t!("context.unbind_folder")
                } else {
                    t!("context.remove_link")
                })
                .on_click(move |_, _, cx| {
                    let agent_id = remove_agent.clone();
                    let value = remove_value.clone();
                    entity.update(cx, |page, cx| {
                        if is_folder {
                            page.remove_agent_context_folder(agent_id, value, cx);
                        } else {
                            page.remove_agent_context_link(agent_id, value, cx);
                        }
                    });
                }),
        )
        .into_any_element()
}

/// 智能体的 MCP 勾选。服务器在能力库里配置，这里只决定这个智能体连哪几台。
pub(crate) fn render_agent_mcp_card(
    agent: &AgentDefinition,
    editor: &AgentEditor,
    entity: Entity<AgentsPage>,
    cx: &App,
) -> AnyElement {
    let selected = agent.mcp_servers.clone();
    let selected_count = selected.len();

    let refresh_entity = entity.clone();
    let refresh_button = compact_chip("agent-mcp-refresh")
        .icon(IconName::Refresh)
        .icon_action(t!("mcp.reload"))
        .on_click(move |_, _, cx| {
            refresh_entity.update(cx, |page, cx| page.refresh_agent_plugins(cx));
        });
    let manage_entity = entity.clone();
    let manage_button = compact_chip("agent-mcp-manage")
        .label(t!("common.manage"))
        .tooltip(t!("mcp.manage_tooltip"))
        .on_click(move |_, _, cx| {
            manage_entity.update(cx, |page, cx| {
                page.open_capabilities("mcp", cx);
            });
        });

    let mut rows: Vec<AnyElement> = editor
        .pi_mcp_servers()
        .iter()
        .map(|server| {
            let (kind_label, detail) = match &server.transport {
                api::model::pi_mcp::PiMcpTransport::Http { url } => (t!("mcp.remote"), url.clone()),
                api::model::pi_mcp::PiMcpTransport::Stdio { command, args } => {
                    (t!("mcp.local"), format!("{command} {}", args.join(" ")))
                }
                api::model::pi_mcp::PiMcpTransport::Unknown => (t!("pick.unknown"), String::new()),
            };
            // 能力库里停用的服务器勾了也不会连：Pi 读到 `enabled: false` 就跳过。
            let broken = (!server.enabled).then(|| t!("mcp.disabled").into_owned());
            agent_plugin_row(
                AgentPick::McpServer,
                &agent.id,
                &server.name,
                &server.name,
                &kind_label,
                server.description.as_deref().unwrap_or(""),
                &detail,
                broken,
                selected.contains(&server.name),
                entity.clone(),
            )
        })
        .collect();
    // 已勾选但 mcp.json 里已经删掉的也要露出来，否则用户没法取消它。
    let pending = editor.pi_pending_text();
    for name in selected.iter().filter(|_| pending.is_none()) {
        if editor
            .pi_mcp_servers()
            .iter()
            .any(|server| server.name == *name)
        {
            continue;
        }
        rows.push(agent_plugin_row(
            AgentPick::McpServer,
            &agent.id,
            name,
            name,
            &t!("pick.unknown"),
            "",
            "",
            Some(t!("mcp.missing").into_owned()),
            true,
            entity.clone(),
        ));
    }

    let body = if rows.is_empty() {
        div()
            .px_5()
            .py_4()
            .type_style(TYPE.text_xsmall)
            .text_color(rgb(ui_theme::faint()))
            .child(pending.unwrap_or_else(|| t!("mcp.empty").into_owned()))
            .into_any_element()
    } else {
        div()
            .px_2()
            .pb_2()
            .flex()
            .flex_col()
            .children(rows)
            .into_any_element()
    };

    agent_surface_card()
        .flex()
        .flex_col()
        .child(
            div()
                .px_5()
                .pt_4()
                .pb_2()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .type_style(TYPE.ui_medium)
                                .text_color(rgb(ui_theme::text()))
                                .child("MCP"),
                        )
                        .child(
                            div()
                                .type_style(TYPE.text_xsmall)
                                .text_color(rgb(ui_theme::muted()))
                                .child(t!("mcp.selected", count = selected_count).into_owned()),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(refresh_button)
                        .when(api::is_registered(CAPABILITIES_VIEW, cx), |row| {
                            row.child(manage_button)
                        }),
                ),
        )
        .child(body)
        .into_any_element()
}

/// 编辑器里可勾选的两类能力：插件（技能 / 扩展）和 MCP 服务器。行的样子一样，
/// 只是写回定义的字段不同。
#[derive(Clone, Copy)]
pub(crate) enum AgentPick {
    Plugin,
    McpServer,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn agent_plugin_row(
    pick: AgentPick,
    agent_id: &str,
    plugin_id: &str,
    name: &str,
    kind_label: &str,
    description: &str,
    origin: &str,
    broken: Option<String>,
    checked: bool,
    entity: Entity<AgentsPage>,
) -> AnyElement {
    let toggle_agent = agent_id.to_string();
    let toggle_plugin = plugin_id.to_string();
    let usable = broken.is_none();
    let subtitle = match &broken {
        Some(reason) => reason.clone(),
        None if !description.is_empty() => description.to_string(),
        None => origin.to_string(),
    };
    let subtitle_color = if usable {
        rgb(ui_theme::faint())
    } else {
        rgb(ui_theme::bad())
    };

    div()
        .px_3()
        .py_2()
        .flex()
        .items_center()
        .gap_3()
        .child(
            Switch::new(match pick {
                AgentPick::Plugin => format!("agent-plugin-{plugin_id}"),
                AgentPick::McpServer => format!("agent-mcp-{plugin_id}"),
            })
            .checked(checked)
            .disabled(!usable && !checked)
            .accessibility_label(name.to_string())
            .on_change(move |_, _, cx| {
                let agent_id = toggle_agent.clone();
                let plugin_id = toggle_plugin.clone();
                entity.update(cx, |page, cx| match pick {
                    AgentPick::Plugin => page.toggle_agent_plugin(agent_id, plugin_id, cx),
                    AgentPick::McpServer => page.toggle_agent_mcp_server(agent_id, plugin_id, cx),
                });
            }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .type_style(TYPE.text_xsmall)
                                .text_color(rgb(ui_theme::text()))
                                .child(name.to_string()),
                        )
                        .child(
                            div()
                                .px_1p5()
                                .rounded(px(4.))
                                .bg(rgb(ui_theme::bg()))
                                .type_style(TYPE.text_xsmall)
                                .text_color(rgb(ui_theme::faint()))
                                .child(kind_label.to_string()),
                        ),
                )
                .child(
                    div()
                        .type_style(TYPE.text_xsmall)
                        .overflow_hidden()
                        .text_color(subtitle_color)
                        .child(truncated_line(subtitle)),
                ),
        )
        .into_any_element()
}

pub(crate) fn compact_chip(id: impl Into<ElementId>) -> Button {
    Button::new(id).quiet().small()
}
