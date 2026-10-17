//! 设置页：Pi 内置 provider 的登录与注销。
//!
//! 凭据助手跑在运行时那台机器上（`pi.auth.*`，见 [`smelt_model::pi_auth_service`]）。
//! 本文件只做两件事：把登录事件搬进 `Workspace` 的状态，以及把用户在面板上的动作
//! 发回运行时。
//!
//! **为什么登录是一条常驻的事件流而不是一次异步调用。** OAuth 中途要把授权链接
//! 或 device code 推给用户看，用户粘回来的 code 又要送回流程，浏览器回调和手工
//! 粘贴还会互相抢跑。所以这里拿着登录 id 长轮询事件，边取边 fold 进视图状态。

use super::*;
use smelt_model::control_api::{
    PiAuthLoginAnswer, PiAuthLoginCancel, PiAuthLoginEvents, PiAuthLoginStart, PiAuthLogout,
    PiAuthModels, PiAuthProviders,
};
use smelt_model::pi_auth_service::{
    LoginAnswerParams, LoginEventsParams, LoginIdParams, LoginStartParams,
};
use smelt_model::pi_service::ProviderIdParams;
use smelt_runtime_client::protocol::control::EmptyParams;

/// 凭据助手首次使用可能要装运行时，给得宽一些。
const AUTH_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

impl Workspace {
    /// 打开凭据面板时问一次：有哪些 provider、各自登录了没有。
    ///
    /// 已经在问的时候不重复问：这会多起一个子进程，还会让面板在两次结果之间跳。
    pub fn refresh_pi_auth_providers(&mut self, cx: &mut Context<Self>) {
        if matches!(self.pi_auth_providers, Some(PiAuthProvidersState::Loading)) {
            return;
        }
        self.pi_auth_error = None;
        self.pi_auth_providers = Some(PiAuthProvidersState::Loading);
        cx.notify();
        self.run_pi_call::<PiAuthProviders>(
            EmptyParams::default(),
            AUTH_CALL_TIMEOUT,
            cx,
            |this, outcome, cx| {
                this.pi_auth_providers = Some(match outcome {
                    Ok(result) => PiAuthProvidersState::Ready(result.providers),
                    Err(error) => PiAuthProvidersState::Failed(error),
                });
                cx.notify();
            },
        );
    }

    /// 在「只看能订阅登录的」和「全部内置 provider」之间切换。
    pub fn toggle_pi_auth_show_all(&mut self, cx: &mut Context<Self>) {
        self.pi_auth_show_all = !self.pi_auth_show_all;
        cx.notify();
    }

