//! ACP 审批卡与选择题卡。

use super::*;
use rust_i18n::t;
use smelt_plugin_api::ui::kit::TYPE;
use smelt_plugin_api::ui::{Checkbox, Radio, TextInput};

/// 选择题的一行。外框、字号和悬停透明度留在这里；勾选语义交给 smelt-ui 的单选 / 复选框。
fn elicit_choice_row<E>(row: E, selected: bool) -> E
where
    E: gpui::Styled + gpui::InteractiveElement,
{
    let ink = gpui::rgb(ui_theme::ink());
    row.w_full()
        .min_w_0()
        .min_h(px(36.))
        .px_3p5()
        .py_2()
        .type_style(TYPE.ui)
        .rounded_lg()
        .border_1()
        .border_color(if selected {
            ink.into()
        } else {
            ui_theme::hair()
        })
        .bg(if selected {
            gpui::rgb(ui_theme::soft_hover())
        } else {
            ui_theme::overlay(0x10)
        })
        .hover(|row| row.opacity(0.85))
}

impl ConversationView {
    pub(super) fn render_approval_ui(
        &self,
        animate_ambient: bool,
        cx: &Context<Self>,
    ) -> (Option<gpui::AnyElement>, Option<gpui::AnyElement>) {
        let muted: gpui::Hsla = gpui::rgb(ui_theme::muted()).into();
        let active_permission = self.permissions.first();
        let permission_is_submitting = self.permission_submitting.is_some();
        let permission_buttons = |card: &PendingPermission| {
            if permission_is_submitting {
                return div()
                    .flex()
                    .items_center()
                    .w_full()
                    .flex_shrink_0()
                    .h(ui_theme::height::lg())
                    .gap_2()
                    .type_style(TYPE.ui)
                    .text_color(muted)
                    .child(ambient_spinner(
                        "acp-permission-submit-spinner",
                        muted,
                        animate_ambient,
                    ))
                    .child(t!("status.working").into_owned())
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
            let mut buttons = div()
                .flex()
                .flex_col()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .gap_2();
            if let Some(pix) = primary_ix {
                let name = card.options[pix].name.clone();
                let option_id = card.options[pix].option_id.clone();
                let tool_call_id = tool_call_id.clone();
                buttons = buttons.child(
                    div().flex().items_center().w_full().min_w_0().child(
                        Button::new((gpui::ElementId::from("acp-perm-primary"), option_id.clone()))
                            .primary()
                            .label(format!("{name} ⌘⏎"))
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
                // 拒绝不毁东西，和其它选项一样是次要按钮；红色留给删除、停止这类不可逆的。
                let option_id = opt.option_id.clone();
                let tool_call_id = tool_call_id.clone();
                let button =
                    Button::new((gpui::ElementId::from("acp-perm-opt"), option_id.clone()));
                let button = button.label(opt.name.clone()).on_click(cx.listener(
                    move |this, _ev, _window, cx| {
                        this.pick_permission(&tool_call_id, &option_id, cx);
                    },
                ));
                buttons =
                    buttons.child(div().flex().items_center().w_full().min_w_0().child(button));
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
                } => div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_normal()
                            .type_style(TYPE.ui)
                            .font_family(smelt_plugin_api::model::font_config::font_family())
                            .text_color(gpui::rgb(ui_theme::muted()))
                            .child(command.clone()),
                    )
                    .children(reason.as_ref().map(|reason| {
                        div()
                            .type_style(TYPE.text_xsmall)
                            .text_color(muted)
                            .child(reason.clone())
                    }))
                    .children(cwd.as_ref().map(|cwd| {
                        div()
                            .type_style(TYPE.text_xsmall)
                            .font_family(smelt_plugin_api::model::font_config::font_family())
                            .text_color(muted)
                            .child(SharedString::from(t!("approval.cwd", path = cwd)))
                    }))
                    .into_any_element(),
                ApprovalDetailsView::FileChange { reason, grant_root } => div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .type_style(TYPE.ui)
                            .text_color(gpui::rgb(ui_theme::muted()))
                            .child(reason.clone().unwrap_or_else(|| pending.question.clone())),
                    )
                    .children(grant_root.as_ref().map(|root| {
                        div()
                            .type_style(TYPE.text_xsmall)
                            .font_family(smelt_plugin_api::model::font_config::font_family())
                            .text_color(muted)
                            .child(SharedString::from(t!("approval.root", path = root)))
                    }))
                    .into_any_element(),
                ApprovalDetailsView::Permissions { summary } => div()
                    .type_style(TYPE.ui)
                    .text_color(gpui::rgb(ui_theme::muted()))
                    .child(summary.clone())
                    .into_any_element(),
                ApprovalDetailsView::Generic => div()
                    .type_style(TYPE.ui)
                    .text_color(gpui::rgb(ui_theme::muted()))
                    .child(pending.question.clone())
                    .into_any_element(),
            };
            let details = div()
                .flex()
                .flex_col()
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
                            .bg(gpui::rgb(ui_theme::need()))
                            .opacity((1.0 - delta).max(0.0) * 0.7)
                            .size(px(8. * scale))
                    },
                )
            });
            div()
                .flex()
                .flex_col()
                .w_full()
                .items_center()
                .px_4()
                .pt_3()
                .pb_3()
                .flex_shrink_0()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .w_full()
                        .min_w_0()
                        .flex_shrink_0()
                        .max_w(ui_theme::conversation_max_width())
                        .p_4()
                        .gap_3()
                        .rounded(ui_theme::card_radius())
                        .border_1()
                        .border_color(gpui::rgb(ui_theme::need()))
                        .bg(ui_theme::tint(ui_theme::need(), 0x0c))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
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
                                            .bg(gpui::rgb(ui_theme::need())),
                                    ),
                                )
                                .child(
                                    div()
                                        .type_style(TYPE.ui_medium)
                                        .child(t!("approval.title").into_owned()),
                                )
                                .child(div().flex_1())
                                .when(remaining > 1, |row| {
                                    row.child(
                                        div()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_full()
                                            .bg(ui_theme::overlay(0x18))
                                            .type_style(TYPE.text_xsmall)
                                            .text_color(muted)
                                            .child(SharedString::from(t!(
                                                "approval.remaining",
                                                count = remaining
                                            ))),
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
            let requires_explicit_submit = card.fields.len() > 1
                || card.fields.iter().any(|field| {
                    !matches!(field.kind, ElicitFieldKindView::Select(_))
                        || field.allow_custom_input
                });
            let show_footer = requires_explicit_submit
                && !matches!(
                    card.fields.as_slice(),
                    [smelt_plugin_api::model::acp_session::ElicitFieldView {
                        kind: ElicitFieldKindView::ExternalUrl(_),
                        ..
                    }]
                );
            let skip = Button::new("acp-elicit-skip")
                .quiet()
                .small()
                .label(t!("approval.skip"))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.dismiss_elicitation(cx);
                }));
            let mut inner = div()
                .flex()
                .flex_col()
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
                .border_color(ui_theme::hair())
                .bg(gpui::rgb(ui_theme::surface()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .w_full()
                        .gap_2()
                        .child(
                            div()
                                .type_style(TYPE.ui_medium)
                                .child(t!("elicit.title").into_owned()),
                        )
                        .child(div().flex_1())
                        .when(has_multi, |row| {
                            row.child(
                                div()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_full()
                                    .bg(ui_theme::overlay(0x18))
                                    .type_style(TYPE.text_xsmall)
                                    .text_color(muted)
                                    .child(t!("elicit.multi").into_owned()),
                            )
                        })
                        .child(skip),
                )
                .when(!card.message.trim().is_empty(), |card_el| {
                    card_el.child(
                        div()
                            .w_full()
                            .min_w_0()
                            .type_style(TYPE.ui)
                            .text_color(muted)
                            .child(card.message.clone()),
                    )
                });
            for (fix, field) in card.fields.iter().enumerate() {
                if let ElicitFieldKindView::ExternalUrl(url) = &field.kind {
                    let url = url.clone();
                    inner = inner.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .type_style(TYPE.text_xsmall)
                                    .text_color(muted)
                                    .child(field.title.clone()),
                            )
                            .child(
                                Button::new((
                                    gpui::ElementId::from("acp-elicit-url"),
                                    field.key.clone(),
                                ))
                                .primary()
                                .label(t!("elicit.open_continue"))
                                .on_click(cx.listener(
                                    move |this, _ev, _window, cx| {
                                        cx.open_url(&url);
                                        this.submit_elicitation(cx);
                                    },
                                )),
                            ),
                    );
                    continue;
                }
                if let ElicitFieldKindView::Text { secret } = &field.kind {
                    let input = self.elicitation_inputs.get(&fix).cloned();
                    inner = inner.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().type_style(TYPE.text_xsmall).text_color(muted).child(
                                if *secret {
                                    t!("elicit.secret", title = field.title).into_owned()
                                } else {
                                    field.title.clone()
                                },
                            ))
                            .children(input.map(|input| TextInput::new(&input).w_full())),
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
                let mut list = div()
                    .flex()
                    .flex_col()
                    .id((gpui::ElementId::from("acp-elicit-field"), field.key.clone()))
                    .w_full()
                    .min_w_0()
                    .flex_shrink_0()
                    .gap_2();
                for (oix, opt) in options.iter().enumerate() {
                    let selected = chosen.contains(&oix);
                    let id = gpui::SharedString::from(format!("acp-elicit-opt-{fix}-{oix}"));
                    let label = opt.display_label();
                    let pick = cx.listener(move |this, _: &(), _window, cx| {
                        this.pick_elicit_option(fix, oix, cx);
                    });
                    list = list.child(if is_multi {
                        elicit_choice_row(
                            Checkbox::new(id)
                                .checked(selected)
                                .label(label)
                                .debug_selector(move || format!("acp-elicit-opt-{fix}-{oix}"))
                                .on_change(move |_, window, cx| pick(&(), window, cx)),
                            selected,
                        )
                        .into_any_element()
                    } else {
                        elicit_choice_row(
                            Radio::new(id)
                                .checked(selected)
                                .label(label)
                                .debug_selector(move || format!("acp-elicit-opt-{fix}-{oix}"))
                                .on_select(move |window, cx| pick(&(), window, cx)),
                            selected,
                        )
                        .into_any_element()
                    });
                }
                if field.allow_custom_input {
                    let input = self.elicitation_inputs.get(&fix).cloned();
                    list = list.children(input.map(|input| {
                        div()
                            .flex()
                            .flex_col()
                            .w_full()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .type_style(TYPE.text_xsmall)
                                    .text_color(muted)
                                    .child(t!("elicit.none_above").into_owned()),
                            )
                            .child(TextInput::new(&input).w_full())
                    }));
                }
                inner = inner.child(
                    div()
                        .flex()
                        .flex_col()
                        .w_full()
                        .min_w_0()
                        .gap_1()
                        .when(card.fields.len() > 1, |d| {
                            d.child(div().type_style(TYPE.text_xsmall).text_color(muted).child(
                                if is_multi {
                                    t!("elicit.multi_title", title = field.title).into_owned()
                                } else {
                                    field.title.clone()
                                },
                            ))
                        })
                        .child(list),
                );
            }
            if show_footer {
                inner = inner.child(
                    div().flex().items_center().w_full().gap_2().child(
                        Button::new("acp-elicit-submit")
                            .debug_selector(|| "acp-elicit-submit".to_string())
                            .primary()
                            .disabled(!ready)
                            .label(t!("elicit.submit"))
                            .on_click(cx.listener(|this, _ev, _window, cx| {
                                if this.elicit_ready(cx) {
                                    this.submit_elicitation(cx);
                                }
                            })),
                    ),
                );
            }
            div()
                .flex()
                .flex_col()
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
