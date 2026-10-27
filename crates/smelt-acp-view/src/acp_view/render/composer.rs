//! ACP 输入栏：补全、模型选择、发送。

use super::*;
use gpui_component::popover::PopoverState;
use smelt_client::daemon_state::DaemonPhase;

/// 排队条目用正文当身份。相同正文按出现次序分开，删掉前面一条时后面的格子跟着正文走。
fn queue_row_id<'a>(
    kind: &'static str,
    text: &'a str,
    seen: &mut std::collections::HashMap<&'a str, usize>,
) -> gpui::ElementId {
    let n = {
        let slot = seen.entry(text).or_insert(0);
        let n = *slot;
        *slot += 1;
        n
    };
    let key = if n == 0 {
        text.to_string()
    } else {
        format!("{text}\u{0}{n}")
    };
    gpui::ElementId::from((gpui::ElementId::from(kind), key))
}

const CAPABILITIES_PANEL_WIDTH: f32 = 320.;

/// 能力弹层：这场对话加载的技能和连的 MCP，按类型分组，底部去改勾选。勾选跟着智能体
/// 定义、启动时就定了，对话里改不了，所以行都只读。
fn capabilities_panel(
    skills: &[smelt_model::pi_plugin_catalog::PiPlugin],
    mcp_servers: &[smelt_model::pi_mcp::PiMcpServer],
    by_agent: bool,
    view: gpui::WeakEntity<AcpView>,
    popover: gpui::WeakEntity<PopoverState>,
) -> gpui::Div {
    let mut panel = v_flex().p_2();
    if skills.is_empty() && mcp_servers.is_empty() {
        panel = panel.child(
            div()
                .px_1()
                .py_3()
                .text_sm()
                .text_color(gpui::rgb(ui_theme::text_muted()))
                .child("这场对话没有加载技能或 MCP"),
        );
    } else {
        let skill_rows = skills.iter().map(|skill| {
            capability_row(
                skill.name.clone(),
                // 描述可能是多行的 frontmatter，压成一行再截断。
                skill
                    .description
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                Some(skill.origin.clone()),
            )
        });
        let mcp_rows = mcp_servers
            .iter()
            .map(|server| capability_row(server.name.clone(), mcp_server_detail(server), None));
        panel = panel.child(
            v_flex()
                .id("acp-capabilities-list")
                .max_h(px(360.))
                .overflow_y_scroll()
                .child(capability_group_header("技能", skills.len()))
                .children(skill_rows)
                .when(skills.is_empty(), |list| list.child(capability_none()))
                .child(capability_separator())
                .child(capability_group_header("MCP", mcp_servers.len()))
                .children(mcp_rows)
                .when(mcp_servers.is_empty(), |list| list.child(capability_none())),
        );
    }
    let (source, action) = if by_agent {
        ("按智能体的勾选加载", "在智能体里修改")
    } else {
        ("按 Pi 的自动发现加载", "在能力库里管理")
    };
    panel.child(capability_separator()).child(
        h_flex()
            .px_1()
            .gap_2()
            .items_center()
            .justify_between()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(gpui::rgb(ui_theme::text_faint()))
                    .child(source),
            )
            .child(
                Button::new("acp-capabilities-manage")
                    .quiet()
                    .compact()
                    .label(action)
                    .on_click(move |_, window, cx| {
                        let _ = popover.update(cx, |state, cx| state.dismiss(window, cx));
                        let _ = view.update(cx, |_, cx| {
                            cx.emit(AcpViewEvent::ManageCapabilities { by_agent })
                        });
                    }),
            ),
    )
}

fn capability_group_header(title: &'static str, count: usize) -> impl IntoElement {
    h_flex()
        .px_1()
        .pt_1()
        .pb_0p5()
        .justify_between()
        .text_xs()
        .text_color(gpui::rgb(ui_theme::text_faint()))
        .child(title)
        .child(count.to_string())
}

/// 一行：名字和来源在上，说明在下，都截成一行。
fn capability_row(name: String, detail: String, tag: Option<String>) -> impl IntoElement {
    v_flex()
        .px_1()
        .py_1()
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(gpui::rgb(ui_theme::text_mid()))
                        .child(name),
                )
                .children(tag.map(|tag| {
                    div()
                        .flex_none()
                        .max_w(px(140.))
                        .truncate()
                        .text_xs()
                        .text_color(gpui::rgb(ui_theme::text_faint()))
                        .child(tag)
                })),
        )
        .when(!detail.is_empty(), |row| {
            row.child(
                div()
                    .truncate()
                    .text_xs()
                    .text_color(gpui::rgb(ui_theme::text_muted()))
                    .child(detail),
            )
        })
}

fn capability_none() -> impl IntoElement {
    div()
        .px_1()
        .pb_1()
        .text_xs()
        .text_color(gpui::rgb(ui_theme::text_muted()))
        .child("没有")
}

fn capability_separator() -> impl IntoElement {
    div().h(px(1.)).my_1().bg(ui_theme::hairline())
}

/// MCP 行的第二行：有说明用说明，没有就给连接方式（运行时已经打过码）。
fn mcp_server_detail(server: &smelt_model::pi_mcp::PiMcpServer) -> String {
    if let Some(description) = &server.description {
        return description.clone();
    }
    match &server.transport {
        smelt_model::pi_mcp::PiMcpTransport::Http { url } => url.clone(),
        smelt_model::pi_mcp::PiMcpTransport::Stdio { command, args } => std::iter::once(command)
            .chain(args)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        smelt_model::pi_mcp::PiMcpTransport::Unknown => String::new(),
    }
}

