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

    /// 找出能回答能力查询的 profile。
    ///
    /// 返回 profile 名而不只是路径：桥要用 `--profile` 把 dsh 装配成**那个**
    /// profile，能力答案本来就是 profile 相关的。
    fn native_dsh_capabilities_profile() -> Option<String> {
        smelt_core::agent_kind::native_dsh_profiles()
            .into_iter()
            .find(|profile| {
                smelt_core::agent_kind::dsh_model_capabilities_entry(&profile.workspace_dir)
                    .is_some()
            })
            .map(|profile| profile.workspace_dir)
    }

    fn run_native_dsh_model_settings(
        action: &str,
        payload: Option<&serde_json::Value>,
    ) -> Result<Vec<u8>, String> {
        smelt_core::agent_kind::dsh_model_settings(action, payload)
    }

    fn load_native_dsh_model_settings() -> Result<NativeDshModelSettings, String> {
        let output = Self::run_native_dsh_model_settings("read", None)?;
        serde_json::from_slice(&output)
            .map_err(|error| format!("读取 DSH 模型设置响应失败：{error}"))
    }

    pub(super) fn reload_native_dsh_model_settings(&mut self) {
        self.dsh_native_model_settings = Some(Self::load_native_dsh_model_settings());
    }

    pub fn edit_native_dsh_model_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::InputState;

        self.dsh_model_editor_error = None;
        let settings = match Self::load_native_dsh_model_settings() {
            Ok(settings) => settings,
            Err(error) => {
                self.dsh_model_editor_error = Some(error.clone());
                crate::status_item::notify_error(error);
                cx.notify();
                return;
            }
        };
        self.dsh_native_model_settings = Some(Ok(settings.clone()));
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
        let Some(profile) = Self::native_dsh_capabilities_profile() else {
            if let Some(editor) = self.dsh_custom_provider_editor.as_mut() {
                editor.discovery = Some(ModelDiscoveryState::Failed(format!(
                    "未找到模型发现器，请把 Smelt Host bundle 更新到 {}",
                    smelt_core::agent_kind::SMELT_HOST_VERSION
                )));
            }
            cx.notify();
            return;
        };
        let trimmed = |state: &Entity<gpui_component::input::InputState>| {
            let value = state.read(cx).value().trim().to_string();
            (!value.is_empty()).then_some(value)
        };
        let request = smelt_core::agent_kind::DshDiscoveryRequest {
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
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(
                    async move { smelt_core::agent_kind::dsh_discover_models(&profile, &request) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(editor) = this.dsh_custom_provider_editor.as_mut() {
                    editor.discovery = Some(match outcome {
                        Ok(report) => ModelDiscoveryState::Ready(report.models),
                        Err(error) => ModelDiscoveryState::Failed(error),
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 把一条发现来的模型填进表单，优先复用空白行。
    pub fn adopt_native_dsh_discovered_model(
        &mut self,
        model: smelt_core::provider_api::DiscoveredModel,
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
        let Some(profile) = Self::native_dsh_capabilities_profile() else {
            self.dsh_model_capabilities = Some(NativeDshCapabilityState::Failed(format!(
                "未找到模型能力查询器，请把 Smelt Host bundle 更新到 {}",
                smelt_core::agent_kind::SMELT_HOST_VERSION
            )));
            cx.notify();
            return;
        };
        self.dsh_model_capabilities = Some(NativeDshCapabilityState::Loading);
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { smelt_core::agent_kind::dsh_model_capabilities(&profile) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.dsh_model_capabilities = Some(match outcome {
                    Ok(capabilities) => NativeDshCapabilityState::Ready(capabilities),
                    Err(error) => NativeDshCapabilityState::Failed(error),
                });
                cx.notify();
            });
        })
        .detach();
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
        let result = (|| -> Result<(), String> {
            let output = Self::run_native_dsh_model_settings("save", Some(&payload))?;
            let response: NativeDshModelSaveResponse = serde_json::from_slice(&output)
                .map_err(|error| format!("读取 DSH 模型管理器响应失败：{error}"))?;
            response
                .ok
                .then_some(())
                .ok_or_else(|| "DSH 模型管理器未确认保存".to_string())
        })();
        if let Err(error) = result {
            self.dsh_model_editor_error = Some(error.clone());
            crate::status_item::notify_error(error);
            cx.notify();
            return;
        }
        self.dsh_model_editor = None;
        self.dsh_model_editor_error = None;
        self.reload_native_dsh_model_settings();
        crate::status_item::notify_success("DeepSeek 模型配置已保存");
        cx.notify();
    }

    fn dsh_custom_model_editor(
        model: Option<&NativeDshCustomModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> DshCustomModelEditor {
        use gpui_component::input::InputState;

        let id = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("例如 gpt-5-mini")
                .default_value(model.map(|model| model.id.clone()).unwrap_or_default())
        });
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("显示名称（可选）")
                .default_value(model.map(|model| model.name.clone()).unwrap_or_default())
        });
        let context_window = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("上下文长度（可选）")
                .default_value(
                    model
                        .and_then(|model| model.context_window)
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                )
        });
        let max_tokens = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("最大输出（可选）")
                .default_value(
                    model
                        .and_then(|model| model.max_tokens)
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                )
        });
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

        let settings = match Self::load_native_dsh_model_settings() {
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
        let auto = previous_id
            .as_deref()
            .is_some_and(|id| smelt_core::dsh_auto_models::load().is_auto(id));
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
        self.dsh_native_model_settings = Some(Ok(settings));
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
        editor.api = smelt_core::provider_api::normalize_provider_api(api).to_string();
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
        window: &mut Window,
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
        let draft = NativeDshCustomProviderDraft {
            previous_id: editor.previous_id.clone(),
            id,
            display_name: editor.display_name.read(cx).value().trim().to_string(),
            api: editor.api.clone(),
            base_url,
            api_key_env,
            api_key,
            set_default,
        };
        if models.is_empty() {
            self.save_native_dsh_custom_provider_from_endpoint(draft, cx);
            return;
        }
        self.commit_native_dsh_custom_provider(draft, models, false, window, cx);
    }

    fn save_native_dsh_custom_provider_from_endpoint(
        &mut self,
        draft: NativeDshCustomProviderDraft,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = Self::native_dsh_capabilities_profile() else {
            let error = format!(
                "未找到模型发现器，请把 Smelt Host bundle 更新到 {}，或手动填写至少一个模型",
                smelt_core::agent_kind::SMELT_HOST_VERSION
            );
            if let Some(editor) = self.dsh_custom_provider_editor.as_mut() {
                editor.discovery = Some(ModelDiscoveryState::Failed(error));
            }
            cx.notify();
            return;
        };
        let request = smelt_core::agent_kind::DshDiscoveryRequest {
            provider: Some(draft.id.clone()),
            base_url: Some(draft.base_url.clone()),
            api: Some(draft.api.clone()),
            api_key: (!draft.api_key.is_empty()).then(|| draft.api_key.clone()),
        };
        if let Some(editor) = self.dsh_custom_provider_editor.as_mut() {
            editor.discovery = Some(ModelDiscoveryState::Loading);
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(
                    async move { smelt_core::agent_kind::dsh_discover_models(&profile, &request) },
                )
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.dsh_custom_provider_editor.is_none() {
                    return;
                }
                let report = match outcome {
                    Ok(report) if report.models.is_empty() => {
                        this.fail_native_dsh_custom_provider_save(
                            "该端点没有报告任何模型，请手动填写至少一个模型".to_string(),
                            cx,
                        );
                        return;
                    }
                    Ok(report) => report,
                    Err(error) => {
                        this.fail_native_dsh_custom_provider_save(
                            format!("{error}\n未能自动获取模型，请手动填写至少一个模型"),
                            cx,
                        );
                        return;
                    }
                };
                let models = report
                    .models
                    .iter()
                    .map(|model| {
                        serde_json::json!({
                            "id": model.id,
                            "name": model.name,
                            "contextWindow": model.context_window,
                            "maxTokens": model.max_tokens,
                        })
                    })
                    .collect::<Vec<_>>();
                this.commit_native_dsh_custom_provider(draft, models, true, window, cx);
            });
        })
        .detach();
    }

    fn fail_native_dsh_custom_provider_save(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(editor) = self.dsh_custom_provider_editor.as_mut() {
            editor.discovery = Some(ModelDiscoveryState::Failed(error));
        }
        cx.notify();
    }

    fn commit_native_dsh_custom_provider(
        &mut self,
        draft: NativeDshCustomProviderDraft,
        models: Vec<serde_json::Value>,
        auto: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let payload = serde_json::json!({
            "previousId": draft.previous_id,
            "id": draft.id,
            "displayName": draft.display_name,
            "api": draft.api,
            "baseURL": draft.base_url,
            "apiKeyEnv": draft.api_key_env,
            "apiKey": (!draft.api_key.is_empty()).then_some(draft.api_key.clone()),
            "models": models,
            "setDefault": draft.set_default,
        });
        let result =
            Self::run_native_dsh_model_settings("save-custom", Some(&payload)).and_then(|output| {
                let response: NativeDshModelSaveResponse = serde_json::from_slice(&output)
                    .map_err(|error| format!("读取 DSH 模型管理器响应失败：{error}"))?;
                response
                    .ok
                    .then_some(())
                    .ok_or_else(|| "DSH 模型管理器未确认保存".to_string())
            });
        if let Err(error) = result {
            self.dsh_model_editor_error = Some(error.clone());
            self.fail_native_dsh_custom_provider_save(error.clone(), cx);
            crate::status_item::notify_error(error);
            return;
        }
        smelt_core::dsh_auto_models::update(|store| {
            if let Some(previous) = draft.previous_id.as_deref() {
                store.rename(previous, &draft.id);
            }
            store.set_auto(&draft.id, auto);
        });
        self.dsh_custom_provider_editor = None;
        self.dsh_model_editor_error = None;
        self.reload_native_dsh_model_settings();
        crate::status_item::notify_success(match (auto, draft.set_default) {
            (true, true) => "自定义 Provider 已保存并设为默认；模型目录将在每次启动时刷新",
            (true, false) => "自定义 Provider 已保存；模型目录将在每次启动时刷新",
            (false, true) => "自定义 Provider 已保存并设为默认",
            (false, false) => "自定义 Provider 已保存",
        });
        cx.notify();
    }

    pub(super) fn open_native_dsh_config(
        &mut self,
        credentials: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let path = if credentials {
            smelt_core::agent_kind::dsh_credentials_path()
        } else {
            smelt_core::agent_kind::dsh_settings_path()
        };
        let Some(path) = path else {
            crate::status_item::notify_error("无法定位 DSH_HOME");
            return;
        };
        if !path.exists() {
            use std::io::Write as _;

            if let Some(parent) = path.parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                crate::status_item::notify_error(format!("创建 DSH 配置目录失败：{error}"));
                return;
            }
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(b"") {
                        crate::status_item::notify_error(format!("创建 DSH 配置文件失败：{error}"));
                        return;
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt as _;
                        if let Err(error) =
                            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                        {
                            crate::status_item::notify_error(format!(
                                "保护 DSH 配置文件失败：{error}"
                            ));
                            return;
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    crate::status_item::notify_error(format!("创建 DSH 配置文件失败：{error}"));
                    return;
                }
            }
        }
        match crate::ide::open_in_system_text_editor(&path) {
            Ok(()) => crate::status_item::notify_success(format!(
                "已在系统文本编辑器打开 {}",
                path.display()
            )),
            Err(error) => crate::status_item::notify_error(error),
        }
    }

    pub(super) fn refresh_native_dsh_settings(&mut self, cx: &mut Context<Self>) {
        self.reload_native_dsh_model_settings();
        cx.notify();
    }

    /// 展开/收起某个 profile 的插件管理面板。
    ///
    /// 展开即同步读一次清单：数据来自本地文件，毫秒级完成，不值得为它做一次异步
    /// 跳转和一个只闪一帧的 loading 态。真正慢的是 add/remove，那两个才走后台。
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
            plugins: Some(smelt_core::agent_kind::dsh_profile_plugins(&profile)),
            profile,
            spec,
            busy: None,
            message: None,
        });
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
            smelt_core::agent_kind::SMELT_HOST_PACKAGE,
            smelt_core::agent_kind::SMELT_HOST_VERSION
        );
        self.run_native_dsh_plugin_action(DshPluginAction::Install, spec, window, cx);
    }

    /// 重新读取当前面板的插件清单。
    pub fn reload_native_dsh_plugins(&mut self, cx: &mut Context<Self>) {
        let Some(manager) = self.dsh_plugin_manager.as_mut() else {
            return;
        };
        manager.plugins = Some(smelt_core::agent_kind::dsh_profile_plugins(
            &manager.profile,
        ));
        cx.notify();
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

        let action_profile = profile.clone();
        let action_target = target.clone();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    match action {
                        DshPluginAction::Install => {
                            smelt_core::agent_kind::dsh_plugin_add(&action_profile, &action_target)
                        }
                        DshPluginAction::Update => smelt_core::agent_kind::dsh_plugin_update(
                            &action_profile,
                            &action_target,
                        ),
                        DshPluginAction::Remove => smelt_core::agent_kind::dsh_plugin_remove(
                            &action_profile,
                            &action_target,
                        ),
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
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
                // 先重读清单再写提示：成功文案要报**真正装上的版本**。pnpm 完全
                // 可能什么都没做就退 0（范围已满足、或新版被供应链门槛挡在
                // `latest` 之外），只说一句"已安装"会把这种空转说成成功。
                manager.plugins = Some(smelt_core::agent_kind::dsh_profile_plugins(
                    &manager.profile,
                ));
                // 桥里的助手每次调用都是新起的进程，所以换掉的那一版**立刻**就在
                // 答话了——面板却还捧着上一次 read 的结果。同一个页面上半截说凭据
                // 缺失、下半截说刚装好新版，是这次更新自己制造的矛盾。
                //
                // 重读要借整个 workspace，所以面板得放手再取一次——期间面板可能已被
                // 收起或切走，取不到就把这次结果丢掉。
                this.reload_native_dsh_model_settings();
                let Some(manager) = this
                    .dsh_plugin_manager
                    .as_mut()
                    .filter(|manager| manager.profile == profile)
                else {
                    return;
                };
                manager.message = Some(match outcome {
                    Err(error) => (Self::condense_plugin_error(&error), false),
                    Ok(_) if action == DshPluginAction::Remove => {
                        (format!("已卸载 {target}，新建会话后生效"), true)
                    }
                    Ok(_) => {
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
                if action != DshPluginAction::Remove {
                    manager
                        .spec
                        .update(cx, |state, cx| state.set_value("", window, cx));
                }
                cx.notify();
            });
        })
        .detach();
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
        if name == smelt_core::agent_kind::SMELT_HOST_PACKAGE
            && !smelt_core::agent_kind::smelt_host_version_is_supported(version)
        {
            return format!(
                "已安装 {name} v{version}，但它超出本版 Smelt 验证过的范围（{}）；如遇异常请更新 Smelt",
                smelt_core::agent_kind::SMELT_HOST_VERSION
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
    /// [`dsh_plugin_update`]: smelt_core::agent_kind::dsh_plugin_update
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

    /// 后台刷新本机 Agent CLI 的诊断快照。
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
                .spawn(async { smelt_core::acp_conn::inspect_acp_runtime() })
                .await;
            let _ = this.update(cx, |_this, cx| {
                cx.set_global(AcpRuntimeState {
                    diagnostics: Some(diagnostics),
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
                    let result = smelt_core::acp_conn::install_acp_cli(agent);
                    let diagnostics = smelt_core::acp_conn::inspect_acp_runtime();
                    (result, diagnostics)
                })
                .await;
            let _ = this.update(cx, |_this, cx| {
                // 探测不到可执行文件的（dsh 走 npx，没有独立 CLI）不当作"没装"：
                // 安装成功就照直说成功，别让一个探不到的事实盖掉真实结果。
                let executable_available = diagnostics
                    .for_agent(agent)
                    .is_none_or(|executable| executable.is_available());
                let (install_message, install_failed) = match result {
                    Ok(report) if executable_available => (
                        format!(
                            "{} 已通过 {} 安装；请在首次启动时完成登录。",
                            agent.label(),
                            report.installer
                        )
                        .into(),
                        false,
                    ),
                    Ok(report) => (
                        format!(
                            "{} 已通过 {} 安装，但当前搜索路径未找到 CLI；重启 Smelt 或检查 PATH 后重试检测。",
                            agent.label(),
                            report.installer
                        )
                        .into(),
                        true,
                    ),
                    Err(error) => (format!("安装失败：{error}").into(), true),
                };
                cx.set_global(AcpRuntimeState {
                    diagnostics: Some(diagnostics),
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
            rows.push((label_input, command_input, !launch_entry_is_builtin(entry)));
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

    /// 原生 Pi 模型设置。第一次读取时才碰磁盘。
    pub(super) fn pi_model_settings_cached(
        &self,
    ) -> &Result<smelt_core::pi_model_settings::PiModelSettings, String> {
        self.pi_model_settings
            .get_or_init(smelt_core::pi_model_settings::load_pi_model_settings)
    }

    /// 丢掉缓存，下次读取重新读盘。
    pub(super) fn reload_pi_model_settings(&mut self) {
        self.pi_model_settings.take();
    }

    /// 用刚写盘的内容顶掉缓存，省掉一次立刻重读。
    fn store_pi_model_settings(
        &mut self,
        settings: smelt_core::pi_model_settings::PiModelSettings,
    ) {
        self.pi_model_settings.take();
        let _ = self.pi_model_settings.set(Ok(settings));
    }

    pub fn edit_pi_model_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::InputState;

        self.pi_model_editor_error = None;
        let settings = match smelt_core::pi_model_settings::load_pi_model_settings() {
            Ok(settings) => settings,
            Err(error) => {
                self.pi_model_editor_error = Some(error.clone());
                crate::status_item::notify_error(error);
                cx.notify();
                return;
            }
        };
        self.store_pi_model_settings(settings.clone());
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

    pub fn set_pi_thinking_level(&mut self, level: String, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.pi_model_editor {
            editor.thinking_level = level;
            cx.notify();
        }
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

        let config = smelt_core::pi_model_settings::PiDefaultModelConfig {
            provider,
            model,
            base_url,
            thinking_level: editor.thinking_level,
        };
        let api_key_opt = if api_key.is_empty() {
            None
        } else {
            Some(api_key.as_str())
        };
        if let Err(error) =
            smelt_core::pi_model_settings::save_pi_default_model(&config, api_key_opt)
        {
            self.pi_model_editor_error = Some(error.clone());
            crate::status_item::notify_error(error);
            cx.notify();
            return;
        }

        self.pi_model_editor = None;
        self.pi_model_editor_error = None;
        self.reload_pi_model_settings();
        crate::status_item::notify_success("Pi 模型配置已保存");
        cx.notify();
    }

    fn pi_custom_model_editor(
        model: Option<&smelt_core::pi_model_settings::PiCustomModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PiCustomModelEditor {
        use gpui_component::input::InputState;

        let id = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("例如 gpt-5-mini")
                .default_value(model.map(|model| model.id.clone()).unwrap_or_default())
        });
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("显示名称（可选）")
                .default_value(model.map(|model| model.name.clone()).unwrap_or_default())
        });
        let context_window = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("上下文长度（可选）")
                .default_value(
                    model
                        .and_then(|model| model.context_window)
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                )
        });
        let max_tokens = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("最大输出（可选）")
                .default_value(
                    model
                        .and_then(|model| model.max_tokens)
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                )
        });
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

        let settings = match smelt_core::pi_model_settings::load_pi_model_settings() {
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
        let auto = previous_id
            .as_deref()
            .is_some_and(|id| smelt_core::pi_auto_models::load().is_auto(id));
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
        self.store_pi_model_settings(settings);
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
        let request = smelt_core::pi_model_discovery::ModelDiscoveryRequest {
            base_url: editor.base_url.read(cx).value().trim().to_string(),
            api: editor.api.clone(),
            api_key: editor.api_key.read(cx).value().trim().to_string(),
        };
        // 编辑器里没填 key，但配置里可能已经有一份（占位符写的就是「已配置」）。
        let request = self.pi_request_with_saved_key(request, editor.previous_id.clone());
        if let Err(error) = smelt_core::pi_model_discovery::models_endpoint(&request.base_url) {
            self.fail_pi_discovery(error, cx);
            return;
        }
        if let Some(editor) = self.pi_custom_provider_editor.as_mut() {
            editor.discovery = Some(ModelDiscoveryState::Loading);
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { smelt_core::pi_model_discovery::discover_models(&request) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(editor) = this.pi_custom_provider_editor.as_mut() {
                    editor.discovery = Some(match outcome {
                        Ok(models) => ModelDiscoveryState::Ready(models),
                        Err(error) => ModelDiscoveryState::Failed(error),
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 编辑器里没现填 key 时，补上已保存的那份。
    ///
    /// 表单里的 key 输入框在「已配置」时是空的（不回显凭据）。不补这一步，编辑
    /// 一个已存在的 provider 时点「获取模型列表」必然 401。
    fn pi_request_with_saved_key(
        &self,
        mut request: smelt_core::pi_model_discovery::ModelDiscoveryRequest,
        provider_id: Option<String>,
    ) -> smelt_core::pi_model_discovery::ModelDiscoveryRequest {
        if !request.api_key.is_empty() {
            return request;
        }
        if let Some(provider_id) = provider_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            && let Some(key) = smelt_core::pi_model_settings::read_pi_provider_api_key_at(
                &smelt_core::pi_model_settings::pi_agent_dir(),
                provider_id,
            )
        {
            request.api_key = key;
        }
        request
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
        model: smelt_core::provider_api::DiscoveredModel,
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
            Some(&smelt_core::pi_model_settings::PiCustomModel {
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
        editor.api = smelt_core::provider_api::normalize_provider_api(api).to_string();
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.pi_custom_provider_editor.clone() else {
            return;
        };
        let id = editor.id.read(cx).value().trim().to_string();
        if let Err(error) = smelt_core::pi_model_settings::validate_custom_provider_id(&id) {
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
            models.push(smelt_core::pi_model_settings::PiCustomModel {
                id: model_id.clone(),
                name: if name.is_empty() { model_id } else { name },
                context_window,
                max_tokens,
            });
        }
        let api_key_for_discovery = editor.api_key.read(cx).value().trim().to_string();

        let api_key = editor.api_key.read(cx).value().trim().to_string();
        let api_key_opt = if api_key.is_empty() {
            None
        } else {
            Some(api_key.as_str())
        };

        let display_name = if display_name.is_empty() {
            id.clone()
        } else {
            display_name
        };

        let draft = PiCustomProviderDraft {
            previous_id: editor.previous_id.clone(),
            id,
            display_name,
            api: editor.api,
            base_url,
            api_key: api_key_opt.map(str::to_string),
            credential_configured: editor.credential_configured || api_key_opt.is_some(),
            set_default,
        };

        // 一个模型都没填 = 「这份目录我不维护，照端点上的来」。先问一次端点，问到
        // 了才落盘：Pi 从 models.json 解析模型，写一份空列表等于这个 provider 一个
        // 模型都选不了。
        if models.is_empty() {
            self.save_pi_custom_provider_from_endpoint(draft, api_key_for_discovery, cx);
            return;
        }
        self.commit_pi_custom_provider(draft, models, false, window, cx);
    }

    fn save_pi_custom_provider_from_endpoint(
        &mut self,
        draft: PiCustomProviderDraft,
        typed_api_key: String,
        cx: &mut Context<Self>,
    ) {
        let request = self.pi_request_with_saved_key(
            smelt_core::pi_model_discovery::ModelDiscoveryRequest {
                base_url: draft.base_url.clone(),
                api: draft.api.clone(),
                api_key: typed_api_key,
            },
            draft.previous_id.clone(),
        );
        if let Err(error) = smelt_core::pi_model_discovery::models_endpoint(&request.base_url) {
            self.fail_pi_discovery(format!("{error}；或手动填写至少一个模型"), cx);
            return;
        }
        if let Some(editor) = self.pi_custom_provider_editor.as_mut() {
            editor.discovery = Some(ModelDiscoveryState::Loading);
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { smelt_core::pi_model_discovery::discover_models(&request) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.pi_custom_provider_editor.is_none() {
                    return;
                }
                let discovered = match outcome {
                    Ok(models) if models.is_empty() => {
                        this.fail_pi_discovery(
                            "该端点没有报告任何模型，请手动填写至少一个模型".to_string(),
                            cx,
                        );
                        return;
                    }
                    Ok(models) => models,
                    Err(error) => {
                        this.fail_pi_discovery(
                            format!("{error}\n未能自动获取模型，请手动填写至少一个模型"),
                            cx,
                        );
                        return;
                    }
                };
                let models = discovered
                    .into_iter()
                    .map(|model| smelt_core::pi_model_settings::PiCustomModel {
                        name: model.label().to_string(),
                        id: model.id,
                        context_window: model.context_window,
                        max_tokens: model.max_tokens,
                    })
                    .collect();
                this.commit_pi_custom_provider(draft, models, true, window, cx);
            });
        })
        .detach();
    }

    fn commit_pi_custom_provider(
        &mut self,
        draft: PiCustomProviderDraft,
        models: Vec<smelt_core::pi_model_settings::PiCustomModel>,
        auto: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = draft.id.clone();
        let provider = smelt_core::pi_model_settings::PiCustomProvider {
            id: id.clone(),
            display_name: draft.display_name,
            api: draft.api,
            base_url: draft.base_url,
            credential_configured: draft.credential_configured,
            models,
        };

        if let Some(prev) = &draft.previous_id
            && prev != &id
        {
            let _ = smelt_core::pi_model_settings::remove_pi_custom_provider(prev);
        }

        if let Err(error) = smelt_core::pi_model_settings::save_pi_custom_provider(
            &provider,
            draft.api_key.as_deref(),
            draft.set_default,
        ) {
            self.pi_model_editor_error = Some(error.clone());
            crate::status_item::notify_error(error);
            cx.notify();
            return;
        }

        // 改名要把「自动」标记带过去，否则用户会得到一个以为自动、其实再也不刷新
        // 的 provider。
        smelt_core::pi_auto_models::update(|store| {
            if let Some(prev) = &draft.previous_id
                && prev != &id
            {
                store.set_auto(prev, false);
            }
            store.set_auto(&id, auto);
        });

        self.pi_custom_provider_editor = None;
        self.pi_model_editor_error = None;
        self.reload_pi_model_settings();
        crate::status_item::notify_success(if auto {
            format!("自定义 Provider {id} 已保存，模型目录将在每次启动时刷新")
        } else {
            format!("自定义 Provider {id} 已保存")
        });
        cx.notify();
    }

    pub fn remove_pi_custom_provider(
        &mut self,
        provider_id: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = smelt_core::pi_model_settings::remove_pi_custom_provider(&provider_id) {
            crate::status_item::notify_error(error);
            cx.notify();
            return;
        }
        smelt_core::pi_auto_models::update(|store| store.set_auto(&provider_id, false));
        self.reload_pi_model_settings();
        crate::status_item::notify_success(format!("已移除自定义 Provider {provider_id}"));
        cx.notify();
    }

    pub(super) fn open_pi_config(
        &mut self,
        file_kind: PiConfigFileKind,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let path = match file_kind {
            PiConfigFileKind::Settings => smelt_core::pi_model_settings::pi_settings_path(),
            PiConfigFileKind::Models => smelt_core::pi_model_settings::pi_models_path(),
            PiConfigFileKind::Credentials => smelt_core::pi_model_settings::pi_auth_path(),
        };
        if !path.exists() {
            use std::io::Write as _;

            if let Some(parent) = path.parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                crate::status_item::notify_error(format!("创建 Pi 配置目录失败：{error}"));
                return;
            }
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(b"{\n}\n") {
                        crate::status_item::notify_error(format!("创建 Pi 配置文件失败：{error}"));
                        return;
                    }
                    #[cfg(unix)]
                    if matches!(file_kind, PiConfigFileKind::Credentials) {
                        use std::os::unix::fs::PermissionsExt as _;
                        let _ =
                            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    crate::status_item::notify_error(format!("创建 Pi 配置文件失败：{error}"));
                    return;
                }
            }
        }
        match crate::ide::open_in_system_text_editor(&path) {
            Ok(()) => crate::status_item::notify_success(format!(
                "已在系统文本编辑器打开 {}",
                path.display()
            )),
            Err(error) => crate::status_item::notify_error(error),
        }
    }

    pub(super) fn refresh_pi_settings(&mut self, cx: &mut Context<Self>) {
        self.reload_pi_model_settings();
        cx.notify();
    }

    /// 扫一次插件目录并缓存。设置页每次渲染都拿缓存，不重复读磁盘。
    pub(super) fn pi_plugins_cached(&self) -> &[smelt_core::pi_plugin_catalog::PiPlugin] {
        self.pi_plugins
            .get_or_init(smelt_core::pi_plugin_catalog::discover_plugins)
    }

    pub(super) fn refresh_pi_plugins(&mut self, cx: &mut Context<Self>) {
        self.pi_plugins.take();
        self.pi_plugin_pending_delete = None;
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
        let plugins = self.pi_plugins_cached().to_vec();
        let Some(plugin) = plugins.iter().find(|plugin| plugin.id == id) else {
            self.pi_plugin_pending_delete = None;
            cx.notify();
            return;
        };
        match smelt_core::pi_plugin_catalog::remove_plugin(plugin) {
            Ok(()) => {
                self.pi_plugin_error = None;
                let removed = id.clone();
                super::apply_agent_host(
                    move |config| {
                        for agent in &mut config.agents {
                            agent.plugins.retain(|plugin_id| *plugin_id != removed);
                        }
                    },
                    cx,
                );
            }
            Err(error) => self.pi_plugin_error = Some(error),
        }
        self.refresh_pi_plugins(cx);
    }

    /// 从本地目录 / 文件导入插件：拷贝进全局插件目录，之后就能在智能体里勾选。
    pub(super) fn import_pi_plugin(
        &mut self,
        kind: smelt_core::pi_plugin_catalog::PiPluginKind,
        cx: &mut Context<Self>,
    ) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: matches!(kind, smelt_core::pi_plugin_catalog::PiPluginKind::Extension),
            directories: true,
            multiple: false,
            prompt: Some("导入".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(source) = paths.into_iter().next() else {
                return;
            };
            let outcome = smelt_core::pi_plugin_catalog::import_plugin(kind, &source);
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok(_) => this.pi_plugin_error = None,
                    Err(error) => this.pi_plugin_error = Some(error),
                }
                this.refresh_pi_plugins(cx);
            });
        })
        .detach();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiConfigFileKind {
    Settings,
    Models,
    Credentials,
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

    /// 从一个完整的插件 package 目录生成安装计划，等待用户审阅权限。
    ///
    /// 目录选择在主线程，校验和摘要放后台；此阶段不复制、不启动。
    pub fn install_user_plugin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择插件包目录（内含 plugin.json）".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let source = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    let _ = cx.update(|_window, _cx| {
                        crate::status_item::notify_error(format!("选择插件目录失败：{error}"));
                    });
                    return;
                }
                Err(_) => return,
            };
            let Some(source) = source else {
                return;
            };
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let root =
                        smelt_paths::smelt_home().ok_or_else(|| "无法定位用户目录".to_string())?;
                    let candidate = smelt_plugin_host::PluginPackage::load(&source)
                        .map_err(|error| error.to_string())?;
                    let conflicts_with_bundled =
                        crate::terminal::discover_installed_plugin_packages()
                            .into_iter()
                            .filter_map(Result::ok)
                            .any(|installed| {
                                matches!(
                                    installed.provenance(),
                                    smelt_plugin_host::PluginProvenance::FirstParty
                                ) && installed.manifest().id == candidate.manifest().id
                            });
                    if conflicts_with_bundled {
                        return Err(format!(
                            "插件 ID {} 已由应用自带插件占用，第三方包不能覆盖它",
                            candidate.manifest().id
                        ));
                    }
                    smelt_plugin_host::plan_user_plugin_install(&root, &source)
                        .map_err(|error| error.to_string())
                })
                .await;
            let _ = this.update_in(cx, |_this, _window, cx| {
                match outcome {
                    Ok(plan) => {
                        let name = plan.name.clone();
                        let version = plan.version.clone();
                        let mut state = cx
                            .try_global::<PluginEnablementState>()
                            .cloned()
                            .unwrap_or_else(PluginEnablementState::load);
                        state.pending_install = Some(plan);
                        cx.set_global(state);
                        crate::status_item::notify_info(format!(
                            "请审阅 {name} v{version} 的安装权限"
                        ));
                    }
                    Err(error) => {
                        crate::status_item::notify_error(format!("安装插件失败：{error}"));
                    }
                }
                cx.refresh_windows();
            });
        })
        .detach();
    }

    pub fn confirm_user_plugin_install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let plan = cx
            .try_global::<PluginEnablementState>()
            .and_then(|state| state.pending_install.clone());
        let Some(plan) = plan else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let root =
                        smelt_paths::smelt_home().ok_or_else(|| "无法定位用户目录".to_string())?;
                    let installed = smelt_plugin_host::install_user_plugin(&root, &plan)
                        .map_err(|error| error.to_string())?;
                    crate::terminal::plugin_reload().map_err(|error| {
                        format!(
                            "插件已安装到磁盘，但通知守护重新加载失败：{error}。重启 Smelt 后会生效"
                        )
                    })?;
                    Ok::<_, String>(installed)
                })
                .await;
            let _ = this.update_in(cx, |_this, _window, cx| {
                match outcome {
                    Ok(installed) => {
                        cx.set_global(PluginEnablementState::load());
                        cx.set_global(PluginRuntimeStatuses::default());
                        crate::plugin_ui::refresh(cx);
                        crate::status_item::notify_success(format!(
                            "已安装 {} v{}",
                            installed.name, installed.version
                        ));
                    }
                    Err(error) => {
                        crate::status_item::notify_error(format!("安装插件失败：{error}"));
                    }
                }
                cx.refresh_windows();
            });
        })
        .detach();
    }

    pub fn cancel_user_plugin_install(&mut self, cx: &mut Context<Self>) {
        let mut state = cx
            .try_global::<PluginEnablementState>()
            .cloned()
            .unwrap_or_else(PluginEnablementState::load);
        state.pending_install = None;
        cx.set_global(state);
        cx.refresh_windows();
    }

    /// 卸载一份用户插件。第一方 bundled 插件没有这个入口，只能停用。
    pub fn uninstall_user_plugin(
        &mut self,
        plugin_id: String,
        plugin_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let id = plugin_id.clone();
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let root =
                        smelt_paths::smelt_home().ok_or_else(|| "无法定位用户目录".to_string())?;
                    smelt_plugin_host::uninstall_user_plugin(&root, &id)
                        .map_err(|error| error.to_string())?;
                    crate::terminal::plugin_reload().map_err(|error| {
                        format!(
                            "插件已从磁盘卸载，但通知守护重新加载失败：{error}。重启 Smelt 后会收口"
                        )
                    })
                })
                .await;
            let _ = this.update_in(cx, |_this, _window, cx| {
                match outcome {
                    Ok(()) => {
                        let mut state = PluginEnablementState::load();
                        // 卸载后清掉残留的 disabled 记录；以后装回来应按默认启用处理。
                        state.enablement.set_enabled(&plugin_id, true);
                        if let Err(error) = state.enablement.save() {
                            eprintln!("[plugins] 清理卸载插件的启用状态失败: {error}");
                        }
                        cx.set_global(state);
                        cx.set_global(PluginRuntimeStatuses::default());
                        crate::plugin_ui::refresh(cx);
                        crate::status_item::notify_success(format!("已卸载 {plugin_name}"));
                    }
                    Err(error) => {
                        crate::status_item::notify_error(format!("卸载插件失败：{error}"));
                    }
                }
                cx.refresh_windows();
            });
        })
        .detach();
    }
}
