//! ACP 消息流虚拟列表。

use super::*;
use gpui_base::motion::MotionReveal;
use rust_i18n::t;
use smelt_plugin_api::model::acp_chat::{SubagentCardTitle, SubagentProgress};
use smelt_plugin_api::ui::kit::TYPE;
use smelt_plugin_api::ui::truncated_line::truncated_line;

fn owner_id(name: &'static str, owner: &gpui::SharedString) -> gpui::ElementId {
    gpui::ElementId::from((gpui::ElementId::from(name), owner.clone()))
}

fn owner_part(name: &'static str, owner: &gpui::SharedString, part: usize) -> gpui::ElementId {
    gpui::ElementId::from((gpui::ElementId::named_usize(name, part), owner.clone()))
}

/// 用户气泡外面包一层右键菜单（[`message_menu`]）：能分叉时给分叉。
///
/// 限宽写在包的这层上：它才是行里的弹性项，气泡铺满它。宽度写在气泡上的话，百分比
/// 会按这层算，而这层的宽又是按气泡量出来的。最小宽度给 markdown 列表留位置：
/// gpui-base 的 markdown 列表块（ol/ul）内部用 `w_full()`/`flex_1()` 排布「序号 + 正文」，
/// 收缩到内容大小时短列表会被测成只有序号那么宽，正文被 `overflow_hidden()` 裁掉。
fn user_bubble_with_menu(
    id: impl Into<gpui::ElementId>,
    bubble: gpui::Stateful<gpui::Div>,
    view: Entity<ConversationView>,
    index: usize,
    fork: Option<SharedString>,
) -> MenuTrigger {
    MenuTrigger::context(id, bubble, move |window, cx| {
        let fork = fork.clone().map(|label| {
            let view = view.clone();
            MenuItem::new(label)
                .icon(IconName::GitBranch)
                .on_click(move |_event, _window, cx| {
                    view.update(cx, |this, cx| this.fork_from_user_message(index, cx));
                })
        });
        message_menu(&view, index, fork, window, cx)
    })
    .max_w(gpui::relative(0.72))
    .min_w(gpui::px(160.))
}

/// 用户消息气泡。外面要套 [`user_bubble_with_menu`]，宽度由那层定。
fn user_message_bubble(id: impl Into<gpui::ElementId>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_4()
        .py_2p5()
        .rounded(ui_theme::bubble_radius())
        .bg(ui_theme::overlay(0x14))
        .hover(|bubble| bubble.bg(ui_theme::overlay(0x1c)))
        .type_style(TYPE.body)
}

/// 只包刚发送的那一条。nonce 进元素 id，虚拟列表把行卸掉再装上时，过了时钟就不再播。
/// Div 没有 scale，用透明度和 6px 下落代替从 0.92 缩放进来。曲线和别的入场一样不过冲
/// （见 docs/ui-design-language.md 第 7 节「动效」）：落过头再弹回只是装饰，交代不了什么。
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
            Animation::new(std::time::Duration::from_millis(ENTER_MS)).with_easing(ease_snap),
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

/// 线 + 字 + 线的分隔条。不用 `Separator::label`：它给字垫一块窗口底色，
/// 会话有背景图时会露出一块色块。
fn labeled_divider(
    label: gpui::SharedString,
    line: gpui::Hsla,
    muted: gpui::Hsla,
) -> gpui::AnyElement {
    div()
        .flex()
        .items_center()
        .w_full()
        .gap_2()
        .my_1()
        .child(div().flex_1().h(px(1.)).bg(line))
        .child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child(label),
        )
        .child(div().flex_1().h(px(1.)).bg(line))
        .into_any_element()
}

/// 对话之间的消息在事件行里只露第一行，悬停看全文。
fn peer_text_preview(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string()
}

/// 事件行里的一行预览，悬停弹出完整正文。悬停只是快捷方式：行尾箭头就地展开，
/// 键盘也够得着。
fn peer_preview_hover(
    id: gpui::ElementId,
    body_id: gpui::ElementId,
    preview: String,
    body: gpui::AnyElement,
    muted: gpui::Hsla,
) -> gpui::AnyElement {
    div()
        .min_w_0()
        .overflow_hidden()
        .child(
            smelt_plugin_api::ui::hover_card::HoverCard::new(id)
                .trigger(
                    div()
                        .max_w(px(360.))
                        .overflow_hidden()
                        .text_color(muted)
                        .child(truncated_line(preview)),
                )
                .child(
                    div()
                        .id(body_id)
                        .w(px(480.))
                        .max_h(px(360.))
                        .overflow_y_scroll()
                        .p_2()
                        .child(body),
                ),
        )
        .into_any_element()
}

/// 事件行的内容：小字、次要色，一行排开。
fn event_line(muted: gpui::Hsla) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .max_w_full()
        .min_w_0()
        .gap_1()
        .type_style(TYPE.text_xsmall)
        .text_color(muted)
}

/// 对话之间互发的消息居中一行，和靠右的用户气泡、靠左的助手回答分开。
fn centered_event(row: gpui::Div) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .w_full()
        .justify_center()
        .py_1()
        .child(row)
}

/// 后台任务、后台子智能体结束的状态行靠左，和过程里的工具行对齐：它们是这段对话
/// 自己干的活，不是别人发来的话。
fn leading_event(row: gpui::Div) -> gpui::Div {
    div().flex().items_center().w_full().py_1().child(row)
}

struct SubagentCard<'a> {
    index: usize,
    id: &'a str,
    title: &'a str,
    status: ToolCallStatus,
    output: &'a [ToolOutputPart],
    children: &'a [AcpEntry],
    /// Pi 子智能体派发里的子智能体。非空时画成派发卡片。
    subagents: &'a [smelt_plugin_api::model::acp_chat::SubagentRun],
    has_expandable_content: bool,
    default_expanded: bool,
    card_expanded: bool,
    failed: bool,
    animate_ambient: bool,
    muted: gpui::Hsla,
    border: gpui::Hsla,
}