    /// 开始一次登录。
    pub fn start_pi_login(
        &mut self,
        provider_id: String,
        provider_name: String,
        method: smelt_model::pi_auth::PiLoginMethod,
        ask_all: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::input::{InputEvent, InputState};

        self.cancel_pi_login(cx);
        self.pi_auth_error = None;
        // 输入框必须在这里建：提问从子进程的事件流里来，那里没有 `Window`。
        let input = cx.new(|cx| InputState::new(window, cx));
        let secret_input = cx.new(|cx| InputState::new(window, cx).masked(true));
        // 回车即提交：这是输入框的常识，缺了它用户会在「填完却没反应」上卡住。
        let input_subscriptions = std::rc::Rc::new(vec![
            cx.subscribe_in(
                &input,
                window,
                move |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::PressEnter { .. } => this.submit_pi_login_prompt(window, cx),
                        // 提交按钮的文案跟着「填没填」变，输入时要重画。
                        InputEvent::Change => cx.notify(),
                        _ => {}
                    }
                },
            ),
            cx.subscribe_in(
                &secret_input,
                window,
                move |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::PressEnter { .. } => this.submit_pi_login_prompt(window, cx),
                        // 提交按钮的文案跟着「填没填」变，输入时要重画。
                        InputEvent::Change => cx.notify(),
                        _ => {}
                    }
                },
            ),
        ]);
        cx.notify();
        let started_id = provider_id;
        cx.spawn(async move |this, cx| {
            let params = LoginStartParams {
                provider_id: started_id.clone(),
                method,
                ask_all,
            };
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    crate::runtime_call::call::<PiAuthLoginStart>(params, AUTH_CALL_TIMEOUT)
                        .map_err(crate::runtime_call::error_text)
                })
                .await;
            let login_id = match outcome {
                Ok(started) => started.login_id,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.pi_auth_error = Some(error);
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.pi_login = Some(PiLoginView {
                    provider_id: started_id.clone(),
                    provider_name,
                    login_id: login_id.clone(),
                    messages: Vec::new(),
                    auth_url: None,
                    instructions: None,
                    device_code: None,
                    prompt: None,
                    input,
                    secret_input,
                    input_subscriptions,
                    outcome: None,
                    models: None,
                });
                cx.notify();
            });
            // 一直取到登录结束。面板换了 provider、关了，或窗口没了，就不再取：运行时
            // 那边没人来取事件，过一会儿会自己杀掉子进程。
            let mut after = 0;
            loop {
                let params = LoginEventsParams {
                    login_id: login_id.clone(),
                    after,
                };
                let polled = cx
                    .background_executor()
                    .spawn(async move {
                        crate::runtime_call::call::<PiAuthLoginEvents>(
                            params,
                            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                        )
                        .map_err(crate::runtime_call::error_text)
                    })
                    .await;
                let batch = match polled {
                    Ok(batch) => batch,
                    Err(error) => smelt_model::pi_auth_service::LoginEventsResult {
                        events: vec![smelt_model::pi_auth::PiLoginEvent::Error { message: error }],
                        finished: true,
                    },
                };
                after += batch.events.len();
                let mut wanted = true;
                for event in batch.events {
                    wanted = matches!(
                        this.update(cx, |this, cx| this
                            .apply_pi_login_event(&login_id, event, cx)),
                        Ok(true)
                    );
                    if !wanted {
                        break;
                    }
                }
                if !wanted || batch.finished {
                    break;
                }
            }
        })
        .detach();
    }

    /// 把一个登录事件 fold 进面板状态。返回 false 表示这条流已经没人要了。
    fn apply_pi_login_event(
        &mut self,
        login_id: &str,
        event: smelt_model::pi_auth::PiLoginEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        use smelt_model::pi_auth::PiLoginEvent;

        let Some(view) = self.pi_login.as_mut() else {
            return false;
        };
        if view.login_id != login_id {
            return false;
        }
        match event {
            PiLoginEvent::AuthUrl { url, instructions } => {
                // 登录本来就是「去浏览器点同意」，这一步不该再要一次点击；
                // 按钮留着，是给自动打开失败或想换浏览器的人用的。
                cx.open_url(&url);
                view.auth_url = Some(url);
                view.instructions = instructions;
            }
            PiLoginEvent::DeviceCode {
                user_code,
                verification_uri,
                ..
            } => {
                cx.open_url(&verification_uri);
                view.device_code = Some((user_code, verification_uri));
            }
            PiLoginEvent::Info { message, links } => {
                view.messages.push(message);
                view.messages.extend(links.into_iter().map(|link| link.url));
            }
            PiLoginEvent::Progress { message } => view.messages.push(message),
            PiLoginEvent::Prompt {
                id,
                prompt_type,
                message,
                placeholder,
                options,
            } => {
                view.prompt = Some(PiLoginPrompt {
                    id,
                    kind: prompt_type,
                    message,
                    placeholder,
                    options,
                });
            }
            PiLoginEvent::PromptDone { id } => {
                if view.prompt.as_ref().is_some_and(|prompt| prompt.id == id) {
                    view.prompt = None;
                }
            }
            PiLoginEvent::Done => {
                view.prompt = None;
                view.auth_url = None;
                view.device_code = None;
                view.outcome = Some(Ok(()));
                let provider_id = view.provider_id.clone();
                self.reload_pi_model_settings(cx);
                self.refresh_pi_auth_providers(cx);
                self.load_pi_models_for(provider_id, cx);
            }
            // `parse_login_event` 把它挡在流外了，走不到这里。
            PiLoginEvent::Unknown => {}
            PiLoginEvent::Error { message } => {
                view.prompt = None;
                // 用户自己点的取消不是失败，会话已经被丢掉，走不到这里。
                view.outcome = Some(Err(message));
            }
            PiLoginEvent::Providers { .. } | PiLoginEvent::Models { .. } => {}
        }
        cx.notify();
        true
    }

    /// 问一遍某个 provider 现在能用哪些模型，好让「设为默认」有的选。
    ///
    /// 结果同时喂给登录面板和模型选择器：两处问的是同一件事，各自起一次子进程
    /// 只是让用户多等一遍。谁还开着就更新谁。
    fn load_pi_models_for(&mut self, provider_id: String, cx: &mut Context<Self>) {
        self.run_pi_call::<PiAuthModels>(
            ProviderIdParams {
                provider_id: provider_id.clone(),
            },
            AUTH_CALL_TIMEOUT,
            cx,
            move |this, outcome, cx| {
                let outcome = outcome.map(|result| result.models);
                if let Some(view) = this.pi_login.as_mut()
                    && view.provider_id == provider_id
                {
                    view.models = Some(outcome.clone());
                }
                if let Some(picker) = this.pi_auth_model_picker.as_mut()
                    && picker.provider_id == provider_id
                {
                    picker.models = Some(outcome);
                }
                cx.notify();
            },
        );
    }

    /// 为一个已配置的 provider 打开模型选择器。
    ///
    /// 「设为默认」不能只在登录成功那一瞬间可用：换模型、换回来、登录早就做完
    /// 了才想起要改，都是常事，否则用户只能回去手打模型 ID。
    pub fn open_pi_model_picker(
        &mut self,
        provider_id: String,
        provider_name: String,
        cx: &mut Context<Self>,
    ) {
        if self
            .pi_auth_model_picker
            .as_ref()
            .is_some_and(|picker| picker.provider_id == provider_id)
        {
            self.pi_auth_model_picker = None;
            cx.notify();
            return;
        }
        self.pi_auth_error = None;
        self.pi_auth_model_picker = Some(PiAuthModelPicker {
            provider_id: provider_id.clone(),
            provider_name,
            models: None,
        });
        cx.notify();
        self.load_pi_models_for(provider_id, cx);
    }

    pub fn close_pi_model_picker(&mut self, cx: &mut Context<Self>) {
        self.pi_auth_model_picker = None;
        cx.notify();
    }

    /// 回答当前提问（文本 / 粘贴的 code / 密钥）。
    pub fn submit_pi_login_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.pi_login.as_ref() else {
            return;
        };
        let Some(prompt) = view.prompt.as_ref() else {
            return;
        };
        let Some(input) = view.active_input().cloned() else {
            return;
        };
        let value = input.read(cx).value().trim().to_string();
        self.answer_pi_login(view.login_id.clone(), prompt.id.clone(), value, cx);
        // 输入框是整场登录共用的，答完就清空：下一个提问不该带着上一个的内容。
        input.update(cx, |state, cx| state.set_value("", window, cx));
        if let Some(view) = self.pi_login.as_mut() {
            view.prompt = None;
        }
        cx.notify();
    }

    /// 回答一个选项式提问。
    pub fn choose_pi_login_option(&mut self, option_id: String, cx: &mut Context<Self>) {
        let Some(view) = self.pi_login.as_ref() else {
            return;
        };
        let Some(prompt) = view.prompt.as_ref() else {
            return;
        };
        self.answer_pi_login(view.login_id.clone(), prompt.id.clone(), option_id, cx);
        if let Some(view) = self.pi_login.as_mut() {
            view.prompt = None;
        }
        cx.notify();
    }

    fn answer_pi_login(
        &mut self,
        login_id: String,
        prompt_id: String,
        value: String,
        cx: &mut Context<Self>,
    ) {
        self.run_pi_call::<PiAuthLoginAnswer>(
            LoginAnswerParams {
                login_id,
                prompt_id,
                value,
            },
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
            cx,
            |this, result, cx| {
                if let Err(error) = result {
                    this.pi_auth_error = Some(error);
                    cx.notify();
                }
            },
        );
    }

    /// 取消（或关掉）登录面板。运行时撤掉回调服务器、杀掉子进程。
    pub fn cancel_pi_login(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.pi_login.take() {
            self.run_pi_call::<PiAuthLoginCancel>(
                LoginIdParams {
                    login_id: view.login_id,
                },
                crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                cx,
                |_, _, _| {},
            );
        }
        cx.notify();
    }

    /// 把某个 provider 的某个模型设成 Pi 的默认模型。
    pub fn set_pi_default_model(
        &mut self,
        provider_id: String,
        model_id: String,
        cx: &mut Context<Self>,
    ) {
        let label = format!("{provider_id} / {model_id}");
        let params = smelt_model::pi_service::SaveDefaultModelParams {
            config: smelt_model::pi_model_settings::PiDefaultModelConfig {
                provider: provider_id,
                model: model_id,
                base_url: String::new(),
                thinking_level: String::new(),
            },
            api_key: None,
            reset_base_url: false,
        };
        self.run_pi_call::<smelt_model::control_api::PiModelsSaveDefault>(
            params,
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
            cx,
            move |this, result, cx| match result {
                Ok(config) => {
                    this.pi_auth_model_picker = None;
                    this.adopt_pi_config(config, true, cx);
                    crate::status_item::notify_success(format!("已设为默认模型：{label}"));
                }
                Err(error) => {
                    this.pi_auth_error = Some(error.clone());
                    crate::status_item::notify_error(&error);
                }
            },
        );
        cx.notify();
    }

    /// 注销一个 provider 的凭据。
    pub fn logout_pi_provider(&mut self, provider_id: String, cx: &mut Context<Self>) {
        self.pi_auth_error = None;
        cx.notify();
        self.run_pi_call::<PiAuthLogout>(
            ProviderIdParams {
                provider_id: provider_id.clone(),
            },
            AUTH_CALL_TIMEOUT,
            cx,
            move |this, outcome, cx| {
                if let Err(error) = outcome {
                    this.pi_auth_error = Some(error);
                } else {
                    this.reload_pi_model_settings(cx);
                    // 凭据没了，它报过的模型也就不能选了。
                    if this
                        .pi_auth_model_picker
                        .as_ref()
                        .is_some_and(|picker| picker.provider_id == provider_id)
                    {
                        this.pi_auth_model_picker = None;
                    }
                }
                this.refresh_pi_auth_providers(cx);
                cx.notify();
            },
        );
    }
}
