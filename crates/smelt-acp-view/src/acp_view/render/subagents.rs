//! Pi 子智能体在对话之外的入口：输入栏「N 个子智能体」、它的弹层，以及看一个子智能体
//! 过程的侧边视图。对话里的派发卡片在 `conversation`。

use super::conversation::{render_nested_transcript, subagent_status_icon};
use super::*;

impl AcpView {
    /// 输入栏入口：只在有子智能体在跑时出现，和「后台任务」分开计数。
    pub(super) fn render_subagent_chip(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let running = running_subagents(&self.subagents).len();
        if running == 0 {
            return None;
        }
        let summary = format!("{running} 个子智能体");
        Some(
            Button::new("acp-subagents")
                .quiet()
                .compact()
                .capsule()
                .selected(self.subagents_open)
                .accessibility_label(summary.clone())
                .child(Icon::new(IconName::Bot).size(px(12.)))
                .child(summary)
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.subagents_open = !this.subagents_open;
                    this.background_tasks_open = false;
                    this.usage_popover_open = false;
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    /// 在跑的子智能体：名字、在做什么、耗时、停止。点一行在侧边看它的过程。
    pub(super) fn render_subagent_popover(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        if !self.subagents_open {
            return None;
        }
        let running = running_subagents(&self.subagents);
        if running.is_empty() {
            return None;
        }
        let muted = cx.theme().muted_foreground;
        let faint = gpui::rgb(ui_theme::text_faint());
        let now = now_unix_ms();
        let mut rows = v_flex()
            .id("acp-subagent-list")
            .max_h(px(320.))
            .overflow_y_scroll()
            .gap_1();
        for view in running {
            let id = view.id.clone();
            let title = view.title.clone();
            let stopping = self.stopping_subagents.contains(&view.id);
            let stop_id = view.id.clone();
            let row = h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(subagent_status_icon(
                    view.status,
                    (gpui::ElementId::from("acp-subagent-popover-spin"), id.clone()).into(),
                    true,
                    muted,
                ))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .text_color(gpui::rgb(ui_theme::text_bright()))
                                .child(title.clone()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(faint)
                                .child(view.last_activity.clone().unwrap_or_else(|| "准备中".into())),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(faint)
                        .child(background_task_elapsed(view.started_at_ms, now)),
                )
                .child(if stopping {
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(gpui::rgb(ui_theme::text_muted()))
                        .child("正在停止…")
                        .into_any_element()
                } else {
                    Button::new((gpui::ElementId::from("acp-subagent-popover-stop"), stop_id.clone()))
                        .danger()
                        .icon_size()
                        .label("停止")
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            cx.stop_propagation();
                            this.stop_subagent(stop_id.clone(), cx);
                        }))
                        .into_any_element()
                });
            rows = rows.child(
                Button::new((gpui::ElementId::from("acp-subagent-popover-row"), id.clone()))
                    .quiet()
                    .w_full()
                    .justify_start()
                    .accessibility_label(format!("查看子智能体 {title} 的过程"))
                    .child(row)
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.open_subagent_inspector(id.clone(), title.clone(), cx);
                    })),
            );
        }
        let panel = v_flex()
            .id("acp-subagent-popover")
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
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .text_color(gpui::rgb(ui_theme::text_mid()))
                            .child("子智能体"),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("acp-subagent-stop-all")
                            .danger()
                            .icon_size()
                            .label("全部停止")
                            .on_click(cx.listener(|this, _ev, _window, cx| {
                                this.stop_all_subagents(cx);
                            })),
                    )
                    .child(
                        Button::new("acp-subagent-popover-close")
                            .dismiss()
                            .icon(IconName::Close)
                            .icon_action("关闭")
                            .on_click(cx.listener(|this, _ev, _window, cx| {
                                this.subagents_open = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(rows);
        Some(
            div()
                .id("acp-subagent-popover-layer")
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

    /// 侧边视图：一个子智能体的过程，只读。运行中能停；子会话每多一条消息就重读一次。
    pub(super) fn render_subagent_inspector(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let inspector = self.subagent_inspector.as_ref()?;
        let t = cx.theme();
        let muted = t.muted_foreground;
        let faint = gpui::rgb(ui_theme::text_faint());
        let view = self.subagents.iter().find(|view| view.id == inspector.id);
        let status_line = match view {
            Some(view) => {
                let mut parts = vec![view.status.label().to_string()];
                if let Some(model) = view.model.as_ref().filter(|model| !model.is_empty()) {
                    parts.push(model.clone());
                }
                parts.push(format_duration(subagent_elapsed_ms(view, now_unix_ms())));
                parts.join(" · ")
            }
            None => inspector.id.clone(),
        };
        let body: gpui::AnyElement = if let Some(error) = inspector.error.as_ref() {
            div()
                .text_sm()
                .text_color(gpui::rgb(ui_theme::red()))
                .child(format!("读不到这个子智能体的过程：{error}"))
                .into_any_element()
        } else if inspector.entries.is_empty() {
            div()
                .text_sm()
                .text_color(faint)
                .child(if inspector.loading || inspector.revision.is_none() {
                    "正在读取…"
                } else {
                    "还没有内容"
                })
                .into_any_element()
        } else {
            render_nested_transcript(&inspector.entries, muted)
        };
        let stop = view.and_then(|view| {
            if !view.status.is_running() {
                return None;
            }
            let stop_id = view.id.clone();
            Some(if self.stopping_subagents.contains(&view.id) {
                div()
                    .text_xs()
                    .text_color(gpui::rgb(ui_theme::text_muted()))
                    .child("正在停止…")
                    .into_any_element()
            } else {
                Button::new("acp-subagent-inspector-stop")
                    .danger_outline()
                    .compact()
                    .label("停止")
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.stop_subagent(stop_id.clone(), cx);
                    }))
                    .into_any_element()
            })
        });
        Some(
            v_flex()
                .id("acp-subagent-inspector")
                .w(px(440.))
                .flex_shrink_0()
                .h_full()
                .border_l_1()
                .border_color(t.border)
                .bg(gpui::rgb(ui_theme::bg_bar()))
                .child(
                    h_flex()
                        .w_full()
                        .px_4()
                        .pt(px(ui_theme::CHROME_ISLAND_CLEARANCE_PX))
                        .pb_2()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(t.border)
                        .child(Icon::new(IconName::Bot).size(px(14.)).text_color(muted))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .truncate()
                                        .text_sm()
                                        .font_medium()
                                        .text_color(gpui::rgb(ui_theme::text_bright()))
                                        .child(inspector.title.clone()),
                                )
                                .child(div().truncate().text_xs().text_color(faint).child(status_line)),
                        )
                        .children(stop)
                        .child(
                            Button::new("acp-subagent-inspector-close")
                                .dismiss()
                                .icon(IconName::Close)
                                .icon_action("关闭子智能体视图")
                                .on_click(cx.listener(|this, _ev, _window, cx| {
                                    this.close_subagent_inspector(cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .id("acp-subagent-inspector-body")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px_4()
                        .py_3()
                        .child(body),
                )
                .into_any_element(),
        )
    }
}
