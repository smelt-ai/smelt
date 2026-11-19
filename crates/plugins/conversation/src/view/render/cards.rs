//! ACP 审批卡与提问卡。

use super::*;
use rust_i18n::t;
use smelt_plugin_api::ui::kit::TYPE;
use smelt_plugin_api::ui::{Checkbox, TextInput};

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

    /// 提问卡：题面在上、「跳过」在右上；选项一行一个，标题下面是说明；能自己写答案的题，
    /// 最后一行是输入框。单选题点一下就是答案；多选、几道题一起问时最后点「提交」。
    ///
    /// 卡片拿着焦点时 ↑↓ 挪高亮、回车或空格选、数字键选第几个、Esc 跳过（见
    /// [`ConversationView::move_elicit_cursor`]）。
    /// `keyboard`：焦点在卡片里，画出键盘高亮。
    pub(super) fn render_elicitation_card(
        &self,
        keyboard: bool,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let card = self.elicitation.as_ref()?;
        let text = gpui::rgb(ui_theme::text());
        let ready = self.elicit_ready(cx);
        let cursor_row = keyboard
            .then(|| elicit_rows(card).get(self.elicit_cursor).copied())
            .flatten();
        let only_choice = matches!(
            card.fields.as_slice(),
            [field] if matches!(field.kind, ElicitFieldKindView::Select(_))
        );
        // 单个单选题点了就是答案；别的（多选、几道题、填空）都要最后点「提交」。外链只有一个按钮。
        let explicit_submit = !only_choice
            && !matches!(
                card.fields.as_slice(),
                [smelt_plugin_api::model::acp_session::ElicitFieldView {
                    kind: ElicitFieldKindView::ExternalUrl(_),
                    ..
                }]
            );
        let custom_text = |field: usize| {
            self.elicitation_inputs
                .get(&field)
                .is_some_and(|input| !input.read(cx).value().trim().is_empty())
        };
        let skip = Button::new("acp-elicit-skip")
            .debug_selector(|| "acp-elicit-skip".to_string())
            .quiet()
            .small()
            .label(t!("approval.skip"))
            .on_click(cx.listener(|this, _ev, _window, cx| {
                this.dismiss_elicitation(cx);
            }));
        let heading = |content: String| {
            div()
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .type_style(TYPE.ui)
                .text_color(text)
                .child(content)
        };
        let mut skip = Some(skip);
        let mut inner = div()
            .flex()
            .flex_col()
            .id("acp-elicit-card")
            .debug_selector(|| "acp-elicit-card".to_string())
            .key_context(ELICIT_KEY_CONTEXT)
            .track_focus(&self.elicit_focus)
            .on_action(cx.listener(|this, _: &ElicitUp, window, cx| {
                this.move_elicit_cursor(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ElicitDown, window, cx| {
                this.move_elicit_cursor(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ElicitPick, window, cx| {
                this.activate_elicit_row(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ElicitSkip, _window, cx| {
                this.dismiss_elicitation(cx);
            }))
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                let modifiers = &ev.keystroke.modifiers;
                if modifiers.platform || modifiers.control || modifiers.alt {
                    return;
                }
                // 数字键只在卡片自己拿着焦点时接；焦点在输入框里时是在写答案。
                if !this.elicit_focus.is_focused(window) {
                    return;
                }
                if let Some(number) = ev
                    .keystroke
                    .key
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=9).contains(n))
                {
                    this.pick_elicit_number(number, window, cx);
                    cx.stop_propagation();
                }
            }))
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .max_w(ui_theme::conversation_max_width())
            .p_4()
            .gap_3()
            .rounded(ui_theme::card_radius())
            .border_1()
            .border_color(ui_theme::hair())
            .bg(gpui::rgb(ui_theme::surface()));
        if !card.message.trim().is_empty() {
            inner = inner.child(
                div()
                    .flex()
                    .items_start()
                    .w_full()
                    .gap_4()
                    .child(heading(card.message.clone()))
                    .children(skip.take()),
            );
        }
        for (fix, field) in card.fields.iter().enumerate() {
            // 几道题一起问时，各题的题面就是它的小标题；只有一道题时题面在卡片正文里。
            let title = (card.fields.len() > 1 || card.message.trim().is_empty())
                .then(|| field.title.trim().to_string())
                .filter(|title| !title.is_empty());
            let mut section = div().flex().flex_col().w_full().min_w_0().gap_2();
            if title.is_some() || skip.is_some() {
                section = section.child(
                    div()
                        .flex()
                        .items_start()
                        .w_full()
                        .gap_4()
                        .child(heading(match &field.kind {
                            ElicitFieldKindView::Text { secret: true } => {
                                t!("elicit.secret", title = field.title).into_owned()
                            }
                            _ => title.unwrap_or_default(),
                        }))
                        .children(skip.take()),
                );
            }
            match &field.kind {
                ElicitFieldKindView::ExternalUrl(url) => {
                    let url = url.clone();
                    section = section.child(
                        div().child(
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
                }
                ElicitFieldKindView::Text { .. } => {
                    section = section.children(
                        self.elicitation_inputs
                            .get(&fix)
                            .map(|input| TextInput::new(input).w_full()),
                    );
                }
                ElicitFieldKindView::Select(options)
                | ElicitFieldKindView::MultiSelect(options) => {
                    let multi = matches!(field.kind, ElicitFieldKindView::MultiSelect(_));
                    let chosen = card.chosen.get(&fix).cloned().unwrap_or_default();
                    // 单选题写了自己的答案，提交的就是写的字，选项不再显示成选中。
                    let typed = custom_text(fix);
                    let mut list = div()
                        .flex()
                        .flex_col()
                        .id((gpui::ElementId::from("acp-elicit-field"), field.key.clone()))
                        .w_full()
                        .min_w_0()
                        .gap_0p5();
                    for (oix, opt) in options.iter().enumerate() {
                        let highlighted = cursor_row
                            == Some(ElicitRow::Option {
                                field: fix,
                                option: oix,
                            });
                        let checked = chosen.contains(&oix) && (multi || !typed);
                        let label = opt.display_label();
                        let row = ListRow::new(gpui::SharedString::from(format!(
                            "acp-elicit-opt-{fix}-{oix}"
                        )))
                        .debug_selector(move || format!("acp-elicit-opt-{fix}-{oix}"))
                        .selected(highlighted)
                        .flex()
                        .items_center()
                        .w_full()
                        .min_w_0()
                        .min_h(px(36.))
                        .px_3()
                        .py_2()
                        .gap_3()
                        .when(multi, |row| {
                            row.child(
                                Checkbox::new(gpui::SharedString::from(format!(
                                    "acp-elicit-check-{fix}-{oix}"
                                )))
                                .checked(checked)
                                .accessibility_label(label.clone()),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .whitespace_normal()
                                .child(
                                    div()
                                        .type_style(if checked && !multi {
                                            TYPE.ui_medium
                                        } else {
                                            TYPE.ui
                                        })
                                        .child(label),
                                )
                                .children(opt.description.clone().map(|description| {
                                    div()
                                        .type_style(TYPE.text_small)
                                        .text_color(gpui::rgb(ui_theme::row_muted(highlighted)))
                                        .child(description)
                                })),
                        )
                        .when(checked && !multi, |row| {
                            row.child(
                                Icon::new(IconName::Check)
                                    .size(px(14.))
                                    .flex_shrink_0()
                                    .text_color(text),
                            )
                        })
                        .on_click(cx.listener(
                            move |this, _ev, window, cx| {
                                this.pick_elicit_option(fix, oix, window, cx);
                            },
                        ));
                        list = list.child(row);
                    }
                    if field.allow_custom_input
                        && let Some(input) = self.elicitation_inputs.get(&fix)
                    {
                        // 单个单选题没有底部的「提交」：写了答案就在这一行里交。
                        let inline_submit = only_choice && typed;
                        list = list.child(
                            div()
                                .flex()
                                .items_center()
                                .w_full()
                                .min_w_0()
                                .mt_1()
                                .gap_2()
                                .child(TextInput::new(input).w_full().flex_1())
                                .when(inline_submit, |row| {
                                    row.child(
                                        Button::new(("acp-elicit-custom-submit", fix))
                                            .debug_selector(|| {
                                                "acp-elicit-custom-submit".to_string()
                                            })
                                            .primary()
                                            .small()
                                            .icon(IconName::ArrowUp)
                                            .icon_action(t!("elicit.submit"))
                                            .on_click(cx.listener(|this, _ev, _window, cx| {
                                                if this.elicit_ready(cx) {
                                                    this.submit_elicitation(cx);
                                                }
                                            })),
                                    )
                                }),
                        );
                    }
                    section = section.child(list);
                }
            }
            inner = inner.child(section);
        }
        if explicit_submit {
            inner = inner.child(
                div().flex().justify_end().w_full().child(
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
        Some(
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
                .into_any_element(),
        )
    }
}
