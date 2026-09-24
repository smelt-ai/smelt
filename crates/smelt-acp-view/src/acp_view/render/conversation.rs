//! ACP 消息流虚拟列表。

use super::*;

const USER_MESSAGE_ACTION_GROUP: &str = "acp-user-message-actions";

impl AcpView {
    pub(super) fn render_message_list(
        &self,
        animate_ambient: bool,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let current_turn_active = self.has_active_turn();
        let conversation_layout = std::rc::Rc::new(build_conversation_layout_with_timings(
            &self.entries,
            current_turn_active,
            self.loaded_entries_offset,
            &self.turn_timings,
            live_elapsed_ms(self.turn_started_at_ms),
        ));
        // These values are stable for the whole render pass. Computing them once keeps the
        // virtual-list item builder from rescanning the full conversation for every row.
        let active_permission_tool_id = self
            .permissions
            .first()
            .map(|card| card.tool_call_id.clone());
        let latest_user_message_index = super::super::latest_user_message_index(&self.entries);
        let view = cx.entity();
        let list = virtual_list(self.list_state.clone(), move |i, _window, app| {
            let conversation_layout = conversation_layout.clone();
            view.update(app, |this, cx| {
                let t = cx.theme();
                let muted = t.muted_foreground;
                let presentation = conversation_layout.get(i).copied().unwrap_or_default();
                let final_answer = presentation.final_answer;
                let process_group = presentation.process_group;
                let process_expanded = process_group.is_some_and(|group| {
                    group.active || this.expanded_process_groups.contains(&group.first)
                });
                if let Some(group) = process_group
                    && !process_expanded
                {
                    if group.first != i {
                        // 高度必须是 0：空 div 若测出残高，虚拟列表会把回答叠在标题上。
                        return div().h(px(0.)).w_full().overflow_hidden().into_any_element();
                    }
                    // 折叠态只构建摘要，不先渲染一遍随后会被丢弃的首个工具卡。
                    // Read 的大输出或 Edit 的 diff 因此不会在静态重绘时白做解析。
                    return h_flex()
                        .w_full()
                        .justify_center()
                        .px_4()
                        .pb(px(12.))
                        .overflow_hidden()
                        .child(
                            div()
                                .w_full()
                                .max_w(ui_theme::conversation_max_width())
                                .child(this.render_process_group_header(
                                    group,
                                    false,
                                    animate_ambient,
                                    muted,
                                    cx,
                                )),
                        )
                        .into_any_element();
                }
                let should_cache_tool_diff = match this.entries.get(i) {
                    Some(AcpEntry::ToolCall {
                        id,
                        status,
                        output,
                        children,
                        ..
                    }) => {
                        let has_pending_permission =
                            active_permission_tool_id.as_deref() == Some(id.as_str());
                        let card_expanded = this.tool_card_is_expanded(
                            id,
                            tool_output_has_content(output) || !children.is_empty(),
                            tool_has_live_children(children),
                        );
                        let compact_in_process = process_expanded
                            && process_group.is_some()
                            && !card_expanded
                            && tool_uses_compact_process_row(*status, has_pending_permission);
                        // Compact Edit rows still need the cached diff totals for the inline
                        // "+N -M" summary; text-only rows can keep the lazy path.
                        !compact_in_process || tool_output_has_diff(output)
                    }
                    _ => false,
                };
                if should_cache_tool_diff {
                    this.ensure_diff_cache_for_entry(i);
                }
                this.ensure_tool_image_cache_for_entry(i);
                let tool_run = process_expanded
                    .then_some(process_group)
                    .flatten()
                    .and_then(|group| {
                        consecutive_compact_tool_run(
                            &this.entries,
                            i,
                            group.first,
                            group.end,
                            active_permission_tool_id.as_deref(),
                        )
                    });
                if let Some(run) = tool_run.as_ref()
                    && i != run.start
                    && let Some(AcpEntry::ToolCall {
                        id,
                        status,
                        output,
                        children,
                        ..
                    }) = this.entries.get(i)
                {
                    let has_pending_permission =
                        active_permission_tool_id.as_deref() == Some(id.as_str());
                    let card_expanded = this.tool_card_is_expanded(
                        id,
                        tool_output_has_content(output) || !children.is_empty(),
                        tool_has_live_children(children),
                    );
                    if !card_expanded
                        && tool_uses_compact_process_row(*status, has_pending_permission)
                    {
                        return div().into_any_element();
                    }
                }
                let entry = &this.entries[i];
                let mut el: gpui::AnyElement = match entry {
                    // agent 回显的「中断」标记不是用户说的话，别套成气泡——
                    // 那会读成「用户发了一条叫 [Request interrupted...] 的消息」。
                    AcpEntry::User(text) if is_interrupt_marker(text) => h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .my_1()
                        .child(div().flex_1().h(px(1.)).bg(t.border))
                        .child(div().text_xs().text_color(muted).child("已中断"))
                        .child(div().flex_1().h(px(1.)).bg(t.border))
                        .into_any_element(),
                    // 用户气泡右对齐限宽（对齐设计稿）：整行铺满时跟 agent 正文
                    // 混成一片，看不出谁在说话。
                    AcpEntry::User(_) => {
                        let task_view = view.clone();
                        let task_cwd = this.cwd.clone();
                        let can_edit = this.supports_rewind && !this.has_active_turn();
                        let show_edit = latest_user_message_index == Some(i) && can_edit;
                        let show_fork_edit = latest_user_message_index != Some(i) && can_edit;
                        h_flex()
                            .group(USER_MESSAGE_ACTION_GROUP)
                            .w_full()
                            .justify_end()
                            .gap_2()
                            .when(show_fork_edit, |row| {
                                row.child(
                                    Button::new(("acp-fork-edit-message", i))
                                        .icon(Icon::empty().path("smelt-icons/git-branch.svg"))
                                        .ghost()
                                        .xsmall()
                                        .opacity(0.)
                                        .group_hover(USER_MESSAGE_ACTION_GROUP, |style| {
                                            style.opacity(1.)
                                        })
                                        .tooltip("保留原对话，从这条之前分叉并编辑重发")
                                        .on_click(cx.listener(
                                            move |this, _ev, _window, cx| {
                                                this.fork_from_user_message(i, cx);
                                            },
                                        )),
                                )
                            })
                            .when(show_edit, |row| {
                                row.child(
                                    Button::new(("acp-edit-message", i))
                                        .icon(Icon::new(IconName::Undo2))
                                        .ghost()
                                        .xsmall()
                                        .opacity(0.)
                                        .group_hover(USER_MESSAGE_ACTION_GROUP, |style| {
                                            style.opacity(1.)
                                        })
                                        .tooltip("编辑并重发最后一条消息")
                                        .on_click(cx.listener(
                                            move |this, _ev, _window, cx| {
                                                this.rewind_to_message(i, cx);
                                            },
                                        )),
                                )
                            })
                            .child(
                                div()
                                    .id(("acp-user-message", i))
                                    .max_w(gpui::relative(0.72))
                                // gpui-component 的 markdown 列表块（ol/ul）内部用
                                // `w_full()`/`flex_1()` 排布"序号 + 正文"。这个气泡
                                // 是收缩到内容大小（只有 max_w，没有 width）的 flex
                                // item，短列表内容会被误测成只有序号那么宽，正文被
                                // `overflow_hidden()` 悄悄裁掉——只剩"1." "2." 悬浮。
                                // 兜个最小宽度，给列表正文留出可见空间。
                                    .min_w(gpui::px(160.))
                                    .px_4()
                                    .py_2p5()
                                    .rounded(px(20.))
                                    .bg(ui_theme::overlay(0x14))
                                    .hover(|bubble| bubble.bg(ui_theme::overlay(0x1c)))
                                    .text_sm()
                                    .child(smelt_ui::markdown_mermaid::markdown_view_clickable(
                                        ("acp-user-md", i),
                                        cached_entry_markdown(
                                            &this.rendered_markdown,
                                            i,
                                            entry,
                                            this.cwd.as_deref(),
                                        ),
                                    ))
                                    .context_menu(move |menu, window, cx| {
                                        selected_text_context_menu(
                                            menu,
                                            task_view.clone(),
                                            task_cwd.clone(),
                                            window,
                                            cx,
                                        )
                                    }),
                            )
                            .into_any_element()
                    }
                    AcpEntry::UserWithImages { text, images } => {
                        let task_view = view.clone();
                        let task_cwd = this.cwd.clone();
                        let can_edit = this.supports_rewind && !this.has_active_turn();
                        let show_edit = latest_user_message_index == Some(i) && can_edit;
                        let show_fork_edit = latest_user_message_index != Some(i) && can_edit;
                        let mut content = v_flex().gap_2();
                        if !text.trim().is_empty() {
                            content = content.child(
                                smelt_ui::markdown_mermaid::markdown_view_clickable(
                                    ("acp-user-images-md", i),
                                    cached_entry_markdown(
                                        &this.rendered_markdown,
                                        i,
                                        entry,
                                        this.cwd.as_deref(),
                                    ),
                                ),
                            );
                        }
                        let mut image_strip = h_flex().gap_2().flex_wrap();
                        for image_ix in 0..images.len() {
                            if let Some(image) = this.rendered_images.get(&(i, image_ix)).cloned() {
                                image_strip = image_strip.child(render_clickable_image_thumb(
                                    image,
                                    ("acp-sent-image", i * 1024 + image_ix),
                                    t.border,
                                    cx,
                                ));
                            }
                        }
                        h_flex()
                            .w_full()
                            .justify_end()
                            .gap_2()
                            .group(USER_MESSAGE_ACTION_GROUP)
                            .when(show_fork_edit, |row| {
                                row.child(
                                    Button::new(("acp-fork-edit-message", i))
                                        .icon(Icon::empty().path("smelt-icons/git-branch.svg"))
                                        .ghost()
                                        .xsmall()
                                        .opacity(0.)
                                        .group_hover(USER_MESSAGE_ACTION_GROUP, |style| {
                                            style.opacity(1.)
                                        })
                                        .tooltip("保留原对话，从这条之前分叉并编辑重发（图片需重新添加）")
                                        .on_click(cx.listener(
                                            move |this, _ev, _window, cx| {
                                                this.fork_from_user_message(i, cx);
                                            },
                                        )),
                                )
                            })
                            .when(show_edit, |row| {
                                    row.child(
                                        Button::new(("acp-edit-message", i))
                                            .icon(Icon::new(IconName::Undo2))
                                            .ghost()
                                            .xsmall()
                                            .opacity(0.)
                                            .group_hover(USER_MESSAGE_ACTION_GROUP, |style| {
                                                style.opacity(1.)
                                            })
                                            .tooltip("编辑并重发最后一条消息（图片需重新添加）")
                                            .on_click(cx.listener(
                                                move |this, _ev, _window, cx| {
                                                    this.rewind_to_message(i, cx);
                                                },
                                            )),
                                    )
                                },
                            )
                            .child(
                                div()
                                    .id(("acp-user-images-message", i))
                                    .max_w(gpui::relative(0.8))
                                    // 同上：避免短的有序/无序列表被收缩到只剩序号宽度、
                                    // 正文被裁没。
                                    .min_w(gpui::px(160.))
                                    .px_3()
                                    .py_3()
                                    .rounded(px(20.))
                                    .bg(ui_theme::overlay(0x14))
                                    .hover(|bubble| bubble.bg(ui_theme::overlay(0x1c)))
                                    .text_sm()
                                    .child(content.child(image_strip))
                                    .context_menu(move |menu, window, cx| {
                                        selected_text_context_menu(
                                            menu,
                                            task_view.clone(),
                                            task_cwd.clone(),
                                            window,
                                            cx,
                                        )
                                    }),
                            )
                            .into_any_element()
                    }
                    // 过程组里的思考和中间正文都走进展行：一行摘要，长文可展开。
                    AcpEntry::Assistant { text, .. }
                        if process_group.is_some() && text.trim().is_empty() =>
                    {
                        div().h(px(0.)).w_full().overflow_hidden().into_any_element()
                    }
                    AcpEntry::Assistant { text, .. } if process_group.is_some() => this
                        .render_progress_entry(i, entry, text, true, muted, cx),
                    AcpEntry::Assistant {
                        text,
                        thought: true,
                    } => this.render_progress_entry(i, entry, text, false, muted, cx),
                    AcpEntry::Assistant {
                        text,
                        thought: false,
                    } => {
                        let task_view = view.clone();
                        let task_cwd = this.cwd.clone();
                        let answer = v_flex()
                            .id(("acp-assistant-message", i))
                            .w_full()
                            .min_w_0()
                            .text_sm()
                            .text_color(t.foreground)
                            .child(smelt_ui::markdown_mermaid::markdown_view_clickable(
                                ("acp-md", i),
                                cached_entry_markdown(
                                    &this.rendered_markdown,
                                    i,
                                    entry,
                                    this.cwd.as_deref(),
                                ),
                            ))
                            .when(final_answer, |col| {
                                col.child(
                                    h_flex()
                                        .pt_1()
                                        .gap_1()
                                        .child(
                                            Clipboard::new(("acp-copy-answer", i))
                                                .value(text.clone())
                                                .tooltip("复制回答"),
                                        )
                                        .when(
                                            smelt_core::session_handoff::live_fork_is_available(
                                                this.agent,
                                            ) && this.acp_session_id.is_some(),
                                            |row| {
                                                row.child(
                                                    Button::new(("acp-fork-answer", i))
                                                        .icon(
                                                            Icon::empty()
                                                                .path("smelt-icons/git-branch.svg"),
                                                        )
                                                        .ghost()
                                                        .xsmall()
                                                        .tooltip("分叉对话")
                                                        .on_click(cx.listener(
                                                            move |this, _ev, _window, cx| {
                                                                this.fork_from_answer(i, cx);
                                                            },
                                                        )),
                                                )
                                            },
                                        ),
                                )
                            })
                            .when(!final_answer, |col| col.text_color(muted).text_xs())
                            .context_menu(move |menu, window, cx| {
                                selected_text_context_menu(
                                    menu,
                                    task_view.clone(),
                                    task_cwd.clone(),
                                    window,
                                    cx,
                                )
                            });
                        answer.into_any_element()
                    }
                    AcpEntry::ToolCall {
                        title,
                        status,
                        output,
                        ..
                    } if is_task_completion_tool_title(title) => {
                        let task_view = view.clone();
                        let task_cwd = this.cwd.clone();
                        let (status_label, status_color) = match status {
                            ToolCallStatus::Pending => {
                                ("等待完成", gpui::rgb(ui_theme::text_muted()))
                            }
                            ToolCallStatus::InProgress => {
                                ("正在完成", gpui::rgb(ui_theme::blue()))
                            }
                            ToolCallStatus::Completed => {
                                ("完成", gpui::rgb(ui_theme::green()))
                            }
                            ToolCallStatus::Failed => {
                                ("完成失败", gpui::rgb(ui_theme::red()))
                            }
                        };
                        let summary = completion_summary_text(output);
                        let has_summary = !summary.trim().is_empty();
                        let body = if has_summary {
                            summary
                        } else {
                            "任务完成".to_string()
                        };
                        let body_markdown = if !has_summary {
                            markdown_text_for_cwd(&body, this.cwd.as_deref()).into()
                        } else {
                            cached_entry_markdown(
                                &this.rendered_markdown,
                                i,
                                entry,
                                this.cwd.as_deref(),
                            )
                        };
                        let mut answer = v_flex()
                            .id(("acp-completion-message", i))
                            .w_full()
                            .min_w_0()
                            .gap_1()
                            .text_sm()
                            .when(!final_answer, |col| col.text_color(muted))
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Icon::new(IconName::Check)
                                            .size(px(14.))
                                            .text_color(status_color),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_medium()
                                            .text_color(status_color)
                                            .child(status_label),
                                    ),
                            )
                            .child(smelt_ui::markdown_mermaid::markdown_view_clickable(
                                ("acp-completion-md", i),
                                body_markdown,
                            ))
                            .context_menu(move |menu, window, cx| {
                                selected_text_context_menu(
                                    menu,
                                    task_view.clone(),
                                    task_cwd.clone(),
                                    window,
                                    cx,
                                )
                            });
                        if final_answer {
                            answer = answer.child(
                                h_flex()
                                    .pt_1()
                                    .gap_1()
                                    .child(
                                        Clipboard::new(("acp-copy-completion", i))
                                            .value(body)
                                            .tooltip("复制完成摘要"),
                                    ),
                            );
                        }
                        answer.into_any_element()
                    }
                    AcpEntry::ToolCall {
                        id,
                        title,
                        kind,
                        status,
                        output,
                        children,
                    } => {
                        let failed = matches!(status, ToolCallStatus::Failed);
                        let status_label: &str = match status {
                            ToolCallStatus::Pending => "待执行",
                            ToolCallStatus::InProgress => "执行中",
                            ToolCallStatus::Completed => "完成",
                            ToolCallStatus::Failed => "失败",
                        };

                        let has_pending_permission =
                            active_permission_tool_id.as_deref() == Some(id.as_str());
                        let has_nested_transcript = !children.is_empty();
                        let is_subagent =
                            matches!(kind, ToolKind::Collaborate) || has_nested_transcript;
                        let has_expandable_content = is_subagent
                            || tool_output_has_content(output)
                            || has_nested_transcript;
                        let default_expanded = tool_has_live_children(children)
                            || (is_subagent
                                && matches!(
                                    status,
                                    ToolCallStatus::Pending | ToolCallStatus::InProgress
                                ));
                        let card_expanded =
                            this.tool_card_is_expanded(id, has_expandable_content, default_expanded);
                        let compact_in_process = process_expanded
                            && process_group.is_some()
                            && !card_expanded
                            && !is_subagent
                            && tool_uses_compact_process_row(*status, has_pending_permission);

                        // 已完成工具在展开的过程组里占绝大多数。提前返回紧凑行，
                        // 只保留必要的 diff 统计，不构建随后会被丢弃的完整卡片。
                        // 连续同类工具由外层收成「搜索了 N 次」，起点占位给合并行。
                        if compact_in_process
                            && tool_run.as_ref().is_some_and(|run| run.start == i)
                        {
                            div().w_full().into_any_element()
                        } else if compact_in_process {
                            let can_expand = has_expandable_content;
                            let diff_stats = cached_diff_stats(
                                this.rendered_diffs.get(id).map(|parts| parts.as_slice()),
                            )
                            .or_else(|| diff_stats_for_output(output));
                            let faint = gpui::rgb(ui_theme::text_faint());
                            let ink = if failed {
                                gpui::rgb(ui_theme::red()).into()
                            } else {
                                muted
                            };
                            let mut row = h_flex()
                                .id(("acp-tool-compact", i))
                                .debug_selector(move || format!("ACP_TOOL_COMPACT_{i}"))
                                .w_full()
                                .min_h(px(22.))
                                .gap_2()
                                .items_center()
                                .when(can_expand, |row| row.cursor_pointer())
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_color(ink)
                                        .when(can_expand, |icon| {
                                            icon.group_hover(PEEK_ROW_GROUP, |s| {
                                                s.text_color(gpui::rgb(ui_theme::text_bright()))
                                            })
                                        })
                                        .child(
                                            Icon::new(process_timeline_icon(kind)).size(px(13.)),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(ink)
                                        .when(can_expand, |label| {
                                            label.group_hover(PEEK_ROW_GROUP, |s| {
                                                s.text_color(gpui::rgb(ui_theme::text_bright()))
                                            })
                                        })
                                        .child(compact_tool_headline(*kind, title)),
                                );
                            if failed {
                                row = row.child(
                                    div()
                                        .flex_shrink_0()
                                        .text_xs()
                                        .text_color(gpui::rgb(ui_theme::red()))
                                        .child("失败"),
                                );
                            } else if let Some((added, removed)) = diff_stats {
                                row = row.child(render_compact_diff_stats(added, removed));
                            } else if let Some(summary) =
                                tool_result_summary(*kind, *status, output)
                            {
                                row = row.child(
                                    div()
                                        .flex_shrink_0()
                                        .text_xs()
                                        .text_color(faint)
                                        .child(summary),
                                );
                            }
                            if can_expand {
                                this.tool_output_popover(
                                    i,
                                    muted,
                                    Button::new(("acp-tool-compact-trigger", i))
                                        .text()
                                        .w_full()
                                        .justify_start()
                                        .group(PEEK_ROW_GROUP)
                                        .child(row),
                                )
                                .into_any_element()
                            } else {
                                row.into_any_element()
                            }
                        } else if is_subagent {
                            this.render_subagent_card(
                                i,
                                id,
                                title,
                                *status,
                                output,
                                children,
                                has_expandable_content,
                                default_expanded,
                                card_expanded,
                                failed,
                                animate_ambient,
                                muted,
                                t.border,
                                cx,
                            )
                        } else {
                            let faint = gpui::rgb(ui_theme::text_faint());
                            let ink = if failed {
                                gpui::rgb(ui_theme::red()).into()
                            } else {
                                muted
                            };
                            let diff_stats = cached_diff_stats(
                                this.rendered_diffs.get(id).map(|parts| parts.as_slice()),
                            )
                            .or_else(|| diff_stats_for_output(output));
                            let header_right: gpui::AnyElement = if matches!(
                                status,
                                ToolCallStatus::InProgress
                            ) {
                                if animate_ambient {
                                    ambient_spinner(
                                        ("acp-tool-status-spin", i),
                                        muted,
                                        animate_ambient,
                                    )
                                } else {
                                    div()
                                        .text_xs()
                                        .text_color(faint)
                                        .child(status_label)
                                        .into_any_element()
                                }
                            } else if failed {
                                div()
                                    .text_xs()
                                    .text_color(gpui::rgb(ui_theme::red()))
                                    .child(status_label)
                                    .into_any_element()
                            } else if let Some((total_added, total_removed)) = diff_stats {
                                render_compact_diff_stats(total_added, total_removed)
                            } else if let Some(summary) =
                                tool_result_summary(*kind, *status, output)
                            {
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(faint)
                                    .child(summary)
                                    .into_any_element()
                            } else if matches!(status, ToolCallStatus::Pending) {
                                div()
                                    .text_xs()
                                    .text_color(faint)
                                    .child(status_label)
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            };

                        let mut card = v_flex().w_full();
                        let mut header = h_flex()
                            .id(("acp-tool-card-toggle", i))
                            .w_full()
                            .min_h(px(22.))
                            .gap_2()
                            .items_center();
                        if has_expandable_content {
                            let id_for_toggle = id.clone();
                            header = header.group(EXPAND_ICON_GROUP).cursor_pointer().on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _ev, _window, cx| {
                                    this.toggle_tool_card(
                                        i,
                                        id_for_toggle.clone(),
                                        has_expandable_content,
                                        default_expanded,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                }),
                            );
                        }
                        let header = header
                            .child(if has_expandable_content {
                                expandable_lead_icon(
                                    process_timeline_icon(kind),
                                    card_expanded,
                                    ink,
                                )
                            } else {
                                Icon::new(process_timeline_icon(kind))
                                    .size(px(13.))
                                    .text_color(ink)
                                    .into_any_element()
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(ink)
                                    .child(compact_tool_headline(*kind, title)),
                            )
                            .child(header_right);
                        card = card.child(header);
                        if card_expanded {
                            for (part_ix, part) in output.iter().enumerate() {
                                card = match part {
                                    ToolOutputPart::Diff { path, .. } => {
                                        let cached = this
                                            .rendered_diffs
                                            .get(id)
                                            .and_then(|parts| parts.get(part_ix))
                                            .and_then(Option::as_ref);
                                        card.child(
                                            v_flex()
                                                .pl_5()
                                                .pt_1()
                                                .pb_1()
                                                .gap_1()
                                                .child(
                                                    selectable_plain_text(
                                                        ("acp-diff-path", i * 100 + part_ix),
                                                        path,
                                                    )
                                                        .text_xs()
                                                        .text_color(muted)
                                                )
                                                .children(cached.map(|diff| {
                                                    render_diff_lines(
                                                        &diff.lines,
                                                        (i, part_ix),
                                                        t.border,
                                                        t.muted_foreground,
                                                    )
                                                })),
                                        )
                                    }
                                    ToolOutputPart::Text(text) if !text.trim().is_empty() => {
                                        // adapter 把工具输出包在 markdown 围栏里（```console…```），
                                        // 当纯文本渲染会把 ``` 直接显示出来。剥掉再展示。
                                        let body = strip_code_fence(text);
                                        let lines: Vec<&str> = body.lines().collect();
                                        let total = lines.len();
                                        let key = id.to_string();
                                        let expanded = this.expanded_tools.contains(&key);
                                        // 默认只出前 8 行：以前是 max_h + overflow_hidden，
                                        // 内容被硬切掉且没有任何展开入口，等于看不到全部。
                                        let shown =
                                            if expanded || total <= TOOL_OUTPUT_PREVIEW_LINES {
                                                body.to_string()
                                            } else {
                                                lines[..TOOL_OUTPUT_PREVIEW_LINES].join("\n")
                                            };
                                        let need_toggle = total > TOOL_OUTPUT_PREVIEW_LINES;
                                        // 真正的控制台输出（bash stdout、文件内容……）保持等宽纯文本，
                                        // 星号、井号都是内容本身，不能被当 markdown 解析。其他
                                        // 自由格式工具输出可能本来就是按 markdown 写的
                                        // （`##`/`**`/列表），`Other` 工具要保留这种格式。
                                        let body_el: gpui::AnyElement =
                                            if matches!(kind, ToolKind::Other) {
                                                smelt_ui::markdown_mermaid::markdown_view_clickable(
                                                    ("acp-tool-output-md", i * 100 + part_ix),
                                                    shown,
                                                )
                                                .text_xs()
                                                .text_color(muted)
                                                .into_any_element()
                                            } else {
                                                selectable_plain_text(
                                                    ("acp-tool-output-text", i * 100 + part_ix),
                                                    &shown,
                                                )
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .font_family(smelt_core::font_config::font_family())
                                                    .into_any_element()
                                            };
                                        card.child(
                                            v_flex()
                                                .pl_5()
                                                .pt_1()
                                                .pb_1()
                                                .gap_1()
                                                .child(body_el)
                                                .when(need_toggle, |d| {
                                                    let key = key.clone();
                                                    d.child(
                                                    div()
                                                        .id(("acp-tool-toggle", i * 100 + part_ix))
                                                        .text_xs()
                                                        .text_color(faint)
                                                        .cursor_pointer()
                                                        .hover(|d| d.opacity(0.8))
                                                        .child(if expanded {
                                                            "收起".to_string()
                                                        } else {
                                                            format!("展开全部 {total} 行")
                                                        })
                                                        .on_mouse_down(
                                                            gpui::MouseButton::Left,
                                                            cx.listener(
                                                                move |this, _ev, _window, cx| {
                                                                    if !this
                                                                        .expanded_tools
                                                                        .remove(&key)
                                                                    {
                                                                        this.expanded_tools
                                                                            .insert(key.clone());
                                                                    }
                                                                    this.list_state
                                                                        .remeasure_items(i..i.saturating_add(1));
                                                                    cx.stop_propagation();
                                                                    cx.notify();
                                                                },
                                                            ),
                                                        ),
                                                )
                                                }),
                                        )
                                    }
                                    ToolOutputPart::Image(_) => {
                                        let decoded = this
                                            .rendered_tool_images
                                            .get(id)
                                            .and_then(|parts| parts.get(part_ix))
                                            .and_then(Option::as_ref)
                                            .cloned();
                                        match decoded {
                                            Some(image) => card.child(
                                                div().pl_5().pt_1().pb_1().child(
                                                    render_clickable_image_thumb(
                                                        image,
                                                        ("acp-tool-image", i * 100 + part_ix),
                                                        t.border,
                                                        cx,
                                                    ),
                                                ),
                                            ),
                                            None => card,
                                        }
                                    }
                                    ToolOutputPart::Text(_) => card,
                                    ToolOutputPart::Terminal { output, .. }
                                        if !output.trim().is_empty() =>
                                    {
                                        card.child(
                                            v_flex().pl_5().pt_1().pb_1().child(
                                                selectable_plain_text(
                                                    ("acp-tool-output-terminal", i * 100 + part_ix),
                                                    output,
                                                )
                                                .text_xs()
                                                .text_color(muted)
                                                .font_family(smelt_core::font_config::font_family()),
                                            ),
                                        )
                                    }
                                    ToolOutputPart::Terminal { .. } => card,
                                };
                            }
                            if !children.is_empty() {
                                card = card.child(render_nested_transcript(children, muted));
                            }
                        }
                        card.into_any_element()
                        }
                    }
                    AcpEntry::Divider(label) => h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .my_1()
                        .child(div().flex_1().h(px(1.)).bg(t.border))
                        .child(div().text_xs().text_color(muted).child(label.clone()))
                        .child(div().flex_1().h(px(1.)).bg(t.border))
                        .into_any_element(),
                };
                if let Some(run) = tool_run.as_ref()
                    && run.start == i
                {
                    let start_has_card = match this.entries.get(i) {
                        Some(AcpEntry::ToolCall {
                            id,
                            status,
                            output,
                            children,
                            ..
                        }) => {
                            let pending =
                                active_permission_tool_id.as_deref() == Some(id.as_str());
                            this.tool_card_is_expanded(
                                id,
                                tool_output_has_content(output) || !children.is_empty(),
                                tool_has_live_children(children),
                            ) || !tool_uses_compact_process_row(*status, pending)
                        }
                        _ => true,
                    };
                    let mut col = v_flex()
                        .w_full()
                        .gap_1()
                        .child(this.render_compact_tool_run(run, muted, cx));
                    if start_has_card {
                        col = col.child(el);
                    }
                    el = col.into_any_element();
                }
                let in_process_timeline = process_expanded && process_group.is_some();
                if in_process_timeline {
                    el = div()
                        .w_full()
                        .pb(px(4.))
                        .child(el)
                        .into_any_element();
                }
                if let Some(group) = process_group
                    && group.first == i
                    && process_expanded
                    && !group.active
                {
                    el = v_flex()
                        .w_full()
                        .gap_1()
                        .child(this.render_process_group_header(
                            group,
                            true,
                            animate_ambient,
                            muted,
                            cx,
                        ))
                        .child(el)
                        .into_any_element();
                }
                if let Some(group) = process_group
                    && group.active
                    && process_expanded
                    && i + 1 == group.end
                {
                    el = this.with_live_elapsed_footer(el, group.elapsed_ms, animate_ambient);
                }
                if current_turn_active
                    && i + 1 == this.entries.len()
                    && is_user_entry(entry)
                    && !current_turn_has_agent_output(&this.entries)
                {
                    el = v_flex()
                        .w_full()
                        .gap_3()
                        .child(el)
                        .child(this.render_working_status(
                            live_elapsed_ms(this.turn_started_at_ms),
                            muted,
                            animate_ambient,
                            i,
                        ))
                        .into_any_element();
                }
                if current_turn_active
                    && i + 1 == this.entries.len()
                    && process_group.is_none()
                    && !is_user_entry(entry)
                {
                    el = this.with_live_elapsed_footer(
                        el,
                        live_elapsed_ms(this.turn_started_at_ms),
                        animate_ambient,
                    );
                }
                // `gpui::list` 不像 flex 容器那样处理 `gap`；间距必须属于
                // 虚拟项本身，否则测得的高度不包含消息间的留白。
                let bottom = if in_process_timeline {
                    0.
                } else {
                    match entry {
                        AcpEntry::ToolCall { title, .. }
                            if is_task_completion_tool_title(title) =>
                        {
                            16.
                        }
                        AcpEntry::ToolCall {
                            kind: ToolKind::Collaborate,
                            ..
                        } => 12.,
                        AcpEntry::ToolCall { .. } => 8.,
                        AcpEntry::Assistant { thought: true, .. } => 4.,
                        _ => 16.,
                    }
                };
                h_flex()
                    .w_full()
                    .justify_center()
                    .px_4()
                    .pb(px(bottom))
                    .overflow_hidden()
                    .child(
                        div()
                            .w_full()
                            .max_w(ui_theme::conversation_max_width())
                            .child(el),
                    )
                    .into_any_element()
            })
        })
        .w_full()
        .flex_1()
        .min_h_0()
        .pt_4();
        list.into_any_element()
    }

    fn render_working_status(
        &self,
        elapsed_ms: Option<u64>,
        muted: gpui::Hsla,
        animate_ambient: bool,
        entry_ix: usize,
    ) -> gpui::AnyElement {
        let label = elapsed_ms
            .map(format_duration)
            .map(|elapsed| format!("已用 {elapsed}"))
            .unwrap_or_else(|| "已用 0s".to_string());
        h_flex()
            .id(("acp-working", entry_ix))
            .debug_selector(move || format!("ACP_WORKING_{entry_ix}"))
            .w_full()
            .h(px(28.))
            .flex_shrink_0()
            .gap_1p5()
            .items_center()
            .child(ambient_spinner(
                ("acp-working-spinner", entry_ix),
                gpui::rgb(ui_theme::blue()).into(),
                animate_ambient,
            ))
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(muted)
                    .child(label),
            )
            .into_any_element()
    }

    fn with_live_elapsed_footer(
        &self,
        el: gpui::AnyElement,
        elapsed_ms: Option<u64>,
        animate_ambient: bool,
    ) -> gpui::AnyElement {
        v_flex()
            .w_full()
            .gap_1()
            .child(el)
            .child(self.render_working_status(
                elapsed_ms,
                gpui::rgb(ui_theme::text_muted()).into(),
                animate_ambient,
                usize::MAX,
            ))
            .into_any_element()
    }

    fn render_process_group_header(
        &self,
        group: ProcessGroupInfo,
        expanded: bool,
        animate_ambient: bool,
        muted: gpui::Hsla,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let group_key = group.first;
        let group_end = group.end;
        let group_label = process_group_header_label(&self.entries, group);
        let group_accent_u32 = if group.active {
            ui_theme::blue()
        } else {
            ui_theme::text_muted()
        };
        let group_accent = gpui::rgb(group_accent_u32);
        h_flex()
            .id(("acp-process-group", group.first))
            .debug_selector(move || format!("ACP_PROCESS_GROUP_{}", group.first))
            .w_full()
            .h(px(28.))
            .flex_shrink_0()
            .gap_1p5()
            .items_center()
            .cursor_pointer()
            .hover(|row| row.opacity(0.78))
            .when(group.active, |row| {
                row.child(ambient_spinner(
                    ("acp-process-group-spinner", group.first),
                    group_accent.into(),
                    animate_ambient,
                ))
            })
            .when_some(group_label, |row, label| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(muted)
                        .child(label),
                )
            })
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(gpui::rgb(ui_theme::text_faint()))
                    .child(if expanded { "▾" } else { "▸" }),
            )
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                if !this.expanded_process_groups.remove(&group_key) {
                    this.expanded_process_groups.insert(group_key);
                }
                this.list_state
                    .remeasure_items(group_key..group_end.min(this.entries.len()));
                cx.notify();
            }))
            .into_any_element()
    }

    fn render_compact_tool_run(
        &self,
        run: &CompactToolRun,
        muted: gpui::Hsla,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let start = run.start;
        let open = self.tool_run_is_open(start);
        let count = run.indices.len();
        let faint = gpui::rgb(ui_theme::text_faint());
        let items: Vec<(usize, String)> = run
            .indices
            .iter()
            .filter_map(|&ix| match self.entries.get(ix) {
                Some(AcpEntry::ToolCall { title, .. }) => Some((ix, title.clone())),
                _ => None,
            })
            .collect();
        let header = h_flex()
            .id(("acp-tool-run", start))
            .debug_selector(move || format!("ACP_TOOL_RUN_{start}"))
            .group(EXPAND_ICON_GROUP)
            .w_full()
            .min_h(px(22.))
            .gap_2()
            .items_center()
            .cursor_pointer()
            .child(expandable_lead_icon(
                process_timeline_icon(&run.kind),
                open,
                muted,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(muted)
                    .child(compact_tool_run_label(run.kind, count)),
            )
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                this.toggle_tool_run(start, cx);
            }));
        let mut col = v_flex()
            .id(("acp-tool-run-box", start))
            .w_full()
            .child(header);
        if open {
            for (ix, title) in items {
                let can_expand = match self.entries.get(ix) {
                    Some(AcpEntry::ToolCall {
                        output, children, ..
                    }) => tool_output_has_content(output) || !children.is_empty(),
                    _ => false,
                };
                let row = h_flex()
                    .id(("acp-tool-run-item", ix))
                    .debug_selector(move || format!("ACP_TOOL_RUN_ITEM_{ix}"))
                    .w_full()
                    .min_h(px(20.))
                    .pl_5()
                    .gap_2()
                    .items_center()
                    .when(can_expand, |row| row.cursor_pointer())
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(faint)
                            .when(can_expand, |icon| {
                                icon.group_hover(PEEK_ROW_GROUP, |s| {
                                    s.text_color(gpui::rgb(ui_theme::text_bright()))
                                })
                            })
                            .child(Icon::new(process_timeline_icon(&run.kind)).size(px(13.))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(faint)
                            .when(can_expand, |label| {
                                label.group_hover(PEEK_ROW_GROUP, |s| {
                                    s.text_color(gpui::rgb(ui_theme::text_bright()))
                                })
                            })
                            .child(compact_tool_headline(run.kind, &title)),
                    );
                col = col.child(if can_expand {
                    self.tool_output_popover(
                        ix,
                        muted,
                        Button::new(("acp-tool-run-item-trigger", ix))
                            .text()
                            .w_full()
                            .justify_start()
                            .group(PEEK_ROW_GROUP)
                            .child(row),
                    )
                    .into_any_element()
                } else {
                    row.into_any_element()
                });
            }
        }
        col.into_any_element()
    }

    fn render_subagent_card(
        &self,
        i: usize,
        id: &str,
        title: &str,
        status: ToolCallStatus,
        output: &[ToolOutputPart],
        children: &[AcpEntry],
        has_expandable_content: bool,
        default_expanded: bool,
        card_expanded: bool,
        failed: bool,
        animate_ambient: bool,
        muted: gpui::Hsla,
        border: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let faint = gpui::rgb(ui_theme::text_faint());
        let red = gpui::rgb(ui_theme::red());
        let ink = if failed { red.into() } else { muted };
        let running = matches!(status, ToolCallStatus::InProgress | ToolCallStatus::Pending)
            || smelt_core::acp_chat::has_unfinished_tool_call(children);
        let roster = smelt_core::acp_chat::is_subagent_roster(children);
        let mut headline = smelt_core::acp_chat::subagent_card_title(title, children);
        if headline.is_empty() {
            headline = "子代理".to_string();
        }
        let progress = smelt_core::acp_chat::subagent_progress_text(status, children);
        let progress_failed = failed || progress.contains("失败");
        let show_fail_label = failed && !progress.contains("失败");
        let show_header_spinner = running && !(card_expanded && roster);
        let show_check = !running && !progress_failed && progress.is_empty();
        let id_for_toggle = id.to_string();
        let mut header = h_flex()
            .id(("acp-subagent-toggle", i))
            .w_full()
            .min_h(px(24.))
            .gap_2()
            .items_center()
            .group(EXPAND_ICON_GROUP)
            .cursor_pointer()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, _ev, _window, cx| {
                    this.toggle_tool_card(
                        i,
                        id_for_toggle.clone(),
                        has_expandable_content,
                        default_expanded,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .child(expandable_lead_icon(IconName::Bot, card_expanded, ink))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(ink)
                    .child(headline),
            );
        if !progress.is_empty() {
            header = header.child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(if progress_failed { red } else { faint })
                    .child(progress),
            );
        }
        let status_el: gpui::AnyElement = if show_header_spinner {
            if animate_ambient {
                ambient_spinner(("acp-subagent-spin", i), muted, animate_ambient)
            } else {
                Icon::new(IconName::Loader)
                    .size(px(13.))
                    .text_color(muted)
                    .into_any_element()
            }
        } else if show_fail_label {
            div()
                .flex_shrink_0()
                .text_xs()
                .text_color(red)
                .child("失败")
                .into_any_element()
        } else if show_check {
            Icon::new(IconName::Check)
                .size(px(13.))
                .text_color(faint)
                .into_any_element()
        } else {
            div().into_any_element()
        };
        header = header.child(status_el);
        let mut card = v_flex()
            .id(("acp-subagent-card", i))
            .w_full()
            .rounded(ui_theme::card_radius())
            .border_1()
            .border_color(if failed { red.into() } else { border })
            .bg(ui_theme::glass_card())
            .px_3()
            .py_2()
            .gap_1p5()
            .child(header);
        if card_expanded {
            let summary = subagent_output_summary(output);
            if let Some(text) = summary.as_ref() {
                card = card.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_xs()
                        .text_color(muted)
                        .child(text.clone()),
                );
            }
            if !children.is_empty() {
                card = card.child(self.render_subagent_children(
                    i,
                    children,
                    roster,
                    animate_ambient,
                    muted,
                    cx,
                ));
            }
        }
        card.into_any_element()
    }

    fn render_subagent_children(
        &self,
        parent_ix: usize,
        children: &[AcpEntry],
        roster: bool,
        animate_ambient: bool,
        muted: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if !roster {
            return render_nested_transcript(children, muted);
        }
        let faint = gpui::rgb(ui_theme::text_faint());
        let red = gpui::rgb(ui_theme::red());
        let mut col = v_flex().w_full().gap_1();
        for (child_ix, child) in children.iter().enumerate() {
            col = col.child(match child {
                AcpEntry::Assistant { text, thought } => {
                    let line = text.lines().next().unwrap_or_default();
                    if line.is_empty() {
                        div().into_any_element()
                    } else {
                        div()
                            .pl(px(21.))
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(if *thought {
                                format!("思考  {line}")
                            } else {
                                line.to_string()
                            })
                            .into_any_element()
                    }
                }
                AcpEntry::ToolCall {
                    id,
                    title,
                    kind: ToolKind::Collaborate,
                    status,
                    output,
                    children: nested,
                } => {
                    let failed = matches!(status, ToolCallStatus::Failed);
                    let nested_running = smelt_core::acp_chat::has_unfinished_tool_call(nested);
                    let ink = if failed { red.into() } else { muted };
                    let mut name = smelt_core::acp_chat::subagent_visible_title(title);
                    if name.is_empty() {
                        name = "子代理".to_string();
                    }
                    let has_nested = !nested.is_empty() || tool_output_has_content(output);
                    let expanded = self.tool_card_is_expanded(id, has_nested, false);
                    let spinning = matches!(status, ToolCallStatus::InProgress) || nested_running;
                    let glyph = subagent_child_status_glyph(
                        ("acp-subagent-child-spin", parent_ix * 4096 + child_ix),
                        spinning,
                        failed,
                        matches!(status, ToolCallStatus::Completed) && !spinning,
                        animate_ambient,
                        ink,
                        faint.into(),
                    );
                    let mut row = h_flex()
                        .id(("acp-subagent-child", parent_ix * 4096 + child_ix))
                        .w_full()
                        .min_h(px(22.))
                        .gap_2()
                        .items_center();
                    if has_nested {
                        let id_for_toggle = id.clone();
                        row = row
                            .group(EXPAND_ICON_GROUP)
                            .cursor_pointer()
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _ev, _window, cx| {
                                    this.toggle_tool_card(
                                        parent_ix,
                                        id_for_toggle.clone(),
                                        true,
                                        false,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                }),
                            )
                            .child(expandable_lead(glyph, expanded, ink));
                    } else {
                        row = row.child(status_lead(glyph));
                    }
                    row = row.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(ink)
                            .child(name),
                    );
                    if has_nested && expanded {
                        v_flex()
                            .w_full()
                            .gap_1()
                            .child(row)
                            .child(
                                div()
                                    .w_full()
                                    .pl(px(21.))
                                    .child(render_nested_transcript(nested, muted)),
                            )
                            .into_any_element()
                    } else {
                        row.into_any_element()
                    }
                }
                AcpEntry::ToolCall {
                    title,
                    kind,
                    status,
                    children: nested,
                    ..
                } => nested_tool_row(*kind, title, *status, nested, muted),
                _ => div().into_any_element(),
            });
        }
        col.into_any_element()
    }

    fn tool_output_popover(&self, entry_ix: usize, muted: gpui::Hsla, trigger: Button) -> Popover {
        let (kind, output, children, diffs, images) = match self.entries.get(entry_ix) {
            Some(AcpEntry::ToolCall {
                id,
                kind,
                output,
                children,
                ..
            }) => (
                *kind,
                output.clone(),
                children.clone(),
                self.rendered_diffs.get(id).cloned(),
                self.rendered_tool_images.get(id).cloned(),
            ),
            _ => {
                return Popover::new(("acp-tool-output-pop", entry_ix))
                    .anchor(Anchor::BottomLeft)
                    .appearance(false)
                    .p_0()
                    .trigger(trigger);
            }
        };
        let border: gpui::Hsla = ui_theme::card_stroke().into();
        Popover::new(("acp-tool-output-pop", entry_ix))
            .anchor(Anchor::BottomLeft)
            .appearance(false)
            .p_0()
            .trigger(trigger)
            .content(move |_, _, _| {
                render_tool_output_popover_body(ToolOutputPopoverBody {
                    entry_ix,
                    kind,
                    output: &output,
                    diffs: diffs.as_deref(),
                    images: images.as_deref(),
                    children: &children,
                    muted,
                    border,
                })
            })
    }

    /// 中间正文与 thought 的统一展示。ACP adapter 对两者的切分并不稳定，同一句
    /// “接下来检查调用方”可能被不同 agent 放进任意一种 chunk；UI 不应因此跳成
    /// 两套完全不同的组件。
    fn render_progress_entry(
        &self,
        entry_ix: usize,
        entry: &AcpEntry,
        text: &str,
        in_process: bool,
        muted: gpui::Hsla,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let expanded = self.expanded_thoughts.contains(&entry_ix);
        let preview = progress_summary(text);
        let has_details = progress_has_details(text);
        let mut row = h_flex()
            .id((
                if in_process {
                    "acp-progress-toggle"
                } else {
                    "acp-analysis-toggle"
                },
                entry_ix,
            ))
            .debug_selector(move || format!("ACP_PROGRESS_{entry_ix}"))
            .w_full()
            .min_w_0()
            .min_h(px(if in_process { 22. } else { 26. }))
            .when(!in_process, |row| row.px_2().rounded_md())
            .gap_2()
            .items_center()
            .when(in_process && has_details, |row| {
                row.group(EXPAND_ICON_GROUP)
            })
            .child(if in_process {
                let thought = matches!(entry, AcpEntry::Assistant { thought: true, .. });
                if has_details && thought {
                    expandable_lead_icon(IconName::Asterisk, expanded, muted)
                } else if thought {
                    Icon::new(IconName::Asterisk)
                        .size(px(13.))
                        .text_color(muted)
                        .into_any_element()
                } else if has_details {
                    expandable_lead(
                        div()
                            .size(px(8.))
                            .rounded_full()
                            .border_1()
                            .border_color(muted)
                            .into_any_element(),
                        expanded,
                        muted,
                    )
                } else {
                    div()
                        .size(px(8.))
                        .flex_shrink_0()
                        .rounded_full()
                        .border_1()
                        .border_color(muted)
                        .into_any_element()
                }
            } else {
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .font_medium()
                    .text_color(muted)
                    .child("分析摘要")
                    .into_any_element()
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(muted)
                    .child(preview),
            );
        if has_details {
            row = row
                .cursor_pointer()
                .when(!in_process, |row| {
                    row.hover(|row| row.bg(gpui::rgb(ui_theme::bg_hover())))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(muted)
                                .child(if expanded { "▾" } else { "▸" }),
                        )
                })
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    if !this.expanded_thoughts.remove(&entry_ix) {
                        this.expanded_thoughts.insert(entry_ix);
                    }
                    this.list_state
                        .remeasure_items(entry_ix..entry_ix.saturating_add(1));
                    cx.notify();
                }));
        }
        v_flex()
            .w_full()
            .min_w_0()
            .child(row)
            .when(expanded && has_details, |col| {
                col.child(
                    div()
                        .min_w_0()
                        .pl(px(if in_process { 24. } else { 8. }))
                        .pr_2()
                        .pt_2()
                        .pb_2()
                        .text_xs()
                        .text_color(muted)
                        .child(smelt_ui::markdown_mermaid::markdown_view_clickable(
                            (
                                if in_process {
                                    "acp-progress-md"
                                } else {
                                    "acp-analysis-md"
                                },
                                entry_ix,
                            ),
                            cached_entry_markdown(
                                &self.rendered_markdown,
                                entry_ix,
                                entry,
                                self.cwd.as_deref(),
                            ),
                        )),
                )
            })
            .into_any_element()
    }
}