impl ConversationView {
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
    ) -> smelt_plugin_api::ui::markdown_mermaid::TextView {
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
                let state = cx.new(|cx| {
                    smelt_plugin_api::ui::markdown_mermaid::TextViewState::markdown(&text, cx)
                });
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
        smelt_plugin_api::ui::markdown_mermaid::chat_markdown_view(
            &state,
            file_links(&self.runtime, self.cwd.as_deref()),
        )
    }

    pub(super) fn render_message_list(
        &self,
        animate_ambient: bool,
        clear_capsule: bool,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
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
                let muted: gpui::Hsla = gpui::rgb(ui_theme::muted()).into();
                let border: gpui::Hsla = ui_theme::hair().into();
                let presentation = conversation_layout.get(i).copied().unwrap_or_default();
                let empty_body = this.entries.get(i).is_some_and(is_empty_assistant_body);
                if empty_body && i + 1 < this.entries.len() {
                    return div().h(px(0.)).w_full().overflow_hidden().into_any_element();
                }
                let final_answer = presentation.final_answer;
                let turn_fold = presentation.turn_fold;
                let fold_expanded = turn_fold
                    .is_some_and(|fold| this.expanded_turn_folds.contains(&fold.first));
                // 展开的回合摘要挂在折叠范围的第一行上面。
                let fold_header_here = turn_fold.filter(|fold| fold_expanded && fold.first == i);
                let process_group = presentation.process_group;
                let process_expanded = process_group
                    .is_some_and(|group| this.expanded_process_groups.contains(&group.first));
                let collapsed_header = match (turn_fold, process_group) {
                    (Some(fold), _) if !fold_expanded => {
                        Some((fold.first, None))
                    }
                    (_, Some(group)) if !process_expanded => Some((group.first, Some(group))),
                    _ => None,
                };
                if let Some((first, group)) = collapsed_header {
                    if first != i {
                        return this.tail_only_row(i, &conversation_layout, animate_ambient, cx);
                    }
                    // 折叠态只构建摘要，不先渲染一遍随后会被丢弃的首个工具卡。
                    // Read 的大输出或 Edit 的 diff 因此不会在静态重绘时白做解析。
                    let header = match (group, turn_fold) {
                        (Some(group), _) => {
                            this.render_segment_head(group, false, animate_ambient, muted, cx)
                        }
                        (None, Some(fold)) => this.render_turn_fold_header(fold, false, muted, cx),
                        (None, None) => div().into_any_element(),
                    };
                    let tail = this.render_turn_tail(i, &conversation_layout, animate_ambient, cx);
                    return div().flex().items_center()
                        .w_full()
                        .justify_center()
                        .px_4()
                        .pb(px(12.))
                        .child(
                            div().flex().flex_col()
                                .w_full()
                                .max_w(ui_theme::conversation_max_width())
                                .gap_1()
                                .when_some(fold_header_here, |col, fold| {
                                    col.child(this.render_turn_fold_header(fold, true, muted, cx))
                                })
                                .child(header)
                                .when_some(tail, |col, tail| col.child(div().pt_2().child(tail))),
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
                        return this.tail_only_row(i, &conversation_layout, animate_ambient, cx);
                    }
                }
                // 段首行要补 diff 缓存（可变借用），得在借出 `entry` 之前画好。
                let segment_head = process_group
                    .filter(|group| process_expanded && group.first == i)
                    .map(|group| {
                        this.render_segment_head(group, true, animate_ambient, muted, cx)
                    });
                let entry = &this.entries[i];
                let mut el: gpui::AnyElement = match entry {
                    // agent 回显的「中断」标记不是用户说的话，别套成气泡——
                    // 那会读成「用户发了一条叫 [Request interrupted...] 的消息」。
                    AcpEntry::User(text) if is_interrupt_marker(text) => {
                        labeled_divider(t!("divider.interrupted").into(), border, muted)
                    }
                    // 用户气泡右对齐限宽（对齐设计稿）：整行铺满时跟 agent 正文
                    // 混成一片，看不出谁在说话。
                    AcpEntry::User(_) => {
                        let menu_view = view.clone();
                        let can_edit = this.supports_rewind && !this.has_active_turn();
                        let show_edit = latest_user_message_index == Some(i) && can_edit;
                        let show_fork_edit = latest_user_message_index != Some(i) && can_edit;
                        div().flex().items_center()
                            .w_full()
                            .justify_end()
                            .gap_2()
                            .when(show_edit, |row| {
                                row.child(
                                    Button::new(("acp-edit-message", i))
                                        .icon(IconName::Undo)
                                        .quiet()
                                        .small()
                                        .icon_action(t!("edit.resend"))
                                        .on_click(cx.listener(
                                            move |this, _ev, _window, cx| {
                                                this.rewind_to_message(i, cx);
                                            },
                                        )),
                                )
                            })
                            .children(this.menu_failure_mark(i, cx))
                            .child(user_bubble_with_enter(
                                this.user_enter_nonce_for(i),
                                i,
                                user_bubble_with_menu(
                                    ("acp-user-message-menu", i),
                                    user_message_bubble(("acp-user-message", i))
                                        .debug_selector(move || format!("ACP_USER_MESSAGE_{i}"))
                                        .child(this.retained_markdown(
                                            "acp-user-md",
                                            i,
                                            0,
                                            cached_entry_markdown(&this.rendered_markdown, i, entry),
                                            cx,
                                        )),
                                    menu_view,
                                    i,
                                    show_fork_edit.then(|| t!("fork.here").into()),
                                ),
                            ))
                            .into_any_element()
                    }
                    AcpEntry::UserWithImages { text, images } => {
                        let menu_view = view.clone();
                        let can_edit = this.supports_rewind && !this.has_active_turn();
                        let show_edit = latest_user_message_index == Some(i) && can_edit;
                        let show_fork_edit = latest_user_message_index != Some(i) && can_edit;
                        let mut content = div().flex().flex_col().gap_2();
                        if !text.trim().is_empty() {
                            content = content.child(
                                this.retained_markdown(
                                    "acp-user-images-md",
                                    i,
                                    0,
                                    cached_entry_markdown(&this.rendered_markdown, i, entry),
                                    cx,
                                ),
                            );
                        }
                        let mut image_strip = div().flex().items_center().gap_2().flex_wrap();
                        for image_ix in 0..images.len() {
                            if let Some(image) = this.rendered_images.get(&(i, image_ix)).cloned() {
                                image_strip = image_strip.child(render_clickable_image_thumb(
                                    image,
                                    ("acp-sent-image", image_ix),
                                    border,
                                    cx,
                                ));
                            }
                        }
                        div().flex().items_center()
                            .w_full()
                            .justify_end()
                            .gap_2()
                            .when(show_edit, |row| {
                                row.child(
                                    Button::new(("acp-edit-message", i))
                                        .icon(IconName::Undo)
                                        .quiet()
                                        .small()
                                        .icon_action(t!("edit.resend_images"))
                                        .on_click(cx.listener(
                                            move |this, _ev, _window, cx| {
                                                this.rewind_to_message(i, cx);
                                            },
                                        )),
                                )
                            })
                            .children(this.menu_failure_mark(i, cx))
                            .child(user_bubble_with_enter(
                                this.user_enter_nonce_for(i),
                                i,
                                user_bubble_with_menu(
                                    ("acp-user-images-message-menu", i),
                                    user_message_bubble(("acp-user-images-message", i))
                                        .child(content.child(image_strip)),
                                    menu_view,
                                    i,
                                    show_fork_edit.then(|| t!("fork.here_images").into()),
                                ),
                            ))
                            .into_any_element()
                    }
                    // 过程组只收思考和工具，普通正文始终以完整 Markdown 显示。
                    AcpEntry::Assistant { text, thought: false } if text.trim().is_empty() => {
                        div().h(px(0.)).w_full().overflow_hidden().into_any_element()
                    }
                    // 还没写出字的思考不占行，「正在想」由末尾的「现在」说。
                    AcpEntry::Assistant { text, thought: true } if text.trim().is_empty() => {
                        div().h(px(0.)).w_full().overflow_hidden().into_any_element()
                    }
                    AcpEntry::Assistant { text, thought: true } if process_group.is_some() => this
                        .render_progress_entry(
                            i,
                            entry,
                            text,
                            true,
                            false,
                            muted,
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
                        presentation.show_settled_thought_elapsed,
                        muted,
                        cx,
                    ),
                    AcpEntry::Assistant {
                        text,
                        thought: false,
                    } => {
                        let answer = div().flex().flex_col()
                            .id(("acp-assistant-message", i))
                            .w_full()
                            .min_w_0()
                            .type_style(TYPE.body)
                            .text_color(gpui::rgb(ui_theme::text()))
                            .child(this.retained_markdown(
                                "acp-md",
                                i,
                                0,
                                cached_entry_markdown(&this.rendered_markdown, i, entry),
                                cx,
                            ))
                            .when(final_answer, |col| {
                                col.child(
                                    div().flex().items_center()
                                        .pt_1()
                                        .gap_1()
                                        .child(this.render_copy_button(
                                            CopySite::Answer(i),
                                            text.clone(),
                                            t!("copy.answer"),
                                            cx,
                                        ))
                                        .when(
                                            smelt_plugin_api::model::session_handoff::live_fork_is_available(
                                                this.agent,
                                            ) && this.acp_session_id.is_some(),
                                            |row| {
                                                row.child(
                                                    Button::new(("acp-fork-answer", i))
                                                        .icon(IconName::GitBranch)
                                                        .quiet()
                                                        .small()
                                                        .icon_action(t!("fork.title"))
                                                        .on_click(cx.listener(
                                                            move |this, _ev, _window, cx| {
                                                                this.fork_from_answer(i, cx);
                                                            },
                                                        )),
                                                )
                                            },
                                        ),
                                )
                            });
                        let menu_view = view.clone();
                        MenuTrigger::context(
                            ("acp-assistant-message-menu", i),
                            answer.children(this.menu_failure_mark(i, cx)),
                            move |window, cx| message_menu(&menu_view, i, None, window, cx),
                        )
                        .w_full()
                        .min_w_0()
                        .into_any_element()
                    }
                    AcpEntry::ToolCall {
                        title,
                        status,
                        output,
                        ..
                    } if is_task_completion_tool_title(title) => {
                        let (status_label, status_color) = match status {
                            ToolCallStatus::Pending => {
                                (t!("completion.pending"), gpui::rgb(ui_theme::muted()))
                            }
                            ToolCallStatus::InProgress => {
                                (t!("completion.in_progress"), gpui::rgb(ui_theme::run()))
                            }
                            ToolCallStatus::Completed => {
                                (t!("completion.done"), gpui::rgb(ui_theme::ok()))
                            }
                            ToolCallStatus::Failed => {
                                (t!("completion.failed"), gpui::rgb(ui_theme::bad()))
                            }
                        };
                        let summary = completion_summary_text(output);
                        let has_summary = !summary.trim().is_empty();
                        let body = if has_summary {
                            summary
                        } else {
                            t!("completion.default").into_owned()
                        };
                        let body_markdown = if !has_summary {
                            body.clone().into()
                        } else {
                            cached_entry_markdown(&this.rendered_markdown, i, entry)
                        };
                        let mut answer = div().flex().flex_col()
                            .id(("acp-completion-message", i))
                            .w_full()
                            .min_w_0()
                            .gap_1()
                            .type_style(TYPE.body)
                            .when(!final_answer, |col| col.text_color(muted))
                            .child(
                                div().flex().items_center()
                                    .gap_2()
                                    .child(
                                        Icon::new(IconName::Check)
                                            .size(px(14.))
                                            .text_color(status_color),
                                    )
                                    .child(
                                        div()
                                            .type_style(TYPE.text_xsmall_medium)
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
                            ));
                        if final_answer {
                            answer = answer.child(
                                div().flex().items_center()
                                    .pt_1()
                                    .gap_1()
                                    .child(this.render_copy_button(
                                        CopySite::Completion(i),
                                        body.clone(),
                                        t!("copy.completion"),
                                        cx,
                                    )),
                            );
                        }
                        let menu_view = view.clone();
                        MenuTrigger::context(
                            ("acp-completion-message-menu", i),
                            answer.children(this.menu_failure_mark(i, cx)),
                            move |window, cx| message_menu(&menu_view, i, None, window, cx),
                        )
                        .w_full()
                        .min_w_0()
                        .into_any_element()
                    }
                    AcpEntry::ToolCall {
                        id,
                        title,
                        kind,
                        status,
                        output,
                        children,
                        subagents,
                        peer,
                    } => {
                        let owner = super::super::lookup_owner(&this.layout_cache.borrow().owner_ids, id);
                        let failed = matches!(status, ToolCallStatus::Failed);
                        let status_label = SharedString::from(match status {
                            ToolCallStatus::Pending => t!("tool.pending"),
                            ToolCallStatus::InProgress => t!("tool.in_progress"),
                            ToolCallStatus::Completed => t!("tool.done"),
                            ToolCallStatus::Failed => t!("tool.failed"),
                        });

                        let has_pending_permission =
                            active_permission_tool_id.as_deref() == Some(id.as_str());
                        let has_nested_transcript = !children.is_empty();
                        // Pi 子智能体派发：过程在各自的子会话里，卡片只占一行；并行时能展开成
                        // 每个子智能体一行。
                        let is_pi_dispatch = !subagents.is_empty();
                        let is_subagent = is_pi_dispatch
                            || matches!(kind, ToolKind::Collaborate)
                            || has_nested_transcript;
                        let has_expandable_content = if is_pi_dispatch {
                            // 几个子智能体时，标题行能收起下面那几行；点行打开侧边视图。
                            subagents.len() > 1
                        } else {
                            is_subagent || tool_output_has_content(output) || has_nested_transcript
                        };
                        let default_expanded = if is_pi_dispatch {
                            subagents.len() > 1
                        } else {
                            tool_has_live_children(children)
                                || (is_subagent
                                    && matches!(
                                        status,
                                        ToolCallStatus::Pending | ToolCallStatus::InProgress
                                    ))
                        };
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
                        // 发给别的 Pi 对话的一条不是工具卡片，是居中的事件行。
                        if let Some(send) = peer.as_ref() {
                            this.render_peer_send(i, &owner, send, *status, muted, cx)
                        } else if compact_in_process
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
                            let faint = gpui::rgb(ui_theme::faint());
                            let ink = if failed {
                                gpui::rgb(ui_theme::bad()).into()
                            } else {
                                muted
                            };
                            let mut row = div().flex().items_center()
                                .id(owner_id("acp-tool-compact", &owner))
                                .debug_selector(move || format!("ACP_TOOL_COMPACT_{i}"))
                                .w_full()
                                .min_h(px(22.))
                                .gap_2()
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_color(ink)
                                        .when(can_expand, |icon| {
                                            icon.group_hover(PEEK_ROW_GROUP, |s| {
                                                s.text_color(gpui::rgb(ui_theme::text()))
                                            })
                                        })
                                        .child(
                                            process_timeline_icon(kind).size(px(13.)),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .type_style(TYPE.text_xsmall)
                                        .text_color(ink)
                                        .when(can_expand, |label| {
                                            label.group_hover(PEEK_ROW_GROUP, |s| {
                                                s.text_color(gpui::rgb(ui_theme::text()))
                                            })
                                        })
                                        .child(truncated_line(headline.clone())),
                                );
                            if failed {
                                row = row.child(
                                    div()
                                        .flex_shrink_0()
                                        .type_style(TYPE.text_xsmall)
                                        .text_color(gpui::rgb(ui_theme::bad()))
                                        .child(SharedString::from(t!("tool.failed"))),
                                );
                            } else if let Some((added, removed)) = diff_stats {
                                row = row.child(render_compact_diff_stats(added, removed));
                            } else if let Some(summary) =
                                tool_result_summary(*kind, *status, output)
                            {
                                row = row.child(
                                    div()
                                        .flex_shrink_0()
                                        .type_style(TYPE.text_xsmall)
                                        .text_color(faint)
                                        .child(summary),
                                );
                            }
                            if can_expand {
                                this.tool_output_popover(
                                    i,
                                    muted,
                                    Button::new(owner_id("acp-tool-compact-trigger", &owner))
                                        .quiet()
                                        .w_full()
                                        .justify_start()
                                        .group(PEEK_ROW_GROUP)
                                        .accessibility_label(headline)
                                        .child(row),
                                    cx,
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
                                    subagents,
                                    has_expandable_content,
                                    default_expanded,
                                    card_expanded,
                                    failed,
                                    animate_ambient,
                                    muted,
                                    border,
                                },
                                cx,
                            )
                        } else {
                            let faint = gpui::rgb(ui_theme::faint());
                            let ink = if failed {
                                gpui::rgb(ui_theme::bad()).into()
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
                                        .type_style(TYPE.text_xsmall)
                                        .text_color(faint)
                                        .child(status_label)
                                        .into_any_element()
                                }
                            } else if failed {
                                div()
                                    .type_style(TYPE.text_xsmall)
                                    .text_color(gpui::rgb(ui_theme::bad()))
                                    .child(status_label)
                                    .into_any_element()
                            } else if let Some((total_added, total_removed)) = diff_stats {
                                render_compact_diff_stats(total_added, total_removed)
                            } else if let Some(summary) =
                                tool_result_summary(*kind, *status, output)
                            {
                                div()
                                    .flex_shrink_0()
                                    .type_style(TYPE.text_xsmall)
                                    .text_color(faint)
                                    .child(summary)
                                    .into_any_element()
                            } else if matches!(status, ToolCallStatus::Pending) {
                                div()
                                    .type_style(TYPE.text_xsmall)
                                    .text_color(faint)
                                    .child(status_label)
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            };

                        let mut card = div().flex().flex_col().w_full();
                        let mut header = div().flex().items_center()
                            .id(owner_id("acp-tool-card-toggle", &owner))
                            .w_full()
                            .min_h(px(22.))
                            .gap_2();
                        if has_expandable_content {
                            let id_for_toggle = id.clone();
                            header = header.group(EXPAND_ICON_GROUP).on_mouse_down(
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
                                process_timeline_icon(kind)
                                    .size(px(13.))
                                    .text_color(ink)
                                    .into_any_element()
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .type_style(TYPE.text_xsmall)
                                    .text_color(ink)
                                    .child(truncated_line(compact_tool_headline(*kind, title))),
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
                                            div().flex().flex_col()
                                                .pl_5()
                                                .pt_1()
                                                .pb_1()
                                                .gap_1()
                                                .child(
                                                    plain_text_view(
                                                        owner_part("acp-diff-path", &owner, part_ix),
                                                        path,
                                                    )
                                                        .type_style(TYPE.text_xsmall)
                                                        .text_color(muted)
                                                )
                                                .children(cached.map(|diff| {
                                                    render_diff_lines(
                                                        &diff.lines,
                                                        owner_part("acp-diff", &owner, part_ix),
                                                        border,
                                                        muted,
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
                                            .type_style(TYPE.text_xsmall)
                                            .text_color(muted)
                                            .into_any_element();
                                            card.child(
                                                div().flex().flex_col()
                                                    .pl_5()
                                                    .pt_1()
                                                    .pb_1()
                                                    .gap_1()
                                                    .child(body_el)
                                                    .when(need_toggle, |d| {
                                                        let key = key.clone();
                                                        d.child(
                                                            Button::new(owner_part(
                                                                "acp-tool-toggle",
                                                                &owner,
                                                                part_ix,
                                                            ))
                                                            .quiet()
                                                            .small()
                                                            .label(if expanded {
                                                                t!("toggle.collapse").into_owned()
                                                            } else {
                                                                t!("toggle.expand_lines", count = total)
                                                                    .into_owned()
                                                            })
                                                            .on_click(
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
                                                        border,
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
                        if let Some(peer) = note.peer.as_ref() {
                            this.render_peer_note(i, &note_owner, entry, note, peer, muted, border, cx)
                        } else {
                        // 后台任务和后台子智能体的结果：靠左一行，不是谁说的话。输出可能很长，
                        // 模型已经拿到完整内容，人要看时再点开，展开后限高滚动。展开状态和思考
                        // 详情共用 `expanded_thoughts`（key 都是 entries 下标，互不冲突）。
                        let has_output = !note.output_tail.trim().is_empty();
                        let expanded = has_output && this.expanded_thoughts.contains(&i);
                        let icon = match note.kind {
                            smelt_plugin_api::model::acp_chat::TaskNoteKind::Subagent => IconName::Bot,
                            _ => IconName::Terminal,
                        };
                        let mut row = event_line(muted)
                            .child(Icon::new(icon).size(px(13.)).text_color(muted))
                            .child(div().min_w_0().overflow_hidden().child(truncated_line(note.summary())));
                        if has_output {
                            row = row.child(this.render_event_toggle(
                                owner_id("acp-task-note-row", &note_owner),
                                i,
                                expanded,
                                cx,
                            ));
                        }
                        let mut block = div().flex().flex_col().w_full().child(leading_event(row));
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
                                        plain_text_view(
                                            owner_id("acp-task-note", &note_owner),
                                            &note.output_tail,
                                        )
                                        .type_style(TYPE.text_xsmall)
                                        .text_color(muted)
                                        .font_family(smelt_plugin_api::model::font_config::font_family()),
                                    ),
                            );
                        }
                        block.into_any_element()
                        }
                    }
                    AcpEntry::TurnFailed { detail, hint } => {
                        // 回合失败是状态，不是助手的回答：单独一块原话加下一步，可以选中复制。
                        let text = smelt_plugin_api::model::acp_chat::turn_failed_text(detail, *hint);
                        div()
                            .w_full()
                            .min_w_0()
                            .type_style(TYPE.body)
                            .text_color(gpui::rgb(ui_theme::text()))
                            .child(plain_text_view(("acp-turn-failed", i), &text))
                            .into_any_element()
                    }
                    AcpEntry::Divider(_) | AcpEntry::NewSession { .. } => labeled_divider(
                        entry.divider_label().unwrap_or_default().into(),
                        border,
                        muted,
                    ),
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
                    let mut col = div().flex().flex_col()
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
                if let Some(head) = segment_head {
                    el = div().flex().flex_col()
                        .w_full()
                        .gap_1()
                        .child(head)
                        .child(el)
                        .into_any_element();
                }
                if let Some(fold) = fold_header_here {
                    el = div().flex().flex_col()
                        .w_full()
                        .gap_1()
                        .child(this.render_turn_fold_header(fold, true, muted, cx))
                        .child(el)
                        .into_any_element();
                }
                if let Some(tail) = this.render_turn_tail(i, &conversation_layout, animate_ambient, cx) {
                    el = div().flex().flex_col()
                        .w_full()
                        .gap_3()
                        .child(el)
                        .child(tail)
                        .into_any_element();
                }
                // `gpui::list` 不像 flex 容器那样处理 `gap`；间距必须属于
                // 虚拟项本身，否则测得的高度不包含消息间的留白。
                let bottom = if in_process_timeline || empty_body {
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
                let top = if !empty_body && clear_capsule && (i == 0 || this.revealed_prompt == Some(i)) {
                    ui_theme::CHROME_ISLAND_CLEARANCE_PX
                } else {
                    0.
                };
                // 行外壳不裁剪。拖选文字的自动滚动拿最近的裁剪区当视口，行一裁剪，
                // 短消息整行都落在贴边触发带里，横着拖也会滚走列表；裁剪交给列表本身。
                div().flex().items_center()
                    .w_full()
                    .justify_center()
                    .px_4()
                    .pt(px(top))
                    .pb(px(bottom))
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
        retry: &smelt_plugin_api::model::acp_session::ProviderRetryView,
        entry_ix: usize,
        animate_ambient: bool,
    ) -> gpui::AnyElement {
        let mark: gpui::Hsla = gpui::rgb(ui_theme::need()).into();
        div()
            .flex()
            .flex_col()
            .id(("acp-provider-retry", entry_ix))
            .gap_0p5()
            .type_style(TYPE.text_xsmall)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_color(gpui::rgb(ui_theme::muted()))
                    .child(quiet_dots(
                        ("acp-provider-retry-dots", entry_ix),
                        mark,
                        animate_ambient,
                    ))
                    .child(provider_retry_label(retry, now_unix_ms())),
            )
            .when(!retry.error.is_empty(), |col| {
                col.child(
                    div()
                        .pl(px(14.))
                        .overflow_hidden()
                        .text_color(gpui::rgb(ui_theme::muted()))
                        .child(truncated_line(retry.error.clone())),
                )
            })
            .into_any_element()
    }

    fn render_turn_outcome_card(
        &self,
        entry_ix: usize,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let (message, action) = turn_outcome_card(self.turn_outcome, self.supports_rewind)?;
        Some(
            div()
                .flex()
                .items_center()
                .id(("acp-turn-outcome", entry_ix))
                .w_full()
                .gap_2()
                .px_3()
                .py_2()
                .rounded(ui_theme::card_radius())
                .border_1()
                .border_color(ui_theme::hair())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .type_style(TYPE.ui)
                        .text_color(gpui::rgb(ui_theme::need()))
                        .child(message),
                )
                .children(action.map(|action| {
                    let label = match action {
                        TurnOutcomeAction::Retry => t!("action.retry"),
                        TurnOutcomeAction::Continue => t!("prompt.continue"),
                    };
                    Button::new(("acp-turn-outcome-action", entry_ix))
                        .small()
                        .label(label)
                        .on_click(cx.listener(move |this, _ev, window, cx| {
                            this.rerun_last_turn(action, window, cx);
                        }))
                }))
                .into_any_element(),
        )
    }

    /// 对话末尾挂的东西，只挂在最后一行上，不另占虚拟项：在跑时是「现在」，
    /// 自动重试时换成重试说明；跑完了没正常结束给一张能重试或继续的卡。
    fn render_turn_tail(
        &self,
        entry_ix: usize,
        layout: &[EntryPresentation],
        animate_ambient: bool,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if entry_ix + 1 != self.entries.len() {
            return None;
        }
        if !self.has_active_turn() {
            return self.render_turn_outcome_card(entry_ix, cx);
        }
        Some(match self.provider_retry.as_ref() {
            Some(retry) => self.render_provider_retry(retry, entry_ix, animate_ambient),
            None => self.render_now_tail(entry_ix, layout, animate_ambient),
        })
    }

    /// 收起的行高度为 0；它要是最后一行，就只画末尾挂的东西。
    fn tail_only_row(
        &self,
        entry_ix: usize,
        layout: &[EntryPresentation],
        animate_ambient: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match self.render_turn_tail(entry_ix, layout, animate_ambient, cx) {
            Some(tail) => div()
                .flex()
                .items_center()
                .w_full()
                .justify_center()
                .px_4()
                .pb(px(12.))
                .child(
                    div()
                        .w_full()
                        .max_w(ui_theme::conversation_max_width())
                        .child(tail),
                )
                .into_any_element(),
            // 高度必须是 0：空 div 若测出残高，虚拟列表会把回答叠在标题上。
            None => div()
                .h(px(0.))
                .w_full()
                .overflow_hidden()
                .into_any_element(),
        }
    }

    /// 「现在」：这一轮正在做什么、跑了多久。进行中的状态只在这里说一次，
    /// 段落和工具行不再各挂一份转圈和耗时；子智能体列在派出它们的那段里。
    fn render_now_tail(
        &self,
        entry_ix: usize,
        layout: &[EntryPresentation],
        animate_ambient: bool,
    ) -> gpui::AnyElement {
        let muted: gpui::Hsla = gpui::rgb(ui_theme::muted()).into();
        let faint = gpui::rgb(ui_theme::faint());
        let now_state = now_state(
            &self.entries,
            &self.subagents,
            !self.permissions.is_empty(),
            |ix| {
                layout
                    .get(ix)
                    .and_then(|item| item.process_group)
                    .is_some_and(|group| !self.expanded_process_groups.contains(&group.first))
            },
        );
        let lead = match now_state.activity {
            NowActivity::Waiting => div()
                .size(px(6.))
                .mx(px(3.5))
                .flex_shrink_0()
                .rounded_full()
                .bg(gpui::rgb(ui_theme::need()))
                .into_any_element(),
            NowActivity::Thinking | NowActivity::Answering => {
                quiet_dots(("acp-now-dots", entry_ix), muted, animate_ambient)
            }
            _ => ambient_spinner(("acp-now-spinner", entry_ix), muted, animate_ambient),
        };
        let line = div()
            .flex()
            .items_center()
            .w_full()
            .min_h(ui_theme::height::xxs())
            .gap_1p5()
            .type_style(TYPE.text_xsmall)
            .text_color(muted)
            .child(lead)
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .child(truncated_line(now_activity_label(&now_state))),
            )
            .when_some(
                live_elapsed_ms(self.turn_started_at_ms),
                |line, elapsed_ms| line.child(elapsed_suffix(elapsed_ms, muted)),
            );
        let mut col = div()
            .flex()
            .flex_col()
            .id(("acp-now", entry_ix))
            .debug_selector(move || format!("ACP_NOW_{entry_ix}"))
            .w_full()
            .gap_0p5()
            .child(line);
        if let Some(excerpt) = now_state.thought_excerpt {
            col = col.child(
                div()
                    .pl(px(19.))
                    .w_full()
                    .min_w_0()
                    .overflow_hidden()
                    .type_style(TYPE.text_xsmall)
                    .text_color(faint)
                    .child(truncated_line(excerpt)),
            );
        }
        col.font_features(ui_theme::tabular_figures())
            .into_any_element()
    }

    /// 段首一行：计数行，下面是这段改了哪些文件的标签，点开的标签在原地展开改动。
    fn render_segment_head(
        &mut self,
        group: ProcessGroupInfo,
        expanded: bool,
        animate_ambient: bool,
        muted: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if group.tally.diff_parts > 0 {
            for ix in group.first..group.end.min(self.entries.len()) {
                if matches!(
                    self.entries.get(ix),
                    Some(AcpEntry::ToolCall { output, .. }) if tool_output_has_diff(output)
                ) {
                    self.ensure_diff_cache_for_entry(ix);
                }
            }
        }
        let chips = if group.tally.diff_parts > 0 {
            segment_edit_chips(&self.entries, &self.rendered_diffs, group.first, group.end)
        } else {
            Vec::new()
        };
        let show_chips = !chips.is_empty() && chips.len() <= MAX_EDIT_CHIPS;
        let edits_as_chips = show_chips && chips.len() == group.tally.edited_files;
        let header = self.render_process_group_header(group, expanded, edits_as_chips, muted, cx);
        // 展开后子智能体就是段里的卡片，收起时才在计数行下面一个一行列出来。
        let agents = if expanded || group.tally.agents == 0 {
            Vec::new()
        } else {
            self.render_segment_agents(group, animate_ambient, muted, cx)
        };
        if !show_chips && agents.is_empty() {
            return header;
        }
        let mut col = div().flex().flex_col().w_full().gap_1().child(header);
        if show_chips {
            col = self.render_segment_chips(col, group, &chips, muted, cx);
        }
        col.children(agents).into_any_element()
    }

    /// 段里改过的文件一个一个标签；点开的标签在下面原地展开这段对它的改动。
    fn render_segment_chips(
        &self,
        mut col: gpui::Div,
        group: ProcessGroupInfo,
        chips: &[EditChip],
        muted: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let group_key = group.first;
        let mut row = div().flex().flex_wrap().w_full().gap_1p5().pb_1();
        let mut open_chips = Vec::new();
        for (chip_ix, chip) in chips.iter().enumerate() {
            let key = (group_key, chip.path.clone());
            let open = self.expanded_edit_chips.contains(&key);
            if open {
                open_chips.push(chip);
            }
            row = row.child(
                render_edit_chip(chip, group_key, open)
                    .debug_selector(move || format!("ACP_EDIT_CHIP_{group_key}_{chip_ix}"))
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        if !this.expanded_edit_chips.remove(&key) {
                            this.expanded_edit_chips.insert(key.clone());
                        }
                        this.list_state
                            .remeasure_items(group_key..group_key.saturating_add(1));
                        cx.notify();
                    })),
            );
        }
        col = col.child(row);
        let border: gpui::Hsla = ui_theme::hair().into();
        for chip in open_chips {
            let mut block = div().flex().flex_col().w_full().gap_1().pb_2().child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(muted)
                    .child(truncated_line(chip.path.clone())),
            );
            for &(entry_ix, part_ix) in &chip.parts {
                let Some(AcpEntry::ToolCall { id, .. }) = self.entries.get(entry_ix) else {
                    continue;
                };
                let Some(diff) = self
                    .rendered_diffs
                    .get(id)
                    .and_then(|parts| parts.get(part_ix))
                    .and_then(Option::as_ref)
                else {
                    continue;
                };
                block = block.child(render_diff_lines(
                    &diff.lines,
                    gpui::ElementId::from((
                        gpui::ElementId::named_usize("acp-edit-chip-diff", entry_ix),
                        gpui::SharedString::from(part_ix.to_string()),
                    )),
                    border,
                    muted,
                ));
            }
            col = col.child(block);
        }
        col
    }

    /// 段里派出的子智能体，一个一行。Pi 的点开看它的过程；别的 agent 的子任务没有单独会话，
    /// 点开就展开这段、打开它的卡片。
    fn render_segment_agents(
        &self,
        group: ProcessGroupInfo,
        animate_ambient: bool,
        muted: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let now = now_unix_ms();
        let mut rows = Vec::new();
        for ix in group.first..group.end.min(self.entries.len()) {
            let Some(AcpEntry::ToolCall {
                id,
                title,
                kind,
                status,
                output,
                children,
                subagents,
                peer,
            }) = self.entries.get(ix)
            else {
                continue;
            };
            if peer.is_some() {
                continue;
            }
            let owner = super::super::lookup_owner(&self.layout_cache.borrow().owner_ids, id);
            if !subagents.is_empty() {
                let dispatch_running =
                    matches!(status, ToolCallStatus::InProgress | ToolCallStatus::Pending);
                for (run_ix, run) in subagents.iter().enumerate() {
                    let run_status = subagent_status(run, &self.subagents, dispatch_running);
                    rows.push(self.render_subagent_run_row(
                        &owner,
                        run_ix,
                        run,
                        title,
                        run_status,
                        animate_ambient,
                        muted,
                        now,
                        cx,
                    ));
                }
                continue;
            }
            if *kind != ToolKind::Collaborate {
                continue;
            }
            let running = matches!(status, ToolCallStatus::InProgress | ToolCallStatus::Pending)
                || smelt_plugin_api::model::acp_chat::has_unfinished_tool_call(children);
            let run_status = if running {
                SubagentStatus::Running
            } else if matches!(status, ToolCallStatus::Failed) {
                SubagentStatus::Failed
            } else {
                SubagentStatus::Completed
            };
            let name = match smelt_plugin_api::model::acp_chat::subagent_card_title(title, children)
            {
                SubagentCardTitle::Count(count) => crate::plural(
                    count,
                    t!("subagent.roster.one"),
                    t!("subagent.roster.other", count = count),
                ),
                SubagentCardTitle::Named(name) if name.is_empty() => {
                    t!("subagent.generic").into_owned()
                }
                SubagentCardTitle::Named(name) => name,
            };
            let detail = if running {
                children.iter().rev().find_map(|child| match child {
                    AcpEntry::ToolCall { kind, title, .. } => {
                        Some(compact_tool_headline(*kind, title))
                    }
                    _ => None,
                })
            } else {
                (run_status == SubagentStatus::Failed).then(|| t!("tool.failed").into_owned())
            };
            let red = gpui::rgb(ui_theme::bad());
            let faint = gpui::rgb(ui_theme::faint());
            let row = div()
                .flex()
                .items_center()
                .w_full()
                .min_h(px(22.))
                .gap_2()
                .pl(px(22.))
                .child(subagent_status_icon(
                    run_status,
                    owner_id("acp-segment-agent-spin", &owner),
                    animate_ambient,
                    muted,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .type_style(TYPE.text_xsmall)
                        .text_color(muted)
                        .child(truncated_line(name.clone())),
                )
                .when_some(detail, |row, detail| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .max_w(px(280.))
                            .overflow_hidden()
                            .type_style(TYPE.text_xsmall)
                            .text_color(if run_status == SubagentStatus::Failed {
                                red
                            } else {
                                faint
                            })
                            .child(truncated_line(detail)),
                    )
                });
            let tool_id = id.clone();
            let has_content = tool_output_has_content(output) || !children.is_empty();
            let group_key = group.first;
            let group_end = group.end;
            rows.push(
                Button::new(owner_id("acp-segment-agent", &owner))
                    .quiet()
                    .w_full()
                    .justify_start()
                    .accessibility_label(name)
                    .debug_selector(move || format!("ACP_SEGMENT_AGENT_{ix}"))
                    .child(row)
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.expanded_process_groups.insert(group_key);
                        if has_content {
                            this.collapsed_tool_cards.remove(&tool_id);
                            this.expanded_tool_cards.insert(tool_id.clone());
                        }
                        this.list_state
                            .remeasure_items(group_key..group_end.min(this.entries.len()));
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        rows
    }

    fn render_process_group_header(
        &self,
        group: ProcessGroupInfo,
        expanded: bool,
        edits_as_chips: bool,
        muted: gpui::Hsla,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let group_key = group.first;
        let group_end = group.end;
        summary_row(
            ("acp-process-group", group.first),
            process_group_header_label(group, edits_as_chips),
            tally_failed_label(&group.tally),
            expanded,
            muted,
        )
        .debug_selector(move || format!("ACP_PROCESS_GROUP_{}", group.first))
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

    fn render_turn_fold_header(
        &self,
        fold: TurnFoldInfo,
        expanded: bool,
        muted: gpui::Hsla,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let fold_key = fold.first;
        let fold_end = fold.end;
        summary_row(
            ("acp-turn-fold", fold.first),
            turn_fold_label(fold),
            None,
            expanded,
            muted,
        )
        .debug_selector(move || format!("ACP_TURN_FOLD_{}", fold.first))
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            if !this.expanded_turn_folds.remove(&fold_key) {
                this.expanded_turn_folds.insert(fold_key);
            }
            this.list_state
                .remeasure_items(fold_key..fold_end.min(this.entries.len()));
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
        let faint = gpui::rgb(ui_theme::faint());
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
        let header = div()
            .flex()
            .items_center()
            .id(owner_id("acp-tool-run", &run_owner))
            .role(gpui::Role::Button)
            .aria_label(run_label.clone())
            .debug_selector(move || format!("ACP_TOOL_RUN_{start}"))
            .group(EXPAND_ICON_GROUP)
            .w_full()
            .min_h(px(22.))
            .gap_2()
            .child(expandable_lead_icon(
                process_timeline_icon(&run.kind),
                open,
                muted,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .type_style(TYPE.text_xsmall)
                    .text_color(muted)
                    .child(truncated_line(run_label)),
            )
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                this.toggle_tool_run(start, cx);
            }));
        let mut col = div()
            .flex()
            .flex_col()
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
                let row = div()
                    .flex()
                    .items_center()
                    .id(owner_id("acp-tool-run-item", &tool_owner))
                    .debug_selector(move || format!("ACP_TOOL_RUN_ITEM_{ix}"))
                    .w_full()
                    .min_h(px(20.))
                    .pl_5()
                    .gap_2()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(faint)
                            .when(can_expand, |icon| {
                                icon.group_hover(PEEK_ROW_GROUP, |s| {
                                    s.text_color(gpui::rgb(ui_theme::text()))
                                })
                            })
                            .child(process_timeline_icon(&run.kind).size(px(13.))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .type_style(TYPE.text_xsmall)
                            .text_color(faint)
                            .when(can_expand, |label| {
                                label.group_hover(PEEK_ROW_GROUP, |s| {
                                    s.text_color(gpui::rgb(ui_theme::text()))
                                })
                            })
                            .child(truncated_line(headline.clone())),
                    );
                col = col.child(if can_expand {
                    self.tool_output_popover(
                        ix,
                        muted,
                        Button::new(owner_id("acp-tool-run-item-trigger", &tool_owner))
                            .quiet()
                            .w_full()
                            .justify_start()
                            .group(PEEK_ROW_GROUP)
                            .accessibility_label(headline)
                            .child(row),
                        cx,
                    )
                    .into_any_element()
                } else {
                    row.into_any_element()
                });
            }
        }
        col.into_any_element()
    }

    /// 发给别的 Pi 对话的一条：居中一行「已发任务给 B」，起新对话的是「新建对话 C 并交代任务」。
    /// 点对方标题切过去；正文露第一行，悬停看全文，点箭头展开。
    fn render_peer_send(
        &self,
        index: usize,
        owner: &gpui::SharedString,
        send: &smelt_plugin_api::model::acp_chat::PeerSend,
        status: ToolCallStatus,
        muted: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let red: gpui::Hsla = gpui::rgb(ui_theme::bad()).into();
        let running = matches!(status, ToolCallStatus::Pending | ToolCallStatus::InProgress);
        let failed = send.error.is_some() || matches!(status, ToolCallStatus::Failed);
        let kind = peer_message_noun(send.kind);
        let (icon, ink, lead) = match (failed, running, send.spawned) {
            (true, _, true) => (IconName::Warning, red, t!("peer.spawn")),
            (true, _, false) => (IconName::Warning, red, t!("peer.send_to")),
            (false, true, true) => (IconName::Plus, muted, t!("peer.spawning")),
            (false, false, true) => (IconName::Plus, muted, t!("peer.spawn")),
            (false, true, false) => (IconName::ArrowRight, muted, t!("peer.sending", kind = kind)),
            (false, false, false) => (IconName::ArrowRight, muted, t!("peer.sent", kind = kind)),
        };
        let lead = SharedString::from(lead);
        // 新建对话交代的就是任务：「新建对话 C 并交代任务」。
        let tail =
            (send.spawned && !failed && !running).then(|| SharedString::from(t!("peer.with_task")));
        let has_body = !send.text.trim().is_empty();
        let expanded = has_body && self.expanded_thoughts.contains(&index);
        let mut row = event_line(muted)
            .child(Icon::new(icon).size(px(13.)).text_color(ink))
            .child(div().flex_shrink_0().child(lead))
            .child(self.render_conversation_link(
                owner_id("acp-peer-send-to", owner),
                &send.to_id,
                &send.to_title,
                cx,
            ));
        if let Some(tail) = tail {
            row = row.child(div().flex_shrink_0().child(tail));
        }
        if failed {
            let reason = send.error.as_deref().map_or_else(
                || t!("tool.failed").into_owned(),
                |error| t!("peer.failed", error = error).into_owned(),
            );
            row = row.child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_color(red)
                    .child(truncated_line(reason)),
            );
        }
        if has_body {
            row = row
                .child(peer_preview_hover(
                    owner_id("acp-peer-send-hover", owner),
                    owner_id("acp-peer-send-hover-body", owner),
                    peer_text_preview(&send.text),
                    plain_text_view(owner_id("acp-peer-send-hover-text", owner), &send.text)
                        .type_style(TYPE.text_xsmall)
                        .into_any_element(),
                    muted,
                ))
                .child(self.render_event_toggle(
                    owner_id("acp-peer-send-toggle", owner),
                    index,
                    expanded,
                    cx,
                ));
        }
        let mut block = div().flex().flex_col().w_full().child(centered_event(row));
        if expanded {
            block = block.child(
                div()
                    .id(owner_id("acp-peer-send-body", owner))
                    .w_full()
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .pb_2()
                    .child(
                        plain_text_view(owner_id("acp-peer-send-text", owner), &send.text)
                            .type_style(TYPE.text_xsmall)
                            .text_color(muted),
                    ),
            );
        }
        block.into_any_element()
    }

    /// 别的 Pi 对话发来的消息：居中一行「A 发来任务」加正文第一行，悬停看全文，
    /// 点箭头在下面展开（引用块）。退信同样画在这里：警示图标加「B 没收到你的消息」，
    /// 正文是原因和原文开头。
    #[allow(clippy::too_many_arguments)]
    fn render_peer_note(
        &self,
        index: usize,
        owner: &gpui::SharedString,
        entry: &AcpEntry,
        note: &smelt_plugin_api::model::acp_chat::TaskNote,
        peer: &smelt_plugin_api::model::acp_chat::PeerNote,
        muted: gpui::Hsla,
        border: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (icon, ink): (IconName, gpui::Hsla) =
            if peer.kind == smelt_plugin_api::model::acp_session::PeerMessageKind::Bounce {
                (IconName::Warning, gpui::rgb(ui_theme::bad()).into())
            } else {
                (IconName::Inbox, muted)
            };
        let row = event_line(muted)
            .child(Icon::new(icon).size(px(13.)).text_color(ink))
            .child(self.render_conversation_link(
                owner_id("acp-peer-note-from", owner),
                &peer.source_session_id,
                &peer.source_title,
                cx,
            ))
            .child(
                div()
                    .flex_shrink_0()
                    .child(peer_message_received(peer.kind)),
            );
        if note.output_tail.trim().is_empty() {
            return centered_event(row).into_any_element();
        }
        let expanded = self.expanded_thoughts.contains(&index);
        let markdown = cached_entry_markdown(&self.rendered_markdown, index, entry);
        let row = row
            .child(peer_preview_hover(
                owner_id("acp-peer-note-hover", owner),
                owner_id("acp-peer-note-hover-body", owner),
                peer_text_preview(&note.output_tail),
                // 悬停卡片和展开的正文可能同时在屏上，各用各的排版状态。
                self.retained_markdown("acp-peer-md-hover", index, 0, markdown.clone(), cx)
                    .into_any_element(),
                muted,
            ))
            .child(self.render_event_toggle(
                owner_id("acp-peer-note-toggle", owner),
                index,
                expanded,
                cx,
            ));
        let mut block = div()
            .flex()
            .flex_col()
            .w_full()
            .gap_1()
            .child(centered_event(row));
        if expanded {
            block = block.child(
                div()
                    .id(owner_id("acp-peer-note-body", owner))
                    .w_full()
                    .pl_3()
                    .border_l_2()
                    .border_color(border)
                    .child(self.retained_markdown("acp-peer-md", index, 0, markdown, cx)),
            );
        }
        block.into_any_element()
    }

    /// 事件行里的对话标题。点了切到那个对话；没有标题时显示 id。
    fn render_conversation_link(
        &self,
        id: gpui::ElementId,
        sid: &str,
        title: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let label = if title.trim().is_empty() {
            sid.to_string()
        } else {
            title.to_string()
        };
        let sid = sid.to_string();
        Button::new(id)
            .quiet()
            .small()
            .label(label.clone())
            .tooltip(sid.clone())
            .accessibility_label(t!("peer.open", title = label).into_owned())
            .on_click(cx.listener(move |_this, _ev, _window, cx| {
                // 新对话还在建，没有 id 可跳。
                if !sid.is_empty() {
                    cx.emit(ConversationViewEvent::OpenConversation { sid: sid.clone() });
                }
            }))
            .into_any_element()
    }

    /// 事件行末尾的展开箭头。展开状态记在 `expanded_thoughts`，key 是 entries 下标。
    fn render_event_toggle(
        &self,
        id: gpui::ElementId,
        index: usize,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        Button::new(id)
            .quiet()
            .small()
            .icon(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .accessibility_label(if expanded {
                t!("toggle.collapse")
            } else {
                t!("toggle.expand")
            })
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                if !this.expanded_thoughts.remove(&index) {
                    this.expanded_thoughts.insert(index);
                }
                this.list_state
                    .remeasure_items(index..index.saturating_add(1));
                cx.notify();
            }))
            .into_any_element()
    }

    fn render_subagent_card(
        &self,
        card: SubagentCard<'_>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if !card.subagents.is_empty() {
            return self.render_dispatch_card(card, cx);
        }
        let SubagentCard {
            index: i,
            id,
            title,
            status,
            output,
            children,
            subagents: _,
            has_expandable_content,
            default_expanded,
            card_expanded,
            failed,
            animate_ambient,
            muted,
            border,
        } = card;
        let faint = gpui::rgb(ui_theme::faint());
        let red = gpui::rgb(ui_theme::bad());
        let ink = if failed { red.into() } else { muted };
        let running = matches!(status, ToolCallStatus::InProgress | ToolCallStatus::Pending)
            || smelt_plugin_api::model::acp_chat::has_unfinished_tool_call(children);
        let roster = smelt_plugin_api::model::acp_chat::is_subagent_roster(children);
        let headline = match smelt_plugin_api::model::acp_chat::subagent_card_title(title, children)
        {
            SubagentCardTitle::Count(count) => crate::plural(
                count,
                t!("subagent.roster.one"),
                t!("subagent.roster.other", count = count),
            ),
            SubagentCardTitle::Named(name) if name.is_empty() => {
                t!("subagent.generic").into_owned()
            }
            SubagentCardTitle::Named(name) => name,
        };
        let progress = smelt_plugin_api::model::acp_chat::subagent_progress(status, children);
        let progress_failed = failed || matches!(progress, SubagentProgress::Failed(_));
        let show_fail_label = failed && !matches!(progress, SubagentProgress::Failed(_));
        let show_header_spinner = running && !(card_expanded && roster);
        let show_check = !running && !progress_failed && progress == SubagentProgress::None;
        let progress = match progress {
            SubagentProgress::None => None,
            SubagentProgress::Running { done, total } => Some(format!("{done}/{total}")),
            SubagentProgress::Failed(count) => {
                Some(t!("dispatch.failed", count = count).into_owned())
            }
            SubagentProgress::Steps(count) => Some(crate::plural(
                count,
                t!("subagent.steps.one"),
                t!("subagent.steps.other", count = count),
            )),
        };
        let owner = super::super::lookup_owner(&self.layout_cache.borrow().owner_ids, id);
        let id_for_toggle = id.to_string();
        let mut header = div()
            .flex()
            .items_center()
            .id(owner_id("acp-subagent-toggle", &owner))
            .w_full()
            .min_h(px(24.))
            .gap_2()
            .group(EXPAND_ICON_GROUP)
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
                    .overflow_hidden()
                    .type_style(TYPE.ui)
                    .text_color(ink)
                    .child(truncated_line(headline)),
            );
        if let Some(progress) = progress {
            header = header.child(
                div()
                    .flex_shrink_0()
                    .type_style(TYPE.text_xsmall)
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
                .type_style(TYPE.text_xsmall)
                .text_color(red)
                .child(SharedString::from(t!("tool.failed")))
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
        let mut card = div()
            .flex()
            .flex_col()
            .id(owner_id("acp-subagent-card", &owner))
            .w_full()
            .rounded(ui_theme::card_radius())
            .border_1()
            .border_color(if failed { red.into() } else { border })
            .bg(gpui::rgb(ui_theme::surface()))
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
                        .type_style(TYPE.text_xsmall)
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

    /// Pi 子智能体派发。一个子智能体就是一行；并行、串联时标题行下面每个子智能体一行，
    /// 点标题行收起或展开。点子智能体那一行在侧边看它的过程，运行中的能就地停下。过程在
    /// 各自的子会话里，不在父对话里展开。状态按子智能体 id 取（`subagent_status`）。
    fn render_dispatch_card(
        &self,
        card: SubagentCard<'_>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let SubagentCard {
            index: i,
            id,
            title,
            status,
            subagents,
            has_expandable_content,
            default_expanded,
            card_expanded,
            animate_ambient,
            muted,
            border,
            ..
        } = card;
        let faint = gpui::rgb(ui_theme::faint());
        let red = gpui::rgb(ui_theme::bad());
        let now = now_unix_ms();
        let dispatch_running = matches!(status, ToolCallStatus::InProgress | ToolCallStatus::Pending);
        let statuses: Vec<SubagentStatus> = subagents
            .iter()
            .map(|run| subagent_status(run, &self.subagents, dispatch_running))
            .collect();
        let live: Vec<&SubagentView> = subagents
            .iter()
            .filter_map(|run| self.subagents.iter().find(|view| view.id == run.id))
            .collect();
        let single = subagents.len() == 1;
        let activity = if single {
            live.first().and_then(|view| view.last_activity.as_deref())
        } else {
            None
        };
        let progress = dispatch_progress(&statuses, activity);
        let owner = super::super::lookup_owner(&self.layout_cache.borrow().owner_ids, id);
        let headline = if single {
            subagent_run_title(&subagents[0], title)
        } else {
            crate::plural(
                subagents.len(),
                t!("subagent.summary.one"),
                t!("subagent.summary.other", count = subagents.len()),
            )
        };
        let ink = if progress.problem { red.into() } else { muted };
        let header_status = if single {
            statuses[0]
        } else if progress.running {
            SubagentStatus::Running
        } else if progress.problem {
            SubagentStatus::Failed
        } else {
            SubagentStatus::Completed
        };
        let mut header = div()
            .flex()
            .items_center()
            .w_full()
            .min_h(px(24.))
            .gap_2()
            .child(if has_expandable_content {
                expandable_lead_icon(IconName::Bot, card_expanded, ink).into_any_element()
            } else {
                Icon::new(IconName::Bot)
                    .size(px(14.))
                    .text_color(ink)
                    .into_any_element()
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .type_style(TYPE.ui)
                    .text_color(ink)
                    .child(truncated_line(headline.clone())),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .max_w(px(360.))
                    .overflow_hidden()
                    .type_style(TYPE.text_xsmall)
                    .text_color(if progress.problem { red } else { faint })
                    .child(truncated_line(progress.text)),
            )
            .child(subagent_status_icon(
                header_status,
                owner_id("acp-subagent-spin", &owner),
                animate_ambient,
                muted,
            ))
            .when_some(dispatch_elapsed_ms(&live, now), |header, elapsed_ms| {
                header.child(elapsed_suffix(elapsed_ms, muted))
            });
        if single {
            header = header.children(self.render_subagent_stop(&subagents[0].id, statuses[0], cx));
        }
        let header: gpui::AnyElement = if single {
            let run_id = subagents[0].id.clone();
            let run_title = headline.clone();
            Button::new(owner_id("acp-subagent-open", &owner))
                .quiet()
                .w_full()
                .justify_start()
                .group(SUBAGENT_STOP_GROUP)
                .accessibility_label(t!("subagent.view_process", title = run_title).into_owned())
                .child(header)
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    this.open_subagent_inspector(run_id.clone(), run_title.clone(), cx);
                }))
                .into_any_element()
        } else {
            let id_for_toggle = id.to_string();
            Button::new(owner_id("acp-subagent-toggle", &owner))
                .quiet()
                .w_full()
                .justify_start()
                .group(EXPAND_ICON_GROUP)
                .accessibility_label(if card_expanded {
                    t!("toggle.collapse_named", name = headline).into_owned()
                } else {
                    t!("toggle.expand_named", name = headline).into_owned()
                })
                .child(header)
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    this.toggle_tool_card(
                        i,
                        id_for_toggle.clone(),
                        has_expandable_content,
                        default_expanded,
                        cx,
                    );
                }))
                .into_any_element()
        };
        let mut card = div()
            .flex()
            .flex_col()
            .id(owner_id("acp-subagent-card", &owner))
            .w_full()
            .rounded(ui_theme::card_radius())
            .border_1()
            .border_color(if progress.problem { red.into() } else { border })
            .bg(gpui::rgb(ui_theme::surface()))
            .px_3()
            .py_2()
            .gap_1()
            .child(header);
        if !single && card_expanded {
            for (index, (run, run_status)) in subagents.iter().zip(&statuses).enumerate() {
                card = card.child(self.render_subagent_run_row(
                    &owner,
                    index,
                    run,
                    title,
                    *run_status,
                    animate_ambient,
                    muted,
                    now,
                    cx,
                ));
            }
        }
        card.into_any_element()
    }

    /// 派发卡片里的一个子智能体：状态、名字、在做什么、耗时、停止。整行点开侧边视图。
    #[allow(clippy::too_many_arguments)]
    fn render_subagent_run_row(
        &self,
        owner: &gpui::SharedString,
        index: usize,
        run: &smelt_plugin_api::model::acp_chat::SubagentRun,
        dispatch_title: &str,
        run_status: SubagentStatus,
        animate_ambient: bool,
        muted: gpui::Hsla,
        now: u64,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let faint = gpui::rgb(ui_theme::faint());
        let red = gpui::rgb(ui_theme::bad());
        let view = self.subagents.iter().find(|view| view.id == run.id);
        let problem = matches!(run_status, SubagentStatus::Failed | SubagentStatus::Interrupted);
        let detail = view
            .and_then(|view| view.last_activity.clone())
            .filter(|_| run_status.is_running())
            .unwrap_or_else(|| subagent_status_label(run_status).to_string());
        let run_title = subagent_run_title(run, dispatch_title);
        let row_owner = gpui::SharedString::from(format!("{owner}-{index}"));
        let row = div()
            .flex()
            .items_center()
            .w_full()
            .min_h(px(22.))
            .gap_2()
            .pl(px(22.))
            .child(subagent_status_icon(
                run_status,
                owner_id("acp-subagent-row-spin", &row_owner),
                animate_ambient,
                muted,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .type_style(TYPE.text_xsmall)
                    .text_color(muted)
                    .child(truncated_line(run_title.clone())),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .max_w(px(280.))
                    .overflow_hidden()
                    .type_style(TYPE.text_xsmall)
                    .text_color(if problem { red } else { faint })
                    .child(truncated_line(detail)),
            )
            .when_some(view.map(|view| subagent_elapsed_ms(view, now)), |row, elapsed_ms| {
                row.child(elapsed_suffix(elapsed_ms, muted))
            })
            .children(self.render_subagent_stop(&run.id, run_status, cx));
        let run_id = run.id.clone();
        Button::new(owner_id("acp-subagent-row-open", &row_owner))
            .quiet()
            .w_full()
            .justify_start()
            .group(SUBAGENT_STOP_GROUP)
            .accessibility_label(t!("subagent.view_process", title = run_title).into_owned())
            .child(row)
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                this.open_subagent_inspector(run_id.clone(), run_title.clone(), cx);
            }))
            .into_any_element()
    }

    /// 运行中的子智能体就地停下。点过之后到它真停下之前显示「正在停止…」。
    pub(super) fn render_subagent_stop(
        &self,
        id: &str,
        status: SubagentStatus,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !status.is_running() || !self.subagents.iter().any(|view| view.id == id) {
            return None;
        }
        if self.stopping_subagents.contains(id) {
            return Some(
                div()
                    .flex_shrink_0()
                    .type_style(TYPE.text_xsmall)
                    .text_color(gpui::rgb(ui_theme::muted()))
                    .child(t!("task.stopping").into_owned())
                    .into_any_element(),
            );
        }
        let stop_id = id.to_string();
        // 整行 hover 才显出来，用透明度而不是不渲染，耗时才不会跳。
        Some(
            Button::new((gpui::ElementId::from("acp-subagent-stop"), stop_id.clone()))
                .danger_quiet()
                .small()
                .label(t!("stop.stop"))
                .flex_shrink_0()
                .opacity(0.0)
                .group_hover(SUBAGENT_STOP_GROUP, |s| s.opacity(1.0))
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    // 不要顺带打开侧边视图。
                    cx.stop_propagation();
                    this.stop_subagent(stop_id.clone(), cx);
                }))
                .into_any_element(),
        )
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
        let faint = gpui::rgb(ui_theme::faint());
        let red = gpui::rgb(ui_theme::bad());
        let mut col = div().flex().flex_col().w_full().gap_1();
        for child in children {
            col = col.child(match child {
                AcpEntry::Assistant { text, thought } => {
                    let line = text.lines().next().unwrap_or_default();
                    if line.is_empty() {
                        div().into_any_element()
                    } else {
                        div()
                            .pl(px(21.))
                            .type_style(TYPE.text_xsmall)
                            .text_color(muted)
                            .overflow_hidden()
                            .child(truncated_line(if *thought {
                                t!("thought.line", line = line).into_owned()
                            } else {
                                line.to_string()
                            }))
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
                    subagents: _,
                    peer: _,
                } => {
                    let failed = matches!(status, ToolCallStatus::Failed);
                    let nested_running =
                        smelt_plugin_api::model::acp_chat::has_unfinished_tool_call(nested);
                    let ink = if failed { red.into() } else { muted };
                    let mut name = smelt_plugin_api::model::acp_chat::subagent_visible_title(title);
                    if name.is_empty() {
                        name = t!("subagent.generic").into_owned();
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
                    let mut row = div()
                        .flex()
                        .items_center()
                        .id(owner_id("acp-subagent-child", &owner))
                        .w_full()
                        .min_h(px(22.))
                        .gap_2();
                    if has_nested {
                        let id_for_toggle = id.clone();
                        row = row
                            .group(EXPAND_ICON_GROUP)
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
                            .overflow_hidden()
                            .type_style(TYPE.text_xsmall)
                            .text_color(ink)
                            .child(truncated_line(name)),
                    );
                    if has_nested && expanded {
                        div()
                            .flex()
                            .flex_col()
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

    fn tool_output_popover(
        &self,
        entry_ix: usize,
        muted: gpui::Hsla,
        trigger: Button,
        cx: &Context<Self>,
    ) -> Popover {
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
                    .trigger(trigger);
            }
        };
        let border: gpui::Hsla = ui_theme::hair().into();
        let view = cx.weak_entity();
        // 卡片不留内边距：正文自己滚动，内边距在滚动的内容里。
        Popover::new(owner_id("acp-tool-output-pop", &tool_id))
            .anchor(Anchor::BottomLeft)
            .p_0()
            .trigger(trigger)
            .content(move |_, _, cx| {
                let view = view.clone();
                let popover = cx.weak_entity();
                // 折叠卡片只有 hover 预览这一条显示路径，解码必须在这里触发，
                // 否则缩略图永远停在占位。已在途/已完成时是空操作。
                for image in images.iter().flatten().flatten() {
                    smelt_plugin_api::ui::image::ensure_preview(image, cx);
                }
                render_tool_output_popover_body(ToolOutputPopoverBody {
                    tool_id: tool_id.clone(),
                    kind,
                    output: &output,
                    diffs: diffs.as_deref(),
                    images: images.as_deref(),
                    children: &children,
                    muted,
                    border,
                    on_preview_image: std::rc::Rc::new(move |image, window, cx| {
                        // 先收起 hover 卡片，再交给工作台弹大图，避免两层浮层叠在一起。
                        popover.update(cx, |state, cx| state.dismiss(window, cx)).ok();
                        view.update(cx, |_, cx| {
                            cx.emit(ConversationViewEvent::PreviewImage(image))
                        })
                        .ok();
                    }),
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
        show_settled_elapsed: bool,
        muted: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let expanded = self.expanded_thoughts.contains(&entry_ix);
        let preview = progress_summary(text);
        let has_details = progress_has_details(text);
        // `in_process` 是「在过程组里」，不是「还在跑」：过程组里的思考和中间说明跑完也
        // 一直在组里，摘要只有一行，长文必须能点开，否则后半段永远看不到。
        // 悬停底和点击同一条件，避免看起来不能点却还能点。
        let interactive = has_details;
        // 回合没结束时没有完成耗时，进行中的耗时在末尾的「现在」里。
        let settled = if show_settled_elapsed {
            settled_thought_elapsed(
                &self.entries,
                entry_ix,
                self.loaded_entries_offset,
                &self.turn_timings,
            )
        } else {
            None
        };
        let mut row = ListRow::new((
            if in_process {
                "acp-progress-toggle"
            } else {
                "acp-analysis-toggle"
            },
            entry_ix,
        ))
        .debug_selector(move || format!("ACP_PROGRESS_{entry_ix}"))
        .h_flex()
        .w_full()
        .min_w_0()
        .min_h(px(if in_process { 22. } else { 26. }))
        .when(!in_process, |row| row.px_2().rounded_md())
        .gap_2()
        .disabled(!interactive)
        .when(in_process && interactive, |row| row.group(EXPAND_ICON_GROUP))
        .child(if in_process {
            // 过程组里的行不另加尾部箭头，和工具行一样：指上去时左侧图标换成箭头。
            let thought = matches!(entry, AcpEntry::Assistant { thought: true, .. });
            let circle = || {
                div()
                    .size(px(8.))
                    .flex_shrink_0()
                    .rounded_full()
                    .border_1()
                    .border_color(muted)
                    .into_any_element()
            };
            if thought && interactive {
                expandable_lead_icon(IconName::Asterisk, expanded, muted)
            } else if thought {
                Icon::new(IconName::Asterisk)
                    .size(px(13.))
                    .text_color(muted)
                    .into_any_element()
            } else if interactive {
                expandable_lead(circle(), expanded, muted)
            } else {
                circle()
            }
        } else {
            div()
                .flex_shrink_0()
                .type_style(TYPE.text_xsmall_medium)
                .text_color(muted)
                .child(t!("thought.summary").into_owned())
                .into_any_element()
        })
        .child(
            // 摘要常是中英混排，GPUI 的 `.truncate()` 会把省略号裁掉，见 `truncated_line`。
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child(truncated_line(preview.clone())),
        )
        .when_some(settled, |row, elapsed_ms| {
            row.child(
                div()
                    .flex_shrink_0()
                    .type_style(TYPE.text_xsmall)
                    .text_color(muted)
                    .child(settled_thought_label(elapsed_ms)),
            )
        });
        if interactive {
            row = row
                .role(gpui::Role::Button)
                .aria_label(preview)
                .when(!in_process, |row| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .type_style(TYPE.text_xsmall)
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
        let mut col = div().flex().flex_col().w_full().min_w_0().child(row);
        if has_details && (reveal.is_some() || expanded) {
            let details = div()
                .min_w_0()
                .pl(px(if in_process { 24. } else { 8. }))
                .pr_2()
                .pt_2()
                .pb_2()
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child(self.retained_markdown(
                    if in_process {
                        "acp-progress-md"
                    } else {
                        "acp-analysis-md"
                    },
                    entry_ix,
                    0,
                    cached_entry_markdown(&self.rendered_markdown, entry_ix, entry),
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
        let font = smelt_plugin_api::model::font_config::font_family();
        let body_el = match mode {
            ToolLogMode::Inline => {
                plain_text_view(owner_part("acp-tool-log-text", &owner, part_ix), body)
                    .type_style(TYPE.text_xsmall)
                    .text_color(muted)
                    .font_family(font)
                    .into_any_element()
            }
            ToolLogMode::Preview => plain_text_view(
                owner_part("acp-tool-log-text", &owner, part_ix),
                &preview.shown,
            )
            .type_style(TYPE.text_xsmall)
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
        div()
            .flex()
            .flex_col()
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
                    t!("toggle.collapse").into_owned()
                } else {
                    t!("toggle.expand_lines", count = total).into_owned()
                };
                block.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            Button::new(owner_part("acp-tool-toggle", &owner, part_ix))
                                .quiet()
                                .small()
                                .label(toggle_label)
                                .on_click(cx.listener(move |this, _ev, _window, cx| {
                                    if !this.expanded_tools.remove(&key) {
                                        this.expanded_tools.insert(key.clone());
                                    }
                                    this.list_state
                                        .remeasure_items(entry_ix..entry_ix.saturating_add(1));
                                    cx.stop_propagation();
                                    cx.notify();
                                })),
                        )
                        .child(self.render_copy_button(
                            CopySite::ToolLog {
                                tool_id: tool_id.to_string(),
                                part_ix,
                            },
                            copy_text,
                            t!("copy.all"),
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
    let row_font = smelt_plugin_api::model::font_config::font_family();
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
    /// 点缩略图看大图。hover 卡片拿不到 `Context<ConversationView>`，经弱引用转发事件。
    on_preview_image: PreviewImageHandler,
}

type PreviewImageHandler =
    std::rc::Rc<dyn Fn(std::sync::Arc<gpui::Image>, &mut Window, &mut App)>;

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
        on_preview_image,
    } = body;
    // 滚动容器必须是 block。v_flex 会把子项压进 max_h，content 高度等于视口，
    // 滚轮加在 offset 上也会被钳回 0。卡片本身不滚时，长日志的行视口自己滚。
    let mut col = div().flex().flex_col().w_full().p_2().gap_1();
    for (part_ix, part) in output.iter().enumerate() {
        col = match part {
            ToolOutputPart::Diff { path, .. } => {
                let cached = diffs
                    .and_then(|parts| parts.get(part_ix))
                    .and_then(Option::as_ref);
                col.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            plain_text_view(
                                owner_part("acp-tool-pop-diff-path", &tool_id, part_ix),
                                path,
                            )
                            .type_style(TYPE.text_xsmall)
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
                    // 解码由 popover 的 content 闭包触发（缓存按图片 id 全局共享）。
                    Some(image) => {
                        let preview_image = image.clone();
                        let on_preview = on_preview_image.clone();
                        col.child(
                            div()
                                .id(owner_part("acp-tool-pop-image", &tool_id, part_ix))
                                .overflow_hidden()
                                .rounded_md()
                                .border_1()
                                .border_color(border)
                                .hover(|s| s.border_color(ui_theme::hair_strong()))
                                .on_click(move |_, window, cx| {
                                    on_preview(preview_image.clone(), window, cx)
                                })
                                .child(image_thumb_element(image)),
                        )
                    }
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
        return plain_text_view(owner_part(text_name, tool_id, part_ix), body)
            .type_style(TYPE.text_xsmall)
            .text_color(muted)
            .font_family(smelt_plugin_api::model::font_config::font_family())
            .into_any_element();
    }
    let total = preview.total;
    let copy = body.to_string();
    div()
        .flex()
        .flex_col()
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
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(gpui::rgb(ui_theme::faint()))
                        .child(SharedString::from(crate::plural(
                            total,
                            t!("lines.total.one"),
                            t!("lines.total.other", count = total),
                        ))),
                )
                .child(
                    Clipboard::new(owner_part(copy_name, tool_id, part_ix))
                        .value(copy)
                        .icon_action(t!("copy.all")),
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
    let view = smelt_plugin_api::ui::markdown_mermaid::markdown_view_clickable(
        owner_part("acp-tool-pop-md", tool_id, part_ix),
        body.to_string(),
    )
    .type_style(TYPE.text_xsmall)
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

pub(super) fn render_nested_transcript(children: &[AcpEntry], muted: gpui::Hsla) -> gpui::AnyElement {
    let mut col = div().flex().flex_col().w_full().gap_1();
    for child in children {
        col = col.child(match child {
            AcpEntry::Assistant { text, thought } => {
                let line = text.lines().next().unwrap_or_default();
                if line.is_empty() {
                    div().into_any_element()
                } else {
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(muted)
                        .overflow_hidden()
                        .child(truncated_line(if *thought {
                            t!("thought.line", line = line).into_owned()
                        } else {
                            line.to_string()
                        }))
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
        gpui::rgb(ui_theme::bad()).into()
    } else {
        muted
    };
    let headline = if matches!(kind, ToolKind::Collaborate) {
        let visible = smelt_plugin_api::model::acp_chat::subagent_visible_title(title);
        if visible.is_empty() {
            t!("subagent.generic").into_owned()
        } else {
            visible
        }
    } else {
        compact_tool_headline(kind, title)
    };
    let mut row = div()
        .flex()
        .items_center()
        .w_full()
        .min_h(px(20.))
        .gap_2()
        .child(process_timeline_icon(&kind).size(px(12.)).text_color(ink))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .type_style(TYPE.text_xsmall)
                .text_color(ink)
                .child(truncated_line(headline)),
        );
    if failed {
        row = row.child(
            div()
                .flex_shrink_0()
                .type_style(TYPE.text_xsmall)
                .text_color(gpui::rgb(ui_theme::bad()))
                .child(SharedString::from(t!("tool.failed"))),
        );
    }
    if nested.is_empty() {
        row.into_any_element()
    } else {
        div()
            .flex()
            .flex_col()
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

/// 子智能体状态的图标。状态不只靠颜色：图标形状各不相同，旁边总有文字。
pub(super) fn subagent_status_icon(
    status: SubagentStatus,
    spinner_id: gpui::ElementId,
    animate_ambient: bool,
    muted: gpui::Hsla,
) -> gpui::AnyElement {
    let faint: gpui::Hsla = gpui::rgb(ui_theme::faint()).into();
    let (icon, color) = match status {
        SubagentStatus::Running => {
            return if animate_ambient {
                ambient_spinner(spinner_id, muted, animate_ambient)
            } else {
                Icon::new(IconName::Loader)
                    .size(px(13.))
                    .text_color(muted)
                    .into_any_element()
            };
        }
        SubagentStatus::Completed => (IconName::CircleCheck, faint),
        SubagentStatus::Failed => (IconName::CircleX, gpui::rgb(ui_theme::bad()).into()),
        SubagentStatus::Interrupted => (IconName::Warning, gpui::rgb(ui_theme::bad()).into()),
        SubagentStatus::Stopped => (IconName::Pause, faint),
    };
    Icon::new(icon)
        .size(px(13.))
        .text_color(color)
        .into_any_element()
}

/// 一个改动标签：文件名、合计 +/−。点开时描边加深、垫一层底色。
fn render_edit_chip(chip: &EditChip, group_key: usize, open: bool) -> gpui::Stateful<gpui::Div> {
    let mono = smelt_plugin_api::model::font_config::font_family();
    div()
        .flex()
        .items_center()
        .id(gpui::ElementId::from((
            gpui::ElementId::named_usize("acp-edit-chip", group_key),
            gpui::SharedString::from(chip.path.clone()),
        )))
        .flex_shrink_0()
        .gap_1p5()
        .px_2()
        .py(px(1.))
        .rounded_md()
        .border_1()
        .border_color(if open {
            ui_theme::hair_strong()
        } else {
            ui_theme::hair()
        })
        .when(open, |chip| chip.bg(ui_theme::row_hover()))
        .hover(|chip| chip.border_color(ui_theme::hair_strong()))
        .cursor_pointer()
        .font_family(mono)
        .type_style(TYPE.text_xsmall)
        .text_color(gpui::rgb(ui_theme::text()))
        .child(chip.label.clone())
        .when(chip.added > 0, |row| {
            row.child(
                div()
                    .text_color(gpui::rgb(ui_theme::ok()))
                    .child(format!("+{}", chip.added)),
            )
        })
        .when(chip.removed > 0, |row| {
            row.child(
                div()
                    .text_color(gpui::rgb(ui_theme::bad()))
                    .child(format!("−{}", chip.removed)),
            )
        })
}

/// 回合摘要和每段计数共用的一行：可选的状态标记、计数句、红色的失败数、展开箭头。
fn summary_row(
    id: impl Into<gpui::ElementId>,
    label: String,
    failed: Option<String>,
    expanded: bool,
    muted: gpui::Hsla,
) -> gpui::Stateful<gpui::Div> {
    div()
        .flex()
        .items_center()
        .id(id)
        .w_full()
        .h(ui_theme::height::sm())
        .flex_shrink_0()
        .gap_1p5()
        .cursor_pointer()
        .hover(|row| row.opacity(0.78))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child(truncated_line(label)),
        )
        .when_some(failed, |row, failed| {
            row.child(
                div()
                    .flex_shrink_0()
                    .type_style(TYPE.text_xsmall)
                    .text_color(gpui::rgb(ui_theme::bad()))
                    .child(format!("· {failed}")),
            )
        })
        .child(
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size(px(12.))
            .flex_shrink_0()
            .text_color(gpui::rgb(ui_theme::faint())),
        )
}

fn elapsed_suffix(elapsed_ms: u64, color: gpui::Hsla) -> gpui::Div {
    div()
        .flex_shrink_0()
        .type_style(TYPE.text_xsmall)
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
            .text_color(gpui::rgb(ui_theme::bad()))
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
/// 子智能体那一行：指上去才显出「停止」。
const SUBAGENT_STOP_GROUP: &str = "acp-subagent-stop-row";

/// 可展开行：平时左侧是工具图标，hover 换成箭头。展开仍靠点击。
fn expandable_lead_icon(
    rest_icon: impl Into<Icon>,
    expanded: bool,
    color: impl Into<gpui::Hsla>,
) -> gpui::AnyElement {
    let color = color.into();
    let rest_icon: Icon = rest_icon.into();
    hover_swap_lead(
        rest_icon.size(px(13.)).text_color(color).into_any_element(),
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
    smelt_plugin_api::ui::image::thumb_element(image, px(160.), px(280.), 8.0)
}

/// 可点开大图的缩略图卡片（已发消息里的图、工具返回的图共用）。
fn render_clickable_image_thumb(
    image: std::sync::Arc<gpui::Image>,
    id: impl Into<gpui::ElementId>,
    border: gpui::Hsla,
    cx: &Context<ConversationView>,
) -> gpui::AnyElement {
    // 每帧调用是安全的：已在途、已完成或已失败时不会再启动任务。
    smelt_plugin_api::ui::image::ensure_preview(&image, cx);
    let thumb = image_thumb_element(&image);
    let preview_image = image;
    div()
        .id(id)
        .overflow_hidden()
        .rounded_md()
        .border_1()
        .border_color(border)
        .hover(|image| image.border_color(ui_theme::hair_strong()))
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.emit(ConversationViewEvent::PreviewImage(preview_image.clone()));
        }))
        .child(thumb)
        .into_any_element()
}
