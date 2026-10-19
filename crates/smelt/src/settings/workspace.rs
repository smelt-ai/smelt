//! `Workspace` 的设置页交互与渲染实现。
//!
//! 设置数据模型、远程生命周期和 Agent hooks 分别由父模块及其子模块提供；本文件
//! 只保留设置页对 `Workspace` 的状态操作，避免把数千行 UI 回调堆在模块入口。

use super::*;

impl Workspace {
    /// 一个自定义 provider 的凭据存在哪个引用名下。
    ///
    /// 名字由 id 派生而不是让人取：引用是"这把 key 存在哪"，不是配置项。让两个
    /// provider 填出同一个名字，第二次保存就会把第一个的 key 顶掉——凭据表按名字
    /// 索引，同名就是同一条。派生出来的 `<ID>_API_KEY` 天然跟着 id 唯一，回头看
    /// 凭据文件也一眼知道是谁的。
    ///
    /// 已经配好的那条沿用原名：它下面存着真的 key，而 Smelt 读不回明文，改名就等于
    /// 让用户重输一次。没有 key 的端点不给引用名——无鉴权的网关本来就不该在凭据表
    /// 里占个空位。
    pub(super) fn custom_provider_credential_ref(
        configured: &str,
        id: &str,
        has_key: bool,
    ) -> String {
        if !configured.is_empty() {
            return configured.to_string();
        }
        if !has_key {
            return String::new();
        }
        format!("{}_API_KEY", id.to_ascii_uppercase().replace('-', "_"))
    }

    /// 当前配置里的自定义 provider，读不到就当没有。
    fn native_dsh_custom_providers(&self) -> Vec<NativeDshCustomProvider> {
        self.dsh_native_model_settings
            .as_ref()
            .and_then(|settings| settings.as_ref().ok())
            .map(|settings| settings.custom_providers.clone())
            .unwrap_or_default()
    }

    pub(super) fn reload_native_dsh_model_settings(&mut self, cx: &mut Context<Self>) {
        self.refresh_dsh_config(cx);
    }

    /// 缓存里的 DSH 模型设置；还没从运行时取到时给出原因。
    fn native_dsh_model_settings_view(&self) -> Result<NativeDshModelSettings, String> {
        match &self.dsh_native_model_settings {
            Some(settings) => settings.clone(),
            None => Err(self.dsh_config_pending_text()),
        }
    }

    pub fn edit_native_dsh_model_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::InputState;