/// hover 预览的渲染入参。工具输出的展示要素（原始 parts + 两份渲染缓存 + 配色）
/// 一起传，散成八个位置参数既超 clippy 阈值也容易调错顺序。
struct ToolOutputPopoverBody<'a> {
    entry_ix: usize,
    kind: ToolKind,
    output: &'a [ToolOutputPart],
    diffs: Option<&'a [Option<super::super::CachedDiff>]>,
    images: Option<&'a [Option<std::sync::Arc<gpui::Image>>]>,
    children: &'a [AcpEntry],
    muted: gpui::Hsla,
    border: gpui::Hsla,
}

fn render_tool_output_popover_body(body: ToolOutputPopoverBody<'_>) -> gpui::AnyElement {
    let ToolOutputPopoverBody {
        entry_ix,
        kind,
        output,
        diffs,
        images,
        children,
        muted,
        border,
    } = body;
    let mut col = v_flex()
        .id(("acp-tool-output-pop-body", entry_ix))
        .min_w(px(280.))
        .max_w(px(520.))
        .max_h(px(360.))
        .p_2()
        .gap_1()
        .overflow_y_scroll()
        .bg(ui_theme::glass_floating())
        .border_1()
        .border_color(ui_theme::card_stroke())
        .rounded(px(8.));
    for (part_ix, part) in output.iter().enumerate() {
        col = match part {
            ToolOutputPart::Diff { path, .. } => {
                let cached = diffs
                    .and_then(|parts| parts.get(part_ix))
                    .and_then(Option::as_ref);
                col.child(
                    v_flex()
                        .gap_1()
                        .child(
                            selectable_plain_text(
                                ("acp-tool-pop-diff-path", entry_ix * 100 + part_ix),
                                path,
                            )
                            .text_xs()
                            .text_color(muted),
                        )
                        .children(cached.map(|diff| {
                            render_diff_lines(&diff.lines, (entry_ix, part_ix), border, muted)
                        })),
                )
            }
            ToolOutputPart::Text(text) if !text.trim().is_empty() => {
                let body = strip_code_fence(text);
                let body_el: gpui::AnyElement = if matches!(kind, ToolKind::Other) {
                    smelt_ui::markdown_mermaid::markdown_view_clickable(
                        ("acp-tool-pop-md", entry_ix * 100 + part_ix),
                        body.to_string(),
                    )
                    .text_xs()
                    .text_color(muted)
                    .into_any_element()
                } else {
                    selectable_plain_text(("acp-tool-pop-text", entry_ix * 100 + part_ix), &body)
                        .text_xs()
                        .text_color(muted)
                        .font_family(smelt_core::font_config::font_family())
                        .into_any_element()
                };
                col.child(body_el)
            }
            ToolOutputPart::Image(_) => {
                match images
                    .and_then(|parts| parts.get(part_ix))
                    .and_then(Option::as_ref)
                {
                    // hover 预览没有 `Context<AcpView>`，只出缩略图，不接点击大图；
                    // 解码交给展开卡片路径触发（缓存是内容寻址的，全局共享）。
                    Some(image) => col.child(image_thumb_element(image)),
                    None => col,
                }
            }
            ToolOutputPart::Text(_) => col,
            ToolOutputPart::Terminal { output, .. } if !output.trim().is_empty() => col.child(
                selectable_plain_text(("acp-tool-pop-terminal", entry_ix * 100 + part_ix), output)
                    .text_xs()
                    .text_color(muted)
                    .font_family(smelt_core::font_config::font_family()),
            ),
            ToolOutputPart::Terminal { .. } => col,
        };
    }
    if !children.is_empty() {
        col = col.child(render_nested_transcript(children, muted));
    }
    col.into_any_element()
}

