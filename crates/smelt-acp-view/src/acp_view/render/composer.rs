//! ACP 输入栏：补全、模型选择、发送。

use super::*;
use smelt_core::daemon_state::DaemonPhase;

impl AcpView {
    pub(super) fn render_composer(
        &self,
        focused: bool,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if matches!(
            &self.conversation_binding,
            smelt_core::conversation::ConversationBinding::Automation { .. }
        ) {
            return None;
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
                    gpui::rgb(ui_theme::text_bright())
                } else {
                    gpui::rgb(ui_theme::text_mid())
                };
                let label = if let Some(range) = item.match_range.clone() {
                    h_flex()
                        .flex_shrink_0()
                        .text_xs()
                        .font_family(smelt_core::font_config::font_family())
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
                        .font_family(smelt_core::font_config::font_family())
                        .text_color(label_color)
                        .child(item.label.clone())
                        .into_any_element()
                };
                list = list.child(
                    h_flex()
                        .id(("acp-completion-item", ix))
                        .px_3()
                        .py_1p5()
                        .gap_2()
                        .items_center()
                        .when(selected, |d| d.bg(ui_theme::tint(ui_theme::accent(), 0x38)))
                        .cursor_pointer()
                        .hover(move |d| {
                            d.bg(if selected {
                                ui_theme::tint(ui_theme::accent(), 0x48)
                            } else {
                                ui_theme::overlay(0x20)
                            })
                        })
                        .child(label)
                        .when(!item.hint.is_empty(), |row| {
                            row.child(
                                div()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(if selected { t.foreground } else { muted })
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
                    .and_then(usage_warn_color)
                    .unwrap_or(ui_theme::accent())
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
                let current_provider_id = current_provider.as_ref().map(|p| p.id.clone());
                let current_model_name = model_state
                    .as_ref()
                    .map(|model| model.current_name.clone())
                    .unwrap_or_default();
                let grouped = grouped_models.is_some();
                Button::new("acp-model-pill")
                    .ghost()
                    .small()
                    .rounded(ButtonRounded::Size(px(16.)))
                    .dropdown_caret(true)
                    .label(model_label)
                    .text_color(gpui::rgb(if next_turn_notice.is_some() {
                        ui_theme::yellow()
                    } else {
                        ui_theme::text_mid()
                    }))
                    .hover(|d| d.bg(ui_theme::overlay(0x28)))
                    // 输入栏贴窗口底，默认 TopLeft 会往下弹，长模型列表把配置挤出屏幕。
                    .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, window, cx| {
                        let mut menu = menu;
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
                                        let name = provider.name.clone();
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
                                    let nest = composer_should_nest_models(
                                        extra_configs.len(),
                                        provider_groups.len(),
                                        model_options.len(),
                                    );
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
                                    menu = if nest {
                                        menu.submenu(model_section, window, cx, move |menu, _, _| {
                                            fill(menu)
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
                        menu
                    })
                    .into_any_element()
            } else {
                div()
                    .h(px(24.))
                    .px_2()
                    .rounded(px(16.))
                    .flex()
                    .items_center()
                    .text_sm()
                    .text_color(gpui::rgb(ui_theme::text_mid()))
                    .child(model_label)
                    .into_any_element()
            };
            let skills_control = (self.agent == ConversationAgentKind::Pi).then(|| {
                let skills = smelt_core::pi_plugin_catalog::loaded_skills_for_launch(
                    &self.launch,
                    self.cwd.as_deref().map(std::path::Path::new),
                );
                let count = skills.len();
                Button::new("acp-skills-pill")
                    .ghost()
                    .small()
                    .rounded(ButtonRounded::Size(px(16.)))
                    .dropdown_caret(true)
                    .label(format!("技能 {count}"))
                    .text_color(gpui::rgb(ui_theme::text_mid()))
                    .hover(|d| d.bg(ui_theme::overlay(0x28)))
                    .dropdown_menu_with_anchor(
                        gpui::Anchor::BottomLeft,
                        move |mut menu, _window, _cx| {
                            if skills.is_empty() {
                                return menu.item(PopupMenuItem::label("这场对话没有加载技能"));
                            }
                            menu = menu.item(PopupMenuItem::label("已加载的技能"));
                            for skill in &skills {
                                let label = if skill.description.is_empty() {
                                    skill.name.clone()
                                } else {
                                    let desc = skill
                                        .description
                                        .split_whitespace()
                                        .collect::<Vec<_>>()
                                        .join(" ");
                                    let desc = if desc.chars().count() > 36 {
                                        format!("{}…", desc.chars().take(36).collect::<String>())
                                    } else {
                                        desc
                                    };
                                    format!("{} · {desc}", skill.name)
                                };
                                menu = menu.item(PopupMenuItem::new(label).disabled(true));
                            }
                            menu
                        },
                    )
                    .into_any_element()
            });
            let composer_border = if focused {
                ui_theme::tint(ui_theme::accent(), 0x66)
            } else {
                ui_theme::overlay(0x22)
            };
            let composer = v_flex()
                .w_full()
                .max_w(ui_theme::conversation_max_width())
                .rounded(ui_theme::composer_radius())
                .border_1()
                .border_color(composer_border)
                .bg(ui_theme::glass_input())
                .shadow_sm()
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
                        let immediate_action_available =
                            can_move_next && !self.immediate_cancel_pending;
                        let native_items = self.queued_steering.iter().map(|text| {
                            (native_queue_item_kind_label(false), text.as_str())
                        }).chain(self.queued_follow_up.iter().map(|text| {
                            (native_queue_item_kind_label(true), text.as_str())
                        }));
                        for (ix, (kind, text)) in native_items.enumerate() {
                            let preview: String = text.chars().take(60).collect();
                            let preview = if text.chars().count() > 60 {
                                format!("{preview}…")
                            } else {
                                preview
                            };
                            strip = strip.child(
                                h_flex()
                                    .id(("acp-native-queue-item", ix))
                                    .gap_2()
                                    .items_center()
                                    .px_2p5()
                                    .py_1()
                                    .rounded_md()
                                    .bg(ui_theme::overlay(0x14))
                                    .border_1()
                                    .border_color(t.border)
                                    .child(
                                        Icon::new(IconName::LoaderCircle)
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
                                    .when(self.immediate_cancel_pending && ix == 0, |row| {
                                        row.child(
                                            div()
                                                .text_xs()
                                                .text_color(gpui::rgb(ui_theme::text_muted()))
                                                .child("正在停止当前回答…"),
                                        )
                                    })
                                    .when(immediate_action_available, |row| {
                                        row.child(
                                            div()
                                                .id(("acp-native-queue-immediate", ix))
                                                .text_xs()
                                                .text_color(gpui::rgb(if immediate_cancels_turn {
                                                    ui_theme::yellow()
                                                } else {
                                                    ui_theme::accent()
                                                }))
                                                .cursor_pointer()
                                                .hover(|d| d.opacity(0.8))
                                                .tooltip(move |window, cx| {
                                                    gpui_component::tooltip::Tooltip::new(
                                                        if immediate_cancels_turn {
                                                            "停止当前回答后，立即发送这条消息"
                                                        } else {
                                                            "立即发送这条消息"
                                                        },
                                                    )
                                                    .build(window, cx)
                                                })
                                                .child(if immediate_cancels_turn {
                                                    "立即发送（停止当前回答）"
                                                } else {
                                                    "立即发送"
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
                        if self.supports_native_queue && !self.immediate_cancel_pending {
                            strip = strip.child(
                                div()
                                    .id("acp-native-queue-clear")
                                    .text_xs()
                                    .text_color(gpui::rgb(ui_theme::text_muted()))
                                    .cursor_pointer()
                                    .hover(|d| d.opacity(0.8))
                                    .child("撤回排队")
                                    .on_click(cx.listener(|this, _ev, _window, cx| {
                                        this.clear_native_queue(cx);
                                    })),
                            );
                        }
                        col.child(strip)
                    },
                )
                // 排队消息条：当前 turn 未结束时不会立刻打给 agent，得让人看见
                // 「排队中」，还能撤回，避免误以为消息已丢失。
                .when(!self.queued_prompts.is_empty(), |col| {
                    let mut strip = v_flex().px_4().pt_3().gap_1p5();
                    let can_move_next =
                        !matches!(self.phase, DaemonPhase::Connecting | DaemonPhase::Dead);
                    let immediate_cancels_turn = should_cancel_for_immediate_prompt(
                        &self.phase,
                        self.prompt_dispatch_pending,
                    );
                    let immediate_action_available =
                        can_move_next && !self.immediate_cancel_pending;
                    for (ix, (text, images)) in self.queued_prompts.iter().enumerate() {
                        let preview: String = text.chars().take(60).collect();
                        let preview = if text.chars().count() > 60 {
                            format!("{preview}…")
                        } else {
                            preview
                        };
                        let img_suffix = if images.is_empty() {
                            String::new()
                        } else {
                            format!("（含 {} 张图）", images.len())
                        };
                        strip = strip.child(
                            h_flex()
                                .id(("acp-queued-prompt", ix))
                                .gap_2()
                                .items_center()
                                .px_2p5()
                                .py_1()
                                .rounded_md()
                                .bg(ui_theme::overlay(0x14))
                                .border_1()
                                .border_color(t.border)
                                .child(
                                    Icon::new(IconName::LoaderCircle)
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
                                .when(self.immediate_cancel_pending && ix == 0, |row| {
                                    row.child(
                                        div()
                                            .text_xs()
                                            .text_color(gpui::rgb(ui_theme::text_muted()))
                                            .child("正在停止当前回答…"),
                                    )
                                })
                                .when(immediate_action_available, |row| {
                                    row.child(
                                        div()
                                            .id(("acp-queued-prompt-immediate", ix))
                                            .text_xs()
                                            .text_color(gpui::rgb(if immediate_cancels_turn {
                                                ui_theme::yellow()
                                            } else {
                                                ui_theme::accent()
                                            }))
                                            .cursor_pointer()
                                            .hover(|d| d.opacity(0.8))
                                            .tooltip(move |window, cx| {
                                                gpui_component::tooltip::Tooltip::new(
                                                    if immediate_cancels_turn {
                                                        "停止当前回答后，立即发送这条消息"
                                                    } else {
                                                        "立即发送这条消息"
                                                    },
                                                )
                                                .build(window, cx)
                                            })
                                            .child(if immediate_cancels_turn {
                                                "立即发送（停止当前回答）"
                                            } else {
                                                "立即发送"
                                            })
                                            .on_click(cx.listener(
                                                move |this, _ev, _window, cx| {
                                                    this.send_queued_prompt_immediately(ix, cx);
                                                },
                                            )),
                                    )
                                })
                                .when(
                                    can_move_next && !self.immediate_cancel_pending && ix > 0,
                                    |row| {
                                        row.child(
                                            div()
                                                .id(("acp-queued-prompt-send", ix))
                                                .text_xs()
                                                .text_color(gpui::rgb(ui_theme::accent()))
                                                .cursor_pointer()
                                                .hover(|d| d.opacity(0.8))
                                                .tooltip(|window, cx| {
                                                    gpui_component::tooltip::Tooltip::new(
                                                        "当前回合结束后，优先发送这条消息",
                                                    )
                                                    .build(window, cx)
                                                })
                                                .child("下一条发送")
                                                .on_click(cx.listener(
                                                    move |this, _ev, _window, cx| {
                                                        this.move_queued_prompt_next(ix, cx);
                                                    },
                                                )),
                                        )
                                    },
                                )
                                .child(
                                    div()
                                        .id(("acp-queued-prompt-remove", ix))
                                        .text_xs()
                                        .text_color(gpui::rgb(ui_theme::text_muted()))
                                        .cursor_pointer()
                                        .hover(|d| d.opacity(0.8))
                                        .child("撤回")
                                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                                            if ix < this.queued_prompts.len() {
                                                this.queued_prompts.remove(ix);
                                            }
                                            cx.notify();
                                        })),
                                ),
                        );
                    }
                    col.child(strip)
                })
                // 待发图片的缩略图条：粘完得看得见「贴上了」，还得能反悔。
                .when(!self.pending_images.is_empty(), |col| {
                    let mut strip = h_flex().px_4().pt_3().gap_2().items_center().flex_wrap();
                    for (ix, im) in self.pending_images.iter().enumerate() {
                        let preview_image = im.clone();
                        strip = strip.child(
                            div()
                                .id(("acp-pending-img", ix))
                                .relative()
                                .cursor_pointer()
                                .hover(|d| d.opacity(0.88))
                                .on_click(cx.listener(move |this, _ev, _window, cx| {
                                    let _ = this;
                                    cx.emit(AcpViewEvent::PreviewImage(preview_image.clone()));
                                }))
                                .child(
                                    gpui::img(im.clone())
                                        .h(px(56.))
                                        .max_w(px(96.))
                                        .rounded_md()
                                        .border_1()
                                        .border_color(t.border),
                                )
                                .child(
                                    // 右上角小 ×：点掉这张。
                                    div()
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
                                        .cursor_pointer()
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
                                .children(skills_control)
                                .children(next_turn_notice.as_ref().map(|label| {
                                    div()
                                        .flex_shrink_0()
                                        .text_xs()
                                        .text_color(gpui::rgb(ui_theme::yellow()))
                                        .child(label.clone())
                                }))
                                .child(
                                    div()
                                        .id("acp-trajectory")
                                        .flex_shrink_0()
                                        .size(px(18.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(4.))
                                        .cursor_pointer()
                                        .text_color(gpui::rgb(ui_theme::text_muted()))
                                        .hover(|d| d.bg(ui_theme::overlay(0x22)))
                                        .tooltip(|window, cx| {
                                            gpui_component::tooltip::Tooltip::new("会话追踪")
                                                .build(window, cx)
                                        })
                                        .child(Icon::new(IconName::Inspector).size(px(16.)))
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
                                        .debug_selector(|| "ACP_USAGE_RING".into())
                                        .flex_shrink_0()
                                        .size(px(18.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
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
                        )
                        .children(self.restart_error.as_ref().map(|err| {
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(gpui::rgb(ui_theme::red()))
                                .child(format!("重启失败：{err}"))
                        }))
                        .child(
                            div()
                                .id("acp-attach")
                                .flex_shrink_0()
                                .size(px(32.))
                                .rounded_full()
                                .bg(ui_theme::overlay(0x14))
                                .text_color(gpui::rgb(ui_theme::text_mid()))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_lg()
                                .font_semibold()
                                .cursor_pointer()
                                .hover(|d| d.bg(ui_theme::overlay(0x22)))
                                .tooltip(|window, cx| {
                                    gpui_component::tooltip::Tooltip::new("添加文件")
                                        .build(window, cx)
                                })
                                .child("+")
                                .on_click(cx.listener(|this, _ev, window, cx| {
                                    this.pick_composer_files(window, cx);
                                })),
                        )
                        // 主操作始终一个：跑着是停止，空闲才是发送。排队仍走 Enter。
                        .when(self.is_visibly_running(), |row| {
                            row.child(
                                div()
                                    .id("acp-stop")
                                    .flex_shrink_0()
                                    .h(px(32.))
                                    .px_3()
                                    .rounded(ui_theme::row_radius())
                                    .bg(ui_theme::tint(ui_theme::red(), 0x28))
                                    .flex()
                                    .items_center()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(gpui::rgb(ui_theme::red()))
                                    .cursor_pointer()
                                    .hover(|d| d.opacity(0.85))
                                    .child("停止")
                                    .on_click(
                                        cx.listener(|this, _ev, _window, _cx| this.cancel_turn()),
                                    ),
                            )
                        })
                        .when(!self.is_visibly_running(), |row| {
                            let can_send =
                                self.input_has_draft || !self.pending_images.is_empty();
                            row.child(
                                // Grok 式发送：圆钮 + 上箭头。有内容才亮强调蓝，空着是灰底。
                                div()
                                    .id("acp-send")
                                    .flex_shrink_0()
                                    .size(px(32.))
                                    .rounded_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_sm()
                                    .font_semibold()
                                    .when(can_send, |d| {
                                        d.bg(gpui::rgb(ui_theme::accent()))
                                            .text_color(gpui::rgb(ui_theme::on_accent()))
                                            .cursor_pointer()
                                            .hover(|d| d.opacity(0.88))
                                    })
                                    .when(!can_send, |d| {
                                        d.bg(ui_theme::overlay(0x18))
                                            .text_color(gpui::rgb(ui_theme::text_faint()))
                                    })
                                    .child("↑")
                                    .on_click(cx.listener(|this, _ev, window, cx| {
                                        this.submit_input(window, false, cx);
                                    })),
                            )
                        }),
                );

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
        let t = cx.theme();
        let muted = t.muted_foreground;
        let pct = usage_percent(used, size);
        let fill = usage_warn_color(pct).unwrap_or(ui_theme::accent());
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
                        div()
                            .id("acp-usage-close")
                            .size(px(24.))
                            .rounded_md()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(muted)
                            .cursor_pointer()
                            .hover(|d| d.bg(ui_theme::overlay(0x20)))
                            .child(Icon::new(IconName::Close).size(px(14.)))
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
        for (i, row) in legend.into_iter().enumerate() {
            panel = panel.child(
                h_flex()
                    .id(("acp-usage-row", i))
                    .w_full()
                    .h(px(32.))
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
                    div()
                        .id("acp-usage-compact")
                        .h(px(28.))
                        .px_3()
                        .rounded(ui_theme::row_radius())
                        .flex()
                        .items_center()
                        .text_xs()
                        .font_medium()
                        .bg(ui_theme::overlay(0x18))
                        .text_color(gpui::rgb(ui_theme::text_mid()))
                        .cursor_pointer()
                        .hover(|d| d.bg(ui_theme::overlay(0x28)))
                        .child("压缩上下文")
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
}
