//! 模型页、插件页背后的操作：默认模型、自定义 Provider、打开配置文件、导入删除插件。

use super::*;

impl CapabilitiesPage {
    /// 自定义模型的四个输入框。
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

    pub fn edit_pi_model_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::InputState;

        self.pi_model_editor_error = None;
        let settings = match self.pi_model_settings().cloned() {
            Ok(settings) => settings,
            Err(error) => {
                self.pi_model_editor_error = Some(error.clone());
                notify_error(error, cx);
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
        self.run_pi_mutation::<api::model::control_api::PiModelsSetThinkingLevel>(
            api::model::pi_service::ThinkingLevelParams { level },
            cx,
            |_, error, cx| notify_error(&error, cx),
        );
        cx.notify();
    }

    pub fn save_pi_model_settings(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.pi_model_editor.clone() else {
            return;
        };
        let provider = editor.provider.read(cx).value().trim().to_string();
        if provider.is_empty() {
            notify_error("提供方不能为空", cx);
            return;
        }
        let model = editor.model.read(cx).value().trim().to_string();
        if model.is_empty() {
            notify_error("模型 ID 不能为空", cx);
            return;
        }
        let base_url = editor.base_url.read(cx).value().trim().to_string();
        if !base_url.is_empty()
            && let Err(error) = api::model::pi_model_settings::validate_api_base_url(&base_url)
        {
            notify_error(error, cx);
            return;
        }
        let api_key = editor.api_key.read(cx).value().to_string();
        if api_key.is_empty() && !editor.credential_configured {
            notify_error("请输入 API key", cx);
            return;
        }

        let params = api::model::pi_service::SaveDefaultModelParams {
            config: api::model::pi_model_settings::PiDefaultModelConfig {
                provider,
                model,
                base_url,
                thinking_level: editor.thinking_level,
            },
            api_key: (!api_key.is_empty()).then_some(api_key),
            reset_base_url: true,
        };
        self.run_pi_call::<api::model::control_api::PiModelsSaveDefault>(
            params,
            api::CALL_TIMEOUT,
            cx,
            |this, result, cx| match result {
                Ok(config) => {
                    this.pi_model_editor = None;
                    this.pi_model_editor_error = None;
                    this.adopt_pi_config(config, cx);
                    notify_success("Pi 模型配置已保存", cx);
                }
                Err(error) => this.reject_pi_model_edit(error, cx),
            },
        );
    }

    fn pi_custom_model_editor(
        model: Option<&api::model::pi_model_settings::PiCustomModel>,
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

        let settings = match self.pi_model_settings().cloned() {
            Ok(settings) => settings,
            Err(error) => {
                self.pi_model_editor_error = Some(error.clone());
                notify_error(error, cx);
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
            self.pi_library_config()
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
    /// 走 Pi 自己的直连发现：列模型本来就只是一次 `GET /models`。
    pub fn discover_pi_custom_models(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.pi_custom_provider_editor.as_ref() else {
            return;
        };
        if matches!(editor.discovery, Some(ModelDiscoveryState::Loading)) {
            return;
        }
        // 编辑器里没填 key 时，运行时用这个 provider 已存的那份（占位符写的就是「已配置」）。
        let params = api::model::pi_service::DiscoverModelsParams {
            base_url: editor.base_url.read(cx).value().trim().to_string(),
            api: editor.api.clone(),
            api_key: editor.api_key.read(cx).value().trim().to_string(),
            saved_provider_id: editor.previous_id.clone(),
        };
        if let Err(error) = api::model::pi_model_discovery::models_endpoint(&params.base_url) {
            self.fail_pi_discovery(error, cx);
            return;
        }
        if let Some(editor) = self.pi_custom_provider_editor.as_mut() {
            editor.discovery = Some(ModelDiscoveryState::Loading);
        }
        cx.notify();
        self.run_pi_call::<api::model::control_api::PiProvidersDiscoverModels>(
            params,
            api::CALL_TIMEOUT,
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
        model: api::model::provider_api::DiscoveredModel,
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
            Some(&api::model::pi_model_settings::PiCustomModel {
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
        editor.api = api::model::provider_api::normalize_provider_api(api).to_string();
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
        notify_error(&error, cx);
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
        if let Err(error) = api::model::pi_model_settings::validate_custom_provider_id(&id) {
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
        if let Err(error) = api::model::pi_model_settings::validate_api_base_url(&base_url) {
            self.reject_pi_model_edit(error, cx);
            return;
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
            models.push(api::model::pi_model_settings::PiCustomModel {
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
            if let Err(error) = api::model::pi_model_discovery::models_endpoint(&base_url) {
                self.fail_pi_discovery(format!("{error}；或手动填写至少一个模型"), cx);
                return;
            }
            if let Some(editor) = self.pi_custom_provider_editor.as_mut() {
                editor.discovery = Some(ModelDiscoveryState::Loading);
            }
            cx.notify();
        }
        let params = api::model::pi_service::SaveCustomProviderParams {
            previous_id: editor.previous_id.clone(),
            provider: api::model::pi_model_settings::PiCustomProvider {
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
        self.run_pi_call::<api::model::control_api::PiProvidersSaveCustom>(
            params,
            api::CALL_TIMEOUT,
            cx,
            move |this, result, cx| match result {
                Ok(saved) => {
                    this.pi_custom_provider_editor = None;
                    this.pi_model_editor_error = None;
                    this.adopt_pi_config(saved.config, cx);
                    notify_success(
                        if saved.auto {
                            format!("自定义 Provider {id} 已保存，模型目录将在每次启动时刷新")
                        } else {
                            format!("自定义 Provider {id} 已保存")
                        },
                        cx,
                    );
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
        self.run_pi_call::<api::model::control_api::PiProvidersRemoveCustom>(
            api::model::pi_service::ProviderIdParams {
                provider_id: provider_id.clone(),
            },
            api::CALL_TIMEOUT,
            cx,
            move |this, result, cx| match result {
                Ok(config) => {
                    this.adopt_pi_config(config, cx);
                    notify_success(format!("已移除自定义 Provider {provider_id}"), cx);
                }
                Err(error) => notify_error(error, cx),
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
        // 文件在那台机器上，这台电脑的编辑器打不开；照路径开还可能开到本机同名的另一份。
        if let RuntimeKey::Remote(_) = &self.machine {
            let name = api::host(cx)
                .machines(cx)
                .into_iter()
                .find(|machine| machine.key == self.machine)
                .map(|machine| machine.name)
                .unwrap_or_default();
            notify_error(
                format!("配置文件在「{name}」上，这台电脑打不开。可以在那台机器的终端里改。"),
                cx,
            );
            return;
        }
        let file = match file_kind {
            PiConfigFileKind::Settings => api::model::pi_service::PiConfigFile::Settings,
            PiConfigFileKind::Models => api::model::pi_service::PiConfigFile::Models,
            PiConfigFileKind::Credentials => api::model::pi_service::PiConfigFile::Credentials,
            PiConfigFileKind::Mcp => api::model::pi_service::PiConfigFile::Mcp,
        };
        self.run_pi_call::<api::model::control_api::PiConfigEnsureFile>(
            api::model::pi_service::EnsureConfigFileParams { file },
            api::CALL_TIMEOUT,
            cx,
            |_, result, cx| {
                let path = match result {
                    Ok(result) => std::path::PathBuf::from(result.path),
                    Err(error) => {
                        notify_error(error, cx);
                        return;
                    }
                };
                let opened =
                    api::OPEN_IN_TEXT_EDITOR.run(&api::OpenInTextEditor { path: path.clone() }, cx);
                cx.spawn(async move |_, cx| {
                    let message = opened.await;
                    cx.update(|cx| match message {
                        Ok(()) => {
                            notify_success(format!("已在系统文本编辑器打开 {}", path.display()), cx)
                        }
                        Err(error) => notify_error(error, cx),
                    })
                })
                .detach();
            },
        );
    }

    pub(super) fn refresh_pi_settings(&mut self, cx: &mut Context<Self>) {
        self.refresh_pi_library(cx);
        cx.notify();
    }

    pub(super) fn refresh_pi_plugins(&mut self, cx: &mut Context<Self>) {
        self.pi_plugin_pending_delete = None;
        self.refresh_pi_library(cx);
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

    /// 真删 `~/.pi` 下的实体。智能体对它的勾选由运行时一并清掉，桌面端收到 Pi 配置变化
    /// 后重读那台机器的智能体配置。
    pub(super) fn confirm_pi_plugin_delete(&mut self, id: String, cx: &mut Context<Self>) {
        self.pi_plugin_pending_delete = None;
        self.run_pi_call::<api::model::control_api::PiPluginsRemove>(
            api::model::pi_service::PluginIdParams { id },
            api::CALL_TIMEOUT,
            cx,
            move |this, result, cx| match result {
                Ok(config) => {
                    this.pi_plugin_error = None;
                    this.adopt_pi_config(config, cx);
                }
                Err(error) => {
                    this.pi_plugin_error = Some(error);
                    this.refresh_pi_library(cx);
                }
            },
        );
        cx.notify();
    }

    /// 导入插件：在运行时那台机器上选目录（扩展也可以是单个文件），运行时把它拷贝进
    /// 全局插件目录，之后就能在智能体里勾选。
    pub(super) fn import_pi_plugin(
        &mut self,
        kind: api::model::pi_plugin_catalog::PiPluginKind,
        cx: &mut Context<Self>,
    ) {
        use api::model::pi_plugin_catalog::PiPluginKind;

        let request = api::PathRequest {
            machine: self.machine.clone(),
            title: "选择要导入的插件".to_string(),
            confirm: "导入".to_string(),
            // 扩展可以是单个文件，技能是目录。
            files: kind == PiPluginKind::Extension,
            multiple: false,
        };
        let picked = api::PICK_PATHS.run(&request, cx);
        cx.spawn(async move |page, cx| {
            let source = match picked.await {
                Ok(paths) => paths.and_then(|paths| paths.into_iter().next()),
                Err(error) => {
                    cx.update(|cx| notify_error(error, cx));
                    return;
                }
            };
            if let Some(source) = source {
                page.update(cx, |page, cx| page.import_pi_plugin_from(kind, source, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn import_pi_plugin_from(
        &mut self,
        kind: api::model::pi_plugin_catalog::PiPluginKind,
        source: String,
        cx: &mut Context<Self>,
    ) {
        self.run_pi_mutation::<api::model::control_api::PiPluginsImport>(
            api::model::pi_service::ImportPluginParams { kind, source },
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
