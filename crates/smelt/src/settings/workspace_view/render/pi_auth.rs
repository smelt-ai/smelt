//! 设置页：Pi 内置 Provider 的凭据与登录。
//!
//! 和「自定义 Provider」那一节的分工：那边是填一个网关地址加一把 key，这边是
//! Pi 自己认识的 provider——它们的凭据要么来自订阅登录（授权链接 / device code），
//! 要么是一把存进 `auth.json` 的 key，两条路都由 Pi 的登录流程自己跑（见
//! [`smelt_core::pi_auth`]）。
//!
//! 列表本身（和自定义 Provider 合并、排序、折叠）在 `pi_model.rs`；这里只画内置
//! Provider 的一行，以及就地展开的登录面板和模型选择面板。

use super::*;
use smelt_core::pi_auth::{PiAuthProvider, PiAuthStatus, PiLoginMethod, PiPromptKind};

/// 内置 Provider 的一行。按钮区只放当下最该点的那一个，其余动作收进「⋯」：
/// 原来一行最多五个按钮，扫一眼分不出哪个是正路。
pub(super) fn builtin_provider_row(
    entity: Entity<Workspace>,
    provider: &PiAuthProvider,
    is_default: bool,
    login_busy: bool,
    palette: super::pi_model::Palette,
) -> Div {
    let configured = provider.status.configured();
    let status_text = match provider.status {
        PiAuthStatus::Oauth => "已登录",
        PiAuthStatus::ApiKey => "API key",
        PiAuthStatus::None => "未配置",
    };
    // 凭据来源要说清楚：一个来自环境变量的 key 和一个存在 auth.json 里的 key，
    // 排查起来是两回事，而「注销」只动得了后者。
    let detail = match &provider.source {
        Some(source) if configured => format!("{} · {} · {}", provider.id, status_text, source),
        _ => format!("{} · {}", provider.id, status_text),
    };

    let login = |method: PiLoginMethod, manual: bool| {
        let entity = entity.clone();
        let id = provider.id.clone();
        let name = provider.name.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            let (id, name) = (id.clone(), name.clone());
            entity.update(cx, |workspace, cx| {
                workspace.start_pi_login(id, name, method, manual, window, cx);
            });
        }
    };

    let primary = if configured {
        let pick_entity = entity.clone();
        let id = provider.id.clone();
        let name = provider.name.clone();
        Some(
            Button::new(format!("pi-auth-models-{}", provider.id))
                .ghost()
                .small()
                .label("选模型")
                .tooltip("列出这个 Provider 现在能用的模型，挑一个设为默认")
                .on_click(move |_, _, cx| {
                    let (id, name) = (id.clone(), name.clone());
                    pick_entity.update(cx, |workspace, cx| {
                        workspace.open_pi_model_picker(id, name, cx);
                    });
                }),
        )
    } else if provider.supports_oauth {
        Some(
            Button::new(format!("pi-auth-oauth-{}", provider.id))
                .small()
                .label("登录")
                .tooltip("在浏览器里完成授权；只有一个正确答案的步骤会自动跳过")
                .disabled(login_busy)
                .on_click(login(PiLoginMethod::OAuth, false)),
        )
    } else if provider.supports_api_key {
        Some(
            Button::new(format!("pi-auth-key-{}", provider.id))
                .small()
                .label("填 API key")
                .disabled(login_busy)
                .on_click(login(PiLoginMethod::ApiKey, true)),
        )
    } else {
        None
    };

    // 「⋯」里的动作：主按钮已经占掉的那个不再重复。
    let reauth =
        (configured && provider.supports_oauth).then(|| login(PiLoginMethod::OAuth, false));
    // 自动跳过的步骤总有人要自己回答：GitHub 企业版实例、换一种登录方式。
    let manual_oauth = provider
        .supports_oauth
        .then(|| login(PiLoginMethod::OAuth, true));
    let api_key = (provider.supports_api_key && (configured || provider.supports_oauth))
        .then(|| login(PiLoginMethod::ApiKey, true));
    let api_key_label = if provider.status == PiAuthStatus::ApiKey {
        "更换 API key"
    } else {
        "改用 API key"
    };
    let logout = configured.then(|| {
        let entity = entity.clone();
        let id = provider.id.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            let id = id.clone();
            entity.update(cx, |workspace, cx| workspace.logout_pi_provider(id, cx));
        }
    });
    let has_more =
        reauth.is_some() || manual_oauth.is_some() || api_key.is_some() || logout.is_some();
    let more = has_more.then(|| {
        let reauth = reauth.map(std::rc::Rc::new);
        let manual_oauth = manual_oauth.map(std::rc::Rc::new);
        let api_key = api_key.map(std::rc::Rc::new);
        let logout = logout.map(std::rc::Rc::new);
        Button::new(format!("pi-auth-more-{}", provider.id))
            .ghost()
            .small()
            .icon(IconName::Ellipsis)
            .dropdown_menu(move |mut menu, _, _| {
                if let Some(handler) = reauth.clone() {
                    menu = menu.item(
                        PopupMenuItem::new("重新登录")
                            .disabled(login_busy)
                            .on_click(move |event, window, cx| (*handler)(event, window, cx)),
                    );
                }
                if let Some(handler) = manual_oauth.clone() {
                    menu = menu.item(
                        PopupMenuItem::new("逐项登录（企业版域名、登录方式等）")
                            .disabled(login_busy)
                            .on_click(move |event, window, cx| (*handler)(event, window, cx)),
                    );
                }
                if let Some(handler) = api_key.clone() {
                    menu = menu.item(
                        PopupMenuItem::new(api_key_label)
                            .disabled(login_busy)
                            .on_click(move |event, window, cx| (*handler)(event, window, cx)),
                    );
                }
                if let Some(handler) = logout.clone() {
                    menu = menu.separator().item(
                        PopupMenuItem::new("注销")
                            .disabled(login_busy)
                            .on_click(move |event, window, cx| (*handler)(event, window, cx)),
                    );
                }
                menu
            })
    });

    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            v_flex()
                .min_w_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .font_medium()
                                .text_color(palette.fg)
                                .child(provider.name.clone()),
                        )
                        .children(
                            provider
                                .subscription
                                .then(|| super::pi_model::pill("订阅", palette.muted)),
                        )
                        .children(
                            is_default.then(|| super::pi_model::pill("默认", palette.success)),
                        ),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(palette.muted)
                        .child(detail),
                ),
        )
        .child(
            h_flex()
                .flex_none()
                .gap_1()
                .items_center()
                .children(primary)
                .children(more),
        )
}

