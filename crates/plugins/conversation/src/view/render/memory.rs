//! 输入框上方的记忆卡片：这一轮写下或改过的记忆，就地展开看正文、采纳、删除。
//!
//! 和输入框同宽、居中，贴在它上面。每行先说记了什么（描述），标识名和改动方式压成一行
//! 小字；颜色只留给要人拿主意的「待确认」。

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
/// 展开箭头占的宽度，正文和小字行跟描述对齐。
const CHEVRON_COLUMN: f32 = 12.;

fn layer_key(layer: AgentMemoryLayer) -> &'static str {
    match layer {
        AgentMemoryLayer::Agent => "agent",
        AgentMemoryLayer::User => "user",
    }
}

impl ConversationView {
    pub(super) fn render_memory_changes(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let changes = self.visible_memory_changes()?;
        let agent_id = changes.agent_id.clone();
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .pl_3()
            .pr_1p5()
            .pt_1p5()
            .child(
                Icon::new(IconName::Brain)
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
            .p_1()
            .children(
                changes
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(ix, entry)| self.render_memory_change_row(ix, entry, cx)),
            );
        let card = div()
            .flex()
            .flex_col()
            .w_full()
            .max_w(ui_theme::conversation_max_width())
            .rounded(ui_theme::card_radius())
            .border_1()
            .border_color(ui_theme::hair())
            .bg(gpui::rgb(ui_theme::surface()))
            .debug_selector(|| "ACP_MEMORY_CARD".into())
            .child(header)
            .child(rows)
            .children(self.memory_error.as_ref().map(|error| {
                div()
                    .px_3()
                    .pb_2()
                    .type_style(TYPE.text_xsmall)
                    .text_color(gpui::rgb(ui_theme::bad()))
                    .child(error.clone())
            }));
        // 和输入框一样居中、同宽，横向内边距也一样，两张卡片的边对齐。
        Some(
            div()
                .flex()
                .justify_center()
                .w_full()
                .px_4()
                .pt_2()
                .child(card)
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
        let pending = entry.pending_reason_text();
        let faint = gpui::rgb(ui_theme::faint());
        let row_id = format!("{}-{}", layer_key(entry.layer), entry.file_name);

        // 描述是给人看的那句；没写描述时退回标识名，小字行里就不再重复它。
        let (title, name) = if entry.description.is_empty() {
            (entry.name.clone(), None)
        } else {
            (entry.description.clone(), Some(entry.name.clone()))
        };
        let mut meta = Vec::new();
        meta.extend(name);
        meta.push(
            if entry.created {
                t!("memory.kind_created")
            } else {
                t!("memory.kind_changed")
            }
            .into_owned(),
        );
        if entry.layer == AgentMemoryLayer::User {
            // 全局记忆所有智能体共用，删除时影响面不同，标出来。
            meta.push(t!("memory.scope_global").into_owned());
        }

        let toggle_key = key.clone();
        let summary = div()
            .flex()
            .id(SharedString::from(format!("acp-memory-row-{row_id}")))
            .flex_1()
            .min_w(px(0.))
            .gap_1p5()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                this.toggle_memory_expanded(toggle_key.clone(), cx);
            }))
            .child(
                // 和描述第一行居中对齐。
                div().flex().items_center().h(px(20.)).child(
                    Icon::new(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(px(CHEVRON_COLUMN))
                    .flex_shrink_0()
                    .text_color(faint),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        div()
                            .min_w(px(0.))
                            .type_style(TYPE.ui)
                            .text_color(gpui::rgb(ui_theme::text()))
                            .when(!expanded, |text| text.truncate())
                            .child(title),
                    )
                    .child(
                        div()
                            .flex()
                            .min_w(px(0.))
                            .gap_1()
                            .type_style(TYPE.text_xsmall)
                            .text_color(faint)
                            // 待确认的原因放最前：采纳前要知道它为什么没生效。
                            .children(pending.into_iter().flat_map(|reason| {
                                [
                                    div()
                                        .flex_shrink_0()
                                        .text_color(gpui::rgb(ui_theme::need()))
                                        .child(
                                            t!("memory.pending_reason", reason = reason)
                                                .into_owned(),
                                        ),
                                    div().flex_shrink_0().child("·"),
                                ]
                            }))
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .when(!expanded, |text| text.truncate())
                                    .child(meta.join(" · ")),
                            ),
                    ),
            );

        let mut actions = div()
            .flex()
            .items_center()
            .flex_shrink_0()
            .h(px(20.))
            .gap_1();
        if busy {
            actions = actions.child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(faint)
                    .child(t!("status.working").into_owned()),
            );
        } else {
            let delete_key = key.clone();
            let delete = Button::new(SharedString::from(format!("acp-memory-delete-{row_id}")))
                .small()
                .disabled(any_busy);
            // 平时是安静的灰字，不抢描述的眼；点一下才变成红色的确认。
            let delete = if armed {
                delete.danger().label(t!("memory.confirm_delete")).tooltip(
                    if entry.layer == AgentMemoryLayer::User {
                        t!("memory.delete_global_hint")
                    } else {
                        t!("memory.delete_hint")
                    },
                )
            } else {
                delete.quiet().label(t!("memory.delete_button"))
            };
            actions = actions.child(delete.on_click(cx.listener(move |this, _ev, _window, cx| {
                this.request_memory_action(delete_key.clone(), MemoryActionKind::Delete, cx);
            })));
            if entry.pending_reason.is_some() {
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
        }

        div()
            .flex()
            .flex_col()
            .id(("acp-memory-change", ix))
            .w_full()
            .px_2()
            .py_1p5()
            .rounded(ui_theme::row_radius())
            .hover(|row| row.bg(gpui::rgb(ui_theme::soft())))
            .child(div().flex().w_full().gap_3().child(summary).child(actions))
            .when(expanded, |row| {
                row.child(
                    div()
                        .id(SharedString::from(format!("acp-memory-body-{row_id}")))
                        .mt_1p5()
                        .ml(px(CHEVRON_COLUMN + 6.))
                        .px_2p5()
                        .py_2()
                        .max_h(px(MEMORY_BODY_MAX_HEIGHT))
                        .overflow_y_scroll()
                        .rounded(px(6.))
                        .bg(ui_theme::overlay(0x0c))
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
