//! 输入框上方的记忆卡片：这一轮写下或改过的记忆，就地展开看正文、采纳、删除。

use super::*;
use gpui::SharedString;
use rust_i18n::t;
use smelt_plugin_api::model::acp_session::MemoryChangeView;
use smelt_plugin_api::model::control_api::AgentMemoryLayer;
use smelt_plugin_api::ui::kit::TYPE;

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

impl ConversationView {
    pub(super) fn render_memory_changes(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let changes = self.visible_memory_changes()?;
        let border = ui_theme::hair();
        let agent_id = changes.agent_id.clone();
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Icon::new(IconName::BookOpen)
                    .size_3p5()
                    .flex_shrink_0()
                    .text_color(gpui::rgb(ui_theme::muted())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .type_style(TYPE.text_xsmall)
                    .text_color(gpui::rgb(ui_theme::muted()))
                    .child(memory_changes_summary(&changes.entries)),
            )
            .child(
                Button::new("acp-memory-open-page")
                    .quiet()
                    .small()
                    .label(t!("memory.page"))
                    .tooltip(t!("memory.page_hint"))
                    .on_click(cx.listener(move |_this, _ev, _window, cx| {
                        cx.emit(ConversationViewEvent::OpenAgentMemory {
                            agent_id: agent_id.clone(),
                        });
                    })),
            )
            .child(
                Button::new("acp-memory-dismiss")
                    .dismiss()
                    .icon(IconName::Close)
                    .icon_action(t!("memory.collapse"))
                    .on_click(cx.listener(|this, _ev, _window, cx| {
                        this.dismiss_memory_changes(cx);
                    })),
            );
        let rows = div()
            .flex()
            .flex_col()
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
            div()
                .flex()
                .flex_col()
                .w_full()
                .px_4()
                .py_2()
                .gap_1p5()
                .border_t_1()
                .border_color(border)
                .bg(ui_theme::shade(ui_theme::side()))
                .child(header)
                .child(rows)
                .children(self.memory_error.as_ref().map(|error| {
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(gpui::rgb(ui_theme::bad()))
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
        let faint = gpui::rgb(ui_theme::faint());
        let row_id = format!("{}-{}", layer_key(entry.layer), entry.file_name);

        let mut badges = div().flex().items_center().flex_shrink_0().gap_1();
        badges = badges.child(memory_badge(
            if entry.created {
                t!("memory.badge_new")
            } else {
                t!("memory.badge_changed")
            },
            ui_theme::muted(),
        ));
        if entry.layer == AgentMemoryLayer::User {
            // 全局记忆所有智能体共用，删除时影响面不同，标出来。
            badges = badges.child(memory_badge(t!("memory.badge_global"), ui_theme::run()));
        }
        badges = badges.child(if pending {
            memory_badge(t!("memory.badge_pending"), ui_theme::need())
        } else {
            memory_badge(t!("memory.badge_active"), ui_theme::ok())
        });

        let subtitle = match entry.pending_reason_text() {
            // 待确认时把原因放前面：用户采纳前要知道它为什么没生效。
            Some(reason) if entry.description.is_empty() => reason,
            Some(reason) => t!(
                "memory.pending_subtitle",
                reason = reason,
                description = entry.description
            )
            .into_owned(),
            None => entry.description.clone(),
        };

        let toggle_key = key.clone();
        let summary = div()
            .flex()
            .items_center()
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
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .min_w(px(0.))
                            .gap_2()
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .truncate()
                                    .type_style(TYPE.text_xsmall)
                                    .text_color(gpui::rgb(ui_theme::text()))
                                    .child(entry.name.clone()),
                            )
                            .child(badges),
                    )
                    .when(!subtitle.is_empty(), |col| {
                        col.child(
                            div()
                                .type_style(TYPE.text_xsmall)
                                .text_color(faint)
                                .when(!expanded, |text| text.truncate())
                                .child(subtitle),
                        )
                    }),
            );

        let mut actions = div().flex().items_center().flex_shrink_0().gap_1();
        if busy {
            actions = actions.child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(faint)
                    .child(t!("status.working").into_owned()),
            );
        } else {
            if pending {
                let approve_key = key.clone();
                actions = actions.child(
                    Button::new(SharedString::from(format!("acp-memory-approve-{row_id}")))
                        .primary()
                        .small()
                        .label(t!("memory.approve_button"))
                        .disabled(any_busy)
                        .tooltip(t!("memory.approve_hint"))
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.request_memory_action(
                                approve_key.clone(),
                                MemoryActionKind::Approve,
                                cx,
                            );
                        })),
                );
            }
            let delete_key = key.clone();
            let delete = Button::new(SharedString::from(format!("acp-memory-delete-{row_id}")))
                .small()
                .disabled(any_busy);
            let delete = if armed {
                delete.danger().label(t!("memory.confirm_delete")).tooltip(
                    if entry.layer == AgentMemoryLayer::User {
                        t!("memory.delete_global_hint")
                    } else {
                        t!("memory.delete_hint")
                    },
                )
            } else {
                delete.danger_quiet().label(t!("memory.delete_button"))
            };
            actions = actions.child(delete.on_click(cx.listener(move |this, _ev, _window, cx| {
                this.request_memory_action(delete_key.clone(), MemoryActionKind::Delete, cx);
            })));
        }

        div()
            .flex()
            .flex_col()
            .id(("acp-memory-change", ix))
            .w_full()
            .px_2()
            .py_1()
            .rounded(ui_theme::row_radius())
            .hover(|row| row.bg(gpui::rgb(ui_theme::soft())))
            .child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .gap_2()
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
                        .type_style(TYPE.text_xsmall)
                        .whitespace_normal()
                        .text_color(gpui::rgb(ui_theme::muted()))
                        .child(if entry.body.is_empty() {
                            t!("memory.empty_body").into_owned()
                        } else {
                            entry.body.clone()
                        }),
                )
            })
            .into_any_element()
    }
}

/// 行里的小标签：底色是字色叠一层淡色。
fn memory_badge(label: impl Into<SharedString>, color: u32) -> gpui::Div {
    let label = label.into();
    div()
        .flex_shrink_0()
        .px_1()
        .rounded_sm()
        .type_style(TYPE.small)
        .bg(ui_theme::tint(color, 0x22))
        .text_color(gpui::rgb(color))
        .child(label)
}
