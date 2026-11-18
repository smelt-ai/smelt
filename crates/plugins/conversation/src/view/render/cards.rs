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
    /// 权限请求长在输入框顶上，和输入框连成一体：一个琥珀色小圆点说「等你」，
    /// 要做的事，再是一列选项。选项是 agent 给的、长短不定，排成菜单行比并排按钮稳；
    /// 主选项默认选中，⌘⏎ 直接确认。
    pub(super) fn render_permission_tray(
        &self,
        animate_ambient: bool,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let pending = self.permissions.first()?;
        let muted: gpui::Hsla = gpui::rgb(ui_theme::muted()).into();
        let mono = smelt_plugin_api::model::font_config::font_family();
        let remaining = self.permissions.len();
        let title = match &pending.details {
            ApprovalDetailsView::Command { .. } => t!("approval.kind.command"),
            ApprovalDetailsView::FileChange { .. } => t!("approval.kind.file_change"),
            ApprovalDetailsView::Permissions { .. } => t!("approval.kind.permissions"),
            ApprovalDetailsView::Generic => t!("approval.title"),
        };
        let note = |text: String| {
            div()
                .w_full()
                .min_w_0()
                .whitespace_normal()
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child(text)
        };
        let details = match &pending.details {
            ApprovalDetailsView::Command {
                command,
                cwd,
                reason,
            } => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .px_2p5()
                        .py_1p5()
                        .rounded(px(6.))
                        .bg(ui_theme::overlay(0x0c))
                        .whitespace_normal()
                        .type_style(TYPE.text_xsmall)
                        .font_family(mono.clone())
                        .text_color(gpui::rgb(ui_theme::text()))
                        .child(command.clone()),
                )
                .children(reason.clone().map(note))
                .children(cwd.as_ref().map(|cwd| {
                    note(t!("approval.cwd", path = cwd).into_owned()).font_family(mono.clone())
                })),
            ApprovalDetailsView::FileChange { reason, grant_root } => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(note(
                    reason.clone().unwrap_or_else(|| pending.question.clone()),
                ))
                .children(grant_root.as_ref().map(|root| {
                    note(t!("approval.root", path = root).into_owned()).font_family(mono.clone())
                })),
            ApprovalDetailsView::Permissions { summary } => div().child(note(summary.clone())),
            ApprovalDetailsView::Generic => div().child(note(pending.question.clone())),
        };
        let header = div()
            .flex()
            .items_center()
            .w_full()
            .gap_2()
            .type_style(TYPE.text_small)
            .child(
                div()
                    .size(px(7.))
                    .mx(px(3.5))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(gpui::rgb(ui_theme::need())),
            )
            .child(
                div()
                    .type_style(TYPE.text_small_medium)
                    .text_color(gpui::rgb(ui_theme::text()))
                    .child(title.into_owned()),
            )
            .child(
                div()
                    .text_color(muted)
                    .child(t!("now.waiting").into_owned()),
            )
            .child(div().flex_1())
            .when(remaining > 1, |row| {
                row.child(
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(muted)
                        .child(t!("approval.remaining", count = remaining).into_owned()),
                )
            });
        let options = if self.permission_submitting.is_some() {
            div()
                .flex()
                .items_center()
                .h(px(30.))
                .px_2()
                .gap_2()
                .type_style(TYPE.text_small)
                .text_color(muted)
                .child(ambient_spinner(
                    "acp-permission-submit-spinner",
                    muted,
                    animate_ambient,
                ))
                .child(t!("status.working").into_owned())
        } else {
            let primary_ix = pending.options.iter().position(|option| {
                matches!(
                    option.kind,
                    PermissionOptionKindView::AllowOnce | PermissionOptionKindView::AllowAlways
                )
            });
            let mut options = div().flex().flex_col().w_full().gap_0p5();
            for (ix, option) in pending.options.iter().enumerate() {
                let primary = Some(ix) == primary_ix;
                let tool_call_id = pending.tool_call_id.clone();
                let option_id = option.option_id.clone();
                options = options.child(
                    ListRow::new((gpui::ElementId::from("acp-perm-option"), option_id.clone()))
                        .debug_selector(move || format!("ACP_PERMISSION_OPTION_{ix}"))
                        .flex()
                        .items_center()
                        .w_full()
                        .h(px(30.))
                        .px_2()
                        .gap_2p5()
                        .type_style(TYPE.text_small)
                        .selected(primary)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .child(truncated_line(option.name.clone())),
                        )
                        .when(primary, |row| {
                            row.child(
                                div()
                                    .flex_shrink_0()
                                    .type_style(TYPE.text_xsmall)
                                    .text_color(muted)
                                    .child("⌘⏎"),
                            )
                        })
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.pick_permission(&tool_call_id, &option_id, cx);
                        })),
                );
            }
            options
        };
        Some(
            div()
                .flex()
                .flex_col()
                .w_full()
                .min_w_0()
                .px_3p5()
                .pt_3()
                .pb_2p5()
                .gap_2()
                .border_b_1()
                .border_color(ui_theme::hair())
                .debug_selector(|| "ACP_PERMISSION_TRAY".into())
                .child(header)
                .child(
                    div()
                        .id("acp-permission-details")
                        .pl(px(18.))
                        .w_full()
                        .min_w_0()
                        .max_h(px(176.))
                        .overflow_y_scroll()
                        .child(details),
                )
                .child(div().pl(px(10.)).w_full().child(options))
                .into_any_element(),
        )
    }

    pub(super) fn render_elicitation_card(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let muted: gpui::Hsla = gpui::rgb(ui_theme::muted()).into();
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
        elicitation.map(|el| el.into_any_element())
    }
}
