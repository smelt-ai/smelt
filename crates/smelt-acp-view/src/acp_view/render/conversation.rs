//! ACP 消息流虚拟列表。

use super::*;
use gpui_base::motion::MotionReveal;

fn owner_id(name: &'static str, owner: &gpui::SharedString) -> gpui::ElementId {
    gpui::ElementId::from((gpui::ElementId::from(name), owner.clone()))
}

fn owner_part(name: &'static str, owner: &gpui::SharedString, part: usize) -> gpui::ElementId {
    gpui::ElementId::from((gpui::ElementId::named_usize(name, part), owner.clone()))
}

fn user_message_context_menu(
    menu: PopupMenu,
    view: Entity<AcpView>,
    cwd: Option<String>,
    fork_index: Option<usize>,
    fork_label: &'static str,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let menu = if let Some(index) = fork_index {
        let fork_view = view.clone();
        menu.item(
            PopupMenuItem::new(fork_label)
                .icon(Icon::empty().path("smelt-icons/git-branch.svg"))
                .on_click(move |_event, _window, cx| {
                    fork_view.update(cx, |this, cx| this.fork_from_user_message(index, cx));
                }),
        )
    } else {
        menu
    };
    selected_text_context_menu(menu, view, cwd, window, cx)
}

/// 用户消息气泡。
///
/// gpui-component 的 markdown 列表块（ol/ul）内部用 `w_full()`/`flex_1()`
/// 排布「序号 + 正文」。气泡收缩到内容大小（只有 max_w，没有 width）时，短列表
/// 会被测成只有序号那么宽，正文被 `overflow_hidden()` 裁掉。最小宽度给正文留位置。
fn user_message_bubble(id: impl Into<gpui::ElementId>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .max_w(gpui::relative(0.72))
        .min_w(gpui::px(160.))
        .px_4()
        .py_2p5()
        .rounded(ui_theme::bubble_radius())
        .bg(ui_theme::overlay(0x14))
        .hover(|bubble| bubble.bg(ui_theme::overlay(0x1c)))
        .text_sm()
}

/// 只包刚发送的那一条。nonce 进元素 id，虚拟列表把行卸掉再装上时，过了时钟就不再播。
/// Div 没有 scale，用透明度和 6px 下落代替从 0.92 缩放进来。
fn user_bubble_with_enter<E>(nonce: Option<u64>, index: usize, bubble: E) -> gpui::AnyElement
where
    E: gpui::Styled + gpui::IntoElement + 'static,
{
    let Some(nonce) = nonce else {
        return bubble.into_any_element();
    };
    bubble
        .with_animation(
            format!("acp-user-enter-{nonce}-{index}"),
            Animation::new(std::time::Duration::from_millis(ENTER_MS)).with_easing(ease_overshoot),
            |bubble, delta| {
                bubble
                    .opacity(delta.clamp(0.0, 1.0))
                    .relative()
                    .top(px(enter_offset_px(delta)))
            },
        )
        .into_any_element()
}

/// 回合中新出现的条目：淡入，同时从下方 4px 升到原位。只改绘制位置，不改行高，
/// 虚拟列表不用为它重新测量。
fn entry_with_enter(progress: Option<f32>, el: gpui::AnyElement) -> gpui::AnyElement {
    let Some(progress) = progress else {
        return el;
    };
    div()
        .relative()
        .top(px(4. * (1. - progress)))
        .opacity(progress)
        .child(el)
        .into_any_element()
}

struct SubagentCard<'a> {
    index: usize,
    id: &'a str,
    title: &'a str,
    status: ToolCallStatus,
    output: &'a [ToolOutputPart],
    children: &'a [AcpEntry],
    has_expandable_content: bool,
    default_expanded: bool,
    card_expanded: bool,
    failed: bool,
    animate_ambient: bool,
    muted: gpui::Hsla,
    border: gpui::Hsla,
    live_elapsed: Option<u64>,
}

impl AcpView {
    /// 消息列表里的 Markdown 正文。解析状态由视图按位置留着，不挂在元素上。
    ///
    /// 挂在元素上的状态，行一滚出虚拟列表的渲染范围就跟着丢。超过 4KB 的正文在后台
    /// 解析，滚回来先以空内容量出一个很矮的高度，往回滚时会把整条回答跳过去，
    /// 解析完又一下撑开。解析结果落地（含图片加载完重排）时请列表重新量这一行，
    /// 还在视口外的行就能在滚到之前量对。
    fn retained_markdown(
        &self,
        site: &'static str,
        entry_ix: usize,
        part: usize,
        text: gpui::SharedString,
        cx: &mut Context<Self>,
    ) -> gpui_component::text::TextView {
        let global = self.loaded_entries_offset + entry_ix;
        let mut retained = self.retained_text.borrow_mut();
        let state = match retained.get_mut(&(site, global, part)) {
            Some(slot) => {
                if slot.text.as_ptr() != text.as_ptr() || slot.text.len() != text.len() {
                    slot.state.update(cx, |state, cx| state.set_text(&text, cx));
                    slot.text = text;
                }
                slot.state.clone()
            }
            None => {
                let state = cx.new(|cx| gpui_component::text::TextViewState::markdown(&text, cx));
                let remeasure = cx.observe(&state, move |this, _, cx| {
                    let Some(ix) = global
                        .checked_sub(this.loaded_entries_offset)
                        .filter(|ix| *ix < this.entries.len())
                    else {
                        return;
                    };
                    this.list_state.remeasure_items(ix..ix + 1);
                    cx.notify();
                });
                retained.insert(
                    (site, global, part),
                    super::super::RetainedText {
                        state: state.clone(),
                        text,
                        _remeasure: remeasure,
                    },
                );
                state
            }
        };
        smelt_ui::markdown_mermaid::markdown_view_for_state(&state)
    }

