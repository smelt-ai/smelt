//! 设置页：Pi 内置 Provider 的凭据与登录。
//!
//! 和「自定义 Provider」那一节的分工：那边是填一个网关地址加一把 key，这边是
//! Pi 自己认识的 provider——它们的凭据要么来自订阅登录（授权链接 / device code），
//! 要么是一把存进 `auth.json` 的 key，两条路都由 Pi 的登录流程自己跑（见
//! [`api::model::pi_auth`]）。
//!
//! 列表本身（和自定义 Provider 合并、排序、折叠）在 `pi_model.rs`；这里只画内置
//! Provider 的一行，以及就地展开的登录面板和模型选择面板。

use super::*;
use api::model::pi_auth::{PiAuthProvider, PiAuthStatus, PiLoginMethod, PiPromptKind};
use api::ui::truncated_line::truncated_line;
use api::ui::{Clipboard, IconAction as _};
use smelt_plugin_api::ui::kit::TYPE;

/// 内置 Provider 的一行。按钮区只放当下最该点的那一个，其余动作收进「⋯」：
/// 原来一行最多五个按钮，扫一眼分不出哪个是正路。
pub(super) fn builtin_provider_row(
    entity: Entity<CapabilitiesPage>,
    provider: &PiAuthProvider,
    is_default: bool,
    login_busy: bool,
    palette: super::models::Palette,
) -> Div {
    let configured = provider.status.configured();
    let status_text = match provider.status {
        PiAuthStatus::Oauth => t!("auth.status.oauth"),
        PiAuthStatus::ApiKey => t!("auth.status.api_key"),
        PiAuthStatus::None => t!("auth.status.none"),
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
                .quiet()
                .small()
                .label(t!("auth.choose_model"))
                .tooltip(t!("auth.choose_model_tooltip"))
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
                .label(t!("auth.sign_in"))
                .tooltip(t!("auth.sign_in_tooltip"))
                .disabled(login_busy)
                .on_click(login(PiLoginMethod::OAuth, false)),
        )
    } else if provider.supports_api_key {
        Some(
            Button::new(format!("pi-auth-key-{}", provider.id))
                .small()
                .label(t!("auth.enter_api_key"))
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
        t!("auth.change_api_key")
    } else {
        t!("auth.use_api_key")
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
        let trigger = Button::new(format!("pi-auth-more-{}", provider.id))
            .quiet()
            .small()
            .icon(IconName::Ellipsis)
            .accessibility_label(t!("auth.more_actions"));
        MenuTrigger::dropdown(
            SharedString::from(format!("pi-auth-more-menu-{}", provider.id)),
            trigger,
            move |_, _| {
                let mut menu = Menu::new();
                if let Some(handler) = reauth.clone() {
                    menu = menu.item(
                        MenuItem::new(t!("auth.sign_in_again"))
                            .disabled(login_busy)
                            .on_click(move |event, window, cx| (*handler)(event, window, cx)),
                    );
                }
                if let Some(handler) = manual_oauth.clone() {
                    menu = menu.item(
                        MenuItem::new(t!("auth.sign_in_step_by_step"))
                            .disabled(login_busy)
                            .on_click(move |event, window, cx| (*handler)(event, window, cx)),
                    );
                }
                if let Some(handler) = api_key.clone() {
                    menu = menu.item(
                        MenuItem::new(api_key_label.clone())
                            .disabled(login_busy)
                            .on_click(move |event, window, cx| (*handler)(event, window, cx)),
                    );
                }
                if let Some(handler) = logout.clone() {
                    menu = menu.separator().item(
                        MenuItem::new(t!("auth.sign_out"))
                            .disabled(login_busy)
                            .on_click(move |event, window, cx| (*handler)(event, window, cx)),
                    );
                }
                menu
            },
        )
    });

    div()
        .flex()
        .items_center()
        .w_full()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .overflow_hidden()
                                .type_style(TYPE.ui)
                                .text_color(palette.fg)
                                .child(truncated_line(provider.name.clone())),
                        )
                        .children(
                            provider
                                .subscription
                                .then(|| Badge::new().child(t!("badge.subscription").into_owned())),
                        )
                        .children(is_default.then(|| {
                            Badge::new()
                                .accent()
                                .child(t!("badge.default").into_owned())
                        })),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .type_style(TYPE.text_xsmall)
                        .text_color(palette.muted)
                        .child(truncated_line(detail)),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .flex_none()
                .gap_1()
                .children(primary)
                .children(more),
        )
}

