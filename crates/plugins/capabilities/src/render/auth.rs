//! 就地展开的登录面板：内置服务商的订阅 / API key 登录，和 MCP 服务器的 OAuth 登录共用。
//!
//! 面板画在被登录的那一行下面（模型页、MCP 页的列表里），所以不再写「登录 X」，也不另套
//! 一层卡片。登录成功面板就收起，那一行的状态自己会变；失败留着说原因。

use super::*;
use api::model::pi_auth::PiPromptKind;
use api::ui::{Clipboard, IconAction as _};
use smelt_plugin_api::ui::kit::TYPE;

pub(super) fn login_panel(
    entity: Entity<CapabilitiesPage>,
    view: &PiLoginView,
    cx: &App,
) -> impl IntoElement {
    let fg: Hsla = rgb(ui_theme::text()).into();
    let muted: Hsla = rgb(ui_theme::muted()).into();
    let failed = view.error.is_some();
    // 第一行：眼下在等什么（或为什么失败），右边取消 / 关闭。
    let headline = match &view.error {
        Some(error) => div()
            .type_style(TYPE.text_small)
            .text_color(rgb(ui_theme::bad()))
            .child(t!("auth.failed", error = error).into_owned()),
        // 授权链接 / 验证码是流程跑到一半才推过来的。在那之前先说等下要开浏览器，免得用户
        // 以为登录就是在这里填点什么。
        None if view.auth_url.is_none() && view.device_code.is_none() && view.prompt.is_none() => {
            div()
                .type_style(TYPE.text_small)
                .text_color(muted)
                .child(t!("auth.waiting").into_owned())
        }
        None => div(),
    };
    let cancel_entity = entity.clone();
    let mut panel = div().flex().flex_col().w_full().gap_3().py_1().child(
        div()
            .flex()
            .items_center()
            .justify_between()
            .w_full()
            .gap_3()
            .child(headline.flex_1().min_w_0())
            .child(
                Button::new("pi-login-cancel")
                    .quiet()
                    .small()
                    .label(if failed {
                        t!("common.close")
                    } else {
                        t!("common.cancel")
                    })
                    .on_click(move |_, _, cx| {
                        cancel_entity.update(cx, |page, cx| page.cancel_pi_login(cx));
                    }),
            ),
    );

    if let Some((user_code, verification_uri)) = &view.device_code {
        let uri = verification_uri.clone();
        let code = user_code.clone();
        panel = panel.child(
            div()
                .flex()
                .flex_col()
                .w_full()
                .gap_2()
                .child(
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(muted)
                        .child(t!("auth.device_code").into_owned()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .px_3()
                                .py_1()
                                .rounded_lg()
                                .bg(api::ui::ui_theme::overlay(0x18))
                                .text_lg()
                                .font_semibold()
                                .text_color(fg)
                                .child(user_code.clone()),
                        )
                        // 复制后换成对勾，看得出已经进了剪贴板，再去浏览器里贴。
                        .child(
                            Clipboard::new("pi-login-copy-code")
                                .value(code)
                                .icon_action(t!("common.copy")),
                        )
                        .child(
                            Button::new("pi-login-open-verification")
                                .small()
                                .label(t!("auth.open_verification"))
                                .on_click(move |_, _, cx| cx.open_url(&uri)),
                        ),
                )
                .child(
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(muted)
                        .child(verification_uri.clone()),
                ),
        );
    }

    if let Some(url) = &view.auth_url {
        let open_url = url.clone();
        let copy_url = url.clone();
        panel = panel.child(
            div()
                .flex()
                .flex_col()
                .w_full()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("pi-login-open-url")
                                .small()
                                .label(t!("auth.open_authorization"))
                                .on_click(move |_, _, cx| cx.open_url(&open_url)),
                        )
                        .child(
                            Clipboard::new("pi-login-copy-url")
                                .value(copy_url)
                                .icon_action(t!("auth.copy_link")),
                        ),
                )
                .children(view.instructions.clone().map(|instructions| {
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(muted)
                        .child(instructions)
                })),
        );
    }

    if let Some(prompt) = &view.prompt {
        panel = panel.child(
            div()
                .flex()
                .flex_col()
                .w_full()
                .gap_2()
                .child(
                    div()
                        .type_style(TYPE.text_xsmall_medium)
                        .text_color(fg)
                        .child(prompt.message.clone()),
                )
                .children(prompt.placeholder.clone().map(|placeholder| {
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(muted)
                        .child(t!("auth.example", placeholder = placeholder).into_owned())
                }))
                .child(match prompt.kind {
                    PiPromptKind::Select => div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_wrap()
                        .children(prompt.options.iter().map(|option| {
                            let choose_entity = entity.clone();
                            let option_id = option.id.clone();
                            Button::new(format!("pi-login-option-{}", option.id))
                                .small()
                                .label(option.label.clone())
                                .on_click(move |_, _, cx| {
                                    let option_id = option_id.clone();
                                    choose_entity.update(cx, |workspace, cx| {
                                        workspace.choose_pi_login_option(option_id, cx);
                                    });
                                })
                        }))
                        .into_any_element(),
                    _ => div()
                        .flex()
                        .items_center()
                        .w_full()
                        .gap_2()
                        .children(
                            view.active_input()
                                .map(|input| div().flex_1().min_w_0().child(TextInput::new(input))),
                        )
                        .child({
                            let submit_entity = entity.clone();
                            // 空着也可能是正确答案（Copilot 问企业域名，用
                            // github.com 就该留空）。按钮直说这一点，否则用户
                            // 会卡在「不知道该填什么」上。
                            let empty = view
                                .active_input()
                                .map(|input| input.read(cx).value().trim().is_empty())
                                .unwrap_or(true);
                            Button::new("pi-login-submit")
                                .small()
                                .label(if empty {
                                    t!("auth.submit_empty")
                                } else {
                                    t!("auth.submit")
                                })
                                .on_click(move |_, window, cx| {
                                    submit_entity.update(cx, |workspace, cx| {
                                        workspace.submit_pi_login_prompt(window, cx);
                                    });
                                })
                        })
                        .into_any_element(),
                }),
        );
    }

    // 只留最近几条：进度是给人看「还在动」的，不是日志。
    for message in view.messages.iter().rev().take(3).rev() {
        panel = panel.child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child(message.clone()),
        );
    }

    panel
}