fn subagent_output_summary(output: &[ToolOutputPart]) -> Option<String> {
    let text = completion_summary_text(output);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let line = trimmed.lines().next().unwrap_or(trimmed).trim();
    if trimmed.lines().nth(1).is_some() {
        Some(format!("{line}…"))
    } else {
        Some(line.to_string())
    }
}

fn render_nested_transcript(children: &[AcpEntry], muted: gpui::Hsla) -> gpui::AnyElement {
    let mut col = v_flex().w_full().gap_1();
    for child in children {
        col = col.child(match child {
            AcpEntry::Assistant { text, thought } => {
                let line = text.lines().next().unwrap_or_default();
                if line.is_empty() {
                    div().into_any_element()
                } else {
                    div()
                        .text_xs()
                        .text_color(muted)
                        .truncate()
                        .child(if *thought {
                            format!("思考  {line}")
                        } else {
                            line.to_string()
                        })
                        .into_any_element()
                }
            }
            AcpEntry::ToolCall {
                title,
                kind,
                status,
                children: nested,
                ..
            } => nested_tool_row(*kind, title, *status, nested, muted),
            _ => div().into_any_element(),
        });
    }
    col.into_any_element()
}

fn nested_tool_row(
    kind: ToolKind,
    title: &str,
    status: ToolCallStatus,
    nested: &[AcpEntry],
    muted: gpui::Hsla,
) -> gpui::AnyElement {
    let failed = matches!(status, ToolCallStatus::Failed);
    let ink = if failed {
        gpui::rgb(ui_theme::red()).into()
    } else {
        muted
    };
    let headline = if matches!(kind, ToolKind::Collaborate) {
        let visible = smelt_core::acp_chat::subagent_visible_title(title);
        if visible.is_empty() {
            "子代理".to_string()
        } else {
            visible
        }
    } else {
        compact_tool_headline(kind, title)
    };
    let mut row = h_flex()
        .w_full()
        .min_h(px(20.))
        .gap_2()
        .items_center()
        .child(
            Icon::new(process_timeline_icon(&kind))
                .size(px(12.))
                .text_color(ink),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(ink)
                .child(headline),
        );
    if failed {
        row = row.child(
            div()
                .flex_shrink_0()
                .text_xs()
                .text_color(gpui::rgb(ui_theme::red()))
                .child("失败"),
        );
    }
    if nested.is_empty() {
        row.into_any_element()
    } else {
        v_flex()
            .w_full()
            .gap_1()
            .child(row)
            .child(
                div()
                    .w_full()
                    .pl(px(21.))
                    .child(render_nested_transcript(nested, muted)),
            )
            .into_any_element()
    }
}

