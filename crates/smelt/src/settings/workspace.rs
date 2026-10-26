//! `Workspace` 的设置页交互与渲染实现。
//!
//! 设置数据模型、远程生命周期和 Agent hooks 分别由父模块及其子模块提供；本文件
//! 只保留设置页对 `Workspace` 的状态操作，避免把数千行 UI 回调堆在模块入口。

use super::*;

impl Workspace {
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
    /// 更新通道只在 macOS 上存在（设计文档 D4）。
    #[cfg(target_os = "macos")]
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
                // 探测不到可执行文件的不当作"没装"：
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
        let count = super::agent_host_settings(&crate::RuntimeKey::Local, cx)
            .profiles
            .len();
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

        let profiles = super::agent_host_settings(&crate::RuntimeKey::Local, cx)
            .profiles
            .clone();
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
                            &crate::RuntimeKey::Local,
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
                        &crate::RuntimeKey::Local,
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
            &crate::RuntimeKey::Local,
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
            &crate::RuntimeKey::Local,
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
            &crate::RuntimeKey::Local,
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
        match self.pi_library_config() {
            Some(config) => config.model_settings.clone(),
            None => Err(self.pi_library_pending_text()),
        }
    }

    /// 凭据或模型目录在别处变了（登录、登出）：重新取快照；是本机的，还要告知活着的会话。
    pub(super) fn reload_pi_model_settings(&mut self, cx: &mut Context<Self>) {
        if *self.pi_library_runtime() == crate::RuntimeKey::Local {
            smelt_ui::provider_status_global::ProviderStatus::bump_pi_catalog(cx);
        }
        self.refresh_pi_library(cx);
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
        // 文件在那台机器上，这台电脑的编辑器打不开；照路径开还可能开到本机同名的另一份。
        if let crate::RuntimeKey::Remote(_) = self.pi_library_runtime() {
            crate::status_item::notify_error(format!(
                "配置文件在「{}」上，这台电脑打不开。可以在那台机器的终端里改。",
                crate::runtimes::runtime_name(self.pi_library_runtime(), cx)
            ));
            return;
        }
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
                    // 插件删在能力库那台机器上，只清那台机器上的智能体的勾选。
                    super::apply_agent_host(
                        &this.pi_library_runtime().clone(),
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
        kind: smelt_model::pi_plugin_catalog::PiPluginKind,
        cx: &mut Context<Self>,
    ) {
        let Some(runtime) = crate::runtimes::runtime(self.pi_library_runtime(), cx) else {
            self.pi_plugin_error = Some("先配对一台远程机器，再导入插件".to_string());
            cx.notify();
            return;
        };
        self.open_dir_picker(
            runtime,
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