pub(super) fn login_panel(
    entity: Entity<CapabilitiesPage>,
    view: &PiLoginView,
    current_default: Option<&str>,
    cx: &App,
) -> impl IntoElement {
    let fg: Hsla = rgb(ui_theme::text()).into();
    let muted: Hsla = rgb(ui_theme::muted()).into();
    let border: Hsla = ui_theme::hair().into();
    let mut panel = div()
        .flex()
        .flex_col()
        .w_full()
        .gap_3()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(border)
        .child(
            div()
                .flex()
                .items_center()
                .w_full()
                .justify_between()
                .child(
                    div()
                        .type_style(TYPE.ui_medium)
                        .text_color(fg)
                        .child(t!("auth.login_title", name = view.title()).into_owned()),
                )
                .child({
                    let cancel_entity = entity.clone();
                    let finished = view.outcome.is_some();
                    Button::new("pi-login-cancel")
                        .quiet()
                        .small()
                        .label(if finished {
                            t!("common.close")
                        } else {
                            t!("common.cancel")
                        })
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
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child(t!("auth.waiting").into_owned()),
        );
    }

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

    match &view.outcome {
        Some(Err(error)) => {
            panel = panel.child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(rgb(ui_theme::bad()))
                    .child(t!("auth.failed", error = error).into_owned()),
            );
        }
        Some(Ok(())) => {
            panel = panel.child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(rgb(api::ui::ui_theme::ok()))
                    .child(SharedString::from(if view.mcp_server().is_some() {
                        t!("auth.done_mcp")
                    } else {
                        t!("auth.done")
                    })),
            );
            if let Some(provider_id) = view.provider_id() {
                panel = panel.child(model_choices(
                    entity,
                    provider_id,
                    view.models.as_ref(),
                    current_default,
                    fg,
                    muted,
                    border,
                ));
            }
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
    entity: Entity<CapabilitiesPage>,
    provider_id: &str,
    models: Option<&Result<Vec<api::model::provider_api::DiscoveredModel>, String>>,
    current_default: Option<&str>,
    fg: gpui::Hsla,
    muted: gpui::Hsla,
    border: gpui::Hsla,
) -> AnyElement {
    const VISIBLE_MODELS: usize = 12;

    let hint = |text: String| {
        div()
            .type_style(TYPE.text_xsmall)
            .text_color(muted)
            .child(text)
            .into_any_element()
    };
    let models = match models {
        None => return hint(t!("auth.models_loading").into_owned()),
        Some(Err(error)) => {
            return hint(t!("auth.models_failed", error = error).into_owned());
        }
        Some(Ok(models)) if models.is_empty() => {
            return hint(t!("auth.models_empty").into_owned());
        }
        Some(Ok(models)) => models,
    };

    let mut section = div().flex().flex_col().w_full().gap_2().child(
        div()
            .type_style(TYPE.text_xsmall_medium)
            .text_color(fg)
            .child(t!("auth.set_default_title").into_owned()),
    );
    for model in models.iter().take(VISIBLE_MODELS) {
        let is_current = current_default == Some(model.id.as_str());
        let set_entity = entity.clone();
        let provider = provider_id.to_string();
        let model_id = model.id.clone();
        section = section.child(
            div()
                .flex()
                .items_center()
                .w_full()
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
                        .overflow_hidden()
                        .type_style(TYPE.text_xsmall)
                        .text_color(fg)
                        .child(truncated_line(model.label().to_string())),
                )
                .child(if is_current {
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(muted)
                        .child(t!("auth.current_default").into_owned())
                        .into_any_element()
                } else {
                    Button::new(format!("pi-set-default-{provider_id}-{}", model.id))
                        .quiet()
                        .small()
                        .label(t!("auth.set_default"))
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
        section = section.child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child(t!("auth.more_models", count = models.len() - VISIBLE_MODELS).into_owned()),
        );
    }
    section.into_any_element()
}

/// 对着某个已配置 provider 打开的模型选择器。
pub(super) fn model_picker_panel(
    entity: Entity<CapabilitiesPage>,
    picker: &PiAuthModelPicker,
    current_default: Option<&str>,
    fg: gpui::Hsla,
    muted: gpui::Hsla,
    border: gpui::Hsla,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_3()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(border)
        .child(
            div()
                .flex()
                .items_center()
                .w_full()
                .justify_between()
                .child(
                    div()
                        .type_style(TYPE.ui_medium)
                        .text_color(fg)
                        .child(t!("auth.picker_title", name = picker.provider_name).into_owned()),
                )
                .child({
                    let close_entity = entity.clone();
                    Button::new("pi-model-picker-close")
                        .quiet()
                        .small()
                        .label(t!("common.close"))
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
