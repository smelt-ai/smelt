//! 页面上的卡片、表单件和页头。

use super::*;

const IDENTITY_NAME_HEIGHT: f32 = 28.;

pub(crate) fn render_identity_name(input: &Entity<InputState>) -> AnyElement {
    editor_field_frame()
        .w_full()
        .child(
            Input::new(input)
                .appearance(false)
                .focus_bordered(false)
                .focus_ring(false)
                .w_full()
                .h(px(IDENTITY_NAME_HEIGHT))
                .px_2()
                .text_lg()
                .font_semibold(),
        )
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
        .border_color(ui_theme::card_stroke())
        .bg(rgb(ui_theme::bg_card()))
}

pub(crate) fn content_card() -> Div {
    content_card_style(div())
}

pub(crate) fn agent_surface_card() -> Div {
    content_card()
}

pub(crate) fn render_agent_error(error: String) -> AnyElement {
    gpui_component::alert::Alert::error("agent-error", error).into_any_element()
}

pub(crate) fn render_engine_mark(kind: Option<ConversationAgentKind>, size: f32) -> Icon {
    match kind {
        Some(kind) => Icon::empty()
            .path(kind.icon_asset())
            .size(px(size))
            .text_color(rgb(ui_theme::text_mid())),
        None => Icon::new(IconName::Bot)
            .size(px(size))
            .text_color(rgb(ui_theme::text_mid())),
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
        Button::new("agent-engine")
            .quiet()
            .compact()
            .icon(icon)
            .label(label)
            .tooltip("执行这个智能体的运行时。后续 skill 和插件会挂在这个引擎上。")
            .dropdown_menu(move |mut menu, _, _| {
                for kind in agent_engine_kinds() {
                    let entity = entity.clone();
                    let id = agent_id.clone();
                    menu = menu.item(PopupMenuItem::new(kind.label()).on_click(move |_, _, cx| {
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
        "跟随 Pi 默认".to_string()
    } else {
        format!("{model_provider} / {model_id}")
    };
    Button::new("agent-model")
        .quiet()
        .compact()
        .label(label)
        .tooltip("这个智能体的新对话和自动化用这套模型。对话里仍可再改。")
        .dropdown_menu(move |mut menu, window, cx| {
            let inherit_entity = entity.clone();
            let inherit_id = agent.id.clone();
            menu = menu.item(
                PopupMenuItem::new("跟随 Pi 默认")
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
                let item_entity = entity.clone();
                let item_id = agent.id.clone();
                let current_provider = model_provider.clone();
                let current_model = model_id.clone();
                let models = group.models;
                menu = menu.submenu(
                    group.provider_name.clone(),
                    window,
                    cx,
                    move |mut sub, _, _| {
                        for choice in &models {
                            let checked =
                                choice.provider == current_provider && choice.id == current_model;
                            let item_entity = item_entity.clone();
                            let item_id = item_id.clone();
                            let provider = choice.provider.clone();
                            let model = choice.id.clone();
                            sub = sub.item(
                                PopupMenuItem::new(choice.short_label().to_string())
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
                        // 这一层没有再嵌套。模型一多就超出窗口，在二级里滚。
                        sub.scrollable(true)
                    },
                );
            }
            menu
        })
        .into_any_element()
}

pub(crate) fn render_engine_chip(label: String, kind: Option<ConversationAgentKind>) -> AnyElement {
    Tag::secondary()
        .rounded_full()
        .flex_none()
        .gap_1()
        .child(render_engine_mark(kind, 12.))
        .child(label)
        .into_any_element()
}

/// 页头前缘的返回。可见的只有符号，悬停提示是控件名。它只退一层，侧栏标题负责回根页。
pub(crate) fn product_back_button(
    id: &'static str,
    tooltip: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    Button::new(id)
        .quiet()
        .icon(IconName::ChevronLeft)
        .icon_action(tooltip)
        .on_click(on_click)
}

/// 目录页头：和自动化顶栏同一条高度。左边页名加个数，不写副标题；右边是同尺寸的新建按钮。
pub(crate) fn product_page_header(
    title: &'static str,
    count: usize,
    count_unit: &'static str,
    action: AnyElement,
    window: &Window,
    cx: &App,
) -> AnyElement {
    api::host(cx)
        .page_header(window, cx)
        .justify_between()
        .gap_3()
        .child(
            div()
                .h(ui_theme::height::sm())
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_medium()
                        .text_color(rgb(ui_theme::text_bright()))
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(ui_theme::text_muted()))
                        .child(format!("{count} {count_unit}")),
                ),
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
        .icon(gpui_component::Icon::new(
            gpui_kit_assets::IconName::RefreshCw,
        ))
        .icon_action("重新扫描插件目录")
        .on_click(move |_, _, cx| {
            refresh_entity.update(cx, |page, cx| page.refresh_agent_plugins(cx));
        });
    let manage_entity = entity.clone();
    let manage_button = compact_chip("agent-plugins-manage")
        .label("管理")
        .tooltip("去能力库导入或删除插件")
        .on_click(move |_, _, cx| {
            manage_entity.update(cx, |page, cx| {
                page.open_capabilities("plugins", cx);
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
                plugin.kind.label(),
                &plugin.description,
                &plugin.origin,
                plugin.broken.clone(),
                selected.contains(&plugin.id),
                entity.clone(),
                cx,
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
            Some((kind, name)) => (kind.label(), name.to_string()),
            None => ("未知", id.clone()),
        };
        rows.push(agent_plugin_row(
            AgentPick::Plugin,
            &agent.id,
            id,
            &name,
            kind_label,
            "",
            "",
            Some("磁盘上已经找不到了".to_string()),
            true,
            entity.clone(),
            cx,
        ));
    }

    let body = if rows.is_empty() {
        div()
            .px_5()
            .py_4()
            .text_xs()
            .text_color(rgb(ui_theme::text_faint()))
            .child(pending.unwrap_or_else(|| {
                "还没有装任何插件。点「管理」去能力库导入技能或扩展。".to_string()
            }))
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
                                .text_sm()
                                .font_medium()
                                .text_color(rgb(ui_theme::text_bright()))
                                .child("技能"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(ui_theme::text_muted()))
                                .child(format!(
                                    "已勾选 {selected_count} 个。对话只加载勾选的技能和扩展，没勾的一律不加载。改动对新开的对话生效。"
                                )),
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
            .map(|path| format!("工作目录 {}", path.to_string_lossy()))
            .unwrap_or_else(|| "工作目录".to_string()),
        RuntimeKey::Remote(_) => "工作目录在那台机器上".to_string(),
    };

    let pick_agent = agent.id.clone();
    let pick_entity = entity.clone();
    let add_folder = compact_chip("agent-context-add-folder")
        .icon(IconName::Plus)
        .label("目录")
        .tooltip("绑定一个本地目录")
        .on_click(move |_, _, cx| {
            let agent_id = pick_agent.clone();
            pick_entity.update(cx, |page, cx| page.pick_agent_context_folders(agent_id, cx));
        });

    let link_agent = agent.id.clone();
    let link_entity = entity.clone();
    let add_link = compact_chip("agent-context-add-link")
        .icon(IconName::Plus)
        .label("链接")
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
            "目录",
            true,
            entity.clone(),
        ));
    }
    for link in &agent.context_links {
        rows.push(agent_context_row(
            &agent.id,
            link,
            "链接",
            false,
            entity.clone(),
        ));
    }

    let body = if rows.is_empty() {
        div()
            .px_5()
            .py_4()
            .text_xs()
            .text_color(rgb(ui_theme::text_faint()))
            .child("还没绑定任何上下文。绑定后智能体就知道去哪儿找素材。")
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
                                .text_sm()
                                .font_medium()
                                .text_color(rgb(ui_theme::text_bright()))
                                .child("绑定的上下文"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(ui_theme::text_muted()))
                                .child("目录和链接会写进 system prompt。改动对新开的对话生效。"),
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
            div()
                .px_3()
                .pb_2()
                .child(framed_text_input(&editor.context_link, "上下文链接").w_full()),
        )
        .child(
            div()
                .px_5()
                .pb_3()
                .text_xs()
                .text_color(rgb(ui_theme::text_faint()))
                .child(format!(
                    "{space_root}，同一智能体的所有对话共用它。工作目录和绑定目录下的 {} 会自动作为技能加载。",
                    api::model::pi_plugin_catalog::WORKSPACE_SKILL_DIRS.join(" / ")
                )),
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
        .accessibility_label("记忆")
        .tooltip(if memory_enabled {
            "关闭后不再注入和写入记忆，已有记忆保留"
        } else {
            "打开后在对话和自动化里使用记忆"
        })
        .on_click(move |checked, _, cx| {
            let agent_id = agent_id.clone();
            let enabled = *checked;
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
        AgentEditorTab::Config => "配置".to_string(),
        AgentEditorTab::Memory => match memory_pending_count(&editor.memories) {
            0 => "记忆".to_string(),
            pending => format!("记忆 · {pending} 待确认"),
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
        .label("管理")
        .icon(IconName::ChevronRight)
        .tooltip("查看、搜索和删除记忆")
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
                .text_sm()
                .font_medium()
                .text_color(rgb(ui_theme::text_bright()))
                .child("记忆"),
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
                .text_xs()
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_color(rgb(ui_theme::text_muted()))
                        .child(truncated_line(if agent.memory_enabled {
                            memory_summary_text(
                                editor.memories.agent.entries.len(),
                                editor.memories.user.entries.len(),
                            )
                        } else {
                            "已关闭：不注入、不写入，已有记忆保留".to_string()
                        })),
                )
                .when(pending > 0, |row| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .text_color(rgb(ui_theme::red()))
                            .child(format!("{pending} 条待确认")),
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
        .icon(gpui_component::Icon::new(
            gpui_kit_assets::IconName::RefreshCw,
        ))
        .icon_action("重新读取记忆")
        .on_click(move |_, _, cx| {
            refresh_entity.update(cx, |page, cx| page.refresh_agent_memories(cx));
        });
    let query = editor.memory_query.read(cx).value().to_string();
    let filter = editor.memory_filter;
    let narrowed = !query.trim().is_empty() || filter != MemoryFilter::All;
    let filter_entity = entity.clone();
    let filters = choice_tabs(
        "agent-memory-filters",
        MemoryFilter::ALL
            .iter()
            .position(|option| *option == filter),
        MemoryFilter::ALL.into_iter().map(|option| {
            Tab::new().label(option.label()).tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(option.tooltip()).build(window, cx)
            })
        }),
        move |ix, cx| {
            if let Some(option) = MemoryFilter::ALL.get(ix).copied() {
                filter_entity.update(cx, |page, cx| page.set_agent_memory_filter(option, cx));
            }
        },
    );
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
                                .text_sm()
                                .font_medium()
                                .text_color(rgb(ui_theme::text_bright()))
                                .child("记忆"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(ui_theme::text_muted()))
                                .child(if memory_enabled {
                                    "智能体在对话和自动化里记下的长期信息。改动从下一轮对话起生效。"
                                } else {
                                    "记忆已关闭：不注入、不写入。下面的记忆保留着，重新打开后照常使用。"
                                }),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(refresh_button)
                        .child(agent_memory_switch("agent-memory-enabled", agent, entity.clone())),
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
                        .child(Input::new(&editor.memory_query).small().cleanable(true)),
                )
                .child(filters),
        )
        .child(agent_memory_section(
            &agent.id,
            AgentMemorySection {
                layer: MemoryLayer::Agent,
                title: "这个智能体",
                empty_hint: "还没有记忆。用户纠正它或让它记住什么时，它会自己写下来。",
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
                title: "全局（所有智能体共享）",
                empty_hint: "还没有全局记忆。这里只放你本人的情况和你纠正过的做法。",
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
    title: &'static str,
    empty_hint: &'static str,
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
        .icon_size()
        .icon(IconName::FolderOpen)
        .icon_action("打开记忆文件夹")
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
            .text_xs()
            .text_color(rgb(ui_theme::text_faint()))
            .child(if narrowed && !listing.entries.is_empty() {
                "没有匹配的记忆"
            } else {
                empty_hint
            })
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
                        .text_xs()
                        .font_medium()
                        .text_color(rgb(ui_theme::text_muted()))
                        .child(memory_section_title(title, listing, narrowed.then_some(shown))),
                )
                .child(open_folder),
        )
        .when(listing.core_over_budget, |section| {
            section.child(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(ui_theme::red()))
                    .child(format!(
                        "规则类记忆正文超过 {}KB 常驻上限，启动时只会注入一部分。请合并或删掉过时的规则。",
                        api::model::agent_memory::CORE_MEMORY_MAX_BYTES / 1000,
                    )),
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
        Some(shown) => format!("{shown} / {total} 条"),
        None => format!("{total} 条"),
    };
    let pending = listing
        .entries
        .iter()
        .filter(|entry| entry.pending_reason.is_some())
        .count();
    if pending == 0 {
        format!("{title} · {count}")
    } else {
        format!("{title} · {count}，{pending} 条待确认")
    }
}

pub(crate) fn memory_kind_label(kind: Option<&str>) -> &'static str {
    match kind {
        Some("user") => "关于你",
        Some("feedback") => "做法",
        Some("project") => "事实",
        Some("reference") => "资源",
        _ => "格式有误",
    }
}

/// 行尾的状态：待确认和长期没用的标红，提示用户处理；用过的显示次数。
pub(crate) fn memory_usage_label(
    entry: &api::model::agent_memory::MemoryEntry,
) -> Option<(String, bool)> {
    if entry.pending_reason.is_some() {
        return Some(("待确认".to_string(), true));
    }
    if entry.stale {
        return Some(("长期未用".to_string(), true));
    }
    (entry.read_count > 0).then(|| (format!("用过 {} 次", entry.read_count), false))
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
    let subtitle = match &entry.pending_reason {
        // 待确认时把原因放在描述前面，用户采纳前能看到它为什么没生效。
        Some(reason) => format!("{reason}：{}", entry.description),
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
                .text_xs()
                .text_color(rgb(ui_theme::text_faint()))
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
                            .text_color(rgb(ui_theme::text_faint())),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .text_xs()
                                .text_color(rgb(ui_theme::text_bright()))
                                .overflow_hidden()
                                .child(truncated_line(entry.name.clone())),
                        ),
                )
                .child(
                    // 待确认时前面挂着原因，一行放不下就把描述挤没了；给两行再省略。
                    div()
                        .text_xs()
                        .text_color(rgb(ui_theme::text_faint()))
                        .text_ellipsis()
                        .line_clamp(2)
                        .child(subtitle),
                ),
        )
        .when_some(memory_usage_label(entry), |row, (label, stale)| {
            row.child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(rgb(if stale {
                        ui_theme::red()
                    } else {
                        ui_theme::text_faint()
                    }))
                    .child(label),
            )
        })
        .child(
            compact_chip(format!("agent-memory-open-{layer_key}-{}", entry.file_name))
                .icon_size()
                .icon(IconName::ExternalLink)
                .icon_action("用文本编辑器打开")
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
                .icon_size()
                .icon(IconName::Check)
                .accessibility_label("采纳")
                .tooltip("采纳：之后的对话会用到这条记忆")
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
            .icon_size()
            .icon(IconName::Close)
            .icon_action("删除这条记忆")
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
                .text_color(rgb(ui_theme::text_faint()))
                .child("（正文为空）")
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
        .bg(rgb(ui_theme::bg_column()))
        .text_xs()
        .text_color(rgb(ui_theme::text_muted()))
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
                .text_xs()
                .text_color(rgb(ui_theme::text_faint()))
                .child(kind_label.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(rgb(ui_theme::text_bright()))
                .overflow_hidden()
                .child(truncated_line(value.to_string())),
        )
        .child(
            compact_chip(format!("agent-context-remove-{kind_label}-{value}"))
                .icon_size()
                .icon(api::ui::trash_icon())
                .icon_action(if is_folder {
                    "解除这个目录的绑定"
                } else {
                    "移除这个链接"
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
        .icon(gpui_component::Icon::new(
            gpui_kit_assets::IconName::RefreshCw,
        ))
        .icon_action("重新读取 mcp.json")
        .on_click(move |_, _, cx| {
            refresh_entity.update(cx, |page, cx| page.refresh_agent_plugins(cx));
        });
    let manage_entity = entity.clone();
    let manage_button = compact_chip("agent-mcp-manage")
        .label("管理")
        .tooltip("去能力库添加、登录或停用 MCP 服务器")
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
                api::model::pi_mcp::PiMcpTransport::Http { url } => ("远程", url.clone()),
                api::model::pi_mcp::PiMcpTransport::Stdio { command, args } => {
                    ("本地", format!("{command} {}", args.join(" ")))
                }
                api::model::pi_mcp::PiMcpTransport::Unknown => ("未知", String::new()),
            };
            // 能力库里停用的服务器勾了也不会连：Pi 读到 `enabled: false` 就跳过。
            let broken = (!server.enabled).then(|| "已在能力库停用，勾选也不会连接".to_string());
            agent_plugin_row(
                AgentPick::McpServer,
                &agent.id,
                &server.name,
                &server.name,
                kind_label,
                server.description.as_deref().unwrap_or(""),
                &detail,
                broken,
                selected.contains(&server.name),
                entity.clone(),
                cx,
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
            "未知",
            "",
            "",
            Some("mcp.json 里已经没有这台服务器".to_string()),
            true,
            entity.clone(),
            cx,
        ));
    }

    let body =
        if rows.is_empty() {
            div()
                .px_5()
                .py_4()
                .text_xs()
                .text_color(rgb(ui_theme::text_faint()))
                .child(pending.unwrap_or_else(|| {
                    "还没有配置 MCP 服务器。点「管理」去能力库添加。".to_string()
                }))
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
                                .text_sm()
                                .font_medium()
                                .text_color(rgb(ui_theme::text_bright()))
                                .child("MCP"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(ui_theme::text_muted()))
                                .child(format!(
                                    "已勾选 {selected_count} 个。对话只连勾选的服务器，没勾的不启动。改动对新开的对话生效。"
                                )),
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
    cx: &App,
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
        rgb(ui_theme::text_faint())
    } else {
        cx.theme().danger.into()
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
            .on_click(move |_, _, cx| {
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
                                .text_xs()
                                .text_color(rgb(ui_theme::text_bright()))
                                .child(name.to_string()),
                        )
                        .child(
                            div()
                                .px_1p5()
                                .rounded(px(4.))
                                .bg(rgb(ui_theme::bg_stage()))
                                .text_xs()
                                .text_color(rgb(ui_theme::text_faint()))
                                .child(kind_label.to_string()),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .overflow_hidden()
                        .text_color(subtitle_color)
                        .child(truncated_line(subtitle)),
                ),
        )
        .into_any_element()
}

/// 组件默认描边叠在卡片底上几乎看不见；聚焦时还会在外侧加 3px ring，把整块撑开。
/// 短输入共用这一层：固定 1px 边 + 锁死高度 + 裁掉 ring。智能体和自动化都走这里。
pub(crate) fn editor_field_frame() -> Div {
    div()
        .flex()
        .items_center()
        .flex_none()
        .h(ui_theme::height::sm())
        .overflow_hidden()
        .border_1()
        .border_color(rgb(ui_theme::border_loud()))
        .rounded(px(8.))
        .bg(rgb(ui_theme::bg_column()))
}

pub(crate) fn framed_text_input(input: &Entity<InputState>, aria: &'static str) -> Div {
    editor_field_frame().child(
        Input::new(input)
            .small()
            .appearance(false)
            .focus_bordered(false)
            .focus_ring(false)
            .w_full()
            .aria_label(aria),
    )
}

pub(crate) fn framed_textarea(textarea: Textarea) -> AnyElement {
    div()
        .border_1()
        .border_color(rgb(ui_theme::border_loud()))
        .rounded(px(8.))
        .bg(rgb(ui_theme::bg_column()))
        .child(textarea.appearance(false))
        .into_any_element()
}
pub(crate) fn compact_chip(id: impl Into<ElementId>) -> Button {
    Button::new(id).quiet().compact()
}

/// 几个互斥选项的分段开关，选中块在选项之间滑动。`on_select` 拿到被点的下标。
pub(crate) fn choice_tabs(
    id: impl Into<ElementId>,
    selected: Option<usize>,
    tabs: impl IntoIterator<Item = Tab>,
    on_select: impl Fn(usize, &mut App) + 'static,
) -> TabBar {
    TabBar::new(id)
        .segmented()
        .flex_none()
        .when_some(selected, |bar, ix| bar.selected_index(ix))
        .on_click(move |ix, _, cx| on_select(*ix, cx))
        .children(tabs)
}