pub(super) fn login_panel(
    entity: Entity<Workspace>,
    view: &PiLoginView,
    current_default: Option<&str>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let fg = theme.foreground;
    let muted = theme.muted_foreground;
    let border = theme.border;
    let danger_fg = theme.danger_foreground;
    let mut panel = v_flex()
        .w_full()
        .gap_3()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(border)
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(fg)
                        .child(format!("登录 {}", view.provider_name)),
                )
                .child({
                    let cancel_entity = entity.clone();
                    let finished = view.outcome.is_some();
                    Button::new("pi-login-cancel")
                        .ghost()
                        .small()
                        .label(if finished { "关闭" } else { "取消" })
                        .on_click(move |_, _, cx| {
                            cancel_entity.update(cx, |workspace, cx| {
                                workspace.cancel_pi_login(cx);
                            });
                        })
                }),
        );

    // 授权链接/验证码是流程跑到一半才推过来的。在那之前先把「等下要开浏览器」
    // 说在前面，免得用户以为登录就是在这个框里填点什么。
    if view.outcome.is_none() && view.auth_url.is_none() && view.device_code.is_none() {
        panel = panel.child(
            div()
                .text_xs()
                .text_color(muted)
                .child("稍后会给出授权链接或验证码，用浏览器完成登录。"),
        );
    }

    if let Some((user_code, verification_uri)) = &view.device_code {
        let uri = verification_uri.clone();
        let code = user_code.clone();
        panel = panel.child(
            v_flex()
                .w_full()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("在浏览器里打开验证页，输入下面这串码："),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .px_3()
                                .py_1()
                                .rounded_lg()
                                .bg(crate::ui_theme::overlay(0x18))
                                .text_lg()
                                .font_semibold()
                                .text_color(fg)
                                .child(user_code.clone()),
                        )
                        .child(
                            Button::new("pi-login-copy-code")
                                .ghost()
                                .small()
                                .label("复制")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                                }),
                        )
                        .child(
                            Button::new("pi-login-open-verification")
                                .small()
                                .label("打开验证页")
                                .on_click(move |_, _, cx| cx.open_url(&uri)),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(verification_uri.clone()),
                ),
        );
    }

    if let Some(url) = &view.auth_url {
        let open_url = url.clone();
        let copy_url = url.clone();
        panel = panel.child(
            v_flex()
                .w_full()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("pi-login-open-url")
                                .small()
                                .label("在浏览器中打开授权页")
                                .on_click(move |_, _, cx| cx.open_url(&open_url)),
                        )
                        .child(
                            Button::new("pi-login-copy-url")
                                .ghost()
                                .small()
                                .label("复制链接")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        copy_url.clone(),
                                    ));
                                }),
                        ),
                )
                .children(
                    view.instructions
                        .clone()
                        .map(|instructions| div().text_xs().text_color(muted).child(instructions)),
                ),
        );
    }

    if let Some(prompt) = &view.prompt {
        panel = panel.child(
            v_flex()
                .w_full()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(fg)
                        .child(prompt.message.clone()),
                )
                .children(prompt.placeholder.clone().map(|placeholder| {
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("示例：{placeholder}"))
                }))
                .child(match prompt.kind {
                    PiPromptKind::Select => h_flex()
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
                    _ => h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .children(
                            view.active_input()
                                .map(|input| div().flex_1().min_w_0().child(Input::new(input))),
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
                                .label(if empty { "留空提交" } else { "提交" })
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
        panel = panel.child(div().text_xs().text_color(muted).child(message.clone()));
    }

    match &view.outcome {
        Some(Err(error)) => {
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(danger_fg)
                    .child(format!("登录失败：{error}")),
            );
        }
        Some(Ok(())) => {
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(rgb(crate::ui_theme::green()))
                    .child("登录成功，凭据已写入 ~/.pi/agent/auth.json。"),
            );
            panel = panel.child(model_choices(
                entity,
                &view.provider_id,
                view.models.as_ref(),
                current_default,
                fg,
                muted,
                border,
            ));
        }
        None => {}
    }

    panel
}