fn status_lead(child: gpui::AnyElement) -> gpui::AnyElement {
    div()
        .size(px(13.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .child(child)
        .into_any_element()
}

fn subagent_child_status_glyph(
    spin_id: impl Into<gpui::ElementId>,
    spinning: bool,
    failed: bool,
    completed: bool,
    animate_ambient: bool,
    ink: gpui::Hsla,
    faint: gpui::Hsla,
) -> gpui::AnyElement {
    if failed {
        return Icon::new(IconName::Close)
            .size(px(12.))
            .text_color(gpui::rgb(ui_theme::red()))
            .into_any_element();
    }
    if spinning {
        return if animate_ambient {
            ambient_spinner(spin_id, ink, animate_ambient)
        } else {
            Icon::new(IconName::Loader)
                .size(px(12.))
                .text_color(ink)
                .into_any_element()
        };
    }
    if completed {
        return Icon::new(IconName::Check)
            .size(px(12.))
            .text_color(faint)
            .into_any_element();
    }
    div()
        .size(px(8.))
        .rounded_full()
        .border_1()
        .border_color(faint)
        .into_any_element()
}

const EXPAND_ICON_GROUP: &str = "acp-expand-row";
const PEEK_ROW_GROUP: &str = "acp-peek-row";

/// 可展开行：平时左侧是工具图标，hover 换成箭头。展开仍靠点击。
fn expandable_lead_icon(
    rest_icon: IconName,
    expanded: bool,
    color: impl Into<gpui::Hsla>,
) -> gpui::AnyElement {
    let color = color.into();
    hover_swap_lead(
        Icon::new(rest_icon)
            .size(px(13.))
            .text_color(color)
            .into_any_element(),
        if expanded {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        },
        color,
    )
}

fn expandable_lead(
    rest: gpui::AnyElement,
    expanded: bool,
    color: impl Into<gpui::Hsla>,
) -> gpui::AnyElement {
    hover_swap_lead(
        rest,
        if expanded {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        },
        color,
    )
}

fn hover_swap_lead(
    rest: gpui::AnyElement,
    hover_icon: IconName,
    color: impl Into<gpui::Hsla>,
) -> gpui::AnyElement {
    let color = color.into();
    div()
        .relative()
        .size(px(13.))
        .flex_shrink_0()
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .group_hover(EXPAND_ICON_GROUP, |s| s.opacity(0.))
                .child(rest),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .opacity(0.)
                .group_hover(EXPAND_ICON_GROUP, |s| s.opacity(1.))
                .child(Icon::new(hover_icon).size(px(13.)).text_color(color)),
        )
        .into_any_element()
}

/// 缩略图元素：降采样 + 内容寻址缓存 + canvas contain 绘制。历史上大图/长截图
/// 在调用点反复出 bug（撑破屏幕、只露顶部、撞 sprite atlas 上限），已发消息、
/// 工具输出图片统一收敛到这一条路径，不再各写一份。
fn image_thumb_element(image: &std::sync::Arc<gpui::Image>) -> gpui::AnyElement {
    match smelt_ui::image::cached(image) {
        Some(render) => smelt_ui::image::contain_canvas_at_height(render, px(160.), px(280.), 8.0)
            .into_any_element(),
        // SVG 或解码未完成：回退原 `img` 渲染（svg 走 gpui 全彩栅格化管线；
        // 解码完成后切到 contain 路径）。
        None => gpui::img(image.clone())
            .h(px(160.))
            .max_w(px(280.))
            .into_any_element(),
    }
}

/// 后台解码到 `smelt_ui::image` 缓存；解码完成后通知重绘切到 contain 路径。
fn ensure_image_decoded(image: &std::sync::Arc<gpui::Image>, cx: &Context<AcpView>) {
    if smelt_ui::image::cached(image).is_some() || image.format == gpui::ImageFormat::Svg {
        return;
    }
    let fetch_image = image.clone();
    cx.spawn(async move |this, cx| {
        smelt_ui::image::fetch_async(fetch_image, cx.background_executor()).await;
        let _ = this.update(cx, |_, cx| cx.notify());
    })
    .detach();
}

/// 可点开大图的缩略图卡片（已发消息里的图、工具返回的图共用）。
fn render_clickable_image_thumb(
    image: std::sync::Arc<gpui::Image>,
    id: impl Into<gpui::ElementId>,
    border: gpui::Hsla,
    cx: &Context<AcpView>,
) -> gpui::AnyElement {
    ensure_image_decoded(&image, cx);
    let thumb = image_thumb_element(&image);
    let preview_image = image;
    div()
        .id(id)
        .overflow_hidden()
        .rounded_md()
        .border_1()
        .border_color(border)
        .cursor_pointer()
        .hover(|image| image.border_color(ui_theme::tint(ui_theme::accent(), 0x72)))
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.emit(AcpViewEvent::PreviewImage(preview_image.clone()));
        }))
        .child(thumb)
        .into_any_element()
}
