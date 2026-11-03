//! 输入框上方的记忆卡片：这一轮写下或改过的记忆，就地展开看正文、采纳、删除。

use super::*;
use gpui::SharedString;
use smelt_model::acp_session::MemoryChangeView;
use smelt_model::control_api::AgentMemoryLayer;

/// 卡片里的行列表最多这么高，再多就在卡片里滚动，不把消息流挤没。
const MEMORY_ROWS_MAX_HEIGHT: f32 = 240.;
/// 展开的正文最多这么高。
const MEMORY_BODY_MAX_HEIGHT: f32 = 160.;

fn layer_key(layer: AgentMemoryLayer) -> &'static str {
    match layer {
        AgentMemoryLayer::Agent => "agent",
        AgentMemoryLayer::User => "user",
    }
}

impl AcpView {
    pub(super) fn render_memory_changes(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let changes = self.visible_memory_changes()?;
        let border = cx.theme().border;
        let agent_id = changes.agent_id.clone();
        let header = h_flex()
            .items_center()
            .gap_2()
            .child(
                Icon::new(IconName::BookOpen)
                    .size_3p5()
                    .flex_shrink_0()
                    .text_color(gpui::rgb(ui_theme::accent())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_xs()
                    .text_color(gpui::rgb(ui_theme::text_mid()))
                    .child(memory_changes_summary(&changes.entries)),
            )
            .child(
                Button::new("acp-memory-open-page")
                    .quiet()
                    .compact()
                    .label("记忆页")
                    .text_color(gpui::rgb(ui_theme::text_muted()))
                    .tooltip("去这个智能体的记忆页，管理全部记忆")
                    .on_click(cx.listener(move |_this, _ev, _window, cx| {
                        cx.emit(AcpViewEvent::OpenAgentMemory {
                            agent_id: agent_id.clone(),
                        });
                    })),
            )
            .child(
                Button::new("acp-memory-dismiss")
                    .dismiss()
                    .icon(IconName::Close)
                    .icon_action("收起记忆卡片")
                    .on_click(cx.listener(|this, _ev, _window, cx| {
                        this.dismiss_memory_changes(cx);
                    })),
            );
        let rows = v_flex()
            .id("acp-memory-rows")
            .w_full()
            .max_h(px(MEMORY_ROWS_MAX_HEIGHT))
            .overflow_y_scroll()
            .gap_0p5()
            .children(
                changes
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(ix, entry)| self.render_memory_change_row(ix, entry, cx)),
            );
        Some(
            v_flex()
                .w_full()
                .px_4()
                .py_2()
                .gap_1p5()
                .border_t_1()
                .border_color(border)
                .bg(ui_theme::tint(ui_theme::accent(), 0x0c))
                .child(header)
                .child(rows)
                .children(self.memory_error.as_ref().map(|error| {
                    div()
                        .text_xs()
                        .text_color(gpui::rgb(ui_theme::red()))
                        .child(error.clone())
                }))
                .into_any_element(),
        )
    }