        self.dsh_model_editor_error = None;
        let settings = match self.native_dsh_model_settings_view() {
            Ok(settings) => settings,
            Err(error) => {
                self.dsh_model_editor_error = Some(error.clone());
                crate::status_item::notify_error(error);
                cx.notify();
                return;
            }
        };
        self.dsh_custom_provider_editor = None;
        if settings.default_model.provider != NATIVE_DSH_EDITABLE_PROVIDER {
            let error = format!(
                "当前默认 provider 为 {}；交互式配置暂仅支持 deepseek-official",
                settings.default_model.provider
            );
            self.dsh_model_editor_error = Some(error.clone());
            crate::status_item::notify_error(error);
            cx.notify();
            return;
        }
        let reasoning_effort = if settings.default_model.reasoning_effort.is_empty() {
            settings.deepseek.reasoning_effort.clone()
        } else {
            settings.default_model.reasoning_effort.clone()
        };
        let model = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("模型 ID，如 deepseek-v4-flash")
                .default_value(settings.default_model.model)
        });
        let base_url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("留空使用 https://api.deepseek.com")
                .default_value(settings.deepseek.base_url)
        });
        let configured_api_key_env = settings.deepseek.api_key_env.clone();
        let api_key = cx.new(|cx| {
            InputState::new(window, cx).masked(true).placeholder(
                if settings.credential_configured {
                    "已配置；输入新值以覆盖"
                } else {
                    "输入 DeepSeek API key"
                },
            )
        });
        self.dsh_model_editor = Some(DshModelEditor {
            model,
            base_url,
            api_key,
            reasoning_effort,
            configured_api_key_env,
            credential_configured: settings.credential_configured,
        });
        self.refresh_native_dsh_model_capabilities(cx);
        cx.notify();
    }

    /// 后台问一次每条路由支持哪些推理强度。
    ///
    /// 只在打开表单时问，不做定时刷新：答案只随 provider 配置变化，而配置只能
    /// 从这个表单改，保存后会重新打开。查询期间界面显示"读取中"而不是先摆一排
    /// 档位，因为摆出来的那一排正是我们无法保证正确的东西。
    /// 问端点它提供哪些模型，省得用户一个一个手打。
    pub fn discover_native_dsh_custom_models(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.dsh_custom_provider_editor.as_ref() else {
            return;
        };
        if matches!(editor.discovery, Some(ModelDiscoveryState::Loading)) {
            return;
        }
        let trimmed = |state: &Entity<gpui_component::input::InputState>| {
            let value = state.read(cx).value().trim().to_string();
            (!value.is_empty()).then_some(value)
        };
        let request = smelt_model::agent_kind::DshDiscoveryRequest {
            provider: trimmed(&editor.id),
            base_url: trimmed(&editor.base_url),
            api: Some(editor.api.clone()),
            api_key: trimmed(&editor.api_key),
        };
        if request.base_url.is_none() {
            if let Some(editor) = self.dsh_custom_provider_editor.as_mut() {
                editor.discovery = Some(ModelDiscoveryState::Failed(
                    "请先填写 API 地址，模型列表要从那个端点读取".to_string(),
                ));
            }
            cx.notify();
            return;
        }
        if let Some(editor) = self.dsh_custom_provider_editor.as_mut() {
            editor.discovery = Some(ModelDiscoveryState::Loading);
        }
        cx.notify();
        self.run_pi_call::<smelt_model::control_api::DshProvidersDiscoverModels>(
            request,
            super::dsh_config::DSH_CALL_TIMEOUT,
            cx,
            |this, outcome, cx| {
                if let Some(editor) = this.dsh_custom_provider_editor.as_mut() {
                    editor.discovery = Some(match outcome {
                        Ok(report) => ModelDiscoveryState::Ready(report.models),
                        Err(error) => ModelDiscoveryState::Failed(error),
                    });
                }
                cx.notify();
            },
        );
    }

    /// 把一条发现来的模型填进表单，优先复用空白行。
    pub fn adopt_native_dsh_discovered_model(
        &mut self,
        model: smelt_model::provider_api::DiscoveredModel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.dsh_custom_provider_editor.as_ref() else {
            return;
        };
        if editor
            .models
            .iter()
            .any(|row| row.id.read(cx).value().trim() == model.id)
        {
            return;
        }
        let blank = editor
            .models
            .iter()
            .position(|row| row.id.read(cx).value().trim().is_empty());
        let row = Self::dsh_custom_model_editor(
            Some(&NativeDshCustomModel {
                id: model.id.clone(),
                name: model.name.clone().unwrap_or_default(),
                context_window: model.context_window,
                max_tokens: model.max_tokens,
            }),
            window,
            cx,
        );
        if let Some(editor) = self.dsh_custom_provider_editor.as_mut() {
            match blank {
                Some(index) => editor.models[index] = row,
                None => editor.models.push(row),
            }
        }
        cx.notify();
    }

    fn refresh_native_dsh_model_capabilities(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.dsh_model_capabilities,
            Some(NativeDshCapabilityState::Loading)
        ) {
            return;
        }
        self.dsh_model_capabilities = Some(NativeDshCapabilityState::Loading);
        self.run_pi_call::<smelt_model::control_api::DshModelsCapabilities>(
            smelt_runtime_client::protocol::control::EmptyParams::default(),
            super::dsh_config::DSH_CALL_TIMEOUT,
            cx,
            |this, outcome, cx| {
                this.dsh_model_capabilities = Some(match outcome {
                    Ok(capabilities) => NativeDshCapabilityState::Ready(capabilities),
                    Err(error) => NativeDshCapabilityState::Failed(error),
                });
                cx.notify();
            },
        );
    }

    /// 选中一档推理强度。
    ///
    /// 只接受当前路由**广告过**的档位。以前这里写死了 off/low/high/max，于是任何
    /// 界面 bug 或过期状态都能把一个该路由根本不认的档位写进 settings.yaml，
    /// 而那种错要等到下一次真发请求才炸。
    pub fn set_native_dsh_reasoning_effort(&mut self, effort: String, cx: &mut Context<Self>) {
        let advertised = matches!(
            self.native_dsh_effort_view(cx),
            Some(DshEffortView::Choices { ref efforts, .. })
                if efforts.iter().any(|candidate| candidate.id == effort)
        );
        if !advertised {
            return;
        }
        if let Some(editor) = &mut self.dsh_model_editor {
            editor.reasoning_effort = effort;
            cx.notify();
        }
    }

    /// 当前表单选中的 provider/model 对应的推理强度栏该画什么。
    ///
    /// 外层 `None` 只表示"表单没打开"。表单打开时一定给出一个明确的状态，不把
    /// "在查"、"没有"、"查失败"混成同一个空值——那正是用户会误读成 bug 的地方。
    pub(super) fn native_dsh_effort_view(&self, cx: &App) -> Option<DshEffortView> {
        let editor = self.dsh_model_editor.as_ref()?;
        let model = editor.model.read(cx).value().trim().to_string();
        Some(match &self.dsh_model_capabilities {
            None | Some(NativeDshCapabilityState::Loading) => DshEffortView::Loading,
            Some(NativeDshCapabilityState::Failed(error)) => DshEffortView::Unknown(error.clone()),
            Some(NativeDshCapabilityState::Ready(capabilities)) => {
                match capabilities.efforts(NATIVE_DSH_EDITABLE_PROVIDER, &model) {
                    None => DshEffortView::Unavailable,
                    Some(efforts) => DshEffortView::Choices {
                        efforts: efforts.to_vec(),
                        default_effort: capabilities
                            .model(NATIVE_DSH_EDITABLE_PROVIDER, &model)
                            .and_then(|row| row.default_effort.clone()),
                    },
                }
            }
        })
    }

    pub(super) fn native_dsh_model_choices_view(&self, cx: &App) -> Option<DshModelChoicesView> {
        self.dsh_model_editor.as_ref()?;
        let _ = cx;
        Some(match &self.dsh_model_capabilities {
            None | Some(NativeDshCapabilityState::Loading) => DshModelChoicesView::Loading,
            Some(NativeDshCapabilityState::Failed(error)) => {
                DshModelChoicesView::Unknown(error.clone())
            }
            Some(NativeDshCapabilityState::Ready(capabilities)) => {
                let models = capabilities.models(NATIVE_DSH_EDITABLE_PROVIDER).to_vec();
                if models.is_empty() {
                    DshModelChoicesView::Unavailable
                } else {
                    DshModelChoicesView::Choices(models)
                }
            }
        })
    }

    pub fn cancel_native_dsh_model_edit(&mut self, cx: &mut Context<Self>) {
        self.dsh_model_editor = None;
        self.dsh_model_editor_error = None;
        cx.notify();
    }

    pub fn save_native_dsh_model_settings(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.dsh_model_editor.clone() else {
            return;
        };
        let model = editor.model.read(cx).value().trim().to_string();
        if model.is_empty() {
            crate::status_item::notify_error("模型 ID 不能为空");
            return;
        }
        let base_url = editor.base_url.read(cx).value().trim().to_string();
        if !base_url.is_empty() {
            match url::Url::parse(&base_url) {
                Ok(url) if matches!(url.scheme(), "http" | "https") => {}
                _ => {
                    crate::status_item::notify_error("API 地址必须是完整的 HTTP(S) URL");
                    return;
                }
            }
        }
        // 引用名跟着配置走，不给人改：官方 provider 只有一条，它的 key 该存在哪
        // 没有第二个答案（dsh 的默认就是 DEEPSEEK_API_KEY）。让人填只会填出第二个
        // 名字来，而凭据表按名字索引——改一次名，原来那把 key 就没人认领了。
        let api_key_env = editor.configured_api_key_env.clone();
        let api_key = editor.api_key.read(cx).value().to_string();
        if api_key.is_empty() && !editor.credential_configured {
            crate::status_item::notify_error("请输入 API key");
            return;
        }
        let payload = serde_json::json!({
            "model": model,
            "reasoningEffort": editor.reasoning_effort,
            "baseURL": base_url,
            "apiKeyEnv": api_key_env,
            "apiKey": (!api_key.is_empty()).then_some(api_key),
        });
        self.run_pi_call::<smelt_model::control_api::DshModelsSaveDefault>(
            smelt_model::dsh_service::SaveDefaultModelParams { payload },
            super::dsh_config::DSH_CALL_TIMEOUT,
            cx,
            |this, result, cx| {
                match result {
                    Ok(config) => {
                        this.dsh_model_editor = None;
                        this.dsh_model_editor_error = None;
                        this.adopt_dsh_config(config, cx);
                        crate::status_item::notify_success("DeepSeek 模型配置已保存");
                    }
                    Err(error) => {
                        this.dsh_model_editor_error = Some(error.clone());
                        crate::status_item::notify_error(error);
                    }
                }
                cx.notify();
            },
        );
    }

    /// 自定义模型的四个输入框。Pi 和 DSH 的字段、占位文案一样；两边的保存路径仍各自走。
    fn custom_model_inputs(
        id: String,
        name: String,
        context_window: String,
        max_tokens: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (
        Entity<gpui_component::input::InputState>,
        Entity<gpui_component::input::InputState>,
        Entity<gpui_component::input::InputState>,
        Entity<gpui_component::input::InputState>,
    ) {
        use gpui_component::input::InputState;

        let id = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("例如 gpt-5-mini")
                .default_value(id)
        });
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("显示名称（可选）")
                .default_value(name)
        });
        let context_window = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("上下文长度（可选）")
                .default_value(context_window)
        });
        let max_tokens = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("最大输出（可选）")
                .default_value(max_tokens)
        });
        (id, name, context_window, max_tokens)
    }

    fn dsh_custom_model_editor(
        model: Option<&NativeDshCustomModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> DshCustomModelEditor {
        let (id, name, context_window, max_tokens) = Self::custom_model_inputs(
            model.map(|model| model.id.clone()).unwrap_or_default(),
            model.map(|model| model.name.clone()).unwrap_or_default(),
            model
                .and_then(|model| model.context_window)
                .map(|value| value.to_string())
                .unwrap_or_default(),
            model
                .and_then(|model| model.max_tokens)
                .map(|value| value.to_string())
                .unwrap_or_default(),
            window,
            cx,
        );
        DshCustomModelEditor {
            id,
            name,
            context_window,
            max_tokens,
        }
    }

    pub fn edit_native_dsh_custom_provider(
        &mut self,
        provider_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::input::InputState;

        let settings = match self.native_dsh_model_settings_view() {
            Ok(settings) => settings,
            Err(error) => {
                self.dsh_model_editor_error = Some(error.clone());
                crate::status_item::notify_error(error);
                cx.notify();
                return;
            }
        };
        let existing = provider_id.as_ref().and_then(|provider_id| {
            settings
                .custom_providers
                .iter()
                .find(|provider| &provider.id == provider_id)
                .cloned()
        });
        let previous_id = existing.as_ref().map(|provider| provider.id.clone());
        let provider_value = existing
            .as_ref()
            .map(|provider| provider.id.clone())
            .unwrap_or_default();
        let display_name_value = existing
            .as_ref()
            .map(|provider| provider.display_name.clone())
            .unwrap_or_default();
        let api_key_env_value = existing
            .as_ref()
            .map(|provider| provider.api_key_env.clone())
            .unwrap_or_default();
        let base_url_value = existing
            .as_ref()
            .map(|provider| provider.base_url.clone())
            .unwrap_or_default();
        let id = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("例如 acme-gateway")
                .default_value(provider_value)
        });
        let display_name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("例如 Acme Gateway")
                .default_value(display_name_value)
        });
        let api_key = cx.new(|cx| {
            InputState::new(window, cx).masked(true).placeholder(
                if existing
                    .as_ref()
                    .is_some_and(|provider| provider.credential_configured)
                {
                    "已配置；输入新值以覆盖"
                } else {
                    "API key（可留空）"
                },
            )
        });
        let base_url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://gateway.example/v1")
                .default_value(base_url_value)
        });
        let auto = previous_id.as_deref().is_some_and(|id| {
            self.dsh_config
                .as_ref()
                .is_some_and(|config| config.auto_provider_ids.iter().any(|auto| auto == id))
        });
        let mut models = if auto {
            Vec::new()
        } else {
            existing
                .as_ref()
                .map(|provider| {
                    provider
                        .models
                        .iter()
                        .map(|model| Self::dsh_custom_model_editor(Some(model), window, cx))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        if models.is_empty() && !auto {
            models.push(Self::dsh_custom_model_editor(None, window, cx));
        }
        self.dsh_model_editor = None;
        self.dsh_model_editor_error = None;
        self.dsh_custom_provider_editor = Some(DshCustomProviderEditor {
            previous_id,
            id,
            display_name,
            api_key,
            base_url,
            api: existing
                .as_ref()
                .map(|provider| provider.api.clone())
                .filter(|api| !api.is_empty())
                .unwrap_or_else(|| "openai-completions".to_string()),
            models,
            discovery: None,
            configured_api_key_env: api_key_env_value,
            credential_configured: existing
                .as_ref()
                .is_some_and(|provider| provider.credential_configured),
        });
        cx.notify();
    }

    pub fn set_native_dsh_custom_api(&mut self, api: &str, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.dsh_custom_provider_editor else {
            return;
        };
        editor.api = smelt_model::provider_api::normalize_provider_api(api).to_string();
        cx.notify();
    }

    pub fn add_native_dsh_custom_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let row = Self::dsh_custom_model_editor(None, window, cx);
        if let Some(editor) = &mut self.dsh_custom_provider_editor {
            editor.models.push(row);
        }
        cx.notify();
    }

    pub fn remove_native_dsh_custom_model(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.dsh_custom_provider_editor else {
            return;
        };
        if index < editor.models.len() {
            editor.models.remove(index);
            cx.notify();
        }
    }

    pub fn cancel_native_dsh_custom_provider(&mut self, cx: &mut Context<Self>) {
        self.dsh_custom_provider_editor = None;
        self.dsh_model_editor_error = None;
        cx.notify();
    }

    pub fn save_native_dsh_custom_provider(
        &mut self,
        set_default: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.dsh_custom_provider_editor.clone() else {
            return;
        };
        let id = editor.id.read(cx).value().trim().to_string();
        if !id.chars().next().is_some_and(|ch| ch.is_ascii_lowercase())
            || !id.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .chars()
                        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
            })
        {
            crate::status_item::notify_error(
                "Provider ID 需以小写字母开头，只能包含小写字母、数字和连字符",
            );
            return;
        }
        // 新建时撞上已有的 id，或改名撞上别人的 id，都会让 dsh 拿新配置整条盖掉那个
        // provider——它的地址、模型、凭据引用一起没了，而 `save-custom` 照样回 ok。
        // `previous_id` 把"就是在编辑它自己"排除在外。
        if self
            .native_dsh_custom_providers()
            .into_iter()
            .any(|provider| provider.id == id && Some(&provider.id) != editor.previous_id.as_ref())
        {
            crate::status_item::notify_error(format!("Provider ID「{id}」已被占用，请换一个"));
            return;
        }
        let base_url = editor.base_url.read(cx).value().trim().to_string();
        match url::Url::parse(&base_url) {
            Ok(url) if matches!(url.scheme(), "http" | "https") => {}
            _ => {
                crate::status_item::notify_error("API 地址必须是完整的 HTTP(S) URL");
                return;
            }
        }
        let api_key = editor.api_key.read(cx).value().to_string();
        let api_key_env = Self::custom_provider_credential_ref(
            &editor.configured_api_key_env,
            &id,
            !api_key.is_empty(),
        );
        let parse_optional = |value: String, label: &str| -> Result<Option<u64>, String> {
            let value = value.trim();
            if value.is_empty() {
                return Ok(None);
            }
            value
                .parse::<u64>()
                .ok()
                .filter(|value| *value > 0)
                .map(Some)
                .ok_or_else(|| format!("{label}必须是正整数"))
        };
        let mut models = Vec::with_capacity(editor.models.len());
        for (index, model) in editor.models.iter().enumerate() {
            let model_id = model.id.read(cx).value().trim().to_string();
            let model_name = model.name.read(cx).value().trim().to_string();
            let context_window_raw = model.context_window.read(cx).value().trim().to_string();
            let max_tokens_raw = model.max_tokens.read(cx).value().trim().to_string();
            if model_id.is_empty()
                && model_name.is_empty()
                && context_window_raw.is_empty()
                && max_tokens_raw.is_empty()
            {
                continue;
            }
            if model_id.is_empty() {
                crate::status_item::notify_error(format!("请填写模型 {} 的模型 ID", index + 1));
                return;
            }
            let context_window = match parse_optional(
                context_window_raw,
                &format!("模型 {} 的上下文长度", index + 1),
            ) {
                Ok(value) => value,
                Err(error) => {
                    crate::status_item::notify_error(error);
                    return;
                }
            };
            let max_tokens =
                match parse_optional(max_tokens_raw, &format!("模型 {} 的最大输出", index + 1))
                {
                    Ok(value) => value,
                    Err(error) => {
                        crate::status_item::notify_error(error);
                        return;
                    }
                };
            models.push(serde_json::json!({
                "id": model_id,
                "name": model_name,
                "contextWindow": context_window,
                "maxTokens": max_tokens,
            }));
        }
        let from_endpoint = models.is_empty();
        if from_endpoint {
            if let Some(editor) = self.dsh_custom_provider_editor.as_mut() {
                editor.discovery = Some(ModelDiscoveryState::Loading);
            }
            cx.notify();
        }
        let set_default_flag = set_default;
        let params = smelt_model::dsh_service::SaveCustomProviderParams {
            previous_id: editor.previous_id.clone(),
            id,
            display_name: editor.display_name.read(cx).value().trim().to_string(),
            api: editor.api.clone(),
            base_url,
            api_key_env,
            api_key,
            models,
            set_default,
        };
        self.run_pi_call::<smelt_model::control_api::DshProvidersSaveCustom>(
            params,
            super::dsh_config::DSH_CALL_TIMEOUT,
            cx,
            move |this, result, cx| {
                match result {
                    Ok(saved) => {
                        this.dsh_custom_provider_editor = None;
                        this.dsh_model_editor_error = None;
                        this.adopt_dsh_config(saved.config, cx);
                        crate::status_item::notify_success(match (saved.auto, set_default_flag) {
                            (true, true) => {
                                "自定义 Provider 已保存并设为默认；模型目录将在每次启动时刷新"
                            }
                            (true, false) => "自定义 Provider 已保存；模型目录将在每次启动时刷新",
                            (false, true) => "自定义 Provider 已保存并设为默认",
                            (false, false) => "自定义 Provider 已保存",
                        });
                    }
                    Err(error) => {
                        if !from_endpoint {
                            this.dsh_model_editor_error = Some(error.clone());
                            crate::status_item::notify_error(error.clone());
                        }
                        this.fail_native_dsh_custom_provider_save(error, cx);
                    }
                }
                cx.notify();
            },
        );
    }

    fn fail_native_dsh_custom_provider_save(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(editor) = self.dsh_custom_provider_editor.as_mut() {
            editor.discovery = Some(ModelDiscoveryState::Failed(error));
        }
        cx.notify();
    }

    /// 让运行时确保配置文件存在，再用系统文本编辑器打开。只有本机运行时打得开。
    pub(super) fn open_native_dsh_config(
        &mut self,
        credentials: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_pi_call::<smelt_model::control_api::DshConfigEnsureFile>(
            smelt_model::dsh_service::EnsureConfigFileParams { credentials },
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
            cx,
            |_, result, _| {
                let path = match result {
                    Ok(result) => std::path::PathBuf::from(result.path),
                    Err(error) => {
                        crate::status_item::notify_error(error);
                        return;
                    }
                };
                match crate::ide::open_in_system_text_editor(&path) {
                    Ok(()) => crate::status_item::notify_success(format!(
                        "已在系统文本编辑器打开 {}",
                        path.display()
                    )),
                    Err(error) => crate::status_item::notify_error(error),
                }
            },
        );
    }

    pub(super) fn refresh_native_dsh_settings(&mut self, cx: &mut Context<Self>) {
        self.refresh_dsh_config(cx);
        cx.notify();
    }

    /// 展开/收起某个 profile 的插件管理面板。展开时请运行时读一次清单。
    pub fn toggle_native_dsh_plugin_manager(
        &mut self,
        profile: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .dsh_plugin_manager
            .as_ref()
            .is_some_and(|manager| manager.profile == profile)
        {
            self.dsh_plugin_manager = None;
            cx.notify();
            return;
        }
        self.ensure_native_dsh_plugin_manager(profile, window, cx);
        cx.notify();
    }

    /// 保证面板正指向 `profile`，必要时新建。
    fn ensure_native_dsh_plugin_manager(
        &mut self,
        profile: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .dsh_plugin_manager
            .as_ref()
            .is_some_and(|manager| manager.profile == profile)
        {
            return;
        }
        use gpui_component::input::InputState;
        let spec = cx.new(|cx| {
            InputState::new(window, cx).placeholder("插件包名，如 @smelt-ai/dsh-acp-rich@0.1.3")
        });
        self.dsh_plugin_manager = Some(DshPluginManager {
            plugins: None,
            profile,
            spec,
            busy: None,
            message: None,
        });
        self.reload_native_dsh_plugins(cx);
    }

    /// 为还没装 Smelt Host 的 profile 安装它。
    ///
    /// 走和普通插件完全一样的通道：打开面板、后台执行、就地显示进度与结果。以前
    /// 这里会另开一个终端，用户既看不到进度也不知道装完没有。
    pub fn install_native_dsh_smelt_host(
        &mut self,
        profile: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_native_dsh_plugin_manager(profile, window, cx);
        let spec = format!(
            "{}@{}",
            smelt_model::agent_kind::SMELT_HOST_PACKAGE,
            smelt_model::agent_kind::SMELT_HOST_VERSION
        );
        self.run_native_dsh_plugin_action(DshPluginAction::Install, spec, window, cx);
    }

    /// 请运行时重新读取当前面板的插件清单。回来时面板已切走就丢掉。
    pub fn reload_native_dsh_plugins(&mut self, cx: &mut Context<Self>) {
        let Some(profile) = self.dsh_plugin_manager.as_ref().map(|m| m.profile.clone()) else {
            return;
        };
        self.run_pi_call::<smelt_model::control_api::DshPluginsList>(
            smelt_model::dsh_service::ProfileParams {
                profile: profile.clone(),
            },
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
            cx,
            move |this, result, cx| {
                if let Some(manager) = this
                    .dsh_plugin_manager
                    .as_mut()
                    .filter(|manager| manager.profile == profile)
                {
                    manager.plugins = Some(result.and_then(|listed| listed.plugins));
                    cx.notify();
                }
            },
        );
    }

    /// 在面板里执行安装、更新或卸载。
    ///
    /// 三条都会拉起 pnpm 走网络，几秒到几十秒不等，必须放后台，否则整个窗口卡死
    /// ——那正是用户最初抱怨的「点了没反应」的另一种形态。执行期间面板显示忙碌态
    /// 并禁用按钮，结束后无论成败都重读清单：失败也可能已经改了一半依赖树，界面
    /// 必须显示真实现状而不是我们以为的现状。
    fn run_native_dsh_plugin_action(
        &mut self,
        action: DshPluginAction,
        target: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(manager) = self.dsh_plugin_manager.as_mut() else {
            return;
        };
        if manager.busy.is_some() {
            return;
        }
        let target = target.trim().to_string();
        if target.is_empty() {
            manager.message = Some((
                match action {
                    DshPluginAction::Install => "请先填写要安装的插件包名".to_string(),
                    DshPluginAction::Update => "请指定要更新的插件".to_string(),
                    DshPluginAction::Remove => "请指定要卸载的插件".to_string(),
                },
                false,
            ));
            cx.notify();
            return;
        }
        let profile = manager.profile.clone();
        manager.busy = Some(match action {
            // 明说要等：安装要下载依赖，慢网络下几分钟很正常。不写这句，用户会
            // 在第一分钟就认定"卡死了"——这正是这次要消除的体验。
            DshPluginAction::Install => {
                format!("正在安装 {target}…（需下载依赖，可能要几分钟）")
            }
            DshPluginAction::Update => format!("正在更新 {target}…（要向 registry 查最新版）"),
            DshPluginAction::Remove => format!("正在卸载 {target}…"),
        });
        manager.message = None;
        cx.notify();

        let params = smelt_model::dsh_service::PluginActionParams {
            profile: profile.clone(),
            action: match action {
                DshPluginAction::Install => smelt_model::dsh_service::PluginAction::Install,
                DshPluginAction::Update => smelt_model::dsh_service::PluginAction::Update,
                DshPluginAction::Remove => smelt_model::dsh_service::PluginAction::Remove,
            },
            target: target.clone(),
        };
        let window_handle = window.window_handle();
        self.run_pi_call::<smelt_model::control_api::DshPluginsRun>(
            params,
            super::dsh_config::DSH_PLUGIN_TIMEOUT,
            cx,
            move |this, result, cx| {
                // 面板可能在等待期间被收起或切到别的 profile；那时这次结果已经不属于
                // 当前界面，直接丢弃，不要把它写到别人的面板上。
                let Some(manager) = this
                    .dsh_plugin_manager
                    .as_mut()
                    .filter(|manager| manager.profile == profile)
                else {
                    return;
                };
                manager.busy = None;
                let (outcome, plugins) = match result {
                    Ok(done) => (done.outcome, Some(done.plugins)),
                    Err(error) => (Err(error), None),
                };
                // 运行时已经重读了清单：成功文案要报**真正装上的版本**。pnpm 完全
                // 可能什么都没做就退 0（范围已满足、或新版被供应链门槛挡在
                // `latest` 之外），只说一句"已安装"会把这种空转说成成功。
                if let Some(plugins) = plugins {
                    manager.plugins = Some(plugins);
                }
                manager.message = Some(match outcome {
                    Err(error) => (Self::condense_plugin_error(&error), false),
                    Ok(()) if action == DshPluginAction::Remove => {
                        (format!("已卸载 {target}，新建会话后生效"), true)
                    }
                    Ok(()) => {
                        let name = Self::plugin_name_of(&target);
                        let installed = manager
                            .plugins
                            .as_ref()
                            .and_then(|plugins| plugins.as_ref().ok())
                            .and_then(|plugins| plugins.iter().find(|plugin| plugin.name == name))
                            .and_then(|plugin| plugin.installed.clone());
                        (
                            Self::plugin_install_message(name, installed.as_deref()),
                            true,
                        )
                    }
                });
                let spec = (action != DshPluginAction::Remove).then(|| manager.spec.clone());
                // 桥里的助手每次调用都是新起的进程，换掉的那一版**立刻**就在答话了；
                // 模型设置跟着重读，免得同一页上半截说凭据缺失、下半截说刚装好新版。
                this.refresh_dsh_config(cx);
                if let Some(spec) = spec {
                    let _ = window_handle.update(cx, |_, window, cx| {
                        spec.update(cx, |state, cx| state.set_value("", window, cx));
                    });
                }
                cx.notify();
            },
        );
    }

    /// 安装/更新成功后的那句话。
    ///
    /// 报实际版本，因为"点了没反应"的真正形态是**执行成功但版本没变**。桥还多一
    /// 层：更新拉的是 registry 的 latest，可能超出本版 Smelt 验证过的范围，那时得
    /// 说出来——但只是提醒，不是拦截，装都装上了。
    fn plugin_install_message(name: &str, installed: Option<&str>) -> String {
        let Some(version) = installed else {
            return format!("已安装 {name}，新建会话后生效");
        };
        if name == smelt_model::agent_kind::SMELT_HOST_PACKAGE
            && !smelt_model::agent_kind::smelt_host_version_is_supported(version)
        {
            return format!(
                "已安装 {name} v{version}，但它超出本版 Smelt 验证过的范围（{}）；如遇异常请更新 Smelt",
                smelt_model::agent_kind::SMELT_HOST_VERSION
            );
        }
        format!("已安装 {name} v{version}，新建会话后生效")
    }

    /// 从 `@scope/name@range` 里取回包名。
    ///
    /// scope 包的首字符也是 `@`，所以分隔版本的那个 `@` 要从第二个字符起找。
    pub(super) fn plugin_name_of(spec: &str) -> &str {
        match spec
            .char_indices()
            .skip(1)
            .find(|(_, ch)| *ch == '@')
            .map(|(index, _)| index)
        {
            Some(index) => &spec[..index],
            None => spec,
        }
    }

    /// pnpm 的失败输出常常几十行，其中大部分是进度与堆栈。设置页放不下也不该放，
    /// 取最后几行有内容的即可——真正的原因几乎总在末尾。
    fn condense_plugin_error(error: &str) -> String {
        let tail = error
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        if tail.is_empty() {
            "执行失败，原因未知".to_string()
        } else {
            tail
        }
    }

    /// 安装输入框里的包名。
    pub fn install_native_dsh_plugin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(manager) = self.dsh_plugin_manager.as_ref() else {
            return;
        };
        let spec = manager.spec.read(cx).value().trim().to_string();
        self.run_native_dsh_plugin_action(DshPluginAction::Install, spec, window, cx);
    }

    /// 更新一个已列出的插件到最新版。
    ///
    /// 只传包名，版本由 [`dsh_plugin_update`] 在运行时向 registry 问——不能拿
    /// `SMELT_HOST_VERSION` 当目标：那是编译进本版 Smelt 的常量，用它就等于要求
    /// 用户为了升桥先升 Smelt。桥的补丁版发得比 Smelt 勤，这个顺序是反的。
    ///
    /// [`dsh_plugin_update`]: smelt_model::agent_kind::dsh_plugin_update
    pub fn reinstall_native_dsh_plugin(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_native_dsh_plugin_action(DshPluginAction::Update, name, window, cx);
    }

    /// 卸载一个插件。
    pub fn remove_native_dsh_plugin(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_native_dsh_plugin_action(DshPluginAction::Remove, name, window, cx);
    }

    /// 懒创建外观设置的有状态控件（需要 window）。
    pub fn ensure_appearance_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.opacity_slider.is_some()
            && self.ui_font_size_slider.is_some()
            && self.font_size_slider.is_some()
            && self.bg_image_opacity_slider.is_some()
            && self.bg_color_picker.is_some()
        {
            return;
        }

        // 不透明度滑块 + 界面字号滑块 + 终端字号滑块 + 背景图透明度滑块 + 背景色取色器。
        let ap = cx.global::<Appearance>().clone();
        let opacity_slider = cx.new(|_| {
            SliderState::new()
                .min(60.0)
                .max(100.0)
                .step(5.0)
                .default_value(ap.window_opacity() * 100.0)
        });
        let ui_font_size_slider = cx.new(|_| {
            SliderState::new()
                .min(MIN_UI_FONT_PX as f32)
                .max(MAX_UI_FONT_PX as f32)
                .step(1.0)
                .default_value(ap.ui_font_px as f32)
        });
        let font_size_slider = cx.new(|_| {
            SliderState::new()
                .min(terminal_view::MIN_FONT_PX as f32)
                .max(terminal_view::MAX_FONT_PX as f32)
                .step(1.0)
                .default_value(ap.font_px as f32)
        });
        let bg_image_opacity_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(100.0)
                .step(5.0)
                .default_value(ap.bg_image_opacity.clamp(0.0, 1.0) * 100.0)
        });
        let bg_color_picker =
            cx.new(|cx| ColorPickerState::new(window, cx).default_value(rgb(ap.bg_color)));

        self.settings_subs.clear();

        self.settings_subs.push(
            cx.subscribe(&opacity_slider, |this, _s, ev: &SliderEvent, cx| {
                let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
                if let SliderValue::Single(x) = v {
                    let op = (*x / 100.0).clamp(0.3, 1.0);
                    this.set_appearance(move |a| a.opacity = op, cx);
                }
            }),
        );
        self.settings_subs.push(cx.subscribe(
            &ui_font_size_slider,
            |this, _s, ev: &SliderEvent, cx| {
                let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
                if let SliderValue::Single(x) = v {
                    let size = x
                        .round()
                        .clamp(MIN_UI_FONT_PX as f32, MAX_UI_FONT_PX as f32)
                        as u32;
                    this.set_appearance(move |a| a.ui_font_px = size, cx);
                }
            },
        ));
        self.settings_subs.push(cx.subscribe(
            &font_size_slider,
            |this, _s, ev: &SliderEvent, cx| {
                let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
                if let SliderValue::Single(x) = v {
                    let size = x.round().clamp(
                        terminal_view::MIN_FONT_PX as f32,
                        terminal_view::MAX_FONT_PX as f32,
                    ) as u32;
                    terminal_view::set_font_px(size);
                    this.set_appearance(move |a| a.font_px = size, cx);
                }
            },
        ));
        self.settings_subs.push(cx.subscribe(
            &bg_image_opacity_slider,
            |this, _s, ev: &SliderEvent, cx| {
                let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
                if let SliderValue::Single(x) = v {
                    let op = (*x / 100.0).clamp(0.0, 1.0);
                    this.set_appearance(move |a| a.bg_image_opacity = op, cx);
                }
            },
        ));
        self.settings_subs.push(cx.subscribe(
            &bg_color_picker,
            |this, _s, ev: &ColorPickerEvent, cx| {
                let ColorPickerEvent::Change(c) = ev;
                if let Some(hsla) = c {
                    let color = hsla_to_rgb(*hsla);
                    this.set_appearance(move |a| a.bg_color = color, cx);
                }
            },
        ));
        self.opacity_slider = Some(opacity_slider);
        self.ui_font_size_slider = Some(ui_font_size_slider);
        self.font_size_slider = Some(font_size_slider);
        self.bg_image_opacity_slider = Some(bg_image_opacity_slider);
        self.bg_color_picker = Some(bg_color_picker);
    }

    /// 无 window 版：改全局 + 存盘 + 重绘。窗口背景（透明/模糊）由 render 里的
    /// applied_window_bg 同步——供 slider/color_picker 的订阅回调用（它们拿不到 window）。
    pub fn set_appearance(&mut self, f: impl FnOnce(&mut Appearance), cx: &mut Context<Self>) {
        apply_appearance(f, cx);
        cx.notify();
        cx.refresh_windows();
    }

    /// 切换在线更新通道并立即检查一次。通道切换只影响后续更新来源，不改变当前包的
    /// 展示版本；若新通道指向不同发布包，仍按 URL 差异正常下载。
    pub fn set_update_channel(&mut self, channel: updater::UpdateChannel, cx: &mut Context<Self>) {
        if cx.global::<UpdateSettings>().channel == channel {
            return;
        }
        if !self.update_status.can_check() {
            return;
        }
        apply_update_channel(channel, cx);
        self.check_for_update(false, cx);
    }

    /// 后台刷新运行时那台机器上 Agent CLI 的诊断快照。
    ///
    /// 这条路径只运行固定的 `--version`，不启动 agent 或访问网络；因此设置页可
    /// 安全地自动首查，并允许用户手动复查 PATH 变动。
    pub fn refresh_acp_runtime(&mut self, cx: &mut Context<Self>) {
        if cx
            .try_global::<AcpRuntimeState>()
            .is_some_and(|state| state.refreshing || state.installing.is_some())
        {
            return;
        }
        cx.set_global(AcpRuntimeState {
            refreshing: true,
            ..cx.try_global::<AcpRuntimeState>()
                .cloned()
                .unwrap_or_default()
        });
        cx.spawn(async move |this, cx| {
            let diagnostics = cx
                .background_executor()
                .spawn(async { inspect_agent_clis() })
                .await;
            let _ = this.update(cx, |_this, cx| {
                cx.set_global(AcpRuntimeState {
                    diagnostics: diagnostics.or_else(|| {
                        cx.try_global::<AcpRuntimeState>()
                            .and_then(|state| state.diagnostics.clone())
                    }),
                    refreshing: false,
                    checked_at: Some(Instant::now()),
                    ..cx.try_global::<AcpRuntimeState>()
                        .cloned()
                        .unwrap_or_default()
                });
            });
        })
        .detach();
    }

    /// 用户确认后，在后台通过本机包管理器安装缺失的 Agent CLI。安装结束无论成功
    /// 与否都重新探测一次，因此 PATH 改动、包管理器成功但 CLI 不可执行等状态能
    /// 立即反映在设置页，而不是只报一个笼统的「完成」。
    pub fn install_acp_cli(&mut self, agent: ConversationAgentKind, cx: &mut Context<Self>) {
        if cx
            .try_global::<AcpRuntimeState>()
            .is_some_and(|state| state.installing.is_some() || state.refreshing)
        {
            return;
        }
        cx.set_global(AcpRuntimeState {
            installing: Some(agent),
            install_message: None,
            install_failed: false,
            ..cx.try_global::<AcpRuntimeState>()
                .cloned()
                .unwrap_or_default()
        });
        cx.spawn(async move |this, cx| {
            let (result, diagnostics) = cx
                .background_executor()
                .spawn(async move {
                    match crate::runtime_call::call::<smelt_model::control_api::AgentCliInstall>(
                        smelt_model::control_api::AgentCliInstallParams {
                            agent: agent.id().to_string(),
                        },
                        CLI_INSTALL_TIMEOUT,
                    ) {
                        Ok(installed) => (Ok(installed.installer), Some(installed.diagnostics)),
                        // 装到一半失败也可能已经改了东西，重新探测一次。
                        Err(error) => (
                            Err(crate::runtime_call::error_text(error)),
                            inspect_agent_clis(),
                        ),
                    }
                })
                .await;
            let _ = this.update(cx, |_this, cx| {
                // 探测不到可执行文件的（dsh 走 npx，没有独立 CLI）不当作"没装"：
                // 安装成功就照直说成功，别让一个探不到的事实盖掉真实结果。
                let executable_available = diagnostics
                    .as_ref()
                    .and_then(|diagnostics| diagnostics.for_agent(agent))
                    .is_none_or(|executable| executable.is_available());
                let (install_message, install_failed) = match result {
                    Ok(installer) if executable_available => (
                        format!(
                            "{} 已通过 {installer} 安装；请在首次启动时完成登录。",
                            agent.label(),
                        )
                        .into(),
                        false,
                    ),
                    Ok(installer) => (
                        format!(
                            "{} 已通过 {installer} 安装，但当前搜索路径未找到 CLI；重启 Smelt 或检查 PATH 后重试检测。",
                            agent.label(),
                        )
                        .into(),
                        true,
                    ),
                    Err(error) => (format!("安装失败：{error}").into(), true),
                };
                cx.set_global(AcpRuntimeState {
                    diagnostics: diagnostics.or_else(|| {
                        cx.try_global::<AcpRuntimeState>()
                            .and_then(|state| state.diagnostics.clone())
                    }),
                    refreshing: false,
                    checked_at: Some(Instant::now()),
                    installing: None,
                    install_message: Some(install_message),
                    install_failed,
                });
            });
        })
        .detach();
    }

    /// 启动项条数变了就重建输入框（增删后调用）。
    pub fn reset_launch_inputs(&mut self) {
        self.launch_inputs = None;
    }

    /// 懒创建启动项列表编辑器（需要 window）。
    pub fn ensure_launch_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = cx.global::<LaunchConfig>().entries.len();
        let stale = self
            .launch_inputs
            .as_ref()
            .is_none_or(|i| i.rows.len() != count);
        if stale {
            self.init_launch_inputs(window, cx);
        }
    }

    fn init_launch_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::{InputEvent, InputState};

        let entries = cx.global::<LaunchConfig>().entries.clone();
        let save_on = |ev: &InputEvent| matches!(ev, InputEvent::Change | InputEvent::Blur);
        let mut rows = Vec::new();
        let mut subs = Vec::new();
        for (i, entry) in entries.iter().enumerate() {
            let label_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("显示名称")
                    .default_value(entry.label.clone())
            });
            let command_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("启动命令，如 claude")
                    .default_value(entry.command.clone())
            });
            subs.push(
                cx.subscribe(&label_input, move |_, s, ev: &InputEvent, cx| {
                    if save_on(ev) {
                        let v = s.read(cx).value().to_string();
                        apply_launch_config(
                            |c| {
                                if let Some(e) = c.entries.get_mut(i) {
                                    e.label = v;
                                }
                            },
                            cx,
                        );
                    }
                }),
            );
            subs.push(
                cx.subscribe(&command_input, move |_, s, ev: &InputEvent, cx| {
                    if save_on(ev) {
                        let v = s.read(cx).value().to_string();
                        apply_launch_config(
                            |c| {
                                if let Some(e) = c.entries.get_mut(i) {
                                    e.command = v;
                                }
                            },
                            cx,
                        );
                    }
                }),
            );
            rows.push((
                label_input,
                command_input,
                !smelt_model::launch_settings::launch_entry_is_builtin(entry),
            ));
        }
        self.launch_inputs = Some(LaunchInputs { rows, _subs: subs });
    }

    pub fn add_launch_entry(&mut self, cx: &mut Context<Self>) {
        apply_launch_config(
            |c| {
                c.entries.push(LaunchEntry {
                    label: "新启动项".into(),
                    command: String::new(),
                    provider: None,
                });
            },
            cx,
        );
        self.reset_launch_inputs();
        cx.notify();
    }

    pub fn remove_launch_entry(&mut self, index: usize, cx: &mut Context<Self>) {
        apply_launch_config(
            |c| {
                remove_launch_entry_at(c, index);
            },
            cx,
        );
        self.reset_launch_inputs();
        cx.notify();
    }

    /// 手动添加 workspace 条数变了就重建输入框（增删后调用）。
    pub fn reset_profile_inputs(&mut self) {
        self.profile_inputs = None;
    }

    /// 懒创建 workspace 列表编辑器（需要 window）。
    pub fn ensure_profile_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = cx.global::<AgentHostState>().profiles.len();
        let stale = self
            .profile_inputs
            .as_ref()
            .is_none_or(|i| i.rows.len() != count);
        if stale {
            self.init_profile_inputs(window, cx);
        }
    }

    fn init_profile_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::{InputEvent, InputState};

        let profiles = cx.global::<AgentHostState>().profiles.clone();
        let save_on = |ev: &InputEvent| matches!(ev, InputEvent::Change | InputEvent::Blur);
        let mut rows = Vec::new();
        let mut subs = Vec::new();
        for (i, p) in profiles.iter().enumerate() {
            let id = p.id.clone();
            let label_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("显示名称")
                    .default_value(p.label.clone())
            });
            let dir_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("workspace 目录，如 ~/.claude-quant")
                    .default_value(p.workspace_dir.clone())
            });
            let id_for_label = id.clone();
            subs.push(
                cx.subscribe(&label_input, move |_, s, ev: &InputEvent, cx| {
                    if save_on(ev) {
                        let v = s.read(cx).value().to_string();
                        let id = id_for_label.clone();
                        apply_agent_host(
                            move |c| {
                                if let Some(p) = c.profiles.iter_mut().find(|p| p.id == id) {
                                    p.label = v;
                                }
                            },
                            cx,
                        );
                    }
                }),
            );
            let id_for_dir = id.clone();
            subs.push(cx.subscribe(&dir_input, move |_, s, ev: &InputEvent, cx| {
                if save_on(ev) {
                    let v = s.read(cx).value().to_string();
                    let id = id_for_dir.clone();
                    apply_agent_host(
                        move |c| {
                            if let Some(p) = c.profiles.iter_mut().find(|p| p.id == id) {
                                p.workspace_dir = v;
                            }
                        },
                        cx,
                    );
                }
            }));
            let _ = i;
            rows.push((label_input, dir_input));
        }
        self.profile_inputs = Some(ProfileInputs { rows, _subs: subs });
    }

    /// 新增一个手动 workspace：默认接 Claude（用户改目录之前就得选个 agent，
    /// Claude 是最常见的场景，跟「新建 ACP 对话」菜单的默认排位一致）。
    pub fn add_profile(&mut self, cx: &mut Context<Self>) {
        apply_agent_host(
            |c| {
                c.profiles.push(AcpProfile {
                    native_launch: None,
                    id: uuid::Uuid::new_v4().to_string(),
                    kind_id: ConversationAgentKind::Claude.id().to_string(),
                    label: "新 workspace".into(),
                    workspace_dir: String::new(),
                });
            },
            cx,
        );
        self.reset_profile_inputs();
        cx.notify();
    }

    pub fn remove_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        apply_agent_host(
            |c| {
                if index < c.profiles.len() {
                    c.profiles.remove(index);
                }
            },
            cx,
        );
        self.reset_profile_inputs();
        cx.notify();
    }

    /// 改某个 workspace 接的 agent 种类（下拉菜单选中项回调）。
    pub fn set_profile_kind(
        &mut self,
        index: usize,
        kind: ConversationAgentKind,
        cx: &mut Context<Self>,
    ) {
        apply_agent_host(
            move |c| {
                if let Some(p) = c.profiles.get_mut(index) {
                    p.kind_id = kind.id().to_string();
                }
            },
            cx,
        );
        cx.notify();
    }

    /// 缓存里的 Pi 模型设置；还没从运行时取到时给出原因。
    pub(super) fn pi_model_settings_view(
        &self,
    ) -> Result<smelt_model::pi_model_settings::PiModelSettings, String> {
        match &self.pi_config {
            Some(config) => config.model_settings.clone(),
            None => Err(self
                .pi_config_error
                .clone()
                .unwrap_or_else(|| "正在读取 Pi 配置…".to_string())),
        }
    }

    /// 凭据或模型目录在别处变了（登录、登出）：重新取快照，并告知活着的会话。
    pub(super) fn reload_pi_model_settings(&mut self, cx: &mut Context<Self>) {
        smelt_ui::provider_status_global::ProviderStatus::bump_pi_catalog(cx);
        self.refresh_pi_config(cx);
    }

    pub fn edit_pi_model_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::InputState;

        self.pi_model_editor_error = None;
        let settings = match self.pi_model_settings_view() {
            Ok(settings) => settings,
            Err(error) => {
                self.pi_model_editor_error = Some(error.clone());
                crate::status_item::notify_error(error);
                cx.notify();
                return;
            }
        };
        self.pi_custom_provider_editor = None;

        let provider = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("提供方，如 deepseek / anthropic")
                .default_value(settings.default_model.provider)
        });
        let model = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("模型 ID，如 deepseek-chat")
                .default_value(settings.default_model.model)
        });
        let base_url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("留空使用官方默认端点")
                .default_value(settings.default_model.base_url)
        });
        let api_key = cx.new(|cx| {
            InputState::new(window, cx).masked(true).placeholder(
                if settings.credential_configured {
                    "已配置；输入新值以覆盖"
                } else {
                    "输入 API key"
                },
            )
        });
        self.pi_model_editor = Some(PiModelEditor {
            provider,
            model,
            base_url,
            api_key,
            thinking_level: settings.default_model.thinking_level,
            credential_configured: settings.credential_configured,
        });
        cx.notify();
    }

    pub fn cancel_pi_model_edit(&mut self, cx: &mut Context<Self>) {
        self.pi_model_editor = None;
        self.pi_model_editor_error = None;
        cx.notify();
    }

    /// 直接改默认模型的推理强度。只在用户选过默认模型时可用（界面会禁用；运行时
    /// 也会拒绝），否则写进去的 provider/model 会是 Pi 的兜底值，等于替用户做了选择。
    pub fn set_pi_default_thinking_level(&mut self, level: String, cx: &mut Context<Self>) {
        // 手动填写表单开着时也同步，免得稍后保存表单把这次改动盖回去。
        if let Some(editor) = &mut self.pi_model_editor {
            editor.thinking_level = level.clone();
        }
        self.run_pi_mutation::<smelt_model::control_api::PiModelsSetThinkingLevel>(
            smelt_model::pi_service::ThinkingLevelParams { level },
            true,
            cx,
            |_, error, _| crate::status_item::notify_error(&error),
        );
        cx.notify();
    }

    pub fn save_pi_model_settings(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.pi_model_editor.clone() else {
            return;
        };
        let provider = editor.provider.read(cx).value().trim().to_string();
        if provider.is_empty() {
            crate::status_item::notify_error("提供方不能为空");
            return;
        }
        let model = editor.model.read(cx).value().trim().to_string();
        if model.is_empty() {
            crate::status_item::notify_error("模型 ID 不能为空");
            return;
        }
        let base_url = editor.base_url.read(cx).value().trim().to_string();
        if !base_url.is_empty() {
            match url::Url::parse(&base_url) {
                Ok(url) if matches!(url.scheme(), "http" | "https") => {}
                _ => {
                    crate::status_item::notify_error("API 地址必须是完整的 HTTP(S) URL");
                    return;
                }
            }
        }
        let api_key = editor.api_key.read(cx).value().to_string();
        if api_key.is_empty() && !editor.credential_configured {
            crate::status_item::notify_error("请输入 API key");
            return;
        }

        let params = smelt_model::pi_service::SaveDefaultModelParams {
            config: smelt_model::pi_model_settings::PiDefaultModelConfig {
                provider,
                model,
                base_url,
                thinking_level: editor.thinking_level,
            },
            api_key: (!api_key.is_empty()).then_some(api_key),
            reset_base_url: true,
        };
        self.run_pi_call::<smelt_model::control_api::PiModelsSaveDefault>(
            params,
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
            cx,
            |this, result, cx| match result {
                Ok(config) => {
                    this.pi_model_editor = None;
                    this.pi_model_editor_error = None;
                    this.adopt_pi_config(config, true, cx);
                    crate::status_item::notify_success("Pi 模型配置已保存");
                }
                Err(error) => this.reject_pi_model_edit(error, cx),
            },
        );
    }

    fn pi_custom_model_editor(
        model: Option<&smelt_model::pi_model_settings::PiCustomModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PiCustomModelEditor {
        let (id, name, context_window, max_tokens) = Self::custom_model_inputs(
            model.map(|model| model.id.clone()).unwrap_or_default(),
            model.map(|model| model.name.clone()).unwrap_or_default(),
            model
                .and_then(|model| model.context_window)
                .map(|value| value.to_string())
                .unwrap_or_default(),
            model
                .and_then(|model| model.max_tokens)
                .map(|value| value.to_string())
                .unwrap_or_default(),
            window,
            cx,
        );
        PiCustomModelEditor {
            id,
            name,
            context_window,
            max_tokens,
        }
    }

    pub fn edit_pi_custom_provider(
        &mut self,
        provider_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::input::InputState;

        let settings = match self.pi_model_settings_view() {
            Ok(settings) => settings,
            Err(error) => {
                self.pi_model_editor_error = Some(error.clone());
                crate::status_item::notify_error(error);
                cx.notify();
                return;
            }
        };
        let existing = provider_id.as_ref().and_then(|provider_id| {
            settings
                .custom_providers
                .iter()
                .find(|provider| &provider.id == provider_id)
                .cloned()
        });
        let previous_id = existing.as_ref().map(|provider| provider.id.clone());
        let provider_value = existing
            .as_ref()
            .map(|provider| provider.id.clone())
            .unwrap_or_default();
        let display_name_value = existing
            .as_ref()
            .map(|provider| provider.display_name.clone())
            .unwrap_or_default();
        let base_url_value = existing
            .as_ref()
            .map(|provider| provider.base_url.clone())
            .unwrap_or_default();
        let id = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("例如 acme-gateway")
                .default_value(provider_value)
        });
        let display_name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("可选，默认用 Provider ID")
                .default_value(display_name_value)
        });
        let api_key = cx.new(|cx| {
            InputState::new(window, cx).masked(true).placeholder(
                if existing
                    .as_ref()
                    .is_some_and(|provider| provider.credential_configured)
                {
                    "已配置；输入新值以覆盖"
                } else {
                    "API key（可留空）"
                },
            )
        });
        let base_url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://gateway.example/v1")
                .default_value(base_url_value)
        });
        // 标记为「自动」的 provider 打开时不回填模型行：配置里那份是我们刷进去的，
        // 回填等于让用户一保存就把它变回手工维护。留空即代表「继续自动」。
        let auto = previous_id.as_deref().is_some_and(|id| {
            self.pi_config
                .as_ref()
                .is_some_and(|config| config.auto_provider_ids.iter().any(|auto| auto == id))
        });
        let mut models = if auto {
            Vec::new()
        } else {
            existing
                .as_ref()
                .map(|provider| {
                    provider
                        .models
                        .iter()
                        .map(|model| Self::pi_custom_model_editor(Some(model), window, cx))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        if models.is_empty() && !auto {
            models.push(Self::pi_custom_model_editor(None, window, cx));
        }
        self.pi_model_editor = None;
        self.pi_model_editor_error = None;
        self.pi_custom_provider_editor = Some(PiCustomProviderEditor {
            previous_id,
            id,
            display_name,
            api_key,
            base_url,
            api: existing
                .as_ref()
                .map(|provider| provider.api.clone())
                .filter(|api| !api.is_empty())
                .unwrap_or_else(|| "openai-completions".to_string()),
            models,
            discovery: None,
            credential_configured: existing
                .as_ref()
                .is_some_and(|provider| provider.credential_configured),
        });
        cx.notify();
    }

    /// 问端点它提供哪些模型，省得用户一个一个手打。
    ///
    /// 走 Pi 自己的直连发现，不经过 dsh 的 Node 桥：只用 Pi 的用户机器上没有那
    /// 套东西，而列模型本来就只是一次 `GET /models`。
    pub fn discover_pi_custom_models(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.pi_custom_provider_editor.as_ref() else {
            return;
        };
        if matches!(editor.discovery, Some(ModelDiscoveryState::Loading)) {
            return;
        }
        // 编辑器里没填 key 时，运行时用这个 provider 已存的那份（占位符写的就是「已配置」）。
        let params = smelt_model::pi_service::DiscoverModelsParams {
            base_url: editor.base_url.read(cx).value().trim().to_string(),
            api: editor.api.clone(),
            api_key: editor.api_key.read(cx).value().trim().to_string(),
            saved_provider_id: editor.previous_id.clone(),
        };
        if let Err(error) = smelt_model::pi_model_discovery::models_endpoint(&params.base_url) {
            self.fail_pi_discovery(error, cx);
            return;
        }
        if let Some(editor) = self.pi_custom_provider_editor.as_mut() {
            editor.discovery = Some(ModelDiscoveryState::Loading);
        }
        cx.notify();
        self.run_pi_call::<smelt_model::control_api::PiProvidersDiscoverModels>(
            params,
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
            cx,
            |this, outcome, cx| {
                if let Some(editor) = this.pi_custom_provider_editor.as_mut() {
                    editor.discovery = Some(match outcome {
                        Ok(result) => ModelDiscoveryState::Ready(result.models),
                        Err(error) => ModelDiscoveryState::Failed(error),
                    });
                }
                cx.notify();
            },
        );
    }

    fn fail_pi_discovery(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(editor) = self.pi_custom_provider_editor.as_mut() {
            editor.discovery = Some(ModelDiscoveryState::Failed(error));
        }
        cx.notify();
    }

    /// 把发现结果里的一个模型填进下面的列表。优先复用一行还空着的，避免每点一个
    /// 就在末尾留一行空白。
    pub fn adopt_pi_discovered_model(
        &mut self,
        model: smelt_model::provider_api::DiscoveredModel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.pi_custom_provider_editor.as_ref() else {
            return;
        };
        if editor
            .models
            .iter()
            .any(|row| row.id.read(cx).value().trim() == model.id)
        {
            return;
        }
        let blank = editor
            .models
            .iter()
            .position(|row| row.id.read(cx).value().trim().is_empty());
        let row = Self::pi_custom_model_editor(
            Some(&smelt_model::pi_model_settings::PiCustomModel {
                id: model.id.clone(),
                name: model.name.clone().unwrap_or_default(),
                context_window: model.context_window,
                max_tokens: model.max_tokens,
            }),
            window,
            cx,
        );
        if let Some(editor) = self.pi_custom_provider_editor.as_mut() {
            match blank {
                Some(index) => editor.models[index] = row,
                None => editor.models.push(row),
            }
        }
        cx.notify();
    }

    pub fn set_pi_custom_api(&mut self, api: &str, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.pi_custom_provider_editor else {
            return;
        };
        editor.api = smelt_model::provider_api::normalize_provider_api(api).to_string();
        cx.notify();
    }

    pub fn add_pi_custom_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let row = Self::pi_custom_model_editor(None, window, cx);
        if let Some(editor) = &mut self.pi_custom_provider_editor {
            editor.models.push(row);
        }
        cx.notify();
    }

    pub fn remove_pi_custom_model(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.pi_custom_provider_editor else {
            return;
        };
        if index < editor.models.len() {
            editor.models.remove(index);
            cx.notify();
        }
    }

    pub fn cancel_pi_custom_provider(&mut self, cx: &mut Context<Self>) {
        self.pi_custom_provider_editor = None;
        self.pi_model_editor_error = None;
        cx.notify();
    }

    /// 校验没过时必须写进表单：系统通知在前台窗口上经常看不见，用户会以为保存坏了。
    fn reject_pi_model_edit(&mut self, error: impl Into<String>, cx: &mut Context<Self>) {
        let error = error.into();
        self.pi_model_editor_error = Some(error.clone());
        crate::status_item::notify_error(&error);
        cx.notify();
    }

    pub fn save_pi_custom_provider(
        &mut self,
        set_default: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.pi_custom_provider_editor.clone() else {
            return;
        };
        let id = editor.id.read(cx).value().trim().to_string();
        if let Err(error) = smelt_model::pi_model_settings::validate_custom_provider_id(&id) {
            self.reject_pi_model_edit(error, cx);
            return;
        }

        // 显示名称可空：落盘时用 Provider ID 顶上。这里再拦一次，用户选完模型往下
        // 点保存，只会看到按钮没反应——报错还只走系统通知。
        let display_name = editor.display_name.read(cx).value().trim().to_string();

        let base_url = editor.base_url.read(cx).value().trim().to_string();
        if base_url.is_empty() {
            self.reject_pi_model_edit("请填写 API 地址", cx);
            return;
        }
        match url::Url::parse(&base_url) {
            Ok(url) if matches!(url.scheme(), "http" | "https") => {}
            _ => {
                self.reject_pi_model_edit("API 地址必须是完整的 HTTP(S) URL", cx);
                return;
            }
        }

        let mut models = Vec::new();
        for row in &editor.models {
            let model_id = row.id.read(cx).value().trim().to_string();
            if model_id.is_empty() {
                continue;
            }
            let name = row.name.read(cx).value().trim().to_string();
            let context_window = row
                .context_window
                .read(cx)
                .value()
                .trim()
                .parse::<u64>()
                .ok();
            let max_tokens = row.max_tokens.read(cx).value().trim().parse::<u64>().ok();
            models.push(smelt_model::pi_model_settings::PiCustomModel {
                id: model_id.clone(),
                name: if name.is_empty() { model_id } else { name },
                context_window,
                max_tokens,
            });
        }
        let api_key = editor.api_key.read(cx).value().trim().to_string();
        let display_name = if display_name.is_empty() {
            id.clone()
        } else {
            display_name
        };
        // 一个模型都没填 = 「这份目录我不维护，照端点上的来」：运行时先问一次端点，
        // 问到了才落盘，并标成自动维护。
        let from_endpoint = models.is_empty();
        if from_endpoint {
            if let Err(error) = smelt_model::pi_model_discovery::models_endpoint(&base_url) {
                self.fail_pi_discovery(format!("{error}；或手动填写至少一个模型"), cx);
                return;
            }
            if let Some(editor) = self.pi_custom_provider_editor.as_mut() {
                editor.discovery = Some(ModelDiscoveryState::Loading);
            }
            cx.notify();
        }
        let params = smelt_model::pi_service::SaveCustomProviderParams {
            previous_id: editor.previous_id.clone(),
            provider: smelt_model::pi_model_settings::PiCustomProvider {
                id: id.clone(),
                display_name,
                api: editor.api,
                base_url,
                credential_configured: editor.credential_configured || !api_key.is_empty(),
                models,
            },
            api_key: (!api_key.is_empty()).then_some(api_key),
            set_default,
        };
        self.run_pi_call::<smelt_model::control_api::PiProvidersSaveCustom>(
            params,
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
            cx,
            move |this, result, cx| match result {
                Ok(saved) => {
                    this.pi_custom_provider_editor = None;
                    this.pi_model_editor_error = None;
                    this.adopt_pi_config(saved.config, true, cx);
                    crate::status_item::notify_success(if saved.auto {
                        format!("自定义 Provider {id} 已保存，模型目录将在每次启动时刷新")
                    } else {
                        format!("自定义 Provider {id} 已保存")
                    });
                }
                Err(error) if from_endpoint => this.fail_pi_discovery(error, cx),
                Err(error) => this.reject_pi_model_edit(error, cx),
            },
        );
    }

    pub fn remove_pi_custom_provider(
        &mut self,
        provider_id: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_pi_call::<smelt_model::control_api::PiProvidersRemoveCustom>(
            smelt_model::pi_service::ProviderIdParams {
                provider_id: provider_id.clone(),
            },
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
            cx,
            move |this, result, cx| match result {
                Ok(config) => {
                    this.adopt_pi_config(config, true, cx);
                    crate::status_item::notify_success(format!(
                        "已移除自定义 Provider {provider_id}"
                    ));
                }
                Err(error) => crate::status_item::notify_error(error),
            },
        );
    }

    /// 让运行时确保配置文件存在，再用系统文本编辑器打开。文件在运行时那台机器上，
    /// 只有本机运行时打得开。
    pub(super) fn open_pi_config(
        &mut self,
        file_kind: PiConfigFileKind,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let file = match file_kind {
            PiConfigFileKind::Settings => smelt_model::pi_service::PiConfigFile::Settings,
            PiConfigFileKind::Models => smelt_model::pi_service::PiConfigFile::Models,
            PiConfigFileKind::Credentials => smelt_model::pi_service::PiConfigFile::Credentials,
            PiConfigFileKind::Mcp => smelt_model::pi_service::PiConfigFile::Mcp,
        };
        self.run_pi_call::<smelt_model::control_api::PiConfigEnsureFile>(
            smelt_model::pi_service::EnsureConfigFileParams { file },
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
            cx,
            |_, result, _| {
                let path = match result {
                    Ok(result) => std::path::PathBuf::from(result.path),
                    Err(error) => {
                        crate::status_item::notify_error(error);
                        return;
                    }
                };
                match crate::ide::open_in_system_text_editor(&path) {
                    Ok(()) => crate::status_item::notify_success(format!(
                        "已在系统文本编辑器打开 {}",
                        path.display()
                    )),
                    Err(error) => crate::status_item::notify_error(error),
                }
            },
        );
    }

    pub(super) fn refresh_pi_settings(&mut self, cx: &mut Context<Self>) {
        self.reload_pi_model_settings(cx);
        cx.notify();
    }

    pub(super) fn refresh_pi_plugins(&mut self, cx: &mut Context<Self>) {
        self.pi_plugin_pending_delete = None;
        self.refresh_pi_config(cx);
        cx.notify();
    }

    /// 点一下删除只是进入待确认状态；再点一次才真删。
    pub(super) fn request_pi_plugin_delete(&mut self, id: String, cx: &mut Context<Self>) {
        self.pi_plugin_error = None;
        self.pi_plugin_pending_delete = Some(id);
        cx.notify();
    }

    pub(super) fn cancel_pi_plugin_delete(&mut self, cx: &mut Context<Self>) {
        self.pi_plugin_pending_delete = None;
        cx.notify();
    }

    /// 真删 `~/.pi` 下的实体，并把所有引用它的智能体的勾选一并清掉。
    pub(super) fn confirm_pi_plugin_delete(&mut self, id: String, cx: &mut Context<Self>) {
        self.pi_plugin_pending_delete = None;
        self.run_pi_call::<smelt_model::control_api::PiPluginsRemove>(
            smelt_model::pi_service::PluginIdParams { id: id.clone() },
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
            cx,
            move |this, result, cx| match result {
                Ok(config) => {
                    this.pi_plugin_error = None;
                    super::apply_agent_host(
                        move |host| {
                            for agent in &mut host.agents {
                                agent.plugins.retain(|plugin_id| *plugin_id != id);
                            }
                        },
                        cx,
                    );
                    this.adopt_pi_config(config, false, cx);
                }
                Err(error) => {
                    this.pi_plugin_error = Some(error);
                    this.refresh_pi_config(cx);
                }
            },
        );
        cx.notify();
    }

    /// 导入插件：在运行时那台机器上选目录（扩展也可以是单个文件），运行时把它拷贝进
    /// 全局插件目录，之后就能在智能体里勾选。
    pub(super) fn import_pi_plugin(
        &mut self,
        kind: smelt_model::pi_plugin_catalog::PiPluginKind,
        cx: &mut Context<Self>,
    ) {
        self.open_dir_picker(
            crate::runtimes::Runtime::local(),
            crate::dir_picker::DirPickerPurpose::PluginImport { kind },
            cx,
        );
    }

    pub(crate) fn import_pi_plugin_from(
        &mut self,
        kind: smelt_model::pi_plugin_catalog::PiPluginKind,
        source: String,
        cx: &mut Context<Self>,
    ) {
        self.run_pi_mutation::<smelt_model::control_api::PiPluginsImport>(
            smelt_model::pi_service::ImportPluginParams { kind, source },
            false,
            cx,
            |this, error, _| this.pi_plugin_error = Some(error),
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiConfigFileKind {
    Settings,
    Models,
    Credentials,
    Mcp,
}

impl Workspace {
    /// 设置 / 清除背景图（不影响窗口透明度，故无需 window）。
    pub fn set_bg_image(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        apply_appearance(|a| a.bg_image = path, cx);
        cx.notify();
    }

    /// 弹原生选择框选一张背景图。
    pub fn pick_bg_image(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择背景图片".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths
                    .into_iter()
                    .next()
                    .and_then(|p| p.to_str().map(String::from))
            {
                this.update(cx, |this, cx| this.set_bg_image(Some(p), cx))
                    .ok();
            }
        })
        .detach();
    }
}

/// 装 CLI 要等包管理器下载，比普通调用久得多。
const CLI_INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// 请运行时探测各家 agent CLI。连不上运行时返回 `None`，调用方保留上一次的结果。
fn inspect_agent_clis() -> Option<smelt_model::acp_conn::AcpRuntimeDiagnostics> {
    crate::runtime_call::call::<smelt_model::control_api::AgentCliInspect>(
        smelt_runtime_client::protocol::control::EmptyParams::default(),
        crate::runtime_call::RUNTIME_CALL_TIMEOUT,
    )
    .inspect_err(|error| eprintln!("[settings] 探测 agent CLI 失败：{error}"))
    .ok()
}