/// 从一个 provider 现在能用的模型里挑一个设成默认。
///
/// 模型列表来自 Pi 的 `getAvailable`，已按凭据过滤（Copilot 只给账号启用过的
/// 那些）。让用户回头去默认模型表单手打一个 ID，多半会打到一个这把凭据用不了的。
fn model_choices(
    entity: Entity<Workspace>,
    provider_id: &str,
    models: Option<&Result<Vec<smelt_core::provider_api::DiscoveredModel>, String>>,
    current_default: Option<&str>,
    fg: gpui::Hsla,
    muted: gpui::Hsla,
    border: gpui::Hsla,
) -> AnyElement {
    const VISIBLE_MODELS: usize = 12;

    let hint = |text: String| {
        div()
            .text_xs()
            .text_color(muted)
            .child(text)
            .into_any_element()
    };
    let models = match models {
        None => return hint("正在读取可用模型…".to_string()),
        Some(Err(error)) => {
            return hint(format!(
                "读不到可用模型（{error}），可在「默认模型配置」里手填。"
            ));
        }
        Some(Ok(models)) if models.is_empty() => {
            return hint(
                "这个 Provider 没有报告可用模型，可在「默认模型配置」里手填。".to_string(),
            );
        }
        Some(Ok(models)) => models,
    };

    let mut section = v_flex().w_full().gap_2().child(
        div()
            .text_xs()
            .font_semibold()
            .text_color(fg)
            .child("设为默认模型"),
    );
    for model in models.iter().take(VISIBLE_MODELS) {
        let is_current = current_default == Some(model.id.as_str());
        let set_entity = entity.clone();
        let provider = provider_id.to_string();
        let model_id = model.id.clone();
        section = section.child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap_3()
                .px_3()
                .py_1p5()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(fg)
                        .child(model.label().to_string()),
                )
                .child(if is_current {
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("当前默认")
                        .into_any_element()
                } else {
                    Button::new(format!("pi-set-default-{provider_id}-{}", model.id))
                        .ghost()
                        .small()
                        .label("设为默认")
                        .on_click(move |_, _window, cx| {
                            let (provider, model_id) = (provider.clone(), model_id.clone());
                            set_entity.update(cx, |workspace, cx| {
                                workspace.set_pi_default_model(provider, model_id, cx);
                            });
                        })
                        .into_any_element()
                }),
        );
    }
    if models.len() > VISIBLE_MODELS {
        section = section.child(div().text_xs().text_color(muted).child(format!(
            "另有 {} 个模型，可在「默认模型配置」里填写。",
            models.len() - VISIBLE_MODELS
        )));
    }
    section.into_any_element()
}

/// 对着某个已配置 provider 打开的模型选择器。
pub(super) fn model_picker_panel(
    entity: Entity<Workspace>,
    picker: &PiAuthModelPicker,
    current_default: Option<&str>,
    fg: gpui::Hsla,
    muted: gpui::Hsla,
    border: gpui::Hsla,
) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap_3()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(border)
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(fg)
                        .child(format!("{} 的可用模型", picker.provider_name)),
                )
                .child({
                    let close_entity = entity.clone();
                    Button::new("pi-model-picker-close")
                        .ghost()
                        .small()
                        .label("关闭")
                        .on_click(move |_, _, cx| {
                            close_entity.update(cx, |workspace, cx| {
                                workspace.close_pi_model_picker(cx);
                            });
                        })
                }),
        )
        .child(model_choices(
            entity,
            &picker.provider_id,
            picker.models.as_ref(),
            current_default,
            fg,
            muted,
            border,
        ))
}