impl AcpView {
    pub(super) fn render_composer(
        &self,
        composer_focus: f32,
        sweep_presence: f32,
        animate_ambient: bool,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if matches!(
            &self.conversation_binding,
            smelt_model::conversation::ConversationBinding::Automation { .. }
        ) {
            // 自动化 Run 没有输入框；有进程或子智能体在跑时，入口单独占底部一行。
            let chip = self.render_background_task_chip(cx);
            let subagent_chip = self.render_subagent_chip(cx);
            if chip.is_none() && subagent_chip.is_none() {
                return None;
            }
            return Some(
                v_flex()
                    .relative()
                    .w_full()
                    .px_4()
                    .py_2()
                    .items_center()
                    .children(self.render_background_task_popover(cx))
                    .children(self.render_subagent_popover(cx))
                    .child(
                        h_flex()
                            .w_full()
                            .max_w(ui_theme::conversation_max_width())
                            .gap_2()
                            .children(subagent_chip)
                            .children(chip),
                    )
                    .into_any_element(),
            );
        }
        let t = cx.theme();
        let muted = t.muted_foreground;
        // ACP 的 grouped select 通常按 provider 组织模型。保留分组后，输入栏可
        // 先切 Provider，再只展示该 Provider 下的 Model；平铺协议仍走旧模型入口。
        // 运行中点选先 overlay 到勾选和胶囊上，agent 确认前快照旧值不能把它打回去。
        let pending_config_values = self.pending_config_values.clone();
        let next_turn_hint = self.has_active_turn();
        let next_turn_notice = composer_next_turn_notice(
            &pending_config_choice_names(
                &pending_config_values,
                &self.config_options,
                self.model.as_ref(),
            ),
            next_turn_hint,
        );
        let model_state = self
            .model
            .as_ref()
            .map(|model| overlay_model_state(model, &pending_config_values));
        let config_options = overlay_session_configs(&self.config_options, &pending_config_values);
        // 补全弹层画在输入框上方，并与 composer 共用宽度和容器。它仍在正常流中，
        // 因而不会被窗口底边裁掉，但视觉上不再是一条横贯消息区的列表。
        let completion_bar = self.completion.as_ref().map(|popup| {
            let mut list = v_flex()
                .id("acp-completion")
                .w_full()
                .max_w(ui_theme::conversation_max_width())
                .max_h(px(260.))
                .overflow_y_scroll()
                .track_scroll(&self.completion_scroll)
                .mb_2()
                .rounded(ui_theme::card_radius())
                .border_1()
                .border_color(ui_theme::card_stroke())
                .bg(ui_theme::glass_floating())
                .shadow_lg();
            for (ix, item) in popup.items.iter().enumerate() {
                let selected = ix == popup.selected;
                let label_color = if selected {
                    gpui::rgb(ui_theme::text_selected())
                } else {
                    gpui::rgb(ui_theme::text_mid())
                };
                let label = if let Some(range) = item.match_range.clone() {
                    h_flex()
                        .flex_shrink_0()
                        .text_xs()
                        .font_family(smelt_model::font_config::font_family())
                        .text_color(label_color)
                        .child(item.label[..range.start].to_string())
                        .child(
                            div()
                                .font_semibold()
                                .text_color(gpui::rgb(ui_theme::accent()))
                                .child(item.label[range.clone()].to_string()),
                        )
                        .child(item.label[range.end..].to_string())
                        .into_any_element()
                } else {
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .font_family(smelt_model::font_config::font_family())
                        .text_color(label_color)
                        .child(item.label.clone())
                        .into_any_element()
                };
                list = list.child(
                    ListRow::new((
                        gpui::ElementId::from("acp-completion-item"),
                        item.label.clone(),
                    ))
                    .card()
                    .selected(selected)
                    .flex()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .child(label)
                    .when(!item.hint.is_empty(), |row| {
                        row.child(
                            div()
                                .min_w_0()
                                .text_xs()
                                .text_color(if selected {
                                    gpui::rgb(ui_theme::text_selected()).into()
                                } else {
                                    muted
                                })
                                .truncate()
                                .child(item.hint.clone()),
                        )
                    })
                    .on_click(cx.listener(move |this, _ev, window, cx| {
                        if let Some(popup) = &mut this.completion {
                            popup.selected = ix;
                        }
                        this.accept_completion(window, cx);
                    })),
                );
            }
            list.child(
                div()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(t.border)
                    .text_xs()
                    .text_color(muted)
                    .child("↑↓ 选择   Enter/Tab 插入   Esc 关闭"),
            )
        });

        let input_row = self.input.as_ref().map(|input| {
            let provider_groups = model_state
                .as_ref()
                .map(|model| model.provider_groups.clone())
                .unwrap_or_default();
            let current_provider = model_state
                .as_ref()
                .and_then(selected_provider_group)
                .cloned();
            let grouped_models = current_provider.as_ref().map(|group| group.options.clone());
            let model_options = grouped_models.clone().unwrap_or_else(|| {
                model_state
                    .as_ref()
                    .map(|model| model.options.clone())
                    .unwrap_or_default()
            });
            let current_model = model_state
                .as_ref()
                .map(|model| model.current_value.clone());
            let model_config_id = model_state.as_ref().map(|model| model.config_id.clone());
            let model_label = composer_model_label(model_state.as_ref(), self.agent.short_label());
            let usage_pct = self.usage.map(|(used, size)| usage_percent(used, size));
            let usage_color = if self.compacting {
                ui_theme::yellow()
            } else {
                usage_pct
                    .map(usage_ring_color)
                    .unwrap_or(ui_theme::text_faint())
            };
            let extra_configs: Vec<_> = config_options
                .into_iter()
                .filter(|config| config.options.len() > 1)
                .collect();
            let has_menu = !composer_menu_sections(
                provider_groups.len(),
                model_options.len(),
                extra_configs.len(),
            )
            .is_empty();
            let model_control = if has_menu {
                let this = cx.entity();
                // Provider 菜单项要按 agent 决定是否补额度括号，闭包里拿不到 self。
                let agent = self.agent;
                let current_provider_id = current_provider.as_ref().map(|p| p.id.clone());
                let current_model_name = model_state
                    .as_ref()
                    .map(|model| model.current_name.clone())
                    .unwrap_or_default();
                let grouped = grouped_models.is_some();
                Button::new("acp-model-pill")
                    .quiet()
                    .compact()
                    .capsule()
                    .dropdown_caret(true)
                    .label(model_label)
                    .when(next_turn_notice.is_some(), |button| {
                        button.text_color(cx.theme().warning)
                    })
                    // 输入栏贴窗口底，默认 TopLeft 会往下弹，长模型列表把配置挤出屏幕。
                    .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, window, cx| {
                        let mut menu = menu;
                        let nest_models = composer_should_nest_models(
                            extra_configs.len(),
                            provider_groups.len(),
                            model_options.len(),
                        );
                        // 根菜单一带滚动就画不出「模型」二级，滚动只能放在没有再嵌套的那一层。
                        let model_scroll = composer_model_scroll(nest_models);
                        let sections = composer_menu_sections(
                            provider_groups.len(),
                            model_options.len(),
                            extra_configs.len(),
                        );
                        for (ix, section) in sections.iter().enumerate() {
                            if ix > 0 {
                                menu = menu.separator();
                            }
                            match *section {
                                ComposerMenuSection::Provider => {
                                    menu = menu.item(PopupMenuItem::label("Provider"));
                                    for provider in &provider_groups {
                                        let is_current = current_provider_id.as_deref()
                                            == Some(provider.id.as_str());
                                        let name = provider_menu_label(agent, provider, cx);
                                        // 只有一个 provider 时没得切，如实列出当前项即可。
                                        let switch_to = if provider_groups.len() > 1 {
                                            provider_switch_value(provider, &current_model_name)
                                                .map(ToOwned::to_owned)
                                        } else {
                                            None
                                        };
                                        let item =
                                            PopupMenuItem::new(name).checked(is_current);
                                        let item = match switch_to {
                                            Some(target_value) => {
                                                let config_id = model_config_id.clone();
                                                let this = this.clone();
                                                item.on_click(move |_ev, _window, cx| {
                                                    if let Some(config_id) = config_id.clone() {
                                                        this.update(cx, |v, view_cx| {
                                                            v.select_config_option(
                                                                config_id,
                                                                target_value.clone(),
                                                                view_cx,
                                                            );
                                                        });
                                                    }
                                                })
                                            }
                                            None => item.disabled(true),
                                        };
                                        menu = menu.item(item);
                                    }
                                }
                                ComposerMenuSection::Model => {
                                    let model_options = model_options.clone();
                                    let current_model = current_model.clone();
                                    let config_id = model_config_id.clone();
                                    let this = this.clone();
                                    let fill = move |mut menu: PopupMenu| {
                                        for (value, name) in &model_options {
                                            let is_cur =
                                                current_model.as_deref() == Some(value.as_str());
                                            let option_label = if grouped {
                                                name.clone()
                                            } else {
                                                model_label_with_provider(value, name)
                                            };
                                            let value = value.clone();
                                            let config_id = config_id.clone();
                                            let this = this.clone();
                                            let item = PopupMenuItem::new(option_label)
                                                .checked(is_cur);
                                            // 单模型没得切，但必须显示，否则弹层只剩会话配置。
                                            let item = if model_options.len() > 1 {
                                                item.on_click(move |_ev, _window, cx| {
                                                    if let Some(config_id) = config_id.clone() {
                                                        this.update(cx, |v, view_cx| {
                                                            v.select_config_option(
                                                                config_id,
                                                                value.clone(),
                                                                view_cx,
                                                            );
                                                        });
                                                    }
                                                })
                                            } else {
                                                item.disabled(true)
                                            };
                                            menu = menu.item(item);
                                        }
                                        menu
                                    };
                                    let model_section = composer_config_section_label(
                                        "模型",
                                        model_config_id.as_deref().is_some_and(|id| {
                                            config_selection_is_pending(&pending_config_values, id)
                                        }),
                                        next_turn_hint,
                                    );
                                    menu = if nest_models {
                                        menu.submenu(model_section, window, cx, move |menu, _, _| {
                                            let menu = fill(menu);
                                            if model_scroll.submenu {
                                                menu.scrollable(true)
                                            } else {
                                                menu
                                            }
                                        })
                                    } else {
                                        fill(menu.item(PopupMenuItem::label(model_section)))
                                    };
                                }
                                ComposerMenuSection::Config(config_ix) => {
                                    let config = &extra_configs[config_ix];
                                    menu = menu.item(PopupMenuItem::label(
                                        composer_config_section_label(
                                            &config.name,
                                            config_selection_is_pending(
                                                &pending_config_values,
                                                &config.config_id,
                                            ),
                                            next_turn_hint,
                                        ),
                                    ));
                                    for (value, name) in &config.options {
                                        let is_cur = config.current_name == *name;
                                        let value = value.clone();
                                        let config_id = config.config_id.clone();
                                        let this = this.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(name.clone())
                                                .checked(is_cur)
                                                .on_click(move |_ev, _window, cx| {
                                                    this.update(cx, |v, view_cx| {
                                                        v.select_config_option(
                                                            config_id.clone(),
                                                            value.clone(),
                                                            view_cx,
                                                        );
                                                    });
                                                }),
                                        );
                                    }
                                }
                            }
                        }
                        if model_scroll.root {
                            menu = menu.scrollable(true);
                        }
                        menu
                    })
                    .into_any_element()
            } else {
                div()
                    .h(ui_theme::height::xs())
                    .px_2()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .text_sm()
                    .text_color(gpui::rgb(ui_theme::text_mid()))
                    .child(model_label)
                    .into_any_element()
            };
            let capabilities_control = (self.agent == ConversationAgentKind::Pi).then(|| {
                // 缓存在 `ensure_loaded_skills`；这里再向运行时要，会把扫盘拉回每帧。
                let skills = self.loaded_skills().to_vec();
                let mcp_servers = self.loaded_mcp_servers().to_vec();
                // 产品智能体按勾选加载，裸 Pi 按 Pi 自己的全局目录和 mcp.json。
                let by_agent = self
                    .launch
                    .env
                    .contains_key(smelt_model::agent_kind::SMELT_AGENT_PLUGIN_ARGS_ENV);
                let view = cx.entity().downgrade();
                // 图标作为子元素放进去，保持 16px；理由同旁边的「会话追踪」。
                let trigger = Button::new("acp-capabilities")
                    .quiet()
                    .icon_size()
                    .text_color(gpui::rgb(ui_theme::text_muted()))
                    .child(Icon::new(gpui_kit_assets::IconName::Puzzle).size(px(16.)))
                    .icon_action(loaded_capabilities_summary(skills.len(), mcp_servers.len()));
                Popover::new("acp-capabilities-pop")
                    .anchor(Anchor::BottomLeft)
                    .w(px(CAPABILITIES_PANEL_WIDTH))
                    .trigger(trigger)
                    .content(move |_, _, cx| {
                        let popover = cx.entity().downgrade();
                        capabilities_panel(&skills, &mcp_servers, by_agent, view.clone(), popover)
                    })
                    .into_any_element()
            });
            let composer_border = mix_rgba(
                ui_theme::overlay(0x22),
                ui_theme::tint(ui_theme::accent(), 0x66),
                composer_focus,
            );
            let composer = v_flex()
                .relative()
                .w_full()
                .max_w(ui_theme::conversation_max_width())
                .rounded(ui_theme::composer_radius())
                .border_1()
                .border_color(composer_border)
                .bg(ui_theme::glass_input())
                .shadow(composer_shadows(
                    composer_focus,
                    gpui::rgb(ui_theme::accent()),
                ))
                .child(
                    div()
                        .id("acp-composer-input")
                        .px_4()
                        .pt_3()
                        .pb_1()
                        .min_h(px(52.))
                        .when(
                            self.supports_native_queue && self.is_visibly_running(),
                            |box_| {
                                box_.tooltip(|window, cx| {
                                    gpui_component::tooltip::Tooltip::new(
                                        composer_native_queue_shortcut_hint(),
                                    )
                                    .build(window, cx)
                                })
                            },
                        )
                        .child(Textarea::new(input).appearance(false)),
                )
                .when(
                    !self.queued_steering.is_empty() || !self.queued_follow_up.is_empty(),
                    |col| {
                        let mut strip = v_flex().px_4().pt_3().gap_1p5();
                        let can_move_next = !matches!(
                            self.phase,
                            DaemonPhase::Connecting | DaemonPhase::Dead
                        );
                        let immediate_cancels_turn = should_cancel_for_immediate_prompt(
                            &self.phase,
                            self.prompt_dispatch_pending,
                        );
                        let immediate_action_available = can_move_next;
                        let native_items = self.queued_steering.iter().map(|text| {
                            (false, text.as_str())
                        }).chain(self.queued_follow_up.iter().map(|text| {
                            (true, text.as_str())
                        }));
                        let mut seen_steer = std::collections::HashMap::new();
                        let mut seen_follow = std::collections::HashMap::new();
                        for (ix, (follow, text)) in native_items.enumerate() {
                            let kind = native_queue_item_kind_label(follow);
                            let row_id = queue_row_id(
                                if follow {
                                    "acp-native-queue-follow"
                                } else {
                                    "acp-native-queue-steer"
                                },
                                text,
                                if follow {
                                    &mut seen_follow
                                } else {
                                    &mut seen_steer
                                },
                            );
                            let preview: String = text.chars().take(60).collect();
                            let preview = if text.trim().is_empty() {
                                // 只有图的插话，文字是空串。
                                "[图片]".to_string()
                            } else if text.chars().count() > 60 {
                                format!("{preview}…")
                            } else {
                                preview
                            };
                            strip = strip.child(
                                h_flex()
                                    .id(row_id)
                                    .gap_2()
                                    .items_center()
                                    .px_2p5()
                                    .py_1()
                                    .rounded_md()
                                    .bg(ui_theme::overlay(0x14))
                                    .border_1()
                                    .border_color(t.border)
                                    .child(
                                        Icon::new(gpui_kit_assets::IconName::Clock)
                                            .size_3p5()
                                            .text_color(gpui::rgb(ui_theme::text_muted())),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .text_xs()
                                            .text_color(gpui::rgb(ui_theme::text_muted()))
                                            .child(format!("{kind} · {preview}")),
                                    )
                                    .when(immediate_action_available, |row| {
                                        let immediate_label = if immediate_cancels_turn {
                                            "立即发送（停止当前回答）"
                                        } else {
                                            "立即发送"
                                        };
                                        row.child(
                                            Button::new("acp-native-queue-immediate")
                                                .quiet()
                                                .icon_size()
                                                .label(immediate_label)
                                                .text_color(gpui::rgb(if immediate_cancels_turn {
                                                    ui_theme::yellow()
                                                } else {
                                                    ui_theme::accent()
                                                }))
                                                .tooltip(if immediate_cancels_turn {
                                                    "停止当前回答后，立即发送这条消息"
                                                } else {
                                                    "立即发送这条消息"
                                                })
                                                .on_click(cx.listener(
                                                    move |this, _ev, _window, cx| {
                                                        this.send_native_queue_item_immediately(
                                                            ix, cx,
                                                        );
                                                    },
                                                )),
                                        )
                                    }),
                            );
                        }
                        if self.supports_native_queue {
                            strip = strip.child(
                                Button::new("acp-native-queue-clear")
                                    .quiet()
                                    .icon_size()
                                    .label("撤回排队")
                                    .text_color(gpui::rgb(ui_theme::text_muted()))
                                    .on_click(cx.listener(|this, _ev, _window, cx| {
                                        this.clear_native_queue(cx);
                                    })),
                            );
                        }
                        col.child(strip)
                    },
                )
                // 守护进程排队条：不能中途插话的 agent 在回合结束前不会收到新消息，
                // 得让人看见「排队中」，还能撤回改了再发，免得以为消息丢了。
                .when(!self.pending_prompts.is_empty(), |col| {
                    let mut strip = v_flex().px_4().pt_3().gap_1p5();
                    for item in &self.pending_prompts {
                        let id = item.id;
                        let preview: String = item.text.chars().take(60).collect();
                        let preview = if item.text.trim().is_empty() {
                            "[图片]".to_string()
                        } else if item.text.chars().count() > 60 {
                            format!("{preview}…")
                        } else {
                            preview
                        };
                        let img_suffix = if item.image_count == 0 || item.text.trim().is_empty() {
                            String::new()
                        } else {
                            format!("（含 {} 张图）", item.image_count)
                        };
                        strip = strip.child(
                            h_flex()
                                .id(("acp-pending-prompt", id as usize))
                                .gap_2()
                                .items_center()
                                .px_2p5()
                                .py_1()
                                .rounded_md()
                                .bg(ui_theme::overlay(0x14))
                                .border_1()
                                .border_color(t.border)
                                .child(
                                    Icon::new(gpui_kit_assets::IconName::Clock)
                                        .size_3p5()
                                        .text_color(gpui::rgb(ui_theme::text_muted())),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .text_xs()
                                        .text_color(gpui::rgb(ui_theme::text_muted()))
                                        .child(format!("排队中 · {preview}{img_suffix}")),
                                )
                                .when(item.withdrawable, |row| {
                                    row.child(
                                        Button::new(("acp-pending-prompt-withdraw", id as usize))
                                            .quiet()
                                            .icon_size()
                                            .label("撤回")
                                            .text_color(gpui::rgb(ui_theme::text_muted()))
                                            .tooltip("撤回这条，放回输入框")
                                            .on_click(cx.listener(
                                                move |this, _ev, _window, cx| {
                                                    this.withdraw_pending_prompt(id, cx);
                                                },
                                            )),
                                    )
                                }),
                        );
                    }
                    col.child(strip)
                })
                // 别的 Pi 对话发来、还没送进这个对话的消息。这边忙时要等手上的工具调用做完才送到，
                // 期间不画出来就像丢了。送到后正文在对话里显示，这一行随之消失。
                .when(
                    self.peer_inbox.iter().any(|message| {
                        message.status == smelt_model::acp_session::PeerMessageStatus::Pending
                    }),
                    |col| {
                        let muted = gpui::rgb(ui_theme::text_muted());
                        let mut strip = v_flex().px_4().pt_3().gap_1p5();
                        let pending = self.peer_inbox.iter().filter(|message| {
                            message.status == smelt_model::acp_session::PeerMessageStatus::Pending
                        });
                        for (ix, message) in pending.enumerate() {
                            let from = if message.source_title.trim().is_empty() {
                                message.source_session_id.as_str()
                            } else {
                                message.source_title.as_str()
                            };
                            strip = strip.child(
                                h_flex()
                                    .id(("acp-peer-inbox", ix))
                                    .gap_2()
                                    .items_center()
                                    .px_2p5()
                                    .py_1()
                                    .rounded_md()
                                    .bg(ui_theme::overlay(0x14))
                                    .border_1()
                                    .border_color(t.border)
                                    .child(Icon::new(IconName::Inbox).size_3p5().text_color(muted))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .truncate()
                                            .text_xs()
                                            .text_color(muted)
                                            .child(format!(
                                                "〈{from}〉{} · 还没送到，手上的工具调用做完就送进来",
                                                message.kind.received()
                                            )),
                                    ),
                            );
                        }
                        col.child(strip)
                    },
                )
                // 待发图片的缩略图条：粘完得看得见「贴上了」，还得能反悔。
                .when(!self.pending_images.is_empty(), |col| {
                    let mut strip = h_flex().px_4().pt_3().gap_2().items_center().flex_wrap();
                    for (ix, im) in self.pending_images.iter().enumerate() {
                        let awaiting_decode = smelt_ui::image::preview_pending(im);
                        if awaiting_decode {
                            smelt_ui::image::ensure_preview(im, cx);
                        }
                        // 地址而不是下标：删掉前面一张时，后面的缩略图不会重播挤入。
                        let image_key = std::sync::Arc::as_ptr(im) as usize;
                        let enter = self.pending_image_enter.get(&image_key).map(|started| {
                            let t = (started.elapsed().as_secs_f32() / (ENTER_MS as f32 / 1000.0))
                                .clamp(0.0, 1.0);
                            ease_snap(t)
                        });
                        let thumb = if awaiting_decode {
                            let image = im.clone();
                            ambient_animation(
                                ("acp-pending-img-pulse", image_key),
                                std::time::Duration::from_millis(700),
                                true,
                                move |delta| {
                                    let opacity = 1.0 - 0.4 * (delta * std::f32::consts::PI).sin();
                                    div().opacity(opacity).child(smelt_ui::image::thumb_element(
                                        &image,
                                        px(56.),
                                        px(96.),
                                        8.0,
                                    ))
                                },
                            )
                            .into_any_element()
                        } else {
                            smelt_ui::image::thumb_element(im, px(56.), px(96.), 8.0)
                        };
                        let preview_image = im.clone();
                        let remove_image = cx.entity().downgrade();
                        strip = strip.child(
                            div()
                                .id(("acp-pending-img", image_key))
                                .role(gpui::Role::Button)
                                .aria_label("预览图片")
                                .relative()
                                
                                .hover(|d| d.opacity(0.88))
                                .border_1()
                                .border_color(t.border)
                                .rounded_md()
                                .overflow_hidden()
                                .when_some(enter, |card, factor| {
                                    card.max_w(px(96. * factor.max(0.15))).opacity(factor)
                                })
                                .on_click(cx.listener(move |this, _ev, _window, cx| {
                                    let _ = this;
                                    cx.emit(AcpViewEvent::PreviewImage(preview_image.clone()));
                                }))
                                .child(thumb)
                                .child(
                                    // 右上角小 ×：点掉这张。
                                    div()
                                        .id(("acp-pending-img-remove", image_key))
                                        .role(gpui::Role::Button)
                                        .aria_label("移除图片")
                                        .absolute()
                                        .top(px(-4.))
                                        .right(px(-4.))
                                        .size(px(16.))
                                        .rounded_full()
                                        .bg(ui_theme::overlay(0xcc))
                                        .text_xs()
                                        .text_color(gpui::rgb(ui_theme::text_mid()))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        
                                        .hover(|d| d.opacity(0.8))
                                        .child("×")
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(move |this, _ev, _window, cx| {
                                                if ix < this.pending_images.len() {
                                                    this.pending_images.remove(ix);
                                                }
                                                cx.stop_propagation();
                                                cx.notify();
                                            }),
                                        )
                                        .on_a11y_action(
                                            gpui::AccessibleAction::Click,
                                            move |_, _, cx| {
                                                let _ = remove_image.update(cx, |this, cx| {
                                                    if ix < this.pending_images.len() {
                                                        this.pending_images.remove(ix);
                                                    }
                                                    cx.notify();
                                                });
                                            },
                                        ),
                                ),
                        );
                    }
                    col.child(strip)
                })
                .child(
                    h_flex()
                        .px_4()
                        .pt_1()
                        .pb_3()
                        .gap_2()
                        .items_center()
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w(px(0.))
                                .gap_2()
                                .items_center()
                                .child(model_control)
                                .children(next_turn_notice.as_ref().map(|label| {
                                    div()
                                        .flex_shrink_0()
                                        .text_xs()
                                        .text_color(gpui::rgb(ui_theme::yellow()))
                                        .child(label.clone())
                                }))
                                .children(capabilities_control)
                                .child(
                                    // 图标作为子元素放进去，保持 16px；`icon()` 会按按钮尺寸缩到 13.5px。
                                    Button::new("acp-trajectory")
                                        .quiet()
                                        .icon_size()
                                        .text_color(gpui::rgb(ui_theme::text_muted()))
                                        .child(Icon::new(IconName::Inspector).size(px(16.)))
                                        .icon_action("会话追踪")
                                        .on_click(cx.listener(|this, _ev, _window, cx| {
                                            this.open_trajectory_window(cx);
                                        })),
                                )
                                .when(self.compacting || self.usage.is_some(), |row| {
                                    let ring_color: gpui::Hsla = gpui::rgb(usage_color).into();
                                    let track_color: gpui::Hsla =
                                        gpui::rgb(ui_theme::text_muted()).into();
                                    let pct = usage_pct.unwrap_or(0);
                                    let compacting = self.compacting;
                                    let popover_open = self.usage_popover_open;
                                    let usage = self.usage;
                                    let cached_read = self.usage_cached_read;
                                    let mut chip = div()
                                        .id("acp-usage")
                                        .role(gpui::Role::Button)
                                        .aria_label(if compacting {
                                            "压缩中"
                                        } else {
                                            "上下文用量"
                                        })
                                        .debug_selector(|| "ACP_USAGE_RING".into())
                                        .flex_shrink_0()
                                        .size(px(18.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        
                                        .hover(|d| d.opacity(0.8))
                                        .child(if compacting {
                                            ambient_spinner(
                                                "acp-usage-spin",
                                                gpui::rgb(ui_theme::yellow()).into(),
                                                true,
                                            )
                                        } else {
                                            usage_ring(pct, ring_color, track_color)
                                        })
                                        .on_click(cx.listener(|this, _ev, _window, cx| {
                                            this.usage_popover_open = !this.usage_popover_open;
                                            this.background_tasks_open = false;
                                            this.subagents_open = false;
                                            cx.notify();
                                        }));
                                    if !popover_open {
                                        chip = chip.tooltip(move |window, cx| {
                                            if compacting {
                                                return gpui_component::tooltip::Tooltip::new(
                                                    "压缩中…",
                                                )
                                                .build(window, cx);
                                            }
                                            let (used, size) = usage.unwrap_or((0, 0));
                                            gpui_component::tooltip::Tooltip::element(
                                                move |_, _| {
                                                    render_usage_hover_card(used, size, cached_read)
                                                },
                                            )
                                            .p_2()
                                            .build(window, cx)
                                        });
                                    }
                                    row.child(chip)
                                })
                                .children(self.render_subagent_chip(cx))
                                .children(self.render_background_task_chip(cx))
                        )
                        .children(self.restart_error.as_ref().map(|err| {
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(gpui::rgb(ui_theme::red()))
                                .child(format!("重启失败：{err}"))
                        }))
                        .child(
                            Button::new("acp-attach")
                                .normal()
                                .circle()
                                .icon(IconName::Plus)
                                .icon_action("添加文件")
                                .on_click(cx.listener(|this, _ev, window, cx| {
                                    this.pick_composer_files(window, cx);
                                })),
                        )
                        // 主操作始终一个：跑着是停止，空闲才是发送。排队仍走 Enter。
                        .when(self.is_visibly_running(), |row| {
                            let in_cancel = self.cancel_requested_at.is_some() || self.restarting;
                            let label = if self.restarting {
                                "正在重启"
                            } else if in_cancel {
                                "强制停止"
                            } else {
                                "停止"
                            };
                            row.child(
                                Button::new("acp-stop")
                                    .danger()
                                    .circle()
                                    .label("■")
                                    .icon_action(label)
                                    .on_click(
                                        cx.listener(|this, _ev, _window, cx| this.on_stop_button_click(cx)),
                                    ),
                            )
                        })
                        .when(!self.is_visibly_running(), |row| {
                            let can_send =
                                self.input_has_draft || !self.pending_images.is_empty();
                            row.child(
                                // 圆钮 + 上箭头。有内容才是主操作，空着不可点。
                                Button::new("acp-send")
                                    .commit()
                                    .circle()
                                    .disabled(!can_send)
                                    .label("↑")
                                    .accessibility_label("发送")
                                    .on_click(cx.listener(|this, _ev, window, cx| {
                                        this.submit_input(window, false, cx);
                                    })),
                            )
                        }),
                )
                // 跑着时顶边上来回扫一道光。轨道骑在 1px 顶边上，左右让开圆角，只走直的那段。
                // 回合结束后等它淡完再拿掉。
                .when(sweep_presence > 0.0, |col| {
                    // 等批准、等回答时回合还算在跑，但该用户动手了：停住、换成审批卡的黄。
                    let awaiting_user =
                        !self.permissions.is_empty() || self.elicitation.is_some();
                    let (color, mode) = if awaiting_user {
                        (ui_theme::yellow(), SweepMode::Hold)
                    } else if animate_ambient && !cx.reduce_motion() {
                        (ui_theme::blue(), SweepMode::Sweep)
                    } else {
                        (ui_theme::blue(), SweepMode::Still)
                    };
                    col.child(
                        div()
                            .absolute()
                            .top(px(-1.))
                            .left(ui_theme::composer_radius())
                            .right(ui_theme::composer_radius())
                            .h(px(1.))
                            .child(running_sweep(
                                "acp-composer-running-sweep",
                                gpui::rgb(color).into(),
                                mode,
                                sweep_presence,
                            )),
                    )
                });

            v_flex()
                // 外层必须先有确定的宽度：`composer` 自己既是 `w_full` 又有
                // `max_w`，若父节点按内容收缩，IME 组合文本触发重测量时会让
                // `w_full` 在不同帧解析成不同宽度，导致输入框从居中跳到左侧。
                .relative()
                .w_full()
                .px_4()
                .py_3()
                .items_center()
                .children(self.render_usage_popover(cx))
                .children(self.render_background_task_popover(cx))
                .children(self.render_subagent_popover(cx))
                .children(completion_bar)
                .child(composer)
        });
        input_row.map(|row| row.into_any_element())
    }

    fn render_usage_popover(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        if !self.usage_popover_open {
            return None;
        }
        let (used, size) = self.usage?;
        let pct = usage_percent(used, size);
        let fill = usage_meter_color(pct);
        let header_tokens = if size == 0 {
            format!("{} Tokens", compact_token_count(used))
        } else {
            format!("{} Tokens", usage_token_header(used, size))
        };
        let mut panel = v_flex()
            .id("acp-usage-popover")
            .debug_selector(|| "ACP_USAGE_POPOVER".into())
            .w_full()
            .max_w(ui_theme::conversation_max_width())
            .px_4()
            .pt_3()
            .pb_3()
            .gap_3()
            .rounded(ui_theme::card_radius())
            .border_1()
            .border_color(ui_theme::card_stroke())
            .bg(ui_theme::glass_floating())
            .shadow_lg()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .text_color(gpui::rgb(ui_theme::text_mid()))
                            .child("Context Usage"),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("acp-usage-close")
                            .dismiss()
                            .icon(IconName::Close)
                            .icon_action("关闭")
                            .on_click(cx.listener(|this, _ev, _window, cx| {
                                this.usage_popover_open = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_end()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .text_color(gpui::rgb(ui_theme::text_bright()))
                            .child(format!("{pct}% Full")),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_sm()
                            .text_color(gpui::rgb(ui_theme::text_muted()))
                            .child(header_tokens),
                    ),
            );
        let legend = composer_usage_breakdown(
            used,
            size,
            self.usage_cached_read,
            self.usage_breakdown.as_ref(),
            fill,
        );
        panel = panel.child(render_usage_stacked_bar(&legend, size));
        for row in legend {
            panel = panel.child(
                h_flex()
                    .id((gpui::ElementId::from("acp-usage-row"), row.label))
                    .w_full()
                    .h(ui_theme::height::md())
                    .px_2()
                    .gap_2()
                    .items_center()
                    .rounded(ui_theme::row_radius())
                    .hover(|d| d.bg(ui_theme::overlay(0x14)))
                    .child(div().size(px(8.)).rounded_sm().bg(gpui::rgb(row.color)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(gpui::rgb(ui_theme::text()))
                            .child(row.label),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_sm()
                            .text_color(gpui::rgb(ui_theme::text_muted()))
                            .child(compact_token_count(row.tokens)),
                    ),
            );
        }
        if let Some(cost) = self.usage_cost.filter(|value| *value > 0.0) {
            panel = panel.child(
                h_flex().w_full().px_2().child(
                    div()
                        .text_xs()
                        .text_color(gpui::rgb(ui_theme::text_faint()))
                        .child(format!("Cost {}", format_cost(cost))),
                ),
            );
        }
        if self.supports_compaction {
            let compact_row = if self.compacting {
                h_flex().w_full().justify_end().child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(ambient_spinner(
                            "acp-usage-compact-spin",
                            gpui::rgb(ui_theme::yellow()).into(),
                            true,
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(gpui::rgb(ui_theme::yellow()))
                                .child("压缩中…"),
                        ),
                )
            } else {
                h_flex().w_full().justify_end().child(
                    Button::new("acp-usage-compact")
                        .normal()
                        .compact()
                        .label("压缩上下文")
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            this.compact_context(cx);
                        })),
                )
            };
            panel = panel.child(compact_row);
        }
        Some(
            div()
                .id("acp-usage-popover-layer")
                .absolute()
                .bottom(gpui::relative(1.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .pb_2()
                .occlude()
                .child(panel)
                .into_any_element(),
        )
    }

    /// 后台任务入口：只在有进程在跑时出现，点开看是哪几个。
    fn render_background_task_chip(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let running = running_background_tasks(&self.background_tasks).len();
        if running == 0 {
            return None;
        }
        let summary = background_task_summary(running);
        Some(
            Button::new("acp-background-tasks")
                .quiet()
                .compact()
                .capsule()
                .selected(self.background_tasks_open)
                .accessibility_label(summary.clone())
                .child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .bg(gpui::rgb(ui_theme::blue())),
                )
                .child(summary)
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.background_tasks_open = !this.background_tasks_open;
                    this.usage_popover_open = false;
                    this.subagents_open = false;
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    /// 只列还在跑的进程；结束的记录留在对话正文里。
    fn render_background_task_popover(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        if !self.background_tasks_open {
            return None;
        }
        let running = running_background_tasks(&self.background_tasks);
        if running.is_empty() {
            return None;
        }
        let mono = smelt_model::font_config::font_family();
        let mut rows = v_flex()
            .id("acp-background-task-list")
            .max_h(px(320.))
            .overflow_y_scroll()
            .gap_1();
        let now_ms = now_unix_ms();
        for task in running {
            let id = task.id.clone();
            let stop_id = task.id.clone();
            let stopping = self.stopping_background_tasks.contains(&task.id);
            let title_line = background_task_one_line(&task.title);
            let mut row = v_flex()
                .id((gpui::ElementId::from("acp-background-task"), id.clone()))
                .role(gpui::Role::Button)
                .aria_label(title_line.clone())
                .gap_1()
                .px_2()
                .py_1p5()
                .rounded(ui_theme::row_radius())
                
                .hover(|row| row.bg(ui_theme::overlay(0x14)))
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    if !this.expanded_background_tasks.remove(&id) {
                        this.expanded_background_tasks.insert(id.clone());
                    }
                    cx.notify();
                }))
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(gpui::rgb(ui_theme::blue()))
                                .child(task.status.label()),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(gpui::rgb(ui_theme::text_faint()))
                                .child(task.id.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .text_color(gpui::rgb(ui_theme::text_bright()))
                                .child(title_line),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(gpui::rgb(ui_theme::text_faint()))
                                .child(background_task_elapsed(task.started_at_ms, now_ms)),
                        )
                        .child(if stopping {
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(gpui::rgb(ui_theme::text_muted()))
                                .child("正在停止…")
                                .into_any_element()
                        } else {
                            Button::new((
                                gpui::ElementId::from("acp-background-task-stop"),
                                stop_id.clone(),
                            ))
                            .danger()
                            .icon_size()
                            .label("停止")
                            .on_click(cx.listener(move |this, _ev, _window, cx| {
                                // 不要顺带展开或收起这一行的输出。
                                cx.stop_propagation();
                                this.stop_background_task(stop_id.clone(), cx);
                            }))
                            .into_any_element()
                        }),
                )
                .child(
                    div()
                        .pl(px(4.))
                        .text_xs()
                        .truncate()
                        .font_family(mono.clone())
                        .text_color(gpui::rgb(ui_theme::text_muted()))
                        .child(background_task_one_line(&task.command)),
                );
            if self.expanded_background_tasks.contains(&task.id) && !task.output.is_empty() {
                let mut output = v_flex()
                    .pl(px(4.))
                    .font_family(mono.clone())
                    .text_xs()
                    .text_color(gpui::rgb(ui_theme::text_mid()));
                for line in background_task_output_lines(&task.output) {
                    output = output.child(div().truncate().child(line.to_string()));
                }
                row = row.child(output);
            }
            rows = rows.child(row);
        }
        let panel = v_flex()
            .id("acp-background-task-popover")
            .w_full()
            .max_w(ui_theme::conversation_max_width())
            .px_4()
            .pt_3()
            .pb_3()
            .gap_2()
            .rounded(ui_theme::card_radius())
            .border_1()
            .border_color(ui_theme::card_stroke())
            .bg(ui_theme::glass_floating())
            .shadow_lg()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .text_color(gpui::rgb(ui_theme::text_mid()))
                            .child("后台任务"),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("acp-background-task-close")
                            .dismiss()
                            .icon(IconName::Close)
                            .icon_action("关闭")
                            .on_click(cx.listener(|this, _ev, _window, cx| {
                                this.background_tasks_open = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(rows);
        Some(
            div()
                .id("acp-background-task-popover-layer")
                .absolute()
                .bottom(gpui::relative(1.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .pb_2()
                .occlude()
                .child(panel)
                .into_any_element(),
        )
    }
}