    fn render_memory_change_row(
        &self,
        ix: usize,
        entry: &MemoryChangeView,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let key: (AgentMemoryLayer, String) = (entry.layer, entry.file_name.clone());
        let expanded = self.memory_expanded.as_ref() == Some(&key);
        let armed = self.memory_delete_armed.as_ref() == Some(&key);
        let busy = self.memory_busy.as_ref() == Some(&key);
        let any_busy = self.memory_busy.is_some();
        let pending = entry.pending_reason.is_some();
        let faint = gpui::rgb(ui_theme::text_faint());
        let row_id = format!("{}-{}", layer_key(entry.layer), entry.file_name);

        let mut badges = h_flex().flex_shrink_0().gap_1().items_center();
        badges = badges.child(memory_badge(
            if entry.created { "新" } else { "改" },
            ui_theme::text_muted(),
        ));
        if entry.layer == AgentMemoryLayer::User {
            // 全局记忆所有智能体共用，删除时影响面不同，标出来。
            badges = badges.child(memory_badge("全局", ui_theme::blue()));
        }
        badges = badges.child(if pending {
            memory_badge("待确认", ui_theme::yellow())
        } else {
            memory_badge("已生效", ui_theme::green())
        });

        let subtitle = match &entry.pending_reason {
            // 待确认时把原因放前面：用户采纳前要知道它为什么没生效。
            Some(reason) if entry.description.is_empty() => reason.clone(),
            Some(reason) => format!("{reason}：{}", entry.description),
            None => entry.description.clone(),
        };

        let toggle_key = key.clone();
        let summary = h_flex()
            .id(SharedString::from(format!("acp-memory-row-{row_id}")))
            .flex_1()
            .min_w(px(0.))
            .gap_1p5()
            .items_center()
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                this.toggle_memory_expanded(toggle_key.clone(), cx);
            }))
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(px(12.))
                .flex_shrink_0()
                .text_color(faint),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        h_flex()
                            .min_w(px(0.))
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_xs()
                                    .text_color(gpui::rgb(ui_theme::text_bright()))
                                    .child(entry.name.clone()),
                            )
                            .child(badges),
                    )
                    .when(!subtitle.is_empty(), |col| {
                        col.child(
                            div()
                                .text_xs()
                                .text_color(faint)
                                .when(!expanded, |text| text.truncate())
                                .child(subtitle),
                        )
                    }),
            );

        let mut actions = h_flex().flex_shrink_0().gap_1().items_center();
        if busy {
            actions = actions.child(div().text_xs().text_color(faint).child("处理中…"));
        } else {
            if pending {
                let approve_key = key.clone();
                actions = actions.child(
                    Button::new(SharedString::from(format!("acp-memory-approve-{row_id}")))
                        .commit()
                        .compact()
                        .label("采纳")
                        .disabled(any_busy)
                        .tooltip("采纳：之后的对话会用到这条记忆")
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.request_memory_action(
                                approve_key.clone(),
                                AcpMemoryActionKind::Approve,
                                cx,
                            );
                        })),
                );
            }
            let delete_key = key.clone();
            let delete = Button::new(SharedString::from(format!("acp-memory-delete-{row_id}")))
                .compact()
                .disabled(any_busy);
            let delete = if armed {
                delete.danger().label("确认删除").tooltip(
                    if entry.layer == AgentMemoryLayer::User {
                        "全局记忆删了会从所有智能体里消失"
                    } else {
                        "删了就没了"
                    },
                )
            } else {
                delete.outline().label("删除")
            };
            actions = actions.child(delete.on_click(cx.listener(move |this, _ev, _window, cx| {
                this.request_memory_action(delete_key.clone(), AcpMemoryActionKind::Delete, cx);
            })));
        }

        v_flex()
            .id(("acp-memory-change", ix))
            .w_full()
            .px_2()
            .py_1()
            .rounded(ui_theme::row_radius())
            .hover(|row| row.bg(gpui::rgb(ui_theme::bg_row_hover())))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(summary)
                    .child(actions),
            )
            .when(expanded, |row| {
                row.child(
                    div()
                        .id(SharedString::from(format!("acp-memory-body-{row_id}")))
                        .mt_1()
                        .ml(px(18.))
                        .px_2()
                        .py_1p5()
                        .max_h(px(MEMORY_BODY_MAX_HEIGHT))
                        .overflow_y_scroll()
                        .rounded(ui_theme::row_radius())
                        .bg(ui_theme::overlay(0x10))
                        .text_xs()
                        .whitespace_normal()
                        .text_color(gpui::rgb(ui_theme::text_mid()))
                        .child(if entry.body.is_empty() {
                            "（正文为空）".to_string()
                        } else {
                            entry.body.clone()
                        }),
                )
            })
            .into_any_element()
    }
}

/// 行里的小标签：底色是字色叠一层淡色。
fn memory_badge(label: &'static str, color: u32) -> gpui::Div {
    div()
        .flex_shrink_0()
        .px_1()
        .rounded_sm()
        .text_size(px(10.))
        .bg(ui_theme::tint(color, 0x22))
        .text_color(gpui::rgb(color))
        .child(label)
}