    pub(super) fn render_message_list(
        &self,
        animate_ambient: bool,
        clear_capsule: bool,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let current_turn_active = self.has_active_turn();
        let conversation_layout = self.conversation_layout();
        // 虚拟列表把行高缓存到重新测量。高度还在变的思考详情和入场气泡，
        // 不在布局前标脏的话，动画只改样式、行高不动。
        let reveal_indices: Vec<usize> = self.thought_reveals.keys().copied().collect();
        for ix in reveal_indices {
            self.list_state.remeasure_items(ix..ix.saturating_add(1));
        }
        // These values are stable for the whole render pass. Computing them once keeps the
        // virtual-list item builder from rescanning the full conversation for every row.
        let active_permission_tool_id = self
            .permissions
            .first()
            .map(|card| card.tool_call_id.clone());
        let latest_user_message_index = super::super::latest_user_message_index(&self.entries);
        if self
            .user_enter
            .as_ref()
            .is_some_and(|enter| enter.started.is_some())
            && let Some(ix) = latest_user_message_index
        {
            self.list_state.remeasure_items(ix..ix.saturating_add(1));
        }
        let view = cx.entity();
        let list = virtual_list(self.list_state.clone(), move |i, _window, app| {
            let conversation_layout = conversation_layout.clone();
            view.update(app, |this, cx| {
                let t = cx.theme().colors;
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
                let elapsed_here = row_live_elapsed(
                    current_turn_active,
                    i,
                    &this.entries,
                    process_group,
                    process_group
                        .filter(|group| group.active)
                        .and_then(|group| group.elapsed_ms)
                        .or_else(|| live_elapsed_ms(this.turn_started_at_ms)),
                    active_permission_tool_id.as_deref(),
                );
                let run_wraps_elapsed =
                    tool_run.as_ref().is_some_and(|run| run.start == i) && elapsed_here.is_some();
                let row_elapsed = elapsed_here.filter(|_| !run_wraps_elapsed);
                let run_elapsed = elapsed_here.filter(|_| run_wraps_elapsed);
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
                        let fork_index = show_fork_edit.then_some(i);
                        h_flex()
                            .w_full()
                            .justify_end()
                            .gap_2()
                            .when(show_edit, |row| {
                                row.child(
                                    Button::new(("acp-edit-message", i))
                                        .icon(Icon::new(IconName::Undo2))
                                        .ghost()
                                        .xsmall()
                                        .icon_action("编辑并重发最后一条消息")
                                        .on_click(cx.listener(
                                            move |this, _ev, _window, cx| {
                                                this.rewind_to_message(i, cx);
                                            },
                                        )),
                                )
                            })
                            .child(user_bubble_with_enter(
                                this.user_enter_nonce_for(i),
                                i,
                                user_message_bubble(("acp-user-message", i))
                                    .debug_selector(move || format!("ACP_USER_MESSAGE_{i}"))
                                    .child(this.retained_markdown(
                                        "acp-user-md",
                                        i,
                                        0,
                                        cached_entry_markdown(
                                            &this.rendered_markdown,
                                            i,
                                            entry,
                                            FileLinks::of(&this.runtime, this.cwd.as_deref()),
                                        ),
                                        cx,
                                    ))
                                    .context_menu(move |menu, window, cx| {
                                        user_message_context_menu(
                                            menu,
                                            task_view.clone(),
                                            task_cwd.clone(),
                                            fork_index,
                                            "从这里分叉并编辑",
                                            window,
                                            cx,
                                        )
                                    }),
                            ))
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
                                this.retained_markdown(
                                    "acp-user-images-md",
                                    i,
                                    0,
                                    cached_entry_markdown(
                                        &this.rendered_markdown,
                                        i,
                                        entry,
                                        FileLinks::of(&this.runtime, this.cwd.as_deref()),
                                    ),
                                    cx,
                                ),
                            );
                        }
                        let mut image_strip = h_flex().gap_2().flex_wrap();
                        for image_ix in 0..images.len() {
                            if let Some(image) = this.rendered_images.get(&(i, image_ix)).cloned() {
                                image_strip = image_strip.child(render_clickable_image_thumb(
                                    image,
                                    ("acp-sent-image", image_ix),
                                    t.border,
                                    cx,
                                ));
                            }
                        }
                        let fork_index = show_fork_edit.then_some(i);
                        h_flex()
                            .w_full()
                            .justify_end()
                            .gap_2()
                            .when(show_edit, |row| {
                                row.child(
                                    Button::new(("acp-edit-message", i))
                                        .icon(Icon::new(IconName::Undo2))
                                        .ghost()
                                        .xsmall()
                                        .icon_action("编辑并重发最后一条消息（图片需重新添加）")
                                        .on_click(cx.listener(
                                            move |this, _ev, _window, cx| {
                                                this.rewind_to_message(i, cx);
                                            },
                                        )),
                                )
                            })
                            .child(user_bubble_with_enter(
                                this.user_enter_nonce_for(i),
                                i,
                                user_message_bubble(("acp-user-images-message", i))
                                    .child(content.child(image_strip))
                                    .context_menu(move |menu, window, cx| {
                                        user_message_context_menu(
                                            menu,
                                            task_view.clone(),
                                            task_cwd.clone(),
                                            fork_index,
                                            "从这里分叉并编辑（图片需重新添加）",
                                            window,
                                            cx,
                                        )
                                    }),
                            ))
                            .into_any_element()
                    }
                    // 过程组里的思考和中间正文都走进展行：一行摘要，长文可展开。
                    AcpEntry::Assistant { text, .. }
                        if process_group.is_some() && text.trim().is_empty() =>
                    {
                        div().h(px(0.)).w_full().overflow_hidden().into_any_element()
                    }
                    AcpEntry::Assistant { text, .. } if process_group.is_some() => this
                        .render_progress_entry(
                            i,
                            entry,
                            text,
                            true,
                            muted,
                            row_elapsed,
                            animate_ambient,
                            cx,
                        ),
                    AcpEntry::Assistant {
                        text,
                        thought: true,
                    } => this.render_progress_entry(
                        i,
                        entry,
                        text,
                        false,
                        muted,
                        row_elapsed,
                        animate_ambient,
                        cx,
                    ),
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
                            .child(this.retained_markdown(
                                "acp-md",
                                i,
                                0,
                                cached_entry_markdown(
                                    &this.rendered_markdown,
                                    i,
                                    entry,
                                    FileLinks::of(&this.runtime, this.cwd.as_deref()),
                                ),
                                cx,
                            ))
                            .when(final_answer, |col| {
                                col.child(
                                    h_flex()
                                        .pt_1()
                                        .gap_1()
                                        .child(this.render_copy_button(
                                            CopySite::Answer(i),
                                            text.clone(),
                                            "复制回答",
                                            cx,
                                        ))
                                        .when(
                                            smelt_model::session_handoff::live_fork_is_available(
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
                                                        .icon_action("分叉对话")
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
                            markdown_text_for_cwd(&body, FileLinks::of(&this.runtime, this.cwd.as_deref())).into()
                        } else {
                            cached_entry_markdown(
                                &this.rendered_markdown,
                                i,
                                entry,
                                FileLinks::of(&this.runtime, this.cwd.as_deref()),
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
                            .child(this.retained_markdown(
                                "acp-completion-md",
                                i,
                                0,
                                body_markdown,
                                cx,
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
                                    .child(this.render_copy_button(
                                        CopySite::Completion(i),
                                        body.clone(),
                                        "复制完成摘要",
                                        cx,
                                    )),
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
                        let owner = super::super::lookup_owner(&this.layout_cache.borrow().owner_ids, id);
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
                            let headline = compact_tool_headline(*kind, title);
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
                                .id(owner_id("acp-tool-compact", &owner))
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
                                        .child(headline.clone()),
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
                            if let Some(elapsed_ms) = row_elapsed {
                                row = row.child(elapsed_suffix(elapsed_ms, muted));
                            }
                            if can_expand {
                                this.tool_output_popover(
                                    i,
                                    muted,
                                    Button::new(owner_id("acp-tool-compact-trigger", &owner))
                                        .text()
                                        .w_full()
                                        .justify_start()
                                        .group(PEEK_ROW_GROUP)
                                        .accessibility_label(headline)
                                        .child(row),
                                )
                                .into_any_element()
                            } else {
                                row.into_any_element()
                            }
                        } else if is_subagent {
                            this.render_subagent_card(
                                SubagentCard {
                                    index: i,
                                    id,
                                    title,
                                    status: *status,
                                    output,
                                    children,
                                    has_expandable_content,
                                    default_expanded,
                                    card_expanded,
                                    failed,
                                    animate_ambient,
                                    muted,
                                    border: t.border,
                                    live_elapsed: row_elapsed,
                                },
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
                                        "acp-tool-status-spin",
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
                            .id(owner_id("acp-tool-card-toggle", &owner))
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
                            .child(header_right)
                            .when_some(row_elapsed, |header, elapsed_ms| {
                                header.child(elapsed_suffix(elapsed_ms, muted))
                            });
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
                                                        owner_part("acp-diff-path", &owner, part_ix),
                                                        path,
                                                    )
                                                        .text_xs()
                                                        .text_color(muted)
                                                )
                                                .children(cached.map(|diff| {
                                                    render_diff_lines(
                                                        &diff.lines,
                                                        owner_part("acp-diff", &owner, part_ix),
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
                                        // 自由格式工具可能本来就是 markdown（`##`/`**`/列表）。
                                        // 控制台输出、文件内容走等宽行视口，星号和井号都是正文。
                                        if matches!(kind, ToolKind::Other) {
                                            let lines: Vec<&str> = body.lines().collect();
                                            let total = lines.len();
                                            let key = id.to_string();
                                            let expanded = this.expanded_tools.contains(&key);
                                            let shown = if expanded
                                                || total <= TOOL_OUTPUT_PREVIEW_LINES
                                            {
                                                body.to_string()
                                            } else {
                                                lines[..TOOL_OUTPUT_PREVIEW_LINES].join("\n")
                                            };
                                            let need_toggle = total > TOOL_OUTPUT_PREVIEW_LINES;
                                            let body_el = this.retained_markdown(
                                                "acp-tool-output-md",
                                                i,
                                                part_ix,
                                                shown.into(),
                                                cx,
                                            )
                                            .text_xs()
                                            .text_color(muted)
                                            .into_any_element();
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
                                                                .id(owner_part(
                                                                    "acp-tool-toggle",
                                                                    &owner,
                                                                    part_ix,
                                                                ))
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
                                                                                .remeasure_items(
                                                                                    i..i.saturating_add(1),
                                                                                );
                                                                            cx.stop_propagation();
                                                                            cx.notify();
                                                                        },
                                                                    ),
                                                                ),
                                                        )
                                                    }),
                                            )
                                        } else {
                                            card.child(this.render_plain_tool_log(
                                                PlainToolLog {
                                                    entry_ix: i,
                                                    part_ix,
                                                    body,
                                                    tool_id: id,
                                                    muted,
                                                },
                                                cx,
                                            ))
                                        }
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
                                                        owner_part("acp-tool-image", &owner, part_ix),
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
                                        card.child(this.render_plain_tool_log(
                                            PlainToolLog {
                                                entry_ix: i,
                                                part_ix,
                                                body: output,
                                                tool_id: id,
                                                muted,
                                            },
                                            cx,
                                        ))
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
                    AcpEntry::TaskNote(note) => {
                        let note_owner =
                            super::super::lookup_owner(&this.layout_cache.borrow().owner_ids, &note.id);
                        // 后台任务和后台子智能体的结果：默认只占一行。输出可能很长，模型已经
                        // 拿到完整内容，人要看时再点开，展开后限高滚动。展开状态和思考详情
                        // 共用 `expanded_thoughts`（key 都是 entries 下标，互不冲突）。
                        let has_output = !note.output_tail.trim().is_empty();
                        let expanded = has_output && this.expanded_thoughts.contains(&i);
                        let summary = h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_1p5()
                            .items_center()
                            .py_1()
                            .text_xs()
                            .text_color(muted)
                            .child(div().min_w_0().truncate().child(note.summary()));
                        let summary = if has_output {
                            let hover = t.foreground;
                            summary
                                .id(owner_id("acp-task-note-row", &note_owner))
                                .cursor_pointer()
                                .hover(move |row| row.text_color(hover))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .child(if expanded { "▾" } else { "▸" }),
                                )
                                .on_click(cx.listener(move |this, _ev, _window, cx| {
                                    if !this.expanded_thoughts.remove(&i) {
                                        this.expanded_thoughts.insert(i);
                                    }
                                    this.list_state.remeasure_items(i..i.saturating_add(1));
                                    cx.notify();
                                }))
                                .into_any_element()
                        } else {
                            summary.into_any_element()
                        };
                        let mut block = v_flex().w_full().child(summary);
                        if expanded {
                            block = block.child(
                                div()
                                    .id(owner_id("acp-task-note-output", &note_owner))
                                    .w_full()
                                    .max_h(px(320.))
                                    .overflow_y_scroll()
                                    .pl_3()
                                    .pb_2()
                                    .child(
                                        selectable_plain_text(
                                            owner_id("acp-task-note", &note_owner),
                                            &note.output_tail,
                                        )
                                        .text_xs()
                                        .text_color(muted)
                                        .font_family(smelt_model::font_config::font_family()),
                                    ),
                            );
                        }
                        block.into_any_element()
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
                        .child(this.render_compact_tool_run(run, muted, run_elapsed, cx));
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
                if current_turn_active
                    && i + 1 == this.entries.len()
                    && is_user_entry(entry)
                    && !current_turn_has_agent_output(&this.entries)
                    && this.provider_retry.is_none()
                {
                    el = v_flex()
                        .w_full()
                        .gap_3()
                        .child(el)
                        .child(this.render_working_status(
                            live_elapsed_ms(this.turn_started_at_ms),
                            i,
                            animate_ambient,
                        ))
                        .into_any_element();
                }
                // 对话末尾：在跑时说明正在自动重试；跑完了没正常结束就给一张卡，
                // 能直接重试或继续。都挂在最后一条上，不另占一行虚拟项。
                if i + 1 == this.entries.len() {
                    let tail = if current_turn_active {
                        this.provider_retry
                            .as_ref()
                            .map(|retry| this.render_provider_retry(retry, i, animate_ambient))
                    } else {
                        this.render_turn_outcome_card(i, cx)
                    };
                    if let Some(tail) = tail {
                        el = v_flex()
                            .w_full()
                            .gap_3()
                            .child(el)
                            .child(tail)
                            .into_any_element();
                    }
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
                // 没有顶栏时，第一条和从胶囊跳回的那一问让开标题胶囊；滚上去
                // 仍会钻到它底下。
                let top = if clear_capsule && (i == 0 || this.revealed_prompt == Some(i)) {
                    ui_theme::CHROME_ISLAND_CLEARANCE_PX
                } else {
                    0.
                };
                h_flex()
                    .w_full()
                    .justify_center()
                    .px_4()
                    .pt(px(top))
                    .pb(px(bottom))
                    .overflow_hidden()
                    .child(
                        div()
                            .w_full()
                            .max_w(ui_theme::conversation_max_width())
                            .child(entry_with_enter(this.entry_enter_progress(i), el)),
                    )
                    .into_any_element()
            })
        })
        // 这条列表是缓存视图的根。GPUI 用 layout_as_root 单独排它，
        // 不在外层 flex 里。flex_1 只对 flex 子项生效，根节点高度会塌成 0：
        // 历史补进来之后欢迎语消失，舞台只剩一块黑。
        .size_full()
        .min_h_0();
        list.into_any_element()
    }

    /// 模型还没出字时的一行耗时。有动作之后，时间改挂在那一行后面。
    fn render_provider_retry(
        &self,
        retry: &smelt_model::acp_session::ProviderRetryView,
        entry_ix: usize,
        animate_ambient: bool,
    ) -> gpui::AnyElement {
        let mark: gpui::Hsla = gpui::rgb(ui_theme::yellow()).into();
        v_flex()
            .id(("acp-provider-retry", entry_ix))
            .gap_0p5()
            .text_xs()
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .text_color(gpui::rgb(ui_theme::text_mid()))
                    .child(quiet_dots(
                        ("acp-provider-retry-dots", entry_ix),
                        mark,
                        animate_ambient,
                    ))
                    .child(provider_retry_label(retry)),
            )
            .when(!retry.error.is_empty(), |col| {
                col.child(
                    div()
                        .pl(px(14.))
                        .truncate()
                        .text_color(gpui::rgb(ui_theme::text_muted()))
                        .child(retry.error.clone()),
                )
            })
            .into_any_element()
    }

    fn render_turn_outcome_card(
        &self,
        entry_ix: usize,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let (message, action) = turn_outcome_card(self.turn_outcome)?;
        let label = match action {
            TurnOutcomeAction::Retry => "重试",
            TurnOutcomeAction::Continue => "继续",
        };
        Some(
            h_flex()
                .id(("acp-turn-outcome", entry_ix))
                .w_full()
                .gap_2()
                .items_center()
                .px_3()
                .py_2()
                .rounded(ui_theme::card_radius())
                .border_1()
                .border_color(ui_theme::card_stroke())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(gpui::rgb(ui_theme::yellow()))
                        .child(message),
                )
                .child(
                    Button::new(("acp-turn-outcome-action", entry_ix))
                        .small()
                        .label(label)
                        .on_click(cx.listener(move |this, _ev, window, cx| {
                            this.rerun_last_turn(action, window, cx);
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_working_status(
        &self,
        elapsed_ms: Option<u64>,
        entry_ix: usize,
        animate_ambient: bool,
    ) -> gpui::AnyElement {
        let elapsed = elapsed_ms
            .map(format_duration)
            .unwrap_or_else(|| "0s".to_string());
        let mark: gpui::Hsla = gpui::rgb(ui_theme::text_muted()).into();
        h_flex()
            .id(("acp-working", entry_ix))
            .debug_selector(move || format!("ACP_WORKING_{entry_ix}"))
            .h(ui_theme::height::xxs())
            .gap_1p5()
            .items_center()
            .text_xs()
            .font_features(ui_theme::tabular_figures())
            .text_color(gpui::rgb(ui_theme::text_muted()))
            .child(quiet_dots(
                ("acp-working-dots", entry_ix),
                mark,
                animate_ambient,
            ))
            .child(working_status_label(&elapsed))
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
        let group_accent_u32 = ui_theme::text_muted();
        let group_accent = gpui::rgb(group_accent_u32);
        // 有工具在跑仍用 spinner。只有思考、没有进行中的工具或子 agent 时换成安静的点。
        let thinking = group.active
            && group.active_tool_kind.is_none()
            && group.agents == 0
            && group.active_agents == 0;
        h_flex()
            .id(("acp-process-group", group.first))
            .debug_selector(move || format!("ACP_PROCESS_GROUP_{}", group.first))
            .w_full()
            .h(ui_theme::height::sm())
            .flex_shrink_0()
            .gap_1p5()
            .items_center()
            .cursor_pointer()
            .hover(|row| row.opacity(0.78))
            .when(group.active && !thinking, |row| {
                row.child(ambient_spinner(
                    ("acp-process-group-spinner", group.first),
                    group_accent.into(),
                    animate_ambient,
                ))
            })
            .when(thinking, |row| {
                row.child(quiet_dots(
                    ("acp-process-think", group.first),
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
        live_elapsed: Option<u64>,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let start = run.start;
        let open = self.tool_run_is_open(start);
        let count = run.indices.len();
        let faint = gpui::rgb(ui_theme::text_faint());
        let owner_cache = &self.layout_cache;
        let run_owner = run
            .indices
            .iter()
            .find_map(|&ix| match self.entries.get(ix) {
                Some(AcpEntry::ToolCall { id, .. }) => Some(super::super::lookup_owner(
                    &owner_cache.borrow().owner_ids,
                    id,
                )),
                _ => None,
            })
            .unwrap_or_else(|| gpui::SharedString::from(start.to_string()));
        let run_label = compact_tool_run_label(run.kind, count);
        let header = h_flex()
            .id(owner_id("acp-tool-run", &run_owner))
            .role(gpui::Role::Button)
            .aria_label(run_label.clone())
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
                    .child(run_label),
            )
            .when_some(live_elapsed, |header, elapsed_ms| {
                header.child(elapsed_suffix(elapsed_ms, muted))
            })
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                this.toggle_tool_run(start, cx);
            }));
        let mut col = v_flex()
            .id(owner_id("acp-tool-run-box", &run_owner))
            .w_full()
            .child(header);
        if open {
            for &ix in &run.indices {
                let Some((tool_owner, can_expand, title)) =
                    self.entries.get(ix).and_then(|entry| {
                        let AcpEntry::ToolCall {
                            id,
                            title,
                            output,
                            children,
                            ..
                        } = entry
                        else {
                            return None;
                        };
                        Some((
                            super::super::lookup_owner(&owner_cache.borrow().owner_ids, id),
                            tool_output_has_content(output) || !children.is_empty(),
                            title.clone(),
                        ))
                    })
                else {
                    continue;
                };
                let headline = compact_tool_headline(run.kind, &title);
                let row = h_flex()
                    .id(owner_id("acp-tool-run-item", &tool_owner))
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
                            .child(headline.clone()),
                    );
                col = col.child(if can_expand {
                    self.tool_output_popover(
                        ix,
                        muted,
                        Button::new(owner_id("acp-tool-run-item-trigger", &tool_owner))
                            .text()
                            .w_full()
                            .justify_start()
                            .group(PEEK_ROW_GROUP)
                            .accessibility_label(headline)
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
        card: SubagentCard<'_>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let SubagentCard {
            index: i,
            id,
            title,
            status,
            output,
            children,
            has_expandable_content,
            default_expanded,
            card_expanded,
            failed,
            animate_ambient,
            muted,
            border,
            live_elapsed,
        } = card;
        let faint = gpui::rgb(ui_theme::text_faint());
        let red = gpui::rgb(ui_theme::red());
        let ink = if failed { red.into() } else { muted };
        // 转后台的派发调用一启动就返回，调用本身是「完成」。子智能体还在不在跑，以宿主里
        // 挂着这次调用的后台任务为准：Pi 没了任务也跟着结束，不会一直转。
        let background_running = background_task_running_for(&self.background_tasks, id);
        let running = matches!(status, ToolCallStatus::InProgress | ToolCallStatus::Pending)
            || smelt_model::acp_chat::has_unfinished_tool_call(children)
            || background_running;
        let roster = smelt_model::acp_chat::is_subagent_roster(children);
        let mut headline = smelt_model::acp_chat::subagent_card_title(title, children);
        if headline.is_empty() {
            headline = "子代理".to_string();
        }
        let mut progress = smelt_model::acp_chat::subagent_progress_text(status, children);
        if background_running && progress.is_empty() {
            progress = "后台运行中".to_string();
        }
        let progress_failed = failed || progress.contains("失败");
        let show_fail_label = failed && !progress.contains("失败");
        let show_header_spinner = running && !(card_expanded && roster);
        let show_check = !running && !progress_failed && progress.is_empty();
        let owner = super::super::lookup_owner(&self.layout_cache.borrow().owner_ids, id);
        let id_for_toggle = id.to_string();
        let mut header = h_flex()
            .id(owner_id("acp-subagent-toggle", &owner))
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
                ambient_spinner(
                    owner_id("acp-subagent-spin", &owner),
                    muted,
                    animate_ambient,
                )
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
        header = header
            .child(status_el)
            .when_some(live_elapsed, |header, elapsed_ms| {
                header.child(elapsed_suffix(elapsed_ms, muted))
            });
        let mut card = v_flex()
            .id(owner_id("acp-subagent-card", &owner))
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
        for child in children {
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
                    let nested_running = smelt_model::acp_chat::has_unfinished_tool_call(nested);
                    let ink = if failed { red.into() } else { muted };
                    let mut name = smelt_model::acp_chat::subagent_visible_title(title);
                    if name.is_empty() {
                        name = "子代理".to_string();
                    }
                    let has_nested = !nested.is_empty() || tool_output_has_content(output);
                    let expanded = self.tool_card_is_expanded(id, has_nested, false);
                    let spinning = matches!(status, ToolCallStatus::InProgress) || nested_running;
                    let owner =
                        super::super::lookup_owner(&self.layout_cache.borrow().owner_ids, id);
                    let glyph = subagent_child_status_glyph(
                        owner_id("acp-subagent-child-spin", &owner),
                        spinning,
                        failed,
                        matches!(status, ToolCallStatus::Completed) && !spinning,
                        animate_ambient,
                        ink,
                        faint.into(),
                    );
                    let mut row = h_flex()
                        .id(owner_id("acp-subagent-child", &owner))
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
        let (tool_id, kind, output, children, diffs, images) = match self.entries.get(entry_ix) {
            Some(AcpEntry::ToolCall {
                id,
                kind,
                output,
                children,
                ..
            }) => (
                super::super::lookup_owner(&self.layout_cache.borrow().owner_ids, id),
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
        Popover::new(owner_id("acp-tool-output-pop", &tool_id))
            .anchor(Anchor::BottomLeft)
            .appearance(false)
            .p_0()
            .trigger(trigger)
            .content(move |_, _, _| {
                render_tool_output_popover_body(ToolOutputPopoverBody {
                    tool_id: tool_id.clone(),
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
        live_elapsed: Option<u64>,
        animate_ambient: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let expanded = self.expanded_thoughts.contains(&entry_ix);
        let mut preview = progress_summary(text);
        if !in_process && preview.is_empty() && live_elapsed.is_some() {
            preview = "正在想".to_string();
        }
        let has_details = progress_has_details(text);
        let settled = if !in_process && live_elapsed.is_none() {
            settled_thought_elapsed(
                &self.entries,
                entry_ix,
                self.loaded_entries_offset,
                &self.turn_timings,
            )
        } else {
            None
        };
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
                if thought && live_elapsed.is_some() {
                    quiet_dots(("acp-thought-dots", entry_ix), muted, animate_ambient)
                } else if has_details && thought {
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
            } else if live_elapsed.is_some() {
                quiet_dots(("acp-thought-dots", entry_ix), muted, animate_ambient)
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
                    .child(preview.clone()),
            )
            .when_some(live_elapsed, |row, elapsed_ms| {
                row.child(elapsed_suffix(elapsed_ms, muted))
            })
            .when_some(settled, |row, elapsed_ms| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(muted)
                        .child(settled_thought_label(elapsed_ms)),
                )
            });
        if has_details {
            row = row
                .role(gpui::Role::Button)
                .aria_label(preview)
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
                    let open = if this.expanded_thoughts.remove(&entry_ix) {
                        false
                    } else {
                        this.expanded_thoughts.insert(entry_ix);
                        true
                    };
                    this.thought_reveals.insert(
                        entry_ix,
                        ThoughtReveal {
                            open,
                            started: std::time::Instant::now(),
                        },
                    );
                    this.list_state
                        .remeasure_items(entry_ix..entry_ix.saturating_add(1));
                    cx.notify();
                }));
        }
        let reveal = self.thought_reveal_progress(entry_ix);
        let mut col = v_flex().w_full().min_w_0().child(row);
        if has_details && (reveal.is_some() || expanded) {
            let details = div()
                .min_w_0()
                .pl(px(if in_process { 24. } else { 8. }))
                .pr_2()
                .pt_2()
                .pb_2()
                .text_xs()
                .text_color(muted)
                .child(self.retained_markdown(
                    if in_process {
                        "acp-progress-md"
                    } else {
                        "acp-analysis-md"
                    },
                    entry_ix,
                    0,
                    cached_entry_markdown(
                        &self.rendered_markdown,
                        entry_ix,
                        entry,
                        FileLinks::of(&self.runtime, self.cwd.as_deref()),
                    ),
                    cx,
                ))
                .into_any_element();
            col = if let Some(progress) = reveal {
                col.child(MotionReveal::new(
                    ("acp-thought-reveal", entry_ix),
                    progress,
                    details,
                ))
            } else {
                col.child(details)
            };
        }
        col.into_any_element()
    }

    /// 控制台和文件内容。短的整段可选；长的默认前几行，展开后是固定高度的行视口。
    /// 视口里只构建看得到的行，全文留在复制按钮上。
    fn render_plain_tool_log(&self, log: PlainToolLog<'_>, cx: &Context<Self>) -> gpui::AnyElement {
        let PlainToolLog {
            entry_ix,
            part_ix,
            body,
            tool_id,
            muted,
        } = log;
        let owner = super::super::lookup_owner(&self.layout_cache.borrow().owner_ids, tool_id);
        let preview = tool_log_preview(body);
        let expanded = self.expanded_tools.contains(tool_id);
        let mode = tool_log_mode(preview.total, expanded);
        let font = smelt_model::font_config::font_family();
        let body_el = match mode {
            ToolLogMode::Inline => {
                selectable_plain_text(owner_part("acp-tool-log-text", &owner, part_ix), body)
                    .text_xs()
                    .text_color(muted)
                    .font_family(font)
                    .into_any_element()
            }
            ToolLogMode::Preview => selectable_plain_text(
                owner_part("acp-tool-log-text", &owner, part_ix),
                &preview.shown,
            )
            .text_xs()
            .text_color(muted)
            .font_family(font)
            .into_any_element(),
            ToolLogMode::Viewport => tool_log_viewport(ToolLogViewport {
                list_id: owner_part("acp-tool-log", &owner, part_ix),
                box_id: owner_part("acp-tool-log-box", &owner, part_ix),
                debug_marker: format!("ACP_TOOL_LOG_{tool_id}_{part_ix}"),
                line_marker_prefix: None,
                body,
                muted,
            }),
        };
        let total = preview.total;
        let copy_text = body.to_string();
        v_flex()
            .pl_5()
            .pt_1()
            .pb_1()
            .gap_1()
            .w_full()
            .min_w_0()
            .child(body_el)
            .when(mode != ToolLogMode::Inline, |block| {
                let key = tool_id.to_string();
                let toggle_label = if expanded {
                    "收起".to_string()
                } else {
                    format!("展开全部 {total} 行")
                };
                let toggle_view = cx.entity().downgrade();
                let toggle_key = key.clone();
                block.child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(
                            div()
                                .id(owner_part("acp-tool-toggle", &owner, part_ix))
                                .role(gpui::Role::Button)
                                .aria_label(toggle_label.clone())
                                .text_xs()
                                .text_color(gpui::rgb(ui_theme::text_faint()))
                                .cursor_pointer()
                                .hover(|row| row.opacity(0.8))
                                .child(toggle_label)
                                .on_mouse_down(
                                    gpui::MouseButton::Left,
                                    cx.listener(move |this, _ev, _window, cx| {
                                        if !this.expanded_tools.remove(&key) {
                                            this.expanded_tools.insert(key.clone());
                                        }
                                        this.list_state
                                            .remeasure_items(entry_ix..entry_ix.saturating_add(1));
                                        cx.stop_propagation();
                                        cx.notify();
                                    }),
                                )
                                .on_a11y_action(gpui::AccessibleAction::Click, move |_, _, cx| {
                                    let _ = toggle_view.update(cx, |this, cx| {
                                        if !this.expanded_tools.remove(&toggle_key) {
                                            this.expanded_tools.insert(toggle_key.clone());
                                        }
                                        this.list_state
                                            .remeasure_items(entry_ix..entry_ix.saturating_add(1));
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(self.render_copy_button(
                            CopySite::ToolLog {
                                tool_id: tool_id.to_string(),
                                part_ix,
                            },
                            copy_text,
                            "复制全部",
                            cx,
                        )),
                )
            })
            .into_any_element()
    }
}

/// 控制台、文件内容这类纯文本日志。短输出整段画出，长输出走行视口。
struct PlainToolLog<'a> {
    entry_ix: usize,
    part_ix: usize,
    body: &'a str,
    tool_id: &'a str,
    muted: gpui::Hsla,
}

/// 长日志的行视口。高度封顶，滚轮在视口里走完全文，布局只保留看得到的行。
struct ToolLogViewport<'a> {
    list_id: gpui::ElementId,
    box_id: gpui::ElementId,
    debug_marker: String,
    /// 行标记前缀。测试用来确认滚过预览范围后，后面的行真的画出来了。
    line_marker_prefix: Option<String>,
    body: &'a str,
    muted: gpui::Hsla,
}

fn tool_log_viewport(viewport: ToolLogViewport<'_>) -> gpui::AnyElement {
    let ToolLogViewport {
        list_id,
        box_id,
        debug_marker,
        line_marker_prefix,
        body,
        muted,
    } = viewport;
    let lines = std::rc::Rc::new(split_log_lines(body));
    let widest = widest_log_line_index(&lines);
    let height = tool_log_viewport_height(lines.len());
    let row_lines = lines.clone();
    let row_font = smelt_model::font_config::font_family();
    let list = uniform_list(list_id, lines.len(), move |range, _window, _app| {
        let font = row_font.clone();
        let line_marker_prefix = line_marker_prefix.clone();
        range
            .map(|index| {
                let mut row = div()
                    .h(px(TOOL_LOG_LINE_HEIGHT_PX))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_size(px(12.))
                    .line_height(px(TOOL_LOG_LINE_HEIGHT_PX))
                    .text_color(muted)
                    .font_family(font.clone());
                if let Some(prefix) = line_marker_prefix.as_ref() {
                    let marker = format!("{prefix}{index}");
                    row = row.debug_selector(move || marker);
                }
                row.child(row_lines[index].clone())
            })
            .collect::<Vec<_>>()
    })
    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
    .with_width_from_item(Some(widest))
    .size_full();
    div()
        .id(box_id)
        .debug_selector(move || debug_marker)
        .h(px(height))
        .w_full()
        .overflow_hidden()
        .child(list)
        .into_any_element()
}

/// 点开后的工具输出。长日志不铺全文，走固定高度的行视口，在卡片里滚动。
struct ToolOutputPopoverBody<'a> {
    tool_id: gpui::SharedString,
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
        tool_id,
        kind,
        output,
        diffs,
        images,
        children,
        muted,
        border,
    } = body;
    // 滚动容器必须是 block。v_flex 会把子项压进 max_h，content 高度等于视口，
    // 滚轮加在 offset 上也会被钳回 0。卡片本身不滚时，长日志的行视口自己滚。
    let mut col = v_flex().w_full().p_2().gap_1();
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
                                owner_part("acp-tool-pop-diff-path", &tool_id, part_ix),
                                path,
                            )
                            .text_xs()
                            .text_color(muted),
                        )
                        .children(cached.map(|diff| {
                            render_diff_lines(
                                &diff.lines,
                                owner_part("acp-diff", &tool_id, part_ix),
                                border,
                                muted,
                            )
                        })),
                )
            }
            ToolOutputPart::Text(text) if !text.trim().is_empty() => {
                let body = strip_code_fence(text);
                col.child(popover_tool_text(&tool_id, part_ix, kind, body, muted))
            }
            ToolOutputPart::Image(_) => {
                match images
                    .and_then(|parts| parts.get(part_ix))
                    .and_then(Option::as_ref)
                {
                    // hover 预览没有 `Context<AcpView>`，只出缩略图，不接点击大图；
                    // 解码交给展开卡片路径触发（缓存按图片 id 全局共享）。
                    Some(image) => col.child(image_thumb_element(image)),
                    None => col,
                }
            }
            ToolOutputPart::Text(_) => col,
            ToolOutputPart::Terminal { output, .. } if !output.trim().is_empty() => col.child(
                popover_plain_log(&tool_id, part_ix, "terminal", output, muted),
            ),
            ToolOutputPart::Terminal { .. } => col,
        };
    }
    if !children.is_empty() {
        col = col.child(render_nested_transcript(children, muted));
    }
    div()
        .id(owner_id("acp-tool-output-pop-body", &tool_id))
        .min_w(px(280.))
        .max_w(px(520.))
        .max_h(px(360.))
        .overflow_y_scroll()
        .bg(ui_theme::glass_floating())
        .border_1()
        .border_color(ui_theme::card_stroke())
        .rounded(px(8.))
        .child(col)
        .into_any_element()
}

fn popover_tool_text(
    tool_id: &gpui::SharedString,
    part_ix: usize,
    kind: ToolKind,
    body: &str,
    muted: gpui::Hsla,
) -> gpui::AnyElement {
    if matches!(kind, ToolKind::Other) {
        popover_markdown_log(tool_id, part_ix, body, muted)
    } else {
        popover_plain_log(tool_id, part_ix, "text", body, muted)
    }
}

/// 短输出整段可选。超过预览行数后改走行视口，滚轮能走到最后一行。
fn popover_plain_log(
    tool_id: &gpui::SharedString,
    part_ix: usize,
    slot: &'static str,
    body: &str,
    muted: gpui::Hsla,
) -> gpui::AnyElement {
    let preview = tool_log_preview(body);
    let text_name = match slot {
        "terminal" => "acp-tool-pop-terminal",
        _ => "acp-tool-pop-text",
    };
    let box_name = match slot {
        "terminal" => "acp-tool-pop-terminal-box",
        _ => "acp-tool-pop-text-box",
    };
    let copy_name = match slot {
        "terminal" => "acp-tool-pop-terminal-copy",
        _ => "acp-tool-pop-text-copy",
    };
    if !preview.truncated() {
        return selectable_plain_text(owner_part(text_name, tool_id, part_ix), body)
            .text_xs()
            .text_color(muted)
            .font_family(smelt_model::font_config::font_family())
            .into_any_element();
    }
    let total = preview.total;
    let copy = body.to_string();
    v_flex()
        .w_full()
        .gap_1()
        .child(tool_log_viewport(ToolLogViewport {
            list_id: owner_part(text_name, tool_id, part_ix),
            box_id: owner_part(box_name, tool_id, part_ix),
            debug_marker: format!("ACP_TOOL_POP_LOG_{slot}_{tool_id}_{part_ix}"),
            line_marker_prefix: Some(format!("ACP_TOOL_POP_LINE_{tool_id}_{part_ix}_")),
            body,
            muted,
        }))
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui::rgb(ui_theme::text_faint()))
                        .child(format!("共 {total} 行")),
                )
                .child(
                    Clipboard::new(owner_part(copy_name, tool_id, part_ix))
                        .value(copy)
                        .icon_action("复制全部"),
                ),
        )
        .into_any_element()
}

fn popover_markdown_log(
    tool_id: &gpui::SharedString,
    part_ix: usize,
    body: &str,
    muted: gpui::Hsla,
) -> gpui::AnyElement {
    let preview = tool_log_preview(body);
    let view = smelt_ui::markdown_mermaid::markdown_view_clickable(
        owner_part("acp-tool-pop-md", tool_id, part_ix),
        body.to_string(),
    )
    .text_xs()
    .text_color(muted);
    if !preview.truncated() {
        return view.into_any_element();
    }
    div()
        .id(owner_part("acp-tool-pop-md-scroll", tool_id, part_ix))
        .w_full()
        .max_h(px(tool_log_viewport_height(preview.total)))
        .overflow_y_scroll()
        .child(view)
        .into_any_element()
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
        let visible = smelt_model::acp_chat::subagent_visible_title(title);
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

fn elapsed_suffix(elapsed_ms: u64, color: gpui::Hsla) -> gpui::Div {
    div()
        .flex_shrink_0()
        .text_xs()
        .font_features(ui_theme::tabular_figures())
        .text_color(color)
        .child(format!("· {}", format_duration(elapsed_ms)))
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

/// 缩略图元素：降采样 + 按图片 id 缓存 + canvas contain 绘制。历史上大图/长截图
/// 在调用点反复出 bug（撑破屏幕、只露顶部、撞 sprite atlas 上限），已发消息、
/// 工具输出图片统一收敛到这一条路径，不再各写一份。
fn image_thumb_element(image: &std::sync::Arc<gpui::Image>) -> gpui::AnyElement {
    // 栅格图只画降采样预览。未完成时是占位，不把原图交给 `img()`。
    smelt_ui::image::thumb_element(image, px(160.), px(280.), 8.0)
}

/// 可点开大图的缩略图卡片（已发消息里的图、工具返回的图共用）。
fn render_clickable_image_thumb(
    image: std::sync::Arc<gpui::Image>,
    id: impl Into<gpui::ElementId>,
    border: gpui::Hsla,
    cx: &Context<AcpView>,
) -> gpui::AnyElement {
    // 每帧调用是安全的：已在途、已完成或已失败时不会再启动任务。
    smelt_ui::image::ensure_preview(&image, cx);
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
