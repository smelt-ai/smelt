//! ACP 审批卡与选择题卡。

use super::*;

impl AcpView {
    pub(super) fn render_approval_ui(
        &self,
        animate_ambient: bool,
        cx: &Context<Self>,
    ) -> (Option<gpui::AnyElement>, Option<gpui::AnyElement>) {
        let t = cx.theme();
        let muted = t.muted_foreground;
        let active_permission = self.permissions.first();
        let permission_is_submitting = self.permission_submitting.is_some();
        let permission_buttons = |card: &PendingPermission| {
            if permission_is_submitting {
                return h_flex()
                    .w_full()
                    .flex_shrink_0()
                    .h(px(36.))
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .text_color(muted)
                    .child(ambient_spinner(
                        "acp-permission-submit-spinner",
                        muted,
                        animate_ambient,
                    ))
                    .child("处理中…")
                    .into_any_element();
            }
            let tool_call_id = card.tool_call_id.clone();
            let primary_ix = card.options.iter().position(|o| {
                matches!(
                    o.kind,
                    PermissionOptionKindView::AllowOnce | PermissionOptionKindView::AllowAlways
                )
            });
            // `flex_wrap` 在这个固定于 composer 上方的纵向卡片里会漏算换行后的
            // 高度，导致第二行画到卡片外。每个操作独占一行，既保证卡片测量正确，
            // 也让长选项名称不会挤压或遮住其它操作。
            let mut buttons = v_flex().w_full().min_w_0().flex_shrink_0().gap_2();
            if let Some(pix) = primary_ix {
                let name = card.options[pix].name.clone();
                let option_id = card.options[pix].option_id.clone();
                let tool_call_id = tool_call_id.clone();
                // 主按钮改胶囊 + hover 时轻微上浮带阴影——批准是这张卡最想让人点的
                // 动作，得比其余选项更有「弹一下」的手感，不只是纯色块换个透明度。
                buttons = buttons.child(
                    h_flex().w_full().min_w_0().child(
                        div()
                            .id(format!("acp-perm-primary-{option_id}"))
                            .relative()
                            .h(px(36.))
                            .px_4()
                            .flex()
                            .items_center()
                            .rounded_full()
                            .bg(gpui::rgb(ui_theme::green()))
                            .text_color(gpui::rgb(ui_theme::on_accent()))
                            .text_sm()
                            .font_semibold()
                            .cursor_pointer()
                            .shadow_sm()
                            .hover(|d| d.opacity(0.9).shadow_md().top(px(-1.)))
                            .child(format!("{name} ⌘⏎"))
                            .on_click(cx.listener(move |this, _ev, _window, cx| {
                                this.pick_permission(&tool_call_id, &option_id, cx);
                            })),
                    ),
                );
            }
            for (ix, opt) in card.options.iter().enumerate() {
                if Some(ix) == primary_ix {
                    continue;
                }
                let danger = matches!(
                    opt.kind,
                    PermissionOptionKindView::RejectOnce | PermissionOptionKindView::RejectAlways
                );
                let option_id = opt.option_id.clone();
                let tool_call_id = tool_call_id.clone();
                // 次级选项也改软底胶囊：danger 用红色调软底，其余用中性灰软底,
                // 不再是空心线框——跟主按钮的实心胶囊放一起才是同一套语言，
                // 而不是「一个填色一个描边」的两套风格拼在一起。
                let bg_u32 = if danger {
                    ui_theme::red()
                } else {
                    ui_theme::text_muted()
                };
                let button = div()
                    .id(format!("acp-perm-opt-{option_id}"))
                    .h(px(36.))
                    .px_3p5()
                    .flex()
                    .items_center()
                    .rounded_full()
                    .bg(ui_theme::tint(bg_u32, 0x1c))
                    .text_sm()
                    .cursor_pointer()
                    .when(danger, |d| d.text_color(gpui::rgb(ui_theme::red())))
                    .hover(|d| d.bg(ui_theme::tint(bg_u32, 0x30)))
                    .child(opt.name.clone())
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.pick_permission(&tool_call_id, &option_id, cx);
                    }));
                buttons = buttons.child(h_flex().w_full().min_w_0().child(button));
            }
            buttons.into_any_element()
        };
        let permission = active_permission.map(|pending| {
            let remaining = self.permissions.len();
            let details = match &pending.details {
                ApprovalDetailsView::Command {
                    command,
                    cwd,
                    reason,
                } => v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_normal()
                            .text_sm()
                            .font_family(smelt_core::font_config::font_family())
                            .text_color(gpui::rgb(ui_theme::text_mid()))
                            .child(command.clone()),
                    )
                    .children(
                        reason
                            .as_ref()
                            .map(|reason| div().text_xs().text_color(muted).child(reason.clone())),
                    )
                    .children(cwd.as_ref().map(|cwd| {
                        div()
                            .text_xs()
                            .font_family(smelt_core::font_config::font_family())
                            .text_color(muted)
                            .child(format!("工作目录：{cwd}"))
                    }))
                    .into_any_element(),
                ApprovalDetailsView::FileChange { reason, grant_root } => v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(gpui::rgb(ui_theme::text_mid()))
                            .child(reason.clone().unwrap_or_else(|| pending.question.clone())),
                    )
                    .children(grant_root.as_ref().map(|root| {
                        div()
                            .text_xs()
                            .font_family(smelt_core::font_config::font_family())
                            .text_color(muted)
                            .child(format!("授权目录：{root}"))
                    }))
                    .into_any_element(),
                ApprovalDetailsView::Permissions { summary } => div()
                    .text_sm()
                    .text_color(gpui::rgb(ui_theme::text_mid()))
                    .child(summary.clone())
                    .into_any_element(),
                ApprovalDetailsView::Generic => div()
                    .text_sm()
                    .text_color(gpui::rgb(ui_theme::text_mid()))
                    .child(pending.question.clone())
                    .into_any_element(),
            };
            let details = v_flex()
                .id("acp-permission-details")
                .w_full()
                .min_w_0()
                .max_h(px(176.))
                .overflow_y_scroll()
                .flex_shrink_0()
                .child(details);
            let permission_ping = animate_ambient.then(|| {
                ambient_animation(
                    "acp-permission-ping",
                    std::time::Duration::from_millis(1600),
                    true,
                    |delta| {
                        let scale = 1.0 + delta * 1.6;
                        div()
                            .absolute()
                            .inset_0()
                            .rounded_full()
                            .bg(gpui::rgb(ui_theme::yellow()))
                            .opacity((1.0 - delta).max(0.0) * 0.7)
                            .size(px(8. * scale))
                    },
                )
            });
            v_flex()
                .w_full()
                .items_center()
                .px_4()
                .pt_3()
                .pb_3()
                .flex_shrink_0()
                .child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .flex_shrink_0()
                        .max_w(ui_theme::conversation_max_width())
                        .p_4()
                        .gap_3()
                        .rounded(ui_theme::card_radius())
                        .border_1()
                        .border_color(gpui::rgb(ui_theme::yellow()))
                        .bg(ui_theme::tint(ui_theme::yellow(), 0x0c))
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    // 需要批准的点缀一个扩散的 ping 环——跟系统级
                                    // 通知红点常见的那种脉冲一样，比静止圆点更有
                                    // 「这里正等你」的紧迫感，而不是容易被忽略的
                                    // 一个死圆点。
                                    div().relative().size_2().children(permission_ping).child(
                                        div()
                                            .absolute()
                                            .inset_0()
                                            .size_2()
                                            .rounded_full()
                                            .bg(gpui::rgb(ui_theme::yellow())),
                                    ),
                                )
                                .child(div().text_sm().font_semibold().child("需要批准"))
                                .child(div().flex_1())
                                .when(remaining > 1, |row| {
                                    row.child(
                                        div()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_full()
                                            .bg(ui_theme::overlay(0x18))
                                            .text_xs()
                                            .text_color(muted)
                                            .child(format!("{remaining} 项待处理")),
                                    )
                                }),
                        )
                        .child(details)
                        .child(permission_buttons(pending)),
                )
                .into_any_element()
        });
        // 选择题跟审批卡同一套居中栏；描边走 Grok 强调蓝，不走警告黄。
        // 单字段单选点击即提交；多选是勾选列表，选齐后亮「提交」。
        let elicitation = self.elicitation.as_ref().map(|card| {
            let ready = self.elicit_ready(cx);
            let has_multi = card
                .fields
                .iter()
                .any(|field| matches!(field.kind, ElicitFieldKindView::MultiSelect(_)));
            let multi_field = card.fields.len() > 1
                || card
                    .fields
                    .first()
                    .is_some_and(|f| !matches!(f.kind, ElicitFieldKindView::Select(_)));
            let show_footer = multi_field
                && !matches!(
                    card.fields.as_slice(),
                    [smelt_core::acp_session::ElicitFieldView {
                        kind: ElicitFieldKindView::ExternalUrl(_),
                        ..
                    }]
                );
            let skip = div()
                .id("acp-elicit-skip")
                .h(px(28.))
                .px_3()
                .flex()
                .items_center()
                .rounded_full()
                .text_sm()
                .text_color(muted)
                .cursor_pointer()
                .hover(|d| d.opacity(0.8))
                .child("跳过")
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.dismiss_elicitation(cx);
                }));
            let mut inner = v_flex()
                .id("acp-elicit-card")
                .debug_selector(|| "acp-elicit-card".to_string())
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .max_w(ui_theme::conversation_max_width())
                .p_4()
                .gap_3()
                .rounded(ui_theme::card_radius())
                .border_1()
                .border_color(gpui::rgb(ui_theme::accent()))
                .bg(ui_theme::tint(ui_theme::accent(), 0x14))
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .child(div().text_sm().font_semibold().child("等你选择"))
                        .child(div().flex_1())
                        .when(has_multi, |row| {
                            row.child(
                                div()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_full()
                                    .bg(ui_theme::overlay(0x18))
                                    .text_xs()
                                    .text_color(muted)
                                    .child("可多选"),
                            )
                        })
                        .child(skip),
                )
                .when(!card.message.trim().is_empty(), |card_el| {
                    card_el.child(
                        div()
                            .w_full()
                            .min_w_0()
                            .text_sm()
                            .text_color(muted)
                            .child(card.message.clone()),
                    )
                });
            for (fix, field) in card.fields.iter().enumerate() {
                if let ElicitFieldKindView::ExternalUrl(url) = &field.kind {
                    let url = url.clone();
                    inner = inner.child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().text_color(muted).child(field.title.clone()))
                            .child(
                                div()
                                    .id(("acp-elicit-url", fix))
                                    .px_3()
                                    .py_2()
                                    .rounded_lg()
                                    .bg(gpui::rgb(ui_theme::blue()))
                                    .text_color(gpui::white())
                                    .text_sm()
                                    .cursor_pointer()
                                    .hover(|d| d.opacity(0.85))
                                    .child("打开并继续")
                                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                                        cx.open_url(&url);
                                        this.submit_elicitation(cx);
                                    })),
                            ),
                    );
                    continue;
                }
                if let ElicitFieldKindView::Text { secret } = &field.kind {
                    let input = self.elicitation_inputs.get(&fix).cloned();
                    inner = inner.child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().text_color(muted).child(format!(
                                "{}{}",
                                field.title,
                                if *secret { "（保密）" } else { "" }
                            )))
                            .children(input.map(|input| Input::new(&input).w_full())),
                    );
                    continue;
                }
                let (options, is_multi) = match &field.kind {
                    ElicitFieldKindView::Select(o) => (o, false),
                    ElicitFieldKindView::MultiSelect(o) => (o, true),
                    ElicitFieldKindView::Text { .. } => unreachable!(),
                    ElicitFieldKindView::ExternalUrl(_) => unreachable!(),
                };
                let chosen = card.chosen.get(&fix).cloned().unwrap_or_default();
                let mut list = v_flex().w_full().min_w_0().flex_shrink_0().gap_2();
                for (oix, opt) in options.iter().enumerate() {
                    let selected = chosen.contains(&oix);
                    let accent = gpui::rgb(ui_theme::accent());
                    list = list.child(
                        h_flex().w_full().min_w_0().child(
                            div()
                                .id(format!("acp-elicit-opt-{fix}-{oix}"))
                                .debug_selector(move || format!("acp-elicit-opt-{fix}-{oix}"))
                                .w_full()
                                .min_w_0()
                                .min_h(px(36.))
                                .px_3p5()
                                .py_2()
                                .flex()
                                .items_start()
                                .gap_2()
                                .rounded_lg()
                                .border_1()
                                .border_color(if selected { accent.into() } else { t.border })
                                .bg(if selected {
                                    ui_theme::tint(ui_theme::accent(), 0x24)
                                } else {
                                    ui_theme::overlay(0x10)
                                })
                                .cursor_pointer()
                                .hover(|d| d.opacity(0.85))
                                .child(
                                    div()
                                        .mt(px(2.))
                                        .size(px(16.))
                                        .flex_shrink_0()
                                        .border_1()
                                        .border_color(if selected {
                                            accent.into()
                                        } else {
                                            t.border
                                        })
                                        .when(is_multi, |d| d.rounded(px(4.)))
                                        .when(!is_multi, |d| d.rounded_full())
                                        .when(selected, |d| d.bg(accent)),
                                )
                                .child(div().min_w_0().flex_1().text_sm().child(opt.label.clone()))
                                .on_click(cx.listener(move |this, _ev, _window, cx| {
                                    this.pick_elicit_option(fix, oix, cx);
                                })),
                        ),
                    );
                }
                if field.allow_custom_input {
                    let input = self.elicitation_inputs.get(&fix).cloned();
                    list = list.children(input.map(|input| {
                        v_flex()
                            .w_full()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("以上都不是？输入自己的答案"),
                            )
                            .child(Input::new(&input).w_full())
                    }));
                }
                inner = inner.child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap_1()
                        .when(card.fields.len() > 1, |d| {
                            d.child(div().text_xs().text_color(muted).child(format!(
                                "{}{}",
                                field.title.clone(),
                                if is_multi { "（可多选）" } else { "" }
                            )))
                        })
                        .child(list),
                );
            }
            if show_footer {
                inner = inner.child(
                    h_flex().w_full().gap_2().items_center().child(
                        div()
                            .id("acp-elicit-submit")
                            .debug_selector(|| "acp-elicit-submit".to_string())
                            .h(px(36.))
                            .px_4()
                            .flex()
                            .items_center()
                            .rounded_full()
                            .text_sm()
                            .font_semibold()
                            .when(ready, |d| {
                                d.bg(gpui::rgb(ui_theme::action_fill()))
                                    .text_color(gpui::rgb(ui_theme::action_on()))
                                    .cursor_pointer()
                                    .hover(|x| x.opacity(0.9))
                            })
                            .when(!ready, |d| {
                                d.border_1().border_color(t.border).text_color(muted)
                            })
                            .child("提交")
                            .on_click(cx.listener(|this, _ev, _window, cx| {
                                if this.elicit_ready(cx) {
                                    this.submit_elicitation(cx);
                                }
                            })),
                    ),
                );
            }
            v_flex()
                .w_full()
                .items_center()
                .px_4()
                .pt_3()
                .pb_3()
                .flex_shrink_0()
                .child(inner)
        });
        (permission, elicitation.map(|el| el.into_any_element()))
    }
}
